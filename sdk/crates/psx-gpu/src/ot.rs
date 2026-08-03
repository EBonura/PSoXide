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
//!   bits 24..=31: word count (0..=15) of this packet's data
//! ```
//!
//! An "empty OT" has every entry pointing at its predecessor,
//! ending in `0x00FFFFFF`. Submitting such an OT sends nothing to
//! GP0. As primitives are added, their packets chain in.

use core::ptr;

const OT_ADDR_MASK: u32 = 0x00FF_FFFF;
const OT_END: u32 = OT_ADDR_MASK;
const OT_MAX_EXTRA_HOPS: usize = 131_072;

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
        Self {
            entries: [0x00FF_FFFF; N],
        }
    }

    /// Reset every slot for a fresh frame. Entry `[0]` is the
    /// terminator (farthest from camera); each higher slot points
    /// to the slot below. Submission starts at `[N-1]` so the
    /// DMA walker visits `[N-1] → [N-2] → … → [0] → end`.
    pub fn clear(&mut self) {
        // CPU clear by default: the OTC DMA is one of the channels the
        // CL2 silicon probes showed can wedge busy-forever on real
        // hardware, and a wedged boot-time clear freezes the engine at
        // its first frame with no diagnostic. N stores per frame is a
        // measurable but small cost; callers that trust their DMA can
        // opt back in via [`Self::clear_via_otc_dma`].
        self.clear_software();
    }

    /// Opt-in OTC DMA clear (the pre-CL2 default). On hardware whose DMA
    /// controller wedges, this spins forever inside the DMA wait.
    #[cfg(target_arch = "mips")]
    pub fn clear_via_otc_dma(&mut self) {
        let cleared = psx_io::dma::clear_ordering_table(&mut self.entries);
        if !cleared {
            // Wedged channel or an over-large table: the CPU path always
            // produces a valid chain, so never hand back a stale one.
            self.clear_software();
        }
    }

    fn clear_software(&mut self) {
        // Slot 0 is the sentinel; chain walks stop here.
        self.entries[0] = 0x00FF_FFFF;
        for i in 1..N {
            let prev = &self.entries[i - 1] as *const u32 as u32 & 0x00FF_FFFF;
            self.entries[i] = prev;
        }
    }

    /// Prepend a primitive packet into the depth-`z` slot. `packet_ptr`
    /// must point at the packet's tag word (first `u32`); `words` is
    /// the count of data words that follow the tag (≤ 15).
    ///
    /// # Safety
    /// Caller guarantees that `[packet_ptr .. packet_ptr + 1 + words]`
    /// is live, writable, 4-byte-aligned RAM for the duration of the
    /// OT submission. Primitives returned by the builders in
    /// [`crate::prim`] satisfy this.
    pub unsafe fn insert(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        let z = z.min(N - 1);
        unsafe { self.insert_unchecked(z, packet_ptr, words) };
    }

    /// Prepend a primitive packet into an already-clamped depth slot.
    ///
    /// # Safety
    /// Same packet lifetime/alignment requirements as [`insert`](Self::insert).
    /// In addition, `z` must be less than `N`.
    #[inline(always)]
    pub unsafe fn insert_unchecked(&mut self, z: usize, packet_ptr: *mut u32, words: u8) {
        debug_assert!(z < N);
        let old_head = self.entries[z] & OT_ADDR_MASK;
        let tag = ((words as u32) << 24) | old_head;
        unsafe { ptr::write_volatile(packet_ptr, tag) };
        let pkt_addr = packet_ptr as u32 & OT_ADDR_MASK;
        self.entries[z] = pkt_addr;
    }

    /// Prepend a primitive whose packet-word count is already stored in the
    /// high byte of `tag_high` into an already-clamped depth slot.
    ///
    /// # Safety
    /// Same requirements as [`insert_unchecked`](Self::insert_unchecked).
    /// The low 24 bits of `tag_high` must be zero.
    #[inline(always)]
    pub unsafe fn insert_unchecked_tag_high(
        &mut self,
        z: usize,
        packet_ptr: *mut u32,
        tag_high: u32,
    ) {
        debug_assert!(z < N);
        debug_assert_eq!(tag_high & OT_ADDR_MASK, 0);
        let old_head = self.entries[z] & OT_ADDR_MASK;
        unsafe { ptr::write_volatile(packet_ptr, tag_high | old_head) };
        let pkt_addr = packet_ptr as u32 & OT_ADDR_MASK;
        self.entries[z] = pkt_addr;
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
    /// alignment, and writability requirements of [`Self::insert_unchecked`],
    /// and every encoded slot must be less than `N`.
    #[inline]
    pub unsafe fn insert_packed_commands_reverse_unchecked(
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
            let command_end = unsafe { commands.add(command_count.saturating_mul(2)) };
            let entries = self.entries.as_mut_ptr();
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
                let command = unsafe { commands.add(index * 2) };
                let packet_ptr = unsafe { ptr::read(command) } as *mut u32;
                let slot_words = unsafe { ptr::read(command.add(1)) } as u32;
                let slot = (slot_words & u16::MAX as u32) as usize;
                debug_assert!(slot < N);
                unsafe {
                    self.insert_unchecked_tag_high(slot, packet_ptr, slot_words & 0xFF00_0000)
                };
            }
        }
    }

    /// Insert a primitive struct. The struct must be `#[repr(C)]`
    /// with its first field being the tag `u32`. `words` is the
    /// number of data words that follow the tag.
    pub fn add<T>(&mut self, z: usize, prim: &mut T, words: u8) {
        unsafe { self.insert(z, prim as *mut T as *mut u32, words) };
    }

    /// Pointer to the slot where DMA starts (`[N-1]`). Passed to
    /// [`submit_via_dma`] as the linked-list entry point.
    #[inline]
    pub fn submit_head(&self) -> *const u32 {
        &self.entries[N - 1] as *const u32
    }

    /// Submit the whole table to GPU via DMA channel 2 linked-list
    /// mode and wait for completion. Forwards to
    /// [`crate::submit_linked_list`].
    pub fn submit(&self) {
        crate::submit_linked_list(self.submit_head());
    }

    /// Kick the table's DMA walk without waiting for it to finish.
    /// Forwards to [`crate::submit_linked_list_async`]; pair it with
    /// [`crate::submit_linked_list_wait`]. The table (and every
    /// primitive it chains) must stay live and unmodified until that
    /// wait returns.
    pub fn submit_async(&self) {
        crate::submit_linked_list_async(self.submit_head());
    }

    /// Walk the linked chain in DMA submission order, producing one
    /// `(packet_ptr, words)` pair per primitive packet.
    ///
    /// Used by the editor's host-side preview to convert an OT into a
    /// `psx-gpu-render` command log without DMAing through real
    /// hardware. The hardware DMA walker follows the same pointers in
    /// the same order, so the iterator output is bit-equivalent to
    /// what the GPU would consume.
    ///
    /// # Safety
    /// Every chained packet must be live for the lifetime of the
    /// returned iterator -- exactly the same invariant `submit()`
    /// requires. Primitives produced by [`crate::prim::*`] paired with
    /// a `PrimitiveArena` satisfy this; bespoke chains must guarantee
    /// the same.
    pub unsafe fn iter_packets(&self) -> OtPacketIter {
        // The submit head holds the address of the first chained
        // packet (its low 24 bits). PS1 hardware masks to 24 bits
        // because RAM is 2 MB and packet pointers can omit the high
        // byte. On host the same masking still recovers the address
        // because all OT-chained primitives live in the same arena
        // whose pointer fits in 24 bits relative to a stable base --
        // [`PrimitiveArena`] enforces that.
        OtPacketIter {
            next: self.entries[N - 1] & OT_ADDR_MASK,
            base_high: (self.submit_head() as usize) & !(OT_ADDR_MASK as usize),
            last_packet: OT_END,
            remaining_hops: N.saturating_add(OT_MAX_EXTRA_HOPS),
        }
    }
}

/// Walks an [`OrderingTable`]'s chain in DMA submission order.
///
/// Each `next()` returns the pointer to a packet and the number of
/// data words that follow its tag (so the full packet occupies
/// `1 + words` u32s starting at the returned pointer). The terminal
/// `0x00FFFFFF` marker stops iteration cleanly.
pub struct OtPacketIter {
    next: u32,
    base_high: usize,
    last_packet: u32,
    remaining_hops: usize,
}

impl Iterator for OtPacketIter {
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
            let ptr = (self.base_high | self.next as usize) as *const u32;
            // SAFETY: ptr was reached by walking the chain that
            // `iter_packets`'s caller swore was live; tag word is
            // always present in any chained slot.
            let tag = unsafe { ptr::read_volatile(ptr) };
            let words = ((tag >> 24) & 0xFF) as u8;
            self.next = tag & OT_ADDR_MASK;
            if words > 0 {
                self.last_packet = ptr as u32 & OT_ADDR_MASK;
                return Some((ptr, words));
            }
        }
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
        packet: *mut u32,
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
                packet: a.as_mut_ptr(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: b.as_mut_ptr(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: c.as_mut_ptr(),
                slot_words: 2 | (1 << 24),
            },
        ];

        unsafe {
            ot.insert_packed_commands_reverse_unchecked(
                commands.as_ptr().cast::<usize>(),
                commands.len(),
            );
        }

        let mut iter = unsafe { ot.iter_packets() };
        assert_eq!(iter.next().unwrap().0, a.as_ptr());
        assert_eq!(iter.next().unwrap().0, b.as_ptr());
        assert_eq!(iter.next().unwrap().0, c.as_ptr());
        assert!(iter.next().is_none());
    }

    /// Build a primitive packet by hand (one tag word + N data words),
    /// insert it, and walk the chain. The iterator must report the
    /// same `(ptr, words)` pair we inserted.
    #[test]
    fn iter_packets_walks_a_single_inserted_primitive() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        // Packet layout: [tag, w0, w1, w2] -- 3 data words after the tag.
        let mut packet: [u32; 4] = [0; 4];
        packet[1] = 0xAAAA_BBBB;
        packet[2] = 0xCCCC_DDDD;
        packet[3] = 0xEEEE_FFFF;
        unsafe {
            ot.insert(2, packet.as_mut_ptr(), 3);
        }

        let mut iter = unsafe { ot.iter_packets() };
        let entry = iter.next().expect("one entry");
        assert_eq!(entry.0 as usize, packet.as_ptr() as usize);
        assert_eq!(entry.1, 3);
        assert!(iter.next().is_none());
    }

    /// Two primitives in different slots -- chain walks both; later
    /// inserts (lower slot) come first because `clear()` chains
    /// high-to-low and the DMA head is `[N-1]`.
    #[test]
    fn iter_packets_walks_multiple_slots_in_dma_order() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut a: [u32; 2] = [0, 0xA];
        let mut b: [u32; 2] = [0, 0xB];
        unsafe {
            // a is in a deeper (further from camera) slot than b, so b
            // should appear first when walking from the head.
            ot.insert(2, a.as_mut_ptr(), 1);
            ot.insert(5, b.as_mut_ptr(), 1);
        }

        let mut iter = unsafe { ot.iter_packets() };
        // DMA walker starts at [N-1] = [7] and chains down to [0].
        // b lives in slot 5, a in slot 2 -- both should appear, b first.
        let first = iter.next().expect("first entry").0 as usize;
        let second = iter.next().expect("second entry").0 as usize;
        assert!(iter.next().is_none());
        assert_eq!(first, b.as_ptr() as usize);
        assert_eq!(second, a.as_ptr() as usize);
    }

    /// Multiple primitives in the same slot chain via the most-
    /// recently-inserted-first rule.
    #[test]
    fn iter_packets_chains_primitives_within_one_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut first: [u32; 2] = [0, 0x1111];
        let mut second: [u32; 2] = [0, 0x2222];
        unsafe {
            ot.insert(1, first.as_mut_ptr(), 1);
            ot.insert(1, second.as_mut_ptr(), 1);
        }

        let mut iter = unsafe { ot.iter_packets() };
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
    fn iter_packets_stops_on_duplicate_packet_in_same_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet: [u32; 2] = [0, 0xAA00_0000];
        unsafe {
            ot.insert(1, packet.as_mut_ptr(), 1);
            ot.insert(1, packet.as_mut_ptr(), 1);
        }

        let mut iter = unsafe { ot.iter_packets() };
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
    fn iter_packets_stops_on_duplicate_packet_through_empty_slot() {
        let mut ot: OrderingTable<4> = OrderingTable::new();
        ot.clear();
        let mut packet: [u32; 2] = [0, 0xBB00_0000];
        unsafe {
            ot.insert(1, packet.as_mut_ptr(), 1);
            ot.insert(2, packet.as_mut_ptr(), 1);
        }

        let mut iter = unsafe { ot.iter_packets() };
        let entry = iter.next().expect("first duplicate packet");
        assert_eq!(entry.0 as usize, packet.as_ptr() as usize);
        assert_eq!(entry.1, 1);
        assert!(iter.next().is_none());
    }
}
