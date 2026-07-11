//! FNV-1a-64 -- the one hash every PSoXide parity surface uses.
//!
//! VRAM/display checkpoint hashes, library fingerprints, and the MCP
//! screenshot cache all compare FNV-1a-64 values produced in different
//! crates; before this module each of them hand-rolled the loop, and
//! the copies could drift without any test noticing until checkpoint
//! hashes stopped matching. One implementation, no_std, no deps.
//!
//! Not a cryptographic hash -- stable fingerprinting of trusted data
//! only.

const OFFSET_BASIS: u64 = 0xCBF2_9CE4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01B3;

/// Incremental FNV-1a-64 for callers that hash non-contiguous parts
/// (clipped display rows, multi-slice fingerprints).
pub struct Fnv1a64(u64);

impl Fnv1a64 {
    /// Hasher seeded with the FNV-1a offset basis.
    pub const fn new() -> Self {
        Self(OFFSET_BASIS)
    }

    /// Fold `bytes` into the running hash.
    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(PRIME);
        }
    }

    /// Current hash value. The hasher stays usable afterwards.
    pub const fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot FNV-1a-64 of a byte slice.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.update(bytes);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_vectors() {
        // Published FNV-1a-64 test vectors.
        assert_eq!(fnv1a_64(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xAF63_DC4C_8601_EC8C);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171F73967E8);
    }

    #[test]
    fn incremental_equals_one_shot() {
        let mut h = Fnv1a64::new();
        h.update(b"foo");
        h.update(b"bar");
        assert_eq!(h.finish(), fnv1a_64(b"foobar"));
    }
}
