//! Trimming conversation history without breaking it.
//!
//! The naive approach — drop the oldest message — produces an array where a `ToolResult`
//! survives its `ToolCall`, and every major provider answers that with a 400. It is the
//! same failure `SessionState::replay` guards against from the other direction.
//!
//! So history is dropped in **turn groups**: an assistant message that requested tools
//! plus every result answering it, kept or dropped together.

use rivet_core::model::{Message, Role};

/// An indivisible span of history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnGroup {
    /// First message index, inclusive.
    pub start: usize,
    /// Last message index, exclusive.
    pub end: usize,
    pub tokens: u32,
}

/// Split messages into groups that may be dropped whole, and only whole.
#[must_use]
pub fn group(messages: &[Message]) -> Vec<TurnGroup> {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let start = index;
        index += 1;
        if messages[start].role == Role::Assistant && !messages[start].tool_calls().is_empty() {
            // Every answer to this message belongs with it.
            while index < messages.len() && messages[index].role == Role::Tool {
                index += 1;
            }
        }
        groups.push(TurnGroup {
            start,
            end: index,
            tokens: messages[start..index]
                .iter()
                .map(Message::estimated_tokens)
                .fold(0u32, u32::saturating_add),
        });
    }
    groups
}

/// The tokens the newest group costs — the floor history cannot go below.
#[must_use]
pub fn floor_tokens(groups: &[TurnGroup]) -> u32 {
    groups.last().map_or(0, |g| g.tokens)
}

/// What a trim kept.
#[derive(Debug)]
pub struct Trimmed {
    pub messages: Vec<Message>,
    /// Groups dropped for budget, oldest first.
    pub dropped_groups: usize,
    pub tokens: u32,
}

/// Keep as much recent history as fits, plus everything pinned.
///
/// `pinned` is a count of leading **messages** that must never be dropped — the message a
/// checkpoint projects its summary into. Dropping it would hand the model total amnesia
/// about everything the checkpoint folded away.
///
/// Returns `None` when even the pinned prefix plus the newest group cannot fit; the caller
/// reports that as `LimitReached { ContextSize }` rather than shipping a broken prompt.
#[must_use]
pub fn trim(messages: &[Message], budget: u32, pinned: usize) -> Option<Trimmed> {
    let groups = group(messages);
    if groups.is_empty() {
        return Some(Trimmed {
            messages: Vec::new(),
            dropped_groups: 0,
            tokens: 0,
        });
    }

    let pinned_groups = groups.iter().filter(|g| g.end <= pinned).count();
    let mut keep = vec![false; groups.len()];
    let mut used = 0u32;

    for (index, g) in groups.iter().enumerate().take(pinned_groups) {
        keep[index] = true;
        used = used.saturating_add(g.tokens);
    }

    // The newest group is the floor: a run that cannot send its own last turn has nothing
    // to say.
    let last = groups.len() - 1;
    if !keep[last] {
        keep[last] = true;
        used = used.saturating_add(groups[last].tokens);
    }
    if used > budget {
        return None;
    }

    // Then walk backwards, oldest dropped first.
    for index in (pinned_groups..last).rev() {
        let cost = groups[index].tokens;
        if used.saturating_add(cost) > budget {
            break;
        }
        used += cost;
        keep[index] = true;
    }

    let mut kept_messages = Vec::new();
    let mut dropped_groups = 0;
    for (index, g) in groups.iter().enumerate() {
        if keep[index] {
            kept_messages.extend_from_slice(&messages[g.start..g.end]);
        } else {
            dropped_groups += 1;
        }
    }

    Some(Trimmed {
        messages: kept_messages,
        dropped_groups,
        tokens: used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::id::ToolCallId;
    use rivet_core::model::ContentBlock;
    use rivet_core::tool::ToolCall;

    fn call(id: ToolCallId) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id,
                name: "read_file".into(),
                input: serde_json::json!({ "path": "a" }),
            })],
        }
    }

    fn result(id: ToolCallId) -> Message {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                call_id: id,
                content: "contents".into(),
                is_error: false,
            }],
        }
    }

    /// user, assistant(call A + call B), tool(A), tool(B), assistant(text)
    fn conversation() -> Vec<Message> {
        let a = ToolCallId::new();
        let b = ToolCallId::new();
        let mut assistant = call(a);
        if let ContentBlock::ToolCall(second) = &mut call(b).content[0] {
            assistant
                .content
                .push(ContentBlock::ToolCall(second.clone()));
        }
        vec![
            Message::user("read a"),
            assistant,
            result(a),
            result(b),
            Message::assistant("done"),
        ]
    }

    #[test]
    fn a_tool_call_and_its_results_form_one_group() {
        let groups = group(&conversation());
        assert_eq!(groups.len(), 3, "user | assistant+2 results | assistant");
        assert_eq!(groups[1].start, 1);
        assert_eq!(groups[1].end, 4);
    }

    #[test]
    fn trimming_never_orphans_a_tool_result() {
        // A per-message trim would keep `tool(B)` and drop the assistant that asked for
        // it, which is a provider 400.
        let messages = conversation();
        let trimmed = trim(&messages, 1, 0).expect("the newest group is the floor");
        for (index, message) in trimmed.messages.iter().enumerate() {
            if message.role == Role::Tool {
                let previous = &trimmed.messages[..index];
                assert!(
                    previous.iter().any(|m| !m.tool_calls().is_empty()),
                    "a tool result must follow the call it answers"
                );
            }
        }
    }

    #[test]
    fn a_generous_budget_keeps_everything() {
        let messages = conversation();
        let trimmed = trim(&messages, 100_000, 0).unwrap();
        assert_eq!(trimmed.messages.len(), messages.len());
        assert_eq!(trimmed.dropped_groups, 0);
    }

    #[test]
    fn the_oldest_group_goes_first() {
        let messages = conversation();
        let groups = group(&messages);
        // Room for the last two groups but not the first.
        let budget = groups[1].tokens + groups[2].tokens;
        let trimmed = trim(&messages, budget, 0).unwrap();
        assert_eq!(trimmed.dropped_groups, 1);
        assert_ne!(
            trimmed.messages[0].text(),
            "read a",
            "the oldest group is the one that goes"
        );
    }

    #[test]
    fn a_pinned_prefix_is_never_dropped() {
        // The message a checkpoint folds its summary into: losing it is amnesia.
        let mut messages = vec![Message::user("[Summary of earlier conversation]\nlots")];
        messages.extend(conversation());
        let groups = group(&messages);
        // Exactly enough for the pinned summary plus the newest turn, and nothing else.
        let budget = groups[0].tokens + groups[groups.len() - 1].tokens;
        let trimmed = trim(&messages, budget, 1).expect("pinned plus floor must fit");
        assert!(trimmed.dropped_groups > 0, "the middle really was dropped");
        assert!(
            trimmed.messages[0].text().contains("Summary of earlier"),
            "{:?}",
            trimmed.messages[0]
        );
    }

    #[test]
    fn a_budget_below_the_floor_reports_no_answer() {
        let messages = vec![Message::user("x".repeat(10_000))];
        assert!(
            trim(&messages, 1, 0).is_none(),
            "better to say the context is too small than to send a broken array"
        );
    }

    #[test]
    fn empty_history_trims_to_nothing() {
        let trimmed = trim(&[], 100, 0).unwrap();
        assert!(trimmed.messages.is_empty());
        assert_eq!(trimmed.tokens, 0);
    }
}
