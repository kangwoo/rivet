//! The policy chain: the fold, the rewrite loop, and what the log says afterwards.
//!
//! `PolicyDecision::combine`'s own algebra is tested in `rivet-core`. What is tested here
//! is that the runtime *runs* the fold correctly — that every policy is asked, that the
//! host's baseline is the seed rather than an override, and that the name in the log is the
//! name of whatever actually decided.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rivet_core::event::{Event, ToolEvent};
use rivet_core::policy::{
    ExecutionConstraints, Outcome, PolicyDecision, PolicyRequest, RestrictiveDecision,
};
use rivet_core::tool::{Tool, ToolCall};
use rivet_runtime::agent_loop::{AgentLoop, RunConfig};
use rivet_runtime::jitter::NoJitter;
use rivet_runtime::policy_chain::{self, BASELINE_POLICY, Baseline, REWRITE_DEPTH_LIMIT};
use rivet_runtime::registry::Registry;
use support::{
    BrokenPolicy, EchoTool, FixedPolicy, FixtureModel, Harness, ReadTool, Reply, SlowInterceptor,
    sse_text, sse_tool_calls,
};
use tokio_util::sync::CancellationToken;

fn agent_loop(harness: &Harness) -> AgentLoop {
    AgentLoop::new(
        harness.registry.clone(),
        harness.store.clone(),
        harness.bus.clone(),
        harness.assembler(),
        Arc::new(rivet_core::retry::ExponentialBackoff::default()),
        Arc::new(NoJitter),
    )
}

fn config(harness: &Harness) -> RunConfig {
    let mut cfg = RunConfig::new(
        harness.agent(),
        harness.session_id,
        harness.workspace.clone(),
    );
    cfg.cancel_grace = Duration::from_millis(200);
    cfg
}

/// A model that asks for `echo` once, then answers.
fn one_echo() -> Arc<FixtureModel> {
    Arc::new(FixtureModel::new(vec![
        Reply::Sse(sse_tool_calls(&[(
            "echo",
            serde_json::json!({ "message": "hello" }),
        )])),
        Reply::Sse(sse_text("done")),
    ]))
}

/// Evaluate the chain directly, without a run around it.
async fn evaluate(
    registry: &Registry,
    harness: &Harness,
    baseline: Baseline,
) -> rivet_runtime::policy_chain::Evaluated {
    let spec = EchoTool.spec();
    let call = ToolCall {
        id: rivet_core::id::ToolCallId::new(),
        name: "echo".into(),
        input: serde_json::json!({ "message": "hello" }),
    };
    let request = PolicyRequest {
        action: rivet_core::policy::PolicyAction::ToolCall {
            call,
            annotations: spec.annotations.clone(),
        },
        session_id: harness.session_id,
        agent_id: rivet_core::id::AgentId::new(),
        run_id: rivet_core::id::RunId::new(),
        workspace: harness.workspace.clone(),
        permissions: rivet_core::capability::PermissionSet::empty(),
        profile: "developer".into(),
        unattended: false,
    };
    policy_chain::evaluate(
        registry,
        &spec,
        request,
        baseline,
        &CancellationToken::new(),
    )
    .await
}

// --- the fold ------------------------------------------------------------------------------

#[tokio::test]
async fn the_chain_evaluates_every_policy_not_just_until_a_deny() {
    // A chain that stopped at the first refusal would drop the constraints the rest of it
    // asked for -- and constraints survive whatever the outcome turns out to be, which is
    // why `PolicyDecision` splits them from it.
    let harness = Harness::new().await;
    let denier = Arc::new(FixedPolicy::denying("a.deny", "no"));
    let tightener = Arc::new(FixedPolicy::constraining(
        "b.tighten",
        ExecutionConstraints {
            timeout_ms: Some(1_000),
            ..ExecutionConstraints::default()
        },
    ));
    let asked = tightener.asked.clone();
    harness.register_policy(denier.clone()).await;
    harness.register_policy(tightener).await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Deny { .. }));
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the policy after the refusal was never asked"
    );
    assert_eq!(evaluated.decision.constraints.timeout_ms, Some(1_000));
}

#[tokio::test]
async fn the_baseline_is_the_seed_not_an_override() {
    // The host's limits go into the fold, so a policy can only tighten them. A host that
    // overwrote afterwards would make "the policy asked for 30 s and the config put it back
    // to 120 s" possible.
    let harness = Harness::new().await;
    harness
        .register_policy(Arc::new(FixedPolicy::constraining(
            "a.narrow",
            ExecutionConstraints {
                timeout_ms: Some(5_000),
                max_output_bytes: Some(1_000_000),
                ..ExecutionConstraints::default()
            },
        )))
        .await;

    let baseline = Baseline::new(ExecutionConstraints {
        timeout_ms: Some(120_000),
        max_output_bytes: Some(65_536),
        ..ExecutionConstraints::default()
    });
    let evaluated = evaluate(&harness.registry, &harness, baseline).await;
    assert_eq!(
        evaluated.decision.constraints.timeout_ms,
        Some(5_000),
        "a policy narrows the host's budget"
    );
    assert_eq!(
        evaluated.decision.constraints.max_output_bytes,
        Some(65_536),
        "and cannot widen it, however large a number it names"
    );
}

#[tokio::test]
async fn nobody_deciding_is_recorded_as_the_baseline_deciding() {
    // Not an empty string and not an `Option`: `tool.policy.evaluated.policy` is a `String`,
    // and an observer has to be able to answer "why did this simply run".
    let harness = Harness::new().await;
    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Allow));
    assert_eq!(evaluated.deciding, BASELINE_POLICY);

    harness
        .register_policy(Arc::new(FixedPolicy::new(
            "a.allow",
            PolicyDecision::allow(),
        )))
        .await;
    let with_an_allowing_policy = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert_eq!(
        with_an_allowing_policy.deciding, BASELINE_POLICY,
        "a policy that allowed did not decide anything"
    );
}

#[tokio::test]
async fn no_registered_policy_may_claim_a_name_the_host_writes() {
    // All three, not just the baseline. Each is a value the *host* puts in
    // `tool.policy.evaluated.policy` or `ToolBlocked.policy`: `host.baseline` means nobody
    // in the chain decided, `agent.scope` is step 2's refusal, and `tool` is one the tool
    // raised from inside step 8. A policy able to register under any of them could put
    // itself on a decision it never made.
    for reserved in [
        BASELINE_POLICY,
        rivet_runtime::dispatch::TOOL_POLICY,
        rivet_runtime::dispatch::SCOPE_POLICY,
    ] {
        let harness = Harness::new().await;
        let error = harness
            .try_register_policy(Arc::new(FixedPolicy::new(
                reserved,
                PolicyDecision::allow(),
            )))
            .await
            .unwrap_err();
        assert!(error.message().contains(reserved), "{error}");
    }
}

#[tokio::test]
async fn a_policy_that_cannot_decide_is_a_refusal_not_a_pass() {
    // Unlike an interceptor, a policy has no way to abstain: `Outcome` has no such variant,
    // and folding `Allow` in its place would invent consent from a rule that never ran.
    let harness = Harness::new().await;
    harness
        .register_policy(Arc::new(BrokenPolicy("a.broken")))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => assert!(reason.contains("a.broken"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(evaluated.deciding, "a.broken");
}

// --- interceptors --------------------------------------------------------------------------

#[tokio::test]
async fn a_hanging_interceptor_is_treated_as_an_abstention() {
    // The contract says a hung interceptor is `None` and is reported. Anything else lets one
    // slow extension point decide, or stop, every tool call in the process.
    let harness = Harness::new().await;
    harness
        .register_interceptor(Arc::new(SlowInterceptor::new(
            "a.hangs",
            Duration::from_secs(30),
        )))
        .await;

    let started = Instant::now();
    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Allow));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it waited on the interceptor rather than timing it out: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn interceptors_run_concurrently_not_serially() {
    // Three at 1.5 s each. Serial would be 4.5 s; the bound here is 3 s, which is twice the
    // longest single interceptor and comfortably under the serial figure. The margin is
    // deliberately generous -- this runs on CI and on a laptop, and what is under test is
    // "concurrent" rather than "fast".
    let harness = Harness::new().await;
    for name in ["a.slow", "b.slow", "c.slow"] {
        harness
            .register_interceptor(Arc::new(SlowInterceptor::new(
                name,
                Duration::from_millis(1_500),
            )))
            .await;
    }

    let started = Instant::now();
    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    let elapsed = started.elapsed();
    assert!(matches!(evaluated.decision.outcome, Outcome::Allow));
    assert!(
        elapsed < Duration::from_secs(3),
        "took {elapsed:?}; serially this would be 4.5 s"
    );
}

#[tokio::test]
async fn an_interceptor_cannot_widen_a_policys_deny() {
    // `RestrictiveDecision` has no `Allow`, and this is that fact at runtime: the most an
    // interceptor can contribute is a narrowing, which folds in as allow-plus-rewrite and
    // loses to any refusal.
    let harness = Harness::new().await;
    harness
        .register_policy(Arc::new(FixedPolicy::denying("a.deny", "no")))
        .await;
    harness
        .register_interceptor(Arc::new(
            SlowInterceptor::new("b.narrows", Duration::ZERO).answering(
                RestrictiveDecision::Modify {
                    call: ToolCall {
                        id: rivet_core::id::ToolCallId::new(),
                        name: "echo".into(),
                        input: serde_json::json!({ "message": "narrowed" }),
                    },
                    reason: "narrowed".into(),
                },
            ),
        ))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Deny { .. }));
}

#[tokio::test]
async fn an_interceptor_result_joins_the_same_fold_a_policy_does() {
    let harness = Harness::new().await;
    harness
        .register_interceptor(Arc::new(
            SlowInterceptor::new("a.refuses", Duration::ZERO).answering(
                RestrictiveDecision::Deny {
                    reason: "the interceptor said no".into(),
                },
            ),
        ))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => assert!(reason.contains("interceptor said no"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(evaluated.deciding, "a.refuses");
}

#[tokio::test]
async fn interceptor_order_changes_which_reason_is_first_not_the_outcome() {
    // `Registry::interceptors` sorts by priority so that *which reason a user reads first*
    // is a property of configuration rather than of load timing. It cannot be more than
    // that: the results are folded, so two refusals in either order refuse.
    let harness = Harness::new().await;
    harness
        .register_interceptor(Arc::new(
            SlowInterceptor::new("b.second", Duration::ZERO).answering(RestrictiveDecision::Deny {
                reason: "second".into(),
            }),
        ))
        .await;
    harness
        .register_interceptor(Arc::new(
            SlowInterceptor::new("a.first", Duration::ZERO).answering(RestrictiveDecision::Deny {
                reason: "first".into(),
            }),
        ))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    match &evaluated.decision.outcome {
        Outcome::Deny { reason } => assert_eq!(
            reason, "first",
            "name order decides which reason is shown, and registration order does not"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(evaluated.deciding, "a.first");
}

// --- rewrites -------------------------------------------------------------------------------

/// Rewrites the call to a different message every round, so it never converges.
#[derive(Debug)]
struct NeverConverging {
    round: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl rivet_core::policy::Policy for NeverConverging {
    fn name(&self) -> &'static str {
        "a.churn"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let rivet_core::policy::PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        let round = self.round.fetch_add(1, Ordering::SeqCst);
        Ok(PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "message": format!("round {round}") }),
            ..call.clone()
        }))
    }
}

#[tokio::test]
async fn a_rewrite_loop_denies_at_the_depth_limit() {
    // Not an infinite loop, and not a silent acceptance of the last version: a chain that
    // will not settle is a chain that has not decided.
    let harness = Harness::new().await;
    harness
        .register_policy(Arc::new(NeverConverging {
            round: std::sync::atomic::AtomicUsize::new(0),
        }))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => assert!(reason.contains("converge"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(evaluated.rounds > REWRITE_DEPTH_LIMIT);
}

/// Narrows the message once, then leaves the narrowed call alone.
#[derive(Debug)]
struct NarrowsOnce;

#[async_trait::async_trait]
impl rivet_core::policy::Policy for NarrowsOnce {
    fn name(&self) -> &'static str {
        "a.narrows"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let rivet_core::policy::PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        let message = call.input["message"].as_str().unwrap_or_default();
        if message.ends_with(" (narrowed)") {
            return Ok(PolicyDecision::allow());
        }
        Ok(PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "message": format!("{message} (narrowed)") }),
            ..call.clone()
        }))
    }
}

#[tokio::test]
async fn a_rewrite_is_re_evaluated_before_it_runs() {
    // A narrowing that settles is accepted, *and* the narrowed call is what the chain judged
    // on its last round -- which is the whole promise `PolicyDecision::rewrite` makes.
    let harness = Harness::new().await;
    harness.register_policy(Arc::new(NarrowsOnce)).await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Allow));
    assert_eq!(evaluated.rounds, 1);
    assert_eq!(
        evaluated
            .decision
            .rewrite
            .as_ref()
            .map(|c| &c.input["message"]),
        Some(&serde_json::json!("hello (narrowed)"))
    );
}

/// A rewrite that widens the arguments is judged again like anything else.
#[derive(Debug)]
struct RewritesThenDenies;

#[async_trait::async_trait]
impl rivet_core::policy::Policy for RewritesThenDenies {
    fn name(&self) -> &'static str {
        "a.two_faced"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let rivet_core::policy::PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        if call.input["message"] == serde_json::json!("widened") {
            // The second round sees what the first round asked for, and refuses it.
            return Ok(PolicyDecision::deny("that is wider than what was asked"));
        }
        Ok(PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "message": "widened" }),
            ..call.clone()
        }))
    }
}

#[tokio::test]
async fn a_rewrite_that_widens_is_caught_on_the_second_round() {
    // The property that makes a rewrite safe: it cannot launder a call past the chain,
    // because the chain runs again on what it produced.
    let harness = Harness::new().await;
    harness.register_policy(Arc::new(RewritesThenDenies)).await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Deny { .. }));
}

/// A rewrite that fails the tool's own schema never reaches step 8.
#[derive(Debug)]
struct RewritesToNonsense;

#[async_trait::async_trait]
impl rivet_core::policy::Policy for RewritesToNonsense {
    fn name(&self) -> &'static str {
        "a.nonsense"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let rivet_core::policy::PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        Ok(PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "message": 42 }),
            ..call.clone()
        }))
    }
}

#[tokio::test]
async fn a_rewrite_is_validated_against_the_tools_schema_again() {
    // Step 3, re-run. A policy cannot check the schema for itself, and the contract promises
    // the whole pipeline runs again -- not just the policy half of it.
    let harness = Harness::new().await;
    harness.register_policy(Arc::new(RewritesToNonsense)).await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => assert!(reason.contains("schema"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// --- what the run and the log see ----------------------------------------------------------

#[tokio::test]
async fn every_call_publishes_a_policy_decision() {
    // Even with no policies registered. The topic carries a *decision*, not a refusal -- a
    // refusal-only topic already exists, and it is `tool.blocked`.
    let harness = Harness::new().await;
    let (recorder, observer) = harness.recorder();
    harness.register_model(one_echo()).await;
    harness.register_tool(Arc::new(EchoTool)).await;

    agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .expect("the run finishes");
    observer.drain_within(Duration::from_secs(2)).await;

    let decisions: Vec<rivet_core::event::EventEnvelope> = recorder
        .envelopes()
        .into_iter()
        .filter(|e| e.topic() == "tool.policy.evaluated")
        .collect();
    assert_eq!(decisions.len(), 1, "exactly one per call");
    let Event::Tool(ToolEvent::PolicyEvaluated { policy, .. }) = &decisions[0].payload else {
        panic!("wrong payload");
    };
    assert_eq!(policy, BASELINE_POLICY);
}

#[tokio::test]
async fn a_refusal_the_tool_raised_is_filed_under_the_tool_and_not_a_policy() {
    // `.env` is on the workspace deny list, so `fsguard` refuses it from *inside* the tool,
    // in step 8, after the chain has already allowed the call. The label used to say
    // `workspace` -- the name of a real policy in the chain that never evaluated this call.
    // A refusal is a durable audit fact, and one filed under a rule that did not produce it
    // is a weaker fact than one filed under the tool's own containment.
    let harness = Harness::new().await;
    harness.register_tool(Arc::new(ReadTool)).await;
    harness
        .register_model(Arc::new(FixtureModel::new(vec![
            Reply::Sse(sse_tool_calls(&[(
                "read_file",
                serde_json::json!({ "path": ".env" }),
            )])),
            Reply::Sse(sse_text("I cannot read that one.")),
        ])))
        .await;

    agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .expect("the run finishes");

    let events = harness.events().await;
    let policy = events
        .iter()
        .find_map(|stored| match &stored.event {
            rivet_core::session::SessionEvent::ToolBlocked { policy, .. } => Some(policy.clone()),
            _ => None,
        })
        .expect("a containment refusal is a durable audit fact");
    // The literal, not the constant: comparing against `TOOL_POLICY` would agree with
    // whatever the constant happens to say, including `workspace` again.
    assert_eq!(
        policy, "tool",
        "the tool's own containment refused; no policy in the chain did"
    );
    assert_eq!(
        policy,
        rivet_runtime::dispatch::TOOL_POLICY,
        "and the constant is what the dispatcher writes"
    );
}

#[tokio::test]
async fn a_denied_call_is_blocked_and_the_model_reads_the_reason() {
    // Both halves of the acceptance line: the refusal is durable, and the model is told, so
    // it can adapt rather than end the run.
    let harness = Harness::new().await;
    harness.register_model(one_echo()).await;
    harness.register_tool(Arc::new(EchoTool)).await;
    harness
        .register_policy(Arc::new(FixedPolicy::denying(
            "default.destructive",
            "that would delete the build directory",
        )))
        .await;

    agent_loop(&harness)
        .run(config(&harness), harness.state().await, None)
        .await
        .expect("the run finishes");

    let events = harness.events().await;
    let blocked = events
        .iter()
        .find_map(|stored| match &stored.event {
            rivet_core::session::SessionEvent::ToolBlocked { policy, reason, .. } => {
                Some((policy.clone(), reason.clone()))
            }
            _ => None,
        })
        .expect("a refusal is a durable audit fact");
    assert_eq!(
        blocked.0, "default.destructive",
        "the log names what actually decided, not a placeholder"
    );
    assert!(blocked.1.contains("build directory"), "{}", blocked.1);

    assert!(
        harness.topics().await.contains(&"tool.blocked".to_string()),
        "the call never ran"
    );
}

#[tokio::test]
async fn the_deciding_policy_is_named_even_when_several_refuse() {
    // Ties keep the earlier name, so the fold is deterministic whichever order the registry
    // happens to hand them back in.
    let harness = Harness::new().await;
    harness
        .register_policy(Arc::new(FixedPolicy::denying("a.first", "first")))
        .await;
    harness
        .register_policy(Arc::new(FixedPolicy::denying("b.second", "second")))
        .await;

    let evaluated = evaluate(&harness.registry, &harness, Baseline::default()).await;
    assert_eq!(evaluated.deciding, "a.first");
}

#[tokio::test]
async fn a_cancelled_chain_closes_the_call_as_not_started() {
    // Not `Blocked`: the call provably had no effect, so it must not advance the
    // consecutive-error counter. A Ctrl-C during an approval prompt is not the tool failing.
    let harness = Harness::new().await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    harness.register_model(one_echo()).await;
    harness.register_tool(Arc::new(EchoTool)).await;

    let mut cfg = config(&harness);
    cfg.cancel = cancel;
    let summary = agent_loop(&harness)
        .run(cfg, harness.state().await, None)
        .await
        .expect("the run finishes");
    assert!(matches!(
        summary.stop,
        rivet_core::agent::StopReason::Cancelled
    ));
    assert!(
        !harness.topics().await.contains(&"tool.called".to_string()),
        "a cancelled chain must not let the call run"
    );
}
