//! Free planes through the cook: render runs, collision records and the
//! character walking the cooked result.

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, CollisionTrace,
    CollisionTraceProvider, CollisionTraceQuery, RoomPoint, Q12,
};
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::write::aligned_copy;
use psx_grid::{
    plane_kind, CollisionView, LevelView, RoomView, CHUNK_HAS_PLANES, LEVEL_HAS_PLANES,
    QUAD_DOUBLE, QUAD_PLANE,
};

use super::tests::cook;
use crate::grid::plane::{GridPlane, PlaneCollision, PlaneUvMode};
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
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

/// Two floors separated by a three sector ditch, one wide room. Sector
/// edge is 80 runtime units at the default scale.
fn ditch_world() -> GridWorld {
    let mut room = GridRoom::new("hall", 9, 3);
    for z in 0..3 {
        for x in 0..9 {
            let height = if (3..6).contains(&x) { -2560 } else { 0 };
            room.sector_or_insert(x, z).unwrap().floor =
                Some(GridSurface::flat(height, Some(rid(7))));
        }
    }
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn bridge() -> GridPlane {
    // Sectors 3 to 6 along x, centred on the middle row, a sector wide.
    let mut plane = GridPlane::new([4608, 0, 1536], [4096, 1024]);
    plane.rotation = [0, 90, 0];
    plane.collision = PlaneCollision::Walkable;
    plane.material = Some(rid(8));
    plane
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

fn walk(world: &Native, from: RoomPoint, move_x: i32, move_z: i32, ticks: u32) -> RoomPoint {
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
    motor.position()
}

#[test]
fn a_world_with_planes_cooks_and_validates_end_to_end() {
    let mut world = ditch_world();
    world.rooms[0].planes.push(bridge());
    // A solid slanted wall and a double-sided banner.
    let mut fence = GridPlane::new([1536, 512, 1536], [2048, 1024]);
    fence.rotation = [35, 0, 0];
    fence.collision = PlaneCollision::Solid;
    world.rooms[0].planes.push(fence);
    let mut banner = GridPlane::new([7168, 1024, 512], [1024, 2048]);
    banner.double_sided = true;
    banner.uv_mode = PlaneUvMode::Stretch;
    banner.fullbright = true;
    world.rooms[0].planes.push(banner);
    let cooked = cook(&world).unwrap();
    let level = LevelView::parse(leak(&cooked.core)).unwrap();
    assert_ne!(level.header.flags & LEVEL_HAS_PLANES, 0);
    let room = RoomView::parse(leak(&cooked.chunks[0])).unwrap();
    room.validate_full(level.materials.len()).unwrap();
    assert_ne!(room.col.header.flags & CHUNK_HAS_PLANES, 0);
    let refs = room.render.plane_refs;
    assert_eq!(refs.len(), 3);
    for reference in refs {
        let run = &room.render.quads[usize::from(reference.quad)
            ..usize::from(reference.quad) + usize::from(reference.count)];
        assert!(!run.is_empty());
        assert!(run.iter().all(|q| q.flags & QUAD_PLANE != 0));
        assert!(reference.x0 <= reference.x1 && reference.z0 <= reference.z1);
    }
    // The bridge is four sectors long, one wide: four tiles. The fence is
    // two by one. The stretched banner is a single double-sided quad.
    assert_eq!(
        refs.iter().map(|r| r.count).collect::<Vec<_>>(),
        vec![4, 2, 1]
    );
    let banner_quad = &room.render.quads[usize::from(refs[2].quad)];
    assert_ne!(banner_quad.flags & QUAD_DOUBLE, 0);
    // Fullbright writes the tint: neutral material, neutral colour.
    assert!(banner_quad.color.iter().all(|c| *c == 0x0080_8080));
    // Collision: a platform and a wall, no record for the banner.
    let kinds: Vec<u8> = {
        let mut kinds: Vec<u8> = room.col.col_planes.iter().map(|p| p.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    };
    assert_eq!(kinds, vec![plane_kind::PLATFORM, plane_kind::WALL]);
    assert!(room.col.col_planes.iter().all(|p| p.id == 0 || p.id == 1));
    // Each plane has exactly one primary copy, the one point rays test.
    for id in [0u8, 1] {
        let copies: Vec<_> = room.col.col_planes.iter().filter(|p| p.id == id).collect();
        assert!(copies.len() > 1, "plane {id} spans several sectors");
        assert_eq!(
            copies
                .iter()
                .filter(|p| p.flags & psx_grid::COLPLANE_PRIMARY != 0)
                .count(),
            1
        );
    }
    // The bytes are a pure function of the document.
    let again = cook(&world).unwrap();
    assert_eq!(again.chunks, cooked.chunks);
    assert_eq!(again.core, cooked.core);
}

#[test]
fn the_cooked_bridge_carries_the_character_over_the_ditch() {
    let mut world = ditch_world();
    world.rooms[0].planes.push(bridge());
    let (native, cooked) = native(&world);
    assert!(
        cooked.report.warnings.is_empty(),
        "{:?}",
        cooked.report.warnings
    );
    // Row 1 is z = 80..160; walk +x from the left floor at height 0.
    let across = walk(&native, RoomPoint::new(100, 0, 120), 4096, 0, 260);
    assert!(across.x > 6 * 80 + 20, "crossed the ditch: {across:?}");
    assert_eq!(across.y, 0, "{across:?}");
    // Without the bridge the same walk drops into the ditch.
    let (bare, _) = self::native(&ditch_world());
    let fell = walk(&bare, RoomPoint::new(100, 0, 120), 4096, 0, 260);
    assert!(fell.y < -100, "{fell:?}");
}

#[test]
fn a_cooked_solid_plane_blocks_walking_and_sight() {
    let mut world = ditch_world();
    let mut fence = GridPlane::new([1536, 512, 1536], [3072, 1024]);
    fence.rotation = [90, 0, 0];
    fence.collision = PlaneCollision::Solid;
    world.rooms[0].planes.push(fence);
    // The fence stands across the room at x = 1536 grid = 120 runtime,
    // from z = 0 to 240 (three sectors of 80), 80 units tall.
    let (native, _) = native(&world);
    let end = walk(&native, RoomPoint::new(30, 0, 120), 4096, 0, 200);
    assert!(end.x < 120 - 6, "{end:?}");
    assert!(end.x > 120 - 6 - 14, "{end:?}");
    assert!(!native
        .collision
        .line_of_sight([30, 40, 120], [200, 40, 120]));
    assert!(native
        .collision
        .line_of_sight([30, 120, 120], [200, 120, 120]));
    let mut provider = GridTraceProvider::new(&native.collision);
    let mut out = CollisionTrace::default();
    assert!(provider.trace_into(
        CollisionTraceQuery::point(RoomPoint::new(30, 40, 120), RoomPoint::new(200, 40, 120)),
        &mut out
    ));
    assert!(out.fraction_q12 < 4096 && out.normal_q12[0] < 0, "{out:?}");
}

#[test]
fn planes_outside_the_room_or_steep_walkables_are_reported() {
    use crate::grid::limits::{validate, GridIssueKind, Severity};
    let mut world = ditch_world();
    let mut far = GridPlane::new([90_000, 0, 1536], [1024, 1024]);
    far.rotation = [0, 90, 0];
    world.rooms[0].planes.push(far);
    let issues = validate(&world);
    assert!(issues.iter().any(
        |i| matches!(i.kind, GridIssueKind::PlaneOutsideRoom { index: 0 })
            && i.severity() == Severity::Error
    ));
    assert!(cook(&world).is_err());
    world.rooms[0].planes.clear();
    let mut steep = GridPlane::new([4608, 0, 1536], [1024, 1024]);
    steep.rotation = [0, 30, 0];
    steep.collision = PlaneCollision::Walkable;
    world.rooms[0].planes.push(steep);
    let issues = validate(&world);
    assert!(issues
        .iter()
        .any(|i| matches!(i.kind, GridIssueKind::PlaneTooSteep { .. })
            && i.severity() == Severity::Warning));
    assert!(cook(&world).is_ok());
}

/// A binder for which material 9 is see-through (a banner or a grate).
struct SeeThrough;

impl super::MaterialBinder for SeeThrough {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<super::BoundMaterial, String> {
        super::tests::TestBinder.bind(material)
    }

    fn see_through(&self, material: Option<ResourceId>) -> bool {
        material == Some(rid(9))
    }
}

#[test]
fn the_camera_passes_through_see_through_planes_by_default_and_the_flag_overrides() {
    let flagged = |plane: GridPlane| -> bool {
        let mut world = ditch_world();
        world.rooms[0].planes.push(plane);
        let cooked = super::cook_level(
            super::CookInput {
                world: &world,
                binder: &mut SeeThrough,
                lights: &[],
                first_pack_sector: 100,
                lod_far_cells: 0,
                texture_repeat: true,
            },
            None,
        )
        .unwrap();
        let record = cooked.tables[0].col_planes[0];
        record.flags & psx_grid::COLPLANE_CAMERA_PASS != 0
    };
    let wall = |material, pass| {
        let mut plane = GridPlane::new([2048, 256, 1536], [2048, 512]);
        plane.collision = PlaneCollision::Solid;
        plane.material = material;
        plane.camera_pass = pass;
        plane
    };
    // Automatic: a see-through material passes, an opaque one does not.
    assert!(flagged(wall(Some(rid(9)), None)));
    assert!(!flagged(wall(Some(rid(8)), None)));
    // The flag wins either way.
    assert!(!flagged(wall(Some(rid(9)), Some(false))));
    assert!(flagged(wall(Some(rid(8)), Some(true))));
}
