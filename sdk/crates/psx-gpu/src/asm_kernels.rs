// SPDX-License-Identifier: GPL-2.0-or-later
//! Hand-scheduled R3000 kernels for the ordering-table link passes.
//!
//! Compiled in with the `asm-kernels` feature, which is off by default: the
//! inline-asm loops in [`crate::ot`] stay the reference and the default, and
//! the portable Rust definition is what the host build runs. Every kernel here
//! is differentially tested against that definition by the `hello-asmprobe`
//! guest and was kept only because it measured faster on the emulator's cycle
//! model (see `docs/asm-kernels-2026-10-08.md`).
//!
//! # Why these loops are slower than their instruction count
//!
//! A RAM load costs seven clocks and the R3000 has no data cache, so a link
//! pass that does `load tag, compute slot, load head, store` spends more time
//! waiting than computing. The CPU keeps issuing instructions during a RAM
//! load as long as they neither use the bus nor read the loaded register: the
//! first two pay their clock, the next four are free (hwtest v1.23 records
//! 0x125-0x128). These kernels therefore
//!
//! * run ahead by one packet: the next tag is loaded while this packet's slot
//!   arithmetic runs, and the OT head load is followed by the next packet's
//!   pointer arithmetic (seven ALU instructions, four of them free);
//! * address RAM through its physical (KUSEG) alias, which is the form the
//!   OT stores anyway, so no per-packet address mask is needed;
//! * rotate three pointer registers and two tag/entry/mask banks across six
//!   copies of the body, so nothing is ever moved between registers.
//!
//! Loads and stores are issued in the same order as the reference per packet;
//! a packet's tag is never read after it has been rewritten.

use core::arch::global_asm;

extern "C" {
    /// Link a contiguous stream of tagged packets into an ordering table.
    ///
    /// `a0 = first`, `a1 = end`, `a2 = entries`. See
    /// [`crate::ot::OrderingTable::link_tagged_packet_stream_unchecked`] for
    /// the tag format and the safety contract.
    pub(crate) fn psx_gpu_link_tagged_stream(first: *mut u32, end: *mut u32, entries: *mut u32);
}

// Registers (all caller-saved): P ring a0/a3/v1, tags t0/t1, entry addresses
// t2/t3, word-count masks t4/t5, old head t6, scratch v0; a1 = end,
// a2 = entries, t7 = &entries[0xffff] (the skip sentinel), t9 = 0xff000000.
//
// Iteration j (BODY): the `beq` ends the pass when the next packet is `end`
// (its delay slot issues the OT head load); then the next packet's pointer
// (`Pq`), entry address (`en`) and mask (`topn`) are computed in the head
// load's shadow, the head and mask are merged, the next-but-one tag is loaded
// in the sentinel branch's delay slot, and the two stores link the packet.
global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.psx_gpu_link,"ax",@progbits
    .macro PSX_GPU_BODY Pc, Pn, Pq, Tc, Tn, ec, en, topc, topn, last, skip
    beq    \Pn, $a1, \last
    lw     $t6, 0(\ec)
    srl    $v0, \Tn, 22
    addu   \Pq, \Pn, $v0
    addiu  \Pq, \Pq, 4
    andi   $v0, \Tn, 0xffff
    sll    $v0, $v0, 2
    addu   \en, $a2, $v0
    and    \topn, \Tn, $t9
    or     $t6, $t6, \topc
    beq    \ec, $t7, \skip
    lw     \Tc, 0(\Pq)
    sw     $t6, 0(\Pc)
    sw     \Pc, 0(\ec)
\skip:
    .endm
    .macro PSX_GPU_LAST Pc, ec, topc, last
\last:
    beq    \ec, $t7, 9f
    nop
    or     $t6, $t6, \topc
    sw     $t6, 0(\Pc)
    sw     \Pc, 0(\ec)
    b      9f
    nop
    .endm

    .globl psx_gpu_link_tagged_stream
    .type psx_gpu_link_tagged_stream,@function
psx_gpu_link_tagged_stream:
    nop                             # entry nop: immune to a hoisted argument load
    sltu   $t0, $a0, $a1
    beqz   $t0, 9f
    lui    $t8, 0x00ff
    ori    $t8, $t8, 0xffff
    and    $a0, $a0, $t8            # physical addresses, the form the OT holds
    and    $a1, $a1, $t8
    and    $a2, $a2, $t8
    lui    $t9, 0xff00
    ori    $t7, $zero, 0xffff
    sll    $t7, $t7, 2
    addu   $t7, $a2, $t7
    # Packet 0: its tag, the pointer to packet 1, packet 1's tag, and
    # packet 0's entry address and word-count mask.
    lw     $t0, 0($a0)
    nop
    srl    $v0, $t0, 22
    addu   $a3, $a0, $v0
    addiu  $a3, $a3, 4
    lw     $t1, 0($a3)
    andi   $v0, $t0, 0xffff
    sll    $v0, $v0, 2
    addu   $t2, $a2, $v0
    and    $t4, $t0, $t9
2:
    PSX_GPU_BODY $a0, $a3, $v1, $t0, $t1, $t2, $t3, $t4, $t5, .Lpsx_gpu_last0, .Lpsx_gpu_skip0
    PSX_GPU_BODY $a3, $v1, $a0, $t1, $t0, $t3, $t2, $t5, $t4, .Lpsx_gpu_last1, .Lpsx_gpu_skip1
    PSX_GPU_BODY $v1, $a0, $a3, $t0, $t1, $t2, $t3, $t4, $t5, .Lpsx_gpu_last2, .Lpsx_gpu_skip2
    PSX_GPU_BODY $a0, $a3, $v1, $t1, $t0, $t3, $t2, $t5, $t4, .Lpsx_gpu_last3, .Lpsx_gpu_skip3
    PSX_GPU_BODY $a3, $v1, $a0, $t0, $t1, $t2, $t3, $t4, $t5, .Lpsx_gpu_last4, .Lpsx_gpu_skip4
    PSX_GPU_BODY $v1, $a0, $a3, $t1, $t0, $t3, $t2, $t5, $t4, .Lpsx_gpu_last5, .Lpsx_gpu_skip5
    b      2b
    nop
    PSX_GPU_LAST $a0, $t2, $t4, .Lpsx_gpu_last0
    PSX_GPU_LAST $a3, $t3, $t5, .Lpsx_gpu_last1
    PSX_GPU_LAST $v1, $t2, $t4, .Lpsx_gpu_last2
    PSX_GPU_LAST $a0, $t3, $t5, .Lpsx_gpu_last3
    PSX_GPU_LAST $a3, $t2, $t4, .Lpsx_gpu_last4
    PSX_GPU_LAST $v1, $t3, $t5, .Lpsx_gpu_last5
9:
    jr     $ra
    nop
    .size psx_gpu_link_tagged_stream, .-psx_gpu_link_tagged_stream
"#
);
