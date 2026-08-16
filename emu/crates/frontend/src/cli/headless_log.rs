//! CSV logging sinks for headless `launch` runs: display-hash
//! checkpoints, per-guest-frame counters, and per-frame guest
//! telemetry profiles.

use std::io::{BufWriter, Write};
use std::path::Path;

use emulator_core::{telemetry, Bus};

use super::counter_total;

pub(super) struct DisplayHashLog {
    writer: Option<BufWriter<std::fs::File>>,
    interval: u64,
    checkpoint_kind: &'static str,
}

impl DisplayHashLog {
    pub(super) fn new(
        path: Option<&Path>,
        interval: u64,
        checkpoint_kind: &'static str,
    ) -> Result<Self, String> {
        let Some(path) = path else {
            return Ok(Self {
                writer: None,
                interval: interval.max(1),
                checkpoint_kind,
            });
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let file =
            std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "checkpoint_kind,checkpoint_frame,guest_frame,visual_frame,cpu_tick,bus_cycles,display_hash,width,height,byte_len"
        )
        .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(Self {
            writer: Some(writer),
            interval: interval.max(1),
            checkpoint_kind,
        })
    }

    pub(super) fn record(
        &mut self,
        checkpoint_frame: u64,
        guest_frame: u64,
        visual_frame: u64,
        cpu_tick: u64,
        bus_cycles: u64,
        bus: &Bus,
    ) -> Result<(), String> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        if !checkpoint_frame.is_multiple_of(self.interval) {
            return Ok(());
        }
        let (hash, width, height, byte_len) = bus.gpu.display_hash();
        writeln!(
            writer,
            "{},{checkpoint_frame},{guest_frame},{visual_frame},{cpu_tick},{bus_cycles},0x{hash:016x},{width},{height},{byte_len}",
            self.checkpoint_kind
        )
        .map_err(|e| format!("write visual hash log: {e}"))
    }

    pub(super) fn flush(&mut self) -> Result<(), String> {
        if let Some(writer) = self.writer.as_mut() {
            writer
                .flush()
                .map_err(|e| format!("flush visual hash log: {e}"))?;
        }
        Ok(())
    }
}

/// Per-guest-frame log of frametime + the portal/streaming masks and camera
/// position, so a streaming/visibility change can be measured frame-by-frame
/// and a drawn-but-not-visible room pinned to an exact camera position.
pub(super) struct CounterLog {
    writer: Option<BufWriter<std::fs::File>>,
}

impl CounterLog {
    pub(super) fn new(path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = path else {
            return Ok(Self { writer: None });
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let file =
            std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "guest_frame,cpu_tick,bus_cycles,resident_mask,active_mask,drawn_mask,visible_mask,cam_x_biased,cam_y_biased,cam_z_biased,current_room,player_x_biased,player_z_biased,player_facing_yaw_q12,view_yaw_q12,cam_local_x_biased,cam_local_z_biased"
        )
        .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(Self {
            writer: Some(writer),
        })
    }

    pub(super) fn record(
        &mut self,
        guest_frame: u64,
        cpu_tick: u64,
        bus_cycles: u64,
        bus: &Bus,
    ) -> Result<(), String> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        use telemetry::counter as c;
        let g = |id| bus.telemetry.counter_latest_value(id);
        writeln!(
            writer,
            "{guest_frame},{cpu_tick},{bus_cycles},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            g(c::ROOM_STREAM_RESIDENT_MASK_LO),
            g(c::ROOM_ACTIVE_CHUNK_MASK_LO),
            g(c::ROOM_DRAWN_CHUNK_MASK_LO),
            g(c::PORTAL_VIS_VISIBLE_MASK_LO),
            g(c::ROOM_CAMERA_GLOBAL_X_BIASED),
            g(c::ROOM_CAMERA_GLOBAL_Y_BIASED),
            g(c::ROOM_CAMERA_GLOBAL_Z_BIASED),
            g(c::PORTAL_VIS_CURRENT_ROOM),
            g(c::ROOM_PLAYER_LOCAL_X_BIASED),
            g(c::ROOM_PLAYER_LOCAL_Z_BIASED),
            g(c::PLAYER_FACING_YAW_Q12),
            g(c::ROOM_PLAYER_VIEW_YAW_Q12),
            g(c::ROOM_CAMERA_LOCAL_X_BIASED),
            g(c::ROOM_CAMERA_LOCAL_Z_BIASED),
        )
        .map_err(|e| format!("write counter log: {e}"))
    }

    pub(super) fn flush(&mut self) -> Result<(), String> {
        if let Some(writer) = self.writer.as_mut() {
            writer
                .flush()
                .map_err(|e| format!("flush counter log: {e}"))?;
        }
        Ok(())
    }
}

const PROFILE_LOG_HEADER: &[&str] = &[
    "guest_frame",
    "end_cpu_tick",
    "start_bus_cycles",
    "end_bus_cycles",
    "frame_cycles",
    "frame_markers",
    "fixed_update_task",
    "visual_render_task",
    "update",
    "render",
    "present",
    "frame_clear",
    "camera",
    "portal_visibility",
    "active_room_window",
    "room_surface_cache",
    "vram_upload",
    "cd_room_chunk_load",
    "room",
    "room_visible_list",
    "room_cell_select",
    "room_project",
    "room_depth_prep",
    "room_surface_draw",
    "sky",
    "far_vista",
    "image_props",
    "box_props",
    "box_prop_debris",
    "box_prop_shards",
    "image_cards",
    "model_instances",
    "model_bounds",
    "model_draw",
    "player",
    "player_bounds",
    "player_draw",
    "equipment",
    "textured_model_joints",
    "textured_model_project",
    "textured_model_faces",
    "world_flush",
    "ot_submit",
    "ot_wait",
    "sim_collision",
    "sim_room_track",
    "sim_residency",
    "sim_pump",
    "sim_solve",
    "update_actor",
    "update_window",
    "game_logic",
    "game_entities_thought",
    "player_melee_hits",
    "player_hits_taken",
    "cell_lookup",
    "cell_depth",
    "cell_collect",
    "sim_ticks",
    "visual_frames",
    "visual_skipped_vblanks",
    "visual_deadline_misses",
    "visual_lateness_vblanks",
    "cd_room_chunk_loads",
    "cd_room_chunk_bytes",
    "cd_room_chunk_sectors",
    "cd_room_chunk_status",
    "room_stream_requests",
    "room_stream_misses",
    "room_stream_prefetch_requests",
    "room_stream_pending_loads",
    "room_stream_evicts",
    "room_stream_failed_loads",
    "room_stream_resident_slots",
    "room_stream_loading_mask",
    "room_active_chunks",
    "room_visible_cells",
    "room_cells_drawn",
    "room_cells_considered",
    "room_cells_culled",
    "room_cells_range_culled",
    "room_vis_fallback_draws",
    "room_surf_material",
    "room_surf_projected",
    "room_surf_screen",
    "room_surf_kind",
    "room_surf_backface",
    "room_surf_lighting",
    "room_surf_submit",
    "room_surf_profiled",
    "room_surf_screen_culled",
    "room_surf_backface_culled",
    "room_submit_hw_safe_test",
    "room_submit_packet_fill",
    "room_submit_primitive_push",
    "room_submit_depth",
    "room_submit_command",
    "room_submit_fallback",
    "room_surfaces_considered",
    "room_projected_vertices",
    "room_surf_whole_quads",
    "room_surf_split_tris",
    "room_surf_tr_subdivision_candidates",
    "room_surf_tr_subdivision_submitted",
    // Warp probe: predicted affine texture error vs what the depth-band rule
    // actually did. See docs/texture-warping-2026-07-27.md.
    "warp_subdivided_count",
    "warp_subdivided_sum16",
    "warp_subdivided_max16",
    "warp_subdivided_under_1tx",
    "warp_untouched_count",
    "warp_untouched_sum16",
    "warp_untouched_max16",
    "warp_untouched_under_1tx",
    "room_surf_options",
    "room_surf_cell_setup",
    "room_surf_call",
    "room_surface_packets",
    "room_surface_commands",
    "tri_primitives",
    "tri_primitive_remaining",
    "world_commands",
    "room_submit_primitive_overflows",
    "model_instance_draws",
    "player_projected_vertices",
    "player_submitted_tris",
    "textured_model_parts",
    "textured_model_vertices",
    "textured_model_faces_counter",
    "textured_model_fast_submitted_tris",
    "textured_model_split_tris",
    "textured_model_packed_unclamped",
    "textured_model_packed_clamped",
    "textured_model_packed_general",
    "textured_model_fallback_faces",
    "resident_mask",
    "active_mask",
    "drawn_mask",
    "visible_mask",
    "missing_mask",
    "camera_x_biased",
    "camera_y_biased",
    "camera_z_biased",
    "player_view_yaw_q12",
    "current_room",
    "player_room",
];

struct ProfileFrame {
    guest_frame: u64,
    start_bus_cycles: u64,
    events: Vec<telemetry::GuestTelemetryEvent>,
}

/// Per-completed-guest-frame log of runtime telemetry, used to identify which
/// hot systems overlap on deadline-miss spikes in a recorded run.
pub(super) struct GuestProfileLog {
    writer: Option<BufWriter<std::fs::File>>,
    current_frame: Option<ProfileFrame>,
}

impl GuestProfileLog {
    pub(super) fn new(path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = path else {
            return Ok(Self {
                writer: None,
                current_frame: None,
            });
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let file =
            std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{}", PROFILE_LOG_HEADER.join(","))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(Self {
            writer: Some(writer),
            current_frame: None,
        })
    }

    pub(super) fn add_events(
        &mut self,
        events: &[telemetry::GuestTelemetryEvent],
        end_cpu_tick: u64,
        end_bus_cycles: u64,
    ) -> Result<(), String> {
        if self.writer.is_none() {
            return Ok(());
        }
        for &event in events {
            if matches!(event.kind, telemetry::GuestTelemetryKind::FrameBegin) {
                self.flush_current(end_cpu_tick, event.cycles)?;
                self.current_frame = Some(ProfileFrame {
                    guest_frame: event.value as u64,
                    start_bus_cycles: event.cycles,
                    events: vec![event],
                });
            } else if let Some(frame) = self.current_frame.as_mut() {
                frame.events.push(event);
            }
        }
        if events.is_empty() {
            self.flush_current(end_cpu_tick, end_bus_cycles)?;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self, end_cpu_tick: u64, end_bus_cycles: u64) -> Result<(), String> {
        self.flush_current(end_cpu_tick, end_bus_cycles)
    }

    pub(super) fn flush(&mut self) -> Result<(), String> {
        if let Some(writer) = self.writer.as_mut() {
            writer
                .flush()
                .map_err(|e| format!("flush profile log: {e}"))?;
        }
        Ok(())
    }

    fn flush_current(&mut self, end_cpu_tick: u64, end_bus_cycles: u64) -> Result<(), String> {
        let Some(frame) = self.current_frame.take() else {
            return Ok(());
        };
        self.write_frame(frame, end_cpu_tick, end_bus_cycles)
    }

    fn write_frame(
        &mut self,
        frame: ProfileFrame,
        end_cpu_tick: u64,
        end_bus_cycles: u64,
    ) -> Result<(), String> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        let summary = telemetry::GuestTelemetrySummary::from_events(&frame.events);
        let stage = |id: u16| {
            summary
                .stage_cycles
                .get(id as usize)
                .copied()
                .unwrap_or_default()
        };
        let task = |id: u16| {
            summary
                .task_cycles
                .get(id as usize)
                .copied()
                .unwrap_or_default()
        };
        let counter_latest = |id: u16| {
            summary
                .counter_latest_values
                .get(id as usize)
                .copied()
                .unwrap_or_default()
        };
        let mut row = Vec::with_capacity(PROFILE_LOG_HEADER.len());
        macro_rules! push {
            ($value:expr) => {
                row.push(($value).to_string())
            };
        }

        use telemetry::{counter as c, stage as s, task as t};
        push!(frame.guest_frame);
        push!(end_cpu_tick);
        push!(frame.start_bus_cycles);
        push!(end_bus_cycles);
        push!(end_bus_cycles.saturating_sub(frame.start_bus_cycles));
        push!(summary.frames);
        push!(task(t::FIXED_UPDATE));
        push!(task(t::VISUAL_RENDER));
        push!(stage(s::UPDATE));
        push!(stage(s::RENDER));
        push!(stage(s::PRESENT));
        push!(stage(s::FRAME_CLEAR));
        push!(stage(s::CAMERA));
        push!(stage(s::PORTAL_VISIBILITY));
        push!(stage(s::ACTIVE_ROOM_WINDOW));
        push!(stage(s::ROOM_SURFACE_CACHE));
        push!(stage(s::VRAM_UPLOAD));
        push!(stage(s::CD_ROOM_CHUNK_LOAD));
        push!(stage(s::ROOM));
        push!(stage(s::ROOM_VISIBLE_LIST));
        push!(stage(s::ROOM_CELL_SELECT));
        push!(stage(s::ROOM_PROJECT));
        push!(stage(s::ROOM_DEPTH_PREP));
        push!(stage(s::ROOM_SURFACE_DRAW));
        push!(stage(s::SKY));
        push!(stage(s::FAR_VISTA));
        push!(stage(s::IMAGE_PROPS));
        push!(stage(s::BOX_PROPS));
        push!(stage(s::BOX_PROP_DEBRIS));
        push!(stage(s::BOX_PROP_SHARDS));
        push!(stage(s::IMAGE_CARDS));
        push!(stage(s::MODEL_INSTANCES));
        push!(stage(s::MODEL_BOUNDS));
        push!(stage(s::MODEL_DRAW));
        push!(stage(s::PLAYER));
        push!(stage(s::PLAYER_BOUNDS));
        push!(stage(s::PLAYER_DRAW));
        push!(stage(s::EQUIPMENT));
        push!(stage(s::TEXTURED_MODEL_JOINTS));
        push!(stage(s::TEXTURED_MODEL_PROJECT));
        push!(stage(s::TEXTURED_MODEL_FACES));
        push!(stage(s::WORLD_FLUSH));
        push!(stage(s::OT_SUBMIT));
        push!(stage(s::OT_WAIT));
        push!(stage(s::SIM_COLLISION));
        push!(stage(s::SIM_ROOM_TRACK));
        push!(stage(s::SIM_RESIDENCY));
        push!(stage(s::SIM_PUMP));
        push!(stage(s::SIM_SOLVE));
        push!(stage(s::UPDATE_ACTOR));
        push!(stage(s::UPDATE_WINDOW));
        push!(stage(s::GAME_LOGIC));
        push!(counter_latest(c::GAME_ENTITIES_THOUGHT));
        push!(counter_latest(c::PLAYER_MELEE_HITS));
        push!(counter_latest(c::PLAYER_HITS_TAKEN));
        push!(stage(s::CELL_LOOKUP));
        push!(stage(s::CELL_DEPTH));
        push!(stage(s::CELL_COLLECT));
        push!(counter_total(&summary, c::SIM_TICKS));
        push!(counter_total(&summary, c::VISUAL_FRAMES));
        push!(counter_total(&summary, c::VISUAL_SKIPPED_VBLANKS));
        push!(counter_total(&summary, c::VISUAL_DEADLINE_MISSES));
        push!(counter_latest(c::VISUAL_MAX_LATENESS_VBLANKS));
        push!(counter_total(&summary, c::CD_ROOM_CHUNK_LOADS));
        push!(counter_total(&summary, c::CD_ROOM_CHUNK_BYTES));
        push!(counter_total(&summary, c::CD_ROOM_CHUNK_SECTORS));
        push!(counter_latest(c::CD_ROOM_CHUNK_STATUS));
        push!(counter_total(&summary, c::ROOM_STREAM_REQUESTS));
        push!(counter_total(&summary, c::ROOM_STREAM_MISSES));
        push!(counter_total(&summary, c::ROOM_STREAM_PREFETCH_REQUESTS));
        push!(counter_total(&summary, c::ROOM_STREAM_PENDING_LOADS));
        push!(counter_total(&summary, c::ROOM_STREAM_EVICTIONS));
        push!(counter_total(&summary, c::ROOM_STREAM_FAILED_LOADS));
        push!(counter_latest(c::ROOM_STREAM_RESIDENT_SLOTS));
        push!(counter_latest(c::ROOM_STREAM_LOADING_MASK_LO));
        push!(counter_total(&summary, c::ROOM_ACTIVE_CHUNKS));
        push!(counter_total(&summary, c::ROOM_VISIBLE_CELLS));
        push!(counter_total(&summary, c::ROOM_CELLS_DRAWN));
        push!(counter_total(&summary, c::ROOM_CELLS_CONSIDERED));
        push!(counter_total(&summary, c::ROOM_CELLS_CULLED));
        push!(counter_total(&summary, c::ROOM_CELLS_RANGE_CULLED));
        push!(counter_total(&summary, c::ROOM_VISIBILITY_FALLBACK_DRAWS));
        push!(counter_total(&summary, c::ROOM_SURF_MATERIAL_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_PROJECTED_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_SCREEN_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_KIND_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_BACKFACE_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_LIGHTING_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_SUBMIT_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_PROFILED));
        push!(counter_total(&summary, c::ROOM_SURF_SCREEN_CULLED));
        push!(counter_total(&summary, c::ROOM_SURF_BACKFACE_CULLED));
        push!(counter_total(&summary, c::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES));
        push!(counter_total(&summary, c::ROOM_SUBMIT_PACKET_FILL_CYCLES));
        push!(counter_total(
            &summary,
            c::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES
        ));
        push!(counter_total(&summary, c::ROOM_SUBMIT_DEPTH_CYCLES));
        push!(counter_total(&summary, c::ROOM_SUBMIT_COMMAND_CYCLES));
        push!(counter_total(&summary, c::ROOM_SUBMIT_FALLBACK_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURFACES_CONSIDERED));
        push!(counter_total(&summary, c::ROOM_PROJECTED_VERTICES));
        push!(counter_total(&summary, c::ROOM_SURF_WHOLE_QUADS));
        push!(counter_total(&summary, c::ROOM_SURF_SPLIT_TRIS));
        push!(counter_total(
            &summary,
            c::ROOM_SURF_TR_SUBDIVISION_CANDIDATES
        ));
        push!(counter_total(
            &summary,
            c::ROOM_SURF_TR_SUBDIVISION_SUBMITTED
        ));
        push!(counter_total(&summary, c::ROOM_SURF_WARP_SUBDIVIDED_COUNT));
        push!(counter_total(&summary, c::ROOM_SURF_WARP_SUBDIVIDED_SUM));
        push!(counter_latest(c::ROOM_SURF_WARP_SUBDIVIDED_MAX));
        push!(counter_total(
            &summary,
            c::ROOM_SURF_WARP_SUBDIVIDED_UNDER_1TX
        ));
        push!(counter_total(&summary, c::ROOM_SURF_WARP_UNTOUCHED_COUNT));
        push!(counter_total(&summary, c::ROOM_SURF_WARP_UNTOUCHED_SUM));
        push!(counter_latest(c::ROOM_SURF_WARP_UNTOUCHED_MAX));
        push!(counter_total(
            &summary,
            c::ROOM_SURF_WARP_UNTOUCHED_UNDER_1TX
        ));
        push!(counter_total(&summary, c::ROOM_SURF_OPTIONS_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_CELL_SETUP_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURF_CALL_CYCLES));
        push!(counter_total(&summary, c::ROOM_SURFACE_PACKETS));
        push!(counter_total(&summary, c::ROOM_SURFACE_COMMANDS));
        push!(counter_total(&summary, c::TRI_PRIMITIVES));
        push!(counter_latest(c::TRI_PRIMITIVE_REMAINING));
        push!(counter_total(&summary, c::WORLD_COMMANDS));
        push!(counter_total(&summary, c::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS));
        push!(counter_total(&summary, c::MODEL_INSTANCE_DRAWS));
        push!(counter_total(&summary, c::PLAYER_PROJECTED_VERTICES));
        push!(counter_total(&summary, c::PLAYER_SUBMITTED_TRIS));
        push!(counter_total(&summary, c::TEXTURED_MODEL_PARTS));
        push!(counter_total(&summary, c::TEXTURED_MODEL_VERTICES));
        push!(counter_total(&summary, c::TEXTURED_MODEL_FACES));
        push!(counter_total(
            &summary,
            c::TEXTURED_MODEL_FAST_SUBMITTED_TRIS
        ));
        push!(counter_total(&summary, c::TEXTURED_MODEL_SPLIT_TRIS));
        push!(counter_total(
            &summary,
            c::TEXTURED_MODEL_PACKED_UNCLAMPED_CALLS
        ));
        push!(counter_total(
            &summary,
            c::TEXTURED_MODEL_PACKED_CLAMPED_CALLS
        ));
        push!(counter_total(
            &summary,
            c::TEXTURED_MODEL_PACKED_GENERAL_CALLS
        ));
        push!(counter_total(
            &summary,
            c::TEXTURED_MODEL_FALLBACK_FACE_CALLS
        ));
        push!(counter_latest(c::ROOM_STREAM_RESIDENT_MASK_LO));
        push!(counter_latest(c::ROOM_ACTIVE_CHUNK_MASK_LO));
        push!(counter_latest(c::ROOM_DRAWN_CHUNK_MASK_LO));
        push!(counter_latest(c::PORTAL_VIS_VISIBLE_MASK_LO));
        push!(counter_latest(c::PORTAL_VIS_MISSING_MASK_LO));
        push!(counter_latest(c::ROOM_CAMERA_GLOBAL_X_BIASED));
        push!(counter_latest(c::ROOM_CAMERA_GLOBAL_Y_BIASED));
        push!(counter_latest(c::ROOM_CAMERA_GLOBAL_Z_BIASED));
        push!(counter_latest(c::ROOM_PLAYER_VIEW_YAW_Q12));
        push!(counter_latest(c::PORTAL_VIS_CURRENT_ROOM));
        push!(counter_latest(c::ROOM_PLAYER_ROOM_INDEX));

        debug_assert_eq!(row.len(), PROFILE_LOG_HEADER.len());
        writeln!(writer, "{}", row.join(",")).map_err(|e| format!("write profile log: {e}"))
    }
}
