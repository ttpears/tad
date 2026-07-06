//! Unified UI actions. Every keyboard binding and every mouse click,
//! drag or scroll boils down to one of these — `execute` is the one
//! place that actually mutates `App` for it. Keeping the mutation here
//! (rather than scattered across `keys.rs`'s per-key arms and
//! `mouse.rs`'s per-event arms) means the two input paths can't drift:
//! a key and a click that mean the same thing run the exact same code.

use crate::{snooze, tmux};

use super::dispatch::{
    pull_pane, resolve_pane, return_pane, tad_window_id, OpenTarget, ResolvedPane,
};
use super::format;
use super::rows::{self, RowKind, Section};
use super::{App, ConfirmKillTarget, InputMode, NewSessionField, PinnedPane, TextInput};

// `pub(crate)`, not `pub(super)`: `Hit::Click` (pub(crate) — see
// hit.rs) holds an `Action`, so this must be at least as visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Move the cursor to this row index (a plain click on a
    /// not-currently-selected row).
    Select(usize),
    /// Enter / click-on-already-selected-row: open it.
    Activate(usize),
    TogglePin(usize),
    ToggleSection(Section),
    MoveCursor(i32),
    JumpSection(Section),
    Home,
    End,
    Kill,
    Rename,
    Snooze,
    ClearSnooze,
    NewSession,
    Filter,
    Refresh,
    Quit,
    /// The dashboard doesn't have a theme-picker modal yet (that's
    /// Task 9's job — see `theme::builtin_names`/`by_name`, already
    /// staged for it). The footer's `t` chip constructs this today so
    /// it's clickable; `execute` no-ops on it until the modal exists.
    OpenThemePicker,
    ToggleOverlay,
    /// Reserved: nothing constructs this yet. Mouse-wheel-over-sidebar
    /// maps directly to `MoveCursor` (see `mouse.rs`) since the
    /// sidebar has no independent scroll position — it always tracks
    /// the cursor. Kept for symmetry with `ScrollPreview` in case an
    /// independent-scroll UX is wanted later.
    #[allow(dead_code)]
    ScrollSidebar(i32),
    ScrollPreview(i32),
    SetSidebarWidth(u16),
}

/// Run `action` against `app`. This is the whole of what used to be
/// inline in `keys::handle_key`'s match arms — both `keys.rs` and
/// `mouse.rs` are thin callers of this.
pub(super) fn execute(app: &mut App, action: Action) {
    let prev_cursor = app.cursor;
    match action {
        Action::Select(i) => {
            if app.rows.get(i).map(|r| r.selectable).unwrap_or(false) {
                app.cursor = i;
            }
        }
        Action::Activate(i) => activate(app, i),
        Action::TogglePin(i) => toggle_pin(app, i),
        Action::ToggleSection(section) => {
            if !app.collapsed.remove(&section) {
                app.collapsed.insert(section);
            }
            app.refresh_rows();
        }
        Action::MoveCursor(delta) => {
            if let Some(next) = rows::step_selectable(&app.rows, app.cursor, delta) {
                app.cursor = next;
            }
        }
        Action::JumpSection(section) => {
            if let Some(i) = rows::section_header_index(&app.rows, section) {
                app.cursor = i;
            }
        }
        Action::Home => {
            if let Some(i) = rows::first_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        Action::End => {
            if let Some(i) = rows::last_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        Action::Kill => kill_selected(app),
        Action::Rename => rename_selected(app),
        Action::Snooze => {
            if matches!(app.selected_row().map(|r| &r.kind), Some(RowKind::Agent(_))) {
                app.snooze_cursor = 0;
                app.input_mode = InputMode::SnoozeSelect;
            }
        }
        Action::ClearSnooze => {
            if let Some(RowKind::Agent(target)) = app.selected_row().map(|r| r.kind.clone()) {
                let _ = snooze::clear(&target);
            }
        }
        Action::NewSession => new_session_selected(app),
        Action::Filter => {
            app.input_mode = InputMode::Filter;
            app.filter.clear();
            app.refresh_rows();
            if let Some(i) = rows::first_item_index(&app.rows) {
                app.cursor = i;
            }
        }
        Action::Refresh => app.refresh(),
        Action::Quit => app.should_quit = true,
        Action::OpenThemePicker => {
            // No-op until Task 9 adds the modal.
        }
        Action::ToggleOverlay => app.sidebar_overlay = !app.sidebar_overlay,
        Action::ScrollSidebar(delta) => {
            if let Some(next) = rows::step_selectable(&app.rows, app.cursor, delta) {
                app.cursor = next;
            }
        }
        Action::ScrollPreview(delta) => {
            let max = app.preview_lines().len() as u16;
            let new = if delta < 0 {
                app.preview_scroll
                    .saturating_sub(delta.unsigned_abs() as u16)
            } else {
                app.preview_scroll.saturating_add(delta as u16)
            };
            app.preview_scroll = new.min(max);
        }
        Action::SetSidebarWidth(w) => app.sidebar_width = w.clamp(20, 60),
    }
    if app.cursor != prev_cursor {
        app.preview_scroll = 0;
    }
}

/// What Enter (or a click on the already-selected row) does: map the
/// row's kind to a dispatch target and schedule the dashboard to exit
/// into it. No-op on rows that aren't an open-able item (section/group
/// headers).
fn activate(app: &mut App, i: usize) {
    let target = match app.rows.get(i).map(|r| r.kind.clone()) {
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

fn kill_selected(app: &mut App) {
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

fn rename_selected(app: &mut App) {
    // Rename the tmux window containing the selected agent. Only
    // meaningful on an Agent row; on others it's a no-op.
    if let Some(RowKind::Agent(target)) = app.selected_row().map(|r| r.kind.clone()) {
        if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
            app.rename_agent_target = Some(target.clone());
            // Prefill with the current window name (pristine so the
            // first keystroke replaces it cleanly, same UX as the
            // Hosts-view new-session prefill).
            app.rename_agent_text = TextInput::pristine(agent.window_name.clone());
            app.input_mode = InputMode::RenameAgent;
        }
    }
}

fn new_session_selected(app: &mut App) {
    // `n` semantics depend on the selected row:
    //   * Host   → new tmux session prefilled with the host as the
    //     SSH target
    //   * other → blank new tmux session
    match app.selected_row().map(|r| r.kind.clone()) {
        Some(RowKind::Host(h)) => {
            app.new_session_name = TextInput::pristine(format::short_name(&h));
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

fn toggle_pin(app: &mut App, i: usize) {
    let env = PullEnv {
        inside_tmux: std::env::var_os("TMUX").is_some(),
        tad_window_id: tad_window_id(),
    };
    // Resolve the target row to stable ids first; the decision
    // function never touches tmux itself.
    let kind = app.rows.get(i).map(|r| r.kind.clone());
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
                app.flash = Some("swap failed — new pane vanished, original returned".to_string());
            }
        }
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

/// What should pin/unpin do? Pure — all tmux state arrives
/// pre-resolved. `pullable` is true iff the target row is a Session or
/// Agent row (the only kinds that can be pinned).
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
    // Something's already out: this is primarily the way home, from
    // any selection. Selecting a different pullable row swaps instead.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_app, mk_data, mk_session};
    use crate::dashboard::PinnedPane;

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
        // Pinning always offers the way home, even when the current
        // selection isn't itself pullable.
        for pullable in [true, false] {
            assert!(matches!(
                decide_pull(pullable, None, Some(&pinned("%5")), &env_ok()),
                PullAction::ReturnCurrent
            ));
        }
    }

    /// Something is out and the newly-selected row's pane is (somehow)
    /// already in tad's window: can't pull it, so pin/unpin falls back
    /// to returning the current pane — safe, pinned here on purpose.
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
    fn toggle_section_updates_collapsed_and_rebuilds_rows() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        assert!(!app.collapsed.contains(&Section::Sessions));
        execute(&mut app, Action::ToggleSection(Section::Sessions));
        assert!(app.collapsed.contains(&Section::Sessions));
        // Rows were rebuilt: the session item is now hidden behind its
        // collapsed header.
        assert!(rows::index_of(&app.rows, &RowKind::Session("work".into())).is_none());
        execute(&mut app, Action::ToggleSection(Section::Sessions));
        assert!(!app.collapsed.contains(&Section::Sessions));
        assert!(rows::index_of(&app.rows, &RowKind::Session("work".into())).is_some());
    }

    #[test]
    fn move_cursor_skips_unselectable_rows() {
        let agents = vec![mk_agent("s1:0.0", "s1", 5), mk_agent("s1:1.0", "s1", 10)];
        let mut app = mk_app(mk_data(vec![], agents));
        let start = app.cursor;
        assert!(app.rows[start].selectable);
        execute(&mut app, Action::MoveCursor(1));
        assert!(app.rows[app.cursor].selectable);
        assert_ne!(
            app.rows[app.cursor].kind,
            RowKind::AgentGroupHeader("s1".into())
        );
    }

    #[test]
    fn set_sidebar_width_clamps_to_bounds() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        execute(&mut app, Action::SetSidebarWidth(5));
        assert_eq!(app.sidebar_width, 20);
        execute(&mut app, Action::SetSidebarWidth(200));
        assert_eq!(app.sidebar_width, 60);
        execute(&mut app, Action::SetSidebarWidth(40));
        assert_eq!(app.sidebar_width, 40);
    }

    #[test]
    fn activate_on_session_row_sets_open_after_and_quits() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        app.cursor = rows::index_of(&app.rows, &RowKind::Session("work".into())).unwrap();
        let cursor = app.cursor;
        execute(&mut app, Action::Activate(cursor));
        assert!(matches!(
            &app.open_after,
            Some(OpenTarget::AttachExisting(name)) if name == "work"
        ));
        assert!(app.should_quit);
    }

    #[test]
    fn toggle_pin_noops_on_group_host_and_header_rows() {
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
        for kind in [
            RowKind::Group("g1".into()),
            RowKind::Host("h1".into()),
            RowKind::SectionHeader(Section::Groups),
        ] {
            let i = rows::index_of(&app.rows, &kind).unwrap();
            // `toggle_pin` reads the *real* process environment (no
            // tmux vars in a test run), so the exact refusal reason
            // varies by sandbox — but none of these kinds resolve to a
            // pane (`row` stays `None`, nothing was pinned to begin
            // with), so `decide_pull` can only ever answer `None` or
            // `Refuse`, never `Pull`/`SwapTo`/`ReturnCurrent`. Pins
            // never change is the environment-independent invariant.
            let pins_before = app.pins.len();
            execute(&mut app, Action::TogglePin(i));
            assert_eq!(app.pins.len(), pins_before, "kind {kind:?} changed pins");
        }
    }
}
