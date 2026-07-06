//! Keyboard event handlers, one per `InputMode`. The main `handle_key`
//! is the global (mode = None) dispatcher; the others handle their
//! respective modals' typing semantics. All five mutate `&mut App` and
//! schedule post-exit dispatch via `app.open_after`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{snooze, tmux};

use super::dispatch::{
    kill_agent, pull_pane, rename_agent_window, resolve_pane, return_pane, tad_window_id,
    OpenTarget, ResolvedPane,
};
use super::format::short_name;
use super::rows::{self, RowKind, Section};
use super::{App, ConfirmKillTarget, InputMode, NewSessionField, PinnedPane, TextInput};

pub(super) fn handle_filter_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let filter_before = app.filter.as_str().to_string();
    match key.code {
        // Navigate the (filtered) rows without leaving filter mode —
        // the whole point: see a match, arrow to it, Enter, done.
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => move_selection(app, 10),
        KeyCode::PageUp => move_selection(app, -10),
        KeyCode::Tab => jump_next_section(app),
        KeyCode::BackTab => jump_prev_section(app),
        KeyCode::Enter => activate_selected(app),
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
    let filter_changed = app.filter.as_str() != filter_before;
    if filter_changed {
        app.refresh_rows();
        // Typing narrows the rows — snap to the top match so Enter
        // just works.
        if let Some(first) = rows::first_item_index(&app.rows) {
            app.cursor = first;
        }
    } else if app.cursor >= app.rows.len() {
        app.cursor = app.rows.len().saturating_sub(1);
    }
}

pub(super) fn handle_rename_agent_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::None;
            app.rename_agent_text.clear();
            app.rename_agent_target = None;
        }
        KeyCode::Enter => {
            let new_name = app.rename_agent_text.as_str().trim().to_string();
            let target = app.rename_agent_target.take();
            app.rename_agent_text.clear();
            app.input_mode = InputMode::None;
            if let Some(t) = target {
                if !new_name.is_empty() {
                    let _ = rename_agent_window(&t, &new_name);
                    app.refresh();
                }
            }
        }
        KeyCode::Backspace => app.rename_agent_text.backspace(),
        KeyCode::Delete => app.rename_agent_text.delete(),
        KeyCode::Left => app.rename_agent_text.left(),
        KeyCode::Right => app.rename_agent_text.right(),
        KeyCode::Home => app.rename_agent_text.home(),
        KeyCode::End => app.rename_agent_text.end(),
        KeyCode::Char('u') if ctrl => app.rename_agent_text.clear(),
        KeyCode::Char('a') if ctrl => app.rename_agent_text.home(),
        KeyCode::Char('e') if ctrl => app.rename_agent_text.end(),
        KeyCode::Char(c) if !ctrl => app.rename_agent_text.insert(c),
        _ => {}
    }
}

pub(super) fn handle_snooze_key(app: &mut App, key: KeyEvent) {
    // Snooze intervals come from the per-refresh `data.ui` cache
    // (loaded once when AppData was built); no file read per keypress.
    let intervals = app.data.ui.snooze_intervals.clone();
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
            let target = match app.selected_row().map(|r| r.kind.clone()) {
                Some(RowKind::Agent(t)) => Some(t),
                _ => None,
            };
            if let (Some(target), Some(dur)) = (target, intervals.get(app.snooze_cursor).copied()) {
                let _ = snooze::snooze(&target, dur);
                app.input_mode = InputMode::None;
                // If we got here from --select-agent, close the
                // dashboard so the caller's "look at this one agent"
                // flow returns to wherever it came from. Otherwise
                // just dismiss the modal and stay.
                if app.from_popup {
                    app.should_quit = true;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn handle_new_session_key(app: &mut App, key: KeyEvent) {
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

pub(super) fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.flash = None;
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab => jump_next_section(app),
        KeyCode::BackTab => jump_prev_section(app),
        KeyCode::Char('1') => jump_to_section(app, Section::Sessions),
        KeyCode::Char('2') => jump_to_section(app, Section::Groups),
        KeyCode::Char('3') => jump_to_section(app, Section::Hosts),
        KeyCode::Char('4') => jump_to_section(app, Section::Agents),
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::PageDown | KeyCode::Char('J') => move_selection(app, 10),
        KeyCode::PageUp | KeyCode::Char('K') => move_selection(app, -10),
        KeyCode::Home | KeyCode::Char('g') => {
            if let Some(i) = rows::first_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            if let Some(i) = rows::last_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        KeyCode::Enter => activate_selected(app),
        KeyCode::Char('o') => {
            let env = PullEnv {
                inside_tmux: std::env::var_os("TMUX").is_some(),
                tad_window_id: tad_window_id(),
            };
            // Resolve the selected row to stable ids first; the decision
            // function never touches tmux itself.
            let kind = app.selected_row().map(|r| r.kind.clone());
            let pullable = matches!(kind, Some(RowKind::Session(_)) | Some(RowKind::Agent(_)));
            let row = match &kind {
                Some(RowKind::Session(name)) => resolve_pane(&tmux::exact_target(name)),
                Some(RowKind::Agent(target)) => resolve_pane(&tmux::exact_target(target)),
                _ => None,
            };
            let pinned = app.pins.first().cloned();
            match decide_pull(pullable, row.as_ref(), pinned.as_ref(), &env) {
                PullAction::None => {}
                PullAction::Refuse(msg) => app.flash = Some(msg.to_string()),
                PullAction::ReturnCurrent => {
                    if let Some(p) = app.pins.pop() {
                        return_pane(&p);
                    }
                    app.refresh();
                }
                PullAction::Pull(r) => {
                    let _ = execute_pull(app, r);
                }
                PullAction::SwapTo(r) => {
                    if let Some(p) = app.pins.pop() {
                        return_pane(&p);
                    }
                    if !execute_pull(app, r) {
                        app.flash =
                            Some("swap failed — new pane vanished, original returned".to_string());
                    }
                }
            }
        }
        KeyCode::Char('d') => {
            // Arm the confirm-kill modal — nothing dies until the user
            // confirms with y/Enter. The victim is captured here so a
            // background refresh can't change what gets killed.
            //   * Sessions → tmux kill-session (heavy: drops every pane
            //     in the session)
            //   * Agents   → SIGINT to the agent's claude PID (gentle:
            //     claude flushes its transcript, pane stays open with
            //     its shell so you can verify what happened)
            match app.selected_row().map(|r| r.kind.clone()) {
                Some(RowKind::Session(name)) => {
                    app.confirm_kill = Some(ConfirmKillTarget::Session { name });
                    app.input_mode = InputMode::ConfirmKill;
                }
                Some(RowKind::Agent(target)) => {
                    if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                        app.confirm_kill = Some(ConfirmKillTarget::Agent {
                            target: target.clone(),
                            pid: agent.agent_pid,
                            window_name: agent.window_name.clone(),
                        });
                        app.input_mode = InputMode::ConfirmKill;
                    }
                }
                _ => {}
            }
        }
        KeyCode::Char('R') => {
            // Rename the tmux window containing the selected agent.
            // Only meaningful on an Agent row; on others it's a no-op.
            // Uppercase R so it doesn't collide with `r` (manual refresh).
            if let Some(RowKind::Agent(target)) = app.selected_row().map(|r| r.kind.clone()) {
                if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                    app.rename_agent_target = Some(target.clone());
                    // Prefill with the current window name (pristine
                    // so the first keystroke replaces it cleanly,
                    // same UX as the Hosts `n`-prefilled SSH).
                    app.rename_agent_text = TextInput::pristine(agent.window_name.clone());
                    app.input_mode = InputMode::RenameAgent;
                }
            }
        }
        KeyCode::Char('s') => {
            // Snooze the selected agent. Only meaningful on an Agent
            // row — on others it's a no-op.
            if matches!(app.selected_row().map(|r| &r.kind), Some(RowKind::Agent(_))) {
                app.snooze_cursor = 0;
                app.input_mode = InputMode::SnoozeSelect;
            }
        }
        KeyCode::Char('S') => {
            // Clear an active snooze on the selected agent. Useful if you
            // snoozed the wrong row or changed your mind.
            if let Some(RowKind::Agent(target)) = app.selected_row().map(|r| r.kind.clone()) {
                let _ = snooze::clear(&target);
            }
        }
        KeyCode::Char('/') => {
            app.input_mode = InputMode::Filter;
            app.filter.clear();
            app.refresh_rows();
            if let Some(i) = rows::first_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        KeyCode::Char('n') => {
            // `n` semantics depend on the selected row:
            //   * Host   → new tmux session prefilled with the host as
            //     the SSH target
            //   * other → blank new tmux session
            match app.selected_row().map(|r| r.kind.clone()) {
                Some(RowKind::Host(h)) => {
                    app.new_session_name = TextInput::pristine(short_name(&h));
                    app.new_session_host = TextInput::pristine(h);
                    app.new_session_field = NewSessionField::Name;
                    app.input_mode = InputMode::NewSession;
                }
                _ => {
                    app.new_session_name = TextInput::new();
                    app.new_session_host = TextInput::new();
                    app.new_session_field = NewSessionField::Name;
                    app.input_mode = InputMode::NewSession;
                }
            }
        }
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        _ => {}
    }
}

/// What Enter does: map the selected row's kind to a dispatch target
/// and schedule the dashboard to exit into it. No-op on rows that
/// aren't an open-able item (section/group headers).
fn activate_selected(app: &mut App) {
    let target = match app.selected_row().map(|r| r.kind.clone()) {
        Some(RowKind::Session(name)) => Some(OpenTarget::AttachExisting(name)),
        Some(RowKind::Group(name)) => Some(OpenTarget::Group(name)),
        Some(RowKind::Host(name)) => Some(OpenTarget::Host(name)),
        Some(RowKind::Agent(target)) => Some(OpenTarget::JumpToPane(target)),
        _ => None,
    };
    if let Some(t) = target {
        app.open_after = Some(t);
        app.should_quit = true;
    }
}

/// The section the cursor currently sits within — walks backward to
/// the nearest `SectionHeader` row at or before `cursor`.
fn current_section(rows: &[rows::Row], cursor: usize) -> Option<Section> {
    let clamped = cursor.min(rows.len().saturating_sub(1));
    rows[..=clamped].iter().rev().find_map(|r| match r.kind {
        RowKind::SectionHeader(s) => Some(s),
        _ => None,
    })
}

fn jump_to_section(app: &mut App, section: Section) {
    if let Some(i) = rows::section_header_index(&app.rows, section) {
        app.cursor = i;
    }
}

fn jump_next_section(app: &mut App) {
    let cur = current_section(&app.rows, app.cursor).unwrap_or(Section::Sessions);
    let idx = Section::ALL.iter().position(|s| *s == cur).unwrap_or(0);
    let next = Section::ALL[(idx + 1) % Section::ALL.len()];
    jump_to_section(app, next);
}

fn jump_prev_section(app: &mut App) {
    let cur = current_section(&app.rows, app.cursor).unwrap_or(Section::Sessions);
    let n = Section::ALL.len();
    let idx = Section::ALL.iter().position(|s| *s == cur).unwrap_or(0);
    let prev = Section::ALL[(idx + n - 1) % n];
    jump_to_section(app, prev);
}

fn move_selection(app: &mut App, delta: i32) {
    if let Some(next) = rows::step_selectable(&app.rows, app.cursor, delta) {
        app.cursor = next;
    }
}

/// Everything `decide_pull` needs about the environment, gathered by
/// the caller so the decision stays pure.
pub(super) struct PullEnv {
    pub(super) inside_tmux: bool,
    /// None = not in a regular pane (popup, or resolution failed).
    pub(super) tad_window_id: Option<String>,
}

#[derive(Debug)]
pub(super) enum PullAction {
    None,
    Refuse(&'static str),
    ReturnCurrent,
    Pull(ResolvedPane),
    SwapTo(ResolvedPane),
}

/// What should `o` do? Pure — all tmux state arrives pre-resolved.
/// `pullable` is true iff the currently-selected row is a Session or
/// Agent row (the only kinds `o` can pin).
fn decide_pull(
    pullable: bool,
    row: Option<&ResolvedPane>,
    pinned: Option<&PinnedPane>,
    env: &PullEnv,
) -> PullAction {
    if !env.inside_tmux {
        return PullAction::Refuse("pull needs tad inside tmux");
    }
    let Some(tad_win) = env.tad_window_id.as_deref() else {
        return PullAction::Refuse("pull doesn't work in the popup — run tad in a regular pane");
    };
    // Something's already out: `o` is primarily the way home, from any
    // selection. Selecting a different pullable row swaps instead.
    if let Some(p) = pinned {
        return match row {
            Some(r) if r.pane_id != p.pane_id && r.window_id != tad_win => {
                PullAction::SwapTo(r.clone())
            }
            _ => PullAction::ReturnCurrent,
        };
    }
    match (pullable, row) {
        (true, Some(r)) => {
            if r.window_id == tad_win {
                PullAction::Refuse("that pane is already here")
            } else {
                PullAction::Pull(r.clone())
            }
        }
        _ => PullAction::None,
    }
}

/// Run the join and record the pinned state; flash on failure (the
/// pane can vanish between resolution and join).
/// Returns true if the join succeeded.
fn execute_pull(app: &mut App, r: ResolvedPane) -> bool {
    let label = format!("{}:{}", r.session, r.window_name);
    if pull_pane(&r) {
        app.pins.clear();
        app.pins.push(PinnedPane {
            pane_id: r.pane_id,
            origin_window_id: r.window_id,
            origin_session: r.session,
            origin_window_name: r.window_name,
            origin_window_index: r.window_index,
            label,
        });
        app.refresh();
        true
    } else {
        app.flash = Some("pull failed — pane vanished?".to_string());
        false
    }
}

pub(super) fn handle_confirm_kill_key(app: &mut App, key: KeyEvent) {
    let target = app.confirm_kill.take();
    app.input_mode = InputMode::None;
    if !confirm_kill_accepts(key.code) {
        return;
    }
    // The victim may have died on its own while the modal was open
    // (session killed elsewhere, agent exited). Both kill paths are
    // benign on a stale target: tmux errors are ignored and the
    // SIGINT result is discarded, so confirming just refreshes.
    match target {
        Some(ConfirmKillTarget::Session { name }) => {
            tmux::kill_session(&name);
            app.refresh();
        }
        Some(ConfirmKillTarget::Agent { pid, .. }) => {
            let _ = kill_agent(pid);
            app.refresh();
        }
        None => {}
    }
}

/// True when `code` confirms the pending kill. Everything else —
/// Esc, n, even a habitual second `d` — cancels. Default is No.
fn confirm_kill_accepts(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_app, mk_data, mk_session};
    use crate::dashboard::{ConfirmKillTarget, InputMode, PinnedPane};

    fn rp(pane: &str, win: &str) -> ResolvedPane {
        ResolvedPane {
            pane_id: pane.into(),
            window_id: win.into(),
            session: "origin".into(),
            window_name: "work".into(),
            window_index: "1".into(),
        }
    }

    fn pinned(pane: &str) -> PinnedPane {
        PinnedPane {
            pane_id: pane.into(),
            origin_window_id: "@9".into(),
            origin_session: "origin".into(),
            origin_window_name: "work".into(),
            origin_window_index: "1".into(),
            label: "origin:work".into(),
        }
    }

    fn env_ok() -> PullEnv {
        PullEnv {
            inside_tmux: true,
            tad_window_id: Some("@1".into()),
        }
    }

    #[test]
    fn pull_refused_outside_tmux() {
        let env = PullEnv {
            inside_tmux: false,
            tad_window_id: None,
        };
        assert!(matches!(
            decide_pull(true, Some(&rp("%5", "@2")), None, &env),
            PullAction::Refuse(m) if m.contains("inside tmux")
        ));
    }

    #[test]
    fn pull_refused_in_popup() {
        let env = PullEnv {
            inside_tmux: true,
            tad_window_id: None,
        };
        assert!(matches!(
            decide_pull(true, Some(&rp("%5", "@2")), None, &env),
            PullAction::Refuse(m) if m.contains("popup")
        ));
    }

    #[test]
    fn pull_noop_on_non_pullable_rows_even_with_resolved_pane() {
        // Group/Host rows never resolve to a pane in practice, but
        // decide_pull must still no-op if it somehow received one.
        assert!(matches!(
            decide_pull(false, Some(&rp("%5", "@2")), None, &env_ok()),
            PullAction::None
        ));
    }

    #[test]
    fn pull_noop_without_selection() {
        assert!(matches!(
            decide_pull(true, None, None, &env_ok()),
            PullAction::None
        ));
    }

    #[test]
    fn pull_refused_when_pane_already_in_tads_window() {
        // tad's window is @1; the row's pane lives in @1 too.
        assert!(matches!(
            decide_pull(true, Some(&rp("%5", "@1")), None, &env_ok()),
            PullAction::Refuse(m) if m.contains("already here")
        ));
    }

    #[test]
    fn pull_plain_pull_when_nothing_out() {
        let row = rp("%5", "@2");
        match decide_pull(true, Some(&row), None, &env_ok()) {
            PullAction::Pull(r) => assert_eq!(r, row),
            other => panic!("expected Pull, got {other:?}"),
        }
    }

    #[test]
    fn pull_same_row_again_returns_it() {
        assert!(matches!(
            decide_pull(true, Some(&rp("%5", "@2")), Some(&pinned("%5")), &env_ok()),
            PullAction::ReturnCurrent
        ));
    }

    #[test]
    fn pull_different_row_swaps() {
        let row = rp("%6", "@3");
        match decide_pull(true, Some(&row), Some(&pinned("%5")), &env_ok()) {
            PullAction::SwapTo(r) => assert_eq!(r, row),
            other => panic!("expected SwapTo, got {other:?}"),
        }
    }

    #[test]
    fn pull_returns_current_regardless_of_pullable_when_something_is_out() {
        // `o` always offers the way home, even when the current
        // selection isn't itself pullable.
        for pullable in [true, false] {
            assert!(matches!(
                decide_pull(pullable, None, Some(&pinned("%5")), &env_ok()),
                PullAction::ReturnCurrent
            ));
        }
    }

    #[test]
    fn flash_clears_on_next_keypress() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.flash = Some("pull needs tad inside tmux".into());
        handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(app.flash.is_none());
    }

    #[test]
    fn d_on_sessions_arms_confirm_instead_of_killing() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        handle_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.input_mode, InputMode::ConfirmKill);
        assert_eq!(
            app.confirm_kill,
            Some(ConfirmKillTarget::Session {
                name: "work".into()
            })
        );
    }

    #[test]
    fn d_on_agents_arms_confirm_with_captured_pid() {
        // pid 0 so even a buggy confirm path could never signal anything.
        let mut a = mk_agent("work:1.0", "work", 0);
        a.agent_pid = 0;
        let agents = vec![a];
        let mut app = mk_app(mk_data(vec![], agents));
        // The only selectable item is the agent row itself (the
        // session group header isn't selectable), so mk_app's default
        // cursor already lands there.
        assert_eq!(
            app.selected_row().map(|r| r.kind.clone()),
            Some(RowKind::Agent("work:1.0".into()))
        );
        handle_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.input_mode, InputMode::ConfirmKill);
        assert_eq!(
            app.confirm_kill,
            Some(ConfirmKillTarget::Agent {
                target: "work:1.0".into(),
                pid: 0,
                window_name: "w".into(),
            })
        );
    }

    #[test]
    fn d_on_group_and_host_rows_does_nothing() {
        let mut data = mk_data(vec![], vec![]);
        data.groups = vec![(
            "g1".to_string(),
            crate::config::Group {
                layout: "panes".to_string(),
                hosts: vec![],
            },
        )];
        data.hosts = vec![crate::dashboard::HostRow {
            name: "h1".to_string(),
            groups: vec![],
            source: String::new(),
        }];
        let mut app = mk_app(data);
        for kind in [RowKind::Group("g1".into()), RowKind::Host("h1".into())] {
            app.cursor = rows::index_of(&app.rows, &kind).unwrap();
            handle_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
            assert_eq!(app.input_mode, InputMode::None);
            assert!(app.confirm_kill.is_none());
        }
    }

    #[test]
    fn only_y_and_enter_confirm() {
        assert!(confirm_kill_accepts(KeyCode::Char('y')));
        assert!(confirm_kill_accepts(KeyCode::Char('Y')));
        assert!(confirm_kill_accepts(KeyCode::Enter));
        // Everything else cancels — including d itself (a habitual
        // double-tap must not kill) and random keys.
        assert!(!confirm_kill_accepts(KeyCode::Esc));
        assert!(!confirm_kill_accepts(KeyCode::Char('n')));
        assert!(!confirm_kill_accepts(KeyCode::Char('d')));
        assert!(!confirm_kill_accepts(KeyCode::Char(' ')));
        assert!(!confirm_kill_accepts(KeyCode::Down));
    }

    /// Something is out and the newly-selected row's pane is (somehow)
    /// already in tad's window: can't pull it, so `o` falls back to
    /// returning the current pane — safe, pinned here on purpose.
    #[test]
    fn pull_with_target_in_tads_window_returns_current() {
        assert!(matches!(
            decide_pull(
                true,
                Some(&rp("%6", "@1")), // different pane, but in tad's window @1
                Some(&pinned("%5")),
                &env_ok()
            ),
            PullAction::ReturnCurrent
        ));
    }

    #[test]
    fn cancel_clears_modal_without_side_effects() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        app.input_mode = InputMode::ConfirmKill;
        app.confirm_kill = Some(ConfirmKillTarget::Session {
            name: "work".into(),
        });
        handle_confirm_kill_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input_mode, InputMode::None);
        assert!(app.confirm_kill.is_none());
        // The session list is untouched — cancel returned before any
        // kill or refresh.
        assert_eq!(app.data.sessions.len(), 1);
    }

    #[test]
    fn jump_to_section_moves_cursor_to_that_headers_row() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        jump_to_section(&mut app, Section::Hosts);
        assert_eq!(
            app.selected_row().map(|r| r.kind.clone()),
            Some(RowKind::SectionHeader(Section::Hosts))
        );
    }

    #[test]
    fn jump_next_section_cycles_forward_and_wraps() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        // Starts on Sessions' header (only section headers exist here).
        jump_next_section(&mut app);
        assert_eq!(
            current_section(&app.rows, app.cursor),
            Some(Section::Agents)
        );
        jump_next_section(&mut app);
        assert_eq!(
            current_section(&app.rows, app.cursor),
            Some(Section::Groups)
        );
        jump_next_section(&mut app);
        assert_eq!(current_section(&app.rows, app.cursor), Some(Section::Hosts));
        jump_next_section(&mut app);
        assert_eq!(
            current_section(&app.rows, app.cursor),
            Some(Section::Sessions)
        );
    }
}
