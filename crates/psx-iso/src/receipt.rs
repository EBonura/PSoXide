//! The gate receipt: proof inside a PSX-EXE that `psoxide-pgo apply` built it under the
//! architecture gate (`sdk/profile.toml`, `psx-gate`).
//!
//! The receipt lives in the header's reserved region (`400h..`), which the BIOS and the
//! emulator's side-load ignore, so it changes no loaded byte. It carries the SHA-256 of the
//! executable with the receipt area zeroed, so an executable patched or relinked after the
//! gate ran no longer matches and cannot be pressed.
//!
//! ```text
//!   400h  "PSXGATE1"
//!   408h  sha256 of the whole file (header and t_size payload bytes) with 400h..460h zeroed
//!   428h  kind: 0 shipping, 1 diag (a diagnostic feature of an SDK crate is on)
//!   429h  program name length (at most 47)
//!   42Ah  program name (root package name of the program)
//! ```

use crate::exe::EXE_HEADER_BYTES;

/// Where the receipt starts in the header.
pub const RECEIPT_OFFSET: usize = 0x400;
/// Bytes the receipt occupies.
pub const RECEIPT_BYTES: usize = 0x60;
const MAGIC: &[u8; 8] = b"PSXGATE1";
const NAME_AT: usize = 0x2a;
/// Longest program name a receipt records.
pub const MAX_NAME: usize = RECEIPT_BYTES - NAME_AT;

/// What a receipt says about the build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// No diagnostic feature of an SDK crate is on.
    Shipping,
    /// At least one diagnostic feature is on: the image is for development, not a disc.
    Diag,
}

/// A receipt that matches its executable.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Receipt {
    /// Shipping or diag.
    pub kind: Kind,
    /// The program the gate checked it as.
    pub program: [u8; MAX_NAME],
    /// Used bytes of `program`.
    pub program_len: u8,
}

impl Receipt {
    /// The program name.
    #[must_use]
    pub fn program(&self) -> &str {
        core::str::from_utf8(&self.program[..usize::from(self.program_len)]).unwrap_or("")
    }
}

/// Why an executable has no usable receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiptError {
    /// Not a PSX-EXE, or shorter than its header.
    NotAnExe,
    /// No receipt: the executable did not come through `psoxide-pgo apply`.
    Missing,
    /// A receipt whose hash is not this executable's: it was changed after the gate ran.
    Mismatch,
}

/// The digest the receipt records: everything up to the end of the header's payload, with the
/// receipt area zeroed, and the payload read as its header says (a short tail is zeros).
fn digest(bytes: &[u8]) -> [u8; 32] {
    let t_size = u32::from_le_bytes([bytes[0x1c], bytes[0x1d], bytes[0x1e], bytes[0x1f]]) as usize;
    let total = EXE_HEADER_BYTES + t_size;
    let mut sha = Sha256::new();
    sha.update(&bytes[..RECEIPT_OFFSET]);
    sha.update(&[0u8; RECEIPT_BYTES]);
    let end = total.min(bytes.len());
    sha.update(&bytes[RECEIPT_OFFSET + RECEIPT_BYTES..end]);
    let mut missing = total - end;
    let zeros = [0u8; 256];
    while missing > 0 {
        let n = missing.min(zeros.len());
        sha.update(&zeros[..n]);
        missing -= n;
    }
    sha.finish()
}

fn is_exe(bytes: &[u8]) -> bool {
    bytes.len() >= EXE_HEADER_BYTES && &bytes[..8] == b"PS-X EXE"
}

/// Write a receipt for `program` into the header of `bytes` (a PSX-EXE).
///
/// # Errors
/// [`ReceiptError::NotAnExe`] when `bytes` is not a PSX-EXE or `program` is too long.
pub fn write(bytes: &mut [u8], program: &str, kind: Kind) -> Result<(), ReceiptError> {
    if !is_exe(bytes) || program.len() > MAX_NAME {
        return Err(ReceiptError::NotAnExe);
    }
    bytes[RECEIPT_OFFSET..RECEIPT_OFFSET + RECEIPT_BYTES].fill(0);
    let sum = digest(bytes);
    let area = &mut bytes[RECEIPT_OFFSET..RECEIPT_OFFSET + RECEIPT_BYTES];
    area[..8].copy_from_slice(MAGIC);
    area[8..40].copy_from_slice(&sum);
    area[40] = u8::from(kind == Kind::Diag);
    area[41] = program.len() as u8;
    area[NAME_AT..NAME_AT + program.len()].copy_from_slice(program.as_bytes());
    Ok(())
}

/// Read and verify the receipt of a PSX-EXE.
///
/// # Errors
/// See [`ReceiptError`].
pub fn verify(bytes: &[u8]) -> Result<Receipt, ReceiptError> {
    if !is_exe(bytes) {
        return Err(ReceiptError::NotAnExe);
    }
    let area = &bytes[RECEIPT_OFFSET..RECEIPT_OFFSET + RECEIPT_BYTES];
    if &area[..8] != MAGIC {
        return Err(ReceiptError::Missing);
    }
    if area[8..40] != digest(bytes) {
        return Err(ReceiptError::Mismatch);
    }
    let len = area[41];
    if usize::from(len) > MAX_NAME {
        return Err(ReceiptError::Mismatch);
    }
    let mut program = [0u8; MAX_NAME];
    program.copy_from_slice(&area[NAME_AT..NAME_AT + MAX_NAME]);
    Ok(Receipt {
        kind: if area[40] == 0 {
            Kind::Shipping
        } else {
            Kind::Diag
        },
        program,
        program_len: len,
    })
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4), streaming, no allocation.
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// A fresh hash.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    /// Add bytes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    /// The digest.
    #[must_use]
    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.length * 8;
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(add);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn hex(sum: [u8; 32]) -> std::string::String {
        use std::fmt::Write;
        sum.iter().fold(std::string::String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    #[test]
    fn sha256_known_answers() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex(h.finish()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut h = Sha256::new();
        h.update(b"");
        assert_eq!(
            hex(h.finish()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut h = Sha256::new();
        for _ in 0..100_000 {
            h.update(b"aaaaaaaaaa");
        }
        assert_eq!(
            hex(h.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    fn exe(payload: usize) -> Vec<u8> {
        let mut v = vec![0u8; EXE_HEADER_BYTES + payload];
        v[..8].copy_from_slice(b"PS-X EXE");
        v[0x1c..0x20].copy_from_slice(&(payload as u32).to_le_bytes());
        for (i, b) in v[EXE_HEADER_BYTES..].iter_mut().enumerate() {
            *b = i as u8;
        }
        v
    }

    #[test]
    fn a_fresh_exe_has_no_receipt() {
        assert_eq!(verify(&exe(2048)), Err(ReceiptError::Missing));
        assert_eq!(verify(b"nope"), Err(ReceiptError::NotAnExe));
    }

    #[test]
    fn a_written_receipt_verifies_and_names_the_program() {
        let mut v = exe(2048);
        write(&mut v, "rover", Kind::Shipping).unwrap();
        let r = verify(&v).unwrap();
        assert_eq!((r.program(), r.kind), ("rover", Kind::Shipping));
        write(&mut v, "x", Kind::Diag).unwrap();
        assert_eq!(verify(&v).unwrap().kind, Kind::Diag);
    }

    #[test]
    fn a_changed_payload_or_header_fails_and_a_short_tail_is_zeros() {
        let mut v = exe(2048);
        write(&mut v, "p", Kind::Shipping).unwrap();
        let mut patched = v.clone();
        patched[EXE_HEADER_BYTES + 5] ^= 1;
        assert_eq!(verify(&patched), Err(ReceiptError::Mismatch));
        let mut moved = v.clone();
        moved[0x10] = 1;
        assert_eq!(verify(&moved), Err(ReceiptError::Mismatch));
        // A link that stops a few bytes short: the receipt hashes the tail as zeros, so padding
        // the file out (what the disc builder does) keeps it valid.
        let mut short = exe(4096);
        short.truncate(short.len() - 100);
        short[EXE_HEADER_BYTES + 3996..].fill(0);
        write(&mut short, "p", Kind::Shipping).unwrap();
        let mut padded = short.clone();
        padded.resize(EXE_HEADER_BYTES + 4096, 0);
        assert!(verify(&short).is_ok() && verify(&padded).is_ok());
    }
}
