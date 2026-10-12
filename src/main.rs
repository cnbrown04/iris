//! iris: GitHub Actions deployment approvals on the desktop.
//!
//! - `iris` / `iris tui` opens the lazygit-style approval TUI.
//! - `iris daemon` polls and pops banners (the systemd user service).
//! - `iris list` prints the waiting runs.
//! - `iris notify-test` pops one informational test banner.
//!
//! The banner never approves; approving happens in the TUI, one POST per
//! run that reviews its pending environments in a single API call.

mod config;
mod daemon;
mod github;
mod list;
mod notify;
mod state;
mod text;
mod tui;

use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(
    name = "iris",
    version,
    about = "GitHub Actions deployment approvals: desktop watcher and workflow TUI",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum Command {
    /// The lazygit-style approval TUI (the default with no subcommand)
    Tui,
    /// Poll forever and pop the desktop banners (the systemd mode)
    Daemon {
        /// Run one poll cycle and exit
        #[arg(long)]
        once: bool,
    },
    /// Print the runs waiting for approval
    List,
    /// Pop one informational test banner
    NotifyTest,
}

fn main() {
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match cli.cmd.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(&cfg),
        Command::Daemon { once } => daemon::run(&cfg, once),
        Command::List => list::run(&cfg),
        Command::NotifyTest => {
            match notify::show(
                "iris: test banner (informational)",
                "- infrastructure (atlas): example. The real banner lists the waiting runs.\nApprove in the TUI: iris",
                0,
                &cfg.runs_url,
                cfg.notify_ms,
            ) {
                Ok(id) => eprintln!("notify-test: banner shown (id {id}, informational only)"),
                Err(e) => {
                    eprintln!("notify-test failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}
