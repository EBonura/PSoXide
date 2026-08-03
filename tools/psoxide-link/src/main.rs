//! Hydrate a pinned PSoXide source tree into a game repository.
//!
//! Games that build against this SDK have to reach it somehow, and the demo
//! disc had grown three answers. Half-Life pinned a git rev in Cargo.lock and
//! copied the resolved checkout into a local cache. NitroXide, PSXcel, the
//! Celeste collection and GH-PSX each vendored the whole repository as a
//! submodule. VoXide used a relative path that only resolves inside the demo
//! disc superproject.
//!
//! The vendored ones drifted, which is the argument. Bumping four submodules by
//! hand is a chore nobody does, so three of them sat eight commits behind a
//! measured SPU fix. This is Half-Life's answer, generalised, so no game has to
//! write it again:
//!
//! * Cargo owns the pin. A git dependency on a crate from this repository puts
//!   a rev in the game's lockfile, and cargo caches the checkout once per
//!   machine rather than once per game.
//! * `cargo metadata` says where that checkout landed, and this copies it to a
//!   stable path so plain `path = "..."` dependencies and the linker script
//!   both resolve.
//! * `--from` overrides the pin with a working tree, which is what lets the
//!   demo disc force every program onto one SDK regardless of what each game
//!   pinned for itself.
//!
//! It is a build step rather than a `build.rs` because Cargo resolves manifests
//! before it runs build scripts: by the time a build script could hydrate
//! anything, the path dependencies it was meant to satisfy have already failed
//! to resolve.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The file that proves a directory really is a PSoXide source tree, checked
/// before anything is copied and again before a cache is trusted. Games link
/// against it directly, so its absence is the failure that matters.
const SENTINEL: &str = "sdk/psoxide.ld";

/// Written into the hydrated tree to record what it was made from, so an
/// unchanged pin costs a string compare instead of a recursive copy.
const MARKER: &str = ".psoxide-source";

/// Default crate to look for in `cargo metadata`. Any crate from this
/// repository would anchor the resolution; this one is small and stable.
const DEFAULT_PIN_CRATE: &str = "psx-iso";

/// Never copied: build output and version control, which are large, and
/// content directories that belong to whoever authored them.
fn skip(relative: &Path) -> bool {
    let mut components = relative.components();
    let top = components
        .next()
        .and_then(|c| c.as_os_str().to_str());
    matches!(
        top,
        Some(".git" | "target" | "captures" | "dist" | "data" | "graphify-out")
    ) || components.any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "target")))
}

fn copy_tree(source: &Path, destination: &Path, relative: &Path) -> Result<u64> {
    let mut copied = 0;
    for entry in fs::read_dir(source.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        if skip(&child) {
            continue;
        }
        let target = destination.join(&child);
        let ty = entry.file_type()?;
        if ty.is_dir() {
            fs::create_dir_all(&target)?;
            copied += copy_tree(source, destination, &child)?;
        } else if ty.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Ask Cargo where it put the pinned checkout.
///
/// The rev is matched against the package's source URL rather than trusted
/// from the command line alone, so a stale `--rev` fails loudly here instead
/// of silently hydrating whatever else Cargo happened to resolve.
fn pinned_checkout(workspace: &Path, pin_crate: &str, rev: &str) -> Result<PathBuf> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace)
        .args(["metadata", "--format-version", "1"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata did not contain packages")?;
    for package in packages {
        if package["name"].as_str() != Some(pin_crate) {
            continue;
        }
        if !package["source"].as_str().unwrap_or_default().contains(rev) {
            continue;
        }
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .ok_or("package metadata omitted manifest_path")?,
        );
        // <checkout>/<subdir>/<crate>/Cargo.toml -> <checkout>
        return manifest
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("unexpected manifest path {}", manifest.display()).into());
    }
    Err(format!(
        "cargo has not resolved {pin_crate} at {rev}. The game's manifest needs a git \
         dependency on it pinned to that rev, or pass --from to use a working tree."
    )
    .into())
}

fn verify_tree(path: &Path) -> Result<()> {
    if path.join(SENTINEL).is_file() {
        return Ok(());
    }
    Err(format!(
        "{} is not a PSoXide source tree ({SENTINEL} is missing)",
        path.display()
    )
    .into())
}

struct Options {
    from: Option<PathBuf>,
    into: PathBuf,
    workspace: PathBuf,
    pin_crate: String,
    rev: Option<String>,
    quiet: bool,
}

fn usage() -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "psoxide-link -- hydrate a pinned PSoXide source tree\n\
         \n\
             --from PATH       use this working tree instead of the Cargo pin\n\
             --into DIR        where to hydrate (default .psoxide)\n\
             --workspace DIR   where to run cargo metadata (default .)\n\
             --pin-crate NAME  crate anchoring the pin (default {DEFAULT_PIN_CRATE})\n\
             --rev SHA         the pinned rev, required unless --from is given\n\
             --quiet           only report when work is actually done\n"
    );
    s
}

fn parse(mut args: env::Args) -> Result<Options> {
    let mut options = Options {
        from: None,
        into: PathBuf::from(".psoxide"),
        workspace: PathBuf::from("."),
        pin_crate: DEFAULT_PIN_CRATE.into(),
        rev: None,
        quiet: false,
    };
    let _ = args.next();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| -> Box<dyn std::error::Error> { format!("{arg} needs a value").into() })
        };
        match arg.as_str() {
            "--from" => options.from = Some(PathBuf::from(value()?)),
            "--into" => options.into = PathBuf::from(value()?),
            "--workspace" => options.workspace = PathBuf::from(value()?),
            "--pin-crate" => options.pin_crate = value()?,
            "--rev" => options.rev = Some(value()?),
            "--quiet" => options.quiet = true,
            "-h" | "--help" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}\n\n{}", usage()).into()),
        }
    }
    Ok(options)
}

fn run(options: &Options) -> Result<()> {
    // Identity, not just a path: a hydrated tree is only reusable if it came
    // from the same place. A local tree is never reused, because a working
    // tree changes under you and a marker cannot see that.
    let (source, id, reusable) = match &options.from {
        Some(path) => {
            let path = path.canonicalize()?;
            verify_tree(&path)?;
            let id = format!("local:{}", path.display());
            (path, id, false)
        }
        None => {
            let rev = options
                .rev
                .as_deref()
                .ok_or("--rev is required unless --from is given")?;
            let path = pinned_checkout(&options.workspace, &options.pin_crate, rev)?;
            verify_tree(&path)?;
            (path, format!("git:{rev}"), true)
        }
    };

    let marker = options.into.join(MARKER);
    let fresh = reusable
        && fs::read_to_string(&marker).ok().as_deref() == Some(id.as_str())
        && options.into.join(SENTINEL).is_file();
    if fresh {
        if !options.quiet {
            println!("psoxide-link: {} already at {id}", options.into.display());
        }
        return Ok(());
    }

    if options.into.exists() {
        fs::remove_dir_all(&options.into)?;
    }
    fs::create_dir_all(&options.into)?;
    let files = copy_tree(&source, &options.into, Path::new(""))?;
    // The marker goes down last, so an interrupted copy is not mistaken for a
    // complete one on the next run.
    fs::write(&marker, &id)?;
    println!(
        "psoxide-link: hydrated {id} into {} ({files} files)",
        options.into.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let options = match parse(env::args()) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("psoxide-link: {error}");
            return ExitCode::FAILURE;
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("psoxide-link: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_and_version_control_are_not_copied() {
        assert!(skip(Path::new(".git")));
        assert!(skip(Path::new("target")));
        assert!(skip(Path::new("sdk/target")));
        assert!(skip(Path::new("engine/examples/.git")));
    }

    #[test]
    fn the_sdk_itself_is_copied() {
        assert!(!skip(Path::new("sdk")));
        assert!(!skip(Path::new("sdk/crates/psx-sfx/src/lib.rs")));
        assert!(!skip(Path::new("sdk/psoxide.ld")));
        // "data" is skipped only at the top level: a crate's own data
        // directory is source, and dropping it would hydrate a broken tree.
        assert!(skip(Path::new("data")));
        assert!(!skip(Path::new("sdk/crates/psx-font/data/basic.png")));
    }
}
