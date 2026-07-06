//! Unified UI actions. Every keyboard binding and every mouse click,
//! drag or scroll boils down to one of these — `execute` is the one
//! place that actually mutates `App` for it. Keeping the mutation here
//! (rather than scattered across `keys.rs`'s per-key arms and
//! `mouse.rs`'s per-event arms) means the two input paths can't drift:
//! a key and a click that mean the same thing run the exact same code.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agents;
use crate::theme;
use crate::{snooze, tmux};

use super::dispatch::{self, resolve_pane, tad_window_id, OpenTarget};
use super::format;
use super::grid;
use super::keys;
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
    /// `t` (or the footer's theme chip): opens the theme picker.
    OpenThemePicker,
    /// A click on theme-picker row `i`: moves the cursor there and
    /// live-applies that theme, same as arrowing to it with j/k.
    ThemeOption(usize),
    /// A click on snooze-picker row `i`: moves the cursor there
    /// (confirming still needs `ModalConfirm`/Enter).
    SnoozeOption(usize),
    /// A click on a modal's `↵ confirm`-style hint chip. `execute`
    /// routes this to whichever mode is currently open's own Enter
    /// handling — see `handle_modal_key`.
    ModalConfirm,
    /// A click on a modal's `Esc cancel` hint chip. Routes to whichever
    /// mode is currently open's own Esc handling.
    ModalCancel,
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
                enter_mode(app, InputMode::SnoozeSelect);
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
        Action::OpenThemePicker => open_theme_picker(app),
        Action::ThemeOption(i) => apply_theme_cursor(app, i),
        Action::SnoozeOption(i) => {
            if i < app.data.ui.snooze_intervals.len() {
                app.snooze_cursor = i;
            }
        }
        Action::ModalConfirm => handle_modal_key(app, KeyCode::Enter),
        Action::ModalCancel => handle_modal_key(app, KeyCode::Esc),
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

/// Enter `mode`, clearing any in-progress divider drag along with it.
///
/// A drag can only *start* while `input_mode` is `None` (`mouse.rs`'s
/// `hit_allowed` blocks a `Hit::Divider` down-click in any modal mode),
/// but the keys that open a modal don't otherwise touch `app.drag` —
/// without this, a drag begun just before a modal-opening key lands
/// would sit in `app.drag` and keep firing `SetSidebarWidth`/
/// `save_state` off drag/release events that land while the modal has
/// exclusive input. `mouse.rs` also re-checks `input_mode` itself
/// belt-and-suspenders, but every modal-opening path should route
/// through here rather than assigning `app.input_mode` directly.
fn enter_mode(app: &mut App, mode: InputMode) {
    app.drag = None;
    app.input_mode = mode;
}

/// `t` / the footer's theme chip: open the picker, capturing the
/// active theme (and its name, for the initial cursor position) so
/// Esc can restore it after the cursor's live-preview has moved
/// `app.theme` elsewhere.
fn open_theme_picker(app: &mut App) {
    let names = theme::builtin_names();
    app.theme_before = Some(app.theme);
    app.theme_before_name = theme::current_name();
    app.theme_cursor = app
        .theme_before_name
        .as_deref()
        .and_then(|n| names.iter().position(|x| *x == n))
        .unwrap_or(0);
    enter_mode(app, InputMode::ThemeSelect);
}

/// Move the theme-picker cursor to `i` and live-apply that theme —
/// shared by a click on picker row `i` (`Action::ThemeOption`) and
/// `keys::handle_theme_key`'s j/k/arrow handling.
pub(super) fn apply_theme_cursor(app: &mut App, i: usize) {
    let names = theme::builtin_names();
    if let Some(name) = names.get(i) {
        if let Some(t) = theme::by_name(name) {
            app.theme_cursor = i;
            app.theme = t;
        }
    }
}

/// Route a modal's `↵ confirm` / `Esc cancel` hint-chip click to
/// whichever `InputMode` is currently open's own key handler — the
/// exact same code path a real Enter/Esc keypress would take, so
/// there's no separate "confirm" logic to drift out of sync with it.
/// A no-op in `None`/`Filter`, neither of which is one of the four
/// modals (or the theme picker) this routes for.
fn handle_modal_key(app: &mut App, code: KeyCode) {
    let key = KeyEvent::new(code, KeyModifiers::NONE);
    match app.input_mode {
        InputMode::NewSession => keys::handle_new_session_key(app, key),
        InputMode::SnoozeSelect => keys::handle_snooze_key(app, key),
        InputMode::RenameAgent => keys::handle_rename_agent_key(app, key),
        InputMode::ConfirmKill => keys::handle_confirm_kill_key(app, key),
        InputMode::ThemeSelect => keys::handle_theme_key(app, key),
        InputMode::None | InputMode::Filter => {}
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
            enter_mode(app, InputMode::ConfirmKill);
        }
        Some(RowKind::Agent(target)) => {
            if let Some(agent) = app.data.agents.iter().find(|a| a.target == target) {
                app.confirm_kill = Some(ConfirmKillTarget::Agent {
                    target: target.clone(),
                    pid: agent.agent_pid,
                    window_name: agent.window_name.clone(),
                });
                enter_mode(app, InputMode::ConfirmKill);
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
            enter_mode(app, InputMode::RenameAgent);
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
            enter_mode(app, InputMode::NewSession);
        }
        _ => {
            app.new_session_name = TextInput::new();
            app.new_session_host = TextInput::new();
            app.new_session_field = NewSessionField::Name;
            enter_mode(app, InputMode::NewSession);
        }
    }
}

/// What `o` (or a click on the pin dot) does: resolve the target row
/// to stable ids, ask `grid::decide_pin` for the pure verdict, and
/// carry it out. All the actual tmux side effects live in
/// `dispatch::{pin_pane, unpin_pane}` — this function and
/// `apply_pin_decision` just wire the decision to them.
fn toggle_pin(app: &mut App, i: usize) {
    let env = grid::PinEnv {
        inside_tmux: std::env::var_os("TMUX").is_some(),
        tad_window_id: tad_window_id(),
    };
    // Resolve the target row to stable ids first; the decision
    // function never touches tmux itself.
    let kind = app.rows.get(i).map(|r| r.kind.clone());
    let row = match &kind {
        Some(RowKind::Session(name)) => resolve_pane(&tmux::exact_target(name)),
        Some(RowKind::Agent(target)) => resolve_pane(&tmux::exact_target(target)),
        _ => None,
    };
    let decision = grid::decide_pin(row.as_ref(), &app.pins, &env);
    apply_pin_decision(app, decision, kind.as_ref());
}

/// Carry out a `grid::PinAction` against `app`. Split out from
/// `toggle_pin` so the Refuse/None arms — the only ones with no tmux
/// side effect — can be exercised directly in tests without ever
/// resolving a real pane.
fn apply_pin_decision(app: &mut App, decision: grid::PinAction, kind: Option<&RowKind>) {
    match decision {
        grid::PinAction::None => {}
        grid::PinAction::Refuse(msg) => app.flash = Some(msg.to_string()),
        grid::PinAction::Unpin(idx) => {
            let p = app.pins.remove(idx);
            let remaining = app.pins.len();
            dispatch::unpin_pane(&p, remaining, &mut app.saved_border_status);
            app.refresh();
        }
        grid::PinAction::Pin(r) => {
            let label = format!("{}:{}", r.session, r.window_name);
            let title = pin_title(pin_dot(app, kind), &label);
            if dispatch::pin_pane(
                &r,
                &app.pins,
                app.sidebar_width,
                &title,
                &mut app.saved_border_status,
            ) {
                app.pins.push(PinnedPane {
                    pane_id: r.pane_id,
                    origin_window_id: r.window_id,
                    origin_session: r.session,
                    origin_window_name: r.window_name,
                    origin_window_index: r.window_index,
                    label,
                });
                app.refresh();
            } else {
                app.flash = Some("pin failed — pane vanished?".to_string());
            }
        }
    }
}

/// The dot a freshly-pinned pane's title should carry: the agent's own
/// state dot (animated the same as its sidebar row) for an Agent-origin
/// pin, else the plain filled dot Sessions rows always show.
fn pin_dot(app: &App, kind: Option<&RowKind>) -> char {
    let Some(RowKind::Agent(target)) = kind else {
        return '●';
    };
    let Some(agent) = app.data.agents.iter().find(|a| &a.target == target) else {
        return '●';
    };
    let state = agents::agent_state(
        agent,
        format::snoozed(&app.data, target),
        app.data.ui.attention_idle,
    );
    agents::state_dot(state, app.spinner_tick)
}

/// `select-pane -T` title text for a pinned pane: `<dot> <label>`.
fn pin_title(dot: char, label: &str) -> String {
    format!("{dot} {label}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::testutil::{mk_agent, mk_app, mk_data, mk_session};

    // -- apply_pin_decision: the Refuse/None arms have no tmux side
    // effect, so they're exercised directly with a hand-built
    // `grid::PinAction` rather than through `toggle_pin`'s real
    // env/tmux resolution (see `decide_pin`'s own exhaustive coverage
    // in grid.rs for the pure decision logic itself).

    #[test]
    fn apply_pin_decision_refuse_sets_flash_and_leaves_pins_untouched() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        apply_pin_decision(
            &mut app,
            grid::PinAction::Refuse("pin needs tad inside tmux"),
            None,
        );
        assert_eq!(app.flash.as_deref(), Some("pin needs tad inside tmux"));
        assert!(app.pins.is_empty());
    }

    #[test]
    fn apply_pin_decision_none_is_a_noop() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.flash = None;
        apply_pin_decision(&mut app, grid::PinAction::None, None);
        assert!(app.flash.is_none());
        assert!(app.pins.is_empty());
    }

    #[test]
    fn pin_title_formats_dot_and_label() {
        assert_eq!(pin_title('●', "work:main"), "● work:main");
    }

    #[test]
    fn pin_dot_defaults_to_filled_dot_for_non_agent_or_unknown_target() {
        let app = mk_app(mk_data(vec![], vec![]));
        assert_eq!(pin_dot(&app, None), '●');
        assert_eq!(pin_dot(&app, Some(&RowKind::Session("work".into()))), '●');
        // Agent kind but the target isn't in `data.agents` (vanished
        // between resolve and here) — still falls back cleanly.
        assert_eq!(pin_dot(&app, Some(&RowKind::Agent("gone:0.0".into()))), '●');
    }

    #[test]
    fn pin_dot_uses_the_agents_own_state_dot_when_found() {
        let agent = mk_agent("s1:0.0", "s1", 5);
        let target = agent.target.clone();
        let mut app = mk_app(mk_data(vec![], vec![agent]));
        app.spinner_tick = 0;
        let expected = {
            let agent = &app.data.agents[0];
            let state = agents::agent_state(
                agent,
                format::snoozed(&app.data, &target),
                app.data.ui.attention_idle,
            );
            agents::state_dot(state, app.spinner_tick)
        };
        assert_eq!(pin_dot(&app, Some(&RowKind::Agent(target))), expected);
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
            // pane (`row` stays `None`, and nothing was pinned to begin
            // with), so `decide_pin` can only ever answer `None` or
            // `Refuse`, never `Unpin`/`Pin`. Pins never change is the
            // environment-independent invariant.
            let pins_before = app.pins.len();
            execute(&mut app, Action::TogglePin(i));
            assert_eq!(app.pins.len(), pins_before, "kind {kind:?} changed pins");
        }
    }

    #[test]
    fn open_theme_picker_captures_the_active_theme_and_opens_the_mode() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        let before_accent = format!("{:?}", app.theme.accent);
        execute(&mut app, Action::OpenThemePicker);
        assert_eq!(app.input_mode, InputMode::ThemeSelect);
        let captured = app.theme_before.expect("theme_before must be captured");
        assert_eq!(format!("{:?}", captured.accent), before_accent);
        assert!(app.theme_cursor < theme::builtin_names().len());
    }

    #[test]
    fn opening_the_theme_picker_clears_an_in_progress_divider_drag() {
        // A divider drag started just before `t` is pressed isn't
        // cleared by the key handling itself — only `enter_mode`
        // (routed through here via `open_theme_picker`) does that, so
        // this is the representative case for the belt described in
        // `enter_mode`'s doc comment.
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.drag = Some(crate::dashboard::hit::DragKind::Divider);
        execute(&mut app, Action::OpenThemePicker);
        assert_eq!(app.input_mode, InputMode::ThemeSelect);
        assert!(app.drag.is_none());
    }

    #[test]
    fn theme_option_click_moves_cursor_and_live_applies() {
        let names = theme::builtin_names();
        let dracula_idx = names.iter().position(|n| *n == "dracula").unwrap();
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.theme = theme::by_name("tokyonight").unwrap();
        app.input_mode = InputMode::ThemeSelect;
        execute(&mut app, Action::ThemeOption(dracula_idx));
        assert_eq!(app.theme_cursor, dracula_idx);
        let dracula = theme::by_name("dracula").unwrap();
        assert_eq!(
            format!("{:?}", app.theme.accent),
            format!("{:?}", dracula.accent)
        );
    }

    #[test]
    fn snooze_option_click_moves_the_snooze_cursor_in_range() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.data.ui.snooze_intervals = vec![
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(300),
            std::time::Duration::from_secs(3600),
        ];
        execute(&mut app, Action::SnoozeOption(2));
        assert_eq!(app.snooze_cursor, 2);
        // Out-of-range index is ignored rather than panicking or
        // corrupting the cursor.
        execute(&mut app, Action::SnoozeOption(99));
        assert_eq!(app.snooze_cursor, 2);
    }

    #[test]
    fn modal_confirm_routes_to_the_current_modes_enter_handling() {
        // NewSession's Enter arm has no filesystem side effect (the
        // real tmux/session creation happens post-exit via
        // `open_after`), so it's safe to exercise the real routed
        // handler here.
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.new_session_name = TextInput::pristine("foo".to_string());
        app.input_mode = InputMode::NewSession;
        execute(&mut app, Action::ModalConfirm);
        assert!(matches!(
            &app.open_after,
            Some(OpenTarget::CreateNew { name, host }) if name == "foo" && host.is_none()
        ));
        assert!(app.should_quit);
        assert_eq!(app.input_mode, InputMode::None);
    }

    #[test]
    fn modal_cancel_routes_to_the_current_modes_esc_handling() {
        // ConfirmKill's Esc arm just clears the modal — no kill is
        // issued — so, like the NewSession case above, this exercises
        // the real routed handler with no real side effect.
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        app.input_mode = InputMode::ConfirmKill;
        app.confirm_kill = Some(ConfirmKillTarget::Session {
            name: "work".into(),
        });
        execute(&mut app, Action::ModalCancel);
        assert_eq!(app.input_mode, InputMode::None);
        assert!(app.confirm_kill.is_none());
        assert_eq!(app.data.sessions.len(), 1);
    }

    #[test]
    fn modal_cancel_on_theme_select_restores_the_pre_picker_theme() {
        // ThemeSelect's Esc arm only restores `theme_before` in memory
        // (no `save_theme_name` call), so it's safe to exercise here
        // too — unlike Confirm, which does write the user's config.
        let mut app = mk_app(mk_data(vec![], vec![]));
        let original = theme::by_name("tokyonight").unwrap();
        app.theme = original;
        app.theme_before = Some(original);
        app.input_mode = InputMode::ThemeSelect;
        app.theme = theme::by_name("dracula").unwrap();
        execute(&mut app, Action::ModalCancel);
        assert_eq!(
            format!("{:?}", app.theme.accent),
            format!("{:?}", original.accent)
        );
        assert_eq!(app.input_mode, InputMode::None);
    }
}
