//! Discovery layer for AI coding agents running across tmux panes.
//!
//! The "Agents" dashboard view and the tmux status-line segment (`tad
//! status`) both read from this module. Detection is process-tree
//! based: enumerate every tmux pane, walk the descendant pids of each
//! pane shell, and ask each registered [`crate::provider::Provider`]
//! whether the process matches. Activity comes from the mtime of the
//! provider's session-transcript file for the agent's cwd.
//!
//! Provider-agnostic by design — today there's exactly one provider
//! (Claude Code), but everything that varies between agent tools
//! (process name, transcript location, attention parsing) lives
//! behind the trait, not here.
//!
//! Linux-only: process-tree walking reads `/proc/<pid>/task/<tid>/children`.
//! tad's release artifacts are Linux x86_64, so this is in line with
//! the rest of the project.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

use crate::provider::{self, Provider};
use crate::transcript;

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
    /// PID of the agent process inside the pane (not the pane shell).
    pub agent_pid: u32,
    /// Id of the [`Provider`] that matched this process (e.g.
    /// `"claude"`). Use [`crate::provider::by_id`] to look up the
    /// provider impl for classify / transcript-path operations
    /// you want to do later — the registry may have changed between
    /// scan time and lookup, so handle `None`.
    pub provider_id: &'static str,
    /// Mtime of the most recent transcript file (provider-specific
    /// location), if one exists.
    pub last_activity: Option<SystemTime>,
    pub transcript_path: Option<PathBuf>,
    /// Best-effort "is this agent waiting for me right now" derived
    /// from the tail of the provider's transcript. Unknown when
    /// there's no transcript or the format is unfamiliar — callers
    /// should fall back to the mtime-based `activity_status` heuristic.
    pub attention: transcript::Attention,
}

impl Agent {
    /// "Working" if the transcript mtime is within `active_window`,
    /// else "idle". `NoTranscript` if we couldn't find any transcript.
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

/// Scan every tmux pane on the running server, return one Agent per
/// pane whose process tree contains a process matched by some
/// registered provider. Empty Vec if tmux isn't running or no agents
/// found.
pub fn scan() -> Vec<Agent> {
    let Some(output) = list_panes() else {
        return Vec::new();
    };
    let providers = provider::providers();
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
        let Some((agent_pid, prov)) = find_agent_pid(pane_pid, providers) else {
            continue;
        };
        let cwd = PathBuf::from(parts[5]);
        let transcript_path = prov.latest_transcript(&cwd);
        let last_activity = transcript_path
            .as_deref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        let attention = transcript_path
            .as_deref()
            .map(|p| prov.classify_attention(p))
            .unwrap_or(transcript::Attention::Unknown);
        out.push(Agent {
            target: format!("{}:{}.{}", parts[0], parts[1], parts[3]),
            session: parts[0].to_string(),
            window_index: parts[1].to_string(),
            window_name: parts[2].to_string(),
            pane_index: parts[3].to_string(),
            cwd,
            agent_pid,
            provider_id: prov.id(),
            last_activity,
            transcript_path,
            attention,
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

/// BFS the descendant process tree rooted at `root_pid` looking for a
/// process matched by any registered provider. Stops as soon as one is
/// found and returns the matched PID and the provider that claimed it.
/// First-provider-wins: providers earlier in the [`provider::providers`]
/// slice take precedence when both match the same comm (shouldn't
/// happen if `matches_comm` predicates are well-designed).
fn find_agent_pid(
    root_pid: u32,
    providers: &[&'static dyn Provider],
) -> Option<(u32, &'static dyn Provider)> {
    let mut stack = vec![root_pid];
    // Cheap loop guard against pathological /proc states.
    let mut visited = 0usize;
    while let Some(pid) = stack.pop() {
        visited += 1;
        if visited > 4096 {
            return None;
        }
        if let Some(comm) = process_comm(pid) {
            for p in providers {
                if p.matches_comm(&comm) {
                    return Some((pid, *p));
                }
            }
        }
        push_children(pid, &mut stack);
    }
    None
}

fn process_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
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

/// Human-friendly "Xs/Xm/Xh ago" formatter shared by the dashboard
/// preview and the agents-view line formatter.
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
    use std::path::PathBuf;

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
            agent_pid: 1,
            provider_id: "claude",
            last_activity: t,
            transcript_path: None,
            attention: crate::transcript::Attention::Unknown,
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
            agent_pid: 1,
            provider_id: "claude",
            last_activity: Some(SystemTime::now() - Duration::from_secs(5)),
            transcript_path: None,
            attention: crate::transcript::Attention::Unknown,
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

    /// Smoke test: walking our own current process should terminate
    /// (cargo test isn't running claude, so no match expected).
    #[test]
    fn find_agent_pid_on_self_terminates() {
        let me = std::process::id();
        assert!(find_agent_pid(me, provider::providers()).is_none());
    }
}
