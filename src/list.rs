//! `iris list`: the waiting runs as plain text.

use crate::config::Config;
use crate::github::Client;
use crate::text::{rel_age, truncate};

pub fn run(cfg: &Config) {
    let client = Client::new(&cfg.repo);
    let waiting = client.waiting().unwrap_or_default();
    if waiting.is_empty() {
        println!("No deployments are waiting for approval.");
        return;
    }
    println!(
        "Waiting deployments on {} ({}):\n",
        cfg.repo,
        waiting.len()
    );
    for (i, run) in waiting.iter().enumerate() {
        let envs = client
            .pending(run.id)
            .unwrap_or_default()
            .iter()
            .map(|e| {
                if e.can_approve {
                    e.name.clone()
                } else {
                    format!("{} (not you)", e.name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let envs = if envs.is_empty() {
            "-".to_string()
        } else {
            envs
        };
        let title = truncate(run.title(), cfg.title_max);
        let age = rel_age(run.created_at.as_deref());
        println!(
            "  {}) {:<15} {:<11} {:>5} {}",
            i + 1,
            run.name(),
            envs,
            age,
            title
        );
    }
    println!();
    println!("Open: {}", cfg.runs_url);
    println!("Approve in the TUI: iris");
}
