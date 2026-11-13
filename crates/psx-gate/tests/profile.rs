//! The profile against the tree it guards: every crate that has features runs the build-script
//! check, every feature the profile lists exists, and every exception is well formed. (A
//! feature no table lists is refused by the build script when anything enables it; that is the
//! rule, not a gap.)

use psx_gate::profile::{declared_features, Profile, COUNTED, KINDS};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn profile() -> Profile {
    Profile::read(&root().join("sdk/profile.toml")).expect("sdk/profile.toml parses")
}

fn crate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for parent in ["sdk/crates", "engine/crates", "crates"] {
        for entry in fs::read_dir(root().join(parent))
            .expect("crate directory")
            .flatten()
        {
            if entry.path().join("Cargo.toml").is_file() {
                dirs.push(entry.path());
            }
        }
    }
    dirs.sort();
    dirs
}

#[test]
fn every_featured_crate_runs_the_check() {
    let mut problems = Vec::new();
    for dir in crate_dirs() {
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let features = declared_features(&manifest);
        if features.is_empty() {
            continue;
        }
        let script = fs::read_to_string(dir.join("build.rs")).unwrap_or_default();
        if !script.contains("features::run_build_script()") {
            problems.push(format!(
                "{name}: has features but no build.rs running the gate"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_listed_feature_exists() {
    let profile = profile();
    let mut declared = BTreeSet::new();
    for dir in crate_dirs() {
        let manifest = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        for feature in declared_features(&manifest) {
            declared.insert(format!("{name}/{feature}"));
        }
    }
    let mut stale = Vec::new();
    for (table, list) in [
        ("shipping", &profile.shipping),
        ("diag", &profile.diag),
        ("host", &profile.host),
    ] {
        for (krate, features) in list {
            for feature in features {
                if !declared.contains(&format!("{krate}/{feature}")) {
                    stale.push(format!(
                        "[{table}] {krate}/{feature} is not a feature of any crate"
                    ));
                }
            }
        }
    }
    assert!(stale.is_empty(), "{}", stale.join("\n"));
}

/// The package names of the programs in this repository (every example directory holds a
/// package of that name).
fn local_programs() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for parent in ["sdk/examples", "engine/examples"] {
        for entry in fs::read_dir(root().join(parent))
            .expect("examples")
            .flatten()
        {
            if entry.path().join("Cargo.toml").is_file() {
                names.insert(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    names
}

#[test]
fn exceptions_are_well_formed_unique_and_about_programs_of_this_repository() {
    let profile = profile();
    let local = local_programs();
    let mut seen = BTreeSet::new();
    for e in &profile.exceptions {
        let who = format!("{} {} {}", e.program, e.kind, e.item);
        assert!(KINDS.contains(&e.kind.as_str()), "{who}: unknown kind");
        assert!(
            local.contains(&e.program),
            "{who}: sdk/profile.toml lists only programs of this repository; any other program \
             lists itself in its own psx-gate.toml"
        );
        assert_eq!(
            e.count.is_some(),
            COUNTED.contains(&e.kind.as_str()),
            "{who}: a count belongs to the line kinds only"
        );
        assert!(
            e.count.is_none_or(|n| n > 0),
            "{who}: a zero count is a stale entry"
        );
        assert!(
            tagged(&e.remove),
            "{who}: remove is a short tag (W<n>, unscheduled or decision)"
        );
        assert!(seen.insert(who.clone()), "{who}: listed twice");
    }
}

fn tagged(remove: &str) -> bool {
    remove == "unscheduled"
        || remove == "decision"
        || remove
            .strip_prefix('W')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn probes_are_programs_of_this_repository_and_get_no_line_entries() {
    let profile = profile();
    let local = local_programs();
    for name in &profile.probe {
        assert!(local.contains(name), "probe {name} is not an example here");
        for e in profile.exceptions.iter().filter(|e| &e.program == name) {
            assert!(
                !["register-literal", "vector-write", "cop0-asm"].contains(&e.kind.as_str()),
                "{name} is a probe: its {} entry for {} is dead",
                e.kind,
                e.item
            );
        }
    }
}

#[test]
fn deprecated_symbols_are_still_defined_in_the_sdk() {
    let profile = profile();
    let mut sources = String::new();
    for parent in ["sdk/crates", "engine/crates", "crates"] {
        for dir in fs::read_dir(root().join(parent)).expect("dir").flatten() {
            sources += &fs::read_to_string(dir.path().join("src/lib.rs")).unwrap_or_default();
        }
    }
    for symbol in &profile.deprecated {
        assert!(
            sources.contains(&format!("fn {symbol}")) || sources.contains(&format!("struct {symbol}")),
            "deprecated symbol {symbol} is not defined in any SDK crate: delete it from [deprecated]"
        );
    }
}
