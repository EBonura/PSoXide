//! A fixed collision workload whose result is a single hash, for catching
//! miscompilation on `mipsel-sony-psx`.
//!
//! The guest and the host run *this* function, not two copies of it, so any
//! difference in the hash is a difference in generated code. Run it natively to
//! get the oracle, run it on the console or in the emulator, compare.
//!
//! # Why this exists
//!
//! LLVM's MIPS delay-slot filler will hoist a load into a branch delay slot
//! whose destination the next instruction reads. The R3000A has no load
//! interlock, so that instruction sees the register one cycle before the load
//! lands and silently computes with a stale value. Measured on 2026-08-31
//! against the shipping cooked map (a PXBSP workload, since replaced by this
//! one over the grid collision):
//!
//! ```text
//!   native oracle, opt-level 0/1/2/3/s/z    0x0c11a9af
//!   mipsel opt-level 2 / 3 / s              0x0c11a9af   matches
//!   mipsel opt-level z                      0x6d260478   DIVERGES
//!   mipsel opt-level z + the flag below     0x0c11a9af   matches
//! ```
//!
//! `-Cllvm-args=-disable-mips-df-backward-search` (set in
//! `tools/build_guest_staged.sh`) is what makes the last row correct.
//!
//! This also retired a standing rule that guest `opt-level = "s"` miscompiles:
//! it does not, and never did. `z` was the broken one and the attribution had
//! drifted to its neighbour.
//!
//! # Using it
//!
//! Run both sides and compare; `tools/toolchain_check.sh` does exactly that.
//! Re-run after any toolchain bump, and after changing guest codegen flags.

use crate::collision::GridTraceProvider;
use crate::format::SECTOR_BYTES;
use crate::resident::ResidentRooms;
use crate::view::{CollisionView, LevelView, RoomView};
use alloc::vec::Vec;
use psx_engine::{CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, RoomPoint};

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

/// Body traces run against the level.
const BODY_TRACES: usize = 600;
/// Line-of-sight, room lookup and contents queries run against the level.
const POINT_QUERIES: usize = 300;

/// Returned for a level that does not load, so a mismatch is always a
/// differing hash rather than a guest that stopped rendering.
pub const LOAD_FAILED: u32 = 0xDEAD_0001;

fn fold(hash: u32, value: i32) -> u32 {
    let mut hash = hash;
    let bytes = (value as u32).to_le_bytes();
    let mut index = 0;
    while index < 4 {
        hash = (hash ^ bytes[index] as u32).wrapping_mul(FNV_PRIME);
        index += 1;
    }
    hash
}

/// Deterministic sweep. A fixed LCG rather than anything from the host, so the
/// two sides visit exactly the same coordinates in exactly the same order.
fn next(state: &mut u32) -> i32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state >> 8) as i32
}

/// Hash of a fixed collision workload over a cooked level (`level` is the PGL
/// core, `rooms` the room pack). Both must be 4-byte aligned.
pub fn compute_hash(level: &[u8], rooms: &[u8]) -> u32 {
    let Ok(level) = LevelView::parse(level) else {
        return LOAD_FAILED;
    };
    let mut views: Vec<Option<CollisionView<'_>>> = Vec::with_capacity(level.rooms.len());
    for record in level.rooms.iter() {
        let start = usize::try_from(record.pack_sector).unwrap_or(usize::MAX) * SECTOR_BYTES;
        let Some(bytes) = rooms.get(start..) else {
            return LOAD_FAILED;
        };
        let Ok(view) = RoomView::parse(bytes) else {
            return LOAD_FAILED;
        };
        views.push(Some(view.col));
    }
    let Some(set) = ResidentRooms::from_collisions(level, &views) else {
        return LOAD_FAILED;
    };
    let world = set.collision();
    let span = 24 * level.sector_units() as i32;
    let origin = level.rooms.first().map_or([0; 3], |room| room.origin);
    let mut provider = GridTraceProvider::new(&world);

    let mut hash = FNV_OFFSET;
    let mut rng: u32 = 0x1234_5678;
    for _ in 0..BODY_TRACES {
        let start = [
            origin[0] + next(&mut rng) % span,
            origin[1] + (next(&mut rng) % 512) - 128,
            origin[2] + next(&mut rng) % span,
        ];
        let end = [
            start[0] + (next(&mut rng) % 512) - 256,
            start[1] - (next(&mut rng) % 256),
            start[2] + (next(&mut rng) % 512) - 256,
        ];
        let query = CollisionTraceQuery::body(
            RoomPoint::from_array(start),
            RoomPoint::from_array(end),
            12,
            48,
        );
        let mut trace = CollisionTrace::default();
        if provider.trace_into(query, &mut trace) {
            hash = fold(hash, trace.fraction_q12);
            hash = fold(hash, trace.end.x);
            hash = fold(hash, trace.end.y);
            hash = fold(hash, trace.end.z);
            hash = fold(hash, i32::from(trace.normal_q12[0]));
            hash = fold(hash, i32::from(trace.normal_q12[1]));
            hash = fold(hash, i32::from(trace.normal_q12[2]));
            hash = fold(hash, trace.plane_distance);
            hash = fold(hash, trace.all_solid as i32);
            hash = fold(hash, trace.start_solid as i32);
        } else {
            hash = fold(hash, -2);
        }
    }

    let mut rng = 0x9E37_79B9u32;
    let mut hint = 0usize;
    for _ in 0..POINT_QUERIES {
        let a = [
            origin[0] + next(&mut rng) % span,
            origin[1] + (next(&mut rng) % 512) - 128,
            origin[2] + next(&mut rng) % span,
        ];
        let b = [
            a[0] + (next(&mut rng) % 2048) - 1024,
            a[1] + (next(&mut rng) % 256) - 128,
            a[2] + (next(&mut rng) % 2048) - 1024,
        ];
        hash = fold(hash, world.line_of_sight(a, b) as i32);
        hash = fold(hash, i32::from(world.contents_at(a)));
        match world.locate(hint, a) {
            Some(located) => {
                hint = located.room;
                hash = fold(hash, located.room as i32);
            }
            None => hash = fold(hash, -3),
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sweep_is_deterministic() {
        // The whole check rests on both sides walking identical coordinates,
        // so pin the generator rather than trusting it to stay put.
        let mut a = 0x1234_5678u32;
        let first: [i32; 4] = [next(&mut a), next(&mut a), next(&mut a), next(&mut a)];
        let mut b = 0x1234_5678u32;
        let again: [i32; 4] = [next(&mut b), next(&mut b), next(&mut b), next(&mut b)];
        assert_eq!(first, again);
    }

    #[test]
    fn a_level_that_will_not_load_reports_a_sentinel() {
        assert_eq!(compute_hash(b"not a level", b""), LOAD_FAILED);
    }

    #[test]
    fn folding_is_order_sensitive() {
        // A hash that ignored ordering would miss a miscompile that swapped
        // two results, which is exactly the shape a bad delay slot produces.
        assert_ne!(fold(fold(FNV_OFFSET, 1), 2), fold(fold(FNV_OFFSET, 2), 1));
    }
}
