//! The approval modal, drawn from state.
//!
//! This file holds the *drawing* half: a hand-made `AppState` is the whole input, exactly
//! as it is for every other panel. The key handling and the round trip live next to
//! `handle_key` in `app.rs`, where they do not need the key entry point to be public.

use rivet_tui::{AppState, ApprovalView, draw};

/// What a terminal would show for `state`.
fn rendered(state: &AppState) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("test backend");
    terminal
        .draw(|frame| draw(frame, state))
        .expect("the test backend does not fail");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn an_approval_modal_is_drawn_from_state_alone() {
    // No bus, no runtime, no channel: a hand-made `AppState` is the whole input, the same
    // property every other panel has.
    let mut state = AppState::default();
    assert!(
        !rendered(&state).contains("Approval required"),
        "nothing is pending"
    );

    state.pending = Some(ApprovalView {
        reason: "the command matches the destructive shape `rm -rf`".into(),
        preview: "rm -rf build".into(),
        allow_remember: true,
    });
    let screen = rendered(&state);
    assert!(screen.contains("Approval required"), "{screen}");
    assert!(screen.contains("rm -rf build"), "{screen}");
    assert!(screen.contains("[y] allow once"), "{screen}");
    assert!(
        screen.contains("[a] allow for this session"),
        "the policy offered to remember it: {screen}"
    );
}

#[test]
fn a_policy_that_will_not_be_remembered_does_not_offer_the_key() {
    // The shell gate is never rememberable, and a prompt that offered `a` anyway would be
    // offering something the policy refused to give.
    let state = AppState {
        pending: Some(ApprovalView {
            reason: "destructive".into(),
            preview: "rm -rf build".into(),
            allow_remember: false,
        }),
        ..AppState::default()
    };
    let screen = rendered(&state);
    assert!(screen.contains("[y] allow once"), "{screen}");
    assert!(!screen.contains("this session"), "{screen}");
}
