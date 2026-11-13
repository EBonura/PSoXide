// SPDX-License-Identifier: GPL-2.0-or-later
//! The boot splash every PSoXide game opens with: the Bonnie Studios mark above one line of
//! text, faded in, held and faded out, the line carrying a sweeping sheen.
//!
//! A game calls [`show`] once at boot with the line, for example `"Built with PSoXide"`.
//! Nothing else about the
//! splash is a parameter: the timing, the picture, the colours and the skip rule are the
//! same in every game, so the frames are too.
//!
//! * **Timing.** 150 frames: 32 fading in, 74 held, 44 fading out, one frame per VBlank. The
//!   presentation cap is lifted for the splash and put back when it ends, so the length is
//!   the same under any cap.
//! * **Skip.** A fresh press of Cross, Circle or Start, after the first 8 frames, ends it.
//!   The splash reads the pad only through the closure the caller gives it, so a game
//!   passes whatever it polls its pads with (`psx_pad::Controllers`) and the splash has no
//!   way of reading a pad of its own.
//! * **Brightness.** The player's BRIGHTNESS step is applied over every frame, faded with
//!   the picture so the fade ends black.
//! * **Pay for use.** The mark (4,692 bytes, run-length coded), its decoder and the text
//!   code are linked only into a program that calls [`show`]. No heap, no cargo feature.
//!
//! # What it needs
//!
//! A 320x240 [`DoubleBuffer`] and the [`Gpu`], both already set up by the caller; the pad
//! and the VBlank counter (`psx_rt::interrupts::install_vblank_counter`) are the caller's too.
//! [`show`] leaves the display on the buffer it last drew, 2/128 of full brightness, which
//! is black to the eye, and does not clear it.
//!
//! # VRAM it uses and leaves behind
//!
//! | what | where |
//! |---|---|
//! | the mark, 4 bits a pixel | texture page x 896..928, y 0..128 |
//! | the mark's palette | x 768..784, y 256 |
//! | the 8x8 `BASIC` font | texture page x 320 (an atlas of 256 by 32 texels), palette at x 320, y 256 |
//!
//! Nothing is put back afterwards; a game uploads its own textures over these columns, as
//! the hand-written splashes it replaces did.
//!
//! ```ignore
//! let mut pads = psx_pad::Controllers::new();
//! psx_splash::show(
//!     &mut gpu,
//!     &mut buffers,
//!     "Built with PSoXide",
//!     settings.brightness,
//!     || pads.poll(&mut port, Port::One).buttons,
//! );
//! ```

#![no_std]
#![cfg_attr(target_arch = "mips", feature(optimize_attribute))]
#![warn(missing_docs)]

mod logo;

use psx_display::Brightness;
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DoubleBuffer, Resolution};
use psx_gpu::material::TextureMaterial;
use psx_gpu::prim::QuadTexturedMaterial;
use psx_gpu::Gpu;
use psx_pad::{button, ButtonState};
use psx_vram::{upload_16bpp, Clut, TextureDepth, TexturePage, VramRect};

const MARK_TPAGE: TexturePage = TexturePage::new(896, 0, TextureDepth::Bit4);
const MARK_CLUT: Clut = Clut::new(768, 256);
const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

/// Frames of fade in, hold and fade out, and the whole.
const FADE_IN: i32 = 32;
const HOLD: i32 = 74;
const TOTAL: i32 = 150;
const FADE_OUT: i32 = TOTAL - FADE_IN - HOLD;
/// Frames before a press can skip, so a button still down from boot does not.
const SKIP_GRACE: i32 = 8;
/// Buttons that skip.
const SKIP: u16 = button::CROSS | button::CIRCLE | button::START;
/// Baseline of the line, and half the sheen's width in letters.
const LINE_Y: i16 = 150;
const SHEEN: i32 = 18;

/// Play the splash with `line` under the mark, and return when it has run or been skipped.
///
/// `buttons` is called once per frame and returns the pad's buttons as the caller reads
/// them. `brightness` is the player's setting (`Brightness::DEFAULT` draws nothing extra).
/// `buffers` must be a 320x240 pair.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
pub fn show(
    gpu: &mut Gpu,
    buffers: &mut DoubleBuffer,
    line: &str,
    brightness: Brightness,
    mut buttons: impl FnMut() -> ButtonState,
) {
    debug_assert_eq!(
        buffers.size(),
        (320, 240),
        "the splash is laid out for 320x240"
    );
    upload_mark();
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    let mark = TextureMaterial::opaque(MARK_CLUT.uv_word(), MARK_TPAGE.uv_word(0), (0, 0, 0));
    let x0 = 160 - (font.text_width(line) / 2) as i16;
    let span = line.chars().count() as i32 + SHEEN;

    // One frame per VBlank whatever the presentation cap is: 150 frames are 2.5 seconds.
    let cap = psx_rt::interrupts::min_flip_vblanks();
    psx_rt::interrupts::set_min_flip_vblanks(1);
    let skips = |b: ButtonState| b.is_held(SKIP);
    let mut previous = buttons();
    let mut frame = 0;
    while frame < TOTAL {
        let now = buttons();
        if frame > SKIP_GRACE && skips(now) && !skips(previous) {
            break;
        }
        previous = now;
        let level = if frame < FADE_IN {
            frame * 0x80 / FADE_IN
        } else if frame < FADE_IN + HOLD {
            0x80
        } else {
            (TOTAL - frame) * 0x80 / FADE_OUT
        }
        .clamp(0, 0x80);
        let grey = level as u8;

        buffers.clear(gpu, (0, 0, 0));
        // The 128x128 mark drawn 96x96, centred above the line.
        gpu.draw(&QuadTexturedMaterial::with_material(
            [(112, 34), (208, 34), (112, 130), (208, 130)],
            [(0, 0), (128, 0), (0, 128), (128, 128)],
            mark.with_tint((grey, grey, grey)),
        ));
        draw_line(&font, x0, line, (frame / 2).rem_euclid(span), level);
        if let Some(overlay) = brightness.faded_overlay(Resolution::R320X240, grey) {
            gpu.draw(&overlay);
        }
        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        buffers.swap(gpu);
        frame += 1;
    }
    psx_rt::interrupts::set_min_flip_vblanks(cap);
}

/// Expand the mark into VRAM eight rows at a time (512 bytes of stack, once), and put its
/// palette beside it. Entry 0 of the palette is opaque near-black: the backdrop is black and
/// 0x0000 is transparent.
fn upload_mark() {
    const ROWS: u16 = 8;
    let mut words = logo::expand(&logo::CODED);
    let (x, y) = (MARK_TPAGE.x(), MARK_TPAGE.y());
    for band in 0..(logo::WORDS / 32) as u16 / ROWS {
        let mut pixels = [0u16; 32 * ROWS as usize];
        for slot in &mut pixels {
            *slot = words.next().unwrap_or(0);
        }
        upload_16bpp(VramRect::new(x, y + band * ROWS, 32, ROWS), &pixels);
    }
    let mut palette = logo::PALETTE;
    palette[0] = 0x0421;
    upload_16bpp(VramRect::new(MARK_CLUT.x(), MARK_CLUT.y(), 16, 1), &palette);
}

/// The line: a black outline, then each letter as a gradient whose brightness rises toward
/// the sheen's head, all scaled by the fade `level` (0 to 0x80).
fn draw_line(font: &FontAtlas, mut x: i16, line: &str, head: i32, level: i32) {
    let mix = |colour: (u8, u8, u8), near: i32| -> (u8, u8, u8) {
        let channel = |value: u8| {
            let base = i32::from(value) * level / 0x80;
            (base + (level - base) * near / SHEEN) as u8
        };
        (channel(colour.0), channel(colour.1), channel(colour.2))
    };
    for (index, ch) in line.char_indices() {
        let glyph = &line[index..index + ch.len_utf8()];
        for (dx, dy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
            font.draw_text(x + dx, LINE_Y + dy, glyph, (0, 0, 0));
        }
        let near = (SHEEN - (index as i32 - head).abs() * 6).max(0);
        font.draw_text_gradient(
            x,
            LINE_Y,
            glyph,
            mix((0x68, 0x80, 0x80), near),
            mix((0x38, 0x58, 0x80), near),
        );
        x += font.text_width(glyph) as i16;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn fnv1a(words: impl Iterator<Item = u16>) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for word in words {
            for byte in word.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn the_mark_expands_to_its_4096_words() {
        assert_eq!(logo::expand(&logo::CODED).count(), logo::WORDS);
        assert_eq!(fnv1a(logo::expand(&logo::CODED)), logo::FNV1A);
    }

    #[test]
    fn runs_and_literals_expand_in_order() {
        // 3 x 0x0000, literal 0x1234 0x5678, 4 x 0xBEEF.
        let coded = [
            0x80, 0x00, 0x00, 0x01, 0x34, 0x12, 0x78, 0x56, 0x81, 0xef, 0xbe,
        ];
        let words: std::vec::Vec<u16> = logo::expand(&coded).collect();
        assert_eq!(
            words,
            [0, 0, 0, 0x1234, 0x5678, 0xbeef, 0xbeef, 0xbeef, 0xbeef]
        );
    }

    #[test]
    fn a_truncated_stream_stops_without_panicking() {
        assert_eq!(logo::expand(&[0x81, 0xef]).count(), 0);
        assert_eq!(logo::expand(&[0x01, 0x34, 0x12]).count(), 1);
        assert_eq!(logo::expand(&[]).count(), 0);
    }

    #[test]
    fn the_fade_ends_at_two_128ths() {
        let last = (TOTAL - (TOTAL - 1)) * 0x80 / FADE_OUT;
        assert_eq!(last, 2);
        assert_eq!(FADE_IN + HOLD + FADE_OUT, TOTAL);
    }
}
