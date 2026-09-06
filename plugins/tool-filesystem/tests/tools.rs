//! The four filesystem tools, against a real temporary repository.

mod support;

use rivet_core::error::ErrorKind;
use rivet_core::tool::Tool;
use rivet_tool_filesystem::{ListDir, ReadFile, Search, WriteFile};
use support::Fixture;

#[tokio::test]
async fn read_file_returns_a_files_contents() {
    let fixture = Fixture::new();
    let result = ReadFile
        .execute(fixture.ctx(), serde_json::json!({ "path": "src/main.rs" }))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("println!"), "{}", result.content);
    assert_eq!(result.structured.unwrap()["lines"], 3);
}

#[tokio::test]
async fn read_file_honors_a_line_window() {
    let fixture = Fixture::new();
    let result = ReadFile
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": "src/main.rs", "offset": 2, "limit": 1 }),
        )
        .await
        .unwrap();
    assert!(
        result.content.starts_with("    println!"),
        "{}",
        result.content
    );
    assert!(
        result.content.contains("[lines 2-2 of 3]"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn read_file_reports_a_binary_file_rather_than_dumping_it() {
    // Not the model's problem to fix, so not an error -- but not something to paste into
    // a prompt either.
    let fixture = Fixture::new();
    std::fs::write(
        fixture.path("logo.png"),
        [0x89, b'P', b'N', b'G', 0x00, 0x1a],
    )
    .unwrap();
    let result = ReadFile
        .execute(fixture.ctx(), serde_json::json!({ "path": "logo.png" }))
        .await
        .unwrap();
    assert!(!result.is_error, "a binary file is not a failure");
    assert!(result.content.contains("binary file"), "{}", result.content);
}

#[tokio::test]
async fn read_file_refuses_a_denied_path_and_says_so() {
    // A path the caller named: the refusal is raised, because the model asked for this
    // file specifically and has to know it was refused.
    let fixture = Fixture::new();
    let err = ReadFile
        .execute(fixture.ctx(), serde_json::json!({ "path": ".env" }))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PolicyDenied);
}

#[tokio::test]
async fn read_file_refuses_to_leave_the_workspace() {
    let fixture = Fixture::new();
    let escape = fixture.outside.path().join("passwd");
    let err = ReadFile
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": escape.display().to_string() }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PolicyDenied);
}

#[tokio::test]
async fn write_file_creates_and_replaces() {
    let fixture = Fixture::new();
    let ctx = fixture.ctx();
    WriteFile
        .execute(
            ctx.clone(),
            serde_json::json!({ "path": "src/new.rs", "content": "pub fn added() {}\n" }),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.path("src/new.rs")).unwrap(),
        "pub fn added() {}\n"
    );

    let result = WriteFile
        .execute(
            ctx,
            serde_json::json!({ "path": "src/new.rs", "content": "pub fn replaced() {}\n" }),
        )
        .await
        .unwrap();
    assert!(result.content.contains("wrote"), "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(fixture.path("src/new.rs")).unwrap(),
        "pub fn replaced() {}\n"
    );
}

#[tokio::test]
async fn write_file_refuses_a_missing_parent_directory() {
    let fixture = Fixture::new();
    let err = WriteFile
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": "nope/here.rs", "content": "x" }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[tokio::test]
async fn list_dir_lists_the_workspace_root() {
    // The representative prompt starts here: "list the files here and explain what this
    // project does".
    let fixture = Fixture::new();
    let result = ListDir
        .execute(fixture.ctx(), serde_json::json!({}))
        .await
        .unwrap();
    assert!(result.content.contains("Cargo.toml"), "{}", result.content);
    assert!(result.content.contains("src/"), "{}", result.content);
    assert!(
        !result.content.contains(".env"),
        "a denied entry is skipped, not raised: {}",
        result.content
    );
    assert!(
        !result.content.contains(".git"),
        "and noise does not cost context: {}",
        result.content
    );
}

#[tokio::test]
async fn list_dir_descends_when_asked() {
    let fixture = Fixture::new();
    let shallow = ListDir
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": ".", "depth": 1 }),
        )
        .await
        .unwrap();
    assert!(!shallow.content.contains("main.rs"), "{}", shallow.content);

    let deep = ListDir
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": ".", "depth": 2 }),
        )
        .await
        .unwrap();
    assert!(deep.content.contains("src/main.rs"), "{}", deep.content);
}

#[tokio::test]
async fn search_finds_a_literal_string() {
    let fixture = Fixture::new();
    let result = Search
        .execute(fixture.ctx(), serde_json::json!({ "query": "TODO" }))
        .await
        .unwrap();
    assert!(
        result.content.contains("src/lib.rs:1:"),
        "{}",
        result.content
    );
    assert_eq!(result.structured.unwrap()["matches"], 1);
}

#[tokio::test]
async fn search_is_case_insensitive_by_default() {
    let fixture = Fixture::new();
    let insensitive = Search
        .execute(fixture.ctx(), serde_json::json!({ "query": "todo" }))
        .await
        .unwrap();
    assert!(
        insensitive.content.contains("src/lib.rs"),
        "{}",
        insensitive.content
    );

    let sensitive = Search
        .execute(
            fixture.ctx(),
            serde_json::json!({ "query": "todo", "case_sensitive": true }),
        )
        .await
        .unwrap();
    assert!(
        sensitive.content.contains("no matches"),
        "{}",
        sensitive.content
    );
}

#[tokio::test]
async fn search_filters_by_glob() {
    let fixture = Fixture::new();
    let result = Search
        .execute(
            fixture.ctx(),
            serde_json::json!({ "query": "name", "glob": "**/*.rs" }),
        )
        .await
        .unwrap();
    assert!(
        result.content.contains("no matches"),
        "`name` only appears in Cargo.toml: {}",
        result.content
    );
}

#[tokio::test]
async fn search_skips_denied_files_instead_of_failing() {
    // One `.env` in a tree must not turn every search into a refusal. The secret still
    // never reaches the model.
    let fixture = Fixture::new();
    let result = Search
        .execute(
            fixture.ctx(),
            serde_json::json!({ "query": "secret-do-not-read" }),
        )
        .await
        .expect("a denied entry met while walking is skipped, not raised");
    assert!(result.content.contains("no matches"), "{}", result.content);
}

#[tokio::test]
async fn search_refuses_a_denied_path_argument() {
    // ...but a path the caller *named* is refused, because that is a fact the model asked
    // for and must see.
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.path(".ssh")).unwrap();
    let err = Search
        .execute(
            fixture.ctx(),
            serde_json::json!({ "query": "anything", "path": ".ssh" }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PolicyDenied);
}

#[tokio::test]
async fn search_stops_when_the_run_is_cancelled() {
    // The only long-running tool in Phase 1, and so the one that has to notice.
    let fixture = Fixture::new();
    for n in 0..300 {
        std::fs::write(fixture.path(&format!("file{n}.txt")), "needle\n".repeat(50)).unwrap();
    }
    fixture.cancel.cancel();

    let started = std::time::Instant::now();
    let err = Search
        .execute(fixture.ctx(), serde_json::json!({ "query": "needle" }))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Cancelled);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "cancellation has a five-second budget for the whole run"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_walk_never_follows_a_link_out_of_the_workspace() {
    let fixture = Fixture::new();
    std::fs::write(fixture.outside.path().join("leak.txt"), "needle-outside").unwrap();
    std::os::unix::fs::symlink(fixture.outside.path(), fixture.path("elsewhere")).unwrap();

    let listed = ListDir
        .execute(
            fixture.ctx(),
            serde_json::json!({ "path": ".", "depth": 5 }),
        )
        .await
        .unwrap();
    assert!(
        listed.content.contains("elsewhere -> (symlink"),
        "{}",
        listed.content
    );
    assert!(!listed.content.contains("leak.txt"), "{}", listed.content);

    let found = Search
        .execute(
            fixture.ctx(),
            serde_json::json!({ "query": "needle-outside" }),
        )
        .await
        .unwrap();
    assert!(found.content.contains("no matches"), "{}", found.content);
}
