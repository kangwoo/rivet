//! The job contract: durable intent, separate from agent execution.
//!
//! A job is *what we are trying to achieve*. A run is *one attempt*. Keeping them apart
//! is what lets a job survive a crashed run, a failed review, or a model swap.
//!
//! The state machine here is total and validated: [`JobState::can_transition_to`] is the
//! only place transitions are defined, so no scheduler or workflow plugin can invent one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::id::{JobId, RunId};
use crate::time::Timestamp;

/// Lifecycle of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    /// Created; dependencies not yet satisfied.
    Pending,
    /// Dependencies satisfied; waiting for a scheduler slot.
    Ready,
    /// An agent run is in flight.
    Running,
    /// Blocked on something external: a human, another system, a timer.
    Waiting,
    /// Work is done; awaiting a verdict.
    Review,
    /// Terminal, successful.
    Completed,
    /// Terminal, unsuccessful. Retries are exhausted or the failure is not retryable.
    Failed,
    /// Terminal, abandoned.
    Cancelled,
}

impl JobState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// The single source of truth for legal transitions.
    ///
    /// Notable rules:
    /// - Terminal states are absorbing. Nothing leaves `Completed`; re-opening means a
    ///   new job, so history stays honest.
    /// - `Review -> Ready` is how "request changes" works, and it is the loop that makes
    ///   the review gate useful rather than a rubber stamp.
    /// - `Failed` is reachable from `Review` (reject), from `Running` (error), and from
    ///   `Ready` (retry budget exhausted). Without that last one a job whose attempts ran
    ///   out would sit in `Ready` forever: it cannot enter `Running`, and every other exit
    ///   was closed — the graph would never settle.
    #[must_use]
    // The arms are written out one state at a time on purpose: this table is the
    // security-relevant part of the job runtime and reads better than a merged pattern.
    #[allow(clippy::match_same_arms)]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use JobState::{Cancelled, Completed, Failed, Pending, Ready, Review, Running, Waiting};
        match (self, next) {
            // Cancellation is always available from any non-terminal state.
            (s, Cancelled) if !s.is_terminal() => true,
            (Pending, Ready | Failed) => true,
            (Ready, Running | Pending | Failed) => true,
            (Running, Review | Failed | Waiting | Ready) => true,
            (Waiting, Ready | Failed) => true,
            (Review, Completed | Ready | Failed) => true,
            _ => false,
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "PENDING",
            Self::Ready => "READY",
            Self::Running => "RUNNING",
            Self::Waiting => "WAITING",
            Self::Review => "REVIEW",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        };
        f.write_str(s)
    }
}

/// A unit of durable intent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    /// What "done" means, in prose. Goes into the agent's context.
    pub goal: String,
    /// Checkable conditions. A review agent evaluates against these rather than vibes.
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Private: the only legal way to change it is [`Job::transition_to`]. A public
    /// field would make [`JobState::can_transition_to`] advisory, and the review gate
    /// bypassable with a single assignment.
    state: JobState,
    /// Private for the same reason: mutating dependencies in place would sidestep the
    /// cycle check that [`JobGraph::insert`] performs.
    #[serde(default)]
    depends_on: Vec<JobId>,
    /// Runs attached so far, newest last.
    #[serde(default)]
    pub runs: Vec<RunId>,
    pub attempts: u32,
    /// Cap on attempts before `Failed`. Prevents a job from burning tokens forever.
    pub max_attempts: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Free-form, for workflow plugins.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl Job {
    pub fn new(goal: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: JobId::new(),
            goal: goal.into(),
            acceptance: Vec::new(),
            state: JobState::Pending,
            depends_on: Vec::new(),
            runs: Vec::new(),
            attempts: 0,
            max_attempts: 3,
            created_at: now,
            updated_at: now,
            labels: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_dependencies(mut self, deps: impl IntoIterator<Item = JobId>) -> Self {
        self.depends_on = deps.into_iter().collect();
        self
    }

    #[must_use]
    pub const fn state(&self) -> JobState {
        self.state
    }

    #[must_use]
    pub fn depends_on(&self) -> &[JobId] {
        &self.depends_on
    }

    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.attempts >= self.max_attempts
    }

    /// Move to `next`, validating the transition.
    ///
    /// # Errors
    /// - The transition is not in [`JobState::can_transition_to`].
    /// - Entering `Running` would exceed `max_attempts`. Enforcing the budget here rather
    ///   than in the scheduler means no workflow plugin can spend it twice.
    pub fn transition_to(&mut self, next: JobState) -> crate::Result<()> {
        if !self.state.can_transition_to(next) {
            return Err(crate::Error::invalid_argument(format!(
                "illegal job transition {} -> {next}",
                self.state
            )));
        }
        if next == JobState::Running && self.exhausted() {
            return Err(crate::Error::invalid_argument(format!(
                "job {} has used all {} attempts",
                self.id, self.max_attempts
            )));
        }
        if next == JobState::Running {
            self.attempts += 1;
        }
        self.state = next;
        self.updated_at = Timestamp::now();
        Ok(())
    }

    /// Finish a run that needs no review.
    ///
    /// The state machine forbids `Running -> Completed` so the review gate cannot be
    /// skipped by accident. A workflow whose [`Workflow::requires_review`] is `false` uses
    /// this instead: the job still passes *through* `Review`, so the log shows a gate
    /// that was opened rather than one that was never there.
    pub fn complete_without_review(&mut self) -> crate::Result<()> {
        self.transition_to(JobState::Review)?;
        self.transition_to(JobState::Completed)
    }

    /// Apply a review verdict.
    pub fn apply_verdict(&mut self, verdict: ReviewVerdict) -> crate::Result<()> {
        self.transition_to(verdict.resulting_state())
    }
}

/// A verdict on a job in `Review`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// -> `Completed`.
    Approve,
    /// -> `Ready` for another attempt.
    RequestChanges,
    /// -> `Failed`. Not worth another attempt.
    Reject,
}

impl ReviewVerdict {
    #[must_use]
    pub const fn resulting_state(self) -> JobState {
        match self {
            Self::Approve => JobState::Completed,
            Self::RequestChanges => JobState::Ready,
            Self::Reject => JobState::Failed,
        }
    }
}

/// A DAG of jobs.
///
/// Deserialization goes through [`JobGraph::from_jobs`], so a graph loaded from disk is
/// validated exactly like one built in memory. Without that, making `Job::state` private
/// would buy nothing: a hand-edited or corrupted state file could reintroduce a cycle, or
/// a job sitting in `COMPLETED` that never passed review.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct JobGraph {
    jobs: BTreeMap<JobId, Job>,
}

impl<'de> Deserialize<'de> for JobGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let jobs = BTreeMap::<JobId, Job>::deserialize(deserializer)?;
        Self::from_jobs(jobs.into_values()).map_err(serde::de::Error::custom)
    }
}

impl JobGraph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a graph from jobs, validating dependencies and acyclicity.
    ///
    /// # Errors
    /// A dependency that names no known job, a self-dependency, or a cycle.
    pub fn from_jobs(jobs: impl IntoIterator<Item = Job>) -> crate::Result<Self> {
        let mut graph = Self::new();
        // Insert first so forward references between jobs resolve, then validate once.
        for job in jobs {
            if job.depends_on.contains(&job.id) {
                return Err(crate::Error::invalid_argument(format!(
                    "job {} depends on itself",
                    job.id
                )));
            }
            graph.jobs.insert(job.id, job);
        }
        for job in graph.jobs.values() {
            for dep in &job.depends_on {
                if !graph.jobs.contains_key(dep) {
                    return Err(crate::Error::not_found(format!(
                        "job {} depends on unknown job {dep}",
                        job.id
                    )));
                }
            }
        }
        graph.assert_acyclic()?;
        Ok(graph)
    }

    /// Insert a job, rejecting anything that would create a cycle or dangle.
    ///
    /// Validating on insert rather than on schedule means a malformed graph is a config
    /// error at startup, not a deadlock at 3am.
    pub fn insert(&mut self, job: Job) -> crate::Result<()> {
        for dep in &job.depends_on {
            if !self.jobs.contains_key(dep) && *dep != job.id {
                return Err(crate::Error::not_found(format!(
                    "job {} depends on unknown job {dep}",
                    job.id
                )));
            }
        }
        if job.depends_on.contains(&job.id) {
            return Err(crate::Error::invalid_argument(format!(
                "job {} depends on itself",
                job.id
            )));
        }
        let id = job.id;
        let previous = self.jobs.insert(id, job);
        if let Err(e) = self.assert_acyclic() {
            // Restore the prior state: a rejected insert must not leave a cyclic graph
            // behind for the next caller to trip over.
            match previous {
                Some(old) => self.jobs.insert(id, old),
                None => self.jobs.remove(&id),
            };
            return Err(e);
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.jobs.get(&id)
    }

    /// Transition a job, validating the move.
    ///
    /// This replaces a `get_mut`-style accessor on purpose. Handing out `&mut Job` would
    /// let a caller assign `state` or `depends_on` directly, which is exactly how the
    /// review gate and the cycle check get bypassed.
    pub fn apply(&mut self, id: JobId, next: JobState) -> crate::Result<JobState> {
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or_else(|| crate::Error::not_found(format!("unknown job {id}")))?;
        let from = job.state;
        job.transition_to(next)?;
        let _ = from;
        Ok(next)
    }

    /// Attach a run to a job. The only other in-place mutation the graph permits.
    pub fn attach_run(&mut self, id: JobId, run: crate::id::RunId) -> crate::Result<()> {
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or_else(|| crate::Error::not_found(format!("unknown job {id}")))?;
        job.runs.push(run);
        Ok(())
    }

    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Jobs whose dependencies are all `Completed` and that are still `Pending`.
    ///
    /// A dependency in a *failed* terminal state never satisfies: the dependent stays
    /// `Pending` forever rather than running against a broken precondition. Detecting
    /// that situation is [`JobGraph::blocked`]'s job.
    #[must_use]
    pub fn newly_ready(&self) -> Vec<JobId> {
        self.jobs
            .values()
            .filter(|t| t.state == JobState::Pending)
            .filter(|t| {
                t.depends_on.iter().all(|d| {
                    self.jobs
                        .get(d)
                        .is_some_and(|dep| dep.state == JobState::Completed)
                })
            })
            .map(|t| t.id)
            .collect()
    }

    /// Jobs that can never become ready because a dependency failed or was cancelled.
    ///
    /// Without this, a graph with one failed leaf looks "still working" forever.
    #[must_use]
    pub fn blocked(&self) -> Vec<JobId> {
        self.jobs
            .values()
            .filter(|t| !t.state.is_terminal())
            .filter(|t| {
                t.depends_on.iter().any(|d| {
                    self.jobs.get(d).is_some_and(|dep| {
                        matches!(dep.state, JobState::Failed | JobState::Cancelled)
                    })
                })
            })
            .map(|t| t.id)
            .collect()
    }

    /// True when no job can make further progress and none is waiting on anything.
    ///
    /// `Waiting` counts as live. A job parked on a human approval or an external system
    /// has not finished, and a runtime that treats it as settled shuts down mid-workflow —
    /// which is exactly what an `approval-gate` workflow does on every run.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        if self.jobs.values().all(|t| t.state.is_terminal()) {
            return true;
        }
        let live = self.jobs.values().any(|t| {
            matches!(
                t.state,
                JobState::Ready | JobState::Running | JobState::Review | JobState::Waiting
            )
        });
        !live && self.newly_ready().is_empty()
    }

    /// Kahn's algorithm, used purely as a cycle check.
    fn assert_acyclic(&self) -> crate::Result<()> {
        let mut indegree: BTreeMap<JobId, usize> =
            self.jobs.keys().map(|id| (*id, 0usize)).collect();
        for job in self.jobs.values() {
            for dep in &job.depends_on {
                if self.jobs.contains_key(dep) {
                    *indegree.entry(job.id).or_default() += 1;
                }
                let _ = dep;
            }
        }

        let mut queue: VecDeque<JobId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = BTreeSet::new();

        while let Some(id) = queue.pop_front() {
            visited.insert(id);
            for job in self.jobs.values() {
                if job.depends_on.contains(&id) {
                    let entry = indegree.entry(job.id).or_default();
                    *entry = entry.saturating_sub(1);
                    if *entry == 0 && !visited.contains(&job.id) {
                        queue.push_back(job.id);
                    }
                }
            }
        }

        if visited.len() == self.jobs.len() {
            Ok(())
        } else {
            Err(crate::Error::invalid_argument(
                "job graph contains a dependency cycle",
            ))
        }
    }
}

/// Decides which ready jobs run next.
///
/// A workflow is a *pure policy over graph shape*. It never mutates the graph — the job
/// runtime applies transitions — so a buggy workflow plugin can stall progress but cannot
/// corrupt state.
#[async_trait]
pub trait Workflow: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    /// Given the current graph, which jobs should be dispatched now.
    ///
    /// Returning an empty vec means "nothing right now", not "done" — the job runtime
    /// decides completion via [`JobGraph::is_settled`].
    async fn next(&self, graph: &JobGraph) -> crate::Result<Vec<JobId>>;

    /// Whether a job leaving `Running` should go to `Review` or straight to `Completed`.
    async fn requires_review(&self, _graph: &JobGraph, _job: &Job) -> bool {
        false
    }
}

/// Turns dispatch decisions into agent runs.
#[async_trait]
pub trait Scheduler: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    /// How many runs may be in flight at once.
    fn max_concurrency(&self) -> usize {
        1
    }

    /// Claim a job for execution. Returns `false` if the slot was taken — this is the
    /// primitive a distributed scheduler needs to avoid two workers on one job.
    async fn claim(&self, job_id: JobId) -> crate::Result<bool>;

    async fn release(&self, job_id: JobId) -> crate::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_absorbing() {
        for terminal in [JobState::Completed, JobState::Failed, JobState::Cancelled] {
            for next in [JobState::Ready, JobState::Running, JobState::Review] {
                assert!(
                    !terminal.can_transition_to(next),
                    "{terminal} must not reach {next}"
                );
            }
        }
    }

    #[test]
    fn review_can_send_work_back() {
        assert!(JobState::Review.can_transition_to(JobState::Ready));
        assert!(JobState::Review.can_transition_to(JobState::Completed));
        assert!(JobState::Review.can_transition_to(JobState::Failed));
    }

    #[test]
    fn running_never_jumps_straight_to_completed() {
        assert!(
            !JobState::Running.can_transition_to(JobState::Completed),
            "completion must go through Review so the gate cannot be skipped"
        );
    }

    #[test]
    fn cancellation_is_available_from_any_live_state() {
        for live in [
            JobState::Pending,
            JobState::Ready,
            JobState::Running,
            JobState::Waiting,
            JobState::Review,
        ] {
            assert!(live.can_transition_to(JobState::Cancelled), "{live}");
        }
    }

    #[test]
    fn illegal_transitions_are_rejected_with_a_useful_message() {
        let mut job = Job::new("do a thing");
        let err = job.transition_to(JobState::Completed).unwrap_err();
        assert!(err.message().contains("PENDING -> COMPLETED"), "{err}");
        assert_eq!(
            job.state,
            JobState::Pending,
            "state must not change on error"
        );
    }

    #[test]
    fn entering_running_counts_an_attempt() {
        let mut job = Job::new("g");
        job.max_attempts = 2;
        job.transition_to(JobState::Ready).unwrap();
        job.transition_to(JobState::Running).unwrap();
        assert_eq!(job.attempts, 1);
        assert!(!job.exhausted());
        job.transition_to(JobState::Review).unwrap();
        job.transition_to(JobState::Ready).unwrap();
        job.transition_to(JobState::Running).unwrap();
        assert_eq!(job.attempts, 2);
        assert!(job.exhausted(), "retry budget must be enforceable");
    }

    #[test]
    fn dependencies_gate_readiness() {
        let mut graph = JobGraph::new();
        let a = Job::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Job::new("test").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        assert_eq!(graph.newly_ready(), vec![a_id], "b is blocked on a");

        for state in [
            JobState::Ready,
            JobState::Running,
            JobState::Review,
            JobState::Completed,
        ] {
            graph.apply(a_id, state).unwrap();
        }

        assert_eq!(
            graph.newly_ready(),
            vec![b_id],
            "b unblocks once a completes"
        );
    }

    #[test]
    fn a_failed_dependency_surfaces_as_blocked_not_ready() {
        let mut graph = JobGraph::new();
        let a = Job::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Job::new("deploy").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        graph.apply(a_id, JobState::Ready).unwrap();
        graph.apply(a_id, JobState::Running).unwrap();
        graph.apply(a_id, JobState::Failed).unwrap();

        assert!(!graph.newly_ready().contains(&b_id));
        assert_eq!(graph.blocked(), vec![b_id]);
        assert!(
            graph.is_settled(),
            "a graph that cannot progress must read as settled"
        );
    }

    #[test]
    fn a_waiting_job_is_not_settled() {
        // An approval-gate workflow parks a job in WAITING. If that reads as settled,
        // the job runtime shuts down while a human is still deciding.
        let mut graph = JobGraph::new();
        let a = Job::new("deploy");
        let a_id = a.id;
        graph.insert(a).unwrap();

        graph.apply(a_id, JobState::Ready).unwrap();
        graph.apply(a_id, JobState::Running).unwrap();
        graph.apply(a_id, JobState::Waiting).unwrap();

        assert!(
            !graph.is_settled(),
            "a job waiting on a human has not finished"
        );
    }

    #[test]
    fn the_state_machine_cannot_be_bypassed_by_assignment() {
        // There is deliberately no way to write `job.state = Completed`. The only entry
        // point is `apply`, which validates.
        let mut graph = JobGraph::new();
        let t = Job::new("deploy to production");
        let id = t.id;
        graph.insert(t).unwrap();
        graph.apply(id, JobState::Ready).unwrap();
        graph.apply(id, JobState::Running).unwrap();

        let err = graph.apply(id, JobState::Completed).unwrap_err();
        assert!(err.message().contains("RUNNING -> COMPLETED"), "{err}");
        assert_eq!(graph.get(id).unwrap().state(), JobState::Running);
    }

    #[test]
    fn a_review_free_workflow_still_passes_through_the_gate() {
        let mut job = Job::new("format the code");
        job.transition_to(JobState::Ready).unwrap();
        job.transition_to(JobState::Running).unwrap();
        job.complete_without_review().unwrap();
        assert_eq!(job.state(), JobState::Completed);
    }

    #[test]
    fn the_attempt_budget_is_enforced_by_the_transition() {
        let mut job = Job::new("flaky");
        job.max_attempts = 1;
        job.transition_to(JobState::Ready).unwrap();
        job.transition_to(JobState::Running).unwrap();
        job.transition_to(JobState::Ready).unwrap();

        let err = job.transition_to(JobState::Running).unwrap_err();
        assert!(err.message().contains("all 1 attempts"), "{err}");
        assert_eq!(
            job.state(),
            JobState::Ready,
            "a refused attempt must not advance the job"
        );
    }

    #[test]
    fn unknown_dependencies_are_rejected_at_insert() {
        let mut graph = JobGraph::new();
        let orphan = Job::new("b").with_dependencies([JobId::new()]);
        assert!(graph.insert(orphan).is_err());
    }

    #[test]
    fn self_dependency_is_rejected() {
        let mut graph = JobGraph::new();
        let mut t = Job::new("loop");
        t.depends_on = vec![t.id];
        assert!(graph.insert(t).is_err());
    }

    #[test]
    fn cycles_are_rejected() {
        let mut graph = JobGraph::new();
        let a = Job::new("a");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Job::new("b").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        // Close the loop by replacing `a` with a version that depends on `b`.
        let mut cyclic = Job::new("a");
        cyclic.id = a_id;
        let cyclic = cyclic.with_dependencies([b_id]);

        assert!(
            graph.insert(cyclic).is_err(),
            "a -> b -> a must be detected"
        );
    }

    #[test]
    fn a_persisted_cycle_is_rejected_on_load() {
        // Making the fields private is worthless if a state file can reintroduce a cycle.
        let a = JobId::new();
        let b = JobId::new();
        let json = serde_json::json!({
            a.as_uuid().to_string(): {
                "id": a, "goal": "a", "state": "PENDING", "depends_on": [b],
                "runs": [], "attempts": 0, "max_attempts": 3,
                "created_at": "1970-01-01T00:00:00Z", "updated_at": "1970-01-01T00:00:00Z"
            },
            b.as_uuid().to_string(): {
                "id": b, "goal": "b", "state": "PENDING", "depends_on": [a],
                "runs": [], "attempts": 0, "max_attempts": 3,
                "created_at": "1970-01-01T00:00:00Z", "updated_at": "1970-01-01T00:00:00Z"
            }
        });
        let err = serde_json::from_value::<JobGraph>(json).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn a_persisted_graph_round_trips() {
        let mut graph = JobGraph::new();
        let a = Job::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        graph
            .insert(Job::new("test").with_dependencies([a_id]))
            .unwrap();

        let json = serde_json::to_string(&graph).unwrap();
        let back: JobGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.newly_ready(), vec![a_id]);
    }

    #[test]
    fn a_persisted_unknown_dependency_is_rejected() {
        let a = JobId::new();
        let json = serde_json::json!({
            a.as_uuid().to_string(): {
                "id": a, "goal": "a", "state": "PENDING", "depends_on": [JobId::new()],
                "runs": [], "attempts": 0, "max_attempts": 3,
                "created_at": "1970-01-01T00:00:00Z", "updated_at": "1970-01-01T00:00:00Z"
            }
        });
        assert!(serde_json::from_value::<JobGraph>(json).is_err());
    }

    #[test]
    fn a_rejected_insert_leaves_the_graph_usable() {
        let mut graph = JobGraph::new();
        let a = Job::new("a");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Job::new("b").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        let mut cyclic = Job::new("a");
        cyclic.id = a_id;
        let _ = graph.insert(cyclic.with_dependencies([b_id]));

        // The rejected insert must not have left the cycle in place.
        assert_eq!(graph.newly_ready(), vec![a_id]);
        assert_eq!(graph.len(), 2);
    }

    #[test]
    fn verdicts_map_to_states() {
        assert_eq!(
            ReviewVerdict::Approve.resulting_state(),
            JobState::Completed
        );
        assert_eq!(
            ReviewVerdict::RequestChanges.resulting_state(),
            JobState::Ready
        );
        assert_eq!(ReviewVerdict::Reject.resulting_state(), JobState::Failed);
    }
}
