use std::path::PathBuf;

use crate::brush::Brush;
use crate::{MaterialResource, ProjectDocument, ResourceData};

use super::fixtures::terrain_project;
use super::gate::GateFailure;
use super::{partition, Partition, PartitionInput, PartitionParams, StreamReport};

fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects")
}

fn run(
    project: &ProjectDocument,
    root: &std::path::Path,
    params: &PartitionParams,
) -> (PartitionInput, Partition, StreamReport) {
    let input = PartitionInput::from_project(project, root).expect("input");
    let part = partition(&input, params);
    let report = StreamReport::build(&project.name, &input, &part);
    (input, part, report)
}

fn reach() -> (ProjectDocument, PathBuf) {
    let root = projects_dir().join("graybox-reach");
    let project = ProjectDocument::load_from_path(root.join("project.ron")).expect("reach");
    (project, root)
}

/// A sealed 1024-unit authored room: a handful of brushes, far under budget.
fn tiny_project() -> ProjectDocument {
    let mut project = ProjectDocument::new("Tiny");
    let m = project.add_resource("M", ResourceData::Material(MaterialResource::opaque(None)));
    let mut brushes = Vec::new();
    for (min, max) in [
        ([0, -256, 0], [4096, 0, 4096]),
        ([0, 0, 0], [256, 1024, 4096]),
        ([3840, 0, 0], [4096, 1024, 4096]),
        ([256, 0, 0], [3840, 1024, 256]),
        ([256, 0, 3840], [3840, 1024, 4096]),
        ([0, 1024, 0], [4096, 1280, 4096]),
    ] {
        let mut b = Brush::cuboid(min, max);
        for f in &mut b.faces {
            f.material = Some(m);
        }
        brushes.push(b);
    }
    project.active_scene_mut().brushes = brushes;
    project
}

#[test]
fn project_below_budget_is_exactly_one_region() {
    let project = tiny_project();
    let (_, part, report) = run(
        &project,
        std::path::Path::new("."),
        &PartitionParams::default(),
    );
    assert_eq!(part.regions.len(), 1);
    assert_eq!(part.tree.nodes.len(), 1, "no cuts");
    assert!(part.regions[0].counts.bytes() < part.params.region_target_bytes);
    assert!(part.graph.apertures.is_empty());
    assert!(report.text.contains("single region, no cuts"));
}

#[test]
fn reach_partition_invariants() {
    let (project, root) = reach();
    let params = PartitionParams::default();
    let (input, part, report) = run(&project, &root, &params);
    assert!(part.regions.len() > 1, "reach exceeds one region");

    // Cells tile the world: volumes add up and no two overlap.
    let world = part.world_bounds;
    let total: f64 = part.regions.iter().map(|r| r.bounds.volume()).sum();
    assert!((total - world.volume()).abs() < 1.0e-6 * world.volume());
    for (i, a) in part.regions.iter().enumerate() {
        for b in &part.regions[i + 1..] {
            assert!(
                !a.bounds.overlaps_strict(&b.bounds),
                "{} overlaps {}",
                a.id,
                b.id
            );
        }
    }
    // Ids are dense and match the cut tree.
    for (i, r) in part.regions.iter().enumerate() {
        assert_eq!(r.id as usize, i);
        assert_eq!(part.region_at(r.bounds.center()), r.id);
    }
    // Every input surface lands in exactly one region by centroid, and the
    // clipped pieces never lose faces.
    let faces: u32 = part.regions.iter().map(|r| r.counts.faces).sum();
    assert!(faces as usize >= input.render.len());
    let spawns: u32 = part.regions.iter().map(|r| r.counts.spawns).sum();
    assert_eq!(spawns as usize, input.spawns.len());
    // Hard limits hold, and every region respects the PVS row width.
    for r in &part.regions {
        assert!(
            r.violations(&params).is_empty(),
            "region {} {:?}",
            r.id,
            r.violations(&params)
        );
    }
    assert!(part.gate.max_row_bytes as usize <= psx_bsp::pxbsp::PXBSP_MAX_VISIBILITY_BYTES);
    // The graph is symmetric and the closure contains the region itself.
    for (a, list) in part.graph.adjacency.iter().enumerate() {
        for &(b, _) in list {
            assert!(part.graph.adjacency[b as usize]
                .iter()
                .any(|&(x, _)| x as usize == a));
        }
        assert!(part.closure.visible[a].contains(&(a as u32)));
    }
    // The layout is a permutation with contiguous, non-overlapping extents.
    let mut order = part.layout.order.clone();
    order.sort_unstable();
    assert_eq!(order, (0..part.regions.len() as u32).collect::<Vec<_>>());
    let mut at = 0;
    for &r in &part.layout.order {
        assert_eq!(part.layout.start_sector[r as usize], at);
        at += part.layout.sectors[r as usize];
    }
    assert_eq!(at, part.layout.total_sectors);
    assert!(part.layout.cost <= part.layout.baseline_cost);
    // The verdicts are printed.
    assert!(report.text.contains("rho verdict:"));
    assert!(report.text.contains("Pool verdict:"));
    assert!(report.json.contains("\"worst_bytes_per_unit\""));
}

#[test]
fn reach_model_matches_the_shipping_cook_face_count() {
    // The cooker packs 1,145 faces for graybox-reach (11,450 B face lump).
    // The reachability filter must reproduce that census before any cut.
    let (project, root) = reach();
    let input = PartitionInput::from_project(&project, &root).unwrap();
    assert_eq!(input.render.len(), 1145);
}

#[test]
fn report_is_byte_identical_across_runs() {
    let (project, root) = reach();
    let params = PartitionParams::default();
    let (_, _, a) = run(&project, &root, &params);
    let (_, _, b) = run(&project, &root, &params);
    assert_eq!(a.text, b.text);
    assert_eq!(a.json, b.json);
}

#[test]
fn terrain_report_is_deterministic_and_collision_bound() {
    let project = terrain_project(6, 1024, 4096, 7);
    let params = PartitionParams::default();
    let (_, part, a) = run(&project, std::path::Path::new("."), &params);
    let (_, _, b) = run(&project, std::path::Path::new("."), &params);
    assert_eq!(a.json, b.json);
    let clips: u32 = part.regions.iter().map(|r| r.counts.clip_nodes).sum();
    assert!(clips > 0, "terrain wedges cost clipnodes");
}

#[test]
fn rank_row_overflow_is_a_precise_error() {
    // A huge per-slot leaf cap widens every row past the 1024-byte limit.
    let (project, root) = reach();
    let mut params = PartitionParams::default();
    params.caps.leaves = 4096;
    let (_, part, report) = run(&project, &root, &params);
    let wide: Vec<_> = part
        .gate
        .failures
        .iter()
        .filter_map(|f| match f {
            GateFailure::RankRowTooWide {
                region,
                closure,
                width_bytes,
                limit_bytes,
            } => Some((*region, *closure, *width_bytes, *limit_bytes)),
            _ => None,
        })
        .collect();
    assert!(!wide.is_empty());
    for (_, closure, width, limit) in wide {
        assert_eq!(limit, 1024);
        assert_eq!(width, (closure as u32 * 4096).div_ceil(8));
        assert!(width > limit);
    }
    assert!(!report.passed);
    assert!(report.text.contains("PVS row needs"));
}

#[test]
fn oversize_region_without_a_cut_is_reported_not_clamped() {
    // Forbid every cut by demanding cells no thinner than the world.
    let (project, root) = reach();
    let params = PartitionParams {
        min_cell_extent: 1.0e9,
        ..PartitionParams::default()
    };
    let (_, part, report) = run(&project, &root, &params);
    assert_eq!(part.regions.len(), 1);
    assert!(part.gate.failures.iter().any(|f| matches!(
        f,
        GateFailure::RegionOverCap {
            unsplittable: true,
            ..
        }
    )));
    assert!(!report.passed);
}

#[test]
fn clipping_conserves_surface_area_across_cells() {
    // Cutting loses no polygon and carries none twice: the clipped pieces of
    // all cells add up to the input area, which is the ownership rule for
    // surfaces lying exactly on a cut plane.
    let (project, root) = reach();
    let input = PartitionInput::from_project(&project, &root).unwrap();
    let part = partition(&input, &PartitionParams::default());
    let area = |list: &[crate::brush_compile::CompiledSurface]| -> f64 {
        list.iter()
            .map(|s| super::geometry::polygon_area(&s.vertices))
            .sum()
    };
    let expected = area(&input.render);
    let clipped: f64 = part
        .regions
        .iter()
        .map(|r| {
            area(&super::account::clip_surfaces(
                &input.render,
                &input.render_bounds,
                &r.bounds,
            ))
        })
        .sum();
    assert!(
        ((clipped - expected) / expected).abs() < 1.0e-6,
        "{clipped} vs {expected}"
    );
}

#[test]
fn cook_overrides_are_recorded_in_the_report() {
    let mut params = PartitionParams::default();
    super::CookOverrides {
        pool_bytes: Some(1 << 20),
        rho_batched: Some(true),
        ..Default::default()
    }
    .apply(&mut params);
    assert_eq!(params.pool_bytes, 1 << 20);
    assert_eq!(params.overrides.len(), 2);
    let project = tiny_project();
    let (_, _, report) = run(&project, std::path::Path::new("."), &params);
    assert!(report.text.contains("Parameter overrides in effect"));
    assert!(report.json.contains("pool_bytes = 1048576"));
    // An override equal to the default records nothing.
    let mut same = PartitionParams::default();
    super::CookOverrides {
        pool_bytes: Some(super::P_WORLD_ESTIMATE_BYTES),
        ..Default::default()
    }
    .apply(&mut same);
    assert!(same.overrides.is_empty());
}
