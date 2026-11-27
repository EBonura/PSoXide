//! `pack-bench` -- times psx-pack's load paths on a real level change, headless.
//!
//! The disc carries a WORLD.PAK with a game's chunks at their real sizes (the
//! lane that built this used Half-Life's 714-chunk pack); `LEVEL` is the chunk
//! list one level change loads, in the order the game asks for it. Each path
//! loads the whole list and reports the VBlanks it took on the terminal:
//!
//! * `SCAN`: `load_chunk`, which scans the pack table from disc per chunk;
//! * `BOUNCED`: `load_chunk_bounced`, the same through a bounce that
//!   remembers its sector;
//! * `INDEX`: `read_index` once, then `load_indexed` per chunk;
//! * `RUN`: `load_run`, the list sorted by disc position, neighbours sharing a read;
//! * `DECODE`: the in-place HLZC decode of every compressed chunk on the
//!   list, timed on its own, to set against the read time;
//! * `COPY`: CPU clocks of one 2 KB sector copy, what popping whole sectors
//!   straight into the destination saves per sector.
//!
//! Every chunk's FNV is checked against the table, so a path that reads the
//! wrong bytes fails rather than reporting a time. Prints `pack-bench: DONE`
//! or `pack-bench: FAIL <what>`.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_io::timers::{self, Timer};
use psx_pack::cd::{
    find_entry, find_indexed, load_chunk, load_chunk_bounced, load_indexed, load_run, read_index,
    Bounce, IndexEntry, RunItem, SectorReader, SECTOR_WORDS, WORLD_PACK_DEFAULT_LBA,
};
use psx_pack::{decompress_hlzc_in_place, fnv1a32, HLZC_MAGIC};
use psx_rt::interrupts::{install_vblank_counter, vblank_count};
use psx_rt::tty;

/// Half-Life's c1a1d -> c1a1c level change, in request order (emulator trace
/// of the hl-loads lane, 2026-11-27).
const LEVEL: [u32; 17] = [
    31, 3001, 3077, 3115, 3315, 3002, 3004, 30, 1000, 2128, 2141, 2105, 1101, 2156, 1304, 1340,
    2104,
];
/// The biggest raw chunk on the list is 442,276 B; room for it plus the
/// in-place margin.
const DST_WORDS: usize = 120 * 1024;
/// The run path loads the whole list at once, each chunk in its own place: the
/// list stores 1,230,006 B. The other paths use its first `DST_WORDS`.
const ARENA_WORDS: usize = 312 * 1024;
/// Every entry of the pack (714 on the HL disc).
const INDEX_CAP: usize = 768;

static mut SCRATCH: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut ARENA: [u32; ARENA_WORDS] = [0; ARENA_WORDS];
static mut INDEX: [IndexEntry; INDEX_CAP] = [IndexEntry::EMPTY; INDEX_CAP];
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
    let (scratch, arena, index, bounce, sums) = unsafe {
        (
            &mut *addr_of_mut!(SCRATCH),
            &mut *addr_of_mut!(ARENA),
            &mut *addr_of_mut!(INDEX),
            &mut *addr_of_mut!(BOUNCE),
            &mut *addr_of_mut!(SUMS),
        )
    };
    let (dst, _) = arena.split_at_mut(DST_WORDS);
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
        let Some(len) = load_chunk(rd, pack, id, scratch, dst) else {
            fail("SCAN LOAD")
        };
        check(i, dst, len, "SCAN FNV");
    }
    report("SCAN", vblank_count().wrapping_sub(t));

    // BOUNCED: the same through a bounce that remembers its sector.
    let t = vblank_count();
    for (i, &id) in LEVEL.iter().enumerate() {
        let Some(len) = load_chunk_bounced(rd, pack, id, bounce, dst) else {
            fail("BOUNCED LOAD")
        };
        check(i, dst, len, "BOUNCED FNV");
    }
    report("BOUNCED", vblank_count().wrapping_sub(t));

    // INDEX: the table once, then a seek per chunk.
    let t = vblank_count();
    let Some(count) = read_index(rd, pack, scratch, index, |_| true) else {
        fail("INDEX READ")
    };
    report("INDEX-TABLE", vblank_count().wrapping_sub(t));
    let index = &index[..count];
    for (i, &id) in LEVEL.iter().enumerate() {
        let Some(e) = find_indexed(index, id) else {
            fail("INDEX FIND")
        };
        let Some(len) = load_indexed(rd, pack, &e, scratch, dst) else {
            fail("INDEX LOAD")
        };
        check(i, dst, len, "INDEX FNV");
    }
    report("INDEX", vblank_count().wrapping_sub(t));

    // DECODE: in-place decode of the compressed chunks, timed apart from
    // their reads (which are timed too, for the share).
    let mut read_vbl = 0u32;
    let mut decode_vbl = 0u32;
    let mut stored = 0u32;
    let mut raw = 0u32;
    for &id in LEVEL.iter() {
        let Some(e) = find_indexed(index, id) else {
            fail("DECODE FIND")
        };
        let t = vblank_count();
        let Some(len) = load_indexed(rd, pack, &e, scratch, dst) else {
            fail("DECODE LOAD")
        };
        read_vbl += vblank_count().wrapping_sub(t);
        if bytes(dst)[..4] != HLZC_MAGIC {
            continue;
        }
        let t = vblank_count();
        // SAFETY: [u32] viewed as its own bytes for the in-place decoder.
        let all = unsafe {
            core::slice::from_raw_parts_mut(dst.as_mut_ptr().cast::<u8>(), dst.len() * 4)
        };
        let Some(out) = decompress_hlzc_in_place(all, len) else {
            fail("DECODE")
        };
        decode_vbl += vblank_count().wrapping_sub(t);
        stored += len as u32;
        raw += out as u32;
    }
    report("DECODE-READS", read_vbl);
    report("DECODE", decode_vbl);
    tty::print("pack-bench: DECODE stored 0x");
    tty::print_hex_u32(stored);
    tty::print(" raw 0x");
    tty::print_hex_u32(raw);
    tty::println("");

    // COPY: one sector copy in CPU clocks, root counter 2 at the system clock
    // (the runtime's entry rate), best of 16.
    let (from, to) = dst.split_at_mut(SECTOR_WORDS);
    let mut best = u32::MAX;
    for _ in 0..16 {
        let a = timers::counter(Timer::Timer2);
        // SAFETY: two distinct sector-sized windows of `dst`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                from.as_ptr().cast::<u8>(),
                to.as_mut_ptr().cast::<u8>(),
                SECTOR_WORDS * 4,
            );
        }
        core::hint::black_box(&mut *to);
        let b = timers::counter(Timer::Timer2);
        best = best.min(b.wrapping_sub(a) as u32);
    }
    tty::print("pack-bench: COPY clocks 0x");
    tty::print_hex_u32(best);
    tty::println("");

    // RUN: the same list as runs (neighbours on the disc share one read),
    // each chunk into its own part of the arena.
    {
        let mut rest: &mut [u32] = &mut arena[..];
        let mut items: [Option<RunItem<'_>>; LEVEL.len()] = [const { None }; LEVEL.len()];
        for (slot, &id) in items.iter_mut().zip(LEVEL.iter()) {
            let Some(entry) = find_indexed(index, id) else {
                fail("RUN FIND")
            };
            let words = (entry.byte_size as usize).div_ceil(4);
            if words > rest.len() {
                fail("RUN ARENA");
            }
            let (dst, tail) = core::mem::take(&mut rest).split_at_mut(words);
            rest = tail;
            *slot = Some(RunItem { entry, dst });
        }
        let mut items = items.map(|item| item.expect("filled"));
        let t = vblank_count();
        let Some(starts) = load_run(rd, pack, &mut items, scratch) else {
            fail("RUN LOAD")
        };
        report("RUN", vblank_count().wrapping_sub(t));
        tty::print("pack-bench: RUN starts 0x");
        tty::print_hex_u32(starts);
        tty::println("");
        for item in items.iter() {
            let Some(i) = LEVEL.iter().position(|&id| id == item.entry.chunk_id) else {
                fail("RUN ID")
            };
            check(i, item.dst, item.entry.byte_size as usize, "RUN FNV");
        }
    }

    rd.stop();
    tty::println("pack-bench: DONE");
    loop {
        psx_rt::interrupts::wait_vblank();
    }
}
