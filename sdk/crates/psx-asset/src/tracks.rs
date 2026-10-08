//! Version 6 `.psxanim` clips: per-joint keyed tracks.
//!
//! The layout is documented on [`psxed_format::animation::tracks`]. A clip holds
//! one rotation track (smallest-three quaternion keys) and one translation track
//! (variable-bit offsets) per joint. Every track is `segments + 1` evenly spaced
//! keys over the clip, with at most seven distinct segment counts per clip, so a
//! sample maps its playback position onto every rate once and each joint then
//! only indexes.
//!
//! Decoding is integer only: Q12 quaternion components, a 513 entry square root
//! table for the omitted component, Q8 interpolation, one renormalising step and
//! the usual quaternion to matrix products. The cooker calls the same functions
//! to prove its error budget, so what it measures is what ships.

use core::ptr;

use psx_gte::math::Vec3I32;
use psxed_format::animation::tracks as fmt;

use crate::{JointPose, ParseError};

/// `round(sqrt(n))` for the table builder.
const fn isqrt_round(n: u32) -> u32 {
    let mut lo = 0u32;
    let mut hi = 4200u32;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if mid * mid <= n {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if n - lo * lo > lo {
        lo + 1
    } else {
        lo
    }
}

const fn build_sqrt_table() -> [u16; 514] {
    let mut table = [0u16; 514];
    let mut i = 0usize;
    while i <= 513 {
        table[i] = isqrt_round((i as u32) << 15) as u16;
        i += 1;
    }
    table
}

/// `round(sqrt(index << 15))`: the Q12 square root of a Q24 value sampled every
/// 2^15, one entry past the top so the interpolation of an exact 2^24 (the
/// identity rotation) has a neighbour. The omitted quaternion component is at least one half, so the argument
/// stays far from the steep start of the curve and one linear step is exact to
/// well under a Q12 unit.
pub const SQRT_TABLE: [u16; 514] = build_sqrt_table();

const MUL: [i32; 3] = [
    fmt::component_mul(0),
    fmt::component_mul(1),
    fmt::component_mul(2),
];

const RATE_TABLE_OFFSET: usize = psxed_format::AssetHeader::SIZE + 8;
const DESCRIPTORS_OFFSET: usize = RATE_TABLE_OFFSET + fmt::RATE_TABLE_SIZE;

/// Positions of one playback sample on every rate of a clip.
///
/// Entry [`fmt::RATE_CONSTANT`] is always `(0, 0)`, which is how a constant
/// track reads its only key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TrackPos {
    idx: [u16; 8],
    frac: [u16; 8],
}

impl TrackPos {
    /// All tracks at their first key.
    pub const ZERO: Self = Self {
        idx: [0; 8],
        frac: [0; 8],
    };

    /// Key index and Q8 fraction (`0..=256`) of rate `rate`.
    pub fn rate(&self, rate: usize) -> (u16, u16) {
        (self.idx[rate & 7], self.frac[rate & 7])
    }
}

/// A decoded rotation key: Q12 `[x, y, z, w]` and the omitted component index.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RotKey {
    /// Q12 quaternion components, x y z w.
    pub q: [i32; 4],
    /// Index of the component the key omitted.
    pub omitted: u8,
}

/// Validated view of a v6 payload. Built only by [`validate`], which proves
/// every unchecked read in this module stays inside `blob`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tracks {
    blob: *const u8,
    joint_count: u16,
    key_area: usize,
    len: usize,
    translation_shift: u8,
}

// SAFETY: `Tracks` is only a pointer to immutable cooked data owned by the
// caller's `'a` slice; it is never written through.
unsafe impl Send for Tracks {}
// SAFETY: as above, shared reads of immutable data.
unsafe impl Sync for Tracks {}

#[inline(always)]
fn le_u16(blob: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([blob[at], blob[at + 1]])
}

/// Check a v6 blob completely. On success every key read the decoder can make
/// lies inside `blob`.
pub(crate) fn validate(
    blob: &[u8],
    joint_count: u16,
    frame_count: u16,
    translation_shift: u8,
) -> Result<Tracks, ParseError> {
    let bad = ParseError::InvalidAnimationLayout;
    if !(2..=fmt::MAX_FRAMES).contains(&frame_count) || translation_shift > 8 {
        return Err(bad);
    }
    let joints = joint_count as usize;
    let key_area = joints
        .checked_mul(fmt::DESCRIPTOR_SIZE)
        .and_then(|d| d.checked_add(DESCRIPTORS_OFFSET))
        .ok_or(ParseError::TableOverflow)?;
    if blob.len() < key_area + fmt::SLACK_BYTES {
        return Err(ParseError::Truncated);
    }
    let area_len = blob.len() - fmt::SLACK_BYTES - key_area;
    if area_len > fmt::MAX_KEY_AREA {
        return Err(bad);
    }
    let rate_count = blob[RATE_TABLE_OFFSET] as usize;
    if rate_count == 0 || rate_count > fmt::MAX_RATES {
        return Err(bad);
    }
    let mut segments = [0usize; 8];
    for (r, slot) in segments.iter_mut().enumerate().take(rate_count) {
        let s = le_u16(blob, RATE_TABLE_OFFSET + 2 + r * 2) as usize;
        if s == 0 || s > frame_count as usize - 1 {
            return Err(bad);
        }
        *slot = s;
    }
    let keys_for = |rate: u8| -> Option<usize> {
        if rate == fmt::RATE_CONSTANT {
            Some(1)
        } else if (rate as usize) < rate_count {
            Some(segments[rate as usize] + 1)
        } else {
            None
        }
    };
    // Every read is a u32 at the key start plus a u16 four bytes on.
    let fits = |off: usize, keys: usize, stride: usize| -> bool {
        let last = off + (keys - 1) * stride;
        let end = off + keys * stride;
        end <= area_len && last + 6 <= area_len + fmt::SLACK_BYTES
    };
    for j in 0..joints {
        let d = DESCRIPTORS_OFFSET + j * fmt::DESCRIPTOR_SIZE;
        let rot_ctl = blob[d];
        let trans_ctl = blob[d + 1];
        let axis = le_u16(blob, d + 2);
        let rot_off = le_u16(blob, d + 4) as usize;
        let trans_off = le_u16(blob, d + 6) as usize;
        let key_bytes = blob[d + 14] as usize;
        let size = (rot_ctl >> 3) & 3;
        if rot_ctl >> 5 != 0 || size > 2 || trans_ctl >> 7 != 0 || axis >> 15 != 0 {
            return Err(bad);
        }
        let rot_keys = keys_for(rot_ctl & 7).ok_or(bad)?;
        let trans_keys = keys_for(trans_ctl & 7).ok_or(bad)?;
        let (bx, by, bz) = (
            (axis & 31) as usize,
            ((axis >> 5) & 31) as usize,
            ((axis >> 10) & 31) as usize,
        );
        // The axis extraction needs b0 + b1 < 32 and each field below 32 bits.
        if bx > 15 || by > 15 || bz > 15 || key_bytes != (bx + by + bz).div_ceil(8) {
            return Err(bad);
        }
        if !fits(rot_off, rot_keys, fmt::rotation_key_bytes(size)) {
            return Err(bad);
        }
        if !fits(trans_off, trans_keys, key_bytes) {
            return Err(bad);
        }
    }
    Ok(Tracks {
        blob: blob.as_ptr(),
        joint_count,
        key_area,
        len: blob.len(),
        translation_shift,
    })
}

/// Map a playback position (Q8 source frames in `0..(frame_count - 1) << 8`) to
/// a key index and Q8 fraction on every rate.
pub(crate) fn track_pos(blob: &[u8], pos_q8: u32) -> TrackPos {
    let mut out = TrackPos::ZERO;
    let rate_count = blob[RATE_TABLE_OFFSET] as usize;
    let mut r = 0;
    while r < rate_count {
        let seg = u32::from(le_u16(blob, RATE_TABLE_OFFSET + 2 + r * 2));
        let factor = u32::from(le_u16(blob, RATE_TABLE_OFFSET + 16 + r * 2));
        // pos_q8 < 2^16 and factor <= 2^15 keep the product inside u32.
        let sp = (pos_q8 * factor) >> 15;
        let i = sp >> 8;
        if i >= seg {
            out.idx[r] = (seg - 1) as u16;
            out.frac[r] = 256;
        } else {
            out.idx[r] = i as u16;
            out.frac[r] = (sp & 255) as u16;
        }
        r += 1;
    }
    out
}

#[inline(always)]
unsafe fn read_u32(p: *const u8) -> u32 {
    // SAFETY: the caller keeps `p..p + 4` inside the validated blob.
    u32::from_le(unsafe { ptr::read_unaligned(p.cast::<u32>()) })
}

#[inline(always)]
unsafe fn read_u16(p: *const u8) -> u32 {
    // SAFETY: the caller keeps `p..p + 2` inside the validated blob.
    u32::from(u16::from_le(unsafe {
        ptr::read_unaligned(p.cast::<u16>())
    }))
}

/// Decode one rotation key. Needs six readable bytes at `p`.
///
/// # Safety
/// `p..p + 6` must be readable.
#[inline(always)]
unsafe fn decode_rot(p: *const u8, size: u32) -> RotKey {
    // SAFETY: forwarded from the caller.
    let lo = unsafe { read_u32(p) };
    // SAFETY: forwarded from the caller.
    let hi = unsafe { read_u16(p.add(4)) };
    let bits = 10 + 2 * size;
    let mask = (1u32 << bits) - 1;
    let sh1 = 2 + bits;
    let sh2 = 2 + 2 * bits;
    let sign = 32 - bits;
    let c0 = (lo >> 2) & mask;
    let c1 = (lo >> sh1) & mask;
    let c2 = ((lo >> sh2) | (hi << (32 - sh2))) & mask;
    let mul = MUL[size as usize];
    let dq = |c: u32| -> i32 {
        let signed = ((c << sign) as i32) >> sign;
        (signed * mul + (1 << 13)) >> 14
    };
    let (a, b, c) = (dq(c0), dq(c1), dq(c2));
    let r = (1i32 << 24) - (a * a + b * b + c * c);
    let r = if r < 0 { 0 } else { r };
    let i = (r >> 15) as usize;
    let t0 = i32::from(SQRT_TABLE[i]);
    let t1 = i32::from(SQRT_TABLE[i + 1]);
    let w = t0 + (((t1 - t0) * (r & 0x7fff)) >> 15);
    let omitted = (lo & 3) as u8;
    let q = match omitted {
        0 => [w, a, b, c],
        1 => [a, w, b, c],
        2 => [a, b, w, c],
        _ => [a, b, c, w],
    };
    RotKey { q, omitted }
}

/// Matrix of the quaternion `a` blended towards `b` by `frac / 256`.
#[inline(always)]
fn rotation_matrix(a: RotKey, b: Option<RotKey>, frac: i32) -> [[i16; 3]; 3] {
    let mut q = a.q;
    let mut renorm = false;
    if let Some(b) = b {
        let mut bq = b.q;
        // Canonical keys keep their largest component positive, so two keys that
        // agree on it can only be on one hemisphere unless they are far apart.
        // When it differs, check the dot product and take the short way round.
        if a.omitted != b.omitted
            && a.q[0] * bq[0] + a.q[1] * bq[1] + a.q[2] * bq[2] + a.q[3] * bq[3] < 0
        {
            bq = [-bq[0], -bq[1], -bq[2], -bq[3]];
        }
        q = [
            a.q[0] + (((bq[0] - a.q[0]) * frac) >> 8),
            a.q[1] + (((bq[1] - a.q[1]) * frac) >> 8),
            a.q[2] + (((bq[2] - a.q[2]) * frac) >> 8),
            a.q[3] + (((bq[3] - a.q[3]) * frac) >> 8),
        ];
        renorm = frac != 0 && frac != 256;
    }
    quat_to_matrix(q, renorm)
}

#[inline(always)]
fn quat_to_matrix(q: [i32; 4], renorm: bool) -> [[i16; 3]; 3] {
    let [mut x, mut y, mut z, mut w] = q;
    if renorm {
        // One Newton-style step of the inverse square root around unit length.
        let n = (x * x + y * y + z * z + w * w + 2048) >> 12;
        let e = (4096 - n).clamp(-4096, 4096);
        let r = 4096 + (e >> 1) + ((3 * e * e) >> 15);
        x = (x * r + 2048) >> 12;
        y = (y * r + 2048) >> 12;
        z = (z * r + 2048) >> 12;
        w = (w * r + 2048) >> 12;
    }
    let m2 = |a: i32, b: i32| (a * b + 1024) >> 11;
    let (xx, yy, zz) = (m2(x, x), m2(y, y), m2(z, z));
    let (xy, xz, yz) = (m2(x, y), m2(x, z), m2(y, z));
    let (wx, wy, wz) = (m2(w, x), m2(w, y), m2(w, z));
    [
        [
            (4096 - (yy + zz)) as i16,
            (xy - wz) as i16,
            (xz + wy) as i16,
        ],
        [
            (xy + wz) as i16,
            (4096 - (xx + zz)) as i16,
            (yz - wx) as i16,
        ],
        [
            (xz - wy) as i16,
            (yz + wx) as i16,
            (4096 - (xx + yy)) as i16,
        ],
    ]
}

/// Decode a translation key into three axis codes.
///
/// # Safety
/// `p..p + 6` must be readable.
#[inline(always)]
unsafe fn decode_trans_codes(p: *const u8, axis: u32) -> [i32; 3] {
    // SAFETY: forwarded from the caller.
    let lo = unsafe { read_u32(p) };
    // SAFETY: forwarded from the caller.
    let hi = unsafe { read_u16(p.add(4)) };
    let (b0, b1, b2) = (axis & 31, (axis >> 5) & 31, (axis >> 10) & 31);
    let (m0, m1, m2) = ((1u32 << b0) - 1, (1u32 << b1) - 1, (1u32 << b2) - 1);
    let sh = b0 + b1;
    let c0 = lo & m0;
    let c1 = (lo >> b0) & m1;
    // `<< (31 - sh) << 1` keeps the shift below 32 even when sh == 0.
    let c2 = ((lo >> sh) | ((hi << (31 - sh)) << 1)) & m2;
    [c0 as i32, c1 as i32, c2 as i32]
}

/// Evaluate the pose of joint `joint` at the positions in `pos`.
///
/// # Safety
/// `tracks` must come from [`validate`] and `joint < joint_count`.
#[inline]
pub(crate) unsafe fn joint_pose(tracks: &Tracks, joint: usize, pos: &TrackPos) -> JointPose {
    debug_assert!(joint < tracks.joint_count as usize);
    // SAFETY: `joint < joint_count`, so the 16 byte descriptor is inside the
    // blob `validate` measured.
    let d = unsafe {
        tracks
            .blob
            .add(DESCRIPTORS_OFFSET + joint * fmt::DESCRIPTOR_SIZE)
    };
    // SAFETY: the descriptor is 16 readable bytes.
    let w0 = unsafe { read_u32(d) };
    // SAFETY: as above.
    let w1 = unsafe { read_u32(d.add(4)) };
    // SAFETY: as above.
    let w2 = unsafe { read_u32(d.add(8)) };
    // SAFETY: as above.
    let w3 = unsafe { read_u32(d.add(12)) };
    let rot_rate = (w0 & 7) as usize;
    let size = (w0 >> 3) & 3;
    let trans_rate = ((w0 >> 8) & 7) as usize;
    let step = (w0 >> 11) & 15;
    let axis = w0 >> 16;
    let rot_off = (w1 & 0xffff) as usize;
    let trans_off = (w1 >> 16) as usize;
    let key_bytes = (w3 >> 16) & 0xff;
    // SAFETY: `validate` proved offsets, key counts and strides stay inside the
    // key area plus its slack, for every index `track_pos` can produce.
    let area = unsafe { tracks.blob.add(tracks.key_area) };

    let (ri, rf) = (pos.idx[rot_rate] as usize, i32::from(pos.frac[rot_rate]));
    let stride = fmt::rotation_key_bytes(size as u8);
    // Debug builds (the corruption tests) check every window against the blob.
    debug_assert!(
        tracks.key_area + rot_off + (ri + usize::from(rf != 0)) * stride + 6 <= tracks.len
    );
    // SAFETY: key `ri` of the rotation track starts inside the validated range.
    let ka = unsafe { decode_rot(area.add(rot_off + ri * stride), size) };
    let matrix = if rf == 0 {
        rotation_matrix(ka, None, 0)
    } else {
        // SAFETY: a non zero fraction implies `ri + 1` is a key of the track.
        let kb = unsafe { decode_rot(area.add(rot_off + (ri + 1) * stride), size) };
        rotation_matrix(ka, Some(kb), rf)
    };

    let (ti, tf) = (
        pos.idx[trans_rate] as usize,
        i32::from(pos.frac[trans_rate]),
    );
    let min = [
        (w2 as i16) as i32,
        ((w2 >> 16) as i16) as i32,
        (w3 as i16) as i32,
    ];
    let shift = u32::from(tracks.translation_shift);
    let kb = key_bytes as usize;
    debug_assert!(tracks.key_area + trans_off + (ti + usize::from(tf != 0)) * kb + 6 <= tracks.len);
    // SAFETY: as for the rotation track.
    let ca = unsafe { decode_trans_codes(area.add(trans_off + ti * kb), axis) };
    let mut t = [
        min[0] + (ca[0] << step),
        min[1] + (ca[1] << step),
        min[2] + (ca[2] << step),
    ];
    if tf != 0 {
        // SAFETY: a non zero fraction implies `ti + 1` is a key of the track.
        let cb = unsafe { decode_trans_codes(area.add(trans_off + (ti + 1) * kb), axis) };
        for a in 0..3 {
            let b = min[a] + (cb[a] << step);
            t[a] += ((b - t[a]) * tf) >> 8;
        }
    }
    JointPose {
        matrix,
        translation: Vec3I32::new(t[0] << shift, t[1] << shift, t[2] << shift),
    }
}

/// Safe single-key helpers the cooker uses to search and verify tracks with the
/// exact decoder.
pub mod host {
    use super::*;

    /// Decode a rotation key from the first six bytes of `key`.
    pub fn decode_rotation_key(key: &[u8; 6], size_code: u8) -> RotKey {
        // SAFETY: `key` is exactly six readable bytes.
        unsafe { decode_rot(key.as_ptr(), u32::from(size_code.min(2))) }
    }

    /// Matrix of `a` blended towards `b` by `frac / 256` (`0..=256`).
    pub fn blend_rotation(a: RotKey, b: Option<RotKey>, frac: i32) -> [[i16; 3]; 3] {
        rotation_matrix(a, b, frac)
    }

    /// Decode a translation key's three axis codes from six bytes.
    pub fn decode_translation_codes(key: &[u8; 6], axis_bits: u16) -> [i32; 3] {
        // SAFETY: `key` is exactly six readable bytes.
        unsafe { decode_trans_codes(key.as_ptr(), u32::from(axis_bits)) }
    }

    /// Key index and fraction of a playback position, as the sampler computes
    /// them for a clip of `frame_count` frames with `segments` segments.
    pub fn locate(segments: u16, frame_count: u16, pos_q8: u32) -> (u16, u16) {
        let factor = ((u32::from(segments) << 15) + (u32::from(frame_count) - 1) / 2)
            / (u32::from(frame_count) - 1);
        let sp = (pos_q8 * factor) >> 15;
        let i = sp >> 8;
        if i >= u32::from(segments) {
            (segments - 1, 256)
        } else {
            (i as u16, (sp & 255) as u16)
        }
    }

    /// The Q15 factor stored in the rate table for a segment count.
    pub fn rate_factor(segments: u16, frame_count: u16) -> u16 {
        (((u32::from(segments) << 15) + (u32::from(frame_count) - 1) / 2)
            / (u32::from(frame_count) - 1)) as u16
    }
}
