// SPDX-License-Identifier: GPL-2.0-or-later
//! The SCREEN X and SCREEN Y options: where the picture sits on the
//! television.
//!
//! A CRT's overscan differs from set to set, so the picture is moved in the
//! video signal (the GPU's display range, which [`DisplayConfig`] already
//! writes), not in VRAM: nothing is cropped and no draw call changes. The rows
//! read `CENTRE` at zero, `LEFT n` / `RIGHT n` for X and `UP n` / `DOWN n`
//! for Y, in pixels.

use psx_gpu::display::DisplayConfig;

use crate::label::Label;

/// The picture position: pixels right of and below centre, each within
/// `-RANGE..=RANGE`. `RANGE` defaults to 16, WipEout's and Hollow Knight's.
///
/// ```
/// use psx_display::ScreenOffset;
/// let offset = ScreenOffset::<16>::CENTRE.stepped_x(-3).stepped_y(2);
/// assert_eq!(offset.label_x().as_str(), "LEFT 3");
/// assert_eq!(offset.label_y().as_str(), "DOWN 2");
/// assert_eq!(offset.pixels(), (-3, 2));
/// ```
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScreenOffset<const RANGE: i8 = 16> {
    x: i8,
    y: i8,
}

impl<const RANGE: i8> ScreenOffset<RANGE> {
    /// The standard, centred picture.
    pub const CENTRE: Self = Self { x: 0, y: 0 };

    /// The offset `x` pixels right and `y` down, each clamped to the range.
    pub const fn new(x: i8, y: i8) -> Self {
        Self {
            x: clamp(x, RANGE),
            y: clamp(y, RANGE),
        }
    }

    /// Whether the picture is centred.
    pub const fn is_centre(self) -> bool {
        self.x == 0 && self.y == 0
    }

    /// Pixels right of centre (negative is left).
    pub const fn x(self) -> i8 {
        self.x
    }

    /// Pixels below centre (negative is up).
    pub const fn y(self) -> i8 {
        self.y
    }

    /// This offset with X moved by `delta` pixels, stopping at the ends.
    pub const fn stepped_x(self, delta: i8) -> Self {
        Self::new(add(self.x, delta), self.y)
    }

    /// This offset with Y moved by `delta` pixels, stopping at the ends.
    pub const fn stepped_y(self, delta: i8) -> Self {
        Self::new(self.x, add(self.y, delta))
    }

    /// What the SCREEN X row shows: `CENTRE`, `LEFT n` or `RIGHT n`.
    pub const fn label_x(self) -> Label {
        if self.x == 0 {
            Label::word(b"CENTRE")
        } else if self.x < 0 {
            Label::toward(b"LEFT", self.x.unsigned_abs())
        } else {
            Label::toward(b"RIGHT", self.x.unsigned_abs())
        }
    }

    /// What the SCREEN Y row shows: `CENTRE`, `UP n` or `DOWN n`.
    pub const fn label_y(self) -> Label {
        if self.y == 0 {
            Label::word(b"CENTRE")
        } else if self.y < 0 {
            Label::toward(b"UP", self.y.unsigned_abs())
        } else {
            Label::toward(b"DOWN", self.y.unsigned_abs())
        }
    }

    /// The save bytes, X then Y, each in two's complement; zero is centred,
    /// which is also what a save from before the setting reads as.
    pub const fn to_bytes(self) -> [u8; 2] {
        [self.x as u8, self.y as u8]
    }

    /// The offset two save bytes hold. Any bytes decode: a value outside the
    /// range is clamped to the nearest end.
    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self::new(bytes[0] as i8, bytes[1] as i8)
    }

    /// The offset as `(x, y)` pixels, the form [`DisplayConfig::with_offset`]
    /// takes.
    pub const fn pixels(self) -> (i16, i16) {
        (self.x as i16, self.y as i16)
    }

    /// `display` with the picture moved to this offset. Hand the result to
    /// the GPU's display setter to apply it at once; it is a video-signal
    /// change and costs nothing per frame.
    pub const fn apply_to(self, display: DisplayConfig) -> DisplayConfig {
        display.with_offset(self.pixels())
    }
}

const fn clamp(value: i8, range: i8) -> i8 {
    let range = if range < 0 { 0 } else { range };
    if value < -range {
        -range
    } else if value > range {
        range
    } else {
        value
    }
}

const fn add(value: i8, delta: i8) -> i8 {
    let sum = value as i16 + delta as i16;
    if sum < i8::MIN as i16 {
        i8::MIN
    } else if sum > i8::MAX as i16 {
        i8::MAX
    } else {
        sum as i8
    }
}

#[cfg(test)]
mod tests {
    use super::ScreenOffset;
    use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};

    #[test]
    fn labels_read_centre_at_zero() {
        let offset = ScreenOffset::<16>::CENTRE;
        assert_eq!(offset.label_x().as_str(), "CENTRE");
        assert_eq!(offset.label_y().as_str(), "CENTRE");
        assert!(offset.is_centre());
        let offset = ScreenOffset::<16>::new(16, -16);
        assert_eq!(offset.label_x().as_str(), "RIGHT 16");
        assert_eq!(offset.label_y().as_str(), "UP 16");
        let offset = ScreenOffset::<16>::new(-1, 1);
        assert_eq!(offset.label_x().as_str(), "LEFT 1");
        assert_eq!(offset.label_y().as_str(), "DOWN 1");
    }

    #[test]
    fn stepping_and_new_clamp_to_the_range() {
        let mut offset = ScreenOffset::<16>::CENTRE;
        for _ in 0..40 {
            offset = offset.stepped_x(1).stepped_y(-1);
        }
        assert_eq!((offset.x(), offset.y()), (16, -16));
        assert_eq!(
            ScreenOffset::<16>::new(i8::MAX, i8::MIN).pixels(),
            (16, -16)
        );
        assert_eq!(ScreenOffset::<16>::new(0, 0).stepped_x(i8::MAX).x(), 16);
        assert_eq!(ScreenOffset::<24>::new(100, -100).pixels(), (24, -24));
        assert_eq!(ScreenOffset::<0>::new(5, 5), ScreenOffset::<0>::CENTRE);
    }

    #[test]
    fn the_save_bytes_match_wipeouts_and_survive_any_byte() {
        for x in -16..=16i8 {
            for y in [-16, -1, 0, 1, 16] {
                let offset = ScreenOffset::<16>::new(x, y);
                // WipEout saves `screen_x as u8` then `screen_y as u8`.
                assert_eq!(offset.to_bytes(), [x as u8, y as u8]);
                assert_eq!(ScreenOffset::<16>::from_bytes(offset.to_bytes()), offset);
            }
        }
        for byte in 0..=u8::MAX {
            let want = (byte as i8).clamp(-16, 16);
            assert_eq!(
                ScreenOffset::<16>::from_bytes([byte, byte]).pixels(),
                (want as i16, want as i16)
            );
        }
    }

    #[test]
    fn the_offset_moves_the_display_config() {
        let display = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240);
        let moved = ScreenOffset::<16>::new(-4, 3).apply_to(display);
        assert_eq!(moved.offset, (-4, 3));
        assert_eq!(moved.resolution, display.resolution);
        assert_eq!(ScreenOffset::<16>::CENTRE.apply_to(display), display);
    }
}
