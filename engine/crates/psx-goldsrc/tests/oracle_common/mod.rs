//! Shared plumbing for the frozen-oracle tests: each builds a scratch cargo
//! project out of frozen legacy source, the current shared source and a
//! harness, then runs it and requires a clean exit.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// `engine/crates/psx-goldsrc/tests`.
pub fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// The editor repository root (the SDK crates live under `sdk/`).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("editor root")
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A scratch directory removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(prefix: &str) -> Scratch {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("{prefix}{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(path.join("src")).expect("scratch directory");
        Scratch(path)
    }

    pub fn write(&self, relative: &str, text: &str) {
        std::fs::write(self.0.join(relative), text).expect("write scratch file");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `cargo <args>` in `dir`; panic with its output if it fails, else return
/// its trimmed stdout.
pub fn cargo(dir: &Path, args: &[&str]) -> String {
    let run = Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run cargo");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    assert!(
        run.status.success(),
        "cargo {args:?} failed:\n{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    stdout.trim().to_string()
}

/// The matching closing brace of an item whose opening brace ended at `end`
/// (exclusive): the index one past the brace that balances it.
pub fn skip_block(source: &str, mut end: usize) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 1i32;
    while depth != 0 {
        depth += i32::from(bytes[end] == b'{') - i32::from(bytes[end] == b'}');
        end += 1;
    }
    end
}
