//! The on-disk state of the watcher, under the cache dir:
//!   watch/<run_id>        phase word: waiting | running
//!   watch/<run_id>.meta   workflow \t title (for the result ping)
//!   shown/<banner>        epoch of the last banner \n its notification id
//! Writes are atomic (tmp + rename).

use std::fs;
use std::path::{Path, PathBuf};

pub struct State {
    cache: PathBuf,
}

impl State {
    pub fn new(cache: &Path) -> Self {
        Self {
            cache: cache.to_path_buf(),
        }
    }

    fn watch_dir(&self) -> PathBuf {
        let d = self.cache.join("watch");
        let _ = fs::create_dir_all(&d);
        d
    }

    fn shown_dir(&self) -> PathBuf {
        let d = self.cache.join("shown");
        let _ = fs::create_dir_all(&d);
        d
    }

    /// Every watched run with its phase, sorted by id.
    pub fn watch_list(&self) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        let Ok(dir) = fs::read_dir(self.watch_dir()) else {
            return out;
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if name.starts_with('.') || name.ends_with(".meta") {
                continue;
            }
            if let Ok(id) = name.parse::<u64>() {
                let phase = fs::read_to_string(entry.path())
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                out.push((id, phase));
            }
        }
        out.sort();
        out
    }

    pub fn watch_set(&self, id: u64, phase: &str, workflow: &str, title: &str) {
        let dir = self.watch_dir();
        let _ = atomic_write(&dir.join(id.to_string()), format!("{phase}\n"));
        let _ = atomic_write(
            &dir.join(format!("{id}.meta")),
            format!("{workflow}\t{title}\n"),
        );
    }

    pub fn watch_del(&self, id: u64) {
        let dir = self.watch_dir();
        let _ = fs::remove_file(dir.join(id.to_string()));
        let _ = fs::remove_file(dir.join(format!("{id}.meta")));
    }

    /// The (epoch, notification id) of a banner, when we have shown one.
    pub fn stamp(&self, banner: &str) -> Option<(u64, u32)> {
        let text = fs::read_to_string(self.shown_dir().join(banner)).ok()?;
        let mut lines = text.lines();
        let epoch: u64 = lines.next()?.trim().parse().ok()?;
        let id: u32 = lines.next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
        Some((epoch, id))
    }

    pub fn stamp_write(&self, banner: &str, epoch: u64, id: u32) {
        let _ = atomic_write(
            &self.shown_dir().join(banner),
            format!("{epoch}\n{id}\n"),
        );
    }

    /// True when the banner was shown less than `secs` ago.
    pub fn fresh(&self, banner: &str, secs: u64) -> bool {
        match self.stamp(banner) {
            Some((epoch, _)) => now().saturating_sub(epoch) < secs,
            None => false,
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn atomic_write(path: &Path, text: String) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}
