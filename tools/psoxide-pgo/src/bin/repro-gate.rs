//! Reproducibility gate: build one checkout from two different directories
//! and require byte-identical artifacts.
//!
//! A guest executable must depend only on the sources and the pins, never on
//! the path the repository happens to be checked out at, nor on the stage
//! directory a build copies sources into (see `psoxide-rustc`). This gate
//! proves it for any game: it clones the source tree twice, into directories
//! of different depth and name length, gives each clone its own output and
//! stage directories, runs the same build command in both, and compares the
//! named artifacts byte for byte.
//!
//! ```text
//! repro-gate --source <checkout> --build '<shell command>' \
//!            --artifact '{out}/game.exe' [--artifact ...] [--work DIR] [--keep]
//! ```
//!
//! The build command runs under `sh -c` in the clone, with
//! `PSOXIDE_REPRO_CHECKOUT` (the clone), `PSOXIDE_REPRO_OUT` (an output
//! directory outside every clone) and `PSOXIDE_GUEST_STAGE_ROOT` (a private
//! stage root) set. `{out}` and `{checkout}` in an `--artifact` are replaced
//! with the clone's output and checkout directories.
//!
//! Cargo build directories (`target/`) are not cloned, so every host tool is
//! rebuilt inside its own clone: a tool built elsewhere bakes the original
//! checkout path in (`env!("CARGO_MANIFEST_DIR")`) and would defeat the test.
//!
//! Exit status: 0 identical, 1 different, 2 usage or build failure.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::{env, fs};

/// Parent directories of the two clones. They differ in depth and in length
/// on purpose: a length-sensitive layout (aligned section sizes) shows up.
const VARIANTS: [(&str, &str); 2] = [
    ("a", "co"),
    ("b", "second/deeper/directory-with-a-longer-name"),
];

struct Options {
    source: PathBuf,
    build: String,
    artifacts: Vec<String>,
    work: PathBuf,
    keep: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("repro-gate: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let options = parse(env::args().skip(1))?;
    fs::create_dir_all(&options.work).map_err(|e| format!("{}: {e}", options.work.display()))?;
    let work = options.work.canonicalize().map_err(|e| e.to_string())?;
    let source = options
        .source
        .canonicalize()
        .map_err(|e| format!("source: {e}"))?;
    if work.starts_with(&source) {
        return Err("--work must not be inside --source".into());
    }
    let mut built: Vec<Vec<PathBuf>> = Vec::new();
    for (name, parent) in VARIANTS {
        let checkout = work.join(name).join(parent).join("checkout");
        let out = work.join(format!("out-{name}"));
        let stage = work.join(format!("stage-{name}"));
        println!("repro-gate: [{name}] cloning to {}", checkout.display());
        clone_tree(&source, &checkout)?;
        fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        let log = work.join(format!("build-{name}.log"));
        println!("repro-gate: [{name}] building (log {})", log.display());
        build(&options.build, &checkout, &out, &stage, &log)?;
        built.push(
            options
                .artifacts
                .iter()
                .map(|a| expand(a, &out, &checkout))
                .collect(),
        );
    }
    let identical = compare(&options.artifacts, &built[0], &built[1])?;
    if options.keep {
        println!("repro-gate: kept {}", work.display());
    } else {
        remove_tree(&work);
    }
    Ok(if identical {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let (mut source, mut build, mut work, mut keep) = (None, None, None, false);
    let mut artifacts = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--source" => source = Some(PathBuf::from(value("--source")?)),
            "--build" => build = Some(value("--build")?),
            "--artifact" => artifacts.push(value("--artifact")?),
            "--work" => work = Some(PathBuf::from(value("--work")?)),
            "--keep" => keep = true,
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }
    if artifacts.is_empty() {
        return Err(format!("at least one --artifact is required\n{}", usage()));
    }
    Ok(Options {
        source: source.ok_or_else(|| format!("--source is required\n{}", usage()))?,
        build: build.ok_or_else(|| format!("--build is required\n{}", usage()))?,
        artifacts,
        work: work.unwrap_or_else(|| {
            env::temp_dir().join(format!("psoxide-repro-gate-{}", std::process::id()))
        }),
        keep,
    })
}

fn usage() -> &'static str {
    "usage: repro-gate --source DIR --build 'CMD' --artifact '{out}/FILE' [--artifact ...] \
     [--work DIR] [--keep]"
}

fn expand(template: &str, out: &Path, checkout: &Path) -> PathBuf {
    PathBuf::from(
        template
            .replace("{out}", &out.to_string_lossy())
            .replace("{checkout}", &checkout.to_string_lossy()),
    )
}

/// Copy `source` to `dest` with APFS clones where available, then drop the
/// Cargo build directories.
fn clone_tree(source: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().ok_or("destination has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let cloned = Command::new("cp").arg("-cR").arg(source).arg(dest).status();
    if !matches!(cloned, Ok(status) if status.success()) {
        let _ = fs::remove_dir_all(dest);
        let copied = Command::new("cp").arg("-R").arg(source).arg(dest).status();
        if !matches!(copied, Ok(status) if status.success()) {
            return Err(format!(
                "cannot copy {} to {}",
                source.display(),
                dest.display()
            ));
        }
    }
    remove_cargo_targets(dest);
    Ok(())
}

/// Remove every directory Cargo marked as a build directory.
fn remove_cargo_targets(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        if path.join("CACHEDIR.TAG").is_file() {
            let _ = fs::remove_dir_all(&path);
        } else if entry.file_name() != ".git" {
            remove_cargo_targets(&path);
        }
    }
}

fn build(
    command: &str,
    checkout: &Path,
    out: &Path,
    stage: &Path,
    log: &Path,
) -> Result<(), String> {
    let log_file = fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let log_copy = log_file.try_clone().map_err(|e| e.to_string())?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(checkout)
        .env("PSOXIDE_REPRO_CHECKOUT", checkout)
        .env("PSOXIDE_REPRO_OUT", out)
        .env("PSOXIDE_GUEST_STAGE_ROOT", stage)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_copy))
        .status()
        .map_err(|e| format!("cannot run the build: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let text = fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(15).collect();
    Err(format!(
        "build failed ({status}); last lines of {}:\n{}",
        log.display(),
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    ))
}

/// Compare each artifact pair, printing one line per artifact.
fn compare(names: &[String], first: &[PathBuf], second: &[PathBuf]) -> Result<bool, String> {
    let mut identical = true;
    for ((name, a), b) in names.iter().zip(first).zip(second) {
        let left = fs::read(a).map_err(|e| format!("{}: {e}", a.display()))?;
        let right = fs::read(b).map_err(|e| format!("{}: {e}", b.display()))?;
        if left == right {
            println!(
                "SAME      {name}: {} bytes, sha256 {}",
                left.len(),
                sha256(a)
            );
            continue;
        }
        identical = false;
        let first_difference = left.iter().zip(&right).position(|(x, y)| x != y);
        let different = left.iter().zip(&right).filter(|(x, y)| x != y).count();
        println!(
            "DIFFERENT {name}: {} vs {} bytes, {} differing bytes (first at offset {}), \
             sha256 {} vs {}",
            left.len(),
            right.len(),
            different + left.len().abs_diff(right.len()),
            first_difference.map_or_else(|| "none".to_owned(), |at| at.to_string()),
            sha256(a),
            sha256(b)
        );
    }
    Ok(identical)
}

fn sha256(path: &Path) -> String {
    Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
        .unwrap_or_else(|| "?".to_owned())
}

/// Delete the work directory. `remove_dir_all` removes symbolic links without
/// following them, so a link inside a clone cannot reach outside it.
fn remove_tree(dir: &Path) {
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(dir).status();
    if let Err(error) = fs::remove_dir_all(dir) {
        eprintln!("repro-gate: could not remove {}: {error}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_templates_expand_both_directories() {
        let path = expand("{out}/x/{checkout}.bin", Path::new("/o"), Path::new("/c"));
        assert_eq!(path, PathBuf::from("/o/x//c.bin"));
    }

    #[test]
    fn missing_pieces_are_reported() {
        let args = ["--source", "/s", "--build", "true"].map(String::from);
        let error = parse(args.into_iter()).err().expect("no artifact");
        assert!(error.contains("--artifact"), "{error}");
    }

    #[test]
    fn variants_differ_in_depth_and_length() {
        let (a, b) = (VARIANTS[0].1, VARIANTS[1].1);
        assert_ne!(a.len(), b.len());
        assert_ne!(a.matches('/').count(), b.matches('/').count());
    }
}
