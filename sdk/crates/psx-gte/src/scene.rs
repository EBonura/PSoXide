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
                // HWB-010/011 input-commit gap for the final V2 write.
                // RTPT reads V2 as part of this operation; issuing it
                // immediately can consume the previous vertex on silicon.
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
    /// Collect the projected triple. If the GTE is still busy the
    /// first MFC2 interlocks until the op retires, exactly like the
    /// blocking wrapper -- overlapped scalar work between kick and
    /// read is pure profit.
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
            // HWB-010/011 input-commit gap. RTPT consumes V2 after the
            // final MTC2 above, so V0/V1 having ample distance does not make
            // this schedule safe: without these two slots real silicon can
            // project a stale V2. Keep this in sync with project_vertex_mips.
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
    let sum = depths[0] as i64 + depths[1] as i64 + depths[2] as i64;
    ((sum * zsf3 as i64) >> 12).clamp(0, u16::MAX as i64) as u16
}

/// Compute AVSZ4's saturated OTZ result from four cached projected depths.
///
/// This is the four-vertex counterpart to [`average_z3_otz`] and matches the
/// GTE's `AVSZ4` operation with the supplied `ZSF4` value. MIPS render loops
/// should normally prefer [`average_cached_z4`].
#[inline]
pub fn average_z4_otz(depths: [u16; 4], zsf4: i16) -> u16 {
    let sum = depths[0] as i64 + depths[1] as i64 + depths[2] as i64 + depths[3] as i64;
    ((sum * zsf4 as i64) >> 12).clamp(0, u16::MAX as i64) as u16
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
    fn scheduled_nclip_matches_software_area_on_host() {
        let vertices = [(-320, 112), (47, -91), (511, 230)];
        assert_eq!(
            screen_area_mac0_scheduled(vertices),
            screen_area_mac0(vertices)
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
    }
}
