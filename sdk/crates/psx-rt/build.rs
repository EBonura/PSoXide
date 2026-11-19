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

/// The build variable that sets the interrupt dispatcher's stack size (bytes, saved context included).
const STACK_VAR: &str = "PSX_DISPATCH_STACK_BYTES";
/// The size when the variable is unset.
const DEFAULT_STACK_BYTES: usize = 512;
/// The dispatcher saves 112 bytes of context at the top of its stack; the smallest size accepted leaves 16 for
/// a handler. What a program's handlers really need is for the stack guard to say (sdk/docs/INTERRUPTS.md).
const MIN_STACK_BYTES: usize = 128;
/// The largest size accepted: a bound on `.bss`, far above any handler's measured need.
const MAX_STACK_BYTES: usize = 4096;
/// The stack is 16-byte aligned, so any other size would only reserve padding.
const STACK_STEP: usize = 16;

fn main() {
    features::run_build_script();
    // The capacity is generated into the crate's source, not read with `option_env!`, so a compiler
    // cache keys on it like on any other source text.
    println!("cargo:rerun-if-env-changed={WORDS_VAR}");
    println!("cargo:rerun-if-env-changed={STACK_VAR}");
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
    let bytes = match std::env::var(STACK_VAR) {
        Ok(text) => match text.trim().parse::<usize>() {
            Ok(bytes)
                if (MIN_STACK_BYTES..=MAX_STACK_BYTES).contains(&bytes)
                    && bytes.is_multiple_of(STACK_STEP) =>
            {
                bytes
            }
            _ => panic!(
                "{STACK_VAR}={text:?}: expected a multiple of {STACK_STEP} from {MIN_STACK_BYTES} \
                 to {MAX_STACK_BYTES}"
            ),
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_STACK_BYTES,
        Err(error) => panic!("{STACK_VAR}: {error}"),
    };
    std::fs::write(
        out.join("dispatch_stack_bytes.rs"),
        format!("const DISPATCH_STACK_BYTES: usize = {bytes};\n"),
    )
    .expect("write dispatch_stack_bytes.rs");
}
