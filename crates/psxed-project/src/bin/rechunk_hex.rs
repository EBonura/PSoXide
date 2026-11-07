//! Re-cut a project's grid world into hexagonal rooms, in place.
//!
//! ```text
//! rechunk-hex projects/grid-citadel-hex6-sky/project.ron 4
//! ```
//!
//! Copy the project directory first: the project file is rewritten. The
//! world keeps its stream radii and streams as a ring afterwards
//! ([`psxed_project::grid::edit::chunks::rechunk_hex`]).

use std::process::ExitCode;

use psxed_project::grid::edit::chunks::{rechunk_hex, HexLayout};
use psxed_project::ProjectDocument;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(size)) = (args.next(), args.next()) else {
        eprintln!("usage: rechunk-hex <project.ron> <hexagon size in sectors>");
        return ExitCode::from(2);
    };
    let Ok(size) = size.parse::<f64>() else {
        eprintln!("hexagon size must be a number of sectors, got {size:?}");
        return ExitCode::from(2);
    };
    let mut project = match ProjectDocument::load_from_path(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("cannot load {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(world) = project.active_scene_mut().world.grid_mut() else {
        eprintln!("{path} has no grid world");
        return ExitCode::FAILURE;
    };
    let report = match rechunk_hex(world, HexLayout { size }) {
        Ok(report) => report,
        Err(refusal) => {
            eprintln!("cannot re-chunk: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = project.save_to_path(&path) {
        eprintln!("cannot save {path}: {error}");
        return ExitCode::FAILURE;
    }
    println!(
        "{path}: {} rooms -> {} hexagons of size {size}, widest {} sectors",
        report.rooms_before, report.rooms, report.widest
    );
    ExitCode::SUCCESS
}
