//! Render helpers built around PS1 ordering tables.
//!
//! The SDK-level [`psx_gpu::ot::OrderingTable`] is intentionally a
//! thin hardware wrapper. This module adds the engine-facing shape:
//! begin a frame, add primitives at depth slots, submit once, and use
//! fixed backing arenas for primitive packets without depending on an
//! allocator.

use psx_gpu::{
    ot::OrderingTable,
    prim::{
        LineMono, QuadFlat, QuadGouraud, QuadTextured, QuadTexturedGouraud, QuadTexturedMaterial,
        RectFlat, Sprite, TriFlat, TriGouraud, TriTextured, TriTexturedGouraud,
    },
};
use psx_math::int32::InvariantDivisor31;

/// GPU primitive packet that can be inserted into an ordering table.
///
/// The associated `WORDS` value is the number of data words following
/// the packet tag. SDK primitive structs expose this as an inherent
/// constant; this trait lets engine render helpers use it without
/// every call site repeating the constant manually.
pub trait GpuPacket {
    /// Number of data words after the tag word.
    const WORDS: u8;
}

macro_rules! impl_gpu_packet {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl GpuPacket for $ty {
                const WORDS: u8 = <$ty>::WORDS;
            }
        )+
    };
}

impl_gpu_packet!(
    TriFlat,
    TriGouraud,
    QuadFlat,
    RectFlat,
    QuadGouraud,
    LineMono,
    TriTextured,
    TriTexturedGouraud,
    QuadTextured,
    QuadTexturedGouraud,
    QuadTexturedMaterial,
    Sprite,
);

/// Camera-space depth used for ordering-table mapping.
///
/// This is the post-projection-space `z` scalar used by renderer
/// passes to choose an OT slot. It is intentionally separate from raw
/// world Z coordinates: a higher camera depth is farther from the
/// camera and should map toward the back of the ordering table.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CameraDepth {
    raw: i32,
}

impl CameraDepth {
    /// Zero depth.
    pub const ZERO: Self = Self { raw: 0 };

    /// Build from a raw camera-space depth.
    pub const fn new(raw: i32) -> Self {
        Self { raw }
    }

    /// Raw camera-space depth.
    pub const fn raw(self) -> i32 {
        self.raw
    }

    /// Add a signed bias with saturation.
    pub const fn saturating_add(self, bias: i32) -> Self {
        Self::new(self.raw.saturating_add(bias))
    }
}

/// Type-level ordering-table depth helper.
///
/// `OtDepth<N>` names the number of slots carried by an
/// [`OrderingTable<N>`] / [`OtFrame<N>`], so constants can request
/// common bands without repeating `OT_DEPTH - 1` by hand.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct OtDepth<const DEPTH: usize>;

impl<const DEPTH: usize> OtDepth<DEPTH> {
    /// Number of OT slots.
    pub const SLOT_COUNT: usize = DEPTH;

    /// Nearest/front slot.
    pub const FRONT_SLOT: DepthSlot = DepthSlot::new(0);

    /// Farthest/back slot, clamped for zero-depth tables.
    pub const BACK_SLOT: DepthSlot = if DEPTH == 0 {
        DepthSlot::new(0)
    } else {
        DepthSlot::new(DEPTH - 1)
    };

    /// Whole-table band.
    pub const fn whole_band() -> DepthBand {
        DepthBand::new(Self::FRONT_SLOT.index(), Self::BACK_SLOT.index())
    }

    /// Build an inclusive band clamped to this table depth.
    pub const fn band(front: usize, back: usize) -> DepthBand {
        let max_slot = Self::BACK_SLOT.index();
        let front = if front > max_slot { max_slot } else { front };
        let back = if back > max_slot { max_slot } else { back };
        DepthBand::new(front, back)
    }
}

/// A clamped ordering-table slot.
///
/// Higher slot indices are farther from the camera and are submitted
/// earlier by the PS1 linked-list DMA walk. Lower slots draw later
/// and therefore appear in front.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DepthSlot {
    index: usize,
}

impl DepthSlot {
    /// Build a depth slot from a raw index.
    ///
    /// The value is clamped by [`OtFrame::add_slot`] against the
    /// actual ordering-table depth.
    pub const fn new(index: usize) -> Self {
        Self { index }
    }

    /// Raw slot index.
    pub const fn index(self) -> usize {
        self.index
    }
}

/// Linear mapping from camera-space depth to OT slots.
///
/// `near` maps to slot `0` (front) and `far` maps to the last OT slot
/// (back). Depths outside the range clamp.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DepthRange {
    near: CameraDepth,
    far: CameraDepth,
    /// `1 / (far - near)` prepared here, once, so the per-primitive slot
    /// mapping is a `multu` rather than a `div` (see [`Self::span_divisor`]).
    span_divisor: InvariantDivisor31,
}

impl DepthRange {
    /// Create a range where `near` is front and `far` is back.
    ///
    /// If `far <= near`, every value maps to slot `0`; this avoids
    /// division by zero and makes invalid ranges fail visually
    /// conservative.
    pub const fn new(near: i32, far: i32) -> Self {
        Self::from_depths(CameraDepth::new(near), CameraDepth::new(far))
    }

    /// Create from typed camera-space depths.
    pub const fn from_depths(near: CameraDepth, far: CameraDepth) -> Self {
        let span = if far.raw() > near.raw() {
            far.raw() - near.raw()
        } else {
            // Degenerate ranges answer the front slot before dividing.
            1
        };
        Self {
            near,
            far,
            span_divisor: InvariantDivisor31::new(span as u32),
        }
    }

    /// Exact `x / (far - near)` for `x < 2^31`, prepared when the range was
    /// built. For a degenerate range (`far <= near`) it divides by one.
    pub const fn span_divisor(self) -> InvariantDivisor31 {
        self.span_divisor
    }

    /// Front depth.
    pub const fn near(self) -> i32 {
        self.near.raw()
    }

    /// Back depth.
    pub const fn far(self) -> i32 {
        self.far.raw()
    }

    /// Typed front depth.
    pub const fn near_depth(self) -> CameraDepth {
        self.near
    }

    /// Typed back depth.
    pub const fn far_depth(self) -> CameraDepth {
        self.far
    }

    /// Map `depth` into an OT slot for a table with `DEPTH` slots.
    pub fn slot<const DEPTH: usize>(self, depth: i32) -> DepthSlot {
        self.slot_depth::<DEPTH>(CameraDepth::new(depth))
    }

    /// Map typed `depth` into an OT slot for a table with `DEPTH` slots.
    pub fn slot_depth<const DEPTH: usize>(self, depth: CameraDepth) -> DepthSlot {
        DepthBand::whole().slot_depth::<DEPTH>(self, depth)
    }
}

/// Inclusive subset of an ordering table reserved for one render layer.
///
/// Engines often reserve the farthest slot for backgrounds and the
/// nearest slots for overlays/effects. A band lets a scene map
/// camera-space depth into only the slots allocated to world
/// geometry, keeping those layers from fighting each other.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DepthBand {
    front: usize,
    back: usize,
}

impl DepthBand {
    /// Build an inclusive OT slot band.
    ///
    /// `front` is the nearest slot in the band; `back` is the
    /// farthest. Values are clamped by [`slot`](Self::slot) against
    /// the actual ordering-table depth.
    pub const fn new(front: usize, back: usize) -> Self {
        Self { front, back }
    }

    /// The whole ordering table.
    pub const fn whole() -> Self {
        Self {
            front: 0,
            back: usize::MAX,
        }
    }

    /// Nearest slot requested by this band.
    pub const fn front(self) -> usize {
        self.front
    }

    /// Farthest slot requested by this band.
    pub const fn back(self) -> usize {
        self.back
    }

    /// Map `depth` through `range` into this inclusive band.
    pub fn slot<const DEPTH: usize>(self, range: DepthRange, depth: i32) -> DepthSlot {
        self.slot_depth::<DEPTH>(range, CameraDepth::new(depth))
    }

    /// Map typed `depth` through `range` into this inclusive band.
    pub fn slot_depth<const DEPTH: usize>(
        self,
        range: DepthRange,
        depth: CameraDepth,
    ) -> DepthSlot {
        if DEPTH == 0 {
            return DepthSlot::new(0);
        }

        let max_slot = DEPTH - 1;
        let front = self.front.min(max_slot);
        let back = self.back.min(max_slot);
        let near = range.near.raw();
        let far = range.far.raw();
        let depth = depth.raw();
        if back <= front || far <= near || depth <= near {
            return DepthSlot::new(front);
        }
        if depth >= far {
            return DepthSlot::new(back);
        }

        // `far > near` here, so the range's prepared divisor is exactly
        // `1 / (far - near)`; the positive saturating product stays below
        // 2^31, its exact domain.
        let offset = depth - near;
        let band_slots = (back - front) as i32;
        let scaled = offset.saturating_mul(band_slots) as u32;
        DepthSlot::new(front + range.span_divisor.divide(scaled) as usize)
    }
}

/// One frame's mutable view of an ordering table.
///
/// Constructing this with [`begin`](Self::begin) clears the table for
/// the current frame. Calling [`submit`](Self::submit) consumes the
/// frame view, which keeps call sites honest: all inserts happen
/// before the DMA submission.
#[must_use = "call submit() to send the ordering table to the GPU"]
pub struct OtFrame<'a, const DEPTH: usize> {
    ot: &'a mut OrderingTable<DEPTH>,
}

impl<'a, const DEPTH: usize> OtFrame<'a, DEPTH> {
    /// Clear `ot` and begin a new frame.
    ///
    /// `DEPTH` must be greater than zero; the underlying SDK
    /// ordering table has the same requirement.
    pub fn begin(ot: &'a mut OrderingTable<DEPTH>) -> Self {
        debug_assert!(DEPTH > 0);
        ot.clear();
        Self { ot }
    }

    /// Continue inserting into an already-started ordering table.
    ///
    /// This is for bridge code that still owns legacy OT emission but
    /// wants an engine render pass to append packets into that same
    /// frame. Callers are responsible for clearing `ot` before the
    /// first packet of the frame.
    pub fn resume(ot: &'a mut OrderingTable<DEPTH>) -> Self {
        debug_assert!(DEPTH > 0);
        Self { ot }
    }

    /// Insert a primitive at a raw OT slot.
    pub fn add<T>(&mut self, slot: usize, prim: &mut T, words: u8) {
        debug_assert!(words <= 15);
        self.ot.add(slot, prim, words);
    }

    /// Insert a raw primitive packet pointer at a raw OT slot.
    ///
    /// # Safety
    /// `packet_ptr` must point at the first word of a live GPU packet
    /// that remains writable until the ordering-table DMA has consumed
    /// it. `words` is the number of data words following the tag word.
    pub unsafe fn add_raw(&mut self, slot: usize, packet_ptr: *mut u32, words: u8) {
        debug_assert!(words <= 15);
        unsafe { self.ot.insert(slot, packet_ptr, words) };
    }

    /// Insert a primitive at a typed OT slot.
    pub fn add_slot<T>(&mut self, slot: DepthSlot, prim: &mut T, words: u8) {
        self.add(slot.index(), prim, words);
    }

    /// Insert a raw primitive packet pointer at a typed OT slot.
    ///
    /// # Safety
    /// Same requirements as [`add_raw`](Self::add_raw).
    pub unsafe fn add_raw_slot(&mut self, slot: DepthSlot, packet_ptr: *mut u32, words: u8) {
        unsafe { self.add_raw(slot.index(), packet_ptr, words) };
    }

    /// Insert a raw primitive packet pointer at an already-clamped raw OT slot.
    ///
    /// # Safety
    /// Same requirements as [`add_raw`](Self::add_raw). In addition, `slot`
    /// must be less than `DEPTH`.
    #[inline(always)]
    pub unsafe fn add_raw_unchecked(&mut self, slot: usize, packet_ptr: *mut u32, words: u8) {
        debug_assert!(words <= 15);
        unsafe { self.ot.insert_unchecked(slot, packet_ptr, words) };
    }

    /// Insert a raw primitive whose packet length is already in GPU-tag form
    /// at an already-clamped raw OT slot.
    ///
    /// # Safety
    /// Same requirements as [`add_raw_unchecked`](Self::add_raw_unchecked).
    /// The low 24 bits of `tag_high` must be zero.
    #[inline(always)]
    pub unsafe fn add_raw_tag_unchecked(
        &mut self,
        slot: usize,
        packet_ptr: *mut u32,
        tag_high: u32,
    ) {
        unsafe {
            self.ot
                .insert_unchecked_tag_high(slot, packet_ptr, tag_high)
        };
    }

    /// Insert compact raw packet commands in reverse array order.
    ///
    /// See [`OrderingTable::insert_packed_commands_reverse_unchecked`] for the
    /// two-word command layout.
    ///
    /// # Safety
    ///
    /// `commands` must point at `command_count * 2` readable words laid out
    /// as that method documents, and the ordering table must have room for
    /// them: neither is checked here.
    #[inline(always)]
    pub unsafe fn add_packed_commands_reverse_unchecked(
        &mut self,
        commands: *const usize,
        command_count: usize,
    ) {
        unsafe {
            self.ot
                .insert_packed_commands_reverse_unchecked(commands, command_count)
        };
    }

    /// Insert a contiguous stream of classic packets whose temporary tag
    /// carries its target ordering-table slot.
    ///
    /// # Safety
    ///
    /// `first..end` must be a writable sequence of complete tagged packets,
    /// and every non-sentinel slot encoded in that sequence must be less than
    /// `DEPTH`. The packet storage must remain live and unmodified until the
    /// submitted ordering-table DMA has completed.
    #[inline(always)]
    pub unsafe fn add_tagged_packet_stream_unchecked(&mut self, first: *mut u32, end: *mut u32) {
        unsafe { self.ot.insert_tagged_packet_stream_unchecked(first, end) };
    }

    /// Insert a tagged stream committed from the shared primitive packet
    /// arena.
    ///
    /// Unlike [`add_tagged_packet_stream_unchecked`](Self::add_tagged_packet_stream_unchecked),
    /// this form carries the exact committed range and cannot accidentally
    /// link unused reservation capacity into the ordering table.
    ///
    /// # Safety
    ///
    /// The arena storage represented by `stream` must remain live and
    /// unmodified until the ordering-table DMA has completed. Every
    /// non-sentinel slot encoded in the stream must be less than `DEPTH`.
    #[inline(always)]
    pub unsafe fn add_committed_tagged_packet_stream_unchecked(
        &mut self,
        stream: PrimitivePacketStream,
    ) {
        if stream.is_empty() {
            return;
        }
        unsafe {
            self.ot
                .insert_tagged_packet_stream_unchecked(stream.first, stream.end)
        };
    }

    /// Insert a known SDK GPU packet at a raw OT slot.
    pub fn add_packet<T: GpuPacket>(&mut self, slot: usize, prim: &mut T) {
        self.add(slot, prim, T::WORDS);
    }

    /// Insert a known SDK GPU packet at a typed OT slot.
    pub fn add_packet_slot<T: GpuPacket>(&mut self, slot: DepthSlot, prim: &mut T) {
        self.add_slot(slot, prim, T::WORDS);
    }

    /// Map camera-space `depth` through `range` and insert the
    /// primitive into the resulting OT slot.
    pub fn add_depth<T>(&mut self, range: DepthRange, depth: i32, prim: &mut T, words: u8) {
        self.add_camera_depth(range, CameraDepth::new(depth), prim, words);
    }

    /// Map typed camera-space `depth` through `range` and insert the
    /// primitive into the resulting OT slot.
    pub fn add_camera_depth<T>(
        &mut self,
        range: DepthRange,
        depth: CameraDepth,
        prim: &mut T,
        words: u8,
    ) {
        self.add_slot(range.slot_depth::<DEPTH>(depth), prim, words);
    }

    /// Map camera-space `depth` through `range` and insert a known
    /// SDK GPU packet into the resulting OT slot.
    pub fn add_packet_depth<T: GpuPacket>(&mut self, range: DepthRange, depth: i32, prim: &mut T) {
        self.add_packet_camera_depth(range, CameraDepth::new(depth), prim);
    }

    /// Map typed camera-space `depth` through `range` and insert a
    /// known SDK GPU packet into the resulting OT slot.
    pub fn add_packet_camera_depth<T: GpuPacket>(
        &mut self,
        range: DepthRange,
        depth: CameraDepth,
        prim: &mut T,
    ) {
        self.add_packet_slot(range.slot_depth::<DEPTH>(depth), prim);
    }

    /// Submit this frame's ordering table via DMA linked-list mode.
    pub fn submit(self) {
        self.ot.submit();
    }

    /// Kick this frame's ordering-table DMA without blocking on the GPU
    /// draw, returning an [`OtSubmitInFlight`] guard that must be waited
    /// on before the table is reused.
    ///
    /// The ordering table and every primitive it chains must stay live
    /// and unmodified until [`OtSubmitInFlight::wait`] returns. This is
    /// the seam profiling code uses to time the GPU-draw wait separately
    /// from the CPU-side build + kick, and the seam a future render
    /// pipeline uses to overlap the GPU draw with CPU work.
    pub fn submit_async(self) -> OtSubmitInFlight {
        self.ot.submit_async();
        OtSubmitInFlight(())
    }
}

/// In-flight ordering-table DMA kicked by [`OtFrame::submit_async`].
///
/// It carries no borrow yet: the caller must keep the ordering table and
/// primitive storage alive until [`wait`](Self::wait). The private unit field
/// keeps it constructible only here. `#[must_use]` flags a *dropped* guard (a
/// kicked DMA whose handle is discarded); it does not by itself enforce that
/// `wait()` runs before the table is reused -- today the sole caller waits
/// immediately, and a future overlap pipeline will tie the borrow in.
#[must_use = "call wait() for the GPU DMA before reusing the ordering table"]
pub struct OtSubmitInFlight(());

impl OtSubmitInFlight {
    /// Block until the kicked ordering-table DMA walk has finished.
    pub fn wait(self) {
        psx_gpu::submit_linked_list_wait();
    }

    /// Hand completion to the app runner's presentation flip.
    ///
    /// The runner drains the channel (and the GPU queue) before calling
    /// [`Scene::render_overlay`](crate::Scene::render_overlay) and
    /// flipping the display, so a scene that kicks at the end of
    /// [`Scene::render`](crate::Scene::render) can detach instead of
    /// blocking. The ordering table and primitive storage stay borrowed
    /// by the walker until that flip; the scene must not touch them or
    /// issue immediate GP0 draws in between.
    pub fn detach(self) {}
}

/// Fixed backing storage for primitive packets.
///
/// PS1 render packets must live in RAM until the OT DMA walk has
/// consumed them. This arena wraps caller-owned storage and writes
/// primitives sequentially each frame. Call [`reset`](Self::reset)
/// before reusing it for a new frame.
pub struct PrimitiveArena<'a, T> {
    storage: &'a mut [T],
    len: usize,
}

/// Minimal typed primitive sink used by render paths.
///
/// Some games can afford one arena per packet type, while PS1-sized
/// scenes often need one shared packet scratch buffer for mixed flat
/// and Gouraud packets. This trait keeps render code independent of
/// that storage choice.
pub trait PrimitiveSink<T> {
    /// Number of primitives written through this sink.
    fn len(&self) -> usize;

    /// Backing primitive capacity when it is statically known.
    fn capacity(&self) -> usize;

    /// Write `prim` and return a stable mutable reference suitable
    /// for ordering-table insertion.
    fn push(&mut self, prim: T) -> Option<&mut T>;

    /// Write without checking capacity.
    ///
    /// # Safety
    ///
    /// The caller must prove [`Self::remaining`] is at least one before
    /// every call and must not retain another reference into this sink.
    unsafe fn push_unchecked(&mut self, prim: T) -> &mut T {
        // SAFETY: delegated to the caller's capacity proof.
        unsafe { self.push(prim).unwrap_unchecked() }
    }

    /// True if no primitives have been written.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remaining primitive slots or packet-sized equivalents.
    fn remaining(&self) -> usize {
        self.capacity().saturating_sub(self.len())
    }
}

/// A sink that accepts both packet shapes a room surface can emit: the
/// per-triangle [`TriTexturedGouraud`] (subdivided / triangle-depth
/// surfaces) and the whole-surface [`QuadTexturedGouraud`]
/// (prepared-depth quads). Implemented for any sink that takes both, so
/// the room draw chain can carry one parameter instead of two.
pub trait RoomSurfaceSink:
    PrimitiveSink<TriTexturedGouraud> + PrimitiveSink<QuadTexturedGouraud>
{
}

impl<T> RoomSurfaceSink for T where
    T: PrimitiveSink<TriTexturedGouraud> + PrimitiveSink<QuadTexturedGouraud>
{
}

impl<'a, T> PrimitiveArena<'a, T> {
    /// Wrap caller-owned primitive storage.
    pub fn new(storage: &'a mut [T]) -> Self {
        Self { storage, len: 0 }
    }

    /// Clear the arena for reuse. Existing values are left in memory
    /// and will be overwritten by subsequent [`push`](Self::push)
    /// calls.
    pub fn reset(&mut self) {
        self.len = 0;
    }

    /// Number of primitives written this frame.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True if no primitives have been written this frame.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Backing storage capacity.
    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// Remaining primitive slots.
    pub fn remaining(&self) -> usize {
        self.capacity().saturating_sub(self.len)
    }

    /// Write `prim` and return a mutable reference suitable for OT
    /// insertion. Returns `None` if the arena is full.
    pub fn push(&mut self, prim: T) -> Option<&mut T> {
        let idx = self.push_index(prim)?;
        Some(&mut self.storage[idx])
    }

    /// Write `prim` and return its arena index.
    ///
    /// This is useful for render passes that need to build packets
    /// first, sort draw commands, and only then borrow the packets
    /// for ordering-table insertion.
    pub fn push_index(&mut self, prim: T) -> Option<usize> {
        if self.len >= self.storage.len() {
            return None;
        }
        let idx = self.len;
        self.len += 1;
        self.storage[idx] = prim;
        Some(idx)
    }

    /// Borrow a primitive previously written by [`push_index`](Self::push_index).
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        Some(&mut self.storage[index])
    }
}

impl<T> PrimitiveSink<T> for PrimitiveArena<'_, T> {
    fn len(&self) -> usize {
        self.len
    }

    fn capacity(&self) -> usize {
        self.storage.len()
    }

    fn push(&mut self, prim: T) -> Option<&mut T> {
        PrimitiveArena::push(self, prim)
    }

    unsafe fn push_unchecked(&mut self, prim: T) -> &mut T {
        let index = self.len;
        self.len += 1;
        // SAFETY: the trait contract requires the caller to preflight capacity.
        let slot = unsafe { self.storage.get_unchecked_mut(index) };
        *slot = prim;
        slot
    }
}

/// Backing words in one mixed primitive-arena slot.
///
/// Retained raw packet producers use this to convert a tightly packed stream
/// into the same capacity unit used by [`PrimitivePacketScratch`].
pub const PRIMITIVE_PACKET_SLOT_WORDS: usize = {
    // Slots must hold the largest packet that shares the arena. The
    // textured-Gouraud quad (14 words) is wider than the triangle
    // (11 words); a single quad replaces two triangles, so widening the
    // slot still reduces total slot pressure for quad-heavy rooms.
    let tri = core::mem::size_of::<TriTexturedGouraud>().div_ceil(core::mem::size_of::<u32>());
    let quad = core::mem::size_of::<QuadTexturedGouraud>().div_ceil(core::mem::size_of::<u32>());
    if quad > tri {
        quad
    } else {
        tri
    }
};

/// One aligned primitive packet slot sized for the largest triangle packet.
#[derive(Copy, Clone)]
#[repr(C, align(4))]
struct PrimitivePacketSlot {
    words: [u32; PRIMITIVE_PACKET_SLOT_WORDS],
}

impl PrimitivePacketSlot {
    const ZERO: Self = Self {
        words: [0; PRIMITIVE_PACKET_SLOT_WORDS],
    };
}

/// Fixed slot-backed primitive scratch aligned for PSX GPU packets.
pub struct PrimitivePacketScratch<const SLOTS: usize> {
    slots: [PrimitivePacketSlot; SLOTS],
}

impl<const SLOTS: usize> PrimitivePacketScratch<SLOTS> {
    /// Zero-initialised scratch storage for statics.
    pub const ZERO: Self = Self {
        slots: [PrimitivePacketSlot::ZERO; SLOTS],
    };

    /// Borrow the entire slot allocation as contiguous packet words.
    ///
    /// Retained renderers use this view when they materialize variable-width
    /// packet streams directly. The typed arena and raw-word view are mutually
    /// exclusive borrows, so a frame cannot accidentally use both layouts at
    /// once.
    pub fn words_mut(&mut self) -> &mut [u32] {
        let words = SLOTS.saturating_mul(PRIMITIVE_PACKET_SLOT_WORDS);
        unsafe { core::slice::from_raw_parts_mut(self.slots.as_mut_ptr().cast::<u32>(), words) }
    }
}

/// Exact tagged-packet range committed from [`PrimitivePacketArena`].
///
/// The pointers remain valid only while the arena's backing
/// [`PrimitivePacketScratch`] remains live and unmodified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "link the committed stream into the frame ordering table"]
pub struct PrimitivePacketStream {
    first: *mut u32,
    end: *mut u32,
    words: usize,
    packets: usize,
}

impl PrimitivePacketStream {
    /// Number of contiguous words in the committed stream.
    pub const fn word_count(self) -> usize {
        self.words
    }

    /// Number of tagged GPU packets reported by the producer.
    pub const fn packet_count(self) -> usize {
        self.packets
    }

    /// True when the committed stream contains no words or packets.
    pub const fn is_empty(self) -> bool {
        self.words == 0
    }

    /// First word in the committed packet stream.
    pub const fn first_ptr(self) -> *mut u32 {
        self.first
    }

    /// One-past-the-end word in the committed packet stream.
    pub const fn end_ptr(self) -> *mut u32 {
        self.end
    }
}

/// Exclusive contiguous-word reservation from [`PrimitivePacketArena`].
///
/// Dropping a reservation without committing it leaves the arena cursor
/// unchanged. A successful commit advances by the slots needed for the words
/// actually used, not by the reservation's maximum capacity.
#[must_use = "commit the written prefix or drop the reservation without advancing the arena"]
pub struct PrimitivePacketWordReservation<'arena, 'storage> {
    arena: &'arena mut PrimitivePacketArena<'storage>,
    first_slot: usize,
    capacity_words: usize,
}

impl PrimitivePacketWordReservation<'_, '_> {
    /// Maximum number of contiguous words writable through this reservation.
    pub const fn capacity_words(&self) -> usize {
        self.capacity_words
    }

    /// Borrow the reserved words for a retained renderer to materialize a
    /// tightly packed tagged stream.
    pub fn words_mut(&mut self) -> &mut [u32] {
        let first = unsafe {
            self.arena
                .storage
                .as_mut_ptr()
                .add(self.first_slot)
                .cast::<u32>()
        };
        unsafe { core::slice::from_raw_parts_mut(first, self.capacity_words) }
    }

    /// Commit the exact prefix written by the producer.
    ///
    /// Returns `None` without advancing the arena if the word count exceeds
    /// the reservation or if only one of `used_words` and `packet_count` is
    /// zero.
    pub fn commit(self, used_words: usize, packet_count: usize) -> Option<PrimitivePacketStream> {
        if crate::r3000_usize_gt(used_words, self.capacity_words)
            || (used_words == 0) != (packet_count == 0)
        {
            return None;
        }
        let used_slots = used_words.div_ceil(PRIMITIVE_PACKET_SLOT_WORDS);
        let next_used_slots = self.arena.used_slots.checked_add(used_slots)?;
        if crate::r3000_usize_gt(next_used_slots, self.arena.storage.len()) {
            return None;
        }
        let next_packet_count = self.arena.packet_count.checked_add(packet_count)?;
        let first = unsafe {
            self.arena
                .storage
                .as_mut_ptr()
                .add(self.first_slot)
                .cast::<u32>()
        };
        let end = unsafe { first.add(used_words) };
        self.arena.used_slots = next_used_slots;
        self.arena.packet_count = next_packet_count;
        Some(PrimitivePacketStream {
            first,
            end,
            words: used_words,
            packets: packet_count,
        })
    }
}

/// Type-erased packet arena over [`PrimitivePacketScratch`].
///
/// Packets of different concrete types are written into fixed slots
/// sized to [`TriTexturedGouraud`]. The returned references stay
/// valid until the arena is reset/reused, so callers can insert their
/// packet pointers into an ordering table exactly like with
/// [`PrimitiveArena`].
pub struct PrimitivePacketArena<'a> {
    storage: &'a mut [PrimitivePacketSlot],
    used_slots: usize,
    packet_count: usize,
}

impl<'a> PrimitivePacketArena<'a> {
    /// Wrap slot-backed primitive scratch.
    pub fn new<const SLOTS: usize>(scratch: &'a mut PrimitivePacketScratch<SLOTS>) -> Self {
        Self {
            storage: &mut scratch.slots,
            used_slots: 0,
            packet_count: 0,
        }
    }

    /// Number of mixed packets written this frame.
    pub const fn len(&self) -> usize {
        self.packet_count
    }

    /// Packet capacity expressed in worst-case Gouraud triangle slots.
    pub fn capacity(&self) -> usize {
        self.storage.len()
    }

    /// Physical fixed-size slots consumed by typed packets and committed raw
    /// streams.
    pub const fn used_slots(&self) -> usize {
        self.used_slots
    }

    /// Remaining worst-case packet slots.
    pub fn remaining(&self) -> usize {
        self.capacity().saturating_sub(self.used_slots)
    }

    /// Remaining storage as a contiguous word capacity.
    pub fn remaining_words(&self) -> usize {
        self.remaining().saturating_mul(PRIMITIVE_PACKET_SLOT_WORDS)
    }

    /// Mutate an initialized range of slots as packets of one concrete type.
    ///
    /// This supports post-submit packet effects without another geometry pass
    /// or packet buffer. The range is validated against the arena cursor; a
    /// malformed range returns `false` without touching storage.
    ///
    /// # Safety
    ///
    /// Every slot in `start_slot..end_slot` must have been initialized as `T`
    /// during this arena frame and must still contain `T`. The callback must
    /// not modify the packet's tag or retain the reference.
    pub unsafe fn mutate_typed_slots<T>(
        &mut self,
        start_slot: usize,
        end_slot: usize,
        mut mutate: impl FnMut(&mut T),
    ) -> bool {
        let size = core::mem::size_of::<T>();
        if size == 0
            || size > core::mem::size_of::<PrimitivePacketSlot>()
            || core::mem::align_of::<T>() > core::mem::align_of::<PrimitivePacketSlot>()
            || start_slot > end_slot
            || end_slot > self.used_slots
        {
            return false;
        }
        let mut index = start_slot;
        while index < end_slot {
            let packet = self.storage[index].words.as_mut_ptr().cast::<T>();
            // SAFETY: guaranteed by this method's caller contract and the
            // range/size/alignment validation above.
            mutate(unsafe { &mut *packet });
            index += 1;
        }
        true
    }

    /// True when no packets have been written.
    pub const fn is_empty(&self) -> bool {
        self.packet_count == 0
    }

    /// Reserve a contiguous word range from the unconsumed arena tail.
    ///
    /// The reservation does not advance the arena until
    /// [`commit`](PrimitivePacketWordReservation::commit) succeeds.
    pub fn reserve_packet_words(
        &mut self,
        capacity_words: usize,
    ) -> Option<PrimitivePacketWordReservation<'_, 'a>> {
        if capacity_words == 0 {
            return None;
        }
        let reserved_slots = capacity_words.div_ceil(PRIMITIVE_PACKET_SLOT_WORDS);
        if reserved_slots > self.remaining() {
            return None;
        }
        let first_slot = self.used_slots;
        Some(PrimitivePacketWordReservation {
            arena: self,
            first_slot,
            capacity_words,
        })
    }

    fn push_packet<T>(&mut self, prim: T) -> Option<&mut T> {
        let size = core::mem::size_of::<T>();
        if size == 0 || size > core::mem::size_of::<PrimitivePacketSlot>() {
            return None;
        }
        if core::mem::align_of::<T>() > core::mem::align_of::<PrimitivePacketSlot>() {
            return None;
        }
        if self.used_slots >= self.storage.len() {
            return None;
        }
        let ptr = self.storage[self.used_slots].words.as_mut_ptr().cast::<T>();
        unsafe {
            ptr.write(prim);
            self.used_slots += 1;
            self.packet_count += 1;
            Some(&mut *ptr)
        }
    }

    /// Advance over a packet that is already initialized in the next fixed
    /// slot and borrow it again without changing its payload or type.
    ///
    /// # Safety
    /// The next slot must contain a valid `T` written by an earlier arena use,
    /// and no intervening write may have changed that slot to another packet
    /// type. The returned reference must not outlive this arena borrow.
    pub unsafe fn reuse_packet<T>(&mut self) -> Option<&mut T> {
        let size = core::mem::size_of::<T>();
        if size == 0 || size > core::mem::size_of::<PrimitivePacketSlot>() {
            return None;
        }
        if core::mem::align_of::<T>() > core::mem::align_of::<PrimitivePacketSlot>() {
            return None;
        }
        if self.used_slots >= self.storage.len() {
            return None;
        }
        let ptr = self.storage[self.used_slots].words.as_mut_ptr().cast::<T>();
        self.used_slots += 1;
        self.packet_count += 1;
        Some(unsafe { &mut *ptr })
    }
}

impl<T> PrimitiveSink<T> for PrimitivePacketArena<'_> {
    fn len(&self) -> usize {
        self.packet_count
    }

    fn capacity(&self) -> usize {
        self.packet_count.saturating_add(self.remaining())
    }

    fn push(&mut self, prim: T) -> Option<&mut T> {
        self.push_packet(prim)
    }

    unsafe fn push_unchecked(&mut self, prim: T) -> &mut T {
        debug_assert!(core::mem::size_of::<T>() > 0);
        debug_assert!(core::mem::size_of::<T>() <= core::mem::size_of::<PrimitivePacketSlot>());
        debug_assert!(core::mem::align_of::<T>() <= core::mem::align_of::<PrimitivePacketSlot>());
        let index = self.used_slots;
        self.used_slots += 1;
        self.packet_count += 1;
        // SAFETY: the trait contract proves `index` is in range; the slot
        // alignment/size assertions are compile-time properties of `T`.
        let ptr = unsafe { self.storage.get_unchecked_mut(index) }
            .words
            .as_mut_ptr()
            .cast::<T>();
        unsafe {
            ptr.write(prim);
            &mut *ptr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_range_clamps_and_scales() {
        let range = DepthRange::new(100, 900);
        assert_eq!(range.slot::<8>(0).index(), 0);
        assert_eq!(range.slot::<8>(100).index(), 0);
        assert_eq!(range.slot::<8>(500).index(), 3);
        assert_eq!(range.slot::<8>(899).index(), 6);
        assert_eq!(range.slot::<8>(900).index(), 7);
        assert_eq!(range.slot::<8>(1200).index(), 7);
    }

    #[test]
    fn depth_band_reserves_front_and_back_slots() {
        let range = DepthRange::new(0, 4000);
        let band = DepthBand::new(2, 5);
        assert_eq!(band.slot::<8>(range, -100).index(), 2);
        assert_eq!(band.slot::<8>(range, 0).index(), 2);
        assert_eq!(band.slot::<8>(range, 2000).index(), 3);
        assert_eq!(band.slot::<8>(range, 3999).index(), 4);
        assert_eq!(band.slot::<8>(range, 4000).index(), 5);
        assert_eq!(band.slot::<8>(range, 9000).index(), 5);
    }

    #[test]
    fn depth_band_clamps_to_table_depth() {
        let range = DepthRange::new(0, 100);
        let band = DepthBand::new(6, 99);
        assert_eq!(band.slot::<8>(range, 0).index(), 6);
        assert_eq!(band.slot::<8>(range, 100).index(), 7);
    }

    #[test]
    fn invalid_depth_range_maps_front() {
        let range = DepthRange::new(100, 100);
        assert_eq!(range.slot::<8>(500).index(), 0);
    }

    #[test]
    fn typed_depth_range_matches_raw_mapping() {
        let range = DepthRange::from_depths(CameraDepth::new(100), CameraDepth::new(900));
        assert_eq!(range.near_depth(), CameraDepth::new(100));
        assert_eq!(range.far_depth(), CameraDepth::new(900));
        assert_eq!(range.slot_depth::<8>(CameraDepth::new(500)).index(), 3);
        assert_eq!(
            CameraDepth::new(500).saturating_add(400),
            CameraDepth::new(900)
        );
    }

    #[test]
    fn ot_depth_builds_table_sized_bands() {
        assert_eq!(OtDepth::<8>::SLOT_COUNT, 8);
        assert_eq!(OtDepth::<8>::BACK_SLOT.index(), 7);
        assert_eq!(OtDepth::<8>::whole_band(), DepthBand::new(0, 7));
        assert_eq!(OtDepth::<8>::band(2, 99), DepthBand::new(2, 7));
        assert_eq!(OtDepth::<0>::whole_band(), DepthBand::new(0, 0));
    }

    #[test]
    fn primitive_arena_pushes_until_full() {
        let mut storage = [0u16; 2];
        let mut arena = PrimitiveArena::new(&mut storage);

        assert!(arena.is_empty());
        assert_eq!(arena.capacity(), 2);
        assert_eq!(arena.remaining(), 2);

        *arena.push(7).expect("slot 0") = 8;
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.remaining(), 1);

        arena.push(9).expect("slot 1");
        assert_eq!(arena.push(10), None);
        assert_eq!(arena.len(), 2);

        arena.reset();
        assert!(arena.is_empty());
        assert_eq!(arena.remaining(), 2);
    }

    #[test]
    fn primitive_arena_can_reborrow_by_index() {
        let mut storage = [0u16; 2];
        let mut arena = PrimitiveArena::new(&mut storage);

        let idx = arena.push_index(7).expect("slot 0");
        *arena.get_mut(idx).expect("reborrow") = 12;

        assert_eq!(*arena.get_mut(idx).expect("slot still live"), 12);
        assert!(arena.get_mut(1).is_none());
    }

    #[test]
    fn packet_arena_can_relink_an_initialized_slot_without_rewriting_payload() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(C)]
        struct Packet {
            tag: u32,
            command: u32,
            payload: u32,
        }

        let expected = Packet {
            tag: 0x0300_1234,
            command: 0x3400_00ff,
            payload: 0x89ab_cdef,
        };
        let mut scratch = PrimitivePacketScratch::<1>::ZERO;
        {
            let mut arena = PrimitivePacketArena::new(&mut scratch);
            arena.push(expected).expect("initial packet");
        }

        let mut arena = PrimitivePacketArena::new(&mut scratch);
        let replayed = unsafe { arena.reuse_packet::<Packet>() }.expect("cached packet");
        assert_eq!(*replayed, expected);
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.remaining(), 0);
    }

    #[test]
    fn packet_arena_mutates_only_the_valid_typed_slot_range() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(C)]
        struct Packet {
            tag: u32,
            value: u32,
        }

        let mut scratch = PrimitivePacketScratch::<3>::ZERO;
        let mut arena = PrimitivePacketArena::new(&mut scratch);
        arena.push(Packet { tag: 1, value: 10 }).unwrap();
        arena.push(Packet { tag: 2, value: 20 }).unwrap();
        arena.push(Packet { tag: 3, value: 30 }).unwrap();

        let changed = unsafe {
            arena.mutate_typed_slots::<Packet>(1, 3, |packet| {
                packet.value += 5;
            })
        };
        assert!(changed);
        assert!(!unsafe { arena.mutate_typed_slots::<Packet>(2, 4, |_| {}) });

        let mut replay = PrimitivePacketArena::new(&mut scratch);
        assert_eq!(
            unsafe { *replay.reuse_packet::<Packet>().unwrap() }.value,
            10
        );
        assert_eq!(
            unsafe { *replay.reuse_packet::<Packet>().unwrap() }.value,
            25
        );
        assert_eq!(
            unsafe { *replay.reuse_packet::<Packet>().unwrap() }.value,
            35
        );
    }

    #[test]
    fn packet_scratch_exposes_every_slot_as_contiguous_words() {
        let mut scratch = PrimitivePacketScratch::<2>::ZERO;
        let words = scratch.words_mut();
        assert_eq!(words.len(), 2 * PRIMITIVE_PACKET_SLOT_WORDS);
        words[PRIMITIVE_PACKET_SLOT_WORDS] = 0x1234_5678;
        assert_eq!(scratch.slots[1].words[0], 0x1234_5678);
    }

    #[test]
    fn packet_arena_commits_tight_stream_then_continues_with_typed_slot() {
        #[derive(Clone, Copy)]
        #[repr(C)]
        struct Packet {
            tag: u32,
            command: u32,
            payload: u32,
        }

        let mut scratch = PrimitivePacketScratch::<4>::ZERO;
        let mut arena = PrimitivePacketArena::new(&mut scratch);
        arena
            .push(Packet {
                tag: 0,
                command: 1,
                payload: 2,
            })
            .expect("typed prefix");

        let remaining_words = arena.remaining_words();
        let mut reservation = arena
            .reserve_packet_words(remaining_words)
            .expect("contiguous tail");
        let first = reservation.words_mut().as_mut_ptr();
        reservation.words_mut()[0] = 0x1111_1111;
        reservation.words_mut()[14] = 0x2222_2222;
        let stream = reservation.commit(15, 2).expect("valid stream prefix");

        assert_eq!(stream.first_ptr(), first);
        assert_eq!(stream.end_ptr(), unsafe { first.add(15) });
        assert_eq!(stream.word_count(), 15);
        assert_eq!(stream.packet_count(), 2);
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.remaining(), 1);

        arena
            .push(Packet {
                tag: 3,
                command: 4,
                payload: 5,
            })
            .expect("typed suffix");
        assert_eq!(arena.len(), 4);
        assert_eq!(arena.remaining(), 0);
    }

    #[test]
    fn invalid_packet_word_commit_does_not_advance_arena() {
        let mut scratch = PrimitivePacketScratch::<2>::ZERO;
        let mut arena = PrimitivePacketArena::new(&mut scratch);
        {
            let mut reservation = arena.reserve_packet_words(8).expect("reservation");
            assert_eq!(reservation.capacity_words(), 8);
            reservation.words_mut()[0] = 0xfeed_beef;
        }
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.used_slots(), 0);
        assert_eq!(arena.remaining(), 2);

        let reservation = arena.reserve_packet_words(8).expect("reservation");
        assert!(reservation.commit(9, 1).is_none());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.used_slots(), 0);
        assert_eq!(arena.remaining(), 2);

        let reservation = arena.reserve_packet_words(8).expect("reservation");
        assert!(reservation.commit(4, 0).is_none());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.used_slots(), 0);
        assert_eq!(arena.remaining(), 2);
    }

    #[test]
    fn dense_packet_stream_keeps_typed_sink_remaining_capacity_sound() {
        #[derive(Clone, Copy)]
        #[repr(C)]
        struct Packet {
            words: [u32; 3],
        }

        let mut scratch = PrimitivePacketScratch::<2>::ZERO;
        let mut arena = PrimitivePacketArena::new(&mut scratch);
        let stream = arena
            .reserve_packet_words(10)
            .expect("one-slot stream")
            .commit(10, 3)
            .expect("dense packets");
        assert_eq!(stream.packet_count(), 3);
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.capacity(), 2);
        assert_eq!(arena.used_slots(), 1);
        assert_eq!(arena.remaining(), 1);
        assert_eq!(
            <PrimitivePacketArena<'_> as PrimitiveSink<Packet>>::capacity(&arena),
            4
        );
        assert_eq!(
            <PrimitivePacketArena<'_> as PrimitiveSink<Packet>>::remaining(&arena),
            1
        );
    }
}
