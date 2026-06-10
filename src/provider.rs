//! Provider abstraction over "AI coding agents tad knows how to find
//! and classify." Today there's exactly one implementation
//! ([`ClaudeProvider`]) — the trait exists so that adding aider /
//! codex / opencode / whatever someone uses next is a single new file
//! plus an entry in [`providers()`], not a grep-and-replace across
//! `agents.rs`, `transcript.rs`, `dispatch.rs`, and `cli.rs`.
//!
//! Provider responsibility is the *coupled* part of agent support —
//! the bits that change between tools:
//!
//!   * what the process is called on disk (`matches_comm`)
//!   * where its session state lives (`latest_transcript`)
//!   * how to read "is this agent waiting for me?" from that state
//!     (`classify_attention`)
//!
//! The *decoupled* parts — process-tree walking, tmux pane scanning,
//! the watcher state machine, snooze handling, the Agents view —
//! are all provider-agnostic and live where they already live.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::transcript::Attention;

pub(crate) trait Provider: Send + Sync {
    /// Short stable identifier used as the value of `Agent::provider_id`.
    /// Lowercase, ASCII, no spaces — appears in logs and could
    /// eventually appear in config.
    fn id(&self) -> &'static str;

    /// Human-friendly display name. Used in error messages, doctor
    /// output, README references. Free-form.
    fn label(&self) -> &'static str;

    /// True iff a process whose `/proc/<pid>/comm` reads as `comm` is
    /// an instance of this provider. Cheap; called for every process
    /// in every pane's process tree on every scan.
    fn matches_comm(&self, comm: &str) -> bool;

    /// The most recent session transcript for an agent running with
    /// the given cwd, or None if we don't have one (provider stores
    /// transcripts elsewhere, agent just started, etc.). The path's
    /// mtime is used as `last_activity`.
    fn latest_transcript(&self, cwd: &Path) -> Option<PathBuf>;

    /// Read the tail of `transcript` and decide whether the agent is
    /// currently waiting on the user. Called once per scan per agent;
    /// cached internally by [`crate::transcript::classify_file`] so
    /// the steady-state cost is one `stat`.
    fn classify_attention(&self, transcript: &Path) -> Attention;

}

// ---- Claude Code provider ----

pub(crate) struct ClaudeProvider;

static CLAUDE_PROVIDER: ClaudeProvider = ClaudeProvider;

impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn matches_comm(&self, comm: &str) -> bool {
        // Linux truncates `comm` at 15 bytes; `claude` fits in 6.
        // The `claude-*` / `claude *` prefixes catch wrapper scripts
        // some users symlink under names like `claude-code`.
        comm == "claude" || comm.starts_with("claude-") || comm.starts_with("claude ")
    }

    fn latest_transcript(&self, cwd: &Path) -> Option<PathBuf> {
        let dir = claude_transcript_dir(cwd);
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

    fn classify_attention(&self, transcript: &Path) -> Attention {
        crate::transcript::classify_file(transcript)
    }
}

/// Claude Code stores transcripts under `~/.claude/projects/<encoded-cwd>/`
/// where the encoding is "every `/` becomes `-`". So `/home/me/repo`
/// becomes `-home-me-repo`. Implementation detail of [`ClaudeProvider`]
/// — other providers will have their own conventions.
fn claude_transcript_dir(cwd: &Path) -> PathBuf {
    let encoded = cwd.to_string_lossy().replace('/', "-");
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
        .join(encoded)
}

// ---- Registry ----

/// All known providers in priority order. Today: just `ClaudeProvider`.
/// Adding a second provider is one entry here plus one impl above.
pub(crate) fn providers() -> &'static [&'static dyn Provider] {
    static SLICE: &[&dyn Provider] = &[&CLAUDE_PROVIDER];
    SLICE
}

/// Fallback provider when an agent's `provider_id` can't be matched
/// against the registry (e.g. the id is absent or unknown). Today
/// there's only one provider so this is always `ClaudeProvider`.
pub(crate) fn default_provider() -> &'static dyn Provider {
    &CLAUDE_PROVIDER
}

/// Look up a provider by its id slug. Returns `None` if no provider
/// with that id is registered — callers that store `Agent::provider_id`
/// should be prepared for this when a provider is removed between
/// `Agent`-build time and lookup.
pub(crate) fn by_id(id: &str) -> Option<&'static dyn Provider> {
    providers().iter().copied().find(|p| p.id() == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_at_least_claude() {
        assert!(providers().iter().any(|p| p.id() == "claude"));
        assert_eq!(default_provider().id(), "claude");
        assert_eq!(by_id("claude").map(|p| p.id()), Some("claude"));
        assert!(by_id("nope").is_none());
    }

    #[test]
    fn claude_matches_canonical_comm_forms() {
        let p = ClaudeProvider;
        assert!(p.matches_comm("claude"));
        assert!(p.matches_comm("claude-code"));
        assert!(p.matches_comm("claude wrapper")); // space-prefix wrappers
        assert!(!p.matches_comm("claudette"));
        assert!(!p.matches_comm("not-claude"));
        assert!(!p.matches_comm(""));
    }

    #[test]
    fn claude_transcript_dir_encodes_path_with_dashes() {
        let dir = claude_transcript_dir(Path::new("/home/me/git/tad-github"));
        let s = dir.to_string_lossy();
        assert!(s.contains("/.claude/projects/-home-me-git-tad-github"));
    }

}
