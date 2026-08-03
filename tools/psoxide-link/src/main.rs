//! CLI over `psoxide_link`, for callers that want a working tree hydrated
//! rather than the pinned one: `--from` is how the demo disc puts every
//! program on a single SDK whatever each game pinned for itself.
//!
//! A game hydrating its own pin does not need this. Its pin crate depends on
//! the library and calls [`psoxide_link::hydrate_pinned`], which needs no
//! cargo metadata and no path into a tree that does not exist yet.

use std::env;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use psoxide_link::{hydrate, pinned_checkout, Result};

/// Default crate to look for in `cargo metadata`.
const DEFAULT_PIN_CRATE: &str = "psx-iso";

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
            args.next().ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("{arg} needs a value").into()
            })
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
    match &options.from {
        Some(path) => hydrate(&path.canonicalize()?, &options.into, None, options.quiet),
        None => {
            let rev = options
                .rev
                .as_deref()
                .ok_or("--rev is required unless --from is given")?;
            let source = pinned_checkout(&options.workspace, &options.pin_crate, rev)?;
            hydrate(
                &source,
                &options.into,
                Some(&format!("git:{rev}")),
                options.quiet,
            )
        }
    }
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
