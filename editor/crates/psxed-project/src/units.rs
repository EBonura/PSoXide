//! Authored-to-engine unit normalization for BSP projects.
//!
//! Authored PSoXide content runs ~16x Quake's unit scale (Aletha is
//! 1024 units tall; Quake's player is 56). The runtime engine shares
//! its numeric regime with quake-psx -- GTE sz units, OTZ subdivision
//! bands, collision hull numbers (the characterless fallback is
//! literally Quake's 16/56 player hull) -- so the cook divides every
//! length-typed quantity by [`WORLD_UNIT_DIVISOR`] and hands the
//! runtime Quake-scale data. Authored files never change: the editor
//! keeps authoring at the historical scale and the scaling happens on
//! an in-memory clone at cook entry ([`scale_project_to_engine_units`]).
//!
//! Model meshes scale by dividing the cooked `.psxmdl` VERTEX table
//! ([`scale_model_blob_to_engine_units`]) and every model-local
//! quantity that rides on it (animation translations, sockets, grips,
//! combat capsules), NOT the header's `local_to_world_q12`. The
//! runtime folds that q12 into i16 Q12 pose matrices before the GTE
//! (`scaled_pose_matrix`), so a 16x smaller q12 would leave rotation
//! entries with ~2 bits of precision and visibly deform skinned
//! meshes. Shrinking the local space instead keeps the fold at its
//! pre-migration precision, the same trade quake-psx makes with its
//! u8 alias vertices and ~10-bit Q12 scale.
//!
//! Speeds quantize to integers per tick at the new scale (walk 44 ->
//! 3), which nudges movement feel by a few percent; that is the
//! accepted cost until the sim adopts quake-core's Q20.12 coordinates.

use crate::brush::Brush;
use crate::{
    ColliderShape, LogicNodeKind, NodeKind, ProjectDocument, ResourceData, Scene, SceneNode,
};

/// Authored units per engine (Quake-scale) unit.
pub const WORLD_UNIT_DIVISOR: i32 = 16;

/// [`crate::brush_compile::SURFACE_EXTENT_UNITS`] expressed in engine
/// units: the extent cap the cook applies to pre-scaled BSP geometry.
/// 128 engine units sits between Quake's 240-unit qbsp splits and its
/// 64-ish structural faces; the editor preview keeps subdividing at
/// the authored-scale constant so preview==cook holds patch for patch.
pub const ENGINE_SURFACE_EXTENT_UNITS: f64 =
    crate::brush_compile::SURFACE_EXTENT_UNITS / WORLD_UNIT_DIVISOR as f64;

#[inline]
fn div_i32(value: i32) -> i32 {
    // Round to nearest, half away from zero: deterministic and keeps
    // coincident authored values coincident after scaling.
    let d = WORLD_UNIT_DIVISOR;
    if value >= 0 {
        (value + d / 2) / d
    } else {
        (value - d / 2) / d
    }
}

#[inline]
/// Movement speeds cook to Q8 engine units per tick (256 = one unit per
/// tick) so the divisor loses no precision: the player motor carries a
/// sub-unit remainder. Enemy records still take whole units (see
/// `cook_props_lights`), which shift this back down.
fn speed_q8(value: i32) -> i32 {
    value.saturating_mul(256 / WORLD_UNIT_DIVISOR)
}

/// Grid-format projects cook at authored scale, but the runtime reads
/// speeds in Q8 regardless: convert only the speed fields (x256).
pub fn speeds_to_q8_unscaled(project: &mut ProjectDocument) {
    let q8 = |v: i32| v.saturating_mul(256);
    for scene in &mut project.scenes {
        for node in &mut scene.nodes {
            if let NodeKind::CharacterController {
                settings: Some(settings),
                ..
            } = &mut node.kind
            {
                settings.walk_speed = q8(settings.walk_speed);
                settings.run_speed = q8(settings.run_speed);
                settings.roll_speed = q8(settings.roll_speed);
                settings.backstep_speed = q8(settings.backstep_speed);
            }
        }
    }
    for resource in &mut project.resources {
        if let ResourceData::Character(character) = &mut resource.data {
            character_speeds_to_q8_unscaled(character);
        }
    }
}

/// Grid-format counterpart of [`scale_default_character_to_engine_units`]
/// for a Character built after the clone pass (the cook's fallback).
pub fn character_speeds_to_q8_unscaled(character: &mut crate::CharacterResource) {
    let q8 = |v: i32| v.saturating_mul(256);
    character.walk_speed = q8(character.walk_speed);
    character.run_speed = q8(character.run_speed);
    character.roll_speed = q8(character.roll_speed);
    character.backstep_speed = q8(character.backstep_speed);
}

fn div_i32_min1(value: i32) -> i32 {
    if value > 0 {
        div_i32(value).max(1)
    } else {
        div_i32(value)
    }
}

#[inline]
fn div_u16(value: u16) -> u16 {
    div_i32(value as i32).clamp(0, u16::MAX as i32) as u16
}

#[inline]
fn div_u16_min1(value: u16) -> u16 {
    if value > 0 {
        div_u16(value).max(1)
    } else {
        0
    }
}

#[inline]
fn div_i16(value: i16) -> i16 {
    div_i32(value as i32) as i16
}

#[inline]
fn div_f32(value: f32) -> f32 {
    value / WORLD_UNIT_DIVISOR as f32
}

/// Scale one authored brush: vertex positions divide; each face's UV
/// scale divides with them so texel density on the surface is
/// unchanged (`uv = world / scale`, both numerator and denominator
/// shrink together).
fn scale_brush(brush: &mut Brush) {
    for face in &mut brush.faces {
        for point in &mut face.points {
            for axis in point.iter_mut() {
                *axis = div_i32(*axis);
            }
        }
        for axis in face.uv.scale_q8.iter_mut() {
            *axis = div_i16(*axis).max(1);
        }
    }
}

fn scale_node(node: &mut SceneNode) {
    for axis in node.transform.translation.iter_mut() {
        *axis = div_f32(*axis);
    }
    // Spawn-bearing nodes keep their authored floor clearance: a
    // 1-unit authored lift must not round onto the floor plane and
    // fail the spawn-in-solid check.
    if matches!(node.kind, NodeKind::SpawnPoint { .. } | NodeKind::Entity) {
        node.transform.translation[1] = node.transform.translation[1].ceil();
    }
    match &mut node.kind {
        NodeKind::World {
            sector_size,
            sky,
            far_vista,
            camera,
            culling,
            physics,
            ..
        } => {
            *sector_size = div_i32_min1(*sector_size);
            sky.cloud_layer.altitude = div_u16(sky.cloud_layer.altitude);
            sky.cloud_layer.extent = div_u16_min1(sky.cloud_layer.extent);
            far_vista.radius = div_i32_min1(far_vista.radius);
            far_vista.height = div_i32_min1(far_vista.height);
            far_vista.vertical_offset = div_i32(far_vista.vertical_offset);
            camera.distance = div_i32_min1(camera.distance);
            camera.height = div_i32(camera.height);
            camera.target_height = div_i32(camera.target_height);
            camera.min_floor_clearance = div_i32(camera.min_floor_clearance);
            culling.draw_distance = div_i32_min1(culling.draw_distance);
            physics.gravity_per_tick = div_i32_min1(physics.gravity_per_tick);
        }
        NodeKind::ImageProp {
            width,
            height,
            collision_size,
            ..
        } => {
            *width = div_u16_min1(*width);
            *height = div_u16_min1(*height);
            for axis in collision_size.iter_mut() {
                *axis = div_u16_min1(*axis);
            }
        }
        NodeKind::BoxProp { vertices, .. } => {
            for vertex in vertices.iter_mut() {
                for axis in vertex.iter_mut() {
                    *axis = div_i16(*axis);
                }
            }
        }
        NodeKind::ModelRenderer { visual_offset, .. } => {
            for axis in visual_offset.iter_mut() {
                *axis = div_i16(*axis);
            }
        }
        NodeKind::Collider { shape, .. } => match shape {
            ColliderShape::Box { half_extents } => {
                for axis in half_extents.iter_mut() {
                    *axis = div_u16_min1(*axis);
                }
            }
            ColliderShape::Sphere { radius } => *radius = div_u16_min1(*radius),
            ColliderShape::Capsule { radius, height } => {
                *radius = div_u16_min1(*radius);
                *height = div_u16_min1(*height);
            }
        },
        NodeKind::Interactable { radius, .. } => *radius = div_u16_min1(*radius),
        // PointLight radius is authored in SECTORS and resolves against
        // the (already scaled) world sector size, so the multiplier
        // itself must not scale.
        NodeKind::PointLight { .. } => {}
        NodeKind::ParticleEmitter { settings } => {
            settings.start_size = div_u16_min1(settings.start_size);
            settings.end_size = div_u16_min1(settings.end_size);
            for axis in settings.base_velocity_q4.iter_mut() {
                *axis = div_i16(*axis);
            }
            for axis in settings.random_velocity_q4.iter_mut() {
                *axis = div_u16(*axis);
            }
            for axis in settings.acceleration_q4.iter_mut() {
                *axis = div_i16(*axis);
            }
            settings.spawn_radius = div_u16(settings.spawn_radius);
        }
        NodeKind::Logic { kind, .. } => match kind {
            LogicNodeKind::TriggerVolume { size } => {
                for axis in size.iter_mut() {
                    *axis = div_u16_min1(*axis);
                }
            }
            LogicNodeKind::Door { open_offset, .. } => {
                for axis in open_offset.iter_mut() {
                    *axis = div_i16(*axis);
                }
            }
            LogicNodeKind::Relay | LogicNodeKind::Multisource { .. } => {}
        },
        NodeKind::Animator { action_clips, .. } => {
            for clip in action_clips.iter_mut() {
                if let Some(options) = &mut clip.options {
                    options.push_distance = div_i32_min1(options.push_distance);
                }
            }
        }
        NodeKind::CylinderProp { geometry, .. } => {
            for axis in geometry.radius.iter_mut() {
                *axis = div_u16_min1(*axis);
            }
            geometry.height = div_u16_min1(geometry.height);
        }
        NodeKind::CharacterController { settings, .. } => {
            // A controller with no override has nothing of its own to scale;
            // the Character resource it defers to is scaled with the others.
            if let Some(settings) = settings {
                scale_controller_settings(settings);
            }
        }
        NodeKind::Camera { settings } => {
            settings.distance = div_i32_min1(settings.distance);
            settings.height = div_i32(settings.height);
            settings.target_height = div_i32(settings.target_height);
            settings.min_floor_clearance = div_i32(settings.min_floor_clearance);
        }
        _ => {}
    }
}

fn scale_controller_settings(settings: &mut crate::CharacterControllerSettings) {
    settings.radius = div_u16_min1(settings.radius);
    settings.height = div_u16_min1(settings.height);
    settings.walk_speed = speed_q8(settings.walk_speed);
    settings.run_speed = speed_q8(settings.run_speed);
    settings.roll_speed = speed_q8(settings.roll_speed);
    settings.backstep_speed = speed_q8(settings.backstep_speed);
    // Placed enemies may override the Character's behavior on the node;
    // the embedded block carries the same length-typed fields.
    if let Some(behavior) = &mut settings.enemy {
        scale_enemy_behavior(behavior);
    }
}

fn scale_enemy_behavior(behavior: &mut crate::EnemyBehaviorSettings) {
    behavior.aggro_radius = div_u16_min1(behavior.aggro_radius);
    behavior.preferred_distance = div_u16_min1(behavior.preferred_distance);
    behavior.spacing_tolerance = div_u16_min1(behavior.spacing_tolerance);
    for axis in behavior.patrol_offset.iter_mut() {
        *axis = div_i32(*axis);
    }
}

fn scale_scene(scene: &mut Scene) {
    for brush in &mut scene.brushes {
        scale_brush(brush);
    }
    for node in &mut scene.nodes {
        scale_node(node);
    }
}

fn scale_resource(data: &mut ResourceData) {
    match data {
        ResourceData::Character(character) => {
            character.radius = div_u16_min1(character.radius);
            character.height = div_u16_min1(character.height);
            character.walk_speed = speed_q8(character.walk_speed);
            character.run_speed = speed_q8(character.run_speed);
            character.roll_speed = speed_q8(character.roll_speed);
            character.backstep_speed = speed_q8(character.backstep_speed);
            character.camera_distance = div_i32_min1(character.camera_distance);
            character.camera_height = div_i32(character.camera_height);
            character.camera_target_height = div_i32(character.camera_target_height);
            character.camera_min_floor_clearance = div_i32(character.camera_min_floor_clearance);
            if let Some(behavior) = &mut character.enemy_behavior {
                scale_enemy_behavior(behavior);
            }
            // Joint-local endpoints ride on the (÷16) model-local space;
            // the radius is engine units. Both divide.
            for volume in &mut character.combat_capsules {
                div_point(&mut volume.capsule.start);
                div_point(&mut volume.capsule.end);
                volume.capsule.radius = div_u16_min1(volume.capsule.radius);
            }
        }
        ResourceData::Model(model) => {
            model.world_height = div_u16_min1(model.world_height);
            model.collision_radius = div_u16_min1(model.collision_radius);
            for socket in &mut model.attachments {
                div_point(&mut socket.translation);
            }
        }
        ResourceData::AnimationClip(clip) => {
            div_point(&mut clip.calibration.offset);
            for key in &mut clip.pose_corrections {
                div_point(&mut key.translation);
            }
        }
        ResourceData::AnimationSet(set) => {
            for binding in &mut set.action_clips {
                if let Some(options) = &mut binding.options {
                    options.push_distance = div_i32_min1(options.push_distance);
                }
            }
        }
        ResourceData::Weapon(weapon) => {
            weapon.arc_reach = div_u16_min1(weapon.arc_reach);
            div_point(&mut weapon.grip.translation);
            for hitbox in &mut weapon.hitboxes {
                match &mut hitbox.shape {
                    crate::WeaponHitShape::Box {
                        center,
                        half_extents,
                    } => {
                        div_point(center);
                        for axis in half_extents.iter_mut() {
                            *axis = div_u16_min1(*axis);
                        }
                    }
                    crate::WeaponHitShape::Capsule { start, end, radius } => {
                        div_point(start);
                        div_point(end);
                        *radius = div_u16_min1(*radius);
                    }
                }
            }
        }
        _ => {}
    }
}

#[inline]
fn div_point(point: &mut [i32; 3]) {
    for axis in point.iter_mut() {
        *axis = div_i32(*axis);
    }
}

/// Scale the cook's built-in fallback character, which is constructed
/// after the project clone was scaled and so misses the resource pass.
pub fn scale_default_character_to_engine_units(character: &mut crate::CharacterResource) {
    let mut data = ResourceData::Character(character.clone());
    scale_resource(&mut data);
    if let ResourceData::Character(scaled) = data {
        *character = scaled;
    }
}

/// Divide every length-typed authored quantity by
/// [`WORLD_UNIT_DIVISOR`], in place. Call once, on a clone, at cook
/// entry, for BSP-format projects only.
pub fn scale_project_to_engine_units(project: &mut ProjectDocument) {
    for scene in &mut project.scenes {
        scale_scene(scene);
    }
    for resource in &mut project.resources {
        scale_resource(&mut resource.data);
    }
}

#[inline]
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[inline]
fn div_i16_in_place(bytes: &mut [u8], offset: usize) {
    let value = i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    bytes[offset..offset + 2].copy_from_slice(&div_i16(value).to_le_bytes());
}

/// Rescale a cooked `.psxmdl` blob to engine units by dividing every
/// vertex position (model-local i16) by [`WORLD_UNIT_DIVISOR`]. The
/// header's `local_to_world_q12` is deliberately left alone (see the
/// module docs). Blobs that do not lay out as a v4 model are left
/// untouched; the cook's parse step reports them.
pub fn scale_model_blob_to_engine_units(bytes: &mut [u8]) {
    use psxed_format::model::{
        JointRecord, MaterialRecord, ModelHeader, PartRecord, MAGIC, VERSION, VERTEX_RECORD_SIZE,
    };
    let payload = psxed_format::AssetHeader::SIZE;
    if bytes.len() < payload + ModelHeader::SIZE
        || bytes[..4] != MAGIC
        || read_u16(bytes, 4) != VERSION
    {
        return;
    }
    let header = &bytes[payload..payload + ModelHeader::SIZE];
    let joint_count = read_u16(header, 0) as usize;
    let part_count = read_u16(header, 2) as usize;
    let vertex_count = read_u16(header, 4) as usize;
    let material_count = read_u16(header, 8) as usize;
    let first_vertex = payload
        + ModelHeader::SIZE
        + joint_count * JointRecord::SIZE
        + material_count * MaterialRecord::SIZE
        + part_count * PartRecord::SIZE;
    let end = first_vertex + vertex_count * VERTEX_RECORD_SIZE;
    if bytes.len() < end {
        return;
    }
    for record in (first_vertex..end).step_by(VERTEX_RECORD_SIZE) {
        for axis in 0..3 {
            div_i16_in_place(bytes, record + axis * 2);
        }
    }
}

/// Rescale a cooked `.psxanim` blob to engine units: pose translations
/// are model-local (`stored << translation_shift` in v2/v3, raw i32 in
/// v1) and must shrink with the model's vertex table. Rotations are
/// unitless and untouched. Malformed blobs are left for the parse step.
pub fn scale_animation_blob_to_engine_units(bytes: &mut [u8]) {
    use psxed_format::animation::{
        AnimationHeader, MAGIC, POSE_RECORD_SIZE, POSE_RECORD_SIZE_V1, POSE_RECORD_SIZE_V3,
        VERSION, VERSION_V1, VERSION_V3,
    };
    let payload = psxed_format::AssetHeader::SIZE;
    if bytes.len() < payload + AnimationHeader::SIZE || bytes[..4] != MAGIC {
        return;
    }
    let version = read_u16(bytes, 4);
    let (record_size, translation_offset) = match version {
        VERSION_V1 => (POSE_RECORD_SIZE_V1, 18),
        VERSION => (POSE_RECORD_SIZE, 18),
        VERSION_V3 => (POSE_RECORD_SIZE_V3, 14),
        _ => return,
    };
    let pose_count = read_u16(bytes, payload) as usize * read_u16(bytes, payload + 2) as usize;
    let first_pose = payload + AnimationHeader::SIZE;
    let end = first_pose + pose_count * record_size;
    if bytes.len() < end {
        return;
    }
    if version == VERSION_V1 {
        for record in (first_pose..end).step_by(record_size) {
            for axis in 0..3 {
                let at = record + translation_offset + axis * 4;
                let value = i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                bytes[at..at + 4].copy_from_slice(&div_i32(value).to_le_bytes());
            }
        }
        return;
    }
    // WORLD_UNIT_DIVISOR is 2^4: with enough shared shift the divide is
    // a free header edit; otherwise divide the stored values (rounded)
    // and drop the shift.
    const _: () = assert!(WORLD_UNIT_DIVISOR & (WORLD_UNIT_DIVISOR - 1) == 0);
    const SHIFT_BITS: u16 = WORLD_UNIT_DIVISOR.trailing_zeros() as u16;
    let shift = read_u16(bytes, payload + 6);
    if shift >= SHIFT_BITS {
        bytes[payload + 6..payload + 8].copy_from_slice(&(shift - SHIFT_BITS).to_le_bytes());
        return;
    }
    bytes[payload + 6..payload + 8].copy_from_slice(&0u16.to_le_bytes());
    for record in (first_pose..end).step_by(record_size) {
        for axis in 0..3 {
            let at = record + translation_offset + axis * 2;
            let stored = i16::from_le_bytes([bytes[at], bytes[at + 1]]) as i32;
            let scaled = div_i32(stored << shift) as i16;
            bytes[at..at + 2].copy_from_slice(&scaled.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_is_symmetric_and_preserves_coincidence() {
        assert_eq!(div_i32(4096), 256);
        assert_eq!(div_i32(-4096), -256);
        assert_eq!(div_i32(65), 4);
        assert_eq!(div_i32(-65), -4);
        assert_eq!(div_i32(8), 1);
    }

    fn asset(magic: &[u8; 4], version: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = magic.to_vec();
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn model_blob_scale_divides_vertices_and_keeps_q12() {
        // One joint, one material, one part, two vertices, no faces.
        let mut payload = Vec::new();
        for value in [1u16, 1, 2, 0, 1, 64, 64, 64] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&[0xFF, 0xFF, 0, 0]); // joint: root
        payload.extend_from_slice(&[0; 8]); // material
        payload.extend_from_slice(&[0; 16]); // part
        for vertex in [[-32000i16, 65, 8], [4096, -4096, 0]] {
            for axis in vertex {
                payload.extend_from_slice(&axis.to_le_bytes());
            }
            payload.extend_from_slice(&[0xFF, 0]);
        }
        let mut blob = asset(b"PSMD", 4, &payload);
        scale_model_blob_to_engine_units(&mut blob);
        let model = psx_asset::Model::from_bytes(&blob).expect("still parses");
        assert_eq!(model.local_to_world_q12(), 64);
        let v0 = model.vertex(0).unwrap().position;
        let v1 = model.vertex(1).unwrap().position;
        assert_eq!((v0.x, v0.y, v0.z), (-2000, 4, 1));
        assert_eq!((v1.x, v1.y, v1.z), (256, -256, 0));
    }

    fn animation_poses(version: u16, shift: u16, translations: &[[i32; 3]]) -> Vec<u8> {
        use psxed_format::animation::{POSE_RECORD_SIZE, POSE_RECORD_SIZE_V1, POSE_RECORD_SIZE_V3};
        let mut payload = Vec::new();
        for value in [translations.len() as u16, 1, 30, shift] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        for translation in translations {
            match version {
                1 => {
                    payload.extend_from_slice(&[0; POSE_RECORD_SIZE_V1 - 12]);
                    for axis in translation {
                        payload.extend_from_slice(&axis.to_le_bytes());
                    }
                }
                2 | 3 => {
                    let rotation = if version == 2 {
                        POSE_RECORD_SIZE - 6
                    } else {
                        POSE_RECORD_SIZE_V3 - 6
                    };
                    payload.extend_from_slice(&vec![0; rotation]);
                    for axis in translation {
                        payload.extend_from_slice(&(*axis as i16).to_le_bytes());
                    }
                }
                _ => unreachable!(),
            }
        }
        asset(b"PSXA", version, &payload)
    }

    fn decoded_translations(blob: &[u8]) -> Vec<[i32; 3]> {
        let animation = psx_asset::Animation::from_bytes(blob).expect("still parses");
        (0..animation.joint_count())
            .map(|joint| {
                let t = animation.pose(0, joint).unwrap().translation;
                [t.x, t.y, t.z]
            })
            .collect()
    }

    #[test]
    fn animation_blob_scale_divides_model_local_translations() {
        // v3 with a shared shift >= 4: exact via the header.
        let mut blob = animation_poses(3, 6, &[[100, -100, 7]]);
        scale_animation_blob_to_engine_units(&mut blob);
        assert_eq!(decoded_translations(&blob), vec![[400, -400, 28]]);
        // v2 with a small shift: stored values divide, shift drops to 0.
        let mut blob = animation_poses(2, 1, &[[1000, -1000, 9]]);
        scale_animation_blob_to_engine_units(&mut blob);
        assert_eq!(decoded_translations(&blob), vec![[125, -125, 1]]);
        // v1 raw i32 translations.
        let mut blob = animation_poses(1, 0, &[[65536, -65536, 24]]);
        scale_animation_blob_to_engine_units(&mut blob);
        assert_eq!(decoded_translations(&blob), vec![[4096, -4096, 2]]);
    }
}
