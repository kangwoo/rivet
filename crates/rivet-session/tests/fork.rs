//! Forking: a new session inheriting `[1, at_seq]`, and saying where it came from.

mod support;

use rivet_core::session::{SessionEvent, SessionStore};
use support::{created, store, user};

async fn seeded() -> (
    rivet_session::JsonlSessionStore,
    tempfile::TempDir,
    rivet_core::id::SessionId,
) {
    let (store, dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    for n in 0..4 {
        store
            .append(
                id,
                rivet_core::session::Expect::Seq(n + 1),
                user(&format!("message {n}")),
            )
            .await
            .unwrap();
    }
    (store, dir, id)
}

#[tokio::test]
async fn a_fork_inherits_the_prefix_and_records_where_it_branched() {
    let (store, _dir, source) = seeded().await;
    let forked = rivet_core::id::SessionId::new();

    let summary = store.fork(source, 3, forked).await.unwrap();
    assert_eq!(summary.id, forked);
    assert_eq!(summary.last_seq, 3);

    let events = store.read(forked, 1, 100).await.unwrap();
    assert_eq!(events.len(), 3);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1, "seq stays dense from 1");
    }

    match &events[0].event {
        SessionEvent::Created { parent, .. } => {
            let point = parent.as_ref().expect("a fork records its origin");
            assert_eq!(point.session_id, source);
            assert_eq!(point.at_seq, 3);
        }
        other => panic!("a session must begin with session.created, got {other:?}"),
    }
}

#[tokio::test]
async fn a_forked_session_keeps_growing_independently() {
    let (store, _dir, source) = seeded().await;
    let forked = rivet_core::id::SessionId::new();
    store.fork(source, 2, forked).await.unwrap();

    store
        .append(
            forked,
            rivet_core::session::Expect::Seq(2),
            user("a new branch"),
        )
        .await
        .unwrap();

    assert_eq!(store.last_seq(forked).await.unwrap(), 3);
    assert_eq!(
        store.last_seq(source).await.unwrap(),
        5,
        "the source is untouched"
    );
}

#[tokio::test]
async fn historical_timestamps_are_preserved() {
    let (store, _dir, source) = seeded().await;
    let original = store.read(source, 1, 3).await.unwrap();
    let forked = rivet_core::id::SessionId::new();
    store.fork(source, 3, forked).await.unwrap();
    let copied = store.read(forked, 1, 3).await.unwrap();

    for (before, after) in original.iter().zip(copied.iter()) {
        assert_eq!(
            before.at, after.at,
            "when something happened is a fact, not a detail of this copy"
        );
    }
}

#[tokio::test]
async fn forking_past_the_end_is_refused() {
    let (store, _dir, source) = seeded().await;
    let err = store
        .fork(source, 99, rivet_core::id::SessionId::new())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::InvalidArgument);
    assert!(err.message().contains("cannot fork at seq 99"), "{err}");
}

#[tokio::test]
async fn forking_at_seq_zero_is_refused() {
    let (store, _dir, source) = seeded().await;
    let err = store
        .fork(source, 0, rivet_core::id::SessionId::new())
        .await
        .unwrap_err();
    assert!(err.message().contains("seq 0"), "{err}");
}
