use std::path::PathBuf;

/// Runtime settings, all from the environment with sane defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// The repository to watch, `owner/name`.
    pub repo: String,
    /// Seconds between polls of the daemon.
    pub poll_secs: u64,
    /// An ignored banner pops again after this many seconds.
    pub reping_secs: u64,
    /// How long a desktop banner stays on screen, in milliseconds.
    pub notify_ms: i32,
    /// Commit titles in banners and the TUI truncate to this length.
    pub title_max: usize,
    /// Seconds between the automatic refreshes of the TUI.
    pub auto_secs: u64,
    /// Where the watch state and the banner stamps live.
    pub cache: PathBuf,
    /// The URL of the Actions page of the repository.
    pub runs_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        let repo = std::env::var("IRIS_REPO")
            .unwrap_or_else(|_| "cnbrown04/homelab-iac".to_string());
        let cache = std::env::var("IRIS_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_cache());
        Self {
            runs_url: format!("https://github.com/{repo}/actions"),
            repo,
            poll_secs: num("IRIS_POLL_SECS", 60),
            reping_secs: num("IRIS_REPING_SECS", 600),
            notify_ms: num("IRIS_NOTIFY_MS", 15_000),
            title_max: num("IRIS_TITLE_MAX", 70),
            auto_secs: num("IRIS_AUTO_SECS", 30),
            cache,
        }
    }
}

fn num<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn default_cache() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join("iris")
}
