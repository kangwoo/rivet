//! HTTP status, headers and error bodies -> [`rivet_core::Error`]. Pure.
//!
//! Everything the retry layer needs is carried by [`ErrorKind`]; nothing here asks the
//! caller to string-match a provider message. Getting this table wrong is how an agent
//! either hammers a permanently-broken endpoint or gives up on a 503.

use rivet_core::error::{Capability, Error, ErrorKind};

use crate::wire::{WireError, WireErrorBody};

/// How much of a response body is safe and useful to attach.
const BODY_SNIPPET_BYTES: usize = 512;

/// Upper bound on a retry hint, in milliseconds (one day). `RetryPolicy` caps this again
/// with its own `max_delay_ms`; this only keeps an absurd header from becoming an absurd
/// number.
const MAX_RETRY_AFTER_MS: f64 = 86_400_000.0;

/// Classify a non-2xx response.
///
/// `retry_after_ms` comes from [`retry_after_ms`]; `body` is the raw response body, which
/// may or may not be JSON. Neither the API key nor any request header is ever attached —
/// `details` is read by humans and shipped to telemetry.
#[must_use]
pub fn classify_http(
    status: u16,
    retry_after_ms: Option<u64>,
    body: &str,
    request_id: Option<&str>,
) -> Error {
    let parsed: Option<WireError> = serde_json::from_str::<WireErrorBody>(body)
        .ok()
        .and_then(|b| b.error);
    let provider_message = parsed
        .as_ref()
        .map(|e| e.message.clone())
        .filter(|m| !m.is_empty());

    let kind = match status {
        400 | 422 => ErrorKind::InvalidArgument,
        401 | 403 => ErrorKind::Upstream,
        404 => ErrorKind::NotFound,
        408 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited { retry_after_ms },
        500 | 502 | 503 | 504 | 529 => ErrorKind::Transient,
        s if (500..600).contains(&s) => ErrorKind::Transient,
        _ => ErrorKind::Upstream,
    };

    let message = provider_message.map_or_else(
        || format!("model provider returned HTTP {status}"),
        |m| format!("model provider returned HTTP {status}: {m}"),
    );

    Error::new(kind, Capability::Model, message).with_details(details(status, body, request_id))
}

/// Classify an `{"error": {...}}` object that arrived *inside* a `200` stream.
///
/// Gateways do this when the failure only becomes apparent after headers are flushed. The
/// status code is gone by then, so the `type`/`code` fields are all we have.
#[must_use]
pub fn classify_stream_error(error: &WireError) -> Error {
    let tag = error
        .kind
        .clone()
        .or_else(|| {
            error
                .code
                .as_ref()
                .and_then(|c| c.as_str().map(str::to_string))
        })
        .unwrap_or_default();

    // Spelled out one classification at a time: merging arms with the wildcard would hide
    // which provider tags we have actually seen and decided about.
    #[allow(clippy::match_same_arms)]
    let kind = match tag.as_str() {
        "rate_limit_exceeded" | "rate_limit_error" => ErrorKind::RateLimited {
            retry_after_ms: None,
        },
        "insufficient_quota" | "authentication_error" | "permission_error" => ErrorKind::Upstream,
        "server_error" | "overloaded_error" | "api_error" => ErrorKind::Transient,
        "invalid_request_error" => ErrorKind::InvalidArgument,
        // Unknown classification means "assume permanent", the safe default.
        _ => ErrorKind::Upstream,
    };

    let message = if error.message.is_empty() {
        "model provider reported an error mid-stream".to_string()
    } else {
        format!(
            "model provider reported an error mid-stream: {}",
            error.message
        )
    };

    Error::new(kind, Capability::Model, message)
        .with_details(serde_json::json!({ "provider_error_type": tag }))
}

/// A stream that ended without `[DONE]` and without a `finish_reason`.
///
/// Retryable: a truncated connection is exactly the failure a retry exists for, and the
/// alternative — reporting a partial answer as complete — is worse.
#[must_use]
pub fn incomplete_stream() -> Error {
    Error::transient(
        Capability::Model,
        "the model stream ended without a finish reason; the response is incomplete",
    )
}

/// A `data:` frame that was not JSON.
#[must_use]
pub fn undecodable_frame(raw: &str, cause: &impl std::fmt::Display) -> Error {
    Error::transient(Capability::Model, "a stream frame was not valid JSON")
        .with_cause(cause)
        .with_details(serde_json::json!({ "frame": snippet(raw) }))
}

/// Read a retry hint from response headers.
///
/// Order is `Retry-After` (seconds, or an HTTP-date) then the `x-ratelimit-reset-*`
/// family, because the first is the standard and the rest are the fallback several
/// gateways actually populate. Taking a lookup closure keeps this testable and keeps
/// `reqwest` out of a pure module.
pub fn retry_after_ms<'a>(header: impl Fn(&str) -> Option<&'a str>) -> Option<u64> {
    if let Some(raw) = header("retry-after")
        && let Some(ms) = parse_retry_after(raw)
    {
        return Some(ms);
    }
    for name in ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"] {
        if let Some(raw) = header(name)
            && let Some(ms) = parse_duration_hint(raw)
        {
            return Some(ms);
        }
    }
    None
}

/// `Retry-After` is either delta-seconds or an HTTP-date.
fn parse_retry_after(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Ok(seconds) = raw.parse::<f64>() {
        return seconds_to_millis(seconds);
    }
    let at = jiff::fmt::rfc2822::parse(raw).ok()?.timestamp();
    let delta = at.as_millisecond() - jiff::Timestamp::now().as_millisecond();
    u64::try_from(delta.max(0)).ok()
}

/// The `x-ratelimit-reset-*` headers use a suffixed duration: `6m0s`, `1.5s`, `250ms`.
fn parse_duration_hint(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(seconds) = raw.parse::<f64>() {
        return seconds_to_millis(seconds);
    }

    let mut total_ms = 0f64;
    let mut number = String::new();
    let mut unit = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if !unit.is_empty() {
                total_ms += apply_unit(&number, &unit)?;
                number.clear();
                unit.clear();
            }
            number.push(ch);
        } else if ch.is_ascii_alphabetic() {
            unit.push(ch);
        } else {
            return None;
        }
    }
    if number.is_empty() || unit.is_empty() {
        return None;
    }
    total_ms += apply_unit(&number, &unit)?;
    millis_to_u64(total_ms)
}

/// Seconds -> milliseconds, refusing anything that is not a sane wait.
fn seconds_to_millis(seconds: f64) -> Option<u64> {
    millis_to_u64(seconds * 1000.0)
}

/// The one place a float becomes a delay. Clamped, so a hostile header cannot produce a
/// nonsensical wait and the truncation is deliberate rather than incidental.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn millis_to_u64(millis: f64) -> Option<u64> {
    if !millis.is_finite() || millis < 0.0 {
        return None;
    }
    Some(millis.round().min(MAX_RETRY_AFTER_MS) as u64)
}

fn apply_unit(number: &str, unit: &str) -> Option<f64> {
    let value: f64 = number.parse().ok()?;
    let factor = match unit {
        "ms" => 1.0,
        "s" => 1_000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        _ => return None,
    };
    Some(value * factor)
}

fn details(status: u16, body: &str, request_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "status": status,
        "request_id": request_id,
        "body": snippet(body),
    })
}

/// The leading bytes of a body, cut on a character boundary.
fn snippet(raw: &str) -> String {
    if raw.len() <= BODY_SNIPPET_BYTES {
        return raw.to_string();
    }
    let mut end = BODY_SNIPPET_BYTES;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    raw[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_table_matches_the_design() {
        let cases: [(u16, ErrorKind); 9] = [
            (400, ErrorKind::InvalidArgument),
            (422, ErrorKind::InvalidArgument),
            (401, ErrorKind::Upstream),
            (403, ErrorKind::Upstream),
            (404, ErrorKind::NotFound),
            (408, ErrorKind::Timeout),
            (500, ErrorKind::Transient),
            (529, ErrorKind::Transient),
            (418, ErrorKind::Upstream),
        ];
        for (status, expected) in cases {
            assert_eq!(
                classify_http(status, None, "", None).kind(),
                expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn rate_limits_carry_the_server_supplied_delay() {
        let err = classify_http(429, Some(7_000), "", None);
        assert_eq!(
            err.kind(),
            ErrorKind::RateLimited {
                retry_after_ms: Some(7_000)
            }
        );
        assert!(err.is_retryable());
    }

    #[test]
    fn five_hundreds_retry_and_four_hundreds_do_not() {
        assert!(classify_http(503, None, "", None).is_retryable());
        assert!(classify_http(599, None, "", None).is_retryable());
        assert!(!classify_http(400, None, "", None).is_retryable());
        assert!(!classify_http(401, None, "", None).is_retryable());
    }

    #[test]
    fn the_provider_message_reaches_the_operator() {
        let body =
            r#"{"error":{"message":"model `nope` not found","type":"invalid_request_error"}}"#;
        let err = classify_http(404, None, body, Some("req_123"));
        assert!(err.message().contains("model `nope` not found"), "{err}");
        assert_eq!(err.details().unwrap()["request_id"], "req_123");
    }

    #[test]
    fn a_body_snippet_is_bounded_and_never_splits_a_character() {
        let body = "한".repeat(1000);
        let err = classify_http(500, None, &body, None);
        let attached = err.details().unwrap()["body"].as_str().unwrap().to_string();
        assert!(attached.len() <= BODY_SNIPPET_BYTES);
        assert!(body.starts_with(&attached));
    }

    #[test]
    fn mid_stream_errors_are_classified_by_type() {
        let cases = [
            ("rate_limit_exceeded", true),
            ("server_error", true),
            ("insufficient_quota", false),
            ("invalid_request_error", false),
            ("something_new", false),
        ];
        for (tag, retryable) in cases {
            let err = classify_stream_error(&WireError {
                message: "boom".into(),
                kind: Some(tag.into()),
                code: None,
            });
            assert_eq!(err.is_retryable(), retryable, "{tag}");
        }
    }

    #[test]
    fn retry_after_reads_seconds_then_the_ratelimit_family() {
        assert_eq!(
            retry_after_ms(|n| (n == "retry-after").then_some("3")),
            Some(3_000)
        );
        assert_eq!(
            retry_after_ms(|n| (n == "x-ratelimit-reset-requests").then_some("6m0s")),
            Some(360_000)
        );
        assert_eq!(
            retry_after_ms(|n| (n == "x-ratelimit-reset-tokens").then_some("250ms")),
            Some(250)
        );
        assert_eq!(retry_after_ms(|_| None), None);
    }

    #[test]
    fn an_http_date_retry_after_is_understood() {
        let future = jiff::Timestamp::now() + jiff::Span::new().seconds(30);
        let formatted = jiff::fmt::rfc2822::to_string(&future.to_zoned(jiff::tz::TimeZone::UTC))
            .expect("rfc2822");
        let ms = retry_after_ms(|n| (n == "retry-after").then_some(formatted.as_str()))
            .expect("an HTTP-date must parse");
        assert!((25_000..=31_000).contains(&ms), "got {ms}");
    }

    #[test]
    fn an_incomplete_stream_is_retryable() {
        assert!(incomplete_stream().is_retryable());
        assert!(undecodable_frame("{oops", &"expected value").is_retryable());
    }
}
