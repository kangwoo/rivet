//! Append, read, list: the everyday path, plus the durability claim it rests on.

mod support;

use rivet_core::session::{Expect, SessionStore};
use rivet_session::JsonlSessionStore;
use support::{assistant_calling, created, store, tool_call, user};

#[tokio::test]
async fn a_created_session_starts_at_seq_one() {
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    let stored = store.create(id, created()).await.unwrap();
    assert_eq!(stored.seq, 1);
    assert_eq!(store.last_seq(id).await.unwrap(), 1);
}

#[tokio::test]
async fn creating_the_same_session_twice_is_refused() {
    // Opening an existing log is `append`'s job. Letting `create` do it would silently
    // reopen a conversation as if it were new.
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    let err = store.create(id, created()).await.unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::InvalidArgument);
    assert!(err.message().contains("already exists"), "{err}");
}

#[tokio::test]
async fn appended_events_survive_reopening_the_store() {
    // This is the whole point: `append` returns only once the bytes are on the medium, so
    // a brand new store object sees everything a crash would have had to preserve.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let id = rivet_core::id::SessionId::new();

    {
        let store = JsonlSessionStore::new(&root);
        store.create(id, created()).await.unwrap();
        store
            .append(id, Expect::Seq(1), user("explain this repo"))
            .await
            .unwrap();
    }

    let reopened = JsonlSessionStore::new(&root);
    let events = reopened.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(reopened.last_seq(id).await.unwrap(), 2);
}

#[tokio::test]
async fn read_honors_the_window() {
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    for n in 0..5 {
        store
            .append(id, Expect::Seq(n + 1), user(&format!("message {n}")))
            .await
            .unwrap();
    }

    let page = store.read(id, 3, 2).await.unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].seq, 3);
    assert_eq!(page[1].seq, 4);
    assert!(store.read(id, 99, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn reading_an_unknown_session_is_not_found() {
    let (store, _dir) = store();
    let err = store
        .read(rivet_core::id::SessionId::new(), 1, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::NotFound);
}

#[tokio::test]
async fn list_is_newest_first_and_titled_by_the_first_user_message() {
    let (store, _dir) = store();
    let older = rivet_core::id::SessionId::new();
    store.create(older, created()).await.unwrap();
    store
        .append(older, Expect::Seq(1), user("the older question"))
        .await
        .unwrap();

    let newer = rivet_core::id::SessionId::new();
    store.create(newer, created()).await.unwrap();
    store
        .append(newer, Expect::Seq(1), user("the newer question"))
        .await
        .unwrap();

    let listed = store.list(10).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, newer, "newest first, without an index file");
    assert_eq!(listed[0].title, "the newer question");
    assert_eq!(listed[1].id, older);

    assert_eq!(store.list(1).await.unwrap().len(), 1, "limit is honored");
}

#[tokio::test]
async fn listing_an_empty_root_is_not_an_error() {
    let (store, _dir) = store();
    assert!(store.list(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_event_variant_round_trips_through_the_log() {
    // The log is an on-disk format; a variant that cannot make the round trip is a
    // session that cannot be resumed.
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    let run_id = rivet_core::id::RunId::new();
    let call = tool_call("read_file");

    store.create(id, created()).await.unwrap();
    store
        .append(id, Expect::Seq(1), support::run_started(run_id))
        .await
        .unwrap();
    store
        .append(id, Expect::Seq(2), user("read Cargo.toml"))
        .await
        .unwrap();
    store
        .append(id, Expect::Seq(3), assistant_calling(run_id, &call))
        .await
        .unwrap();
    store
        .append(
            id,
            Expect::Seq(4),
            rivet_core::session::SessionEvent::ToolCalled {
                run_id,
                call: call.clone(),
            },
        )
        .await
        .unwrap();
    store
        .append(
            id,
            Expect::Seq(5),
            support::completed(run_id, &call, "[package]"),
        )
        .await
        .unwrap();

    let events = store.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 6, "created + five appends");
    let state = rivet_core::session::SessionState::replay(&events);
    assert!(state.pending_tool_calls().is_empty());
    assert_eq!(state.messages.len(), 3, "user, assistant, tool result");
}

#[tokio::test]
async fn a_log_line_is_one_json_object_per_fact() {
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    store.create(id, created()).await.unwrap();
    store.append(id, Expect::Seq(1), user("hi")).await.unwrap();

    let raw = String::from_utf8(support::log_bytes(&store, id)).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(raw.ends_with('\n'), "every line is terminated");
    for line in lines {
        let value: serde_json::Value = serde_json::from_str(line).expect("one object per line");
        assert!(value["seq"].is_u64());
        assert!(value["event"]["type"].is_string());
    }
}
