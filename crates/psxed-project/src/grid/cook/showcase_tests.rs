//! The showcase level: blockout, budget and the real motor walking it.

use crate::grid::faces::room_face_counts;
use crate::grid::showcase::{build, Mats};
use crate::grid::tree::check;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;
use crate::Scene;

fn made() -> GridWorld {
    let mut w = GridWorld {
        scale: crate::grid::starter::STARTER_SCALE,
        ..GridWorld::default()
    };
    let mut scene = Scene::new("showcase");
    let plan = build(&mut w, &mut scene, &Mats::default()).expect("the showcase builds");
    for n in &plan.notes {
        eprintln!("note: {n}");
    }
    w
}

#[test]
fn print_budget() {
    let w = made();
    let faces = room_face_counts(&w);
    for (i, r) in w.rooms.iter().enumerate() {
        eprintln!(
            "room {i} {:>16}: {:>5} faces {:>4} leaves {:>2} meshes origin {:?} size {}x{}",
            r.name,
            faces[i],
            r.leaf_count(),
            r.meshes.len(),
            r.origin,
            r.width,
            r.depth
        );
        assert!(check(r).is_empty(), "{}", r.name);
    }
    let _ = GridScale::DEFAULT;
    // ASCII plan: one letter per room.
    let (mut x0, mut z0, mut x1, mut z1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for r in &w.rooms {
        for (k, _) in r.leaves() {
            let (lo, hi) = k.sector_range();
            x0 = x0.min(lo[0]);
            z0 = z0.min(lo[1]);
            x1 = x1.max(hi[0]);
            z1 = z1.max(hi[1]);
        }
    }
    for z in z0..=z1 {
        let mut line = format!("{z:>3} ");
        for x in x0..=x1 {
            let mut c = '.';
            for (i, r) in w.rooms.iter().enumerate() {
                if r.leaves().any(|(k, _)| k.touches_sector([x, z])) {
                    c = char::from(b'A' + i as u8);
                }
            }
            line.push(c);
        }
        eprintln!("{line}");
    }
}

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint, Q12,
};
use psx_grid::collision::GridTraceProvider;

use super::motor_harness::{config, native, Native};
use super::RuntimeUnits;

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

/// Walk the motor through `legs` (world sectors `[x, z]` as fractions: grid
/// units / 1024 times 100), stopping within 6 units of each or after `ticks`.
/// Returns every stop's position and the highest and lowest height.
fn walk(
    world: &Native,
    from: RoomPoint,
    legs: &[[i32; 2]],
    ticks: u32,
    zsign: i32,
    units: &RuntimeUnits,
) -> (Vec<RoomPoint>, [i32; 2]) {
    let cfg: CharacterMotorConfig = if std::env::var("SC_REACH").is_ok() {
        super::motor_harness::reach_config()
    } else {
        config()
    };
    let mut motor = CharacterMotorState::new(from, Angle::ZERO);
    let (mut lo, mut hi) = (from.y, from.y);
    let mut stops = Vec::new();
    for leg in legs {
        let goal = [units.rt(leg[0] * 1024 / 100), units.rt(leg[1] * 1024 / 100)];
        for _ in 0..ticks {
            let p = motor.position();
            let (dx, dz) = (goal[0] - p.x, goal[1] - p.z);
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
    (stops, [lo, hi])
}

/// The route, in hundredths of a sector. Each entry is a stop.
const ROUTE: &[(&str, [i32; 2])] = &[
    ("start", [250, 650]),
    ("bridge", [1000, 650]),
    ("lever", [2150, 650]),
    ("gate", [2300, 650]),
    ("gatehouse", [2500, 750]),
    ("nave door", [2750, 780]),
    ("nave", [3200, 800]),
    ("aisle door", [3150, 1050]),
    ("cloister north", [3150, 1300]),
    ("cloister east", [3200, 1300]),
    ("cloister south", [3200, 2300]),
    ("stair top", [2900, 2300]),
    ("stair", [2900, 2450]),
    ("crypt landing", [2900, 2950]),
    ("crypt", [2300, 3000]),
    ("machine", [1500, 3000]),
    ("dome", [1150, 3000]),
    ("tower door", [1250, 2780]),
    ("tower", [1300, 2500]),
    ("tower west", [1150, 2150]),
    ("ring nw", [1230, 2060]),
    ("ring n", [1350, 2060]),
    ("ring ne", [1470, 2060]),
    ("ring e", [1470, 2150]),
    ("ring se", [1470, 2250]),
    ("ring s", [1350, 2240]),
    ("ring sw", [1230, 2240]),
    ("ring w", [1230, 2150]),
    ("ring n 2", [1350, 2060]),
    ("ring ne 2", [1470, 2060]),
    ("ring e 2", [1470, 2150]),
    ("ring se 2", [1470, 2250]),
    ("ring s 2", [1350, 2240]),
    ("ring sw 2", [1230, 2240]),
    ("ring w 2", [1230, 2150]),
    ("top west", [1150, 2150]),
    ("top south", [1150, 2350]),
    ("top east", [1550, 2350]),
    ("top door", [1550, 2060]),
    ("high bridge", [1550, 1500]),
    ("lodge door", [1550, 1120]),
    ("lodge", [1350, 900]),
    ("lodge north", [1350, 760]),
];

#[test]
fn a_body_walks_the_route_to_the_tower() {
    let w = made();
    let units = RuntimeUnits::new(w.scale).unwrap();
    let nat = native(&w);
    let from = std::env::var("SC_FROM")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let start = RoomPoint::new(
        units.rt(ROUTE[from].1[0] * 1024 / 100),
        std::env::var("SC_Y")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        units.rt(ROUTE[from].1[1] * 1024 / 100),
    );
    let sign = z_sign(&nat, start);
    let legs: Vec<[i32; 2]> = ROUTE[from..].iter().map(|(_, p)| *p).collect();
    let (stops, span) = walk(&nat, start, &legs, 1500, sign, &units);
    for ((name, goal), p) in ROUTE[from..].iter().zip(&stops) {
        let g = [
            units.rt(goal[0] * 1024 / 100),
            units.rt(goal[1] * 1024 / 100),
        ];
        eprintln!("{name:>16}: at {:?} goal {g:?} y {}", [p.x, p.z], p.y);
    }
    eprintln!("height span {span:?}");
    for ((name, goal), p) in ROUTE[from..].iter().zip(&stops) {
        let g = [
            units.rt(goal[0] * 1024 / 100),
            units.rt(goal[1] * 1024 / 100),
        ];
        assert!(
            (p.x - g[0]).abs() <= 8 && (p.z - g[1]).abs() <= 8,
            "stuck before {name}: at {:?}, goal {g:?}",
            [p.x, p.z]
        );
    }
    // Down into the crypt, up the tower, and home on the lodge floor.
    assert!(span[0] <= units.rt(-1024) + 3, "the crypt depth: {span:?}");
    assert!(span[1] >= units.rt(2048) - 3, "the tower top: {span:?}");
    let end = stops.last().unwrap();
    assert!(end.y.abs() <= 3, "ends on the lodge floor: {end:?}");
}

#[test]
fn print_portals() {
    let w = made();
    let units = RuntimeUnits::new(w.scale).unwrap();
    let sw = crate::grid::sector_view::sector_world(&w).map(|(x, _)| x);
    let pw = sw.as_ref().unwrap_or(&w);
    let portals = super::portals::derive(pw, &units).unwrap();
    for (i, ps) in portals.iter().enumerate() {
        let dests: Vec<u16> = ps.iter().map(|p| p.dest).collect();
        eprintln!("room {i} {:>22} -> {dests:?}", w.rooms[i].name);
    }
}

#[test]
fn print_wanted_rooms() {
    use psx_grid::stream::StreamWindow;
    let mut w = made();
    w.stream = crate::grid::world::StreamRadii {
        draw_far: 3,
        active_radius: 3,
        lead_radius: 1,
        ..crate::grid::world::StreamRadii::DEFAULT
    };
    let cooked = super::tests::cook(&w).expect("cooks");
    let words: &'static mut [u32] = psx_grid::write::aligned_copy(&cooked.core).leak();
    // SAFETY: the words live for the program and cover the core's bytes.
    let core: &'static [u8] =
        unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), cooked.core.len()) };
    let level = &psx_grid::LevelView::parse(core).expect("a level");
    let units = RuntimeUnits::new(w.scale).unwrap();
    let mut window = StreamWindow::new(level);
    let mut wanted = Vec::new();
    let s = units.rt(1024);
    for (label, room, x, z) in [
        ("landing centre", 0usize, 2 * s + s / 2, 6 * s + s / 2),
        ("landing east edge", 0, 6 * s - 8, 6 * s + s / 2),
        ("deck west", 1, 8 * s, 6 * s + s / 2),
        ("deck middle", 2, 13 * s, 6 * s + s / 2),
    ] {
        window.invalidate();
        window.wanted(level, Some((room, [x, 0, z])), Some((room, [x, 0, z])), &mut wanted);
        let names: Vec<String> = wanted
            .iter()
            .map(|w| format!("{}{}", w.room, if matches!(w.want, psx_grid::stream::Want::Need) { "" } else { "(lead)" }))
            .collect();
        eprintln!("{label}: {names:?}");
    }
}

#[test]
fn print_pool_by_radius() {
    let w0 = made();
    for far in [3u8, 4, 5, 6, 8] {
        let mut w = w0.clone();
        w.stream = crate::grid::world::StreamRadii {
            draw_far: far,
            active_radius: far,
            lead_radius: 1,
            ..crate::grid::world::StreamRadii::DEFAULT
        };
        let cooked = super::tests::cook(&w).expect("cooks");
        eprintln!("draw_far {far}: pool {} pages", cooked.report.pool_pages);
    }
}

/// Walk the real cooked package of a written project (`SC_PROJECT=<dir>`):
/// the bytes the guest gets, not the test cook's.
#[test]
fn walk_the_written_project() {
    let Ok(dir) = std::env::var("SC_PROJECT") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let project = crate::ProjectDocument::load_from_path(dir.join("project.ron")).unwrap();
    let (package, report) = crate::playtest::build_package(&project, &dir);
    assert!(report.is_ok(), "{:?}", report.error_messages());
    let g = package.unwrap().world_geometry;
    let keep: Option<Vec<usize>> = std::env::var("SC_KEEP")
        .ok()
        .map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect());
    let nat = super::motor_harness::native_subset(&g.core, &g.room_chunks, keep.as_deref());
    let w = made();
    let units = RuntimeUnits::new(w.scale).unwrap();
    let start = RoomPoint::new(units.rt(250 * 1024 / 100), 0, units.rt(650 * 1024 / 100));
    let sign = z_sign(&nat, start);
    let legs: Vec<[i32; 2]> = ROUTE.iter().map(|(_, p)| *p).collect();
    let (stops, span) = walk(&nat, start, &legs, 1500, sign, &units);
    for ((name, _), p) in ROUTE.iter().zip(&stops) {
        eprintln!("{name:>16}: at {:?} y {}", [p.x, p.z], p.y);
    }
    eprintln!("span {span:?}");
}
