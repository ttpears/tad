//! Keyboard event handlers, one per `InputMode`. The main `handle_key`
//! is the global (mode = None) dispatcher; the others handle their
//! respective modals' typing semantics. All five mutate `&mut App` and
//! schedule post-exit dispatch via `app.open_after`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::theme;
use crate::{snooze, tmux};

use super::action::{self, execute, Action};
use super::dispatch::{kill_agent, rename_agent_window, OpenTarget};
use super::rows::{self, RowKind, Section};
use super::{App, ConfirmKillTarget, InputMode, NewSessionField};

pub(super) fn handle_filter_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let filter_before = app.filter.as_str().to_string();
    match key.code {
        // Navigate the (filtered) rows without leaving filter mode —
        // the whole point: see a match, arrow to it, Enter, done.
        KeyCode::Down => execute(app, Action::MoveCursor(1)),
        KeyCode::Up => execute(app, Action::MoveCursor(-1)),
        KeyCode::PageDown => execute(app, Action::MoveCursor(10)),
        KeyCode::PageUp => execute(app, Action::MoveCursor(-10)),
        KeyCode::Tab => jump_next_section(app),
        KeyCode::BackTab => jump_prev_section(app),
        KeyCode::Enter => execute(app, Action::Activate(app.cursor)),
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

/// What a theme-picker keypress resolves to — pure over just the
/// cursor position and list length, so it's trivial to unit-test
/// exhaustively without ever touching `App` or the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThemePick {
    /// Move the cursor to this index (already clamped in-range).
    Move(usize),
    /// Persist the theme at the current cursor and close the picker.
    Confirm,
    /// Restore the pre-picker theme and close the picker.
    Cancel,
    /// Not a theme-picker key — no-op.
    None,
}

/// The pure decision behind `handle_theme_key`. j/k/arrows move one
/// step, clamped at the ends (no wraparound); Enter confirms; Esc
/// cancels; anything else is `None`.
pub(super) fn theme_pick_result(code: KeyCode, cursor: usize, len: usize) -> ThemePick {
    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            if cursor > 0 {
                ThemePick::Move(cursor - 1)
            } else {
                ThemePick::None
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if cursor + 1 < len {
                ThemePick::Move(cursor + 1)
            } else {
                ThemePick::None
            }
        }
        KeyCode::Enter => ThemePick::Confirm,
        KeyCode::Esc => ThemePick::Cancel,
        _ => ThemePick::None,
    }
}

/// Theme picker (`InputMode::ThemeSelect`): a thin side-effect shell
/// around `theme_pick_result` — j/k/arrows live-apply the theme under
/// the cursor (shares `action::apply_theme_cursor` with a click on the
/// same row); Enter persists it via `theme::save_theme_name` and
/// closes; Esc restores whatever was active before the picker opened.
pub(super) fn handle_theme_key(app: &mut App, key: KeyEvent) {
    let len = theme::builtin_names().len();
    match theme_pick_result(key.code, app.theme_cursor, len) {
        ThemePick::Move(i) => action::apply_theme_cursor(app, i),
        ThemePick::Confirm => {
            if let Some(name) = theme::builtin_names().get(app.theme_cursor) {
                let _ = theme::save_theme_name(name);
            }
            app.theme_before = None;
            app.theme_before_name = None;
            app.input_mode = InputMode::None;
        }
        ThemePick::Cancel => {
            if let Some(t) = app.theme_before.take() {
                app.theme = t;
            }
            app.theme_before_name = None;
            app.input_mode = InputMode::None;
        }
        ThemePick::None => {}
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
        KeyCode::Char('q') | KeyCode::Esc => execute(app, Action::Quit),
        KeyCode::Tab => jump_next_section(app),
        KeyCode::BackTab => jump_prev_section(app),
        // Digit shortcuts follow the sidebar's VISUAL section order
        // (Sessions, Agents, Groups, Hosts — see rows::Section::ALL),
        // not any historical view ordering.
        KeyCode::Char('1') => jump_to_section(app, Section::Sessions),
        KeyCode::Char('2') => jump_to_section(app, Section::Agents),
        KeyCode::Char('3') => jump_to_section(app, Section::Groups),
        KeyCode::Char('4') => jump_to_section(app, Section::Hosts),
        KeyCode::Down | KeyCode::Char('j') => execute(app, Action::MoveCursor(1)),
        KeyCode::Up | KeyCode::Char('k') => execute(app, Action::MoveCursor(-1)),
        KeyCode::PageDown | KeyCode::Char('J') => execute(app, Action::MoveCursor(10)),
        KeyCode::PageUp | KeyCode::Char('K') => execute(app, Action::MoveCursor(-10)),
        KeyCode::Home | KeyCode::Char('g') => execute(app, Action::Home),
        KeyCode::End | KeyCode::Char('G') => execute(app, Action::End),
        KeyCode::Enter => execute(app, Action::Activate(app.cursor)),
        // Toggle collapse on the section the cursor is currently in
        // (on a header row, that's the header's own section).
        KeyCode::Char(' ') => {
            let section = current_section(&app.rows, app.cursor).unwrap_or(Section::Sessions);
            execute(app, Action::ToggleSection(section));
        }
        // Only meaningful in Narrow layout (the renderer ignores it
        // otherwise), so it's harmless to bind unconditionally here.
        KeyCode::Char('`') => execute(app, Action::ToggleOverlay),
        KeyCode::Char('o') => execute(app, Action::TogglePin(app.cursor)),
        KeyCode::Char('d') => execute(app, Action::Kill),
        KeyCode::Char('R') => execute(app, Action::Rename),
        KeyCode::Char('s') => execute(app, Action::Snooze),
        KeyCode::Char('S') => execute(app, Action::ClearSnooze),
        KeyCode::Char('/') => execute(app, Action::Filter),
        KeyCode::Char('n') => execute(app, Action::NewSession),
        KeyCode::Char('r') => execute(app, Action::Refresh),
        KeyCode::Char('t') => execute(app, Action::OpenThemePicker),
        KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => execute(app, Action::Quit),
        _ => {}
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
    execute(app, Action::JumpSection(section));
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
    use crate::dashboard::{ConfirmKillTarget, InputMode};

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

    /// `1`/`2`/`3`/`4` follow the sidebar's visual order — Sessions,
    /// Agents, Groups, Hosts — not any older view ordering.
    #[test]
    fn digit_keys_jump_to_sections_in_visual_order() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        let cases = [
            ('1', Section::Sessions),
            ('2', Section::Agents),
            ('3', Section::Groups),
            ('4', Section::Hosts),
        ];
        for (digit, expect) in cases {
            handle_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            assert_eq!(
                app.selected_row().map(|r| r.kind.clone()),
                Some(RowKind::SectionHeader(expect)),
                "digit {digit}"
            );
        }
    }

    #[test]
    fn space_toggles_the_cursors_section_from_a_header_row() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        app.cursor = rows::section_header_index(&app.rows, Section::Sessions).unwrap();
        assert!(!app.collapsed.contains(&Section::Sessions));
        handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.collapsed.contains(&Section::Sessions));
        handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.collapsed.contains(&Section::Sessions));
    }

    #[test]
    fn space_toggles_the_containing_section_from_an_item_row() {
        let mut app = mk_app(mk_data(vec![mk_session("work")], vec![]));
        app.cursor = rows::index_of(&app.rows, &RowKind::Session("work".into())).unwrap();
        handle_key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.collapsed.contains(&Section::Sessions));
    }

    #[test]
    fn backtick_toggles_sidebar_overlay() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        assert!(!app.sidebar_overlay);
        handle_key(&mut app, KeyCode::Char('`'), KeyModifiers::NONE);
        assert!(app.sidebar_overlay);
        handle_key(&mut app, KeyCode::Char('`'), KeyModifiers::NONE);
        assert!(!app.sidebar_overlay);
    }

    #[test]
    fn t_opens_the_theme_picker() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        handle_key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(app.input_mode, InputMode::ThemeSelect);
        assert!(app.theme_before.is_some());
    }

    // -- theme_pick_result: exhaustive over every key the picker cares
    // about, at both ends of the cursor range so the clamping (no
    // wraparound) is covered too.

    #[test]
    fn theme_pick_result_up_at_top_is_none() {
        assert_eq!(theme_pick_result(KeyCode::Up, 0, 3), ThemePick::None);
        assert_eq!(theme_pick_result(KeyCode::Char('k'), 0, 3), ThemePick::None);
    }

    #[test]
    fn theme_pick_result_up_moves_back_one() {
        assert_eq!(theme_pick_result(KeyCode::Up, 2, 3), ThemePick::Move(1));
        assert_eq!(
            theme_pick_result(KeyCode::Char('k'), 2, 3),
            ThemePick::Move(1)
        );
    }

    #[test]
    fn theme_pick_result_down_at_bottom_is_none() {
        assert_eq!(theme_pick_result(KeyCode::Down, 2, 3), ThemePick::None);
        assert_eq!(theme_pick_result(KeyCode::Char('j'), 2, 3), ThemePick::None);
    }

    #[test]
    fn theme_pick_result_down_moves_forward_one() {
        assert_eq!(theme_pick_result(KeyCode::Down, 0, 3), ThemePick::Move(1));
        assert_eq!(
            theme_pick_result(KeyCode::Char('j'), 0, 3),
            ThemePick::Move(1)
        );
    }

    #[test]
    fn theme_pick_result_enter_confirms() {
        assert_eq!(theme_pick_result(KeyCode::Enter, 1, 3), ThemePick::Confirm);
    }

    #[test]
    fn theme_pick_result_esc_cancels() {
        assert_eq!(theme_pick_result(KeyCode::Esc, 1, 3), ThemePick::Cancel);
    }

    #[test]
    fn theme_pick_result_other_keys_are_none() {
        assert_eq!(theme_pick_result(KeyCode::Char('x'), 1, 3), ThemePick::None);
        assert_eq!(theme_pick_result(KeyCode::Tab, 1, 3), ThemePick::None);
    }

    // -- handle_theme_key: Move and Cancel only — Confirm calls the
    // real `theme::save_theme_name`, which writes the user's actual
    // config.yaml, so it's deliberately left untested here (the pure
    // `theme_pick_result::Confirm` routing above is the coverage for
    // that arm; see the task brief).

    #[test]
    fn theme_cursor_move_live_applies_the_theme() {
        // tokyonight (index 0) and dracula (index 2) have visibly
        // different accents — moving the cursor there must repaint
        // `app.theme` immediately, not just on confirm.
        let names = theme::builtin_names();
        let dracula_idx = names.iter().position(|n| *n == "dracula").unwrap();
        let mut app = mk_app(mk_data(vec![], vec![]));
        app.theme = theme::by_name("tokyonight").unwrap();
        app.theme_cursor = 0;
        app.input_mode = InputMode::ThemeSelect;
        for _ in 0..dracula_idx {
            handle_theme_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.theme_cursor, dracula_idx);
        let dracula = theme::by_name("dracula").unwrap();
        assert_eq!(
            format!("{:?}", app.theme.accent),
            format!("{:?}", dracula.accent)
        );
        // The picker stays open — only Confirm/Cancel close it.
        assert_eq!(app.input_mode, InputMode::ThemeSelect);
    }

    #[test]
    fn theme_esc_restores_the_pre_picker_theme() {
        let mut app = mk_app(mk_data(vec![], vec![]));
        let original = theme::by_name("tokyonight").unwrap();
        app.theme = original;
        app.theme_before = Some(original);
        app.theme_before_name = Some("tokyonight".to_string());
        app.theme_cursor = 0;
        app.input_mode = InputMode::ThemeSelect;
        // Simulate having arrowed onto a different theme before
        // changing our mind. tokyonight-storm (index 1) shares
        // tokyonight's accent, so go one further to dracula (index 2),
        // which is visibly different.
        handle_theme_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_theme_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_ne!(
            format!("{:?}", app.theme.accent),
            format!("{:?}", original.accent)
        );
        handle_theme_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            format!("{:?}", app.theme.accent),
            format!("{:?}", original.accent)
        );
        assert_eq!(app.input_mode, InputMode::None);
        assert!(app.theme_before.is_none());
    }
}
