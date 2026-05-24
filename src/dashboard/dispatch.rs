//! Post-dashboard dispatch: what to do once the user picked a row and
//! the dashboard exited. Lives separate from `keys.rs` because the
//! handlers only build [`OpenTarget`] values — the actual tmux side
//! effects (switching pane, spawning a window, creating a session) all
//! happen here after the alternate-screen has been torn down.

use anyhow::Result;

use crate::agents;
use crate::projects::{self, Project};
use crate::sessions;
use crate::tmux;

use super::AppData;

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
    /// Spawn a new `claude` agent inside a project. Resolves the project
    /// root from a fresh `projects::scan()` at dispatch time so we pick
    /// up sessions that appeared between the modal opening and the
    /// user confirming.
    SpawnAgent {
        project_name: String,
        prompt: Option<String>,
    },
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

/// Spawn `claude` in a new tmux window inside `project_name`'s root.
///
///   * if the project already has at least one tmux session, add a new
///     window to it (uses the alphabetically-first session for stability)
///   * otherwise create a new detached session named after the project
///   * either way, switch the current client to the new window (if
///     we're inside tmux) or attach (if not)
///
/// `prompt` is passed as a single positional arg to `claude`, properly
/// shell-quoted so prompts with spaces / quotes / dollar signs don't
/// get mangled.
pub(super) fn spawn_agent_in_project(project_name: &str, prompt: Option<&str>) -> Result<i32> {
    // Fresh post-dashboard scan so we see any session/agent the user
    // might have spawned in another window between opening the modal
    // and confirming. Pays one round of tmux + /proc + transcript
    // reads, which is fine since dispatch is one-shot.
    let sessions = sessions::list().unwrap_or_default();
    let agents = agents::scan();
    let projects = projects::from_scanned(&sessions, &agents);
    let p = projects
        .iter()
        .find(|p| p.name == project_name)
        .ok_or_else(|| anyhow::anyhow!("project {} no longer exists", project_name))?;
    let root_str = p.root.to_string_lossy().into_owned();
    // Provider-agnostic: the spawn command (and its prompt-quoting)
    // belongs to the provider. Pick the provider the project's
    // existing agents are using (so a project full of `aider` doesn't
    // get an unexpected `claude`); fall back to the configured
    // default when there's no existing agent to learn from.
    let prov = p
        .agents
        .first()
        .and_then(|a| crate::provider::by_id(a.provider_id))
        .unwrap_or_else(crate::provider::default_provider);
    let cmd = prov.spawn_command(prompt);

    let (session_name, window_target) = if let Some(s) = p.sessions.first() {
        let out = tmux::run([
            "new-window",
            "-t",
            &s.name,
            "-c",
            &root_str,
            "-n",
            project_name,
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
            &cmd,
        ])?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux new-window failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let target = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (s.name.clone(), target)
    } else {
        // Ask tmux for the actual created target via `-P -F` rather than
        // assuming `{project_name}:0`. If the requested session name was
        // already taken (race with another spawn / external tmux work)
        // tmux may pick a different name, and the window index isn't
        // always 0 for new-session either if the user has window-base-
        // index set. Reading the real target avoids switching to a
        // window that doesn't exist.
        let out = tmux::run([
            "new-session",
            "-d",
            "-s",
            project_name,
            "-c",
            &root_str,
            "-n",
            project_name,
            "-P",
            "-F",
            "#{session_name}:#{window_index}",
            &cmd,
        ])?;
        if !out.status.success() {
            anyhow::bail!(
                "tmux new-session failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let target = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Pull the session name back out of the target for the
        // attach-fallback branch below; if anything's malformed, fall
        // back to the project name as a best guess.
        let session = target
            .split(':')
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| project_name.to_string());
        (session, target)
    };

    let inside_tmux = std::env::var_os("TMUX").is_some();
    if inside_tmux {
        let _ = tmux::run(["switch-client", "-t", &window_target]);
    } else {
        let _ = tmux::run(["attach", "-t", &session_name]);
    }
    Ok(0)
}

/// Find the longest-prefix project of `$PWD` in the scanned list.
/// Returns the row index for preselection — `None` when PWD isn't
/// under any known project (e.g. `tad` was launched from /tmp).
pub(super) fn current_project_index(projects: &[Project]) -> Option<usize> {
    let pwd = std::env::current_dir().ok()?;
    let mut best: Option<(usize, usize)> = None;
    for (i, p) in projects.iter().enumerate() {
        if pwd.starts_with(&p.root) {
            let len = p.root.as_os_str().len();
            if best.map(|(_, l)| len > l).unwrap_or(true) {
                best = Some((i, len));
            }
        }
    }
    best.map(|(i, _)| i)
}

/// Project Enter: prefer attaching to the project's most-recently-active
/// session; fall back to jumping to its most-recently-active agent's
/// pane; if the project has neither, do nothing.
pub(super) fn project_enter_target(data: &AppData, name: &str) -> Option<OpenTarget> {
    let p = data.projects.iter().find(|p| p.name == name)?;
    if let Some(s) = p.sessions.iter().max_by_key(|s| s.activity_ts) {
        return Some(OpenTarget::AttachExisting(s.name.clone()));
    }
    if let Some(a) = p
        .agents
        .iter()
        .max_by_key(|a| a.last_activity.unwrap_or(std::time::SystemTime::UNIX_EPOCH))
    {
        return Some(OpenTarget::JumpToPane(a.target.clone()));
    }
    None
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
    use super::super::AppData;
    use super::*;
    use crate::agents::Agent;
    use crate::sessions::Session;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

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

    // shell_quote moved to `crate::shell::quote`; coverage is in shell.rs.

    /// Project Enter dispatch picks the most-recently-active session,
    /// falls back to the most-recently-active agent, returns None when
    /// neither exists.
    #[test]
    fn project_enter_falls_back_through_session_agent_none() {
        let now = SystemTime::now();
        let agent = Agent {
            target: "s:0.0".into(),
            session: "s".into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/repo"),
            agent_pid: 1,
            provider_id: "claude",
            last_activity: Some(now),
            transcript_path: None,
            attention: crate::transcript::Attention::Unknown,
        };
        let session = Session {
            name: "s".into(),
            windows: 1,
            attached: false,
            active_window: "w".into(),
            active_path: "/repo".into(),
            created_ts: 0,
            activity_ts: 100,
            activity_str: "1m".into(),
        };

        let data_empty = AppData {
            sessions: vec![],
            groups: vec![],
            hosts: vec![],
            agents: vec![],
            snoozes: crate::snooze::SnoozeState::default(),
            ui: crate::ui_config::UiConfig::default(),
            projects: vec![Project {
                root: PathBuf::from("/repo"),
                name: "repo".into(),
                sessions: vec![],
                agents: vec![],
                last_activity: None,
            }],
        };
        assert!(project_enter_target(&data_empty, "repo").is_none());

        let data_agent_only = AppData {
            sessions: vec![],
            groups: vec![],
            hosts: vec![],
            agents: vec![agent.clone()],
            snoozes: crate::snooze::SnoozeState::default(),
            ui: crate::ui_config::UiConfig::default(),
            projects: vec![Project {
                root: PathBuf::from("/repo"),
                name: "repo".into(),
                sessions: vec![],
                agents: vec![agent.clone()],
                last_activity: Some(now),
            }],
        };
        assert!(matches!(
            project_enter_target(&data_agent_only, "repo"),
            Some(OpenTarget::JumpToPane(t)) if t == "s:0.0"
        ));

        let data_session_wins = AppData {
            sessions: vec![session.clone()],
            groups: vec![],
            hosts: vec![],
            agents: vec![agent.clone()],
            snoozes: crate::snooze::SnoozeState::default(),
            ui: crate::ui_config::UiConfig::default(),
            projects: vec![Project {
                root: PathBuf::from("/repo"),
                name: "repo".into(),
                sessions: vec![session.clone()],
                agents: vec![agent.clone()],
                last_activity: Some(now - Duration::from_secs(60)),
            }],
        };
        assert!(matches!(
            project_enter_target(&data_session_wins, "repo"),
            Some(OpenTarget::AttachExisting(n)) if n == "s"
        ));
    }
}
