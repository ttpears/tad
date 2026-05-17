//! Ratatui state machine and render loop for the wizard.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config;
use crate::wizard::discovery::{HostCandidate, SessionCandidate};
use crate::wizard::SourceSet;

pub const LAYOUTS: &[&str] = &["panes", "synced-panes", "windows", "browse"];

#[derive(Debug, Clone, Copy)]
pub enum Entry {
    FirstLaunch,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    EditMode,
    Welcome,
    Sessions,
    Hosts,
    BuildGroups,
    Confirm,
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
    pub sources: SourceSet,
    pub host_candidates: Vec<HostCandidate>,
    pub session_candidates: Vec<SessionCandidate>,
    pub selected_hosts: BTreeSet<String>,
    pub selected_sessions: BTreeSet<String>,
    pub session_overrides: BTreeMap<String, (String, usize)>,
    pub filter: String,
    pub built: Vec<(String, config::Group)>,
    pub form: GroupForm,
    pub scan_errors: Vec<String>,
    pub config_exists: bool,
    pub status_flash: Option<String>,
}

impl WizardState {
    pub fn for_first_launch() -> Self {
        Self {
            stage: Stage::Welcome,
            sources: SourceSet::ALL,
            host_candidates: Vec::new(),
            session_candidates: Vec::new(),
            selected_hosts: BTreeSet::new(),
            selected_sessions: BTreeSet::new(),
            session_overrides: BTreeMap::new(),
            filter: String::new(),
            built: Vec::new(),
            form: GroupForm::default(),
            scan_errors: Vec::new(),
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
            Stage::Welcome
        };
        s
    }

    pub fn next_stage_from(&self, current: Stage) -> Option<Stage> {
        match current {
            Stage::EditMode => Some(Stage::Welcome),
            Stage::Welcome => {
                if self.sources.tmux_sessions {
                    Some(Stage::Sessions)
                } else if self.sources.shell || self.sources.ssh_config || self.sources.known_hosts
                {
                    Some(Stage::Hosts)
                } else {
                    None
                }
            }
            Stage::Sessions => {
                if self.sources.shell || self.sources.ssh_config || self.sources.known_hosts {
                    Some(Stage::Hosts)
                } else {
                    Some(Stage::Confirm)
                }
            }
            Stage::Hosts => Some(Stage::BuildGroups),
            Stage::BuildGroups => Some(Stage::Confirm),
            Stage::Confirm => Some(Stage::Done),
            Stage::Done | Stage::Cancelled => None,
        }
    }

    pub fn can_advance(&self, stage: Stage) -> Result<(), &'static str> {
        match stage {
            Stage::Welcome => {
                if self.sources.count() == 0 {
                    Err("select at least one source")
                } else {
                    Ok(())
                }
            }
            Stage::Hosts => {
                if self.selected_hosts.is_empty() {
                    Err("select at least one host")
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    pub fn toggle_source(&mut self, idx: usize) {
        match idx {
            0 => self.sources.shell = !self.sources.shell,
            1 => self.sources.ssh_config = !self.sources.ssh_config,
            2 => self.sources.known_hosts = !self.sources.known_hosts,
            3 => self.sources.tmux_sessions = !self.sources.tmux_sessions,
            _ => {}
        }
    }

    pub fn toggle_host(&mut self, host: &str) {
        if !self.selected_hosts.remove(host) {
            self.selected_hosts.insert(host.to_string());
        }
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

    pub fn assemble_groups(&self) -> Vec<(String, config::Group)> {
        let mut out: Vec<(String, config::Group)> = Vec::new();
        for s in &self.session_candidates {
            if !self.selected_sessions.contains(&s.name) {
                continue;
            }
            let (name, layout_idx) = self
                .session_overrides
                .get(&s.name)
                .cloned()
                .unwrap_or_else(|| (s.name.clone(), 2));
            out.push((
                name,
                config::Group {
                    layout: LAYOUTS[layout_idx].to_string(),
                    hosts: s.windows.clone(),
                },
            ));
        }
        out.extend(self.built.clone());
        out
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

use crate::wizard::discovery;

/// View-layer cursor positions per screen. Kept outside `WizardState` since
/// these are purely UI concerns the state machine doesn't care about.
#[derive(Default)]
struct Cursors {
    welcome: usize,
    hosts: usize,
    sessions: usize,
    built: usize,
    edit: usize,
}

/// RAII guard restoring the terminal even on panic.
struct TermGuard;

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

pub fn run(entry: Entry) -> Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let _guard = TermGuard;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = Terminal::new(backend)?;

    let config_exists = config::config_path().exists();
    let mut state = match entry {
        Entry::FirstLaunch => WizardState::for_first_launch(),
        Entry::Config => WizardState::for_config(config_exists),
    };

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
        let cancel_pressed = key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && !key.modifiers.contains(KeyModifiers::SHIFT));
        if !filter_mode && !typing_name && cancel_pressed {
            state.stage = Stage::Cancelled;
            break;
        }

        match state.stage {
            Stage::EditMode => handle_edit_mode(&mut state, key, &mut cursors.edit),
            Stage::Welcome => handle_welcome(&mut state, key, &mut cursors.welcome),
            Stage::Sessions => handle_sessions(&mut state, key, &mut cursors.sessions),
            Stage::Hosts => handle_hosts(&mut state, key, &mut cursors.hosts, &mut filter_mode),
            Stage::BuildGroups => {
                handle_build_groups(&mut state, key, &mut form_field, &mut cursors.built)
            }
            Stage::Confirm => {
                if let KeyCode::Char(c) = key.code {
                    if c == 'y' || c == 'Y' {
                        write_and_finish(&mut state)?;
                        break;
                    } else if c == 'n' || c == 'N' {
                        if let Some(prev) = previous_stage(&state) {
                            state.stage = prev;
                        }
                    }
                }
            }
            Stage::Done | Stage::Cancelled => break,
        }
    }

    Ok(())
}

fn previous_stage(state: &WizardState) -> Option<Stage> {
    if !state.built.is_empty() || !state.selected_hosts.is_empty() {
        Some(Stage::BuildGroups)
    } else if !state.selected_sessions.is_empty() {
        Some(Stage::Sessions)
    } else {
        Some(Stage::Welcome)
    }
}

fn handle_welcome(state: &mut WizardState, key: crossterm::event::KeyEvent, cursor: &mut usize) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if *cursor > 0 {
                *cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if *cursor < 3 {
                *cursor += 1;
            }
        }
        KeyCode::Char(' ') => state.toggle_source(*cursor),
        KeyCode::Enter => {
            if let Err(msg) = state.can_advance(Stage::Welcome) {
                state.status_flash = Some(msg.to_string());
            } else {
                if state.sources.tmux_sessions && state.session_candidates.is_empty() {
                    state.session_candidates = discovery::scan_tmux_sessions();
                    for s in &state.session_candidates {
                        if s.usable {
                            state.selected_sessions.insert(s.name.clone());
                        }
                    }
                }
                if (state.sources.shell || state.sources.ssh_config || state.sources.known_hosts)
                    && state.host_candidates.is_empty()
                {
                    let (cands, errs) = discovery::scan_hosts(state.sources);
                    state.host_candidates = cands;
                    state.scan_errors = errs;
                }
                if let Some(next) = state.next_stage_from(Stage::Welcome) {
                    state.stage = next;
                }
            }
        }
        _ => {}
    }
}

fn handle_sessions(state: &mut WizardState, key: crossterm::event::KeyEvent, cursor: &mut usize) {
    let len = state.session_candidates.len();
    if len == 0 {
        if key.code == KeyCode::Enter {
            if let Some(next) = state.next_stage_from(Stage::Sessions) {
                state.stage = next;
            }
        }
        return;
    }
    match key.code {
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
        KeyCode::Char(' ') => {
            let name = state.session_candidates[*cursor].name.clone();
            if !state.selected_sessions.remove(&name) {
                state.selected_sessions.insert(name);
            }
        }
        KeyCode::Char('l') => {
            let name = state.session_candidates[*cursor].name.clone();
            let entry = state
                .session_overrides
                .entry(name.clone())
                .or_insert((name, 2));
            entry.1 = (entry.1 + 1) % LAYOUTS.len();
        }
        KeyCode::Enter => {
            if let Some(next) = state.next_stage_from(Stage::Sessions) {
                state.stage = next;
            }
        }
        _ => {}
    }
}

fn filtered_hosts(state: &WizardState) -> Vec<&HostCandidate> {
    let f = state.filter.to_lowercase();
    state
        .host_candidates
        .iter()
        .filter(|c| f.is_empty() || c.host.to_lowercase().contains(&f))
        .collect()
}

fn handle_hosts(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    cursor: &mut usize,
    filter_mode: &mut bool,
) {
    if *filter_mode {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => *filter_mode = false,
            KeyCode::Backspace => {
                state.filter.pop();
            }
            KeyCode::Char(c) => state.filter.push(c),
            _ => {}
        }
        *cursor = 0;
        return;
    }
    let visible: Vec<String> = filtered_hosts(state)
        .iter()
        .map(|c| c.host.clone())
        .collect();
    let len = visible.len();
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
        KeyCode::Char(' ') => {
            if let Some(h) = visible.get(*cursor) {
                state.toggle_host(h);
            }
        }
        KeyCode::Char('a') => {
            for h in &visible {
                state.selected_hosts.insert(h.clone());
            }
        }
        KeyCode::Char('n') => state.selected_hosts.clear(),
        KeyCode::Char('/') => {
            *filter_mode = true;
            state.filter.clear();
        }
        KeyCode::Enter => {
            if let Err(msg) = state.can_advance(Stage::Hosts) {
                state.status_flash = Some(msg.to_string());
            } else if let Some(next) = state.next_stage_from(Stage::Hosts) {
                state.stage = next;
            }
        }
        _ => {}
    }
}

fn handle_build_groups(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    form_field: &mut usize,
    cursor_built: &mut usize,
) {
    match key.code {
        KeyCode::Tab => *form_field = (*form_field + 1) % 3,
        KeyCode::BackTab => *form_field = (*form_field + 2) % 3,
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
        KeyCode::Char(c) if *form_field == 0 => state.form.name.push(c),
        KeyCode::Backspace if *form_field == 0 => {
            state.form.name.pop();
        }
        KeyCode::Char(' ') if *form_field == 2 => {
            let hosts: Vec<String> = state.selected_hosts.iter().cloned().collect();
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
            let n = state.selected_hosts.len();
            if n > 0 && *cursor_built + 1 < n {
                *cursor_built += 1;
            }
        }
        KeyCode::Enter => {
            if state.form.name.trim().is_empty() {
                if let Some(next) = state.next_stage_from(Stage::BuildGroups) {
                    state.stage = next;
                }
            } else if let Err(msg) = state.commit_form() {
                state.status_flash = Some(msg.to_string());
            }
        }
        KeyCode::Char('d') if *form_field != 0 => {
            if !state.built.is_empty() {
                state.built.pop();
            }
        }
        _ => {}
    }
}

fn handle_edit_mode(state: &mut WizardState, key: crossterm::event::KeyEvent, cursor: &mut usize) {
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
        KeyCode::Char('i') => {
            state.sources = SourceSet::NONE;
            state.stage = Stage::Welcome;
        }
        _ => {}
    }
}

fn write_and_finish(state: &mut WizardState) -> Result<()> {
    let mut doc = config::load().unwrap_or_default();
    let incoming = state.assemble_groups();
    let count = incoming.len();
    let renames = merge_into_doc(&mut doc, incoming);
    config::save(&doc)?;
    let mut msg = format!(
        "wrote {} groups to {}",
        count,
        config::config_path().display()
    );
    if !renames.is_empty() {
        let rs: Vec<String> = renames
            .iter()
            .map(|(a, b)| format!("{}→{}", a, b))
            .collect();
        msg.push_str(&format!(" (renamed: {})", rs.join(", ")));
    }
    state.status_flash = Some(msg);
    state.stage = Stage::Done;
    Ok(())
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
        Stage::Welcome => "tad config — import sources (local files only, no network)",
        Stage::Sessions => "tad config — import tmux sessions as groups",
        Stage::Hosts => "tad config — discovered hosts",
        Stage::BuildGroups => "tad config — build groups",
        Stage::Confirm => "tad config — confirm",
        Stage::Done | Stage::Cancelled => "tad config",
    };
    let p = Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(p, area);
}

fn draw_footer(f: &mut Frame, area: Rect, state: &WizardState, filter_mode: bool) {
    let hint = match state.stage {
        _ if filter_mode => "/filter… Enter to apply · Esc cancel".to_string(),
        Stage::EditMode => "d delete · i re-run imports · q quit".to_string(),
        Stage::Welcome => format!(
            "space toggle · enter next · q cancel · will scan {} local sources — no network access",
            state.sources.count()
        ),
        Stage::Sessions => "space toggle · l layout · enter next · q cancel".to_string(),
        Stage::Hosts => format!(
            "space · a all · n none · / filter · enter next · q cancel · {} selected of {}",
            state.selected_hosts.len(),
            state.host_candidates.len()
        ),
        Stage::BuildGroups => "tab fields · ←/→ layout · space toggle member · enter commit · d undo · empty-name enter = done".to_string(),
        Stage::Confirm => "y write · n back · esc cancel".to_string(),
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
    _filter_mode: bool,
    form_field: usize,
) {
    match state.stage {
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
        Stage::Welcome => {
            let labels = [
                "Shell history       ~/.bash_history, ~/.zsh_history, fish",
                "~/.ssh/config       Host blocks (not wildcards)",
                "~/.ssh/known_hosts  accepted hosts (not hashed)",
                "Tmux sessions       import existing sessions as groups",
            ];
            let on = [
                state.sources.shell,
                state.sources.ssh_config,
                state.sources.known_hosts,
                state.sources.tmux_sessions,
            ];
            let items: Vec<ListItem> = labels
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let mark = if i == cursors.welcome { "→ " } else { "  " };
                    let box_ = if on[i] { "[x]" } else { "[ ]" };
                    ListItem::new(format!("{}{} {}", mark, box_, l))
                })
                .collect();
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).title("Sources")),
                area,
            );
        }
        Stage::Sessions => {
            let items: Vec<ListItem> = state
                .session_candidates
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mark = if i == cursors.sessions { "→ " } else { "  " };
                    let box_ = if state.selected_sessions.contains(&s.name) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    let (gname, layout_idx) = state
                        .session_overrides
                        .get(&s.name)
                        .cloned()
                        .unwrap_or_else(|| (s.name.clone(), 2));
                    let tail = if !s.usable {
                        "  (skipped: no host-like windows)"
                    } else {
                        ""
                    };
                    ListItem::new(format!(
                        "{}{} {:<20} {} windows  group:{:<18} layout:{}{}",
                        mark,
                        box_,
                        s.name,
                        s.windows.len(),
                        gname,
                        LAYOUTS[layout_idx],
                        tail
                    ))
                })
                .collect();
            f.render_widget(
                List::new(items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Tmux sessions"),
                ),
                area,
            );
        }
        Stage::Hosts => {
            let visible = filtered_hosts(state);
            let items: Vec<ListItem> = visible
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mark = if i == cursors.hosts { "→ " } else { "  " };
                    let box_ = if state.selected_hosts.contains(&c.host) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    let mut tags = Vec::new();
                    if c.sources.shell {
                        tags.push("shell");
                    }
                    if c.sources.ssh_config {
                        tags.push("ssh-config");
                    }
                    if c.sources.known_hosts {
                        tags.push("known-hosts");
                    }
                    ListItem::new(format!(
                        "{}{} {:<30} ({})",
                        mark,
                        box_,
                        c.host,
                        tags.join(", ")
                    ))
                })
                .collect();
            let title = if state.filter.is_empty() {
                "Hosts".to_string()
            } else {
                format!("Hosts (filter: {})", state.filter)
            };
            f.render_widget(
                List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
                area,
            );
        }
        Stage::BuildGroups => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            let hosts: Vec<String> = state.selected_hosts.iter().cloned().collect();
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
            let title = if form_field == 2 {
                "Members ●"
            } else {
                "Members"
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
            f.render_widget(
                Paragraph::new(state.form.name.as_str())
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
            let built_items: Vec<ListItem> = state
                .built
                .iter()
                .map(|(n, g)| {
                    ListItem::new(format!("{} ({}, {} hosts)", n, g.layout, g.hosts.len()))
                })
                .collect();
            f.render_widget(
                List::new(built_items)
                    .block(Block::default().borders(Borders::ALL).title("Built so far")),
                right[1],
            );
        }
        Stage::Confirm => {
            let incoming = state.assemble_groups();
            let mut lines = Vec::new();
            if state.config_exists {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} new groups will be merged into existing config",
                        incoming.len()
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            for (n, g) in &incoming {
                lines.push(Line::from(format!(
                    "  {:<20} {:<14} hosts: {}",
                    n,
                    g.layout,
                    g.hosts.join(", ")
                )));
            }
            f.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Confirm"))
                    .wrap(Wrap { trim: false }),
                area,
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
    fn welcome_requires_at_least_one_source_to_advance() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet::NONE;
        assert!(s.can_advance(Stage::Welcome).is_err());
        s.sources.shell = true;
        assert!(s.can_advance(Stage::Welcome).is_ok());
    }

    #[test]
    fn next_stage_skips_sessions_when_off() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet {
            shell: true,
            ssh_config: false,
            known_hosts: false,
            tmux_sessions: false,
        };
        assert_eq!(s.next_stage_from(Stage::Welcome), Some(Stage::Hosts));
    }

    #[test]
    fn next_stage_skips_hosts_when_only_sessions_on() {
        let mut s = WizardState::for_first_launch();
        s.sources = SourceSet {
            shell: false,
            ssh_config: false,
            known_hosts: false,
            tmux_sessions: true,
        };
        assert_eq!(s.next_stage_from(Stage::Welcome), Some(Stage::Sessions));
        assert_eq!(s.next_stage_from(Stage::Sessions), Some(Stage::Confirm));
    }

    #[test]
    fn host_screen_requires_selection() {
        let mut s = WizardState::for_first_launch();
        assert!(s.can_advance(Stage::Hosts).is_err());
        s.selected_hosts.insert("h1".to_string());
        assert!(s.can_advance(Stage::Hosts).is_ok());
    }

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
    fn assemble_groups_applies_session_overrides_and_appends_built() {
        let mut s = WizardState::for_first_launch();
        s.session_candidates = vec![SessionCandidate {
            name: "tmuxA".into(),
            windows: vec!["h1".into(), "h2".into()],
            usable: true,
        }];
        s.selected_sessions.insert("tmuxA".into());
        s.session_overrides
            .insert("tmuxA".into(), ("renamed".into(), 0));
        s.form.name = "hand".into();
        s.form.members.insert("h3".into());
        s.commit_form().unwrap();
        let out = s.assemble_groups();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "renamed");
        assert_eq!(out[0].1.layout, "panes");
        assert_eq!(out[1].0, "hand");
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

    use crate::wizard::discovery::HostCandidate;
    use crate::wizard::discovery::SourceFlags;

    #[test]
    fn end_to_end_assemble_from_session_and_handbuilt() {
        let mut s = WizardState::for_first_launch();
        s.host_candidates = vec![
            HostCandidate {
                host: "h1".into(),
                sources: SourceFlags {
                    shell: true,
                    ..Default::default()
                },
            },
            HostCandidate {
                host: "h2".into(),
                sources: SourceFlags {
                    ssh_config: true,
                    ..Default::default()
                },
            },
        ];
        s.session_candidates = vec![SessionCandidate {
            name: "tmux1".into(),
            windows: vec!["wA".into(), "wB".into()],
            usable: true,
        }];
        s.selected_sessions.insert("tmux1".into());

        assert!(s.can_advance(Stage::Welcome).is_ok());
        s.selected_hosts.insert("h1".into());
        s.selected_hosts.insert("h2".into());
        assert!(s.can_advance(Stage::Hosts).is_ok());
        s.form.name = "all".into();
        s.form.layout_idx = 0;
        s.form.members.insert("h1".into());
        s.form.members.insert("h2".into());
        s.commit_form().unwrap();

        let groups = s.assemble_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "tmux1");
        assert_eq!(groups[0].1.layout, "windows");
        assert_eq!(groups[0].1.hosts, vec!["wA".to_string(), "wB".to_string()]);
        assert_eq!(groups[1].0, "all");
        assert_eq!(groups[1].1.layout, "panes");
        assert_eq!(groups[1].1.hosts, vec!["h1".to_string(), "h2".to_string()]);
    }
}
