//! `tad tmux-keybind` — print or install a tmux popup binding for the dashboard.
//!
//! Delegates all the marker-block / atomic-write / path-resolve plumbing
//! to [`crate::tmux_conf`]; this module just owns the keybind-specific
//! block body and the CLI ergonomics around it. The popup opens tad
//! over the current pane and closes on exit, so the user is left
//! exactly where they were.

use anyhow::Result;
use std::path::Path;

use crate::tmux_conf::{self, UpsertOutcome};

const BEGIN_MARKER: &str = "# >>> tad tmux-keybind >>>";
const END_MARKER: &str = "# <<< tad tmux-keybind <<<";

fn block_body(key: char, width: &str, height: &str) -> String {
    format!(
        "# Managed by `tad tmux-keybind`. Re-run to update; remove with\n\
         # `tad tmux-keybind --uninstall`.\n\
         bind-key {key} display-popup -E -w {width} -h {height} 'tad'",
    )
}

pub fn print(key: char, width: &str, height: &str, conf_path: Option<&Path>) {
    let path = tmux_conf::resolve_path(conf_path);
    println!("# Recommended tad tmux keybinding:");
    println!("# (would be written to {})", path.display());
    println!();
    println!("{BEGIN_MARKER}");
    print!("{}", block_body(key, width, height));
    println!();
    println!("{END_MARKER}");
    println!();
    println!("# Apply without restarting tmux:");
    println!("#   tmux source-file {}", path.display());
    println!("#");
    println!("# Or let tad install it for you:");
    println!("#   tad tmux-keybind --install");
}

pub fn install(key: char, width: &str, height: &str, conf_path: Option<&Path>) -> Result<i32> {
    let path = tmux_conf::resolve_path(conf_path);
    let body = block_body(key, width, height);
    let outcome = tmux_conf::upsert_block(&path, BEGIN_MARKER, END_MARKER, &body)?;
    match outcome {
        UpsertOutcome::Unchanged => {
            println!("✓ tad keybindings already up to date in {}", path.display());
            return Ok(0);
        }
        UpsertOutcome::Added => println!("✓ Added tad keybindings in {}", path.display()),
        UpsertOutcome::Updated => println!("✓ Updated tad keybindings in {}", path.display()),
    }
    if tmux_conf::try_reload(&path, conf_path.is_some()) {
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
    let path = tmux_conf::resolve_path(conf_path);
    if !path.exists() {
        println!("nothing to remove: {} doesn't exist", path.display());
        return Ok(0);
    }
    if tmux_conf::remove_block(&path, BEGIN_MARKER, END_MARKER)? {
        println!("✓ Removed tad keybindings from {}", path.display());
        if tmux_conf::try_reload(&path, conf_path.is_some()) {
            println!("✓ Reloaded tmux config");
        }
    } else {
        println!("no tad keybindings block found in {}", path.display());
    }
    Ok(0)
}
