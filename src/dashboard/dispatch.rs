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

/// A pane fully resolved to tmux's stable ids, ready to pull.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct ResolvedPane {
    pub(super) pane_id: String,
    pub(super) window_id: String,
    pub(super) session: String,
    pub(super) window_name: String,
    pub(super) window_index: String,
}

/// Resolve a tmux target (session name or `session:win.pane`) to stable
/// ids. None when the target no longer exists. A bare session target
/// resolves to that session's active pane — exactly what the Sessions
/// view should pull.
pub(super) fn resolve_pane(target: &str) -> Option<ResolvedPane> {
    let out = crate::tmux::run([
        "display-message",
        "-p",
        "-t",
        target,
        "#{pane_id}\t#{window_id}\t#{session_name}\t#{window_name}\t#{window_index}",
    ])
    .ok()
    .filter(|o| o.status.success())?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 5 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }
    Some(ResolvedPane {
        pane_id: parts[0].into(),
        window_id: parts[1].into(),
        session: parts[2].into(),
        window_name: parts[3].into(),
        window_index: parts[4].into(),
    })
}

/// tad's own window id, when running in a regular tmux pane. None =
/// `$TMUX_PANE` unset, the display-popup (popup panes belong to no
/// window, so the format expands empty — verified live), or tmux error.
pub(super) fn tad_window_id() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let out = crate::tmux::run(["display-message", "-p", "-t", &pane, "#{window_id}"])
        .ok()
        .filter(|o| o.status.success())?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Join the resolved pane beside tad (right, 60%) and focus it
/// (join-pane focuses the moved pane by default).
pub(super) fn pull_pane(p: &ResolvedPane) -> bool {
    let Ok(tad_pane) = std::env::var("TMUX_PANE") else {
        return false;
    };
    let out = crate::tmux::run([
        "join-pane",
        "-h",
        "-l",
        "60%",
        "-s",
        &p.pane_id,
        "-t",
        &tad_pane,
    ]);
    matches!(out, Ok(o) if o.status.success())
}

/// True when the tmux target still resolves to a non-empty `{fmt}`.
fn target_alive(target: &str, fmt: &str) -> bool {
    crate::tmux::run(["display-message", "-p", "-t", target, fmt])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// Send a pinned pane home.
///   * pane gone (user killed it while out) → no-op
///   * origin window alive → join back into it (`-d`: focus stays here)
///   * window closed (the pane was its only one) → break-pane into the
///     origin session, restore the window name, try the old index
///     (best-effort — "index in use" is fine)
///   * origin session also gone → break-pane with no target lands it as
///     its own window in tad's session, renamed; not stranded beside tad
///
/// Callers are responsible for removing the pane from `App::pins`.
pub(super) fn return_pane(p: &super::PinnedPane) {
    if !target_alive(&p.pane_id, "#{pane_id}") {
        return;
    }
    if target_alive(&p.origin_window_id, "#{window_id}") {
        let _ = crate::tmux::run([
            "join-pane",
            "-d",
            "-h",
            "-s",
            &p.pane_id,
            "-t",
            &p.origin_window_id,
        ]);
        return;
    }
    let dst = format!("{}:", p.origin_session);
    let broke = crate::tmux::run(["break-pane", "-d", "-s", &p.pane_id, "-t", &dst]);
    if !matches!(broke, Ok(o) if o.status.success()) {
        // Origin session died too — break into tad's own session so the
        // pane at least isn't stuck beside the dashboard.
        let _ = crate::tmux::run(["break-pane", "-d", "-s", &p.pane_id]);
    }
    // rename-window resolves a %pane target to the window containing it — works after either break path.
    let _ = crate::tmux::run(["rename-window", "-t", &p.pane_id, &p.origin_window_name]);
    let idx = format!("{}:{}", p.origin_session, p.origin_window_index);
    let _ = crate::tmux::run(["move-window", "-d", "-s", &p.pane_id, "-t", &idx]);
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
