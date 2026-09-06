//! Retry policy.
//!
//! Separated from the agent loop because "how many times do we retry a 429" is an
//! operational decision that differs between a laptop and CI, and because the loop should
//! not grow branches for every failure mode.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorKind};

/// What to do after a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RetryDecision {
    /// Wait, then retry.
    RetryAfter { delay_ms: u64 },
    /// Do not retry.
    Stop,
}

/// Decides whether an operation is retried.
pub trait RetryPolicy: Send + Sync + fmt::Debug {
    /// `attempt` is 1-based: the first failure is attempt 1.
    fn should_retry(&self, error: &Error, attempt: u32) -> RetryDecision;
}

/// Exponential backoff with full jitter, capped.
///
/// Honors a server-supplied `retry_after_ms` over its own schedule — an upstream that
/// tells you when to come back knows better than a local curve.
#[derive(Clone, Copy, Debug)]
pub struct ExponentialBackoff {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    /// Multiplier per attempt.
    pub factor: u32,
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            factor: 2,
        }
    }
}

impl ExponentialBackoff {
    /// Deterministic delay for `attempt`, before jitter.
    ///
    /// Jitter is applied by the runtime, not here, so this stays pure and testable.
    #[must_use]
    pub fn delay_for(&self, attempt: u32) -> u64 {
        let exponent = attempt.saturating_sub(1).min(20);
        let scaled = self
            .base_delay_ms
            .saturating_mul(u64::from(self.factor).saturating_pow(exponent));
        scaled.min(self.max_delay_ms)
    }
}

impl RetryPolicy for ExponentialBackoff {
    fn should_retry(&self, error: &Error, attempt: u32) -> RetryDecision {
        if attempt >= self.max_attempts || !error.is_retryable() {
            return RetryDecision::Stop;
        }
        // A server-provided backoff always wins.
        if let ErrorKind::RateLimited {
            retry_after_ms: Some(server_delay),
        } = error.kind()
        {
            return RetryDecision::RetryAfter {
                delay_ms: server_delay.min(self.max_delay_ms),
            };
        }
        RetryDecision::RetryAfter {
            delay_ms: self.delay_for(attempt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Capability;

    #[test]
    fn permanent_errors_are_never_retried() {
        let policy = ExponentialBackoff::default();
        let err = Error::invalid_argument("bad schema");
        assert_eq!(policy.should_retry(&err, 1), RetryDecision::Stop);
    }

    #[test]
    fn policy_denials_are_never_retried() {
        let policy = ExponentialBackoff::default();
        let err = Error::policy_denied("nope");
        assert_eq!(policy.should_retry(&err, 1), RetryDecision::Stop);
    }

    #[test]
    fn transient_errors_back_off_exponentially() {
        let policy = ExponentialBackoff::default();
        let err = Error::transient(Capability::Model, "connection reset");
        assert_eq!(
            policy.should_retry(&err, 1),
            RetryDecision::RetryAfter { delay_ms: 500 }
        );
        assert_eq!(
            policy.should_retry(&err, 2),
            RetryDecision::RetryAfter { delay_ms: 1000 }
        );
        assert_eq!(
            policy.should_retry(&err, 3),
            RetryDecision::RetryAfter { delay_ms: 2000 }
        );
    }

    #[test]
    fn attempts_are_capped() {
        let policy = ExponentialBackoff::default();
        let err = Error::transient(Capability::Model, "flaky");
        assert_eq!(policy.should_retry(&err, 4), RetryDecision::Stop);
    }

    #[test]
    fn a_server_supplied_delay_overrides_the_local_curve() {
        let policy = ExponentialBackoff::default();
        let err = Error::rate_limited(Some(7_000), "429");
        assert_eq!(
            policy.should_retry(&err, 1),
            RetryDecision::RetryAfter { delay_ms: 7_000 },
            "attempt 1 would otherwise wait only 500ms"
        );
    }

    #[test]
    fn a_server_delay_is_still_capped() {
        let policy = ExponentialBackoff::default();
        let err = Error::rate_limited(Some(3_600_000), "come back in an hour");
        assert_eq!(
            policy.should_retry(&err, 1),
            RetryDecision::RetryAfter { delay_ms: 30_000 }
        );
    }

    #[test]
    fn delay_never_overflows() {
        let policy = ExponentialBackoff::default();
        assert_eq!(policy.delay_for(u32::MAX), policy.max_delay_ms);
    }
}
