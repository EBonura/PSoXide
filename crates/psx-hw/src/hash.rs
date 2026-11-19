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

#[cfg(not(target_arch = "mips"))]
const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
#[cfg(not(target_arch = "mips"))]
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// Host only (`cfg(not(target_arch = "mips"))`): the 64-bit multiply is not guest code. A guest
/// that needs this hash folds it with `psx_math::WideU::fnv1a_step`.
///
/// Incremental FNV-1a-64 for callers that hash non-contiguous parts
/// (clipped display rows, multi-slice fingerprints).
#[cfg(not(target_arch = "mips"))]
pub struct Fnv1a64(u64);

#[cfg(not(target_arch = "mips"))]
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

#[cfg(not(target_arch = "mips"))]
impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot FNV-1a-64 of a byte slice. Host only, like [`Fnv1a64`].
#[cfg(not(target_arch = "mips"))]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.update(bytes);
    h.finish()
}

/// The reflected CRC-32 polynomial (IEEE 802.3, zlib, PNG, `binascii.crc32`).
const CRC32_POLY: u32 = 0xedb8_8320;

/// The CRC-32 of every 4-bit value: 64 bytes, so a byte is two table steps instead of eight
/// bit steps (the guest has no data cache, so a bigger table costs a bus read per byte for
/// little more speed than this one gives).
const CRC32_NIBBLES: [u32; 16] = {
    let mut table = [0u32; 16];
    let mut n = 0;
    while n < 16 {
        let mut crc = n as u32;
        let mut bit = 0;
        while bit < 4 {
            crc = (crc >> 1) ^ (CRC32_POLY & 0u32.wrapping_sub(crc & 1));
            bit += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
};

/// Incremental CRC-32 (IEEE 802.3): the one every PSoXide check of that kind uses (PNG chunks,
/// hardware-test payload frames, save frames). The same function as `binascii.crc32`.
pub struct Crc32(u32);

impl Crc32 {
    /// A CRC that has seen no bytes.
    #[must_use]
    pub const fn new() -> Self {
        Self(!0)
    }

    /// Fold `bytes` into the running CRC.
    pub fn update(&mut self, bytes: &[u8]) {
        let mut crc = self.0;
        for &byte in bytes {
            crc ^= u32::from(byte);
            crc = (crc >> 4) ^ CRC32_NIBBLES[(crc & 15) as usize];
            crc = (crc >> 4) ^ CRC32_NIBBLES[(crc & 15) as usize];
        }
        self.0 = crc;
    }

    /// The CRC of everything fed so far. The hasher stays usable afterwards.
    #[must_use]
    pub const fn finish(&self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot CRC-32 of a byte slice.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_check_value_and_the_bitwise_form() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        let bitwise = |data: &[u8]| {
            let mut crc = !0u32;
            for &byte in data {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (CRC32_POLY & 0u32.wrapping_sub(crc & 1));
                }
            }
            !crc
        };
        let data: [u8; 300] = core::array::from_fn(|i| (i as u32 * 61 + 9) as u8);
        for len in 0..=data.len() {
            assert_eq!(crc32(&data[..len]), bitwise(&data[..len]), "len {len}");
        }
        let mut split = Crc32::default();
        split.update(&data[..100]);
        split.update(&data[100..]);
        assert_eq!(split.finish(), crc32(&data));
    }

    #[test]
    fn matches_reference_vectors() {
        // Published FNV-1a-64 test vectors.
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn incremental_equals_one_shot() {
        let mut h = Fnv1a64::new();
        h.update(b"foo");
        h.update(b"bar");
        assert_eq!(h.finish(), fnv1a_64(b"foobar"));
    }
}
