// SPDX-License-Identifier: GPL-2.0-or-later
//! Fixed-capacity cache metadata and byte storage for `no_std`/no-heap targets.
//!
//! The two types in this crate deliberately solve different problems:
//!
//! - [`SlotTable`] owns cache identity, asynchronous load state, pinning, and
//!   LRU choice. A load completes through an opaque [`Reservation`], so a late
//!   completion cannot publish into a slot that has been cancelled and reused.
//! - [`FixedByteArena`] owns bytes in caller-selected fixed storage. Its handles
//!   are generation checked, and explicit safe-point compaction reports a
//!   layout generation for invalidating derived pointer-bearing views.
//!
//! Neither type owns game policy, performs I/O, allocates VRAM, or parses an
//! asset. Callers keep those concerns in a transaction around the two
//! primitives and release any returned resources on cancellation or eviction.
//!
//! Both types have an all-zero [`zeroed`](SlotTable::zeroed) constructor so a
//! large instance can live in `.bss`. Do not replace a large runtime instance
//! by value on MIPS; initialize and mutate the static arena in place.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod private {
    pub trait Sealed {}
}

/// A cache payload whose empty representation is all zeroes.
///
/// This trait is sealed so [`SlotTable::zeroed`] can guarantee that every
/// payload byte in an empty table is zero. Arrays of the supported primitive
/// types can hold larger caller-defined metadata without giving the table
/// ownership of the underlying RAM or VRAM resource.
pub trait CacheValue: private::Sealed + Copy {
    /// The all-zero empty value.
    const ZERO: Self;
}

macro_rules! impl_cache_value {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl private::Sealed for $ty {}
            impl CacheValue for $ty {
                const ZERO: Self = 0;
            }
        )+
    };
}

impl_cache_value!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl private::Sealed for () {}

impl CacheValue for () {
    const ZERO: Self = ();
}

impl<T: CacheValue, const N: usize> private::Sealed for [T; N] {}

impl<T: CacheValue, const N: usize> CacheValue for [T; N] {
    const ZERO: Self = [T::ZERO; N];
}

/// Identity of one chunk inside one mounted source/catalog generation.
///
/// `source` is deliberately part of the key: caller-chosen chunk ids are only
/// stable inside their pack or catalog. A caller must choose a new non-zero
/// source value when the mounted pack, resolved variant catalog, or hot-loaded
/// project changes. Chunk id zero remains valid.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    source: u32,
    chunk_id: u32,
}

impl CacheKey {
    const EMPTY: Self = Self {
        source: 0,
        chunk_id: 0,
    };

    /// Build a key. Source zero is reserved for an empty table slot.
    pub const fn new(source: u32, chunk_id: u32) -> Option<Self> {
        if source == 0 {
            None
        } else {
            Some(Self { source, chunk_id })
        }
    }

    /// Mounted source/catalog generation chosen by the caller.
    pub const fn source(self) -> u32 {
        self.source
    }

    /// Caller-chosen chunk or semantic asset id inside [`source`](Self::source).
    pub const fn chunk_id(self) -> u32 {
        self.chunk_id
    }

    const fn is_valid(self) -> bool {
        self.source != 0
    }
}

/// Load state of one cache slot.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// The slot holds no key or value.
    Empty = 0,
    /// A reservation owns the slot while an asynchronous value is produced.
    Loading = 1,
    /// The value is complete and may be resolved by its handle.
    Ready = 2,
}

/// Generation-checked identity of one cache slot incarnation.
///
/// Fields are private so callers cannot forge a completion token. A handle
/// ceases to resolve after cancellation, release, eviction, or slot reuse.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SlotHandle {
    slot: u16,
    generation: u32,
}

impl SlotHandle {
    /// Logical slot index, suitable for indexing caller-owned parallel storage.
    pub const fn slot(self) -> usize {
        self.slot as usize
    }

    /// Incarnation generation of the logical slot.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Opaque authority to finish or cancel one newly-started load.
///
/// Only [`SlotTable::reserve`] creates this token. Existing `Loading` lookups
/// return a handle, not another reservation, so duplicate requests cannot both
/// publish a result.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    key: CacheKey,
    handle: SlotHandle,
}

impl Reservation {
    /// Key whose value is being produced.
    pub const fn key(self) -> CacheKey {
        self.key
    }

    /// Generation-checked destination handle.
    pub const fn handle(self) -> SlotHandle {
        self.handle
    }
}

/// A ready entry removed to make room or explicitly released.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Evicted<V: CacheValue> {
    key: CacheKey,
    value: V,
    handle: SlotHandle,
}

impl<V: CacheValue> Evicted<V> {
    /// Removed key.
    pub const fn key(self) -> CacheKey {
        self.key
    }

    /// Removed caller-owned value/handle metadata.
    pub const fn value(self) -> V {
        self.value
    }

    /// Handle that identified the entry before it was invalidated.
    pub const fn handle(self) -> SlotHandle {
        self.handle
    }

    /// Split into `(key, value, old_handle)`.
    pub const fn into_parts(self) -> (CacheKey, V, SlotHandle) {
        (self.key, self.value, self.handle)
    }
}

/// Result of asking the table to make a key resident.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReserveResult<V: CacheValue> {
    /// The key is already ready; no load should start.
    Ready(SlotHandle),
    /// The key already has an in-flight load; no duplicate load should start.
    Loading(SlotHandle),
    /// The caller owns a new load transaction.
    Reserved {
        /// Token required by [`SlotTable::complete`] or [`SlotTable::cancel`].
        reservation: Reservation,
        /// Ready entry displaced by LRU selection, if the table was full.
        evicted: Option<Evicted<V>>,
    },
}

/// Why a new reservation could not be created.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReserveError {
    /// Source zero is reserved for empty slots.
    InvalidKey,
    /// Every slot is pinned or has a load in flight.
    NoVictim,
}

/// Why an opaque handle or reservation no longer authorizes an operation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// The handle's slot is outside this table.
    InvalidSlot,
    /// The slot has been released, cancelled, evicted, or reused.
    Stale,
    /// The slot incarnation exists but is not in the required state.
    WrongState,
}

/// A value rejected by [`SlotTable::complete`].
///
/// Returning ownership is important for resource handles: the caller can free
/// a RAM/VRAM allocation created by a completion that lost a cancellation race.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RejectedValue<V: CacheValue> {
    reason: TokenError,
    value: V,
}

impl<V: CacheValue> RejectedValue<V> {
    /// Rejection reason.
    pub const fn reason(self) -> TokenError {
        self.reason
    }

    /// Rejected value returned to the caller for cleanup.
    pub const fn value(self) -> V {
        self.value
    }

    /// Split into `(reason, value)`.
    pub const fn into_parts(self) -> (TokenError, V) {
        (self.reason, self.value)
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
struct Slot<V: CacheValue> {
    key: CacheKey,
    value: V,
    generation: u32,
    last_used: u32,
    state: SlotState,
    pinned: u8,
}

impl<V: CacheValue> Slot<V> {
    const fn empty(generation: u32) -> Self {
        Self {
            key: CacheKey::EMPTY,
            value: V::ZERO,
            generation,
            last_used: 0,
            state: SlotState::Empty,
            pinned: 0,
        }
    }
}

/// Fixed-capacity, mount-scoped cache identity table.
///
/// Lookup is a linear scan over `N`. PS1 residency sets are intentionally small,
/// and this avoids a dense reverse table proportional to a sparse `u32` id
/// space. The table owns only copyable metadata; variable-sized bytes and VRAM
/// rectangles remain in caller-owned allocators.
pub struct SlotTable<V: CacheValue, const N: usize> {
    slots: [Slot<V>; N],
    epoch: u32,
}

impl<V: CacheValue, const N: usize> SlotTable<V, N> {
    /// All-zero empty table suitable for a static `.bss` arena.
    pub const fn zeroed() -> Self {
        assert!(N <= u16::MAX as usize + 1);
        Self {
            slots: [Slot::empty(0); N],
            epoch: 0,
        }
    }

    /// Number of logical slots.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Current wrapping LRU clock.
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Advance the wrapping LRU clock once per cache cycle/frame.
    pub fn bump_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Reserve a key or report its existing state.
    ///
    /// Empty slots are preferred. Otherwise the ready, unpinned slot with the
    /// greatest wrapping age is replaced. Loading slots are never ordinary
    /// victims. The displaced value is returned for resource cleanup.
    pub fn reserve(&mut self, key: CacheKey) -> Result<ReserveResult<V>, ReserveError> {
        if !key.is_valid() {
            return Err(ReserveError::InvalidKey);
        }
        if let Some(slot) = self.find_key(key) {
            let handle = self.handle(slot);
            return match self.slots[slot].state {
                SlotState::Ready => {
                    self.slots[slot].last_used = self.epoch;
                    Ok(ReserveResult::Ready(handle))
                }
                SlotState::Loading => Ok(ReserveResult::Loading(handle)),
                SlotState::Empty => unreachable!(),
            };
        }

        let slot = self.choose_slot().ok_or(ReserveError::NoVictim)?;
        let evicted = if self.slots[slot].state == SlotState::Ready {
            Some(Evicted {
                key: self.slots[slot].key,
                value: self.slots[slot].value,
                handle: self.handle(slot),
            })
        } else {
            None
        };
        let generation = next_generation(self.slots[slot].generation);
        self.slots[slot] = Slot {
            key,
            value: V::ZERO,
            generation,
            last_used: self.epoch,
            state: SlotState::Loading,
            pinned: 0,
        };
        let reservation = Reservation {
            key,
            handle: self.handle(slot),
        };
        Ok(ReserveResult::Reserved {
            reservation,
            evicted,
        })
    }

    /// Publish a load result if `reservation` still owns this slot incarnation.
    ///
    /// On failure, the value is returned inside [`RejectedValue`] so the caller
    /// can roll back any resource allocation it represents.
    pub fn complete(
        &mut self,
        reservation: Reservation,
        value: V,
    ) -> Result<SlotHandle, RejectedValue<V>> {
        let slot = match self.validate_reservation(reservation) {
            Ok(slot) => slot,
            Err(reason) => return Err(RejectedValue { reason, value }),
        };
        self.slots[slot].value = value;
        self.slots[slot].state = SlotState::Ready;
        self.slots[slot].last_used = self.epoch;
        Ok(self.handle(slot))
    }

    /// Cancel a load if `reservation` still owns this slot incarnation.
    pub fn cancel(&mut self, reservation: Reservation) -> Result<CacheKey, TokenError> {
        let slot = self.validate_reservation(reservation)?;
        let key = self.slots[slot].key;
        self.clear_slot(slot);
        Ok(key)
    }

    /// Resolve a ready key and mark it most recently used.
    pub fn get(&mut self, key: CacheKey) -> Option<(SlotHandle, &V)> {
        let slot = self.find_key(key)?;
        if self.slots[slot].state != SlotState::Ready {
            return None;
        }
        self.slots[slot].last_used = self.epoch;
        let handle = self.handle(slot);
        Some((handle, &self.slots[slot].value))
    }

    /// Resolve a ready key without touching its LRU age.
    pub fn peek(&self, key: CacheKey) -> Option<(SlotHandle, &V)> {
        let slot = self.find_key(key)?;
        if self.slots[slot].state != SlotState::Ready {
            return None;
        }
        Some((self.handle(slot), &self.slots[slot].value))
    }

    /// Resolve a ready handle without touching its LRU age.
    pub fn resolve(&self, handle: SlotHandle) -> Option<&V> {
        let slot = self.validate_handle(handle).ok()?;
        if self.slots[slot].state != SlotState::Ready {
            return None;
        }
        Some(&self.slots[slot].value)
    }

    /// Current handle and state for a key, if present.
    pub fn state_of(&self, key: CacheKey) -> Option<(SlotHandle, SlotState)> {
        let slot = self.find_key(key)?;
        Some((self.handle(slot), self.slots[slot].state))
    }

    /// Pin or unpin one exact slot incarnation.
    pub fn set_pinned(&mut self, handle: SlotHandle, pinned: bool) -> Result<(), TokenError> {
        let slot = self.validate_handle(handle)?;
        if self.slots[slot].state == SlotState::Empty {
            return Err(TokenError::WrongState);
        }
        self.slots[slot].pinned = u8::from(pinned);
        Ok(())
    }

    /// Clear every pin. Loading slots remain protected by their state.
    pub fn unpin_all(&mut self) {
        for slot in &mut self.slots {
            slot.pinned = 0;
        }
    }

    /// Explicitly release one exact ready slot incarnation.
    ///
    /// This is allowed for pinned entries because the handle makes the caller's
    /// intent explicit. Loading entries must be cancelled through their
    /// reservation instead.
    pub fn release(&mut self, handle: SlotHandle) -> Result<Evicted<V>, TokenError> {
        let slot = self.validate_handle(handle)?;
        if self.slots[slot].state != SlotState::Ready {
            return Err(TokenError::WrongState);
        }
        let removed = Evicted {
            key: self.slots[slot].key,
            value: self.slots[slot].value,
            handle,
        };
        self.clear_slot(slot);
        Ok(removed)
    }

    /// Explicitly release a ready key. An absent key returns `Ok(None)`.
    ///
    /// A loading key returns [`TokenError::WrongState`] and must instead be
    /// cancelled by the owner of its reservation.
    pub fn evict_ready(&mut self, key: CacheKey) -> Result<Option<Evicted<V>>, TokenError> {
        let Some(slot) = self.find_key(key) else {
            return Ok(None);
        };
        if self.slots[slot].state != SlotState::Ready {
            return Err(TokenError::WrongState);
        }
        self.release(self.handle(slot)).map(Some)
    }

    /// Number of ready entries.
    pub fn ready_len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state == SlotState::Ready)
            .count()
    }

    /// Number of loading or ready entries.
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.state != SlotState::Empty)
            .count()
    }

    /// Whether no slot is loading or ready.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn find_key(&self, key: CacheKey) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.state != SlotState::Empty && slot.key == key)
    }

    fn handle(&self, slot: usize) -> SlotHandle {
        SlotHandle {
            slot: slot as u16,
            generation: self.slots[slot].generation,
        }
    }

    fn validate_handle(&self, handle: SlotHandle) -> Result<usize, TokenError> {
        let slot = handle.slot as usize;
        if slot >= N {
            return Err(TokenError::InvalidSlot);
        }
        if self.slots[slot].generation != handle.generation
            || self.slots[slot].state == SlotState::Empty
        {
            return Err(TokenError::Stale);
        }
        Ok(slot)
    }

    fn validate_reservation(&self, reservation: Reservation) -> Result<usize, TokenError> {
        let slot = self.validate_handle(reservation.handle)?;
        if self.slots[slot].key != reservation.key {
            return Err(TokenError::Stale);
        }
        if self.slots[slot].state != SlotState::Loading {
            return Err(TokenError::WrongState);
        }
        Ok(slot)
    }

    fn choose_slot(&self) -> Option<usize> {
        if let Some(empty) = self
            .slots
            .iter()
            .position(|slot| slot.state == SlotState::Empty)
        {
            return Some(empty);
        }

        let mut victim = None;
        let mut greatest_age = 0u32;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.state != SlotState::Ready || slot.pinned != 0 {
                continue;
            }
            let age = self.epoch.wrapping_sub(slot.last_used);
            if victim.is_none() || age > greatest_age {
                victim = Some(index);
                greatest_age = age;
            }
        }
        victim
    }

    fn clear_slot(&mut self, slot: usize) {
        let generation = next_generation(self.slots[slot].generation);
        self.slots[slot] = Slot::empty(generation);
    }
}

const fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 {
        1
    } else {
        next
    }
}

/// Generation-checked identity of one byte-arena allocation.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ByteHandle {
    slot: u16,
    generation: u32,
}

impl ByteHandle {
    /// Logical allocation slot.
    pub const fn slot(self) -> usize {
        self.slot as usize
    }

    /// Incarnation generation of the allocation.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Byte range occupied by one allocation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ByteRange {
    offset: u32,
    len: u32,
}

impl ByteRange {
    /// Offset from the start of the arena.
    pub const fn offset(self) -> usize {
        self.offset as usize
    }

    /// Exact payload length, excluding alignment gaps.
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Whether the range is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Byte-arena allocation or handle failure.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArenaError {
    /// Logical slot is outside the arena.
    InvalidSlot,
    /// Zero bytes cannot form an allocation, or the size exceeds the arena.
    InvalidSize,
    /// The logical slot already owns an allocation.
    Occupied,
    /// No contiguous aligned range currently fits the allocation.
    NoSpace,
    /// The allocation is not in the state required by this operation.
    WrongState,
    /// A write did not begin exactly where the previous fragment ended.
    NonSequentialWrite,
    /// Sealing was attempted before every promised payload byte was written.
    Incomplete,
    /// Compaction was requested while a writable I/O destination is live.
    Busy,
    /// The allocation was released or its slot was reused.
    StaleHandle,
}

/// Result of an explicit byte-arena compaction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    moved_allocations: u32,
    layout_generation: u32,
}

impl Compaction {
    /// Number of allocations whose byte offset changed.
    pub const fn moved_allocations(self) -> usize {
        self.moved_allocations as usize
    }

    /// Arena layout generation after compaction.
    pub const fn layout_generation(self) -> u32 {
        self.layout_generation
    }

    /// Whether any live allocation moved.
    pub const fn moved(self) -> bool {
        self.moved_allocations != 0
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
struct ByteRun {
    offset: u32,
    len: u32,
    written: u32,
    generation: u32,
    state: ByteState,
}

impl ByteRun {
    const EMPTY: Self = Self {
        offset: 0,
        len: 0,
        written: 0,
        generation: 0,
        state: ByteState::Empty,
    };
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ByteState {
    Empty = 0,
    Writable = 1,
    Sealed = 2,
}

/// Fixed, aligned byte arena with generation-checked logical slots.
///
/// `BYTES` is the total storage, `SLOTS` the maximum simultaneous allocations,
/// and `ALIGN` the required start alignment. For direct CD destinations use a
/// concrete capacity such as `FixedByteArena<{32 * 2048}, 8, 2048>`.
/// Allocation never grows or allocates from the heap.
///
/// [`compact`](Self::compact) is explicit because moving bytes invalidates
/// derived raw pointers and parsed views. Safe Rust slices cannot survive its
/// mutable borrow, but callers retaining offset-derived or foreign pointers must
/// compare [`layout_generation`](Self::layout_generation) and rebuild them.
pub struct FixedByteArena<const BYTES: usize, const SLOTS: usize, const ALIGN: usize> {
    bytes: [u8; BYTES],
    runs: [ByteRun; SLOTS],
    layout_generation: u32,
}

impl<const BYTES: usize, const SLOTS: usize, const ALIGN: usize>
    FixedByteArena<BYTES, SLOTS, ALIGN>
{
    /// All-zero empty arena suitable for a static `.bss` allocation.
    pub const fn zeroed() -> Self {
        assert!(ALIGN != 0 && (ALIGN & (ALIGN - 1)) == 0);
        assert!(BYTES <= u32::MAX as usize);
        assert!(SLOTS <= u16::MAX as usize + 1);
        Self {
            bytes: [0; BYTES],
            runs: [ByteRun::EMPTY; SLOTS],
            layout_generation: 0,
        }
    }

    /// Total byte capacity.
    pub const fn capacity_bytes(&self) -> usize {
        BYTES
    }

    /// Number of logical allocation slots.
    pub const fn slot_capacity(&self) -> usize {
        SLOTS
    }

    /// Required start alignment for every allocation.
    pub const fn alignment(&self) -> usize {
        ALIGN
    }

    /// Generation of the current byte layout.
    pub const fn layout_generation(&self) -> u32 {
        self.layout_generation
    }

    /// Exact bytes held by live allocations, excluding alignment gaps.
    pub fn used_bytes(&self) -> usize {
        self.runs
            .iter()
            .filter(|run| run.state != ByteState::Empty)
            .map(|run| run.len as usize)
            .sum()
    }

    /// Number of live allocations.
    pub fn allocation_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|run| run.state != ByteState::Empty)
            .count()
    }

    /// Allocate an aligned byte range for one empty logical slot.
    pub fn prepare(&mut self, slot: usize, byte_count: usize) -> Result<ByteHandle, ArenaError> {
        if slot >= SLOTS {
            return Err(ArenaError::InvalidSlot);
        }
        if byte_count == 0 || byte_count > BYTES || byte_count > u32::MAX as usize {
            return Err(ArenaError::InvalidSize);
        }
        if self.runs[slot].state != ByteState::Empty {
            return Err(ArenaError::Occupied);
        }
        let offset = self.find_gap(byte_count).ok_or(ArenaError::NoSpace)?;
        let generation = next_generation(self.runs[slot].generation);
        self.runs[slot] = ByteRun {
            offset: offset as u32,
            len: byte_count as u32,
            written: 0,
            generation,
            state: ByteState::Writable,
        };
        Ok(ByteHandle {
            slot: slot as u16,
            generation,
        })
    }

    /// Current allocation handle for a logical slot.
    pub fn handle_for_slot(&self, slot: usize) -> Option<ByteHandle> {
        let run = self.runs.get(slot)?;
        if run.state == ByteState::Empty {
            return None;
        }
        Some(ByteHandle {
            slot: slot as u16,
            generation: run.generation,
        })
    }

    /// Seal a fully-written allocation, making it visible to readers.
    pub fn seal(&mut self, handle: ByteHandle) -> Result<(), ArenaError> {
        let slot = self.validate_handle(handle)?;
        if self.runs[slot].state != ByteState::Writable {
            return Err(ArenaError::WrongState);
        }
        if self.runs[slot].written != self.runs[slot].len {
            return Err(ArenaError::Incomplete);
        }
        self.runs[slot].state = ByteState::Sealed;
        Ok(())
    }

    /// Resolve an exact sealed allocation as immutable bytes.
    pub fn resolve(&self, handle: ByteHandle) -> Result<&[u8], ArenaError> {
        let slot = self.validate_handle(handle)?;
        if self.runs[slot].state != ByteState::Sealed {
            return Err(ArenaError::WrongState);
        }
        let range = self.range_for_slot(slot);
        self.bytes
            .get(range.offset()..range.offset() + range.len())
            .ok_or(ArenaError::StaleHandle)
    }

    /// Append a bounded fragment to an exact writable allocation.
    ///
    /// Fragments must be contiguous and in order. This lets [`seal`](Self::seal)
    /// prove that the exact promised payload, excluding sector padding, arrived.
    pub fn write(
        &mut self,
        handle: ByteHandle,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), ArenaError> {
        let slot = self.validate_handle(handle)?;
        if self.runs[slot].state != ByteState::Writable {
            return Err(ArenaError::WrongState);
        }
        if offset != self.runs[slot].written as usize {
            return Err(ArenaError::NonSequentialWrite);
        }
        let end = offset
            .checked_add(bytes.len())
            .ok_or(ArenaError::InvalidSize)?;
        if end > self.runs[slot].len as usize {
            return Err(ArenaError::InvalidSize);
        }
        let base = self.runs[slot].offset as usize;
        let target = self
            .bytes
            .get_mut(base + offset..base + end)
            .ok_or(ArenaError::InvalidSize)?;
        target.copy_from_slice(bytes);
        self.runs[slot].written = end as u32;
        Ok(())
    }

    /// Current byte range for an exact live allocation.
    pub fn range(&self, handle: ByteHandle) -> Result<ByteRange, ArenaError> {
        let slot = self.validate_handle(handle)?;
        Ok(self.range_for_slot(slot))
    }

    /// Release one exact allocation and invalidate its handle.
    pub fn release(&mut self, handle: ByteHandle) -> Result<ByteRange, ArenaError> {
        let slot = self.validate_handle(handle)?;
        let range = self.range_for_slot(slot);
        let generation = next_generation(self.runs[slot].generation);
        self.runs[slot] = ByteRun {
            generation,
            ..ByteRun::EMPTY
        };
        Ok(range)
    }

    /// Pack live ranges toward byte zero at an explicit caller-chosen safe point.
    ///
    /// Logical allocation generations do not change: handles still resolve to
    /// their moved bytes. `layout_generation` changes once if anything moved so
    /// callers can invalidate derived offset/pointer caches.
    pub fn compact(&mut self) -> Result<Compaction, ArenaError> {
        if self.runs.iter().any(|run| run.state == ByteState::Writable) {
            return Err(ArenaError::Busy);
        }
        let mut cursor = 0usize;
        let mut moved = 0usize;

        loop {
            let mut next_slot = None;
            let mut next_offset = usize::MAX;
            for (slot, run) in self.runs.iter().enumerate() {
                let offset = run.offset as usize;
                if run.state == ByteState::Sealed && offset >= cursor && offset < next_offset {
                    next_slot = Some(slot);
                    next_offset = offset;
                }
            }
            let Some(slot) = next_slot else {
                break;
            };
            let len = self.runs[slot].len as usize;
            let destination = align_up(cursor, ALIGN).expect("valid arena alignment");
            debug_assert!(destination <= next_offset);
            if destination != next_offset {
                self.bytes
                    .copy_within(next_offset..next_offset + len, destination);
                self.runs[slot].offset = destination as u32;
                moved += 1;
            }
            cursor = destination + len;
        }

        if moved != 0 {
            self.layout_generation = next_generation(self.layout_generation);
        }
        Ok(Compaction {
            moved_allocations: moved as u32,
            layout_generation: self.layout_generation,
        })
    }

    fn validate_handle(&self, handle: ByteHandle) -> Result<usize, ArenaError> {
        let slot = handle.slot as usize;
        if slot >= SLOTS {
            return Err(ArenaError::InvalidSlot);
        }
        let run = self.runs[slot];
        if run.state == ByteState::Empty || run.generation != handle.generation {
            return Err(ArenaError::StaleHandle);
        }
        Ok(slot)
    }

    fn range_for_slot(&self, slot: usize) -> ByteRange {
        let run = self.runs[slot];
        ByteRange {
            offset: run.offset,
            len: run.len,
        }
    }

    fn find_gap(&self, byte_count: usize) -> Option<usize> {
        let mut cursor = 0usize;
        loop {
            let candidate = align_up(cursor, ALIGN)?;
            let mut next_start = BYTES;
            let mut next_end = BYTES;
            for run in &self.runs {
                if run.state == ByteState::Empty {
                    continue;
                }
                let start = run.offset as usize;
                if start >= candidate && start < next_start {
                    next_start = start;
                    next_end = start.checked_add(run.len as usize)?;
                }
            }
            if candidate.checked_add(byte_count)? <= next_start {
                return Some(candidate);
            }
            if next_start == BYTES {
                return None;
            }
            cursor = next_end;
        }
    }
}

const fn align_up(value: usize, alignment: usize) -> Option<usize> {
    let mask = alignment - 1;
    match value.checked_add(mask) {
        Some(rounded) => Some(rounded & !mask),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Table = SlotTable<u32, 3>;

    static ZERO_TABLE: SlotTable<[u32; 2], 2> = SlotTable::zeroed();
    static ZERO_ARENA: FixedByteArena<32, 2, 8> = FixedByteArena::zeroed();

    fn key(source: u32, chunk: u32) -> CacheKey {
        CacheKey::new(source, chunk).expect("valid cache key")
    }

    fn reserve_new<V: CacheValue, const N: usize>(
        table: &mut SlotTable<V, N>,
        key: CacheKey,
    ) -> (Reservation, Option<Evicted<V>>) {
        match table.reserve(key).expect("reservation") {
            ReserveResult::Reserved {
                reservation,
                evicted,
            } => (reservation, evicted),
            _ => panic!("expected a new reservation"),
        }
    }

    fn put<const N: usize>(table: &mut SlotTable<u32, N>, key: CacheKey, value: u32) -> SlotHandle {
        let (reservation, evicted) = reserve_new(table, key);
        assert!(evicted.is_none());
        table.complete(reservation, value).expect("completion")
    }

    #[test]
    fn zeroed_constructors_are_const_and_logically_empty() {
        assert!(ZERO_TABLE.is_empty());
        assert_eq!(ZERO_TABLE.epoch(), 0);
        assert_eq!(ZERO_TABLE.capacity(), 2);
        assert_eq!(ZERO_ARENA.used_bytes(), 0);
        assert_eq!(ZERO_ARENA.allocation_count(), 0);
        assert_eq!(ZERO_ARENA.layout_generation(), 0);
    }

    #[test]
    fn source_zero_is_reserved_but_chunk_zero_is_valid() {
        assert_eq!(CacheKey::new(0, 7), None);
        let mut table = Table::zeroed();
        let valid = key(1, 0);
        let (reservation, _) = reserve_new(&mut table, valid);
        table.complete(reservation, 9).unwrap();
        assert_eq!(table.peek(valid).map(|(_, value)| *value), Some(9));
    }

    #[test]
    fn duplicate_requests_do_not_create_two_completion_tokens() {
        let mut table = Table::zeroed();
        let asset = key(1, 4);
        let (reservation, _) = reserve_new(&mut table, asset);
        assert_eq!(
            table.reserve(asset),
            Ok(ReserveResult::Loading(reservation.handle()))
        );
        let handle = table.complete(reservation, 44).unwrap();
        assert_eq!(table.reserve(asset), Ok(ReserveResult::Ready(handle)));
        let duplicate = table.complete(reservation, 55).unwrap_err();
        assert_eq!(duplicate.into_parts(), (TokenError::WrongState, 55));
        assert_eq!(table.resolve(handle), Some(&44));
    }

    #[test]
    fn stale_completion_after_cancel_and_reuse_cannot_publish() {
        let mut table = SlotTable::<u32, 1>::zeroed();
        let old_key = key(1, 1);
        let new_key = key(1, 2);
        let (old, _) = reserve_new(&mut table, old_key);
        assert_eq!(table.cancel(old), Ok(old_key));
        let (new, _) = reserve_new(&mut table, new_key);
        assert_eq!(old.handle().slot(), new.handle().slot());
        assert_ne!(old.handle().generation(), new.handle().generation());

        let rejected = table.complete(old, 111).unwrap_err();
        assert_eq!(rejected.into_parts(), (TokenError::Stale, 111));
        assert_eq!(
            table.state_of(new_key),
            Some((new.handle(), SlotState::Loading))
        );
        assert_eq!(table.complete(new, 222), Ok(new.handle()));
        assert_eq!(table.peek(new_key).map(|(_, value)| *value), Some(222));
    }

    #[test]
    fn loading_slots_are_never_lru_victims() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        let (first, _) = reserve_new(&mut table, key(1, 1));
        let (second, _) = reserve_new(&mut table, key(1, 2));
        assert_eq!(table.reserve(key(1, 3)), Err(ReserveError::NoVictim));
        assert_eq!(table.complete(first, 1), Ok(first.handle()));
        let (third, evicted) = reserve_new(&mut table, key(1, 3));
        assert_eq!(evicted.unwrap().value(), 1);
        assert_eq!(
            table.state_of(key(1, 2)),
            Some((second.handle(), SlotState::Loading))
        );
        assert_eq!(table.complete(third, 3), Ok(third.handle()));
    }

    #[test]
    fn lru_eviction_returns_key_value_and_old_handle() {
        let mut table = Table::zeroed();
        let first = key(1, 1);
        let second = key(1, 2);
        let third = key(1, 3);
        let fourth = key(1, 4);
        let first_handle = put(&mut table, first, 10);
        table.bump_epoch();
        put(&mut table, second, 20);
        table.bump_epoch();
        put(&mut table, third, 30);
        table.bump_epoch();
        let _ = table.get(second);
        let _ = table.get(third);

        let (reservation, evicted) = reserve_new(&mut table, fourth);
        let evicted = evicted.expect("full table evicts LRU");
        assert_eq!(evicted.into_parts(), (first, 10, first_handle));
        assert_eq!(table.resolve(first_handle), None);
        table.complete(reservation, 40).unwrap();
    }

    #[test]
    fn pinning_protects_ready_entry_but_explicit_release_is_allowed() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        let pinned = put(&mut table, key(1, 1), 10);
        table.bump_epoch();
        put(&mut table, key(1, 2), 20);
        table.set_pinned(pinned, true).unwrap();
        table.bump_epoch();
        let (_, evicted) = reserve_new(&mut table, key(1, 3));
        assert_eq!(evicted.unwrap().value(), 20);
        assert_eq!(table.resolve(pinned), Some(&10));
        assert_eq!(table.release(pinned).unwrap().value(), 10);
        assert_eq!(table.resolve(pinned), None);
    }

    #[test]
    fn all_pinned_ready_slots_refuse_ordinary_eviction() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        let first = put(&mut table, key(1, 1), 10);
        let second = put(&mut table, key(1, 2), 20);
        table.set_pinned(first, true).unwrap();
        table.set_pinned(second, true).unwrap();
        assert_eq!(table.reserve(key(1, 3)), Err(ReserveError::NoVictim));
    }

    #[test]
    fn explicit_ready_eviction_returns_resources_and_refuses_loading_slot() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        let ready_key = key(1, 1);
        let loading_key = key(1, 2);
        put(&mut table, ready_key, 77);
        let (_loading, _) = reserve_new(&mut table, loading_key);
        assert_eq!(table.evict_ready(loading_key), Err(TokenError::WrongState));
        let evicted = table.evict_ready(ready_key).unwrap().unwrap();
        assert_eq!(evicted.key(), ready_key);
        assert_eq!(evicted.value(), 77);
        assert_eq!(table.evict_ready(ready_key), Ok(None));
    }

    #[test]
    fn lru_age_is_correct_across_epoch_wrap() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        table.epoch = u32::MAX - 1;
        let old_key = key(1, 1);
        let fresh_key = key(1, 2);
        put(&mut table, old_key, 10);
        table.bump_epoch();
        put(&mut table, fresh_key, 20);
        table.bump_epoch(); // wraps to zero
        let _ = table.get(fresh_key);

        let (_, evicted) = reserve_new(&mut table, key(1, 3));
        assert_eq!(evicted.unwrap().key(), old_key);
    }

    #[test]
    fn mount_source_prevents_equal_chunk_ids_from_aliasing() {
        let mut table = SlotTable::<u32, 2>::zeroed();
        let first = key(7, 99);
        let second = key(8, 99);
        put(&mut table, first, 1);
        put(&mut table, second, 2);
        assert_eq!(table.peek(first).map(|(_, value)| *value), Some(1));
        assert_eq!(table.peek(second).map(|(_, value)| *value), Some(2));
    }

    #[test]
    fn generation_wrap_never_produces_the_empty_zero_generation() {
        let mut table = SlotTable::<u32, 1>::zeroed();
        table.slots[0].generation = u32::MAX;
        let (reservation, _) = reserve_new(&mut table, key(1, 1));
        assert_eq!(reservation.handle().generation(), 1);
    }

    #[test]
    fn byte_arena_allocates_aligned_ranges_and_checks_handles() {
        let mut arena = FixedByteArena::<48, 3, 8>::zeroed();
        let first = arena.prepare(0, 3).unwrap();
        let second = arena.prepare(1, 9).unwrap();
        assert_eq!(arena.range(first).unwrap(), ByteRange { offset: 0, len: 3 });
        assert_eq!(
            arena.range(second).unwrap(),
            ByteRange { offset: 8, len: 9 }
        );
        assert_eq!(arena.resolve(first), Err(ArenaError::WrongState));
        arena.write(first, 0, &[1, 2, 3]).unwrap();
        arena.seal(first).unwrap();
        assert_eq!(arena.resolve(first), Ok(&[1, 2, 3][..]));
        assert_eq!(arena.used_bytes(), 12);
        assert_eq!(arena.allocation_count(), 2);
    }

    #[test]
    fn byte_release_and_reuse_invalidate_stale_handle() {
        let mut arena = FixedByteArena::<32, 1, 8>::zeroed();
        let first = arena.prepare(0, 8).unwrap();
        assert_eq!(arena.release(first).unwrap().len(), 8);
        assert_eq!(arena.resolve(first), Err(ArenaError::StaleHandle));
        let second = arena.prepare(0, 8).unwrap();
        assert_ne!(first.generation(), second.generation());
        assert_eq!(arena.release(first), Err(ArenaError::StaleHandle));
        arena.write(second, 0, &[0; 8]).unwrap();
        arena.seal(second).unwrap();
        assert!(arena.resolve(second).is_ok());
    }

    #[test]
    fn explicit_compaction_preserves_bytes_handles_and_reports_layout_change() {
        let mut arena = FixedByteArena::<40, 4, 8>::zeroed();
        let first = arena.prepare(0, 8).unwrap();
        let gap = arena.prepare(1, 8).unwrap();
        let moved = arena.prepare(2, 8).unwrap();
        arena.write(first, 0, &[1; 8]).unwrap();
        arena.seal(first).unwrap();
        arena.write(moved, 0, &[3; 8]).unwrap();
        arena.seal(moved).unwrap();
        arena.release(gap).unwrap();
        assert_eq!(arena.range(moved).unwrap().offset(), 16);

        let result = arena.compact().unwrap();
        assert_eq!(result.moved_allocations(), 1);
        assert_eq!(result.layout_generation(), 1);
        assert_eq!(arena.range(moved).unwrap().offset(), 8);
        assert_eq!(arena.resolve(moved), Ok(&[3; 8][..]));
        assert_eq!(arena.resolve(first), Ok(&[1; 8][..]));

        let unchanged = arena.compact().unwrap();
        assert!(!unchanged.moved());
        assert_eq!(unchanged.layout_generation(), 1);
    }

    #[test]
    fn fragmented_arena_requires_caller_chosen_compaction() {
        let mut arena = FixedByteArena::<32, 4, 8>::zeroed();
        let first = arena.prepare(0, 8).unwrap();
        let middle = arena.prepare(1, 8).unwrap();
        let last = arena.prepare(2, 8).unwrap();
        arena.write(first, 0, &[1; 8]).unwrap();
        arena.seal(first).unwrap();
        arena.write(last, 0, &[9; 8]).unwrap();
        arena.seal(last).unwrap();
        arena.release(middle).unwrap();
        assert_eq!(arena.prepare(3, 12), Err(ArenaError::NoSpace));
        assert!(arena.compact().unwrap().moved());
        let tail = arena.prepare(3, 12).unwrap();
        assert_eq!(arena.range(tail).unwrap().offset(), 16);
        assert_eq!(arena.resolve(last), Ok(&[9; 8][..]));
        assert!(arena.resolve(first).is_ok());
    }

    #[test]
    fn byte_arena_rejects_invalid_requests_without_changing_state() {
        let mut arena = FixedByteArena::<16, 1, 4>::zeroed();
        assert_eq!(arena.prepare(1, 4), Err(ArenaError::InvalidSlot));
        assert_eq!(arena.prepare(0, 0), Err(ArenaError::InvalidSize));
        assert_eq!(arena.prepare(0, 17), Err(ArenaError::InvalidSize));
        let handle = arena.prepare(0, 4).unwrap();
        assert_eq!(arena.prepare(0, 4), Err(ArenaError::Occupied));
        assert_eq!(arena.allocation_count(), 1);
        assert_eq!(arena.resolve(handle), Err(ArenaError::WrongState));
        arena.write(handle, 0, &[0; 4]).unwrap();
        arena.seal(handle).unwrap();
        assert!(arena.resolve(handle).is_ok());
    }

    #[test]
    fn writable_arena_blocks_compaction_and_requires_complete_sequential_write() {
        let mut arena = FixedByteArena::<16, 1, 4>::zeroed();
        let handle = arena.prepare(0, 6).unwrap();
        assert_eq!(arena.compact(), Err(ArenaError::Busy));
        assert_eq!(
            arena.write(handle, 1, &[9]),
            Err(ArenaError::NonSequentialWrite)
        );
        arena.write(handle, 0, &[1, 2]).unwrap();
        assert_eq!(arena.seal(handle), Err(ArenaError::Incomplete));
        assert_eq!(
            arena.write(handle, 2, &[3, 4, 5, 6, 7]),
            Err(ArenaError::InvalidSize)
        );
        arena.write(handle, 2, &[3, 4, 5, 6]).unwrap();
        arena.seal(handle).unwrap();
        assert_eq!(arena.write(handle, 6, &[]), Err(ArenaError::WrongState));
        assert_eq!(arena.resolve(handle), Ok(&[1, 2, 3, 4, 5, 6][..]));
    }

    #[test]
    fn cache_cancel_and_arena_release_reject_every_late_old_transaction_write() {
        let mut table = SlotTable::<u32, 1>::zeroed();
        let mut arena = FixedByteArena::<8, 1, 4>::zeroed();
        let (old_reservation, _) = reserve_new(&mut table, key(1, 1));
        let old_bytes = arena.prepare(old_reservation.handle().slot(), 4).unwrap();
        arena.write(old_bytes, 0, &[1, 1]).unwrap();

        table.cancel(old_reservation).unwrap();
        arena.release(old_bytes).unwrap();
        let (new_reservation, _) = reserve_new(&mut table, key(1, 2));
        let new_bytes = arena.prepare(new_reservation.handle().slot(), 4).unwrap();

        assert_eq!(
            table.complete(old_reservation, 111).unwrap_err().value(),
            111
        );
        assert_eq!(
            arena.write(old_bytes, 2, &[1, 1]),
            Err(ArenaError::StaleHandle)
        );
        assert_eq!(arena.seal(old_bytes), Err(ArenaError::StaleHandle));

        arena.write(new_bytes, 0, &[2, 2, 2, 2]).unwrap();
        arena.seal(new_bytes).unwrap();
        table.complete(new_reservation, 222).unwrap();
        assert_eq!(arena.resolve(new_bytes), Ok(&[2, 2, 2, 2][..]));
        assert_eq!(table.peek(key(1, 2)).map(|(_, value)| *value), Some(222));
    }

    #[test]
    fn byte_generation_wrap_never_produces_zero() {
        let mut arena = FixedByteArena::<8, 1, 1>::zeroed();
        arena.runs[0].generation = u32::MAX;
        let handle = arena.prepare(0, 1).unwrap();
        assert_eq!(handle.generation(), 1);
    }

    #[test]
    fn layout_generation_wrap_never_produces_zero() {
        let mut arena = FixedByteArena::<24, 2, 8>::zeroed();
        let gap = arena.prepare(0, 8).unwrap();
        let moved = arena.prepare(1, 8).unwrap();
        arena.write(moved, 0, &[0; 8]).unwrap();
        arena.seal(moved).unwrap();
        arena.release(gap).unwrap();
        arena.layout_generation = u32::MAX;
        assert!(arena.compact().unwrap().moved());
        assert_eq!(arena.layout_generation(), 1);
        assert_eq!(arena.range(moved).unwrap().offset(), 0);
    }
}
