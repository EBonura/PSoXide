//! HMA1 animation cooking (host side).
//!
//! Today's HMD8 palettes store one 20-byte model-space affine per used bone
//! per retained pose, with every bone sharing the same few pose times. HMA1
//! instead stores each bone's LOCAL rotation as a quaternion track with its
//! own key rate (every 1, 2, 3, 4, 6, 8 or 16 source frames, or one key),
//! picked per bone and clip as the cheapest option whose error, measured at
//! the bone's vertices, child joints and lever-arm points in model space,
//! stays inside a tolerance. Keys are fitted closed-loop against the already
//! quantized ancestors, so hierarchy error does not accumulate.
//!
//! Callers describe the skeleton ([`Skeleton`]) and supply clips through
//! [`ClipSource`] (exact local transforms at fractional source frames, in the
//! same cooked space the runtime uses). GoldSrc specifics stay with the
//! caller (hl-bsp's MDL baker).

#![allow(clippy::too_many_arguments, clippy::needless_range_loop)]

mod encode;
pub mod hma_dec;

pub use encode::{encode, needed_bones, track_bytes, Encoded, Opts};

/// Rotation (row-major) and translation, cooked units.
pub type Mat34 = ([[f64; 3]; 3], [f64; 3]);

/// Skeleton facts the encoder needs, all in cooked space.
pub struct Skeleton {
    /// Source parent index per bone, -1 for roots. Parents precede children.
    pub parents: Vec<i32>,
    /// Bind (default) local translation per bone, cooked units.
    pub bind_t: Vec<[f64; 3]>,
    /// Bone-local vertices per bone exactly as the runtime stores them.
    pub verts: Vec<Vec<[f64; 3]>>,
    /// Cooked units per world unit (the error tolerance is in world units).
    pub scale: f64,
}

/// One clip's exact source motion.
pub trait ClipSource {
    /// Identity for de-duplication: two clips with the same key share bytes.
    fn key(&self) -> usize;
    /// Source frame intervals covered by one pass (numframes - 1, >= 1).
    fn n_int(&self) -> usize;
    fn looping(&self) -> bool;
    /// Exact local transform of every bone at fractional source frame `pos`.
    fn local_at(&self, pos: f64) -> Vec<Mat34>;
}

pub fn apply(m: &Mat34, v: [f64; 3]) -> [f64; 3] {
    [
        m.0[0][0] * v[0] + m.0[0][1] * v[1] + m.0[0][2] * v[2] + m.1[0],
        m.0[1][0] * v[0] + m.0[1][1] * v[1] + m.0[1][2] * v[2] + m.1[1],
        m.0[2][0] * v[0] + m.0[2][1] * v[1] + m.0[2][2] * v[2] + m.1[2],
    ]
}

pub fn concat(p: &Mat34, c: &Mat34) -> Mat34 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = p.0[i][0] * c.0[0][j] + p.0[i][1] * c.0[1][j] + p.0[i][2] * c.0[2][j];
        }
    }
    (r, apply(p, c.1))
}

/// Compose local transforms parents-first into model space.
pub fn world_of(parents: &[i32], local: &[Mat34]) -> Vec<Mat34> {
    let mut out: Vec<Mat34> = Vec::with_capacity(local.len());
    for (b, l) in local.iter().enumerate() {
        let w = if parents[b] < 0 {
            *l
        } else {
            concat(&out[parents[b] as usize], l)
        };
        out.push(w);
    }
    out
}

/// Unit quaternion (x, y, z, w) of a rotation matrix.
pub fn mat_quat(m: &[[f64; 3]; 3]) -> [f64; 4] {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

/// Q12 quaternion (x, y, z, w) of a rotation matrix, for [`hmd8_section`]'s
/// jaw record.
pub fn quat_q12(m: &[[f64; 3]; 3]) -> [i16; 4] {
    let q = mat_quat(m);
    q.map(|v| (v * 4096.0).round().clamp(-32768.0, 32767.0) as i16)
}

/// The mouth controller as the runtime applies it: fold `open` (the fully
/// open controller rotation, Q12 quaternion) into HMA1 bone `bone`'s local
/// rotation, before it (`post == false`, a Z-rotation controller) or after
/// it (`post == true`, an X-rotation controller).
#[derive(Clone, Copy, Debug)]
pub struct JawRecord {
    pub bone: u8,
    pub post: bool,
    pub open: [i16; 4],
}

/// The HMD8 section that carries an HMA1 blob (`psx_asset::hmd8`, flag bit
/// 8): `u32 len`, `u16 n_used`, `u8 jaw_bone` (0xff none), `u8 jaw_flags`,
/// `i16 jaw_open[4]`, `u8 map[n_used]` (HMD8 bone -> HMA1 bone), padding so
/// the blob starts 2-aligned in the HMD8 file, the blob, four zero bytes (the
/// runtime reads keys a word at a time) and padding to 4. `abs_start` is the
/// offset of the section's first byte from the start of the HMD8 file.
pub fn hmd8_section(abs_start: usize, map: &[u8], jaw: Option<JawRecord>, blob: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(&(map.len() as u16).to_le_bytes());
    match jaw {
        Some(j) => {
            s.push(j.bone);
            s.push(j.post as u8);
            for v in j.open {
                s.extend_from_slice(&v.to_le_bytes());
            }
        }
        None => {
            s.extend_from_slice(&[0xff, 0]);
            for v in [0i16, 0, 0, 4096] {
                s.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    s.extend_from_slice(map);
    while (abs_start + 4 + s.len()) & 1 != 0 {
        s.push(0);
    }
    s.extend_from_slice(blob);
    s.extend_from_slice(&[0; 4]);
    while s.len() & 3 != 0 {
        s.push(0);
    }
    let mut out = Vec::with_capacity(4 + s.len());
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(&s);
    out
}

/// The recommended production setting: mixed 8/12-bit keys, range
/// reduction, closed-loop least-squares keys, absolute error bound.
pub fn production_opts(tol: f64) -> Opts {
    Opts {
        qfmt: 3,
        tol,
        closed_loop: true,
        flat: false,
        fit: true,
        normalize: true,
        rate_mask: 0x7f,
        absolute: true,
        cubic: false,
        budget: 0.0,
    }
}

/// Smallest tolerance on a fixed ladder whose blob fits `max_bytes` (the
/// model's current HMD8 pose bytes, for a no-regression cook). Falls back
/// to the loosest rung.
pub fn encode_within(
    sk: &Skeleton,
    clips: &[&dyn ClipSource],
    bones: &[usize],
    max_bytes: usize,
) -> (Encoded, f64) {
    const LADDER: [f64; 13] = [
        0.25, 0.35, 0.5, 0.7, 1.0, 1.4, 2.0, 2.8, 4.0, 5.6, 8.0, 11.0, 16.0,
    ];
    let mut last = None;
    for tol in LADDER {
        let e = encode(sk, clips, bones, production_opts(tol));
        if e.bytes.len() <= max_bytes {
            return (e, tol);
        }
        last = Some((e, tol));
    }
    last.unwrap()
}
