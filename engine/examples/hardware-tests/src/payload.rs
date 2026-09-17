// SPDX-License-Identifier: GPL-2.0-or-later
//! Payload plumbing shared by the capture page and every targeted probe:
//! a bounded little-endian writer, CRC-32, Base64, the FNV hashes the probes
//! summarise SPU RAM with, and QR module lookup. One copy, so a host decoder
//! that checks one probe's CRC checks them all.

/// Bounded little-endian writer over a caller-owned buffer. Writing past the
/// end panics: a payload that outgrew its buffer is a layout bug, and a
/// truncated capture would decode as a different, wrong one.
pub(crate) struct BinaryBuffer<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> BinaryBuffer<'a> {
    pub(crate) fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The bytes written so far.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub(crate) fn push_u8(&mut self, value: u8) {
        assert!(self.len < self.bytes.len(), "payload binary overflow");
        self.bytes[self.len] = value;
        self.len += 1;
    }

    pub(crate) fn push_u16(&mut self, value: u16) {
        self.push_bytes(&value.to_le_bytes());
    }

    pub(crate) fn push_u32(&mut self, value: u32) {
        self.push_bytes(&value.to_le_bytes());
    }

    pub(crate) fn push_bytes(&mut self, values: &[u8]) {
        let end = self.len + values.len();
        assert!(end <= self.bytes.len(), "payload binary overflow");
        self.bytes[self.len..end].copy_from_slice(values);
        self.len = end;
    }
}

/// Append to a text buffer. Panics past the end, like `BinaryBuffer`.
pub(crate) fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let end = *len + bytes.len();
    target[*len..end].copy_from_slice(bytes);
    *len = end;
}

/// CRC-32 (IEEE, reflected), the one `binascii.crc32` computes on the host.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Standard padded Base64. Returns the encoded length.
pub(crate) fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut target = 0usize;
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        assert!(target + 4 <= output.len(), "payload Base64 overflow");
        output[target] = ALPHABET[(a >> 2) as usize];
        output[target + 1] = ALPHABET[(((a & 0x03) << 4) | (b >> 4)) as usize];
        output[target + 2] = if chunk.len() > 1 {
            ALPHABET[(((b & 0x0F) << 2) | (c >> 6)) as usize]
        } else {
            b'='
        };
        output[target + 3] = if chunk.len() > 2 {
            ALPHABET[(c & 0x3F) as usize]
        } else {
            b'='
        };
        target += 4;
    }
    target
}

/// FNV-1a over little-endian halfwords; an odd trailing byte is ignored.
pub(crate) fn fnv16_bytes(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for pair in bytes.chunks_exact(2) {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        hash = (hash ^ value as u32).wrapping_mul(0x0100_0193);
    }
    hash
}

/// FNV-1a over words, low halfword first, so it agrees with `fnv16_bytes`
/// over the same memory.
pub(crate) fn fnv32_words(words: &[u32]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &word in words {
        hash = (hash ^ (word & 0xFFFF)).wrapping_mul(0x0100_0193);
        hash = (hash ^ (word >> 16)).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Whether module (x, y) is dark in a QR bitmap packed one bit per module,
/// LSB first, in rows of `stride` modules.
pub(crate) fn qr_module(modules: &[u8], stride: usize, x: usize, y: usize) -> bool {
    let bit = y * stride + x;
    modules[bit / 8] & (1 << (bit & 7)) != 0
}

/// Quiet zone, in modules, on every side of a drawn symbol.
pub(crate) const QR_QUIET: i16 = 4;

/// Draw a `size` x `size` symbol from a bitmap packed in rows of `stride`,
/// centred horizontally at `top`: a white field, then one black rectangle per
/// horizontal run of dark modules.
pub(crate) fn draw_qr(modules: &[u8], stride: usize, size: usize, top: i16, scale: i16) {
    let total = (size as i16 + QR_QUIET * 2) * scale;
    let left = (320 - total) / 2;
    psx_gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
    let data_left = left + QR_QUIET * scale;
    let data_top = top + QR_QUIET * scale;
    for y in 0..size {
        let mut x = 0usize;
        while x < size {
            while x < size && !qr_module(modules, stride, x, y) {
                x += 1;
            }
            let first = x;
            while x < size && qr_module(modules, stride, x, y) {
                x += 1;
            }
            if first < x {
                psx_gpu::draw_rect_flat(
                    data_left + first as i16 * scale,
                    data_top + y as i16 * scale,
                    ((x - first) as i16 * scale) as u16,
                    scale as u16,
                    0,
                    0,
                    0,
                );
            }
        }
    }
}
