//! The three default policies, through the **real** chain rather than called directly.
//!
//! # Why these live here and not in `rivet-runtime`
//!
//! `default.grant` is defined in this crate, and the runtime cannot reach it: the dependency
//! arrow in this repository runs `plugin -> rivet-runtime` (`tool-filesystem` has had that
//! edge since Phase 1, for `fsguard`), and the reverse is not made even in tests. A
//! dev-dependency the *other* way is the same direction, so the chain runs here.
//!
//! # Why through the chain at all
//!
//! Because the claim is about what happens to a call, not about what a function returns.
//! `Registry::scoped` is public, so the tools are put into a real registry **without going
//! through the plugin loader** — which is the point: the registration layer is what Phase 2
//! built, and this proves the policy layer holds when something registers a write tool
//! anyway.

use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, PluginId, PluginInstanceId, RunId, SessionId, ToolCallId};
use rivet_core::plugin::PluginRegistry;
use rivet_core::policy::{Outcome, PolicyAction, PolicyRequest};
use rivet_core::tool::{Tool, ToolAnnotations, ToolCall, ToolContext, ToolResult, ToolSpec};
use rivet_core::workspace::Workspace;
use rivet_policy_default::{DefaultPolicyPlugin, Settings};
use rivet_runtime::policy_chain::{self, Baseline};
use rivet_runtime::registry::{Owner, Registry};
use tokio_util::sync::CancellationToken;

/// A tool shaped like `write_file`: it declares that it mutates, and it takes a path.
#[derive(Debug)]
struct WriteShaped;

#[async_trait]
impl Tool for WriteShaped {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write_file",
            "writes a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        )
        .unwrap()
        .with_annotations(ToolAnnotations {
            read_only: false,
            destructive: true,
            ..ToolAnnotations::default()
        })
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        Ok(ToolResult::ok("written"))
    }
}

/// A registry holding the three default policies and `write_file`.
async fn registry() -> Registry {
    let registry = Registry::new(rivet_runtime::BroadcastBus::new());
    let scoped = registry.scoped(Owner {
        plugin_id: PluginId::new("rivet.policy-default").unwrap(),
        instance_id: PluginInstanceId::new(),
    });
    for policy in DefaultPolicyPlugin::policies(&Settings::default()) {
        scoped.register_policy(policy).await.expect("policy");
    }
    // Registered by hand, past the loader. Phase 2's answer to `readonly` is that this
    // registration never happens; the question here is what happens when it does.
    registry
        .scoped(Owner {
            plugin_id: PluginId::new("acme.tool-write").unwrap(),
            instance_id: PluginInstanceId::new(),
        })
        .register_tool(Arc::new(WriteShaped))
        .await
        .expect("tool");
    registry
}

fn grant(profile: &str) -> PermissionSet {
    let mut granted = vec![Permission::FsRead(FsScope::Workspace)];
    if matches!(profile, "developer" | "ci") {
        granted.push(Permission::FsWrite(FsScope::Workspace));
    }
    PermissionSet::new(granted)
}

/// Run the whole chain for a `write_file` call under `profile`.
async fn write_under(profile: &str, input: serde_json::Value) -> policy_chain::Evaluated {
    let registry = registry().await;
    let spec = WriteShaped.spec();
    let request = PolicyRequest {
        action: PolicyAction::ToolCall {
            call: ToolCall {
                id: ToolCallId::new(),
                name: "write_file".into(),
                input,
            },
            annotations: spec.annotations.clone(),
        },
        session_id: SessionId::new(),
        agent_id: AgentId::new(),
        run_id: RunId::new(),
        workspace: Workspace::new(std::path::PathBuf::from("/repo"))
            .with_denied([".env".to_string()])
            .unwrap(),
        permissions: grant(profile),
        profile: profile.to_string(),
        unattended: false,
    };
    policy_chain::evaluate(
        &registry,
        &spec,
        request,
        Baseline::default(),
        &CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn a_readonly_profile_denies_a_write_even_if_the_tool_is_registered() {
    // The policy layer of "readonly really refuses a write". The registration layer is
    // deliberately bypassed here: it answers "the model is never offered the tool", and it
    // cannot answer "what if some other plugin registers one".
    let evaluated = write_under(
        "readonly",
        serde_json::json!({ "path": "src/main.rs", "content": "x" }),
    )
    .await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => {
            assert!(reason.contains("fs_write"), "{reason}");
            assert!(reason.contains("readonly"), "{reason}");
        }
        other => panic!("a registered write tool ran under `readonly`: {other:?}"),
    }
    assert_eq!(evaluated.deciding, "default.grant");
}

#[tokio::test]
async fn the_same_call_runs_under_a_writable_profile() {
    // The other half: the rule refuses a write under a grant with none, not every write.
    let evaluated = write_under(
        "developer",
        serde_json::json!({ "path": "src/main.rs", "content": "x" }),
    )
    .await;
    assert!(
        matches!(evaluated.decision.outcome, Outcome::Allow),
        "{:?}",
        evaluated.decision.outcome
    );
}

#[tokio::test]
async fn the_three_default_policies_fold_to_the_strictest() {
    // `production` puts every call in front of a person (`RequireApproval`) *and* grants no
    // write (`Deny`). Most-restrictive-wins means the refusal, and the name in the log is
    // the policy that produced it.
    let evaluated = write_under(
        "production",
        serde_json::json!({ "path": "src/main.rs", "content": "x" }),
    )
    .await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Deny { .. }));
    assert_eq!(
        evaluated.deciding, "default.grant",
        "the refusal outranks the approval, and the log names what refused"
    );
}

#[tokio::test]
async fn the_default_policy_refuses_an_escaping_cwd_before_the_sandbox_sees_it() {
    // The lexical layer, and it has no filesystem at all -- which is what lets it cover the
    // tools that never open a file. `fsguard` is the *other* layer, after the open; the two
    // are independent and this test proves the first one alone.
    let evaluated = write_under(
        "developer",
        serde_json::json!({ "path": "../../etc/passwd", "content": "x" }),
    )
    .await;
    match evaluated.decision.outcome {
        Outcome::Deny { reason } => assert!(reason.contains("escapes"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(evaluated.deciding, "default.workspace");
}

#[tokio::test]
async fn a_denied_path_is_refused_by_the_lexical_layer_too() {
    let evaluated = write_under(
        "developer",
        serde_json::json!({ "path": ".env", "content": "x" }),
    )
    .await;
    assert!(matches!(evaluated.decision.outcome, Outcome::Deny { .. }));
    assert_eq!(evaluated.deciding, "default.workspace");
}
