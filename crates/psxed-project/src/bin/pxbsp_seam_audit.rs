//! Read-only watertightness report for a cooked PXBSP.
//!
//! Usage: pxbsp-seam-audit <cooked.pxbsp | project.ron> [tolerance-units] [sample-count]

use std::process::ExitCode;

use psxed_project::brush_seams::{find_t_junctions, open_edges};

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(path) = args.first() else {
        eprintln!("usage: pxbsp-seam-audit <cooked.pxbsp> [tolerance] [samples]");
        return ExitCode::from(2);
    };
    let tolerance = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(1i64);
    let samples = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(12usize);
    let bytes = if path.ends_with(".ron") {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("{path}: {error}");
                return ExitCode::from(2);
            }
        };
        let project = match psxed_project::ProjectDocument::from_ron_str(&text) {
            Ok(project) => project,
            Err(error) => {
                eprintln!("{path}: parse failed: {error}");
                return ExitCode::from(2);
            }
        };
        let root = std::path::Path::new(path)
            .parent()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let (package, validation) = psxed_project::playtest::build_package(&project, &root);
        for error in &validation.errors {
            eprintln!("error: {error}");
        }
        let Some(package) = package else {
            eprintln!("the project did not produce a playtest package");
            return ExitCode::from(1);
        };
        package.world_geometry.bytes.clone()
    } else {
        match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("{path}: {error}");
                return ExitCode::from(2);
            }
        }
    };
    let mut map = psx_bsp::pxbsp_resident::PxbspResidentMap::with_capacity(bytes.len());
    if let Err(error) = map.load(0, &mut psx_bsp::SliceReader::new(&bytes)) {
        eprintln!("could not load cooked PXBSP: {error}");
        return ExitCode::from(1);
    }
    let vertices = map.vertices();
    let faces = map.faces();
    let materials = map.materials();
    let mut polygons = Vec::new();
    let mut face_ids = Vec::new();
    let mut sky_faces = 0usize;
    let mut sky_polygons: Vec<Vec<[i32; 3]>> = Vec::new();
    for index in 0..faces.len() {
        let Some(face) = faces.get(index) else { continue };
        let sky = materials
            .get(face.texture.max(0) as usize)
            .is_some_and(|m| m.flags & psx_bsp::pxbsp::material_flags::SKY_APERTURE != 0);
        let polygon: Vec<[i32; 3]> = (0..face.vertex_count.max(0) as usize)
            .filter_map(|k| vertices.get(face.first_vertex as usize + k))
            .map(|v| [i32::from(v.position.x), i32::from(v.position.y), i32::from(v.position.z)])
            .collect();
        if sky {
            sky_faces += 1;
            sky_polygons.push(polygon);
            continue;
        }
        polygons.push(polygon);
        face_ids.push(index);
    }
    let found = find_t_junctions(&polygons, tolerance);
    let mut touched_edges = std::collections::BTreeSet::new();
    for t in &found {
        touched_edges.insert((t.polygon, t.edge));
    }
    println!(
        "faces {} (drawable {}, sky {}), vertices {}; T-junction corners {} on {} edges (tolerance {})",
        faces.len(),
        polygons.len(),
        sky_faces,
        vertices.len(),
        found.len(),
        touched_edges.len(),
        tolerance
    );
    for t in found.iter().take(samples) {
        let poly = &polygons[t.polygon];
        println!(
            "  face {} edge {}: {:?} -> {:?} has foreign corner {:?} from face {}",
            face_ids[t.polygon],
            t.edge,
            poly[t.edge],
            poly[(t.edge + 1) % poly.len()],
            t.position,
            face_ids[t.touching_polygon]
        );
        println!("      face polygon {:?}", poly);
        println!("      touching polygon {:?}", polygons[t.touching_polygon]);
    }
    let open = open_edges(&polygons, &sky_polygons);
    println!("open edges (shared with no other polygon): {}", open.len());
    for &(polygon, edge) in open.iter().take(samples) {
        let poly = &polygons[polygon];
        println!(
            "  face {} edge {}: {:?} -> {:?}",
            face_ids[polygon],
            edge,
            poly[edge],
            poly[(edge + 1) % poly.len()]
        );
    }
    ExitCode::SUCCESS
}
