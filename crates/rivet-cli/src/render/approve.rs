//! The approval prompt for the modes that are not the TUI.
//!
//! # The prompt goes to stderr
//!
//! stdout is the *answer*: `rivet "summarize this" > answer.md` has to produce an answer,
//! not an answer with a question mixed into it. Every other operator-facing line in this
//! binary already goes to stderr, and an approval prompt is the most operator-facing line
//! there is.
//!
//! # A pipe gets no sink at all
//!
//! [`PromptApprover::for_stdin`] returns `None` when stdin is not a terminal. Building one
//! anyway would leave `rivet … < /dev/null` hanging on a read that can never return — which
//! is exactly the failure `--headless` exists to prevent, arriving through a door nobody
//! marked. The host folds the missing sink into `unattended`, so those runs *refuse* the
//! approval instead of waiting for it.

use std::io::{IsTerminal, Write};

use async_trait::async_trait;
use rivet_core::policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink};

/// Asks on stderr and reads one line from stdin.
#[derive(Clone, Copy, Debug)]
pub struct PromptApprover;

impl PromptApprover {
    /// One, if there is a terminal to ask at.
    #[must_use]
    pub fn for_stdin() -> Option<Self> {
        Self::for_terminal(std::io::stdin().is_terminal())
    }

    /// [`PromptApprover::for_stdin`] with the answer supplied, so a test can ask both ways.
    #[must_use]
    pub fn for_terminal(is_terminal: bool) -> Option<Self> {
        is_terminal.then_some(Self)
    }

    /// The prompt text, as one block. Pulled out so a test can read it without a terminal.
    #[must_use]
    pub fn prompt(request: &ApprovalRequest) -> String {
        let keys = if request.allow_remember {
            "[y] allow once  [a] allow for this session  [n] deny"
        } else {
            "[y] allow once  [n] deny"
        };
        format!(
            "\napproval required: {}\n  {}\n  {keys} > ",
            request.reason, request.preview
        )
    }

    /// Map one typed line onto an outcome.
    ///
    /// Anything unrecognized — including end-of-input, which arrives as an empty read — is
    /// a refusal. The alternative is re-prompting a person who has walked away, and the
    /// closed answer is the right default for a question about running `rm -rf`.
    #[must_use]
    pub fn read_answer(line: &str, allow_remember: bool) -> ApprovalOutcome {
        match line.trim() {
            "y" | "Y" => ApprovalOutcome::Approved,
            "a" | "A" if allow_remember => ApprovalOutcome::ApprovedForSession,
            _ => ApprovalOutcome::Denied,
        }
    }
}

#[async_trait]
impl ApprovalSink for PromptApprover {
    async fn request(&self, request: ApprovalRequest) -> rivet_core::Result<ApprovalOutcome> {
        let mut stderr = std::io::stderr();
        let _ = stderr.write_all(Self::prompt(&request).as_bytes());
        let _ = stderr.flush();

        let allow_remember = request.allow_remember;
        // Reading a line parks a thread. On the blocking pool it parks one that is meant to
        // be parked, rather than a runtime worker with the bus pump on it.
        let line = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map(|_| line)
        })
        .await
        .map_err(|e| {
            rivet_core::Error::internal("the approval prompt did not finish").with_cause(e)
        })?
        .unwrap_or_default();

        Ok(Self::read_answer(&line, allow_remember))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivet_core::id::{ApprovalId, SessionId};

    fn ask(allow_remember: bool) -> ApprovalRequest {
        ApprovalRequest {
            id: ApprovalId::new(),
            session_id: SessionId::new(),
            reason: "the command matches the destructive shape `rm -rf`".into(),
            preview: "rm -rf build".into(),
            allow_remember,
            scope_key: "shell:rm".into(),
        }
    }

    #[test]
    fn a_non_tty_stdin_produces_no_sink() {
        // The host turns this into `unattended`, which makes the run refuse rather than
        // wait. A sink built here would hang on a read that can never return.
        assert!(PromptApprover::for_terminal(false).is_none());
        assert!(PromptApprover::for_terminal(true).is_some());
    }

    #[test]
    fn the_prompt_shows_what_will_happen_and_which_keys_answer() {
        let text = PromptApprover::prompt(&ask(true));
        assert!(text.contains("rm -rf build"), "{text}");
        assert!(text.contains("[y] allow once"), "{text}");
        assert!(text.contains("[a] allow for this session"), "{text}");
    }

    #[test]
    fn a_prompt_that_may_not_be_remembered_does_not_offer_the_key() {
        let text = PromptApprover::prompt(&ask(false));
        assert!(text.contains("[y] allow once"), "{text}");
        assert!(!text.contains("this session"), "{text}");
    }

    #[test]
    fn the_three_answers_map_to_the_three_outcomes() {
        assert_eq!(
            PromptApprover::read_answer("y\n", true),
            ApprovalOutcome::Approved
        );
        assert_eq!(
            PromptApprover::read_answer("a\n", true),
            ApprovalOutcome::ApprovedForSession
        );
        assert_eq!(
            PromptApprover::read_answer("n\n", true),
            ApprovalOutcome::Denied
        );
    }

    #[test]
    fn anything_else_is_a_refusal() {
        // Including end of input, which arrives as an empty read. Re-prompting somebody who
        // has walked away is not an improvement on refusing.
        for line in ["", "\n", "maybe", "yes please", "Q"] {
            assert_eq!(
                PromptApprover::read_answer(line, true),
                ApprovalOutcome::Denied,
                "{line:?}"
            );
        }
    }

    #[test]
    fn the_remember_answer_is_inert_when_it_was_not_offered() {
        assert_eq!(
            PromptApprover::read_answer("a", false),
            ApprovalOutcome::Denied,
            "a grant the policy refused to offer is not one a keystroke may take"
        );
    }
}
