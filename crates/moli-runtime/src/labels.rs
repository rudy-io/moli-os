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
    crate::files::save(path, text.into_bytes(), crate::files::Mode::Shared).await
}
