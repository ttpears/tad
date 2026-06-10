//! Post-dashboard dispatch: what to do once the user picked a row and
//! the dashboard exited. Lives separate from `keys.rs` because the
//! handlers only build [`OpenTarget`] values — the actual tmux side
//! effects (switching pane, creating a session) all
//! happen here after the alternate-screen has been torn down.

use anyhow::Result;

/// The choice the user made by pressing Enter on the dashboard. Each
/// variant maps to a single dispatch function below; the
/// `OpenTarget::Dispatch` switch lives in `run_with` (spine).
///
/// `pub(crate)` rather than `pub(super)` so it can be a field type of
/// `App::open_after` without the asymmetric-visibility warning. The
/// dispatch module is itself not exported from `dashboard`, so this
/// doesn't actually leak the type outside the dashboard subtree.
pub(crate) enum OpenTarget {
    /// Attach to an existing session by name, no prompt.
    AttachExisting(String),
    /// Create a new session, optionally running `ssh <host>` as its command.
    CreateNew {
        name: String,
        host: Option<String>,
    },
    Group(String),
    Host(String),
    /// Jump to a specific tmux pane by `session:window.pane` target.
    JumpToPane(String),
}

/// Jump to a tmux pane. When tad is invoked from inside a tmux client
/// (the common case via the popup keybind), `switch-client` flips us
/// there and the popup closes. Outside tmux, fall back to `attach -t`
/// which brings up the session containing the pane.
pub(super) fn jump_to_pane(target: &str) -> Result<i32> {
    let inside_tmux = std::env::var_os("TMUX").is_some();
    if inside_tmux {
        let status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", target])
            .status();
        if matches!(&status, Ok(s) if s.success()) {
            return Ok(0);
        }
    }
    // Outside tmux: split target on ':' to get the session name and attach.
    let session = target.split(':').next().unwrap_or(target);
    let attach = std::process::Command::new("tmux")
        .args(["attach", "-t", session])
        .status();
    match attach {
        Ok(s) if s.success() => Ok(0),
        _ => Ok(1),
    }
}

/// Kill a Claude Code agent by sending SIGINT to its `claude` PID.
/// Gentle on purpose: SIGINT lets claude flush its transcript and any
/// in-flight tool calls before exiting. The pane stays open with its
/// shell — the next dashboard refresh sees the agent's gone and drops
/// the row. Returns true iff we successfully signalled the process.
pub(super) fn kill_agent(agent_pid: u32) -> bool {
    if agent_pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(agent_pid as i32, libc::SIGINT) };
    rc == 0
}

/// Rename the tmux window containing an agent. `target` is the
/// pane target (`session:window.pane`); we strip the `.pane` suffix
/// because `rename-window` operates on windows. Returns true iff the
/// tmux command succeeded.
pub(super) fn rename_agent_window(target: &str, new_name: &str) -> bool {
    let window_target = window_target_of(target);
    let out = crate::tmux::run(["rename-window", "-t", &window_target, new_name]);
    matches!(out, Ok(o) if o.status.success())
}

/// Strip the `.pane` suffix from a `session:window.pane` target to
/// produce the `session:window` form tmux's `rename-window` /
/// `select-window` / etc. accept. Split out so the parse can be
/// unit-tested without a tmux subprocess.
fn window_target_of(target: &str) -> String {
    match target.rfind('.') {
        Some(dot) => target[..dot].to_string(),
        None => target.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_target_strips_pane_suffix() {
        // canonical form
        assert_eq!(window_target_of("my-session:3.0"), "my-session:3");
        // sessions with dots in their names — the .pane suffix is the
        // *last* dot, so we use rfind
        assert_eq!(
            window_target_of("session.with.dots:7.2"),
            "session.with.dots:7"
        );
        // no pane suffix → pass through unchanged
        assert_eq!(window_target_of("session:0"), "session:0");
    }
}
