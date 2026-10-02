//! `site`: the website's data importers, checks and builds.
//!
//! ```text
//! site import-sdk                      sdk/README.md tables -> data/sdk.json
//! site import-compat [MD]              COMPATIBILITY.md -> data/compat.json
//! site import-emu-bench [CSV]          emu-bench summary -> data/emu-bench.json
//! site import-sdk-reference [--sdk-root DIR] [--check]
//!                                      crate structures and example sources
//! site pins                            sdk=/emulator= pins for $GITHUB_OUTPUT
//! site player --emulator DIR --out DIR the pinned emulator's web player bundle
//! site stage-examples --player DIR --sdk DIR --examples DIR
//! site sdk-docs --sdk-root DIR --target-dir DIR [--base-url URL]
//! site check --base-url URL [--output-dir public]
//! site browser-check [--zola PATH]     layout, images, FAQ and theme in Chrome
//! site review-shots [--zola PATH]      full-page screenshots and a contact sheet
//! site usability --set before|after [--site DIR] [--zola PATH] [--json FILE]
//! ```
//!
//! Every data file these write is compared byte for byte (`git diff` in CI,
//! `--check`), so they write JSON exactly as the Python scripts they replace
//! did (see [`crate::pyjson`]).

mod browser;
mod check;
mod docs;
mod html_entities;
mod import;
mod player;
mod reference;
mod stage;

const USAGE: &str = "site tasks: import-sdk, import-compat [MD], import-emu-bench [CSV],
  import-sdk-reference [--sdk-root DIR] [--check], pins,
  player --emulator DIR --out DIR, stage-examples --player DIR --sdk DIR --examples DIR,
  sdk-docs --sdk-root DIR --target-dir DIR [--base-url URL],
  check --base-url URL [--output-dir public], browser-check [--zola PATH],
  review-shots [--zola PATH], usability --set SET [--site DIR] [--zola PATH] [--json FILE]";

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The website source directory.
pub fn site_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../website");
    root.canonicalize().unwrap_or(root)
}

/// Read a text file as Python's `read_text()` does: UTF-8 with universal
/// newlines, so `\r\n` and lone `\r` arrive as `\n`.
pub fn read_text(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    })
}

/// Write a file, naming it in the error.
pub fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<(), String> {
    fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// A parsed TOML file.
pub fn read_toml(path: &Path) -> Result<toml::Table, String> {
    read_text(path)?
        .parse::<toml::Table>()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `data/examples.toml`: the SDK and emulator revisions the site's player,
/// examples and references are built from.
pub fn pins() -> Result<toml::Table, String> {
    read_toml(&site_root().join("data/examples.toml"))
}

/// A string field of a TOML table.
pub fn text<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("missing string {key}"))
}

/// Run a command to completion, failing on a nonzero exit.
pub fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("{:?}: {e}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed: {status}"))
    }
}

/// Run a command and return its trimmed standard output.
pub fn output(command: &mut Command) -> Result<String, String> {
    let out = command
        .output()
        .map_err(|e| format!("{:?}: {e}", command.get_program()))?;
    if !out.status.success() {
        return Err(format!("{command:?} failed: {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git diff --exit-code REV -- PATHS` in `repo`: the tree there must be the
/// pinned source, so nothing is published under a revision it did not come
/// from.
pub fn require_pinned(repo: &Path, revision: &str, paths: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["diff", "--exit-code", revision, "--"])
        .args(paths)
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} differs from the pinned SDK {revision} in {paths:?}",
            repo.display()
        ))
    }
}

/// Simple `--flag value` / `--flag=value` / switch parsing for the tasks.
pub struct Args {
    values: Vec<(String, Option<String>)>,
}

impl Args {
    /// Parse `args`, treating the names in `switches` as value-less.
    pub fn parse(args: &[String], switches: &[&str]) -> Result<Self, String> {
        let mut values = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if let Some(flag) = arg.strip_prefix("--") {
                if let Some((name, value)) = flag.split_once('=') {
                    values.push((name.to_string(), Some(value.to_string())));
                } else if switches.contains(&flag) {
                    values.push((flag.to_string(), None));
                } else {
                    let value = iter
                        .next()
                        .ok_or_else(|| format!("--{flag} needs a value"))?;
                    values.push((flag.to_string(), Some(value.clone())));
                }
            } else {
                return Err(format!("unexpected argument {arg}"));
            }
        }
        Ok(Self { values })
    }

    /// The last value given for `--name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// A required `--name` value.
    pub fn need(&self, name: &str) -> Result<&str, String> {
        self.get(name)
            .ok_or_else(|| format!("--{name} is required"))
    }

    /// Whether the switch `--name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.values.iter().any(|(n, _)| n == name)
    }
}

/// Run one site task.
pub fn main(args: &[String]) -> Result<(), String> {
    let Some((task, rest)) = args.split_first() else {
        return Err("site needs a task; see `site --help`".into());
    };
    match task.as_str() {
        "import-sdk" => import::sdk(),
        "import-compat" => import::compat(rest),
        "import-emu-bench" => import::emu_bench(rest),
        "import-sdk-reference" => reference::main(rest),
        "pins" => {
            let pins = pins()?;
            println!("sdk={}", text(&pins, "sdk_revision")?);
            println!("emulator={}", text(&pins, "emulator_revision")?);
            Ok(())
        }
        "player" => player::main(rest),
        "stage-examples" => stage::main(rest),
        "sdk-docs" => docs::main(rest),
        "check" => check::main(rest),
        "browser-check" => browser::browser_check(rest),
        "review-shots" => browser::review_shots(rest),
        "usability" => browser::usability(rest),
        "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown site task {other}")),
    }
}
