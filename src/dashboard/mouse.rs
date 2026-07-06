//! Mouse → `Action` mapping. Left-click resolves through the
//! current-frame `HitMap` (see `render.rs`, which registers it);
//! clicking the row that's already selected promotes `Select` to
//! `Activate`. The divider between sidebar and preview live-resizes
//! the sidebar while dragged and persists on release. Scrolling moves
//! the cursor over the sidebar, or scrolls the preview pane.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::action::{self, Action};
use super::hit::{DragKind, Hit};
use super::App;

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
        Hit::Divider | Hit::SidebarZone | Hit::PreviewZone => None,
    }
}

pub(super) fn handle_mouse(app: &mut App, ev: MouseEvent) {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(hit) = app.hits.at(ev.column, ev.row).cloned() else {
                return;
            };
            if matches!(hit, Hit::Divider) {
                app.drag = Some(DragKind::Divider);
                return;
            }
            if let Some(act) = click_action(&hit, app.cursor) {
                action::execute(app, act);
            }
        }
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
        MouseEventKind::ScrollUp => match app.hits.at(ev.column, ev.row) {
            Some(Hit::SidebarZone) => action::execute(app, Action::MoveCursor(-3)),
            Some(Hit::PreviewZone) => action::execute(app, Action::ScrollPreview(-3)),
            _ => {}
        },
        MouseEventKind::ScrollDown => match app.hits.at(ev.column, ev.row) {
            Some(Hit::SidebarZone) => action::execute(app, Action::MoveCursor(3)),
            Some(Hit::PreviewZone) => action::execute(app, Action::ScrollPreview(3)),
            _ => {}
        },
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
}
