//! Projecting a log into the row `rivet session list` shows.

use rivet_core::id::SessionId;
use rivet_core::session::{SessionEvent, SessionSummary, StoredEvent};
use rivet_core::time::Timestamp;

/// How much of the first user message becomes the title.
const TITLE_CHARS: usize = 80;

/// Fold a whole log into a summary row.
#[must_use]
pub fn summarize(id: SessionId, events: &[StoredEvent]) -> SessionSummary {
    let created_at = events.first().map_or_else(Timestamp::now, |e| e.at);
    let updated_at = events.last().map_or(created_at, |e| e.at);
    let mut title = String::new();
    let mut closed = false;

    for stored in events {
        match &stored.event {
            SessionEvent::UserMessage { message } if title.is_empty() => {
                title = truncate_chars(&message.text(), TITLE_CHARS);
            }
            SessionEvent::Closed { .. } => closed = true,
            _ => {}
        }
    }

    SessionSummary {
        id,
        created_at,
        updated_at,
        last_seq: events.last().map_or(0, |e| e.seq),
        title,
        closed,
    }
}

/// Cut on a character boundary, never mid-glyph.
fn truncate_chars(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::model::Message;

    fn stored(seq: u64, event: SessionEvent) -> StoredEvent {
        StoredEvent {
            seq,
            at: Timestamp::from_millis(i64::try_from(seq).unwrap() * 1000).unwrap(),
            event,
        }
    }

    #[test]
    fn the_title_is_the_first_user_message() {
        let events = vec![
            stored(
                1,
                SessionEvent::Created {
                    workspace_root: "/repo".into(),
                    parent: None,
                },
            ),
            stored(
                2,
                SessionEvent::UserMessage {
                    message: Message::user("explain this repo"),
                },
            ),
            stored(
                3,
                SessionEvent::UserMessage {
                    message: Message::user("and now the tests"),
                },
            ),
        ];
        let summary = summarize(SessionId::new(), &events);
        assert_eq!(summary.title, "explain this repo");
        assert_eq!(summary.last_seq, 3);
        assert!(!summary.closed);
        assert!(summary.updated_at > summary.created_at);
    }

    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        let korean = "한".repeat(200);
        let events = vec![stored(
            1,
            SessionEvent::UserMessage {
                message: Message::user(korean),
            },
        )];
        let summary = summarize(SessionId::new(), &events);
        assert_eq!(summary.title.chars().count(), TITLE_CHARS);
        assert!(summary.title.ends_with('…'));
    }

    #[test]
    fn a_closed_session_says_so() {
        let events = vec![stored(
            1,
            SessionEvent::Closed {
                reason: "done".into(),
            },
        )];
        assert!(summarize(SessionId::new(), &events).closed);
    }
}
