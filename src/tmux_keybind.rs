//! `tad tmux-keybind` — print or install a tmux popup binding for the dashboard.
//!
//! The popup opens tad over the current pane and closes on exit, so the user
//! is left exactly where they were. Install writes a marker-delimited block
//! to `~/.tmux.conf` so re-running updates in place instead of duplicating.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const BEGIN_MARKER: &str = "# >>> tad tmux-keybind >>>";
const END_MARKER: &str = "# <<< tad tmux-keybind <<<";

fn block(key: char, width: &str, height: &str) -> String {
    format!(
        "{BEGIN_MARKER}\n\
         # Managed by `tad tmux-keybind`. Re-run to update; remove with\n\
         # `tad tmux-keybind --uninstall`.\n\
         bind-key {key} display-popup -E -w {width} -h {height} 'tad'\n\
         {END_MARKER}\n",
    )
}

/// Pick the tmux config file to modify, preferring user-managed local
/// overrides (Oh-My-Tmux / oh-my-tmux variants) so we never clobber a
/// framework's main `~/.tmux.conf`. Probe order:
///   1. `--conf-path` override (passed by caller)
///   2. `$TAD_TMUX_CONF` env var
///   3. `~/.tmux.conf.local`  (gpakosz/.tmux convention)
///   4. `~/.tmux.local.conf`  (alternate spelling some teams use)
///   5. `$XDG_CONFIG_HOME/tmux/tmux.conf` (if it exists)
///   6. `~/.tmux.conf` (classic; created if missing)
fn resolve_conf_path(override_path: Option<&Path>) -> PathBuf {
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

/// Replace an existing managed block, or append one if none exists.
fn merge_block(existing: &str, new_block: &str) -> Result<String> {
    let begin = existing.find(BEGIN_MARKER);
    let end = existing.find(END_MARKER);
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
        (Some(_), Some(_)) => bail!(
            "tmux.conf has tad-keybind markers in the wrong order — \
             please fix it by hand"
        ),
        (Some(_), None) | (None, Some(_)) => bail!(
            "tmux.conf has only one of the tad-keybind markers — \
             please fix it by hand"
        ),
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

fn strip_block(existing: &str) -> Result<Option<String>> {
    let begin = existing.find(BEGIN_MARKER);
    let end = existing.find(END_MARKER);
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
        _ => bail!("tmux.conf has malformed tad-keybind markers — please fix it by hand"),
    }
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tad-keybind.tmp");
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

/// Returns true if we're in a tmux client and the source-file succeeded.
fn try_reload_tmux(path: &Path) -> bool {
    if std::env::var_os("TMUX").is_none() {
        return false;
    }
    Command::new("tmux")
        .args(["source-file", &path.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn print(key: char, width: &str, height: &str, conf_path: Option<&Path>) {
    let path = resolve_conf_path(conf_path);
    println!("# Recommended tad tmux keybinding:");
    println!("# (would be written to {})", path.display());
    println!();
    print!("{}", block(key, width, height));
    println!();
    println!("# Apply without restarting tmux:");
    println!("#   tmux source-file {}", path.display());
    println!("#");
    println!("# Or let tad install it for you:");
    println!("#   tad tmux-keybind --install");
}

pub fn install(key: char, width: &str, height: &str, conf_path: Option<&Path>) -> Result<i32> {
    let path = resolve_conf_path(conf_path);
    let existing = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let new_block = block(key, width, height);
    let merged = merge_block(&existing, &new_block)?;
    if merged == existing {
        println!("✓ tad keybindings already up to date in {}", path.display());
        return Ok(0);
    }
    atomic_write(&path, &merged)?;
    let verb = if existing.contains(BEGIN_MARKER) {
        "Updated"
    } else {
        "Added"
    };
    println!("✓ {} tad keybindings in {}", verb, path.display());
    if try_reload_tmux(&path) {
        println!("✓ Reloaded tmux config");
    } else {
        println!(
            "  (run `tmux source-file {}` or restart tmux to apply)",
            path.display()
        );
    }
    println!();
    println!("  prefix + {key}  →  tad dashboard popup");
    Ok(0)
}

pub fn uninstall(conf_path: Option<&Path>) -> Result<i32> {
    let path = resolve_conf_path(conf_path);
    let existing = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("nothing to remove: {} doesn't exist", path.display());
            return Ok(0);
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let Some(new) = strip_block(&existing)? else {
        println!("no tad keybindings block found in {}", path.display());
        return Ok(0);
    };
    atomic_write(&path, &new)?;
    println!("✓ Removed tad keybindings from {}", path.display());
    if try_reload_tmux(&path) {
        println!("✓ Reloaded tmux config");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_includes_markers_and_bind() {
        let b = block('D', "80%", "80%");
        assert!(b.starts_with(BEGIN_MARKER));
        assert!(b.trim_end().ends_with(END_MARKER));
        assert!(b.contains("bind-key D display-popup -E -w 80% -h 80% 'tad'"));
    }

    #[test]
    fn merge_appends_block_when_missing() {
        let merged = merge_block("set -g mouse on\n", &block('D', "80%", "80%")).unwrap();
        assert!(merged.contains("set -g mouse on"));
        assert!(merged.contains("bind-key D display-popup"));
        assert_eq!(merged.matches(BEGIN_MARKER).count(), 1);
    }

    #[test]
    fn merge_replaces_existing_block() {
        let initial = merge_block("set -g mouse on\n", &block('D', "80%", "80%")).unwrap();
        let updated = merge_block(&initial, &block('T', "90%", "90%")).unwrap();
        assert!(!updated.contains("bind-key D display-popup"));
        assert!(updated.contains("bind-key T display-popup -E -w 90% -h 90%"));
        assert!(updated.contains("set -g mouse on"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
        assert_eq!(updated.matches(END_MARKER).count(), 1);
    }

    #[test]
    fn merge_into_empty_file() {
        let merged = merge_block("", &block('D', "80%", "80%")).unwrap();
        assert!(merged.starts_with(BEGIN_MARKER));
    }

    #[test]
    fn merge_preserves_trailing_content_after_block() {
        let initial = "set -g mouse on\n";
        let with_block = merge_block(initial, &block('D', "80%", "80%")).unwrap();
        // Simulate user adding content after our block.
        let appended = format!("{with_block}\nset -g status on\n");
        let updated = merge_block(&appended, &block('T', "90%", "90%")).unwrap();
        assert!(updated.contains("set -g mouse on"));
        assert!(updated.contains("set -g status on"));
        assert!(updated.contains("bind-key T display-popup"));
        assert!(!updated.contains("bind-key D display-popup"));
    }

    #[test]
    fn strip_removes_block_and_leaves_rest() {
        let initial = merge_block("set -g mouse on\n", &block('D', "80%", "80%")).unwrap();
        let stripped = strip_block(&initial).unwrap().unwrap();
        assert_eq!(stripped, "set -g mouse on\n");
    }

    #[test]
    fn strip_returns_none_when_no_block() {
        let s = "set -g mouse on\n";
        assert!(strip_block(s).unwrap().is_none());
    }

    #[test]
    fn strip_on_block_only_file_returns_empty() {
        let initial = merge_block("", &block('D', "80%", "80%")).unwrap();
        let stripped = strip_block(&initial).unwrap().unwrap();
        assert_eq!(stripped, "");
    }

    #[test]
    fn merge_rejects_only_one_marker() {
        let busted = format!("{BEGIN_MARKER}\nbind-key D ...\n");
        let err = merge_block(&busted, &block('D', "80%", "80%")).unwrap_err();
        assert!(err.to_string().contains("only one"));
    }

    #[test]
    fn resolve_prefers_explicit_override() {
        let explicit = PathBuf::from("/tmp/explicit-tmux.conf");
        assert_eq!(resolve_conf_path(Some(&explicit)), explicit);
    }

    #[test]
    fn resolve_prefers_env_var_over_home() {
        // Save + clear so the test is hermetic.
        let prev = std::env::var_os("TAD_TMUX_CONF");
        std::env::set_var("TAD_TMUX_CONF", "/tmp/from-env-tmux.conf");
        let got = resolve_conf_path(None);
        match prev {
            Some(v) => std::env::set_var("TAD_TMUX_CONF", v),
            None => std::env::remove_var("TAD_TMUX_CONF"),
        }
        assert_eq!(got, PathBuf::from("/tmp/from-env-tmux.conf"));
    }

    #[test]
    fn merge_rejects_reversed_markers() {
        let busted = format!("{END_MARKER}\nstuff\n{BEGIN_MARKER}\n");
        let err = merge_block(&busted, &block('D', "80%", "80%")).unwrap_err();
        assert!(err.to_string().contains("wrong order"));
    }
}
