//! Native TUI dashboard. Live updates every ~1.5s.
//!
//! This file is the spine — it owns the shared state types (`App`,
//! `AppData`, `View`, `InputMode`, `TextInput`, `OpenTarget`-via-
//! `dispatch`), `state_path`/`load`/`save_last_view`, `RunOpts` and
//! `run_with`, and the event/render loop. Everything else lives in
//! a submodule:
//!
//!   * `keys`     — per-mode keyboard handlers + global `handle_key`
//!   * `render`   — `ui()` + tabs / main / status composition
//!   * `format`   — per-view row formatters + cwd/truncate helpers
//!   * `preview`  — per-view preview-pane builders
//!   * `modal`    — new-session / snooze / rename / confirm-kill overlays
//!   * `dispatch` — `OpenTarget` + post-dashboard tmux side effects
//!
//! Submodule items are `pub(super)` — visible across the dashboard
//! tree, not to the rest of the crate. The crate-public surface is
//! just `RunOpts` and `run_with`.

mod dispatch;
mod format;
mod keys;
mod modal;
mod preview;
mod render;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::{
    agents::{self, Agent},
    config, groups,
    sessions::{self, Session},
    theme::{self, Theme},
};

use dispatch::OpenTarget;

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
pub(super) struct TextInput {
    pub(super) value: String,
    /// Byte index into `value` (UTF-8 aware).
    pub(super) cursor: usize,
    pub(super) pristine: bool,
}

impl TextInput {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn pristine(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            value,
            cursor,
            pristine: true,
        }
    }
    pub(super) fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.pristine = false;
    }
    pub(super) fn insert(&mut self, c: char) {
        if self.pristine {
            self.clear();
        }
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }
    pub(super) fn backspace(&mut self) {
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
    pub(super) fn delete(&mut self) {
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
    pub(super) fn left(&mut self) {
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
    pub(super) fn right(&mut self) {
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
    pub(super) fn home(&mut self) {
        self.pristine = false;
        self.cursor = 0;
    }
    pub(super) fn end(&mut self) {
        self.pristine = false;
        self.cursor = self.value.len();
    }
    pub(super) fn as_str(&self) -> &str {
        &self.value
    }
    pub(super) fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Sessions,
    Groups,
    Hosts,
    Agents,
}

impl View {
    pub(super) fn next(self) -> Self {
        match self {
            View::Sessions => View::Groups,
            View::Groups => View::Hosts,
            View::Hosts => View::Agents,
            View::Agents => View::Sessions,
        }
    }
    pub(super) fn prev(self) -> Self {
        match self {
            View::Sessions => View::Agents,
            View::Groups => View::Sessions,
            View::Hosts => View::Groups,
            View::Agents => View::Hosts,
        }
    }
    pub(super) fn title(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Groups => "Groups",
            View::Hosts => "Hosts",
            View::Agents => "Agents",
        }
    }
    pub(super) fn index(self) -> usize {
        match self {
            View::Sessions => 0,
            View::Groups => 1,
            View::Hosts => 2,
            View::Agents => 3,
        }
    }
    pub(super) fn slug(self) -> &'static str {
        match self {
            View::Sessions => "sessions",
            View::Groups => "groups",
            View::Hosts => "hosts",
            View::Agents => "agents",
        }
    }
    /// Unrecognized slugs — including the legacy `"projects"` persisted
    /// by pre-removal versions — return None; the caller falls back to
    /// Sessions so an old state file can't strand the user.
    pub(super) fn from_slug(s: &str) -> Option<Self> {
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

/// Single-entry cache for the preview pane. Building a preview can
/// shell out to tmux and read transcript files, and the draw loop runs
/// several times a second — without this the selected row's preview
/// would redo that IO every frame. Keyed by (view, row, refresh
/// generation): selection moves and data refreshes miss naturally,
/// every other frame hits.
#[derive(Default)]
pub(super) struct PreviewCache {
    pub(super) key: Option<(View, String, u64)>,
    pub(super) lines: Vec<Line<'static>>,
}

pub(super) struct AppData {
    pub(super) sessions: Vec<Session>,
    pub(super) groups: Vec<(String, config::Group)>,
    /// host → list of groups it belongs to
    pub(super) hosts: Vec<HostRow>,
    /// Claude Code agents discovered across tmux panes.
    pub(super) agents: Vec<Agent>,
    /// Snooze map loaded once per refresh (~1.5s) so the per-row
    /// formatters don't each re-open the snooze file. Cheap to load
    /// (one small YAML), but multiplied by every visible Agents row
    /// and every preview render it was a real waste.
    pub(super) snoozes: crate::snooze::SnoozeState,
    /// User UI prefs cached per refresh for the same reason as
    /// `snoozes` — format_agent_line previously read `config.yaml`
    /// once per row per render.
    pub(super) ui: crate::ui_config::UiConfig,
}

/// One row in the Hosts view: a host name, the groups it belongs to (if
/// any), and a pre-rendered source tag from discovery.
#[derive(Debug, Clone)]
pub(super) struct HostRow {
    pub(super) name: String,
    pub(super) groups: Vec<String>,
    pub(super) source: String,
}

pub(super) fn build_host_rows(
    discovered: Vec<crate::discovery::HostCandidate>,
    group_members: std::collections::BTreeMap<String, Vec<String>>,
) -> Vec<HostRow> {
    let lc_members: std::collections::BTreeMap<String, Vec<String>> = group_members
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.clone()))
        .collect();
    let mut rows: Vec<HostRow> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for h in &discovered {
        let key = h.host.to_lowercase();
        seen.insert(key.clone());
        let groups = lc_members
            .get(&h.host.to_lowercase())
            .cloned()
            .unwrap_or_default();
        rows.push(HostRow {
            name: h.host.clone(),
            groups,
            source: crate::discovery::source_tag(h),
        });
    }
    for (host, groups) in &group_members {
        if seen.contains(&host.to_lowercase()) {
            continue;
        }
        rows.push(HostRow {
            name: host.clone(),
            groups: groups.clone(),
            source: String::new(),
        });
    }
    rows
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
        let discovered = crate::discovery::discover(&crate::discovery::DiscoveryConfig::load());
        let hosts = build_host_rows(discovered, hosts_map);
        let agents = agents::scan();
        let snoozes = crate::snooze::load(std::time::SystemTime::now());
        let ui = crate::ui_config::load();
        Self {
            sessions,
            groups,
            hosts,
            agents,
            snoozes,
            ui,
        }
    }
}

/// The one pane currently pulled beside tad, with everything needed to
/// send it home. All ids are tmux's stable handles (`%pane`, `@window`),
/// resolved at pull time so renames/shuffles can't misroute the return.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct PulledPane {
    pub(super) pane_id: String,
    pub(super) origin_window_id: String,
    pub(super) origin_session: String,
    pub(super) origin_window_name: String,
    pub(super) origin_window_index: String,
    /// Row label for the status line (`session:window`).
    pub(super) label: String,
}

/// Victim captured when the user pressed `d`, so the ~1.5s background
/// refresh can't swap what gets killed between arming and confirming.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ConfirmKillTarget {
    /// `tmux kill-session` — drops every pane in the session.
    Session { name: String },
    /// SIGINT to the agent's PID — gentle; pane and shell survive.
    /// The kill only needs `pid`; `target` records which row was
    /// armed and `window_name` feeds the modal text.
    Agent {
        target: String,
        pid: u32,
        window_name: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum InputMode {
    None,
    Filter,
    NewSession,
    SnoozeSelect,
    /// `R` on an Agents row: one-field modal prefilled with the
    /// agent's current window name. Enter renames the window in place
    /// (no dashboard exit; the next refresh shows the new name).
    RenameAgent,
    /// `d` on a Sessions/Agents row: y/N confirmation before the kill.
    /// Default is No — only y/Y/Enter confirm.
    ConfirmKill,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NewSessionField {
    Name,
    Host,
}

pub(super) struct App {
    pub(super) view: View,
    pub(super) data: AppData,
    pub(super) list_state_sessions: ListState,
    pub(super) list_state_groups: ListState,
    pub(super) list_state_hosts: ListState,
    pub(super) list_state_agents: ListState,
    /// Cursor over `data.ui.snooze_intervals` in the snooze modal.
    pub(super) snooze_cursor: usize,
    /// New window name being typed in the rename-agent modal.
    pub(super) rename_agent_text: TextInput,
    /// `session:window.pane` of the agent being renamed (captured at
    /// modal-open time so a mid-modal refresh doesn't drift the target).
    pub(super) rename_agent_target: Option<String>,
    /// Victim of the pending confirm-kill modal (captured at arm time).
    pub(super) confirm_kill: Option<ConfirmKillTarget>,
    /// Pane currently pulled beside tad, if any. See [`PulledPane`].
    pub(super) pulled_pane: Option<PulledPane>,
    /// Transient one-line status message (guard refusals). Cleared on
    /// the next keypress.
    pub(super) flash: Option<String>,
    /// Set when launched via `--select-agent`. The caller is scripting
    /// a "look at this one agent" flow, so after the user snoozes or
    /// otherwise resolves the row we exit so they return to wherever
    /// they came from.
    pub(super) from_popup: bool,
    pub(super) filter: TextInput,
    pub(super) input_mode: InputMode,
    pub(super) new_session_name: TextInput,
    pub(super) new_session_host: TextInput,
    pub(super) new_session_field: NewSessionField,
    pub(super) should_quit: bool,
    pub(super) open_after: Option<OpenTarget>,
    pub(super) theme: Theme,
    /// See [`PreviewCache`].
    pub(super) preview_cache: PreviewCache,
    /// Bumped by `refresh()`; part of the preview-cache key so cached
    /// preview lines never outlive the data they were built from.
    pub(super) refresh_generation: u64,
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
        // The Agents view's items() injects session-header rows; the
        // initial index 0 may land on a header. Snap to the next data
        // row so the first thing the user sees highlighted is an
        // actual agent, not a synthetic separator.
        snap_agents_selection_to_data_row(&mut a, &data);
        App {
            // Default to Sessions. Old `dashboard.state` still wins when
            // present; a legacy "projects" value falls back here too.
            view: load_last_view().unwrap_or(View::Sessions),
            data,
            list_state_sessions: s,
            list_state_groups: g,
            list_state_hosts: h,
            list_state_agents: a,
            snooze_cursor: 0,
            rename_agent_text: TextInput::new(),
            rename_agent_target: None,
            confirm_kill: None,
            pulled_pane: None,
            flash: None,
            from_popup: false,
            filter: TextInput::new(),
            input_mode: InputMode::None,
            new_session_name: TextInput::new(),
            new_session_host: TextInput::new(),
            new_session_field: NewSessionField::Name,
            should_quit: false,
            open_after: None,
            theme: theme::load(),
            preview_cache: PreviewCache::default(),
            refresh_generation: 0,
        }
    }

    pub(super) fn list_state_mut(&mut self) -> &mut ListState {
        match self.view {
            View::Sessions => &mut self.list_state_sessions,
            View::Groups => &mut self.list_state_groups,
            View::Hosts => &mut self.list_state_hosts,
            View::Agents => &mut self.list_state_agents,
        }
    }

    pub(super) fn items(&self) -> Vec<String> {
        let iter: Box<dyn Iterator<Item = String>> = match self.view {
            View::Sessions => Box::new(self.data.sessions.iter().map(|s| s.name.clone())),
            View::Groups => Box::new(self.data.groups.iter().map(|(n, _)| n.clone())),
            View::Hosts => Box::new(self.data.hosts.iter().map(|r| r.name.clone())),
            View::Agents => Box::new(agent_items_grouped_by_session(&self.data).into_iter()),
        };
        if self.filter.is_empty() {
            iter.collect()
        } else {
            let f = self.filter.as_str().to_lowercase();
            iter.filter(|x| x.to_lowercase().contains(&f)).collect()
        }
    }

    pub(super) fn selected(&self) -> Option<String> {
        let state = match self.view {
            View::Sessions => &self.list_state_sessions,
            View::Groups => &self.list_state_groups,
            View::Hosts => &self.list_state_hosts,
            View::Agents => &self.list_state_agents,
        };
        let idx = state.selected()?;
        let item = self.items().get(idx).cloned()?;
        // Header rows in the Agents view aren't "selections" for any
        // action key — return None so Enter / d / R / s / n no-op
        // and snooze/rename modals don't open with a header as their
        // target.
        if is_agent_header(&item) {
            return None;
        }
        Some(item)
    }

    pub(super) fn refresh(&mut self) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        self.data = AppData::load();
        // Clamp selections to new list sizes. The Agents view uses a
        // grouped list (session header rows interleaved with agent
        // rows), so the row count there comes from the grouped items()
        // helper rather than data.agents.len(), and the clamp needs
        // to land on a non-header row.
        let agents_view_len = agent_items_grouped_by_session(&self.data).len();
        for (state, len) in [
            (&mut self.list_state_sessions, self.data.sessions.len()),
            (&mut self.list_state_groups, self.data.groups.len()),
            (&mut self.list_state_hosts, self.data.hosts.len()),
            (&mut self.list_state_agents, agents_view_len),
        ] {
            match (state.selected(), len) {
                (_, 0) => state.select(None),
                (Some(i), n) if i >= n => state.select(Some(n - 1)),
                (None, _) => state.select(Some(0)),
                _ => {}
            }
        }
        // Ensure the Agents-view selection isn't on a synthetic header
        // row (would happen on first load and after a refresh narrows
        // the list).
        snap_agents_selection_to_data_row(&mut self.list_state_agents, &self.data);
    }

    /// Preview lines for the current selection, via the single-entry
    /// cache. Builders only run on a cache miss (selection moved or
    /// the data refreshed).
    pub(super) fn preview_lines(&mut self) -> Vec<Line<'static>> {
        let Some(name) = self.selected() else {
            return vec![Line::from(Span::styled(
                "no selection",
                Style::default().fg(self.theme.muted),
            ))];
        };
        let key = (self.view, name.clone(), self.refresh_generation);
        if self.preview_cache.key.as_ref() != Some(&key) {
            let lines = match self.view {
                View::Sessions => preview::preview_session(&self.data, &name, &self.theme),
                View::Groups => preview::preview_group(&self.data, &name, &self.theme),
                View::Hosts => preview::preview_host(&self.data, &name, &self.theme),
                View::Agents => preview::preview_agent(&self.data, &name, &self.theme),
            };
            self.preview_cache = PreviewCache {
                key: Some(key),
                lines,
            };
        }
        // Intentional per-frame clone: tens of Lines at ~5fps is noise,
        // and returning a borrow would tie Paragraph's lifetime to App.
        self.preview_cache.lines.clone()
    }
}

/// If the agents-view list state is parked on a header row, advance
/// it forward to the next data row. No-op if the selection is already
/// on data, or if no agents exist at all.
pub(super) fn snap_agents_selection_to_data_row(state: &mut ListState, data: &AppData) {
    let items = agent_items_grouped_by_session(data);
    if items.is_empty() {
        return;
    }
    let Some(cur) = state.selected() else {
        return;
    };
    let mut idx = cur.min(items.len() - 1);
    let start = idx;
    while is_agent_header(&items[idx]) {
        idx = (idx + 1) % items.len();
        if idx == start {
            // Pathological all-headers case (can't happen since a
            // session only appears when at least one agent belongs to it, but be safe).
            return;
        }
    }
    state.select(Some(idx));
}

/// Sentinel that prefixes Agents-view "header" rows — synthetic
/// non-selectable entries inserted between session groups to visually
/// separate them. Chosen as ASCII bell + a leading sigil because no
/// tmux target / agent name will ever contain those characters; the
/// `§` is the visible part of the rendered header.
pub(super) const AGENT_HEADER_SIGIL: &str = "\x07§";

pub(super) fn is_agent_header(item: &str) -> bool {
    item.starts_with(AGENT_HEADER_SIGIL)
}

/// Build the Agents-view item list: group agents by their tmux session,
/// most-recently-active session first, then emit one header row followed
/// by that session's agent target rows (most-recent agent first). The
/// header carries the session name and an at-a-glance summary
/// (`N agents · M awaiting`) so the busiest sessions read first.
fn agent_items_grouped_by_session(data: &AppData) -> Vec<String> {
    let mut by_session: Vec<(String, Vec<&Agent>)> = Vec::new();
    for a in &data.agents {
        match by_session.iter_mut().find(|(s, _)| s == &a.session) {
            Some((_, v)) => v.push(a),
            None => by_session.push((a.session.clone(), vec![a])),
        }
    }
    // Most-recently-active session first; None activity sorts last.
    by_session.sort_by_key(|(_, agents)| {
        std::cmp::Reverse(agents.iter().filter_map(|a| a.last_activity).max())
    });
    let mut items = Vec::new();
    for (session, mut agents) in by_session {
        agents.sort_by_key(|a| std::cmp::Reverse(a.last_activity));
        let awaiting = agents
            .iter()
            .filter(|a| a.attention == crate::transcript::Attention::AwaitingInput)
            .count();
        let plural = if agents.len() == 1 { "" } else { "s" };
        let suffix = if awaiting > 0 {
            format!(" · {awaiting} awaiting")
        } else {
            String::new()
        };
        items.push(format!(
            "{AGENT_HEADER_SIGIL} {} · {} agent{plural}{suffix}",
            session,
            agents.len()
        ));
        for a in agents {
            items.push(a.target.clone());
        }
    }
    items
}

/// Options for launching the dashboard. Today: open on a specific agent
/// row (`--select-agent <target>` — useful for scripts that want to
/// jump straight to a particular agent).
#[derive(Debug, Default, Clone)]
pub struct RunOpts {
    /// If Some, the dashboard opens on the Agents view with the row whose
    /// `target` matches preselected. Missing-target = no preselection
    /// (we still open on Agents).
    pub select_agent: Option<String>,
}

pub fn run_with(opts: RunOpts) -> Result<i32> {
    // The dashboard runs unconditionally. Sessions and agents show with no
    // config needed; `tad config` opens the groups editor when the user
    // wants to define groups.
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
        Some(OpenTarget::JumpToPane(target)) => dispatch::jump_to_pane(&target),
        None => Ok(1),
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
        terminal.draw(|f| render::ui(f, &mut app))?;

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
                    InputMode::Filter => keys::handle_filter_key(&mut app, key),
                    InputMode::SnoozeSelect => keys::handle_snooze_key(&mut app, key),
                    InputMode::NewSession => keys::handle_new_session_key(&mut app, key),
                    InputMode::RenameAgent => keys::handle_rename_agent_key(&mut app, key),
                    InputMode::ConfirmKill => keys::handle_confirm_kill_key(&mut app, key),
                    InputMode::None => keys::handle_key(&mut app, key.code, key.modifiers),
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

/// Shared test fixtures for the dashboard tree's test modules
/// (dashboard.rs, keys.rs, preview.rs). Compiled only for tests.
#[cfg(test)]
pub(super) mod testutil {
    use super::*;

    pub(super) fn mk_agent(target: &str, session: &str, secs: u64) -> Agent {
        Agent {
            target: target.into(),
            session: session.into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/repo"),
            agent_pid: 1,
            provider_id: "claude",
            last_activity: Some(std::time::UNIX_EPOCH + Duration::from_secs(secs)),
            transcript_path: None,
            attention: crate::transcript::Attention::Unknown,
        }
    }

    pub(super) fn mk_session(name: &str) -> Session {
        Session {
            name: name.into(),
            windows: 1,
            attached: false,
            active_window: "w".into(),
            active_path: "/repo".into(),
            created_ts: 0,
            activity_ts: 0,
            activity_str: "1m".into(),
        }
    }

    pub(super) fn mk_data(sessions: Vec<Session>, agents: Vec<Agent>) -> AppData {
        AppData {
            sessions,
            groups: vec![],
            hosts: vec![],
            agents,
            snoozes: crate::snooze::SnoozeState::default(),
            ui: crate::ui_config::UiConfig::default(),
        }
    }

    pub(super) fn mk_app(view: View, data: AppData) -> App {
        let mut list = ListState::default();
        list.select(Some(0));
        App {
            view,
            data,
            list_state_sessions: list.clone(),
            list_state_groups: list.clone(),
            list_state_hosts: ListState::default(),
            list_state_agents: list,
            snooze_cursor: 0,
            rename_agent_text: TextInput::new(),
            rename_agent_target: None,
            confirm_kill: None,
            pulled_pane: None,
            flash: None,
            from_popup: false,
            filter: TextInput::new(),
            input_mode: InputMode::None,
            new_session_name: TextInput::new(),
            new_session_host: TextInput::new(),
            new_session_field: NewSessionField::Name,
            should_quit: false,
            open_after: None,
            theme: crate::theme::load(),
            preview_cache: PreviewCache::default(),
            refresh_generation: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{mk_agent, mk_data};
    use super::*;

    fn mk_data_with_group(layout: &str) -> AppData {
        let mut data = mk_data(vec![], vec![]);
        data.groups = vec![(
            "prod".to_string(),
            crate::config::Group {
                layout: layout.to_string(),
                hosts: vec!["web1".to_string()],
            },
        )];
        data
    }

    #[test]
    fn preview_cache_serves_stale_until_generation_bumps() {
        let mut app = testutil::mk_app(View::Groups, mk_data_with_group("panes"));
        let first = format!("{:?}", app.preview_lines());
        // Mutate underlying data WITHOUT bumping the generation — the
        // cache must serve the stale lines (that's the point: no
        // rebuild per frame).
        app.data.groups[0].1.layout = "windows".to_string();
        let cached = format!("{:?}", app.preview_lines());
        assert_eq!(first, cached);
        // What refresh() does each ~1.5s tick:
        app.refresh_generation = app.refresh_generation.wrapping_add(1);
        let rebuilt = format!("{:?}", app.preview_lines());
        assert_ne!(cached, rebuilt);
        assert!(rebuilt.contains("windows"));
    }

    #[test]
    fn preview_cache_misses_on_selection_change() {
        let mut app = testutil::mk_app(View::Groups, mk_data_with_group("panes"));
        app.data.groups.push((
            "staging".to_string(),
            crate::config::Group {
                layout: "browse".to_string(),
                hosts: vec![],
            },
        ));
        let first = format!("{:?}", app.preview_lines());
        app.list_state_groups.select(Some(1));
        let second = format!("{:?}", app.preview_lines());
        assert_ne!(first, second);
        assert!(second.contains("staging"));
    }

    #[test]
    fn preview_lines_without_selection_says_so() {
        let mut app = testutil::mk_app(View::Hosts, mk_data(vec![], vec![]));
        // mk_app leaves list_state_hosts unselected.
        let lines = format!("{:?}", app.preview_lines());
        assert!(lines.contains("no selection"));
    }

    #[test]
    fn grouped_items_emit_header_then_agents_per_session() {
        // cops has the most recent activity (300) so it sorts first;
        // salt's agents sort most-recent-first within the session.
        let data = mk_data(
            vec![],
            vec![
                mk_agent("salt:1.0", "salt", 100),
                mk_agent("salt:2.0", "salt", 200),
                mk_agent("cops:1.0", "cops", 300),
            ],
        );
        let items = agent_items_grouped_by_session(&data);
        // header, agent, header, agent, agent
        assert_eq!(items.len(), 5);
        assert!(is_agent_header(&items[0]));
        assert!(items[0].contains("cops"));
        assert!(items[0].contains("1 agent"));
        assert!(!items[0].contains("1 agents")); // singular wording
        assert_eq!(items[1], "cops:1.0");
        assert!(is_agent_header(&items[2]));
        assert!(items[2].contains("salt"));
        assert!(items[2].contains("2 agents"));
        assert_eq!(items[3], "salt:2.0"); // most-recent agent first
        assert_eq!(items[4], "salt:1.0");
    }

    #[test]
    fn header_summary_includes_awaiting_count_when_nonzero() {
        let mut a = mk_agent("salt:1.0", "salt", 100);
        a.attention = crate::transcript::Attention::AwaitingInput;
        let data = mk_data(vec![], vec![a]);
        let items = agent_items_grouped_by_session(&data);
        assert!(items[0].contains("1 awaiting"));
    }

    #[test]
    fn snap_skips_a_header_at_the_initial_position() {
        let data = mk_data(vec![], vec![mk_agent("salt:1.0", "salt", 100)]);
        let mut state = ListState::default();
        state.select(Some(0)); // would land on the header
        snap_agents_selection_to_data_row(&mut state, &data);
        // Snapped forward past the header to the agent row.
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn build_host_rows_matches_group_membership_case_insensitively() {
        use crate::discovery::{HostCandidate, SourceFlags};
        let discovered = vec![HostCandidate {
            host: "web1".into(),
            sources: SourceFlags {
                shell: true,
                ..Default::default()
            },
            count: 5,
        }];
        let mut group_members: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        group_members.insert("Web1".into(), vec!["prod".into()]); // different casing than discovered
        let rows = build_host_rows(discovered, group_members);
        // exactly one row for web1 (deduped), and it carries the group
        let web1: Vec<_> = rows
            .iter()
            .filter(|r| r.name.eq_ignore_ascii_case("web1"))
            .collect();
        assert_eq!(web1.len(), 1);
        assert_eq!(web1[0].groups, vec!["prod".to_string()]);
    }

    #[test]
    fn legacy_projects_slug_is_not_a_view() {
        // Pre-removal builds persisted "projects" in dashboard.state;
        // from_slug must reject it so App::new falls back to Sessions.
        assert!(View::from_slug("projects").is_none());
        assert!(matches!(View::from_slug("sessions"), Some(View::Sessions)));
    }

    #[test]
    fn build_host_rows_unions_discovered_and_group_members() {
        use crate::discovery::{HostCandidate, SourceFlags};
        let discovered = vec![HostCandidate {
            host: "web1".into(),
            sources: SourceFlags {
                ssh_config: true,
                ..Default::default()
            },
            count: 0,
        }];
        let mut group_members: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        group_members.insert("db1".into(), vec!["prod".into()]); // only in a group, not discovered
        let rows = build_host_rows(discovered, group_members);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"web1"));
        assert!(names.contains(&"db1")); // group member still shows
        let web1 = rows.iter().find(|r| r.name == "web1").unwrap();
        assert!(web1.source.contains("ssh-config"));
    }
}
