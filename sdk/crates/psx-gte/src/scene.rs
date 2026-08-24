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
use crate::{cfc2, ctc2, mfc2, mtc2};
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

/// Result of the classic PS1 integer-vector normalisation path.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct ClassicNormalizedVector {
    /// Q12 unit vector produced by the GTE reciprocal-table path.
    pub vector: Vec3I16,
    /// Squared integer length of the input X/Y pair.
    pub xy_squared: i32,
    /// Squared integer length of all three input components.
    pub squared: i32,
}

// Reciprocal square-root factors for the normalised squared range 64..=255.
// This is the original PS1 fixed-point table: retaining its exact entries
// makes animation, AI and projectile direction results portable across games.
const CLASSIC_NORMALIZE_TABLE: [i16; 192] = [
    0x1000, 0x0fe0, 0x0fc1, 0x0fa3, 0x0f85, 0x0f68, 0x0f4c, 0x0f30, 0x0f15, 0x0efb, 0x0ee1, 0x0ec7,
    0x0eae, 0x0e96, 0x0e7e, 0x0e66, 0x0e4f, 0x0e38, 0x0e22, 0x0e0c, 0x0df7, 0x0de2, 0x0dcd, 0x0db9,
    0x0da5, 0x0d91, 0x0d7e, 0x0d6b, 0x0d58, 0x0d45, 0x0d33, 0x0d21, 0x0d10, 0x0cff, 0x0cee, 0x0cdd,
    0x0ccc, 0x0cbc, 0x0cac, 0x0c9c, 0x0c8d, 0x0c7d, 0x0c6e, 0x0c5f, 0x0c51, 0x0c42, 0x0c34, 0x0c26,
    0x0c18, 0x0c0a, 0x0bfd, 0x0bef, 0x0be2, 0x0bd5, 0x0bc8, 0x0bbb, 0x0baf, 0x0ba2, 0x0b96, 0x0b8a,
    0x0b7e, 0x0b72, 0x0b67, 0x0b5b, 0x0b50, 0x0b45, 0x0b39, 0x0b2e, 0x0b24, 0x0b19, 0x0b0e, 0x0b04,
    0x0af9, 0x0aef, 0x0ae5, 0x0adb, 0x0ad1, 0x0ac7, 0x0abd, 0x0ab4, 0x0aaa, 0x0aa1, 0x0a97, 0x0a8e,
    0x0a85, 0x0a7c, 0x0a73, 0x0a6a, 0x0a61, 0x0a59, 0x0a50, 0x0a47, 0x0a3f, 0x0a37, 0x0a2e, 0x0a26,
    0x0a1e, 0x0a16, 0x0a0e, 0x0a06, 0x09fe, 0x09f6, 0x09ef, 0x09e7, 0x09e0, 0x09d8, 0x09d1, 0x09c9,
    0x09c2, 0x09bb, 0x09b4, 0x09ad, 0x09a5, 0x099e, 0x0998, 0x0991, 0x098a, 0x0983, 0x097c, 0x0976,
    0x096f, 0x0969, 0x0962, 0x095c, 0x0955, 0x094f, 0x0949, 0x0943, 0x093c, 0x0936, 0x0930, 0x092a,
    0x0924, 0x091e, 0x0918, 0x0912, 0x090d, 0x0907, 0x0901, 0x08fb, 0x08f6, 0x08f0, 0x08eb, 0x08e5,
    0x08e0, 0x08da, 0x08d5, 0x08cf, 0x08ca, 0x08c5, 0x08bf, 0x08ba, 0x08b5, 0x08b0, 0x08ab, 0x08a6,
    0x08a1, 0x089c, 0x0897, 0x0892, 0x088d, 0x0888, 0x0883, 0x087e, 0x087a, 0x0875, 0x0870, 0x086b,
    0x0867, 0x0862, 0x085e, 0x0859, 0x0855, 0x0850, 0x084c, 0x0847, 0x0843, 0x083e, 0x083a, 0x0836,
    0x0831, 0x082d, 0x0829, 0x0824, 0x0820, 0x081c, 0x0818, 0x0814, 0x0810, 0x080c, 0x0808, 0x0804,
];

#[inline(always)]
fn gte_input_commit_gap() {
    #[cfg(target_arch = "mips")]
    unsafe {
        asm!(
            ".word 0",
            ".word 0",
            options(nostack, nomem, preserves_flags),
        );
    }
}

/// Normalise a Q12 `i32` vector through the classic GTE reciprocal-square-root
/// schedule.
///
/// Inputs are truncated to signed integer components before squaring, matching
/// the historical PS1 helper used by Quake-era engines. The output is a Q12
/// unit vector plus the integer X/Y and XYZ squared lengths. This clobbers the
/// GTE IR, MAC and leading-count data registers, but no matrix or projection
/// control state.
#[inline]
pub fn normalize_classic_q12_scheduled(input: Vec3I32) -> ClassicNormalizedVector {
    let x = (input.x >> 12) as i16;
    let y = (input.y >> 12) as i16;
    let z = (input.z >> 12) as i16;

    mtc2!(9, x as i32 as u32);
    mtc2!(10, y as i32 as u32);
    mtc2!(11, z as i32 as u32);
    gte_input_commit_gap();
    // SAFETY: IR1 through IR3 were loaded above and given the silicon-safe
    // input-commit distance before SQR consumes them.
    unsafe { ops::sqr_sf0() };
    let x_squared = mfc2!(25) as i32;
    let y_squared = mfc2!(26) as i32;
    let z_squared = mfc2!(27) as i32;
    let xy_squared = x_squared.wrapping_add(y_squared);
    let squared = xy_squared.wrapping_add(z_squared);
    if squared <= 0 {
        return ClassicNormalizedVector {
            vector: Vec3I16::ZERO,
            xy_squared,
            squared,
        };
    }

    mtc2!(30, squared as u32);
    gte_input_commit_gap();
    let leading = (mfc2!(31) & !1) as i32;
    let normalised_squared = if leading >= 24 {
        squared.wrapping_shl((leading - 24) as u32)
    } else {
        squared >> (24 - leading)
    };
    let table_index = (normalised_squared - 64) as usize;
    let reciprocal = CLASSIC_NORMALIZE_TABLE[table_index];
    let output_shift = (31 - leading) >> 1;

    // Load IR0 first so the three vector writes and the explicit input gap
    // give every operand ample time to commit before GPF reads them.
    mtc2!(8, reciprocal as i32 as u32);
    mtc2!(9, x as i32 as u32);
    mtc2!(10, y as i32 as u32);
    mtc2!(11, z as i32 as u32);
    gte_input_commit_gap();
    // SAFETY: IR0 through IR3 contain the reciprocal/vector product inputs.
    unsafe { ops::gpf_sf0() };
    let nx = (mfc2!(25) as i32 >> output_shift) as i16;
    let ny = (mfc2!(26) as i32 >> output_shift) as i16;
    let nz = (mfc2!(27) as i32 >> output_shift) as i16;

    ClassicNormalizedVector {
        vector: Vec3I16::new(nx, ny, nz),
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
pub fn load_rotation(m: &Mat3I16) {
    ctc2!(0, pack_xy(m.m[0][0], m.m[0][1]));
    ctc2!(1, pack_xy(m.m[0][2], m.m[1][0]));
    ctc2!(2, pack_xy(m.m[1][1], m.m[1][2]));
    ctc2!(3, pack_xy(m.m[2][0], m.m[2][1]));
    ctc2!(4, m.m[2][2] as i32 as u32);
}

/// Load the light-direction matrix (LLM, control 8..=12).
pub fn load_light_matrix(m: &Mat3I16) {
    ctc2!(8, pack_xy(m.m[0][0], m.m[0][1]));
    ctc2!(9, pack_xy(m.m[0][2], m.m[1][0]));
    ctc2!(10, pack_xy(m.m[1][1], m.m[1][2]));
    ctc2!(11, pack_xy(m.m[2][0], m.m[2][1]));
    ctc2!(12, m.m[2][2] as i32 as u32);
}

/// Load four clip-plane normals for [`classify_aabb_clip4`] and
/// [`aabb_outside_clip4`].
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

/// Load the light-colour matrix (LCM, control 16..=20).
pub fn load_light_colour_matrix(m: &Mat3I16) {
    ctc2!(16, pack_xy(m.m[0][0], m.m[0][1]));
    ctc2!(17, pack_xy(m.m[0][2], m.m[1][0]));
    ctc2!(18, pack_xy(m.m[1][1], m.m[1][2]));
    ctc2!(19, pack_xy(m.m[2][0], m.m[2][1]));
    ctc2!(20, m.m[2][2] as i32 as u32);
}

/// Load the translation vector (TR, control 5..=7).
pub fn load_translation(t: Vec3I32) {
    ctc2!(5, t.x as u32);
    ctc2!(6, t.y as u32);
    ctc2!(7, t.z as u32);
}

/// Load the background-colour bias (BK, control 13..=15).
pub fn load_background_colour(c: Vec3I32) {
    ctc2!(13, c.x as u32);
    ctc2!(14, c.y as u32);
    ctc2!(15, c.z as u32);
}

/// Load the far-colour bias (FC, control 21..=23) used by depth-cue
/// interpolation.
pub fn load_far_colour(c: Vec3I32) {
    ctc2!(21, c.x as u32);
    ctc2!(22, c.y as u32);
    ctc2!(23, c.z as u32);
}

/// Set OFX and OFY (control 24, 25) -- the screen-space offsets applied
/// post-divide. Values are 15.16 fixed point; `160 << 16` = 160.0 px.
pub fn set_screen_offset(ofx_15_16: i32, ofy_15_16: i32) {
    ctc2!(24, ofx_15_16 as u32);
    ctc2!(25, ofy_15_16 as u32);
}

/// Set the projection-plane distance H (control 26). Larger H = longer
/// focal length = narrower FOV.
pub fn set_projection_plane(h: u16) {
    ctc2!(26, h as i32 as u32);
}

/// Set the depth-cue coefficients DQA / DQB (control 27, 28).
/// Depth-cue outputs IR0 = DQA/H + DQB, scaled to 0..0x1000.
pub fn set_depth_cue(dqa: i16, dqb: i32) {
    ctc2!(27, dqa as i32 as u32);
    ctc2!(28, dqb as u32);
}

/// Set the AVSZ3/AVSZ4 averaging weights (control 29, 30). Typical
/// values: `ZSF3 = 0x555` (= 1/3 in 0.12), `ZSF4 = 0x400` (= 1/4).
pub fn set_avsz_weights(zsf3: i16, zsf4: i16) {
    ctc2!(29, zsf3 as i32 as u32);
    ctc2!(30, zsf4 as i32 as u32);
}

/// Load `v` into the V0 input slot (data registers 0 and 1) and run
/// RTPS to project it. Returns the screen-space pair + depth so the
/// caller can immediately use the result.
///
/// Assumes the rotation matrix, translation, screen offset, and
/// projection plane have already been set.
pub fn project_vertex(v: Vec3I16) -> Projected {
    mtc2!(0, v.xy_packed());
    mtc2!(1, v.z_packed());
    // SAFETY: V0 has just been loaded; RT / TR / H / OFX / OFY are
    // assumed to be set by the caller's scene setup.
    unsafe { ops::rtps() };
    let sxy = mfc2!(14);
    let sz = mfc2!(19) as u16;
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
pub fn project_triangle(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> [Projected; 3] {
    // Load all three vertices first (data regs 0..=5), then fire RTPT.
    mtc2!(0, v0.xy_packed());
    mtc2!(1, v0.z_packed());
    mtc2!(2, v1.xy_packed());
    mtc2!(3, v1.z_packed());
    mtc2!(4, v2.xy_packed());
    mtc2!(5, v2.z_packed());
    // SAFETY: all three vertices are loaded; scene-setup registers
    // are the caller's responsibility.
    unsafe { ops::rtpt() };
    // After RTPT, SXY FIFO holds (v0, v1, v2) in slots 0/1/2, and
    // SZ FIFO holds them in SZ1/SZ2/SZ3.
    let sxy0 = mfc2!(12);
    let sxy1 = mfc2!(13);
    let sxy2 = mfc2!(14);
    let sz1 = mfc2!(17) as u16;
    let sz2 = mfc2!(18) as u16;
    let sz3 = mfc2!(19) as u16;
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
pub fn transform_vertex(v: Vec3I16) -> Vec3I32 {
    mtc2!(0, v.xy_packed());
    mtc2!(1, v.z_packed());
    // SAFETY: V0 has just been loaded; RT/TR are set by scene setup.
    unsafe { ops::mvmva_rt_v0_tr_sf1() };
    Vec3I32::new(mfc2!(25) as i32, mfc2!(26) as i32, mfc2!(27) as i32)
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

/// In-flight RTPT kicked by [`rtpt_kick`]; [`read`](Self::read)
/// collects the three projected results.
///
/// Between kick and read the caller must not issue any other GTE op
/// or touch GTE registers; arbitrary scalar CPU work is fine and is
/// the whole point -- it runs while the GTE projects. On the host
/// build the projection happens eagerly at kick and `read` just
/// returns it.
#[must_use = "call read() to collect the projected triple"]
pub struct RtptInFlight(#[cfg(not(target_arch = "mips"))] [Projected; 3]);

/// Load V0..V2 and issue RTPT without reading results, so the caller
/// can overlap the GTE op with scalar work for the previous triple.
///
/// The MTC2 order matches [`project_triangle_scheduled`]: V0 is
/// written 6 instructions before RTPT issues and RTPT consumes V2
/// last, so the HWB-010/011 commit-slip hazard profile is unchanged.
#[inline(always)]
pub fn rtpt_kick(v0: Vec3I16, v1: Vec3I16, v2: Vec3I16) -> RtptInFlight {
    #[cfg(target_arch = "mips")]
    {
        let v0_xy = v0.xy_packed();
        let v0_z = v0.z_packed();
        let v1_xy = v1.xy_packed();
        let v1_z = v1.z_packed();
        let v2_xy = v2.xy_packed();
        let v2_z = v2.z_packed();
        unsafe {
            asm!(
                // MTC2 $8..$13 into V0/V1/V2 input registers.
                ".word 0x48880000",
                ".word 0x48890800",
                ".word 0x488a1000",
                ".word 0x488b1800",
                ".word 0x488c2000",
                ".word 0x488d2800",
                // Conservative HWB-010/011 input-commit gap for the final V2
                // write. The original console failure proved the rule for
                // MVMVA/RTPS, not RTPT; hardware-tests v1.20 records 0xC0-C3
                // explicitly arbitrate whether RTPT needs these two slots.
                ".word 0",
                ".word 0",
                // RTPT.
                ".word 0x4a080030",
                in("$8") v0_xy,
                in("$9") v0_z,
                in("$10") v1_xy,
                in("$11") v1_z,
                in("$12") v2_xy,
                in("$13") v2_z,
                options(nostack, nomem, preserves_flags),
            );
        }
        RtptInFlight()
    }
    #[cfg(not(target_arch = "mips"))]
    {
        RtptInFlight(project_triangle(v0, v1, v2))
    }
}

impl RtptInFlight {
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
            unsafe {
                asm!(
                    // Read SXY0/SXY1/SXY2/SZ1/SZ2/SZ3; each MFC2's
                    // load-delay slot is filled by the next, so only
                    // the final read needs the explicit NOP.
                    ".word 0x48086000",
                    ".word 0x48096800",
                    ".word 0x480a7000",
                    ".word 0x480b8800",
                    ".word 0x480c9000",
                    ".word 0x480d9800",
                    ".word 0",
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
    unsafe {
        asm!(
            // MTC2 $8,VXY0 and $9,VZ0.
            ".word 0x48880000",
            ".word 0x48890800",
            // HWB-010/011 input-commit gap. The single-vertex path used to
            // issue RTPS immediately here, leaving VXY0 at the same two-tick
            // distance as the console-confirmed MVMVA vertex-explosion case.
            // Two NOPs move it to the known-safe four-instruction distance.
            ".word 0",
            ".word 0",
            // RTPS.
            ".word 0x4a080001",
            // Read SXY2 and SZ3. Two MFC2s can share one final
            // load-delay NOP instead of one NOP per wrapper call.
            ".word 0x48087000",
            ".word 0x48099800",
            ".word 0",
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
    unsafe {
        asm!(
            // MTC2 $8..$13 into V0/V1/V2 input registers.
            ".word 0x48880000",
            ".word 0x48890800",
            ".word 0x488a1000",
            ".word 0x488b1800",
            ".word 0x488c2000",
            ".word 0x488d2800",
            // Conservative HWB-010/011 input-commit gap. The original
            // console failure proved MVMVA/RTPS; hardware-tests v1.20 records
            // 0xC0-C3 measure RTPT directly before we remove these slots.
            // Keep this in sync with rtpt_kick.
            ".word 0",
            ".word 0",
            // RTPT.
            ".word 0x4a080030",
            // Read SXY0/SXY1/SXY2/SZ1/SZ2/SZ3. Each MFC2's
            // load-delay slot is filled by the next MFC2, so only the
            // final read needs an explicit NOP before Rust observes
            // the output registers.
            ".word 0x48086000",
            ".word 0x48096800",
            ".word 0x480a7000",
            ".word 0x480b8800",
            ".word 0x480c9000",
            ".word 0x480d9800",
            ".word 0",
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
    unsafe {
        asm!(
            // MTC2 $8,VXY0 and $9,VZ0.
            ".word 0x48880000",
            ".word 0x48890800",
            // HWB-010/011 hazard gap (console-confirmed fix): two buffer
            // NOPs push the VXY0 write's commit distance from 2 to 4
            // instructions. Without them, real silicon can commit the
            // write BETWEEN the MVMVA's sequential MAC1 and MAC2 compute
            // phases, so MAC1 reads the previous V0.x -- the cortex
            // vertex-explosion mechanism seen in the HWB-010 live capture.
            ".word 0",
            ".word 0",
            // MVMVA RT,V0,TR,sf=1.
            ".word 0x4a080012",
            // Read MAC1/MAC2/MAC3. Consecutive MFC2 instructions fill
            // each other's load-delay slot; only the final read needs
            // an explicit NOP before Rust observes the outputs.
            ".word 0x4808c800",
            ".word 0x4809d000",
            ".word 0x480ad800",
            ".word 0",
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
        unsafe {
            asm!(
                // Live schedule, byte-identical to transform_vertex_mips:
                // MTC2 $8,VXY0 / MTC2 $9,VZ0 / MVMVA / MAC1,2,3 reads.
                ".word 0x48880000",
                ".word 0x48890800",
                ".word 0x4a080012",
                ".word 0x4808c800",
                ".word 0x4809d000",
                ".word 0x480ad800",
                ".word 0",
                // Probe tail, strictly AFTER the live reads: 4-NOP
                // settle gap, MAC1 re-read ($11), then TRX/TRY/TRZ
                // (cr5..cr7) read-back; chained CFC2s share delay
                // slots, final NOP covers the last one.
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0x480bc800",
                ".word 0x484c2800",
                ".word 0x484d3000",
                ".word 0x484e3800",
                ".word 0",
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
            tr: [cfc2!(5) as i32, cfc2!(6) as i32, cfc2!(7) as i32],
        }
    }
}

/// Read the last three projected Z values and compute their average
/// via AVSZ3 (weighted by ZSF3). Returns OTZ -- the depth key most
/// renderers use for ordering-table inserts.
pub fn average_z_triangle() -> u16 {
    // SAFETY: no input registers to prepare -- AVSZ3 reads SZ1..SZ3
    // which were populated by the most recent RTPT / project_triangle.
    unsafe { ops::avsz3() };
    mfc2!(7) as u16
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
        unsafe {
            asm!(
                // Load SZ1..SZ3, then leave the hardware-safe two-slot MTC2
                // commit gap before AVSZ3 consumes the final write.
                ".word 0x48888800",
                ".word 0x48899000",
                ".word 0x488a9800",
                ".word 0",
                ".word 0",
                ".word 0x4a00002d",
                // MFC2 has one CPU load-delay slot.
                ".word 0x48083800",
                ".word 0",
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
        mtc2!(17, depths[0] as u32);
        mtc2!(18, depths[1] as u32);
        mtc2!(19, depths[2] as u32);
        // SAFETY: SZ1, SZ2 and SZ3 were loaded immediately above.
        unsafe { ops::avsz3() };
        mfc2!(7) as u16
    }
}

/// Scale a three-depth sum into the classic 2,048-slot OTZ domain.
///
/// This is exactly `(sum * 0x155) >> 12`. The MIPS sequence factors 0x155 as
/// `5 * 17 * 4 + 1`, avoiding two shifts and additions emitted for the flat
/// constant multiply on MIPS I.
#[inline(always)]
pub fn classic_otz3_from_sum(sum: u32) -> u16 {
    #[cfg(target_arch = "mips")]
    {
        let mut otz = sum;
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
        unsafe {
            asm!(
                // Load SZ0..SZ3, then leave the hardware-safe two-slot MTC2
                // commit gap before AVSZ4 consumes the final write.
                ".word 0x48888000",
                ".word 0x48898800",
                ".word 0x488a9000",
                ".word 0x488b9800",
                ".word 0",
                ".word 0",
                ".word 0x4a00002e",
                // MFC2 has one CPU load-delay slot.
                ".word 0x48083800",
                ".word 0",
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
        mtc2!(16, depths[0] as u32);
        mtc2!(17, depths[1] as u32);
        mtc2!(18, depths[2] as u32);
        mtc2!(19, depths[3] as u32);
        // SAFETY: SZ0 through SZ3 were loaded immediately above.
        unsafe { ops::avsz4() };
        mfc2!(7) as u16
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
pub fn average_z3_otz(depths: [u16; 3], zsf3: i16) -> u16 {
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
/// This is the four-vertex counterpart to [`average_z3_otz`] and matches the
/// GTE's `AVSZ4` operation with the supplied `ZSF4` value. MIPS render loops
/// should normally prefer [`average_cached_z4`].
#[inline]
pub fn average_z4_otz(depths: [u16; 4], zsf4: i16) -> u16 {
    // psx-numeric-allow-next-line: AVSZ4 wide accumulator, same reasoning as average_z3_otz
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

#[cfg(target_arch = "mips")]
#[inline(always)]
fn aabb_dot_mvmva<const OP: u32, const READ_MAC: u32>(v: Vec3I16) -> i32 {
    let mut dot = v.xy_packed();
    unsafe {
        asm!(
            ".word 0x48880000",
            ".word 0x48890800",
            // Same console-confirmed V0 commit distance as
            // transform_vertex_mips.
            ".word 0",
            ".word 0",
            ".word {op}",
            ".word {read_mac}",
            ".word 0",
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
            0 => aabb_dot_mvmva::<0x4a00_6012, 0x4808_c800>(v),
            1 => aabb_dot_mvmva::<0x4a00_6012, 0x4808_d000>(v),
            2 => aabb_dot_mvmva::<0x4a00_6012, 0x4808_d800>(v),
            3 => aabb_dot_mvmva::<0x4a02_6012, 0x4808_c800>(v),
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
pub fn aabb_outside_clip4(
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
pub fn screen_area_mac0(vertices: [(i16, i16); 3]) -> i32 {
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
/// by the console measurements documented on [`screen_area_mac0`]. This is
/// useful in tight indexed-model loops where the GTE would otherwise be idle.
#[inline(always)]
pub fn screen_area_mac0_scheduled(vertices: [(i16, i16); 3]) -> i32 {
    #[cfg(target_arch = "mips")]
    {
        let sxy0 = pack_xy(vertices[0].0, vertices[0].1);
        let sxy1 = pack_xy(vertices[1].0, vertices[1].1);
        let mut area = pack_xy(vertices[2].0, vertices[2].1);
        unsafe {
            asm!(
                // MTC2 $8/$9/$10,SXY0/SXY1/SXY2.
                ".word 0x48886000",
                ".word 0x48896800",
                ".word 0x488a7000",
                // HWB-010/011 input-commit gap before NCLIP consumes SXY2.
                ".word 0",
                ".word 0",
                // NCLIP plus its hardware-confirmed MAC0 result gap.
                ".word 0x4a000006",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                // MFC2 $10,MAC0 plus its CPU load-delay slot.
                ".word 0x480ac000",
                ".word 0",
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
        screen_area_mac0(vertices)
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
        unsafe {
            asm!(
                // MTC2 SXY0..SXY2 and the hardware-confirmed input gap.
                ".word 0x48886000",
                ".word 0x48896800",
                ".word 0x488a7000",
                ".word 0",
                ".word 0",
                ".word 0x4a000006",
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
                ".word 0x480ac000",
                ".word 0",
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
            screen_area_mac0(vertices),
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
/// schedules. The returned area is identical to [`screen_area_mac0`], and the
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
        unsafe {
            asm!(
                // Load SXY0..SXY2 and leave the measured input-commit gap.
                ".word 0x48886000",
                ".word 0x48896800",
                ".word 0x488a7000",
                ".word 0",
                ".word 0",
                ".word 0x4a000006",
                // Loading SZ1..SZ3 is independent work inside NCLIP's
                // hardware-confirmed eight-instruction MAC0 result gap.
                ".word 0x488b8800",
                ".word 0x488c9000",
                ".word 0x488d9800",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                ".word 0",
                // Capture NCLIP's MAC0 before AVSZ3 overwrites it. AVSZ3 is
                // independent of the CPU load-delay result.
                ".word 0x480ac000",
                ".word 0x4a00002d",
                ".word 0x480b3800",
                ".word 0",
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
        (screen_area_mac0(vertices), average_cached_z3(depths))
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
pub fn screen_area_and_classic_otz3_scheduled(
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
        unsafe {
            asm!(
                // Load SXY0..SXY2 and leave the measured input-commit gap.
                ".word 0x48886000",
                ".word 0x48896800",
                ".word 0x488a7000",
                ".word 0",
                ".word 0",
                // NCLIP.
                ".word 0x4a000006",
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
                ".word 0x480ac000",
                "srl $11, $12, 12",
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
        (screen_area_mac0(vertices), ((sum * 0x155) >> 12) as u16)
    }
}

/// Back-face test for three already-projected screen-space vertices.
///
/// Useful when a renderer cached/projected vertices first and later wants
/// the signed screen-space area test for arbitrary indexed faces. Uses the
/// software [`screen_area_mac0`] (see its note on the NCLIP MAC0 hazard).
pub fn screen_triangle_back_facing(vertices: [(i16, i16); 3]) -> bool {
    screen_area_mac0(vertices) <= 0
}

/// Read the GTE FLAG register. Non-zero indicates at least one error
/// bit fired during the last op (overflow, saturation, divide
/// overflow). Useful for debug prints on a frame that looks wrong.
pub fn read_flag() -> u32 {
    cfc2!(31)
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
        assert_eq!(
            screen_area_mac0_scheduled(vertices),
            screen_area_mac0(vertices)
        );
    }

    #[test]
    fn scheduled_nclip_face_unpack_matches_packed_record() {
        let vertices = [(-320, 112), (47, -91), (511, 230)];
        let corners = [0xabcd_c123, 0x0123_4567, 0xfedc_89ab];
        assert_eq!(
            screen_area_and_unpack_model_face_scheduled(vertices, corners),
            (
                screen_area_mac0(vertices),
                [0xabcd, 0x0123, 0xfedc],
                ((0xc123 >> 14) & 3) as u8,
            ),
        );
    }

    #[test]
    fn cached_average_z_matches_avsz_saturation_rules() {
        assert_eq!(average_z3_otz([100, 200, 300], 1_365), 199);
        assert_eq!(average_z4_otz([100, 200, 300, 400], 1_024), 250);
        assert_eq!(average_z3_otz([u16::MAX; 3], i16::MAX), u16::MAX);
        assert_eq!(average_z4_otz([u16::MAX; 4], -1), 0);

        set_avsz_weights(1_365, 1_024);
        assert_eq!(average_cached_z3([100, 200, 300]), 199);
        assert_eq!(average_cached_z4([100, 200, 300, 400]), 250);
        assert_eq!(classic_otz3_from_sum(100 + 200 + 300), 49);

        let vertices = [(-320, 112), (47, -91), (511, 230)];
        assert_eq!(
            screen_area_and_average_cached_z3_scheduled(vertices, [100, 200, 300]),
            (screen_area_mac0(vertices), 199),
        );
        assert_eq!(
            screen_area_and_classic_otz3_scheduled(vertices, [100, 200, 300]),
            (screen_area_mac0(vertices), 49),
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
            assert_eq!(aabb_outside_clip4(mins, maxs, &planes, flags), expected < 0,);
        }
        assert_eq!(core::mem::size_of::<AabbClipPlane>(), 12);
        assert_eq!(core::mem::align_of::<AabbClipPlane>(), 4);
    }
}
