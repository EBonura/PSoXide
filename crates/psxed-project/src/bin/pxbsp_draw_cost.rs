//! Read-only PXBSP/PVS draw-cost report for an editor project.
//!
//! Usage:
//!   pxbsp-draw-cost <project.ron> [hot-leaf-count]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::{
    playtest::{analyze_pxbsp_draw_cost, build_package},
    ProjectDocument,
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(project_path) = args.first() else {
        eprintln!("usage: pxbsp-draw-cost <project.ron> [hot-leaf-count]");
        return ExitCode::from(2);
    };
    let limit = match args.get(1) {
        Some(value) => match value.parse::<usize>() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid hot-leaf-count '{value}': {error}");
                return ExitCode::from(2);
            }
        },
        None => 12,
    };
    let text = match std::fs::read_to_string(project_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{project_path}: {error}");
            return ExitCode::from(2);
        }
    };
    let project = match ProjectDocument::from_ron_str(&text) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("{project_path}: parse failed: {error}");
            return ExitCode::from(2);
        }
    };
    let project_root = Path::new(project_path)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let (package, validation) = build_package(&project, &project_root);
    for warning in &validation.warnings {
        eprintln!("warning: {warning}");
    }
    if !validation.errors.is_empty() {
        for error in &validation.errors {
            eprintln!("error: {error}");
        }
        return ExitCode::from(1);
    }
    let Some(package) = package else {
        eprintln!("the project did not produce a playtest package");
        return ExitCode::from(1);
    };
    let report = match analyze_pxbsp_draw_cost(&package) {
        Ok(Some(report)) => report,
        Ok(None) => {
            eprintln!("the cooked package does not contain PXBSP world geometry");
            return ExitCode::from(1);
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "PXBSP draw cost: {} faces, {} base triangles, {} all-face base packet slots, {} non-solid leaves, {} unreadable PVS rows",
        report.world_face_count,
        report.world_base_triangle_count,
        report.world_base_packet_slots,
        report.non_solid_leaf_count,
        report.unreadable_pvs_leaf_count,
    );
    for leaf in report.heaviest_leaves(limit) {
        let location = leaf.authored_surface_anchor.map_or_else(
            || "anchor unavailable".to_string(),
            |[x, y, z]| format!("authored anchor=({x}, {y}, {z})"),
        );
        let bounds = leaf
            .authored_surface_bounds_min
            .zip(leaf.authored_surface_bounds_max)
            .map_or_else(String::new, |(min, max)| {
                format!(" bounds={min:?}..{max:?}")
            });
        println!(
            "leaf {}: PVS leaves={} unique faces={} sky={} base triangles={} base packet slots={} {location}{bounds}",
            leaf.leaf_index,
            leaf.visible_leaf_count,
            leaf.visible_face_count,
            leaf.visible_sky_aperture_face_count,
            leaf.base_triangle_count,
            leaf.base_packet_slots,
        );
    }
    ExitCode::SUCCESS
}
