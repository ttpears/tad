//! CLI parsing and top-level dispatch.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::{dashboard, groups, sessions};

#[derive(Parser, Debug)]
#[command(
    name = "tad",
    version,
    about = "Tmux session and group manager with fzf-powered dashboard",
    long_about = None,
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Open a group of hosts (one window per host, or split panes per layout).
    /// With HOST, drill into a single host from the group.
    #[arg(short = 'g', value_name = "GROUP", num_args = 1..=2)]
    pub group: Option<Vec<String>>,

    /// Free-form positional: attach/create a session by name.
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
    GroupHosts {
        group: String,
    },
    /// Add a group.
    GroupsAdd {
        name: String,
        /// One of: panes | synced-panes | windows | browse
        layout: String,
        /// Hosts (FQDN or short).
        hosts: Vec<String>,
    },
    /// Remove a group.
    GroupsRm { name: String },
    /// Open the groups file in $EDITOR.
    GroupsEdit,

    // Internal flags consumed by the fzf dashboard.
    #[command(hide = true)]
    DashSource { view: String },
    #[command(hide = true)]
    DashCycle,
    #[command(hide = true)]
    DashPrompt,
    #[command(hide = true)]
    DashHeader,
    #[command(hide = true)]
    DashState,
    #[command(hide = true)]
    DashPreview { view: String, name: String },
    #[command(hide = true)]
    DashKill { view: String, name: String },
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
    // No args → dashboard, with fallback to numeric picker.
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
        Cmd::GroupsAdd {
            name,
            layout,
            hosts,
        } => {
            if hosts.is_empty() {
                bail!("at least one host required");
            }
            groups::add(&name, &layout, &hosts)
        }
        Cmd::GroupsRm { name } => groups::remove(&name),
        Cmd::GroupsEdit => groups::edit(),

        Cmd::DashSource { view } => {
            dashboard::source(&view)?;
            Ok(0)
        }
        Cmd::DashCycle => {
            dashboard::cycle()?;
            Ok(0)
        }
        Cmd::DashPrompt => {
            dashboard::prompt();
            Ok(0)
        }
        Cmd::DashHeader => {
            dashboard::header();
            Ok(0)
        }
        Cmd::DashState => {
            println!("{}", dashboard::read_state());
            Ok(0)
        }
        Cmd::DashPreview { view, name } => {
            dashboard::preview(&view, &name)?;
            Ok(0)
        }
        Cmd::DashKill { view, name } => {
            dashboard::kill(&view, &name);
            Ok(0)
        }
    }
}
