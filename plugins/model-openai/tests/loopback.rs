//! The real `reqwest` path, offline.
//!
//! Fixture tests pin the decoder; these pin everything between the decoder and the
//! socket. They also make the cancellation contract -- "dropping the returned stream has
//! to abort the request" -- observable from the *server* side, which is the only place it
//! can honestly be checked.

mod support;

use std::sync::Arc;

use futures_util::StreamExt;
use rivet_core::model::{
    Message, Model, ModelId, ModelParams, ModelRequest, StreamEvent, ToolChoice,
};
use rivet_model_openai::{OpenAiConfig, OpenAiModel, SequentialIds};
use support::loopback::{LoopbackServer, Reply};

fn model(base_url: &str) -> OpenAiModel {
    let config = OpenAiConfig {
        base_url: base_url.to_string(),
        request_timeout_ms: 10_000,
        connect_timeout_ms: 2_000,
        ..OpenAiConfig::default()
    };
    OpenAiModel::with_ids(
        ModelId::new("loopback/test-model").unwrap(),
        config,
        "test-key".into(),
        Arc::new(SequentialIds::new()),
    )
    .expect("adapter")
}

fn request() -> ModelRequest {
    ModelRequest {
        model: ModelId::new("loopback/test-model").unwrap(),
        system: Some("be brief".into()),
        messages: vec![Message::user("what is here?")],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        params: ModelParams::default(),
    }
}

#[tokio::test]
async fn a_real_http_round_trip_decodes_a_tool_call() {
    let server =
        LoopbackServer::start(vec![Reply::Sse(support::fixture("openai_tool_call.sse"))]).await;
    let model = model(&server.base_url);

    let mut stream = model.stream(request()).await.expect("stream");
    let mut done = None;
    while let Some(event) = stream.next().await {
        if let StreamEvent::Done { message, .. } = event.expect("no stream error") {
            done = Some(message);
        }
    }

    let message = done.expect("exactly one Done");
    assert_eq!(message.tool_calls()[0].name, "read_file");

    let sent = server.requests().await;
    assert_eq!(sent.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&sent[0]).expect("we sent JSON");
    assert_eq!(
        body["model"], "test-model",
        "the provider segment is stripped"
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["content"], "be brief");
}

#[tokio::test]
async fn dropping_the_stream_disconnects_the_request() {
    // The Model contract requires this, and the 5-second cancellation budget depends on
    // it: a stream that keeps reading after cancellation holds the run open.
    let frames: Vec<String> = (0..200)
        .map(|i| {
            format!(
                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"chunk {i} \"}}}}]}}\n\n"
            )
        })
        .collect();
    let server = LoopbackServer::start(vec![Reply::Dripping {
        frames,
        delay_ms: 5,
    }])
    .await;
    let model = model(&server.base_url);

    let mut stream = model.stream(request()).await.expect("stream");
    // Read a little, then walk away.
    for _ in 0..2 {
        let _ = stream.next().await;
    }
    drop(stream);

    // The server notices within its own write cadence; give it a bounded window.
    for _ in 0..200 {
        if server.client_hung_up() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        server.client_hung_up(),
        "the server must observe the disconnect; otherwise the request is still in flight"
    );
}

#[tokio::test]
async fn a_rate_limit_carries_the_servers_own_backoff() {
    let server = LoopbackServer::start(vec![Reply::Status {
        code: 429,
        headers: vec![("Retry-After".into(), "12".into())],
        body: r#"{"error":{"message":"slow down","type":"rate_limit_exceeded"}}"#.into(),
    }])
    .await;

    let Err(err) = model(&server.base_url).stream(request()).await else {
        panic!("a 429 must not produce a stream")
    };
    assert_eq!(
        err.kind(),
        rivet_core::error::ErrorKind::RateLimited {
            retry_after_ms: Some(12_000)
        },
        "a server that says when to come back knows better than the local curve"
    );
}

#[tokio::test]
async fn a_bad_request_is_not_retried_and_says_why() {
    let server = LoopbackServer::start(vec![Reply::Status {
        code: 400,
        headers: vec![("x-request-id".into(), "req_42".into())],
        body: r#"{"error":{"message":"unknown parameter `reasoning_effort`","type":"invalid_request_error"}}"#
            .into(),
    }])
    .await;

    let Err(err) = model(&server.base_url).stream(request()).await else {
        panic!("a 400 must not produce a stream")
    };
    assert!(!err.is_retryable());
    assert!(err.message().contains("reasoning_effort"), "{err}");
    assert_eq!(err.details().unwrap()["request_id"], "req_42");
}

#[tokio::test]
async fn a_connection_refused_is_transient() {
    // Nothing is listening on this port; the adapter must classify it as retryable rather
    // than as a permanent upstream failure.
    let model = model("http://127.0.0.1:1/v1");
    let Err(err) = model.stream(request()).await else {
        panic!("nothing is listening; there is no stream to return")
    };
    assert!(err.is_retryable(), "{err}");
}
