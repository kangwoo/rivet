//! Request encoding: the half of the round trip a fixture cannot check.

use rivet_core::id::ToolCallId;
use rivet_core::model::{
    ContentBlock, Message, ModelId, ModelParams, ModelRequest, Role, ToolChoice,
};
use rivet_core::tool::{ToolCall, ToolSpec};
use rivet_model_openai::OpenAiConfig;
use rivet_model_openai::encode::{encode_request, wire_model};

fn request(messages: Vec<Message>) -> ModelRequest {
    ModelRequest {
        model: ModelId::new("deepseek/deepseek-chat").unwrap(),
        system: Some("you are careful".into()),
        messages,
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        params: ModelParams::default(),
    }
}

fn encoded(request: &ModelRequest) -> serde_json::Value {
    encode_request(request, &OpenAiConfig::default()).expect("encode")
}

#[test]
fn the_provider_segment_is_stripped_from_the_wire_model() {
    assert_eq!(
        wire_model(&ModelId::new("deepseek/deepseek-chat").unwrap()),
        "deepseek-chat"
    );
    assert_eq!(
        wire_model(&ModelId::new("openrouter/meta-llama/llama-3-70b").unwrap()),
        "meta-llama/llama-3-70b",
        "only the first segment routes to this adapter"
    );
}

#[test]
fn the_system_prompt_becomes_the_first_message() {
    let body = encoded(&request(vec![Message::user("hi")]));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "you are careful");
    assert_eq!(messages[1]["role"], "user");
}

#[test]
fn one_tool_message_is_emitted_per_result_block() {
    // The schema allows exactly one `tool_call_id` per message, so a Role::Tool message
    // holding two results has to fan out.
    let a = ToolCallId::new();
    let b = ToolCallId::new();
    let message = Message {
        role: Role::Tool,
        content: vec![
            ContentBlock::ToolResult {
                call_id: a,
                content: "first".into(),
                is_error: false,
            },
            ContentBlock::ToolResult {
                call_id: b,
                content: "second".into(),
                is_error: true,
            },
        ],
    };
    let body = encoded(&request(vec![message]));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "system + two tool messages");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[1]["content"], "first");
    assert_eq!(messages[2]["content"], "second");
    assert_ne!(messages[1]["tool_call_id"], messages[2]["tool_call_id"]);
}

#[test]
fn tool_call_ids_render_consistently_on_both_sides() {
    // The provider only ever sees ids we minted, and the id on the assistant message must
    // be the same string as the one on its answer -- otherwise every reply is a 400.
    let id = ToolCallId::new();
    let assistant = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall(ToolCall {
            id,
            name: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        })],
    };
    let result = Message {
        role: Role::Tool,
        content: vec![ContentBlock::ToolResult {
            call_id: id,
            content: "contents".into(),
            is_error: false,
        }],
    };
    let body = encoded(&request(vec![assistant, result]));
    let messages = body["messages"].as_array().unwrap();
    let called = messages[1]["tool_calls"][0]["id"].as_str().unwrap();
    let answered = messages[2]["tool_call_id"].as_str().unwrap();
    assert_eq!(called, answered);
    assert!(called.starts_with("tc_"), "{called}");
    assert_eq!(called.len(), 35, "hyphenless, to stay under a 40-char cap");
}

#[test]
fn tool_arguments_go_out_as_a_json_string() {
    let assistant = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolCall(ToolCall {
            id: ToolCallId::new(),
            name: "search".into(),
            input: serde_json::json!({ "query": "TODO" }),
        })],
    };
    let body = encoded(&request(vec![assistant]));
    let arguments = body["messages"][1]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("arguments must be a string, not an object");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
        serde_json::json!({ "query": "TODO" })
    );
    assert!(
        body["messages"][1].get("content").is_none(),
        "a tool-call-only assistant message sends no content"
    );
}

#[test]
fn tool_specs_become_function_declarations() {
    let mut req = request(vec![Message::user("go")]);
    req.tools = vec![
        ToolSpec::new(
            "read_file",
            "read a file",
            serde_json::json!({ "type": "object" }),
        )
        .unwrap(),
    ];
    req.tool_choice = ToolChoice::Named("read_file".into());
    let body = encoded(&req);
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["tool_choice"]["function"]["name"], "read_file");
}

#[test]
fn tool_choice_is_omitted_when_there_are_no_tools() {
    // Several endpoints reject a tool_choice with an empty tools array, and it means
    // nothing anyway.
    let mut req = request(vec![Message::user("go")]);
    req.tool_choice = ToolChoice::Required;
    let body = encoded(&req);
    assert!(body.get("tool_choice").is_none(), "{body}");
    assert!(body.get("tools").is_none());
}

#[test]
fn unknown_params_pass_through_untouched() {
    // The contract says an adapter ignores extras rather than erroring, so one config can
    // target several providers.
    let mut req = request(vec![Message::user("go")]);
    req.params.extra.insert(
        "reasoning_effort".into(),
        serde_json::Value::String("high".into()),
    );
    req.params.temperature = Some(0.2);
    let body = encoded(&req);
    assert_eq!(body["reasoning_effort"], "high");
    assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
}

#[test]
fn stream_options_can_be_turned_off() {
    let config = OpenAiConfig {
        include_usage: false,
        ..OpenAiConfig::default()
    };
    let body = encode_request(&request(vec![Message::user("hi")]), &config).unwrap();
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["stream"], true);
}

#[test]
fn reasoning_blocks_are_not_replayed_by_default() {
    let assistant = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Reasoning {
                text: "internal".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::text("visible"),
        ],
    };
    let body = encoded(&request(vec![assistant]));
    assert_eq!(body["messages"][1]["content"], "visible");
}
