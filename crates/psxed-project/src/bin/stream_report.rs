//! CLI: partition a project into streamable regions and print the cook
//! report (design 2026-10-08, milestone M4). Host only; writes nothing
//! unless asked.
//!
//! Usage:
//!   stream-report <project.ron> [--json PATH] [--text PATH] [--enforce]
//!                 [--params COOK.ron] [--pool-bytes N]
//!   stream-report --fixture terrain16 [--json PATH] [--text PATH] [--enforce]
//!
//! Exit codes: 0 pass (or not enforcing), 1 gate failure with --enforce,
//! 2 usage or load error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::brush_region::{fixtures, report_for_project, CookOverrides, PartitionParams};
use psxed_project::ProjectDocument;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut source: Option<String> = None;
    let mut fixture: Option<String> = None;
    let mut json_path: Option<PathBuf> = None;
    let mut text_path: Option<PathBuf> = None;
    let mut enforce = false;
    let mut params_path: Option<PathBuf> = None;
    let mut pool_bytes: Option<u32> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_path = args.next().map(PathBuf::from),
            "--text" => text_path = args.next().map(PathBuf::from),
            "--fixture" => fixture = args.next(),
            "--enforce" => enforce = true,
            "--params" => params_path = args.next().map(PathBuf::from),
            "--pool-bytes" => pool_bytes = args.next().and_then(|v| v.parse().ok()),
            other if !other.starts_with("--") && source.is_none() => source = Some(other.into()),
            other => {
                eprintln!("[stream-report] unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let (project, root) = match (&source, &fixture) {
        (Some(path), None) => match ProjectDocument::load_from_path(path) {
            Ok(project) => {
                let root = Path::new(path)
                    .parent()
                    .map_or_else(|| PathBuf::from("."), PathBuf::from);
                (project, root)
            }
            Err(error) => {
                eprintln!("[stream-report] {path}: {error:?}");
                return ExitCode::from(2);
            }
        },
        (None, Some(name)) if name == "terrain16" => (
            // 16 x 16 cells of 1024 authored units (64 engine units), 4096
            // authored amplitude.
            fixtures::terrain_project(16, 1024, 4096, 7),
            PathBuf::from("."),
        ),
        _ => {
            eprintln!(
                "usage: stream-report <project.ron> | --fixture terrain16 [--json P] [--text P] [--enforce] [--params COOK.ron] [--pool-bytes N]"
            );
            return ExitCode::from(2);
        }
    };
    let mut params = PartitionParams::default();
    if let Some(path) = &params_path {
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| CookOverrides::from_ron_str(&text))
        {
            Ok(overrides) => overrides.apply(&mut params),
            Err(error) => {
                eprintln!("[stream-report] {}: {error}", path.display());
                return ExitCode::from(2);
            }
        }
    }
    CookOverrides {
        pool_bytes,
        ..CookOverrides::default()
    }
    .apply(&mut params);
    let started = std::time::Instant::now();
    match report_for_project(&project, &root, &params) {
        Ok((_, report)) => {
            print!("{}", report.text);
            eprintln!(
                "[stream-report] partitioned in {:.1}s",
                started.elapsed().as_secs_f64()
            );
            if let Some(path) = json_path {
                std::fs::write(path, &report.json).expect("write json report");
            }
            if let Some(path) = text_path {
                std::fs::write(path, &report.text).expect("write text report");
            }
            if enforce && !report.passed {
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[stream-report] {error}");
            ExitCode::from(2)
        }
    }
}
