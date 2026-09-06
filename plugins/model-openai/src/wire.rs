//! The `OpenAI` chat-completions wire format, as serde types.
//!
//! Only the fields Rivet actually reads are modeled, and every one of them is
//! `#[serde(default)]`. Compatible endpoints disagree about which fields are optional
//! (Ollama omits `usage`, `DeepSeek` adds `reasoning_content`, vLLM sometimes omits the
//! tool-call `index`), and a decoder that rejects an unexpected shape turns a cosmetic
//! difference into a failed run.

use serde::{Deserialize, Serialize};

/// One `data:` frame of a streaming chat completion.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatChunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
    /// Some gateways start with `200 OK` and then report the failure inside the stream.
    #[serde(default)]
    pub error: Option<WireError>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// `DeepSeek` and some vLLM builds stream chain-of-thought here.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// `OpenRouter`'s spelling of the same thing.
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<DeltaToolCall>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DeltaToolCall {
    /// Absent in dialects that send one complete tool call per chunk.
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<DeltaFunction>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DeltaFunction {
    #[serde(default)]
    pub name: Option<String>,
    /// A fragment of the arguments JSON, not necessarily parseable on its own.
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u64,
}

/// The error envelope both `4xx` bodies and mid-stream failures use.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct WireError {
    #[serde(default)]
    pub message: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub code: Option<serde_json::Value>,
}

/// The top-level object a `4xx`/`5xx` body carries.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct WireErrorBody {
    #[serde(default)]
    pub error: Option<WireError>,
}

// --- request side ---------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireFunctionCall,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireFunctionCall {
    pub name: String,
    /// A JSON *string*, per the `OpenAI` schema — not an object.
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireFunctionSpec,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireFunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
