//! Render-independent authority for one actor's sampled skeleton pose.
//!
//! Body rendering, equipment sockets, and gameplay hit volumes must not each
//! reconstruct animation phase and presentation transforms independently.
//! This compact snapshot freezes those inputs for one simulation tick and
//! provides the shared joint sampling path. Rendering remains a consumer of
//! the snapshot rather than its owner.

use psx_asset::{Animation, JointPose, ModelPoseBlend};
use psx_engine::{
    apply_model_pose_translation, compute_joint_world_basis, compute_joint_world_transform,
    JointWorldTransform, LocalToWorldScale, Mat3I16, ModelPoseTranslation, SimTick, WorldVertex,
};

/// Stable skeleton-pose inputs for one actor at one simulation tick.
///
/// The snapshot is intentionally independent of materials, cameras, ordering
/// tables, and render scratch. A caller may therefore reuse the same value for
/// visible body geometry, attachment sockets, and combat capsules.
#[derive(Copy, Clone, Debug)]
pub struct ActorPoseSnapshot {
    tick: SimTick,
    animation: Animation<'static>,
    phase_q12: u32,
    blend_from: Option<ModelPoseBlend<'static>>,
    origin: WorldVertex,
    rotation: Mat3I16,
    local_to_world: LocalToWorldScale,
    pose_translation: ModelPoseTranslation,
}

impl ActorPoseSnapshot {
    /// Capture all inputs needed to sample one actor's stable world pose.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        tick: SimTick,
        animation: Animation<'static>,
        phase_q12: u32,
        blend_from: Option<ModelPoseBlend<'static>>,
        origin: WorldVertex,
        rotation: Mat3I16,
        local_to_world: LocalToWorldScale,
        pose_translation: ModelPoseTranslation,
    ) -> Self {
        Self {
            tick,
            animation,
            phase_q12,
            blend_from,
            origin,
            rotation,
            local_to_world,
            pose_translation,
        }
    }

    /// Simulation tick whose actor state produced this snapshot.
    pub const fn tick(self) -> SimTick {
        self.tick
    }

    /// Primary animation sampled by this snapshot.
    pub const fn animation(self) -> Animation<'static> {
        self.animation
    }

    /// Q12 animation phase frozen into the snapshot.
    pub const fn phase_q12(self) -> u32 {
        self.phase_q12
    }

    /// Re-sample this retained pose's animation at another Q12 phase while
    /// preserving its actor transform and presentation policy.
    ///
    /// Continuous combat uses this only for the portion of an active-frame
    /// window which fell between two retained simulation poses.
    pub const fn with_phase_q12(mut self, phase_q12: u32) -> Self {
        self.phase_q12 = phase_q12;
        self
    }

    /// Optional outgoing-pose crossfade sampled with the body.
    pub const fn blend_from(self) -> Option<ModelPoseBlend<'static>> {
        self.blend_from
    }

    /// Floor/visual-offset-adjusted model origin in world space.
    pub const fn origin(self) -> WorldVertex {
        self.origin
    }

    /// Actor presentation rotation.
    pub const fn rotation(self) -> Mat3I16 {
        self.rotation
    }

    /// Model-local to world scale.
    pub const fn local_to_world(self) -> LocalToWorldScale {
        self.local_to_world
    }

    /// Root-motion/floor-anchor correction applied to every sampled joint.
    pub const fn pose_translation(self) -> ModelPoseTranslation {
        self.pose_translation
    }

    /// Sample one joint through the exact phase, crossfade, and root
    /// correction frozen into this snapshot.
    pub fn joint_pose(self, joint: u16) -> Option<JointPose> {
        let primary = self.animation.pose_looped_q12(self.phase_q12, joint)?;
        let blended = match self.blend_from {
            Some(blend) => blend.blend_toward(primary, joint),
            None => primary,
        };
        Some(apply_model_pose_translation(blended, self.pose_translation))
    }

    /// Sample one joint in world space for combat and attachment geometry.
    pub fn joint_world_transform(self, joint: u16) -> Option<JointWorldTransform> {
        Some(compute_joint_world_transform(
            self.joint_pose(joint)?,
            self.rotation,
            self.local_to_world,
            self.origin,
        ))
    }

    /// Sample the unscaled world orientation for an attached model.
    ///
    /// The attached model supplies its own local-to-world scale; using the
    /// scaled joint transform as an orientation would apply character scale a
    /// second time and collapse weapon geometry.
    pub fn joint_world_basis(self, joint: u16) -> Option<Mat3I16> {
        Some(compute_joint_world_basis(
            self.joint_pose(joint)?,
            self.rotation,
        ))
    }

    /// Sample one joint once and return both its scaled world transform and
    /// unscaled attachment basis.
    pub fn joint_world_transform_and_basis(
        self,
        joint: u16,
    ) -> Option<(JointWorldTransform, Mat3I16)> {
        let pose = self.joint_pose(joint)?;
        Some((
            compute_joint_world_transform(pose, self.rotation, self.local_to_world, self.origin),
            compute_joint_world_basis(pose, self.rotation),
        ))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::combat::transform_actor_combat_capsule;
    use psx_level::CombatCapsuleRecord;
    use std::{boxed::Box, vec::Vec};

    fn one_joint_animation(translations: &[i16]) -> Animation<'static> {
        const ANIMATION_HEADER_SIZE: usize = 8;
        const POSE_RECORD_SIZE: usize = 24;
        let payload_len = ANIMATION_HEADER_SIZE + translations.len() * POSE_RECORD_SIZE;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSXA");
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&(translations.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&30u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        for &translation in translations {
            for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            for value in [translation, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        Animation::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("test animation")
    }

    fn snapshot(
        tick: u32,
        animation: Animation<'static>,
        blend_from: Option<ModelPoseBlend<'static>>,
    ) -> ActorPoseSnapshot {
        ActorPoseSnapshot::new(
            SimTick::from_u32(tick),
            animation,
            0,
            blend_from,
            WorldVertex::new(1_000, 2_000, 3_000),
            Mat3I16::IDENTITY,
            LocalToWorldScale::IDENTITY,
            ModelPoseTranslation { x: 50, y: 0, z: 0 },
        )
    }

    #[test]
    fn one_snapshot_drives_socket_basis_and_combat_capsule_geometry() {
        let outgoing = one_joint_animation(&[100, 100]);
        let incoming = one_joint_animation(&[300, 300]);
        let blend_from = outgoing
            .looped_pose_sample_q12(0)
            .map(|sample| ModelPoseBlend {
                sample,
                alpha_q12: 1 << 11,
            });
        let pose = snapshot(17, incoming, blend_from);

        assert_eq!(pose.tick(), SimTick::from_u32(17));
        let (joint, basis) = pose
            .joint_world_transform_and_basis(0)
            .expect("joint transform and basis");
        assert_eq!(basis, Mat3I16::IDENTITY);
        assert_eq!(joint.translation, WorldVertex::new(1_250, 2_000, 3_000));

        let record = CombatCapsuleRecord {
            joint: 0,
            flags: 0,
            action: 0,
            reserved: 0,
            start: [10, 0, 0],
            end: [30, 0, 0],
            radius: 8,
            active_start_frame: 0,
            active_end_frame: 0,
            damage: 0,
            poise_damage: 0,
            projectile_speed: 0,
            projectile_lifetime_ticks: 0,
            projectile_min_range: 0,
            projectile_max_range: 0,
            projectile_tint_rgb: [0; 3],
            projectile_damage_channel: psx_level::projectile_damage_channel::ZENITH,
            projectile_core_rgb: [0; 3],
            projectile_trail_segments: 0,
            projectile_glow_rgb: [0; 3],
            projectile_length_ticks: 0,
            projectile_impact_rgb: [0; 3],
            projectile_trail_spacing_ticks: 0,
            projectile_charge_start_frame: 0,
            projectile_glow_scale_q8: 0,
            projectile_impact_lifetime_ticks: 0,
            projectile_reserved: 0,
        };
        let capsule = transform_actor_combat_capsule(&record, pose).expect("combat capsule");
        assert_eq!(capsule.start, [1_260, 2_000, 3_000]);
        assert_eq!(capsule.end, [1_280, 2_000, 3_000]);
    }

    /// Regression for the historical "spawn-adjacent tick produced an
    /// `i32::MIN` weapon-origin X" transient (preview-era report, handoff
    /// 7.1). Under the retained-pose authority the pre-refresh path cannot
    /// be sampled at all (snapshots are `Option` and consumers skip `None`),
    /// so what remains to prove is arithmetic: no legal cooked input, even
    /// at the quantization and scale extremes, can saturate a weapon origin
    /// or blade endpoint to the `i32::MIN` sentinel. This drives the exact
    /// socket/capsule sampling entry points combat and equipment use.
    #[test]
    fn extreme_legal_inputs_cannot_produce_a_min_sentinel_weapon_origin() {
        const SANE_WORLD_BOUND: i32 = 16_000_000;
        let assert_sane = |value: i32, what: &str| {
            assert_ne!(value, i32::MIN, "{what} saturated to the i32::MIN sentinel");
            assert!(
                value.abs() < SANE_WORLD_BOUND,
                "{what} left the sane world envelope: {value}"
            );
        };

        // Full-magnitude Q12 rotation rows, worst-case sign mixing.
        let spun = Mat3I16 {
            m: [[0, 0, 4096], [0, -4096, 0], [-4096, 0, 0]],
        };
        for translation in [i16::MIN, i16::MAX, 0] {
            let animation = one_joint_animation(&[translation, translation]);
            for scale_q12 in [0x1000u16, 0x2000, u16::MAX] {
                for origin_x in [-1_000_000i32, 1_000_000] {
                    let pose = ActorPoseSnapshot::new(
                        SimTick::from_u32(0),
                        animation,
                        0,
                        None,
                        WorldVertex::new(origin_x, 1_000_000, -1_000_000),
                        spun,
                        LocalToWorldScale::from_q12(scale_q12),
                        ModelPoseTranslation {
                            x: 32_767,
                            y: -32_768,
                            z: 32_767,
                        },
                    );
                    let (joint, _basis) = pose
                        .joint_world_transform_and_basis(0)
                        .expect("extreme pose still samples");
                    assert_sane(joint.translation.x, "weapon origin x");
                    assert_sane(joint.translation.y, "weapon origin y");
                    assert_sane(joint.translation.z, "weapon origin z");

                    // The longest authored blade in tracked content is 30k
                    // model units; push the full compact range both ways.
                    let record = CombatCapsuleRecord {
                        joint: 0,
                        flags: 0,
                        action: 0,
                        reserved: 0,
                        start: [32_767, -32_768, 32_767],
                        end: [-32_768, 32_767, -32_768],
                        radius: 255,
                        active_start_frame: 0,
                        active_end_frame: 0,
                        damage: 0,
                        poise_damage: 0,
                        projectile_speed: 0,
                        projectile_lifetime_ticks: 0,
                        projectile_min_range: 0,
                        projectile_max_range: 0,
                        projectile_tint_rgb: [0; 3],
                        projectile_damage_channel: psx_level::projectile_damage_channel::ZENITH,
                        projectile_core_rgb: [0; 3],
                        projectile_trail_segments: 0,
                        projectile_glow_rgb: [0; 3],
                        projectile_length_ticks: 0,
                        projectile_impact_rgb: [0; 3],
                        projectile_trail_spacing_ticks: 0,
                        projectile_charge_start_frame: 0,
                        projectile_glow_scale_q8: 0,
                        projectile_impact_lifetime_ticks: 0,
                        projectile_reserved: 0,
                    };
                    let capsule =
                        transform_actor_combat_capsule(&record, pose).expect("extreme capsule");
                    for (axis, what) in ["x", "y", "z"].iter().enumerate() {
                        assert_sane(capsule.start[axis], &std::format!("blade start {what}"));
                        assert_sane(capsule.end[axis], &std::format!("blade end {what}"));
                    }
                }
            }
        }
    }

    #[test]
    fn completed_spawn_crossfade_cannot_leak_the_outgoing_pose() {
        let outgoing = one_joint_animation(&[i16::MIN, i16::MIN]);
        let incoming = one_joint_animation(&[100, 100]);
        let blend_from = outgoing
            .looped_pose_sample_q12(0)
            .map(|sample| ModelPoseBlend {
                sample,
                alpha_q12: 1 << 12,
            });
        let pose = snapshot(1, incoming, blend_from);

        assert_eq!(
            pose.joint_world_transform(0)
                .expect("joint transform")
                .translation,
            WorldVertex::new(1_150, 2_000, 3_000)
        );
    }
}
