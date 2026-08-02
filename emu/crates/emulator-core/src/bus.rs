//! System bus: owns physical memory and dispatches loads to regions.
//!
//! Current coverage: RAM, BIOS, scratchpad. Everything else panics on
//! access -- deliberately, because we want unmapped reads to be loud
//! until each region's owning module (GPU, SPU, CD-ROM, …) is wired up.
//!
//! ## Provenance
//!
//! Portions of this module are parity-matched against, and in places
//! derived from, PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later. Points of
//! correspondence are flagged inline with `Redux` references. PSoXide is
//! released under GPL-2.0-or-later in part to honor this lineage; see
//! `LICENSE` and `docs/license-audit.md`.

use psx_hw::memory::{self, to_physical};
use thiserror::Error;

use crate::cdrom::CdRom;
use crate::dma::Dma;
use crate::gpu::Gpu;
use crate::irq::{Irq, IrqSource};
use crate::mmio_trace::{MmioKind, MmioTrace};
use crate::sio::Sio0;
use crate::sio1::Sio1;
use crate::spu::Spu;
use crate::telemetry::GuestTelemetry;
use crate::timers::Timers;

mod memory_timing;
mod timing;

pub(crate) use memory_timing::AccessWidth;
use memory_timing::MemoryControl;
use timing::*;

/// Physical address of `I_STAT` (interrupt status / ack register).
const IRQ_STAT_ADDR: u32 = 0x1F80_1070;
/// Physical address of `I_MASK` (interrupt enable register).
const IRQ_MASK_ADDR: u32 = 0x1F80_1074;

/// `#[serde(with = "big_bytes")]` for boxed fixed-size byte buffers
/// (`Bus::ram`, `Bus::io`). Plain `Box<[u8; N]>: Serialize` would work
/// too (serde's array impl covers arbitrary `N` via const generics),
/// but it walks the array element-by-element through `serialize_seq`;
/// routing through `serde_bytes` instead hits `serialize_bytes`, which
/// postcard encodes as a flat length-prefixed byte run instead of one
/// varint-tagged element at a time -- worth it once `N` reaches
/// megabytes (`Bus::ram` is 2 MiB). `N` is inferred at each call site
/// from the field's declared array length, so one generic module
/// covers every boxed byte-array field on `Bus`.
mod big_bytes {
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S, const N: usize>(bytes: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde_bytes::serialize(bytes.as_slice(), serializer)
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<Box<[u8; N]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let buf = serde_bytes::ByteBuf::deserialize(deserializer)?;
        buf.into_vec()
            .into_boxed_slice()
            .try_into()
            .map_err(|v: Box<[u8]>| {
                serde::de::Error::custom(format!(
                    "save-state buffer has {} bytes, expected {N}",
                    v.len()
                ))
            })
    }
}

/// `#[serde(default = ...)]` target for the skipped [`Bus::bios`]
/// field -- `Box<[u8; N]>` has no `Default` impl for `N` this large
/// (std's array `Default` only goes up to 32), so a zeroed literal
/// stands in until the frontend load path overwrites it with the real
/// BIOS bytes.
fn default_bios() -> Box<[u8; memory::bios::SIZE]> {
    Box::new([0; memory::bios::SIZE])
}

fn default_dram_refresh_deadline() -> u64 {
    memory_timing::DRAM_REFRESH_PERIOD_CYCLES
}

/// Errors constructing a [`Bus`].
#[derive(Error, Debug)]
pub enum BusError {
    /// BIOS image was not exactly 512 KiB.
    #[error("BIOS image must be exactly {expected} bytes, got {actual}")]
    BiosSize {
        /// Expected size in bytes.
        expected: usize,
        /// Size that was actually provided.
        actual: usize,
    },
}

/// The PS1 system bus.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Bus {
    #[serde(with = "big_bytes")]
    ram: Box<[u8; memory::ram::SIZE]>,
    /// Excluded from save states: the BIOS image is load-time
    /// configuration (whatever `.bin` the frontend pointed at), not
    /// mutable game state, and embedding a 512 KiB copy in every save
    /// file would be pure waste. `Default` gives an all-zero
    /// placeholder on deserialize; the frontend load path immediately
    /// overwrites it with the currently-running `Bus`'s real BIOS
    /// bytes before the restored `Bus` is used for anything.
    #[serde(skip, default = "default_bios")]
    bios: Box<[u8; memory::bios::SIZE]>,
    #[serde(with = "big_bytes")]
    scratchpad: Box<[u8; memory::scratchpad::SIZE]>,
    /// Write-echoes-on-read buffer for the MMIO window. **Placeholder.**
    /// Individual peripherals with real semantics (IRQ below, later GPU /
    /// SPU / CD-ROM / DMA / timers) intercept their own ranges ahead of
    /// this fallback; the rest of MMIO still round-trips writes to reads.
    #[serde(with = "big_bytes")]
    io: Box<[u8; memory::io::SIZE]>,
    /// Last value driven on the CPU data bus. On-die registers narrower than
    /// a word leave undriven lanes intact; instruction-cache refills and CPU
    /// stores also update this latch, making those open-bus bits observable.
    data_bus_latch: u32,
    /// Programmable external-bus wait states and the nine memory-control
    /// registers at `0x1F80_1000..=0x1F80_1020`.
    memory_control: MemoryControl,
    /// Interrupt controller (`I_STAT` / `I_MASK`). Accessed via the MMIO
    /// dispatch below and queried by the CPU each step to update
    /// `COP0.CAUSE.IP[2]`.
    irq: Irq,
    /// Root counters (Timer 0 / 1 / 2). Phase 2e is register-backing
    /// only; ticking lands with the cycle model.
    pub timers: Timers,
    /// DMA controller (7 channels + DPCR + DICR). Phase 2g is
    /// register-backing only; transfers land as subsystems come online.
    dma: Dma,
    /// GPU -- owns VRAM and handles the GP0/GP1 MMIO ports. The
    /// frontend's VRAM viewer reads `bus.gpu.vram` directly.
    pub gpu: Gpu,
    /// SPU -- full 24-voice ADPCM synthesis, ADSR envelopes, stereo
    /// mixing at 44.1 kHz. Output drains into `spu.audio_out`; the
    /// frontend pulls samples every frame via [`Spu::drain_audio`].
    /// Public so the frontend can access the audio queue + tests can
    /// inspect voice state directly.
    pub spu: Spu,
    /// SIO0 -- controller / memory-card port. Currently models a cold
    /// port with nothing connected; enough to satisfy BIOS init polls.
    sio0: Sio0,
    /// General-purpose serial port at `0x1F80_1050`.
    sio1: Sio1,
    /// CD-ROM controller -- byte-granular MMIO at 0x1F80_1800..=0x1803.
    /// Exposed public so diagnostics can inspect FIFO / command state.
    pub cdrom: CdRom,
    /// Motion decoder. Defensive stub today -- register shape is
    /// faithful (idle / empty status, DMA-enable latching, reset)
    /// but no real Huffman / IDCT / YUV→RGB. Games that poll MDEC
    /// status see plausible values instead of the unmapped 0xFFFF_FFFF.
    pub mdec: crate::mdec::Mdec,
    /// Cumulative CPU cycles retired since reset. Fed by `Cpu::step`
    /// via [`Bus::tick`]. Peripherals read this to schedule events
    /// (VBlank, timer ticks, DMA completion). Phase 4a just counts;
    /// Phase 4b starts firing IRQs off it.
    cycles: u64,
    /// Absolute CPU-cycle edge of the next 44.1 kHz SPU sample. Keeping this
    /// phase on the emulated bus avoids losing the fractional remainder when
    /// BIOS warmup hands execution to a frontend or fast-booted executable.
    #[serde(default = "default_spu_sample_deadline")]
    spu_sample_deadline: u64,
    /// Enable the short SPU-DMA acceptance aperture measured on the late PAL
    /// SCPH-9902. Earlier consoles accept a full DMA burst immediately after
    /// SPUCNT changes, which commercial games rely on during scene changes.
    #[serde(default)]
    scph_9902_spu_dma_aperture: bool,
    /// Completion point of the single-entry CPU main-RAM write buffer.
    #[serde(default)]
    ram_write_buffer_ready_cycle: u64,
    /// Absolute divider deadline for the next DRAM refresh request.
    #[serde(default = "default_dram_refresh_deadline")]
    dram_refresh_deadline: u64,
    /// Issue cycle of the previous CPU main-RAM access. Refresh arbitration
    /// distinguishes the eight- and nine-clock load streams measured on
    /// silicon.
    #[serde(default)]
    last_cpu_ram_access_cycle: u64,
    // VBlank scheduling lives in `scheduler` under
    // [`EventSlot::VBlank`]. Seeded at `FIRST_VBLANK_CYCLE` by
    // `Bus::new`; every VBlank handler invocation re-schedules the
    // next one `VBLANK_PERIOD_CYCLES` later.
    /// Unified event scheduler -- the 15-slot queue that owns
    /// DMA / CDROM / VBlank / SPU / MDEC / SIO timings, matching
    /// Redux's `m_regs.interrupt` + `intTargets`. See
    /// [`crate::scheduler`] for the model.
    ///
    /// Migration status: DMA channel completions (slots `GpuDma`,
    /// `GpuOtcDma`, `CdrDma`, `MdecInDma`, `MdecOutDma`, `SpuDma`)
    /// run through the scheduler. VBlank, CDROM command / read
    /// events, SPU async, SIO -- still on their legacy per-subsystem
    /// timers; migrations land in follow-up commits.
    pub scheduler: crate::scheduler::Scheduler,
    /// Bounded ring buffer of recent MMIO accesses. Zero-sized and
    /// no-op at every call site unless the `trace-mmio` Cargo feature
    /// is enabled -- see `mmio_trace.rs` for the rationale. Debug
    /// tooling -- excluded from save states.
    #[serde(skip)]
    pub mmio_trace: MmioTrace,
    /// Out-of-band profiler/debug telemetry emitted by instrumented
    /// homebrew through the Expansion 2 debug port. Debug tooling --
    /// excluded from save states.
    #[serde(skip)]
    pub telemetry: GuestTelemetry,
    /// When true, the CPU replaces fetches at `0xA0` / `0xB0` / `0xC0`
    /// with a host-Rust implementation of the BIOS syscall they
    /// dispatch to. Off by default so parity tests stay bit-exact
    /// against Redux (which does the real BIOS ROM dispatch).
    /// Turned on by [`Bus::enable_hle_bios`] -- typically right after
    /// side-loading an EXE that wants BIOS services but skipped the
    /// BIOS's own init.
    pub hle_bios_enabled: bool,
    /// Per-(table, func) count of HLE BIOS calls. Diagnostic only.
    /// `[table][func]` where table is 0=A, 1=B, 2=C. Excluded from
    /// save states.
    #[serde(skip, default = "default_hle_bios_calls")]
    hle_bios_calls: [[u32; 256]; 3],
    /// Guest `JumpBuffer` registered through BIOS B(19h) `HookEntryInt`.
    /// Side-loaded executables do not have a retail kernel to remember and
    /// invoke this hook, so the HLE path retains the guest pointer and the CPU
    /// uses it when a real emulated hardware IRQ is taken.
    #[serde(default)]
    hle_irq_jump_buffer: Option<u32>,
    /// HSync cycles for the current video region (NTSC = 2172,
    /// PAL = 2167). Used by the timer bank's HBlank source and by
    /// the VBlank scheduler. Flipped by [`Bus::set_pal_mode`];
    /// defaults to NTSC for existing parity tests.
    hsync_cycles: u64,
    /// HSync cadence used by the VBlank scheduler. Redux changes
    /// the active PAL/NTSC thresholds on GP1 display-mode writes,
    /// but the base counter's target can retain its previous cadence;
    /// keeping this separate from Timer 1's HBlank source preserves
    /// that phase.
    vblank_hsync_cycles: u64,
    /// VBlank period in cycles -- one full non-interlaced field at the
    /// current video region. 571_236 for NTSC, 680_438 for PAL at the
    /// canonical physical scanline periods.
    vblank_period: u64,
    /// Addresses we've already logged as unmapped reads. Keeps
    /// log noise bounded when a buggy game pokes a bad pointer
    /// in a tight loop. Excluded from save states.
    #[serde(skip)]
    unmapped_read_seen: std::collections::BTreeSet<u32>,
    /// Same, writes. Separate so read + write to the same bad
    /// address both log at least once. Excluded from save states.
    #[serde(skip)]
    unmapped_write_seen: std::collections::BTreeSet<u32>,
    /// Diagnostic: when true, every DMA-completion schedule pushes
    /// a record to `dma_log`. Off by default -- only the
    /// `probe_dma_schedules` example flips it on. Excluded from save
    /// states.
    #[serde(skip)]
    dma_log_enabled: bool,
    #[serde(skip)]
    dma_log: Vec<(String, u64, u64, u64)>,
    /// Optional linked-list GPU DMA node trace used to diagnose ordering-table
    /// corruption/order changes without perturbing guest code generation.
    #[serde(skip)]
    gpu_linked_list_log_enabled: bool,
    #[serde(skip)]
    gpu_linked_list_transfer: u32,
    #[serde(skip)]
    gpu_linked_list_log: Vec<(u32, u32, u32)>,
}

/// `#[serde(default = ...)]` target for the skipped
/// [`Bus::hle_bios_calls`] field -- the outer `[T; 3]` would satisfy
/// `Default` on its own, but `T = [u32; 256]` doesn't, so the whole
/// nested array needs an explicit zeroed literal.
fn default_hle_bios_calls() -> [[u32; 256]; 3] {
    [[0; 256]; 3]
}

fn default_spu_sample_deadline() -> u64 {
    crate::spu::SAMPLE_CYCLES
}

impl Bus {
    /// Build a bus with the given BIOS image. RAM and scratchpad are
    /// zero-initialised; hardware leaves them in an undefined state, but
    /// zeroing is deterministic and adequate for a cold-boot harness.
    pub fn new(bios: Vec<u8>) -> Result<Self, BusError> {
        if bios.len() != memory::bios::SIZE {
            return Err(BusError::BiosSize {
                expected: memory::bios::SIZE,
                actual: bios.len(),
            });
        }

        let bios_arr: Box<[u8; memory::bios::SIZE]> = bios
            .into_boxed_slice()
            .try_into()
            .expect("size was just checked");

        let mut bus = Self {
            ram: zeroed_box(),
            bios: bios_arr,
            scratchpad: zeroed_box(),
            io: zeroed_box(),
            data_bus_latch: u32::MAX,
            memory_control: MemoryControl::default(),
            irq: Irq::new(),
            timers: Timers::new(),
            dma: Dma::new(),
            gpu: Gpu::new(),
            spu: Spu::new(),
            sio0: Sio0::new(),
            sio1: Sio1::new(),
            cdrom: CdRom::new(),
            mdec: crate::mdec::Mdec::new(),
            cycles: 0,
            spu_sample_deadline: default_spu_sample_deadline(),
            scph_9902_spu_dma_aperture: false,
            ram_write_buffer_ready_cycle: 0,
            dram_refresh_deadline: default_dram_refresh_deadline(),
            last_cpu_ram_access_cycle: 0,
            scheduler: {
                let mut s = crate::scheduler::Scheduler::new();
                // Seed the first VBlank at scanline 243. Every fire
                // of `EventSlot::VBlank` in `drain_scheduler_events`
                // reschedules the next one.
                s.schedule(crate::scheduler::EventSlot::VBlank, 0, FIRST_VBLANK_CYCLE);
                // NOTE: SPU scheduler seed is deliberately *not* here.
                // Reason: Redux's SPU runs in a detached `std::thread`
                // that doesn't run during the parity-oracle trace. If
                // we tick the SPU on every 768th cycle during the same
                // window, our ADSR advances (envelope non-zero,
                // voice_on_cycle bookkeeping) while Redux's stays
                // frozen -- and downstream SPU reads diverge. Until
                // the parity oracle learns to pump Redux's SPU thread
                // synchronously, we leave SPU synthesis dormant during
                // CPU execution and pump it on demand from the
                // frontend's per-frame audio callback instead. See
                // `Spu::seed_scheduler` + `Bus::run_spu_samples`.
                s
            },
            mmio_trace: MmioTrace::new(),
            telemetry: GuestTelemetry::new(),
            hle_bios_enabled: false,
            hle_bios_calls: [[0; 256]; 3],
            hle_irq_jump_buffer: None,
            hsync_cycles: HSYNC_CYCLES_NTSC,
            vblank_hsync_cycles: HSYNC_CYCLES_NTSC,
            vblank_period: VBLANK_PERIOD_CYCLES_NTSC,
            unmapped_read_seen: std::collections::BTreeSet::new(),
            unmapped_write_seen: std::collections::BTreeSet::new(),
            dma_log_enabled: false,
            dma_log: Vec::new(),
            gpu_linked_list_log_enabled: false,
            gpu_linked_list_transfer: 0,
            gpu_linked_list_log: Vec::new(),
        };

        bus.maybe_poison_memory();
        Ok(bus)
    }

    /// Debug aid for hardware-only bugs. Real hardware powers up with garbage
    /// in RAM and VRAM; the emulator zeroes them, which hides guest code that
    /// reads memory before writing it (an uninitialized read returns 0 here but
    /// noise on silicon). Setting `PSOXIDE_POISON_MEM` fills RAM, scratchpad,
    /// and VRAM with a non-zero pattern (a hex byte like `0xAA`, or `1` for the
    /// 0xAA default) so those reads surface as visible corruption. No-op when
    /// the env var is unset, so normal/parity runs stay deterministic.
    fn maybe_poison_memory(&mut self) {
        let Ok(spec) = std::env::var("PSOXIDE_POISON_MEM") else {
            return;
        };
        let byte = match spec.trim() {
            "" | "1" | "true" | "yes" => 0xAA,
            s => u8::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0xAA),
        };
        self.ram.fill(byte);
        self.scratchpad.fill(byte);
        let halfword = (u16::from(byte) << 8) | u16::from(byte);
        self.gpu.vram.words_mut().fill(halfword);
        eprintln!("[poison] RAM + scratchpad + VRAM filled with 0x{byte:02x}");
    }

    /// Build a deterministic bus for HLE-BIOS side-loaded homebrew.
    ///
    /// The synthetic BIOS contains the retail reset-vector word and is zero
    /// elsewhere because execution starts from a PSX-EXE entry point. Keeping
    /// the architecturally visible first word makes ROM probes agree with
    /// hardware without pretending to provide copyrighted firmware. Callers
    /// must enable HLE BIOS dispatch before running guests that use the
    /// `0xA0` / `0xB0` / `0xC0` syscall tables. Do not use this for
    /// retail disc boot or parity checks against a real BIOS.
    pub fn new_without_bios() -> Self {
        let mut bios = vec![0u8; memory::bios::SIZE];
        bios[..4].copy_from_slice(&0x3C08_0013u32.to_le_bytes());
        Self::new(bios).expect("synthetic BIOS size is fixed")
    }

    /// Switch to PAL video timing: 50 Hz refresh, 314-scanline
    /// frames, 2167 HSync cycles. Resets the VBlank scheduler to
    /// the PAL first-VBlank cycle + period, and reconfigures the
    /// HBlank tick rate for Timer 1. PAL retail games select this
    /// through GP1 display-mode writes; NTSC is the reset default.
    ///
    /// Calling this after any stepping has already happened leaves
    /// cumulative `cycles` in place -- the next VBlank will still
    /// fire at the correct *frame* boundary even if mid-frame.
    pub fn set_pal_mode(&mut self) {
        self.set_video_region(true);
    }

    /// Switch to NTSC video timing: approximately 59.8 Hz refresh,
    /// 263-scanline frames, 2172 HSync cycles.
    pub fn set_ntsc_mode(&mut self) {
        self.set_video_region(false);
    }

    fn set_video_region(&mut self, pal: bool) {
        let old = current_video_params(self.vblank_hsync_cycles, self.vblank_period);
        let (hsync, first_vblank, canonical_period, start_scanline, total_scanlines) = if pal {
            (
                HSYNC_CYCLES_PAL,
                FIRST_VBLANK_CYCLE_PAL,
                VBLANK_PERIOD_CYCLES_PAL,
                VBLANK_START_SCANLINE_PAL,
                HSYNC_TOTAL_PAL,
            )
        } else {
            (
                HSYNC_CYCLES_NTSC,
                FIRST_VBLANK_CYCLE_NTSC,
                VBLANK_PERIOD_CYCLES_NTSC,
                VBLANK_START_SCANLINE_NTSC,
                HSYNC_TOTAL_NTSC,
            )
        };
        if self.hsync_cycles == hsync {
            return;
        }
        let vblank_hsync = if self.cycles == 0 {
            hsync
        } else {
            old.map(|old| old.hsync).unwrap_or(hsync)
        };
        let period = if self.cycles == 0 {
            canonical_period
        } else {
            vblank_hsync.saturating_mul(total_scanlines)
        };
        let delay = if self.cycles == 0 {
            first_vblank
        } else {
            let current_scanline = old
                .and_then(|old| {
                    self.scheduler
                        .target(crate::scheduler::EventSlot::VBlank)
                        .map(|t| (old, t))
                })
                .map(|(old, next_vblank)| {
                    estimate_current_scanline(
                        self.cycles,
                        next_vblank,
                        old.period,
                        old.hsync,
                        old.start_scanline,
                        old.total_scanlines,
                    )
                })
                .unwrap_or(0)
                % total_scanlines;
            let line_phase = old
                .and_then(|old| {
                    self.scheduler
                        .target(crate::scheduler::EventSlot::VBlank)
                        .map(|t| (old, t))
                })
                .map(|(old, next_vblank)| {
                    estimate_scanline_phase(self.cycles, next_vblank, old.period, old.hsync)
                })
                .unwrap_or(0);
            let remaining_lines = if current_scanline < start_scanline {
                start_scanline - current_scanline
            } else {
                total_scanlines - current_scanline + start_scanline
            };
            // Redux's base counter target is not recalculated by the
            // display-mode write, so the next VBlank remains aligned
            // to the old hsync cadence while using the new region's
            // VBlank-start scanline.
            remaining_lines
                .saturating_mul(vblank_hsync)
                .saturating_sub(line_phase)
                .saturating_add(4)
                .max(1)
        };
        self.hsync_cycles = hsync;
        self.vblank_hsync_cycles = vblank_hsync;
        self.vblank_period = period;
        self.scheduler.cancel(crate::scheduler::EventSlot::VBlank);
        // Preserve current scanline phase across the region switch.
        // Redux's auto-video path changes the active video setting
        // from GP1 display-mode writes, but the counter phase keeps
        // marching; restarting from scanline 0 makes PAL games miss
        // the next VBlank by a large fraction of a frame.
        self.scheduler
            .schedule(crate::scheduler::EventSlot::VBlank, self.cycles, delay);
    }

    /// Current HSync period in CPU cycles -- NTSC = 2172, PAL = 2167.
    pub fn hsync_cycles(&self) -> u64 {
        self.hsync_cycles
    }

    /// Select the late PAL PSone memory-controller profile measured on an
    /// SCPH-9902. This is applied after a substitute earlier PAL BIOS warmup,
    /// whose own register programming reflects different motherboard timing.
    pub fn apply_scph_9902_profile(&mut self) {
        self.memory_control = MemoryControl::default();
        self.scph_9902_spu_dma_aperture = true;
        self.spu.apply_scph_9902_profile();
        self.timers.set_vblank_sync_offset_lines(29);
        self.timers.set_counter_read_extra_hold(0, 8);
        self.timers.set_timer0_dot_read_extra_hold(5);
        self.timers.set_counter_read_extra_hold(2, 5);
    }

    /// Restore the observable retail BIOS shell SPU handoff used by warm disc
    /// fast boot after the abbreviated kernel warmup skips the shell itself.
    pub fn apply_retail_bios_shell_audio_profile(&mut self) {
        self.spu.apply_retail_bios_shell_audio_profile();
    }

    /// Current VBlank period -- one frame in cycles.
    pub fn vblank_period(&self) -> u64 {
        self.vblank_period
    }

    /// Turn on HLE BIOS interception. Call after side-loading an EXE
    /// that expects BIOS services to be live without running the real
    /// BIOS boot sequence. Never enable when validating parity -- the
    /// oracle emulator runs the real BIOS ROM and will diverge.
    pub fn enable_hle_bios(&mut self) {
        self.hle_bios_enabled = true;
        self.hle_irq_jump_buffer = None;
        // The retail kernel publishes a Process** at 0x108. Side-loading an
        // EXE skips that initialization, so provide a reserved low-RAM
        // process/thread pair for homebrew exception hooks. The guest remains
        // free to replace the unresolved-handler pointer at 0x300.
        self.write32(
            crate::hle_bios::PROCESS_LIST_PTR,
            crate::hle_bios::SYNTHETIC_PROCESS,
        );
        self.write32(
            crate::hle_bios::SYNTHETIC_PROCESS,
            crate::hle_bios::SYNTHETIC_THREAD,
        );
    }

    pub(crate) fn set_hle_irq_jump_buffer(&mut self, pointer: Option<u32>) {
        self.hle_irq_jump_buffer = pointer.filter(|pointer| *pointer != 0);
    }

    pub(crate) fn hle_irq_jump_buffer(&self) -> Option<u32> {
        self.hle_irq_jump_buffer
    }

    /// Plug a digital controller into port 1 so homebrew / commercial
    /// games can poll for button state. Convenience: most single-player
    /// games use port 1 only.
    pub fn attach_digital_pad_port1(&mut self) {
        let old = std::mem::take(self.sio0.port1_mut());
        let memcard = old.into_memcard();
        let mut device = crate::pad::PortDevice::empty().with_pad(crate::pad::DigitalPad::new());
        if let Some(card) = memcard {
            device = device.with_memcard(card);
        }
        self.sio0.attach_port1(device);
    }

    /// Plug an original digital-only controller into port 1. Unlike the
    /// default DualShock-compatible pad, this device ignores analog-mode
    /// negotiation and always reports ID 0x41.
    pub fn attach_original_digital_pad_port1(&mut self) {
        let old = std::mem::take(self.sio0.port1_mut());
        let memcard = old.into_memcard();
        let mut device =
            crate::pad::PortDevice::empty().with_pad(crate::pad::DigitalPad::new_digital_only());
        if let Some(card) = memcard {
            device = device.with_memcard(card);
        }
        self.sio0.attach_port1(device);
    }

    /// Immutable access to SIO0 for diagnostics.
    pub fn sio0(&self) -> &Sio0 {
        &self.sio0
    }

    /// Plug a memory card into port 1 with the given backing
    /// contents (128 KiB buffer, typically loaded from a
    /// `.mcd` file). Pass an empty `Vec` to start with a fresh
    /// card. Keeps any pad already attached -- real hardware
    /// multiplexes pad + memcard on the same port.
    pub fn attach_memcard_port1(&mut self, initial_bytes: Vec<u8>) {
        let card = if initial_bytes.len() == crate::pad::MEMCARD_SIZE {
            crate::pad::MemoryCard::from_bytes(initial_bytes)
        } else {
            crate::pad::MemoryCard::new()
        };
        // Preserve any existing pad.
        let pad = std::mem::take(self.sio0.port1_mut()).into_pad();
        let mut device = crate::pad::PortDevice::empty().with_memcard(card);
        if let Some(p) = pad {
            device = device.with_pad(p);
        }
        self.sio0.attach_port1(device);
    }

    /// Splice in the two fields a save-state load deliberately leaves
    /// blank -- the BIOS image and the mounted disc -- from `donor`, a
    /// `Bus` the frontend already had to freshly construct (via its
    /// normal per-`GameKind` boot path) in order to know which BIOS
    /// bytes and which disc image belong to this save. Neither field
    /// is serialized (see the `#[serde(skip)]` docs on
    /// [`Bus`]'s `bios` field and on [`crate::cdrom::CdRom`]'s `disc`
    /// field), so a restored `Bus` is unusable until this runs.
    ///
    /// Takes `donor` by `&mut` to move the disc out of it rather than
    /// clone a potentially 700+ MB image; `donor` is left with no
    /// disc mounted afterward and should be discarded by the caller.
    pub fn restore_excluded_from(&mut self, donor: &mut Bus) {
        self.bios = donor.bios.clone();
        self.cdrom
            .restore_disc_for_savestate(donor.cdrom.take_disc());
    }

    /// Remove any memory card from port 1 while preserving the pad.
    pub fn detach_memcard_port1(&mut self) {
        let device = std::mem::take(self.sio0.port1_mut()).without_memcard();
        self.sio0.attach_port1(device);
    }

    /// Snapshot the port-1 memcard bytes for persistence. `None`
    /// when there's no card on port 1 or the card hasn't been
    /// written since load.
    pub fn memcard_port1_snapshot(&mut self) -> Option<Vec<u8>> {
        let card = self.sio0.port1_mut().memcard_mut()?;
        if !card.is_dirty() {
            return None;
        }
        let bytes = card.as_bytes().to_vec();
        card.clear_dirty();
        Some(bytes)
    }

    /// Update the buttons currently held on the port-1 controller.
    /// Called by the frontend each frame from the keyboard state.
    pub fn set_port1_buttons(&mut self, buttons: crate::pad::ButtonState) {
        self.sio0.set_port1_buttons(buttons);
    }

    /// Enable/disable the slow original-controller (SCPH-1200) timing model on
    /// SIO0. When on, a guest poll that clocks bytes without waiting for each
    /// `/ACK` pulse desyncs, reproducing the hardware failure headlessly.
    pub fn set_slow_pad(&mut self, slow: bool) {
        self.sio0.set_slow_pad(slow);
    }

    /// Update the analog-stick positions on the port-1
    /// controller. Each axis is `0..=255` with `0x80` = centre.
    /// No-op when no pad is attached to port 1. The stick values
    /// are only observed by games once the pad is in Analog mode
    /// (which they enter via the DualShock config protocol).
    pub fn set_port1_sticks(&mut self, right_x: u8, right_y: u8, left_x: u8, left_y: u8) {
        if let Some(pad) = self.sio0.port1_mut().pad_mut() {
            pad.set_sticks(right_x, right_y, left_x, left_y);
        }
    }

    /// Simulate pressing the port-1 DualShock Analog button.
    /// Returns `true` when the pad accepted the toggle.
    pub fn press_port1_analog_button(&mut self) -> bool {
        self.sio0.port1_mut().press_analog_button()
    }

    /// Force the port-1 DualShock into Analog mode without toggling
    /// back to Digital if it is already Analog.
    pub fn force_port1_analog_mode(&mut self) -> bool {
        self.sio0.port1_mut().force_analog_mode()
    }

    /// Current port-1 pad mode, if a pad is attached.
    pub fn port1_pad_mode(&self) -> Option<crate::pad::PadMode> {
        self.sio0.port1().pad().map(|pad| pad.mode())
    }

    /// Snapshot of the port-1 DualShock vibration-motor state:
    /// `(small_on, big_strength)` where `small_on` is a binary
    /// on/off and `big_strength` is 0..=255. Returns `(false, 0)`
    /// when no pad is attached to port 1. Frontend drives host
    /// haptics from this each frame.
    pub fn port1_motor_state(&self) -> (bool, u8) {
        self.sio0
            .port1()
            .pad()
            .map(|p| p.motor_state())
            .unwrap_or((false, 0))
    }

    /// Histogram of pad command bytes observed on port 1 since boot.
    /// `None` when no controller is attached.
    pub fn port1_pad_command_histogram(&self) -> Option<&[u32; 256]> {
        self.sio0.port1().pad().map(|p| p.command_histogram())
    }

    /// Recent pad command bytes seen on port 1, oldest first.
    pub fn port1_pad_recent_commands(&self) -> Vec<u8> {
        self.sio0
            .port1()
            .pad()
            .map(|p| p.recent_commands())
            .unwrap_or_default()
    }

    /// Histogram of memory-card command bytes observed on port 1.
    pub fn port1_memcard_command_histogram(&self) -> Option<&[u32; 256]> {
        self.sio0.port1().memcard().map(|m| m.command_histogram())
    }

    /// Recent memory-card protocol events seen on port 1.
    pub fn port1_memcard_recent_events(&self) -> Vec<crate::pad::MemcardEvent> {
        self.sio0
            .port1()
            .memcard()
            .map(|m| m.recent_events())
            .unwrap_or_default()
    }

    /// Histogram of transaction-leading bytes seen on SIO0 port 1.
    pub fn port1_first_byte_histogram(&self) -> &[u32; 256] {
        self.sio0.port1().first_byte_histogram()
    }

    /// Recent transaction-leading bytes seen on SIO0 port 1.
    pub fn port1_recent_first_bytes(&self) -> Vec<u8> {
        self.sio0.port1().recent_first_bytes()
    }

    /// Completed `0x42` poll transactions on port 1 since power-on. This is
    /// the guest's input clock, not the host's: it advances once per pad read
    /// however long the game took to get there. Poll-bound input tapes
    /// (`PXITAPE2`) index off it so a replay follows the same route whatever
    /// the frame rate.
    /// Sectors the CD controller read that software never collected. See
    /// [`crate::cdrom::CdRom::dropped_sectors`].
    pub fn cdrom_dropped_sectors(&self) -> u64 {
        self.cdrom.dropped_sectors()
    }

    /// Disc positions of the first and last dropped sector.
    pub fn cdrom_dropped_lba_range(&self) -> (u32, u32) {
        self.cdrom.dropped_lba_range()
    }

    pub fn port1_completed_polls(&self) -> u64 {
        self.sio0
            .port1()
            .pad()
            .map(|p| p.completed_polls())
            .unwrap_or(0)
    }

    /// Recent completed `0x42` poll transactions seen on port 1.
    pub fn port1_recent_polls(&self) -> Vec<crate::pad::PollSnapshot> {
        self.sio0
            .port1()
            .pad()
            .map(|p| p.recent_polls())
            .unwrap_or_default()
    }

    /// Plug a digital controller into port 2. Used by two-player
    /// several commercial games.
    /// SIO0 already multiplexes port 1 / port 2 internally via
    /// the CTRL.SLOT bit -- games switch between them per poll.
    pub fn attach_digital_pad_port2(&mut self) {
        let old = std::mem::take(self.sio0.port2_mut());
        let memcard = old.into_memcard();
        let mut device = crate::pad::PortDevice::empty().with_pad(crate::pad::DigitalPad::new());
        if let Some(card) = memcard {
            device = device.with_memcard(card);
        }
        self.sio0.attach_port2(device);
    }

    /// Plug a memory card into port 2. Same semantics as
    /// [`Bus::attach_memcard_port1`].
    pub fn attach_memcard_port2(&mut self, initial_bytes: Vec<u8>) {
        let card = if initial_bytes.len() == crate::pad::MEMCARD_SIZE {
            crate::pad::MemoryCard::from_bytes(initial_bytes)
        } else {
            crate::pad::MemoryCard::new()
        };
        let pad = std::mem::take(self.sio0.port2_mut()).into_pad();
        let mut device = crate::pad::PortDevice::empty().with_memcard(card);
        if let Some(p) = pad {
            device = device.with_pad(p);
        }
        self.sio0.attach_port2(device);
    }

    /// Remove any memory card from port 2 while preserving the pad.
    pub fn detach_memcard_port2(&mut self) {
        let device = std::mem::take(self.sio0.port2_mut()).without_memcard();
        self.sio0.attach_port2(device);
    }

    /// Snapshot the port-2 memcard bytes for persistence. `None`
    /// when there's no card on port 2 or the card hasn't been
    /// written since load.
    pub fn memcard_port2_snapshot(&mut self) -> Option<Vec<u8>> {
        let card = self.sio0.port2_mut().memcard_mut()?;
        if !card.is_dirty() {
            return None;
        }
        let bytes = card.as_bytes().to_vec();
        card.clear_dirty();
        Some(bytes)
    }

    /// Update the buttons currently held on the port-2 controller.
    pub fn set_port2_buttons(&mut self, buttons: crate::pad::ButtonState) {
        self.sio0.set_port2_buttons(buttons);
    }

    /// Port-2 DualShock motor state. Mirrors
    /// [`Bus::port1_motor_state`].
    pub fn port2_motor_state(&self) -> (bool, u8) {
        self.sio0
            .port2()
            .pad()
            .map(|p| p.motor_state())
            .unwrap_or((false, 0))
    }

    /// Internal: log one HLE BIOS call. Called from the HLE dispatcher.
    pub(crate) fn hle_bios_log_call(&mut self, table: crate::hle_bios::Table, func: u8) {
        let idx = match table {
            crate::hle_bios::Table::A => 0,
            crate::hle_bios::Table::B => 1,
            crate::hle_bios::Table::C => 2,
        };
        self.hle_bios_calls[idx][func as usize] =
            self.hle_bios_calls[idx][func as usize].saturating_add(1);
        if std::env::var_os("PSOXIDE_TRACE_HLE_BIOS").is_some() {
            eprintln!("[hle-bios] {table:?}({func:02x}h)");
        }
    }

    /// Snapshot of HLE BIOS call counts: `[A, B, C]` tables × 256
    /// function slots. Diagnostic only.
    pub fn hle_bios_call_counts(&self) -> [[u32; 256]; 3] {
        self.hle_bios_calls
    }

    /// True when `phys` sits inside the MMIO window at `0x1F80_1000..0x1F80_2000`.
    /// Used to filter trace recording -- RAM / BIOS fetches are out of scope.
    #[inline]
    fn is_mmio(phys: u32) -> bool {
        (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys)
    }

    /// Record an MMIO access in the ring buffer when tracing is enabled.
    /// Call sites stay cfg-free; the inner record() is a no-op otherwise.
    #[inline]
    fn trace_mmio(&mut self, kind: MmioKind, phys: u32, value: u32) {
        if Self::is_mmio(phys) {
            self.mmio_trace.record(self.cycles, kind, phys, value);
        }
    }

    /// Advance the SIO0 byte/ACK timers to the current bus cycle,
    /// forward any newly latched controller IRQ to `I_STAT`, and
    /// (re)schedule [`EventSlot::Sio`] for whatever deadline is
    /// next pending. With the scheduler firing the wake-up, the
    /// per-instruction poll is no longer needed -- `Bus::tick`
    /// dropped its `service_sio0` call. Read paths still call
    /// this synchronously so a load that happens between the
    /// branch tests sees a deadline that's already due.
    fn service_sio0(&mut self) {
        self.sio0.tick(self.cycles);
        if self.sio0.take_pending_irq() {
            self.irq.raise(IrqSource::Controller);
        }
        self.reschedule_sio0_event();
    }

    /// (Re)plant the [`EventSlot::Sio`] entry on the scheduler so
    /// the next deadline (transfer / ack / ack-end) wakes us up
    /// without us polling every instruction. Cancels any prior
    /// pending Sio event when SIO0 has gone idle.
    fn reschedule_sio0_event(&mut self) {
        if let Some(deadline) = self.sio0.next_deadline() {
            let delta = deadline.saturating_sub(self.cycles);
            self.scheduler
                .schedule(crate::scheduler::EventSlot::Sio, self.cycles, delta);
        } else {
            self.scheduler.cancel(crate::scheduler::EventSlot::Sio);
        }
    }

    /// Cycle count at which the next VBlank is scheduled to fire.
    /// Exposed for diagnostics / the HUD. Reads through to the
    /// scheduler, which is the source of truth since Phase 5a.
    pub fn next_vblank_cycle(&self) -> u64 {
        self.scheduler
            .target(crate::scheduler::EventSlot::VBlank)
            .unwrap_or(u64::MAX)
    }

    /// Advance the cycle counter by `n` cycles and run any scheduled
    /// peripheral events that have come due. Called once per
    /// instruction from `Cpu::step` (charging BIAS before the opcode).
    ///
    /// CDROM is deliberately NOT processed here -- only at
    /// `drain_scheduler_events_post_op`, which the CPU calls at
    /// branch-delay-slot boundaries. That matches Redux's
    /// `branchTest` timing (psxinterpreter.cc:1650) where CDROM
    /// `interrupt()` fires at end-of-delay-slot, not on every
    /// instruction. Ticking CDROM on every BIAS makes our ACK
    /// land one or two instructions earlier than Redux's -- long
    /// enough for a CDROM-polling spin-wait (e.g. a commercial title's BIOS
    /// ReadTOC-Ack wait at step ~90M) to see a different register
    /// byte than Redux and exit the loop early.
    #[inline]
    pub fn tick(&mut self, n: u32) {
        self.advance_cycles(n);
        self.drain_scheduler_events_without_cdr_dma();
        // SIO0 used to be polled here (every instruction).
        // It's now woken up by `EventSlot::Sio` from the scheduler
        // -- see `drain_scheduler_events_inner`. Read paths still
        // call `service_sio0` synchronously so MMIO loads observe
        // any deadline that's already due.
    }

    /// Public entry point that the CPU calls between the opcode's
    /// `add_cycles` (memory-access charges) and the delay-slot IRQ
    /// check. Ensures any scheduler event whose target was crossed
    /// DURING the opcode (not just at the BIAS tick that starts the
    /// instruction) raises its IRQ bit in time for the same step's
    /// exception dispatch. Redux achieves the same effect via
    /// `branchTest` → `counters->update()`.
    pub fn drain_scheduler_events_post_op(&mut self) {
        // Advance timer state to `now` once per branch boundary so
        // any IRQ that would have fired between the last branchTest
        // and this one lands in `I_STAT` in time for the same
        // step's exception dispatch. Per-instruction `Bus::tick`
        // doesn't touch timers anymore; this is the only path that
        // matters for IRQ visibility. Mirrors Redux's
        // `Counters::update` call at the top of `branchTest`.
        self.service_timers();
        self.drain_scheduler_events();
        let cdrom_irq_pending =
            self.irq.stat() & self.irq.mask() & (1 << (IrqSource::Cdrom as u32)) != 0;
        if self
            .cdrom
            .tick_with_irq_pending(self.cycles, cdrom_irq_pending)
            && self.cdrom.should_wake_cpu()
        {
            self.irq.raise(IrqSource::Cdrom);
        }
        // SIO0 wake-up comes from the scheduler dispatch above
        // (`EventSlot::Sio` in `drain_scheduler_events_inner`),
        // not from a separate poll. The `take_due` walk is
        // strict-greater-than, so events that were due as of
        // `now` will fire next branch test; SIO0's parity
        // tolerance is well within that window.
    }

    /// Walk every scheduler slot whose deadline has passed and
    /// dispatch its handler. Mirrors Redux's `branchTest` interrupt
    /// loop (`core/r3000a.cc`), which uses a single 15-slot queue
    /// to drive DMA / CDROM / SPU / MDEC / SIO completions.
    ///
    /// DMA channel completions all funnel through the shared
    /// `Dma` IRQ line: each per-channel slot clears CHCR bit 24
    /// and records a master-edge if the channel's DICR bit is
    /// armed. One IRQ raise covers any number of simultaneous
    /// completions this tick.
    ///
    /// Slots we haven't migrated yet (CDROM, VBlank, SPU, SIO,
    /// MDEC) will never appear here because no subsystem schedules
    /// them -- the legacy timers still own those. Each migration
    /// replaces a legacy timer with `scheduler.schedule(...)` and
    /// adds a `match` arm here.
    #[inline]
    fn drain_scheduler_events_without_cdr_dma(&mut self) {
        self.drain_scheduler_events_inner(false, false);
    }

    fn drain_scheduler_events(&mut self) {
        self.drain_scheduler_events_inner(true, true);
    }

    fn drain_scheduler_events_inner(&mut self, include_cdr_dma: bool, include_sio: bool) {
        use crate::scheduler::EventSlot;
        let now = self.cycles;
        // Fast path -- runs once per retired instruction via
        // `Bus::tick`. `lowest_target` is the cached minimum across
        // every active slot (VBlank included), and both take rules
        // (`take_slot_due_inclusive`: fire when `target <= now`;
        // `take_due`: fire when `target < now`) can only fire once
        // `now` has reached the target, so `now < lowest_target`
        // proves every taker below would return `None`. One compare
        // instead of the VBlank check + exclude-mask setup + queue
        // walk that used to run on every instruction.
        if now < self.scheduler.lowest_target() {
            return;
        }
        let mut dma_edge = false;
        // NOTE: `service_timers()` is intentionally NOT called here.
        // This function runs from the per-instruction `Bus::tick`
        // path (via `drain_scheduler_events_without_cdr_dma`); the
        // whole point of the lazy refactor is to avoid touching
        // timer state on every instruction. The branch-boundary
        // drain (`drain_scheduler_events_post_op`) advances timers
        // before doing anything else, which is enough to keep
        // timer IRQs firing at parity-relevant cycles.

        // Redux updates root counters with `cycle >= nextCounter`
        // before walking the strict interrupt-slot queue. VBlank is
        // our root-counter-style event, so handle it inclusively here
        // instead of via `take_due`'s generic `target < now` rule.
        // Advance the lazy timer bank while the old VBlank target is still
        // installed. Timer 1 sync modes must cross/reset at that edge; doing
        // this after rescheduling loses the boundary entirely.
        if self
            .scheduler
            .target(EventSlot::VBlank)
            .is_some_and(|target| target <= now)
        {
            self.service_timers();
        }
        while let Some(target) = self
            .scheduler
            .take_slot_due_inclusive(EventSlot::VBlank, now)
        {
            self.irq.raise(IrqSource::VBlank);
            self.gpu.toggle_vblank_field();
            self.timers.notify_vblank();
            self.scheduler
                .schedule(EventSlot::VBlank, target, self.vblank_period);
        }

        // SIO0 IRQ delivery follows Redux's interrupt queue: due SIO
        // targets are processed at the branch-test/post-op drain, not
        // from the per-instruction BIAS tick. Processing them from
        // `Bus::tick` can make I_STAT bit 7 visible a few
        // instructions early inside BIOS/game interrupt handlers
        // (first observed route drift at 266,946,810).
        if include_sio {
            while self
                .scheduler
                .take_slot_due_inclusive(EventSlot::Sio, now)
                .is_some()
            {
                self.service_sio0();
                // If `service_sio0` chained a follow-up deadline
                // that's also already due (e.g. transfer → ack
                // within one dispatch), pick it up on the next
                // iteration. Otherwise the future deadline waits for
                // the next branch-test drain or an explicit SIO MMIO
                // read/write.
            }
        }

        let mut exclude_mask = 0u32;
        if !include_cdr_dma {
            exclude_mask |= 1 << EventSlot::CdrDma.bit();
        }
        if !include_sio {
            exclude_mask |= 1 << EventSlot::Sio.bit();
        }
        while let Some((slot, target)) = if exclude_mask == 0 {
            self.scheduler.take_due(now)
        } else {
            self.scheduler.take_due_excluding(now, exclude_mask)
        } {
            match slot {
                EventSlot::MdecInDma => {
                    if self.complete_dma_channel(0) {
                        dma_edge = true;
                    }
                }
                EventSlot::MdecOutDma => {
                    if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() {
                        eprintln!(
                            "[mdec-dma] output due cycle={} target={target} ch0={:#010x} ch1={:#010x}",
                            self.cycles,
                            self.dma.channels[0].channel_control,
                            self.dma.channels[1].channel_control
                        );
                    }
                    if self.mdec.complete_dma_out() && self.complete_dma_channel(0) {
                        dma_edge = true;
                    }
                    if self.complete_dma_channel(1) {
                        dma_edge = true;
                    }
                }
                EventSlot::GpuDma => {
                    if self.complete_dma_channel(2) {
                        dma_edge = true;
                    }
                }
                EventSlot::CdrDma => {
                    if self.complete_dma_channel(3) {
                        dma_edge = true;
                    }
                }
                EventSlot::SpuDma => {
                    self.spu.end_dma();
                    if self.complete_dma_channel(4) {
                        dma_edge = true;
                    }
                    self.service_spu_irq();
                }
                EventSlot::GpuOtcDma => {
                    if self.complete_dma_channel(6) {
                        dma_edge = true;
                    }
                }
                EventSlot::VBlank => {
                    self.irq.raise(IrqSource::VBlank);
                    // Toggle GPUSTAT bit 31 (interlace / field flag)
                    // -- some BIOS and game code polls this instead
                    // of (or in addition to) the VBlank IRQ to detect
                    // frame boundaries. Matches Redux's
                    // `SoftGPU::vblank` which XORs the same bit.
                    self.gpu.toggle_vblank_field();
                    // Tell the timer bank -- Timer 1 sync-mode-1
                    // resets its counter on this pulse.
                    self.timers.notify_vblank();
                    // Re-arm the next VBlank from the original
                    // target, not `now`. A 500K-cycle drain lag
                    // would otherwise accumulate drift every time.
                    self.scheduler
                        .schedule(EventSlot::VBlank, target, self.vblank_period);
                }
                EventSlot::SpuAsync => {
                    // Kept for forward compatibility; we pump the SPU
                    // from the frontend instead of the scheduler while
                    // the parity oracle runs with a dormant SPU
                    // thread. If anything schedules this slot it is
                    // a logic bug -- log and drop.
                    debug_assert!(false, "SpuAsync fired but SPU pumps from frontend");
                }
                // Not-yet-migrated slots. A subsystem scheduling one
                // of these today would silently do nothing; they're
                // listed so `match` stays exhaustive as migrations
                // roll in.
                EventSlot::Sio
                | EventSlot::Sio1
                | EventSlot::Cdr
                | EventSlot::CdRead
                | EventSlot::CdrPlay
                | EventSlot::CdrDbuf
                | EventSlot::CdrLid => {}
            }
        }
        // CDROM DMA completion is observed by Redux at the exact
        // target boundary in retail boot paths (a commercial title license-sector
        // DMA lands here). Keep the generic scheduler strict for the
        // other interrupt slots, but let CDR DMA finish on equality.
        if include_cdr_dma
            && self
                .scheduler
                .take_slot_due_inclusive(EventSlot::CdrDma, now)
                .is_some()
            && self.complete_dma_channel(3)
        {
            dma_edge = true;
        }
        if dma_edge {
            self.irq.raise(IrqSource::Dma);
        }
    }

    /// Finalise a DMA channel's transfer: clear the start bit in
    /// CHCR and notify the DMA controller so it updates DICR and
    /// returns whether this channel's IRQ-enable bit caused the
    /// shared `IrqSource::Dma` line to transition high. Caller
    /// raises that IRQ once per tick if any channel was on the
    /// edge.
    fn complete_dma_channel(&mut self, ch: usize) -> bool {
        if ch == 6 {
            // OTC direction/decrement is hardwired to bit 1; both manual
            // trigger and busy clear when the transfer completes.
            self.dma.channels[ch].channel_control = 1 << 1;
        } else {
            self.dma.channels[ch].channel_control &= !(1 << 24);
        }
        self.dma.notify_channel_done(ch)
    }

    /// Advance the cycle counter without running peripheral schedulers.
    /// Used by load/store opcodes to charge the per-data-access cycle
    /// (Redux's `m_regs.cycle += 1` inside `read8/16/32` and
    /// `write8/16/32` in `psxmem.cc`). VBlank / DMA6 / CDROM schedulers
    /// still see the accumulated cycle count when `tick()` runs at end
    /// of instruction -- matching Redux's `psxBranchTest`, which only
    /// runs after delay slots and observes the post-BIAS,
    /// post-data-access total. Timers, however, see every cycle so
    /// their counter values stay in lock-step with Redux's cycle-derived
    /// `count = (now - cycle_start) / rate` model.
    pub fn add_cycles(&mut self, n: u32) {
        self.advance_cycles(n);
    }

    /// Charge root-counter MMIO read wait states while latching only the
    /// selected counter. The public timer program observes exactly the
    /// software instructions between two reads of the same counter
    /// (1000 -> 1011), while another counter still measures the complete
    /// three-cycle MMIO transaction in the independent access-time program.
    /// VBlank, DMA, the other root counters, and the global CPU clock continue
    /// to advance normally.
    pub(crate) fn add_root_counter_read_stalls(&mut self, addr: u32, n: u32) {
        self.service_timers();
        self.timers.hold_counter_for_read(to_physical(addr), n);
        self.advance_cycles(n);
    }

    /// CPU data-read stall cycles beyond the instruction's one-cycle issue.
    /// Bus clients such as DMA use the raw read/write methods and therefore
    /// do not accidentally pay CPU pipeline costs.
    #[inline]
    pub(crate) fn cpu_read_stalls(&mut self, virt: u32, width: AccessWidth) -> u32 {
        // Root-counter reads use the same three-cycle total (one issue + two
        // wait) measured for the other internal MMIO registers by the public
        // access-time suite. Counter phase differences in compound loops must
        // be modeled at their real CPU/bus dependency, not hidden in this
        // independently observable access cost.
        let stalls = self.memory_control.read_stalls(virt, width);
        let phys = to_physical(virt);
        let external_counter_overlap = (memory::expansion1::BASE
            ..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
            || (memory::expansion2::BASE
                ..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
                .contains(&phys)
            || (memory::expansion3::BASE
                ..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
                .contains(&phys)
            || (0x1F80_1800..0x1F80_1804).contains(&phys)
            || (0x1F80_1C00..0x1F80_2000).contains(&phys);
        if external_counter_overlap {
            self.timers
                .overlap_counter_write_with_external_read(self.cycles, stalls);
        }
        if phys < memory::ram::MIRROR_END {
            let access_gap = self.cycles.saturating_sub(self.last_cpu_ram_access_cycle);
            self.last_cpu_ram_access_cycle = self.cycles;
            let refresh_stall = if (0xA000_0000..0xC000_0000).contains(&virt) {
                if access_gap <= 16 {
                    6
                } else {
                    memory_timing::DRAM_REFRESH_UNCACHED_STALL_CYCLES
                }
            } else if access_gap == 8 {
                2
            } else {
                memory_timing::DRAM_REFRESH_CACHED_STALL_CYCLES
            };
            stalls.saturating_add(memory_timing::dram_refresh_wait(
                self.cycles,
                &mut self.dram_refresh_deadline,
                refresh_stall,
            ))
        } else {
            stalls
        }
    }

    /// CPU store stalls beyond the instruction's one-cycle issue cost.
    ///
    /// The physical SCPH-9902 capture resolves cached and uncached main-RAM
    /// stores at two clocks each, while scratchpad stores remain one clock.
    /// Keep this separate from CPU reads: the external write buffer hides
    /// most of the read wait-state cost but still occupies one extra clock.
    #[inline]
    pub(crate) fn cpu_write_stalls(&mut self, virt: u32) -> u32 {
        if to_physical(virt) < memory::ram::MIRROR_END {
            self.last_cpu_ram_access_cycle = self.cycles;
            let refresh_stall = if (0xA000_0000..0xC000_0000).contains(&virt) {
                memory_timing::DRAM_REFRESH_UNCACHED_STALL_CYCLES
            } else {
                memory_timing::DRAM_REFRESH_CACHED_STALL_CYCLES
            };
            1u32.saturating_add(memory_timing::dram_refresh_wait(
                self.cycles,
                &mut self.dram_refresh_deadline,
                refresh_stall,
            ))
        } else {
            0
        }
    }

    /// Uncached instruction-fetch stall cycles.
    #[inline]
    pub(crate) fn instruction_read_stalls(&mut self, virt: u32) -> u32 {
        let stalls = self.memory_control.instruction_read_stalls(virt);
        if to_physical(virt) < memory::ram::MIRROR_END {
            stalls.saturating_add(memory_timing::dram_refresh_wait(
                self.cycles,
                &mut self.dram_refresh_deadline,
                memory_timing::DRAM_REFRESH_UNCACHED_STALL_CYCLES,
            ))
        } else {
            stalls
        }
    }

    /// Cache-line refill stall cycles for `words` fetched from `phys`.
    #[inline]
    pub(crate) fn icache_fill_stalls(&mut self, phys: u32, words: u32) -> u32 {
        let stalls = self.memory_control.icache_fill_stalls(phys, words);
        if words != 0 && phys < memory::ram::MIRROR_END {
            stalls.saturating_add(memory_timing::dram_refresh_wait(
                self.cycles,
                &mut self.dram_refresh_deadline,
                memory_timing::DRAM_REFRESH_CACHED_STALL_CYCLES,
            ))
        } else {
            stalls
        }
    }

    /// Inner cycle-advancement helper shared by `tick` and `add_cycles`.
    /// Any cycle delta must flow through this function so the timer
    /// bank's accumulator matches Redux's lazy-read timer model.
    fn advance_cycles(&mut self, n: u32) {
        self.cycles = self.cycles.wrapping_add(n as u64);
        // Timers used to be ticked here every instruction (~25M
        // calls/sec, three accumulator-divides each). They're now
        // advanced lazily -- `service_timers()` runs once per
        // scheduler drain and on demand from MMIO read / write
        // paths. The lazy advance reads `self.cycles` so it
        // observes the same effective time the per-tick path did.
        // GPU execution credit is expressed directly in CPU/bus
        // cycles. Keeping the unit identical to the global clock makes
        // silicon-derived command costs composable with Timer 1/HBlank
        // measurements and avoids the old arbitrary 32× decay scale.
        self.gpu.decay_busy(n as u64);
    }

    /// Advance the timer bank to the current bus cycle and forward
    /// any newly-latched timer IRQs to `I_STAT`. Cheap when nothing
    /// changed (just a `last_advance` saturating-sub returning 0);
    /// the work happens only in proportion to the cycles elapsed
    /// since the last call. Read / write paths call this before
    /// observing timer state; the scheduler drain calls it once
    /// per branch-test boundary so IRQs fire on time.
    fn service_timers(&mut self) {
        let next_vblank = self
            .scheduler
            .target(crate::scheduler::EventSlot::VBlank)
            .unwrap_or(u64::MAX);
        let fired = self.timers.advance_to_video(
            self.cycles,
            self.hsync_cycles,
            self.gpu.dot_clock_divisor(),
            next_vblank,
            self.vblank_period,
        );
        if fired & 1 != 0 {
            self.irq.raise(IrqSource::Timer0);
        }
        if fired & 2 != 0 {
            self.irq.raise(IrqSource::Timer1);
        }
        if fired & 4 != 0 {
            self.irq.raise(IrqSource::Timer2);
        }
    }

    fn service_spu_irq(&mut self) {
        if self.spu.take_irq_pending() {
            self.irq.raise(IrqSource::Spu);
        }
    }

    fn service_gpu_irq(&mut self) {
        if self.gpu.take_irq_acknowledged() {
            self.irq.clear(IrqSource::Gpu);
        }
        if self.gpu.take_irq_requested() {
            self.irq.raise(IrqSource::Gpu);
        }
    }

    /// Cumulative cycles since reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Raw view of the 2 MiB main RAM. For headless `--dump-ram`
    /// snapshots and offline diffing.
    pub fn ram(&self) -> &[u8] {
        &self.ram[..]
    }

    /// Record `addr` as seen on an unmapped *read*, returning
    /// `true` the first time so the caller logs once.
    fn log_unmapped_read_once(&mut self, addr: u32) -> bool {
        self.unmapped_read_seen.insert(addr)
    }

    /// Record `addr` as seen on an unmapped *write*, returning
    /// `true` the first time so the caller logs once.
    fn log_unmapped_write_once(&mut self, addr: u32) -> bool {
        self.unmapped_write_seen.insert(addr)
    }

    /// Drive the VBlank + DMA + (eventually) CDROM / SPU event
    /// loops once. Provided as a public entry point so tests can
    /// advance peripheral state directly without stepping the CPU.
    /// Production callers hit it transitively via
    /// [`Bus::tick`] → [`Bus::drain_scheduler_events`].
    pub fn run_vblank_scheduler(&mut self) {
        self.drain_scheduler_events();
    }

    /// Pump the SPU forward by `n` samples on exact 768-cycle edges.
    ///
    /// Also forwards any CD audio samples the CDROM has decoded
    /// (CD-DA / XA ADPCM) into the SPU's CD input mix -- one
    /// drain-and-feed per call keeps the latency bounded.
    pub fn run_spu_samples(&mut self, n: usize) {
        self.cdrom.pump_cdda_samples(n);
        // Move any freshly-decoded CDROM audio into the SPU's CD
        // input queue so it participates in this frame's mix.
        let cd_samples = self.cdrom.drain_cd_audio();
        if !cd_samples.is_empty() {
            self.spu.feed_cd_audio(&cd_samples);
        }
        for _ in 0..n {
            let sample_cycle = self.spu_sample_deadline;
            self.spu_sample_deadline = self
                .spu_sample_deadline
                .saturating_add(crate::spu::SAMPLE_CYCLES);
            self.spu.tick_sample(sample_cycle);
            self.service_spu_irq();
        }
    }

    /// Produce every SPU sample whose exact clock edge has elapsed. The phase
    /// belongs to the emulated machine, so BIOS warmup and frontend handoffs
    /// cannot discard a partial sample period.
    pub fn run_spu_to_current_cycle(&mut self) -> usize {
        if self.spu_sample_deadline > self.cycles {
            return 0;
        }
        let elapsed = self.cycles - self.spu_sample_deadline;
        let due = (elapsed / crate::spu::SAMPLE_CYCLES + 1) as usize;
        self.run_spu_samples(due);
        due
    }

    /// Borrow the interrupt controller -- caller can `.raise()` sources
    /// or inspect state without going through MMIO.
    pub fn irq_mut(&mut self) -> &mut Irq {
        &mut self.irq
    }

    /// Diagnostic: per-channel count of CHCR writes with the start
    /// bit set since reset. Index 0..=6 corresponds to MDEC-in,
    /// MDEC-out, GPU, CD-ROM, SPU, PIO, OTC.
    pub fn dma_start_triggers(&self) -> [u64; 7] {
        self.dma.start_trigger_counts
    }

    /// True when some source is both pending in `I_STAT` and enabled
    /// in `I_MASK`. The CPU mirrors this into `COP0.CAUSE.IP[2]`.
    pub fn external_interrupt_pending(&mut self) -> bool {
        self.irq.pending_tick()
    }

    /// Borrow the IRQ controller immutably for diagnostics.
    pub fn irq(&self) -> &Irq {
        &self.irq
    }

    /// Borrow the timer bank immutably for diagnostics.
    pub fn timers(&self) -> &Timers {
        &self.timers
    }

    /// Copy a PSX-EXE payload into RAM at its declared load address.
    ///
    /// The caller is expected to also seed the CPU (see
    /// [`crate::Cpu::seed_from_exe`]) so execution begins at the
    /// EXE's entry point. `load_addr` must point inside the 2 MiB
    /// RAM window; addresses outside panic.
    ///
    /// Used by `PSOXIDE_EXE` side-loading in the frontend / smoke
    /// harness to bypass the BIOS entirely and run homebrew directly.
    pub fn load_exe_payload(&mut self, load_addr: u32, payload: &[u8]) {
        let base = load_addr & 0x001F_FFFF; // KSEG/KUSEG -> physical RAM
        assert!(
            (base as usize) + payload.len() <= self.ram.len(),
            "EXE payload overflows RAM: load_addr={load_addr:#010x} len={}",
            payload.len()
        );
        self.ram[base as usize..base as usize + payload.len()].copy_from_slice(payload);
    }

    /// Zero the optional BSS range declared by a PSX-EXE header.
    ///
    /// The BIOS clears this area before jumping to the executable;
    /// side-load and fast-boot paths need to do the same because they
    /// bypass the BIOS loader.
    pub fn clear_exe_bss(&mut self, bss_addr: u32, bss_size: u32) {
        if bss_size == 0 {
            return;
        }
        let base = bss_addr & 0x001F_FFFF; // KSEG/KUSEG -> physical RAM
        let size = bss_size as usize;
        assert!(
            (base as usize) + size <= self.ram.len(),
            "EXE BSS overflows RAM: bss_addr={bss_addr:#010x} len={size}",
        );
        self.ram[base as usize..base as usize + size].fill(0);
    }

    /// Zero an address range in main RAM after KSEG/KUSEG address
    /// normalization. The end address is exclusive.
    pub fn clear_ram_range(&mut self, start_addr: u32, end_addr: u32) {
        let start = (start_addr & 0x001F_FFFF) as usize;
        let end = (end_addr & 0x001F_FFFF) as usize;
        if end <= start {
            return;
        }
        assert!(
            end <= self.ram.len(),
            "RAM clear range overflows: start={start_addr:#010x} end={end_addr:#010x}",
        );
        self.ram[start..end].fill(0);
    }

    /// Run DMA on a single channel after its CHCR was just written
    /// with the start bit set. Mirrors Redux's per-channel
    /// `dmaExec<N>` dispatch in `psxhw.cc` -- each CHCR write goes
    /// to exactly one channel's handler, NOT a sweep across every
    /// channel. That distinction matters: if another channel's
    /// transfer was still in-flight (start bit set, awaiting its
    /// scheduled completion), a sweep re-runs it and schedules a
    /// second target that overwrites the first.
    /// Channels named by `PSOXIDE_WEDGE_DMA` (a bitmask, decimal or
    /// `0x`-prefixed) latch busy forever instead of transferring, the way
    /// a real controller does when a kick never completes.
    ///
    /// Modelling ideal hardware is what let three separate guest hangs
    /// reach burned discs: the emulator's DMA always completes, so a
    /// `while is_busy {}` that hangs a console runs clean here. With this
    /// set, a headless run reproduces the wedge instead.
    fn dma_wedge_mask() -> u8 {
        static MASK: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
        *MASK.get_or_init(|| {
            let Ok(raw) = std::env::var("PSOXIDE_WEDGE_DMA") else {
                return 0;
            };
            let raw = raw.trim();
            let parsed = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
                Some(hex) => u8::from_str_radix(hex, 16),
                None => raw.parse::<u8>(),
            };
            parsed.unwrap_or(0)
        })
    }

    fn run_dma_channel(&mut self, ch: usize) {
        if Self::dma_wedge_mask() & (1 << ch) != 0 {
            // No transfer, no scheduled completion: CHCR keeps its START
            // bit, so `is_busy` stays true and any later kick on this
            // channel is ignored, exactly like the silicon failure.
            return;
        }
        // Each channel: run the transfer now (so memory / GPU state is
        // up-to-date for any immediate follow-up reads), but defer the
        // CHCR start-bit clear and DMA IRQ raise to the channel's
        // scheduled completion cycle. Redux schedules one cycle per
        // word transferred (`scheduleGPUOTCDMAIRQ(size)`, etc.), which
        // keeps the BIOS's "poll CHCR until done" loop matching our
        // trace step-for-step. An immediate IRQ raise triggers the
        // handler ~1 hblank early and diverges the trace by dozens of
        // instructions.
        use crate::scheduler::EventSlot;
        if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() && ch <= 1 {
            let channel = self.dma.channels[ch];
            eprintln!(
                "[mdec-dma] start ch={ch} cycle={} dpcr={:#010x} bcr={:#010x} chcr={:#010x} out_ready={}",
                self.cycles,
                self.dma.dpcr,
                channel.block_control,
                channel.channel_control,
                self.mdec.can_dma_out()
            );
        }
        // Run only the channel whose CHCR was just written.
        match ch {
            0 => {
                if let Some(mdec_words) = self.run_dma_mdec_in() {
                    if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() {
                        eprintln!(
                            "[mdec-dma] input accepted cycle={} words={mdec_words} command={:#010x} state={:?} rle={} next={:?} out_ready={} wait_for_out={}",
                            self.cycles,
                            self.mdec.command_history().last().copied().unwrap_or(0),
                            self.mdec.state(),
                            self.mdec.queued_rle_halfwords(),
                            self.mdec.next_rle_halfword(),
                            self.mdec.output_ready(),
                            self.mdec.decode_dma0_waits_for_output()
                        );
                    }
                    if self.mdec.decode_dma0_waits_for_output() {
                        self.try_schedule_ready_mdec_out();
                    } else {
                        let target = self.cycles + mdec_words as u64;
                        self.log_dma_schedule("MdecIn", mdec_words as u64, target);
                        self.scheduler.schedule(
                            EventSlot::MdecInDma,
                            self.cycles,
                            mdec_words as u64,
                        );
                    }
                }
            }
            1 => {
                self.try_schedule_ready_mdec_out();
            }
            2 => {
                if let Some(gpu_cycles) = self.run_dma_gpu() {
                    let target = self.cycles + gpu_cycles as u64;
                    self.log_dma_schedule("GpuDma", gpu_cycles as u64, target);
                    self.scheduler
                        .schedule(EventSlot::GpuDma, self.cycles, gpu_cycles as u64);
                }
            }
            3 => {
                let ch = self.dma.channels[3];
                let fifo_len = self.cdrom.data_fifo_len();
                let armed = self.cdrom.data_transfer_armed();
                if let Some(cdrom_words) = self.run_dma_cdrom() {
                    let label = format!(
                        "CdrDma words={cdrom_words} fifo={fifo_len} armed={} madr=0x{:08x} bcr=0x{:08x} chcr=0x{:08x}",
                        armed as u8, ch.base, ch.block_control, ch.channel_control
                    );
                    if cdrom_words == 0 {
                        self.log_dma_schedule(&label, 0, self.cycles);
                        if self.complete_dma_channel(3) {
                            self.irq.raise(IrqSource::Dma);
                        }
                    } else {
                        let delay = match self.dma.channels[3].channel_control {
                            0x1140_0100 => (cdrom_words / 4).max(1) as u64,
                            _ => cdrom_words as u64,
                        };
                        let target = self.cycles + delay;
                        self.log_dma_schedule(&label, delay, target);
                        self.scheduler
                            .schedule(EventSlot::CdrDma, self.cycles, delay);
                    }
                }
            }
            4 => {
                if let Some(spu_delay) = self.run_dma_spu() {
                    let target = self.cycles + spu_delay as u64;
                    self.log_dma_schedule("SpuDma", spu_delay as u64, target);
                    self.scheduler
                        .schedule(EventSlot::SpuDma, self.cycles, spu_delay as u64);
                }
            }
            6 => {
                let otc_words = if self.dma.is_channel_enabled(6) {
                    self.dma.run_otc(&mut self.ram[..])
                } else {
                    0
                };
                if otc_words > 0 {
                    // OTC owns the main-RAM bus for the complete transfer, so
                    // the CPU cannot execute the following CHCR read until the
                    // ordering table is finished. PS1 DRAM hyper-page mode is
                    // one cycle per word plus one row-address setup per 16
                    // words (the same measured model used by DuckStation).
                    let otc_cycles = otc_words.saturating_add(otc_words.div_ceil(16));
                    // SCPH-9902 PX6 capture: the first CHCR read after the
                    // CPU regains the bus still sees START|TRIGGER, and the
                    // immediately following read sees both clear. Keep the
                    // completion edge one cycle beyond the bus-hold window;
                    // the first MMIO read starts in that observable slot and
                    // advances through the scheduled clear for the next read.
                    let completion_delay = otc_cycles.saturating_add(2);
                    let target = self.cycles + completion_delay as u64;
                    self.log_dma_schedule("GpuOtc", completion_delay as u64, target);
                    self.scheduler.schedule(
                        EventSlot::GpuOtcDma,
                        self.cycles,
                        completion_delay as u64,
                    );
                    self.add_cycles(otc_cycles);
                }
            }
            _ => {
                // Channel 5 (PIO) + invalid indices -- skip silently.
                // Matches Redux's `#if 0` guard that disables PIO DMA.
            }
        }
    }

    /// Optional per-DMA-schedule log. Off by default; the
    /// `probe_dma_schedules` example enables it via the setter to
    /// capture every DMA completion's `(cycle_now, delta, target)`
    /// for cycle-parity diagnosis. Stored on the bus so the probe
    /// can drain it after a run without poking CPU-execution paths.
    pub fn set_dma_log_enabled(&mut self, enabled: bool) {
        self.dma_log_enabled = enabled;
        if enabled {
            self.dma_log.clear();
        }
    }

    /// Drain collected DMA schedule events.
    pub fn drain_dma_log(&mut self) -> Vec<(String, u64, u64, u64)> {
        std::mem::take(&mut self.dma_log)
    }

    /// Enable or disable capture of `(transfer, node_address, header)` for
    /// every ordering-table node consumed by linked-list GPU DMA.
    pub fn set_gpu_linked_list_log_enabled(&mut self, enabled: bool) {
        self.gpu_linked_list_log_enabled = enabled;
        if enabled {
            self.gpu_linked_list_transfer = 0;
            self.gpu_linked_list_log.clear();
        }
    }

    /// Return the captured linked-list GPU DMA nodes.
    pub fn gpu_linked_list_log(&self) -> &[(u32, u32, u32)] {
        &self.gpu_linked_list_log
    }

    fn log_dma_schedule(&mut self, kind: &str, delta: u64, target: u64) {
        if self.dma_log_enabled {
            self.dma_log
                .push((kind.to_string(), self.cycles, delta, target));
        }
    }

    fn try_schedule_ready_mdec_out(&mut self) {
        use crate::scheduler::EventSlot;

        if self.scheduler.is_pending(EventSlot::MdecOutDma) || !self.mdec.can_dma_out() {
            if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() {
                eprintln!(
                    "[mdec-dma] output deferred cycle={} pending={} can_out={}",
                    self.cycles,
                    self.scheduler.is_pending(EventSlot::MdecOutDma),
                    self.mdec.can_dma_out()
                );
            }
            return;
        }
        if let Some(mdec_words) = self.run_dma_mdec_out() {
            // Redux's MDEC model schedules output DMA by byte count
            // multiplied by MDEC_BIAS=2.0, i.e. 8 cycles per 32-bit word.
            let delay = mdec_words as u64 * 8;
            let target = self.cycles + delay;
            self.log_dma_schedule("MdecOut", delay, target);
            self.scheduler
                .schedule(EventSlot::MdecOutDma, self.cycles, delay);
            if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() {
                eprintln!(
                    "[mdec-dma] output scheduled cycle={} words={mdec_words} delay={delay}",
                    self.cycles
                );
            }
        } else if std::env::var_os("PSOXIDE_TRACE_MDEC_DMA").is_some() {
            let channel = self.dma.channels[1];
            eprintln!(
                "[mdec-dma] output not armed cycle={} dpcr={:#010x} bcr={:#010x} chcr={:#010x}",
                self.cycles, self.dma.dpcr, channel.block_control, channel.channel_control
            );
        }
    }

    /// Execute DMA channel 0 → MDEC input. Ships command + RLE data
    /// from main RAM to the MDEC's input queue. Sync mode 1 (block)
    /// is the only mode PS1 software uses for this channel.
    fn run_dma_mdec_in(&mut self) -> Option<u32> {
        if !self.dma.is_channel_enabled(0) {
            return None;
        }
        let ch = &self.dma.channels[0];
        if (ch.channel_control >> 24) & 1 == 0 {
            return None;
        }
        let total_words = mdec_dma_word_count(ch.block_control, ch.channel_control);
        let step: u32 = if (ch.channel_control >> 1) & 1 != 0 {
            0xFFFF_FFFCu32
        } else {
            4
        };
        let mut addr = ch.base & 0x001F_FFFC;
        let mut words: Vec<u32> = Vec::with_capacity(total_words as usize);
        for _ in 0..total_words {
            words.push(read_ram_u32(&self.ram[..], addr));
            addr = addr.wrapping_add(step);
        }
        self.mdec.dma_write_in(&words);
        Some(total_words)
    }

    /// Execute DMA channel 1 → main RAM from MDEC output. Pulls
    /// decoded pixel words from the MDEC's output queue and writes
    /// them to main RAM at `MADR`.
    fn run_dma_mdec_out(&mut self) -> Option<u32> {
        if !self.dma.is_channel_enabled(1) {
            return None;
        }
        if !self.mdec.can_dma_out() {
            return None;
        }
        let ch = &self.dma.channels[1];
        if (ch.channel_control >> 24) & 1 == 0 {
            return None;
        }
        let total_words = mdec_dma_word_count(ch.block_control, ch.channel_control);
        let step: u32 = if (ch.channel_control >> 1) & 1 != 0 {
            0xFFFF_FFFCu32
        } else {
            4
        };
        let mut addr = ch.base & 0x001F_FFFC;
        let mut words = vec![0u32; total_words as usize];
        self.mdec.dma_read_out(&mut words);
        for word in words {
            let offset = (addr & 0x001F_FFFF) as usize;
            if offset + 4 <= self.ram.len() {
                self.ram[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            }
            addr = addr.wrapping_add(step);
        }
        Some(total_words)
    }

    /// Execute DMA channel 4 ↔ SPU. The SPU FIFO cadence follows the active
    /// memory-controller profile: 16 CPU cycles per halfword on the earlier
    /// public COMMON0=5 profile and 32 on the captured late PAL profile.
    ///
    /// - Direction bit 0 = 1: main RAM → SPU RAM (normal -- upload sample data).
    /// - Direction bit 0 = 0: SPU RAM → main RAM (rare -- live capture).
    ///
    /// CHCR start/busy bits are NOT cleared here; the completion
    /// handler in `drain_scheduler_events` does that at the scheduled
    /// cycle.
    fn run_dma_spu(&mut self) -> Option<u32> {
        if !self.dma.is_channel_enabled(4) {
            return None;
        }
        let ch = &self.dma.channels[4];
        if (ch.channel_control >> 24) & 1 == 0 {
            return None;
        }
        // SPUCNT must be in a DMA transfer mode (write or read) for
        // the transfer to land; otherwise the channel is armed but
        // the SPU doesn't accept words. Games always program SPUCNT
        // before kicking the channel, so this is a belt-and-braces
        // check.
        if !self.spu.dma_transfer_enabled() {
            // Still return Some so the completion IRQ fires -- the
            // CHCR start bit must clear or the BIOS's DMA-wait loop
            // hangs forever.
            return Some(0);
        }
        self.spu.begin_dma(self.cycles);
        let sync_mode = (ch.channel_control >> 9) & 0x3;
        let bcr = ch.block_control;
        // BCR length is a count of 32-bit DMA words. Each transferred word
        // maps to TWO 16-bit SPU-RAM halfwords, so the SPU receives twice
        // this many halfwords (Redux's writeDMAMem `size` = 2 x BCR product).
        // Completion timing is governed by the SPU FIFO engine, not merely
        // the much faster main-RAM DMA bus transaction.
        let (word_count, block_words): (u32, u32) = match sync_mode {
            0 => {
                let words = bcr & 0xFFFF;
                (words, words)
            }
            1 => {
                let block_size = bcr & 0xFFFF;
                let block_count = (bcr >> 16) & 0xFFFF;
                (block_size * block_count, block_size)
            }
            _ => (0, 0), // Linked list + reserved -- not used for SPU.
        };
        let halfword_count = word_count.saturating_mul(2);
        let to_spu = ch.channel_control & 1 != 0;
        let step: u32 = if (ch.channel_control >> 1) & 1 != 0 {
            0xFFFF_FFFEu32
        } else {
            2
        };
        let mut addr = ch.base & 0x001F_FFFF;
        if to_spu {
            let mut words: Vec<u16> = Vec::with_capacity(halfword_count as usize);
            for _ in 0..halfword_count {
                words.push(read_ram_u16(&self.ram[..], addr));
                addr = addr.wrapping_add(step);
            }
            if !self.scph_9902_spu_dma_aperture || self.spu.dma_write_ready_at(self.cycles) {
                self.spu.dma_write(&words);
            } else {
                // SCPH-9902 accepts the first 32-bit DMA word while the
                // freshly-written SPUCNT mode is still crossing into the
                // sample-clock domain, then stops accepting the burst.
                // Preserve that single-word aperture instead of treating
                // an early kick as either a full transfer or a total drop.
                self.spu.dma_write(&words[..words.len().min(2)]);
            }
        } else {
            let mut words = vec![0u16; halfword_count as usize];
            self.spu.dma_read_blocks(
                &mut words,
                block_words.saturating_mul(2) as usize,
                self.memory_control.spu_dma_read_is_stable(),
            );
            for word in words {
                write_ram_u16(&mut self.ram[..], addr, word);
                addr = addr.wrapping_add(step);
            }
        }
        self.service_spu_irq();
        Some(self.memory_control.spu_dma_cycles(word_count))
    }

    /// Execute DMA channel 3 → CPU. Block mode (sync=1) is the only
    /// mode used for CD-ROM reads: pull `BS × BA` words from the data
    /// FIFO and write them to RAM at `MADR` with +4 step. Returns
    /// `Some(word_count)` when a transfer was kicked (so the caller
    /// can schedule the completion IRQ), `None` when the channel
    /// wasn't armed. CHCR start/busy bits are NOT cleared here -- the
    /// per-channel scheduler does that at the completion cycle.
    fn run_dma_cdrom(&mut self) -> Option<u32> {
        if !self.dma.is_channel_enabled(3) {
            return None;
        }
        let ch = self.dma.channels[3];
        if (ch.channel_control >> 24) & 1 == 0 {
            return None;
        }
        // Redux rejects DMA3 kicks only until a sector is ready in
        // the transfer buffer (`m_read == 0`). It does not require
        // the request-register bit that gates MMIO data reads; the
        // BIOS kicks DMA before that latch is armed in one commercial
        // CDROM handler and expects CHCR bit 24 to remain busy for
        // the scheduled DMA window.
        if self.cdrom.data_fifo_len() == 0 {
            return Some(0);
        }
        let sync_mode = (ch.channel_control >> 9) & 0x3;
        // PSX BIOS + most games use sync mode 1 (block request) for
        // CDROM reads, but some firmware paths use sync mode 0
        // (manual / immediate). They differ only in how BCR is
        // interpreted:
        //
        //   mode 0 (manual): BCR is the total number of words to
        //                    transfer. BA is ignored.
        //   mode 1 (block):  BCR is (BA << 16) | BS -- transfer BS
        //                    words per request, BA times.
        //
        // Both result in the same byte flow from the CDROM data
        // FIFO to RAM; computing `total_words` from the right BCR
        // encoding is what matters. Earlier we short-circuited
        // sync_mode!=1 to Some(0), which silently dropped every
        // BIOS disc read: the FIFO still filled (we saw LBA 16 in
        // it from cdrom_drive_test) but its bytes never landed in
        // RAM, and the BIOS's PVD parse fell back to reading
        // LBA 0 on empty input.
        let bcr = ch.block_control;
        let requested_words = match sync_mode {
            0 => bcr & 0xFFFF,
            1 => {
                let block_size = bcr & 0xFFFF;
                let block_count = (bcr >> 16) & 0xFFFF;
                block_size * block_count.max(1)
            }
            _ => {
                // Linked-list (2) + reserved (3) -- not used for
                // CDROM. Drop the trigger silently.
                return Some(0);
            }
        };
        // Redux falls back to the active sector size when BCR asks
        // for zero words (for example Ape Escape programs `0001/0000`
        // and expects a full 2048-byte transfer). Our FIFO already
        // holds the exact transfer payload, so derive the word count
        // from its live length.
        let total_words = if requested_words == 0 {
            self.cdrom.data_fifo_words()
        } else {
            requested_words
        };
        let mut addr = ch.base & 0x001F_FFFC;
        let step: u32 = if (ch.channel_control >> 1) & 1 != 0 {
            0xFFFF_FFFCu32
        } else {
            4
        };

        for _ in 0..total_words {
            let b0 = self.cdrom.pop_dma_data_byte() as u32;
            let b1 = self.cdrom.pop_dma_data_byte() as u32;
            let b2 = self.cdrom.pop_dma_data_byte() as u32;
            let b3 = self.cdrom.pop_dma_data_byte() as u32;
            let word = b0 | (b1 << 8) | (b2 << 16) | (b3 << 24);
            let offset = (addr & 0x001F_FFFF) as usize;
            if offset + 4 <= self.ram.len() {
                self.ram[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
            }
            addr = addr.wrapping_add(step);
        }
        Some(total_words)
    }

    /// Execute DMA channel 2 → GPU (GP0). Supports all three useful sync
    /// modes:
    ///
    /// - **Mode 0 (manual)**: Ship the low BCR halfword's word count. GPU
    ///   transfers begin from CHCR's busy bit alone; unlike OTC, channel 2
    ///   does not require the separate manual-trigger bit.
    /// - **Mode 1 (block)**: Ship `BS × BA` words starting at
    ///   `MADR` straight into GP0, with the `MADR` step direction
    ///   given by CHCR bit 1 (+4 or -4). PS1 always uses +4 direction
    ///   for CPU→GPU.
    /// - **Mode 2 (linked list)**: Walk a chain of packets in RAM.
    ///   Each node header is `[NN AAAAAA]` -- top byte = word count
    ///   (following 32-bit words to ship to GP0), low 24 bits = next
    ///   node address. Terminator is `AAAAAA == 0xFFFFFF`.
    ///
    /// Returns `Some(completion_cycles)` when a transfer was kicked
    /// (caller uses it to schedule the GpuDma event), `None` when
    /// the channel wasn't armed. CHCR start/busy bits are NOT cleared
    /// here -- the scheduler does that at the completion cycle.
    ///
    /// `completion_cycles` depends on sync mode, transfer direction, and
    /// whether GP0 is currently receiving an A0h VRAM upload:
    ///
    /// - **A0h image upload**: calibrated against JaCzekanski/ps1-tests
    ///   build-158 `dma/chopping/psx.log`. Real hardware costs roughly one
    ///   cycle per word plus ten cycles per mode-1 block. Manual chopping
    ///   adds the programmed CPU window plus six arbitration cycles per DMA
    ///   window.
    /// - **Block command traffic**: follows the captured NOP-DMA sweep: DRAM
    ///   hyper-page transfer (17 clocks per 16 words), ten request-arbitration
    ///   clocks per block, and five fixed setup clocks.
    /// - **GPU→RAM download**: about 2.195 cycles per packed 32-bit word,
    ///   calibrated against the public 320×240 `gpu/bandwidth` silicon run.
    /// - **Linked list**: `total_words`, per Redux L568:
    ///   `scheduleGPUDMAIRQ(size)` where size is the
    ///   `gpuDmaChainSize` traversed count.
    fn run_dma_gpu(&mut self) -> Option<u32> {
        if !self.dma.is_channel_enabled(2) {
            return None;
        }
        let ch = &self.dma.channels[2];
        if (ch.channel_control >> 24) & 1 == 0 {
            return None;
        }
        let sync_mode = (ch.channel_control >> 9) & 0x3;
        let direction_to_device = ch.channel_control & 1 != 0;
        self.gpu.note_dma_transfer_started();
        let completion = match sync_mode {
            0 => self.dma_gpu_manual(direction_to_device),
            1 => self.dma_gpu_block(direction_to_device),
            2 => self.dma_gpu_linked_list(),
            _ => 0, // prohibited mode 3
        };
        self.service_gpu_irq();
        // Start bit stays set until the scheduled completion event
        // fires -- Redux's `gpuInterrupt` is where `clearDMABusy<2>()`
        // is called. BIOS polling of CHCR bit 24 during the transfer
        // window must read 1 until the IRQ fires.
        Some(completion)
    }

    fn dma_gpu_block(&mut self, to_device: bool) -> u32 {
        let ch = self.dma.channels[2];
        let mut addr = ch.base & 0x001F_FFFC;
        let bcr = ch.block_control;
        let block_size = bcr & 0xFFFF;
        let block_count = ((bcr >> 16) & 0xFFFF).max(1);
        let total_words = block_size.saturating_mul(block_count);
        let upload_active = to_device && self.gpu.vram_upload_active();
        let step = if (ch.channel_control >> 1) & 1 != 0 {
            // Decrement mode -- rarely used for GPU but handle for safety.
            0xFFFF_FFFCu32
        } else {
            4
        };
        if to_device {
            for _ in 0..total_words {
                let word = read_ram_u32(&self.ram[..], addr);
                self.gpu.gp0_push_dma(word);
                addr = addr.wrapping_add(step);
            }
        } else {
            for _ in 0..total_words {
                let word = self
                    .gpu
                    .read32_at(crate::gpu::GP0_ADDR, self.cycles)
                    .unwrap_or(0);
                write_ram_u32(&mut self.ram[..], addr, word);
                addr = addr.wrapping_add(step);
            }
        }
        // Image uploads are gated by the GPU's request line. The public
        // silicon sweep uses a 2048-word A0h upload and varies BCR's block
        // shape from 1x2048 through 128x16; one word + ten arbitration
        // cycles per block, plus the calibrated fixed pipeline term, keeps
        // the whole sweep within a few percent. Command traffic keeps the
        // hardware-calibrated NOP command-traffic model.
        if to_device {
            if upload_active {
                gpu_upload_block_cycles(total_words, block_count)
            } else {
                gpu_command_block_cycles(total_words, block_count)
            }
        } else {
            gpu_download_cycles(total_words)
        }
    }

    fn dma_gpu_manual(&mut self, to_device: bool) -> u32 {
        let ch = self.dma.channels[2];
        let mut addr = ch.base & 0x001F_FFFC;
        let total_words = dma_count_16(ch.block_control & 0xFFFF);
        let upload_active = to_device && self.gpu.vram_upload_active();
        let step = if (ch.channel_control >> 1) & 1 != 0 {
            0xFFFF_FFFC
        } else {
            4
        };

        if to_device {
            for _ in 0..total_words {
                let word = read_ram_u32(&self.ram[..], addr);
                self.gpu.gp0_push_dma(word);
                addr = addr.wrapping_add(step);
            }
        } else {
            for _ in 0..total_words {
                let word = self
                    .gpu
                    .read32_at(crate::gpu::GP0_ADDR, self.cycles)
                    .unwrap_or(0);
                write_ram_u32(&mut self.ram[..], addr, word);
                addr = addr.wrapping_add(step);
            }
        }

        if upload_active {
            gpu_upload_manual_cycles(total_words, ch.channel_control)
        } else {
            gpu_download_cycles(total_words)
        }
    }

    fn dma_gpu_linked_list(&mut self) -> u32 {
        let mut addr = self.dma.channels[2].base & 0x001F_FFFC;
        let transfer = self.gpu_linked_list_transfer;
        self.gpu_linked_list_transfer = self.gpu_linked_list_transfer.wrapping_add(1);
        // Count traversed headers and payload words, then apply the captured
        // DRAM-burst and per-node arbitration costs to the completion delay.
        let mut total_words: u32 = 0;
        let mut node_count: u32 = 0;
        // Bound the walk so a malformed list can't spin forever.
        for _ in 0..0x100_0000 {
            let header = read_ram_u32(&self.ram[..], addr);
            if self.gpu_linked_list_log_enabled {
                self.gpu_linked_list_log.push((transfer, addr, header));
            }
            node_count = node_count.saturating_add(1);
            let word_count = (header >> 24) & 0xFF;
            for i in 0..word_count {
                let word_addr = addr.wrapping_add(4 + i * 4);
                let word = read_ram_u32(&self.ram[..], word_addr);
                self.gpu.gp0_push_dma(word);
            }
            // Redux charges `(header >> 24) + 1` per node (see
            // `gpuDmaChainSize:474`). The `+1` covers the header
            // fetch; payload accounts for the rest.
            total_words = total_words.saturating_add(word_count + 1);
            // End-of-chain: hardware uses *any* pointer with bit 23
            // set (0x800000) as the terminator, not just the common
            // 0x00FF_FFFF sentinel. Matches Redux's
            // `while (!(addr & 0x800000))` at gpu.cc:483.
            if (header & 0x800000) != 0 {
                return gpu_command_linked_cycles(total_words, node_count);
            }
            addr = header & 0x00FF_FFFF;
        }
        gpu_command_linked_cycles(total_words, node_count)
    }

    /// Non-panicking byte read. Returns `None` for addresses outside
    /// any currently-mapped region. Diagnostic UIs (memory viewer,
    /// disassembler) use this to browse arbitrary ranges without
    /// crashing the emulator on unmapped addresses.
    ///
    /// Byte-granular reads of the GPU / timer / DMA / IRQ MMIO don't
    /// try to decompose the typed 32-bit registers -- they return the
    /// echo-buffer byte, which is fine for a diagnostic dump.
    /// Byte write that silently drops addresses outside mapped RAM /
    /// scratchpad. Used by HLE BIOS helpers (like `memset`) where
    /// a buggy guest program shouldn't panic the host.
    pub fn write8_safe(&mut self, virt: u32, value: u8) -> bool {
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            self.ram[(phys as usize) % memory::ram::SIZE] = value;
            true
        } else if (memory::scratchpad::BASE
            ..memory::scratchpad::BASE + memory::scratchpad::SIZE as u32)
            .contains(&phys)
        {
            self.scratchpad[(phys - memory::scratchpad::BASE) as usize] = value;
            true
        } else {
            false
        }
    }

    /// Side-effect-free byte read -- used by diagnostics (trace
    /// printer, parity oracle) that must not perturb peripheral
    /// state. Returns `None` for addresses that aren't backed by
    /// plain memory (CD-ROM FIFO, timers, etc.); the caller is
    /// expected to read those through [`Bus::read8`] if needed.
    pub fn try_read8(&self, virt: u32) -> Option<u8> {
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            return Some(self.ram[(phys as usize) % memory::ram::SIZE]);
        }
        if (memory::scratchpad::BASE..memory::scratchpad::BASE + memory::scratchpad::SIZE as u32)
            .contains(&phys)
        {
            return Some(self.scratchpad[(phys - memory::scratchpad::BASE) as usize]);
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return Some(self.bios[(phys - memory::bios::BASE) as usize]);
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            return Some(self.io[(phys - memory::io::BASE) as usize]);
        }
        if (memory::expansion1::BASE..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
        {
            return Some(0xFF);
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return Some(0xFF);
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return Some(0xFF);
        }
        None
    }

    /// Read a 32-bit little-endian word from a virtual address.
    ///
    /// Panics on any address that does not resolve to a currently-mapped
    /// region. This is intentional -- unmapped reads during development
    /// should surface immediately, not return silent zeros.
    ///
    /// `&mut self` because some peripherals (notably CD-ROM) mutate on
    /// read -- popping response FIFOs, advancing data-transfer state.
    pub fn read8(&mut self, virt: u32) -> u8 {
        let phys = to_physical(virt);
        let value = self.read8_impl(virt, phys);
        let shift = (phys & 3) * 8;
        self.data_bus_latch =
            (self.data_bus_latch & !(0xFF << shift)) | (u32::from(value) << shift);
        self.trace_mmio(MmioKind::R8, phys, value as u32);
        value
    }

    fn read8_impl(&mut self, virt: u32, phys: u32) -> u8 {
        if phys < memory::ram::MIRROR_END {
            return self.ram[(phys as usize) % memory::ram::SIZE];
        }
        if CdRom::contains(phys) {
            return self.cdrom.read8(phys);
        }
        if let Some(offset) = scratchpad_offset(virt, phys) {
            return self.scratchpad[offset];
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return self.bios[(phys - memory::bios::BASE) as usize];
        }
        if (memory::expansion1::BASE..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
        {
            return 0xFF;
        }
        if MemoryControl::contains(phys) {
            return self.memory_control.read(phys, AccessWidth::Byte) as u8;
        }
        if phys == IRQ_STAT_ADDR {
            return self.irq.stat() as u8;
        }
        if phys == IRQ_MASK_ADDR {
            return self.irq.mask() as u8;
        }
        if Timers::contains(phys) {
            self.service_timers();
            let aligned = phys & !3;
            return (self.timers.read32(aligned) >> ((phys & 3) * 8)) as u8;
        }
        if Dma::contains(phys) {
            return self.dma.read8(phys);
        }
        if let Some(value) = self.gpu.read32_at(phys & !3, self.cycles) {
            self.service_gpu_irq();
            return (value >> ((phys & 3) * 8)) as u8;
        }
        if Spu::contains(phys) {
            let aligned = phys & !1;
            return (self.spu.read16_at(aligned, self.cycles) >> ((phys & 1) * 8)) as u8;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            return self.sio0.read8(phys).unwrap_or(0);
        }
        if Sio1::contains(phys) {
            let aligned = phys & !3;
            return (self.sio1.read32(aligned) >> ((phys & 3) * 8)) as u8;
        }
        if crate::mdec::Mdec::contains(phys) {
            let aligned = phys & !3;
            return (self.mdec.read32(aligned) >> ((phys & 3) * 8)) as u8;
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            return self.io[(phys - memory::io::BASE) as usize];
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return 0xFF;
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return 0xFF;
        }
        // Unmapped read on real hardware returns the last bus
        // value (essentially random from software's POV). Many
        // games have wild pointers here and there; panicking
        // would halt perfectly-good emulation. Return 0xFF so
        // software sees "no peripheral." Log once at non-trivial
        // addresses so a real bug doesn't hide in the noise.
        if self.log_unmapped_read_once(virt) {
            eprintln!("[bus] unmapped read8 @ virt={virt:#010x} phys={phys:#010x}");
        }
        0xFF
    }

    /// Read a 16-bit little-endian half-word from a virtual address.
    /// Unmapped regions behave identically to [`Bus::read8`] (see the
    /// region-by-region notes there).
    ///
    /// `&mut self` for the same reason as `read8` -- peripheral-side
    /// effects.
    pub fn read16(&mut self, virt: u32) -> u16 {
        let phys = to_physical(virt);
        let value = self.read16_impl(virt, phys);
        let shift = (phys & 2) * 8;
        self.data_bus_latch =
            (self.data_bus_latch & !(0xFFFF << shift)) | (u32::from(value) << shift);
        self.trace_mmio(MmioKind::R16, phys, value as u32);
        value
    }

    fn read16_impl(&mut self, virt: u32, phys: u32) -> u16 {
        if phys < memory::ram::MIRROR_END {
            let off = (phys as usize) % memory::ram::SIZE;
            return u16::from_le_bytes([self.ram[off], self.ram[off + 1]]);
        }
        if CdRom::contains(phys) {
            let byte = self.cdrom.read8(phys) as u16;
            return byte | (byte << 8);
        }
        if let Some(off) = scratchpad_offset(virt, phys) {
            return u16::from_le_bytes([self.scratchpad[off], self.scratchpad[off + 1]]);
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            let off = (phys - memory::bios::BASE) as usize;
            return u16::from_le_bytes([self.bios[off], self.bios[off + 1]]);
        }
        if (memory::expansion1::BASE..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
        {
            return 0xFFFF;
        }
        if MemoryControl::contains(phys) {
            return self.memory_control.read(phys, AccessWidth::Half) as u16;
        }
        // Same rationale as in `write16_impl`: BIOS reads `I_STAT` /
        // `I_MASK` via `lhu` and would otherwise see the stale echo
        // buffer instead of the live interrupt-controller state.
        if phys == IRQ_STAT_ADDR {
            return self.irq.stat() as u16;
        }
        if phys == IRQ_MASK_ADDR {
            return self.irq.mask() as u16;
        }
        // Timer registers are 16-bit on hardware; the BIOS's
        // counter-polling loop uses `lhu`. Without this dispatch the
        // counter reads zero from the io[] echo buffer and the loop
        // never sees the tick advance.
        if Timers::contains(phys) {
            self.service_timers();
            return self.timers.read32(phys) as u16;
        }
        if Dma::contains(phys) {
            return self.dma.read16(phys);
        }
        if Spu::contains(phys) {
            return self.spu.read16_at(phys, self.cycles);
        }
        if let Some(value) = self.gpu.read32_at(phys & !3, self.cycles) {
            self.service_gpu_irq();
            return (value >> ((phys & 2) * 8)) as u16;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            return self.sio0.read16(phys).unwrap_or(0);
        }
        if Sio1::contains(phys) {
            return (self.sio1.read32(phys & !3) >> ((phys & 2) * 8)) as u16;
        }
        if crate::mdec::Mdec::contains(phys) {
            return (self.mdec.read32(phys & !3) >> ((phys & 2) * 8)) as u16;
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            let off = (phys - memory::io::BASE) as usize;
            return u16::from_le_bytes([self.io[off], self.io[off + 1]]);
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return 0xFFFF;
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return 0xFFFF;
        }
        if self.log_unmapped_read_once(virt) {
            eprintln!("[bus] unmapped read16 @ virt={virt:#010x} phys={phys:#010x}");
        }
        0xFFFF
    }

    /// Read a 32-bit little-endian word from a virtual address. This is
    /// the instruction-fetch path.
    ///
    /// `&mut self` because CD-ROM byte reads (composited into a u32 for
    /// the rare case software word-accesses that range) mutate.
    #[inline]
    pub fn read32(&mut self, virt: u32) -> u32 {
        let phys = to_physical(virt);
        // RAM first, ahead of the telemetry hook, the MMIO trace, and
        // the peripheral dispatch cascade. Exact by address disjointness:
        // the telemetry ports live in expansion-2 and `is_mmio` covers
        // only the I/O window, so no RAM access ever hits either. This
        // is the hottest load in the emulator -- every instruction
        // fetch and almost every guest load lands here -- and inlining
        // just this arm into `Cpu::execute_one` skips an outlined call.
        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            let value = read_u32_le(&self.ram[offset..]);
            self.data_bus_latch = value;
            return value;
        }
        let value = self.read32_impl(virt, phys);
        self.data_bus_latch = value;
        self.trace_mmio(MmioKind::R32, phys, value);
        value
    }

    /// Instruction-side word fetch used by uncached execution and I-cache
    /// fills. The R3000A's instruction path does not replace the CPU data-bus
    /// hold value that leaks into undriven MMIO lanes.
    pub(crate) fn read_instruction32(&mut self, virt: u32) -> u32 {
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            return read_u32_le(&self.ram[offset..]);
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            let offset = (phys - memory::bios::BASE) as usize;
            return read_u32_le(&self.bios[offset..]);
        }
        self.read32(virt)
    }

    /// Side-effect-free peek at a 32-bit instruction word. Returns
    /// `None` when `virt` isn't in RAM or BIOS -- those are the only
    /// places PS1 code ever executes from, so a `None` here is
    /// cheap to treat as "can't be a GTE cofun" at the call site.
    ///
    /// Used by [`crate::Cpu::should_take_interrupt`] -- see the
    /// "interrupts vs GTE" hardware bug workaround.
    pub fn peek_instruction(&self, virt: u32) -> Option<u32> {
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            Some(read_u32_le(&self.ram[offset..]))
        } else if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32)
            .contains(&phys)
        {
            let offset = (phys - memory::bios::BASE) as usize;
            Some(read_u32_le(&self.bios[offset..]))
        } else {
            None
        }
    }

    fn read32_impl(&mut self, virt: u32, phys: u32) -> u32 {
        if let Some(value) = self.telemetry.observe_read32(phys, self.cycles) {
            return value;
        }
        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            return read_u32_le(&self.ram[offset..]);
        }

        if let Some(offset) = scratchpad_offset(virt, phys) {
            return read_u32_le(&self.scratchpad[offset..]);
        }

        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            let offset = (phys - memory::bios::BASE) as usize;
            return read_u32_le(&self.bios[offset..]);
        }

        if (memory::expansion1::BASE..memory::expansion1::BASE + memory::expansion1::SIZE as u32)
            .contains(&phys)
        {
            return 0xFFFF_FFFF;
        }

        if MemoryControl::contains(phys) {
            return self.memory_control.read(phys, AccessWidth::Word);
        }

        if phys == IRQ_STAT_ADDR {
            return self.irq.stat();
        }
        if phys == IRQ_MASK_ADDR {
            return (self.data_bus_latch & 0xFFFF_0000) | self.irq.mask();
        }
        if Timers::contains(phys) {
            if self.ram_write_buffer_ready_cycle > self.cycles {
                self.advance_cycles((self.ram_write_buffer_ready_cycle - self.cycles) as u32);
            }
            self.ram_write_buffer_ready_cycle = self.cycles;
            self.service_timers();
            return (self.data_bus_latch & 0xFFFF_0000) | self.timers.read32(phys);
        }
        if Dma::contains(phys) {
            return self.dma.read32(phys);
        }
        if let Some(v) = self.gpu.read32_at(phys, self.cycles) {
            self.service_gpu_irq();
            return v;
        }
        if Spu::contains(phys) {
            return self.spu.read32_at(phys, self.cycles);
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            return self.sio0.read32(phys).unwrap_or(0);
        }
        if Sio1::contains(phys) {
            return self.sio1.read32(phys);
        }
        if CdRom::contains(phys) {
            // The CD controller is attached through an 8-bit bridge. Wider
            // CPU reads sample the selected byte port on every active lane.
            let byte = self.cdrom.read8(phys) as u32;
            return byte * 0x0101_0101;
        }
        if crate::mdec::Mdec::contains(phys) {
            return self.mdec.read32(phys);
        }

        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            let offset = (phys - memory::io::BASE) as usize;
            return read_u32_le(&self.io[offset..]);
        }

        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
            || (memory::expansion3::BASE
                ..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
                .contains(&phys)
        {
            return 0xFFFF_FFFF;
        }

        if self.log_unmapped_read_once(virt) {
            eprintln!("[bus] unmapped read32 @ virt={virt:#010x} phys={phys:#010x}");
        }
        0xFFFF_FFFF
    }

    /// Write a 32-bit little-endian word to a virtual address.
    ///
    /// - **RAM / scratchpad**: committed to the backing storage.
    /// - **BIOS ROM**: silently dropped (ROM is read-only).
    /// - **Cache-control register** `0xFFFE_0130`: normally intercepted
    ///   by the CPU; silently dropped here as a defensive fallback for
    ///   direct bus clients.
    /// - **MMIO window** `0x1F80_1000..0x1F80_2000`: silently dropped
    ///   for now. Individual peripheral stubs will attach as we add
    ///   them; until then, BIOS's memory-controller init writes are
    ///   no-ops for architectural parity.
    #[inline]
    pub fn write32(&mut self, virt: u32, value: u32) {
        if virt == memory::cache_control::ADDR {
            return;
        }

        let phys = to_physical(virt);
        // RAM-first fast path; see `read32` for the exactness argument
        // (every telemetry / trace / peripheral address is outside RAM).
        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            self.ram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            return;
        }
        self.trace_mmio(MmioKind::W32, phys, value);
        self.write32_impl(virt, phys, value);
    }

    /// CPU `SW` transaction. Unlike host-side initialization through
    /// [`Bus::write32`], an architectural store drives the data-bus latch.
    /// GP0 writes additionally stall behind queued GPU execution work: on
    /// silicon the store itself blocks once the input path is occupied even
    /// though DMA-ready GPUSTAT.28 can remain high for CPU-fed rendering.
    pub(crate) fn cpu_write32(&mut self, virt: u32, value: u32) {
        self.data_bus_latch = value;
        let store_stall = self.cpu_write_stalls(virt);
        self.add_cycles(store_stall);
        if to_physical(virt) < memory::ram::MIRROR_END {
            self.ram_write_buffer_ready_cycle = self.cycles.saturating_add(8);
        }
        if to_physical(virt) == crate::gpu::GP0_ADDR {
            if !self.gpu.note_cpu_gp0_arrival() {
                // Strict-FIFO mode: silicon loses the word outright, and a
                // lost word stalls nothing.
                return;
            }
            let stall = self.gpu.cpu_gp0_write_stall();
            self.add_cycles(stall);
        }
        self.write32(virt, value);
    }

    fn write32_impl(&mut self, virt: u32, phys: u32, value: u32) {
        if self.telemetry.observe_write32(phys, value, self.cycles) {
            return;
        }
        if MemoryControl::contains(phys) {
            self.memory_control.write(phys, AccessWidth::Word, value);
            return;
        }
        if phys == IRQ_STAT_ADDR {
            self.irq.write_stat_at(value, self.cycles);
            return;
        }
        if phys == IRQ_MASK_ADDR {
            self.irq.write_mask_at(value, self.cycles);
            return;
        }
        if Timers::contains(phys) {
            // Advance pre-write so an in-flight tick fires its IRQ
            // before the write resets / re-arms the counter, then
            // perform the write.
            self.service_timers();
            self.timers.write32(phys, value, self.cycles);
            return;
        }
        if Dma::contains(phys) {
            if self.dma.write32(phys, value) {
                self.irq.raise(IrqSource::Dma);
            }
            // Only a CHCR write with bit 24 set starts a transfer --
            // matches Redux's `dmaExec<N>` dispatcher in `psxhw.cc`,
            // which runs from the per-channel `case 0x1f80_1088/98/
            // a8/b8/c8/e8` arms. Crucially it runs ONLY channel N,
            // not a sweep across all channels.
            //
            // Earlier we called `maybe_run_dma()` (iterates every
            // channel) on every CHCR trigger. If another channel's
            // transfer was still in-flight (start bit still set,
            // awaiting its scheduled completion), the sweep re-ran
            // it, scheduling a second completion that overwrote the
            // first. Example caught by `probe_dma_schedules`:
            // writing channel 6's CHCR at cycle 46246689 re-ran
            // channel 2's in-flight DMA, pushing its IRQ target from
            // 46247457 to 46247720 -- which is exactly the -2488-
            // cycle drift `probe_cycle_first_divergence` flagged at
            // step 19474544.
            let offset = phys.wrapping_sub(Dma::BASE);
            let field = offset & 0xF;
            let is_chcr_write = field == 0x8;
            let start_bit_set = value & (1 << 24) != 0;
            if is_chcr_write && start_bit_set {
                let channel = ((offset & 0x70) >> 4) as usize;
                self.run_dma_channel(channel);
            }
            return;
        }
        if self.gpu.write32_at(phys, value, self.cycles) {
            self.service_gpu_irq();
            if phys == crate::gpu::GP1_ADDR && (value >> 24) == 0x08 {
                if value & (1 << 3) != 0 {
                    self.set_pal_mode();
                } else {
                    self.set_ntsc_mode();
                }
            }
            return;
        }
        if Spu::contains(phys) {
            self.spu.write32_at(phys, value, self.cycles);
            self.service_spu_irq();
            return;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            self.sio0.write32_at(phys, value, self.cycles);
            self.service_sio0();
            return;
        }
        if Sio1::contains(phys) {
            self.sio1.write16(phys, value as u16);
            return;
        }
        if CdRom::contains(phys) {
            self.write_cdrom_lanes(phys, value, 4);
            return;
        }
        if crate::mdec::Mdec::contains(phys) {
            self.mdec.write32(phys, value);
            return;
        }

        let bytes = value.to_le_bytes();

        if phys < memory::ram::MIRROR_END {
            let offset = (phys as usize) % memory::ram::SIZE;
            self.ram[offset..offset + 4].copy_from_slice(&bytes);
            return;
        }

        if let Some(offset) = scratchpad_offset(virt, phys) {
            self.scratchpad[offset..offset + 4].copy_from_slice(&bytes);
            return;
        }

        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            let offset = (phys - memory::io::BASE) as usize;
            self.io[offset..offset + 4].copy_from_slice(&bytes);
            return;
        }

        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return;
        }

        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return;
        }

        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return;
        }

        if self.log_unmapped_write_once(virt) {
            eprintln!(
                "[bus] unmapped write32 @ virt={virt:#010x} phys={phys:#010x} value={value:#010x}"
            );
        }
    }

    /// Write a byte to a virtual address. Unmapped writes in MMIO /
    /// expansion / BIOS ranges are silently dropped (same rationale as
    /// [`Bus::write32`]).
    pub fn write8(&mut self, virt: u32, value: u8) {
        let phys = to_physical(virt);
        self.trace_mmio(MmioKind::W8, phys, value as u32);
        self.write8_impl(virt, phys, value);
    }

    /// CPU `SB` transaction. The RAM byte lane receives only the low byte, but
    /// the R3000A still drives all 32 source-register bits on the peripheral
    /// bus. Several PS1 devices ignore byte enables and latch that complete
    /// word; direct bus clients keep using [`Bus::write8`] for an actual byte.
    pub(crate) fn cpu_write8(&mut self, virt: u32, source: u32) {
        self.data_bus_latch = source;
        let store_stall = self.cpu_write_stalls(virt);
        self.add_cycles(store_stall);
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            self.ram_write_buffer_ready_cycle = self.cycles.saturating_add(8);
        }
        self.trace_mmio(MmioKind::W8, phys, source & 0xFF);
        if !self.cpu_write_narrow_mmio(virt, phys, source, AccessWidth::Byte) {
            self.write8_impl(virt, phys, source as u8);
        }
    }

    /// CPU `SH` transaction; see [`Bus::cpu_write8`] for why `source` retains
    /// the complete GPR value even though RAM consumes only its low halfword.
    pub(crate) fn cpu_write16(&mut self, virt: u32, source: u32) {
        self.data_bus_latch = source;
        let store_stall = self.cpu_write_stalls(virt);
        self.add_cycles(store_stall);
        let phys = to_physical(virt);
        if phys < memory::ram::MIRROR_END {
            self.ram_write_buffer_ready_cycle = self.cycles.saturating_add(8);
        }
        self.trace_mmio(MmioKind::W16, phys, source & 0xFFFF);
        if !self.cpu_write_narrow_mmio(virt, phys, source, AccessWidth::Half) {
            self.write16_impl(virt, phys, source as u16);
        }
    }

    fn cpu_write_narrow_mmio(
        &mut self,
        virt: u32,
        phys: u32,
        source: u32,
        width: AccessWidth,
    ) -> bool {
        if phys == IRQ_STAT_ADDR {
            self.irq.write_stat_at(source, self.cycles);
            return true;
        }
        if phys == IRQ_MASK_ADDR {
            self.irq.write_mask_at(source, self.cycles);
            return true;
        }
        if Timers::contains(phys) {
            self.service_timers();
            if std::env::var_os("PSOXIDE_TRACE_TIMERS").is_some()
                && (phys - Timers::BASE) % Timers::STRIDE == 4
            {
                eprintln!(
                    "[timers] mode-write t{} value={:04x} cycle={} next-vblank={} period={}",
                    (phys - Timers::BASE) / Timers::STRIDE,
                    source & 0xFFFF,
                    self.cycles,
                    self.next_vblank_cycle(),
                    self.vblank_period
                );
            }
            self.timers.write32(phys & !3, source, self.cycles);
            return true;
        }
        if Dma::contains(phys) {
            // DMA registers ignore byte enables and observe the complete CPU
            // source word. Reuse the normal word path so CHCR side effects and
            // IRQ behavior remain centralized.
            self.write32_impl(virt, phys & !3, source);
            return true;
        }
        if Spu::contains(phys) {
            self.spu.write16_at(phys & !1, source as u16, self.cycles);
            self.service_spu_irq();
            return true;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            self.sio0.write16_at(phys & !1, source as u16, self.cycles);
            self.service_sio0();
            return true;
        }
        if Sio1::contains(phys) {
            self.sio1.write16(phys & !1, source as u16);
            return true;
        }
        if CdRom::contains(phys) {
            self.write_cdrom_lanes(phys, source, if width == AccessWidth::Byte { 1 } else { 2 });
            return true;
        }
        let aligned = phys & !3;
        if self.gpu.write32_at(aligned, source, self.cycles) {
            self.service_gpu_irq();
            return true;
        }
        if crate::mdec::Mdec::contains(phys) {
            self.mdec.write32(aligned, source);
            return true;
        }
        false
    }

    fn write_cdrom_lanes(&mut self, phys: u32, value: u32, lanes: u32) {
        for lane in 0..lanes {
            let byte = (value >> (lane * 8)) as u8;
            // The CD-ROM controller is an 8-bit peripheral. A wider CPU
            // store repeats its byte lanes onto the selected port rather than
            // walking across the four adjacent CD-ROM registers. Real-hardware
            // bit-width probes depend on the final lane becoming the new index.
            if self.cdrom.write8_at(phys, byte, self.cycles) {
                self.irq.raise(IrqSource::Cdrom);
            }
        }
    }

    fn write8_impl(&mut self, virt: u32, phys: u32, value: u8) {
        if phys < memory::ram::MIRROR_END {
            self.ram[(phys as usize) % memory::ram::SIZE] = value;
            return;
        }
        // Expansion-2 debug console char-out (PCSX-Redux convention,
        // 0x1F802080): the port the public test suites print to
        // (JaCzekanski ps1-tests, Redux homebrew). Forward to stdout so
        // their console-verified corpora run headless with capturable TTY.
        if phys == 0x1F80_2080 {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&[value]);
            if value == b'\n' {
                let _ = out.flush();
            }
            return;
        }
        if let Some(offset) = scratchpad_offset(virt, phys) {
            self.scratchpad[offset] = value;
            return;
        }
        if MemoryControl::contains(phys) {
            self.memory_control
                .write(phys, AccessWidth::Byte, value as u32);
            return;
        }
        // CDROM is byte-addressed (4 registers, each switching meaning by
        // index). Without this dispatch the BIOS's `sb` to 1F801800..1803
        // ends up in the io[] echo buffer and the CDROM module never sees
        // the index switch / param push / command write -- so commands are
        // silently dropped, no IRQ ever fires, and the BIOS stalls in the
        // event-wait loop after the Sony intro.
        if CdRom::contains(phys) {
            // Thread `self.cycles` through so the CDROM scheduler
            // anchors response-IRQ deadlines on the exact cycle at
            // the cmd-port write, matching Redux's `AddIrqQueue`.
            if self.cdrom.write8_at(phys, value, self.cycles) {
                self.irq.raise(IrqSource::Cdrom);
            }
            return;
        }
        if Dma::contains(phys) {
            if self.dma.write8(phys, value) {
                self.irq.raise(IrqSource::Dma);
            }
            return;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            self.sio0.write8_at(phys, value, self.cycles);
            self.service_sio0();
            return;
        }
        if Sio1::contains(phys) {
            self.sio1.write16(phys & !1, value as u16);
            return;
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            self.io[(phys - memory::io::BASE) as usize] = value;
            return;
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return;
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return;
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return;
        }
        if self.log_unmapped_write_once(virt) {
            eprintln!(
                "[bus] unmapped write8 @ virt={virt:#010x} phys={phys:#010x} value={value:#04x}"
            );
        }
    }

    /// Write a 16-bit half-word to a virtual address. Same unmapped-region
    /// policy as [`Bus::write32`].
    pub fn write16(&mut self, virt: u32, value: u16) {
        let phys = to_physical(virt);
        self.trace_mmio(MmioKind::W16, phys, value as u32);
        self.write16_impl(virt, phys, value);
    }

    fn write16_impl(&mut self, virt: u32, phys: u32, value: u16) {
        let bytes = value.to_le_bytes();
        if phys < memory::ram::MIRROR_END {
            let off = (phys as usize) % memory::ram::SIZE;
            self.ram[off..off + 2].copy_from_slice(&bytes);
            return;
        }
        if let Some(off) = scratchpad_offset(virt, phys) {
            self.scratchpad[off..off + 2].copy_from_slice(&bytes);
            return;
        }
        if MemoryControl::contains(phys) {
            self.memory_control
                .write(phys, AccessWidth::Half, value as u32);
            return;
        }
        if CdRom::contains(phys) {
            self.write_cdrom_lanes(phys, value as u32, 2);
            return;
        }
        // The BIOS uses `sh` (16-bit store) to write `I_MASK` and ack
        // `I_STAT`. Without this dispatch the value lands in the io[]
        // echo buffer and the IRQ controller never sees it -- meaning
        // mask stays 0 and pending() always returns false, so no IRQ
        // exception is ever taken.
        if phys == IRQ_STAT_ADDR {
            self.irq.write_stat_at(value as u32, self.cycles);
            return;
        }
        if phys == IRQ_MASK_ADDR {
            self.irq.write_mask_at(value as u32, self.cycles);
            return;
        }
        // Timer registers are 16-bit on hardware.
        if Timers::contains(phys) {
            self.service_timers();
            self.timers.write32(phys, value as u32, self.cycles);
            return;
        }
        if Dma::contains(phys) {
            if self.dma.write16(phys, value) {
                self.irq.raise(IrqSource::Dma);
            }
            return;
        }
        if Spu::contains(phys) {
            self.spu.write16_at(phys, value, self.cycles);
            self.service_spu_irq();
            return;
        }
        if Sio0::contains(phys) {
            self.service_sio0();
            self.sio0.write16_at(phys, value, self.cycles);
            self.service_sio0();
            return;
        }
        if Sio1::contains(phys) {
            self.sio1.write16(phys, value);
            return;
        }
        if self.gpu.write32_at(phys & !3, value as u32, self.cycles) {
            self.service_gpu_irq();
            return;
        }
        if crate::mdec::Mdec::contains(phys) {
            self.mdec.write32(phys & !3, value as u32);
            return;
        }
        if (memory::io::BASE..memory::io::BASE + memory::io::SIZE as u32).contains(&phys) {
            let off = (phys - memory::io::BASE) as usize;
            self.io[off..off + 2].copy_from_slice(&bytes);
            return;
        }
        if (memory::expansion2::BASE..memory::expansion2::BASE + memory::expansion2::SIZE as u32)
            .contains(&phys)
        {
            return;
        }
        if (memory::expansion3::BASE..memory::expansion3::BASE + memory::expansion3::SIZE as u32)
            .contains(&phys)
        {
            return;
        }
        if (memory::bios::BASE..memory::bios::BASE + memory::bios::SIZE as u32).contains(&phys) {
            return;
        }
        if self.log_unmapped_write_once(virt) {
            eprintln!(
                "[bus] unmapped write16 @ virt={virt:#010x} phys={phys:#010x} value={value:#06x}"
            );
        }
    }
}

/// Resolve a scratchpad access to its in-scratchpad byte offset, honoring
/// the segment rule. The scratchpad is the CPU D-cache repurposed as fast
/// RAM, so it answers only through cached segments (KUSEG, KSEG0). A KSEG1
/// access is uncached and bypasses the cache, so on hardware it never
/// reaches the scratchpad; model that as unmapped. Returns `None` for a
/// non-scratchpad physical address or any KSEG1 virtual address.
#[inline]
fn scratchpad_offset(virt: u32, phys: u32) -> Option<usize> {
    // KSEG1 spans 0xA000_0000..0xC000_0000 (uncached).
    if (0xA000_0000..0xC000_0000).contains(&virt) {
        return None;
    }
    let base = memory::scratchpad::BASE;
    if (base..base + memory::scratchpad::SIZE as u32).contains(&phys) {
        Some((phys - base) as usize)
    } else {
        None
    }
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[inline]
fn dma_count_16(raw: u32) -> u32 {
    match raw & 0xFFFF {
        0 => 0x1_0000,
        count => count,
    }
}

/// Completion delay for block-mode GPU command traffic (as opposed to A0h
/// image data). Main RAM DMA hyper-page mode costs about 17 clocks per 16
/// words; each BCR block then crosses the GPU request arbiter once.
#[inline]
fn gpu_command_block_cycles(total_words: u32, block_count: u32) -> u32 {
    total_words
        .saturating_add(total_words.div_ceil(16))
        .saturating_add(block_count.saturating_mul(10))
        .saturating_add(5)
}

/// Linked-list command traffic pays the same DRAM burst slope, with a larger
/// per-node header/pointer arbitration cost than contiguous BCR blocks.
#[inline]
fn gpu_command_linked_cycles(total_words: u32, node_count: u32) -> u32 {
    total_words
        .saturating_add(total_words.div_ceil(16))
        .saturating_add(node_count.saturating_mul(15))
        .saturating_add(5)
}

/// Completion delay for a mode-1 RAM-to-GPU A0h image upload.
///
/// Fitted to the 65 block shapes in JaCzekanski/ps1-tests build-158's
/// `dma/chopping` real-console log. The source transfers approximately one
/// word per cycle and pays about ten GPU-DREQ arbitration cycles per block.
/// The 250-cycle term is the fixed upload/FIFO pipeline cost after removing
/// the test harness's timer and polling overhead.
#[inline]
fn gpu_upload_block_cycles(total_words: u32, block_count: u32) -> u32 {
    total_words
        .saturating_add(block_count.saturating_mul(10))
        .saturating_add(250)
}

/// Completion delay for a mode-0 RAM-to-GPU A0h image upload.
///
/// Without chopping the public 2048-word silicon capture takes 2196 cycles
/// including about 25 cycles of timer/polling harness, hence `words + 123`.
/// Chopping alternates `2^N` DMA words with `2^M` CPU clocks and pays six
/// arbitration clocks per window.
#[inline]
fn gpu_upload_manual_cycles(total_words: u32, channel_control: u32) -> u32 {
    let base = total_words.saturating_add(123);
    if channel_control & (1 << 8) == 0 {
        return base;
    }

    let dma_window = 1u32 << ((channel_control >> 16) & 7);
    let cpu_window = 1u32 << ((channel_control >> 20) & 7);
    let windows = total_words.div_ceil(dma_window);
    // Two corners in the silicon sweep pay one extra arbitration clock per
    // window. Keep them explicit instead of perturbing the regular model:
    // the other 62 combinations follow the six-clock rule closely.
    let extra_arbitration =
        (dma_window == 2 && cpu_window == 1) || (dma_window == 1 && cpu_window == 2);
    let arbitration = if extra_arbitration { 7 } else { 6 };
    base.saturating_add(windows.saturating_mul(arbitration + cpu_window))
}

/// Completion delay for a GPU-to-RAM packed-pixel download.
///
/// JaCzekanski/ps1-tests build-158's `gpu/bandwidth` capture transfers a
/// 320×240 16bpp image 400 times. Its 15,770 HBlank duration resolves to
/// roughly 2.195 CPU cycles per 32-bit GPUREAD word after subtracting the
/// command/poll harness. This path is intentionally independent from the
/// faster RAM-to-GPU upload request cadence.
#[inline]
fn gpu_download_cycles(total_words: u32) -> u32 {
    total_words.saturating_mul(281).div_ceil(128)
}

/// Word read from a RAM slice at a physical RAM offset (already masked
/// to the 2 MiB range). Used by the DMA-GPU paths to pull command
/// words without going through the full bus dispatch.
fn read_ram_u32(ram: &[u8], phys: u32) -> u32 {
    let offset = (phys & 0x001F_FFFF) as usize;
    if offset + 4 <= ram.len() {
        u32::from_le_bytes([
            ram[offset],
            ram[offset + 1],
            ram[offset + 2],
            ram[offset + 3],
        ])
    } else {
        0
    }
}

fn write_ram_u32(ram: &mut [u8], phys: u32, value: u32) {
    let offset = (phys & 0x001F_FFFF) as usize;
    if offset + 4 <= ram.len() {
        ram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// Halfword read from a RAM slice at a physical RAM offset. SPU DMA is
/// halfword-based in Redux: the BCR product counts 16-bit samples and
/// the completion delay is half that product.
fn read_ram_u16(ram: &[u8], phys: u32) -> u16 {
    let offset = (phys & 0x001F_FFFF) as usize;
    if offset + 2 <= ram.len() {
        u16::from_le_bytes([ram[offset], ram[offset + 1]])
    } else {
        0
    }
}

fn write_ram_u16(ram: &mut [u8], phys: u32, value: u16) {
    let offset = (phys & 0x001F_FFFF) as usize;
    if offset + 2 <= ram.len() {
        ram[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
}

fn mdec_dma_word_count(block_control: u32, channel_control: u32) -> u32 {
    match (channel_control >> 9) & 0x3 {
        0 => block_control & 0xFFFF,
        1 => {
            let block_size = block_control & 0xFFFF;
            let block_count = (block_control >> 16).max(1) & 0xFFFF;
            block_size * block_count
        }
        _ => 0,
    }
}

fn zeroed_box<const N: usize>() -> Box<[u8; N]> {
    // Allocates a zero-initialised slice and converts it. The try_into
    // cannot fail because the source slice has exactly N elements.
    vec![0u8; N]
        .into_boxed_slice()
        .try_into()
        .expect("vec length matches const N")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irq::IrqSource;
    use crate::scheduler::EventSlot;

    fn synthetic_bios() -> Vec<u8> {
        // 512 KiB. First word is 0xDEADBEEF little-endian, then zeros.
        let mut bios = vec![0u8; memory::bios::SIZE];
        bios[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        bios
    }

    #[test]
    fn rejects_wrong_sized_bios() {
        assert!(matches!(
            Bus::new(vec![0u8; 1024]),
            Err(BusError::BiosSize { .. })
        ));
    }

    #[test]
    fn new_without_bios_exposes_retail_reset_vector_word() {
        let mut bus = Bus::new_without_bios();
        assert_eq!(bus.read32(0xBFC0_0000), 0x3C08_0013);
        assert_eq!(bus.read32(0xBFC0_0004), 0);
    }

    #[test]
    fn default_video_region_is_ntsc() {
        let bus = Bus::new(synthetic_bios()).unwrap();
        assert_eq!(bus.hsync_cycles(), HSYNC_CYCLES_NTSC);
        assert_eq!(bus.vblank_period(), VBLANK_PERIOD_CYCLES_NTSC);
    }

    #[test]
    fn set_pal_mode_switches_timings_and_reschedules_vblank() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.set_pal_mode();
        assert_eq!(bus.hsync_cycles(), HSYNC_CYCLES_PAL);
        assert_eq!(bus.vblank_period(), VBLANK_PERIOD_CYCLES_PAL);
        // VBlank slot should be armed for the PAL first-VBlank cycle.
        let target = bus
            .scheduler
            .target(crate::scheduler::EventSlot::VBlank)
            .expect("VBlank must be rescheduled on PAL switch");
        assert_eq!(target, FIRST_VBLANK_CYCLE_PAL);
    }

    #[test]
    fn gp1_display_mode_switches_video_region() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.write32(crate::gpu::GP1_ADDR, 0x0800_0008);
        assert_eq!(bus.hsync_cycles(), HSYNC_CYCLES_PAL);
        assert_eq!(bus.vblank_period(), VBLANK_PERIOD_CYCLES_PAL);
        assert_eq!(
            bus.scheduler
                .target(crate::scheduler::EventSlot::VBlank)
                .unwrap(),
            FIRST_VBLANK_CYCLE_PAL,
        );

        bus.write32(crate::gpu::GP1_ADDR, 0x0800_0000);
        assert_eq!(bus.hsync_cycles(), HSYNC_CYCLES_NTSC);
        assert_eq!(bus.vblank_period(), VBLANK_PERIOD_CYCLES_NTSC);
        assert_eq!(
            bus.scheduler
                .target(crate::scheduler::EventSlot::VBlank)
                .unwrap(),
            FIRST_VBLANK_CYCLE_NTSC,
        );
    }

    #[test]
    fn gpu_irq_command_latches_and_acknowledges_i_stat() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.write32(crate::gpu::GP0_ADDR, 0x1F00_0000);
        assert_eq!(bus.read32(crate::gpu::GP1_ADDR) & (1 << 24), 0);
        bus.add_cycles(crate::gpu::GP1_STATUS_LATCH_CYCLES as u32);
        assert_ne!(bus.read32(crate::gpu::GP1_ADDR) & (1 << 24), 0);
        assert_ne!(
            bus.read32(IRQ_STAT_ADDR) & (1 << (IrqSource::Gpu as u32)),
            0
        );

        bus.write32(crate::gpu::GP1_ADDR, 0x0200_0000);
        assert_ne!(bus.read32(crate::gpu::GP1_ADDR) & (1 << 24), 0);
        bus.add_cycles(crate::gpu::GP1_STATUS_LATCH_CYCLES as u32);
        assert_eq!(bus.read32(crate::gpu::GP1_ADDR) & (1 << 24), 0);
        assert_eq!(
            bus.read32(IRQ_STAT_ADDR) & (1 << (IrqSource::Gpu as u32)),
            0
        );
    }

    #[test]
    fn timer2_target_sticky_survives_lazy_bus_service() {
        const TIMER2_COUNTER: u32 = 0x1F80_1120;
        const TIMER2_MODE: u32 = 0x1F80_1124;
        const TIMER2_TARGET: u32 = 0x1F80_1128;
        const MODE_RESET_AT_TARGET: u32 = 1 << 3;
        const MODE_REACHED_TARGET: u32 = 1 << 11;

        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.write32(TIMER2_TARGET, 32);
        bus.write32(TIMER2_COUNTER, 0);
        bus.write32(TIMER2_MODE, MODE_RESET_AT_TARGET);
        bus.tick(8192);

        let mode = bus.read32(TIMER2_MODE);
        let counter = bus.read32(TIMER2_COUNTER) & 0xFFFF;

        assert_ne!(mode & MODE_REACHED_TARGET, 0);
        assert!(
            counter < 32,
            "reset-at-target should wrap the counter below target, got {counter}"
        );
    }

    #[test]
    fn video_region_switch_preserves_scanline_phase() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        let phase = 500;
        bus.cycles = FIRST_VBLANK_CYCLE_NTSC + 100 * HSYNC_CYCLES_NTSC + phase;
        bus.scheduler.cancel(crate::scheduler::EventSlot::VBlank);
        bus.scheduler.schedule(
            crate::scheduler::EventSlot::VBlank,
            0,
            FIRST_VBLANK_CYCLE_NTSC + VBLANK_PERIOD_CYCLES_NTSC,
        );

        bus.write32(crate::gpu::GP1_ADDR, 0x0800_0008);

        let current_scanline = (VBLANK_START_SCANLINE_NTSC + 100) % HSYNC_TOTAL_NTSC;
        let remaining = VBLANK_START_SCANLINE_PAL - current_scanline;
        let expected = bus.cycles + remaining * HSYNC_CYCLES_NTSC - phase + 4;
        assert_eq!(
            bus.scheduler
                .target(crate::scheduler::EventSlot::VBlank)
                .unwrap(),
            expected,
        );
    }

    #[test]
    fn pal_to_ntsc_switch_wraps_scanlines_beyond_ntsc_frame() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.set_pal_mode();
        bus.cycles = FIRST_VBLANK_CYCLE_PAL + 280 * HSYNC_CYCLES_PAL + 500;
        bus.scheduler.cancel(crate::scheduler::EventSlot::VBlank);
        bus.scheduler.schedule(
            crate::scheduler::EventSlot::VBlank,
            0,
            FIRST_VBLANK_CYCLE_PAL + VBLANK_PERIOD_CYCLES_PAL,
        );

        bus.write32(crate::gpu::GP1_ADDR, 0x0800_0000);

        assert!(
            bus.scheduler
                .target(crate::scheduler::EventSlot::VBlank)
                .unwrap()
                > bus.cycles
        );
    }

    #[test]
    fn reads_first_bios_word_via_kseg1_reset_vector() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        assert_eq!(bus.read32(memory::bios::RESET_VECTOR), 0xDEAD_BEEF);
    }

    #[test]
    fn reads_first_bios_word_via_kseg0_and_kuseg() {
        // BIOS physical base mapped into KSEG0 (cached) and KUSEG.
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        assert_eq!(bus.read32(0x9FC0_0000), 0xDEAD_BEEF); // KSEG0
        assert_eq!(bus.read32(0x1FC0_0000), 0xDEAD_BEEF); // KUSEG physical alias
    }

    #[test]
    fn ram_starts_zeroed() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        assert_eq!(bus.read32(0x0000_0000), 0);
        assert_eq!(bus.read32(0x8000_0000), 0); // KSEG0 RAM
    }

    #[test]
    fn ram_mirrors_wrap_within_8mib() {
        // Hardware mirrors the 2 MiB RAM four times up to 0x0080_0000.
        // A write to offset 0 should be visible at +2 MiB, +4 MiB, +6 MiB.
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.ram[0..4].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        assert_eq!(bus.read32(0x0000_0000), 0x1122_3344);
        assert_eq!(bus.read32(0x0020_0000), 0x1122_3344);
        assert_eq!(bus.read32(0x0040_0000), 0x1122_3344);
        assert_eq!(bus.read32(0x0060_0000), 0x1122_3344);
    }

    #[test]
    fn segment_aliases_resolve_to_same_ram_word() {
        // One commercial title stashed a flag in a pointer's segment bits: the
        // C4-plant struct lived once in RAM, and the pointer was OR-ed with
        // 0x8000_0000 (KSEG0) or 0xA000_0000 (KSEG1) to encode wall vs
        // ground. All three segment views must resolve to the same word.
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.write32(0x8012_0000, 0xCAFE_F00D); // write through KSEG0
        assert_eq!(bus.read32(0x0012_0000), 0xCAFE_F00D); // KUSEG
        assert_eq!(bus.read32(0x8012_0000), 0xCAFE_F00D); // KSEG0
        assert_eq!(bus.read32(0xA012_0000), 0xCAFE_F00D); // KSEG1
    }

    #[test]
    fn scratchpad_is_unreachable_through_kseg1() {
        // Scratchpad is the D-cache used as fast RAM: cached segments only.
        // A KSEG1 (uncached) view bypasses the cache, so it must not reach
        // the scratchpad -- reads see open bus, writes are dropped.
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.write32(0x1F80_0010, 0x1234_5678); // KUSEG scratchpad
        assert_eq!(bus.read32(0x1F80_0010), 0x1234_5678); // KUSEG sees it
        assert_eq!(bus.read32(0x9F80_0010), 0x1234_5678); // KSEG0 sees it
        assert_eq!(bus.read32(0xBF80_0010), 0xFFFF_FFFF); // KSEG1: unmapped
        bus.write32(0xBF80_0010, 0xDEAD_BEEF); // KSEG1 write dropped
        assert_eq!(bus.read32(0x1F80_0010), 0x1234_5678); // scratchpad intact
    }

    #[test]
    fn expansion_open_bus_reads_cover_every_access_width() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        for base in [memory::expansion2::BASE, memory::expansion3::BASE] {
            assert_eq!(bus.read8(base), 0xFF);
            assert_eq!(bus.read16(base), 0xFFFF);
            assert_eq!(bus.read32(base), 0xFFFF_FFFF);
        }
    }

    #[test]
    fn cpu_narrow_dma_stores_drive_the_complete_source_register() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cpu_write8(Dma::BASE, 0x1234_5678);
        assert_eq!(bus.read32(Dma::BASE), 0x0034_5678);

        bus.cpu_write16(Dma::BASE + Dma::DPCR_OFFSET, 0x1234_5678);
        assert_eq!(bus.read32(Dma::BASE + Dma::DPCR_OFFSET), 0x1234_5678);
    }

    #[test]
    fn native_width_bridges_shape_narrow_mmio_accesses() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.cpu_write8(Sio0::BASE + 0x8, 0x1234_5678);
        assert_eq!(bus.read32(Sio0::BASE + 0x8), 0x38);
        assert_eq!(bus.read8(Sio0::BASE + 0x8), 0x38);

        bus.cpu_write8(Sio1::BASE + 0x8, 0x1234_5678);
        assert_eq!(bus.read32(Sio1::BASE + 0x8), 0x78);

        bus.cpu_write8(crate::spu::SPUCNT, 0x1234_5678);
        assert_eq!(bus.read16(crate::spu::SPUCNT), 0x5678);

        assert_eq!(bus.read16(crate::cdrom::BASE), 0x1818);
        assert_eq!(bus.read32(crate::cdrom::BASE), 0x1818_1818);
        bus.cpu_write16(crate::cdrom::BASE, 0x1234_5678);
        assert_eq!(bus.read16(crate::cdrom::BASE), 0x1A1A);
        assert_eq!(bus.read32(crate::mdec::MDEC_CTRL_STAT), 0x8004_0000);
        assert_eq!(bus.read16(crate::mdec::MDEC_CTRL_STAT), 0);
        assert_eq!(bus.read8(crate::mdec::MDEC_CTRL_STAT), 0);
    }

    #[test]
    fn narrow_on_die_reads_retain_undriven_data_bus_lanes() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.cpu_write8(IRQ_MASK_ADDR, 0x1234_5678);
        // Instruction-side traffic must not replace the data-side hold value.
        let _ = bus.read_instruction32(0);
        assert_eq!(bus.read32(IRQ_MASK_ADDR), 0x1234_0678);

        bus.cpu_write16(Timers::BASE + 8, 0x1234_5678);
        assert_eq!(bus.read32(Timers::BASE + 8), 0x1234_5678);
    }

    #[test]
    fn vblank_scheduler_fires_at_first_threshold() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        // Just below the first VBlank -- no fire yet.
        bus.tick(FIRST_VBLANK_CYCLE as u32 - 1);
        bus.run_vblank_scheduler();
        assert_eq!(bus.irq.stat() & 1, 0);

        // Cross the threshold -- one VBlank fires.
        bus.tick(2);
        bus.run_vblank_scheduler();
        assert_eq!(bus.irq.stat() & 1, 1);
        assert_eq!(
            bus.next_vblank_cycle(),
            FIRST_VBLANK_CYCLE + VBLANK_PERIOD_CYCLES
        );
    }

    #[test]
    fn vblank_scheduler_fires_on_exact_threshold() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.tick(FIRST_VBLANK_CYCLE as u32);
        assert_eq!(bus.irq.stat() & 1, 1);
        assert_eq!(
            bus.next_vblank_cycle(),
            FIRST_VBLANK_CYCLE + VBLANK_PERIOD_CYCLES
        );
    }

    #[test]
    fn vblank_scheduler_catches_up_after_long_tick() {
        // Tick far past the first VBlank in one go -- the scheduler
        // must fire every VBlank that would have elapsed, not just one.
        // Ack each time so we can count.
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.tick((FIRST_VBLANK_CYCLE + 3 * VBLANK_PERIOD_CYCLES + 1) as u32);
        bus.run_vblank_scheduler();
        // irq.stat bit 0 is either 0 or 1 -- can't count from there.
        // Instead, check next_vblank_cycle advanced by 4 periods.
        let expected = FIRST_VBLANK_CYCLE + 4 * VBLANK_PERIOD_CYCLES;
        assert_eq!(bus.next_vblank_cycle(), expected);
        // VBlank bit should be set (at least one fire happened).
        assert_eq!(bus.irq.stat() & 1, 1);
    }

    #[test]
    fn vblank_source_index_is_0() {
        // Sanity: IrqSource::VBlank is bit 0, matching Redux's setIrq(0x01).
        assert_eq!(IrqSource::VBlank as u32, 0);
    }

    #[test]
    fn sio_irq_waits_for_branch_boundary_drain() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.sio0
            .attach_port1(crate::pad::PortDevice::empty().with_pad(crate::pad::DigitalPad::new()));
        bus.write16(Sio0::BASE + 0x0A, 0x1002); // JOYN_OUTPUT | ACK_IRQ_ENABLE
        bus.write16(Sio0::BASE + 0x0E, 0x0001); // ACK after 8 cycles
        bus.write8(Sio0::BASE, 0x01);

        assert_eq!(bus.scheduler.target(EventSlot::Sio), Some(8));
        bus.tick(9);
        assert_eq!(
            bus.irq.stat() & (1 << (IrqSource::Controller as u32)),
            0,
            "per-instruction BIAS ticks must not raise SIO IRQ early"
        );

        bus.drain_scheduler_events_post_op();
        assert_ne!(
            bus.irq.stat() & (1 << (IrqSource::Controller as u32)),
            0,
            "Redux raises the SIO IRQ from the branch-test interrupt queue"
        );
    }

    #[test]
    fn cdrom_dma_requires_ready_sector() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cdrom.debug_seed_data_fifo(&[], false, false);
        bus.dma.dpcr = 1 << (3 * 4 + 3);
        bus.dma.channels[3].base = 0;
        bus.dma.channels[3].block_control = 1;
        bus.dma.channels[3].channel_control = 0x1100_0000;

        bus.run_dma_channel(3);
        assert_eq!(read_ram_u32(&bus.ram[..], 0), 0);
        assert_eq!(bus.dma.channels[3].channel_control & (1 << 24), 0);
        assert_eq!(bus.scheduler.target(EventSlot::CdrDma), None);
    }

    #[test]
    fn cdrom_dma_drains_ready_sector_without_request_latch() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cdrom.debug_seed_data_fifo(&[1, 2, 3, 4], true, false);
        bus.dma.dpcr = 1 << (3 * 4 + 3);
        bus.dma.channels[3].base = 0;
        bus.dma.channels[3].block_control = 1;
        bus.dma.channels[3].channel_control = 0x1100_0000;

        bus.run_dma_channel(3);

        assert_eq!(read_ram_u32(&bus.ram[..], 0), 0x0403_0201);
        assert_ne!(bus.dma.channels[3].channel_control & (1 << 24), 0);
        assert_eq!(bus.scheduler.target(EventSlot::CdrDma), Some(1));

        bus.tick(1);
        assert_ne!(bus.dma.channels[3].channel_control & (1 << 24), 0);
        bus.drain_scheduler_events_post_op();
        assert_eq!(bus.dma.channels[3].channel_control & (1 << 24), 0);
    }

    #[test]
    fn dma_does_not_lose_byte_writes_to_dicr() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();

        bus.write8(0x1F80_10F6, 0x80);
        assert_eq!(bus.read8(0x1F80_10F6) & 0x08, 0);
        assert!(!bus.dma.notify_channel_done(3));

        bus.write8(0x1F80_10F6, 0x88);
        assert_ne!(bus.read8(0x1F80_10F6) & 0x08, 0);
        assert!(bus.dma.notify_channel_done(3));
    }

    #[test]
    fn cdrom_dma_zero_bcr_falls_back_to_buffered_sector_size() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cdrom
            .debug_seed_data_fifo(&[1, 2, 3, 4, 5, 6, 7, 8], true, true);
        bus.dma.dpcr = 1 << (3 * 4 + 3);
        bus.dma.channels[3].base = 0;
        bus.dma.channels[3].block_control = 0;
        bus.dma.channels[3].channel_control = 0x1100_0000;

        assert_eq!(bus.run_dma_cdrom(), Some(2));
        assert_eq!(read_ram_u32(&bus.ram[..], 0), 0x0403_0201);
        assert_eq!(read_ram_u32(&bus.ram[..], 4), 0x0807_0605);
        assert_eq!(bus.cdrom.data_fifo_len(), 0);
        assert!(!bus.cdrom.data_transfer_armed());
    }

    #[test]
    fn cdrom_burst_dma_uses_redux_quarter_rate_completion_delay() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cycles = 100;
        bus.cdrom
            .debug_seed_data_fifo(&[1, 2, 3, 4, 5, 6, 7, 8], true, true);
        bus.dma.dpcr = 1 << (3 * 4 + 3);
        bus.dma.channels[3].base = 0;
        bus.dma.channels[3].block_control = 2;
        bus.dma.channels[3].channel_control = 0x1140_0100;

        bus.run_dma_channel(3);

        assert_eq!(bus.scheduler.target(EventSlot::CdrDma), Some(101));
    }

    #[test]
    fn gpu_block_dma_from_device_reads_gpuread_instead_of_pushing_gp0() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.dma.dpcr = 1 << (2 * 4 + 3);
        bus.gpu.vram.set_pixel(4, 5, 0xCAFE);
        bus.gpu.vram.set_pixel(5, 5, 0xBEEF);

        bus.gpu.gp0_push(0xC0_00_00_00);
        bus.gpu.gp0_push(0x0005_0004);
        bus.gpu.gp0_push(0x0001_0002);
        let hist_before = bus.gpu.gp0_opcode_histogram();

        // If the DMA path ignores direction, this word is pushed to GP0
        // as a fill command. Correct direction reads GPUREAD into RAM.
        write_ram_u32(&mut bus.ram[..], 0x300, 0x0200_FF00);
        bus.dma.channels[2].base = 0x300;
        bus.dma.channels[2].block_control = 1;
        bus.dma.channels[2].channel_control = 0x0100_0200;

        assert_eq!(bus.run_dma_gpu(), Some(3));
        assert_eq!(read_ram_u32(&bus.ram[..], 0x300), 0xBEEF_CAFE);
        assert_eq!(bus.gpu.gp0_opcode_histogram(), hist_before);
    }

    #[test]
    fn gpu_upload_block_dma_uses_silicon_calibrated_request_pacing() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.dma.dpcr = 1 << (2 * 4 + 3);
        bus.gpu.gp0_push(0xA000_0000);
        bus.gpu.gp0_push(0);
        bus.gpu.gp0_push((64 << 16) | 64); // 4096 pixels / 2 = 2048 words
        bus.dma.channels[2].base = 0x400;
        bus.dma.channels[2].block_control = (128 << 16) | 16;
        bus.dma.channels[2].channel_control = 0x0100_0201;

        // 2048 words + 10 cycles * 128 blocks + 250-cycle pipeline.
        assert_eq!(bus.run_dma_gpu(), Some(3578));
        assert!(!bus.gpu.vram_upload_active());
    }

    #[test]
    fn gpu_download_dma_uses_silicon_calibrated_readback_pacing() {
        assert_eq!(gpu_download_cycles(38_400), 84_300);
    }

    #[test]
    fn architectural_gp0_store_stalls_behind_gpu_execution_backlog() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        let before = bus.cycles();
        bus.gpu.charge_busy(1_234);

        bus.cpu_write32(crate::gpu::GP0_ADDR, 0); // GP0 NOP

        assert_eq!(bus.cycles(), before + 1_234);
        assert!(!bus.gpu.is_busy());
    }

    #[test]
    fn gpu_manual_dma_moves_upload_data_without_trigger_bit() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.dma.dpcr = 1 << (2 * 4 + 3);
        bus.gpu.gp0_push(0xA000_0000);
        bus.gpu.gp0_push((5 << 16) | 4);
        bus.gpu.gp0_push((1 << 16) | 2);
        write_ram_u32(&mut bus.ram[..], 0x400, 0xBEEF_CAFE);
        bus.dma.channels[2].base = 0x400;
        bus.dma.channels[2].block_control = 1;
        bus.dma.channels[2].channel_control = 0x0100_0001;

        assert_eq!(bus.run_dma_gpu(), Some(124));
        assert_eq!(bus.gpu.vram.get_pixel(4, 5), 0xCAFE);
        assert_eq!(bus.gpu.vram.get_pixel(5, 5), 0xBEEF);
    }

    #[test]
    fn gpu_manual_chopping_cycles_include_dma_and_cpu_windows() {
        let chcr = (1 << 8) | (3 << 16) | (4 << 20); // DMA 8 words, CPU 16 clocks
        assert_eq!(gpu_upload_manual_cycles(2048, chcr), 7803);
    }

    #[test]
    fn spu_dma_transfers_two_halfwords_per_word_at_late_pal_cadence() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cycles = crate::spu::SAMPLE_CYCLES;
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        bus.spu.write16(crate::spu::SPUCNT, 2 << 4);
        bus.dma.dpcr = 1 << (4 * 4 + 3);

        // 64 distinct source halfwords -- a 32-word DMA must move ALL of
        // them (each 32-bit word is two SPU halfwords). The earlier bug
        // moved only 32, leaving the upper half of every sample bank zero.
        for i in 0..64u16 {
            write_ram_u16(&mut bus.ram[..], 0x100 + u32::from(i) * 2, 0x2000 + i);
        }

        // BCR = block_size 8 * block_count 4 = 32 words = 64 halfwords.
        bus.dma.channels[4].base = 0x100;
        bus.dma.channels[4].block_control = (4 << 16) | 8;
        bus.dma.channels[4].channel_control = 0x0100_0201;
        bus.run_dma_channel(4);

        // 64 halfwords at 32 clocks plus a 34-clock DRAM burst = 2082.
        assert_eq!(bus.scheduler.target(EventSlot::SpuDma), Some(2850));

        // All 64 halfwords landed in SPU RAM (the bug stopped at 32).
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        let mut copied = [0u16; 64];
        bus.spu.dma_read(&mut copied);
        assert_eq!(copied[0], 0x2000);
        assert_eq!(copied[31], 0x201F);
        assert_eq!(copied[63], 0x203F);
    }

    #[test]
    fn early_spu_dma_write_accepts_only_the_first_32_bit_word() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.apply_scph_9902_profile();
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        bus.spu.write16(crate::spu::SPUCNT, 2 << 4);
        bus.dma.dpcr = 1 << (4 * 4 + 3);
        for (i, value) in [0x1111, 0x2222, 0x3333, 0x4444].into_iter().enumerate() {
            write_ram_u16(&mut bus.ram[..], 0x100 + i as u32 * 2, value);
        }
        bus.dma.channels[4].base = 0x100;
        bus.dma.channels[4].block_control = (1 << 16) | 2;
        bus.dma.channels[4].channel_control = 0x0100_0201;

        assert!(bus.run_dma_spu().is_some());

        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        let mut copied = [0u16; 4];
        bus.spu.dma_read(&mut copied);
        assert_eq!(copied, [0x1111, 0x2222, 0, 0]);
    }

    #[test]
    fn default_profile_accepts_full_early_spu_dma_write() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        bus.spu.write16(crate::spu::SPUCNT, 2 << 4);
        bus.dma.dpcr = 1 << (4 * 4 + 3);
        for (i, value) in [0x1111, 0x2222, 0x3333, 0x4444].into_iter().enumerate() {
            write_ram_u16(&mut bus.ram[..], 0x100 + i as u32 * 2, value);
        }
        bus.dma.channels[4].base = 0x100;
        bus.dma.channels[4].block_control = (1 << 16) | 2;
        bus.dma.channels[4].channel_control = 0x0100_0201;

        assert!(bus.run_dma_spu().is_some());

        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        let mut copied = [0u16; 4];
        bus.spu.dma_read(&mut copied);
        assert_eq!(copied, [0x1111, 0x2222, 0x3333, 0x4444]);
    }

    #[test]
    fn spu_dma_irq_addr_match_raises_irq9_immediately() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cycles = crate::spu::SAMPLE_CYCLES;
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        bus.spu.write16(crate::spu::IRQ_ADDR, 0);
        bus.spu.write16(crate::spu::SPUCNT, (2 << 4) | (1 << 6));
        bus.dma.dpcr = 1 << (4 * 4 + 3);
        write_ram_u16(&mut bus.ram[..], 0x100, 0xCAFE);

        bus.dma.channels[4].base = 0x100;
        bus.dma.channels[4].block_control = (1 << 16) | 1;
        bus.dma.channels[4].channel_control = 0x0100_0201;
        bus.run_dma_channel(4);

        assert_ne!(bus.irq.stat() & (1 << (IrqSource::Spu as u32)), 0);
    }

    #[test]
    fn decoded_buffer_spu_irq_reaches_i_stat() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.spu.write16(crate::spu::IRQ_ADDR, 0x0080); // 0x400 bytes
        bus.spu.write16(crate::spu::SPUCNT, 1 << 6);

        bus.run_spu_samples(1);

        assert_ne!(bus.irq.stat() & (1 << (IrqSource::Spu as u32)), 0);
    }

    #[test]
    fn spu_read_dma_writes_halfwords_back_to_ram() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        // Non-zero SPU delay bits make SPU->RAM DMA reads stable.
        bus.memory_control.write(
            0x1F80_1014,
            crate::bus::memory_timing::AccessWidth::Word,
            0x2209_31E1,
        );
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);
        bus.spu.write16(crate::spu::SPUCNT, 3 << 4);
        bus.dma.dpcr = 1 << (4 * 4 + 3);
        let payload = [0x1111u16, 0x2222, 0x3333, 0x4444];
        bus.spu.dma_write(&payload);
        bus.spu.write16(crate::spu::TRANSFER_ADDR, 0);

        bus.dma.channels[4].base = 0x300;
        bus.dma.channels[4].block_control = (1 << 16) | 4;
        bus.dma.channels[4].channel_control = 0x0100_0200;

        assert_eq!(bus.run_dma_spu(), Some(261));
        assert_eq!(read_ram_u16(&bus.ram[..], 0x300), 0x1111);
        assert_eq!(read_ram_u16(&bus.ram[..], 0x302), 0x2222);
        assert_eq!(read_ram_u16(&bus.ram[..], 0x304), 0x3333);
        assert_eq!(read_ram_u16(&bus.ram[..], 0x306), 0x4444);
    }

    #[test]
    fn otc_completion_clears_trigger_and_busy_but_keeps_hardwired_step() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.dma.channels[6].channel_control = 0x1100_0002;
        assert!(!bus.complete_dma_channel(6));
        assert_eq!(bus.dma.channels[6].channel_control, 0x0000_0002);
    }

    #[test]
    fn otc_halts_cpu_for_dram_hyper_page_transfer() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        bus.cycles = 100;
        bus.dma.dpcr = 1 << (6 * 4 + 3);
        bus.dma.channels[6].base = 0x400;
        bus.dma.channels[6].block_control = 16;
        bus.dma.channels[6].channel_control = 0x1100_0002;

        bus.run_dma_channel(6);

        // 16 data cycles + one DRAM row-address setup cycle.
        assert_eq!(bus.cycles, 117);
        assert_eq!(bus.scheduler.target(EventSlot::GpuOtcDma), Some(119));
        assert_eq!(bus.dma.channels[6].channel_control, 0x1100_0002);

        // Two cycles after the CPU regains the bus reaches the completion
        // edge. A real MMIO read occupies this boundary, returning the old
        // value before the following read observes the clear state.
        bus.tick(3);
        assert_eq!(bus.dma.channels[6].channel_control, 0x0000_0002);
    }

    #[test]
    fn mdec_decode_dma0_completes_with_final_dma1() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        seed_one_macroblock_decode(&mut bus);
        enable_mdec_dma(&mut bus);

        bus.dma.channels[0].base = 0x100;
        bus.dma.channels[0].block_control = 6;
        bus.dma.channels[0].channel_control = 0x0100_0201;
        bus.run_dma_channel(0);

        assert_eq!(bus.scheduler.target(EventSlot::MdecInDma), None);
        assert_ne!(bus.dma.channels[0].channel_control & (1 << 24), 0);

        bus.dma.channels[1].base = 0x200;
        bus.dma.channels[1].block_control = 192;
        bus.dma.channels[1].channel_control = 0x0100_0200;
        bus.run_dma_channel(1);

        assert_eq!(bus.scheduler.target(EventSlot::MdecOutDma), Some(192 * 8));
        assert_ne!(bus.dma.channels[0].channel_control & (1 << 24), 0);
        assert_ne!(bus.dma.channels[1].channel_control & (1 << 24), 0);

        bus.tick(192 * 8 + 1);
        assert_eq!(bus.dma.channels[0].channel_control & (1 << 24), 0);
        assert_eq!(bus.dma.channels[1].channel_control & (1 << 24), 0);
    }

    #[test]
    fn pending_mdec_dma1_fires_after_dma0_produces_output() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        seed_one_macroblock_decode(&mut bus);
        enable_mdec_dma(&mut bus);

        bus.dma.channels[1].base = 0x200;
        bus.dma.channels[1].block_control = 192;
        bus.dma.channels[1].channel_control = 0x0100_0200;
        bus.run_dma_channel(1);

        assert_eq!(bus.scheduler.target(EventSlot::MdecOutDma), None);
        assert_ne!(bus.dma.channels[1].channel_control & (1 << 24), 0);

        bus.dma.channels[0].base = 0x100;
        bus.dma.channels[0].block_control = 6;
        bus.dma.channels[0].channel_control = 0x0100_0201;
        bus.run_dma_channel(0);

        assert_eq!(bus.scheduler.target(EventSlot::MdecInDma), None);
        assert_eq!(bus.scheduler.target(EventSlot::MdecOutDma), Some(192 * 8));
        assert_ne!(read_ram_u32(&bus.ram[..], 0x200), 0);
    }

    #[test]
    fn mdec_mono_dma1_completes_with_zero_block_count_encoding() {
        let mut bus = Bus::new(synthetic_bios()).unwrap();
        enable_mdec_dma(&mut bus);
        bus.mdec.write32(crate::mdec::MDEC_CTRL_STAT, 0x6000_0000);
        bus.mdec.write32(crate::mdec::MDEC_CMD_DATA, 0x4000_0001);
        bus.mdec.dma_write_in(&[0x0101_0101; 32]);
        bus.mdec.write32(crate::mdec::MDEC_CMD_DATA, 0x2000_0001);
        bus.mdec.dma_write_in(&[0xFE00_0000]);

        bus.dma.channels[1].base = 0x200;
        // The build-158 test binary uses BS=0x20 even though its 4-bit
        // sample is only eight words, leaving the block-count field zero.
        bus.dma.channels[1].block_control = 0x20;
        bus.dma.channels[1].channel_control = 0x0100_0200;
        bus.run_dma_channel(1);

        assert_eq!(bus.scheduler.target(EventSlot::MdecOutDma), Some(32 * 8));
        bus.tick(32 * 8 + 1);
        assert_eq!(bus.dma.channels[1].channel_control & (1 << 24), 0);
        assert_eq!(read_ram_u32(&bus.ram[..], 0x200), 0x8888_8888);
    }

    fn enable_mdec_dma(bus: &mut Bus) {
        bus.dma.dpcr = (1 << 3) | (1 << 7);
    }

    fn seed_one_macroblock_decode(bus: &mut Bus) {
        bus.mdec.write32(crate::mdec::MDEC_CMD_DATA, 0x4000_0001);
        bus.mdec.dma_write_in(&[0x01_01_01_01; 32]);
        bus.mdec.write32(crate::mdec::MDEC_CMD_DATA, 0x3000_0006);
        for i in 0..6 {
            let offset = 0x100 + i * 4;
            bus.ram[offset..offset + 4].copy_from_slice(&0xFE00_0010u32.to_le_bytes());
        }
    }
}
