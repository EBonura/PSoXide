//! Writing one library file so that no reader ever sees it half written.

use crate::provenance::sha256_file;
use crate::Error;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// The temporary name `live` is written under: a dot file beside it.
pub(crate) fn temporary_name(live: &Path) -> PathBuf {
    let file_name = live
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    live.with_file_name(format!(".{file_name}.installing"))
}

/// Write `live` by filling a temporary file beside it, checking that it hashes to `expected`,
/// and renaming it over `live`. A failed publish leaves the previous `live` untouched and the
/// temporary file removed.
pub(crate) fn publish(
    live: &Path,
    expected: &str,
    fill: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), Error> {
    let temporary = temporary_name(live);
    let outcome = fill(&temporary)
        .map_err(|error| Error::io(format!("write {}", temporary.display()), error))
        .and_then(|()| {
            let actual = sha256_file(&temporary)
                .map_err(|error| Error::io(format!("read {}", temporary.display()), error))?;
            if actual == expected {
                Ok(())
            } else {
                Err(Error::DigestMismatch {
                    path: temporary.clone(),
                    actual,
                    expected: expected.into(),
                })
            }
        })
        .and_then(|()| {
            fs::rename(&temporary, live)
                .map_err(|error| Error::io(format!("rename onto {}", live.display()), error))
        });
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::sha256_bytes;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "psx-library-publish-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_published_file_replaces_the_live_one_and_leaves_no_temporary() {
        let dir = scratch("ok");
        let live = dir.join("Game Name (repo).bin");
        fs::write(&live, b"old").unwrap();
        publish(&live, &sha256_bytes(b"new"), |temporary| {
            fs::write(temporary, b"new")
        })
        .unwrap();
        assert_eq!(fs::read(&live).unwrap(), b"new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wrong_digest_keeps_the_live_file_and_removes_the_temporary() {
        let dir = scratch("bad");
        let live = dir.join("live.bin");
        fs::write(&live, b"old").unwrap();
        let error = publish(&live, &sha256_bytes(b"expected"), |temporary| {
            fs::write(temporary, b"corrupt")
        })
        .unwrap_err();
        assert!(matches!(error, Error::DigestMismatch { .. }), "{error}");
        assert_eq!(fs::read(&live).unwrap(), b"old");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_fill_keeps_the_live_file() {
        let dir = scratch("fill");
        let live = dir.join("live.bin");
        fs::write(&live, b"old").unwrap();
        let error = publish(&live, "unused", |temporary| {
            fs::write(temporary, b"half")?;
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        assert!(matches!(error, Error::Io { .. }), "{error}");
        assert_eq!(fs::read(&live).unwrap(), b"old");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
