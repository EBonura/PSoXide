//! The architecture gate's feature check (sdk/profile.toml; sdk/docs/GATE.md).
#![allow(dead_code, missing_docs)]
#[path = "../../../crates/psx-gate/src/features.rs"]
mod features;
#[path = "../../../crates/psx-gate/src/profile.rs"]
mod profile;

/// The build variable that sets the post-link hazard patcher's trampoline capacity (code words).
const WORDS_VAR: &str = "PSX_HAZARD_TRAMPOLINE_WORDS";
/// The capacity when the variable is unset.
const DEFAULT_WORDS: usize = 96;
/// The largest capacity accepted: a bound on `.data`, far above any program's measured need.
const MAX_WORDS: usize = 1024;

fn main() {
    features::run_build_script();
    // The capacity is generated into the crate's source, not read with `option_env!`, so a compiler
    // cache keys on it like on any other source text.
    println!("cargo:rerun-if-env-changed={WORDS_VAR}");
    let words = match std::env::var(WORDS_VAR) {
        Ok(text) => match text.trim().parse::<usize>() {
            Ok(words) if (1..=MAX_WORDS).contains(&words) => words,
            _ => panic!("{WORDS_VAR}={text:?}: expected a whole number from 1 to {MAX_WORDS}"),
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_WORDS,
        Err(error) => panic!("{WORDS_VAR}: {error}"),
    };
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(
        out.join("hazard_trampoline_words.rs"),
        format!("const HAZARD_TRAMPOLINE_WORDS: usize = {words};\n"),
    )
    .expect("write hazard_trampoline_words.rs");
}
