//! Frustum planes of the grid renderer and their GTE evaluation.

use psx_gte::math::{Mat3I16, Vec3I16, Vec3I32 as GteVec3I32};
#[cfg(target_arch = "mips")]
use psx_gte::scene;
use psx_math::int32::isqrt_i32;

use super::projection::ViewProjection;

/// All five clip planes still need testing.
pub const CLIP_ALL_PLANES: u8 = 0x1f;
/// Index of the near plane in [`FrustumPlanes::planes`].
const CLIP_NEAR_PLANE: usize = 0;

/// Frustum planes in the space the loaded GTE view transforms from (room
/// local for the grid renderer): `dot(normal, p) + offset >= 0` keeps `p`.
/// Normals are the view rows combined per plane (Q12), offsets are in the
/// same Q12 domain.
///
/// The GPU does not clip: a quad with a vertex behind the camera projects to
/// garbage or to a primitive beyond the 1023x511 GPU limit and is dropped
/// whole, which blacks out entire rooms when the third-person camera is
/// cornered. Clipping the polygon before projection keeps every emitted
/// vertex inside the frustum, so nothing the GPU receives can be oversize.
#[derive(Copy, Clone, Debug)]
pub struct FrustumPlanes {
    /// Plane normal and constant term, `distance(p) = n . p + c`, all `i32`.
    ///
    /// The near plane is exact: a Q12 view row against `i16` positions and
    /// an `i16`-range camera origin fits `i32`. The side planes carry the
    /// focal length and half-extent factors, so their normals are rounded
    /// down by `side_shift` bits to fit; [`Self::side_error`] bounds the
    /// rounding error of any side distance and every classification below
    /// treats that band as "undecided" so a rounded plane can neither reject
    /// a visible polygon nor vouch for a box it does not contain.
    pub planes: [([i32; 3], i32); 5],
    /// Worst-case rounding error of a side-plane distance (zero for near).
    pub side_error: i32,
    /// Bit `3 * plane + axis` set when that plane normal's component is
    /// negative, so a box's farthest corner along the normal takes its
    /// minimum on that axis. Held in one register through the node walk
    /// instead of six normal loads per plane per node.
    outer_bits: u16,
}

impl FrustumPlanes {
    /// Build from the view rotation/translation currently loaded in the GTE.
    /// The view may carry a uniform scale (the world pass loads x3); the near distance
    /// is scaled by the row length so `near_world` stays in world units.
    pub fn from_view(
        rotation: &Mat3I16,
        translation: GteVec3I32,
        projection: ViewProjection,
    ) -> Self {
        let row = |i: usize| -> [i32; 3] {
            [
                rotation.m[i][0] as i32,
                rotation.m[i][1] as i32,
                rotation.m[i][2] as i32,
            ]
        };
        let (right, down, forward) = (row(0), row(1), row(2));
        // Squared Q12 row length: three products of at most 3 * 4096 each,
        // under 4.6e8, so the 32-bit square root answers it.
        let scale_q12 =
            isqrt_i32(forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2]);
        let near_view = projection.near_world.saturating_mul(scale_q12) >> 12;
        let far_view = if projection.far_world == i32::MAX {
            i32::MAX
        } else {
            projection.far_world.saturating_mul(scale_q12) >> 12
        };
        // A finite far cull fades colours to black over its last 3/8, so
        // node boxes popping out at the cull are already dark.
        if far_view == i32::MAX {
            psx_engine::affine_surface::clear_depth_fade();
        } else {
            let far = far_view.max(1) as u32;
            psx_engine::affine_surface::set_depth_fade(far * 5 / 8, far);
        }
        // The view translation is the rotated, negated camera origin in world
        // units, so `T.z << 12` is `-forward . camera` and every distance is
        // camera-relative: a Q12 row against an `i16` position stays under
        // 7e8, and the near constant under 2^30, so the near plane is exact
        // in `i32` and bit-identical to the former i64 evaluation.
        let near = (forward, (translation.z << 12).wrapping_sub(near_view << 12));
        // side planes: |view_x| <= (half_w + margin)/H * view_z, same for y.
        let kx = projection.half_width + projection.edge_margin;
        let ky = projection.half_height + projection.edge_margin;
        let h = projection.focal_length.max(1);
        // Largest normal L1 that keeps `n . q` inside i32 for |q| < 65536 with
        // room for the constant term.
        const SIDE_L1_LIMIT: i32 = 30_000;
        let mut side_shift = 0u32;
        let raw_side = |axis: [i32; 3], k: i32, sign: i32| -> [i32; 3] {
            [
                k * forward[0] - sign * h * axis[0],
                k * forward[1] - sign * h * axis[1],
                k * forward[2] - sign * h * axis[2],
            ]
        };
        let raw = [
            raw_side(right, kx, 1),
            raw_side(right, kx, -1),
            raw_side(down, ky, 1),
            raw_side(down, ky, -1),
        ];
        for n in &raw {
            let l1 = n[0].unsigned_abs() + n[1].unsigned_abs() + n[2].unsigned_abs();
            while (l1 >> side_shift) > SIDE_L1_LIMIT as u32 {
                side_shift += 1;
            }
        }
        let round = |value: i32| -> i32 {
            if side_shift == 0 {
                value
            } else {
                (value + (1 << (side_shift - 1))) >> side_shift
            }
        };
        // The side constant is `(k * T.z - sign * h * T.axis) << 12` reduced
        // by the same shift, exact because the shift never exceeds twelve.
        let side = |n: [i32; 3], axis_t: i32, k: i32, sign: i32| -> ([i32; 3], i32) {
            let n = [round(n[0]), round(n[1]), round(n[2])];
            let constant = k
                .wrapping_mul(translation.z)
                .wrapping_sub(sign.wrapping_mul(h).wrapping_mul(axis_t));
            let constant = if side_shift <= 12 {
                constant << (12 - side_shift)
            } else {
                constant >> (side_shift - 12)
            };
            (n, constant)
        };
        // Each rounded component is off by at most one half; the distance of a
        // point up to 65535 units away on every axis is off by at most that
        // times three halves. Zero when nothing was rounded.
        let side_error = if side_shift == 0 { 0 } else { 3 * 65_536 / 2 };
        let planes = [
            near,
            side(raw[0], translation.x, kx, 1),
            side(raw[1], translation.x, kx, -1),
            side(raw[2], translation.y, ky, 1),
            side(raw[3], translation.y, ky, -1),
        ];
        let mut outer_bits = 0u16;
        for (index, plane) in planes.iter().enumerate() {
            for axis in 0..3 {
                if plane.0[axis] < 0 {
                    outer_bits |= 1 << (3 * index + axis);
                }
            }
        }
        Self {
            planes,
            side_error,
            outer_bits,
        }
    }
    /// Planes for a camera at the origin: build once per frame from the view
    /// rotation and test world boxes with [`Self::cull_world_aabb`]. The
    /// camera position never enters the planes, so nothing about them
    /// depends on how far the eye is from the world origin.
    pub fn at_eye(rotation: &Mat3I16, projection: ViewProjection) -> Self {
        Self::from_view(rotation, GteVec3I32::ZERO, projection)
    }

    /// [`Self::cull_aabb`] for a world-space box, with `self` built by
    /// [`Self::at_eye`]: the box is moved to be eye-relative before the
    /// `i16` conversion, so worlds past 32767 units from the origin cull
    /// exactly like worlds near it. A box more than 32767 units from the eye
    /// along an axis is clamped to that distance, still far outside any
    /// draw distance.
    pub fn cull_world_aabb(
        &self,
        eye: [i32; 3],
        min: [i32; 3],
        max: [i32; 3],
        mask: u8,
    ) -> Option<u8> {
        let rel = |v: i32, axis: usize| {
            v.wrapping_sub(eye[axis])
                .max(i32::from(i16::MIN))
                .min(i32::from(i16::MAX)) as i16
        };
        self.cull_aabb(
            Vec3I16 {
                x: rel(min[0], 0),
                y: rel(min[1], 1),
                z: rel(min[2], 2),
            },
            Vec3I16 {
                x: rel(max[0], 0),
                y: rel(max[1], 1),
                z: rel(max[2], 2),
            },
            mask,
        )
    }

    /// Distance of `position` from plane `index`: the dot on the GTE on the
    /// guest (the planes must be loaded by [`load_gte_clip_planes`]), the
    /// same wrapping `i32` products on the host.
    #[inline(always)]
    fn distance_at(&self, index: usize, position: [i16; 3]) -> i32 {
        #[cfg(target_arch = "mips")]
        {
            let xy = (position[0] as u16 as u32) | ((position[1] as u16 as u32) << 16);
            let z = position[2] as i32 as u32;
            // One MVMVA yields three plane dots but a box corner is chosen
            // for one plane, so only that MAC is read back.
            let dot = unsafe {
                match index {
                    0 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC1>(xy, z),
                    1 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC2>(xy, z),
                    2 => gte_dot_one::<MVMVA_LLM_V0_SF0, MFC2_MAC3>(xy, z),
                    3 => gte_dot_one::<MVMVA_LCM_V0_SF0, MFC2_MAC1>(xy, z),
                    _ => gte_dot_one::<MVMVA_LCM_V0_SF0, MFC2_MAC2>(xy, z),
                }
            };
            dot.wrapping_add(self.planes[index].1)
        }
        #[cfg(not(target_arch = "mips"))]
        {
            Self::distance(&self.planes[index], position)
        }
    }
    /// Signed distance of `position` from `plane`; negative is outside.
    #[inline(always)]
    pub fn distance(plane: &([i32; 3], i32), position: [i16; 3]) -> i32 {
        plane.0[0]
            .wrapping_mul(position[0] as i32)
            .wrapping_add(plane.0[1].wrapping_mul(position[1] as i32))
            .wrapping_add(plane.0[2].wrapping_mul(position[2] as i32))
            .wrapping_add(plane.1)
    }
    /// Hierarchical frustum test of one node box against the live clip planes.
    ///
    /// `mask` names the planes a caller still has to care about; the rest were
    /// already proven satisfied by an ancestor box. Returns `None` when the box
    /// is wholly outside one of the masked planes, otherwise the residual mask
    /// for the subtree, with every plane this box proves cleared.
    ///
    /// Unrolled per plane so every plane index is a constant: the runtime
    /// loop form dispatched the GTE read through a jump table and reloaded
    /// three normal components from RAM for each corner, per plane, per
    /// node. The corner selectors come from [`Self::outer_bits`].
    pub fn cull_aabb(&self, mins: Vec3I16, maxs: Vec3I16, mask: u8) -> Option<u8> {
        let lo = [mins.x, mins.y, mins.z];
        let hi = [maxs.x, maxs.y, maxs.z];
        let bits = self.outer_bits;
        let side_error = self.side_error;
        let mut residual = mask;
        macro_rules! plane {
            ($index:literal) => {
                if mask & (1 << $index) != 0 {
                    let outer = [
                        if bits & (1 << (3 * $index)) != 0 {
                            lo[0]
                        } else {
                            hi[0]
                        },
                        if bits & (1 << (3 * $index + 1)) != 0 {
                            lo[1]
                        } else {
                            hi[1]
                        },
                        if bits & (1 << (3 * $index + 2)) != 0 {
                            lo[2]
                        } else {
                            hi[2]
                        },
                    ];
                    let error = if $index == CLIP_NEAR_PLANE {
                        0
                    } else {
                        side_error
                    };
                    if self.distance_at($index, outer) < -error {
                        return None;
                    }
                    let inner = [
                        if bits & (1 << (3 * $index)) != 0 {
                            hi[0]
                        } else {
                            lo[0]
                        },
                        if bits & (1 << (3 * $index + 1)) != 0 {
                            hi[1]
                        } else {
                            lo[1]
                        },
                        if bits & (1 << (3 * $index + 2)) != 0 {
                            hi[2]
                        } else {
                            lo[2]
                        },
                    ];
                    if self.distance_at($index, inner) >= error {
                        residual &= !(1 << $index);
                    }
                }
            };
        }
        plane!(0);
        plane!(1);
        plane!(2);
        plane!(3);
        plane!(4);
        Some(residual)
    }
}

#[cfg(target_arch = "mips")]
const MVMVA_LLM_V0_SF0: u32 = 0x4A02_6012;
/// `MVMVA(mx=LCM, vx=V0, cv=none, sf=0, lm=0)`.
#[cfg(target_arch = "mips")]
const MVMVA_LCM_V0_SF0: u32 = 0x4A04_6012;

/// Load the five frustum planes for the cell cull:
/// planes 0..3 fill the light matrix rows, planes 3..5 the first two colour
/// matrix rows. Both matrices are free during the world pass (the batch
/// writer only uses the rotation and translation), and every later lit
/// submission reloads them.
#[cfg(target_arch = "mips")]
#[inline(never)]
pub fn load_gte_clip_planes(planes: &[([i32; 3], i32); 5]) {
    #[inline(always)]
    fn row(plane: &([i32; 3], i32)) -> [i16; 3] {
        // Near rows are Q12 view rows, side rows are rounded to an L1 norm
        // of at most SIDE_L1_LIMIT, so every component fits.
        debug_assert!(plane
            .0
            .iter()
            .all(|c| (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(c)));
        [plane.0[0] as i16, plane.0[1] as i16, plane.0[2] as i16]
    }
    scene::load_light_matrix(&Mat3I16 {
        m: [row(&planes[0]), row(&planes[1]), row(&planes[2])],
    });
    scene::load_light_color_matrix(&Mat3I16 {
        m: [row(&planes[3]), row(&planes[4]), [0; 3]],
    });
}

/// `mfc2 $8, MAC1..MAC3` encodings for [`gte_dot_one`].
#[cfg(target_arch = "mips")]
const MFC2_MAC1: u32 = 0x4808_c800;
#[cfg(target_arch = "mips")]
const MFC2_MAC2: u32 = 0x4808_d000;
#[cfg(target_arch = "mips")]
const MFC2_MAC3: u32 = 0x4808_d800;

/// One vertex through `OP` and a single MAC result selected by `READ`.
#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn gte_dot_one<const OP: u32, const READ: u32>(xy: u32, z: u32) -> i32 {
    let a: u32;
    unsafe {
        core::arch::asm!(
            ".word 0x48880000", // mtc2 $8, VXY0
            ".word 0x48890800", // mtc2 $9, VZ0
            ".word 0",
            ".word 0",
            ".word {op}",
            ".word {read}",
            ".word 0",
            op = const OP,
            read = const READ,
            inlateout("$8") xy => a,
            in("$9") z,
            options(nostack, nomem, preserves_flags),
        );
    }
    a as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::clip::ClipPlane;
    use crate::render::projection::{load_view_rotation, ViewProjection};
    use psx_engine::attributed_clip::AttributedClipPlane;
    use psx_engine::AffineVertex;

    fn planes_for(origin: [i32; 3], yaw: u16, pitch: u16) -> FrustumPlanes {
        psx_gte::host::reset();
        let view = Mat3I16::rotate_xyz(pitch >> 4, yaw >> 4, 0);
        let transform = load_view_rotation(
            GteVec3I32::new(origin[0] << 12, origin[1] << 12, origin[2] << 12),
            view,
        );
        let projection = ViewProjection {
            focal_length: 320,
            half_width: 160,
            half_height: 120,
            ..ViewProjection::DEFAULT
        };
        FrustumPlanes::from_view(&transform.rotation, transform.translation, projection)
    }

    fn vertex(p: [i16; 3]) -> AffineVertex {
        AffineVertex {
            position: p,
            uv: [0, 0],
            color: 0x808080,
            screen: [0, 0],
            depth: 0,
        }
    }

    /// Deterministic small PRNG so the sweeps cover many boxes without a
    /// dependency.
    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state
    }

    fn corners(mins: [i16; 3], maxs: [i16; 3]) -> [[i16; 3]; 8] {
        core::array::from_fn(|i| {
            [
                if i & 1 == 0 { mins[0] } else { maxs[0] },
                if i & 2 == 0 { mins[1] } else { maxs[1] },
                if i & 4 == 0 { mins[2] } else { maxs[2] },
            ]
        })
    }

    /// A world far from the origin culls exactly like the same world near
    /// it: only the eye-relative box matters.
    #[test]
    fn culling_is_camera_relative_and_world_offset_free() {
        psx_gte::host::reset();
        let view = Mat3I16::rotate_xyz(100 >> 4, 1024 >> 4, 0);
        let projection = ViewProjection {
            focal_length: 320,
            half_width: 160,
            half_height: 120,
            ..ViewProjection::DEFAULT
        };
        let planes = FrustumPlanes::at_eye(&view, projection);
        let mut state = 0x1357_9bdfu32;
        let (mut kept, mut dropped) = (0, 0);
        for _ in 0..3000 {
            let mut axis = || (lcg(&mut state) % 1800) as i32 - 900;
            let eye = [axis(), axis(), axis()];
            let min = [eye[0] + axis(), eye[1] + axis(), eye[2] + axis()];
            let size = (lcg(&mut state) % 400) as i32 + 1;
            let max = [min[0] + size, min[1] + size / 2 + 1, min[2] + size];
            let near = planes.cull_world_aabb(eye, min, max, CLIP_ALL_PLANES);
            for offset in [
                [100_000, 0, 0],
                [0, 0, -250_000],
                [70_000, -90_000, 400_000],
            ] {
                let shift = |p: [i32; 3]| core::array::from_fn::<i32, 3, _>(|i| p[i] + offset[i]);
                let far =
                    planes.cull_world_aabb(shift(eye), shift(min), shift(max), CLIP_ALL_PLANES);
                assert_eq!(
                    near, far,
                    "box {min:?}..{max:?} from {eye:?} moved by {offset:?}"
                );
            }
            if near.is_some() {
                kept += 1;
            } else {
                dropped += 1;
            }
        }
        assert!(kept > 100 && dropped > 100, "kept {kept} dropped {dropped}");
    }

    /// A rejected box has every corner outside one plane, and a plane the
    /// box clears has every corner inside it (beyond the rounding band).
    #[test]
    fn box_classification_agrees_with_its_corners() {
        let planes = planes_for([37, 70, 273], 1024, 100);
        let mut state = 0x0bad_c0deu32;
        let (mut rejected, mut cleared) = (0usize, 0usize);
        for _ in 0..3000 {
            let mut axis = || (lcg(&mut state) % 900) as i16 - 450;
            let min = [axis(), axis(), axis()];
            let size = (lcg(&mut state) % 300) as i16 + 1;
            let max = [min[0] + size, min[1] + size, min[2] + size];
            let points = corners(min, max);
            let result = planes.cull_aabb(
                Vec3I16::new(min[0], min[1], min[2]),
                Vec3I16::new(max[0], max[1], max[2]),
                CLIP_ALL_PLANES,
            );
            match result {
                None => {
                    rejected += 1;
                    let some_plane_rejects = (0..5).any(|index| {
                        let error = if index == CLIP_NEAR_PLANE {
                            0
                        } else {
                            planes.side_error
                        };
                        points
                            .iter()
                            .all(|&p| FrustumPlanes::distance(&planes.planes[index], p) < -error)
                    });
                    assert!(
                        some_plane_rejects,
                        "box {min:?}..{max:?} rejected without cause"
                    );
                }
                Some(residual) => {
                    for index in 0..5usize {
                        if residual & (1 << index) == 0 {
                            cleared += 1;
                            assert!(
                                points.iter().all(|&p| {
                                    FrustumPlanes::distance(&planes.planes[index], p) >= 0
                                }),
                                "plane {index} cleared for {min:?}..{max:?} with a corner outside"
                            );
                        }
                    }
                }
            }
        }
        assert!(
            rejected > 100 && cleared > 100,
            "{rejected} rejected, {cleared} cleared"
        );
    }

    /// Two quads visit a shared edge in opposite directions. The crossing must
    /// not depend on which endpoint comes first, or the neighbours disagree by
    /// a pixel and leave a crack.
    #[test]
    fn shared_edge_clips_to_the_same_vertex_in_either_direction() {
        let planes = planes_for([37, 70, 273], 1024, 100);
        let mut state = 0x2b7e_1516u32;
        let mut crossings = 0usize;
        for _ in 0..4000 {
            let point = |state: &mut u32| {
                let mut axis = || (lcg(state) % 1400) as i16 - 700;
                [axis(), axis(), axis()]
            };
            let a = vertex(point(&mut state));
            let mut b = vertex(point(&mut state));
            b.uv = [(lcg(&mut state) % 256) as u8, (lcg(&mut state) % 256) as u8];
            b.color = lcg(&mut state) & 0x00ff_ffff;
            for plane in &planes.planes {
                let da = FrustumPlanes::distance(plane, a.position);
                let db = FrustumPlanes::distance(plane, b.position);
                if (da >= 0) == (db >= 0) {
                    continue;
                }
                let clip = ClipPlane(plane);
                let forward = clip.intersection(0, &a, da, 1, &b, db);
                let backward = clip.intersection(1, &b, db, 0, &a, da);
                assert_eq!(forward, backward, "plane {plane:?} edge {a:?} {b:?}");
                crossings += 1;
            }
        }
        assert!(crossings > 500, "only {crossings} crossings exercised");
    }
}
