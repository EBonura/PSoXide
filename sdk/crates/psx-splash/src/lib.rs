// SPDX-License-Identifier: GPL-2.0-or-later
//! The boot splash every PSoXide game opens with: the Bonnie Studios mark above one line of
//! text, the mark rising into place with a glint crossing it, the line rising in under it with a
//! sweeping sheen, then all of it fading out. A game made with a partner can put the partner's
//! logo beside the mark and a second line under the first ([`show_with_partner`]).
//!
//! A game calls [`show`] once at boot with the line, for example `"Built with PSoXide"`.
//! Nothing else about the
//! splash is a parameter: the timing, the picture, the colours and the skip rule are the
//! same in every game, so the frames are too.
//!
//! * **Timing.** 150 frames, one frame per VBlank: the mark (and a partner's logo, 12 frames
//!   later, from the other side) eases in over 28 frames, the glint crosses from frame 46 to 86,
//!   the line rises in from frame 30 (a second line from 40), and the last 44 fade out. The
//!   presentation cap is lifted for the splash and put back when it ends, so the length is
//!   the same under any cap.
//! * **Skip.** A fresh press of Cross, Circle or Start, after the first 8 frames, ends it.
//!   The splash reads the pad only through the closure the caller gives it, so a game
//!   passes whatever it polls its pads with (`psx_pad::Controllers`) and the splash has no
//!   way of reading a pad of its own.
//! * **Brightness.** The player's BRIGHTNESS step is applied over every frame, faded with
//!   the picture so the fade ends black.
//! * **Pay for use.** The mark (2,746 bytes, LZ coded), its decoder and the text
//!   code are linked only into a program that calls [`show`]. No heap, no cargo feature.
//!
//! # What it needs
//!
//! A 320x240 [`DoubleBuffer`] and the [`Gpu`], both already set up by the caller; the pad
//! and the VBlank counter (`psx_rt::interrupts::install_vblank_counter`) are the caller's too.
//! The splash waits on the counter without installing it: a program that has not installed it
//! asserts in a debug build, and in a release build the wait gives up after its bound (about a
//! second) and the splash ends there instead of hanging. `psx-engine`'s `App` installs it
//! before the splash.
//! [`show`] leaves the display on the buffer it last drew, 2/128 of full brightness, which
//! is black to the eye, and does not clear it.
//!
//! # VRAM it uses and leaves behind
//!
//! | what | where |
//! |---|---|
//! | the mark, 4 bits a pixel | texture page x 896..928, y 0..128 |
//! | the mark's palette, and its copy with the semi-transparency bit (the glint) | x 768..800, y 256 |
//! | a partner's logo, 8 bits a pixel | texture page x 640, up to 127 words wide, y 0.. |
//! | the logo's palette and its glint copy | x 768..1024, y 258 and 259 |
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
use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::prim::{QuadTexturedGouraud, QuadTexturedMaterial};
use psx_gpu::Gpu;
use psx_pad::{button, ButtonState};
use psx_vram::{upload_16bpp, Clut, TextureDepth, TexturePage, VramRect};

const MARK_TPAGE: TexturePage = TexturePage::new(896, 0, TextureDepth::Bit4);
const MARK_CLUT: Clut = Clut::new(768, 256);
const MARK_GLINT_CLUT: Clut = Clut::new(784, 256);
const LOGO_TPAGE: TexturePage = TexturePage::new(640, 0, TextureDepth::Bit8);
const LOGO_CLUT: Clut = Clut::new(768, 258);
const LOGO_GLINT_CLUT: Clut = Clut::new(768, 259);
const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
/// The font as statics of this crate: a `const` the generic upload takes by reference is emitted as
/// anonymous data in the caller's crate, which no linker script can place by name (a boot overlay
/// that holds the splash cannot take it, and under fat LTO it stays resident).
// Boot-only data resident code may reach while the splash runs: with one caller, LTO folds the
// font's address into psx-font's resident `FontAtlas::upload`, and only the splash calls it with
// this font (psoxide-boot.ld refuses any resident reference into `.boot_inner`).
#[cfg_attr(
    target_arch = "mips",
    link_section = ".rodata.boot_only.psx_splash.FONT_BITMAP"
)]
static FONT_BITMAP: [u8; 1024] = psx_font::fonts::basic::BASIC_BITMAP;
#[cfg_attr(
    target_arch = "mips",
    link_section = ".rodata.boot_only.psx_splash.FONT"
)]
static FONT: psx_font::BitmapFont = psx_font::BitmapFont {
    bitmap: &FONT_BITMAP,
    ..BASIC
};

/// Frames in all, and fading out at the end.
const TOTAL: i32 = 150;
const FADE_OUT: i32 = 44;
/// Each element eases in over `ENTER` frames from its start, sliding `SLIDE` pixels (the lines
/// `RISE`); the glint crosses in `GLINT` frames.
const ENTER: i32 = 28;
const SLIDE: i32 = 40;
const RISE: i32 = 8;
const MARK_AT: i32 = 0;
const LOGO_AT: i32 = 12;
const LINE_AT: i32 = 30;
const SECOND_AT: i32 = 40;
const GLINT_AT: i32 = 46;
const GLINT: i32 = 40;
/// The glint's half width in pixels, its brightest tint, and the strips it is drawn in.
const GLINT_BAND: i32 = 22;
const GLINT_PEAK: i32 = 0x70;
const GLINT_STRIP: i16 = 8;
/// Frames before a press can skip, so a button still down from boot does not.
const SKIP_GRACE: i32 = 8;
/// Buttons that skip.
const SKIP: u16 = button::CROSS | button::CIRCLE | button::START;
/// The mark's size on screen; the gap beside a partner's logo, and above the lines; the gap
/// between two lines.
const MARK: i16 = 96;
const GAP: i16 = 24;
const LINES_GAP: i16 = 16;
const SECOND_GAP: i16 = 6;
/// Without a partner, the mark's top and the line's baseline (the layout of the first splash).
const SOLO_MARK_Y: i16 = 34;
const SOLO_LINE_Y: i16 = 150;
/// Half the sheen's width in letters.
const SHEEN: i32 = 18;
/// Reads of the VBlank count a frame's wait gives up after (psx-rt's own bound).
const VBLANK_SPINS: u32 = 4_000_000;

/// A partner credited on the splash: its logo beside the mark and a second line under the first.
/// Made with [`Partner::new`], so a field added later breaks no game.
#[non_exhaustive]
pub struct Partner<'a> {
    /// The second line, for example `"in collaboration with ..."`.
    pub line: &'a str,
    /// The logo's width in pixels: even, at most 254. It is drawn 1:1.
    pub width: u16,
    /// The logo's height in pixels, at most 200.
    pub height: u16,
    /// Its 256 colours (GPU 15-bit; 0x0000 is transparent, draw the background as 0x8000).
    pub palette: &'a [u16; 256],
    /// One palette index a pixel, rows top to bottom, two pixels a word (the left one low):
    /// `width / 2 * height` words.
    pub pixels: &'a [u16],
}

impl<'a> Partner<'a> {
    /// A partner with its second `line` and its logo: `width` by `height` pixels (even width, at
    /// most 254 by 200), its 256-colour `palette` and its `pixels`, one index a pixel, two a word.
    #[must_use]
    pub const fn new(
        line: &'a str,
        width: u16,
        height: u16,
        palette: &'a [u16; 256],
        pixels: &'a [u16],
    ) -> Self {
        Self {
            line,
            width,
            height,
            palette,
            pixels,
        }
    }
}

/// Play the splash with `line` under the mark, and return when it has run or been skipped.
///
/// `buttons` is called once per frame and returns the pad's buttons as the caller reads
/// them. `brightness` is the player's setting (`Brightness::DEFAULT` draws nothing extra).
/// `buffers` must be a 320x240 pair.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
#[cfg_attr(target_arch = "mips", link_section = ".text.boot_only.psx_splash.show")]
pub fn show(
    gpu: &mut Gpu,
    buffers: &mut DoubleBuffer,
    line: &str,
    brightness: Brightness,
    buttons: impl FnMut() -> ButtonState,
) {
    play(gpu, buffers, line, None, brightness, &mut { buttons });
}

/// [`show`] with a partner's logo beside the mark and its line under `line`.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_only.psx_splash.show_with_partner"
)]
pub fn show_with_partner(
    gpu: &mut Gpu,
    buffers: &mut DoubleBuffer,
    line: &str,
    partner: &Partner<'_>,
    brightness: Brightness,
    buttons: impl FnMut() -> ButtonState,
) {
    play(gpu, buffers, line, Some(partner), brightness, &mut {
        buttons
    });
}

/// A picture on screen: where it is drawn, its texels' size, and its two materials (opaque, and
/// the additive one the glint is drawn with).
#[derive(Clone, Copy)]
struct Picture {
    x: i16,
    y: i16,
    w: i16,
    h: i16,
    tw: u8,
    th: u8,
    base: TextureMaterial,
    glint: TextureMaterial,
}

/// The ease-out offset of an element that enters at `at`, `distance` at the start and 0 at the
/// end, and its fade level (0 to 0x80).
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.enter"
)]
fn enter(frame: i32, at: i32, distance: i32) -> (i16, i32) {
    let t = (frame - at).clamp(0, ENTER);
    let rest = ENTER - t;
    (
        (distance * rest * rest / (ENTER * ENTER)) as i16,
        t * 0x80 / ENTER,
    )
}

/// `palette` with the semi-transparency bit on every colour but the transparent one.
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.glint_palette"
)]
fn glint_palette(palette: &[u16], out: &mut [u16]) {
    for (to, from) in out.iter_mut().zip(palette) {
        *to = if *from == 0 { 0 } else { *from | 0x8000 };
    }
}

/// Not generic (the pad closure is a trait object) and never inlined, so the splash is compiled
/// once, here, for size.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.play"
)]
fn play(
    gpu: &mut Gpu,
    buffers: &mut DoubleBuffer,
    line: &str,
    partner: Option<&Partner<'_>>,
    brightness: Brightness,
    buttons: &mut dyn FnMut() -> ButtonState,
) {
    debug_assert_eq!(
        buffers.size(),
        (320, 240),
        "the splash is laid out for 320x240"
    );
    upload_mark();
    let mut stp = [0u16; 256];
    glint_palette(&logo::PALETTE, &mut stp);
    upload_16bpp(
        VramRect::new(MARK_GLINT_CLUT.x(), MARK_GLINT_CLUT.y(), 16, 1),
        &stp[..16],
    );
    let font = FontAtlas::upload(&FONT, FONT_TPAGE, FONT_CLUT);
    let mut mark = Picture {
        x: 112,
        y: SOLO_MARK_Y,
        w: MARK,
        h: MARK,
        tw: 128,
        th: 128,
        base: TextureMaterial::opaque(MARK_CLUT.uv_word(), MARK_TPAGE.uv_word(0), (0, 0, 0)),
        glint: TextureMaterial::blended(
            MARK_GLINT_CLUT.uv_word(),
            MARK_TPAGE.uv_word(0),
            (0, 0, 0),
            BlendMode::Add,
        ),
    };
    let mut line_y = SOLO_LINE_Y;
    let mut logo = None;
    let mut second = ("", 0, 0);
    if let Some(p) = partner {
        debug_assert!(
            p.width % 2 == 0 && p.width <= 254 && p.height <= 200,
            "partner logo too large"
        );
        debug_assert_eq!(
            p.pixels.len(),
            usize::from(p.width / 2) * usize::from(p.height)
        );
        upload_16bpp(
            VramRect::new(LOGO_CLUT.x(), LOGO_CLUT.y(), 256, 1),
            p.palette,
        );
        glint_palette(p.palette, &mut stp);
        upload_16bpp(
            VramRect::new(LOGO_GLINT_CLUT.x(), LOGO_GLINT_CLUT.y(), 256, 1),
            &stp,
        );
        upload_16bpp(
            VramRect::new(LOGO_TPAGE.x(), LOGO_TPAGE.y(), p.width / 2, p.height),
            p.pixels,
        );
        // The two pictures side by side, centred on the taller, the two lines under them; the
        // whole block centred on the screen.
        let (lw, lh) = (p.width as i16, p.height as i16);
        let pictures = lh.max(MARK);
        let top = (240 - (pictures + LINES_GAP + 8 + SECOND_GAP + 8)) / 2;
        let x0 = (320 - (MARK + GAP + lw)) / 2;
        mark.x = x0;
        mark.y = top + (pictures - MARK) / 2;
        logo = Some(Picture {
            x: x0 + MARK + GAP,
            y: top + (pictures - lh) / 2,
            w: lw,
            h: lh,
            tw: lw as u8,
            th: lh as u8,
            base: TextureMaterial::opaque(LOGO_CLUT.uv_word(), LOGO_TPAGE.uv_word(0), (0, 0, 0)),
            glint: TextureMaterial::blended(
                LOGO_GLINT_CLUT.uv_word(),
                LOGO_TPAGE.uv_word(0),
                (0, 0, 0),
                BlendMode::Add,
            ),
        });
        line_y = top + pictures + LINES_GAP;
        let x = 160 - (font.text_width(p.line) / 2) as i16;
        second = (p.line, x, line_y + 8 + SECOND_GAP);
    }
    let line_x = 160 - (font.text_width(line) / 2) as i16;
    // The glint's path: a diagonal band from left of the pictures to right of them.
    let left = i32::from(mark.x);
    let right = logo.map_or(i32::from(mark.x + mark.w), |l| i32::from(l.x + l.w));
    let (glint_from, glint_to) = (left - GLINT_BAND - i32::from(MARK) / 2, right + GLINT_BAND);

    // One frame per VBlank whatever the presentation cap is: 150 frames are 2.5 seconds.
    let cap = psx_rt::present::cap();
    psx_rt::present::set_cap(psx_rt::present::PresentCap::UNCAPPED);
    let skips = |b: ButtonState| b.is_held(SKIP);
    let mut previous = buttons();
    let mut frame = 0;
    while frame < TOTAL {
        let now = buttons();
        if frame > SKIP_GRACE && skips(now) && !skips(previous) {
            break;
        }
        previous = now;
        // Everything fades out together at the end; the overlay fades with it so the end is black.
        let out = if frame < TOTAL - FADE_OUT {
            0x80
        } else {
            ((TOTAL - frame) * 0x80 / FADE_OUT).max(0)
        };

        buffers.clear(gpu, (0, 0, 0));
        // Alone, the mark rises into place; beside a partner it slides in from the left and the
        // logo from the right.
        let (d, level) = enter(
            frame,
            MARK_AT,
            if logo.is_some() { SLIDE } else { RISE * 2 },
        );
        if logo.is_some() {
            draw_picture(gpu, &mark, -d, 0, level.min(out));
        } else {
            draw_picture(gpu, &mark, 0, d, level.min(out));
        }
        if let Some(l) = &logo {
            let (dx, level) = enter(frame, LOGO_AT, SLIDE);
            draw_picture(gpu, l, dx, 0, level.min(out));
        }
        let t = frame - GLINT_AT;
        if (0..GLINT).contains(&t) {
            let at = glint_from + (glint_to - glint_from) * t / GLINT;
            draw_glint(gpu, &mark, at);
            if let Some(l) = &logo {
                draw_glint(gpu, l, at);
            }
        }
        let head = (frame - LINE_AT) / 2;
        let (dy, level) = enter(frame, LINE_AT, RISE);
        draw_line(&font, line_x, line_y + dy, line, head, level.min(out));
        if logo.is_some() {
            let (dy, level) = enter(frame, SECOND_AT, RISE);
            draw_line(
                &font,
                second.1,
                second.2 + dy,
                second.0,
                head - 4,
                level.min(out),
            );
        }
        if let Some(overlay) = brightness.faded_overlay(Resolution::R320X240, out as u8) {
            gpu.draw(&overlay);
        }
        gpu.wait_idle();
        // The installed-only wait: it never reaches `install_vblank_counter` and with it the
        // I-cache flush, so a program that runs the splash from an overlay can prove its call tree
        // clean. A program that has not installed the counter asserts in a debug build; in a
        // release build the bounded wait runs out once and the splash ends.
        if !psx_rt::interrupts::try_wait_vblank(VBLANK_SPINS) {
            debug_assert!(
                false,
                "psx_splash::show: the VBlank count never advanced; call \
                 psx_rt::interrupts::install_vblank_counter first"
            );
            break;
        }
        buffers.swap(gpu);
        frame += 1;
    }
    psx_rt::present::set_cap(cap);
}

/// A picture, `(dx, dy)` off its place, at fade `level` (0 to 0x80).
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.draw_picture"
)]
fn draw_picture(gpu: &mut Gpu, p: &Picture, dx: i16, dy: i16, level: i32) {
    if level <= 0 {
        return;
    }
    let grey = level as u8;
    let (x, y) = (p.x + dx, p.y + dy);
    gpu.draw(&QuadTexturedMaterial::with_material(
        [(x, y), (x + p.w, y), (x, y + p.h), (x + p.w, y + p.h)],
        [(0, 0), (p.tw, 0), (0, p.th), (p.tw, p.th)],
        p.base.with_tint((grey, grey, grey)),
    ));
}

/// The glint over a picture: the picture again, added over itself in vertical strips whose
/// corners are lit by their distance from a diagonal line through x `at` (at the picture's top,
/// leaning right as it goes down), so a soft bright band crosses it.
#[cfg_attr(target_arch = "mips", optimize(size))]
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.draw_glint"
)]
fn draw_glint(gpu: &mut Gpu, p: &Picture, at: i32) {
    let light = |x: i16, y: i16| {
        let d = (i32::from(x) - (at + i32::from(y - p.y) / 2)).abs();
        let v = (GLINT_PEAK * (GLINT_BAND - d) / GLINT_BAND).max(0) as u8;
        (v, v, v)
    };
    let (y0, y1) = (p.y, p.y + p.h);
    let strips = (p.w + GLINT_STRIP - 1) / GLINT_STRIP;
    for i in 0..strips {
        let (sx0, sx1) = (i * GLINT_STRIP, ((i + 1) * GLINT_STRIP).min(p.w));
        let (x0, x1) = (p.x + sx0, p.x + sx1);
        let lit = [light(x0, y0), light(x1, y0), light(x0, y1), light(x1, y1)];
        if lit.iter().all(|c| c.0 == 0) {
            continue;
        }
        let u = |sx: i16| (i32::from(sx) * i32::from(p.tw) / i32::from(p.w)) as u8;
        gpu.draw(&QuadTexturedGouraud::with_material(
            [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
            [(u(sx0), 0), (u(sx1), 0), (u(sx0), p.th), (u(sx1), p.th)],
            lit,
            p.glint,
        ));
    }
}

/// Expand the mark into VRAM sixteen rows at a time (1,024 bytes of stack, once), and put its
/// palette beside it. Entry 0 of the palette is opaque near-black: the backdrop is black and
/// 0x0000 is transparent.
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.upload_mark"
)]
fn upload_mark() {
    let mut y = MARK_TPAGE.y();
    logo::expand(&mut |band| {
        upload_16bpp(VramRect::new(MARK_TPAGE.x(), y, 32, 16), band);
        y += 16;
    });
    upload_16bpp(
        VramRect::new(MARK_CLUT.x(), MARK_CLUT.y(), 16, 1),
        &logo::PALETTE,
    );
}

/// A letter's colour at fade `level` (0 to 0x80), brightened toward white by `near`, how close the
/// sheen's head is (0 to `SHEEN`). A function, not a closure: a closure cannot be placed in boot-only
/// memory by name and would stay resident.
#[cfg_attr(target_arch = "mips", link_section = ".text.boot_inner.psx_splash.mix")]
fn mix(colour: (u8, u8, u8), near: i32, level: i32) -> (u8, u8, u8) {
    let channel = |value: u8| {
        let base = i32::from(value) * level / 0x80;
        (base + (level - base) * near / SHEEN) as u8
    };
    (channel(colour.0), channel(colour.1), channel(colour.2))
}

/// The line: a black outline, then each letter as a gradient whose brightness rises toward
/// the sheen's head, all scaled by the fade `level` (0 to 0x80).
#[cfg_attr(target_arch = "mips", optimize(size))]
#[cfg_attr(
    target_arch = "mips",
    link_section = ".text.boot_inner.psx_splash.draw_line"
)]
fn draw_line(font: &FontAtlas, mut x: i16, y: i16, line: &str, head: i32, level: i32) {
    if level <= 0 {
        return;
    }
    for (index, ch) in line.char_indices() {
        // The one character as a `&str`, copied out so nothing re-slices `line`.
        let mut one = [0u8; 4];
        let glyph: &str = ch.encode_utf8(&mut one);
        for (dx, dy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
            font.draw_text(x + dx, y + dy, glyph, (0, 0, 0));
        }
        let near = (SHEEN - (index as i32 - head).abs() * 6).max(0);
        font.draw_text_gradient(
            x,
            y,
            glyph,
            mix((0x68, 0x80, 0x80), near, level),
            mix((0x38, 0x58, 0x80), near, level),
        );
        // `BASIC` is fixed width and has no advance table, so every glyph advances the same.
        x += i16::from(font.font().advance_x);
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
    fn the_mark_expands_to_its_8_bands_and_its_words() {
        let mut words = std::vec::Vec::new();
        logo::expand(&mut |band| words.extend_from_slice(band));
        assert_eq!(words.len(), logo::WORDS);
        assert_eq!(fnv1a(words.into_iter()), logo::FNV1A);
    }

    #[test]
    fn the_fade_ends_at_two_128ths() {
        let last = (TOTAL - (TOTAL - 1)) * 0x80 / FADE_OUT;
        assert_eq!(last, 2);
        const { assert!(GLINT_AT + GLINT <= TOTAL - FADE_OUT && SECOND_AT + ENTER <= TOTAL - FADE_OUT) };
    }
}

#[cfg(test)]
mod enter_tests {
    use super::*;

    #[test]
    fn an_entrance_starts_away_and_dark_and_ends_in_place_and_full() {
        assert_eq!(enter(0, 0, SLIDE), (SLIDE as i16, 0));
        assert_eq!(enter(ENTER, 0, SLIDE), (0, 0x80));
        assert_eq!(enter(-5, 0, SLIDE), (SLIDE as i16, 0));
        assert_eq!(enter(500, 0, SLIDE), (0, 0x80));
    }
}
