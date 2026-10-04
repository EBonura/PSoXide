//! Scene / camera / projection helpers.
//!
//! The register macros in [`regs`][crate::regs] are the only thing
//! that actually touches the GTE; everything here is a convenience
//! layer that bundles the ~8 writes a typical 3D frame needs into
//! named functions. All functions are safe -- the macros they wrap
//! already contain the `unsafe { asm! }` internally, and there's
//! nothing we can do with a bad matrix value that would be undefined
//! behaviour (worst case: the projected vertex is garbage).
//!
//! Typical frame:
//!
//! ```ignore
//! scene::set_screen_offset(160 << 16, 120 << 16);
//! scene::set_projection_plane(200);
//! let rot = Mat3I16::rotate_y(angle);
//! scene::load_rotation(&rot);
//! scene::load_translation(Vec3I32::new(0, 0, 0x4000));
//! for v in vertices {
//!     let p = scene::project_vertex(v);
//!     draw_point(p.sx, p.sy);
//! }
//! ```

use crate::math::{Mat3I16, Vec3I16, Vec3I32};
use crate::ops;
use crate::regs::pack_xy;
use crate::{read_control, read_data, write_control, write_data};
#[cfg(target_arch = "mips")]
use core::arch::asm;

/// Result of a single perspective-projected vertex -- screen-space
/// (x, y) in pixels plus the MAC3 depth used for ordering-table
/// inserts. `Projected` is `Copy` + trivially packed so the caller
/// can collect per-vertex results into an array and rasterise later.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Projected {
    /// Screen-space X, clamped to GTE's ±0x400 range.
    pub sx: i16,
    /// Screen-space Y.
    pub sy: i16,
    /// Depth post-divide, 0..0xFFFF after saturation.
    pub sz: u16,
}

/// A direction scaled to Q12 unit length by
/// [`normalize_classic_q12_scheduled`], with the squared lengths it came
/// from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct ClassicNormalizedVector {
    /// The input direction at length 4096 (Q12 1.0); zero when the
    /// squared length is zero or wrapped negative.
    pub vector: Vec3I16,
    /// `x * x + y * y` of the whole-unit components, wrapping.
    pub xy_squared: i32,
    /// `x * x + y * y + z * z` of the whole-unit components, wrapping.
    pub squared: i32,
}

/// floor(sqrt(n)) by bisection, for building tables at compile time.
const fn isqrt_u32(n: u32) -> u32 {
    let (mut lo, mut hi) = (0u32, 1u32 << 16);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if mid * mid <= n {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Q12 reciprocal square roots of the sums 64..=255, scaled so the sum 64
/// maps to 1.0: entry `m - 64` is floor(4096 * sqrt(64 / m)). That equals
/// floor(32768 / sqrt(m)) = floor(sqrt(2^30 / m)), and since flooring the
/// argument first doesn't change an integer square root's floor, it is
/// computed exactly in integers.
const RSQRT_Q12_FROM_64: [i16; 192] = {
    let mut table = [0i16; 192];
    let mut i = 0;
    while i < 192 {
        table[i] = isqrt_u32((1u32 << 30) / (64 + i as u32)) as i16;
        i += 1;
    }
    table
};

/// Scale a Q12 direction to length 4096 with a 192-entry reciprocal square
/// root table, the classic fixed-point way.
///
/// Each component is floored to whole units (`>> 12`) and kept as 16 bits.
/// With `s` the sum of their squares (wrapping `i32`):
///
/// 1. `L` = leading zero bits of `s`, rounded down to even.
/// 2. `s` shifted by `L - 24` lands in 64..=255; the table gives
///    `r` = floor(4096 * sqrt(64 / that)).
/// 3. Each component becomes `(c * r) >> ((31 - L) / 2)`.
///
/// Because `L` is even, the table entry equals 4096 * 2^(15 - L/2) / sqrt(s),
/// and the shift in step 3 removes that power of two exactly, leaving
/// 4096 * c / sqrt(s) up to the table's truncation. A zero or wrapped (negative)
/// `s` gives a zero vector. Pure integer work: no GTE state is used.
pub fn normalize_classic_q12_scheduled(input: Vec3I32) -> ClassicNormalizedVector {
    let c = [
        (input.x >> 12) as i16,
        (input.y >> 12) as i16,
        (input.z >> 12) as i16,
    ];
    let square = |v: i16| i32::from(v) * i32::from(v);
    let xy_squared = square(c[0]).wrapping_add(square(c[1]));
    let squared = xy_squared.wrapping_add(square(c[2]));
    if squared <= 0 {
        return ClassicNormalizedVector {
            vector: Vec3I16::new(0, 0, 0),
            xy_squared,
            squared,
        };
    }
    let even_zeros = (squared as u32).leading_zeros() & !1;
    let in_range = if even_zeros >= 24 {
        squared << (even_zeros - 24)
    } else {
        squared >> (24 - even_zeros)
    };
    let r = i32::from(RSQRT_Q12_FROM_64[(in_range - 64) as usize]);
    let shift = (31 - even_zeros) / 2;
    let scale = |v: i16| ((i32::from(v) * r) >> shift) as i16;
    ClassicNormalizedVector {
        vector: Vec3I16::new(scale(c[0]), scale(c[1]), scale(c[2])),
        xy_squared,
        squared,
    }
}

/// Four-byte-aligned plane record used by the GTE AABB clip batch.
///
/// This layout deliberately matches retained engines that store
/// `(normal, kind, signbits, distance)`. `kind` remains caller-owned, while
/// `signbits` caches the negative-normal mask (`x | y << 1 | z << 2`) used to
/// select AABB support points without rereading the normal components.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct AabbClipPlane {
    /// Signed Q12 plane normal.
    pub normal: [i16; 3],
    /// Caller-owned plane kind or axial classification.
    pub kind: u8,
    /// Negative-normal mask (`x | y << 1 | z << 2`).
    pub signbits: u8,
    /// Q12 plane distance.
    pub distance: i32,
}

/// Load the rotation matrix into the GTE's RT control registers (0..=4).
#[doc(alias = "SetRotMatrix")]
pub fn load_rotation(m: &Mat3I16) {
    write_control!(0, pack_xy(m.m[0][0], m.m[0][1]));
    write_control!(1, pack_xy(m.m[0][2], m.m[1][0]));
    write_control!(2, pack_xy(m.m[1][1], m.m[1][2]));
    write_control!(3, pack_xy(m.m[2][0], m.m[2][1]));
    write_control!(4, m.m[2][2] as i32 as u32);
}

/// Load the light-direction matrix (LLM, control 8..=12).
#[doc(alias = "SetLightMatrix")]
pub fn load_light_matrix(m: &Mat3I16) {
    write_control!(8, pack_xy(m.m[0][0], m.m[0][1]));
    write_control!(9, pack_xy(m.m[0][2], m.m[1][0]));
    write_control!(10, pack_xy(m.m[1][1], m.m[1][2]));
    write_control!(11, pack_xy(m.m[2][0], m.m[2][1]));
    write_control!(12, m.m[2][2] as i32 as u32);
}

/// Load four clip-plane normals for [`classify_aabb_clip4`] and
/// [`is_aabb_outside_clip4`].
///
/// Planes zero through two occupy the rotation-matrix rows. Plane three uses
/// the first light-matrix row. This intentionally replaces both matrices;
/// callers must restore their camera and lighting state before projection or
/// lit geometry submission.
pub fn load_aabb_clip4(planes: &[AabbClipPlane; 4]) {
    load_rotation(&Mat3I16 {
        m: [planes[0].normal, planes[1].normal, planes[2].normal],
    });
    load_light_matrix(&Mat3I16 {
        m: [planes[3].normal, [0; 3], [0; 3]],
    });
}

/// Load the light-color matrix (LCM, control 16..=20).
#[doc(alias = "SetColorMatrix")]
pub fn load_light_color_matrix(m: &Mat3I16) {
    write_control!(16, pack_xy(m.m[0][0], m.m[0][1]));
    write_control!(17, pack_xy(m.m[0][2], m.m[1][0]));
    write_control!(18, pack_xy(m.m[1][1], m.m[1][2]));
    write_control!(19, pack_xy(m.m[2][0], m.m[2][1]));
    write_control!(20, m.m[2][2] as i32 as u32);
}

/// Load the translation vector (TR, control 5..=7).
#[doc(alias = "SetTransMatrix")]
pub fn load_translation(t: Vec3I32) {
    write_control!(5, t.x as u32);
    write_control!(6, t.y as u32);
    write_control!(7, t.z as u32);
}

/// Load the background-color bias (BK, control 13..=15).
#[doc(alias = "SetBackColor")]
pub fn load_background_color(c: Vec3I32) {
    write_control!(13, c.x as u32);
    write_control!(14, c.y as u32);
    write_control!(15, c.z as u32);
}

/// Load the far-color bias (FC, control 21..=23) used by depth-cue
/// interpolation.
#[doc(alias = "SetFarColor")]
pub fn load_far_color(c: Vec3I32) {
    write_control!(21, c.x as u32);
    write_control!(22, c.y as u32);
    write_control!(23, c.z as u32);
}

/// Set OFX and OFY (control 24, 25) -- the screen-space offsets applied
/// post-divide. Values are 15.16 fixed point; `160 << 16` = 160.0 px.
#[doc(alias = "SetGeomOffset")]
pub fn set_screen_offset(ofx_15_16: i32, ofy_15_16: i32) {
    write_control!(24, ofx_15_16 as u32);
    write_control!(25, ofy_15_16 as u32);
}

/// Set the projection-plane distance H (control 26). Larger H = longer
/// focal length = narrower FOV.
#[doc(alias = "SetGeomScreen")]
pub fn set_projection_plane(h: u16) {
    write_control!(26, h as i32 as u32);
}

/// Set the depth-cue coefficients DQA / DQB (control 27, 28).
/// Depth-cue outputs IR0 = DQA/H + DQB, scaled to 0..0x1000.
pub fn set_depth_cue(dqa: i16, dqb: i32) {
    write_control!(27, dqa as i32 as u32);
    write_control!(28, dqb as u32);
}

/// Set the AVSZ3/AVSZ4 averaging weights (control 29, 30). Typical
/// values: `ZSF3 = 0x555` (= 1/3 in 0.12), `ZSF4 = 0x400` (= 1/4).
#[doc(alias = "AVSZ")]
#[doc(alias = "ZSF3")]
#[doc(alias = "ZSF4")]
pub fn set_average_z_weights(zsf3: i16, zsf4: i16) {
    write_control!(29, zsf3 as i32 as u32);
    write_control!(30, zsf4 as i32 as u32);
}

/// Load `v` into the V0 input slot (data registers 0 and 1) and run
/// RTPS to project it. Returns the screen-space pair + depth so the
/// caller can immediately use the result.
///
/// Assumes the rotation matrix, translation, screen offset, and
/// projection plane have already been set.
#[doc(alias = "RotTransPers")]
pub fn project_vertex(v: Vec3I16) -> Projected {
    write_data!(0, v.xy_packed());
    write_data!(1, v.z_packed());
    // SAFETY: V0 has just been loaded; RT / TR / H / OFX / OFY are
    // assumed to be set by the caller's scene setup.
    unsafe { ops::project_single() };
    let sxy = read_data!(14);
    let sz = read_data!(19) as u16;
    Projected {
        sx: sxy as i16,
        sy: (sxy >> 16) as i16,
        sz,
    }
}

/// Project three vertices as a batch via RTPT -- one GTE call, three
/// results out of the SXY FIFO + SZ FIFO. Slightly faster than three
/// successive [`project_vertex`] calls because RTPT shares setup.
///
/// The returned array is `[v0_result, v1_result, v2_result]`.
#[doc(alias = "RotTransPers3")]
pub fn project_triangle(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> [Projected; 3] {
    // Load all three vertices first (data regs 0..=5), then fire RTPT.
    write_data!(0, v0.xy_packed());
    write_data!(1, v0.z_packed());
    write_data!(2, v1.xy_packed());
    write_data!(3, v1.z_packed());
    write_data!(4, v2.xy_packed());
    write_data!(5, v2.z_packed());
    // SAFETY: all three vertices are loaded; scene-setup registers
    // are the caller's responsibility.
    unsafe { ops::project_triple() };
    // After RTPT, SXY FIFO holds (v0, v1, v2) in slots 0/1/2, and
    // SZ FIFO holds them in SZ1/SZ2/SZ3.
    let sxy0 = read_data!(12);
    let sxy1 = read_data!(13);
    let sxy2 = read_data!(14);
    let sz1 = read_data!(17) as u16;
    let sz2 = read_data!(18) as u16;
    let sz3 = read_data!(19) as u16;
    [
        Projected {
            sx: sxy0 as i16,
            sy: (sxy0 >> 16) as i16,
            sz: sz1,
        },
        Projected {
            sx: sxy1 as i16,
            sy: (sxy1 >> 16) as i16,
            sz: sz2,
        },
        Projected {
            sx: sxy2 as i16,
            sy: (sxy2 >> 16) as i16,
            sz: sz3,
        },
    ]
}

/// Transform one vertex by the currently loaded RT/TR matrix without
/// perspective projection. Returns MAC1/2/3 in view-space units.
///
/// Assumes the rotation matrix and translation have already been set.
#[doc(alias = "RotTrans")]
pub fn transform_vertex(v: Vec3I16) -> Vec3I32 {
    write_data!(0, v.xy_packed());
    write_data!(1, v.z_packed());
    // SAFETY: V0 has just been loaded; RT/TR are set by scene setup.
    unsafe { ops::rotate_translate_v0() };
    Vec3I32::new(
        read_data!(25) as i32,
        read_data!(26) as i32,
        read_data!(27) as i32,
    )
}

/// Transform one vertex with a lower-overhead MIPS register schedule.
///
/// Keep the default helper compact; use this variant in measured hot
/// paths that already keep the relevant GTE camera matrix loaded.
#[inline(always)]
pub fn transform_vertex_scheduled(v: Vec3I16) -> Vec3I32 {
    #[cfg(target_arch = "mips")]
    {
        transform_vertex_mips(v)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        transform_vertex(v)
    }
}

/// Compose two Q12 rotation/scale matrices with the GTE's scheduled MVMVA
/// path.
///
/// The left matrix is loaded into the rotation registers and each column of
/// `right` is transformed in turn. Translation is forced to zero, so the
/// result is exactly the rotation/scale product. This clobbers the live GTE
/// rotation and translation state; callers normally load the returned scene
/// matrix immediately afterward.
#[inline]
pub fn compose_rotation_scheduled(left: &Mat3I16, right: &Mat3I16) -> Mat3I16 {
    load_rotation(left);
    load_translation(Vec3I32::ZERO);

    let c0 = transform_vertex_scheduled(Vec3I16::new(right.m[0][0], right.m[1][0], right.m[2][0]));
    let c1 = transform_vertex_scheduled(Vec3I16::new(right.m[0][1], right.m[1][1], right.m[2][1]));
    let c2 = transform_vertex_scheduled(Vec3I16::new(right.m[0][2], right.m[1][2], right.m[2][2]));
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

    Mat3I16 {
        m: [
            [clamp(c0.x), clamp(c1.x), clamp(c2.x)],
            [clamp(c0.y), clamp(c1.y), clamp(c2.y)],
            [clamp(c0.z), clamp(c1.z), clamp(c2.z)],
        ],
    }
}

/// Project one vertex with a lower-overhead MIPS register schedule.
///
/// This is intended for very hot batched paths that have been benchmarked
/// with the larger inlined code shape. The portable path delegates to
/// [`project_vertex`] so host preview/emulator tests remain identical.
#[inline(always)]
pub fn project_vertex_scheduled(v: Vec3I16) -> Projected {
    #[cfg(target_arch = "mips")]
    {
        project_vertex_mips(v)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        project_vertex(v)
    }
}

/// Project three vertices with a lower-overhead MIPS register schedule.
///
/// The normal [`project_triangle`] helper stays compact for general users;
/// this variant is used only by renderer loops where profiling shows that
/// shaving COP2 register wrapper overhead pays for the extra code size.
#[inline(always)]
pub fn project_triangle_scheduled(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> [Projected; 3] {
    #[cfg(target_arch = "mips")]
    {
        project_triangle_mips(v0, v1, v2)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        project_triangle(v0, v1, v2)
    }
}

/// In-flight RTPT kicked by [`start_project_triple`]; [`read`](Self::read)
/// collects the three projected results.
///
/// Between kick and read the caller must not issue any other GTE op
/// or touch GTE registers; arbitrary scalar CPU work is fine and is
/// the whole point -- it runs while the GTE projects. On the host
/// build the projection happens eagerly at kick and `read` just
/// returns it.
#[must_use = "call read() to collect the projected triple"]
#[doc(alias = "RTPT")]
pub struct ProjectTripleInFlight(#[cfg(not(target_arch = "mips"))] [Projected; 3]);

/// Load V0..V2 and issue RTPT without reading results, so the caller
/// can overlap the GTE op with scalar work for the previous triple.
///
/// The MTC2 order matches [`project_triangle_scheduled`]: V0 is
/// written 6 instructions before RTPT issues and RTPT consumes V2
/// last, so the HWB-010/011 commit-slip hazard profile is unchanged.
#[inline(always)]
#[doc(alias = "RTPT")]
pub fn start_project_triple(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> ProjectTripleInFlight {
    #[cfg(target_arch = "mips")]
    {
        let v0_xy = v0.xy_packed();
        let v0_z = v0.z_packed();
        let v1_xy = v1.xy_packed();
        let v1_z = v1.z_packed();
        let v2_xy = v2.xy_packed();
        let v2_z = v2.z_packed();
        // SAFETY: the six words are MTC2 $8..$13 into data regs 0..5 (V0..V2),
        // two NOPs, then RTPT. Each MTC2 only reads its declared `in` register,
        // RTPT only touches GTE registers, and no CPU register is written, so
        // `nomem`/`nostack` with no outputs is accurate.
        unsafe {
            asm!(
                // MTC2 $8..$13 into V0/V1/V2 input registers.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word {w3}",
                ".word {w4}",
                ".word {w5}",
                // Conservative HWB-010/011 input-commit gap for the final V2
                // write. The original console failure proved the rule for
                // MVMVA/RTPS, not RTPT; hardware-tests v1.20 records 0xC0-C3
                // explicitly arbitrate whether RTPT needs these two slots.
                ".word 0",
                ".word 0",
                // RTPT.
                ".word {w6}",
                w0 = const psx_hw::gte::mtc2(8, 0),
                w1 = const psx_hw::gte::mtc2(9, 1),
                w2 = const psx_hw::gte::mtc2(10, 2),
                w3 = const psx_hw::gte::mtc2(11, 3),
                w4 = const psx_hw::gte::mtc2(12, 4),
                w5 = const psx_hw::gte::mtc2(13, 5),
                w6 = const psx_hw::gte::command::RTPT,
                in("$8") v0_xy,
                in("$9") v0_z,
                in("$10") v1_xy,
                in("$11") v1_z,
                in("$12") v2_xy,
                in("$13") v2_z,
                options(nostack, nomem, preserves_flags),
            );
        }
        ProjectTripleInFlight()
    }
    #[cfg(not(target_arch = "mips"))]
    {
        ProjectTripleInFlight(project_triangle(v0, v1, v2))
    }
}

impl ProjectTripleInFlight {
    /// Collect the projected triple.
    ///
    /// MFC2 does not provide a general GTE-busy interlock: the console result
    /// in hardware-tests record 0xB0 disproved that older assumption for
    /// NCLIP/MAC0. This RTPT sequence is safe only if its first result is ready
    /// by the read issue point; v1.20 records 0xC4-C7 measure that exact window.
    /// Until silicon answers them, callers must not treat a short overlap as a
    /// guaranteed blocking wait.
    #[inline(always)]
    pub fn read(self) -> [Projected; 3] {
        #[cfg(target_arch = "mips")]
        {
            let sxy0: u32;
            let sxy1: u32;
            let sxy2: u32;
            let sz1: u32;
            let sz2: u32;
            let sz3: u32;
            // SAFETY: six MFC2s copy SXY0..SXY2 (data 12..14) and SZ1..SZ3 (17..19)
            // into $8..$13, all declared as outputs; each read's load-delay slot is
            // covered by the next MFC2 and the final NOP. No memory or stack is
            // touched. Reading before RTPT finishes yields stale values, not UB (see
            // the doc comment above).
            unsafe {
                asm!(
                    // Read SXY0/SXY1/SXY2/SZ1/SZ2/SZ3; each MFC2's
                    // load-delay slot is filled by the next, so only
                    // the final read needs the explicit NOP.
                    ".word {w0}",
                    ".word {w1}",
                    ".word {w2}",
                    ".word {w3}",
                    ".word {w4}",
                    ".word {w5}",
                    ".word 0",
                    w0 = const psx_hw::gte::mfc2(8, 12),
                    w1 = const psx_hw::gte::mfc2(9, 13),
                    w2 = const psx_hw::gte::mfc2(10, 14),
                    w3 = const psx_hw::gte::mfc2(11, 17),
                    w4 = const psx_hw::gte::mfc2(12, 18),
                    w5 = const psx_hw::gte::mfc2(13, 19),
                    out("$8") sxy0,
                    out("$9") sxy1,
                    out("$10") sxy2,
                    out("$11") sz1,
                    out("$12") sz2,
                    out("$13") sz3,
                    options(nostack, nomem, preserves_flags),
                );
            }
            [
                Projected {
                    sx: sxy0 as i16,
                    sy: (sxy0 >> 16) as i16,
                    sz: sz1 as u16,
                },
                Projected {
                    sx: sxy1 as i16,
                    sy: (sxy1 >> 16) as i16,
                    sz: sz2 as u16,
                },
                Projected {
                    sx: sxy2 as i16,
                    sy: (sxy2 >> 16) as i16,
                    sz: sz3 as u16,
                },
            ]
        }
        #[cfg(not(target_arch = "mips"))]
        {
            self.0
        }
    }
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn project_vertex_mips(v: Vec3I16) -> Projected {
    let mut sxy = v.xy_packed();
    let mut sz = v.z_packed();
    // SAFETY: MTC2 $8/$9 into V0, two NOPs, RTPS, then MFC2 SXY2 (data 14)
    // into $8 and SZ3 (data 19) into $9 plus a load-delay NOP. Only $8 and $9
    // are written and both are `inlateout`; no memory or stack is touched.
    unsafe {
        asm!(
            // MTC2 $8,VXY0 and $9,VZ0.
            ".word {w0}",
            ".word {w1}",
            // HWB-010/011 input-commit gap. The single-vertex path used to
            // issue RTPS immediately here, leaving VXY0 at the same two-tick
            // distance as the console-confirmed MVMVA vertex-explosion case.
            // Two NOPs move it to the known-safe four-instruction distance.
            ".word 0",
            ".word 0",
            // RTPS.
            ".word {w2}",
            // Read SXY2 and SZ3. Two MFC2s can share one final
            // load-delay NOP instead of one NOP per wrapper call.
            ".word {w3}",
            ".word {w4}",
            ".word 0",
            w0 = const psx_hw::gte::mtc2(8, 0),
            w1 = const psx_hw::gte::mtc2(9, 1),
            w2 = const psx_hw::gte::command::RTPS,
            w3 = const psx_hw::gte::mfc2(8, 14),
            w4 = const psx_hw::gte::mfc2(9, 19),
            inlateout("$8") sxy,
            inlateout("$9") sz,
            options(nostack, nomem, preserves_flags),
        );
    }
    Projected {
        sx: sxy as i16,
        sy: (sxy >> 16) as i16,
        sz: sz as u16,
    }
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn project_triangle_mips(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> [Projected; 3] {
    let v0_xy = v0.xy_packed();
    let v0_z = v0.z_packed();
    let v1_xy = v1.xy_packed();
    let v1_z = v1.z_packed();
    let v2_xy = v2.xy_packed();
    let v2_z = v2.z_packed();
    let sxy0: u32;
    let sxy1: u32;
    let sxy2: u32;
    let sz1: u32;
    let sz2: u32;
    let sz3: u32;
    // SAFETY: MTC2 $8..$13 into V0..V2, two NOPs, RTPT, then MFC2s of SXY0..2
    // and SZ1..3 back into $8..$13 with a final load-delay NOP. Exactly those
    // six registers are written, each declared `inlateout`; no memory or
    // stack is touched.
    unsafe {
        asm!(
            // MTC2 $8..$13 into V0/V1/V2 input registers.
            ".word {w0}",
            ".word {w1}",
            ".word {w2}",
            ".word {w3}",
            ".word {w4}",
            ".word {w5}",
            // Conservative HWB-010/011 input-commit gap. The original
            // console failure proved MVMVA/RTPS; hardware-tests v1.20 records
            // 0xC0-C3 measure RTPT directly before we remove these slots.
            // Keep this in sync with start_project_triple.
            ".word 0",
            ".word 0",
            // RTPT.
            ".word {w6}",
            // Read SXY0/SXY1/SXY2/SZ1/SZ2/SZ3. Each MFC2's
            // load-delay slot is filled by the next MFC2, so only the
            // final read needs an explicit NOP before Rust observes
            // the output registers.
            ".word {w7}",
            ".word {w8}",
            ".word {w9}",
            ".word {w10}",
            ".word {w11}",
            ".word {w12}",
            ".word 0",
            w0 = const psx_hw::gte::mtc2(8, 0),
            w1 = const psx_hw::gte::mtc2(9, 1),
            w2 = const psx_hw::gte::mtc2(10, 2),
            w3 = const psx_hw::gte::mtc2(11, 3),
            w4 = const psx_hw::gte::mtc2(12, 4),
            w5 = const psx_hw::gte::mtc2(13, 5),
            w6 = const psx_hw::gte::command::RTPT,
            w7 = const psx_hw::gte::mfc2(8, 12),
            w8 = const psx_hw::gte::mfc2(9, 13),
            w9 = const psx_hw::gte::mfc2(10, 14),
            w10 = const psx_hw::gte::mfc2(11, 17),
            w11 = const psx_hw::gte::mfc2(12, 18),
            w12 = const psx_hw::gte::mfc2(13, 19),
            inlateout("$8") v0_xy => sxy0,
            inlateout("$9") v0_z => sxy1,
            inlateout("$10") v1_xy => sxy2,
            inlateout("$11") v1_z => sz1,
            inlateout("$12") v2_xy => sz2,
            inlateout("$13") v2_z => sz3,
            options(nostack, nomem, preserves_flags),
        );
    }
    [
        Projected {
            sx: sxy0 as i16,
            sy: (sxy0 >> 16) as i16,
            sz: sz1 as u16,
        },
        Projected {
            sx: sxy1 as i16,
            sy: (sxy1 >> 16) as i16,
            sz: sz2 as u16,
        },
        Projected {
            sx: sxy2 as i16,
            sy: (sxy2 >> 16) as i16,
            sz: sz3 as u16,
        },
    ]
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn transform_vertex_mips(v: Vec3I16) -> Vec3I32 {
    let xy = v.xy_packed();
    let z = v.z_packed();
    let mac1: u32;
    let mac2: u32;
    let mac3: u32;
    // SAFETY: MTC2 $8/$9 into V0, two NOPs, MVMVA (RT, V0, TR), then MFC2
    // MAC1..MAC3 (data 25..27) into $8..$10 with a final load-delay NOP. Only
    // $8..$10 are written and all are declared outputs; no memory or stack is
    // touched.
    unsafe {
        asm!(
            // MTC2 $8,VXY0 and $9,VZ0.
            ".word {w0}",
            ".word {w1}",
            // HWB-010/011 hazard gap (console-confirmed fix): two buffer
            // NOPs push the VXY0 write's commit distance from 2 to 4
            // instructions. Without them, real silicon can commit the
            // write BETWEEN the MVMVA's sequential MAC1 and MAC2 compute
            // phases, so MAC1 reads the previous V0.x -- the cortex
            // vertex-explosion mechanism seen in the HWB-010 live capture.
            ".word 0",
            ".word 0",
            // MVMVA RT,V0,TR,sf=1.
            ".word {w2}",
            // Read MAC1/MAC2/MAC3. Consecutive MFC2 instructions fill
            // each other's load-delay slot; only the final read needs
            // an explicit NOP before Rust observes the outputs.
            ".word {w3}",
            ".word {w4}",
            ".word {w5}",
            ".word 0",
            w0 = const psx_hw::gte::mtc2(8, 0),
            w1 = const psx_hw::gte::mtc2(9, 1),
            w2 = const psx_hw::gte::command::ROTATE_TRANSLATE_V0,
            w3 = const psx_hw::gte::mfc2(8, 25),
            w4 = const psx_hw::gte::mfc2(9, 26),
            w5 = const psx_hw::gte::mfc2(10, 27),
            inlateout("$8") xy => mac1,
            inlateout("$9") z => mac2,
            lateout("$10") mac3,
            options(nostack, nomem, preserves_flags),
        );
    }
    Vec3I32::new(mac1 as i32, mac2 as i32, mac3 as i32)
}

/// Result of [`transform_vertex_probed`]: the live-schedule transform
/// output plus post-op hazard evidence.
#[derive(Clone, Copy)]
pub struct TransformProbe {
    /// The transform result exactly as the hot path reads it (the
    /// IMMEDIATE MAC1/2/3 reads, same schedule as
    /// `transform_vertex_mips`). Consumers keep using this so the
    /// probed build behaves identically to the live engine.
    pub out: Vec3I32,
    /// MAC1 re-read after a 4-NOP settle gap. Differs from `out.x`
    /// only if the immediate MAC1 read was served stale.
    pub x_settled: i32,
    /// Translation control regs (TRX/TRY/TRZ, cr5..cr7) read back
    /// AFTER the op (nothing writes them during, so this is the value
    /// in effect while the MVMVA executed). The compose path loads
    /// zero here; nonzero = the zero write did not land for this op.
    pub tr: [i32; 3],
}

/// Hazard-hunt instrument (keep): one MVMVA on the DELIBERATELY
/// UNPADDED pre-HWB-011 schedule (V0 writes 1-2 instructions before
/// the op -- the schedule that trips the silicon MTC2-commit hazard),
/// with evidence reads appended strictly AFTER the op: a settled MAC1
/// re-read and a TRX/TRY/TRZ readback. This is the live evidence
/// channel that decoded the vertex explosion; it stays in the SDK so the
/// next hardware mystery starts from a proven instrument. Not for production paths -- use
/// [`transform_vertex_scheduled`], whose schedule carries the
/// console-confirmed hazard gap.
#[inline(always)]
pub fn transform_vertex_probed(v: Vec3I16) -> TransformProbe {
    #[cfg(target_arch = "mips")]
    {
        let xy = v.xy_packed();
        let z = v.z_packed();
        let mac1: u32;
        let mac2: u32;
        let mac3: u32;
        let mac1_settled: u32;
        let trx: u32;
        let try_: u32;
        let trz: u32;
        // SAFETY: same MTC2/MVMVA/MFC2 shape as `transform_vertex_mips` minus the
        // input gap, then a MAC1 re-read into $11 and CFC2 TRX/TRY/TRZ (control
        // 5..7) into $12..$14. Every written register ($8..$14) is a declared
        // output and no memory or stack is touched. The deliberately unpadded
        // schedule can return stale MAC values; that is a data hazard, not UB.
        unsafe {
            asm!(
                // Live schedule, byte-identical to transform_vertex_mips:
                // MTC2 $8,VXY0 / MTC2 $9,VZ0 / MVMVA / MAC1,2,3 reads.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word {w3}",
                ".word {w4}",
                ".word {w5}",
                ".word 0",
                // Probe tail, strictly AFTER the live reads: 4-NOP
                // settle gap, MAC1 re-read ($11), then TRX/TRY/TRZ
                // (cr5..cr7) read-back; chained CFC2s share delay
                // slots, final NOP covers the last one.
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word {w6}",
                ".word {w7}",
                ".word {w8}",
                ".word {w9}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 0),
                w1 = const psx_hw::gte::mtc2(9, 1),
                w2 = const psx_hw::gte::command::ROTATE_TRANSLATE_V0,
                w3 = const psx_hw::gte::mfc2(8, 25),
                w4 = const psx_hw::gte::mfc2(9, 26),
                w5 = const psx_hw::gte::mfc2(10, 27),
                w6 = const psx_hw::gte::mfc2(11, 25),
                w7 = const psx_hw::gte::cfc2(12, 5),
                w8 = const psx_hw::gte::cfc2(13, 6),
                w9 = const psx_hw::gte::cfc2(14, 7),
                inlateout("$8") xy => mac1,
                inlateout("$9") z => mac2,
                lateout("$10") mac3,
                lateout("$11") mac1_settled,
                lateout("$12") trx,
                lateout("$13") try_,
                lateout("$14") trz,
                options(nostack, nomem, preserves_flags),
            );
        }
        TransformProbe {
            out: Vec3I32::new(mac1 as i32, mac2 as i32, mac3 as i32),
            x_settled: mac1_settled as i32,
            tr: [trx as i32, try_ as i32, trz as i32],
        }
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let out = transform_vertex(v);
        TransformProbe {
            out,
            x_settled: out.x,
            tr: [
                read_control!(5) as i32,
                read_control!(6) as i32,
                read_control!(7) as i32,
            ],
        }
    }
}

/// Read the last three projected Z values and compute their average
/// via AVSZ3 (weighted by ZSF3). Returns OTZ -- the depth key most
/// renderers use for ordering-table inserts.
pub fn average_z_triangle() -> u16 {
    // SAFETY: no input registers to prepare -- AVSZ3 reads SZ1..SZ3
    // which were populated by the most recent RTPT / project_triangle.
    unsafe { ops::average_z3() };
    read_data!(7) as u16
}

/// Reload three cached projected depths into the GTE SZ FIFO and compute OTZ.
///
/// This is the indexed-mesh counterpart to [`average_z_triangle`]. It keeps
/// cached vertex projection while using the PS1's AVSZ3 unit instead of a
/// software 64-bit multiply for every face. The configured ZSF3 weight is
/// read from GTE control register 29.
#[inline(always)]
pub fn average_cached_z3(depths: [u16; 3]) -> u16 {
    #[cfg(target_arch = "mips")]
    {
        let mut otz = depths[0] as u32;
        // SAFETY: MTC2 $8..$10 into SZ1..SZ3 (data 17..19), two NOPs, AVSZ3,
        // then MFC2 OTZ (data 7) into $8 plus a load-delay NOP. Only $8 is written
        // (`inlateout`); $9/$10 are read-only inputs; no memory or stack is
        // touched.
        unsafe {
            asm!(
                // Load SZ1..SZ3, then leave the hardware-safe two-slot MTC2
                // commit gap before AVSZ3 consumes the final write.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word 0",
                ".word 0",
                ".word {w3}",
                // MFC2 has one CPU load-delay slot.
                ".word {w4}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 17),
                w1 = const psx_hw::gte::mtc2(9, 18),
                w2 = const psx_hw::gte::mtc2(10, 19),
                w3 = const psx_hw::gte::command::AVSZ3,
                w4 = const psx_hw::gte::mfc2(8, 7),
                inlateout("$8") otz,
                in("$9") depths[1] as u32,
                in("$10") depths[2] as u32,
                options(nostack, nomem, preserves_flags),
            );
        }
        otz as u16
    }
    #[cfg(not(target_arch = "mips"))]
    {
        write_data!(17, depths[0] as u32);
        write_data!(18, depths[1] as u32);
        write_data!(19, depths[2] as u32);
        // SAFETY: SZ1, SZ2 and SZ3 were loaded immediately above.
        unsafe { ops::average_z3() };
        read_data!(7) as u16
    }
}

/// Scale a three-depth sum into the classic 2,048-slot OTZ domain.
///
/// This is exactly `(sum * 0x155) >> 12`. The MIPS sequence factors 0x155 as
/// `5 * 17 * 4 + 1`, avoiding two shifts and additions emitted for the flat
/// constant multiply on MIPS I.
#[inline(always)]
#[doc(alias = "OTZ")]
pub fn classic_ordering_depth3_from_sum(sum: u32) -> u16 {
    #[cfg(target_arch = "mips")]
    {
        let mut otz = sum;
        // SAFETY: pure ALU shift-add on $8..$10. $8 is `inlateout` and the
        // $9/$10 scratch registers are declared `lateout(_)`; no memory or stack
        // is touched and `addu`/`sll`/`srl` never trap.
        unsafe {
            asm!(
                "sll $9, $8, 2",
                "addu $9, $9, $8",
                "sll $10, $9, 4",
                "addu $9, $10, $9",
                "sll $9, $9, 2",
                "addu $9, $9, $8",
                "srl $8, $9, 12",
                inlateout("$8") otz,
                lateout("$9") _,
                lateout("$10") _,
                options(nostack, nomem, preserves_flags),
            );
        }
        otz as u16
    }
    #[cfg(not(target_arch = "mips"))]
    {
        ((sum * 0x155) >> 12) as u16
    }
}

/// Reload four cached projected depths into the GTE SZ FIFO and compute OTZ.
///
/// This is the AVSZ4 counterpart to [`average_cached_z3`]. The configured
/// ZSF4 weight is read from GTE control register 30.
#[inline(always)]
pub fn average_cached_z4(depths: [u16; 4]) -> u16 {
    #[cfg(target_arch = "mips")]
    {
        let mut otz = depths[0] as u32;
        // SAFETY: MTC2 $8..$11 into SZ0..SZ3 (data 16..19), two NOPs, AVSZ4,
        // then MFC2 OTZ (data 7) into $8 plus a load-delay NOP. Only $8 is written
        // (`inlateout`); $9..$11 are read-only inputs; no memory or stack is
        // touched.
        unsafe {
            asm!(
                // Load SZ0..SZ3, then leave the hardware-safe two-slot MTC2
                // commit gap before AVSZ4 consumes the final write.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word {w3}",
                ".word 0",
                ".word 0",
                ".word {w4}",
                // MFC2 has one CPU load-delay slot.
                ".word {w5}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 16),
                w1 = const psx_hw::gte::mtc2(9, 17),
                w2 = const psx_hw::gte::mtc2(10, 18),
                w3 = const psx_hw::gte::mtc2(11, 19),
                w4 = const psx_hw::gte::command::AVSZ4,
                w5 = const psx_hw::gte::mfc2(8, 7),
                inlateout("$8") otz,
                in("$9") depths[1] as u32,
                in("$10") depths[2] as u32,
                in("$11") depths[3] as u32,
                options(nostack, nomem, preserves_flags),
            );
        }
        otz as u16
    }
    #[cfg(not(target_arch = "mips"))]
    {
        write_data!(16, depths[0] as u32);
        write_data!(17, depths[1] as u32);
        write_data!(18, depths[2] as u32);
        write_data!(19, depths[3] as u32);
        // SAFETY: SZ0 through SZ3 were loaded immediately above.
        unsafe { ops::average_z4() };
        read_data!(7) as u16
    }
}

/// Compute AVSZ3's saturated OTZ result from three cached projected depths.
///
/// This pure-software form is useful for host processing and for callers that
/// cannot disturb the live GTE FIFO. MIPS render loops should normally prefer
/// [`average_cached_z3`]. The arithmetic is exactly the GTE operation: sum the
/// unsigned 16-bit depths, multiply by signed `ZSF3`, shift right by 12, then
/// saturate to OTZ's unsigned 16-bit range.
#[inline]
#[doc(alias = "OTZ")]
#[doc(alias = "AverageZ3")]
pub fn average_z3_ordering_depth(depths: [u16; 3], zsf3: i16) -> u16 {
    // ZSF3 is i16 and each depth is u16, so the product reaches ~2^41: the
    // hardware AVSZ3 accumulates in the GTE's own wide register and this is
    // the software mirror of it. An i32 accumulator would wrap and hand the
    // ordering table a near OTZ for far geometry.
    // psx-numeric-allow-next-line: AVSZ3 wide accumulator, see above
    let sum = depths[0] as i64 + depths[1] as i64 + depths[2] as i64;
    // psx-numeric-allow-next-line: AVSZ3 wide accumulator
    ((sum * zsf3 as i64) >> 12).clamp(0, u16::MAX as i64) as u16
}

/// Compute AVSZ4's saturated OTZ result from four cached projected depths.
///
/// This is the four-vertex counterpart to [`average_z3_ordering_depth`] and matches the
/// GTE's `AVSZ4` operation with the supplied `ZSF4` value. MIPS render loops
/// should normally prefer [`average_cached_z4`].
#[inline]
#[doc(alias = "OTZ")]
#[doc(alias = "AverageZ4")]
pub fn average_z4_ordering_depth(depths: [u16; 4], zsf4: i16) -> u16 {
    // psx-numeric-allow-next-line: AVSZ4 wide accumulator, same reasoning as average_z3_ordering_depth
    let sum = depths[0] as i64 + depths[1] as i64 + depths[2] as i64 + depths[3] as i64;
    // psx-numeric-allow-next-line: AVSZ4 wide accumulator
    ((sum * zsf4 as i64) >> 12).clamp(0, u16::MAX as i64) as u16
}

#[inline(always)]
fn aabb_outer_support(mins: [i16; 3], maxs: [i16; 3], signbits: u8) -> Vec3I16 {
    Vec3I16::new(
        if signbits & 1 != 0 { mins[0] } else { maxs[0] },
        if signbits & 2 != 0 { mins[1] } else { maxs[1] },
        if signbits & 4 != 0 { mins[2] } else { maxs[2] },
    )
}

/// `MVMVA` of V0 by the rotation matrix, no translation, `sf` clear.
#[cfg(target_arch = "mips")]
const RT_V0_NO_TRANSLATION: u32 = psx_hw::gte::mvmva(
    psx_hw::gte::Matrix::Rotation,
    psx_hw::gte::Vector::V0,
    psx_hw::gte::Translation::None,
);

/// `MVMVA` of V0 by the light matrix, no translation, `sf` clear.
#[cfg(target_arch = "mips")]
const LLM_V0_NO_TRANSLATION: u32 = psx_hw::gte::mvmva(
    psx_hw::gte::Matrix::Light,
    psx_hw::gte::Vector::V0,
    psx_hw::gte::Translation::None,
);

/// `MFC2 $8, MAC1 + n`.
#[cfg(target_arch = "mips")]
const fn read_mac(n: u32) -> u32 {
    psx_hw::gte::mfc2(8, 25 + n)
}

#[cfg(target_arch = "mips")]
#[inline(always)]
fn aabb_dot_mvmva<const OP: u32, const READ_MAC: u32>(v: Vec3I16) -> i32 {
    let mut dot = v.xy_packed();
    // SAFETY: MTC2 $8/$9 into V0, two NOPs, then `OP` and `READ_MAC`. The
    // only instantiations (`aabb_clip_dot`) pass MVMVA encodings (`0x4a00_6012`
    // RT, `0x4a02_6012` LLM; cv=none, sf=0) and MFC2 of MAC1..MAC3 into $8,
    // so only $8 is written (`inlateout`) and no memory or stack is touched.
    // This private fn trusts its const arguments; any new caller must keep
    // `READ_MAC` writing $8 only.
    unsafe {
        asm!(
            ".word {w0}",
            ".word {w1}",
            // Same console-confirmed V0 commit distance as
            // transform_vertex_mips.
            ".word 0",
            ".word 0",
            ".word {op}",
            ".word {read_mac}",
            ".word 0",
            w0 = const psx_hw::gte::mtc2(8, 0),
            w1 = const psx_hw::gte::mtc2(9, 1),
            op = const OP,
            read_mac = const READ_MAC,
            inlateout("$8") dot,
            in("$9") v.z_packed(),
            options(nostack, nomem, preserves_flags),
        );
    }
    dot as i32
}

#[inline(always)]
fn aabb_clip_dot(v: Vec3I16, _plane: &AabbClipPlane, index: usize) -> i32 {
    #[cfg(target_arch = "mips")]
    {
        match index {
            0 => aabb_dot_mvmva::<{ RT_V0_NO_TRANSLATION }, { read_mac(0) }>(v),
            1 => aabb_dot_mvmva::<{ RT_V0_NO_TRANSLATION }, { read_mac(1) }>(v),
            2 => aabb_dot_mvmva::<{ RT_V0_NO_TRANSLATION }, { read_mac(2) }>(v),
            3 => aabb_dot_mvmva::<{ LLM_V0_NO_TRANSLATION }, { read_mac(0) }>(v),
            _ => 0,
        }
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let _ = index;
        _plane.normal[0] as i32 * v.x as i32
            + _plane.normal[1] as i32 * v.y as i32
            + _plane.normal[2] as i32 * v.z as i32
    }
}

/// Classify a signed-integer AABB against four planes loaded by
/// [`load_aabb_clip4`].
///
/// `clip_flags` selects active planes in bits zero through three. Returns
/// `-1` when the box is fully outside any active plane; otherwise returns the
/// remaining intersecting-plane flags after fully-inside planes are cleared.
/// The GTE path computes the same unshifted signed Q12 dot products as six
/// scalar MIPS multiplications per active plane.
#[inline]
pub fn classify_aabb_clip4(
    mins: [i16; 3],
    maxs: [i16; 3],
    planes: &[AabbClipPlane; 4],
    mut clip_flags: u8,
) -> i32 {
    let mut index = 0usize;
    while index < 4 {
        let flag = 1u8 << index;
        if clip_flags & flag != 0 {
            let plane = &planes[index];
            let outer = aabb_outer_support(mins, maxs, plane.signbits);
            if aabb_clip_dot(outer, plane, index) < plane.distance {
                return -1;
            }
            let inner = aabb_outer_support(maxs, mins, plane.signbits);
            if aabb_clip_dot(inner, plane, index) >= plane.distance {
                clip_flags &= !flag;
            }
        }
        index += 1;
    }
    clip_flags as i32
}

/// Return whether a signed-integer AABB is outside any selected plane loaded
/// by [`load_aabb_clip4`].
///
/// Always inlined: renderers call this once per candidate face inside their
/// selection loop, and as an out-of-line call it spent a third of its cycles
/// on the call itself (two arrays by value plus the plane pointer).
#[inline(always)]
pub fn is_aabb_outside_clip4(
    mins: [i16; 3],
    maxs: [i16; 3],
    planes: &[AabbClipPlane; 4],
    clip_flags: u8,
) -> bool {
    let mut index = 0usize;
    while index < 4 {
        let flag = 1u8 << index;
        if clip_flags & flag != 0 {
            let plane = &planes[index];
            let outer = aabb_outer_support(mins, maxs, plane.signbits);
            if aabb_clip_dot(outer, plane, index) < plane.distance {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// Signed screen-space area of a triangle -- the same value GTE `NCLIP`
/// writes to MAC0: `SX0*(SY1-SY2) + SX1*(SY2-SY0) + SX2*(SY0-SY1)`.
/// Positive = front-facing, `<= 0` = back-facing/degenerate.
///
/// Computed in software rather than via `NCLIP` on purpose: reading MAC0
/// immediately after `NCLIP` returns a STALE value on real PS1 hardware
/// (the GTE result-read hazard -- MAC0 settles a few cycles later than the
/// CPU reads it), which mis-culls and drops wall faces on silicon while
/// looking fine on every emulator. The i32 cross product is exact for
/// screen coordinates (|coord| <= 0x400 after clamping) and has no read
/// latency. Confirmed against real hardware (cortex GTE disc 2026-06-09:
/// NCLIP MAC0 back-to-back read is stale, +8 nops reads correct).
#[inline]
#[doc(alias = "NCLIP")]
#[doc(alias = "MAC0")]
pub fn screen_area(vertices: [(i16, i16); 3]) -> i32 {
    let (sx0, sy0) = (vertices[0].0 as i32, vertices[0].1 as i32);
    let (sx1, sy1) = (vertices[1].0 as i32, vertices[1].1 as i32);
    let (sx2, sy2) = (vertices[2].0 as i32, vertices[2].1 as i32);
    // Algebraically identical to the three-product NCLIP expansion above,
    // but expressed as one 2D cross product. On MIPS-I this removes one MULT
    // from every cached-room and model-face backface test.
    (sx1 - sx0) * (sy2 - sy0) - (sy1 - sy0) * (sx2 - sx0)
}

/// Run a hardware-safe `NCLIP` for an already-projected triangle.
///
/// The two input NOPs and eight-instruction result distance are both required
/// by the console measurements documented on [`screen_area`]. This is
/// useful in tight indexed-model loops where the GTE would otherwise be idle.
#[inline(always)]
#[doc(alias = "NCLIP")]
#[doc(alias = "MAC0")]
pub fn screen_area_scheduled(vertices: [(i16, i16); 3]) -> i32 {
    #[cfg(target_arch = "mips")]
    {
        let sxy0 = pack_xy(vertices[0].0, vertices[0].1);
        let sxy1 = pack_xy(vertices[1].0, vertices[1].1);
        let mut area = pack_xy(vertices[2].0, vertices[2].1);
        // SAFETY: MTC2 $8..$10 into SXY0..SXY2 (data 12..14), two NOPs, NCLIP,
        // eight NOPs, then MFC2 MAC0 (data 24) into $10 plus a load-delay NOP.
        // Only $10 is written (`inlateout`); no memory or stack is touched.
        unsafe {
            asm!(
                // MTC2 $8/$9/$10,SXY0/SXY1/SXY2.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                // HWB-010/011 input-commit gap before NCLIP consumes SXY2.
                ".word 0",
                ".word 0",
                // NCLIP plus its hardware-confirmed MAC0 result gap.
                ".word {w3}",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                // MFC2 $10,MAC0 plus its CPU load-delay slot.
                ".word {w4}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 12),
                w1 = const psx_hw::gte::mtc2(9, 13),
                w2 = const psx_hw::gte::mtc2(10, 14),
                w3 = const psx_hw::gte::command::NCLIP,
                w4 = const psx_hw::gte::mfc2(10, 24),
                in("$8") sxy0,
                in("$9") sxy1,
                inlateout("$10") area,
                options(nostack, nomem, preserves_flags),
            );
        }
        area as i32
    }
    #[cfg(not(target_arch = "mips"))]
    {
        screen_area(vertices)
    }
}

/// Run hardware-safe `NCLIP` while unpacking one aligned model-face record.
///
/// Runtime model faces already carry three `vertex | uv << 16` corner words,
/// with a two-bit palette selector in bits 14..15 of the first vertex word.
/// The five register-only unpack instructions occupy five of NCLIP's eight
/// mandatory MAC0 result slots. No GTE input or result hazard is shortened.
#[inline(always)]
pub fn screen_area_and_unpack_model_face_scheduled(
    vertices: [(i16, i16); 3],
    corner_words: [u32; 3],
) -> (i32, [u16; 3], u8) {
    #[cfg(target_arch = "mips")]
    {
        let sxy0 = pack_xy(vertices[0].0, vertices[0].1);
        let sxy1 = pack_xy(vertices[1].0, vertices[1].1);
        let mut area = pack_xy(vertices[2].0, vertices[2].1);
        let mut uv0 = corner_words[0];
        let mut uv1 = corner_words[1];
        let mut uv2 = corner_words[2];
        let palette_bank: u32;
        // SAFETY: MTC2 $8..$10 into SXY0..SXY2, NCLIP, register-only `srl`/`andi`
        // on $11..$14, then MFC2 MAC0 into $10. The written registers ($10..$14)
        // are all declared `inlateout`/`lateout`; no memory or stack is touched.
        unsafe {
            asm!(
                // MTC2 SXY0..SXY2 and the hardware-confirmed input gap.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word 0",
                ".word 0",
                ".word {w3}",
                // Register-only face unpacking inside NCLIP's complete
                // eight-instruction MAC0 result gap.
                "srl $14, $11, 14",
                "andi $14, $14, 3",
                "srl $11, $11, 16",
                "srl $12, $12, 16",
                "srl $13, $13, 16",
                ".word 0",
                ".word 0",
                ".word 0",
                // MFC2 MAC0 plus the CPU load-delay slot.
                ".word {w4}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 12),
                w1 = const psx_hw::gte::mtc2(9, 13),
                w2 = const psx_hw::gte::mtc2(10, 14),
                w3 = const psx_hw::gte::command::NCLIP,
                w4 = const psx_hw::gte::mfc2(10, 24),
                in("$8") sxy0,
                in("$9") sxy1,
                inlateout("$10") area,
                inlateout("$11") uv0,
                inlateout("$12") uv1,
                inlateout("$13") uv2,
                lateout("$14") palette_bank,
                options(nostack, nomem, preserves_flags),
            );
        }
        (
            area as i32,
            [uv0 as u16, uv1 as u16, uv2 as u16],
            palette_bank as u8,
        )
    }
    #[cfg(not(target_arch = "mips"))]
    {
        (
            screen_area(vertices),
            [
                (corner_words[0] >> 16) as u16,
                (corner_words[1] >> 16) as u16,
                (corner_words[2] >> 16) as u16,
            ],
            ((corner_words[0] >> 14) & 3) as u8,
        )
    }
}

/// Run hardware-safe `NCLIP` and `AVSZ3` for an already-projected indexed
/// triangle.
///
/// The cached screen coordinates and depths are loaded together. The three
/// SZ writes occupy part of the silicon-required NCLIP result gap, so callers
/// that need both winding and an OT key avoid paying two independent GTE
/// schedules. The returned area is identical to [`screen_area`], and the
/// depth is identical to [`average_cached_z3`] with the installed ZSF3.
#[inline(always)]
pub fn screen_area_and_average_cached_z3_scheduled(
    vertices: [(i16, i16); 3],
    depths: [u16; 3],
) -> (i32, u16) {
    #[cfg(target_arch = "mips")]
    {
        let sxy0 = pack_xy(vertices[0].0, vertices[0].1);
        let sxy1 = pack_xy(vertices[1].0, vertices[1].1);
        let mut area = pack_xy(vertices[2].0, vertices[2].1);
        let mut otz = depths[0] as u32;
        // SAFETY: MTC2 $8..$10 into SXY0..SXY2, NCLIP, MTC2 $11..$13 into
        // SZ1..SZ3, then MFC2 MAC0 into $10, AVSZ3, and MFC2 OTZ into $11 with a
        // final load-delay NOP. Only $10 and $11 are written (`inlateout`);
        // $12/$13 are read-only; no memory or stack is touched.
        unsafe {
            asm!(
                // Load SXY0..SXY2 and leave the measured input-commit gap.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word 0",
                ".word 0",
                ".word {w3}",
                // Loading SZ1..SZ3 is independent work inside NCLIP's
                // hardware-confirmed eight-instruction MAC0 result gap.
                ".word {w4}",
                ".word {w5}",
                ".word {w6}",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                // Capture NCLIP's MAC0 before AVSZ3 overwrites it. AVSZ3 is
                // independent of the CPU load-delay result.
                ".word {w7}",
                ".word {w8}",
                ".word {w9}",
                ".word 0",
                w0 = const psx_hw::gte::mtc2(8, 12),
                w1 = const psx_hw::gte::mtc2(9, 13),
                w2 = const psx_hw::gte::mtc2(10, 14),
                w3 = const psx_hw::gte::command::NCLIP,
                w4 = const psx_hw::gte::mtc2(11, 17),
                w5 = const psx_hw::gte::mtc2(12, 18),
                w6 = const psx_hw::gte::mtc2(13, 19),
                w7 = const psx_hw::gte::mfc2(10, 24),
                w8 = const psx_hw::gte::command::AVSZ3,
                w9 = const psx_hw::gte::mfc2(11, 7),
                in("$8") sxy0,
                in("$9") sxy1,
                inlateout("$10") area,
                inlateout("$11") otz,
                in("$12") depths[1] as u32,
                in("$13") depths[2] as u32,
                options(nostack, nomem, preserves_flags),
            );
        }
        (area as i32, otz as u16)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        (screen_area(vertices), average_cached_z3(depths))
    }
}

/// Run hardware-safe `NCLIP` and compute the classic 0x155-scaled OTZ from
/// three cached depths without disturbing the GTE depth FIFO.
///
/// `ZSF3 = 0x155` is the historical 2,048-slot ordering-table scale used by
/// the classic affine path. The CPU shift-add sequence is placed entirely in
/// NCLIP's required MAC0 result gap, so it replaces the later AVSZ3 command
/// without extending the hazard schedule.
#[inline(always)]
#[doc(alias = "OTZ")]
pub fn screen_area_and_classic_ordering_depth3_scheduled(
    vertices: [(i16, i16); 3],
    depths: [u16; 3],
) -> (i32, u16) {
    #[cfg(target_arch = "mips")]
    {
        let sxy0 = pack_xy(vertices[0].0, vertices[0].1);
        let sxy1 = pack_xy(vertices[1].0, vertices[1].1);
        let mut area = pack_xy(vertices[2].0, vertices[2].1);
        let mut otz = depths[0] as u32;
        let depth1 = depths[1] as u32;
        let depth2 = depths[2] as u32;
        // SAFETY: MTC2 $8..$10 into SXY0..SXY2, NCLIP, ALU shift-add on
        // $11..$13, MFC2 MAC0 into $10 and a final `srl` into $11. Every written
        // register ($10..$13) is declared `inlateout` (scratch outputs discarded);
        // no memory or stack is touched and the ALU ops never trap.
        unsafe {
            asm!(
                // Load SXY0..SXY2 and leave the measured input-commit gap.
                ".word {w0}",
                ".word {w1}",
                ".word {w2}",
                ".word 0",
                ".word 0",
                // NCLIP.
                ".word {w3}",
                // Fill NCLIP's eight-instruction MAC0 result gap with the
                // exact sum * 0x155 sequence: 5x, 85x, then 341x.
                "addu $11, $11, $12",
                "addu $11, $11, $13",
                "sll $12, $11, 2",
                "addu $12, $12, $11",
                "sll $13, $12, 4",
                "addu $12, $13, $12",
                "sll $12, $12, 2",
                "addu $12, $12, $11",
                // Read MAC0 and use its CPU load-delay slot for the OTZ
                // scale. Neither instruction depends on the other's result.
                ".word {w4}",
                "srl $11, $12, 12",
                w0 = const psx_hw::gte::mtc2(8, 12),
                w1 = const psx_hw::gte::mtc2(9, 13),
                w2 = const psx_hw::gte::mtc2(10, 14),
                w3 = const psx_hw::gte::command::NCLIP,
                w4 = const psx_hw::gte::mfc2(10, 24),
                in("$8") sxy0,
                in("$9") sxy1,
                inlateout("$10") area,
                inlateout("$11") otz,
                inlateout("$12") depth1 => _,
                inlateout("$13") depth2 => _,
                options(nostack, nomem, preserves_flags),
            );
        }
        (area as i32, otz as u16)
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let sum = depths[0] as u32 + depths[1] as u32 + depths[2] as u32;
        (screen_area(vertices), ((sum * 0x155) >> 12) as u16)
    }
}

/// Back-face test for three already-projected screen-space vertices.
///
/// Useful when a renderer cached/projected vertices first and later wants
/// the signed screen-space area test for arbitrary indexed faces. Uses the
/// software [`screen_area`] (see its note on the NCLIP MAC0 hazard).
pub fn is_screen_triangle_back_facing(vertices: [(i16, i16); 3]) -> bool {
    screen_area(vertices) <= 0
}

/// Read the GTE FLAG register. Non-zero indicates at least one error
/// bit fired during the last op (overflow, saturation, divide
/// overflow). Useful for debug prints on a frame that looks wrong.
#[doc(alias = "FLAG")]
pub fn error_flags() -> u32 {
    read_control!(31)
}

/// Renamed to [`set_average_z_weights`].
#[deprecated(note = "renamed to `set_average_z_weights`")]
#[inline(always)]
pub fn set_avsz_weights(zsf3: i16, zsf4: i16) {
    set_average_z_weights(zsf3, zsf4)
}

/// Renamed to [`start_project_triple`].
#[deprecated(note = "renamed to `start_project_triple`")]
#[inline(always)]
pub fn rtpt_kick(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> ProjectTripleInFlight {
    start_project_triple(v0, v1, v2)
}

/// Renamed to [`classic_ordering_depth3_from_sum`].
#[deprecated(note = "renamed to `classic_ordering_depth3_from_sum`")]
#[inline(always)]
pub fn classic_otz3_from_sum(sum: u32) -> u16 {
    classic_ordering_depth3_from_sum(sum)
}

/// Renamed to [`is_aabb_outside_clip4`].
#[deprecated(note = "renamed to `is_aabb_outside_clip4`")]
#[inline(always)]
pub fn aabb_outside_clip4(
    mins: [i16; 3],
    maxs: [i16; 3],
    planes: &[AabbClipPlane; 4],
    clip_flags: u8,
) -> bool {
    is_aabb_outside_clip4(mins, maxs, planes, clip_flags)
}

/// Renamed to [`screen_area_scheduled`].
#[deprecated(note = "renamed to `screen_area_scheduled`")]
#[inline(always)]
pub fn screen_area_mac0_scheduled(vertices: [(i16, i16); 3]) -> i32 {
    screen_area_scheduled(vertices)
}

#[cfg(all(test, not(target_arch = "mips")))]
mod host_smoke {
    //! Smoke tests for the host-side software-GTE shim.
    //!
    //! On hardware these helpers compile to inline COP2 instructions,
    //! so testing them via Rust integration would require running on
    //! a PS1. On host they route through the per-thread Gte from
    //! `psx-gte-core`, which we *can* poke at directly to confirm the
    //! routing produces matching output.
    use super::*;
    use crate::host;

    fn install_identity() {
        load_rotation(&Mat3I16::IDENTITY);
        load_translation(Vec3I32::ZERO);
        set_screen_offset(160 << 16, 120 << 16);
        set_projection_plane(200);
    }

    #[test]
    fn rtps_through_host_shim_projects_an_in_front_vertex() {
        host::reset();
        install_identity();
        // V0 = (0, 0, 1024) -- straight ahead, depth 1024. With H=200
        // the GTE divides 200/sz3 (≈0x4000/sz3 internally), giving an
        // X/Y near the screen offset for a vertex at the origin.
        let projected = project_vertex(Vec3I16::new(0, 0, 1024));
        assert_eq!(projected.sx, 160);
        assert_eq!(projected.sy, 120);
        assert!(
            projected.sz > 0,
            "near-plane vertex must yield non-zero depth"
        );
    }

    #[test]
    fn rtpt_through_host_shim_matches_three_separate_rtps_calls() {
        host::reset();
        install_identity();
        let a = Vec3I16::new(-256, 0, 1024);
        let b = Vec3I16::new(256, 0, 1024);
        let c = Vec3I16::new(0, 256, 1024);

        let batch = project_triangle(a, b, c);

        host::reset();
        install_identity();
        let p_a = project_vertex(a);
        let p_b = project_vertex(b);
        let p_c = project_vertex(c);

        assert_eq!(batch[0], p_a);
        assert_eq!(batch[1], p_b);
        assert_eq!(batch[2], p_c);
    }

    #[test]
    fn mvmva_transform_through_host_shim_applies_rt_and_tr() {
        host::reset();
        load_rotation(&Mat3I16::IDENTITY);
        load_translation(Vec3I32::new(10, -20, 30));

        let transformed = transform_vertex(Vec3I16::new(100, 200, 300));

        assert_eq!(transformed, Vec3I32::new(110, 180, 330));
    }

    #[test]
    fn scheduled_rotation_compose_matches_cpu_matrix_product() {
        host::reset();
        let left = Mat3I16::rotate_z(37).mul(&Mat3I16::rotate_y(91));
        let right = Mat3I16::rotate_y(13)
            .mul(&Mat3I16::rotate_x(55))
            .scale_columns_q12([3072, 4096, 5120]);

        assert_eq!(compose_rotation_scheduled(&left, &right), left.mul(&right));
    }

    #[test]
    fn classic_normalize_matches_reference_table_results() {
        for (input, vector, xy_squared, squared) in [
            ([1, 0, 0], [4096, 0, 0], 1, 1),
            ([3, 4, 0], [2457, 3276, 0], 25, 25),
            ([10, 20, 30], [1097, 2195, 3293], 500, 1400),
            ([-10, 20, 30], [-1098, 2195, 3293], 500, 1400),
            ([100, 50, -20], [3610, 1805, -723], 12_500, 12_900),
            ([600, 0, 200], [3898, 0, 1299], 360_000, 400_000),
        ] {
            host::reset();
            let result = normalize_classic_q12_scheduled(Vec3I32::new(
                input[0] << 12,
                input[1] << 12,
                input[2] << 12,
            ));
            assert_eq!(
                result,
                ClassicNormalizedVector {
                    vector: Vec3I16::new(vector[0], vector[1], vector[2]),
                    xy_squared,
                    squared,
                }
            );
        }

        host::reset();
        assert_eq!(
            normalize_classic_q12_scheduled(Vec3I32::ZERO),
            ClassicNormalizedVector::default()
        );
    }

    #[test]
    fn scheduled_nclip_matches_software_area_on_host() {
        let vertices = [(-320, 112), (47, -91), (511, 230)];
        assert_eq!(screen_area_scheduled(vertices), screen_area(vertices));
    }

    #[test]
    fn scheduled_nclip_face_unpack_matches_packed_record() {
        let vertices = [(-320, 112), (47, -91), (511, 230)];
        let corners = [0xabcd_c123, 0x0123_4567, 0xfedc_89ab];
        assert_eq!(
            screen_area_and_unpack_model_face_scheduled(vertices, corners),
            (
                screen_area(vertices),
                [0xabcd, 0x0123, 0xfedc],
                ((0xc123 >> 14) & 3) as u8,
            ),
        );
    }

    #[test]
    fn cached_average_z_matches_avsz_saturation_rules() {
        assert_eq!(average_z3_ordering_depth([100, 200, 300], 1_365), 199);
        assert_eq!(average_z4_ordering_depth([100, 200, 300, 400], 1_024), 250);
        assert_eq!(average_z3_ordering_depth([u16::MAX; 3], i16::MAX), u16::MAX);
        assert_eq!(average_z4_ordering_depth([u16::MAX; 4], -1), 0);

        set_average_z_weights(1_365, 1_024);
        assert_eq!(average_cached_z3([100, 200, 300]), 199);
        assert_eq!(average_cached_z4([100, 200, 300, 400]), 250);
        assert_eq!(classic_ordering_depth3_from_sum(100 + 200 + 300), 49);

        let vertices = [(-320, 112), (47, -91), (511, 230)];
        assert_eq!(
            screen_area_and_average_cached_z3_scheduled(vertices, [100, 200, 300]),
            (screen_area(vertices), 199),
        );
        assert_eq!(
            screen_area_and_classic_ordering_depth3_scheduled(vertices, [100, 200, 300]),
            (screen_area(vertices), 49),
        );
    }

    #[test]
    fn four_plane_aabb_clip_matches_scalar_support_points() {
        let plane = |normal: [i16; 3], distance| AabbClipPlane {
            normal,
            kind: 0,
            signbits: (normal[0] < 0) as u8
                | (((normal[1] < 0) as u8) << 1)
                | (((normal[2] < 0) as u8) << 2),
            distance,
        };
        let planes = [
            plane([4096, 1024, -512], -3000),
            plane([-2048, 4096, 256], 7000),
            plane([300, -900, 4096], -12000),
            plane([-700, -500, -4096], 16000),
        ];
        load_aabb_clip4(&planes);

        let scalar = |mins: [i16; 3], maxs: [i16; 3], mut flags: u8| {
            for (index, plane) in planes.iter().enumerate() {
                let flag = 1u8 << index;
                if flags & flag == 0 {
                    continue;
                }
                let outer = [
                    if plane.normal[0] < 0 {
                        mins[0]
                    } else {
                        maxs[0]
                    },
                    if plane.normal[1] < 0 {
                        mins[1]
                    } else {
                        maxs[1]
                    },
                    if plane.normal[2] < 0 {
                        mins[2]
                    } else {
                        maxs[2]
                    },
                ];
                let inner = [
                    if plane.normal[0] < 0 {
                        maxs[0]
                    } else {
                        mins[0]
                    },
                    if plane.normal[1] < 0 {
                        maxs[1]
                    } else {
                        mins[1]
                    },
                    if plane.normal[2] < 0 {
                        maxs[2]
                    } else {
                        mins[2]
                    },
                ];
                let dot = |point: [i16; 3]| {
                    plane.normal[0] as i32 * point[0] as i32
                        + plane.normal[1] as i32 * point[1] as i32
                        + plane.normal[2] as i32 * point[2] as i32
                };
                if dot(outer) < plane.distance {
                    return -1;
                }
                if dot(inner) >= plane.distance {
                    flags &= !flag;
                }
            }
            flags as i32
        };

        for (mins, maxs, flags) in [
            ([-10, -20, -30], [40, 50, 60], 0x0f),
            ([100, 200, 300], [120, 240, 360], 0x0f),
            ([-32000, -100, 20], [-30000, 100, 80], 0x05),
        ] {
            let expected = scalar(mins, maxs, flags);
            assert_eq!(classify_aabb_clip4(mins, maxs, &planes, flags), expected);
            assert_eq!(
                is_aabb_outside_clip4(mins, maxs, &planes, flags),
                expected < 0,
            );
        }
        assert_eq!(core::mem::size_of::<AabbClipPlane>(), 12);
        assert_eq!(core::mem::align_of::<AabbClipPlane>(), 4);
    }

    /// FNV-1a 64 of `normalize_classic_q12_scheduled` over 40,000 inputs
    /// spread across every magnitude, including ones whose squares wrap.
    /// Recorded from the implementation that came before the generated
    /// table; the rewrite has to land on the same digest.
    #[test]
    fn classic_normalize_matches_previous_results_digest() {
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed
        };
        let mut hash = 0xCBF2_9CE4_8422_2325u64;
        let mut eat = |value: i32| {
            for byte in value.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
            }
        };
        for i in 0..40_000u32 {
            // Shift each component right by 0..=31 bits so small, medium,
            // large and wrapping magnitudes all show up.
            let mut component = || (next() as i32) >> (next() % 32);
            let input = Vec3I32::new(component(), component(), component());
            let input = if i % 7 == 0 {
                Vec3I32::new(input.x, 0, 0)
            } else {
                input
            };
            host::reset();
            let out = normalize_classic_q12_scheduled(input);
            eat(i32::from(out.vector.x));
            eat(i32::from(out.vector.y));
            eat(i32::from(out.vector.z));
            eat(out.xy_squared);
            eat(out.squared);
        }
        assert_eq!(hash, 0x803B_4A0D_7EA1_0CB5);
    }

    /// The table the previous implementation used, captured by running it.
    const PREVIOUS_TABLE: [i16; 192] = [
        4096, 4064, 4033, 4003, 3973, 3944, 3916, 3888, 3861, 3835, 3809, 3783, 3758, 3734, 3710,
        3686, 3663, 3640, 3618, 3596, 3575, 3554, 3533, 3513, 3493, 3473, 3454, 3435, 3416, 3397,
        3379, 3361, 3344, 3327, 3310, 3293, 3276, 3260, 3244, 3228, 3213, 3197, 3182, 3167, 3153,
        3138, 3124, 3110, 3096, 3082, 3069, 3055, 3042, 3029, 3016, 3003, 2991, 2978, 2966, 2954,
        2942, 2930, 2919, 2907, 2896, 2885, 2873, 2862, 2852, 2841, 2830, 2820, 2809, 2799, 2789,
        2779, 2769, 2759, 2749, 2740, 2730, 2721, 2711, 2702, 2693, 2684, 2675, 2666, 2657, 2649,
        2640, 2631, 2623, 2615, 2606, 2598, 2590, 2582, 2574, 2566, 2558, 2550, 2543, 2535, 2528,
        2520, 2513, 2505, 2498, 2491, 2484, 2477, 2469, 2462, 2456, 2449, 2442, 2435, 2428, 2422,
        2415, 2409, 2402, 2396, 2389, 2383, 2377, 2371, 2364, 2358, 2352, 2346, 2340, 2334, 2328,
        2322, 2317, 2311, 2305, 2299, 2294, 2288, 2283, 2277, 2272, 2266, 2261, 2255, 2250, 2245,
        2239, 2234, 2229, 2224, 2219, 2214, 2209, 2204, 2199, 2194, 2189, 2184, 2179, 2174, 2170,
        2165, 2160, 2155, 2151, 2146, 2142, 2137, 2133, 2128, 2124, 2119, 2115, 2110, 2106, 2102,
        2097, 2093, 2089, 2084, 2080, 2076, 2072, 2068, 2064, 2060, 2056, 2052,
    ];

    #[test]
    fn generated_rsqrt_table_matches_the_previous_table() {
        assert_eq!(RSQRT_Q12_FROM_64, PREVIOUS_TABLE);
    }
}
