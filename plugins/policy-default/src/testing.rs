//! Request builders shared by this crate's unit tests.
//!
//! A policy is a pure function of a [`PolicyRequest`], so a test needs one and nothing
//! else — no registry, no runtime, no filesystem. These build one.

use rivet_core::capability::{FsScope, Permission, PermissionSet};
use rivet_core::id::{AgentId, RunId, SessionId, ToolCallId};
use rivet_core::policy::{PolicyAction, PolicyRequest};
use rivet_core::tool::{ToolAnnotations, ToolCall};
use rivet_core::workspace::Workspace;

/// A workspace with the deny list the shipped example uses.
///
/// Not canonicalized and not on disk: `Workspace::resolve` is lexical, which is exactly the
/// property `default.workspace` rests on.
#[must_use]
pub fn workspace() -> Workspace {
    Workspace::new(std::path::PathBuf::from("/repo"))
        .with_denied([".env".to_string(), ".git/config".to_string()])
        .expect("the literal deny patterns compile")
}

/// A `read_only` / `destructive` pair, spelled once.
#[must_use]
pub fn annotated(read_only: bool, destructive: bool) -> ToolAnnotations {
    ToolAnnotations {
        read_only,
        destructive,
        ..ToolAnnotations::default()
    }
}

/// A `developer` request for `tool` with `input`, and no annotations declared.
#[must_use]
pub fn request_for(tool: &str, input: serde_json::Value) -> PolicyRequest {
    request_annotated(tool, input, ToolAnnotations::default())
}

/// A `developer` request with annotations of its own.
#[must_use]
pub fn request_annotated(
    tool: &str,
    input: serde_json::Value,
    annotations: ToolAnnotations,
) -> PolicyRequest {
    request_in_profile("developer", tool, input, annotations)
}

/// A request in a named profile, with the grant that profile would normally compute.
#[must_use]
pub fn request_in_profile(
    profile: &str,
    tool: &str,
    input: serde_json::Value,
    annotations: ToolAnnotations,
) -> PolicyRequest {
    let permissions = match profile {
        "developer" | "ci" => PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
        ]),
        _ => PermissionSet::new([Permission::FsRead(FsScope::Workspace)]),
    };
    let mut request = request_with(tool, permissions, annotations);
    request.profile = profile.to_string();
    if let PolicyAction::ToolCall { call, .. } = &mut request.action {
        call.input = input;
    }
    request
}

/// A request with an explicit grant, for the tests that vary that axis alone.
#[must_use]
pub fn request_with(
    tool: &str,
    permissions: PermissionSet,
    annotations: ToolAnnotations,
) -> PolicyRequest {
    PolicyRequest {
        action: PolicyAction::ToolCall {
            call: ToolCall {
                id: ToolCallId::new(),
                name: tool.to_string(),
                input: serde_json::json!({}),
            },
            annotations,
        },
        session_id: SessionId::new(),
        agent_id: AgentId::new(),
        run_id: RunId::new(),
        workspace: workspace(),
        permissions,
        profile: "developer".to_string(),
        unattended: false,
    }
}
