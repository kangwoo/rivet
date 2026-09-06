//! `ModelRequest` -> `OpenAI` chat-completions JSON. Pure: no clock, no I/O, no network.

use rivet_core::model::{ContentBlock, Message, ModelRequest, Role, ToolChoice};
use rivet_core::tool::ToolCall;
use serde_json::{Map, Value};

use crate::config::OpenAiConfig;
use crate::wire::{
    ChatRequest, WireFunctionCall, WireFunctionSpec, WireMessage, WireTool, WireToolCall,
};

/// Render a [`rivet_core::id::ToolCallId`] the way the wire sees it.
///
/// `simple` (hyphenless) rather than `Display`: the 39-char `tc_<hyphenated>` form sits
/// right on the 40-character id limit some endpoints impose. Nothing parses this back —
/// ids arriving in a response are always freshly minted (see [`crate::ids`]).
#[must_use]
pub fn wire_call_id(call: &ToolCall) -> String {
    format!("tc_{}", call.id.as_uuid().simple())
}

/// The model name to put on the wire: everything after the first `/`.
///
/// `openrouter/meta-llama/llama-3-70b` -> `meta-llama/llama-3-70b`, because the first
/// segment routes to *this adapter* and is not part of the provider's own namespace.
#[must_use]
pub fn wire_model(model: &rivet_core::model::ModelId) -> &str {
    model
        .as_str()
        .split_once('/')
        .map_or(model.as_str(), |(_, rest)| rest)
}

/// Encode a request, returning the JSON body to POST.
///
/// Images are dropped: Phase 1 has no vision tool and no CLI path that produces one, and
/// the multi-part content form differs enough between endpoints to be its own risk.
pub fn encode_request(request: &ModelRequest, config: &OpenAiConfig) -> rivet_core::Result<Value> {
    let mut messages = Vec::new();

    if let Some(system) = &request.system {
        messages.push(WireMessage {
            role: "system",
            content: Some(system.clone()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }

    for message in &request.messages {
        encode_message(message, config, &mut messages);
    }

    let tools: Vec<WireTool> = request
        .tools
        .iter()
        .map(|spec| WireTool {
            kind: "function",
            function: WireFunctionSpec {
                name: spec.name.clone(),
                description: spec.description.clone(),
                parameters: spec.input_schema.clone(),
            },
        })
        .collect();

    // A `tool_choice` with no tools is rejected by several endpoints, and means nothing.
    let tool_choice = if tools.is_empty() {
        None
    } else {
        Some(encode_tool_choice(&request.tool_choice))
    };

    let wire = ChatRequest {
        model: wire_model(&request.model).to_string(),
        messages,
        tools,
        tool_choice,
        stream: true,
        stream_options: config
            .include_usage
            .then(|| serde_json::json!({ "include_usage": true })),
        temperature: request.params.temperature,
        top_p: request.params.top_p,
        max_tokens: request.params.max_output_tokens,
        stop: request.params.stop_sequences.clone(),
    };

    let mut body = serde_json::to_value(&wire)?;
    // `ModelParams.extra` is contractually "pass through what you do not understand".
    if let Some(object) = body.as_object_mut() {
        merge_extra(object, &request.params.extra);
    }
    Ok(body)
}

fn merge_extra(target: &mut Map<String, Value>, extra: &Map<String, Value>) {
    for (key, value) in extra {
        target.insert(key.clone(), value.clone());
    }
}

fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => Value::String("auto".into()),
        ToolChoice::None => Value::String("none".into()),
        ToolChoice::Required => Value::String("required".into()),
        ToolChoice::Named(name) => {
            serde_json::json!({ "type": "function", "function": { "name": name } })
        }
    }
}

fn encode_message(message: &Message, config: &OpenAiConfig, out: &mut Vec<WireMessage>) {
    match message.role {
        Role::Tool => {
            // One wire message per result: the schema allows exactly one `tool_call_id`.
            for block in &message.content {
                if let ContentBlock::ToolResult {
                    call_id, content, ..
                } = block
                {
                    out.push(WireMessage {
                        role: "tool",
                        content: Some(content.clone()),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(format!("tc_{}", call_id.as_uuid().simple())),
                    });
                }
            }
        }
        Role::System | Role::User | Role::Assistant => {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                _ => "assistant",
            };
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::Reasoning { text: t, signature } => {
                        // Most compatible endpoints reject an unknown `reasoning_content`
                        // on the way in, so replaying it is opt-in.
                        if config.send_reasoning && signature.is_some() {
                            text.push_str(t);
                        }
                    }
                    ContentBlock::ToolCall(call) => tool_calls.push(WireToolCall {
                        id: wire_call_id(call),
                        kind: "function",
                        function: WireFunctionCall {
                            name: call.name.clone(),
                            arguments: call.input.to_string(),
                        },
                    }),
                    ContentBlock::ToolResult { .. } | ContentBlock::Image { .. } => {}
                }
            }
            // An assistant message that is nothing but tool calls has no content, and
            // sending `""` upsets some endpoints.
            let content = if text.is_empty() && !tool_calls.is_empty() {
                None
            } else {
                Some(text)
            };
            out.push(WireMessage {
                role,
                content,
                tool_calls,
                tool_call_id: None,
            });
        }
    }
}
