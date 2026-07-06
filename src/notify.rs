//! Passive attention signal substrate. The watcher writes a per-window
//! tmux user-variable, `@tad-attn`, that downstream renderers (the
//! `tad install` window-status block, custom user formats, external
//! scripts) read via `tmux show-option -wv`. This replaces the
//! pre-v0.11 `tmux display-popup` approach — the watcher no longer
//! interrupts.
//!
//! Two operations live here:
//!  - `set_attn(target, on)` — toggle the marker on the agent's
//!    *window* (pane target's window scope), so a single `@tad-attn`
//!    decision covers any pane in that window.
//!  - `is_visited(target)` — is the user actually looking at that pane
//!    right now? "Looking at" means active pane AND the containing
//!    session has at least one attached client. A pane that is the
//!    active pane in a detached session is not visited.
//!
//! A third, unrelated operation also lives here: `send_blocked`, an
//! actual OS desktop notification (via the freedesktop `notify-send`
//! CLI — no new crate dependency, same "shell out and ignore failure"
//! idiom as `TmuxNotifier::set_attn`) fired by the dashboard when an
//! agent transitions into `Blocked`. It shares this module because
//! it's the same category of thing — a best-effort "look at this"
//! signal — even though the transport differs.

use std::process::Command;

/// Behavior surface for the watcher's effect on the world. Real impl
/// shells out to tmux; the test impl in `crate::watch::tests` records
/// every call so the state machine can be exercised without a tmux
/// server.
pub(crate) trait AttentionNotifier {
    /// Mark the *window* containing `target` (`session:window.pane`) as
    /// needing attention (`on=true`) or not (`on=false`). Unsetting goes
    /// through `set-option -wu` so the variable returns to truly-unset,
    /// not "set to empty" — `#{?@tad-attn,…,}` distinguishes the two.
    fn set_attn(&mut self, target: &str, on: bool);

    /// True iff the user is currently looking at `target`: it's the
    /// active pane in its session AND the session has an attached
    /// client. Errors / no-client → false (don't claim a visit on
    /// failure; the watcher's sticky-clear semantics make this
    /// conservative direction correct).
    fn is_visited(&mut self, target: &str) -> bool;
}

/// Production notifier. Each call shells out to one `tmux` invocation;
/// `tad watch`'s default 5s poll makes that cost trivial.
pub(crate) struct TmuxNotifier;

impl AttentionNotifier for TmuxNotifier {
    fn set_attn(&mut self, target: &str, on: bool) {
        let Some(window_target) = window_scope(target) else {
            return;
        };
        let mut cmd = Command::new("tmux");
        if on {
            cmd.args(["set-option", "-w", "-t", &window_target, "@tad-attn", "1"]);
        } else {
            cmd.args(["set-option", "-wu", "-t", &window_target, "@tad-attn"]);
        }
        let _ = cmd.status();
    }

    fn is_visited(&mut self, target: &str) -> bool {
        let out = Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                target,
                "-F",
                "#{pane_active}|#{session_attached}",
            ])
            .output();
        let out = match out {
            Ok(o) if o.status.success() => o,
            _ => return false,
        };
        let s = String::from_utf8_lossy(&out.stdout);
        let line = s.trim();
        let mut parts = line.split('|');
        let pane_active = parts.next().unwrap_or("0");
        // session_attached is the count of clients; "0" if detached.
        let session_attached = parts.next().unwrap_or("0");
        pane_active == "1" && session_attached != "0"
    }
}

/// Fire a desktop notification announcing that `window_name` is
/// blocked, waiting on the user. Best-effort, matching the
/// `TmuxNotifier::set_attn` idiom: one shell-out, failures (missing
/// `notify-send`, no notification daemon, a headless/CI environment)
/// are swallowed rather than surfaced — a broken notification path
/// must never interrupt the dashboard.
pub(crate) fn send_blocked(window_name: &str) {
    let body = format!("{window_name} is blocked, waiting for input");
    let _ = Command::new("notify-send").args(["tad", &body]).status();
}

/// Strip the `.pane` suffix off a `session:window.pane` target so the
/// result targets the *window*. Returns None for malformed inputs —
/// callers treat that as "nothing to mark" rather than a crash, since
/// the watcher will retry on the next tick.
fn window_scope(target: &str) -> Option<String> {
    // Split off the pane part; tmux accepts `session:window` as a
    // valid window target.
    target
        .rsplit_once('.')
        .map(|(window, _pane)| window.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_scope_strips_pane_suffix() {
        assert_eq!(window_scope("salt:1.0").as_deref(), Some("salt:1"));
        assert_eq!(window_scope("my work:2.3").as_deref(), Some("my work:2"));
    }

    #[test]
    fn window_scope_rejects_targets_without_pane() {
        // `session:window` without a pane suffix isn't a pane target
        // and shouldn't be silently treated as one.
        assert!(window_scope("salt:1").is_none());
        assert!(window_scope("salt").is_none());
    }
}
