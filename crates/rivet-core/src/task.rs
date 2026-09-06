//! The task contract: durable intent, separate from agent execution.
//!
//! A task is *what we are trying to achieve*. A run is *one attempt*. Keeping them apart
//! is what lets a task survive a crashed run, a failed review, or a model swap.
//!
//! The state machine here is total and validated: [`TaskState::can_transition_to`] is the
//! only place transitions are defined, so no scheduler or workflow plugin can invent one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::id::{RunId, TaskId};
use crate::time::Timestamp;

/// Lifecycle of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
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

impl TaskState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// The single source of truth for legal transitions.
    ///
    /// Notable rules:
    /// - Terminal states are absorbing. Nothing leaves `Completed`; re-opening means a
    ///   new task, so history stays honest.
    /// - `Review -> Ready` is how "request changes" works, and it is the loop that makes
    ///   the review gate useful rather than a rubber stamp.
    /// - `Failed` is reachable from `Review` (reject), from `Running` (error), and from
    ///   `Ready` (retry budget exhausted). Without that last one a task whose attempts ran
    ///   out would sit in `Ready` forever: it cannot enter `Running`, and every other exit
    ///   was closed — the graph would never settle.
    #[must_use]
    // The arms are written out one state at a time on purpose: this table is the
    // security-relevant part of the task runtime and reads better than a merged pattern.
    #[allow(clippy::match_same_arms)]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use TaskState::{Cancelled, Completed, Failed, Pending, Ready, Review, Running, Waiting};
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

impl fmt::Display for TaskState {
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
pub struct Task {
    pub id: TaskId,
    /// What "done" means, in prose. Goes into the agent's context.
    pub goal: String,
    /// Checkable conditions. A review agent evaluates against these rather than vibes.
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Private: the only legal way to change it is [`Task::transition_to`]. A public
    /// field would make [`TaskState::can_transition_to`] advisory, and the review gate
    /// bypassable with a single assignment.
    state: TaskState,
    /// Private for the same reason: mutating dependencies in place would sidestep the
    /// cycle check that [`TaskGraph::insert`] performs.
    #[serde(default)]
    depends_on: Vec<TaskId>,
    /// Runs attached so far, newest last.
    #[serde(default)]
    pub runs: Vec<RunId>,
    pub attempts: u32,
    /// Cap on attempts before `Failed`. Prevents a task from burning tokens forever.
    pub max_attempts: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Free-form, for workflow plugins.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl Task {
    pub fn new(goal: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: TaskId::new(),
            goal: goal.into(),
            acceptance: Vec::new(),
            state: TaskState::Pending,
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
    pub fn with_dependencies(mut self, deps: impl IntoIterator<Item = TaskId>) -> Self {
        self.depends_on = deps.into_iter().collect();
        self
    }

    #[must_use]
    pub const fn state(&self) -> TaskState {
        self.state
    }

    #[must_use]
    pub fn depends_on(&self) -> &[TaskId] {
        &self.depends_on
    }

    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.attempts >= self.max_attempts
    }

    /// Move to `next`, validating the transition.
    ///
    /// # Errors
    /// - The transition is not in [`TaskState::can_transition_to`].
    /// - Entering `Running` would exceed `max_attempts`. Enforcing the budget here rather
    ///   than in the scheduler means no workflow plugin can spend it twice.
    pub fn transition_to(&mut self, next: TaskState) -> crate::Result<()> {
        if !self.state.can_transition_to(next) {
            return Err(crate::Error::invalid_argument(format!(
                "illegal task transition {} -> {next}",
                self.state
            )));
        }
        if next == TaskState::Running && self.exhausted() {
            return Err(crate::Error::invalid_argument(format!(
                "task {} has used all {} attempts",
                self.id, self.max_attempts
            )));
        }
        if next == TaskState::Running {
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
    /// this instead: the task still passes *through* `Review`, so the log shows a gate
    /// that was opened rather than one that was never there.
    pub fn complete_without_review(&mut self) -> crate::Result<()> {
        self.transition_to(TaskState::Review)?;
        self.transition_to(TaskState::Completed)
    }

    /// Apply a review verdict.
    pub fn apply_verdict(&mut self, verdict: ReviewVerdict) -> crate::Result<()> {
        self.transition_to(verdict.resulting_state())
    }
}

/// A verdict on a task in `Review`.
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
    pub const fn resulting_state(self) -> TaskState {
        match self {
            Self::Approve => TaskState::Completed,
            Self::RequestChanges => TaskState::Ready,
            Self::Reject => TaskState::Failed,
        }
    }
}

/// A DAG of tasks.
///
/// Deserialization goes through [`TaskGraph::from_tasks`], so a graph loaded from disk is
/// validated exactly like one built in memory. Without that, making `Task::state` private
/// would buy nothing: a hand-edited or corrupted state file could reintroduce a cycle, or
/// a task sitting in `COMPLETED` that never passed review.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct TaskGraph {
    tasks: BTreeMap<TaskId, Task>,
}

impl<'de> Deserialize<'de> for TaskGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tasks = BTreeMap::<TaskId, Task>::deserialize(deserializer)?;
        Self::from_tasks(tasks.into_values()).map_err(serde::de::Error::custom)
    }
}

impl TaskGraph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a graph from tasks, validating dependencies and acyclicity.
    ///
    /// # Errors
    /// A dependency that names no known task, a self-dependency, or a cycle.
    pub fn from_tasks(tasks: impl IntoIterator<Item = Task>) -> crate::Result<Self> {
        let mut graph = Self::new();
        // Insert first so forward references between tasks resolve, then validate once.
        for task in tasks {
            if task.depends_on.contains(&task.id) {
                return Err(crate::Error::invalid_argument(format!(
                    "task {} depends on itself",
                    task.id
                )));
            }
            graph.tasks.insert(task.id, task);
        }
        for task in graph.tasks.values() {
            for dep in &task.depends_on {
                if !graph.tasks.contains_key(dep) {
                    return Err(crate::Error::not_found(format!(
                        "task {} depends on unknown task {dep}",
                        task.id
                    )));
                }
            }
        }
        graph.assert_acyclic()?;
        Ok(graph)
    }

    /// Insert a task, rejecting anything that would create a cycle or dangle.
    ///
    /// Validating on insert rather than on schedule means a malformed graph is a config
    /// error at startup, not a deadlock at 3am.
    pub fn insert(&mut self, task: Task) -> crate::Result<()> {
        for dep in &task.depends_on {
            if !self.tasks.contains_key(dep) && *dep != task.id {
                return Err(crate::Error::not_found(format!(
                    "task {} depends on unknown task {dep}",
                    task.id
                )));
            }
        }
        if task.depends_on.contains(&task.id) {
            return Err(crate::Error::invalid_argument(format!(
                "task {} depends on itself",
                task.id
            )));
        }
        let id = task.id;
        let previous = self.tasks.insert(id, task);
        if let Err(e) = self.assert_acyclic() {
            // Restore the prior state: a rejected insert must not leave a cyclic graph
            // behind for the next caller to trip over.
            match previous {
                Some(old) => self.tasks.insert(id, old),
                None => self.tasks.remove(&id),
            };
            return Err(e);
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// Transition a task, validating the move.
    ///
    /// This replaces a `get_mut`-style accessor on purpose. Handing out `&mut Task` would
    /// let a caller assign `state` or `depends_on` directly, which is exactly how the
    /// review gate and the cycle check get bypassed.
    pub fn apply(&mut self, id: TaskId, next: TaskState) -> crate::Result<TaskState> {
        let task = self
            .tasks
            .get_mut(&id)
            .ok_or_else(|| crate::Error::not_found(format!("unknown task {id}")))?;
        let from = task.state;
        task.transition_to(next)?;
        let _ = from;
        Ok(next)
    }

    /// Attach a run to a task. The only other in-place mutation the graph permits.
    pub fn attach_run(&mut self, id: TaskId, run: crate::id::RunId) -> crate::Result<()> {
        let task = self
            .tasks
            .get_mut(&id)
            .ok_or_else(|| crate::Error::not_found(format!("unknown task {id}")))?;
        task.runs.push(run);
        Ok(())
    }

    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Tasks whose dependencies are all `Completed` and that are still `Pending`.
    ///
    /// A dependency in a *failed* terminal state never satisfies: the dependent stays
    /// `Pending` forever rather than running against a broken precondition. Detecting
    /// that situation is [`TaskGraph::blocked`]'s job.
    #[must_use]
    pub fn newly_ready(&self) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|t| t.state == TaskState::Pending)
            .filter(|t| {
                t.depends_on.iter().all(|d| {
                    self.tasks
                        .get(d)
                        .is_some_and(|dep| dep.state == TaskState::Completed)
                })
            })
            .map(|t| t.id)
            .collect()
    }

    /// Tasks that can never become ready because a dependency failed or was cancelled.
    ///
    /// Without this, a graph with one failed leaf looks "still working" forever.
    #[must_use]
    pub fn blocked(&self) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|t| !t.state.is_terminal())
            .filter(|t| {
                t.depends_on.iter().any(|d| {
                    self.tasks.get(d).is_some_and(|dep| {
                        matches!(dep.state, TaskState::Failed | TaskState::Cancelled)
                    })
                })
            })
            .map(|t| t.id)
            .collect()
    }

    /// True when no task can make further progress and none is waiting on anything.
    ///
    /// `Waiting` counts as live. A task parked on a human approval or an external system
    /// has not finished, and a runtime that treats it as settled shuts down mid-workflow —
    /// which is exactly what an `approval-gate` workflow does on every run.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        if self.tasks.values().all(|t| t.state.is_terminal()) {
            return true;
        }
        let live = self.tasks.values().any(|t| {
            matches!(
                t.state,
                TaskState::Ready | TaskState::Running | TaskState::Review | TaskState::Waiting
            )
        });
        !live && self.newly_ready().is_empty()
    }

    /// Kahn's algorithm, used purely as a cycle check.
    fn assert_acyclic(&self) -> crate::Result<()> {
        let mut indegree: BTreeMap<TaskId, usize> =
            self.tasks.keys().map(|id| (*id, 0usize)).collect();
        for task in self.tasks.values() {
            for dep in &task.depends_on {
                if self.tasks.contains_key(dep) {
                    *indegree.entry(task.id).or_default() += 1;
                }
                let _ = dep;
            }
        }

        let mut queue: VecDeque<TaskId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = BTreeSet::new();

        while let Some(id) = queue.pop_front() {
            visited.insert(id);
            for task in self.tasks.values() {
                if task.depends_on.contains(&id) {
                    let entry = indegree.entry(task.id).or_default();
                    *entry = entry.saturating_sub(1);
                    if *entry == 0 && !visited.contains(&task.id) {
                        queue.push_back(task.id);
                    }
                }
            }
        }

        if visited.len() == self.tasks.len() {
            Ok(())
        } else {
            Err(crate::Error::invalid_argument(
                "task graph contains a dependency cycle",
            ))
        }
    }
}

/// Decides which ready tasks run next.
///
/// A workflow is a *pure policy over graph shape*. It never mutates the graph — the task
/// runtime applies transitions — so a buggy workflow plugin can stall progress but cannot
/// corrupt state.
#[async_trait]
pub trait Workflow: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    /// Given the current graph, which tasks should be dispatched now.
    ///
    /// Returning an empty vec means "nothing right now", not "done" — the task runtime
    /// decides completion via [`TaskGraph::is_settled`].
    async fn next(&self, graph: &TaskGraph) -> crate::Result<Vec<TaskId>>;

    /// Whether a task leaving `Running` should go to `Review` or straight to `Completed`.
    async fn requires_review(&self, _graph: &TaskGraph, _task: &Task) -> bool {
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

    /// Claim a task for execution. Returns `false` if the slot was taken — this is the
    /// primitive a distributed scheduler needs to avoid two workers on one task.
    async fn claim(&self, task_id: TaskId) -> crate::Result<bool>;

    async fn release(&self, task_id: TaskId) -> crate::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_absorbing() {
        for terminal in [
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            for next in [TaskState::Ready, TaskState::Running, TaskState::Review] {
                assert!(
                    !terminal.can_transition_to(next),
                    "{terminal} must not reach {next}"
                );
            }
        }
    }

    #[test]
    fn review_can_send_work_back() {
        assert!(TaskState::Review.can_transition_to(TaskState::Ready));
        assert!(TaskState::Review.can_transition_to(TaskState::Completed));
        assert!(TaskState::Review.can_transition_to(TaskState::Failed));
    }

    #[test]
    fn running_never_jumps_straight_to_completed() {
        assert!(
            !TaskState::Running.can_transition_to(TaskState::Completed),
            "completion must go through Review so the gate cannot be skipped"
        );
    }

    #[test]
    fn cancellation_is_available_from_any_live_state() {
        for live in [
            TaskState::Pending,
            TaskState::Ready,
            TaskState::Running,
            TaskState::Waiting,
            TaskState::Review,
        ] {
            assert!(live.can_transition_to(TaskState::Cancelled), "{live}");
        }
    }

    #[test]
    fn illegal_transitions_are_rejected_with_a_useful_message() {
        let mut task = Task::new("do a thing");
        let err = task.transition_to(TaskState::Completed).unwrap_err();
        assert!(err.message().contains("PENDING -> COMPLETED"), "{err}");
        assert_eq!(
            task.state,
            TaskState::Pending,
            "state must not change on error"
        );
    }

    #[test]
    fn entering_running_counts_an_attempt() {
        let mut task = Task::new("g");
        task.max_attempts = 2;
        task.transition_to(TaskState::Ready).unwrap();
        task.transition_to(TaskState::Running).unwrap();
        assert_eq!(task.attempts, 1);
        assert!(!task.exhausted());
        task.transition_to(TaskState::Review).unwrap();
        task.transition_to(TaskState::Ready).unwrap();
        task.transition_to(TaskState::Running).unwrap();
        assert_eq!(task.attempts, 2);
        assert!(task.exhausted(), "retry budget must be enforceable");
    }

    #[test]
    fn dependencies_gate_readiness() {
        let mut graph = TaskGraph::new();
        let a = Task::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Task::new("test").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        assert_eq!(graph.newly_ready(), vec![a_id], "b is blocked on a");

        for state in [
            TaskState::Ready,
            TaskState::Running,
            TaskState::Review,
            TaskState::Completed,
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
        let mut graph = TaskGraph::new();
        let a = Task::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Task::new("deploy").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        graph.apply(a_id, TaskState::Ready).unwrap();
        graph.apply(a_id, TaskState::Running).unwrap();
        graph.apply(a_id, TaskState::Failed).unwrap();

        assert!(!graph.newly_ready().contains(&b_id));
        assert_eq!(graph.blocked(), vec![b_id]);
        assert!(
            graph.is_settled(),
            "a graph that cannot progress must read as settled"
        );
    }

    #[test]
    fn a_waiting_task_is_not_settled() {
        // An approval-gate workflow parks a task in WAITING. If that reads as settled,
        // the task runtime shuts down while a human is still deciding.
        let mut graph = TaskGraph::new();
        let a = Task::new("deploy");
        let a_id = a.id;
        graph.insert(a).unwrap();

        graph.apply(a_id, TaskState::Ready).unwrap();
        graph.apply(a_id, TaskState::Running).unwrap();
        graph.apply(a_id, TaskState::Waiting).unwrap();

        assert!(
            !graph.is_settled(),
            "a task waiting on a human has not finished"
        );
    }

    #[test]
    fn the_state_machine_cannot_be_bypassed_by_assignment() {
        // There is deliberately no way to write `task.state = Completed`. The only entry
        // point is `apply`, which validates.
        let mut graph = TaskGraph::new();
        let t = Task::new("deploy to production");
        let id = t.id;
        graph.insert(t).unwrap();
        graph.apply(id, TaskState::Ready).unwrap();
        graph.apply(id, TaskState::Running).unwrap();

        let err = graph.apply(id, TaskState::Completed).unwrap_err();
        assert!(err.message().contains("RUNNING -> COMPLETED"), "{err}");
        assert_eq!(graph.get(id).unwrap().state(), TaskState::Running);
    }

    #[test]
    fn a_review_free_workflow_still_passes_through_the_gate() {
        let mut task = Task::new("format the code");
        task.transition_to(TaskState::Ready).unwrap();
        task.transition_to(TaskState::Running).unwrap();
        task.complete_without_review().unwrap();
        assert_eq!(task.state(), TaskState::Completed);
    }

    #[test]
    fn the_attempt_budget_is_enforced_by_the_transition() {
        let mut task = Task::new("flaky");
        task.max_attempts = 1;
        task.transition_to(TaskState::Ready).unwrap();
        task.transition_to(TaskState::Running).unwrap();
        task.transition_to(TaskState::Ready).unwrap();

        let err = task.transition_to(TaskState::Running).unwrap_err();
        assert!(err.message().contains("all 1 attempts"), "{err}");
        assert_eq!(
            task.state(),
            TaskState::Ready,
            "a refused attempt must not advance the task"
        );
    }

    #[test]
    fn unknown_dependencies_are_rejected_at_insert() {
        let mut graph = TaskGraph::new();
        let orphan = Task::new("b").with_dependencies([TaskId::new()]);
        assert!(graph.insert(orphan).is_err());
    }

    #[test]
    fn self_dependency_is_rejected() {
        let mut graph = TaskGraph::new();
        let mut t = Task::new("loop");
        t.depends_on = vec![t.id];
        assert!(graph.insert(t).is_err());
    }

    #[test]
    fn cycles_are_rejected() {
        let mut graph = TaskGraph::new();
        let a = Task::new("a");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Task::new("b").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        // Close the loop by replacing `a` with a version that depends on `b`.
        let mut cyclic = Task::new("a");
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
        let a = TaskId::new();
        let b = TaskId::new();
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
        let err = serde_json::from_value::<TaskGraph>(json).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn a_persisted_graph_round_trips() {
        let mut graph = TaskGraph::new();
        let a = Task::new("implement");
        let a_id = a.id;
        graph.insert(a).unwrap();
        graph
            .insert(Task::new("test").with_dependencies([a_id]))
            .unwrap();

        let json = serde_json::to_string(&graph).unwrap();
        let back: TaskGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.newly_ready(), vec![a_id]);
    }

    #[test]
    fn a_persisted_unknown_dependency_is_rejected() {
        let a = TaskId::new();
        let json = serde_json::json!({
            a.as_uuid().to_string(): {
                "id": a, "goal": "a", "state": "PENDING", "depends_on": [TaskId::new()],
                "runs": [], "attempts": 0, "max_attempts": 3,
                "created_at": "1970-01-01T00:00:00Z", "updated_at": "1970-01-01T00:00:00Z"
            }
        });
        assert!(serde_json::from_value::<TaskGraph>(json).is_err());
    }

    #[test]
    fn a_rejected_insert_leaves_the_graph_usable() {
        let mut graph = TaskGraph::new();
        let a = Task::new("a");
        let a_id = a.id;
        graph.insert(a).unwrap();
        let b = Task::new("b").with_dependencies([a_id]);
        let b_id = b.id;
        graph.insert(b).unwrap();

        let mut cyclic = Task::new("a");
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
            TaskState::Completed
        );
        assert_eq!(
            ReviewVerdict::RequestChanges.resulting_state(),
            TaskState::Ready
        );
        assert_eq!(ReviewVerdict::Reject.resulting_state(), TaskState::Failed);
    }
}
