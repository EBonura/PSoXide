//! Placed meshes through the cook: render runs, collision records, the
//! character walking and standing on the cooked result, and the golden rule
//! that a world with no meshes cooks exactly as before.

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint, Q12,
};
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::write::aligned_copy;
use psx_grid::{
    plane_kind, CollisionView, LevelView, RoomView, CHUNK_HAS_PLANES, QUAD_PLANE,
};

use super::tests::cook;
use crate::grid::mesh::{
    ArchProfile, ArchSpec, ColumnSpec, GridMesh, ImportProxy, ImportedMesh, MeshAsset, MeshKind,
    RubbleSpec,
};
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::{GridScale, SECTOR_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

fn leak(bytes: &[u8]) -> &'static [u8] {
    let words: &'static mut [u32] = aligned_copy(bytes).leak();
    // SAFETY: the words live for the program and cover `bytes.len()` bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

/// A 12 by 8 sector hall with a flat floor (a sector is 80 runtime units).
fn hall() -> GridWorld {
    let mut room = GridRoom::new("hall", 12, 8);
    for z in 0..8 {
        for x in 0..12 {
            room.sector_or_insert(x, z).unwrap().floor = Some(GridSurface::flat(0, Some(rid(7))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// The arch of the tests: a 2 sector opening, 120 units to the springing line,
/// a metre over, a band of 20 units, one sector deep.
fn arch_spec() -> ArchSpec {
    ArchSpec::default()
}

/// A hall with one arch at the middle, its axis along z (the opening runs
/// along x at yaw 0, so the walkers go along z through it).
fn arch_hall() -> GridWorld {
    let mut world = hall();
    world.mesh_assets.push(MeshAsset::new("arch", MeshKind::Arch(arch_spec())));
    let mut mesh = GridMesh::new(1, 0, [6 * SECTOR_UNITS, 0, 4 * SECTOR_UNITS]);
    mesh.set_material(0, Some(rid(8)));
    mesh.set_material(1, Some(rid(9)));
    world.rooms[0].meshes.push(mesh);
    world
}

struct Native {
    collision: GridCollision<'static>,
}

fn native(world: &GridWorld) -> (Native, crate::grid::cook::CookedLevel) {
    let cooked = cook(world).unwrap();
    let level = LevelView::parse(leak(&cooked.core)).unwrap();
    let rooms: Vec<Option<CollisionView<'static>>> = cooked
        .chunks
        .iter()
        .map(|c| Some(CollisionView::parse(leak(c)).unwrap()))
        .collect();
    let rooms: &'static [Option<CollisionView<'static>>] = rooms.leak();
    let collision = psx_grid::ResidentRooms::from_collisions(level, rooms)
        .expect("a collision world")
        .leak_collision();
    (Native { collision }, cooked)
}

fn walk(world: &Native, from: RoomPoint, move_x: i32, move_z: i32, ticks: u32) -> CharacterMotorState {
    let config =
        CharacterMotorConfig::character_with_body(12, 48, 8 << 8, 8 << 8, Angle::from_q12(1024));
    let mut motor = CharacterMotorState::new(from, Angle::ZERO);
    let input = CharacterMotorInput {
        move_x: Q12::from_raw(move_x),
        move_z: Q12::from_raw(move_z),
        ..CharacterMotorInput::default()
    };
    for _ in 0..ticks {
        let mut provider = GridTraceProvider::new(&world.collision);
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, config, 1)
            .expect("in range");
    }
    motor
}

#[test]
fn a_mesh_cooks_to_one_plane_run_and_validates() {
    let world = arch_hall();
    let built = world.mesh_assets[0].build().unwrap();
    let cooked = cook(&world).unwrap();
    let level = LevelView::parse(leak(&cooked.core)).unwrap();
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    room.validate_full(level.materials.len()).unwrap();
    assert_ne!(room.col.header.flags & CHUNK_HAS_PLANES, 0);
    let refs = room.render.plane_refs;
    assert_eq!(refs.len(), 1);
    let run = &room.render.quads
        [usize::from(refs[0].quad)..usize::from(refs[0].quad) + usize::from(refs[0].count)];
    assert!(run.iter().all(|q| q.flags & QUAD_PLANE != 0));
    // Every asset face is a quad of the run (the texture splitter may cut a
    // long one, never merge).
    assert!(run.len() >= built.faces.len());
    // The run's box sits over the arch's sectors: 4 across (the opening and
    // both piers), one deep... a sector either side of the centre.
    assert!(refs[0].x0 <= 5 && refs[0].x1 >= 6, "{:?}", (refs[0].x0, refs[0].x1));
    assert!(refs[0].z0 >= 3 && refs[0].z1 <= 4);
    // Two materials were used by the instance, so the room draws three
    // (the floor's and the arch's two).
    assert_eq!(room.render.material_use.len(), 4);
    // The bytes are a pure function of the document.
    let again = cook(&world).unwrap();
    assert_eq!(again.chunks, cooked.chunks);
    assert_eq!(again.core, cooked.core);
}

#[test]
fn an_arch_cooks_to_a_slab_per_piece_and_a_box_per_pier() {
    let world = arch_hall();
    let cooked = cook(&world).unwrap();
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    let mut ids: Vec<u8> = room.col.col_planes.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    // 8 slabs and 2 piers.
    assert_eq!(ids.len(), 10, "{ids:?}");
    // Ids continue after the room's (zero) planes.
    assert_eq!(ids[0], 0);
    let kinds = |kind: u8| {
        room.col
            .col_planes
            .iter()
            .filter(|p| p.kind == kind && p.flags & psx_grid::COLPLANE_PRIMARY != 0)
            .count()
    };
    // Slabs near the crown face up (platforms), the haunches are walls or
    // blockers, the piers blockers.
    assert!(kinds(plane_kind::PLATFORM) >= 2);
    assert!(kinds(plane_kind::BLOCKER) >= 2);
}

#[test]
fn a_body_walks_through_the_opening_and_stops_at_a_pier() {
    let (native, cooked) = native(&arch_hall());
    // The arch stands at (480, 320); its opening is 160 wide from x = 400,
    // the piers 20 wide beside it, the band 40 deep (z = 280 to 360).
    let through = walk(&native, RoomPoint::new(480, 0, 230), 0, 4096, 120).position();
    assert!(through.z > 360 + 20, "walked through: {through:?}");
    assert_eq!(through.y, 0, "{through:?}");
    // From the right the right pier stops it at its outer face, x = 580.
    let blocked = walk(&native, RoomPoint::new(700, 0, 320), -4096, 0, 200).position();
    assert!(blocked.x > 580 && blocked.x < 580 + 12 + 14, "{blocked:?}");
    // The only complaint: the haunch pieces block as boxes (one line).
    assert_eq!(cooked.report.warnings.len(), 1, "{:?}", cooked.report.warnings);
    assert!(cooked.report.warnings[0].contains("over-blocks"));
}

#[test]
fn the_crown_of_the_arch_is_a_surface_to_stand_on() {
    let (native, _) = native(&arch_hall());
    // Dropped over the crown, the body lands on the extrados: spring 120 +
    // rise 80 + band 20 = 220 runtime units.
    let landed = walk(&native, RoomPoint::new(480, 300, 320), 0, 0, 60);
    let y = landed.position().y;
    assert!((y - 220).abs() <= 2, "stood at {y}");
    assert!(landed.grounded());
    // Beside the arch the same drop falls to the floor.
    let beside = walk(&native, RoomPoint::new(480, 300, 160), 0, 0, 60);
    assert_eq!(beside.position().y, 0);
}

#[test]
fn a_turned_arch_collides_where_it_is_drawn() {
    // Turned a quarter: the opening runs along z, so a body goes through it
    // along x.
    let mut world = arch_hall();
    world.rooms[0].meshes[0].rotation = [90, 0, 0];
    let (native, _) = native(&world);
    let through = walk(&native, RoomPoint::new(380, 0, 320), 4096, 0, 120).position();
    assert!(through.x > 480 + 40 + 20, "{through:?}");
    // And the old opening line is now a pier face.
    let blocked = walk(&native, RoomPoint::new(480, 0, 160), 0, 4096, 200).position();
    assert!(blocked.z < 320 - 10, "{blocked:?}");
}

#[test]
fn a_column_blocks_and_has_a_standable_capital() {
    let mut world = hall();
    world.mesh_assets.push(MeshAsset::new("column", MeshKind::Column(ColumnSpec::default())));
    world.rooms[0]
        .meshes
        .push(GridMesh::new(1, 0, [6 * SECTOR_UNITS, 0, 4 * SECTOR_UNITS]));
    let (native, _) = native(&world);
    // The plinth is 1.5 radii wide: 192 grid units = 15 runtime units of
    // radius, 22.5 of plinth half width.
    let blocked = walk(&native, RoomPoint::new(380, 0, 320), 4096, 0, 200).position();
    assert!(blocked.x < 480 - 20, "{blocked:?}");
    // A body dropped over the capital stands on it: height 2560 grid = 200.
    let on_top = walk(&native, RoomPoint::new(480, 260, 320), 0, 0, 60);
    assert!((on_top.position().y - 200).abs() <= 2, "{:?}", on_top.position());
}

#[test]
fn rubble_draws_and_does_not_collide() {
    let mut world = hall();
    world.mesh_assets.push(MeshAsset::new("rubble", MeshKind::Rubble(RubbleSpec::default())));
    world.rooms[0]
        .meshes
        .push(GridMesh::new(1, 0, [6 * SECTOR_UNITS, 0, 4 * SECTOR_UNITS]));
    let (native, cooked) = native(&world);
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    assert_eq!(room.render.plane_refs.len(), 1);
    assert!(room.col.col_planes.is_empty());
    let through = walk(&native, RoomPoint::new(380, 0, 320), 4096, 0, 200).position();
    assert!(through.x > 560, "{through:?}");
}

#[test]
fn an_imported_mesh_and_a_decor_only_instance_cook() {
    let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\n\
               f 5 6 7 8\nf 2 1 4 3\nf 1 5 8 4\nf 6 2 3 7\nf 8 7 3 4\nf 1 2 6 5\n";
    let crate_mesh = ImportedMesh::from_obj(obj, 512.0, "crate.obj", ImportProxy::BoxTop).unwrap();
    let mut world = hall();
    world.mesh_assets.push(MeshAsset::new("crate", MeshKind::Imported(crate_mesh)));
    world.rooms[0]
        .meshes
        .push(GridMesh::new(1, 0, [3 * SECTOR_UNITS, 0, 3 * SECTOR_UNITS]));
    let mut decor = GridMesh::new(2, 0, [8 * SECTOR_UNITS, 0, 3 * SECTOR_UNITS]);
    decor.collide = false;
    world.rooms[0].meshes.push(decor);
    let cooked = cook(&world).unwrap();
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    assert_eq!(room.render.plane_refs.len(), 2);
    // One instance collides (four walls and a top), the other not at all.
    let primary = room
        .col
        .col_planes
        .iter()
        .filter(|p| p.flags & psx_grid::COLPLANE_PRIMARY != 0)
        .count();
    assert_eq!(primary, 5);
}

#[test]
fn a_mesh_turned_off_a_quarter_still_blocks_exactly_and_warns_when_pitched() {
    let mut world = hall();
    world.mesh_assets.push(MeshAsset::new("column", MeshKind::Column(ColumnSpec::default())));
    let mut m = GridMesh::new(1, 0, [6 * SECTOR_UNITS, 0, 4 * SECTOR_UNITS]);
    m.rotation = [30, 0, 0];
    world.rooms[0].meshes.push(m);
    let cooked = cook(&world).unwrap();
    assert!(cooked.report.warnings.is_empty(), "{:?}", cooked.report.warnings);
    world.rooms[0].meshes[0].rotation = [30, 10, 0];
    let cooked = cook(&world).unwrap();
    assert!(cooked
        .report
        .warnings
        .iter()
        .any(|w| w.contains("bounding box")));
}

#[test]
fn a_world_without_meshes_cooks_as_it_always_did() {
    // The mesh tables are empty and the chunk has no plane marker.
    let world = hall();
    let cooked = cook(&world).unwrap();
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    assert!(room.render.plane_refs.is_empty());
    assert_eq!(room.col.header.flags & CHUNK_HAS_PLANES, 0);
    // An asset nobody places changes nothing either.
    let mut with_asset = hall();
    with_asset
        .mesh_assets
        .push(MeshAsset::new("arch", MeshKind::Arch(ArchSpec::default())));
    let again = cook(&with_asset).unwrap();
    assert_eq!(again.chunks, cooked.chunks);
    assert_eq!(again.core, cooked.core);
}

#[test]
fn the_crown_socket_hook_point_is_96_under_the_grip() {
    use crate::playtest::mesh_sockets_for_test;
    let world = arch_hall();
    let hooks = mesh_sockets_for_test(&world);
    assert_eq!(hooks.len(), 1);
    // Crown intrados: spring 1536 + rise 1024 = 2560 gu, the grip 32 under it
    // (2528 gu = 197.5, rounded to 198 runtime units); the record sits 96
    // below the grip, at the arch's centre.
    assert_eq!((hooks[0].x, hooks[0].y, hooks[0].z), (480, 198 - 96, 320));
    let mut turned = world.clone();
    turned.rooms[0].meshes[0].rotation = [90, 0, 0];
    let hook = mesh_sockets_for_test(&turned)[0];
    assert_eq!((hook.x, hook.z), (480, 320));
    // The heading follows the instance's yaw (Q12: 1024 is a quarter).
    assert_eq!(hook.yaw, 1024);
    // Another profile keeps the crown at spring + rise.
    let mut pointed = world.clone();
    pointed.mesh_assets[0] = MeshAsset::new(
        "p",
        MeshKind::Arch(ArchSpec {
            profile: ArchProfile::Pointed,
            rise: 1536,
            ..ArchSpec::default()
        }),
    );
    assert_eq!(mesh_sockets_for_test(&pointed)[0].y, 238 - 96); // (3072 - 32) gu is 237.5, rounded away from zero
}
