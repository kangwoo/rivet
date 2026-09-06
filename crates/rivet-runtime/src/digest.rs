//! A stable fingerprint for an assembled request.
//!
//! Recorded in `model.requested` before the request goes out, so an audit can tell two
//! runs apart and a cache analysis can tell when the prompt actually changed. It goes in
//! a durable log, which is why it is `SHA-256` rather than
//! `std::collections::hash_map::DefaultHasher`: the standard library makes no promise
//! that its hash is stable across builds, and a digest that changes when you upgrade the
//! compiler answers no question at all.

use rivet_core::model::ModelRequest;
use sha2::{Digest, Sha256};

/// Hash a request into `sha256:<hex>`.
///
/// Determinism comes from `serde_json`: struct fields serialize in declaration order and
/// `serde_json::Map` is a `BTreeMap`, so the same request always produces the same bytes.
///
/// # Errors
/// Only if the request cannot be serialized, which would mean a non-string map key.
pub fn request_digest(request: &ModelRequest) -> rivet_core::Result<String> {
    let bytes = serde_json::to_vec(request)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::model::{Message, ModelId, ModelParams, ToolChoice};

    fn request(text: &str) -> ModelRequest {
        ModelRequest {
            model: ModelId::new("openai/gpt-4o").unwrap(),
            system: Some("be brief".into()),
            messages: vec![Message::user(text)],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            params: ModelParams::default(),
        }
    }

    #[test]
    fn the_same_request_always_hashes_the_same() {
        assert_eq!(
            request_digest(&request("hi")).unwrap(),
            request_digest(&request("hi")).unwrap()
        );
    }

    #[test]
    fn a_changed_prompt_changes_the_digest() {
        assert_ne!(
            request_digest(&request("hi")).unwrap(),
            request_digest(&request("hello")).unwrap()
        );
    }

    #[test]
    fn the_digest_names_its_algorithm() {
        let digest = request_digest(&request("hi")).unwrap();
        assert!(digest.starts_with("sha256:"), "{digest}");
        assert_eq!(digest.len(), "sha256:".len() + 64);
    }

    #[test]
    fn extra_params_are_part_of_the_fingerprint() {
        let mut with_extra = request("hi");
        with_extra
            .params
            .extra
            .insert("seed".into(), serde_json::json!(7));
        assert_ne!(
            request_digest(&request("hi")).unwrap(),
            request_digest(&with_extra).unwrap()
        );
    }
}
