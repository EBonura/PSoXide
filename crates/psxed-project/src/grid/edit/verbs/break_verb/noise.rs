//! The hash every Break decision comes from.
//!
//! A decision is a pure function of the seed, a world position and an op id:
//! it never consumes a sequence, so the result does not depend on the order
//! the cells are visited in, widening the region does not reshuffle the
//! interior, and raising the intensity only adds operations (the test is
//! `hash below a threshold that grows with the intensity`). Integers only, so
//! a seed means the same thing on every machine.

/// What a decision is about, so two decisions on one position differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Op {
    /// A bite at a plateau edge or a dent inside.
    Crack = 1,
    /// Which diagonal and which half a crack uses.
    CrackShape,
    /// How far a crack drops.
    CrackDrop,
    /// A lattice vertex moves.
    Vertex,
    /// How far it moves, and which way.
    VertexDelta,
    /// A plateau lip vertex crumbles.
    Lip,
    /// An open wall top.
    Top,
    /// How far it drops.
    TopDrop,
    /// A whole wall segment stubs.
    Stub,
    /// A ceiling vertex moves.
    Ceil,
    /// A cell turns into a gap.
    Gap,
    /// Material mix on a cracked tile.
    Mix,
}

fn finalize(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// A 32 bit hash of `(seed, op, position)`.
pub fn hash(seed: u32, op: Op, at: [i32; 2], salt: u32) -> u32 {
    let mut h = finalize(seed ^ 0x9e37_79b9);
    h = finalize(h ^ (op as u32).wrapping_mul(0x85eb_ca6b));
    h = finalize(h ^ (at[0] as u32).wrapping_mul(0xc2b2_ae35));
    h = finalize(h ^ (at[1] as u32).wrapping_mul(0x27d4_eb2f));
    finalize(h ^ salt.wrapping_mul(0x1656_67b1))
}

/// Whether the decision passes at `permille` chances in a thousand.
pub fn chance(seed: u32, op: Op, at: [i32; 2], permille: u32) -> bool {
    hash(seed, op, at, 0) % 1000 < permille
}

/// A number in `0..n` for the decision (`n` of 0 gives 0).
pub fn below(seed: u32, op: Op, at: [i32; 2], salt: u32, n: u32) -> u32 {
    if n == 0 {
        0
    } else {
        hash(seed, op, at, salt) % n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_pure_and_spreads() {
        assert_eq!(hash(7, Op::Crack, [3, 4], 0), hash(7, Op::Crack, [3, 4], 0));
        assert_ne!(hash(7, Op::Crack, [3, 4], 0), hash(8, Op::Crack, [3, 4], 0));
        assert_ne!(
            hash(7, Op::Crack, [3, 4], 0),
            hash(7, Op::Vertex, [3, 4], 0)
        );
        assert_ne!(hash(7, Op::Crack, [3, 4], 0), hash(7, Op::Crack, [4, 3], 0));
        // About 30 percent pass at 300 permille.
        let hits = (0..4000)
            .filter(|i| chance(11, Op::Crack, [i % 64, i / 64], 300))
            .count();
        assert!((1000..1400).contains(&hits), "{hits}");
        // Monotone: whatever passes at 200 passes at 500.
        for i in 0..500 {
            let at = [i, -i];
            if chance(5, Op::Gap, at, 200) {
                assert!(chance(5, Op::Gap, at, 500));
            }
        }
    }
}
