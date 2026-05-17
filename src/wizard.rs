//! First-launch wizard and `tad config` editor.
//!
//! Discovery is local-only: scans files on this machine, never the network.

use anyhow::Result;

pub mod discovery;
pub mod ui;

/// Bit-mask of which import sources to scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceSet {
    pub shell: bool,
    pub ssh_config: bool,
    pub known_hosts: bool,
    pub tmux_sessions: bool,
}

impl SourceSet {
    pub const ALL: Self = Self {
        shell: true,
        ssh_config: true,
        known_hosts: true,
        tmux_sessions: true,
    };

    pub const NONE: Self = Self {
        shell: false,
        ssh_config: false,
        known_hosts: false,
        tmux_sessions: false,
    };

    pub fn count(self) -> usize {
        self.shell as usize
            + self.ssh_config as usize
            + self.known_hosts as usize
            + self.tmux_sessions as usize
    }
}

/// First-launch entry: bare `tad` with no groups.yaml.
/// Pre-checks all sources, runs the wizard, returns Ok regardless of whether
/// the user wrote anything — caller falls through to the dashboard either way.
pub fn run_first_launch() -> Result<()> {
    ui::run(ui::Entry::FirstLaunch)
}

/// `tad config` entry: re-runnable. If config exists, opens Edit mode;
/// otherwise behaves like first launch.
pub fn run_config() -> Result<i32> {
    ui::run(ui::Entry::Config)?;
    Ok(0)
}
