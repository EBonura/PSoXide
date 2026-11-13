//! `tools/build_guest_staged.sh` builds the guest from a copy of the files it lists in `CLOSURE`.
//! A file the guest build reads that is not in that list makes the staged build fail, so this
//! test reads the list from the script and checks that every file a source in the closure pulls
//! in by `#[path]`, `include!`, `include_str!` or `include_bytes!` is itself inside the closure,
//! and that the profile the build scripts look for above the crate is there.

use std::fs;
use std::path::{Component, Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The entries of `CLOSURE`, with `$GUEST_DIR` expanded.
fn closure() -> Vec<String> {
    let script = fs::read_to_string(root().join("tools/build_guest_staged.sh")).unwrap();
    let guest = script
        .lines()
        .find_map(|line| line.strip_prefix("GUEST_DIR=\""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("GUEST_DIR in the script")
        .to_string();
    let start = script.find("CLOSURE=\"").expect("CLOSURE in the script") + "CLOSURE=\"".len();
    let end = start + script[start..].find('"').expect("end of CLOSURE");
    script[start..end]
        .split_whitespace()
        .map(|entry| entry.replace("$GUEST_DIR", &guest))
        .collect()
}

/// `path` with `.` and `..` folded away, relative to the repository root.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The string literals that follow a file-reading attribute or macro in `text`.
fn referenced(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for marker in [
        "#[path = \"",
        "include!(\"",
        "include_str!(\"",
        "include_bytes!(\"",
    ] {
        let mut rest = text;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            if let Some(end) = rest.find('"') {
                found.push(rest[..end].to_string());
            }
        }
    }
    found
}

#[test]
fn the_staged_guest_closure_holds_every_file_the_guest_build_reads() {
    let closure = closure();
    let inside = |path: &Path| closure.iter().any(|entry| path.starts_with(entry));
    let mut problems = Vec::new();
    for entry in &closure {
        let top = root().join(entry);
        assert!(top.exists(), "CLOSURE lists {entry}, which does not exist");
        let mut files = Vec::new();
        if top.is_dir() {
            sources(&top, &mut files);
        } else if top.extension().is_some_and(|ext| ext == "rs") {
            files.push(top.clone());
        }
        for file in files {
            let text = fs::read_to_string(&file).unwrap_or_default();
            for name in referenced(&text) {
                // `#[path]` is relative to the directory of a `mod` file, which for the build
                // scripts and crate roots this tree uses is the file's own directory.
                let target = normalize(
                    file.strip_prefix(root())
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(&name)
                        .as_path(),
                );
                if root().join(&target).exists() && !inside(&target) {
                    problems.push(format!(
                        "{} reads {}, which is not in CLOSURE of tools/build_guest_staged.sh",
                        file.strip_prefix(root()).unwrap().display(),
                        target.display()
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_staged_guest_closure_holds_the_profile_the_build_scripts_look_for() {
    assert!(
        closure().iter().any(|entry| entry == "sdk/profile.toml"),
        "the build scripts read sdk/profile.toml above the crate; CLOSURE must stage it"
    );
}
