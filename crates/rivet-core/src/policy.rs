//! The policy contract: the decision layer between "the model asked" and "it happened".
//!
//! Policies are evaluated as a chain. The composition rule is **most restrictive wins**,
//! and it is defined here rather than left to each host, because a security decision that
//! depends on registration order is not a security decision.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capability::PermissionSet;
use crate::id::{AgentId, ApprovalId, RunId, SessionId, TaskId};
use crate::tool::{ToolAnnotations, ToolCall};
use crate::workspace::Workspace;

/// What the runtime is asking permission for.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PolicyAction {
    /// Run a tool. The most common case.
    ToolCall {
        call: ToolCall,
        /// The tool author's self-declared behavior. Advisory — see [`ToolAnnotations`].
        annotations: ToolAnnotations,
    },
    /// Load a plugin at startup or hot-reload.
    LoadPlugin {
        plugin_id: String,
        requested: PermissionSet,
    },
    /// Start an agent run for a task.
    StartRun {
        task_id: Option<TaskId>,
        model: String,
    },
    /// Move a task to a terminal state without human sign-off.
    AutoApproveTask { task_id: TaskId },
}

/// Everything a policy may consider.
///
/// A policy is a *pure function* of this struct. It must not read global state, consult
/// the filesystem, or depend on evaluation order. This is what makes decisions
/// reproducible from a session log during an audit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyRequest {
    pub action: PolicyAction,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub workspace: Workspace,
    /// The grant in force before this decision.
    pub permissions: PermissionSet,
    /// Named profile in force (`developer`, `readonly`, `ci`, ...).
    pub profile: String,
    /// True when no human can answer a prompt — headless, CI, daemon.
    ///
    /// A policy returning [`Outcome::RequireApproval`] here is really returning
    /// `Deny`; the runtime converts it so behavior in CI is predictable rather than
    /// hanging.
    pub unattended: bool,
}

/// What a policy concluded, ignoring any rewrite it also asked for.
///
/// Ordered: later variants are stricter, and [`PolicyDecision::combine`] keeps the
/// strictest.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Allow,
    /// Ask a human. Only meaningful when `!unattended`; the runtime converts it to `Deny`
    /// otherwise so CI behaves predictably instead of hanging.
    RequireApproval {
        reason: String,
        /// Rendered summary of exactly what will happen, for the approval prompt.
        preview: String,
        /// Whether "always allow this" may be offered for the rest of the session.
        allow_remember: bool,
        /// Stable key identifying *what* is being approved, so a remembered approval can
        /// match a later, differently-identified call. Keying on the call id would make
        /// "remember" useless: every subsequent call has a fresh id.
        scope_key: String,
    },
    Deny {
        /// Shown to the model as the tool result, so it can adapt rather than retry.
        reason: String,
    },
}

impl Outcome {
    /// Higher is stricter.
    #[must_use]
    pub const fn severity(&self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::RequireApproval { .. } => 1,
            Self::Deny { .. } => 2,
        }
    }
}

/// A policy's answer: an outcome, an optional narrowing rewrite, and limits.
///
/// The three are **separate fields rather than variants of one enum**, because they are
/// independent. An earlier design made `Modify` a variant ranked between `Allow` and
/// `RequireApproval`, which produced two distinct bugs: a policy that narrowed a command
/// to `--dry-run` had its narrowing silently discarded the moment any other policy asked
/// for approval, and a bare `Allow` folded over a constrained one erased the sandbox
/// requirement. Splitting them means a rewrite and a constraint survive no matter what
/// the outcome turns out to be.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub outcome: Outcome,
    /// A narrowed version of the call.
    ///
    /// A rewrite may only *reduce* what happens: add `--dry-run`, drop a flag, scope a
    /// path. Two guarantees make that enforceable rather than aspirational:
    ///
    /// 1. [`PolicyDecision::rewrite_is_wellformed`] rejects a rewrite that changes the
    ///    call's identity, and the runtime rejects the decision if it fails.
    /// 2. The runtime **re-runs the full pipeline** on the rewritten call — schema
    ///    validation and the entire policy chain — up to a small fixed depth. So even a
    ///    rewrite that widens the arguments is judged again before it runs, and cannot
    ///    launder a denied call into an allowed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<ToolCall>,
    #[serde(default)]
    pub constraints: ExecutionConstraints,
}

impl PolicyDecision {
    #[must_use]
    pub fn allow() -> Self {
        Self {
            outcome: Outcome::Allow,
            rewrite: None,
            constraints: ExecutionConstraints::default(),
        }
    }

    #[must_use]
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Deny {
                reason: reason.into(),
            },
            rewrite: None,
            constraints: ExecutionConstraints::default(),
        }
    }

    #[must_use]
    pub fn with_constraints(mut self, constraints: ExecutionConstraints) -> Self {
        self.constraints = constraints;
        self
    }

    #[must_use]
    pub fn with_rewrite(mut self, call: ToolCall) -> Self {
        self.rewrite = Some(call);
        self
    }

    /// Ordering used to combine chained policies. Higher wins.
    #[must_use]
    pub const fn severity(&self) -> u8 {
        self.outcome.severity()
    }

    /// Whether a rewrite preserves the call's identity.
    ///
    /// Identity is the id and the tool name. A "rewrite" that changes either is not a
    /// narrowing of this call, it is a different call wearing its approval.
    #[must_use]
    pub fn rewrite_is_wellformed(&self, original: &ToolCall) -> bool {
        self.rewrite
            .as_ref()
            .is_none_or(|r| r.id == original.id && r.name == original.name)
    }

    /// Combine two decisions, most restrictive wins.
    ///
    /// - **Outcome**: the stricter of the two. Ties keep `self`, so a chain folds left
    ///   deterministically regardless of how many policies agree.
    /// - **Constraints**: merged field by field, always toward the tighter value.
    /// - **Rewrite**: one survives; two *different* rewrites escalate the outcome to
    ///   `Deny`. Composing two independent narrowings is not something a runtime can do
    ///   safely by guessing, and picking one arbitrarily would silently discard the other.
    ///
    /// Interceptor results join this same fold (converted from [`RestrictiveDecision`])
    /// rather than short-circuiting it, so no single extension point decides alone.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        let constraints = self.constraints.merge(&other.constraints);

        let (rewrite, conflict) = match (self.rewrite.clone(), other.rewrite.clone()) {
            (None, None) => (None, false),
            (Some(a), None) => (Some(a), false),
            (None, Some(b)) => (Some(b), false),
            (Some(a), Some(b)) if a == b => (Some(a), false),
            (Some(_), Some(_)) => (None, true),
        };

        let outcome = if conflict {
            Outcome::Deny {
                reason: "two policies asked for different rewrites of the same call; \
                         refusing rather than guessing which narrowing to keep"
                    .to_string(),
            }
        } else if other.severity() > self.severity() {
            other.outcome
        } else {
            self.outcome
        };

        Self {
            outcome,
            rewrite,
            constraints,
        }
    }
}

/// Limits attached to an allowed action.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionConstraints {
    pub timeout_ms: Option<u64>,
    /// Name of a registered sandbox provider. `None` runs in-process, which is only
    /// acceptable for tools that touch nothing outside the workspace.
    pub sandbox: Option<String>,
    /// Cap on bytes returned to the model.
    pub max_output_bytes: Option<u64>,
    /// Grant narrowed further for this one execution.
    pub permissions: Option<PermissionSet>,
}

impl ExecutionConstraints {
    /// Merge two constraint sets toward the tighter value on every axis.
    ///
    /// `None` means "no opinion", so it never loosens a limit the other side set. This is
    /// what stops a permissive policy from erasing another policy's sandbox requirement
    /// simply by being folded in after it.
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            timeout_ms: min_opt(self.timeout_ms, other.timeout_ms),
            // Requiring a sandbox is stricter than not caring.
            sandbox: self.sandbox.clone().or_else(|| other.sandbox.clone()),
            max_output_bytes: min_opt(self.max_output_bytes, other.max_output_bytes),
            permissions: match (&self.permissions, &other.permissions) {
                (Some(a), Some(b)) => Some(a.intersect(b)),
                (Some(a), None) => Some(a.clone()),
                (None, Some(b)) => Some(b.clone()),
                (None, None) => None,
            },
        }
    }
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// A decision that can only *restrict*.
///
/// This is what an [`crate::plugin::Interceptor`] returns. It deliberately has no plain
/// "allow" variant: an interceptor that could answer "allow" would become an
/// order-dependent way to widen permission — exactly what [`PolicyDecision::combine`]
/// exists to prevent. An interceptor can add a restriction or narrow a call; it can never
/// remove a restriction another policy added.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RestrictiveDecision {
    Deny {
        reason: String,
    },
    RequireApproval {
        reason: String,
        preview: String,
        allow_remember: bool,
        /// Stable key for a remembered approval. See [`Outcome::RequireApproval`].
        scope_key: String,
    },
    /// Narrow the call. Folds in as `Allow` plus a rewrite, and the rewritten call is
    /// re-evaluated by the whole pipeline before it runs.
    Modify {
        call: ToolCall,
        reason: String,
    },
}

impl From<RestrictiveDecision> for PolicyDecision {
    fn from(value: RestrictiveDecision) -> Self {
        match value {
            RestrictiveDecision::Deny { reason } => Self::deny(reason),
            RestrictiveDecision::RequireApproval {
                reason,
                preview,
                allow_remember,
                scope_key,
            } => Self {
                outcome: Outcome::RequireApproval {
                    reason,
                    preview,
                    allow_remember,
                    scope_key,
                },
                rewrite: None,
                constraints: ExecutionConstraints::default(),
            },
            // A narrowing carries no outcome of its own: it folds in as `Allow` plus a
            // rewrite, so a `Deny` from any policy still wins and the rewrite is
            // re-evaluated before it runs.
            RestrictiveDecision::Modify { call, .. } => Self::allow().with_rewrite(call),
        }
    }
}

/// A pending human decision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub session_id: SessionId,
    pub reason: String,
    pub preview: String,
    pub allow_remember: bool,
    /// Stable identity of what is being approved. See [`Outcome::RequireApproval`].
    pub scope_key: String,
}

/// A human's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    /// Approved, and remember for the rest of the session.
    ApprovedForSession,
    Denied,
    /// Nobody answered in time. Treated as `Denied`.
    TimedOut,
}

/// Asks a human. Implemented by the TUI, by a CLI prompt, or by a webhook.
#[async_trait]
pub trait ApprovalSink: Send + Sync + fmt::Debug {
    async fn request(&self, request: ApprovalRequest) -> crate::Result<ApprovalOutcome>;
}

/// A policy.
#[async_trait]
pub trait Policy: Send + Sync + fmt::Debug {
    /// Stable name, for logs and for explaining *which* rule denied something.
    fn name(&self) -> &str;

    async fn evaluate(&self, request: &PolicyRequest) -> crate::Result<PolicyDecision>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{FsScope, Permission};
    use crate::id::ToolCallId;

    fn call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: "shell".into(),
            input: serde_json::json!({ "command": "rm -rf build" }),
        }
    }

    fn deny() -> PolicyDecision {
        PolicyDecision::deny("no")
    }

    fn approval() -> PolicyDecision {
        RestrictiveDecision::RequireApproval {
            reason: "risky".into(),
            preview: "rm -rf /".into(),
            allow_remember: false,
            scope_key: "shell:rm".into(),
        }
        .into()
    }

    #[test]
    fn most_restrictive_wins_in_both_orders() {
        assert_eq!(PolicyDecision::allow().combine(deny()).severity(), 2);
        assert_eq!(deny().combine(PolicyDecision::allow()).severity(), 2);
        assert_eq!(approval().combine(PolicyDecision::allow()).severity(), 1);
        assert_eq!(PolicyDecision::allow().combine(approval()).severity(), 1);
    }

    #[test]
    fn a_single_deny_survives_any_number_of_allows() {
        let decision = [
            PolicyDecision::allow(),
            deny(),
            PolicyDecision::allow(),
            approval(),
        ]
        .into_iter()
        .fold(PolicyDecision::allow(), PolicyDecision::combine);
        assert!(matches!(decision.outcome, Outcome::Deny { .. }));
    }

    #[test]
    fn ties_are_deterministic() {
        let a = PolicyDecision::deny("first");
        let b = PolicyDecision::deny("second");
        match a.combine(b).outcome {
            Outcome::Deny { reason } => assert_eq!(reason, "first"),
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn an_interceptor_cannot_express_a_bare_allow() {
        // Deny and RequireApproval are strictly stricter than Allow. Modify folds in as
        // Allow-plus-rewrite, which is not a widening: a Deny elsewhere still wins and
        // the rewrite is re-evaluated before it runs.
        assert_eq!(
            PolicyDecision::from(RestrictiveDecision::Deny { reason: "n".into() }).severity(),
            2
        );
        assert_eq!(approval().severity(), 1);

        let modify: PolicyDecision = RestrictiveDecision::Modify {
            call: call(),
            reason: "narrowed".into(),
        }
        .into();
        assert!(modify.rewrite.is_some());
        assert!(
            matches!(modify.combine(deny()).outcome, Outcome::Deny { .. }),
            "a narrowing interceptor must not override a denying policy"
        );
    }

    #[test]
    fn a_narrowing_survives_an_approval_requirement() {
        // The whole point of narrowing `rm -rf build` to `--dry-run` is lost if asking a
        // human for approval silently discards the rewrite and runs the original.
        let narrowed = ToolCall {
            id: call().id,
            name: "shell".into(),
            input: serde_json::json!({ "command": "rm -rf build --dry-run" }),
        };
        let modify = PolicyDecision::allow().with_rewrite(narrowed.clone());
        let combined = modify.combine(approval());
        assert_eq!(combined.severity(), 1, "approval still required");
        assert_eq!(
            combined.rewrite.as_ref().map(|c| &c.input),
            Some(&narrowed.input),
            "the narrowing must survive the fold"
        );
    }

    #[test]
    fn conflicting_rewrites_deny_rather_than_guess() {
        let original = call();
        let a = PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "command": "echo a" }),
            ..original.clone()
        });
        let b = PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "command": "echo b" }),
            ..original
        });
        let combined = a.combine(b);
        assert!(
            matches!(combined.outcome, Outcome::Deny { .. }),
            "picking one arbitrarily would silently discard the other narrowing"
        );
        assert!(combined.rewrite.is_none());
    }

    #[test]
    fn identical_rewrites_do_not_conflict() {
        let rewritten = call();
        let a = PolicyDecision::allow().with_rewrite(rewritten.clone());
        let b = PolicyDecision::allow().with_rewrite(rewritten);
        assert!(matches!(a.combine(b).outcome, Outcome::Allow));
    }

    #[test]
    fn a_rewrite_may_not_change_the_calls_identity() {
        let original = call();
        let impostor = PolicyDecision::allow().with_rewrite(ToolCall {
            id: original.id,
            name: "curl".into(), // different tool
            input: original.input.clone(),
        });
        assert!(
            !impostor.rewrite_is_wellformed(&original),
            "a different tool wearing this call's approval is not a narrowing"
        );

        let ok = PolicyDecision::allow().with_rewrite(ToolCall {
            input: serde_json::json!({ "command": "rm -rf build --dry-run" }),
            ..original.clone()
        });
        assert!(ok.rewrite_is_wellformed(&original));
        assert!(PolicyDecision::allow().rewrite_is_wellformed(&original));
    }

    #[test]
    fn constraints_are_never_loosened_by_an_unopinionated_policy() {
        // A bare `Allow` from a policy with no opinion must not erase another policy's
        // sandbox requirement.
        let strict = PolicyDecision::allow().with_constraints(ExecutionConstraints {
            timeout_ms: Some(5_000),
            sandbox: Some("docker".into()),
            max_output_bytes: Some(1024),
            permissions: Some(crate::capability::PermissionSet::new([Permission::FsRead(
                FsScope::Workspace,
            )])),
        });

        for combined in [
            strict.clone().combine(PolicyDecision::allow()),
            PolicyDecision::allow().combine(strict.clone()),
        ] {
            assert_eq!(combined.constraints.sandbox.as_deref(), Some("docker"));
            assert_eq!(combined.constraints.timeout_ms, Some(5_000));
            assert_eq!(combined.constraints.max_output_bytes, Some(1024));
            assert!(combined.constraints.permissions.is_some());
        }
    }

    #[test]
    fn constraints_merge_toward_the_tighter_value() {
        let a = PolicyDecision::allow().with_constraints(ExecutionConstraints {
            timeout_ms: Some(30_000),
            max_output_bytes: Some(1_000_000),
            ..ExecutionConstraints::default()
        });
        let b = PolicyDecision::allow().with_constraints(ExecutionConstraints {
            timeout_ms: Some(5_000),
            max_output_bytes: Some(4_096),
            ..ExecutionConstraints::default()
        });
        let merged = a.combine(b).constraints;
        assert_eq!(merged.timeout_ms, Some(5_000));
        assert_eq!(merged.max_output_bytes, Some(4_096));
    }

    #[test]
    fn merged_permissions_are_intersected() {
        let a = PolicyDecision::allow().with_constraints(ExecutionConstraints {
            permissions: Some(crate::capability::PermissionSet::new([
                Permission::FsRead(FsScope::Workspace),
                Permission::ProcessSpawn,
            ])),
            ..ExecutionConstraints::default()
        });
        let b = PolicyDecision::allow().with_constraints(ExecutionConstraints {
            permissions: Some(crate::capability::PermissionSet::new([Permission::FsRead(
                FsScope::Workspace,
            )])),
            ..ExecutionConstraints::default()
        });
        let merged = a.combine(b).constraints.permissions.unwrap();
        assert!(!merged.contains(&Permission::ProcessSpawn));
        assert!(merged.contains(&Permission::FsRead(FsScope::Workspace)));
    }
}
