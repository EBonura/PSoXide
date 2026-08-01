//! Glue over `psx_game_runtime::character`: re-exports the character/
//! animation vocabulary, threads this example's player-speed scale into
//! the record decode, and keeps the gameplay-side value types
//! (checkpoint, message overlay, evade intent) that phase 3 owns.

use super::*;

pub(super) use psx_game_runtime::character::{player_anim_is_attack, PlayerAnim, PlayerAnimBlend, RuntimeCharacter};

/// Decode a cooked character record with this example's global
/// player-speed scale.
pub(super) fn runtime_character_from_record(c: &LevelCharacterRecord) -> RuntimeCharacter {
    RuntimeCharacter::from_record(c, PLAYER_SPEED_SCALE_NUM, PLAYER_SPEED_SCALE_DEN)
}

/// Scale a speed by this example's global player-speed knob.
pub(super) fn scaled_player_speed(speed: i32) -> i32 {
    psx_game_runtime::character::scaled_player_speed(
        speed,
        PLAYER_SPEED_SCALE_NUM,
        PLAYER_SPEED_SCALE_DEN,
    )
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct RuntimeCheckpoint {
    pub(super) room: RoomIndex,
    pub(super) position: RoomPoint,
    pub(super) yaw: Angle,
    pub(super) checkpoint_id: &'static str,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct RuntimeMessageOverlay {
    pub(super) title: &'static str,
    pub(super) body: &'static str,
}

#[derive(Copy, Clone, Debug, Default)]
pub(super) struct EvadeRunIntent {
    pub(super) sprint: bool,
    pub(super) evade: bool,
}
