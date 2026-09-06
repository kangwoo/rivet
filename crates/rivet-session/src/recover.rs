//! Reading a log that a crash may have interrupted.
//!
//! `append` writes one line with one `write_all`, so the only line a `kill -9` can damage
//! is the last one. That single fact is what makes recovery decidable:
//!
//! - a torn **last** line is our own half-finished write -> truncate it and say so
//! - a broken **middle** line was not written by us -> fail hard, because a replay that
//!   silently skips a fact is a replay that lies
//! - a `seq` gap or a backwards `seq` -> fail hard, for the same reason

use std::path::Path;

use rivet_core::error::Error;
use rivet_core::session::StoredEvent;

/// What a scan of a log file found.
#[derive(Debug)]
pub struct Scan {
    pub events: Vec<StoredEvent>,
    /// Byte length of the intact prefix. Bytes past this are a torn write.
    pub good_bytes: u64,
    /// Set when the file ends in a partial line that must be truncated away.
    pub torn_tail: bool,
}

impl Scan {
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.events.last().map_or(0, |e| e.seq)
    }
}

/// Parse a whole log, classifying any damage.
///
/// # Errors
/// [`rivet_core::error::ErrorKind::Storage`] for corruption anywhere but the final line.
pub fn scan(path: &Path, data: &[u8]) -> rivet_core::Result<Scan> {
    let mut events = Vec::new();
    let mut good_bytes = 0u64;
    let mut torn_tail = false;
    let mut offset = 0usize;

    while offset < data.len() {
        let Some(relative) = data[offset..].iter().position(|b| *b == b'\n') else {
            // No terminator: a write that did not finish.
            torn_tail = true;
            break;
        };
        let line = &data[offset..offset + relative];
        let end = offset + relative + 1;

        match serde_json::from_slice::<StoredEvent>(line) {
            Ok(event) => {
                let expected = events.last().map_or(1, |e: &StoredEvent| e.seq + 1);
                if event.seq != expected {
                    return Err(corrupt(
                        path,
                        &format!(
                            "expected seq {expected} but found {} at byte {offset}",
                            event.seq
                        ),
                    ));
                }
                events.push(event);
                good_bytes = end as u64;
                offset = end;
            }
            Err(cause) => {
                if end == data.len() {
                    // The last line, terminated but unparseable: still our torn write.
                    torn_tail = true;
                    break;
                }
                return Err(corrupt(
                    path,
                    &format!("line at byte {offset} is not a session event"),
                )
                .with_cause(cause));
            }
        }
    }

    Ok(Scan {
        events,
        good_bytes,
        torn_tail,
    })
}

fn corrupt(path: &Path, detail: &str) -> Error {
    Error::storage(format!(
        "session log `{}` is corrupt: {detail}. \
         This was not written by an interrupted append; replaying it would drop facts.",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::model::Message;
    use rivet_core::session::SessionEvent;
    use rivet_core::time::Timestamp;

    fn line(seq: u64, text: &str) -> String {
        let event = StoredEvent {
            seq,
            at: Timestamp::from_millis(i64::try_from(seq).unwrap()).unwrap(),
            event: SessionEvent::UserMessage {
                message: Message::user(text),
            },
        };
        format!("{}\n", serde_json::to_string(&event).unwrap())
    }

    fn path() -> &'static Path {
        Path::new("/tmp/log.jsonl")
    }

    #[test]
    fn an_intact_log_scans_cleanly() {
        let data = format!("{}{}", line(1, "a"), line(2, "b"));
        let scan = scan(path(), data.as_bytes()).unwrap();
        assert_eq!(scan.events.len(), 2);
        assert_eq!(scan.last_seq(), 2);
        assert!(!scan.torn_tail);
        assert_eq!(scan.good_bytes, data.len() as u64);
    }

    #[test]
    fn a_write_cut_mid_line_truncates_to_the_last_good_newline() {
        let good = line(1, "a");
        let data = format!("{good}{{\"seq\":2,\"at\":\"197");
        let scan = scan(path(), data.as_bytes()).unwrap();
        assert_eq!(scan.events.len(), 1);
        assert!(scan.torn_tail);
        assert_eq!(scan.good_bytes, good.len() as u64);
    }

    #[test]
    fn a_terminated_but_unparseable_last_line_is_also_a_torn_write() {
        let good = line(1, "a");
        let data = format!("{good}{{\"seq\":2,\"at\"\n");
        let scan = scan(path(), data.as_bytes()).unwrap();
        assert_eq!(scan.events.len(), 1);
        assert!(scan.torn_tail);
    }

    #[test]
    fn damage_in_the_middle_is_a_hard_failure() {
        // We never write a line after a broken one, so this is somebody else's damage --
        // and quietly skipping it would make replay report a conversation that never
        // happened.
        let data = format!("{}garbage\n{}", line(1, "a"), line(2, "b"));
        let err = scan(path(), data.as_bytes()).unwrap_err();
        assert_eq!(err.kind(), rivet_core::error::ErrorKind::Storage);
        assert!(err.message().contains("corrupt"), "{err}");
    }

    #[test]
    fn a_sequence_gap_is_a_hard_failure() {
        let data = format!("{}{}", line(1, "a"), line(3, "c"));
        let err = scan(path(), data.as_bytes()).unwrap_err();
        assert!(err.message().contains("expected seq 2"), "{err}");
    }

    #[test]
    fn an_empty_log_is_not_corrupt() {
        let scan = scan(path(), b"").unwrap();
        assert!(scan.events.is_empty());
        assert_eq!(scan.last_seq(), 0);
        assert!(!scan.torn_tail);
    }
}
