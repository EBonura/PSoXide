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

struct TestBinder;

impl MaterialBinder for TestBinder {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        Ok(BoundMaterial {
            asset: material.map_or(0, |m| m.raw() as u16),
            flags: 0,
            blend: 0,
            uv_motion: 0,
            tint: [128; 3],
            textured: material.is_some(),
        })
    }
}

fn cook(world: &GridWorld) -> Result<CookedLevel, String> {
    cook_level(
        CookInput {
            world,
            binder: &mut TestBinder,
            lights: &[],
            first_pack_sector: 100,
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

fn load_reference(name: &str) -> Option<crate::ProjectDocument> {
    let path = crate::projects_dir().join(name).join("project.ron");
    path.is_file()
        .then(|| crate::ProjectDocument::load_from_path(path).expect("the reference project loads"))
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
        "grid-starter",
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
    }
}
