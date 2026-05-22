//! Discovery layer for Claude Code agents running across tmux panes.
//!
//! The "Agents" dashboard view and the tmux status-line segment (`tad status`)
//! both read from this module. Detection is process-tree based: enumerate
//! every tmux pane, walk the descendant pids of each pane shell, and match a
//! process named `claude`. Activity comes from the mtime of the most recent
//! `.jsonl` transcript file under `~/.claude/projects/<encoded-cwd>/` — that's
//! the SDK's standard session-transcript format, so it's the most stable
//! signal we can read without hooks.
//!
//! Linux-only: process-tree walking reads `/proc/<pid>/task/<tid>/children`.
//! tad's release artifacts are Linux x86_64, so this is in line with the
//! rest of the project.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct Agent {
    /// `session:window.pane` — pass directly to `tmux switch-client -t` /
    /// `tmux select-pane -t`.
    pub target: String,
    pub session: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_index: String,
    pub cwd: PathBuf,
    /// PID of the `claude` process inside the pane (not the pane shell).
    pub claude_pid: u32,
    /// Mtime of the most recent transcript jsonl, if one exists in the
    /// encoded-cwd directory under `~/.claude/projects/`.
    pub last_activity: Option<SystemTime>,
    pub transcript_path: Option<PathBuf>,
}

impl Agent {
    /// "Working" if the transcript mtime is within `active_window`, else
    /// "idle". `None` if we couldn't find any transcript at all.
    pub fn activity_status(&self, active_window: Duration) -> ActivityStatus {
        let Some(t) = self.last_activity else {
            return ActivityStatus::NoTranscript;
        };
        match SystemTime::now().duration_since(t) {
            Ok(elapsed) if elapsed <= active_window => ActivityStatus::Active(elapsed),
            Ok(elapsed) => ActivityStatus::Idle(elapsed),
            // Clock went backwards — treat as active rather than crash.
            Err(_) => ActivityStatus::Active(Duration::ZERO),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityStatus {
    Active(Duration),
    Idle(Duration),
    NoTranscript,
}

/// Scan every tmux pane on the running server, return one Agent per pane
/// whose process tree contains a `claude` process. Empty Vec if tmux isn't
/// running or no agents found.
pub fn scan() -> Vec<Agent> {
    let Some(output) = list_panes() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split('\x1f').collect();
        if parts.len() != 6 {
            continue;
        }
        let pane_pid: u32 = match parts[4].parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let Some(claude_pid) = find_claude_pid(pane_pid) else {
            continue;
        };
        let cwd = PathBuf::from(parts[5]);
        let transcript_path = latest_transcript(&cwd);
        let last_activity = transcript_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        out.push(Agent {
            target: format!("{}:{}.{}", parts[0], parts[1], parts[3]),
            session: parts[0].to_string(),
            window_index: parts[1].to_string(),
            window_name: parts[2].to_string(),
            pane_index: parts[3].to_string(),
            cwd,
            claude_pid,
            last_activity,
            transcript_path,
        });
    }
    // Most recently active first, no-transcript last.
    out.sort_by(|a, b| match (a.last_activity, b.last_activity) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.target.cmp(&b.target),
    });
    out
}

fn list_panes() -> Option<String> {
    // \x1f (US — Unit Separator) keeps the parse robust against window names
    // with spaces, dashes, colons, dots, etc.
    let out = Command::new("tmux")
        .args([
            "list-panes",
            "-aF",
            "#{session_name}\x1f#{window_index}\x1f#{window_name}\x1f#{pane_index}\x1f#{pane_pid}\x1f#{pane_current_path}",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// BFS the descendant process tree rooted at `root_pid`, returning the first
/// pid we find whose `comm` matches `claude`. Stops as soon as one is found.
fn find_claude_pid(root_pid: u32) -> Option<u32> {
    let mut stack = vec![root_pid];
    // Cheap loop guard against pathological /proc states.
    let mut visited = 0usize;
    while let Some(pid) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            return None;
        }
        if is_claude(pid) {
            return Some(pid);
        }
        push_children(pid, &mut stack);
    }
    None
}

/// Match either the bare `claude` binary or `claude-*` wrapper variants
/// (some users symlink under names like `claude-code`). 15-byte `comm` is
/// truncated by the kernel, so anything longer than that won't match here —
/// but `claude` itself fits comfortably.
fn is_claude(pid: u32) -> bool {
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return false;
    };
    let comm = comm.trim();
    comm == "claude" || comm.starts_with("claude-") || comm.starts_with("claude ")
}

fn push_children(pid: u32, out: &mut Vec<u32>) {
    let task_dir = format!("/proc/{pid}/task");
    let Ok(tids) = std::fs::read_dir(&task_dir) else {
        return;
    };
    for tid_entry in tids.flatten() {
        let children_path = tid_entry.path().join("children");
        let Ok(s) = std::fs::read_to_string(&children_path) else {
            continue;
        };
        for tok in s.split_ascii_whitespace() {
            if let Ok(child) = tok.parse::<u32>() {
                out.push(child);
            }
        }
    }
}

/// Claude Code stores transcripts under `~/.claude/projects/<encoded-cwd>/`
/// where the encoding is "every `/` becomes `-`". So `/home/me/repo` becomes
/// `-home-me-repo`. Returns the path even if it doesn't exist on disk; use
/// `latest_transcript` to get the actually-present jsonl.
pub fn transcript_dir(cwd: &Path) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
        .join(encoded_cwd(cwd))
}

fn encoded_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

/// Most recently-modified `.jsonl` in the encoded-cwd dir, or `None` if the
/// dir is missing or has no jsonl files.
pub fn latest_transcript(cwd: &Path) -> Option<PathBuf> {
    let dir = transcript_dir(cwd);
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut latest: Option<(PathBuf, SystemTime)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        match &latest {
            Some((_, t)) if *t >= mtime => {}
            _ => latest = Some((path, mtime)),
        }
    }
    latest.map(|(p, _)| p)
}

/// Aggregate counts for the status-line segment.
pub struct StatusCounts {
    pub total: usize,
    pub active: usize,
    pub idle: usize,
}

pub fn counts(agents: &[Agent], active_window: Duration) -> StatusCounts {
    let total = agents.len();
    let active = agents
        .iter()
        .filter(|a| matches!(a.activity_status(active_window), ActivityStatus::Active(_)))
        .count();
    StatusCounts {
        total,
        active,
        idle: total - active,
    }
}

/// Human-friendly "Xs/Xm/Xh ago" formatter shared by the dashboard preview
/// and the agents-view line formatter.
pub fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_cwd_replaces_slashes_with_dashes() {
        assert_eq!(
            encoded_cwd(Path::new("/home/me/git/tad-github")),
            "-home-me-git-tad-github"
        );
        assert_eq!(encoded_cwd(Path::new("/")), "-");
    }

    #[test]
    fn transcript_dir_lives_under_dot_claude_projects() {
        let p = transcript_dir(Path::new("/home/me/repo"));
        let s = p.to_string_lossy();
        assert!(s.contains("/.claude/projects/-home-me-repo"));
    }

    #[test]
    fn format_elapsed_uses_appropriate_unit() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m");
        assert_eq!(format_elapsed(Duration::from_secs(3 * 3600 + 10)), "3h");
        assert_eq!(format_elapsed(Duration::from_secs(2 * 86_400)), "2d");
    }

    #[test]
    fn counts_partitions_active_and_idle() {
        let now = SystemTime::now();
        let mk = |t: Option<SystemTime>| Agent {
            target: "s:0.0".into(),
            session: "s".into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/tmp"),
            claude_pid: 1,
            last_activity: t,
            transcript_path: None,
        };
        let agents = vec![
            mk(Some(now)),                           // active
            mk(Some(now - Duration::from_secs(90))), // idle
            mk(None),                                // counts as idle
        ];
        let c = counts(&agents, Duration::from_secs(30));
        assert_eq!(c.total, 3);
        assert_eq!(c.active, 1);
        assert_eq!(c.idle, 2);
    }

    #[test]
    fn activity_status_classifies_within_window() {
        let agent = Agent {
            target: "s:0.0".into(),
            session: "s".into(),
            window_index: "0".into(),
            window_name: "w".into(),
            pane_index: "0".into(),
            cwd: PathBuf::from("/tmp"),
            claude_pid: 1,
            last_activity: Some(SystemTime::now() - Duration::from_secs(5)),
            transcript_path: None,
        };
        assert!(matches!(
            agent.activity_status(Duration::from_secs(30)),
            ActivityStatus::Active(_)
        ));
        assert!(matches!(
            agent.activity_status(Duration::from_secs(1)),
            ActivityStatus::Idle(_)
        ));
    }

    /// Smoke test: walking our own current process should succeed (we won't
    /// find a `claude` in our test process tree, so it should be None — but
    /// importantly the function must terminate and not panic).
    #[test]
    fn find_claude_pid_on_self_terminates() {
        let me = std::process::id();
        // We're running under cargo test, no claude here.
        assert!(find_claude_pid(me).is_none());
    }
}
