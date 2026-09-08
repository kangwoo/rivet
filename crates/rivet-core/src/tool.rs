//! The tool contract.
//!
//! A tool is a pure capability: it declares a schema, receives validated input and a
//! narrow context, and returns a result. It does **not** decide whether it is allowed to
//! run — that is [`crate::policy`] — and it does not decide *where* it runs — that is
//! [`crate::sandbox`].
//!
//! The practical consequence: a tool never calls `is_dangerous()` on itself, and a tool
//! author cannot accidentally grant their own tool more reach than the operator's profile
//! allows.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capability::PermissionSet;
use crate::id::{AgentId, RunId, SessionId, ToolCallId};
use crate::workspace::Workspace;

/// A tool's declared interface. This is what gets sent to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique within a run. Must match `^[a-zA-Z0-9_-]{1,64}$` — the intersection of what
    /// major providers accept.
    pub name: String,
    /// Written for the model, not for a human reader. Say when to use it and when not to.
    pub description: String,
    /// JSON Schema (draft 2020-12) for the input object.
    pub input_schema: serde_json::Value,
    /// Hints the runtime and UI use. Not sent to the model.
    #[serde(default)]
    pub annotations: ToolAnnotations,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> crate::Result<Self> {
        let name = name.into();
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
        if !valid {
            return Err(crate::Error::invalid_argument(format!(
                "tool name `{name}` must be 1-64 chars of [a-zA-Z0-9_-]"
            )));
        }
        if !input_schema.is_object() {
            return Err(crate::Error::invalid_argument(format!(
                "tool `{name}` input_schema must be a JSON Schema object"
            )));
        }
        Ok(Self {
            name,
            description: description.into(),
            input_schema,
            annotations: ToolAnnotations::default(),
        })
    }

    #[must_use]
    pub fn with_annotations(mut self, annotations: ToolAnnotations) -> Self {
        self.annotations = annotations;
        self
    }
}

/// Advisory metadata about a tool's behavior.
///
/// These are **hints from the tool author to the policy layer**, not enforcement. A
/// hostile or buggy plugin can lie; policy must therefore never rely on `read_only` alone
/// to permit an action. They exist so the *default* policy can be strict without a
/// hand-maintained list of every tool in existence.
/// A flag struct: the assertions are independent and a tool may make several at once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ToolAnnotations {
    /// Author asserts the tool mutates nothing.
    pub read_only: bool,
    /// Author asserts running it twice is equivalent to running it once.
    pub idempotent: bool,
    /// Author asserts effects may be irreversible (delete, push, deploy).
    pub destructive: bool,
    /// Author asserts it reaches the network.
    pub network: bool,
    /// Rough wall-clock expectation, used for progress UI and default timeouts.
    pub expected_duration_ms: Option<u64>,
}

/// A tool invocation requested by the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    /// Raw arguments as produced by the model. Not yet schema-validated.
    pub input: serde_json::Value,
}

/// The outcome of running a tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// What the model sees. Already truncated to fit a context budget.
    pub content: String,
    /// Set when the tool failed in a way the model should observe and react to (a failing
    /// test, a compile error). Distinct from returning `Err`, which means the *runtime*
    /// failed and the model may not be able to help.
    pub is_error: bool,
    /// Machine-readable detail for UIs and evaluators. Never shown to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// Set when `content` was truncated, so the UI can offer the full artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Truncation>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            structured: None,
            truncated: None,
        }
    }

    /// A failure the *model* should see and respond to.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            structured: None,
            truncated: None,
        }
    }

    #[must_use]
    pub fn with_structured(mut self, value: serde_json::Value) -> Self {
        self.structured = Some(value);
        self
    }
}

/// Records that output was cut down before reaching the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Truncation {
    pub original_bytes: u64,
    pub retained_bytes: u64,
    /// Where the full output was persisted, if anywhere.
    pub artifact_ref: Option<String>,
}

/// The part of a tool's context that is **plain data**.
///
/// Split out from the live handles below so it can be serialized. When a tool runs in
/// another process or in a WASM module, this struct is what crosses the boundary; the
/// capabilities in [`ToolHost`] become RPC calls. Keeping the split explicit now is the
/// difference between "recompile the plugin for Phase 6" and "rewrite every plugin".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolContextData {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
    /// The workspace this call is scoped to. For an out-of-process tool the root is the
    /// path *inside* its own filesystem view, which may differ from the host's.
    pub workspace: Workspace,
    /// The *effective* grant, already narrowed by profile and policy.
    ///
    /// True since Phase 4: the profile's grant is narrowed again by
    /// [`crate::policy::ExecutionConstraints::permissions`], folded across the whole
    /// chain, and the result is what lands here and in the sandbox's
    /// [`crate::sandbox::SandboxRequest::permissions`]. A tool and its confinement
    /// therefore read the same grant.
    pub permissions: PermissionSet,
    /// Wall-clock budget for this call, from [`crate::policy::ExecutionConstraints`].
    pub timeout_ms: Option<u64>,
    /// Bytes of output the runtime will accept before truncating.
    pub max_output_bytes: Option<u64>,
}

/// The live capabilities a tool may use.
///
/// Every method here is one the runtime can service locally today and over RPC later.
/// Note what is *absent*: no `Runtime`, no `ToolRegistry`, no `SessionStore`. A tool that
/// could call back into the runtime could re-enter the agent loop, bypass policy, or
/// deadlock the dispatcher. Tools that genuinely need to spawn work do it by returning a
/// result the agent acts on.
#[async_trait]
pub trait ToolHost: Send + Sync + fmt::Debug {
    /// Report progress. Fire-and-forget, like every publish in Rivet.
    fn progress(&self, message: &str);

    /// Whether the run has been cancelled. Poll this in long loops.
    fn is_cancelled(&self) -> bool;

    /// Resolve when the run is cancelled, for use in `select!`.
    async fn cancelled(&self);

    /// Run a process under whatever confinement policy chose.
    ///
    /// A tool must never spawn a process itself: going through the host is what makes the
    /// sandbox, the timeout and the output cap actually apply.
    async fn exec(
        &self,
        spec: crate::sandbox::ExecSpec,
    ) -> crate::Result<crate::sandbox::ExecOutput>;
}

/// Everything a tool is allowed to reach: serializable data plus live capabilities.
#[derive(Clone)]
pub struct ToolContext {
    pub data: ToolContextData,
    pub host: Arc<dyn ToolHost>,
}

impl ToolContext {
    #[must_use]
    pub fn new(data: ToolContextData, host: Arc<dyn ToolHost>) -> Self {
        Self { data, host }
    }

    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        &self.data.workspace
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionSet {
        &self.data.permissions
    }

    #[must_use]
    pub fn call_id(&self) -> ToolCallId {
        self.data.call_id
    }

    /// Convenience: `Err(Cancelled)` if the run has been cancelled.
    pub fn check_cancelled(&self) -> crate::Result<()> {
        if self.host.is_cancelled() {
            Err(crate::Error::cancelled("run cancelled"))
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

/// An executable capability offered to the model.
#[async_trait]
pub trait Tool: Send + Sync + fmt::Debug {
    fn spec(&self) -> ToolSpec;

    /// Run the tool.
    ///
    /// Contract:
    /// - `input` has already been validated against `spec().input_schema`.
    /// - Return `Ok(ToolResult { is_error: true, .. })` for failures the model should
    ///   read and react to. Return `Err` only when the runtime itself could not execute.
    /// - Honor cancellation via `ctx.host`. A tool that ignores it wedges the whole run.
    /// - Spawn processes through `ctx.host.exec`, never directly: that is where the
    ///   sandbox, the timeout and the output cap are applied.
    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> crate::Result<ToolResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_constrained_to_the_provider_intersection() {
        let schema = serde_json::json!({ "type": "object" });
        assert!(ToolSpec::new("read_file", "d", schema.clone()).is_ok());
        assert!(
            ToolSpec::new("read.file", "d", schema.clone()).is_err(),
            "dot rejected"
        );
        assert!(
            ToolSpec::new("", "d", schema.clone()).is_err(),
            "empty rejected"
        );
        assert!(
            ToolSpec::new("a".repeat(65), "d", schema).is_err(),
            "too long"
        );
    }

    #[test]
    fn schema_must_be_an_object() {
        assert!(ToolSpec::new("t", "d", serde_json::json!("string")).is_err());
        assert!(ToolSpec::new("t", "d", serde_json::json!({"type": "object"})).is_ok());
    }

    #[test]
    fn tool_context_data_survives_a_process_boundary() {
        // Phase 6 moves tools out of process. If this stops compiling or round-tripping,
        // the split between data and live capabilities has been broken.
        let data = ToolContextData {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            run_id: RunId::new(),
            call_id: ToolCallId::new(),
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            permissions: PermissionSet::empty(),
            timeout_ms: Some(30_000),
            max_output_bytes: Some(65_536),
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: ToolContextData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.call_id, data.call_id);
        assert_eq!(back.timeout_ms, Some(30_000));
        assert_eq!(back.workspace.root(), data.workspace.root());
    }

    #[test]
    fn model_visible_failure_is_not_a_runtime_error() {
        let result = ToolResult::error("test failed: 3 assertions");
        assert!(result.is_error);
        assert!(result.content.contains("3 assertions"));
    }
}
