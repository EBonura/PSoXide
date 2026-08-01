use super::*;

impl Playtest {
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

    pub(super) fn respawn_after_water_death(&mut self) {
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
        self.motor.snap_to(position, yaw);
        self.player_health = self.player_health_max;
        self.water_death_ticks_remaining = 0;
        self.anim_state = PlayerAnim::Idle;
        self.anim_blend_from = None;
        self.anim_lock_until_tick = SimTick::ZERO;
        self.lock_target = None;
        self.soft_lock_target = None;
        self.active_interactable = None;
        self.evade_run_hold_ticks = 0;
        self.evade_run_hold_consumed = false;
        self.game_entities.spawn_from_records(GAME_ENTITIES);
        self.logic.init_from_records(LOGIC);
        self.box_props.reset_dynamic_state();
        self.sync_door_box_props();
        self.camera.snap_to_player_with_yaw(
            self.camera_target(None, false),
            self.camera_config(),
            yaw,
        );
        self.render_camera = world_camera_from_position_focus(
            PROJECTION,
            self.camera.position(),
            self.camera.focus(),
        );
        #[cfg(not(feature = "cd-stream-bench"))]
        self.load_active_room_window();
        telemetry::debug_log("player water:respawn");
    }

    /// Switch the player animation state, recording the outgoing
    /// pose so the renderer can crossfade instead of hard-cutting.
    pub(super) fn switch_player_anim(&mut self, anim: PlayerAnim, now: SimTick) {
        let old = self.anim_state;
        self.anim_blend_from = Some((old, now.saturating_sub(self.anim_start_tick), now));
        self.anim_state = anim;
        self.anim_start_tick = now;
    }

    /// Resolve the active crossfade for this render tick, if any.
    ///
    /// Alpha ramps linearly over the window; attacks use the short
    /// window so combat stays snappy while locomotion soft-blends.
    pub(super) fn player_anim_blend(&self, now: SimTick) -> Option<PlayerAnimBlend> {
        let (anim, local_tick, switch_tick) = self.anim_blend_from?;
        let duration = if player_anim_is_attack(self.anim_state)
            || self.anim_state.is_motor_fixed_action()
        {
            PLAYER_ANIM_BLEND_ACTION_TICKS
        } else {
            PLAYER_ANIM_BLEND_LOCOMOTION_TICKS
        };
        let elapsed = now.saturating_sub(switch_tick);
        if elapsed >= duration {
            return None;
        }
        Some(PlayerAnimBlend {
            anim,
            local_tick,
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
        self.switch_player_anim(anim, now);
        if player_anim_is_attack(anim) {
            // A fresh swing gets a fresh one-hit-per-enemy mask.
            self.swing_hit_mask = 0;
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
                character.action_speed(anim.action()),
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
        };
        if let Some(room) = ROOMS.get(self.room_index.to_usize()) {
            config.gravity_per_tick = room.gravity_per_tick;
        }
        config
    }

    pub(super) fn camera_orbit_speed_level(&self) -> u8 {
        ROOMS
            .get(self.room_index.to_usize())
            .map(|room| room.camera.orbit_speed_level)
            .unwrap_or(LevelCameraRecord::DEFAULT.orbit_speed_level)
    }

    pub(super) fn collect_collision_blockers(
        &self,
        out: &mut [CharacterCollisionCylinder],
    ) -> usize {
        let mut count = 0usize;
        for (index, inst) in MODEL_INSTANCES.iter().enumerate() {
            if inst.room != self.room_index || count >= out.len() {
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
            out[count] = CharacterCollisionCylinder::new(center, radius, height);
            count += 1;
        }
        count
            + psx_game_runtime::cylinder_props::collect_cylinder_prop_collision_blockers(
                CYLINDER_PROPS,
                self.room_index,
                &mut out[count..],
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
        if !self.player_moved_last_tick || self.water_death_ticks_remaining > 0 {
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
        config.height = config.height.max(256);
        config.min_floor_clearance = camera.min_floor_clearance;
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
        self.current_collision_room?;
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
        let input = if self.lock_target.is_some() {
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                recenter: ctx.is_held(button::L1),
            }
        } else {
            camera_input(ctx, self.camera_orbit_speed_level())
        };
        let lock_target = self
            .lock_target_position()
            .or_else(|| self.soft_lock_target_position());
        let target = self.camera_target(lock_target, self.anim_state != PlayerAnim::Idle);
        let config = self.camera_config();
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
        self.target_position(self.lock_target?)
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

    pub(super) fn find_best_interactable(&self) -> Option<usize> {
        let player = self.motor.position();
        let mut best = None;
        let mut best_distance = i32::MAX;
        for (index, interactable) in INTERACTABLES.iter().enumerate() {
            if !interactable_is_active(interactable) || interactable.room != self.room_index {
                continue;
            }
            let target = RoomPoint::new(interactable.x, interactable.y, interactable.z);
            let distance = distance_xz_sq(player, target);
            let radius_sq = square_i32_saturating(interactable.radius as i32);
            if distance <= radius_sq && distance < best_distance {
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
        if !interactable_is_active(interactable) {
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
                self.checkpoint = Some(RuntimeCheckpoint {
                    room: self.room_index,
                    position: self.motor.position(),
                    yaw: self.motor.yaw(),
                    checkpoint_id: interactable.checkpoint_id,
                });
                self.open_interactable_message(interactable);
                true
            }
        }
    }

    pub(super) fn open_interactable_message(&mut self, interactable: &InteractableRecord) {
        let (title, body) = interactable_message_text(interactable);
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
    }

    pub(super) fn lock_target_indicator_position(&self) -> Option<RoomPoint> {
        self.target_indicator_position(self.lock_target?)
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
        if self.lock_target.is_some() {
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
