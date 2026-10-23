//! How well the ground of a grid world follows the model it was made from.
//!
//! Usage: `gridcover <project dir> <fine_hits.npy> <frame.json> [origin x,z] [out tsv]`
//!
//! For every ray of the model the top of the solid that stands on the ground
//! (the chain of solid intervals from the ground up, joined across gaps under
//! the float clearance) is compared with the height of the highest upward
//! face of the grid over the same spot. Columns where the grid is far below
//! the model are model that the conversion dropped (a thin gap between towers
//! that closed, a column lost to the closing and opening of the voxels);
//! columns where it is far above are model the grid invented.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use psxed_project::grid::faces::all_faces_with;
use psxed_project::grid::float::columns::{Frame, COLUMNS_PER_SECTOR};
use psxed_project::grid::float::cover::raster_tops;
use psxed_project::grid::float::npy::parse_f32;
use psxed_project::ProjectDocument;

/// Open space under a solid that still counts as standing on the ground.
const CLEARANCE: f64 = 2304.0;
/// Differences under this are the fit's tolerance, grid units.
const AGREE: f64 = 768.0;

fn ground_top(hits: &[f32], frame: &Frame) -> Option<f64> {
    let mut heights: Vec<f64> = hits
        .iter()
        .filter(|h| h.is_finite())
        .map(|&h| frame.grid_units(h))
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| b.total_cmp(a));
    let mut intervals: Vec<(f64, f64)> = heights
        .chunks(2)
        .map(|p| (p.get(1).copied().unwrap_or(0.0).max(0.0), p[0]))
        .filter(|(b, t)| t > b)
        .collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut top = 0.0f64;
    for (bottom, t) in intervals {
        if bottom - top < CLEARANCE {
            top = top.max(t);
        } else {
            break;
        }
    }
    Some(top)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: gridcover <project dir> <fine_hits.npy> <frame.json> [x,z] [out tsv]");
        return ExitCode::from(2);
    }
    let run = || -> Result<(), String> {
        let frame = Frame::from_json(
            &std::fs::read_to_string(&args[2]).map_err(|e| format!("{}: {e}", args[2]))?,
        )?;
        let npy = parse_f32(&std::fs::read(&args[1]).map_err(|e| format!("{}: {e}", args[1]))?)?;
        let [rows, cols, depth] = npy.shape[..] else {
            return Err("the hit array must be rows x columns x hits".to_string());
        };
        let origin: [i32; 2] = match args.get(3).and_then(|s| s.split_once(',')) {
            Some((x, z)) => [
                x.parse().map_err(|_| "bad origin x")?,
                z.parse().map_err(|_| "bad origin z")?,
            ],
            None => [2, 2],
        };
        let doc =
            ProjectDocument::load_from_path(std::path::Path::new(&args[0]).join("project.ron"))
                .map_err(|e| format!("{e:?}"))?;
        let world = doc.active_scene().world.grid().ok_or("no grid world")?;
        let sector_editor = f64::from(world.scale.to_editor(1024));
        let to_grid = 1024.0 / sector_editor;
        let faces = all_faces_with(world, &());
        let top = raster_tops(&faces, sector_editor);
        let (mut agree, mut low, mut high, mut bare) = (0u64, 0u64, 0u64, 0u64);
        let mut worst: BTreeMap<(i32, i32), (u32, f64)> = BTreeMap::new();
        let mut tsv = String::new();
        let mut none = String::new();
        for row in 0..rows {
            for col in 0..cols {
                let start = (row * cols + col) * depth;
                let Some(model) = ground_top(&npy.data[start..start + depth], &frame) else {
                    continue;
                };
                let key = (
                    origin[0] * COLUMNS_PER_SECTOR as i32 + col as i32,
                    origin[1] * COLUMNS_PER_SECTOR as i32 + row as i32,
                );
                let Some(&grid) = top.get(&key) else {
                    bare += 1;
                    let _ = writeln!(none, "{}\t{}\t{:.0}", key.0, key.1, model);
                    continue;
                };
                let diff = grid * to_grid - model;
                if diff.abs() <= AGREE {
                    agree += 1;
                } else if diff < 0.0 {
                    low += 1;
                    let sector = (
                        key.0.div_euclid(COLUMNS_PER_SECTOR as i32),
                        key.1.div_euclid(COLUMNS_PER_SECTOR as i32),
                    );
                    let e = worst.entry(sector).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 = e.1.min(diff);
                    let _ = writeln!(
                        tsv,
                        "{}\t{}\t{:.0}\t{:.0}",
                        key.0,
                        key.1,
                        model,
                        grid * to_grid
                    );
                } else {
                    high += 1;
                }
            }
        }
        println!(
            "{} rays with a ground run: {agree} within {AGREE} grid units, {low} grid LOWER than the model, {high} grid higher, {bare} with no grid face at all",
            agree + low + high + bare
        );
        println!(
            "{} sectors hold a column the grid leaves more than {AGREE} below the model",
            worst.len()
        );
        let mut deep: Vec<_> = worst.iter().collect();
        deep.sort_by(|a, b| a.1 .1.total_cmp(&b.1 .1));
        for (sector, (n, d)) in deep.into_iter().take(12) {
            println!("  sector {sector:?}: {n} columns, worst {d:.0} grid units");
        }
        if let Some(out) = args.get(4) {
            std::fs::write(out, tsv).map_err(|e| format!("{out}: {e}"))?;
            std::fs::write(format!("{out}.bare"), none).map_err(|e| format!("{out}: {e}"))?;
        }
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("gridcover: {m}");
            ExitCode::FAILURE
        }
    }
}
