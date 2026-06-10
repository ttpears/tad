use clap::Parser;
use owo_colors::OwoColorize;

mod agents;
mod cli;
mod config;
mod dashboard;
mod discovery;
mod doctor;
mod groups;
mod install;
mod notify;
mod proc_util;
mod provider;
mod sessions;
mod snooze;
mod theme;
mod tmux;
mod tmux_conf;
mod tmux_keybind;
mod transcript;
mod ui_config;
mod watch;
mod wizard;

fn main() {
    // Restore default SIGPIPE so that piping into `head`/`less`/etc. exits
    // cleanly instead of panicking on the first failed write after the
    // reader closed its end.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // Pre-v0.10 layouts kept groups in ~/.config/tad/groups.yaml. Fold
    // them into the unified config.yaml on first startup; idempotent.
    config::migrate_if_needed();

    let cli = cli::Cli::parse();
    let rc = match cli::dispatch(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {:#}", "error:".red().bold(), e);
            1
        }
    };
    std::process::exit(rc);
}
