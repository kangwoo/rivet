//! SSE frames -> [`StreamEvent`]s. Pure: feed it strings, get events back.
//!
//! Keeping this free of `reqwest` is what lets the nine recorded `.sse` fixtures pin the
//! riskiest part of the adapter — dialect differences in tool-call framing — without a
//! network or a clock. The only non-determinism, tool-call id minting, is injected
//! through [`ToolCallIdFactory`].

use std::collections::HashMap;
use std::sync::Arc;

use rivet_core::context::estimate_tokens;
use rivet_core::model::{ContentBlock, Message, Role, StopReason, StreamEvent, Usage};
use rivet_core::tool::ToolCall;

use crate::error;
use crate::ids::ToolCallIdFactory;
use crate::wire::ChatChunk;

/// The sentinel frame that ends an `OpenAI` stream.
pub const DONE_SENTINEL: &str = "[DONE]";

/// One in-progress content block, in the order the runtime will see it.
#[derive(Debug)]
enum Block {
    Text(String),
    Reasoning(String),
    Tool {
        id: rivet_core::id::ToolCallId,
        name: String,
        arguments: String,
    },
}

/// Incremental decoder for one response stream.
///
/// Feed every `data:` payload to [`StreamDecoder::push`], then call
/// [`StreamDecoder::finish`] once the body ends.
#[derive(Debug)]
pub struct StreamDecoder {
    ids: Arc<dyn ToolCallIdFactory>,
    blocks: Vec<Block>,
    /// Wire tool-call index -> our block index.
    tool_blocks: HashMap<usize, usize>,
    text_block: Option<usize>,
    reasoning_block: Option<usize>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    saw_done: bool,
    /// Tokens the request was estimated to cost, used when the provider reports no usage.
    input_tokens_estimate: u64,
}

impl StreamDecoder {
    #[must_use]
    pub fn new(ids: Arc<dyn ToolCallIdFactory>, input_tokens_estimate: u64) -> Self {
        Self {
            ids,
            blocks: Vec::new(),
            tool_blocks: HashMap::new(),
            text_block: None,
            reasoning_block: None,
            finish_reason: None,
            usage: None,
            saw_done: false,
            input_tokens_estimate,
        }
    }

    /// Whether the terminating `[DONE]` sentinel has been seen.
    #[must_use]
    pub fn saw_done(&self) -> bool {
        self.saw_done
    }

    /// Decode one `data:` payload into zero or more stream events.
    ///
    /// # Errors
    /// A frame that is not JSON, or one carrying an `{"error": ...}` object, ends the
    /// stream. Both are classified by [`crate::error`] so the retry layer can act.
    pub fn push(&mut self, data: &str) -> rivet_core::Result<Vec<StreamEvent>> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == DONE_SENTINEL {
            self.saw_done = true;
            return Ok(Vec::new());
        }

        let chunk: ChatChunk =
            serde_json::from_str(data).map_err(|e| error::undecodable_frame(data, &e))?;

        if let Some(wire_error) = &chunk.error {
            return Err(error::classify_stream_error(wire_error));
        }

        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_read_tokens: usage.prompt_tokens_details.map_or(0, |d| d.cached_tokens),
                cache_write_tokens: 0,
            });
        }

        let mut events = Vec::new();
        for choice in chunk.choices {
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
            let delta = choice.delta;
            if let Some(text) = delta.content.filter(|t| !t.is_empty()) {
                let index = self.text_index(&mut events);
                if let Some(Block::Text(buffer)) = self.blocks.get_mut(index) {
                    buffer.push_str(&text);
                }
                events.push(StreamEvent::TextDelta { index, text });
            }
            if let Some(text) = delta
                .reasoning_content
                .or(delta.reasoning)
                .filter(|t| !t.is_empty())
            {
                let index = self.reasoning_index(&mut events);
                if let Some(Block::Reasoning(buffer)) = self.blocks.get_mut(index) {
                    buffer.push_str(&text);
                }
                events.push(StreamEvent::ReasoningDelta { index, text });
            }
            for call in delta.tool_calls {
                self.push_tool_delta(&call, &mut events);
            }
        }
        Ok(events)
    }

    /// One tool-call fragment.
    ///
    /// Fragments are keyed on the wire `index`, because `id` and `function.name` normally
    /// arrive only on the first fragment and `arguments` is split arbitrarily. Dialects
    /// that omit `index` (Ollama sends one complete call per chunk) are treated as index
    /// `0`, and a differing `id` opens a new block so consecutive complete calls do not
    /// merge into one.
    fn push_tool_delta(
        &mut self,
        call: &crate::wire::DeltaToolCall,
        events: &mut Vec<StreamEvent>,
    ) {
        let wire_index = call.index.unwrap_or(0);
        let mut block_index = self.tool_blocks.get(&wire_index).copied();

        if let (Some(existing), Some(new_id)) = (block_index, call.id.as_deref())
            && let Some(Block::Tool { id, .. }) = self.blocks.get(existing)
            && format!("{id}") != new_id
            && call.index.is_none()
        {
            // A second complete call in a dialect with no index: start a fresh block.
            block_index = None;
        }

        let index = if let Some(index) = block_index {
            index
        } else {
            {
                let index = self.blocks.len();
                let id = self.ids.next();
                let name = call
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_default();
                self.blocks.push(Block::Tool {
                    id,
                    name: name.clone(),
                    arguments: String::new(),
                });
                self.tool_blocks.insert(wire_index, index);
                events.push(StreamEvent::BlockStart {
                    index,
                    block: ContentBlock::ToolCall(ToolCall {
                        id,
                        name,
                        input: serde_json::Value::Object(serde_json::Map::new()),
                    }),
                });
                index
            }
        };

        let Some(Block::Tool {
            name, arguments, ..
        }) = self.blocks.get_mut(index)
        else {
            return;
        };
        if let Some(function) = &call.function {
            if let Some(new_name) = &function.name
                && !new_name.is_empty()
            {
                name.clone_from(new_name);
            }
            if let Some(fragment) = &function.arguments
                && !fragment.is_empty()
            {
                arguments.push_str(fragment);
                events.push(StreamEvent::ToolCallDelta {
                    index,
                    partial_json: fragment.clone(),
                });
            }
        }
    }

    fn text_index(&mut self, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        let index = self.blocks.len();
        self.blocks.push(Block::Text(String::new()));
        self.text_block = Some(index);
        events.push(StreamEvent::BlockStart {
            index,
            block: ContentBlock::text(""),
        });
        index
    }

    fn reasoning_index(&mut self, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(index) = self.reasoning_block {
            return index;
        }
        let index = self.blocks.len();
        self.blocks.push(Block::Reasoning(String::new()));
        self.reasoning_block = Some(index);
        events.push(StreamEvent::BlockStart {
            index,
            block: ContentBlock::Reasoning {
                text: String::new(),
                signature: None,
            },
        });
        index
    }

    /// Close every open block and produce the terminal [`StreamEvent::Done`].
    ///
    /// # Errors
    /// [`crate::error::incomplete_stream`] when the body ended with neither `[DONE]` nor
    /// a `finish_reason`: that is a truncated connection, not an answer.
    pub fn finish(&mut self) -> rivet_core::Result<Vec<StreamEvent>> {
        if self.finish_reason.is_none() && !self.saw_done {
            return Err(error::incomplete_stream());
        }

        let mut events: Vec<StreamEvent> = (0..self.blocks.len())
            .map(|index| StreamEvent::BlockEnd { index })
            .collect();

        let mut content = Vec::with_capacity(self.blocks.len());
        let mut has_tool_call = false;
        for block in &self.blocks {
            match block {
                Block::Text(text) => content.push(ContentBlock::Text { text: text.clone() }),
                Block::Reasoning(text) => content.push(ContentBlock::Reasoning {
                    text: text.clone(),
                    signature: None,
                }),
                Block::Tool {
                    id,
                    name,
                    arguments,
                } => {
                    has_tool_call = true;
                    content.push(ContentBlock::ToolCall(ToolCall {
                        id: *id,
                        name: name.clone(),
                        input: parse_arguments(arguments),
                    }));
                }
            }
        }

        let message = Message {
            role: Role::Assistant,
            content,
        };
        let stop_reason = self.stop_reason(has_tool_call);
        let usage = self.usage.unwrap_or_else(|| self.estimate_usage(&message));

        events.push(StreamEvent::Done {
            message,
            stop_reason,
            usage,
        });
        Ok(events)
    }

    /// Map `finish_reason`, then override it when the message actually carries tool calls.
    ///
    /// The override is not defensive programming: `DeepSeek` has been observed sending
    /// `"stop"` alongside a tool call, and trusting the field there ends the turn without
    /// ever running the tool.
    fn stop_reason(&self, has_tool_call: bool) -> StopReason {
        if has_tool_call {
            return StopReason::ToolUse;
        }
        match self.finish_reason.as_deref() {
            Some("tool_calls" | "function_call") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal,
            // A stop sequence is indistinguishable from a natural ending on this wire:
            // the endpoint strips the sequence from the output and still says "stop".
            _ => StopReason::EndTurn,
        }
    }

    /// Fill in usage for providers that report none (Ollama, some vLLM builds).
    ///
    /// Leaving it zero would be worse than an estimate: `max_total_tokens` would then be
    /// a limit that can never trip.
    fn estimate_usage(&self, message: &Message) -> Usage {
        Usage {
            input_tokens: self.input_tokens_estimate,
            output_tokens: u64::from(message.estimated_tokens()),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }
}

/// Parse accumulated tool arguments, refusing to guess when they are truncated.
///
/// A non-object survives to the dispatcher, whose schema check rejects it and hands the
/// model an error naming the problem — which is recoverable. Inventing an object is not.
fn parse_arguments(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

/// Estimate the tokens a request will cost, for providers that report no usage.
#[must_use]
pub fn estimate_request_tokens(request: &rivet_core::model::ModelRequest) -> u64 {
    let mut total = request.system.as_deref().map_or(0u32, estimate_tokens);
    for message in &request.messages {
        total = total.saturating_add(message.estimated_tokens());
    }
    for tool in &request.tools {
        total = total.saturating_add(estimate_tokens(&tool.description));
        total = total.saturating_add(estimate_tokens(&tool.input_schema.to_string()));
    }
    u64::from(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SequentialIds;

    fn decoder() -> StreamDecoder {
        StreamDecoder::new(Arc::new(SequentialIds::new()), 42)
    }

    #[test]
    fn truncated_arguments_stay_raw_rather_than_being_guessed() {
        let value = parse_arguments("{\"path\": \"src/li");
        assert_eq!(
            value,
            serde_json::Value::String("{\"path\": \"src/li".into()),
            "a non-object reaches schema validation, which the model can act on"
        );
    }

    #[test]
    fn empty_arguments_decode_to_an_empty_object() {
        assert_eq!(parse_arguments("  "), serde_json::json!({}));
    }

    #[test]
    fn a_stream_that_just_stops_is_transient() {
        let mut d = decoder();
        d.push(r#"{"choices":[{"delta":{"content":"partial"}}]}"#)
            .unwrap();
        let err = d.finish().unwrap_err();
        assert!(err.is_retryable(), "{err}");
    }

    #[test]
    fn tool_calls_override_a_stop_finish_reason() {
        let mut d = decoder();
        d.push(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1",
               "function":{"name":"read_file","arguments":"{}"}}]},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
        let events = d.finish().unwrap();
        match events.last().unwrap() {
            StreamEvent::Done { stop_reason, .. } => {
                assert_eq!(*stop_reason, StopReason::ToolUse);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn usage_is_estimated_when_the_provider_reports_none() {
        let mut d = decoder();
        d.push(r#"{"choices":[{"delta":{"content":"hello there"},"finish_reason":"stop"}]}"#)
            .unwrap();
        let events = d.finish().unwrap();
        match events.last().unwrap() {
            StreamEvent::Done { usage, .. } => {
                assert_eq!(usage.input_tokens, 42);
                assert!(usage.output_tokens > 0, "a zero would disarm the limit");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }
}
