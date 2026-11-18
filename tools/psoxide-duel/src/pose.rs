//! A fighter's posed body on the host: the cooked model and clips loaded by
//! the game's own loader, and each tick's pose resolved by the game's own
//! resolver from the fighter's animation, exactly as the disc's
//! `refresh_actor_pose_snapshots` does. Contact then runs the one melee
//! contact path (`combat::ActiveStrike`) on those poses, so the harness
//! tests the same blade and hurtbox geometry the disc does.

use std::fmt;

use psx_asset::{Animation, ModelPart, ModelVertex};
use psx_engine::{SimTick, TexturedModelRenderFace, VideoHz};
use psx_game_runtime::actor_pose::ActorPoseSnapshot;
use psx_game_runtime::entities::{GameEntities, GameEntityClip};
use psx_game_runtime::model_rendering::{
    self as mr, InstanceActorPoseSnapshot, ModelInstancePoseLayer, ModelInstancePoseOverride,
    ModelTables, RuntimeModelAsset,
};
use psx_level::{
    AssetId, CombatCapsuleRecord, LevelGameEntityRecord, LevelModelClipBoundsRecord,
    LevelModelClipRecord, LevelModelRecord, ModelClipIndex, ModelClipTableIndex,
    ModelFrameBoundsIndex, ModelIndex, ModelSocketIndex, OptionalModelClipIndex,
};
use psxed_project::playtest::{OwnedLevelRecords, PlaytestCombatCapsule, PlaytestPackage};

/// Model slots the host loader keeps (a project's whole model table).
const MODELS: usize = 64;
/// Clip slots the host loader keeps.
const CLIPS: usize = 2048;

/// One fighter's cooked body: models, clips and its capsules as the guest
/// reads them, and the model instance its record places.
pub struct Rig {
    tables: ModelTables,
    models: Box<[Option<RuntimeModelAsset>; MODELS]>,
    clips: Box<[Option<Animation<'static>>; CLIPS]>,
    instance: u16,
    /// The fighter's combat capsules as `COMBAT_CAPSULES` holds them.
    pub capsules: &'static [CombatCapsuleRecord],
}

impl fmt::Debug for Rig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rig")
            .field("instance", &self.instance)
            .field("capsules", &self.capsules.len())
            .finish()
    }
}

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

/// A cooked capsule as the guest's record.
fn capsule_record(c: &PlaytestCombatCapsule) -> CombatCapsuleRecord {
    CombatCapsuleRecord {
        joint: c.joint,
        flags: c.flags,
        action: c.action,
        reserved: 0,
        start: c.start,
        end: c.end,
        radius: c.radius,
        active_start_frame: c.active_start_frame,
        active_end_frame: c.active_end_frame,
        damage: c.damage,
        poise_damage: c.poise_damage,
        projectile_speed: c.projectile_speed,
        projectile_lifetime_ticks: c.projectile_lifetime_ticks,
        projectile_min_range: c.projectile_min_range,
        projectile_max_range: c.projectile_max_range,
        projectile_tint_rgb: c.projectile_tint_rgb,
        projectile_damage_channel: c.projectile_damage_channel,
        projectile_core_rgb: c.projectile_core_rgb,
        projectile_trail_segments: c.projectile_trail_segments,
        projectile_glow_rgb: c.projectile_glow_rgb,
        projectile_length_ticks: c.projectile_length_ticks,
        projectile_impact_rgb: c.projectile_impact_rgb,
        projectile_trail_spacing_ticks: c.projectile_trail_spacing_ticks,
        projectile_charge_start_frame: c.projectile_charge_start_frame,
        projectile_glow_scale_q8: c.projectile_glow_scale_q8,
        projectile_impact_lifetime_ticks: c.projectile_impact_lifetime_ticks,
        projectile_reserved: 0,
    }
}

impl Rig {
    /// Load the body `record` places from `package`, with `capsules` (the
    /// fighter's own slice of the cooked capsules). None when the package
    /// has more models or clips than the host tables hold, or the record
    /// places no loaded model.
    pub fn build(
        package: &PlaytestPackage,
        record: &LevelGameEntityRecord,
        capsules: &[PlaytestCombatCapsule],
    ) -> Option<Self> {
        if package.models.len() > MODELS || package.model_clips.len() > CLIPS {
            return None;
        }
        let model_records = leak(
            package
                .models
                .iter()
                .map(|m| LevelModelRecord {
                    name: "",
                    mesh_asset: AssetId(m.mesh_asset_index as u16),
                    texture_asset: m.texture_asset_index.map(|i| AssetId(i as u16)),
                    clip_first: ModelClipTableIndex(m.clip_first),
                    clip_count: m.clip_count,
                    default_clip: ModelClipIndex(m.default_clip),
                    socket_first: ModelSocketIndex(m.socket_first),
                    socket_count: m.socket_count,
                    world_height: m.world_height,
                    collision_radius: m.collision_radius,
                    flags: 0,
                })
                .collect(),
        );
        let clip_records = leak(
            package
                .model_clips
                .iter()
                .map(|c| LevelModelClipRecord {
                    model: ModelIndex(c.model),
                    name: "",
                    animation_asset: AssetId(c.animation_asset_index as u16),
                })
                .collect(),
        );
        let clip_bounds = leak(
            package
                .model_clip_bounds
                .iter()
                .map(|b| LevelModelClipBoundsRecord {
                    model: ModelIndex(b.model),
                    clip: ModelClipTableIndex(b.clip),
                    first_frame: ModelFrameBoundsIndex(b.first_frame),
                    frame_count: b.frame_count,
                    floor_y: b.floor_y,
                    pose_offset: b.pose_offset,
                    flags: b.flags,
                    first_root_xz: b.first_root_xz,
                })
                .collect(),
        );
        let owned = OwnedLevelRecords::build(package);
        let instances = leak(owned.records().model_instances.to_vec());
        let assets: &'static [&'static [u8]] = leak(
            package
                .assets
                .iter()
                .map(|a| &*Box::leak(a.bytes.clone().into_boxed_slice()))
                .collect(),
        );
        let mut models = Box::new([None; MODELS]);
        let mut clips = Box::new([None; CLIPS]);
        let mut faces = vec![TexturedModelRenderFace::default(); 1 << 17];
        let mut parts = vec![ModelPart::ZERO; 1 << 14];
        let mut vertices = vec![
            ModelVertex {
                position: psx_engine::Vec3I16::new(0, 0, 0),
                joint1: psx_asset::NO_JOINT8,
                blend: 0,
            };
            1 << 17
        ];
        let (mut face_count, mut part_count, mut vertex_count) = (0, 0, 0);
        mr::load_runtime_models(
            model_records,
            clip_records,
            &mut models,
            &mut clips,
            &mut faces,
            &mut face_count,
            &mut parts,
            &mut part_count,
            &mut vertices,
            &mut vertex_count,
            |asset, _| assets.get(usize::from(asset.0)).copied(),
            |asset, _| {
                Some(psx_game_runtime::vram::VramSlot {
                    asset,
                    clut_mode: psx_game_runtime::vram::VramSlotClutMode::ModelAtlas,
                    ready: true,
                    clut_word: 0,
                    tpage_word: 0,
                    texture_window: psx_gpu::material::TextureWindow::NONE,
                    texture_width: 256,
                    texture_height: 256,
                    region: psx_vram::VramHandle::Empty,
                    clut_region: psx_vram::VramHandle::Empty,
                })
            },
        );
        let instance = record.model_instance;
        let model = instances.get(usize::from(instance))?.model;
        models.get(model.to_usize())?.as_ref()?;
        Some(Self {
            tables: ModelTables {
                model_clip_bounds: clip_bounds,
                model_frame_bounds: &[],
                model_sockets: &[],
                model_instances: instances,
                equipment: &[],
                weapons: &[],
                weapon_hitboxes: &[],
                entities: &[],
            },
            models,
            clips,
            instance,
            capsules: leak(capsules.iter().map(capsule_record).collect()),
        })
    }

    /// The body presented as the player's character is: her Character's
    /// scale, offset and yaw on a yaw-only turn (`resolve_player_actor_pose`),
    /// not the placement she was cooked into.
    pub fn presented_as(mut self, body: &psx_level::LevelCharacterRecord) -> Self {
        let mut instances = self.tables.model_instances.to_vec();
        if let Some(instance) = instances.get_mut(usize::from(self.instance)) {
            instance.visual_scale_q8 = body.visual_scale_q8;
            instance.visual_offset = body.visual_offset;
            instance.visual_yaw = body.visual_yaw;
            instance.pitch = 0;
            instance.roll = 0;
        }
        self.tables.model_instances = leak(instances);
        self
    }

    fn model(&self) -> Option<RuntimeModelAsset> {
        let instance = self
            .tables
            .model_instances
            .get(usize::from(self.instance))?;
        self.models
            .get(instance.model.to_usize())
            .copied()
            .flatten()
    }

    /// How many ticks clip `clip` plays (the guest's
    /// `clip_duration_ticks`), for the fighter's animation tracks.
    pub fn clip_ticks(&self, clip: &GameEntityClip) -> u32 {
        self.model()
            .and_then(|model| model.clip(&*self.clips, ModelClipIndex(clip.clip)))
            .map_or(0, |animation| {
                mr::clip_duration_ticks(animation, VideoHz::NTSC, clip.speed_q8, clip.frame_range)
            })
    }

    /// Fighter slot 0 of `entities`, posed at `tick` as the disc poses it:
    /// its shown clip (the retained attack's clip while one is under way),
    /// crossfade and moving-attack leg layer.
    pub fn pose<const N: usize>(
        &self,
        entities: &GameEntities<N>,
        attack_clip: Option<GameEntityClip>,
        tick: SimTick,
    ) -> Option<InstanceActorPoseSnapshot> {
        let position = entities.position(0);
        let animation = entities.animation(0);
        let clip = attack_clip.unwrap_or(animation.clip);
        let overrides = [ModelInstancePoseOverride {
            instance: self.instance,
            x: position[0],
            y: position[1],
            z: position[2],
            yaw: entities.yaw(0),
            clip: OptionalModelClipIndex::some(ModelClipIndex(clip.clip)),
            phase_ticks: clip.phase_ticks,
            one_shot: clip.one_shot,
            speed_q8: clip.speed_q8,
            frame_range: clip.frame_range,
            layer: animation
                .fade
                .map(|(clip, alpha_q12)| ModelInstancePoseLayer {
                    clip,
                    alpha_q12,
                    joint_mask: u32::MAX,
                }),
        }];
        let pose = mr::resolve_instance_actor_pose(
            self.tables,
            &self.models,
            &self.clips,
            &overrides,
            usize::from(self.instance),
            tick,
            VideoHz::NTSC,
        )?;
        let legs = pose.model().body_masks.legs;
        let Some((gait, weight)) = animation.legs.filter(|_| legs != 0) else {
            return Some(pose);
        };
        let Some(gait_clip) = pose.model().clip(&*self.clips, ModelClipIndex(gait.clip)) else {
            return Some(pose);
        };
        let phase = mr::animation_phase_at_tick_q12(
            gait_clip,
            u32::from(gait.phase_ticks),
            VideoHz::NTSC,
            true,
            gait.speed_q8,
            gait.frame_range,
        );
        Some(
            pose.with_pose(
                pose.pose()
                    .with_weighted_joint_layer(gait_clip, phase, legs, weight),
            ),
        )
    }
}

/// A fighter's pose this tick and the one before on the same clip.
#[derive(Default)]
pub struct Poses {
    /// This tick's.
    pub now: Option<InstanceActorPoseSnapshot>,
    /// Last tick's.
    pub before: Option<InstanceActorPoseSnapshot>,
}

impl Poses {
    /// Take this tick's pose; last tick's is kept for the sweep.
    pub fn advance(&mut self, now: Option<InstanceActorPoseSnapshot>) {
        self.before = self.now.take();
        self.now = now;
    }

    /// This tick's pose.
    pub fn current(&self) -> Option<&ActorPoseSnapshot> {
        self.now.as_ref().map(InstanceActorPoseSnapshot::pose)
    }

    /// Last tick's pose when it played the same clip (the sweep's start).
    pub fn previous(&self) -> Option<&ActorPoseSnapshot> {
        let now = self.now.as_ref()?;
        self.before
            .as_ref()
            .filter(|before| before.clip_local() == now.clip_local())
            .map(InstanceActorPoseSnapshot::pose)
    }
}
