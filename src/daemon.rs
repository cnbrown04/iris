//! The daemon: polls the repository and pops the desktop banners.
//!
//! New waiters announce at once; a banner the owner ignored re-pings after
//! the re-ping interval (same notification id, so it replaces in place and
//! never stacks). Runs this process has announced stay watched until they
//! complete: the conclusion lands as a second banner ("the result ping") and
//! the watch entry is deleted. A vanished run deletes its entry too.

use std::thread::sleep;
use std::time::Duration;

use crate::config::Config;
use crate::github::Client;
use crate::notify;
use crate::state::{self, State};
use crate::text::truncate;

pub fn run(cfg: &Config, once: bool) {
    let client = Client::new(&cfg.repo);
    let s = State::new(&cfg.cache);
    log(
        format!(
            "watching {} every {}s (re-ping after {}s)",
            cfg.repo, cfg.poll_secs, cfg.reping_secs
        )
        .as_str(),
    );
    loop {
        if let Err(e) = tick(&client, &s, cfg) {
            log(format!("tick failed: {e}").as_str());
        }
        if once {
            return;
        }
        sleep(Duration::from_secs(cfg.poll_secs));
    }
}

fn tick(client: &Client, s: &State, cfg: &Config) -> Result<(), String> {
    let waiting = client
        .waiting()
        .map_err(|e| format!("the list of waiting runs: {e}"))?;

    // 1. Announce the waiters.
    if !waiting.is_empty() {
        let mut body = String::new();
        for run in &waiting {
            let envs = client.pending(run.id).unwrap_or_default();
            let names = envs
                .iter()
                .filter(|e| e.can_approve)
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let title = truncate(run.title(), cfg.title_max);
            if names.is_empty() {
                body.push_str(&format!("- {} : {title}\n", run.name()));
            } else {
                body.push_str(&format!("- {} ({names}) : {title}\n", run.name()));
            }
            s.watch_set(run.id, "waiting", run.name(), run.title());
        }
        if !s.fresh("waiting", cfg.reping_secs) {
            body.pop(); // the last newline
            let n = waiting.len();
            let summary = if n == 1 {
                "1 deployment waiting for your approval".to_string()
            } else {
                format!("{n} deployments waiting for your approval")
            };
            let replace = s.stamp("waiting").map(|(_, id)| id).unwrap_or(0);
            match notify::show(
                &format!("iris: {summary}"),
                &body,
                replace,
                &cfg.runs_url,
                cfg.notify_ms,
            ) {
                Ok(id) => s.stamp_write("waiting", state::now(), id),
                Err(e) => log(format!("banner failed: {e}").as_str()),
            }
        }
    }

    // 2. Runs that left the waiting list: promote, or ping the result once.
    for (id, phase) in s.watch_list() {
        if waiting.iter().any(|r| r.id == id) {
            continue;
        }
        let run = match client.run(id) {
            Ok(run) => run,
            Err(_) => {
                s.watch_del(id);
                continue;
            }
        };
        if run.status.as_deref() == Some("completed") {
            let message = match run.conclusion() {
                "success" => format!("✔ {} finished: success", run.name()),
                "failure" | "timed_out" | "startup_failure" => {
                    format!("✘ {} finished: {}", run.name(), run.conclusion())
                }
                "cancelled" => format!("⊘ {} was cancelled", run.name()),
                other => format!("{} finished: {other}", run.name()),
            };
            let title = truncate(run.title(), cfg.title_max);
            let body = if title.is_empty() {
                message.clone()
            } else {
                format!("{message} · {title}")
            };
            let url = run
                .html_url
                .clone()
                .unwrap_or_else(|| run_url(cfg, id));
            if let Err(e) = notify::show("iris: result", &body, 0, &url, cfg.notify_ms) {
                log(format!("result banner failed: {e}").as_str());
            }
            s.watch_del(id);
        } else if phase != "running" {
            s.watch_set(id, "running", run.name(), run.title());
        }
    }
    Ok(())
}

fn run_url(cfg: &Config, id: u64) -> String {
    format!("https://github.com/{}/actions/runs/{id}", cfg.repo)
}

fn log(text: &str) {
    let stamp = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("iris"));
    eprintln!("{stamp} {text}");
}
