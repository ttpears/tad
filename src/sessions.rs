//! Session enumeration, attach/create, and completion output.

use anyhow::Result;
use std::io::{BufRead, BufReader, Write};

use crate::tmux;

#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub active_window: String,
    pub active_path: String,
    #[allow(dead_code)]
    pub created_ts: i64,
    pub activity_ts: i64,
    pub activity_str: String,
}

const FMT: &str =
    "#{session_name}\t#{session_windows}\t#{session_attached}\t#{W:#{?window_active,#{window_name}#,#{pane_current_path},}}\t#{session_created}\t#{session_activity}\t#{t/p:session_activity}";

pub fn list() -> Result<Vec<Session>> {
    let out = tmux::run(["list-sessions", "-F", FMT])?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.splitn(7, '\t').collect();
        if parts.len() < 7 {
            continue;
        }
        let active = parts[3];
        let (win, _, path) = match active.find(',') {
            Some(idx) => (&active[..idx], ",", &active[idx + 1..]),
            None => (active, "", ""),
        };
        sessions.push(Session {
            name: parts[0].to_string(),
            windows: parts[1].parse().unwrap_or(0),
            attached: parts[2] != "0" && !parts[2].is_empty(),
            active_window: win.to_string(),
            active_path: path.to_string(),
            created_ts: parts[4].parse().unwrap_or(0),
            activity_ts: parts[5].parse().unwrap_or(0),
            activity_str: parts[6].to_string(),
        });
    }
    Ok(sessions)
}

/// Print `state<TAB>name:description` for shell completion.
pub fn print_completions() -> Result<()> {
    let mut sessions = list()?;
    sessions.sort_by(|a, b| b.activity_ts.cmp(&a.activity_ts));
    let home = std::env::var("HOME").unwrap_or_default();
    for s in &sessions {
        let win = truncate(
            if s.active_window.is_empty() {
                "—"
            } else {
                &s.active_window
            },
            10,
        );
        let mut path = s.active_path.clone();
        if !home.is_empty() && path.starts_with(&home) {
            path = format!("~{}", &path[home.len()..]);
        }
        let path = truncate(&path, 38);
        let desc = format!(
            "{:>3} win  ·  {:<10}  ·  {:<38}  ·  {:>8}",
            s.windows, win, path, s.activity_str
        );
        let safe_name = s.name.replace(':', "\\:");
        let state = if s.attached { "attached" } else { "detached" };
        println!("{}\t{}:{}", state, safe_name, desc);
    }
    Ok(())
}

pub fn attach_or_create(name: &str) -> Result<i32> {
    if tmux::has_session(name) {
        return tmux::enter(name);
    }
    if !confirm(&format!("Create new tmux session named {}?", name)) {
        return Ok(1);
    }
    tmux::try_run(["new-session", "-d", "-s", name])?;
    tmux::enter(name)
}

/// Attach if it exists, else create without prompting. Optional `host` runs
/// `ssh <host>` as the new window's command. Used by the dashboard, where
/// the user already confirmed by typing the name and pressing Enter.
pub fn attach_or_create_silent(name: &str, host: Option<&str>) -> Result<i32> {
    if tmux::has_session(name) {
        return tmux::enter(name);
    }
    match host {
        Some(h) => {
            let cmd = format!("ssh {}", h);
            tmux::try_run(["new-session", "-d", "-s", name, &cmd])?;
        }
        None => {
            tmux::try_run(["new-session", "-d", "-s", name])?;
        }
    }
    tmux::enter(name)
}

/// Open a single-host tmux session whose name is the short hostname.
pub fn attach_or_create_remote(fqdn: &str) -> Result<i32> {
    let short = fqdn.split('.').next().unwrap_or(fqdn).to_string();
    if !tmux::has_session(&short) {
        tmux::try_run(["new-session", "-d", "-s", &short, &format!("ssh {}", fqdn)])?;
    }
    tmux::enter(&short)
}

/// Numbered `select`-style picker used as fallback when the TUI dashboard
/// can't run (non-TTY, broken terminal, etc.).
pub fn picker_fallback() -> Result<i32> {
    let sessions = list()?;
    if sessions.is_empty() {
        eprint!("No tmux sessions. Name for a new one (blank to cancel): ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok();
        let name = line.trim();
        if name.is_empty() {
            return Ok(1);
        }
        return attach_or_create(name);
    }
    eprintln!("Existing sessions:");
    for (i, s) in sessions.iter().enumerate() {
        eprintln!(
            "  {}) {} ({} windows){}",
            i + 1,
            s.name,
            s.windows,
            if s.attached { " [attached]" } else { "" }
        );
    }
    eprint!("Select (number, or 0 to cancel): ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    let raw = line.trim();
    if raw == "0" || raw.is_empty() {
        return Ok(1);
    }
    let idx: usize = raw.parse().unwrap_or(0);
    if idx == 0 || idx > sessions.len() {
        return Ok(1);
    }
    attach_or_create(&sessions[idx - 1].name)
}

fn confirm(prompt: &str) -> bool {
    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        let mut writer = tty.try_clone().unwrap();
        let _ = write!(writer, "{} [Y/n] ", prompt);
        writer.flush().ok();
        let mut reader = BufReader::new(tty);
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            let v = line.trim().to_lowercase();
            return v.is_empty() || v.starts_with('y');
        }
    }
    false
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
