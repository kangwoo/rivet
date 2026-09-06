//! An `OpenAI`-compatible chat-completions adapter.
//!
//! One adapter serves `OpenAI`, `DeepSeek`, Ollama, vLLM and `OpenRouter`, because they
//! share a wire format — but only roughly, and the differences are exactly where an agent
//! breaks. So the risky parts are pure functions with recorded fixtures behind them:
//!
//! ```text
//! encode.rs   ModelRequest -> request JSON
//! decode.rs   SSE frames   -> StreamEvent, one Done, always
//! error.rs    status/body  -> ErrorKind, which drives retry
//! ```
//!
//! Only this module knows about `reqwest`.
//!
//! # Dialect notes
//!
//! - **Tool-call ids.** The provider's id is discarded and a [`rivet_core::id::ToolCallId`]
//!   is minted in its place; every message array we send renders our id. See
//!   [`ids`].
//! - **`StopSequence` is not reported.** Compatible endpoints strip the matched sequence
//!   and still send `finish_reason: "stop"`, so it is indistinguishable from a natural
//!   ending. Both are reported as [`StopReason::EndTurn`]; both end the turn, so nothing
//!   downstream behaves differently.
//! - **Usage may be estimated.** Providers that report none (Ollama, some vLLM builds) get
//!   an adapter-side estimate rather than zeros, because zeros would turn
//!   `max_total_tokens` into a limit that never trips. The substitution is logged at
//!   `debug`.
//! - **Reasoning is not replayed** unless `send_reasoning` is set: most compatible
//!   endpoints reject the field on the way in.

pub mod config;
pub mod decode;
pub mod encode;
pub mod error;
pub mod ids;
pub mod wire;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use rivet_core::capability::Permission;
use rivet_core::error::{Capability, Error};
use rivet_core::model::{
    Model, ModelCapabilities, ModelId, ModelRequest, ModelStream, StopReason, StreamEvent,
};
use rivet_core::plugin::{Plugin, PluginContext, PluginHandle, PluginManifest};

pub use config::OpenAiConfig;
pub use decode::StreamDecoder;
pub use ids::{SequentialIds, ToolCallIdFactory, Uuidv7Ids};

/// The plugin id this crate registers under.
pub const PLUGIN_ID: &str = "rivet.model-openai";

/// This crate's `rivet-plugin.toml`, for a host catalog to hand to the loader.
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");

/// Set to a directory to dump every raw SSE body for later use as a test fixture.
pub const RECORD_FIXTURES_ENV: &str = "RIVET_RECORD_FIXTURES";

/// A model served by an `OpenAI`-compatible endpoint.
#[derive(Debug)]
pub struct OpenAiModel {
    id: ModelId,
    config: OpenAiConfig,
    api_key: String,
    http: reqwest::Client,
    ids: Arc<dyn ToolCallIdFactory>,
}

impl OpenAiModel {
    /// Build an adapter.
    ///
    /// # Errors
    /// Fails when the HTTP client cannot be constructed with the configured timeouts.
    pub fn new(id: ModelId, config: OpenAiConfig, api_key: String) -> rivet_core::Result<Self> {
        Self::with_ids(id, config, api_key, Arc::new(Uuidv7Ids))
    }

    /// Build an adapter with an explicit tool-call id factory.
    ///
    /// # Errors
    /// Fails when the HTTP client cannot be constructed with the configured timeouts.
    pub fn with_ids(
        id: ModelId,
        config: OpenAiConfig,
        api_key: String,
        ids: Arc<dyn ToolCallIdFactory>,
    ) -> rivet_core::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.request_timeout_ms))
            .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
            .build()
            .map_err(|e| {
                Error::new(
                    rivet_core::error::ErrorKind::Internal,
                    Capability::Model,
                    "could not build the HTTP client",
                )
                .with_cause(e)
            })?;
        Ok(Self {
            id,
            config,
            api_key,
            http,
            ids,
        })
    }

    /// The configuration in force, for `rivet doctor`.
    #[must_use]
    pub fn config(&self) -> &OpenAiConfig {
        &self.config
    }
}

#[async_trait]
impl Model for OpenAiModel {
    fn id(&self) -> &ModelId {
        &self.id
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            tools: self.config.supports_tools,
            vision: self.config.supports_vision,
            reasoning: self.config.supports_reasoning,
            prompt_caching: self.config.supports_prompt_caching,
            context_window: self.config.context_window,
            max_output_tokens: self.config.max_output_tokens,
        }
    }

    async fn count_tokens(&self, request: &ModelRequest) -> rivet_core::Result<u64> {
        Ok(decode::estimate_request_tokens(request))
    }

    async fn stream(&self, request: ModelRequest) -> rivet_core::Result<ModelStream> {
        let input_estimate = decode::estimate_request_tokens(&request);
        let body = encode::encode_request(&request, &self.config)?;

        let response = self
            .http
            .post(self.config.completions_url())
            .bearer_auth(&self.api_key)
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(classify_transport)?;

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        if !status.is_success() {
            let retry_after = {
                let headers = response.headers().clone();
                error::retry_after_ms(|name| headers.get(name).and_then(|v| v.to_str().ok()))
            };
            let text = response.text().await.unwrap_or_default();
            return Err(error::classify_http(
                status.as_u16(),
                retry_after,
                &text,
                request_id.as_deref(),
            ));
        }

        let mut recorder = Recorder::open();
        let bytes = response.bytes_stream().map(move |chunk| {
            if let Ok(bytes) = &chunk {
                recorder.write(bytes);
            }
            chunk
        });

        Ok(into_stream(
            bytes.eventsource(),
            StreamDecoder::new(self.ids.clone(), input_estimate),
        ))
    }
}

/// Decode a recorded SSE body exactly as a live response would be decoded.
///
/// This is the entry point the `.sse` fixtures use, so a fixture test exercises the real
/// framing and the real decoder rather than a test-only re-implementation of both. Pair
/// it with [`RECORD_FIXTURES_ENV`] to replay something a live run captured.
#[must_use]
pub fn decode_recorded_stream(
    body: &str,
    ids: Arc<dyn ToolCallIdFactory>,
    input_tokens_estimate: u64,
) -> ModelStream {
    let owned = body.to_string();
    let bytes =
        futures_util::stream::once(
            async move { Ok::<_, std::convert::Infallible>(owned.into_bytes()) },
        );
    into_stream(
        bytes.eventsource(),
        StreamDecoder::new(ids, input_tokens_estimate),
    )
}

/// Drive an SSE event stream through the decoder, emitting exactly one `Done`.
fn into_stream<S, E>(events: S, decoder: StreamDecoder) -> ModelStream
where
    S: futures_util::Stream<Item = Result<eventsource_stream::Event, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    struct State<S> {
        events: S,
        decoder: StreamDecoder,
        queue: std::collections::VecDeque<StreamEvent>,
        finished: bool,
    }

    let state = State {
        events: Box::pin(events),
        decoder,
        queue: std::collections::VecDeque::new(),
        finished: false,
    };

    Box::pin(futures_util::stream::unfold(
        state,
        |mut state| async move {
            loop {
                if let Some(event) = state.queue.pop_front() {
                    return Some((Ok(event), state));
                }
                if state.finished {
                    return None;
                }
                match state.events.next().await {
                    Some(Ok(frame)) => match state.decoder.push(&frame.data) {
                        Ok(more) => state.queue.extend(more),
                        Err(err) => {
                            state.finished = true;
                            return Some((Err(err), state));
                        }
                    },
                    Some(Err(err)) => {
                        state.finished = true;
                        let err = Error::transient(Capability::Model, "the model stream failed")
                            .with_cause(err);
                        return Some((Err(err), state));
                    }
                    None => {
                        state.finished = true;
                        match state.decoder.finish() {
                            Ok(tail) => state.queue.extend(tail),
                            Err(err) => return Some((Err(err), state)),
                        }
                    }
                }
            }
        },
    ))
}

fn classify_transport(err: reqwest::Error) -> Error {
    let kind = if err.is_timeout() {
        rivet_core::error::ErrorKind::Timeout
    } else {
        // Connect, DNS, TLS and reset all belong in the same bucket: try again.
        rivet_core::error::ErrorKind::Transient
    };
    Error::new(
        kind,
        Capability::Model,
        "could not reach the model provider",
    )
    .with_cause(err)
}

/// Appends raw response bytes to a file when [`RECORD_FIXTURES_ENV`] is set.
///
/// This is how the `.sse` fixtures in `tests/fixtures` were produced: somebody with a key
/// runs a real request once and commits the bytes, so everyone else tests offline.
#[derive(Debug)]
struct Recorder(Option<std::fs::File>);

impl Recorder {
    fn open() -> Self {
        let Ok(dir) = std::env::var(RECORD_FIXTURES_ENV) else {
            return Self(None);
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return Self(None);
        }
        let name = format!(
            "{}/recorded-{}.sse",
            dir,
            rivet_core::Timestamp::now().as_millis()
        );
        Self(std::fs::File::create(name).ok())
    }

    fn write(&mut self, bytes: &[u8]) {
        use std::io::Write;
        if let Some(file) = self.0.as_mut() {
            let _ = file.write_all(bytes);
        }
    }
}

/// Registers one [`OpenAiModel`] built from the `[plugins."rivet.model-openai"]` table.
///
/// The model id arrives in `ctx.config` under the host-injected `agent` key rather than
/// through a constructor argument. That is the Phase 6 shape applied early: a plugin on
/// the other side of a process boundary receives bytes, not a [`ModelId`].
#[derive(Debug)]
pub struct OpenAiPlugin {
    manifest: PluginManifest,
}

impl OpenAiPlugin {
    #[must_use]
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }
}

#[async_trait]
impl Plugin for OpenAiPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    async fn load(&self, ctx: PluginContext) -> rivet_core::Result<PluginHandle> {
        // A model with no egress cannot answer, so this fails loudly rather than
        // degrading. `tool-filesystem` takes the other idiom -- both are in
        // `docs/plugin.md` §4.2, and which one applies is the plugin's decision.
        if !ctx
            .permissions
            .granted()
            .iter()
            .any(|p| matches!(p, Permission::NetworkHttp(_)))
        {
            return Err(Error::plugin(format!(
                "`{PLUGIN_ID}` needs `network_http`, and the active profile grants none; \
                 a model plugin with no egress cannot serve a request"
            )));
        }

        let config: OpenAiConfig = if ctx.config.is_null() {
            OpenAiConfig::default()
        } else {
            serde_json::from_value(ctx.config.clone()).map_err(|e| {
                Error::plugin(format!("`[plugins.\"{PLUGIN_ID}\"]` is not valid")).with_cause(e)
            })?
        };

        let model_id = ctx.config["agent"]["model"]
            .as_str()
            .ok_or_else(|| {
                Error::plugin(format!(
                    "`{PLUGIN_ID}` was loaded without `agent.model` in its config; \
                     the host injects that key for every plugin"
                ))
            })
            .and_then(ModelId::new)?;

        // Fail here, not at the first request: a missing key is a setup mistake and the
        // operator should learn about it before a session exists.
        let api_key = std::env::var(&config.api_key_env).map_err(|_| {
            Error::new(
                rivet_core::error::ErrorKind::InvalidArgument,
                Capability::Model,
                format!(
                    "the environment variable `{}` is not set; \
                     it must hold the API key for `{}`",
                    config.api_key_env, config.base_url
                ),
            )
        })?;

        let model = OpenAiModel::new(model_id, config, api_key)?;
        let name = model.id().to_string();
        ctx.registry.register_model(Arc::new(model)).await?;
        Ok(PluginHandle::new([format!("model:{name}")]))
    }

    async fn unload(&self, _ctx: PluginContext) -> rivet_core::Result<()> {
        Ok(())
    }
}

/// Whether a stop reason ends the run without further tool work.
#[must_use]
pub fn ends_the_turn(stop: StopReason) -> bool {
    matches!(
        stop,
        StopReason::EndTurn | StopReason::StopSequence | StopReason::Refusal
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_matches_the_crate() {
        let manifest = rivet_plugin::parse(MANIFEST_TOML).expect("the shipped manifest parses");
        assert_eq!(manifest.id.as_str(), PLUGIN_ID);
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.is_compatible_with(rivet_core::ABI_VERSION));
    }

    #[test]
    fn the_manifest_declares_a_model_and_asks_only_for_network_access() {
        let manifest = rivet_plugin::parse(MANIFEST_TOML).unwrap();
        assert_eq!(
            manifest.capabilities,
            [rivet_core::capability::CapabilityKind::Model]
        );
        assert_eq!(manifest.permissions, [Permission::NetworkHttp(None)]);
    }
}
