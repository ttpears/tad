//! Modal overlays: new-session, snooze-picker, new-agent. Pure render
//! functions over `&App` — the corresponding `handle_*_key` lives in
//! `keys.rs` and they coordinate via `app.input_mode`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::snooze;

use super::{App, NewSessionField, TextInput};

pub(super) fn render_new_agent_modal(f: &mut Frame, area: Rect, app: &App) {
    let project = app.new_agent_project.clone().unwrap_or_default();
    let width = 78.min(area.width.saturating_sub(4));
    let height = 7;
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" new agent in {project} "),
            Style::default()
                .fg(theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));
    // Render the prompt as a single field. Empty = "just spawn claude
    // with no prompt"; non-empty = "spawn claude with this as the
    // initial message."
    let value = app.new_agent_prompt.as_str();
    let cur = app.new_agent_prompt.cursor.min(value.len());
    let (pre, post) = value.split_at(cur);
    let mut field_spans = vec![Span::styled(
        format!("  {:<8}", "prompt:"),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if value.is_empty() {
        field_spans.push(Span::styled(
            "(empty — just spawn claude)".to_string(),
            Style::default().fg(theme.muted),
        ));
        field_spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
    } else if post.is_empty() {
        field_spans.push(Span::styled(pre.to_string(), Style::default().fg(theme.fg)));
        field_spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
    } else {
        let mut chars = post.chars();
        let cursor_char = chars.next().unwrap_or(' ');
        let after: String = chars.collect();
        field_spans.push(Span::styled(pre.to_string(), Style::default().fg(theme.fg)));
        field_spans.push(Span::styled(
            cursor_char.to_string(),
            Style::default().bg(theme.accent).fg(theme.fg),
        ));
        field_spans.push(Span::styled(after, Style::default().fg(theme.fg)));
    }
    let lines = vec![
        Line::from(""),
        Line::from(field_spans),
        Line::from(""),
        Line::from(Span::styled(
            "  ↵ spawn   Esc cancel   (claude opens in a new window in the project's root)"
                .to_string(),
            Style::default().fg(theme.muted),
        )),
    ];
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);
}

pub(super) fn render_snooze_modal(f: &mut Frame, area: Rect, app: &App) {
    // Snooze durations come from the per-refresh `data.ui` cache (which
    // pulled them from config.yaml once), not a fresh file read.
    let intervals = &app.data.ui.snooze_intervals;
    let target = app.selected().unwrap_or_default();
    let width = 56.min(area.width.saturating_sub(4));
    let height = (intervals.len() as u16 + 5).min(area.height.saturating_sub(2));
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" snooze {} ", target),
            Style::default()
                .fg(theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));
    let mut lines: Vec<Line> = Vec::with_capacity(intervals.len() + 4);
    lines.push(Line::from(""));
    for (i, dur) in intervals.iter().enumerate() {
        let selected = i == app.snooze_cursor;
        let marker = if selected { "▶ " } else { "  " };
        let label_style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", marker), label_style),
            Span::styled(snooze::format_duration(*dur), label_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ pick   ↵ snooze   Esc cancel".to_string(),
        Style::default().fg(theme.muted),
    )));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);
}

pub(super) fn render_new_session_modal(f: &mut Frame, area: Rect, app: &App) {
    let width = 70.min(area.width.saturating_sub(4));
    let height = 7;
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent))
        .title(Span::styled(
            " new session ",
            Style::default()
                .fg(app.theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));

    let active = app.new_session_field;
    let theme = app.theme;
    let field_line =
        move |label: &str, field: &TextInput, active: bool, placeholder: &str| -> Line<'static> {
            let label_style = if active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            let mut spans = vec![Span::styled(format!("  {:<6}", label), label_style)];
            let value = field.as_str();
            if value.is_empty() {
                if active {
                    spans.push(Span::styled(
                        placeholder.to_string(),
                        Style::default().fg(theme.muted),
                    ));
                    spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
                } else {
                    spans.push(Span::styled(
                        placeholder.to_string(),
                        Style::default().fg(theme.muted),
                    ));
                }
            } else if !active {
                spans.push(Span::styled(
                    value.to_string(),
                    Style::default().fg(theme.fg),
                ));
            } else {
                // Active field with content. Pristine values get muted/italic so
                // the user knows the next keystroke will replace them. Otherwise
                // render with a block cursor at the cursor position.
                let value_style = if field.pristine {
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC)
                } else {
                    Style::default().fg(theme.fg)
                };
                let cur = field.cursor.min(value.len());
                let (pre, post) = value.split_at(cur);
                if field.pristine {
                    spans.push(Span::styled(value.to_string(), value_style));
                    spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
                } else if post.is_empty() {
                    spans.push(Span::styled(pre.to_string(), value_style));
                    spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
                } else {
                    let mut chars = post.chars();
                    let cursor_char = chars.next().unwrap_or(' ');
                    let after_cursor: String = chars.collect();
                    spans.push(Span::styled(pre.to_string(), value_style));
                    spans.push(Span::styled(
                        cursor_char.to_string(),
                        Style::default().bg(theme.accent).fg(theme.fg),
                    ));
                    spans.push(Span::styled(after_cursor, value_style));
                }
            }
            Line::from(spans)
        };

    let lines = vec![
        Line::from(""),
        field_line(
            "name:",
            &app.new_session_name,
            active == NewSessionField::Name,
            "(required)",
        ),
        field_line(
            "ssh:",
            &app.new_session_host,
            active == NewSessionField::Host,
            "(optional — blank = no ssh)",
        ),
        Line::from(""),
        Line::from(Span::styled(
            "  Tab/↑↓ field  ←→ cursor  ^U clear  ↵ create  Esc cancel",
            Style::default().fg(app.theme.muted),
        )),
    ];

    let inner = Paragraph::new(lines).block(block);
    f.render_widget(inner, popup);
}

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
