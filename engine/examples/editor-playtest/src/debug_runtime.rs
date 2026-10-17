//! Emulator-only diagnostic lines for the deterministic replay studies. Each
//! writer is compiled out of every build without `emulator-telemetry`; the
//! camera pose line also builds under the lighter `camera-pose-log`.

#[cfg(any(feature = "emulator-telemetry", feature = "camera-pose-log"))]
#[cfg_attr(not(feature = "emulator-telemetry"), allow(dead_code, unused_imports))]
use super::*;

#[cfg(any(feature = "emulator-telemetry", feature = "camera-pose-log"))]
const DEBUG_LOG_LINE_CAP: usize = 256;

#[cfg(any(feature = "emulator-telemetry", feature = "camera-pose-log"))]
#[cfg_attr(not(feature = "emulator-telemetry"), allow(dead_code, unused_imports))]
struct DebugLogLine {
    bytes: [u8; DEBUG_LOG_LINE_CAP],
    len: usize,
}

#[cfg(any(feature = "emulator-telemetry", feature = "camera-pose-log"))]
#[cfg_attr(not(feature = "emulator-telemetry"), allow(dead_code, unused_imports))]
impl DebugLogLine {
    fn new(prefix: &str) -> Self {
        let mut line = Self {
            bytes: [0; DEBUG_LOG_LINE_CAP],
            len: 0,
        };
        line.push_str(prefix);
        line
    }

    fn push_str(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.bytes.len() {
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_u32(&mut self, value: u32) {
        let mut scratch = [0u8; 10];
        let mut remaining = value;
        let mut len = 0usize;
        loop {
            scratch[len] = b'0' + (remaining % 10) as u8;
            len += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        while len > 0 {
            len -= 1;
            self.push_byte(scratch[len]);
        }
    }

    fn push_i32(&mut self, value: i32) {
        if value < 0 {
            self.push_byte(b'-');
            self.push_u32(value.wrapping_neg() as u32);
        } else {
            self.push_u32(value as u32);
        }
    }

    fn emit(&self) {
        telemetry::debug_line(&self.bytes[..self.len]);
    }
}

#[cfg(feature = "emulator-telemetry")]
fn encode_debug_map_position(value: i32) -> u32 {
    let encoded = value.saturating_add(DEBUG_MAP_POSITION_BIAS);
    if encoded < 0 {
        0
    } else {
        encoded as u32
    }
}

/// Sparse diagnostic evidence for lens/profile and target-anchor transitions.
#[cfg(feature = "emulator-telemetry")]
pub(super) fn debug_log_camera_profile(
    tick: u32,
    focal: i32,
    distance: i32,
    locked: bool,
    focus_y: i32,
    anchor_y: Option<i32>,
) {
    let mut line = DebugLogLine::new("camera-profile tick=");
    line.push_u32(tick);
    line.push_str(" focal=");
    line.push_u32(focal.max(0) as u32);
    line.push_str(" distance=");
    line.push_u32(distance.max(0) as u32);
    line.push_str(" locked=");
    line.push_u32(u32::from(locked));
    line.push_str(" focus_y_biased=");
    line.push_u32(encode_debug_map_position(focus_y));
    line.push_str(" anchor_y_biased=");
    line.push_u32(anchor_y.map_or(0, encode_debug_map_position));
    line.emit();
}

/// Sample the opt-in encounter with a shared simulation clock. Diagnostic builds
/// only; normal disc builds compile the call and formatting out entirely.
#[cfg(feature = "emulator-telemetry")]
pub(super) fn debug_log_enemy_tactics(
    tick: u32,
    player: [i32; 3],
    position: [i32; 3],
    yaw: i16,
    state: u8,
    clip: u16,
    t: psx_game_runtime::entities::EnemyTacticalSnapshot,
) {
    let mut line = DebugLogLine::new("enemy-study,");
    for value in [
        tick as i32,
        player[0],
        player[1],
        player[2],
        position[0],
        position[1],
        position[2],
        i32::from(yaw),
        i32::from(state),
        i32::from(clip),
        t.goal as i32,
        t.result as i32,
        i32::from(t.generation),
        i32::from(t.remaining),
        i32::from(t.running),
        i32::from(t.retries),
    ] {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

/// Input and stance evidence for the ranged weapon acceptance tape.
#[cfg(feature = "emulator-telemetry")]
pub(super) fn debug_log_player_weapon(values: [i32; 13]) {
    let mut line = DebugLogLine::new("player-weapon,");
    for value in values {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

/// Mode, tether and composition evidence for the deterministic camera tape.
#[cfg(feature = "emulator-telemetry")]
pub(super) fn debug_log_aim_camera(values: [i32; 14]) {
    let mut line = DebugLogLine::new("aim-camera,");
    for value in values {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

/// Per-update follow-camera pose: the exact eye, focus, orbit, boom and lift
/// values that explain what the camera did on a tape tick.
#[cfg(feature = "camera-pose-log")]
pub(super) fn debug_log_camera_pose(values: [i32; 21]) {
    let mut line = DebugLogLine::new("camera-pose,");
    for value in values {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

/// Muzzle, velocity, body target and retained player hurtbox for replay calibration.
#[cfg(feature = "emulator-telemetry")]
pub(super) fn debug_log_enemy_shot(values: [i32; 17]) {
    let mut line = DebugLogLine::new("enemy-shot,");
    for value in values {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

/// Per-tick simulation state for the sim-state gate: values that depend only on
/// the input tape and the simulation, never on frame pacing or cycle counts.
/// The entity fields fold every cooked entity slot into one FNV-1a hash plus
/// the first three raw positions for human diffing.
#[cfg(feature = "sim-trace")]
pub(super) fn debug_log_sim_state(values: &[i32]) {
    let mut line = DebugLogLine::new("sim-state,");
    for &value in values {
        line.push_i32(value);
        line.push_byte(b',');
    }
    line.emit();
}

#[cfg(feature = "sim-trace")]
impl Playtest {
    /// Emit this tick's `sim-state` line (see [`debug_log_sim_state`]).
    pub(super) fn trace_sim_state(&self, ctx: &Ctx) {
        let player = self.motor.position();
        let eye = self.render_camera.position;
        let count = self.game_entities.count();
        let mut hash: u32 = 0x811c_9dc5;
        let mut fold = |value: i32| {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
            }
        };
        let mut first = [0i32; 9];
        for index in 0..count {
            let position = self.game_entities.position(index);
            let state = self.game_entities.state(index) as i32;
            fold(position[0]);
            fold(position[1]);
            fold(position[2]);
            fold(state);
            if index < 3 {
                first[index * 3] = position[0];
                first[index * 3 + 1] = position[1];
                first[index * 3 + 2] = position[2];
            }
        }
        debug_log_sim_state(&[
            ctx.sim_tick.as_u32() as i32,
            player.x,
            player.y,
            player.z,
            i32::from(self.motor.yaw().as_q12()),
            self.motor.vertical_speed_q8(),
            i32::from(self.motor.grounded()),
            i32::from(self.room_index.raw()),
            eye.x,
            eye.y,
            eye.z,
            i32::from(self.camera.yaw().as_q12()),
            i32::from(self.camera.pitch_q12()),
            count as i32,
            hash as i32,
            first[0],
            first[1],
            first[2],
            first[3],
            first[4],
            first[5],
            first[6],
            first[7],
            first[8],
        ]);
    }
}
