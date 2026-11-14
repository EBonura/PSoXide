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
    // Every source file of every crate: an entry point may live in a module (`psx_rt::interrupts`).
    fn read_sources(dir: &std::path::Path, sources: &mut String) {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                read_sources(&path, sources);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                *sources += &fs::read_to_string(&path).unwrap_or_default();
            }
        }
    }
    let mut sources = String::new();
    for parent in ["sdk/crates", "engine/crates", "crates"] {
        for dir in fs::read_dir(root().join(parent)).expect("dir").flatten() {
            read_sources(&dir.path().join("src"), &mut sources);
        }
    }
    for symbol in &profile.deprecated {
        // `crate::name` must be defined in that crate (`psx_pad` is the directory `psx-pad`).
        let (haystack, name) = match symbol.split_once("::") {
            Some((krate, name)) => {
                let dir = krate.replace('_', "-");
                let mut own = String::new();
                for parent in ["sdk/crates", "engine/crates", "crates"] {
                    own += &fs::read_to_string(root().join(parent).join(&dir).join("src/lib.rs"))
                        .unwrap_or_default();
                }
                (own, name)
            }
            None => (sources.clone(), symbol.as_str()),
        };
        assert!(
            haystack.contains(&format!("fn {name}")) || haystack.contains(&format!("struct {name}")),
            "deprecated symbol {symbol} is not defined in any SDK crate: delete it from [deprecated]"
        );
    }
}

/// A program of this repository that holds a feature exception is built by `psoxide-pgo apply`
/// (which tells the build scripts who it is) or by plain cargo with `PSX_GATE_PROGRAM=<name>`; a
/// recipe or script that builds it by plain cargo without naming it is refused by the first SDK
/// crate's build script, and nobody finds out until the recipe runs. This reads every make file,
/// script and workflow that builds guests, so a new recipe cannot forget it.
#[test]
fn recipes_that_build_a_program_with_feature_exceptions_by_plain_cargo_name_it() {
    let profile = profile();
    let programs: BTreeSet<&str> = profile
        .exceptions
        .iter()
        .filter(|e| e.kind == "feature")
        .map(|e| e.program.as_str())
        .collect();
    let mut files = vec![root().join("Makefile")];
    for (dir, ext) in [
        ("tools", "mk"),
        ("tools", "sh"),
        (".github/workflows", "yml"),
    ] {
        for entry in fs::read_dir(root().join(dir)).expect("dir").flatten() {
            if entry.path().extension().is_some_and(|e| e == ext) {
                files.push(entry.path());
            }
        }
    }
    let mut problems = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_default();
        for program in &programs {
            let names_it = format!("PSX_GATE_PROGRAM={program}");
            let dir = format!("examples/{program}");
            let builds = |line: &str| line.contains("cargo build") && !line.contains("psoxide-pgo");
            for line in text.lines().filter(|l| l.contains(&dir) && builds(l)) {
                if !line.contains(&names_it) {
                    problems.push(format!(
                        "{}: builds {program} without {names_it}: {}",
                        file.display(),
                        line.trim()
                    ));
                }
            }
            // A script that enters the program's directory (or keeps it in a variable) and builds
            // on another line.
            let enters = |line: &str| {
                line.contains(&dir) && (line.contains("cd ") || line.contains("GUEST_DIR="))
            };
            if text.lines().any(enters) && text.lines().any(builds) && !text.contains(&names_it) {
                problems.push(format!(
                    "{}: mentions {dir} and runs cargo build, but never sets {names_it}",
                    file.display()
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
