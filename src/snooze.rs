//! Per-agent snooze state shared between the dashboard and `tad watch`.
//!
//! The dashboard writes snoozes from the `s` modal in the Agents view;
//! the watcher reads them on every tick and suppresses popups for any
//! target whose deadline is still in the future. Stored as a tiny YAML
//! file under `$XDG_STATE_HOME/tad/snooze.yaml` keyed by tmux target
//! (`session:window.pane`) → unix-epoch-seconds-until.
//!
//! Why YAML and not JSON: serde_yml is already a dep (config.yaml uses
//! it). One less crate for a 2-field file format isn't free.
//!
//! Pruning of expired entries happens lazily on every read/write — no
//! background reaper, the file caps itself at the working set of
//! currently-snoozed agents.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct SnoozeState {
    /// target → unix-epoch-seconds when the snooze expires.
    #[serde(default)]
    pub snoozes: HashMap<String, u64>,
}

fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tad")
        .join("snooze.yaml")
}

/// Load the snooze state, dropping anything already expired. Missing /
/// unreadable / malformed file → empty state (snoozing is best-effort
/// and we never want a corrupt file to keep the watcher silent forever).
pub fn load(now: SystemTime) -> SnoozeState {
    let path = state_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return SnoozeState::default(),
    };
    let mut state: SnoozeState = serde_yml::from_str(&text).unwrap_or_default();
    prune_expired(&mut state, now);
    state
}

pub fn save(state: &SnoozeState) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let text = serde_yml::to_string(state)?;
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn now_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn prune_expired(state: &mut SnoozeState, now: SystemTime) {
    let cutoff = now_secs(now);
    state.snoozes.retain(|_, until| *until > cutoff);
}

pub fn is_snoozed(state: &SnoozeState, target: &str, now: SystemTime) -> bool {
    state
        .snoozes
        .get(target)
        .map(|t| *t > now_secs(now))
        .unwrap_or(false)
}

/// Convenience: snooze a target for `duration`, writing the file. Last
/// snooze wins (re-snoozing the same target replaces the deadline).
pub fn snooze(target: &str, duration: Duration) -> Result<()> {
    let now = SystemTime::now();
    let mut state = load(now);
    let until = now_secs(now).saturating_add(duration.as_secs());
    state.snoozes.insert(target.to_string(), until);
    save(&state)
}

/// Convenience: clear any snooze for `target`. No-op if none set.
pub fn clear(target: &str) -> Result<()> {
    let now = SystemTime::now();
    let mut state = load(now);
    if state.snoozes.remove(target).is_some() {
        save(&state)?;
    }
    Ok(())
}

/// "in 5 minutes" / "in 2 hours" / "in 3 days" — used by the modal
/// list and the agent preview's snooze badge.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3600 {
        format!("in {}m", secs / 60)
    } else if secs < 86_400 {
        let h = secs / 3600;
        let rem_m = (secs % 3600) / 60;
        if rem_m == 0 {
            format!("in {h}h")
        } else {
            format!("in {h}h{rem_m}m")
        }
    } else {
        format!("in {}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn is_snoozed_respects_deadline() {
        let now = mk_now();
        let mut state = SnoozeState::default();
        state.snoozes.insert("s:0.0".into(), now_secs(now) + 60);
        state.snoozes.insert("s:0.1".into(), now_secs(now) - 1);
        assert!(is_snoozed(&state, "s:0.0", now));
        assert!(!is_snoozed(&state, "s:0.1", now));
        assert!(!is_snoozed(&state, "never-snoozed", now));
    }

    #[test]
    fn prune_drops_expired_entries() {
        let now = mk_now();
        let mut state = SnoozeState::default();
        state.snoozes.insert("alive".into(), now_secs(now) + 60);
        state.snoozes.insert("expired".into(), now_secs(now) - 1);
        prune_expired(&mut state, now);
        assert!(state.snoozes.contains_key("alive"));
        assert!(!state.snoozes.contains_key("expired"));
    }

    #[test]
    fn format_duration_uses_appropriate_unit() {
        assert_eq!(format_duration(Duration::from_secs(45)), "in 45s");
        assert_eq!(format_duration(Duration::from_secs(5 * 60)), "in 5m");
        assert_eq!(format_duration(Duration::from_secs(2 * 3600)), "in 2h");
        assert_eq!(
            format_duration(Duration::from_secs(2 * 3600 + 30 * 60)),
            "in 2h30m"
        );
        assert_eq!(format_duration(Duration::from_secs(3 * 86_400)), "in 3d");
    }

    #[test]
    fn yaml_round_trip() {
        let mut state = SnoozeState::default();
        state.snoozes.insert("s:0.0".into(), 1_700_000_000);
        state.snoozes.insert("other:1.2".into(), 1_700_000_300);
        let yaml = serde_yml::to_string(&state).unwrap();
        let back: SnoozeState = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(back.snoozes.len(), 2);
        assert_eq!(back.snoozes.get("s:0.0"), Some(&1_700_000_000));
    }

    #[test]
    fn empty_yaml_loads_as_default() {
        let state: SnoozeState = serde_yml::from_str("snoozes: {}").unwrap();
        assert!(state.snoozes.is_empty());
    }
}
