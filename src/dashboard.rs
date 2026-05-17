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
use std::time::{Duration, Instant};

use crate::{
    config, groups,
    sessions::{self, Session},
    theme::{self, Theme},
    tmux,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Sessions,
    Groups,
    Hosts,
}

impl View {
    fn next(self) -> Self {
        match self {
            View::Sessions => View::Groups,
            View::Groups => View::Hosts,
            View::Hosts => View::Sessions,
        }
    }
    fn prev(self) -> Self {
        match self {
            View::Sessions => View::Hosts,
            View::Groups => View::Sessions,
            View::Hosts => View::Groups,
        }
    }
    fn title(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Groups => "Groups",
            View::Hosts => "Hosts",
        }
    }
    fn index(self) -> usize {
        match self {
            View::Sessions => 0,
            View::Groups => 1,
            View::Hosts => 2,
        }
    }
}

struct AppData {
    sessions: Vec<Session>,
    groups: Vec<(String, config::Group)>,
    /// host → list of groups it belongs to
    hosts: Vec<(String, Vec<String>)>,
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
        Self {
            sessions,
            groups,
            hosts,
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    None,
    Filter,
    NewSession,
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
    filter: String,
    input_mode: InputMode,
    new_session_name: String,
    new_session_host: String,
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
        App {
            view: View::Sessions,
            data,
            list_state_sessions: s,
            list_state_groups: g,
            list_state_hosts: h,
            filter: String::new(),
            input_mode: InputMode::None,
            new_session_name: String::new(),
            new_session_host: String::new(),
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
        }
    }

    fn items(&self) -> Vec<String> {
        let iter: Box<dyn Iterator<Item = String>> = match self.view {
            View::Sessions => Box::new(self.data.sessions.iter().map(|s| s.name.clone())),
            View::Groups => Box::new(self.data.groups.iter().map(|(n, _)| n.clone())),
            View::Hosts => Box::new(self.data.hosts.iter().map(|(n, _)| n.clone())),
        };
        if self.filter.is_empty() {
            iter.collect()
        } else {
            let f = self.filter.to_lowercase();
            iter.filter(|x| x.to_lowercase().contains(&f)).collect()
        }
    }

    fn selected(&self) -> Option<String> {
        let state = match self.view {
            View::Sessions => &self.list_state_sessions,
            View::Groups => &self.list_state_groups,
            View::Hosts => &self.list_state_hosts,
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

pub fn run() -> Result<i32> {
    // First-launch wizard: if no groups.yaml, offer the import wizard. The
    // wizard owns its own terminal; on return, fall through to the dashboard.
    if !crate::config::config_path().exists() {
        let _ = crate::wizard::run_first_launch();
    }
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = app_loop(&mut terminal);

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
        None => Ok(1),
    }
}

fn app_loop<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<App> {
    let mut app = App::new();
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
                    InputMode::Filter => handle_filter_key(&mut app, key.code),
                    InputMode::NewSession => handle_new_session_key(&mut app, key.code),
                    InputMode::None => handle_key(&mut app, key.code, key.modifiers),
                }
            }
        }
        if app.should_quit {
            return Ok(app);
        }
    }
}

fn handle_filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Enter | KeyCode::Esc => app.input_mode = InputMode::None,
        KeyCode::Backspace => {
            app.filter.pop();
        }
        KeyCode::Char(c) => app.filter.push(c),
        _ => {}
    }
    let len = app.items().len();
    let state = app.list_state_mut();
    match state.selected() {
        Some(i) if i >= len => state.select(if len == 0 { None } else { Some(len - 1) }),
        None if len > 0 => state.select(Some(0)),
        _ => {}
    }
}

fn handle_new_session_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.new_session_name.clear();
            app.new_session_host.clear();
            app.new_session_field = NewSessionField::Name;
        }
        KeyCode::Enter => {
            let name = app.new_session_name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let host = {
                let h = app.new_session_host.trim();
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
        }
        KeyCode::Tab | KeyCode::Down => {
            app.new_session_field = match app.new_session_field {
                NewSessionField::Name => NewSessionField::Host,
                NewSessionField::Host => NewSessionField::Name,
            };
        }
        KeyCode::BackTab | KeyCode::Up => {
            app.new_session_field = match app.new_session_field {
                NewSessionField::Name => NewSessionField::Host,
                NewSessionField::Host => NewSessionField::Name,
            };
        }
        KeyCode::Backspace => match app.new_session_field {
            NewSessionField::Name => {
                app.new_session_name.pop();
            }
            NewSessionField::Host => {
                app.new_session_host.pop();
            }
        },
        KeyCode::Char(c) => match app.new_session_field {
            NewSessionField::Name => app.new_session_name.push(c),
            NewSessionField::Host => app.new_session_host.push(c),
        },
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
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Filter;
            app.filter.clear();
            let len = app.items().len();
            if len > 0 {
                app.list_state_mut().select(Some(0));
            }
        }
        KeyCode::Char('n') => {
            // Pre-seed name + host from the highlighted item where possible:
            // - Sessions view: name = session name, host blank
            // - Groups view: name = group name, host blank
            // - Hosts view: name = short host, host = full FQDN
            let (name, host) = match (app.view, app.selected()) {
                (View::Hosts, Some(h)) => (short_name(&h), h),
                (_, Some(s)) => (short_name(&s), String::new()),
                _ => (String::new(), String::new()),
            };
            app.new_session_name = name;
            app.new_session_host = host;
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
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = [View::Sessions, View::Groups, View::Hosts]
        .iter()
        .map(|v| Line::from(v.title()))
        .collect();
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
    let field_line = |label: &str, value: &str, active: bool, placeholder: &str| -> Line<'static> {
        let label_style = if active {
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.muted)
        };
        let value_style = Style::default().fg(app.theme.fg);
        let cursor = if active {
            Span::styled("█", Style::default().fg(app.theme.accent))
        } else {
            Span::raw("")
        };
        let value_span = if value.is_empty() && !active {
            Span::styled(
                placeholder.to_string(),
                Style::default().fg(app.theme.muted),
            )
        } else {
            Span::styled(value.to_string(), value_style)
        };
        Line::from(vec![
            Span::styled(format!("  {:<6}", label), label_style),
            value_span,
            cursor,
        ])
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
            "  Tab next field   ↵ create   Esc cancel",
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
            };
            ListItem::new(line)
        })
        .collect();

    let title = if app.input_mode == InputMode::Filter || !app.filter.is_empty() {
        format!(" {} — /{} ", app.view.title(), app.filter)
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
            View::Sessions => preview_session(&name, &app.theme),
            View::Groups => preview_group(&app.data, &name, &app.theme),
            View::Hosts => preview_host(&app.data, &name, &app.theme),
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

fn preview_session(name: &str, theme: &Theme) -> Vec<Line<'static>> {
    let windows = tmux::run([
        "list-windows",
        "-t",
        name,
        "-F",
        "  #{window_index}: #{window_name}  (#{window_panes} panes)",
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
    let mut lines = vec![Line::from(vec![
        Span::styled("session: ", Style::default().fg(theme.muted)),
        Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(""));
    for l in windows.lines() {
        lines.push(Line::from(Span::styled(
            l.to_string(),
            Style::default().fg(theme.fg),
        )));
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

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let line = match app.input_mode {
        InputMode::Filter => Line::from(vec![
            Span::styled("/", Style::default().fg(theme.warning)),
            Span::styled(app.filter.clone(), Style::default().fg(theme.fg)),
            Span::styled(
                "    Esc/Enter exits filter",
                Style::default().fg(theme.muted),
            ),
        ]),
        InputMode::NewSession => Line::from(Span::styled(
            "type session name, Enter to create, Esc to cancel",
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
            spans.extend(bind("1/2/3", "jump"));
            spans.extend(bind("↵", "open"));
            spans.extend(bind("n", "new"));
            if app.view == View::Sessions {
                spans.extend(bind("d", "kill"));
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
