//! Three regions: the job panel, the agent panel, the status bar.
//!
//! Drawing is a function of [`AppState`] and nothing else, so a `TestBackend` snapshot is
//! the whole truth about what a terminal would show.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::state::{AppState, ApprovalView, Panel, ToolStatus};

/// What the job panel says while there is nothing to show.
///
/// A named constant because a test asserts on it: an empty panel that says nothing looks
/// broken, and "the job runtime lands in Phase 5" is the difference between a missing
/// feature and a bug.
pub const NO_JOBS: &str = "no jobs — the job runtime lands in Phase 5";

/// Render the whole screen.
pub fn draw(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(rows[0]);

    draw_jobs(frame, panels[0], state);
    draw_agent(frame, panels[1], state);
    draw_status(frame, rows[1], state);
    // Last, and over everything: an approval is the one thing on this screen the run is
    // actually waiting on.
    if let Some(pending) = &state.pending {
        draw_approval(frame, area, pending);
    }
}

/// The keys an approval prompt offers, in the order it lists them.
///
/// A named constant because a test asserts on it and because the three map onto the three
/// [`rivet_core::policy::ApprovalOutcome`] variants a person can produce — `a` is offered
/// only when the policy said the grant may be remembered.
pub const APPROVAL_KEYS: &str = "[y] allow once   [a] allow for this session   [n] deny";

/// The same, for a policy that did not offer to remember the grant.
pub const APPROVAL_KEYS_ONCE: &str = "[y] allow once   [n] deny";

/// The modal, centred over whatever was underneath.
fn draw_approval(frame: &mut Frame<'_>, area: Rect, pending: &ApprovalView) {
    let width = area.width.saturating_sub(4).clamp(20, 72);
    let height = area.height.saturating_sub(4).clamp(5, 9);
    let modal = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let keys = if pending.allow_remember {
        APPROVAL_KEYS
    } else {
        APPROVAL_KEYS_ONCE
    };
    let lines = vec![
        Line::from(Span::styled(
            pending.reason.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(pending.preview.clone()),
        Line::from(""),
        Line::from(keys),
    ];

    // `Clear` first: the panels underneath have already been drawn into these cells.
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Approval required")
                    .border_style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .wrap(Wrap { trim: true }),
        modal,
    );
}

fn panel_style(state: &AppState, panel: Panel) -> Style {
    if state.focus == panel {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn draw_jobs(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Jobs")
        .border_style(panel_style(state, Panel::Jobs));

    let lines: Vec<Line<'_>> = if state.jobs.jobs.is_empty() {
        vec![Line::from(NO_JOBS)]
    } else {
        state
            .jobs
            .jobs
            .iter()
            .flat_map(|job| {
                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        format!("{:<10}", job.state),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(job.goal.clone()),
                ])];
                if !job.runs.is_empty() {
                    lines.push(Line::from(format!("  runs: {}", job.runs.join(", "))));
                }
                if let Some(verdict) = &job.verdict {
                    lines.push(Line::from(format!("  review: {verdict}")));
                }
                lines
            })
            .collect()
    };

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_agent(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Agent · turn {}", state.run.turn))
        .border_style(panel_style(state, Panel::Agent));

    let mut lines: Vec<Line<'_>> = Vec::new();
    if state.run.text_dropped > 0 {
        lines.push(Line::from(format!(
            "… {} character(s) elided",
            state.run.text_dropped
        )));
    }
    for line in state.run.text.lines() {
        lines.push(Line::from(line.to_string()));
    }
    if !state.run.tools.is_empty() || state.run.tools_dropped > 0 {
        lines.push(Line::from(""));
    }
    if state.run.tools_dropped > 0 {
        lines.push(Line::from(format!(
            "… {} tool call(s) elided",
            state.run.tools_dropped
        )));
    }
    for tool in &state.run.tools {
        let mut span = format!("→ {} [{}]", tool.name, tool.status.label());
        if let Some(ms) = tool.duration_ms {
            let _ = write!(span, " {ms}ms");
        }
        if let Some(progress) = &tool.progress {
            let _ = write!(span, " — {progress}");
        }
        lines.push(Line::from(Span::styled(
            span,
            match tool.status {
                ToolStatus::Failed | ToolStatus::Blocked => {
                    Style::default().add_modifier(Modifier::BOLD)
                }
                _ => Style::default(),
            },
        )));
    }
    if state.run.retries > 0 {
        lines.push(Line::from(format!("retried {}×", state.run.retries)));
    }
    if let Some(error) = &state.run.last_error {
        lines.push(Line::from(format!("! {error}")));
    }
    if let Some(stop) = &state.run.stop {
        lines.push(Line::from(format!("stopped: {stop}")));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    frame.render_widget(
        Paragraph::new(Line::from(status_line(state, area.width))),
        area,
    );
}

/// The separator between status segments.
const SEP: &str = " · ";

/// The status bar's text, fitted to `width`.
///
/// Pulled out of the drawing so a test can read it without a backend — and so the rule the
/// crate is built on stays checkable: **every field here comes from an event.** The profile
/// name and the workspace root are not on this line because no event carries them, and the
/// answer to wanting them is another event, not an import.
///
/// Fitting drops whole segments rather than truncating the line, because a status bar cut
/// mid-word ("… 0 pl") tells the reader nothing and hides that anything was cut. Each
/// segment carries a rank; the ones that survive a narrow terminal are the ones a user
/// cannot work without — which turn it is, what it cost, and how to get out.
#[must_use]
pub fn status_line(state: &AppState, width: u16) -> String {
    let short = |value: &Option<String>| -> String {
        value.as_deref().map_or_else(
            || "—".to_string(),
            |id| id.chars().take(12).collect::<String>(),
        )
    };

    // (rank, text), in display order. Lower rank survives longer.
    let mut segments: Vec<(u8, String)> = vec![
        (7, short(&state.status.session)),
        (6, short(&state.status.run)),
        (4, state.status.model.as_deref().unwrap_or("—").to_string()),
        (0, format!("turn {}", state.status.turn)),
        (
            1,
            format!(
                "{}↑/{}↓ tok",
                state.status.input_tokens, state.status.output_tokens
            ),
        ),
        (2, format!("{} tool", state.status.tool_calls)),
        (8, format!("{} plugin", state.status.plugins)),
    ];
    if state.status.dropped > 0 {
        // "at least": a subscriber far enough behind can miss its own lag report, so this
        // is a floor rather than a total. `docs/events.md` §5 says the same thing.
        segments.push((3, format!("≥{} dropped", state.status.dropped)));
    }
    if state.status.dropped_elsewhere > 0 {
        // Somebody else's losses, said in the third person and ranked to go first when the
        // bar has to shed segments. Summed into the one above, it read as "this screen
        // missed 42 events" when the screen had received every one of them.
        segments.push((9, format!("{} elsewhere", state.status.dropped_elsewhere)));
    }
    if state.status.shutting_down {
        segments.push((3, "shutting down".to_string()));
    }
    segments.push((5, "[q] quit  [c] cancel  [tab] panel".to_string()));

    fit(segments, width as usize)
}

/// Join what fits, keeping display order and dropping by rank.
fn fit(segments: Vec<(u8, String)>, width: usize) -> String {
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by_key(|index| segments[*index].0);

    let mut keep = vec![false; segments.len()];
    let mut used = 0usize;
    for index in order {
        let len = segments[index].1.chars().count();
        let cost = if used == 0 {
            len
        } else {
            len + SEP.chars().count()
        };
        if used + cost > width {
            continue;
        }
        used += cost;
        keep[index] = true;
    }

    segments
        .into_iter()
        .enumerate()
        .filter(|(index, _)| keep[*index])
        .map(|(_, (_, text))| text)
        .collect::<Vec<_>>()
        .join(SEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_status_bar_keeps_the_segments_a_user_cannot_work_without() {
        let mut state = AppState::default();
        state.status.model = Some("openai/gpt-4o".to_string());
        state.status.turn = 2;

        let narrow = status_line(&state, 80);
        assert!(narrow.chars().count() <= 80, "{narrow}");
        assert!(narrow.contains("turn 2"), "{narrow}");
        assert!(narrow.contains("[q] quit"), "{narrow}");
        assert!(
            !narrow.contains(" pl"),
            "a segment was cut mid-word instead of dropped: {narrow}"
        );
    }

    #[test]
    fn a_wide_status_bar_keeps_everything() {
        let mut state = AppState::default();
        state.status.session = Some("ses_1".to_string());
        state.status.plugins = 3;
        let wide = status_line(&state, 200);
        assert!(wide.contains("3 plugin"), "{wide}");
        assert!(wide.contains("ses_1"), "{wide}");
    }
}
