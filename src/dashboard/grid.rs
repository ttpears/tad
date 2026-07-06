//! Pin-grid decision logic (scaffold — implemented in the herdr-cockpit
//! overhaul, Task 3).
//!
//! Pure decision logic for the multi-pane "pin grid" that replaces the
//! single-pane pull (see `dashboard.rs`'s `PinnedPane` / `keys.rs`'s
//! `decide_pull` for the one-pane precursor this generalizes). Nothing
//! in this module touches tmux — callers resolve panes and gather
//! environment facts, then hand them here for a pure decision.

// TODO(herdr-cockpit): consumed by Task 8 — remove this allow once wired up.
#![allow(dead_code)]

/// Max number of panes the grid holds beside tad.
pub(super) const MAX_PINS: usize = 4;

/// Where the next pinned pane joins. `pins` = pane_ids of current pins in
/// pin order; `tad_pane` = $TMUX_PANE. Returns None when full.
/// Scheme (tad pane untouched, right column fills):
///   pin 1 → JoinStep { target: tad_pane, horizontal: true,  size_pct: Some(live_pct) }
///   pin 2 → JoinStep { target: pins[0],  horizontal: false, size_pct: None }
///   pin 3 → JoinStep { target: pins[0],  horizontal: true,  size_pct: None }
///   pin 4 → JoinStep { target: pins[1],  horizontal: true,  size_pct: None }
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JoinStep {
    pub(super) target: String,
    pub(super) horizontal: bool,
    pub(super) size_pct: Option<u16>,
}

pub(super) fn join_step(pins: &[String], tad_pane: &str, live_pct: u16) -> Option<JoinStep> {
    match pins.len() {
        0 => Some(JoinStep {
            target: tad_pane.to_string(),
            horizontal: true,
            size_pct: Some(live_pct),
        }),
        1 => Some(JoinStep {
            target: pins[0].clone(),
            horizontal: false,
            size_pct: None,
        }),
        2 => Some(JoinStep {
            target: pins[0].clone(),
            horizontal: true,
            size_pct: None,
        }),
        3 => Some(JoinStep {
            target: pins[1].clone(),
            horizontal: true,
            size_pct: None,
        }),
        _ => None,
    }
}

/// Decide what `o`/click-pin does. Pure analog of the old decide_pull.
#[derive(Debug)]
pub(super) enum PinAction {
    None,
    Refuse(&'static str), // outside tmux / popup / already in tad's window / MAX_PINS reached / already pinned handled as Unpin
    Unpin(usize),         // index into pins
    Pin(super::dispatch::ResolvedPane),
}

pub(super) struct PinEnv {
    pub(super) inside_tmux: bool,
    pub(super) tad_window_id: Option<String>,
}

/// What should pinning do with the currently-selected row? Pure — all
/// tmux state arrives pre-resolved via `row` and `env`. Rules, in order:
///   1. not inside tmux → Refuse
///   2. no tad window id (popup) → Refuse
///   3. no selection → None
///   4. row already pinned → Unpin(its index)
///   5. row lives in tad's own window → Refuse
///   6. grid already full → Refuse
///   7. otherwise → Pin
pub(super) fn decide_pin(
    row: Option<&super::dispatch::ResolvedPane>,
    pins: &[super::PinnedPane],
    env: &PinEnv,
) -> PinAction {
    if !env.inside_tmux {
        return PinAction::Refuse("pin needs tad inside tmux");
    }
    let Some(tad_win) = env.tad_window_id.as_deref() else {
        return PinAction::Refuse("pin doesn't work in the popup — run tad in a regular pane");
    };
    let Some(row) = row else {
        return PinAction::None;
    };
    if let Some(idx) = pins.iter().position(|p| p.pane_id == row.pane_id) {
        return PinAction::Unpin(idx);
    }
    if row.window_id == tad_win {
        return PinAction::Refuse("that pane is already here");
    }
    if pins.len() >= MAX_PINS {
        return PinAction::Refuse("pin limit reached (4) — unpin one first");
    }
    PinAction::Pin(row.clone())
}

#[cfg(test)]
mod tests {
    use super::super::dispatch::ResolvedPane;
    use super::*;

    fn rp(pane: &str, win: &str) -> ResolvedPane {
        ResolvedPane {
            pane_id: pane.into(),
            window_id: win.into(),
            session: "origin".into(),
            window_name: "work".into(),
            window_index: "1".into(),
        }
    }

    fn pinned(pane: &str) -> super::super::PinnedPane {
        super::super::PinnedPane {
            pane_id: pane.into(),
            origin_window_id: "@9".into(),
            origin_session: "origin".into(),
            origin_window_name: "work".into(),
            origin_window_index: "1".into(),
            label: "origin:work".into(),
        }
    }

    fn env_ok() -> PinEnv {
        PinEnv {
            inside_tmux: true,
            tad_window_id: Some("@1".into()),
        }
    }

    fn pins_of(panes: &[&str]) -> Vec<String> {
        panes.iter().map(|p| p.to_string()).collect()
    }

    // -- join_step --

    #[test]
    fn join_step_first_pin_joins_tad_pane_horizontal_with_live_pct() {
        let step = join_step(&[], "%tad", 65).expect("room for pin 1");
        assert_eq!(
            step,
            JoinStep {
                target: "%tad".into(),
                horizontal: true,
                size_pct: Some(65),
            }
        );
    }

    #[test]
    fn join_step_second_pin_joins_first_pin_vertical_no_size() {
        let pins = pins_of(&["%1"]);
        let step = join_step(&pins, "%tad", 65).expect("room for pin 2");
        assert_eq!(
            step,
            JoinStep {
                target: "%1".into(),
                horizontal: false,
                size_pct: None,
            }
        );
    }

    #[test]
    fn join_step_third_pin_joins_first_pin_horizontal_no_size() {
        let pins = pins_of(&["%1", "%2"]);
        let step = join_step(&pins, "%tad", 65).expect("room for pin 3");
        assert_eq!(
            step,
            JoinStep {
                target: "%1".into(),
                horizontal: true,
                size_pct: None,
            }
        );
    }

    #[test]
    fn join_step_fourth_pin_joins_second_pin_horizontal_no_size() {
        let pins = pins_of(&["%1", "%2", "%3"]);
        let step = join_step(&pins, "%tad", 65).expect("room for pin 4");
        assert_eq!(
            step,
            JoinStep {
                target: "%2".into(),
                horizontal: true,
                size_pct: None,
            }
        );
    }

    #[test]
    fn join_step_none_when_full() {
        let pins = pins_of(&["%1", "%2", "%3", "%4"]);
        assert_eq!(join_step(&pins, "%tad", 65), None);
    }

    // -- decide_pin --

    #[test]
    fn pin_refused_outside_tmux() {
        let env = PinEnv {
            inside_tmux: false,
            tad_window_id: None,
        };
        assert!(matches!(
            decide_pin(Some(&rp("%5", "@2")), &[], &env),
            PinAction::Refuse(m) if m.contains("inside tmux")
        ));
    }

    #[test]
    fn pin_refused_in_popup() {
        let env = PinEnv {
            inside_tmux: true,
            tad_window_id: None,
        };
        assert!(matches!(
            decide_pin(Some(&rp("%5", "@2")), &[], &env),
            PinAction::Refuse(m) if m.contains("popup")
        ));
    }

    #[test]
    fn pin_noop_without_selection() {
        assert!(matches!(decide_pin(None, &[], &env_ok()), PinAction::None));
    }

    #[test]
    fn pin_unpins_when_row_already_pinned() {
        let pins = vec![pinned("%5"), pinned("%6")];
        assert!(matches!(
            decide_pin(Some(&rp("%6", "@2")), &pins, &env_ok()),
            PinAction::Unpin(1)
        ));
    }

    #[test]
    fn pin_refused_when_row_in_tads_window() {
        assert!(matches!(
            decide_pin(Some(&rp("%5", "@1")), &[], &env_ok()),
            PinAction::Refuse(m) if m.contains("already here")
        ));
    }

    #[test]
    fn pin_refused_when_grid_full() {
        let pins = vec![pinned("%1"), pinned("%2"), pinned("%3"), pinned("%4")];
        assert!(matches!(
            decide_pin(Some(&rp("%5", "@2")), &pins, &env_ok()),
            PinAction::Refuse(m) if m.contains("limit reached")
        ));
    }

    #[test]
    fn pin_pins_new_row_when_room_remains() {
        let pins = vec![pinned("%1")];
        let row = rp("%5", "@2");
        match decide_pin(Some(&row), &pins, &env_ok()) {
            PinAction::Pin(r) => assert_eq!(r, row),
            other => panic!("expected Pin, got {other:?}"),
        }
    }
}
