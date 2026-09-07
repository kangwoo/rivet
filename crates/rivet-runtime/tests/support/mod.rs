//! Test doubles for the agent loop.
//!
//! Two choices here are deliberate. The model double replays **recorded SSE bytes through
//! the real adapter's decoder**, so a loop test exercises the same code path a live run
//! would; and it records every request it was handed, which is how "did the model actually
//! see the tool result?" becomes an assertion rather than an assumption.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use rivet_core::agent::{AgentSpec, RunLimits};
use rivet_core::context::ContextProvider;
use rivet_core::error::{Capability, Error};
use rivet_core::id::{PluginId, PluginInstanceId, SessionId};
use rivet_core::model::{Model, ModelCapabilities, ModelId, ModelRequest, ModelStream, Role};
use rivet_core::session::{Expect, SessionEvent, SessionStore, SessionSummary, StoredEvent};
use rivet_core::tool::{Tool, ToolContext, ToolResult, ToolSpec};
use rivet_core::workspace::Workspace;
use rivet_model_openai::{SequentialIds, decode_recorded_stream};
use rivet_runtime::registry::{Owner, ScopedRegistry};
use rivet_runtime::{BroadcastBus, ContextAssembler, Registry};
use rivet_session::JsonlSessionStore;
use tokio::sync::Mutex;

// --- SSE bodies ------------------------------------------------------------------------

/// A plain text answer.
pub fn sse_text(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
         \"content\":{}}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\
         \"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":5}}}}\n\n\
         data: [DONE]\n\n",
        serde_json::Value::String(text.to_string())
    )
}

/// One or more tool calls in a single response.
pub fn sse_tool_calls(calls: &[(&str, serde_json::Value)]) -> String {
    let mut body = String::new();
    for (index, (name, arguments)) in calls.iter().enumerate() {
        let _ = write!(
            body,
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
             \"tool_calls\":[{{\"index\":{index},\"id\":\"call_{index}\",\"type\":\"function\",\
             \"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\
             \"finish_reason\":null}}]}}\n\n",
            serde_json::Value::String(arguments.to_string())
        );
    }
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\
         \"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n\
         data: [DONE]\n\n",
    );
    body
}

/// A plain text answer delivered as `chunks` separate deltas.
///
/// One `agent.text.delta` per chunk, so a test can put a known number of events on the bus
/// through the real decoder rather than by publishing them by hand.
pub fn sse_text_in_chunks(text: &str, chunks: usize) -> String {
    let mut body = String::new();
    for index in 0..chunks.max(1) {
        let _ = write!(
            body,
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
             \"content\":{}}},\"finish_reason\":null}}]}}\n\n",
            serde_json::Value::String(format!("{text}{index} "))
        );
    }
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
         \"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n\
         data: [DONE]\n\n",
    );
    body
}

/// An answer cut off at the output token limit.
pub fn sse_truncated(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
         \"content\":{}}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\n\n\
         data: [DONE]\n\n",
        serde_json::Value::String(text.to_string())
    )
}

/// A text answer reporting a large usage, for the token limit.
pub fn sse_text_costing(text: &str, tokens: u64) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
         \"content\":{}}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\
         \"usage\":{{\"prompt_tokens\":{tokens},\"completion_tokens\":0}}}}\n\n\
         data: [DONE]\n\n",
        serde_json::Value::String(text.to_string())
    )
}

// --- the model double -------------------------------------------------------------------

/// What the model should do for the next request.
#[derive(Clone, Debug)]
pub enum Reply {
    /// Decode this SSE body through the real adapter.
    Sse(String),
    /// Fail before a stream exists.
    Fail(Error),
}

/// A `Model` that replays a script and records what it was asked.
#[derive(Debug)]
pub struct FixtureModel {
    id: ModelId,
    script: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<ModelRequest>>,
    /// Used when the script runs dry, so a runaway loop ends rather than panics.
    fallback: String,
}

impl FixtureModel {
    #[must_use]
    pub fn new(replies: Vec<Reply>) -> Self {
        Self {
            id: ModelId::new("fixture/test-model").unwrap(),
            script: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            fallback: sse_text("done"),
        }
    }

    /// A model that answers the same way forever, for tests about limits rather than
    /// about conversations.
    #[must_use]
    pub fn repeating(body: String) -> Self {
        Self {
            id: ModelId::new("fixture/test-model").unwrap(),
            script: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            fallback: body,
        }
    }

    /// Every request the loop sent, in order.
    pub async fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().await.clone()
    }

    /// How many requests were sent. Retries show up here as separate requests.
    pub async fn request_count(&self) -> usize {
        self.seen.lock().await.len()
    }
}

#[async_trait]
impl Model for FixtureModel {
    fn id(&self) -> &ModelId {
        &self.id
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tools: true,
            context_window: Some(128_000),
            ..ModelCapabilities::default()
        }
    }

    async fn stream(&self, request: ModelRequest) -> rivet_core::Result<ModelStream> {
        self.seen.lock().await.push(request);
        let reply = self
            .script
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| Reply::Sse(self.fallback.clone()));
        match reply {
            Reply::Sse(body) => Ok(decode_recorded_stream(
                &body,
                Arc::new(SequentialIds::new()),
                10,
            )),
            Reply::Fail(error) => Err(error),
        }
    }
}

/// The tool results visible in a recorded request, in order.
#[must_use]
pub fn tool_results_in(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .flat_map(|m| {
            m.content.iter().filter_map(|b| match b {
                rivet_core::model::ContentBlock::ToolResult { content, .. } => {
                    Some(content.clone())
                }
                _ => None,
            })
        })
        .collect()
}

// --- tool doubles -----------------------------------------------------------------------

/// Succeeds, echoing its input.
#[derive(Debug)]
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "echo",
            "echo the message back",
            serde_json::json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
                "additionalProperties": false
            }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        Ok(ToolResult::ok(
            input["message"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

/// Reads a file through the same guard the real filesystem tools use.
#[derive(Debug)]
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read_file",
            "read a file from the workspace",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        use tokio::io::AsyncReadExt;
        let path = input["path"].as_str().unwrap_or_default();
        let (mut file, _real) =
            rivet_runtime::fsguard::open_read(ctx.workspace(), std::path::Path::new(path)).await?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .await
            .map_err(|e| Error::internal("read failed").with_cause(e))?;
        Ok(ToolResult::ok(text))
    }
}

/// Always reports a failure the model should react to.
#[derive(Debug)]
pub struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "always_fails",
            "always reports an error",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        Ok(ToolResult::error("it failed again"))
    }
}

/// Reports progress before it answers, so a test can observe `tool.execute.progress`.
///
/// That topic has exactly one publisher — `ctx.host.progress` in `dispatch.rs` — and no
/// tool in this repository calls it, so without this double the topic is unreachable from
/// a loop test.
#[derive(Debug)]
pub struct ProgressTool;

#[async_trait]
impl Tool for ProgressTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "with_progress",
            "reports progress, then answers",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        ctx.host.progress("halfway");
        Ok(ToolResult::ok("done"))
    }
}

/// Honors cancellation promptly.
///
/// `trip` makes the timing deterministic: the token is cancelled the instant the tool
/// starts, so the interruption is guaranteed to land *inside* a tool call rather than
/// racing the model round trip that precedes it. Sleeping for a fixed time in the test and
/// hoping the run got far enough is how a cancellation test becomes a flaky one.
#[derive(Debug, Default)]
pub struct SlowTool {
    trip: Option<tokio_util::sync::CancellationToken>,
}

impl SlowTool {
    #[must_use]
    pub fn tripping(cancel: tokio_util::sync::CancellationToken) -> Self {
        Self { trip: Some(cancel) }
    }
}

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "slow",
            "takes a while, but stops when asked",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        if let Some(trip) = &self.trip {
            trip.cancel();
        }
        tokio::select! {
            () = ctx.host.cancelled() => Err(Error::cancelled("slow tool stopped on request")),
            () = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                Ok(ToolResult::ok("finished after all"))
            }
        }
    }
}

/// Ignores cancellation entirely, so the grace period has to give up on it.
#[derive(Debug, Default)]
pub struct StubbornTool {
    trip: Option<tokio_util::sync::CancellationToken>,
}

impl StubbornTool {
    #[must_use]
    pub fn tripping(cancel: tokio_util::sync::CancellationToken) -> Self {
        Self { trip: Some(cancel) }
    }
}

#[async_trait]
impl Tool for StubbornTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "stubborn",
            "ignores cancellation",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        if let Some(trip) = &self.trip {
            trip.cancel();
        }
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        Ok(ToolResult::ok("eventually"))
    }
}

/// Panics, to prove one bad tool does not take the run with it.
#[derive(Debug)]
pub struct PanickingTool;

#[async_trait]
impl Tool for PanickingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "panics",
            "panics on purpose",
            serde_json::json!({ "type": "object", "additionalProperties": false }),
        )
        .unwrap()
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        panic!("this tool is broken");
    }
}

// --- a store that fails on demand ---------------------------------------------------------

/// Wraps a store and fails the nth append, for the "cannot write the log" path.
#[derive(Debug)]
pub struct FailingStore {
    inner: Arc<dyn SessionStore>,
    fail_at: usize,
    seen: std::sync::atomic::AtomicUsize,
}

impl FailingStore {
    #[must_use]
    pub fn new(inner: Arc<dyn SessionStore>, fail_at: usize) -> Self {
        Self {
            inner,
            fail_at,
            seen: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl SessionStore for FailingStore {
    async fn create(&self, id: SessionId, event: SessionEvent) -> rivet_core::Result<StoredEvent> {
        self.inner.create(id, event).await
    }

    async fn append(
        &self,
        id: SessionId,
        expect: Expect,
        event: SessionEvent,
    ) -> rivet_core::Result<StoredEvent> {
        let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if n == self.fail_at {
            return Err(Error::storage("the disk is full"));
        }
        self.inner.append(id, expect, event).await
    }

    async fn read(
        &self,
        id: SessionId,
        from_seq: u64,
        limit: usize,
    ) -> rivet_core::Result<Vec<StoredEvent>> {
        self.inner.read(id, from_seq, limit).await
    }

    async fn last_seq(&self, id: SessionId) -> rivet_core::Result<u64> {
        self.inner.last_seq(id).await
    }

    async fn list(&self, limit: usize) -> rivet_core::Result<Vec<SessionSummary>> {
        self.inner.list(limit).await
    }

    async fn fork(
        &self,
        source: SessionId,
        at_seq: u64,
        new_id: SessionId,
    ) -> rivet_core::Result<SessionSummary> {
        self.inner.fork(source, at_seq, new_id).await
    }
}

// --- the harness --------------------------------------------------------------------------

/// A registry, bus, store and workspace wired together the way the CLI wires them.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub registry: Registry,
    pub bus: Arc<BroadcastBus>,
    pub store: Arc<dyn SessionStore>,
    pub workspace: Workspace,
    pub session_id: SessionId,
    scoped: ScopedRegistry,
}

impl Harness {
    pub async fn new() -> Self {
        Self::with_store(|store| store).await
    }

    /// A harness whose bus holds `capacity` events, for tests about lag.
    ///
    /// The default 4096 is deep enough that making a subscriber fall behind would mean
    /// publishing thousands of events; a shallow bus makes the same property provable in a
    /// handful.
    pub async fn with_bus_capacity(capacity: usize) -> Self {
        Self::build(|store| store, capacity).await
    }

    /// Build a harness whose store is wrapped by `wrap`.
    pub async fn with_store(
        wrap: impl FnOnce(Arc<dyn SessionStore>) -> Arc<dyn SessionStore>,
    ) -> Self {
        Self::build(wrap, 4096).await
    }

    async fn build(
        wrap: impl FnOnce(Arc<dyn SessionStore>) -> Arc<dyn SessionStore>,
        bus_capacity: usize,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let workspace =
            rivet_runtime::workspace::open(dir.path(), [".env".to_string()]).expect("workspace");
        let bus = Arc::new(BroadcastBus::with_capacity(bus_capacity));
        let registry = Registry::new((*bus).clone());
        let scoped = registry.scoped(Owner {
            plugin_id: PluginId::new("rivet.test").unwrap(),
            instance_id: PluginInstanceId::new(),
        });

        let inner: Arc<dyn SessionStore> =
            Arc::new(JsonlSessionStore::new(dir.path().join(".rivet/sessions")));
        let store = wrap(inner);

        let session_id = SessionId::new();
        store
            .create(
                session_id,
                SessionEvent::Created {
                    workspace_root: workspace.root().display().to_string(),
                    parent: None,
                },
            )
            .await
            .expect("create session");

        Self {
            dir,
            registry,
            bus,
            store,
            workspace,
            session_id,
            scoped,
        }
    }

    /// A subscriber that remembers every bus topic it saw, already attached.
    ///
    /// Attached through `observe` rather than `attach`, because a test wants to *end* the
    /// pump and read what it got — the same reason the host does.
    #[must_use]
    pub fn recorder(&self) -> (Arc<Recorder>, rivet_runtime::Observer) {
        let recorder = Arc::new(Recorder::default());
        let observer = self.bus.observe(recorder.clone());
        (recorder, observer)
    }

    pub async fn register_model(&self, model: Arc<dyn Model>) {
        use rivet_core::plugin::PluginRegistry;
        self.scoped.register_model(model).await.expect("model");
    }

    pub async fn register_tool(&self, tool: Arc<dyn Tool>) {
        use rivet_core::plugin::PluginRegistry;
        self.scoped.register_tool(tool).await.expect("tool");
    }

    pub async fn register_provider(&self, provider: Arc<dyn ContextProvider>) {
        use rivet_core::plugin::PluginRegistry;
        self.scoped
            .register_context_provider(provider)
            .await
            .expect("provider");
    }

    /// An agent bound to the fixture model, with generous limits.
    #[must_use]
    #[allow(clippy::unused_self)] // Reads as part of the harness at every call site.
    pub fn agent(&self) -> AgentSpec {
        let mut agent = AgentSpec::new("test", ModelId::new("fixture/test-model").unwrap());
        agent.instructions = "Be brief.".into();
        agent.limits = RunLimits {
            max_turns: 10,
            max_duration_ms: 60_000,
            max_total_tokens: 1_000_000,
            max_context_tokens: 100_000,
            max_consecutive_tool_errors: 3,
        };
        agent
    }

    /// An assembler with just the system prompt provider, so tests assert on the loop
    /// rather than on a repository sketch.
    #[must_use]
    #[allow(clippy::unused_self)] // Reads as part of the harness at every call site.
    pub fn assembler(&self) -> ContextAssembler {
        ContextAssembler::new(vec![Arc::new(
            rivet_runtime::context::providers::SystemPromptProvider::new("Be brief."),
        )])
    }

    /// The conversation as a fresh process would rebuild it.
    pub async fn state(&self) -> rivet_core::session::SessionState {
        rivet_core::session::SessionState::replay(&self.events().await)
    }

    /// Everything currently in the session log.
    pub async fn events(&self) -> Vec<StoredEvent> {
        rivet_runtime::session_recovery::read_all(self.store.as_ref(), self.session_id)
            .await
            .expect("read log")
    }

    /// The session event wire names, in order. Reads like a story in a failure message.
    pub async fn topics(&self) -> Vec<String> {
        self.events()
            .await
            .iter()
            .map(|e| {
                serde_json::to_value(&e.event).unwrap()["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

/// Collects bus topics, and the payloads a test needs to look inside.
#[derive(Debug, Default)]
pub struct Recorder {
    seen: std::sync::Mutex<Vec<rivet_core::event::EventEnvelope>>,
}

impl Recorder {
    /// Every topic seen, in delivery order.
    #[must_use]
    pub fn topics(&self) -> Vec<String> {
        self.envelopes()
            .iter()
            .map(|e| e.topic().to_string())
            .collect()
    }

    #[must_use]
    pub fn envelopes(&self) -> Vec<rivet_core::event::EventEnvelope> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The lag reports seen, as `(subscriber, dropped)`.
    #[must_use]
    pub fn lag_reports(&self) -> Vec<(String, u64)> {
        self.envelopes()
            .iter()
            .filter_map(|e| match &e.payload {
                rivet_core::event::Event::Runtime(
                    rivet_core::event::RuntimeEvent::SubscriberLagged {
                        subscriber,
                        dropped,
                    },
                ) => Some((subscriber.clone(), *dropped)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl rivet_core::event::EventSubscriber for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }

    async fn on_event(&self, envelope: &rivet_core::event::EventEnvelope) {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(envelope.clone());
    }
}

/// Fold a log the way a fresh process would.
#[must_use]
pub fn replay(events: &[StoredEvent]) -> rivet_core::session::SessionState {
    rivet_core::session::SessionState::replay(events)
}

/// A capability-tagged transient error, for retry tests.
#[must_use]
pub fn transient(message: &str) -> Error {
    Error::transient(Capability::Model, message)
}
