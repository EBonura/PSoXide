//! `psoxide-components`: bring a tree's imported components to its
//! `components.lock.json` (see [`psoxide_link::components`]).
//!
//! ```text
//! psoxide-components [--root DIR] [--source NAME=GIT_CHECKOUT]... [--check] [--lock FILE]
//! ```
//!
//! `--root` defaults to the current directory. `--source` exports a component
//! from a local checkout (its locked commit, never the working tree) instead
//! of fetching it from GitHub. `--lock` reads a lock that lives outside an
//! ignored generated tree. `--check` only verifies, offline.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use psoxide_link::components::materialize;
use psoxide_link::Result;

const USAGE: &str = "usage: psoxide-components [--root DIR] [--source NAME=GIT_CHECKOUT]... [--check] [--lock FILE]";

fn run() -> Result<()> {
    let mut root = PathBuf::from(".");
    let mut sources = BTreeMap::new();
    let mut check = false;
    let mut lock = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--root" => root = PathBuf::from(value()?),
            "--lock" => lock = Some(PathBuf::from(value()?)),
            "--source" => {
                let item = value()?;
                let (name, path) = item
                    .split_once('=')
                    .ok_or_else(|| format!("--source wants NAME=GIT_CHECKOUT, got {item}"))?;
                sources.insert(name.to_string(), PathBuf::from(path));
            }
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}").into()),
        }
    }
    materialize(&root, &sources, check, lock.as_deref())?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("psoxide-components: {error}");
            ExitCode::FAILURE
        }
    }
}
