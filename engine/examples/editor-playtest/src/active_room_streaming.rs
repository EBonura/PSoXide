//! Glue over `psx_game_runtime::room_streaming`: re-exports the
//! scheduler vocabulary and keeps the resolvers that read this
//! example's `static mut` slot buffers (`STREAMED_ROOM_WORDS`) and
//! scheduler instance (`ROOM_STREAM_SCHEDULER`), plus the cooked-table
//! threading for the room-graph ring, until the statics/VRAM carve.

use super::*;

#[cfg(feature = "cd-stream-bench")]
pub(super) use psx_game_runtime::room_streaming::{
    room_requested, streamed_chunk_range_valid, streamed_room_chunk_view, RoomStreamLoadPlan,
    RoomStreamScheduler,
};

/// Parse a streamed room's collision view out of its slot byte
/// buffer, re-validating residency first. The `'static` lifetime on
/// the result is a lie (see the contract on
/// `streamed_record_slice` in active_room_cache.rs): the slices point
/// into a slot the scheduler can overwrite, so the value is only good
/// until the next streaming step. Holding it longer is sound only for
/// ACTIVE-WINDOW rooms, which are pinned against eviction; the
/// camera/motor collision caches rely on exactly that, plus cache
/// keys that include the active-room mask so a room leaving the
/// window forces a re-gather before its slot can be reused.
#[cfg(feature = "cd-stream-bench")]
pub(super) fn parse_streamed_compact_collision_room(
    slot: usize,
    index: RoomIndex,
) -> Option<CompactCollisionRoom<'static>> {
    let _ = slot;
    unsafe {
        let resident_slot = ROOM_STREAM_SCHEDULER.resident_slot_for(index)?;
        let byte_count = ROOM_STREAM_SCHEDULER.resident_byte_count(resident_slot)?;
        let bytes = streamed_room_slot_bytes(resident_slot, byte_count)?;
        let view = streamed_room_chunk_view(bytes, index)?;
        if view.flags & STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT == 0 {
            return None;
        }
        let collision =
            bytes.get(view.collision_offset..view.collision_offset + view.collision_bytes)?;
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_HITS, 1);
        CompactCollisionRoom::from_bytes(collision).ok()
    }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_slot_bytes(slot: usize, byte_count: usize) -> Option<&'static [u8]> {
    if slot >= STREAMED_ROOM_SLOT_COUNT || byte_count > STREAMED_ROOM_SLOT_BYTES {
        return None;
    }
    unsafe {
        let ptr = core::ptr::addr_of!(STREAMED_ROOM_WORDS[slot])
            .cast::<u32>()
            .cast::<u8>();
        Some(core::slice::from_raw_parts(ptr, byte_count))
    }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_is_resident(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_resident(index) }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_is_loading(index: RoomIndex) -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.is_loading(index) }
}

#[cfg(feature = "cd-stream-bench")]
pub(super) fn streamed_room_stream_active() -> bool {
    unsafe { ROOM_STREAM_SCHEDULER.job.is_active() }
}

/// This example's room-graph ring over the cooked `ROOMS`/`ROOM_PORTALS`
/// tables; the BFS itself lives in
/// [`psx_game_runtime::room_streaming::room_graph_ring`].
pub(super) fn room_graph_ring(
    start: RoomIndex,
    max_depth: u16,
    out: &mut [RoomIndex],
    out_cap: usize,
) -> usize {
    psx_game_runtime::room_streaming::room_graph_ring::<MAX_STREAMED_ROOM_INDEX_COUNT>(
        ROOMS,
        ROOM_PORTALS,
        start,
        max_depth,
        out,
        out_cap,
    )
}
