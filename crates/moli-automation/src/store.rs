//! On disk: `automations.json` (written whole, atomically) and the runs,
//! one JSON line each, trimmed when the file grows.

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::Automation;
use crate::run::Run;

/// Runs kept in memory (and on disk after a trim).
pub(crate) const KEEP_RUNS: usize = 500;
const TRIM_AT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct File {
    automations: Vec<Automation>,
}

pub(crate) async fn load(path: Option<&Path>) -> anyhow::Result<Vec<Automation>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    match read(path).await {
        Ok(list) => Ok(list),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Vec::new())
        }
        Err(e) => {
            // A damaged file must not keep the house from starting: the last
            // good copy, else nothing (the damaged file is kept aside).
            tracing::error!(error = %e, "automations file damaged: falling back to the backup");
            let aside = path.with_extension(format!("json.damaged-{}", moli_core::now_ms()));
            let _ = tokio::fs::copy(path, &aside).await;
            match read(&path.with_extension("json.bak")).await {
                Ok(list) => Ok(list),
                Err(e) => {
                    tracing::error!(error = %e, "no usable backup: starting without automations");
                    Ok(Vec::new())
                }
            }
        }
    }
}

async fn read(path: &Path) -> anyhow::Result<Vec<Automation>> {
    let bytes = tokio::fs::read(path).await?;
    let file: File = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("{} is damaged: {e}", path.display()))?;
    Ok(file.automations)
}

/// Written whole: to a temporary file, flushed to the disk, then renamed;
/// the previous version kept as `.bak`.
pub(crate) async fn save(path: Option<&Path>, list: &[Automation]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let Some(path) = path else {
        return Ok(());
    };
    let json = serde_json::to_vec_pretty(&serde_json::json!({ "automations": list }))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = tokio::fs::File::create(&tmp).await?;
    file.write_all(&json).await?;
    file.sync_all().await?;
    drop(file);
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::copy(path, path.with_extension("json.bak")).await;
    }
    tokio::fs::rename(&tmp, path).await
}

pub(crate) async fn load_runs(path: Option<&Path>) -> VecDeque<Run> {
    let Some(path) = path else {
        return VecDeque::new();
    };
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return VecDeque::new();
    };
    let mut skipped = 0;
    let mut runs: VecDeque<Run> = text
        .lines()
        .filter_map(|l| {
            serde_json::from_str(l).ok().or_else(|| {
                skipped += 1;
                None
            })
        })
        .collect();
    if skipped > 0 {
        // Written by another version, or damaged: kept in the file until a trim (see `append_run`).
        tracing::warn!(skipped, "automation runs: lines not understood");
    }
    while runs.len() > KEEP_RUNS {
        runs.pop_front();
    }
    runs
}

/// Appends a finished run; rewrites the file with the last runs when it
/// gets big.
pub(crate) async fn append_run(path: Option<&Path>, run: &Run, recent: Vec<Run>) {
    let Some(path) = path else {
        return;
    };
    let result = async {
        use tokio::io::AsyncWriteExt as _;
        let big = tokio::fs::metadata(path)
            .await
            .is_ok_and(|m| m.len() > TRIM_AT_BYTES);
        if big {
            let mut text = String::new();
            for r in &recent {
                text.push_str(&serde_json::to_string(r)?);
                text.push('\n');
            }
            // The trim keeps what is in memory; lines this version could not read (a rollback,
            // a damaged line) would go with the rest, so the whole file is kept aside first.
            tokio::fs::copy(path, path.with_extension("jsonl.bak")).await?;
            let tmp = path.with_extension("jsonl.tmp");
            tokio::fs::write(&tmp, text).await?;
            tokio::fs::rename(&tmp, path).await?;
            return anyhow::Ok(());
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        let mut line = serde_json::to_string(run)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "automation run not recorded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::RunStatus;

    #[tokio::test]
    async fn a_trim_keeps_the_lines_it_could_not_read_aside() {
        let path = std::env::temp_dir().join(format!("moli-runs-{}.jsonl", std::process::id()));
        let bak = path.with_extension("jsonl.bak");
        // One line of a future version, big enough to ask for a trim.
        let unknown = "x".repeat(usize::try_from(TRIM_AT_BYTES).unwrap() + 1);
        tokio::fs::write(&path, format!("{unknown}\n"))
            .await
            .unwrap();
        assert!(load_runs(Some(&path)).await.is_empty());

        let run = Run {
            id: 1,
            automation: "a".into(),
            name: "a".into(),
            trigger: "t".into(),
            why: "w".into(),
            started: 0,
            ended: None,
            status: RunStatus::Done,
            dry: false,
            steps: Vec::new(),
        };
        append_run(Some(&path), &run, vec![run.clone()]).await;

        assert_eq!(
            load_runs(Some(&path)).await.len(),
            1,
            "trimmed to the known runs"
        );
        assert!(
            tokio::fs::read_to_string(&bak)
                .await
                .unwrap()
                .starts_with("xxxx")
        );
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(&bak).await;
    }
}
