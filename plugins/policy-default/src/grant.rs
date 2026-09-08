//! `default.grant`: the profile's grant, enforced at call time as well as at registration.
//!
//! Phase 2 made `readonly` mean "`write_file` is never registered". That is real, and it is
//! not the whole answer: it says nothing about a *different* plugin registering a tool of
//! the same name, or a differently named write tool, and `tool-filesystem` does not read
//! `ctx.permissions()` when it runs. So a `write_file` that got registered anyway would
//! simply work. This closes that, in the one place a decision belongs:
//!
//! > A call whose tool declares `read_only == false` is refused when the grant in force
//! > holds no `FsWrite`.
//!
//! # Two ways this is wrong, in opposite directions
//!
//! **It lets a liar through.** `annotations` are self-reported, so a tool that says
//! `read_only: true` passes. `docs/security.md` §5 makes in-process plugins trusted code,
//! so what this layer stops is a *mistake*, not an attack — which is the use
//! [`rivet_core::tool::ToolAnnotations`] documents for itself.
//!
//! **It stops a tool that simply said nothing**, and this is the one that actually happens.
//! `ToolAnnotations::default()` has `read_only: false`, so a tool author who never wrote an
//! annotation is treated as mutating and does not run under `readonly`, `reviewer` or
//! `production`. Fail-closed is the right direction — but not silently, so the refusal says
//! which of the two it was, and `docs/plugin.md` tells tool authors to declare it.

use async_trait::async_trait;
use rivet_core::capability::Permission;
use rivet_core::policy::{PolicyAction, PolicyDecision, PolicyRequest};

/// Refuses a mutating call under a grant with no write permission.
#[derive(Clone, Copy, Debug)]
pub struct GrantPolicy;

#[async_trait]
impl rivet_core::policy::Policy for GrantPolicy {
    fn name(&self) -> &'static str {
        "default.grant"
    }

    async fn evaluate(&self, request: &PolicyRequest) -> rivet_core::Result<PolicyDecision> {
        let PolicyAction::ToolCall { call, annotations } = &request.action else {
            return Ok(PolicyDecision::allow());
        };
        if annotations.read_only || holds_any_write(request) {
            return Ok(PolicyDecision::allow());
        }

        // Which of the two cases this is decides what an author can do about it, so the
        // reason says which.
        let declaration = if annotations.destructive {
            format!("`{}` declares that it mutates", call.name)
        } else {
            format!(
                "`{}` does not declare `read_only`, so it is treated as mutating",
                call.name
            )
        };
        Ok(PolicyDecision::deny(format!(
            "the `{}` profile grants no `fs_write`, and {declaration}",
            request.profile
        )))
    }
}

/// Whether the grant holds write access of *any* reach.
///
/// `granted().iter().any(..)` rather than `allows(&FsWrite(Workspace))`: `allows` asks
/// whether the grant covers a specific request, so a profile holding only
/// `FsWrite(Subtree("src"))` would answer `false` and read as having no write access at
/// all. Narrow write access is still write access; *which path* is the business of
/// `default.workspace` and of `fsguard`.
fn holds_any_write(request: &PolicyRequest) -> bool {
    request
        .permissions
        .granted()
        .iter()
        .any(|held| matches!(held, Permission::FsWrite(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{annotated, request_with};
    use rivet_core::capability::{FsScope, PermissionSet};
    use rivet_core::policy::{Outcome, Policy};
    use rivet_core::tool::ToolAnnotations;

    fn readonly() -> PermissionSet {
        PermissionSet::new([Permission::FsRead(FsScope::Workspace)])
    }

    fn developer() -> PermissionSet {
        PermissionSet::new([
            Permission::FsRead(FsScope::Workspace),
            Permission::FsWrite(FsScope::Workspace),
        ])
    }

    async fn decide(permissions: PermissionSet, annotations: ToolAnnotations) -> PolicyDecision {
        GrantPolicy
            .evaluate(&request_with("write_file", permissions, annotations))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_mutating_tool_is_denied_without_fs_write() {
        // The whole table, both axes: five grants times `read_only` either way.
        let mutating = annotated(false, true);
        let reading = annotated(true, false);
        for (label, grant, write) in [
            ("developer", developer(), true),
            ("ci", developer(), true),
            ("readonly", readonly(), false),
            ("reviewer", readonly(), false),
            ("production", readonly(), false),
        ] {
            let mutating_decision = decide(grant.clone(), mutating.clone()).await;
            assert_eq!(
                matches!(mutating_decision.outcome, Outcome::Allow),
                write,
                "{label}: a mutating tool under a grant that {} write",
                if write { "has" } else { "lacks" }
            );
            assert!(
                matches!(decide(grant, reading.clone()).await.outcome, Outcome::Allow),
                "{label}: a read-only tool runs everywhere"
            );
        }
    }

    #[tokio::test]
    async fn a_read_only_tool_passes_under_every_profile() {
        // The rule is about mutation, not about being strict. A `readonly` profile that
        // refused `read_file` would be a profile nobody could use.
        for grant in [developer(), readonly(), PermissionSet::empty()] {
            assert!(
                matches!(
                    decide(grant.clone(), annotated(true, false)).await.outcome,
                    Outcome::Allow
                ),
                "a read-only tool was refused under {grant:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_refusal_names_the_profile_and_the_tool() {
        match decide(readonly(), annotated(false, true)).await.outcome {
            Outcome::Deny { reason } => {
                assert!(reason.contains("fs_write"), "{reason}");
                assert!(reason.contains("write_file"), "{reason}");
            }
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_subtree_write_grant_still_counts_as_write() {
        // `allows(&FsWrite(Workspace))` would answer `false` here and read a narrowly
        // scoped profile as having no write access at all. Which path is allowed is
        // `default.workspace`'s question, not this one's.
        let narrow = PermissionSet::new([Permission::FsWrite(FsScope::Subtree("src".into()))]);
        assert!(
            matches!(
                decide(narrow, annotated(false, true)).await.outcome,
                Outcome::Allow
            ),
            "narrow write access is still write access"
        );
    }

    #[tokio::test]
    async fn a_tool_that_lies_about_being_read_only_passes_this_layer() {
        // Asserted, not lamented. `annotations` are self-reported and `security.md` §5
        // makes in-process plugins trusted code: this layer catches an author's mistake,
        // and the honest place to say so is a test that pins the limit.
        let liar = ToolAnnotations {
            read_only: true,
            destructive: true,
            ..ToolAnnotations::default()
        };
        assert!(matches!(
            decide(readonly(), liar).await.outcome,
            Outcome::Allow
        ));
    }

    #[tokio::test]
    async fn a_tool_that_declares_no_annotations_is_treated_as_mutating() {
        // `ToolAnnotations::default()` is `read_only: false`, so an author who wrote
        // nothing is refused under a read-only grant. Fail-closed, and the reason has to
        // say which of the two cases it is or the author cannot act on it.
        match decide(readonly(), ToolAnnotations::default()).await.outcome {
            Outcome::Deny { reason } => assert!(
                reason.contains("does not declare `read_only`"),
                "the author has to be able to tell this from a real refusal: {reason}"
            ),
            other => panic!("expected a deny, got {other:?}"),
        }
    }
}
