use super::*;
use crate::playtest_runtime::bsp_hazard_damage;

/// Borrow the initialized prefix of a `MaybeUninit` scratch array.
///
/// # Safety
///
/// Every element below `len` must have been initialized as `T`.
unsafe fn initialized_prefix<T>(values: &[core::mem::MaybeUninit<T>], len: usize) -> &[T] {
    debug_assert!(len <= values.len());
    // SAFETY: guaranteed by the caller; MaybeUninit<T> has T's layout.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<T>(), len) }
}

/// Mutably borrow the initialized prefix of a `MaybeUninit` scratch array.
///
/// # Safety
///
/// Every element below `len` must have been initialized as `T`.
unsafe fn initialized_prefix_mut<T>(
    values: &mut [core::mem::MaybeUninit<T>],
    len: usize,
) -> &mut [T] {
    debug_assert!(len <= values.len());
    // SAFETY: guaranteed by the caller; MaybeUninit<T> has T's layout.
    unsafe { core::slice::from_raw_parts_mut(values.as_mut_ptr().cast::<T>(), len) }
}

impl Playtest {
    /// Phase-3 gameplay layer tick: entity state machines (moved
    /// through the engine motor's collision by [`SceneEntityMover`]),
    /// the logic event graph, and the effect dispatch that maps fire
    /// marks onto doors / message overlays / checkpoints. Runs
    /// unconditionally on every gameplay update (before input-mode
    /// early returns -- the souls sim does not pause with the pad);
    /// with zero cooked records (cortex today) the guard keeps it to
    /// a two-load check, preserving the bit-identical gates and the
    /// budget's <1k idle rule.
    pub(super) fn tick_gameplay_layer(&mut self, ctx: &Ctx) {
        // Deferred tokens are one-simulation-tick capabilities. Clear even on
        // zero-record and 30 Hz off-ticks so an i-frame whiff can never be
        // replayed later against a different retained pose.
        self.deferred_enemy_attacks.clear();
        if GAME_ENTITIES.is_empty() && LOGIC.is_empty() {
            return;
        }
        telemetry::stage_begin(telemetry::stage::GAME_LOGIC);
        // The portal-expanded active set as a compact index list; a
        // handful of loads per tick at MAX_ACTIVE_ROOMS = 16.
        let mut active_rooms =
            [const { core::mem::MaybeUninit::<RoomIndex>::uninit() }; MAX_ACTIVE_ROOMS];
        let mut active_count = 0usize;
        for slot in self.window.rooms.iter() {
            if let Some(active) = slot {
                active_rooms[active_count].write(active.index);
                active_count += 1;
            }
        }
        let player = self.motor.position();
        let player_pos = [player.x, player.y, player.z];
        let (player_radius, player_height) = match self.character {
            Some(character) => (character.radius, character.height),
            None if self.bsp.is_some() => (BSP_PLAYER_RADIUS, BSP_PLAYER_HEIGHT),
            None => (0, 0),
        };
        let now = ctx.sim_tick.as_u32();
        // NPC state and collision run at the authored 30 Hz visual cadence.
        // State clocks and movement consume a two-tick delta, preserving exact
        // 60 Hz-authored speeds/durations while avoiding two identical,
        // collision-heavy decisions per displayed frame. Player control,
        // combat arcs, and the logic graph remain 60 Hz below.
        let npc_tick_due =
            cfg!(feature = "npc-think-60hz") || ctx.sim_tick.as_u32().is_multiple_of(2);
        let npc_delta_ticks = if cfg!(feature = "npc-think-60hz") {
            1
        } else {
            2
        };
        let entity_count = self.game_entities.count().min(MAX_GAME_ENTITIES);
        let mut entity_positions =
            [const { core::mem::MaybeUninit::<[i32; 3]>::uninit() }; MAX_GAME_ENTITIES];
        let mut entity_dead =
            [const { core::mem::MaybeUninit::<bool>::uninit() }; MAX_GAME_ENTITIES];
        for index in 0..entity_count {
            entity_positions[index].write(self.game_entities.position(index));
            entity_dead[index].write(
                self.game_entities.state(index)
                    == psx_game_runtime::entities::GameEntityState::Dead,
            );
        }
        // SAFETY: both prefixes were initialized in the loop above.
        let entity_positions = unsafe { initialized_prefix(&entity_positions, entity_count) };
        // SAFETY: both prefixes were initialized in the loop above.
        let entity_dead = unsafe { initialized_prefix(&entity_dead, entity_count) };
        let spatial_masks = self.bsp.as_mut().map(|bsp| {
            // The observer stays inside the player's body rather than on the
            // floor seam. Actors and model instances are linked by complete
            // dynamic bounds below, matching Quake's multi-leaf entity rule.
            let observer = RoomPoint::new(
                player.x,
                player.y.saturating_add(player_height.max(1) >> 1),
                player.z,
            );
            let mut entity_activation_bounds =
                [const { core::mem::MaybeUninit::<BspVisibilityBounds>::uninit() };
                    MAX_GAME_ENTITIES];
            for index in 0..entity_count {
                let Some(record) = GAME_ENTITIES.get(index) else {
                    // Preserve initialization even if a malformed runtime table
                    // violates the cook's one-to-one entity contract.
                    entity_activation_bounds[index].write(BspVisibilityBounds::EMPTY);
                    continue;
                };
                entity_activation_bounds[index].write(BspVisibilityBounds::cylinder(
                    entity_positions[index],
                    i32::from(record.radius),
                    i32::from(record.height),
                ));
            }
            // SAFETY: every branch above writes the complete prefix.
            let entity_activation_bounds =
                unsafe { initialized_prefix(&entity_activation_bounds, entity_count) };
            let entity_mask = bsp.visible_bounds_mask(observer, entity_activation_bounds);
            let instance_count = MODEL_INSTANCES.len().min(MAX_MODEL_INSTANCES);
            let mut instance_activation_bounds =
                [const { core::mem::MaybeUninit::<BspVisibilityBounds>::uninit() };
                    MAX_MODEL_INSTANCES];
            for index in 0..instance_count {
                let instance = &MODEL_INSTANCES[index];
                let model = self
                    .models
                    .get(instance.model.to_usize())
                    .copied()
                    .flatten();
                let radius = model.map_or(0, |model| i32::from(model.collision_radius));
                let height = model.map_or(1, |model| i32::from(model.world_height));
                instance_activation_bounds[index].write(BspVisibilityBounds::cylinder(
                    [instance.x, instance.y, instance.z],
                    radius,
                    height,
                ));
            }
            // SAFETY: every element in the requested instance prefix was
            // initialized in the loop above.
            let instance_activation_bounds =
                unsafe { initialized_prefix_mut(&mut instance_activation_bounds, instance_count) };
            for (entity, record) in GAME_ENTITIES.iter().enumerate() {
                let instance = usize::from(record.model_instance);
                let Some(bounds) = instance_activation_bounds.get_mut(instance) else {
                    continue;
                };
                *bounds = BspVisibilityBounds::cylinder(
                    entity_positions[entity],
                    i32::from(record.radius),
                    i32::from(record.height),
                );
            }
            let instance_mask = bsp.visible_bounds_mask(observer, instance_activation_bounds);
            let logic_count = LOGIC.len().min(MAX_LOGIC_RECORDS);
            let mut logic_positions =
                [const { core::mem::MaybeUninit::<[i32; 3]>::uninit() }; MAX_LOGIC_RECORDS];
            for (index, record) in LOGIC.iter().take(logic_count).enumerate() {
                logic_positions[index].write([record.x, record.y, record.z]);
            }
            // SAFETY: the complete `logic_count` prefix was initialized above.
            let logic_positions = unsafe { initialized_prefix(&logic_positions, logic_count) };
            let mut logic_mask = bsp.visible_points_mask(observer, logic_positions);
            // A volume may span several leaves while its representative
            // origin lies in only one. Never suppress a touch that already
            // contains the player; the logic runtime repeats this inclusive
            // AABB test before firing the record.
            for (index, record) in LOGIC.iter().enumerate().take(64) {
                if record.kind == psx_level::logic_kind::TRIGGER_VOLUME
                    && player_pos[0] >= record.min[0]
                    && player_pos[0] <= record.max[0]
                    && player_pos[1] >= record.min[1]
                    && player_pos[1] <= record.max[1]
                    && player_pos[2] >= record.min[2]
                    && player_pos[2] <= record.max[2]
                {
                    logic_mask |= 1u64 << index;
                }
            }
            (entity_mask, logic_mask, instance_mask)
        });
        self.game_entities
            .set_spatial_active_mask(spatial_masks.map(|masks| masks.0));
        self.logic
            .set_spatial_active_mask(spatial_masks.map(|masks| masks.1));
        self.bsp_instance_visible_mask = spatial_masks.map_or(u16::MAX, |masks| masks.2 as u16);
        if let Some((entity_mask, _, _)) = spatial_masks {
            // Live entities whose leaf sits outside the player's PVS row
            // this tick: their idle/patrol AI is gated and their
            // body/equipment/shadow rendering is suppressed above.
            let mut suppressed = 0u32;
            for (index, dead) in entity_dead.iter().enumerate().take(64) {
                if !dead && entity_mask & (1u64 << index) == 0 {
                    suppressed += 1;
                }
            }
            if suppressed > 0 {
                telemetry::counter(telemetry::counter::GAME_ENTITY_PVS_SUPPRESSIONS, suppressed);
            }
        }
        let entity_stats = if npc_tick_due {
            // Souls i-frames: the deferred tick never resolves contact, so
            // this pre-motor value only feeds the shared tick-input contract.
            // `resolve_enemy_melee` re-queries invulnerability after the motor
            // update, pairing it with the same retained pose contact uses.
            let player_invulnerable = self.motor.is_action_invulnerable(self.motor_config());
            let mut mover = SceneEntityMover {
                bsp: self.bsp.as_mut(),
                window: &self.window,
                box_props: &self.box_props,
                models: &self.models,
                entity_positions,
                entity_dead,
                player,
                player_room: self.room_index,
                player_radius,
                player_height,
            };
            self.game_entities.tick_delta_deferred(
                GAME_ENTITIES,
                psx_game_runtime::entities::GameEntityTickInput {
                    player: player_pos,
                    player_room: self.room_index,
                    player_radius,
                    player_invulnerable,
                    // SAFETY: every active-room entry below `active_count` was
                    // initialized while walking the resident window above.
                    active_rooms: unsafe { initialized_prefix(&active_rooms, active_count) },
                },
                &mut mover,
                npc_delta_ticks,
                &mut self.deferred_enemy_attacks,
            )
        } else {
            psx_game_runtime::entities::GameEntityTickStats::default()
        };
        self.logic.tick(
            LOGIC,
            psx_game_runtime::logic::LogicTickInput {
                player: player_pos,
                player_room: self.room_index,
                // SAFETY: every active-room entry below `active_count` was
                // initialized while walking the resident window above.
                active_rooms: unsafe { initialized_prefix(&active_rooms, active_count) },
            },
            now,
        );
        self.dispatch_logic_effects();
        telemetry::counter(
            telemetry::counter::GAME_ENTITIES_THOUGHT,
            u32::from(entity_stats.thought),
        );
        if entity_stats.patrol_enters > 0 {
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_PATROL_ENTERS,
                u32::from(entity_stats.patrol_enters),
            );
        }
        if entity_stats.aggro_enters > 0 {
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_AGGRO_ENTERS,
                u32::from(entity_stats.aggro_enters),
            );
        }
        if entity_stats.windup_enters > 0 {
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_WINDUP_ENTERS,
                u32::from(entity_stats.windup_enters),
            );
        }
        if entity_stats.attack_enters > 0 {
            telemetry::counter(
                telemetry::counter::GAME_ENTITY_ATTACK_ENTERS,
                u32::from(entity_stats.attack_enters),
            );
        }
        let fired_total = self.logic.stats().fired;
        let fired_delta = fired_total.saturating_sub(self.logic_fired_reported);
        if fired_delta > 0 {
            telemetry::counter(
                telemetry::counter::LOGIC_RECORDS_FIRED,
                u32::from(fired_delta),
            );
        }
        self.logic_fired_reported = fired_total;
        telemetry::stage_end(telemetry::stage::GAME_LOGIC);
    }

    pub(super) fn init_gameplay(&mut self) {
        crate::game_trace("editor-playtest: gameplay init begin");
        self.shadow_material = upload_shadow_texture();
        self.particle_material = upload_particle_texture();
        self.bsp = if generated::PLAYTEST_USES_PXBSP {
            let mut bsp = BspRuntime::load_manifest()
                .unwrap_or_else(|error| panic!("cooked PXBSP initialization failed: {error}"));
            let _ = bsp.refresh_materials();
            Some(bsp)
        } else {
            None
        };
        if self.bsp.is_some() {
            self.current_ambient_rgb = PXBSP_AMBIENT_RGB;
        }
        crate::game_trace("editor-playtest: gameplay bsp ok");

        // Empty manifest? Boot to a clear-coloured screen.
        if ROOMS.is_empty() {
            crate::game_trace("editor-playtest: gameplay empty");
            return;
        };

        // Player init: prefer PLAYER_CONTROLLER (cook output)
        // for spawn + character; fall back to the bare
        // PLAYER_SPAWN for placeholder manifests. The spawn room
        // may be a manual portal room rather than room zero.
        let (spawn, character) = match PLAYER_CONTROLLER {
            Some(pc) => {
                let character = CHARACTERS
                    .get(pc.character.to_usize())
                    .map(runtime_character_from_record);
                (pc.spawn, character)
            }
            None => (PLAYER_SPAWN, None),
        };
        if ROOMS.get(spawn.room.to_usize()).is_none() {
            return;
        };
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            self.load_runtime_models();
            self.runtime_models_loaded = true;
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            self.runtime_models_loaded = false;
        }
        self.rebuild_box_prop_runtime();
        self.spawn = RoomPoint::new(spawn.x, spawn.y, spawn.z);
        self.character = character;
        self.motor
            .snap_to(self.spawn, Angle::from_q12(spawn.yaw as u16));
        self.room_index = spawn.room;
        self.anim_state = PlayerAnim::Idle;
        self.anim_start_tick = SimTick::ZERO;
        self.anim_blend_from = None;
        self.anim_lock_until_tick = SimTick::ZERO;
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
        self.game_entities.spawn_from_records(GAME_ENTITIES);
        self.deferred_enemy_attacks.clear();
        self.combat_projectiles.clear();
        self.logic.init_from_records(LOGIC);
        self.logic_fired_reported = 0;
        self.player_vitality = DualVitality::equal(PLAYER_MAX_HEALTH);
        self.power_up_loadout = PowerUpLoadout::DEFAULT;
        self.power_up_inventory = BoostInventory::STARTER;
        if self.poi_save_loaded {
            self.restore_claimed_poi_rewards();
        }
        self.selected_power_up_slot = BoostSlotId::HorizonEmpty as u8;
        self.selected_power_up_item = BoostProtocol::Rupture;
        self.hazard_death_ticks_remaining = 0;
        self.death_by_combat = false;
        self.weapon_attach_reported = false;
        self.swing_hit_mask = 0;
        self.clear_actor_pose_snapshots();
        self.sync_door_box_props();
        // Start the camera behind the AUTHORED spawn facing so the
        // SpawnPoint's editor rotation is honoured in Play (movement is
        // camera-relative, so a fixed start yaw silently overrode it).
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            Angle::from_q12(spawn.yaw as u16),
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
        #[cfg(feature = "cd-stream-benchmark")]
        cd_stream::run_benchmark(cd_arena());
    }

    pub(super) fn update_gameplay(&mut self, ctx: &mut Ctx) {
        // First gameplay update after loading: anchor the animation
        // epoch here so value-based phases do not inherit the variable
        // loading duration (see `gameplay_epoch` in main.rs).
        if !self.gameplay_epoch_set {
            // First gameplay update after loading: the loading scene's
            // streamed UI images (re-uploaded after the menu release for
            // the loading screen) are no longer drawn. Free their VRAM
            // slots so room textures stop competing with them, and force
            // a material refresh so any upload that was dropped during
            // the loading burst is re-queued into the freed space.
            release_ui_images();
            self.room_materials_unresolved = true;
            self.ensure_poi_save_loaded();
            self.open_world_message_once();
            self.gameplay_epoch = ctx.sim_tick;
            self.gameplay_epoch_set = true;
            // First spawn plays the intro with control locked out for the
            // clip's length. This has to arm HERE rather than in
            // `init_gameplay`: the streaming load sits between the two, and
            // a lock armed against tick zero has already expired by the time
            // the first gameplay tick runs. `start_player_anim_action` is a
            // no-op when no Intro clip is bound, so a character without one
            // starts in Idle exactly as before.
            self.start_player_anim_action(PlayerAnim::Intro, ctx.sim_tick, ctx.video_hz);
        }
        self.retry_poi_card_io(ctx.sim_tick.as_u32());
        self.portal_debug_log_cooldown = self.portal_debug_log_cooldown.saturating_sub(1);
        self.step_streaming_jobs(ctx);
        self.tick_gameplay_layer(ctx);
        if let Some(bsp) = self.bsp.as_mut() {
            bsp.tick_doors();
        }

        if ctx.just_pressed(button::R3) {
            if self.is_locked() {
                self.lock_target = None;
                self.lock_anchor = None;
            } else {
                self.lock_target = self.find_best_lock_target(LOCK_RANGE);
                if self.lock_target.is_none() {
                    // Nothing to lock onto: bind facing to a point ahead, so
                    // the locked locomotion set can be exercised anywhere.
                    let position = self.motor.position();
                    let yaw = self.motor.yaw();
                    self.lock_anchor = Some(RoomPoint::new(
                        position
                            .x
                            .saturating_add(yaw.sin().mul_i32(LOCK_ANCHOR_DISTANCE)),
                        position.y.saturating_add(LOCK_ANCHOR_HEIGHT),
                        position
                            .z
                            .saturating_add(yaw.cos().mul_i32(LOCK_ANCHOR_DISTANCE)),
                    ));
                }
            }
            if self.is_locked() {
                telemetry::debug_log("player lock:on");
            } else {
                telemetry::debug_log("player lock:off");
            }
            self.lock_switch_stick_held = false;
            self.lock_invalid_ticks = 0;
            self.soft_lock_target = None;
        }
        if ctx.just_pressed(COLLISION_DEBUG_BUTTON) {
            self.show_collision_debug = !self.show_collision_debug;
        }

        let poi_interaction_consumed = self.poi_messages.active().is_some();
        if poi_interaction_consumed && ctx.just_pressed(INTERACT_BUTTON) {
            self.advance_poi_message();
        }

        if self.message_overlay.is_some() {
            if ctx.just_pressed(INTERACT_BUTTON) || ctx.just_pressed(button::CIRCLE) {
                self.message_overlay = None;
            }
            self.camera_turning_last_tick = false;
            return;
        }

        if !ctx.pad.is_analog() {
            self.camera_turning_last_tick = false;
            return;
        }

        if ctx.just_pressed(button::SELECT) {
            self.free_orbit = !self.free_orbit;
        }
        let delta_vblanks = 1u16;
        telemetry::stage_begin(telemetry::stage::UPDATE_ACTOR);
        self.advance_box_prop_break_events(delta_vblanks);
        self.advance_box_prop_falls(delta_vblanks);
        if CAMERA_SWEEP_ENABLED {
            telemetry::stage_end(telemetry::stage::UPDATE_ACTOR);
            self.update_camera_sweep(delta_vblanks);
            return;
        }
        if self.free_orbit {
            let (right_x, right_y) = camera_stick_axes(ctx, self.analog_deadzone);
            self.camera_turning_last_tick = right_x != 0 || right_y != 0;
            self.orbit_yaw = self.orbit_yaw.add_signed_q12(scale_i16_by_vblanks(
                stick_to_yaw_delta(
                    psx_engine::InputAxis::new(right_x.saturating_neg()),
                    self.camera_orbit_speed_level(),
                    0,
                ),
                delta_vblanks,
            ));
            self.orbit_radius = (self.orbit_radius
                + scale_i32_by_vblanks(
                    stick_to_radius_delta(psx_engine::InputAxis::new(right_y), 0),
                    delta_vblanks,
                ))
            .clamp(CAMERA_RADIUS_MIN, CAMERA_RADIUS_MAX);
            let button_yaw_step =
                scale_i16_by_vblanks(CAMERA_YAW_STEP.as_q12() as i16, delta_vblanks);
            let button_radius_step = scale_i32_by_vblanks(CAMERA_RADIUS_STEP, delta_vblanks);
            if ctx.is_held(button::RIGHT) {
                self.orbit_yaw = self.orbit_yaw.add_signed_q12(button_yaw_step);
            }
            if ctx.is_held(button::LEFT) {
                self.orbit_yaw = self
                    .orbit_yaw
                    .add_signed_q12(button_yaw_step.saturating_neg());
            }
            if ctx.is_held(button::UP) {
                self.orbit_radius = (self.orbit_radius - button_radius_step).max(CAMERA_RADIUS_MIN);
            }
            if ctx.is_held(button::DOWN) {
                self.orbit_radius = (self.orbit_radius + button_radius_step).min(CAMERA_RADIUS_MAX);
            }
            self.player_moved_last_tick = false;
            self.active_interactable = None;
            telemetry::stage_end(telemetry::stage::UPDATE_ACTOR);
            telemetry::stage_begin(telemetry::stage::CAMERA);
            self.render_camera = self.free_orbit_camera();
            telemetry::stage_end(telemetry::stage::CAMERA);
            self.refresh_active_room_window_if_needed();
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            self.prewarm_visible_cell_caches();
            return;
        }

        let now = ctx.sim_tick;
        let action_locked =
            self.anim_lock_until_tick > now || self.hazard_death_ticks_remaining > 0;
        self.refresh_active_interactable();
        if !action_locked && !poi_interaction_consumed {
            if let Some(index) = self.active_interactable {
                if ctx.just_pressed(INTERACT_BUTTON)
                    && self.activate_interactable(index, now.as_u32())
                {
                    self.evade_run_hold_ticks = 0;
                    self.evade_run_hold_consumed = false;
                    self.camera_turning_last_tick = false;
                    telemetry::stage_end(telemetry::stage::UPDATE_ACTOR);
                    return;
                }
            } else if ctx.just_pressed(INTERACT_BUTTON)
                && self.activate_nearest_bsp_door(now.as_u32())
            {
                self.evade_run_hold_ticks = 0;
                self.evade_run_hold_consumed = false;
                self.camera_turning_last_tick = false;
                telemetry::stage_end(telemetry::stage::UPDATE_ACTOR);
                return;
            }
        }
        let circle = self.update_evade_run_button(ctx, delta_vblanks);
        let lock_facing_yaw = self
            .lock_target_position()
            .and_then(|target| psx_engine::yaw_to_point(self.motor.position(), target));
        let mut input = if action_locked {
            CharacterMotorInput::default()
        } else {
            motor_input(
                ctx,
                self.camera.yaw(),
                self.analog_deadzone,
                circle.sprint,
                circle.evade,
                lock_facing_yaw,
            )
        };
        // R1/R2/R1+R2 horizontal, L1/L2/L1+L2 vertical. See `update_attack_input`.
        if self.update_attack_input(ctx, now, action_locked) {
            input = CharacterMotorInput::default();
        }
        // Three-part walk: ramp the stick during the windup, glide on the
        // held vector during the winddown (the clips are in place; the
        // motor's speed envelope has to match their foot speed).
        let stick_active = input.move_x.raw() != 0 || input.move_z.raw() != 0;
        if !action_locked {
            input = self.walk_transition_input(input, stick_active, now, ctx.video_hz);
        }
        let mut config = self.motor_config();
        let bsp_contents = self
            .bsp
            .as_ref()
            .and_then(|bsp| bsp.player_contents(self.motor.position(), config.height));
        if bsp_contents.is_some_and(|sample| sample.water_level > 0) {
            // Shared BSP liquids retain 60 percent locomotion. The same
            // contents query feeds Quake's richer swim rules; the general
            // editor playtest has no vertical swim input contract yet.
            config.walk_speed = config.walk_speed.saturating_mul(60) / 100;
            config.run_speed = config.run_speed.saturating_mul(60) / 100;
        }
        if let Some(water) = self.water_cell_at(self.room_index, self.motor.position()) {
            if self.motor.position().y < water.surface_y && water.depth < water.lethal_depth {
                // The authored percentage is the exact retained locomotion
                // speed. There is no hidden second slowdown band.
                let movement_percent = water.movement_percent.clamp(10, 100);
                let movement_percent = i32::from(movement_percent);
                config.walk_speed = config.walk_speed.saturating_mul(movement_percent) / 100;
                config.run_speed = config.run_speed.saturating_mul(movement_percent) / 100;
            }
        }
        if action_locked && player_anim_is_attack(self.anim_state) {
            if let Some(character) = self.character {
                let local_tick = now.saturating_sub(self.anim_start_tick);
                if let Some(push_speed) = self.player_action_push_speed(
                    character,
                    self.anim_state,
                    local_tick,
                    ctx.video_hz,
                ) {
                    input.walk = 1;
                    config.walk_speed = push_speed;
                    config.run_speed = config.run_speed.max(push_speed);
                }
            }
        }
        if self.anim_lock_until_tick > now && player_anim_is_attack(self.anim_state) {
            self.break_box_props_for_attack(config);
        } else if let Some(trigger) =
            box_prop_movement_break_trigger(input, config, self.motor.stamina_q12())
        {
            self.break_box_props_for_movement(trigger, input, config, delta_vblanks);
        }
        telemetry::stage_end(telemetry::stage::UPDATE_ACTOR);
        telemetry::stage_begin(telemetry::stage::SIM_COLLISION);
        let mut blockers =
            psx_engine::FixedScratch::<CharacterCollisionCylinder, MAX_COLLISION_CYLINDERS>::new();
        self.collect_collision_blockers_into(&mut blockers);
        let mut aabb_blockers =
            psx_engine::FixedScratch::<CharacterCollisionAabb, MAX_STATIC_PROP_AABB_BLOCKERS>::new(
            );
        if self.bsp.is_some() {
            if self
                .collect_static_prop_aabb_blockers_checked_into(&mut aabb_blockers)
                .is_none()
            {
                // Cooked BSP collision state is authoritative. A malformed or
                // overflowing prop table freezes this frame instead of
                // allowing movement through a silently omitted blocker.
                telemetry::stage_end(telemetry::stage::SIM_COLLISION);
                return;
            }
        } else {
            self.collect_static_prop_aabb_blockers_into(&mut aabb_blockers);
        }
        let motor_frame = if self.bsp.is_some() {
            // The resident provider owns its bounded hull scratch and mover
            // transforms. Actor/cylinder and authored image/box/arch blockers
            // compose over that provider in stable cooked/live order; BSP
            // frames never build a grid-room collision set.
            telemetry::stage_end(telemetry::stage::SIM_COLLISION);
            telemetry::stage_begin(telemetry::stage::SIM_SOLVE);
            self.bsp
                .as_mut()
                .expect("checked resident BSP backend")
                .update_motor(
                    &mut self.motor,
                    input,
                    config,
                    delta_vblanks,
                    blockers.as_slice(),
                    aabb_blockers.as_slice(),
                )
                .expect("PXBSP player trace failed")
        } else {
            let mut collision_rooms =
                [const { CharacterCollisionRoom::EMPTY }; MAX_COLLISION_ROOMS];
            let collision_room_count = if self.chunked_level() {
                let catchup = delta_vblanks.min(4) as i32;
                let margin = config
                    .radius
                    .saturating_add(config.run_speed.saturating_mul(catchup));
                self.collect_collision_rooms(self.motor.position(), margin, &mut collision_rooms)
            } else {
                0
            };
            let single_collision_room = if collision_room_count == 1 {
                collision_rooms[0].room
            } else {
                None
            };
            let room_collision = match collision_room_count {
                0 => self
                    .current_collision_room
                    .as_ref()
                    .map(|room| room.collision()),
                1 => single_collision_room.as_ref().map(|room| room.collision()),
                _ => None,
            };
            let collision = if collision_room_count <= 1 {
                CharacterCollision::new_with_aabbs(
                    room_collision,
                    blockers.as_slice(),
                    aabb_blockers.as_slice(),
                )
            } else {
                CharacterCollision::rooms_with_aabbs(
                    &collision_rooms[..collision_room_count],
                    blockers.as_slice(),
                    aabb_blockers.as_slice(),
                )
            };
            telemetry::stage_end(telemetry::stage::SIM_COLLISION);
            telemetry::stage_begin(telemetry::stage::SIM_SOLVE);
            self.motor
                .update_vblanks_with_collision(collision, input, config, delta_vblanks)
        };
        telemetry::stage_end(telemetry::stage::SIM_SOLVE);
        self.player_moved_last_tick = motor_frame.moved;
        telemetry::stage_begin(telemetry::stage::SIM_ROOM_TRACK);
        if !self.update_current_room_from_player() {
            self.refresh_active_room_window_if_needed();
        }
        telemetry::stage_end(telemetry::stage::SIM_ROOM_TRACK);

        let hazard_countdown_was_active = self.hazard_death_ticks_remaining > 0;
        if !hazard_countdown_was_active {
            if let Some(sample) = self
                .bsp
                .as_ref()
                .and_then(|bsp| bsp.player_contents(self.motor.position(), config.height))
            {
                let damage = bsp_hazard_damage(sample.contents, now.as_u32());
                if damage > 0 {
                    let died = self.apply_untyped_player_damage(damage);
                    telemetry::counter(telemetry::counter::PLAYER_LIQUID_DAMAGE_EVENTS, 1);
                    telemetry::debug_log(match sample.contents {
                        psx_bsp::collision::CONTENTS_LAVA => "player bsp:lava",
                        psx_bsp::collision::CONTENTS_SLIME => "player bsp:slime",
                        _ => "player bsp:liquid",
                    });
                    if died {
                        self.arm_player_death(false, BSP_HAZARD_DEATH_TICKS, now, ctx.video_hz);
                    }
                }
            }
        }

        // The countdown/respawn below serves EVERY death cause: combat
        // damage arms it from `resolve_enemy_melee`, hazards and lethal
        // water from the sites above and below.
        if hazard_countdown_was_active {
            self.hazard_death_ticks_remaining = self.hazard_death_ticks_remaining.saturating_sub(1);
            if self.hazard_death_ticks_remaining == 0 {
                self.respawn_after_death();
            }
        } else if self.hazard_death_ticks_remaining == 0 {
            if let Some(water) = self.water_cell_at(self.room_index, self.motor.position()) {
                let submerged = self.motor.position().y
                    <= water
                        .surface_y
                        .saturating_sub(i32::from(water.death_submerge_depth));
                if water.depth >= water.lethal_depth && submerged {
                    self.player_vitality.empty_all();
                    self.arm_player_death(false, water.death_delay_ticks, now, ctx.video_hz);
                    telemetry::debug_log("player water:death");
                }
            }
        }

        let new_state = if self.anim_lock_until_tick > now {
            // Any locked action (attack, evade, hit) cancels the walk phases.
            self.loco = LocoPhase::Idle;
            self.anim_state
        } else {
            let motor_anim = player_anim_from_motor(motor_frame.anim);
            self.walk_transition_state(motor_anim, stick_active, now, ctx.video_hz)
        };
        telemetry::counter(
            telemetry::counter::PLAYER_ANIM_ACTION,
            new_state.action().to_index() as u32,
        );
        if new_state != self.anim_state {
            self.switch_player_anim(new_state, now, ctx.video_hz);
            if new_state == PlayerAnim::Roll {
                telemetry::debug_log("player roll:start");
            }
            if new_state.is_motor_fixed_action() {
                if let Some(character) = self.character {
                    self.lock_player_anim_action(character, new_state, now, ctx.video_hz);
                }
            }
        }

        if self.lock_target.is_some() {
            let target_exists = self
                .lock_target
                .is_some_and(|index| self.target_position(index).is_some());
            if !target_exists {
                self.lock_target = None;
                self.lock_switch_stick_held = false;
                self.lock_invalid_ticks = 0;
            } else if self.lock_target_valid(LOCK_BREAK_RANGE) {
                self.lock_invalid_ticks = 0;
                self.update_lock_target_switch(ctx);
            } else if self.lock_invalid_ticks >= LOCK_BREAK_GRACE_VBLANKS {
                self.lock_target = None;
                self.lock_switch_stick_held = false;
                self.lock_invalid_ticks = 0;
            } else {
                self.lock_invalid_ticks = self.lock_invalid_ticks.saturating_add(1);
            }
        }
        let (camera_right_x, camera_right_y) = ctx.pad.sticks.right_centered();
        self.camera_turning_last_tick = !self.is_locked()
            && psx_engine::Deadzone::new(self.analog_deadzone)
                .outside(camera_right_x, camera_right_y);
        if SOFT_LOCK_ENABLED {
            self.update_soft_lock(ctx);
        } else {
            self.soft_lock_target = None;
            self.soft_lock_suppressed = false;
        }

        telemetry::stage_begin(telemetry::stage::CAMERA);
        self.render_camera = self.update_follow_camera(ctx);
        telemetry::stage_end(telemetry::stage::CAMERA);
        telemetry::stage_begin(telemetry::stage::UPDATE_WINDOW);
        self.refresh_active_room_window_if_needed();
        telemetry::stage_end(telemetry::stage::UPDATE_WINDOW);
        #[cfg(all(
            feature = "world-grid-visible",
            not(feature = "vis-full-active-chunks")
        ))]
        self.prewarm_visible_cell_caches();
    }
}

/// Phase of the three-part walk (idle -> windup -> cruise -> winddown -> idle).
/// `Idle` is the all-zero boot value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum LocoPhase {
    Idle = 0,
    Windup,
    Cruise,
    /// Stick released while cruising: keep walking at full speed until the
    /// stride reaches a phase a winddown clip starts from, then stop.
    StopPending,
    Winddown,
}

/// The four clips one gait's phase machine drives. Walk and run differ only
/// in which actions they name, so the machine below is written once.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) struct Gait {
    cruise: PlayerAnim,
    windup: PlayerAnim,
    winddown: PlayerAnim,
    winddown_alt: PlayerAnim,
}

const WALK_GAIT: Gait = Gait {
    cruise: PlayerAnim::Walk,
    windup: PlayerAnim::WalkWindup,
    winddown: PlayerAnim::WalkWinddown,
    winddown_alt: PlayerAnim::WalkWinddownAlt,
};

const RUN_GAIT: Gait = Gait {
    cruise: PlayerAnim::Run,
    windup: PlayerAnim::RunWindup,
    winddown: PlayerAnim::RunWinddown,
    winddown_alt: PlayerAnim::RunWinddownAlt,
};

/// The gait a motor animation belongs to, `None` for everything that is not
/// a forward stepping cycle (idle, attacks, rolls, strafes).
const fn gait_of(anim: PlayerAnim) -> Option<Gait> {
    match anim {
        PlayerAnim::Walk => Some(WALK_GAIT),
        PlayerAnim::Run => Some(RUN_GAIT),
        _ => None,
    }
}

/// Q12 smoothstep of `elapsed / duration`, clamped to 0..=1.
fn smoothstep_q12(elapsed: u32, duration: u32) -> Q12 {
    let t = if duration == 0 {
        Q12::SCALE
    } else {
        let mut numerator = elapsed.min(duration);
        let mut denominator = duration;
        while numerator > (u32::MAX >> 12) {
            numerator = (numerator + 1) >> 1;
            denominator = (denominator + 1) >> 1;
        }
        ((numerator << 12) / denominator.max(1)).min(Q12::SCALE as u32) as i32
    };
    // t*t*(3 - 2t)
    let tt = (t * t) >> 12;
    Q12::from_raw((tt * (3 * Q12::SCALE - 2 * t)) >> 12)
}

impl Playtest {
    /// Duration in ticks of a bound one-shot walk transition, `None` when
    /// the action has no clip (the transition is then skipped).
    fn walk_transition_ticks(&self, anim: PlayerAnim, video_hz: VideoHz) -> Option<u32> {
        let character = self.character?;
        if character.action_clip(anim.action()).is_none() {
            return None;
        }
        let clip = character.clip_for(anim);
        Some(
            self.player_clip_duration_vblanks(
                character,
                clip,
                video_hz,
                character.action_speed(anim.action()),
                character.action_frame_range(anim.action()),
            )
            .unwrap_or(30)
            .max(1),
        )
    }

    /// Shape the motor input for the current walk phase: windup ramps the
    /// stick from zero, winddown keeps moving along the last stick vector
    /// while its clip fades out. Also records the glide vector.
    fn walk_transition_input(
        &mut self,
        mut input: CharacterMotorInput,
        stick_active: bool,
        now: SimTick,
        video_hz: VideoHz,
    ) -> CharacterMotorInput {
        if stick_active {
            self.loco_glide = (input.move_x, input.move_z);
        }
        let elapsed = now.as_u32().saturating_sub(self.loco_start_tick.as_u32());
        match self.loco {
            LocoPhase::Windup => {
                if let Some(duration) = self.walk_transition_ticks(self.gait().windup, video_hz) {
                    let ramp = smoothstep_q12(elapsed, duration);
                    input.move_x = input.move_x.mul_q12(ramp);
                    input.move_z = input.move_z.mul_q12(ramp);
                }
            }
            LocoPhase::StopPending if !stick_active => {
                input.move_x = self.loco_glide.0;
                input.move_z = self.loco_glide.1;
                input.walk = 0;
                input.sprint = false;
            }
            LocoPhase::Winddown if !stick_active => {
                if let Some(duration) = self.walk_transition_ticks(self.loco_stop_anim, video_hz) {
                    let fade = Q12::from_raw(Q12::SCALE - smoothstep_q12(elapsed, duration).raw());
                    input.move_x = self.loco_glide.0.mul_q12(fade);
                    input.move_z = self.loco_glide.1.mul_q12(fade);
                    input.walk = 0;
                    input.sprint = false;
                }
            }
            _ => {}
        }
        input
    }

    /// Advance the walk phase from the motor's animation intent and the raw
    /// stick, returning the player animation state to show. Windup and
    /// winddown only happen when their clips are bound; otherwise this is
    /// exactly the old motor-driven state.
    fn walk_transition_state(
        &mut self,
        motor_anim: PlayerAnim,
        stick_active: bool,
        now: SimTick,
        video_hz: VideoHz,
    ) -> PlayerAnim {
        let elapsed = now.as_u32().saturating_sub(self.loco_start_tick.as_u32());
        let motor_gait = gait_of(motor_anim);
        // Sprint pressed or released mid-move swaps the gait under the phase.
        if let Some(gait) = motor_gait {
            if self.loco == LocoPhase::Idle {
                self.loco_gait = gait;
            }
        }
        let gait = self.gait();
        let windup = self.walk_transition_ticks(gait.windup, video_hz);
        let winddown = self.walk_transition_ticks(gait.winddown, video_hz);
        // Blocked against a wall the motor reports Idle with the stick held;
        // keep the phase and show what the motor says, as before.
        let stepping = motor_gait == Some(gait);
        let switched = motor_gait.is_some() && !stepping;
        let released = !stick_active;
        let other = !(motor_gait.is_some() || motor_anim == PlayerAnim::Idle);
        match self.loco {
            LocoPhase::Idle => {
                if stepping {
                    self.loco_start_tick = now;
                    if windup.is_some() {
                        self.loco = LocoPhase::Windup;
                        gait.windup
                    } else {
                        self.loco = LocoPhase::Cruise;
                        gait.cruise
                    }
                } else {
                    motor_anim
                }
            }
            LocoPhase::Windup => {
                if released && winddown.is_some() {
                    self.begin_winddown(gait.winddown, now)
                } else if released || other {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                } else if switched {
                    // Gait changed during the ramp: run the new gait's windup.
                    self.enter_gait_windup(motor_anim, now, video_hz)
                } else if windup.is_some_and(|d| elapsed >= d) {
                    self.loco = LocoPhase::Cruise;
                    gait.cruise
                } else {
                    gait.windup
                }
            }
            LocoPhase::Cruise => {
                if stepping {
                    gait.cruise
                } else if switched {
                    // Walk <-> run at speed: no transition clip exists between
                    // the two cycles, so the cruise swaps directly.
                    self.loco_gait = gait_of(motor_anim).unwrap_or(WALK_GAIT);
                    motor_anim
                } else if released && winddown.is_some() {
                    self.loco = LocoPhase::StopPending;
                    self.loco_start_tick = now;
                    self.stop_if_in_phase(now, video_hz)
                } else if motor_anim == PlayerAnim::Idle && !released {
                    // blocked while holding the stick
                    PlayerAnim::Idle
                } else {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                }
            }
            LocoPhase::StopPending => {
                if stick_active && stepping {
                    self.loco = LocoPhase::Cruise;
                    gait.cruise
                } else if stick_active && switched {
                    self.loco = LocoPhase::Cruise;
                    self.loco_gait = gait_of(motor_anim).unwrap_or(WALK_GAIT);
                    motor_anim
                } else if other {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                } else {
                    self.stop_if_in_phase(now, video_hz)
                }
            }
            LocoPhase::Winddown => {
                if stick_active && stepping {
                    // Stick back before the stop finished: straight into cruise.
                    self.loco = LocoPhase::Cruise;
                    gait.cruise
                } else if stick_active && switched {
                    self.enter_gait_windup(motor_anim, now, video_hz)
                } else if other {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                } else if self
                    .walk_transition_ticks(self.loco_stop_anim, video_hz)
                    .is_none_or(|d| elapsed >= d)
                {
                    self.loco = LocoPhase::Idle;
                    PlayerAnim::Idle
                } else {
                    self.loco_stop_anim
                }
            }
        }
    }

    /// The gait whose clips the current phase is playing.
    fn gait(&self) -> Gait {
        self.loco_gait
    }

    /// Start (or restart) a gait's windup, falling straight through to its
    /// cruise when no windup clip is bound.
    fn enter_gait_windup(
        &mut self,
        motor_anim: PlayerAnim,
        now: SimTick,
        video_hz: VideoHz,
    ) -> PlayerAnim {
        let gait = gait_of(motor_anim).unwrap_or(WALK_GAIT);
        self.loco_gait = gait;
        self.loco_start_tick = now;
        if self.walk_transition_ticks(gait.windup, video_hz).is_some() {
            self.loco = LocoPhase::Windup;
            gait.windup
        } else {
            self.loco = LocoPhase::Cruise;
            gait.cruise
        }
    }

    /// Attacks. Two axes, three levels each: R1/R2/R1+R2 swing horizontally,
    /// L1/L2/L1+L2 strike vertically. Every level plays its clip whole, which
    /// is what these single recorded performances were made for.
    ///
    /// The pair window is why this is not a plain `just_pressed` chain: two
    /// buttons meant as one press arrive on different ticks, so the first
    /// press waits [`ATTACK_PAIR_WINDOW_TICKS`] to see whether its partner
    /// follows. A level whose clip is not bound is skipped, so an axis can
    /// ship one clip at a time.
    ///
    /// Returns true while an attack owns the player's input.
    fn update_attack_input(&mut self, ctx: &Ctx, now: SimTick, action_locked: bool) -> bool {
        /// (level 1 button, level 2 button, the three level animations)
        const AXES: [(u16, u16, [PlayerAnim; 3]); 2] = [
            (
                LIGHT_ATTACK_BUTTON,
                HEAVY_ATTACK_BUTTON,
                [
                    PlayerAnim::LightAttack,
                    PlayerAnim::HeavyAttack,
                    PlayerAnim::ComboAttack,
                ],
            ),
            (
                VERT_LIGHT_ATTACK_BUTTON,
                VERT_HEAVY_ATTACK_BUTTON,
                [
                    PlayerAnim::VertLightAttack,
                    PlayerAnim::VertHeavyAttack,
                    PlayerAnim::VertComboAttack,
                ],
            ),
        ];
        let Some((pressed_at, axis)) = self.attack_press else {
            if action_locked || !self.motor.action().is_idle() {
                return false;
            }
            for (index, (first, second, _)) in AXES.iter().enumerate() {
                if ctx.just_pressed(*first) || ctx.just_pressed(*second) {
                    self.attack_press = Some((now, index as u8));
                    return true;
                }
            }
            return false;
        };
        let (first, second, anims) = AXES[axis as usize];
        let (low, high) = (ctx.is_held(first), ctx.is_held(second));
        // Both down resolves the window early; there is nothing left to wait for.
        let waited = now.as_u32().saturating_sub(pressed_at.as_u32());
        if !(low && high) && waited < ATTACK_PAIR_WINDOW_TICKS {
            return true;
        }
        self.attack_press = None;
        let anim = match (low, high) {
            (true, true) => anims[2],
            (false, true) => anims[1],
            // A press already released by the end of the window is still a
            // level 1: these buttons are taps, not holds.
            _ => anims[0],
        };
        let bound = self
            .character
            .is_some_and(|character| character.action_clip(anim.action()).is_some());
        if bound && self.start_player_anim_action(anim, now, ctx.video_hz) {
            telemetry::counter(telemetry::counter::PLAYER_ATTACK_STARTS, 1);
        }
        true
    }

    fn begin_winddown(&mut self, anim: PlayerAnim, now: SimTick) -> PlayerAnim {
        self.loco = LocoPhase::Winddown;
        self.loco_start_tick = now;
        self.loco_stop_anim = anim;
        anim
    }

    /// While a stop is pending: keep the walk cycle until it reaches the
    /// stride start (winddown clip) or, when the mirrored clip is bound, the
    /// half stride (its mirror). Falls back to an immediate stop when the walk
    /// clip's cycle length cannot be resolved.
    fn stop_if_in_phase(&mut self, now: SimTick, video_hz: VideoHz) -> PlayerAnim {
        let gait = self.gait();
        let Some(cycle) = self.walk_transition_ticks(gait.cruise, video_hz) else {
            return self.begin_winddown(gait.winddown, now);
        };
        let alt_bound = self
            .character
            .is_some_and(|c| c.action_clip(gait.winddown_alt.action()).is_some());
        // Ticks since the Walk clip started (its frame 0 is the stride start).
        let position = now.as_u32().saturating_sub(self.anim_start_tick.as_u32()) % cycle;
        if position <= 1 || position + 1 >= cycle {
            self.begin_winddown(gait.winddown, now)
        } else if alt_bound && position.abs_diff(cycle / 2) <= 1 {
            self.begin_winddown(gait.winddown_alt, now)
        } else {
            gait.cruise
        }
    }
}
