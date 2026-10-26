//! What the house asked Moli and what it answered, kept 30 days in a
//! JSON-lines file of the data directory: the dashboard's history and the
//! evening recap. Text only: never the audio, never a key.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// How long an exchange is kept.
const KEEP_MS: i64 = 30 * 24 * 3600 * 1000;
/// At most this many kept (memory and file).
const MAX: usize = 5_000;
/// While Moli runs, the file is pruned again after this many lines, and a
/// day after it last was: it never grows much past what is kept.
const PRUNE_EVERY: usize = 500;
const PRUNE_AFTER_MS: i64 = 24 * 3600 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A question and Moli's answer.
    Turn,
    /// A question Moli could not answer (the provider failed…).
    Failed,
    /// A satellite woke and heard nothing it could understand: a wake word
    /// said for nothing, or a false activation.
    Silence,
    /// A message left at a door, passed on to the household.
    Door,
}

/// An order given during a turn, as the history shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub device: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    /// Milliseconds since the epoch.
    pub at: i64,
    pub kind: Kind,
    /// Where it was asked: `satellite`, `bubble`, `page`…
    pub surface: String,
    pub spoken: bool,
    pub question: String,
    pub reply: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub orders: Vec<Order>,
    /// How long Moli took, from the question to the answer.
    pub ms: u64,
    /// What the satellite heard, while recording is on (`recordings/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// What it cost, estimated (dollars of the provider's credit).
    #[serde(default)]
    pub cost: Cost,
}

/// An exchange's estimated cost, in dollars, by what was paid for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// The model's thinking (tokens).
    pub think: f64,
    /// Speech to text.
    pub hear: f64,
    /// The answer's voice.
    pub speak: f64,
    /// Web searches.
    pub search: f64,
    pub total: f64,
}

impl Cost {
    pub(crate) fn new(think: f64, hear: f64, speak: f64, search: f64) -> Self {
        Self {
            think,
            hear,
            speak,
            search,
            total: think + hear + speak + search,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Exchanges {
    disk: Mutex<OnDisk>,
    kept: Mutex<VecDeque<Exchange>>,
}

/// The file, and how much it grew since it was last pruned.
#[derive(Debug, Default)]
struct OnDisk {
    path: Option<PathBuf>,
    /// Lines added since then, and when that was.
    added: usize,
    pruned_ms: i64,
}

impl Exchanges {
    /// Reads what the file holds, keeps the last 30 days and rewrites it.
    pub(crate) fn open(&self, path: PathBuf, now_ms: i64) {
        let old: VecDeque<Exchange> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let kept = keep(old, now_ms);
        rewrite(&path, &kept);
        *self.kept.lock().unwrap_or_else(PoisonError::into_inner) = kept;
        *self.disk.lock().unwrap_or_else(PoisonError::into_inner) = OnDisk {
            path: Some(path),
            added: 0,
            pruned_ms: now_ms,
        };
    }

    pub(crate) fn add(&self, exchange: Exchange) {
        let now_ms = exchange.at;
        let line = serde_json::to_string(&exchange);
        // Held to the end: a pruning never loses a line added meanwhile.
        let mut disk = self.disk.lock().unwrap_or_else(PoisonError::into_inner);
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.push_back(exchange);
        while kept.len() > MAX {
            kept.pop_front();
        }
        let Some(path) = disk.path.clone() else {
            return;
        };
        // Pruned while Moli runs too, not only at start (this exchange kept).
        if disk.added + 1 >= PRUNE_EVERY || now_ms - disk.pruned_ms >= PRUNE_AFTER_MS {
            rewrite(&path, &keep(kept.clone(), now_ms));
            disk.added = 0;
            disk.pruned_ms = now_ms;
        } else if let Ok(line) = line {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                tracing::warn!(path = %path.display(), error = %e, "assistant exchange not saved");
            }
            disk.added += 1;
        }
    }

    /// Those since `from_ms`, oldest first.
    pub(crate) fn since(&self, from_ms: i64) -> Vec<Exchange> {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|e| e.at >= from_ms)
            .cloned()
            .collect()
    }
}

fn keep(mut all: VecDeque<Exchange>, now_ms: i64) -> VecDeque<Exchange> {
    all.retain(|e| e.at >= now_ms - KEEP_MS);
    while all.len() > MAX {
        all.pop_front();
    }
    all
}

/// The file, whole (never half written).
fn rewrite(path: &Path, kept: &VecDeque<Exchange>) {
    let text: String = kept
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .map(|line| line + "\n")
        .collect();
    if let Err(e) =
        moli_runtime::files::write_atomic(path, text.as_bytes(), moli_runtime::files::Mode::Shared)
    {
        tracing::warn!(path = %path.display(), error = %e, "assistant exchanges not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(at: i64, question: &str) -> Exchange {
        Exchange {
            at,
            kind: Kind::Turn,
            surface: "satellite".into(),
            spoken: true,
            question: question.into(),
            reply: "C'est fait.".into(),
            tools: vec!["set".into()],
            orders: vec![Order {
                device: "Plafonnier".into(),
                status: "done".into(),
            }],
            ms: 1_200,
            audio: None,
            cost: Cost::default(),
        }
    }

    #[test]
    fn exchanges_are_kept_thirty_days_across_a_restart() {
        let dir = std::env::temp_dir().join(format!("moli-exchanges-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exchanges.jsonl");
        let _ = std::fs::remove_file(&path);
        let day = 24 * 3600 * 1000;
        let now = 100 * day;

        let log = Exchanges::default();
        log.open(path.clone(), now);
        log.add(said(now - 40 * day, "trop vieux"));
        log.add(said(now - day, "hier"));
        log.add(said(now, "maintenant"));
        assert_eq!(log.since(now - 2 * day).len(), 2);

        // A restart: the file is read back, the old one dropped for good.
        let again = Exchanges::default();
        again.open(path.clone(), now);
        let back = again.since(0);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].question, "hier");
        assert_eq!(back[1].orders[0].device, "Plafonnier");
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Moli may run for months without a restart: the file is pruned as it
    /// goes, a day after it last was and every few hundred lines.
    #[test]
    fn the_file_is_pruned_while_moli_runs() {
        let dir = std::env::temp_dir().join(format!("moli-exchanges-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exchanges.jsonl");
        let _ = std::fs::remove_file(&path);
        let lines = || std::fs::read_to_string(&path).unwrap().lines().count();
        let day = 24 * 3600 * 1000;
        let start = 100 * day;

        let log = Exchanges::default();
        log.open(path.clone(), start);
        log.add(said(start, "il y a un mois"));
        log.add(said(start + 60_000, "une minute après"));
        assert_eq!(lines(), 2);
        // A month later, Moli still running: the old ones leave the file.
        log.add(said(start + 31 * day, "aujourd'hui"));
        assert_eq!(lines(), 1);

        // A busy day: the file never holds much more than is kept.
        let busy = start + 32 * day;
        for i in 0..MAX + 3 * PRUNE_EVERY {
            log.add(said(busy + i64::try_from(i).unwrap(), "encore"));
        }
        assert!(lines() < MAX + PRUNE_EVERY, "{} lines", lines());
        assert_eq!(log.since(0).len(), MAX);
        let left = std::fs::read_dir(path.parent().unwrap()).unwrap();
        assert!(
            left.flatten()
                .all(|e| !e.path().to_string_lossy().ends_with(".tmp"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
