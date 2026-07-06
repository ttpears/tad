//! Screen-space hit-testing for the mouse. `render.rs` rebuilds a
//! `HitMap` every frame as it draws (`App::hits`); `mouse.rs` consults
//! it to turn a click/scroll's `(x, y)` into an [`Action`].
//!
//! Regions can overlap (e.g. a row's dot column sits inside that row's
//! full-width click region, which itself sits inside the sidebar's
//! whole-rect zone) — `at` always resolves to the most specific one by
//! walking registrations newest-first, so whoever registered *last*
//! wins. Callers rely on this: register broad zones before narrow
//! rows/chips, and base UI before modal overlays.

use ratatui::layout::Rect;

use super::action::Action;

// `pub(crate)` rather than `pub(super)` — same reasoning as
// `rows::Row`/`RowKind`: `App::hits`/`App::drag` are `pub(super)` from
// `dashboard.rs`, which (being a crate-root module file) makes them
// visible crate-wide, so the types those fields hold must be at least
// as visible or rustc flags a private-in-public warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hit {
    /// A concrete action to run on left-click.
    Click(Action),
    /// The draggable column between the sidebar and the preview pane.
    Divider,
    /// The sidebar's full rect — catches scroll wheel events that
    /// don't land on a specific row.
    SidebarZone,
    /// The preview pane's full rect.
    PreviewZone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragKind {
    Divider,
}

/// Accumulated (rect, hit) registrations for the frame just drawn.
/// Cleared and rebuilt every render — there is no stale state to
/// invalidate.
#[derive(Default)]
pub(crate) struct HitMap {
    regions: Vec<(Rect, Hit)>,
}

impl HitMap {
    pub(super) fn clear(&mut self) {
        self.regions.clear();
    }

    pub(super) fn register(&mut self, rect: Rect, hit: Hit) {
        self.regions.push((rect, hit));
    }

    /// The hit at `(x, y)`, if any. Last-registered match wins (modals
    /// register after the base UI, and within a widget, more specific
    /// sub-regions are registered after the region they sit inside).
    pub(super) fn at(&self, x: u16, y: u16) -> Option<&Hit> {
        self.regions
            .iter()
            .rev()
            .find(|(rect, _)| contains(rect, x, y))
            .map(|(_, hit)| hit)
    }
}

fn contains(rect: &Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn at_misses_outside_every_region() {
        let mut hits = HitMap::default();
        hits.register(rect(0, 0, 10, 10), Hit::SidebarZone);
        assert_eq!(hits.at(20, 20), None);
    }

    #[test]
    fn at_returns_the_only_match() {
        let mut hits = HitMap::default();
        hits.register(rect(0, 0, 10, 10), Hit::Click(Action::Refresh));
        assert_eq!(hits.at(5, 5), Some(&Hit::Click(Action::Refresh)));
    }

    #[test]
    fn at_prefers_the_last_registered_overlapping_region() {
        let mut hits = HitMap::default();
        hits.register(rect(0, 0, 20, 20), Hit::SidebarZone);
        hits.register(rect(5, 5, 3, 1), Hit::Click(Action::TogglePin(2)));
        // Inside the narrow, later-registered region: wins.
        assert_eq!(hits.at(6, 5), Some(&Hit::Click(Action::TogglePin(2))));
        // Inside the broad zone but outside the narrow region: falls
        // back to the earlier registration.
        assert_eq!(hits.at(1, 1), Some(&Hit::SidebarZone));
    }

    #[test]
    fn clear_empties_all_registrations() {
        let mut hits = HitMap::default();
        hits.register(rect(0, 0, 10, 10), Hit::PreviewZone);
        hits.clear();
        assert_eq!(hits.at(1, 1), None);
    }

    #[test]
    fn register_bounds_are_exclusive_on_the_far_edge() {
        let mut hits = HitMap::default();
        hits.register(rect(2, 2, 3, 3), Hit::Divider);
        assert_eq!(hits.at(4, 4), Some(&Hit::Divider));
        assert_eq!(hits.at(5, 5), None); // x=2+3=5, y=2+3=5: just past the edge
        assert_eq!(hits.at(1, 2), None); // just before the left edge
    }
}
