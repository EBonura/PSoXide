// SPDX-License-Identifier: GPL-2.0-or-later
//! The BRIGHTNESS option: a signed step around the picture as drawn.
//!
//! The row reads `DEFAULT` at the centre, `DARKER 1` to `DARKER 5` to the left
//! and `BRIGHTER 1` to `BRIGHTER 5` to the right. The GPU has no gamma, so a
//! step is applied as one semi-transparent grey rectangle over the finished
//! frame ([`BrightnessOverlay`]): subtracted to darken, added to brighten. At
//! the centre nothing is drawn, so the default frame is the frame as drawn and
//! the setting costs nothing.

use psx_gpu::display::Resolution;
use psx_gpu::prim::GpuPacket;
use psx_hw::gpu::{gp0, packet};

use crate::label::Label;

/// Grey (of 255) the overlay carries per step, either way. The GPU blends in
/// five bits per channel and a rectangle's grey is its top five bits, so 8 is
/// the smallest grey that changes the picture by one blend unit: every step
/// moves it by exactly one more than the step before.
const GAIN_PER_STEP: u8 = 8;

/// GP0(E1h) semi-transparency field value for `background + foreground`.
const BLEND_ADD: u32 = 1;
/// GP0(E1h) semi-transparency field value for `background - foreground`.
const BLEND_SUBTRACT: u32 = 2;

/// The BRIGHTNESS setting: [`Brightness::MIN`] to [`Brightness::MAX`], where
/// [`Brightness::DEFAULT`] (zero) is the picture as drawn.
///
/// ```
/// use psx_display::Brightness;
/// let mut brightness = Brightness::DEFAULT;
/// brightness = brightness.stepped(1);
/// assert_eq!(brightness.label().as_str(), "BRIGHTER 1");
/// brightness = brightness.stepped(-3);
/// assert_eq!(brightness.label().as_str(), "DARKER 2");
/// ```
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Brightness(i8);

impl Brightness {
    /// Steps either side of the picture as drawn.
    pub const STEP_COUNT: i8 = 5;
    /// The darkest step.
    pub const MIN: Self = Self(-Self::STEP_COUNT);
    /// The brightest step.
    pub const MAX: Self = Self(Self::STEP_COUNT);
    /// The picture as drawn.
    pub const DEFAULT: Self = Self(0);

    /// The step `level`: negative is darker, positive brighter. A level
    /// outside [`MIN`](Self::MIN) to [`MAX`](Self::MAX) is clamped.
    pub const fn new(level: i8) -> Self {
        if level < -Self::STEP_COUNT {
            Self::MIN
        } else if level > Self::STEP_COUNT {
            Self::MAX
        } else {
            Self(level)
        }
    }

    /// The signed step: negative darker, positive brighter, zero as drawn.
    pub const fn level(self) -> i8 {
        self.0
    }

    /// Whether this is the picture as drawn, which needs no overlay.
    pub const fn is_default(self) -> bool {
        self.0 == 0
    }

    /// This setting moved by `delta` steps (left is negative, right
    /// positive), stopping at the ends instead of wrapping.
    pub const fn stepped(self, delta: i8) -> Self {
        let level = self.0 as i16 + delta as i16;
        if level < -(Self::STEP_COUNT as i16) {
            Self::MIN
        } else if level > Self::STEP_COUNT as i16 {
            Self::MAX
        } else {
            Self(level as i8)
        }
    }

    /// What the options row shows: `DEFAULT`, `DARKER n` or `BRIGHTER n`.
    pub const fn label(self) -> Label {
        Self::label_for_level(self.0)
    }

    /// The row text for any signed `level`, with no clamp. A game whose own
    /// brightness steps are not five either side (a palette with its own
    /// count of gamma rows) keeps its range and its application and shows
    /// the same words through this.
    pub const fn label_for_level(level: i8) -> Label {
        if level == 0 {
            Label::word(b"DEFAULT")
        } else if level < 0 {
            Label::toward(b"DARKER", level.unsigned_abs())
        } else {
            Label::toward(b"BRIGHTER", level.unsigned_abs())
        }
    }

    /// The save byte: the step in two's complement, so zero (the default) is
    /// also what a save from before the setting reads as.
    pub const fn to_byte(self) -> u8 {
        self.0 as u8
    }

    /// The setting a save byte holds. Any byte decodes: one outside the
    /// range is clamped to the nearest end.
    pub const fn from_byte(byte: u8) -> Self {
        Self::new(byte as i8)
    }

    /// Grey of 255 the overlay carries: zero at the default, else the step
    /// count times 8, darker or brighter. On the GPU's five bits that is
    /// exactly the step count (1 to 5) subtracted or added per channel.
    pub const fn overlay_gain(self) -> u8 {
        self.0.unsigned_abs() * GAIN_PER_STEP
    }

    /// The overlay grey while a screen fades: [`overlay_gain`](Self::overlay_gain)
    /// scaled by `fade`, where 128 is the screen at full strength. A fade to
    /// black then still ends black instead of a lifted grey.
    pub const fn faded_gain(self, fade: u8) -> u8 {
        let grey = self.overlay_gain() as u32 * fade as u32 / 128;
        if grey > 255 {
            255
        } else {
            grey as u8
        }
    }

    /// The overlay for a `resolution` frame, or `None` at the default (no
    /// packet, no GPU work, no per-frame cost).
    pub const fn overlay(self, resolution: Resolution) -> Option<BrightnessOverlay> {
        if self.is_default() {
            None
        } else {
            Some(BrightnessOverlay::new(self, resolution))
        }
    }

    /// The overlay for a screen mid-fade (see [`faded_gain`](Self::faded_gain)),
    /// or `None` when the setting is at the default or the fade has taken the
    /// grey to nothing.
    pub const fn faded_overlay(
        self,
        resolution: Resolution,
        fade: u8,
    ) -> Option<BrightnessOverlay> {
        let grey = self.faded_gain(fade);
        if grey == 0 {
            None
        } else {
            Some(BrightnessOverlay::with_grey(
                self.level() > 0,
                grey,
                resolution,
            ))
        }
    }
}

/// The overlay packet: a draw-mode word and one semi-transparent grey
/// rectangle (GP0 62h) covering the frame, as one linked-list node.
///
/// Link it into slot 0 of the frame's ordering table **before** anything
/// else is inserted there. An insert prepends and slot 0 is walked last, so
/// the first node in is the last drawn, and the overlay then lifts or dims
/// the HUD and menus along with the scene. The draw area and offset must
/// cover the whole frame when it runs (a split screen widens them first); its
/// rectangle starts at the draw offset's origin.
///
/// It leaves GP0(E1h) set to its own semi-transparent mode with dithering
/// off, as the last node of a frame; the next frame's first packets set their
/// own draw mode.
///
/// ```
/// use psx_display::Brightness;
/// use psx_gpu::display::Resolution;
/// let overlay = Brightness::new(-2).overlay(Resolution::R320X240).unwrap();
/// assert_eq!(overlay.color_command & 0x00FF_FFFF, 16 * 0x0001_0101);
/// assert!(Brightness::DEFAULT.overlay(Resolution::R320X240).is_none());
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(C, align(4))]
pub struct BrightnessOverlay {
    /// OT linkage.
    pub tag: u32,
    /// GP0(E1h) draw mode selecting `B + F` (brighter) or `B - F` (darker).
    pub draw_mode: u32,
    /// Semi-transparent flat rectangle opcode and the grey, `0x62gggggg`.
    pub color_command: u32,
    /// Top-left corner, relative to the draw offset.
    pub origin: u32,
    /// Width and height.
    pub size: u32,
}

impl BrightnessOverlay {
    /// Data-word count after the tag.
    pub const WORDS: u8 = 4;

    /// The overlay for `brightness` over a `resolution` frame. At
    /// [`Brightness::DEFAULT`] the gain is zero and the packet changes
    /// nothing, but prefer [`Brightness::overlay`], which gives no packet.
    pub const fn new(brightness: Brightness, resolution: Resolution) -> Self {
        Self::with_grey(
            brightness.level() > 0,
            brightness.overlay_gain(),
            resolution,
        )
    }

    const fn with_grey(brighter: bool, grey: u8, resolution: Resolution) -> Self {
        let blend = if brighter { BLEND_ADD } else { BLEND_SUBTRACT };
        let grey = grey as u32;
        Self {
            tag: 0,
            draw_mode: gp0::draw_mode(0, 0, blend, 0, false, true),
            color_command: packet::FLAT_RECT | packet::SEMI_TRANSPARENT | (grey * 0x0001_0101),
            origin: 0,
            size: ((resolution.height() as u32) << 16) | resolution.width() as u32,
        }
    }

    /// The four payload words in the order the GPU reads them, for a caller
    /// that copies packets into its own arena instead of linking this one.
    pub const fn words(&self) -> [u32; Self::WORDS as usize] {
        [self.draw_mode, self.color_command, self.origin, self.size]
    }
}

// SAFETY: `repr(C, align(4))`, a `u32` tag first, then `WORDS` (four) payload
// words of plain `u32` data, no interior mutability.
unsafe impl GpuPacket for BrightnessOverlay {
    const WORDS: u8 = BrightnessOverlay::WORDS;
}

const _: () = {
    assert!(
        core::mem::size_of::<BrightnessOverlay>() >= 4 * (1 + BrightnessOverlay::WORDS as usize)
    );
    assert!(core::mem::align_of::<BrightnessOverlay>() >= 4);
};

#[cfg(test)]
mod tests {
    use super::{Brightness, BrightnessOverlay};
    use psx_gpu::display::Resolution;

    /// The grey of step `n` either way, as the overlay states it.
    fn grey(n: i8) -> u32 {
        n.unsigned_abs() as u32 * 8
    }

    /// The four packet words for a 320x240 frame: the draw-mode word (GP0
    /// E1h with B + F or B - F, and bit 10 set so an interlaced frame can draw
    /// into the field being shown), the rectangle command, its corner and its
    /// size.
    fn words(level: i8) -> [u32; 4] {
        let mode = if level > 0 { 0xE100_0420 } else { 0xE100_0440 };
        [
            mode,
            0x6200_0000 | (grey(level) * 0x0001_0101),
            0,
            320 | (240 << 16),
        ]
    }

    #[test]
    fn the_gain_table_is_eight_grey_per_step_both_ways() {
        let table = [0, 8, 16, 24, 32, 40];
        for steps in 0..=5i8 {
            assert_eq!(
                Brightness::new(-steps).overlay_gain(),
                table[steps as usize]
            );
            assert_eq!(Brightness::new(steps).overlay_gain(), table[steps as usize]);
        }
    }

    #[test]
    fn the_overlay_words_are_pinned_for_every_step() {
        for level in -5..=5i8 {
            if level == 0 {
                continue;
            }
            let overlay = Brightness::new(level)
                .overlay(Resolution::R320X240)
                .unwrap();
            assert_eq!(overlay.words(), words(level), "level {level}");
        }
        // Spelled out for the two ends.
        assert_eq!(
            Brightness::MIN
                .overlay(Resolution::R320X240)
                .unwrap()
                .words(),
            [0xE100_0440, 0x6228_2828, 0, 0x00F0_0140]
        );
        assert_eq!(
            Brightness::MAX
                .overlay(Resolution::R320X240)
                .unwrap()
                .words(),
            [0xE100_0420, 0x6228_2828, 0, 0x00F0_0140]
        );
    }

    /// The brighter words are the ones WipEout always drew (its draw-mode word
    /// leaves bit 10 clear: the GPU ignores it for progressive frames).
    #[test]
    fn the_brighter_words_are_wipeouts() {
        for level in 1..=5i8 {
            let mut w = Brightness::new(level)
                .overlay(Resolution::R320X240)
                .unwrap()
                .words();
            w[0] &= !(1 << 10);
            assert_eq!(
                w,
                [
                    0xE100_0020,
                    0x6200_0000 | (level as u32 * 8 * 0x0001_0101),
                    0,
                    320 | (240 << 16)
                ]
            );
        }
    }

    /// Hollow Knight's fade rule: step count times the per-step grey, times
    /// the screen's gain over 128, capped at 255.
    #[test]
    fn the_faded_gain_is_hollow_knights() {
        for level in -5..=5i8 {
            let per = 8;
            for fade in 0..=u8::MAX {
                let want = (level.unsigned_abs() as u32 * per * fade as u32 / 128).min(255) as u8;
                assert_eq!(
                    Brightness::new(level).faded_gain(fade),
                    want,
                    "{level} {fade}"
                );
            }
        }
        let full = Brightness::new(3)
            .faded_overlay(Resolution::R320X240, 128)
            .unwrap();
        assert_eq!(
            full,
            Brightness::new(3).overlay(Resolution::R320X240).unwrap()
        );
        assert!(Brightness::new(3)
            .faded_overlay(Resolution::R320X240, 0)
            .is_none());
        assert!(Brightness::DEFAULT
            .faded_overlay(Resolution::R320X240, 128)
            .is_none());
        // Half faded, darker: half the grey, still subtracting.
        let half = Brightness::new(-4)
            .faded_overlay(Resolution::R320X240, 64)
            .unwrap();
        assert_eq!(half.color_command, 0x6200_0000 | (16 * 0x0001_0101));
        assert_eq!(half.draw_mode, full.draw_mode ^ 0x0000_0060);
    }

    #[test]
    fn the_default_has_no_overlay() {
        assert!(Brightness::DEFAULT.overlay(Resolution::R320X240).is_none());
        assert_eq!(Brightness::DEFAULT.overlay_gain(), 0);
        assert!(Brightness::default().is_default());
    }

    #[test]
    fn the_packet_is_one_node_of_five_words() {
        assert_eq!(core::mem::size_of::<BrightnessOverlay>(), 20);
        assert_eq!(core::mem::align_of::<BrightnessOverlay>(), 4);
        let overlay = BrightnessOverlay::new(Brightness::new(1), Resolution::R256X240);
        assert_eq!(overlay.tag, 0);
        assert_eq!(overlay.size, 256 | (240 << 16));
    }

    #[test]
    fn labels_read_default_in_the_middle() {
        let want = [
            "DARKER 5",
            "DARKER 4",
            "DARKER 3",
            "DARKER 2",
            "DARKER 1",
            "DEFAULT",
            "BRIGHTER 1",
            "BRIGHTER 2",
            "BRIGHTER 3",
            "BRIGHTER 4",
            "BRIGHTER 5",
        ];
        for (at, want) in want.iter().enumerate() {
            let level = at as i8 - 5;
            assert_eq!(Brightness::new(level).label().as_str(), *want);
            assert_eq!(Brightness::label_for_level(level).as_str(), *want);
        }
        assert_eq!(Brightness::label_for_level(6).as_str(), "BRIGHTER 6");
        assert_eq!(Brightness::label_for_level(-1).as_str(), "DARKER 1");
    }

    #[test]
    fn stepping_clamps_at_both_ends() {
        let mut brightness = Brightness::DEFAULT;
        for _ in 0..20 {
            brightness = brightness.stepped(1);
        }
        assert_eq!(brightness, Brightness::MAX);
        for _ in 0..20 {
            brightness = brightness.stepped(-1);
        }
        assert_eq!(brightness, Brightness::MIN);
        assert_eq!(Brightness::MIN.stepped(i8::MIN), Brightness::MIN);
        assert_eq!(Brightness::MAX.stepped(i8::MAX), Brightness::MAX);
        assert_eq!(Brightness::new(i8::MIN), Brightness::MIN);
        assert_eq!(Brightness::new(i8::MAX), Brightness::MAX);
        assert_eq!(Brightness::new(2).stepped(-2), Brightness::DEFAULT);
    }

    #[test]
    fn the_save_byte_is_wipeouts_and_survives_any_byte() {
        for level in -5..=5i8 {
            // WipEout saves `brightness as u8` and reads `(v as i8).clamp`.
            assert_eq!(Brightness::new(level).to_byte(), level as u8);
            assert_eq!(Brightness::from_byte(level as u8).level(), level);
        }
        assert_eq!(Brightness::DEFAULT.to_byte(), 0);
        for byte in 0..=u8::MAX {
            let level = (byte as i8).clamp(-5, 5);
            assert_eq!(Brightness::from_byte(byte).level(), level, "byte {byte}");
        }
    }

    /// The GPU blends in five bits per channel and a rectangle's grey is its
    /// top five bits (`>> 3`, no dither on a rectangle; the emulator's
    /// `blend_pixel` and `rgb24_to_bgr15` do the same), so what a step takes
    /// off or adds to the picture is the grey `>> 3`. Every step must be a
    /// different amount from its neighbours, the same either way: a smaller
    /// per-step grey leaves dead or repeated steps.
    #[test]
    fn every_step_is_a_distinct_five_bit_amount() {
        let five_bit = |level: i8| Brightness::new(level).overlay_gain() >> 3;
        assert_eq!([-1, -2, -3, -4, -5].map(five_bit), [1, 2, 3, 4, 5]);
        assert_eq!([1, 2, 3, 4, 5].map(five_bit), [1, 2, 3, 4, 5]);
        for level in -4..=4i8 {
            let here = five_bit(level);
            let next = five_bit(level + level.signum());
            if level != 0 {
                assert_eq!(next, here + 1, "level {level}");
            }
        }
        assert_eq!(five_bit(0), 0);
        assert_eq!(five_bit(1), 1);
        assert_eq!(five_bit(-1), 1);
    }
}
