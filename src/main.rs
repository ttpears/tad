use clap::Parser;
use owo_colors::OwoColorize;

mod cli;
mod config;
mod dashboard;
mod groups;
mod sessions;
mod theme;
mod tmux;

fn main() {
    // Restore default SIGPIPE so that piping into `head`/`less`/etc. exits
    // cleanly instead of panicking on the first failed write after the
    // reader closed its end.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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
