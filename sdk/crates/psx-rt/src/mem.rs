// SPDX-License-Identifier: GPL-2.0-or-later
//! Hand-scheduled R3000 `memcpy` / `memset` / `memcmp` / `bcmp`.
//!
//! `compiler-builtins` (built with `compiler-builtins-mem`) supplies these as
//! weak symbols implemented as generic Rust loops: one word per iteration,
//! six instructions per four bytes copied, and a byte loop for anything
//! misaligned. They show up at two to four percent of retired instructions in
//! every guest that streams packets, poses or sectors through RAM.
//!
//! These strong definitions override them for every guest that links
//! `psx-rt`, the same way [`super::builtins`] overrides the broken signed
//! 64-bit divide. The loops move sixteen bytes per iteration with the loop
//! counter folded into the end-pointer compare, use `lwl`/`lwr` for a
//! misaligned source instead of a shift-and-merge, and keep every register
//! in the caller-saved set so no frame is needed. Load delay slots are
//! honoured explicitly (`.set noreorder`): no register is read in the
//! instruction after the load that writes it.
//!
//! Only `memmove` is left to `compiler-builtins`; nothing hot calls it.
//!
//! **Entry nop.** LLVM's delay-slot filler may hoist an argument load into
//! the caller's `jal`/`jalr` delay slot, so the callee's first instruction
//! runs inside that load's delay and would read the stale register. The
//! post-link trampoline patch (`tools/hazard_patch.py`) fixes the `jal` sites
//! it can see, but a register call (`jalr`) has no static target. One `nop`
//! at each entry makes these routines immune regardless of who calls them;
//! it costs one cycle per call against loops that move sixteen bytes per
//! eleven instructions.

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.psx_rt_mem,"ax",@progbits

# ---------------------------------------------------------------------------
# void *memcpy(void *dst, const void *src, size_t n)
#   a0 = dst, a1 = src, a2 = n, returns v0 = dst
# ---------------------------------------------------------------------------
    .globl memcpy
    .type memcpy,@function
memcpy:
    nop                             # see "Entry nop" above
    sltiu  $t0, $a2, 16
    bnez   $t0, .Lcpy_tail
    move   $v0, $a0
    andi   $t0, $a0, 3
    beqz   $t0, .Lcpy_dst_aligned
    nop
    # Head bytes until the destination is word aligned (n >= 16 so this
    # cannot exhaust the count).
.Lcpy_head:
    lbu    $t1, 0($a1)
    addiu  $a1, $a1, 1
    addiu  $a2, $a2, -1
    sb     $t1, 0($a0)
    addiu  $a0, $a0, 1
    andi   $t0, $a0, 3
    bnez   $t0, .Lcpy_head
    nop
.Lcpy_dst_aligned:
    andi   $t0, $a1, 3
    bnez   $t0, .Lcpy_src_misaligned
    srl    $t1, $a2, 4
    # Both aligned: sixteen bytes per iteration, t2 = start of the last block.
    beqz   $t1, .Lcpy_words
    sll    $t1, $t1, 4
    addu   $t2, $a0, $t1
    addiu  $t2, $t2, -16
    subu   $a2, $a2, $t1
.Lcpy_block:
    lw     $t3, 0($a1)
    lw     $t4, 4($a1)
    lw     $t5, 8($a1)
    lw     $t6, 12($a1)
    addiu  $a1, $a1, 16
    sw     $t3, 0($a0)
    sw     $t4, 4($a0)
    sw     $t5, 8($a0)
    sw     $t6, 12($a0)
    bne    $a0, $t2, .Lcpy_block
    addiu  $a0, $a0, 16
.Lcpy_words:
    # Fewer than sixteen bytes left, both pointers word aligned.
    srl    $t1, $a2, 2
    beqz   $t1, .Lcpy_tail
    andi   $a2, $a2, 3
.Lcpy_word:
    lw     $t3, 0($a1)
    addiu  $t1, $t1, -1
    addiu  $a1, $a1, 4
    sw     $t3, 0($a0)
    bnez   $t1, .Lcpy_word
    addiu  $a0, $a0, 4
.Lcpy_tail:
    beqz   $a2, .Lcpy_done
    nop
.Lcpy_byte:
    lbu    $t3, 0($a1)
    addiu  $a2, $a2, -1
    addiu  $a1, $a1, 1
    sb     $t3, 0($a0)
    bnez   $a2, .Lcpy_byte
    addiu  $a0, $a0, 1
.Lcpy_done:
    jr     $ra
    nop

    # Destination aligned, source not: unaligned word loads through
    # lwr/lwl (little endian: lwr takes the low bytes from the lower
    # address). The pairs are interleaved so the lwl that merges into a
    # register runs two instructions after the lwr that started it.
.Lcpy_src_misaligned:
    beqz   $t1, .Lcpy_uwords
    sll    $t1, $t1, 4
    addu   $t2, $a0, $t1
    addiu  $t2, $t2, -16
    subu   $a2, $a2, $t1
.Lcpy_ublock:
    lwr    $t3, 0($a1)
    lwr    $t4, 4($a1)
    lwl    $t3, 3($a1)
    lwl    $t4, 7($a1)
    lwr    $t5, 8($a1)
    lwr    $t6, 12($a1)
    lwl    $t5, 11($a1)
    lwl    $t6, 15($a1)
    addiu  $a1, $a1, 16
    sw     $t3, 0($a0)
    sw     $t4, 4($a0)
    sw     $t5, 8($a0)
    sw     $t6, 12($a0)
    bne    $a0, $t2, .Lcpy_ublock
    addiu  $a0, $a0, 16
.Lcpy_uwords:
    srl    $t1, $a2, 2
    beqz   $t1, .Lcpy_tail
    andi   $a2, $a2, 3
.Lcpy_uword:
    lwr    $t3, 0($a1)
    lwl    $t3, 3($a1)
    addiu  $t1, $t1, -1
    addiu  $a1, $a1, 4
    sw     $t3, 0($a0)
    bnez   $t1, .Lcpy_uword
    addiu  $a0, $a0, 4
    b      .Lcpy_tail
    nop
    .size memcpy, .-memcpy

# ---------------------------------------------------------------------------
# void *memset(void *s, int c, size_t n)
#   a0 = s, a1 = c, a2 = n, returns v0 = s
# ---------------------------------------------------------------------------
    .globl memset
    .type memset,@function
memset:
    nop                             # see "Entry nop" above
    sltiu  $t0, $a2, 16
    bnez   $t0, .Lset_tail
    move   $v0, $a0
    andi   $a1, $a1, 0xff
    sll    $t0, $a1, 8
    or     $a1, $a1, $t0
    sll    $t0, $a1, 16
    or     $a1, $a1, $t0
    andi   $t0, $a0, 3
    beqz   $t0, .Lset_aligned
    nop
.Lset_head:
    sb     $a1, 0($a0)
    addiu  $a0, $a0, 1
    addiu  $a2, $a2, -1
    andi   $t0, $a0, 3
    bnez   $t0, .Lset_head
    nop
.Lset_aligned:
    srl    $t1, $a2, 4
    beqz   $t1, .Lset_words
    sll    $t1, $t1, 4
    addu   $t2, $a0, $t1
    addiu  $t2, $t2, -16
    subu   $a2, $a2, $t1
.Lset_block:
    sw     $a1, 0($a0)
    sw     $a1, 4($a0)
    sw     $a1, 8($a0)
    sw     $a1, 12($a0)
    bne    $a0, $t2, .Lset_block
    addiu  $a0, $a0, 16
.Lset_words:
    srl    $t1, $a2, 2
    beqz   $t1, .Lset_tail
    andi   $a2, $a2, 3
.Lset_word:
    addiu  $t1, $t1, -1
    sw     $a1, 0($a0)
    bnez   $t1, .Lset_word
    addiu  $a0, $a0, 4
.Lset_tail:
    beqz   $a2, .Lset_done
    nop
.Lset_byte:
    addiu  $a2, $a2, -1
    sb     $a1, 0($a0)
    bnez   $a2, .Lset_byte
    addiu  $a0, $a0, 1
.Lset_done:
    jr     $ra
    nop
    .size memset, .-memset

# ---------------------------------------------------------------------------
# int memcmp(const void *s1, const void *s2, size_t n)
# int bcmp(const void *s1, const void *s2, size_t n)
#   a0 = s1, a1 = s2, a2 = n, returns v0 = first byte difference (s1 - s2)
#   Word compare while both pointers are aligned; the first differing word
#   is re-read bytewise so the sign of the result is the C one.
# ---------------------------------------------------------------------------
    .globl memcmp
    .type memcmp,@function
    .globl bcmp
    .type bcmp,@function
memcmp:
bcmp:
    nop                             # see "Entry nop" above
    beqz   $a2, .Lcmp_equal
    or     $t0, $a0, $a1
    andi   $t0, $t0, 3
    bnez   $t0, .Lcmp_bytes
    srl    $t1, $a2, 2
    beqz   $t1, .Lcmp_bytes
    nop
.Lcmp_word:
    lw     $t2, 0($a0)
    lw     $t3, 0($a1)
    addiu  $t1, $t1, -1
    bne    $t2, $t3, .Lcmp_bytes
    nop
    addiu  $a0, $a0, 4
    addiu  $a2, $a2, -4
    bnez   $t1, .Lcmp_word
    addiu  $a1, $a1, 4
.Lcmp_bytes:
    beqz   $a2, .Lcmp_equal
    nop
.Lcmp_byte:
    lbu    $t2, 0($a0)
    lbu    $t3, 0($a1)
    addiu  $a2, $a2, -1
    bne    $t2, $t3, .Lcmp_differ
    addiu  $a0, $a0, 1
    bnez   $a2, .Lcmp_byte
    addiu  $a1, $a1, 1
.Lcmp_equal:
    jr     $ra
    move   $v0, $zero
.Lcmp_differ:
    jr     $ra
    subu   $v0, $t2, $t3
    .size memcmp, .-memcmp
    .set macro
    .set reorder
"#
);
