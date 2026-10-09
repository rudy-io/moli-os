//! Append-only journal on disk (JSON Lines), so the audit trail survives
//! restarts and upgrades. Writes happen on a dedicated thread: recording a
//! journal entry never blocks the async runtime.
//!
//! The file is bounded: past `TRIM_AT_BYTES`, it is rewritten with its last
//! `KEEP_LINES` lines (startup reads it whole: it must stay small), and the
//! whole previous file is kept beside it as `journal.jsonl.1`: what a burst
//! of commands pushes out of the live file is still on disk. Write failures
//! are counted and shown on `/api/health`, never silent.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use moli_core::JournalEntry;

const TRIM_AT_BYTES: u64 = 8 * 1024 * 1024;
const KEEP_LINES: usize = 2_000;

#[derive(Debug)]
pub(crate) struct JournalLog {
    tx: mpsc::Sender<Arc<JournalEntry>>,
    failures: Arc<AtomicU64>,
}

impl JournalLog {
    /// Opens (or creates) the log and returns its last `keep` entries.
    pub(crate) fn open(path: &Path, keep: usize) -> io::Result<(Self, Vec<Arc<JournalEntry>>)> {
        let mut entries: Vec<Arc<JournalEntry>> = match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .map(Arc::new)
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        if entries.len() > keep {
            entries.drain(..entries.len() - keep);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = append_to(path)?;
        let mut size = file.metadata().map_or(0, |m| m.len());
        let (tx, rx) = mpsc::channel::<Arc<JournalEntry>>();
        let failures = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&failures);
        let path: PathBuf = path.to_owned();
        std::thread::Builder::new()
            .name("moli-journal".into())
            .spawn(move || {
                let mut out = BufWriter::new(file);
                for entry in rx {
                    let mut line = match serde_json::to_vec(&*entry) {
                        Ok(line) => line,
                        Err(e) => {
                            counted.fetch_add(1, Ordering::Relaxed);
                            tracing::error!(error = %e, id = entry.id, "journal entry not persisted");
                            continue;
                        }
                    };
                    line.push(b'\n');
                    if let Err(e) = out.write_all(&line).and_then(|()| out.flush()) {
                        counted.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(error = %e, id = entry.id, "journal entry not persisted");
                        continue;
                    }
                    size += line.len() as u64;
                    if size > TRIM_AT_BYTES {
                        match trim(&path) {
                            Ok((file, new_size)) => {
                                out = BufWriter::new(file);
                                size = new_size;
                            }
                            Err(e) => {
                                counted.fetch_add(1, Ordering::Relaxed);
                                tracing::error!(error = %e, "journal not trimmed");
                            }
                        }
                    }
                }
            })?;
        Ok((Self { tx, failures }, entries))
    }

    pub(crate) fn append(&self, entry: Arc<JournalEntry>) {
        if self.tx.send(entry).is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Entries that could not be written to disk since the start.
    pub(crate) fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

fn append_to(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// Keeps the last `KEEP_LINES` lines (written aside, then renamed), the
/// whole file becoming the previous generation; returns the reopened file
/// and its size.
fn trim(path: &Path) -> io::Result<(File, u64)> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let kept = &lines[lines.len().saturating_sub(KEEP_LINES)..];
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut out = BufWriter::new(File::create(&tmp)?);
        for line in kept {
            out.write_all(line.as_bytes())?;
            out.write_all(b"\n")?;
        }
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
    }
    std::fs::rename(path, previous(path))?;
    std::fs::rename(&tmp, path)?;
    let file = append_to(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

/// `journal.jsonl` → `journal.jsonl.1`.
fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_keeps_the_last_lines() {
        let dir = std::env::temp_dir().join(format!("moli-journal-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.jsonl");
        let text = (0..KEEP_LINES + 500).fold(String::new(), |mut text, i| {
            use std::fmt::Write as _;
            let _ = writeln!(text, "{{\"n\":{i}}}");
            text
        });
        std::fs::write(&path, text).unwrap();
        let (_file, size) = trim(&path).unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert_eq!(kept.lines().count(), KEEP_LINES);
        assert_eq!(kept.lines().next(), Some("{\"n\":500}"));
        assert_eq!(size, kept.len() as u64);
        let before = std::fs::read_to_string(dir.join("journal.jsonl.1")).unwrap();
        assert_eq!(
            before.lines().count(),
            KEEP_LINES + 500,
            "the previous file is kept whole"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
