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
//!   * `modal`    — new-session / snooze / new-agent overlays
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
use ratatui::{backend::CrosstermBackend, widgets::ListState, Terminal};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::{
    agents::{self, Agent},
    config, groups,
    projects::{self, Project},
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
    Projects,
    Sessions,
    Groups,
    Hosts,
    Agents,
}

impl View {
    pub(super) fn next(self) -> Self {
        match self {
            View::Projects => View::Sessions,
            View::Sessions => View::Groups,
            View::Groups => View::Hosts,
            View::Hosts => View::Agents,
            View::Agents => View::Projects,
        }
    }
    pub(super) fn prev(self) -> Self {
        match self {
            View::Projects => View::Agents,
            View::Sessions => View::Projects,
            View::Groups => View::Sessions,
            View::Hosts => View::Groups,
            View::Agents => View::Hosts,
        }
    }
    pub(super) fn title(self) -> &'static str {
        match self {
            View::Projects => "Projects",
            View::Sessions => "Sessions",
            View::Groups => "Groups",
            View::Hosts => "Hosts",
            View::Agents => "Agents",
        }
    }
    pub(super) fn index(self) -> usize {
        match self {
            View::Projects => 0,
            View::Sessions => 1,
            View::Groups => 2,
            View::Hosts => 3,
            View::Agents => 4,
        }
    }
    pub(super) fn slug(self) -> &'static str {
        match self {
            View::Projects => "projects",
            View::Sessions => "sessions",
            View::Groups => "groups",
            View::Hosts => "hosts",
            View::Agents => "agents",
        }
    }
    pub(super) fn from_slug(s: &str) -> Option<Self> {
        match s.trim() {
            "projects" => Some(View::Projects),
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

pub(super) struct AppData {
    pub(super) sessions: Vec<Session>,
    pub(super) groups: Vec<(String, config::Group)>,
    /// host → list of groups it belongs to
    pub(super) hosts: Vec<(String, Vec<String>)>,
    /// Claude Code agents discovered across tmux panes.
    pub(super) agents: Vec<Agent>,
    /// Project (typically git repo) frame around everything else.
    /// Derived from session/agent cwds — the primary noun.
    pub(super) projects: Vec<Project>,
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
        // Projects are a pure aggregation over the same sessions+agents
        // — pass the slices in so we don't re-run the tmux subprocess
        // and /proc walk a second time per refresh.
        let project_list = projects::from_scanned(&sessions, &agents);
        let snoozes = crate::snooze::load(std::time::SystemTime::now());
        let ui = crate::ui_config::load();
        Self {
            sessions,
            groups,
            hosts,
            agents,
            projects: project_list,
            snoozes,
            ui,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InputMode {
    None,
    Filter,
    NewSession,
    SnoozeSelect,
    /// `n` on a Projects row: one-field modal collecting an optional
    /// initial prompt, then spawning `claude` in a new tmux window in
    /// the project's root.
    NewAgent,
    /// `R` on an Agents row: one-field modal prefilled with the
    /// agent's current window name. Enter renames the window in place
    /// (no dashboard exit; the next refresh shows the new name).
    RenameAgent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NewSessionField {
    Name,
    Host,
}

pub(super) struct App {
    pub(super) view: View,
    pub(super) data: AppData,
    pub(super) list_state_projects: ListState,
    pub(super) list_state_sessions: ListState,
    pub(super) list_state_groups: ListState,
    pub(super) list_state_hosts: ListState,
    pub(super) list_state_agents: ListState,
    /// Cursor over `data.ui.snooze_intervals` in the snooze modal.
    pub(super) snooze_cursor: usize,
    /// Initial prompt text input for the new-agent modal. Optional —
    /// empty just spawns `claude` with no preset prompt.
    pub(super) new_agent_prompt: TextInput,
    /// Project the new-agent modal is targeting (captured when the
    /// modal opens, so a mid-modal refresh doesn't drift the target).
    pub(super) new_agent_project: Option<String>,
    /// New window name being typed in the rename-agent modal.
    pub(super) rename_agent_text: TextInput,
    /// `session:window.pane` of the agent being renamed (captured at
    /// modal-open time for the same reason as `new_agent_project`).
    pub(super) rename_agent_target: Option<String>,
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
        // The Agents view's items() injects project-header rows; the
        // initial index 0 may land on a header. Snap to the next data
        // row so the first thing the user sees highlighted is an
        // actual agent, not a synthetic separator.
        snap_agents_selection_to_data_row(&mut a, &data);
        let mut p = ListState::default();
        p.select(if data.projects.is_empty() {
            None
        } else {
            Some(0)
        });
        App {
            // Default to Projects — the user-facing primary noun for
            // the cockpit. Old `dashboard.state` still wins when present
            // so power users who lived on Sessions stay there.
            view: load_last_view().unwrap_or(View::Projects),
            data,
            list_state_projects: p,
            list_state_sessions: s,
            list_state_groups: g,
            list_state_hosts: h,
            list_state_agents: a,
            snooze_cursor: 0,
            new_agent_prompt: TextInput::new(),
            new_agent_project: None,
            rename_agent_text: TextInput::new(),
            rename_agent_target: None,
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

    pub(super) fn list_state_mut(&mut self) -> &mut ListState {
        match self.view {
            View::Projects => &mut self.list_state_projects,
            View::Sessions => &mut self.list_state_sessions,
            View::Groups => &mut self.list_state_groups,
            View::Hosts => &mut self.list_state_hosts,
            View::Agents => &mut self.list_state_agents,
        }
    }

    pub(super) fn items(&self) -> Vec<String> {
        // Projects are keyed by their `name` (basename of root); when
        // two roots collide on name the latter shadows the former in
        // the list, which is acceptable for a v1 (caller can rename
        // its directory or we can disambiguate with the parent later).
        let iter: Box<dyn Iterator<Item = String>> = match self.view {
            View::Projects => Box::new(self.data.projects.iter().map(|p| p.name.clone())),
            View::Sessions => Box::new(self.data.sessions.iter().map(|s| s.name.clone())),
            View::Groups => Box::new(self.data.groups.iter().map(|(n, _)| n.clone())),
            View::Hosts => Box::new(self.data.hosts.iter().map(|(n, _)| n.clone())),
            View::Agents => Box::new(agent_items_grouped_by_project(&self.data).into_iter()),
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
            View::Projects => &self.list_state_projects,
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
        self.data = AppData::load();
        // Clamp selections to new list sizes. The Agents view uses a
        // grouped list (project header rows interleaved with agent
        // rows), so the row count there comes from the grouped items()
        // helper rather than data.agents.len(), and the clamp needs
        // to land on a non-header row.
        let agents_view_len = agent_items_grouped_by_project(&self.data).len();
        for (state, len) in [
            (&mut self.list_state_projects, self.data.projects.len()),
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
}

/// If the agents-view list state is parked on a header row, advance
/// it forward to the next data row. No-op if the selection is already
/// on data, or if no agents exist at all.
pub(super) fn snap_agents_selection_to_data_row(state: &mut ListState, data: &AppData) {
    let items = agent_items_grouped_by_project(data);
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
            // Pathological all-headers case (can't happen since we
            // skip empty projects, but be safe).
            return;
        }
    }
    state.select(Some(idx));
}

/// Sentinel that prefixes Agents-view "header" rows — synthetic
/// non-selectable entries inserted between project groups to visually
/// separate them. Chosen as ASCII bell + a leading sigil because no
/// tmux target / agent name will ever contain those characters; the
/// `§` is the visible part of the rendered header.
pub(super) const AGENT_HEADER_SIGIL: &str = "\x07§";

pub(super) fn is_agent_header(item: &str) -> bool {
    item.starts_with(AGENT_HEADER_SIGIL)
}

/// Build the Agents-view item list: for each project (in the order
/// projects::scan already sorted them — most-recently-active first),
/// emit one header row followed by that project's agent target rows.
/// Projects with zero agents are skipped. The header row carries the
/// project name and an at-a-glance summary (`N agents · M awaiting`)
/// so the heaviest projects sort and read first.
fn agent_items_grouped_by_project(data: &AppData) -> Vec<String> {
    let mut items = Vec::new();
    for project in &data.projects {
        if project.agents.is_empty() {
            continue;
        }
        let awaiting = project
            .agents
            .iter()
            .filter(|a| a.attention == crate::transcript::Attention::AwaitingInput)
            .count();
        let plural = if project.agents.len() == 1 { "" } else { "s" };
        let suffix = if awaiting > 0 {
            format!(" · {awaiting} awaiting")
        } else {
            String::new()
        };
        items.push(format!(
            "{AGENT_HEADER_SIGIL} {} · {} agent{plural}{suffix}",
            project.name,
            project.agents.len()
        ));
        for a in &project.agents {
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
        Some(OpenTarget::JumpToPane(target)) => dispatch::jump_to_pane(&target),
        Some(OpenTarget::SpawnAgent {
            project_name,
            prompt,
        }) => dispatch::spawn_agent_in_project(&project_name, prompt.as_deref()),
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
    } else if app.view == View::Projects {
        // Cwd-aware preselection: if the user launched tad from inside a
        // known project, drop them on that project's row. Turns the
        // dashboard from "browser of all projects" into "default to where
        // I already am, browse when I want."
        if let Some(idx) = dispatch::current_project_index(&app.data.projects) {
            app.list_state_projects.select(Some(idx));
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
                    InputMode::NewAgent => keys::handle_new_agent_key(&mut app, key),
                    InputMode::NewSession => keys::handle_new_session_key(&mut app, key),
                    InputMode::RenameAgent => keys::handle_rename_agent_key(&mut app, key),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Agent;
    use crate::projects::Project;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn mk_agent(target: &str, cwd: &str) -> Agent {
        Agent {
            target: target.into(),
            session: target.split(':').next().unwrap_or("").into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from(cwd),
            agent_pid: 1,
            provider_id: "claude",
            last_activity: Some(SystemTime::now()),
            transcript_path: None,
            attention: crate::transcript::Attention::Unknown,
        }
    }

    fn mk_data(projects: Vec<Project>, agents: Vec<Agent>) -> AppData {
        AppData {
            sessions: vec![],
            groups: vec![],
            hosts: vec![],
            agents,
            projects,
            snoozes: crate::snooze::SnoozeState::default(),
            ui: crate::ui_config::UiConfig::default(),
        }
    }

    #[test]
    fn grouped_items_emit_header_then_agents_per_project() {
        let a1 = mk_agent("salt:1.0", "/repo/salt");
        let a2 = mk_agent("salt:2.0", "/repo/salt");
        let a3 = mk_agent("cops:1.0", "/repo/cops");
        let data = mk_data(
            vec![
                Project {
                    root: PathBuf::from("/repo/salt"),
                    name: "salt".into(),
                    sessions: vec![],
                    agents: vec![a1.clone(), a2.clone()],
                    last_activity: a1.last_activity,
                },
                Project {
                    root: PathBuf::from("/repo/cops"),
                    name: "cops".into(),
                    sessions: vec![],
                    agents: vec![a3.clone()],
                    last_activity: a3.last_activity,
                },
            ],
            vec![a1, a2, a3],
        );

        let items = agent_items_grouped_by_project(&data);
        // header, agent, agent, header, agent
        assert_eq!(items.len(), 5);
        assert!(is_agent_header(&items[0]));
        assert!(items[0].contains("salt"));
        assert!(items[0].contains("2 agents"));
        assert_eq!(items[1], "salt:1.0");
        assert_eq!(items[2], "salt:2.0");
        assert!(is_agent_header(&items[3]));
        assert!(items[3].contains("cops"));
        assert!(items[3].contains("1 agent"));
        assert!(!items[3].contains("1 agents")); // singular wording
        assert_eq!(items[4], "cops:1.0");
    }

    #[test]
    fn grouped_items_skip_projects_with_no_agents() {
        let a = mk_agent("salt:1.0", "/repo/salt");
        let data = mk_data(
            vec![
                Project {
                    root: PathBuf::from("/repo/salt"),
                    name: "salt".into(),
                    sessions: vec![],
                    agents: vec![a.clone()],
                    last_activity: a.last_activity,
                },
                // Empty project (in projects list because it has a
                // tmux session but no live claude in any pane) — must
                // not emit a header.
                Project {
                    root: PathBuf::from("/repo/dormant"),
                    name: "dormant".into(),
                    sessions: vec![],
                    agents: vec![],
                    last_activity: None,
                },
            ],
            vec![a],
        );
        let items = agent_items_grouped_by_project(&data);
        assert_eq!(items.len(), 2);
        assert!(is_agent_header(&items[0]));
        assert!(!items.iter().any(|i| i.contains("dormant")));
    }

    #[test]
    fn header_summary_includes_awaiting_count_when_nonzero() {
        let mut a = mk_agent("salt:1.0", "/repo/salt");
        a.attention = crate::transcript::Attention::AwaitingInput;
        let data = mk_data(
            vec![Project {
                root: PathBuf::from("/repo/salt"),
                name: "salt".into(),
                sessions: vec![],
                agents: vec![a.clone()],
                last_activity: a.last_activity,
            }],
            vec![a],
        );
        let items = agent_items_grouped_by_project(&data);
        assert!(items[0].contains("1 awaiting"));
    }

    #[test]
    fn snap_skips_a_header_at_the_initial_position() {
        let a = mk_agent("salt:1.0", "/repo/salt");
        let data = mk_data(
            vec![Project {
                root: PathBuf::from("/repo/salt"),
                name: "salt".into(),
                sessions: vec![],
                agents: vec![a.clone()],
                last_activity: a.last_activity,
            }],
            vec![a],
        );
        let mut state = ListState::default();
        state.select(Some(0)); // would land on the header
        snap_agents_selection_to_data_row(&mut state, &data);
        // Snapped forward past the header to the agent row.
        assert_eq!(state.selected(), Some(1));
    }
}
