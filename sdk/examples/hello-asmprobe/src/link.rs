// SPDX-License-Identifier: GPL-2.0-or-later
//! Tagged-packet-stream link loop: the portable reference, the inline-asm loop
//! the SDK uses by default, and the `asm-kernels` kernel.

use crate::{print_dec, print_u32, Rng};
use core::arch::global_asm;
use core::hint::black_box;

pub const OT_N: usize = 2048;
const OT_END: u32 = 0x00ff_ffff;
const MASK: u32 = 0x00ff_ffff;
const ARENA_WORDS: usize = 1 << 16;

#[repr(C, align(4))]
pub struct Ot(pub [u32; OT_N]);

static mut OT_REF: Ot = Ot([0; OT_N]);
static mut OT_TST: Ot = Ot([0; OT_N]);
static mut ARENA_REF: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];
static mut ARENA_TST: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];
static mut ARENA_SRC: [u32; ARENA_WORDS + 64] = [0; ARENA_WORDS + 64];

pub fn ot_clear(ot: &mut Ot) {
    let base = ot.0.as_ptr() as u32;
    ot.0[0] = OT_END;
    for i in 1..OT_N {
        ot.0[i] = base.wrapping_add(4 * (i as u32 - 1)) & MASK;
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

extern "C" {
    fn asmprobe_link_cur(first: *mut u32, end: *mut u32, entries: *mut u32);
}

// The inline-asm loop `ot.rs` uses without the `asm-kernels` feature, as a leaf
// function: the baseline the kernel is timed against.
global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.asmprobe_link,"ax",@progbits
    .globl asmprobe_link_cur
    .type asmprobe_link_cur,@function
asmprobe_link_cur:
    nop
    sltu   $t0, $a0, $a1
    beqz   $t0, 9f
    nop
    lui    $t7, 0x00ff
    ori    $t7, $t7, 0xffff
    ori    $t9, $zero, 0xffff
    lui    $t8, 0xff00
2:
    lw     $t1, 0($a0)
    and    $t3, $a0, $t7
    srl    $t2, $t1, 22
    andi   $t5, $t1, 0xffff
    addu   $t2, $a0, $t2
    beq    $t5, $t9, 3f
    addiu  $t2, $t2, 4
    sll    $t5, $t5, 2
    addu   $t5, $a2, $t5
    lw     $t6, 0($t5)
    and    $t1, $t1, $t8
    or     $t6, $t6, $t1
    sw     $t6, 0($a0)
    sw     $t3, 0($t5)
3:
    bne    $t2, $a1, 2b
    move   $a0, $t2
9:
    jr     $ra
    nop
    .size asmprobe_link_cur, .-asmprobe_link_cur
"#
);

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

/// The SDK's own entry point (`asm-kernels` selects the kernel under test).
unsafe extern "C" fn sdk_link(first: *mut u32, end: *mut u32, entries: *mut u32) {
    let ot = &mut *(entries as *mut psx_gpu::ot::OrderingTable<OT_N>);
    let mut frame = ot.resume_frame();
    frame.add_tagged_packet_stream_unchecked(first, end);
}

/// Fill `arena` with a deterministic packet stream and return its end.
/// `mode` selects the slot distribution; every mode keeps bits 16..23 of a tag
/// clear, as the staged format requires.
unsafe fn make_stream(arena: *mut u32, rng: &mut Rng, packets: usize, mode: u32) -> *mut u32 {
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
            0 => s % OT_N as u32,
            1 => {
                if s % 5 != 0 {
                    1000 + (s >> 8) % 64
                } else {
                    (s >> 8) % OT_N as u32
                }
            }
            2 => 0,
            3 => OT_N as u32 - 1,
            4 => {
                if s % 20 == 0 {
                    0xffff
                } else {
                    (s >> 4) % OT_N as u32
                }
            }
            5 => {
                asc = (asc + 1 + (s % 3)) % OT_N as u32;
                asc
            }
            6 => (OT_N as u32 - 1) - (i as u32 % OT_N as u32),
            9 => 0xffff,
            10 => {
                if i == 0 || i + 1 == packets {
                    0xffff
                } else {
                    (s >> 4) % OT_N as u32
                }
            }
            11 => {
                if i % 2 == 0 {
                    0xffff
                } else {
                    (s >> 4) % OT_N as u32
                }
            }
            12 => 5,
            _ => (s >> 4) % OT_N as u32,
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
    let ot_ref = &mut *core::ptr::addr_of_mut!(OT_REF);
    let ot_tst = &mut *core::ptr::addr_of_mut!(OT_TST);
    let src = core::ptr::addr_of_mut!(ARENA_SRC) as *mut u32;
    let arena_ref = core::ptr::addr_of_mut!(ARENA_REF) as *mut u32;
    let arena_tst = core::ptr::addr_of_mut!(ARENA_TST) as *mut u32;
    let mut rng = Rng(seed | 1);
    let end_src = make_stream(src, &mut rng, packets, mode);
    let n = end_src.offset_from(src) as usize;
    core::ptr::copy_nonoverlapping(src, arena_ref, n);
    core::ptr::copy_nonoverlapping(src, arena_tst, n);
    // The table holds addresses of its own entries and of packets, so both runs
    // must see the same addresses: link the reference into the reference arena
    // with the reference table, the candidate into the test arena with the test
    // table, and compare after mapping addresses to offsets.
    ot_clear(ot_ref);
    ot_clear(ot_tst);
    link_ref(ot_ref.0.as_mut_ptr(), arena_ref, arena_ref.add(n));
    kernel(arena_tst, arena_tst.add(n), ot_tst.0.as_mut_ptr());
    let mut bad = 0u32;
    // Entries: packet addresses are offsets from each arena's base.
    let base_ref = arena_ref as u32 & MASK;
    let base_tst = arena_tst as u32 & MASK;
    let tbl_ref = ot_ref.0.as_ptr() as u32 & MASK;
    let tbl_tst = ot_tst.0.as_ptr() as u32 & MASK;
    for i in 0..OT_N {
        let a = ot_ref.0[i];
        let b = ot_tst.0[i];
        let ok = if a >= base_ref && a < base_ref + (n as u32) * 4 {
            b == a - base_ref + base_tst
        } else if a >= tbl_ref && a < tbl_ref + (OT_N as u32) * 4 {
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
        let is_tag = a & 0x00ff_0000 == 0 && false;
        let _ = is_tag;
        if a != b {
            // A tag word holds an address: map it.
            let (ra, rb) = (a & MASK, b & MASK);
            let ok = (a >> 24) == (b >> 24)
                && ((ra >= base_ref
                    && ra < base_ref + (n as u32) * 4
                    && rb == ra - base_ref + base_tst)
                    || (ra >= tbl_ref
                        && ra < tbl_ref + (OT_N as u32) * 4
                        && rb == ra - tbl_ref + tbl_tst)
                    || ra == rb);
            if !ok {
                bad += 1;
            }
        }
    }
    bad
}

pub fn run(cases: &mut u32, failures: &mut u32) {
    // Self-check: the harness must reject a wrong kernel on the sentinel mode.
    let caught = unsafe { run_variant(mutant_link, 4242, 500, 4) };
    *cases += 1;
    if caught == 0 {
        *failures += 1;
        psx_rt::tty::println("  harness failed to catch the mutant");
    }
    let kernels: [(&str, unsafe extern "C" fn(*mut u32, *mut u32, *mut u32)); 2] =
        [("cur", asmprobe_link_cur), ("sdk", sdk_link)];
    for (name, k) in kernels.iter() {
        let mut fails = 0u32;
        psx_rt::tty::print("testing ");
        psx_rt::tty::println(name);
        for mode in 0..13u32 {
            for &packets in &[0usize, 1, 2, 3, 7, 64, 500, 3000] {
                for seed in 1..6u32 {
                    *cases += 1;
                    let bad = unsafe { run_variant(*k, seed * 7919 + mode, packets, mode) };
                    if bad != 0 {
                        fails += 1;
                        *failures += 1;
                        if fails < 4 {
                            crate::tty_fail(name, mode, packets as u32, bad);
                        }
                    }
                }
            }
        }
    }
}

/// Time `kernel` over `reps` fresh streams of `packets` packets; returns
/// (total cycles, total packets). The empty-timer overhead is removed.
unsafe fn time_kernel(
    kernel: unsafe extern "C" fn(*mut u32, *mut u32, *mut u32),
    mode: u32,
    packets: usize,
    reps: u32,
) -> (u32, u32) {
    let ot_tst = &mut *core::ptr::addr_of_mut!(OT_TST);
    let src = core::ptr::addr_of_mut!(ARENA_SRC) as *mut u32;
    let arena_tst = core::ptr::addr_of_mut!(ARENA_TST) as *mut u32;
    let mut rng = Rng(12345 + mode);
    let end_src = make_stream(src, &mut rng, packets, mode);
    let n = end_src.offset_from(src) as usize;
    let mut total = 0u32;
    let mut overhead = 0u32;
    for _ in 0..reps {
        core::ptr::copy_nonoverlapping(src, arena_tst, n);
        ot_clear(ot_tst);
        let a = crate::clock();
        let b = crate::clock();
        overhead += b.wrapping_sub(a) as u32;
        let a = crate::clock();
        black_box(kernel)(arena_tst, arena_tst.add(n), ot_tst.0.as_mut_ptr());
        let b = crate::clock();
        total += b.wrapping_sub(a) as u32;
    }
    (total.saturating_sub(overhead), packets as u32 * reps)
}

pub fn bench() {
    let kernels: [(&str, unsafe extern "C" fn(*mut u32, *mut u32, *mut u32)); 2] =
        [("cur", asmprobe_link_cur), ("sdk", sdk_link)];
    for mode in [0u32, 1, 4] {
        for (name, k) in kernels.iter() {
            let (cycles, packets) = unsafe { time_kernel(*k, mode, 300, 16) };
            crate::tty_label(name, mode);
            // hundredths of a cycle per packet
            print_dec("", cycles * 100 / packets);
        }
    }
}
