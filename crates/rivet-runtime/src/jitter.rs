//! Jitter for retry backoff.
//!
//! `ExponentialBackoff::delay_for` is deliberately pure — its own comment says jitter is
//! the runtime's job — so that a retry curve can be asserted exactly in a test. This is
//! the runtime half.
//!
//! Full jitter (`[0, delay)`) rather than a fraction of the delay: when several agents hit
//! the same rate limit, a fixed proportion keeps them synchronized and they collide again
//! on the next attempt.
//!
//! No `rand` dependency. Retry timing is not a security decision, and a xorshift seeded
//! from the clock and the run id is enough to decorrelate two runs.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_core::id::RunId;

/// Spreads retries out in time.
pub trait Jitter: Send + Sync + fmt::Debug {
    /// Return an actual delay for a computed one.
    fn apply(&self, delay_ms: u64) -> u64;
}

/// Full jitter: uniform in `[0, delay)`.
#[derive(Debug)]
pub struct FullJitter {
    state: AtomicU64,
}

impl FullJitter {
    /// Seed from the clock and a run id, so two concurrent runs do not retry in lockstep.
    #[must_use]
    pub fn for_run(run_id: RunId) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos().into());
        // Only the low half is needed; a seed does not have to be injective.
        let from_id = run_id.as_uuid().as_u64_pair().1;
        // A zero state would make xorshift return zero forever.
        Self {
            state: AtomicU64::new((nanos ^ from_id) | 1),
        }
    }

    fn next(&self) -> u64 {
        // xorshift64*, sufficient for spreading out retries.
        let mut x = self.state.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state.store(x, Ordering::Relaxed);
        x
    }
}

impl Default for FullJitter {
    fn default() -> Self {
        Self::for_run(RunId::new())
    }
}

impl Jitter for FullJitter {
    fn apply(&self, delay_ms: u64) -> u64 {
        if delay_ms == 0 {
            return 0;
        }
        self.next() % delay_ms
    }
}

/// No jitter, for tests that assert an exact retry curve.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoJitter;

impl Jitter for NoJitter {
    fn apply(&self, delay_ms: u64) -> u64 {
        delay_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_jitter_stays_inside_the_window() {
        let jitter = FullJitter::default();
        for _ in 0..1000 {
            assert!(jitter.apply(500) < 500);
        }
        assert_eq!(jitter.apply(0), 0, "a zero delay must not divide by zero");
    }

    #[test]
    fn full_jitter_actually_varies() {
        // A "jitter" that returns the same number keeps every client synchronized, which
        // is the failure it exists to prevent.
        let jitter = FullJitter::default();
        let sample: Vec<u64> = (0..20).map(|_| jitter.apply(10_000)).collect();
        assert!(sample.windows(2).any(|w| w[0] != w[1]), "{sample:?}");
    }

    #[test]
    fn no_jitter_is_the_identity() {
        assert_eq!(NoJitter.apply(1234), 1234);
    }
}
