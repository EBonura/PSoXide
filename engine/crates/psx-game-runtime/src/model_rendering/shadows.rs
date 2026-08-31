//! Projected actor shadows.
//!
//! The blob decal in [`instances`][super::instances] draws one axis-aligned
//! quad under an actor. This module draws the actor's *own geometry*
//! flattened onto the ground plane, which is what "a real shadow" means on
//! this hardware: PS1 semi-transparency is a per-primitive GPU feature, so
//! the darkening itself is free; the whole cost is the CPU re-submitting the
//! mesh.
//!
//! # Why this costs one extra model pass and not one line more
//!
//! The model submit path already computes
//!
//! ```text
//!     p_world = origin + R * m          (m = local_to_world * joint * vertex)
//!     p_view  = camera_view * (p_world - camera.position)
//! ```
//!
//! A shadow cast by a light travelling along `d` (with `d.y < 0`) onto the
//! plane `y = floor_y` maps
//!
//! ```text
//!     p_shadow = p + d * (p.y - floor_y) / (-d.y)
//! ```
//!
//! Substituting `p = origin + R * m` and separating the constant part gives
//!
//! ```text
//!     p_shadow = origin_s + (F * R) * m
//!     origin_s = origin + d * (origin.y - floor_y) / (-d.y)      (origin_s.y == floor_y)
//!     F        = I + d * e_y^T / (-d.y)                          (row 1 is all zero)
//! ```
//!
//! which is *exactly the same submit call* with two substituted arguments.
//! No new transform code, no silhouette extraction, no edge adjacency, no
//! clipper. `F * R` is `R` with row 1 zeroed and rows 0/2 sheared by the
//! light's slant, so [`projected_shadow_rotation`] is nine multiply-adds and
//! [`projected_shadow_origin`] is two divides, both once per actor.
//!
//! Backface culling stays ON for the shadow pass. After flattening, a closed
//! mesh's triangles split into an up-facing set and a down-facing set that
//! cover the same silhouette, so culling one of them halves the packets and
//! halves the overdraw without changing the shape.

use super::*;

/// Direction a shadow-casting light travels, as a ground-plane slant.
///
/// Stored as the horizontal displacement per unit of *downward* travel in
/// Q12, so `slant_x = 0, slant_z = 0` is straight down (an actor's shadow
/// directly beneath it) and `slant_x = 2048` casts the shadow half a unit
/// east for every unit the light falls.
///
/// Slants are clamped into a range the Q12 `Mat3I16` can carry: a row of
/// `F * R` can reach `|R| * (1 + |slant|)`, and `R`'s cells are already up
/// to 1.0 in Q12, so a slant beyond ~2.0 would overflow the `i16` cells.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ShadowLight {
    slant_x_q12: i32,
    slant_z_q12: i32,
}

/// Largest slant the Q12 matrix cells can carry without saturating.
const SHADOW_SLANT_MAX_Q12: i32 = 2 << 12;

impl ShadowLight {
    /// A light straight overhead: the shadow lands directly under the actor.
    pub const OVERHEAD: Self = Self {
        slant_x_q12: 0,
        slant_z_q12: 0,
    };

    /// Build a slanted light. Both slants are horizontal units per unit of
    /// downward travel, Q12, clamped to +/-2.0.
    pub const fn new(slant_x_q12: i32, slant_z_q12: i32) -> Self {
        Self {
            slant_x_q12: clamp_slant(slant_x_q12),
            slant_z_q12: clamp_slant(slant_z_q12),
        }
    }

    /// Horizontal X displacement per unit of downward travel, Q12.
    pub const fn slant_x_q12(self) -> i32 {
        self.slant_x_q12
    }

    /// Horizontal Z displacement per unit of downward travel, Q12.
    pub const fn slant_z_q12(self) -> i32 {
        self.slant_z_q12
    }

    /// True when the light is straight down, which lets the caller skip the
    /// shear entirely and just zero row 1.
    pub const fn is_overhead(self) -> bool {
        self.slant_x_q12 == 0 && self.slant_z_q12 == 0
    }
}

const fn clamp_slant(value: i32) -> i32 {
    if value > SHADOW_SLANT_MAX_Q12 {
        SHADOW_SLANT_MAX_Q12
    } else if value < -SHADOW_SLANT_MAX_Q12 {
        -SHADOW_SLANT_MAX_Q12
    } else {
        value
    }
}

/// Flatten a model's instance rotation onto the ground plane.
///
/// Returns `F * R`: row 1 zeroed (every vertex lands on the plane) and rows
/// 0/2 sheared by the light's slant. Cells saturate into `i16` rather than
/// wrapping, so an extreme slant degrades into a longer shadow instead of a
/// scrambled one.
#[inline]
pub fn projected_shadow_rotation(rotation: Mat3I16, light: ShadowLight) -> Mat3I16 {
    let [r0, r1, r2] = rotation.m;
    if light.is_overhead() {
        return Mat3I16 {
            m: [r0, [0; 3], r2],
        };
    }
    let sx = light.slant_x_q12();
    let sz = light.slant_z_q12();
    let mut m = [[0i16; 3]; 3];
    let mut col = 0usize;
    while col < 3 {
        let mid = i32::from(r1[col]);
        m[0][col] = clamp_i16(i32::from(r0[col]).saturating_add((sx * mid) >> 12));
        m[2][col] = clamp_i16(i32::from(r2[col]).saturating_add((sz * mid) >> 12));
        col += 1;
    }
    Mat3I16 { m }
}

/// Project a model's world origin onto the ground plane along the light.
///
/// The returned point always has `y == floor_y`. An actor already at or
/// below the plane projects to itself horizontally, so a shadow never runs
/// *backwards* when the ground query returns a floor above the feet.
#[inline]
pub fn projected_shadow_origin(
    origin: WorldVertex,
    floor_y: i32,
    light: ShadowLight,
) -> WorldVertex {
    let drop = origin.y.saturating_sub(floor_y).max(0);
    if light.is_overhead() || drop == 0 {
        return origin.with_y(floor_y);
    }
    let dx = (light.slant_x_q12().saturating_mul(drop)) >> 12;
    let dz = (light.slant_z_q12().saturating_mul(drop)) >> 12;
    WorldVertex::new(
        origin.x.saturating_add(dx),
        floor_y,
        origin.z.saturating_add(dz),
    )
}

/// Tuning for the projected (flattened-geometry) shadow pass.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProjectedShadowTuning {
    /// Direction the shadow-casting light travels.
    pub light: ShadowLight,
    /// Lift above the ground plane, so the flattened mesh never lands in the
    /// floor's own depth slot.
    pub floor_lift: i32,
    /// Depth bias ADDED to the caller's actor bias. Negative pulls the
    /// shadow toward the camera (in front of the floor); positive pushes it
    /// away (behind the actor's own feet).
    pub depth_bias: i32,
    /// Semi-transparency used by the shadow packets.
    ///
    /// [`BlendMode::Average`] darkens proportionally (`(B + F) / 2` with a
    /// near-black `F` halves whatever the floor was) and can never clip to a
    /// flat black hole, which is why it reads better than
    /// [`BlendMode::Subtract`] on a level whose floors span a wide brightness
    /// range. `Subtract` is kept selectable because it is the darker of the
    /// two on already-bright ground.
    pub blend: BlendMode,
    /// Flat colour the shadow packets modulate with. Only read when the
    /// material is not raw-textured.
    pub tint: (u8, u8, u8),
    /// Highest actor-above-floor distance that still casts. Beyond it the
    /// pass is skipped entirely, so a falling or flying actor stops paying
    /// for a shadow nobody can associate with it.
    pub max_drop: i32,
}

impl ProjectedShadowTuning {
    /// Straight-down, half-darkening shadow with no depth adjustment beyond
    /// the caller's actor clearance.
    pub const DEFAULT: Self = Self {
        light: ShadowLight::OVERHEAD,
        floor_lift: 1,
        depth_bias: 0,
        blend: BlendMode::Average,
        tint: (0, 0, 0),
        max_drop: 1 << 14,
    };
}

/// Surface options for the projected shadow pass.
///
/// `split_textured_triangles = true` with `max_edge = 0` is not "subdivide
/// the shadow": in `world_pass_model` that exact pair is the gate on the
/// packed batch path (`packed_fast_faces`), where a model whose projected
/// bounds are proved in-front and inside the hardware extents emits packets
/// without any per-face validation. Turning splitting *off* drops the whole
/// model onto the general per-face path. Measured on the benchmark tape, the
/// off variant cost the player stage 650k cycles per frame against 224k for
/// the body draw it was shadowing -- three times a full player draw for a
/// second pass over the same mesh. With the pair restored it costs about one.
///
/// `max_edge = 0` still matters: it keeps the splitter focused on hardware
/// extent limits instead of subdividing for affine-warp quality, which a
/// flat dark silhouette has no use for.
#[inline]
pub fn projected_shadow_options(
    options: WorldSurfaceOptions,
    tuning: ProjectedShadowTuning,
    material: TextureMaterial,
) -> WorldSurfaceOptions {
    options
        .with_depth_policy(DepthPolicy::Average)
        .with_depth_bias(options.depth_bias.saturating_add(tuning.depth_bias))
        .with_cull_mode(CullMode::Back)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0)
        .with_adaptive_subdivision(false)
        .with_model_uv_mapping(ModelUvMapping::Authored)
}

/// The material a shadow pass draws with.
///
/// Derived from the actor's own atlas material so the pass needs no extra
/// VRAM: same tpage, same CLUT, the model's own UVs. Dropping the raw-texture
/// bit is what makes it a shadow -- the packet's flat colour then modulates
/// every texel, so a near-black tint collapses the atlas to near-black
/// whatever it actually contains, and the blend mode does the rest.
#[inline]
pub fn projected_shadow_material(
    base: TextureMaterial,
    tuning: ProjectedShadowTuning,
) -> TextureMaterial {
    base.with_blend_mode(tuning.blend)
        .with_tint(tuning.tint)
        .with_raw_texture(false)
        .with_dither(false)
}

/// Draw one actor's geometry flattened onto the ground plane.
///
/// `floor_y` is the ground the shadow lands on, in the same room-local space
/// as the pose's origin. Callers that have no ground query pass the actor's
/// own floor anchor, which is exact whenever the actor is standing.
///
/// Returns the submit stats so the caller can fold them into its own model
/// counters, and `None` when the actor is too far above the plane to cast.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn draw_actor_projected_shadow<
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
>(
    tuning: ProjectedShadowTuning,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
    runtime_model: RuntimeModelAsset,
    pose: ActorPoseSnapshot,
    floor_y: i32,
    base_material: TextureMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> Option<TexturedModelRenderStats> {
    let origin = pose.origin();
    if origin.y.saturating_sub(floor_y) > tuning.max_drop {
        return None;
    }
    let faces = runtime_model_faces(runtime_model, model_faces);
    let plane_y = floor_y.saturating_add(tuning.floor_lift);
    let material = projected_shadow_material(base_material, tuning);
    Some(submit_runtime_model_predecoded(
        world,
        triangles,
        runtime_model,
        pose.animation(),
        pose.phase_q12(),
        pose.blend_from(),
        *camera,
        projected_shadow_origin(origin, plane_y, tuning.light),
        projected_shadow_rotation(pose.rotation(), tuning.light),
        pose.local_to_world(),
        pose.pose_translation(),
        material,
        None,
        projected_shadow_options(options, tuning, material),
        faces,
        model_parts,
        model_vertices,
        false,
        scratch,
    ))
}

/// Draw a projected shadow under every placed model instance of
/// `current_room` that survived visibility.
///
/// Mirrors [`draw_model_instance_shadows`][super::draw_model_instance_shadows]
/// so a build can swap one for the other, but reads the already-resolved
/// per-instance pose instead of the cooked spawn transform, which is what
/// lets an enemy's shadow follow its animation.
#[allow(clippy::too_many_arguments)]
pub fn draw_model_instance_projected_shadows<
    const MAX_MODEL_INSTANCES: usize,
    const MODEL_VERTEX_CAP: usize,
    const JOINT_CAP: usize,
    const OT_DEPTH: usize,
>(
    tables: ModelTables,
    knobs: ModelDrawKnobs,
    tuning: ProjectedShadowTuning,
    scratch: &mut ModelDrawScratch<MODEL_VERTEX_CAP, JOINT_CAP>,
    current_room: RoomIndex,
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let mut drawn = 0usize;
    for pose in instance_poses
        .iter()
        .take(knobs.max_model_instances)
        .copied()
        .flatten()
    {
        if drawn >= knobs.max_model_instances {
            break;
        }
        let index = pose.instance_index();
        if index >= 64 || visible_instance_mask & (1u64 << index) == 0 {
            continue;
        }
        let Some(record) = tables.model_instances.get(index) else {
            continue;
        };
        if record.room != current_room {
            continue;
        }
        let runtime_model = pose.model();
        let actor_pose = pose.pose();
        // The pose's own origin, not the cooked `record.y`: a live entity
        // bound to this instance moves, and the cooked spawn height would
        // leave its shadow behind on the floor it started on.
        let floor_y = actor_pose.origin().y;
        draw_actor_projected_shadow(
            tuning,
            scratch,
            runtime_model,
            actor_pose,
            floor_y,
            runtime_model.material,
            camera,
            options,
            model_faces,
            model_parts,
            model_vertices,
            triangles,
            world,
        );
        drawn += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q12(v: f32) -> i16 {
        (v * 4096.0) as i16
    }

    /// Row 1 of the flattened rotation must be exactly zero: that is what
    /// puts every projected vertex on the plane.
    #[test]
    fn overhead_light_zeroes_the_vertical_row() {
        let r = Mat3I16 {
            m: [
                [q12(0.5), q12(-0.5), q12(0.25)],
                [q12(0.75), q12(1.0), q12(-0.25)],
                [q12(-1.0), q12(0.5), q12(0.5)],
            ],
        };
        let f = projected_shadow_rotation(r, ShadowLight::OVERHEAD);
        assert_eq!(f.m[1], [0, 0, 0]);
        assert_eq!(f.m[0], r.m[0]);
        assert_eq!(f.m[2], r.m[2]);
    }

    /// A slanted light shears rows 0 and 2 by the vertical row, which is what
    /// makes the shadow lean. Row 1 stays zero.
    #[test]
    fn slanted_light_shears_by_the_vertical_row() {
        let r = Mat3I16 {
            m: [[q12(1.0), 0, 0], [0, q12(1.0), 0], [0, 0, q12(1.0)]],
        };
        let light = ShadowLight::new(q12(0.5) as i32, q12(-0.25) as i32);
        let f = projected_shadow_rotation(r, light);
        assert_eq!(f.m[1], [0, 0, 0]);
        // Column 1 of an identity R is the vertical axis, so it is the only
        // column the shear can move.
        assert_eq!(f.m[0], [q12(1.0), q12(0.5), 0]);
        assert_eq!(f.m[2], [0, q12(-0.25), q12(1.0)]);
    }

    /// Cells saturate instead of wrapping, so an extreme slant lengthens the
    /// shadow rather than scrambling it.
    #[test]
    fn extreme_slant_saturates_rather_than_wrapping() {
        let r = Mat3I16 {
            m: [[i16::MAX, 0, 0], [i16::MAX, 0, 0], [0, 0, i16::MAX]],
        };
        let f = projected_shadow_rotation(r, ShadowLight::new(SHADOW_SLANT_MAX_Q12 * 4, 0));
        assert_eq!(f.m[0][0], i16::MAX);
        assert_eq!(f.m[1], [0, 0, 0]);
    }

    /// The projected origin always lands exactly on the plane.
    #[test]
    fn projected_origin_lands_on_the_plane() {
        let origin = WorldVertex::new(100, 900, -40);
        for light in [
            ShadowLight::OVERHEAD,
            ShadowLight::new(q12(0.5) as i32, q12(0.25) as i32),
        ] {
            let p = projected_shadow_origin(origin, 128, light);
            assert_eq!(p.y, 128);
        }
    }

    /// A slanted light displaces the origin by slant * drop.
    #[test]
    fn slanted_origin_displaces_by_slant_times_drop() {
        let origin = WorldVertex::new(0, 1128, 0);
        let p = projected_shadow_origin(origin, 128, ShadowLight::new(q12(0.5) as i32, 0));
        assert_eq!(p, WorldVertex::new(500, 128, 0));
    }

    /// An actor at or below the plane keeps its horizontal position, so a
    /// ground query that reports a floor above the feet cannot fling the
    /// shadow away from the actor.
    #[test]
    fn actor_below_the_plane_does_not_displace() {
        let origin = WorldVertex::new(7, 100, -7);
        let p = projected_shadow_origin(origin, 400, ShadowLight::new(q12(2.0) as i32, 0));
        assert_eq!(p, WorldVertex::new(7, 400, -7));
    }

    /// Slants are clamped so `F * R` cannot overflow the Q12 cells.
    #[test]
    fn slant_is_clamped_to_the_representable_range() {
        assert_eq!(
            ShadowLight::new(1 << 20, -(1 << 20)).slant_x_q12(),
            SHADOW_SLANT_MAX_Q12
        );
        assert_eq!(
            ShadowLight::new(1 << 20, -(1 << 20)).slant_z_q12(),
            -SHADOW_SLANT_MAX_Q12
        );
    }

    /// The shadow pass must stay on the packed batch path: `splitting = true`
    /// with `max_edge = 0` is the gate on it, and clearing either one costs
    /// three times a full model draw.
    #[test]
    fn shadow_options_keep_the_packed_batch_path() {
        let base = WorldSurfaceOptions::new(
            psx_engine::DepthBand::new(0, 511),
            psx_engine::DepthRange::new(4, 4096),
        )
        .with_depth_bias(-32)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(48)
        .with_adaptive_subdivision(true);
        let material = TextureMaterial::opaque(0, 0, (128, 128, 128));
        let tuning = ProjectedShadowTuning {
            depth_bias: -6,
            ..ProjectedShadowTuning::DEFAULT
        };
        let out = projected_shadow_options(base, tuning, material);
        assert!(out.split_textured_triangles);
        assert_eq!(out.textured_split_max_edge, 0);
        assert!(!out.adaptive_subdivision);
        // The actor clearance the caller already applied is kept, not
        // replaced: that is the bug the blob decal has.
        assert_eq!(out.depth_bias, -38);
    }
}
