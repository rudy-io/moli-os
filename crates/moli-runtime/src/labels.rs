//! Label persistence: one human-editable TOML file, written atomically.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use moli_core::{DeviceId, Label};

pub(crate) fn load(path: &Path) -> io::Result<BTreeMap<DeviceId, Label>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e),
    }
}

pub(crate) async fn save(path: &Path, labels: &BTreeMap<DeviceId, Label>) -> io::Result<()> {
    let text = toml::to_string_pretty(labels).map_err(io::Error::other)?;
    write_atomic(path, text.as_bytes()).await
}

/// Write-then-rename: a crash never leaves a half-written file.
pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}
