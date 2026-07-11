//! Glue over `psx_game_runtime::room_streaming`: re-exports the
//! scheduler vocabulary and keeps the thin resolvers that borrow this
//! example's static slot-buffer (`STREAMED_ROOM_SLOTS`) and scheduler
//! (`ROOM_STREAM_SCHEDULER`) instances, plus the cooked-table threading
//! for the room-graph ring. The buffer reads themselves live on
//! [`StreamedRoomSlots`] since the vram_runtime carve.

use super::*;

#[cfg(feature = "cd-stream-bench")]
pub(super) use psx_game_runtime::room_streaming::{
    room_requested, RoomStreamLoadPlan, RoomStreamScheduler, StreamedRoomSlots,
};

/// Parse a streamed room's collision view out of its slot byte buffer,
/// re-validating residency first. The `'static` on the result comes
/// from borrowing the example's static slot-buffer instance -- see the
/// staleness contract on [`StreamedRoomSlots`]: the value is only good
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
        (*core::ptr::addr_of!(STREAMED_ROOM_SLOTS))
            .compact_collision_room(&mut *core::ptr::addr_of_mut!(ROOM_STREAM_SCHEDULER), index)
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
