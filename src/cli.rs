//! CLI parsing and top-level dispatch.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::{dashboard, groups, sessions};

#[derive(Parser, Debug)]
#[command(
    name = "tad",
    version,
    about = "Tmux session and group manager with native TUI dashboard",
    long_about = None,
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Open a group of hosts (one window per host, or split panes per layout).
    /// With HOST, drill into a single host from the group.
    #[arg(short = 'g', value_name = "GROUP", num_args = 1..=2)]
    pub group: Option<Vec<String>>,

    /// Attach/create a session by name.
    pub session: Option<String>,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List sessions (state<TAB>name:description) for shell completion.
    Complete,
    /// List groups (name:description) for shell completion.
    Groups,
    /// Print hosts in a group.
    GroupHosts { group: String },
    /// Add a group. With no args, launches an interactive wizard.
    GroupsAdd {
        name: Option<String>,
        /// One of: panes | synced-panes | windows | browse
        layout: Option<String>,
        /// Hosts (FQDN or short).
        hosts: Vec<String>,
    },
    /// Remove a group.
    GroupsRm { name: String },
    /// Open the groups file in $EDITOR.
    GroupsEdit,
    /// Open the wizard / editor. First launch when no config exists,
    /// otherwise opens edit mode with re-run-imports access.
    Config,
}

pub fn dispatch(cli: Cli) -> Result<i32> {
    if let Some(cmd) = cli.cmd {
        return run_subcommand(cmd);
    }
    if let Some(group_args) = cli.group {
        let name = group_args
            .first()
            .ok_or_else(|| anyhow::anyhow!("usage: tad -g GROUP [HOST]"))?;
        let host = group_args.get(1).cloned();
        return groups::open(name, host.as_deref());
    }
    if let Some(name) = cli.session {
        return sessions::attach_or_create(&name);
    }
    // No args → TUI dashboard. Fall back to a numbered picker if the
    // terminal can't be controlled (non-TTY, weird env, etc.).
    match dashboard::run() {
        Ok(rc) => Ok(rc),
        Err(e) => {
            eprintln!("tad: dashboard unavailable ({:#}); falling back to picker", e);
            sessions::picker_fallback()
        }
    }
}

fn run_subcommand(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Complete => {
            sessions::print_completions()?;
            Ok(0)
        }
        Cmd::Groups => {
            groups::print_index()?;
            Ok(0)
        }
        Cmd::GroupHosts { group } => {
            groups::print_hosts(&group)?;
            Ok(0)
        }
        Cmd::GroupsAdd { name, layout, hosts } => {
            match (name, layout) {
                (Some(n), Some(l)) if !hosts.is_empty() => groups::add(&n, &l, &hosts),
                (None, None) => groups::add_interactive(),
                _ => bail!("usage: tad groups-add NAME LAYOUT HOST [HOST...]  (or no args for the wizard)"),
            }
        }
        Cmd::GroupsRm { name } => groups::remove(&name),
        Cmd::GroupsEdit => groups::edit(),
        Cmd::Config => crate::wizard::run_config(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_config_subcommand() {
        let cli = Cli::try_parse_from(["tad", "config"]).expect("parse");
        assert!(matches!(cli.cmd, Some(Cmd::Config)));
    }
}
