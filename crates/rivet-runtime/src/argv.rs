//! Building an argv a model cannot turn into a flag.
//!
//! A tool that puts a model-supplied string into argv is one `-` away from handing the
//! child program an *option* instead of an operand. `git --no-pager diff --output=../x`
//! exits 0 and writes a file one level above the working directory — from a tool annotated
//! `read_only`, under a profile that grants no `fs_write` at all. Schema validation does not
//! catch it (`{"type":"string"}` says nothing about the value), the workspace policy does not
//! see it (it inspects `path` and `cwd`, and this is neither), and `sandbox-local` reports
//! `filesystem_isolation: false`, so nothing downstream confines the write either.
//!
//! # Why this is a module and not a rule
//!
//! The rule already existed. `tool-git` put `--` before its `path` argument precisely so
//! that "a path that looks like a revision is still a path" — and then `rev` was pushed two
//! lines earlier with no guard at all. A rule applied at each call site is a rule the next
//! argument skips by being added in the wrong place, and there is nothing in a `Vec<String>`
//! to notice.
//!
//! So the shape is a type with **no way in that is unsafe**. Every argv entry arrives through
//! one of four doors, and each is safe by its own mechanism:
//!
//! | Door | Who chose it | What makes it safe |
//! |---|---|---|
//! | [`Argv::flag`] | the tool | `&'static str` — a value read out of the model's JSON cannot be one |
//! | [`Argv::option`] | the model | bound to a [`Consuming`] option, which takes the next entry verbatim |
//! | [`Argv::operand`] | the model | **refused** when it could be read as an option |
//! | [`Argv::pathspec`] | the model | after the `--` separator, which [`Argv`] emits, not the caller |
//!
//! There is no `push`. Adding an argument means picking a door, and every door is closed.
//!
//! # Why the option is a type and not a `&'static str`
//!
//! [`Argv::option`]'s safety is a fact about the *flag*, not about the value: it holds only
//! while the flag really consumes the next argv entry. Taking a `&'static str` left that as
//! something the author had to get right — `argv.option("--staged", model_value)` compiles,
//! and puts a model-supplied string in argv free-standing.
//!
//! [`Consuming`] is that fact written down. Its field is private and its only values are the
//! `const` items declared beside it, so a fourth option is an edit to *this* file — the one
//! whose whole subject is this guard — rather than a line in a plugin. The same list is what
//! [`option_exposure`] uses to decide whether a value is bound, which is why the walk in
//! `tool-git`'s and `tool-shell`'s injection tests can now see that misuse: its binder rule
//! stopped being a guess about anything beginning with `-` and became this list.
//!
//! Gluing the value on (`--flag=value`) is the other obvious answer, and it is not taken:
//! it works for `--max-count` and `-m`, but `sh -c<command>` is not reliable across `/bin/sh`
//! implementations, and this module has to hold for any program.
//!
//! # Why a leading `-` and not `--end-of-options`
//!
//! `git` has supported `--end-of-options` since 2.24 and it is the canonical fix for this in
//! git specifically. It is not the fix *here*: this module is what `tool-shell` uses too, and
//! the property — "a value the model supplied is never parsed as an option" — has to hold for
//! any program. Refusing the leading `-` holds everywhere, needs no version floor, and gives
//! the model a reason it can act on rather than a silently reinterpreted argument.

use rivet_core::error::{Capability, Error, ErrorKind};
use rivet_core::sandbox::ExecSpec;

/// An option that consumes the argv entry after it.
///
/// The closed set of flags [`Argv::option`] will bind a model-supplied value to. A private
/// field is the whole mechanism: there is no way to name one that is not declared here, so
/// "this flag takes a separate operand" is checked once, when the constant is written, rather
/// than assumed at every call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Consuming(&'static str);

impl Consuming {
    /// `git commit -m <message>`.
    pub const MESSAGE: Self = Self("-m");
    /// `sh -c <command>`.
    pub const COMMAND: Self = Self("-c");
    /// `git log --max-count <n>`.
    ///
    /// The separate form, not `--max-count=<n>`: gluing consumes nothing, so what follows a
    /// glued option is free-standing.
    pub const MAX_COUNT: Self = Self("--max-count");

    /// Every one of them, in one place, because [`option_exposure`] reads this list as its
    /// binder rule. A constant added above and left out here would make the oracle blind to
    /// exactly the argument it was added for, so they are declared together.
    const ALL: [Self; 3] = [Self::MESSAGE, Self::COMMAND, Self::MAX_COUNT];

    /// The flag as it reaches argv.
    #[must_use]
    pub fn flag(self) -> &'static str {
        self.0
    }

    /// Whether `arg` is an option that consumes the entry after it.
    fn binds(arg: &str) -> bool {
        Self::ALL.iter().any(|consuming| consuming.0 == arg)
    }
}

/// An argument list under construction.
#[derive(Clone, Debug, Default)]
pub struct Argv {
    head: Vec<String>,
    /// Held apart so the `--` separator is emitted by [`Argv::into_args`] rather than
    /// remembered by a caller, and so a pathspec cannot land before it.
    paths: Vec<String>,
}

impl Argv {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A literal the **tool** chose: `diff`, `--staged`, `-c`.
    ///
    /// `&'static str` is the whole guard. A string read out of the model's input borrows from
    /// a local `serde_json::Value` and does not live for `'static`, so it cannot arrive here
    /// by accident — only by a deliberate leak, which is a different kind of mistake.
    pub fn flag(&mut self, flag: &'static str) {
        self.head.push(flag.to_string());
    }

    /// A model-supplied value **bound to the option that precedes it**: `-m <message>`,
    /// `-c <command>`, `--max-count <n>`.
    ///
    /// Safe because the option consumes the next argv entry verbatim, whatever it starts
    /// with — that is what makes `git commit -m "--amend"` a commit with a strange message
    /// rather than an amend. The pairing is one call, so the binding cannot be broken by
    /// something being inserted between the two pushes.
    ///
    /// That the option consumes anything at all is [`Consuming`]'s job to know. A flag that
    /// does not cannot be named here, so the value cannot come out free-standing.
    pub fn option(&mut self, flag: Consuming, value: &str) {
        self.head.push(flag.0.to_string());
        self.head.push(value.to_string());
    }

    /// A model-supplied value standing on its own: a revision, a branch name, a ref.
    ///
    /// This is the one the child parses as an option if it starts with `-`, so this is the
    /// one that is refused. `what` names the input key so the model is told which of its
    /// arguments to fix.
    ///
    /// # Errors
    /// [`ErrorKind::PolicyDenied`] when the value could be read as an option. The same kind
    /// as an escaping path, and for the same reason: the dispatcher records it as
    /// `tool.blocked`, so an attempt to reach outside the workspace is a durable fact rather
    /// than a tool result that scrolls away.
    pub fn operand(&mut self, what: &str, value: &str) -> rivet_core::Result<()> {
        // Spelled out rather than calling [`looks_like_an_option`], which is what
        // [`option_exposure`] uses. The guard and the oracle that checks built argument lists
        // against it must be able to disagree: sharing one predicate meant a mutation of the
        // guard blinded the oracle at the same moment, and the schema walk that is supposed
        // to catch the mutation passed vacuously instead.
        if value.starts_with('-') {
            return Err(Error::new(
                ErrorKind::PolicyDenied,
                Capability::Tool,
                format!(
                    "`{what}` = `{value}` starts with `-`, so the command would read it as an \
                     option rather than a value. Arguments that begin with `-` are refused: \
                     one of them (`--output=…`) writes a file wherever it points."
                ),
            ));
        }
        self.head.push(value.to_string());
        Ok(())
    }

    /// A path, placed after the `--` separator.
    ///
    /// The separator is [`Argv`]'s to emit, and pathspecs are collected apart from everything
    /// else so they always land after it however the calls are ordered. Past `--` a leading
    /// `-` is a filename, so nothing is refused here — a file really named `-rf` is still a
    /// file, and containment for the path itself is `fsguard`'s job, before this.
    pub fn pathspec(&mut self, path: &str) {
        // An empty pathspec is dropped, not emitted. `git … -- ""` exits 128 with "empty
        // string is not a valid pathspec. please use . instead if you meant to match all
        // paths" — and `.` is the input that produces it, because a workspace-relative `.`
        // strips to nothing, so a model that follows git's own advice loops. Emitting
        // nothing means "everything", which is exactly what an empty pathspec meant. One
        // guard at the single door rather than one at each call site is this module's own
        // argument.
        if path.is_empty() {
            return;
        }
        self.paths.push(path.to_string());
    }

    /// The finished argument list.
    #[must_use]
    pub fn into_args(self) -> Vec<String> {
        let mut args = self.head;
        if !self.paths.is_empty() {
            args.push("--".to_string());
            args.extend(self.paths);
        }
        args
    }

    /// The finished [`ExecSpec`], for the program the **tool** named.
    ///
    /// `&'static str` for the same reason as [`Argv::flag`]: the program is never something
    /// the model chose.
    #[must_use]
    pub fn into_exec(self, program: &'static str) -> ExecSpec {
        ExecSpec::new(program, self.into_args())
    }
}

/// Whether a child program would read this string as an option.
#[must_use]
pub fn looks_like_an_option(value: &str) -> bool {
    value.starts_with('-')
}

/// Where `value` sits in `args` if the child could read it as an option.
///
/// The invariant of this module, written as code so a test can check it against a *built*
/// argument list rather than against the source that built it. `None` means the value is
/// shielded — absent, past the `--` separator, bound to the option before it, or simply not
/// option-shaped. `Some(index)` means the child will parse it as an option.
///
/// A test that feeds every declared string input an option-shaped value and asserts `None`
/// here covers arguments nobody has written yet, which is the failure this module exists to
/// make impossible.
#[must_use]
pub fn option_exposure(args: &[String], value: &str) -> Option<usize> {
    if !looks_like_an_option(value) {
        return None;
    }
    let index = args.iter().position(|arg| arg == value)?;

    // Past the separator, everything is an operand.
    if let Some(separator) = args.iter().position(|arg| arg == "--")
        && index > separator
    {
        return None;
    }

    // Bound to the option before it, which consumes it verbatim. The rule is [`Consuming`]'s
    // list, not a guess: "the previous token starts with `-`" is the assumption `option` used
    // to make and could not enforce, and an oracle sharing it could not see the misuse — a
    // value bound to a flag that consumes nothing would look shielded to both.
    if index > 0 && Consuming::binds(&args[index - 1]) {
        return None;
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_and_an_operand_land_in_the_order_they_were_added() {
        let mut argv = Argv::new();
        argv.flag("diff");
        argv.flag("--staged");
        argv.operand("rev", "HEAD~1").expect("an ordinary revision");
        assert_eq!(argv.into_args(), ["diff", "--staged", "HEAD~1"]);
    }

    #[test]
    fn a_revision_that_looks_like_a_flag_is_refused() {
        // The vulnerability this module exists for: `git --no-pager diff --output=../x`
        // exits 0 and writes outside the working directory.
        let mut argv = Argv::new();
        let error = argv
            .operand("rev", "--output=../../pwned")
            .expect_err("an operand that begins with `-` is an option, not a value");
        assert_eq!(error.kind(), ErrorKind::PolicyDenied);
        assert!(error.message().contains("rev"), "{error}");
        assert!(error.message().contains("--output"), "{error}");
        assert!(
            argv.into_args().is_empty(),
            "a refused operand must leave nothing behind"
        );
    }

    #[test]
    fn a_bound_value_may_look_like_a_flag() {
        // `git commit -m "--amend"` is a commit with a strange message, not an amend: `-m`
        // consumes the next entry whatever it starts with. Refusing it here would be
        // refusing something that was never dangerous.
        let mut argv = Argv::new();
        argv.flag("commit");
        argv.option(Consuming::MESSAGE, "--amend --author=someone");
        let built = argv.into_args();
        assert_eq!(built, ["commit", "-m", "--amend --author=someone"]);
        assert_eq!(option_exposure(&built, "--amend --author=someone"), None);
    }

    #[test]
    fn the_separator_is_emitted_once_and_paths_always_follow_it() {
        // Whatever order the calls come in. A caller that had to remember `--` is a caller
        // that can forget it, which is how `rev` came to be pushed before one.
        let mut argv = Argv::new();
        argv.flag("log");
        argv.pathspec("src/main.rs");
        argv.flag("--oneline");
        argv.pathspec("docs");
        assert_eq!(
            argv.into_args(),
            ["log", "--oneline", "--", "src/main.rs", "docs"]
        );
    }

    #[test]
    fn a_path_that_looks_like_a_flag_is_a_path() {
        // Past `--` a leading `-` is a filename. Containment for the path itself happened
        // before this, in `fsguard`.
        let mut argv = Argv::new();
        argv.flag("diff");
        argv.pathspec("-rf");
        let built = argv.into_args();
        assert_eq!(built, ["diff", "--", "-rf"]);
        assert_eq!(option_exposure(&built, "-rf"), None);
    }

    #[test]
    fn an_argv_with_no_paths_has_no_separator() {
        let mut argv = Argv::new();
        argv.flag("status");
        assert_eq!(argv.into_args(), ["status"]);
    }

    #[test]
    fn exposure_finds_a_free_standing_option_and_nothing_else() {
        let free = ["diff".to_string(), "--output=x".to_string()];
        assert_eq!(option_exposure(&free, "--output=x"), Some(1));

        // Absent.
        assert_eq!(option_exposure(&free, "--other"), None);
        // Not option-shaped at all.
        assert_eq!(option_exposure(&free, "diff"), None);
        // After a value-carrying option, which consumes nothing.
        let after_equals = [
            "log".to_string(),
            "--max-count=20".to_string(),
            "--output=x".to_string(),
        ];
        assert_eq!(option_exposure(&after_equals, "--output=x"), Some(2));
    }

    #[test]
    fn the_oracle_binds_to_the_declared_options_and_to_nothing_else() {
        // The half that was a guess. Every `Consuming` shields what follows it --
        for consuming in Consuming::ALL {
            let built = [
                "cmd".to_string(),
                consuming.flag().to_string(),
                "--output=x".to_string(),
            ];
            assert_eq!(
                option_exposure(&built, "--output=x"),
                None,
                "`{}` consumes the entry after it",
                consuming.flag()
            );
        }
        // -- and an option-shaped flag that is *not* one does not, however much it looks
        // like a binder. `--staged` takes no operand, so a value placed after it is
        // free-standing and `git` reads it as an option. The old heuristic said `None`
        // here, which is what made the schema walk blind to `option("--staged", …)`.
        let misused = [
            "diff".to_string(),
            "--staged".to_string(),
            "--output=x".to_string(),
        ];
        assert_eq!(option_exposure(&misused, "--output=x"), Some(2));
    }

    #[test]
    fn an_empty_pathspec_is_left_out_rather_than_emitted() {
        // `path: "."` resolves to the workspace root and strips to `""`. `git … -- ""` exits
        // 128 telling the model to use `.`, which is what it wrote. Nothing at all means the
        // same thing an empty pathspec meant: everything.
        let mut argv = Argv::new();
        argv.flag("diff");
        argv.pathspec("");
        assert_eq!(
            argv.into_args(),
            ["diff"],
            "an empty pathspec takes the separator with it"
        );

        let mut with_a_real_one = Argv::new();
        with_a_real_one.flag("log");
        with_a_real_one.pathspec("");
        with_a_real_one.pathspec("src");
        assert_eq!(with_a_real_one.into_args(), ["log", "--", "src"]);
    }

    #[test]
    fn the_program_comes_from_the_tool_and_the_args_from_the_builder() {
        let mut argv = Argv::new();
        argv.flag("-c");
        let spec = argv.into_exec("sh");
        assert_eq!(spec.program, "sh");
        assert_eq!(spec.args, ["-c"]);
        assert!(spec.env.is_empty(), "the sandbox fills this, not the tool");
    }
}
