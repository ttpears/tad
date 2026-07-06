//! Top-level frame composition: the three-row layout (header / main /
//! status), the two-column main pane (list / preview), and the
//! per-mode footer. Modal overlays are drawn on top by `ui()`.
//!
//! The list here is a temporary flat rendering of `app.rows` via
//! `format::format_row` — Task 6 replaces this with the real sidebar
//! (indentation, collapse carets, state dots, scrolling).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::agents;

use super::modal::{
    render_confirm_kill_modal, render_new_session_modal, render_rename_agent_modal,
    render_snooze_modal,
};
use super::rows::RowKind;
use super::{format, App, InputMode};

pub(super) fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(f, chunks[0], app);
    render_main(f, chunks[1], app);
    render_status(f, chunks[2], app);

    if app.input_mode == InputMode::NewSession {
        render_new_session_modal(f, area, app);
    }
    if app.input_mode == InputMode::SnoozeSelect {
        render_snooze_modal(f, area, app);
    }
    if app.input_mode == InputMode::RenameAgent {
        render_rename_agent_modal(f, area, app);
    }
    if app.input_mode == InputMode::ConfirmKill {
        render_confirm_kill_modal(f, area, app);
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    // Live agent summary so the cockpit's state is visible without
    // scrolling to the Agents section.
    let agents_summary = if app.data.agents.is_empty() {
        "no agents".to_string()
    } else {
        let c = agents::counts(&app.data.agents, std::time::Duration::from_secs(30));
        if c.idle == 0 {
            format!("{} agents", c.total)
        } else if c.active == 0 {
            format!("{} agents idle", c.total)
        } else {
            format!("{}/{} agents active", c.active, c.total)
        }
    };
    let line = Line::from(vec![
        Span::styled(
            " tad ",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(agents_summary, Style::default().fg(app.theme.muted)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border));
    f.render_widget(Paragraph::new(line).block(block), area);
}

/// 40/60 list/preview normally; the list takes everything while a pane
/// is pinned — the real pane sits where the preview was. The preview
/// carries the live content (transcript tail, pane capture) so it gets
/// the bigger share; the list's widest rows (Agents, ~66 cols) still
/// fit at 40% of a wide terminal and clip gracefully on narrow ones.
fn main_constraints(pinned: bool) -> [Constraint; 2] {
    if pinned {
        [Constraint::Percentage(100), Constraint::Percentage(0)]
    } else {
        [Constraint::Percentage(40), Constraint::Percentage(60)]
    }
}

fn render_main(f: &mut Frame, area: Rect, app: &mut App) {
    let pinned = !app.pins.is_empty();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(main_constraints(pinned))
        .split(area);
    render_list(f, chunks[0], app);
    if !pinned {
        render_preview(f, chunks[1], app);
    }
}

fn render_list(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let list_items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| ListItem::new(format::format_row(&app.data, row, theme)))
        .collect();

    let title = if app.input_mode == InputMode::Filter || !app.filter.is_empty() {
        format!(" sidebar — /{} ", app.filter.as_str())
    } else {
        " sidebar ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(title);

    let list = List::new(list_items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(app.theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_preview(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(" preview ");
    let para = Paragraph::new(app.preview_lines())
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let line = match app.input_mode {
        InputMode::Filter => {
            let value = app.filter.as_str();
            let cur = app.filter.cursor.min(value.len());
            let (pre, post) = value.split_at(cur);
            let mut spans = vec![
                Span::styled("/", Style::default().fg(theme.warning)),
                Span::styled(pre.to_string(), Style::default().fg(theme.fg)),
            ];
            if post.is_empty() {
                spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
            } else {
                let mut chars = post.chars();
                let c = chars.next().unwrap_or(' ');
                let rest: String = chars.collect();
                spans.push(Span::styled(
                    c.to_string(),
                    Style::default().bg(theme.accent).fg(theme.fg),
                ));
                spans.push(Span::styled(rest, Style::default().fg(theme.fg)));
            }
            spans.push(Span::styled(
                "    ↑↓ nav  ↵ open  ⇥ section  ^U clear  Esc exit",
                Style::default().fg(theme.muted),
            ));
            Line::from(spans)
        }
        InputMode::NewSession => Line::from(Span::styled(
            "type session name, Enter to create, Esc to cancel",
            Style::default().fg(theme.muted),
        )),
        InputMode::SnoozeSelect => Line::from(Span::styled(
            "↑↓ pick duration   ↵ snooze   Esc cancel",
            Style::default().fg(theme.muted),
        )),
        InputMode::RenameAgent => Line::from(Span::styled(
            "type new window name   ↵ rename   Esc cancel",
            Style::default().fg(theme.muted),
        )),
        InputMode::ConfirmKill => Line::from(Span::styled(
            "y/↵ confirm kill   Esc/n cancel",
            Style::default().fg(theme.muted),
        )),
        InputMode::None => {
            // A transient flash (guard refusal) owns the whole line
            // until the next keypress clears it.
            if let Some(msg) = &app.flash {
                Line::from(Span::styled(
                    msg.clone(),
                    Style::default().fg(theme.warning),
                ))
            } else {
                let bind = |key: &str, label: &str| -> Vec<Span<'static>> {
                    vec![
                        Span::styled(format!("{} ", key), Style::default().fg(theme.accent)),
                        Span::styled(format!("{}  ", label), Style::default().fg(theme.fg)),
                    ]
                };
                let mut spans = Vec::new();
                if let Some(p) = app.pins.first() {
                    spans.push(Span::styled(
                        format!("◀ {} pinned  ", p.label),
                        Style::default()
                            .fg(theme.accent_bold)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.extend(bind("o", "return"));
                }
                let kind = app.selected_row().map(|r| r.kind.clone());
                let pullable = matches!(kind, Some(RowKind::Session(_)) | Some(RowKind::Agent(_)));
                spans.extend(bind("↑↓/jk", "nav"));
                spans.extend(bind("⇥", "section"));
                spans.extend(bind("1/2/3/4", "jump"));
                spans.extend(bind("↵", "open"));
                spans.extend(bind("n", "new"));
                if app.pins.is_empty() && pullable {
                    spans.extend(bind("o", "pull"));
                }
                if matches!(kind, Some(RowKind::Session(_))) {
                    spans.extend(bind("d", "kill"));
                }
                if matches!(kind, Some(RowKind::Agent(_))) {
                    spans.extend(bind("d", "kill"));
                    spans.extend(bind("R", "rename"));
                    spans.extend(bind("s", "snooze"));
                }
                spans.extend(bind("/", "filter"));
                spans.extend(bind("r", "refresh"));
                spans.extend(bind("q", "quit"));
                Line::from(spans)
            }
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_takes_everything_while_a_pane_is_pinned() {
        assert_eq!(
            main_constraints(true),
            [Constraint::Percentage(100), Constraint::Percentage(0)]
        );
        assert_eq!(
            main_constraints(false),
            [Constraint::Percentage(40), Constraint::Percentage(60)]
        );
    }
}
