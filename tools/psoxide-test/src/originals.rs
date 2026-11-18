//! Original game files keyed by their content, never by where they sit on this machine.
//!
//! An input (`[inputs] NAME = "/path"` in the user config) is a file or a directory tree. Its
//! digest is SHA-256 over the sorted `relative path, file digest` pairs (a single file: its own
//! digest). Hashing gigabytes on every run would be slow, so each file's digest is cached in
//! `<work>/input-digests.json` under its path with the size, modification time and inode it had;
//! a file whose stat changed is hashed again. Receipts, result facts and the clean-clone key hold
//! the digest, so moving the files changes nothing and changing their bytes re-runs what reads them.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Stamp {
    size: u64,
    mtime_ns: i128,
    inode: u64,
    sha256: String,
}

/// The per-file digest cache, written back when dropped.
pub struct Cache {
    path: PathBuf,
    entries: BTreeMap<String, Stamp>,
    changed: bool,
}

impl Cache {
    /// Load the cache at `path` (empty when missing or unreadable).
    pub fn load(path: &Path) -> Self {
        let entries = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Cache {
            path: path.to_path_buf(),
            entries,
            changed: false,
        }
    }

    /// Write the cache back if anything was hashed.
    fn save(&self) {
        if self.changed {
            if let Ok(text) = serde_json::to_string(&self.entries) {
                let tmp = self.path.with_extension("tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &self.path);
                }
            }
        }
    }

    fn file(&mut self, path: &Path) -> Result<String, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mtime_ns = i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec());
        let key = path.display().to_string();
        if let Some(s) = self.entries.get(&key) {
            if s.size == meta.len() && s.mtime_ns == mtime_ns && s.inode == meta.ino() {
                return Ok(s.sha256.clone());
            }
        }
        let sha256 = util::sha256_file(path)?;
        self.entries.insert(
            key,
            Stamp {
                size: meta.len(),
                mtime_ns,
                inode: meta.ino(),
                sha256: sha256.clone(),
            },
        );
        self.changed = true;
        Ok(sha256)
    }

    /// The content digest of a file or a directory tree (symlinks followed, hidden files kept).
    pub fn digest(&mut self, root: &Path) -> Result<String, String> {
        if root.is_file() {
            return self.file(root);
        }
        if !root.is_dir() {
            return Err(format!(
                "original game files {} do not exist",
                root.display()
            ));
        }
        let mut files = Vec::new();
        walk(root, root, &mut files)?;
        files.sort();
        let mut key = util::Key::new("original");
        for rel in files {
            let digest = self.file(&root.join(&rel))?;
            key.part(&rel, &digest);
        }
        Ok(key.finish())
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.save();
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            out.push(rel.display().to_string());
        }
    }
    Ok(())
}

/// The `{input:NAME}` names a text uses, in order of first use.
pub fn names(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(at) = rest.find("{input:") {
        let tail = &rest[at + 7..];
        let Some(end) = tail.find('}') else { break };
        let name = tail[..end].to_string();
        if !out.contains(&name) {
            out.push(name);
        }
        rest = &tail[end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_digest_follows_content_not_location() {
        let dir = tempfile::tempdir().unwrap();
        for side in ["a", "b"] {
            std::fs::create_dir_all(dir.path().join(side).join("sub")).unwrap();
            std::fs::write(dir.path().join(side).join("x.bin"), "x").unwrap();
            std::fs::write(dir.path().join(side).join("sub/y.bin"), "y").unwrap();
        }
        let mut cache = Cache::load(&dir.path().join("cache.json"));
        let a = cache.digest(&dir.path().join("a")).unwrap();
        let b = cache.digest(&dir.path().join("b")).unwrap();
        assert_eq!(a, b, "same bytes at two paths give one digest");
        std::fs::write(dir.path().join("b/sub/y.bin"), "z").unwrap();
        assert_ne!(cache.digest(&dir.path().join("b")).unwrap(), a);
        std::fs::rename(dir.path().join("a/x.bin"), dir.path().join("a/w.bin")).unwrap();
        assert_ne!(
            cache.digest(&dir.path().join("a")).unwrap(),
            a,
            "a rename is a change"
        );
    }

    #[test]
    fn the_cache_survives_a_reload_and_notices_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("disc.bin");
        std::fs::write(&file, "one").unwrap();
        let store = dir.path().join("cache.json");
        let mut cache = Cache::load(&store);
        let first = cache.digest(&file).unwrap();
        cache.save();
        let mut again = Cache::load(&store);
        assert_eq!(again.digest(&file).unwrap(), first);
        assert!(!again.changed, "an unchanged stat is not hashed again");
        std::fs::write(&file, "two!").unwrap();
        assert_ne!(again.digest(&file).unwrap(), first);
    }

    #[test]
    fn names_lists_each_input_once() {
        assert_eq!(
            names("{input:a}/x {input:b} {input:a}"),
            ["a".to_string(), "b".to_string()]
        );
        assert!(names("plain").is_empty());
    }
}
