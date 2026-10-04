//! Ordering Table: depth-sorted linked-list of GPU primitives.
//!
//! The PS1 has no Z-buffer. Games sort primitives back-to-front
//! (painter's algorithm) by inserting them into an OT slot indexed
//! by depth. Each OT slot is the head of a linked list; primitives
//! prepend themselves so the most-recently-inserted draws first
//! within a slot.
//!
//! Once a frame's primitives are inserted, the whole OT is shipped
//! to GPU GP0 via DMA channel 2 in linked-list mode. The DMA walker
//! follows the `next` pointers embedded in each packet's first word
//! until it hits `0x00FFFFFF` (end of chain).
//!
//! Each 32-bit OT entry (and primitive header) is:
//!
//! ```text
//!   bits 0..=23: address of next packet (24-bit, masked into RAM)
//!   bits 24..=31: word count of this packet's data, at most
//!                 `crate::chain::MAX_NODE_WORDS` (16) on silicon
//! ```
//!
//! An "empty OT" has every entry pointing at its predecessor,
//! ending in `0x00FFFFFF`. Submitting such an OT sends nothing to
//! GP0. As primitives are added, their packets chain in.

use core::marker::PhantomData;
use core::ptr;
use psx_io::periph::OrderingTableClearDma;

const OT_ADDR_MASK: u32 = psx_hw::dma::linked_list::ADDRESS_MASK;
const OT_END: u32 = psx_hw::dma::linked_list::END;
const OT_MAX_EXTRA_HOPS: usize = 131_072;
/// A staged-tag bit that tagged-stream insertion ignores: the stream inserts
/// keep only the word count and the slot from a staged tag.
#[deprecated(note = "has no effect: the scoped texture-window coalescing that read it was removed")]
pub const TAG_SCOPED_TEXTURE_WINDOW: u32 = 1 << 16;
/// Whether DMA can read `packet`: on the console, main RAM (any mirror or
/// segment) and not the scratchpad, whose low 24 bits would name a RAM
/// address instead. Host builds accept any address.
fn is_dma_reachable(packet: *mut u32) -> bool {
    if cfg!(target_arch = "mips") {
        let physical = psx_hw::memory::to_physical(packet.addr() as u32);
        physical < psx_hw::memory::ram::MIRROR_END
    } else {
        true
    }
}

/// Fixed-size OT. `N` depth slots. Typical values: 256, 1024, 4096.
#[repr(C, align(4))]
pub struct OrderingTable<const N: usize> {
    entries: [u32; N],
}

impl<const N: usize> OrderingTable<N> {
    /// Create a table with every slot being a chain terminator.
    /// Call [`clear`](Self::clear) before submitting -- that wires
    /// up the inter-slot chain so DMA walks across all `N` slots.
    pub const fn new() -> Self {
        const { assert!(N > 0, "an ordering table needs at least one slot") };
        Self {
            entries: [OT_END; N],
        }
    }

    /// Reset every slot for a fresh frame. Entry `[0]` is the
    /// terminator (farthest from camera); each higher slot points
    /// to the slot below. Submission starts at `[N-1]` so the
    /// DMA walker visits `[N-1] → [N-2] → … → [0] → end`.
    #[doc(alias = "ClearOTagR")]
    pub fn clear(&mut self) {
        // CPU clear by default: the OTC DMA is one of the channels the
        // CL2 silicon probes showed can wedge busy-forever on real
        // hardware, and a wedged boot-time clear freezes the engine at
        // its first frame with no diagnostic. N stores per frame is a
        // measurable but small cost; callers that trust their DMA can
        // opt back in via [`Self::clear_with_dma`].
        self.clear_software();
    }

    /// [`clear`](Self::clear) through the ordering-table-clear DMA channel
    /// (channel 6) instead of the CPU.
    ///
    /// The wait is bounded: if the channel wedges, which the CL2 silicon
    /// probes saw it do, or the table is longer than a 16-bit word count,
    /// the CPU clear runs instead, so the table is always valid. Host
    /// builds always clear with the CPU.
    #[doc(alias = "ClearOTagR")]
    pub fn clear_with_dma(&mut self, dma: &mut OrderingTableClearDma) {
        #[cfg(not(target_arch = "mips"))]
        let _ = &dma;
        #[cfg(target_arch = "mips")]
        if dma.clear_table(&mut self.entries) {
            return;
        }
        self.clear_software();
    }

    /// Renamed to [`Self::clear_with_dma`], which takes the channel's token.
    #[deprecated(note = "use `clear_with_dma` with the `OrderingTableClearDma` token")]
    #[inline(always)]
    pub fn clear_via_otc_dma(&mut self) {
        // SAFETY: a token is a logic guard, not a memory-safety one (see
        // `psx_io::periph`); this method never took one.
        self.clear_with_dma(&mut unsafe { OrderingTableClearDma::steal() });
    }

    #[cfg(not(target_arch = "mips"))]
    fn clear_software(&mut self) {
        // Slot 0 is the sentinel; chain walks stop here. Each other slot links
        // the one below it, by the low 24 bits of its address.
        let base = self.entries.as_mut_ptr().expose_provenance() as u32;
        self.entries[0] = OT_END;
        for i in 1..N {
            self.entries[i] = base.wrapping_add(4 * (i as u32 - 1)) & OT_ADDR_MASK;
        }
    }

    #[cfg(target_arch = "mips")]
    fn clear_software(&mut self) {
        // The PS1 RAM window occupies only the low 2 MiB of the DMA address
        // domain, so advancing a low-24-bit OT address across this table
        // cannot wrap. Schedule eight dependent pointer values per branch;
        // this preserves the exact OTC chain while avoiding the scalar
        // pointer-mask and branch cost for every one of the 2,048 Quake slots.
        let entries = self.entries.as_mut_ptr();
        // SAFETY: `entries` points at the `N` (at least one) words `self` owns.
        unsafe { ptr::write(entries, OT_END) };
        let bulk_words = (N.saturating_sub(1) / 8) * 8;
        // SAFETY: one past entry 0 stays inside `entries`, or one past its end.
        let mut cursor = unsafe { entries.add(1) };
        // SAFETY: `bulk_words` is at most `N - 1`, so this stays inside `entries` or one past
        // it.
        let bulk_end = unsafe { cursor.add(bulk_words) };
        let mut previous = entries.expose_provenance() as u32 & OT_ADDR_MASK;
        if bulk_words != 0 {
            // SAFETY: the loop stores `bulk_words` words from `entries + 1`, all inside the
            // table, and touches no other memory or the stack.
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "2:",
                    "sw $9, 0($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 4($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 8($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 12($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 16($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 20($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 24($8)",
                    "addiu $9, $9, 4",
                    "sw $9, 28($8)",
                    "addiu $9, $9, 4",
                    "addiu $8, $8, 32",
                    "bne $8, $10, 2b",
                    "nop",
                    ".set reorder",
                    inout("$8") cursor,
                    inout("$9") previous,
                    in("$10") bulk_end,
                    options(nostack),
                );
            }
        }
        // SAFETY: one past the last entry of `entries`, the loop bound.
        while cursor < unsafe { entries.add(N) } {
            // SAFETY: `cursor` is below `entries + N`, inside the table.
            unsafe { ptr::write(cursor, previous) };
            // SAFETY: stays at most one past the end of `entries`.
            cursor = unsafe { cursor.add(1) };
            previous = previous.wrapping_add(4);
        }
    }

    /// Prepend a primitive packet into the depth-`z` slot. `packet_ptr`
    /// must point at the packet's tag word (first `u32`); `words` is
    /// the count of data words that follow the tag, at most
    /// [`crate::chain::MAX_NODE_WORDS`].
    ///
    /// # Safety
    /// Caller guarantees that `[packet_ptr .. packet_ptr + 1 + words]`
    /// is live, writable, 4-byte-aligned RAM for the duration of the
    /// OT submission. Primitives returned by the builders in
    /// [`crate::prim`] satisfy this.
    ///
    /// # Panics
    /// If `words` exceeds [`crate::chain::MAX_NODE_WORDS`]: silicon loses words
    /// from a longer node.
    pub(crate) unsafe fn link(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        assert!(
            words as usize <= crate::chain::MAX_NODE_WORDS,
            "GPU DMA node longer than MAX_NODE_WORDS"
        );
        let z = z.min(N - 1);
        // SAFETY: `z` was clamped below `N`; the packet contract is forwarded.
        unsafe { self.link_unchecked(z, packet_ptr, words) };
    }

    /// Prepend a primitive packet into an already-clamped depth slot.
    ///
    /// # Safety
    /// Same packet lifetime/alignment requirements as [`link`](Self::link).
    /// In addition, `z` must be less than `N`.
    #[inline(always)]
    pub(crate) unsafe fn link_unchecked(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        debug_assert!(words as usize <= crate::chain::MAX_NODE_WORDS);
        // SAFETY: forwarded contract; a word count below 256 fills the top byte only.
        unsafe { self.link_tag_high_unchecked(z, packet_ptr, (words as u32) << 24) };
    }

    /// Prepend a primitive whose packet-word count is already stored in the
    /// high byte of `tag_high` into an already-clamped depth slot.
    ///
    /// # Safety
    /// Same requirements as [`link_unchecked`](Self::link_unchecked).
    /// The low 24 bits of `tag_high` must be zero.
    #[inline(always)]
    pub(crate) unsafe fn link_tag_high_unchecked(
        &mut self,
        z: usize,
        packet_ptr: *mut u32,
        tag_high: u32,
    ) {
        debug_assert!(z < N);
        debug_assert_eq!(tag_high & OT_ADDR_MASK, 0);
        debug_assert!((tag_high >> 24) as usize <= crate::chain::MAX_NODE_WORDS);
        debug_assert!(is_dma_reachable(packet_ptr), "GPU DMA reads main RAM only");
        // SAFETY: the caller guarantees `z < N`.
        let entry = unsafe { self.entries.get_unchecked_mut(z) };
        let old_head = *entry & OT_ADDR_MASK;
        // SAFETY: the caller guarantees a live, writable, aligned tag word.
        unsafe { ptr::write_volatile(packet_ptr, tag_high | old_head) };
        *entry = packet_ptr.expose_provenance() as u32 & OT_ADDR_MASK;
    }

    /// Insert an array of compact raw packet commands in caller order.
    ///
    /// Each command is two machine words: a packet pointer followed by a
    /// packed word containing the OT slot in bits 0..15 and the GPU packet
    /// word count in bits 24..31. Commands are consumed first-to-last, so the
    /// OT's prepend semantics deliberately reverse commands which share a
    /// slot. This matches repeated classic `addPrim` calls exactly.
    ///
    /// Use [`Self::link_packed_commands_reverse_unchecked`] when same-slot
    /// submission order must instead be preserved.
    ///
    /// # Safety
    /// `commands` must point to `command_count * 2` readable machine words in
    /// the documented layout. Every packet pointer must meet the lifetime,
    /// alignment, and writability requirements of [`Self::link_unchecked`],
    /// every encoded slot must be less than `N`, and every word count at most
    /// [`crate::chain::MAX_NODE_WORDS`].
    #[inline]
    pub(crate) unsafe fn link_packed_commands_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        if command_count == 0 {
            return;
        }
        debug_assert!(N > 0);

        #[cfg(target_arch = "mips")]
        {
            // SAFETY: the caller guarantees `command_count * 2` readable words at `commands`.
            let command_end = unsafe { commands.add(command_count.saturating_mul(2)) };
            let entries = self.entries.as_mut_ptr();
            // SAFETY: the loop reads the caller's command words and writes each packet's tag
            // and its slot entry, all covered by this function's contract; it uses only the
            // clobbered registers.
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "2:",
                    "lw $11, 0($8)",
                    "lw $12, 4($8)",
                    "addiu $8, $8, 8",
                    "sll $14, $11, 8",
                    "andi $13, $12, 0xffff",
                    "srl $14, $14, 8",
                    "srl $12, $12, 24",
                    "sll $13, $13, 2",
                    "sll $12, $12, 24",
                    "addu $13, $10, $13",
                    "lw $9, 0($13)",
                    "nop",
                    "and $9, $9, $15",
                    "or $9, $9, $12",
                    "sw $9, 0($11)",
                    "sw $14, 0($13)",
                    "bne $8, $16, 2b",
                    "nop",
                    ".set reorder",
                    inout("$8") commands => _,
                    in("$10") entries,
                    in("$16") command_end,
                    lateout("$9") _,
                    lateout("$11") _,
                    lateout("$12") _,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    options(nostack),
                );
            }
        }

        #[cfg(not(target_arch = "mips"))]
        {
            for index in 0..command_count {
                // SAFETY: `index < command_count`, inside the caller's command array.
                let command = unsafe { commands.add(index * 2) };
                // SAFETY: the first word of each command is its packet pointer.
                let packet_ptr =
                    ptr::with_exposed_provenance_mut::<u32>(unsafe { ptr::read(command) });
                // SAFETY: the second word of each command is its slot and word count.
                let slot_words = unsafe { ptr::read(command.add(1)) } as u32;
                let slot = (slot_words & u16::MAX as u32) as usize;
                debug_assert!(slot < N);
                // SAFETY: the caller guarantees the packet and the slot meet
                // `link_tag_high_unchecked`'s contract.
                unsafe { self.link_tag_high_unchecked(slot, packet_ptr, slot_words & 0xFF00_0000) };
            }
        }
    }

    /// Insert a reverse-ordered array of compact raw packet commands.
    ///
    /// Each command is exactly two machine words: a packet pointer followed by a
    /// packed word containing the OT slot in bits 0..15 and the GPU packet
    /// word count in bits 24..31. Commands are consumed last-to-first, which
    /// preserves their original submission order despite OT insertion being
    /// prepend-only.
    ///
    /// On PS1 this is one tightly scheduled MIPS loop, matching the direct OT
    /// linking used by late commercial engines while retaining the caller's
    /// exact same-slot packet order. Host builds use the scalar equivalent so
    /// command-stream tests exercise identical semantics.
    ///
    /// # Safety
    /// `commands` must point to `command_count * 2` readable machine words in the
    /// documented layout. Every encoded packet pointer must meet the lifetime,
    /// alignment, and writability requirements of [`Self::link_unchecked`],
    /// every encoded slot must be less than `N`, and every word count at most
    /// [`crate::chain::MAX_NODE_WORDS`].
    #[inline]
    pub(crate) unsafe fn link_packed_commands_reverse_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        if command_count == 0 {
            return;
        }
        debug_assert!(N > 0);

        #[cfg(target_arch = "mips")]
        {
            // SAFETY: the caller guarantees `command_count * 2` readable words at `commands`.
            let command_end = unsafe { commands.add(command_count.saturating_mul(2)) };
            let entries = self.entries.as_mut_ptr();
            // SAFETY: the loop reads the caller's command words and writes each packet's tag
            // and its slot entry, all covered by this function's contract; it uses only the
            // clobbered registers.
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "2:",
                    "addiu $8, $8, -8",
                    "lw $11, 0($8)",
                    "lw $12, 4($8)",
                    // Fill the packet-pointer load delay while making its
                    // low-24-bit OT representation. Fill the metadata load
                    // delay before reading its slot.
                    "sll $14, $11, 8",
                    "andi $13, $12, 0xffff",
                    "srl $14, $14, 8",
                    "srl $12, $12, 24",
                    "sll $13, $13, 2",
                    "sll $12, $12, 24",
                    "addu $13, $10, $13",
                    "lw $9, 0($13)",
                    "nop",
                    "and $9, $9, $15",
                    "or $9, $9, $12",
                    "sw $9, 0($11)",
                    "sw $14, 0($13)",
                    "bne $8, $16, 2b",
                    "nop",
                    ".set reorder",
                    inout("$8") command_end => _,
                    in("$10") entries,
                    in("$16") commands,
                    lateout("$9") _,
                    lateout("$11") _,
                    lateout("$12") _,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    options(nostack),
                );
            }
        }

        #[cfg(not(target_arch = "mips"))]
        {
            let mut index = command_count;
            while index != 0 {
                index -= 1;
                // SAFETY: `index < command_count`, inside the caller's command array.
                let command = unsafe { commands.add(index * 2) };
                // SAFETY: the first word of each command is its packet pointer.
                let packet_ptr =
                    ptr::with_exposed_provenance_mut::<u32>(unsafe { ptr::read(command) });
                // SAFETY: the second word of each command is its slot and word count.
                let slot_words = unsafe { ptr::read(command.add(1)) } as u32;
                let slot = (slot_words & u16::MAX as u32) as usize;
                debug_assert!(slot < N);
                // SAFETY: the caller guarantees the packet and the slot meet
                // `link_tag_high_unchecked`'s contract.
                unsafe { self.link_tag_high_unchecked(slot, packet_ptr, slot_words & 0xFF00_0000) };
            }
        }
    }

    /// Insert a contiguous stream of classic tagged GPU packets.
    ///
    /// Before this call, each packet tag stores its GPU data-word count in
    /// bits 24..31 and its target OT slot in bits 0..15. Packets are walked
    /// from `first` to `end` and prepended in that order, exactly matching a
    /// sequence of classic `addPrim` calls. A slot value of `0xffff` skips the
    /// packet, which lets callers keep separately ordered HUD packets in the
    /// same arena.
    ///
    /// This format lets C and retained-mode renderers stage depth keys in the
    /// packet tags without a cross-language call or a separate command array
    /// per primitive. The final link pass remains owned by PSoXide.
    ///
    /// # Safety
    /// `first..end` must be a writable, contiguous sequence of complete GPU
    /// packets. Every packet's word count must describe the next packet
    /// exactly and be at most [`crate::chain::MAX_NODE_WORDS`], and every
    /// non-sentinel slot must be less than `N`.
    #[inline]
    pub(crate) unsafe fn link_tagged_packet_stream_unchecked(
        &mut self,
        first: *mut u32,
        end: *mut u32,
    ) {
        if first >= end {
            return;
        }
        debug_assert!(N > 0);

        #[cfg(target_arch = "mips")]
        {
            let entries = self.entries.as_mut_ptr();
            // Sixteen instructions per packet, two RAM loads. Every OT slot
            // already holds a 24-bit address with a zero top byte (`clear`,
            // `link_unchecked*` and this loop all store masked packet
            // addresses), so the old head needs no mask before the packet's
            // word count is OR-ed in.
            // SAFETY: the loop walks `first..end` by each packet's own word count, which the
            // caller guarantees is exact, and writes only packet tags and in-range slot
            // entries.
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    // Persistent constants: low-24-bit DMA address mask, the
                    // screen-packet sentinel staged by retained callers, and
                    // the word-count byte mask.
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "ori $17, $0, 0xffff",
                    "lui $18, 0xff00",
                    "2:",
                    "lw $9, 0($8)",
                    // The packet's 24-bit address fills the tag load delay.
                    "and $11, $8, $15",
                    // tag>>22 is the packet byte count excluding its tag;
                    // add four bytes while filling the sentinel branch slot.
                    "srl $10, $9, 22",
                    "andi $13, $9, 0xffff",
                    "addu $10, $8, $10",
                    "beq $13, $17, 3f",
                    "addiu $10, $10, 4",
                    // Prepend the packet to its already-bounded OT slot; the
                    // word-count mask fills the OT-head load delay.
                    "sll $13, $13, 2",
                    "addu $13, $12, $13",
                    "lw $14, 0($13)",
                    "and $9, $9, $18",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "sw $11, 0($13)",
                    "3:",
                    "bne $10, $16, 2b",
                    "move $8, $10",
                    ".set reorder",
                    inout("$8") first => _,
                    in("$12") entries,
                    in("$16") end,
                    lateout("$9") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    lateout("$17") _,
                    lateout("$18") _,
                    options(nostack),
                );
            }
        }

        #[cfg(not(target_arch = "mips"))]
        {
            let mut packet = first;
            while packet < end {
                // SAFETY: `packet` is below `end`, at a packet tag the caller vouches for.
                let staged_tag = unsafe { ptr::read(packet) };
                let words = (staged_tag >> 24) as usize;
                let slot = (staged_tag & u16::MAX as u32) as usize;
                // SAFETY: the tag's word count is exact, so this is the next packet or `end`.
                let next = unsafe { packet.add(words + 1) };
                if slot != u16::MAX as usize {
                    debug_assert!(slot < N);
                    // SAFETY: the caller guarantees a writable packet and an in-range slot.
                    unsafe { self.link_tag_high_unchecked(slot, packet, staged_tag & 0xFF00_0000) };
                }
                packet = next;
            }
            debug_assert_eq!(packet, end);
        }
    }

    /// Insert a tagged packet stream while quantising every non-sentinel OT
    /// slot by a compile-time right shift.
    ///
    /// This preserves the packet sequence and every raw depth calculation,
    /// but lets a caller back the final DMA chain with a smaller ordering
    /// table. A staged slot of `0xffff` remains the screen-packet sentinel and
    /// is skipped before the shift. For example, `SLOT_SHIFT = 3` maps the
    /// classic 0..2047 depth range onto 256 slots.
    ///
    /// # Safety
    ///
    /// The packet lifetime and layout requirements match
    /// [`Self::link_tagged_packet_stream_unchecked`]. Every shifted slot must
    /// be less than `N`, and `SLOT_SHIFT` must be less than 16.
    #[inline]
    pub(crate) unsafe fn link_tagged_packet_stream_shifted_unchecked<const SLOT_SHIFT: u32>(
        &mut self,
        first: *mut u32,
        end: *mut u32,
    ) {
        if first >= end {
            return;
        }
        debug_assert!(N > 0);
        debug_assert!(SLOT_SHIFT < 16);

        #[cfg(target_arch = "mips")]
        {
            let entries = self.entries.as_mut_ptr();
            // The `link_tagged_packet_stream_unchecked` loop with the slot
            // shift after the sentinel test.
            // SAFETY: the loop walks the caller's exact packet sequence and writes only tags
            // and slot entries, which `SLOT_SHIFT` keeps below `N` per the contract.
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "ori $17, $0, 0xffff",
                    "lui $18, 0xff00",
                    "2:",
                    "lw $9, 0($8)",
                    "and $11, $8, $15",
                    "srl $10, $9, 22",
                    "andi $13, $9, 0xffff",
                    "addu $10, $8, $10",
                    "beq $13, $17, 3f",
                    "addiu $10, $10, 4",
                    "srl $13, $13, {slot_shift}",
                    "sll $13, $13, 2",
                    "addu $13, $12, $13",
                    "lw $14, 0($13)",
                    "and $9, $9, $18",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "sw $11, 0($13)",
                    "3:",
                    "bne $10, $16, 2b",
                    "move $8, $10",
                    ".set reorder",
                    slot_shift = const SLOT_SHIFT,
                    inout("$8") first => _,
                    in("$12") entries,
                    in("$16") end,
                    lateout("$9") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    lateout("$17") _,
                    lateout("$18") _,
                    options(nostack),
                );
            }
        }

        #[cfg(not(target_arch = "mips"))]
        {
            let mut packet = first;
            while packet < end {
                // SAFETY: `packet` is below `end`, at a packet tag the caller vouches for.
                let staged_tag = unsafe { ptr::read(packet) };
                let words = (staged_tag >> 24) as usize;
                let slot = (staged_tag & u16::MAX as u32) as usize;
                // SAFETY: the tag's word count is exact, so this is the next packet or `end`.
                let next = unsafe { packet.add(words + 1) };
                if slot != u16::MAX as usize {
                    let shifted_slot = slot >> SLOT_SHIFT;
                    debug_assert!(shifted_slot < N);
                    // SAFETY: the caller guarantees a writable packet and an in-range shifted
                    // slot.
                    unsafe {
                        self.link_tag_high_unchecked(shifted_slot, packet, staged_tag & 0xFF00_0000)
                    };
                }
                packet = next;
            }
            debug_assert_eq!(packet, end);
        }
    }

    /// Insert a primitive struct. The struct must be `#[repr(C)]`
    /// with its first field being the tag `u32`. `words` is the
    /// number of data words that follow the tag.
    ///
    /// A struct larger than one DMA node does not build:
    ///
    /// ```compile_fail
    /// let mut ot: psx_gpu::ot::OrderingTable<4> = psx_gpu::ot::OrderingTable::new();
    /// let mut packet = [0u32; 2 + psx_gpu::MAX_NODE_WORDS];
    /// unsafe { ot.add(0, &mut packet, 17) };
    /// ```
    ///
    /// # Safety
    ///
    /// The table keeps `prim`'s address after this borrow ends: `prim` must
    /// stay live, unmoved and unmodified until every submission of this
    /// table has been waited out, and `words` must not exceed its payload.
    #[deprecated(
        note = "keeps the packet's address past the borrow and trusts `words`; use `OrderingTable::frame` and `OtFrame::add`"
    )]
    pub unsafe fn add<T>(&mut self, z: usize, prim: &mut T, words: u8) {
        const {
            assert!(
                core::mem::size_of::<T>() <= 4 * (crate::chain::MAX_NODE_WORDS + 1),
                "primitive larger than one GPU DMA node"
            )
        };
        // SAFETY: forwarded contract.
        unsafe { self.link(z, prim as *mut T as *mut u32, words) };
    }

    /// Prepend a raw packet to slot `z` (clamped to `N - 1`).
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_raw`](crate::frame::OtFrame::add_raw).
    #[deprecated(note = "use `OtFrame::add_raw`, through `frame()` or `resume_frame()`")]
    #[inline(always)]
    pub unsafe fn insert(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        // SAFETY: forwarded contract.
        unsafe { self.link(z, packet_ptr, words) }
    }

    /// Prepend a raw packet to slot `z` without the clamp or length check.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_raw_unchecked`](crate::frame::OtFrame::add_raw_unchecked).
    #[deprecated(note = "use `OtFrame::add_raw_unchecked`, through `frame()` or `resume_frame()`")]
    #[inline(always)]
    pub unsafe fn insert_unchecked(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        // SAFETY: forwarded contract.
        unsafe { self.link_unchecked(z, packet_ptr, words) }
    }

    /// Prepend a raw packet whose word count is already in tag position.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_raw_tag_high_unchecked`](crate::frame::OtFrame::add_raw_tag_high_unchecked).
    #[deprecated(
        note = "use `OtFrame::add_raw_tag_high_unchecked`, through `frame()` or `resume_frame()`"
    )]
    #[inline(always)]
    pub unsafe fn insert_unchecked_tag_high(
        &mut self,
        z: usize,
        packet_ptr: *mut u32,
        tag_high: u32,
    ) {
        // SAFETY: forwarded contract.
        unsafe { self.link_tag_high_unchecked(z, packet_ptr, tag_high) }
    }

    /// Add packed two-word commands, first to last.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_packed_commands_unchecked`](crate::frame::OtFrame::add_packed_commands_unchecked).
    #[deprecated(
        note = "use `OtFrame::add_packed_commands_unchecked`, through `frame()` or `resume_frame()`"
    )]
    #[inline(always)]
    pub unsafe fn insert_packed_commands_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        // SAFETY: forwarded contract.
        unsafe { self.link_packed_commands_unchecked(commands, command_count) }
    }

    /// Add packed two-word commands, last to first.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_packed_commands_reverse_unchecked`](crate::frame::OtFrame::add_packed_commands_reverse_unchecked).
    #[deprecated(
        note = "use `OtFrame::add_packed_commands_reverse_unchecked`, through `frame()` or `resume_frame()`"
    )]
    #[inline(always)]
    pub unsafe fn insert_packed_commands_reverse_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        // SAFETY: forwarded contract.
        unsafe { self.link_packed_commands_reverse_unchecked(commands, command_count) }
    }

    /// Add a contiguous stream of packets whose tags carry their slot.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_tagged_packet_stream_unchecked`](crate::frame::OtFrame::add_tagged_packet_stream_unchecked).
    #[deprecated(
        note = "use `OtFrame::add_tagged_packet_stream_unchecked`, through `frame()` or `resume_frame()`"
    )]
    #[inline(always)]
    pub unsafe fn insert_tagged_packet_stream_unchecked(&mut self, first: *mut u32, end: *mut u32) {
        // SAFETY: forwarded contract.
        unsafe { self.link_tagged_packet_stream_unchecked(first, end) }
    }

    /// Add a tagged packet stream with every slot shifted right.
    ///
    /// # Safety
    ///
    /// As [`OtFrame::add_tagged_packet_stream_shifted_unchecked`](crate::frame::OtFrame::add_tagged_packet_stream_shifted_unchecked).
    #[deprecated(
        note = "use `OtFrame::add_tagged_packet_stream_shifted_unchecked`, through `frame()` or `resume_frame()`"
    )]
    #[inline(always)]
    pub unsafe fn insert_tagged_packet_stream_shifted_unchecked<const SLOT_SHIFT: u32>(
        &mut self,
        first: *mut u32,
        end: *mut u32,
    ) {
        // SAFETY: forwarded contract.
        unsafe { self.link_tagged_packet_stream_shifted_unchecked::<SLOT_SHIFT>(first, end) }
    }

    /// End this table's DMA walk with GP0(1Fh), so the GPU raises
    /// [`crate::is_draw_done`] once everything in the table is drawn.
    ///
    /// Links slot 0, the last one walked, to [`crate::chain::DRAW_DONE_NODE`];
    /// packets inserted at slot 0 afterwards still draw before it. Call it
    /// after every [`clear`](Self::clear) and before anything is inserted at
    /// slot 0. Pair the submission with [`crate::arm_draw_done`].
    ///
    /// Host builds leave the table as it is: the shared node lives outside
    /// the table's address window, which [`packets`](Self::packets)
    /// relies on.
    ///
    /// # Panics
    /// If slot 0 is not empty.
    pub fn end_with_draw_done(&mut self) {
        #[cfg(target_arch = "mips")]
        // SAFETY: the shared node is immutable static RAM for the whole run.
        unsafe {
            self.end_with_node(crate::chain::DRAW_DONE_NODE.as_ptr())
        };
    }

    /// Continue this table's walk into `head`, the first node of a chain
    /// that ends the list itself: a command recording
    /// (`psx_io::gpu::begin_recording_raw`) closed on [`crate::chain::DRAW_DONE_NODE`],
    /// say, for a frame published to `psx_rt::present`.
    ///
    /// # Safety
    ///
    /// `head` must point at a 4-byte-aligned node tag in RAM, and every node
    /// reachable from it must meet [`crate::chain::submit_async_raw`]'s
    /// contract (at most [`crate::chain::MAX_NODE_WORDS`] payload words each, live
    /// and unmodified) until every walk of this table has finished.
    ///
    /// # Panics
    ///
    /// If slot 0 is not empty: call it right after [`clear`](Self::clear),
    /// before anything is inserted at slot 0.
    #[inline]
    pub unsafe fn end_with_chain(&mut self, head: *const u32) {
        // SAFETY: forwarded contract.
        unsafe { self.end_with_node(head) }
    }

    /// Link slot 0 to `node`, a node whose own link ends the list.
    ///
    /// # Safety
    /// `node` must stay live and unmodified while the table is submitted.
    pub(crate) unsafe fn end_with_node(&mut self, node: *const u32) {
        assert!(
            self.entries[0] == OT_END,
            "end_with_draw_done needs an empty slot 0: call it right after clear"
        );
        self.entries[0] = node.expose_provenance() as u32 & OT_ADDR_MASK;
    }

    /// Pointer to the slot where DMA starts (`[N-1]`). Passed to
    /// [`crate::chain::submit_raw`] as the linked-list entry point.
    #[inline]
    pub fn submit_head(&self) -> *const u32 {
        // From the whole array, so the pointer may reach every entry.
        self.entries.as_ptr().wrapping_add(N - 1)
    }

    /// Submit the whole table to GPU via DMA channel 2 linked-list
    /// mode and wait for completion.
    ///
    /// # Safety
    ///
    /// As [`crate::chain::submit_raw`] for the chain this table heads:
    /// every packet linked into it must be live and unmodified, and the
    /// table must not have moved since it was cleared.
    #[deprecated(
        note = "nothing proves the linked packets are alive; use `OrderingTable::frame` and `OtFrame::submit`"
    )]
    pub unsafe fn submit(&self) {
        // SAFETY: forwarded contract.
        unsafe { crate::chain::start_walk(self.submit_head()) };
        crate::chain::wait_walk();
    }

    /// Kick the table's DMA walk without waiting for it to finish; pair
    /// it with [`crate::chain::wait`].
    ///
    /// # Safety
    ///
    /// As [`crate::chain::submit_async_raw`]: the table and every
    /// packet it chains must stay live, unmoved and unmodified until that
    /// wait returns.
    #[deprecated(
        note = "returns mid-walk with the table still mutable; use `OtFrame::submit_with`, `FrameStorage::draw_async` or `FramePair`"
    )]
    pub unsafe fn submit_async(&self) {
        // SAFETY: forwarded contract.
        unsafe { crate::chain::start_walk(self.submit_head()) };
    }

    /// Walk the linked chain in DMA submission order, producing one
    /// `(packet_ptr, words)` pair per primitive packet.
    ///
    /// Used by the editor's host-side preview to convert an OT into a
    /// `psx-gpu-render` command log without DMAing through real
    /// hardware. The hardware DMA walker follows the same links in
    /// the same order, so the iterator yields what the GPU would consume.
    ///
    /// A link holds only the low 24 bits of an address, as on the console.
    /// A link into this table is read through the table; any other link is
    /// read as an exposed address with the table's upper address bits,
    /// which every packet in console RAM shares.
    ///
    /// # Safety
    /// Every chained packet must be live for the lifetime of the
    /// returned iterator, exactly the invariant a submission requires, and
    /// its address must have been exposed, as every insert does.
    pub unsafe fn packets(&self) -> Packets<'_> {
        let table = self.entries.as_ptr();
        Packets {
            table,
            table_low: table.addr() as u32 & OT_ADDR_MASK,
            table_bytes: N * 4,
            next: self.entries[N - 1] & OT_ADDR_MASK,
            base_high: table.addr() & !(OT_ADDR_MASK as usize),
            last_packet: OT_END,
            remaining_hops: N.saturating_add(OT_MAX_EXTRA_HOPS),
            _table: PhantomData,
        }
    }

    /// Renamed to [`Self::packets`].
    ///
    /// # Safety
    /// See [`Self::packets`].
    #[deprecated(note = "renamed to `packets`")]
    #[inline(always)]
    pub unsafe fn iter_packets(&self) -> Packets<'_> {
        // SAFETY: same contract as the renamed function.
        unsafe { self.packets() }
    }
}

/// Renamed to [`Packets`].
#[deprecated(note = "renamed to `Packets`")]
pub type OtPacketIter<'a> = Packets<'a>;

/// Walks an [`OrderingTable`]'s chain in DMA submission order.
///
/// Each `next()` returns the pointer to a packet and the number of
/// data words that follow its tag (so the full packet occupies
/// `1 + words` u32s starting at the returned pointer). The terminal
/// `0x00FFFFFF` marker stops iteration cleanly.
#[derive(Debug)]
pub struct Packets<'a> {
    /// The table's first entry, with the table's provenance.
    table: *const u32,
    /// Low 24 bits of `table`'s address, as links name it.
    table_low: u32,
    /// Size of the table in bytes.
    table_bytes: usize,
    next: u32,
    base_high: usize,
    last_packet: u32,
    remaining_hops: usize,
    _table: PhantomData<&'a [u32]>,
}

impl Packets<'_> {
    /// The word `link` names: a table entry through the table, anything
    /// else through its exposed address.
    fn resolve(&self, link: u32) -> *const u32 {
        let offset = link.wrapping_sub(self.table_low) as usize;
        if offset < self.table_bytes {
            self.table.wrapping_byte_add(offset)
        } else {
            ptr::with_exposed_provenance(self.base_high | link as usize)
        }
    }
}

impl Iterator for Packets<'_> {
    type Item = (*const u32, u8);

    fn next(&mut self) -> Option<Self::Item> {
        // Walk the chain, skipping empty stepping-stones -- OT slots
        // that hold `words=0` because they were never targeted by an
        // `insert`. The DMA hardware silently no-ops through those
        // and only forwards entries with actual packet data, so this
        // iterator presents the same view to the cmd-log adapter.
        loop {
            if self.next == OT_END {
                return None;
            }
            if self.remaining_hops == 0 || self.next == self.last_packet {
                self.next = OT_END;
                return None;
            }
            self.remaining_hops -= 1;
            let link = self.next;
            let ptr = self.resolve(link);
            // SAFETY: ptr was reached by walking the chain that
            // `packets`'s caller swore was live; tag word is
            // always present in any chained slot.
            let tag = unsafe { ptr::read_volatile(ptr) };
            let words = ((tag >> 24) & 0xFF) as u8;
            self.next = tag & OT_ADDR_MASK;
            if words > 0 {
                self.last_packet = link;
                return Some((ptr, words));
            }
        }
    }
}

/// Shows the slot count, not the `N` link words.
impl<const N: usize> core::fmt::Debug for OrderingTable<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OrderingTable")
            .field("slots", &N)
            .finish_non_exhaustive()
    }
}

impl<const N: usize> Default for OrderingTable<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(target_arch = "mips")))]
mod tests {
    use super::*;

    #[repr(C)]
    struct PackedCommand {
        /// The packet's exposed address.
        packet: usize,
        slot_words: usize,
    }

    #[test]
    fn packed_reverse_insert_preserves_submission_order_within_slots() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut a = [0u32; 2];
        let mut b = [0u32; 2];
        let mut c = [0u32; 2];
        let commands = [
            PackedCommand {
                packet: a.as_mut_ptr().expose_provenance(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: b.as_mut_ptr().expose_provenance(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: c.as_mut_ptr().expose_provenance(),
                slot_words: 2 | (1 << 24),
            },
        ];

        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link_packed_commands_reverse_unchecked(
                commands.as_ptr().cast::<usize>(),
                commands.len(),
            );
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        assert_eq!(iter.next().unwrap().0, a.as_ptr());
        assert_eq!(iter.next().unwrap().0, b.as_ptr());
        assert_eq!(iter.next().unwrap().0, c.as_ptr());
        assert!(iter.next().is_none());
    }

    #[test]
    fn packed_forward_insert_matches_repeated_prepend_semantics() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut a = [0u32; 2];
        let mut b = [0u32; 2];
        let commands = [
            PackedCommand {
                packet: a.as_mut_ptr().expose_provenance(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: b.as_mut_ptr().expose_provenance(),
                slot_words: 4 | (1 << 24),
            },
        ];

        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link_packed_commands_unchecked(commands.as_ptr().cast::<usize>(), commands.len());
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        assert_eq!(iter.next().unwrap().0, b.as_ptr());
        assert_eq!(iter.next().unwrap().0, a.as_ptr());
        assert!(iter.next().is_none());
    }

    #[test]
    fn tagged_packet_stream_matches_prepend_and_skips_sentinel_packets() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut packets = [
            (1 << 24) | 4,
            0xAAAA_AAAA,
            (2 << 24) | u16::MAX as u32,
            0xBBBB_BBBB,
            0xCCCC_CCCC,
            (1 << 24) | 4,
            0xDDDD_DDDD,
        ];

        let first = packets.as_mut_ptr();
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link_tagged_packet_stream_unchecked(first, first.add(packets.len()));
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        // SAFETY: index 5 is inside `packets`.
        assert_eq!(iter.next().unwrap().0, unsafe { packets.as_ptr().add(5) });
        assert_eq!(iter.next().unwrap().0, packets.as_ptr());
        assert!(iter.next().is_none());
        assert_eq!(packets[2] & 0x00ff_ffff, u16::MAX as u32);
    }

    #[test]
    fn shifted_tagged_stream_quantises_slots_after_the_sentinel_test() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packets = [
            (1 << 24) | 31,
            0xAAAA_AAAA,
            (1 << 24) | u16::MAX as u32,
            0xBBBB_BBBB,
            (1 << 24) | 24,
            0xCCCC_CCCC,
        ];

        // SAFETY: `packets` is one contiguous run of complete packets whose slots fit the
        // table.
        unsafe {
            {
                let first = packets.as_mut_ptr();
                ot.link_tagged_packet_stream_shifted_unchecked::<3>(
                    first,
                    first.add(packets.len()),
                );
            }
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        // SAFETY: index 4 is inside `packets`.
        assert_eq!(iter.next().unwrap().0, unsafe { packets.as_ptr().add(4) });
        assert_eq!(iter.next().unwrap().0, packets.as_ptr());
        assert!(iter.next().is_none());
        assert_eq!(packets[2] & OT_ADDR_MASK, u16::MAX as u32);
    }

    /// Build a primitive packet by hand (one tag word + N data words),
    /// insert it, and walk the chain. The iterator must report the
    /// same `(ptr, words)` pair we inserted.
    #[test]
    fn packets_walks_a_single_inserted_primitive() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        // Packet layout: [tag, w0, w1, w2] -- 3 data words after the tag.
        let mut packet: [u32; 4] = [0; 4];
        packet[1] = 0xAAAA_BBBB;
        packet[2] = 0xCCCC_DDDD;
        packet[3] = 0xEEEE_FFFF;
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link(2, packet.as_mut_ptr(), 3);
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        let entry = iter.next().expect("one entry");
        assert_eq!(entry.0 as usize, packet.as_ptr() as usize);
        assert_eq!(entry.1, 3);
        assert!(iter.next().is_none());
    }

    /// Two primitives in different slots -- chain walks both; later
    /// inserts (lower slot) come first because `clear()` chains
    /// high-to-low and the DMA head is `[N-1]`.
    #[test]
    fn packets_walks_multiple_slots_in_dma_order() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut a: [u32; 2] = [0, 0xA];
        let mut b: [u32; 2] = [0, 0xB];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            // a is in a deeper (further from camera) slot than b, so b
            // should appear first when walking from the head.
            ot.link(2, a.as_mut_ptr(), 1);
            ot.link(5, b.as_mut_ptr(), 1);
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        // DMA walker starts at [N-1] = [7] and chains down to [0].
        // b lives in slot 5, a in slot 2 -- both should appear, b first.
        let first = iter.next().expect("first entry").0 as usize;
        let second = iter.next().expect("second entry").0 as usize;
        assert!(iter.next().is_none());
        assert_eq!(first, b.as_ptr() as usize);
        assert_eq!(second, a.as_ptr() as usize);
    }

    #[test]
    fn insert_takes_a_full_fifo_of_words() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet = [0u32; 1 + crate::chain::MAX_NODE_WORDS];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe { ot.link(1, packet.as_mut_ptr(), crate::chain::MAX_NODE_WORDS as u8) };
        assert_eq!(packet[0] >> 24, crate::chain::MAX_NODE_WORDS as u32);
    }

    #[test]
    #[should_panic(expected = "longer than MAX_NODE_WORDS")]
    fn insert_refuses_a_node_longer_than_the_fifo() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet = [0u32; 2 + crate::chain::MAX_NODE_WORDS];
        // SAFETY: the call panics on the word count before it touches the packet.
        unsafe {
            ot.link(
                1,
                packet.as_mut_ptr(),
                crate::chain::MAX_NODE_WORDS as u8 + 1,
            )
        };
    }

    /// The draw-done node is the last thing walked, after every slot and
    /// after packets inserted at slot 0 once it is linked.
    #[test]
    fn end_with_node_puts_the_node_after_every_packet() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let node = [(1u32 << 24) | OT_END, 0x1F00_0000];
        let mut far: [u32; 2] = [0, 0xF];
        let mut near: [u32; 2] = [0, 0xE];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.end_with_node(node.as_ptr());
            ot.link(0, far.as_mut_ptr(), 1);
            ot.link(3, near.as_mut_ptr(), 1);
        }
        let walked: [usize; 3] = {
            // SAFETY: every packet linked into the table is a local still alive here.
            let mut iter = unsafe { ot.packets() };
            let order = [
                iter.next().expect("near").0 as usize,
                iter.next().expect("far").0 as usize,
                iter.next().expect("node").0 as usize,
            ];
            assert!(iter.next().is_none());
            order
        };
        assert_eq!(
            walked,
            [
                near.as_ptr() as usize,
                far.as_ptr() as usize,
                node.as_ptr() as usize
            ]
        );
    }

    #[test]
    #[should_panic(expected = "empty slot 0")]
    fn end_with_node_refuses_a_used_slot_0() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let node = [(1u32 << 24) | OT_END, 0x1F00_0000];
        let mut packet: [u32; 2] = [0, 0xF];
        // SAFETY: the packet and the node are locals that outlive the table's use.
        unsafe {
            ot.link(0, packet.as_mut_ptr(), 1);
            ot.end_with_node(node.as_ptr());
        }
    }

    #[test]
    fn the_shared_draw_done_node_is_one_gp0_1f_word_then_the_end() {
        // SAFETY: the node is two words of immutable static data.
        let node = unsafe { core::slice::from_raw_parts(crate::chain::DRAW_DONE_NODE.as_ptr(), 2) };
        assert_eq!(node[0] >> 24, 1);
        assert_eq!(node[0] & OT_ADDR_MASK, OT_END);
        assert_eq!(node[1], psx_hw::gpu::gp0::REQUEST_IRQ);
        assert_eq!(node[1] >> 24, 0x1F);
    }

    /// Multiple primitives in the same slot chain via the most-
    /// recently-inserted-first rule.
    #[test]
    fn packets_chains_primitives_within_one_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut first: [u32; 2] = [0, 0x1111];
        let mut second: [u32; 2] = [0, 0x2222];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link(1, first.as_mut_ptr(), 1);
            ot.link(1, second.as_mut_ptr(), 1);
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        // `second` was inserted last and prepends to the chain head;
        // it walks first.
        let head = iter.next().expect("first").0 as usize;
        let tail = iter.next().expect("second").0 as usize;
        assert!(iter.next().is_none());
        assert_eq!(head, second.as_ptr() as usize);
        assert_eq!(tail, first.as_ptr() as usize);
    }

    /// Re-inserting the same packet is an invalid OT chain because
    /// the packet only has one tag word to store its next pointer.
    /// The host iterator should fail closed instead of spinning
    /// forever while previewing a malformed frame.
    #[test]
    fn packets_stops_on_duplicate_packet_in_same_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet: [u32; 2] = [0, 0xAA00_0000];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link(1, packet.as_mut_ptr(), 1);
            ot.link(1, packet.as_mut_ptr(), 1);
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        let entry = iter.next().expect("first duplicate packet");
        assert_eq!(entry.0 as usize, packet.as_ptr() as usize);
        assert_eq!(entry.1, 1);
        assert!(iter.next().is_none());
    }

    /// A duplicate packet can also form a two-hop cycle through an
    /// empty OT slot when it is inserted into different slots. This
    /// catches the editor-preview failure mode where the cmd-log walk
    /// could peg the host thread.
    #[test]
    fn packets_stops_on_duplicate_packet_through_empty_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet: [u32; 2] = [0, 0xBB00_0000];
        // SAFETY: the packets are locals that outlive every use of the table in this test, and
        // their slots and word counts fit the table.
        unsafe {
            ot.link(1, packet.as_mut_ptr(), 1);
            ot.link(2, packet.as_mut_ptr(), 1);
        }

        // SAFETY: every packet linked into the table is a local still alive here.
        let mut iter = unsafe { ot.packets() };
        let entry = iter.next().expect("first duplicate packet");
        assert_eq!(entry.0 as usize, packet.as_ptr() as usize);
        assert_eq!(entry.1, 1);
        assert!(iter.next().is_none());
    }
}
