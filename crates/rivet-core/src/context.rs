//! Context assembly.
//!
//! The naive design — "concatenate everything providers return" — fails the moment the
//! result exceeds the window. So the contract is budget-aware from the start: every item
//! declares a [`ContextSlot`] and a [`Priority`], and the assembler drops from the bottom
//! rather than truncating mid-item.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, JobId, RunId, SessionId};
use crate::model::Message;
use crate::workspace::Workspace;

/// Where an item lands in the assembled request. Ordering is the declaration order below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSlot {
    /// Identity and rules. Always first, never dropped.
    SystemPrompt,
    /// Repository shape, language, build commands.
    Environment,
    /// The active job and its acceptance criteria.
    Job,
    /// Recalled memory.
    Memory,
    /// Skills or playbooks selected for this run.
    Skills,
    /// Live runtime state: available tools, current git status.
    RuntimeState,
    /// Conversation history. Managed by the session, not by providers.
    History,
}

/// What survives when the budget is tight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Drop this before anything else.
    Optional,
    Normal,
    /// Drop only if the request would otherwise be impossible.
    Important,
    /// Never dropped. If required items alone exceed the budget, assembly fails loudly
    /// instead of silently producing a broken request.
    Required,
}

/// One contribution to the model request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub slot: ContextSlot,
    pub priority: Priority,
    /// Stable key. A provider re-running must reuse the key so the assembler can dedupe
    /// and so prompt caching stays effective.
    pub key: String,
    pub content: String,
    /// Provider's own token estimate. The assembler falls back to a byte heuristic when
    /// absent.
    pub estimated_tokens: Option<u32>,
}

impl ContextItem {
    pub fn new(slot: ContextSlot, key: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            slot,
            priority: Priority::Normal,
            key: key.into(),
            content: content.into(),
            estimated_tokens: None,
        }
    }

    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Token estimate, falling back to [`estimate_tokens`].
    #[must_use]
    pub fn tokens(&self) -> u32 {
        self.estimated_tokens
            .unwrap_or_else(|| estimate_tokens(&self.content))
    }
}

/// What a provider is told about the run it is contributing to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextRequest {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub job_id: Option<JobId>,
    pub workspace: Workspace,
    /// Turn number within the run. Providers use this to skip expensive work after the
    /// first turn (a repo tree rarely changes between turns).
    pub turn: u32,
    /// Tokens still available. Advisory: a provider that ignores it gets truncated.
    pub budget_tokens: u32,
}

/// A source of context.
///
/// Providers must be **fast and side-effect free**. They run on every turn, on the
/// critical path, before the model call.
#[async_trait]
pub trait ContextProvider: Send + Sync + fmt::Debug {
    fn name(&self) -> &str;

    async fn provide(&self, request: &ContextRequest) -> crate::Result<Vec<ContextItem>>;
}

/// A crude, deliberately conservative token estimate.
///
/// The usual `bytes / 4` rule is calibrated on English and is badly wrong elsewhere: a
/// Korean or Japanese character is 3 bytes of UTF-8 but often a token or two by itself,
/// so `bytes / 4` can *under*count CJK text by 2-4x. Undercounting is the dangerous
/// direction — it produces requests that overflow the window after the budget said they
/// fit.
///
/// So the estimate works in characters, not bytes, and charges non-ASCII characters more.
/// It is still only an estimate: a provider that exposes a real tokenizer should override
/// [`crate::model::Model::count_tokens`], and every caller must treat the result as a
/// lower bound on accuracy rather than a guarantee.
#[must_use]
pub fn estimate_tokens(text: &str) -> u32 {
    let mut ascii = 0usize;
    let mut wide = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    // ~4 ASCII chars per token; ~1.5 tokens per CJK/other character.
    let estimate = ascii / 4 + wide * 3 / 2;
    u32::try_from(estimate).unwrap_or(u32::MAX)
}

/// The result of assembling context into a request.
#[derive(Clone, Debug)]
pub struct AssembledContext {
    pub system: String,
    pub messages: Vec<Message>,
    /// Items dropped for budget, reported so a UI can warn and an evaluator can correlate
    /// quality with what got cut.
    pub dropped: Vec<DroppedItem>,
    pub estimated_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DroppedItem {
    pub key: String,
    pub slot: ContextSlot,
    pub tokens: u32,
}

/// Select items that fit the budget, dropping lowest priority first.
///
/// Ties break on slot order, then on the provider's declaration order, so assembly is
/// deterministic — the same inputs always produce the same prompt, which is what makes
/// prompt caching and replay meaningful.
///
/// # Errors
/// Returns [`crate::error::ErrorKind::InvalidArgument`] when [`Priority::Required`] items
/// alone exceed the budget. Silently shipping a broken prompt is worse than failing.
/// Select items that fit the budget, dropping lowest priority first.
///
/// Two properties this must have, both of which a naive greedy fill gets wrong:
///
/// 1. **No priority inversion.** Once an item cannot fit, nothing of *lower* priority is
///    admitted either. A plain greedy pass would skip a 400-token `Important` item and
///    then happily admit a 40-token `Optional` one, which is the opposite of what
///    "priority" means. Within one priority band, ties break on slot order then
///    declaration order.
/// 2. **Deduplication by key.** Providers are re-run every turn and may legitimately
///    return the same item twice (two providers both contributing `git.status`). The
///    first occurrence wins, so the result is stable across turns and prompt caching
///    keeps working.
///
/// # Errors
/// Returns [`crate::error::ErrorKind::InvalidArgument`] when [`Priority::Required`] items
/// alone exceed the budget. Silently shipping a broken prompt is worse than failing.
pub fn fit_to_budget(
    items: Vec<ContextItem>,
    budget_tokens: u32,
) -> crate::Result<(Vec<ContextItem>, Vec<DroppedItem>)> {
    // Dedupe by key. The *highest priority* occurrence wins, not the first: a stray
    // Optional item sharing a key with the system prompt must never be the one that
    // survives. Position is preserved from the winning entry so ordering stays stable.
    let mut best: BTreeMap<String, (usize, ContextItem)> = BTreeMap::new();
    let mut dropped: Vec<DroppedItem> = Vec::new();
    for (idx, item) in items.into_iter().enumerate() {
        match best.get(&item.key) {
            Some((_, existing)) if existing.priority >= item.priority => {
                dropped.push(DroppedItem {
                    key: item.key,
                    slot: item.slot,
                    tokens: item.estimated_tokens.unwrap_or_default(),
                });
            }
            Some((_, existing)) => {
                dropped.push(DroppedItem {
                    key: existing.key.clone(),
                    slot: existing.slot,
                    tokens: existing.estimated_tokens.unwrap_or_default(),
                });
                best.insert(item.key.clone(), (idx, item));
            }
            None => {
                best.insert(item.key.clone(), (idx, item));
            }
        }
    }
    let mut deduped: Vec<(usize, ContextItem)> = best.into_values().collect();
    deduped.sort_by_key(|(idx, _)| *idx);

    let required_total: u32 = deduped
        .iter()
        .filter(|(_, i)| i.priority == Priority::Required)
        .map(|(_, i)| i.tokens())
        .sum();

    if required_total > budget_tokens {
        return Err(crate::Error::invalid_argument(format!(
            "required context is {required_total} tokens but the budget is {budget_tokens}; \
             raise max_context_tokens or shorten the system prompt"
        )));
    }

    // Highest priority first; within a band, slot order then declaration order.
    deduped.sort_by(|(ia, a), (ib, b)| {
        b.priority
            .cmp(&a.priority)
            .then(a.slot.cmp(&b.slot))
            .then(ia.cmp(ib))
    });

    let mut used = 0u32;
    let mut kept: Vec<(usize, ContextItem)> = Vec::new();
    // Once something fails to fit, every later (lower-or-equal priority) item is dropped
    // too. This is what prevents a small Optional item from displacing a large Important
    // one.
    let mut admitting = true;

    for (idx, item) in deduped {
        let cost = item.tokens();
        if admitting && used.saturating_add(cost) <= budget_tokens {
            used += cost;
            kept.push((idx, item));
        } else {
            if item.priority == Priority::Required {
                // Cannot happen: required_total was checked above.
                return Err(crate::Error::internal(
                    "a required context item was dropped despite fitting the budget check",
                ));
            }
            admitting = false;
            dropped.push(DroppedItem {
                key: item.key,
                slot: item.slot,
                tokens: cost,
            });
        }
    }

    // Restore slot order for the surviving items.
    kept.sort_by(|(ia, a), (ib, b)| a.slot.cmp(&b.slot).then(ia.cmp(ib)));
    Ok((kept.into_iter().map(|(_, item)| item).collect(), dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(slot: ContextSlot, key: &str, tokens: u32, priority: Priority) -> ContextItem {
        ContextItem {
            slot,
            priority,
            key: key.into(),
            content: String::new(),
            estimated_tokens: Some(tokens),
        }
    }

    #[test]
    fn everything_fits_when_the_budget_is_large() {
        let items = vec![
            item(ContextSlot::SystemPrompt, "sys", 100, Priority::Required),
            item(ContextSlot::Memory, "mem", 50, Priority::Optional),
        ];
        let (kept, dropped) = fit_to_budget(items, 1000).unwrap();
        assert_eq!(kept.len(), 2);
        assert!(dropped.is_empty());
    }

    #[test]
    fn lowest_priority_is_dropped_first() {
        let items = vec![
            item(ContextSlot::SystemPrompt, "sys", 100, Priority::Required),
            item(ContextSlot::Memory, "mem", 100, Priority::Optional),
            item(ContextSlot::Job, "job", 100, Priority::Important),
        ];
        let (kept, dropped) = fit_to_budget(items, 250).unwrap();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].key, "mem");
        assert_eq!(
            kept.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["sys", "job"]
        );
    }

    #[test]
    fn surviving_items_come_back_in_slot_order() {
        let items = vec![
            item(ContextSlot::RuntimeState, "state", 10, Priority::Normal),
            item(ContextSlot::SystemPrompt, "sys", 10, Priority::Required),
            item(ContextSlot::Job, "job", 10, Priority::Normal),
        ];
        let (kept, _) = fit_to_budget(items, 100).unwrap();
        assert_eq!(
            kept.iter().map(|i| i.slot).collect::<Vec<_>>(),
            [
                ContextSlot::SystemPrompt,
                ContextSlot::Job,
                ContextSlot::RuntimeState
            ]
        );
    }

    #[test]
    fn assembly_fails_loudly_when_required_items_alone_overflow() {
        let items = vec![item(
            ContextSlot::SystemPrompt,
            "sys",
            5000,
            Priority::Required,
        )];
        let err = fit_to_budget(items, 1000).unwrap_err();
        assert!(err.message().contains("required context"), "{err}");
    }

    #[test]
    fn priority_is_never_inverted_by_a_small_low_priority_item() {
        // A greedy fill would skip the 400-token Important item, then admit the 40-token
        // Optional one. That is exactly backwards.
        let items = vec![
            item(ContextSlot::SystemPrompt, "sys", 100, Priority::Required),
            item(ContextSlot::Job, "job", 400, Priority::Important),
            item(ContextSlot::Memory, "mem", 40, Priority::Optional),
        ];
        let (kept, dropped) = fit_to_budget(items, 200).unwrap();
        assert_eq!(
            kept.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["sys"]
        );
        let dropped_keys: Vec<_> = dropped.iter().map(|d| d.key.as_str()).collect();
        assert!(dropped_keys.contains(&"job"));
        assert!(
            dropped_keys.contains(&"mem"),
            "nothing of lower priority may be admitted once something was dropped"
        );
    }

    #[test]
    fn a_duplicate_key_never_evicts_a_higher_priority_item() {
        // A stray Optional item sharing a key with the system prompt must not be the one
        // that survives -- that would silently delete the safety instructions.
        let items = vec![
            item(ContextSlot::Memory, "sys", 10, Priority::Optional),
            item(ContextSlot::SystemPrompt, "sys", 10, Priority::Required),
        ];
        let (kept, _) = fit_to_budget(items, 1000).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0].priority,
            Priority::Required,
            "the higher-priority occurrence must win regardless of declaration order"
        );
        assert_eq!(kept[0].slot, ContextSlot::SystemPrompt);
    }

    #[test]
    fn duplicate_keys_are_collapsed() {
        // Two providers both contributing `git.status` must not double-charge the budget
        // or produce a prompt that changes shape between turns.
        let items = vec![
            item(
                ContextSlot::RuntimeState,
                "git.status",
                50,
                Priority::Normal,
            ),
            item(ContextSlot::Environment, "git.status", 50, Priority::Normal),
        ];
        let (kept, dropped) = fit_to_budget(items, 1000).unwrap();
        assert_eq!(kept.len(), 1, "equal priority: the first occurrence wins");
        assert_eq!(kept[0].slot, ContextSlot::RuntimeState);
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn cjk_text_is_not_undercounted() {
        // `bytes / 4` would call this ~45 tokens; the real count is far higher.
        // Undercounting is the dangerous direction: it overflows the window after the
        // budget said the request fit.
        let korean = "로그인 API를 구현해줘. 테스트가 통과해야 한다.".repeat(4);
        let bytes_over_four = u32::try_from(korean.len() / 4).unwrap();
        let estimate = estimate_tokens(&korean);
        assert!(
            estimate > bytes_over_four,
            "estimate {estimate} must exceed the naive {bytes_over_four}"
        );
    }

    #[test]
    fn ascii_estimation_stays_in_the_usual_range() {
        let text = "a".repeat(400);
        assert_eq!(estimate_tokens(&text), 100);
    }

    #[test]
    fn selection_is_deterministic_across_runs() {
        let build = || {
            vec![
                item(ContextSlot::Memory, "a", 60, Priority::Normal),
                item(ContextSlot::Memory, "b", 60, Priority::Normal),
            ]
        };
        let (first, _) = fit_to_budget(build(), 100).unwrap();
        let (second, _) = fit_to_budget(build(), 100).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 1, "only one of the two fits");
        assert_eq!(first[0].key, "a", "ties break on declaration order");
    }
}
