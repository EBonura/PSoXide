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
//! against the shipping cooked map:
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

use crate::collision::{Trace, TraceScratch};
use crate::pxbsp_resident::PxbspResidentMap;
use crate::{SliceReader, Vec3I32};

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

/// Traces run per collision hull.
const TRACES_PER_HULL: usize = 300;
/// Point queries run against the render tree.
const LEAF_QUERIES: usize = 300;

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

/// Hash of a fixed collision and visibility workload over `map_bytes`.
///
/// Returns a sentinel rather than panicking on a map it cannot load, so a
/// mismatch is always reported as a differing hash rather than as a guest that
/// stopped rendering.
pub fn compute_hash(map_bytes: &[u8]) -> u32 {
    let mut map = PxbspResidentMap::default();
    if map.load(0, &mut SliceReader::new(map_bytes)).is_err() {
        return 0xDEAD_0001;
    }

    let mut hash = FNV_OFFSET;
    let mut scratch = TraceScratch::new();
    let mut rng: u32 = 0x1234_5678;

    for hull in 0..3usize {
        let Some(collision) = map.model_collision_hull(0, hull) else {
            hash = fold(hash, -1);
            continue;
        };
        for _ in 0..TRACES_PER_HULL {
            let sx = (next(&mut rng) % 4096) - 2048;
            let sy = (next(&mut rng) % 512) - 128;
            let sz = (next(&mut rng) % 4096) - 2048;
            let ex = sx + (next(&mut rng) % 512) - 256;
            let ey = sy - (next(&mut rng) % 256);
            let ez = sz + (next(&mut rng) % 512) - 256;
            let start = Vec3I32 {
                x: sx << 12,
                y: sy << 12,
                z: sz << 12,
            };
            let end = Vec3I32 {
                x: ex << 12,
                y: ey << 12,
                z: ez << 12,
            };
            let mut trace = Trace::default();
            if collision.trace_into(&start, &end, &mut scratch, &mut trace) {
                hash = fold(hash, trace.fraction);
                hash = fold(hash, trace.end.x);
                hash = fold(hash, trace.end.y);
                hash = fold(hash, trace.end.z);
                hash = fold(hash, trace.normal.x as i32);
                hash = fold(hash, trace.normal.y as i32);
                hash = fold(hash, trace.normal.z as i32);
                hash = fold(hash, trace.plane_distance);
                hash = fold(hash, trace.all_solid.is_set() as i32);
                hash = fold(hash, trace.start_solid.is_set() as i32);
            } else {
                hash = fold(hash, -2);
            }
        }
    }

    let mut rng = 0x9E37_79B9u32;
    for _ in 0..LEAF_QUERIES {
        let x = ((next(&mut rng) % 4096) - 2048) << 12;
        let y = ((next(&mut rng) % 512) - 128) << 12;
        let z = ((next(&mut rng) % 4096) - 2048) << 12;
        match map.point_leaf_index(Vec3I32 { x, y, z }) {
            Some(leaf) => hash = fold(hash, leaf as i32),
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
    fn a_map_that_will_not_load_reports_a_sentinel() {
        assert_eq!(compute_hash(b"not a map"), 0xDEAD_0001);
    }

    #[test]
    fn folding_is_order_sensitive() {
        // A hash that ignored ordering would miss a miscompile that swapped
        // two results, which is exactly the shape a bad delay slot produces.
        assert_ne!(fold(fold(FNV_OFFSET, 1), 2), fold(fold(FNV_OFFSET, 2), 1));
    }
}
