//! Native TUI dashboard. Live updates every ~1.5s.
//!
//! This file is the spine — it owns the shared state types (`App`,
//! `AppData`, `InputMode`, `TextInput`, `OpenTarget`-via-`dispatch`),
//! `state_path`/`load_state`/`save_state`, `RunOpts` and `run_with`,
//! and the event/render loop. Everything else lives in a submodule:
//!
//!   * `rows`     — pure row-tree model (`Section`, `RowKind`, `Row`,
//!     `build_rows`, cursor-movement helpers)
//!   * `grid`     — pin-grid decision logic (multi-pane precursor)
//!   * `keys`     — per-mode keyboard handlers + global `handle_key`
//!   * `render`   — `ui()` + header / main / status composition
//!   * `format`   — per-row-kind formatters + cwd/truncate helpers
//!   * `preview`  — per-row-kind preview-pane builders
//!   * `modal`    — new-session / snooze / rename / confirm-kill overlays
//!   * `dispatch` — `OpenTarget` + post-dashboard tmux side effects
//!
//! Submodule items are `pub(super)` — visible across the dashboard
//! tree, not to the rest of the crate. The crate-public surface is
//! just `RunOpts` and `run_with`.

mod dispatch;
mod format;
mod grid;
mod keys;
mod modal;
mod preview;
mod render;
mod rows;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{
    agents::{self, Agent},
    config, groups,
    sessions::{self, Session},
    theme::{self, Theme},
};

use dispatch::OpenTarget;

/// `~/.local/state/tad/dashboard.state` — the user's cursor position,
/// collapsed sections and sidebar width. Falls back to the cache dir
/// and finally `/tmp` so we always have a path.
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

/// Single-entry cache for the preview pane. Building a preview can
/// shell out to tmux and read transcript files, and the draw loop runs
/// several times a second — without this the selected row's preview
/// would redo that IO every frame. Keyed by (row kind, refresh
/// generation): selection moves and data refreshes miss naturally,
/// every other frame hits.
#[derive(Default)]
pub(super) struct PreviewCache {
    pub(super) key: Option<(rows::RowKind, u64)>,
    pub(super) lines: Vec<Line<'static>>,
}

// ---- persisted state (selection / collapsed sections / sidebar width) ----

/// Parsed contents of `state_path()`. All three fields are optional —
/// a fresh install, a partially-written file, or an unrecognized slug
/// all degrade to `None`/empty rather than an error.
#[derive(Default, Debug, PartialEq)]
struct PersistedState {
    selected: Option<(rows::Section, String)>,
    collapsed: std::collections::HashSet<rows::Section>,
    sidebar_width: Option<u16>,
}

/// Reconstruct the `RowKind` a persisted `(section, item)` pair refers
/// to. An empty item name means "that section's header row".
fn kind_for_selection(section: rows::Section, item: &str) -> rows::RowKind {
    if item.is_empty() {
        return rows::RowKind::SectionHeader(section);
    }
    match section {
        rows::Section::Sessions => rows::RowKind::Session(item.to_string()),
        rows::Section::Agents => rows::RowKind::Agent(item.to_string()),
        rows::Section::Groups => rows::RowKind::Group(item.to_string()),
        rows::Section::Hosts => rows::RowKind::Host(item.to_string()),
    }
}

/// The inverse of `kind_for_selection` — what would we persist for this
/// row? `None` for `AgentGroupHeader`, which is never a cursor position.
fn selection_key(kind: &rows::RowKind) -> Option<(rows::Section, String)> {
    match kind {
        rows::RowKind::SectionHeader(s) => Some((*s, String::new())),
        rows::RowKind::Session(n) => Some((rows::Section::Sessions, n.clone())),
        rows::RowKind::Agent(t) => Some((rows::Section::Agents, t.clone())),
        rows::RowKind::Group(n) => Some((rows::Section::Groups, n.clone())),
        rows::RowKind::Host(n) => Some((rows::Section::Hosts, n.clone())),
        rows::RowKind::AgentGroupHeader(_) => None,
    }
}

/// Parse the `k=v` state-file format:
/// `selected=<section-slug>:<item-name-or-empty-for-header>`,
/// `collapsed=<comma-separated-slugs>`, `sidebar=<cols>`.
///
/// Tolerates the legacy pre-rework format, which was just a bare view
/// slug (`"sessions"`, `"agents"`, …) with no `=` at all: that selects
/// the matching section's header, with nothing collapsed and the
/// default width. An unrecognized legacy slug (e.g. the even-older
/// `"projects"`) yields a fully-default state.
fn parse_state(content: &str) -> PersistedState {
    if !content.contains('=') {
        let slug = content.trim();
        return match rows::Section::from_slug(slug) {
            Some(section) => PersistedState {
                selected: Some((section, String::new())),
                ..Default::default()
            },
            None => PersistedState::default(),
        };
    }
    let mut state = PersistedState::default();
    for line in content.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "selected" => {
                if let Some((slug, item)) = v.split_once(':') {
                    if let Some(section) = rows::Section::from_slug(slug) {
                        state.selected = Some((section, item.to_string()));
                    }
                }
            }
            "collapsed" => {
                state.collapsed = v.split(',').filter_map(rows::Section::from_slug).collect();
            }
            "sidebar" => {
                if let Ok(n) = v.trim().parse::<u16>() {
                    state.sidebar_width = Some(n);
                }
            }
            _ => {}
        }
    }
    state
}

/// Render the `k=v` state-file format from live values. Pure — the
/// caller decides what "selected" resolves to (`selection_key` of the
/// current row).
fn serialize_state(
    selected: Option<(rows::Section, String)>,
    collapsed: &std::collections::HashSet<rows::Section>,
    sidebar_width: u16,
) -> String {
    let selected_str = selected
        .map(|(s, item)| format!("{}:{}", s.slug(), item))
        .unwrap_or_default();
    let mut collapsed_slugs: Vec<&str> = collapsed.iter().map(|s| s.slug()).collect();
    collapsed_slugs.sort_unstable();
    format!(
        "selected={selected_str}\ncollapsed={}\nsidebar={sidebar_width}\n",
        collapsed_slugs.join(",")
    )
}

fn load_state_from(path: &Path) -> PersistedState {
    std::fs::read_to_string(path)
        .map(|s| parse_state(&s))
        .unwrap_or_default()
}

fn save_state_to(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, content);
}

fn load_state() -> PersistedState {
    load_state_from(&state_path())
}

fn save_state(app: &App) {
    let selected = app.selected_row().and_then(|r| selection_key(&r.kind));
    let content = serialize_state(selected, &app.collapsed, app.sidebar_width);
    save_state_to(&state_path(), &content);
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

/// A pane currently pinned beside tad, with everything needed to send
/// it home. All ids are tmux's stable handles (`%pane`, `@window`),
/// resolved at pin time so renames/shuffles can't misroute the return.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct PinnedPane {
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
    pub(super) data: AppData,
    /// Flat row tree — rebuilt by [`App::refresh_rows`] whenever the
    /// data, collapsed set, or filter text changes.
    pub(super) rows: Vec<rows::Row>,
    /// Index into `rows`. Always on a selectable row (see
    /// `rows::step_selectable`) except transiently during a rebuild.
    pub(super) cursor: usize,
    /// First visible row in the sidebar viewport; `render::scroll_to_cursor`
    /// keeps this in sync with `cursor` so the selection stays on screen.
    pub(super) sidebar_scroll: usize,
    pub(super) collapsed: std::collections::HashSet<rows::Section>,
    /// Sidebar width in columns; persisted, clamped to 20..=60.
    pub(super) sidebar_width: u16,
    /// Narrow-terminal overlay mode: is the sidebar currently drawn
    /// full-screen over the main pane? (No key toggles this yet — a
    /// later task wires one up; the renderer already honors it.)
    pub(super) sidebar_overlay: bool,
    /// Panes currently pinned beside tad. Treated as at-most-one until
    /// Task 8 wires up the multi-pane grid via `grid::decide_pin`.
    pub(super) pins: Vec<PinnedPane>,
    /// Bumped once per `refresh()`; animates `agents::state_dot`'s
    /// Working frames.
    pub(super) spinner_tick: u64,
    /// Agent state as of the previous refresh, keyed by target — lets
    /// a later task detect state transitions (e.g. Working → Blocked)
    /// worth notifying on.
    // TODO(herdr-cockpit): consumed by the notifications task.
    #[allow(dead_code)]
    pub(super) prev_agent_states: std::collections::HashMap<String, crate::agents::AgentState>,
    /// Cursor over `data.ui.snooze_intervals` in the snooze modal.
    pub(super) snooze_cursor: usize,
    /// New window name being typed in the rename-agent modal.
    pub(super) rename_agent_text: TextInput,
    /// `session:window.pane` of the agent being renamed (captured at
    /// modal-open time so a mid-modal refresh doesn't drift the target).
    pub(super) rename_agent_target: Option<String>,
    /// Victim of the pending confirm-kill modal (captured at arm time).
    pub(super) confirm_kill: Option<ConfirmKillTarget>,
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
        let persisted = load_state();
        let sidebar_width = persisted.sidebar_width.unwrap_or(32).clamp(20, 60);
        let collapsed = persisted.collapsed;
        let rows = rows::build_rows(&data, &collapsed, "");
        let cursor = persisted
            .selected
            .as_ref()
            .and_then(|(section, item)| rows::index_of(&rows, &kind_for_selection(*section, item)))
            .or_else(|| rows::first_item_index(&rows))
            .unwrap_or(0);
        App {
            data,
            rows,
            cursor,
            sidebar_scroll: 0,
            collapsed,
            sidebar_width,
            sidebar_overlay: false,
            pins: Vec::new(),
            spinner_tick: 0,
            prev_agent_states: std::collections::HashMap::new(),
            snooze_cursor: 0,
            rename_agent_text: TextInput::new(),
            rename_agent_target: None,
            confirm_kill: None,
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

    /// Rebuild `rows` from the current `data`/`collapsed`/`filter`,
    /// keeping the cursor on the same `RowKind` when it's still
    /// present; otherwise snap forward to the nearest selectable row.
    pub(super) fn refresh_rows(&mut self) {
        let current_kind = self.rows.get(self.cursor).map(|r| r.kind.clone());
        self.rows = rows::build_rows(&self.data, &self.collapsed, self.filter.as_str());
        self.cursor = current_kind
            .as_ref()
            .and_then(|k| rows::index_of(&self.rows, k))
            .or_else(|| {
                let clamped = self.cursor.min(self.rows.len().saturating_sub(1));
                rows::step_selectable(&self.rows, clamped, 0)
            })
            .unwrap_or(0);
    }

    pub(super) fn selected_row(&self) -> Option<&rows::Row> {
        self.rows.get(self.cursor)
    }

    pub(super) fn refresh(&mut self) {
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        self.data = AppData::load();
        self.refresh_rows();
    }

    /// Preview lines for the current selection, via the single-entry
    /// cache. Builders only run on a cache miss (selection moved or
    /// the data refreshed).
    pub(super) fn preview_lines(&mut self) -> Vec<Line<'static>> {
        let no_selection = || {
            vec![Line::from(Span::styled(
                "no selection",
                Style::default().fg(self.theme.muted),
            ))]
        };
        let Some(row) = self.selected_row() else {
            return no_selection();
        };
        let kind = row.kind.clone();
        let name = match &kind {
            rows::RowKind::Session(n)
            | rows::RowKind::Group(n)
            | rows::RowKind::Host(n)
            | rows::RowKind::Agent(n) => n.clone(),
            rows::RowKind::SectionHeader(_) | rows::RowKind::AgentGroupHeader(_) => {
                return no_selection();
            }
        };
        let key = (kind.clone(), self.refresh_generation);
        if self.preview_cache.key.as_ref() != Some(&key) {
            let lines = match &kind {
                rows::RowKind::Session(_) => {
                    preview::preview_session(&self.data, &name, &self.theme)
                }
                rows::RowKind::Group(_) => preview::preview_group(&self.data, &name, &self.theme),
                rows::RowKind::Host(_) => preview::preview_host(&self.data, &name, &self.theme),
                rows::RowKind::Agent(_) => preview::preview_agent(&self.data, &name, &self.theme),
                rows::RowKind::SectionHeader(_) | rows::RowKind::AgentGroupHeader(_) => {
                    unreachable!("handled above")
                }
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

/// Options for launching the dashboard. Today: open with a specific
/// agent row preselected (`--select-agent <target>` — useful for
/// scripts that want to jump straight to a particular agent).
#[derive(Debug, Default, Clone)]
pub struct RunOpts {
    /// If Some, the dashboard preselects the row whose agent `target`
    /// matches. Missing-target = falls back to the Agents section
    /// header so the user still lands somewhere relevant.
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

    let mut app = result?;
    // Every clean exit (q/Esc/Ctrl-C/Enter dispatch) sends every pinned
    // pane home before tad leaves the building.
    for p in app.pins.drain(..) {
        dispatch::return_pane(&p);
    }
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

/// Snapshot of everything `save_state` persists, used to detect
/// whether a keypress actually changed anything worth writing.
fn persist_snapshot(
    app: &App,
) -> (
    Option<(rows::Section, String)>,
    std::collections::HashSet<rows::Section>,
    u16,
) {
    let selected = app.selected_row().and_then(|r| selection_key(&r.kind));
    (selected, app.collapsed.clone(), app.sidebar_width)
}

fn app_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    opts: RunOpts,
) -> Result<App> {
    let mut app = App::new();
    // Honor --select-agent: try to select the matching row. If the
    // target isn't in the scan (agent vanished between the watcher
    // noticing and us launching), fall back to the Agents section
    // header so we still land somewhere relevant.
    if let Some(target) = &opts.select_agent {
        app.from_popup = true;
        app.refresh_rows();
        app.cursor = rows::index_of(&app.rows, &rows::RowKind::Agent(target.clone()))
            .or_else(|| rows::section_header_index(&app.rows, rows::Section::Agents))
            .unwrap_or(app.cursor);
    }
    let mut last_snapshot = persist_snapshot(&app);
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
        let snapshot = persist_snapshot(&app);
        if snapshot != last_snapshot {
            save_state(&app);
            last_snapshot = snapshot;
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

    pub(super) fn mk_app(data: AppData) -> App {
        let collapsed: std::collections::HashSet<rows::Section> = std::collections::HashSet::new();
        let filter = TextInput::new();
        let built_rows = rows::build_rows(&data, &collapsed, filter.as_str());
        let cursor = rows::first_item_index(&built_rows).unwrap_or(0);
        App {
            data,
            rows: built_rows,
            cursor,
            sidebar_scroll: 0,
            collapsed,
            sidebar_width: 32,
            sidebar_overlay: false,
            pins: Vec::new(),
            spinner_tick: 0,
            prev_agent_states: std::collections::HashMap::new(),
            snooze_cursor: 0,
            rename_agent_text: TextInput::new(),
            rename_agent_target: None,
            confirm_kill: None,
            flash: None,
            from_popup: false,
            filter,
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
    use super::testutil::mk_data;
    use super::*;
    use rows::{RowKind, Section};

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
        let mut app = testutil::mk_app(mk_data_with_group("panes"));
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
        let mut app = testutil::mk_app(mk_data_with_group("panes"));
        app.data.groups.push((
            "staging".to_string(),
            crate::config::Group {
                layout: "browse".to_string(),
                hosts: vec![],
            },
        ));
        app.refresh_rows();
        let first = format!("{:?}", app.preview_lines());
        app.cursor = rows::index_of(&app.rows, &RowKind::Group("staging".into())).unwrap();
        let second = format!("{:?}", app.preview_lines());
        assert_ne!(first, second);
        assert!(second.contains("staging"));
    }

    #[test]
    fn preview_lines_without_selection_says_so() {
        // Wholly empty data: the cursor sits on a SectionHeader row,
        // which isn't a "selection" for the preview pane.
        let mut app = testutil::mk_app(mk_data(vec![], vec![]));
        let lines = format!("{:?}", app.preview_lines());
        assert!(lines.contains("no selection"));
    }

    #[test]
    fn preview_cache_keys_on_row_kind_not_just_name() {
        // A session and a group sharing the same name must not collide
        // in the cache — this is exactly why the key is RowKind, not
        // a bare String.
        let mut data = mk_data(vec![testutil::mk_session("shared")], vec![]);
        data.groups = vec![(
            "shared".to_string(),
            crate::config::Group {
                layout: "panes".to_string(),
                hosts: vec![],
            },
        )];
        let mut app = testutil::mk_app(data);
        app.cursor = rows::index_of(&app.rows, &RowKind::Session("shared".into())).unwrap();
        let session_preview = format!("{:?}", app.preview_lines());
        app.cursor = rows::index_of(&app.rows, &RowKind::Group("shared".into())).unwrap();
        let group_preview = format!("{:?}", app.preview_lines());
        assert_ne!(session_preview, group_preview);
    }

    #[test]
    fn refresh_rows_keeps_cursor_on_same_row_kind_after_data_change() {
        let data = mk_data(vec![testutil::mk_session("alpha")], vec![]);
        let mut app = testutil::mk_app(data);
        app.cursor = rows::index_of(&app.rows, &RowKind::Session("alpha".into())).unwrap();
        // A new session appears earlier in the list, but "alpha" is
        // still present — the cursor must stay on it.
        app.data
            .sessions
            .insert(0, testutil::mk_session("aaa-first"));
        app.refresh_rows();
        assert_eq!(
            app.selected_row().unwrap().kind,
            RowKind::Session("alpha".into())
        );
    }

    #[test]
    fn refresh_rows_snaps_to_nearest_selectable_when_row_vanishes() {
        let data = mk_data(
            vec![testutil::mk_session("alpha"), testutil::mk_session("beta")],
            vec![],
        );
        let mut app = testutil::mk_app(data);
        app.cursor = rows::index_of(&app.rows, &RowKind::Session("beta".into())).unwrap();
        app.data.sessions.retain(|s| s.name != "beta");
        app.refresh_rows();
        // "beta" is gone; the cursor must land on some selectable row
        // rather than pointing past the end or at a header/divider.
        assert!(app.selected_row().unwrap().selectable);
        assert!(app.cursor < app.rows.len());
    }

    #[test]
    fn parse_state_round_trips_selected_collapsed_and_width() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert(Section::Hosts);
        let content = serialize_state(
            Some((Section::Agents, "work:1.0".to_string())),
            &collapsed,
            40,
        );
        let parsed = parse_state(&content);
        assert_eq!(
            parsed.selected,
            Some((Section::Agents, "work:1.0".to_string()))
        );
        assert_eq!(parsed.collapsed, collapsed);
        assert_eq!(parsed.sidebar_width, Some(40));
    }

    #[test]
    fn parse_state_handles_header_selection_with_empty_item() {
        let content = serialize_state(
            Some((Section::Sessions, String::new())),
            &Default::default(),
            32,
        );
        let parsed = parse_state(&content);
        assert_eq!(parsed.selected, Some((Section::Sessions, String::new())));
    }

    #[test]
    fn parse_state_tolerates_legacy_single_slug_file() {
        for slug in ["sessions", "groups", "hosts", "agents"] {
            let parsed = parse_state(slug);
            assert_eq!(
                parsed.selected,
                Some((Section::from_slug(slug).unwrap(), String::new())),
                "slug {slug}"
            );
            assert!(parsed.collapsed.is_empty());
            assert!(parsed.sidebar_width.is_none());
        }
    }

    #[test]
    fn parse_state_rejects_legacy_projects_slug() {
        // Pre-removal builds persisted "projects"; from_slug rejects
        // it so the caller falls back to the default (Sessions header).
        let state = parse_state("projects");
        assert!(state.selected.is_none());
    }

    #[test]
    fn load_and_save_state_round_trip_through_a_real_file() {
        let path = std::env::temp_dir().join(format!(
            "tad-dashboard-state-test-{}-{}.state",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert(Section::Groups);
        let content = serialize_state(Some((Section::Hosts, "web1".to_string())), &collapsed, 45);
        save_state_to(&path, &content);
        let loaded = load_state_from(&path);
        assert_eq!(loaded.selected, Some((Section::Hosts, "web1".to_string())));
        assert_eq!(loaded.collapsed, collapsed);
        assert_eq!(loaded.sidebar_width, Some(45));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_state_from_missing_file_is_default() {
        let path = std::env::temp_dir().join(format!(
            "tad-dashboard-state-missing-{}.state",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_state_from(&path), PersistedState::default());
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
