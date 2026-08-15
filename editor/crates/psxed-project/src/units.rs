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
//! Model meshes and animations scale losslessly through the cooked
//! `.psxmdl` header's `local_to_world_q12` field
//! ([`scale_model_blob_to_engine_units`]): quantized vertex and pose
//! data is stored relative to that transform, so dividing the one u16
//! rescales every vertex, socket, and animation frame bit-exactly.
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
            scale_controller_settings(settings);
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
    settings.walk_speed = div_i32_min1(settings.walk_speed);
    settings.run_speed = div_i32_min1(settings.run_speed);
    settings.roll_speed = div_i32_min1(settings.roll_speed);
    settings.backstep_speed = div_i32_min1(settings.backstep_speed);
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
            character.walk_speed = div_i32_min1(character.walk_speed);
            character.run_speed = div_i32_min1(character.run_speed);
            character.roll_speed = div_i32_min1(character.roll_speed);
            character.backstep_speed = div_i32_min1(character.backstep_speed);
            character.camera_distance = div_i32_min1(character.camera_distance);
            character.camera_height = div_i32(character.camera_height);
            character.camera_target_height = div_i32(character.camera_target_height);
            character.camera_min_floor_clearance = div_i32(character.camera_min_floor_clearance);
            if let Some(behavior) = &mut character.enemy_behavior {
                behavior.aggro_radius = div_u16_min1(behavior.aggro_radius);
                behavior.preferred_distance = div_u16_min1(behavior.preferred_distance);
                behavior.spacing_tolerance = div_u16_min1(behavior.spacing_tolerance);
                for axis in behavior.patrol_offset.iter_mut() {
                    *axis = div_i32(*axis);
                }
            }
            // Combat capsules are joint-local model space: they scale
            // through the model blob's local_to_world_q12, not here.
        }
        ResourceData::Model(model) => {
            model.world_height = div_u16_min1(model.world_height);
            model.collision_radius = div_u16_min1(model.collision_radius);
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
            // Grip translation and hitboxes are weapon-model-local:
            // scaled through the weapon model blob.
        }
        _ => {}
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
        for brush in &mut scene.brushes {
            scale_brush(brush);
        }
        for node in &mut scene.nodes {
            scale_node(node);
        }
    }
    for resource in &mut project.resources {
        scale_resource(&mut resource.data);
    }
}

/// Rescale a cooked `.psxmdl` blob to engine units by dividing its
/// `local_to_world_q12` header field (absolute offset 26: 12-byte
/// asset header + offset 14 in the model header). Vertex, socket, and
/// pose data are stored relative to that transform, so this is exact.
pub fn scale_model_blob_to_engine_units(bytes: &mut [u8]) {
    const OFFSET: usize = 12 + 14;
    if bytes.len() < OFFSET + 2 {
        return;
    }
    let raw = u16::from_le_bytes([bytes[OFFSET], bytes[OFFSET + 1]]);
    // 0 encodes the 4096 (1.0) default in the format.
    let current = if raw == 0 { 4096 } else { raw };
    let scaled = (current / WORLD_UNIT_DIVISOR as u16).max(1);
    bytes[OFFSET..OFFSET + 2].copy_from_slice(&scaled.to_le_bytes());
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

    #[test]
    fn model_blob_scale_divides_the_q12_header_field() {
        let mut blob = vec![0u8; 32];
        blob[26..28].copy_from_slice(&4096u16.to_le_bytes());
        scale_model_blob_to_engine_units(&mut blob);
        assert_eq!(u16::from_le_bytes([blob[26], blob[27]]), 256);
        // The zero default means 4096 and must scale the same way.
        let mut blob = vec![0u8; 32];
        scale_model_blob_to_engine_units(&mut blob);
        assert_eq!(u16::from_le_bytes([blob[26], blob[27]]), 256);
    }
}
