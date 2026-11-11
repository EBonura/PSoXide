//! One small level built only from the level verbs (Room, Corridor, Opening,
//! the doorway helper, Arcade, Terrain, Bridge, Building) and checked three
//! ways: it normalises, it cooks, and a body walks it end to end with the
//! real character motor over the cooked collision.
//!
//! Plan (world sectors, one lattice; rooms are separate where a wall is
//! shared, so the door is cut on both sides):
//!
//! ```text
//!   tower A (octagon, room 0)  door  corridor + ramp  hall (+ arcade, window)
//!       x 0..7                x 8     x 8..19            x 20..31   (room 1)
//!   courtyard (terrain, room 1) x 32..39   bridge x 40..47   tower B (room 2)
//! ```

use std::collections::BTreeSet;

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint, Q12,
};
use psx_grid::collision::GridTraceProvider;

use super::motor_harness::{config, native, Native};
use super::tests::cook;
use super::RuntimeUnits;
use crate::grid::edit::verbs::arcade::{build_arcade, ArcadeParams};
use crate::grid::edit::verbs::bridge::{build_bridge, BridgeParams};
use crate::grid::edit::verbs::building::{build_building, BuildingParams};
use crate::grid::edit::verbs::corridor::{lay_corridor, CorridorParams, Waypoint};
use crate::grid::edit::verbs::opening::{cut_opening, OpeningKind, OpeningParams, OpeningProfile};
use crate::grid::edit::verbs::room::{lay_room, Footprint, RoomParams, RoomTarget};
use crate::grid::edit::verbs::terrain::{lay_terrain, Edge, TerrainParams};
use crate::grid::edit::CellRect;
use crate::grid::sector::WallEdge;
use crate::grid::tree::check;
use crate::grid::units::GridScale;
use crate::grid::walk::{analyse, Spawn};
use crate::grid::world::GridWorld;

fn cells(a: [i32; 2], b: [i32; 2]) -> BTreeSet<[i32; 2]> {
    CellRect::from_corners(a, b).cells().collect()
}

struct Level {
    world: GridWorld,
    /// The tower A door, before it is cut.
    door: OpeningParams,
    /// Where the bridge meets tower B's door, grid units `[x, y, z]`.
    far_door: [i32; 3],
    courtyard_floor: i32,
}

/// Everything but the tower A door.
fn build() -> Level {
    let mut w = GridWorld {
        scale: GridScale::DEFAULT,
        ..GridWorld::default()
    };
    // Tower A: an octagon, maximal leaves, room 0.
    let tower = RoomParams::new(
        Footprint::Octagon {
            rect: CellRect::from_corners([0, 0], [7, 7]),
            cut: 2,
        },
        "tower a",
        4096,
    );
    lay_room(&mut w, &tower).unwrap();
    // The hall, room 1, a click and more above the tower floor.
    let mut hall = RoomParams::new(
        Footprint::Rect(CellRect::from_corners([20, 0], [31, 8])),
        "hall",
        4096,
    );
    hall.base = 1024;
    let made = lay_room(&mut w, &hall).unwrap();
    assert_eq!(made.room, 1);
    // The corridor from tower A to the hall: flat, then a ramp up.
    let mut corridor = CorridorParams::new(
        RoomTarget::Existing(1),
        vec![
            Waypoint::at_height([8, 4], 0),
            Waypoint::at([14, 4]),
            Waypoint::at_height([20, 4], 1024),
        ],
        3072,
    );
    corridor.base = 0;
    let out = lay_corridor(&mut w, &corridor).unwrap();
    assert_eq!(out.legs[1].rise, 1024);
    // A window in the hall's north wall.
    cut_opening(
        &mut w,
        &OpeningParams {
            profile: OpeningProfile::Rectangular,
            room: 1,
            cell: [25, 0],
            side: WallEdge::North,
            from: 25 * 4 + 1,
            to: 25 * 4 + 3,
            kind: OpeningKind::Window { sill: 768 },
            top: 2560,
            material: None,
        },
    )
    .unwrap();
    // An arcade along the hall, off the walking row.
    let mut arcade = ArcadeParams::line(1, [22 * 1024, 3 * 1024], [30 * 1024, 3 * 1024], 1024, 4);
    arcade.pier = 256;
    arcade.spring = 1536;
    build_arcade(&mut w, &arcade).unwrap();
    // The courtyard: a flat strip at the east edge, then seeded ground.
    let mut strip = TerrainParams::new(1, cells([39, 1], [39, 7]), 1);
    strip.base = Some(1024);
    strip.create = true;
    strip.amplitude_clicks = 0;
    strip.follow_ceiling = false;
    lay_terrain(&mut w, &strip).unwrap();
    let mut yard = TerrainParams::new(1, cells([32, 1], [38, 7]), 42);
    yard.base = Some(1024);
    yard.create = true;
    yard.amplitude_clicks = 2;
    yard.edge = Edge::Blend(2);
    yard.follow_ceiling = false;
    let r = lay_terrain(&mut w, &yard).unwrap();
    assert!(r.steepest <= 576);
    // Tower B: a Building in room 2 with a door on its west side.
    let mut b = BuildingParams::new(
        RoomTarget::New("tower b".into()),
        Footprint::Octagon {
            rect: CellRect::from_corners([48, 1], [55, 8]),
            cut: 2,
        },
    );
    b.base = 1024;
    b.storeys = 1;
    b.stair = false;
    b.roof_parapet = None;
    b.window_every = 0;
    b.door_sides = vec![WallEdge::West];
    let made = build_building(&mut w, &b).unwrap();
    assert_eq!(made.doors.len(), 1, "{:?}", made.notes);
    let door = made.doors[0];
    // The bridge: from the courtyard's east edge to the door plane.
    let bridge = BridgeParams::deck(
        RoomTarget::New("bridge".into()),
        [40 * 1024, 1024, door.center[2]],
        [door.center[0], 1024, door.center[2]],
        1024,
        512,
    );
    build_bridge(&mut w, &bridge).unwrap();
    let door_a = OpeningParams {
        profile: OpeningProfile::Rectangular,
        room: 0,
        cell: [7, 4],
        side: WallEdge::East,
        from: 16,
        to: 20,
        kind: OpeningKind::Door,
        top: 2048,
        material: None,
    };
    Level {
        world: w,
        door: door_a,
        far_door: door.center,
        courtyard_floor: 1024,
    }
}

/// Walk the motor towards each waypoint (runtime units `[x, z]`), up to
/// `ticks` ticks a leg, stopping within 6 units. Returns the final position
/// and the lowest and highest height reached.
fn walk(
    world: &Native,
    from: RoomPoint,
    legs: &[[i32; 2]],
    ticks: u32,
    zsign: i32,
) -> (RoomPoint, [i32; 2], Vec<RoomPoint>) {
    let cfg: CharacterMotorConfig = config();
    let mut motor = CharacterMotorState::new(from, Angle::ZERO);
    let (mut lo, mut hi) = (from.y, from.y);
    let mut stops = Vec::new();
    for leg in legs {
        for _ in 0..ticks {
            let p = motor.position();
            let (dx, dz) = (leg[0] - p.x, leg[1] - p.z);
            if dx.abs() <= 6 && dz.abs() <= 6 {
                break;
            }
            let len = f64::from(dx).hypot(f64::from(dz));
            let input = CharacterMotorInput {
                move_x: Q12::from_raw((f64::from(dx) / len * 4096.0) as i32),
                move_z: Q12::from_raw((f64::from(dz) / len * 4096.0 * f64::from(zsign)) as i32),
                ..CharacterMotorInput::default()
            };
            let mut provider = GridTraceProvider::new(&world.collision);
            motor
                .update_vblanks_with_trace_provider(&mut provider, input, cfg, 1)
                .expect("in range");
            lo = lo.min(motor.position().y);
            hi = hi.max(motor.position().y);
        }
        stops.push(motor.position());
    }
    (motor.position(), [lo, hi], stops)
}

/// Which way the stick's z axis moves the body: +1 or -1.
fn z_sign(world: &Native, from: RoomPoint) -> i32 {
    let mut motor = CharacterMotorState::new(from, Angle::ZERO);
    let input = CharacterMotorInput {
        move_z: Q12::from_raw(4096),
        ..CharacterMotorInput::default()
    };
    for _ in 0..12 {
        let mut provider = GridTraceProvider::new(&world.collision);
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, config(), 1)
            .expect("in range");
    }
    if motor.position().z >= from.z {
        1
    } else {
        -1
    }
}

/// Walk the cooked level with the real motor: tower A, door, corridor, ramp,
/// hall, courtyard, bridge, tower B. Returns the end point and every stop.
fn walk_level(world: &GridWorld, level: &Level) -> (RoomPoint, Vec<RoomPoint>) {
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let rt = |gu: i32| units.rt(gu);
    let nat = native(world);
    let start = RoomPoint::new(rt(3 * 1024 + 512), 0, rt(4 * 1024 + 512));
    let sign = z_sign(&nat, start);
    let row = rt(4 * 1024 + 512);
    let legs = [
        [rt(10 * 1024), row],
        [rt(18 * 1024), row],
        [rt(22 * 1024), row],
        [rt(36 * 1024), row],
        [rt(39 * 1024 + 512), rt(level.far_door[2])],
        [rt(level.far_door[0] + 2 * 1024), rt(level.far_door[2])],
    ];
    let (end, _span, stops) = walk(&nat, start, &legs, 900, sign);
    (end, stops)
}

#[test]
fn the_level_normalises_and_cooks() {
    let level = build();
    for room in &level.world.rooms {
        assert!(check(room).is_empty(), "{}", room.name);
    }
    assert_eq!(level.world.rooms.len(), 4, "tower A, hall, tower B, bridge");
    cook(&level.world).expect("the level cooks");
}

#[test]
fn a_closed_door_blocks_and_the_cut_door_opens_the_tower() {
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let level = build();
    let rt = |gu: i32| units.rt(gu);
    // Start inside tower A on the walking row, aim at the corridor.
    let start = RoomPoint::new(rt(3 * 1024 + 512), 0, rt(4 * 1024 + 512));
    let goal = [[rt(10 * 1024), rt(4 * 1024 + 512)]];
    let closed = native(&level.world);
    let (end, _, _) = walk(&closed, start, &goal, 400, 1);
    assert!(end.x < rt(8 * 1024), "the wall stops the body: {end:?}");
    let mut open = level.world.clone();
    let out = cut_opening(&mut open, &level.door).unwrap();
    assert!(out.mirrored, "the corridor side is cut too");
    let opened = native(&open);
    let sign = z_sign(&opened, start);
    let (end, _, _) = walk(&opened, start, &goal, 400, sign);
    assert!(end.x >= rt(10 * 1024) - 6, "through the door: {end:?}");
}

#[test]
fn the_level_is_walkable_end_to_end() {
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let rt = |gu: i32| units.rt(gu);
    let level = build();
    let mut world = level.world.clone();
    cut_opening(&mut world, &level.door).unwrap();
    for room in &world.rooms {
        assert!(check(room).is_empty());
    }
    // The walkability query: from a spawn in tower A, every standable sector
    // of tower A, the corridor, the hall and the courtyard is reachable and
    // nothing is steep or too tall to climb.
    let spawn = Spawn {
        room: 0,
        cell: [3, 4],
    };
    let a = analyse(&world, &[spawn], &psx_engine::WalkLimits::DEFAULT).unwrap();
    assert!(a.reach_ran);
    let counts = a.counts();
    assert_eq!(counts[0], 0, "steep sectors");
    assert_eq!(counts[2], 0, "steps too tall");
    // Unreachable standable sectors: tower B is only reached over the bridge
    // (a mesh), which the sector flood does not see, so allow exactly its
    // sectors and nothing else.
    let tower_b = a.rooms[2].standable_count();
    assert!(
        counts[4] <= tower_b,
        "unreachable {} against tower B's {}",
        counts[4],
        tower_b
    );
    let (end, stops) = walk_level(&world, &level);
    let top = rt(level.courtyard_floor);
    assert!(
        end.x >= rt(level.far_door[0] + 1024) - 6,
        "walked into tower B: {end:?}"
    );
    assert!(
        (end.y - top).abs() <= 3,
        "stands on tower B's floor: {end:?}"
    );
    assert!(
        stops[2].y >= top - 3,
        "the ramp was climbed: {:?}",
        stops[2]
    );
}

#[test]
fn a_ruined_hall_keeps_the_critical_path() {
    use crate::grid::edit::verbs::ruin::{ruin_region, RuinParams};
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let level = build();
    let mut base = level.world.clone();
    cut_opening(&mut base, &level.door).unwrap();
    let spawn = Spawn {
        room: 0,
        cell: [3, 4],
    };
    for seed in 0..4 {
        let mut world = base.clone();
        let mut p = RuinParams::new(1, cells([20, 0], [31, 8]), seed, 70);
        p.keep = cells([20, 4], [31, 4]);
        p.spawns = vec![spawn];
        let report = ruin_region(&mut world, &p).unwrap();
        for room in &world.rooms {
            assert!(check(room).is_empty(), "seed {seed}: {}", room.name);
        }
        cook(&world).expect("a ruin cooks");
        let (end, _) = walk_level(&world, &level);
        assert!(
            end.x >= units.rt(level.far_door[0] + 1024) - 6,
            "seed {seed}: the path survives {report:?}: {end:?}"
        );
    }
}

#[test]
fn a_body_walks_every_styled_corridor_end_to_end() {
    use crate::grid::edit::verbs::corridor::LegShape;
    use crate::grid::edit::verbs::style::{self, Roles};
    use crate::grid::edit::verbs::style_fixtures as fixtures;
    use crate::grid::edit::verbs::styled::{lay_styled_corridor, StyledCorridor};
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let rt = |gu: i32| units.rt(gu);
    for (style, width) in [
        (style::plain(), 1),
        (fixtures::ribbed_walk(), 2),
        (fixtures::pointed_barrel(), 2),
        (fixtures::groin_walk(), 2),
        (fixtures::piered_barrel(), 2),
    ] {
        let name = style.name.clone();
        let mut w = GridWorld {
            scale: GridScale::DEFAULT,
            ..GridWorld::default()
        };
        // A straight run with a ramp up in the middle and a landing at each
        // end, so a styled leg climbs and a plain landing follows.
        let sc = StyledCorridor {
            target: RoomTarget::New("run".into()),
            waypoints: vec![
                Waypoint::at_height([0, 0], 0),
                Waypoint::at([6, 0]),
                Waypoint::at_height([14, 0], 512),
            ],
            width,
            base: 0,
            leg: LegShape::Ramp,
            style,
            roles: Roles::default(),
        };
        lay_styled_corridor(&mut w, &sc).unwrap_or_else(|e| panic!("{name}: {}", e.describe()));
        cook(&w).unwrap_or_else(|e| panic!("{name} cooks: {e}"));
        let nat = native(&w);
        // Down the middle of the corridor.
        let z = rt(width * 512);
        let start = RoomPoint::new(rt(512), 0, z);
        let (end, [_, hi], _) = walk(&nat, start, &[[rt(14 * 1024 + 512), z]], 1500, 1);
        assert!(
            end.x >= rt(14 * 1024) - 6,
            "{name}: walked the corridor end to end: {end:?}"
        );
        assert!(hi >= rt(512) - 4, "{name}: climbed to the landing: {hi}");
    }
}

#[test]
fn a_body_walks_the_rib_corridor_through_every_rib() {
    use crate::grid::edit::verbs::bay_corridor::{lay_bay_corridor, rib_corridor, BayCorridor};
    use crate::grid::edit::verbs::style::Roles;
    let units = RuntimeUnits::new(GridScale::DEFAULT).unwrap();
    let rt = |gu: i32| units.rt(gu);
    let mut w = GridWorld {
        scale: GridScale::DEFAULT,
        ..GridWorld::default()
    };
    // Twelve sectors along +X: four bays, a rib in each.
    let run = BayCorridor {
        target: RoomTarget::New("rib".into()),
        waypoints: vec![Waypoint::at([0, 2]), Waypoint::at([11, 2])],
        width: 6,
        base: 0,
        bay: rib_corridor(),
        roles: Roles::default(),
    };
    let out = lay_bay_corridor(&mut w, &run).unwrap_or_else(|e| panic!("{}", e.describe()));
    assert_eq!(out.bays, 4);
    cook(&w).expect("cooks");
    let nat = native(&w);
    // Down the middle of the six sector width.
    let z = rt(3 * 1024);
    let start = RoomPoint::new(rt(512), 0, z);
    let (end, [lo, hi], _) = walk(&nat, start, &[[rt(11 * 1024 + 512), z]], 2500, 1);
    assert!(
        end.x >= rt(11 * 1024) - 6,
        "walked through the ribs: {end:?}"
    );
    assert!(
        hi - lo <= rt(128),
        "the floor is level in the middle: {lo} {hi}"
    );
}
