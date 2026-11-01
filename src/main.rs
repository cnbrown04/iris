//! iris: GitHub Actions deployment approvals on the desktop.
//!
//! - `iris` / `iris tui` opens the lazygit-style approval TUI.
//! - `iris daemon` polls and pops banners (the systemd user service).
//! - `iris list` prints the waiting runs.
//! - `iris notify-test` pops a test banner and reports which button was clicked.
//!
//! Each waiting run gets a banner with Approve / Ignore / Deny; Approve and
//! Deny review all of the run's environments in one API call, like the TUI.

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
    /// Pop a test banner with the buttons and report the click
    NotifyTest,
}

fn main() {
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match cli.cmd.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(&cfg),
        Command::Daemon { once } => daemon::run(&cfg, once),
        Command::List => list::run(&cfg),
        Command::NotifyTest => notify_test(&cfg),
    }
}

/// One banner with the real buttons. Approve / Ignore / Deny only print what
/// they would do; clicking the banner opens the Actions page for real.
fn notify_test(cfg: &Config) {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Err(e) = notify::listen(move |ev| {
        let _ = tx.send(ev);
    }) {
        eprintln!("notify-test: can't listen for clicks: {e}");
    }
    let banner = notify::Banner {
        summary: "iris: test banner",
        body: "infrastructure would wait here\nDeploys to atlas (the buttons are a dry run)",
        replace: 0,
        actions: &[
            (notify::DEFAULT, "Open"),
            ("approve", "Approve"),
            ("ignore", "Ignore"),
            ("deny", "Deny"),
        ],
        timeout_ms: 0,
        urgency: 1,
    };
    let id = match notify::show(&banner) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("notify-test failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("notify-test: banner {id} shown, waiting 60s for a click");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(notify::Event::Action { id: got, key }) if got == id => {
                if key == notify::DEFAULT {
                    eprintln!("notify-test: banner clicked, opening {}", cfg.runs_url);
                    notify::open_url(&cfg.runs_url, Some(id));
                } else {
                    eprintln!("notify-test: {key} clicked (dry run, nothing done)");
                }
                notify::close(id);
                return;
            }
            Ok(notify::Event::Closed { id: got, reason }) if got == id => {
                eprintln!("notify-test: banner closed (reason {reason})");
                return;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    notify::close(id);
    eprintln!("notify-test: no click");
}
