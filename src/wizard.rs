//! First-launch wizard and `tad config` editor.
//!
//! Discovery is local-only: scans files on this machine, never the network.

use anyhow::Result;

pub mod ui;

/// `tad config` entry: the groups editor is opt-in. If groups already exist it
/// opens Edit mode; otherwise it starts adding a group immediately. The
/// dashboard no longer launches this automatically — bare `tad` goes straight
/// to the TUI.
pub fn run_config() -> Result<i32> {
    ui::run()?;
    Ok(0)
}
