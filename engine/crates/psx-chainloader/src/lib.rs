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
    /// Read one complete user-data sector.
    /// # Safety
    /// The output must be writable and not overlap any live loader state.
    unsafe fn read_sector(&mut self, out: &mut [u32; SECTOR_WORDS]) -> bool;
    /// Stop a stream after a chunk or failure.
    /// # Safety
    /// Same exclusive-device requirement as `prepare`.
    unsafe fn stop(&mut self);
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
        if !reader.start_read_seek_first(exe_lba, SEEK_POLL) {
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

    // The payload must not reach this blob; `mkdisc` rejects such a game at
    // build time, so a failure here means the disc and the blob disagree.
    if t_addr < 0x8001_0000 || t_addr.saturating_add(t_size) > loader_base {
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
    unsafe { reader.stop() };
    let mut sector = 0u32;
    while sector < sectors {
        let chunk_lba = exe_lba + 1 + sector;
        if !unsafe { reader.start_read_seek_first(chunk_lba, SEEK_POLL) } {
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
            k += 1;
        }
        unsafe { reader.stop() };
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
