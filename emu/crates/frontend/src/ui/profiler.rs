//! Frame-profiler data + UI.
//!
//! The frontend records one sample per redraw. Samples are cheap wall-clock
//! spans around the existing single-threaded shell stages, so the profiler can
//! answer whether a slow frame is dominated by guest execution, CPU VRAM
//! uploads, hardware-render replay, or egui/wgpu presentation. Guest cycle
//! budget metrics are tracked separately so PS1 workload does not get hidden
//! behind fast host wall-clock timings.

use std::{collections::VecDeque, fmt::Write as _};

use egui::{Color32, RichText};
use emulator_core::telemetry::{
    counter, counter_name, stage, stage_name, task_name, GuestTelemetryEvent, COUNTER_COUNT,
    STAGE_COUNT, TASK_COUNT,
};

use crate::theme;

const HISTORY_CAP: usize = 500;
const LOG_INTERVAL_MS: f32 = 1000.0;
const LIVE_AVERAGE_WINDOW_MS: f32 = 100.0;
const BUDGET_60_MS: f32 = 1000.0 / 60.0;
const BUDGET_30_MS: f32 = 1000.0 / 30.0;
const PSX_MASTER_CLOCK_HZ: f32 = 33_868_800.0;
const PSX_CYCLES_PER_MS: f32 = PSX_MASTER_CLOCK_HZ / 1000.0;
const NTSC_CPU_CYCLES_PER_VBLANK: f32 = PSX_MASTER_CLOCK_HZ / 60.0;
const VISUAL_FRAME_INTERVAL_CAP: usize = 4;
const GUEST_RENDER_BREAKDOWN_STAGES: &[(u16, &str)] = &[
    (stage::SKY, "sky"),
    (stage::FAR_VISTA, "far vista"),
    (stage::ROOM, "room"),
    (stage::ENTITY_MARKERS, "markers"),
    (stage::IMAGE_PROPS, "image props"),
    (stage::MODEL_INSTANCES, "models"),
    (stage::PLAYER, "player"),
    (stage::EQUIPMENT, "equipment"),
    (stage::WORLD_FLUSH, "flush/sort"),
    (stage::OT_SUBMIT, "ot submit"),
    (stage::OT_WAIT, "ot wait"),
];
const PROFILE_LOG_STAGE_PER_VISUAL_FIELDS: &[(u16, &str)] = &[
    (stage::FRAME_CLEAR, "guest_clear_v"),
    (stage::RENDER, "guest_render_v"),
    (stage::SKY, "guest_sky_v"),
    (stage::FAR_VISTA, "guest_vista_v"),
    (stage::ROOM, "guest_room_v"),
    (stage::ROOM_VISIBLE_LIST, "guest_room_list_v"),
    (stage::ROOM_CELL_SELECT, "guest_room_select_v"),
    (stage::ROOM_PROJECT, "guest_room_project_v"),
    (stage::ROOM_DEPTH_PREP, "guest_room_depth_v"),
    (stage::ROOM_SURFACE_DRAW, "guest_room_surface_v"),
    (stage::ENTITY_MARKERS, "guest_markers_v"),
    (stage::IMAGE_PROPS, "guest_props_v"),
    (stage::MODEL_INSTANCES, "guest_models_v"),
    (stage::PLAYER, "guest_player_v"),
    (stage::EQUIPMENT, "guest_equipment_v"),
    (stage::WORLD_FLUSH, "guest_flush_v"),
    (stage::OT_SUBMIT, "guest_ot_v"),
    (stage::OT_WAIT, "guest_ot_wait_v"),
    (stage::PRESENT, "guest_present_v"),
];
const PROFILE_LOG_STAGE_PER_HIT_FIELDS: &[(u16, &str)] = &[
    (stage::UPDATE, "guest_update_hit"),
    (stage::CAMERA, "guest_camera_hit"),
    (stage::PORTAL_VISIBILITY, "guest_portal_vis_hit"),
    (stage::ACTIVE_ROOM_WINDOW, "guest_room_window_hit"),
    (stage::ROOM_SURFACE_CACHE, "guest_room_cache_hit"),
    (stage::VRAM_UPLOAD, "guest_vram_hit"),
    (stage::MODEL_BOUNDS, "guest_model_bounds_hit"),
    (stage::MODEL_DRAW, "guest_model_draw_hit"),
    (stage::PLAYER_BOUNDS, "guest_player_bounds_hit"),
    (stage::PLAYER_DRAW, "guest_player_draw_hit"),
    (stage::TEXTURED_MODEL_JOINTS, "guest_mdl_joints_hit"),
    (stage::TEXTURED_MODEL_PROJECT, "guest_mdl_project_hit"),
    (stage::TEXTURED_MODEL_FACES, "guest_mdl_faces_hit"),
];
const PROFILE_LOG_TASK_PER_HIT_FIELDS: &[(u16, &str)] = &[
    (
        emulator_core::telemetry::task::FIXED_UPDATE,
        "task_fixed_hit",
    ),
    (
        emulator_core::telemetry::task::VISUAL_RENDER,
        "task_visual_hit",
    ),
];
const PROFILE_LOG_COUNTER_PER_VISUAL_FIELDS: &[(u16, &str)] = &[
    (counter::ROOM_ACTIVE_CHUNKS, "room_chunks_v"),
    (counter::ROOM_CACHED_DRAWS, "room_cached_v"),
    (counter::ROOM_UNCACHED_DRAWS, "room_uncached_v"),
    (counter::ROOM_CACHE_FALLBACK_DRAWS, "room_cache_fb_v"),
    (
        counter::ROOM_VISIBILITY_FALLBACK_DRAWS,
        "room_visibility_fb_v",
    ),
    (counter::ROOM_CACHE_CELLS, "room_cache_cells_v"),
    (counter::ROOM_CACHE_VERTICES, "room_cache_verts_v"),
    (counter::ROOM_CACHE_SURFACES, "room_cache_surfaces_v"),
    (counter::ROOM_VISIBLE_CELLS, "room_visible_cells_v"),
    (counter::ROOM_CELLS_CONSIDERED, "room_cells_v"),
    (counter::ROOM_CELLS_DRAWN, "room_cells_drawn_v"),
    (counter::ROOM_CELLS_CULLED, "room_cells_culled_v"),
    (counter::ROOM_CELLS_RANGE_CULLED, "room_range_culled_v"),
    (counter::ROOM_SURFACES_CONSIDERED, "room_surfaces_v"),
    (counter::ROOM_PROJECTED_VERTICES, "room_projected_verts_v"),
    (counter::TRI_PRIMITIVES, "tri_prims_v"),
    (counter::TRI_PRIMITIVE_REMAINING, "tri_free_v"),
    (counter::WORLD_COMMANDS, "world_cmds_v"),
    (counter::MODEL_INSTANCE_DRAWS, "model_draws_v"),
    (counter::MODEL_INSTANCE_BOUNDS_TESTS, "model_bounds_v"),
    (
        counter::MODEL_INSTANCE_BOUNDS_CULLED,
        "model_bounds_culled_v",
    ),
    (counter::MODEL_INSTANCE_PROJECTED_VERTICES, "model_verts_v"),
    (counter::MODEL_INSTANCE_SUBMITTED_TRIS, "model_tris_v"),
    (counter::PLAYER_BOUNDS_TESTS, "player_bounds_v"),
    (counter::PLAYER_BOUNDS_CULLED, "player_bounds_culled_v"),
    (counter::PLAYER_PROJECTED_VERTICES, "player_verts_v"),
    (counter::PLAYER_SUBMITTED_TRIS, "player_tris_v"),
    (
        counter::TEXTURED_MODEL_CPU_BLEND_VERTICES,
        "mdl_cpu_blend_verts_v",
    ),
    (
        counter::TEXTURED_MODEL_PACKED_FACE_CALLS,
        "mdl_packed_faces_v",
    ),
    (
        counter::TEXTURED_MODEL_PACKED_UNCLAMPED_CALLS,
        "mdl_packed_unclamped_v",
    ),
    (
        counter::TEXTURED_MODEL_PACKED_CLAMPED_CALLS,
        "mdl_packed_clamped_v",
    ),
    (
        counter::TEXTURED_MODEL_PACKED_GENERAL_CALLS,
        "mdl_packed_general_v",
    ),
    (
        counter::TEXTURED_MODEL_FALLBACK_FACE_CALLS,
        "mdl_fallback_faces_v",
    ),
    (
        counter::TEXTURED_MODEL_HW_EXTENT_FALLBACKS,
        "mdl_hw_fallback_v",
    ),
    (counter::TEXTURED_MODEL_NEAR_DROPS, "mdl_near_drops_v"),
    (
        counter::TEXTURED_MODEL_HW_UNSAFE_DROPS,
        "mdl_hw_unsafe_drops_v",
    ),
    (
        counter::TEXTURED_MODEL_FAST_SUBMITTED_TRIS,
        "mdl_fast_tris_v",
    ),
    (counter::TEXTURED_MODEL_SPLIT_TRIS, "mdl_split_tris_v"),
    (counter::TEXTURED_MODEL_SKIPPED_TRIS, "mdl_skipped_tris_v"),
    (
        counter::TEXTURED_MODEL_CPU_BLEND_SUBMITS,
        "mdl_cpu_blend_submits_v",
    ),
    (
        counter::TEXTURED_MODEL_PRIMARY_JOINT_SUBMITS,
        "mdl_primary_submits_v",
    ),
    (
        counter::TEXTURED_MODEL_ALL_FRONT_SUBMITS,
        "mdl_all_front_submits_v",
    ),
    (
        counter::TEXTURED_MODEL_ALL_HW_BOUNDS_SUBMITS,
        "mdl_all_hw_bounds_submits_v",
    ),
    (counter::EQUIPMENT_DRAWS, "equipment_draws_v"),
    (counter::EQUIPMENT_PROJECTED_VERTICES, "equipment_verts_v"),
    (counter::EQUIPMENT_SUBMITTED_TRIS, "equipment_tris_v"),
    (counter::ROOM_SURF_MATERIAL_CYCLES, "surf_material_cyc_v"),
    (counter::ROOM_SURF_PROJECTED_CYCLES, "surf_projected_cyc_v"),
    (counter::ROOM_SURF_SCREEN_CYCLES, "surf_screen_cyc_v"),
    (counter::ROOM_SURF_KIND_CYCLES, "surf_kind_cyc_v"),
    (counter::ROOM_SURF_BACKFACE_CYCLES, "surf_backface_cyc_v"),
    (counter::ROOM_SURF_LIGHTING_CYCLES, "surf_lighting_cyc_v"),
    (counter::ROOM_SURF_SUBMIT_CYCLES, "surf_submit_cyc_v"),
    (counter::ROOM_SURF_PROFILED, "surf_profiled_v"),
    (counter::ROOM_SURF_SCREEN_CULLED, "surf_screen_culled_v"),
    (counter::ROOM_SURF_BACKFACE_CULLED, "surf_backface_culled_v"),
    (
        counter::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES,
        "submit_hw_test_cyc_v",
    ),
    (
        counter::ROOM_SUBMIT_PACKET_FILL_CYCLES,
        "submit_packet_cyc_v",
    ),
    (
        counter::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES,
        "submit_prim_push_cyc_v",
    ),
    (counter::ROOM_SUBMIT_DEPTH_CYCLES, "submit_depth_cyc_v"),
    (counter::ROOM_SUBMIT_COMMAND_CYCLES, "submit_command_cyc_v"),
    (
        counter::ROOM_SUBMIT_FALLBACK_CYCLES,
        "submit_fallback_cyc_v",
    ),
    (
        counter::ROOM_SURF_TR_SUBDIVISION_CANDIDATES,
        "surf_tr_candidates_v",
    ),
    (
        counter::ROOM_SURF_TR_SUBDIVISION_SUBMITTED,
        "surf_tr_submitted_v",
    ),
    (counter::ROOM_SUBMIT_HW_SAFE_CALLS, "submit_hw_calls_v"),
    (
        counter::ROOM_SUBMIT_FALLBACK_CALLS,
        "submit_fallback_calls_v",
    ),
    (counter::ROOM_STREAM_REQUESTS, "stream_req_v"),
    (counter::ROOM_STREAM_MISSES, "stream_miss_v"),
    (counter::ROOM_STREAM_PREFETCH_REQUESTS, "stream_prefetch_v"),
    (counter::ROOM_STREAM_RESIDENT_SLOTS, "stream_resident_v"),
    (counter::ROOM_STREAM_PENDING_LOADS, "stream_pending_v"),
    (counter::ROOM_STREAM_EVICTIONS, "stream_evict_v"),
    (counter::ROOM_STREAM_PROTECTED_FULL, "stream_full_v"),
    (
        counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT,
        "stream_missing_v",
    ),
];

/// Timing breakdown returned by [`crate::gfx::Graphics::render`].
#[derive(Clone, Copy, Debug, Default)]
pub struct EguiRenderProfile {
    /// egui-winit input conversion.
    pub input_ms: f32,
    /// User UI closure, including all panels.
    pub ui_ms: f32,
    /// Platform-output handoff.
    pub platform_output_ms: f32,
    /// Shape tessellation.
    pub tessellate_ms: f32,
    /// Surface acquisition.
    pub surface_ms: f32,
    /// egui texture updates.
    pub texture_update_ms: f32,
    /// egui vertex/index buffer updates.
    pub buffer_update_ms: f32,
    /// egui render pass encoding.
    pub paint_ms: f32,
    /// Queue submit, pre-present notify, and surface present.
    pub submit_present_ms: f32,
    /// Full [`crate::gfx::Graphics::render`] wall time.
    pub total_ms: f32,
}

/// Guest-runtime profiler data emitted by instrumented homebrew.
#[derive(Clone, Copy, Debug)]
pub struct GuestRuntimeProfile {
    /// Number of guest frame-begin markers observed.
    pub frames: f32,
    /// Total cycle spans per guest stage id.
    pub stage_cycles: [f32; STAGE_COUNT],
    /// Completed span count per guest stage id.
    pub stage_hits: [f32; STAGE_COUNT],
    /// Largest single completed stage span per id.
    pub stage_max_cycles: [f32; STAGE_COUNT],
    /// Total cycle spans per scheduled guest task id.
    pub task_cycles: [f32; TASK_COUNT],
    /// Completed span count per scheduled guest task id.
    pub task_hits: [f32; TASK_COUNT],
    /// Largest single completed scheduled-task span per id.
    pub task_max_cycles: [f32; TASK_COUNT],
    /// Summed counter values per guest counter id.
    pub counters: [f32; COUNTER_COUNT],
    /// Largest single value observed per guest counter id.
    pub counter_max_values: [f32; COUNTER_COUNT],
    /// Last value observed per guest counter id.
    pub counter_latest_values: [u32; COUNTER_COUNT],
    /// Guest cycles between consecutive paced visual-frame markers.
    visual_frame_interval_cycles: [f32; VISUAL_FRAME_INTERVAL_CAP],
    /// Number of populated entries in `visual_frame_interval_cycles`.
    visual_frame_interval_count: u8,
    /// Sum of all measured visual-frame intervals represented by this profile.
    visual_frame_interval_cycle_total: f32,
    /// Number of measured visual-frame intervals represented by this profile.
    visual_frame_interval_hits: f32,
}

impl Default for GuestRuntimeProfile {
    fn default() -> Self {
        Self {
            frames: 0.0,
            stage_cycles: [0.0; STAGE_COUNT],
            stage_hits: [0.0; STAGE_COUNT],
            stage_max_cycles: [0.0; STAGE_COUNT],
            task_cycles: [0.0; TASK_COUNT],
            task_hits: [0.0; TASK_COUNT],
            task_max_cycles: [0.0; TASK_COUNT],
            counters: [0.0; COUNTER_COUNT],
            counter_max_values: [0.0; COUNTER_COUNT],
            counter_latest_values: [0; COUNTER_COUNT],
            visual_frame_interval_cycles: [0.0; VISUAL_FRAME_INTERVAL_CAP],
            visual_frame_interval_count: 0,
            visual_frame_interval_cycle_total: 0.0,
            visual_frame_interval_hits: 0.0,
        }
    }
}

impl GuestRuntimeProfile {
    fn append_visual_frame_interval(&mut self, cycles: f32) {
        if !cycles.is_finite() || cycles <= 0.0 {
            return;
        }
        let count = self.visual_frame_interval_count as usize;
        if count < VISUAL_FRAME_INTERVAL_CAP {
            self.visual_frame_interval_cycles[count] = cycles;
            self.visual_frame_interval_count += 1;
        } else {
            self.visual_frame_interval_cycles.rotate_left(1);
            self.visual_frame_interval_cycles[VISUAL_FRAME_INTERVAL_CAP - 1] = cycles;
        }
    }

    fn accumulate(&mut self, other: Self) {
        self.frames += other.frames;
        let mut i = 0;
        while i < STAGE_COUNT {
            self.stage_cycles[i] += other.stage_cycles[i];
            self.stage_hits[i] += other.stage_hits[i];
            self.stage_max_cycles[i] = self.stage_max_cycles[i].max(other.stage_max_cycles[i]);
            i += 1;
        }
        let mut task_index = 0;
        while task_index < TASK_COUNT {
            self.task_cycles[task_index] += other.task_cycles[task_index];
            self.task_hits[task_index] += other.task_hits[task_index];
            self.task_max_cycles[task_index] =
                self.task_max_cycles[task_index].max(other.task_max_cycles[task_index]);
            task_index += 1;
        }
        let mut j = 0;
        while j < COUNTER_COUNT {
            self.counters[j] += other.counters[j];
            self.counter_max_values[j] =
                self.counter_max_values[j].max(other.counter_max_values[j]);
            if other.counter_latest_values[j] > 0
                || other.counter_max_values[j] > 0.0
                || other.counters[j] > 0.0
            {
                self.counter_latest_values[j] = other.counter_latest_values[j];
            }
            j += 1;
        }
        self.visual_frame_interval_cycle_total += other.visual_frame_interval_cycle_total;
        self.visual_frame_interval_hits += other.visual_frame_interval_hits;
        for &cycles in other
            .visual_frame_interval_cycles
            .iter()
            .take(other.visual_frame_interval_count as usize)
        {
            self.append_visual_frame_interval(cycles);
        }
    }

    fn divide(&mut self, n: f32) {
        self.frames /= n;
        let mut i = 0;
        while i < STAGE_COUNT {
            self.stage_cycles[i] /= n;
            self.stage_hits[i] /= n;
            i += 1;
        }
        let mut task_index = 0;
        while task_index < TASK_COUNT {
            self.task_cycles[task_index] /= n;
            self.task_hits[task_index] /= n;
            task_index += 1;
        }
        let mut j = 0;
        while j < COUNTER_COUNT {
            self.counters[j] /= n;
            j += 1;
        }
        self.visual_frame_interval_cycle_total /= n;
        self.visual_frame_interval_hits /= n;
    }

    fn has_data(self) -> bool {
        self.frames > 0.0
            || self.stage_cycles.iter().any(|&cycles| cycles > 0.0)
            || self.task_cycles.iter().any(|&cycles| cycles > 0.0)
            || self.counters.iter().any(|&value| value > 0.0)
    }

    fn cycle_budget_per_guest_frame(self) -> f32 {
        if self.frames > 0.0 {
            PSX_MASTER_CLOCK_HZ / 60.0
        } else {
            0.0
        }
    }

    fn stage_cycles_per_guest_frame(self, stage_id: usize) -> f32 {
        per_guest_frame(self.stage_cycles[stage_id], self.frames)
    }

    fn stage_cycles_per_hit(self, stage_id: usize) -> f32 {
        if self.stage_hits[stage_id] > 0.0 {
            self.stage_cycles[stage_id] / self.stage_hits[stage_id]
        } else {
            0.0
        }
    }

    fn task_cycles_per_hit(self, task_id: usize) -> f32 {
        if self.task_hits[task_id] > 0.0 {
            self.task_cycles[task_id] / self.task_hits[task_id]
        } else {
            0.0
        }
    }

    fn counter_per_guest_frame(self, counter_id: usize) -> f32 {
        per_guest_frame(self.counters[counter_id], self.frames)
    }

    fn counter_per_visual_frame(self, counter_id: usize) -> f32 {
        let visual_frames = self.counter_total(counter::VISUAL_FRAMES as usize);
        if visual_frames > 0.0 {
            self.counters[counter_id] / visual_frames
        } else {
            self.counter_per_guest_frame(counter_id)
        }
    }

    fn counter_total(self, counter_id: usize) -> f32 {
        self.counters[counter_id]
    }

    pub(crate) fn counter_max_value(self, counter_id: usize) -> f32 {
        self.counter_max_values[counter_id]
    }

    pub(crate) fn counter_latest_value(self, counter_id: usize) -> u32 {
        self.counter_latest_values[counter_id]
    }

    fn has_counter_observation(self, counter_id: usize) -> bool {
        self.counter_latest_value(counter_id) > 0
            || self.counter_max_value(counter_id) > 0.0
            || self.counter_total(counter_id) > 0.0
    }

    fn has_pacing_data(self) -> bool {
        self.counter_total(emulator_core::telemetry::counter::SIM_TICKS as usize) > 0.0
            || self.counter_total(emulator_core::telemetry::counter::VISUAL_FRAMES as usize) > 0.0
            || self
                .counter_total(emulator_core::telemetry::counter::VISUAL_DEADLINE_MISSES as usize)
                > 0.0
    }

    fn visual_frame_interval_cycles(self) -> Option<f32> {
        if self.visual_frame_interval_hits > 0.0 {
            Some(self.visual_frame_interval_cycle_total / self.visual_frame_interval_hits)
        } else {
            None
        }
    }

    pub(crate) fn visual_frame_intervals_ms(self) -> ([f32; VISUAL_FRAME_INTERVAL_CAP], u8) {
        let mut intervals_ms = [0.0; VISUAL_FRAME_INTERVAL_CAP];
        for (interval_ms, &cycles) in intervals_ms.iter_mut().zip(
            self.visual_frame_interval_cycles
                .iter()
                .take(self.visual_frame_interval_count as usize),
        ) {
            *interval_ms = cycles / PSX_CYCLES_PER_MS;
        }
        (intervals_ms, self.visual_frame_interval_count)
    }

    fn visual_interval_vblanks(self) -> f32 {
        if self.frames > 0.0 {
            self.counter_total(emulator_core::telemetry::counter::VISUAL_INTERVAL_VBLANKS as usize)
                / self.frames
        } else {
            0.0
        }
    }

    fn render_cycles_per_visual_frame(self) -> f32 {
        let visual_frames =
            self.counter_total(emulator_core::telemetry::counter::VISUAL_FRAMES as usize);
        if visual_frames > 0.0 {
            self.stage_cycles[emulator_core::telemetry::stage::RENDER as usize] / visual_frames
        } else {
            0.0
        }
    }

    fn stage_cycles_per_visual_frame(self, stage_id: usize) -> f32 {
        let visual_frames = self.counter_total(counter::VISUAL_FRAMES as usize);
        if visual_frames > 0.0 {
            self.stage_cycles[stage_id] / visual_frames
        } else {
            self.stage_cycles_per_guest_frame(stage_id)
        }
    }

    fn paced_visual_budget_status(self) -> &'static str {
        let render_cycles = self.render_cycles_per_visual_frame();
        let interval = self.visual_interval_vblanks();
        if render_cycles <= 0.0 || interval <= 0.0 {
            "?"
        } else if render_cycles <= NTSC_CPU_CYCLES_PER_VBLANK * interval {
            "pass"
        } else {
            "fail"
        }
    }
}

impl EguiRenderProfile {
    fn accumulate(&mut self, other: Self) {
        self.input_ms += other.input_ms;
        self.ui_ms += other.ui_ms;
        self.platform_output_ms += other.platform_output_ms;
        self.tessellate_ms += other.tessellate_ms;
        self.surface_ms += other.surface_ms;
        self.texture_update_ms += other.texture_update_ms;
        self.buffer_update_ms += other.buffer_update_ms;
        self.paint_ms += other.paint_ms;
        self.submit_present_ms += other.submit_present_ms;
        self.total_ms += other.total_ms;
    }

    fn divide(&mut self, n: f32) {
        self.input_ms /= n;
        self.ui_ms /= n;
        self.platform_output_ms /= n;
        self.tessellate_ms /= n;
        self.surface_ms /= n;
        self.texture_update_ms /= n;
        self.buffer_update_ms /= n;
        self.paint_ms /= n;
        self.submit_present_ms /= n;
        self.total_ms /= n;
    }
}

/// One frontend redraw sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameProfileSample {
    /// Monotonic frontend redraw sample id, assigned by [`FrameProfiler::record`].
    pub sample_serial: u32,
    /// Host delta between redraw callbacks.
    pub host_dt_ms: f32,
    /// Full RedrawRequested handler wall time.
    pub total_ms: f32,
    /// Menu/gamepad/input/menu action work.
    pub input_ms: f32,
    /// Guest CPU/bus execution.
    pub emu_ms: f32,
    /// SPU sample generation + host-audio queue push.
    pub audio_ms: f32,
    /// GP0 command-log drain.
    pub cmd_log_ms: f32,
    /// Optional compute-rasterizer shadow replay.
    pub compute_ms: f32,
    /// CPU VRAM -> egui VRAM texture upload.
    pub vram_upload_ms: f32,
    /// 24bpp display texture upload.
    pub display_upload_ms: f32,
    /// Hardware-renderer scale decision/reallocation.
    pub hw_scale_ms: f32,
    /// CPU VRAM snapshot clone for the hardware renderer.
    pub hw_vram_clone_ms: f32,
    /// Hardware-renderer command translation + wgpu submit.
    pub hw_render_ms: f32,
    /// egui/wgpu UI render breakdown.
    pub egui: EguiRenderProfile,
    /// Number of emulated frames stepped during this redraw.
    pub frames_run: f32,
    /// Retired CPU ticks during this redraw.
    pub cpu_ticks: f32,
    /// Emulated bus cycles during this redraw.
    pub bus_cycles: f32,
    /// Total PS1 video-frame cycle budget targeted by the stepped frames.
    pub psx_budget_cycles: f32,
    /// Number of VBlank IRQ raises observed while stepping guest frames.
    pub psx_vblanks: f32,
    /// Number of stepped VBlanks that emitted at least one draw packet.
    pub psx_draw_vblanks: f32,
    /// Count of guest frames stopped by the frontend safety step cap.
    pub psx_step_cap_misses: f32,
    /// Recognised GTE function commands executed during this redraw.
    pub gte_ops: f32,
    /// Estimated internal GTE command cycles during this redraw.
    pub gte_estimated_cycles: f32,
    /// Captured GP0 packets replayed by render sidecars.
    pub gpu_cmds: f32,
    /// Total FIFO words inside the captured GP0 packets.
    pub gpu_words: f32,
    /// Captured polygon/line/rectangle packets.
    pub gpu_draw_cmds: f32,
    /// Captured VRAM copy/upload packets.
    pub gpu_image_cmds: f32,
    /// Current hardware-renderer internal scale.
    pub hw_scale: f32,
    /// Out-of-band guest runtime telemetry.
    pub guest: GuestRuntimeProfile,
}

impl FrameProfileSample {
    /// Rows shown in the profiler panel.
    pub fn stage_rows(self) -> [(&'static str, f32); 18] {
        [
            ("input/menu", self.input_ms),
            ("guest emu", self.emu_ms),
            ("spu/audio", self.audio_ms),
            ("cmd log", self.cmd_log_ms),
            ("compute", self.compute_ms),
            ("vram upload", self.vram_upload_ms),
            ("24bpp upload", self.display_upload_ms),
            ("hw scale", self.hw_scale_ms),
            ("vram clone", self.hw_vram_clone_ms),
            ("hw render", self.hw_render_ms),
            ("ui input", self.egui.input_ms),
            ("ui build", self.egui.ui_ms),
            ("ui tess", self.egui.tessellate_ms),
            ("ui textures", self.egui.texture_update_ms),
            ("ui buffers", self.egui.buffer_update_ms),
            ("ui paint", self.egui.paint_ms),
            ("present", self.egui.submit_present_ms),
            ("total", self.total_ms),
        ]
    }

    fn accumulate(&mut self, other: Self) {
        self.host_dt_ms += other.host_dt_ms;
        self.total_ms += other.total_ms;
        self.input_ms += other.input_ms;
        self.emu_ms += other.emu_ms;
        self.audio_ms += other.audio_ms;
        self.cmd_log_ms += other.cmd_log_ms;
        self.compute_ms += other.compute_ms;
        self.vram_upload_ms += other.vram_upload_ms;
        self.display_upload_ms += other.display_upload_ms;
        self.hw_scale_ms += other.hw_scale_ms;
        self.hw_vram_clone_ms += other.hw_vram_clone_ms;
        self.hw_render_ms += other.hw_render_ms;
        self.egui.accumulate(other.egui);
        self.frames_run += other.frames_run;
        self.cpu_ticks += other.cpu_ticks;
        self.bus_cycles += other.bus_cycles;
        self.psx_budget_cycles += other.psx_budget_cycles;
        self.psx_vblanks += other.psx_vblanks;
        self.psx_draw_vblanks += other.psx_draw_vblanks;
        self.psx_step_cap_misses += other.psx_step_cap_misses;
        self.gte_ops += other.gte_ops;
        self.gte_estimated_cycles += other.gte_estimated_cycles;
        self.gpu_cmds += other.gpu_cmds;
        self.gpu_words += other.gpu_words;
        self.gpu_draw_cmds += other.gpu_draw_cmds;
        self.gpu_image_cmds += other.gpu_image_cmds;
        self.hw_scale += other.hw_scale;
        self.guest.accumulate(other.guest);
    }

    fn divide(&mut self, n: f32) {
        self.host_dt_ms /= n;
        self.total_ms /= n;
        self.input_ms /= n;
        self.emu_ms /= n;
        self.audio_ms /= n;
        self.cmd_log_ms /= n;
        self.compute_ms /= n;
        self.vram_upload_ms /= n;
        self.display_upload_ms /= n;
        self.hw_scale_ms /= n;
        self.hw_vram_clone_ms /= n;
        self.hw_render_ms /= n;
        self.egui.divide(n);
        self.frames_run /= n;
        self.cpu_ticks /= n;
        self.bus_cycles /= n;
        self.psx_budget_cycles /= n;
        self.psx_vblanks /= n;
        self.psx_draw_vblanks /= n;
        self.psx_step_cap_misses /= n;
        self.gte_ops /= n;
        self.gte_estimated_cycles /= n;
        self.gpu_cmds /= n;
        self.gpu_words /= n;
        self.gpu_draw_cmds /= n;
        self.gpu_image_cmds /= n;
        self.hw_scale /= n;
        self.guest.divide(n);
    }

    /// Add guest-runtime telemetry to this sample.
    pub fn add_guest_profile(&mut self, guest: GuestRuntimeProfile) {
        self.guest.accumulate(guest);
    }

    pub fn host_fps(self) -> f32 {
        fps_from_ms(self.host_dt_ms)
    }

    pub fn psx_budget_percent(self) -> f32 {
        if self.psx_budget_cycles > 0.0 {
            (self.bus_cycles / self.psx_budget_cycles) * 100.0
        } else {
            0.0
        }
    }

    pub fn emulated_vblank_hz(self) -> f32 {
        if self.host_dt_ms > 0.0 {
            self.psx_vblanks * 1000.0 / self.host_dt_ms
        } else {
            0.0
        }
    }

    pub fn guest_refresh_hz(self) -> f32 {
        let budget = self.budget_cycles_per_guest_frame();
        if budget > 0.0 {
            PSX_MASTER_CLOCK_HZ / budget
        } else {
            0.0
        }
    }

    pub fn psx_draw_hz(self) -> f32 {
        if self.psx_vblanks > 0.0 {
            self.guest_refresh_hz() * (self.psx_draw_vblanks / self.psx_vblanks)
        } else {
            0.0
        }
    }

    pub fn guest_visual_frame_hz(self) -> Option<f32> {
        self.guest
            .visual_frame_interval_cycles()
            .map(|cycles| PSX_MASTER_CLOCK_HZ / cycles)
    }

    pub fn guest_visual_frame_count(self) -> f32 {
        self.guest
            .counter_total(emulator_core::telemetry::counter::VISUAL_FRAMES as usize)
    }

    pub fn guest_visual_interval_vblanks(self) -> Option<f32> {
        if !self.guest.has_pacing_data() {
            return None;
        }
        let interval = self.guest.visual_interval_vblanks();
        if interval > 0.0 {
            Some(interval)
        } else {
            None
        }
    }

    pub fn guest_visual_deadline_misses(self) -> f32 {
        self.guest
            .counter_total(emulator_core::telemetry::counter::VISUAL_DEADLINE_MISSES as usize)
    }

    pub fn guest_visual_max_lateness_vblanks(self) -> f32 {
        self.guest.counter_max_value(
            emulator_core::telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS as usize,
        )
    }

    fn bus_cycles_per_guest_frame(self) -> f32 {
        per_guest_frame(self.bus_cycles, self.frames_run)
    }

    fn budget_cycles_per_guest_frame(self) -> f32 {
        per_guest_frame(self.psx_budget_cycles, self.frames_run)
    }

    fn cpu_ticks_per_guest_frame(self) -> f32 {
        per_guest_frame(self.cpu_ticks, self.frames_run)
    }

    fn gte_ops_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gte_ops, self.frames_run)
    }

    fn gte_cycles_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gte_estimated_cycles, self.frames_run)
    }

    fn gpu_cmds_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gpu_cmds, self.frames_run)
    }

    fn gpu_words_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gpu_words, self.frames_run)
    }

    fn gpu_draw_cmds_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gpu_draw_cmds, self.frames_run)
    }

    fn gpu_image_cmds_per_guest_frame(self) -> f32 {
        per_guest_frame(self.gpu_image_cmds, self.frames_run)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogMode {
    Off,
    Summary,
    EveryFrame,
}

/// Rolling profiler state.
pub struct FrameProfiler {
    samples: VecDeque<FrameProfileSample>,
    next_sample_serial: u32,
    log_mode: LogMode,
    log_accum_ms: f32,
    guest_stage_starts: [Option<u64>; STAGE_COUNT],
    guest_task_starts: [Option<u64>; TASK_COUNT],
    guest_last_visual_frame_cycle: Option<u64>,
    capture: Option<FrameProfilerCapture>,
}

/// Bounded aggregate for a full input recording or replay.
///
/// The rolling profiler intentionally retains only 500 host frames. This
/// aggregate keeps totals and high-water marks for an arbitrarily long route
/// without retaining one very wide telemetry sample per redraw.
#[derive(Clone, Copy, Debug, Default)]
struct FrameProfilerCapture {
    sample_count: u64,
    totals: FrameProfileSample,
    max_host_dt_ms: f32,
    max_total_ms: f32,
    max_emu_ms: f32,
    max_vram_upload_ms: f32,
    max_hw_render_ms: f32,
    observed_tri_capacity: u32,
}

impl FrameProfilerCapture {
    fn record(&mut self, sample: FrameProfileSample) {
        self.sample_count = self.sample_count.saturating_add(1);
        self.max_host_dt_ms = self.max_host_dt_ms.max(sample.host_dt_ms);
        self.max_total_ms = self.max_total_ms.max(sample.total_ms);
        self.max_emu_ms = self.max_emu_ms.max(sample.emu_ms);
        self.max_vram_upload_ms = self.max_vram_upload_ms.max(sample.vram_upload_ms);
        self.max_hw_render_ms = self.max_hw_render_ms.max(sample.hw_render_ms);

        let used = sample
            .guest
            .counter_latest_value(counter::TRI_PRIMITIVES as usize);
        let free = sample
            .guest
            .counter_latest_value(counter::TRI_PRIMITIVE_REMAINING as usize);
        if sample
            .guest
            .has_counter_observation(counter::TRI_PRIMITIVES as usize)
            && sample
                .guest
                .has_counter_observation(counter::TRI_PRIMITIVE_REMAINING as usize)
        {
            self.observed_tri_capacity = self.observed_tri_capacity.max(used.saturating_add(free));
        }
        self.totals.accumulate(sample);
    }
}

/// Completed whole-run profiler capture.
#[derive(Clone, Copy, Debug)]
pub struct FrameProfilerCaptureReport {
    capture: FrameProfilerCapture,
}

impl FrameProfilerCaptureReport {
    /// Number of frontend samples folded into this report.
    pub fn sample_count(self) -> u64 {
        self.capture.sample_count
    }

    /// Export one compact row per host metric, guest stage/task and observed
    /// guest counter. This stays small even for hour-long input tapes.
    pub fn csv(self) -> String {
        let capture = self.capture;
        let samples = capture.sample_count.max(1) as f32;
        let guest = capture.totals.guest;
        let visual_frames = guest
            .counter_total(counter::VISUAL_FRAMES as usize)
            .max(1.0);
        let stream_capacity = guest.counter_max_value(counter::ROOM_STREAM_SLOT_LIMIT as usize);
        let mut out =
            String::from("kind,id,name,total,hits,average,max,latest,capacity,peak_percent\n");

        push_capture_row(
            &mut out,
            "session",
            0,
            "host samples",
            capture.sample_count as f32,
            capture.sample_count as f32,
            1.0,
            capture.sample_count as f32,
            capture.sample_count as u32,
            None,
        );
        push_capture_row(
            &mut out,
            "host",
            0,
            "host frame dt ms",
            capture.totals.host_dt_ms,
            capture.sample_count as f32,
            capture.totals.host_dt_ms / samples,
            capture.max_host_dt_ms,
            0,
            None,
        );
        push_capture_row(
            &mut out,
            "host",
            1,
            "total frame work ms",
            capture.totals.total_ms,
            capture.sample_count as f32,
            capture.totals.total_ms / samples,
            capture.max_total_ms,
            0,
            None,
        );
        push_capture_row(
            &mut out,
            "host",
            2,
            "emulation ms",
            capture.totals.emu_ms,
            capture.sample_count as f32,
            capture.totals.emu_ms / samples,
            capture.max_emu_ms,
            0,
            None,
        );
        push_capture_row(
            &mut out,
            "host",
            3,
            "vram upload ms",
            capture.totals.vram_upload_ms,
            capture.sample_count as f32,
            capture.totals.vram_upload_ms / samples,
            capture.max_vram_upload_ms,
            0,
            None,
        );
        push_capture_row(
            &mut out,
            "host",
            4,
            "hardware render ms",
            capture.totals.hw_render_ms,
            capture.sample_count as f32,
            capture.totals.hw_render_ms / samples,
            capture.max_hw_render_ms,
            0,
            None,
        );

        for id in 1..STAGE_COUNT {
            let total = guest.stage_cycles[id];
            let hits = guest.stage_hits[id];
            if total <= 0.0 && hits <= 0.0 {
                continue;
            }
            push_capture_row(
                &mut out,
                "stage",
                id,
                stage_name(id as u16),
                total,
                hits,
                if hits > 0.0 { total / hits } else { 0.0 },
                guest.stage_max_cycles[id],
                0,
                None,
            );
        }
        for id in 1..TASK_COUNT {
            let total = guest.task_cycles[id];
            let hits = guest.task_hits[id];
            if total <= 0.0 && hits <= 0.0 {
                continue;
            }
            push_capture_row(
                &mut out,
                "task",
                id,
                task_name(id as u16),
                total,
                hits,
                if hits > 0.0 { total / hits } else { 0.0 },
                guest.task_max_cycles[id],
                0,
                None,
            );
        }
        for id in 1..COUNTER_COUNT {
            let total = guest.counters[id];
            let max = guest.counter_max_values[id];
            let latest = guest.counter_latest_values[id];
            if total <= 0.0 && max <= 0.0 && latest == 0 {
                continue;
            }
            let capacity = match id as u16 {
                counter::TRI_PRIMITIVES | counter::WORLD_COMMANDS
                    if capture.observed_tri_capacity > 0 =>
                {
                    Some(capture.observed_tri_capacity as f32)
                }
                counter::ROOM_STREAM_RESIDENT_SLOTS if stream_capacity > 0.0 => {
                    Some(stream_capacity)
                }
                _ => None,
            };
            push_capture_row(
                &mut out,
                "counter",
                id,
                counter_name(id as u16),
                total,
                visual_frames,
                total / visual_frames,
                max,
                latest,
                capacity,
            );
        }
        out
    }
}

fn push_capture_row(
    out: &mut String,
    kind: &str,
    id: usize,
    name: &str,
    total: f32,
    hits: f32,
    average: f32,
    max: f32,
    latest: u32,
    capacity: Option<f32>,
) {
    let (capacity_text, percent_text) = capacity.map_or_else(
        || (String::new(), String::new()),
        |capacity| {
            let percent = if capacity > 0.0 {
                max * 100.0 / capacity
            } else {
                0.0
            };
            (format!("{capacity:.0}"), format!("{percent:.2}"))
        },
    );
    let _ = writeln!(
        out,
        "{kind},{id},{name},{total:.0},{hits:.0},{average:.3},{max:.0},{latest},{capacity_text},{percent_text}"
    );
}

impl Default for FrameProfiler {
    fn default() -> Self {
        let log_mode = match std::env::var("PSOXIDE_PROFILE") {
            Ok(value) if matches!(value.as_str(), "trace" | "frame" | "all") => LogMode::EveryFrame,
            Ok(value) if value != "0" && !value.eq_ignore_ascii_case("off") => LogMode::Summary,
            _ => LogMode::Off,
        };
        Self {
            samples: VecDeque::with_capacity(HISTORY_CAP),
            next_sample_serial: 0,
            log_mode,
            log_accum_ms: 0.0,
            guest_stage_starts: [None; STAGE_COUNT],
            guest_task_starts: [None; TASK_COUNT],
            guest_last_visual_frame_cycle: None,
            capture: None,
        }
    }
}

impl FrameProfiler {
    /// Add one sample. Returns a log line when `PSOXIDE_PROFILE` asks for stderr output.
    pub fn record(&mut self, mut sample: FrameProfileSample) -> Option<String> {
        self.next_sample_serial = self.next_sample_serial.wrapping_add(1);
        sample.sample_serial = self.next_sample_serial;
        if let Some(capture) = self.capture.as_mut() {
            capture.record(sample);
        }
        if self.samples.len() >= HISTORY_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);

        match self.log_mode {
            LogMode::Off => None,
            LogMode::EveryFrame => Some(format_log_line("frame", sample)),
            LogMode::Summary => {
                self.log_accum_ms += sample.host_dt_ms.max(sample.total_ms).max(0.0);
                if self.log_accum_ms >= LOG_INTERVAL_MS {
                    self.log_accum_ms = 0.0;
                    self.average().map(|avg| format_log_line("avg", avg))
                } else {
                    None
                }
            }
        }
    }

    /// Most recent sample.
    pub fn latest(&self) -> Option<FrameProfileSample> {
        self.samples.back().copied()
    }

    /// Most recent sample that contains one of the requested guest counters.
    pub fn latest_with_guest_counters(&self, counter_ids: &[u16]) -> Option<FrameProfileSample> {
        self.samples.iter().rev().copied().find(|sample| {
            counter_ids
                .iter()
                .any(|&id| sample.guest.has_counter_observation(id as usize))
        })
    }

    /// Most recent sample that contains every requested guest counter.
    pub fn latest_with_all_guest_counters(
        &self,
        counter_ids: &[u16],
    ) -> Option<FrameProfileSample> {
        self.samples.iter().rev().copied().find(|sample| {
            counter_ids
                .iter()
                .all(|&id| sample.guest.has_counter_observation(id as usize))
        })
    }

    /// Average across the rolling window.
    pub fn average(&self) -> Option<FrameProfileSample> {
        let n = self.samples.len();
        if n == 0 {
            return None;
        }
        let mut avg = FrameProfileSample::default();
        for sample in &self.samples {
            avg.accumulate(*sample);
        }
        avg.divide(n as f32);
        Some(avg)
    }

    /// Short moving average for live HUD numbers.
    pub fn live_average(&self) -> Option<FrameProfileSample> {
        self.average_recent_ms(LIVE_AVERAGE_WINDOW_MS)
    }

    /// Average across the newest samples that cover roughly `window_ms`.
    pub fn average_recent_ms(&self, window_ms: f32) -> Option<FrameProfileSample> {
        let mut avg = FrameProfileSample::default();
        let mut n = 0usize;
        let mut elapsed_ms = 0.0f32;
        let target_ms = window_ms.max(0.0);
        for sample in self.samples.iter().rev() {
            avg.accumulate(*sample);
            n += 1;
            elapsed_ms += sample.host_dt_ms.max(sample.total_ms).max(0.0);
            if elapsed_ms >= target_ms {
                break;
            }
        }
        if n == 0 {
            return None;
        }
        avg.divide(n as f32);
        Some(avg)
    }

    /// Clear the rolling window.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.log_accum_ms = 0.0;
        self.guest_stage_starts = [None; STAGE_COUNT];
        self.guest_task_starts = [None; TASK_COUNT];
        self.guest_last_visual_frame_cycle = None;
    }

    /// Start a fresh bounded whole-run capture.
    pub fn begin_capture(&mut self) {
        self.capture = Some(FrameProfilerCapture::default());
    }

    /// True while samples are being folded into a whole-run capture.
    pub fn capture_active(&self) -> bool {
        self.capture.is_some()
    }

    /// Finish the active whole-run capture.
    pub fn finish_capture(&mut self) -> Option<FrameProfilerCaptureReport> {
        self.capture
            .take()
            .map(|capture| FrameProfilerCaptureReport { capture })
    }

    /// Number of samples currently retained in the rolling history.
    pub fn history_len(&self) -> usize {
        self.samples.len()
    }

    /// Export the rolling history as a wide CSV for offline diagnosis.
    pub fn history_csv(&self) -> String {
        let mut out = String::new();
        push_history_csv_header(&mut out);
        for (index, sample) in self.samples.iter().copied().enumerate() {
            push_history_csv_sample(&mut out, index, sample);
        }
        out
    }

    /// Fold raw guest events into one frontend-frame sample, preserving
    /// open stage spans across samples when the guest misses a VBlank budget.
    pub fn consume_guest_events(&mut self, events: &[GuestTelemetryEvent]) -> GuestRuntimeProfile {
        let mut out = GuestRuntimeProfile::default();
        for event in events {
            match event.kind {
                emulator_core::telemetry::GuestTelemetryKind::FrameBegin => {
                    out.frames += 1.0;
                }
                emulator_core::telemetry::GuestTelemetryKind::StageBegin => {
                    if let Some(slot) = self.guest_stage_starts.get_mut(event.id as usize) {
                        *slot = Some(event.cycles);
                    }
                }
                emulator_core::telemetry::GuestTelemetryKind::StageEnd => {
                    let Some(slot) = self.guest_stage_starts.get_mut(event.id as usize) else {
                        continue;
                    };
                    let Some(start) = slot.take() else {
                        continue;
                    };
                    let idx = event.id as usize;
                    let cycles = event.cycles.saturating_sub(start) as f32;
                    out.stage_cycles[idx] += cycles;
                    out.stage_hits[idx] += 1.0;
                    out.stage_max_cycles[idx] = out.stage_max_cycles[idx].max(cycles);
                }
                emulator_core::telemetry::GuestTelemetryKind::TaskBegin => {
                    if let Some(slot) = self.guest_task_starts.get_mut(event.id as usize) {
                        *slot = Some(event.cycles);
                    }
                }
                emulator_core::telemetry::GuestTelemetryKind::TaskEnd => {
                    let Some(slot) = self.guest_task_starts.get_mut(event.id as usize) else {
                        continue;
                    };
                    let Some(start) = slot.take() else {
                        continue;
                    };
                    let idx = event.id as usize;
                    let cycles = event.cycles.saturating_sub(start) as f32;
                    out.task_cycles[idx] += cycles;
                    out.task_hits[idx] += 1.0;
                    out.task_max_cycles[idx] = out.task_max_cycles[idx].max(cycles);
                }
                emulator_core::telemetry::GuestTelemetryKind::Counter => {
                    if event.id == counter::VISUAL_FRAMES && event.value > 0 {
                        if let Some(cycles) = self
                            .guest_last_visual_frame_cycle
                            .and_then(|previous| event.cycles.checked_sub(previous))
                            .filter(|&cycles| cycles > 0)
                        {
                            let cycles_per_frame = cycles as f32 / event.value as f32;
                            out.visual_frame_interval_cycle_total += cycles as f32;
                            out.visual_frame_interval_hits += event.value as f32;
                            for _ in 0..event.value.min(VISUAL_FRAME_INTERVAL_CAP as u32) {
                                out.append_visual_frame_interval(cycles_per_frame);
                            }
                        }
                        self.guest_last_visual_frame_cycle = Some(event.cycles);
                    }
                    if let Some(counter) = out.counters.get_mut(event.id as usize) {
                        *counter += event.value as f32;
                    }
                    if let Some(max_value) = out.counter_max_values.get_mut(event.id as usize) {
                        *max_value = (*max_value).max(event.value as f32);
                    }
                    if let Some(latest_value) = out.counter_latest_values.get_mut(event.id as usize)
                    {
                        *latest_value = event.value;
                    }
                }
                emulator_core::telemetry::GuestTelemetryKind::Unknown(_) => {}
            }
        }
        out
    }
}

/// Paint profiler contents inside an existing container.
pub fn draw_contents(ui: &mut egui::Ui, profiler: &mut FrameProfiler) {
    let Some(avg) = profiler.live_average().or_else(|| profiler.average()) else {
        ui.monospace("(no frame samples yet)");
        return;
    };
    let latest = profiler.latest().unwrap_or(avg);

    ui.horizontal_wrapped(|ui| {
        metric(ui, "EMU Hz", format!("{:.1}", avg.emulated_vblank_hz()));
        if let Some(visual_hz) = avg.guest_visual_frame_hz() {
            metric(ui, "VIS Hz", format!("{visual_hz:.1}"));
        }
        metric(ui, "DRAW Hz", format!("{:.1}", avg.psx_draw_hz()));
        metric(ui, "CAP", format!("{:.0}", avg.psx_step_cap_misses));
    });
    ui.horizontal_wrapped(|ui| {
        metric(ui, "STEP", format!("{:.0}%", avg.psx_budget_percent()));
        metric(ui, "VBL/R", format!("{:.1}", avg.psx_vblanks));
        metric(
            ui,
            "CYC/F",
            format!("{:.0}", avg.bus_cycles_per_guest_frame()),
        );
        metric(
            ui,
            "BUD/F",
            format!("{:.0}", avg.budget_cycles_per_guest_frame()),
        );
        metric(
            ui,
            "INS/F",
            format!("{:.0}", avg.cpu_ticks_per_guest_frame()),
        );
    });
    ui.horizontal_wrapped(|ui| {
        metric(ui, "GPU/V", format!("{:.2}", avg.psx_draw_vblanks));
        metric(
            ui,
            "CMD/F",
            format!("{:.0}", avg.gpu_cmds_per_guest_frame()),
        );
        metric(
            ui,
            "DRAW/F",
            format!("{:.0}", avg.gpu_draw_cmds_per_guest_frame()),
        );
        metric(
            ui,
            "IMG/F",
            format!("{:.0}", avg.gpu_image_cmds_per_guest_frame()),
        );
        metric(ui, "GTE/F", format!("{:.0}", avg.gte_ops_per_guest_frame()));
        metric(
            ui,
            "GTEC/F",
            format!("{:.0}", avg.gte_cycles_per_guest_frame()),
        );
    });
    ui.horizontal_wrapped(|ui| {
        metric(ui, "HOST FPS", format!("{:.1}", avg.host_fps()));
        metric(ui, "HOST AVG", format!("{:.2} ms", avg.total_ms));
        metric(ui, "HOST LAST", format!("{:.2} ms", latest.total_ms));
        metric(ui, "UI", format!("{:.2} ms", avg.egui.total_ms));
        metric(ui, "HW", format!("{:.2} ms", avg.hw_render_ms));
        metric(ui, "SCALE", format!("{:.0}x", latest.hw_scale.max(1.0)));
    });

    ui.add_space(4.0);
    section_label(ui, "Host Frame");
    draw_history(ui, profiler);
    ui.add_space(6.0);

    let max_ms = avg.total_ms.max(BUDGET_60_MS).max(1.0);
    for (label, ms) in avg.stage_rows() {
        // The compute shadow rasterizer is opt-in (--gpu-compute); hide its
        // permanently-zero row for everyone else.
        if label == "compute" && ms <= 0.0 {
            continue;
        }
        let color = if label == "total" {
            theme::ACCENT
        } else {
            theme::TEXT
        };
        bar_row(
            ui,
            label,
            color,
            ms / max_ms,
            color_for_ms(ms),
            &[(format!("{ms:6.2}"), VAL_MS_W, false)],
        );
    }

    if avg.guest.has_data() {
        ui.add_space(8.0);
        section_label(ui, "Guest Runtime");
        ui.horizontal_wrapped(|ui| {
            metric(ui, "GFR/R", format!("{:.1}", avg.guest.frames));
            metric(
                ui,
                "UPD/F",
                format!(
                    "{:.0}",
                    avg.guest.stage_cycles_per_guest_frame(
                        emulator_core::telemetry::stage::UPDATE as usize
                    )
                ),
            );
            metric(
                ui,
                "REN",
                format!(
                    "{:.0}",
                    avg.guest
                        .stage_cycles_per_hit(emulator_core::telemetry::stage::RENDER as usize)
                ),
            );
            metric(
                ui,
                "MOD",
                format!(
                    "{:.0}",
                    avg.guest.stage_cycles_per_hit(
                        emulator_core::telemetry::stage::MODEL_INSTANCES as usize
                    )
                ),
            );
        });
        if avg.guest.has_pacing_data() {
            ui.horizontal_wrapped(|ui| {
                metric(
                    ui,
                    "SIM",
                    format!(
                        "{:.0}",
                        avg.guest
                            .counter_total(emulator_core::telemetry::counter::SIM_TICKS as usize)
                    ),
                );
                metric(
                    ui,
                    "VIS",
                    format!(
                        "{:.0}",
                        avg.guest.counter_total(
                            emulator_core::telemetry::counter::VISUAL_FRAMES as usize
                        )
                    ),
                );
                metric(
                    ui,
                    "INT",
                    format!("{:.2}", avg.guest.visual_interval_vblanks()),
                );
                metric(
                    ui,
                    "MISS",
                    format!(
                        "{:.0}",
                        avg.guest.counter_total(
                            emulator_core::telemetry::counter::VISUAL_DEADLINE_MISSES as usize
                        )
                    ),
                );
                metric(
                    ui,
                    "LATE",
                    format!(
                        "{:.0}",
                        avg.guest.counter_max_value(
                            emulator_core::telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS as usize
                        )
                    ),
                );
                metric(
                    ui,
                    "REN/V",
                    format!("{:.0}", avg.guest.render_cycles_per_visual_frame()),
                );
                metric(
                    ui,
                    "VBUD",
                    avg.guest.paced_visual_budget_status().to_string(),
                );
            });
        }
        draw_guest_scheduler_tasks(ui, avg.guest);
        draw_guest_render_breakdown(ui, avg.guest);
        draw_guest_runtime(ui, avg.guest);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.small_button("Log Snapshot").clicked() {
            eprintln!("{}", format_log_line("ui", latest));
        }
        if ui.small_button("Clear").clicked() {
            profiler.clear();
        }
    });
}

fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    // One atomic chip: the nested horizontal keeps label+value together,
    // so the wrapped parent row wraps BETWEEN chips, never inside one.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(
            RichText::new(label)
                .color(theme::TEXT_DIM)
                .size(theme::FONT_SIZE_SMALL),
        );
        ui.label(
            RichText::new(value)
                .color(theme::TEXT)
                .monospace()
                .size(theme::FONT_SIZE_SMALL),
        );
    });
}

/// Small accent caption used to group the profiler's tables.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .color(theme::ACCENT)
            .monospace()
            .size(theme::FONT_SIZE_SMALL),
    );
}

// ---- Responsive bar rows --------------------------------------------------
// Every bar table shares these fixed label/value column widths, so bars
// align ACROSS sections, and the bar itself absorbs whatever width the
// sidebar currently has: resizing stretches the bars, never the columns.
const ROW_H: f32 = 13.0;
const BAR_H: f32 = 9.0;
const ROW_LABEL_W: f32 = 108.0;
const BAR_MIN_W: f32 = 36.0;
const VAL_MS_W: f32 = 52.0;
const VAL_CYC_W: f32 = 78.0;
const VAL_PCT_W: f32 = 46.0;
const VAL_WORST_W: f32 = 88.0;

/// One `label | bar | values...` row. `values` are (text, cell width, dim).
fn bar_row(
    ui: &mut egui::Ui,
    label: &str,
    label_color: egui::Color32,
    frac: f32,
    fill: egui::Color32,
    values: &[(String, f32, bool)],
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            [ROW_LABEL_W, ROW_H],
            egui::Label::new(
                RichText::new(label)
                    .color(label_color)
                    .monospace()
                    .size(theme::FONT_SIZE_SMALL),
            )
            .truncate(),
        );
        let reserved: f32 = values.iter().map(|v| v.1 + 6.0).sum();
        let bar_w = (ui.available_width() - reserved).max(BAR_MIN_W);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, BAR_H), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, theme::WIDGET_BG);
        let fill_w = (rect.width() * frac.clamp(0.0, 1.0)).max(1.0);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height())),
            2.0,
            fill,
        );
        for (text, width, dim) in values {
            ui.add_sized(
                [*width, ROW_H],
                egui::Label::new(
                    RichText::new(text)
                        .color(if *dim { theme::TEXT_DIM } else { theme::TEXT })
                        .monospace()
                        .size(theme::FONT_SIZE_SMALL),
                ),
            );
        }
    });
}

fn draw_guest_scheduler_tasks(ui: &mut egui::Ui, guest: GuestRuntimeProfile) {
    let has_tasks = guest.task_cycles.iter().any(|&cycles| cycles > 0.0);
    if !has_tasks {
        return;
    }

    ui.add_space(4.0);
    section_label(ui, "Scheduler Tasks");

    let max_cycles = (0..TASK_COUNT)
        .map(|id| guest.task_cycles_per_hit(id))
        .fold(NTSC_CPU_CYCLES_PER_VBLANK / 4.0, f32::max)
        .max(1.0);

    for id in 0..TASK_COUNT {
        let cycles = guest.task_cycles_per_hit(id);
        if cycles <= 0.0 {
            continue;
        }
        bar_row(
            ui,
            task_name(id as u16),
            theme::TEXT,
            cycles / max_cycles,
            theme::ACCENT_HOVER,
            &[
                (format!("{cycles:7.0} cyc"), VAL_CYC_W, false),
                (
                    format!("worst {:.0}", guest.task_max_cycles[id]),
                    VAL_WORST_W,
                    true,
                ),
                (
                    format!("{:.3} ms", cycles / PSX_CYCLES_PER_MS),
                    VAL_MS_W,
                    true,
                ),
            ],
        );
    }
}

fn draw_guest_runtime(ui: &mut egui::Ui, guest: GuestRuntimeProfile) {
    ui.add_space(4.0);
    section_label(ui, "Guest Stages");
    let max_cycles = (1..STAGE_COUNT)
        .map(|id| guest.stage_cycles_per_hit(id))
        .fold(guest.cycle_budget_per_guest_frame() / 4.0, f32::max)
        .max(1.0);

    for id in 1..STAGE_COUNT {
        let cycles = guest.stage_cycles_per_hit(id);
        if cycles <= 0.0 {
            continue;
        }
        bar_row(
            ui,
            stage_name(id as u16),
            theme::TEXT,
            cycles / max_cycles,
            theme::ACCENT_HOVER,
            &[
                (format!("{cycles:7.0} cyc"), VAL_CYC_W, false),
                (
                    format!("{:.3} ms", cycles / PSX_CYCLES_PER_MS),
                    VAL_MS_W,
                    true,
                ),
            ],
        );
    }

    let has_counters = guest.counters.iter().any(|&value| value > 0.0);
    if !has_counters {
        return;
    }

    // 200+ telemetry counter ids exist; the full dump is reference
    // material, not at-a-glance data -- keep it folded by default.
    ui.add_space(4.0);
    egui::CollapsingHeader::new(
        RichText::new("All counters")
            .color(theme::TEXT_DIM)
            .monospace()
            .size(theme::FONT_SIZE_SMALL),
    )
    .default_open(false)
    .show(ui, |ui| {
        egui::Grid::new("guest-runtime-counter-grid")
            .num_columns(2)
            .spacing(egui::vec2(8.0, 3.0))
            .striped(false)
            .show(ui, |ui| {
                for id in 1..COUNTER_COUNT {
                    let value = guest.counter_per_guest_frame(id);
                    if value <= 0.0 {
                        continue;
                    }
                    counter_row(ui, counter_name(id as u16), value);
                }
            });
    });
}

fn counter_row(ui: &mut egui::Ui, label: &str, value: f32) {
    ui.label(
        RichText::new(label)
            .color(theme::TEXT_DIM)
            .monospace()
            .size(theme::FONT_SIZE_SMALL),
    );
    ui.label(
        RichText::new(format!("{value:.0}"))
            .color(theme::TEXT)
            .monospace()
            .size(theme::FONT_SIZE_SMALL),
    );
    ui.end_row();
}

fn draw_guest_render_breakdown(ui: &mut egui::Ui, guest: GuestRuntimeProfile) {
    let render_cycles = guest.stage_cycles[stage::RENDER as usize].max(1.0);
    if render_cycles <= 1.0 {
        return;
    }

    ui.add_space(4.0);
    section_label(ui, "Render %");

    let mut accounted = 0.0;
    for &(stage_id, label) in GUEST_RENDER_BREAKDOWN_STAGES {
        let cycles = guest.stage_cycles[stage_id as usize];
        if cycles <= 0.0 {
            continue;
        }
        accounted += cycles;
        guest_render_percent_row(ui, label, cycles, render_cycles);
    }
    let other = (render_cycles - accounted).max(0.0);
    if other > render_cycles * 0.005 {
        guest_render_percent_row(ui, "other", other, render_cycles);
    }
}

fn guest_render_percent_row(ui: &mut egui::Ui, label: &str, cycles: f32, render_cycles: f32) {
    let percent = cycles * 100.0 / render_cycles.max(1.0);
    bar_row(
        ui,
        label,
        theme::TEXT,
        percent / 100.0,
        theme::ACCENT_HOVER,
        &[
            (format!("{percent:5.1}%"), VAL_PCT_W, false),
            (
                format!("{:.3} ms", cycles / PSX_CYCLES_PER_MS),
                VAL_MS_W,
                true,
            ),
        ],
    );
}

fn draw_history(ui: &mut egui::Ui, profiler: &FrameProfiler) {
    // Follow the panel width down to a sane floor instead of overflowing
    // the sidebar at its minimum width.
    let desired = egui::vec2(ui.available_width().max(80.0), 72.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, theme::CONTENT_BG);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, theme::SEPARATOR),
        egui::StrokeKind::Inside,
    );

    let max_ms = profiler
        .samples
        .iter()
        .map(|s| s.total_ms)
        .fold(BUDGET_30_MS, f32::max)
        .max(1.0);
    draw_budget_line(ui, rect, max_ms, BUDGET_60_MS, "16.7");
    draw_budget_line(ui, rect, max_ms, BUDGET_30_MS, "33.3");

    let count = profiler.samples.len();
    if count < 2 {
        return;
    }
    let left = rect.left() + 6.0;
    let right = rect.right() - 6.0;
    let top = rect.top() + 6.0;
    let bottom = rect.bottom() - 8.0;
    let span_x = (right - left).max(1.0);
    let span_y = (bottom - top).max(1.0);
    let mut points = Vec::with_capacity(count);
    for (i, sample) in profiler.samples.iter().enumerate() {
        let x = left + span_x * (i as f32 / (count - 1) as f32);
        let y = bottom - span_y * (sample.total_ms / max_ms).clamp(0.0, 1.0);
        points.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, theme::ACCENT_HOVER),
    ));
}

fn draw_budget_line(ui: &egui::Ui, rect: egui::Rect, max_ms: f32, budget: f32, label: &str) {
    if budget > max_ms {
        return;
    }
    let top = rect.top() + 6.0;
    let bottom = rect.bottom() - 8.0;
    let y = bottom - (bottom - top) * (budget / max_ms).clamp(0.0, 1.0);
    let painter = ui.painter();
    painter.line_segment(
        [
            egui::pos2(rect.left() + 4.0, y),
            egui::pos2(rect.right() - 4.0, y),
        ],
        egui::Stroke::new(1.0, theme::SEPARATOR),
    );
    painter.text(
        egui::pos2(rect.right() - 32.0, y - 10.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::monospace(theme::FONT_SIZE_SMALL),
        theme::TEXT_DIM,
    );
}

fn push_history_csv_header(out: &mut String) {
    out.push_str(
        "index,sample_serial,host_dt_ms,total_ms,input_ms,emu_ms,audio_ms,cmd_log_ms,\
         compute_ms,vram_upload_ms,display_upload_ms,hw_scale_ms,hw_vram_clone_ms,\
         hw_render_ms,egui_input_ms,egui_ui_ms,egui_platform_output_ms,\
         egui_tessellate_ms,egui_texture_update_ms,egui_buffer_update_ms,\
         egui_paint_ms,egui_submit_present_ms,egui_total_ms,frames_run,cpu_ticks,\
         bus_cycles,psx_budget_cycles,psx_vblanks,psx_draw_vblanks,\
         psx_step_cap_misses,gte_ops,gte_estimated_cycles,gpu_cmds,gpu_words,\
         gpu_draw_cmds,gpu_image_cmds,hw_scale,host_fps,emulated_vblank_hz,\
         psx_draw_hz,guest_visual_hz,guest_visual_frames,\
         guest_visual_interval_vblanks,guest_visual_deadline_misses,\
         guest_visual_max_lateness_vblanks,guest_render_cycles_per_visual,\
         guest_visual_budget_status",
    );
    for id in 0..STAGE_COUNT {
        push_csv_metric_header(out, "stage", id, stage_name(id as u16), "cycles");
        push_csv_metric_header(out, "stage", id, stage_name(id as u16), "hits");
        push_csv_metric_header(out, "stage", id, stage_name(id as u16), "max");
    }
    for id in 0..TASK_COUNT {
        push_csv_metric_header(out, "task", id, task_name(id as u16), "cycles");
        push_csv_metric_header(out, "task", id, task_name(id as u16), "hits");
        push_csv_metric_header(out, "task", id, task_name(id as u16), "max");
    }
    for id in 0..COUNTER_COUNT {
        push_csv_metric_header(out, "counter", id, counter_name(id as u16), "total");
        push_csv_metric_header(out, "counter", id, counter_name(id as u16), "max");
        push_csv_metric_header(out, "counter", id, counter_name(id as u16), "latest");
    }
    out.push('\n');
}

fn push_csv_metric_header(out: &mut String, prefix: &str, id: usize, name: &str, suffix: &str) {
    let _ = write!(
        out,
        ",{prefix}_{id}_{name}_{suffix}",
        name = csv_identifier(name)
    );
}

fn csv_identifier(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().max(1));
    let mut last_was_underscore = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_underscore = false;
        } else if !last_was_underscore {
            out.push('_');
            last_was_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

fn push_history_csv_sample(out: &mut String, index: usize, sample: FrameProfileSample) {
    let _ = write!(out, "{index}");
    push_csv_display(out, sample.sample_serial);
    push_csv_f32_3(out, sample.host_dt_ms);
    push_csv_f32_3(out, sample.total_ms);
    push_csv_f32_3(out, sample.input_ms);
    push_csv_f32_3(out, sample.emu_ms);
    push_csv_f32_3(out, sample.audio_ms);
    push_csv_f32_3(out, sample.cmd_log_ms);
    push_csv_f32_3(out, sample.compute_ms);
    push_csv_f32_3(out, sample.vram_upload_ms);
    push_csv_f32_3(out, sample.display_upload_ms);
    push_csv_f32_3(out, sample.hw_scale_ms);
    push_csv_f32_3(out, sample.hw_vram_clone_ms);
    push_csv_f32_3(out, sample.hw_render_ms);
    push_csv_f32_3(out, sample.egui.input_ms);
    push_csv_f32_3(out, sample.egui.ui_ms);
    push_csv_f32_3(out, sample.egui.platform_output_ms);
    push_csv_f32_3(out, sample.egui.tessellate_ms);
    push_csv_f32_3(out, sample.egui.texture_update_ms);
    push_csv_f32_3(out, sample.egui.buffer_update_ms);
    push_csv_f32_3(out, sample.egui.paint_ms);
    push_csv_f32_3(out, sample.egui.submit_present_ms);
    push_csv_f32_3(out, sample.egui.total_ms);
    push_csv_f32_1(out, sample.frames_run);
    push_csv_f32_0(out, sample.cpu_ticks);
    push_csv_f32_0(out, sample.bus_cycles);
    push_csv_f32_0(out, sample.psx_budget_cycles);
    push_csv_f32_1(out, sample.psx_vblanks);
    push_csv_f32_1(out, sample.psx_draw_vblanks);
    push_csv_f32_0(out, sample.psx_step_cap_misses);
    push_csv_f32_0(out, sample.gte_ops);
    push_csv_f32_0(out, sample.gte_estimated_cycles);
    push_csv_f32_0(out, sample.gpu_cmds);
    push_csv_f32_0(out, sample.gpu_words);
    push_csv_f32_0(out, sample.gpu_draw_cmds);
    push_csv_f32_0(out, sample.gpu_image_cmds);
    push_csv_f32_1(out, sample.hw_scale);
    push_csv_f32_3(out, sample.host_fps());
    push_csv_f32_3(out, sample.emulated_vblank_hz());
    push_csv_f32_3(out, sample.psx_draw_hz());
    push_csv_f32_3(out, sample.guest_visual_frame_hz().unwrap_or(0.0));
    push_csv_f32_0(out, sample.guest_visual_frame_count());
    push_csv_f32_3(out, sample.guest_visual_interval_vblanks().unwrap_or(0.0));
    push_csv_f32_0(out, sample.guest_visual_deadline_misses());
    push_csv_f32_0(out, sample.guest_visual_max_lateness_vblanks());
    push_csv_f32_0(out, sample.guest.render_cycles_per_visual_frame());
    push_csv_display(out, sample.guest.paced_visual_budget_status());
    for id in 0..STAGE_COUNT {
        let _ = write!(
            out,
            ",{:.0},{:.0},{:.0}",
            sample.guest.stage_cycles[id],
            sample.guest.stage_hits[id],
            sample.guest.stage_max_cycles[id]
        );
    }
    for id in 0..TASK_COUNT {
        let _ = write!(
            out,
            ",{:.0},{:.0},{:.0}",
            sample.guest.task_cycles[id],
            sample.guest.task_hits[id],
            sample.guest.task_max_cycles[id]
        );
    }
    for id in 0..COUNTER_COUNT {
        let _ = write!(
            out,
            ",{:.0},{:.0},{}",
            sample.guest.counters[id],
            sample.guest.counter_max_values[id],
            sample.guest.counter_latest_values[id]
        );
    }
    out.push('\n');
}

fn push_csv_display(out: &mut String, value: impl std::fmt::Display) {
    let _ = write!(out, ",{value}");
}

fn push_csv_f32_3(out: &mut String, value: f32) {
    let _ = write!(out, ",{value:.3}");
}

fn push_csv_f32_1(out: &mut String, value: f32) {
    let _ = write!(out, ",{value:.1}");
}

fn push_csv_f32_0(out: &mut String, value: f32) {
    let _ = write!(out, ",{value:.0}");
}

fn color_for_ms(ms: f32) -> Color32 {
    if ms >= BUDGET_30_MS {
        Color32::from_rgb(230, 93, 76)
    } else if ms >= BUDGET_60_MS {
        Color32::from_rgb(220, 170, 70)
    } else {
        theme::ACCENT
    }
}

fn fps_from_ms(ms: f32) -> f32 {
    if ms > 0.0 {
        1000.0 / ms
    } else {
        0.0
    }
}

fn per_guest_frame(total: f32, frames_run: f32) -> f32 {
    if frames_run > 0.0 {
        total / frames_run
    } else {
        0.0
    }
}

fn format_log_line(kind: &str, sample: FrameProfileSample) -> String {
    let mut line = format!(
        "[profile {kind}] total={:.2}ms host_dt={:.2}ms fps={:.1} run={:.1} \
         input={:.2}ms cmdlog={:.2}ms compute={:.2}ms disp={:.2}ms hwscale={:.2}ms \
         emu={:.2}ms audio={:.2}ms vram={:.2}ms hw={:.2}ms ui={:.2}ms \
         host_fps={:.1} emu_hz={:.1} vis_hz={:.1} draw_hz={:.1} step={:.1}% \
         cyc_f={:.0} budget_f={:.0} instr_f={:.0} vblanks={:.1} capmiss={:.0} \
         gte_f={:.0} gtecy_f={:.0} cmd_f={:.0} draw_f={:.0} image_f={:.0} words_f={:.0} \
         guest_frames={:.1} guest_render_hit={:.0} guest_models_hit={:.0} guest_player_hit={:.0} \
         guest_flush_hit={:.0} guest_prims_f={:.0} guest_cmds_f={:.0} \
         guest_sim={:.0} guest_visual={:.0} guest_int={:.2} guest_miss={:.0} \
         guest_late={:.0} guest_render_visual={:.0} guest_vbud={} \
         scale={:.0}x ticks={:.0} cycles={:.0}",
        sample.total_ms,
        sample.host_dt_ms,
        fps_from_ms(sample.host_dt_ms),
        sample.frames_run,
        sample.input_ms,
        sample.cmd_log_ms,
        sample.compute_ms,
        sample.display_upload_ms,
        sample.hw_scale_ms,
        sample.emu_ms,
        sample.audio_ms,
        sample.vram_upload_ms,
        sample.hw_render_ms,
        sample.egui.total_ms,
        sample.host_fps(),
        sample.emulated_vblank_hz(),
        sample.guest_visual_frame_hz().unwrap_or(0.0),
        sample.psx_draw_hz(),
        sample.psx_budget_percent(),
        sample.bus_cycles_per_guest_frame(),
        sample.budget_cycles_per_guest_frame(),
        sample.cpu_ticks_per_guest_frame(),
        sample.psx_vblanks,
        sample.psx_step_cap_misses,
        sample.gte_ops_per_guest_frame(),
        sample.gte_cycles_per_guest_frame(),
        sample.gpu_cmds_per_guest_frame(),
        sample.gpu_draw_cmds_per_guest_frame(),
        sample.gpu_image_cmds_per_guest_frame(),
        sample.gpu_words_per_guest_frame(),
        sample.guest.frames,
        sample
            .guest
            .stage_cycles_per_hit(emulator_core::telemetry::stage::RENDER as usize),
        sample
            .guest
            .stage_cycles_per_hit(emulator_core::telemetry::stage::MODEL_INSTANCES as usize),
        sample
            .guest
            .stage_cycles_per_hit(emulator_core::telemetry::stage::PLAYER as usize),
        sample
            .guest
            .stage_cycles_per_hit(emulator_core::telemetry::stage::WORLD_FLUSH as usize),
        sample
            .guest
            .counter_per_guest_frame(emulator_core::telemetry::counter::TRI_PRIMITIVES as usize),
        sample
            .guest
            .counter_per_guest_frame(emulator_core::telemetry::counter::WORLD_COMMANDS as usize),
        sample
            .guest
            .counter_total(emulator_core::telemetry::counter::SIM_TICKS as usize),
        sample
            .guest
            .counter_total(emulator_core::telemetry::counter::VISUAL_FRAMES as usize),
        sample.guest.visual_interval_vblanks(),
        sample
            .guest
            .counter_total(emulator_core::telemetry::counter::VISUAL_DEADLINE_MISSES as usize),
        sample.guest.counter_max_value(
            emulator_core::telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS as usize,
        ),
        sample.guest.render_cycles_per_visual_frame(),
        sample.guest.paced_visual_budget_status(),
        sample.hw_scale.max(1.0),
        sample.cpu_ticks,
        sample.bus_cycles,
    );
    append_guest_profile_log_fields(&mut line, sample.guest);
    line
}

fn append_guest_profile_log_fields(line: &mut String, guest: GuestRuntimeProfile) {
    for &(stage_id, label) in PROFILE_LOG_STAGE_PER_VISUAL_FIELDS {
        let cycles = guest.stage_cycles_per_visual_frame(stage_id as usize);
        let _ = write!(line, " {label}={cycles:.0}");
    }
    for &(stage_id, label) in PROFILE_LOG_STAGE_PER_HIT_FIELDS {
        let cycles = guest.stage_cycles_per_hit(stage_id as usize);
        let _ = write!(line, " {label}={cycles:.0}");
    }
    for &(task_id, label) in PROFILE_LOG_TASK_PER_HIT_FIELDS {
        let cycles = guest.task_cycles_per_hit(task_id as usize);
        let _ = write!(line, " {label}={cycles:.0}");
    }
    for &(counter_id, label) in PROFILE_LOG_COUNTER_PER_VISUAL_FIELDS {
        let value = guest.counter_per_visual_frame(counter_id as usize);
        let _ = write!(line, " {label}={value:.0}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_samples() {
        let mut profiler = FrameProfiler {
            samples: VecDeque::with_capacity(HISTORY_CAP),
            next_sample_serial: 0,
            log_mode: LogMode::Off,
            log_accum_ms: 0.0,
            guest_stage_starts: [None; STAGE_COUNT],
            guest_task_starts: [None; TASK_COUNT],
            guest_last_visual_frame_cycle: None,
            capture: None,
        };
        profiler.record(FrameProfileSample {
            total_ms: 10.0,
            emu_ms: 4.0,
            frames_run: 1.0,
            cpu_ticks: 10_000.0,
            bus_cycles: 100.0,
            psx_budget_cycles: 200.0,
            gte_ops: 4.0,
            gte_estimated_cycles: 40.0,
            gpu_cmds: 100.0,
            egui: EguiRenderProfile {
                total_ms: 2.0,
                ..EguiRenderProfile::default()
            },
            ..FrameProfileSample::default()
        });
        profiler.record(FrameProfileSample {
            total_ms: 20.0,
            emu_ms: 8.0,
            frames_run: 1.0,
            cpu_ticks: 20_000.0,
            bus_cycles: 300.0,
            psx_budget_cycles: 400.0,
            gte_ops: 8.0,
            gte_estimated_cycles: 80.0,
            gpu_cmds: 300.0,
            egui: EguiRenderProfile {
                total_ms: 4.0,
                ..EguiRenderProfile::default()
            },
            ..FrameProfileSample::default()
        });

        let avg = profiler.average().unwrap();
        assert_eq!(avg.total_ms, 15.0);
        assert_eq!(avg.emu_ms, 6.0);
        assert_eq!(avg.gpu_cmds, 200.0);
        assert_eq!(avg.egui.total_ms, 3.0);
        assert!((avg.psx_budget_percent() - (100.0 * 200.0 / 300.0)).abs() < 0.001);
        assert_eq!(avg.cpu_ticks_per_guest_frame(), 15_000.0);
        assert_eq!(avg.gte_ops_per_guest_frame(), 6.0);
        assert_eq!(avg.gte_cycles_per_guest_frame(), 60.0);
    }

    #[test]
    fn recent_average_uses_newest_time_window() {
        let mut profiler = FrameProfiler::default();
        profiler.record(FrameProfileSample {
            host_dt_ms: 60.0,
            total_ms: 60.0,
            emu_ms: 1.0,
            gpu_cmds: 10.0,
            ..FrameProfileSample::default()
        });
        profiler.record(FrameProfileSample {
            host_dt_ms: 60.0,
            total_ms: 60.0,
            emu_ms: 3.0,
            gpu_cmds: 30.0,
            ..FrameProfileSample::default()
        });
        profiler.record(FrameProfileSample {
            host_dt_ms: 60.0,
            total_ms: 60.0,
            emu_ms: 9.0,
            gpu_cmds: 90.0,
            ..FrameProfileSample::default()
        });

        let avg = profiler.average_recent_ms(100.0).unwrap();

        assert_eq!(avg.emu_ms, 6.0);
        assert_eq!(avg.gpu_cmds, 60.0);
        assert_eq!(profiler.live_average().unwrap().emu_ms, 6.0);
    }

    #[test]
    fn guest_stage_spans_can_cross_samples() {
        let mut profiler = FrameProfiler::default();
        let first = [GuestTelemetryEvent {
            cycles: 100,
            kind: emulator_core::telemetry::GuestTelemetryKind::StageBegin,
            id: emulator_core::telemetry::stage::RENDER,
            value: 0,
        }];
        let second = [GuestTelemetryEvent {
            cycles: 250,
            kind: emulator_core::telemetry::GuestTelemetryKind::StageEnd,
            id: emulator_core::telemetry::stage::RENDER,
            value: 0,
        }];

        let a = profiler.consume_guest_events(&first);
        let b = profiler.consume_guest_events(&second);

        assert_eq!(
            a.stage_cycles[emulator_core::telemetry::stage::RENDER as usize],
            0.0
        );
        assert_eq!(
            b.stage_cycles[emulator_core::telemetry::stage::RENDER as usize],
            150.0
        );
        assert_eq!(
            b.stage_max_cycles[emulator_core::telemetry::stage::RENDER as usize],
            150.0
        );
    }

    #[test]
    fn whole_run_capture_survives_the_rolling_history_limit() {
        let mut profiler = FrameProfiler::default();
        profiler.begin_capture();
        for frame in 0..(HISTORY_CAP + 25) {
            let mut sample = FrameProfileSample {
                total_ms: frame as f32,
                host_dt_ms: 16.0,
                ..FrameProfileSample::default()
            };
            sample.guest.counter_latest_values[counter::TRI_PRIMITIVES as usize] = frame as u32;
            sample.guest.counter_latest_values[counter::TRI_PRIMITIVE_REMAINING as usize] =
                1536u32.saturating_sub(frame as u32);
            sample.guest.counter_max_values[counter::TRI_PRIMITIVES as usize] = frame as f32;
            sample.guest.counters[counter::TRI_PRIMITIVES as usize] = frame as f32;
            profiler.record(sample);
        }

        assert_eq!(profiler.history_len(), HISTORY_CAP);
        let report = profiler.finish_capture().unwrap();
        assert_eq!(report.sample_count(), (HISTORY_CAP + 25) as u64);
        let csv = report.csv();
        assert!(csv.contains("counter,1,tri prims"));
        assert!(csv.contains(",1536,"));
        assert!(!profiler.capture_active());
    }

    #[test]
    fn guest_task_spans_track_average_and_worst_hit() {
        let mut profiler = FrameProfiler::default();
        let task_id = emulator_core::telemetry::task::FIXED_UPDATE;
        let events = [
            GuestTelemetryEvent {
                cycles: 100,
                kind: emulator_core::telemetry::GuestTelemetryKind::TaskBegin,
                id: task_id,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 180,
                kind: emulator_core::telemetry::GuestTelemetryKind::TaskEnd,
                id: task_id,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 200,
                kind: emulator_core::telemetry::GuestTelemetryKind::TaskBegin,
                id: task_id,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 340,
                kind: emulator_core::telemetry::GuestTelemetryKind::TaskEnd,
                id: task_id,
                value: 0,
            },
        ];

        let guest = profiler.consume_guest_events(&events);

        assert_eq!(guest.task_cycles[task_id as usize], 220.0);
        assert_eq!(guest.task_hits[task_id as usize], 2.0);
        assert_eq!(guest.task_cycles_per_hit(task_id as usize), 110.0);
        assert_eq!(guest.task_max_cycles[task_id as usize], 140.0);
    }

    #[test]
    fn guest_pacing_counters_track_totals_and_lateness_max() {
        let mut profiler = FrameProfiler::default();
        let events = [
            GuestTelemetryEvent {
                cycles: 10,
                kind: emulator_core::telemetry::GuestTelemetryKind::FrameBegin,
                id: 0,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 20,
                kind: emulator_core::telemetry::GuestTelemetryKind::StageBegin,
                id: emulator_core::telemetry::stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 120,
                kind: emulator_core::telemetry::GuestTelemetryKind::StageEnd,
                id: emulator_core::telemetry::stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 130,
                kind: emulator_core::telemetry::GuestTelemetryKind::Counter,
                id: emulator_core::telemetry::counter::SIM_TICKS,
                value: 3,
            },
            GuestTelemetryEvent {
                cycles: 140,
                kind: emulator_core::telemetry::GuestTelemetryKind::Counter,
                id: emulator_core::telemetry::counter::VISUAL_FRAMES,
                value: 1,
            },
            GuestTelemetryEvent {
                cycles: 150,
                kind: emulator_core::telemetry::GuestTelemetryKind::Counter,
                id: emulator_core::telemetry::counter::VISUAL_INTERVAL_VBLANKS,
                value: 3,
            },
            GuestTelemetryEvent {
                cycles: 160,
                kind: emulator_core::telemetry::GuestTelemetryKind::Counter,
                id: emulator_core::telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS,
                value: 2,
            },
        ];

        let guest = profiler.consume_guest_events(&events);

        assert!(guest.has_pacing_data());
        assert_eq!(
            guest.counter_total(emulator_core::telemetry::counter::SIM_TICKS as usize),
            3.0
        );
        assert_eq!(
            guest.counter_max_value(
                emulator_core::telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS as usize
            ),
            2.0
        );
        assert_eq!(guest.visual_interval_vblanks(), 3.0);
        assert_eq!(guest.render_cycles_per_visual_frame(), 100.0);
        assert_eq!(guest.paced_visual_budget_status(), "pass");
    }

    #[test]
    fn latest_with_all_guest_counters_ignores_partial_scheduler_samples() {
        let mut profiler = FrameProfiler::default();
        let required = [
            emulator_core::telemetry::counter::ROOM_CAMERA_GLOBAL_X_BIASED,
            emulator_core::telemetry::counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
            emulator_core::telemetry::counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
            emulator_core::telemetry::counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        ];
        let loading_counter = emulator_core::telemetry::counter::ROOM_STREAM_LOADING_MASK_LO;

        let mut render_sample = FrameProfileSample::default();
        for &counter_id in &required {
            render_sample.guest.counter_latest_values[counter_id as usize] = 1;
        }
        render_sample.guest.counter_latest_values[loading_counter as usize] = 2;
        profiler.record(render_sample);

        let mut scheduler_sample = FrameProfileSample::default();
        scheduler_sample.guest.counter_latest_values[loading_counter as usize] = 4;
        profiler.record(scheduler_sample);

        assert_eq!(
            profiler
                .latest_with_guest_counters(&[loading_counter])
                .unwrap()
                .guest
                .counter_latest_value(loading_counter as usize),
            4
        );
        assert_eq!(
            profiler
                .latest_with_all_guest_counters(&required)
                .unwrap()
                .guest
                .counter_latest_value(loading_counter as usize),
            2
        );
    }

    #[test]
    fn visual_frame_hz_uses_guest_cycle_intervals() {
        let mut profiler = FrameProfiler::default();
        let visual_frame = |cycles| GuestTelemetryEvent {
            cycles,
            kind: emulator_core::telemetry::GuestTelemetryKind::Counter,
            id: counter::VISUAL_FRAMES,
            value: 1,
        };

        let first = profiler.consume_guest_events(&[visual_frame(100)]);
        let second = profiler.consume_guest_events(&[visual_frame(1_142_572)]);
        let sample = FrameProfileSample {
            host_dt_ms: 1.0,
            guest: second,
            ..FrameProfileSample::default()
        };

        assert_eq!(first.visual_frame_interval_hits, 0.0);
        assert!((sample.guest_visual_frame_hz().unwrap() - 29.645).abs() < 0.001);
        let (intervals_ms, count) = second.visual_frame_intervals_ms();
        assert_eq!(count, 1);
        assert!((intervals_ms[0] - 33.732).abs() < 0.001);
    }

    #[test]
    fn log_line_separates_host_and_guest_work() {
        let line = format_log_line(
            "ui",
            FrameProfileSample {
                host_dt_ms: 8.0,
                total_ms: 5.0,
                frames_run: 1.0,
                bus_cycles: 564_398.0,
                psx_budget_cycles: 564_398.0,
                psx_vblanks: 1.0,
                psx_draw_vblanks: 1.0,
                cpu_ticks: 220_000.0,
                gpu_cmds: 40.0,
                gpu_draw_cmds: 32.0,
                gpu_image_cmds: 2.0,
                gpu_words: 280.0,
                gte_ops: 96.0,
                gte_estimated_cycles: 1_700.0,
                ..FrameProfileSample::default()
            },
        );

        assert!(line.contains("host_dt=8.00ms"));
        assert!(line.contains("host_fps=125.0"));
        assert!(line.contains("emu_hz=125.0"));
        assert!(line.contains("vis_hz=0.0"));
        assert!(line.contains("draw_hz=60.0"));
        assert!(line.contains("step=100.0%"));
        assert!(line.contains("cyc_f=564398"));
        assert!(line.contains("gte_f=96"));
        assert!(line.contains("draw_f=32"));
        assert!(line.contains("guest_vbud=?"));
    }
}
