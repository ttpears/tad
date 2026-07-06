//! Mouse → `Action` mapping. Left-click resolves through the
//! current-frame `HitMap` (see `render.rs`, which registers it);
//! clicking the row that's already selected promotes `Select` to
//! `Activate`. The divider between sidebar and preview live-resizes
//! the sidebar while dragged and persists on release. Scrolling moves
//! the cursor over the sidebar, or scrolls the preview pane.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::action::{self, Action};
use super::hit::{DragKind, Hit};
use super::{App, InputMode};

/// What a left-click on `hit` should do, given the current cursor
/// position. Pure — no `App` access, so it's trivial to unit test.
///
/// Clicking a not-currently-selected row just selects it; clicking the
/// row that's *already* selected promotes straight to activating it
/// (mirrors double-click-to-open conventions without needing to track
/// click timing).
pub(super) fn click_action(hit: &Hit, cursor: usize) -> Option<Action> {
    match hit {
        Hit::Click(Action::Select(i)) if *i == cursor => Some(Action::Activate(*i)),
        Hit::Click(a) => Some(a.clone()),
        Hit::Modal(a) => Some(a.clone()),
        Hit::Divider | Hit::SidebarZone | Hit::PreviewZone => None,
    }
}

/// Whether `hit` is live given the current `mode`. Outside any modal
/// (`InputMode::None`) every hit is live as usual. While a modal is
/// open, only the regions *it* registered during its own render
/// (`Hit::Modal`) fire — a click that lands on a base-UI region
/// underneath the (visually opaque, `Clear`-backed) overlay is inert,
/// exactly as if the modal had eaten the whole screen.
pub(super) fn hit_allowed(hit: &Hit, mode: InputMode) -> bool {
    mode == InputMode::None || matches!(hit, Hit::Modal(_))
}

pub(super) fn handle_mouse(app: &mut App, ev: MouseEvent) {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(hit) = app.hits.at(ev.column, ev.row).cloned() else {
                return;
            };
            if !hit_allowed(&hit, app.input_mode) {
                return;
            }
            if matches!(hit, Hit::Divider) {
                app.drag = Some(DragKind::Divider);
                return;
            }
            if let Some(act) = click_action(&hit, app.cursor) {
                action::execute(app, act);
            }
        }
        // The divider drag/persist below only ever starts in
        // `InputMode::None` (a `Hit::Divider` down-click is blocked by
        // `hit_allowed` in any modal mode), so no separate mode check
        // is needed here.
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.drag == Some(DragKind::Divider) {
                action::execute(app, Action::SetSidebarWidth(ev.column));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.drag.take() == Some(DragKind::Divider) {
                // Live resize during the drag already updated
                // sidebar_width frame-by-frame; persist the final
                // value now that the gesture is done.
                super::save_state(app);
            }
        }
        MouseEventKind::ScrollUp if app.input_mode == InputMode::None => {
            match app.hits.at(ev.column, ev.row) {
                Some(Hit::SidebarZone) => action::execute(app, Action::MoveCursor(-3)),
                Some(Hit::PreviewZone) => action::execute(app, Action::ScrollPreview(-3)),
                _ => {}
            }
        }
        MouseEventKind::ScrollDown if app.input_mode == InputMode::None => {
            match app.hits.at(ev.column, ev.row) {
                Some(Hit::SidebarZone) => action::execute(app, Action::MoveCursor(3)),
                Some(Hit::PreviewZone) => action::execute(app, Action::ScrollPreview(3)),
                _ => {}
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_on_selected_row_promotes_to_activate() {
        let hit = Hit::Click(Action::Select(3));
        assert_eq!(click_action(&hit, 3), Some(Action::Activate(3)));
    }

    #[test]
    fn click_on_other_row_just_selects() {
        let hit = Hit::Click(Action::Select(3));
        assert_eq!(click_action(&hit, 0), Some(Action::Select(3)));
    }

    #[test]
    fn click_on_non_select_action_passes_through_unchanged() {
        let hit = Hit::Click(Action::TogglePin(4));
        assert_eq!(click_action(&hit, 4), Some(Action::TogglePin(4)));
    }

    #[test]
    fn click_on_zone_hits_is_none() {
        assert_eq!(click_action(&Hit::SidebarZone, 0), None);
        assert_eq!(click_action(&Hit::PreviewZone, 0), None);
        assert_eq!(click_action(&Hit::Divider, 0), None);
    }

    #[test]
    fn click_on_modal_hit_passes_the_action_through() {
        let hit = Hit::Modal(Action::ModalConfirm);
        assert_eq!(click_action(&hit, 0), Some(Action::ModalConfirm));
    }

    // -- hit_allowed: the modal hit-precedence rule itself. Outside a
    // modal (`InputMode::None`) everything is live; inside one, only
    // that modal's own `Hit::Modal` regions are.

    #[test]
    fn none_mode_allows_every_hit_kind() {
        assert!(hit_allowed(&Hit::Click(Action::Refresh), InputMode::None));
        assert!(hit_allowed(
            &Hit::Modal(Action::ModalConfirm),
            InputMode::None
        ));
        assert!(hit_allowed(&Hit::SidebarZone, InputMode::None));
        assert!(hit_allowed(&Hit::PreviewZone, InputMode::None));
        assert!(hit_allowed(&Hit::Divider, InputMode::None));
    }

    #[test]
    fn modal_mode_blocks_non_modal_hits() {
        assert!(!hit_allowed(
            &Hit::Click(Action::Refresh),
            InputMode::SnoozeSelect
        ));
        assert!(!hit_allowed(&Hit::SidebarZone, InputMode::SnoozeSelect));
        assert!(!hit_allowed(&Hit::PreviewZone, InputMode::SnoozeSelect));
        assert!(!hit_allowed(&Hit::Divider, InputMode::SnoozeSelect));
    }

    #[test]
    fn modal_mode_allows_only_that_modals_hits() {
        for mode in [
            InputMode::SnoozeSelect,
            InputMode::NewSession,
            InputMode::RenameAgent,
            InputMode::ConfirmKill,
            InputMode::ThemeSelect,
        ] {
            assert!(
                hit_allowed(&Hit::Modal(Action::ModalConfirm), mode),
                "mode {mode:?}"
            );
            assert!(
                !hit_allowed(&Hit::Click(Action::Quit), mode),
                "mode {mode:?}"
            );
        }
    }
}
