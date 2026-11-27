//! `pack-bench` -- times psx-pack's load paths on a real level change, headless.
//!
//! The disc carries a WORLD.PAK with a game's chunks at their real sizes (the
//! lane that built this used Half-Life's 714-chunk pack); `LEVEL` is the chunk
//! list one level change loads, in the order the game asks for it. Each path
//! loads the whole list and reports the VBlanks it took on the terminal:
//!
//! * `SCAN`: `load_chunk` per chunk, which scans the pack table from disc
//!   for each one;
//! * `LOOKUP` + `RUN`: the entries found through one `Bounce`
//!   (`find_entry_bounced`), then `load_run`, neighbours on the disc sharing
//!   a read; `BATCH` is the two together.
//!
//! Every chunk's FNV is checked against the table, so a path that reads the
//! wrong bytes fails rather than reporting a time. Prints `pack-bench: DONE`
//! or `pack-bench: FAIL <what>`.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_pack::cd::{
    find_entry, find_entry_bounced, load_chunk, load_run, Bounce, RunItem, SectorReader,
    SECTOR_WORDS, WORLD_PACK_DEFAULT_LBA,
};
use psx_pack::{fnv1a32, PackEntry};
use psx_rt::interrupts::{install_vblank_counter, vblank_count};
use psx_rt::tty;

/// Half-Life's c1a1d -> c1a1c level change, in request order (emulator trace
/// of the hl-loads lane, 2026-11-27).
const LEVEL: [u32; 17] = [
    31, 3001, 3077, 3115, 3315, 3002, 3004, 30, 1000, 2128, 2141, 2105, 1101, 2156, 1304, 1340,
    2104,
];
/// The run path holds the whole list at once, each chunk in its own place:
/// the list stores 1,230,006 B. `SCAN` uses the start of it.
const ARENA_WORDS: usize = 312 * 1024;

static mut SCRATCH: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut ARENA: [u32; ARENA_WORDS] = [0; ARENA_WORDS];
static mut BOUNCE: Bounce = Bounce::new();
static mut SUMS: [u32; LEVEL.len()] = [0; LEVEL.len()];

fn report(name: &str, vblanks: u32) {
    tty::print("pack-bench: ");
    tty::print(name);
    tty::print(" vbl 0x");
    tty::print_hex_u32(vblanks);
    tty::println("");
}

fn fail(what: &str) -> ! {
    tty::print("pack-bench: FAIL ");
    tty::println(what);
    loop {
        psx_rt::interrupts::wait_vblank();
    }
}

fn bytes(words: &[u32]) -> &[u8] {
    // SAFETY: [u32] viewed as its own bytes (little-endian target).
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 4) }
}

/// Check the chunk just loaded into `dst` against the FNV `SUMS[i]` holds.
fn check(i: usize, dst: &[u32], len: usize, what: &str) {
    // SAFETY: main runs once; the static is only touched from main.
    let want = unsafe { (*addr_of_mut!(SUMS))[i] };
    if fnv1a32(&bytes(dst)[..len]) != want {
        fail(what);
    }
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    install_vblank_counter();
    let rd = &mut SectorReader::with_cd(peripherals.cd);
    // SAFETY: main runs once; these statics are only touched here.
    let (scratch, arena, bounce, sums) = unsafe {
        (
            &mut *addr_of_mut!(SCRATCH),
            &mut *addr_of_mut!(ARENA),
            &mut *addr_of_mut!(BOUNCE),
            &mut *addr_of_mut!(SUMS),
        )
    };
    if !rd.prepare() {
        fail("PREPARE");
    }
    let pack = WORLD_PACK_DEFAULT_LBA;

    // The table's checksums, for every later check (not timed).
    for (i, &id) in LEVEL.iter().enumerate() {
        match find_entry(rd, pack, id, scratch) {
            Some(e) => sums[i] = e.checksum,
            None => fail("ENTRY"),
        }
    }

    // SCAN: the table scan per chunk.
    let t = vblank_count();
    for (i, &id) in LEVEL.iter().enumerate() {
        let Some(len) = load_chunk(rd, pack, id, scratch, arena) else {
            fail("SCAN LOAD")
        };
        check(i, arena, len, "SCAN FNV");
    }
    report("SCAN", vblank_count().wrapping_sub(t));

    // LOOKUP + RUN: the entries through one bounce, then the list as runs.
    let start = vblank_count();
    let mut entries = [None::<PackEntry>; LEVEL.len()];
    for (slot, &id) in entries.iter_mut().zip(LEVEL.iter()) {
        *slot = find_entry_bounced(rd, pack, id, bounce);
        if slot.is_none() {
            fail("LOOKUP");
        }
    }
    let looked = vblank_count();
    report("LOOKUP", looked.wrapping_sub(start));
    let mut rest: &mut [u32] = &mut arena[..];
    let mut items: [Option<RunItem<'_>>; LEVEL.len()] = [const { None }; LEVEL.len()];
    for (slot, entry) in items.iter_mut().zip(entries.iter()) {
        let entry = entry.expect("found");
        let words = (entry.byte_size as usize).div_ceil(4);
        if words > rest.len() {
            fail("RUN ARENA");
        }
        let (dst, tail) = core::mem::take(&mut rest).split_at_mut(words);
        rest = tail;
        *slot = Some(RunItem::new(entry, dst));
    }
    let mut items = items.map(|item| item.expect("filled"));
    let t = vblank_count();
    let Some(starts) = load_run(rd, pack, &mut items, scratch) else {
        fail("RUN LOAD")
    };
    let end = vblank_count();
    report("RUN", end.wrapping_sub(t));
    report("BATCH", end.wrapping_sub(start));
    tty::print("pack-bench: RUN starts 0x");
    tty::print_hex_u32(starts);
    tty::println("");
    for item in items.iter() {
        let Some(i) = LEVEL.iter().position(|&id| id == item.entry.chunk_id) else {
            fail("RUN ID")
        };
        check(i, item.dst, item.entry.byte_size as usize, "RUN FNV");
    }

    rd.stop();
    tty::println("pack-bench: DONE");
    loop {
        psx_rt::interrupts::wait_vblank();
    }
}
