//! Waypoint follower for demo-derived routes (`HLWPTS1`).
//!
//! Unlike `semantic_input` (an open-loop 20 Hz input tape), this replays a
//! demo playthrough as *positions*: each tick it steers the live player
//! toward the next waypoint with normal physics, pressing USE/JUMP/DUCK where
//! the source demo did. GoldSrc and hl-psx physics differ, so position
//! feedback is the only replay that survives the divergence. Diagnostic-only,
//! behind the `route-follow` feature; no `std`, no floats.
//!
//! Wire format (all little-endian):
//!   header: `HLWPTS1\0` | u16 n_segments | u16 reserved(0)
//!   segment: 16B NUL-padded map name | u32 n_wp
//!   waypoint (16B): i32 x | i32 y (vertical) | i32 z | u16 flags | u16 radius

// Only the route-follow feature links this. The follower moved verbatim from
// the ports; keep its arithmetic as written rather than restyle it for lints
// the game crates never ran.
#![allow(
    dead_code,
    clippy::precedence,
    clippy::new_without_default,
    clippy::manual_is_multiple_of,
    clippy::type_complexity
)]

use crate::semantic_input::{Sample, ACTION_DUCK, ACTION_JUMP, ACTION_USE};

pub const MAGIC: &[u8; 8] = b"HLWPTS1\0";
pub const FLAG_USE: u16 = 1 << 0;
pub const FLAG_JUMP: u16 = 1 << 1;
pub const FLAG_DUCK: u16 = 1 << 2;

const WP_SIZE: usize = 16;
const SEG_HEADER: usize = 20;

/// Horizontal arrival also requires the vertical gap to close (lift rides).
const VERTICAL_SLACK: i32 = 72;
/// Ticks without waypoint progress before recovery hops start.
const STALL_HOP: u32 = 200;
/// Ticks without progress before the waypoint is abandoned. Keeping the run
/// going yields a full report of every unreachable spot instead of one.
const STALL_SKIP: u32 = 900;

#[derive(Clone, Copy)]
pub struct Waypoint {
    pub pos: [i32; 3],
    pub flags: u16,
    pub radius: u16,
}

#[derive(Clone, Copy)]
pub struct Route {
    data: &'static [u8],
    n_segments: usize,
}

#[derive(Debug)]
pub struct Error;

fn rd_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn rd_i32(d: &[u8], o: usize) -> i32 {
    rd_u32(d, o) as i32
}

impl Route {
    pub fn parse(data: &'static [u8]) -> Result<Self, Error> {
        if data.len() < 12 || &data[..8] != MAGIC {
            return Err(Error);
        }
        let n_segments = rd_u16(data, 8) as usize;
        // Validate the segment chain covers the buffer exactly.
        let mut off = 12usize;
        for _ in 0..n_segments {
            if off + SEG_HEADER > data.len() {
                return Err(Error);
            }
            let n_wp = rd_u32(data, off + 16) as usize;
            off += SEG_HEADER + n_wp * WP_SIZE;
            if off > data.len() {
                return Err(Error);
            }
        }
        if off != data.len() {
            return Err(Error);
        }
        Ok(Self { data, n_segments })
    }

    /// Byte offset of the segment's waypoint array + its count.
    fn find_segment(&self, map_name: &str) -> Option<(usize, usize)> {
        let mut off = 12usize;
        for _ in 0..self.n_segments {
            let name = &self.data[off..off + 16];
            let end = name.iter().position(|&b| b == 0).unwrap_or(16);
            let n_wp = rd_u32(self.data, off + 16) as usize;
            if &name[..end] == map_name.as_bytes() {
                return Some((off + SEG_HEADER, n_wp));
            }
            off += SEG_HEADER + n_wp * WP_SIZE;
        }
        None
    }

    fn waypoint(&self, wp_off: usize, i: usize) -> Waypoint {
        let o = wp_off + i * WP_SIZE;
        Waypoint {
            pos: [
                rd_i32(self.data, o),
                rd_i32(self.data, o + 4),
                rd_i32(self.data, o + 8),
            ],
            flags: rd_u16(self.data, o + 12),
            radius: rd_u16(self.data, o + 14),
        }
    }
}

/// Integer atan2 in the runtime's yaw convention: forward = (sin yaw, ., cos
/// yaw) in world x/z, so `atan2_q12(dx, dz)` is the yaw that faces (dx, dz).
/// 33-entry quarter table, linear interpolation, exact to ~1 angle unit.
const ATAN_TAB: [u16; 33] = [
    0, 20, 41, 61, 81, 101, 121, 140, 160, 179, 197, 216, 234, 252, 269, 286, 303, 319, 335, 350,
    365, 380, 394, 408, 422, 435, 447, 460, 472, 484, 495, 506, 512,
];

fn atan_q12(num: i32, den: i32) -> i32 {
    // num <= den, both positive: angle 0..512
    if den == 0 {
        return 512;
    }
    let r = ((num << 12) / den).clamp(0, 4096);
    let idx = (r >> 7) as usize;
    let frac = r & 127;
    let a = ATAN_TAB[idx] as i32;
    let b = ATAN_TAB[(idx + 1).min(32)] as i32;
    a + ((b - a) * frac >> 7)
}

pub fn atan2_q12(x: i32, z: i32) -> u16 {
    if x == 0 && z == 0 {
        return 0;
    }
    let (ax, az) = (x.abs(), z.abs());
    let base = if az >= ax {
        atan_q12(ax, az)
    } else {
        1024 - atan_q12(az, ax)
    };
    let a = match (x >= 0, z >= 0) {
        (true, true) => base,
        (true, false) => 2048 - base,
        (false, false) => 2048 + base,
        (false, true) => 4096 - base,
    };
    (a & 0xFFF) as u16
}

pub struct Follower {
    seg: Option<(usize, usize)>,
    cursor: usize,
    stall: u32,
    skipped: u16,
}

impl Follower {
    pub const fn new() -> Self {
        Self {
            seg: None,
            cursor: 0,
            stall: 0,
            skipped: 0,
        }
    }

    /// Route segments are optional per map: an uncovered map plays neutral
    /// input rather than failing, so partial routes still verify their maps.
    pub fn begin_map(&mut self, route: Route, map_name: &str) {
        self.seg = route.find_segment(map_name);
        self.cursor = 0;
        self.stall = 0;
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn skipped(&self) -> u16 {
        self.skipped
    }

    pub fn done(&self) -> bool {
        matches!(self.seg, Some((_, n)) if self.cursor >= n)
    }

    pub fn steer(
        &mut self,
        route: Route,
        pos: [i32; 3],
        yaw: u16,
        pitch: i16,
        tick: u32,
    ) -> Sample {
        let Some((wp_off, n_wp)) = self.seg else {
            return Sample::NEUTRAL;
        };
        // Consume every waypoint already satisfied this tick (dense corners).
        while self.cursor < n_wp {
            let wp = route.waypoint(wp_off, self.cursor);
            let dx = (wp.pos[0] - pos[0]).clamp(-6000, 6000);
            let dy = wp.pos[1] - pos[1];
            let dz = (wp.pos[2] - pos[2]).clamp(-6000, 6000);
            let r = (wp.radius as i32).max(16);
            if dx * dx + dz * dz <= r * r && dy.abs() <= VERTICAL_SLACK {
                self.cursor += 1;
                self.stall = 0;
            } else {
                break;
            }
        }
        if self.cursor >= n_wp {
            return Sample::NEUTRAL;
        }
        let wp = route.waypoint(wp_off, self.cursor);
        let dx = (wp.pos[0] - pos[0]).clamp(-6000, 6000);
        let dy = wp.pos[1] - pos[1];
        let dz = (wp.pos[2] - pos[2]).clamp(-6000, 6000);
        let h2 = dx * dx + dz * dz;
        let r = (wp.radius as i32).max(16);

        let desired = atan2_q12(dx, dz);
        let err = ((desired as i32 - yaw as i32 + 2048) & 4095) - 2048;
        let turn = err.clamp(-127, 127) as i8;
        // Walk when roughly facing the goal; hold position when we are above
        // or below it (waiting on a lift) so tiny deltas do not orbit.
        // NB a reduced-speed band while turning (60 fwd at err<1024) measured
        // SLOWER end to end: corner-cutting changed the path enough to wedge
        // the player in c1a0d furniture the stop-turn-walk gait never touches.
        let forward = if h2 > r * r && err.abs() < 640 {
            127
        } else {
            0
        };
        // Aim up/down at nearby vertical goals: ladders climb by view pitch.
        let want_pitch: i32 = if h2 < 200 * 200 && dy > 48 {
            700
        } else if h2 < 200 * 200 && dy < -48 {
            -500
        } else {
            0
        };
        let look = (want_pitch - pitch as i32).clamp(-127, 127) as i8;

        let mut actions = 0u16;
        if wp.flags & FLAG_DUCK != 0 {
            actions |= ACTION_DUCK;
        }
        if wp.flags & FLAG_USE != 0 && h2 < 96 * 96 && tick % 8 == 0 {
            actions |= ACTION_USE;
        }
        if wp.flags & FLAG_JUMP != 0 && h2 < 128 * 128 && tick % 12 == 0 {
            actions |= ACTION_JUMP;
        }
        self.stall += 1;
        if self.stall > STALL_HOP && tick % 16 == 0 {
            actions |= ACTION_JUMP; // recovery hop over lips/debris
        }
        if self.stall > STALL_SKIP {
            self.cursor += 1; // abandon; later waypoints may still be reachable
            self.stall = 0;
            self.skipped = self.skipped.saturating_add(1);
        }
        Sample {
            forward,
            strafe: 0,
            turn,
            look,
            actions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_bytes(maps: &[(&str, &[(i32, i32, i32, u16)])]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(MAGIC);
        d.extend_from_slice(&(maps.len() as u16).to_le_bytes());
        d.extend_from_slice(&0u16.to_le_bytes());
        for (name, wps) in maps {
            let mut n = [0u8; 16];
            n[..name.len()].copy_from_slice(name.as_bytes());
            d.extend_from_slice(&n);
            d.extend_from_slice(&(wps.len() as u32).to_le_bytes());
            for &(x, y, z, flags) in *wps {
                d.extend_from_slice(&x.to_le_bytes());
                d.extend_from_slice(&y.to_le_bytes());
                d.extend_from_slice(&z.to_le_bytes());
                d.extend_from_slice(&flags.to_le_bytes());
                d.extend_from_slice(&24u16.to_le_bytes());
            }
        }
        d
    }

    fn leak(v: Vec<u8>) -> &'static [u8] {
        Box::leak(v.into_boxed_slice())
    }

    #[test]
    fn parse_rejects_garbage_and_truncation() {
        assert!(Route::parse(b"NOTAMAGIC000").is_err());
        let good = route_bytes(&[("c1a0", &[(0, 0, 100, 0)])]);
        let mut cut = good.clone();
        cut.pop();
        assert!(Route::parse(leak(cut)).is_err());
        assert!(Route::parse(leak(good)).is_ok());
    }

    #[test]
    fn atan2_matches_yaw_convention_on_axes() {
        assert_eq!(atan2_q12(0, 100), 0); // +z = yaw 0
        assert_eq!(atan2_q12(100, 0), 1024); // +x = yaw 1024
        assert_eq!(atan2_q12(0, -100), 2048);
        assert_eq!(atan2_q12(-100, 0), 3072);
        assert_eq!(atan2_q12(100, 100), 512);
    }

    #[test]
    fn steers_toward_waypoint_and_advances() {
        let bytes = leak(route_bytes(&[("c1a0", &[(0, 0, 100, 0), (0, 0, 400, 0)])]));
        let route = Route::parse(bytes).unwrap();
        let mut f = Follower::new();
        f.begin_map(route, "c1a0");
        // facing +x (yaw 1024) with target at +z: must turn negative and not walk
        let s = f.steer(route, [0, 0, 0], 1024, 0, 1);
        assert!(s.turn < 0, "turns toward +z");
        assert_eq!(s.forward, 0, "does not walk while facing away");
        // facing the goal: walks
        let s = f.steer(route, [0, 0, 0], 0, 0, 2);
        assert_eq!(s.forward, 127);
        // arriving at wp0 advances the cursor to wp1
        let _ = f.steer(route, [0, 0, 95], 0, 0, 3);
        assert_eq!(f.cursor(), 1);
        // past the last waypoint the follower goes neutral and reports done
        let _ = f.steer(route, [0, 0, 395], 0, 0, 4);
        let s = f.steer(route, [0, 0, 395], 0, 0, 5);
        assert!(f.done());
        assert_eq!(s.forward, 0);
    }

    #[test]
    fn unknown_map_plays_neutral() {
        let bytes = leak(route_bytes(&[("c1a0", &[(0, 0, 100, 0)])]));
        let route = Route::parse(bytes).unwrap();
        let mut f = Follower::new();
        f.begin_map(route, "c9z9");
        let s = f.steer(route, [0, 0, 0], 0, 0, 1);
        assert_eq!(s.forward, 0);
        assert_eq!(s.turn, 0);
    }
}
