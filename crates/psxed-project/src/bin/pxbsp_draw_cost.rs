//! Read-only PXBSP/PVS draw-cost report for an editor project.
//!
//! Usage:
//!   pxbsp-draw-cost <project.ron> [hot-leaf-count]
//!   pxbsp-draw-cost <cooked.pxbsp>    -- face-width histogram only

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::{
    playtest::{analyze_pxbsp_draw_cost, build_package, PlaytestWorldGeometry},
    ProjectDocument,
};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(project_path) = args.first() else {
        eprintln!("usage: pxbsp-draw-cost <project.ron> [hot-leaf-count] | <cooked.pxbsp>");
        return ExitCode::from(2);
    };
    if project_path.ends_with(".pxbsp") {
        return match std::fs::read(project_path) {
            Ok(bytes) => {
                print_face_widths(&bytes);
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{project_path}: {error}");
                ExitCode::from(2)
            }
        };
    }
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
    if let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry {
        print_face_widths(&world.bytes);
    }
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

/// Vertex-count histogram of every cooked face, world and brush models.
fn print_face_widths(bytes: &[u8]) {
    let mut map = psx_bsp::pxbsp_resident::PxbspResidentMap::with_capacity(bytes.len());
    if let Err(error) = map.load(0, &mut psx_bsp::SliceReader::new(bytes)) {
        eprintln!("could not load cooked PXBSP: {error}");
        return;
    }
    let mut histogram = std::collections::BTreeMap::<usize, usize>::new();
    let faces = map.faces();
    for face in (0..faces.len()).filter_map(|index| faces.get(index)) {
        *histogram
            .entry(face.vertex_count.max(0) as usize)
            .or_default() += 1;
    }
    let widest = histogram.keys().next_back().copied().unwrap_or(0);
    let counts = histogram
        .iter()
        .map(|(width, count)| format!("{width}:{count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let limit = psx_bsp::render::PXBSP_MAX_FACE_VERTICES;
    let over: usize = histogram.range(limit + 1..).map(|(_, count)| count).sum();
    println!(
        "face vertices: widest {widest}, {over} over the runtime limit {limit}, histogram {counts}"
    );
}
