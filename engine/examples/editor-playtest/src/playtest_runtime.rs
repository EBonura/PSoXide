use super::*;

enum PoiSaveLoad {
    Loaded(SaveBlock),
    NewGame,
    Retry,
}

fn load_poi_save_from_card(port: &mut psx_engine::ControllerPort) -> PoiSaveLoad {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(port, psx_mc::Slot::One));
    let mut bytes = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
    let len = match card.read(PROJECT_SAVE_NAME, &mut bytes) {
        Ok(len) => len,
        Err(psx_mc::Error::NotFound) => return PoiSaveLoad::NewGame,
        Err(_) => return PoiSaveLoad::Retry,
    };
    if len != bytes.len() {
        // Preserve existing POI/module progress when upgrading the original
        // 128-byte save. Its exit pose starts empty and is populated at the
        // next title handoff. Other sizes are unsupported schemas.
        return SaveBlock::decode_legacy_v1(&bytes[..len], PERSISTENT_FLAG_COUNT)
            .map(PoiSaveLoad::Loaded)
            .unwrap_or(PoiSaveLoad::NewGame);
    }
    match SaveBlock::decode(&bytes, PERSISTENT_FLAG_COUNT) {
        Some(save) => PoiSaveLoad::Loaded(save),
        None => PoiSaveLoad::Retry,
    }
}

fn save_poi_state_to_card(port: &mut psx_engine::ControllerPort, save: &SaveBlock) -> bool {
    let mut card = psx_mc::Card::new(psx_mc::HardwareCard::on_port(port, psx_mc::Slot::One));
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

fn apply_debug_body_fallback(
    config: &mut CharacterMotorConfig,
    has_world: bool,
    has_character: bool,
) {
    if has_world && !has_character {
        config.radius = WORLD_PLAYER_RADIUS;
        config.height = WORLD_PLAYER_HEIGHT;
        config.walk_speed = WORLD_FALLBACK_PLAYER_SPEED;
        config.run_speed = WORLD_FALLBACK_PLAYER_SPEED;
    }
}

/// Sim ticks per POI panel/type-on step. The panel and type-on timings were
/// tuned at the playtest's 30 Hz visual cadence, one presented frame every
/// second 60 Hz tick, so a step every two ticks keeps that look while making
/// it independent of how fast frames are actually presented.
pub(super) const POI_PRESENTATION_TICKS_PER_STEP: u8 = 2;

/// Chest height, runtime units, at which the interact prompt tests line of
/// sight for a lever, message or checkpoint (half the player hull, as the
/// melee occlusion probe does).
const INTERACT_SIGHT_LIFT: i32 = 28;

/// Deterministic damage cadence for shared hazard contents. Water is
/// non-damaging; slime deals four health twice per second, lava deals ten
/// health four times per second at the 60 Hz simulation rate.
pub(super) const fn hazard_damage(contents: i16, tick: u32) -> u16 {
    match contents {
        psx_grid::contents::CONTENTS_SLIME if tick % 30 == 0 => 4,
        psx_grid::contents::CONTENTS_LAVA if tick % 15 == 0 => 10,
        _ => 0,
    }
}

const _: () = {
    assert!(hazard_damage(psx_grid::contents::CONTENTS_WATER, 30) == 0);
    assert!(hazard_damage(psx_grid::contents::CONTENTS_SLIME, 29) == 0);
    assert!(hazard_damage(psx_grid::contents::CONTENTS_SLIME, 30) == 4);
    assert!(hazard_damage(psx_grid::contents::CONTENTS_LAVA, 14) == 0);
    assert!(hazard_damage(psx_grid::contents::CONTENTS_LAVA, 15) == 10);
};

/// The shading view of the room the player is in, over the resident rooms of
/// the grid world. Takes the pieces rather than the whole playtest, so a
/// draw call can hold it while it mutably borrows other fields.
pub(super) fn room_lighting_of(
    world_rt: &Option<WorldRuntime>,
    room_index: RoomIndex,
    ambient: [u8; 3],
    camera: WorldCamera,
) -> Option<RuntimeRoomLighting<'_>> {
    let world = world_rt.as_ref()?;
    let room_record = ROOMS.get(room_index.to_usize())?;
    Some(RuntimeRoomLighting {
        room_index,
        ambient: Rgb8::from_array(ambient),
        camera,
        fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
        fog_rgb: Rgb8::from_array(room_record.fog_rgb),
        fog_near: room_record.fog_near,
        fog_far: room_record.fog_far,
        lights: room_light_slice(LIGHTS.as_slice(), room_index),
        probes: Some(world.light_probes()),
    })
}

impl Playtest {
    /// Depth range for effects in `room`: they sort against the grid
    /// renderer, see [`CLASSIC_DEPTH_RANGE`].
    pub(super) fn effect_depth_range(&self, room: RoomIndex) -> DepthRange {
        if ROOMS.get(room.to_usize()).is_some() {
            CLASSIC_DEPTH_RANGE
        } else {
            WORLD_DEPTH_RANGE
        }
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn ensure_poi_save_loaded(&mut self, port: &mut psx_engine::ControllerPort) {
        if self.poi_save_loaded {
            return;
        }
        match load_poi_save_from_card(port) {
            PoiSaveLoad::Loaded(saved) => self.poi_save = saved,
            PoiSaveLoad::NewGame => {}
            PoiSaveLoad::Retry => return,
        }
        self.poi_save_loaded = true;
        self.restore_claimed_poi_rewards();
        self.restore_persistent_destructibles();
        // The logic records were armed before the card was read.
        self.restore_persistent_logic();
        self.sync_door_box_props();
        if self.continue_pending {
            self.apply_continue();
        }
    }

    /// Record persistent logic record `index`'s state in the save (on every
    /// change, in memory; the card write waits for the next save boundary
    /// like every other flag). Doors and switches only; others do nothing.
    pub(super) fn store_persistent_logic(&mut self, index: usize) {
        if self.logic.store_persistent(
            LOGIC.as_slice(),
            LOGIC_SLOTS.as_slice(),
            index,
            &mut self.poi_save,
        ) {
            self.poi_save_dirty = true;
            self.flush_wanted = true;
        }
    }

    /// Whether a persistent door is mid-slide.
    fn persistent_door_moving(&self) -> bool {
        LOGIC.iter().enumerate().any(|(index, record)| {
            record.kind == psx_level::logic_kind::DOOR
                && self
                    .logic
                    .door_slide(LOGIC.as_slice(), index)
                    .is_some_and(|slide| !slide.cleared && slide.offset != [0, 0, 0])
        })
    }

    /// Whether the player is at ease enough for a card write: no persistent
    /// door sliding, no fight, no message up, feet on the ground.
    fn flush_is_calm(&self) -> bool {
        !self.persistent_door_moving()
            && !self.combat_music.engaged
            && self.message_overlay.is_none()
            && self.game_entities.actor_motor().grounded()
    }

    /// Write the save after a persistent door or lever changed, at the first
    /// calm moment (see [`Self::flush_is_calm`]). The write is blocking, as
    /// the checkpoint and title-exit ones are: it costs no memory, and a
    /// calm moment is where a short hitch is cheapest. The
    /// `background-card-write` feature drives the card a transaction a frame
    /// instead, for about 5 KB of heap.
    #[cfg(not(feature = "background-card-write"))]
    pub(super) fn pump_poi_flush(&mut self, ctx: &mut Ctx) {
        if !self.flush_wanted || !self.poi_save_loaded {
            return;
        }
        if !self.poi_save_dirty {
            self.flush_wanted = false;
            return;
        }
        if self.flush_is_calm() {
            self.flush_poi_save(ctx.controller_port());
        }
    }

    /// The background form of [`Self::pump_poi_flush`]: psx-mc's job writes
    /// one card transaction a frame, so nothing stalls.
    #[cfg(feature = "background-card-write")]
    pub(super) fn pump_poi_flush(&mut self, ctx: &mut Ctx) {
        use psx_mc::Step;
        let now = ctx.sim_tick.as_u32();
        if self.card_job.is_busy() {
            let mut card = psx_mc::HardwareCard::on_port(ctx.controller_port(), psx_mc::Slot::One);
            let bytes = {
                let mut bytes = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
                self.poi_save.encode(&mut bytes);
                bytes
            };
            match self.card_job.step(&mut card, now) {
                Ok(Step::NeedsPlan) => {
                    let planned = self
                        .card_job
                        .plan(|c| c.write(PROJECT_SAVE_NAME, PROJECT_SAVE_TITLE, &bytes));
                    if planned.is_err() {
                        self.poi_save_dirty = true;
                    }
                }
                Ok(_) => {}
                // The card went away or refused: stay dirty, a boundary
                // write tries again.
                Err(_) => self.poi_save_dirty = true,
            }
            return;
        }
        if !self.flush_wanted || !self.poi_save_loaded {
            return;
        }
        if !self.poi_save_dirty {
            self.flush_wanted = false;
            return;
        }
        if !self.flush_is_calm() {
            return;
        }
        self.flush_wanted = false;
        self.poi_save_dirty = false;
        self.card_job.begin();
    }

    /// Apply the saved state of every persistent logic record to the
    /// just-armed runtime: boot after a card load, and every respawn.
    pub(super) fn restore_persistent_logic(&mut self) {
        self.logic
            .restore_persistent(LOGIC.as_slice(), LOGIC_SLOTS.as_slice(), &self.poi_save);
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
        for interactable in &INTERACTABLES {
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

    /// Probe the card once the title is up: does a save with a rest point
    /// exist? The Continue button is shown only then. A busy or absent card
    /// is asked again every couple of seconds.
    pub(super) fn probe_continue(&mut self, tick: u32, port: &mut psx_engine::ControllerPort) {
        const RETRY_TICKS: u32 = 120;
        if self.continue_probe != 0 || !(self.continue_tries == 0 || tick % RETRY_TICKS == 0) {
            return;
        }
        self.continue_tries = self.continue_tries.saturating_add(1);
        self.continue_probe = match load_poi_save_from_card(port) {
            PoiSaveLoad::Loaded(save)
                if save.checkpoint_id != 0 && save.resume_position.is_some() =>
            {
                2
            }
            PoiSaveLoad::Loaded(_) | PoiSaveLoad::NewGame => 1,
            PoiSaveLoad::Retry => 0,
        };
    }

    /// Whether the title offers Continue.
    pub(super) fn continue_available(&self) -> bool {
        self.continue_probe == 2
    }

    /// Choose Continue: the next gameplay init reads the card instead of
    /// starting a new game.
    pub(super) fn request_continue(&mut self) {
        self.continue_pending = true;
        self.continue_tries = 0;
    }

    /// Card reads spent on a pending Continue before it gives up and starts
    /// a new game.
    const CONTINUE_MAX_TRIES: u16 = 600;

    /// True while loading must wait for the card read a Continue needs.
    pub(super) fn continue_waiting(&self) -> bool {
        self.continue_pending && !self.poi_save_loaded
    }

    /// Whether the loading screen should read the card this tick.
    pub(super) fn continue_wants_read(&mut self) -> bool {
        if !self.continue_waiting() {
            return false;
        }
        if self.continue_tries >= Self::CONTINUE_MAX_TRIES {
            // Give up: a new game, and never merge an old card later.
            self.continue_pending = false;
            self.poi_save_loaded = true;
            self.poi_save_dirty = true;
            return false;
        }
        self.continue_tries += 1;
        true
    }

    /// Put the player at the saved rest point: the checkpoint is the save's
    /// pose, and the respawn path does the rest (vitality, enemies, doors
    /// and levers from the save).
    fn apply_continue(&mut self) {
        self.continue_pending = false;
        let Some(pose) = self.poi_save.resume_position else {
            return;
        };
        let at = RoomPoint::new(pose.x, pose.y, pose.z);
        let wanted = self.poi_save.checkpoint_id;
        let checkpoint_id = INTERACTABLES
            .iter()
            .find(|i| {
                i.kind == InteractableKind::Checkpoint
                    && psx_level::StableId::of_name(i.checkpoint_id).raw() == wanted
            })
            .map_or("", |i| i.checkpoint_id);
        self.checkpoint = Some(RuntimeCheckpoint {
            // Grid worlds cook as one playtest room whatever their rooms are.
            room: self.room_index,
            position: at,
            yaw: Angle::from_q12(pose.yaw),
            checkpoint_id,
        });
        self.respawn_after_death();
        self.continuing = true;
    }

    /// Flush pending POI state at an intentional save boundary. Memory-card
    /// filesystem writes take many video periods on real hardware, so they
    /// must never run inside a live interaction or an arbitrary gameplay tick.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn flush_poi_save(&mut self, port: &mut psx_engine::ControllerPort) {
        #[cfg(feature = "background-card-write")]
        let busy = self.card_job.is_busy();
        #[cfg(not(feature = "background-card-write"))]
        let busy = false;
        if !self.poi_save_loaded || !(self.poi_save_dirty || busy) {
            return;
        }
        // A blocking write supersedes a background one in progress.
        #[cfg(feature = "background-card-write")]
        self.card_job.abort();
        self.flush_wanted = false;
        self.poi_save_dirty = !save_poi_state_to_card(port, &self.poi_save);
    }

    pub(super) fn retry_poi_card_load(&mut self, tick: u32, port: &mut psx_engine::ControllerPort) {
        const RETRY_TICKS: u32 = 300;
        if self.poi_save_loaded || tick % RETRY_TICKS != 0 {
            return;
        }
        self.ensure_poi_save_loaded(port);
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
                abilities: interactable.reward_abilities,
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
                abilities: interactable.reward_abilities,
            },
        };
        candidate.is_depleted(&self.poi_save)
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn grant_point_of_interest_reward(&mut self, index: usize) -> Option<BoostModuleId> {
        let Some(interactable) = INTERACTABLES.get(index) else {
            return None;
        };
        let persistence = poi_persistent_flags(interactable);
        if persistence.reward_claimed(&self.poi_save) {
            return None;
        }
        // Abilities are granted first and never fail: they cost no inventory
        // room, so a full pack cannot keep a fragment's gift from the player.
        let abilities = interactable.reward_abilities;
        let newly_unlocked = abilities & !self.abilities().bits();
        let mut module = None;
        if interactable.reward_quantity != 0 {
            if let Some(candidate) = boost_module_for_reward(interactable.reward_resource) {
                let already_owned = self.power_up_inventory.contains(candidate)
                    || BoostSlotId::ALL
                        .iter()
                        .any(|slot| self.power_up_loadout.module(*slot) == candidate);
                if already_owned || !self.power_up_inventory.add(candidate) {
                    return None;
                }
                module = Some(candidate);
            }
        }
        if module.is_none() && abilities == 0 {
            return None;
        }
        if self.poi_save.grant_abilities(abilities) {
            self.poi_save_dirty = true;
        }
        if persistence.mark_reward_claimed(&mut self.poi_save) {
            self.poi_save_dirty = true;
        }
        self.queue_gameplay_sfx(LevelGameplaySfxEvent::ItemAcquired);
        if newly_unlocked != 0 {
            self.announce_unlocked_abilities(newly_unlocked);
        }
        module
    }

    /// Tell the player what they can now do, through the legacy message
    /// overlay (runtime copy, like the socket hint, so it needs no cooked
    /// page). The module panel, if the same pickup also gave one, comes up
    /// after this is dismissed.
    fn announce_unlocked_abilities(&mut self, unlocked: u16) {
        use psx_level::ability;
        let (title, body) = if unlocked & ability::SPRINT != 0 && unlocked & ability::DASH != 0 {
            (
                crate::loc::tr("ui.ability.both.title", "SPRINT AND DASH"),
                crate::loc::tr(
                    "ui.ability.both.body",
                    "HOLD CIRCLE TO SPRINT.\nTAP CIRCLE TO DASH.",
                ),
            )
        } else if unlocked & ability::SPRINT != 0 {
            (
                crate::loc::tr("ui.ability.sprint.title", "SPRINT"),
                crate::loc::tr("ui.ability.sprint.body", "HOLD CIRCLE TO SPRINT."),
            )
        } else {
            (
                crate::loc::tr("ui.ability.dash.title", "DASH"),
                crate::loc::tr("ui.ability.dash.body", "TAP CIRCLE TO DASH."),
            )
        };
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
        self.set_poi_page_type_frame(0);
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn advance_poi_message(&mut self) {
        if self.poi_closing {
            return;
        }
        if !self.acquired_module.is_none() {
            if !self.complete_acquired_item_reveal() {
                self.begin_message_close();
            }
            return;
        }
        if self.complete_active_message_reveal() {
            return;
        }
        if let Some(message) = self.poi_messages.active() {
            if message.page_offset() + 1 < message.page_count() {
                self.finish_poi_message();
            } else {
                self.begin_message_close();
            }
        }
    }

    fn begin_message_close(&mut self) {
        self.poi_closing = true;
        self.set_poi_presentation_frames(
            psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES,
            self.poi_page_type_frame,
        );
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn dismiss_legacy_message(&mut self) {
        if !self.poi_closing && !self.complete_legacy_message_reveal() {
            self.begin_message_close();
        }
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn finish_poi_message(&mut self) {
        use psx_game_runtime::poi::{MessageAdvance, MessageSource};
        if !self.acquired_module.is_none() {
            self.acquired_module = BoostModuleId::NONE;
            self.set_poi_presentation_frames(0, 0);
            // Souls-style one-time tutorial: the first pickup explains where
            // the module goes. Runtime-only copy through the legacy overlay,
            // so it needs no cooked page and no save flag.
            if !self.socket_hint_shown {
                self.socket_hint_shown = true;
                self.message_overlay = Some(RuntimeMessageOverlay {
                    title: "",
                    body: crate::loc::tr(
                        "ui.hint.sockets",
                        "START: OPENS INVENTORY\nEQUIP RECOVERED MODULES.",
                    ),
                });
                self.set_poi_page_type_frame(0);
            }
            return;
        }
        match self.poi_messages.advance() {
            MessageAdvance::Advanced(_) => {
                self.set_poi_page_type_frame(0);
            }
            MessageAdvance::Closed(MessageSource::PointOfInterest(index)) => {
                if let Some(interactable) = INTERACTABLES.get(usize::from(index)) {
                    if poi_persistent_flags(interactable).mark_read(&mut self.poi_save) {
                        self.poi_save_dirty = true;
                    }
                }
                if let Some(module) = self.grant_point_of_interest_reward(usize::from(index)) {
                    self.acquired_module = module;
                    self.set_poi_presentation_frames(0, 0);
                }
            }
            MessageAdvance::Closed(MessageSource::World) | MessageAdvance::Inactive => {}
        }
    }

    fn complete_acquired_item_reveal(&mut self) -> bool {
        let Some(module) = self
            .acquired_module
            .index()
            .and_then(|index| BOOST_MODULES.get(index))
        else {
            return false;
        };
        let required_panel = psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES;
        let name = self.acquired_module.index().map_or(module.name, |index| {
            crate::loc::module_name(index, module.name)
        });
        let prefix = crate::loc::item_acquired_prefix();
        let required_type = u16::try_from(prefix.chars().count() + name.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_mul(psx_engine::ui::MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
        if self.poi_panel_frame >= required_panel && self.poi_page_type_frame >= required_type {
            return false;
        }
        self.set_poi_presentation_frames(required_panel, required_type);
        true
    }

    /// The first Cross press during panel motion or type-on completes the
    /// current presentation instead of skipping unread copy. A later press is
    /// then free to advance/close through `MessageController`.
    fn complete_active_message_reveal(&mut self) -> bool {
        let Some(message) = self.poi_messages.active() else {
            return false;
        };
        let Some(page_text) = crate::loc::page_text(message.page() as usize) else {
            return false;
        };
        let required_panel = if matches!(
            message.source(),
            psx_game_runtime::poi::MessageSource::PointOfInterest(_)
        ) {
            psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES
        } else {
            0
        };
        let required_type = u16::try_from(page_text.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_mul(psx_engine::ui::MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
        if self.poi_panel_frame >= required_panel && self.poi_page_type_frame >= required_type {
            return false;
        }

        self.set_poi_presentation_frames(required_panel, required_type);
        true
    }

    /// A legacy Message/Checkpoint panel follows the same first-press rule as
    /// paged Archive copy: finish the current type-on before allowing the next
    /// press to dismiss it.
    pub(super) fn complete_legacy_message_reveal(&mut self) -> bool {
        let Some(message) = self.message_overlay else {
            return false;
        };
        let required_type = u16::try_from(message.body.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_mul(psx_engine::ui::MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
        if self.poi_page_type_frame >= required_type {
            return false;
        }
        self.set_poi_page_type_frame(required_type);
        true
    }

    /// Keep live, prepared, and submitted POI presentation counters coherent
    /// when input changes the content between deferred render stages. Without
    /// this atomic reset a new page can flash fully typed, or a reward panel
    /// can draw at its compact endpoint before jumping back to the full panel.
    pub(super) fn set_poi_presentation_frames(&mut self, panel: u16, page: u16) {
        self.poi_panel_frame = panel;
        self.prepared_poi_panel_frame = panel;
        self.overlay_poi_panel_frame = panel;
        self.set_poi_page_type_frame(page);
    }

    pub(super) fn set_poi_page_type_frame(&mut self, page: u16) {
        self.poi_page_type_frame = page;
        self.prepared_poi_page_type_frame = page;
        self.overlay_poi_page_type_frame = page;
    }

    /// Advance the POI panel and type-on from the fixed simulation, one
    /// presentation step every `POI_PRESENTATION_TICKS_PER_STEP` sim ticks.
    ///
    /// This used to run from `render`, once per prepared frame. The type-on
    /// gates input (the first press only completes unfinished copy), so a
    /// slower render left the copy unfinished when a press arrived and the
    /// same press did something else: on the souls gate, a heavier world pass
    /// kept a message open and the tape walked into the wrong fight. Driving it
    /// from the sim makes the press behave the same at every render rate.
    pub(super) fn tick_poi_presentation(&mut self) {
        self.poi_presentation_subtick = self.poi_presentation_subtick.saturating_add(1);
        if self.poi_presentation_subtick < POI_PRESENTATION_TICKS_PER_STEP {
            return;
        }
        self.poi_presentation_subtick = 0;
        self.advance_poi_presentation_frame();
    }

    /// Freeze the presentation progress the frame being prepared will draw.
    pub(super) fn snapshot_poi_presentation_for_render(&mut self) {
        self.prepared_poi_panel_frame = self.poi_panel_frame;
        self.prepared_poi_page_type_frame = self.poi_page_type_frame;
    }

    /// One presentation step of the POI panel and type-on.
    pub(super) fn advance_poi_presentation_frame(&mut self) {
        if self.poi_closing {
            self.poi_panel_frame = self.poi_panel_frame.saturating_sub(1);
            if self.poi_panel_frame == 0 {
                self.poi_closing = false;
                if !self.acquired_module.is_none() || self.poi_messages.active().is_some() {
                    self.finish_poi_message();
                } else {
                    self.message_overlay = None;
                }
            }
            return;
        }
        if !self.acquired_module.is_none() {
            if self.poi_panel_frame < psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
                self.poi_panel_frame = self.poi_panel_frame.saturating_add(1);
            } else {
                self.poi_page_type_frame = self.poi_page_type_frame.saturating_add(1);
            }
            return;
        }
        let Some(message) = self.poi_messages.active() else {
            if self.message_overlay.is_some() {
                self.poi_page_type_frame = self.poi_page_type_frame.saturating_add(1);
            }
            return;
        };
        if matches!(
            message.source(),
            psx_game_runtime::poi::MessageSource::PointOfInterest(_)
        ) && self.poi_panel_frame < psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES
        {
            self.poi_panel_frame = self.poi_panel_frame.saturating_add(1);
        } else {
            self.poi_page_type_frame = self.poi_page_type_frame.saturating_add(1);
        }
    }

    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn open_demo_outro_once(&mut self) {
        const OUTRO_SCENE: u8 = 1;
        // A scripted duel is not the demo's story: it releases the other
        // training entities as dead at its start (a Heavy among them would
        // read as the boss defeated) and its fighter may kill the boss.
        if self.duel.active
            || self.poi_messages.world_message_shown(OUTRO_SCENE)
            || self.death_ticks_remaining != 0
            || self.message_overlay.is_some()
            || !self.acquired_module.is_none()
            || self.inventory_overlay_active
        {
            return;
        }
        // This demo's authored boss model opts into the closing message.
        // Disabled actors begin Dead and must never complete the demo.
        let boss_defeated = GAME_ENTITIES.iter().enumerate().any(|(index, record)| {
            record.flags & psx_level::game_entity_flags::ENABLED != 0
                && self.game_entities.state(index)
                    == psx_game_runtime::entities::GameEntityState::Dead
                && MODEL_INSTANCES
                    .get(usize::from(record.model_instance))
                    .and_then(|instance| MODELS.get(instance.model.to_usize()))
                    .is_some_and(|model| model.name == "Heavy Enemy Model")
                && self.game_entities.state(index)
                    == psx_game_runtime::entities::GameEntityState::Dead
                && self
                    .game_entities
                    .clip_for_state(GAME_ENTITIES.as_slice(), index)
                    .phase_ticks
                    >= 90
        });
        if boss_defeated
            && self.poi_messages.open_world(
                OUTRO_SCENE,
                psx_game_runtime::poi::MessagePageSpan::new(
                    crate::loc::DEMO_OUTRO_FIRST,
                    crate::loc::DEMO_OUTRO_COUNT,
                ),
            )
        {
            self.set_poi_presentation_frames(psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES, 0);
        }
    }

    pub(super) fn initial_world_message_active(&self) -> bool {
        let (Some(active), Some(welcome)) = (self.poi_messages.active(), WORLD_MESSAGE.get())
        else {
            return false;
        };
        active.source() == psx_game_runtime::poi::MessageSource::World
            && active.page() - active.page_offset() == welcome.page_first
            && active.page_count() == welcome.page_count
    }

    pub(super) fn open_world_message_once(&mut self) {
        let Some(message) = WORLD_MESSAGE.get() else {
            return;
        };
        if self.poi_messages.open_world(
            0,
            psx_game_runtime::poi::MessagePageSpan::new(message.page_first, message.page_count),
        ) {
            // World copy has no compact prompt to expand from, so its first
            // presented frame begins typing immediately.
            self.set_poi_presentation_frames(psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES, 0);
        }
    }

    /// Fixed-point stat multipliers from the four endpoint sockets at the
    /// current health and energy levels.
    pub(super) fn vitality_modifiers(&self) -> VitalityModifiers {
        // All four sockets act in both stances.
        self.power_up_loadout
            .modifiers(self.boost_bars(), BOOST_MODULES)
    }

    /// The levels the boost sockets read: the health bar and the energy bar.
    pub(super) fn boost_bars(&self) -> BoostBars {
        BoostBars {
            health: self.game_entities.actor().health,
            energy: VitalityPool::at(
                self.game_entities.actor().flow.energy,
                self.game_entities.actor_stats().energy_max,
            ),
        }
    }

    /// Live authored playback rate for an animation. Only the four attack
    /// actions use the module Attack Speed lane; locomotion and defensive
    /// actions retain their authored cadence.
    pub(super) fn player_action_speed_q8(
        &self,
        character: &RuntimeCharacter,
        anim: PlayerAnim,
    ) -> u16 {
        live_action_speed_q8(self.vitality_modifiers(), character, anim)
    }

    pub(super) fn player_character_for_anim(
        &self,
        character: &RuntimeCharacter,
        _anim: PlayerAnim,
    ) -> RuntimeCharacter {
        let mut character = *character;
        let modifiers = self.vitality_modifiers();
        for anim in [
            PlayerAnim::LightAttack,
            PlayerAnim::HeavyAttack,
            PlayerAnim::VertLightAttack,
            PlayerAnim::VertHeavyAttack,
            PlayerAnim::RangedAttack,
        ] {
            let action = anim.action();
            let authored = character.action_speed(action);
            character.action_speeds[action.to_index()] = modifiers.attack_speed_q8(authored);
        }
        character
    }

    pub(super) const fn player_attack_channel(anim: PlayerAnim) -> VitalityChannelId {
        match anim {
            PlayerAnim::VertLightAttack
            | PlayerAnim::VertHeavyAttack
            | PlayerAnim::RangedAttack => VitalityChannelId::Two,
            _ => VitalityChannelId::One,
        }
    }

    // Test hook: damage straight onto her fighter, no landing.
    #[cfg(test)]
    /// Apply an untyped incoming hit as aligned with the active stance.
    pub(super) fn apply_untyped_player_damage(&mut self, damage: u16) -> bool {
        // Untyped damage has no colour to compare, so it is treated as aligned
        // with whatever is active: the player cannot read an axis that was
        // never authored, and should not be punished for it.
        self.player_take_damage(CombatantId::WORLD, HitKind::Melee, None, damage)
    }

    // Test hook: damage straight onto her fighter, no landing.
    #[cfg(test)]
    /// Damage from a hazard volume, untyped like a legacy hit. A hit bigger
    /// than the health left is lethal.
    pub(super) fn apply_hazard_player_damage(&mut self, damage: u16) -> bool {
        self.player_take_damage(CombatantId::WORLD, HitKind::Hazard, None, damage)
    }

    // Test hook: damage straight onto her fighter, no landing.
    #[cfg(test)]
    /// The projectile colour sets the multiplier; the active stance receives damage.
    pub(super) fn apply_typed_player_damage(
        &mut self,
        channel: psx_game_runtime::projectiles::ProjectileDamageChannel,
        damage: u16,
    ) -> bool {
        self.apply_typed_player_damage_from(CombatantId::WORLD, channel, damage)
    }

    // Test hook: damage straight onto her fighter, no landing.
    #[cfg(test)]
    /// [`Self::apply_typed_player_damage`] from the fighter that shot it.
    pub(super) fn apply_typed_player_damage_from(
        &mut self,
        attacker: CombatantId,
        channel: psx_game_runtime::projectiles::ProjectileDamageChannel,
        damage: u16,
    ) -> bool {
        let channel = match channel {
            psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon => {
                VitalityChannelId::One
            }
            psx_game_runtime::projectiles::ProjectileDamageChannel::Zenith => {
                VitalityChannelId::Two
            }
        };
        self.player_take_damage(attacker, HitKind::Projectile, Some(channel), damage)
    }

    // Test hook: damage straight onto her fighter, no landing.
    #[cfg(test)]
    /// The player's health damage, through [`Combatant::receive_hit`].
    ///
    /// Shell modifiers reduce the hit first. `channel` `None` is untyped and
    /// counts as aligned with the active stance. `attacker` is who dealt it
    /// ([`CombatantId::WORLD`] for hazards). True when the health bar is
    /// empty afterwards.
    pub(super) fn player_take_damage(
        &mut self,
        attacker: CombatantId,
        kind: HitKind,
        channel: Option<VitalityChannelId>,
        damage: u16,
    ) -> bool {
        let damage = self.vitality_modifiers().incoming_damage(damage);
        let strike = Strike {
            attacker,
            kind,
            channel: channel.unwrap_or(self.game_entities.actor().stance.active()),
            damage,
            poise: 0,
            crit: false,
        };
        let posture = self.player_posture(false, SimTick::ZERO);
        let (hit, _) = self.game_entities.receive_actor_hit(strike, posture);
        if hit.damage_applied > 0 {
            self.queue_gameplay_sfx(LevelGameplaySfxEvent::PlayerDamage);
        }
        hit.defeated
    }

    /// Her posture for a hit at `now`, read off her body: `armored`, the
    /// reaction her animation is playing (a knockdown while her fighter is
    /// down, a stagger while Stun or Parried plays, a flinch while her Hit
    /// React still holds her) and whether she binds a Hit React clip. The
    /// landing and the reaction rule are every fighter's
    /// ([`psx_game_runtime::entities::GameEntities::receive_actor_hit`]).
    pub(super) fn player_posture(&self, armored: bool, now: SimTick) -> Posture {
        let reacting = if self.game_entities.actor().knocked_down() {
            Some(Reaction::Knockdown)
        } else {
            match self.anim.state {
                PlayerAnim::Stun => Some(Reaction::Stagger),
                PlayerAnim::Parried => Some(Reaction::Parried),
                PlayerAnim::HitReact if self.game_entities.actor_action().lock_until > now => {
                    Some(Reaction::Flinch)
                }
                _ => None,
            }
        };
        Posture {
            armored,
            reacting,
            flinches: self
                .character
                .is_some_and(|c| c.action_clip(PlayerAnim::HitReact.action()).is_some()),
        }
    }

    #[inline]
    pub(super) fn queue_gameplay_sfx(&mut self, event: LevelGameplaySfxEvent) {
        self.gameplay_sfx_events |= event.bit();
    }

    /// Arm the shared player death sequence: `delay_ticks` of locked
    /// Death animation, then [`Self::respawn_after_death`] when the
    /// countdown in `update_gameplay` reaches zero. One arming point
    /// for every cause (combat damage, liquid hazards, lethal
    /// water); callers floor health themselves. PLAYER_DEATHS counts
    /// here so the death event is recorded even if a run ends before
    /// the respawn completes.
    pub(super) fn arm_player_death(&mut self, by_combat: bool, now: SimTick, video_hz: VideoHz) {
        let delay_ticks = self.death_ticks(video_hz);
        self.death_ticks_remaining = delay_ticks;
        self.death_by_combat = by_combat;
        self.switch_player_anim(PlayerAnim::Death, now, video_hz);
        self.game_entities.actor_action_mut().lock_until =
            now.saturating_add(u32::from(delay_ticks));
        self.lock_target = None;
        self.camera_recenter_requested = false;
        self.soft_lock_target = None;
        self.active_interactable = None;
        telemetry::counter(telemetry::counter::PLAYER_DEATHS, 1);
    }

    /// How long the player's character dies, by the one dying rule every
    /// fighter follows: its own Death clip, at the speed and frames it
    /// plays (an entity's plays out the same way before its body
    /// dissolves); [`DEATH_FALLBACK_TICKS`] for a body with no Death clip.
    fn death_ticks(&self, video_hz: VideoHz) -> u16 {
        self.character
            .as_ref()
            .filter(|character| character.action_clip(PlayerAnim::Death.action()).is_some())
            .and_then(|character| {
                self.player_clip_duration_vblanks(
                    character,
                    character.clip_for(PlayerAnim::Death),
                    video_hz,
                    self.player_action_speed_q8(character, PlayerAnim::Death),
                    character.action_frame_range(PlayerAnim::Death.action()),
                )
            })
            .map_or(DEATH_FALLBACK_TICKS, |ticks| {
                ticks.clamp(1, u32::from(u16::MAX)) as u16
            })
    }

    /// Clear run progress while keeping front-end settings and loaded assets.
    // Initial load and the menu share the full reset; keep one copy in RAM.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn reset_new_game(&mut self) {
        self.hook_travel = None;
        self.hook_target = None;
        self.hook_attached = None;
        self.fall_from_arch = false;
        self.hook_charge = psx_game_runtime::hook_points::ArchCharge::EMPTY;
        self.hook_selected = None;
        self.hook_visible = 0;
        self.hook_fov_delta_q8 = 0;
        self.hook_burst_started = false;
        self.opening = opening_sequence::OpeningSequence::default();
        // New Game must not reload a previous run from the card, including
        // when a card was absent at boot and becomes available later.
        self.poi_save = SaveBlock::new(
            self.game_entities.actor_stats().max_health,
            PERSISTENT_FLAG_COUNT,
        );
        self.poi_save_loaded = true;
        self.poi_save_load_attempted = true;
        self.poi_save_dirty = true;
        // Overwrite the old card save soon, in the background.
        self.flush_wanted = true;
        self.poi_messages = MessageController::new();
        self.poi_closing = false;
        self.set_poi_presentation_frames(0, 0);
        self.socket_hint_shown = false;
        self.souls = SoulsWallet::EMPTY;
        self.damage_numbers = DamageNumbers::default();
        self.gameplay_epoch_set = false;
        self.player_contents_memo = None;
        self.gameplay_sfx_events = 0;
        self.lock_target = None;
        self.soft_lock_target = None;
        self.lock_switch_stick_held = false;
        self.lock_invalid_ticks = 0;
        self.soft_lock_suppressed = false;
        self.evade_run_hold_ticks = 0;
        self.evade_run_hold_consumed = false;
        self.evade_latched_move = (Q12::ZERO, Q12::ZERO);
        self.game_entities.actor_action_mut().evade_buffer = 0;
        self.anim.state = PlayerAnim::Idle;
        self.game_entities.actor_action_mut().start = SimTick::ZERO;
        self.anim.switched = None;
        self.game_entities.actor_action_mut().lock_until = SimTick::ZERO;
        self.loco = LocoPhase::Idle;
        self.active_interactable = None;
        self.checkpoint = None;
        self.message_overlay = None;
        self.box_props.reset_dynamic_state();
        // Phase-3 gameplay layer: spawn entity/logic state 1:1 from
        // the cooked tables (empty tables leave both inert; the same
        // calls re-run on future checkpoint respawns), then push the
        // initial door states onto their box props (START_ON doors
        // begin open without a fire event).
        self.spawn_entities();
        self.deferred_enemy_attacks.clear();
        self.combat_projectiles.clear();
        self.combat_projectile_impacts.clear();
        self.game_entities.actor_action_mut().buffer.clear();
        self.game_entities
            .actor_action_mut()
            .authored_attack
            .clear();
        self.game_entities.actor_action_mut().authored_dodge.clear();
        self.logic.init_from_records(LOGIC.as_slice());
        self.logic_fired_reported = 0;
        // Playtest lives in a zeroed MaybeUninit, so the combatant is set
        // explicitly on every reset rather than relying on the zero pattern.
        let kits = self.player_kits();
        let (actor, stats) = self.game_entities.actor_and_stats();
        actor.reset(stats, kits);
        self.camera_recenter_requested = false;
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.aim_control = aim_control::AimControl::EMPTY;
        self.vitality_circles = VitalityCircleState::EMPTY;
        self.power_up_loadout = PowerUpLoadout::DEFAULT;
        self.power_up_inventory = BoostInventory::EMPTY;
        self.selected_power_up_slot = BoostSlotId::HealthEmpty as u8;
        self.selected_power_up_item = BoostModuleId::NONE;
        self.inventory_ui_state = crate::playtest_scene::INVENTORY_UI_SOCKETS;
        self.inventory_overlay_active = false;
        self.combat_music = CombatMusicState::default();
        self.acquired_module = BoostModuleId::NONE;
        self.death_ticks_remaining = 0;
        self.game_entities
            .clear_hazard_push(RuntimeGameEntities::ACTOR_SLOT);
        self.death_by_combat = false;
        self.weapon_attach_reported = false;
        self.game_entities
            .begin_swing(RuntimeGameEntities::ACTOR_SLOT);
        self.destructibles.reset();
        self.clear_actor_pose_snapshots();
    }

    /// Spawn every entity from the cooked tables, each with its own
    /// Character as its body, and give the actor slot her swap timing.
    fn spawn_entities(&mut self) {
        self.game_entities.set_characters(&CHARACTERS[..]);
        self.game_entities
            .spawn_from_records_with_ids(GAME_ENTITIES.as_slice(), GAME_ENTITY_IDS.as_slice());
        self.entity_attacks_timed = false;
        self.game_entities
            .set_swap_timing(RuntimeGameEntities::ACTOR_SLOT, self.player_stance_config);
    }

    /// Start a fresh combat life without changing checkpoints or collected items.
    /// In-flight shots and queued inputs belong to the old life, not the spawn.
    fn reset_life_combat(&mut self) {
        let kits = self.player_kits();
        let (actor, stats) = self.game_entities.actor_and_stats();
        actor.reset(stats, kits);
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.aim_control = aim_control::AimControl::EMPTY;
        self.combat_projectiles.clear();
        self.combat_projectile_impacts.clear();
        self.deferred_enemy_attacks.clear();
        self.game_entities.actor_action_mut().clear_requests();
        self.game_entities
            .begin_swing(RuntimeGameEntities::ACTOR_SLOT);
        self.camera_recenter_requested = false;
        self.player_actor_pose = None;
        self.previous_player_actor_pose = None;
        self.player_contents_memo = None;
        self.game_entities.actor_motor_mut().interrupt_action();
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.aim_control = aim_control::AimControl::EMPTY;
        self.loco = LocoPhase::Idle;
    }

    /// Put the world back to its authored state except what the save keeps:
    /// enemies respawn, transient logic (triggers, doors, one-shots) re-arms,
    /// persistent doors and levers keep what the player did to them, box
    /// props reset. A death and a rest at a checkpoint both do this.
    fn rearm_world(&mut self) {
        self.spawn_entities();
        self.logic.init_from_records(LOGIC.as_slice());
        self.restore_persistent_logic();
        // The logic runtime restarts its rolling fired total with the
        // records; restart the reported watermark with it or the
        // LOGIC_RECORDS_FIRED delta counter under-reports until the new
        // total passes the pre-reset one.
        self.logic_fired_reported = 0;
        self.box_props.reset_dynamic_state();
        self.sync_door_box_props();
    }

    /// Rest at a checkpoint like a bonfire: both vitality pools refill, the
    /// world re-arms (see [`Self::rearm_world`]) and the save, which now holds
    /// the new rest point, is written. The respawn point itself was set by
    /// [`Self::set_checkpoint`].
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn rest_at_checkpoint(&mut self, port: &mut psx_engine::ControllerPort) {
        self.reset_life_combat();
        self.deferred_enemy_attacks.clear();
        self.rearm_world();
        self.flush_poi_save(port);
    }

    /// Respawn after ANY completed death sequence (combat or hazard).
    ///
    /// Persistence policy (the souls rule: the world resets, the
    /// checkpoint persists): `self.checkpoint` deliberately survives
    /// death and decides the respawn pose; enemies, logic records
    /// (including fired-once triggers), door states, and box props are
    /// TRANSIENT and re-arm 1:1 from their cooked tables below.
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn respawn_after_death(&mut self) {
        self.hook_travel = None;
        self.hook_target = None;
        self.hook_attached = None;
        self.fall_from_arch = false;
        self.hook_charge = psx_game_runtime::hook_points::ArchCharge::EMPTY;
        self.hook_selected = None;
        self.hook_visible = 0;
        self.hook_fov_delta_q8 = 0;
        self.hook_burst_started = false;
        let respawning_at_checkpoint = self.checkpoint.is_some();
        let (room, position, yaw) = if let Some(checkpoint) = self.checkpoint {
            (checkpoint.room, checkpoint.position, checkpoint.yaw)
        } else {
            let spawn = PLAYER_CONTROLLER
                .get()
                .map_or(PLAYER_SPAWN.get(), |controller| controller.spawn);
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
        self.game_entities
            .actor_motor_mut()
            .snap_to(position, motor_yaw);
        self.reset_life_combat();
        self.death_ticks_remaining = 0;
        self.anim.state = PlayerAnim::Idle;
        self.anim.switched = None;
        self.game_entities.actor_action_mut().lock_until = SimTick::ZERO;
        self.lock_target = None;
        self.soft_lock_target = None;
        self.active_interactable = None;
        self.evade_run_hold_ticks = 0;
        self.evade_run_hold_consumed = false;
        // New life: the next successful weapon socket resolution counts
        // as this life's PLAYER_WEAPON_ATTACHMENTS event.
        self.weapon_attach_reported = false;
        self.rearm_world();
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            // The camera orbits behind the player: half a turn from the
            // facing a checkpoint stores (a spawn stores the orbit yaw).
            if respawning_at_checkpoint {
                yaw.add(Angle::HALF)
            } else {
                yaw
            },
        );
        self.render_camera = world_camera_from_position_focus(
            self.camera.projection(PROJECTION),
            self.camera.position(),
            self.camera.focus(),
        );
        #[cfg(not(feature = "cd-stream-bench"))]
        if self.world_rt.is_none() {
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
        self.game_entities
            .actor_action_mut()
            .authored_attack
            .clear();
        self.game_entities.actor_action_mut().authored_dodge.clear();
        // Gait to gait, enter the incoming cycle at the phase the outgoing one
        // had reached: the shared track backdates the clock by the carry.
        let carried = self.gait_phase_carry(self.anim.state, anim, now, video_hz);
        self.anim.switch(
            anim,
            &mut self.game_entities.actor_action_mut().start,
            now,
            carried,
        );
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
        let Some(character) = self.character.as_ref() else {
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
        fighter_anim::gait_phase_carry(
            now.saturating_sub(self.game_entities.actor_action().start),
            out_cycle,
            in_cycle,
        )
    }

    /// Resolve the active crossfade for this render tick, if any.
    ///
    /// Alpha ramps linearly over the window; attacks use the short
    /// window so combat stays snappy while locomotion soft-blends.
    ///
    /// Keep outgoing clips advancing except when a gait stops: continuing
    /// that stride would show extra steps after the motor is already still.
    pub(super) fn player_anim_blend(&self, now: SimTick) -> Option<PlayerAnimBlend> {
        self.anim.fade(now).map(|fade| PlayerAnimBlend {
            anim: fade.from,
            local_tick: fade.local_tick,
            alpha_q12: fade.alpha_q12,
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
        if !self.lock_player_anim_action(&character, anim, now, video_hz) {
            return false;
        }
        self.switch_player_anim(anim, now, video_hz);
        if player_anim_is_attack(anim) {
            // A fresh swing gets a fresh one-hit-per-enemy mask.
            self.game_entities
                .begin_swing(RuntimeGameEntities::ACTOR_SLOT);
            self.ranged_ready.released = 0;
            self.destructibles.begin_swing();
        }
        true
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn player_melee_read(&self) -> u8 {
        if !matches!(
            self.anim.state,
            PlayerAnim::LightAttack | PlayerAnim::HeavyAttack
        ) {
            return 0;
        }
        let (Some(c), Some(pose)) = (self.character.as_ref(), self.player_actor_pose.as_ref())
        else {
            return 0;
        };
        let first = c.combat_capsule_first.to_usize();
        let frame = pose.pose().phase_q12() >> 12;
        let mut first_hit = u32::MAX;
        let mut end_hit = 0;
        for capsule in COMBAT_CAPSULES
            .get(first..first + usize::from(c.combat_capsule_count))
            .unwrap_or(&[])
        {
            if capsule.flags & psx_level::combat_capsule_flags::HITBOX != 0
                && capsule.action == self.anim.state.action().to_index() as u8
            {
                first_hit = first_hit.min(u32::from(capsule.active_start_frame));
                end_hit = end_hit.max(u32::from(capsule.active_end_frame));
            }
        }
        if first_hit == u32::MAX {
            0
        } else if frame < first_hit {
            1
        } else if frame >= end_hit {
            3
        } else {
            2
        }
    }

    /// The final half of a melee windup is readable and interruptible; active
    /// heavy armour and ordinary movement never count as a firearm opening.
    pub(super) fn player_shot_opening(&self) -> bool {
        if !matches!(
            self.anim.state,
            PlayerAnim::LightAttack | PlayerAnim::HeavyAttack
        ) {
            return false;
        }
        let (Some(c), Some(pose)) = (self.character.as_ref(), self.player_actor_pose.as_ref())
        else {
            return false;
        };
        let first = c.combat_capsule_first.to_usize();
        let frame = pose.pose().phase_q12() >> 12;
        COMBAT_CAPSULES
            .get(first..first + usize::from(c.combat_capsule_count))
            .unwrap_or(&[])
            .iter()
            .filter(|c| {
                c.flags & psx_level::combat_capsule_flags::HITBOX != 0
                    && c.action == self.anim.state.action().to_index() as u8
            })
            .map(|c| u32::from(c.active_start_frame))
            .min()
            .is_some_and(|start| frame >= start / 2 && frame < start)
    }

    /// Test hook: a poise-only hit of `damage` by `attacker` on her own
    /// stance (a swing, else a bolt when `shot_only`), then the reaction it
    /// calls for. True when it broke posture.
    #[cfg(test)]
    pub(super) fn react_player_to_hit(
        &mut self,
        attacker: CombatantId,
        damage: u16,
        armored: bool,
        shot_only: bool,
        crit: bool,
        ctx: &Ctx,
    ) -> bool {
        if self.death_ticks_remaining != 0 {
            return false;
        }
        let strike = Strike {
            attacker,
            kind: if shot_only {
                HitKind::Projectile
            } else {
                HitKind::Melee
            },
            channel: self.game_entities.actor().stance.active(),
            damage: 0,
            poise: damage,
            crit,
        };
        let posture = self.player_posture(armored, ctx.sim_tick);
        let (out, reaction) = self.game_entities.receive_actor_hit(strike, posture);
        if let Some(reaction) = reaction {
            self.play_player_reaction(reaction, ctx);
        }
        out.poise_broken
    }

    /// She parried a strike by fighter `attacker` (her
    /// [`Combatant::receive_hit`] gave the refund, swap cooldown and crit;
    /// `riposte` is what the parry did to an entity attacker): the tell, the
    /// refund over her in the parry colour and break sparks on the attacker.
    // Rare (a parry, crit or knockdown): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn player_parried(
        &mut self,
        attacker: Option<usize>,
        riposte: psx_game_runtime::entities::GameEntityHitOutcome,
        ctx: &Ctx,
    ) {
        telemetry::debug_log("player parry");
        self.queue_gameplay_sfx(LevelGameplaySfxEvent::Parry);
        self.duel_log_event(ctx, 0, duel::DUEL_EVENT_PARRY);
        let now = self.gameplay_tick(ctx.sim_tick);
        let player = self.game_entities.actor_motor().position();
        let height = self.character.map_or(0, |c| c.height.max(0) as u16);
        let attacker = attacker.filter(|&slot| slot < self.game_entities.count());
        let zenith = self.game_entities.actor().stance.active() == VitalityChannelId::Two;
        self.player_flash
            .begin(&COMBAT_FEEDBACK, FlashKind::Parry, zenith);
        self.spawn_parry_burst(
            [player.x, player.y, player.z],
            height,
            attacker.map(|slot| self.game_entities.position(slot)),
            self.room_index,
            zenith,
        );
        self.damage_numbers.spawn(
            player_hit_anchor([player.x, player.y, player.z], height),
            self.room_index,
            self.game_entities.actor_stats().parry_refund,
            DamageNumberChannel::Parry,
            now,
        );
        let Some((attacker, record)) =
            attacker.and_then(|slot| GAME_ENTITIES.get(slot).map(|record| (slot, record)))
        else {
            return;
        };
        self.spawn_combat_hit_sparks(
            self.game_entities.position(attacker),
            self.game_entities.body(record).height,
            record.room,
            false,
            riposte.staggered,
            false,
        );
    }

    /// Entity `index` parried the player's hit: she takes the riposte, and
    /// the tell shows as for the player's own parry, the refund over the
    /// entity in the parry colour and break sparks on her.
    // Rare (a parry, crit or knockdown): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn entity_parried_player(&mut self, index: usize, ctx: &Ctx) {
        let Some(record) = GAME_ENTITIES.get(index) else {
            return;
        };
        telemetry::debug_log("enemy parry");
        self.queue_gameplay_sfx(LevelGameplaySfxEvent::Parry);
        self.duel_log_event(ctx, 1, duel::DUEL_EVENT_PARRY);
        let now = self.gameplay_tick(ctx.sim_tick);
        self.damage_numbers.spawn(
            struck_actor_anchor(
                self.game_entities.position(index),
                self.game_entities.body(record).height,
            ),
            record.room,
            self.game_entities.body(record).combat.parry_refund,
            DamageNumberChannel::Parry,
            now,
        );
        let zenith = self.game_entities.stance(index) == VitalityChannelId::Two;
        self.game_entities
            .start_flash(index, &COMBAT_FEEDBACK, FlashKind::Parry, zenith);
        let player = self.game_entities.actor_motor().position();
        self.spawn_parry_burst(
            self.game_entities.position(index),
            self.game_entities.body(record).height,
            Some([player.x, player.y, player.z]),
            record.room,
            zenith,
        );
        // The riposte lands on her through her one landing.
        let staggered = self
            .receive_on_player(
                ctx,
                Strike::parry(
                    CombatantId::entity(index),
                    self.game_entities.actor().stance.active(),
                ),
            )
            .is_some_and(|(outcome, _, _)| outcome.staggered);
        self.spawn_combat_hit_sparks(
            [player.x, player.y, player.z],
            self.character.map_or(0, |c| c.height.max(0) as u16),
            self.room_index,
            false,
            staggered,
            false,
        );
    }

    /// Play `reaction` (decided by the one rule,
    /// [`Posture::reaction`]) on her body as every body plays it: the clip of
    /// its verb, else the first bound stand-in of the verb's chain
    /// ([`Reaction::verb`]), locked for the clip's length (a riposte's
    /// stagger for the stagger's); a knockdown for her `knockdown_ticks`.
    pub(super) fn play_player_reaction(&mut self, reaction: Reaction, ctx: &Ctx) {
        // Player animation starts and locks use the absolute simulation clock.
        // Enemy animation phases use gameplay_tick; using that epoch here makes
        // the reaction appear expired as soon as update_gameplay checks its lock.
        let now = ctx.sim_tick;
        if reaction == Reaction::Knockdown {
            self.knock_player_down(ctx);
            return;
        }
        // Same lock, same rules: only the clip differs.
        let lock = self.player_reaction_anim(if reaction == Reaction::Flinch {
            Reaction::Flinch
        } else {
            Reaction::Stagger
        });
        let anim = self.player_reaction_anim(reaction);
        self.interrupt_player_for_reaction();
        let has_reaction = self.start_player_anim_action(lock, now, ctx.video_hz);
        let recovery = if has_reaction {
            // Follow authored speed and range through the final recovery pose.
            self.game_entities
                .actor_action()
                .lock_until
                .saturating_sub(now)
                .max(1)
        } else {
            // A body that binds no clip for it is still held, as an entity is.
            u32::from(psx_game_runtime::entities::GAME_ENTITY_STAGGER_TICKS)
        };
        if !has_reaction || anim != lock {
            self.switch_player_anim(anim, now, ctx.video_hz);
        }
        self.game_entities.actor_action_mut().lock_until = now.saturating_add(recovery);
        self.anim.switched = None;
        telemetry::debug_log(if reaction == Reaction::Flinch {
            "player hit:react"
        } else {
            "player poise:break"
        });
    }

    /// The state her body shows for `reaction` ([`Reaction::shown`]).
    // Rare (a reaction): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn player_reaction_anim(&self, reaction: Reaction) -> PlayerAnim {
        use psx_level::CharacterAnimationAction as Action;
        let resolved = self
            .character
            .as_ref()
            .and_then(|c| psx_level::combat_verbs::resolve(c, reaction.verb()));
        match reaction.shown(resolved) {
            Action::Stun => PlayerAnim::Stun,
            Action::Parried => PlayerAnim::Parried,
            Action::Knockdown => PlayerAnim::Knockdown,
            _ => PlayerAnim::HitReact,
        }
    }

    /// Drop every action, buffer and aim the player had going: a reaction
    /// owns her from here.
    fn interrupt_player_for_reaction(&mut self) {
        self.game_entities.actor_motor_mut().interrupt_action();
        self.ranged_ready = combat_input::RangedReady::EMPTY;
        self.aim_control = aim_control::AimControl::EMPTY;
        self.game_entities.actor_action_mut().clear_requests();
        self.loco = LocoPhase::Idle;
    }

    /// Knock the player down: the knockdown clip (her stagger clip until one
    /// is authored) held for her `knockdown_ticks`, during which she can
    /// neither act nor move, the same count her [`Combatant`] is down for.
    // Rare (a parry, crit or knockdown): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn knock_player_down(&mut self, ctx: &Ctx) {
        let now = ctx.sim_tick;
        self.interrupt_player_for_reaction();
        let anim = self.player_reaction_anim(Reaction::Knockdown);
        self.switch_player_anim(anim, now, ctx.video_hz);
        self.game_entities.actor_action_mut().lock_until = now.saturating_add(u32::from(
            self.game_entities.actor_stats().knockdown_ticks.max(1),
        ));
        self.anim.switched = None;
        telemetry::debug_log("player poise:knockdown");
    }

    /// The parry burst, in the parrier's stance colour, at the contact point:
    /// halfway between the parrier at `parrier` (feet, `height` tall) and the
    /// attacker at `attacker` (feet; the parrier's own spot when unknown, as
    /// for a bolt with no owner), at the parrier's chest height. Always the
    /// same effect for either side; ordinary hit sparks are unchanged.
    // Rare (a parry): kept out of line and small.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    fn spawn_parry_burst(
        &mut self,
        parrier: [i32; 3],
        height: u16,
        attacker: Option<[i32; 3]>,
        room: RoomIndex,
        zenith: bool,
    ) {
        let other = attacker.unwrap_or(parrier);
        let _ = self.combat_projectile_impacts.spawn_effect(
            [
                parrier[0] / 2 + other[0] / 2,
                parrier[1].saturating_add(i32::from(height) * 5 / 8),
                parrier[2] / 2 + other[2] / 2,
            ],
            room,
            COMBAT_FEEDBACK.burst_radius,
            COMBAT_FEEDBACK.burst_style(zenith),
        );
    }

    /// The tells of a landed hit, the same for either side: a flash on the
    /// struck fighter's model, white-hot cooling into the attacker's stance
    /// colour ([`LandedHit::zenith`]), and, for a melee contact, a small
    /// spark halfway between the fighters at the victim's chest height. A
    /// bolt has its own impact, so it flashes only. A parry never comes
    /// through here, and a parry flash is not replaced by a hit.
    // Out of line and small: it runs on every landed hit, three call sites.
    #[inline(never)]
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn show_hit(&mut self, hit: LandedHit) {
        let (feet, height, room) = if hit.victim == CombatantId::PLAYER {
            self.player_flash
                .begin(&COMBAT_FEEDBACK, hit.kind, hit.zenith);
            let p = self.game_entities.actor_motor().position();
            (
                [p.x, p.y, p.z],
                self.character.map_or(0, |c| c.height.max(0) as u16),
                self.room_index,
            )
        } else {
            let index = usize::from(hit.victim.0);
            let Some(record) = GAME_ENTITIES.get(index) else {
                return;
            };
            self.game_entities
                .start_flash(index, &COMBAT_FEEDBACK, hit.kind, hit.zenith);
            (
                self.game_entities.position(index),
                self.game_entities.body(record).height,
                record.room,
            )
        };
        let Some(from) = hit.contact_from else {
            return;
        };
        let _ = self.combat_projectile_impacts.spawn_effect(
            [
                feet[0] / 2 + from[0] / 2,
                feet[1].saturating_add(i32::from(height) * 5 / 8),
                feet[2] / 2 + from[2] / 2,
            ],
            room,
            COMBAT_FEEDBACK.spark_radius,
            COMBAT_FEEDBACK.spark_style(hit.kind, hit.zenith),
        );
    }

    pub(super) fn spawn_combat_hit_sparks(
        &mut self,
        position: [i32; 3],
        height: u16,
        room: RoomIndex,
        zenith: bool,
        staggered: bool,
        defeated: bool,
    ) {
        let _ = self.combat_projectile_impacts.spawn_effect(
            [
                position[0],
                position[1].saturating_add(i32::from(height) / 2),
                position[2],
            ],
            room,
            if defeated {
                16
            } else if staggered {
                10
            } else {
                6
            },
            psx_game_runtime::combat_feedback::melee_impact_style(zenith, staggered, defeated),
        );
    }

    pub(super) fn lock_player_anim_action(
        &mut self,
        character: &RuntimeCharacter,
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
        self.game_entities.actor_action_mut().hold(now, duration);
        true
    }

    /// Resolve permissions on the same clip phase used by the visible player.
    pub(super) fn player_combat_sample(
        &self,
        ctx: &Ctx,
    ) -> psx_game_runtime::combat_timing::Sample {
        use psx_game_runtime::combat_timing::Sample;
        let Some(character) = self.character.as_ref() else {
            return Sample::LEGACY;
        };
        if !character
            .combat_windows
            .iter()
            .any(|w| w.action == self.anim.state.action().to_index() as u8)
        {
            return Sample::LEGACY;
        }
        let Some(clip) = self
            .models
            .get(character.model.to_usize())
            .copied()
            .flatten()
            .and_then(|model| model.clip(&self.clips, character.clip_for(self.anim.state)))
        else {
            return Sample::LEGACY;
        };
        let phase = psx_game_runtime::model_rendering::animation_phase_at_tick_q12(
            clip,
            ctx.sim_tick
                .saturating_sub(self.game_entities.actor_action().start),
            ctx.video_hz,
            false,
            self.player_action_speed_q8(character, self.anim.state),
            character.action_frame_range(self.anim.state.action()),
        );
        psx_game_runtime::combat_timing::Sample::new(
            &character.combat_windows,
            self.anim.state.action().to_index() as u8,
            phase,
        )
    }

    /// What the player's current action grants this tick: the authored
    /// `Invulnerable` window of the playing clip, else the motor's dodge
    /// i-frames (`roll_invulnerable_frames` motor ticks from its first frame).
    pub(super) fn player_action_protection(
        &self,
        ctx: &Ctx,
        config: CharacterMotorConfig,
    ) -> ActionProtection {
        ActionProtection {
            authored: self
                .player_combat_sample(ctx)
                .active(psx_level::CombatWindowKind::Invulnerable),
            dodge: self
                .game_entities
                .actor_motor()
                .is_action_invulnerable(config),
        }
    }

    /// The player as the target of a hit, asked through the same
    /// [`Defender::invulnerable`] rule as every enemy.
    ///
    /// A stance change bursts the body apart the way the dodge does, so the
    /// swap window is the character's `roll_invulnerable_frames`, the same
    /// field an enemy's swap window reads from its own record. The swap is
    /// pressed before the stance clock advances in the same update, so
    /// contact resolution first sees elapsed = 1: `1..=frames` is `frames`
    /// ticks, the dodge's count, press tick included.
    pub(super) fn player_defender(&self, ctx: &Ctx) -> Defender<'_> {
        let config = self.motor_config();
        Defender {
            combatant: &self.game_entities.actor(),
            swap_invulnerable_ticks: config.roll_invulnerable_frames,
            parry_ticks: self.game_entities.actor_stats().parry_ticks,
            action: self.player_action_protection(ctx, config),
        }
    }

    /// The combat kits the player character binds, which decide what she
    /// may do under the same rules as every enemy ([`CombatKits`]): strike
    /// with the melee kit, shoot with the ranged kit, dodge with the
    /// mobility kit, swap with both weapons. A model-less debug body binds
    /// nothing but keeps its dodge.
    pub(super) fn player_kits(&self) -> CombatKits {
        match &self.character {
            Some(character) => character.combat_kits(),
            None => CombatKits::from_bits(CombatKits::MOBILITY),
        }
    }

    /// Whether enemy melee and projectiles pass through the player this tick.
    pub(super) fn player_invulnerable(&self, ctx: &Ctx) -> bool {
        self.player_defender(ctx).invulnerable()
    }

    /// The player's motor configuration: her character's, under the game's
    /// fighter rules, her boosts and abilities.
    // Read from a dozen places a tick; one copy on the PS1.
    #[inline(never)]
    pub(super) fn motor_config(&self) -> CharacterMotorConfig {
        let mut config = match &self.character {
            Some(c) => c.motor_config(),
            None => CharacterMotorConfig::character(
                0,
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                scaled_player_speed(FALLBACK_PLAYER_SPEED),
                FALLBACK_PLAYER_YAW_STEP,
            ),
        };
        config = fighter_motor_rules(config, self.room_index, self.character.is_some());
        // Characterless projects use the cooker's matching debug body.
        // Authored characters keep their Character-bound radius and height
        // instead of silently shrinking the player.
        apply_debug_body_fallback(
            &mut config,
            self.world_rt.is_some(),
            self.character.is_some(),
        );
        let modifiers = self.vitality_modifiers();
        config.walk_speed = modifiers.movement_speed(config.walk_speed);
        config.run_speed = modifiers.movement_speed(config.run_speed);
        // Sprinting is something the player has to have been given: the one
        // gate every sprint decision of the motor passes through.
        config.run_enabled &= self.abilities().sprint();
        config
    }

    /// What the player can do now: the abilities the level starts them with
    /// and the ones they have since earned (the save keeps those).
    pub(super) fn abilities(&self) -> psx_game_runtime::abilities::PlayerAbilities {
        psx_game_runtime::abilities::PlayerAbilities::effective(
            WORLD_START_ABILITIES.get(),
            self.poi_save.abilities,
        )
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
        self.collect_collision_blockers_near_into(out, None)
    }

    /// [`Self::collect_collision_blockers_into`], leaving out the cylinders
    /// that cannot touch a body moving from `near` this tick (farther than
    /// [`BLOCKER_REACH`] beyond their radius) and away fighters' bodies.
    pub(super) fn collect_collision_blockers_near_into<
        S: psx_engine::BoundedSink<CharacterCollisionCylinder>,
    >(
        &self,
        out: &mut S,
        near: Option<RoomPoint>,
    ) -> usize {
        /// More than any body moves in one tick, its own radius included.
        const BLOCKER_REACH: i32 = 4096;
        let mut count = 0usize;
        // Present instances only: an away fighter's body blocks no one.
        for &(index, owner) in self.present_instances.entries() {
            let inst = &MODEL_INSTANCES[usize::from(index)];
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
            let center = match GAME_ENTITIES
                .get(usize::from(owner))
                .map(|_| usize::from(owner))
            {
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
            if let Some(near) = near {
                let reach = radius.saturating_add(BLOCKER_REACH);
                if (center.x - near.x).abs() > reach || (center.z - near.z).abs() > reach {
                    continue;
                }
            }
            if !out.try_push(CharacterCollisionCylinder::new(center, radius, height)) {
                return count;
            }
            count += 1;
        }
        count
            + psx_game_runtime::cylinder_props::collect_cylinder_prop_collision_blockers_into(
                CYLINDER_PROPS.as_slice(),
                self.room_index,
                out,
            )
    }

    #[cfg(feature = "collision-debug-overlay")]
    pub(super) fn draw_collision_debug_overlay(&self, gpu: &mut Gpu, camera: WorldCamera) {
        if let Some(character) = self.character.as_ref() {
            draw_collision_cylinder_debug(
                gpu,
                self.game_entities.actor_motor().position(),
                character.radius,
                character.height,
                camera,
                (0x40, 0xd8, 0xff),
            );
        }

        if self.world_rt.is_some() {
            for inst in MODEL_INSTANCES.iter() {
                if inst.room != self.room_index {
                    continue;
                }
                let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                    continue;
                };
                draw_collision_cylinder_debug(
                    gpu,
                    RoomPoint::new(inst.x, inst.y, inst.z),
                    i32::from(model.collision_radius),
                    i32::from(model.world_height),
                    camera,
                    (0xff, 0xd0, 0x40),
                );
            }
        }
    }

    #[inline(never)]
    pub(super) fn draw_particle_emitters<'a>(
        &self,
        camera: WorldCamera,
        elapsed_tick: SimTick,
        ot: &mut OtFrame<'a, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'a>,
    ) -> usize {
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let mut submitted = 0usize;
        let depth_range = self.effect_depth_range(self.room_index);
        let mut projector = None;
        let switches = self.visual_switch_mask();
        for emitter in &PARTICLE_EMITTERS {
            if emitter.room != self.room_index
                || !psx_grid::visual_switch::shown(emitter.visual, switches)
            {
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
            // A cooked sprite still uploading skips this frame rather than
            // flashing the built-in disc.
            let sprite = match emitter.texture_asset {
                Some(texture) => match prop_texture_slot(texture) {
                    Some(slot) => psx_game_runtime::particles::ParticleSprite::from_slot(slot),
                    None => continue,
                },
                None => psx_game_runtime::particles::ParticleSprite::builtin(particle_material),
            };
            submitted += draw_particle_emitter(
                *emitter,
                camera,
                loaded_projector,
                depth_range,
                sprite,
                elapsed_tick,
                ot,
                primitive_packets,
            );
        }
        submitted
    }

    /// Draw live combat bolts after world submission so their additive quads
    /// participate in the same depth table as authored particle effects.
    #[inline(never)]
    pub(super) fn draw_combat_projectiles<'a>(
        &self,
        camera: WorldCamera,
        ot: &mut OtFrame<'a, OT_DEPTH>,
        primitive_packets: &mut PrimitivePacketArena<'a>,
    ) -> usize {
        let Some(particle_material) = self.particle_material else {
            return 0;
        };
        let mut submitted = 0usize;
        // A named eye socket follows the same sampled head as the visible mesh.
        // Models without an authored eye simply omit this presentation effect.
        for (index, record) in GAME_ENTITIES.iter().enumerate() {
            let Some(progress) = self.game_entities.stance_eye_pulse_q12(index) else {
                continue;
            };
            let room = record.room;
            if room != self.room_index {
                continue;
            }
            let room_camera = camera;
            let Some(snapshot) = self
                .instance_actor_poses
                .get(usize::from(record.model_instance))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            let model = snapshot.model();
            let first = model.socket_first.to_usize();
            let Some(socket) = MODEL_SOCKETS
                .get(first..first + usize::from(model.socket_count))
                .and_then(|sockets| sockets.iter().find(|socket| socket.name == "stance_eye"))
            else {
                continue;
            };
            let Some(eye) = snapshot
                .pose()
                .joint_world_point(socket.joint, socket.translation)
            else {
                continue;
            };
            let range = self.effect_depth_range(room);
            let clearance = -current_actor_surface_options(room).depth_bias + 2;
            submitted += psx_game_runtime::particles::draw_stance_eye_pulse(
                eye,
                progress,
                stance_rgb(self.game_entities.stance(index)),
                room_camera,
                DepthRange::new(range.near() + clearance, range.far() + clearance),
                particle_material,
                ot,
                primitive_packets,
            );
        }
        // Charge flares are sampled from the same retained pose token as the
        // eventual release, so the animated muzzle and presentation cannot
        // drift apart even when NPC simulation runs below source clip rate.
        if self.ranged_ready.firing(
            self.overlay_sim_tick
                .as_u32()
                .wrapping_add(self.gameplay_epoch.as_u32()),
        ) && self.ranged_ready.released == 0
        {
            if let (Some(character), Some(pose)) =
                (self.character.as_ref(), self.player_actor_pose.as_ref())
            {
                let first = character.combat_capsule_first.to_usize();
                let capsules = COMBAT_CAPSULES
                    .get(first..first + usize::from(character.combat_capsule_count))
                    .unwrap_or(&[]);
                if let Some(mut charge) = psx_game_runtime::combat::authored_projectile_charge(
                    capsules,
                    CharacterAnimationAction::RangedAttack,
                    Some(pose.pose()),
                ) {
                    charge.visual.core_rgb = [224, 255, 248];
                    charge.visual.glow_rgb = [64, 208, 168];
                    let range = self.effect_depth_range(self.room_index);
                    let clearance = -current_actor_surface_options(self.room_index).depth_bias
                        + i32::from(charge.radius.max(2));
                    submitted += draw_projectile_charge(
                        charge,
                        camera,
                        None,
                        DepthRange::new(range.near() + clearance, range.far() + clearance),
                        particle_material,
                        ot,
                        primitive_packets,
                    );
                }
            }
        }
        let mut attack_index = 0usize;
        while attack_index < self.deferred_enemy_attacks.len() {
            let Some(attack) = self.deferred_enemy_attacks.get(attack_index) else {
                break;
            };
            attack_index += 1;
            let Some(entity) = GAME_ENTITIES.get(attack.entity()) else {
                continue;
            };
            if !attack.is_ranged() {
                continue;
            }
            let capsule_body = self.game_entities.body(entity);
            let first = capsule_body.combat_capsule_first.to_usize();
            let end = first.saturating_add(usize::from(capsule_body.combat_capsule_count));
            let capsules = COMBAT_CAPSULES.get(first..end).unwrap_or(&[]);
            let pose = self
                .instance_actor_poses
                .get(entity.model_instance as usize)
                .and_then(Option::as_ref)
                .map(|snapshot| snapshot.pose());
            let Some(mut charge) = psx_game_runtime::combat::authored_projectile_charge(
                capsules,
                attack.action(),
                pose,
            ) else {
                continue;
            };
            if attack.room() != self.room_index {
                continue;
            }
            let room_camera = camera;
            charge.visual = crate::game_logic_runtime::enemy_projectile_visual(charge.visual);
            let range = self.effect_depth_range(attack.room());
            let clearance = -current_actor_surface_options(attack.room()).depth_bias
                + i32::from(charge.radius.max(2));
            let depth_range = DepthRange::new(range.near() + clearance, range.far() + clearance);
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
            if projectile.room != self.room_index {
                index += 1;
                continue;
            }
            let (room_camera, depth_range) = (camera, self.effect_depth_range(projectile.room));
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
            if impact.room != self.room_index {
                impact_index += 1;
                continue;
            }
            let (room_camera, depth_range) = (camera, self.effect_depth_range(impact.room));
            // Use the same half-sector clearance as actor meshes. A hit on a
            // large floor/wall polygon also needs this allowance for its centre
            // sort key. Effects still sort in world depth, never the HUD band.
            let depth_range = if impact.visual.crystal || impact.visual.star_points != 0 {
                let clearance = -current_actor_surface_options(impact.room).depth_bias
                    + i32::from(impact.radius.max(2));
                DepthRange::new(
                    depth_range.near() + clearance,
                    depth_range.far() + clearance,
                )
            } else {
                depth_range
            };
            let effect_submitted = draw_projectile_impact(
                impact,
                room_camera,
                None,
                depth_range,
                particle_material,
                ot,
                primitive_packets,
            );
            submitted += effect_submitted;
            impact_index += 1;
        }
        submitted
    }

    /// Gameplay-anchored animation tick: raw sim ticks minus the epoch
    /// captured at the first gameplay update. Value-based animation
    /// phases (ambient models, particles, atmosphere, HUD pulse) use
    /// this so visuals are a pure function of gameplay time instead of
    /// inheriting the build- and disc-dependent loading duration.
    pub(super) fn gameplay_tick(&self, now: SimTick) -> SimTick {
        SimTick::from_u32(now.as_u32().wrapping_sub(self.gameplay_epoch.as_u32()))
    }

    /// The pulsing weapon tint while her parry's crit is armed (presentation
    /// only), in her stance colour; `None` otherwise.
    pub(super) fn player_armed_glow(&self, now: SimTick) -> Option<(u8, u8, u8)> {
        let actor = self.game_entities.actor();
        (actor.crit != 0).then(|| {
            COMBAT_FEEDBACK.armed_glow_rgb(
                actor.stance.active() == VitalityChannelId::Two,
                self.gameplay_tick(now).as_u32(),
            )
        })
    }

    pub(super) fn camera_config(&self) -> ThirdPersonCameraConfig {
        if self.world_rt.is_some() && self.character.is_none() {
            let mut config = ThirdPersonCameraConfig::character(
                WORLD_FALLBACK_CAMERA_DISTANCE,
                WORLD_FALLBACK_CAMERA_HEIGHT,
                WORLD_FALLBACK_CAMERA_TARGET_HEIGHT,
            );
            config.min_floor_clearance = WORLD_FALLBACK_CAMERA_CLEARANCE;
            config.collision_margin = WORLD_FALLBACK_CAMERA_MARGIN;
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
        if self.world_rt.is_some() {
            // Keep a small authored gap between the point-traced camera and
            // walls. The renderer clips any remaining near-plane
            // intersection exactly.
            config.collision_margin = WORLD_CAMERA_WALL_MARGIN;
        }
        config.recenter_preserves_pitch = camera.recenter_preserves_pitch;
        config.fov_y_degrees = camera.fov_y_degrees;
        config.blend_profiles = camera.blend_profiles;
        config.lock_target_framing = camera.lock_target_framing;
        if camera.lock_target_framing {
            // Native ordinary orbit limits, converted from degrees to Q12 turns.
            config.pitch_min_q12 = -455; // -40 degrees
            config.pitch_max_q12 = 796; // +70 degrees
        }
        config.lock_profile =
            camera
                .lock_profile
                .map(|profile| psx_engine::ThirdPersonCameraProfile {
                    distance: profile.distance,
                    height: profile.height,
                    target_height: profile.target_height,
                    fov_y_degrees: profile.fov_y_degrees,
                    shoulder_offset: profile.shoulder_offset,
                });
        config.max_distance = config
            .max_distance
            .max(camera.lock_profile.map_or(0, |p| p.distance));
        if camera.accelerated_orbit {
            config.accelerated_orbit_speed = Some(camera.orbit_speed_level);
            config.manual_cooldown_frames = 120;
            config.manual_release_frames = 60;
        }
        config.position_lag_shift = camera.position_lag_shift;
        config.position_vertical_lag_shift = Some(camera.position_vertical_lag_shift);
        config.focus_lag_shift = camera.focus_lag_shift;
        config.focus_vertical_lag_shift = Some(camera.focus_vertical_lag_shift);
        config.distance_lag_shift = camera.distance_lag_shift;
        config.collision_solve_interval = CAMERA_COLLISION_SOLVE_INTERVAL;
        if self.player_has_ranged_weapon() {
            // Aim overrides composition, never the persistent target. Reversing
            // L2 changes the destination of the same live blend.
            config.blend_profiles = true;
            config.profile_response_q12 = 1024;
            config.preserve_profile_pitch = true;
            if self.ranged_ready.aiming() {
                // Sky arches need an upward sightline; free and locked profiles stay authored.
                config.pitch_min_q12 = -796;
                config.pitch_max_q12 = 796;
                config.composition_override = Some(psx_engine::ThirdPersonCameraProfile {
                    distance: camera.distance * 10 / 13,
                    height: camera.target_height + 16,
                    target_height: camera.target_height,
                    shoulder_offset: camera.distance * 3 / 26,
                    fov_y_degrees: camera.fov_y_degrees,
                });
                config.lock_frame_percent = 0;
                config.lock_target_framing = true;
                config.auto_align_when_moving = false;
                config.accelerated_orbit_speed = None;
            }
        }
        config
    }

    pub(super) fn camera_target(
        &self,
        lock_target: Option<RoomPoint>,
        moving: bool,
    ) -> ThirdPersonCameraTarget {
        ThirdPersonCameraTarget {
            player: self.game_entities.actor_motor().position(),
            player_yaw: self.game_entities.actor_motor().yaw(),
            moving,
            lock_target,
        }
    }

    pub(super) fn current_room_lighting(
        &self,
        camera: WorldCamera,
    ) -> Option<RuntimeRoomLighting<'_>> {
        room_lighting_of(
            &self.world_rt,
            self.room_index,
            self.current_ambient_rgb,
            camera,
        )
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
    }

    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn update_follow_camera(&mut self, ctx: &Ctx) -> WorldCamera {
        #[cfg(feature = "sim-trace")]
        self.camera.pin_throttle_phase(
            ctx.sim_tick.as_u32(),
            self.camera_config().collision_solve_interval,
        );
        let mut camera = self.solve_follow_camera(ctx);
        let age = self
            .hook_travel
            .map(|flight| ctx.sim_tick.as_u32().wrapping_sub(flight.started));
        self.hook_fov_delta_q8 = psx_game_runtime::hook_points::ease_lens_q8(
            self.hook_fov_delta_q8,
            psx_game_runtime::hook_points::lens_target_q8(age),
        );
        // Apply after profile solving so all free/locked user settings remain the baseline.
        camera.projection.focal_length =
            (camera.projection.focal_length * (256 + self.hook_fov_delta_q8) / 256).max(1);
        #[cfg(feature = "emulator-telemetry")]
        if self.player_has_ranged_weapon() && ctx.sim_tick.every(6) {
            let [x, y] = self.aim_control.angles();
            let [tx, ty, tz] = self.ranged_target();
            crate::debug_runtime::debug_log_aim_camera([
                ctx.sim_tick.as_u32() as i32,
                i32::from(self.ranged_ready.aiming()),
                i32::from(self.is_locked()),
                i32::from(x),
                i32::from(y),
                camera.position.x,
                camera.position.y,
                camera.position.z,
                self.camera.distance(),
                i32::from(self.camera.pitch_q12()),
                tx,
                ty,
                tz,
                self.lock_target.map_or(-1, |index| index as i32),
            ]);
        }
        #[cfg(feature = "camera-pose-log")]
        {
            let player = self.game_entities.actor_motor().position();
            let eye = self.camera.position();
            let focus = self.camera.focus();
            let (right_x, _) = ctx.pad.sticks.right_centered();
            crate::debug_runtime::debug_log_camera_pose([
                ctx.sim_tick.as_u32() as i32,
                player.x,
                player.y,
                player.z,
                eye.x,
                eye.y,
                eye.z,
                focus.x,
                focus.y,
                focus.z,
                i32::from(self.camera.yaw().as_q12()),
                i32::from(self.camera.pitch_q12()),
                self.camera.distance(),
                i32::from(self.camera.collision_pull_in()),
                i32::from(right_x),
                i32::from(self.camera.lift_pitch_q12()),
                i32::from(self.camera.lift_goals_q12().0),
                i32::from(self.camera.lift_goals_q12().1),
                i32::from(self.camera.slide_yaw_q12()),
                self.world_rt
                    .as_ref()
                    .map_or(0, |world_rt| world_rt.camera_traces() as i32),
                i32::from(self.camera.escape_goal_q12()),
            ]);
        }
        #[cfg(feature = "emulator-telemetry")]
        if ctx.sim_tick.every(30) {
            debug_log_camera_profile(
                ctx.sim_tick.as_u32(),
                camera.projection.focal_length,
                self.camera.distance(),
                self.is_locked(),
                self.camera.focus().y,
                self.lock_target
                    .or(self.soft_lock_target)
                    .and_then(|index| self.camera_target_anchor(index))
                    .map(|point| point.y),
            );
        }
        camera
    }

    fn solve_follow_camera(&mut self, ctx: &Ctx) -> WorldCamera {
        if self
            .aim_control
            .camera_transition(self.ranged_ready.aiming(), self.is_locked())
        {
            self.camera.release_lock_preserving_view();
        }
        let mut config = self.camera_config();
        let mut input = if self.is_locked() {
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                // Shoulder inputs remain exclusively owned by combat even
                // while hard locked.
                recenter: false,
            }
        } else {
            camera_input(
                ctx,
                self.camera_orbit_speed_level(),
                self.analog_deadzone,
                config.accelerated_orbit_speed.is_some(),
            )
        };
        input.recenter |= core::mem::take(&mut self.camera_recenter_requested);
        if self.ranged_ready.aiming() && !self.is_locked() {
            // Deliberate aiming uses a lower, unaccelerated stick rate.
            let (x, y) = camera_stick_axes(ctx, self.analog_deadzone);
            input.yaw_delta_q12 = (-i32::from(x) * 12 / 128) as i16;
            input.pitch_delta_q12 = (i32::from(y) * 10 / 128) as i16;
            input.recenter = false;
        }
        if let Some(flight) = self.hook_travel {
            config.accelerated_orbit_speed = None;
            config.auto_align_when_moving = false;
            config.preserve_profile_pitch = true;
            config.position_lag_shift = 0;
            config.position_vertical_lag_shift = Some(0);
            config.focus_lag_shift = 0;
            config.focus_vertical_lag_shift = Some(0);
            // Align during anticipation, then hold exactly behind the route.
            // Both axes ignore stick/recenter input until flight is finished.
            let yaw_error = self
                .camera
                .yaw()
                .shortest_delta_q12(flight.camera_yaw(self.camera.yaw()));
            input.yaw_delta_q12 = if ctx.sim_tick.as_u32().wrapping_sub(flight.started)
                < psx_game_runtime::hook_points::LAUNCH_TICKS
            {
                yaw_error.clamp(-96, 96)
            } else {
                yaw_error
            };
            input.pitch_delta_q12 = self.hook_camera_pitch - self.camera.pitch_q12();
            input.recenter = false;
        }
        let lock_target = if self.ranged_ready.aiming() {
            self.lock_target_indicator_position().map(|_| {
                let [x, y, z] = self.ranged_target();
                RoomPoint::new(x, y, z)
            })
        } else if config.lock_target_framing {
            self.lock_target
                .or(self.soft_lock_target)
                .and_then(|index| self.camera_target_anchor(index))
        } else {
            self.lock_target_position()
                .or_else(|| self.soft_lock_target_position())
        };
        let target = self.camera_target(lock_target, self.anim.state != PlayerAnim::Idle);
        let mut prop_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self.world_rt.is_some()
            && self
                .collect_static_prop_aabb_blockers_checked_into(&mut prop_blockers)
                .is_none()
        {
            // Invalid generated prop collision freezes the existing camera
            // instead of treating the obstructed boom as clear.
            return world_camera_from_position_focus(
                self.camera.projection(PROJECTION),
                self.camera.position(),
                self.camera.focus(),
            );
        }
        let Some(world_rt) = self.world_rt.as_mut() else {
            // No world cooked yet (the placeholder manifest): nothing to
            // collide with, so hold the current view.
            return world_camera_from_position_focus(
                self.camera.projection(PROJECTION),
                self.camera.position(),
                self.camera.focus(),
            );
        };
        world_rt
            .update_camera(
                &mut self.camera,
                target,
                input,
                config,
                1,
                prop_blockers.as_slice(),
                &self.destructibles,
            )
            .expect("grid camera trace failed")
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

    /// Gameplay entity currently driving combat readability. A deliberate
    /// hard lock wins; otherwise reuse the camera's existing soft lock. This
    /// is the single target authority for the enemy HUD and stance tell.
    pub(super) fn combat_target_entity_index(&self) -> Option<usize> {
        let model_instance = self.lock_target.or(self.soft_lock_target)?;
        self.target_position(model_instance)?;
        game_entity_for_instance(u16::try_from(model_instance).ok()?)
    }

    /// Camera-only anchor: three quarters of the scaled model's standing height.
    /// Gameplay targeting continues to use the live root position.
    fn camera_target_anchor(&self, index: usize) -> Option<RoomPoint> {
        let instance = MODEL_INSTANCES.get(index)?;
        let root = self.target_position(index)?;
        let height = MODELS.get(instance.model.to_usize()).map(|model| {
            i32::from(model.world_height) * i32::from(instance.visual_scale_q8) / 256
        })?;
        Some(root.with_y(root.y.saturating_add(height.saturating_mul(3) / 4)))
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
            .live_position_for_model_instance(GAME_ENTITIES.as_slice(), instance)?;
        Some(RoomPoint::new(live[0], live[1], live[2]))
    }

    pub(super) fn refresh_active_interactable(&mut self) {
        self.active_interactable = self.find_best_interactable();
    }

    pub(super) fn find_best_interactable(&mut self) -> Option<usize> {
        let player = self.game_entities.actor_motor().position();
        let mut best = None;
        let mut best_distance = u32::MAX;
        for (index, interactable) in INTERACTABLES.iter().enumerate() {
            if !self.point_of_interest_available(interactable)
                || interactable.room != self.room_index
                // A spent one-shot lever offers no prompt.
                || (interactable.kind == InteractableKind::Switch
                    && self.logic.is_removed(usize::from(interactable.logic)))
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
            // A point of interest has a cooked beacon to see. Every other kind
            // (lever, message, checkpoint) has no beacon: radius plus a clear
            // line through the grid collision, chest to chest, so a prompt
            // never shows through a wall or a closed door.
            if let Some(world_rt) = self.world_rt.as_mut() {
                let seen = if interactable.kind == InteractableKind::PointOfInterest {
                    world_rt.typed_world_object_directly_visible(
                        player,
                        psx_level::world_object_kind::POINT_OF_INTEREST_BEACON,
                        index,
                        &self.destructibles,
                    )
                } else {
                    world_rt.sight_clear(
                        [
                            player.x,
                            player.y.saturating_add(INTERACT_SIGHT_LIFT),
                            player.z,
                        ],
                        [
                            interactable.x,
                            interactable.y.saturating_add(INTERACT_SIGHT_LIFT),
                            interactable.z,
                        ],
                    )
                };
                if !seen {
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
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn activate_interactable(
        &mut self,
        index: usize,
        now: u32,
        port: &mut psx_engine::ControllerPort,
    ) -> bool {
        let Some(interactable) = INTERACTABLES.get(index) else {
            return false;
        };
        if !self.point_of_interest_available(interactable) {
            return false;
        }
        if interactable.logic != psx_level::INTERACTABLE_LOGIC_NONE {
            let fired = self.logic.fire_index(
                LOGIC.as_slice(),
                usize::from(interactable.logic),
                psx_game_runtime::logic::UseCode::Toggle,
                now,
            );
            if fired {
                self.dispatch_logic_effects(port);
                if interactable.kind == InteractableKind::Checkpoint {
                    self.rest_at_checkpoint(port);
                }
            }
            return fired;
        }
        match interactable.kind {
            // A lever without its paired record (a hand-rolled manifest)
            // has nothing to fire.
            InteractableKind::Switch => false,
            InteractableKind::Message => {
                self.open_interactable_message(interactable);
                true
            }
            InteractableKind::Checkpoint => {
                self.set_checkpoint(
                    RuntimeCheckpoint {
                        room: self.room_index,
                        position: self.game_entities.actor_motor().position(),
                        yaw: self.game_entities.actor_motor().yaw(),
                        checkpoint_id: interactable.checkpoint_id,
                    },
                    port,
                );
                self.rest_at_checkpoint(port);
                self.open_interactable_message(interactable);
                true
            }
            InteractableKind::PointOfInterest => {
                // A successful pickup is already a complete interaction. Do
                // not make the player acknowledge an authored recovery page
                // before showing the useful confirmation. Lore-only POIs and
                // rewards that could not be granted still use their pages.
                if let Some(module) = self.grant_point_of_interest_reward(index) {
                    if poi_persistent_flags(interactable).mark_read(&mut self.poi_save) {
                        self.poi_save_dirty = true;
                    }
                    self.acquired_module = module;
                    self.set_poi_presentation_frames(
                        psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES,
                        0,
                    );
                    return true;
                }
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
                    self.set_poi_presentation_frames(0, 0);
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
    #[cfg_attr(target_arch = "mips", optimize(size))]
    pub(super) fn set_checkpoint(
        &mut self,
        checkpoint: RuntimeCheckpoint,
        port: &mut psx_engine::ControllerPort,
    ) {
        if self.checkpoint != Some(checkpoint) {
            telemetry::counter(telemetry::counter::PLAYER_CHECKPOINT_ACTIVATIONS, 1);
        }
        self.checkpoint = Some(checkpoint);
        // The save names the checkpoint by its stable id (the hash of its
        // authored id), never by a table index.
        let stable = psx_level::StableId::of_name(checkpoint.checkpoint_id).raw();
        if self.poi_save.checkpoint_id != stable {
            self.poi_save.checkpoint_id = stable;
            self.poi_save_dirty = true;
        }
        // The rest pose is what Continue restores.
        let position = checkpoint.position;
        self.poi_save_dirty |= self.poi_save.set_resume_position(SavedPlayerPosition {
            x: position.x,
            y: position.y,
            z: position.z,
            yaw: checkpoint.yaw.as_q12(),
        });
        // Checkpoints are the genre-visible save boundary. POI interaction
        // stays smooth; its read/reward state reaches the card here instead.
        self.flush_poi_save(port);
    }

    pub(super) fn open_interactable_message(&mut self, interactable: &InteractableRecord) {
        if self.poi_messages.active().is_some() {
            return;
        }
        let (title, body) = interactable_message_text(interactable);
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
        self.set_poi_page_type_frame(0);
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
            .map(|model| i32::from(model.world_height) * i32::from(target.visual_scale_q8) / 256)
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
        distance_xz_sq(self.game_entities.actor_motor().position(), target)
            <= square_i32_saturating(range)
    }

    pub(super) fn find_best_lock_target(&self, range: i32) -> Option<usize> {
        self.find_lock_target(range, self.ranged_ready.aiming())
    }

    // Rare (a lock press or a target switch): keep it out of the gameplay
    // update, which otherwise inlines it whole (1.6 KB on the PS1).
    #[inline(never)]
    pub(super) fn find_lock_target(&self, range: i32, aiming: bool) -> Option<usize> {
        let player = self.game_entities.actor_motor().position();
        let view_yaw = self.camera.yaw().add(Angle::HALF);
        let range_sq = square_i32_saturating(range);
        let mut best: Option<(usize, i32)> = None;
        for (index, _) in MODEL_INSTANCES.iter().enumerate() {
            // Use the same cooked visibility set that gates instance
            // rendering. A longer lock range must not make actors behind a
            // wall targetable merely because they share the active room.
            if index >= u16::BITS as usize || self.instance_visible_mask & (1u16 << index) == 0 {
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
            if aiming {
                let Some(projected) = self
                    .target_indicator_position(index)
                    .and_then(|p| self.render_camera.project_world(p))
                else {
                    continue;
                };
                let dx =
                    i32::from(projected.sx) - i32::from(self.render_camera.projection.screen_x);
                let dy =
                    i32::from(projected.sy) - i32::from(self.render_camera.projection.screen_y);
                if dx.abs() > 100 || dy.abs() > 80 {
                    continue;
                }
                let score = -(dx * dx + dy * dy);
                if best.is_none_or(|(_, previous)| score > previous) {
                    best = Some((index, score));
                }
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
        let player = self.game_entities.actor_motor().position();
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
            if index >= u16::BITS as usize || self.instance_visible_mask & (1u16 << index) == 0 {
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

/// Live authored playback rate for an animation, Q8. Only the four attack
/// actions use the module Attack Speed lane; locomotion and defensive actions
/// retain their authored cadence.
///
/// Takes already-resolved modifiers so a caller that needs several speeds at
/// once pays for [`Playtest::vitality_modifiers`] a single time.
pub(super) fn live_action_speed_q8(
    modifiers: VitalityModifiers,
    character: &RuntimeCharacter,
    anim: PlayerAnim,
) -> u16 {
    let authored = character.action_speed(anim.action());
    if player_anim_is_attack(anim) {
        modifiers.attack_speed_q8(authored)
    } else {
        authored
    }
}

#[cfg(test)]
mod combat_baseline_tests;

#[cfg(test)]
mod combat_hit_fx_tests;

#[cfg(test)]
mod combat_parry_tests;

#[cfg(test)]
mod combat_controller_tests;

#[cfg(test)]
mod combat_mover_tests;

#[cfg(test)]
mod combat_verbs_tests;

#[cfg(test)]
mod life_reset_tests {
    use super::*;
    const PLAYER_MAX_HEALTH: u16 = CombatStats::DEFAULT.max_health;
    use psx_game_runtime::fighter_anim::AnimSwitch;
    use psx_game_runtime::projectiles::{CombatTeam, ProjectileSpawn};

    fn test_scene() -> std::boxed::Box<Playtest> {
        let layout = std::alloc::Layout::new::<Playtest>();
        let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
        assert!(!raw.is_null());
        unsafe { Playtest::init_zeroed(raw) };
        unsafe { std::boxed::Box::from_raw(raw) }
    }

    #[test]
    fn sprint_needs_the_ability_and_the_motor_obeys() {
        use psx_level::ability;
        let mut scene = test_scene();
        // The fixture level starts the player with nothing.
        assert!(!scene.abilities().sprint() && !scene.abilities().dash());
        assert!(
            !scene.motor_config().run_enabled,
            "holding Circle cannot sprint yet"
        );
        // A fragment grants sprint, for good, through the save.
        assert!(scene.poi_save.grant_abilities(ability::SPRINT));
        assert!(scene.abilities().sprint() && !scene.abilities().dash());
        assert!(scene.motor_config().run_enabled);
        // Starting a new game forgets what was earned.
        scene.poi_save = SaveBlock::new(PLAYER_MAX_HEALTH, PERSISTENT_FLAG_COUNT);
        assert!(!scene.motor_config().run_enabled);
    }

    #[test]
    fn a_tap_on_circle_dashes_only_once_dash_is_unlocked() {
        use psx_level::ability;
        let mut scene = test_scene();
        // Press, then release a few ticks later.
        let tap = |scene: &mut Playtest| {
            let held = scene.evade_run_intent(true, true, false, 1);
            assert!(
                held.sprint && !held.evade,
                "holding asks to sprint, never to dash"
            );
            scene.evade_run_intent(false, false, true, 1)
        };
        assert!(!tap(&mut scene).evade, "locked: the tap does nothing");
        assert_eq!(
            scene.game_entities.actor_action().evade_buffer,
            0,
            "and nothing is buffered"
        );
        // Sprint alone does not unlock the dash.
        scene.poi_save.grant_abilities(ability::SPRINT);
        assert!(!tap(&mut scene).evade);
        scene.poi_save.grant_abilities(ability::DASH);
        assert!(tap(&mut scene).evade, "unlocked: the same tap dashes");
    }

    #[test]
    fn a_long_hold_is_a_sprint_not_a_dash_whether_or_not_it_is_unlocked() {
        use psx_level::ability;
        let mut scene = test_scene();
        scene.poi_save.grant_abilities(ability::DASH);
        scene.evade_run_intent(true, true, false, 1);
        for _ in 0..EVADE_RUN_HOLD_VBLANKS {
            scene.evade_run_intent(false, true, false, 1);
        }
        assert!(!scene.evade_run_intent(false, false, true, 1).evade);
    }

    #[test]
    fn an_ability_reward_is_granted_once_with_no_item_and_survives_the_card() {
        use psx_level::ability;
        let mut scene = test_scene();
        assert!(!scene.abilities().sprint());
        // The reward is a record's bits; claim them the way a pickup does.
        let persistence = psx_game_runtime::poi::PoiPersistentFlags { read: 0, reward: 0 };
        assert!(!persistence.reward_claimed(&scene.poi_save));
        assert!(scene.poi_save.grant_abilities(ability::SPRINT));
        assert!(persistence.mark_reward_claimed(&mut scene.poi_save));
        scene.announce_unlocked_abilities(ability::SPRINT);
        assert_eq!(scene.message_overlay.unwrap().title, "SPRINT");
        let mut frame = [0u8; psx_game_runtime::save::SAVE_BLOCK_BYTES];
        scene.poi_save.encode(&mut frame);
        let back = SaveBlock::decode(&frame, scene.poi_save.flag_count).unwrap();
        assert!(back.abilities & ability::SPRINT != 0);
    }

    #[test]
    fn walk_windup_hands_off_without_blending_a_frozen_endpoint() {
        let mut scene = test_scene();
        scene.anim.state = PlayerAnim::Walk;
        scene.anim.switched = Some(AnimSwitch {
            from: PlayerAnim::WalkWindup,
            local_tick: 40,
            at: SimTick::from_u32(100),
        });
        assert!(scene.player_anim_blend(SimTick::from_u32(100)).is_none());
        assert!(scene.player_anim_blend(SimTick::from_u32(102)).is_none());

        // An interrupted stop still blends when movement resumes.
        scene.anim.switched = Some(AnimSwitch {
            from: PlayerAnim::WalkWinddown,
            local_tick: 12,
            at: SimTick::from_u32(100),
        });
        assert!(scene.player_anim_blend(SimTick::from_u32(100)).is_some());
    }

    #[test]
    fn stopping_a_gait_blends_from_the_release_pose_without_more_steps() {
        let mut scene = test_scene();
        for gait in [
            PlayerAnim::Walk,
            PlayerAnim::Run,
            PlayerAnim::StrafeLeft,
            PlayerAnim::WalkBackward,
        ] {
            for stop in [
                PlayerAnim::Idle,
                PlayerAnim::WalkWinddown,
                PlayerAnim::WalkWinddownAlt,
                PlayerAnim::RunWinddown,
                PlayerAnim::RunWinddownAlt,
            ] {
                scene.anim.state = stop;
                scene.anim.switched = Some(AnimSwitch {
                    from: gait,
                    local_tick: 17,
                    at: SimTick::from_u32(100),
                });
                for elapsed in 0..4 {
                    let blend = scene
                        .player_anim_blend(SimTick::from_u32(100 + elapsed))
                        .unwrap();
                    assert_eq!(blend.local_tick, 17);
                    assert_eq!(blend.alpha_q12, (elapsed * 1024) as u16);
                }
                assert!(scene.player_anim_blend(SimTick::from_u32(104)).is_none());
            }
        }
        scene.anim.state = PlayerAnim::Run;
        scene.anim.switched = Some(AnimSwitch {
            from: PlayerAnim::Walk,
            local_tick: 17,
            at: SimTick::from_u32(100),
        });
        assert_eq!(
            scene
                .player_anim_blend(SimTick::from_u32(102))
                .unwrap()
                .local_tick,
            19
        );
    }

    #[test]
    fn only_initial_world_pages_lock_controls_through_the_closing_animation() {
        use psx_game_runtime::poi::MessagePageSpan;
        let mut scene = test_scene();
        let welcome = WORLD_MESSAGE
            .get()
            .expect("test project has a welcome message");
        assert!(!scene.initial_world_message_active());
        scene.open_world_message_once();
        for page in 0..welcome.page_count {
            assert!(scene.initial_world_message_active());
            assert!(scene.gameplay_menu_blocked());
            assert!(
                !scene.cinematic_active(),
                "welcome keeps the normal HUD and music"
            );
            assert_eq!(scene.poi_messages.active().unwrap().page_offset(), page);
            scene.set_poi_presentation_frames(u16::MAX, u16::MAX);
            scene.advance_poi_message();
        }
        assert!(scene.poi_closing);
        for _ in 0..psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
            assert!(scene.initial_world_message_active());
            scene.advance_poi_presentation_frame();
        }
        assert!(!scene.initial_world_message_active());
        assert!(!scene.gameplay_menu_blocked());
        scene.open_world_message_once();
        assert!(
            !scene.initial_world_message_active(),
            "welcome stays dismissed"
        );
        assert!(scene.poi_messages.open_world(
            1,
            MessagePageSpan::new(crate::loc::DEMO_OUTRO_FIRST, crate::loc::DEMO_OUTRO_COUNT,)
        ));
        assert!(
            !scene.initial_world_message_active(),
            "outro stays interactive"
        );
        while scene.poi_messages.active().is_some() {
            scene.poi_messages.advance();
        }
        assert!(scene.poi_messages.open_poi(
            0,
            MessagePageSpan::new(welcome.page_first, welcome.page_count)
        ));
        assert!(
            !scene.initial_world_message_active(),
            "POI source never locks controls"
        );
    }

    #[test]
    fn legacy_dismiss_finishes_copy_then_contracts_over_presented_frames() {
        let mut scene = test_scene();
        scene.message_overlay = Some(RuntimeMessageOverlay {
            title: "",
            body: "RECOVERED",
        });
        scene.dismiss_legacy_message();
        assert!(!scene.poi_closing, "first press completes unfinished copy");
        scene.dismiss_legacy_message();
        assert!(scene.poi_closing);
        let duration = psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES;
        for remaining in (1..duration).rev() {
            scene.dismiss_legacy_message(); // repeated input must not restart/cancel contraction
            scene.advance_poi_presentation_frame();
            assert_eq!(scene.poi_panel_frame, remaining);
            assert!(scene.message_overlay.is_some());
        }
        scene.advance_poi_presentation_frame();
        assert!(!scene.poi_closing);
        assert!(scene.message_overlay.is_none());
    }

    #[test]
    fn message_press_behaves_the_same_at_every_render_rate() {
        // One message and one Cross press on a fixed sim tick, replayed with a
        // frame presented every 1, 2, 3, 5 and 9 sim ticks. On the sim clock
        // the copy has finished typing by the press, so the press must start
        // the close at every render rate and the panel must be gone on the
        // same tick. Advancing the type-on per presented frame fails this.
        fn run(ticks_per_frame: u32) -> (bool, Option<u32>) {
            const PRESS_TICK: u32 = 44;
            let mut scene = test_scene();
            scene.message_overlay = Some(RuntimeMessageOverlay {
                title: "",
                body: "Relay synchronized.",
            });
            let mut closing_after_press = false;
            let mut closed_at = None;
            for tick in 0..120u32 {
                if tick == PRESS_TICK {
                    scene.dismiss_legacy_message();
                    closing_after_press = scene.poi_closing;
                }
                scene.tick_poi_presentation();
                if tick % ticks_per_frame == 0 {
                    scene.snapshot_poi_presentation_for_render();
                }
                if closed_at.is_none() && scene.message_overlay.is_none() {
                    closed_at = Some(tick);
                }
            }
            (closing_after_press, closed_at)
        }
        let reference = run(2);
        assert!(
            reference.0,
            "the press lands after the copy finished typing"
        );
        assert!(reference.1.is_some(), "the panel closes");
        for ticks_per_frame in [1, 3, 5, 9] {
            assert_eq!(
                run(ticks_per_frame),
                reference,
                "a frame every {ticks_per_frame} sim ticks"
            );
        }
    }

    #[test]
    fn pickup_dismiss_shows_inventory_hint_only_after_contraction() {
        let mut scene = test_scene();
        scene.acquired_module = BoostModuleId(1);
        scene.set_poi_presentation_frames(u16::MAX, u16::MAX);
        scene.advance_poi_message();
        assert!(scene.poi_closing);
        for _ in 1..psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
            scene.advance_poi_presentation_frame();
            assert!(scene.message_overlay.is_none());
            assert!(!scene.acquired_module.is_none());
        }
        scene.advance_poi_presentation_frame();
        assert!(scene.acquired_module.is_none());
        assert!(scene.socket_hint_shown);
        assert_eq!(
            scene.message_overlay.unwrap().body,
            "START: OPENS INVENTORY\nEQUIP RECOVERED MODULES."
        );
        assert_eq!(scene.poi_page_type_frame, 0, "hint starts with unread copy");
    }

    #[test]
    fn only_the_last_page_dismisses_the_message_panel() {
        let mut scene = test_scene();
        scene
            .poi_messages
            .open_world(0, psx_game_runtime::poi::MessagePageSpan::new(0, 2));
        scene.set_poi_presentation_frames(u16::MAX, u16::MAX);
        scene.advance_poi_message();
        assert!(!scene.poi_closing);
        assert_eq!(scene.poi_messages.active().unwrap().page_offset(), 1);
        scene.set_poi_page_type_frame(u16::MAX);
        scene.advance_poi_message();
        assert!(scene.poi_closing);
        for _ in 0..psx_engine::ui::MESSAGE_PANEL_EXPAND_FRAMES {
            scene.advance_poi_presentation_frame();
        }
        assert!(scene.poi_messages.active().is_none());
    }

    #[test]
    fn new_game_discards_progress_and_messages_but_preserves_settings() {
        let layout = std::alloc::Layout::new::<Playtest>();
        let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
        assert!(!raw.is_null());
        unsafe { Playtest::init_zeroed(raw) };
        let mut scene = unsafe { std::boxed::Box::from_raw(raw) };
        scene.poi_save.set_flag(0);
        scene.poi_save.set_resume_position(SavedPlayerPosition {
            x: 1,
            y: 2,
            z: 3,
            yaw: 4,
        });
        scene
            .poi_messages
            .open_world(0, psx_game_runtime::poi::MessagePageSpan::new(0, 1));
        scene.socket_hint_shown = true;
        scene.poi_closing = true;
        scene.souls.award(200, 100);
        scene.lock_target = Some(1);
        scene.soft_lock_target = Some(1);
        scene
            .game_entities
            .actor_action_mut()
            .buffer
            .request(1, 100);
        scene.game_entities.actor_action_mut().evade_buffer = 8;
        scene.gameplay_epoch_set = true;
        scene.death_ticks_remaining = 10;
        scene.checkpoint = Some(RuntimeCheckpoint {
            room: RoomIndex::ZERO,
            position: RoomPoint::new(1, 2, 3),
            yaw: Angle::ZERO,
            checkpoint_id: "old run",
        });
        assert!(scene.power_up_inventory.add(BoostModuleId(1)));
        scene
            .game_entities
            .spawn_from_records_with_ids(GAME_ENTITIES.as_slice(), GAME_ENTITY_IDS.as_slice());
        let enemy = GAME_ENTITIES
            .iter()
            .position(|record| record.flags & psx_level::game_entity_flags::ENABLED != 0);
        if let Some(index) = enemy {
            scene.game_entities.apply_hit(
                GAME_ENTITIES.as_slice(),
                index,
                VitalityChannelId::One,
                u16::MAX,
                u16::MAX,
            );
            assert_eq!(
                scene.game_entities.state(index),
                psx_game_runtime::entities::GameEntityState::Dead
            );
        }
        scene.brightness_level = 3;
        scene.analog_deadzone = 27;

        scene.reset_new_game();

        assert_eq!(
            scene.poi_save,
            SaveBlock::new(PLAYER_MAX_HEALTH, PERSISTENT_FLAG_COUNT)
        );
        assert!(scene.poi_save_loaded && scene.poi_save_load_attempted);
        assert!(scene.poi_save_dirty, "next save replaces the old run");
        assert!(scene.poi_messages.active().is_none());
        assert!(!scene.poi_messages.world_message_shown(0));
        assert!(!scene.socket_hint_shown);
        assert!(!scene.poi_closing);
        assert_eq!(scene.souls.total(), 0);
        assert!(scene.lock_target.is_none() && scene.soft_lock_target.is_none());
        assert_eq!(
            scene.game_entities.actor_action_mut().buffer.take(101),
            None
        );
        assert_eq!(scene.game_entities.actor_action().evade_buffer, 0);
        assert!(!scene.gameplay_epoch_set);
        assert_eq!(scene.death_ticks_remaining, 0);
        assert_eq!(scene.game_entities.actor().health(), PLAYER_MAX_HEALTH);
        assert!(scene.power_up_inventory.is_empty());
        assert!(scene.checkpoint.is_none());
        assert_eq!(scene.game_entities.count(), GAME_ENTITIES.len());
        if let Some(index) = enemy {
            assert_ne!(
                scene.game_entities.state(index),
                psx_game_runtime::entities::GameEntityState::Dead
            );
            assert!(scene.game_entities.health(index) > 0);
        }
        assert_eq!(scene.brightness_level, 3);
        assert_eq!(scene.analog_deadzone, 27);
    }

    #[test]
    fn a_hit_bigger_than_the_bar_is_lethal_whatever_dealt_it_and_an_ordinary_one_is_not() {
        let layout = std::alloc::Layout::new::<Playtest>();
        let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
        assert!(!raw.is_null());
        unsafe {
            Playtest::init_zeroed(raw);
        }
        let mut storage = unsafe { std::boxed::Box::from_raw(raw) };
        let scene = &mut *storage;
        // An untyped hit and a hazard hit bigger than the bar both kill:
        // there is no other pool to swap into.
        assert!(scene.apply_untyped_player_damage(1000));
        assert!(scene.game_entities.actor().defeated());
        let kits = scene.player_kits();
        let (actor, stats) = scene.game_entities.actor_and_stats();
        actor.reset(stats, kits);
        assert!(scene.apply_hazard_player_damage(1000));
        assert!(scene.game_entities.actor_mut().defeated());
        // A small hit takes only what it deals.
        let kits = scene.player_kits();
        let (actor, stats) = scene.game_entities.actor_and_stats();
        actor.reset(stats, kits);
        assert!(!scene.apply_hazard_player_damage(10));
        assert!(!scene.game_entities.actor_mut().defeated());
        assert_eq!(
            scene.game_entities.actor_mut().health(),
            PLAYER_MAX_HEALTH - 10
        );
    }

    #[test]
    fn respawn_discards_old_projectiles_spent_health_swap_and_queued_actions() {
        // Use the production validity initializer on heap storage: the scene
        // contains non-zero Option niches and is too large for the test stack.
        let layout = std::alloc::Layout::new::<Playtest>();
        let raw = unsafe { std::alloc::alloc_zeroed(layout) as *mut Playtest };
        assert!(!raw.is_null());
        unsafe {
            Playtest::init_zeroed(raw);
        }
        let mut storage = unsafe { std::boxed::Box::from_raw(raw) };
        let scene = &mut *storage;
        let config = scene.player_stance_config;
        assert!(scene.game_entities.actor_mut().stance.request_swap(&config));
        scene.game_entities.actor_mut().health.apply_damage(150);
        scene
            .game_entities
            .actor_action_mut()
            .buffer
            .request(1, 100);
        scene.game_entities.actor_action_mut().evade_buffer = 8;
        assert!(scene
            .game_entities
            .latch_strike(RuntimeGameEntities::ACTOR_SLOT, 0, 1));
        scene.camera_recenter_requested = true;
        scene
            .combat_projectiles
            .spawn(ProjectileSpawn {
                position: [0; 3],
                velocity: [1, 0, 0],
                radius: 2,
                damage: 50,
                poise_damage: 50,
                lifetime_ticks: 100,
                room: RoomIndex::ZERO,
                team: CombatTeam::Enemy,
                owner: 0,
                tint_rgb: [255; 3],
                damage_channel: psx_game_runtime::projectiles::ProjectileDamageChannel::Horizon,
                visual: psx_game_runtime::projectiles::ProjectileVisualStyle::EMPTY,
            })
            .unwrap();
        assert_eq!(scene.combat_projectiles.len(), 1);
        scene.souls.award(50, 100);
        scene.reset_life_combat();
        assert_eq!(scene.combat_projectiles.len(), 0);
        assert_eq!(
            scene.game_entities.actor_action_mut().buffer.take(101),
            None
        );
        assert_eq!(scene.game_entities.actor_action().evade_buffer, 0);
        assert_eq!(
            scene
                .game_entities
                .spent_windows(RuntimeGameEntities::ACTOR_SLOT, 0),
            0
        );
        assert!(!scene.camera_recenter_requested);
        assert_eq!(
            scene.game_entities.actor().stance.active(),
            VitalityChannelId::One
        );
        assert!(scene.game_entities.actor().stance.can_swap());
        assert_eq!(scene.game_entities.actor().health(), PLAYER_MAX_HEALTH);
        assert_eq!(
            scene.souls.total(),
            50,
            "progress survives a new combat life"
        );
    }
}

/// Every fighter's motor standing in liquid: walk and run keep 60 percent
/// (the playtest has no vertical swim input yet).
pub(super) fn liquid_motor_rule(
    mut config: CharacterMotorConfig,
    in_liquid: bool,
) -> CharacterMotorConfig {
    if in_liquid {
        config.walk_speed = config.walk_speed.saturating_mul(60) / 100;
        config.run_speed = config.run_speed.saturating_mul(60) / 100;
    }
    config
}

/// This game's rules over a fighter's motor configuration, the player's and
/// every enemy's alike: no stamina limit (Cortex has no stamina), the
/// decelerating roll of an authored character, and the gravity of the room
/// the fighter is in.
pub(super) fn fighter_motor_rules(
    config: CharacterMotorConfig,
    room: RoomIndex,
    authored: bool,
) -> CharacterMotorConfig {
    let mut config = config.without_stamina_limit();
    config.roll_decelerates = authored;
    if let Some(room) = ROOMS.get(room.to_usize()) {
        config.gravity_per_tick_q8 = room.gravity_per_tick_q8;
    }
    config
}
