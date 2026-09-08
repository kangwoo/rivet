//! Pipeline step 6: the approval decision, and the durable pair it always leaves behind.
//!
//! ```text
//! ① remembered scope_key ? ── yes ──▶ approved,  actor = None
//! ② unattended, or no sink ? ─ yes ──▶ denied,    actor = None
//! ③ otherwise ─────────────────────▶ ask the sink, actor = Some(who)
//! ```
//!
//! # Memory is looked up before the unattended conversion
//!
//! `unattended` means "nobody can answer *now*". A remembered grant means "somebody
//! already did, and said to keep it for this session". Converting first would let a flag on
//! today's run throw away a decision a person made durably, so `rivet resume --headless`
//! would silently stop honoring "allow for this session". The reverse order is defensible —
//! it is the more closed one — but it makes the acceptance line ("a remembered approval
//! survives a resume") true only of attended resumes.
//!
//! # Both events, on every path
//!
//! `approval.requested` and `approval.resolved` are written even when nothing was asked.
//! The alternative — writing them only when a human was involved — makes "the model asked
//! for something approvable and CI refused it" recoverable only by parsing the reason on
//! `tool.blocked`, and that is exactly the fact an audit is looking for. What distinguishes
//! the paths is `actor`: `None` is a rule answering, `Some` is a person.
//!
//! # The memory is a projection, not runtime state
//!
//! The set here is seeded from [`rivet_core::session::SessionState::remembered_approvals`],
//! which is itself a fold over the log. Keeping grants only in memory would make a resumed
//! session behave differently from the original and leave the grant out of the audit.

use std::collections::BTreeSet;
use std::sync::Arc;

use rivet_core::event::{Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::{RunId, ToolCallId};
use rivet_core::policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink, Outcome, PolicyDecision};
use rivet_core::session::SessionEvent;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::policy_chain;
use crate::session_log::SessionWriter;

/// What a policy asked to have approved.
#[derive(Clone, Debug)]
pub struct Ask {
    pub reason: String,
    pub preview: String,
    pub allow_remember: bool,
    pub scope_key: String,
}

impl Ask {
    /// The [`Outcome`] this ask came from, so the unattended conversion sees the same
    /// value the chain produced rather than a second encoding of it.
    #[must_use]
    fn to_outcome(&self) -> Outcome {
        Outcome::RequireApproval {
            reason: self.reason.clone(),
            preview: self.preview.clone(),
            allow_remember: self.allow_remember,
            scope_key: self.scope_key.clone(),
        }
    }

    /// The ask a `RequireApproval` outcome carries, if that is what it is.
    #[must_use]
    pub fn from_outcome(outcome: &Outcome) -> Option<Self> {
        match outcome {
            Outcome::RequireApproval {
                reason,
                preview,
                allow_remember,
                scope_key,
            } => Some(Self {
                reason: reason.clone(),
                preview: preview.clone(),
                allow_remember: *allow_remember,
                scope_key: scope_key.clone(),
            }),
            Outcome::Allow | Outcome::Deny { .. } => None,
        }
    }
}

/// Everything one run needs to resolve an approval.
///
/// Cheap to clone; every clone shares one memory of what was granted, because a grant made
/// during a turn has to apply to the next call in the same turn.
#[derive(Clone, Debug, Default)]
pub struct Approvals {
    sink: Option<Arc<dyn ApprovalSink>>,
    remembered: Arc<Mutex<BTreeSet<String>>>,
}

impl Approvals {
    /// An approver with no memory yet.
    #[must_use]
    pub fn new(sink: Option<Arc<dyn ApprovalSink>>) -> Self {
        Self {
            sink,
            remembered: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    /// Seed the memory from the session log's projection. This is all of "a remembered
    /// approval survives a resume".
    #[must_use]
    pub fn with_remembered<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.remembered = Arc::new(Mutex::new(keys.into_iter().map(Into::into).collect()));
        self
    }

    /// Whether a human could be asked at all.
    #[must_use]
    pub fn has_sink(&self) -> bool {
        self.sink.is_some()
    }

    /// Step 6, from the request event to the resolved one.
    ///
    /// # Errors
    /// Only when the session log cannot be written — the same failure that makes every
    /// other durable step fatal.
    pub async fn resolve(
        &self,
        session: &SessionWriter,
        bus: &Arc<dyn EventBus>,
        at: Where,
        ask: &Ask,
        unattended: bool,
        cancel: &CancellationToken,
    ) -> rivet_core::Result<ApprovalOutcome> {
        session
            .append(SessionEvent::ApprovalRequested {
                run_id: at.run_id,
                call_id: at.call_id,
                reason: ask.reason.clone(),
                preview: ask.preview.clone(),
                scope_key: ask.scope_key.clone(),
            })
            .await?;
        publish(
            bus,
            &at,
            ToolEvent::ApprovalRequested {
                call_id: at.call_id,
                reason: ask.reason.clone(),
            },
        );

        let (outcome, actor) = self.decide(at, ask, unattended, cancel).await;

        // Only the event that *creates* the memory says `remembered`. `SessionState::apply`
        // pushes on that flag without folding duplicates, so writing it again on every
        // later use of the same key would grow the projection once per call.
        let remembered = outcome == ApprovalOutcome::ApprovedForSession
            && self.remembered.lock().await.insert(ask.scope_key.clone());

        session
            .append(SessionEvent::ApprovalResolved {
                run_id: at.run_id,
                call_id: at.call_id,
                scope_key: ask.scope_key.clone(),
                outcome: outcome.clone(),
                remembered,
                actor,
            })
            .await?;
        publish(
            bus,
            &at,
            ToolEvent::ApprovalResolved {
                call_id: at.call_id,
                approved: matches!(
                    outcome,
                    ApprovalOutcome::Approved | ApprovalOutcome::ApprovedForSession
                ),
            },
        );
        Ok(outcome)
    }

    /// The three branches, in the order this module's documentation gives.
    async fn decide(
        &self,
        at: Where,
        ask: &Ask,
        unattended: bool,
        cancel: &CancellationToken,
    ) -> (ApprovalOutcome, Option<String>) {
        // ① A person already answered, durably.
        if self.remembered.lock().await.contains(&ask.scope_key) {
            return (ApprovalOutcome::Approved, None);
        }

        // ② Nobody can answer. `sink.is_none()` counts as unattended for an embedder that
        //    never supplied one: an approval with nowhere to go must not pass quietly.
        //    The CLI never builds that combination — `run.rs` folds the missing sink into
        //    `unattended` itself — but `rivet-runtime` is usable without it.
        let converted = policy_chain::apply_unattended(
            PolicyDecision {
                outcome: ask.to_outcome(),
                rewrite: None,
                constraints: rivet_core::policy::ExecutionConstraints::default(),
            },
            unattended || self.sink.is_none(),
        );
        let (Outcome::RequireApproval { .. }, Some(sink)) = (&converted.outcome, &self.sink) else {
            return (ApprovalOutcome::Denied, None);
        };

        // ③ Ask, racing the run's cancellation. No separate approval deadline is invented:
        //    `max_duration_ms` already reaches this token, and a timeout of our own would
        //    add "denied while the operator fetched coffee" as a new failure mode.
        let request = ApprovalRequest {
            id: rivet_core::id::ApprovalId::new(),
            session_id: at.session_id,
            reason: ask.reason.clone(),
            preview: ask.preview.clone(),
            allow_remember: ask.allow_remember,
            scope_key: ask.scope_key.clone(),
        };
        let answered = tokio::select! {
            answered = sink.request(request) => answered,
            () = cancel.cancelled() => return (ApprovalOutcome::TimedOut, None),
        };
        match answered {
            Ok(outcome) => (outcome, Some(actor_name())),
            Err(error) => {
                tracing::warn!(%error, "the approval sink failed; treating it as a refusal");
                (ApprovalOutcome::Denied, None)
            }
        }
    }
}

/// Which call, in which run, an approval belongs to.
#[derive(Clone, Copy, Debug)]
pub struct Where {
    pub session_id: rivet_core::id::SessionId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
}

fn publish(bus: &Arc<dyn EventBus>, at: &Where, event: ToolEvent) {
    bus.publish(EventEnvelope::new(Event::Tool(event)).for_run(at.session_id, at.run_id));
}

/// Who the host records as having answered.
///
/// The contract has no identity service, so this is the operator's login name — the same
/// thing a shell prompt shows. `actor` is `None` whenever a rule answered, so this string
/// only ever appears next to an answer a person actually gave.
fn actor_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_string())
}
