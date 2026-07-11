//! Debug "freelook" camera: an optional view-transform delta injected into the
//! GTE for `RTPS`/`RTPT` so the projected camera can rotate and dolly while the
//! emulated game keeps running.
//!
//! ## How it works
//! Every 3D PS1 game funnels world geometry through the GTE: it loads a view
//! rotation matrix into control regs 0..=4 and a translation into 5..=7, then
//! issues `RTPS` (one vertex) or `RTPT` (three). We compose a delta `(Rd, Td)`
//! onto that transform in view space just before the op runs, then restore the
//! originals so the game's loaded matrix is untouched for whatever it does next:
//!
//! ```text
//! view' = Rd * (RT * V + TR) + Td   =>   RT' = Rd*RT,   TR' = Rd*TR + Td
//! ```
//!
//! Because we touch only the RT/TR view matrix (not the light/colour matrices
//! `MVMVA` uses), lighting is unaffected, and 2D HUD primitives never reach
//! `RTPS` so they stay put. The game still culls and streams against its OWN
//! camera, so pushing the view far reveals holes -- a hard invariant of
//! injecting at the projection layer, not a bug.
// ponytail: host f32 + per-op compose, debug path only. The bit-accurate GTE
// model in psx-gte-core stays integer and untouched; this never runs unless a
// caller flips `enabled`.

use psx_gte_core::Gte;

/// GTE rotation-matrix entries are 1.3.12 fixed point (raw / 4096).
const RT_ONE: f32 = 4096.0;

/// GTE command numbers we inject into (low 6 bits of the COP2 instruction).
const CMD_RTPS: u32 = 0x01;
const CMD_RTPT: u32 = 0x30;

/// Optional debug freelook camera delta. The frontend sets this on the CPU once
/// per frame; `enabled = false` (the default) is a no-op.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FreelookState {
    /// When `false`, the GTE hook is a no-op and the game projects normally.
    pub enabled: bool,
    /// Delta yaw about the view Y axis, radians.
    pub yaw: f32,
    /// Delta pitch about the view X axis, radians.
    pub pitch: f32,
    /// View-space translation offset X, GTE world units.
    pub tx: f32,
    /// View-space translation offset Y, GTE world units.
    pub ty: f32,
    /// View-space translation offset Z, GTE world units.
    pub tz: f32,
}

type Mat3 = [[f32; 3]; 3];
type Vec3 = [f32; 3];

fn mat3_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut r = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

fn mat3_vec(m: Mat3, v: Vec3) -> Vec3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Delta rotation `Rd = yaw(Y) * pitch(X)`.
fn delta_matrix(fl: &FreelookState) -> Mat3 {
    let (sy, cy) = fl.yaw.sin_cos();
    let (sp, cp) = fl.pitch.sin_cos();
    let yaw = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let pitch = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
    mat3_mul(yaw, pitch)
}

/// Compose the freelook delta onto a view transform. Pure: the testable core.
fn compose(rt: Mat3, tr: Vec3, fl: &FreelookState) -> (Mat3, Vec3) {
    let rd = delta_matrix(fl);
    let rt2 = mat3_mul(rd, rt);
    let mut tr2 = mat3_vec(rd, tr);
    tr2[0] += fl.tx;
    tr2[1] += fl.ty;
    tr2[2] += fl.tz;
    (rt2, tr2)
}

// --- GTE control-register (un)packing ---------------------------------------
// ctrl 0: RT11|RT12   1: RT13|RT21   2: RT22|RT23   3: RT31|RT32   4: RT33
// Each RT half is an i16 in the low/high word. ctrl 5/6/7: TRX/TRY/TRZ (i32).

fn lo(v: u32) -> f32 {
    (v & 0xffff) as i16 as f32 / RT_ONE
}
fn hi(v: u32) -> f32 {
    (v >> 16) as i16 as f32 / RT_ONE
}
fn pack(a: f32, b: f32) -> u32 {
    let q = |x: f32| ((x * RT_ONE).round().clamp(-32768.0, 32767.0) as i16) as u16 as u32;
    q(a) | (q(b) << 16)
}

fn read_view(cop2: &Gte) -> (Mat3, Vec3) {
    let (c0, c1, c2, c3, c4) = (
        cop2.read_control(0),
        cop2.read_control(1),
        cop2.read_control(2),
        cop2.read_control(3),
        cop2.read_control(4),
    );
    let rt = [
        [lo(c0), hi(c0), lo(c1)],
        [hi(c1), lo(c2), hi(c2)],
        [lo(c3), hi(c3), lo(c4)],
    ];
    let tr = [
        cop2.read_control(5) as i32 as f32,
        cop2.read_control(6) as i32 as f32,
        cop2.read_control(7) as i32 as f32,
    ];
    (rt, tr)
}

fn write_view(cop2: &mut Gte, rt: Mat3, tr: Vec3) {
    cop2.write_control(0, pack(rt[0][0], rt[0][1]));
    cop2.write_control(1, pack(rt[0][2], rt[1][0]));
    cop2.write_control(2, pack(rt[1][1], rt[1][2]));
    cop2.write_control(3, pack(rt[2][0], rt[2][1]));
    cop2.write_control(4, pack(rt[2][2], 0.0));
    cop2.write_control(5, tr[0] as i32 as u32);
    cop2.write_control(6, tr[1] as i32 as u32);
    cop2.write_control(7, tr[2] as i32 as u32);
}

/// If `instr` is an `RTPS`/`RTPT` and `fl` is enabled, compose the freelook
/// delta onto the view transform and return the saved control regs 0..=7 so the
/// caller can [`restore`] them after the op. Returns `None` (nothing changed)
/// otherwise.
pub(crate) fn apply_for_op(cop2: &mut Gte, fl: &FreelookState, instr: u32) -> Option<[u32; 8]> {
    if !fl.enabled {
        return None;
    }
    let cmd = instr & 0x3f;
    if cmd != CMD_RTPS && cmd != CMD_RTPT {
        return None;
    }
    let mut saved = [0u32; 8];
    for (i, s) in saved.iter_mut().enumerate() {
        *s = cop2.read_control(i as u8);
    }
    let (rt, tr) = read_view(cop2);
    let (rt2, tr2) = compose(rt, tr, fl);
    write_view(cop2, rt2, tr2);
    Some(saved)
}

/// Restore control regs 0..=7 saved by [`apply_for_op`].
pub(crate) fn restore(cop2: &mut Gte, saved: &[u32; 8]) {
    for (i, v) in saved.iter().enumerate() {
        cop2.write_control(i as u8, *v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }
    fn mat_approx(a: Mat3, b: Mat3) {
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    approx(a[i][j], b[i][j]),
                    "i{i} j{j}: {} != {}",
                    a[i][j],
                    b[i][j]
                );
            }
        }
    }

    #[test]
    fn identity_delta_is_passthrough() {
        let fl = FreelookState {
            enabled: true,
            ..Default::default()
        };
        let (rt2, tr2) = compose(ID, [10.0, 20.0, 30.0], &fl);
        mat_approx(rt2, ID);
        assert!(approx(tr2[0], 10.0) && approx(tr2[1], 20.0) && approx(tr2[2], 30.0));
    }

    #[test]
    fn yaw_90_rotates_identity_to_yaw_matrix() {
        let fl = FreelookState {
            enabled: true,
            yaw: std::f32::consts::FRAC_PI_2,
            ..Default::default()
        };
        let (rt2, _) = compose(ID, [0.0; 3], &fl);
        mat_approx(rt2, [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]]);
    }

    #[test]
    fn translation_offset_adds_in_view_space() {
        let fl = FreelookState {
            enabled: true,
            tx: 5.0,
            ty: -7.0,
            tz: 100.0,
            ..Default::default()
        };
        let (_, tr2) = compose(ID, [1.0, 2.0, 3.0], &fl);
        assert!(approx(tr2[0], 6.0) && approx(tr2[1], -5.0) && approx(tr2[2], 103.0));
    }

    #[test]
    fn pack_unpack_roundtrips_rotation_entries() {
        let p = pack(0.5, -0.25); // 1.3.12: 0.5 -> 2048, -0.25 -> -1024
        assert!(approx(lo(p), 0.5));
        assert!(approx(hi(p), -0.25));
    }
}
