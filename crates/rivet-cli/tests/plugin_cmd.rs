//! `rivet plugin list | show | new`, driven as a subprocess.
//!
//! These deliberately never set an API key: `list` and `show` stop at `VALIDATED` and
//! construct nothing, so needing a credential to ask what plugins exist would be a bug.

mod support;

use std::path::Path;
use std::process::Stdio;

use support::Workspace;

/// A base URL nothing connects to: none of these commands reaches a provider.
const UNUSED_PROVIDER: &str = "http://127.0.0.1:1/v1";

/// Run `rivet` in `dir` with no credential in the environment.
async fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rivet"))
        .args(args)
        .current_dir(dir)
        .env_remove(support::KEY_ENV)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .expect("spawn rivet");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test]
async fn plugin_list_works_without_a_credential() {
    // Phase 1 routed this through the whole load path, so it failed on a machine with no
    // provider key -- while answering a question that has nothing to do with credentials.
    let workspace = Workspace::new(UNUSED_PROVIDER);
    let (code, stdout, stderr) = run_in(workspace.path(), &["plugin", "list"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    for id in [
        "rivet.model-openai",
        "rivet.tool-filesystem",
        "rivet.context-builtin",
    ] {
        assert!(stdout.contains(id), "{id} missing: {stdout}");
    }
    assert!(stdout.contains("VALIDATED"), "{stdout}");
    assert!(
        stdout.contains("builtin(rivet-tool-filesystem)"),
        "{stdout}"
    );
}

#[tokio::test]
async fn plugin_list_says_which_ids_the_config_actually_enables() {
    // Every catalog entry gets a row, or the command could not answer "what could I turn
    // on"; the column is what keeps that from reading as "all of these are on".
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rivet.toml"),
        "[plugins]\nenabled = [\"rivet.tool-filesystem\"]\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_in(dir.path(), &["plugin", "list"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let row = |id: &str| -> String {
        stdout
            .lines()
            .find(|line| line.contains(id))
            .unwrap_or_else(|| panic!("no row for {id}: {stdout}"))
            .to_string()
    };
    assert!(row("rivet.tool-filesystem").contains("yes"), "{stdout}");
    assert!(
        row("rivet.context-builtin").contains("yes"),
        "it is not optional, so it is on even unlisted: {stdout}"
    );
    assert!(
        row("rivet.model-openai").contains("no"),
        "and one the config left out says so: {stdout}"
    );
}

#[tokio::test]
async fn plugin_show_prints_requested_against_effective() {
    let workspace = Workspace::new(UNUSED_PROVIDER);
    let (code, stdout, stderr) = run_in(
        workspace.path(),
        &[
            "--profile",
            "readonly",
            "plugin",
            "show",
            "rivet.tool-filesystem",
        ],
    )
    .await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("fs_read(workspace)"), "{stdout}");
    assert!(stdout.contains("fs_write(workspace)"), "{stdout}");
    assert!(
        stdout.contains("removed by profile `readonly`"),
        "DoD 3 has to be visible to an operator, not only to a test: {stdout}"
    );
    assert!(stdout.contains("abi          0.1"), "{stdout}");

    // ...and under a writable profile it is granted instead.
    let (code, stdout, _stderr) = run_in(
        workspace.path(),
        &[
            "--profile",
            "developer",
            "plugin",
            "show",
            "rivet.tool-filesystem",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert!(!stdout.contains("removed by profile"), "{stdout}");
}

#[tokio::test]
async fn plugin_show_for_an_unknown_id_lists_the_ones_that_exist() {
    let workspace = Workspace::new(UNUSED_PROVIDER);
    let (code, _stdout, stderr) = run_in(workspace.path(), &["plugin", "show", "rivet.nope"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("rivet.tool-filesystem"), "{stderr}");
}

#[tokio::test]
async fn an_enabled_id_this_build_does_not_provide_fails_at_startup() {
    // The Phase 1 "warn and skip" middle category is gone: an id either loads or is a typo.
    //
    // The fixture was `rivet.tool-shell` until Phase 4 shipped it. `acme.` is a namespace
    // this repository never provides, so it stays a typo whatever a later phase adds.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rivet.toml"),
        "[plugins]\nenabled = [\"acme.tool-nonesuch\"]\n",
    )
    .unwrap();

    let (code, _stdout, stderr) = run_in(dir.path(), &["doctor"]).await;
    assert_eq!(code, 2, "a configuration problem is exit code 2: {stderr}");
    assert!(stderr.contains("acme.tool-nonesuch"), "{stderr}");
    assert!(
        stderr.contains("rivet.tool-filesystem"),
        "and it says what is available: {stderr}"
    );
}

#[tokio::test]
async fn plugin_new_scaffolds_a_crate_whose_manifest_parses() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_in(dir.path(), &["plugin", "new", "acme.tool-lint"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");

    let crate_dir = dir.path().join("acme-tool-lint");
    let manifest = std::fs::read_to_string(crate_dir.join("rivet-plugin.toml")).unwrap();
    let parsed = rivet_plugin::parse(&manifest).expect("the scaffold's own manifest parses");
    assert_eq!(parsed.id.as_str(), "acme.tool-lint");
    assert!(parsed.is_compatible_with(rivet_core::ABI_VERSION));

    let cargo = std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"acme-tool-lint\""), "{cargo}");
    let lib = std::fs::read_to_string(crate_dir.join("src/lib.rs")).unwrap();
    assert!(lib.contains("pub struct ToolLintPlugin"), "{lib}");
    assert!(lib.contains("MANIFEST_TOML"), "{lib}");

    // The printed next step has to name the same symbols the scaffold defines, or it is
    // a snippet that does not compile.
    assert!(stdout.contains("acme_tool_lint::MANIFEST_TOML"), "{stdout}");
    assert!(
        stdout.contains("acme_tool_lint::ToolLintPlugin::new"),
        "{stdout}"
    );
}

#[tokio::test]
async fn plugin_new_refuses_to_write_into_an_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, _stderr) = run_in(dir.path(), &["plugin", "new", "acme.tool-twice"]).await;
    assert_eq!(code, 0);

    let (code, _stdout, stderr) = run_in(dir.path(), &["plugin", "new", "acme.tool-twice"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("already exists"), "{stderr}");
}

#[tokio::test]
async fn plugin_new_validates_the_id_before_touching_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _stdout, stderr) = run_in(dir.path(), &["plugin", "new", "toollint"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("namespace.name"), "{stderr}");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "half a scaffold is worse than none"
    );
}
