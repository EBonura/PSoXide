// SPDX-License-Identifier: GPL-2.0-or-later
//! The linear run.
//!
//! One pass through every chip, in an order chosen so nothing inherits state
//! it did not make: CPU and RAM, then the interrupt, DMA and timer fabric the
//! rest depend on, then the GTE, the GPU (with MDEC and the display modes),
//! the SPU, the CD drive (polled, XA, CD-DA, the streaming transport), the
//! controller port registers, the performance sweeps and, last, everything
//! that is hard on the drive or can hang a console.
//!
//! Every area starts with [`reset_area`], which puts the machine in one
//! defined state (GPU reset and the font again, every SPU voice keyed off with
//! its volumes at zero, the CD drive initialised and paused, DMA channels
//! idle, the interrupt mask and our exception vector restored), and ends with
//! a handoff record that says whether it left the machine clean.
//!
//! Nothing is audible outside the audio and CD-DA steps, and the run ends by
//! silencing everything and proving it (record [`SILENCE_FINAL`]) before the
//! capture is shown.
//!
//! The record ids this file owns:
//! `0x410`-`0x419` area handoffs, `0x41A` final silence, `0x41B` run info,
//! `0x41C` the handoff baseline.

use super::*;
use crate::ui;
use psx_engine::{Ctx, PadState};
use psx_gpu::display::DisplayConfig;
use psx_gpu::prim::FillRect;
use psx_rt::interrupts;

pub(crate) const AREA_COUNT: usize = 10;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Area {
    Boot,
    CpuRam,
    IrqDmaTimers,
    Gte,
    Gpu,
    Spu,
    Cd,
    Sio,
    Perf,
    Drive,
}

impl Area {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Boot => "BOOT SNAPSHOT",
            Self::CpuRam => "CPU AND RAM",
            Self::IrqDmaTimers => "IRQ, DMA, TIMERS",
            Self::Gte => "GTE",
            Self::Gpu => "GPU, MDEC, DISPLAY",
            Self::Spu => "SPU",
            Self::Cd => "CD, XA, CD-DA, STREAM",
            Self::Sio => "SIO",
            Self::Perf => "PERFORMANCE",
            Self::Drive => "DRIVE AND BUS STRESS",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// The area a conformance case runs in. Mostly its group; a few cases need
/// more of the machine than their group says (the GTE-versus-IRQ cases need a
/// GTE already proven, the list-busy cases a drawing GPU, the scratchpad stack
/// the GPU, SPU and drive all at once).
fn test_area(spec: &TestSpec) -> Area {
    match spec.id {
        0x00C8..=0x00CB => Area::Gte,
        0x00CC..=0x00CF => Area::Gpu,
        0x00D0..=0x00D2 => Area::Perf,
        0x00D3..=0x00E9 => Area::Gpu,
        _ => match spec.group {
            "CPU" | "RAM" => Area::CpuRam,
            "IRQ" | "DMA" | "TMR" => Area::IrqDmaTimers,
            "GTE" => Area::Gte,
            "GPU" => Area::Gpu,
            "SPU" => Area::Spu,
            "CD" => Area::Cd,
            "SIO" => Area::Sio,
            _ => Area::CpuRam,
        },
    }
}

// ---------------------------------------------------------------- handoff

/// Handoff flag bits; a set bit is clean.
const H_VECTOR: u16 = 1 << 0;
const H_IRQ_MASK: u16 = 1 << 1;
const H_DMA_IDLE: u16 = 1 << 2;
const H_GPU_IDLE: u16 = 1 << 3;
const H_SPU_QUIET: u16 = 1 << 4;
const H_CD_IDLE: u16 = 1 << 5;
const H_CLEAN: u16 = 0x3F;

/// rec handoff: clean_flags_0x3F_is_clean, interrupt_mask, voices_active_high_dma_busy_mask_low
pub(crate) const HANDOFF: u16 = 0x410;
/// rec handoff_baseline: irq_mask_baseline, dpcr_baseline_low_half, dpcr_baseline_high_half (the values every area's handoff is compared with, taken as the run started; v2.1 recorded 0x3D in all ten areas without them)
pub(crate) const HANDOFF_BASELINE: u16 = 0x41C;
/// rec silence_final: flags_0x7F_is_silent, cd_capture_peak, voices_with_envelope
pub(crate) const SILENCE_FINAL: u16 = 0x41A;
/// rec run_info: skipped_risky, steps, records_taken
pub(crate) const RUN_INFO: u16 = 0x41B;

#[derive(Copy, Clone)]
struct Handoff {
    flags: u16,
    irq_mask: u16,
    busy: u16,
}

/// What a run keeps while it goes.
pub(crate) struct Run {
    pub(crate) font: Option<FontAtlas>,
    pub(crate) results: [TestResult; TEST_COUNT],
    pub(crate) timing: TimingReport,
    pub(crate) next: usize,
    pub(crate) scans: [ScanReport; 3],
    pub(crate) pad: PadState,
    /// Skip the steps that can hang a console (hold L2 when starting).
    pub(crate) skip_risky: bool,
    /// Write memory-card frames back (the same bytes just read) in the pad
    /// engine's lease step, and show the checkpoint pages after each area: only
    /// when L1 and R1 were held at the start, as the card diagnostic asks for
    /// its writes.
    pub(crate) write_cards: bool,
    /// An id for the checkpoint files of this run.
    pub(crate) checkpoint_id: u16,
    handoffs: [Handoff; AREA_COUNT],
    irq_baseline: u32,
    dpcr_baseline: u32,
    vector_ref: [u32; 2],
    silence: [u16; 3],
    /// Precision slots filled so far, by their fixed index ranges.
    pub(crate) precision_next: usize,
}

impl Run {
    pub(crate) const fn new() -> Self {
        Self {
            font: None,
            results: [TestResult::pending(); TEST_COUNT],
            timing: TimingReport {
                summary: ScanReport::pending(),
                records: [TimingRecord::pending(); TIMING_RECORD_COUNT],
                memory_control: [0; MEMORY_CONTROL_REGISTER_COUNT],
                precision: [0; PRECISION_VALUE_COUNT],
            },
            next: 0,
            scans: [ScanReport::pending(); 3],
            pad: PadState::NONE,
            skip_risky: false,
            write_cards: false,
            checkpoint_id: 0,
            handoffs: [Handoff {
                flags: 0,
                irq_mask: 0,
                busy: 0xFFFF,
            }; AREA_COUNT],
            irq_baseline: 0,
            dpcr_baseline: 0,
            vector_ref: [0; 2],
            silence: [0; 3],
            precision_next: 0,
        }
    }

    /// Add finished records (from a case that measured several at once).
    pub(crate) fn push_all(&mut self, records: &[TimingRecord]) {
        for record in records {
            push_timing_record(&mut self.timing.records, &mut self.next, *record);
        }
    }

    pub(crate) fn push(&mut self, record: TimingRecord) {
        self.push_all(&[record]);
    }

    fn font(&self) -> &FontAtlas {
        self.font.as_ref().expect("font uploaded by reset_area")
    }
}

// ---------------------------------------------------------------- hardware

const VECTOR_ADDR: *const u32 = 0x8000_0080 as *const u32;
const DPCR: u32 = 0x1F80_10F0;

fn read_vector() -> [u32; 2] {
    // SAFETY: two aligned reads of kernel RAM.
    unsafe {
        [
            core::ptr::read_volatile(VECTOR_ADDR),
            core::ptr::read_volatile(VECTOR_ADDR.add(1)),
        ]
    }
}

/// Every SPU voice keyed off with its volumes zeroed and its release as fast
/// as it goes, no voice routed to reverb, noise or modulation, main and CD
/// volume zero, CD and external audio input off, reverb off with its wet depth
/// zero. After a couple of frames no voice has an envelope left.
pub(crate) fn silence_spu() {
    use psx_hw::spu as hw;
    // SAFETY: plain SPU register writes.
    unsafe {
        psx_io::write_u16(hw::mask::KEY_OFF_LO, 0xFFFF);
        psx_io::write_u16(hw::mask::KEY_OFF_HI, 0x00FF);
        for v in 0..24u32 {
            let base = hw::voice::base(v);
            psx_io::write_u16(base + hw::voice::VOL_LEFT, 0);
            psx_io::write_u16(base + hw::voice::VOL_RIGHT, 0);
            // Release: linear, shift 0, the fastest there is.
            psx_io::write_u16(base + hw::voice::ADSR_HI, 0);
        }
        psx_io::write_u16(hw::mask::KEY_OFF_LO, 0xFFFF);
        psx_io::write_u16(hw::mask::KEY_OFF_HI, 0x00FF);
        psx_io::write_u16(hw::mask::REVERB_ENABLE_LO, 0);
        psx_io::write_u16(hw::mask::REVERB_ENABLE_HI, 0);
        psx_io::write_u16(hw::mask::NOISE_LO, 0);
        psx_io::write_u16(hw::mask::NOISE_HI, 0);
        psx_io::write_u16(hw::mask::PITCH_MOD_LO, 0);
        psx_io::write_u16(hw::mask::PITCH_MOD_HI, 0);
        for reg in [
            hw::MAIN_VOL_LEFT,
            hw::MAIN_VOL_RIGHT,
            hw::REVERB_VOL_LEFT,
            hw::REVERB_VOL_RIGHT,
            hw::CD_VOL_LEFT,
            hw::CD_VOL_RIGHT,
            0x1F80_1DB4,
            0x1F80_1DB6,
        ] {
            psx_io::write_u16(reg, 0);
        }
        // SPU enabled and unmuted, but CD input (bit 0), external input
        // (bit 1), reverb (bit 7), IRQ (bit 6) and the transfer mode all off.
        psx_io::write_u16(hw::SPUCNT, 0xC000);
    }
}

/// How many voices still have an envelope, and a bitmask of them.
fn voices_with_envelope() -> (u32, u32) {
    let mut mask = 0u32;
    for v in 0..24u32 {
        // SAFETY: plain SPU register read.
        let level = unsafe { psx_io::read_u16(psx_hw::spu::voice::base(v) + 0xC) };
        if level != 0 {
            mask |= 1 << v;
        }
    }
    (mask.count_ones(), mask)
}

/// The drive: stopped reading and playing, no interrupt waiting, CD-DA muted.
pub(crate) fn cd_quiesce() {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]);
    cd_drain_irqs();
    let _ = psx_io::cd::try_set_mode(0, CD_SPINS);
    let _ = psx_io::cd::try_mute(CD_SPINS);
    cd_drain_irqs();
}

fn cd_drain_irqs() {
    for _ in 0..8 {
        let flag = psx_io::cd::irq_flag_value();
        if flag == 0 {
            break;
        }
        psx_io::cd::discard_response();
        psx_io::cd::acknowledge_irq(flag);
    }
}

/// CD init, then pause: the same starting point whatever the last area did.
fn cd_reset() {
    cd_clock_reset();
    let _ = cd_command_until_complete_timed(psx_hw::cd::CMD_INIT, &[]);
    cd_quiesce();
}

fn dma_idle_all() {
    const CHANNELS: [dma::Channel; 7] = [
        dma::Channel::MdecIn,
        dma::Channel::MdecOut,
        dma::Channel::Gpu,
        dma::Channel::Cd,
        dma::Channel::Spu,
        dma::Channel::Expansion,
        dma::Channel::OrderingTableClear,
    ];
    for channel in CHANNELS {
        dma::abort(channel);
    }
}

fn timers_off() {
    for timer in [
        timers::Timer::Timer0,
        timers::Timer::Timer1,
        timers::Timer::Timer2,
    ] {
        timers::set_mode(timer, 0);
        timers::set_counter(timer, 0);
    }
}

/// Everything but the GPU: what each area does when it finishes, and what
/// [`reset_area`] does first.
fn quiesce(run: &Run) {
    irq::set_mask(0);
    irq::acknowledge(0x07FF);
    // Our handler back in the vector (a streaming transport leaves its own
    // wrapper there), which also unmasks VBlank and enables interrupts.
    interrupts::install_vblank_counter();
    irq::set_mask(0);
    dma_idle_all();
    // SAFETY: restoring the DMA control word the engine started with.
    unsafe { psx_io::write_u32(DPCR, run.dpcr_baseline) };
    timers_off();
    silence_spu();
    cd_quiesce();
    irq::acknowledge(0x07FF);
    irq::set_mask(run.irq_baseline);
}

/// Put the machine in the defined starting state for `area`.
pub(crate) fn reset_area(run: &mut Run, ctx: &mut Ctx, area: Area) {
    quiesce(run);
    // SPU: the SDK's own initialisation on top of the silence, so the area
    // starts from what a game would.
    psx_spu::init();
    silence_spu();
    if area != Area::Boot {
        cd_reset();
    }
    // GPU: a full reset (which only `Gpu::new` performs), then the engine's
    // draw buffer back and the font in VRAM again.
    let _ = Gpu::new(
        probe_gpu_dma(),
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    run.font = Some(ui::upload_font());
    let (gpu, fb) = ctx.gpu_and_buffers();
    gpu.draw(&FillRect::new((0, 0), (320, 512), (6, 8, 18)));
    fb.apply_draw_target(gpu);
    ctx.request_timing_realign();
    ui::banner(run.font());
    irq::set_mask(run.irq_baseline);
}

fn busy_mask() -> u16 {
    let mut mask = 0u16;
    for (bit, channel) in [
        dma::Channel::MdecIn,
        dma::Channel::MdecOut,
        dma::Channel::Gpu,
        dma::Channel::Cd,
        dma::Channel::Spu,
        dma::Channel::Expansion,
        dma::Channel::OrderingTableClear,
    ]
    .into_iter()
    .enumerate()
    {
        if dma::is_busy(channel) {
            mask |= 1 << bit;
        }
    }
    mask
}

fn handoff(run: &mut Run, area: Area) {
    quiesce(run);
    let status = gpu_io::status().bits();
    let busy = busy_mask();
    let (active, _) = voices_with_envelope();
    cd_clock_reset();
    let mut flags = 0u16;
    if read_vector() == run.vector_ref {
        flags |= H_VECTOR;
    }
    if irq::mask() == run.irq_baseline {
        flags |= H_IRQ_MASK;
    }
    if busy == 0 {
        flags |= H_DMA_IDLE;
    }
    // Ready for commands (26) and for DMA (28), display enabled (23 clear).
    if status & (1 << 26) != 0 && status & (1 << 28) != 0 && status & (1 << 23) == 0 {
        flags |= H_GPU_IDLE;
    }
    if active == 0 {
        flags |= H_SPU_QUIET;
    }
    if psx_io::cd::irq_flag_value() == 0 {
        flags |= H_CD_IDLE;
    }
    run.handoffs[area.index()] = Handoff {
        flags,
        irq_mask: irq::mask() as u16,
        busy: ((active as u16) << 8) | busy,
    };
    tty::print("hardware-tests: handoff mask=0x");
    tty::print_hex_u32(irq::mask());
    tty::print(" baseline=0x");
    tty::print_hex_u32(run.irq_baseline);
    tty::print("\n");
    tty::print("hardware-tests: handoff area=");
    tty_print_dec_u8(area.index() as u8);
    tty::print(" flags=0x");
    tty::print_hex_u32(flags as u32);
    tty::println(if flags == H_CLEAN { " clean" } else { " DIRTY" });
}

/// The final silence check: flags (all must be set), the CD capture buffer's
/// peak, and how many voices still have an envelope.
fn verify_silence(run: &mut Run) {
    silence_spu();
    cd_quiesce();
    // Two frames for envelopes to release and the capture buffer to refresh.
    let _ = interrupts::try_wait_vblank(4_000_000);
    let _ = interrupts::try_wait_vblank(4_000_000);
    use psx_hw::spu as hw;
    let (active, _) = voices_with_envelope();
    let mut volumes_zero = true;
    for v in 0..24u32 {
        // SAFETY: plain SPU register reads.
        unsafe {
            let base = hw::voice::base(v);
            if psx_io::read_u16(base) != 0 || psx_io::read_u16(base + 2) != 0 {
                volumes_zero = false;
            }
        }
    }
    // SAFETY: plain SPU register reads.
    let (main_l, main_r, cd_l, cd_r, cnt, eon) = unsafe {
        (
            psx_io::read_u16(hw::MAIN_VOL_LEFT),
            psx_io::read_u16(hw::MAIN_VOL_RIGHT),
            psx_io::read_u16(hw::CD_VOL_LEFT),
            psx_io::read_u16(hw::CD_VOL_RIGHT),
            psx_io::read_u16(hw::SPUCNT),
            psx_io::read_u16(hw::mask::REVERB_ENABLE_LO),
        )
    };
    let mut buffer = [0u32; 256];
    spu_dma_read(0, &mut buffer);
    let mut peak = 0u32;
    for word in buffer {
        for half in [word as u16 as i16 as i32, (word >> 16) as u16 as i16 as i32] {
            peak = peak.max(half.unsigned_abs());
        }
    }
    let playing = match psx_io::cd::try_status(CD_SPINS) {
        Some(response) => response
            .bytes()
            .first()
            .is_some_and(|stat| stat & 0x80 != 0),
        None => false,
    };
    let mut flags = 0u16;
    if active == 0 {
        flags |= 1 << 0;
    }
    if volumes_zero {
        flags |= 1 << 1;
    }
    if main_l == 0 && main_r == 0 {
        flags |= 1 << 2;
    }
    if cd_l == 0 && cd_r == 0 {
        flags |= 1 << 3;
    }
    if cnt & 0x0003 == 0 {
        flags |= 1 << 4;
    }
    if cnt & 0x0080 == 0 && eon == 0 {
        flags |= 1 << 5;
    }
    if !playing && peak == 0 {
        flags |= 1 << 6;
    }
    run.silence = [flags, peak.min(0xFFFF) as u16, active as u16];
    tty::print("hardware-tests: silence flags=0x");
    tty::print_hex_u32(flags as u32);
    tty::print(" peak=");
    tty_print_dec_u16(peak.min(0xFFFF) as u16);
    tty::print(" voices=");
    tty_print_dec_u16(active as u16);
    tty::println(if flags == 0x7F {
        " silent"
    } else {
        " NOT SILENT"
    });
}

// ------------------------------------------------------------------- steps

#[derive(Copy, Clone)]
struct Step {
    area: Area,
    name: &'static str,
    /// Can hang a console or is hard on the drive; skipped on request.
    risky: bool,
    run: fn(&mut Run),
}

const fn step(area: Area, name: &'static str, run: fn(&mut Run)) -> Step {
    Step {
        area,
        name,
        risky: false,
        run,
    }
}

const fn risky(area: Area, name: &'static str, run: fn(&mut Run)) -> Step {
    Step {
        area,
        name,
        risky: true,
        run,
    }
}

macro_rules! tests_step {
    ($name:ident, $area:expr) => {
        fn $name(run: &mut Run) {
            run_tests(run, $area);
        }
    };
}

tests_step!(tests_cpu, Area::CpuRam);
tests_step!(tests_irq, Area::IrqDmaTimers);
tests_step!(tests_gte, Area::Gte);
tests_step!(tests_gpu, Area::Gpu);
tests_step!(tests_spu, Area::Spu);
tests_step!(tests_cd, Area::Cd);
tests_step!(tests_sio, Area::Sio);
tests_step!(tests_perf, Area::Perf);

macro_rules! records_step {
    ($name:ident, $push:path) => {
        fn $name(run: &mut Run) {
            $push(&mut run.timing.records, &mut run.next);
        }
    };
}

records_step!(records_cpu, records::push_cpu_records);
records_step!(records_irq, records::push_irq_dma_timer_records);
records_step!(records_gte, records::push_gte_records);
records_step!(records_gpu, records::push_gpu_records);
records_step!(records_spu, records::push_spu_records);
records_step!(records_cd, records::push_cd_records);
records_step!(records_sio, records::push_sio_records);
records_step!(records_gpu_batches, gpu_probes::push);
records_step!(records_perf_safe, perf_probes::push_safe);
records_step!(records_perf_extended, perf_probes::push_extended);
records_step!(records_risky_ab, perf_probes::push_risky);
records_step!(records_gte_latency, perf_probes::push_gte_latency);
records_step!(records_dma_channels, perf_probes::push_dma_channels);
records_step!(records_audit, perf_probes::push_audit);
records_step!(records_cd_dma, dma_matrix::cd);
records_step!(records_mdec_dma, dma_matrix::mdec);
records_step!(records_timer1_rate, timer1_rate::run);
records_step!(records_tick_loss, tick_loss::run);
records_step!(records_sio_setup, sio_timing::setup_sweep);
records_step!(records_sio_pad, sio_timing::pad_timing);
records_step!(records_sio_card, sio_timing::card_timing);
records_step!(records_engine_setup, pad_engine::setup_sweep);
records_step!(records_engine_ack, pad_engine::pacing_ack);
records_step!(records_engine_timed, pad_engine::pacing_timed);

fn run_tests(run: &mut Run, area: Area) {
    let total = TESTS.iter().filter(|spec| test_area(spec) == area).count();
    let mut done = 0;
    for (index, spec) in TESTS.iter().enumerate() {
        if test_area(spec) != area {
            continue;
        }
        ui::detail(run.font(), spec.group, spec.name);
        tty::print("hardware-tests: run ");
        tty_print_dec_u16(index as u16);
        tty::print(" ");
        tty::print(spec.group);
        tty::print(": ");
        tty::println(spec.name);
        let mut result = (spec.run)();
        if index == PAD_POLL_TEST_INDEX {
            result = pad_poll_result(run.pad);
        }
        run.results[index] = result;
        report::print_case_report(index, result);
        done += 1;
        ui::sub(done, total);
    }
}

/// Pad polls and card reads under load: the GPU load paints over the picture,
/// so the progress screen is drawn again afterwards.
fn step_sio_mix(run: &mut Run) {
    sio_timing::pad_and_card(&mut run.timing.records, &mut run.next);
    ui::repaint(run.font());
}

/// Card sector reads (and, with L1 and R1 held at the start, write-backs of the
/// same bytes) through the engine's lease.
fn step_engine_card(run: &mut Run) {
    pad_engine::card_lease(&mut run.timing.records, &mut run.next, run.write_cards);
}

/// The engine with a GPU walk, SPU DMA and a CD read going; the load paints
/// over the picture, so it is drawn again afterwards.
fn step_engine_load(run: &mut Run) {
    pad_engine::under_load(&mut run.timing.records, &mut run.next);
    ui::repaint(run.font());
}

/// The DualShock motor packets, the poll-cost comparison and the operator's
/// yes or no for each motor level.
fn step_rumble(run: &mut Run) {
    let font = run.font.take().expect("font uploaded by reset_area");
    rumble::run(&font, &mut run.timing.records, &mut run.next);
    run.font = Some(font);
}

/// The optional hot-plug window. Says what it wants on the detail line and
/// counts the seconds down.
fn step_sio_hotplug(run: &mut Run) {
    let font = run.font.take().expect("font uploaded by reset_area");
    pad_engine::hotplug(
        |seconds| {
            let mut line = ui::Line::new();
            line.s("UNPLUG+REPLUG ONE (OPTIONAL) ").u(seconds).s("S");
            ui::detail(&font, "PAD", line.as_str());
        },
        &mut run.timing.records,
        &mut run.next,
    );
    run.font = Some(font);
}

fn step_boot_snapshot(run: &mut Run) {
    run.push_all(&boot::records());
    run.timing.memory_control =
        MEMORY_CONTROL_REGISTERS.map(|address| unsafe { psx_io::read_u32(address) });
}

fn step_kernel_timing(run: &mut Run) {
    probe_gpu!(gpu);
    let skip_bios_vblank = run.skip_risky;
    let records = kernel_timing::run_headless(gpu, run.font(), skip_bios_vblank);
    run.push_all(&records);
}

fn step_cpu_sweep(run: &mut Run) {
    run.scans[0] = run_cpu_scan();
}

fn step_gte_sweep(run: &mut Run) {
    run.scans[1] = run_gte_scan();
}

fn step_spu_map(run: &mut Run) {
    run.scans[2] = run_spu_scan();
}

fn precision_at(
    run: &mut Run,
    start: usize,
    fill: fn(&mut [u32; PRECISION_VALUE_COUNT], &mut usize),
) {
    let mut next = start;
    fill(&mut run.timing.precision, &mut next);
    run.precision_next = next;
}

fn precision_timer_step(run: &mut Run) {
    precision_at(run, 61, precision_timer);
}

fn precision_remaining_step(run: &mut Run) {
    precision_at(run, 73, precision_remaining);
}

fn precision_gpu_step(run: &mut Run) {
    precision_at(run, 43, precision_gpu);
}

fn precision_identity_step(run: &mut Run) {
    precision_at(run, 128, precision_identity_and_raster);
}

fn precision_spu_step(run: &mut Run) {
    precision_at(run, 0, precision_spu);
}

const STEPS: &[Step] = &[
    // 0: read-only
    step(Area::Boot, "BOOT STATE", step_boot_snapshot),
    step(Area::Boot, "KERNEL TIMING", step_kernel_timing),
    // 1
    step(Area::CpuRam, "CPU AND RAM CASES", tests_cpu),
    step(Area::CpuRam, "CPU SWEEP", step_cpu_sweep),
    step(Area::CpuRam, "CPU AND BUS TIMING", records_cpu),
    // 2
    step(Area::IrqDmaTimers, "IRQ DMA TIMER CASES", tests_irq),
    step(Area::IrqDmaTimers, "IRQ DMA TIMER TIMING", records_irq),
    step(Area::IrqDmaTimers, "TIMER PRECISION", precision_timer_step),
    step(
        Area::IrqDmaTimers,
        "TIMER 1 HBLANK RATE",
        records_timer1_rate,
    ),
    step(
        Area::IrqDmaTimers,
        "POLLED TIMER TICK LOSS",
        records_tick_loss,
    ),
    // 3
    step(Area::Gte, "GTE CASES", tests_gte),
    step(Area::Gte, "GTE SWEEP", step_gte_sweep),
    step(Area::Gte, "GTE TIMING", records_gte),
    step(Area::Gte, "GTE COMMAND LATENCY", records_gte_latency),
    step(Area::Gte, "GTE PRECISION", precision_remaining_step),
    // 4
    step(Area::Gpu, "GPU CASES", tests_gpu),
    step(Area::Gpu, "GPU TIMING AND MDEC", records_gpu),
    step(Area::Gpu, "GPU BATCHES", records_gpu_batches),
    step(Area::Gpu, "MDEC DECODE", step_mdec),
    step(Area::Gpu, "GPU PRECISION", precision_gpu_step),
    step(Area::Gpu, "RASTER HASHES", precision_identity_step),
    step(Area::Gpu, "DISPLAY WIDTHS", step_widths),
    step(Area::Gpu, "480I INTERLACE", step_interlace),
    // 5
    step(Area::Spu, "SPU INIT STATE", step_spu_init),
    step(Area::Spu, "SPU PRECISION", precision_spu_step),
    step(Area::Spu, "SPU CASES", tests_spu),
    step(Area::Spu, "SPU MAP", step_spu_map),
    step(Area::Spu, "SPU DMA TIMING", records_spu),
    step(Area::Spu, "UI SAMPLE END AND LOOP", step_sb1),
    step(Area::Spu, "SPU RAM AND VOICES", step_sb2),
    step(Area::Spu, "CAPTURE RINGS", step_sb4),
    step(Area::Spu, "BANK HANDOFF", step_handoff),
    // 6
    step(Area::Cd, "CD CASES", tests_cd),
    step(Area::Cd, "CD POLLED TIMING", records_cd),
    step(Area::Cd, "CD DATA VERSUS AUDIO ROUTE", step_cd_route),
    step(Area::Cd, "CD READ MECHANISMS", step_cl2),
    step(Area::Cd, "XA MUSIC LOOP", step_xa),
    step(Area::Cd, "STREAM COST", step_stream_cost),
    step(Area::Cd, "CD-DA HANDOFF", step_stream_handoff),
    // 7
    step(Area::Sio, "SIO CASES", tests_sio),
    step(Area::Sio, "SIO TIMING", records_sio),
    step(Area::Sio, "SIO0 SELECT DELAY", records_sio_setup),
    step(Area::Sio, "SIO0 PAD ACK TIMING", records_sio_pad),
    step(Area::Sio, "SIO0 CARD ACK TIMING", records_sio_card),
    step(Area::Sio, "PAD AND CARD UNDER LOAD", step_sio_mix),
    step(Area::Sio, "ENGINE SETUP SWEEP", records_engine_setup),
    step(Area::Sio, "ENGINE ACK PACING", records_engine_ack),
    step(Area::Sio, "ENGINE TIMED PACING", records_engine_timed),
    step(Area::Sio, "ENGINE AND CARD LEASE", step_engine_card),
    step(Area::Sio, "ENGINE UNDER LOAD", step_engine_load),
    step(Area::Sio, "DUALSHOCK MOTORS", step_rumble),
    step(Area::Sio, "PAD HOT-PLUG WINDOW", step_sio_hotplug),
    // 8
    step(Area::Perf, "STACK AND LEVER CASES", tests_perf),
    step(Area::Perf, "WARM PROBES", records_perf_safe),
    step(
        Area::Perf,
        "EXTENDED PROBES AND SHAPES",
        records_perf_extended,
    ),
    step(Area::Perf, "DMA VERSUS CPU LOADS", records_dma_channels),
    step(Area::Perf, "MDEC DMA VERSUS CPU", records_mdec_dma),
    step(Area::Perf, "AUDIT PROBES", records_audit),
    // 9: last, hardest on the machine
    step(Area::Drive, "CD DMA VERSUS CPU", records_cd_dma),
    step(Area::Drive, "CD MOTOR", step_stream_motor),
    risky(Area::Drive, "REGISTER A/B (CAN HANG)", records_risky_ab),
];

fn step_mdec(run: &mut Run) {
    run.push_all(&mdec_check::run());
}

fn step_widths(run: &mut Run) {
    probe_gpu!(gpu);
    run.push_all(&display_widths::run_widths(gpu));
}

fn step_interlace(run: &mut Run) {
    probe_gpu!(gpu);
    run.push_all(&display_widths::run_interlace(gpu));
}

fn step_sb1(run: &mut Run) {
    run.push_all(&sample_probe::SampleProbe::new().run());
}

fn step_sb2(run: &mut Run) {
    run.push_all(&spu_probe::SpuProbe::new().run());
}

fn step_sb4(run: &mut Run) {
    run.push_all(&ring_probe::run());
}

fn step_handoff(run: &mut Run) {
    use handoff_probe::{HandoffProbe, Variant};
    let mut probe = HandoffProbe::new();
    run.push_all(&probe.run(Variant::Baseline, 0));
    run.push_all(&probe.run(Variant::Safe2, 1));
}

fn step_cd_route(run: &mut Run) {
    run.push_all(&cd_route::run());
}

fn step_cl2(run: &mut Run) {
    run.push_all(&cd_chain_probe::run_matrix());
}

fn step_xa(run: &mut Run) {
    let result = xa_loop::run();
    run.push_all(&xa_loop::records(&result));
}

fn step_stream_cost(run: &mut Run) {
    probe_gpu!(gpu);
    let font = ui::upload_font();
    let mut screen = console_tests::Screen::new(gpu);
    run.push_all(&cdstream_cases::run_cost(&mut screen, &font));
}

fn step_stream_handoff(run: &mut Run) {
    probe_gpu!(gpu);
    let font = ui::upload_font();
    let mut screen = console_tests::Screen::new(gpu);
    run.push_all(&cdstream_cases::run_handoff(&mut screen, &font));
}

fn step_stream_motor(run: &mut Run) {
    probe_gpu!(gpu);
    let font = ui::upload_font();
    let mut screen = console_tests::Screen::new(gpu);
    run.push_all(&cdstream_cases::run_motor(&mut screen, &font));
}

fn step_spu_init(run: &mut Run) {
    run.push(boot::post_init_reverb());
}

/// Run everything, in order. Blocks; the picture is the progress screen.
pub(crate) fn execute(
    run: &mut Run,
    ctx: &mut Ctx,
    pad: PadState,
    skip_risky: bool,
    capture: &mut crate::photo::PhotoCapture,
) {
    run.pad = pad;
    run.skip_risky = skip_risky;
    run.checkpoint_id = (interrupts::vblank_count() as u16) ^ 0x5A5A;
    run.irq_baseline = irq::mask();
    // SAFETY: plain read of the DMA control word.
    run.dpcr_baseline = unsafe { psx_io::read_u32(DPCR) };
    run.results = [TestResult::pending(); TEST_COUNT];
    run.next = 0;
    run.timing.records = [TimingRecord::pending(); TIMING_RECORD_COUNT];
    run.font = Some(ui::upload_font());
    ui::begin_run(run.font(), STEPS.len());
    interrupts::install_vblank_counter();
    run.vector_ref = read_vector();
    irq::set_mask(run.irq_baseline);

    let mut current: Option<Area> = None;
    for (index, step) in STEPS.iter().enumerate() {
        if current != Some(step.area) {
            if let Some(previous) = current {
                handoff(run, previous);
                if run.write_cards {
                    show_checkpoint(run, capture);
                }
            }
            reset_area(run, ctx, step.area);
            current = Some(step.area);
        }
        if step.risky && run.skip_risky {
            tty::print("hardware-tests: skipped risky step ");
            tty::println(step.name);
            continue;
        }
        ui::begin_step(
            run.font(),
            step.area.index(),
            AREA_COUNT,
            step.area.name(),
            index,
            step.name,
        );
        tty::print("hardware-tests: step ");
        tty_print_dec_u16(index as u16);
        tty::print(" ");
        tty::println(step.name);
        // Forget a skip from the step before; look at SELECT for this one.
        bounds::begin_step();
        bounds::poll();
        (step.run)(run);
    }
    if let Some(previous) = current {
        handoff(run, previous);
    }
    ui::begin_step(
        run.font(),
        AREA_COUNT - 1,
        AREA_COUNT,
        "FINISHING",
        STEPS.len(),
        "SILENCE",
    );
    verify_silence(run);
    finish(run);
}

/// Fold the area results into the report: the handoff records, the silence
/// record, the digest the header carries.
fn finish(run: &mut Run) {
    for (area, h) in run.handoffs.iter().enumerate() {
        let record = crate::console_tests::record(
            HANDOFF + area as u16,
            h.flags as u32,
            h.irq_mask as u32,
            h.busy as u32,
        );
        push_timing_record(&mut run.timing.records, &mut run.next, record);
    }
    push_timing_record(
        &mut run.timing.records,
        &mut run.next,
        crate::console_tests::record(
            HANDOFF_BASELINE,
            run.irq_baseline,
            run.dpcr_baseline & 0xFFFF,
            run.dpcr_baseline >> 16,
        ),
    );
    let silence = crate::console_tests::record(
        SILENCE_FINAL,
        run.silence[0] as u32,
        run.silence[1] as u32,
        run.silence[2] as u32,
    );
    push_timing_record(&mut run.timing.records, &mut run.next, silence);
    let info = crate::console_tests::record(
        RUN_INFO,
        run.skip_risky as u32,
        STEPS.len() as u32,
        run.next as u32 + 1,
    );
    push_timing_record(&mut run.timing.records, &mut run.next, info);

    let mut hash = 0x5449_4D33;
    let mut jitter = 0u32;
    for record in run.timing.records {
        hash = mix32(hash, record.id as u32);
        hash = mix32(hash, record.work as u32);
        hash = mix32(hash, record.min as u32);
        hash = mix32(hash, record.max as u32);
        jitter = jitter.wrapping_add(record.max.wrapping_sub(record.min) as u32);
    }
    for (index, value) in run.timing.memory_control.iter().copied().enumerate() {
        hash = mix32(hash, 0x4D43_0000 | index as u32);
        hash = mix32(hash, value);
    }
    for (index, value) in run.timing.precision.iter().copied().enumerate() {
        hash = mix32(hash, 0x5052_0000 | index as u32);
        hash = mix32(hash, value);
    }
    run.timing.summary = ScanReport::info(run.next as u16, hash, jitter);
    report::print_conformance_report(&run.results);
}

/// Frames each checkpoint page stays up.
const CHECKPOINT_PAGE_FRAMES: u32 = 90;

/// The capture so far, encoded quietly and shown as its QR pages (not printed
/// to the TTY), so that a run that stops partway leaves its first areas on film.
/// Only with L1 and R1 held as the run started: a full set after every area
/// is a lot of pages to film.
fn show_checkpoint(run: &mut Run, capture: &mut crate::photo::PhotoCapture) {
    run.timing.summary.runs = (run.checkpoint_id >> 8) as u8;
    capture.encode_quiet(
        &run.timing,
        &run.results,
        run.checkpoint_id as u8,
        run.scans,
    );
    for page in 0..capture.page_count() {
        capture.prepare_page(page);
        crate::photo::draw_capture_page(run.font(), capture, page);
        for _ in 0..CHECKPOINT_PAGE_FRAMES {
            let _ = interrupts::try_wait_vblank(4_000_000);
        }
    }
    ui::repaint(run.font());
}

/// Whether every area's handoff came back clean and the final silence check
/// passed. The headless gate reads this off the TTY.
pub(crate) fn all_clean(run: &Run) -> bool {
    run.handoffs
        .iter()
        .all(|h| h.flags == H_CLEAN && h.busy == 0)
        && run.silence[0] == 0x7F
}

const _: () = assert!(
    TIMING_RECORD_COUNT >= 400,
    "the run takes a few hundred records; see docs/hardware-test-disc.md"
);
