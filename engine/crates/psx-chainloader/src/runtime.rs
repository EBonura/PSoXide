//! Bare-metal teardown, retry diagnostics, cache flush and register handoff.
use crate::paint;
use crate::{Bounds, Observer, Reader, Span, SECTOR_WORDS};
use psx_pack::cd::SectorReader;

/// Collection-specific loading visuals. Shared transport owns the CD device.
pub trait Presentation {
    /// Configure drawing after the shared GPU reset.
    fn setup(&mut self);
    /// Show the initial clean loading screen.
    fn begin(&mut self);
    /// Update a clean progress display after a stopped chunk.
    fn update(&mut self, complete: u32, total: u32);
    /// Complete a clean loading display before verification.
    fn finish(&mut self);
    /// Select the product's diagnostic framebuffer state.
    fn diagnostic_mode(&mut self);
    /// Report dead scratchpad on an otherwise clean loading screen.
    fn no_scratchpad(&mut self);
    /// Time passes during the load (see `Observer::tick`): animate the clean
    /// loading screen here. Not called once the diagnostic checklist is up.
    /// Callbacks must not issue CD commands or modify the payload. The default
    /// does nothing.
    fn tick(&mut self) {}
}

impl Reader for SectorReader {
    unsafe fn prepare(&mut self) -> bool {
        SectorReader::prepare(self)
    }
    unsafe fn start_read_seek_first(&mut self, lba: u32, limit: u32) -> bool {
        SectorReader::start_read_seek_first(self, lba, limit)
    }
    unsafe fn start_read_seek_first_idle(
        &mut self,
        lba: u32,
        limit: u32,
        idle: &mut dyn FnMut(),
    ) -> bool {
        SectorReader::start_read_seek_first_with(self, lba, limit, idle)
    }
    unsafe fn read_sector(&mut self, out: &mut [u32; SECTOR_WORDS]) -> bool {
        SectorReader::read_sector(self, out)
    }
    unsafe fn stop(&mut self) {
        SectorReader::stop(self)
    }
    unsafe fn stop_proved(&mut self) -> bool {
        SectorReader::stop_proved(self).is_ok()
    }
}
struct DirectMemory;
// SAFETY: `run` requires the packer-validated physical extent and live-memory separation.
unsafe impl crate::Memory for DirectMemory {
    unsafe fn payload(&mut self, address: u32) -> *mut [u32; SECTOR_WORDS] {
        address as *mut _
    }
}
struct Events<'a, P>(&'a mut P);
impl<P: Presentation> Observer for Events<'_, P> {
    fn stage_ok(&mut self, stage: u32) {
        stage_ok(stage);
    }
    fn progress(&mut self, sector: u32, sectors: u32) {
        if !paint::checklist_shown() {
            self.0.update(sector, sectors);
        } else {
            let done = (sector * BAR_W as u32 / sectors.max(1)) as i16;
            paint::rect(
                BAR_X,
                row_y(5) + 2,
                done.clamp(2, BAR_W),
                LOAD_BAR_H,
                paint::WHITE,
            );
        }
    }
    fn finish(&mut self) {
        if !paint::checklist_shown() {
            self.0.finish();
        }
    }
    fn tick(&mut self) {
        if !paint::checklist_shown() {
            self.0.tick();
        }
    }
}
/// Load stages, in screen order and in failure-code order: stage `n` is
/// row `n - 1` of the checklist. The detail word logged with a failure
/// is per stage: DRIVE none, SEEK the LBA that would not start, HEADER
/// the header LBA, MAGIC the first header word, BOUNDS the load address,
/// PAYLOAD the failing sector index, VERIFY the FNV the RAM actually
/// hashed to. Stage 8 is the panic handler.
const STAGE_NAMES: [&str; 7] = [
    "DRIVE", "SEEK", "HEADER", "MAGIC", "BOUNDS", "PAYLOAD", "VERIFY",
];
const STAGE_PANIC: u32 = 8;

/// Checklist geometry: stage names down the left at 2x scale, OK / FAIL
/// in a status column, the payload bar between them. The fail log sits
/// below `LOG_Y` at 1x and survives the checklist repaint on retry.
const LIST_X: i16 = 8;
const LIST_Y: i16 = 34;
const ROW_H: i16 = 18;
const BAR_X: i16 = 128;
const BAR_W: i16 = 112;
const STATUS_X: i16 = 248;
const LOAD_BAR_H: i16 = 12;
const LOG_Y: i16 = 190;
const VERDICT_Y: i16 = 222;

/// Read the PSX-EXE at `exe_lba` into its load address and run it.
///
/// `lba_offset` and `cdda_track_base` describe where the target's own disc
/// image landed on this one; they are handed to the target's `_start` in the
/// argument registers, where `psx_io::disc_base` picks them up. See
/// `psx-io/src/disc_base.rs`.
///
/// # Safety
/// All `crate::try_load` trusted-header/device/storage requirements apply.
/// The caller, this code, its data and live stack must lie outside the rounded
/// payload extent. `loader_base` must equal the actual blob entry address.
/// Overwrites the former launcher and never returns.
pub unsafe fn run<P: Presentation>(
    exe_lba: u32,
    lba_offset: u32,
    cdda_track_base: u32,
    payload_fnv: u32,
    loader_base: u32,
    loading: P,
) -> ! {
    unsafe {
        run_bounded(
            exe_lba,
            lba_offset,
            cdda_track_base,
            payload_fnv,
            &Bounds::BelowLoader(loader_base),
            loading,
        )
    }
}

/// [`run`] for a loader that does not sit above every payload: the payload
/// must be clear of `reserved` (see [`crate::check_payload`]), which has to
/// cover the loader's code, data and live stack. A payload may then load above
/// the loader as well as below it.
///
/// # Safety
/// As [`run`], with `reserved` in place of `loader_base`.
pub unsafe fn run_reserving<P: Presentation>(
    exe_lba: u32,
    lba_offset: u32,
    cdda_track_base: u32,
    payload_fnv: u32,
    reserved: &[Span],
    loading: P,
) -> ! {
    unsafe {
        run_bounded(
            exe_lba,
            lba_offset,
            cdda_track_base,
            payload_fnv,
            &Bounds::Clear(reserved),
            loading,
        )
    }
}

/// The body of [`run`] and [`run_reserving`].
unsafe fn run_bounded<P: Presentation>(
    exe_lba: u32,
    lba_offset: u32,
    cdda_track_base: u32,
    payload_fnv: u32,
    bounds: &Bounds<'_>,
    mut loading: P,
) -> ! {
    unsafe { quiesce() };

    // The target overwrites the launcher, so the chain loader owns a small
    // copy of the menu's procedural visual. It has no textures and keeps its
    // two framebuffers outside the payload. Diagnostics remain hidden unless
    // an actual load stage fails.
    loading.setup();
    loading.begin();

    let mut header = [0u32; SECTOR_WORDS];

    // Three attempts. The first silicon burn failed every chain-load, and
    // the second (instrumented) one showed prepare failing and an instant
    // retry reaching the header read: the drive is still winding down from
    // the menu's CD-DA when the first commands arrive, and the old settle
    // delay was an empty loop the optimizer deleted. The 2026-08-01 run
    // then proved the payload reads corrupt silently, so a failed checksum
    // is EXPECTED occasionally and a retry is cheaper than a frozen
    // console. Single-speed reads double the per-sector margin; the
    // checksum gate catches whatever still slips through.
    let mut attempt: u32 = 0;
    let exe = loop {
        if paint::checklist_shown() {
            draw_checklist(attempt);
        }
        // A fresh reader every attempt: SectorReader skips its boot-leftover
        // drain after the first prepare(), but a failed attempt leaves the
        // drive mid-stream -- the 2026-08-01 console panels showed retry 2
        // reading an all-ones header straight from a poisoned FIFO.
        // SAFETY: `run` owns the drive from here to the jump, as its contract
        // says; no other `Cd` token is in use.
        let mut reader = SectorReader::with_cd(unsafe { psx_io::periph::Cd::steal() });
        match unsafe {
            crate::load(
                &mut reader,
                exe_lba,
                &mut header,
                payload_fnv,
                &mut DirectMemory,
                &mut Events(&mut loading),
                bounds,
            )
        } {
            Ok(exe) => break exe,
            Err((stage, detail)) => {
                reveal(attempt, stage, &mut loading);
                stage_fail(stage);
                log_fail(attempt, stage, detail, reader.diagnostics());
                attempt += 1;
                if attempt >= 3 {
                    verdict("HALTED - SEE LOG", paint::YELLOW);
                    halt();
                }
                verdict("RETRYING...", paint::DIM);
                settle_delay();
            }
        }
    };

    // The payload's last read ended in a proved Pause (`Reader::stop_proved`), so
    // the drive is idle with no response on the way. Leave the controller the way
    // a BIOS disc boot does: nothing latched, no data request, the CD source
    // acknowledged and the CPU mask closed. SectorReader::prepare in the payload
    // starts from exactly that state.
    unsafe { leave_drive_clean() };
    // The game lands at 0x80010000, where the launcher was executing, so the
    // instruction cache still holds launcher lines; `jr pc0` without this
    // re-executes the launcher (the console showed five green stages, a full
    // payload bar and the loader's own screen). The flush is the SDK's:
    // it waits for every DMA channel to be idle with interrupts masked before
    // it touches the cache control register, because a flush while a channel
    // moves data makes the next instruction fetch return a word of that
    // traffic (Quake, 2026-10-22; see the SDK's docs/CACHE-CONTROL.md).
    psx_rt::cache::flush_instruction_cache();
    // The JUMP row going green means the cache flush returned and the jump
    // is the very next thing: anything wrong past this point is the game's
    // own first moments, not the load. Its status is a scratchpad
    // write/readback probe: NOSPAD is the fingerprint of a cache-control
    // restore swallowed while the cache was isolated -- the game inherits
    // whichever machine this saw.
    // Invisible on a clean load; on a retried one the row is the last
    // thing the diagnostics record before the game takes the machine.
    let alive = unsafe {
        let probe = 0x1F80_0000 as *mut u32;
        core::ptr::write_volatile(probe, 0xC0DE_5EED);
        core::ptr::read_volatile(probe) == 0xC0DE_5EED
    };
    if paint::checklist_shown() {
        let y = row_y(STAGE_NAMES.len());
        paint::text(LIST_X, y, 2, "JUMP", paint::WHITE);
        if alive {
            paint::text(STATUS_X, y, 2, "OK", paint::GREEN);
        } else {
            paint::text(STATUS_X - 32, y, 2, "NOSPAD", paint::YELLOW);
        }
    } else if !alive {
        // Nothing else on a clean load, but a dead scratchpad is worth
        // saying even when the checklist stayed away.
        loading.no_scratchpad();
    }
    unsafe { enter(exe.pc0, exe.gp0, exe.sp, lba_offset, cdda_track_base) }
}

/// First failure: turn the screen on and reconstruct the checklist up to
/// the failing stage, so the panel a photo captures looks the same as if
/// it had painted live. Later failures find the screen already on.
fn reveal<P: Presentation>(attempt: u32, failed_stage: u32, loading: &mut P) {
    if paint::checklist_shown() {
        return;
    }
    paint::show_checklist();
    loading.diagnostic_mode();
    // Wipe the loading screen and put the diagnostics in its place.
    paint::rect(0, 0, 320, 240, paint::RED_BASE);
    draw_checklist(attempt);
    for stage in 1..failed_stage.min(STAGE_NAMES.len() as u32 + 1) {
        stage_ok(stage);
    }
}

fn row_y(row: usize) -> i16 {
    LIST_Y + row as i16 * ROW_H
}

/// Repaint the checklist for a fresh attempt: title, try counter, every
/// stage pending. Only the region above `LOG_Y` is cleared, so the fail
/// log accumulates across retries.
fn draw_checklist(attempt: u32) {
    paint::rect(0, 0, 320, LOG_Y, paint::RED_BASE);
    paint::text(LIST_X, 8, 2, "CHAIN LOADER", paint::WHITE);
    paint::text_bytes(
        232,
        8,
        2,
        &[b'T', b'R', b'Y', b' ', b'1' + attempt as u8],
        paint::DIM,
    );
    for (row, name) in STAGE_NAMES.iter().enumerate() {
        paint::text(LIST_X, row_y(row), 2, name, paint::DIM);
    }
    paint::text(LIST_X, row_y(STAGE_NAMES.len()), 2, "JUMP", paint::DIM);
}

/// Mark load stage `n` (1-based) as passed.
fn stage_ok(stage: u32) {
    if !paint::checklist_shown() {
        return;
    }
    let row = (stage - 1) as usize;
    paint::text(LIST_X, row_y(row), 2, STAGE_NAMES[row], paint::WHITE);
    paint::text(STATUS_X, row_y(row), 2, "OK", paint::GREEN);
}

fn stage_fail(stage: u32) {
    if !paint::checklist_shown() || stage as usize > STAGE_NAMES.len() {
        return;
    }
    let y = row_y((stage - 1) as usize);
    // Clear only the status cell: on the payload row the bar to its left
    // shows how far the read got before it died.
    paint::rect(STATUS_X, y, 320 - STATUS_X, 16, paint::RED_BASE);
    paint::text(STATUS_X, y, 2, "FAIL", paint::YELLOW);
}

fn stage_name(stage: u32) -> &'static str {
    match stage {
        1..=7 => STAGE_NAMES[(stage - 1) as usize],
        STAGE_PANIC => "PANIC",
        _ => "?",
    }
}

/// One line per attempt: which stage failed, its detail word, and the
/// reader's diag word. Hex at 1x -- small, but the checklist above
/// already tells the story at arm's length; this is the close-up.
fn log_fail(attempt: u32, stage: u32, detail: u32, diag: u32) {
    let y = LOG_Y + attempt.min(2) as i16 * 10;
    let mut x = paint::text_bytes(8, y, 1, &[b'T', b'1' + attempt as u8], paint::WHITE);
    x = paint::text(x + 8, y, 1, stage_name(stage), paint::YELLOW);
    x = paint::text(x + 8, y, 1, "D=", paint::DIM);
    x = paint::hex32(x, y, 1, detail, paint::WHITE);
    x = paint::text(x + 8, y, 1, "R=", paint::DIM);
    paint::hex32(x, y, 1, diag, paint::WHITE);
}

/// The bottom line: what happens next.
fn verdict(s: &str, rgb: u32) {
    paint::rect(0, VERDICT_Y, 320, 18, paint::RED_BASE);
    paint::text(8, VERDICT_Y, 2, s, rgb);
}

/// Put the machine in the SDK's entry state and reset the GPU, before another
/// program runs: the one place that job lives, used by [`run`] and by a
/// program that calls another one from a running game (Hollow Knight's intro).
///
/// What it writes, all of it, in order:
///
/// * [`psx_rt::entry::set_entry_state`]: cop0 SR (CU2 only, interrupts off); the
///   memory controller's access timings; every GTE data and control register
///   zeroed; `I_MASK` zero and every `I_STAT` bit acknowledged; every DMA channel
///   given a bounded chance to finish and then aborted, `DPCR` and `DICR` to their
///   entry values; root counters 0 to 2 to mode 0; SIO0 reset and deselected; the
///   SPU: every voice keyed off, reverb, pitch modulation and noise off, reverb
///   volumes and CD input volume zero, reverb work base parked at the top of
///   sound RAM, main volume at maximum, SPU enabled and unmuted with transfer
///   mode stopped and CD input off; the instruction cache flushed and the cache
///   control register set. See `psx_rt::entry` for the exact values.
/// * `GP1(00h)`: the GPU reset (display off, command FIFO cleared, defaults).
///
/// It does not touch the exception vector, sound RAM, VRAM or main RAM. A caller
/// that will keep running afterwards must save and restore whatever of the above
/// it depends on.
///
/// # Safety
/// The caller owns the whole machine: no DMA transfer, interrupt handler or
/// driver may depend on the state this overwrites while it runs.
pub unsafe fn quiesce() {
    // The SDK's entry state (`psx_rt::entry`): the same one `_start` sets for a program a BIOS
    // booted, so a chain-loaded game starts exactly as a game booted from any BIOS does. It
    // keys off the SPU (the launcher hands over with its launch swoosh still sounding, and
    // nothing between here and the game's own `Spu::new` would stop it), masks and
    // acknowledges every interrupt source, waits for the DMA channels (the launcher's last
    // frame's list walk, for one) and aborts one that will not finish, and writes DPCR, DICR,
    // the root counters, SIO0, SR and the cache control register.
    //
    // DPCR becomes the controller's power-on priority ladder with nothing enabled. The third
    // debug burn proved silicon cares about that over zero: with DPCR fully zeroed, re-enabling
    // channel 3 alone (enable bit, priority nibble 0) left the CD DMA transferring nothing, and
    // every chain-loaded header arrived as all zeros with no drive error. (The old comment here
    // said a standalone program inherits this ladder from the BIOS; a Sony console was measured
    // handing over 0x9099, so no BIOS is relied on for it.) The emulator does not model
    // priorities, so only a burn could catch this.
    psx_rt::entry::set_entry_state();
    // GP1(00h): reset the GPU (display off, FIFO cleared, defaults).
    psx_io::gpu::write_display_control(0);
}

/// Hand the CD controller over idle: every response flag acknowledged, the
/// parameter FIFO reset, the data request dropped, and the CD source acknowledged
/// in `I_STAT` with `I_MASK` closed. Runs after the proved stop, so nothing is
/// still to arrive; a flag found here would be a response nobody collected.
///
/// # Safety
/// `run` owns the drive; no other `Cd` token is in use.
unsafe fn leave_drive_clean() {
    let mut cd = unsafe { psx_io::periph::Cd::steal() };
    cd.acknowledge_all_and_reset_parameters();
    cd.clear_data_request();
    psx_io::irq::set_mask(0);
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
}

/// Seed GP / SP / the disc-base handover and jump to the game's entry point.
/// Nothing after this touches the stack, which is about to belong to the
/// target.
///
/// `$a0..$a2` are the MIPS ABI's first three arguments, which is exactly how
/// `psx-rt`'s `_start` declares them, so the handover needs no assembly on the
/// receiving side.
unsafe fn enter(pc0: u32, gp0: u32, sp: u32, lba_offset: u32, cdda_track_base: u32) -> ! {
    unsafe {
        core::arch::asm!(
            ".set push",
            ".set noat",
            "move $28, {gp}",
            "move $29, {sp}",
            "move $30, $0",
            "jr   {pc}",
            "nop",
            ".set pop",
            gp = in(reg) gp0,
            sp = in(reg) sp,
            pc = in(reg) pc0,
            in("$4") psx_io::disc_base::HANDOFF_MAGIC,
            in("$5") lba_offset,
            in("$6") cdda_track_base,
            options(noreturn),
        );
    }
}

/// Give the drive time to settle before the retry: roughly two seconds of
/// GPUSTAT reads. Volatile MMIO reads, so unlike a plain spin loop the
/// optimizer cannot delete it (the first debug burn proved it will).
fn settle_delay() {
    for _ in 0..1_500_000u32 {
        psx_io::gpu::status();
    }
}

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Paint a panic panel using the collection's existing setup policy, then halt.
pub fn panic(setup: impl FnOnce()) -> ! {
    paint::show_checklist();
    setup();
    paint::rect(0, 0, 320, 240, paint::RED_BASE);
    paint::text(8, 8, 2, "LOADER PANIC", paint::YELLOW);
    paint::show();
    halt()
}
