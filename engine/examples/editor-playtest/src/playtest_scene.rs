use super::*;

impl Scene for Playtest {
    fn render_submission(&self) -> RenderSubmission {
        RenderSubmission::Queued
    }

    /// Lend the uploaded HUD font to the flow driver so front-end UI
    /// scenes (the cooked Main Menu) draw their labels and buttons with
    /// the same glyphs the in-game HUD uses.
    fn ui_font(&self) -> Option<&FontAtlas> {
        self.ui_fonts[0].as_ref()
    }

    fn ui_font_at(&self, index: u8) -> Option<&FontAtlas> {
        self.ui_fonts
            .get(index as usize)
            .and_then(|font| font.as_ref())
    }

    fn ui_texture(&self, asset_id: AssetId) -> Option<UiTextureSlot> {
        let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture)?;
        // Streamed UI images carry empty baked bytes; they are already in
        // VRAM (loaded on menu entry), so look up the existing slot rather
        // than re-parsing empty bytes through `ensure_ui_texture_uploaded`.
        let slot = if asset.bytes.is_empty() {
            find_room_texture_vram_slot(asset.id)?
        } else {
            ensure_ui_texture_uploaded(asset.id, asset.bytes)?
        };
        Some(UiTextureSlot {
            clut_word: slot.clut_word,
            tpage_word: slot.tpage_word,
            texture_window: slot.texture_window,
            texture_width: slot.texture_width,
            texture_height: slot.texture_height,
        })
    }

    /// Gameplay and each UI scene use distinct resource-set keys so the flow
    /// driver fires `on_exit_state`/`on_enter_state` across menu-to-menu and
    /// menu-to-gameplay boundaries. The UI font atlas and menu image RAM cache
    /// are shared by all menu states; streamed UI image VRAM is scoped to the
    /// active UI scene so a splash/logo screen does not keep its texture resident
    /// beside every main-menu strip.
    fn state_resource_key(&self, state: SceneStateRef) -> u32 {
        if state.has_gameplay() {
            GAMEPLAY_RESOURCE_KEY
        } else if state.ui_scene != psx_level::UI_SCENE_NONE {
            MENU_RESOURCE_KEY.saturating_add(u32::from(state.ui_scene).saturating_add(1))
        } else {
            MENU_RESOURCE_KEY
        }
    }

    /// Acquire the shared resource set. On first entry: reserve the static VRAM
    /// regions, then pack every UI font into one combined atlas and upload it
    /// in a single `GP0(A0h)` transfer. Uploading the fonts one-at-a-time
    /// desyncs the GPU command stream and freezes the world render, so the
    /// consolidated upload is the fix; routing it through the allocator keeps
    /// the font VRAM tracked.
    fn on_enter_state(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        acquire_shared_ui_fonts(&mut self.ui_fonts);
        // Streamed UI images live only in menu states. Menu entry uploads any
        // already-cached active-scene images but does not read the disc; those
        // reads are stepped after boot by `update_ui_resources` so real hardware
        // can render a first frame before menu preloading starts. Gameplay entry
        // frees previous menu VRAM (see `on_exit_state`). The sky panorama is
        // gameplay-scoped, so it is the mirror image: loaded on gameplay entry
        // and freed on gameplay exit.
        #[cfg(feature = "cd-stream-bench")]
        if state.has_gameplay() {
            load_streamed_sky_from_cd();
        } else {
            note_menu_ui_scene_entered();
            let _ = load_ui_images_for_scene(state.ui_scene);
        }
        let _ = state;
    }

    /// Release the menu's streamed UI images when leaving a menu state so the
    /// gameplay room textures reclaim that VRAM. The shared UI font atlas is NOT
    /// released here (it serves the gameplay HUD too).
    fn on_exit_state(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        if state.has_gameplay() {
            // Re-anchor the animation epoch on the next gameplay entry
            // (see `gameplay_epoch` in main.rs).
            self.gameplay_epoch_set = false;
            self.clear_actor_pose_snapshots();
        }
        #[cfg(feature = "cd-stream-bench")]
        if state.has_gameplay() {
            release_streamed_sky();
        } else {
            release_ui_images();
        }
        let _ = state;
    }

    /// Apply front-end settings chosen before Play. Screen-position options
    /// shift the whole rendered scene through the display window.
    fn apply_options(&mut self, options: &[psx_level::LevelOptionDef], values: &[i32]) {
        for (option, value) in options.iter().zip(values) {
            if option.id == SCREEN_OFFSET_X_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_h_offset(offset_px, psx_gpu::Resolution::R320X240);
            } else if option.id == SCREEN_OFFSET_Y_OPTION_ID {
                let offset_px = (*value).clamp(-128, 127) as i16;
                psx_gpu::set_screen_v_offset(
                    offset_px,
                    psx_gpu::VideoMode::Ntsc,
                    psx_gpu::Resolution::R320X240,
                );
            }
        }
    }

    fn init(&mut self, _ctx: &mut Ctx) {
        self.init_gameplay();
    }

    fn loading_update(&mut self, ctx: &mut Ctx) -> bool {
        self.step_streaming_jobs(ctx);
        self.initial_world_ready()
    }

    /// Real load progress for the authored loading scene's bar: the
    /// initial room ring dominates the load, so it spans 0..3072; the
    /// texture/upload tail takes the last quarter. The engine pins the
    /// bar full once `loading_update` reports ready.
    fn loading_progress_q12(&self) -> i32 {
        #[cfg(not(feature = "cd-stream-bench"))]
        {
            4096
        }
        #[cfg(feature = "cd-stream-bench")]
        {
            if !self.runtime_models_loaded {
                // A failed persistent load never resumes, so leaving the bar
                // parked at whatever fraction it reached reads as "still
                // working". Empty and stuck is the honest signal, and it is the
                // only one this screen can give without authored error UI.
                if persistent_assets_arena().failed() {
                    return 0;
                }
                return persistent_assets_arena().progress_q12().saturating_mul(3) / 8;
            }
            let count = self.resident_desired_count.min(STREAMED_ROOM_SLOT_COUNT);
            if count == 0 {
                return 1536;
            }
            let mut resident = 0usize;
            let mut i = 0usize;
            while i < count {
                let room = self.resident_desired[i];
                if room != INVALID_ROOM_INDEX && streamed_room_is_resident(room) {
                    resident += 1;
                }
                i += 1;
            }
            // Persistent assets span 0..1536, rooms span 1536..3840;
            // the texture/upload tail is the last
            // stretch, pinned to 4096 by the engine once
            // `loading_update` reports fully ready.
            (1536 + (resident as i32).saturating_mul(2304) / count as i32).min(4096)
        }
    }

    /// Re-upload the loading scene's streamed images into VRAM from
    /// the front-end RAM cache (filled by the contiguous menu
    /// preload). Never touches the CD: the laser belongs to the world
    /// stream during loading.
    fn prepare_loading_assets(&mut self, scene: u16) {
        #[cfg(feature = "cd-stream-bench")]
        {
            let loading_images_ready = menu_ui_cache_ready() && load_ui_images_for_scene(scene);
            // The loading images are now in VRAM; this is the overlay
            // handoff point (`MenuGameplayOverlay`): gameplay room draws
            // own the cache's RAM from here. Claims are reset so any
            // rooms built before the handoff (menu-time bootstrap)
            // refill their quads instead of trusting bytes the menu
            // preload may have overwritten.
            if loading_images_ready {
                retire_menu_ui_cache();
                prebuilt_quads_arena().reset_claims();
                self.prewarm_active_room_window_quads();
            }
        }
        #[cfg(not(feature = "cd-stream-bench"))]
        let _ = scene;
    }

    fn update_ui_resources(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
        #[cfg(feature = "cd-stream-bench")]
        if !state.has_gameplay() {
            service_menu_ui_images(state.ui_scene);
        }
        let _ = state;
    }

    /// Hold the menu CD-DA until every front-end UI image is resident, so the
    /// front-end (intro/menu/settings) never reads the CD while music plays.
    fn front_end_assets_ready(&self) -> bool {
        menu_ui_cache_ready()
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.update_gameplay(ctx);
        // This tail runs after every intentional input-mode early return in
        // `update_gameplay`: freeze final actor state once, then run combat
        // from the same snapshots the next body/equipment render consumes.
        self.refresh_actor_pose_snapshots(ctx);
        self.resolve_enemy_melee(ctx);
        self.resolve_player_melee(ctx);
    }

    fn render(&mut self, ctx: &mut Ctx) {
        let camera = self.render_camera;
        self.prepared_overlay_camera = camera;
        self.prepared_overlay_sim_tick = self.gameplay_tick(ctx.sim_tick);
        self.prepared_overlay_analog = ctx.pad.is_analog();
        if !ctx.pad.is_analog() {
            // Keep a valid empty list queued; the prompt is an immediate
            // overlay draw after this list has drained.
            let _ = unsafe { OtFrame::begin(&mut OT) };
            return;
        }

        #[cfg(feature = "fps-overlay")]
        {
            // One presented frame per render() call; measure against the
            // gameplay-anchored tick so the readout is cadence-true.
            let now = self.prepared_overlay_sim_tick.as_u32();
            let gap = now.wrapping_sub(self.fps_last_tick).min(255) as u8;
            if self.fps_window_frames > 0 {
                self.fps_worst_gap = self.fps_worst_gap.max(gap);
            }
            self.fps_last_tick = now;
            self.fps_window_frames = self.fps_window_frames.saturating_add(1);
            if now.wrapping_sub(self.fps_window_start) >= 60 {
                self.fps_display = self.fps_window_frames;
                self.fps_display_worst = self.fps_worst_gap;
                self.fps_window_start = now;
                self.fps_window_frames = 0;
                self.fps_worst_gap = 0;
            }
        }
        let post_cross_debug = POST_CROSS_RENDER_DEBUG_LOGS && self.post_cross_debug_frames != 0;
        let post_cross_detail = post_cross_debug
            && self.post_cross_debug_frames == RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        let mut post_cross_logged_end = false;
        if post_cross_debug {
            debug_log_post_cross_render_start(
                self.room_index,
                camera,
                self.visibility.result.visible_room_mask(),
                self.active_room_mask(),
                self.current_collision_room.is_some(),
            );
        }

        let mut ot = unsafe { OtFrame::begin(&mut OT) };
        let mut primitive_packets = unsafe { PrimitivePacketArena::new(&mut PRIMITIVE_PACKETS) };

        let room_record = ROOMS.get(self.room_index.to_usize());
        // The cooked BSP replaces only static grid surfaces. It writes its
        // tagged packets into the same arena/OT used below, after which the
        // ordinary actor, equipment, effect, and overlay passes continue.
        let bsp_material_tick = self.gameplay_tick(ctx.sim_tick).as_u32();
        if let Some(bsp) = self.bsp.as_mut() {
            telemetry::stage_begin(telemetry::stage::ROOM);
            bsp.draw(camera, bsp_material_tick, &mut primitive_packets, &mut ot);
            telemetry::stage_end(telemetry::stage::ROOM);
        }

        // Sky shares the farthest OT slot with the maximum-depth PXBSP packet.
        // OT insertion prepends, so inserting the sky after PXBSP makes DMA
        // execute the sky first and keeps even a slot-2047 wall in front.
        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::SKY);
            draw_sky_panorama(room_record.sky, camera, &mut ot);
            telemetry::stage_end(telemetry::stage::SKY);
        }

        let mut world = unsafe { begin_world_render_pass(&mut ot, &mut WORLD_COMMANDS) };

        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::FAR_VISTA);
            draw_far_vista_ring(
                camera,
                room_record.far_vista,
                room_surface_options(room_record),
                &mut primitive_packets,
                &mut world,
            );
            telemetry::stage_end(telemetry::stage::FAR_VISTA);
        }

        if self.current_collision_room.is_some() || self.bsp.is_some() {
            let mut total_instance_stats = ModelInstanceDrawStats::default();
            let mut room_active_chunks = 0u32;
            let mut room_cached_draws = 0u32;
            let mut room_uncached_draws = 0u32;
            let mut room_cache_cells = 0u32;
            let mut room_cache_vertices = 0u32;
            let mut room_cache_surfaces = 0u32;
            let mut room_cache_fallback_draws = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_visibility_fallback_draws = 0u32;
            #[cfg(not(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            )))]
            let room_visibility_fallback_draws = 0u32;
            let mut room_active_chunk_mask = RuntimeDebugMask::EMPTY;
            // This mask describes streamed grid chunks, not the resident BSP.
            // BSP draw proof remains the shared primitive/GPU command counters.
            let mut room_drawn_chunk_mask = RuntimeDebugMask::EMPTY;
            #[cfg(feature = "world-grid-visible")]
            let mut room_visible_cells = 0u32;
            #[cfg(all(
                feature = "world-grid-visible",
                not(feature = "vis-full-active-chunks")
            ))]
            let mut room_range_culled_cells = 0u32;
            #[cfg(all(feature = "world-grid-visible", feature = "vis-full-active-chunks"))]
            let room_range_culled_cells = 0u32;
            #[cfg(feature = "world-grid-visible")]
            let mut room_stats_total = GridVisibilityStats::default();
            #[cfg(feature = "room-surface-profile")]
            let mut room_surface_packets = 0u32;
            #[cfg(feature = "room-surface-profile")]
            let mut room_surface_commands = 0u32;

            // Live entity poses: instances bound to game entities
            // render where the entity runtime moved them (phase 3).
            let mut entity_poses = [ModelInstancePoseOverride {
                instance: u16::MAX,
                x: 0,
                y: 0,
                z: 0,
                yaw: 0,
                clip: psx_level::OptionalModelClipIndex::NONE,
                phase_ticks: 0,
                one_shot: false,
            }; MAX_GAME_ENTITIES];
            let entity_pose_count = self.game_entity_pose_overrides(&mut entity_poses);
            let entity_poses = &entity_poses[..entity_pose_count];

            // PXBSP has no ActiveRuntimeRoom: that type owns parsed PSXW
            // render/collision payloads. Draw the singleton metadata room's
            // ordinary gameplay content directly in world space while the BSP
            // renderer above owns only static brush surfaces.
            if self.bsp.is_some() {
                if let (Some(room_record), Some(lighting)) =
                    (room_record, self.current_room_lighting(camera))
                {
                    let room_options = room_surface_options(room_record).with_material_animation(
                        self.gameplay_tick(ctx.sim_tick).as_u32(),
                        ctx.video_hz.as_u16(),
                    );
                    let actor_options = actor_surface_options(room_record).with_material_animation(
                        self.gameplay_tick(ctx.sim_tick).as_u32(),
                        ctx.video_hz.as_u16(),
                    );
                    let instance_stats = self.draw_room_world_content(
                        self.room_index,
                        &camera,
                        &self.materials[..self.material_count],
                        room_options,
                        actor_options,
                        &lighting,
                        ModelInstanceDepthPass::All,
                        entity_poses,
                        ctx,
                        &mut primitive_packets,
                        &mut world,
                    );
                    accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
                }
            }

            let active_draw_order = active_room_draw_order(
                &self.window.rooms,
                camera,
                &self.visibility.result,
                self.room_index,
                cached_room_draw_order_mode(),
            );
            for &active_slot in &active_draw_order {
                if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                    continue;
                }
                let active_slot = active_slot as usize;
                let Some(active) = self.window.rooms[active_slot] else {
                    continue;
                };
                let draws_room = self.portal_visibility_draws_room(active.index);
                if post_cross_detail {
                    debug_log_post_cross_render_room(active_slot, active, draws_room);
                }
                if !draws_room {
                    continue;
                }
                room_active_chunks = room_active_chunks.saturating_add(1);
                let chunk_mask = room_index_debug_mask(active.index);
                room_active_chunk_mask |= chunk_mask;
                if active.surface_cache.ready {
                    room_cache_cells =
                        room_cache_cells.saturating_add(active.surface_cache.cell_count as u32);
                    room_cache_vertices = room_cache_vertices
                        .saturating_add(active.surface_cache.vertex_count as u32);
                    room_cache_surfaces = room_cache_surfaces
                        .saturating_add(active.surface_cache.surface_count as u32);
                }
                let materials = active_room_materials(&active);
                let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                    continue;
                };
                let room_options = room_surface_options(room_record).with_material_animation(
                    self.gameplay_tick(ctx.sim_tick).as_u32(),
                    ctx.video_hz.as_u16(),
                );
                // Actors clear the surface they stand on; see actor_surface_options.
                let actor_options = actor_surface_options(room_record).with_material_animation(
                    self.gameplay_tick(ctx.sim_tick).as_u32(),
                    ctx.video_hz.as_u16(),
                );
                let room_camera = camera_for_room(camera, active);
                let lighting = RuntimeRoomLighting {
                    room_index: active.index,
                    ambient: Rgb8::from_array(active.ambient_rgb),
                    camera: room_camera,
                    fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                    fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                    fog_near: room_record.fog_near,
                    fog_far: room_record.fog_far,
                    lights: room_light_slice(LIGHTS, active.index),
                };
                #[cfg(feature = "room-surface-profile")]
                let room_packet_start = primitive_packets.len();
                #[cfg(feature = "room-surface-profile")]
                let room_command_start = world.command_len();
                telemetry::stage_begin(telemetry::stage::ROOM);
                if self.bsp.is_none() {
                    #[cfg(feature = "world-grid-visible")]
                    {
                        #[cfg(feature = "vis-full-active-chunks")]
                        {
                            let stats = if active.surface_cache.ready {
                                room_cached_draws = room_cached_draws.saturating_add(1);
                                if let Some((
                                    cached_cells,
                                    cached_cell_vertices,
                                    cached_vertices,
                                    cached_surfaces,
                                )) =
                                    room_surface_cache_slices(active.index, active.surface_cache)
                                {
                                    let vertex_count = cached_vertices.len();
                                    let room_projection = room_projection_arena();
                                    let projected_indices =
                                        &mut room_projection.indices[..vertex_count];
                                    let projected_vertices =
                                        &mut room_projection.vertices[..vertex_count];
                                    let projected_depths =
                                        &mut room_projection.depths[..vertex_count];
                                    let cell_scratch = cell_scratch_arena();
                                    let accepted_cell_indices = &mut cell_scratch.indices[..];
                                    let accepted_cell_depths = &mut cell_scratch.depths[..];
                                    generated::draw_project_cached_room!(
                                        &lighting,
                                        draw_indexed_cached_room_vertex_lit_all_cells,
                                        [
                                            cached_cells,
                                            cached_cell_vertices,
                                            cached_vertices,
                                            cached_surfaces,
                                            projected_indices,
                                            projected_vertices,
                                            projected_depths,
                                            accepted_cell_indices,
                                            accepted_cell_depths,
                                            materials,
                                        ],
                                        [
                                            &room_camera,
                                            room_options,
                                            cached_room_depth_mode(),
                                            cached_room_subdivision_mode(),
                                            ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                            active.sector_size,
                                            active.index == self.visibility.root,
                                            Some(prebuilt_room_quads_for(active.index)),
                                            &mut primitive_packets,
                                            &mut world,
                                        ]
                                    )
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    room_cache_fallback_draws =
                                        room_cache_fallback_draws.saturating_add(1);
                                    if let Some(render_room) = active.render() {
                                        room_drawn_chunk_mask |= chunk_mask;
                                        draw_room_vertex_lit(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            &mut primitive_packets,
                                            &mut world,
                                        );
                                    }
                                    GridVisibilityStats::default()
                                }
                            } else {
                                room_uncached_draws = room_uncached_draws.saturating_add(1);
                                if active_surface_cache_failed(active.surface_cache) {
                                    room_cache_fallback_draws =
                                        room_cache_fallback_draws.saturating_add(1);
                                }
                                if let Some(render_room) = active.render() {
                                    room_drawn_chunk_mask |= chunk_mask;
                                    draw_room_vertex_lit(
                                        render_room,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        &mut primitive_packets,
                                        &mut world,
                                    );
                                }
                                GridVisibilityStats::default()
                            };
                            room_visible_cells =
                                room_visible_cells.saturating_add(stats.cells_drawn as u32);
                            if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                                room_drawn_chunk_mask |= chunk_mask;
                            }
                            accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                        }
                        #[cfg(not(feature = "vis-full-active-chunks"))]
                        {
                            let player = self.motor.position();
                            let portal_cell_window = self.portal_cell_window(active.index);
                            // The player's own room anchors its per-cell PVS at
                            // the player; a far room admitted by the portal walk
                            // anchors at the portal that admitted it (the
                            // doorway-eye view). Rooms with no usable anchor
                            // draw every cell through the cached path below --
                            // NEVER a silent skip (the arch-door regression).
                            let window_visibility_anchor = if active.index == self.room_index {
                                Some(player)
                            } else {
                                self.portal_entry_anchor(active.index, active.sector_size)
                            };
                            telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
                            let visible_cells_result = match window_visibility_anchor {
                                Some(window_anchor) => {
                                    let visibility_anchor = RoomPoint::new(
                                        window_anchor.x.saturating_sub(active.offset_x),
                                        window_anchor.y,
                                        window_anchor.z.saturating_sub(active.offset_z),
                                    );
                                    self.cached_precomputed_visible_cells(
                                        active_slot,
                                        active.index,
                                        active.width,
                                        active.depth,
                                        active.sector_size,
                                        visibility_anchor,
                                        active.offset_x,
                                        active.offset_z,
                                        window_anchor,
                                        room_camera,
                                        ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                                            && !self.player_moved_last_tick
                                            && self.camera_turning_last_tick
                                            && active.surface_cache.ready,
                                    )
                                }
                                None => None,
                            };
                            telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
                            let stats = if let Some((cells, range_culled)) = visible_cells_result {
                                room_range_culled_cells =
                                    room_range_culled_cells.saturating_add(range_culled as u32);
                                room_visible_cells =
                                    room_visible_cells.saturating_add(cells.len() as u32);
                                if active.surface_cache.ready {
                                    room_cached_draws = room_cached_draws.saturating_add(1);
                                    if let Some((
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                    )) = room_surface_cache_slices(
                                        active.index,
                                        active.surface_cache,
                                    ) {
                                        let vertex_count = cached_vertices.len();
                                        let room_projection = room_projection_arena();
                                        let projected_indices =
                                            &mut room_projection.indices[..vertex_count];
                                        let projected_vertices =
                                            &mut room_projection.vertices[..vertex_count];
                                        let projected_depths =
                                            &mut room_projection.depths[..vertex_count];
                                        let cell_scratch = cell_scratch_arena();
                                        let accepted_cell_indices = &mut cell_scratch.indices[..];
                                        let accepted_cell_depths = &mut cell_scratch.depths[..];
                                        generated::draw_project_cached_room!(
                                            &lighting,
                                            draw_indexed_cached_room_vertex_lit_visible_cells,
                                            [
                                                cached_cells,
                                                cached_cell_vertices,
                                                cached_vertices,
                                                cached_surfaces,
                                                projected_indices,
                                                projected_vertices,
                                                projected_depths,
                                                accepted_cell_indices,
                                                accepted_cell_depths,
                                                active.depth,
                                                active.sector_size,
                                                materials,
                                            ],
                                            [
                                                &room_camera,
                                                room_options,
                                                cached_room_depth_mode(),
                                                cached_room_subdivision_mode(),
                                                cells,
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                portal_cell_window,
                                                Some(prebuilt_room_quads_for(active.index)),
                                                &mut primitive_packets,
                                                &mut world,
                                            ]
                                        )
                                    } else {
                                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                                        if let Some(render_room) = active.render() {
                                            draw_room_vertex_lit_visible_cells(
                                                render_room,
                                                materials,
                                                &lighting,
                                                &room_camera,
                                                room_options,
                                                cells,
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                &mut primitive_packets,
                                                &mut world,
                                            )
                                        } else {
                                            GridVisibilityStats::default()
                                        }
                                    }
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    if active_surface_cache_failed(active.surface_cache) {
                                        room_cache_fallback_draws =
                                            room_cache_fallback_draws.saturating_add(1);
                                    }
                                    if let Some(render_room) = active.render() {
                                        draw_room_vertex_lit_visible_cells(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            cells,
                                            ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                            &mut primitive_packets,
                                            &mut world,
                                        )
                                    } else {
                                        GridVisibilityStats::default()
                                    }
                                }
                            } else {
                                // No usable anchor or no PVS data for this room.
                                // Draw EVERY cell through the cached path -- it
                                // works for streamed rooms whose full render data
                                // is not resident (active.render() == None), which
                                // the old uncached-only fallback silently skipped
                                // (the arch-door black-room regression).
                                room_visibility_fallback_draws =
                                    room_visibility_fallback_draws.saturating_add(1);
                                if active.surface_cache.ready {
                                    if let Some((
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                    )) = room_surface_cache_slices(
                                        active.index,
                                        active.surface_cache,
                                    ) {
                                        room_cached_draws = room_cached_draws.saturating_add(1);
                                        let vertex_count = cached_vertices.len();
                                        let room_projection = room_projection_arena();
                                        let projected_indices =
                                            &mut room_projection.indices[..vertex_count];
                                        let projected_vertices =
                                            &mut room_projection.vertices[..vertex_count];
                                        let projected_depths =
                                            &mut room_projection.depths[..vertex_count];
                                        let cell_scratch = cell_scratch_arena();
                                        let accepted_cell_indices = &mut cell_scratch.indices[..];
                                        let accepted_cell_depths = &mut cell_scratch.depths[..];
                                        generated::draw_project_cached_room!(
                                            &lighting,
                                            draw_indexed_cached_room_vertex_lit_all_cells,
                                            [
                                                cached_cells,
                                                cached_cell_vertices,
                                                cached_vertices,
                                                cached_surfaces,
                                                projected_indices,
                                                projected_vertices,
                                                projected_depths,
                                                accepted_cell_indices,
                                                accepted_cell_depths,
                                                materials,
                                            ],
                                            [
                                                &room_camera,
                                                room_options,
                                                cached_room_depth_mode(),
                                                cached_room_subdivision_mode(),
                                                ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                                active.sector_size,
                                                // Lateral-cull cells in EVERY no-anchor
                                                // fallback room, not just the root: the
                                                // AABB test is the same conservative
                                                // margin bound the root room already
                                                // trusts, and 3-4 of ~5 drawn
                                                // rooms take this path per frame. Cells
                                                // it rejects are off-screen, so output
                                                // pixels are unchanged; only the
                                                // projection + surface walk for them is
                                                // skipped.
                                                true,
                                                Some(prebuilt_room_quads_for(active.index)),
                                                &mut primitive_packets,
                                                &mut world,
                                            ]
                                        )
                                    } else {
                                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                                        if let Some(render_room) = active.render() {
                                            draw_room_vertex_lit(
                                                render_room,
                                                materials,
                                                &lighting,
                                                &room_camera,
                                                room_options,
                                                &mut primitive_packets,
                                                &mut world,
                                            );
                                        }
                                        GridVisibilityStats::default()
                                    }
                                } else {
                                    room_uncached_draws = room_uncached_draws.saturating_add(1);
                                    if let Some(render_room) = active.render() {
                                        draw_room_vertex_lit(
                                            render_room,
                                            materials,
                                            &lighting,
                                            &room_camera,
                                            room_options,
                                            &mut primitive_packets,
                                            &mut world,
                                        );
                                    }
                                    GridVisibilityStats::default()
                                }
                            };
                            if stats.cells_drawn > 0 || stats.surfaces_considered > 0 {
                                room_drawn_chunk_mask |= chunk_mask;
                            }
                            accumulate_grid_visibility_stats(&mut room_stats_total, stats);
                        }
                    }
                    #[cfg(not(feature = "world-grid-visible"))]
                    {
                        room_uncached_draws = room_uncached_draws.saturating_add(1);
                        if active_surface_cache_failed(active.surface_cache) {
                            room_cache_fallback_draws = room_cache_fallback_draws.saturating_add(1);
                        }
                        if let Some(render_room) = active.render() {
                            room_drawn_chunk_mask |= chunk_mask;
                            draw_room_vertex_lit(
                                render_room,
                                materials,
                                &lighting,
                                &room_camera,
                                room_options,
                                &mut primitive_packets,
                                &mut world,
                            );
                        }
                    }
                }
                telemetry::stage_end(telemetry::stage::ROOM);
                #[cfg(feature = "room-surface-profile")]
                {
                    room_surface_packets = room_surface_packets.saturating_add(
                        primitive_packets.len().saturating_sub(room_packet_start) as u32,
                    );
                    room_surface_commands = room_surface_commands.saturating_add(
                        world.command_len().saturating_sub(room_command_start) as u32,
                    );
                }
                let player = self.motor.position();
                let instance_depth_pass = player_actor_depth_for_room(
                    active,
                    self.character,
                    &self.models,
                    player,
                    &room_camera,
                )
                .map(ModelInstanceDepthPass::BehindPlayer)
                .unwrap_or(ModelInstanceDepthPass::All);
                let instance_stats = self.draw_room_world_content(
                    active.index,
                    &room_camera,
                    materials,
                    room_options,
                    actor_options,
                    &lighting,
                    instance_depth_pass,
                    entity_poses,
                    ctx,
                    &mut primitive_packets,
                    &mut world,
                );
                accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
            }

            // Player draws through the same compact model path as
            // placed model instances.
            if let (Some(character), Some(player_pose)) = (self.character, self.player_actor_pose) {
                let player = self.motor.position();
                let player_lighting = self.current_room_lighting(camera);
                let actor_options = current_actor_surface_options(self.room_index);
                telemetry::stage_begin(telemetry::stage::PLAYER);
                if !cfg!(feature = "actor-shadows-off") {
                    if let Some(shadow_material) = self.shadow_material {
                        draw_actor_shadow(
                            player.x,
                            player.y,
                            player.z,
                            actor_shadow_radius(character.radius),
                            &camera,
                            actor_options,
                            shadow_material,
                            &mut primitive_packets,
                            &mut world,
                        );
                    }
                }
                let player_draw =
                    player_lighting.map_or(PlayerModelDrawStats::default(), |lighting| {
                        draw_player(
                            self.room_index,
                            character,
                            player_pose,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &mut primitive_packets,
                            &mut world,
                        )
                    });
                telemetry::stage_end(telemetry::stage::PLAYER);
                emit_model_counters(
                    player_draw.stats,
                    telemetry::counter::PLAYER_PROJECTED_VERTICES,
                    telemetry::counter::PLAYER_SUBMITTED_TRIS,
                    telemetry::counter::PLAYER_CULLED_TRIS,
                    telemetry::counter::PLAYER_DROPPED_TRIS,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_TESTS,
                    player_draw.bounds_tests as u32,
                );
                telemetry::counter(
                    telemetry::counter::PLAYER_BOUNDS_CULLED,
                    player_draw.bounds_culled as u32,
                );
                telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                let equipment_stats = if player_draw.bounds_culled != 0 {
                    EquipmentDrawStats::default()
                } else {
                    player_lighting.map_or(EquipmentDrawStats::default(), |lighting| {
                        draw_player_equipment(
                            player_pose,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            ctx.sim_tick,
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &mut primitive_packets,
                            &mut world,
                        )
                    })
                };
                telemetry::stage_end(telemetry::stage::EQUIPMENT);
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_DRAWS,
                    equipment_stats.draws as u32,
                );
                if equipment_stats.draws > 0 && !self.weapon_attach_reported {
                    // First frame of this life where the equipped weapon
                    // resolved to its socket pose and submitted: one
                    // PLAYER_WEAPON_ATTACHMENTS event per (re)spawn.
                    self.weapon_attach_reported = true;
                    telemetry::counter(telemetry::counter::PLAYER_WEAPON_ATTACHMENTS, 1);
                }
                emit_model_counters(
                    equipment_stats.stats,
                    telemetry::counter::EQUIPMENT_PROJECTED_VERTICES,
                    telemetry::counter::EQUIPMENT_SUBMITTED_TRIS,
                    telemetry::counter::EQUIPMENT_CULLED_TRIS,
                    telemetry::counter::EQUIPMENT_DROPPED_TRIS,
                );
            }

            if self.character.is_some() {
                let player = self.motor.position();
                let mut instance_equipment_remaining = MAX_EQUIPMENT_DRAWS;
                if self.bsp.is_some() {
                    if let (Some(room_record), Some(lighting)) =
                        (room_record, self.current_room_lighting(camera))
                    {
                        let actor_options = actor_surface_options(room_record)
                            .with_material_animation(
                                self.gameplay_tick(ctx.sim_tick).as_u32(),
                                ctx.video_hz.as_u16(),
                            );
                        telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                        let equipment_stats = draw_instance_equipment(
                            self.room_index,
                            &self.instance_actor_poses,
                            instance_equipment_remaining,
                            self.gameplay_tick(ctx.sim_tick),
                            ctx.video_hz,
                            &camera,
                            actor_options,
                            &lighting,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            &mut primitive_packets,
                            &mut world,
                        );
                        instance_equipment_remaining = instance_equipment_remaining
                            .saturating_sub(equipment_stats.draws as usize);
                        telemetry::stage_end(telemetry::stage::EQUIPMENT);
                    }
                }
                for &active_slot in &active_draw_order {
                    if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                        continue;
                    }
                    let Some(active) = self.window.rooms[active_slot as usize] else {
                        continue;
                    };
                    if !self.portal_visibility_draws_room(active.index) {
                        continue;
                    }
                    let room_camera = camera_for_room(camera, active);
                    let Some(player_depth) = player_actor_depth_for_room(
                        active,
                        self.character,
                        &self.models,
                        player,
                        &room_camera,
                    ) else {
                        continue;
                    };
                    let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                        continue;
                    };
                    let actor_options = room_surface_options(room_record);
                    let lighting = RuntimeRoomLighting {
                        room_index: active.index,
                        ambient: Rgb8::from_array(active.ambient_rgb),
                        camera: room_camera,
                        fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                        fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                        fog_near: room_record.fog_near,
                        fog_far: room_record.fog_far,
                        lights: room_light_slice(LIGHTS, active.index),
                    };
                    telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
                    let instance_stats = draw_model_instances(
                        active.index,
                        &self.instance_actor_poses,
                        self.gameplay_tick(ctx.sim_tick),
                        ctx.video_hz,
                        &room_camera,
                        actor_options,
                        &lighting,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        ModelInstanceDepthPass::InFrontOfPlayer(player_depth),
                        &mut primitive_packets,
                        &mut world,
                    );
                    telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
                    accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
                    // Enemy weapons ride their instances' live poses;
                    // one pass per room after both instance depth
                    // passes (the OT depth-sorts the weapon with its
                    // body).
                    telemetry::stage_begin(telemetry::stage::EQUIPMENT);
                    let equipment_stats = draw_instance_equipment(
                        active.index,
                        &self.instance_actor_poses,
                        instance_equipment_remaining,
                        self.gameplay_tick(ctx.sim_tick),
                        ctx.video_hz,
                        &room_camera,
                        actor_options,
                        &lighting,
                        &self.models,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        &self.clips,
                        &mut primitive_packets,
                        &mut world,
                    );
                    instance_equipment_remaining =
                        instance_equipment_remaining.saturating_sub(equipment_stats.draws as usize);
                    telemetry::stage_end(telemetry::stage::EQUIPMENT);
                }
            }

            telemetry::counter(telemetry::counter::ROOM_ACTIVE_CHUNKS, room_active_chunks);
            emit_room_chunk_mask(
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_LO,
                telemetry::counter::ROOM_ACTIVE_CHUNK_MASK_HI,
                room_active_chunk_mask,
            );
            emit_room_chunk_mask(
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_LO,
                telemetry::counter::ROOM_DRAWN_CHUNK_MASK_HI,
                room_drawn_chunk_mask,
            );
            let debug_view = self.active_room_selection_view();
            emit_player_map_debug(
                self.room_index,
                self.motor.position(),
                self.motor.yaw().as_q12(),
                RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
                self.visibility.camera_global,
                yaw_q12_from_basis(debug_view.sin_yaw, debug_view.cos_yaw),
                debug_view.sin_yaw,
                debug_view.cos_yaw,
                debug_view.sin_pitch,
                debug_view.cos_pitch,
            );
            self.emit_portal_visibility_counters();
            #[cfg(feature = "cd-stream-bench")]
            {
                let room_streams = room_streams_arena();
                telemetry::counter(
                    telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
                    room_streams.resident_slot_count() as u32,
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
                    room_streams.loading_room_mask(),
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
                    room_streams.resident_room_mask(),
                );
            }
            telemetry::counter(telemetry::counter::ROOM_CACHED_DRAWS, room_cached_draws);
            telemetry::counter(telemetry::counter::ROOM_UNCACHED_DRAWS, room_uncached_draws);
            telemetry::counter(telemetry::counter::ROOM_CACHE_CELLS, room_cache_cells);
            telemetry::counter(telemetry::counter::ROOM_CACHE_VERTICES, room_cache_vertices);
            telemetry::counter(telemetry::counter::ROOM_CACHE_SURFACES, room_cache_surfaces);
            telemetry::counter(
                telemetry::counter::ROOM_CACHE_FALLBACK_DRAWS,
                room_cache_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_VISIBILITY_FALLBACK_DRAWS,
                room_visibility_fallback_draws,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNKS_CONSIDERED,
                self.visibility.candidates as u32,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNK_CACHE_SKIPS,
                self.window.cache_skips as u32,
            );
            #[cfg(feature = "world-grid-visible")]
            {
                telemetry::counter(telemetry::counter::ROOM_VISIBLE_CELLS, room_visible_cells);
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_RANGE_CULLED,
                    room_range_culled_cells,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CONSIDERED,
                    room_stats_total.cells_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_DRAWN,
                    room_stats_total.cells_drawn as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_CELLS_CULLED,
                    room_stats_total.cells_frustum_culled as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACES_CONSIDERED,
                    room_stats_total.surfaces_considered as u32,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_PROJECTED_VERTICES,
                    room_stats_total.projected_vertices as u32,
                );
            }
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_DRAWS,
                total_instance_stats.draws as u32,
            );
            #[cfg(feature = "room-surface-profile")]
            {
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACE_PACKETS,
                    room_surface_packets,
                );
                telemetry::counter(
                    telemetry::counter::ROOM_SURFACE_COMMANDS,
                    room_surface_commands,
                );
            }
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_TESTS,
                total_instance_stats.bounds_tests as u32,
            );
            telemetry::counter(
                telemetry::counter::MODEL_INSTANCE_BOUNDS_CULLED,
                total_instance_stats.bounds_culled as u32,
            );
            emit_model_counters(
                total_instance_stats.stats,
                telemetry::counter::MODEL_INSTANCE_PROJECTED_VERTICES,
                telemetry::counter::MODEL_INSTANCE_SUBMITTED_TRIS,
                telemetry::counter::MODEL_INSTANCE_CULLED_TRIS,
                telemetry::counter::MODEL_INSTANCE_DROPPED_TRIS,
            );
            if post_cross_debug {
                debug_log_post_cross_render_end(
                    self.room_index,
                    room_active_chunk_mask,
                    room_drawn_chunk_mask,
                    primitive_packets.len(),
                    primitive_packets.remaining(),
                    world.command_len(),
                );
                post_cross_logged_end = true;
            }
        }

        if post_cross_debug && !post_cross_logged_end {
            debug_log_post_cross_render_end(
                self.room_index,
                RuntimeDebugMask::EMPTY,
                RuntimeDebugMask::EMPTY,
                primitive_packets.len(),
                primitive_packets.remaining(),
                world.command_len(),
            );
        }
        if post_cross_debug {
            self.post_cross_debug_frames = self.post_cross_debug_frames.saturating_sub(1);
        }

        let world_command_len = world.command_len();
        telemetry::stage_begin(telemetry::stage::WORLD_FLUSH);
        world.flush();
        telemetry::stage_end(telemetry::stage::WORLD_FLUSH);
        let _ = self.draw_particle_emitters(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );
        let _ = self.draw_player_water_wade_splash(
            camera,
            self.gameplay_tick(ctx.sim_tick),
            &mut ot,
            &mut primitive_packets,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(telemetry::counter::WORLD_COMMANDS, world_command_len as u32);
        // Submission is deliberately split from packet preparation. The app
        // runner first presents the previous queued frame and clears the new
        // back buffer, then calls submit_render below.
        let _ = ot;
    }

    fn submit_render(&mut self, _ctx: &mut Ctx) {
        self.overlay_camera = self.prepared_overlay_camera;
        self.overlay_sim_tick = self.prepared_overlay_sim_tick;
        self.overlay_analog = self.prepared_overlay_analog;
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        let ot_in_flight = unsafe { OtFrame::resume(&mut OT) }.submit_async();
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);
        ot_in_flight.detach();
    }

    fn render_overlay(&mut self, _ctx: &mut Ctx) {
        if !self.overlay_analog {
            if let Some(font) = self.ui_fonts[0].as_ref() {
                draw_analog_required_prompt(font);
            }
            return;
        }
        let camera = self.overlay_camera;
        let overlay_tick = self.overlay_sim_tick;

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            draw_room_atmosphere_overlay(room_record, overlay_tick);
        }

        if self.show_collision_debug {
            self.draw_collision_debug_overlay(camera);
        }

        if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(target, camera, overlay_tick);
        }

        #[cfg(feature = "fps-overlay")]
        if let Some(font) = self.ui_fonts[0].as_ref() {
            draw_fps_overlay(font, self.fps_display, self.fps_display_worst);
        }

        if self.character.is_some() {
            // The shared UI_NODES pool now holds front-end menu scenes too, so
            // draw only the HUD scene's slice as the in-game overlay.
            let (hud_first, hud_count) = hud_scene_range();
            let font_table = [
                self.ui_fonts[0].as_ref(),
                self.ui_fonts[1].as_ref(),
                self.ui_fonts[2].as_ref(),
                self.ui_fonts[3].as_ref(),
            ];
            draw_player_hud(
                UI_NODES,
                hud_first,
                hud_count,
                &font_table,
                (overlay_tick.as_u32() & 0xffff) as u16,
                self.player_health,
                self.player_health_max,
                self.motor.stamina_q12(),
                self.motor_config().stamina_max_q12,
            );
            // EXPLOSION PROBE (diagnostic): overlay the player's skinned-vertex
            // capture pages. Feature-gated -- probe builds only, never the
            // shipping game or perf-measurement builds.
            #[cfg(feature = "vert-debug-overlay")]
            if let Some(font) = self.ui_fonts[0].as_ref() {
                draw_player_vert_debug(font);
            }
        }

        if let Some(font) = self.ui_fonts[0].as_ref() {
            if let Some(message) = self.message_overlay {
                draw_interactable_message(font, message.title, message.body);
            } else if let Some(index) = self.active_interactable {
                if let Some(interactable) = INTERACTABLES.get(index) {
                    draw_interaction_prompt(font, interactable.prompt);
                }
            }
        }
    }
}

impl Playtest {
    #[allow(clippy::too_many_arguments)]
    fn draw_room_world_content(
        &self,
        room: RoomIndex,
        camera: &WorldCamera,
        materials: &[WorldRenderMaterial],
        room_options: WorldSurfaceOptions,
        actor_options: WorldSurfaceOptions,
        lighting: &RuntimeRoomLighting,
        instance_depth_pass: ModelInstanceDepthPass,
        entity_poses: &[ModelInstancePoseOverride],
        ctx: &Ctx,
        primitive_packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> ModelInstanceDrawStats {
        draw_water(
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        telemetry::stage_begin(telemetry::stage::ENTITY_MARKERS);
        draw_entity_markers(
            ENTITIES,
            room,
            materials,
            camera,
            room_options,
            primitive_packets,
            world,
        );
        telemetry::stage_end(telemetry::stage::ENTITY_MARKERS);
        telemetry::stage_begin(telemetry::stage::IMAGE_PROPS);
        box_prop_profile_begin(telemetry::stage::BOX_PROPS);
        draw_box_props(
            BOX_PROPS,
            BOX_PROP_SURFACES,
            &self.box_props,
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROPS);
        psx_game_runtime::cylinder_props::draw_cylinder_props::<
            _,
            OT_DEPTH,
            { !CYLINDER_PROPS.is_empty() },
        >(
            CYLINDER_PROPS,
            CYLINDER_PROP_SURFACES,
            room,
            camera,
            actor_options,
            lighting,
            prop_texture_slot,
            primitive_packets,
            world,
        );
        psx_game_runtime::arch_props::draw_arch_props(
            ARCH_PROPS,
            ARCH_PROP_SURFACES,
            room,
            camera,
            actor_options,
            lighting,
            prop_texture_slot,
            primitive_packets,
            world,
        );
        box_prop_profile_begin(telemetry::stage::BOX_PROP_DEBRIS);
        draw_box_prop_floor_debris(
            BOX_PROPS,
            &self.box_props,
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROP_DEBRIS);
        box_prop_profile_begin(telemetry::stage::BOX_PROP_SHARDS);
        draw_box_prop_break_events(
            BOX_PROPS,
            &self.box_props,
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::BOX_PROP_SHARDS);
        box_prop_profile_begin(telemetry::stage::IMAGE_CARDS);
        draw_image_props(
            IMAGE_PROPS,
            room,
            camera,
            actor_options,
            lighting,
            primitive_packets,
            world,
        );
        box_prop_profile_end(telemetry::stage::IMAGE_CARDS);
        telemetry::stage_end(telemetry::stage::IMAGE_PROPS);
        telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
        if !cfg!(feature = "actor-shadows-off") {
            if let Some(shadow_material) = self.shadow_material {
                draw_model_instance_shadows(
                    room,
                    camera,
                    actor_options,
                    shadow_material,
                    &self.models,
                    entity_poses,
                    if self.bsp.is_some() {
                        // psx-numeric-allow-next-line: per-instance visibility bitmask, see the parameter
                        u64::from(self.bsp_instance_visible_mask)
                    } else {
                        // psx-numeric-allow-next-line: per-instance visibility bitmask, all instances visible
                        u64::MAX
                    },
                    primitive_packets,
                    world,
                );
            }
        }
        let stats = draw_model_instances(
            room,
            &self.instance_actor_poses,
            self.gameplay_tick(ctx.sim_tick),
            ctx.video_hz,
            camera,
            actor_options,
            lighting,
            &self.model_faces[..self.model_face_count],
            &self.model_parts[..self.model_part_count],
            &self.model_vertices[..self.model_vertex_count],
            instance_depth_pass,
            primitive_packets,
            world,
        );
        telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
        stats
    }
}
