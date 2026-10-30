//! GTE helpers for posed HMD8 models, shared by Half-Life and Counter-Strike.
//!
//! Both ports compose one bone's model-space affine with the camera matrix on
//! the GTE and feed packed HMD8 vertex triples straight to RTPT. The bodies
//! are the ones both games carried as identical private copies; they stay
//! register-for-register the same so moving them changes no output.

use psx_asset::hmd8::{BoneTransform, GteVertexWords};
use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32};
use psx_gte::scene::{self, Projected};

/// Compose one HMD8 model-space bone affine with the already-oriented camera
/// matrix. Four hazard-safe GTE MVMVA operations replace 27 slow R3000 scalar
/// multiplies and leave the GTE ready for the bone range's RTPT batches.
///
/// Leaves the GTE rotation matrix set to `view_model` and the translation
/// vector zero; the caller reloads whatever it needs next.
#[inline(never)]
pub fn compose_bone_gte(
    view_model: &Mat3I16,
    view_origin: Vec3I32,
    bone: BoneTransform,
) -> (Mat3I16, Vec3I32) {
    scene::load_rotation(view_model);
    scene::load_translation(Vec3I32::ZERO);
    let c0 = scene::transform_vertex_scheduled(Vec3I16::new(
        bone.rotation.m[0][0],
        bone.rotation.m[1][0],
        bone.rotation.m[2][0],
    ));
    let c1 = scene::transform_vertex_scheduled(Vec3I16::new(
        bone.rotation.m[0][1],
        bone.rotation.m[1][1],
        bone.rotation.m[2][1],
    ));
    let c2 = scene::transform_vertex_scheduled(Vec3I16::new(
        bone.rotation.m[0][2],
        bone.rotation.m[1][2],
        bone.rotation.m[2][2],
    ));
    let bt = scene::transform_vertex_scheduled(bone.translation);
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    (
        Mat3I16 {
            m: [
                [clamp(c0.x), clamp(c1.x), clamp(c2.x)],
                [clamp(c0.y), clamp(c1.y), clamp(c2.y)],
                [clamp(c0.z), clamp(c1.z), clamp(c2.z)],
            ],
        },
        Vec3I32::new(
            view_origin.x.saturating_add(bt.x),
            view_origin.y.saturating_add(bt.y),
            view_origin.z.saturating_add(bt.z),
        ),
    )
}

/// Feed one already-packed HMD8 triple straight to V0/V1/V2.
///
/// `project_triangle_scheduled` is ideal for general `Vec3I16` callers,
/// but HMD8 vertices already live in the GTE's packed XY + sign-extended Z
/// layout. Keeping that representation removes the repeated halfword
/// shifts/ORs from every animated-model RTPT batch while preserving the
/// hardware-verified MTC2/RTPT/MFC2 schedule byte-for-byte.
#[inline(always)]
pub fn project_triangle_words(
    v0: GteVertexWords,
    v1: GteVertexWords,
    v2: GteVertexWords,
) -> [Projected; 3] {
    #[cfg(target_arch = "mips")]
    {
        let sxy0: u32;
        let sxy1: u32;
        let sxy2: u32;
        let sz1: u32;
        let sz2: u32;
        let sz3: u32;
        // SAFETY: the words are the hardware-verified MTC2 V0..V2, RTPT and
        // MFC2 SXY/SZ schedule `project_triangle_scheduled` uses, with the
        // two-NOP gap before the first read; only `$8..$13` are written.
        unsafe {
            core::arch::asm!(
                ".word 0x48880000",
                ".word 0x48890800",
                ".word 0x488a1000",
                ".word 0x488b1800",
                ".word 0x488c2000",
                ".word 0x488d2800",
                ".word 0x4a080030",
                ".word 0x48086000",
                ".word 0x48096800",
                ".word 0x480a7000",
                ".word 0x480b8800",
                ".word 0x480c9000",
                ".word 0x480d9800",
                ".word 0",
                inlateout("$8") v0.xy => sxy0,
                inlateout("$9") v0.z => sxy1,
                inlateout("$10") v1.xy => sxy2,
                inlateout("$11") v1.z => sz1,
                inlateout("$12") v2.xy => sz2,
                inlateout("$13") v2.z => sz3,
                options(nostack, nomem, preserves_flags),
            );
        }
        return [
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
        ];
    }
    #[cfg(not(target_arch = "mips"))]
    {
        let unpack = |v: GteVertexWords| Vec3I16::new(v.xy as i16, (v.xy >> 16) as i16, v.z as i16);
        scene::project_triangle_scheduled(unpack(v0), unpack(v1), unpack(v2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix(seed: i32) -> Mat3I16 {
        let mut m = [[0i16; 3]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                let v = (seed * 977 + (r as i32 * 3 + c as i32) * 1523) % 4097;
                *cell = (v - 2048) as i16;
            }
        }
        Mat3I16 { m }
    }

    /// The GTE compose is the Q12 matrix product with i16 saturation and a
    /// saturating translation add, the formula the games' scalar code used.
    #[test]
    fn compose_matches_the_scalar_product() {
        for seed in 0..64 {
            let view = matrix(seed);
            let bone = BoneTransform {
                rotation: matrix(seed + 101),
                translation: Vec3I16::new(
                    (seed * 311 % 3001 - 1500) as i16,
                    (seed * 97 % 2001 - 1000) as i16,
                    (seed * 13 % 4001 - 2000) as i16,
                ),
            };
            let origin = Vec3I32::new(seed * 40_000, -seed * 25_000, i32::MAX - seed);
            let (rot, pos) = compose_bone_gte(&view, origin, bone);
            assert_eq!(rot.m, view.mul(&bone.rotation).m, "rotation, seed {seed}");
            let t = view.transform(bone.translation);
            assert_eq!(pos.x, origin.x.saturating_add(t[0]), "x, seed {seed}");
            assert_eq!(pos.y, origin.y.saturating_add(t[1]), "y, seed {seed}");
            assert_eq!(pos.z, origin.z.saturating_add(t[2]), "z, seed {seed}");
        }
    }

    /// The packed-word path agrees with the general `Vec3I16` projection.
    #[test]
    fn packed_words_match_the_general_projection() {
        scene::load_rotation(&Mat3I16::IDENTITY);
        scene::load_translation(Vec3I32::new(0, 0, 600));
        scene::set_projection_plane(400);
        let words = |x: i16, y: i16, z: i16| GteVertexWords {
            xy: (x as u16 as u32) | ((y as u16 as u32) << 16),
            z: z as i32 as u32,
        };
        let tri = [words(-90, 30, 20), words(120, -45, -35), words(5, 77, 120)];
        let got = project_triangle_words(tri[0], tri[1], tri[2]);
        let want = scene::project_triangle_scheduled(
            Vec3I16::new(-90, 30, 20),
            Vec3I16::new(120, -45, -35),
            Vec3I16::new(5, 77, 120),
        );
        for i in 0..3 {
            assert_eq!(
                (got[i].sx, got[i].sy, got[i].sz),
                (want[i].sx, want[i].sy, want[i].sz)
            );
        }
    }
}
