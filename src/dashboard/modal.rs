//! Modal overlays: new-session, snooze-picker, rename-agent,
//! confirm-kill, theme-picker. Render functions over `&mut App` — the
//! `&mut` is only so each can register its own `Hit::Modal` click
//! regions as it draws (see `hit::HitMap`); none of them otherwise
//! mutate `App`. The corresponding `handle_*_key` lives in `keys.rs`
//! and they coordinate via `app.input_mode`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::{snooze, theme};

use super::action::Action;
use super::hit::Hit;
use super::rows::RowKind;
use super::{App, ConfirmKillTarget, NewSessionField, TextInput};

/// Register `Hit::Modal(action)` over the substring `needle` within
/// `text`, the exact string rendered on `Paragraph` line `content_line`
/// (0-based index into the `lines` vec — this adds the top-border
/// offset). Walks `text` char-by-char to find `needle`'s column offset
/// rather than hardcoding one, so it stays correct if the copy changes.
/// A silent no-op if `needle` isn't found (defensive; every call site
/// passes a substring of its own hint text).
fn register_hint_hit(
    app: &mut App,
    popup: Rect,
    content_line: u16,
    text: &str,
    needle: &str,
    action: Action,
) {
    let Some(byte_off) = text.find(needle) else {
        return;
    };
    let col = text[..byte_off].chars().count() as u16;
    let width = needle.chars().count() as u16;
    let rect = Rect {
        x: popup.x + 1 + col,
        y: popup.y + 1 + content_line,
        width,
        height: 1,
    };
    app.hits.register(rect, Hit::Modal(action));
}

/// Register a `Hit::Modal(action)` over an entire option row (theme
/// name / snooze duration). `content_line` is that row's 0-based index
/// into the `Paragraph`'s own `lines` vec — this adds the top-border
/// offset so callers don't have to.
fn register_row_hit(app: &mut App, popup: Rect, content_line: u16, action: Action) {
    let rect = Rect {
        x: popup.x + 1,
        y: popup.y + 1 + content_line,
        width: popup.width.saturating_sub(2),
        height: 1,
    };
    app.hits.register(rect, Hit::Modal(action));
}

pub(super) fn render_rename_agent_modal(f: &mut Frame, area: Rect, app: &mut App) {
    let target = app.rename_agent_target.clone().unwrap_or_default();
    let width = 70.min(area.width.saturating_sub(4));
    let height = 7;
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" rename window for {target} "),
            Style::default()
                .fg(theme.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));

    let field = &app.rename_agent_text;
    let value = field.as_str();
    let mut spans = vec![Span::styled(
        format!("  {:<6}", "name:"),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if value.is_empty() {
        spans.push(Span::styled(
            "(required)".to_string(),
            Style::default().fg(theme.muted),
        ));
        spans.push(Span::styled("▏", Style::default().fg(theme.accent)));
    } else {
        // Pristine values (prefilled current name) get muted/italic so
        // the user knows the first keystroke replaces them — same as
        // the Hosts-view new-session prefill.
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
            let after: String = chars.collect();
            spans.push(Span::styled(pre.to_string(), value_style));
            spans.push(Span::styled(
                cursor_char.to_string(),
                Style::default().bg(theme.accent).fg(theme.fg),
            ));
            spans.push(Span::styled(after, value_style));
        }
    }

    const HINT: &str = "  ↵ rename   Esc cancel   (renames the tmux window, not the session)";
    let lines = vec![
        Line::from(""),
        Line::from(spans),
        Line::from(""),
        Line::from(Span::styled(HINT, Style::default().fg(theme.muted))),
    ];
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);

    register_hint_hit(app, popup, 3, HINT, "↵ rename", Action::ModalConfirm);
    register_hint_hit(app, popup, 3, HINT, "Esc cancel", Action::ModalCancel);
}

pub(super) fn render_snooze_modal(f: &mut Frame, area: Rect, app: &mut App) {
    // Snooze durations come from the per-refresh `data.ui` cache (which
    // pulled them from config.yaml once), not a fresh file read.
    let intervals = &app.data.ui.snooze_intervals;
    let target = match app.selected_row().map(|r| &r.kind) {
        Some(RowKind::Agent(t)) => t.clone(),
        _ => String::new(),
    };
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
    const HINT: &str = "  ↑↓ pick   ↵ snooze   Esc cancel";
    let interval_count = intervals.len();
    let mut lines: Vec<Line> = Vec::with_capacity(interval_count + 4);
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
        HINT,
        Style::default().fg(theme.muted),
    )));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);

    for i in 0..interval_count {
        register_row_hit(app, popup, 1 + i as u16, Action::SnoozeOption(i));
    }
    let hint_line = 2 + interval_count as u16;
    register_hint_hit(
        app,
        popup,
        hint_line,
        HINT,
        "↵ snooze",
        Action::ModalConfirm,
    );
    register_hint_hit(
        app,
        popup,
        hint_line,
        HINT,
        "Esc cancel",
        Action::ModalCancel,
    );
}

pub(super) fn render_new_session_modal(f: &mut Frame, area: Rect, app: &mut App) {
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

    const HINT: &str = "  Tab/↑↓ field  ←→ cursor  ^U clear  ↵ create  Esc cancel";
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
        Line::from(Span::styled(HINT, Style::default().fg(app.theme.muted))),
    ];

    let inner = Paragraph::new(lines).block(block);
    f.render_widget(inner, popup);

    register_hint_hit(app, popup, 4, HINT, "↵ create", Action::ModalConfirm);
    register_hint_hit(app, popup, 4, HINT, "Esc cancel", Action::ModalCancel);
}

pub(super) fn render_confirm_kill_modal(f: &mut Frame, area: Rect, app: &mut App) {
    // Defensive: input_mode and confirm_kill are always set together,
    // so this never fires; render nothing rather than a blank modal.
    let Some(target) = &app.confirm_kill else {
        return;
    };
    let (title, question) = match target {
        ConfirmKillTarget::Session { name } => (
            " kill session ",
            format!("  Kill session {name}? This closes every pane in it."),
        ),
        ConfirmKillTarget::Agent { window_name, .. } => (
            " interrupt agent ",
            format!("  Interrupt agent {window_name}? Sends SIGINT to the agent."),
        ),
    };
    let width = 70.min(area.width.saturating_sub(4));
    let height = 7;
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.error))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    const HINT: &str = "  y/↵ confirm   Esc/n cancel   (any other key cancels)";
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(question, Style::default().fg(theme.fg))),
        Line::from(""),
        Line::from(Span::styled(HINT, Style::default().fg(theme.muted))),
    ];
    f.render_widget(Paragraph::new(lines).block(block), popup);

    // The whole "y/↵ confirm" chip — not just the `y` — is the
    // clickable confirm region.
    register_hint_hit(app, popup, 3, HINT, "y/↵ confirm", Action::ModalConfirm);
    register_hint_hit(app, popup, 3, HINT, "Esc/n cancel", Action::ModalCancel);
}

/// `InputMode::ThemeSelect` — same layout as `render_snooze_modal`:
/// one row per `theme::builtin_names()` entry, current theme's row
/// marked with the cursor arrow (which, live-applied by
/// `keys::handle_theme_key`/`Action::ThemeOption`, is always the
/// currently-active theme while the picker is open).
pub(super) fn render_theme_modal(f: &mut Frame, area: Rect, app: &mut App) {
    let names = theme::builtin_names();
    let width = 56.min(area.width.saturating_sub(4));
    let height = (names.len() as u16 + 5).min(area.height.saturating_sub(2));
    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);
    let palette = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.accent))
        .title(Span::styled(
            " theme ",
            Style::default()
                .fg(palette.accent_bold)
                .add_modifier(Modifier::BOLD),
        ));
    const HINT: &str = "  ↑↓ pick   ↵ confirm   Esc cancel";
    let mut lines: Vec<Line> = Vec::with_capacity(names.len() + 4);
    lines.push(Line::from(""));
    for (i, name) in names.iter().enumerate() {
        let selected = i == app.theme_cursor;
        let marker = if selected { "▶ " } else { "  " };
        let label_style = if selected {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.fg)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", marker), label_style),
            Span::styled(name.to_string(), label_style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        HINT,
        Style::default().fg(palette.muted),
    )));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);

    for (i, _) in names.iter().enumerate() {
        register_row_hit(app, popup, 1 + i as u16, Action::ThemeOption(i));
    }
    let hint_line = 2 + names.len() as u16;
    register_hint_hit(
        app,
        popup,
        hint_line,
        HINT,
        "↵ confirm",
        Action::ModalConfirm,
    );
    register_hint_hit(
        app,
        popup,
        hint_line,
        HINT,
        "Esc cancel",
        Action::ModalCancel,
    );
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
