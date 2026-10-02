//! Fails when a crate or example directory is missing from a README table,
//! or a table row names one that no longer exists.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dirs(path: &str) -> BTreeSet<String> {
    fs::read_dir(root().join(path))
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn listed(readme: &str, pattern: &str) -> BTreeSet<String> {
    let text = fs::read_to_string(root().join(readme)).unwrap();
    Regex::new(&format!("(?m){pattern}"))
        .unwrap()
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect()
}

fn check(readme: &str, pattern: &str, path: &str) {
    let (have, want) = (listed(readme, pattern), dirs(path));
    let missing: Vec<_> = want.difference(&have).collect();
    let stale: Vec<_> = have.difference(&want).collect();
    assert!(
        missing.is_empty(),
        "{readme}: missing from table: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "{readme}: listed but not in {path}: {stale:?}"
    );
}

#[test]
fn sdk_crates() {
    check(
        "sdk/README.md",
        r"^\| \[`([\w-]+)`\]\(crates/",
        "sdk/crates",
    );
}

#[test]
fn sdk_examples() {
    check("sdk/README.md", r"^\| `([\w-]+)` \|", "sdk/examples");
}

#[test]
fn host_crates() {
    check("crates/README.md", r"^\| \[`([\w-]+)`\]\(", "crates");
}
