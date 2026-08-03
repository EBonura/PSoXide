//! GPU -- minimum viable surface for BIOS init + VRAM display.
//!
//! **Phase 2h scope:** the GPU owns VRAM (migrated here from the
//! top-level `vram` module -- re-exported for compatibility), exposes
//! `GPUSTAT` reads with the ready-bit pattern the BIOS polls for, and
//! accepts GP0 / GP1 writes. No command processing, no rasterization,
//! no display output yet -- those arrive in follow-up milestones once
//! DMA actually ships command lists.
//!
//! Register map (single-cycle MMIO, 32-bit):
//! - `0x1F80_1810` GP0 write  / `GPUREAD` read
//! - `0x1F80_1814` GP1 write / `GPUSTAT` read
//!
//! ## Provenance
//!
//! Portions of this module are parity-matched against, and in places
//! derived from, PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later. Points of
//! correspondence are flagged inline with `Redux` references. PSoXide is
//! released under GPL-2.0-or-later in part to honor this lineage; see
//! `LICENSE` and `docs/license-audit.md`.

mod blend;
mod commands;
mod raster;
mod status;

pub use blend::BlendMode;
use blend::{
    blend_pixel, dither_rgb, modulate_tint, modulate_tint_dithered, prim_blend_mode,
    prim_is_semi_trans, rgb24_to_bgr15, split_tint, RAW_TEXTURE_TINT,
};
use raster::{for_each_tri_pixel, triangle_exceeds_hw_extent};
pub use raster::{tri_plane_eval, tri_raster_setup, tri_span_x, TriRasterSetup};
use status::GpuStatus;

use crate::vram::{Vram, VRAM_HEIGHT, VRAM_WIDTH};
use commands::gp0_packet_size;

/// Physical address of the GP0 / GPUREAD port.
pub const GP0_ADDR: u32 = 0x1F80_1810;
/// Physical address of the GP1 / GPUSTAT port.
pub const GP1_ADDR: u32 = 0x1F80_1814;

/// GP1 status latches become CPU-visible after the first immediately
/// following GPUSTAT read but before the second. The captured SCPH-9902
/// transition lands twelve CPU clock cycles after the command write.
pub(crate) const GP1_STATUS_LATCH_CYCLES: u64 = 12;
const GPU_DMA_DIRECTION_LATCH_CYCLES: u64 = 12;
const GPU_DMA_DIRECTION_FAST_LATCH_CYCLES: u64 = 5;

/// `#[serde(default = ...)]` target for the `[u32; 256]`-shaped
/// diagnostic opcode histograms below -- `Default` only covers arrays
/// up to length 32 on stable Rust, so a skipped 256-entry histogram
/// needs an explicit zeroed literal instead.
fn default_u32_256() -> [u32; 256] {
    [0; 256]
}

fn default_u64_256() -> [u64; 256] {
    [0; 256]
}

/// GPU state.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gpu {
    /// Video memory -- 1 MiB, 1024×512 at 16 bpp. The VRAM viewer in
    /// the frontend decodes this each frame.
    pub vram: Vram,
    status: GpuStatus,
    /// Packet assembler for GP0 commands that span multiple words.
    /// Holds words from the start of the current command; once the
    /// full packet has arrived, [`Gpu::execute_gp0_packet`] dispatches
    /// on the opcode and clears the buffer.
    gp0_fifo: Vec<u32>,
    /// Number of words the current packet expects in total (including
    /// the first/opcode word). `0` means "no packet in progress".
    gp0_expected: usize,
    /// Total GP0 writes the GPU has received since reset -- diagnostic
    /// for `examples/smoke_draw` and the frontend HUD, tells us whether
    /// software has actually started shipping commands. Excluded from
    /// save states.
    #[serde(skip)]
    gp0_write_count: u64,
    /// X offset (signed 11-bit) added to every primitive vertex --
    /// set by GP0 0xE5. Usually zero on BIOS boot, non-zero once the
    /// kernel sets up a display-list origin.
    draw_offset_x: i32,
    /// Y offset (signed 11-bit) added to every primitive vertex.
    draw_offset_y: i32,
    /// Drawing area clipping rectangle. All primitive pixels outside
    /// `[left..=right] × [top..=bottom]` are discarded. Set by
    /// GP0 0xE3 (top-left) and 0xE4 (bottom-right).
    draw_area_left: u16,
    draw_area_top: u16,
    draw_area_right: u16,
    draw_area_bottom: u16,
    /// Active CPU→VRAM transfer state. `Some` when GP0 0xA0 has set
    /// up a destination rect and is now expecting pixel-data words.
    vram_upload: Option<VramTransfer>,
    /// Active VRAM→CPU transfer state. `Some` when GP0 0xC0 set up a
    /// source rect and is now supplying pixel-data words via GPUREAD.
    vram_download: Option<VramTransfer>,
    /// Most-recent single-word response to a GP1 0x10 (Get GPU Info)
    /// request -- returned on the next GPUREAD while a VRAM download
    /// isn't active. Matches hardware's "GPU info" latch.
    gpuread_latch: u32,
    /// Raw environment command payloads returned by Redux for GP1
    /// query sub-ops 2..=5. The decoded fields above drive rendering;
    /// these preserve the CPU-visible readback contract.
    texture_window_raw: u32,
    drawing_start_raw: u32,
    drawing_end_raw: u32,
    drawing_offset_raw: u32,

    // --- Texture-page state (GP0 0xE1 draw mode) ---
    /// VRAM X base of the current texture page (pixels, 0..=960,
    /// multiples of 64).
    tex_page_x: u16,
    /// VRAM Y base of the current texture page (0 or 256).
    tex_page_y: u16,
    /// GP1(09h) bit 0 gates the second texture-page Y address bit carried by
    /// GP0(E1h)/polygon tpage bit 11. Retail consoles only populate the lower
    /// 1 MiB, so an enabled upper-bank selection reads as absent VRAM.
    /// Older documentation called this the "allow texture disable" latch.
    vram_2mb_addressing_enabled: bool,
    /// Texture colour depth: 0 = 4bpp (CLUT), 1 = 8bpp (CLUT),
    /// 2 = 15bpp (direct).
    tex_depth: u8,
    /// The PS1 GPU caches the active CLUT and reloads it from VRAM only when
    /// the CLUT *register* (the clut word in a textured primitive) changes --
    /// NOT when the CLUT data in VRAM is overwritten. A game that recolours by
    /// re-uploading the *same* CLUT (e.g. PICO-8 `pal()`) keeps sampling the
    /// stale cache until the clut word actually changes. `clut_cache_reg` is
    /// `u32::MAX` while invalid, forcing a reload on the next textured draw.
    #[serde(with = "crate::serde_big_array::array")]
    clut_cache: [u16; 256],
    clut_cache_reg: u32,
    clut_cache_8bit: bool,
    /// Semi-transparency mode from the current texpage (bits 5-6 of
    /// GP0 0xE1 or of a textured-primitive's tpage override). Kept
    /// as a [`BlendMode`] so primitives can plug it straight into
    /// the rasterizer without re-parsing the bits. Only matters when
    /// a primitive's cmd-bit-1 selects semi-transparent; opaque
    /// prims ignore this field.
    tex_blend_mode: BlendMode,
    /// Texture-window mask X in pixels (multiple of 8). Set by
    /// GP0 0xE2 bits 0..=4, left-shifted by 3. Used at UV-lookup
    /// time to force the high bits of the effective U to a constant
    /// pattern -- typically 0 (no mask) so UV passes through, but
    /// games that tile a texture via this feature set specific bits.
    tex_window_mask_x: u8,
    /// Texture-window mask Y, same shape.
    tex_window_mask_y: u8,
    /// Texture-window offset X (OR'd into the masked-out U bits).
    tex_window_offset_x: u8,
    /// Texture-window offset Y.
    tex_window_offset_y: u8,
    /// When true, every plotted pixel gets its mask bit (VRAM bit 15)
    /// forced to 1. Set by GP0 0xE6 bit 0. Mirrors GPUSTAT bit 11.
    /// Used by games that want to protect pixels from being
    /// overwritten by later primitives (combined with
    /// `mask_check_before_draw`).
    mask_set_on_draw: bool,
    /// When true, the rasterizer skips plotting over existing pixels
    /// whose mask bit (bit 15) is already set. Set by GP0 0xE6 bit
    /// 1. Mirrors GPUSTAT bit 12. Games commonly pair this with
    ///
    /// `mask_set_on_draw`: first pass sets the mask on important
    /// sprites (HUD, transparent edges), later prims can't stomp
    /// them. Without this check, HUDs flicker under overlapping
    /// backgrounds.
    mask_check_before_draw: bool,
    /// When true, apply the PSX 4×4 Bayer dither matrix to every
    /// 24-bit → 15-bit channel reduction. Set by GP0 0xE1 bit 9.
    /// Active on Gouraud-shaded primitives + textured primitives
    /// with tint modulation; flat colour + raw-texture prims are
    /// unaffected (their source is already 15-bit).
    ///
    /// Games rarely enable this -- it's a conservative choice that
    /// trades crispness for banding reduction. When it's on, the
    /// 24-bit shaded intermediate gets a per-pixel offset in the
    /// range -4..=+3 added before the `>> 3` truncation, producing
    /// ordered dither patterns instead of visible 5-bit staircases.
    dither_enabled: bool,

    /// Horizontal-flip flag for textured rectangles (GP0 0xE1 bit 12).
    /// When true, texture U coordinates are mirrored across the
    /// rectangle's midline. Common for sprite animations that reuse
    /// a single source image with direction-dependent facing.
    tex_rect_flip_x: bool,
    /// Vertical-flip flag for textured rectangles (GP0 0xE1 bit 13).
    tex_rect_flip_y: bool,

    /// Active polyline receive state. `None` when no polyline is in
    /// flight; `Some(...)` between a polyline start packet and its
    /// terminator word. While `Some`, every GP0 write is
    /// interpreted as polyline continuation data, bypassing the
    /// regular packet assembler.
    polyline: Option<PolylineState>,
    /// `cmd_log` index of the in-flight polyline's start packet, so
    /// continuation words (vertices / colours, not the terminator)
    /// append to that entry's fifo -- same scheme as
    /// `VramTransfer::cmd_log_index` for upload payloads. `None`
    /// when no polyline is in flight or the log isn't armed. The
    /// HW renderer replays lines from the cmd_log; without this the
    /// log only carries the first segment of every polyline.
    polyline_cmd_log_index: Option<usize>,

    /// Wireframe mode -- replaces filled triangles with their
    /// three edges, rendered as lines at the triangle's primary
    /// colour. Rectangles and already-line primitives are
    /// unchanged. Off by default; toggled from the frontend's
    /// debug toolbar for visualising the geometry a game is
    /// submitting.
    pub wireframe_enabled: bool,
    /// Wireframe edge-pixel journal: every pixel plotted by line
    /// rasterization while wireframe is on, for the render in flight.
    /// Aged two generations and then erased to black (see
    /// [`Gpu::wireframe_frame_boundary`]), so stale edges never
    /// accumulate -- without clearing any pixel the game itself drew
    /// (poly interiors stay transparent).
    wire_pixels_current: Vec<(u16, u16)>,
    /// One render old. NOT erased yet: in a double-buffered game these
    /// pixels sit on the currently-displayed buffer; erasing them at the
    /// vblank IRQ blanks the screen for the window between our erase and
    /// the game's own display flip (the IRQ handler flips after us).
    wire_pixels_prev: Vec<(u16, u16)>,
    /// Two renders old -- these pixels are on the back buffer the game
    /// is about to redraw, never on the displayed one. Safe to erase.
    wire_pixels_prev2: Vec<(u16, u16)>,
    /// `(pre-edge value, value the edge wrote)` for every pixel
    /// currently owned by an edge, keyed by `(y << 16) | x`. Erasing
    /// restores the pre-edge value instead of writing black -- games
    /// that never repaint their background (some commercial menus draw
    /// the car over a static backdrop) would otherwise accumulate a
    /// black scar wherever an edge ever was. The restore only applies
    /// while the pixel still holds what the edge wrote: if the game
    /// overdrew our edge (leaderboard screens redraw their text rows),
    /// restoring would smear stale content over the newer frame.
    wire_saved: std::collections::HashMap<u32, (u16, u16)>,
    /// Previous-vblank value of `wireframe_enabled`, for detecting the
    /// on/off transitions in [`Gpu::wireframe_frame_boundary`].
    wireframe_was_on: bool,
    /// Full-VRAM snapshot taken at the wireframe on-transition, before
    /// the framebuffer rects are cleared. Used at the off-transition to
    /// put back what the clear removed (see `wire_cleared_rects`).
    wire_toggle_snapshot: Option<Vec<u16>>,
    /// Framebuffer rects cleared at the on-transition (draw area +
    /// display area, inclusive corners). Toggling wireframe on clears
    /// them once: polygons no longer overwrite the framebuffer, so the
    /// last textured frame would otherwise sit frozen behind the edges
    /// for the rest of the session (racing and fighting scenes looked
    /// like wires over a stale photo of the scene). Live 2D content
    /// (sprites, HUD, MDEC) still draws on top of the black canvas.
    wire_cleared_rects: Vec<(u16, u16, u16, u16)>,

    /// Pixel-owner trace -- when `Some`, every `plot_pixel` records
    /// the index of the currently-executing GPU command into
    /// `pixel_owner[y*VRAM_WIDTH + x]`. Paired with `cmd_log`
    /// this lets us answer "which command drew the pixel at
    /// (x, y)?" after a run, the essential first step in
    /// diagnosing per-pixel parity divergences against Redux.
    ///
    /// Allocating is opt-in because the buffer is 2 MiB -- tiny
    /// in absolute terms but enough to want control over when it
    /// appears in core state. Debug tracer -- excluded from save
    /// states (re-enabled explicitly by whichever probe wants it).
    #[serde(skip)]
    pub pixel_owner: Option<Vec<u32>>,
    /// Command log -- one entry per GP0 packet executed since this
    /// tracer was enabled. Each entry captures the opcode plus
    /// the raw fifo words the packet consumed, so we can replay
    /// the exact inputs to a single draw in isolation. Debug tracer
    /// -- excluded from save states.
    #[serde(skip)]
    pub cmd_log: Vec<GpuCmdLogEntry>,
    /// Master gate for `cmd_log` pushes. Set by both
    /// `enable_pixel_tracer` and the lighter `enable_cmd_log`.
    /// Decoupled from `pixel_owner.is_some()` so the HW renderer can
    /// capture the GP0 stream without paying for the 2 MiB owner Vec
    /// or its per-pixel stamping cost. Tied to the (excluded) tracer
    /// state above -- excluded from save states, defaults back to off.
    #[serde(skip)]
    cmd_log_enabled: bool,
    /// The index that will be written into `pixel_owner` for the
    /// NEXT pixel plotted -- i.e., the index of the currently-
    /// executing command. Bumped just before each packet dispatch.
    /// Tracer bookkeeping -- excluded from save states.
    #[serde(skip)]
    current_cmd_index: u32,

    /// GPU execution backlog in CPU/bus cycles. Raster and VRAM
    /// commands add silicon-calibrated work; elapsed bus cycles drain
    /// it. While non-zero GPUSTAT's command/DMA-ready bits are clear,
    /// so command submission is paced by the real GPU throughput.
    busy_credit: u64,
    /// Virtual GP0 FIFO occupancy for the overflow diagnostic: CPU
    /// stores that arrived while the GPU was busy, reset whenever a
    /// store finds it idle. The stall model paces every write, so
    /// in-model nothing is ever lost; on silicon nocash documents a
    /// 16-word FIFO that LOSES data on overflow (the demo-disc fail
    /// panel's garbage rectangles are consistent with that). Until the
    /// hardware-tests suite settles stall-vs-drop on a real console,
    /// this counts the writes that WOULD be lost under the drop model.
    /// Diagnostic bookkeeping -- excluded from save states.
    #[serde(skip)]
    gp0_burst_words: u32,
    /// Total CPU GP0 words beyond the virtual FIFO depth. Surfaced in
    /// the frontend's exit diagnostics; nonzero means the guest bursts
    /// unpaced GP0 writes and may desync on hardware.
    #[serde(skip)]
    gp0_overflow_count: u64,
    /// `PSOXIDE_STRICT_GP0_FIFO=1`: experimentally DROP overflowing
    /// words (and skip their stall) the way nocash describes silicon,
    /// so headless runs reproduce the garbage a real console shows.
    /// Cached at construction; excluded from save states.
    #[serde(skip)]
    gp0_fifo_strict: bool,
    /// Remaining queue prefix through the most recently accepted
    /// DMA-sourced command. CPU rendering leaves GPUSTAT.28 ready;
    /// DMA rendering clears it until this prefix drains.
    dma_busy_credit: u64,

    // --- Display area (GP1 0x05 / 0x06 / 0x07 / 0x08) ---
    /// VRAM X of the top-left pixel of the displayed framebuffer.
    display_start_x: u16,
    /// VRAM Y of the top-left pixel of the displayed framebuffer.
    display_start_y: u16,
    /// Horizontal display resolution from GP1 0x08 (pixels). One of
    /// 256, 320, 368, 512, 640.
    display_width: u16,
    /// Vertical resolution flag from GP1(08h) bit 2 -- `true` means
    /// 480-line interlaced (each V-range line doubles). The actual
    /// displayed row count is computed from the V-range (Y1..Y2)
    /// and this flag in [`Gpu::effective_display_height`].
    display_height_480: bool,
    /// V-range Y1 from GP1(07h) bits 0..=9 -- top scanline of the
    /// visible window in the video output. Default ~16.
    v_range_y1: u16,
    /// V-range Y2 from GP1(07h) bits 10..=19 -- bottom scanline of
    /// the visible window. Default ~256, giving 240 visible rows.
    v_range_y2: u16,
    /// H-range X1 from GP1(06h) bits 0..=11 -- left-edge GPU clock
    /// of the visible window. Stored for display-area reporting but
    /// not (yet) used to derive the width.
    h_range_x1: u16,
    /// H-range X2 from GP1(06h) bits 12..=23 -- right-edge GPU clock.
    h_range_x2: u16,
    /// 24bpp colour depth flag from GP1 0x08 bit 4. For now we always
    /// decode VRAM as 15bpp; when this flag comes into play the
    /// frontend's framebuffer view can respect it.
    display_24bpp: bool,
    /// `true` after the BIOS / game has written GP1 0x07 (V-range) or
    /// GP1 0x08 (display mode). Before that, `display_area` reports
    /// (0, 0) -- matching Redux's `takeScreenShot`, which also hands
    /// back a zero-sized image until its internal
    /// `updateDisplayIfChanged` runs (triggered by those same two
    /// GP1 writes). Parity tools rely on this to avoid seeing a
    /// spurious "dimension mismatch" before the first configured
    /// frame even exists.
    display_configured: bool,

    /// Count of executed GP0 packets by opcode byte (the high 8 bits
    /// of the header word). Diagnostic only -- lets `smoke_draw` see at
    /// a glance which primitive types the BIOS is issuing. Excluded
    /// from save states.
    #[serde(skip, default = "default_u32_256")]
    gp0_opcode_hist: [u32; 256],
    /// Accumulated silicon-timing cost by GP0 opcode. This reuses the exact
    /// cost already charged to `busy_credit`, so diagnostics can attribute
    /// GPU work without performing another raster-area estimate.
    #[serde(skip, default = "default_u64_256")]
    gp0_timing_hist: [u64; 256],
    /// Subset of `gp0_timing_hist` submitted through DMA channel 2.
    #[serde(skip, default = "default_u64_256")]
    gp0_dma_timing_hist: [u64; 256],
    /// Count of GP1 writes by opcode byte. Same diagnostic role as
    /// gp0_opcode_hist but for the display / control port. Excluded
    /// from save states.
    #[serde(skip, default = "default_u32_256")]
    gp1_opcode_hist: [u32; 256],
    /// Distinct (x, y) pairs written to GP1 0x05 (display-start). Lets
    /// diagnostics see whether the BIOS is flipping buffers or just
    /// repeatedly re-writing the same location. Excluded from save
    /// states.
    #[serde(skip)]
    display_start_history: std::collections::BTreeSet<(u16, u16)>,
    /// Distinct raw GP1 0x08 display-mode values seen since reset.
    /// Excluded from save states.
    #[serde(skip)]
    display_mode_history: std::collections::BTreeSet<u32>,
    /// Recent GP1 writes in chronological order. Diagnostic only; capped
    /// so long FMV probes do not grow without bound. Excluded from
    /// save states.
    #[serde(skip)]
    gp1_write_history: Vec<u32>,
    /// Latched when GP0(1Fh) requests IRQ1. The bus consumes this and
    /// mirrors it into I_STAT bit 1.
    irq_requested: bool,
    /// Latched when GP1(00h/02h) acknowledges IRQ1. The bus consumes
    /// this and clears I_STAT bit 1.
    irq_acknowledged: bool,
    /// Posted GP1 commands whose GPUSTAT-visible effect has not reached the
    /// status latch yet. Display geometry is updated at write time; only the
    /// status bits and IRQ acknowledge are delayed.
    #[serde(default)]
    pending_gp1_status: std::collections::VecDeque<(u64, u32)>,
    /// A GP1 acknowledge that cancels a not-yet-visible IRQ command leaves
    /// the input path primed; the next IRQ command reaches the status latch
    /// without the usual posted-read delay.
    #[serde(default)]
    irq_after_canceled_set_is_immediate: bool,
    /// GP1(04h) DMA-direction writes share a small control-port latch. A
    /// direction-3 write primes the even-direction path until the next real
    /// GPU DMA transfer consumes it. This is visible on silicon as a
    /// one-GPUSTAT-read difference between otherwise identical direction
    /// sweeps.
    #[serde(default)]
    dma_direction_seen_three: bool,
    /// Once a primed direction latch has been consumed by GPU DMA, subsequent
    /// direction writes take the full posted-control delay until reset.
    #[serde(default)]
    dma_direction_full_delay: bool,
}

/// Public snapshot of the GPU's display configuration, read by the
/// frontend's framebuffer panel. Updated by the GP1 0x05 (display
/// start) and GP1 0x08 (display mode) handlers.
#[derive(Debug, Clone, Copy)]
pub struct DisplayArea {
    /// VRAM X of the top-left displayed pixel.
    pub x: u16,
    /// VRAM Y of the top-left displayed pixel.
    pub y: u16,
    /// Horizontal resolution in pixels (one of 256/320/368/384/512/640).
    pub width: u16,
    /// Vertical resolution in pixels (240 or 480 interlaced).
    pub height: u16,
    /// `true` when the GP1 0x08 colour-depth bit selected 24bpp mode.
    /// The frontend framebuffer panel still decodes VRAM as 15bpp;
    /// respecting this flag is a future refinement.
    pub bpp24: bool,
}

/// One captured GP0 packet in the pixel-tracer's command log.
/// `index` matches the value stored in [`Gpu::pixel_owner`] for every
/// pixel this packet plotted, so `pixel_owner[y*W+x]` → look up the
/// corresponding `cmd_log` entry to see what primitive drew that
/// pixel.
#[derive(Debug, Clone)]
pub struct GpuCmdLogEntry {
    /// Monotonic command index, starting at 0. Wraps via saturation
    /// at u32::MAX; not a concern for typical debug runs (a few
    /// hundred thousand draw calls at most).
    pub index: u32,
    /// Opcode byte -- top 8 bits of the first FIFO word.
    pub opcode: u8,
    /// Full FIFO contents at dispatch time. Draw packets are short
    /// slices (3..=12 words). CPU→VRAM uploads append their payload
    /// words after the 3-word setup so downstream renderers can mirror
    /// direct image transfers such as FMV frames.
    pub fifo: Vec<u32>,
}

/// Expected total word count for a GP0 command starting with `opcode`.
///
/// Host-side OT adapters use this to split one DMA packet containing
/// multiple GP0 commands into the same command-log shape the emulator
/// records while executing the FIFO normally.
pub fn gp0_command_word_count(opcode: u8) -> usize {
    commands::gp0_packet_size(opcode)
}

/// In-flight CPU→VRAM transfer state -- 2 pixels per incoming GP0 word,
/// written in row-major order across the destination rect. Completes
/// when `remaining == 0`, and then the GPU goes back to accepting
/// command packets on GP0.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct VramTransfer {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    /// Row of the next pixel to write (0 = top of the rect).
    row: u16,
    /// Column of the next pixel to write (0 = left of the rect).
    col: u16,
    /// Words still expected (= ceil(w*h / 2)).
    remaining: u32,
    /// Command-log entry for the 0xA0 setup packet. While the upload
    /// payload streams in, append the words there so render backends
    /// can replay direct VRAM image writes.
    cmd_log_index: Option<usize>,
}

impl Gpu {
    /// Construct a fresh GPU -- VRAM zeroed, status at the soft-GPU
    /// always-ready pattern the BIOS expects.
    pub fn new() -> Self {
        Self {
            vram: Vram::new(),
            status: GpuStatus::new(),
            gp0_fifo: Vec::with_capacity(12),
            gp0_expected: 0,
            gp0_write_count: 0,
            draw_offset_x: 0,
            draw_offset_y: 0,
            draw_area_left: 0,
            draw_area_top: 0,
            draw_area_right: VRAM_WIDTH as u16 - 1,
            draw_area_bottom: VRAM_HEIGHT as u16 - 1,
            vram_upload: None,
            tex_page_x: 0,
            tex_page_y: 0,
            vram_2mb_addressing_enabled: false,
            tex_depth: 0,
            clut_cache: [0; 256],
            clut_cache_reg: u32::MAX,
            clut_cache_8bit: false,
            tex_blend_mode: BlendMode::Average,
            tex_window_mask_x: 0,
            tex_window_mask_y: 0,
            tex_window_offset_x: 0,
            tex_window_offset_y: 0,
            mask_set_on_draw: false,
            mask_check_before_draw: false,
            dither_enabled: false,
            tex_rect_flip_x: false,
            tex_rect_flip_y: false,
            polyline: None,
            polyline_cmd_log_index: None,
            wireframe_enabled: false,
            wire_pixels_current: Vec::new(),
            wire_pixels_prev: Vec::new(),
            wire_pixels_prev2: Vec::new(),
            wire_saved: std::collections::HashMap::new(),
            wireframe_was_on: false,
            wire_toggle_snapshot: None,
            wire_cleared_rects: Vec::new(),
            pixel_owner: None,
            cmd_log: Vec::new(),
            cmd_log_enabled: false,
            current_cmd_index: 0,
            busy_credit: 0,
            dma_busy_credit: 0,
            gp0_burst_words: 0,
            gp0_overflow_count: 0,
            gp0_fifo_strict: std::env::var_os("PSOXIDE_STRICT_GP0_FIFO").is_some(),
            display_start_x: 0,
            display_start_y: 0,
            display_width: 320,
            display_height_480: false,
            // Power-on V- and H-range defaults, matching Redux's
            // `SoftGPU::impl::initBackend` which zeroes `Range.x0 =
            // Range.x1 = Range.y0 = Range.y1 = 0`. Crucially the
            // BIOS writes GP1 0x08 (display mode) *before* GP1 0x07
            // (v-range), and because Redux derives Height from
            // `y1 - y0` -- both zero -- its `takeScreenShot` height
            // is 0 during that window. Earlier we defaulted these
            // to 0x10/0x100, which made our screenshot height 240
            // during the same window and broke lockstep parity at
            // step 19.3 M on Crash's BIOS handoff.
            v_range_y1: 0,
            v_range_y2: 0,
            h_range_x1: 0,
            h_range_x2: 0,
            display_24bpp: false,
            display_configured: false,
            vram_download: None,
            gpuread_latch: 0,
            texture_window_raw: 0,
            drawing_start_raw: 0,
            drawing_end_raw: 0,
            drawing_offset_raw: 0,
            gp0_opcode_hist: [0; 256],
            gp0_timing_hist: [0; 256],
            gp0_dma_timing_hist: [0; 256],
            gp1_opcode_hist: [0; 256],
            display_start_history: std::collections::BTreeSet::new(),
            display_mode_history: std::collections::BTreeSet::new(),
            gp1_write_history: Vec::new(),
            irq_requested: false,
            irq_acknowledged: false,
            pending_gp1_status: std::collections::VecDeque::new(),
            irq_after_canceled_set_is_immediate: false,
            dma_direction_seen_three: false,
            dma_direction_full_delay: false,
        }
    }

    /// Consume the GP1(04h) control-port history when channel 2 starts a real
    /// transfer. OTC (channel 6) deliberately does not touch this state.
    pub(crate) fn note_dma_transfer_started(&mut self) {
        if self.dma_direction_seen_three {
            self.dma_direction_full_delay = true;
        }
    }

    /// Distinct display-start corners the BIOS has written to. Useful
    /// for telling a re-write loop from a front/back-buffer flip.
    pub fn display_start_history(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.display_start_history.iter().copied()
    }

    /// Distinct raw GP1 0x08 display-mode values seen since reset.
    pub fn display_mode_history(&self) -> impl Iterator<Item = u32> + '_ {
        self.display_mode_history.iter().copied()
    }

    /// Recent raw GP1 writes in chronological order. Diagnostic.
    pub fn gp1_write_history(&self) -> &[u32] {
        &self.gp1_write_history
    }

    /// Consume a pending GPU IRQ request generated by GP0(1Fh).
    pub fn take_irq_requested(&mut self) -> bool {
        core::mem::take(&mut self.irq_requested)
    }

    /// Consume a pending GPU IRQ acknowledge generated by GP1(00h/02h).
    pub fn take_irq_acknowledged(&mut self) -> bool {
        core::mem::take(&mut self.irq_acknowledged)
    }

    /// Snapshot of the GP0 opcode histogram -- per-byte count of
    /// executed packets keyed by high-byte of word 0. Diagnostic.
    pub fn gp0_opcode_histogram(&self) -> [u32; 256] {
        self.gp0_opcode_hist
    }

    /// Snapshot of accumulated GPU execution cycles grouped by GP0 opcode.
    pub fn gp0_timing_histogram(&self) -> [u64; 256] {
        self.gp0_timing_hist
    }

    /// Snapshot of accumulated GPU DMA cycles grouped by GP0 opcode.
    pub fn gp0_dma_timing_histogram(&self) -> [u64; 256] {
        self.gp0_dma_timing_hist
    }

    /// Snapshot of the GP1 opcode histogram. Diagnostic.
    pub fn gp1_opcode_histogram(&self) -> [u32; 256] {
        self.gp1_opcode_hist
    }

    /// Snapshot of the currently-configured display area, for the
    /// frontend's framebuffer panel. Cheap to call each frame. The
    /// `height` is derived from the V-range + 480-mode flag (see
    /// [`Gpu::effective_display_height`]) so it matches what Redux's
    /// screenshot path reports -- letting milestone parity tests
    /// compare byte-for-byte.
    pub fn display_area(&self) -> DisplayArea {
        // Live register view, even before GP1 0x07/0x08 have been written
        // after a reset: silicon scans out the (persisting) ranges the
        // moment GP1 0x03 re-enables the display, which is how the demo
        // disc's chain-loader screen is visible on a console. This used to
        // return 0x0 until reconfiguration (Redux's `takeScreenShot`
        // semantics), which hid that whole phase from --dump-hw and the
        // GUI panel; the Redux-parity gating now lives in
        // [`Gpu::display_hash`], the only consumer that needs it.
        DisplayArea {
            x: self.display_start_x,
            y: self.display_start_y,
            width: self.display_width,
            height: self.effective_display_height(),
            bpp24: self.display_24bpp,
        }
    }

    /// FNV-1a-64 over the visible display area's 15bpp pixel bytes,
    /// for Redux-parity comparisons. Rows are packed tightly (no
    /// stride padding) so a given (width, height, bpp) maps to a
    /// specific byte sequence -- identical to what Redux's
    /// `PCSX.GPU.takeScreenShot()` produces server-side on the
    /// oracle path.
    ///
    /// Returns `(hash, width, height, byte_len)`. If the display
    /// area extends past VRAM the rows are clipped at the VRAM
    /// edge and the row count is reduced -- matching Redux's
    /// behaviour.
    pub fn display_hash(&self) -> (u64, u32, u32, usize) {
        if !self.display_configured {
            // Match Redux's `takeScreenShot`: zero-sized image until
            // GP1 0x07 or 0x08 has been written after a reset, so the
            // milestone parity hashes compare apples to apples from the
            // very first instruction onward.
            return (psx_hw::hash::Fnv1a64::new().finish(), 0, 0, 0);
        }
        let da = self.display_area();
        let mut h = psx_hw::hash::Fnv1a64::new();
        let mut byte_len = 0usize;
        let vram_w = crate::VRAM_WIDTH as u16;
        let vram_h = crate::VRAM_HEIGHT as u16;
        let effective_h = da.height.min(vram_h.saturating_sub(da.y));
        let effective_w = da.width.min(vram_w.saturating_sub(da.x));
        if da.bpp24 {
            // 24-bit mode: each pixel is 3 bytes packed in VRAM. A row
            // of W 24-bit pixels occupies W*3 bytes = 1.5 * W 16-bit
            // words. We read per-byte to span the straddles.
            for dy in 0..effective_h {
                for dx in 0..effective_w {
                    let (r, g, b) = self.read_pixel_rgb24(da.x + dx, da.y + dy);
                    h.update(&[r, g, b]);
                    byte_len += 3;
                }
            }
        } else {
            for dy in 0..effective_h {
                for dx in 0..effective_w {
                    let pixel = self.vram.get_pixel(da.x + dx, da.y + dy);
                    h.update(&pixel.to_le_bytes());
                    byte_len += 2;
                }
            }
        }
        (h.finish(), effective_w as u32, effective_h as u32, byte_len)
    }

    /// Enable per-pixel command tracing. Allocates the 2 MiB owner
    /// buffer (one u32 per VRAM pixel). Every subsequent
    /// `plot_pixel` stamps the currently-executing command's index
    /// into the buffer; every subsequent `execute_gp0_packet`
    /// pushes a `GpuCmdLogEntry` into `cmd_log`.
    ///
    /// Idempotent: re-enabling resets the tracer to empty.
    pub fn enable_pixel_tracer(&mut self) {
        const SENTINEL_NO_OWNER: u32 = u32::MAX;
        self.pixel_owner = Some(vec![SENTINEL_NO_OWNER; VRAM_WIDTH * VRAM_HEIGHT]);
        self.cmd_log.clear();
        self.current_cmd_index = 0;
        self.cmd_log_enabled = true;
        self.unpin_in_flight_cmd_log();
    }

    /// Enable cmd_log capture WITHOUT allocating the per-pixel owner
    /// buffer. The HW (wgpu render-pipeline) renderer needs the GP0
    /// packet stream to drive its draw calls each frame, but doesn't
    /// need pixel-level provenance -- saves the 2 MiB owner Vec and
    /// the per-`plot_pixel` stamp cost. The bench probes that DO
    /// want owner tracking still call `enable_pixel_tracer`.
    ///
    /// Idempotent: re-enabling clears cmd_log.
    pub fn enable_cmd_log(&mut self) {
        self.cmd_log.clear();
        self.current_cmd_index = 0;
        self.cmd_log_enabled = true;
        self.unpin_in_flight_cmd_log();
    }

    /// Enabling (or re-enabling) capture clears `cmd_log`, so an index
    /// pinned by an in-flight VRAM upload or polyline would dangle into
    /// the fresh log and misattribute the remaining continuation words.
    /// Unpin them: a transfer whose setup packet predates the log simply
    /// is not attributed, which is what an armed-mid-stream log means.
    fn unpin_in_flight_cmd_log(&mut self) {
        if let Some(upload) = self.vram_upload.as_mut() {
            upload.cmd_log_index = None;
        }
        self.polyline_cmd_log_index = None;
    }

    /// Whether `cmd_log` capture is currently armed (either via
    /// `enable_cmd_log` or `enable_pixel_tracer`). Lets the
    /// frontend call `enable_cmd_log` at most once per Bus
    /// lifetime instead of clobbering the log every frame.
    pub fn cmd_log_enabled(&self) -> bool {
        self.cmd_log_enabled
    }

    /// Drain only complete command-log entries.
    ///
    /// CPU→VRAM uploads are one GP0 setup packet followed by many
    /// payload writes, and polylines are one start packet followed by
    /// streamed vertex/colour words. The setup entry owns the
    /// follow-on words in `cmd_log`; if a frontend drains the log
    /// mid-upload / mid-polyline, the remaining words would otherwise
    /// append to an entry that has already been moved out. Keep that
    /// in-flight entry in place and return only the commands before
    /// it. (Both modes consume every GP0 word until they end, so at
    /// most one of the two indices is pending at a time.)
    pub fn drain_completed_cmd_log(&mut self) -> Vec<GpuCmdLogEntry> {
        let upload_index = self.vram_upload.and_then(|t| t.cmd_log_index);
        let pending_index = match (upload_index, self.polyline_cmd_log_index) {
            (Some(u), Some(p)) => Some(u.min(p)),
            (u, p) => u.or(p),
        };
        let Some(pending_index) = pending_index else {
            return std::mem::take(&mut self.cmd_log);
        };
        if pending_index >= self.cmd_log.len() {
            return std::mem::take(&mut self.cmd_log);
        }

        let pending = self.cmd_log.split_off(pending_index);
        let drained = std::mem::replace(&mut self.cmd_log, pending);
        if let Some(upload) = self.vram_upload.as_mut() {
            if upload.cmd_log_index.is_some() {
                upload.cmd_log_index = Some(0);
            }
        }
        if self.polyline_cmd_log_index.is_some() {
            self.polyline_cmd_log_index = Some(0);
        }
        if let Some(entry) = self.cmd_log.first_mut() {
            entry.index = 0;
        }
        drained
    }

    /// Look up which command drew the pixel at (x, y), returning
    /// `None` if no command has touched that pixel since the tracer
    /// was enabled (or if the tracer is off). The returned entry
    /// carries the opcode + raw FIFO words, enough to replay the
    /// single command in isolation.
    pub fn pixel_owner_at(&self, x: u16, y: u16) -> Option<&GpuCmdLogEntry> {
        let pixel_owner = self.pixel_owner.as_ref()?;
        let idx = pixel_owner
            .get(y as usize * VRAM_WIDTH + x as usize)
            .copied()?;
        if idx == u32::MAX {
            return None;
        }
        self.cmd_log.get(idx as usize)
    }

    /// Read one 24-bit display pixel. VRAM bytes are packed: pixel
    /// N lives at byte offsets `3*N..3*N+2` within a row, and each
    /// row is 2048 bytes (1024 × 16-bit). The three bytes may
    /// straddle two VRAM halfwords -- we read them individually.
    fn read_pixel_rgb24(&self, x: u16, y: u16) -> (u8, u8, u8) {
        let byte_x = (x as u32) * 3;
        let word_x = (byte_x / 2) as u16;
        let even = byte_x & 1 == 0;
        let w0 = self.vram.get_pixel(word_x, y);
        let w1 = self.vram.get_pixel(word_x.wrapping_add(1), y);
        if even {
            let r = (w0 & 0xFF) as u8;
            let g = (w0 >> 8) as u8;
            let b = (w1 & 0xFF) as u8;
            (r, g, b)
        } else {
            let r = (w0 >> 8) as u8;
            let g = (w1 & 0xFF) as u8;
            let b = (w1 >> 8) as u8;
            (r, g, b)
        }
    }

    /// Horizontal presentation offset, in displayed pixels, requested by
    /// GP1(06h) relative to the standard centred window. Real hardware slides
    /// the active picture by this much within the video signal (the classic
    /// CRT screen-position adjustment); [`Gpu::display_rgba8`] reproduces it so
    /// the setting is visible even though we otherwise crop to the active
    /// region. Content that leaves the H-range at the standard value -- the
    /// vast majority -- gets `0`.
    pub fn horizontal_display_offset_px(&self) -> i32 {
        // Standard centred H-range start (GP1 06h X1) the SDK and most content
        // use; the offset is measured relative to it. GPU clocks per pixel come
        // from the active dot clock, so 320-wide content uses 8 (matching the
        // SDK's `set_screen_h_offset`).
        const H_DISPLAY_START_STANDARD: i32 = 0x260;
        let divisor = self.dot_clock_divisor().max(1) as i32;
        (self.h_range_x1 as i32 - H_DISPLAY_START_STANDARD) / divisor
    }

    /// Vertical presentation offset, in displayed scanlines, requested by
    /// GP1(07h) relative to the standard centred window. This mirrors the
    /// screen-position preview logic in [`Self::horizontal_display_offset_px`].
    pub fn vertical_display_offset_px(&self) -> i32 {
        let standard = if self.effective_display_height() > 240 {
            0x23
        } else {
            0x10
        };
        self.v_range_y1 as i32 - standard
    }

    /// Produce a row-major `RGBA8` buffer of the current display area.
    /// In 16-bit mode the 5-bit channels are bit-replicated to 8-bit;
    /// in 24-bit mode the packed RGB888 triplets are used directly.
    /// Alpha is always 0xFF. Size = `width * height * 4` bytes.
    ///
    /// Used by the frontend to upload a display texture -- a single
    /// format regardless of the PS1's current bpp, so the wgpu
    /// path doesn't need to branch.
    pub fn display_rgba8(&self) -> (Vec<u8>, u32, u32) {
        let da = self.display_area();
        let vram_w = crate::VRAM_WIDTH as u16;
        let vram_h = crate::VRAM_HEIGHT as u16;
        let eff_h = da.height.min(vram_h.saturating_sub(da.y));
        let eff_w = da.width.min(vram_w.saturating_sub(da.x));
        // GP1(06h)/(07h) screen positioning: slide the picture within the
        // output by the requested offset and black-fill the exposed edge, so a
        // screen-position setting is visible. Real hardware shifts the active
        // window inside the video signal; cropping to the active region would
        // otherwise discard the shift. Standard-centred content -> offset 0.
        let off_x = self
            .horizontal_display_offset_px()
            .clamp(-(eff_w as i32), eff_w as i32);
        let off_y = self
            .vertical_display_offset_px()
            .clamp(-(eff_h as i32), eff_h as i32);
        let mut out = Vec::with_capacity((eff_w as usize) * (eff_h as usize) * 4);
        for dy in 0..eff_h {
            let src_y = dy as i32 - off_y;
            for dx in 0..eff_w {
                let src_x = dx as i32 - off_x;
                if src_x < 0 || src_x >= eff_w as i32 || src_y < 0 || src_y >= eff_h as i32 {
                    out.extend_from_slice(&[0, 0, 0, 0xFF]);
                    continue;
                }
                let sx = da.x + src_x as u16;
                let sy = da.y + src_y as u16;
                if da.bpp24 {
                    let (r, g, b) = self.read_pixel_rgb24(sx, sy);
                    out.extend_from_slice(&[r, g, b, 0xFF]);
                } else {
                    let pixel = self.vram.get_pixel(sx, sy);
                    let r = ((pixel & 0x1F) as u8) << 3;
                    let g = (((pixel >> 5) & 0x1F) as u8) << 3;
                    let b = (((pixel >> 10) & 0x1F) as u8) << 3;
                    // Replicate high 3 bits into low 3 for fuller range.
                    out.extend_from_slice(&[r | (r >> 5), g | (g >> 5), b | (b >> 5), 0xFF]);
                }
            }
        }
        (out, eff_w as u32, eff_h as u32)
    }

    /// Total GP0 writes received since reset. Diagnostic counter.
    pub fn gp0_write_count(&self) -> u64 {
        self.gp0_write_count
    }

    /// Dispatch an MMIO read inside the GPU window. Returns `Some` for
    /// the two valid ports; `None` means the caller should fall through
    /// to a different region.
    ///
    /// `&mut self` because the GP0 (GPUREAD) port drains an in-flight
    /// VRAM→CPU transfer one word at a time; the GP1 (GPUSTAT) port
    /// stays side-effect-free.
    pub fn read32(&mut self, phys: u32) -> Option<u32> {
        self.read32_at(phys, u64::MAX)
    }

    /// MMIO read with CPU-cycle context so posted GP1 status writes can
    /// mature between two consecutive GPUSTAT reads.
    pub fn read32_at(&mut self, phys: u32, now: u64) -> Option<u32> {
        self.apply_pending_gp1_status(now);
        match phys {
            GP0_ADDR => Some(self.read_gpuread()),
            GP1_ADDR => {
                let mut value = self.status.read(
                    self.vram_download.is_some(),
                    !self.is_busy(),
                    !self.is_dma_busy(),
                );
                // IRQ1 first deasserts the two ready signals. The DMA-request
                // output is a separate latch and retains its pre-command
                // value until the IRQ/status event lands (C202 -> D702 on the
                // measured direction-2 path, rather than C002 -> D702).
                if self
                    .pending_gp1_status
                    .iter()
                    .any(|(_, command)| command >> 24 == 0x1F)
                {
                    value |= 1 << 25;
                }
                Some(value)
            }
            _ => None,
        }
    }

    /// GPUREAD -- two paths:
    /// - If a VRAM→CPU transfer is active, return the next 2 packed
    ///   16bpp pixels from the source rect.
    /// - Otherwise return the latch written by the last GP1 0x10.
    fn read_gpuread(&mut self) -> u32 {
        if self.vram_download.is_some() {
            self.download_next_word()
        } else {
            self.gpuread_latch
        }
    }

    /// Toggle GPUSTAT bit 31 (interlace / even-odd line flag). Called
    /// once per VBlank by `Bus::run_vblank_scheduler`. BIOS-side code
    /// often polls this bit to tell that a new frame has started,
    /// independent of the VBlank IRQ.
    pub fn toggle_vblank_field(&mut self) {
        self.status.toggle_field();
        // Piggy-back the wireframe journal rotation on the vblank pulse --
        // it's the GPU's only per-frame hook. No-op unless wireframe drew
        // something recently.
        self.wireframe_frame_boundary();
    }

    /// Erase two-render-old wireframe edges and rotate the journal.
    ///
    /// Three rules, each earned by a live repro:
    /// - Rotate only when the game drew NEW wireframe geometry since the
    ///   last rotation. Games render slower than vblank (30/15 fps, or a
    ///   menu that draws once); rotating on a bare vblank ages the edges
    ///   out and the wireframe blanks until the next render.
    /// - Erase the two-renders-old generation, not last render's. Last
    ///   render's pixels live on the buffer being DISPLAYED right now
    ///   (the game's IRQ handler flips the display after this vblank
    ///   callback); erasing them blanked the screen for the window
    ///   between our erase and the game's flip (Crash intro: black every
    ///   other frame). The two-old generation is on the back buffer the
    ///   game is about to redraw, never the displayed one.
    /// - Skip pixels still present in a newer generation: static scenes
    ///   redraw the same edges in place; erasing a live pixel flickers.
    ///
    /// When wireframe is switched off, all journals are erased so no edge
    /// pixels linger in guest VRAM.
    fn wireframe_frame_boundary(&mut self) {
        let was_on = self.wireframe_was_on;
        self.wireframe_was_on = self.wireframe_enabled;
        if self.wireframe_enabled && !was_on {
            // Toggle just turned on: black out the framebuffer pages once.
            // Polygons stop overwriting the framebuffer in wireframe mode,
            // so the last textured frame would sit frozen behind the edges
            // for the rest of the session (racing and fighting scenes looked
            // like wires over a stale photo). Live 2D content still draws
            // on top of the black canvas. Snapshot first so toggling off
            // can put back what this clear removed.
            self.wire_toggle_snapshot = Some(self.vram.words().to_vec());
            self.wire_cleared_rects.clear();
            self.wire_cleared_rects.push((
                self.draw_area_left,
                self.draw_area_top,
                self.draw_area_right,
                self.draw_area_bottom,
            ));
            let d = self.display_area();
            if d.width > 0 && d.height > 0 {
                // Display width is in pixels; 24bpp packs 2 pixels into
                // 3 VRAM words.
                let w_vram = if d.bpp24 {
                    d.width as u32 * 3 / 2
                } else {
                    d.width as u32
                };
                let r = (d.x as u32 + w_vram - 1).min(VRAM_WIDTH as u32 - 1) as u16;
                let b = (d.y as u32 + d.height as u32 - 1).min(VRAM_HEIGHT as u32 - 1) as u16;
                self.wire_cleared_rects.push((d.x, d.y, r, b));
            }
            for &(l, t, r, b) in &self.wire_cleared_rects {
                self.vram.fill_rect_unwrapped(l, t, r, b, 0);
            }
        }
        if !self.wireframe_enabled {
            if !was_on {
                return;
            }
            // Toggle just turned off: put every edge-owned pixel back to
            // its pre-edge content (only where our edge still stands),
            // then undo the on-transition clear for pixels still black --
            // content the game drew during wireframe stays.
            for (key, (old, written)) in std::mem::take(&mut self.wire_saved) {
                let (x, y) = ((key & 0xFFFF) as u16, (key >> 16) as u16);
                if self.vram.get_pixel(x, y) == written {
                    self.vram.set_pixel(x, y, old);
                }
            }
            self.wire_pixels_prev2 = Vec::new();
            self.wire_pixels_prev = Vec::new();
            self.wire_pixels_current = Vec::new();
            if let Some(snap) = self.wire_toggle_snapshot.take() {
                for &(l, t, r, b) in &self.wire_cleared_rects {
                    for y in t..=b {
                        for x in l..=r {
                            if self.vram.get_pixel(x, y) == 0 {
                                let old = snap[y as usize * VRAM_WIDTH + x as usize];
                                self.vram.set_pixel(x, y, old);
                            }
                        }
                    }
                }
            }
            self.wire_cleared_rects.clear();
            return;
        }
        if self.wire_pixels_current.is_empty() {
            return;
        }
        let key = |&(x, y): &(u16, u16)| ((y as u32) << 16) | x as u32;
        let live: std::collections::HashSet<u32> = self
            .wire_pixels_current
            .iter()
            .chain(&self.wire_pixels_prev)
            .map(key)
            .collect();
        let prev2 = std::mem::take(&mut self.wire_pixels_prev2);
        for p in prev2 {
            let k = key(&p);
            if !live.contains(&k) {
                // Restore the pre-edge content (not black -- see
                // `wire_saved`), but only while the pixel still holds
                // our edge; if the game overdrew it, its newer content
                // stands. Removal ends our ownership either way, so a
                // future edge re-saves whatever is there by then.
                if let Some((old, written)) = self.wire_saved.remove(&k) {
                    if self.vram.get_pixel(p.0, p.1) == written {
                        self.vram.set_pixel(p.0, p.1, old);
                    }
                }
            }
        }
        self.wire_pixels_prev2 = std::mem::take(&mut self.wire_pixels_prev);
        self.wire_pixels_prev = std::mem::take(&mut self.wire_pixels_current);
    }

    /// Add CPU/bus-cycle work to the GPU execution backlog.
    pub fn charge_busy(&mut self, cost: u64) {
        self.busy_credit = self.busy_credit.saturating_add(cost);
    }

    /// Add DMA-fed work and retain the queue prefix through it. Later CPU
    /// commands can extend total busy time without extending DMA busy time.
    fn charge_dma_busy(&mut self, cost: u64) {
        self.busy_credit = self.busy_credit.saturating_add(cost);
        self.dma_busy_credit = self.busy_credit;
    }

    /// Drain busy credit over time. Called by the bus each tick
    /// so the busy flag settles back to "ready" as cycles advance.
    /// One elapsed CPU/bus cycle decays one unit of credit.
    pub fn decay_busy(&mut self, cycles: u64) {
        self.busy_credit = self.busy_credit.saturating_sub(cycles);
        self.dma_busy_credit = self.dma_busy_credit.saturating_sub(cycles);
    }

    /// Is the GPU currently "busy"? Used to gate GPUSTAT ready
    /// bits 26 + 28.
    pub fn is_busy(&self) -> bool {
        if Self::gpu_wedged() {
            return true;
        }
        self.busy_credit > 0
    }

    /// `PSOXIDE_WEDGE_GPU=1` holds the GPUSTAT ready bits low forever,
    /// reproducing a GPU left waiting for the rest of a command (what an
    /// aborted mid-packet linked-list walk does on silicon). Guest code
    /// that spins on `wait_cmd_ready` / `draw_sync` without a bound then
    /// hangs headless instead of only on a console.
    fn gpu_wedged() -> bool {
        static WEDGED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *WEDGED.get_or_init(|| std::env::var_os("PSOXIDE_WEDGE_GPU").is_some())
    }

    /// CPU stall applied when an architectural store targets GP0 while the
    /// preceding command still occupies the input/execution path. Real MMIO
    /// writes wait in hardware even though GPUSTAT.28 remains asserted for
    /// CPU-sourced rendering; this is distinct from software polling bits.
    pub(crate) fn cpu_gp0_write_stall(&self) -> u32 {
        self.busy_credit.min(u64::from(u32::MAX)) as u32
    }

    /// The real GP0 FIFO is this many words deep.
    pub const GP0_FIFO_DEPTH: u32 = 16;

    /// Record one architectural (CPU) GP0 store arriving, BEFORE its
    /// stall is applied. Returns `false` when the word should be dropped
    /// (strict mode only): the guest burst more than a FIFO's worth of
    /// words at a busy GPU, which nocash documents as data loss on
    /// silicon. The default (non-strict) model keeps the calibrated
    /// stall pacing and only counts the would-be losses, so existing
    /// goldens are unaffected while unpaced guests still fail visibly
    /// in the exit diagnostics. Conservative approximation: occupancy
    /// resets whenever a store finds the GPU idle, and per-packet
    /// drains inside a continuous busy burst are not modelled.
    pub(crate) fn note_cpu_gp0_arrival(&mut self) -> bool {
        if !self.is_busy() {
            self.gp0_burst_words = 0;
            return true;
        }
        self.gp0_burst_words = self.gp0_burst_words.saturating_add(1);
        if self.gp0_burst_words <= Self::GP0_FIFO_DEPTH {
            return true;
        }
        if self.gp0_overflow_count == 0 && std::env::var_os("PSOXIDE_TRACE_GP0_OVERFLOW").is_some()
        {
            eprintln!(
                "[gpu] GP0 burst exceeded the {}-word FIFO while busy; \
                 silicon may lose these words (further overflows counted \
                 silently)",
                Self::GP0_FIFO_DEPTH
            );
        }
        self.gp0_overflow_count += 1;
        !self.gp0_fifo_strict
    }

    /// CPU GP0 words that arrived beyond the virtual FIFO depth while
    /// the GPU was busy. Nonzero means the guest bursts unpaced GP0
    /// writes and may desync on real hardware.
    pub fn gp0_overflow_count(&self) -> u64 {
        self.gp0_overflow_count
    }

    fn is_dma_busy(&self) -> bool {
        self.dma_busy_credit > 0
    }

    /// Estimate the command's GPU execution time in CPU/bus cycles.
    ///
    /// The coefficients come from the build-158 `gpu/bandwidth` silicon
    /// capture. They are deliberately expressed as small rational numbers
    /// rather than wall-clock milliseconds: Timer 1 observes HBlank while
    /// the CPU polls GPUSTAT, so a cycle-domain backlog reproduces both the
    /// benchmark and ordinary game synchronization.
    fn gp0_packet_timing_cost(&self, op: u8) -> u64 {
        let flat_cost = |pixels: u64, semi: bool| {
            if semi {
                scale_gpu_pixels(pixels, 51, 64)
            } else {
                scale_gpu_pixels(pixels, 135, 256)
            }
        };
        let flat_poly_cost = |pixels: u64, semi: bool| {
            if semi {
                scale_gpu_pixels(pixels, 51, 64)
            } else {
                scale_gpu_pixels(pixels, 137, 256)
            }
        };
        let textured_rect_cost = |pixels: u64| scale_gpu_pixels(pixels, 135, 128);
        let textured_poly_cost = |pixels: u64| scale_gpu_pixels(pixels, 179, 64);

        match op {
            // GP0(1Fh) traverses the command input path before IRQ1 and the
            // ready flags reach GPUSTAT. On SCPH-9902 the first immediately
            // following read has bits 26/28 clear and the second has them
            // set, the same edge measured for the IRQ status latch itself.
            0x1F => GP1_STATUS_LATCH_CYCLES,
            // GP0(02h) has a dedicated fast fill engine. Silicon moves a
            // little over twelve pixels per CPU cycle for this path.
            0x02 => {
                let size = self.gp0_fifo[2];
                let w = ((size & 0x3FF) + 0x0F) & !0x0F;
                let h = (size >> 16) & 0x1FF;
                scale_gpu_pixels(u64::from(w) * u64::from(h), 41, 512)
            }
            // VRAM-to-VRAM uses the slower internal read/modify/write path.
            0x80..=0x9F => {
                let size = self.gp0_fifo[3];
                let raw_w = size & 0x3FF;
                let raw_h = (size >> 16) & 0x1FF;
                let w = if raw_w == 0 { 1024 } else { raw_w };
                let h = if raw_h == 0 { 512 } else { raw_h };
                scale_gpu_pixels(u64::from(w) * u64::from(h), 171, 128)
            }
            // Flat polygons.
            0x20..=0x23 => {
                let v = [
                    self.decode_vertex(self.gp0_fifo[1]),
                    self.decode_vertex(self.gp0_fifo[2]),
                    self.decode_vertex(self.gp0_fifo[3]),
                ];
                flat_poly_cost(
                    self.timing_polygon_pixels(&v),
                    prim_is_semi_trans(self.gp0_fifo[0]),
                )
            }
            0x28..=0x2B => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[2]);
                let v2 = self.decode_vertex(self.gp0_fifo[3]);
                let v3 = self.decode_vertex(self.gp0_fifo[4]);
                let pixels = self.timing_polygon_pixels(&[v0, v1, v2])
                    + self.timing_polygon_pixels(&[v1, v3, v2]);
                flat_poly_cost(pixels, prim_is_semi_trans(self.gp0_fifo[0]))
            }
            // Gouraud polygons use the same write-side throughput as flat
            // polygons in the current silicon corpus; interpolation setup is
            // small compared with a large primitive.
            0x30..=0x33 => {
                let v = [
                    self.decode_vertex(self.gp0_fifo[1]),
                    self.decode_vertex(self.gp0_fifo[3]),
                    self.decode_vertex(self.gp0_fifo[5]),
                ];
                flat_poly_cost(
                    self.timing_polygon_pixels(&v),
                    prim_is_semi_trans(self.gp0_fifo[0]),
                )
            }
            0x38..=0x3B => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[3]);
                let v2 = self.decode_vertex(self.gp0_fifo[5]);
                let v3 = self.decode_vertex(self.gp0_fifo[7]);
                let pixels = self.timing_polygon_pixels(&[v0, v1, v2])
                    + self.timing_polygon_pixels(&[v1, v3, v2]);
                flat_poly_cost(pixels, prim_is_semi_trans(self.gp0_fifo[0]))
            }
            // Textured flat and Gouraud polygons. Texture fetch and UV
            // interpolation dominate, and the silicon benchmark shows the
            // same throughput with its semi-transparency flag set.
            0x24..=0x27 => {
                let v = [
                    self.decode_vertex(self.gp0_fifo[1]),
                    self.decode_vertex(self.gp0_fifo[3]),
                    self.decode_vertex(self.gp0_fifo[5]),
                ];
                textured_poly_cost(self.timing_polygon_pixels(&v))
            }
            0x2C..=0x2F => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[3]);
                let v2 = self.decode_vertex(self.gp0_fifo[5]);
                let v3 = self.decode_vertex(self.gp0_fifo[7]);
                let pixels = self.timing_polygon_pixels(&[v0, v1, v2])
                    + self.timing_polygon_pixels(&[v1, v3, v2]);
                textured_poly_cost(pixels)
            }
            0x34..=0x37 => {
                let v = [
                    self.decode_vertex(self.gp0_fifo[1]),
                    self.decode_vertex(self.gp0_fifo[4]),
                    self.decode_vertex(self.gp0_fifo[7]),
                ];
                textured_poly_cost(self.timing_polygon_pixels(&v))
            }
            0x3C..=0x3F => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[4]);
                let v2 = self.decode_vertex(self.gp0_fifo[7]);
                let v3 = self.decode_vertex(self.gp0_fifo[10]);
                let pixels = self.timing_polygon_pixels(&[v0, v1, v2])
                    + self.timing_polygon_pixels(&[v1, v3, v2]);
                textured_poly_cost(pixels)
            }
            // Line setup/throughput from the PAL short/long batch pairs.
            // Monochrome is about 0.90 clocks/pixel plus six setup clocks;
            // Gouraud interpolation raises that to 1.03 plus thirteen.
            0x40..=0x4F => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[2]);
                7 + scale_gpu_pixels(timing_line_steps(v0, v1), 29, 32)
            }
            0x50..=0x5F => {
                let v0 = self.decode_vertex(self.gp0_fifo[1]);
                let v1 = self.decode_vertex(self.gp0_fifo[3]);
                16 + scale_gpu_pixels(timing_line_steps(v0, v1), 263, 256)
            }
            // Flat rectangles.
            0x60..=0x63 => {
                let size = self.gp0_fifo[2];
                flat_cost(
                    self.timing_rect_pixels(
                        self.gp0_fifo[1],
                        (size & 0xFFFF) as i32,
                        (size >> 16) as i32,
                    ),
                    prim_is_semi_trans(self.gp0_fifo[0]),
                )
            }
            0x68..=0x6B => flat_cost(
                self.timing_rect_pixels(self.gp0_fifo[1], 1, 1),
                prim_is_semi_trans(self.gp0_fifo[0]),
            ),
            0x70..=0x73 => flat_cost(
                self.timing_rect_pixels(self.gp0_fifo[1], 8, 8),
                prim_is_semi_trans(self.gp0_fifo[0]),
            ),
            0x78..=0x7B => flat_cost(
                self.timing_rect_pixels(self.gp0_fifo[1], 16, 16),
                prim_is_semi_trans(self.gp0_fifo[0]),
            ),
            // Textured rectangles. Like textured polygons, the benchmark's
            // semi flag does not change throughput when sampled texels are
            // opaque, so the texture path owns the coefficient.
            0x64..=0x67 => {
                let size = self.gp0_fifo[3];
                textured_rect_cost(self.timing_rect_pixels(
                    self.gp0_fifo[1],
                    (size & 0xFFFF) as i32,
                    (size >> 16) as i32,
                ))
            }
            0x6C..=0x6F => textured_rect_cost(self.timing_rect_pixels(self.gp0_fifo[1], 1, 1)),
            0x74..=0x77 => textured_rect_cost(self.timing_rect_pixels(self.gp0_fifo[1], 8, 8)),
            0x7C..=0x7F => textured_rect_cost(self.timing_rect_pixels(self.gp0_fifo[1], 16, 16)),
            _ => 0,
        }
    }

    fn timing_rect_pixels(&self, pos: u32, w: i32, h: i32) -> u64 {
        if w <= 0 || h <= 0 {
            return 0;
        }
        let x = sign_extend_11((pos & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((pos >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        let left = x.max(self.draw_area_left as i32).max(0);
        let top = y.max(self.draw_area_top as i32).max(0);
        let right = (x + w)
            .min(self.draw_area_right as i32 + 1)
            .min(VRAM_WIDTH as i32);
        let bottom = (y + h)
            .min(self.draw_area_bottom as i32 + 1)
            .min(VRAM_HEIGHT as i32);
        if left >= right || top >= bottom {
            0
        } else {
            (right - left) as u64 * (bottom - top) as u64
        }
    }

    fn timing_polygon_pixels(&self, vertices: &[(i32, i32)]) -> u64 {
        clipped_polygon_area(
            vertices,
            self.draw_area_left as i32,
            self.draw_area_top as i32,
            self.draw_area_right as i32 + 1,
            self.draw_area_bottom as i32 + 1,
        )
    }

    /// Dispatch an MMIO write inside the GPU window. Returns `true` if
    /// the address belonged to the GPU.
    pub fn write32(&mut self, phys: u32, value: u32) -> bool {
        self.write32_inner(phys, value, None)
    }

    /// MMIO write with CPU-cycle context. GP1(02h) IRQ acknowledge and
    /// GP1(04h) DMA-direction changes are posted to GPUSTAT; direct host-side
    /// callers retain the immediate legacy behaviour through [`Gpu::write32`].
    pub fn write32_at(&mut self, phys: u32, value: u32, now: u64) -> bool {
        self.write32_inner(phys, value, Some(now))
    }

    fn write32_inner(&mut self, phys: u32, value: u32, now: Option<u64>) -> bool {
        if phys == GP1_ADDR && (value >> 24) == 0x04 {
            if let Some(now) = now {
                // Posted control writes continue through the latch even when
                // software follows them with another write instead of a
                // status read. Apply anything that matured before accepting
                // the new command so the queue reflects real elapsed time.
                self.apply_pending_gp1_status(now);
            }
        }
        match phys {
            GP0_ADDR => {
                let fast_irq = value >> 24 == 0x1f && self.irq_after_canceled_set_is_immediate;
                if fast_irq {
                    self.irq_after_canceled_set_is_immediate = false;
                }
                let irq_deadline = now
                    .filter(|_| value >> 24 == 0x1f && !fast_irq)
                    .map(|now| now.saturating_add(GP1_STATUS_LATCH_CYCLES));
                let old_irq_bit = self.status.raw & (1 << 24);
                let old_irq_requested = self.irq_requested;
                self.gp0_write(value, false);
                if let Some(deadline) = irq_deadline {
                    self.status.raw = (self.status.raw & !(1 << 24)) | old_irq_bit;
                    self.irq_requested = old_irq_requested;
                    self.pending_gp1_status.push_back((deadline, 0x1f00_0000));
                }
                true
            }
            GP1_ADDR => {
                let op = ((value >> 24) & 0xFF) as usize;
                self.gp1_opcode_hist[op] = self.gp1_opcode_hist[op].saturating_add(1);
                if self.gp1_write_history.len() == 512 {
                    self.gp1_write_history.remove(0);
                }
                self.gp1_write_history.push(value);
                self.apply_gp1_display(value);
                if let Some(now) = now.filter(|_| matches!(op, 0x02 | 0x04)) {
                    if op == 0x02 && self.status.raw & (1 << 24) == 0 {
                        // An acknowledge that reaches the control port before
                        // a queued GP0(1Fh) has asserted IRQ1 cancels that
                        // request; it must not surface after the clear.
                        let before = self.pending_gp1_status.len();
                        self.pending_gp1_status
                            .retain(|(_, command)| command >> 24 != 0x1F);
                        self.irq_after_canceled_set_is_immediate |=
                            self.pending_gp1_status.len() != before;
                    }
                    let delay = if op == 0x04 {
                        let direction = value & 0x3;
                        let fast = !self.dma_direction_full_delay
                            && (direction == 2
                                || (direction == 0 && self.dma_direction_seen_three));
                        if direction == 3 {
                            self.dma_direction_seen_three = true;
                        }
                        if fast {
                            GPU_DMA_DIRECTION_FAST_LATCH_CYCLES
                        } else {
                            GPU_DMA_DIRECTION_LATCH_CYCLES
                        }
                    } else {
                        GP1_STATUS_LATCH_CYCLES
                    };
                    let deadline = now.saturating_add(delay);
                    self.pending_gp1_status.push_back((deadline, value));
                } else {
                    self.apply_gp1_status(value);
                }
                true
            }
            _ => false,
        }
    }

    fn apply_gp1_status(&mut self, value: u32) {
        if value >> 24 == 0x1f {
            self.status.raw |= 1 << 24;
            self.irq_requested = true;
            return;
        }
        if matches!((value >> 24) & 0xFF, 0x00 | 0x02) {
            self.irq_acknowledged = true;
        }
        self.status.gp1_write(value);
    }

    fn apply_pending_gp1_status(&mut self, now: u64) {
        while self
            .pending_gp1_status
            .front()
            .is_some_and(|(deadline, _)| *deadline <= now)
        {
            let (_, value) = self
                .pending_gp1_status
                .pop_front()
                .expect("front was present");
            self.apply_gp1_status(value);
        }
    }

    /// GP0 0xC0 -- VRAM→CPU transfer. `[cmd, xy, wh]` header; pixel
    /// words are then drained by GPUREAD. Two 16bpp pixels per word
    /// in row-major order across the source rect.
    fn begin_vram_download(&mut self) {
        let xy = self.gp0_fifo[1];
        let wh = self.gp0_fifo[2];
        let x = (xy & 0x3FF) as u16;
        let y = ((xy >> 16) & 0x1FF) as u16;
        let w = {
            let raw = (wh & 0x3FF) as u16;
            if raw == 0 {
                1024
            } else {
                raw
            }
        };
        let h = {
            let raw = ((wh >> 16) & 0x1FF) as u16;
            if raw == 0 {
                512
            } else {
                raw
            }
        };
        let pixels = w as u32 * h as u32;
        let remaining = pixels.div_ceil(2);
        self.vram_download = Some(VramTransfer {
            x,
            y,
            w,
            h,
            row: 0,
            col: 0,
            remaining,
            cmd_log_index: None,
        });
    }

    /// Pop two pixels from the active VRAM→CPU transfer, packed into
    /// a u32 (low 16 = first pixel, high 16 = second). When the
    /// transfer completes, the download slot clears and subsequent
    /// GPUREAD reads return the GP1 0x10 latch.
    fn download_next_word(&mut self) -> u32 {
        let Some(t) = self.vram_download.as_mut() else {
            return self.gpuread_latch;
        };
        let pix_a = Self::read_download_pixel(t, &self.vram);
        let pix_b = Self::read_download_pixel(t, &self.vram);
        t.remaining = t.remaining.saturating_sub(1);
        let word = (pix_a as u32) | ((pix_b as u32) << 16);
        if t.remaining == 0 {
            self.vram_download = None;
        }
        word
    }

    /// Fetch the next pixel from the source rect for a VRAM→CPU
    /// download. Advances row/col; over-draws past the final row
    /// return zero (paired-halving at odd widths).
    fn read_download_pixel(t: &mut VramTransfer, vram: &Vram) -> u16 {
        if t.row >= t.h {
            return 0;
        }
        let px = t.x.wrapping_add(t.col);
        let py = t.y.wrapping_add(t.row);
        let texel = vram.get_pixel(px, py);
        t.col += 1;
        if t.col >= t.w {
            t.col = 0;
            t.row += 1;
        }
        texel
    }

    /// Handle GP1 commands that update the display-area state
    /// (0x05 / 0x06 / 0x07 / 0x08) or the GPU-info latch (0x10).
    /// The status-bit updates stay in `GpuStatus::gp1_write`; this
    /// function captures the geometry + latch the frontend + CPU need.
    fn apply_gp1_display(&mut self, value: u32) {
        let cmd = (value >> 24) & 0xFF;
        match cmd {
            // GP1 0x10 -- Get GPU Info. Sub-op selects what latches
            // into GPUREAD. See nocash PSX-SPX "GPU Memory Transfer
            // Commands / GP1(10h)". Common sub-ops:
            //   0x02 -- texture window (E2 readback)
            //   0x03 -- draw area top-left  (E3 readback)
            //   0x04 -- draw area bottom-right (E4)
            //   0x05 -- draw offset (E5)
            // Redux masks the query to three bits and leaves the
            // latch untouched for 0, 1, 6 and 7.
            0x10 => match value & 0x07 {
                0x02 => self.gpuread_latch = self.texture_window_raw,
                0x03 => self.gpuread_latch = self.drawing_start_raw,
                0x04 => self.gpuread_latch = self.drawing_end_raw,
                0x05 => self.gpuread_latch = self.drawing_offset_raw,
                _ => {}
            },
            // GP1 0x00 -- GPU reset. Matches Redux's `CtrlReset`:
            // clears the display-enable flag + RGB24/interlace bits
            // and resets DrawOffset, but **does not** touch the
            // V/H-ranges or DisplayPosition. The BIOS writes those
            // via the explicit GP1 0x05 / 0x06 / 0x07 commands
            // later, so reset-persisting them matches hardware.
            0x00 => {
                self.reset_command_buffer();
                self.busy_credit = 0;
                self.dma_busy_credit = 0;
                self.tex_page_x = 0;
                self.tex_page_y = 0;
                self.tex_depth = 0;
                self.tex_blend_mode = BlendMode::Average;
                self.dither_enabled = false;
                self.tex_rect_flip_x = false;
                self.tex_rect_flip_y = false;
                self.vram_2mb_addressing_enabled = false;
                self.display_start_x = 0;
                self.display_start_y = 0;
                self.display_width = 320;
                self.display_height_480 = false;
                self.display_24bpp = false;
                self.display_configured = false;
                // Mask flags reset per PSX-SPX (GP1 0x00 clears both).
                self.mask_set_on_draw = false;
                self.mask_check_before_draw = false;
                // GP1(00) drops the GPU's CLUT cache; force a reload on the
                // next textured draw (PSX-SPX reset behaviour).
                self.clut_cache_reg = u32::MAX;
                self.status.raw &= !0x1800;
                self.texture_window_raw = 0;
                // GP1(00) is defined by the PSX-SPX spec as equivalent to
                // GP0(E1h..E6h)=0, so the active (decoded) texture window
                // must be cleared too -- not just the E2 readback latch.
                // Otherwise a draw issued after reset but before the next
                // GP0(E2) would mask UVs with the stale window.
                self.tex_window_mask_x = 0;
                self.tex_window_mask_y = 0;
                self.tex_window_offset_x = 0;
                self.tex_window_offset_y = 0;
                self.drawing_start_raw = 0;
                self.drawing_end_raw = 0;
                self.drawing_offset_raw = 0;
                // The DECODED clip rectangle and draw offset too, not just
                // their readback latches: GP1(00h) = GP0(E1h..E6h) = 0, so
                // on silicon a draw issued before the next E3/E4 clips to
                // the single pixel at the origin. Keeping the previous
                // (usually wide-open) area let FrameBuffer-less guests
                // render headless while painting nothing on hardware,
                // which is exactly what the demo-disc IRQ probe did on a
                // real console until it set its area explicitly.
                self.draw_area_left = 0;
                self.draw_area_top = 0;
                self.draw_area_right = 0;
                self.draw_area_bottom = 0;
                self.draw_offset_x = 0;
                self.draw_offset_y = 0;
                self.gpuread_latch = 0x400;
            }
            // GP1 0x01 -- Reset command buffer. This aborts a partial GP0
            // packet, polyline, or image transfer without resetting display
            // state. DMA/chopping conformance tests rely on it after a block
            // shape transfers fewer pixels than the A0h rectangle requested.
            0x01 => self.reset_command_buffer(),
            // GP1 0x05 -- display area start (top-left corner in VRAM).
            //   bits 9:0  = X (pixels)
            //   bits 18:10 = Y (pixels)
            0x05 => {
                self.display_start_x = (value & 0x3FF) as u16;
                self.display_start_y = ((value >> 10) & 0x1FF) as u16;
                self.display_start_history
                    .insert((self.display_start_x, self.display_start_y));
            }
            // GP1 0x06 -- Horizontal display range (on screen, in GPU
            // clocks -- not pixels). Used for centering the active
            // display inside the video signal; doesn't change the
            // VRAM read window's width. Stored for completeness.
            0x06 => {
                self.h_range_x1 = (value & 0xFFF) as u16;
                self.h_range_x2 = ((value >> 12) & 0xFFF) as u16;
            }
            // GP1 0x07 -- Vertical display range. Bits 0..=9 = top
            // scanline, bits 10..=19 = bottom scanline. Effective
            // rendered rows = (y2 - y1), doubled in 480-interlaced
            // mode. Redux's `takeScreenShot` dimensions come from
            // this, not from the GP1(08h) mode bit -- matching it is
            // what gets us 640×478 instead of 640×480 at boot.
            0x07 => {
                self.v_range_y1 = (value & 0x3FF) as u16;
                self.v_range_y2 = ((value >> 10) & 0x3FF) as u16;
                self.display_configured = true;
            }
            // GP1 0x08 -- display mode. Height is the interlace flag;
            // actual pixel count is derived together with V-range in
            // [`Gpu::effective_display_height`].
            0x08 => {
                self.display_mode_history.insert(value);
                let hres = if value & (1 << 6) != 0 {
                    match value & 0x3 {
                        0 => 368,
                        1 => 384,
                        2 => 512,
                        3 => 640,
                        _ => unreachable!(),
                    }
                } else {
                    match value & 0x3 {
                        0 => 256,
                        1 => 320,
                        2 => 512,
                        3 => 640,
                        _ => unreachable!(),
                    }
                };
                self.display_width = hres;
                self.display_height_480 = value & (1 << 2) != 0;
                self.display_24bpp = value & (1 << 4) != 0;
                self.display_configured = true;
            }
            // GP1 0x09 -- gate the upper VRAM Y-address bit used by E1/tpage
            // bit 11. The public ps1-tests corpus historically names this
            // "allow texture disable"; later silicon research identified it
            // as the 2 MiB VRAM address enable on v2 GPUs.
            0x09 => self.vram_2mb_addressing_enabled = value & 1 != 0,
            _ => {}
        }
    }

    fn reset_command_buffer(&mut self) {
        self.gp0_fifo.clear();
        self.gp0_expected = 0;
        self.vram_upload = None;
        self.vram_download = None;
        self.polyline = None;
        self.polyline_cmd_log_index = None;
    }

    /// Current dot-clock divisor: system clocks per pixel-clock tick.
    /// Indexed by the current display resolution. Values match Redux's
    /// `HDotClock` array in `src/core/psxcounters.cc`.
    ///
    /// Used by Timer 0 when its source is set to "dot clock" (mode
    /// bits 8..9 = 1 or 3). Games that sync to horizontal raster
    /// (rare -- dot-clock timing is usually a bit granular) key off
    /// this.
    pub fn dot_clock_divisor(&self) -> u64 {
        match self.display_width {
            256 => 10,
            320 => 8,
            368 | 384 => 7,
            512 => 5,
            640 => 4,
            _ => 10, // Safe fallback.
        }
    }

    /// Effective vertical pixel count shown on the video output --
    /// derived from V-range (`GP1(07h)`) and the 480-mode flag
    /// (`GP1(08h)` bit 2). Matches Redux's
    /// `PCSX.GPU.takeScreenShot()` height, so using this value for
    /// pixel-parity regression tests lines up byte-for-byte.
    ///
    /// Formula:
    /// ```text
    ///   rows_per_field = max(y2 - y1, 0)
    ///   visible        = rows_per_field * (480-mode ? 2 : 1)
    /// ```
    pub fn effective_display_height(&self) -> u16 {
        let rows = self.v_range_y2.saturating_sub(self.v_range_y1);
        if self.display_height_480 {
            rows.saturating_mul(2)
        } else {
            rows
        }
    }

    /// Feed one 32-bit word to the GP0 packet assembler. Public so DMA
    /// channel 2 can ship words through the same path CPU-direct writes
    /// take.
    pub fn gp0_push(&mut self, word: u32) {
        self.gp0_write(word, false);
    }

    /// DMA-channel-2 version of [`Gpu::gp0_push`]. Packet assembly and
    /// rendering are identical, but completed draw work also drives the
    /// silicon-observed GPUSTAT.28 transition.
    pub(crate) fn gp0_push_dma(&mut self, word: u32) {
        self.gp0_write(word, true);
    }

    /// Whether GP0 is currently accepting packed pixel words for an A0h
    /// CPU-to-VRAM transfer. DMA channel 2 uses this to distinguish image
    /// uploads (paced by GPU DMA requests) from command-list traffic.
    pub(crate) fn vram_upload_active(&self) -> bool {
        self.vram_upload.is_some()
    }

    /// Feed one 32-bit word to the GP0 packet assembler. If this word
    /// completes a packet, the packet is executed and the FIFO clears.
    fn gp0_write(&mut self, word: u32, from_dma: bool) {
        self.gp0_write_count += 1;

        // CPU→VRAM transfer consumes pixel words ahead of the packet
        // assembler -- it's a mode, not a packet.
        if self.vram_upload.is_some() {
            self.ingest_vram_upload_word(word);
            return;
        }

        // Polyline receive -- every word is either a vertex / colour
        // or the terminator sentinel until the list ends.
        if self.polyline.is_some() {
            self.ingest_polyline_word(word);
            return;
        }

        if self.gp0_expected == 0 {
            let op = (word >> 24) & 0xFF;
            self.gp0_expected = gp0_packet_size(op as u8);
            // Single-word commands execute immediately without buffering.
            if self.gp0_expected == 1 {
                self.execute_gp0_single(word);
                self.gp0_expected = 0;
                return;
            }
        }
        self.gp0_fifo.push(word);
        if self.gp0_fifo.len() == self.gp0_expected {
            self.execute_gp0_packet(from_dma);
            self.gp0_fifo.clear();
            self.gp0_expected = 0;
        }
    }

    /// Execute a command whose packet size is exactly 1. Draw-mode
    /// setters (GP0 0xE1..=0xE6) live here; we track drawing-area
    /// and drawing-offset because the rasterizer needs them.
    fn execute_gp0_single(&mut self, word: u32) {
        let op = (word >> 24) & 0xFF;
        let timing_cost = self.gp0_packet_timing_cost(op as u8);
        // Pixel tracer also wants to see state-modifying single-word
        // packets (0xE1..=0xE6 draw-mode / tex-window / draw-area /
        // draw-offset / mask). These don't plot pixels but they shift
        // the state that subsequent draws interpret -- useful to see
        // in the log when chasing a parity divergence.
        if self.cmd_log_enabled {
            let index = self.cmd_log.len() as u32;
            self.current_cmd_index = index;
            self.cmd_log.push(GpuCmdLogEntry {
                index,
                opcode: op as u8,
                fifo: vec![word],
            });
        }
        match op {
            // GP0 0x01 -- Clear texture cache. Real silicon invalidates the
            // cached CLUT as well: the next textured primitive must reload
            // palette data even when it reuses the same CLUT word.
            0x01 => {
                self.clut_cache_reg = u32::MAX;
                self.clut_cache_8bit = false;
            }
            // GP0 0x1F -- request GPU IRQ1. Rare in games, but BIOS and
            // hardware test suites can observe both GPUSTAT.24 and I_STAT.1.
            0x1F => {
                self.status.raw |= 1 << 24;
                self.irq_requested = true;
            }
            // GP0 0xE3 -- drawing area top-left. X bits 9:0, Y bits 18:10.
            0xE3 => {
                self.drawing_start_raw = word & 0x000F_FFFF;
                self.draw_area_left = (word & 0x3FF) as u16;
                self.draw_area_top = ((word >> 10) & 0x1FF) as u16;
            }
            // GP0 0xE4 -- drawing area bottom-right.
            0xE4 => {
                self.drawing_end_raw = word & 0x000F_FFFF;
                self.draw_area_right = (word & 0x3FF) as u16;
                self.draw_area_bottom = ((word >> 10) & 0x1FF) as u16;
            }
            // GP0 0xE5 -- drawing offset. X / Y are both signed 11-bit.
            0xE5 => {
                self.drawing_offset_raw = word & 0x003F_FFFF;
                self.draw_offset_x = sign_extend_11((word & 0x7FF) as i32);
                self.draw_offset_y = sign_extend_11(((word >> 11) & 0x7FF) as i32);
            }
            // GP0 0xE1 -- draw mode: texture page base + colour depth
            // + dither/display/transparency flags. We extract the
            // subset the texture rasterizer needs AND mirror bits
            // 0..=10 into `GpuStatus::raw`, since those are
            // observable via GPUSTAT reads. Redux's softgpu does
            // the equivalent in `gpuWriteStatus` / `sCommand0xE1`,
            // and the BIOS polls GPUSTAT right after each E1h to
            // verify the command took effect. Leaving the status
            // bits stale produces a GPUSTAT divergence that doesn't
            // surface until the poll.
            //
            // E1h layout:
            //   bits 0-3: texture page base X (each unit = 64 pix)
            //   bit  4:   texture page base Y (0=0, 1=256)
            //   bits 5-6: semi-transparency
            //   bits 7-8: texture page colour depth
            //   bit  9:   dither 24→15
            //   bit  10:  drawing to display area
            //   bit  11:  texture-page Y bit 1 (requires GP1 09h enable;
            //             surfaced as GPUSTAT bit 15)
            //   bit  12:  textured rectangle X flip
            //   bit  13:  textured rectangle Y flip
            // These map 1:1 to GPUSTAT bits 0..=10 (plus rect-flip
            // bits that aren't visible in GPUSTAT).
            0xE1 => {
                self.tex_page_x = ((word & 0x0F) as u16) * 64;
                self.tex_page_y = if (word >> 4) & 1 != 0 { 256 } else { 0 };
                let upper_y = self.vram_2mb_addressing_enabled && (word >> 11) & 1 != 0;
                if upper_y {
                    self.tex_page_y += 512;
                }
                self.tex_depth = ((word >> 7) & 0x3) as u8;
                self.tex_blend_mode = BlendMode::from_tpage_bits(word >> 5);
                self.dither_enabled = (word >> 9) & 1 != 0;
                self.tex_rect_flip_x = (word >> 12) & 1 != 0;
                self.tex_rect_flip_y = (word >> 13) & 1 != 0;
                // GPUSTAT bits 0..=10 come from E1h bits 0..=10. E1 bit 11
                // is exposed at GPUSTAT.15 only when GP1(09h) allows it.
                let stat_bits = word & 0x07FF;
                self.status.raw = (self.status.raw & !(0x07FF | 0x8000))
                    | stat_bits
                    | if upper_y { 0x8000 } else { 0 };
            }
            // GP0 0xE6 -- mask-bit setting.
            //   bit 0 = `mask_set_on_draw`: force bit 15 of every
            //           plotted pixel to 1 (protect it against
            //           later draws that check the mask).
            //   bit 1 = `mask_check_before_draw`: skip pixels whose
            //           existing VRAM bit 15 is already 1.
            // Both also surface in GPUSTAT at bits 11 / 12 so
            // software polls see the updated setting.
            0xE6 => {
                self.mask_set_on_draw = word & 1 != 0;
                self.mask_check_before_draw = word & 2 != 0;
                let stat_bits = (word & 0x3) << 11;
                self.status.raw = (self.status.raw & !0x1800) | stat_bits;
            }
            // GP0 0xE2 -- texture window. Lets a textured primitive
            // AND-mask its U/V into a smaller patch of the tpage,
            // effectively tiling a sub-rectangle of texture across
            // the prim. Format:
            //   bits 0-4  : mask X (U high bits forced; in 8-pixel steps)
            //   bits 5-9  : mask Y
            //   bits 10-14: offset X (U low bits OR'd)
            //   bits 15-19: offset Y
            // Per PSX-SPX, the effective texture coordinate is
            //     U' = (U & ~(mask_x << 3)) | ((offset_x & mask_x) << 3)
            //     V' = (V & ~(mask_y << 3)) | ((offset_y & mask_y) << 3)
            // (mask is in 8-pixel units; left-shift by 3 gives the
            // pixel-space mask.) Games that use palettes laid out in
            // small sub-rectangles rely on this to save VRAM -- the
            // same 128×128 tile gets reused for many prims with
            // different offsets.
            0xE2 => {
                self.texture_window_raw = word & 0x000F_FFFF;
                self.tex_window_mask_x = ((word & 0x1F) as u8) << 3;
                self.tex_window_mask_y = (((word >> 5) & 0x1F) as u8) << 3;
                self.tex_window_offset_x = (((word >> 10) & 0x1F) as u8) << 3;
                self.tex_window_offset_y = (((word >> 15) & 0x1F) as u8) << 3;
            }
            _ => {}
        }
        if op == 0x1F {
            // IRQ1 occupies the shared GP0 input path, so GPUSTAT.28 drops
            // with command-ready even when the command came from the CPU.
            self.charge_dma_busy(timing_cost);
        } else {
            self.charge_busy(timing_cost);
        }
    }

    /// Execute a multi-word packet that has just been fully assembled
    /// in `gp0_fifo`. Dispatches on the opcode in word 0.
    fn execute_gp0_packet(&mut self, from_dma: bool) {
        let op = (self.gp0_fifo[0] >> 24) & 0xFF;
        let pixel_cost = self.gp0_packet_timing_cost(op as u8);
        // The raster engine also has a command-level setup latency. Across
        // the bandwidth corpus, 400 repetitions consistently finish about
        // 60 HBlanks early without this term (roughly 320 CPU cycles per
        // command), independent of primitive size or texture mode.
        let timing_cost = if matches!(op, 0x40..=0x5F) {
            // Lines have their own much smaller setup pipeline; the returned
            // cost already includes it and must not inherit the 320-cycle
            // polygon/rectangle setup term.
            pixel_cost
        } else if pixel_cost == 0 {
            0
        } else {
            pixel_cost.saturating_add(320)
        };
        self.gp0_opcode_hist[op as usize] = self.gp0_opcode_hist[op as usize].saturating_add(1);
        self.gp0_timing_hist[op as usize] =
            self.gp0_timing_hist[op as usize].saturating_add(timing_cost);
        if from_dma {
            self.gp0_dma_timing_hist[op as usize] =
                self.gp0_dma_timing_hist[op as usize].saturating_add(timing_cost);
        }
        // If the pixel tracer is armed, stamp this packet into the
        // command log *before* dispatching -- `plot_pixel` uses
        // `current_cmd_index` to tag every write, so it must point
        // at the index of the entry we're about to push.
        if self.cmd_log_enabled {
            let index = self.cmd_log.len() as u32;
            self.current_cmd_index = index;
            self.cmd_log.push(GpuCmdLogEntry {
                index,
                opcode: op as u8,
                fifo: self.gp0_fifo.clone(),
            });
        }
        match op {
            // Monochrome fill rect (ignores draw area / offset).
            0x02 => self.fill_rect(),
            // Monochrome triangle / quad. Bit 3 distinguishes 3-vs-4
            // vertices; bit 1 is opaque-vs-semi-transparent (we treat
            // both as opaque for now).
            0x20..=0x23 => self.draw_monochrome_tri(),
            0x28..=0x2B => self.draw_monochrome_quad(),
            // Single monochrome line -- 3 words.
            0x40..=0x47 => self.draw_line_mono_single(),
            // Polyline monochrome start -- 3 words. After this packet
            // the FIFO enters a streaming mode that accepts vertex
            // words until the 0x55555555 / 0x50005000 terminator.
            0x48..=0x4F => self.draw_line_mono_start_polyline(),
            // Single shaded line -- 4 words.
            0x50..=0x57 => self.draw_line_shaded_single(),
            // Polyline shaded start -- 4 words.
            0x58..=0x5F => self.draw_line_shaded_start_polyline(),
            // Gouraud-shaded triangle / quad -- per-vertex colour
            // interpolated across the primitive via barycentrics.
            0x30..=0x33 => self.draw_shaded_tri(),
            0x38..=0x3B => self.draw_shaded_quad(),
            // Textured (flat-shade) triangle / quad -- per-vertex UV,
            // texture-page and CLUT pulled from the UV words.
            0x24..=0x27 => self.draw_textured_tri(),
            0x2C..=0x2F => self.draw_textured_quad(),
            // Textured + Gouraud shaded -- both per-vertex tint colours
            // AND per-vertex UV. The tint modulates every sampled
            // texel (per PSX-SPX tint formula). Triangle = 9 words,
            // quad = 12 words.
            0x34..=0x37 => self.draw_textured_shaded_tri(),
            0x3C..=0x3F => self.draw_textured_shaded_quad(),
            // Monochrome rectangles -- bit 3 set selects variable size
            // (followed by a W/H word), else 1×1/8×8/16×16 by bits 5:4.
            0x60..=0x63 => self.draw_monochrome_rect_variable(),
            0x68..=0x6B => self.draw_monochrome_rect_sized(1, 1),
            0x70..=0x73 => self.draw_monochrome_rect_sized(8, 8),
            0x78..=0x7B => self.draw_monochrome_rect_sized(16, 16),
            // Textured rectangles -- same geometry as the monochrome
            // variants plus a UV/CLUT word between pos and size.
            0x64..=0x67 => self.draw_textured_rect_variable(),
            0x6C..=0x6F => self.draw_textured_rect_sized(1, 1),
            0x74..=0x77 => self.draw_textured_rect_sized(8, 8),
            0x7C..=0x7F => self.draw_textured_rect_sized(16, 16),
            // CPU→VRAM transfer -- 3 words of setup, then `w*h/2`
            // words of pixel data follow as a separate mode. Hardware
            // ignores the lower 29 bits for transfer commands, so all
            // opcodes in this top-bit group behave as image upload.
            0xA0..=0xBF => self.begin_vram_upload(),
            // VRAM→CPU transfer -- same top-bit-group decoding as upload;
            // pixel words are then pulled by the CPU via GPUREAD.
            0xC0..=0xDF => self.begin_vram_download(),
            // VRAM→VRAM copy -- source rect blitted to dest rect.
            0x80..=0x9F => self.vram_to_vram_copy(),
            _ => {}
        }
        if from_dma {
            self.charge_dma_busy(timing_cost);
        } else {
            self.charge_busy(timing_cost);
        }
    }

    /// GP0 0x80 -- copy a rectangle of VRAM to another VRAM location.
    /// Packet: `[cmd, src_xy, dst_xy, wh]`. PS1 hardware uses the
    /// same packed VRAM fields as CPU upload/download: 10-bit X/width
    /// and 9-bit Y/height, with zero width/height meaning full
    /// 1024/512. The pixel accesses wrap at VRAM edges.
    /// We buffer the source row into a temp so that overlapping
    /// src/dst rects blit correctly.
    fn vram_to_vram_copy(&mut self) {
        let src_word = self.gp0_fifo[1];
        let dst_word = self.gp0_fifo[2];
        let wh_word = self.gp0_fifo[3];
        let sx = (src_word & 0x3FF) as u16;
        let sy = ((src_word >> 16) & 0x1FF) as u16;
        let dx = (dst_word & 0x3FF) as u16;
        let dy = ((dst_word >> 16) & 0x1FF) as u16;
        let raw_w = (wh_word & 0x3FF) as u16;
        let raw_h = ((wh_word >> 16) & 0x1FF) as u16;
        let w = if raw_w == 0 { 1024 } else { raw_w };
        let h = if raw_h == 0 { 512 } else { raw_h };
        // Per PSX-SPX the GP0(E6h) Set-Mask / Check-Mask bits DO apply to
        // VRAM->VRAM copies (unlike GP0(02h) Fill, which ignores them):
        // check-mask preserves destination pixels whose bit15 is set, and
        // force-mask ORs bit15 into every written pixel.
        let mask_check = self.mask_check_before_draw;
        let mask_set = self.mask_set_on_draw;
        let mut row = vec![0u16; w as usize];
        for dy_off in 0..h {
            for dx_off in 0..w {
                row[dx_off as usize] = self.vram.get_pixel(sx + dx_off, sy + dy_off);
            }
            for dx_off in 0..w {
                let (px, py) = (dx + dx_off, dy + dy_off);
                if mask_check && self.vram.get_pixel(px, py) & 0x8000 != 0 {
                    continue;
                }
                let pixel = if mask_set {
                    row[dx_off as usize] | 0x8000
                } else {
                    row[dx_off as usize]
                };
                self.vram.set_pixel(px, py, pixel);
            }
        }
    }

    // --- Primitive rasterization ---

    /// Parse a polygon vertex word: low 16 bits X, next 16 bits Y,
    /// both signed 11-bit. The drawing-offset is added here so callers
    /// get screen-space coordinates ready to rasterize.
    fn decode_vertex(&self, word: u32) -> (i32, i32) {
        let x = sign_extend_11((word & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((word >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        (x, y)
    }

    /// GP0 0x20..=0x23 -- monochrome 3-vertex triangle.
    /// Words: `[cmd+color, v0, v1, v2]`.
    fn draw_monochrome_tri(&mut self) {
        let cmd = self.gp0_fifo[0];
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let v1 = self.decode_vertex(self.gp0_fifo[2]);
        let v2 = self.decode_vertex(self.gp0_fifo[3]);
        self.rasterize_triangle(v0, v1, v2, color, mode);
    }

    /// GP0 0x28..=0x2B -- monochrome 4-vertex quad. Redux draws the
    /// lower/right half first, then the upper/left half, so pixels on
    /// the shared diagonal are owned by `(v0, v1, v2)`.
    fn draw_monochrome_quad(&mut self) {
        let cmd = self.gp0_fifo[0];
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let v1 = self.decode_vertex(self.gp0_fifo[2]);
        let v2 = self.decode_vertex(self.gp0_fifo[3]);
        let v3 = self.decode_vertex(self.gp0_fifo[4]);
        self.rasterize_triangle(v1, v3, v2, color, mode);
        self.rasterize_triangle(v0, v1, v2, color, mode);
    }

    /// GP0 0x30..=0x33 -- Gouraud triangle. Per-vertex RGB24 colours,
    /// interpolated across the triangle via barycentric weights.
    /// Words: `[cmd+c0, v0, c1, v1, c2, v2]`.
    fn draw_shaded_tri(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let c1 = self.gp0_fifo[2] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        let c2 = self.gp0_fifo[4] & 0x00FF_FFFF;
        let v2 = self.decode_vertex(self.gp0_fifo[5]);
        self.rasterize_shaded_triangle(v0, v1, v2, c0, c1, c2, mode);
    }

    /// GP0 0x38..=0x3B -- Gouraud quad. 4 × (colour+vertex) =
    /// 8 words, split in Redux order so the first half wins the
    /// shared diagonal.
    fn draw_shaded_quad(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let c1 = self.gp0_fifo[2] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        let c2 = self.gp0_fifo[4] & 0x00FF_FFFF;
        let v2 = self.decode_vertex(self.gp0_fifo[5]);
        let c3 = self.gp0_fifo[6] & 0x00FF_FFFF;
        let v3 = self.decode_vertex(self.gp0_fifo[7]);
        self.rasterize_shaded_triangle(v1, v3, v2, c1, c3, c2, mode);
        self.rasterize_shaded_triangle(v0, v1, v2, c0, c1, c2, mode);
    }

    /// GP0 0x60..=0x63 -- monochrome variable-size rectangle.
    /// Words: `[cmd+color, xy, wh]`.
    fn draw_monochrome_rect_variable(&mut self) {
        let cmd = self.gp0_fifo[0];
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let pos = self.gp0_fifo[1];
        let size = self.gp0_fifo[2];
        let x = sign_extend_11((pos & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((pos >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        let w = (size & 0xFFFF) as i32;
        let h = ((size >> 16) & 0xFFFF) as i32;
        self.maybe_trace_mono_rect("var", cmd, pos, Some(size), x, y, w, h, color, mode);
        self.paint_rect(x, y, w, h, color, mode);
    }

    /// GP0 0x68/0x70/0x78 -- fixed-size monochrome rectangles.
    fn draw_monochrome_rect_sized(&mut self, w: i32, h: i32) {
        let cmd = self.gp0_fifo[0];
        let color = rgb24_to_bgr15(cmd & 0x00FF_FFFF);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let pos = self.gp0_fifo[1];
        let x = sign_extend_11((pos & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((pos >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        self.maybe_trace_mono_rect("fixed", cmd, pos, None, x, y, w, h, color, mode);
        self.paint_rect(x, y, w, h, color, mode);
    }

    #[allow(clippy::too_many_arguments)]
    fn maybe_trace_mono_rect(
        &self,
        kind: &str,
        cmd: u32,
        pos: u32,
        size: Option<u32>,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: u16,
        mode: BlendMode,
    ) {
        let Some(min_area) = trace_mono_rect_min_area() else {
            return;
        };
        let left = x.max(self.draw_area_left as i32);
        let top = y.max(self.draw_area_top as i32);
        let right = (x + w - 1).min(self.draw_area_right as i32);
        let bottom = (y + h - 1).min(self.draw_area_bottom as i32);
        if left > right || top > bottom {
            return;
        }
        let clipped_w = right - left + 1;
        let clipped_h = bottom - top + 1;
        let clipped_area = clipped_w.saturating_mul(clipped_h);
        if clipped_area < min_area {
            return;
        }
        let trace_index = trace_mono_rect_count();
        if trace_index >= trace_mono_rect_limit() {
            return;
        }
        eprintln!(
            "[gpu-trace] mono_rect #{trace_index} kind={kind} op=0x{:02x} cmd=0x{cmd:08x} pos=0x{pos:08x} size={} xy=({x},{y}) wh={}x{} clip=({left},{top}) {}x{} area={} color=0x{color:04x} mode={mode:?} draw_area=({},{}..{}, {}) offset=({}, {})",
            cmd >> 24,
            size.map(|word| format!("0x{word:08x}"))
                .unwrap_or_else(|| "-".to_string()),
            w,
            h,
            clipped_w,
            clipped_h,
            clipped_area,
            self.draw_area_left,
            self.draw_area_top,
            self.draw_area_right,
            self.draw_area_bottom,
            self.draw_offset_x,
            self.draw_offset_y
        );
    }

    /// Variable-size textured rect. Words: `[cmd+tint, xy, clut+uv, wh]`.
    fn draw_textured_rect_variable(&mut self) {
        let cmd = self.gp0_fifo[0];
        let pos = self.gp0_fifo[1];
        let uv_clut = self.gp0_fifo[2];
        let size = self.gp0_fifo[3];
        let x = sign_extend_11((pos & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((pos >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        let w = (size & 0xFFFF) as i32;
        let h = ((size >> 16) & 0xFFFF) as i32;
        let u0 = (uv_clut & 0xFF) as u16;
        let v0 = ((uv_clut >> 8) & 0xFF) as u16;
        let clut_word = ((uv_clut >> 16) & 0xFFFF) as u16;
        // Raw-texture flag is bit 0 of the *opcode byte* (bit 24 of
        // the full cmd word), per PSX-SPX. Testing `cmd & 1` reads
        // bit 0 of the R channel of the embedded colour instead --
        // so odd-R tints like 0xFF would be mis-flagged raw.
        let tint = if (cmd >> 24) & 1 != 0 {
            RAW_TEXTURE_TINT
        } else {
            split_tint(cmd & 0x00FF_FFFF)
        };
        self.paint_textured_rect(x, y, w, h, u0, v0, clut_word, prim_is_semi_trans(cmd), tint);
    }

    /// Fixed-size textured rect (1×1, 8×8, 16×16).
    /// Words: `[cmd+tint, xy, clut+uv]`.
    fn draw_textured_rect_sized(&mut self, w: i32, h: i32) {
        let cmd = self.gp0_fifo[0];
        let pos = self.gp0_fifo[1];
        let uv_clut = self.gp0_fifo[2];
        let x = sign_extend_11((pos & 0x7FF) as i32) + self.draw_offset_x;
        let y = sign_extend_11(((pos >> 16) & 0x7FF) as i32) + self.draw_offset_y;
        let u0 = (uv_clut & 0xFF) as u16;
        let v0 = ((uv_clut >> 8) & 0xFF) as u16;
        let clut_word = ((uv_clut >> 16) & 0xFFFF) as u16;
        // Raw-texture flag is bit 0 of the *opcode byte* (bit 24 of
        // the full cmd word), per PSX-SPX. Testing `cmd & 1` reads
        // bit 0 of the R channel of the embedded colour instead --
        // so odd-R tints like 0xFF would be mis-flagged raw.
        let tint = if (cmd >> 24) & 1 != 0 {
            RAW_TEXTURE_TINT
        } else {
            split_tint(cmd & 0x00FF_FFFF)
        };
        self.paint_textured_rect(x, y, w, h, u0, v0, clut_word, prim_is_semi_trans(cmd), tint);
    }

    /// Plot a textured rectangle. Each destination pixel samples a
    /// 1:1 texel from the current texture page, CLUT-indexed for
    /// 4bpp / 8bpp modes, direct for 15bpp. Texels of value 0 are
    /// transparent (standard PS1 convention).
    ///
    /// `semi_trans` -- cmd-bit-1. Texels with bit 15 high blend via
    /// `self.tex_blend_mode` when it's set; texels with bit 15 clear
    /// always draw opaque.
    ///
    /// `tint` -- 24-bit vertex colour that modulates each texel (see
    /// [`modulate_tint`]). Raw-texture rectangles pass
    /// `(0x80, 0x80, 0x80)` so modulation is a no-op.
    #[allow(clippy::too_many_arguments)]
    fn paint_textured_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        u0: u16,
        v0: u16,
        clut_word: u16,
        semi_trans: bool,
        tint: (u32, u32, u32),
    ) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.update_clut_if_needed(clut_word);
        let tpage_mode = self.tex_blend_mode;

        let left = x.max(self.draw_area_left as i32);
        let top = y.max(self.draw_area_top as i32);
        let right = (x + w - 1).min(self.draw_area_right as i32);
        let bottom = (y + h - 1).min(self.draw_area_bottom as i32);
        if left > right || top > bottom {
            return;
        }

        let flip_x = self.tex_rect_flip_x;
        let flip_y = self.tex_rect_flip_y;
        for py in top..=bottom {
            for px in left..=right {
                let dx = (px - x) as u16;
                let dy = (py - y) as u16;
                // Silicon's rectangle flip bits reverse the 8-bit texture
                // counters around the command's starting UV; they do not
                // mirror around the far edge of the rectangle. X carries a
                // one-texel bias (u0+1, u0, u0-1...), while Y starts at v0
                // and decrements (v0, v0-1...).
                let tex_u = if flip_x {
                    u0.wrapping_add(1).wrapping_sub(dx)
                } else {
                    u0.wrapping_add(dx)
                };
                let tex_v = if flip_y {
                    v0.wrapping_sub(dy)
                } else {
                    v0.wrapping_add(dy)
                };
                if let Some(texel) = self.sample_texture(tex_u, tex_v) {
                    let shaded = modulate_tint(texel, tint.0, tint.1, tint.2);
                    let mode = if semi_trans && (texel & 0x8000) != 0 {
                        tpage_mode
                    } else {
                        BlendMode::Opaque
                    };
                    self.plot_pixel(px as u16, py as u16, shaded, mode);
                }
            }
        }
    }

    /// Fetch a single texel from the active texture page. Returns
    /// `None` for transparent -- PSX convention is **the resolved
    /// 16-bit colour == 0x0000**, regardless of mode. For 4bpp/8bpp,
    /// that means `CLUT[idx] == 0` (not `idx == 0`); games routinely
    /// place `0x0000` at non-zero CLUT entries to punch transparency
    /// into sprites (e.g. the BIOS TM glyph: background uses a CLUT
    /// index whose entry is 0 → transparent). Checking `idx == 0`
    /// instead is a common simplification that renders those pixels
    /// opaque black, producing the infamous "TM on a black box"
    /// regression. Matches Redux's `getTextureTransCol*` which all
    /// start with `if (color == 0) return;`.
    ///
    /// The incoming `u` / `v` are run through the GP0 0xE2 texture
    /// window first: `U' = (U & ~mask) | (offset & mask)` per axis.
    /// With the default (all zeroes) that's a no-op; games that use
    /// tiling set non-zero mask/offset to reuse a sub-rectangle of
    /// the tpage across multiple primitives.
    /// Reload the CLUT cache from VRAM when the CLUT register (the clut word
    /// of a textured primitive) changes -- or when a 4bpp-loaded cache is
    /// reused for an 8bpp draw. Crucially the cache is NOT reloaded when VRAM
    /// is overwritten, so a game that re-uploads the same CLUT keeps sampling
    /// the stale palette until the clut word changes. Call once per textured
    /// primitive before sampling. 15bpp (direct) has no CLUT.
    fn update_clut_if_needed(&mut self, clut_word: u16) {
        if self.tex_depth >= 2 {
            return;
        }
        let is_8bit = self.tex_depth == 1;
        let reg = clut_word as u32;
        if reg == self.clut_cache_reg && (!is_8bit || self.clut_cache_8bit) {
            return; // cache still valid for this CLUT word
        }
        let clut_x = (clut_word & 0x3F) * 16;
        let clut_y = (clut_word >> 6) & 0x1FF;
        let n = if is_8bit { 256 } else { 16 };
        for i in 0..n {
            self.clut_cache[i] = self.vram.get_pixel(clut_x + i as u16, clut_y);
        }
        self.clut_cache_reg = reg;
        self.clut_cache_8bit = is_8bit;
    }

    fn sample_texture(&self, u: u16, v: u16) -> Option<u16> {
        // PSX-SPX: the GPU's U/V counters are 8 bits -- texture pages
        // wrap every 256 texels horizontally and vertically. Callers
        // pass u16 because rasterizer interpolation works in a wider
        // domain, so we mask down to 8 bits *before* the texture
        // window. Without this, a sprite or polygon whose `U + dx`
        // exceeds 255 reads VRAM PAST the tpage edge -- typically the
        // neighbouring tpage's data, garbage texels, or a different
        // CLUT-driven byte. Visible as smeared / corrupted 2D sprites
        // (character portraits, BIOS dialog frames).
        let u = u & 0xFF;
        let v = v & 0xFF;
        // Apply the texture window -- PSX-SPX:
        //   U' = (U AND NOT(mask_x * 8)) OR ((offset_x * 8) AND (mask_x * 8))
        // but both `mask_*` and `offset_*` are already pre-shifted (×8)
        // when we stored them in the GP0 0xE2 handler.
        let mask_x = self.tex_window_mask_x as u16;
        let mask_y = self.tex_window_mask_y as u16;
        let off_x = self.tex_window_offset_x as u16;
        let off_y = self.tex_window_offset_y as u16;
        let u = (u & !mask_x) | (off_x & mask_x);
        let v = (v & !mask_y) | (off_y & mask_y);

        // PSoXide models a retail 1 MiB VRAM. GP1(09h)+tpage bit 11 selects
        // the unpopulated upper bank on that hardware, so texture reads do
        // not mirror into the lower 512 lines.
        if self.tex_page_y >= VRAM_HEIGHT as u16 {
            return None;
        }
        let tpy = self.tex_page_y.wrapping_add(v);
        let texel = match self.tex_depth {
            0 => {
                // 4bpp: 4 texels per VRAM word; select by (u & 3).
                let tpx = self.tex_page_x.wrapping_add(u / 4);
                let word = self.vram.get_pixel(tpx, tpy);
                let idx = (word >> ((u & 3) * 4)) & 0xF;
                self.clut_cache[idx as usize]
            }
            1 => {
                // 8bpp: 2 texels per VRAM word.
                let tpx = self.tex_page_x.wrapping_add(u / 2);
                let word = self.vram.get_pixel(tpx, tpy);
                let idx = (word >> ((u & 1) * 8)) & 0xFF;
                self.clut_cache[idx as usize]
            }
            _ => {
                // 15bpp: direct colour, 1 texel per word.
                let tpx = self.tex_page_x.wrapping_add(u);
                self.vram.get_pixel(tpx, tpy)
            }
        };
        if texel == 0 {
            None
        } else {
            Some(texel)
        }
    }

    /// Plot a rectangle of `color` in screen-space, clipped to the
    /// GPU's drawing area. `mode` lets the caller pass the
    /// primitive's semi-transparency mode -- opaque for the common
    /// case, one of the blend variants when the GP0 command's
    /// cmd-bit-1 is set.
    fn paint_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u16, mode: BlendMode) {
        if w <= 0 || h <= 0 {
            return;
        }
        let left = x.max(self.draw_area_left as i32);
        let top = y.max(self.draw_area_top as i32);
        let right = (x + w - 1).min(self.draw_area_right as i32);
        let bottom = (y + h - 1).min(self.draw_area_bottom as i32);
        if left > right || top > bottom {
            return;
        }
        if self.pixel_owner.is_none() {
            if mode == BlendMode::Opaque && !self.mask_check_before_draw {
                let color = if self.mask_set_on_draw {
                    color | 0x8000
                } else {
                    color
                };
                self.vram.fill_rect_unwrapped(
                    left as u16,
                    top as u16,
                    right as u16,
                    bottom as u16,
                    color,
                );
                return;
            }

            let left = left as usize;
            let right = right as usize;
            let top = top as usize;
            let bottom = bottom as usize;
            let set_mask = self.mask_set_on_draw;
            let check_mask = self.mask_check_before_draw;
            for py in top..=bottom {
                let row_start = py * VRAM_WIDTH;
                for existing in &mut self.vram.words_mut()[row_start + left..=row_start + right] {
                    if check_mask && *existing & 0x8000 != 0 {
                        continue;
                    }
                    let mut pixel = if mode == BlendMode::Opaque {
                        color
                    } else {
                        blend_pixel(*existing, color, mode)
                    };
                    if set_mask {
                        pixel |= 0x8000;
                    }
                    *existing = pixel;
                }
            }
            return;
        }
        for py in top..=bottom {
            for px in left..=right {
                self.plot_pixel(px as u16, py as u16, color, mode);
            }
        }
    }

    /// Scanline-ish triangle rasterizer using the edge-function test.
    /// For each pixel in the bounding box we evaluate three edge
    /// equations; a pixel is inside iff all three have the same sign.
    /// Works regardless of triangle winding. Clipped to both VRAM
    /// bounds and the active drawing area.
    fn rasterize_triangle(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        color: u16,
        mode: BlendMode,
    ) {
        // Wireframe debug mode: replace the filled interior with three
        // edge lines (interiors stay transparent). Stale-edge cleanup is
        // handled by the per-frame journal (`wireframe_frame_boundary`),
        // not by filling or clearing. Includes over-sized triangles that
        // would normally be dropped by the extent check below.
        if self.wireframe_enabled {
            self.rasterize_line(v0, v1, color, color, mode, false);
            self.rasterize_line(v1, v2, color, color, mode, false);
            self.rasterize_line(v2, v0, color, color, mode, false);
            return;
        }
        if triangle_exceeds_hw_extent(v0, v1, v2) {
            return;
        }
        // PS1 silicon triangle coverage (half-pixel-biased DDA), from
        // the shared `tri_raster_setup`: walk the long edge (a->c) and
        // the two short edges (a->b, b->c) in Q32.32, plot
        // [tri_span_x(left), tri_span_x(right)) per scanline
        // (right-exclusive). Center-sampled; the Redux scanline-delta
        // path it replaces sampled pixel corners, which real hardware
        // (ledger HWB-005) rejects on every diagonal-edged triangle.
        // Flat fill has no determinant bail (`require_attrs = false`):
        // collinear triangles still walk their (empty-ish) spans.
        let Some(setup) = tri_raster_setup([v0, v1, v2], [(0, 0, 0); 3], [(0, 0); 3], false) else {
            return; // zero vertical extent
        };

        let draw_top = self.draw_area_top as i32;
        let draw_bottom = self.draw_area_bottom as i32;
        let draw_left = self.draw_area_left as i32;
        let draw_right = self.draw_area_right as i32;

        for (y0, y1, mut lx, ls, mut rx, rs) in setup.parts {
            let mut y = y0;
            while y < y1 {
                if y >= draw_top && y <= draw_bottom {
                    let xs = tri_span_x(lx).max(draw_left);
                    let xe = tri_span_x(rx).min(draw_right + 1); // right-exclusive
                    let mut x = xs;
                    while x < xe {
                        self.plot_pixel(x as u16, y as u16, color, mode);
                        x += 1;
                    }
                }
                lx += ls;
                rx += rs;
                y += 1;
            }
        }
    }

    /// GP0 0x24..=0x27 -- textured triangle. 7 words:
    /// `[cmd+tint, v0, clut+uv0, v1, tpage+uv1, v2, uv2]`.
    ///
    /// Command bit 0 chooses raw-texture (tint ignored) vs
    /// texture-blended (vertex tint modulates each texel).
    fn draw_textured_tri(&mut self) {
        let cmd = self.gp0_fifo[0];
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let uv0 = self.gp0_fifo[2];
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        let uv1 = self.gp0_fifo[4];
        let v2 = self.decode_vertex(self.gp0_fifo[5]);
        let uv2 = self.gp0_fifo[6];
        let clut_word = ((uv0 >> 16) & 0xFFFF) as u16;
        // The tpage word in UV1 overrides the current draw-mode tpage
        // for the duration of this primitive.
        self.apply_primitive_tpage(uv1);
        let t0 = ((uv0 & 0xFF) as u16, ((uv0 >> 8) & 0xFF) as u16);
        let t1 = ((uv1 & 0xFF) as u16, ((uv1 >> 8) & 0xFF) as u16);
        let t2 = ((uv2 & 0xFF) as u16, ((uv2 >> 8) & 0xFF) as u16);
        // Raw-texture flag is bit 0 of the *opcode byte* (bit 24 of
        // the full cmd word), per PSX-SPX. Testing `cmd & 1` reads
        // bit 0 of the R channel of the embedded colour instead --
        // so odd-R tints like 0xFF would be mis-flagged raw.
        let tint = if (cmd >> 24) & 1 != 0 {
            RAW_TEXTURE_TINT
        } else {
            split_tint(cmd & 0x00FF_FFFF)
        };
        self.rasterize_textured_triangle(
            v0,
            v1,
            v2,
            t0,
            t1,
            t2,
            clut_word,
            prim_is_semi_trans(cmd),
            tint,
        );
    }

    /// GP0 0x2C..=0x2F -- textured quad. 9 words; split in Redux order.
    fn draw_textured_quad(&mut self) {
        let cmd = self.gp0_fifo[0];
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let uv0 = self.gp0_fifo[2];
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        let uv1 = self.gp0_fifo[4];
        let v2 = self.decode_vertex(self.gp0_fifo[5]);
        let uv2 = self.gp0_fifo[6];
        let v3 = self.decode_vertex(self.gp0_fifo[7]);
        let uv3 = self.gp0_fifo[8];
        let clut_word = ((uv0 >> 16) & 0xFFFF) as u16;
        self.apply_primitive_tpage(uv1);
        let t0 = ((uv0 & 0xFF) as u16, ((uv0 >> 8) & 0xFF) as u16);
        let t1 = ((uv1 & 0xFF) as u16, ((uv1 >> 8) & 0xFF) as u16);
        let t2 = ((uv2 & 0xFF) as u16, ((uv2 >> 8) & 0xFF) as u16);
        let t3 = ((uv3 & 0xFF) as u16, ((uv3 >> 8) & 0xFF) as u16);
        let semi = prim_is_semi_trans(cmd);
        // Raw-texture flag is bit 0 of the *opcode byte* (bit 24 of
        // the full cmd word), per PSX-SPX. Testing `cmd & 1` reads
        // bit 0 of the R channel of the embedded colour instead --
        // so odd-R tints like 0xFF would be mis-flagged raw.
        let tint = if (cmd >> 24) & 1 != 0 {
            RAW_TEXTURE_TINT
        } else {
            split_tint(cmd & 0x00FF_FFFF)
        };
        if self.rasterize_axis_aligned_textured_quad(
            v0, v1, v2, v3, t0, t1, t2, t3, clut_word, semi, tint,
        ) {
            return;
        }
        self.rasterize_textured_triangle(v1, v3, v2, t1, t3, t2, clut_word, semi, tint);
        self.rasterize_textured_triangle(v0, v1, v2, t0, t1, t2, clut_word, semi, tint);
    }

    /// GP0 0x34..=0x37 -- textured + Gouraud-shaded triangle.
    /// Words: `[cmd+c0, v0, uv0+clut, c1, v1, uv1+texpage, c2, v2, uv2]`.
    /// Per-vertex tint interpolation is barycentric just like flat
    /// Gouraud, but the tint modulates the sampled texel instead of
    /// being the final pixel colour. Raw-texture mode (bit 0 set)
    /// zeros the tint effect per PSX-SPX.
    fn draw_textured_shaded_tri(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let uv0 = self.gp0_fifo[2];
        let c1 = self.gp0_fifo[3] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[4]);
        let uv1 = self.gp0_fifo[5];
        let c2 = self.gp0_fifo[6] & 0x00FF_FFFF;
        let v2 = self.decode_vertex(self.gp0_fifo[7]);
        let uv2 = self.gp0_fifo[8];
        let clut_word = ((uv0 >> 16) & 0xFFFF) as u16;
        self.apply_primitive_tpage(uv1);
        let t0 = ((uv0 & 0xFF) as u16, ((uv0 >> 8) & 0xFF) as u16);
        let t1 = ((uv1 & 0xFF) as u16, ((uv1 >> 8) & 0xFF) as u16);
        let t2 = ((uv2 & 0xFF) as u16, ((uv2 >> 8) & 0xFF) as u16);
        // See comment on the other textured primitives: raw-texture
        // flag is bit 0 of the opcode byte (= bit 24 of the cmd word),
        // not bit 0 of the full cmd word.
        let raw = (cmd >> 24) & 1 != 0;
        self.rasterize_textured_shaded_triangle(
            v0,
            v1,
            v2,
            t0,
            t1,
            t2,
            c0,
            c1,
            c2,
            clut_word,
            prim_is_semi_trans(cmd),
            raw,
        );
    }

    /// GP0 0x3C..=0x3F -- textured + Gouraud-shaded quad. 12 words;
    /// split in Redux order.
    /// Words: `[cmd+c0, v0, uv0+clut, c1, v1, uv1+texpage, c2, v2, uv2,
    ///          c3, v3, uv3]`.
    fn draw_textured_shaded_quad(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let uv0 = self.gp0_fifo[2];
        let c1 = self.gp0_fifo[3] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[4]);
        let uv1 = self.gp0_fifo[5];
        let c2 = self.gp0_fifo[6] & 0x00FF_FFFF;
        let v2 = self.decode_vertex(self.gp0_fifo[7]);
        let uv2 = self.gp0_fifo[8];
        let c3 = self.gp0_fifo[9] & 0x00FF_FFFF;
        let v3 = self.decode_vertex(self.gp0_fifo[10]);
        let uv3 = self.gp0_fifo[11];
        let clut_word = ((uv0 >> 16) & 0xFFFF) as u16;
        self.apply_primitive_tpage(uv1);
        let t0 = ((uv0 & 0xFF) as u16, ((uv0 >> 8) & 0xFF) as u16);
        let t1 = ((uv1 & 0xFF) as u16, ((uv1 >> 8) & 0xFF) as u16);
        let t2 = ((uv2 & 0xFF) as u16, ((uv2 >> 8) & 0xFF) as u16);
        let t3 = ((uv3 & 0xFF) as u16, ((uv3 >> 8) & 0xFF) as u16);
        let semi = prim_is_semi_trans(cmd);
        // See comment on the other textured primitives: raw-texture
        // flag is bit 0 of the opcode byte (= bit 24 of the cmd word),
        // not bit 0 of the full cmd word.
        let raw = (cmd >> 24) & 1 != 0;
        self.rasterize_textured_shaded_triangle(
            v1, v3, v2, t1, t3, t2, c1, c3, c2, clut_word, semi, raw,
        );
        self.rasterize_textured_shaded_triangle(
            v0, v1, v2, t0, t1, t2, c0, c1, c2, clut_word, semi, raw,
        );
    }

    /// Fast path for the common 2D-sprite case: a flat textured quad
    /// whose vertex order is top-left, top-right, bottom-left,
    /// bottom-right. Redux renders flat textured quads with a true
    /// four-edge scanline walker, not by splitting the primitive into
    /// two triangles; using the same row-wide UV interpolation removes
    /// diagonal sampling seams in BIOS text and loading-screen sprites.
    #[allow(clippy::too_many_arguments)]
    fn rasterize_axis_aligned_textured_quad(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        v3: (i32, i32),
        t0: (u16, u16),
        t1: (u16, u16),
        t2: (u16, u16),
        t3: (u16, u16),
        clut_word: u16,
        semi_trans: bool,
        tint: (u32, u32, u32),
    ) -> bool {
        if self.wireframe_enabled {
            return false;
        }
        if v0.1 != v1.1 || v2.1 != v3.1 || v0.0 != v2.0 || v1.0 != v3.0 {
            return false;
        }
        if triangle_exceeds_hw_extent(v1, v3, v2) || triangle_exceeds_hw_extent(v0, v1, v2) {
            return true;
        }
        let x0 = v0.0;
        let x1 = v1.0;
        let y0 = v0.1;
        let y1 = v2.1;
        let left = x0.min(x1);
        let right = x0.max(x1);
        let top = y0.min(y1);
        let bottom = y0.max(y1);
        let width = right - left;
        let height = bottom - top;
        if width <= 0 || height <= 0 {
            return true;
        }

        let draw_left = self.draw_area_left as i32;
        let draw_right = self.draw_area_right as i32;
        let draw_top = self.draw_area_top as i32;
        let draw_bottom = self.draw_area_bottom as i32;
        let y_start = top.max(draw_top);
        let y_end = (bottom - 1).min(draw_bottom);
        let x_start = left.max(draw_left);
        let x_end = (right - 1).min(draw_right);
        if y_start > y_end || x_start > x_end {
            return true;
        }

        self.update_clut_if_needed(clut_word);
        let tpage_mode = self.tex_blend_mode;
        // Same per-primitive dither rule as the textured-triangle
        // path: flat-tint (non-raw) textured prims are dithered when
        // GP0 0xE1 bit 9 is set; raw-texture prims are not.
        let dither = self.dither_enabled && tint != RAW_TEXTURE_TINT;
        let (top_a, bottom_a, top_b, bottom_b) = if y0 <= y1 {
            (t0, t2, t1, t3)
        } else {
            (t2, t0, t3, t1)
        };
        let (left_top, left_bottom, right_top, right_bottom) = if x0 <= x1 {
            (top_a, bottom_a, top_b, bottom_b)
        } else {
            (top_b, bottom_b, top_a, bottom_a)
        };

        // The PS1 attribute DDA has 12 fractional bits, truncates each
        // per-pixel gradient to that precision, and seeds attributes at
        // the pixel centre (+0.5). This is observable even on a 1-pixel
        // tall quad: interpolating U=0..1 across 100 pixels changes texel
        // at x=52 because floor(4096/100)=40 and ceil(2048/40)=52.
        // Keeping Q16 here (or using an exact rational) produces a visibly
        // different transition and fails the silicon uv-interpolation ROM.
        const ATTR_SHIFT: i64 = 12;
        const ATTR_HALF: i64 = 1 << (ATTR_SHIFT - 1);
        let left_u0 = ((left_top.0 as i64) << ATTR_SHIFT) + ATTR_HALF;
        let left_v0 = ((left_top.1 as i64) << ATTR_SHIFT) + ATTR_HALF;
        let right_u0 = ((right_top.0 as i64) << ATTR_SHIFT) + ATTR_HALF;
        let right_v0 = ((right_top.1 as i64) << ATTR_SHIFT) + ATTR_HALF;
        let delta_left_u =
            ((left_bottom.0 as i64 - left_top.0 as i64) << ATTR_SHIFT) / height as i64;
        let delta_left_v =
            ((left_bottom.1 as i64 - left_top.1 as i64) << ATTR_SHIFT) / height as i64;
        let delta_right_u =
            ((right_bottom.0 as i64 - right_top.0 as i64) << ATTR_SHIFT) / height as i64;
        let delta_right_v =
            ((right_bottom.1 as i64 - right_top.1 as i64) << ATTR_SHIFT) / height as i64;

        for py in y_start..=y_end {
            let row = (py - top) as i64;
            let mut pos_u = left_u0 + row * delta_left_u;
            let mut pos_v = left_v0 + row * delta_left_v;
            let right_u = right_u0 + row * delta_right_u;
            let right_v = right_v0 + row * delta_right_v;
            let delta_u = (right_u - pos_u) / width as i64;
            let delta_v = (right_v - pos_v) / width as i64;
            if x_start > left {
                let skip = (x_start - left) as i64;
                pos_u += skip * delta_u;
                pos_v += skip * delta_v;
            }
            for px in x_start..=x_end {
                let u = (pos_u >> ATTR_SHIFT) as u16;
                let v = (pos_v >> ATTR_SHIFT) as u16;
                if let Some(texel) = self.sample_texture(u, v) {
                    let shaded = if dither {
                        modulate_tint_dithered(texel, tint.0, tint.1, tint.2, px, py)
                    } else {
                        modulate_tint(texel, tint.0, tint.1, tint.2)
                    };
                    let mode = if semi_trans && (texel & 0x8000) != 0 {
                        tpage_mode
                    } else {
                        BlendMode::Opaque
                    };
                    self.plot_pixel(px as u16, py as u16, shaded, mode);
                }
                pos_u += delta_u;
                pos_v += delta_v;
            }
        }
        true
    }

    /// Rasterize a triangle with per-vertex tint colours AND per-vertex
    /// UVs. Combines the math from `rasterize_shaded_triangle` (three
    /// barycentric-weighted colours) and `rasterize_textured_triangle`
    /// (three barycentric-weighted UVs). The interpolated tint
    /// modulates the sampled texel via [`modulate_tint`]; raw-texture
    /// mode passes `0x80, 0x80, 0x80` which is the identity.
    #[allow(clippy::too_many_arguments)]
    fn rasterize_textured_shaded_triangle(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        t0: (u16, u16),
        t1: (u16, u16),
        t2: (u16, u16),
        c0: u32,
        c1: u32,
        c2: u32,
        clut_word: u16,
        semi_trans: bool,
        raw_texture: bool,
    ) {
        if self.wireframe_enabled {
            self.rasterize_line_shaded(v0, v1, c0, c1, BlendMode::Opaque);
            self.rasterize_line_shaded(v1, v2, c1, c2, BlendMode::Opaque);
            self.rasterize_line_shaded(v2, v0, c2, c0, BlendMode::Opaque);
            // Silence unused-var warnings for the texture args we
            // intentionally drop in wireframe mode.
            let _ = (t0, t1, t2, clut_word, semi_trans, raw_texture);
            return;
        }
        if triangle_exceeds_hw_extent(v0, v1, v2) {
            return;
        }
        self.update_clut_if_needed(clut_word);
        let tpage_mode = self.tex_blend_mode;

        let r = |c: u32| (c & 0xFF) as i32;
        let g = |c: u32| ((c >> 8) & 0xFF) as i32;
        let b = |c: u32| ((c >> 16) & 0xFF) as i32;
        let v_rgb = [
            (r(c0), g(c0), b(c0)),
            (r(c1), g(c1), b(c1)),
            (r(c2), g(c2), b(c2)),
        ];
        let v_uv = [
            (t0.0 as i32, t0.1 as i32),
            (t1.0 as i32, t1.1 as i32),
            (t2.0 as i32, t2.1 as i32),
        ];
        let draw_top = self.draw_area_top as i32;
        let draw_bottom = self.draw_area_bottom as i32;
        let draw_left = self.draw_area_left as i32;
        let draw_right = self.draw_area_right as i32;
        let dither = self.dither_enabled;
        for_each_tri_pixel(
            [v0, v1, v2],
            v_rgb,
            v_uv,
            draw_top,
            draw_bottom,
            draw_left,
            draw_right,
            |x, y, ri, gi, bi, u, v| {
                if let Some(texel) = self.sample_texture(u as u16, v as u16) {
                    let (tint_r, tint_g, tint_b) = if raw_texture {
                        RAW_TEXTURE_TINT
                    } else {
                        (ri as u32, gi as u32, bi as u32)
                    };
                    let shaded = if !raw_texture && dither {
                        modulate_tint_dithered(texel, tint_r, tint_g, tint_b, x, y)
                    } else {
                        modulate_tint(texel, tint_r, tint_g, tint_b)
                    };
                    let mode = if semi_trans && (texel & 0x8000) != 0 {
                        tpage_mode
                    } else {
                        BlendMode::Opaque
                    };
                    self.plot_pixel(x as u16, y as u16, shaded, mode);
                }
            },
        );
    }

    /// Apply the tpage bits embedded in a textured-primitive UV word
    /// (they override the draw-mode tpage for this primitive onward).
    ///
    /// The real-silicon `gpu/gp0-e1` corpus establishes that polygon tpage
    /// attributes update draw-mode bits 0..=8 and bit 11, while bits 9..=10
    /// (dither and draw-to-display) remain exactly as set by GP0(E1h).
    ///
    /// Missing this sync surfaced at parity step 60,041,097 as a
    /// GPUSTAT load that differed in the low byte: our GPUSTAT
    /// kept reflecting the last E1's tpage-X even after a textured
    /// polygon's embedded tpage re-pointed it.
    fn apply_primitive_tpage(&mut self, uv_word: u32) {
        let tpage = (uv_word >> 16) & 0xFFFF;
        self.tex_page_x = ((tpage & 0x0F) as u16) * 64;
        self.tex_page_y = if (tpage >> 4) & 1 != 0 { 256 } else { 0 };
        let upper_y = self.vram_2mb_addressing_enabled && (tpage >> 11) & 1 != 0;
        if upper_y {
            self.tex_page_y += 512;
        }
        self.tex_depth = ((tpage >> 7) & 0x3) as u8;
        self.tex_blend_mode = BlendMode::from_tpage_bits(tpage >> 5);
        let stat_bits = tpage & 0x01FF;
        self.status.raw =
            (self.status.raw & !(0x01FF | 0x8000)) | stat_bits | if upper_y { 0x8000 } else { 0 };
    }

    /// Plot a single 15bpp pixel at `(x, y)`. When `mode == Opaque`
    /// this is a plain VRAM write; otherwise we fetch the existing
    /// pixel and run the semi-transparency blend.
    ///
    /// Also respects the GP0 0xE6 mask-bit flags:
    /// - If `mask_check_before_draw` is on and the existing VRAM
    ///   pixel's bit 15 is already set, the plot is dropped (the
    ///   protected pixel survives).
    /// - If `mask_set_on_draw` is on, the new pixel is OR'd with
    ///   bit 15 so subsequent mask checks protect it.
    ///
    /// Callers do their own draw-area clipping before calling this
    /// -- it's the hot per-pixel path and shouldn't re-check bounds.
    fn plot_pixel(&mut self, x: u16, y: u16, fg: u16, mode: BlendMode) {
        let existing = self.vram.get_pixel(x, y);
        if self.mask_check_before_draw && existing & 0x8000 != 0 {
            return;
        }
        let mut pixel = if mode == BlendMode::Opaque {
            fg
        } else {
            blend_pixel(existing, fg, mode)
        };
        if self.mask_set_on_draw {
            pixel |= 0x8000;
        }
        self.vram.set_pixel(x, y, pixel);
        // Stamp ownership for the pixel tracer if enabled. We hit
        // this every time a primitive writes a pixel, but the cost
        // is a single array write behind an Option check -- cheap
        // enough to keep on even in release diagnostic builds.
        if let Some(ref mut owner) = self.pixel_owner {
            owner[y as usize * VRAM_WIDTH + x as usize] = self.current_cmd_index;
        }
    }

    /// Rasterize a textured triangle -- same edge-function test as the
    /// other triangle paths, with nearest-neighbor texture sampling
    /// via barycentric-interpolated UV.
    ///
    /// `semi_trans` is the primitive's command-bit-1 state. When set,
    /// texels with bit 15 high blend via `self.tex_blend_mode`; texels
    /// with bit 15 clear still draw opaquely. When clear, every texel
    /// draws opaque regardless of its bit 15 -- matching PSX-SPX's
    /// per-texel semi-transparency rule.
    ///
    /// `tint` is the 24-bit vertex colour that modulates each texel
    /// (see [`modulate_tint`]). Raw-texture primitives (cmd bit 0 set)
    /// pass `(0x80, 0x80, 0x80)` so modulation is a no-op.
    #[allow(clippy::too_many_arguments)]
    fn rasterize_textured_triangle(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        t0: (u16, u16),
        t1: (u16, u16),
        t2: (u16, u16),
        clut_word: u16,
        semi_trans: bool,
        tint: (u32, u32, u32),
    ) {
        if self.wireframe_enabled {
            // Wireframe uses the first tint channel triple directly
            // for the outline colour (or white for raw-texture prims).
            let edge_rgb = tint.0 | (tint.1 << 8) | (tint.2 << 16);
            let colour = rgb24_to_bgr15(edge_rgb);
            self.rasterize_line(v0, v1, colour, colour, BlendMode::Opaque, false);
            self.rasterize_line(v1, v2, colour, colour, BlendMode::Opaque, false);
            self.rasterize_line(v2, v0, colour, colour, BlendMode::Opaque, false);
            let _ = (t0, t1, t2, clut_word, semi_trans);
            return;
        }
        if triangle_exceeds_hw_extent(v0, v1, v2) {
            return;
        }
        self.update_clut_if_needed(clut_word);
        let tpage_mode = self.tex_blend_mode;
        // Hardware dithers texture-blended (non-raw) polygons even
        // without Gouraud shading: a flat-tint textured prim still
        // gets the 4×4 ordered dither when GP0 0xE1 bit 9 is set.
        // Raw-texture prims (tint == identity) are never dithered.
        let dither = self.dither_enabled && tint != RAW_TEXTURE_TINT;

        let v_uv = [
            (t0.0 as i32, t0.1 as i32),
            (t1.0 as i32, t1.1 as i32),
            (t2.0 as i32, t2.1 as i32),
        ];
        let draw_top = self.draw_area_top as i32;
        let draw_bottom = self.draw_area_bottom as i32;
        let draw_left = self.draw_area_left as i32;
        let draw_right = self.draw_area_right as i32;
        for_each_tri_pixel(
            [v0, v1, v2],
            [(0, 0, 0); 3],
            v_uv,
            draw_top,
            draw_bottom,
            draw_left,
            draw_right,
            |x, y, _r, _g, _b, u, v| {
                if let Some(texel) = self.sample_texture(u as u16, v as u16) {
                    let shaded = if dither {
                        modulate_tint_dithered(texel, tint.0, tint.1, tint.2, x, y)
                    } else {
                        modulate_tint(texel, tint.0, tint.1, tint.2)
                    };
                    let mode = if semi_trans && (texel & 0x8000) != 0 {
                        tpage_mode
                    } else {
                        BlendMode::Opaque
                    };
                    self.plot_pixel(x as u16, y as u16, shaded, mode);
                }
            },
        );
    }

    /// Rasterize a triangle with per-vertex colours -- Gouraud shading.
    /// Same edge-function inside test as the flat path, but interpolates
    /// RGB using normalized barycentric weights `(w0, w1, w2)` per pixel
    /// and packs the result back into a 15-bit BGR VRAM word.
    #[allow(clippy::too_many_arguments)]
    fn rasterize_shaded_triangle(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        v2: (i32, i32),
        c0: u32,
        c1: u32,
        c2: u32,
        mode: BlendMode,
    ) {
        if self.wireframe_enabled {
            self.rasterize_line_shaded(v0, v1, c0, c1, mode);
            self.rasterize_line_shaded(v1, v2, c1, c2, mode);
            self.rasterize_line_shaded(v2, v0, c2, c0, mode);
            return;
        }
        if triangle_exceeds_hw_extent(v0, v1, v2) {
            return;
        }
        // Channel-extract closures -- r/g/b are low/mid/high bytes of the
        // 24-bit word written in the command.
        let r = |c: u32| (c & 0xFF) as i32;
        let g = |c: u32| ((c >> 8) & 0xFF) as i32;
        let b = |c: u32| ((c >> 16) & 0xFF) as i32;
        let v_rgb = [
            (r(c0), g(c0), b(c0)),
            (r(c1), g(c1), b(c1)),
            (r(c2), g(c2), b(c2)),
        ];
        let draw_top = self.draw_area_top as i32;
        let draw_bottom = self.draw_area_bottom as i32;
        let draw_left = self.draw_area_left as i32;
        let draw_right = self.draw_area_right as i32;
        let dither = self.dither_enabled;
        for_each_tri_pixel(
            [v0, v1, v2],
            v_rgb,
            [(0, 0); 3],
            draw_top,
            draw_bottom,
            draw_left,
            draw_right,
            |x, y, ri, gi, bi, _u, _v| {
                let colour = if dither {
                    dither_rgb(ri as i32, gi as i32, bi as i32, x, y)
                } else {
                    rgb24_to_bgr15((ri as u32) | ((gi as u32) << 8) | ((bi as u32) << 16))
                };
                self.plot_pixel(x as u16, y as u16, colour, mode);
            },
        );
    }

    // --- Lines (GP0 0x40..=0x5F) ---

    /// GP0 0x40..=0x47 -- single monochrome line. Packet: `[cmd+color, v0, v1]`.
    fn draw_line_mono_single(&mut self) {
        let cmd = self.gp0_fifo[0];
        let rgb24 = cmd & 0x00FF_FFFF;
        let color = rgb24_to_bgr15(rgb24);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let v1 = self.decode_vertex(self.gp0_fifo[2]);
        // Hardware dithers ALL lines (mono and shaded) when GP0 0xE1
        // bit 9 is set. Route a dithered mono line through the shaded
        // walker with both endpoints sharing the same 24-bit colour;
        // that path already applies per-pixel ordered dither. With
        // dither off, keep the parity-tuned mono Bresenham walk.
        if self.dither_enabled {
            self.rasterize_line_shaded(v0, v1, rgb24, rgb24, mode);
        } else {
            self.rasterize_line(v0, v1, color, color, mode, false);
        }
    }

    /// GP0 0x50..=0x57 -- single Gouraud-shaded line. Packet:
    /// `[cmd+c0, v0, c1, v1]` -- each endpoint carries its own colour
    /// word.
    fn draw_line_shaded_single(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let c1 = self.gp0_fifo[2] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        self.rasterize_line_shaded(v0, v1, c0, c1, mode);
    }

    /// GP0 0x48..=0x4F -- start a monochrome polyline. The initial
    /// packet has the same shape as a single line (cmd+color, v0,
    /// v1); after executing it we switch to receive mode.
    fn draw_line_mono_start_polyline(&mut self) {
        let cmd = self.gp0_fifo[0];
        let rgb24 = cmd & 0x00FF_FFFF;
        let color = rgb24_to_bgr15(rgb24);
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let v1 = self.decode_vertex(self.gp0_fifo[2]);
        // Dither the first segment too (see draw_line_mono_single).
        if self.dither_enabled {
            self.rasterize_line_shaded(v0, v1, rgb24, rgb24, mode);
        } else {
            self.rasterize_line(v0, v1, color, color, mode, false);
        }
        // Enter receive mode with `v1` as the starting point for
        // the next segment.
        self.polyline = Some(PolylineState::Mono {
            color,
            rgb24,
            mode,
            last_vertex: v1,
        });
        self.polyline_cmd_log_index = if self.cmd_log_enabled {
            self.cmd_log.len().checked_sub(1)
        } else {
            None
        };
    }

    /// GP0 0x58..=0x5F -- start a Gouraud polyline. Initial packet
    /// is `[cmd+c0, v0, c1, v1]`; after the first segment we
    /// enter receive mode waiting for alternating (color, vertex)
    /// pairs.
    fn draw_line_shaded_start_polyline(&mut self) {
        let cmd = self.gp0_fifo[0];
        let c0 = cmd & 0x00FF_FFFF;
        let mode = prim_blend_mode(cmd, self.tex_blend_mode);
        let v0 = self.decode_vertex(self.gp0_fifo[1]);
        let c1 = self.gp0_fifo[2] & 0x00FF_FFFF;
        let v1 = self.decode_vertex(self.gp0_fifo[3]);
        self.rasterize_line_shaded(v0, v1, c0, c1, mode);
        self.polyline = Some(PolylineState::Shaded {
            mode,
            last_color: c1,
            last_vertex: v1,
            awaiting_color: true,
            pending_color: 0,
        });
        self.polyline_cmd_log_index = if self.cmd_log_enabled {
            self.cmd_log.len().checked_sub(1)
        } else {
            None
        };
    }

    /// Consume one GP0 word while in polyline mode. Terminator
    /// pattern per PSX-SPX is `0x50005000` / `0x55555555` -- any
    /// word whose top bits match `0x5000_5000 >> 28 == 0x5` in
    /// both high and low halves means "end". We accept the
    /// canonical sentinels.
    fn ingest_polyline_word(&mut self, word: u32) {
        // Sentinel check -- both halves have the terminator pattern.
        // Redux uses `(word & 0xF000F000) == 0x50005000`.
        let is_term = (word & 0xF000_F000) == 0x5000_5000;
        if is_term {
            self.polyline = None;
            self.polyline_cmd_log_index = None;
            return;
        }
        // Append the continuation word to the start packet's cmd_log
        // entry so log consumers (HW renderer) see the whole
        // polyline. The terminator is deliberately NOT appended: the
        // logged fifo holds exactly the drawn data, and data words can
        // never match the sentinel (the check above would have ended
        // the polyline first).
        if let Some(log_index) = self.polyline_cmd_log_index {
            if let Some(entry) = self.cmd_log.get_mut(log_index) {
                entry.fifo.push(word);
            }
        }
        match self.polyline.as_mut().unwrap() {
            PolylineState::Mono {
                color,
                rgb24,
                mode,
                last_vertex,
            } => {
                let c = *color;
                let rgb = *rgb24;
                let m = *mode;
                let v0 = *last_vertex;
                let v1 = self.decode_vertex(word);
                if self.dither_enabled {
                    self.rasterize_line_shaded(v0, v1, rgb, rgb, m);
                } else {
                    self.rasterize_line(v0, v1, c, c, m, false);
                }
                if let Some(PolylineState::Mono { last_vertex, .. }) = self.polyline.as_mut() {
                    *last_vertex = v1;
                }
            }
            PolylineState::Shaded {
                mode,
                last_color,
                last_vertex,
                awaiting_color,
                pending_color,
            } => {
                if *awaiting_color {
                    *pending_color = word & 0x00FF_FFFF;
                    *awaiting_color = false;
                } else {
                    let c0 = *last_color;
                    let c1 = *pending_color;
                    let m = *mode;
                    let v0 = *last_vertex;
                    let v1 = self.decode_vertex(word);
                    self.rasterize_line_shaded(v0, v1, c0, c1, m);
                    if let Some(PolylineState::Shaded {
                        last_color,
                        last_vertex,
                        awaiting_color,
                        ..
                    }) = self.polyline.as_mut()
                    {
                        *last_color = c1;
                        *last_vertex = v1;
                        *awaiting_color = true;
                    }
                }
            }
        }
    }

    /// Rasterize a monochrome line with the PS1's 32.32 coordinate DDA.
    /// Its half-pixel seed, tiny negative bias, rounded coordinate
    /// gradients, and 11-bit position truncation are all visible in the
    /// public silicon line corpus.
    ///
    /// `_interpolate` is reserved for future shaded mode but kept
    /// here so the signature is stable.
    fn rasterize_line(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        c0: u16,
        _c1: u16,
        mode: BlendMode,
        _interpolate: bool,
    ) {
        for_each_line_pixel(v0, v1, |x, y, _step, _steps, _swapped| {
            self.plot_line_pixel(x, y, c0, mode);
        });
    }

    fn plot_line_pixel(&mut self, x: i32, y: i32, colour: u16, mode: BlendMode) {
        let (min_x, max_x) = (self.draw_area_left as i32, self.draw_area_right as i32);
        let (min_y, max_y) = (self.draw_area_top as i32, self.draw_area_bottom as i32);
        if (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y) {
            if self.wireframe_enabled {
                self.wire_plot_recorded(x as u16, y as u16, colour, mode);
            } else {
                self.plot_pixel(x as u16, y as u16, colour, mode);
            }
        }
    }

    /// Plot an edge pixel in wireframe mode, journaling it. Saves the
    /// pixel's pre-edge VRAM value the first time an edge claims it
    /// (later plots see our own edge colour, not the real content), and
    /// records what the plot actually wrote (read back post-plot, so
    /// dither/masking are accounted for) for the conditional restore.
    fn wire_plot_recorded(&mut self, x: u16, y: u16, colour: u16, mode: BlendMode) {
        let old = self.vram.get_pixel(x, y);
        self.plot_pixel(x, y, colour, mode);
        let written = self.vram.get_pixel(x, y);
        let key = ((y as u32) << 16) | x as u32;
        self.wire_saved.entry(key).or_insert((old, written)).1 = written;
        self.wire_pixels_current.push((x, y));
    }

    /// Rasterize a Gouraud-shaded line. Coordinates use the same 32.32
    /// silicon DDA as monochrome lines; colours use a separately
    /// truncated Q12 gradient with a +0.5 seed.
    fn rasterize_line_shaded(
        &mut self,
        v0: (i32, i32),
        v1: (i32, i32),
        c0: u32,
        c1: u32,
        mode: BlendMode,
    ) {
        let (min_x, max_x) = (self.draw_area_left as i32, self.draw_area_right as i32);
        let (min_y, max_y) = (self.draw_area_top as i32, self.draw_area_bottom as i32);
        for_each_line_pixel(v0, v1, |x, y, step, steps, swapped| {
            if (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y) {
                let (start, end) = if swapped { (c1, c0) } else { (c0, c1) };
                let channel = |shift: u32| {
                    let a = ((start >> shift) & 0xFF) as i32;
                    let b = ((end >> shift) & 0xFF) as i32;
                    let value = (a << 12) + (1 << 11) + (((b - a) << 12) / steps) * step;
                    value >> 12
                };
                let r = channel(0);
                let g = channel(8);
                let b = channel(16);
                let colour = if self.dither_enabled {
                    dither_rgb(r, g, b, x, y)
                } else {
                    rgb24_to_bgr15(
                        (r.clamp(0, 255) as u32)
                            | ((g.clamp(0, 255) as u32) << 8)
                            | ((b.clamp(0, 255) as u32) << 16),
                    )
                };
                if self.wireframe_enabled {
                    self.wire_plot_recorded(x as u16, y as u16, colour, mode);
                } else {
                    self.plot_pixel(x as u16, y as u16, colour, mode);
                }
            }
        });
    }

    // --- CPU→VRAM transfer (GP0 0xA0) ---

    /// GP0 0xA0 -- start a CPU-to-VRAM transfer. `[cmd, xy, wh]` is
    /// followed (in subsequent GP0 writes) by `ceil(w*h / 2)` words
    /// of 16bpp pixel data, 2 pixels per word. Transfer state lives
    /// in [`Gpu::vram_upload`] until every pixel has been ingested.
    fn begin_vram_upload(&mut self) {
        let xy = self.gp0_fifo[1];
        let wh = self.gp0_fifo[2];
        let x = (xy & 0x3FF) as u16;
        let y = ((xy >> 16) & 0x1FF) as u16;
        // Hardware uses a wrap-around convention: width/height of 0
        // means 1024 / 512 respectively. Matches Redux.
        let w = {
            let raw = (wh & 0x3FF) as u16;
            if raw == 0 {
                1024
            } else {
                raw
            }
        };
        let h = {
            let raw = ((wh >> 16) & 0x1FF) as u16;
            if raw == 0 {
                512
            } else {
                raw
            }
        };
        let pixels = w as u32 * h as u32;
        // Two 16bpp pixels per 32-bit word, round up.
        let remaining = pixels.div_ceil(2);
        self.vram_upload = Some(VramTransfer {
            x,
            y,
            w,
            h,
            row: 0,
            col: 0,
            remaining,
            cmd_log_index: if self.cmd_log_enabled {
                self.cmd_log.len().checked_sub(1)
            } else {
                None
            },
        });
    }

    /// Consume one word of pixel data for the active CPU→VRAM
    /// transfer. When `remaining` hits zero, the transfer closes and
    /// the next GP0 write is interpreted as a new command.
    fn ingest_vram_upload_word(&mut self, word: u32) {
        if let Some(log_index) = self.vram_upload.as_ref().and_then(|t| t.cmd_log_index) {
            if let Some(entry) = self.cmd_log.get_mut(log_index) {
                entry.fifo.push(word);
            }
        }
        // Per PSX-SPX the GP0(E6h) Set-Mask / Check-Mask bits apply to
        // CPU->VRAM uploads too. Capture them before borrowing the
        // transfer so the per-pixel writer can honour them.
        let mask_check = self.mask_check_before_draw;
        let mask_set = self.mask_set_on_draw;
        let done = {
            let Some(t) = self.vram_upload.as_mut() else {
                return;
            };
            let pix_a = word as u16;
            let pix_b = (word >> 16) as u16;
            Self::write_upload_pixel(t, pix_a, &mut self.vram, mask_check, mask_set);
            Self::write_upload_pixel(t, pix_b, &mut self.vram, mask_check, mask_set);
            t.remaining = t.remaining.saturating_sub(1);
            t.remaining == 0
        };
        if done {
            self.vram_upload = None;
        }
    }

    /// Place the next pixel in an active upload. Advances `col`; at
    /// the right edge wraps to the next `row`. Pixels past the final
    /// row are silently dropped (VRAM wrap on the destination only
    /// applies to coordinates, not to an over-long upload payload).
    fn write_upload_pixel(
        t: &mut VramTransfer,
        pixel: u16,
        vram: &mut Vram,
        mask_check: bool,
        mask_set: bool,
    ) {
        if t.row >= t.h {
            return;
        }
        let px = t.x.wrapping_add(t.col);
        let py = t.y.wrapping_add(t.row);
        // Honour GP0(E6h): with check-mask on, leave a destination pixel
        // whose bit15 is set untouched (but still advance the cursor); with
        // force-mask on, OR bit15 into the stored value. Matches PSX-SPX,
        // which exempts only GP0(02h) Fill from the mask bits.
        if !(mask_check && vram.get_pixel(px, py) & 0x8000 != 0) {
            let out = if mask_set { pixel | 0x8000 } else { pixel };
            vram.set_pixel(px, py, out);
        }
        t.col += 1;
        if t.col >= t.w {
            t.col = 0;
            t.row += 1;
        }
    }

    /// GP0 0x02 -- monochrome fill rectangle, ignores draw mode /
    /// clipping / blending. Writes `color` directly into VRAM.
    ///
    /// Packet layout (Redux `GPU::cmdFillRect`):
    ///   word 0: `0x02RRGGBB`      -- opcode + 24-bit RGB
    ///   word 1: `0xYYYYXXXX`      -- top-left: X is 16-pixel-aligned
    ///   word 2: `0xHHHHWWWW`      -- width is rounded up to 16 pixels
    ///
    /// Both coordinates and sizes wrap mod VRAM dimensions.
    fn fill_rect(&mut self) {
        let color24 = self.gp0_fifo[0] & 0x00FF_FFFF;
        let (x, y) = {
            let w = self.gp0_fifo[1];
            // X is aligned to 16-pixel boundaries; low 4 bits ignored.
            let x = (w & 0x3F0) as u16;
            let y = ((w >> 16) & 0x1FF) as u16;
            (x, y)
        };
        let (w, h) = {
            let s = self.gp0_fifo[2];
            // Width rounded up to next multiple of 16.
            let w = (((s & 0x3FF) + 0x0F) & !0x0F) as u16;
            let h = ((s >> 16) & 0x1FF) as u16;
            (w, h)
        };

        let color15 = rgb24_to_bgr15(color24);
        for row in 0..h {
            for col in 0..w {
                let px = (x + col) as usize % VRAM_WIDTH;
                let py = (y + row) as usize % VRAM_HEIGHT;
                self.vram.set_pixel(px as u16, py as u16, color15);
            }
        }
    }
}

fn scale_gpu_pixels(pixels: u64, numerator: u64, denominator: u64) -> u64 {
    pixels
        .saturating_mul(numerator)
        .saturating_add(denominator - 1)
        / denominator
}

/// Dominant-axis steps used by the GPU line engine, after its signed 11-bit
/// coordinate truncation and oversized-line rejection.
fn timing_line_steps(v0: (i32, i32), v1: (i32, i32)) -> u64 {
    let truncate = |v: i32| sign_extend_11(v & 0x7FF);
    let dx = (truncate(v1.0) - truncate(v0.0)).unsigned_abs();
    let dy = (truncate(v1.1) - truncate(v0.1)).unsigned_abs();
    if dx >= 1024 || dy >= 512 {
        0
    } else {
        u64::from(dx.max(dy))
    }
}

/// Clip a convex primitive to the drawing rectangle and return its area in
/// pixels. Coordinates are Q16.16 throughout so command timing remains fully
/// deterministic across hosts while still handling partially off-screen
/// triangles (the bandwidth corpus deliberately exercises 1/4 and 1/2 clips).
fn clipped_polygon_area(
    vertices: &[(i32, i32)],
    left: i32,
    top: i32,
    right_exclusive: i32,
    bottom_exclusive: i32,
) -> u64 {
    const FP: i64 = 1 << 16;
    let left = i64::from(left.max(0)) * FP;
    let top = i64::from(top.max(0)) * FP;
    let right = i64::from(right_exclusive.min(VRAM_WIDTH as i32)) * FP;
    let bottom = i64::from(bottom_exclusive.min(VRAM_HEIGHT as i32)) * FP;
    if left >= right || top >= bottom || vertices.len() < 3 {
        return 0;
    }

    let mut polygon: Vec<(i64, i64)> = vertices
        .iter()
        .map(|&(x, y)| (i64::from(x) * FP, i64::from(y) * FP))
        .collect();
    polygon = clip_timing_polygon(&polygon, true, left, true);
    polygon = clip_timing_polygon(&polygon, true, right, false);
    polygon = clip_timing_polygon(&polygon, false, top, true);
    polygon = clip_timing_polygon(&polygon, false, bottom, false);
    if polygon.len() < 3 {
        return 0;
    }

    let mut twice_area = 0i128;
    for i in 0..polygon.len() {
        let (x0, y0) = polygon[i];
        let (x1, y1) = polygon[(i + 1) % polygon.len()];
        twice_area += i128::from(x0) * i128::from(y1) - i128::from(x1) * i128::from(y0);
    }
    // `twice_area` is Q32.32 and contains twice the geometric area.
    // Add half a pixel before the shift so small fractional clips round
    // instead of systematically under-billing.
    ((twice_area.unsigned_abs() + (1u128 << 32)) >> 33) as u64
}

fn clip_timing_polygon(
    polygon: &[(i64, i64)],
    x_axis: bool,
    bound: i64,
    keep_greater: bool,
) -> Vec<(i64, i64)> {
    if polygon.is_empty() {
        return Vec::new();
    }
    let coord = |p: (i64, i64)| if x_axis { p.0 } else { p.1 };
    let inside = |p: (i64, i64)| {
        if keep_greater {
            coord(p) >= bound
        } else {
            coord(p) <= bound
        }
    };
    let intersect = |a: (i64, i64), b: (i64, i64)| {
        let ac = coord(a);
        let bc = coord(b);
        let den = bc - ac;
        if den == 0 {
            return a;
        }
        let num = bound - ac;
        let lerp = |av: i64, bv: i64| {
            av + ((i128::from(bv - av) * i128::from(num)) / i128::from(den)) as i64
        };
        if x_axis {
            (bound, lerp(a.1, b.1))
        } else {
            (lerp(a.0, b.0), bound)
        }
    };

    let mut out = Vec::with_capacity(polygon.len() + 1);
    let mut previous = polygon[polygon.len() - 1];
    let mut previous_inside = inside(previous);
    for &current in polygon {
        let current_inside = inside(current);
        if current_inside != previous_inside {
            out.push(intersect(previous, current));
        }
        if current_inside {
            out.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    out
}

/// Walk the PS1 line engine's coordinate DDA. The callback receives the
/// silicon-truncated pixel coordinate, current step, total step count, and
/// whether the hardware swapped endpoints to make X increase.
fn for_each_line_pixel(
    v0: (i32, i32),
    v1: (i32, i32),
    mut plot: impl FnMut(i32, i32, i32, i32, bool),
) {
    let truncate = |v: i32| sign_extend_11(v & 0x7FF);
    let mut p0 = (truncate(v0.0), truncate(v0.1));
    let mut p1 = (truncate(v1.0), truncate(v1.1));
    let dx_abs = (p1.0 - p0.0).abs();
    let dy_abs = (p1.1 - p0.1).abs();
    if dx_abs >= 1024 || dy_abs >= 512 {
        return;
    }

    let steps = dx_abs.max(dy_abs);
    let mut swapped = false;
    if p0.0 >= p1.0 && steps > 0 {
        std::mem::swap(&mut p0, &mut p1);
        swapped = true;
    }

    let divide_coord = |delta: i64| {
        if steps == 0 {
            return 0;
        }
        let rounding = if delta < 0 {
            -i64::from(steps - 1)
        } else if delta > 0 {
            i64::from(steps - 1)
        } else {
            0
        };
        ((delta << 32) + rounding) / i64::from(steps)
    };
    let dx = divide_coord(i64::from(p1.0 - p0.0));
    let dy = divide_coord(i64::from(p1.1 - p0.1));
    let mut x = (i64::from(p0.0) << 32) + (1i64 << 31) - 1024;
    let mut y = (i64::from(p0.1) << 32) + (1i64 << 31) - if dy < 0 { 1024 } else { 0 };

    let color_steps = steps.max(1);
    for step in 0..=steps {
        plot(
            truncate((x >> 32) as i32),
            truncate((y >> 32) as i32),
            step,
            color_steps,
            swapped,
        );
        x += dx;
        y += dy;
    }
}

/// Sign-extend an 11-bit integer (PS1 vertex coords + drawing offset
/// are 11-bit signed).
fn sign_extend_11(v: i32) -> i32 {
    if v & 0x400 != 0 {
        v | !0x7FF
    } else {
        v & 0x7FF
    }
}

fn trace_mono_rect_min_area() -> Option<i32> {
    static VALUE: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("PSOXIDE_TRACE_MONO_RECT_MIN_AREA")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
    })
}

fn trace_mono_rect_limit() -> usize {
    static VALUE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("PSOXIDE_TRACE_MONO_RECT_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(128)
    })
}

fn trace_mono_rect_count() -> usize {
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Transient state held between the start of a polyline primitive
/// (GP0 0x48..=0x4F or 0x58..=0x5F) and its terminator word. Each
/// variant carries the most recently-rasterised endpoint so the
/// next segment can chain from it.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
enum PolylineState {
    /// Monochrome polyline -- all segments use the same color.
    Mono {
        color: u16,
        /// Original command colour retained because dithered continuation
        /// segments operate on 24-bit channels, not the pre-quantized BGR15.
        rgb24: u32,
        mode: BlendMode,
        last_vertex: (i32, i32),
    },
    /// Gouraud polyline -- each segment interpolates between the
    /// prior vertex's colour and the next colour word. Polyline
    /// receive mode alternates between (color word, vertex word)
    /// pairs; `awaiting_color` tracks which half we're on.
    Shaded {
        mode: BlendMode,
        last_color: u32,
        last_vertex: (i32, i32),
        awaiting_color: bool,
        pending_color: u32,
    },
}

impl Default for Gpu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
