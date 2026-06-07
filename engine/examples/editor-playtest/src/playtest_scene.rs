use super::*;

impl Scene for Playtest {
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

    /// Menu and gameplay states use distinct resource-set keys so the flow
    /// driver fires `on_exit_state`/`on_enter_state` across the menu->gameplay
    /// boundary. The UI font atlas is shared by both (acquired once, never torn
    /// down); the keys differ only so the streamed UI image set can be loaded on
    /// menu entry and freed on gameplay entry.
    fn state_resource_key(&self, state: SceneStateRef) -> u32 {
        if state.has_gameplay() {
            GAMEPLAY_RESOURCE_KEY
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
        // Streamed UI images live only in menu states. Load them off UI.PAK on
        // menu entry; gameplay entry frees them (see `on_exit_state`). The sky
        // panorama is gameplay-scoped, so it is the mirror image: loaded on
        // gameplay entry and freed on gameplay exit.
        #[cfg(feature = "cd-stream-bench")]
        if state.has_gameplay() {
            load_streamed_sky_from_cd();
        } else {
            load_ui_images_from_cd();
        }
        let _ = state;
    }

    /// Release the menu's streamed UI images when leaving a menu state so the
    /// gameplay room textures reclaim that VRAM. The shared UI font atlas is NOT
    /// released here (it serves the gameplay HUD too).
    fn on_exit_state(&mut self, state: SceneStateRef, _ctx: &mut Ctx) {
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

    fn update(&mut self, ctx: &mut Ctx) {
        self.update_gameplay(ctx);
    }

    fn render(&mut self, ctx: &mut Ctx) {
        if !ctx.pad.is_analog() {
            if let Some(font) = self.ui_fonts[0].as_ref() {
                draw_analog_required_prompt(font);
            }
            return;
        }

        let camera = self.render_camera;
        let post_cross_debug = POST_CROSS_RENDER_DEBUG_LOGS && self.post_cross_debug_frames != 0;
        let post_cross_detail = post_cross_debug
            && self.post_cross_debug_frames == RUNTIME_SCHEDULE.post_cross_render_debug_frames;
        let mut post_cross_logged_end = false;
        if post_cross_debug {
            debug_log_post_cross_render_start(
                self.room_index,
                camera,
                self.portal_visibility.visible_room_mask(),
                self.active_room_mask(),
                self.current_collision_room.is_some(),
            );
        }

        let mut ot = unsafe { OtFrame::begin(&mut OT) };
        let mut primitive_packets = unsafe { PrimitivePacketArena::new(&mut PRIMITIVE_PACKETS) };

        let room_record = ROOMS.get(self.room_index.to_usize());
        // Sky inserts into the OT background slot before the world pass borrows
        // the OT; world geometry (slots 0..=OT_DEPTH-2) then draws in front.
        if let Some(room_record) = room_record {
            telemetry::stage_begin(telemetry::stage::SKY);
            draw_sky_panorama(room_record.sky, camera, &mut primitive_packets, &mut ot);
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

        if self.current_collision_room.is_some() {
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

            let active_draw_order = active_room_draw_order(
                &self.active_rooms,
                camera,
                &self.portal_visibility,
                self.room_index,
                cached_room_draw_order_mode(),
            );
            for &active_slot in &active_draw_order {
                if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                    continue;
                }
                let active_slot = active_slot as usize;
                let Some(active) = self.active_rooms[active_slot] else {
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
                let materials = active.materials();
                let Some(room_record) = ROOMS.get(active.index.to_usize()) else {
                    continue;
                };
                let room_options = room_surface_options(room_record);
                let actor_options = room_options;
                let room_camera = camera_for_room(camera, active);
                let lighting = RuntimeRoomLighting {
                    room_index: active.index,
                    ambient: Rgb8::from_array(active.ambient_rgb),
                    camera: room_camera,
                    fog_enabled: room_record.flags & room_flags::FOG_ENABLED != 0,
                    fog_rgb: Rgb8::from_array(room_record.fog_rgb),
                    fog_near: room_record.fog_near,
                    fog_far: room_record.fog_far,
                };
                telemetry::stage_begin(telemetry::stage::ROOM);
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
                            )) = room_surface_cache_slices(active.index, active.surface_cache)
                            {
                                let vertex_count = cached_vertices.len();
                                let projected_indices =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_INDICES[..vertex_count] };
                                let projected_vertices =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_VERTICES[..vertex_count] };
                                let projected_ready =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_READY[..vertex_count] };
                                let projected_depths =
                                    unsafe { &mut CACHED_ROOM_PROJECTED_DEPTHS[..vertex_count] };
                                let accepted_cell_indices =
                                    unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_INDICES[..] };
                                let accepted_cell_depths =
                                    unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                                draw_indexed_cached_room_vertex_lit_all_cells(
                                    cached_cells,
                                    cached_cell_vertices,
                                    cached_vertices,
                                    cached_surfaces,
                                    projected_indices,
                                    projected_vertices,
                                    projected_ready,
                                    projected_depths,
                                    accepted_cell_indices,
                                    accepted_cell_depths,
                                    materials,
                                    &lighting,
                                    &room_camera,
                                    room_options,
                                    cached_room_depth_mode(),
                                    cached_room_subdivision_mode(),
                                    ROOM_VISIBLE_CELL_SCREEN_MARGIN,
                                    active.index == self.portal_visibility_root,
                                    &mut primitive_packets,
                                    &mut world,
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
                        let global_visibility_anchor = player;
                        let visibility_anchor = RoomPoint::new(
                            global_visibility_anchor.x.saturating_sub(active.offset_x),
                            player.y,
                            global_visibility_anchor.z.saturating_sub(active.offset_z),
                        );
                        let visibility = GridVisibility::around(
                            visibility_anchor,
                            room_visibility_radius(room_record),
                        )
                        .with_screen_margin(ROOM_VISIBLE_CELL_SCREEN_MARGIN);
                        telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
                        let visible_cells_result = self.cached_precomputed_visible_cells(
                            active_slot,
                            active.index,
                            active.width,
                            active.depth,
                            active.sector_size,
                            visibility_anchor,
                            active.offset_x,
                            active.offset_z,
                            global_visibility_anchor,
                            room_camera,
                            ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                                && !self.player_moved_last_tick
                                && self.camera_turning_last_tick
                                && active.surface_cache.ready,
                        );
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
                                )) =
                                    room_surface_cache_slices(active.index, active.surface_cache)
                                {
                                    let vertex_count = cached_vertices.len();
                                    let projected_indices = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_INDICES[..vertex_count]
                                    };
                                    let projected_vertices = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_VERTICES[..vertex_count]
                                    };
                                    let projected_ready =
                                        unsafe { &mut CACHED_ROOM_PROJECTED_READY[..vertex_count] };
                                    let projected_depths = unsafe {
                                        &mut CACHED_ROOM_PROJECTED_DEPTHS[..vertex_count]
                                    };
                                    let accepted_cell_indices =
                                        unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_INDICES[..] };
                                    let accepted_cell_depths =
                                        unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                                    draw_indexed_cached_room_vertex_lit_visible_cells(
                                        cached_cells,
                                        cached_cell_vertices,
                                        cached_vertices,
                                        cached_surfaces,
                                        projected_indices,
                                        projected_vertices,
                                        projected_ready,
                                        projected_depths,
                                        accepted_cell_indices,
                                        accepted_cell_depths,
                                        active.depth,
                                        active.sector_size,
                                        materials,
                                        &lighting,
                                        &room_camera,
                                        room_options,
                                        cached_room_depth_mode(),
                                        cached_room_subdivision_mode(),
                                        cells,
                                        visibility.screen_margin,
                                        &mut primitive_packets,
                                        &mut world,
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
                                            visibility.screen_margin,
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
                                        visibility.screen_margin,
                                        &mut primitive_packets,
                                        &mut world,
                                    )
                                } else {
                                    GridVisibilityStats::default()
                                }
                            }
                        } else {
                            room_uncached_draws = room_uncached_draws.saturating_add(1);
                            room_visibility_fallback_draws =
                                room_visibility_fallback_draws.saturating_add(1);
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
                telemetry::stage_end(telemetry::stage::ROOM);
                telemetry::stage_begin(telemetry::stage::ENTITY_MARKERS);
                draw_entity_markers(
                    ENTITIES,
                    active.index,
                    materials,
                    &room_camera,
                    room_options,
                    &mut primitive_packets,
                    &mut world,
                );
                telemetry::stage_end(telemetry::stage::ENTITY_MARKERS);
                telemetry::stage_begin(telemetry::stage::IMAGE_PROPS);
                box_prop_profile_begin(telemetry::stage::BOX_PROPS);
                draw_box_props(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    &self.box_prop_runtime,
                    &self.box_prop_fall,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROPS);
                box_prop_profile_begin(telemetry::stage::BOX_PROP_DEBRIS);
                draw_box_prop_floor_debris(
                    BOX_PROPS,
                    &self.box_prop_broken,
                    &self.box_prop_runtime,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROP_DEBRIS);
                box_prop_profile_begin(telemetry::stage::BOX_PROP_SHARDS);
                draw_box_prop_break_events(
                    &self.box_prop_break_events,
                    BOX_PROPS,
                    &self.box_prop_runtime,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::BOX_PROP_SHARDS);
                box_prop_profile_begin(telemetry::stage::IMAGE_CARDS);
                draw_image_props(
                    IMAGE_PROPS,
                    active.index,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &mut primitive_packets,
                    &mut world,
                );
                box_prop_profile_end(telemetry::stage::IMAGE_CARDS);
                telemetry::stage_end(telemetry::stage::IMAGE_PROPS);
                telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
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
                if let Some(shadow_material) = self.shadow_material {
                    draw_model_instance_shadows(
                        active.index,
                        &room_camera,
                        actor_options,
                        shadow_material,
                        &self.models,
                        &mut primitive_packets,
                        &mut world,
                    );
                }
                let instance_stats = draw_model_instances(
                    active.index,
                    ctx.sim_tick,
                    ctx.video_hz,
                    &room_camera,
                    actor_options,
                    &lighting,
                    &self.models,
                    &self.model_faces[..self.model_face_count],
                    &self.model_parts[..self.model_part_count],
                    &self.model_vertices[..self.model_vertex_count],
                    &self.clips,
                    instance_depth_pass,
                    &mut primitive_packets,
                    &mut world,
                );
                telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
                accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
            }

            // Player draws through the same compact model path as
            // placed model instances.
            if let Some(character) = self.character {
                let player = self.motor.position();
                let player_lighting = self.current_room_lighting(camera);
                let actor_options = current_room_surface_options(self.room_index);
                telemetry::stage_begin(telemetry::stage::PLAYER);
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
                let player_draw =
                    player_lighting.map_or(PlayerModelDrawStats::default(), |lighting| {
                        draw_player(
                            character,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            player.x,
                            player.y,
                            player.z,
                            self.motor.yaw(),
                            self.anim_state.action(),
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
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
                            self.room_index,
                            character,
                            &self.models,
                            &self.model_faces[..self.model_face_count],
                            &self.model_parts[..self.model_part_count],
                            &self.model_vertices[..self.model_vertex_count],
                            &self.clips,
                            player.x,
                            player.y,
                            player.z,
                            self.motor.yaw(),
                            self.anim_state.action(),
                            character.clip_for(self.anim_state),
                            self.anim_start_tick,
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
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_ACTIVE_HITBOXES,
                    equipment_stats.active_hitboxes as u32,
                );
                telemetry::counter(
                    telemetry::counter::EQUIPMENT_TARGET_HITS,
                    equipment_stats.target_hits as u32,
                );
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
                for &active_slot in &active_draw_order {
                    if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                        continue;
                    }
                    let Some(active) = self.active_rooms[active_slot as usize] else {
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
                    };
                    telemetry::stage_begin(telemetry::stage::MODEL_INSTANCES);
                    let instance_stats = draw_model_instances(
                        active.index,
                        ctx.sim_tick,
                        ctx.video_hz,
                        &room_camera,
                        actor_options,
                        &lighting,
                        &self.models,
                        &self.model_faces[..self.model_face_count],
                        &self.model_parts[..self.model_part_count],
                        &self.model_vertices[..self.model_vertex_count],
                        &self.clips,
                        ModelInstanceDepthPass::InFrontOfPlayer(player_depth),
                        &mut primitive_packets,
                        &mut world,
                    );
                    telemetry::stage_end(telemetry::stage::MODEL_INSTANCES);
                    accumulate_model_instance_draw_stats(&mut total_instance_stats, instance_stats);
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
                RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
                self.portal_visibility_camera_global,
                yaw_q12_from_basis(debug_view.sin_yaw, debug_view.cos_yaw),
                debug_view.sin_yaw,
                debug_view.cos_yaw,
                debug_view.sin_pitch,
                debug_view.cos_pitch,
            );
            self.emit_portal_visibility_counters();
            #[cfg(feature = "cd-stream-bench")]
            unsafe {
                telemetry::counter(
                    telemetry::counter::ROOM_STREAM_RESIDENT_SLOTS,
                    ROOM_STREAM_SCHEDULER.resident_slot_count() as u32,
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_LO,
                    telemetry::counter::ROOM_STREAM_LOADING_MASK_HI,
                    ROOM_STREAM_SCHEDULER.loading_room_mask(),
                );
                emit_room_chunk_mask(
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_LO,
                    telemetry::counter::ROOM_STREAM_RESIDENT_MASK_HI,
                    ROOM_STREAM_SCHEDULER.resident_room_mask(),
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
                self.active_room_candidates as u32,
            );
            telemetry::counter(
                telemetry::counter::ROOM_CHUNK_CACHE_SKIPS,
                self.active_room_cache_skips as u32,
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
        let _ = self.draw_particle_emitters(camera, ctx.sim_tick, &mut ot, &mut primitive_packets);
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVES,
            primitive_packets.len() as u32,
        );
        telemetry::counter(
            telemetry::counter::TRI_PRIMITIVE_REMAINING,
            primitive_packets.remaining() as u32,
        );
        telemetry::counter(telemetry::counter::WORLD_COMMANDS, world_command_len as u32);
        // Split the submit so profiling can separate CPU build cost from
        // GPU draw cost: OT_SUBMIT times the DMA kick (CPU), OT_WAIT times
        // the blocking wait for the GPU/DMA walk. Waiting immediately after
        // the kick keeps behaviour identical to the old blocking submit().
        telemetry::stage_begin(telemetry::stage::OT_SUBMIT);
        let ot_in_flight = ot.submit_async();
        telemetry::stage_end(telemetry::stage::OT_SUBMIT);
        telemetry::stage_begin(telemetry::stage::OT_WAIT);
        ot_in_flight.wait();
        telemetry::stage_end(telemetry::stage::OT_WAIT);

        if let Some(room_record) = ROOMS.get(self.room_index.to_usize()) {
            draw_room_atmosphere_overlay(room_record, ctx.sim_tick);
        }

        if self.show_collision_debug {
            self.draw_collision_debug_overlay(camera);
        }

        if let Some(target) = self.lock_target_indicator_position() {
            draw_lock_target_indicator(target, camera, ctx.sim_tick);
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
                (ctx.sim_tick.as_u32() & 0xffff) as u16,
                self.motor.stamina_q12(),
                self.motor_config().stamina_max_q12,
            );
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
