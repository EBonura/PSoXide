use egui::{Color32, ColorImage};
use psx_asset::{Animation, JointPose, Model, ModelVertex};
use psx_engine::{
    compute_joint_world_basis, compute_joint_world_transform, Angle, JointWorldTransform,
    LocalToWorldScale, Mat3I16, ProjectedVertex, WorldCamera, WorldProjection, WorldVertex,
};
use psx_gte::math::{Vec3I16, Vec3I32};
use psxed_project::MODEL_SCALE_ONE_Q8;

pub const PREVIEW_WIDTH: usize = 320;
pub const PREVIEW_HEIGHT: usize = 240;
pub const AUTHORING_PREVIEW_MAX_WIDTH: usize = 960;
pub const AUTHORING_PREVIEW_MAX_HEIGHT: usize = 720;

const PREVIEW_NEAR_Z: i32 = 48;
const PREVIEW_FOCAL_LENGTH: i32 = 320;
const PREVIEW_PLAYBACK_HZ: u16 = 60;

/// One joint-local combat capsule drawn over an animated model preview.
#[derive(Clone, Debug)]
pub(crate) struct PreviewCombatCapsule {
    pub joint: u16,
    pub start: [i32; 3],
    pub end: [i32; 3],
    pub radius: u16,
    pub color: Color32,
    pub selected: bool,
}

/// One attachment socket drawn over an animated model preview as a
/// bone-local axis triad. Orientation composes exactly like the runtime
/// equipment path (unscaled joint basis times the socket's local Euler),
/// so the triad shows the frame an attached weapon would inherit.
#[derive(Clone, Debug)]
pub(crate) struct PreviewSocket {
    pub joint: u16,
    pub translation: [i32; 3],
    pub rotation_q12: [i16; 3],
    pub selected: bool,
}

/// Character material override for the preview: replaces the model's
/// own atlas with a resolved material texture (the same bytes the cook
/// ships) plus the material's animated UV motion, advanced on the
/// preview clock exactly like the runtime advances it on sim ticks.
/// Sampling tiles the override so byte-space model UVs repeat a
/// smaller generated texture, the way the crystal/hologram skins read
/// in Play.
pub struct PreviewMaterialLayer<'a> {
    pub atlas: &'a ColorImage,
    pub motion: psx_level::LevelMaterialUvMotion,
}

/// Equipped-weapon overlay: a second rigid model rendered riding a
/// socket, placed with the runtime equipment composition (socket pose
/// times grip inverse) so the preview equals what Play shows.
pub struct PreviewEquippedWeapon<'a> {
    pub model_bytes: &'a [u8],
    pub atlas: &'a ColorImage,
    pub socket_joint: u16,
    pub socket_translation: [i32; 3],
    pub socket_rotation_q12: [i16; 3],
    pub grip_translation: [i32; 3],
    pub grip_rotation_q12: [i16; 3],
    /// Fraction of the weapon that has materialised, where 4096 is complete.
    pub materialization_q12: u16,
    /// Draw the materialised faces as the runtime's green nanobot cage.
    pub wireframe_materialization: bool,
}

/// Preview image plus projected body-part points used by Animation Studio's
/// click-to-attach workflow.
pub(crate) struct ImportPreviewRender {
    pub image: ColorImage,
    pub joint_screen_positions: Vec<Option<[f32; 2]>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JointCapsuleFit {
    pub start: [i32; 3],
    pub end: [i32; 3],
    pub radius: u16,
}

/// Fit a conservative capsule around vertices primarily assigned to `joint`.
/// The longest local axis becomes the segment and the other two axes size the
/// radius. Values are returned in the same engine-unit joint space used by the
/// combat capsule contract.
pub(crate) fn fit_capsule_to_joint(
    model_bytes: &[u8],
    joint: u16,
    visual_scale_q8: u16,
) -> Option<JointCapsuleFit> {
    let model = Model::from_bytes(model_bytes).ok()?;
    if joint >= model.joint_count() {
        return None;
    }
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    let mut found = false;
    for part_index in 0..model.part_count() {
        let part = model.part(part_index)?;
        if part.joint_index() != joint {
            continue;
        }
        let end = part.first_vertex().saturating_add(part.vertex_count());
        for vertex_index in part.first_vertex()..end {
            let vertex = model.vertex(vertex_index)?;
            let values = [
                i32::from(vertex.position.x),
                i32::from(vertex.position.y),
                i32::from(vertex.position.z),
            ];
            for axis in 0..3 {
                min[axis] = min[axis].min(values[axis]);
                max[axis] = max[axis].max(values[axis]);
            }
            found = true;
        }
    }
    if !found {
        return None;
    }
    let scale = preview_model_local_to_world(&model, visual_scale_q8);
    let center = [
        scale.apply(min[0].saturating_add(max[0]) / 2),
        scale.apply(min[1].saturating_add(max[1]) / 2),
        scale.apply(min[2].saturating_add(max[2]) / 2),
    ];
    let extents = [
        scale.apply(max[0].saturating_sub(min[0]).abs()) / 2,
        scale.apply(max[1].saturating_sub(min[1]).abs()) / 2,
        scale.apply(max[2].saturating_sub(min[2]).abs()) / 2,
    ];
    let segment_axis = (0..3).max_by_key(|axis| extents[*axis])?;
    let radius = (0..3)
        .filter(|axis| *axis != segment_axis)
        .map(|axis| extents[axis])
        .max()
        .unwrap_or(1)
        .clamp(12, i32::from(u16::MAX)) as u16;
    let half_segment = extents[segment_axis]
        .saturating_sub(i32::from(radius))
        .max(0);
    let mut start = center;
    let mut end = center;
    start[segment_axis] = start[segment_axis].saturating_sub(half_segment);
    end[segment_axis] = end[segment_axis].saturating_add(half_segment);
    Some(JointCapsuleFit { start, end, radius })
}

#[derive(Copy, Clone, Debug)]
pub struct ImportPreviewOptions {
    pub world_height: i32,
    pub visual_scale_q8: u16,
    pub visual_yaw_q12: i16,
    pub collision_radius: i32,
    pub time_seconds: f64,
    pub yaw_q12: u16,
    pub pitch_q12: u16,
    pub radius: i32,
    pub focus_on_animated_bounds: bool,
    pub preview_in_place: bool,
    pub pose_offset: [i32; 3],
    pub show_animation_root: bool,
    pub show_collision_guides: bool,
    pub show_bones: bool,
}

pub fn render_import_model_preview_with_options(
    model_bytes: &[u8],
    clip_bytes: &[u8],
    atlas: &ColorImage,
    options: ImportPreviewOptions,
) -> Option<ColorImage> {
    render_import_model_preview_with_orientation_at_size(
        model_bytes,
        clip_bytes,
        atlas,
        options,
        yaw_rotation_matrix(options.visual_yaw_q12),
        [PREVIEW_WIDTH, PREVIEW_HEIGHT],
    )
}

/// Render the standard 320x240 cooked-model preview with the material and
/// equipped-weapon overlays used by Animation Studio. Reel/debug tooling uses
/// this entry point to reproduce the in-engine character presentation without
/// exposing the editor-only capsule and joint-picking machinery.
pub fn render_import_model_preview_with_equipment(
    model_bytes: &[u8],
    clip_bytes: &[u8],
    atlas: &ColorImage,
    options: ImportPreviewOptions,
    equipped_weapon: Option<&PreviewEquippedWeapon<'_>>,
    character_material: Option<&PreviewMaterialLayer<'_>>,
) -> Option<ColorImage> {
    render_import_model_preview_with_combat_capsules_at_size(
        model_bytes,
        clip_bytes,
        atlas,
        options,
        yaw_rotation_matrix(options.visual_yaw_q12),
        [PREVIEW_WIDTH, PREVIEW_HEIGHT],
        &[],
        &[],
        equipped_weapon,
        character_material,
        None,
    )
    .map(|render| render.image)
}

/// Render a model preview with the exact presentation transform used by a
/// scene instance. Animation Studio uses this path so pitched and rolled
/// imports do not silently fall back to the older yaw-only resource preview.
pub(crate) fn render_import_model_preview_with_orientation_at_size(
    model_bytes: &[u8],
    clip_bytes: &[u8],
    atlas: &ColorImage,
    options: ImportPreviewOptions,
    instance_rotation: Mat3I16,
    render_size: [usize; 2],
) -> Option<ColorImage> {
    render_import_model_preview_with_combat_capsules_at_size(
        model_bytes,
        clip_bytes,
        atlas,
        options,
        instance_rotation,
        render_size,
        &[],
        &[],
        None,
        None,
        None,
    )
    .map(|render| render.image)
}

/// Render an animated preview with rig-following combat capsules and return
/// the projected body-part points needed for visual joint picking.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_import_model_preview_with_combat_capsules_at_size(
    model_bytes: &[u8],
    clip_bytes: &[u8],
    atlas: &ColorImage,
    options: ImportPreviewOptions,
    instance_rotation: Mat3I16,
    render_size: [usize; 2],
    combat_capsules: &[PreviewCombatCapsule],
    sockets: &[PreviewSocket],
    equipped_weapon: Option<&PreviewEquippedWeapon<'_>>,
    character_material: Option<&PreviewMaterialLayer<'_>>,
    selected_joint: Option<u16>,
) -> Option<ImportPreviewRender> {
    let model = Model::from_bytes(model_bytes).ok()?;
    let animation = Animation::from_bytes(clip_bytes).ok()?;
    if atlas.size[0] == 0 || atlas.size[1] == 0 {
        return None;
    }

    let render_width = render_size[0].clamp(PREVIEW_WIDTH, AUTHORING_PREVIEW_MAX_WIDTH);
    let render_height = render_size[1].clamp(PREVIEW_HEIGHT, AUTHORING_PREVIEW_MAX_HEIGHT);
    let focal_length = ((PREVIEW_FOCAL_LENGTH as i64 * render_width as i64) / PREVIEW_WIDTH as i64)
        .clamp(1, i32::MAX as i64) as i32;
    let vertical_nudge = ((4 * render_height) / PREVIEW_HEIGHT) as i16;

    let projection = WorldProjection::new(
        (render_width / 2) as i16,
        (render_height / 2) as i16 + vertical_nudge,
        focal_length,
        PREVIEW_NEAR_Z,
    );
    let height = options.world_height.max(128);
    let visual_height = scale_q8_i32(height, options.visual_scale_q8).max(1);
    let origin = WorldVertex::new(0, height / 2, 0);
    let frame_q12 = animation.phase_at_tick_q12(
        (options.time_seconds.max(0.0) * PREVIEW_PLAYBACK_HZ as f64) as u32,
        PREVIEW_PLAYBACK_HZ,
    );
    let root_delta = options
        .preview_in_place
        .then(|| root_motion_delta_q12(&animation, frame_q12))
        .flatten();
    let local_to_world = preview_model_local_to_world(&model, options.visual_scale_q8);
    let focus = options
        .focus_on_animated_bounds
        .then(|| {
            animation_preview_focus(
                &model,
                &animation,
                local_to_world,
                instance_rotation,
                origin,
                height,
                options.preview_in_place,
                options.pose_offset,
            )
        })
        .flatten();
    let target = focus.map(|focus| focus.center).unwrap_or(origin);
    let radius = if options.radius > 0 {
        options.radius
    } else {
        focus
            .map(|focus| focus.radius)
            .unwrap_or_else(|| visual_height.saturating_mul(3) / 2)
    }
    .clamp(640, 8192);
    let camera = WorldCamera::orbit(
        projection,
        target,
        radius,
        Angle::from_q12(options.yaw_q12),
        Angle::from_q12(options.pitch_q12),
    );

    let mut image = ColorImage {
        size: [render_width, render_height],
        pixels: vec![Color32::from_rgb(8, 10, 14); render_width * render_height],
    };
    draw_floor_grid(&mut image, camera, projection, focus, origin, height);
    let mut z_buffer = vec![f32::INFINITY; render_width * render_height];
    let mut joint_transforms =
        vec![JointWorldTransform::ZERO; model.joint_count().min(animation.joint_count()) as usize];
    for (joint, transform) in joint_transforms.iter_mut().enumerate() {
        let mut pose = animation.pose_looped_q12(frame_q12, joint as u16)?;
        if let Some(delta) = root_delta {
            apply_root_motion_delta(&mut pose, delta);
        }
        apply_pose_offset(&mut pose, options.pose_offset);
        *transform = compute_joint_world_transform(pose, instance_rotation, local_to_world, origin);
    }
    let projected_body_anchor =
        body_anchor_projected(camera, projection, focus.map(|focus| focus.center), origin);
    let joint_origins: Vec<Option<ProjectedVertex>> = if options.show_bones
        || !combat_capsules.is_empty()
        || !sockets.is_empty()
        || selected_joint.is_some()
    {
        estimated_joint_points(&model, &joint_transforms, camera)
    } else {
        Vec::new()
    };

    let mut projected = vec![None; model.vertex_count() as usize];
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let primary_joint = part.joint_index() as usize;
        let Some(primary) = joint_transforms.get(primary_joint).copied() else {
            continue;
        };
        let start = part.first_vertex() as usize;
        let end = start
            .saturating_add(part.vertex_count() as usize)
            .min(projected.len());
        for (vertex_index, slot) in projected.iter_mut().enumerate().take(end).skip(start) {
            let Some(vertex) = model.vertex(vertex_index as u16) else {
                continue;
            };
            *slot = project_import_model_vertex(vertex, primary, &joint_transforms, camera);
        }
    }

    let double_sided = model.double_sided();
    let (character_atlas, character_uv_offset, character_wrap_uv) = match character_material {
        Some(layer) => {
            // Same tick source as `frame_q12` above, so scrubbing the
            // clip advances the material scroll in lockstep with Play.
            let tick = (options.time_seconds.max(0.0) * PREVIEW_PLAYBACK_HZ as f64) as u32;
            (
                layer.atlas,
                layer.motion.offset_at_tick(tick, PREVIEW_PLAYBACK_HZ),
                true,
            )
        }
        None => (atlas, [0, 0], false),
    };
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let first_face = part.first_face();
        let last_face = first_face.saturating_add(part.face_count());
        for face_index in first_face..last_face {
            let Some(face) = model.face(face_index) else {
                continue;
            };
            let [a, b, c] = face.corners;
            let Some(pa) = projected.get(a.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            let Some(pb) = projected.get(b.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            let Some(pc) = projected.get(c.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            raster_textured_triangle(
                &mut image,
                &mut z_buffer,
                character_atlas,
                [
                    PreviewVertex::from_projected(pa, a.uv).with_uv_offset(character_uv_offset),
                    PreviewVertex::from_projected(pb, b.uv).with_uv_offset(character_uv_offset),
                    PreviewVertex::from_projected(pc, c.uv).with_uv_offset(character_uv_offset),
                ],
                double_sided,
                character_wrap_uv,
            );
        }
    }

    if let Some(weapon) = equipped_weapon {
        draw_equipped_weapon_overlay(
            &mut image,
            &mut z_buffer,
            camera,
            weapon,
            &animation,
            frame_q12,
            root_delta,
            options.pose_offset,
            instance_rotation,
            &joint_transforms,
        );
    }

    if options.show_animation_root {
        if let Some(anchor) = projected_body_anchor {
            draw_animation_root_marker(&mut image, anchor);
        }
    }
    if options.show_collision_guides {
        draw_height_and_collision_guides(
            &mut image,
            camera,
            projection,
            focus,
            origin,
            visual_height,
            options.collision_radius,
        );
    }
    if options.show_bones {
        draw_bone_overlay(&mut image, &model, &joint_origins);
    }

    for capsule in combat_capsules {
        let Some(joint) = joint_transforms.get(capsule.joint as usize).copied() else {
            continue;
        };
        draw_joint_combat_capsule(&mut image, camera, projection, joint, capsule);
    }
    let socket_axis_length = (visual_height / 6).max(96);
    for socket in sockets {
        let Some(joint) = joint_transforms.get(socket.joint as usize).copied() else {
            continue;
        };
        // Same pose sampling as the joint_transforms loop above, but the
        // marker orientation needs the UNSCALED basis (the runtime's
        // weapon-orientation composition), not the vertex-space matrix.
        let Some(mut pose) = animation.pose_looped_q12(frame_q12, socket.joint) else {
            continue;
        };
        if let Some(delta) = root_delta {
            apply_root_motion_delta(&mut pose, delta);
        }
        apply_pose_offset(&mut pose, options.pose_offset);
        let basis = compute_joint_world_basis(pose, instance_rotation).mul(&euler_rotation_q12([
            socket.rotation_q12[0] as u16,
            socket.rotation_q12[1] as u16,
            socket.rotation_q12[2] as u16,
        ]));
        draw_socket_marker(
            &mut image,
            camera,
            projection,
            joint,
            &basis,
            socket,
            socket_axis_length,
        );
    }
    if let Some(joint) = selected_joint
        .and_then(|joint| joint_origins.get(joint as usize))
        .and_then(|point| *point)
    {
        draw_selected_joint_dot(&mut image, joint.sx as i32, joint.sy as i32);
    }

    Some(ImportPreviewRender {
        image,
        joint_screen_positions: joint_origins
            .into_iter()
            .map(|point| point.map(|point| [point.sx as f32, point.sy as f32]))
            .collect(),
    })
}

fn preview_model_local_to_world(model: &Model<'_>, visual_scale_q8: u16) -> LocalToWorldScale {
    let scale_q8 = visual_scale_q8.max(1) as u32;
    let q12 = ((model.local_to_world_q12() as u32)
        .saturating_mul(scale_q8)
        .saturating_add((MODEL_SCALE_ONE_Q8 / 2) as u32))
        / MODEL_SCALE_ONE_Q8 as u32;
    LocalToWorldScale::from_q12(q12.clamp(1, u16::MAX as u32) as u16)
}

fn yaw_rotation_matrix(yaw_q12: i16) -> Mat3I16 {
    let yaw = Angle::from_q12((yaw_q12 as i32).rem_euclid(4096) as u16);
    let s = clamp_i16(yaw.sin().raw());
    let c = clamp_i16(yaw.cos().raw());
    Mat3I16 {
        m: [[c, 0, s], [0, 0x1000, 0], [-s, 0, c]],
    }
}

/// Full instance rotation in the same `Rz * Ry * Rx` order used by the
/// native 3D editor preview and runtime model renderer.
pub(crate) fn euler_rotation_q12(rotation_q12: [u16; 3]) -> Mat3I16 {
    let [pitch_q12, yaw_q12, roll_q12] = rotation_q12;
    if pitch_q12 == 0 && roll_q12 == 0 {
        return yaw_rotation_matrix(yaw_q12 as i16);
    }
    let rx = Mat3I16::rotate_x(Angle::from_q12(pitch_q12).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(yaw_q12).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(roll_q12).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
}

fn scale_q8_i32(value: i32, scale_q8: u16) -> i32 {
    let scale = scale_q8.max(1) as i32;
    value.saturating_mul(scale) / MODEL_SCALE_ONE_Q8 as i32
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PreviewWorldBounds {
    min: WorldVertex,
    max: WorldVertex,
}

impl PreviewWorldBounds {
    const fn new(point: WorldVertex) -> Self {
        Self {
            min: point,
            max: point,
        }
    }

    fn include(&mut self, point: WorldVertex) {
        self.min.x = self.min.x.min(point.x);
        self.min.y = self.min.y.min(point.y);
        self.min.z = self.min.z.min(point.z);
        self.max.x = self.max.x.max(point.x);
        self.max.y = self.max.y.max(point.y);
        self.max.z = self.max.z.max(point.z);
    }

    fn include_bounds(&mut self, bounds: Self) {
        self.include(bounds.min);
        self.include(bounds.max);
    }

    fn center(self) -> WorldVertex {
        WorldVertex::new(
            midpoint_i32(self.min.x, self.max.x),
            midpoint_i32(self.min.y, self.max.y),
            midpoint_i32(self.min.z, self.max.z),
        )
    }

    fn largest_extent(self) -> i32 {
        self.max
            .x
            .saturating_sub(self.min.x)
            .max(self.max.y.saturating_sub(self.min.y))
            .max(self.max.z.saturating_sub(self.min.z))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PreviewFocus {
    center: WorldVertex,
    radius: i32,
    floor_y: i32,
}

fn midpoint_i32(a: i32, b: i32) -> i32 {
    a.saturating_add(b.saturating_sub(a) / 2)
}

fn preview_radius_for_focus(
    bounds: PreviewWorldBounds,
    target: WorldVertex,
    fallback_height: i32,
) -> i32 {
    let max_distance = axis_distance_from_target(bounds.min.x, bounds.max.x, target.x)
        .max(axis_distance_from_target(
            bounds.min.y,
            bounds.max.y,
            target.y,
        ))
        .max(axis_distance_from_target(
            bounds.min.z,
            bounds.max.z,
            target.z,
        ));
    let extent_radius = max_distance
        .saturating_mul(3)
        .max(bounds.largest_extent().saturating_mul(3) / 2);
    extent_radius.max(fallback_height.saturating_mul(3) / 2)
}

fn axis_distance_from_target(min: i32, max: i32, target: i32) -> i32 {
    let lo = (min as i64 - target as i64).abs();
    let hi = (max as i64 - target as i64).abs();
    lo.max(hi).min(i32::MAX as i64) as i32
}

fn animation_preview_focus(
    model: &Model<'_>,
    animation: &Animation<'_>,
    local_to_world: LocalToWorldScale,
    instance_rotation: Mat3I16,
    origin: WorldVertex,
    fallback_height: i32,
    preview_in_place: bool,
    pose_offset: [i32; 3],
) -> Option<PreviewFocus> {
    let frame_count = animation.frame_count().max(1);
    let mut all_bounds: Option<PreviewWorldBounds> = None;
    let mut sum_x = 0i64;
    let mut sum_z = 0i64;
    let mut sampled = 0i64;
    for frame in 0..frame_count {
        let root_delta = preview_in_place
            .then(|| root_motion_delta_at_frame(animation, frame))
            .flatten();
        let joint_world_transforms = build_joint_world_transforms_at_frame(
            model,
            animation,
            frame,
            local_to_world,
            instance_rotation,
            origin,
            root_delta,
            pose_offset,
        );
        let Some(bounds) = animated_model_world_bounds(model, &joint_world_transforms) else {
            continue;
        };
        let center = bounds.center();
        sum_x += center.x as i64;
        sum_z += center.z as i64;
        sampled += 1;
        match &mut all_bounds {
            Some(all_bounds) => all_bounds.include_bounds(bounds),
            None => all_bounds = Some(bounds),
        }
    }

    let bounds = all_bounds?;
    let center = WorldVertex::new(
        clamp_i32_from_i64(sum_x / sampled.max(1)),
        midpoint_i32(bounds.min.y, bounds.max.y),
        clamp_i32_from_i64(sum_z / sampled.max(1)),
    );
    Some(PreviewFocus {
        center,
        radius: preview_radius_for_focus(bounds, center, fallback_height),
        floor_y: bounds.min.y,
    })
}

fn clamp_i32_from_i64(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn build_joint_world_transforms_at_frame(
    model: &Model<'_>,
    animation: &Animation<'_>,
    frame: u16,
    local_to_world: LocalToWorldScale,
    instance_rotation: Mat3I16,
    origin: WorldVertex,
    root_delta: Option<[i32; 3]>,
    pose_offset: [i32; 3],
) -> Vec<JointWorldTransform> {
    let joint_count = model.joint_count().min(animation.joint_count()) as usize;
    let mut transforms = vec![JointWorldTransform::ZERO; joint_count];
    for (joint, transform) in transforms.iter_mut().enumerate() {
        if let Some(mut pose) = animation.pose(frame, joint as u16) {
            if let Some(delta) = root_delta {
                apply_root_motion_delta(&mut pose, delta);
            }
            apply_pose_offset(&mut pose, pose_offset);
            *transform =
                compute_joint_world_transform(pose, instance_rotation, local_to_world, origin);
        }
    }
    transforms
}

fn root_motion_delta_at_frame(animation: &Animation<'_>, frame: u16) -> Option<[i32; 3]> {
    let first = animation.pose(0, 0)?;
    let current = animation.pose(frame, 0)?;
    Some(root_motion_delta(first, current))
}

fn root_motion_delta_q12(animation: &Animation<'_>, frame_q12: u32) -> Option<[i32; 3]> {
    let first = animation.pose(0, 0)?;
    let current = animation.pose_looped_q12(frame_q12, 0)?;
    Some(root_motion_delta(first, current))
}

fn root_motion_delta(first: JointPose, current: JointPose) -> [i32; 3] {
    [
        current.translation.x.saturating_sub(first.translation.x),
        0,
        current.translation.z.saturating_sub(first.translation.z),
    ]
}

fn apply_root_motion_delta(pose: &mut JointPose, delta: [i32; 3]) {
    pose.translation.x = pose.translation.x.saturating_sub(delta[0]);
    pose.translation.y = pose.translation.y.saturating_sub(delta[1]);
    pose.translation.z = pose.translation.z.saturating_sub(delta[2]);
}

fn apply_pose_offset(pose: &mut JointPose, offset: [i32; 3]) {
    pose.translation.x = pose.translation.x.saturating_add(offset[0]);
    pose.translation.y = pose.translation.y.saturating_add(offset[1]);
    pose.translation.z = pose.translation.z.saturating_add(offset[2]);
}

fn animated_model_world_bounds(
    model: &Model<'_>,
    joint_transforms: &[JointWorldTransform],
) -> Option<PreviewWorldBounds> {
    let mut bounds: Option<PreviewWorldBounds> = None;
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let primary_joint = part.joint_index() as usize;
        let Some(primary) = joint_transforms.get(primary_joint).copied() else {
            continue;
        };
        let start = part.first_vertex();
        let end = start.saturating_add(part.vertex_count());
        for vertex_index in start..end {
            let Some(vertex) = model.vertex(vertex_index) else {
                continue;
            };
            let point = transform_import_model_vertex_world(vertex, primary, joint_transforms);
            match &mut bounds {
                Some(bounds) => bounds.include(point),
                None => bounds = Some(PreviewWorldBounds::new(point)),
            }
        }
    }
    bounds
}

fn transform_import_model_vertex_world(
    vertex: ModelVertex,
    primary: JointWorldTransform,
    joint_transforms: &[JointWorldTransform],
) -> WorldVertex {
    if vertex.is_blend() && (vertex.joint1 as usize) < joint_transforms.len() {
        let secondary = joint_transforms[vertex.joint1 as usize];
        lerp_world_vertex(
            world_transform_model_vertex(&primary, vertex.position),
            world_transform_model_vertex(&secondary, vertex.position),
            vertex.blend,
        )
    } else {
        world_transform_model_vertex(&primary, vertex.position)
    }
}

fn world_transform_model_vertex(transform: &JointWorldTransform, position: Vec3I16) -> WorldVertex {
    let vx = position.x as i32;
    let vy = position.y as i32;
    let vz = position.z as i32;
    let m = &transform.rotation.m;
    let x = ((m[0][0] as i32) * vx + (m[0][1] as i32) * vy + (m[0][2] as i32) * vz) >> 12;
    let y = ((m[1][0] as i32) * vx + (m[1][1] as i32) * vy + (m[1][2] as i32) * vz) >> 12;
    let z = ((m[2][0] as i32) * vx + (m[2][1] as i32) * vy + (m[2][2] as i32) * vz) >> 12;
    WorldVertex::new(
        x.saturating_add(transform.translation.x),
        y.saturating_add(transform.translation.y),
        z.saturating_add(transform.translation.z),
    )
}

fn lerp_world_vertex(a: WorldVertex, b: WorldVertex, t: u8) -> WorldVertex {
    let t = t as i32;
    let inv = 256 - t;
    WorldVertex::new(
        ((a.x.saturating_mul(inv)).saturating_add(b.x.saturating_mul(t))) >> 8,
        ((a.y.saturating_mul(inv)).saturating_add(b.y.saturating_mul(t))) >> 8,
        ((a.z.saturating_mul(inv)).saturating_add(b.z.saturating_mul(t))) >> 8,
    )
}

fn draw_floor_grid(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    focus: Option<PreviewFocus>,
    origin: WorldVertex,
    height: i32,
) {
    let center = focus.map(|focus| focus.center).unwrap_or(origin);
    let floor_y = focus.map(|focus| focus.floor_y).unwrap_or(0);
    let extent = (height.saturating_mul(2) / 3).clamp(256, 1024);
    let step = (extent / 4).max(64);
    let line_count = 4;
    let line = Color32::from_rgb(42, 48, 56);
    let axis = Color32::from_rgb(74, 84, 96);

    for i in -line_count..=line_count {
        let offset = i * step;
        let color = if i == 0 { axis } else { line };
        let z = center.z.saturating_add(offset);
        draw_projected_world_line(
            image,
            camera,
            projection,
            WorldVertex::new(center.x.saturating_sub(extent), floor_y, z),
            WorldVertex::new(center.x.saturating_add(extent), floor_y, z),
            color,
        );
        let x = center.x.saturating_add(offset);
        draw_projected_world_line(
            image,
            camera,
            projection,
            WorldVertex::new(x, floor_y, center.z.saturating_sub(extent)),
            WorldVertex::new(x, floor_y, center.z.saturating_add(extent)),
            color,
        );
    }
}

fn draw_height_and_collision_guides(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    focus: Option<PreviewFocus>,
    origin: WorldVertex,
    height: i32,
    collision_radius: i32,
) {
    let floor_y = focus
        .map(|focus| focus.floor_y)
        .unwrap_or(origin.y - height / 2);
    let top_y = floor_y.saturating_add(height.max(1));
    let center_x = origin.x;
    let center_z = origin.z;
    let height_color = Color32::from_rgb(255, 210, 92);
    let radius_color = Color32::from_rgb(88, 205, 255);

    let height_x = center_x.saturating_sub(collision_radius.max(96).saturating_add(80));
    draw_projected_world_line(
        image,
        camera,
        projection,
        WorldVertex::new(height_x, floor_y, center_z),
        WorldVertex::new(height_x, top_y, center_z),
        height_color,
    );
    for y in [floor_y, top_y] {
        draw_projected_world_line(
            image,
            camera,
            projection,
            WorldVertex::new(height_x.saturating_sub(48), y, center_z),
            WorldVertex::new(height_x.saturating_add(48), y, center_z),
            height_color,
        );
    }

    let radius = collision_radius.clamp(1, 8192);
    draw_collision_ellipse(
        image,
        camera,
        projection,
        center_x,
        floor_y,
        center_z,
        radius,
        radius_color,
    );
    draw_collision_ellipse(
        image,
        camera,
        projection,
        center_x,
        top_y,
        center_z,
        radius,
        Color32::from_rgb(62, 150, 190),
    );
    for (x, z) in [
        (center_x.saturating_add(radius), center_z),
        (center_x.saturating_sub(radius), center_z),
        (center_x, center_z.saturating_add(radius)),
        (center_x, center_z.saturating_sub(radius)),
    ] {
        draw_projected_world_line(
            image,
            camera,
            projection,
            WorldVertex::new(x, floor_y, z),
            WorldVertex::new(x, top_y, z),
            radius_color,
        );
    }
}

fn draw_collision_ellipse(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    center_x: i32,
    y: i32,
    center_z: i32,
    radius: i32,
    color: Color32,
) {
    const SEGMENTS: i32 = 32;
    let mut prev = collision_ring_point(center_x, y, center_z, radius, SEGMENTS - 1, SEGMENTS);
    for index in 0..SEGMENTS {
        let next = collision_ring_point(center_x, y, center_z, radius, index, SEGMENTS);
        draw_projected_world_line(image, camera, projection, prev, next, color);
        prev = next;
    }
}

fn collision_ring_point(
    center_x: i32,
    y: i32,
    center_z: i32,
    radius: i32,
    index: i32,
    segments: i32,
) -> WorldVertex {
    let angle_q12 = ((index * 4096) / segments).rem_euclid(4096) as u16;
    let angle = Angle::from_q12(angle_q12);
    WorldVertex::new(
        center_x.saturating_add(angle.sin().mul_i32(radius)),
        y,
        center_z.saturating_add(angle.cos().mul_i32(radius)),
    )
}

fn draw_projected_world_line(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    a: WorldVertex,
    b: WorldVertex,
    color: Color32,
) {
    let Some(a) = project_preview_world(camera, projection, a) else {
        return;
    };
    let Some(b) = project_preview_world(camera, projection, b) else {
        return;
    };
    draw_image_line(
        image,
        a.sx as i32,
        a.sy as i32,
        b.sx as i32,
        b.sy as i32,
        color,
    );
}

fn project_preview_world(
    camera: WorldCamera,
    _projection: WorldProjection,
    vertex: WorldVertex,
) -> Option<ProjectedVertex> {
    camera.project_world(vertex)
}

fn body_anchor_projected(
    camera: WorldCamera,
    projection: WorldProjection,
    focus_center: Option<WorldVertex>,
    fallback: WorldVertex,
) -> Option<ProjectedVertex> {
    project_preview_world(camera, projection, focus_center.unwrap_or(fallback))
}

#[derive(Copy, Clone)]
struct PreviewVertex {
    x: f32,
    y: f32,
    z: f32,
    u: f32,
    v: f32,
}

impl PreviewVertex {
    /// Shift byte-space UVs before interpolation; sampling wraps, so a
    /// triangle crossing the seam tiles instead of smearing.
    fn with_uv_offset(mut self, offset: [u8; 2]) -> Self {
        self.u += offset[0] as f32;
        self.v += offset[1] as f32;
        self
    }

    fn from_projected(projected: ProjectedVertex, uv: (u8, u8)) -> Self {
        Self {
            x: projected.sx as f32,
            y: projected.sy as f32,
            z: projected.sz as f32,
            u: uv.0 as f32,
            v: uv.1 as f32,
        }
    }
}

fn project_import_model_vertex(
    vertex: ModelVertex,
    primary: JointWorldTransform,
    joint_transforms: &[JointWorldTransform],
    camera: WorldCamera,
) -> Option<ProjectedVertex> {
    let world = transform_import_model_vertex_world(vertex, primary, joint_transforms);
    camera.project_world(world)
}

fn estimated_joint_points(
    model: &Model<'_>,
    joint_transforms: &[JointWorldTransform],
    camera: WorldCamera,
) -> Vec<Option<ProjectedVertex>> {
    let mut sums = vec![[0i64; 3]; joint_transforms.len()];
    let mut counts = vec![0i64; joint_transforms.len()];
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let joint = part.joint_index() as usize;
        if joint >= joint_transforms.len() {
            continue;
        }
        let start = part.first_vertex();
        let end = start.saturating_add(part.vertex_count());
        for vertex_index in start..end {
            let Some(vertex) = model.vertex(vertex_index) else {
                continue;
            };
            sums[joint][0] += vertex.position.x as i64;
            sums[joint][1] += vertex.position.y as i64;
            sums[joint][2] += vertex.position.z as i64;
            counts[joint] += 1;
        }
    }

    joint_transforms
        .iter()
        .enumerate()
        .map(|(joint, transform)| {
            if counts[joint] > 0 {
                let local = Vec3I16::new(
                    clamp_i16((sums[joint][0] / counts[joint]) as i32),
                    clamp_i16((sums[joint][1] / counts[joint]) as i32),
                    clamp_i16((sums[joint][2] / counts[joint]) as i32),
                );
                camera.project_world(world_transform_model_vertex(transform, local))
            } else {
                None
            }
        })
        .collect()
}

fn raster_textured_triangle(
    image: &mut ColorImage,
    z_buffer: &mut [f32],
    atlas: &ColorImage,
    tri: [PreviewVertex; 3],
    double_sided: bool,
    wrap_uv: bool,
) {
    let width = image.size[0];
    let height = image.size[1];
    if width == 0 || height == 0 || z_buffer.len() < width.saturating_mul(height) {
        return;
    }
    let area = edge(tri[0], tri[1], tri[2]);
    if area.abs() < f32::EPSILON {
        return;
    }
    // Backface-cull with the engine's sign (front faces project with
    // positive NCLIP, which is negative `edge` here) so the preview
    // shows winding problems instead of silently drawing both sides;
    // a source asset relying on double-sided materials used to look
    // fine here and render inside-out in the scene. Double-sided models
    // skip the cull (the fill below already handles both windings) so
    // the preview matches the in-game double-sided render.
    if !double_sided && area > 0.0 {
        return;
    }

    let min_x = tri
        .iter()
        .map(|p| p.x.floor() as i32)
        .min()
        .unwrap_or(0)
        .clamp(0, width as i32 - 1);
    let max_x = tri
        .iter()
        .map(|p| p.x.ceil() as i32)
        .max()
        .unwrap_or(0)
        .clamp(0, width as i32 - 1);
    let min_y = tri
        .iter()
        .map(|p| p.y.floor() as i32)
        .min()
        .unwrap_or(0)
        .clamp(0, height as i32 - 1);
    let max_y = tri
        .iter()
        .map(|p| p.y.ceil() as i32)
        .max()
        .unwrap_or(0)
        .clamp(0, height as i32 - 1);
    if min_x > max_x || min_y > max_y {
        return;
    }

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = PreviewVertex {
                x: x as f32 + 0.5,
                y: y as f32 + 0.5,
                z: 0.0,
                u: 0.0,
                v: 0.0,
            };
            let w0 = edge(tri[1], tri[2], p);
            let w1 = edge(tri[2], tri[0], p);
            let w2 = edge(tri[0], tri[1], p);
            let inside = if area > 0.0 {
                w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
            } else {
                w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
            };
            if !inside {
                continue;
            }

            let b0 = w0 / area;
            let b1 = w1 / area;
            let b2 = w2 / area;
            let depth = tri[0].z * b0 + tri[1].z * b1 + tri[2].z * b2;
            let index = y as usize * width + x as usize;
            if depth >= z_buffer[index] {
                continue;
            }

            let u = tri[0].u * b0 + tri[1].u * b1 + tri[2].u * b2;
            let v = tri[0].v * b0 + tri[1].v * b1 + tri[2].v * b2;
            image.pixels[index] = if wrap_uv {
                sample_atlas_wrapped(atlas, u, v)
            } else {
                sample_atlas(atlas, u, v)
            };
            z_buffer[index] = depth;
        }
    }
}

fn edge(a: PreviewVertex, b: PreviewVertex, c: PreviewVertex) -> f32 {
    (c.x - a.x) * (b.y - a.y) - (c.y - a.y) * (b.x - a.x)
}

fn sample_atlas(atlas: &ColorImage, u: f32, v: f32) -> Color32 {
    let x = (u.round() as i32).clamp(0, atlas.size[0] as i32 - 1) as usize;
    let y = (v.round() as i32).clamp(0, atlas.size[1] as i32 - 1) as usize;
    atlas.pixels[y * atlas.size[0] + x]
}

/// Tiled sampling for material-override atlases: byte-space model UVs
/// repeat a smaller generated texture (the PS1 texture-window look).
fn sample_atlas_wrapped(atlas: &ColorImage, u: f32, v: f32) -> Color32 {
    let x = (u.round() as i32).rem_euclid(atlas.size[0].max(1) as i32) as usize;
    let y = (v.round() as i32).rem_euclid(atlas.size[1].max(1) as i32) as usize;
    atlas.pixels[y * atlas.size[0] + x]
}

fn draw_animation_root_marker(image: &mut ColorImage, projected: ProjectedVertex) {
    let cx = projected.sx as i32;
    let cy = projected.sy as i32;
    for d in -7..=7 {
        put_marker_pixel(image, cx + d, cy, Color32::from_rgb(30, 220, 255));
        put_marker_pixel(image, cx, cy + d, Color32::from_rgb(30, 220, 255));
    }
    for y in -3..=3 {
        for x in -3..=3 {
            if x * x + y * y <= 9 {
                put_marker_pixel(image, cx + x, cy + y, Color32::from_rgb(255, 60, 170));
            }
        }
    }
    for y in -5i32..=5 {
        for x in -5i32..=5 {
            let edge = x.abs().max(y.abs()) == 5;
            if edge {
                put_marker_pixel(image, cx + x, cy + y, Color32::WHITE);
            }
        }
    }
}

fn draw_bone_overlay(
    image: &mut ColorImage,
    model: &Model<'_>,
    joints: &[Option<ProjectedVertex>],
) {
    let line = Color32::from_rgb(80, 230, 255);
    let dot = Color32::from_rgb(255, 244, 130);
    let count = model.joint_count().min(joints.len() as u16);
    for joint_index in 0..count {
        let Some(joint) = model.joint(joint_index) else {
            continue;
        };
        let Some(parent) = joint.parent() else {
            continue;
        };
        if parent >= count {
            continue;
        }
        let Some(a) = joints.get(parent as usize).and_then(|p| *p) else {
            continue;
        };
        let Some(b) = joints.get(joint_index as usize).and_then(|p| *p) else {
            continue;
        };
        draw_image_line(
            image,
            a.sx as i32,
            a.sy as i32,
            b.sx as i32,
            b.sy as i32,
            line,
        );
    }
    for projected in joints.iter().flatten() {
        draw_joint_dot(image, projected.sx as i32, projected.sy as i32, dot);
    }
}

fn draw_joint_combat_capsule(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    joint: JointWorldTransform,
    capsule: &PreviewCombatCapsule,
) {
    let radius = i32::from(capsule.radius.max(1));
    let line = if capsule.selected {
        // Gold remains legible over Aletha's pale body material while matching
        // the editor's selection language. White disappeared against the
        // model in the real headless Animation Studio capture.
        Color32::from_rgb(255, 194, 72)
    } else {
        capsule.color
    };

    // Three great circles at each endpoint keep the volume readable from any
    // camera angle. Their local axes rotate with the animated joint.
    for center in [capsule.start, capsule.end] {
        for plane in [(0usize, 1usize), (0, 2), (1, 2)] {
            let mut previous = None;
            for step in 0..=24 {
                let angle = Angle::from_q12(((step * 4096) / 24) as u16);
                let mut local = center;
                local[plane.0] = local[plane.0].saturating_add(angle.sin().mul_i32(radius));
                local[plane.1] = local[plane.1].saturating_add(angle.cos().mul_i32(radius));
                let world = joint_local_point(joint, local);
                if let Some(previous) = previous {
                    draw_projected_world_line(image, camera, projection, previous, world, line);
                }
                previous = Some(world);
            }
        }
    }

    // Connect matching cardinal points on the endpoint spheres. This is a
    // deliberately clear editor wireframe rather than runtime collision math.
    for offset in [
        [radius, 0, 0],
        [-radius, 0, 0],
        [0, radius, 0],
        [0, -radius, 0],
        [0, 0, radius],
        [0, 0, -radius],
    ] {
        let a = joint_local_point(joint, add_i32x3(capsule.start, offset));
        let b = joint_local_point(joint, add_i32x3(capsule.end, offset));
        draw_projected_world_line(image, camera, projection, a, b, line);
    }
}

/// Rasterise an equipped weapon riding a socket into the preview,
/// sharing the character's z-buffer so occlusion matches Play.
///
/// The placement math is the runtime's equipment composition verbatim:
/// socket pose = unscaled joint basis x socket Euler, weapon rotation =
/// socket rotation x grip inverse, and the origin backs the scaled grip
/// offset out of the socket origin. The weapon renders at its bind
/// pose (cooked static props ship a 1-frame identity `bind_pose`, the
/// same frame the runtime's clip sampling lands on).
#[allow(clippy::too_many_arguments)]
fn draw_equipped_weapon_overlay(
    image: &mut ColorImage,
    z_buffer: &mut [f32],
    camera: WorldCamera,
    weapon: &PreviewEquippedWeapon<'_>,
    animation: &Animation<'_>,
    frame_q12: u32,
    root_delta: Option<[i32; 3]>,
    pose_offset: [i32; 3],
    instance_rotation: Mat3I16,
    joint_transforms: &[JointWorldTransform],
) -> Option<()> {
    let weapon_model = Model::from_bytes(weapon.model_bytes).ok()?;
    let materialization_q12 = weapon.materialization_q12.min(4096);
    if materialization_q12 == 0 {
        return Some(());
    }
    let joint = joint_transforms
        .get(weapon.socket_joint as usize)
        .copied()?;
    let mut pose = animation.pose_looped_q12(frame_q12, weapon.socket_joint)?;
    if let Some(delta) = root_delta {
        apply_root_motion_delta(&mut pose, delta);
    }
    apply_pose_offset(&mut pose, pose_offset);

    let socket_origin = joint_local_point(joint, weapon.socket_translation);
    let socket_rotation =
        compute_joint_world_basis(pose, instance_rotation).mul(&euler_rotation_q12([
            weapon.socket_rotation_q12[0] as u16,
            weapon.socket_rotation_q12[1] as u16,
            weapon.socket_rotation_q12[2] as u16,
        ]));
    let weapon_rotation =
        socket_rotation.mul(&euler_rotation_q12_inverse(weapon.grip_rotation_q12));
    let weapon_local_to_world = LocalToWorldScale::from_q12(weapon_model.local_to_world_q12());
    let grip = [
        weapon_local_to_world.apply(weapon.grip_translation[0]),
        weapon_local_to_world.apply(weapon.grip_translation[1]),
        weapon_local_to_world.apply(weapon.grip_translation[2]),
    ];
    let grip_world = basis_offset_point(WorldVertex::ZERO, &weapon_rotation, grip);
    let weapon_origin = WorldVertex::new(
        socket_origin.x.saturating_sub(grip_world.x),
        socket_origin.y.saturating_sub(grip_world.y),
        socket_origin.z.saturating_sub(grip_world.z),
    );
    let identity_pose = JointPose {
        matrix: [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]],
        translation: Vec3I32::new(0, 0, 0),
    };
    let weapon_joint = compute_joint_world_transform(
        identity_pose,
        weapon_rotation,
        weapon_local_to_world,
        weapon_origin,
    );

    let weapon_transforms = [weapon_joint];
    let mut projected = vec![None; weapon_model.vertex_count() as usize];
    for (vertex_index, slot) in projected.iter_mut().enumerate() {
        let Some(vertex) = weapon_model.vertex(vertex_index as u16) else {
            continue;
        };
        *slot = project_import_model_vertex(vertex, weapon_joint, &weapon_transforms, camera);
    }
    let double_sided = weapon_model.double_sided();
    let visible_faces = (usize::from(weapon_model.face_count())
        .saturating_mul(usize::from(materialization_q12))
        .saturating_add(4095))
        / 4096;
    let mut visited_faces = 0usize;
    for part_index in 0..weapon_model.part_count() {
        let Some(part) = weapon_model.part(part_index) else {
            continue;
        };
        let first_face = part.first_face();
        let last_face = first_face.saturating_add(part.face_count());
        for face_index in first_face..last_face {
            if visited_faces >= visible_faces {
                return Some(());
            }
            visited_faces = visited_faces.saturating_add(1);
            let Some(face) = weapon_model.face(face_index) else {
                continue;
            };
            let [a, b, c] = face.corners;
            let Some(pa) = projected.get(a.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            let Some(pb) = projected.get(b.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            let Some(pc) = projected.get(c.vertex_index as usize).and_then(|v| *v) else {
                continue;
            };
            if weapon.wireframe_materialization {
                let points = [pa, pb, pc];
                let mut longest = (i64::MIN, 0usize, 1usize);
                for edge in 0..3 {
                    let next = (edge + 1) % 3;
                    let dx = i64::from(points[edge].sx) - i64::from(points[next].sx);
                    let dy = i64::from(points[edge].sy) - i64::from(points[next].sy);
                    let length_sq = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
                    if length_sq > longest.0 {
                        longest = (length_sq, edge, next);
                    }
                }
                let from = points[longest.1];
                let to = points[longest.2];
                draw_image_line(
                    image,
                    i32::from(from.sx),
                    i32::from(from.sy),
                    i32::from(to.sx),
                    i32::from(to.sy),
                    Color32::from_rgb(32, 255, 128),
                );
                continue;
            }
            raster_textured_triangle(
                image,
                z_buffer,
                weapon.atlas,
                [
                    PreviewVertex::from_projected(pa, a.uv),
                    PreviewVertex::from_projected(pb, b.uv),
                    PreviewVertex::from_projected(pc, c.uv),
                ],
                double_sided,
                false,
            );
        }
    }
    Some(())
}

/// Inverse of [`euler_rotation_q12`]: negate each angle and compose in
/// the opposite order, matching the runtime's grip-inverse helper.
fn euler_rotation_q12_inverse(rotation_q12: [i16; 3]) -> Mat3I16 {
    let inv_x = (-(rotation_q12[0] as i32)) as u16;
    let inv_y = (-(rotation_q12[1] as i32)) as u16;
    let inv_z = (-(rotation_q12[2] as i32)) as u16;
    let rx = Mat3I16::rotate_x(Angle::from_q12(inv_x).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(inv_y).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(inv_z).rotate_y_arg());
    rx.mul(&ry).mul(&rz)
}

/// Draw a socket as a bone-local axis triad: X red, Y green, Z blue
/// whiskers from the socket origin, plus a short white origin cross on
/// the selected socket. `basis` is the composed orthonormal socket
/// orientation; the origin rides the SCALED joint matrix (same offset
/// convention as the runtime and the combat capsules).
fn draw_socket_marker(
    image: &mut ColorImage,
    camera: WorldCamera,
    projection: WorldProjection,
    joint: JointWorldTransform,
    basis: &Mat3I16,
    socket: &PreviewSocket,
    axis_length: i32,
) {
    let origin = joint_local_point(joint, socket.translation);
    let dim = |color: Color32| {
        if socket.selected {
            color
        } else {
            Color32::from_rgb(color.r() / 2, color.g() / 2, color.b() / 2)
        }
    };
    let axes = [
        ([axis_length, 0, 0], dim(Color32::from_rgb(240, 92, 92))),
        ([0, axis_length, 0], dim(Color32::from_rgb(110, 226, 110))),
        ([0, 0, axis_length], dim(Color32::from_rgb(116, 160, 255))),
    ];
    for (local, color) in axes {
        let end = basis_offset_point(origin, basis, local);
        draw_projected_world_line(image, camera, projection, origin, end, color);
    }
    if socket.selected {
        let arm = (axis_length / 4).max(24);
        for local in [[arm, 0, 0], [0, arm, 0], [0, 0, arm]] {
            let negative = [-local[0], -local[1], -local[2]];
            let a = basis_offset_point(origin, basis, negative);
            let b = basis_offset_point(origin, basis, local);
            draw_projected_world_line(image, camera, projection, a, b, Color32::WHITE);
        }
    }
}

/// Offset a world point along an orthonormal Q12 basis.
fn basis_offset_point(origin: WorldVertex, basis: &Mat3I16, local: [i32; 3]) -> WorldVertex {
    let rotate = |row: [i16; 3]| {
        i32::from(row[0])
            .saturating_mul(local[0])
            .saturating_add(i32::from(row[1]).saturating_mul(local[1]))
            .saturating_add(i32::from(row[2]).saturating_mul(local[2]))
            >> 12
    };
    WorldVertex::new(
        origin.x.saturating_add(rotate(basis.m[0])),
        origin.y.saturating_add(rotate(basis.m[1])),
        origin.z.saturating_add(rotate(basis.m[2])),
    )
}

fn add_i32x3(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[0].saturating_add(b[0]),
        a[1].saturating_add(b[1]),
        a[2].saturating_add(b[2]),
    ]
}

fn joint_local_point(joint: JointWorldTransform, local: [i32; 3]) -> WorldVertex {
    let rotate = |row: [i16; 3]| {
        i32::from(row[0])
            .saturating_mul(local[0])
            .saturating_add(i32::from(row[1]).saturating_mul(local[1]))
            .saturating_add(i32::from(row[2]).saturating_mul(local[2]))
            >> 12
    };
    WorldVertex::new(
        joint
            .translation
            .x
            .saturating_add(rotate(joint.rotation.m[0])),
        joint
            .translation
            .y
            .saturating_add(rotate(joint.rotation.m[1])),
        joint
            .translation
            .z
            .saturating_add(rotate(joint.rotation.m[2])),
    )
}

fn draw_selected_joint_dot(image: &mut ColorImage, cx: i32, cy: i32) {
    for radius in [6, 5] {
        let color = if radius == 6 {
            Color32::BLACK
        } else {
            Color32::from_rgb(255, 218, 92)
        };
        for y in -radius..=radius {
            for x in -radius..=radius {
                let d2 = x * x + y * y;
                if d2 >= (radius - 1) * (radius - 1) && d2 <= radius * radius {
                    put_marker_pixel(image, cx + x, cy + y, color);
                }
            }
        }
    }
}

fn draw_image_line(
    image: &mut ColorImage,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    color: Color32,
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        put_marker_pixel(image, x0, y0, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = err.saturating_mul(2);
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn draw_joint_dot(image: &mut ColorImage, cx: i32, cy: i32, color: Color32) {
    for y in -1..=1 {
        for x in -1..=1 {
            put_marker_pixel(image, cx + x, cy + y, color);
        }
    }
}

fn put_marker_pixel(image: &mut ColorImage, x: i32, y: i32, color: Color32) {
    let width = image.size[0];
    let height = image.size[1];
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    image.pixels[y as usize * width + x as usize] = color;
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::Q12;
    use std::path::Path;

    fn looking_positive_z_camera() -> WorldCamera {
        WorldCamera::from_basis(
            WorldProjection::new(160, 120, 320, 1),
            WorldVertex::ZERO,
            Q12::ZERO,
            Q12::NEG_ONE,
            Q12::ZERO,
            Q12::ONE,
        )
    }

    #[test]
    fn invalid_secondary_blend_uses_primary_transform() {
        let primary = JointWorldTransform {
            rotation: Mat3I16::IDENTITY,
            translation: WorldVertex::new(0, 0, 100),
        };
        let vertex = ModelVertex {
            position: Vec3I16::new(0, 0, 0),
            joint1: 99,
            blend: 128,
        };
        let projected =
            project_import_model_vertex(vertex, primary, &[primary], looking_positive_z_camera())
                .expect("invalid secondary joint should stay on primary path");

        assert_eq!(projected, ProjectedVertex::new(160, 120, 100));
    }

    // Temporary visual-inspection dump: renders the tracked wraith preview
    // to /tmp/preview_dump.ppm so orientation can be checked by eye.
    #[test]
    #[ignore]
    fn dump_preview_for_inspection() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let model_path = std::env::var("DUMP_MODEL").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith.psxmdl")
                .to_string_lossy()
                .into_owned()
        });
        let clip_path = std::env::var("DUMP_CLIP").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith_idle.psxanim")
                .to_string_lossy()
                .into_owned()
        });
        let model = std::fs::read(model_path).expect("model fixture");
        let clip = std::fs::read(clip_path).expect("animation fixture");
        let atlas = ColorImage {
            size: [128, 128],
            pixels: vec![Color32::from_rgb(210, 90, 70); 128 * 128],
        };
        let image = render_import_model_preview_with_options(
            &model,
            &clip,
            &atlas,
            ImportPreviewOptions {
                world_height: 1024,
                visual_scale_q8: MODEL_SCALE_ONE_Q8,
                visual_yaw_q12: 0,
                collision_radius: 192,
                time_seconds: 0.0,
                yaw_q12: 0,
                pitch_q12: 64,
                radius: 2048,
                focus_on_animated_bounds: true,
                preview_in_place: true,
                pose_offset: [0, 0, 0],
                show_animation_root: true,
                show_collision_guides: true,
                show_bones: false,
            },
        )
        .expect("tracked cooked model should render");
        let mut out = format!("P6\n{} {}\n255\n", image.size[0], image.size[1]).into_bytes();
        for pixel in &image.pixels {
            out.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
        }
        std::fs::write("/tmp/preview_dump.ppm", out).expect("write ppm");
    }

    #[test]
    fn atlas_sampling_clamps_uvs() {
        let image = ColorImage {
            size: [2, 2],
            pixels: vec![
                Color32::from_rgb(1, 0, 0),
                Color32::from_rgb(2, 0, 0),
                Color32::from_rgb(3, 0, 0),
                Color32::from_rgb(4, 0, 0),
            ],
        };

        assert_eq!(
            sample_atlas(&image, -10.0, -10.0),
            Color32::from_rgb(1, 0, 0)
        );
        assert_eq!(sample_atlas(&image, 99.0, 99.0), Color32::from_rgb(4, 0, 0));
    }

    #[test]
    fn socket_markers_draw_over_the_wraith_preview() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let model = std::fs::read(std::env::var("DUMP_MODEL").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith.psxmdl")
                .to_string_lossy()
                .into_owned()
        }))
        .expect("tracked model fixture");
        let clip = std::fs::read(std::env::var("DUMP_CLIP").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith_idle.psxanim")
                .to_string_lossy()
                .into_owned()
        }))
        .expect("tracked animation fixture");
        let atlas = ColorImage {
            size: [128, 128],
            pixels: vec![Color32::from_rgb(210, 90, 70); 128 * 128],
        };
        let options = ImportPreviewOptions {
            world_height: 1024,
            visual_scale_q8: MODEL_SCALE_ONE_Q8,
            visual_yaw_q12: 0,
            collision_radius: 192,
            time_seconds: 0.0,
            yaw_q12: 340,
            pitch_q12: 350,
            radius: 1536,
            focus_on_animated_bounds: true,
            preview_in_place: true,
            pose_offset: [0, 0, 0],
            show_animation_root: false,
            show_collision_guides: false,
            show_bones: false,
        };
        let render = |sockets: &[PreviewSocket]| {
            render_import_model_preview_with_combat_capsules_at_size(
                &model,
                &clip,
                &atlas,
                options,
                yaw_rotation_matrix(0),
                [PREVIEW_WIDTH, PREVIEW_HEIGHT],
                &[],
                sockets,
                None,
                None,
                None,
            )
            .expect("preview renders")
            .image
        };
        let socket_joint = std::env::var("DUMP_SOCKET_JOINT")
            .ok()
            .and_then(|joint| joint.parse().ok())
            .unwrap_or(0u16);
        let plain = render(&[]);
        let marked = render(&[PreviewSocket {
            joint: socket_joint,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
            selected: true,
        }]);
        assert_ne!(
            plain.pixels, marked.pixels,
            "the socket triad must paint over the preview"
        );
        // Optional visual-inspection dump, same convention as
        // dump_preview_for_inspection.
        if let Ok(path) = std::env::var("DUMP_SOCKET_PREVIEW") {
            let mut out = format!("P6\n{} {}\n255\n", marked.size[0], marked.size[1]).into_bytes();
            for pixel in &marked.pixels {
                out.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
            }
            std::fs::write(path, out).expect("write socket preview ppm");
        }
    }

    fn env_vec3(name: &str) -> Option<[i32; 3]> {
        let value = std::env::var(name).ok()?;
        let parts: Vec<i32> = value
            .split(',')
            .filter_map(|part| part.trim().parse().ok())
            .collect();
        (parts.len() == 3).then(|| [parts[0], parts[1], parts[2]])
    }

    #[test]
    fn equipped_weapon_overlay_rides_the_socket() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let model = std::fs::read(std::env::var("DUMP_MODEL").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith.psxmdl")
                .to_string_lossy()
                .into_owned()
        }))
        .expect("tracked model fixture");
        let clip = std::fs::read(std::env::var("DUMP_CLIP").unwrap_or_else(|_| {
            root.join("assets/models/obsidian_wraith/obsidian_wraith_idle.psxanim")
                .to_string_lossy()
                .into_owned()
        }))
        .expect("tracked animation fixture");
        let atlas = ColorImage {
            size: [128, 128],
            pixels: vec![Color32::from_rgb(210, 90, 70); 128 * 128],
        };
        // Default: the wraith doubles as its own weapon; env overrides
        // point at a real weapon model + atlas for visual inspection.
        let weapon_bytes = std::env::var("DUMP_WEAPON_MODEL")
            .ok()
            .map(|path| std::fs::read(path).expect("weapon model"));
        let weapon_model: &[u8] = weapon_bytes.as_deref().unwrap_or(&model);
        let weapon_atlas = std::env::var("DUMP_WEAPON_ATLAS")
            .ok()
            .and_then(|path| {
                crate::model_animation_viewer::decode_psxt_image(
                    &std::fs::read(path).expect("weapon atlas"),
                )
            })
            .unwrap_or_else(|| ColorImage {
                size: [8, 8],
                pixels: vec![Color32::from_rgb(150, 155, 170); 64],
            });
        // Optional character material override so inspection dumps show
        // the real in-game skin (e.g. the generated crystal hologram)
        // instead of the flat placeholder atlas.
        let material_atlas = std::env::var("DUMP_OVERRIDE_ATLAS").ok().and_then(|path| {
            crate::model_animation_viewer::decode_psxt_image(
                &std::fs::read(path).expect("override atlas"),
            )
        });
        let material_layer = material_atlas.as_ref().map(|atlas| PreviewMaterialLayer {
            atlas,
            motion: psx_level::LevelMaterialUvMotion::default(),
        });
        let options = ImportPreviewOptions {
            world_height: 1024,
            visual_scale_q8: MODEL_SCALE_ONE_Q8,
            visual_yaw_q12: 0,
            collision_radius: 192,
            time_seconds: 0.0,
            yaw_q12: std::env::var("DUMP_YAW")
                .ok()
                .and_then(|yaw| yaw.parse().ok())
                .unwrap_or(340),
            pitch_q12: 350,
            radius: 1536,
            focus_on_animated_bounds: true,
            preview_in_place: true,
            pose_offset: [0, 0, 0],
            show_animation_root: false,
            show_collision_guides: false,
            show_bones: false,
        };
        let render = |weapon: Option<&PreviewEquippedWeapon<'_>>| {
            render_import_model_preview_with_combat_capsules_at_size(
                &model,
                &clip,
                &atlas,
                options,
                yaw_rotation_matrix(0),
                [PREVIEW_WIDTH, PREVIEW_HEIGHT],
                &[],
                &[],
                weapon,
                material_layer.as_ref(),
                None,
            )
            .expect("preview renders")
            .image
        };
        let socket_joint = std::env::var("DUMP_SOCKET_JOINT")
            .ok()
            .and_then(|joint| joint.parse().ok())
            .unwrap_or(0u16);
        let socket_translation = env_vec3("DUMP_SOCKET_T").unwrap_or([0, 0, 0]);
        let socket_rotation = env_vec3("DUMP_SOCKET_R")
            .map(|r| [r[0] as i16, r[1] as i16, r[2] as i16])
            .unwrap_or([0, 0, 0]);

        let bare = render(None);
        let with_weapon = render(Some(&PreviewEquippedWeapon {
            model_bytes: weapon_model,
            atlas: &weapon_atlas,
            socket_joint,
            socket_translation,
            socket_rotation_q12: socket_rotation,
            grip_translation: [0, 0, 0],
            grip_rotation_q12: [0, 0, 0],
            materialization_q12: 4096,
            wireframe_materialization: false,
        }));
        assert_ne!(
            bare.pixels, with_weapon.pixels,
            "the equipped weapon must draw over the preview"
        );
        let moved = render(Some(&PreviewEquippedWeapon {
            model_bytes: weapon_model,
            atlas: &weapon_atlas,
            socket_joint,
            socket_translation: [
                socket_translation[0].saturating_add(12_000),
                socket_translation[1],
                socket_translation[2],
            ],
            socket_rotation_q12: socket_rotation,
            grip_translation: [0, 0, 0],
            grip_rotation_q12: [0, 0, 0],
            materialization_q12: 4096,
            wireframe_materialization: false,
        }));
        assert_ne!(
            with_weapon.pixels, moved.pixels,
            "the weapon must track the socket offset"
        );
        if let Ok(path) = std::env::var("DUMP_WEAPON_PREVIEW") {
            let mut out =
                format!("P6\n{} {}\n255\n", with_weapon.size[0], with_weapon.size[1]).into_bytes();
            for pixel in &with_weapon.pixels {
                out.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
            }
            std::fs::write(path, out).expect("write weapon preview ppm");
        }
    }

    #[test]
    fn tracked_wraith_model_renders_nonblank_preview() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let model =
            std::fs::read(root.join("assets/models/obsidian_wraith/obsidian_wraith.psxmdl"))
                .expect("tracked model fixture");
        let clip =
            std::fs::read(root.join("assets/models/obsidian_wraith/obsidian_wraith_idle.psxanim"))
                .expect("tracked animation fixture");
        let atlas = ColorImage {
            size: [128, 128],
            pixels: vec![Color32::from_rgb(210, 90, 70); 128 * 128],
        };

        let image = render_import_model_preview_with_options(
            &model,
            &clip,
            &atlas,
            ImportPreviewOptions {
                world_height: 1024,
                visual_scale_q8: MODEL_SCALE_ONE_Q8,
                visual_yaw_q12: 0,
                collision_radius: 192,
                time_seconds: 0.0,
                yaw_q12: 340,
                pitch_q12: 350,
                radius: 1536,
                focus_on_animated_bounds: true,
                preview_in_place: true,
                pose_offset: [0, 0, 0],
                show_animation_root: true,
                show_collision_guides: true,
                show_bones: false,
            },
        )
        .expect("tracked cooked model should render");
        let background = Color32::from_rgb(8, 10, 14);
        let lit_pixels = image
            .pixels
            .iter()
            .filter(|pixel| **pixel != background)
            .count();

        assert!(
            lit_pixels > 32,
            "expected the cooked model preview to draw visible pixels, got {lit_pixels}"
        );
    }

    #[test]
    fn authoring_preview_renders_at_requested_size() {
        let model = two_joint_model_with_child_part();
        let clip = two_joint_animation_with_child_x_offsets(&[0, 32]);
        let atlas = ColorImage {
            size: [64, 64],
            pixels: vec![Color32::from_rgb(210, 90, 70); 64 * 64],
        };

        let image = render_import_model_preview_with_orientation_at_size(
            &model,
            &clip,
            &atlas,
            ImportPreviewOptions {
                world_height: 1024,
                visual_scale_q8: MODEL_SCALE_ONE_Q8,
                visual_yaw_q12: 0,
                collision_radius: 192,
                time_seconds: 0.0,
                yaw_q12: 340,
                pitch_q12: 350,
                radius: 1536,
                focus_on_animated_bounds: true,
                preview_in_place: true,
                pose_offset: [0, 0, 0],
                show_animation_root: false,
                show_collision_guides: false,
                show_bones: false,
            },
            Mat3I16::IDENTITY,
            [640, 480],
        )
        .expect("authoring preview should render");

        assert_eq!(image.size, [640, 480]);
        assert_eq!(image.pixels.len(), 640 * 480);
    }

    #[test]
    fn rig_attached_capsule_draws_selection_wireframe() {
        let model = two_joint_model_with_child_part();
        let clip = two_joint_animation_with_child_x_offsets(&[0, 32]);
        let atlas = ColorImage {
            size: [64, 64],
            pixels: vec![Color32::from_rgb(210, 90, 70); 64 * 64],
        };
        let selection = Color32::from_rgb(255, 194, 72);

        let render = render_import_model_preview_with_combat_capsules_at_size(
            &model,
            &clip,
            &atlas,
            ImportPreviewOptions {
                world_height: 1024,
                visual_scale_q8: MODEL_SCALE_ONE_Q8,
                visual_yaw_q12: 0,
                collision_radius: 192,
                time_seconds: 0.0,
                yaw_q12: 340,
                pitch_q12: 350,
                radius: 1536,
                focus_on_animated_bounds: true,
                preview_in_place: true,
                pose_offset: [0, 0, 0],
                show_animation_root: false,
                show_collision_guides: false,
                show_bones: false,
            },
            Mat3I16::IDENTITY,
            [640, 480],
            &[PreviewCombatCapsule {
                joint: 1,
                start: [0, -96, 0],
                end: [0, 96, 0],
                radius: 96,
                color: Color32::CYAN,
                selected: true,
            }],
            &[],
            None,
            None,
            None,
        )
        .expect("rig fixture should render");
        let wire_pixels = render
            .image
            .pixels
            .iter()
            .filter(|pixel| **pixel == selection)
            .count();

        assert!(
            wire_pixels > 24,
            "expected a visible selected capsule wireframe, got {wire_pixels} pixels"
        );
    }

    #[test]
    fn estimated_joint_points_skips_joints_without_owned_vertices() {
        let model_bytes = two_joint_model_with_child_part();
        let model = Model::from_bytes(&model_bytes).expect("model fixture");
        let transforms = vec![
            JointWorldTransform {
                rotation: Mat3I16::IDENTITY,
                translation: WorldVertex::new(40, 0, 100),
            },
            JointWorldTransform {
                rotation: Mat3I16::IDENTITY,
                translation: WorldVertex::new(0, 0, 100),
            },
        ];

        let joints = estimated_joint_points(&model, &transforms, looking_positive_z_camera());

        assert_eq!(joints.len(), 2);
        assert_eq!(joints[0], None);
        assert!(joints[1].is_some());
    }

    #[test]
    fn joint_capsule_fit_uses_owned_vertices_and_allows_a_sphere() {
        let model = two_joint_model_with_child_part();

        assert!(fit_capsule_to_joint(&model, 0, MODEL_SCALE_ONE_Q8).is_none());
        let fit = fit_capsule_to_joint(&model, 1, MODEL_SCALE_ONE_Q8)
            .expect("child joint owns the fixture geometry");

        assert!(fit.radius > 0);
        // This fixture is square in its two populated axes, so the most
        // conservative capsule is a sphere. Equal endpoints are an explicit
        // part of the serialized JointCapsule contract.
        assert_eq!(fit.start, fit.end);
    }

    #[test]
    fn animated_world_bounds_use_transformed_part_vertices() {
        let model_bytes = two_joint_model_with_child_part();
        let model = Model::from_bytes(&model_bytes).expect("model fixture");
        let transforms = vec![
            JointWorldTransform {
                rotation: Mat3I16::IDENTITY,
                translation: WorldVertex::new(0, 0, 0),
            },
            JointWorldTransform {
                rotation: Mat3I16::IDENTITY,
                translation: WorldVertex::new(100, 200, 300),
            },
        ];

        let bounds = animated_model_world_bounds(&model, &transforms).expect("bounds");

        assert_eq!(bounds.min, WorldVertex::new(100, 200, 300));
        assert_eq!(bounds.max, WorldVertex::new(148, 248, 300));
        assert_eq!(bounds.center(), WorldVertex::new(124, 224, 300));
    }

    #[test]
    fn preview_focus_averages_centers_across_all_frames() {
        let model_bytes = two_joint_model_with_child_part();
        let model = Model::from_bytes(&model_bytes).expect("model fixture");
        let animation_bytes = two_joint_animation_with_child_x_offsets(&[0, 200]);
        let animation = Animation::from_bytes(&animation_bytes).expect("animation fixture");

        let focus = animation_preview_focus(
            &model,
            &animation,
            LocalToWorldScale::from_q12(model.local_to_world_q12()),
            Mat3I16::IDENTITY,
            WorldVertex::ZERO,
            1024,
            false,
            [0, 0, 0],
        )
        .expect("focus");

        assert_eq!(focus.center, WorldVertex::new(124, 24, 0));
        assert_eq!(focus.floor_y, 0);
        assert!(focus.radius >= 1536);
    }

    #[test]
    fn in_place_preview_removes_root_motion_from_focus() {
        let model_bytes = two_joint_model_with_child_part();
        let model = Model::from_bytes(&model_bytes).expect("model fixture");
        let animation_bytes = two_joint_animation_with_global_x_offsets(&[0, 200]);
        let animation = Animation::from_bytes(&animation_bytes).expect("animation fixture");

        let moving = animation_preview_focus(
            &model,
            &animation,
            LocalToWorldScale::from_q12(model.local_to_world_q12()),
            Mat3I16::IDENTITY,
            WorldVertex::ZERO,
            1024,
            false,
            [0, 0, 0],
        )
        .expect("moving focus");
        let in_place = animation_preview_focus(
            &model,
            &animation,
            LocalToWorldScale::from_q12(model.local_to_world_q12()),
            Mat3I16::IDENTITY,
            WorldVertex::ZERO,
            1024,
            true,
            [0, 0, 0],
        )
        .expect("in-place focus");

        assert_eq!(moving.center, WorldVertex::new(124, 24, 0));
        assert_eq!(in_place.center, WorldVertex::new(24, 24, 0));
        assert_eq!(in_place.floor_y, 0);
    }

    #[test]
    fn body_anchor_uses_focus_center_not_distant_root() {
        let projection = WorldProjection::new(160, 124, 320, 48);
        let focus_center = WorldVertex::new(0, 512, 0);
        let camera = WorldCamera::orbit(
            projection,
            focus_center,
            1536,
            Angle::from_q12(340),
            Angle::from_q12(350),
        );
        let distant_root = WorldVertex::new(-4096, 2048, -4096);

        let anchor = body_anchor_projected(camera, projection, Some(focus_center), distant_root)
            .expect("focus center should project");
        let expected =
            project_preview_world(camera, projection, focus_center).expect("expected projection");

        assert_eq!(anchor, expected);
    }

    #[test]
    fn floor_grid_draws_grounding_pixels() {
        let background = Color32::from_rgb(8, 10, 14);
        let mut image = ColorImage {
            size: [PREVIEW_WIDTH, PREVIEW_HEIGHT],
            pixels: vec![background; PREVIEW_WIDTH * PREVIEW_HEIGHT],
        };
        let projection = WorldProjection::new(160, 124, 320, 48);
        let focus = PreviewFocus {
            center: WorldVertex::new(0, 512, 0),
            radius: 1536,
            floor_y: 0,
        };
        let camera = WorldCamera::orbit(
            projection,
            focus.center,
            focus.radius,
            Angle::from_q12(340),
            Angle::from_q12(350),
        );

        draw_floor_grid(
            &mut image,
            camera,
            projection,
            Some(focus),
            WorldVertex::new(0, 512, 0),
            1024,
        );

        let grid_pixels = image
            .pixels
            .iter()
            .filter(|pixel| **pixel != background)
            .count();
        assert!(grid_pixels > 16, "expected grid pixels, got {grid_pixels}");
    }

    #[test]
    fn preview_visual_scale_multiplies_model_header_scale() {
        let model_bytes = two_joint_model_with_child_part();
        let model = Model::from_bytes(&model_bytes).expect("model fixture");

        assert_eq!(
            preview_model_local_to_world(&model, MODEL_SCALE_ONE_Q8).q12(),
            4096
        );
        assert_eq!(
            preview_model_local_to_world(&model, MODEL_SCALE_ONE_Q8 * 2).q12(),
            8192
        );
    }

    fn two_joint_model_with_child_part() -> Vec<u8> {
        const ASSET_HEADER_SIZE: usize = 12;
        const MODEL_HEADER_SIZE: usize = 16;
        const JOINT_RECORD_SIZE: usize = 4;
        const MATERIAL_RECORD_SIZE: usize = 8;
        const PART_RECORD_SIZE: usize = 16;
        const VERTEX_RECORD_SIZE: usize = 8;
        const FACE_RECORD_SIZE: usize = 12;
        const MODEL_VERSION: u16 = 4;
        const MODEL_FLAGS_HAS_UVS: u16 = 1 << 1;
        const MODEL_FLAGS_RIGID_SKINNED: u16 = 1 << 2;
        const NO_JOINT: u16 = u16::MAX;
        const NO_JOINT8: u8 = u8::MAX;

        let payload_len = MODEL_HEADER_SIZE
            + 2 * JOINT_RECORD_SIZE
            + MATERIAL_RECORD_SIZE
            + PART_RECORD_SIZE
            + 3 * VERTEX_RECORD_SIZE
            + FACE_RECORD_SIZE;
        let mut out = Vec::with_capacity(ASSET_HEADER_SIZE + payload_len);
        out.extend_from_slice(b"PSMD");
        out.extend_from_slice(&MODEL_VERSION.to_le_bytes());
        out.extend_from_slice(&(MODEL_FLAGS_HAS_UVS | MODEL_FLAGS_RIGID_SKINNED).to_le_bytes());
        out.extend_from_slice(&(payload_len as u32).to_le_bytes());

        append_u16(&mut out, 2); // joints
        append_u16(&mut out, 1); // parts
        append_u16(&mut out, 3); // vertices
        append_u16(&mut out, 1); // faces
        append_u16(&mut out, 1); // materials
        append_u16(&mut out, 128);
        append_u16(&mut out, 128);
        append_u16(&mut out, 0);

        append_u16(&mut out, NO_JOINT);
        append_u16(&mut out, 0);
        append_u16(&mut out, 0);
        append_u16(&mut out, 0);

        append_u16(&mut out, 0);
        append_u16(&mut out, 0);
        out.extend_from_slice(&[255, 255, 255, 255]);

        for value in [1u16, 0, 3, 0, 1, 0] {
            append_u16(&mut out, value);
        }
        out.extend_from_slice(&0u32.to_le_bytes());

        for (x, y, z) in [(0i16, 0i16, 0i16), (48, 0, 0), (0, 48, 0)] {
            append_i16(&mut out, x);
            append_i16(&mut out, y);
            append_i16(&mut out, z);
            out.push(NO_JOINT8);
            out.push(0);
        }

        for (vertex, u, v) in [(0u16, 0u8, 0u8), (1, 64, 0), (2, 0, 64)] {
            append_u16(&mut out, vertex);
            out.push(u);
            out.push(v);
        }

        out
    }

    fn two_joint_animation_with_child_x_offsets(offsets: &[i32]) -> Vec<u8> {
        const ASSET_HEADER_SIZE: usize = 12;
        const ANIMATION_HEADER_SIZE: usize = 8;
        const POSE_RECORD_SIZE: usize = 30;
        const ANIMATION_VERSION: u16 = 1;

        let frame_count = offsets.len() as u16;
        let payload_len = ANIMATION_HEADER_SIZE + offsets.len() * 2 * POSE_RECORD_SIZE;
        let mut out = Vec::with_capacity(ASSET_HEADER_SIZE + payload_len);
        out.extend_from_slice(b"PSXA");
        out.extend_from_slice(&ANIMATION_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(payload_len as u32).to_le_bytes());
        append_u16(&mut out, 2);
        append_u16(&mut out, frame_count);
        append_u16(&mut out, 30);
        append_u16(&mut out, 0);

        for x in offsets {
            append_identity_pose(&mut out, [0, 0, 0]);
            append_identity_pose(&mut out, [*x, 0, 0]);
        }

        out
    }

    fn two_joint_animation_with_global_x_offsets(offsets: &[i32]) -> Vec<u8> {
        const ASSET_HEADER_SIZE: usize = 12;
        const ANIMATION_HEADER_SIZE: usize = 8;
        const POSE_RECORD_SIZE: usize = 30;
        const ANIMATION_VERSION: u16 = 1;

        let frame_count = offsets.len() as u16;
        let payload_len = ANIMATION_HEADER_SIZE + offsets.len() * 2 * POSE_RECORD_SIZE;
        let mut out = Vec::with_capacity(ASSET_HEADER_SIZE + payload_len);
        out.extend_from_slice(b"PSXA");
        out.extend_from_slice(&ANIMATION_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(payload_len as u32).to_le_bytes());
        append_u16(&mut out, 2);
        append_u16(&mut out, frame_count);
        append_u16(&mut out, 30);
        append_u16(&mut out, 0);

        for x in offsets {
            append_identity_pose(&mut out, [*x, 0, 0]);
            append_identity_pose(&mut out, [*x, 0, 0]);
        }

        out
    }

    fn append_identity_pose(out: &mut Vec<u8>, translation: [i32; 3]) {
        for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
            append_i16(out, value);
        }
        for value in translation {
            append_i32(out, value);
        }
    }

    fn append_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn append_i16(out: &mut Vec<u8>, value: i16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn append_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
