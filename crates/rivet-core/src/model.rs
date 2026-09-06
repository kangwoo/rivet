//! The model contract.
//!
//! Rivet talks to every provider through one streaming interface. Providers differ wildly
//! in wire format (`OpenAI` chat completions, Anthropic messages, raw completion APIs), so
//! the adapter's job is to normalize *into* the types here — not to leak provider shapes
//! upward.
//!
//! Two decisions worth calling out:
//!
//! 1. **Streaming is the only mode.** A non-streaming provider is adapted by emitting one
//!    chunk then a stop. The reverse (faking streaming on a blocking API) forces every UI
//!    to implement two code paths.
//! 2. **The model never executes anything.** It returns [`ToolCall`]s as data. Execution
//!    is the runtime's job, behind policy. This is what makes `readonly` profiles real.

use std::fmt;
use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::id::ToolCallId;
use crate::tool::{ToolCall, ToolSpec};

/// Stable identifier for a model, namespaced by provider: `openai/gpt-4o`,
/// `anthropic/claude-sonnet-5`, `ollama/qwen2.5-coder`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(raw: impl Into<String>) -> crate::Result<Self> {
        let raw = raw.into();
        if raw.is_empty() || !raw.contains('/') {
            return Err(crate::Error::invalid_argument(format!(
                "model id `{raw}` must be `provider/model`"
            )));
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The provider segment, used to route to a registered model plugin.
    #[must_use]
    pub fn provider(&self) -> &str {
        self.0.split('/').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModelId({})", self.0)
    }
}

/// Who produced a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Carries [`ContentBlock::ToolResult`] back to the model.
    Tool,
}

/// One piece of message content.
///
/// Modeled as blocks rather than a flat string because tool calls, reasoning traces and
/// images all need to survive a session replay without lossy re-encoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Provider-side reasoning. Kept separate from `Text` so a UI can fold it and a
    /// context assembler can drop it from history without touching the answer.
    Reasoning {
        text: String,
        /// Opaque provider token needed to replay reasoning on some APIs.
        signature: Option<String>,
    },
    ToolCall(ToolCall),
    ToolResult {
        call_id: ToolCallId,
        /// Rendered output the model sees.
        content: String,
        is_error: bool,
    },
    Image {
        media_type: String,
        /// Base64. Large blobs should be referenced by the session store instead; see
        /// `docs/architecture.md` on payload offloading.
        data: String,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// A message in the conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Estimated tokens for this message, counting **every** block.
    ///
    /// Uses [`crate::context::estimate_tokens`] per block, so CJK text is not undercounted
    /// the way a flat `bytes / 4` would undercount it.
    #[must_use]
    pub fn estimated_tokens(&self) -> u32 {
        self.content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text, .. } => {
                    crate::context::estimate_tokens(text)
                }
                ContentBlock::ToolCall(call) => crate::context::estimate_tokens(&call.name)
                    .saturating_add(crate::context::estimate_tokens(&call.input.to_string())),
                ContentBlock::ToolResult { content, .. } => {
                    crate::context::estimate_tokens(content)
                }
                // Base64 is ASCII; providers bill images by tile, but over-counting here
                // is the safe direction.
                ContentBlock::Image { data, .. } => {
                    u32::try_from(data.len() / 4).unwrap_or(u32::MAX)
                }
            })
            .fold(0u32, u32::saturating_add)
    }

    /// The bytes a provider will see for this message, counting every block.
    ///
    /// Use [`Message::estimated_tokens`] for budgeting; this is the raw size, for limits
    /// expressed in bytes.
    ///
    /// Unlike [`Message::text`], this counts *every* block: reasoning, tool arguments,
    /// tool results and image payloads all consume context. A 200 KB test log coming back
    /// from a tool is 200 KB of context whether or not it is "text".
    #[must_use]
    pub fn billable_len(&self) -> usize {
        self.content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text, .. } => text.len(),
                ContentBlock::ToolCall(call) => call.name.len() + call.input.to_string().len(),
                ContentBlock::ToolResult { content, .. } => content.len(),
                // Base64 is ~4 chars per 3 bytes; providers bill images by tile, but
                // over-counting here is the safe direction.
                ContentBlock::Image { data, .. } => data.len(),
            })
            .sum()
    }

    /// Concatenated text blocks, ignoring reasoning and tool traffic.
    ///
    /// This is for display and for assertions. **Do not use it for budgeting** — see
    /// [`Message::billable_len`].
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Tool calls requested by this message.
    #[must_use]
    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect()
    }
}

/// Knobs that every provider is expected to honor or explicitly ignore.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub stop_sequences: Vec<String>,
    /// Provider-specific extras. An adapter must ignore keys it does not understand
    /// rather than erroring, so a config can target several providers.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// How the model should treat the offered tools.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

/// One request to a model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelId,
    /// The assembled system prompt. Separate from `messages` because most providers
    /// treat it specially and because it is cache-key relevant.
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,
    pub params: ModelParams,
}

/// Token accounting for one request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// Why the model stopped generating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The model wants tools run; the loop should execute and continue.
    ToolUse,
    /// Output token cap hit. The turn is *incomplete* — see `docs/architecture.md` on
    /// continuation.
    MaxTokens,
    /// A stop sequence matched.
    StopSequence,
    /// The provider refused.
    Refusal,
}

/// One item in a model's output stream.
///
/// The stream is a sequence of deltas terminated by exactly one [`StreamEvent::Done`].
/// Adapters must guarantee that: the runtime relies on it to close a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// A new content block is starting at `index`.
    BlockStart { index: usize, block: ContentBlock },
    /// Append text to the block at `index`.
    TextDelta { index: usize, text: String },
    /// Append reasoning text to the block at `index`.
    ReasoningDelta { index: usize, text: String },
    /// Append raw JSON fragment to the arguments of the tool call at `index`.
    ToolCallDelta { index: usize, partial_json: String },
    /// The block at `index` is complete.
    BlockEnd { index: usize },
    /// Terminal. Carries the fully assembled message so consumers that do not want to
    /// accumulate deltas can ignore everything above.
    Done {
        message: Message,
        stop_reason: StopReason,
        usage: Usage,
    },
}

/// A boxed stream of model output.
pub type ModelStream = Pin<Box<dyn Stream<Item = crate::Result<StreamEvent>> + Send>>;

/// What a provider can do. Used by the runtime to reject impossible requests early and to
/// pick fallbacks.
///
/// A flag struct: each field is an independent yes/no fact about the provider, so the
/// `struct_excessive_bools` heuristic (which suggests an enum) does not apply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    pub prompt_caching: bool,
    /// Total context window in tokens, if the provider publishes one.
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

/// A model provider.
///
/// Implementors live in plugin crates (`plugins/model-openai`, ...) and are registered
/// under a provider name. They must be cancellation-aware: dropping the returned stream
/// has to abort the in-flight HTTP request.
#[async_trait]
pub trait Model: Send + Sync + fmt::Debug {
    /// The id this instance serves.
    fn id(&self) -> &ModelId;

    fn capabilities(&self) -> ModelCapabilities;

    /// Start a streaming completion.
    ///
    /// Errors returned here are *connection-level*. Once the stream is returned, failures
    /// arrive as `Err` items inside it. Both must carry an accurate
    /// [`crate::error::ErrorKind`] so retry works.
    async fn stream(&self, request: ModelRequest) -> crate::Result<ModelStream>;

    /// Best-effort token count for a request, used for context budgeting.
    ///
    /// The default counts **every** billable part of the request — system prompt, tool
    /// schemas, and all message blocks including tool arguments and results — through
    /// [`crate::context::estimate_tokens`], which is character-based rather than
    /// byte-based so CJK text is not undercounted.
    ///
    /// Two earlier versions of this were wrong in instructive ways: one summed only
    /// `Message::text`, reporting a 200 KB tool result as zero tokens; the next divided
    /// total bytes by four, undercounting Korean by 2-4x. Both made the budget
    /// meaningless in exactly the runs it exists to protect. The second fix landed on
    /// `Message::estimated_tokens` but left this body on `billable_len() / 4`, so the
    /// assembler's budget and this re-check disagreed by 2-4x on CJK text -- the very
    /// failure that was supposed to have been repaired. `tests::count_tokens_does_not_undercount_cjk`
    /// pins it now.
    ///
    /// It is still an estimate. Providers exposing a real tokenizer or a counting endpoint
    /// should override this; callers must never treat the result as a guarantee.
    async fn count_tokens(&self, request: &ModelRequest) -> crate::Result<u64> {
        let mut total = request
            .system
            .as_deref()
            .map_or(0, crate::context::estimate_tokens);

        for message in &request.messages {
            total = total.saturating_add(message.estimated_tokens());
        }

        // Tool schemas are sent on every request and are not free.
        for tool in &request.tools {
            total = total.saturating_add(crate::context::estimate_tokens(&tool.description));
            total = total.saturating_add(crate::context::estimate_tokens(
                &tool.input_schema.to_string(),
            ));
        }

        Ok(u64::from(total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_requires_a_provider_segment() {
        assert!(ModelId::new("openai/gpt-4o").is_ok());
        assert!(ModelId::new("gpt-4o").is_err());
        assert_eq!(
            ModelId::new("anthropic/claude").unwrap().provider(),
            "anthropic"
        );
    }

    #[test]
    fn message_text_ignores_reasoning_and_tool_blocks() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "hmm".into(),
                    signature: None,
                },
                ContentBlock::text("answer"),
                ContentBlock::ToolResult {
                    call_id: ToolCallId::new(),
                    content: "ignored".into(),
                    is_error: false,
                },
            ],
        };
        assert_eq!(msg.text(), "answer");
    }

    #[test]
    fn stream_events_round_trip_as_tagged_json() {
        let ev = StreamEvent::TextDelta {
            index: 0,
            text: "hi".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""), "{json}");
        assert_eq!(serde_json::from_str::<StreamEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn billable_length_counts_tool_traffic() {
        let big_result = "x".repeat(200_000);
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: ToolCallId::new(),
                content: big_result,
                is_error: false,
            }],
        };
        assert_eq!(
            msg.text(),
            "",
            "text() deliberately ignores tool traffic; that is why budgeting must not use it"
        );
        assert!(
            msg.billable_len() >= 200_000,
            "a 200KB tool result must not count as zero context"
        );
        assert!(
            msg.estimated_tokens() >= 40_000,
            "and the token estimate must reflect it"
        );
    }

    #[tokio::test]
    async fn count_tokens_does_not_undercount_cjk() {
        // The assembler budgets with `estimate_tokens` and the loop re-checks with this
        // method. If they disagree, the re-check either rejects requests that fit or
        // admits requests that overflow -- and on Korean the byte-based version was wrong
        // by 2-4x in the dangerous direction.
        #[derive(Debug)]
        struct Bare(ModelId);

        #[async_trait]
        impl Model for Bare {
            fn id(&self) -> &ModelId {
                &self.0
            }
            fn capabilities(&self) -> ModelCapabilities {
                ModelCapabilities::default()
            }
            async fn stream(&self, _request: ModelRequest) -> crate::Result<ModelStream> {
                unimplemented!("not exercised")
            }
        }

        let korean = "로그인 API를 구현해줘. 테스트가 통과해야 한다.".repeat(20);
        let request = ModelRequest {
            model: ModelId::new("test/model").unwrap(),
            system: None,
            messages: vec![Message::user(&korean)],
            tools: Vec::new(),
            tool_choice: ToolChoice::default(),
            params: ModelParams::default(),
        };

        let counted = Bare(ModelId::new("test/model").unwrap())
            .count_tokens(&request)
            .await
            .unwrap();
        assert_eq!(
            counted,
            u64::from(crate::context::estimate_tokens(&korean)),
            "the default count must agree with the estimate the assembler budgets against"
        );
        assert!(
            counted > (korean.len() / 4) as u64,
            "and it must exceed the naive bytes/4, which undercounts CJK"
        );
    }

    #[test]
    fn korean_messages_are_not_undercounted() {
        let korean = "로그인 API를 구현해줘. 테스트가 통과해야 한다.".repeat(20);
        let msg = Message::user(&korean);
        let naive_bytes_over_four = u32::try_from(korean.len() / 4).unwrap();
        assert!(
            msg.estimated_tokens() > naive_bytes_over_four,
            "message budgeting must use the character-based estimate, not bytes/4"
        );
    }

    #[test]
    fn billable_length_counts_reasoning_and_tool_arguments() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "thinking hard".into(),
                    signature: None,
                },
                ContentBlock::ToolCall(ToolCall {
                    id: ToolCallId::new(),
                    name: "shell".into(),
                    input: serde_json::json!({ "command": "cargo test --workspace" }),
                }),
            ],
        };
        assert!(msg.billable_len() > "thinking hard".len());
    }

    #[test]
    fn usage_totals_exclude_cache_columns() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 100,
            cache_write_tokens: 20,
        };
        assert_eq!(usage.total(), 15);
    }
}
