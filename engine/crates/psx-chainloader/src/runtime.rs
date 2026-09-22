//! Bare-metal teardown, retry diagnostics, cache flush and register handoff.
use crate::paint;
use crate::{Observer, Reader, SECTOR_WORDS};
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
}

impl Reader for SectorReader {
    unsafe fn prepare(&mut self) -> bool {
        unsafe { SectorReader::prepare(self) }
    }
    unsafe fn start_read_seek_first(&mut self, lba: u32, limit: u32) -> bool {
        unsafe { SectorReader::start_read_seek_first(self, lba, limit) }
    }
    unsafe fn read_sector(&mut self, out: &mut [u32; SECTOR_WORDS]) -> bool {
        unsafe { SectorReader::read_sector(self, out) }
    }
    unsafe fn stop(&mut self) {
        unsafe { SectorReader::stop(self) }
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
        let mut reader = SectorReader::new();
        match unsafe {
            crate::try_load(
                &mut reader,
                exe_lba,
                &mut header,
                payload_fnv,
                &mut DirectMemory,
                &mut Events(&mut loading),
                loader_base,
            )
        } {
            Ok(exe) => break exe,
            Err((stage, detail)) => {
                reveal(attempt, stage, &mut loading);
                stage_fail(stage);
                log_fail(attempt, stage, detail, reader.diag());
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

    unsafe { flush_cache() };
    // The JUMP row going green means the cache flush returned and the jump
    // is the very next thing: anything wrong past this point is the game's
    // own first moments, not the load. Its status is a scratchpad
    // write/readback probe: NOSPAD is the fingerprint of a cache-control
    // restore swallowed while the cache was isolated (see flush_cache's
    // ordering note) -- the game inherits whichever machine this saw.
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

/// Put the hardware back roughly where the BIOS leaves it before a game
/// boots: interrupts masked, DMA channels off, GPU reset. The launcher has
/// been driving the GPU and vblank IRQs, and a game's `_start` does not
/// expect to inherit that.
unsafe fn quiesce() {
    unsafe {
        // Silence the SPU before anything else. The launcher hands over
        // with its launch swoosh still sounding, and nothing between here
        // and the game's own spu::init() stops it -- on console that came
        // out as the sound cutting at the end of the warp and then
        // repeating over the game's first moments. Key off all 24 voices
        // and drop the main volume; every program sets its own on boot.
        psx_io::write16(0x1F80_1D8C, 0xFFFF); // KEY_OFF lo
        psx_io::write16(0x1F80_1D8E, 0x00FF); // KEY_OFF hi
        psx_io::write16(0x1F80_1D80, 0); // main volume L
        psx_io::write16(0x1F80_1D82, 0); // main volume R

        // Mask + acknowledge every interrupt source.
        psx_io::irq::set_mask(0);
        psx_io::irq::ack(u32::MAX);

        // Disable every DMA channel but keep the BIOS's priority ladder.
        // The third debug burn proved silicon cares about the difference:
        // with DPCR fully zeroed, re-enabling channel 3 alone (enable bit,
        // priority nibble 0) left the CD DMA transferring nothing, and
        // every chain-loaded header arrived as all zeros with no drive
        // error. Standalone programs inherit 0x07654321 from the BIOS and
        // the identical reader code works there; hand the next program
        // the same baseline. (The emulator does not model priorities, so
        // only a burn could catch this.)
        psx_io::write32(0x1F80_10F0, 0x0765_4321); // DPCR

        // GP1(00h): reset the GPU (display off, FIFO cleared, defaults).
        psx_io::gpu::write_gp1(0);
    }
    // Clear cop0 SR.IEc (bit 0) so no interrupt fires between here and the
    // game's own setup. Shift the bit out and back rather than masking with a
    // register: MIPS-I `andi` zero-extends, and letting the allocator pick a
    // mask register lands on $at, which the assembler reserves.
    //
    // The nop after mfc0 is load-bearing: MFC0 has a one-instruction
    // load-delay hazard on the R3000, so without it the srl reads the STALE
    // $8 (whatever the caller left there) and writes it into SR. That
    // exact failure shipped once: the launcher's cache flush leaves
    // 0xFFFE0000 in $8, which landed in SR with BEV set and sent every
    // interrupt to the ROM vector. See psx-rt's enable_cpu_interrupts for
    // the same hazard note.
    unsafe {
        core::arch::asm!(
            "mfc0 $8, $12",
            "nop",
            "srl  $8, $8, 1",
            "sll  $8, $8, 1",
            "mtc0 $8, $12",
            "nop",
            out("$8") _,
            options(nostack, preserves_flags),
        );
    }
}

// Direct instruction-cache invalidation, open-coded here so the blob does
// not link `psx-rt` (which would bring a second `_start` and panic handler
// with it).
//
// This replaces a BIOS A(44h) FlushCache tail call. The flush is the last
// thing standing between a loaded payload and a running game: the game's
// code lands at 0x80010000, which is exactly where the launcher was
// executing from moments earlier, so those cache lines hold launcher
// instructions. Miss the invalidation and `jr pc0` re-executes the
// launcher instead of the game, leaving the loader's own screen up --
// which is precisely what the console showed with five green stages and
// a full payload bar.
//
// The BIOS call was never proven on silicon in this position. This
// sequence is: it is the routine psx-rt uses, and the launcher boots and
// runs on the console with it. Steps, per the documented recipe: jump to
// this code's KSEG1 alias so fetches bypass the cache being cleared, put
// the cache-control port in tag-test mode with the i-cache enabled,
// isolate the cache (COP0 SR bit 16) so stores hit tags instead of
// memory, clear one tag per 16-byte line across the 4 KiB cache, then
// DROP ISOLATION FIRST (SR = 0, interrupts still off), restore the
// normal 0x1E988 cache-control value, then the caller's SR.
//
// The restore order is load-bearing and matches the BIOS routine (and
// psx-rt after its 2026-07-31 fix): with IsC still set, whether a store
// reaches the CPU-internal cache-control port or is swallowed by the
// isolated cache is undocumented. A swallowed restore leaves cache
// control at 0x804 -- tag-test latched, SCRATCHPAD UNMAPPED -- which is
// exactly the machine every chain-loaded game would then inherit: a
// green checklist, a dead game, and an emulator that forgives it. The
// scratchpad probe on the JUMP row makes the next burn answer this on
// screen either way.
core::arch::global_asm!(
    r#"
    .set noreorder
    .section .text.loader_flush_cache
    .globl __loader_flush_cache
__loader_flush_cache:
    la    $8, .Lloader_flush_body
    lui   $9, 0x2000
    or    $8, $8, $9
    jr    $8
    nop

.Lloader_flush_body:
    mfc0  $10, $12
    nop
    lui   $8, 0xfffe
    ori   $9, $zero, 0x0804
    sw    $9, 0x0130($8)
    lui   $9, 0x0001
    mtc0  $9, $12
    nop
    nop
    or    $9, $zero, $zero
    ori   $11, $zero, 0x1000
.Lloader_flush_line:
    sw    $zero, 0($9)
    addiu $9, $9, 0x0010
    bne   $9, $11, .Lloader_flush_line
    nop
    mtc0  $zero, $12
    nop
    nop
    lui   $9, 0x0001
    ori   $9, $9, 0xe988
    sw    $9, 0x0130($8)
    mtc0  $10, $12
    nop
    nop
    jr    $31
    nop
    .set reorder
    "#
);

extern "C" {
    #[link_name = "__loader_flush_cache"]
    fn flush_cache();
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
        psx_io::gpu::gpustat();
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
