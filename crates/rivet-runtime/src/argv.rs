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
//! | [`Argv::option`] | the model | bound to the option before it, which consumes it verbatim |
//! | [`Argv::operand`] | the model | **refused** when it could be read as an option |
//! | [`Argv::pathspec`] | the model | after the `--` separator, which [`Argv`] emits, not the caller |
//!
//! There is no `push`. Adding an argument means picking a door, and every door is closed.
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
    /// The option must be one that actually takes a separate argument. If it is not, the
    /// value becomes free-standing and the child rejects the whole invocation loudly — a
    /// malformed command, not a quiet reinterpretation.
    pub fn option(&mut self, flag: &'static str, value: &str) {
        self.head.push(flag.to_string());
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
        if looks_like_an_option(value) {
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

    // Bound to the option before it, which consumes it verbatim. An option carrying its own
    // value (`--max-count=20`) consumes nothing, so what follows it is free-standing.
    if index > 0 {
        let previous = &args[index - 1];
        if previous.starts_with('-') && previous != "--" && !previous.contains('=') {
            return None;
        }
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
        argv.option("-m", "--amend --author=someone");
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
    fn the_program_comes_from_the_tool_and_the_args_from_the_builder() {
        let mut argv = Argv::new();
        argv.flag("-c");
        let spec = argv.into_exec("sh");
        assert_eq!(spec.program, "sh");
        assert_eq!(spec.args, ["-c"]);
        assert!(spec.env.is_empty(), "the sandbox fills this, not the tool");
    }
}
