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
use super::{App, ConfirmKillTarget, InputMode, NewSessionField, PulledPane, TextInput, View};

pub(super) fn handle_filter_key(app: &mut App, key: KeyEvent) {
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
                let target = match app.view {
                    View::Sessions => Some(OpenTarget::AttachExisting(name)),
                    View::Groups => Some(OpenTarget::Group(name)),
                    View::Hosts => Some(OpenTarget::Host(name)),
                    View::Agents => Some(OpenTarget::JumpToPane(name)),
                };
                if let Some(t) = target {
                    app.open_after = Some(t);
                    app.should_quit = true;
                }
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
            if let (Some(target), Some(dur)) =
                (app.selected(), intervals.get(app.snooze_cursor).copied())
            {
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
            let items = app.items();
            if !items.is_empty() {
                // First non-header (Agents view has interleaved
                // session headers; other views never do).
                let first = items
                    .iter()
                    .position(|i| !super::is_agent_header(i))
                    .unwrap_or(0);
                app.list_state_mut().select(Some(first));
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            let items = app.items();
            if !items.is_empty() {
                // Last non-header (Agents view: headers always
                // precede their agents, so the last row is an agent
                // unless the session list is pathological).
                let last = items
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, i)| !super::is_agent_header(i))
                    .map(|(i, _)| i)
                    .unwrap_or(items.len() - 1);
                app.list_state_mut().select(Some(last));
            }
        }
        KeyCode::Enter => {
            if let Some(name) = app.selected() {
                let target = match app.view {
                    View::Sessions => Some(OpenTarget::AttachExisting(name)),
                    View::Groups => Some(OpenTarget::Group(name)),
                    View::Hosts => Some(OpenTarget::Host(name)),
                    View::Agents => Some(OpenTarget::JumpToPane(name)),
                };
                if let Some(t) = target {
                    app.open_after = Some(t);
                    app.should_quit = true;
                }
            }
        }
        KeyCode::Char('o') => {
            let env = PullEnv {
                inside_tmux: std::env::var_os("TMUX").is_some(),
                tad_window_id: tad_window_id(),
            };
            // Resolve the selected row to stable ids first; the decision
            // function never touches tmux itself.
            let row = match (app.view, app.selected()) {
                (View::Sessions, Some(name)) => resolve_pane(&tmux::exact_target(&name)),
                (View::Agents, Some(target)) => resolve_pane(&tmux::exact_target(&target)),
                _ => None,
            };
            match decide_pull(app.view, row.as_ref(), app.pulled_pane.as_ref(), &env) {
                PullAction::None => {}
                PullAction::Refuse(msg) => app.flash = Some(msg.to_string()),
                PullAction::ReturnCurrent => {
                    if let Some(p) = app.pulled_pane.take() {
                        return_pane(&p);
                    }
                    app.refresh();
                }
                PullAction::Pull(r) => {
                    let _ = execute_pull(app, r);
                }
                PullAction::SwapTo(r) => {
                    if let Some(p) = app.pulled_pane.take() {
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
            match (app.view, app.selected()) {
                (View::Sessions, Some(name)) => {
                    app.confirm_kill = Some(ConfirmKillTarget::Session { name });
                    app.input_mode = InputMode::ConfirmKill;
                }
                (View::Agents, Some(target)) => {
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
            // Only meaningful in the Agents view; in others it's a
            // no-op. Uppercase R so it doesn't collide with `r`
            // (manual refresh).
            if app.view == View::Agents {
                if let Some(target) = app.selected() {
                    if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                        app.rename_agent_target = Some(target.clone());
                        // Prefill with the current window name (pristine
                        // so the first keystroke replaces it cleanly,
                        // same UX as Hosts-view's `n`-prefilled SSH).
                        app.rename_agent_text = TextInput::pristine(agent.window_name.clone());
                        app.input_mode = InputMode::RenameAgent;
                    }
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
            // `n` semantics depend on which view you're in:
            //   * Hosts  → new tmux session prefilled with the host as
            //     the SSH target
            //   * others → blank new tmux session
            match (app.view, app.selected()) {
                (View::Hosts, Some(h)) => {
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

/// True when `code` confirms the pending kill. Everything else —
/// Esc, n, even a habitual second `d` — cancels. Default is No.
fn confirm_kill_accepts(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
    )
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
fn decide_pull(
    view: View,
    row: Option<&ResolvedPane>,
    pulled: Option<&PulledPane>,
    env: &PullEnv,
) -> PullAction {
    if !env.inside_tmux {
        return PullAction::Refuse("pull needs tad inside tmux");
    }
    let Some(tad_win) = env.tad_window_id.as_deref() else {
        return PullAction::Refuse("pull doesn't work in the popup — run tad in a regular pane");
    };
    // Something's already out: `o` is primarily the way home, from any
    // view. Selecting a different pullable row swaps instead.
    if let Some(p) = pulled {
        return match row {
            Some(r) if r.pane_id != p.pane_id && r.window_id != tad_win => {
                PullAction::SwapTo(r.clone())
            }
            _ => PullAction::ReturnCurrent,
        };
    }
    match (view, row) {
        (View::Sessions | View::Agents, Some(r)) => {
            if r.window_id == tad_win {
                PullAction::Refuse("that pane is already here")
            } else {
                PullAction::Pull(r.clone())
            }
        }
        _ => PullAction::None,
    }
}

/// Run the join and record the pulled state; flash on failure (the
/// pane can vanish between resolution and join).
/// Returns true if the join succeeded.
fn execute_pull(app: &mut App, r: ResolvedPane) -> bool {
    let label = format!("{}:{}", r.session, r.window_name);
    if pull_pane(&r) {
        app.pulled_pane = Some(PulledPane {
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

fn move_selection(app: &mut App, delta: i32) {
    let items = app.items();
    let len = items.len() as i32;
    if len == 0 {
        return;
    }
    let cur = app.list_state_mut().selected().unwrap_or(0) as i32;
    let mut next = (cur + delta).rem_euclid(len);

    // The Agents view interleaves session-header rows (non-selectable
    // separators) with agent rows. Skip past any header we'd otherwise
    // land on, continuing in the same direction as `delta`. Wrap once;
    // if all rows are headers (shouldn't happen — we don't emit
    // headers for sessions with no agents) we leave the cursor put.
    if app.view == View::Agents {
        let step: i32 = if delta >= 0 { 1 } else { -1 };
        let mut hops = 0;
        while super::is_agent_header(&items[next as usize]) && hops < len {
            next = (next + step).rem_euclid(len);
            hops += 1;
        }
    }
    app.list_state_mut().select(Some(next as usize));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_app, mk_data, mk_session};
    use crate::dashboard::{ConfirmKillTarget, InputMode, View};

    use super::super::dispatch::ResolvedPane;
    use crate::dashboard::PulledPane;

    fn rp(pane: &str, win: &str) -> ResolvedPane {
        ResolvedPane {
            pane_id: pane.into(),
            window_id: win.into(),
            session: "origin".into(),
            window_name: "work".into(),
            window_index: "1".into(),
        }
    }

    fn pulled(pane: &str) -> PulledPane {
        PulledPane {
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
            decide_pull(View::Sessions, Some(&rp("%5", "@2")), None, &env),
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
            decide_pull(View::Sessions, Some(&rp("%5", "@2")), None, &env),
            PullAction::Refuse(m) if m.contains("popup")
        ));
    }

    #[test]
    fn pull_noop_on_groups_and_hosts() {
        for view in [View::Groups, View::Hosts] {
            assert!(matches!(
                decide_pull(view, None, None, &env_ok()),
                PullAction::None
            ));
        }
    }

    #[test]
    fn pull_noop_without_selection() {
        assert!(matches!(
            decide_pull(View::Sessions, None, None, &env_ok()),
            PullAction::None
        ));
    }

    #[test]
    fn pull_refused_when_pane_already_in_tads_window() {
        // tad's window is @1; the row's pane lives in @1 too.
        assert!(matches!(
            decide_pull(View::Sessions, Some(&rp("%5", "@1")), None, &env_ok()),
            PullAction::Refuse(m) if m.contains("already here")
        ));
    }

    #[test]
    fn pull_plain_pull_when_nothing_out() {
        let row = rp("%5", "@2");
        match decide_pull(View::Agents, Some(&row), None, &env_ok()) {
            PullAction::Pull(r) => assert_eq!(r, row),
            other => panic!("expected Pull, got {other:?}"),
        }
    }

    #[test]
    fn pull_same_row_again_returns_it() {
        assert!(matches!(
            decide_pull(
                View::Sessions,
                Some(&rp("%5", "@2")),
                Some(&pulled("%5")),
                &env_ok()
            ),
            PullAction::ReturnCurrent
        ));
    }

    #[test]
    fn pull_different_row_swaps() {
        let row = rp("%6", "@3");
        match decide_pull(View::Agents, Some(&row), Some(&pulled("%5")), &env_ok()) {
            PullAction::SwapTo(r) => assert_eq!(r, row),
            other => panic!("expected SwapTo, got {other:?}"),
        }
    }

    #[test]
    fn pull_returns_from_any_view_when_something_is_out() {
        // `o` always offers the way home, even from Groups/Hosts.
        for view in [View::Groups, View::Hosts] {
            assert!(matches!(
                decide_pull(view, None, Some(&pulled("%5")), &env_ok()),
                PullAction::ReturnCurrent
            ));
        }
    }

    #[test]
    fn flash_clears_on_next_keypress() {
        let mut app = mk_app(View::Sessions, mk_data(vec![], vec![]));
        app.flash = Some("pull needs tad inside tmux".into());
        handle_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(app.flash.is_none());
    }

    #[test]
    fn d_on_sessions_arms_confirm_instead_of_killing() {
        let mut app = mk_app(View::Sessions, mk_data(vec![mk_session("work")], vec![]));
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
        let mut app = mk_app(View::Agents, mk_data(vec![], agents));
        // Index 0 is the session header; select the agent row.
        app.list_state_agents.select(Some(1));
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
    fn d_on_groups_and_hosts_does_nothing() {
        for view in [View::Groups, View::Hosts] {
            let mut app = mk_app(view, mk_data(vec![mk_session("work")], vec![]));
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
                View::Sessions,
                Some(&rp("%6", "@1")), // different pane, but in tad's window @1
                Some(&pulled("%5")),
                &env_ok()
            ),
            PullAction::ReturnCurrent
        ));
    }

    #[test]
    fn cancel_clears_modal_without_side_effects() {
        let mut app = mk_app(View::Sessions, mk_data(vec![mk_session("work")], vec![]));
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
}
