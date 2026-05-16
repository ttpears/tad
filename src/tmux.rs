//! Thin wrappers around the `tmux` CLI. We shell out via std::process rather
//! than depend on a tmux crate — the surface is small and shelling is the
//! same model tmux itself documents.

use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Run `tmux ARGS` and return the captured stdout (and the full Output).
pub fn run<I, S>(args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::new("tmux")
        .args(args)
        .output()
        .context("failed to spawn tmux")?;
    Ok(out)
}

/// Run `tmux ARGS`. If it exits non-zero, print stderr and propagate as an
/// error. Returns stdout on success.
pub fn try_run<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr> + std::fmt::Debug,
{
    let args_vec: Vec<S> = args.into_iter().collect();
    let argv_dbg: Vec<String> = args_vec
        .iter()
        .map(|s| s.as_ref().to_string_lossy().into_owned())
        .collect();
    let out = Command::new("tmux")
        .args(&args_vec)
        .output()
        .context("failed to spawn tmux")?;
    if !out.status.success() {
        eprintln!(
            "tmux {} (exit {}): {}",
            argv_dbg.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::bail!("tmux command failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True if a session by this name exists.
pub fn has_session(name: &str) -> bool {
    let status = Command::new("tmux")
        .args(["has-session", "-t", &exact_target(name)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success())
}

/// Format `=name` so tmux treats it as an exact session match.
pub fn exact_target(name: &str) -> String {
    format!("={}", name)
}

/// True if we're already inside a tmux client.
pub fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Attach to or switch into an existing session, exec-replacing if outside tmux.
pub fn enter(name: &str) -> Result<i32> {
    let target = exact_target(name);
    if in_tmux() {
        try_run(["switch-client", "-t", &target])?;
        Ok(0)
    } else {
        // exec replaces our process; on success it never returns.
        let err = Command::new("tmux")
            .args(["attach", "-d", "-t", &target])
            .exec();
        Err(err.into())
    }
}

/// Create a detached session, with an initial window running the given shell command.
pub fn new_session_detached(name: &str, window_name: &str, shell_cmd: &str) -> Result<()> {
    try_run([
        "new-session",
        "-d",
        "-s",
        name,
        "-n",
        window_name,
        shell_cmd,
    ])?;
    Ok(())
}

/// Split the active pane of the named session's active window.
pub fn split_window(session: &str, shell_cmd: &str) -> Result<()> {
    try_run(["split-window", "-t", &exact_target(session), shell_cmd])?;
    Ok(())
}

/// New window appended after the largest used index in the session.
pub fn new_window(session: &str, window_name: &str, shell_cmd: &str) -> Result<()> {
    try_run([
        "new-window",
        "-a",
        "-t",
        &exact_target(session),
        "-n",
        window_name,
        shell_cmd,
    ])?;
    Ok(())
}

pub fn select_layout(session: &str, layout: &str) -> Result<()> {
    try_run(["select-layout", "-t", &exact_target(session), layout])?;
    Ok(())
}

pub fn set_window_option(session: &str, key: &str, value: &str) -> Result<()> {
    try_run([
        "set-window-option",
        "-t",
        &exact_target(session),
        key,
        value,
    ])?;
    Ok(())
}

/// Select the first window in the session by index.
pub fn select_first_window(session: &str) -> Result<()> {
    let target = exact_target(session);
    let out = try_run(["list-windows", "-t", &target, "-F", "#{window_index}"])?;
    if let Some(first) = out.lines().next() {
        try_run(["select-window", "-t", &format!("{}:{}", target, first)])?;
    }
    Ok(())
}

pub fn kill_session(name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &exact_target(name)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
