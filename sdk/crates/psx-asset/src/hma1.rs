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

pub const N_RATES: usize = 7;
const CLIP_HEADER: usize = 4 + 2 * N_RATES * 2;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Aff {
    pub r: [[i16; 3]; 3],
    pub _pad: i16,
    pub t: [i32; 3],
}

impl Aff {
    pub const ZERO: Aff = Aff {
        r: [[0; 3]; 3],
        _pad: 0,
        t: [0; 3],
    };
}

#[inline(always)]
unsafe fn u8_at(d: *const u8, o: usize) -> u32 {
    unsafe { *d.add(o) as u32 }
}
#[inline(always)]
unsafe fn i8_at(d: *const u8, o: usize) -> i32 {
    unsafe { *d.add(o) as i8 as i32 }
}
#[inline(always)]
unsafe fn u16_at(d: *const u8, o: usize) -> u32 {
    // track data is byte-packed: odd offsets are legal
    unsafe { *d.add(o) as u32 | (*d.add(o + 1) as u32) << 8 }
}
#[inline(always)]
unsafe fn i16_at(d: *const u8, o: usize) -> i32 {
    unsafe { (*d.add(o) as u32 | (*d.add(o + 1) as u32) << 8) as i16 as i32 }
}
#[inline(always)]
unsafe fn u32_unaligned(d: *const u8, o: usize) -> u32 {
    unsafe { ptr::read_unaligned(d.add(o).cast::<u32>()) }
}

#[inline(always)]
unsafe fn read8(d: *const u8, o: usize) -> [i32; 4] {
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
    let e = 4096 - n;
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
fn compose_loaded(p: &Aff, r: &[[i16; 3]; 3], t: [i32; 3], out: &mut Aff) {
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

#[derive(Clone, Copy)]
pub struct Model {
    d: *const u8,
    pub n_bones: usize,
    pub bind_off: usize,
    pub clips_off: usize,
}

impl Model {
    /// `d` must stay alive while the model is used and carry 4 readable
    /// bytes past the last key (the cooker pads the section).
    pub fn new(d: &'static [u8]) -> Model {
        let n_bones = u16::from_le_bytes([d[0], d[1]]) as usize;
        let bind_off = (4 + n_bones + 1) & !1;
        let clips_off = (bind_off + n_bones * 6 + 3) & !3;
        Model {
            d: d.as_ptr(),
            n_bones,
            bind_off,
            clips_off,
        }
    }

    /// Number of clips in the blob.
    pub fn n_clips(&self) -> usize {
        unsafe { u16_at(self.d, 2) as usize }
    }

    /// Source frame intervals of `clip` (numframes - 1, at least 1): the
    /// clip's position runs 0..=`clip_intervals * 256`.
    pub fn clip_intervals(&self, clip: usize) -> u32 {
        unsafe {
            let off =
                ptr::read_unaligned(self.d.add(self.clips_off + clip * 4).cast::<u32>()) as usize;
            u16_at(self.d, off).max(1)
        }
    }

    /// Decode every bone of `clip` at `pos_q8` into model-space affines.
    #[inline(always)]
    pub fn decode(&self, clip: usize, pos_q8: u32, out: &mut [Aff]) {
        self.decode_with(clip, pos_q8, &Jaw::NONE, out)
    }

    /// [`Model::decode`] with a local controller rotation on one bone.
    /// `out` must hold at least `n_bones` entries.
    #[inline(never)]
    pub fn decode_with(&self, clip: usize, pos_q8: u32, jaw: &Jaw, out: &mut [Aff]) {
        unsafe {
            let d = self.d;
            let off = ptr::read_unaligned(d.add(self.clips_off + clip * 4).cast::<u32>()) as usize;
            let mut segi = [0usize; N_RATES];
            let mut segf = [0i32; N_RATES];
            let mut nkeys = [0usize; N_RATES];
            for r in 0..N_RATES {
                let s = u16_at(d, off + 4 + r * 2);
                let factor = u16_at(d, off + 4 + N_RATES * 2 + r * 2);
                let sp = (pos_q8 * factor) >> 15;
                let (mut i, mut f) = ((sp >> 8) as usize, (sp & 255) as i32);
                if i >= s as usize {
                    i = (s as usize).saturating_sub(1);
                    f = 256;
                }
                segi[r] = i;
                segf[r] = f;
                nkeys[r] = s as usize + 1;
            }
            let nb = self.n_bones;
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
                    let i = segi[rc];
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
                    p = ks + nkeys[rc] * kb;
                    lerp4(ex(wa), ex(wb), segf[rc])
                } else {
                    let i = segi[rc];
                    let q = lerp4(read12(d, p + i * 6), read12(d, p + i * 6 + 6), segf[rc]);
                    p += nkeys[rc] * 6;
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
                    let i = segi[pc];
                    let f = segf[pc];
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
                    p = ks + nkeys[pc] * kb;
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
                if parent == 0xff {
                    out[b] = Aff { r, _pad: 0, t };
                } else {
                    if loaded != parent {
                        load_rot(&out[parent].r);
                        loaded = parent;
                    }
                    let pa = out[parent];
                    let mut o = Aff::ZERO;
                    compose_loaded(&pa, &r, t, &mut o);
                    out[b] = o;
                }
            }
        }
    }
}
