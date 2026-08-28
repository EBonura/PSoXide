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
/// Staged-tag marker for a self-contained scoped GP0(E2) packet. Tagged-stream
/// insertion consumes this bit before replacing the low 24 bits with a DMA
/// link, so normal ordering-table submission remains wire-identical.
pub const TAG_SCOPED_TEXTURE_WINDOW: u32 = 1 << 16;
#[cfg(any(
    target_arch = "mips",
    test,
    feature = "ot-window-insert-coalescing"
))]
const GP0_TEXTURE_WINDOW_MASK: u32 = 0xFF00_0000;
#[cfg(any(
    target_arch = "mips",
    test,
    feature = "ot-window-insert-coalescing"
))]
const GP0_TEXTURE_WINDOW: u32 = 0xE200_0000;

/// Work removed by final-GPU-order scoped texture-window coalescing.
///
/// A scoped packet normally carries `E2(selector), primitive, E2(reset)`.
/// Adjacent packets with the same selector can instead carry one selector at
/// the start of the run and one reset at its end without changing primitive
/// order or GPU state at either boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScopedTextureWindowCoalesce {
    /// Scoped packets encountered in the final chain.
    pub window_packets: u32,
    /// Maximal same-selector runs among those packets.
    pub runs: u32,
    /// Run-interior selector commands made unreachable.
    pub selectors_removed: u32,
    /// Run-interior reset commands made unreachable.
    pub resets_removed: u32,
}

/// Rewrite scoped texture-window packets in one already-linked DMA chain.
///
/// `address_base` is the DMA address represented by `base`. This indirection
/// keeps the address/link algorithm host-testable while the PS1 caller maps
/// low-24-bit physical addresses through KSEG0.
///
/// # Safety
///
/// Every non-terminal link reachable from `start_address` must resolve to a
/// live, writable tag word through `base` and `address_base`. Each tag's word
/// count must describe readable packet data. The chain must not be modified
/// concurrently.
#[cfg(any(target_arch = "mips", test))]
unsafe fn coalesce_scoped_texture_window_chain(
    start_address: u32,
    base: *mut u32,
    address_base: u32,
    maximum_hops: usize,
) -> ScopedTextureWindowCoalesce {
    let mut result = ScopedTextureWindowCoalesce::default();
    let mut address = start_address & OT_ADDR_MASK;
    let mut inbound_tag: *mut u32 = ptr::null_mut();
    let mut run_selector = 0u32;
    let mut run_length = 0u32;
    let mut run_last_tag: *mut u32 = ptr::null_mut();
    let mut hops = 0usize;

    while address != OT_END && hops < maximum_hops {
        if address < address_base || (address - address_base) & 3 != 0 {
            break;
        }
        hops += 1;
        let node = unsafe { base.add(((address - address_base) >> 2) as usize) };
        let tag = unsafe { ptr::read_volatile(node) };
        let words = (tag >> 24) as usize;
        let next_address = tag & OT_ADDR_MASK;
        let selector = if words >= 3 {
            unsafe { ptr::read_volatile(node.add(1)) }
        } else {
            0
        };
        let reset = if words >= 3 {
            unsafe { ptr::read_volatile(node.add(words)) }
        } else {
            0
        };
        let scoped_window = selector & GP0_TEXTURE_WINDOW_MASK == GP0_TEXTURE_WINDOW
            && selector != GP0_TEXTURE_WINDOW
            && reset == GP0_TEXTURE_WINDOW;
        let mut linked_tag = node;

        if scoped_window {
            result.window_packets = result.window_packets.wrapping_add(1);
            if run_length != 0 && selector == run_selector && !inbound_tag.is_null() {
                let previous_tag = unsafe { ptr::read_volatile(run_last_tag) };
                let previous_words = previous_tag >> 24;
                debug_assert!(previous_words != 0);
                unsafe {
                    ptr::write_volatile(
                        run_last_tag,
                        ((previous_words - 1) << 24) | (previous_tag & OT_ADDR_MASK),
                    )
                };

                let selector_address = address.wrapping_add(4) & OT_ADDR_MASK;
                let inbound = unsafe { ptr::read_volatile(inbound_tag) };
                unsafe {
                    ptr::write_volatile(
                        inbound_tag,
                        (inbound & !OT_ADDR_MASK) | selector_address,
                    );
                    ptr::write_volatile(
                        node.add(1),
                        (((words as u32) - 1) << 24) | next_address,
                    );
                }
                linked_tag = unsafe { node.add(1) };
                run_last_tag = linked_tag;
                run_length = run_length.wrapping_add(1);
                result.selectors_removed = result.selectors_removed.wrapping_add(1);
                result.resets_removed = result.resets_removed.wrapping_add(1);
            } else {
                run_selector = selector;
                run_length = 1;
                run_last_tag = node;
                result.runs = result.runs.wrapping_add(1);
            }
        } else if words != 0 {
            run_length = 0;
            run_last_tag = ptr::null_mut();
        }

        inbound_tag = linked_tag;
        address = next_address;
    }
    result
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

    #[cfg(not(target_arch = "mips"))]
    fn clear_software(&mut self) {
        // Slot 0 is the sentinel; chain walks stop here.
        self.entries[0] = 0x00FF_FFFF;
        for i in 1..N {
            let prev = &self.entries[i - 1] as *const u32 as u32 & 0x00FF_FFFF;
            self.entries[i] = prev;
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
        unsafe { ptr::write(entries, OT_END) };
        let bulk_words = (N.saturating_sub(1) / 8) * 8;
        let mut cursor = unsafe { entries.add(1) };
        let bulk_end = unsafe { cursor.add(bulk_words) };
        let mut previous = entries as u32 & OT_ADDR_MASK;
        if bulk_words != 0 {
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
        while cursor < unsafe { entries.add(N) } {
            unsafe { ptr::write(cursor, previous) };
            cursor = unsafe { cursor.add(1) };
            previous = previous.wrapping_add(4);
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

    /// Insert an array of compact raw packet commands in caller order.
    ///
    /// Each command is two machine words: a packet pointer followed by a
    /// packed word containing the OT slot in bits 0..15 and the GPU packet
    /// word count in bits 24..31. Commands are consumed first-to-last, so the
    /// OT's prepend semantics deliberately reverse commands which share a
    /// slot. This matches repeated classic `addPrim` calls exactly.
    ///
    /// Use [`Self::insert_packed_commands_reverse_unchecked`] when same-slot
    /// submission order must instead be preserved.
    ///
    /// # Safety
    /// `commands` must point to `command_count * 2` readable machine words in
    /// the documented layout. Every packet pointer must meet the lifetime,
    /// alignment, and writability requirements of [`Self::insert_unchecked`],
    /// and every encoded slot must be less than `N`.
    #[inline]
    pub unsafe fn insert_packed_commands_unchecked(
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
    /// exactly, and every non-sentinel slot must be less than `N`.
    #[inline]
    pub unsafe fn insert_tagged_packet_stream_unchecked(&mut self, first: *mut u32, end: *mut u32) {
        if first >= end {
            return;
        }
        debug_assert!(N > 0);

        #[cfg(all(
            target_arch = "mips",
            not(feature = "ot-window-insert-coalescing")
        ))]
        {
            let entries = self.entries.as_mut_ptr();
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    // Persistent constants: low-24-bit DMA address mask and
                    // the screen-packet sentinel staged by retained callers.
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "ori $17, $0, 0xffff",
                    "2:",
                    "lw $9, 0($8)",
                    "nop",
                    // tag>>22 is the packet byte count excluding its tag;
                    // add four bytes while filling the sentinel branch slot.
                    "srl $10, $9, 22",
                    "andi $11, $9, 0xffff",
                    "addu $10, $8, $10",
                    "beq $11, $17, 3f",
                    "addiu $10, $10, 4",
                    // Prepend the packet to its already-bounded OT slot. The
                    // packet-address shifts fill the OT-head load delay.
                    "sll $13, $11, 2",
                    "addu $13, $12, $13",
                    "lw $14, 0($13)",
                    "sll $11, $8, 8",
                    "srl $9, $9, 24",
                    "and $14, $14, $15",
                    "sll $9, $9, 24",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "srl $11, $11, 8",
                    "sw $11, 0($13)",
                    "3:",
                    "move $8, $10",
                    "bne $8, $16, 2b",
                    "nop",
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
                    options(nostack),
                );
            }
        }

        #[cfg(all(target_arch = "mips", feature = "ot-window-insert-coalescing"))]
        {
            let entries = self.entries.as_mut_ptr();
            let stream_first = first;
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "ori $17, $0, 0xffff",
                    "lui $23, 0xff00",
                    "lui $25, 0x0100",
                    "2:",
                    "lw $9, 0($8)",
                    "nop",
                    "srl $10, $9, 22",
                    "andi $11, $9, 0xffff",
                    "addu $10, $8, $10",
                    "beq $11, $17, 3f",
                    "addiu $10, $10, 4",
                    "sll $13, $11, 2",
                    "addu $13, $12, $13",
                    "lw $14, 0($13)",
                    "srl $19, $9, 16",
                    "andi $19, $19, 1",
                    "beq $19, $0, 4f",
                    "and $14, $14, $15",
                    "and $20, $8, $23",
                    "or $20, $20, $14",
                    "sltu $19, $20, $18",
                    "bne $19, $0, 4f",
                    "nop",
                    "sltu $19, $20, $8",
                    "beq $19, $0, 4f",
                    "nop",
                    "lw $22, 4($20)",
                    "lw $19, 4($8)",
                    "lw $24, 0($20)",
                    "bne $22, $19, 4f",
                    "nop",
                    "srl $19, $22, 24",
                    "ori $22, $0, 0x00e2",
                    "bne $19, $22, 4f",
                    "nop",
                    // Old selector becomes the tag for its polygon/run tail;
                    // the new packet omits its reset and links to that tag.
                    "subu $24, $24, $25",
                    "sw $24, 4($20)",
                    "srl $9, $9, 24",
                    "addiu $9, $9, -1",
                    "sll $9, $9, 24",
                    "addiu $14, $14, 4",
                    "and $14, $14, $15",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "sll $11, $8, 8",
                    "srl $11, $11, 8",
                    "b 3f",
                    "sw $11, 0($13)",
                    "4:",
                    "sll $11, $8, 8",
                    "srl $9, $9, 24",
                    "sll $9, $9, 24",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "srl $11, $11, 8",
                    "sw $11, 0($13)",
                    "3:",
                    "move $8, $10",
                    "bne $8, $16, 2b",
                    "nop",
                    ".set reorder",
                    inout("$8") first => _,
                    in("$12") entries,
                    in("$16") end,
                    in("$18") stream_first,
                    lateout("$9") _,
                    lateout("$10") _,
                    lateout("$11") _,
                    lateout("$13") _,
                    lateout("$14") _,
                    lateout("$15") _,
                    lateout("$17") _,
                    lateout("$19") _,
                    lateout("$20") _,
                    lateout("$22") _,
                    lateout("$23") _,
                    lateout("$24") _,
                    lateout("$25") _,
                    options(nostack),
                );
            }
        }

        #[cfg(all(
            not(target_arch = "mips"),
            not(feature = "ot-window-insert-coalescing")
        ))]
        {
            let mut packet = first;
            while packet < end {
                let staged_tag = unsafe { ptr::read(packet) };
                let words = (staged_tag >> 24) as usize;
                let slot = (staged_tag & u16::MAX as u32) as usize;
                let next = unsafe { packet.add(words + 1) };
                if slot != u16::MAX as usize {
                    debug_assert!(slot < N);
                    unsafe {
                        self.insert_unchecked_tag_high(slot, packet, staged_tag & 0xFF00_0000)
                    };
                }
                packet = next;
            }
            debug_assert_eq!(packet, end);
        }

        #[cfg(all(
            not(target_arch = "mips"),
            feature = "ot-window-insert-coalescing"
        ))]
        {
            let stream_first = first as usize;
            let stream_end = end as usize;
            let stream_base = stream_first & !(OT_ADDR_MASK as usize);
            let mut packet = first;
            while packet < end {
                let staged_tag = unsafe { ptr::read(packet) };
                let words = (staged_tag >> 24) as usize;
                let slot = (staged_tag & u16::MAX as u32) as usize;
                let next = unsafe { packet.add(words + 1) };
                if slot != u16::MAX as usize {
                    debug_assert!(slot < N);
                    let old_address = self.entries[slot] & OT_ADDR_MASK;
                    let old = (stream_base | old_address as usize) as *mut u32;
                    let old_in_stream = (old as usize) >= stream_first
                        && (old as usize) < packet as usize
                        && (old as usize) < stream_end;
                    let marked = staged_tag & TAG_SCOPED_TEXTURE_WINDOW != 0;
                    let selector = if marked {
                        unsafe { ptr::read(packet.add(1)) }
                    } else {
                        0
                    };
                    let same_window = old_in_stream
                        && selector & GP0_TEXTURE_WINDOW_MASK == GP0_TEXTURE_WINDOW
                        && unsafe { ptr::read(old.add(1)) } == selector;
                    if same_window {
                        let old_tag = unsafe { ptr::read(old) };
                        unsafe {
                            ptr::write(
                                old.add(1),
                                old_tag.wrapping_sub(1 << 24),
                            );
                            ptr::write(
                                packet,
                                (((words as u32) - 1) << 24)
                                    | (((old as u32).wrapping_add(4)) & OT_ADDR_MASK),
                            );
                        }
                        self.entries[slot] = packet as u32 & OT_ADDR_MASK;
                    } else {
                        unsafe {
                            self.insert_unchecked_tag_high(
                                slot,
                                packet,
                                staged_tag & 0xFF00_0000,
                            )
                        };
                    }
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
    /// [`Self::insert_tagged_packet_stream_unchecked`]. Every shifted slot must
    /// be less than `N`, and `SLOT_SHIFT` must be less than 16.
    #[inline]
    pub unsafe fn insert_tagged_packet_stream_shifted_unchecked<const SLOT_SHIFT: u32>(
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
            unsafe {
                core::arch::asm!(
                    ".set noreorder",
                    "lui $15, 0x00ff",
                    "ori $15, $15, 0xffff",
                    "ori $17, $0, 0xffff",
                    "2:",
                    "lw $9, 0($8)",
                    "nop",
                    "srl $10, $9, 22",
                    "andi $11, $9, 0xffff",
                    "addu $10, $8, $10",
                    "beq $11, $17, 3f",
                    "addiu $10, $10, 4",
                    "srl $11, $11, {slot_shift}",
                    "sll $13, $11, 2",
                    "addu $13, $12, $13",
                    "lw $14, 0($13)",
                    "sll $11, $8, 8",
                    "srl $9, $9, 24",
                    "and $14, $14, $15",
                    "sll $9, $9, 24",
                    "or $14, $14, $9",
                    "sw $14, 0($8)",
                    "srl $11, $11, 8",
                    "sw $11, 0($13)",
                    "3:",
                    "move $8, $10",
                    "bne $8, $16, 2b",
                    "nop",
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
                    options(nostack),
                );
            }
        }

        #[cfg(not(target_arch = "mips"))]
        {
            let mut packet = first;
            while packet < end {
                let staged_tag = unsafe { ptr::read(packet) };
                let words = (staged_tag >> 24) as usize;
                let slot = (staged_tag & u16::MAX as u32) as usize;
                let next = unsafe { packet.add(words + 1) };
                if slot != u16::MAX as usize {
                    let shifted_slot = slot >> SLOT_SHIFT;
                    debug_assert!(shifted_slot < N);
                    unsafe {
                        self.insert_unchecked_tag_high(
                            shifted_slot,
                            packet,
                            staged_tag & 0xFF00_0000,
                        )
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
    pub fn add<T>(&mut self, z: usize, prim: &mut T, words: u8) {
        unsafe { self.insert(z, prim as *mut T as *mut u32, words) };
    }

    /// Pointer to the slot where DMA starts (`[N-1]`). Passed to
    /// [`submit_via_dma`] as the linked-list entry point.
    #[inline]
    pub fn submit_head(&self) -> *const u32 {
        &self.entries[N - 1] as *const u32
    }

    /// Coalesce adjacent scoped texture-window packets in final GPU order.
    ///
    /// Empty OT nodes are transparent, so packets in neighbouring depth slots
    /// can share state when no intervening GP0 command observes the window.
    /// Only redundant run-interior selectors and resets are made unreachable;
    /// primitive bytes, order, run-entry state, and run-exit state are kept.
    ///
    /// # Safety
    ///
    /// Every packet linked into this table must remain live and writable until
    /// GPU completion, and the table must not yet have been submitted.
    pub unsafe fn coalesce_scoped_texture_windows(&mut self) -> ScopedTextureWindowCoalesce {
        #[cfg(target_arch = "mips")]
        {
            let start = self.submit_head() as u32 & OT_ADDR_MASK;
            unsafe {
                coalesce_scoped_texture_window_chain(
                    start,
                    0x8000_0000usize as *mut u32,
                    0,
                    N.saturating_add(OT_MAX_EXTRA_HOPS),
                )
            }
        }
        #[cfg(not(target_arch = "mips"))]
        {
            ScopedTextureWindowCoalesce::default()
        }
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

    #[test]
    fn packed_forward_insert_matches_repeated_prepend_semantics() {
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        let mut a = [0u32; 2];
        let mut b = [0u32; 2];
        let commands = [
            PackedCommand {
                packet: a.as_mut_ptr(),
                slot_words: 4 | (1 << 24),
            },
            PackedCommand {
                packet: b.as_mut_ptr(),
                slot_words: 4 | (1 << 24),
            },
        ];

        unsafe {
            ot.insert_packed_commands_unchecked(commands.as_ptr().cast::<usize>(), commands.len());
        }

        let mut iter = unsafe { ot.iter_packets() };
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

        unsafe {
            ot.insert_tagged_packet_stream_unchecked(
                packets.as_mut_ptr(),
                packets.as_mut_ptr().add(packets.len()),
            );
        }

        let mut iter = unsafe { ot.iter_packets() };
        assert_eq!(iter.next().unwrap().0, unsafe { packets.as_ptr().add(5) });
        assert_eq!(iter.next().unwrap().0, packets.as_ptr());
        assert!(iter.next().is_none());
        assert_eq!(packets[2] & 0x00ff_ffff, u16::MAX as u32);
    }

    #[cfg(feature = "ot-window-insert-coalescing")]
    #[test]
    fn tagged_insert_coalesces_same_slot_scoped_window_packets() {
        const WINDOW: u32 = 0xe200_0123;
        const RESET: u32 = 0xe200_0000;
        let mut packets = [
            (4 << 24) | TAG_SCOPED_TEXTURE_WINDOW | 3,
            WINDOW,
            0x3400_0001,
            0x0002_0003,
            RESET,
            (4 << 24) | TAG_SCOPED_TEXTURE_WINDOW | 3,
            WINDOW,
            0x3400_0004,
            0x0005_0006,
            RESET,
        ];
        let mut ot: OrderingTable<8> = OrderingTable::new();
        ot.clear();
        unsafe {
            ot.insert_tagged_packet_stream_unchecked(
                packets.as_mut_ptr(),
                packets.as_mut_ptr().add(packets.len()),
            );
        }

        assert_eq!(packets[5] >> 24, 3, "new head omits its reset");
        assert_eq!(packets[5] & OT_ADDR_MASK, (&packets[1] as *const u32 as u32) & OT_ADDR_MASK);
        assert_eq!(packets[1] >> 24, 3, "old selector becomes a tail tag");
        assert_eq!(packets[2], 0x3400_0001);
        assert_eq!(packets[4], RESET, "oldest packet retains the run reset");
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

        unsafe {
            ot.insert_tagged_packet_stream_shifted_unchecked::<3>(
                packets.as_mut_ptr(),
                packets.as_mut_ptr().add(packets.len()),
            );
        }

        let mut iter = unsafe { ot.iter_packets() };
        assert_eq!(iter.next().unwrap().0, unsafe { packets.as_ptr().add(4) });
        assert_eq!(iter.next().unwrap().0, packets.as_ptr());
        assert!(iter.next().is_none());
        assert_eq!(packets[2] & OT_ADDR_MASK, u16::MAX as u32);
    }

    #[test]
    fn scoped_window_runs_keep_only_boundary_state_commands() {
        let mut chain = [0u32; 15];
        let address = |word: usize| (word * 4) as u32;
        chain[0] = address(1);
        chain[1] = (4 << 24) | address(6);
        chain[2] = 0xE234_0012;
        chain[3] = 0x3400_0000;
        chain[4] = 0x1111_1111;
        chain[5] = GP0_TEXTURE_WINDOW;
        chain[6] = address(7);
        chain[7] = (4 << 24) | address(12);
        chain[8] = 0xE234_0012;
        chain[9] = 0x3400_0000;
        chain[10] = 0x2222_2222;
        chain[11] = GP0_TEXTURE_WINDOW;
        chain[12] = (1 << 24) | OT_END;
        chain[13] = 0x2000_0000;

        let result = unsafe {
            coalesce_scoped_texture_window_chain(
                address(0),
                chain.as_mut_ptr(),
                0,
                chain.len(),
            )
        };
        assert_eq!(
            result,
            ScopedTextureWindowCoalesce {
                window_packets: 2,
                runs: 1,
                selectors_removed: 1,
                resets_removed: 1,
            }
        );
        assert_eq!(chain[1] >> 24, 3);
        assert_eq!(chain[6] & OT_ADDR_MASK, address(8));
        assert_eq!(chain[8] >> 24, 3);
        assert_eq!(chain[8] & OT_ADDR_MASK, address(12));
        assert_eq!(chain[11], GP0_TEXTURE_WINDOW);
    }

    #[test]
    fn scoped_window_coalescing_stops_at_plain_gpu_work() {
        let mut chain = [0u32; 16];
        let address = |word: usize| (word * 4) as u32;
        chain[0] = address(1);
        chain[1] = (3 << 24) | address(5);
        chain[2] = 0xE200_1234;
        chain[3] = 0x3400_0000;
        chain[4] = GP0_TEXTURE_WINDOW;
        chain[5] = (1 << 24) | address(7);
        chain[6] = 0x2000_0000;
        chain[7] = (3 << 24) | OT_END;
        chain[8] = 0xE200_1234;
        chain[9] = 0x3400_0000;
        chain[10] = GP0_TEXTURE_WINDOW;

        let result = unsafe {
            coalesce_scoped_texture_window_chain(
                address(0),
                chain.as_mut_ptr(),
                0,
                chain.len(),
            )
        };
        assert_eq!(result.window_packets, 2);
        assert_eq!(result.runs, 2);
        assert_eq!(result.selectors_removed, 0);
        assert_eq!(result.resets_removed, 0);
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
