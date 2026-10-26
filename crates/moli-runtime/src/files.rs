//! The house's files, written whole or not at all: the one way every crate
//! saves one.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Who may read a file written here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// As the process's umask says.
    Shared,
    /// Moli's user only (0600 on unix): people, e-mails, places.
    Private,
}

/// Writes `bytes` to `path`: into a file of its own beside it (two saves at
/// once never share one), synced to the disk, then renamed over `path`. A
/// crash or a power cut leaves the old file or the new one, never half of
/// one. The folder is created if needed.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Mode) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let aside = aside(path);
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if mode == Mode::Private {
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        }
        let mut file = options.open(&aside)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&aside, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&aside);
    }
    written
}

/// [`write_atomic`] from async code (on a blocking thread).
pub async fn save(path: &Path, bytes: Vec<u8>, mode: Mode) -> io::Result<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || write_atomic(&path, &bytes, mode))
        .await
        .map_err(io::Error::other)?
}

/// `<file>.<pid>.<n>.tmp`, unique in this process and across restarts.
fn aside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("moli-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_file_is_replaced_whole_and_nothing_is_left_beside_it() {
        let dir = dir("whole");
        let path = dir.join("sub").join("a.json");
        write_atomic(&path, b"one", Mode::Shared).unwrap();
        write_atomic(&path, b"two", Mode::Shared).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saves_at_once_never_share_a_file() {
        let dir = dir("together");
        let path = dir.join("a.json");
        let threads: Vec<_> = (0..8u8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || write_atomic(&path, &[n; 4096], Mode::Shared))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.iter().all(|b| *b == bytes[0]), "one save, whole");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_for_moli_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir("private");
        let path = dir.join("people.json");
        write_atomic(&path, b"{}", Mode::Private).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
