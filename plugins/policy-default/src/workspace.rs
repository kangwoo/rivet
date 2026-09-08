//! `default.workspace`: path-shaped arguments, checked before anything opens them.
//!
//! This does not replace `rivet_runtime::fsguard`, and the two are not the same check.
//! `fsguard` re-verifies *after* an open, which is the only way to catch a symlink; it can
//! only run inside a tool that actually opens something. This one runs in the chain, has no
//! filesystem at all, and therefore also covers the tools that never open a file — a
//! process's `cwd`, a `git` path argument.
//!
//! # Which arguments are paths
//!
//! [`PATH_KEYS`]. The schema does not say: `ToolSpec.input_schema` describes types, not
//! meanings, so a tool that calls its path `target_file` is invisible here. That limit is
//! written down in `docs/plugin.md` rather than papered over — the real fix is an
//! annotation in the schema vocabulary, and that waits for a tool author who needs it.

use async_trait::async_trait;
use rivet_core::policy::{PolicyAction, PolicyDecision, PolicyRequest};
use serde_json::Value;

/// Input keys treated as paths.
///
/// Declared rather than inferred, and short on purpose: a key that is *sometimes* a path is
/// worse than one that never is, because a false positive refuses a legitimate call with a
/// containment error nobody can act on.
pub const PATH_KEYS: [&str; 2] = ["cwd", "path"];

/// Refuses a call whose path-shaped arguments leave the workspace.
#[derive(Clone, Copy, Debug)]
pub struct WorkspacePolicy;

#[async_trait]
impl rivet_core::policy::Policy for WorkspacePolicy {
    fn name(&self) -> &'static str {
        "default.workspace"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let PolicyAction::ToolCall { call, .. } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        let Some(input) = call.input.as_object() else {
            return Ok(PolicyDecision::allow());
        };

        for key in PATH_KEYS {
            let Some(value) = input.get(key) else {
                continue;
            };
            for candidate in strings_in(value) {
                if let Err(error) = request.workspace.resolve(std::path::Path::new(&candidate)) {
                    return Ok(PolicyDecision::deny(format!(
                        "`{}` was given `{key}` = `{candidate}`, which is refused: {}",
                        call.name,
                        error.message()
                    )));
                }
            }
        }
        Ok(PolicyDecision::allow())
    }
}

/// Every string in a value, so an argument that is a list of paths is checked element by
/// element rather than skipped for not being a string.
fn strings_in(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(items) => items.iter().flat_map(strings_in).collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{request_for, workspace};
    use rivet_core::policy::{Outcome, Policy};

    async fn decide(input: serde_json::Value) -> PolicyDecision {
        WorkspacePolicy
            .evaluate(&request_for("shell", input))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_path_inside_the_workspace_is_allowed() {
        let decision = decide(serde_json::json!({ "path": "src/main.rs" })).await;
        assert!(matches!(decision.outcome, Outcome::Allow));
    }

    #[tokio::test]
    async fn a_path_that_escapes_is_denied_and_the_reason_names_the_key() {
        let decision = decide(serde_json::json!({ "path": "../../etc/passwd" })).await;
        match decision.outcome {
            Outcome::Deny { reason } => {
                assert!(reason.contains("path"), "{reason}");
                assert!(reason.contains("../../etc/passwd"), "{reason}");
            }
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_cwd_that_escapes_is_denied_too() {
        // The key that matters for a process: a tool that opens nothing never reaches
        // `fsguard`, so this is the only lexical check it gets before the sandbox.
        let decision = decide(serde_json::json!({ "command": "ls", "cwd": "/etc" })).await;
        assert!(matches!(decision.outcome, Outcome::Deny { .. }));
    }

    #[tokio::test]
    async fn a_denied_glob_is_refused_by_name() {
        let decision = decide(serde_json::json!({ "path": ".env" })).await;
        match decision.outcome {
            Outcome::Deny { reason } => assert!(reason.contains("deny list"), "{reason}"),
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_element_of_an_array_argument_is_checked() {
        let decision = decide(serde_json::json!({ "path": ["src", "../outside"] })).await;
        match decision.outcome {
            Outcome::Deny { reason } => assert!(reason.contains("../outside"), "{reason}"),
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_path_argument_is_not_treated_as_one() {
        // `command` holds a shell string, not a path. Reading it as one would refuse
        // `git log ../HEAD` with a containment error that means nothing.
        let decision = decide(serde_json::json!({ "command": "cat ../../.env" })).await;
        assert!(
            matches!(decision.outcome, Outcome::Allow),
            "the lexical layer does not read shell strings; see the crate docs"
        );
    }

    #[tokio::test]
    async fn the_policy_never_touches_the_filesystem() {
        // A path that does not exist is judged the same as one that does: this layer is
        // lexical, which is what makes an audit able to replay it from the log.
        let ws = workspace();
        assert!(ws.resolve(std::path::Path::new("nothing/here.rs")).is_ok());
        let decision = decide(serde_json::json!({ "path": "nothing/here.rs" })).await;
        assert!(matches!(decision.outcome, Outcome::Allow));
    }
}
