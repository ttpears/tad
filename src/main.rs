use clap::Parser;

mod cli;
mod config;
mod dashboard;
mod groups;
mod sessions;
mod tmux;

fn main() {
    let cli = cli::Cli::parse();
    let rc = match cli::dispatch(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tad: {:#}", e);
            1
        }
    };
    std::process::exit(rc);
}
