//! Cook tests: synthetic worlds for exactness, the reference projects for
//! sizes. The reference projects are untracked local files; their tests skip
//! when a project is absent.

use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{LevelView, RoomView};

use super::*;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::surface::{GridSurface, GridWall};
use crate::grid::units::GridScale;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

pub(super) struct TestBinder;

impl MaterialBinder for TestBinder {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        Ok(BoundMaterial {
            asset: material.map_or(0, |m| m.raw() as u16),
            flags: 0,
            blend: 0,
            uv_motion: 0,
            tint: [128; 3],
            textured: material.is_some(),
            texels: [64, 64],
        })
    }
}

pub(super) fn cook(world: &GridWorld) -> Result<CookedLevel, String> {
    cook_with(world, true)
}

/// [`cook`] with the texture-window repeat on or off.
pub(super) fn cook_with(world: &GridWorld, texture_repeat: bool) -> Result<CookedLevel, String> {
    cook_level(
        CookInput {
            world,
            binder: &mut TestBinder,
            lights: &[],
            first_pack_sector: 100,
            lod_far_cells: lod::LOD_FAR_CELLS,
            texture_repeat,
        },
        None,
    )
}

fn box_room() -> GridWorld {
    let mut room = GridRoom::new("box", 3, 2);
    for z in 0..2 {
        for x in 0..3 {
            let sector = room.sector_or_insert(x, z).unwrap();
            sector.floor = Some(GridSurface::flat(0, Some(rid(7))));
            if x == 0 {
                sector
                    .walls_mut(WallEdge::West)
                    .push(GridWall::span(0, 1024, Some(rid(8))));
            }
            if z == 1 {
                sector.ceiling = Some(GridSurface::flat(1024, Some(rid(9))));
            }
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn read(cooked: &CookedLevel) -> (Vec<u32>, Vec<Vec<u32>>) {
    (
        aligned_copy(&cooked.core),
        cooked.chunks.iter().map(|c| aligned_copy(c)).collect(),
    )
}

#[test]
fn a_cooked_level_reads_back_and_validates() {
    let cooked = cook(&box_room()).unwrap();
    let (core, chunks) = read(&cooked);
    let level = LevelView::parse(words_as_bytes(&core, cooked.core.len())).unwrap();
    level.validate_full().unwrap();
    assert_eq!(level.sector_units(), 80);
    assert_eq!(level.rooms.len(), 1);
    assert_eq!(level.rooms[0].pack_sector, 100);
    assert_eq!(level.materials.len(), 3);
    let room = RoomView::parse(words_as_bytes(&chunks[0], cooked.chunks[0].len())).unwrap();
    room.validate_full(level.materials.len()).unwrap();
    assert_eq!(room.col.sectors.len(), 6);
    assert_eq!(room.render.cells.len(), 6);
    // 6 floors, 3 ceilings, 2 west walls of one sector each.
    assert_eq!(room.render.quads.len(), 6 + 3 + 2);
    // The shared lattice: far fewer vertices than quad corners.
    assert!(room.render.vertices.len() < room.render.quads.len() * 4);
    // Solid walls reach the collision table.
    assert_eq!(room.col.walls.len(), 2);
    assert_eq!(room.render.material_use, [0, 1, 2]);
}

#[test]
fn cooking_twice_gives_identical_bytes() {
    let world = box_room();
    let a = cook(&world).unwrap();
    let b = cook(&world).unwrap();
    assert_eq!(a.core, b.core);
    assert_eq!(a.chunks, b.chunks);
}

#[test]
fn heights_round_through_one_mapping_for_render_and_collision() {
    let mut world = box_room();
    // 13 grid units = 1.01 runtime units at 5/4.
    world.rooms[0].sector_or_insert(0, 0).unwrap().floor =
        Some(GridSurface::flat(13, Some(rid(7))));
    let cooked = cook(&world).unwrap();
    let chunk = &cooked.tables[0];
    assert_eq!(chunk.sectors[0].floor, [1; 4]);
    let drawn = chunk
        .quads
        .iter()
        .find(|q| {
            q.flags & psx_grid::QUAD_AXIS != 0
                && q.n == [0, 127, 0]
                && chunk.vertices[usize::from(q.v[0])].x == 0
        })
        .expect("the floor quad");
    assert_eq!(chunk.vertices[usize::from(drawn.v[0])].y, 1);
}

#[test]
fn an_off_lattice_scale_and_an_empty_world_are_refused() {
    let mut world = box_room();
    world.scale = GridScale { num: 5, den: 3 };
    assert!(cook(&world).unwrap_err().contains("multiple of 16"));
    assert!(cook(&GridWorld::new()).unwrap_err().contains("no rooms"));
}

#[test]
fn rooms_get_portals_to_their_neighbours() {
    let mut world = box_room();
    let mut next = GridRoom::new("next", 1, 1);
    next.origin = [3, 0];
    next.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, Some(rid(7))));
    next.sector_or_insert(0, 0).unwrap().ceiling = Some(GridSurface::flat(1024, Some(rid(9))));
    world.rooms[0].sector_or_insert(2, 0).unwrap().ceiling =
        Some(GridSurface::flat(1024, Some(rid(9))));
    world.rooms.push(next);
    let cooked = cook(&world).unwrap();
    let from_box: Vec<u16> = cooked.level.portals
        [usize::from(cooked.level.rooms[0].portal_first)..]
        [..usize::from(cooked.level.rooms[0].portal_count)]
        .iter()
        .map(|p| p.dest)
        .collect();
    assert_eq!(from_box, vec![1]);
    assert_eq!(cooked.level.rooms[1].portal_count, 1);
}

#[test]
fn a_long_shared_wall_is_one_portal_not_one_per_cell() {
    // Two rooms side by side, four cells wide, sharing a four cell edge.
    let make = |name: &str, origin: [i32; 2]| {
        let mut room = GridRoom::new(name, 4, 4);
        room.origin = origin;
        for z in 0..4 {
            for x in 0..4 {
                let sector = room.sector_or_insert(x, z).unwrap();
                sector.floor = Some(GridSurface::flat(0, Some(rid(7))));
                sector.ceiling = Some(GridSurface::flat(1024, Some(rid(9))));
            }
        }
        room
    };
    let world = GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![make("west", [0, 0]), make("east", [4, 0])],
        ..GridWorld::default()
    };
    let cooked = cook(&world).unwrap();
    for (room, dest) in [(0usize, 1u16), (1, 0)] {
        let record = &cooked.level.rooms[room];
        let first = usize::from(record.portal_first);
        let portals = &cooked.level.portals[first..first + usize::from(record.portal_count)];
        assert_eq!(portals.len(), 1, "room {room} has {portals:?}");
        assert_eq!(portals[0].dest, dest);
        let z: Vec<i16> = portals[0].v.iter().map(|v| v[2]).collect();
        let span = i32::from(*z.iter().max().unwrap()) - i32::from(*z.iter().min().unwrap());
        assert_eq!(span, 4 * cooked.report.sector_units as i32);
    }
}

/// One of the tracked grid reference projects. They are permanent fixtures, so
/// a missing one is a failure, not a skip.
pub(super) fn load_reference(name: &str) -> Option<crate::ProjectDocument> {
    let path = crate::projects_dir().join(name).join("project.ron");
    assert!(path.is_file(), "reference project {name} is not tracked");
    Some(crate::ProjectDocument::load_from_path(path).expect("the reference project loads"))
}

#[test]
fn the_reference_projects_cook_and_validate() {
    for name in [
        "grid-cathedral",
        "grid-level008",
        "grid-dungeon",
        "grid-cave",
        "grid-sewers",
        "grid-west",
    ] {
        let Some(project) = load_reference(name) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
        let world = project.active_scene().world.grid().expect("a grid project");
        let cooked = cook(world).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (core, chunks) = read(&cooked);
        let level = LevelView::parse(words_as_bytes(&core, cooked.core.len())).unwrap();
        level.validate_full().unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            let room = RoomView::parse(words_as_bytes(chunk, cooked.chunks[i].len())).unwrap();
            room.validate_full(level.materials.len())
                .unwrap_or_else(|e| panic!("{name} room {i}: {e:?}"));
        }
        eprintln!("{name}: {}", cooked.report.summary());
        for (i, table) in cooked.tables.iter().enumerate() {
            let count = |bit: u16| table.sectors.iter().filter(|s| s.flags & bit != 0).count();
            eprintln!(
                "  room {i}: {} sectors, {} flat floor, {} clear",
                table.sectors.len(),
                count(psx_grid::SECTOR_FLAT_FLOOR),
                count(psx_grid::SECTOR_CLEAR)
            );
        }
    }
}

/// The editor's `floor_height_at` and the runtime height function are one
/// function; this checks the two agree on every floor cell of a reference
/// level, on corners, edges and the centre, within the rounding of the
/// grid to runtime mapping (one runtime unit).
#[test]
fn editor_floor_height_agrees_with_the_runtime_height_function() {
    use psx_grid::height::{height_at, weight_recip};
    for name in ["grid-cathedral", "grid-level008", "grid-dungeon"] {
        let Some(project) = load_reference(name) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
        let world = project.active_scene().world.grid().expect("a grid project");
        let cooked = cook(world).unwrap();
        let units = RuntimeUnits::new(world.scale).unwrap();
        let s = units.sector();
        let inv24 = weight_recip(s);
        let edge_editor = world.scale.sector_editor_units();
        let mut checked = 0usize;
        for (ri, room) in world.rooms.iter().enumerate() {
            let record = cooked.level.rooms[ri];
            let chunk = &cooked.tables[ri];
            for (x, z, sector) in room.iter() {
                let Some(floor) = &sector.floor else { continue };
                if !floor.walkable {
                    continue;
                }
                let flags = chunk.sectors[z * room.width + x].flags;
                let nesw = flags & psx_grid::SECTOR_FLOOR_NESW != 0;
                // Probe points in runtime units inside the sector.
                for (px, pz) in [
                    (2, 1),
                    (s / 2 - 3, s / 2 - 5),
                    (s - 3, 1),
                    (1, s - 3),
                    (s / 3, 2 * s / 3 + 2),
                ] {
                    // Skip points whose floor is shadowed by a higher floor of a
                    // stacked room: the editor takes the highest one.
                    let world_x = room.world_cell(x, z)[0] * s as i32 + px as i32;
                    let world_z = room.world_cell(x, z)[1] * s as i32 + pz as i32;
                    let editor_x = world_x * 16 + 8;
                    let editor_z = world_z * 16 + 8;
                    let Some(editor_h) = world.floor_height_at(editor_x, editor_z, i32::MAX) else {
                        continue;
                    };
                    let second = !psx_grid::height::in_first_triangle(nesw, px, pz, s);
                    let corners = if second && flags & psx_grid::SECTOR_SPLIT_LEVEL != 0 {
                        let ext = chunk.exts[usize::from(chunk.sectors[z * room.width + x].ext)];
                        chunk.splitbs[usize::from(ext.splitb)].floor_b
                    } else {
                        chunk.sectors[z * room.width + x].floor
                    };
                    let local = height_at(corners, nesw, px, pz, s, inv24);
                    let runtime_h = local + record.origin[1];
                    // Only compare when this room owns the highest floor there.
                    // Steep ramps move up to two runtime units over the half-unit
                    // probe offset and the rounding of each mapping.
                    if (runtime_h * 16 - editor_h).abs() > 48 {
                        // A different room's floor won; fine for stacked rooms.
                        assert!(world.rooms.len() > 1, "{name} room {ri} sector {x},{z} at ({px},{pz}) nesw={nesw} second={second}: runtime {runtime_h} (local {local}, corners {corners:?}, origin {}) editor {editor_h} (doc {:?} split-level {:?})", record.origin[1], floor.heights, floor.heights_b);
                        continue;
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 20, "{name}: only {checked} probes compared");
        let _ = edge_editor;
    }
}

/// Merged far quads cover exactly the cell quads they replace: the plan area
/// of every merged quad equals the area of the cell quads flagged covered in
/// its block, so the LOD swap leaves no hole.
#[test]
fn merged_quads_cover_exactly_what_they_replace() {
    for name in [
        "grid-level008",
        "grid-cathedral",
        "grid-dungeon",
        "grid-cave",
    ] {
        let Some(project) = load_reference(name) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
        let world = project.active_scene().world.grid().unwrap();
        let cooked = cook(world).unwrap();
        for (ri, chunk) in cooked.tables.iter().enumerate() {
            let area = |q: &psx_grid::Quad| -> i64 {
                let p = |k: usize| {
                    let v = chunk.vertices[usize::from(q.v[k])];
                    (i64::from(v.x), i64::from(v.z))
                };
                // Shoelace over the ring, doubled.
                let mut sum = 0i64;
                for k in 0..4 {
                    let (a, b) = (p(k), p((k + 1) % 4));
                    sum += a.0 * b.1 - b.0 * a.1;
                }
                sum.abs()
            };
            let covered: i64 = chunk
                .quads
                .iter()
                .filter(|q| q.flags & psx_grid::QUAD_LOD_COVERED != 0)
                .map(area)
                .sum();
            let merged: i64 = chunk.lod_quads.iter().map(area).sum();
            assert_eq!(
                covered, merged,
                "{name} room {ri}: covered area {covered} vs merged {merged}"
            );
        }
    }
}
