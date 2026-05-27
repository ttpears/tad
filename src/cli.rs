//! CLI parsing and top-level dispatch.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::{agents, dashboard, doctor, groups, install, sessions, tmux_keybind, watch};

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

    /// Open the dashboard on the Agents view with the given pane target
    /// preselected. `tad watch` uses this to point you at the agent that
    /// just went idle.
    #[arg(long, value_name = "SESSION:WINDOW.PANE")]
    pub select_agent: Option<String>,

    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List sessions (state<TAB>name:description) for shell completion.
    #[command(hide = true)]
    Complete,

    /// Manage groups: list, show hosts, add, remove, edit.
    Groups {
        #[command(subcommand)]
        sub: Option<GroupsCmd>,
    },

    /// Open the wizard / editor. First launch when no config exists,
    /// otherwise opens edit mode with re-run-imports access.
    Config,

    /// One-line tmux status-line segment summarising running Claude Code
    /// agents across all panes. Designed for `#(tad status)` in tmux.conf
    /// status-right. Prints nothing when no agents are running.
    ///
    /// Format: `agents: N` if all are active; `agents: A/N` if some
    /// are idle (`A` = active, `N` = total); appends `· M waiting`
    /// when any agents have transitioned to "awaiting input" recently.
    /// Override the "active" threshold (seconds since the last
    /// transcript write) with `--active-secs` (default: 30).
    Status {
        /// Mtime within this many seconds = "active". Default 30s.
        #[arg(long, default_value_t = 30)]
        active_secs: u64,
    },

    /// Diagnose the half-installed / silently-broken states the cockpit
    /// can land in: missing tools, wrong tmux version, stale pidfile,
    /// missing tmux conf blocks, legacy ui.auto_popup* keys in config,
    /// leftover pre-v0.10 migrated files, etc. Pure-diagnose for now —
    /// prints findings + suggested fixes.
    Doctor,

    /// One-shot setup: write the four tmux conf blocks tad needs
    /// (popup keybind + `#(tad status)` status segment + `tad watch`
    /// session-created hook + per-window `@tad-attn` marker append) so
    /// users don't have to wire each one individually. Each block
    /// lives behind its own marker pair and is idempotent — re-running
    /// updates, `--uninstall` removes. Use `--no-window-marker` to skip
    /// the window-status block if you already customize it heavily.
    Install {
        /// Skip per-block confirmation prompts.
        #[arg(short, long)]
        yes: bool,
        /// Remove the tad-managed blocks instead of installing them.
        #[arg(long, conflicts_with = "yes")]
        uninstall: bool,
        /// Explicit tmux config file. Default: same auto-detection as
        /// `tad tmux-keybind` (~/.tmux.conf.local, etc.).
        #[arg(long, value_name = "PATH")]
        conf_path: Option<std::path::PathBuf>,
        /// Popup keybind (default `D` — uppercase, since lowercase `d`
        /// is tmux's built-in detach).
        #[arg(short, long, default_value_t = 'D')]
        key: char,
        /// Popup width.
        #[arg(long, default_value = "80%")]
        width: String,
        /// Popup height.
        #[arg(long, default_value = "80%")]
        height: String,
        /// tmux status-interval (seconds). 5 keeps the segment fresh
        /// without thrashing.
        #[arg(long, default_value_t = 5)]
        status_interval: u64,
        /// Skip the per-window attention-marker block. By default
        /// `tad install` appends a `!` to window-status-format when an
        /// agent in that window needs your attention; this flag opts
        /// out for users with heavily customised tmux status lines.
        #[arg(long)]
        no_window_marker: bool,
    },

    /// Long-running watcher: poll all tmux panes for Claude Code agents
    /// and keep the per-window `@tad-attn` tmux user-variable in sync
    /// with each agent's attention state. Rendering is passive: the
    /// `tad install` window-status block appends a `!` to the window
    /// when its `@tad-attn` is set, and the `#(tad status)` segment
    /// shows the aggregate count. No popups.
    ///
    /// Run once per user session — `tad install` writes a tmux
    /// session-created hook that does so. A pidfile guard makes a
    /// second `tad watch` exit immediately.
    Watch {
        /// Poll interval in seconds. Default 5.
        #[arg(long, default_value_t = 5)]
        interval_secs: u64,
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

    // ---- legacy renamed commands ----
    // These are hidden from --help and exist only to give a friendly
    // "renamed in v0.10" hint when someone's script or muscle memory
    // still uses the old name. Each accepts any positional args so
    // `tad groups-add foo panes bar` still parses cleanly before we
    // bail with the rename message.
    #[command(
        name = "group-hosts",
        hide = true,
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    LegacyGroupHosts {
        #[arg(allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    #[command(
        name = "groups-add",
        hide = true,
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    LegacyGroupsAdd {
        #[arg(allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    #[command(
        name = "groups-rm",
        hide = true,
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    LegacyGroupsRm {
        #[arg(allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    #[command(name = "groups-edit", hide = true)]
    LegacyGroupsEdit,
}

#[derive(Subcommand, Debug)]
pub enum GroupsCmd {
    /// List known groups (default when no subcommand is given).
    List,
    /// Print hosts in a group.
    Hosts { group: String },
    /// Add a group. With no positional args, launches the interactive wizard.
    Add {
        name: Option<String>,
        /// One of: panes | synced-panes | windows | browse
        layout: Option<String>,
        /// Hosts (FQDN or short).
        hosts: Vec<String>,
    },
    /// Remove a group.
    Rm { name: String },
    /// Open the groups file in $EDITOR.
    Edit,
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
    // No subcommand → TUI dashboard. `--select-agent <target>` opens the
    // Agents view with that row preselected (used by `tad watch`).
    let opts = dashboard::RunOpts {
        select_agent: cli.select_agent,
    };
    match dashboard::run_with(opts) {
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
        Cmd::Groups { sub } => run_groups(sub.unwrap_or(GroupsCmd::List)),
        Cmd::Config => crate::wizard::run_config(),
        Cmd::Status { active_secs } => {
            print_status(active_secs);
            Ok(0)
        }
        Cmd::Watch { interval_secs } => watch::run(interval_secs),
        Cmd::Doctor => doctor::run(),
        Cmd::Install {
            yes,
            uninstall,
            conf_path,
            key,
            width,
            height,
            status_interval,
            no_window_marker,
        } => {
            if uninstall {
                install::uninstall(conf_path.as_deref())
            } else {
                install::run(install::InstallOpts {
                    yes,
                    conf_path,
                    key,
                    width,
                    height,
                    status_interval,
                    no_window_marker,
                })
            }
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
        Cmd::LegacyGroupHosts { .. } => renamed("group-hosts", "groups hosts"),
        Cmd::LegacyGroupsAdd { .. } => renamed("groups-add", "groups add"),
        Cmd::LegacyGroupsRm { .. } => renamed("groups-rm", "groups rm"),
        Cmd::LegacyGroupsEdit => renamed("groups-edit", "groups edit"),
    }
}

fn run_groups(cmd: GroupsCmd) -> Result<i32> {
    match cmd {
        GroupsCmd::List => {
            groups::print_index()?;
            Ok(0)
        }
        GroupsCmd::Hosts { group } => {
            groups::print_hosts(&group)?;
            Ok(0)
        }
        GroupsCmd::Add {
            name,
            layout,
            hosts,
        } => match (name, layout) {
            (Some(n), Some(l)) if !hosts.is_empty() => groups::add(&n, &l, &hosts),
            (None, None) => groups::add_interactive(),
            _ => bail!(
                "usage: tad groups add NAME LAYOUT HOST [HOST...]  (or no args for the wizard)"
            ),
        },
        GroupsCmd::Rm { name } => groups::remove(&name),
        GroupsCmd::Edit => groups::edit(),
    }
}

fn renamed(old: &str, new: &str) -> Result<i32> {
    eprintln!("tad: `{old}` was renamed in v0.10 — use `tad {new}` instead");
    Ok(2)
}

/// Maximum length (chars) of the comma-joined waiting-names list
/// before we fall back to the count. Keeps the status segment from
/// hogging status-right when many agents are waiting.
const WAITING_NAMES_BUDGET: usize = 40;

/// Render the status-line segment to stdout. Empty when no agents — tmux
/// happily renders an empty `#()` segment as nothing, so the user's
/// status-line stays clean when no Claude Code is running.
fn print_status(active_secs: u64) {
    let agents = agents::scan();
    if agents.is_empty() {
        return;
    }
    let c = agents::counts(&agents, std::time::Duration::from_secs(active_secs));
    let ui = crate::ui_config::load();
    let now = std::time::SystemTime::now();
    // "Waiting" only counts AwaitingInput agents whose transcript was
    // written recently — agents that have been sitting idle at the
    // prompt for hours/days are technically "AwaitingInput" but the
    // user clearly walked away, and lighting up the status bar over
    // them teaches the user to ignore the signal.
    let waiting_names: Vec<String> = agents
        .iter()
        .filter(|a| {
            a.attention == crate::transcript::Attention::AwaitingInput
                && a.last_activity
                    .and_then(|t| now.duration_since(t).ok())
                    .map(|age| age <= ui.awaiting_freshness)
                    .unwrap_or(false)
        })
        .map(|a| a.session.clone())
        .collect();
    let base = if c.idle == 0 {
        format!("agents: {}", c.total)
    } else if c.active == 0 {
        format!("agents: {} idle", c.total)
    } else {
        format!("agents: {}/{}", c.active, c.total)
    };
    if let Some(tail) = format_waiting_tail(&waiting_names, WAITING_NAMES_BUDGET) {
        print!("{base} · {tail}");
    } else {
        print!("{base}");
    }
}

/// Build the `waiting: …` tail of the status segment. Returns None when
/// nothing is waiting (caller omits the separator). Returns the name
/// list when it fits in `budget` chars, otherwise the count.
fn format_waiting_tail(names: &[String], budget: usize) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let joined = names.join(", ");
    if joined.chars().count() <= budget {
        Some(format!("waiting: {joined}"))
    } else {
        Some(format!("waiting: {}", names.len()))
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

    #[test]
    fn waiting_tail_is_none_when_empty() {
        let v: Vec<String> = vec![];
        assert!(format_waiting_tail(&v, 40).is_none());
    }

    #[test]
    fn waiting_tail_lists_names_when_under_budget() {
        let v = vec!["foo".to_string(), "bar".to_string()];
        assert_eq!(
            format_waiting_tail(&v, 40).as_deref(),
            Some("waiting: foo, bar")
        );
    }

    #[test]
    fn waiting_tail_falls_back_to_count_over_budget() {
        // 5 names of 10 chars each = 50 chars before joining, way over 20.
        let v = vec![
            "aaaaaaaaaa".to_string(),
            "bbbbbbbbbb".to_string(),
            "cccccccccc".to_string(),
            "dddddddddd".to_string(),
            "eeeeeeeeee".to_string(),
        ];
        assert_eq!(format_waiting_tail(&v, 20).as_deref(), Some("waiting: 5"));
    }

    #[test]
    fn bare_groups_defaults_to_list() {
        let cli = Cli::try_parse_from(["tad", "groups"]).expect("parse");
        match cli.cmd {
            Some(Cmd::Groups { sub }) => assert!(sub.is_none()),
            other => panic!("expected Groups, got {:?}", other),
        }
    }

    #[test]
    fn groups_list_parses() {
        let cli = Cli::try_parse_from(["tad", "groups", "list"]).expect("parse");
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Groups {
                sub: Some(GroupsCmd::List)
            })
        ));
    }

    #[test]
    fn groups_hosts_takes_group_name() {
        let cli = Cli::try_parse_from(["tad", "groups", "hosts", "prod"]).expect("parse");
        match cli.cmd {
            Some(Cmd::Groups {
                sub: Some(GroupsCmd::Hosts { group }),
            }) => assert_eq!(group, "prod"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn groups_add_takes_name_layout_hosts() {
        let cli = Cli::try_parse_from(["tad", "groups", "add", "g1", "panes", "h1", "h2"])
            .expect("parse");
        match cli.cmd {
            Some(Cmd::Groups {
                sub:
                    Some(GroupsCmd::Add {
                        name,
                        layout,
                        hosts,
                    }),
            }) => {
                assert_eq!(name.as_deref(), Some("g1"));
                assert_eq!(layout.as_deref(), Some("panes"));
                assert_eq!(hosts, vec!["h1".to_string(), "h2".to_string()]);
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn groups_rm_takes_name() {
        let cli = Cli::try_parse_from(["tad", "groups", "rm", "g1"]).expect("parse");
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Groups {
                sub: Some(GroupsCmd::Rm { .. })
            })
        ));
    }

    #[test]
    fn groups_edit_parses() {
        let cli = Cli::try_parse_from(["tad", "groups", "edit"]).expect("parse");
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Groups {
                sub: Some(GroupsCmd::Edit)
            })
        ));
    }

    #[test]
    fn legacy_flat_commands_still_parse_so_we_can_print_rename_hints() {
        // These all used to exist as flat subcommands. They're hidden in
        // --help and dispatch to a "renamed to ..." message rather than
        // erroring with a generic "unknown subcommand."
        for argv in [
            &["tad", "group-hosts", "prod"][..],
            &["tad", "groups-add", "g", "panes", "h"][..],
            &["tad", "groups-rm", "g"][..],
            &["tad", "groups-edit"][..],
        ] {
            Cli::try_parse_from(argv.iter().copied())
                .unwrap_or_else(|e| panic!("expected {:?} to parse, got: {e}", argv));
        }
    }
}
