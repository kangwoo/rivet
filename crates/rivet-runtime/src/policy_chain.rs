//! Pipeline steps 4 and 5: interceptors, policies, and the fold that combines them.
//!
//! ```text
//! seed = host baseline  { timeout · max_output_bytes · grant }   ← never `sandbox`
//!   │
//!   ├── 4 Intercept   interceptors(), concurrently, 2 s each
//!   ├── 5 Policy      policies(), in name order, all of them
//!   │
//!   └── fold with `PolicyDecision::combine`  (most restrictive wins)
//!            │
//!    rewrite ┴─▶ re-validate and re-run 4 and 5 on the rewritten call, depth ≤ 3
//! ```
//!
//! # The seed is the baseline, and the baseline has no sandbox
//!
//! `tool_timeout_ms`, `max_output_bytes` and the profile's grant are all
//! [`ExecutionConstraints`] axes, so seeding the fold with them makes the operator's
//! settings compose by the *same* rule as a policy's: tighter wins, and a policy can only
//! narrow them. A host that instead overwrote the result afterwards would make "the policy
//! asked for 30 s and the config put it back to 120 s" possible.
//!
//! `sandbox` is the one axis left out, because [`ExecutionConstraints::merge`] resolves it
//! with `self.or(other)` and provider names have no order. The seed is always `self`, so a
//! seeded `local` would beat a policy that asked for `docker` — the reverse of every other
//! axis. The configured provider travels as a *default* on `DispatchCtx` instead, and this
//! call's provider is `constraints.sandbox` or that default.
//!
//! # Concurrent interceptors, one timeout each
//!
//! [`crate::registry::Registry::interceptors`] sorts by priority, but that order only
//! decides which *reason* a user reads first — the results are folded, so the outcome does
//! not depend on it. Running them serially would therefore buy nothing and cost `N × 2 s`
//! before any tool runs. A shared budget is worse still: a slow interceptor would make the
//! next one abstain, which is timing changing a security decision.
//!
//! The per-round bound is [`INTERCEPTOR_TIMEOUT`], so the worst case for one call is
//! `REWRITE_DEPTH_LIMIT × INTERCEPTOR_TIMEOUT`. That is a bound *derived* from two
//! constants rather than a third one invented here; what stops a chain from outliving its
//! run is the caller's cancellation token, which every wait in this module selects on.

use std::time::Duration;

use futures_util::future::join_all;
use rivet_core::policy::{
    ExecutionConstraints, Outcome, PolicyAction, PolicyDecision, PolicyRequest,
};
use rivet_core::tool::{ToolCall, ToolSpec};
use tokio_util::sync::CancellationToken;

use crate::registry::Registry;

/// The name [`Evaluated::deciding`] carries when nothing in the chain decided anything.
///
/// Reserved: [`crate::registry::ScopedRegistry`]'s `register_policy` refuses it, because
/// a plugin able to register under this name could put itself on decisions it never made.
pub const BASELINE_POLICY: &str = "host.baseline";

/// How long one interceptor gets. Past it, it is folded as `None` and reported.
pub const INTERCEPTOR_TIMEOUT: Duration = Duration::from_millis(2_000);

/// How many times a rewrite may send the chain round again before it is refused.
pub const REWRITE_DEPTH_LIMIT: u8 = 3;

/// The seed the host puts into the fold. Policies can only tighten it.
///
/// `constraints.sandbox` is **always** `None`; see this module's documentation.
#[derive(Clone, Debug, Default)]
pub struct Baseline {
    pub constraints: ExecutionConstraints,
}

impl Baseline {
    /// A baseline from the limits one dispatch is carrying.
    #[must_use]
    pub fn new(constraints: ExecutionConstraints) -> Self {
        Self {
            constraints: ExecutionConstraints {
                // Whatever the caller passed on this axis is dropped rather than trusted:
                // it is the one axis a seed cannot express, and silently keeping it would
                // make a policy's provider lose.
                sandbox: None,
                ..constraints
            },
        }
    }
}

/// One trip through steps 4 and 5.
#[derive(Clone, Debug)]
pub struct Evaluated {
    pub decision: PolicyDecision,
    /// The policy or interceptor whose answer produced the final outcome.
    ///
    /// The same string lands in `tool.policy.evaluated.policy` and in
    /// `session: tool.blocked.policy`. When nobody decided — no policies registered, or
    /// every one of them allowed — the seed decided, and this is [`BASELINE_POLICY`]. An
    /// empty string would leave "why did this simply run?" unanswerable, which is the same
    /// reason the topic is published on allowed calls at all.
    pub deciding: String,
    /// How many times a rewrite sent the chain round again. `0` means no rewrite.
    pub rounds: u8,
}

impl Evaluated {
    fn denied(reason: impl Into<String>, deciding: impl Into<String>, rounds: u8) -> Self {
        Self {
            decision: PolicyDecision::deny(reason),
            deciding: deciding.into(),
            rounds,
        }
    }
}

/// Run the chain for one call.
///
/// `spec` is a parameter because a rewrite has to be re-validated against the tool's
/// schema before it is judged again, and [`PolicyRequest`] does not carry
/// `input_schema` — it carries the call and the annotations. The spec is the one fixed at
/// registration ([`crate::registry::RegisteredTool`]), which the dispatcher already holds.
///
/// `cancel` is selected on at every wait, so a cancelled run does not sit through an
/// interceptor's full budget. The caller decides what a cancelled chain means for the
/// call; the dispatcher closes it as "never started".
pub async fn evaluate(
    registry: &Registry,
    spec: &ToolSpec,
    request: PolicyRequest,
    baseline: Baseline,
    cancel: &CancellationToken,
) -> Evaluated {
    let seed = PolicyDecision::allow().with_constraints(baseline.constraints);
    let Some(original) = call_of(&request).cloned() else {
        let round = one_round(registry, &request, None, cancel).await;
        let mut deciding = BASELINE_POLICY.to_string();
        let decision = seed.clone().combine(round.decision.clone());
        if decision.severity() > seed.severity() {
            deciding = round.deciding;
        }
        return Evaluated {
            decision,
            deciding,
            rounds: 0,
        };
    };
    let mut current = original.clone();
    let mut total = seed;
    let mut deciding = BASELINE_POLICY.to_string();
    let mut rounds = 0u8;

    loop {
        let round = one_round(registry, &request, Some(&current), cancel).await;
        let (mut decision, name) = (round.decision, round.deciding);
        let rewrite = decision.rewrite.take();

        // The rewrite is held back from the accumulator: what a rewritten round decides
        // is folded in, but the call to run is the *last* rewrite, not an older one.
        let combined = total.clone().combine(decision);
        if combined.severity() > total.severity() {
            deciding = name;
        }
        total = combined;

        let Some(rewritten) = rewrite else { break };
        // A fixed point is convergence: a policy that keeps asking for the call it already
        // has has stopped changing anything, so there is nothing left to re-evaluate.
        if rewritten == current {
            break;
        }
        if !PolicyDecision::allow()
            .with_rewrite(rewritten.clone())
            .rewrite_is_wellformed(&current)
        {
            return Evaluated::denied(
                format!(
                    "a policy rewrote `{}` into a call with a different identity; \
                     that is not a narrowing of this call",
                    current.name
                ),
                deciding,
                rounds,
            );
        }
        rounds += 1;
        if rounds > REWRITE_DEPTH_LIMIT {
            return Evaluated::denied(
                format!(
                    "the policy chain did not converge on a rewrite of `{}` within \
                     {REWRITE_DEPTH_LIMIT} rounds",
                    current.name
                ),
                deciding,
                rounds,
            );
        }
        // Step 3 again, on the rewritten call. `PolicyDecision::rewrite`'s own contract
        // promises the whole pipeline is re-run, and schema validation is the half a
        // policy cannot do for itself.
        if let Err(problems) = crate::schema::validate(&spec.input_schema, &rewritten.input) {
            return Evaluated::denied(
                format!(
                    "a policy rewrote `{}` into arguments its own schema rejects: {}",
                    current.name,
                    problems.join("; ")
                ),
                deciding,
                rounds,
            );
        }
        current = rewritten;
    }

    if current != original {
        total = total.with_rewrite(current);
    }
    Evaluated {
        decision: total,
        deciding,
        rounds,
    }
}

/// Convert an approval requirement nobody can answer into a refusal.
///
/// The runtime does this, not each policy: a policy that forgot would hang CI, and the
/// `match` below has no wildcard, so a new [`Outcome`] variant has to answer "what is this
/// when there is nobody to ask" before it compiles.
///
/// Called **after** a remembered grant has been looked for. `unattended` means "nobody can
/// answer now"; a remembered grant means "somebody already did", and a flag on this run
/// must not erase a decision a person made durably.
#[must_use]
pub fn apply_unattended(decision: PolicyDecision, unattended: bool) -> PolicyDecision {
    if !unattended {
        return decision;
    }
    match decision.outcome {
        Outcome::Allow | Outcome::Deny { .. } => decision,
        Outcome::RequireApproval { reason, .. } => PolicyDecision {
            outcome: Outcome::Deny {
                reason: format!("{reason}; approval was required and nothing can ask a human"),
            },
            rewrite: decision.rewrite,
            constraints: decision.constraints,
        },
    }
}

/// The decision one round produced, and who produced it.
struct Round {
    decision: PolicyDecision,
    deciding: String,
}

/// Steps 4 and 5 for one call, folded into a single decision.
async fn one_round(
    registry: &Registry,
    request: &PolicyRequest,
    call: Option<&ToolCall>,
    cancel: &CancellationToken,
) -> Round {
    let request = match call {
        Some(call) => with_call(request, call),
        None => request.clone(),
    };
    let mut answers: Vec<(String, PolicyDecision)> = Vec::new();

    // 4 Intercept. Concurrent, one timeout each, and the whole join races the run's
    // cancellation so a stopped run does not sit through the budget.
    let interceptors = registry.interceptors().await;
    if !interceptors.is_empty() {
        let running = join_all(interceptors.iter().map(|interceptor| {
            let request = &request;
            async move {
                let name = interceptor.name().to_string();
                let outcome = tokio::time::timeout(
                    INTERCEPTOR_TIMEOUT,
                    interceptor.before_tool_call(request),
                )
                .await;
                (name, outcome)
            }
        }));
        let results = tokio::select! {
            results = running => results,
            () = cancel.cancelled() => Vec::new(),
        };
        for (name, outcome) in results {
            match outcome {
                Ok(Ok(Some(decision))) => answers.push((name, decision.into())),
                // An abstention, an error and a hang are the same answer: this extension
                // point had nothing to say. Raising an error to `Deny` lets one bug stop
                // the runtime; lowering it to `Allow` makes the extension point vanish
                // quietly. The contract already calls a hung interceptor a `None` that is
                // reported, and `tracing` is the reporting.
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(interceptor = %name, %error, "interceptor failed; abstaining");
                }
                Err(_) => {
                    tracing::warn!(
                        interceptor = %name,
                        timeout_ms = u64::try_from(INTERCEPTOR_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                        "interceptor did not answer in time; abstaining"
                    );
                }
            }
        }
    }

    // 5 Policy. Every one of them, in name order: a chain that stopped at the first
    // refusal would drop the constraints the rest of it asked for.
    for policy in registry.policies().await {
        let name = policy.name().to_string();
        let decision = match policy.evaluate(&request).await {
            Ok(decision) => decision,
            // Unlike an interceptor, a policy has no way to abstain: `Outcome` has no
            // such variant, and folding `Allow` in its place would invent consent from a
            // rule that failed to run.
            Err(error) => {
                tracing::warn!(policy = %name, %error, "policy failed to evaluate");
                PolicyDecision::deny(format!("policy `{name}` could not decide: {error}"))
            }
        };
        answers.push((name, decision));
    }

    let mut decision = PolicyDecision::allow();
    let mut deciding = BASELINE_POLICY.to_string();
    for (name, answer) in answers {
        let combined = decision.clone().combine(answer);
        if combined.severity() > decision.severity() {
            deciding = name;
        }
        decision = combined;
    }
    Round { decision, deciding }
}

/// The call a request is about, when it is about one.
///
/// The other three [`PolicyAction`] variants have no caller yet, and none of them has a
/// call to rewrite. Returning `None` lets [`evaluate`] fold one round for them and stop,
/// rather than invent a call to loop over.
fn call_of(request: &PolicyRequest) -> Option<&ToolCall> {
    match &request.action {
        PolicyAction::ToolCall { call, .. } => Some(call),
        PolicyAction::LoadPlugin { .. }
        | PolicyAction::StartRun { .. }
        | PolicyAction::AutoApproveJob { .. } => None,
    }
}

/// The same request with a rewritten call in it, so a re-evaluation sees what will run.
fn with_call(request: &PolicyRequest, call: &ToolCall) -> PolicyRequest {
    let mut copy = request.clone();
    if let PolicyAction::ToolCall { call: slot, .. } = &mut copy.action {
        *slot = call.clone();
    }
    copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::policy::RestrictiveDecision;

    fn approval() -> PolicyDecision {
        RestrictiveDecision::RequireApproval {
            reason: "destructive".into(),
            preview: "rm -rf build".into(),
            allow_remember: true,
            scope_key: "shell:rm".into(),
        }
        .into()
    }

    #[test]
    fn an_unattended_approval_becomes_a_deny_that_says_why() {
        let converted = apply_unattended(approval(), true);
        match converted.outcome {
            Outcome::Deny { reason } => {
                assert!(reason.contains("destructive"), "{reason}");
                assert!(reason.contains("ask a human"), "{reason}");
            }
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[test]
    fn an_attended_approval_is_left_alone() {
        assert_eq!(apply_unattended(approval(), false).severity(), 1);
    }

    #[test]
    fn the_conversion_touches_nothing_else() {
        assert_eq!(
            apply_unattended(PolicyDecision::allow(), true).severity(),
            0
        );
        assert_eq!(
            apply_unattended(PolicyDecision::deny("no"), true).severity(),
            2
        );
    }

    #[test]
    fn a_baseline_never_carries_a_sandbox_name() {
        // The axis a seed cannot express: `merge` resolves it left-to-right, so a seeded
        // name would beat every policy that asked for a different one.
        let baseline = Baseline::new(ExecutionConstraints {
            timeout_ms: Some(1_000),
            sandbox: Some("local".into()),
            ..ExecutionConstraints::default()
        });
        assert_eq!(baseline.constraints.sandbox, None);
        assert_eq!(baseline.constraints.timeout_ms, Some(1_000));
    }
}
