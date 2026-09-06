//! The single error type crossing capability boundaries.
//!
//! Design note: every error carries a [`ErrorKind`] that *classifies* it, because the
//! retry layer (`RetryPolicy`) must decide `should_retry` without string-matching
//! provider messages. A plugin that returns an unclassified error is telling the runtime
//! "assume permanent", which is the safe default.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias used throughout Rivet.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// How the runtime should reason about a failure.
///
/// This is deliberately about *disposition*, not about which subsystem failed. The
/// subsystem is already visible in [`Error::source_capability`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The caller sent something malformed. Never retry; fix the input.
    InvalidArgument,
    /// The named thing does not exist.
    NotFound,
    /// A policy said no. Never retry without changing the policy or getting approval.
    PolicyDenied,
    /// A human declined, or the approval expired.
    ApprovalDenied,
    /// The operation was cancelled cooperatively.
    Cancelled,
    /// A deadline elapsed. Usually retryable.
    Timeout,
    /// Upstream rate limit. Retryable after a backoff, ideally the server-provided one.
    RateLimited { retry_after_ms: Option<u64> },
    /// A transient upstream/network failure. Retryable.
    Transient,
    /// The upstream rejected us permanently (bad key, unsupported model, 4xx).
    Upstream,
    /// A plugin misbehaved: bad manifest, failed handshake, panicked, broke a contract.
    Plugin,
    /// Persistence failed.
    Storage,
    /// A bug in Rivet itself, or an invariant violation.
    Internal,
}

impl ErrorKind {
    /// Whether retrying the *identical* operation could plausibly succeed.
    ///
    /// This is a hint for [`crate::retry::RetryPolicy`], not a decision. A policy may
    /// still refuse to retry a retryable error (budget exhausted) or escalate a
    /// non-retryable one to a human.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::RateLimited { .. } | Self::Transient
        )
    }

    /// Whether the failure requires a human before any further progress.
    #[must_use]
    pub fn needs_human(self) -> bool {
        matches!(self, Self::PolicyDenied | Self::ApprovalDenied)
    }
}

/// Which capability produced the error. Used for telemetry and for error messages that
/// point at the right plugin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    #[default]
    Runtime,
    Model,
    Tool,
    Context,
    Policy,
    Sandbox,
    Session,
    Job,
    Plugin,
    Memory,
    Storage,
}

/// The error type crossing every Rivet contract.
#[derive(Clone, Serialize, Deserialize)]
pub struct Error {
    kind: ErrorKind,
    capability: Capability,
    message: String,
    /// Free-form structured detail. Never contains secrets; plugins must redact.
    details: Option<serde_json::Value>,
    /// Rendered causal chain, flattened at construction so `Error` stays `Clone + Send`
    /// and can be serialized across a process/WASM plugin boundary.
    causes: Vec<String>,
}

impl Error {
    pub fn new(kind: ErrorKind, capability: Capability, message: impl Into<String>) -> Self {
        Self {
            kind,
            capability,
            message: message.into(),
            details: None,
            causes: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Attach a rendered cause. Call this at the boundary where you convert a foreign
    /// error, so the chain survives serialization to an out-of-process plugin.
    #[must_use]
    pub fn with_cause(mut self, cause: impl fmt::Display) -> Self {
        self.causes.push(cause.to_string());
        self
    }

    #[must_use]
    pub fn in_capability(mut self, capability: Capability) -> Self {
        self.capability = capability;
        self
    }

    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn source_capability(&self) -> Capability {
        self.capability
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> Option<&serde_json::Value> {
        self.details.as_ref()
    }

    #[must_use]
    pub fn causes(&self) -> &[String] {
        &self.causes
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.kind == ErrorKind::Cancelled
    }

    // --- constructors for the common cases -------------------------------------------

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, Capability::Runtime, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, Capability::Runtime, message)
    }

    pub fn policy_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PolicyDenied, Capability::Policy, message)
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Cancelled, Capability::Runtime, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, Capability::Runtime, message)
    }

    pub fn transient(capability: Capability, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Transient, capability, message)
    }

    pub fn rate_limited(retry_after_ms: Option<u64>, message: impl Into<String>) -> Self {
        Self::new(
            ErrorKind::RateLimited { retry_after_ms },
            Capability::Model,
            message,
        )
    }

    pub fn plugin(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Plugin, Capability::Plugin, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Storage, Capability::Storage, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, Capability::Runtime, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:?}/{:?}] {}",
            self.capability, self.kind, self.message
        )?;
        for cause in &self.causes {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("capability", &self.capability)
            .field("message", &self.message)
            .field("causes", &self.causes)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::invalid_argument("json (de)serialization failed").with_cause(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_drives_retry_and_escalation() {
        assert!(ErrorKind::Transient.is_retryable());
        assert!(
            ErrorKind::RateLimited {
                retry_after_ms: Some(500)
            }
            .is_retryable()
        );
        assert!(!ErrorKind::InvalidArgument.is_retryable());
        assert!(!ErrorKind::PolicyDenied.is_retryable());
        assert!(ErrorKind::PolicyDenied.needs_human());
        assert!(!ErrorKind::Timeout.needs_human());
    }

    #[test]
    fn cancellation_is_never_retried() {
        let err = Error::cancelled("user pressed ctrl-c");
        assert!(err.is_cancelled());
        assert!(!err.is_retryable());
    }

    #[test]
    fn error_survives_a_serialization_round_trip() {
        let err = Error::rate_limited(Some(1_200), "429 from provider")
            .with_cause("upstream said slow down")
            .with_details(serde_json::json!({ "provider": "openai" }));
        let json = serde_json::to_string(&err).unwrap();
        let back: Error = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind(), err.kind());
        assert_eq!(back.causes(), err.causes());
        assert!(back.is_retryable());
    }
}
