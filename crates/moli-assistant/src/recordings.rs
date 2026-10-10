//! What the satellites heard, kept a few days while someone tests the
//! microphone (how far, how much noise): switched on from the dashboard for
//! at most a week, each recording deleted a week after it was made. 16 kHz
//! WAV files in `recordings/` of the data directory.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 24 * 3_600_000;
/// A recording is deleted this long after it was made.
const KEEP_MS: i64 = 7 * DAY_MS;
/// Recording is switched on for at most this long.
pub(crate) const MAX_DAYS: u32 = 7;
const SETTINGS: &str = "settings.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Switch {
    /// Recording until then (milliseconds since the epoch).
    until: Option<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct Recordings {
    dir: Mutex<Option<PathBuf>>,
    until: Mutex<Option<i64>>,
}

impl Recordings {
    /// The folder, its switch read back, the old recordings deleted.
    pub(crate) fn open(&self, dir: PathBuf, now_ms: i64) {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(dir = %dir.display(), error = %e, "no recordings folder");
            return;
        }
        let switch: Switch = std::fs::read(dir.join(SETTINGS))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        *self.until.lock().unwrap_or_else(PoisonError::into_inner) = switch.until;
        prune(&dir, now_ms);
        *self.dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
    }

    /// Until when recording is on, if it is.
    pub(crate) fn until(&self, now_ms: i64) -> Option<i64> {
        self.until
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .filter(|u| *u > now_ms)
    }

    /// On for `days` (at most a week), or off with 0.
    pub(crate) fn switch(&self, days: u32, now_ms: i64) -> std::io::Result<Option<i64>> {
        let until = (days > 0).then(|| now_ms + i64::from(days.min(MAX_DAYS)) * DAY_MS);
        let dir = self
            .dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(dir) = dir {
            let body = serde_json::to_vec(&Switch { until })?;
            std::fs::write(dir.join(SETTINGS), body)?;
        }
        *self.until.lock().unwrap_or_else(PoisonError::into_inner) = until;
        Ok(until)
    }

    /// Keeps `wav` when recording is on; its name.
    pub(crate) fn save(&self, wav: &[u8], now_ms: i64) -> Option<String> {
        self.until(now_ms)?;
        let dir = self
            .dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let mut raw = [0u8; 4];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).ok()?;
        let name = format!("{now_ms}-{}.wav", u32::from_le_bytes(raw));
        if let Err(e) = std::fs::write(dir.join(&name), wav) {
            tracing::warn!(error = %e, "recording not kept");
            return None;
        }
        prune(&dir, now_ms);
        Some(name)
    }

    /// The file of recording `name`, if it is one of ours and still there.
    pub(crate) fn path(&self, name: &str) -> Option<PathBuf> {
        let stem = name.strip_suffix(".wav")?;
        let ours = !stem.is_empty() && stem.chars().all(|c| c.is_ascii_digit() || c == '-');
        if !ours {
            return None;
        }
        let dir = self
            .dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let path = dir.join(name);
        path.is_file().then_some(path)
    }
}

/// When a recording was made, from its name.
fn made_at(name: &str) -> Option<i64> {
    name.strip_suffix(".wav")?.split('-').next()?.parse().ok()
}

/// Deletes the recordings older than a week.
fn prune(dir: &Path, now_ms: i64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if made_at(&name).is_some_and(|at| at < now_ms - KEEP_MS) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recordings_are_kept_while_switched_on_then_a_week() {
        let dir = std::env::temp_dir().join(format!("moli-recordings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let now = 1_000 * DAY_MS;
        let r = Recordings::default();
        r.open(dir.clone(), now);
        assert_eq!(r.save(b"RIFF", now), None, "off by default");
        assert_eq!(
            r.switch(30, now).unwrap(),
            Some(now + 7 * DAY_MS),
            "a week at most"
        );
        let name = r.save(b"RIFF", now).unwrap();
        assert!(r.path(&name).is_some());
        assert!(r.path("../moli.toml").is_none());
        assert!(r.path("settings.json").is_none());
        // A restart keeps the switch; eight days later the file is gone.
        let again = Recordings::default();
        again.open(dir.clone(), now + 8 * DAY_MS);
        assert!(again.path(&name).is_none());
        assert_eq!(again.until(now + 8 * DAY_MS), None, "the week is over");
        assert_eq!(again.until(now + DAY_MS), Some(now + 7 * DAY_MS));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
