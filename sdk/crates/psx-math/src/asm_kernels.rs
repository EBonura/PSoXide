// SPDX-License-Identifier: GPL-2.0-or-later
//! Hand-scheduled R3000 kernels (feature `asm-kernels`).
//!
//! Each kernel is differentially tested against its portable definition by the
//! `hello-asmprobe` guest and is kept only because it measured faster on the
//! emulator's cycle model; the portable definition stays the default.

use crate::clip_lanes::{LanePlane, Lanes8};
use core::arch::global_asm;

extern "C" {
    /// See [`crate::clip_lanes::clip_lanes8_plane`].
    pub(crate) fn psx_math_clip_lanes8_plane(
        src: *const Lanes8,
        count: usize,
        dst: *mut Lanes8,
        plane: *const LanePlane,
    ) -> usize;
}

// clip_lanes8_plane: one-plane Sutherland-Hodgman over eight-lane i32 vertices.
//
// a0 = cur, a1 = end, a2 = dst, a3 = plane; s0 = prev, s1 = prev distance,
// s2 = byte offset of the primary lane, s3 = primary, s4 = depth, s5 = bias;
// t0 = cur distance, t6 = end of dst, t7 = start of dst.
//
// The interpolation is `a + low32(floor((b - a) * t / 4096))`, one signed
// 32x32->64 multiply per lane: it equals the reference's
// `a + (delta / 4096) * t + ((delta % 4096) * t >> 12)` for every `delta` (an
// integer split of the product), with `t` in `0..=4096`.
global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.psx_math_clip_lanes,"ax",@progbits

    .globl psx_math_clip_lanes8_plane
    .type psx_math_clip_lanes8_plane,@function
psx_math_clip_lanes8_plane:
    nop                             # entry nop: immune to a hoisted argument load
    bnez   $a1, 1f
    move   $v0, $zero
    jr     $ra
    nop
1:
    addiu  $sp, $sp, -24
    sw     $s0, 0($sp)
    sw     $s1, 4($sp)
    sw     $s2, 8($sp)
    sw     $s3, 12($sp)
    sw     $s4, 16($sp)
    sw     $s5, 20($sp)
    lw     $s2, 0($a3)
    lw     $s3, 4($a3)
    lw     $s4, 8($a3)
    lw     $s5, 12($a3)
    lw     $t6, 24($a3)
    sll    $a1, $a1, 5
    addu   $a1, $a0, $a1
    move   $t7, $a2
    sll    $t6, $t6, 5
    addu   $t6, $a2, $t6
    addiu  $s0, $a1, -32
    # Distance of the last vertex.
    addu   $t1, $s0, $s2
    lw     $t2, 0($t1)
    lw     $t3, 8($s0)
    nop
    mult   $s3, $t2
    mflo   $t4
    mult   $s4, $t3
    mflo   $t5
    addu   $s1, $t4, $t5
    addu   $s1, $s1, $s5
.Lpsx_math_clip_loop:
    addu   $t1, $a0, $s2
    lw     $t2, 0($t1)             # the primary lane
    lw     $t3, 8($a0)             # z
    nop
    mult   $s3, $t2
    mflo   $t4
    mult   $s4, $t3
    mflo   $t5
    addu   $t0, $t4, $t5
    addu   $t0, $t0, $s5
    slt    $t1, $s1, $zero
    slt    $t4, $t0, $zero
    bne    $t1, $t4, .Lpsx_math_clip_cross
    nop
.Lpsx_math_clip_copy:
    bnez   $t4, .Lpsx_math_clip_next
    nop
    beq    $a2, $t6, .Lpsx_math_clip_full
    nop
    # Copy the vertex; the primary lane and z are in $t2 and $t3 already.
    xori   $t5, $s2, 4
    addu   $t1, $a0, $t5            # the other of x and y, source
    addu   $t5, $a2, $t5            # ... and destination
    lw     $t8, 0($t1)
    addu   $t1, $a2, $s2
    sw     $t2, 0($t1)
    lw     $t9, 12($a0)
    sw     $t3, 8($a2)
    lw     $t1, 16($a0)
    sw     $t8, 0($t5)
    lw     $t2, 20($a0)
    sw     $t9, 12($a2)
    lw     $t3, 24($a0)
    sw     $t1, 16($a2)
    lw     $t8, 28($a0)
    sw     $t2, 20($a2)
    sw     $t3, 24($a2)
    sw     $t8, 28($a2)
    addiu  $a2, $a2, 32
.Lpsx_math_clip_next:
    move   $s0, $a0
    move   $s1, $t0
    addiu  $a0, $a0, 32
    bne    $a0, $a1, .Lpsx_math_clip_loop
    nop
    subu   $v0, $a2, $t7
    srl    $v0, $v0, 5
.Lpsx_math_clip_exit:
    lw     $s0, 0($sp)
    lw     $s1, 4($sp)
    lw     $s2, 8($sp)
    lw     $s3, 12($sp)
    lw     $s4, 16($sp)
    lw     $s5, 20($sp)
    addiu  $sp, $sp, 24
    jr     $ra
    nop
.Lpsx_math_clip_full:
    b      .Lpsx_math_clip_exit
    lw     $v0, 24($a3)

    # A crossing between prev ($s0, $s1) and cur ($a0, $t0).
.Lpsx_math_clip_cross:
    beq    $a2, $t6, .Lpsx_math_clip_full
    nop
    # Order the endpoints by (x, y, z), signed and lexicographic.
    lw     $t3, 0($a0)
    lw     $t4, 0($s0)
    nop
    bne    $t3, $t4, 2f
    slt    $t5, $t3, $t4
    lw     $t3, 4($a0)
    lw     $t4, 4($s0)
    nop
    bne    $t3, $t4, 2f
    slt    $t5, $t3, $t4
    lw     $t3, 8($a0)
    lw     $t4, 8($s0)
    nop
    slt    $t5, $t3, $t4
2:
    move   $t8, $s0
    move   $t9, $a0
    move   $t1, $s1
    beqz   $t5, 3f
    move   $t2, $t0
    move   $t8, $a0
    move   $t9, $s0
    move   $t1, $t0
    move   $t2, $s1
3:
    # t = ratio_q12_i32(da = $t1, da - db), start the divide, and load lane 0
    # while it runs.
    subu   $t2, $t1, $t2
    beqz   $t2, 6f
    xor    $t3, $t1, $t2
    bltz   $t3, 6f
    sra    $t3, $t1, 31
    xor    $t1, $t1, $t3
    subu   $t1, $t1, $t3
    sra    $t3, $t2, 31
    xor    $t2, $t2, $t3
    subu   $t2, $t2, $t3
4:
    srl    $t3, $t1, 19
    beqz   $t3, 5f
    nop
    addiu  $t1, $t1, 1
    srl    $t1, $t1, 1
    addiu  $t2, $t2, 1
    b      4b
    srl    $t2, $t2, 1
5:
    beqz   $t2, 6f
    sll    $t1, $t1, 12
    divu   $zero, $t1, $t2
    lw     $t1, 0($t8)
    lw     $t2, 0($t9)
    nop
    subu   $v0, $t2, $t1
    mflo   $t5
    sltiu  $t3, $t5, 4097
    bnez   $t3, 7f
    nop
    b      7f
    ori    $t5, $zero, 4096
6:
    move   $t5, $zero
    lw     $t1, 0($t8)
    lw     $t2, 0($t9)
    nop
    subu   $v0, $t2, $t1
7:
    mult   $t5, $v0
    lw     $t3, 4($t8)
    lw     $t4, 4($t9)
    mflo   $s1
    mfhi   $t2
    srl    $s1, $s1, 12
    sll    $t2, $t2, 20
    or     $s1, $s1, $t2
    addu   $s1, $s1, $t1
    sw     $s1, 0($a2)
    subu   $v1, $t4, $t3
    mult   $t5, $v1
    lw     $t1, 8($t8)
    lw     $t2, 8($t9)
    mflo   $s1
    mfhi   $t4
    srl    $s1, $s1, 12
    sll    $t4, $t4, 20
    or     $s1, $s1, $t4
    addu   $s1, $s1, $t3
    sw     $s1, 4($a2)
    subu   $v0, $t2, $t1
    mult   $t5, $v0
    lw     $t3, 12($t8)
    lw     $t4, 12($t9)
    mflo   $s1
    mfhi   $t2
    srl    $s1, $s1, 12
    sll    $t2, $t2, 20
    or     $s1, $s1, $t2
    addu   $s1, $s1, $t1
    sw     $s1, 8($a2)
    subu   $v1, $t4, $t3
    mult   $t5, $v1
    lw     $t1, 16($t8)
    lw     $t2, 16($t9)
    mflo   $s1
    mfhi   $t4
    srl    $s1, $s1, 12
    sll    $t4, $t4, 20
    or     $s1, $s1, $t4
    addu   $s1, $s1, $t3
    sw     $s1, 12($a2)
    subu   $v0, $t2, $t1
    mult   $t5, $v0
    lw     $t3, 20($t8)
    lw     $t4, 20($t9)
    mflo   $s1
    mfhi   $t2
    srl    $s1, $s1, 12
    sll    $t2, $t2, 20
    or     $s1, $s1, $t2
    addu   $s1, $s1, $t1
    sw     $s1, 16($a2)
    subu   $v1, $t4, $t3
    mult   $t5, $v1
    lw     $t1, 24($t8)
    lw     $t2, 24($t9)
    mflo   $s1
    mfhi   $t4
    srl    $s1, $s1, 12
    sll    $t4, $t4, 20
    or     $s1, $s1, $t4
    addu   $s1, $s1, $t3
    sw     $s1, 20($a2)
    subu   $v0, $t2, $t1
    mult   $t5, $v0
    lw     $t3, 28($t8)
    lw     $t4, 28($t9)
    mflo   $s1
    mfhi   $t2
    srl    $s1, $s1, 12
    sll    $t2, $t2, 20
    or     $s1, $s1, $t2
    addu   $s1, $s1, $t1
    sw     $s1, 24($a2)
    subu   $v1, $t4, $t3
    mult   $t5, $v1
    mflo   $s1
    mfhi   $t4
    srl    $s1, $s1, 12
    sll    $t4, $t4, 20
    or     $s1, $s1, $t4
    addu   $s1, $s1, $t3
    sw     $s1, 28($a2)
    lw     $t1, 16($a3)
    lw     $t2, 20($a3)
    nop
    beqz   $t1, 8f
    nop
    sw     $t2, 8($a2)
8:
    addiu  $a2, $a2, 32
    slt    $t4, $t0, $zero
    bnez   $t4, .Lpsx_math_clip_next
    addu   $t1, $a0, $s2
    # The copy wants the primary lane and z of cur back.
    lw     $t2, 0($t1)
    lw     $t3, 8($a0)
    b      .Lpsx_math_clip_copy
    nop
    .size psx_math_clip_lanes8_plane, .-psx_math_clip_lanes8_plane
"#
);
