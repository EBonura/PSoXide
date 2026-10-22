//! Collection loader protocol, shared by the demo disc and Arcade.
//!
//! This crate does not provide a runtime entry point or panic handler.
//! Transport retains the shipped three-attempt, 64-sector and RAM FNV-1a
//! contracts. Product loading animation remains with each collection.
#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

#[cfg(target_arch = "mips")]
pub mod paint;
#[cfg(target_arch = "mips")]
pub mod runtime;

/// Words in a user-data CD sector.
pub const SECTOR_WORDS: usize = 512;

/// Reader operations used by one load attempt.
///
/// Implementations retain exclusive ownership of their CD device.
pub trait Reader {
    /// Prepare the drive for a fresh attempt.
    /// # Safety
    /// No concurrent device or interrupt handler may use this reader's hardware.
    unsafe fn prepare(&mut self) -> bool;
    /// Seek and start a stream at an absolute collection LBA.
    /// # Safety
    /// Same exclusive-device requirement as `prepare`.
    unsafe fn start_read_seek_first(&mut self, lba: u32, spin_limit: u32) -> bool;
    /// [`start_read_seek_first`](Self::start_read_seek_first), calling `idle`
    /// while it waits for the seek to finish (once per spin, so it must be
    /// cheap). The default ignores `idle`; a reader that can run it between its
    /// own polls overrides this. `idle` must not touch the CD controller.
    /// # Safety
    /// Same exclusive-device requirement as `prepare`.
    unsafe fn start_read_seek_first_idle(
        &mut self,
        lba: u32,
        spin_limit: u32,
        _idle: &mut dyn FnMut(),
    ) -> bool {
        unsafe { self.start_read_seek_first(lba, spin_limit) }
    }
    /// Read one complete user-data sector.
    /// # Safety
    /// The output must be writable and not overlap any live loader state.
    unsafe fn read_sector(&mut self, out: &mut [u32; SECTOR_WORDS]) -> bool;
    /// Stop a stream after a chunk or failure. Returns once the Pause is
    /// acknowledged, not when the drive has stopped: the next command of a read
    /// sequence cancels the Pause's second response, so this is right between
    /// chunks.
    /// # Safety
    /// Same exclusive-device requirement as `prepare`.
    unsafe fn stop(&mut self);
    /// Stop the stream and return only once the drive has really stopped, with
    /// no Pause response still to come. The console answers a Pause twice and
    /// the second response comes 1.2 to 3.9 million system clocks after the
    /// first, so after a plain [`stop`](Self::stop) it lands inside whatever runs
    /// next. The loader calls this after the last read, before the payload gets
    /// the drive. `false` if the stop could not be proved.
    /// # Safety
    /// Same exclusive-device requirement as `prepare`.
    unsafe fn stop_proved(&mut self) -> bool;
}

/// Address translation for payload storage and RAM checksum readback.
/// # Safety
/// Returned storage must remain valid and exclusively writable for the whole
/// rounded payload extent, and readable for the declared byte length. Translation
/// must be stable across calls, and must not alias header, stack or loader code.
pub unsafe trait Memory {
    /// Translate a trusted target address to stable word-aligned sector storage.
    /// # Safety
    /// Caller must have established the complete extent required by `try_load`.
    unsafe fn payload(&mut self, address: u32) -> *mut [u32; SECTOR_WORDS];
}

/// Presentation events, emitted only at completed protocol boundaries.
pub trait Observer {
    /// A numbered stage completed successfully.
    fn stage_ok(&mut self, stage: u32);
    /// A stopped chunk completed; counts are in user-data sectors.
    fn progress(&mut self, complete: u32, total: u32);
    /// All payload sectors were read, before checksum validation.
    fn finish(&mut self);
    /// Time passes during a load: called after every payload sector, once per
    /// spin of a seek wait and every 4 KiB of the checksum pass, so a
    /// presentation can keep a clean screen animated through the whole load.
    /// It runs between CD steps, with the drive owned by the loader: it must
    /// not touch the controller or the payload, and it must be cheap when
    /// there is nothing to do. The default does nothing.
    fn tick(&mut self) {}
}

/// Top of the console's 2 MiB of RAM, as a KSEG0 address.
pub const RAM_END: u32 = 0x8020_0000;

/// Lowest address a PSX-EXE may load at: the kernel owns everything below it.
pub const MIN_LOAD_ADDR: u32 = 0x8001_0000;

/// A half-open span of RAM, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// First address of the span.
    pub start: u32,
    /// First address past the span.
    pub end: u32,
}

impl Span {
    /// The span `[start, end)`.
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Whether the two spans share an address. An empty span shares none.
    pub const fn overlaps(self, other: Span) -> bool {
        self.start < self.end
            && other.start < other.end
            && self.start < other.end
            && other.start < self.end
    }
}

/// Why a payload cannot be loaded where its header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// The load address is under [`MIN_LOAD_ADDR`].
    BelowFloor,
    /// The payload's sectors run past [`RAM_END`] (or wrap the address space).
    PastRam,
    /// The payload's sectors overlap this reserved span.
    Reserved(Span),
}

/// The RAM a payload of `size` bytes loaded at `address` writes: its whole
/// sectors, because the transport stores every sector in full. `None` if the
/// extent does not fit the address space.
pub fn payload_extent(address: u32, size: u32) -> Option<Span> {
    let end = address.checked_add(size.div_ceil(SECTOR_BYTES).checked_mul(SECTOR_BYTES)?)?;
    Some(Span::new(address, end))
}

/// Check that a payload fits in RAM, at or above [`MIN_LOAD_ADDR`], and clear of
/// every `reserved` span (the loader's own code and the stack it runs on).
///
/// This is the rule [`try_load_reserving`] applies on the console and the one
/// a disc builder should apply before it ships the payload, so the two cannot
/// disagree.
pub fn check_payload(address: u32, size: u32, reserved: &[Span]) -> Result<(), Conflict> {
    if address < MIN_LOAD_ADDR {
        return Err(Conflict::BelowFloor);
    }
    let extent = payload_extent(address, size).ok_or(Conflict::PastRam)?;
    if extent.end > RAM_END {
        return Err(Conflict::PastRam);
    }
    match reserved.iter().find(|span| extent.overlaps(**span)) {
        Some(span) => Err(Conflict::Reserved(*span)),
        None => Ok(()),
    }
}

/// Where a payload may load, as `try_load` and `try_load_reserving` differ in it.
enum Bounds<'a> {
    /// The shipped rule: the payload's logical bytes end at or below the loader.
    BelowLoader(u32),
    /// [`check_payload`] against these spans.
    Clear(&'a [Span]),
}

impl Bounds<'_> {
    fn admits(&self, address: u32, size: u32) -> bool {
        match *self {
            Bounds::BelowLoader(loader_base) => {
                address >= MIN_LOAD_ADDR && address.saturating_add(size) <= loader_base
            }
            Bounds::Clear(reserved) => check_payload(address, size, reserved).is_ok(),
        }
    }
}

/// Entry values copied verbatim from the accepted EXE header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadedExe {
    /// Entry program counter.
    pub pc0: u32,
    /// Initial global pointer.
    pub gp0: u32,
    /// Wrapping sum of the header stack base and offset.
    pub sp: u32,
}
const SECTOR_BYTES: u32 = (SECTOR_WORDS * 4) as u32;

/// Spins granted to one SeekL completion: covers the measured worst-case
/// mech travel (~310 ms) with margin, far short of a hang.
const SEEK_POLL: u32 = 4_000_000;

// PSX-EXE header word offsets (see `psoxide.ld`).
const HDR_PC0: usize = 0x10 / 4;
const HDR_GP0: usize = 0x14 / 4;
const HDR_T_ADDR: usize = 0x18 / 4;
const HDR_T_SIZE: usize = 0x1C / 4;
const HDR_SP_BASE: usize = 0x30 / 4;
const HDR_SP_OFFSET: usize = 0x34 / 4;

const EXE_MAGIC: [u32; 2] = [0x582D_5350, 0x4558_4520]; // "PS-X EXE"

/// Run one load attempt and return its stage/detail failure or entry values.
///
/// The header buffer is scrubbed before reading. Payload checksum is FNV-1a over
/// exactly the declared byte length, read back from destination RAM.
///
/// # Safety
/// This preserves the collections' trusted, packer-validated EXE contract; its
/// legacy bounds check is not a validator for arbitrary hostile EXEs. Before
/// execution the producer must guarantee a 4-byte-aligned destination, a writable
/// contiguous `ceil(t_size / 2048) * 2048` extent below the loader and live stack,
/// a representable disc extent, and valid entry/GP/SP values. Header corruption
/// can evade the legacy logical-size check, so untrusted media requires a separate
/// validation layer. `Memory` must honor the same extent and aliasing contract;
/// the reader must own its device exclusively. Observers must not disturb its CD
/// state or payload memory. No hardening is silently added by this extraction.
pub unsafe fn try_load<R: Reader, M: Memory, O: Observer>(
    reader: &mut R,
    exe_lba: u32,
    header: &mut [u32; SECTOR_WORDS],
    payload_fnv: u32,
    memory: &mut M,
    observer: &mut O,
    loader_base: u32,
) -> Result<LoadedExe, (u32, u32)> {
    unsafe {
        load(
            reader,
            exe_lba,
            header,
            payload_fnv,
            memory,
            observer,
            &Bounds::BelowLoader(loader_base),
        )
    }
}

/// [`try_load`] for a loader that does not sit above every payload.
///
/// The payload is refused (stage 5, detail the load address) unless
/// [`check_payload`] accepts it against `reserved`: at or above
/// [`MIN_LOAD_ADDR`], inside RAM, and with none of its whole sectors on a
/// reserved span. A loader reserves its own code and the stack it runs on, and
/// a payload may then lie above the loader as well as below it. Everything else
/// is `try_load`.
///
/// # Safety
/// As [`try_load`], except that the writable extent must lie clear of every
/// span in `reserved`, and `reserved` must cover the loader's code, data and
/// live stack.
pub unsafe fn try_load_reserving<R: Reader, M: Memory, O: Observer>(
    reader: &mut R,
    exe_lba: u32,
    header: &mut [u32; SECTOR_WORDS],
    payload_fnv: u32,
    memory: &mut M,
    observer: &mut O,
    reserved: &[Span],
) -> Result<LoadedExe, (u32, u32)> {
    unsafe {
        load(
            reader,
            exe_lba,
            header,
            payload_fnv,
            memory,
            observer,
            &Bounds::Clear(reserved),
        )
    }
}

/// One load attempt under `bounds`; see [`try_load`].
unsafe fn load<R: Reader, M: Memory, O: Observer>(
    reader: &mut R,
    exe_lba: u32,
    header: &mut [u32; SECTOR_WORDS],
    payload_fnv: u32,
    memory: &mut M,
    observer: &mut O,
    bounds: &Bounds<'_>,
) -> Result<LoadedExe, (u32, u32)> {
    // Scrub the header buffer before every attempt, volatile so the
    // write cannot be elided. The 2026-08-01 11:00 burn's MAGIC panel
    // showed detail = the requested LBA -- a value that exists only in
    // the launcher's stack, meaning read_sector reported success while
    // transferring nothing and the panel printed stale RAM as if the
    // disc had said it. After this scrub that failure mode reads as
    // detail = 00000000: unambiguous on a photo.
    for word in header.iter_mut() {
        unsafe { core::ptr::write_volatile(word, 0) };
    }
    unsafe {
        // Double speed again. Single speed was belt-and-braces from
        // 2026-08-01, when sustained reads returned corrupt bytes -- but
        // that was BEFORE the SDK's SectorReader learned the BIOS bracket
        // (re-send SetMode between seek completion and ReadN), which is
        // the actual fix. At 75 sectors a second Cortex's 1620-sector
        // payload alone took 22 seconds of pure transfer, and the payload
        // checksum still gates the jump: if this is wrong the screen says
        // so and retries rather than booting garbage.
        if !reader.prepare() {
            return Err((1, 0));
        }
        observer.stage_ok(1);
        // BIOS-style bracket: explicit SeekL waited to completion before
        // ReadN, for the header and every payload chunk alike. Seek poll
        // budget covers the measured worst case (~310 ms cross-disc).
        if !reader.start_read_seek_first_idle(exe_lba, SEEK_POLL, &mut || observer.tick()) {
            reader.stop();
            return Err((2, exe_lba));
        }
        observer.stage_ok(2);
        if !reader.read_sector(header) {
            reader.stop();
            return Err((3, exe_lba));
        }
        observer.stage_ok(3);
    }
    if header[0] != EXE_MAGIC[0] || header[1] != EXE_MAGIC[1] {
        unsafe { reader.stop() };
        return Err((4, header[0]));
    }
    observer.stage_ok(4);

    let pc0 = header[HDR_PC0];
    let gp0 = header[HDR_GP0];
    let t_addr = header[HDR_T_ADDR];
    let t_size = header[HDR_T_SIZE];
    let sp = header[HDR_SP_BASE].wrapping_add(header[HDR_SP_OFFSET]);

    // The payload must not reach the loader; the disc builder rejects such a
    // program at build time, so a failure here means the disc and the loader
    // disagree.
    if !bounds.admits(t_addr, t_size) {
        return Err((5, t_addr));
    }
    observer.stage_ok(5);

    // Payload reads go the way the BIOS reads an EXE: short bursts, each
    // with its own absolute SetLoc + ReadN and a Pause after, instead of
    // one continuous 445-sector stream. This console's BIOS loads 1.4 MB
    // EXEs reliably while our single sustained stream returned corrupt
    // bytes with every stage green (2026-08-01 checksum panels), so the
    // stream length was the variable: a mis-sync can now propagate at
    // most one chunk, and every chunk boundary is a hard re-sync.
    // 64, not 16. Every chunk costs a full SeekL to completion, and at 16
    // Cortex needed a hundred of them -- on the order of fifteen seconds of
    // seeking on top of the transfer. Chunking still bounds how far a
    // mis-sync can propagate and still re-syncs hard at every boundary;
    // it just does it four times less often.
    const CHUNK_SECTORS: u32 = 64;
    let sectors = t_size.div_ceil(SECTOR_BYTES);
    let mut dst = unsafe { memory.payload(t_addr) };
    // With no payload sectors this is the last stop before the handoff.
    if sectors == 0 {
        if !unsafe { reader.stop_proved() } {
            return Err((6, 0));
        }
    } else {
        unsafe { reader.stop() };
    }
    let mut sector = 0u32;
    while sector < sectors {
        let chunk_lba = exe_lba + 1 + sector;
        if !unsafe {
            reader.start_read_seek_first_idle(chunk_lba, SEEK_POLL, &mut || observer.tick())
        } {
            unsafe { reader.stop() };
            return Err((2, chunk_lba));
        }
        let n = CHUNK_SECTORS.min(sectors - sector);
        let mut k = 0;
        while k < n {
            if !unsafe { reader.read_sector(&mut *dst) } {
                unsafe { reader.stop() };
                return Err((6, sector + k));
            }
            dst = unsafe { dst.add(1) };
            observer.tick();
            k += 1;
        }
        // The last chunk's stop is the one the payload inherits: prove it, so
        // no Pause response arrives after the jump. Failing here retries the
        // load; `Reader::stop_proved` leaves the cause in the reader's diag word.
        if sector + n == sectors {
            if !unsafe { reader.stop_proved() } {
                return Err((6, sectors));
            }
        } else {
            unsafe { reader.stop() };
        }
        sector += n;
        observer.progress(sector, sectors);
    }
    observer.finish();
    observer.stage_ok(6);

    // Payload integrity, verified in RAM against the checksum mkdisc
    // computed from the disc layout, and GATING: a mismatch retries the
    // whole load rather than jumping into a corrupt payload. The fail log
    // then shows the hash RAM actually got.
    let mut hash: u32 = 0x811C_9DC5;
    let mut at = unsafe { memory.payload(t_addr) } as *const u8;
    let end = unsafe { at.add(t_size as usize) };
    while at < end {
        if at as usize & 0xFFF == 0 {
            observer.tick();
        }
        // Volatile: the buffer was just written by the sector reader.
        hash ^= unsafe { core::ptr::read_volatile(at) } as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        at = unsafe { at.add(1) };
    }
    if hash != payload_fnv {
        return Err((7, hash));
    }
    observer.stage_ok(7);

    Ok(LoadedExe { pc0, gp0, sp })
}
