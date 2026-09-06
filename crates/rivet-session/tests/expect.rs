//! Optimistic concurrency: `Expect` is what keeps two writers from interleaving a log.

mod support;

use rivet_core::session::{Expect, SessionStore};
use rivet_session::JsonlSessionStore;
use support::{created, store, user};

#[tokio::test]
async fn a_stale_sequence_number_is_refused() {
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    store
        .append(id, Expect::Seq(1), user("first"))
        .await
        .unwrap();

    let err = store
        .append(id, Expect::Seq(1), user("second, from a stale writer"))
        .await
        .unwrap_err();
    assert_eq!(
        err.kind(),
        rivet_core::error::ErrorKind::InvalidArgument,
        "the contract names this error kind, so a caller can branch on it"
    );
    assert!(err.message().contains("expected 1"), "{err}");
    assert_eq!(
        store.read(id, 1, 10).await.unwrap().len(),
        2,
        "the rejected event must not be on disk"
    );
}

#[tokio::test]
async fn expect_any_appends_unconditionally() {
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    let stored = store.append(id, Expect::Any, user("repair")).await.unwrap();
    assert_eq!(stored.seq, 2);
}

#[tokio::test]
async fn a_second_process_appending_is_detected_rather_than_interleaved() {
    // Two `rivet` processes resuming one session is a bug. This store does not lock
    // across processes -- it notices, refuses, and says so, which is the honest thing to
    // do until a real lock lands.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let id = rivet_core::id::SessionId::new();

    let ours = JsonlSessionStore::new(&root);
    ours.create(id, created()).await.unwrap();
    ours.append(id, Expect::Seq(1), user("ours")).await.unwrap();

    // A different store object stands in for a different process.
    let theirs = JsonlSessionStore::new(&root);
    theirs
        .append(id, Expect::Seq(2), user("theirs"))
        .await
        .unwrap();

    let err = ours
        .append(id, Expect::Seq(2), user("ours again"))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::InvalidArgument);
    assert!(err.message().contains("another writer"), "{err}");

    let events = theirs.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 3, "the log is intact, not interleaved");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1, "seq stays dense");
    }
}

#[tokio::test]
async fn appends_are_serialized_within_one_process() {
    // The dispatcher and the loop both write to one session; concurrent appends must not
    // produce a gap or a duplicate seq.
    let (store, _dir) = store();
    let store = std::sync::Arc::new(store);
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();

    let mut tasks = Vec::new();
    for n in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .append(id, Expect::Any, user(&format!("message {n}")))
                .await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    let events = store.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 9);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
    }
}
