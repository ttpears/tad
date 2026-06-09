//! Ratatui state machine and render loop for the wizard.

#![allow(dead_code)]

use std::collections::BTreeSet;

use anyhow::Result;

use crate::config;

pub const LAYOUTS: &[&str] = &["panes", "synced-panes", "windows", "browse"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    EditMode,
    ThemePicker,
    BuildGroups,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct GroupForm {
    pub name: String,
    pub layout_idx: usize,
    pub members: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct WizardState {
    pub stage: Stage,
    pub filter: String,
    pub built: Vec<(String, config::Group)>,
    pub form: GroupForm,
    pub config_exists: bool,
    pub status_flash: Option<String>,
}

impl WizardState {
    pub fn for_first_launch() -> Self {
        Self {
            stage: Stage::BuildGroups,
            filter: String::new(),
            built: Vec::new(),
            form: GroupForm::default(),
            config_exists: false,
            status_flash: None,
        }
    }

    pub fn for_config(config_exists: bool) -> Self {
        let mut s = Self::for_first_launch();
        s.config_exists = config_exists;
        s.stage = if config_exists {
            Stage::EditMode
        } else {
            Stage::BuildGroups
        };
        s
    }

    pub fn next_stage_from(&self, current: Stage) -> Option<Stage> {
        match current {
            Stage::EditMode => Some(Stage::BuildGroups),
            Stage::BuildGroups => Some(Stage::EditMode),
            Stage::ThemePicker => Some(Stage::EditMode),
            Stage::Done | Stage::Cancelled => None,
        }
    }

    pub fn can_advance(&self, _stage: Stage) -> Result<(), &'static str> {
        // BuildGroups validation is delegated to `commit_form`'s checks; no
        // other stage gates advancement.
        Ok(())
    }

    pub fn commit_form(&mut self) -> Result<(), &'static str> {
        let name = self.form.name.trim().to_string();
        if name.is_empty() {
            return Err("group name required");
        }
        if self.built.iter().any(|(n, _)| n == &name) {
            return Err("group name already used in this session");
        }
        if self.form.members.is_empty() {
            return Err("pick at least one host for the group");
        }
        let layout = LAYOUTS[self.form.layout_idx].to_string();
        let hosts: Vec<String> = self.form.members.iter().cloned().collect();
        self.built.push((name, config::Group { layout, hosts }));
        self.form = GroupForm::default();
        Ok(())
    }
}

pub fn merge_into_doc(
    doc: &mut config::Doc,
    incoming: Vec<(String, config::Group)>,
) -> Vec<(String, String)> {
    let mut renames = Vec::new();
    for (name, group) in incoming {
        if !doc.groups.contains_key(&name) {
            doc.groups.insert(name, group);
            continue;
        }
        let mut suffix = 2;
        let final_name = loop {
            let candidate = format!("{}-{}", name, suffix);
            if !doc.groups.contains_key(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        renames.push((name.clone(), final_name.clone()));
        doc.groups.insert(final_name, group);
    }
    renames
}

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::time::Duration;

use crate::discovery;

/// View-layer cursor positions per screen. Kept outside `WizardState` since
/// these are purely UI concerns the state machine doesn't care about.
#[derive(Default)]
struct Cursors {
    /// Index into the member-host list on the BuildGroups screen.
    built: usize,
    edit: usize,
    /// Byte cursor into `state.form.name` (BuildGroups name field).
    name: usize,
    /// Byte cursor into `state.filter` (BuildGroups member filter input).
    filter: usize,
    /// Index into `theme::BUILTIN_THEMES` on the theme picker screen.
    theme: usize,
}

/// Cursor-aware operations on a `(String, byte-cursor)` pair. UTF-8 safe.
mod text {
    pub fn insert(value: &mut String, cursor: &mut usize, c: char) {
        let pos = (*cursor).min(value.len());
        value.insert(pos, c);
        *cursor = pos + c.len_utf8();
    }
    pub fn backspace(value: &mut String, cursor: &mut usize) {
        if *cursor == 0 {
            return;
        }
        let mut prev = *cursor - 1;
        while prev > 0 && !value.is_char_boundary(prev) {
            prev -= 1;
        }
        value.replace_range(prev..*cursor, "");
        *cursor = prev;
    }
    pub fn delete(value: &mut String, cursor: &mut usize) {
        if *cursor >= value.len() {
            return;
        }
        let mut next = *cursor + 1;
        while next < value.len() && !value.is_char_boundary(next) {
            next += 1;
        }
        value.replace_range(*cursor..next, "");
    }
    pub fn left(value: &str, cursor: &mut usize) {
        if *cursor == 0 {
            return;
        }
        let mut c = *cursor - 1;
        while c > 0 && !value.is_char_boundary(c) {
            c -= 1;
        }
        *cursor = c;
    }
    pub fn right(value: &str, cursor: &mut usize) {
        if *cursor >= value.len() {
            return;
        }
        let mut c = *cursor + 1;
        while c < value.len() && !value.is_char_boundary(c) {
            c += 1;
        }
        *cursor = c;
    }
    pub fn home(cursor: &mut usize) {
        *cursor = 0;
    }
    pub fn end(value: &str, cursor: &mut usize) {
        *cursor = value.len();
    }
    pub fn clear(value: &mut String, cursor: &mut usize) {
        value.clear();
        *cursor = 0;
    }
}

/// RAII guard restoring the terminal even on panic.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run() -> Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let _guard = TermGuard;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = Terminal::new(backend)?;

    // "Config exists" for wizard purposes means "user already has groups
    // configured" — not just "config.yaml file is present" (a file with
    // only a theme set still counts as a first launch from the groups
    // perspective). With groups → Edit mode; without → the setup flow.
    let config_exists = config::load()
        .map(|d| !d.groups.is_empty())
        .unwrap_or(false);
    let mut state = WizardState::for_config(config_exists);

    let mut cursors = Cursors::default();
    let mut filter_mode = false;
    let mut form_field = 0usize;

    loop {
        term.draw(|f| draw(f, &state, &cursors, filter_mode, form_field))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let typing_name = state.stage == Stage::BuildGroups && form_field == 0;
        let in_subscreen = state.stage == Stage::ThemePicker;
        let cancel_pressed = key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && !key.modifiers.contains(KeyModifiers::SHIFT));
        if !filter_mode && !typing_name && !in_subscreen && cancel_pressed {
            state.stage = Stage::Cancelled;
            break;
        }

        match state.stage {
            Stage::EditMode => {
                handle_edit_mode(&mut state, key, &mut cursors.edit, &mut cursors.theme)
            }
            Stage::ThemePicker => handle_theme_picker(&mut state, key, &mut cursors.theme),
            Stage::BuildGroups => handle_build_groups(
                &mut state,
                key,
                &mut form_field,
                &mut cursors.built,
                &mut cursors.name,
                &mut cursors.filter,
                &mut filter_mode,
            ),
            Stage::Done | Stage::Cancelled => break,
        }
    }

    Ok(())
}

/// Live-discovered host names for the BuildGroups member picker, with the
/// `/` filter applied.
fn discovered_host_names() -> Vec<String> {
    discovery::discover(&discovery::DiscoveryConfig::load())
        .into_iter()
        .map(|c| c.host)
        .collect()
}

fn filtered_member_hosts(state: &WizardState) -> Vec<String> {
    let f = state.filter.to_lowercase();
    discovered_host_names()
        .into_iter()
        .filter(|h| f.is_empty() || h.to_lowercase().contains(&f))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn handle_build_groups(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    form_field: &mut usize,
    cursor_built: &mut usize,
    name_cursor: &mut usize,
    filter_cursor: &mut usize,
    filter_mode: &mut bool,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if *filter_mode {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => *filter_mode = false,
            KeyCode::Backspace => text::backspace(&mut state.filter, filter_cursor),
            KeyCode::Delete => text::delete(&mut state.filter, filter_cursor),
            KeyCode::Left => text::left(&state.filter, filter_cursor),
            KeyCode::Right => text::right(&state.filter, filter_cursor),
            KeyCode::Home => text::home(filter_cursor),
            KeyCode::End => text::end(&state.filter, filter_cursor),
            KeyCode::Char('u') if ctrl => text::clear(&mut state.filter, filter_cursor),
            KeyCode::Char('a') if ctrl => text::home(filter_cursor),
            KeyCode::Char('e') if ctrl => text::end(&state.filter, filter_cursor),
            KeyCode::Char(c) if !ctrl => text::insert(&mut state.filter, filter_cursor, c),
            _ => {}
        }
        *cursor_built = 0;
        return;
    }
    match key.code {
        KeyCode::Tab => *form_field = (*form_field + 1) % 3,
        KeyCode::BackTab => *form_field = (*form_field + 2) % 3,
        // Left/Right: cursor in the name field, layout cycling on layout
        // field, ignored on members.
        KeyCode::Left if *form_field == 0 => text::left(&state.form.name, name_cursor),
        KeyCode::Right if *form_field == 0 => text::right(&state.form.name, name_cursor),
        KeyCode::Home if *form_field == 0 => text::home(name_cursor),
        KeyCode::End if *form_field == 0 => text::end(&state.form.name, name_cursor),
        KeyCode::Delete if *form_field == 0 => text::delete(&mut state.form.name, name_cursor),
        KeyCode::Char('u') if ctrl && *form_field == 0 => {
            text::clear(&mut state.form.name, name_cursor)
        }
        KeyCode::Char('a') if ctrl && *form_field == 0 => text::home(name_cursor),
        KeyCode::Char('e') if ctrl && *form_field == 0 => text::end(&state.form.name, name_cursor),
        KeyCode::Left => {
            if *form_field == 1 {
                if state.form.layout_idx == 0 {
                    state.form.layout_idx = LAYOUTS.len() - 1;
                } else {
                    state.form.layout_idx -= 1;
                }
            }
        }
        KeyCode::Right => {
            if *form_field == 1 {
                state.form.layout_idx = (state.form.layout_idx + 1) % LAYOUTS.len();
            }
        }
        KeyCode::Char(c) if *form_field == 0 && !ctrl => {
            text::insert(&mut state.form.name, name_cursor, c)
        }
        KeyCode::Backspace if *form_field == 0 => {
            text::backspace(&mut state.form.name, name_cursor)
        }
        KeyCode::Char('/') if *form_field == 2 => {
            *filter_mode = true;
            state.filter.clear();
            *filter_cursor = 0;
            *cursor_built = 0;
        }
        KeyCode::Char(' ') if *form_field == 2 => {
            let hosts = filtered_member_hosts(state);
            if let Some(h) = hosts.get(*cursor_built) {
                if !state.form.members.remove(h) {
                    state.form.members.insert(h.clone());
                }
            }
        }
        KeyCode::Up if *form_field == 2 => {
            if *cursor_built > 0 {
                *cursor_built -= 1;
            }
        }
        KeyCode::Down if *form_field == 2 => {
            let n = filtered_member_hosts(state).len();
            if n > 0 && *cursor_built + 1 < n {
                *cursor_built += 1;
            }
        }
        // Up/Down cycle form fields when not editing the member list.
        KeyCode::Up if *form_field != 2 => {
            *form_field = (*form_field + 2) % 3;
        }
        KeyCode::Down if *form_field != 2 => {
            *form_field = (*form_field + 1) % 3;
        }
        KeyCode::Enter => {
            if state.form.name.trim().is_empty() {
                // Empty name with no work to commit: go back to the list.
                if let Some(next) = state.next_stage_from(Stage::BuildGroups) {
                    state.stage = next;
                }
            } else if let Err(msg) = state.commit_form() {
                state.status_flash = Some(msg.to_string());
            } else {
                // commit_form has pushed the new group into state.built;
                // persist immediately and return to the groups list.
                *name_cursor = 0;
                let mut doc = config::load().unwrap_or_default();
                let incoming = state.built.drain(..).collect::<Vec<_>>();
                let _renames = merge_into_doc(&mut doc, incoming);
                match config::save(&doc) {
                    Ok(()) => state.status_flash = Some("group saved".into()),
                    Err(e) => state.status_flash = Some(format!("save failed: {:#}", e)),
                }
                state.stage = Stage::EditMode;
            }
        }
        _ => {}
    }
}

fn handle_edit_mode(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    cursor: &mut usize,
    theme_cursor: &mut usize,
) {
    let doc = config::load().unwrap_or_default();
    let names: Vec<String> = doc.groups.keys().cloned().collect();
    let len = names.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if *cursor > 0 {
                *cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if len > 0 && *cursor + 1 < len {
                *cursor += 1;
            }
        }
        KeyCode::Char('d') => {
            if let Some(name) = names.get(*cursor) {
                let mut d = config::load().unwrap_or_default();
                d.groups.shift_remove(name);
                let _ = config::save(&d);
            }
        }
        KeyCode::Enter => {
            // Start adding a new group.
            state.stage = Stage::BuildGroups;
        }
        KeyCode::Char('t') => {
            // Position cursor on the currently-active theme (if any).
            *theme_cursor = crate::theme::current_name()
                .and_then(|n| crate::theme::BUILTIN_THEMES.iter().position(|&t| t == n))
                .unwrap_or(0);
            state.stage = Stage::ThemePicker;
        }
        _ => {}
    }
}

fn handle_theme_picker(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    cursor: &mut usize,
) {
    let len = crate::theme::BUILTIN_THEMES.len();
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.stage = Stage::EditMode;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if *cursor > 0 {
                *cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if *cursor + 1 < len {
                *cursor += 1;
            }
        }
        KeyCode::Home | KeyCode::Char('g') => *cursor = 0,
        KeyCode::End | KeyCode::Char('G') => *cursor = len.saturating_sub(1),
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(&name) = crate::theme::BUILTIN_THEMES.get(*cursor) {
                match crate::theme::save_theme_name(name) {
                    Ok(()) => {
                        state.status_flash =
                            Some(format!("saved theme: {} (takes effect next launch)", name));
                        state.stage = Stage::EditMode;
                    }
                    Err(e) => {
                        state.status_flash = Some(format!("failed to save theme: {:#}", e));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Render a single-line value with a visible cursor when `active`. Uses
/// reverse video for the character under the cursor — works on any terminal
/// regardless of theme.
fn line_with_cursor(value: &str, cursor: usize, active: bool) -> Line<'static> {
    if !active {
        return Line::from(value.to_string());
    }
    let cur = cursor.min(value.len());
    let (pre, post) = value.split_at(cur);
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    if post.is_empty() {
        return Line::from(vec![
            Span::raw(pre.to_string()),
            Span::styled(" ".to_string(), cursor_style),
        ]);
    }
    let mut chars = post.chars();
    let c = chars.next().unwrap_or(' ');
    let rest: String = chars.collect();
    Line::from(vec![
        Span::raw(pre.to_string()),
        Span::styled(c.to_string(), cursor_style),
        Span::raw(rest),
    ])
}

fn draw(
    f: &mut Frame,
    state: &WizardState,
    cursors: &Cursors,
    filter_mode: bool,
    form_field: usize,
) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);
    draw_header(f, chunks[0], state);
    draw_body(f, chunks[1], state, cursors, filter_mode, form_field);
    draw_footer(f, chunks[2], state, filter_mode);
}

fn draw_header(f: &mut Frame, area: Rect, state: &WizardState) {
    let title = match state.stage {
        Stage::EditMode => "tad config — edit groups",
        Stage::ThemePicker => "tad config — theme",
        Stage::BuildGroups => "tad config — add group",
        Stage::Done | Stage::Cancelled => "tad config",
    };
    let p = Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, state: &WizardState, filter_mode: bool) {
    let hint = match state.stage {
        _ if filter_mode => "/filter… Enter to apply · Esc cancel".to_string(),
        Stage::EditMode => "enter add group · d delete · t theme · q quit".to_string(),
        Stage::ThemePicker => "↑↓ pick · enter apply · esc back".to_string(),
        Stage::BuildGroups => "tab fields · ←/→ layout · space toggle member · / filter · enter save · esc cancel".to_string(),
        Stage::Done => state.status_flash.clone().unwrap_or_default(),
        Stage::Cancelled => "cancelled".to_string(),
    };
    let mut lines = vec![Line::from(hint)];
    if let Some(flash) = &state.status_flash {
        if state.stage != Stage::Done {
            lines.push(Line::from(Span::styled(
                flash.clone(),
                Style::default().add_modifier(Modifier::REVERSED),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn draw_body(
    f: &mut Frame,
    area: Rect,
    state: &WizardState,
    cursors: &Cursors,
    filter_mode: bool,
    form_field: usize,
) {
    match state.stage {
        Stage::ThemePicker => {
            let current = crate::theme::current_name();
            let items: Vec<ListItem> = crate::theme::BUILTIN_THEMES
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let mark = if i == cursors.theme { "→ " } else { "  " };
                    let active = current.as_deref() == Some(*name);
                    let suffix = if active { "  (current)" } else { "" };
                    let line = format!("{}{:<22}{}", mark, name, suffix);
                    let style = if i == cursors.theme {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    ListItem::new(Line::from(Span::styled(line, style)))
                })
                .collect();
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).title("Themes")),
                area,
            );
        }
        Stage::EditMode => {
            let doc = config::load().unwrap_or_default();
            let items: Vec<ListItem> = doc
                .groups
                .iter()
                .enumerate()
                .map(|(i, (n, g))| {
                    let marker = if i == cursors.edit { "→ " } else { "  " };
                    ListItem::new(format!(
                        "{}{:<20} {:<14} {} hosts",
                        marker,
                        n,
                        g.layout,
                        g.hosts.len()
                    ))
                })
                .collect();
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).title("Groups")),
                area,
            );
        }
        Stage::BuildGroups => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            let hosts: Vec<String> = filtered_member_hosts(state);
            let items: Vec<ListItem> = hosts
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let mark = if form_field == 2 && i == cursors.built {
                        "→ "
                    } else {
                        "  "
                    };
                    let box_ = if state.form.members.contains(h) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    ListItem::new(format!("{}{} {}", mark, box_, h))
                })
                .collect();
            let title: Line = if filter_mode {
                let mut spans = vec![Span::raw("Members  /")];
                let cur = cursors.filter.min(state.filter.len());
                let (pre, post) = state.filter.split_at(cur);
                let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
                spans.push(Span::raw(pre.to_string()));
                if post.is_empty() {
                    spans.push(Span::styled(" ", cursor_style));
                } else {
                    let mut chars = post.chars();
                    let c = chars.next().unwrap_or(' ');
                    let rest: String = chars.collect();
                    spans.push(Span::styled(c.to_string(), cursor_style));
                    spans.push(Span::raw(rest));
                }
                Line::from(spans)
            } else if state.filter.is_empty() {
                Line::from(if form_field == 2 { "Members ●" } else { "Members" })
            } else {
                Line::from(format!("Members (filter: {})", state.filter))
            };
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
                cols[0],
            );

            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(5), Constraint::Min(0)])
                .split(cols[1]);
            let name_title = if form_field == 0 { "Name ●" } else { "Name" };
            let name_line = line_with_cursor(&state.form.name, cursors.name, form_field == 0);
            f.render_widget(
                Paragraph::new(name_line)
                    .block(Block::default().borders(Borders::ALL).title(name_title)),
                Rect {
                    x: right[0].x,
                    y: right[0].y,
                    width: right[0].width,
                    height: 3,
                },
            );
            let layout_title = if form_field == 1 {
                "Layout ●"
            } else {
                "Layout"
            };
            f.render_widget(
                Paragraph::new(LAYOUTS[state.form.layout_idx])
                    .block(Block::default().borders(Borders::ALL).title(layout_title)),
                Rect {
                    x: right[0].x,
                    y: right[0].y + 3,
                    width: right[0].width,
                    height: 2,
                },
            );
            let selected: Vec<ListItem> = state
                .form
                .members
                .iter()
                .map(|h| ListItem::new(h.clone()))
                .collect();
            f.render_widget(
                List::new(selected).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Selected members"),
                ),
                right[1],
            );
        }
        Stage::Done | Stage::Cancelled => {
            let msg = state
                .status_flash
                .clone()
                .unwrap_or_else(|| "done".to_string());
            f.render_widget(Paragraph::new(msg), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_form_validates_and_clears() {
        let mut s = WizardState::for_first_launch();
        s.form.name = "".into();
        assert!(s.commit_form().is_err());
        s.form.name = "prod".into();
        assert!(s.commit_form().is_err());
        s.form.members.insert("h1".into());
        s.form.layout_idx = 2;
        s.commit_form().unwrap();
        assert_eq!(s.built.len(), 1);
        assert_eq!(s.built[0].0, "prod");
        assert_eq!(s.built[0].1.layout, "windows");
        assert_eq!(s.built[0].1.hosts, vec!["h1".to_string()]);
        assert!(s.form.name.is_empty());
        assert!(s.form.members.is_empty());
    }

    #[test]
    fn commit_form_rejects_duplicate_name_within_run() {
        let mut s = WizardState::for_first_launch();
        s.form.name = "g".into();
        s.form.members.insert("h".into());
        s.commit_form().unwrap();
        s.form.name = "g".into();
        s.form.members.insert("h2".into());
        assert!(s.commit_form().is_err());
    }

    #[test]
    fn merge_into_doc_resolves_collisions() {
        let mut doc = config::Doc::default();
        doc.groups.insert(
            "g".into(),
            config::Group {
                layout: "windows".into(),
                hosts: vec!["x".into()],
            },
        );
        let incoming = vec![(
            "g".into(),
            config::Group {
                layout: "panes".into(),
                hosts: vec!["y".into()],
            },
        )];
        let renames = merge_into_doc(&mut doc, incoming);
        assert_eq!(renames, vec![("g".into(), "g-2".into())]);
        assert!(doc.groups.contains_key("g"));
        assert!(doc.groups.contains_key("g-2"));
    }
}
