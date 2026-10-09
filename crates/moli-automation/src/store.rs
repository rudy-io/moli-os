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
    let mut runs: VecDeque<Run> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
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
