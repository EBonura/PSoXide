use super::*;
use crate::playtest_runtime::bsp_hazard_damage;

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
        let mut active_rooms = [INVALID_ROOM_INDEX; MAX_ACTIVE_ROOMS];
        let mut active_count = 0usize;
        for slot in self.window.rooms.iter() {
            if let Some(active) = slot {
                active_rooms[active_count] = active.index;
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
        let mut entity_positions = [[0i32; 3]; MAX_GAME_ENTITIES];
        let mut entity_dead = [false; MAX_GAME_ENTITIES];
        for (index, slot) in entity_positions
            .iter_mut()
            .enumerate()
            .take(self.game_entities.count())
        {
            *slot = self.game_entities.position(index);
            entity_dead[index] = self.game_entities.state(index)
                == psx_game_runtime::entities::GameEntityState::Dead;
        }
        let spatial_masks = self.bsp.as_mut().map(|bsp| {
            // Sample the observer and actors inside their body volumes rather
            // than at the floor seam, which can classify as solid at exact
            // brush boundaries. Runtime movement still owns the live X/Z.
            let observer = RoomPoint::new(
                player.x,
                player.y.saturating_add(player_height.max(1) >> 1),
                player.z,
            );
            let mut entity_activation_points = entity_positions;
            for (index, point) in entity_activation_points
                .iter_mut()
                .enumerate()
                .take(self.game_entities.count())
            {
                let height = GAME_ENTITIES
                    .get(index)
                    .map_or(1, |record| i32::from(record.height).max(1));
                point[1] = point[1].saturating_add(height >> 1);
            }
            let entity_mask = bsp.visible_points_mask(
                observer,
                &entity_activation_points[..self.game_entities.count()],
            );
            let mut instance_activation_points = [[0i32; 3]; MAX_MODEL_INSTANCES];
            for (index, point) in instance_activation_points
                .iter_mut()
                .enumerate()
                .take(MODEL_INSTANCES.len())
            {
                let instance = &MODEL_INSTANCES[index];
                let height = self
                    .models
                    .get(instance.model.to_usize())
                    .copied()
                    .flatten()
                    .map_or(1, |model| i32::from(model.world_height).max(1));
                *point = [
                    instance.x,
                    instance.y.saturating_add(height >> 1),
                    instance.z,
                ];
            }
            for (entity, record) in GAME_ENTITIES.iter().enumerate() {
                let instance = usize::from(record.model_instance);
                let Some(point) = instance_activation_points.get_mut(instance) else {
                    continue;
                };
                let position = entity_positions[entity];
                *point = [
                    position[0],
                    position[1].saturating_add(i32::from(record.height).max(1) >> 1),
                    position[2],
                ];
            }
            let instance_mask = bsp.visible_points_mask(
                observer,
                &instance_activation_points[..MODEL_INSTANCES.len().min(MAX_MODEL_INSTANCES)],
            );
            let mut logic_positions = [[0i32; 3]; MAX_LOGIC_RECORDS];
            for (slot, record) in logic_positions.iter_mut().zip(LOGIC.iter()) {
                *slot = [record.x, record.y, record.z];
            }
            let mut logic_mask = bsp.visible_points_mask(
                observer,
                &logic_positions[..LOGIC.len().min(MAX_LOGIC_RECORDS)],
            );
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
            for (index, dead) in entity_dead
                .iter()
                .enumerate()
                .take(self.game_entities.count().min(64))
            {
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
                    active_rooms: &active_rooms[..active_count],
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
                active_rooms: &active_rooms[..active_count],
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

        // Empty manifest? Boot to a clear-coloured screen.
        if ROOMS.is_empty() {
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
        self.logic.init_from_records(LOGIC);
        self.logic_fired_reported = 0;
        self.player_health = PLAYER_MAX_HEALTH;
        self.player_health_max = PLAYER_MAX_HEALTH;
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
            #[cfg(feature = "cd-stream-bench")]
            {
                release_ui_images();
                self.room_materials_unresolved = true;
            }
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
        self.portal_debug_log_cooldown = self.portal_debug_log_cooldown.saturating_sub(1);
        self.step_streaming_jobs(ctx);
        self.tick_gameplay_layer(ctx);
        if let Some(bsp) = self.bsp.as_mut() {
            bsp.tick_doors();
        }

        if ctx.just_pressed(button::R3) {
            self.lock_target = match self.lock_target {
                Some(_) => None,
                None => self.find_best_lock_target(LOCK_RANGE),
            };
            if self.lock_target.is_some() {
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
            let (right_x, right_y) = ctx.pad.sticks.right_centered();
            self.camera_turning_last_tick = abs_i16(right_x) >= CAMERA_STICK_DEADZONE;
            self.orbit_yaw = self.orbit_yaw.add_signed_q12(scale_i16_by_vblanks(
                stick_to_yaw_delta(
                    psx_engine::InputAxis::new(right_x.saturating_neg()),
                    self.camera_orbit_speed_level(),
                ),
                delta_vblanks,
            ));
            self.orbit_radius = (self.orbit_radius
                + scale_i32_by_vblanks(
                    stick_to_radius_delta(psx_engine::InputAxis::new(right_y)),
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
        if !action_locked {
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
                circle.sprint,
                circle.evade,
                lock_facing_yaw,
            )
        };
        if !action_locked && self.motor.action().is_idle() {
            let started = if ctx.just_pressed(LIGHT_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::LightAttack, now, ctx.video_hz)
            } else if ctx.just_pressed(HEAVY_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::HeavyAttack, now, ctx.video_hz)
            } else if ctx.just_pressed(COMBO_ATTACK_BUTTON) {
                self.start_player_anim_action(PlayerAnim::ComboAttack, now, ctx.video_hz)
            } else {
                false
            };
            if started {
                telemetry::counter(telemetry::counter::PLAYER_ATTACK_STARTS, 1);
                input = CharacterMotorInput::default();
            }
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
        let mut blockers = [CharacterCollisionCylinder::EMPTY; MAX_COLLISION_CYLINDERS];
        let blocker_count = self.collect_collision_blockers(&mut blockers);
        let mut aabb_blockers = [CharacterCollisionAabb::EMPTY; MAX_STATIC_PROP_AABB_BLOCKERS];
        let aabb_blocker_count = if self.bsp.is_some() {
            let Some(count) = self.collect_static_prop_aabb_blockers_checked(&mut aabb_blockers)
            else {
                // Cooked BSP collision state is authoritative. A malformed or
                // overflowing prop table freezes this frame instead of
                // allowing movement through a silently omitted blocker.
                telemetry::stage_end(telemetry::stage::SIM_COLLISION);
                return;
            };
            count
        } else {
            self.collect_static_prop_aabb_blockers(&mut aabb_blockers)
        };
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
                    &blockers[..blocker_count],
                    &aabb_blockers[..aabb_blocker_count],
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
                    &blockers[..blocker_count],
                    &aabb_blockers[..aabb_blocker_count],
                )
            } else {
                CharacterCollision::rooms_with_aabbs(
                    &collision_rooms[..collision_room_count],
                    &blockers[..blocker_count],
                    &aabb_blockers[..aabb_blocker_count],
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
                    let outcome = psx_game_runtime::character::apply_player_damage(
                        self.player_health,
                        false,
                        damage,
                    );
                    self.player_health = outcome.health;
                    telemetry::counter(telemetry::counter::PLAYER_LIQUID_DAMAGE_EVENTS, 1);
                    telemetry::debug_log(match sample.contents {
                        psx_bsp::collision::CONTENTS_LAVA => "player bsp:lava",
                        psx_bsp::collision::CONTENTS_SLIME => "player bsp:slime",
                        _ => "player bsp:liquid",
                    });
                    if outcome.died {
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
                    self.player_health = 0;
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
        let (camera_right_x, _) = ctx.pad.sticks.right_centered();
        self.camera_turning_last_tick =
            self.lock_target.is_none() && abs_i16(camera_right_x) >= CAMERA_STICK_DEADZONE;
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
    Winddown,
}

/// Q12 smoothstep of `elapsed / duration`, clamped to 0..=1.
fn smoothstep_q12(elapsed: u32, duration: u32) -> Q12 {
    let t = if duration == 0 {
        Q12::SCALE
    } else {
        ((elapsed.min(duration) as u64 * Q12::SCALE as u64) / duration as u64) as i32
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
                if let Some(duration) = self.walk_transition_ticks(PlayerAnim::WalkWindup, video_hz)
                {
                    let ramp = smoothstep_q12(elapsed, duration);
                    input.move_x = input.move_x.mul_q12(ramp);
                    input.move_z = input.move_z.mul_q12(ramp);
                }
            }
            LocoPhase::Winddown if !stick_active => {
                if let Some(duration) =
                    self.walk_transition_ticks(PlayerAnim::WalkWinddown, video_hz)
                {
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
        let windup = self.walk_transition_ticks(PlayerAnim::WalkWindup, video_hz);
        let winddown = self.walk_transition_ticks(PlayerAnim::WalkWinddown, video_hz);
        // Blocked against a wall the motor reports Idle with the stick held;
        // keep the phase and show what the motor says, as before.
        let walking = motor_anim == PlayerAnim::Walk;
        let released = !stick_active;
        match self.loco {
            LocoPhase::Idle => {
                if walking {
                    self.loco_start_tick = now;
                    if windup.is_some() {
                        self.loco = LocoPhase::Windup;
                        PlayerAnim::WalkWindup
                    } else {
                        self.loco = LocoPhase::Cruise;
                        PlayerAnim::Walk
                    }
                } else {
                    motor_anim
                }
            }
            LocoPhase::Windup => {
                if released && winddown.is_some() {
                    self.loco = LocoPhase::Winddown;
                    self.loco_start_tick = now;
                    PlayerAnim::WalkWinddown
                } else if released || !(walking || motor_anim == PlayerAnim::Idle) {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                } else if windup.is_some_and(|d| elapsed >= d) {
                    self.loco = LocoPhase::Cruise;
                    PlayerAnim::Walk
                } else {
                    PlayerAnim::WalkWindup
                }
            }
            LocoPhase::Cruise => {
                if walking {
                    PlayerAnim::Walk
                } else if released && winddown.is_some() {
                    self.loco = LocoPhase::Winddown;
                    self.loco_start_tick = now;
                    PlayerAnim::WalkWinddown
                } else if motor_anim == PlayerAnim::Idle && !released {
                    // blocked while holding the stick
                    PlayerAnim::Idle
                } else {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                }
            }
            LocoPhase::Winddown => {
                if stick_active && walking {
                    // Stick back before the stop finished: straight into cruise.
                    self.loco = LocoPhase::Cruise;
                    PlayerAnim::Walk
                } else if !(walking || motor_anim == PlayerAnim::Idle) {
                    self.loco = LocoPhase::Idle;
                    motor_anim
                } else if winddown.is_none_or(|d| elapsed >= d) {
                    self.loco = LocoPhase::Idle;
                    PlayerAnim::Idle
                } else {
                    PlayerAnim::WalkWinddown
                }
            }
        }
    }
}
