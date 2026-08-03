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
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Boxed-error result, since every failure here ends a build.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The file that proves a directory really is a PSoXide source tree, checked
/// before anything is copied and again before a cache is trusted. Games link
/// against it directly, so its absence is the failure that matters.
const SENTINEL: &str = "sdk/psoxide.ld";

/// Written into the hydrated tree to record what it was made from, so an
/// unchanged pin costs a string compare instead of a recursive copy.
const MARKER: &str = ".psoxide-source";

/// Never copied: build output and version control, which are large, and
/// content directories that belong to whoever authored them.
///
/// `build` is the one worth naming. It holds the SDK's own compiled examples
/// and runs to 583 MB, which was 71% of every hydrated tree and was being
/// copied once per game on every disc build. Nothing references it: a game
/// links source, and this workspace's own `build/` is reached through the
/// submodule rather than through anybody's cache.
///
/// `editor` is deliberately NOT skipped, tempting as its 26 MB is: the emulator
/// frontend's default features pull psxed-ui and psxed-project in by path, so
/// dropping it breaks `make run` in every game that has one.
fn skip(relative: &Path) -> bool {
    let mut components = relative.components();
    let top = components.next().and_then(|c| c.as_os_str().to_str());
    matches!(
        top,
        Some(".git" | "target" | "build" | "captures" | "dist" | "data" | "graphify-out")
    ) || components.any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "target")))
}

pub fn copy_tree(source: &Path, destination: &Path, relative: &Path) -> Result<u64> {
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
pub fn pinned_checkout(workspace: &Path, pin_crate: &str, rev: &str) -> Result<PathBuf> {
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

pub fn verify_tree(path: &Path) -> Result<()> {
    if path.join(SENTINEL).is_file() {
        return Ok(());
    }
    Err(format!(
        "{} is not a PSoXide source tree ({SENTINEL} is missing)",
        path.display()
    )
    .into())
}

/// The PSoXide checkout this build of psoxide-link came from.
///
/// When a game depends on this crate by git rev, Cargo puts the whole
/// repository in its own cache and compiles this out of it, so the SDK the
/// game pinned is by construction the one sitting two directories above this
/// manifest. That is what lets a game hydrate with nothing but a Cargo
/// dependency: no cargo metadata, no network beyond the fetch Cargo already
/// did, and no path into a tree that does not exist yet.
pub fn pinned_source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("psoxide-link lives at <checkout>/tools/psoxide-link")
        .to_path_buf()
}

/// Hydrate `source` into `destination`, skipping the copy when a marker shows
/// it is already there. `id` records what it came from; pass `None` for a
/// working tree, which is never reused because it can change underneath you.
pub fn hydrate(source: &Path, destination: &Path, id: Option<&str>, quiet: bool) -> Result<()> {
    verify_tree(source)?;
    let id = match id {
        Some(id) => id.to_string(),
        None => format!("local:{}", source.display()),
    };
    let reusable = id.starts_with("git:");
    let marker = destination.join(MARKER);
    if reusable
        && fs::read_to_string(&marker).ok().as_deref() == Some(id.as_str())
        && destination.join(SENTINEL).is_file()
    {
        if !quiet {
            println!("psoxide-link: {} already at {id}", destination.display());
        }
        return Ok(());
    }
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    let files = copy_tree(source, destination, Path::new(""))?;
    // Written last, so an interrupted copy is not mistaken for a finished one.
    fs::write(&marker, &id)?;
    println!(
        "psoxide-link: hydrated {id} into {} ({files} files)",
        destination.display()
    );
    Ok(())
}

/// Hydrate the checkout this crate was compiled from. The one call a game's
/// pin crate needs.
pub fn hydrate_pinned(destination: &Path, rev: &str, quiet: bool) -> Result<()> {
    hydrate(
        &pinned_source_root(),
        destination,
        Some(&format!("git:{rev}")),
        quiet,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_and_version_control_are_not_copied() {
        assert!(skip(Path::new(".git")));
        assert!(skip(Path::new("target")));
        assert!(skip(Path::new("sdk/target")));
        // 583 MB of the SDK's own compiled examples, once per game.
        assert!(skip(Path::new("build")));
        assert!(skip(Path::new("engine/examples/.git")));
    }

    #[test]
    fn the_sdk_itself_is_copied() {
        assert!(!skip(Path::new("sdk")));
        assert!(!skip(Path::new("sdk/crates/psx-sfx/src/lib.rs")));
        assert!(!skip(Path::new("sdk/psoxide.ld")));
        // The frontend's default features reach these by path, so a hydrated
        // tree without them cannot build `make run`.
        assert!(!skip(Path::new("editor/crates/psxed-ui/src/lib.rs")));
        assert!(!skip(Path::new("emu/crates/frontend/Cargo.toml")));
        // A crate called "build" is source, not the top-level output dir.
        assert!(!skip(Path::new("sdk/crates/psx-gpu/build")));
        // "data" is skipped only at the top level: a crate's own data
        // directory is source, and dropping it would hydrate a broken tree.
        assert!(skip(Path::new("data")));
        assert!(!skip(Path::new("sdk/crates/psx-font/data/basic.png")));
    }
}
