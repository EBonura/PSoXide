//! Collision parity: the native grid provider against the PXBSP collision
//! Play used before (thin slabs derived from the same document, cooked
//! through the BSP pipeline and traced with the PXBSP hull provider).
//!
//! The comparison is one motor tick from many starting points: from every
//! sampled floor cell, in eight directions, the same `CharacterMotorState`
//! update runs through both providers and the resulting positions and
//! `blocked` flags are compared. Square versus round bodies and the
//! different rounding of the two derivations mean they cannot agree to the
//! bit, so the test reports the disagreement and bounds it.

use psx_bsp::collision::TraceScratch;
use psx_bsp::collision_provider::{select_body_hull, PxbspCollisionProvider};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, CollisionTraceShape,
    RoomPoint, Q12,
};
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::write::aligned_copy;
use psx_grid::{CollisionView, LevelView, SECTOR_FLOOR_WALK_A, SECTOR_HAS_FLOOR};

use super::legacy_slabs;
use super::tests::{cook, load_reference, TestBinder};
use crate::brush_world::{compile_brush_world, BrushWorldCookMode, BrushWorldCookOptions};
use crate::ProjectDocument;

const RADIUS: i32 = 12;
const HEIGHT: i32 = 64;

/// Per-level census of one parity run.
#[derive(Default, Debug)]
struct Census {
    probes: usize,
    both_moved: usize,
    both_blocked: usize,
    native_blocked_only: usize,
    legacy_blocked_only: usize,
    max_xz: i32,
    max_y: i32,
    far: usize,
    walks: usize,
    walk_far: usize,
    walk_max: i32,
    examples: Vec<String>,
}

fn leak_bytes(bytes: &[u8]) -> &'static [u8] {
    let words: &'static mut [u32] = aligned_copy(bytes).leak();
    // SAFETY: the words live for the program and cover `bytes.len()` bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

fn legacy_map(project: &ProjectDocument, root: &std::path::Path) -> (PxbspResidentMap, usize) {
    let world = project.active_scene().world.grid().expect("a grid project");
    let derived = legacy_slabs::derive(world, None);
    let mut p = project.clone();
    p.bsp_cook_mode = BrushWorldCookMode::Draft;
    let scene = p.active_scene_mut();
    scene.brushes.extend(derived.brushes);
    scene.brushes.extend(derived.lid);
    crate::units::scale_project_to_engine_units(&mut p);
    let compiled = compile_brush_world(
        &p,
        BrushWorldCookOptions {
            project_root: root,
            mode: BrushWorldCookMode::Draft,
            ambient: [32; 3],
            texture_asset_base: 0,
            collision_hulls: crate::brush_collision_hulls::CollisionHullStrategy::SpatialChains,
        },
    )
    .expect("the legacy slabs compile");
    let hull = select_body_hull(&compiled.body_hulls, RADIUS, HEIGHT).expect("a body hull");
    let map = PxbspResidentMap::from_static(0, leak_bytes(&compiled.pxbsp.bytes)).expect("a map");
    (map, hull)
}

fn directions() -> [(i32, i32); 8] {
    [
        (4096, 0),
        (-4096, 0),
        (0, 4096),
        (0, -4096),
        (2896, 2896),
        (-2896, 2896),
        (2896, -2896),
        (-2896, -2896),
    ]
}

fn run(name: &str) -> Option<Census> {
    let project = load_reference(name)?;
    let world = project.active_scene().world.grid().expect("a grid project");
    let cooked = cook(world).unwrap();
    let core = leak_bytes(&cooked.core);
    let level = LevelView::parse(core).unwrap();
    let rooms: Vec<Option<CollisionView<'static>>> = cooked
        .chunks
        .iter()
        .map(|c| Some(CollisionView::parse(leak_bytes(c)).unwrap()))
        .collect();
    let rooms: &'static [Option<CollisionView<'static>>] = rooms.leak();
    let native = GridCollision::new(level, rooms).expect("a collision world");
    let root = crate::projects_dir().join(name);
    let (map, hull) = legacy_map(&project, &root);
    let mut scratch = TraceScratch::new();
    let config = CharacterMotorConfig::character_with_body(
        RADIUS,
        HEIGHT,
        3 << 8,
        5 << 8,
        Angle::from_q12(1024),
    );
    let s = level.sector_units() as i32;
    let mut census = Census::default();
    for (ri, chunk) in cooked.tables.iter().enumerate() {
        let origin = level.rooms[ri].origin;
        let cols = usize::from(chunk.width);
        let stride = (chunk.sectors.len() / 120).max(1);
        for (index, sector) in chunk.sectors.iter().enumerate().step_by(stride) {
            if sector.flags & SECTOR_HAS_FLOOR == 0 || sector.flags & SECTOR_FLOOR_WALK_A == 0 {
                continue;
            }
            let (cx, cz) = ((index % cols) as i32, (index / cols) as i32);
            for (ox, oz) in [(s / 2, s / 2 + 3), (s / 5, s / 2 + 1), (4 * s / 5, s / 3)] {
                let Some(floor) =
                    native.floor_at(ri, cx as usize, cz as usize, ox as u32, oz as u32)
                else {
                    continue;
                };
                let start = RoomPoint::new(origin[0] + cx * s + ox, floor, origin[2] + cz * s + oz);
                for (dx, dz) in directions() {
                    let input = CharacterMotorInput {
                        move_x: Q12::from_raw(dx),
                        move_z: Q12::from_raw(dz),
                        ..CharacterMotorInput::default()
                    };
                    let mut step = |native_side: bool, ticks: u32| {
                        let mut motor = CharacterMotorState::new(start, Angle::ZERO);
                        let idle = CharacterMotorInput::default();
                        let mut last = None;
                        for tick in 0..(3 + ticks) {
                            let i = if tick >= 3 { input } else { idle };
                            let frame = if native_side {
                                let mut p = GridTraceProvider::new(&native);
                                motor.update_vblanks_with_trace_provider(&mut p, i, config, 1)
                            } else {
                                let mut p = PxbspCollisionProvider::new(
                                    &map,
                                    hull,
                                    &[],
                                    CollisionTraceShape::Body {
                                        radius: RADIUS,
                                        height: HEIGHT,
                                    },
                                    &mut scratch,
                                )
                                .expect("a provider");
                                motor.update_vblanks_with_trace_provider(&mut p, i, config, 1)
                            };
                            last = frame.ok();
                        }
                        last
                    };
                    let (Some(a), Some(b)) = (step(true, 1), step(false, 1)) else {
                        continue;
                    };
                    if let (Some(wa), Some(wb)) = (step(true, 24), step(false, 24)) {
                        census.walks += 1;
                        let d = (wa.position.x - wb.position.x)
                            .abs()
                            .max((wa.position.z - wb.position.z).abs())
                            .max((wa.position.y - wb.position.y).abs());
                        census.walk_max = census.walk_max.max(d);
                        if d > 8 {
                            census.walk_far += 1;
                            if census.examples.len() < 8 {
                                census.examples.push(format!(
                                    "WALK room {ri} cell {cx},{cz} start {:?} dir ({dx},{dz}): native {:?} legacy {:?}",
                                    (start.x, start.y, start.z),
                                    (wa.position.x, wa.position.y, wa.position.z),
                                    (wb.position.x, wb.position.y, wb.position.z)
                                ));
                            }
                        }
                    }
                    census.probes += 1;
                    let dxz = (a.position.x - b.position.x)
                        .abs()
                        .max((a.position.z - b.position.z).abs());
                    let dy = (a.position.y - b.position.y).abs();
                    match (a.blocked, b.blocked) {
                        (true, true) => census.both_blocked += 1,
                        (false, false) => census.both_moved += 1,
                        (true, false) => census.native_blocked_only += 1,
                        (false, true) => census.legacy_blocked_only += 1,
                    }
                    census.max_xz = census.max_xz.max(dxz);
                    census.max_y = census.max_y.max(dy);
                    if dxz > 3 || dy > 3 || a.blocked != b.blocked {
                        if dxz > 3 || dy > 3 {
                            census.far += 1;
                        }
                        if census.examples.len() < 8 {
                            census.examples.push(format!(
                                "room {ri} cell {cx},{cz} start {:?} dir ({dx},{dz}): native {:?} blocked={} legacy {:?} blocked={} floors {:?}",
                                (start.x, start.y, start.z), (a.position.x, a.position.y, a.position.z), a.blocked,
                                (b.position.x, b.position.y, b.position.z), b.blocked, sector.floor
                            ));
                        }
                    }
                }
            }
        }
    }
    eprintln!("{name}: {census:?}");
    for e in &census.examples {
        eprintln!("   {e}");
    }
    Some(census)
}

#[test]
fn native_collision_matches_the_retired_slab_collision() {
    let _ = TestBinder;
    for name in [
        "grid-starter",
        "grid-level008",
        "grid-dungeon",
        "grid-cathedral",
    ] {
        let Some(c) = run(name) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
        assert!(c.probes > 100, "{name}: {} probes", c.probes);
        // Disagreements are bounded; the figures are printed above.
        assert!(
            c.walk_far * 10 <= c.walks,
            "{name}: {} of {} 24-tick walks end more than 8 units apart: {c:?}",
            c.walk_far,
            c.walks
        );
        let disagree = c.native_blocked_only + c.legacy_blocked_only + c.far;
        assert!(
            disagree * 20 <= c.probes,
            "{name}: {disagree} of {} probes disagree: {c:?}",
            c.probes
        );
    }
}
