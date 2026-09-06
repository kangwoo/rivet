//! What a `kill -9` leaves behind, and what the store does about it.
//!
//! `append` writes one line with one `write_all`, so a crash can only damage the final
//! line. Everything else is somebody else's corruption and must not be papered over.

mod support;

use rivet_core::session::{Expect, SessionStore};
use rivet_session::JsonlSessionStore;
use support::{created, store, user};

/// Build a session, then overwrite its log with `mangle`d bytes.
async fn crashed_log(
    mangle: impl Fn(Vec<u8>) -> Vec<u8>,
) -> (tempfile::TempDir, rivet_core::id::SessionId) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let id = rivet_core::id::SessionId::new();
    {
        let store = JsonlSessionStore::new(&root);
        store.create(id, created()).await.unwrap();
        store
            .append(id, Expect::Seq(1), user("first"))
            .await
            .unwrap();
        store
            .append(id, Expect::Seq(2), user("second"))
            .await
            .unwrap();
        let bytes = support::log_bytes(&store, id);
        support::write_log_bytes(&store, id, &mangle(bytes));
    }
    (dir, id)
}

#[tokio::test]
async fn a_half_written_final_line_is_truncated_away() {
    let (dir, id) = crashed_log(|mut bytes| {
        bytes.extend_from_slice(br#"{"seq":4,"at":"2026-09"#);
        bytes
    })
    .await;

    let store = JsonlSessionStore::new(dir.path().join("sessions"));
    let events = store.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 3, "the torn line is gone");
    assert_eq!(store.last_seq(id).await.unwrap(), 3);

    // And the file itself was repaired, so the next append lands at the right offset.
    store
        .append(id, Expect::Seq(3), user("after the crash"))
        .await
        .unwrap();
    let events = store.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].seq, 4);
}

#[tokio::test]
async fn inspecting_a_torn_log_does_not_rewrite_it() {
    // `read` repairs on the way past, because a caller about to *append* needs the file
    // sound first. Inspection is not that caller: `rivet session show` reports what is on
    // disk. `list` already held this line and `show` did not, which is the asymmetry.
    let (dir, id) = crashed_log(|mut bytes| {
        bytes.extend_from_slice(br#"{"seq":4,"at":"2026-09"#);
        bytes
    })
    .await;

    let root = dir.path().join("sessions");
    let store = JsonlSessionStore::new(&root);
    let before = support::log_bytes(&store, id);

    let events = store.read_all_readonly(id).await.unwrap();
    assert_eq!(events.len(), 3, "the torn line is not reported");

    let after = support::log_bytes(&store, id);
    assert_eq!(before, after, "inspection must leave the bytes alone");

    // The repairing path is still there for the caller that needs it.
    let events = store.read(id, 1, 100).await.unwrap();
    assert_eq!(events.len(), 3);
    assert!(
        support::log_bytes(&store, id).len() < before.len(),
        "`read` still repairs"
    );
}

#[tokio::test]
async fn a_torn_line_that_happens_to_end_in_a_newline_is_also_repaired() {
    let (dir, id) = crashed_log(|mut bytes| {
        bytes.extend_from_slice(b"{\"seq\":4,\"at\"\n");
        bytes
    })
    .await;
    let store = JsonlSessionStore::new(dir.path().join("sessions"));
    assert_eq!(store.read(id, 1, 100).await.unwrap().len(), 3);
}

#[tokio::test]
async fn damage_in_the_middle_of_a_log_fails_loudly() {
    // We never write past a broken line, so this is not our crash. Skipping it would make
    // `replay` report a conversation that never happened.
    let (dir, id) = crashed_log(|bytes| {
        let text = String::from_utf8(bytes).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[1] = "{ not json".into();
        format!("{}\n", lines.join("\n")).into_bytes()
    })
    .await;

    let store = JsonlSessionStore::new(dir.path().join("sessions"));
    let err = store.read(id, 1, 100).await.unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::Storage);
    assert!(err.message().contains("corrupt"), "{err}");
}

#[tokio::test]
async fn a_sequence_gap_fails_loudly() {
    let (dir, id) = crashed_log(|bytes| {
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Drop the middle event, leaving 1 and 3.
        format!("{}\n{}\n", lines[0], lines[2]).into_bytes()
    })
    .await;

    let store = JsonlSessionStore::new(dir.path().join("sessions"));
    let err = store.read(id, 1, 100).await.unwrap_err();
    assert_eq!(err.kind(), rivet_core::error::ErrorKind::Storage);
    assert!(err.message().contains("expected seq 2"), "{err}");
}

#[tokio::test]
async fn a_crash_between_call_and_result_still_replays_into_a_resumable_conversation() {
    // The `kill -9` shape from the DoD, end to end through a real file.
    let (store, _dir) = store();
    let id = rivet_core::id::SessionId::new();
    let run_id = rivet_core::id::RunId::new();
    let call = support::tool_call("read_file");

    store.create(id, created()).await.unwrap();
    store
        .append(id, Expect::Seq(1), user("read Cargo.toml"))
        .await
        .unwrap();
    store
        .append(
            id,
            Expect::Seq(2),
            support::assistant_calling(run_id, &call),
        )
        .await
        .unwrap();
    store
        .append(
            id,
            Expect::Seq(3),
            rivet_core::session::SessionEvent::ToolCalled {
                run_id,
                call: call.clone(),
            },
        )
        .await
        .unwrap();
    // ...and the process died here.

    let events = store.read(id, 1, 100).await.unwrap();
    let state = rivet_core::session::SessionState::replay(&events);
    assert!(state.pending_tool_calls().is_empty());
    assert_eq!(
        state.messages.last().unwrap().role,
        rivet_core::model::Role::Tool,
        "replay closes the dangling call so the next request is not a 400"
    );
}
