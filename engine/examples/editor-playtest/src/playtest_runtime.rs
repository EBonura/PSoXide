use super::*;

enum PoiSaveLoad {
    Loaded(SaveBlock),
    NewGame,
    Retry,
}

fn load_poi_save_from_card() -> PoiSaveLoad {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::new(psx_mc::Slot::One));
    let mut bytes = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
    let len = match card.read(PROJECT_SAVE_NAME, &mut bytes) {
        Ok(len) => len,
        Err(psx_mc::Error::NotFound) => return PoiSaveLoad::NewGame,
        Err(_) => return PoiSaveLoad::Retry,
    };
    if len != bytes.len() {
        return PoiSaveLoad::Retry;
    }
    match SaveBlock::decode(&bytes, PERSISTENT_FLAG_COUNT) {
        Some(save) => PoiSaveLoad::Loaded(save),
        None => PoiSaveLoad::Retry,
    }
}

fn save_poi_state_to_card(save: &SaveBlock) -> bool {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::new(psx_mc::Slot::One));
    if card.is_formatted() != Ok(true) {
        // Formatting is destructive and belongs behind an explicit System UI
        // confirmation, never inside a POI interaction.
        return false;
    }
    let mut bytes = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
    save.encode(&mut bytes);
    if card
        .write(PROJECT_SAVE_NAME, PROJECT_SAVE_TITLE, &bytes)
        .is_err()
    {
        return false;
    }
    let mut verify = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
    matches!(
        card.read(PROJECT_SAVE_NAME, &mut verify),
        Ok(len)
            if len == verify.len()
                && SaveBlock::decode(&verify, PERSISTENT_FLAG_COUNT) == Some(*save)
    )
}

fn boost_module_for_reward(resource: u16) -> Option<BoostModuleId> {
    let id = BoostModuleId(resource);
    id.index()
        .filter(|index| BOOST_MODULES.get(*index).is_some())
        .map(|_| id)
}

fn poi_persistent_flags(
    interactable: &InteractableRecord,
) -> psx_game_runtime::poi::PoiPersistentFlags {
    psx_game_runtime::poi::PoiPersistentFlags {
        read: interactable.read_flag,
        reward: interactable.reward_flag,
    }
}

fn apply_bsp_debug_body_fallback(
    config: &mut CharacterMotorConfig,
    uses_bsp: bool,
    has_character: bool,
) {
    if uses_bsp && !has_character {
        config.radius = BSP_PLAYER_RADIUS;
        config.height = BSP_PLAYER_HEIGHT;
        config.walk_speed = BSP_FALLBACK_PLAYER_SPEED;
        config.run_speed = BSP_FALLBACK_PLAYER_SPEED;
    }
}

/// Deterministic damage cadence for shared PXBSP hazard contents. Water is
/// non-damaging; slime deals four health twice per second, lava deals ten
/// health four times per second at the 60 Hz simulation rate.
pub(super) const fn bsp_hazard_damage(contents: i16, tick: u32) -> u16 {
    match contents {
        psx_bsp::collision::CONTENTS_SLIME if tick % 30 == 0 => 4,
        psx_bsp::collision::CONTENTS_LAVA if tick % 15 == 0 => 10,
        _ => 0,
    }
}

const _: () = {
    assert!(bsp_hazard_damage(psx_bsp::collision::CONTENTS_WATER, 30) == 0);
    assert!(bsp_hazard_damage(psx_bsp::collision::CONTENTS_SLIME, 29) == 0);
    assert!(bsp_hazard_damage(psx_bsp::collision::CONTENTS_SLIME, 30) == 4);
    assert!(bsp_hazard_damage(psx_bsp::collision::CONTENTS_LAVA, 14) == 0);
    assert!(bsp_hazard_damage(psx_bsp::collision::CONTENTS_LAVA, 15) == 10);
};

/// Crossfade length for a specific transition, rather than one number for
/// every change of state. What reads well depends on BOTH ends: an attack
/// must start crisply but settle slowly, and a gait change can afford a long
/// fade only because the clips are phase-matched.
fn player_blend_ticks(from: PlayerAnim, to: PlayerAnim) -> u32 {
    if player_anim_is_attack(to) || to.is_motor_fixed_action() {
        // Entering a committed action: its first frames carry the read.
        PLAYER_ANIM_BLEND_ACTION_TICKS
    } else if player_anim_is_attack(from) || matches!(from, PlayerAnim::Intro) {
        // Leaving one: a long swing needs somewhere to land.
        PLAYER_ANIM_BLEND_ACTION_OUT_TICKS
    } else if from.is_gait() && to.is_gait() {
        PLAYER_ANIM_BLEND_GAIT_TICKS
    } else {
        PLAYER_ANIM_BLEND_LOCOMOTION_TICKS
    }
}

impl Playtest {
    pub(super) fn ensure_poi_save_loaded(&mut self) {
        if self.poi_save_loaded {
            return;
        }
        match load_poi_save_from_card() {
            PoiSaveLoad::Loaded(saved) => self.poi_save = saved,
            PoiSaveLoad::NewGame => {}
            PoiSaveLoad::Retry => return,
        }
        self.poi_save_loaded = true;
        self.restore_claimed_poi_rewards();
        self.restore_persistent_destructibles();
    }

    pub(super) fn restore_persistent_destructibles(&mut self) {
        for (index, record) in DESTRUCTIBLES.iter().enumerate() {
            if self.poi_save.flag(usize::from(record.persistent_flag)) {
                let _ = self.destructibles.restore_broken(index);
            }
        }
    }

    pub(super) fn mark_destructible_broken(&mut self, index: usize) {
        let Some(record) = DESTRUCTIBLES.get(index) else {
            return;
        };
        let flag = usize::from(record.persistent_flag);
        if !self.poi_save.flag(flag) {
            self.poi_save.set_flag(flag);
            self.poi_save_dirty = true;
        }
    }

    pub(super) fn restore_claimed_poi_rewards(&mut self) {
        self.power_up_loadout = PowerUpLoadout::DEFAULT;
        self.power_up_inventory = BoostInventory::EMPTY;
        for interactable in INTERACTABLES {
            if interactable.kind != InteractableKind::PointOfInterest
                || !poi_persistent_flags(interactable).reward_claimed(&self.poi_save)
            {
                continue;
            }
            if let Some(module) = boost_module_for_reward(interactable.reward_resource) {
                let _ = self.power_up_inventory.add(module);
            }
        }
    }

    /// Flush pending POI state at an intentional save boundary. Memory-card
    /// filesystem writes take many video periods on real hardware, so they
    /// must never run inside a live interaction or an arbitrary gameplay tick.
    pub(super) fn flush_poi_save(&mut self) {
        if !self.poi_save_loaded || !self.poi_save_dirty {
            return;
        }
        self.poi_save_dirty = !save_poi_state_to_card(&self.poi_save);
    }

    pub(super) fn retry_poi_card_load(&mut self, tick: u32) {
        const RETRY_TICKS: u32 = 300;
        if self.poi_save_loaded || tick % RETRY_TICKS != 0 {
            return;
        }
        self.ensure_poi_save_loaded();
    }

    pub(super) fn point_of_interest_available(&self, interactable: &InteractableRecord) -> bool {
        if interactable.kind != InteractableKind::PointOfInterest {
            return interactable_is_active(interactable);
        }
        if !self.poi_save_loaded {
            return false;
        }
        let candidate = psx_game_runtime::poi::PoiCandidate {
            room: interactable.room.0,
            x: interactable.x,
            z: interactable.z,
            radius: interactable.radius,
            enabled: interactable_is_active(interactable),
            repeatable: interactable.flags & psx_level::interactable_flags::REPEATABLE != 0,
            persistence: poi_persistent_flags(interactable),
            reward: psx_game_runtime::poi::PoiReward {
                resource: interactable.reward_resource,
                quantity: interactable.reward_quantity,
            },
        };
        candidate.is_available(&self.poi_save)
    }

    pub(super) fn point_of_interest_depleted(&self, interactable: &InteractableRecord) -> bool {
        if interactable.kind != InteractableKind::PointOfInterest {
            return false;
        }
        let candidate = psx_game_runtime::poi::PoiCandidate {
            room: interactable.room.0,
            x: interactable.x,
            z: interactable.z,
            radius: interactable.radius,
            enabled: interactable_is_active(interactable),
            repeatable: interactable.flags & psx_level::interactable_flags::REPEATABLE != 0,
            persistence: poi_persistent_flags(interactable),
            reward: psx_game_runtime::poi::PoiReward {
                resource: interactable.reward_resource,
                quantity: interactable.reward_quantity,
            },
        };
        candidate.is_depleted(&self.poi_save)
    }

    fn grant_point_of_interest_reward(&mut self, index: usize) -> Option<BoostModuleId> {
        let Some(interactable) = INTERACTABLES.get(index) else {
            return None;
        };
        let persistence = poi_persistent_flags(interactable);
        if persistence.reward_claimed(&self.poi_save) || interactable.reward_quantity == 0 {
            return None;
        }
        let Some(module) = boost_module_for_reward(interactable.reward_resource) else {
            return None;
        };
        let already_owned = self.power_up_inventory.contains(module)
            || BoostSlotId::ALL
                .iter()
                .any(|slot| self.power_up_loadout.module(*slot) == module);
        if already_owned || !self.power_up_inventory.add(module) {
            return None;
        }
        if persistence.mark_reward_claimed(&mut self.poi_save) {
            self.poi_save_dirty = true;
        }
        Some(module)
    }

    pub(super) fn advance_poi_message(&mut self) {
        use psx_game_runtime::poi::{MessageAdvance, MessageSource};
        if !self.acquired_module.is_none() {
            if self.complete_acquired_item_reveal() {
                return;
            }
            self.acquired_module = BoostModuleId::NONE;
            self.poi_panel_frame = 0;
            self.poi_page_type_frame = 0;
            return;
        }
        if self.complete_active_poi_reveal() {
            return;
        }
        match self.poi_messages.advance() {
            MessageAdvance::Advanced(_) => {
                self.poi_page_type_frame = 0;
            }
            MessageAdvance::Closed(MessageSource::PointOfInterest(index)) => {
                if let Some(interactable) = INTERACTABLES.get(usize::from(index)) {
                    if poi_persistent_flags(interactable).mark_read(&mut self.poi_save) {
                        self.poi_save_dirty = true;
                    }
                }
                if let Some(module) = self.grant_point_of_interest_reward(usize::from(index)) {
                    self.acquired_module = module;
                    self.poi_panel_frame = 0;
                    self.poi_page_type_frame = 0;
                }
            }
            MessageAdvance::Closed(MessageSource::World) | MessageAdvance::Inactive => {}
        }
    }

    fn complete_acquired_item_reveal(&mut self) -> bool {
        const PREFIX: &str = "ITEM ACQUIRED - ";
        let Some(module) = self
            .acquired_module
            .index()
            .and_then(|index| BOOST_MODULES.get(index))
        else {
            return false;
        };
        let required_panel = psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES;
        let required_type = u16::try_from(PREFIX.chars().count() + module.name.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_mul(psx_engine::ui::MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
        if self.poi_panel_frame >= required_panel && self.poi_page_type_frame >= required_type {
            return false;
        }
        self.poi_panel_frame = required_panel;
        self.poi_page_type_frame = required_type;
        true
    }

    /// The first Cross press during panel motion or type-on completes the
    /// current presentation instead of skipping unread copy. A later press is
    /// then free to advance/close through `MessageController`.
    fn complete_active_poi_reveal(&mut self) -> bool {
        use psx_game_runtime::poi::MessageSource;

        let Some(message) = self.poi_messages.active() else {
            return false;
        };
        if !matches!(message.source(), MessageSource::PointOfInterest(_)) {
            return false;
        }
        let Some(page_text) = INTERACTABLE_MESSAGE_PAGES.get(message.page() as usize) else {
            return false;
        };
        let required_panel = psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES;
        let required_type = u16::try_from(page_text.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_mul(psx_engine::ui::MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
        if self.poi_panel_frame >= required_panel && self.poi_page_type_frame >= required_type {
            return false;
        }

        self.poi_panel_frame = required_panel;
        self.poi_page_type_frame = required_type;
        true
    }

    /// Advance Archive presentation only when a new frame is actually being
    /// prepared. This preserves visible intermediate geometry when the fixed
    /// simulation gets several ticks ahead of displayed PS1 frames.
    pub(super) fn advance_poi_presentation_frame(&mut self) {
        if !self.acquired_module.is_none() {
            if self.poi_panel_frame < psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
                self.poi_panel_frame = self.poi_panel_frame.saturating_add(1);
            } else {
                self.poi_page_type_frame = self.poi_page_type_frame.saturating_add(1);
            }
            return;
        }
        let Some(message) = self.poi_messages.active() else {
            return;
        };
        if !matches!(
            message.source(),
            psx_game_runtime::poi::MessageSource::PointOfInterest(_)
        ) {
            return;
        }
        if self.poi_panel_frame < psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
            self.poi_panel_frame = self.poi_panel_frame.saturating_add(1);
        } else {
            self.poi_page_type_frame = self.poi_page_type_frame.saturating_add(1);
        }
    }

    pub(super) fn open_world_message_once(&mut self) {
        let Some(message) = WORLD_MESSAGE else {
            return;
        };
        let _ = self.poi_messages.open_world(
            0,
            psx_game_runtime::poi::MessagePageSpan::new(message.page_first, message.page_count),
        );
    }

    /// Fixed-point stat multipliers from the four endpoint sockets at the
    /// current Horizon/Zenith health positions.
    pub(super) fn vitality_modifiers(&self) -> VitalityModifiers {
        self.power_up_loadout
            .modifiers(&self.player_vitality, BOOST_MODULES)
    }

    /// Live authored playback rate for an animation. Only the four attack
    /// actions use the module Attack Speed lane; locomotion and defensive
    /// actions retain their authored cadence.
    pub(super) fn player_action_speed_q8(
        &self,
        character: RuntimeCharacter,
        anim: PlayerAnim,
    ) -> u16 {
        let authored = character.action_speed(anim.action());
        if player_anim_is_attack(anim) {
            self.vitality_modifiers().attack_speed_q8(authored)
        } else {
            authored
        }
    }

    pub(super) fn player_character_for_anim(
        &self,
        mut character: RuntimeCharacter,
        _anim: PlayerAnim,
    ) -> RuntimeCharacter {
        let modifiers = self.vitality_modifiers();
        for anim in [
            PlayerAnim::LightAttack,
            PlayerAnim::HeavyAttack,
            PlayerAnim::VertLightAttack,
            PlayerAnim::VertHeavyAttack,
        ] {
            let action = anim.action();
            let authored = character.action_speed(action);
            character.action_speeds[action.to_index()] = modifiers.attack_speed_q8(authored);
        }
        character
    }

    pub(super) const fn player_attack_channel(anim: PlayerAnim) -> VitalityChannelId {
        match anim {
            PlayerAnim::VertLightAttack | PlayerAnim::VertHeavyAttack => VitalityChannelId::Two,
            _ => VitalityChannelId::One,
        }
    }

    /// Apply a legacy, untyped incoming hit. Horizon is the migration/default
    /// channel; damage beyond its remaining health spills into Zenith. New
    /// coloured attacks can call `DualVitality::apply_damage` directly once
    /// their authored axis is available.
    pub(super) fn apply_untyped_player_damage(&mut self, damage: u16) -> bool {
        let damage = self.vitality_modifiers().incoming_damage(damage);
        self.player_vitality
            .apply_spill(VitalityChannelId::One, damage)
            .actor_defeated
    }

    /// Route an authored coloured projectile into exactly one vitality pool.
    /// Typed attacks deliberately never spill: reading the attack colour is
    /// the player's opportunity to protect the correct half.
    pub(super) fn apply_typed_player_damage(
        &mut self,
        channel: psx_game_runtime::projectiles::ProjectileDamageChannel,
        damage: u16,
    ) -> bool {
        let damage = self.vitality_modifiers().incoming_damage(damage);
        let channel = match channel {
            psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => {
                VitalityChannelId::One
            }
            psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => {
                VitalityChannelId::Two
            }
        };
        self.player_vitality
            .apply_damage(channel, damage)
            .actor_defeated
    }

    pub(super) fn water_cell_at(
        &self,
        room: RoomIndex,
        position: RoomPoint,
    ) -> Option<&'static LevelWaterCellRecord> {
        let sector_size = i32::from(ROOMS.get(room.to_usize())?.sector_size);
        if sector_size <= 0 {
            return None;
        }
        let x = u16::try_from(position.x.div_euclid(sector_size)).ok()?;
        let z = u16::try_from(position.z.div_euclid(sector_size)).ok()?;
        WATER_CELLS
            .binary_search_by_key(&(room, x, z), |cell| (cell.room, cell.x, cell.z))
            .ok()
            .and_then(|index| WATER_CELLS.get(index))
    }

    /// Arm the shared player death sequence: `delay_ticks` of locked
    /// Death animation, then [`Self::respawn_after_death`] when the
    /// countdown in `update_gameplay` reaches zero. One arming point
    /// for every cause (combat damage, BSP liquid hazards, lethal
    /// water); callers floor health themselves. PLAYER_DEATHS counts
    /// here so the death event is recorded even if a run ends before
    /// the respawn completes.
    pub(super) fn arm_player_death(
        &mut self,
        by_combat: bool,
        delay_ticks: u8,
        now: SimTick,
        video_hz: VideoHz,
    ) {
        self.hazard_death_ticks_remaining = delay_ticks.max(1);
        self.death_by_combat = by_combat;
        self.switch_player_anim(PlayerAnim::Death, now, video_hz);
        self.anim_lock_until_tick = now.saturating_add(u32::from(delay_ticks));
        self.lock_target = None;
        self.soft_lock_target = None;
        self.active_interactable = None;
        telemetry::counter(telemetry::counter::PLAYER_DEATHS, 1);
    }

    /// Respawn after ANY completed death sequence (combat or hazard).
    ///
    /// Persistence policy (the souls rule: the world resets, the
    /// checkpoint persists): `self.checkpoint` deliberately survives
    /// death and decides the respawn pose; enemies, logic records
    /// (including fired-once triggers), door states, and box props are
    /// TRANSIENT and re-arm 1:1 from their cooked tables below.
    pub(super) fn respawn_after_death(&mut self) {
        let respawning_at_checkpoint = self.checkpoint.is_some();
        let (room, position, yaw) = if let Some(checkpoint) = self.checkpoint {
            (checkpoint.room, checkpoint.position, checkpoint.yaw)
        } else {
            let spawn = PLAYER_CONTROLLER.map_or(PLAYER_SPAWN, |controller| controller.spawn);
            (
                spawn.room,
                RoomPoint::new(spawn.x, spawn.y, spawn.z),
                Angle::from_q12(spawn.yaw as u16),
            )
        };
        if ROOMS.get(room.to_usize()).is_none() {
            return;
        }
        self.room_index = room;
        let motor_yaw = if !respawning_at_checkpoint && self.character.is_none() {
            yaw.add(Angle::HALF)
        } else {
            yaw
        };
        self.motor.snap_to(position, motor_yaw);
        self.player_vitality.refill();
        self.hazard_death_ticks_remaining = 0;
        self.anim_state = PlayerAnim::Idle;
        self.anim_blend_from = None;
        self.anim_lock_until_tick = SimTick::ZERO;
        self.lock_target = None;
        self.soft_lock_target = None;
        self.active_interactable = None;
        self.evade_run_hold_ticks = 0;
        self.evade_run_hold_consumed = false;
        // New life: the next successful weapon socket resolution counts
        // as this life's PLAYER_WEAPON_ATTACHMENTS event.
        self.weapon_attach_reported = false;
        self.game_entities.spawn_from_records(GAME_ENTITIES);
        self.logic.init_from_records(LOGIC);
        // The logic runtime restarts its rolling fired total with the
        // records; restart the reported watermark with it or the
        // LOGIC_RECORDS_FIRED delta counter under-reports until the new
        // total passes the pre-death one.
        self.logic_fired_reported = 0;
        self.box_props.reset_dynamic_state();
        self.sync_door_box_props();
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            if respawning_at_checkpoint && self.character.is_none() {
                yaw.add(Angle::HALF)
            } else {
                yaw
            },
        );
        self.render_camera = world_camera_from_position_focus(
            PROJECTION,
            self.camera.position(),
            self.camera.focus(),
        );
        #[cfg(not(feature = "cd-stream-bench"))]
        if self.bsp.is_none() {
            self.load_active_room_window();
        }
        telemetry::debug_log(if self.death_by_combat {
            "player combat:respawn"
        } else {
            "player hazard:respawn"
        });
        self.death_by_combat = false;
    }

    /// Switch the player animation state, recording the outgoing
    /// pose so the renderer can crossfade instead of hard-cutting.
    pub(super) fn switch_player_anim(&mut self, anim: PlayerAnim, now: SimTick, video_hz: VideoHz) {
        let old = self.anim_state;
        self.anim_blend_from = Some((old, now.saturating_sub(self.anim_start_tick), now));
        // Gait to gait, enter the incoming cycle at the phase the outgoing one
        // had reached. Cook-time alignment puts foot-down at frame 0 of every
        // gait clip, so equal phase is the same point in the stride and the
        // feet stay in step across the change. Backdating the start tick is
        // how the phase is expressed: the clip reads as having begun earlier.
        let carried = self.gait_phase_carry(old, anim, now, video_hz);
        self.anim_state = anim;
        self.anim_start_tick = SimTick::from_u32(now.as_u32().saturating_sub(carried));
    }

    /// Ticks to backdate the incoming gait clip by so it starts in phase with
    /// the outgoing one. Zero for anything that is not gait-to-gait, and for
    /// any clip whose duration cannot be resolved.
    fn gait_phase_carry(
        &self,
        from: PlayerAnim,
        to: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> u32 {
        if !from.is_gait() || !to.is_gait() || from == to {
            return 0;
        }
        let Some(character) = self.character else {
            return 0;
        };
        let cycle = |anim: PlayerAnim| {
            self.player_clip_duration_vblanks(
                character,
                character.clip_for(anim),
                video_hz,
                self.player_action_speed_q8(character, anim),
                character.action_frame_range(anim.action()),
            )
            .filter(|ticks| *ticks > 0)
        };
        let (Some(out_cycle), Some(in_cycle)) = (cycle(from), cycle(to)) else {
            return 0;
        };
        let local = now.saturating_sub(self.anim_start_tick) % out_cycle;
        // Same fraction of the incoming cycle, in integer math. Both cycles
        // are cooked u32 tick counts, so the product needs the wider
        // accumulator; narrowing would wrap a long clip's phase to a wrong
        // frame. Once per animation transition, never per vertex.
        // psx-numeric-allow-next-line: cross-multiplied phase rescale, see above
        ((u64::from(local) * u64::from(in_cycle)) / u64::from(out_cycle)) as u32
    }

    /// Resolve the active crossfade for this render tick, if any.
    ///
    /// Alpha ramps linearly over the window; attacks use the short
    /// window so combat stays snappy while locomotion soft-blends.
    ///
    /// The outgoing clip KEEPS PLAYING through the window: its local
    /// tick advances with elapsed time rather than staying pinned to
    /// the switch moment. Holding it still made a released walk freeze
    /// mid-stride and slide into idle, because the fade was lerping
    /// toward a static pose instead of one that was still moving.
    /// Non-looping outgoing clips are safe to advance -- the phase
    /// helper clamps them at their last frame.
    pub(super) fn player_anim_blend(&self, now: SimTick) -> Option<PlayerAnimBlend> {
        let (anim, local_tick, switch_tick) = self.anim_blend_from?;
        let duration = player_blend_ticks(anim, self.anim_state);
        let elapsed = now.saturating_sub(switch_tick);
        if elapsed >= duration {
            return None;
        }
        Some(PlayerAnimBlend {
            anim,
            local_tick: local_tick.saturating_add(elapsed),
            alpha_q12: ((elapsed << 12) / duration.max(1)) as u16,
        })
    }

    pub(super) fn start_player_anim_action(
        &mut self,
        anim: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> bool {
        let Some(character) = self.character else {
            return false;
        };
        if !self.lock_player_anim_action(character, anim, now, video_hz) {
            return false;
        }
        self.switch_player_anim(anim, now, video_hz);
        if player_anim_is_attack(anim) {
            // A fresh swing gets a fresh one-hit-per-enemy mask.
            self.swing_hit_mask = 0;
            self.destructibles.begin_swing();
        }
        true
    }

    pub(super) fn lock_player_anim_action(
        &mut self,
        character: RuntimeCharacter,
        anim: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> bool {
        if character.action_clip(anim.action()).is_none() {
            return false;
        }
        let clip = character.clip_for(anim);
        let duration = self
            .player_clip_duration_vblanks(
                character,
                clip,
                video_hz,
                self.player_action_speed_q8(character, anim),
                character.action_frame_range(anim.action()),
            )
            .unwrap_or(24)
            .max(1);
        self.anim_lock_until_tick = now.saturating_add(duration);
        true
    }

    pub(super) fn motor_config(&self) -> CharacterMotorConfig {
        let mut config = match self.character {
            Some(c) => c.motor_config(),
            None => CharacterMotorConfig::character(
                0,
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                FALLBACK_PLAYER_YAW_STEP,
            ),
        }
        .without_stamina_limit();
        if let Some(room) = ROOMS.get(self.room_index.to_usize()) {
            config.gravity_per_tick = room.gravity_per_tick;
        }
        // Characterless brush projects use the cooker's matching debug body.
        // Authored characters keep their Character-bound radius and height so
        // `BspRuntime::update_motor` can select the exact cooked containing
        // hull instead of silently shrinking the player.
        apply_bsp_debug_body_fallback(&mut config, self.bsp.is_some(), self.character.is_some());
        let modifiers = self.vitality_modifiers();
        config.walk_speed = modifiers.movement_speed(config.walk_speed);
        config.run_speed = modifiers.movement_speed(config.run_speed);
        config
    }

    pub(super) fn camera_orbit_speed_level(&self) -> u8 {
        ROOMS
            .get(self.room_index.to_usize())
            .map(|room| room.camera.orbit_speed_level)
            .unwrap_or(LevelCameraRecord::DEFAULT.orbit_speed_level)
    }

    pub(super) fn collect_collision_blockers_into<
        S: psx_engine::BoundedSink<CharacterCollisionCylinder>,
    >(
        &self,
        out: &mut S,
    ) -> usize {
        let mut count = 0usize;
        for (index, inst) in MODEL_INSTANCES.iter().enumerate() {
            if inst.room != self.room_index {
                continue;
            }
            let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                continue;
            };
            let height = (model.world_height as i32).max(1);
            let radius = i32::from(model.collision_radius).max(1);
            if radius <= 0 {
                continue;
            }
            // An instance bound to a game entity blocks at the
            // entity's LIVE position (phase 3): the player collides
            // with the enemy where it stands, not its spawn point.
            // Dead entities stop blocking (souls corpses are not
            // walls).
            let inst_index = index.min(u16::MAX as usize) as u16;
            let center = match game_entity_for_instance(inst_index) {
                Some(entity) => {
                    if self.game_entities.state(entity)
                        == psx_game_runtime::entities::GameEntityState::Dead
                    {
                        continue;
                    }
                    let live = self.game_entities.position(entity);
                    RoomPoint::new(live[0], live[1], live[2])
                }
                None => RoomPoint::new(inst.x, inst.y, inst.z),
            };
            if !out.try_push(CharacterCollisionCylinder::new(center, radius, height)) {
                return count;
            }
            count += 1;
        }
        count
            + psx_game_runtime::cylinder_props::collect_cylinder_prop_collision_blockers_into(
                CYLINDER_PROPS,
                self.room_index,
                out,
            )
    }

    pub(super) fn collect_collision_rooms(
        &self,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
    ) -> usize {
        let mut count = 0usize;
        let mut collected_rooms = [INVALID_ROOM_INDEX; MAX_COLLISION_ROOMS];
        let current_authored = authored_room_for_chunk(self.room_index);
        for active in self.window.rooms.iter().flatten() {
            if count >= out.len() {
                break;
            }
            if current_authored.is_some()
                && authored_room_for_chunk(active.index) != current_authored
            {
                continue;
            }
            if !active_room_overlaps_collision_window(*active, anchor, margin) {
                continue;
            }
            out[count] = CharacterCollisionRoom::from_collision(
                active.collision_room,
                active.offset_x,
                active.offset_z,
            )
            .with_offset_y(active.offset_y);
            collected_rooms[count] = active.index;
            count += 1;
        }
        count = self.collect_current_portal_collision_rooms(
            current_authored,
            anchor,
            margin,
            out,
            &mut collected_rooms,
            count,
        );
        #[cfg(feature = "cd-stream-bench")]
        {
            count = self.collect_resident_streamed_collision_rooms(
                current_authored,
                anchor,
                margin,
                out,
                &mut collected_rooms,
                count,
            );
        }
        count
    }

    pub(super) fn collect_current_portal_collision_rooms(
        &self,
        current_authored: Option<u32>,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
        collected_rooms: &mut [RoomIndex; MAX_COLLISION_ROOMS],
        mut count: usize,
    ) -> usize {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return count;
        };
        let portal_first = current_record.portal_first as usize;
        let portal_end = portal_first.saturating_add(current_record.portal_count as usize);
        let mut portal_index = portal_first;
        while portal_index < portal_end.min(ROOM_PORTALS.len()) && count < out.len() {
            let portal = ROOM_PORTALS[portal_index];
            portal_index += 1;
            if portal.source_room != self.room_index {
                continue;
            }
            let index = portal.destination_room;
            if collision_room_collected(collected_rooms, count, index) {
                continue;
            }
            if current_authored.is_some() && authored_room_for_chunk(index) != current_authored {
                continue;
            }
            let Some(chunk) = chunk_record_for_room(index) else {
                continue;
            };
            let Some(record) = ROOMS.get(index.to_usize()) else {
                continue;
            };
            if !chunk_overlaps_collision_window(*chunk, current_record, record, anchor, margin) {
                continue;
            }
            let Some(room) = parse_collision_room_for_index(index, record) else {
                continue;
            };
            out[count] = CharacterCollisionRoom::from_collision(
                room,
                room_origin_x(record).saturating_sub(room_origin_x(current_record)),
                room_origin_z(record).saturating_sub(room_origin_z(current_record)),
            )
            .with_offset_y(record.origin_y.saturating_sub(current_record.origin_y));
            collected_rooms[count] = index;
            count += 1;
        }
        count
    }

    #[cfg(feature = "cd-stream-bench")]
    pub(super) fn collect_resident_streamed_collision_rooms(
        &self,
        current_authored: Option<u32>,
        anchor: RoomPoint,
        margin: i32,
        out: &mut [CharacterCollisionRoom<'static>],
        collected_rooms: &mut [RoomIndex; MAX_COLLISION_ROOMS],
        mut count: usize,
    ) -> usize {
        let Some(current_record) = ROOMS.get(self.room_index.to_usize()) else {
            return count;
        };
        for chunk in ROOM_CHUNKS {
            if count >= out.len() {
                break;
            }
            if collision_room_collected(collected_rooms, count, chunk.room) {
                continue;
            }
            if current_authored.is_some() && Some(chunk.authored_room) != current_authored {
                continue;
            }
            if !streamed_room_is_resident(chunk.room) {
                continue;
            }
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            if !chunk_overlaps_collision_window(*chunk, current_record, record, anchor, margin) {
                continue;
            }
            let Some(room) = parse_streamed_compact_collision_room(0, chunk.room) else {
                continue;
            };
            out[count] = CharacterCollisionRoom::from_collision(
                RuntimeCollisionRoom::Compact(room),
                room_origin_x(record).saturating_sub(room_origin_x(current_record)),
                room_origin_z(record).saturating_sub(room_origin_z(current_record)),
            )
            .with_offset_y(record.origin_y.saturating_sub(current_record.origin_y));
            collected_rooms[count] = chunk.room;
            count += 1;
        }
        count
    }

    pub(super) fn draw_collision_debug_overlay(&self, camera: WorldCamera) {
        if let Some(character) = self.character {
            draw_collision_cylinder_debug(
                self.motor.position(),
                character.radius,
                character.height,
                camera,
                (0x40, 0xd8, 0xff),
            );
        }

        if self.bsp.is_some() {
            for inst in MODEL_INSTANCES {
                if inst.room != self.room_index {
                    continue;
                }
                let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                    continue;
                };
                draw_collision_cylinder_debug(
                    RoomPoint::new(inst.x, inst.y, inst.z),
                    i32::from(model.collision_radius),
                    i32::from(model.world_height),
                    camera,
                    (0xff, 0xd0, 0x40),
                );
            }
        }
        for active in self.window.rooms.iter().flatten().copied() {
            let room_camera = camera_for_room(camera, active);
            for inst in MODEL_INSTANCES {
                if inst.room != active.index {
                    continue;
                }
                let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                    continue;
                };
                draw_collision_cylinder_debug(
                    RoomPoint::new(inst.x, inst.y, inst.z),
                    i32::from(model.collision_radius),
                    i32::from(model.world_height),
                    room_camera,
                    (0xff, 0xd0, 0x40),
                );
            }
        }
    }

    pub(super) fn draw_particle_emitters(
        &self,
        camera: WorldCamera,
        elapsed_tick: SimTick,
        ot: &mut OtFrame<'_, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
    ) -> usize {
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let mut submitted = 0usize;
        if self.bsp.is_some() {
            let depth_range = ROOMS
                .get(self.room_index.to_usize())
                .map(room_depth_range)
                .unwrap_or(WORLD_DEPTH_RANGE);
            let mut projector = None;
            for emitter in PARTICLE_EMITTERS {
                if emitter.room != self.room_index {
                    continue;
                }
                let loaded_projector = match projector {
                    Some(projector) => Some(projector),
                    None => {
                        if !PROP_PARTICLE_GTE_PROJECT_ENABLED {
                            None
                        } else {
                            let loaded = LoadedWorldCameraGte::load(camera);
                            projector = Some(loaded);
                            Some(loaded)
                        }
                    }
                };
                submitted += draw_particle_emitter(
                    *emitter,
                    camera,
                    loaded_projector,
                    depth_range,
                    particle_material,
                    elapsed_tick,
                    ot,
                    primitive_packets,
                );
            }
            return submitted;
        }
        for active in self.window.rooms.iter().flatten().copied() {
            if !self.portal_visibility_draws_room(active.index) {
                continue;
            }
            let room_camera = camera_for_room(camera, active);
            let depth_range = ROOMS
                .get(active.index.to_usize())
                .map(room_depth_range)
                .unwrap_or(WORLD_DEPTH_RANGE);
            let mut projector = None;
            for emitter in PARTICLE_EMITTERS {
                if emitter.room != active.index {
                    continue;
                }
                let projector = match projector {
                    Some(projector) => Some(projector),
                    None => {
                        if !PROP_PARTICLE_GTE_PROJECT_ENABLED {
                            None
                        } else {
                            let loaded = LoadedWorldCameraGte::load(room_camera);
                            projector = Some(loaded);
                            Some(loaded)
                        }
                    }
                };
                submitted += draw_particle_emitter(
                    *emitter,
                    room_camera,
                    projector,
                    depth_range,
                    particle_material,
                    elapsed_tick,
                    ot,
                    primitive_packets,
                );
            }
        }
        submitted
    }

    /// Draw live combat bolts after world submission so their additive quads
    /// participate in the same depth table as authored particle effects.
    pub(super) fn draw_combat_projectiles(
        &self,
        camera: WorldCamera,
        ot: &mut OtFrame<'_, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
    ) -> usize {
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let mut submitted = 0usize;
        // Charge flares are sampled from the same retained pose token as the
        // eventual release, so the animated muzzle and presentation cannot
        // drift apart even when NPC simulation runs below source clip rate.
        let mut attack_index = 0usize;
        while attack_index < self.deferred_enemy_attacks.len() {
            let Some(attack) = self.deferred_enemy_attacks.get(attack_index) else {
                break;
            };
            attack_index += 1;
            let Some(entity) = GAME_ENTITIES.get(attack.entity()) else {
                continue;
            };
            if entity.flags & psx_level::game_entity_flags::RANGED_ATTACK == 0 {
                continue;
            }
            let first = entity.combat_capsule_first.to_usize();
            let end = first.saturating_add(usize::from(entity.combat_capsule_count));
            let capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
            let pose = self
                .instance_actor_poses
                .get(entity.model_instance as usize)
                .copied()
                .flatten()
                .map(|snapshot| snapshot.pose());
            let Some(charge) = psx_game_runtime::combat::authored_projectile_charge(
                capsules,
                CharacterAnimationAction::LightAttack,
                pose,
            ) else {
                continue;
            };
            if attack.room() != self.room_index && self.bsp.is_some() {
                continue;
            }
            let room_camera = if self.bsp.is_some() {
                camera
            } else {
                let Some(active) = self
                    .window
                    .rooms
                    .iter()
                    .flatten()
                    .copied()
                    .find(|active| active.index == attack.room())
                else {
                    continue;
                };
                if !self.portal_visibility_draws_room(attack.room()) {
                    continue;
                }
                camera_for_room(camera, active)
            };
            let depth_range = ROOMS
                .get(attack.room().to_usize())
                .map(room_depth_range)
                .unwrap_or(WORLD_DEPTH_RANGE);
            submitted += draw_projectile_charge(
                charge,
                room_camera,
                None,
                depth_range,
                particle_material,
                ot,
                primitive_packets,
            );
        }
        let mut index = 0usize;
        while index < MAX_COMBAT_PROJECTILES {
            let Some(projectile) = self.combat_projectiles.get(index) else {
                index += 1;
                continue;
            };
            let (room_camera, depth_range) = if self.bsp.is_some() {
                if projectile.room != self.room_index {
                    index += 1;
                    continue;
                }
                (
                    camera,
                    ROOMS
                        .get(projectile.room.to_usize())
                        .map(room_depth_range)
                        .unwrap_or(WORLD_DEPTH_RANGE),
                )
            } else {
                let Some(active) = self
                    .window
                    .rooms
                    .iter()
                    .flatten()
                    .copied()
                    .find(|active| active.index == projectile.room)
                else {
                    index += 1;
                    continue;
                };
                if !self.portal_visibility_draws_room(projectile.room) {
                    index += 1;
                    continue;
                }
                (
                    camera_for_room(camera, active),
                    ROOMS
                        .get(projectile.room.to_usize())
                        .map(room_depth_range)
                        .unwrap_or(WORLD_DEPTH_RANGE),
                )
            };
            submitted += draw_projectile_bolt(
                projectile,
                room_camera,
                None,
                depth_range,
                particle_material,
                ot,
                primitive_packets,
            );
            index += 1;
        }
        let mut impact_index = 0usize;
        while impact_index < MAX_PROJECTILE_IMPACTS {
            let Some(impact) = self.combat_projectile_impacts.get(impact_index) else {
                impact_index += 1;
                continue;
            };
            let (room_camera, depth_range) = if self.bsp.is_some() {
                if impact.room != self.room_index {
                    impact_index += 1;
                    continue;
                }
                (
                    camera,
                    ROOMS
                        .get(impact.room.to_usize())
                        .map(room_depth_range)
                        .unwrap_or(WORLD_DEPTH_RANGE),
                )
            } else {
                let Some(active) = self
                    .window
                    .rooms
                    .iter()
                    .flatten()
                    .copied()
                    .find(|active| active.index == impact.room)
                else {
                    impact_index += 1;
                    continue;
                };
                if !self.portal_visibility_draws_room(impact.room) {
                    impact_index += 1;
                    continue;
                }
                (
                    camera_for_room(camera, active),
                    ROOMS
                        .get(impact.room.to_usize())
                        .map(room_depth_range)
                        .unwrap_or(WORLD_DEPTH_RANGE),
                )
            };
            submitted += draw_projectile_impact(
                impact,
                room_camera,
                None,
                depth_range,
                particle_material,
                ot,
                primitive_packets,
            );
            impact_index += 1;
        }
        submitted
    }

    /// Draw the player's lightweight water-foot splash when actually moving
    /// through non-lethal water. The effect is capped at three sprite packets
    /// and derives its phase from time, so it adds no persistent particle state.
    pub(super) fn draw_player_water_wade_splash(
        &self,
        camera: WorldCamera,
        elapsed_tick: SimTick,
        ot: &mut OtFrame<'_, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
    ) -> usize {
        if !self.player_moved_last_tick || self.hazard_death_ticks_remaining > 0 {
            return 0;
        }
        let player = self.motor.position();
        let Some(water) = self.water_cell_at(self.room_index, player) else {
            return 0;
        };
        if player.y >= water.surface_y || water.depth >= water.lethal_depth {
            return 0;
        }
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let depth_range = ROOMS
            .get(self.room_index.to_usize())
            .map(room_depth_range)
            .unwrap_or(WORLD_DEPTH_RANGE);
        let projector =
            PROP_PARTICLE_GTE_PROJECT_ENABLED.then(|| LoadedWorldCameraGte::load(camera));
        draw_water_wade_splash(
            player.x,
            water.surface_y,
            player.z,
            camera,
            projector,
            depth_range,
            particle_material,
            elapsed_tick,
            ot,
            primitive_packets,
        )
    }

    /// Gameplay-anchored animation tick: raw sim ticks minus the epoch
    /// captured at the first gameplay update. Value-based animation
    /// phases (ambient models, particles, atmosphere, HUD pulse) use
    /// this so visuals are a pure function of gameplay time instead of
    /// inheriting the build- and disc-dependent loading duration.
    pub(super) fn gameplay_tick(&self, now: SimTick) -> SimTick {
        SimTick::from_u32(now.as_u32().wrapping_sub(self.gameplay_epoch.as_u32()))
    }

    pub(super) fn camera_config(&self) -> ThirdPersonCameraConfig {
        if self.bsp.is_some() && self.character.is_none() {
            let mut config = ThirdPersonCameraConfig::character(
                BSP_FALLBACK_CAMERA_DISTANCE,
                BSP_FALLBACK_CAMERA_HEIGHT,
                BSP_FALLBACK_CAMERA_TARGET_HEIGHT,
            );
            config.min_floor_clearance = BSP_FALLBACK_CAMERA_CLEARANCE;
            config.collision_margin = BSP_FALLBACK_CAMERA_MARGIN;
            // The debug body has no visible turn animation to protect. Follow
            // its cardinal route promptly so a 90-degree doorway turn shows
            // the next room instead of holding a side wall in frame.
            config.auto_align_when_moving = true;
            config.auto_align_step = Angle::from_q12(64);
            config.collision_solve_interval = CAMERA_COLLISION_SOLVE_INTERVAL;
            return config;
        }
        let camera = ROOMS
            .get(self.room_index.to_usize())
            .map(|room| room.camera)
            .unwrap_or(LevelCameraRecord::DEFAULT);
        let mut config = ThirdPersonCameraConfig::character(
            camera.distance,
            camera.height,
            camera.target_height,
        );
        config.lock_height_boost = camera
            .height
            .saturating_mul(i32::from(camera.lock_rise_percent))
            / 100;
        config.height = config.height.max(16);
        config.min_floor_clearance = camera.min_floor_clearance;
        if self.bsp.is_some() {
            // Keep a small authored gap between the point-traced camera and
            // brush walls. The renderer clips any remaining near-plane
            // intersection exactly.
            config.collision_margin = BSP_CAMERA_WALL_MARGIN;
        }
        config.position_lag_shift = camera.position_lag_shift;
        config.focus_lag_shift = camera.focus_lag_shift;
        config.distance_lag_shift = camera.distance_lag_shift;
        config.collision_solve_interval = CAMERA_COLLISION_SOLVE_INTERVAL;
        config
    }

    pub(super) fn camera_target(
        &self,
        lock_target: Option<RoomPoint>,
        moving: bool,
    ) -> ThirdPersonCameraTarget {
        ThirdPersonCameraTarget {
            player: self.motor.position(),
            player_yaw: self.motor.yaw(),
            moving,
            lock_target,
        }
    }

    pub(super) fn current_room_lighting(&self, camera: WorldCamera) -> Option<RuntimeRoomLighting> {
        if self.bsp.is_none() {
            self.current_collision_room?;
        }
        let room_record = ROOMS.get(self.room_index.to_usize())?;
        Some(RuntimeRoomLighting {
            room_index: self.room_index,
            ambient: Rgb8::from_array(self.current_ambient_rgb),
            camera,
            fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
            fog_rgb: Rgb8::from_array(room_record.fog_rgb),
            fog_near: room_record.fog_near,
            fog_far: room_record.fog_far,
            lights: room_light_slice(LIGHTS, self.room_index),
        })
    }

    pub(super) fn free_orbit_camera(&self) -> WorldCamera {
        WorldCamera::orbit_yaw(
            PROJECTION,
            self.spawn,
            CAMERA_Y_OFFSET,
            self.orbit_radius,
            self.orbit_yaw,
        )
    }

    pub(super) fn update_camera_sweep(&mut self, delta_vblanks: u16) {
        self.orbit_radius = CAMERA_SWEEP_RADIUS.clamp(CAMERA_RADIUS_MIN, CAMERA_RADIUS_MAX);
        self.orbit_yaw = self.orbit_yaw.add_signed_q12(scale_i16_by_vblanks(
            CAMERA_SWEEP_YAW_STEP_Q12,
            delta_vblanks,
        ));
        self.player_moved_last_tick = false;
        self.camera_turning_last_tick = true;
        telemetry::stage_begin(telemetry::stage::CAMERA);
        self.render_camera = self.free_orbit_camera();
        telemetry::stage_end(telemetry::stage::CAMERA);
        if CAMERA_SWEEP_FORCE_VISIBILITY {
            self.force_refresh_active_room_window_view();
        } else {
            self.refresh_active_room_window_if_needed();
        }
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        self.prewarm_visible_cell_caches();
    }

    pub(super) fn update_follow_camera(&mut self, ctx: &Ctx) -> WorldCamera {
        let input = if self.is_locked() {
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                // Shoulder inputs remain exclusively owned by combat even
                // while hard locked.
                recenter: false,
            }
        } else {
            camera_input(ctx, self.camera_orbit_speed_level(), self.analog_deadzone)
        };
        let lock_target = self
            .lock_target_position()
            .or_else(|| self.soft_lock_target_position());
        let target = self.camera_target(lock_target, self.anim_state != PlayerAnim::Idle);
        let config = self.camera_config();
        let mut prop_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self.bsp.is_some()
            && self
                .collect_static_prop_aabb_blockers_checked_into(&mut prop_blockers)
                .is_none()
        {
            // Invalid generated prop collision freezes the existing camera
            // instead of treating the obstructed boom as clear.
            return world_camera_from_position_focus(
                PROJECTION,
                self.camera.position(),
                self.camera.focus(),
            );
        }
        if let Some(bsp) = self.bsp.as_mut() {
            return bsp
                .update_camera(
                    &mut self.camera,
                    target,
                    input,
                    config,
                    1,
                    prop_blockers.as_slice(),
                    &self.destructibles,
                )
                .expect("PXBSP camera trace failed")
                .camera;
        }
        if CAMERA_COLLISION_ENABLED && self.chunked_level() {
            // The camera's blocking-room set changes only when the player
            // crosses a coarse cell or the active window changes, so the
            // per-tick gather (about half of the camera's measured 50k
            // tick cost) is cached. The gather margin grows by the cache
            // quantum, keeping the set a superset of "rooms within camera
            // reach" anywhere inside the key cell -- the solve result is
            // identical because out-of-reach rooms cannot block the sweep.
            // The full-width streaming generation is part of the key because the
            // cached CharacterCollisionRooms hold parses of streamed slot
            // bytes: the active mask lags the pin set while the window job
            // catches up, so residency turnover must force a re-gather
            // even before the active mask changes (streaming audit,
            // 'static slice contract).
            const CAMERA_ROOM_CACHE_QUANTUM: i32 = 512;
            #[cfg(feature = "cd-stream-bench")]
            let resident_generation = room_streams_arena().residency_generation();
            #[cfg(not(feature = "cd-stream-bench"))]
            let resident_generation = 0u32;
            let key = (
                self.room_index,
                target.player.x.div_euclid(CAMERA_ROOM_CACHE_QUANTUM),
                target.player.z.div_euclid(CAMERA_ROOM_CACHE_QUANTUM),
                self.window.generation(),
                resident_generation,
            );
            if key != self.camera_rooms_key {
                let mut collision_rooms =
                    [const { CharacterCollisionRoom::EMPTY }; MAX_COLLISION_ROOMS];
                let margin = config
                    .distance
                    .saturating_add(config.collision_margin)
                    .max(config.min_distance)
                    .saturating_add(CAMERA_ROOM_CACHE_QUANTUM);
                let count =
                    self.collect_collision_rooms(target.player, margin, &mut collision_rooms);
                self.camera_collision_rooms = collision_rooms;
                self.camera_collision_room_count = count;
                self.camera_rooms_key = key;
            }
            return self
                .camera
                .update_vblanks_with_collision_rooms(
                    PROJECTION,
                    &self.camera_collision_rooms[..self.camera_collision_room_count],
                    target,
                    input,
                    config,
                    1u16,
                )
                .camera;
        }
        let collision = if CAMERA_COLLISION_ENABLED {
            self.current_collision_room
                .as_ref()
                .map(|room| room.collision())
        } else {
            None
        };
        self.camera
            .update_vblanks(PROJECTION, collision, target, input, config, 1u16)
            .camera
    }

    pub(super) fn lock_target_position(&self) -> Option<RoomPoint> {
        self.lock_target
            .and_then(|index| self.target_position(index))
    }

    /// True while facing is bound to a live combat target.
    pub(super) fn is_locked(&self) -> bool {
        self.lock_target.is_some()
    }

    pub(super) fn soft_lock_target_position(&self) -> Option<RoomPoint> {
        self.target_position(self.soft_lock_target?)
    }

    pub(super) fn target_position(&self, index: usize) -> Option<RoomPoint> {
        let target = MODEL_INSTANCES.get(index)?;
        if target.room != self.room_index {
            return None;
        }
        let instance = u16::try_from(index).ok()?;
        // Hard/soft lock is combat targeting, not a generic model picker.
        // Requiring a live gameplay entity prevents scenery and dead actors
        // from winning the screen-space acquisition score.
        game_entity_for_instance(instance)?;
        let live = self
            .game_entities
            .live_position_for_model_instance(GAME_ENTITIES, instance)?;
        Some(RoomPoint::new(live[0], live[1], live[2]))
    }

    pub(super) fn refresh_active_interactable(&mut self) {
        self.active_interactable = self.find_best_interactable();
    }

    pub(super) fn find_best_interactable(&mut self) -> Option<usize> {
        let player = self.motor.position();
        let mut best = None;
        let mut best_distance = u64::MAX;
        for (index, interactable) in INTERACTABLES.iter().enumerate() {
            if !self.point_of_interest_available(interactable)
                || interactable.room != self.room_index
            {
                continue;
            }
            let Some(distance) = psx_game_runtime::poi::xz_distance_squared_within_radius(
                [player.x, player.z],
                [interactable.x, interactable.z],
                interactable.radius,
            ) else {
                continue;
            };
            if let Some(bsp) = self.bsp.as_mut() {
                if !bsp.typed_world_object_directly_visible(
                    player,
                    psx_level::world_object_kind::POINT_OF_INTEREST_BEACON,
                    index,
                    &self.destructibles,
                ) {
                    continue;
                }
            }
            if distance < best_distance {
                best = Some(index);
                best_distance = distance;
            }
        }
        best
    }

    /// Interact-prompt activation, migrated onto the LOGIC event
    /// graph (phase 3): the cook pairs every interactable with a
    /// logic record 1:1, so the prompt fires that record and the
    /// terminal effect (message overlay / checkpoint) runs through
    /// `dispatch_logic_effects` -- one dispatch path whether the
    /// record fired from a prompt, a trigger volume, or a relay
    /// chain. A record the runtime refuses (removed by a killtarget,
    /// gated by an unsatisfied master, waiting out its re-arm)
    /// refuses the interaction. The legacy direct path remains only
    /// for hand-rolled manifests whose interactables carry no paired
    /// record.
    pub(super) fn activate_interactable(&mut self, index: usize, now: u32) -> bool {
        let Some(interactable) = INTERACTABLES.get(index) else {
            return false;
        };
        if !self.point_of_interest_available(interactable) {
            return false;
        }
        if interactable.logic != psx_level::INTERACTABLE_LOGIC_NONE {
            let fired = self.logic.fire_index(
                LOGIC,
                usize::from(interactable.logic),
                psx_game_runtime::logic::use_type::TOGGLE,
                now,
            );
            if fired {
                self.dispatch_logic_effects();
            }
            return fired;
        }
        match interactable.kind {
            InteractableKind::Message => {
                self.open_interactable_message(interactable);
                true
            }
            InteractableKind::Checkpoint => {
                self.set_checkpoint(RuntimeCheckpoint {
                    room: self.room_index,
                    position: self.motor.position(),
                    yaw: self.motor.yaw(),
                    checkpoint_id: interactable.checkpoint_id,
                });
                self.open_interactable_message(interactable);
                true
            }
            InteractableKind::PointOfInterest => {
                let Some(message) = INTERACTABLE_MESSAGES.get(interactable.message as usize) else {
                    return false;
                };
                let opened = self.poi_messages.open_poi(
                    index.min(u16::MAX as usize) as u16,
                    psx_game_runtime::poi::MessagePageSpan::new(
                        message.page_first,
                        message.page_count,
                    ),
                );
                if opened {
                    self.poi_panel_frame = 0;
                    self.poi_page_type_frame = 0;
                }
                opened
            }
        }
    }

    /// The single checkpoint assignment point for BOTH activation paths
    /// (the logic-graph CHECKPOINT dispatch and the legacy direct
    /// interactable path are mutually exclusive per activation, so an
    /// activation can never double-count). Only an assignment that
    /// CHANGES the held value counts: re-activating the same checkpoint
    /// from the same pose is a no-op for the counter, while the first
    /// activation of a life (the checkpoint itself persists across
    /// respawn) and any pose/record change count once.
    pub(super) fn set_checkpoint(&mut self, checkpoint: RuntimeCheckpoint) {
        if self.checkpoint != Some(checkpoint) {
            telemetry::counter(telemetry::counter::PLAYER_CHECKPOINT_ACTIVATIONS, 1);
        }
        self.checkpoint = Some(checkpoint);
        // Checkpoints are the genre-visible save boundary. POI interaction
        // stays smooth; its read/reward state reaches the card here instead.
        self.flush_poi_save();
    }

    pub(super) fn open_interactable_message(&mut self, interactable: &InteractableRecord) {
        if self.poi_messages.active().is_some() {
            return;
        }
        let (title, body) = interactable_message_text(interactable);
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
    }

    pub(super) fn lock_target_indicator_position(&self) -> Option<RoomPoint> {
        self.lock_target
            .and_then(|index| self.target_indicator_position(index))
    }

    pub(super) fn target_indicator_position(&self, index: usize) -> Option<RoomPoint> {
        let target = MODEL_INSTANCES.get(index)?;
        let position = self.target_position(index)?;
        let height = MODELS
            .get(target.model.to_usize())
            .map(|model| model.world_height as i32)
            .unwrap_or(1024);
        Some(RoomPoint::new(
            position.x,
            position.y.saturating_add(height >> 1),
            position.z,
        ))
    }

    pub(super) fn lock_target_valid(&self, range: i32) -> bool {
        self.lock_target
            .is_some_and(|index| self.target_index_valid(index, range))
    }

    pub(super) fn target_index_valid(&self, index: usize, range: i32) -> bool {
        let Some(target) = self.target_position(index) else {
            return false;
        };
        distance_xz_sq(self.motor.position(), target) <= square_i32_saturating(range)
    }

    pub(super) fn find_best_lock_target(&self, range: i32) -> Option<usize> {
        let player = self.motor.position();
        let view_yaw = self.camera.yaw().add(Angle::HALF);
        let range_sq = square_i32_saturating(range);
        let mut best: Option<(usize, i32)> = None;
        for (index, _) in MODEL_INSTANCES.iter().enumerate() {
            // Use the same cooked BSP visibility set that gates instance
            // rendering. A longer lock range must not make actors behind a
            // wall targetable merely because they share the active room.
            if index >= u16::BITS as usize || self.bsp_instance_visible_mask & (1u16 << index) == 0
            {
                continue;
            }
            let Some(point) = self.target_position(index) else {
                continue;
            };
            let dx = point.x.saturating_sub(player.x);
            let dz = point.z.saturating_sub(player.z);
            let dist_sq = square_i32_saturating(dx).saturating_add(square_i32_saturating(dz));
            if dist_sq == 0 || dist_sq > range_sq {
                continue;
            }
            let Some((screen_x_q8, forward)) = horizontal_view_coordinates(player, point, view_yaw)
            else {
                continue;
            };
            if abs_i32(screen_x_q8) > LOCK_ACQUIRE_HALF_CONE_Q8 {
                continue;
            }
            // Centre bias dominates, then forward depth and distance break
            // ties. This makes R3 select what the player is looking at rather
            // than a merely-near actor at the edge of the screen.
            let score = forward
                .saturating_mul(16)
                .saturating_sub(abs_i32(screen_x_q8).saturating_mul(96))
                .saturating_sub(dist_sq >> 10);
            match best {
                Some((_, best_score)) if best_score >= score => {}
                _ => best = Some((index, score)),
            }
        }
        best.map(|(index, _)| index)
    }

    pub(super) fn update_soft_lock(&mut self, ctx: &Ctx) {
        if self.is_locked() {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = false;
            return;
        }
        let (right_x, _) = ctx.pad.sticks.right_centered();
        if abs_i16(right_x) >= CAMERA_SOFT_LOCK_BREAK_STICK {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = true;
            return;
        }
        if self.soft_lock_suppressed {
            if self.find_best_lock_target(SOFT_LOCK_BREAK_RANGE).is_none() {
                self.soft_lock_suppressed = false;
            }
            return;
        }
        match self.soft_lock_target {
            Some(index) if self.target_index_valid(index, SOFT_LOCK_BREAK_RANGE) => {}
            _ => self.soft_lock_target = self.find_best_lock_target(SOFT_LOCK_RANGE),
        }
    }

    pub(super) fn update_lock_target_switch(&mut self, ctx: &Ctx) {
        let (right_x, _) = ctx.pad.sticks.right_centered();
        let magnitude = abs_i16(right_x);
        if magnitude <= LOCK_SWITCH_STICK_RELEASE {
            self.lock_switch_stick_held = false;
            return;
        }
        if magnitude < LOCK_SWITCH_STICK_THRESHOLD || self.lock_switch_stick_held {
            return;
        }

        self.switch_lock_target(if right_x > 0 { 1 } else { -1 });
        self.lock_switch_stick_held = true;
    }

    pub(super) fn switch_lock_target(&mut self, direction: i32) {
        let Some(current_index) = self.lock_target else {
            return;
        };
        let Some(current) = self.target_position(current_index) else {
            self.lock_target = None;
            return;
        };
        let player = self.motor.position();
        let view_yaw = self.camera.yaw().add(Angle::HALF);
        let Some((current_screen_x, _)) = horizontal_view_coordinates(player, current, view_yaw)
        else {
            return;
        };
        let range_sq = square_i32_saturating(LOCK_RANGE);
        let mut best: Option<(usize, i32)> = None;
        for (index, _) in MODEL_INSTANCES.iter().enumerate() {
            if index == current_index {
                continue;
            }
            if index >= u16::BITS as usize || self.bsp_instance_visible_mask & (1u16 << index) == 0
            {
                continue;
            }
            let Some(target) = self.target_position(index) else {
                continue;
            };
            let dx = target.x.saturating_sub(player.x);
            let dz = target.z.saturating_sub(player.z);
            let dist_sq = square_i32_saturating(dx).saturating_add(square_i32_saturating(dz));
            if dist_sq == 0 || dist_sq > range_sq {
                continue;
            }
            let Some((candidate_screen_x, forward)) =
                horizontal_view_coordinates(player, target, view_yaw)
            else {
                continue;
            };
            let screen_delta = candidate_screen_x.saturating_sub(current_screen_x);
            if direction > 0 && screen_delta <= 0 || direction < 0 && screen_delta >= 0 {
                continue;
            }
            // Select the next target in screen order, with a small depth and
            // distance penalty so a near neighbour wins over a far backdrop.
            let score = abs_i32(screen_delta)
                .saturating_mul(256)
                .saturating_add(abs_i32(candidate_screen_x))
                .saturating_add(dist_sq >> 12)
                .saturating_sub(forward >> 4);
            match best {
                Some((_, best_score)) if best_score <= score => {}
                _ => best = Some((index, score)),
            }
        }
        if let Some((index, _)) = best {
            self.lock_target = Some(index);
            self.lock_invalid_ticks = 0;
        }
    }
}
