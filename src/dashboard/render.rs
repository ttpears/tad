//! Sidebar cockpit rendering: the terminal-size-driven layout
//! (`layout_mode`), the sidebar itself (rows, section headers, empty-
//! section hints, scroll-to-cursor, the pin/cursor gutter), the live
//! area (preview pane, or nothing while a pane is pinned beside tad),
//! and a discrete-chip footer. Modal overlays are drawn on top by
//! `ui()`.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;

use super::action::Action;
use super::hit::Hit;
use super::modal::{
    render_confirm_kill_modal, render_new_session_modal, render_rename_agent_modal,
    render_snooze_modal, render_theme_modal,
};
use super::rows::{RowKind, Section};
use super::{format, App, InputMode};

/// Terminal-size buckets driving the cockpit's layout — pure function
/// of `(width, height)` so it's unit-testable without a real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutMode {
    /// Too small to render anything useful: a centered notice only.
    TooSmall,
    /// Sidebar hidden by default (a `☰` chip hints it's there);
    /// `app.sidebar_overlay` draws it full-screen on demand.
    Narrow,
    /// Sidebar (fixed width) beside the live area.
    Full,
}

pub(super) fn layout_mode(w: u16, h: u16) -> LayoutMode {
    if w < 40 || h < 10 {
        LayoutMode::TooSmall
    } else if w < 70 {
        LayoutMode::Narrow
    } else {
        LayoutMode::Full
    }
}

/// Keep `cursor` visible in a `viewport`-row window: scrolls the
/// minimum amount so `cursor` is neither above `scroll` nor at/past
/// `scroll + viewport`. A no-op when it's already visible.
pub(super) fn scroll_to_cursor(cursor: usize, scroll: usize, viewport: usize) -> usize {
    if viewport == 0 {
        return scroll;
    }
    if cursor < scroll {
        cursor
    } else if cursor >= scroll + viewport {
        cursor + 1 - viewport
    } else {
        scroll
    }
}

pub(super) fn ui(f: &mut Frame, app: &mut App) {
    // Rebuilt fresh every frame — nothing about mouse hit-testing is
    // stale across renders.
    app.hits.clear();

    let area = f.area();
    let mode = layout_mode(area.width, area.height);
    if mode == LayoutMode::TooSmall {
        render_too_small(f, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let main_area = chunks[0];
    let footer_area = chunks[1];

    let pinned = !app.pins.is_empty();
    if pinned {
        // tad's own pane IS the sidebar now — the pulled pane sits
        // beside it in a real tmux split, so there's nothing of ours
        // to draw in a "live area".
        render_sidebar(f, main_area, app);
    } else if mode == LayoutMode::Narrow {
        if app.sidebar_overlay {
            // Overlay covers the whole main area — nothing of the live
            // area would show through it anyway.
            render_sidebar(f, main_area, app);
        } else {
            render_preview(f, main_area, app);
            render_menu_chip(f, main_area, app);
        }
    } else {
        let sidebar_w = app
            .sidebar_width
            .min(main_area.width.saturating_sub(1))
            .max(1);
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar_w), Constraint::Min(1)])
            .split(main_area);
        render_sidebar(f, split[0], app);
        render_preview(f, split[1], app);
        // The draggable column between them — registered last so it
        // wins over the SidebarZone/PreviewZone hits either side of it
        // register on their own edge column.
        let divider_rect = Rect {
            x: split[0].x + split[0].width.saturating_sub(1),
            y: main_area.y,
            width: 1,
            height: main_area.height,
        };
        app.hits.register(divider_rect, Hit::Divider);
    }

    render_footer(f, footer_area, app);

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
    if app.input_mode == InputMode::ThemeSelect {
        render_theme_modal(f, area, app);
    }
}

fn render_too_small(f: &mut Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    let para = Paragraph::new("tad needs at least 40x10 to render").alignment(Alignment::Center);
    f.render_widget(para, rows[1]);
}

/// Small top-left hint that the sidebar exists but is hidden — the
/// only affordance in narrow, non-overlay mode. Also the one way to
/// open it by mouse: clicking it toggles the overlay.
fn render_menu_chip(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let chip_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.min(3),
        height: 1,
    };
    app.hits
        .register(chip_area, Hit::Click(Action::ToggleOverlay));
    let para = Paragraph::new(Span::styled(
        "☰",
        Style::default()
            .fg(app.theme.accent_bold)
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(para, chip_area);
}

/// Hint text for a section that's expanded (not manually collapsed)
/// but has zero items — rendered directly by the sidebar, not a `Row`
/// (not selectable, no cursor position).
fn empty_hint_text(section: Section) -> &'static str {
    match section {
        Section::Sessions => "no sessions — n starts one",
        Section::Groups => "no groups — run `tad config`",
        Section::Hosts => "no hosts — add groups via `tad config`",
        Section::Agents => "no agents running",
    }
}

fn empty_hint_line(section: Section, theme: &Theme, width: usize) -> Line<'static> {
    let text = format!("  {}", empty_hint_text(section));
    format::clip_line(
        Line::from(Span::styled(text, Style::default().fg(theme.muted))),
        width,
    )
}

/// Build the sidebar's rendered content lines (row lines plus
/// render-level empty-section hints), a parallel Vec mapping each line
/// back to its `app.rows` index (`None` for a hint line — there's no
/// row to click), and the line index corresponding to `app.cursor`.
/// Pure given `App`'s current state — no IO, easy to unit test.
fn build_sidebar_lines(app: &App, width: u16) -> (Vec<Line<'static>>, Vec<Option<usize>>, usize) {
    let mut lines = Vec::with_capacity(app.rows.len());
    let mut line_row: Vec<Option<usize>> = Vec::with_capacity(app.rows.len());
    let mut cursor_idx = 0usize;
    for (i, row) in app.rows.iter().enumerate() {
        if i == app.cursor {
            cursor_idx = lines.len();
        }
        lines.push(format::format_row(
            &app.data,
            row,
            &app.theme,
            app.spinner_tick,
            &app.pins,
            width,
        ));
        line_row.push(Some(i));
        if let RowKind::SectionHeader(section) = &row.kind {
            let section = *section;
            let expanded = !app.collapsed.contains(&section);
            let has_items = app
                .rows
                .get(i + 1)
                .map(|r| !matches!(r.kind, RowKind::SectionHeader(_)))
                .unwrap_or(false);
            if expanded && !has_items {
                lines.push(empty_hint_line(section, &app.theme, width as usize));
                line_row.push(None);
            }
        }
    }
    (lines, line_row, cursor_idx)
}

/// Patch a background color (and bold) onto every span in a line —
/// used to highlight the row under the cursor.
fn highlight(line: Line<'static>, bg: Color) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| Span::styled(s.content, s.style.bg(bg).add_modifier(Modifier::BOLD)))
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let inner_w = area.width.saturating_sub(2); // left/right borders
    let content_w = inner_w.saturating_sub(2); // 2-col gutter
    let viewport = area.height.saturating_sub(2) as usize; // top/bottom borders

    let (lines, line_row, cursor_idx) = build_sidebar_lines(app, content_w);
    app.sidebar_scroll = scroll_to_cursor(cursor_idx, app.sidebar_scroll, viewport.max(1));

    // Whole-rect zone first — the per-row/dot registrations below are
    // more specific and, being registered after, win at their exact
    // rects (see `hit::HitMap::at`).
    app.hits.register(area, Hit::SidebarZone);

    let scroll = app.sidebar_scroll;
    let visible: Vec<Line> = lines
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(viewport.max(1))
        .map(|(i, line)| {
            let gutter = if i == cursor_idx { "▶ " } else { "  " };
            let mut spans = vec![Span::raw(gutter)];
            spans.extend(line.spans);
            let full = Line::from(spans);
            if i == cursor_idx {
                highlight(full, theme.selection_bg)
            } else {
                full
            }
        })
        .collect();

    // Per-row click regions for the lines actually on screen, at their
    // real terminal position (inside the border, below the gutter).
    for (display_row, line_idx) in (scroll..scroll + visible.len()).enumerate() {
        let Some(Some(row_i)) = line_row.get(line_idx) else {
            continue; // an empty-section hint line — nothing to click
        };
        let row_i = *row_i;
        let Some(row) = app.rows.get(row_i) else {
            continue;
        };
        let y = area.y + 1 + display_row as u16;
        let row_rect = Rect {
            x: area.x + 1,
            y,
            width: inner_w,
            height: 1,
        };
        if let RowKind::SectionHeader(section) = &row.kind {
            let section = *section;
            app.hits
                .register(row_rect, Hit::Click(Action::ToggleSection(section)));
        } else if row.selectable {
            app.hits
                .register(row_rect, Hit::Click(Action::Select(row_i)));
            if matches!(&row.kind, RowKind::Agent(_) | RowKind::Session(_)) {
                let dot_rect = Rect {
                    x: area.x + 1 + 2, // past the 2-col gutter
                    y,
                    width: 3.min(content_w),
                    height: 1,
                };
                app.hits
                    .register(dot_rect, Hit::Click(Action::TogglePin(row_i)));
            }
        }
    }

    let title = if app.input_mode == InputMode::Filter || !app.filter.is_empty() {
        format!(" sidebar — /{} ", app.filter.as_str())
    } else {
        " sidebar ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .title(title);
    f.render_widget(Paragraph::new(visible).block(block), area);
}

fn render_preview(f: &mut Frame, area: Rect, app: &mut App) {
    app.hits.register(area, Hit::PreviewZone);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(" preview ");
    let scroll = app.preview_scroll;
    let para = Paragraph::new(app.preview_lines())
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

/// One `[key label]` footer chip.
fn chip(key: &str, label: &str, theme: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled("[", Style::default().fg(theme.muted)),
        Span::styled(
            key.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}"), Style::default().fg(theme.fg)),
        Span::styled("]", Style::default().fg(theme.muted)),
        Span::raw(" "),
    ]
}

/// Context-dependent footer bindings for `InputMode::None`: `d`/`R`/`s`
/// only appear on the row kinds they act on; `o` is "pin" or "return"
/// depending on whether something's already pulled out.
fn footer_chips(app: &App) -> Vec<(&'static str, String)> {
    let kind = app.selected_row().map(|r| r.kind.clone());
    let mut chips = vec![("↵", "open".to_string())];
    if let Some(p) = app.pins.first() {
        chips.push(("o", format!("return {}", p.label)));
    } else if matches!(kind, Some(RowKind::Session(_)) | Some(RowKind::Agent(_))) {
        chips.push(("o", "pin".to_string()));
    }
    chips.push(("n", "new".to_string()));
    if matches!(kind, Some(RowKind::Session(_)) | Some(RowKind::Agent(_))) {
        chips.push(("d", "kill".to_string()));
    }
    if matches!(kind, Some(RowKind::Agent(_))) {
        chips.push(("R", "rename".to_string()));
        chips.push(("s", "snooze".to_string()));
    }
    chips.push(("t", "theme".to_string()));
    chips.push(("/", "filter".to_string()));
    chips.push(("r", "refresh".to_string()));
    chips.push(("q", "quit".to_string()));
    chips
}

/// The `Action` a click on footer chip `key` should run. `o`'s label
/// varies (pin vs. return) but it's the same action either way.
fn chip_action(app: &App, key: &str) -> Option<Action> {
    match key {
        "↵" => Some(Action::Activate(app.cursor)),
        "o" => Some(Action::TogglePin(app.cursor)),
        "n" => Some(Action::NewSession),
        "d" => Some(Action::Kill),
        "R" => Some(Action::Rename),
        "s" => Some(Action::Snooze),
        "t" => Some(Action::OpenThemePicker),
        "/" => Some(Action::Filter),
        "r" => Some(Action::Refresh),
        "q" => Some(Action::Quit),
        _ => None,
    }
}

fn render_footer(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
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
            spans.push(Span::raw("  "));
            spans.extend(chip("↑↓", "nav", &theme));
            spans.extend(chip("↵", "open", &theme));
            spans.extend(chip("⇥", "section", &theme));
            spans.extend(chip("^U", "clear", &theme));
            spans.extend(chip("Esc", "exit", &theme));
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
        InputMode::ThemeSelect => Line::from(Span::styled(
            "↑↓ pick theme   ↵ confirm   Esc cancel",
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
                let mut spans = Vec::new();
                let mut x = area.x;
                for (key, label) in footer_chips(app) {
                    let chip_spans = chip(key, &label, &theme);
                    let width: u16 = chip_spans
                        .iter()
                        .map(|s| s.content.chars().count() as u16)
                        .sum();
                    if let Some(action) = chip_action(app, key) {
                        let rect = Rect {
                            x,
                            y: area.y,
                            width,
                            height: 1,
                        };
                        app.hits.register(rect, Hit::Click(action));
                    }
                    x += width;
                    spans.extend(chip_spans);
                }
                Line::from(spans)
            }
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::rows::{RowKind, Section};
    use crate::dashboard::testutil::{mk_agent, mk_data, mk_session};

    #[test]
    fn layout_mode_too_small_below_width_or_height_floor() {
        assert_eq!(layout_mode(39, 20), LayoutMode::TooSmall);
        assert_eq!(layout_mode(80, 9), LayoutMode::TooSmall);
    }

    #[test]
    fn layout_mode_narrow_below_70_wide() {
        assert_eq!(layout_mode(69, 20), LayoutMode::Narrow);
    }

    #[test]
    fn layout_mode_full_otherwise() {
        assert_eq!(layout_mode(100, 30), LayoutMode::Full);
    }

    #[test]
    fn scroll_to_cursor_no_change_when_already_visible() {
        assert_eq!(scroll_to_cursor(5, 2, 10), 2);
    }

    #[test]
    fn scroll_to_cursor_scrolls_up_when_cursor_above_viewport() {
        assert_eq!(scroll_to_cursor(1, 5, 10), 1);
    }

    #[test]
    fn scroll_to_cursor_scrolls_down_when_cursor_below_viewport() {
        assert_eq!(scroll_to_cursor(20, 0, 10), 11);
    }

    #[test]
    fn scroll_to_cursor_zero_viewport_is_a_no_op() {
        assert_eq!(scroll_to_cursor(20, 3, 0), 3);
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn build_sidebar_lines_hints_empty_expanded_sections() {
        let app = crate::dashboard::testutil::mk_app(mk_data(vec![], vec![]));
        let (lines, _, _) = build_sidebar_lines(&app, 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("no sessions")));
        assert!(texts.iter().any(|t| t.contains("no agents running")));
        assert!(texts.iter().any(|t| t.contains("no groups")));
        assert!(texts.iter().any(|t| t.contains("no hosts")));
    }

    #[test]
    fn build_sidebar_lines_no_hint_for_collapsed_section() {
        let data = mk_data(vec![], vec![]);
        let mut app = crate::dashboard::testutil::mk_app(data);
        app.collapsed.insert(Section::Sessions);
        app.refresh_rows();
        let (lines, _, _) = build_sidebar_lines(&app, 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(!texts.iter().any(|t| t.contains("no sessions")));
    }

    #[test]
    fn build_sidebar_lines_no_hint_when_section_has_items() {
        let data = mk_data(vec![mk_session("web")], vec![]);
        let app = crate::dashboard::testutil::mk_app(data);
        let (lines, _, _) = build_sidebar_lines(&app, 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(!texts.iter().any(|t| t.contains("no sessions")));
        assert!(texts.iter().any(|t| t.contains("web")));
    }

    #[test]
    fn build_sidebar_lines_cursor_index_accounts_for_earlier_hints() {
        // Sessions section is empty (adds one hint line before Agents'
        // header), so the cursor (parked on the Agents section header
        // by mk_app when there's no data) must land one line further
        // down the rendered Vec than its raw index in app.rows.
        let data = mk_data(vec![], vec![mk_agent("s1:0.0", "s1", 5)]);
        let mut app = crate::dashboard::testutil::mk_app(data);
        app.cursor =
            crate::dashboard::rows::section_header_index(&app.rows, Section::Agents).unwrap();
        let (lines, _, cursor_idx) = build_sidebar_lines(&app, 40);
        assert!(line_text(&lines[cursor_idx])
            .trim_start()
            .starts_with("AGENTS"));
        assert!(
            cursor_idx > app.cursor,
            "expected the sessions hint to shift the render index forward"
        );
    }

    #[test]
    fn footer_chips_shows_agent_only_bindings_on_agent_row() {
        let data = mk_data(vec![], vec![mk_agent("s1:0.0", "s1", 5)]);
        let app = crate::dashboard::testutil::mk_app(data);
        assert_eq!(
            app.selected_row().map(|r| r.kind.clone()),
            Some(RowKind::Agent("s1:0.0".into()))
        );
        let keys: Vec<&str> = footer_chips(&app).iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"R"));
        assert!(keys.contains(&"s"));
        assert!(keys.contains(&"d"));
        assert!(keys.contains(&"o"));
    }

    #[test]
    fn footer_chips_omits_agent_only_bindings_on_group_row() {
        let mut data = mk_data(vec![], vec![]);
        data.groups = vec![(
            "g1".to_string(),
            crate::config::Group {
                layout: "panes".to_string(),
                hosts: vec![],
            },
        )];
        let mut app = crate::dashboard::testutil::mk_app(data);
        app.cursor =
            crate::dashboard::rows::index_of(&app.rows, &RowKind::Group("g1".into())).unwrap();
        let keys: Vec<&str> = footer_chips(&app).iter().map(|(k, _)| *k).collect();
        assert!(!keys.contains(&"R"));
        assert!(!keys.contains(&"s"));
        assert!(!keys.contains(&"d"));
        assert!(!keys.contains(&"o"));
    }

    #[test]
    fn footer_chips_shows_return_when_pinned() {
        let data = mk_data(vec![mk_session("work")], vec![]);
        let mut app = crate::dashboard::testutil::mk_app(data);
        app.pins.push(crate::dashboard::PinnedPane {
            pane_id: "%1".into(),
            origin_window_id: "@1".into(),
            origin_session: "work".into(),
            origin_window_name: "w".into(),
            origin_window_index: "0".into(),
            label: "work:w".into(),
        });
        let chips = footer_chips(&app);
        assert!(chips
            .iter()
            .any(|(k, label)| *k == "o" && label.starts_with("return")));
    }
}
