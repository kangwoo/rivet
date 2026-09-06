//! Time handling for durable events.
//!
//! Session events are durable facts that get replayed, audited and diffed, so their
//! timestamps must be unambiguous and stable across machines. We store an explicit UTC
//! instant with millisecond precision rather than a naive local time.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A UTC instant with millisecond precision.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The current instant.
    #[must_use]
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub fn as_millis(&self) -> i64 {
        self.0.as_millisecond()
    }

    /// Build from milliseconds since the Unix epoch.
    pub fn from_millis(millis: i64) -> crate::Result<Self> {
        jiff::Timestamp::from_millisecond(millis)
            .map(Self)
            .map_err(|e| crate::Error::invalid_argument(format!("invalid timestamp: {e}")))
    }

    /// RFC 3339 rendering, always in UTC.
    #[must_use]
    pub fn to_rfc3339(&self) -> String {
        self.0.to_string()
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({})", self.0)
    }
}

impl From<jiff::Timestamp> for Timestamp {
    fn from(value: jiff::Timestamp) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millis_round_trip() {
        let ts = Timestamp::from_millis(1_700_000_000_000).unwrap();
        assert_eq!(ts.as_millis(), 1_700_000_000_000);
    }

    #[test]
    fn serializes_as_rfc3339_utc() {
        let ts = Timestamp::from_millis(0).unwrap();
        let json = serde_json::to_string(&ts).unwrap();
        assert!(json.contains("1970-01-01"), "unexpected encoding: {json}");
        assert!(json.ends_with("Z\""), "must be UTC: {json}");
    }
}
