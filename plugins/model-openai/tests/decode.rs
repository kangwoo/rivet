//! Fixture-driven decoding: every recorded dialect, decoded offline.
//!
//! These are the tests the plan asked for when it called 1.5 the riskiest item. Each
//! fixture pins one way a compatible endpoint differs from the reference one.

mod support;

use rivet_core::model::{ContentBlock, StopReason, StreamEvent};
use support::{decode, done};

fn assembled(
    events: &[rivet_core::Result<StreamEvent>],
) -> (
    &rivet_core::model::Message,
    StopReason,
    rivet_core::model::Usage,
) {
    match done(events) {
        StreamEvent::Done {
            message,
            stop_reason,
            usage,
        } => (message, *stop_reason, *usage),
        other => panic!("not a Done: {other:?}"),
    }
}

#[tokio::test]
async fn plain_text_assembles_into_one_message() {
    let events = decode("openai_text_only.sse").await;
    let (message, stop, _) = assembled(&events);
    assert_eq!(message.text(), "This project is a Rust agent runtime.");
    assert_eq!(stop, StopReason::EndTurn);
}

#[tokio::test]
async fn every_stream_ends_with_exactly_one_done() {
    // The runtime relies on this to close a turn; two Dones or none is a wedged loop.
    for name in [
        "openai_text_only.sse",
        "openai_tool_call.sse",
        "openai_two_tool_calls.sse",
        "openai_usage_in_last_chunk.sse",
        "deepseek_reasoning.sse",
        "ollama_single_chunk.sse",
        "truncated_args.sse",
    ] {
        let events = decode(name).await;
        let dones = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::Done { .. })))
            .count();
        assert_eq!(dones, 1, "{name} produced {dones} Done events");
        assert!(
            matches!(events.last(), Some(Ok(StreamEvent::Done { .. }))),
            "{name}: Done must be last"
        );
    }
}

#[tokio::test]
async fn split_tool_arguments_reassemble_into_one_object() {
    let events = decode("openai_tool_call.sse").await;
    let (message, stop, _) = assembled(&events);
    assert_eq!(stop, StopReason::ToolUse);
    let calls = message.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].input, serde_json::json!({ "path": "Cargo.toml" }));

    // Six fragments arrived, and every one was reported as a delta.
    let deltas = events
        .iter()
        .filter(|e| matches!(e, Ok(StreamEvent::ToolCallDelta { .. })))
        .count();
    assert_eq!(deltas, 5, "one empty opening fragment is not a delta");
}

#[tokio::test]
async fn provider_ids_are_replaced_by_our_own() {
    let events = decode("openai_tool_call.sse").await;
    let (message, _, _) = assembled(&events);
    let id = message.tool_calls()[0].id.to_string();
    assert!(
        !id.contains("call_abc123"),
        "the provider id must not survive: {id}"
    );
    assert!(id.starts_with("tc_"), "{id}");
}

#[tokio::test]
async fn interleaved_parallel_calls_do_not_merge() {
    // Accumulating on anything but `index` welds the two argument strings together.
    let events = decode("openai_two_tool_calls.sse").await;
    let (message, stop, _) = assembled(&events);
    assert_eq!(stop, StopReason::ToolUse);
    let calls = message.tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].input, serde_json::json!({ "path": "README.md" }));
    assert_eq!(calls[1].name, "list_dir");
    assert_eq!(calls[1].input, serde_json::json!({ "path": "src" }));
    assert_ne!(calls[0].id, calls[1].id);
}

#[tokio::test]
async fn usage_arrives_on_a_chunk_with_no_choices() {
    let events = decode("openai_usage_in_last_chunk.sse").await;
    let (_, _, usage) = assembled(&events);
    assert_eq!(usage.input_tokens, 1200);
    assert_eq!(usage.output_tokens, 37);
    assert_eq!(usage.cache_read_tokens, 1024);
}

#[tokio::test]
async fn reasoning_is_kept_separate_from_the_answer() {
    let events = decode("deepseek_reasoning.sse").await;
    let (message, stop, _) = assembled(&events);
    assert_eq!(
        message.text(),
        "Let me look.",
        "reasoning is not the answer"
    );
    let reasoning: Vec<_> = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning.len(), 1);
    assert!(reasoning[0].contains("list_dir is the right tool"));
    assert_eq!(
        stop,
        StopReason::ToolUse,
        "the wire said `stop`, but the message carries a tool call"
    );
}

#[tokio::test]
async fn a_dialect_without_an_index_still_yields_a_call() {
    let events = decode("ollama_single_chunk.sse").await;
    let (message, stop, usage) = assembled(&events);
    assert_eq!(stop, StopReason::ToolUse);
    let calls = message.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "search");
    assert_eq!(calls[0].input, serde_json::json!({ "query": "TODO" }));
    assert_eq!(
        usage.input_tokens, 100,
        "no usage on the wire: the adapter's estimate stands in"
    );
    assert!(
        usage.output_tokens > 0,
        "a zero would make max_total_tokens a limit that never trips"
    );
}

#[tokio::test]
async fn truncated_arguments_are_handed_to_the_model_verbatim() {
    let events = decode("truncated_args.sse").await;
    let (message, stop, _) = assembled(&events);
    assert_eq!(stop, StopReason::ToolUse, "there is still a call to answer");
    let call = message.tool_calls()[0];
    assert_eq!(
        call.input,
        serde_json::Value::String("{\"path\": \"src/li".into()),
        "guessing an object here would run the wrong write_file"
    );
    assert!(
        !call.input.is_object(),
        "schema validation must reject it so the model can retry"
    );
}

#[tokio::test]
async fn a_mid_stream_error_ends_the_stream_with_a_classified_failure() {
    let events = decode("error_mid_stream.sse").await;
    let last = events.last().expect("at least one item");
    let err = last.as_ref().unwrap_err();
    assert!(err.is_retryable(), "server_error is transient: {err}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::Done { .. }))),
        "a failed stream must not also report a completed message"
    );
}

#[tokio::test]
async fn a_stream_that_stops_early_is_reported_as_transient() {
    let events = decode("no_done_sentinel.sse").await;
    let err = events.last().unwrap().as_ref().unwrap_err();
    assert!(err.is_retryable(), "{err}");
    assert!(
        err.message().contains("incomplete"),
        "the operator must be told the answer was cut off: {err}"
    );
}

#[tokio::test]
async fn decoding_is_deterministic() {
    // Same bytes in, byte-identical events out -- the property the injected id factory
    // exists to provide, and the reason these fixtures can be asserted whole.
    let first = decode("openai_two_tool_calls.sse").await;
    let second = decode("openai_two_tool_calls.sse").await;
    let render = |events: &[rivet_core::Result<StreamEvent>]| {
        events
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    pretty_assertions::assert_eq!(render(&first), render(&second));
}
