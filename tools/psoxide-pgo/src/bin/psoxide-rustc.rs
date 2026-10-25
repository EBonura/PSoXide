//! A `RUSTC_WRAPPER` that makes a guest's crate identities independent of
//! where the repository is checked out.
//!
//! Cargo passes every compilation `-C metadata=<hash>`, and rustc derives the
//! crate's disambiguator, and so every mangled symbol, from it. For a path
//! dependency that sits outside the workspace root (a game's
//! `../.psoxide/sdk/crates/...`), Cargo hashes the dependency's absolute
//! path. The same commit checked out in two directories therefore names every
//! function differently, rustc and LLVM order items by those names, and the
//! two guest executables come out the same size with different code and data
//! layout: 85,544 bytes differed between two OoT checkouts, and the layout
//! alone moved one measured window by 0.6 percent.
//!
//! This wrapper replaces that one value with a hash of what identifies the
//! compilation, with the checkout root factored out of the paths: the crate
//! name and type, the edition, the target, the `--cfg` set (features), and
//! the source path relative to the root. Everything else on the command line
//! (rustflags, profile-guided flags, debug info, output and link paths) is
//! left out on purpose, exactly as Cargo leaves rustflags out of its own
//! hash: `psoxide-pgo` relies on a profile-guided build and the DWARF twin it
//! binds the profile to carrying the same symbol names.
//!
//! Usage, from a game's build script:
//!
//! ```text
//! export PSOXIDE_REPRO_ROOT=<the directory every path dependency is under>
//! export RUSTC_WRAPPER=<path to psoxide-rustc>
//! export PSOXIDE_REPRO_INNER=<a compiler cache such as sccache, optional>
//! ```
//!
//! The root is a required input: a wrapper that silently did nothing would
//! look like a reproducible build and not be one, so a missing root stops the
//! build. Sources outside the root are accepted only under Cargo's registry
//! directory and the toolchain's `rust-src`, whose paths do not depend on the
//! checkout; any other absolute path outside the root stops the build too,
//! because it would put a path back into the hash.
//!
//! `cargo` also calls the wrapper to probe the compiler (`-vV`, `--print`);
//! those calls, and any compilation without `-C metadata`, pass through
//! untouched.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The directory every path dependency of the build lives under.
const ROOT_VAR: &str = "PSOXIDE_REPRO_ROOT";
/// An optional second wrapper to run the compiler under, such as `sccache`.
const INNER_VAR: &str = "PSOXIDE_REPRO_INNER";
/// Path fragments after which a source path no longer depends on the machine
/// or on the checkout: the Cargo registry and the toolchain's library source.
const STABLE_MARKERS: [&str; 2] = ["/registry/src/", "/lib/rustlib/src/"];

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("psoxide-rustc: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.is_empty() {
        return Err("expected the compiler as the first argument (RUSTC_WRAPPER use)".into());
    }
    let roots = Roots::from_env()?;
    // Compiler and arguments as text for inspection; the originals are what
    // gets executed, so a non-UTF-8 argument is passed through unchanged.
    let text: Vec<Option<String>> = args.iter().map(|a| a.to_str().map(str::to_owned)).collect();
    if let Some(index) = metadata_index(&text) {
        let identity = identity_of(&text[1..], &roots)?;
        args[index] = OsString::from(format!("metadata={}", fingerprint(&identity)));
    }
    Ok(launch(args))
}

#[cfg(unix)]
fn launch(args: Vec<OsString>) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let mut command = command_for(args);
    let error = command.exec();
    eprintln!("psoxide-rustc: cannot run the compiler: {error}");
    ExitCode::from(2)
}

#[cfg(not(unix))]
fn launch(args: Vec<OsString>) -> ExitCode {
    match command_for(args).status() {
        Ok(status) => status
            .code()
            .map_or(ExitCode::from(2), |c| ExitCode::from(c as u8)),
        Err(error) => {
            eprintln!("psoxide-rustc: cannot run the compiler: {error}");
            ExitCode::from(2)
        }
    }
}

/// The compiler command, behind the inner wrapper when one is configured.
fn command_for(args: Vec<OsString>) -> Command {
    match env::var_os(INNER_VAR).filter(|inner| !inner.is_empty()) {
        Some(inner) => {
            let mut command = Command::new(inner);
            command.args(args);
            command
        }
        None => {
            let mut args = args.into_iter();
            let mut command = Command::new(args.next().expect("checked non-empty"));
            command.args(args);
            command
        }
    }
}

/// The checkout root, as given and with symbolic links resolved: Cargo hands
/// rustc paths in whichever spelling it was started with.
struct Roots {
    spellings: Vec<String>,
}

impl Roots {
    fn from_env() -> Result<Self, String> {
        let root = env::var_os(ROOT_VAR)
            .filter(|root| !root.is_empty())
            .ok_or_else(|| format!("{ROOT_VAR} is not set; refusing to build unpinned"))?;
        let root = PathBuf::from(root);
        let mut spellings = vec![root.to_string_lossy().trim_end_matches('/').to_owned()];
        if let Ok(resolved) = root.canonicalize() {
            let resolved = resolved.to_string_lossy().trim_end_matches('/').to_owned();
            if !spellings.contains(&resolved) {
                spellings.push(resolved);
            }
        }
        Ok(Self { spellings })
    }

    /// `path` relative to the root, when it is under it.
    fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        self.spellings.iter().find_map(|root| {
            path.strip_prefix(root.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
        })
    }
}

/// Position of the value of `-C metadata=...` (the argument after `-C`, or a
/// single `-Cmetadata=...`), in `args`, where `args[0]` is the compiler.
fn metadata_index(args: &[Option<String>]) -> Option<usize> {
    args.iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, arg)| match arg.as_deref() {
            Some("-C") if args.get(i + 1)?.as_deref()?.starts_with("metadata=") => Some(i + 1),
            _ => None,
        })
}

/// What identifies a compilation, as stable text. `args` excludes the compiler.
fn identity_of(args: &[Option<String>], roots: &Roots) -> Result<String, String> {
    let mut name = "";
    let mut edition = "";
    let mut target = "";
    let mut crate_types = BTreeSet::new();
    let mut cfgs = BTreeSet::new();
    let mut source = None;
    let mut i = 0;
    while i < args.len() {
        let Some(arg) = args[i].as_deref() else {
            i += 1;
            continue;
        };
        let mut value = |flag: &str| -> Option<&str> {
            if let Some(joined) = arg
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
            {
                return Some(joined);
            }
            if arg == flag {
                i += 1;
                return args.get(i).and_then(|v| v.as_deref());
            }
            None
        };
        if let Some(v) = value("--crate-name") {
            name = v;
        } else if let Some(v) = value("--edition") {
            edition = v;
        } else if let Some(v) = value("--target") {
            target = v;
        } else if let Some(v) = value("--crate-type") {
            crate_types.insert(v);
        } else if let Some(v) = value("--cfg") {
            cfgs.insert(v);
        } else if source.is_none() && arg.ends_with(".rs") && !arg.starts_with('-') {
            source = Some(arg);
        }
        i += 1;
    }
    if name.is_empty() {
        return Err("a compilation with -C metadata and no --crate-name".into());
    }
    let source = source
        .map(|path| portable_source(path, roots))
        .transpose()?
        .unwrap_or_default();
    let mut identity = format!("name={name}\nedition={edition}\ntarget={target}\nsrc={source}\n");
    for crate_type in crate_types {
        identity.push_str(&format!("type={crate_type}\n"));
    }
    for cfg in cfgs {
        identity.push_str(&format!("cfg={cfg}\n"));
    }
    Ok(identity)
}

/// A source path with the checkout location removed.
fn portable_source(path: &str, roots: &Roots) -> Result<String, String> {
    if !Path::new(path).is_absolute() {
        // Relative to the directory Cargo runs rustc in, which is the
        // workspace root and so already independent of the checkout.
        return Ok(path.trim_start_matches("./").to_owned());
    }
    if let Some(relative) = roots.relative(path) {
        return Ok(format!("<root>/{relative}"));
    }
    for marker in STABLE_MARKERS {
        if let Some(at) = path.find(marker) {
            return Ok(path[at..].to_owned());
        }
    }
    Err(format!(
        "source {path} is outside {ROOT_VAR} ({}) and not in a registry or rust-src; its \
         absolute path would enter the crate hash",
        roots.spellings[0]
    ))
}

/// A 64-bit FNV-1a of the identity, as the 16 hex digits Cargo's own value
/// has. Collisions only matter between crates of the same name in one link.
fn fingerprint(identity: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in identity.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(spellings: &[&str]) -> Roots {
        Roots {
            spellings: spellings.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn args(list: &[&str]) -> Vec<Option<String>> {
        list.iter().map(|s| Some((*s).to_owned())).collect()
    }

    fn compile(root: &str, target_dir: &str, extra: &[&str]) -> Vec<Option<String>> {
        let mut list = vec![
            "--crate-name".to_owned(),
            "psx_gpu".to_owned(),
            "--edition=2021".to_owned(),
            format!("{root}/.psoxide/sdk/crates/psx-gpu/src/lib.rs"),
            "--crate-type".to_owned(),
            "lib".to_owned(),
            "--cfg".to_owned(),
            "feature=\"std\"".to_owned(),
            "--out-dir".to_owned(),
            format!("{target_dir}/deps"),
            "-C".to_owned(),
            "metadata=0123456789abcdef".to_owned(),
            "-C".to_owned(),
            "extra-filename=-0123456789abcdef".to_owned(),
        ];
        list.extend(extra.iter().map(|s| (*s).to_owned()));
        list.into_iter().map(Some).collect()
    }

    fn id(root: &str, target_dir: &str, extra: &[&str]) -> String {
        fingerprint(&identity_of(&compile(root, target_dir, extra), &roots(&[root])).unwrap())
    }

    #[test]
    fn identity_ignores_the_checkout_and_target_directory() {
        assert_eq!(
            id("/work/a/oot", "/t/a", &[]),
            id("/elsewhere/deeper/checkout", "/t/b", &[])
        );
    }

    #[test]
    fn identity_ignores_rustflags_that_cargo_also_ignores() {
        let base = id("/work/a", "/t", &[]);
        let flagged = [
            "-Cdebuginfo=1",
            "-Cstrip=none",
            "-Zprofile-sample-use=/p/rebound.prof",
        ];
        assert_eq!(base, id("/work/a", "/t", &flagged));
    }

    #[test]
    fn identity_changes_with_features_target_and_name() {
        let base = identity_of(&compile("/w", "/t", &[]), &roots(&["/w"])).unwrap();
        let other_cfg = identity_of(
            &compile("/w", "/t", &["--cfg", "feature=\"x\""]),
            &roots(&["/w"]),
        );
        let other_target = identity_of(
            &compile("/w", "/t", &["--target", "mipsel"]),
            &roots(&["/w"]),
        );
        assert_ne!(base, other_cfg.unwrap());
        assert_ne!(base, other_target.unwrap());
        let mut renamed = compile("/w", "/t", &[]);
        renamed[1] = Some("psx_gte".to_owned());
        assert_ne!(base, identity_of(&renamed, &roots(&["/w"])).unwrap());
    }

    #[test]
    fn root_matches_every_spelling() {
        let both = roots(&["/tmp/x", "/private/tmp/x"]);
        assert_eq!(both.relative("/private/tmp/x/a/b.rs"), Some("a/b.rs"));
        assert_eq!(both.relative("/tmp/x/a/b.rs"), Some("a/b.rs"));
        assert_eq!(both.relative("/tmp/xy/a.rs"), None);
    }

    #[test]
    fn registry_and_rust_src_paths_lose_the_machine_prefix() {
        let r = roots(&["/w"]);
        let registry = "/Users/a/.cargo/registry/src/index.crates.io-1949/bitflags-2/src/lib.rs";
        let other_home = "/home/b/.cargo/registry/src/index.crates.io-1949/bitflags-2/src/lib.rs";
        assert_eq!(
            portable_source(registry, &r),
            portable_source(other_home, &r)
        );
        let core = "/x/toolchain/lib/rustlib/src/rust/library/core/src/lib.rs";
        assert_eq!(
            portable_source(core, &r).unwrap(),
            "/lib/rustlib/src/rust/library/core/src/lib.rs"
        );
    }

    #[test]
    fn a_stray_absolute_path_is_refused() {
        let error = portable_source("/Users/me/other/src/lib.rs", &roots(&["/w"])).unwrap_err();
        assert!(error.contains("outside PSOXIDE_REPRO_ROOT"), "{error}");
    }

    #[test]
    fn relative_sources_are_taken_as_they_are() {
        assert_eq!(
            portable_source("./src/main.rs", &roots(&["/w"])).unwrap(),
            "src/main.rs"
        );
    }

    #[test]
    fn metadata_is_found_in_both_spellings_of_the_flag() {
        let split = args(&["rustc", "--crate-name", "a", "-C", "metadata=aa", "x.rs"]);
        assert_eq!(metadata_index(&split), Some(4));
        let probe = args(&["rustc", "-vV"]);
        assert_eq!(metadata_index(&probe), None);
    }

    #[test]
    fn fingerprint_is_sixteen_hex_digits_and_stable() {
        assert_eq!(fingerprint(""), "cbf29ce484222325");
        assert_eq!(fingerprint("a").len(), 16);
    }
}
