//! Exit codes.
//!
//! A caller scripting `rivet` needs to tell "the agent finished" from "a limit stopped
//! it" from "the config is wrong" without parsing prose.

use rivet_core::agent::StopReason;

/// The run finished its turn.
pub const OK: i32 = 0;
/// The run failed, or the runtime did.
pub const FAILED: i32 = 1;
/// Configuration: a bad file, an unknown profile, a missing credential.
pub const CONFIG: i32 = 2;
/// A [`rivet_core::agent::RunLimits`] axis stopped the run.
pub const LIMIT: i32 = 3;
/// A policy refused something the run could not proceed without.
pub const POLICY: i32 = 4;
/// Cancelled, by convention `128 + SIGINT`.
pub const CANCELLED: i32 = 130;

/// The exit code for a finished run.
#[must_use]
pub fn for_stop(stop: &StopReason) -> i32 {
    match stop {
        StopReason::EndTurn => OK,
        StopReason::Error { .. } => FAILED,
        StopReason::LimitReached { .. } => LIMIT,
        // Still unreachable, and deliberately so. A policy refusing a *tool call* is a
        // tool result the model reads and adapts to -- ending the run there would turn
        // `rivet --profile readonly "delete all logs"` into exit 4 instead of an answer
        // saying it cannot. This code becomes reachable when a policy refuses the *run*
        // (a `StartRun` or `LoadPlugin` action), and nothing does that yet. A script that
        // wants to know a policy blocked something reads `tool.blocked` from `--jsonl`.
        StopReason::PolicyBlocked { .. } => POLICY,
        StopReason::Cancelled => CANCELLED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::agent::LimitKind;

    #[test]
    fn every_ending_has_a_distinct_code() {
        assert_eq!(for_stop(&StopReason::EndTurn), 0);
        assert_eq!(
            for_stop(&StopReason::Error {
                message: "boom".into()
            }),
            1
        );
        assert_eq!(
            for_stop(&StopReason::LimitReached {
                limit: LimitKind::Turns
            }),
            3
        );
        assert_eq!(
            for_stop(&StopReason::PolicyBlocked {
                reason: "no".into()
            }),
            4
        );
        assert_eq!(for_stop(&StopReason::Cancelled), 130);
    }
}
