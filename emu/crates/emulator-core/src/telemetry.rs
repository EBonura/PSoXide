//! Guest-runtime telemetry captured out-of-band by the emulator.
//!
//! Instrumented homebrew writes compact event words into a reserved slice of
//! Expansion Region 2. Retail software sees the normal expansion-port
//! behaviour, while PSoXide can timestamp those writes with the emulator's bus
//! cycle counter and surface the result in host-side tooling.

use std::collections::VecDeque;

use psx_hw::memory;

/// Physical base of PSoXide's emulator-only telemetry port.
pub const BASE_PHYS: u32 = memory::expansion2::BASE + 0x0F00;
/// Event command register. A write appends one telemetry event.
pub const EVENT_PHYS: u32 = BASE_PHYS;
/// Event value register. The next command write snapshots this value.
pub const VALUE_PHYS: u32 = BASE_PHYS + 4;
/// Read-only low 32 bits of the emulator-observed guest cycle counter.
pub const CYCLE_PHYS: u32 = BASE_PHYS + 8;
/// Write-only byte sink for guest debug text. Newline commits one log line.
pub const LOG_PHYS: u32 = BASE_PHYS + 12;

const EVENT_CAP: usize = 65_536;
const LOG_CAP: usize = 2_048;
const LOG_LINE_CAP: usize = 384;
const KIND_SHIFT: u32 = 24;
const KIND_MASK: u32 = 0xFF;
const ID_MASK: u32 = 0xFFFF;

pub use psx_telemetry::{
    counter, counter_desc, stage, stage_desc, task, task_desc, COUNTER_COUNT, STAGE_COUNT,
    TASK_COUNT,
};

/// Telemetry event kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestTelemetryKind {
    /// A new guest frame began; `value` is the guest frame index.
    FrameBegin,
    /// A named runtime stage began.
    StageBegin,
    /// A named runtime stage ended.
    StageEnd,
    /// A numeric counter was emitted.
    Counter,
    /// A scheduled task began.
    TaskBegin,
    /// A scheduled task ended.
    TaskEnd,
    /// Unknown event kind preserved for diagnostics.
    Unknown(u8),
}

impl GuestTelemetryKind {
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::FrameBegin,
            2 => Self::StageBegin,
            3 => Self::StageEnd,
            4 => Self::Counter,
            5 => Self::TaskBegin,
            6 => Self::TaskEnd,
            other => Self::Unknown(other),
        }
    }
}

/// One telemetry event timestamped by the emulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestTelemetryEvent {
    /// Bus cycles elapsed when the guest wrote the event command.
    pub cycles: u64,
    /// Event kind.
    pub kind: GuestTelemetryKind,
    /// Stage or counter id, depending on [`kind`](Self::kind).
    pub id: u16,
    /// Latched value from [`VALUE_PHYS`].
    pub value: u32,
}

/// One guest debug line emitted through the telemetry log byte sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestDebugLogLine {
    /// Bus cycles elapsed when the line began.
    pub cycles: u64,
    /// Guest frame count observed when the line began.
    pub frame: u64,
    /// Sanitised ASCII text emitted by the guest.
    pub text: String,
}

/// Rolling capture buffer for guest telemetry events.
pub struct GuestTelemetry {
    pending_value: u32,
    events: VecDeque<GuestTelemetryEvent>,
    debug_logs: VecDeque<GuestDebugLogLine>,
    pending_debug_log: String,
    pending_debug_log_cycles: u64,
    pending_debug_log_frame: u64,
    pending_debug_log_truncated: bool,
    frames_seen: u64,
    counter_totals: [u64; COUNTER_COUNT],
    counter_max_values: [u32; COUNTER_COUNT],
    counter_latest_values: [u32; COUNTER_COUNT],
}

impl Default for GuestTelemetry {
    fn default() -> Self {
        Self {
            pending_value: 0,
            events: VecDeque::with_capacity(EVENT_CAP),
            debug_logs: VecDeque::with_capacity(LOG_CAP),
            pending_debug_log: String::with_capacity(LOG_LINE_CAP),
            pending_debug_log_cycles: 0,
            pending_debug_log_frame: 0,
            pending_debug_log_truncated: false,
            frames_seen: 0,
            counter_totals: [0; COUNTER_COUNT],
            counter_max_values: [0; COUNTER_COUNT],
            counter_latest_values: [0; COUNTER_COUNT],
        }
    }
}

impl GuestTelemetry {
    /// Create an empty telemetry buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `phys` lands inside the telemetry port.
    pub const fn contains(phys: u32) -> bool {
        phys == EVENT_PHYS || phys == VALUE_PHYS || phys == CYCLE_PHYS || phys == LOG_PHYS
    }

    /// Observe a 32-bit read. Returns a value if the telemetry port consumed it.
    pub const fn observe_read32(&self, phys: u32, cycles: u64) -> Option<u32> {
        match phys {
            CYCLE_PHYS => Some(cycles as u32),
            _ => None,
        }
    }

    /// Observe a 32-bit write. Returns true if the telemetry port consumed it.
    pub fn observe_write32(&mut self, phys: u32, value: u32, cycles: u64) -> bool {
        match phys {
            VALUE_PHYS => {
                self.pending_value = value;
                true
            }
            EVENT_PHYS => {
                let raw_kind = ((value >> KIND_SHIFT) & KIND_MASK) as u8;
                let id = (value & ID_MASK) as u16;
                self.push(GuestTelemetryEvent {
                    cycles,
                    kind: GuestTelemetryKind::from_raw(raw_kind),
                    id,
                    value: self.pending_value,
                });
                true
            }
            LOG_PHYS => {
                self.push_debug_log_byte((value & 0xFF) as u8, cycles);
                true
            }
            _ => false,
        }
    }

    /// Drain all captured events in chronological order.
    pub fn drain_events(&mut self) -> Vec<GuestTelemetryEvent> {
        self.events.drain(..).collect()
    }

    /// Drain all complete guest debug log lines in chronological order.
    pub fn drain_debug_logs(&mut self) -> Vec<GuestDebugLogLine> {
        self.debug_logs.drain(..).collect()
    }

    /// Number of guest frame-begin markers observed since reset.
    pub const fn frames_seen(&self) -> u64 {
        self.frames_seen
    }

    /// Summed value observed for a known counter since reset.
    pub fn counter_total(&self, id: u16) -> u64 {
        self.counter_totals
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Largest single value observed for a known counter since reset.
    pub fn counter_max_value(&self, id: u16) -> u32 {
        self.counter_max_values
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Most recent single value observed for a known counter since reset.
    pub fn counter_latest_value(&self, id: u16) -> u32 {
        self.counter_latest_values
            .get(id as usize)
            .copied()
            .unwrap_or_default()
    }

    /// Snapshot of all summed counter values observed since reset.
    pub const fn counter_totals(&self) -> [u64; COUNTER_COUNT] {
        self.counter_totals
    }

    /// Snapshot of all largest counter values observed since reset.
    pub const fn counter_max_values(&self) -> [u32; COUNTER_COUNT] {
        self.counter_max_values
    }

    /// Snapshot of the most recent counter values observed since reset.
    pub const fn counter_latest_values(&self) -> [u32; COUNTER_COUNT] {
        self.counter_latest_values
    }

    fn push(&mut self, event: GuestTelemetryEvent) {
        if matches!(event.kind, GuestTelemetryKind::FrameBegin) {
            self.frames_seen = self.frames_seen.saturating_add(1);
        }
        if matches!(event.kind, GuestTelemetryKind::Counter) {
            if let Some(total) = self.counter_totals.get_mut(event.id as usize) {
                *total = total.saturating_add(event.value as u64);
            }
            if let Some(max_value) = self.counter_max_values.get_mut(event.id as usize) {
                *max_value = (*max_value).max(event.value);
            }
            if let Some(latest_value) = self.counter_latest_values.get_mut(event.id as usize) {
                *latest_value = event.value;
            }
        }
        if self.events.len() >= EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn push_debug_log_byte(&mut self, byte: u8, cycles: u64) {
        match byte {
            b'\n' | 0 => {
                self.flush_debug_log(cycles);
            }
            b'\r' => {}
            byte => {
                if self.pending_debug_log.is_empty() && !self.pending_debug_log_truncated {
                    self.pending_debug_log_cycles = cycles;
                    self.pending_debug_log_frame = self.frames_seen;
                }
                if self.pending_debug_log.len() < LOG_LINE_CAP {
                    self.pending_debug_log.push(sanitise_debug_log_byte(byte));
                } else {
                    self.pending_debug_log_truncated = true;
                }
            }
        }
    }

    fn flush_debug_log(&mut self, cycles: u64) {
        if self.pending_debug_log.is_empty() && !self.pending_debug_log_truncated {
            return;
        }
        if self.pending_debug_log_cycles == 0 {
            self.pending_debug_log_cycles = cycles;
            self.pending_debug_log_frame = self.frames_seen;
        }
        if self.pending_debug_log_truncated {
            self.pending_debug_log.push_str("...");
        }
        if self.debug_logs.len() >= LOG_CAP {
            self.debug_logs.pop_front();
        }
        self.debug_logs.push_back(GuestDebugLogLine {
            cycles: self.pending_debug_log_cycles,
            frame: self.pending_debug_log_frame,
            text: core::mem::take(&mut self.pending_debug_log),
        });
        self.pending_debug_log_cycles = 0;
        self.pending_debug_log_frame = 0;
        self.pending_debug_log_truncated = false;
    }
}

fn sanitise_debug_log_byte(byte: u8) -> char {
    if byte == b'\t' || byte == b' ' || byte.is_ascii_graphic() {
        byte as char
    } else {
        '.'
    }
}

/// Aggregated guest telemetry over a span of events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestTelemetrySummary {
    /// Number of guest frame-begin markers observed.
    pub frames: u64,
    /// Total cycles spent in each known stage id.
    pub stage_cycles: [u64; STAGE_COUNT],
    /// Number of completed spans per known stage id.
    pub stage_hits: [u64; STAGE_COUNT],
    /// Largest single completed span per known stage id.
    pub stage_max_cycles: [u64; STAGE_COUNT],
    /// Total cycles spent in each known scheduled task id.
    pub task_cycles: [u64; TASK_COUNT],
    /// Number of completed spans per known scheduled task id.
    pub task_hits: [u64; TASK_COUNT],
    /// Largest single completed span per known scheduled task id.
    pub task_max_cycles: [u64; TASK_COUNT],
    /// Summed counter values per known counter id.
    pub counters: [u64; COUNTER_COUNT],
    /// Largest single value observed per known counter id.
    pub counter_max_values: [u32; COUNTER_COUNT],
    /// Most recent value observed per known counter id.
    pub counter_latest_values: [u32; COUNTER_COUNT],
}

impl Default for GuestTelemetrySummary {
    fn default() -> Self {
        Self {
            frames: 0,
            stage_cycles: [0; STAGE_COUNT],
            stage_hits: [0; STAGE_COUNT],
            stage_max_cycles: [0; STAGE_COUNT],
            task_cycles: [0; TASK_COUNT],
            task_hits: [0; TASK_COUNT],
            task_max_cycles: [0; TASK_COUNT],
            counters: [0; COUNTER_COUNT],
            counter_max_values: [0; COUNTER_COUNT],
            counter_latest_values: [0; COUNTER_COUNT],
        }
    }
}

impl GuestTelemetrySummary {
    /// Build a summary from raw telemetry events.
    pub fn from_events(events: &[GuestTelemetryEvent]) -> Self {
        let mut out = Self::default();
        out.add_events(events);
        out
    }

    /// Add raw events to this summary.
    pub fn add_events(&mut self, events: &[GuestTelemetryEvent]) {
        let mut stage_start: [Option<u64>; STAGE_COUNT] = [None; STAGE_COUNT];
        let mut task_start: [Option<u64>; TASK_COUNT] = [None; TASK_COUNT];
        for event in events {
            match event.kind {
                GuestTelemetryKind::FrameBegin => {
                    self.frames = self.frames.saturating_add(1);
                }
                GuestTelemetryKind::StageBegin => {
                    if let Some(slot) = stage_start.get_mut(event.id as usize) {
                        *slot = Some(event.cycles);
                    }
                }
                GuestTelemetryKind::StageEnd => {
                    let Some(slot) = stage_start.get_mut(event.id as usize) else {
                        continue;
                    };
                    let Some(start) = slot.take() else {
                        continue;
                    };
                    let idx = event.id as usize;
                    let elapsed = event.cycles.saturating_sub(start);
                    self.stage_cycles[idx] = self.stage_cycles[idx].saturating_add(elapsed);
                    self.stage_hits[idx] = self.stage_hits[idx].saturating_add(1);
                    self.stage_max_cycles[idx] = self.stage_max_cycles[idx].max(elapsed);
                }
                GuestTelemetryKind::TaskBegin => {
                    if let Some(slot) = task_start.get_mut(event.id as usize) {
                        *slot = Some(event.cycles);
                    }
                }
                GuestTelemetryKind::TaskEnd => {
                    let Some(slot) = task_start.get_mut(event.id as usize) else {
                        continue;
                    };
                    let Some(start) = slot.take() else {
                        continue;
                    };
                    let idx = event.id as usize;
                    let elapsed = event.cycles.saturating_sub(start);
                    self.task_cycles[idx] = self.task_cycles[idx].saturating_add(elapsed);
                    self.task_hits[idx] = self.task_hits[idx].saturating_add(1);
                    self.task_max_cycles[idx] = self.task_max_cycles[idx].max(elapsed);
                }
                GuestTelemetryKind::Counter => {
                    let idx = event.id as usize;
                    if let Some(counter) = self.counters.get_mut(idx) {
                        *counter = counter.saturating_add(event.value as u64);
                    }
                    if let Some(max_value) = self.counter_max_values.get_mut(idx) {
                        *max_value = (*max_value).max(event.value);
                    }
                    if let Some(latest_value) = self.counter_latest_values.get_mut(idx) {
                        *latest_value = event.value;
                    }
                }
                GuestTelemetryKind::Unknown(_) => {}
            }
        }
    }

    /// True when at least one event contributed useful data.
    pub fn has_data(&self) -> bool {
        self.frames > 0
            || self.stage_cycles.iter().any(|&cycles| cycles > 0)
            || self.task_cycles.iter().any(|&cycles| cycles > 0)
            || self.counters.iter().any(|&value| value > 0)
    }
}

/// Human-readable stage name for host tooling.
pub fn stage_name(id: u16) -> &'static str {
    match id {
        stage::UPDATE => "update",
        stage::FRAME_CLEAR => "frame clear",
        stage::RENDER => "render total",
        stage::PRESENT => "present/wait",
        stage::CAMERA => "camera",
        stage::ROOM => "room",
        stage::ENTITY_MARKERS => "entity markers",
        stage::MODEL_INSTANCES => "model instances",
        stage::PLAYER => "player",
        stage::MODEL_BOUNDS => "model bounds",
        stage::MODEL_DRAW => "model draw",
        stage::PLAYER_BOUNDS => "player bounds",
        stage::PLAYER_DRAW => "player draw",
        stage::TEXTURED_MODEL_JOINTS => "mdl joints",
        stage::TEXTURED_MODEL_PROJECT => "mdl project",
        stage::TEXTURED_MODEL_FACES => "mdl faces",
        stage::ACTIVE_ROOM_WINDOW => "room window",
        stage::ROOM_SURFACE_CACHE => "room cache build",
        stage::VRAM_UPLOAD => "vram upload",
        stage::CD_STREAM_BENCH => "cd stream bench",
        stage::CD_STREAM_STEADY => "cd stream steady",
        stage::CD_WORLD_PACK_STREAM => "cd world pack",
        stage::CD_ROOM_CHUNK_LOAD => "cd room chunk load",
        stage::ROOM_VISIBLE_LIST => "room visible list",
        stage::ROOM_CELL_SELECT => "room cell select",
        stage::ROOM_PROJECT => "room project",
        stage::ROOM_DEPTH_PREP => "room depth prep",
        stage::ROOM_SURFACE_DRAW => "room surface draw",
        stage::SKY => "sky",
        stage::FAR_VISTA => "far vista",
        stage::IMAGE_PROPS => "image props",
        stage::PORTAL_VISIBILITY => "portal visibility",
        stage::EQUIPMENT => "equipment",
        stage::WORLD_FLUSH => "world flush/sort",
        stage::OT_SUBMIT => "ot submit (kick)",
        stage::OT_WAIT => "ot wait (gpu/dma)",
        stage::SIM_COLLISION => "sim collision",
        stage::SIM_ROOM_TRACK => "sim room track",
        stage::SIM_RESIDENCY => "sim residency",
        stage::SIM_PUMP => "sim pump",
        stage::SIM_SOLVE => "sim solve",
        stage::BOX_PROPS => "box props",
        stage::BOX_PROP_DEBRIS => "box debris",
        stage::BOX_PROP_SHARDS => "box shards",
        stage::IMAGE_CARDS => "image cards",
        stage::UPDATE_ACTOR => "update actor",
        stage::UPDATE_WINDOW => "update window",
        stage::CELL_LOOKUP => "cell lookup",
        stage::CELL_DEPTH => "cell depth",
        stage::CELL_COLLECT => "cell collect",
        stage::GAME_LOGIC => "game logic",
        _ => "unknown",
    }
}

/// Human-readable task name for host tooling.
pub fn task_name(id: u16) -> &'static str {
    match id {
        task::FIXED_UPDATE => "fixed update",
        task::VISUAL_RENDER => "visual render",
        _ => "unknown",
    }
}

/// Human-readable counter name for host tooling.
pub fn counter_name(id: u16) -> &'static str {
    match id {
        counter::TRI_PRIMITIVES => "tri prims",
        counter::WORLD_COMMANDS => "world commands",
        counter::MODEL_INSTANCE_DRAWS => "model draws",
        counter::MODEL_INSTANCE_PROJECTED_VERTICES => "model verts",
        counter::MODEL_INSTANCE_SUBMITTED_TRIS => "model tris",
        counter::MODEL_INSTANCE_CULLED_TRIS => "model culled",
        counter::MODEL_INSTANCE_DROPPED_TRIS => "model dropped",
        counter::PLAYER_PROJECTED_VERTICES => "player verts",
        counter::PLAYER_SUBMITTED_TRIS => "player tris",
        counter::PLAYER_CULLED_TRIS => "player culled",
        counter::PLAYER_DROPPED_TRIS => "player dropped",
        counter::MODEL_OVERFLOW_FLAGS => "overflow flags",
        counter::ROOM_CELLS_CONSIDERED => "room cells",
        counter::ROOM_CELLS_DRAWN => "room cells drawn",
        counter::ROOM_CELLS_CULLED => "room cells culled",
        counter::ROOM_SURFACES_CONSIDERED => "room surfaces",
        counter::EQUIPMENT_DRAWS => "equipment draws",
        counter::EQUIPMENT_PROJECTED_VERTICES => "equipment verts",
        counter::EQUIPMENT_SUBMITTED_TRIS => "equipment tris",
        counter::EQUIPMENT_CULLED_TRIS => "equipment culled",
        counter::EQUIPMENT_DROPPED_TRIS => "equipment dropped",
        counter::MODEL_INSTANCE_BOUNDS_TESTS => "model bound tests",
        counter::MODEL_INSTANCE_BOUNDS_CULLED => "model bound culled",
        counter::PLAYER_BOUNDS_TESTS => "player bound tests",
        counter::PLAYER_BOUNDS_CULLED => "player bound culled",
        counter::TEXTURED_MODEL_JOINTS => "mdl joints",
        counter::TEXTURED_MODEL_PARTS => "mdl parts",
        counter::TEXTURED_MODEL_VERTICES => "mdl verts",
        counter::TEXTURED_MODEL_FACES => "mdl faces",
        counter::ROOM_ACTIVE_CHUNKS => "room chunks",
        counter::ROOM_VISIBLE_CELLS => "room visible cells",
        counter::ROOM_CACHED_DRAWS => "room cached draws",
        counter::ROOM_UNCACHED_DRAWS => "room uncached draws",
        counter::TRI_PRIMITIVE_REMAINING => "tri slots free",
        counter::ROOM_CACHE_CELLS => "room cache cells",
        counter::ROOM_CACHE_VERTICES => "room cache verts",
        counter::ROOM_CACHE_SURFACES => "room cache surfaces",
        counter::ROOM_CACHE_FALLBACK_DRAWS => "room cache fallbacks",
        counter::ROOM_VISIBILITY_FALLBACK_DRAWS => "room visibility fallbacks",
        counter::ROOM_CELLS_RANGE_CULLED => "room range culled",
        counter::ROOM_CHUNKS_CONSIDERED => "room chunks considered",
        counter::ROOM_CHUNK_CACHE_SKIPS => "room chunk cache skips",
        counter::ROOM_WINDOW_REBUILDS => "room window rebuilds",
        counter::ROOM_WINDOW_BUILT_CHUNKS => "room window chunks",
        counter::ROOM_SURFACE_CACHE_BUILDS => "room cache builds",
        counter::ROOM_SURFACE_CACHE_BUILD_CELLS => "cache build cells",
        counter::ROOM_SURFACE_CACHE_BUILD_VERTICES => "cache build verts",
        counter::ROOM_SURFACE_CACHE_BUILD_SURFACES => "cache build surfaces",
        counter::ROOM_TEXTURE_UPLOADS => "room texture uploads",
        counter::MODEL_ATLAS_UPLOADS => "model atlas uploads",
        counter::SIM_TICKS => "sim ticks",
        counter::VISUAL_FRAMES => "visual frames",
        counter::VISUAL_SKIPPED_VBLANKS => "visual skipped vblanks",
        counter::VISUAL_DEADLINE_MISSES => "visual deadline misses",
        counter::VISUAL_INTERVAL_VBLANKS => "visual interval vblanks",
        counter::VISUAL_MAX_LATENESS_VBLANKS => "visual max lateness vblanks",
        counter::CD_STREAM_BENCH_BYTES => "cd stream bytes",
        counter::CD_STREAM_BENCH_SECTORS => "cd stream sectors",
        counter::CD_STREAM_BENCH_POLLS => "cd stream polls",
        counter::CD_STREAM_BENCH_CHECKSUM => "cd stream checksum",
        counter::CD_STREAM_BENCH_EXPECTED_CHECKSUM => "cd stream expected checksum",
        counter::CD_STREAM_BENCH_STATUS => "cd stream status",
        counter::CD_STREAM_STEADY_BYTES => "cd steady bytes",
        counter::CD_STREAM_STEADY_SECTORS => "cd steady sectors",
        counter::CD_WORLD_PACK_BYTES => "cd world bytes",
        counter::CD_WORLD_PACK_SECTORS => "cd world sectors",
        counter::CD_WORLD_PACK_CHUNKS => "cd world chunks",
        counter::CD_WORLD_PACK_CHECKSUM => "cd world checksum",
        counter::CD_WORLD_PACK_STATUS => "cd world status",
        counter::CD_ROOM_CHUNK_BYTES => "cd room chunk bytes",
        counter::CD_ROOM_CHUNK_SECTORS => "cd room chunk sectors",
        counter::CD_ROOM_CHUNK_LOADS => "cd room chunk loads",
        counter::CD_ROOM_CHUNK_HITS => "cd room chunk hits",
        counter::CD_ROOM_CHUNK_STATUS => "cd room chunk status",
        counter::ROOM_STREAM_REQUESTS => "room stream requests",
        counter::ROOM_STREAM_MISSES => "room stream misses",
        counter::ROOM_STREAM_PREFETCH_REQUESTS => "room stream prefetches",
        counter::ROOM_STREAM_RESIDENT_SLOTS => "room stream resident slots",
        counter::ROOM_STREAM_EVICTIONS => "room stream evictions",
        counter::ROOM_STREAM_FAILED_LOADS => "room stream failed loads",
        counter::ROOM_STREAM_PENDING_LOADS => "room stream pending loads",
        counter::ROOM_PROJECTED_VERTICES => "room projected verts",
        counter::ROOM_SURF_MATERIAL_CYCLES => "room surf material cyc",
        counter::ROOM_SURF_PROJECTED_CYCLES => "room surf projected cyc",
        counter::ROOM_SURF_SCREEN_CYCLES => "room surf screen cyc",
        counter::ROOM_SURF_KIND_CYCLES => "room surf kind cyc",
        counter::ROOM_SURF_BACKFACE_CYCLES => "room surf backface cyc",
        counter::ROOM_SURF_LIGHTING_CYCLES => "room surf lighting cyc",
        counter::ROOM_SURF_SUBMIT_CYCLES => "room surf submit cyc",
        counter::ROOM_SURF_PROFILED => "room surf profiled",
        counter::ROOM_SURF_MATERIAL_MISSES => "room surf material misses",
        counter::ROOM_SURF_PROJECTED_REJECTS => "room surf projected rejects",
        counter::ROOM_SURF_SCREEN_CULLED => "room surf screen culled",
        counter::ROOM_SURF_BACKFACE_CULLED => "room surf backface culled",
        counter::ROOM_SURF_FLOORS => "room surf floors",
        counter::ROOM_SURF_CEILINGS => "room surf ceilings",
        counter::ROOM_SURF_WALLS => "room surf walls",
        counter::ROOM_SURF_WHOLE_QUADS => "room surf whole quads",
        counter::ROOM_SURF_SPLIT_TRIS => "room surf split tris",
        counter::ROOM_SURF_LIGHTING_REJECTS => "room surf lighting rejects",
        counter::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES => "room submit hw-safe cyc",
        counter::ROOM_SUBMIT_PACKET_FILL_CYCLES => "room submit packet cyc",
        counter::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES => "room submit prim push cyc",
        counter::ROOM_SUBMIT_DEPTH_CYCLES => "room submit depth cyc",
        counter::ROOM_SUBMIT_COMMAND_CYCLES => "room submit command cyc",
        counter::ROOM_SUBMIT_FALLBACK_CYCLES => "room submit fallback cyc",
        counter::ROOM_SUBMIT_HW_SAFE_CALLS => "room submit hw-safe calls",
        counter::ROOM_SUBMIT_FALLBACK_CALLS => "room submit fallback calls",
        counter::ROOM_SUBMIT_COMMAND_OVERFLOWS => "room submit command overflows",
        counter::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS => "room submit prim overflows",
        counter::MODEL_PROFILE_CYCLES_0 => "model0 cycles",
        counter::MODEL_PROFILE_CYCLES_1 => "model1 cycles",
        counter::MODEL_PROFILE_CYCLES_2 => "model2 cycles",
        counter::MODEL_PROFILE_CYCLES_3 => "model3 cycles",
        counter::MODEL_PROFILE_CYCLES_4 => "model4 cycles",
        counter::MODEL_PROFILE_CYCLES_5 => "model5 cycles",
        counter::MODEL_PROFILE_CYCLES_6 => "model6 cycles",
        counter::MODEL_PROFILE_CYCLES_7 => "model7 cycles",
        counter::MODEL_PROFILE_DRAWS_0 => "model0 draws",
        counter::MODEL_PROFILE_DRAWS_1 => "model1 draws",
        counter::MODEL_PROFILE_DRAWS_2 => "model2 draws",
        counter::MODEL_PROFILE_DRAWS_3 => "model3 draws",
        counter::MODEL_PROFILE_DRAWS_4 => "model4 draws",
        counter::MODEL_PROFILE_DRAWS_5 => "model5 draws",
        counter::MODEL_PROFILE_DRAWS_6 => "model6 draws",
        counter::MODEL_PROFILE_DRAWS_7 => "model7 draws",
        counter::ROOM_STREAM_RESIDENT_MASK_LO => "resident chunk mask lo",
        counter::ROOM_STREAM_RESIDENT_MASK_HI => "resident chunk mask hi",
        counter::ROOM_ACTIVE_CHUNK_MASK_LO => "active chunk mask lo",
        counter::ROOM_ACTIVE_CHUNK_MASK_HI => "active chunk mask hi",
        counter::ROOM_DRAWN_CHUNK_MASK_LO => "drawn chunk mask lo",
        counter::ROOM_DRAWN_CHUNK_MASK_HI => "drawn chunk mask hi",
        counter::ROOM_PLAYER_ROOM_INDEX => "player room index",
        counter::ROOM_PLAYER_LOCAL_X_BIASED => "player local x",
        counter::ROOM_PLAYER_LOCAL_Z_BIASED => "player local z",
        counter::ROOM_PLAYER_VIEW_YAW_Q12 => "player view yaw q12",
        counter::ROOM_CAMERA_LOCAL_X_BIASED => "camera local x",
        counter::ROOM_CAMERA_LOCAL_Z_BIASED => "camera local z",
        counter::PORTAL_VIS_CURRENT_ROOM => "portal current room",
        counter::PORTAL_VIS_VISIBLE_ROOMS => "portal visible rooms",
        counter::PORTAL_VIS_FRONTIER_ROOMS => "portal frontier rooms",
        counter::PORTAL_VIS_FRUSTUMS => "portal frustums",
        counter::PORTAL_VIS_PORTALS_TESTED => "portal tests",
        counter::PORTAL_VIS_PORTALS_ACCEPTED => "portal accepts",
        counter::PORTAL_VIS_REJECT_BACKFACE => "portal reject backface",
        counter::PORTAL_VIS_REJECT_FRUSTUM => "portal reject frustum",
        counter::PORTAL_VIS_REJECT_TINY => "portal reject tiny",
        counter::PORTAL_VIS_CAP_ROOM => "portal room cap",
        counter::PORTAL_VIS_CAP_FRUSTUM => "portal frustum cap",
        counter::PORTAL_VIS_CAP_DEPTH => "portal depth cap",
        counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT => "portal visible missing resident",
        counter::ROOM_STREAM_PRIORITY_CURRENT => "stream priority current",
        counter::ROOM_STREAM_PRIORITY_VISIBLE => "stream priority visible",
        counter::ROOM_STREAM_PRIORITY_FRONTIER => "stream priority frontier",
        counter::ROOM_STREAM_PROTECTED_FULL => "stream protected full",
        counter::PORTAL_VIS_VISIBLE_MASK_LO => "portal visible mask lo",
        counter::PORTAL_VIS_VISIBLE_MASK_HI => "portal visible mask hi",
        counter::PORTAL_VIS_FRONTIER_MASK_LO => "portal frontier mask lo",
        counter::PORTAL_VIS_FRONTIER_MASK_HI => "portal frontier mask hi",
        counter::PORTAL_VIS_MISSING_MASK_LO => "portal missing mask lo",
        counter::PORTAL_VIS_MISSING_MASK_HI => "portal missing mask hi",
        counter::PORTAL_VIS_TESTED_MASK_LO => "portal tested mask lo",
        counter::PORTAL_VIS_TESTED_MASK_HI => "portal tested mask hi",
        counter::PORTAL_VIS_ACCEPTED_MASK_LO => "portal accepted mask lo",
        counter::PORTAL_VIS_ACCEPTED_MASK_HI => "portal accepted mask hi",
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO => "portal frustum reject mask lo",
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI => "portal frustum reject mask hi",
        counter::PORTAL_VIS_BOUNDS_FALLBACKS => "portal bounds fallback",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO => "portal bounds fallback mask lo",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI => "portal bounds fallback mask hi",
        counter::ROOM_STREAM_SLOT_LIMIT => "room stream slot limit",
        counter::ROOM_STREAM_LOADING_MASK_LO => "loading chunk mask lo",
        counter::ROOM_STREAM_LOADING_MASK_HI => "loading chunk mask hi",
        counter::PORTAL_VIS_VISIBLE_BUILD_FAILED => "portal visible build failed",
        counter::PORTAL_VIS_BUILD_FAILED_MASK_LO => "portal build failed mask lo",
        counter::PORTAL_VIS_BUILD_FAILED_MASK_HI => "portal build failed mask hi",
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO => "portal tested portal mask lo",
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI => "portal tested portal mask hi",
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO => "portal accepted portal mask lo",
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI => "portal accepted portal mask hi",
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO => "portal frustum reject portal mask lo",
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI => "portal frustum reject portal mask hi",
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO => {
            "portal bounds fallback portal mask lo"
        }
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI => {
            "portal bounds fallback portal mask hi"
        }
        counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED => "camera view sin yaw q12 biased",
        counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED => "camera view cos yaw q12 biased",
        counter::ROOM_CAMERA_LOCAL_Y_BIASED => "camera local y",
        counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED => "camera view sin pitch q12 biased",
        counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED => "camera view cos pitch q12 biased",
        counter::ROOM_CAMERA_GLOBAL_X_BIASED => "camera global x",
        counter::ROOM_CAMERA_GLOBAL_Y_BIASED => "camera global y",
        counter::ROOM_CAMERA_GLOBAL_Z_BIASED => "camera global z",
        counter::TEXTURED_MODEL_CPU_BLEND_VERTICES => "mdl cpu blend verts",
        counter::TEXTURED_MODEL_PACKED_FACE_CALLS => "mdl packed faces",
        counter::TEXTURED_MODEL_PACKED_UNCLAMPED_CALLS => "mdl packed unclamped faces",
        counter::TEXTURED_MODEL_PACKED_CLAMPED_CALLS => "mdl packed clamped faces",
        counter::TEXTURED_MODEL_PACKED_GENERAL_CALLS => "mdl packed general faces",
        counter::TEXTURED_MODEL_FALLBACK_FACE_CALLS => "mdl fallback faces",
        counter::TEXTURED_MODEL_HW_EXTENT_FALLBACKS => "mdl hw extent fallbacks",
        counter::TEXTURED_MODEL_NEAR_DROPS => "mdl near drops",
        counter::TEXTURED_MODEL_HW_UNSAFE_DROPS => "mdl hw unsafe drops",
        counter::TEXTURED_MODEL_FAST_SUBMITTED_TRIS => "mdl fast tris",
        counter::TEXTURED_MODEL_CPU_BLEND_SUBMITS => "mdl cpu blend submits",
        counter::TEXTURED_MODEL_PRIMARY_JOINT_SUBMITS => "mdl primary joint submits",
        counter::TEXTURED_MODEL_ALL_FRONT_SUBMITS => "mdl all front submits",
        counter::TEXTURED_MODEL_ALL_HW_BOUNDS_SUBMITS => "mdl all hw bounds submits",
        counter::TEXTURED_MODEL_PACKED_UNCLAMPED_ELIGIBLE_SUBMITS => {
            "mdl packed unclamped eligible"
        }
        counter::TEXTURED_MODEL_PACKED_CLAMPED_ELIGIBLE_SUBMITS => "mdl packed clamped eligible",
        counter::TEXTURED_MODEL_PACKED_GENERAL_ELIGIBLE_SUBMITS => "mdl packed general eligible",
        counter::TEXTURED_MODEL_SPLIT_TRIS => "mdl split tris",
        counter::TEXTURED_MODEL_SKIPPED_TRIS => "mdl skipped tris",
        counter::TEXTURED_MODEL_VERTEX_OVERFLOW_SUBMITS => "mdl vertex overflow submits",
        counter::TEXTURED_MODEL_PRIMITIVE_OVERFLOW_SUBMITS => "mdl primitive overflow submits",
        counter::TEXTURED_MODEL_COMMAND_OVERFLOW_SUBMITS => "mdl command overflow submits",
        counter::VRAM_SLOTS_FREED => "vram slots freed",
        counter::VRAM_SLOT_TABLE_FULL => "vram slot table full",
        counter::VRAM_WINDOW_FULL => "vram window full",
        counter::VRAM_CLUT_FULL => "vram clut full",
        counter::VRAM_UPLOAD_QUEUE_FULL => "vram upload queue full",
        counter::ROOM_MATERIAL_TEXTURE_DROPS => "room material texture drops",
        counter::PERSISTENT_ASSET_RESIDENT_BYTES => "persistent asset resident bytes",
        counter::PERSISTENT_ASSET_LOAD_FAILURES => "persistent asset load failures",
        counter::PERSISTENT_ASSET_FAILED_ID => "persistent asset failed id",
        counter::PERSISTENT_ASSET_FAILED_REASON => "persistent asset failed reason",
        counter::ROOM_MATERIAL_SLOT_OVERFLOW => "room material slot overflow",
        counter::GAME_ENTITIES_THOUGHT => "game entities thought",
        counter::GAME_ENTITY_PATROL_ENTERS => "game entity patrol enters",
        counter::GAME_ENTITY_AGGRO_ENTERS => "game entity aggro enters",
        counter::GAME_ENTITY_WINDUP_ENTERS => "game entity windup enters",
        counter::GAME_ENTITY_ATTACK_ENTERS => "game entity attack enters",
        counter::LOGIC_RECORDS_FIRED => "logic records fired",
        counter::GAME_ENTITY_STAGGER_ENTERS => "game entity stagger enters",
        counter::GAME_ENTITY_DEATHS => "game entity deaths",
        counter::PLAYER_MELEE_HITS => "player melee hits",
        counter::PLAYER_HITS_TAKEN => "player hits taken",
        counter::PLAYER_ATTACK_STARTS => "player attack starts",
        counter::PLAYER_DEATHS => "player deaths",
        counter::PLAYER_CHECKPOINT_ACTIVATIONS => "player checkpoint activations",
        counter::PLAYER_DUPLICATE_HIT_REJECTIONS => "player duplicate hit rejections",
        counter::LOGIC_DOOR_ACTIVATIONS => "logic door activations",
        counter::PLAYER_WEAPON_ATTACHMENTS => "player weapon attachments",
        counter::GAME_ENTITY_PVS_SUPPRESSIONS => "game entity pvs suppressions",
        counter::PLAYER_LIQUID_DAMAGE_EVENTS => "player liquid damage events",
        counter::PLAYER_FACING_YAW_Q12 => "player facing yaw q12",
        counter::PLAYER_RENDER_FORWARD_X_Q12_BIASED => "player render forward x q12 biased",
        counter::PLAYER_RENDER_FORWARD_Z_Q12_BIASED => "player render forward z q12 biased",
        counter::PLAYER_ANIM_ACTION => "player anim action",
        counter::ROOM_PLAYER_LOCAL_Y_BIASED => "player local y biased",
        _ => "unknown",
    }
}

/// Encode a guest event command word.
pub const fn encode_event(kind: u8, id: u16) -> u32 {
    ((kind as u32) << KIND_SHIFT) | (id as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_port_latches_value_then_event() {
        let mut telemetry = GuestTelemetry::new();
        assert!(telemetry.observe_write32(VALUE_PHYS, 42, 100));
        assert!(telemetry.observe_write32(
            EVENT_PHYS,
            encode_event(4, counter::WORLD_COMMANDS),
            110
        ));

        let events = telemetry.drain_events();
        assert_eq!(telemetry.frames_seen(), 0);
        assert_eq!(telemetry.counter_total(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.counter_max_value(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.counter_latest_value(counter::WORLD_COMMANDS), 42);
        assert_eq!(telemetry.observe_read32(CYCLE_PHYS, 1234), Some(1234));
        assert_eq!(
            events,
            [GuestTelemetryEvent {
                cycles: 110,
                kind: GuestTelemetryKind::Counter,
                id: counter::WORLD_COMMANDS,
                value: 42,
            }]
        );
    }

    #[test]
    fn telemetry_port_collects_debug_log_lines() {
        let mut telemetry = GuestTelemetry::new();
        assert!(GuestTelemetry::contains(LOG_PHYS));
        assert!(telemetry.observe_write32(EVENT_PHYS, encode_event(1, 0), 90));
        for (i, byte) in b"room cross 1->2\n".iter().copied().enumerate() {
            assert!(telemetry.observe_write32(LOG_PHYS, byte as u32, 100 + i as u64));
        }

        let logs = telemetry.drain_debug_logs();
        assert_eq!(
            logs,
            [GuestDebugLogLine {
                cycles: 100,
                frame: 1,
                text: "room cross 1->2".to_string(),
            }]
        );
        assert!(telemetry.drain_debug_logs().is_empty());
    }

    #[test]
    fn summary_accumulates_stage_spans_and_counters() {
        let events = [
            GuestTelemetryEvent {
                cycles: 10,
                kind: GuestTelemetryKind::FrameBegin,
                id: 0,
                value: 7,
            },
            GuestTelemetryEvent {
                cycles: 20,
                kind: GuestTelemetryKind::StageBegin,
                id: stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 70,
                kind: GuestTelemetryKind::StageEnd,
                id: stage::RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 72,
                kind: GuestTelemetryKind::TaskBegin,
                id: task::VISUAL_RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 120,
                kind: GuestTelemetryKind::TaskEnd,
                id: task::VISUAL_RENDER,
                value: 0,
            },
            GuestTelemetryEvent {
                cycles: 80,
                kind: GuestTelemetryKind::Counter,
                id: counter::TRI_PRIMITIVES,
                value: 12,
            },
            GuestTelemetryEvent {
                cycles: 90,
                kind: GuestTelemetryKind::Counter,
                id: counter::VISUAL_MAX_LATENESS_VBLANKS,
                value: 2,
            },
            GuestTelemetryEvent {
                cycles: 100,
                kind: GuestTelemetryKind::Counter,
                id: counter::VISUAL_MAX_LATENESS_VBLANKS,
                value: 1,
            },
        ];
        let summary = GuestTelemetrySummary::from_events(&events);
        assert_eq!(summary.frames, 1);
        assert_eq!(summary.stage_cycles[stage::RENDER as usize], 50);
        assert_eq!(summary.stage_hits[stage::RENDER as usize], 1);
        assert_eq!(summary.task_cycles[task::VISUAL_RENDER as usize], 48);
        assert_eq!(summary.task_hits[task::VISUAL_RENDER as usize], 1);
        assert_eq!(summary.task_max_cycles[task::VISUAL_RENDER as usize], 48);
        assert_eq!(summary.counters[counter::TRI_PRIMITIVES as usize], 12);
        assert_eq!(
            summary.counters[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            3
        );
        assert_eq!(
            summary.counter_max_values[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            2
        );
        assert_eq!(
            summary.counter_latest_values[counter::VISUAL_MAX_LATENESS_VBLANKS as usize],
            1
        );
    }

    #[test]
    fn every_named_telemetry_id_has_a_description() {
        // Host tooltips show `*_desc(id)`; a named id without one renders a
        // metric with no explanation. The descriptions come from the id
        // rustdoc in psx-telemetry, so this only fails if a name is added
        // for an id that does not exist there.
        for id in 0..STAGE_COUNT as u16 {
            if stage_name(id) != "unknown" {
                assert!(!stage_desc(id).trim().is_empty(), "stage {id} lacks a desc");
            }
        }
        for id in 0..TASK_COUNT as u16 {
            if task_name(id) != "unknown" {
                assert!(!task_desc(id).trim().is_empty(), "task {id} lacks a desc");
            }
        }
        for id in 0..COUNTER_COUNT as u16 {
            if counter_name(id) != "unknown" {
                assert!(
                    !counter_desc(id).trim().is_empty(),
                    "counter {id} lacks a desc"
                );
            }
        }
    }

    #[test]
    fn frame_pacing_counter_names_are_known() {
        assert_eq!(counter_name(counter::SIM_TICKS), "sim ticks");
        assert_eq!(counter_name(counter::VISUAL_FRAMES), "visual frames");
        assert_eq!(
            counter_name(counter::VISUAL_MAX_LATENESS_VBLANKS),
            "visual max lateness vblanks"
        );
        assert_eq!(stage_name(stage::CD_STREAM_BENCH), "cd stream bench");
        assert_eq!(stage_name(stage::CD_STREAM_STEADY), "cd stream steady");
        assert_eq!(stage_name(stage::CD_WORLD_PACK_STREAM), "cd world pack");
        assert_eq!(stage_name(stage::CD_ROOM_CHUNK_LOAD), "cd room chunk load");
        assert_eq!(stage_name(stage::ROOM_VISIBLE_LIST), "room visible list");
        assert_eq!(stage_name(stage::ROOM_CELL_SELECT), "room cell select");
        assert_eq!(stage_name(stage::ROOM_SURFACE_DRAW), "room surface draw");
        assert_eq!(stage_name(stage::SKY), "sky");
        assert_eq!(stage_name(stage::FAR_VISTA), "far vista");
        assert_eq!(stage_name(stage::IMAGE_PROPS), "image props");
        assert_eq!(stage_name(stage::PORTAL_VISIBILITY), "portal visibility");
        assert_eq!(task_name(task::FIXED_UPDATE), "fixed update");
        assert_eq!(task_name(task::VISUAL_RENDER), "visual render");
        assert_eq!(
            counter_name(counter::CD_STREAM_BENCH_STATUS),
            "cd stream status"
        );
        assert_eq!(
            counter_name(counter::CD_ROOM_CHUNK_BYTES),
            "cd room chunk bytes"
        );
        assert_eq!(
            counter_name(counter::ROOM_STREAM_RESIDENT_SLOTS),
            "room stream resident slots"
        );
        assert_eq!(
            counter_name(counter::ROOM_PROJECTED_VERTICES),
            "room projected verts"
        );
        assert_eq!(
            counter_name(counter::ROOM_SURF_SUBMIT_CYCLES),
            "room surf submit cyc"
        );
        assert_eq!(
            counter_name(counter::ROOM_SURF_BACKFACE_CULLED),
            "room surf backface culled"
        );
        assert_eq!(
            counter_name(counter::ROOM_SUBMIT_PACKET_FILL_CYCLES),
            "room submit packet cyc"
        );
    }
}
