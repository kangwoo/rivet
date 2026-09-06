//! Adapter configuration, read from the `[plugins."rivet.model-openai"]` table.

use serde::{Deserialize, Serialize};

/// Settings for one `OpenAI`-compatible endpoint.
///
/// A flag struct: each `supports_*` field is an independent fact about the endpoint, so
/// the `struct_excessive_bools` heuristic (which suggests an enum) does not apply.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(clippy::struct_excessive_bools)]
#[serde(default)]
pub struct OpenAiConfig {
    /// Endpoint root, without the trailing `/chat/completions`.
    pub base_url: String,
    /// Name of the environment variable holding the key. The key itself never appears in
    /// a config file, and never in an error.
    pub api_key_env: String,
    /// Whether to ask for a usage block on the final chunk. Off for endpoints that reject
    /// unknown request fields.
    pub include_usage: bool,
    /// Whether to replay signed reasoning blocks back to the provider.
    pub send_reasoning: bool,
    /// Whole-request timeout, including the streaming body. An adapter that can hang
    /// forever makes `max_duration_ms` a suggestion.
    pub request_timeout_ms: u64,
    /// Connect timeout.
    pub connect_timeout_ms: u64,
    /// Advertised context window, used for budgeting when the caller does not override.
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub supports_prompt_caching: bool,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            include_usage: true,
            send_reasoning: false,
            request_timeout_ms: 600_000,
            connect_timeout_ms: 15_000,
            context_window: None,
            max_output_tokens: None,
            supports_tools: true,
            supports_vision: false,
            supports_reasoning: false,
            supports_prompt_caching: false,
        }
    }
}

impl OpenAiConfig {
    /// The chat-completions URL for this endpoint.
    #[must_use]
    pub fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slashes_do_not_double_up() {
        let config = OpenAiConfig {
            base_url: "http://localhost:11434/v1/".into(),
            ..OpenAiConfig::default()
        };
        assert_eq!(
            config.completions_url(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn config_round_trips_through_json() {
        let json = serde_json::json!({ "base_url": "http://x/v1", "include_usage": false });
        let config: OpenAiConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.base_url, "http://x/v1");
        assert!(!config.include_usage);
        assert_eq!(
            config.api_key_env, "OPENAI_API_KEY",
            "unspecified fields keep their defaults"
        );
    }
}
