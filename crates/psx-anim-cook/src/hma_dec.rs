//! HMA1 reference decoder (host). The PS1 decoder is psx_asset::hma1;
//! both must stay bit-identical (see tests).
//!
//! HMA1 decoder: local-space quaternion tracks with per-bone key rates,
//! evaluated parents-first into model-space affines. Integer only, `core`
//! only, so the identical source runs on the host lab and on the PS1.
//!
//! Layout (little endian, every block 2-byte aligned):
//!
//!   model:  u16 n_bones, u16 n_clips, u8 parent[n_bones] (0xff root), pad,
//!           i16 bind_t[n_bones][3], u32 clip_off[n_clips] (from model start)
//!   clip:   u16 n_int, u8 flags (bit0 loop), u8 qfmt (0 i8x4, 1 12-bit x4, 2 i16x4),
//!           u16 seg[7], u16 factor_q15[7], u8 mode[n_bones], pad,
//!           then per bone: rotation keys, then translation keys (i16x3)
//!   mode:   bits0-2 rotation rate (0..6 keyed, 7 = one key),
//!           bits3-5 translation (0..5 keyed at that rate, 6 = one key, 7 = bind_t)
//!
//! Rate r has seg[r] segments over the clip and seg[r]+1 keys. The caller
//! passes the clip position in source frames * 256; per clip the decoder turns
//! it into a Q8 segment position for each rate with one multiply each.

/// Translation fraction bits: local and accumulated translations carry
/// 1/4 cooked unit so hierarchy rounding does not pile up along chains.
pub const TFRAC: u32 = 2;
pub const RATE_CONST: u8 = 7;
pub const POS_CLIP_CONST: u8 = 6;
pub const POS_BIND: u8 = 7;
pub const N_RATES: usize = 7;
pub const CLIP_HEADER: usize = 4 + 2 * N_RATES * 2;

#[inline(always)]
fn rd_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
#[inline(always)]
fn rd_i16(d: &[u8], o: usize) -> i16 {
    rd_u16(d, o) as i16
}
#[inline(always)]
fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

pub const fn quat_bytes(qfmt: u8) -> usize {
    match qfmt {
        0 => 4,
        1 => 6,
        _ => 8,
    }
}

/// Key count for a rotation / translation code.
#[inline(always)]
pub fn key_count(seg: &[u16; N_RATES], code: u8) -> usize {
    if (code as usize) < N_RATES {
        seg[code as usize] as usize + 1
    } else {
        1
    }
}

#[inline(always)]
pub fn read_quat(d: &[u8], o: usize, qfmt: u8) -> [i32; 4] {
    match qfmt {
        0 => [
            (d[o] as i8 as i32) << 5,
            (d[o + 1] as i8 as i32) << 5,
            (d[o + 2] as i8 as i32) << 5,
            (d[o + 3] as i8 as i32) << 5,
        ],
        1 => {
            let w0 = rd_u16(d, o) as u32;
            let w1 = rd_u16(d, o + 2) as u32;
            let w2 = rd_u16(d, o + 4) as u32;
            let packed = w0 | (w1 << 16);
            let x = packed & 0xfff;
            let y = (packed >> 12) & 0xfff;
            let z = (packed >> 24) | ((w2 & 0xf) << 8);
            let w = w2 >> 4;
            let s = |v: u32| (((v << 20) as i32) >> 20) << 1;
            [s(x), s(y), s(z), s(w)]
        }
        _ => [
            rd_i16(d, o) as i32,
            rd_i16(d, o + 2) as i32,
            rd_i16(d, o + 4) as i32,
            rd_i16(d, o + 6) as i32,
        ],
    }
}

/// Catmull-Rom weights (Q12) for a Q8 fraction (0..=256).
#[inline(always)]
pub fn cr_weights(f: i32) -> [i32; 4] {
    let t = f << 4; // Q12
    let t2 = (t * t) >> 12;
    let t3 = (t2 * t) >> 12;
    [
        (-t3 + 2 * t2 - t) >> 1,
        (3 * t3 - 5 * t2 + 8192) >> 1,
        (-3 * t3 + 4 * t2 + t) >> 1,
        (t3 - t2) >> 1,
    ]
}

#[inline(always)]
pub fn cr4(k: [[i32; 4]; 4], w: &[i32; 4]) -> [i32; 4] {
    let mut o = [0i32; 4];
    for c in 0..4 {
        o[c] = (k[0][c] * w[0] + k[1][c] * w[1] + k[2][c] * w[2] + k[3][c] * w[3] + 2048) >> 12;
    }
    o
}

/// Q12 quaternion product a * b (bit-identical to `psx_asset::hma1::quat_mul`).
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

/// A decoded bone: Q12 rotation and translation in cooked units << TFRAC.
#[derive(Clone, Copy, Default, Debug)]
pub struct Affine {
    pub r: [[i16; 3]; 3],
    pub t: [i32; 3],
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        r: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
        t: [0; 3],
    };
}

/// Unit-length correction then quaternion -> Q12 matrix. `normalize`
/// applies the two-term 1/sqrt(n) series (exact enough for |n-1| < 0.1).
#[inline(always)]
pub fn quat_to_mat(q: [i32; 4], normalize: bool) -> [[i16; 3]; 3] {
    let [mut x, mut y, mut z, mut w] = q;
    if normalize {
        let n = (x * x + y * y + z * z + w * w + 2048) >> 12;
        let e = 4096 - n;
        let r = 4096 + (e >> 1) + ((3 * e * e) >> 15);
        x = (x * r + 2048) >> 12;
        y = (y * r + 2048) >> 12;
        z = (z * r + 2048) >> 12;
        w = (w * r + 2048) >> 12;
    }
    // 2*a*b in Q12 is one rounded shift by 11: no doubled truncation bias.
    let m2 = |a: i32, b: i32| (a * b + 1024) >> 11;
    let (xx, yy, zz) = (m2(x, x), m2(y, y), m2(z, z));
    let (xy, xz, yz) = (m2(x, y), m2(x, z), m2(y, z));
    let (wx, wy, wz) = (m2(w, x), m2(w, y), m2(w, z));
    let c = |v: i32| v.clamp(-32768, 32767) as i16;
    [
        [c(4096 - (yy + zz)), c(xy - wz), c(xz + wy)],
        [c(xy + wz), c(4096 - (xx + zz)), c(yz - wx)],
        [c(xz - wy), c(yz + wx), c(4096 - (xx + yy))],
    ]
}

/// parent * local. GTE MVMVA with sf=0 leaves the unshifted sums in MAC1-3;
/// the CPU rounds them, so no truncation bias accumulates down a chain.
#[inline(always)]
pub fn compose(p: &Affine, r: &[[i16; 3]; 3], t: [i32; 3]) -> Affine {
    let mut o = Affine::default();
    for i in 0..3 {
        for j in 0..3 {
            let s = p.r[i][0] as i32 * r[0][j] as i32
                + p.r[i][1] as i32 * r[1][j] as i32
                + p.r[i][2] as i32 * r[2][j] as i32;
            o.r[i][j] = ((s + 2048) >> 12).clamp(-32768, 32767) as i16;
        }
        let s = p.r[i][0] as i32 * t[0] + p.r[i][1] as i32 * t[1] + p.r[i][2] as i32 * t[2];
        o.t[i] = ((s + 2048) >> 12) + p.t[i];
    }
    o
}

pub struct ClipView<'a> {
    pub d: &'a [u8],
    pub off: usize,
    pub n_int: u16,
    pub looping: bool,
    pub qfmt: u8,
    pub seg: [u16; N_RATES],
    pub factor: [u16; N_RATES],
}

pub struct ModelView<'a> {
    pub d: &'a [u8],
    pub n_bones: usize,
    pub n_clips: usize,
    pub bind_off: usize,
    pub clips_off: usize,
}

impl<'a> ModelView<'a> {
    pub fn new(d: &'a [u8]) -> ModelView<'a> {
        let n_bones = rd_u16(d, 0) as usize;
        let n_clips = rd_u16(d, 2) as usize;
        let bind_off = (4 + n_bones + 1) & !1;
        let clips_off = (bind_off + n_bones * 6 + 3) & !3;
        ModelView {
            d,
            n_bones,
            n_clips,
            bind_off,
            clips_off,
        }
    }
    #[inline(always)]
    pub fn parent(&self, b: usize) -> u8 {
        self.d[4 + b]
    }
    pub fn clip(&self, c: usize) -> ClipView<'a> {
        let off = rd_u32(self.d, self.clips_off + c * 4) as usize;
        let d = self.d;
        let mut seg = [0u16; N_RATES];
        let mut factor = [0u16; N_RATES];
        for r in 0..N_RATES {
            seg[r] = rd_u16(d, off + 4 + r * 2);
            factor[r] = rd_u16(d, off + 4 + N_RATES * 2 + r * 2);
        }
        ClipView {
            d,
            off,
            n_int: rd_u16(d, off),
            looping: d[off + 2] & 1 != 0,
            qfmt: d[off + 3],
            seg,
            factor,
        }
    }

    /// Decode every bone of `clip` at source position `pos_q8` (frames*256)
    /// into model-space affines, parents first. `out.len() >= n_bones`.
    pub fn decode(&self, clip: &ClipView, pos_q8: u32, normalize: bool, out: &mut [Affine]) {
        self.decode_with(clip, pos_q8, normalize, None, out)
    }

    /// [`ModelView::decode`] with a jaw controller: `(bone, post, q)` folds
    /// the Q12 quaternion `q` into that bone's local rotation (`q * local`,
    /// or `local * q` when `post`), exactly as `psx_asset::hma1` does.
    pub fn decode_with(
        &self,
        clip: &ClipView,
        pos_q8: u32,
        normalize: bool,
        jaw: Option<(usize, bool, [i32; 4])>,
        out: &mut [Affine],
    ) {
        let d = self.d;
        let cubic = d[clip.off + 2] & 2 != 0;
        // Per-clip segment positions: one multiply per rate, not per bone.
        let mut segpos = [(0usize, 0i32); N_RATES];
        let mut weights = [[0i32; 4]; N_RATES];
        for r in 0..N_RATES {
            let s = clip.seg[r] as u32;
            let sp = (pos_q8 * clip.factor[r] as u32) >> 15;
            let (mut i, mut f) = ((sp >> 8) as usize, (sp & 255) as i32);
            if i >= s as usize {
                i = (s as usize).saturating_sub(1);
                f = 256;
            }
            segpos[r] = (i, f);
            if cubic {
                weights[r] = cr_weights(f);
            }
        }
        let nb = self.n_bones;
        let modes = clip.off + CLIP_HEADER;
        let mut p = (modes + nb + 1) & !1;
        let qb = quat_bytes(clip.qfmt.min(2));
        let rd3 = |o: usize| {
            [
                rd_i16(d, o) as i32,
                rd_i16(d, o + 2) as i32,
                rd_i16(d, o + 4) as i32,
            ]
        };
        for b in 0..nb {
            let mode = d[modes + b];
            let rc = mode & 7;
            let pc = (mode >> 3) & 7;
            let mixed = clip.qfmt == 3;
            let (kfmt, kqb) = if mixed {
                if mode & 0x40 != 0 {
                    (1u8, 6usize)
                } else {
                    (0u8, 4usize)
                }
            } else {
                (clip.qfmt, qb)
            };
            // rotation
            let q = if rc == RATE_CONST {
                let q = read_quat(d, p, kfmt);
                p += kqb;
                q
            } else if mixed && kfmt == 0 {
                // range-reduced 8-bit keys: 4 x i8 base, key bytes, pad, keys
                let kb = d[p + 4] as usize;
                let w = (kb as u32) * 2;
                let mask = (1u32 << w) - 1;
                let base = [
                    (d[p] as i8 as i32),
                    (d[p + 1] as i8 as i32),
                    (d[p + 2] as i8 as i32),
                    (d[p + 3] as i8 as i32),
                ];
                let ks = p + 6;
                let (i, f) = segpos[rc as usize];
                let key = |k: usize| -> [i32; 4] {
                    let o = ks + k * kb;
                    let word = d[o] as u32
                        | (d[o + 1] as u32) << 8
                        | if kb > 2 { (d[o + 2] as u32) << 16 } else { 0 }
                        | if kb > 3 { (d[o + 3] as u32) << 24 } else { 0 };
                    [
                        (base[0] + (word & mask) as i32) << 5,
                        (base[1] + ((word >> w) & mask) as i32) << 5,
                        (base[2] + ((word >> (2 * w)) & mask) as i32) << 5,
                        (base[3] + ((word >> (3 * w)) & mask) as i32) << 5,
                    ]
                };
                let q = if cubic {
                    let last = clip.seg[rc as usize] as usize;
                    cr4(
                        [
                            key(i.saturating_sub(1)),
                            key(i),
                            key(i + 1),
                            key((i + 2).min(last)),
                        ],
                        &weights[rc as usize],
                    )
                } else {
                    let (a, bq) = (key(i), key(i + 1));
                    [
                        a[0] + (((bq[0] - a[0]) * f) >> 8),
                        a[1] + (((bq[1] - a[1]) * f) >> 8),
                        a[2] + (((bq[2] - a[2]) * f) >> 8),
                        a[3] + (((bq[3] - a[3]) * f) >> 8),
                    ]
                };
                p = ks + key_count(&clip.seg, rc) * kb;
                q
            } else {
                let (i, f) = segpos[rc as usize];
                let q = if cubic {
                    let last = clip.seg[rc as usize] as usize;
                    let at = |k: usize| read_quat(d, p + k * kqb, kfmt);
                    cr4(
                        [
                            at(i.saturating_sub(1)),
                            at(i),
                            at(i + 1),
                            at((i + 2).min(last)),
                        ],
                        &weights[rc as usize],
                    )
                } else {
                    let a = read_quat(d, p + i * kqb, kfmt);
                    let bq = read_quat(d, p + (i + 1) * kqb, kfmt);
                    [
                        a[0] + (((bq[0] - a[0]) * f) >> 8),
                        a[1] + (((bq[1] - a[1]) * f) >> 8),
                        a[2] + (((bq[2] - a[2]) * f) >> 8),
                        a[3] + (((bq[3] - a[3]) * f) >> 8),
                    ]
                };
                p += key_count(&clip.seg, rc) * kqb;
                q
            };
            // translation
            let t = if pc == POS_BIND {
                rd3(self.bind_off + b * 6)
            } else if pc == POS_CLIP_CONST {
                let v = rd3(p);
                p += 6;
                v
            } else {
                let (i, f) = segpos[pc as usize];
                let nk = key_count(&clip.seg, pc);
                let (ks, kb, key): (usize, usize, &dyn Fn(usize) -> [i32; 4]);
                let base = rd3(p);
                let rkb = d[p + 6] as usize;
                let rr = |k: usize| -> [i32; 4] {
                    let o = p + 8 + k * rkb;
                    let mut word = 0u64;
                    for byte in 0..rkb {
                        word |= (d[o + byte] as u64) << (8 * byte);
                    }
                    let w: u32 = match rkb {
                        2 => 5,
                        3 => 8,
                        4 => 10,
                        _ => 16,
                    };
                    let mask = (1u64 << w) - 1;
                    [
                        base[0] + (word & mask) as i32,
                        base[1] + ((word >> w) & mask) as i32,
                        base[2] + ((word >> (2 * w)) & mask) as i32,
                        0,
                    ]
                };
                let raw = |k: usize| -> [i32; 4] {
                    let v = rd3(p + k * 6);
                    [v[0], v[1], v[2], 0]
                };
                if mixed {
                    ks = p + 8;
                    kb = rkb;
                    key = &rr;
                } else {
                    ks = p;
                    kb = 6;
                    key = &raw;
                }
                let v = if cubic {
                    let last = clip.seg[pc as usize] as usize;
                    let r = cr4(
                        [
                            key(i.saturating_sub(1)),
                            key(i),
                            key(i + 1),
                            key((i + 2).min(last)),
                        ],
                        &weights[pc as usize],
                    );
                    [r[0], r[1], r[2]]
                } else {
                    let (a, c) = (key(i), key(i + 1));
                    [
                        a[0] + (((c[0] - a[0]) * f) >> 8),
                        a[1] + (((c[1] - a[1]) * f) >> 8),
                        a[2] + (((c[2] - a[2]) * f) >> 8),
                    ]
                };
                p = ks + nk * kb;
                v
            };
            let q = match jaw {
                Some((jb, post, jq)) if jb == b => {
                    if post {
                        quat_mul(q, jq)
                    } else {
                        quat_mul(jq, q)
                    }
                }
                _ => q,
            };
            let r = quat_to_mat(q, normalize);
            let parent = self.parent(b);
            out[b] = if parent == 0xff {
                Affine { r, t }
            } else {
                let pa = out[parent as usize];
                compose(&pa, &r, t)
            };
        }
    }
}
