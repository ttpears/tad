//! `tad install` — one-shot setup that writes the three tmux conf hooks
//! tad needs (popup keybind, `#(tad status)` segment, `tad watch`
//! startup) so users don't have to wire each one by hand.
//!
//! Each hook is a marker-delimited block (see [`crate::tmux_conf`]),
//! so re-running updates in place, uninstalling removes only the
//! managed blocks, and any unrelated config in the same file is
//! preserved. The blocks coexist alongside each other and alongside
//! anything else the user has.

use anyhow::Result;
use std::io::{self, Write};
use std::path::Path;

use crate::tmux_conf::{self, UpsertOutcome};

const KEYBIND_BEG: &str = "# >>> tad tmux-keybind >>>";
const KEYBIND_END: &str = "# <<< tad tmux-keybind <<<";
const STATUS_BEG: &str = "# >>> tad status segment >>>";
const STATUS_END: &str = "# <<< tad status segment <<<";
const WATCH_BEG: &str = "# >>> tad watch startup >>>";
const WATCH_END: &str = "# <<< tad watch startup <<<";

#[derive(Debug, Clone)]
pub struct InstallOpts {
    pub yes: bool,
    pub conf_path: Option<std::path::PathBuf>,
    pub key: char,
    pub width: String,
    pub height: String,
    pub status_interval: u64,
}

impl Default for InstallOpts {
    fn default() -> Self {
        Self {
            yes: false,
            conf_path: None,
            key: 'D',
            width: "80%".into(),
            height: "80%".into(),
            status_interval: 5,
        }
    }
}

pub fn run(opts: InstallOpts) -> Result<i32> {
    let was_overridden = opts.conf_path.is_some();
    let path = tmux_conf::resolve_path(opts.conf_path.as_deref());
    println!("tad install: target tmux config → {}", path.display());
    println!();
    println!("Will write three marker-delimited blocks (each idempotent,");
    println!("each removable by `tad install --uninstall` or by deleting");
    println!("between its markers):");
    println!();
    println!(
        "  1. popup keybind     prefix + {}  →  tad dashboard popup",
        opts.key
    );
    println!("  2. status segment    #(tad status) in status-right");
    println!("  3. watch startup     `tad watch` on tmux session-created");
    println!();

    let mut wrote_any = false;
    wrote_any |= install_keybind(&path, &opts)?;
    wrote_any |= install_status(&path, &opts)?;
    wrote_any |= install_watch_hook(&path, &opts)?;

    if !wrote_any {
        println!("nothing to change — your tmux conf is already set up");
        return Ok(0);
    }
    if tmux_conf::try_reload(&path, was_overridden) {
        println!("✓ Reloaded tmux config");
    } else {
        println!(
            "  (run `tmux source-file {}` or restart tmux to apply)",
            path.display()
        );
    }
    println!();
    println!("Done. Quick check:");
    println!("  prefix + {}    pops the dashboard", opts.key);
    println!("  status-right   should show `claude: N` once an agent is running");
    println!("  tad status     prints the same string the segment renders");
    Ok(0)
}

pub fn uninstall(conf_path: Option<&Path>) -> Result<i32> {
    let was_overridden = conf_path.is_some();
    let path = tmux_conf::resolve_path(conf_path);
    if !path.exists() {
        println!("nothing to remove: {} doesn't exist", path.display());
        return Ok(0);
    }
    let mut removed_any = false;
    for (label, beg, end) in [
        ("watch startup", WATCH_BEG, WATCH_END),
        ("status segment", STATUS_BEG, STATUS_END),
        ("popup keybind", KEYBIND_BEG, KEYBIND_END),
    ] {
        if tmux_conf::remove_block(&path, beg, end)? {
            println!("✓ Removed {label} block");
            removed_any = true;
        }
    }
    if !removed_any {
        println!("no tad-managed blocks found in {}", path.display());
    } else if tmux_conf::try_reload(&path, was_overridden) {
        println!("✓ Reloaded tmux config");
    }
    Ok(0)
}

fn install_keybind(path: &Path, opts: &InstallOpts) -> Result<bool> {
    let body = format!(
        "# Managed by `tad install`. Re-run to update; remove with\n\
         # `tad install --uninstall`.\n\
         bind-key {key} display-popup -E -w {width} -h {height} 'tad'",
        key = opts.key,
        width = opts.width,
        height = opts.height,
    );
    apply(
        path,
        "popup keybind",
        KEYBIND_BEG,
        KEYBIND_END,
        &body,
        opts.yes,
    )
}

fn install_status(path: &Path, opts: &InstallOpts) -> Result<bool> {
    // `set -g` last-write-wins; acceptable here because this is a
    // managed block and re-running tad install overwrites itself.
    let body = format!(
        "# Managed by `tad install`. Re-run to update; remove with\n\
         # `tad install --uninstall`.\n\
         set -g status-interval {interval}\n\
         set -ga status-right '#[fg=cyan]#(tad status)#[default] '",
        interval = opts.status_interval,
    );
    apply(
        path,
        "status segment",
        STATUS_BEG,
        STATUS_END,
        &body,
        opts.yes,
    )
}

fn install_watch_hook(path: &Path, opts: &InstallOpts) -> Result<bool> {
    // `run-shell -b` runs in the background so tmux's session-created
    // event isn't blocked on `tad watch` starting. The pgrep guard
    // means a re-source of the conf doesn't spawn a duplicate (the
    // pidfile guard inside `tad watch` is a second layer of safety).
    let body = "# Managed by `tad install`. Re-run to update; remove with\n\
                # `tad install --uninstall`.\n\
                set-hook -g session-created 'run-shell -b \"pgrep -x tad >/dev/null || tad watch &\"'";
    apply(
        path,
        "watch startup hook",
        WATCH_BEG,
        WATCH_END,
        body,
        opts.yes,
    )
}

fn apply(path: &Path, label: &str, beg: &str, end: &str, body: &str, yes: bool) -> Result<bool> {
    if !yes
        && !confirm(&format!(
            "write {label} block to {}? [Y/n] ",
            path.display()
        ))
    {
        println!("  skipped {label}");
        return Ok(false);
    }
    match tmux_conf::upsert_block(path, beg, end, body)? {
        UpsertOutcome::Added => {
            println!("✓ Added {label} block");
            Ok(true)
        }
        UpsertOutcome::Updated => {
            println!("✓ Updated {label} block");
            Ok(true)
        }
        UpsertOutcome::Unchanged => {
            println!("✓ {label} already up to date");
            Ok(false)
        }
    }
}

fn confirm(prompt: &str) -> bool {
    if std::env::var_os("TAD_INSTALL_NONINTERACTIVE").is_some() {
        return true;
    }
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut buf = String::new();
    if io::stdin().read_line(&mut buf).is_err() {
        return false;
    }
    let answer = buf.trim().to_ascii_lowercase();
    // Y / y / empty (just hit Enter) → yes; everything else → no.
    answer.is_empty() || answer == "y" || answer == "yes"
}
