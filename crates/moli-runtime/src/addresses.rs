//! Where each driver instance last reached its device (`data/addresses.json`).
//!
//! A device on the home network can get another address from the box any
//! day. A driver that knows its device's MAC and identity can find it again
//! (`moli_net::lan`) and remember where, here. What is learned holds only
//! while the configuration gives the same address: a person who edits it is
//! obeyed. Only learned things: lost, the file is learned again.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use moli_core::InstanceId;
use serde::{Deserialize, Serialize};

/// Longest identity kept: a device's own id (MAC, serial), never a text.
const MAX_IDENTITY: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownAddress {
    /// The address the configuration gives, which the rest was learned for.
    pub configured: String,
    /// Where the device answered last.
    pub host: String,
    /// Its MAC address (`aabbccddeeff`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// What it says it is (its native id), to recognise it elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

impl KnownAddress {
    /// As kept: an identity too long to be one is not kept.
    #[must_use]
    pub(crate) fn bounded(mut self) -> Self {
        if self
            .identity
            .as_ref()
            .is_some_and(|i| i.len() > MAX_IDENTITY)
        {
            self.identity = None;
        }
        self
    }
}

pub(crate) type Book = BTreeMap<InstanceId, KnownAddress>;

/// The book, or an empty one (missing, or unreadable: it is learned again).
pub(crate) fn load(path: &Path) -> Book {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "addresses unreadable, learning them again");
            Book::new()
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Book::new(),
        Err(e) => {
            tracing::warn!(error = %e, "addresses unreadable, learning them again");
            Book::new()
        }
    }
}

pub(crate) async fn save(path: &Path, book: &Book) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(book).map_err(io::Error::other)?;
    crate::files::save(path, bytes, crate::files::Mode::Shared).await
}
