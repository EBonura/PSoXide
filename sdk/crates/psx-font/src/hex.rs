//! Heap-free hex label formatting for debug HUDs.
//!
//! `no_std` guest code cannot reach for `format!`, so HUD overlays
//! that want to print a counter next to a label need a tiny
//! stack-buffer formatter. [`u16_hex`] renders a `u16` as `0xABCD`
//! into a [`HexU16`] whose [`as_str`](HexU16::as_str) feeds straight
//! into [`FontAtlas::draw_text`](crate::FontAtlas::draw_text).

/// A `u16` formatted as `0xABCD`, ready to draw.
#[derive(Copy, Clone)]
pub struct HexU16([u8; 6]);

impl HexU16 {
    /// The formatted text, e.g. `"0x1F40"`.
    pub fn as_str(&self) -> &str {
        // SAFETY: all bytes are ASCII ('0', 'x', hex digits).
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

/// Format a `u16` as a fixed-width `0xABCD` hex label.
pub fn u16_hex(v: u16) -> HexU16 {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = [0u8; 6];
    out[0] = b'0';
    out[1] = b'x';
    out[2] = HEX[((v >> 12) & 0xF) as usize];
    out[3] = HEX[((v >> 8) & 0xF) as usize];
    out[4] = HEX[((v >> 4) & 0xF) as usize];
    out[5] = HEX[(v & 0xF) as usize];
    HexU16(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_fixed_width_uppercase() {
        assert_eq!(u16_hex(0).as_str(), "0x0000");
        assert_eq!(u16_hex(0x1F40).as_str(), "0x1F40");
        assert_eq!(u16_hex(u16::MAX).as_str(), "0xFFFF");
    }
}
