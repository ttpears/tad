//! Top-level frame composition: the three-row layout (tabs / main /
//! status), the two-column main pane (list / preview), and the
//! per-mode footer. Modal overlays are drawn on top by `ui()`.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::agents;

use super::format::{format_agent_line, format_group_line, format_host_line, format_session_line};
use super::modal::{
    render_confirm_kill_modal, render_new_session_modal, render_rename_agent_modal,
    render_snooze_modal,
};
use super::{App, InputMode, View};

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

    render_tabs(f, chunks[0], app);
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

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    // The Agents tab is annotated with a live count so you can see the cockpit's state from any view.
    let agents_title = if app.data.agents.is_empty() {
        "Agents".to_string()
    } else {
        let c = agents::counts(&app.data.agents, std::time::Duration::from_secs(30));
        if c.idle == 0 {
            format!("Agents ({})", c.total)
        } else if c.active == 0 {
            format!("Agents ({} idle)", c.total)
        } else {
            format!("Agents ({}/{})", c.active, c.total)
        }
    };
    let titles: Vec<Line> = vec![
        Line::from(View::Sessions.title()),
        Line::from(View::Groups.title()),
        Line::from(View::Hosts.title()),
        Line::from(agents_title),
    ];
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border))
                .title(" tad "),
        )
        .select(app.view.index())
        .style(Style::default().fg(app.theme.muted))
        .highlight_style(
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
}

fn render_main(f: &mut Frame, area: Rect, app: &mut App) {
    // The preview carries the live content (transcript tail, pane
    // capture) so it gets the bigger share; the list's widest rows
    // (Agents, ~66 cols) still fit at 40% of a wide terminal and clip
    // gracefully on narrow ones.
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    render_list(f, chunks[0], app);
    render_preview(f, chunks[1], app);
}

fn render_list(f: &mut Frame, area: Rect, app: &mut App) {
    let items_strs = app.items();
    let theme = &app.theme;
    let list_items: Vec<ListItem> = items_strs
        .iter()
        .map(|name| {
            let line = match app.view {
                View::Sessions => format_session_line(&app.data, name, theme),
                View::Groups => format_group_line(&app.data, name, theme),
                View::Hosts => format_host_line(&app.data, name, theme),
                View::Agents => format_agent_line(&app.data, name, theme),
            };
            ListItem::new(line)
        })
        .collect();

    let title = if app.input_mode == InputMode::Filter || !app.filter.is_empty() {
        format!(" {} — /{} ", app.view.title(), app.filter.as_str())
    } else {
        // For Agents view, items_strs includes synthetic session-header
        // separators — count the real agents from data, not items, so
        // the title doesn't claim "Agents (15)" when actually 8.
        let count = match app.view {
            super::View::Agents => app.data.agents.len(),
            _ => items_strs.len(),
        };
        format!(" {} ({}) ", app.view.title(), count)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(title);

    // Empty view: instead of a blank box, show a muted hint. The Groups
    // hint points at `tad config` — the wizard is opt-in, so this is the
    // main breadcrumb to it for someone who hasn't set up any groups.
    if list_items.is_empty() && app.input_mode != InputMode::Filter && app.filter.is_empty() {
        let hint = match app.view {
            View::Groups => vec![
                Line::from(Span::styled(
                    "No groups yet.",
                    Style::default().fg(app.theme.fg),
                )),
                Line::from(Span::styled(
                    "Run `tad config` to set up groups.",
                    Style::default().fg(app.theme.muted),
                )),
            ],
            View::Sessions => vec![Line::from(Span::styled(
                "No tmux sessions. Press n to start one.",
                Style::default().fg(app.theme.muted),
            ))],
            View::Agents => vec![Line::from(Span::styled(
                "No Claude agents running.",
                Style::default().fg(app.theme.muted),
            ))],
            View::Hosts => vec![Line::from(Span::styled(
                "No hosts. Add groups with `tad config`.",
                Style::default().fg(app.theme.muted),
            ))],
        };
        f.render_widget(
            Paragraph::new(hint).block(block).wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let list = List::new(list_items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(app.theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let state = match app.view {
        View::Sessions => &mut app.list_state_sessions,
        View::Groups => &mut app.list_state_groups,
        View::Hosts => &mut app.list_state_hosts,
        View::Agents => &mut app.list_state_agents,
    };
    f.render_stateful_widget(list, area, state);
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
                "    ↑↓ nav  ↵ open  ⇥ view  ^U clear  Esc exit",
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
            let bind = |key: &str, label: &str| -> Vec<Span<'static>> {
                vec![
                    Span::styled(format!("{} ", key), Style::default().fg(theme.accent)),
                    Span::styled(format!("{}  ", label), Style::default().fg(theme.fg)),
                ]
            };
            let mut spans = Vec::new();
            spans.extend(bind("↑↓/jk", "nav"));
            spans.extend(bind("⇥", "view"));
            spans.extend(bind("1/2/3/4", "jump"));
            spans.extend(bind("↵", "open"));
            spans.extend(bind("n", "new"));
            if app.view == View::Sessions {
                spans.extend(bind("d", "kill"));
            }
            if app.view == View::Agents {
                spans.extend(bind("d", "kill"));
                spans.extend(bind("R", "rename"));
                spans.extend(bind("s", "snooze"));
            }
            spans.extend(bind("/", "filter"));
            spans.extend(bind("r", "refresh"));
            spans.extend(bind("q", "quit"));
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}
