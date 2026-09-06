//! The one test that needs the network. Excluded from a default run, twice over.
//!
//! `#[ignore]` keeps it out of `cargo test`, and the `RIVET_LIVE` guard keeps it out of
//! `cargo test -- --ignored` on a machine without a key. A cargo *feature* would not
//! work: CI runs `--all-features`, which would switch the network back on.
//!
//! ```bash
//! export DEEPSEEK_API_KEY=...
//! RIVET_LIVE=1 cargo test -p rivet-model-openai --test live -- --ignored
//! ```

use std::sync::Arc;

use futures_util::StreamExt;
use rivet_core::model::{
    Message, Model, ModelId, ModelParams, ModelRequest, StreamEvent, ToolChoice,
};
use rivet_model_openai::{OpenAiConfig, OpenAiModel, Uuidv7Ids};

/// Whether this run was explicitly asked to touch the network.
fn enabled() -> bool {
    std::env::var("RIVET_LIVE").is_ok_and(|v| v != "0")
}

#[tokio::test]
#[ignore = "requires a real provider; run with RIVET_LIVE=1 and an API key"]
async fn live_round_trip() {
    if !enabled() {
        eprintln!("skipped: set RIVET_LIVE=1 to run against a real provider");
        return;
    }

    let config = OpenAiConfig {
        base_url: std::env::var("RIVET_LIVE_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".into()),
        api_key_env: std::env::var("RIVET_LIVE_KEY_ENV")
            .unwrap_or_else(|_| "DEEPSEEK_API_KEY".into()),
        ..OpenAiConfig::default()
    };
    let Ok(api_key) = std::env::var(&config.api_key_env) else {
        panic!(
            "RIVET_LIVE is set but `{}` is not; the test cannot run",
            config.api_key_env
        );
    };
    let model_id =
        std::env::var("RIVET_LIVE_MODEL").unwrap_or_else(|_| "deepseek/deepseek-chat".into());

    let model = OpenAiModel::with_ids(
        ModelId::new(model_id).unwrap(),
        config,
        api_key,
        Arc::new(Uuidv7Ids),
    )
    .expect("adapter");

    let request = ModelRequest {
        model: model.id().clone(),
        system: Some("Answer in one short sentence.".into()),
        messages: vec![Message::user("Say hello.")],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        params: ModelParams {
            max_output_tokens: Some(64),
            ..ModelParams::default()
        },
    };

    let mut stream = model.stream(request).await.expect("stream");
    let mut done = 0;
    while let Some(event) = stream.next().await {
        if let StreamEvent::Done { message, usage, .. } = event.expect("no stream error") {
            done += 1;
            assert!(!message.text().is_empty(), "the model said nothing");
            assert!(
                usage.input_tokens > 0,
                "usage must be populated or estimated"
            );
        }
    }
    assert_eq!(done, 1, "exactly one Done closes the turn");
}
