//! Native TUI dashboard. Live updates every ~1.5s.

use anyhow::Result;
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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::{
    agents::{self, Agent},
    config, groups,
    sessions::{self, Session},
    snooze,
    theme::{self, Theme},
    tmux, ui_config,
};

/// `~/.local/state/tad/dashboard.state` — last view the user was on.
/// Falls back to the cache dir and finally `/tmp` so we always have a path.
fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tad")
        .join("dashboard.state")
}

/// Editable single-line text field with cursor + a "pristine prefill" flag.
/// When `pristine` is set, the first edit replaces the whole value — matches
/// browser autofill behavior so prefilled values don't get in the user's way.
#[derive(Default, Clone)]
struct TextInput {
    value: String,
    /// Byte index into `value` (UTF-8 aware).
    cursor: usize,
    pristine: bool,
}

impl TextInput {
    fn new() -> Self {
        Self::default()
    }
    fn pristine(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            value,
            cursor,
            pristine: true,
        }
    }
    fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.pristine = false;
    }
    fn insert(&mut self, c: char) {
        if self.pristine {
            self.clear();
        }
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }
    fn backspace(&mut self) {
        if self.pristine {
            self.clear();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let mut prev = self.cursor - 1;
        while prev > 0 && !self.value.is_char_boundary(prev) {
            prev -= 1;
        }
        self.value.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }
    fn delete(&mut self) {
        if self.pristine {
            self.clear();
            return;
        }
        if self.cursor >= self.value.len() {
            return;
        }
        let mut next = self.cursor + 1;
        while next < self.value.len() && !self.value.is_char_boundary(next) {
            next += 1;
        }
        self.value.replace_range(self.cursor..next, "");
    }
    fn left(&mut self) {
        self.pristine = false;
        if self.cursor == 0 {
            return;
        }
        let mut c = self.cursor - 1;
        while c > 0 && !self.value.is_char_boundary(c) {
            c -= 1;
        }
        self.cursor = c;
    }
    fn right(&mut self) {
        self.pristine = false;
        if self.cursor >= self.value.len() {
            return;
        }
        let mut c = self.cursor + 1;
        while c < self.value.len() && !self.value.is_char_boundary(c) {
            c += 1;
        }
        self.cursor = c;
    }
    fn home(&mut self) {
        self.pristine = false;
        self.cursor = 0;
    }
    fn end(&mut self) {
        self.pristine = false;
        self.cursor = self.value.len();
    }
    fn as_str(&self) -> &str {
        &self.value
    }
    fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Sessions,
    Groups,
    Hosts,
    Agents,
}

impl View {
    fn next(self) -> Self {
        match self {
            View::Sessions => View::Groups,
            View::Groups => View::Hosts,
            View::Hosts => View::Agents,
            View::Agents => View::Sessions,
        }
    }
    fn prev(self) -> Self {
        match self {
            View::Sessions => View::Agents,
            View::Groups => View::Sessions,
            View::Hosts => View::Groups,
            View::Agents => View::Hosts,
        }
    }
    fn title(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Groups => "Groups",
            View::Hosts => "Hosts",
            View::Agents => "Agents",
        }
    }
    fn index(self) -> usize {
        match self {
            View::Sessions => 0,
            View::Groups => 1,
            View::Hosts => 2,
            View::Agents => 3,
        }
    }
    fn slug(self) -> &'static str {
        match self {
            View::Sessions => "sessions",
            View::Groups => "groups",
            View::Hosts => "hosts",
            View::Agents => "agents",
        }
    }
    fn from_slug(s: &str) -> Option<Self> {
        match s.trim() {
            "sessions" => Some(View::Sessions),
            "groups" => Some(View::Groups),
            "hosts" => Some(View::Hosts),
            "agents" => Some(View::Agents),
            _ => None,
        }
    }
}

fn load_last_view() -> Option<View> {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| View::from_slug(&s))
}

fn save_last_view(view: View) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, view.slug());
}

struct AppData {
    sessions: Vec<Session>,
    groups: Vec<(String, config::Group)>,
    /// host → list of groups it belongs to
    hosts: Vec<(String, Vec<String>)>,
    /// Claude Code agents discovered across tmux panes.
    agents: Vec<Agent>,
}

impl AppData {
    fn load() -> Self {
        let mut sessions = sessions::list().unwrap_or_default();
        sessions.sort_by(|a, b| b.activity_ts.cmp(&a.activity_ts));
        let doc = config::load().unwrap_or_default();
        let mut groups: Vec<(String, config::Group)> = doc
            .groups
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        let mut hosts_map: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (gname, g) in &doc.groups {
            for h in &g.hosts {
                hosts_map.entry(h.clone()).or_default().push(gname.clone());
            }
        }
        for (_, gs) in hosts_map.iter_mut() {
            gs.sort();
        }
        let hosts: Vec<(String, Vec<String>)> = hosts_map.into_iter().collect();
        let agents = agents::scan();
        Self {
            sessions,
            groups,
            hosts,
            agents,
        }
    }
}

enum OpenTarget {
    /// Attach to an existing session by name, no prompt.
    AttachExisting(String),
    /// Create a new session, optionally running `ssh <host>` as its command.
    CreateNew {
        name: String,
        host: Option<String>,
    },
    Group(String),
    Host(String),
    /// Jump to a specific tmux pane by `session:window.pane` target.
    JumpToPane(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    None,
    Filter,
    NewSession,
    SnoozeSelect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewSessionField {
    Name,
    Host,
}

struct App {
    view: View,
    data: AppData,
    list_state_sessions: ListState,
    list_state_groups: ListState,
    list_state_hosts: ListState,
    list_state_agents: ListState,
    /// Cursor over `ui_config().snooze_intervals` in the snooze modal.
    snooze_cursor: usize,
    /// Set when launched via `--select-agent` (i.e. from the auto-popup):
    /// after the user snoozes or otherwise resolves the row, we exit the
    /// dashboard so they're back where they were.
    from_popup: bool,
    filter: TextInput,
    input_mode: InputMode,
    new_session_name: TextInput,
    new_session_host: TextInput,
    new_session_field: NewSessionField,
    should_quit: bool,
    open_after: Option<OpenTarget>,
    theme: Theme,
}

impl App {
    fn new() -> Self {
        let data = AppData::load();
        let mut s = ListState::default();
        s.select(if data.sessions.is_empty() {
            None
        } else {
            Some(0)
        });
        let mut g = ListState::default();
        g.select(if data.groups.is_empty() {
            None
        } else {
            Some(0)
        });
        let mut h = ListState::default();
        h.select(if data.hosts.is_empty() { None } else { Some(0) });
        let mut a = ListState::default();
        a.select(if data.agents.is_empty() {
            None
        } else {
            Some(0)
        });
        App {
            view: load_last_view().unwrap_or(View::Sessions),
            data,
            list_state_sessions: s,
            list_state_groups: g,
            list_state_hosts: h,
            list_state_agents: a,
            snooze_cursor: 0,
            from_popup: false,
            filter: TextInput::new(),
            input_mode: InputMode::None,
            new_session_name: TextInput::new(),
            new_session_host: TextInput::new(),
            new_session_field: NewSessionField::Name,
            should_quit: false,
            open_after: None,
            theme: theme::load(),
        }
    }

    fn list_state_mut(&mut self) -> &mut ListState {
        match self.view {
            View::Sessions => &mut self.list_state_sessions,
            View::Groups => &mut self.list_state_groups,
            View::Hosts => &mut self.list_state_hosts,
            View::Agents => &mut self.list_state_agents,
        }
    }

    fn items(&self) -> Vec<String> {
        // For Agents we key items by the unique target (session:window.pane)
        // so selection survives across refreshes even when window names
        // collide. Lookups go back through `data.agents`.
        let iter: Box<dyn Iterator<Item = String>> = match self.view {
            View::Sessions => Box::new(self.data.sessions.iter().map(|s| s.name.clone())),
            View::Groups => Box::new(self.data.groups.iter().map(|(n, _)| n.clone())),
            View::Hosts => Box::new(self.data.hosts.iter().map(|(n, _)| n.clone())),
            View::Agents => Box::new(self.data.agents.iter().map(|a| a.target.clone())),
        };
        if self.filter.is_empty() {
            iter.collect()
        } else {
            let f = self.filter.as_str().to_lowercase();
            iter.filter(|x| x.to_lowercase().contains(&f)).collect()
        }
    }

    fn selected(&self) -> Option<String> {
        let state = match self.view {
            View::Sessions => &self.list_state_sessions,
            View::Groups => &self.list_state_groups,
            View::Hosts => &self.list_state_hosts,
            View::Agents => &self.list_state_agents,
        };
        let idx = state.selected()?;
        self.items().get(idx).cloned()
    }

    fn refresh(&mut self) {
        self.data = AppData::load();
        // Clamp selections to new list sizes
        for (state, len) in [
            (&mut self.list_state_sessions, self.data.sessions.len()),
            (&mut self.list_state_groups, self.data.groups.len()),
            (&mut self.list_state_hosts, self.data.hosts.len()),
            (&mut self.list_state_agents, self.data.agents.len()),
        ] {
            match (state.selected(), len) {
                (_, 0) => state.select(None),
                (Some(i), n) if i >= n => state.select(Some(n - 1)),
                (None, _) => state.select(Some(0)),
                _ => {}
            }
        }
    }
}

/// Options for launching the dashboard. Today: open on a specific agent
/// row (used by `tad watch` when an agent goes idle and we pop a popup
/// from the auto-popup watcher).
#[derive(Debug, Default, Clone)]
pub struct RunOpts {
    /// If Some, the dashboard opens on the Agents view with the row whose
    /// `target` matches preselected. Missing-target = no preselection
    /// (we still open on Agents).
    pub select_agent: Option<String>,
}

pub fn run_with(opts: RunOpts) -> Result<i32> {
    // First-launch wizard: offer it when the user has no groups defined yet
    // (file missing, file empty, or `groups:` key absent). The wizard owns
    // its own terminal; on return, fall through to the dashboard.
    let needs_wizard = match crate::config::load() {
        Ok(doc) => doc.groups.is_empty(),
        Err(_) => true,
    };
    if needs_wizard {
        let _ = crate::wizard::run_first_launch();
    }
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = app_loop(&mut terminal, opts);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let app = result?;
    match app.open_after {
        Some(OpenTarget::AttachExisting(name)) => sessions::attach_or_create_silent(&name, None),
        Some(OpenTarget::CreateNew { name, host }) => {
            sessions::attach_or_create_silent(&name, host.as_deref())
        }
        Some(OpenTarget::Group(name)) => groups::open(&name, None),
        Some(OpenTarget::Host(name)) => sessions::attach_or_create_remote(&name),
        Some(OpenTarget::JumpToPane(target)) => jump_to_pane(&target),
        None => Ok(1),
    }
}

/// Jump to a tmux pane. When tad is invoked from inside a tmux client
/// (the common case via the popup keybind), `switch-client` flips us there
/// and the popup closes. Outside tmux, fall back to `attach -t` which
/// brings up the session containing the pane.
fn jump_to_pane(target: &str) -> Result<i32> {
    let inside_tmux = std::env::var_os("TMUX").is_some();
    if inside_tmux {
        let status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", target])
            .status();
        if matches!(&status, Ok(s) if s.success()) {
            return Ok(0);
        }
    }
    // Outside tmux: split target on ':' to get the session name and attach.
    let session = target.split(':').next().unwrap_or(target);
    let attach = std::process::Command::new("tmux")
        .args(["attach", "-t", session])
        .status();
    match attach {
        Ok(s) if s.success() => Ok(0),
        _ => Ok(1),
    }
}

fn app_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    opts: RunOpts,
) -> Result<App> {
    let mut app = App::new();
    // Honor --select-agent: jump to Agents and try to select the matching
    // row. If the target isn't in the scan (agent vanished between the
    // watcher noticing and us launching), we still open on Agents — the
    // first row stays selected.
    if let Some(target) = &opts.select_agent {
        app.view = View::Agents;
        app.from_popup = true;
        if let Some(idx) = app.data.agents.iter().position(|a| &a.target == target) {
            app.list_state_agents.select(Some(idx));
        }
    }
    let mut last_view = app.view;
    let mut last_refresh = Instant::now();
    let refresh_every = Duration::from_millis(1500);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if last_refresh.elapsed() > refresh_every {
            app.refresh();
            last_refresh = Instant::now();
        }

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.input_mode {
                    InputMode::Filter => handle_filter_key(&mut app, key),
                    InputMode::SnoozeSelect => handle_snooze_key(&mut app, key),
                    InputMode::NewSession => handle_new_session_key(&mut app, key),
                    InputMode::None => handle_key(&mut app, key.code, key.modifiers),
                }
            }
        }
        if app.view != last_view {
            save_last_view(app.view);
            last_view = app.view;
        }
        if app.should_quit {
            return Ok(app);
        }
    }
}

fn handle_filter_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let filter_before = app.filter.as_str().to_string();
    match key.code {
        // Navigate the (filtered) list without leaving filter mode — the whole
        // point: see a match, arrow to it, Enter, done.
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => move_selection(app, 10),
        KeyCode::PageUp => move_selection(app, -10),
        KeyCode::Tab => app.view = app.view.next(),
        KeyCode::BackTab => app.view = app.view.prev(),
        KeyCode::Enter => {
            if let Some(name) = app.selected() {
                app.open_after = Some(match app.view {
                    View::Sessions => OpenTarget::AttachExisting(name),
                    View::Groups => OpenTarget::Group(name),
                    View::Hosts => OpenTarget::Host(name),
                    View::Agents => OpenTarget::JumpToPane(name),
                });
                app.should_quit = true;
            }
        }
        // Esc clears the filter and exits filter mode in one step.
        KeyCode::Esc => {
            app.filter.clear();
            app.input_mode = InputMode::None;
        }
        // Backspace on an empty filter exits filter mode (mirrors fzf).
        KeyCode::Backspace if app.filter.is_empty() => app.input_mode = InputMode::None,
        KeyCode::Backspace => app.filter.backspace(),
        KeyCode::Delete => app.filter.delete(),
        KeyCode::Left => app.filter.left(),
        KeyCode::Right => app.filter.right(),
        KeyCode::Home => app.filter.home(),
        KeyCode::End => app.filter.end(),
        KeyCode::Char('u') if ctrl => app.filter.clear(),
        KeyCode::Char('a') if ctrl => app.filter.home(),
        KeyCode::Char('e') if ctrl => app.filter.end(),
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char(c) if !ctrl => app.filter.insert(c),
        _ => {}
    }
    let len = app.items().len();
    let filter_changed = app.filter.as_str() != filter_before;
    let state = app.list_state_mut();
    if filter_changed {
        // Typing narrows the list — snap to the top match so Enter just works.
        state.select(if len == 0 { None } else { Some(0) });
    } else {
        match state.selected() {
            Some(i) if i >= len => state.select(if len == 0 { None } else { Some(len - 1) }),
            None if len > 0 => state.select(Some(0)),
            _ => {}
        }
    }
}

fn handle_snooze_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let intervals = ui_config::load().snooze_intervals;
    match key.code {
        KeyCode::Esc => app.input_mode = InputMode::None,
        KeyCode::Up | KeyCode::Char('k') => {
            if app.snooze_cursor > 0 {
                app.snooze_cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.snooze_cursor + 1 < intervals.len() {
                app.snooze_cursor += 1;
            }
        }
        KeyCode::Enter => {
            if let (Some(target), Some(dur)) =
                (app.selected(), intervals.get(app.snooze_cursor).copied())
            {
                let _ = snooze::snooze(&target, dur);
                app.input_mode = InputMode::None;
                // If we got here from --select-agent (the auto-popup),
                // close the dashboard — the user's done responding to
                // the popup and wants to go back to whatever they were
                // doing. Otherwise just dismiss the modal and stay.
                if app.from_popup {
                    app.should_quit = true;
                }
            }
        }
        _ => {}
    }
}

fn handle_new_session_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.new_session_name.clear();
            app.new_session_host.clear();
            app.new_session_field = NewSessionField::Name;
            return;
        }
        KeyCode::Enter => {
            let name = app.new_session_name.as_str().trim().to_string();
            if name.is_empty() {
                return;
            }
            let host = {
                let h = app.new_session_host.as_str().trim();
                if h.is_empty() {
                    None
                } else {
                    Some(h.to_string())
                }
            };
            app.input_mode = InputMode::None;
            app.new_session_name.clear();
            app.new_session_host.clear();
            app.new_session_field = NewSessionField::Name;
            app.open_after = Some(OpenTarget::CreateNew { name, host });
            app.should_quit = true;
            return;
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
            app.new_session_field = match app.new_session_field {
                NewSessionField::Name => NewSessionField::Host,
                NewSessionField::Host => NewSessionField::Name,
            };
            return;
        }
        _ => {}
    }
    let field = match app.new_session_field {
        NewSessionField::Name => &mut app.new_session_name,
        NewSessionField::Host => &mut app.new_session_host,
    };
    match key.code {
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        KeyCode::Backspace => field.backspace(),
        KeyCode::Delete => field.delete(),
        KeyCode::Char('u') if ctrl => field.clear(),
        KeyCode::Char('a') if ctrl => field.home(),
        KeyCode::Char('e') if ctrl => field.end(),
        KeyCode::Char(c) if !ctrl => field.insert(c),
        _ => {}
    }
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab => app.view = app.view.next(),
        KeyCode::BackTab => app.view = app.view.prev(),
        KeyCode::Char('1') => app.view = View::Sessions,
        KeyCode::Char('2') => app.view = View::Groups,
        KeyCode::Char('3') => app.view = View::Hosts,
        KeyCode::Char('4') => app.view = View::Agents,
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::PageDown | KeyCode::Char('J') => move_selection(app, 10),
        KeyCode::PageUp | KeyCode::Char('K') => move_selection(app, -10),
        KeyCode::Home | KeyCode::Char('g') => {
            let len = app.items().len();
            if len > 0 {
                app.list_state_mut().select(Some(0));
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            let len = app.items().len();
            if len > 0 {
                app.list_state_mut().select(Some(len - 1));
            }
        }
        KeyCode::Enter => {
            if let Some(name) = app.selected() {
                app.open_after = Some(match app.view {
                    View::Sessions => OpenTarget::AttachExisting(name),
                    View::Groups => OpenTarget::Group(name),
                    View::Hosts => OpenTarget::Host(name),
                    View::Agents => OpenTarget::JumpToPane(name),
                });
                app.should_quit = true;
            }
        }
        KeyCode::Char('d') => {
            if app.view == View::Sessions {
                if let Some(name) = app.selected() {
                    tmux::kill_session(&name);
                    app.refresh();
                }
            }
        }
        KeyCode::Char('s') => {
            // Snooze the selected agent. Only meaningful in the Agents
            // view — in others it's a no-op.
            if app.view == View::Agents && app.selected().is_some() {
                app.snooze_cursor = 0;
                app.input_mode = InputMode::SnoozeSelect;
            }
        }
        KeyCode::Char('S') => {
            // Clear an active snooze on the selected agent. Useful if you
            // snoozed the wrong row or changed your mind.
            if app.view == View::Agents {
                if let Some(target) = app.selected() {
                    let _ = snooze::clear(&target);
                }
            }
        }
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Filter;
            app.filter.clear();
            let len = app.items().len();
            if len > 0 {
                app.list_state_mut().select(Some(0));
            }
        }
        KeyCode::Char('n') => {
            // Only the Hosts view prefills usefully (short→name, FQDN→ssh).
            // Sessions/Groups views start blank since 'n' means a brand-new
            // session that has nothing to do with the current selection.
            // Prefilled values are marked pristine so the first keystroke
            // replaces them — typing just works.
            match (app.view, app.selected()) {
                (View::Hosts, Some(h)) => {
                    app.new_session_name = TextInput::pristine(short_name(&h));
                    app.new_session_host = TextInput::pristine(h);
                }
                _ => {
                    app.new_session_name = TextInput::new();
                    app.new_session_host = TextInput::new();
                }
            }
            app.new_session_field = NewSessionField::Name;
            app.input_mode = InputMode::NewSession;
        }
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        _ => {}
    }
}

fn move_selection(app: &mut App, delta: i32) {
    let len = app.items().len() as i32;
    if len == 0 {
        return;
    }
    let cur = app.list_state_mut().selected().unwrap_or(0) as i32;
    let mut next = cur + delta;
    next = next.rem_euclid(len);
    app.list_state_mut().select(Some(next as usize));
}

fn ui(f: &mut Frame, app: &mut App) {
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
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    // Agents tab is annotated with live counts so you don't have to switch
    // into the view to know what's going on.
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

fn render_snooze_modal(f: &mut Frame, area: Rect, app: &App) {
    let intervals = ui_config::load().snooze_intervals;
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

fn render_new_session_modal(f: &mut Frame, area: Rect, app: &App) {
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

fn render_main(f: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
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
        format!(" {} ({}) ", app.view.title(), items_strs.len())
    };

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border))
                .title(title),
        )
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

fn format_session_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let s = match data.sessions.iter().find(|s| s.name == name) {
        Some(s) => s,
        None => return Line::from(name.to_string()),
    };
    let marker = if s.attached {
        Span::styled("● ", Style::default().fg(theme.success))
    } else {
        Span::raw("  ")
    };
    Line::from(vec![
        marker,
        Span::styled(
            format!("{:<22}", truncate(&s.name, 22)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>3}w  ", s.windows),
            Style::default().fg(theme.warning),
        ),
        Span::styled(
            format!("{:<12}", truncate(&s.active_window, 12)),
            Style::default().fg(theme.fg),
        ),
        Span::raw(" "),
        Span::styled(s.activity_str.clone(), Style::default().fg(theme.muted)),
    ])
}

fn format_group_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let g = match data.groups.iter().find(|(n, _)| n == name) {
        Some((_, g)) => g,
        None => return Line::from(name.to_string()),
    };
    Line::from(vec![
        Span::styled(
            format!("{:<28}", truncate(name, 28)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>3} hosts  ", g.hosts.len()),
            Style::default().fg(theme.warning),
        ),
        Span::styled(g.layout.clone(), Style::default().fg(theme.muted)),
    ])
}

fn format_agent_line(data: &AppData, target: &str, theme: &Theme) -> Line<'static> {
    let Some(agent) = data.agents.iter().find(|a| a.target == target) else {
        return Line::from(target.to_string());
    };
    let active_window = std::time::Duration::from_secs(30);
    let (marker_text, marker_style, status_text, status_style) =
        match agent.activity_status(active_window) {
            agents::ActivityStatus::Active(d) => (
                "● ",
                Style::default().fg(theme.success),
                format!("active · {}", agents::format_elapsed(d)),
                Style::default().fg(theme.success),
            ),
            agents::ActivityStatus::Idle(d) => (
                "○ ",
                Style::default().fg(theme.muted),
                format!("idle {}", agents::format_elapsed(d)),
                Style::default().fg(theme.muted),
            ),
            agents::ActivityStatus::NoTranscript => (
                "? ",
                Style::default().fg(theme.warning),
                "no transcript".to_string(),
                Style::default().fg(theme.warning),
            ),
        };
    let cwd_short = cwd_for_display(&agent.cwd);

    // If this target has an active snooze, append a "💤 in Xm" badge so
    // the user can see at a glance which rows the watcher is suppressing.
    let snoozes = snooze::load(std::time::SystemTime::now());
    let snooze_badge = snoozes.snoozes.get(target).and_then(|until| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if *until > now {
            Some(std::time::Duration::from_secs(*until - now))
        } else {
            None
        }
    });

    let mut spans = vec![
        Span::styled(marker_text, marker_style),
        Span::styled(
            format!("{:<22}", truncate(target, 22)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<28}", truncate(&cwd_short, 28)),
            Style::default().fg(theme.fg),
        ),
        Span::raw(" "),
        Span::styled(status_text, status_style),
    ];
    if let Some(d) = snooze_badge {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("snoozed {}", snooze::format_duration(d)),
            Style::default().fg(theme.warning),
        ));
    }
    Line::from(spans)
}

/// Shorten a cwd for display: replace $HOME prefix with `~`. The full path
/// is still shown in the preview pane.
fn cwd_for_display(p: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

fn format_host_line(data: &AppData, name: &str, theme: &Theme) -> Line<'static> {
    let in_groups = data
        .hosts
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, g)| g.clone())
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(
            format!("{:<45}", truncate(name, 45)),
            Style::default().fg(theme.accent),
        ),
        Span::raw("  "),
        Span::styled(in_groups.join(", "), Style::default().fg(theme.muted)),
    ])
}

fn render_preview(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border))
        .title(" preview ");
    let lines: Vec<Line> = match app.selected() {
        Some(name) => match app.view {
            View::Sessions => preview_session(&app.data, &name, &app.theme),
            View::Groups => preview_group(&app.data, &name, &app.theme),
            View::Hosts => preview_host(&app.data, &name, &app.theme),
            View::Agents => preview_agent(&app.data, &name, &app.theme),
        },
        None => vec![Line::from(Span::styled(
            "no selection",
            Style::default().fg(app.theme.muted),
        ))],
    };
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn preview_session(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    // Per-pane breakdown so the preview shows what's actually running where,
    // not just a window count. We cross-reference data.agents so panes
    // hosting a claude process get a marker — the Sessions view now hints
    // at the Agents view rather than feeling disconnected from it.
    let panes_raw = tmux::run([
        "list-panes",
        "-t",
        name,
        "-aF",
        // session\twindow_idx\twindow_name\tpane_idx\tpane_current_command\tpane_current_path
        "#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_command}\t#{pane_current_path}",
    ])
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    let meta = tmux::run([
        "display-message",
        "-p",
        "-t",
        name,
        "created: #{t:session_created}\nactivity: #{t:session_activity}",
    ])
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();
    let attached = tmux::run(["display-message", "-p", "-t", name, "#{session_attached}"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let mut lines = vec![Line::from(vec![
        Span::styled("session: ", Style::default().fg(theme.muted)),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    if attached != "0" && !attached.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  attached by {} client(s)", attached),
            Style::default().fg(theme.success),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  detached".to_string(),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));

    // Group panes by window for a sensible visual layout.
    let mut by_window: std::collections::BTreeMap<String, Vec<(String, String, String, String)>> =
        std::collections::BTreeMap::new();
    for line in panes_raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 6 {
            continue;
        }
        let window_key = format!("{:>3}: {}", parts[1], parts[2]);
        by_window.entry(window_key).or_default().push((
            parts[3].to_string(),
            parts[4].to_string(),
            parts[5].to_string(),
            format!("{}:{}.{}", parts[0], parts[1], parts[3]),
        ));
    }

    if by_window.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no panes — session probably just died)".to_string(),
            Style::default().fg(theme.muted),
        )));
    }

    for (window_label, panes) in by_window {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} ({} pane{})",
                window_label,
                panes.len(),
                if panes.len() == 1 { "" } else { "s" }
            ),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )));
        for (pane_idx, cmd, cwd, target) in panes {
            let is_agent = data.agents.iter().any(|a| a.target == target);
            // Lozenge marks claude-hosting panes so the Sessions view
            // surfaces what the Agents view tracks, without an emoji.
            let marker = if is_agent { "◆ " } else { "  " };
            let marker_style = if is_agent {
                Style::default().fg(theme.success)
            } else {
                Style::default().fg(theme.muted)
            };
            let cwd_short = cwd_for_display(std::path::Path::new(&cwd));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {}.", pane_idx),
                    Style::default().fg(theme.muted),
                ),
                Span::raw(" "),
                Span::styled(marker.to_string(), marker_style),
                Span::styled(
                    format!("{:<14}", truncate(&cmd, 14)),
                    Style::default().fg(theme.fg),
                ),
                Span::raw(" "),
                Span::styled(cwd_short, Style::default().fg(theme.muted)),
            ]));
        }
    }

    lines.push(Line::from(""));
    for l in meta.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(theme.muted),
        )));
    }
    lines
}

fn preview_group(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let g = match data.groups.iter().find(|(n, _)| n == name) {
        Some((_, g)) => g,
        None => return vec![Line::from("?")],
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("group: ", Style::default().fg(theme.muted)),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("layout: ", Style::default().fg(theme.muted)),
            Span::styled(g.layout.clone(), Style::default().fg(theme.warning)),
        ]),
        Line::from(Span::styled(
            format!("hosts ({}):", g.hosts.len()),
            Style::default().fg(theme.fg),
        )),
    ];
    for h in &g.hosts {
        lines.push(Line::from(Span::styled(
            format!("  {}", h),
            Style::default().fg(theme.fg),
        )));
    }
    lines
}

fn preview_host(data: &AppData, name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let in_groups: Vec<String> = data
        .hosts
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, g)| g.clone())
        .unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("host: ", Style::default().fg(theme.muted)),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled("member of:", Style::default().fg(theme.fg))),
    ];
    for g in &in_groups {
        lines.push(Line::from(Span::styled(
            format!("  {}", g),
            Style::default().fg(theme.fg),
        )));
    }
    lines
}

fn preview_agent(data: &AppData, target: &str, theme: &Theme) -> Vec<Line<'static>> {
    let Some(agent) = data.agents.iter().find(|a| a.target == target) else {
        return vec![Line::from(Span::styled(
            "agent gone — refresh",
            Style::default().fg(theme.muted),
        ))];
    };
    let active_window = std::time::Duration::from_secs(30);
    let status = match agent.activity_status(active_window) {
        agents::ActivityStatus::Active(d) => {
            format!("● active ({})", agents::format_elapsed(d))
        }
        agents::ActivityStatus::Idle(d) => {
            format!("○ idle for {}", agents::format_elapsed(d))
        }
        agents::ActivityStatus::NoTranscript => "? no transcript on disk".to_string(),
    };
    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{:<10}", k), Style::default().fg(theme.muted)),
            Span::styled(v, Style::default().fg(theme.fg)),
        ])
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("agent: ", Style::default().fg(theme.muted)),
            Span::styled(
                target.to_string(),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        kv("status", status),
        kv("session", agent.session.clone()),
        kv(
            "window",
            format!("{} ({})", agent.window_name, agent.window_index),
        ),
        kv("pane", agent.pane_index.clone()),
        kv("cwd", agent.cwd.display().to_string()),
        kv("pid", agent.claude_pid.to_string()),
    ];
    if let Some(tp) = &agent.transcript_path {
        let short = tp.file_name().map(|s| s.to_string_lossy().into_owned());
        if let Some(name) = short {
            lines.push(kv("transcript", name));
        }
    }
    // Surface an active snooze in the preview alongside the line badge,
    // so a user previewing a row knows the watcher is suppressing it.
    let snoozes = snooze::load(std::time::SystemTime::now());
    if let Some(until) = snoozes.snoozes.get(target) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if *until > now_secs {
            lines.push(kv(
                "snoozed",
                snooze::format_duration(std::time::Duration::from_secs(*until - now_secs)),
            ));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↵ jump to pane   s snooze   S clear snooze".to_string(),
        Style::default().fg(theme.muted),
    )));
    lines
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Strip any FQDN suffix to make a tmux-friendly session name.
fn short_name(s: &str) -> String {
    s.split('.').next().unwrap_or(s).to_string()
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}
