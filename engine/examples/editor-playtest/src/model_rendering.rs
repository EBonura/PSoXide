//! Glue over `psx_game_runtime::model_rendering` (and its
//! instances/equipment submodules): re-exports the draw vocabulary and
//! threads the cooked model tables, this example's knob/tuning consts,
//! and the arena-owned draw scratch into the crate policy, keeping the
//! old call-site signatures.

use super::*;
use crate::playtest_runtime::live_action_speed_q8;
use psx_game_runtime::model_rendering as mr;

#[cfg(feature = "collision-debug-overlay")]
pub(super) use psx_game_runtime::model_rendering::draw_collision_cylinder_debug;
pub(super) use psx_game_runtime::model_rendering::{
    accumulate_model_instance_draw_stats, distance_xz_sq, emit_model_counters, EquipmentDrawStats,
    InstanceActorPoseSnapshot, ModelInstanceDrawStats, ModelInstancePoseOverride, ModelTintSweep,
    PlayerActorPoseSnapshot, PlayerModelDrawStats, RuntimeModelAsset,
};

/// The cooked model-family tables bundled for the crate render policy.
pub(super) fn model_tables() -> mr::ModelTables {
    mr::ModelTables {
        model_clip_bounds: MODEL_CLIP_BOUNDS,
        model_frame_bounds: MODEL_FRAME_BOUNDS,
        model_sockets: MODEL_SOCKETS,
        model_instances: MODEL_INSTANCES,
        equipment: EQUIPMENT,
        weapons: WEAPONS,
        weapon_hitboxes: WEAPON_HITBOXES,
        entities: ENTITIES,
    }
}

const HORIZON_STANCE_RGB: (u8, u8, u8) = (255, 113, 58);
const ZENITH_STANCE_RGB: (u8, u8, u8) = (108, 224, 198);

fn stance_rgb(stance: VitalityChannelId) -> (u8, u8, u8) {
    match stance {
        VitalityChannelId::One => HORIZON_STANCE_RGB,
        VitalityChannelId::Two => ZENITH_STANCE_RGB,
    }
}

/// Stance colours the stance palettes are built from.
const HORIZON_TEXTURE_RGB: (u8, u8, u8) = (214, 75, 48);
const ZENITH_TEXTURE_RGB: (u8, u8, u8) = (67, 169, 154);

/// How far the player's stance palette moves each entry toward the stance
/// hue, Q8. The entry keeps its own brightness; 256 would be the pure hue.
const PLAYER_STANCE_PALETTE_MIX_Q8: i32 = 192;

/// How far the player's lit tint moves toward the stance colour after room
/// lighting, Q8, so the stance still reads under strongly coloured light.
const PLAYER_STANCE_LIT_TINT_Q8: u16 = 128;

/// The player's lit tint pulled toward the active stance colour.
pub(super) fn player_stance_lit_tint(stance: VitalityChannelId) -> mr::LitTintBias {
    mr::LitTintBias {
        color: stance_texture_rgb(stance),
        strength_q8: PLAYER_STANCE_LIT_TINT_Q8,
    }
}

fn stance_texture_rgb(stance: VitalityChannelId) -> (u8, u8, u8) {
    match stance {
        VitalityChannelId::One => HORIZON_TEXTURE_RGB,
        VitalityChannelId::Two => ZENITH_TEXTURE_RGB,
    }
}

const fn bgr555(entry: u16) -> [i32; 3] {
    [
        (entry & 31) as i32,
        ((entry >> 5) & 31) as i32,
        ((entry >> 10) & 31) as i32,
    ]
}

const fn pack_bgr555(rgb: [i32; 3]) -> u16 {
    (rgb[0] as u16) | ((rgb[1] as u16) << 5) | ((rgb[2] as u16) << 10)
}

/// The stance colour at `level` of 31, in 5-bit channels.
fn stance_color5(color: (u8, u8, u8), level: i32) -> [i32; 3] {
    [color.0, color.1, color.2].map(|channel| (i32::from(channel) * level / 31) >> 3)
}

/// Player palette: every entry moves toward the stance hue at the entry's
/// own brightness (relative to the brightest entry), so the body keeps its
/// shading ramp but reads as the stance colour instead of neutral. A
/// transparent entry stays the cut-out.
fn player_stance_palette(entries: &mut [u16], color: (u8, u8, u8)) {
    let brightest = entries
        .iter()
        .map(|&entry| bgr555(entry).into_iter().max().unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1);
    for entry in entries.iter_mut() {
        if *entry & 0x7fff == 0 {
            continue;
        }
        let source = bgr555(*entry);
        let level = source.into_iter().max().unwrap_or(0) * 31 / brightest;
        let target = stance_color5(color, level);
        let mut mixed = [0; 3];
        for channel in 0..3 {
            mixed[channel] = (source[channel]
                + (target[channel] - source[channel]) * PLAYER_STANCE_PALETTE_MIX_Q8 / 256)
                .clamp(0, 31);
        }
        *entry = pack_bgr555(mixed);
    }
}

/// Enemy palette: only the saturated red accents and their pink highlights
/// (eye lenses, lights, warning paint) take the stance colour, at the
/// accent's own brightness. The rest of the texture keeps its authored rust
/// and steel.
fn enemy_stance_palette(entries: &mut [u16], color: (u8, u8, u8)) {
    for entry in entries.iter_mut() {
        let [r, g, b] = bgr555(*entry);
        if r >= 12 && r >= g + 8 && r >= b + 8 {
            *entry = pack_bgr555(stance_color5(color, r));
        }
    }
}

/// Stance palette copies. Drawing through a copy swaps one CLUT word, so the
/// stance colour costs nothing per frame.
///
/// `rows` holds the enemies' model atlases keyed by the atlas's own CLUT word,
/// `[base, Horizon, Zenith]`. `player` holds the Horizon and Zenith copies of
/// the player's covering texture (its material override), built on the first
/// frame that texture is resident. Zero means not built.
#[derive(Copy, Clone)]
pub(super) struct StanceCluts {
    rows: [[u16; 3]; STANCE_CLUT_ATLASES],
    player: [u16; 2],
}

/// Distinct enemy atlases (Cortex shares one between all its enemies).
const STANCE_CLUT_ATLASES: usize = 2;

impl StanceCluts {
    pub(super) const EMPTY: Self = Self {
        rows: [[0; 3]; STANCE_CLUT_ATLASES],
        player: [0; 2],
    };

    /// The player's covering texture and the CLUT word of its `stance` copy.
    /// Characters without a covering texture keep their own look.
    pub(super) fn player_override_clut(
        &mut self,
        character: &RuntimeCharacter,
        stance: VitalityChannelId,
    ) -> Option<(AssetId, u16)> {
        let texture = character.material_override?.texture_asset?;
        let index = stance.index();
        if self.player[index] == 0 {
            // The texture streams with the rooms, so this waits for it.
            model_texture_slot(texture)?;
            self.player[index] = ensure_resident_clut_variant(texture, index as u8, |entries| {
                player_stance_palette(entries, stance_texture_rgb(stance))
            })?;
        }
        Some((texture, self.player[index]))
    }

    /// Build the Horizon and Zenith palette copies of one freshly uploaded
    /// model atlas, when an enemy wears it.
    pub(super) fn upload_for_atlas(
        &mut self,
        texture_asset: AssetId,
        atlas_bytes: &[u8],
        atlas_slot: VramSlot,
    ) {
        let enemy = GAME_ENTITIES.iter().any(|entity| {
            MODEL_INSTANCES
                .get(usize::from(entity.model_instance))
                .and_then(|instance| MODELS.get(instance.model.to_usize()))
                .is_some_and(|model| model.texture_asset == Some(texture_asset))
        });
        if !enemy {
            return;
        }
        let base = atlas_slot.clut_word;
        let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row[0] == base || row[0] == 0)
        else {
            return;
        };
        let mut words = [base; 3];
        for (variant, color) in [HORIZON_TEXTURE_RGB, ZENITH_TEXTURE_RGB]
            .into_iter()
            .enumerate()
        {
            let Some(word) = ensure_clut_variant(
                texture_asset,
                atlas_bytes,
                variant as u8,
                |entries| enemy_stance_palette(entries, color),
            ) else {
                return;
            };
            words[1 + variant] = word;
        }
        *row = words;
    }

    /// `material` redirected to its `stance` palette copy, if it has one.
    fn material(&self, material: TextureMaterial, stance: VitalityChannelId) -> TextureMaterial {
        let base = material.clut_word();
        match self.rows.iter().find(|row| row[0] == base && base != 0) {
            // Runtime atlas materials are plain `opaque` materials over the
            // slot's CLUT and tpage, so this rebuilds exactly that with the
            // copy's CLUT word.
            Some(row) => TextureMaterial::opaque(
                row[1 + stance.index()],
                material.tpage_word(),
                material.tint(),
            ),
            None => material,
        }
    }

    fn enemy_pose(
        &self,
        entities: &RuntimeGameEntities,
        pose: InstanceActorPoseSnapshot,
    ) -> InstanceActorPoseSnapshot {
        let entity = u16::try_from(pose.instance_index())
            .ok()
            .and_then(game_entity_for_instance);
        match entity {
            Some(entity) => pose.with_atlas_material(
                self.material(pose.model().material, entities.stance(entity)),
            ),
            None => pose,
        }
    }
}

/// This example's model draw knobs (the `MODEL_*`/`MAX_*` consts in
/// `runtime_config`, as the crate value struct).
const MODEL_DRAW_KNOBS: mr::ModelDrawKnobs = mr::ModelDrawKnobs {
    texture_split_max_edge: MODEL_TEXTURE_SPLIT_MAX_EDGE,
    max_model_instances: MAX_MODEL_INSTANCES,
    max_equipment_draws: MAX_EQUIPMENT_DRAWS,
    equipment_wire_q12: [mr::ASSEMBLED_Q12; mr::MAX_PLAYER_EQUIPMENT],
    equipment_materialization_skins: [None; mr::MAX_PLAYER_EQUIPMENT],
    instance_equipment_skin: None,
    equipment_materialization: false,
};

/// The Horizon palette, read back from the authored track that already
/// defines it: the player's R1 (Horizon-lane) blade trail. Enemy blades
/// take this so a held weapon reads as the same energy lane the player
/// swings on R1/R2, without a second authored copy of the colour to drift.
fn horizon_skin() -> Option<mr::EquipmentMaterializationSkin> {
    let controller = PLAYER_CONTROLLER?;
    let [r, g, b] = WEAPON_APPEARANCES
        .iter()
        .find(|appearance| {
            appearance.flags & psx_level::weapon_appearance_flags::TRAIL != 0
                && appearance.character == controller.character
                && appearance.action == CharacterAnimationAction::LightAttack
        })
        .map(|appearance| appearance.trail_tip_color)?;
    Some(mr::EquipmentMaterializationSkin::opaque((r, g, b)))
}

/// Cover the visible model, including its scale, rather than the body collider.
pub(super) fn player_phase_height(character: &RuntimeCharacter) -> i32 {
    MODELS
        .get(character.model.to_usize())
        .map_or(character.height, |model| {
            i32::from(model.world_height).saturating_mul(i32::from(character.visual_scale_q8)) / 256
        })
        .max(1)
}

/// Rebuild the player's mesh over a wireframe, following the stance clock.
pub(super) fn player_phase_assembly(
    stance: CombatStance,
    config: &CombatStanceConfig,
    position: RoomPoint,
    height: i32,
) -> Option<mr::ModelPhaseAssembly> {
    mr::ModelPhaseAssembly::new(
        stance.swap_elapsed_ticks(),
        config.swap_duration_ticks,
        stance_rgb(stance.active()),
        position.y,
        height,
    )
}

/// Resolve the same colour tell for one entity-owned model instance. Static
/// props have no game-entity owner and return `None`; enemy stance remains the
/// sole state authority.
fn enemy_stance_tint_sweep(
    entities: &RuntimeGameEntities,
    pose: &InstanceActorPoseSnapshot,
    camera: &WorldCamera,
) -> Option<ModelTintSweep> {
    let instance = u16::try_from(pose.instance_index()).ok()?;
    let entity = game_entity_for_instance(instance)?;
    if !entities.stance_swap_in_progress(entity) {
        return None;
    }
    let [x, y, z] = entities.position(entity);
    let visual_scale_q8 = MODEL_INSTANCES
        .get(pose.instance_index())
        .map_or(256, |record| record.visual_scale_q8.max(1));
    let height =
        i32::from(pose.model().world_height).saturating_mul(i32::from(visual_scale_q8)) / 256;
    let floor = camera.project_world(WorldVertex::new(x, y, z))?;
    let head = camera.project_world(WorldVertex::new(x, y.saturating_add(height), z))?;
    Some(ModelTintSweep::rising(
        stance_rgb(entities.stance(entity)),
        entities.stance_swap_progress_q12(entity),
        head.sy,
        floor.sy,
    ))
}

/// Resolve Horizon/Zenith presentation from the same action-authored colours
/// that drive the blade trail. This keeps the cage, textured weapon and trail
/// on one palette without adding a second gameplay classification.
fn equipment_materialization_skins(
    anim: PlayerAnim,
) -> [Option<mr::EquipmentMaterializationSkin>; mr::MAX_PLAYER_EQUIPMENT] {
    let mut skins = [None; mr::MAX_PLAYER_EQUIPMENT];
    let Some(controller) = PLAYER_CONTROLLER else {
        return skins;
    };
    let action = anim.action();
    // Dual attacks may materialise a second blade on a track that does not
    // author its own trail. Resolve the Horizon/Zenith colour once from the
    // action's trail-bearing track, then apply it to every participating
    // weapon/socket pair.
    let Some([r, g, b]) = WEAPON_APPEARANCES
        .iter()
        .find(|appearance| {
            appearance.flags & psx_level::weapon_appearance_flags::TRAIL != 0
                && appearance.character == controller.character
                && appearance.action == action
        })
        .map(|appearance| appearance.trail_tip_color)
    else {
        return skins;
    };
    for (index, record) in EQUIPMENT.iter().enumerate().take(mr::MAX_PLAYER_EQUIPMENT) {
        let participates = WEAPON_APPEARANCES.iter().any(|appearance| {
            appearance.character == controller.character
                && appearance.action == action
                && appearance.weapon == record.weapon
                && appearance.character_socket == record.character_socket
        });
        if !participates {
            continue;
        }
        skins[index] = Some(mr::EquipmentMaterializationSkin::opaque((r, g, b)));
    }
    skins
}

/// How far up a weapon this authored visibility beat has reached, Q12.
/// The same sampled-frame transition runs into the fully-visible marker and
/// backwards into the hidden marker. A zero transition is a deliberate cut.
fn wire_for_appearance(
    appearance: &psx_level::WeaponAppearanceRecord,
    phase_q12: u32,
    frame_count: u16,
) -> u16 {
    let until = if appearance.hidden_frame == psx_level::CHARACTER_ACTION_FRAME_END_FULL {
        frame_count.saturating_sub(1)
    } else {
        appearance.hidden_frame.min(frame_count.saturating_sub(1))
    };
    let visible_q12 = u32::from(appearance.fully_visible_frame) << 12;
    let until_q12 = u32::from(until).saturating_mul(4096);
    if appearance.transition_frames == 0 {
        return if phase_q12 >= visible_q12 && phase_q12 < until_q12 {
            mr::ASSEMBLED_Q12
        } else {
            0
        };
    }
    let transition = u32::from(appearance.transition_frames);
    let ramp_q12 = transition << 12;
    let open_q12 = visible_q12.saturating_sub(ramp_q12);
    if phase_q12 < open_q12 || phase_q12 >= until_q12 {
        return 0;
    }
    let rising = (phase_q12 - open_q12) / transition;
    let falling = (until_q12 - phase_q12) / transition;
    rising.min(falling).min(u32::from(mr::ASSEMBLED_Q12)) as u16
}

/// How far up the blade every player equipment record has grown this frame.
///
/// Each cooked record is matched to an Animation Studio track by character,
/// action, weapon id, and socket. Scene/table reordering therefore cannot arm
/// the wrong hand.
pub(super) fn equipment_wire_q12(
    anim: PlayerAnim,
    phase_q12: u32,
    frame_count: u16,
) -> [u16; mr::MAX_PLAYER_EQUIPMENT] {
    let mut wire = [0u16; mr::MAX_PLAYER_EQUIPMENT];
    let Some(controller) = PLAYER_CONTROLLER else {
        return wire;
    };
    let action = anim.action();
    for (index, record) in EQUIPMENT.iter().enumerate().take(mr::MAX_PLAYER_EQUIPMENT) {
        let Some(appearance) = WEAPON_APPEARANCES.iter().find(|appearance| {
            appearance.character == controller.character
                && appearance.action == action
                && appearance.weapon == record.weapon
                && appearance.character_socket == record.character_socket
        }) else {
            continue;
        };
        wire[index] = wire_for_appearance(appearance, phase_q12, frame_count);
    }
    wire
}

/// This example's actor floor-shadow tuning (the `SHADOW_*` consts in
/// `runtime_config`, as the crate value struct).
/// Tuning for the projected (flattened-geometry) actor shadow.
///
/// `depth_bias` is ADDED to the actor clearance the caller already applied,
/// so the shadow sits just behind the actor's own body but still in front of
/// the floor it lands on.
#[cfg(feature = "actor-shadows-projected")]
const PROJECTED_SHADOW_TUNING: mr::ProjectedShadowTuning = mr::ProjectedShadowTuning {
    light: mr::ShadowLight::OVERHEAD,
    floor_lift: SHADOW_FLOOR_LIFT,
    depth_bias: 0,
    blend: psx_gpu::material::BlendMode::Average,
    tint: (0, 0, 0),
    max_drop: 512,
};

const SHADOW_TUNING: mr::ShadowTuning = mr::ShadowTuning {
    floor_lift: SHADOW_FLOOR_LIFT,
    depth_bias: SHADOW_DEPTH_BIAS,
    radius_scale_num: SHADOW_RADIUS_SCALE_NUM,
    radius_scale_den: SHADOW_RADIUS_SCALE_DEN,
    radius_min: SHADOW_RADIUS_MIN,
    radius_max: SHADOW_RADIUS_MAX,
};

impl Playtest {
    #[cfg(feature = "cd-stream-bench")]
    #[inline(never)]
    pub(super) fn step_persistent_model_assets(&mut self) -> bool {
        {
            let assets = persistent_assets_arena_mut();
            assets.begin(UI_PACK_START_LBA, UI_PACK_TOC, ASSETS);
            assets.pump(cd_arena(), RUNTIME_SCHEDULE.stream_pump_sectors_per_tick);
        }
        if persistent_assets_arena().ready() && !self.runtime_models_loaded {
            self.load_runtime_models();
            self.runtime_models_loaded = true;
            // Model parsing owns the CD first. Only now seed portal visibility
            // and the incremental room-window job; the same tick can reconcile
            // and pump WORLD.PAK without two readers sharing the controller.
            if self.bsp.is_none() {
                self.load_active_room_window();
            }
        }
        self.runtime_models_loaded
    }

    pub(super) fn player_clip_duration_vblanks(
        &self,
        character: &RuntimeCharacter,
        clip: ModelClipIndex,
        video_hz: VideoHz,
        speed_q8: u16,
        frame_range: psx_level::CharacterActionFrameRange,
    ) -> Option<u32> {
        mr::player_clip_duration_vblanks(
            &self.models,
            &self.clips,
            character,
            clip,
            video_hz,
            speed_q8,
            frame_range,
        )
    }

    pub(super) fn player_action_push_speed(
        &self,
        character: &RuntimeCharacter,
        anim: PlayerAnim,
        local_tick: u32,
        video_hz: VideoHz,
    ) -> Option<i32> {
        mr::player_action_push_speed(
            &self.models,
            &self.clips,
            character,
            anim,
            local_tick,
            video_hz,
        )
    }

    #[inline(never)]
    pub(super) fn load_runtime_models(&mut self) {
        #[cfg(not(feature = "cd-stream-bench"))]
        mr::load_runtime_models(
            MODELS,
            MODEL_CLIPS,
            &mut self.models,
            &mut self.clips,
            &mut self.model_faces,
            &mut self.model_face_count,
            &mut self.model_parts,
            &mut self.model_part_count,
            &mut self.model_vertices,
            &mut self.model_vertex_count,
            runtime_model_asset_bytes,
            |texture_asset, atlas_bytes| {
                let slot = ensure_model_atlas_uploaded(texture_asset, atlas_bytes)?;
                self.stance_cluts
                    .upload_for_atlas(texture_asset, atlas_bytes, slot);
                Some(slot)
            },
        );
        #[cfg(feature = "cd-stream-bench")]
        self.load_streamed_runtime_models();
        assert!(
            self.models.iter().take(MODELS.len()).all(Option::is_some),
            "runtime model load dropped a cooked model"
        );
        self.sort_weapon_faces_hilt_first();
    }

    /// Decode model source blobs from the shared loading scratch, retaining
    /// only compact geometry pools and VRAM atlas slots for gameplay.
    #[cfg(feature = "cd-stream-bench")]
    fn load_streamed_runtime_models(&mut self) {
        mr::reset_runtime_model_tables(
            &mut self.models,
            &mut self.clips,
            &mut self.model_face_count,
            &mut self.model_part_count,
            &mut self.model_vertex_count,
        );
        mr::load_runtime_model_clips(MODEL_CLIPS, &mut self.clips, runtime_model_asset_bytes);

        for (index, record) in MODELS.iter().enumerate() {
            if index >= self.models.len() {
                break;
            }
            let Some(texture_asset) = record.texture_asset else {
                continue;
            };
            let stance_cluts = &mut self.stance_cluts;
            let Some(atlas_slot) = with_transient_gameplay_asset_bytes(
                texture_asset,
                AssetKind::Texture,
                |atlas_bytes| {
                    let slot = ensure_model_atlas_uploaded(texture_asset, atlas_bytes)?;
                    stance_cluts.upload_for_atlas(texture_asset, atlas_bytes, slot);
                    Some(slot)
                },
            )
            .flatten() else {
                continue;
            };
            let decoded = with_transient_gameplay_asset_bytes(
                record.mesh_asset,
                AssetKind::ModelMesh,
                |mesh_bytes| {
                    RuntimeModelAsset::from_record_bytes(
                        psx_level::ModelIndex::new(index as u16),
                        record,
                        mesh_bytes,
                        atlas_slot,
                        &mut self.model_faces,
                        &mut self.model_face_count,
                        &mut self.model_parts,
                        &mut self.model_part_count,
                        &mut self.model_vertices,
                        &mut self.model_vertex_count,
                    )
                },
            )
            .flatten();
            self.models[index] = decoded;
        }
    }

    /// Drop every parsed view into the scene-lifetime gameplay asset arena.
    /// Call before handing those bytes back to the front-end UI cache.
    pub(super) fn unload_runtime_models(&mut self) {
        for model in self.models.iter_mut() {
            *model = None;
        }
        for clip in self.clips.iter_mut() {
            *clip = None;
        }
        self.model_face_count = 0;
        self.model_part_count = 0;
        self.model_vertex_count = 0;
        self.runtime_models_loaded = false;
        self.stance_cluts = StanceCluts::EMPTY;
        self.clear_actor_pose_snapshots();
    }

    /// Order every weapon model's faces along its blade, hilt first.
    ///
    /// This is what makes the materialise effect cheap: with the faces in this
    /// order, "filled up to here" is a slice of the face list, so the solid
    /// part goes through the ordinary model path in one call instead of being
    /// submitted face by face. Face order is otherwise arbitrary, since the
    /// ordering table sorts by depth.
    fn sort_weapon_faces_hilt_first(&mut self) {
        for weapon in WEAPONS {
            let Some(model_index) = weapon.model else {
                continue;
            };
            let Some(Some(model)) = self.models.get(model_index.to_usize()) else {
                continue;
            };
            let first = model.face_first as usize;
            let count = model.face_count as usize;
            let vertex_first = model.vertex_first as usize;
            if first + count > self.model_face_count {
                continue;
            }
            // Insertion sort: a weapon has fewer than a hundred faces, and the
            // guest does not need a general sort dragged in for it.
            let axis = |face: &TexturedModelRenderFace| -> i32 {
                let mut sum = 0i32;
                for corner in 0..3 {
                    let index = vertex_first + face.vertex_indices()[corner] as usize;
                    if let Some(vertex) = self.model_vertices.get(index) {
                        sum += i32::from(vertex.position.y);
                    }
                }
                sum
            };
            for i in first + 1..first + count {
                let mut j = i;
                while j > first && axis(&self.model_faces[j - 1]) > axis(&self.model_faces[j]) {
                    self.model_faces.swap(j - 1, j);
                    j -= 1;
                }
            }
        }
    }
}

fn runtime_model_asset_bytes(asset_id: AssetId, kind: AssetKind) -> Option<&'static [u8]> {
    let asset = find_asset_of_kind(ASSETS, asset_id, kind)?;
    if !asset.bytes.is_empty() {
        return Some(asset.bytes);
    }
    #[cfg(feature = "cd-stream-bench")]
    {
        persistent_assets_arena().bytes_for(asset)
    }
    #[cfg(not(feature = "cd-stream-bench"))]
    {
        None
    }
}

/// Read one transient gameplay asset through the shared loading scratch and
/// consume it before the next staged read overwrites that memory.
#[cfg(feature = "cd-stream-bench")]
fn with_transient_gameplay_asset_bytes<R>(
    asset_id: AssetId,
    kind: AssetKind,
    consume: impl FnOnce(&[u8]) -> R,
) -> Option<R> {
    let asset = find_asset_of_kind(ASSETS, asset_id, kind)?;
    if !asset.bytes.is_empty() {
        return Some(consume(asset.bytes));
    }
    if asset.flags & psx_level::asset_flags::STREAMED_GAMEPLAY_TRANSIENT == 0 {
        return None;
    }
    let byte_count = asset.ram_bytes as usize;
    let scratch = font_scratch_arena();
    let stage = scratch.stage_words_mut(byte_count.div_ceil(4))?;
    let result = psx_game_runtime::cd_stream::read_chunk_blocking(
        cd_arena(),
        UI_PACK_START_LBA,
        UI_PACK_TOC,
        asset.id.0 as u32,
        stage,
    );
    if result.status != psx_game_runtime::cd_stream::ROOM_CHUNK_STATUS_OK
        || result.bytes != byte_count
    {
        return None;
    }
    Some(consume(scratch.staged_bytes(result.bytes)?))
}

/// Read UI SFX sample `index` off UI.PAK into the font/sky staging scratch
/// and hand its bytes to `consume`. Runs once per sample at boot, before the
/// first font pack, so the scratch is free; the bank then lives in SPU RAM
/// only.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn with_streamed_ui_sfx_sample(index: usize, consume: &mut dyn FnMut(&[u8])) -> bool {
    let chunk = UI_SFX_PACK_FIRST_CHUNK + index as u32;
    let Some(entry) = UI_PACK_TOC
        .iter()
        .find(|entry| u32::from(entry.room.raw()) == chunk)
    else {
        return false;
    };
    let byte_count = entry.byte_size as usize;
    let scratch = font_scratch_arena();
    let Some(stage) = scratch.stage_words_mut(byte_count.div_ceil(4)) else {
        return false;
    };
    let result = psx_game_runtime::cd_stream::read_chunk_blocking(
        cd_arena(),
        UI_PACK_START_LBA,
        UI_PACK_TOC,
        chunk,
        stage,
    );
    if result.status != psx_game_runtime::cd_stream::ROOM_CHUNK_STATUS_OK
        || result.bytes != byte_count
    {
        return false;
    }
    let Some(bytes) = scratch.staged_bytes(result.bytes) else {
        return false;
    };
    consume(bytes);
    true
}

fn room_reflection_probe_slot(room: RoomIndex) -> Option<VramSlot> {
    let asset = ROOM_REFLECTION_PROBES
        .get(room.to_usize())
        .copied()
        .flatten()?;
    find_room_texture_vram_slot(asset)
}

impl Playtest {
    /// Freeze actor presentation after the simulation tick. Rendering,
    /// equipment sockets, and combat consume these values until the next tick.
    pub(super) fn refresh_actor_pose_snapshots(&mut self, ctx: &Ctx) {
        let player = self.motor.position();
        self.previous_player_actor_pose = self.player_actor_pose;
        // `RuntimeCharacter` is 672 bytes and the R3000A has no data cache, so
        // every by-value hop through this path is a full-latency RAM copy. The
        // cooked record is borrowed from here down.
        self.player_actor_pose = match self.character.as_ref() {
            Some(character) => {
                // The vitality Attack Speed lane was the only thing that made
                // the presentation character differ from the cooked one, and
                // the resolver reads exactly two speeds because of it. Hand it
                // those two numbers instead of a retuned copy of the record.
                let blend = self.player_anim_blend(ctx.sim_tick);
                let modifiers = self.vitality_modifiers();
                let speeds = mr::PlayerActionSpeeds {
                    action_q8: live_action_speed_q8(modifiers, character, self.anim_state),
                    blend_q8: blend.map_or(0, |blend| {
                        live_action_speed_q8(modifiers, character, blend.anim)
                    }),
                };
                let clip_local = character.clip_for(self.anim_state);
                // The clip's first-frame root is constant for the clip, and
                // sampling it was a whole pose decode per tick. Last tick's
                // snapshot already carries it; reuse it while the same clip of
                // the same model is playing. A clip or model change simply
                // misses and the resolver samples once.
                let cached_clip_first_root_xz = self
                    .previous_player_actor_pose
                    .filter(|previous| {
                        previous.clip_local() == clip_local
                            && previous.model().index == character.model
                    })
                    .and_then(|previous| previous.clip_first_root_xz());
                mr::resolve_player_actor_pose(
                    model_tables(),
                    character,
                    &self.models,
                    &self.clips,
                    player.x,
                    player.y,
                    player.z,
                    self.motor.yaw(),
                    self.anim_state.action(),
                    clip_local,
                    self.anim_start_tick,
                    blend,
                    speeds,
                    cached_clip_first_root_xz,
                    ctx.sim_tick,
                    ctx.video_hz,
                )
            }
            None => None,
        };

        // Where rendering samples the phase is where it is worth measuring:
        // this is the number that says which cooked frame is on screen.
        if let Some(pose) = self.player_actor_pose {
            self.player_dash_assembly.observe(pose);
            if pose.action() == CharacterAnimationAction::Intro
                && self.opening.take_punch(pose.pose().phase_q12())
            {
                self.player_dash_assembly.burst(ctx.sim_tick);
            }
            telemetry::counter(
                telemetry::counter::PLAYER_ANIM_PHASE_Q12,
                pose.pose().phase_q12(),
            );
        }

        self.player_swing_sound();
        let mut overrides =
            psx_engine::FixedScratch::<ModelInstancePoseOverride, MAX_GAME_ENTITIES>::new();
        self.game_entity_pose_overrides(&mut overrides);
        let overrides = overrides.as_slice();
        let elapsed_tick = self.gameplay_tick(ctx.sim_tick);
        let count = MODEL_INSTANCES.len().min(self.instance_actor_poses.len());
        let bsp_resident = self.bsp.is_some();
        let mut index = 0usize;
        while index < count {
            // The visibility mask discards the pose after resolving it, so
            // test it first. `resolve_instance_actor_pose` reads only shared
            // tables, which makes the skip output-identical.
            let corpse_gone = GAME_ENTITIES
                .iter()
                .enumerate()
                .find(|(_, r)| usize::from(r.model_instance) == index)
                .is_some_and(|(entity, record)| {
                    if record.flags & psx_level::game_entity_flags::ENABLED == 0 {
                        return true;
                    }
                    if self.game_entities.state(entity)
                        != psx_game_runtime::entities::GameEntityState::Dead
                    {
                        return false;
                    }
                    self.models
                        .get(MODEL_INSTANCES[index].model.to_usize())
                        .copied()
                        .flatten()
                        .and_then(|model| {
                            model.clip(&self.clips, psx_level::ModelClipIndex(record.death_clip))
                        })
                        .and_then(|animation| {
                            enemy_death_dissolve(
                                &self.game_entities,
                                index,
                                animation,
                                ctx.video_hz,
                            )
                        })
                        .is_some_and(|effect| effect.finished())
                });
            if corpse_gone
                || (bsp_resident && self.bsp_instance_visible_mask & (1u16 << index) == 0)
            {
                self.instance_actor_poses[index] = None;
                self.previous_instance_actor_poses[index] = None;
                index += 1;
                continue;
            }
            let previous = self.instance_actor_poses[index];
            self.previous_instance_actor_poses[index] = previous;
            self.instance_actor_poses[index] = mr::resolve_instance_actor_pose(
                model_tables(),
                &self.models,
                &self.clips,
                overrides,
                index,
                elapsed_tick,
                ctx.video_hz,
            );
            self.enemy_swing_sound(index, previous, self.instance_actor_poses[index]);
            index += 1;
        }
        self.instance_actor_poses[count..].fill(None);
        self.previous_instance_actor_poses[count..].fill(None);
    }

    pub(super) fn clear_actor_pose_snapshots(&mut self) {
        self.player_actor_pose = None;
        self.player_dash_assembly = mr::PlayerDashAssembly::new();
        self.previous_player_actor_pose = None;
        self.previous_instance_actor_poses.fill(None);
        for pose in self.instance_actor_poses.iter_mut() {
            *pose = None;
        }
    }
}

/// Draw the player's animated model through the crate policy.
pub(super) fn draw_player(
    current_room: RoomIndex,
    character: &RuntimeCharacter,
    player_pose: PlayerActorPoseSnapshot,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    phase_assembly: Option<mr::ModelPhaseAssembly>,
    dash_assembly: &mut mr::PlayerDashAssembly,
    stance_clut: Option<(AssetId, u16)>,
    stance_tint: Option<mr::LitTintBias>,
    triangles: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> PlayerModelDrawStats {
    let phase_assembly = phase_assembly.filter(|_| {
        matches!(
            dash_assembly.visual(elapsed_tick),
            mr::DashWireVisual::Solid
        )
    });
    let finish = phase_assembly.filter(|effect| effect.is_assembled());
    let first_slot = triangles.used_slots();
    let mut stats = mr::draw_player_from_pose::<
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_BOUNDS_CULLING_ENABLED,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        MODEL_DRAW_KNOBS,
        model_scratch_arena(),
        character,
        player_pose,
        model_faces,
        model_parts,
        model_vertices,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        room_reflection_probe_slot(current_room),
        &mut |asset| {
            // The covering texture is drawn through its stance palette copy.
            let mut slot = model_texture_slot(asset)?;
            if let Some((_, clut_word)) = stance_clut.filter(|(texture, _)| *texture == asset) {
                slot.clut_word = clut_word;
            }
            Some(slot)
        },
        phase_assembly.filter(|effect| !effect.is_assembled()),
        Some(dash_assembly),
        stance_tint,
        triangles,
        world,
    );
    if let Some(finish) = finish {
        // SAFETY: the solid body submit above emits only TriTextured packets.
        // Dash line phases are excluded, and equipment has not been submitted.
        let _ = unsafe { finish.apply_finish_to_model_packets(triangles, first_slot) };
    }
    stats.stats.submitted_triangles =
        stats
            .stats
            .submitted_triangles
            .saturating_add(dash_assembly.draw_departure(
                elapsed_tick,
                *camera,
                options,
                triangles,
                world,
            ));
    stats
}

/// Draw non-player equipment riding its bound model instances (the
/// per-room enemy weapon pass) through the crate policy.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_instance_equipment(
    current_room: RoomIndex,
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    max_draws: usize,
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut out = EquipmentDrawStats::default();
    let mut remaining = max_draws.min(MODEL_DRAW_KNOBS.max_equipment_draws);
    let instance_equipment_skin = horizon_skin();
    // By reference: `.copied().flatten()` moved every slot's whole
    // `Option<InstanceActorPoseSnapshot>` (and, once PGO stopped inlining the
    // iterator, through compiler-builtins' memmove) just to read it.
    for pose in instance_poses.iter().flatten() {
        if remaining == 0 {
            break;
        }
        let mut knobs = MODEL_DRAW_KNOBS;
        knobs.max_equipment_draws = remaining;
        knobs.instance_equipment_skin = instance_equipment_skin;
        let stats = mr::draw_instance_equipment_from_pose::<
            MAX_RUNTIME_MODELS,
            MAX_RUNTIME_MODEL_CLIPS,
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            MODEL_PROFILE_ENABLED,
        >(
            model_tables(),
            knobs,
            model_scratch_arena(),
            current_room,
            pose,
            elapsed_tick,
            video_hz,
            camera,
            options,
            lighting,
            models,
            model_faces,
            model_parts,
            model_vertices,
            clips,
            triangles,
            world,
        );
        mr::accumulate_equipment_draw_stats(&mut out, stats);
        remaining = remaining.saturating_sub(stats.draws as usize);
        if stats.stats.primitive_overflow || stats.stats.command_overflow {
            break;
        }
    }
    out
}

/// Draw the player's attached equipment through the crate policy.
pub(super) fn draw_player_equipment(
    anim: PlayerAnim,
    wire_q12: [u16; mr::MAX_PLAYER_EQUIPMENT],
    player_pose: PlayerActorPoseSnapshot,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    clips: &[Option<Animation<'static>>; MAX_RUNTIME_MODEL_CLIPS],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut (impl PrimitiveSink<TriTextured>
              + PrimitiveSink<psx_gpu::prim::LineMono>
              + PrimitiveSink<psx_gpu::prim::QuadGouraudBlended>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> EquipmentDrawStats {
    let mut knobs = MODEL_DRAW_KNOBS;
    knobs.equipment_wire_q12 = wire_q12;
    knobs.equipment_materialization_skins = equipment_materialization_skins(anim);
    knobs.equipment_materialization = true;
    let mut out = mr::draw_player_equipment_from_pose::<
        MAX_RUNTIME_MODELS,
        MAX_RUNTIME_MODEL_CLIPS,
        MODEL_VERTEX_CAP,
        JOINT_CAP,
        OT_DEPTH,
        MODEL_PROFILE_ENABLED,
    >(
        model_tables(),
        knobs,
        model_scratch_arena(),
        player_pose,
        models,
        model_faces,
        model_parts,
        model_vertices,
        clips,
        elapsed_tick,
        video_hz,
        camera,
        options,
        lighting,
        triangles,
        world,
    );
    if let Some(controller) = PLAYER_CONTROLLER {
        let trail = mr::draw_player_weapon_trails_from_pose::<MAX_RUNTIME_MODELS, OT_DEPTH>(
            model_tables(),
            WEAPON_APPEARANCES,
            controller.character,
            anim.action(),
            wire_q12,
            player_pose,
            models,
            model_parts,
            model_vertices,
            camera,
            options,
            triangles,
            world,
        );
        out.stats.submitted_triangles = out
            .stats
            .submitted_triangles
            .saturating_add(trail.submitted_triangles);
        out.stats.culled_triangles = out
            .stats
            .culled_triangles
            .saturating_add(trail.culled_triangles);
        out.stats.dropped_triangles = out
            .stats
            .dropped_triangles
            .saturating_add(trail.dropped_triangles);
        out.stats.primitive_overflow |= trail.primitive_overflow;
        out.stats.command_overflow |= trail.command_overflow;
    }
    out
}

/// One dissolving corpse at a time keeps its posed vertices here (about
/// 3.7 KB); a second corpse dissolving at the same time takes the slower,
/// per-frame path.
fn death_capture() -> &'static mut mr::ModelDeathCapture<MODEL_VERTEX_CAP> {
    static mut CAPTURE: mr::ModelDeathCapture<MODEL_VERTEX_CAP> = mr::ModelDeathCapture::new();
    // SAFETY: the model draw pass runs on the one guest thread and holds the
    // reference only for one instance draw.
    unsafe { &mut *core::ptr::addr_of_mut!(CAPTURE) }
}

/// The entity timer keeps running offscreen, so culled corpses cannot restart.
#[inline(never)]
fn enemy_death_dissolve(
    entities: &RuntimeGameEntities,
    instance: usize,
    animation: Animation<'static>,
    video_hz: VideoHz,
) -> Option<mr::ModelDeathDissolve> {
    let index = GAME_ENTITIES
        .iter()
        .position(|r| usize::from(r.model_instance) == instance)?;
    if entities.state(index) != psx_game_runtime::entities::GameEntityState::Dead {
        return None;
    }
    mr::ModelDeathDissolve::after_death(
        entities.clip_for_state(GAME_ENTITIES, index).phase_ticks,
        animation.frame_count(),
        animation.sample_rate_hz(),
        video_hz,
    )
}

/// Animate + draw the placed model instances of `current_room` through
/// the crate policy.
pub(super) fn draw_model_instances(
    current_room: RoomIndex,
    entities: &RuntimeGameEntities,
    stance_cluts: &StanceCluts,
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    elapsed_tick: SimTick,
    video_hz: VideoHz,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> ModelInstanceDrawStats {
    let mut out = ModelInstanceDrawStats::default();
    for pose in instance_poses
        .iter()
        .take(MODEL_DRAW_KNOBS.max_model_instances)
        .flatten()
    {
        // An enemy wears its guard: the accents of its atlas are drawn
        // through the palette copy of its current stance.
        let pose = &stance_cluts.enemy_pose(entities, *pose);
        let first_slot = triangles.used_slots();
        let stats = mr::draw_model_instance_from_pose::<
            MODEL_VERTEX_CAP,
            JOINT_CAP,
            OT_DEPTH,
            MODEL_BOUNDS_CULLING_ENABLED,
            MODEL_PROFILE_ENABLED,
        >(
            model_tables(),
            MODEL_DRAW_KNOBS,
            model_scratch_arena(),
            current_room,
            pose,
            enemy_death_dissolve(
                entities,
                pose.instance_index(),
                pose.pose().animation(),
                video_hz,
            ),
            Some(death_capture()),
            elapsed_tick,
            video_hz,
            camera,
            options,
            lighting,
            room_reflection_probe_slot(current_room),
            model_faces,
            model_parts,
            model_vertices,
            &mut model_texture_slot,
            triangles,
            world,
        );
        // Resolve/project the tell only after the ordinary instance draw
        // survived room and bounds rejection. Static props still stop
        // at the cheap owner lookup; only an actively swapping enemy pays the
        // two endpoint projections and packet post-pass.
        if triangles.used_slots() > first_slot {
            if let Some(sweep) = enemy_stance_tint_sweep(entities, pose, camera) {
                // SAFETY: an instance body submit emits only TriTextured
                // packets in this immediate arena range. Enemy equipment is
                // a later pass.
                let _ = unsafe { sweep.apply_to_model_packets(triangles, first_slot) };
            }
        }
        accumulate_model_instance_draw_stats(&mut out, stats);
        if stats.stats.primitive_overflow || stats.stats.command_overflow {
            break;
        }
    }
    out
}

/// Draw the floor shadow decal under every placed model instance.
pub(super) fn draw_model_instance_shadows(
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    models: &[Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    pose_overrides: &[ModelInstancePoseOverride],
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_model_instance_shadows(
        model_tables(),
        MODEL_DRAW_KNOBS,
        SHADOW_TUNING,
        current_room,
        camera,
        options,
        material,
        models,
        pose_overrides,
        visible_instance_mask
            & instance_poses
                .iter()
                .enumerate()
                .fold(0, |mask, (i, pose)| {
                    if pose.is_some() {
                        mask | (1 << i)
                    } else {
                        mask
                    }
                }),
        triangles,
        world,
    );
}

/// Draw one actor's circular floor shadow decal.
pub(super) fn draw_actor_shadow(
    x: i32,
    floor_y: i32,
    z: i32,
    radius: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_actor_shadow(
        SHADOW_TUNING,
        x,
        floor_y,
        z,
        radius,
        camera,
        options,
        material,
        triangles,
        world,
    );
}

/// Draw a 64x64 gameplay decal from the shared effects page without borrowing
/// the actor-shadow material policy.
pub(super) fn draw_ground_decal(
    x: i32,
    floor_y: i32,
    z: i32,
    radius: i32,
    uv_origin: (u8, u8),
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    material: TextureMaterial,
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_ground_decal(
        x,
        floor_y,
        z,
        radius,
        SHADOW_FLOOR_LIFT,
        SHADOW_DEPTH_BIAS,
        uv_origin,
        camera,
        options,
        material,
        triangles,
        world,
    );
}

/// Shadow decal radius for an actor's collision radius.
pub(super) fn actor_shadow_radius(base_radius: i32) -> i32 {
    mr::actor_shadow_radius(SHADOW_TUNING, base_radius)
}

/// Draw the player's own geometry flattened onto the floor plane.
#[cfg(feature = "actor-shadows-projected")]
pub(super) fn draw_player_projected_shadow(
    player_pose: PlayerActorPoseSnapshot,
    floor_y: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let runtime_model = player_pose.model();
    mr::draw_actor_projected_shadow(
        PROJECTED_SHADOW_TUNING,
        model_scratch_arena(),
        runtime_model,
        player_pose.pose(),
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
}

/// Draw the placed model instances of `current_room` flattened onto their
/// floor planes.
#[cfg(feature = "actor-shadows-projected")]
pub(super) fn draw_model_instance_projected_shadows(
    current_room: RoomIndex,
    instance_poses: &[Option<InstanceActorPoseSnapshot>; MAX_MODEL_INSTANCES],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    // psx-numeric-allow-next-line: one bit per model instance; the width IS the instance capacity
    visible_instance_mask: u64,
    model_faces: &[TexturedModelRenderFace],
    model_parts: &[ModelPart],
    model_vertices: &[ModelVertex],
    triangles: &mut (impl PrimitiveSink<TriTextured> + PrimitiveSink<psx_gpu::prim::LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    mr::draw_model_instance_projected_shadows(
        model_tables(),
        MODEL_DRAW_KNOBS,
        PROJECTED_SHADOW_TUNING,
        model_scratch_arena(),
        current_room,
        instance_poses,
        camera,
        options,
        visible_instance_mask,
        model_faces,
        model_parts,
        model_vertices,
        triangles,
        world,
    );
}
