//! HMA1: per-bone local quaternion animation tracks (the HMD8 pose-palette
//! replacement cooked by `psx-anim-cook`).
//!
//! Each clip stores, per bone, a rotation track and a translation code:
//!
//! * rotation: one key, or keys every 1/2/3/4/6/8/16 source frames (the
//!   clip header maps the playback position to each rate with one multiply,
//!   so a bone's key lookup is an index, not a search);
//! * 8-bit keys are range-reduced (i8 base + 4/6/8-bit offsets per
//!   component), 12-bit keys are packed; the cooker picks per bone;
//! * translation: the bind value, one per-clip value, or range-reduced Q2
//!   keys (quarter cooked units, so hierarchy rounding stays small).
//!
//! [`Model::decode`] evaluates every bone parents-first into model-space
//! affines. Composition runs on the GTE (MVMVA sf=0, rounded on the CPU) so
//! it matches the host reference decoder bit for bit; on other targets the
//! same arithmetic runs in Rust.
//!
//! Layout (little endian): u16 n_bones, u16 n_clips, u8 parent[n] (0xff
//! root), pad to 2, i16 bind_t[n][3] (Q2), pad to 4, u32 clip_off[n_clips];
//! clip: u16 n_int, u8 flags, u8 qfmt(=3), u16 seg[7], u16 factor_q15[7],
//! u8 mode[n], pad to 2, then byte-packed tracks.

#![allow(missing_docs, clippy::needless_range_loop)]

use core::ptr;

pub const RATE_COUNT: usize = 7;
/// Renamed to [`RATE_COUNT`].
#[deprecated(note = "renamed to `RATE_COUNT`")]
pub const N_RATES: usize = RATE_COUNT;
const CLIP_HEADER: usize = 4 + 2 * RATE_COUNT * 2;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Affine {
    pub r: [[i16; 3]; 3],
    pub _pad: i16,
    pub t: [i32; 3],
}

impl Affine {
    pub const ZERO: Affine = Affine {
        r: [[0; 3]; 3],
        _pad: 0,
        t: [0; 3],
    };
}

/// Renamed to [`Affine`].
#[deprecated(note = "renamed to `Affine`")]
pub type Aff = Affine;

// The readers below take a raw pointer into a blob `Model::new` validated.
// Each has the same contract: every byte it reads, `d + o` up to the width it
// names, lies inside that blob.

#[inline(always)]
unsafe fn u8_at(d: *const u8, o: usize) -> u32 {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe { *d.add(o) as u32 }
}
#[inline(always)]
unsafe fn i8_at(d: *const u8, o: usize) -> i32 {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe { *d.add(o) as i8 as i32 }
}
#[inline(always)]
unsafe fn u16_at(d: *const u8, o: usize) -> u32 {
    // track data is byte-packed: odd offsets are legal
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe { *d.add(o) as u32 | (*d.add(o + 1) as u32) << 8 }
}
#[inline(always)]
unsafe fn i16_at(d: *const u8, o: usize) -> i32 {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe { (*d.add(o) as u32 | (*d.add(o + 1) as u32) << 8) as i16 as i32 }
}
#[inline(always)]
unsafe fn u32_unaligned(d: *const u8, o: usize) -> u32 {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe { ptr::read_unaligned(d.add(o).cast::<u32>()) }
}

#[inline(always)]
unsafe fn read8(d: *const u8, o: usize) -> [i32; 4] {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe {
        [
            i8_at(d, o) << 5,
            i8_at(d, o + 1) << 5,
            i8_at(d, o + 2) << 5,
            i8_at(d, o + 3) << 5,
        ]
    }
}
#[inline(always)]
unsafe fn read12(d: *const u8, o: usize) -> [i32; 4] {
    // SAFETY: the caller guarantees the bytes read are inside the blob.
    unsafe {
        let packed = u16_at(d, o) | (u16_at(d, o + 2) << 16);
        let w2 = u16_at(d, o + 4);
        let s = |v: u32| (((v << 20) as i32) >> 20) << 1;
        [
            s(packed),
            s(packed >> 12),
            s((packed >> 24) | (w2 << 8)),
            s(w2 >> 4),
        ]
    }
}

/// Segment index, Q8 fraction and key count of rate `r` for the clip at
/// `off`: the position maps to each rate with one multiply.
#[inline(never)]
unsafe fn seg(d: *const u8, off: usize, r: usize, pos_q8: u32) -> (usize, i32, usize) {
    // SAFETY: `off` is a validated clip and `r < RATE_COUNT`, so both reads are
    // inside its 32-byte header.
    unsafe {
        let s = u16_at(d, off + 4 + r * 2) as usize;
        let factor = u16_at(d, off + 4 + RATE_COUNT * 2 + r * 2);
        let sp = (pos_q8 * factor) >> 15;
        let i = (sp >> 8) as usize;
        if i >= s {
            (s.saturating_sub(1), 256, s + 1)
        } else {
            (i, (sp & 255) as i32, s + 1)
        }
    }
}

#[inline(always)]
fn lerp4(a: [i32; 4], b: [i32; 4], f: i32) -> [i32; 4] {
    [
        a[0] + (((b[0] - a[0]) * f) >> 8),
        a[1] + (((b[1] - a[1]) * f) >> 8),
        a[2] + (((b[2] - a[2]) * f) >> 8),
        a[3] + (((b[3] - a[3]) * f) >> 8),
    ]
}

#[inline(always)]
fn quat_to_mat(q: [i32; 4]) -> [[i16; 3]; 3] {
    let [mut x, mut y, mut z, mut w] = q;
    let n = (x * x + y * y + z * z + w * w + 2048) >> 12;
    // The renormalising series is only meaningful while the length error is at most one unit;
    // clamping keeps a damaged key (components up to 12224) from overflowing `3 * e * e`,
    // and leaves every in-range key as it was.
    let e = (4096 - n).clamp(-4096, 4096);
    let r = 4096 + (e >> 1) + ((3 * e * e) >> 15);
    x = (x * r + 2048) >> 12;
    y = (y * r + 2048) >> 12;
    z = (z * r + 2048) >> 12;
    w = (w * r + 2048) >> 12;
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

/// MVMVA(RT, V0, cv=none, sf=0): raw 32-bit MAC1..3 for one column.
#[cfg(target_arch = "mips")]
#[inline(always)]
fn mvmva_raw(xy: u32, z: u32) -> [i32; 3] {
    // SAFETY: GTE-only asm: it writes VXY0/VZ0, runs MVMVA on the rotation
    // already loaded, and reads MAC1..3 into $8..$10, all declared operands.
    // No memory or stack access.
    unsafe {
        let m1: u32;
        let m2: u32;
        let m3: u32;
        core::arch::asm!(
            ".word 0x48880000", // mtc2 $8, VXY0
            ".word 0x48890800", // mtc2 $9, VZ0
            ".word 0",          // VXY0 commit gap (HWB-010/011)
            ".word 0",
            ".word 0x4a006012", // MVMVA RT,V0,none,sf=0
            ".word 0x4808c800", // mfc2 $8, MAC1
            ".word 0x4809d000", // mfc2 $9, MAC2
            ".word 0x480ad800", // mfc2 $10, MAC3
            ".word 0",
            inlateout("$8") xy => m1,
            inlateout("$9") z => m2,
            lateout("$10") m3,
            options(nostack, nomem, preserves_flags),
        );
        [m1 as i32, m2 as i32, m3 as i32]
    }
}

#[inline(always)]
fn pack(a: i32, b: i32) -> u32 {
    (a as u32 & 0xffff) | ((b as u32) << 16)
}

/// parent * (r, t), rounded. On the PS1 the parent's rotation must already
/// be in the GTE (see `decode`); elsewhere the same sums run in Rust.
#[inline(always)]
fn compose_loaded(p: &Affine, r: &[[i16; 3]; 3], t: [i32; 3], out: &mut Affine) {
    let rd = |v: i32| (v + 2048) >> 12;
    let cols = |xy: u32, z: u32| -> [i32; 3] {
        #[cfg(target_arch = "mips")]
        {
            mvmva_raw(xy, z)
        }
        #[cfg(not(target_arch = "mips"))]
        {
            let v = [
                (xy & 0xffff) as i16 as i32,
                (xy >> 16) as i16 as i32,
                z as i16 as i32,
            ];
            let mut o = [0i32; 3];
            for i in 0..3 {
                o[i] = p.r[i][0] as i32 * v[0] + p.r[i][1] as i32 * v[1] + p.r[i][2] as i32 * v[2];
            }
            o
        }
    };
    let c0 = cols(pack(r[0][0] as i32, r[1][0] as i32), r[2][0] as i32 as u32);
    let c1 = cols(pack(r[0][1] as i32, r[1][1] as i32), r[2][1] as i32 as u32);
    let c2 = cols(pack(r[0][2] as i32, r[1][2] as i32), r[2][2] as i32 as u32);
    let ct = cols(pack(t[0], t[1]), t[2] as u32);
    for i in 0..3 {
        out.r[i][0] = rd(c0[i]) as i16;
        out.r[i][1] = rd(c1[i]) as i16;
        out.r[i][2] = rd(c2[i]) as i16;
        out.t[i] = rd(ct[i]) + p.t[i];
    }
}

#[inline(always)]
fn load_rot(_r: &[[i16; 3]; 3]) {
    #[cfg(target_arch = "mips")]
    psx_gte::scene::load_rotation(&psx_gte::math::Mat3I16 { m: *_r });
}

/// A local rotation folded into one bone before its children compose: the
/// GoldSrc mouth controller, which adds to one Euler angle of the jaw bone.
/// For a Z-rotation controller that is `delta * local` (`post == false`); for
/// an X-rotation controller it is `local * delta` (`post == true`).
#[derive(Clone, Copy)]
pub struct Jaw {
    /// HMA1 bone index, or `usize::MAX` for none.
    pub bone: usize,
    pub post: bool,
    /// Q12 quaternion (x, y, z, w); need not be unit length (the decoder
    /// renormalises the product).
    pub q: [i32; 4],
}

impl Jaw {
    pub const NONE: Jaw = Jaw {
        bone: usize::MAX,
        post: false,
        q: [0, 0, 0, 4096],
    };

    /// The controller opened `amount`/64 of the way from closed to `full`
    /// (a Q12 quaternion): a linear blend from identity, renormalised by the
    /// decoder, exact in direction and close in angle for mouth-sized turns.
    pub fn open(bone: usize, post: bool, full: [i16; 4], amount: u8) -> Jaw {
        if amount == 0 {
            return Jaw::NONE;
        }
        let a = amount.min(64) as i32;
        let id = [0, 0, 0, 4096];
        let mut q = [0i32; 4];
        for k in 0..4 {
            q[k] = id[k] + (((full[k] as i32 - id[k]) * a) >> 6);
        }
        Jaw { bone, post, q }
    }
}

/// Q12 quaternion product a * b.
#[inline(always)]
pub fn quat_mul(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    let r = |v: i32| (v + 2048) >> 12;
    [
        r(aw * bx + ax * bw + ay * bz - az * by),
        r(aw * by - ax * bz + ay * bw + az * bx),
        r(aw * bz + ax * by - ay * bx + az * bw),
        r(aw * bw - ax * bx - ay * by - az * bz),
    ]
}

/// A validated HMA1 blob.
///
/// The decoder reads keys without bounds checks, so the only way to build
/// one is [`Model::new`], which walks every clip and bone once and proves
/// that no playback position can read outside the blob. The fields are
/// private so safe code cannot widen what was proven.
#[derive(Clone, Copy)]
pub struct Model {
    d: *const u8,
    n_bones: usize,
    n_clips: usize,
    bind_off: usize,
    clips_off: usize,
}

/// Fixed layout of a blob: `(n_bones, n_clips, bind_off, clips_off)`, when
/// the header, parents, bind translations and clip offset table fit `d`.
fn layout(d: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let n_bones = u16::from_le_bytes([*d.first()?, *d.get(1)?]) as usize;
    let n_clips = u16::from_le_bytes([*d.get(2)?, *d.get(3)?]) as usize;
    let bind_off = (4 + n_bones + 1) & !1;
    let clips_off = (bind_off + n_bones * 6 + 3) & !3;
    (clips_off + n_clips * 4 <= d.len()).then_some((n_bones, n_clips, bind_off, clips_off))
}

/// One past the last byte a keyed track can read: keys `i` and `i + 1` of
/// `key_bytes` each from `keys`, `read` bytes per key load, where `i` is at
/// most `max(segments, 1) - 1` (see `seg`).
fn track_end(keys: usize, segments: usize, key_bytes: usize, read: usize) -> usize {
    keys.saturating_add(segments.max(1).saturating_mul(key_bytes))
        .saturating_add(read)
}

/// Walk every clip exactly as `decode_with` does and check each read against
/// `d`, for every playback position at once.
fn tracks_fit(d: &[u8], n_bones: usize, n_clips: usize, clips_off: usize) -> bool {
    let len = d.len();
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    for clip in 0..n_clips {
        let o = clips_off + clip * 4;
        let off = u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize;
        let modes = off.saturating_add(CLIP_HEADER);
        if modes.saturating_add(n_bones) > len {
            return false;
        }
        let segments = |rate: usize| u16_at(off + 4 + rate * 2);
        let mut p = (modes + n_bones + 1) & !1;
        for b in 0..n_bones {
            let mode = d[modes + b] as usize;
            let (rc, pc, hi) = (mode & 7, (mode >> 3) & 7, mode & 0x40 != 0);
            // Rotation.
            let end = if rc == 7 {
                p = p.saturating_add(if hi { 6 } else { 4 });
                p
            } else if !hi {
                if p.saturating_add(6) > len {
                    return false;
                }
                let kb = d[p + 4] as usize;
                // 8-bit keys pack 4, 6 or 8 bits per component (2..=4 bytes);
                // wider would overflow the decoder's 32-bit field shifts.
                if kb > 4 {
                    return false;
                }
                let ks = p + 6;
                p = ks.saturating_add((segments(rc) + 1).saturating_mul(kb));
                track_end(ks, segments(rc), kb, 4)
            } else {
                let keys = p;
                p = p.saturating_add((segments(rc) + 1).saturating_mul(6));
                track_end(keys, segments(rc), 6, 6)
            };
            if end > len {
                return false;
            }
            // Translation (pc == 7 reads the bind table `layout` checked).
            let end = if pc == 7 {
                0
            } else if pc == 6 {
                p = p.saturating_add(6);
                p
            } else {
                if p.saturating_add(7) > len {
                    return false;
                }
                let kb = d[p + 6] as usize;
                let ks = p + 8;
                p = ks.saturating_add((segments(pc) + 1).saturating_mul(kb));
                if kb == 6 {
                    track_end(ks, segments(pc), 6, 6)
                } else {
                    track_end(ks, segments(pc), kb, 4)
                }
            };
            if end > len {
                return false;
            }
        }
    }
    true
}

/// [`layout`] of a blob that passes every check [`Model::new`] makes, for
/// callers that only validate and may not hold `d` for `'static`.
pub(crate) fn validated_layout(d: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let (n_bones, n_clips, bind_off, clips_off) = layout(d)?;
    if n_clips == 0 {
        return None;
    }
    for b in 0..n_bones {
        let parent = d[4 + b] as usize;
        if parent != 0xff && parent >= b {
            return None;
        }
    }
    if !tracks_fit(d, n_bones, n_clips, clips_off) {
        return None;
    }
    Some((n_bones, n_clips, bind_off, clips_off))
}

impl Model {
    /// Validate and wrap an HMA1 blob. `None` when any clip, at any playback
    /// position, would read outside `d`, when a bone's parent does not come
    /// before it (the decoder composes parents first), or when there are no
    /// clips.
    pub fn new(d: &'static [u8]) -> Option<Model> {
        let (n_bones, n_clips, bind_off, clips_off) = validated_layout(d)?;
        Some(Model {
            d: d.as_ptr(),
            n_bones,
            n_clips,
            bind_off,
            clips_off,
        })
    }

    /// [`Model::new`] without the walk, for a blob that already passed it.
    ///
    /// # Safety
    /// `Model::new(d)` must return `Some`.
    #[inline]
    pub(crate) unsafe fn new_unchecked(d: &'static [u8]) -> Model {
        // `new` returned Some for these bytes, so `layout` does too.
        let (n_bones, n_clips, bind_off, clips_off) = layout(d).unwrap_or((0, 1, 0, 0));
        Model {
            d: d.as_ptr(),
            n_bones,
            n_clips,
            bind_off,
            clips_off,
        }
    }

    /// Number of bones; `decode` needs this many `Affine` slots.
    #[inline]
    pub fn bone_count(&self) -> usize {
        self.n_bones
    }

    /// Number of clips in the blob (at least one).
    #[inline]
    pub fn clip_count(&self) -> usize {
        self.n_clips
    }

    /// Renamed to [`Model::bone_count`].
    #[deprecated(note = "renamed to `bone_count`")]
    #[inline(always)]
    pub fn n_bones(&self) -> usize {
        self.bone_count()
    }

    /// Renamed to [`Model::clip_count`].
    #[deprecated(note = "renamed to `clip_count`")]
    #[inline(always)]
    pub fn n_clips(&self) -> usize {
        self.clip_count()
    }

    /// Byte offset of `clip`'s record, clamped to the last clip.
    #[inline(always)]
    fn clip_off(&self, clip: usize) -> usize {
        let clip = clip.min(self.n_clips - 1);
        // SAFETY: `layout` checked the clip offset table, and `clip` is below
        // `n_clips`; the load is unaligned.
        unsafe { ptr::read_unaligned(self.d.add(self.clips_off + clip * 4).cast::<u32>()) as usize }
    }

    /// Source frame intervals of `clip` (numframes - 1, at least 1): the
    /// clip's position runs 0..=`clip_intervals * 256`. Clips past the last
    /// read the last.
    pub fn clip_intervals(&self, clip: usize) -> u32 {
        // SAFETY: `tracks_fit` checked every clip's header is in the blob.
        unsafe { u16_at(self.d, self.clip_off(clip)).max(1) }
    }

    /// Decode every bone of `clip` at `pos_q8` into model-space affines.
    /// Clips past the last decode the last; an `out` shorter than
    /// [`Model::bone_count`] is left untouched.
    #[inline(always)]
    pub fn decode(&self, clip: usize, pos_q8: u32, out: &mut [Affine]) {
        self.decode_with(clip, pos_q8, &Jaw::NONE, out)
    }

    /// [`Model::decode`] with a local controller rotation on one bone.
    /// `out` must hold at least `n_bones` entries.
    #[inline(never)]
    pub fn decode_with(&self, clip: usize, pos_q8: u32, jaw: &Jaw, out: &mut [Affine]) {
        let off = self.clip_off(clip);
        // SAFETY: `Model::new` ran `tracks_fit`, which follows this same walk
        // for every clip and bounds each read below for any `pos_q8` (`seg`
        // clamps the key index to the segment count it checked). Parents
        // precede children (checked by `new`) and `out` holds `nb` bones
        // (checked below), so the unchecked `out` accesses are in bounds.
        unsafe {
            let d = self.d;
            let nb = self.n_bones;
            if out.len() < nb {
                return;
            }
            let modes = off + CLIP_HEADER;
            let mut p = (modes + nb + 1) & !1;
            let mut loaded = usize::MAX;
            for b in 0..nb {
                let mode = u8_at(d, modes + b);
                let rc = (mode & 7) as usize;
                let pc = ((mode >> 3) & 7) as usize;
                let hi = mode & 0x40 != 0;
                let q = if rc == 7 {
                    if hi {
                        let q = read12(d, p);
                        p += 6;
                        q
                    } else {
                        let q = read8(d, p);
                        p += 4;
                        q
                    }
                } else if !hi {
                    let (i, f, nk) = seg(d, off, rc, pos_q8);
                    let kb = u8_at(d, p + 4) as usize;
                    let w = (kb as u32) << 1;
                    let mask = (1u32 << w) - 1;
                    let (b0, b1, b2, b3) = (
                        i8_at(d, p),
                        i8_at(d, p + 1),
                        i8_at(d, p + 2),
                        i8_at(d, p + 3),
                    );
                    let ks = p + 6;
                    let wa = u32_unaligned(d, ks + i * kb);
                    let wb = u32_unaligned(d, ks + (i + 1) * kb);
                    let ex = |word: u32| -> [i32; 4] {
                        [
                            (b0 + (word & mask) as i32) << 5,
                            (b1 + ((word >> w) & mask) as i32) << 5,
                            (b2 + ((word >> (2 * w)) & mask) as i32) << 5,
                            (b3 + ((word >> (3 * w)) & mask) as i32) << 5,
                        ]
                    };
                    p = ks + nk * kb;
                    lerp4(ex(wa), ex(wb), f)
                } else {
                    let (i, f, nk) = seg(d, off, rc, pos_q8);
                    let q = lerp4(read12(d, p + i * 6), read12(d, p + i * 6 + 6), f);
                    p += nk * 6;
                    q
                };
                let t = if pc == 7 {
                    let o = self.bind_off + b * 6;
                    [i16_at(d, o), i16_at(d, o + 2), i16_at(d, o + 4)]
                } else if pc == 6 {
                    let v = [i16_at(d, p), i16_at(d, p + 2), i16_at(d, p + 4)];
                    p += 6;
                    v
                } else {
                    let base = [i16_at(d, p), i16_at(d, p + 2), i16_at(d, p + 4)];
                    let kb = u8_at(d, p + 6) as usize;
                    let ks = p + 8;
                    let (i, f, nk) = seg(d, off, pc, pos_q8);
                    let v = if kb == 6 {
                        let a = [
                            i16_at(d, ks + i * 6) & 0xffff,
                            i16_at(d, ks + i * 6 + 2) & 0xffff,
                            i16_at(d, ks + i * 6 + 4) & 0xffff,
                        ];
                        let c = [
                            i16_at(d, ks + i * 6 + 6) & 0xffff,
                            i16_at(d, ks + i * 6 + 8) & 0xffff,
                            i16_at(d, ks + i * 6 + 10) & 0xffff,
                        ];
                        [
                            base[0] + a[0] + (((c[0] - a[0]) * f) >> 8),
                            base[1] + a[1] + (((c[1] - a[1]) * f) >> 8),
                            base[2] + a[2] + (((c[2] - a[2]) * f) >> 8),
                        ]
                    } else {
                        let w: u32 = if kb == 2 {
                            5
                        } else if kb == 3 {
                            8
                        } else {
                            10
                        };
                        let mask = (1u32 << w) - 1;
                        let wa = u32_unaligned(d, ks + i * kb);
                        let wb = u32_unaligned(d, ks + (i + 1) * kb);
                        let ex = |word: u32| {
                            [
                                (word & mask) as i32,
                                ((word >> w) & mask) as i32,
                                ((word >> (2 * w)) & mask) as i32,
                            ]
                        };
                        let (a, c) = (ex(wa), ex(wb));
                        [
                            base[0] + a[0] + (((c[0] - a[0]) * f) >> 8),
                            base[1] + a[1] + (((c[1] - a[1]) * f) >> 8),
                            base[2] + a[2] + (((c[2] - a[2]) * f) >> 8),
                        ]
                    };
                    p = ks + nk * kb;
                    v
                };
                let q = if b == jaw.bone {
                    if jaw.post {
                        quat_mul(q, jaw.q)
                    } else {
                        quat_mul(jaw.q, q)
                    }
                } else {
                    q
                };
                let r = quat_to_mat(q);
                let parent = u8_at(d, 4 + b) as usize;
                // Parents precede children (checked when the HMD8 loads) and
                // `out` holds `nb` bones (checked above).
                if parent == 0xff {
                    *out.get_unchecked_mut(b) = Affine { r, _pad: 0, t };
                } else {
                    let pa = *out.get_unchecked(parent);
                    if loaded != parent {
                        load_rot(&pa.r);
                        loaded = parent;
                    }
                    compose_loaded(&pa, &r, t, out.get_unchecked_mut(b));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f64::consts::FRAC_1_SQRT_2;

    /// Q12 rotation matrix of a unit quaternion given as floats (the
    /// standard formula, in f64, normalised first).
    fn reference(q: [f64; 4]) -> [[f64; 3]; 3] {
        let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        let [x, y, z, w] = [q[0] / n, q[1] / n, q[2] / n, q[3] / n];
        [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - w * z),
                2.0 * (x * z + w * y),
            ],
            [
                2.0 * (x * y + w * z),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - w * x),
            ],
            [
                2.0 * (x * z - w * y),
                2.0 * (y * z + w * x),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ]
    }

    #[test]
    fn quat_to_mat_matches_the_float_formula_for_near_unit_input() {
        // Rotations about each axis and a diagonal one, with the quaternion
        // scaled up to 5% off unit length, which the renormalise step absorbs.
        let axes: [[f64; 4]; 5] = [
            [0.0, 0.0, 0.0, 1.0],
            [0.5, 0.5, 0.5, 0.5],
            [0.0, 0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2],
            [0.3, -0.5, 0.2, 0.78],
            [-0.6, 0.1, 0.0, 0.79],
        ];
        for axis in axes {
            for scale in [0.95, 1.0, 1.05] {
                let q = axis.map(|c| (c * 4096.0 * scale) as i32);
                let got = quat_to_mat(q);
                let want = reference(axis);
                for r in 0..3 {
                    for c in 0..3 {
                        let expected = (want[r][c] * 4096.0).round() as i32;
                        let diff = (got[r][c] as i32 - expected).abs();
                        assert!(diff <= 24, "{axis:?} x{scale} [{r}][{c}] off by {diff}");
                    }
                }
            }
        }
    }

    #[test]
    fn quat_to_mat_survives_the_largest_components_a_key_can_hold() {
        // An 8-bit key is an i8 base plus an 8-bit offset, shifted left by 5:
        // components from -4096 to 12224. Their squared length is far from
        // 4096, and `3 * e * e` in i32 overflowed (a panic in debug, garbage
        // in release) once the length error passed about 26,000.
        let extremes = [-4096, 0, 4096, 12224];
        for x in extremes {
            for y in extremes {
                for z in extremes {
                    for w in extremes {
                        let m = quat_to_mat([x, y, z, w]);
                        // Garbage rotation is allowed; unbounded work is not.
                        let _ = m;
                    }
                }
            }
        }
    }
}
