//! The daemon: polls the repository and pops the desktop banners.
//!
//! Every run that waits for approval gets its own banner with Approve,
//! Ignore and Deny buttons; clicking the banner itself opens the run on
//! GitHub. The banner stays until it's clicked or closed. A banner closed
//! with its × re-pings after the re-ping interval (only once it's gone, so
//! banners never stack); Ignore stops the banners of that run for good.
//! Runs this process has announced stay watched until they complete: the
//! conclusion lands as a second banner ("the result ping") and the watch
//! entry is deleted. A vanished run deletes its entry too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use crate::config::Config;
use crate::github::Client;
use crate::notify::{self, Banner, Event};
use crate::state::{self, State};
use crate::text::truncate;

/// The comments GitHub keeps in the deployment history.
const APPROVE_COMMENT: &str = "approved with iris";
const REJECT_COMMENT: &str = "rejected with iris";

/// A run's approval banner that is on screen right now.
#[derive(Debug, Clone)]
struct Open {
    run_id: u64,
    workflow: String,
    url: String,
    /// The environments this user may review, and their names for messages.
    env_ids: Vec<u64>,
    env_names: String,
}

type OpenBanners = Arc<Mutex<HashMap<u32, Open>>>;

pub fn run(cfg: &Config, once: bool) {
    let client = Client::new(&cfg.repo);
    let s = State::new(&cfg.cache);
    let open: OpenBanners = Arc::new(Mutex::new(HashMap::new()));
    log(&format!(
        "watching {} every {}s (re-ping after {}s)",
        cfg.repo, cfg.poll_secs, cfg.reping_secs
    ));
    if !once {
        start_listener(cfg, &open);
    }
    loop {
        if let Err(e) = tick(&client, &s, cfg, &open) {
            log(&format!("tick failed: {e}"));
        }
        if once {
            return;
        }
        sleep(Duration::from_secs(cfg.poll_secs));
    }
}

fn tick(client: &Client, s: &State, cfg: &Config, open: &OpenBanners) -> Result<(), String> {
    let waiting = client
        .waiting()
        .map_err(|e| format!("the list of waiting runs: {e}"))?;

    // 1. One approval banner per waiting run.
    for run in &waiting {
        s.watch_set(run.id, "waiting", run.name(), run.title());
        if s.ignored(run.id) || open.lock().unwrap().values().any(|o| o.run_id == run.id) {
            continue;
        }
        let stamp = state::run_banner(run.id);
        if s.fresh(&stamp, cfg.reping_secs) {
            continue;
        }
        let envs = client.pending(run.id).unwrap_or_default();
        let reviewable: Vec<_> = envs.iter().filter(|e| e.can_approve).collect();
        let env_names = reviewable
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let title = truncate(run.title(), cfg.title_max);
        let summary = format!("iris: {} is waiting for approval", run.name());
        let body = match (env_names.is_empty(), title.is_empty()) {
            (true, _) => format!("{title}\nYou can't review this one yourself."),
            (false, true) => format!("Deploys to {env_names}"),
            (false, false) => format!("{title}\nDeploys to {env_names}"),
        };
        let actions: &[(&str, &str)] = if reviewable.is_empty() {
            &[(notify::DEFAULT, "Open"), ("ignore", "Ignore")]
        } else {
            &[
                (notify::DEFAULT, "Open"),
                ("approve", "Approve"),
                ("ignore", "Ignore"),
                ("deny", "Deny"),
            ]
        };
        let banner = Banner {
            summary: &summary,
            body: &body,
            replace: 0,
            actions,
            timeout_ms: 0,
            urgency: 1,
        };
        match notify::show(&banner) {
            Ok(id) => {
                s.stamp_write(&stamp, state::now(), id);
                open.lock().unwrap().insert(
                    id,
                    Open {
                        run_id: run.id,
                        workflow: run.name().to_string(),
                        url: run.html_url.clone().unwrap_or_else(|| run_url(cfg, run.id)),
                        env_ids: reviewable.iter().map(|e| e.id).collect(),
                        env_names,
                    },
                );
            }
            Err(e) => log(&format!("banner failed: {e}")),
        }
    }

    // 2. Runs that left the waiting list: promote, or ping the result once.
    for (id, phase) in s.watch_list() {
        if waiting.iter().any(|r| r.id == id) {
            continue;
        }
        // Reviewed elsewhere (TUI, website): its approval banner is stale.
        let stale: Vec<u32> = open
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, o)| o.run_id == id)
            .map(|(nid, _)| *nid)
            .collect();
        for nid in stale {
            open.lock().unwrap().remove(&nid);
            notify::close(nid);
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
            if let Err(e) = notify::show(&Banner::info("iris: result", &body, cfg.notify_ms)) {
                log(&format!("result banner failed: {e}"));
            }
            s.watch_del(id);
        } else if phase != "running" {
            s.watch_set(id, "running", run.name(), run.title());
        }
    }
    Ok(())
}

/// Handle the clicks on the approval banners.
fn start_listener(cfg: &Config, open: &OpenBanners) {
    let open = Arc::clone(open);
    let cfg = cfg.clone();
    let res = notify::listen(move |event| match event {
        Event::Closed { id, .. } => {
            open.lock().unwrap().remove(&id);
        }
        Event::Action { id, key } => {
            let Some(o) = open.lock().unwrap().remove(&id) else {
                return;
            };
            notify::close(id);
            let cfg = cfg.clone();
            // gh calls take a moment; keep reading signals meanwhile.
            std::thread::spawn(move || act(&cfg, id, &key, &o));
        }
    });
    if let Err(e) = res {
        log(&format!("can't listen for banner clicks: {e}"));
    }
}

fn act(cfg: &Config, banner: u32, key: &str, o: &Open) {
    let client = Client::new(&cfg.repo);
    let s = State::new(&cfg.cache);
    match key {
        notify::DEFAULT => notify::open_url(&o.url, Some(banner)),
        "ignore" => {
            s.ignore(o.run_id);
            log(&format!("run {} ignored", o.run_id));
        }
        "approve" | "deny" => {
            let (state, comment, verb) = if key == "approve" {
                ("approved", APPROVE_COMMENT, "Approved")
            } else {
                ("rejected", REJECT_COMMENT, "Denied")
            };
            let (summary, body) = match client.review(o.run_id, &o.env_ids, state, comment) {
                Ok(()) => {
                    log(&format!("run {} {state} ({})", o.run_id, o.env_names));
                    (
                        format!("iris: {verb} {}", o.workflow),
                        format!("{verb} the deploy to {}.", o.env_names),
                    )
                }
                Err(e) => {
                    log(&format!("{e}"));
                    (
                        format!("iris: couldn't {key} {}", o.workflow),
                        format!("{e}\nTry the TUI (iris) or GitHub."),
                    )
                }
            };
            if let Err(e) = notify::show(&Banner::info(&summary, &body, cfg.notify_ms)) {
                log(&format!("banner failed: {e}"));
            }
        }
        other => log(&format!("unknown banner action {other:?}")),
    }
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
