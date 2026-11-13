//! G1: the feature check every SDK crate that has features runs from its `build.rs`.
//!
//! A guest (MIPS) build of the crate stops when it is built with a feature that no table of
//! `sdk/profile.toml` lists and that the program being built holds no exception for. A build
//! script sees only its own crate's features, not the program, so it learns the program from two
//! environment variables: [`FILE_VAR`] (the program's own `psx-gate.toml`) and [`PROGRAM_VAR`]
//! (a program inside the SDK repository). With neither set only the tables count, so a program
//! that holds a feature exception and is built by plain cargo is refused until it names its
//! file. What this check does not do, which `psx-gate` (run by `psoxide-pgo apply`) does: read
//! the file's counts, find a stale entry, or tell whether the file belongs to the program being
//! built. Host builds (tests, tools) are not checked, and neither is a `cargo clippy` run (it
//! links no program).

use crate::profile::{declared_features, Profile};
use std::path::Path;

/// The environment variable naming the program's own `psx-gate.toml`. `psoxide-pgo` sets it for
/// the builds it runs; a program built by plain cargo sets it in its `.cargo/config.toml`
/// (`[env]`, `relative = true`).
pub const FILE_VAR: &str = "PSX_GATE_FILE";
/// The environment variable naming a program inside the SDK repository, whose exceptions in
/// `sdk/profile.toml` apply (`psoxide-pgo` sets it for those programs).
pub const PROGRAM_VAR: &str = "PSX_GATE_PROGRAM";

/// Who a build is for, as far as a build script can know it.
#[derive(Default)]
#[non_exhaustive]
pub struct Own {
    /// The program's own `psx-gate.toml`.
    pub file: Option<Profile>,
    /// The program inside the SDK repository whose entries in `sdk/profile.toml` apply.
    pub local: Option<String>,
}

#[must_use]
/// The refusals for `krate` built with `enabled` features: a feature no table of the profile
/// lists and no exception of the program being built grants.
pub fn refusals(krate: &str, enabled: &[String], profile: &Profile, own: &Own) -> Vec<String> {
    enabled
        .iter()
        .filter(|feature| {
            !profile.lists_feature(krate, feature)
                && !own
                    .file
                    .as_ref()
                    .is_some_and(|file| file.feature_excepted(krate, feature, None))
                && !own.local.as_deref().is_some_and(|program| {
                    profile.feature_excepted(krate, feature, Some(program))
                })
        })
        .map(|feature| {
            format!(
                "psx-gate: {krate}: feature `{feature}` is not in sdk/profile.toml and the program \
                 being built has no exception for it; STANDARD.md rule 2 (no cargo feature selects \
                 a different implementation of an SDK job). Use the SDK's one implementation, or if \
                 it is a development tap add it under [diag]. A program that holds an exception \
                 builds with `psoxide-pgo apply`, or names its psx-gate.toml in {FILE_VAR}. Who \
                 enables it: `cargo tree -e features -i {krate}`."
            )
        })
        .collect()
}

/// The body of an SDK crate's `build.rs`: refuse an unlisted feature of a guest build.
///
/// # Panics
/// When cargo does not set `CARGO_MANIFEST_DIR` or `CARGO_PKG_NAME`, which it always does for a
/// build script.
pub fn run_build_script() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC_WORKSPACE_WRAPPER");
    println!("cargo:rerun-if-env-changed={FILE_VAR}");
    println!("cargo:rerun-if-env-changed={PROGRAM_VAR}");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let manifest = Path::new(&manifest_dir);
    let profile_path = match Profile::find_from(manifest) {
        Ok(path) => path,
        Err(e) => fail(&[format!("psx-gate: {e}")]),
    };
    println!("cargo:rerun-if-changed={}", profile_path.display());
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("mips") {
        return;
    }
    // `cargo clippy --all-features` (make lint) turns every feature on for the guest crates and
    // links nothing: no program comes out of a clippy run, so there is nothing to refuse.
    let wrapper = std::env::var_os("RUSTC_WORKSPACE_WRAPPER").unwrap_or_default();
    if Path::new(&wrapper)
        .file_stem()
        .is_some_and(|stem| stem == "clippy-driver")
    {
        return;
    }
    let profile =
        Profile::read(&profile_path).unwrap_or_else(|e| fail(&[format!("psx-gate: {e}")]));
    let krate = std::env::var("CARGO_PKG_NAME").expect("cargo sets CARGO_PKG_NAME");
    let text = std::fs::read_to_string(manifest.join("Cargo.toml")).unwrap_or_default();
    let enabled: Vec<String> = declared_features(&text)
        .into_iter()
        .filter(|f| {
            std::env::var_os(format!(
                "CARGO_FEATURE_{}",
                f.to_uppercase().replace('-', "_")
            ))
            .is_some()
        })
        .collect();
    let mut own = Own {
        local: std::env::var(PROGRAM_VAR)
            .ok()
            .filter(|name| !name.is_empty()),
        file: None,
    };
    if let Some(file) = std::env::var_os(FILE_VAR).filter(|file| !file.is_empty()) {
        let file = Path::new(&file);
        println!("cargo:rerun-if-changed={}", file.display());
        own.file = Some(
            std::fs::read_to_string(file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))
                .and_then(|text| Profile::parse_program_file(&text, file))
                .unwrap_or_else(|e| fail(&[format!("psx-gate: {FILE_VAR}: {e}")])),
        );
    }
    let found = refusals(&krate, &enabled, &profile, &own);
    if !found.is_empty() {
        fail(&found);
    }
}

fn fail(lines: &[String]) -> ! {
    for line in lines {
        eprintln!("{line}");
    }
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile::parse(
            "[shipping]\npsx-rt = [\"runtime\"]\n[diag]\npsx-rt = [\"host-tty\"]\n\
             [[exception]]\nprogram = \"example\"\nkind = \"feature\"\nitem = \"psx-pad/irq-engine\"\nremove = \"W2\"\n",
            Path::new("p"),
        )
        .unwrap()
    }

    fn program_file() -> Own {
        Own {
            file: Some(
                Profile::parse_program_file(
                    "[[exception]]\nkind = \"feature\"\nitem = \"psx-rt/alloc\"\nremove = \"W9\"\n",
                    Path::new("psx-gate.toml"),
                )
                .unwrap(),
            ),
            local: None,
        }
    }

    #[test]
    fn a_listed_feature_passes() {
        let f = vec!["runtime".to_string(), "host-tty".to_string()];
        assert!(refusals("psx-rt", &f, &profile(), &Own::default()).is_empty());
    }

    #[test]
    fn an_excepted_feature_passes_only_for_the_program_that_holds_the_exception() {
        let irq = ["irq-engine".to_string()];
        // A program inside the SDK repository, named by the environment.
        let local = Own {
            file: None,
            local: Some("example".into()),
        };
        assert!(refusals("psx-pad", &irq, &profile(), &local).is_empty());
        let other = Own {
            file: None,
            local: Some("another".into()),
        };
        assert_eq!(refusals("psx-pad", &irq, &profile(), &other).len(), 1);
        // No program named at all: only the tables count.
        assert_eq!(
            refusals("psx-pad", &irq, &profile(), &Own::default()).len(),
            1
        );
        // A program's own file.
        let alloc = ["alloc".to_string()];
        assert!(refusals("psx-rt", &alloc, &profile(), &program_file()).is_empty());
        assert_eq!(
            refusals("psx-rt", &alloc, &profile(), &Own::default()).len(),
            1
        );
    }

    #[test]
    fn an_unlisted_feature_is_refused_with_the_rule_and_the_way_to_find_it() {
        let found = refusals(
            "psx-rt",
            &["strict".to_string()],
            &profile(),
            &program_file(),
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("psx-rt: feature `strict`") && found[0].contains("rule 2"));
        assert!(found[0].contains("cargo tree -e features -i psx-rt"));
        // The same name under another crate is a different feature.
        assert_eq!(
            refusals(
                "psx-pad",
                &["runtime".to_string()],
                &profile(),
                &Own::default()
            )
            .len(),
            1
        );
    }
}
