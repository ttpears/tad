//! Shared machinery for tad's tmux-conf-editing surface.
//!
//! Used by `tmux-keybind` for the popup keybind block and by `install`
//! for the popup keybind + `#(tad status)` segment + `tad watch`
//! startup hook. Each consumer supplies its own marker pair and body;
//! this module handles path resolution, atomic writes, marker-delimited
//! upsert and remove, and best-effort tmux reload.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pick the tmux config file to modify, preferring user-managed local
/// overrides so we never clobber a framework's main `~/.tmux.conf`.
/// Probe order:
///   1. explicit override (caller's --conf-path)
///   2. `$TAD_TMUX_CONF` env var
///   3. `~/.tmux.conf.local`  (gpakosz/.tmux convention)
///   4. `~/.tmux.local.conf`  (alternate spelling some teams use)
///   5. `$XDG_CONFIG_HOME/tmux/tmux.conf` (if it exists)
///   6. `~/.tmux.conf` (classic; created if missing)
pub fn resolve_path(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    if let Some(env) = std::env::var_os("TAD_TMUX_CONF") {
        return PathBuf::from(env);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for candidate in [home.join(".tmux.conf.local"), home.join(".tmux.local.conf")] {
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Some(xdg) = dirs::config_dir() {
        let xdg_conf = xdg.join("tmux").join("tmux.conf");
        if xdg_conf.is_file() {
            return xdg_conf;
        }
    }
    home.join(".tmux.conf")
}

pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tad.tmp");
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(contents.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// True iff we're inside a tmux client and `tmux source-file <path>`
/// returned 0. Best-effort — failures are non-fatal.
///
/// Callers should only invoke this when they're confident the path
/// they're editing is the live tmux server's conf — sourcing an
/// arbitrary file into the user's live tmux applies its `set-option`
/// / `bind-key` / `set-hook` directives to the running server, which
/// is *not* what someone editing `~/.tmux.conf.local-test-copy`
/// expects. The `was_overridden` flag lets callers communicate that
/// they used `--conf-path` / `$TAD_TMUX_CONF` and we should skip the
/// reload to avoid polluting the live server.
pub fn try_reload(path: &Path, was_overridden: bool) -> bool {
    if was_overridden {
        return false;
    }
    if std::env::var_os("TMUX").is_none() {
        return false;
    }
    Command::new("tmux")
        .args(["source-file", &path.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum UpsertOutcome {
    /// Block was added (no prior block with that marker pair).
    Added,
    /// Existing block was replaced because its body differed.
    Updated,
    /// Existing block already had the requested body — no write.
    Unchanged,
}

/// Insert or replace a marker-delimited block in a tmux conf file.
/// `body` should not include the marker lines — they're added here.
/// The whole block is written as one chunk so re-runs are idempotent
/// and the user can safely hand-edit content outside the markers.
pub fn upsert_block(
    path: &Path,
    begin_marker: &str,
    end_marker: &str,
    body: &str,
) -> Result<UpsertOutcome> {
    let existing = read_or_empty(path)?;
    let new_block = format_block(begin_marker, end_marker, body);
    let merged = merge_block(&existing, begin_marker, end_marker, &new_block)?;
    if merged == existing {
        return Ok(UpsertOutcome::Unchanged);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    atomic_write(path, &merged)?;
    Ok(if existing.contains(begin_marker) {
        UpsertOutcome::Updated
    } else {
        UpsertOutcome::Added
    })
}

/// Remove a marker-delimited block. Returns true iff the block existed
/// and was removed; false if there was nothing to remove.
pub fn remove_block(path: &Path, begin_marker: &str, end_marker: &str) -> Result<bool> {
    let existing = read_or_empty(path)?;
    if !existing.contains(begin_marker) {
        return Ok(false);
    }
    let Some(stripped) = strip_block(&existing, begin_marker, end_marker)? else {
        return Ok(false);
    };
    atomic_write(path, &stripped)?;
    Ok(true)
}

fn read_or_empty(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn format_block(begin: &str, end: &str, body: &str) -> String {
    let body = body.trim_end_matches('\n');
    format!("{begin}\n{body}\n{end}\n")
}

fn merge_block(
    existing: &str,
    begin_marker: &str,
    end_marker: &str,
    new_block: &str,
) -> Result<String> {
    let begin = existing.find(begin_marker);
    let end = existing.find(end_marker);
    match (begin, end) {
        (Some(b), Some(e)) if e > b => {
            let end_eol = existing[e..]
                .find('\n')
                .map(|n| e + n + 1)
                .unwrap_or(existing.len());
            let mut out = String::with_capacity(existing.len() + new_block.len());
            out.push_str(&existing[..b]);
            out.push_str(new_block);
            out.push_str(&existing[end_eol..]);
            Ok(out)
        }
        (Some(_), Some(_)) => bail!("markers `{begin_marker}` / `{end_marker}` are in the wrong order in the file — please fix it by hand"),
        (Some(_), None) | (None, Some(_)) => bail!("only one of the markers `{begin_marker}` / `{end_marker}` is present — please fix it by hand"),
        (None, None) => {
            let sep = if existing.is_empty() || existing.ends_with("\n\n") {
                ""
            } else if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            let mut out = String::with_capacity(existing.len() + sep.len() + new_block.len());
            out.push_str(existing);
            out.push_str(sep);
            out.push_str(new_block);
            Ok(out)
        }
    }
}

fn strip_block(existing: &str, begin_marker: &str, end_marker: &str) -> Result<Option<String>> {
    let begin = existing.find(begin_marker);
    let end = existing.find(end_marker);
    match (begin, end) {
        (None, None) => Ok(None),
        (Some(b), Some(e)) if e > b => {
            let end_eol = existing[e..]
                .find('\n')
                .map(|n| e + n + 1)
                .unwrap_or(existing.len());
            let before = existing[..b].trim_end_matches('\n');
            let after = existing[end_eol..].trim_start_matches('\n');
            let mut out = String::with_capacity(existing.len());
            out.push_str(before);
            if !before.is_empty() && !after.is_empty() {
                out.push('\n');
            }
            out.push_str(after);
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            Ok(Some(out))
        }
        _ => bail!("markers `{begin_marker}` / `{end_marker}` are malformed in the file — please fix it by hand"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEG: &str = "# >>> testblock >>>";
    const END: &str = "# <<< testblock <<<";

    #[test]
    fn format_block_has_markers_around_body() {
        let b = format_block(BEG, END, "set -g mouse on");
        assert_eq!(
            b,
            "# >>> testblock >>>\nset -g mouse on\n# <<< testblock <<<\n"
        );
    }

    #[test]
    fn merge_appends_when_missing() {
        let merged =
            merge_block("set -g status on\n", BEG, END, &format_block(BEG, END, "x")).unwrap();
        assert!(merged.contains("set -g status on"));
        assert!(merged.contains("# >>> testblock >>>"));
        assert!(merged.contains("# <<< testblock <<<"));
    }

    #[test]
    fn merge_replaces_existing_block_in_place() {
        let v1 = merge_block("a\n", BEG, END, &format_block(BEG, END, "old")).unwrap();
        let v2 = merge_block(&v1, BEG, END, &format_block(BEG, END, "new")).unwrap();
        assert!(v2.contains("new"));
        assert!(!v2.contains("old"));
        assert!(v2.contains("a"));
        assert_eq!(v2.matches(BEG).count(), 1);
    }

    #[test]
    fn strip_removes_block_and_preserves_other_content() {
        let v1 = merge_block("a\n", BEG, END, &format_block(BEG, END, "x")).unwrap();
        let stripped = strip_block(&v1, BEG, END).unwrap().unwrap();
        assert_eq!(stripped, "a\n");
    }

    #[test]
    fn multiple_named_blocks_coexist() {
        // Verifies the install case: keybind + status + watch hook can
        // all live in the same file with their own markers.
        let beg2 = "# >>> other >>>";
        let end2 = "# <<< other <<<";
        let v1 = merge_block("a\n", BEG, END, &format_block(BEG, END, "block-1")).unwrap();
        let v2 = merge_block(&v1, beg2, end2, &format_block(beg2, end2, "block-2")).unwrap();
        assert!(v2.contains("block-1"));
        assert!(v2.contains("block-2"));
        assert!(v2.contains(BEG));
        assert!(v2.contains(beg2));
        // Updating one block doesn't disturb the other.
        let v3 = merge_block(&v2, BEG, END, &format_block(BEG, END, "block-1-v2")).unwrap();
        assert!(v3.contains("block-1-v2"));
        assert!(!v3.contains("block-1\n"));
        assert!(v3.contains("block-2"));
    }
}
