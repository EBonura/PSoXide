//! The player's authored spawn stands on the native floor of every reference
//! level.

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint,
};
use psx_grid::collision::GridTraceProvider;
use psx_grid::write::aligned_copy;
use psx_grid::{CollisionView, LevelView};

use super::tests::{cook, load_reference};

fn leak_bytes(bytes: &[u8]) -> &'static [u8] {
    let words: &'static mut [u32] = aligned_copy(bytes).leak();
    // SAFETY: the words live for the program and cover `bytes.len()` bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

/// The player's authored spawn must stand on native collision: after 120 idle
/// ticks the motor is still within a few units of where it was placed (it
/// settles onto the floor, it does not fall through the world).
#[test]
fn the_player_spawn_stands_on_the_native_floor() {
    for name in [
        "grid-level008",
        "grid-dungeon",
        "grid-cathedral",
        "grid-cave",
        "grid-sewers",
        "grid-west",
    ] {
        let Some(project) = load_reference(name) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
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
        let native = psx_grid::ResidentRooms::from_collisions(level, rooms)
            .expect("a collision world")
            .leak_collision();
        let scene = project.active_scene();
        let spawn = scene
            .nodes()
            .iter()
            .find(|n| {
                n.children.iter().any(|c| {
                    scene.node(*c).is_some_and(|k| {
                        matches!(
                            &k.kind,
                            crate::NodeKind::CharacterController { player: true, .. }
                        )
                    })
                })
            })
            .map(|n| n.transform.translation)
            .expect("a player entity");
        let start = RoomPoint::new(
            (spawn[0] / 16.0).round() as i32,
            (spawn[1] / 16.0).round() as i32,
            (spawn[2] / 16.0).round() as i32,
        );
        let config = CharacterMotorConfig::character_with_body(
            12,
            64,
            3 << 8,
            5 << 8,
            Angle::from_q12(1024),
        );
        let mut motor = CharacterMotorState::new(start, Angle::ZERO);
        let mut last = start;
        for _ in 0..120 {
            let mut p = GridTraceProvider::new(&native);
            if let Ok(frame) = motor.update_vblanks_with_trace_provider(
                &mut p,
                CharacterMotorInput::default(),
                config,
                1,
            ) {
                last = frame.position;
            }
        }
        let located = native.locate(0, [start.x, start.y, start.z]);
        eprintln!("{name}: spawn {start:?} -> {last:?} located {located:?}");
        assert!(
            (last.y - start.y).abs() <= 40,
            "{name}: the spawn {start:?} ends at {last:?}: it fell or floated"
        );
    }
}
