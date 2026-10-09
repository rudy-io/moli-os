//! Profile files a human approved.
//!
//! A built-in profile ships with the binary and was reviewed with it. A file
//! (written by an agent, downloaded…) runs only once a human has read what it
//! reads and writes (`moli-os profile trust <file>`) and its SHA-256 is
//! recorded here. Any edit changes the hash: a new approval is needed.

use std::io;
use std::path::Path;

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

/// File name, in the data directory.
pub const TRUST_FILE: &str = "trusted-profiles.toml";

const HEADER: &str = "# Profile files a human approved (moli-os profile trust <file>).\n\
# A profile file runs only if its SHA-256 is listed: any edit needs a new approval.\n";

/// A profile file: where it was read, and the SHA-256 of its exact text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Approvals {
    #[serde(default)]
    trusted: Vec<Approval>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    /// Where the file was when approved (for humans: the hash is what counts).
    path: String,
    sha256: String,
}

/// Lowercase hex SHA-256 of a profile's text.
#[must_use]
pub fn sha256(text: &str) -> String {
    use std::fmt::Write as _;
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn parse(text: &str) -> io::Result<Approvals> {
    toml::from_str(text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Whether a human approved this text (by its hash). No file: nothing is.
pub async fn is_trusted(trust_file: &Path, sha256: &str) -> io::Result<bool> {
    let text = match tokio::fs::read_to_string(trust_file).await {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(parse(&text)?.trusted.iter().any(|a| a.sha256 == sha256))
}

/// What [`record`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    Added,
    /// The same path had another version: that one is no longer approved.
    Replaced,
    AlreadyTrusted,
}

/// Records a human's approval. Approving a new version of a file withdraws
/// the old one.
pub fn record(trust_file: &Path, file: &ProfileFile) -> io::Result<Recorded> {
    let mut approvals = match std::fs::read_to_string(trust_file) {
        Ok(text) => parse(&text)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Approvals::default(),
        Err(e) => return Err(e),
    };
    let approval = Approval {
        path: file.path.clone(),
        sha256: file.sha256.clone(),
    };
    if approvals.trusted.contains(&approval) {
        return Ok(Recorded::AlreadyTrusted);
    }
    let before = approvals.trusted.len();
    approvals.trusted.retain(|a| a.path != file.path);
    let recorded = if approvals.trusted.len() < before {
        Recorded::Replaced
    } else {
        Recorded::Added
    };
    approvals.trusted.push(approval);
    let text = toml::to_string(&approvals).map_err(io::Error::other)?;
    if let Some(dir) = trust_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    // Written aside then renamed: a crash never leaves half a list.
    let tmp = trust_file.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("{HEADER}\n{text}"))?;
    std::fs::rename(&tmp, trust_file)?;
    Ok(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approvals_follow_the_exact_text() {
        let dir = std::env::temp_dir().join(format!("moli-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trust_file = dir.join(TRUST_FILE);
        let v1 = ProfileFile {
            path: "/data/profiles/x.toml".into(),
            sha256: sha256("version 1"),
        };
        assert!(
            !is_trusted(&trust_file, &v1.sha256).await.unwrap(),
            "no file"
        );
        assert_eq!(record(&trust_file, &v1).unwrap(), Recorded::Added);
        assert_eq!(record(&trust_file, &v1).unwrap(), Recorded::AlreadyTrusted);
        assert!(is_trusted(&trust_file, &v1.sha256).await.unwrap());

        // An edit is a new text: not approved until a human says so, and
        // approving it withdraws the old version.
        let v2 = ProfileFile {
            sha256: sha256("version 2"),
            ..v1.clone()
        };
        assert!(!is_trusted(&trust_file, &v2.sha256).await.unwrap());
        assert_eq!(record(&trust_file, &v2).unwrap(), Recorded::Replaced);
        assert!(is_trusted(&trust_file, &v2.sha256).await.unwrap());
        assert!(!is_trusted(&trust_file, &v1.sha256).await.unwrap());
        let text = std::fs::read_to_string(&trust_file).unwrap();
        assert!(text.starts_with("# Profile files") && text.contains("[[trusted]]"));

        std::fs::write(&trust_file, "nonsense = [").unwrap();
        assert!(is_trusted(&trust_file, &v2.sha256).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hashes_are_lowercase_hex() {
        assert_eq!(
            sha256(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
