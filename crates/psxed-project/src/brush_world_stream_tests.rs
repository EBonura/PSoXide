//! Gates for the streamed cook (M6): the streamed map must show what the
//! same geometry shows as one whole map, installing in any order must never
//! leave a dangling reference, and unresident space must read solid.
//!
//! The *flat reference* below is built independently of the runtime install
//! code: it concatenates the cooked region tables with plain additive bases
//! and the dense, slot-free PVS rows the cooker computed before it rewrote
//! them into the rank layout. Equality against it therefore exercises the
//! rank rewrite, the slot relocation, the link patches and the renderer's
//! streamed PVS decode together.

use std::path::PathBuf;

use psx_bsp::collision::{CONTENTS_EMPTY, CONTENTS_SOLID};
use psx_bsp::pxbsp::{PxbspIndex, PxbspLumpKind};
use psx_bsp::pxbsp_resident::stream::{RegionLoader, StreamError};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::{configure_projection, load_pxbsp_view, Camera, Renderer};
use psx_bsp::{SliceReader, Vec3I32};

use super::*;
use super::{flat_rd16 as rd16, flatten_streamed as flatten, FlatWorld as Flat};
use crate::brush_region::PartitionParams;
use crate::stream_world::{generate, load_donor, StreamWorldConfig};

fn projects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects")
}

fn cook(project: &ProjectDocument, root: &Path, params: &PartitionParams) -> StreamedBrushWorld {
    match cook_project_streamed(project, root, BrushWorldCookMode::Draft, [0; 3], params)
        .expect("cook")
    {
        CookedWorld::Streamed(world) => *world,
        CookedWorld::Whole(_) => panic!("expected more than one region"),
    }
}

fn graybox() -> (ProjectDocument, PathBuf) {
    let root = projects_dir().join("graybox-reach");
    let project = ProjectDocument::load_from_path(root.join("project.ron")).expect("graybox-reach");
    (project, root)
}

fn lcg(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state >> 8
}

// ---- the flat reference ----------------------------------------------------

fn load_flat(flat: &Flat) -> PxbspResidentMap {
    let mut map = PxbspResidentMap::with_capacity(flat.bytes.len() * 2);
    map.load(1, &mut SliceReader::new(&flat.bytes))
        .expect("the flat reference is a valid legacy map");
    map
}

fn load_streamed(world: &StreamedBrushWorld, slots: u16) -> PxbspResidentMap {
    let mut map = PxbspResidentMap::with_capacity(world.container.len() * 8 + (1 << 22));
    map.load_streamed(1, &mut SliceReader::new(&world.container), slots)
        .expect("streamed container");
    map
}

fn install_all(world: &StreamedBrushWorld, map: &mut PxbspResidentMap) {
    let mut loader = RegionLoader::new(SliceReader::new(&world.region_pack), 0);
    let mut buf = vec![0u8; world.region_pack.len().max(4096)];
    for r in 0..world.index.regions.len() as u16 {
        let n = loader.load(&world.index, r, &mut buf).expect("region read");
        let slot = map.streaming().unwrap().free_slot().expect("free slot");
        map.install_region(r, slot, &buf[..n]).expect("install");
    }
}

/// The guest's install: regions in disc order, each into the slot of its own
/// number, fed one 2 KiB sector at a time straight from the region pack.
fn install_by_sectors_in_disc_order(world: &StreamedBrushWorld, map: &mut PxbspResidentMap) {
    let mut order: Vec<u16> = (0..world.index.regions.len() as u16).collect();
    order.sort_by_key(|&r| world.index.regions[r as usize].sector_start);
    for region in order {
        let entry = world.index.regions[region as usize];
        let mut job = map.begin_install(region, region).expect("begin");
        let start = entry.sector_start as usize * 2048;
        let pack = &world.region_pack[start..start + entry.sectors() as usize * 2048];
        let mut progress = None;
        for sector in pack.chunks(2048) {
            progress = Some(map.install_feed(&mut job, sector).expect("feed"));
        }
        assert_eq!(
            progress,
            Some(psx_bsp::pxbsp_resident::stream::InstallProgress::Staged)
        );
        assert_ne!(job.materials(), 0, "a region draws something");
        map.link_region(&mut job).expect("link");
    }
}

// ---- gate (a): streamed == whole ------------------------------------------

struct Sampled {
    views: usize,
    skipped: usize,
    pvs_faces_total: usize,
    frame_faces_total: usize,
    source_ids_total: usize,
}

fn compare_views(world: &StreamedBrushWorld, views: usize, seed: u32) -> Sampled {
    compare_views_with(world, views, seed, install_all, false)
}

/// [`compare_views`] with the install strategy under test. With `ordered` the
/// face lists must also agree in draw order, not just as sets: the renderer
/// emits packets in that order, so equal order is what makes the frames pixel
/// identical where two surfaces tie in depth.
fn compare_views_with(
    world: &StreamedBrushWorld,
    views: usize,
    seed: u32,
    install: fn(&StreamedBrushWorld, &mut PxbspResidentMap),
    ordered: bool,
) -> Sampled {
    configure_projection();
    let flat = flatten(world);
    let flat_map = load_flat(&flat);
    let regions = world.index.regions.len() as u16;
    let mut streamed = load_streamed(world, regions);
    install(world, &mut streamed);
    streamed.check_integrity().expect("integrity");

    let mut flat_renderer =
        Renderer::new_pxbsp_with_nodes(flat_map.faces().len(), flat_map.nodes().len());
    let mut streamed_renderer =
        Renderer::new_pxbsp_with_nodes(streamed.faces().len(), streamed.nodes().len());
    let (mins, maxs) = world.debug.world_bounds;
    let mut state = seed;
    let mut out = Sampled {
        views: 0,
        skipped: 0,
        pvs_faces_total: 0,
        frame_faces_total: 0,
        source_ids_total: 0,
    };
    let state_ref = streamed.streaming().unwrap();
    let caps = state_ref.index().caps;
    let mut attempts = 0;
    while out.views < views {
        attempts += 1;
        assert!(
            attempts < views * 400,
            "could not find {views} open camera positions"
        );
        let mut origin = [0i32; 3];
        for axis in 0..3 {
            let span = i32::from(maxs[axis]) - i32::from(mins[axis]);
            let value = i32::from(mins[axis]) + (lcg(&mut state) as i32).rem_euclid(span.max(1));
            origin[axis] = value << 12;
        }
        let camera = Camera {
            origin: Vec3I32 {
                x: origin[0],
                y: origin[1],
                z: origin[2],
            },
            angles: [
                (lcg(&mut state) % 4096) as i16 - 2048,
                (lcg(&mut state) % 4096) as i16,
                0,
            ],
        };
        let flat_leaf = flat_map.point_leaf_index(camera.origin).unwrap();
        let streamed_leaf = streamed.point_leaf_index(camera.origin).unwrap();
        let flat_open =
            flat_leaf != 0 && flat_map.leaves().get(flat_leaf).unwrap().contents != CONTENTS_SOLID;
        let streamed_open = streamed_leaf != 0
            && streamed.leaves().get(streamed_leaf).unwrap().contents != CONTENTS_SOLID;
        assert_eq!(
            flat_open, streamed_open,
            "the two maps disagree on whether {origin:?} is solid"
        );
        if !flat_open {
            out.skipped += 1;
            continue;
        }
        // Same leaf in both maps.
        let (region, local) = flat.leaf_of[flat_leaf];
        let slot_state = streamed.streaming().unwrap();
        let slot = slot_state.slot_of(region as u16).expect("installed") as usize;
        let top_leaves = slot_state.index().top.leaves as usize;
        assert_eq!(
            streamed_leaf,
            top_leaves + slot * caps.leaves as usize + (local - 1)
        );

        let view = load_pxbsp_view(camera);
        let (pvs_flat, frame_flat) = flat_renderer
            .debug_world_selection(&flat_map, camera, view)
            .expect("flat selection");
        let (pvs_streamed, frame_streamed) = streamed_renderer
            .debug_world_selection(&streamed, camera, view)
            .expect("streamed selection");
        let key_flat = |f: &u16| flat.face_of[*f as usize];
        let key_streamed = |f: &u16| {
            let slot = *f as usize / caps.faces as usize;
            let region = streamed
                .streaming()
                .unwrap()
                .region_in_slot(slot as u16)
                .unwrap() as usize;
            (region, *f as usize % caps.faces as usize)
        };
        let mut a: Vec<_> = pvs_flat.iter().map(key_flat).collect();
        let mut b: Vec<_> = pvs_streamed.iter().map(key_streamed).collect();
        let mut c: Vec<_> = frame_flat.iter().map(key_flat).collect();
        let mut d: Vec<_> = frame_streamed.iter().map(key_streamed).collect();
        if ordered {
            assert_eq!(
                a, b,
                "PVS face order differs at {origin:?} angles {:?}",
                camera.angles
            );
            assert_eq!(
                c, d,
                "frame face order differs at {origin:?} angles {:?}",
                camera.angles
            );
        }
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(
            a, b,
            "PVS face sets differ at {origin:?} angles {:?}",
            camera.angles
        );
        c.sort_unstable();
        d.sort_unstable();
        assert_eq!(
            c, d,
            "frame face sets differ at {origin:?} angles {:?}",
            camera.angles
        );
        // The same sets by stable source id.
        let ids = |keys: &[(usize, usize)]| {
            let mut v: Vec<u32> = keys
                .iter()
                .map(|&(r, f)| world.debug.face_source[r][f])
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        assert_eq!(ids(&c), ids(&d));
        out.source_ids_total += ids(&c).len();
        out.pvs_faces_total += a.len();
        out.frame_faces_total += c.len();
        out.views += 1;
    }
    assert_eq!(streamed.streaming().unwrap().pvs_missing(), 0);
    out
}

#[test]
fn graybox_reach_streamed_equals_whole_over_sampled_views() {
    let (project, root) = graybox();
    let world = cook(&project, &root, &PartitionParams::default());
    assert!(world.index.regions.len() >= 2);
    let sampled = compare_views(&world, 400, 0xC0FFEE);
    eprintln!(
        "graybox-reach: {} regions, {} views ({} closed samples skipped), {} pvs faces, {} frame faces, {} source ids",
        world.index.regions.len(),
        sampled.views,
        sampled.skipped,
        sampled.pvs_faces_total,
        sampled.frame_faces_total,
        sampled.source_ids_total
    );
    assert!(sampled.frame_faces_total > 0);
}

/// Face sets a streamed map draws at `camera`, as sorted stable source ids:
/// the potentially visible set and the frame after the renderer's culling.
fn drawn_source_ids(
    world: &StreamedBrushWorld,
    map: &PxbspResidentMap,
    renderer: &mut Renderer,
    camera: Camera,
) -> (Vec<u32>, Vec<u32>) {
    let view = load_pxbsp_view(camera);
    let (pvs, frame) = renderer
        .debug_world_selection(map, camera, view)
        .expect("selection");
    let faces = map.streaming().unwrap().index().caps.faces as usize;
    let ids = |list: &[u16]| {
        let mut ids: Vec<u32> = list
            .iter()
            .map(|&f| {
                let slot = f as usize / faces;
                let region = map
                    .streaming()
                    .unwrap()
                    .region_in_slot(slot as u16)
                    .unwrap();
                world.debug.face_source[region as usize][f as usize % faces]
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    (ids(&pvs), ids(&frame))
}

/// Distance from `eye` (engine units) to the nearest vertex of the face with
/// stable source id `id`.
fn nearest_vertex_distance(world: &StreamedBrushWorld, id: u32, eye: [i32; 3]) -> Option<f64> {
    for (r, sources) in world.debug.face_source.iter().enumerate() {
        let Some(local) = sources.iter().position(|&s| s == id) else {
            continue;
        };
        let build = &world.debug.regions[r];
        let face = &build.faces[local * 10..local * 10 + 10];
        let first = rd16(face, 2) as usize;
        let count = face[7] as usize;
        let mut best = f64::MAX;
        for v in first..first + count {
            let at = v * 12;
            let p = [0, 2, 4]
                .map(|o| i16::from_le_bytes([build.vertices[at + o], build.vertices[at + o + 1]]));
            let d: f64 = (0..3)
                .map(|a| f64::from(i32::from(p[a]) - eye[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            best = best.min(d);
        }
        return Some(best);
    }
    None
}

/// Every face `inner` selects from a sampled viewpoint within `reach` of the
/// eye, `outer` selects too. (The renderer's frame selection keeps faces past
/// the far reject until projection throws them out, so a face farther than
/// `reach` is not a drawn face and may be absent.) A tighter PVS may select
/// fewer faces than a looser one: a face only in a loose set is hidden behind
/// something. Returns the totals selected by each, within reach.
fn assert_frames_contained(
    inner: &StreamedBrushWorld,
    outer: &StreamedBrushWorld,
    views: usize,
    seed: u32,
    reach: f64,
) -> (usize, usize) {
    configure_projection();
    assert_eq!(inner.index.regions.len(), outer.index.regions.len());
    let regions = inner.index.regions.len() as u16;
    let mut map_a = load_streamed(inner, regions);
    install_all(inner, &mut map_a);
    let mut map_b = load_streamed(outer, regions);
    install_all(outer, &mut map_b);
    let mut renderer_a = Renderer::new_pxbsp_with_nodes(map_a.faces().len(), map_a.nodes().len());
    let mut renderer_b = Renderer::new_pxbsp_with_nodes(map_b.faces().len(), map_b.nodes().len());
    let (mins, maxs) = inner.debug.world_bounds;
    let mut state = seed;
    let (mut done, mut attempts, mut drawn_inner, mut drawn_outer) = (0, 0, 0, 0);
    while done < views {
        attempts += 1;
        assert!(
            attempts < views * 400,
            "could not find {views} open positions"
        );
        let mut origin = [0i32; 3];
        for axis in 0..3 {
            let span = i32::from(maxs[axis]) - i32::from(mins[axis]);
            origin[axis] =
                (i32::from(mins[axis]) + (lcg(&mut state) as i32).rem_euclid(span.max(1))) << 12;
        }
        let camera = Camera {
            origin: Vec3I32 {
                x: origin[0],
                y: origin[1],
                z: origin[2],
            },
            angles: [
                (lcg(&mut state) % 4096) as i16 - 2048,
                (lcg(&mut state) % 4096) as i16,
                0,
            ],
        };
        let leaf = map_a.point_leaf_index(camera.origin).unwrap();
        if leaf == 0 || map_a.leaves().get(leaf).unwrap().contents == CONTENTS_SOLID {
            continue;
        }
        let (_, frame_a) = drawn_source_ids(inner, &map_a, &mut renderer_a, camera);
        let (_, frame_b) = drawn_source_ids(outer, &map_b, &mut renderer_b, camera);
        let eye = origin.map(|v| v >> 12);
        let in_reach =
            |id: &u32| nearest_vertex_distance(inner, *id, eye).is_some_and(|d| d <= reach);
        let wanted: Vec<u32> = frame_a.iter().copied().filter(in_reach).collect();
        let missing: Vec<_> = wanted.iter().filter(|id| !frame_b.contains(id)).collect();
        assert!(
            missing.is_empty(),
            "faces {missing:?} are within reach of {origin:?} angles {:?} and selected by the inner cook, not the outer",
            camera.angles,
        );
        drawn_inner += wanted.len();
        drawn_outer += frame_b.iter().filter(|id| in_reach(id)).count();
        done += 1;
    }
    (drawn_inner, drawn_outer)
}

fn cook_with(
    project: &ProjectDocument,
    root: &Path,
    mode: BrushWorldCookMode,
    pvs: StreamPvs,
) -> StreamedBrushWorld {
    match cook_project_gated(
        project,
        root,
        mode,
        [0; 3],
        &PartitionParams::default(),
        pvs,
    )
    .expect("cook")
    .world
    {
        CookedWorld::Streamed(world) => *world,
        CookedWorld::Whole(_) => panic!("expected more than one region"),
    }
}

/// The far-reject bound must not lose a face: everything the exact
/// whole-world flow draws is drawn by the clustered flow, and the clustered
/// flow draws nothing the conservative `mightsee` rows would not.
fn check_clustered_against_whole_world(
    project: &ProjectDocument,
    root: &Path,
    name: &str,
    seed: u32,
) {
    let reach = PartitionParams::default().vis_distance;
    let clustered = cook_with(
        project,
        root,
        BrushWorldCookMode::Draft,
        StreamPvs::Clustered { reach },
    );
    let exact = cook_with(
        project,
        root,
        BrushWorldCookMode::Release,
        StreamPvs::Global,
    );
    let (exact_drawn, clustered_drawn) =
        assert_frames_contained(&exact, &clustered, 400, seed, reach);
    eprintln!(
        "{name}: {exact_drawn} faces within reach selected by the whole-world exact flow, all selected by the clustered flow ({clustered_drawn} selected); widest |V| {} vs {}",
        clustered.stats.max_vis_count, exact.stats.max_vis_count
    );
    assert!(exact_drawn > 0);
}

#[test]
fn graybox_reach_clustered_visibility_loses_no_face_the_whole_world_flow_draws() {
    let (project, root) = graybox();
    check_clustered_against_whole_world(&project, &root, "graybox-reach", 0x5EED);
}

#[test]
fn stress_world_small_variant_clustered_visibility_loses_no_face() {
    let (project, root) = small_world();
    check_clustered_against_whole_world(&project, &root, "stress-small", 0xFA57);
}

#[test]
fn the_guest_install_draws_in_the_same_order_as_the_whole_map() {
    let (project, root) = graybox();
    // The partition the M7 guest build uses: three regions of about 16 sectors.
    let params = PartitionParams {
        region_target_bytes: 40_000,
        region_hard_cap_bytes: 80_000,
        ..PartitionParams::default()
    };
    let world = cook(&project, &root, &params);
    assert_eq!(world.index.regions.len(), 3);
    let sampled = compare_views_with(
        &world,
        300,
        0x5EED_0007,
        install_by_sectors_in_disc_order,
        true,
    );
    println!(
        "guest install: {} views, {} frame faces, pool {} B for {} B of payload",
        sampled.views,
        sampled.frame_faces_total,
        world.index.regions.len() * {
            let c = world.index.caps;
            c.faces as usize * 10
                + c.vertices as usize * 12
                + c.planes as usize * 12
                + c.marks as usize * 2
                + c.nodes as usize * 16
                + c.clip_nodes as usize * 6
                + c.leaves as usize * 14
                + c.vis_bytes as usize
        },
        world.payloads.iter().map(Vec::len).sum::<usize>(),
    );
    assert!(sampled.frame_faces_total > 0);
}

fn small_world() -> (ProjectDocument, PathBuf) {
    let donor = load_donor(&projects_dir()).expect("donor");
    let generated = generate(&StreamWorldConfig::small([4, 4]), &donor).expect("world");
    (generated.project, projects_dir().join("graybox-reach"))
}

#[test]
fn stress_world_small_variant_streamed_equals_whole() {
    let (project, root) = small_world();
    let world = cook(&project, &root, &PartitionParams::default());
    let sampled = compare_views(&world, 400, 0xBADC0DE);
    eprintln!(
        "stress-small: {} regions, {} views ({} skipped), {} frame faces",
        world.index.regions.len(),
        sampled.views,
        sampled.skipped,
        sampled.frame_faces_total
    );
    assert!(sampled.frame_faces_total > 0);
}

#[test]
fn the_cook_judges_its_own_numbers_not_the_partitioners_estimates() {
    use crate::brush_region::ClosureSource;

    let (project, root) = graybox();
    let params = PartitionParams::default();
    let gated = cook_project_gated(
        &project,
        &root,
        BrushWorldCookMode::Draft,
        [0; 3],
        &params,
        StreamPvs::Clustered {
            reach: params.vis_distance,
        },
    )
    .expect("cook");
    let CookedWorld::Streamed(world) = &gated.world else {
        panic!("graybox-reach streams");
    };
    let measured = gated
        .measured
        .as_ref()
        .expect("a streamed cook is re-gated");
    assert_eq!(gated.estimated.closure.source, ClosureSource::Sampled);
    assert_eq!(measured.closure.source, ClosureSource::PortalFlow);
    // Closure, payload sizes and skeleton are the cook's.
    for r in 0..world.index.regions.len() {
        let list: Vec<u32> = world
            .index
            .vis_list(r)
            .iter()
            .map(|&q| u32::from(q))
            .collect();
        let mut sorted = list.clone();
        sorted.sort_unstable();
        assert_eq!(measured.closure.visible[r], sorted);
        assert_eq!(
            measured.regions[r].counts.bytes() as usize,
            world.payloads[r].len()
        );
    }
    assert!(measured.gate.skeleton_measured);
    assert_eq!(measured.gate.skeleton_bytes, world.container.len() as u64);
    assert_eq!(
        measured.gate.row_leaf_cap,
        u32::from(world.index.caps.leaves)
    );
    // The estimate is left as it was.
    assert!(!gated.estimated.gate.skeleton_measured);
    // A one-region project has nothing to re-judge.
    let mut under = PartitionParams {
        region_target_bytes: 64 * 1024 * 1024,
        region_hard_cap_bytes: 64 * 1024 * 1024,
        pool_bytes: u32::MAX,
        ..PartitionParams::default()
    };
    under.caps = crate::brush_region::SlotCaps {
        faces: u32::MAX,
        vertices: u32::MAX,
        nodes: u32::MAX,
        leaves: u32::MAX,
        mark_surfaces: u32::MAX,
        clip_nodes: u32::MAX,
    };
    let whole = cook_project_gated(
        &project,
        &root,
        BrushWorldCookMode::Draft,
        [0; 3],
        &under,
        StreamPvs::Clustered { reach: 2860.0 },
    )
    .expect("cook");
    assert!(whole.measured.is_none());
}

#[test]
fn vestibules_keep_a_region_to_its_door_neighbours() {
    // A row of rooms joined by centred doors, a baffle inside each: a region
    // sees the rooms either side of it and nobody behind them.
    let donor = load_donor(&projects_dir()).expect("donor");
    let config = StreamWorldConfig {
        grid: [6, 1],
        corridor_pct: 0,
        interior_pct: 0,
        courtyard_pct: 0,
        terrain_pct: 0,
        max_hooks: 0,
        enemy_pct: 0,
        // Bare shells under a target no two of them fit in, so a cell is a
        // room.
        detail_bytes: 0,
        region_target_bytes: 12_288,
        ..StreamWorldConfig::default()
    };
    let world = generate(&config, &donor).expect("world");
    assert_eq!(world.stats.over_budget_modules, 0);
    let mut params = PartitionParams::default();
    world.overrides.apply(&mut params);
    let cooked = cook(
        &world.project,
        &projects_dir().join("graybox-reach"),
        &params,
    );
    let regions = cooked.index.regions.len();
    assert_eq!(regions, 6, "one region per room");
    // Region ids run along x, so the neighbours of r are r - 1 and r + 1.
    for r in 0..regions {
        let mut seen: Vec<usize> = cooked
            .index
            .vis_list(r)
            .iter()
            .map(|&q| q as usize)
            .collect();
        seen.sort_unstable();
        let expected: Vec<usize> = (r.saturating_sub(1)..=(r + 1).min(regions - 1)).collect();
        assert_eq!(seen, expected, "region {r}");
    }
}

// ---- gate (b): collision --------------------------------------------------

#[test]
fn collision_matches_the_whole_map_and_absent_regions_are_walls() {
    let (project, root) = graybox();
    let world = cook(&project, &root, &PartitionParams::default());
    let flat = flatten(&world);
    let flat_map = load_flat(&flat);
    let regions = world.index.regions.len() as u16;
    let mut map = load_streamed(&world, regions);
    let (mins, maxs) = world.debug.world_bounds;
    let mut state = 77u32;
    let sample = |state: &mut u32| {
        let mut p = [0i32; 3];
        for axis in 0..3 {
            let span = i32::from(maxs[axis]) - i32::from(mins[axis]);
            p[axis] = (i32::from(mins[axis]) + (lcg(state) as i32).rem_euclid(span.max(1))) << 12;
        }
        Vec3I32 {
            x: p[0],
            y: p[1],
            z: p[2],
        }
    };
    // Nothing installed: every hull reads solid everywhere.
    for _ in 0..500 {
        let p = sample(&mut state);
        for hull in 0..3 {
            assert_eq!(
                map.model_collision_hull(0, hull).unwrap().point_contents(p),
                Some(CONTENTS_SOLID),
                "hull {hull} with nothing installed"
            );
        }
    }
    install_all(&world, &mut map);
    let mut open = 0;
    for _ in 0..4000 {
        let p = sample(&mut state);
        for hull in 0..3 {
            let a = flat_map
                .model_collision_hull(0, hull)
                .unwrap()
                .point_contents(p);
            let b = map.model_collision_hull(0, hull).unwrap().point_contents(p);
            assert_eq!(a, b, "hull {hull} contents at {p:?}");
            open += usize::from(hull == 1 && a == Some(CONTENTS_EMPTY));
        }
        // And a short trace.
        let q = Vec3I32 {
            x: p.x + (lcg(&mut state) as i32 % 64 - 32) * 4096,
            y: p.y + (lcg(&mut state) as i32 % 64 - 32) * 4096,
            z: p.z + (lcg(&mut state) as i32 % 64 - 32) * 4096,
        };
        for hull in 1..3 {
            let mut scratch_a = psx_bsp::collision::TraceScratch::new();
            let mut scratch_b = psx_bsp::collision::TraceScratch::new();
            let mut ta = psx_bsp::collision::Trace::default();
            let mut tb = psx_bsp::collision::Trace::default();
            let ra = flat_map.model_collision_hull(0, hull).unwrap().trace_into(
                &p,
                &q,
                &mut scratch_a,
                &mut ta,
            );
            let rb = map.model_collision_hull(0, hull).unwrap().trace_into(
                &p,
                &q,
                &mut scratch_b,
                &mut tb,
            );
            assert_eq!(ra, rb);
            assert_eq!(
                (ta.fraction, ta.end),
                (tb.fraction, tb.end),
                "hull {hull} trace {p:?}->{q:?}"
            );
        }
    }
    assert!(open > 100, "the sample should hit open space ({open})");
    // Evicting a region turns exactly its cell into a wall.
    for victim in 0..regions {
        map.uninstall_region(victim).unwrap();
        map.check_integrity().unwrap();
        for _ in 0..800 {
            let p = sample(&mut state);
            let at_victim = map.unresident_region_at(p) == Some(victim);
            let c = map.model_collision_hull(0, 1).unwrap().point_contents(p);
            if at_victim {
                assert_eq!(c, Some(CONTENTS_SOLID));
                let leaf = map.point_leaf_index(p).unwrap();
                assert_eq!(map.leaves().get(leaf).unwrap().contents, CONTENTS_SOLID);
            }
        }
        let mut loader = RegionLoader::new(SliceReader::new(&world.region_pack), 0);
        let mut buf = vec![0u8; world.region_pack.len().max(4096)];
        let n = loader.load(&world.index, victim, &mut buf).unwrap();
        map.install_region(
            victim,
            map.streaming().unwrap().free_slot().unwrap(),
            &buf[..n],
        )
        .unwrap();
    }
    assert_eq!(
        map.install_region(0, 0, &world.payloads[0]),
        Err(StreamError::AlreadyInstalled)
    );
}

#[test]
fn random_install_orders_on_a_cooked_world_never_dangle() {
    let (project, root) = graybox();
    let world = cook(&project, &root, &PartitionParams::default());
    let regions = world.index.regions.len() as u16;
    let slots = (regions / 2).max(2);
    let mut state = 4242u32;
    let mut ops = 0;
    for _ in 0..30 {
        let mut map = load_streamed(&world, slots);
        for _ in 0..150 {
            let region = (lcg(&mut state) % u32::from(regions)) as u16;
            if map.streaming().unwrap().is_resident(region) {
                if lcg(&mut state).is_multiple_of(2) {
                    map.uninstall_region(region).unwrap();
                    ops += 1;
                }
            } else if let Some(slot) = map.streaming().unwrap().free_slot() {
                map.install_region(region, slot, &world.payloads[region as usize])
                    .unwrap();
                ops += 1;
            }
            map.check_integrity().unwrap_or_else(|e| panic!("{e:?}"));
        }
    }
    eprintln!("random install/uninstall: {ops} operations, integrity checked after each");
}

// ---- format and cook properties -------------------------------------------

#[test]
fn a_loader_without_streaming_support_refuses_the_container() {
    let (project, root) = graybox();
    let world = cook(&project, &root, &PartitionParams::default());
    let mut map = PxbspResidentMap::with_capacity(world.container.len() * 2);
    // The streaming loader accepts it only through load_streamed.
    assert_eq!(
        map.load(1, &mut SliceReader::new(&world.container)),
        Err(psx_bsp::pxbsp_resident::PxbspMapLoadError::StreamedWorldUnsupported)
    );
    // A loader that predates the feature validates leaf contents -6..=-1 and
    // would see the -7 stub leaves: build that view by erasing the index.
    let index = PxbspIndex::read(&mut SliceReader::new(&world.container)).unwrap();
    let leaves = index.lump(PxbspLumpKind::Leaves);
    let record = &world.container[leaves.offset as usize + 14..][..14];
    assert_eq!(record[0] as i8, -7, "stub leaf wire contents");
}

#[test]
fn cook_is_deterministic_and_regions_fit_their_declared_shape() {
    let (project, root) = graybox();
    let a = cook(&project, &root, &PartitionParams::default());
    let b = cook(&project, &root, &PartitionParams::default());
    assert_eq!(a.container, b.container);
    assert_eq!(a.region_pack, b.region_pack);
    for (r, entry) in a.index.regions.iter().enumerate() {
        assert_eq!(
            entry.fnv,
            psx_bsp::pxbsp_resident::stream::fnv1a32(&a.payloads[r])
        );
        assert!(entry.vis_count as usize * a.index.caps.leaves as usize / 8 <= 1024);
        assert_eq!(a.index.vis_list(r)[0] as usize, r);
        // Every face of the region has a stable source id.
        assert_eq!(a.debug.face_source[r].len(), a.stats.regions[r].faces);
    }
    eprintln!("{:#?}", a.stats.regions);
}

#[test]
fn under_budget_projects_cook_byte_identical_to_the_whole_map_path() {
    let (project, root) = graybox();
    // A pool and region size no partition needs to cut for.
    let params = PartitionParams {
        region_target_bytes: 64 * 1024 * 1024,
        region_hard_cap_bytes: 64 * 1024 * 1024,
        pool_bytes: u32::MAX,
        caps: crate::brush_region::SlotCaps {
            faces: u32::MAX,
            vertices: u32::MAX,
            nodes: u32::MAX,
            leaves: u32::MAX,
            mark_surfaces: u32::MAX,
            clip_nodes: u32::MAX,
        },
        ..PartitionParams::default()
    };
    let cooked = cook_project_streamed(&project, &root, BrushWorldCookMode::Draft, [0; 3], &params)
        .expect("cook");
    let CookedWorld::Whole(whole) = cooked else {
        panic!("one region must take the whole-map path");
    };
    let mut scaled = project.clone();
    crate::units::scale_project_to_engine_units(&mut scaled);
    let reference = compile_brush_world(
        &scaled,
        BrushWorldCookOptions {
            project_root: &root,
            mode: BrushWorldCookMode::Draft,
            ambient: [0; 3],
            texture_asset_base: 0,
            collision_hulls: whole_map_hull_strategy(&project),
        },
    )
    .expect("reference cook");
    assert_eq!(whole.pxbsp.bytes, reference.pxbsp.bytes);
    // And an empty StreamingIndex lump, as before.
    let index = PxbspIndex::read(&mut SliceReader::new(&whole.pxbsp.bytes)).unwrap();
    assert_eq!(index.lump(PxbspLumpKind::StreamingIndex).len, 0);
}
