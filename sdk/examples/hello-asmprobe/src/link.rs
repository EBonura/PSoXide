// SPDX-License-Identifier: GPL-2.0-or-later
//! Tagged-packet-stream link loop: the portable definition against the inline-asm loop the
//! SDK runs on the console.

use crate::{print_dec, print_u32, Rng};
use core::hint::black_box;

pub const OT_N: usize = 2048;
/// The largest table the staged tag format can address: slot 0xffff is the skip sentinel.
const BIG_N: usize = 0xffff;
const OT_END: u32 = 0x00ff_ffff;
const MASK: u32 = 0x00ff_ffff;
const ARENA_WORDS: usize = 1 << 16;

static mut OT_REF: [u32; OT_N + 1] = [0; OT_N + 1];
static mut OT_TST: [u32; OT_N + 1] = [0; OT_N + 1];
static mut BIG_REF: [u32; BIG_N + 1] = [0; BIG_N + 1];
static mut BIG_TST: [u32; BIG_N + 1] = [0; BIG_N + 1];
static mut ARENA_REF: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];
static mut ARENA_TST: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];
static mut ARENA_SRC: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];

/// Clear an ordering table of `slots` entries: slot 0 ends the walk, slot i points at slot i-1.
unsafe fn clear_table(table: *mut u32, slots: usize) {
    let base = table as u32;
    table.write(OT_END);
    for i in 1..slots {
        table
            .add(i)
            .write(base.wrapping_add(4 * (i as u32 - 1)) & MASK);
    }
}

/// The portable definition, line for line the host branch of
/// `OrderingTable::link_tagged_packet_stream_unchecked`.
pub unsafe fn link_ref(entries: *mut u32, first: *mut u32, end: *mut u32) {
    let mut p = first;
    while p < end {
        let tag = p.read();
        let words = (tag >> 24) as usize;
        let slot = (tag & 0xffff) as usize;
        let next = p.add(words + 1);
        if slot != 0xffff {
            let head = entries.add(slot).read() & MASK;
            p.write((tag & 0xff00_0000) | head);
            entries.add(slot).write(p as u32 & MASK);
        }
        p = next;
    }
}

/// A deliberately wrong kernel for the harness self-check: it forgets the
/// sentinel and links skipped packets into slot 0xffff & mask.
unsafe extern "C" fn mutant_link(first: *mut u32, end: *mut u32, entries: *mut u32) {
    let mut p = first;
    while p < end {
        let tag = p.read();
        let words = (tag >> 24) as usize;
        let slot = (tag & 0xffff) as usize & (OT_N - 1);
        let next = p.add(words + 1);
        let head = entries.add(slot).read() & MASK;
        p.write((tag & 0xff00_0000) | head);
        entries.add(slot).write(p as u32 & MASK);
        p = next;
    }
}

/// The SDK's own entry point: the console's link loop.
unsafe extern "C" fn sdk_link(first: *mut u32, end: *mut u32, entries: *mut u32) {
    let ot = &mut *(entries as *mut psx_gpu::ot::OrderingTable<OT_N>);
    let mut frame = ot.resume_frame();
    frame.add_tagged_packet_stream_unchecked(first, end);
}

/// The same through the largest table.
unsafe extern "C" fn sdk_link_big(first: *mut u32, end: *mut u32, entries: *mut u32) {
    let ot = &mut *(entries as *mut psx_gpu::ot::OrderingTable<BIG_N>);
    let mut frame = ot.resume_frame();
    frame.add_tagged_packet_stream_unchecked(first, end);
}

/// The portable definition as a kernel, for timing.
unsafe extern "C" fn rust_link(first: *mut u32, end: *mut u32, entries: *mut u32) {
    link_ref(entries, first, end)
}

/// Fill `arena` with a deterministic packet stream and return its end.
/// `mode` selects the slot distribution; every mode keeps bits 16..23 of a tag
/// clear, as the staged format requires.
unsafe fn make_stream(
    arena: *mut u32,
    rng: &mut Rng,
    packets: usize,
    mode: u32,
    slots: u32,
) -> *mut u32 {
    let mut p = arena;
    let mut asc = 0u32;
    for i in 0..packets {
        let r = rng.next();
        let words = match mode {
            7 => 0,
            8 => (r >> 8) % 3, // zero-length mixed with tiny
            12 => 15,
            _ => match r % 16 {
                0..=4 => 4,
                5..=8 => 5 + (r >> 8) % 2,
                9..=12 => 8 + (r >> 8) % 2,
                13 => 1 + (r >> 8) % 3,
                _ => 10 + (r >> 8) % 6,
            },
        };
        let s = rng.next();
        let slot = match mode {
            0 => s % slots,
            1 => {
                if s % 5 != 0 {
                    slots / 2 + (s >> 8) % 64
                } else {
                    (s >> 8) % slots
                }
            }
            2 => 0,
            3 => slots - 1,
            4 => {
                if s % 20 == 0 {
                    0xffff
                } else {
                    (s >> 4) % slots
                }
            }
            5 => {
                asc = (asc + 1 + (s % 3)) % slots;
                asc
            }
            6 => (slots - 1) - (i as u32 % slots),
            9 => 0xffff,
            10 => {
                if i == 0 || i + 1 == packets {
                    0xffff
                } else {
                    (s >> 4) % slots
                }
            }
            11 => {
                if i % 2 == 0 {
                    0xffff
                } else {
                    (s >> 4) % slots
                }
            }
            12 => 5,
            _ => (s >> 4) % slots,
        };
        p.write((words << 24) | slot);
        for w in 0..words as usize {
            p.add(1 + w).write(rng.next());
        }
        p = p.add(1 + words as usize);
    }
    p
}

unsafe fn run_variant(
    kernel: unsafe extern "C" fn(*mut u32, *mut u32, *mut u32),
    seed: u32,
    packets: usize,
    mode: u32,
) -> u32 {
    let ot_ref = core::ptr::addr_of_mut!(OT_REF) as *mut u32;
    let ot_tst = core::ptr::addr_of_mut!(OT_TST) as *mut u32;
    run_tables(kernel, seed, packets, mode, OT_N, ot_ref, ot_tst)
}

/// Link one deterministic stream with the portable definition into `tab_ref` and with `kernel`
/// into `tab_tst` (each table of `slots` entries, followed by one guard word the kernel must not
/// touch), and count the disagreements in the tables and in the packets' tags.
unsafe fn run_tables(
    kernel: unsafe extern "C" fn(*mut u32, *mut u32, *mut u32),
    seed: u32,
    packets: usize,
    mode: u32,
    slots: usize,
    tab_ref: *mut u32,
    tab_tst: *mut u32,
) -> u32 {
    let src = core::ptr::addr_of_mut!(ARENA_SRC) as *mut u32;
    let arena_ref = core::ptr::addr_of_mut!(ARENA_REF) as *mut u32;
    let arena_tst = core::ptr::addr_of_mut!(ARENA_TST) as *mut u32;
    let mut rng = Rng(seed | 1);
    let end_src = make_stream(src, &mut rng, packets, mode, slots as u32);
    let n = end_src.offset_from(src) as usize;
    core::ptr::copy_nonoverlapping(src, arena_ref, n);
    core::ptr::copy_nonoverlapping(src, arena_tst, n);
    // The table holds addresses of its own entries and of packets, so both runs
    // must see the same addresses: link the reference into the reference arena
    // with the reference table, the candidate into the test arena with the test
    // table, and compare after mapping addresses to offsets.
    clear_table(tab_ref, slots);
    clear_table(tab_tst, slots);
    tab_ref.add(slots).write(GUARD);
    tab_tst.add(slots).write(GUARD);
    link_ref(tab_ref, arena_ref, arena_ref.add(n));
    kernel(arena_tst, arena_tst.add(n), tab_tst);
    let mut bad = 0u32;
    if tab_tst.add(slots).read() != GUARD {
        bad += 1;
    }
    // Entries: packet addresses are offsets from each arena's base.
    let base_ref = arena_ref as u32 & MASK;
    let base_tst = arena_tst as u32 & MASK;
    let tbl_ref = tab_ref as u32 & MASK;
    let tbl_tst = tab_tst as u32 & MASK;
    let table_bytes = slots as u32 * 4;
    for i in 0..slots {
        let a = tab_ref.add(i).read();
        let b = tab_tst.add(i).read();
        let ok = if a >= base_ref && a < base_ref + (n as u32) * 4 {
            b == a - base_ref + base_tst
        } else if a >= tbl_ref && a < tbl_ref + table_bytes {
            b == a - tbl_ref + tbl_tst
        } else {
            a == b
        };
        if !ok {
            bad += 1;
        }
    }
    for i in 0..n {
        let a = *arena_ref.add(i);
        let b = *arena_tst.add(i);
        // Link words differ by the base; compare the count byte exactly and the
        // address as an offset. Data words are bit-identical.
        if a != b {
            // A tag word holds an address: map it.
            let (ra, rb) = (a & MASK, b & MASK);
            let ok = (a >> 24) == (b >> 24)
                && ((ra >= base_ref
                    && ra < base_ref + (n as u32) * 4
                    && rb == ra - base_ref + base_tst)
                    || (ra >= tbl_ref
                        && ra < tbl_ref + table_bytes
                        && rb == ra - tbl_ref + tbl_tst)
                    || ra == rb);
            if !ok {
                bad += 1;
            }
        }
    }
    bad
}

const GUARD: u32 = 0xdead_beef;

/// A link pass under test: `(first, end, entries)`.
type Kernel = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32);

pub fn run(cases: &mut u32, failures: &mut u32) {
    // Self-check: the harness must reject a wrong kernel on the sentinel mode.
    let caught = unsafe { run_variant(mutant_link, 4242, 500, 4) };
    *cases += 1;
    if caught == 0 {
        *failures += 1;
        psx_rt::tty::println("  harness failed to catch the mutant");
    }
    let kernels: [(&str, Kernel, Kernel); 1] = [("sdk", sdk_link, sdk_link_big)];
    let big_ref = core::ptr::addr_of_mut!(BIG_REF) as *mut u32;
    let big_tst = core::ptr::addr_of_mut!(BIG_TST) as *mut u32;
    for (name, k, k_big) in kernels {
        let mut fails = 0u32;
        psx_rt::tty::print("testing ");
        psx_rt::tty::println(name);
        let mut check = |bad: u32, mode: u32, packets: usize| {
            *cases += 1;
            if bad != 0 {
                fails += 1;
                *failures += 1;
                if fails < 4 {
                    crate::tty_fail(name, mode, packets as u32, bad);
                }
            }
        };
        // Every stream length up to 40 (the kernel is unrolled six times), then longer ones.
        for mode in 0..13u32 {
            for packets in (0..=40usize).chain([64, 500, 3000]) {
                for seed in 1..4u32 {
                    check(
                        unsafe { run_variant(k, seed * 7919 + mode, packets, mode) },
                        mode,
                        packets,
                    );
                }
            }
        }
        // The largest table: slot 0xffff is the sentinel, and the word after the last entry is
        // the address the sentinel would name; it must stay untouched.
        for mode in 0..13u32 {
            for packets in [0usize, 1, 2, 5, 64, 500, 3000] {
                for seed in 1..3u32 {
                    let bad = unsafe {
                        run_tables(
                            k_big,
                            seed * 104729 + mode,
                            packets,
                            mode,
                            BIG_N,
                            big_ref,
                            big_tst,
                        )
                    };
                    check(bad, 0x100 + mode, packets);
                }
            }
        }
    }
}

/// Time `kernel` over `reps` fresh streams of `packets` packets; returns
/// (total cycles, total packets). The empty-timer overhead is removed.
unsafe fn time_kernel(kernel: Kernel, mode: u32, packets: usize, reps: u32) -> (u32, u32) {
    let ot_tst = core::ptr::addr_of_mut!(OT_TST) as *mut u32;
    let src = core::ptr::addr_of_mut!(ARENA_SRC) as *mut u32;
    let arena_tst = core::ptr::addr_of_mut!(ARENA_TST) as *mut u32;
    let mut rng = Rng(12345 + mode);
    let end_src = make_stream(src, &mut rng, packets, mode, OT_N as u32);
    let n = end_src.offset_from(src) as usize;
    let mut total = 0u32;
    let mut overhead = 0u32;
    for _ in 0..reps {
        core::ptr::copy_nonoverlapping(src, arena_tst, n);
        clear_table(ot_tst, OT_N);
        let a = crate::clock();
        let b = crate::clock();
        overhead += b.wrapping_sub(a) as u32;
        let a = crate::clock();
        black_box(kernel)(arena_tst, arena_tst.add(n), ot_tst);
        let b = crate::clock();
        total += b.wrapping_sub(a) as u32;
    }
    (total.saturating_sub(overhead), packets as u32 * reps)
}

/// Cycles per 100 packets for stream lengths from one packet to a busy frame's worth, with the
/// slots spread over the table (0), crowded near the middle (1), ascending (5) and all in one (2).
pub fn bench() {
    let kernels: [(&str, Kernel); 2] = [("rust", rust_link), ("sdk", sdk_link)];
    for mode in [0u32, 1, 5, 2] {
        for packets in [1usize, 3, 8, 30, 100, 300, 600] {
            for (name, k) in kernels {
                let (cycles, total) =
                    unsafe { time_kernel(k, mode, packets, 2400 / packets as u32 + 1) };
                crate::tty_label(name, mode, packets as u32);
                // hundredths of a cycle per packet
                print_dec("", cycles * 100 / total);
            }
        }
    }
}
