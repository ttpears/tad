//! CLI parsing and top-level dispatch.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::{agents, dashboard, groups, sessions, tmux_keybind};

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
    /// One-line tmux status-line segment summarising running Claude Code
    /// agents across all panes. Designed for `#(tad status)` in tmux.conf
    /// status-right. Prints nothing when no agents are running.
    ///
    /// Format: `claude: N` if all agents are active; `claude: A/N` if
    /// some are idle (`A` = active, `N` = total). Override the
    /// "active" threshold (seconds since the last transcript write) with
    /// `--active-secs` (default: 30).
    Status {
        /// Mtime within this many seconds = "active". Default 30s.
        #[arg(long, default_value_t = 30)]
        active_secs: u64,
    },
    /// Print or install a tmux popup keybinding that opens the dashboard.
    /// Default key is `D` (uppercase — lowercase `d` is tmux detach).
    ///
    /// Without --conf-path, the target file is auto-detected:
    /// ~/.tmux.conf.local, ~/.tmux.local.conf, $XDG_CONFIG_HOME/tmux/tmux.conf,
    /// then ~/.tmux.conf (created if missing). Override with --conf-path or
    /// the TAD_TMUX_CONF env var. Edits stay inside a marker-delimited
    /// managed block so unrelated config is preserved.
    TmuxKeybind {
        /// Write the binding to the resolved tmux config (and reload tmux
        /// if running).
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        /// Remove the managed tad keybinding block from the resolved tmux
        /// config.
        #[arg(long)]
        uninstall: bool,
        /// Key to bind after the tmux prefix.
        #[arg(short, long, default_value_t = 'D')]
        key: char,
        /// Popup width (percent or columns; passed to tmux display-popup -w).
        #[arg(long, default_value = "80%")]
        width: String,
        /// Popup height (percent or rows; passed to tmux display-popup -h).
        #[arg(long, default_value = "80%")]
        height: String,
        /// Explicit tmux config file to read/write. Overrides auto-detection
        /// and $TAD_TMUX_CONF.
        #[arg(long, value_name = "PATH")]
        conf_path: Option<std::path::PathBuf>,
    },
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
            eprintln!(
                "tad: dashboard unavailable ({:#}); falling back to picker",
                e
            );
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
        } => match (name, layout) {
            (Some(n), Some(l)) if !hosts.is_empty() => groups::add(&n, &l, &hosts),
            (None, None) => groups::add_interactive(),
            _ => bail!(
                "usage: tad groups-add NAME LAYOUT HOST [HOST...]  (or no args for the wizard)"
            ),
        },
        Cmd::GroupsRm { name } => groups::remove(&name),
        Cmd::GroupsEdit => groups::edit(),
        Cmd::Config => crate::wizard::run_config(),
        Cmd::Status { active_secs } => {
            print_status(active_secs);
            Ok(0)
        }
        Cmd::TmuxKeybind {
            install,
            uninstall,
            key,
            width,
            height,
            conf_path,
        } => {
            let conf = conf_path.as_deref();
            if uninstall {
                tmux_keybind::uninstall(conf)
            } else if install {
                tmux_keybind::install(key, &width, &height, conf)
            } else {
                tmux_keybind::print(key, &width, &height, conf);
                Ok(0)
            }
        }
    }
}

/// Render the status-line segment to stdout. Empty when no agents — tmux
/// happily renders an empty `#()` segment as nothing, so the user's
/// status-line stays clean when no Claude Code is running.
fn print_status(active_secs: u64) {
    let agents = agents::scan();
    if agents.is_empty() {
        return;
    }
    let c = agents::counts(&agents, std::time::Duration::from_secs(active_secs));
    if c.idle == 0 {
        print!("claude: {}", c.total);
    } else if c.active == 0 {
        print!("claude: {} idle", c.total);
    } else {
        print!("claude: {}/{}", c.active, c.total);
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
