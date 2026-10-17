//! A cooked grid world and the real character motor walking it, for the
//! tests of the level verbs: the same `GridTraceProvider` the guest uses
//! over the same cooked bytes.

use psx_engine::{
    Angle, CharacterMotorConfig, CharacterMotorInput, CharacterMotorState, RoomPoint, Q12,
};
use psx_grid::collision::{GridCollision, GridTraceProvider};
use psx_grid::write::aligned_copy;
use psx_grid::{CollisionView, LevelView};

use super::tests::cook;
use crate::grid::world::GridWorld;

fn leak(bytes: &[u8]) -> &'static [u8] {
    let words: &'static mut [u32] = aligned_copy(bytes).leak();
    // SAFETY: the words live for the program and cover `bytes.len()` bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

/// Cooked native collision of a world.
pub(super) struct Native {
    pub collision: GridCollision<'static>,
}

/// Cook `world` and open its collision.
pub(super) fn native(world: &GridWorld) -> Native {
    let cooked = cook(world).expect("the world cooks");
    let level = LevelView::parse(leak(&cooked.core)).expect("a level");
    let rooms: Vec<Option<CollisionView<'static>>> = cooked
        .chunks
        .iter()
        .map(|c| Some(CollisionView::parse(leak(c)).expect("a room")))
        .collect();
    let rooms: &'static [Option<CollisionView<'static>>] = rooms.leak();
    Native {
        collision: GridCollision::new(level, rooms).expect("a collision world"),
    }
}

/// The motor configuration of the starter's Graybox Reach character.
pub(super) fn config() -> CharacterMotorConfig {
    CharacterMotorConfig::character_with_body(12, 48, 8 << 8, 8 << 8, Angle::from_q12(1024))
}

/// The Graybox Reach player as the project authors it: radius 188 and height
/// 1024 authored units (a sixteenth in runtime units), walking at a few units
/// a tick.
pub(super) fn reach_config() -> CharacterMotorConfig {
    CharacterMotorConfig::character_with_body(12, 64, 3 << 8, 5 << 8, Angle::from_q12(1024))
}

/// Walk `ticks` fixed ticks with the stick held towards `(move_x, move_z)`
/// (Q12), returning the position after every tick.
pub(super) fn walk_path(
    world: &Native,
    from: RoomPoint,
    move_x: i32,
    move_z: i32,
    ticks: u32,
) -> Vec<RoomPoint> {
    walk_path_with(config(), world, from, move_x, move_z, ticks)
}

/// [`walk_path`] with a given motor configuration.
pub(super) fn walk_path_with(
    config: CharacterMotorConfig,
    world: &Native,
    from: RoomPoint,
    move_x: i32,
    move_z: i32,
    ticks: u32,
) -> Vec<RoomPoint> {
    let mut motor = CharacterMotorState::new(from, Angle::ZERO);
    let input = CharacterMotorInput {
        move_x: Q12::from_raw(move_x),
        move_z: Q12::from_raw(move_z),
        ..CharacterMotorInput::default()
    };
    let mut path = Vec::with_capacity(ticks as usize);
    for _ in 0..ticks {
        let mut provider = GridTraceProvider::new(&world.collision);
        motor
            .update_vblanks_with_trace_provider(&mut provider, input, config, 1)
            .expect("in range");
        path.push(motor.position());
    }
    path
}
