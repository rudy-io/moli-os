//! Last known values, persisted across restarts.
//!
//! Many devices (sleepy Zigbee sensors above all) only report on change or
//! every few hours. Without a cache, a restart would show "unknown" until
//! they speak again. Samples keep their original timestamp: the UI shows
//! how old a restored value is, nothing is passed off as fresh.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::sync::Arc;

use moli_core::{DeviceId, Sample};

pub(crate) type States = HashMap<DeviceId, BTreeMap<Arc<str>, Sample>>;

pub(crate) fn load(path: &Path) -> io::Result<States> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(States::new()),
        Err(e) => Err(e),
    }
}

pub(crate) async fn save(path: &Path, states: &States) -> io::Result<()> {
    let bytes = serde_json::to_vec(states).map_err(io::Error::other)?;
    crate::labels::write_atomic(path, &bytes).await
}
