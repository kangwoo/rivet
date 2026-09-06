//! Memory and evaluation. Both are plugins; neither is on the MVP critical path.
//!
//! They live in `rivet-core` only because their *contracts* must be stable before anyone
//! builds against them. No default implementation ships in core.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::RunSummary;
use crate::id::SessionId;
use crate::time::Timestamp;

/// A retrievable fact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub content: String,
    /// Where it came from, so a stale memory can be invalidated with its source.
    pub source: MemorySource,
    pub created_at: Timestamp,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Relevance, populated by `recall`, not by `remember`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MemorySource {
    /// Written by a human, e.g. a `CLAUDE.md`-style project file.
    Authored { path: String },
    /// Derived from a session.
    Session { session_id: SessionId },
    /// Produced by a tool or an external system.
    External { origin: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub text: String,
    pub limit: usize,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A memory backend.
#[async_trait]
pub trait Memory: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    async fn recall(&self, query: &MemoryQuery) -> crate::Result<Vec<MemoryItem>>;

    async fn remember(&self, item: MemoryItem) -> crate::Result<()>;

    async fn forget(&self, id: &str) -> crate::Result<()>;
}

/// A score for a finished run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evaluation {
    pub evaluator: String,
    /// 0.0 to 1.0.
    pub score: f32,
    pub passed: bool,
    pub notes: String,
    #[serde(default)]
    pub metrics: serde_json::Map<String, serde_json::Value>,
}

/// Scores a completed run. Used by the review gate and by offline benchmarking.
#[async_trait]
pub trait Evaluator: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    async fn evaluate(&self, run: &RunSummary) -> crate::Result<Evaluation>;
}
