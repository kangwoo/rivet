//! One writer per session log.
//!
//! [`rivet_core::session::Expect::Seq`] is what stops two writers interleaving a log, and
//! it only works if the writer knows the current sequence number. The loop and the
//! dispatcher both append to one session, so they share this handle rather than each
//! keeping their own guess: two independent guesses would make every second append a
//! spurious conflict.
//!
//! `Expect::Any` is deliberately not offered here. The contract reserves it for a repair
//! tool, and the agent loop is not one.

use std::fmt;
use std::sync::Arc;

use rivet_core::id::SessionId;
use rivet_core::session::{Expect, SessionEvent, SessionState, SessionStore, StoredEvent};
use tokio::sync::Mutex;

/// A sequence-tracking writer for one session.
#[derive(Clone)]
pub struct SessionWriter {
    store: Arc<dyn SessionStore>,
    session_id: SessionId,
    last_seq: Arc<Mutex<u64>>,
}

impl fmt::Debug for SessionWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionWriter")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl SessionWriter {
    /// Build a writer positioned at `last_seq`.
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>, session_id: SessionId, last_seq: u64) -> Self {
        Self {
            store,
            session_id,
            last_seq: Arc::new(Mutex::new(last_seq)),
        }
    }

    /// Build a writer positioned at the end of an already-replayed state.
    #[must_use]
    pub fn at_state(
        store: Arc<dyn SessionStore>,
        session_id: SessionId,
        state: &SessionState,
    ) -> Self {
        Self::new(store, session_id, state.last_seq)
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub fn store(&self) -> &Arc<dyn SessionStore> {
        &self.store
    }

    /// The sequence number of the last event this writer appended.
    pub async fn last_seq(&self) -> u64 {
        *self.last_seq.lock().await
    }

    /// Append one event, holding the position lock across the write.
    ///
    /// # Errors
    /// Whatever the store returns. An `InvalidArgument` here means another writer got
    /// there first, and the run must stop rather than interleave.
    pub async fn append(&self, event: SessionEvent) -> rivet_core::Result<StoredEvent> {
        let mut position = self.last_seq.lock().await;
        let stored = self
            .store
            .append(self.session_id, Expect::Seq(*position), event)
            .await?;
        *position = stored.seq;
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::model::Message;
    use rivet_core::session::{SessionSummary, StoredEvent};
    use rivet_core::time::Timestamp;

    /// A store that records what expectations it was handed.
    #[derive(Debug, Default)]
    struct RecordingStore {
        seen: Mutex<Vec<Expect>>,
        seq: std::sync::atomic::AtomicU64,
    }

    #[async_trait::async_trait]
    impl SessionStore for RecordingStore {
        async fn create(
            &self,
            _id: SessionId,
            event: SessionEvent,
        ) -> rivet_core::Result<StoredEvent> {
            Ok(StoredEvent {
                seq: 1,
                at: Timestamp::now(),
                event,
            })
        }

        async fn append(
            &self,
            _id: SessionId,
            expect: Expect,
            event: SessionEvent,
        ) -> rivet_core::Result<StoredEvent> {
            self.seen.lock().await.push(expect);
            let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok(StoredEvent {
                seq,
                at: Timestamp::now(),
                event,
            })
        }

        async fn read(
            &self,
            _id: SessionId,
            _from_seq: u64,
            _limit: usize,
        ) -> rivet_core::Result<Vec<StoredEvent>> {
            Ok(Vec::new())
        }

        async fn last_seq(&self, _id: SessionId) -> rivet_core::Result<u64> {
            Ok(0)
        }

        async fn list(&self, _limit: usize) -> rivet_core::Result<Vec<SessionSummary>> {
            Ok(Vec::new())
        }

        async fn fork(
            &self,
            _source: SessionId,
            _at_seq: u64,
            _new_id: SessionId,
        ) -> rivet_core::Result<SessionSummary> {
            unimplemented!("not exercised")
        }
    }

    #[tokio::test]
    async fn each_append_expects_the_previous_sequence_number() {
        // Forgetting to advance is the bug that made every second synthetic close fail
        // with InvalidArgument.
        let store = Arc::new(RecordingStore::default());
        let writer = SessionWriter::new(store.clone(), SessionId::new(), 7);
        for _ in 0..3 {
            writer
                .append(SessionEvent::UserMessage {
                    message: Message::user("hi"),
                })
                .await
                .unwrap();
        }
        let seen = store.seen.lock().await.clone();
        assert_eq!(seen, [Expect::Seq(7), Expect::Seq(1), Expect::Seq(2)]);
        assert_eq!(writer.last_seq().await, 3);
    }
}
