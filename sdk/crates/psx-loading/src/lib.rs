// SPDX-License-Identifier: GPL-2.0-or-later
//! An animated LOADING indicator for the stretches where a program is blocked
//! on the disc.
//!
//! A load that blocks for seconds (a chain-load, a pack read, a sound bank
//! streamed to the SPU) leaves the picture frozen, and a frozen picture reads
//! as a hang. This crate draws the one thing such a picture needs: a label
//! (`Loading`) with a tail that steps (trailing dots, or a ring of lights)
//! and an ink that breathes, so the screen visibly moves while the CPU is busy
//! elsewhere.
//!
//! The adoption guide, game by game, is `sdk/docs/LOADING-INDICATOR.md`.
//!
//! The look every PSoXide game uses is the **corner widget**: a small black
//! square in the bottom-right corner of the picture with a little animation in it
//! and no text ([`Style::in_corner`] with an empty label). The animation is the
//! game's own: the built-in ring of lights in the game's colours ([`Tail::Spinner`])
//! or art it draws itself ([`Tail::Art`]). A label with dots is there for
//! programs that want words.
//!
//! # Design
//!
//! * **Plain rectangles.** Every glyph is drawn as GP0(60h) monochrome
//!   rectangles from a [`psx_font::BitmapFont`]'s 1-bit source, so the
//!   indicator needs no texture, no CLUT and no atlas, and disturbs no
//!   texture state. That is what lets one implementation serve a chain-load
//!   blob (nothing in VRAM but the picture), a game mid-boot and a menu with
//!   its own atlases. A game picks its look by giving the font, the scale, the
//!   inks, the backing colour and where the box sits ([`Style`]).
//! * **Polled, never waiting.** [`Indicator::poll`] takes the caller's tick
//!   (one per VBlank) and draws only when the picture would change: at most
//!   once a VBlank, never waiting for one. A blocking loop calls it between
//!   its own steps (after each CD sector, each spin of a seek wait). Nothing
//!   here runs in an interrupt, so a draw never overlaps the caller's own GPU
//!   traffic.
//! * **No hardware owned.** The indicator builds GP0 packets and hands them to
//!   a [`Sink`]; [`Gp0`] (guest only) is the sink that writes them to the GPU
//!   with the one-wait-per-packet pacing the silicon needs. A host test sink
//!   records them, which is how the layout and the animation are tested.
//! * **No float, no 64-bit, no heap.** Colours blend in integer steps and the
//!   breathing is a triangle wave over a sixteen-step ramp.
//!
//! The caller owns the GPU state the rectangles are drawn under: the drawing
//! area and offset must cover the box, and the mask bit and draw mode must be
//! at their defaults (see [`prepare`]).
//!
//! ```
//! use psx_font::fonts::BASIC_8X16;
//! use psx_loading::{Indicator, Rgb, Sink, Style};
//!
//! struct Count(usize);
//! impl Sink for Count {
//!     fn packet(&mut self, _words: &[u32]) {
//!         self.0 += 1;
//!     }
//! }
//!
//! const STYLE: Style = Style::new(&BASIC_8X16, "Loading", Rgb(200, 120, 20), Rgb(5, 5, 7));
//! let mut indicator = Indicator::new(STYLE);
//! let mut sink = Count(0);
//! // The first poll draws; the same tick again draws nothing.
//! assert!(indicator.poll(&mut sink, 0));
//! assert!(!indicator.poll(&mut sink, 0));
//! ```

#![no_std]
#![warn(missing_docs)]

use psx_font::BitmapFont;

/// An 8-bit-per-channel colour, as the GP0 rectangle commands take it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// The GP0 colour word: `0x00BBGGRR`.
    pub const fn word(self) -> u32 {
        self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16
    }

    /// `self` at `level` of [`LEVELS`] of the way to `other`.
    const fn toward(self, other: Rgb, level: u8) -> Rgb {
        const fn mix(a: u8, b: u8, level: u8) -> u8 {
            let a = a as i32;
            let b = b as i32;
            (a + (b - a) * level as i32 / LEVELS as i32) as u8
        }
        Rgb(
            mix(self.0, other.0, level),
            mix(self.1, other.1, level),
            mix(self.2, other.2, level),
        )
    }
}

/// Where the indicator's GP0 packets go.
pub trait Sink {
    /// Take one complete GP0 packet. A real sink waits for the GPU's command
    /// FIFO once, then writes every word of the packet back to back (see [`Gp0`]).
    fn packet(&mut self, words: &[u32]);
}

/// The sink that writes packets to the GPU's command port.
///
/// One bounded wait for `GPUSTAT` ready per packet, then every word of the
/// packet back to back. Pacing every word is as wrong as pacing none: bit 26
/// drops while the GPU ingests the rest of a multi-word packet, so a per-word
/// wait spins its whole bound before each vertex word (silicon finding, see
/// the chain loader's `paint`). The wait is bounded and gives up quietly: a
/// dead GPU must not hang the screen that is meant to show a live load.
#[cfg(target_arch = "mips")]
pub struct Gp0;

#[cfg(target_arch = "mips")]
impl Sink for Gp0 {
    fn packet(&mut self, words: &[u32]) {
        let _ = psx_io::gpu::try_wait_command_ready(GP0_WAIT_SPINS);
        for &word in words {
            psx_io::gpu::write_command(word);
        }
    }
}

/// Spin budget for the ready wait before one packet.
#[cfg(target_arch = "mips")]
const GP0_WAIT_SPINS: u32 = 1_000_000;

/// Steps of the breathing ramp: the ink is `ink_dim` at level 0 and
/// `ink_bright` at this level.
pub const LEVELS: u8 = 16;

/// Lights on the spinner's ring: the edge cells of a 4 x 4 grid, which is a square.
const RING_CELLS: usize = 12;
/// The ring's cells (column, row) in order around the square, clockwise from the
/// top left corner.
const RING: [(u8, u8); RING_CELLS] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
    (2, 3),
    (1, 3),
    (0, 3),
    (0, 2),
    (0, 1),
];
/// Cells along one side of the ring.
const RING_SIDE_CELLS: i16 = 4;
/// How brightly the light `n` places behind the head burns, out of 256: the head,
/// then a tail that fades. Further back a light is only the unlit dot.
const TRAIL: [u16; 5] = [256, 168, 98, 52, 24];
/// Default dot size and pitch (cell to cell) of the ring, in unscaled display units
/// (one unit is one pixel tall; see [`Style::with_display`]).
const RING_DOT: i16 = 3;
const RING_PITCH: i16 = 5;
/// Gap before the spinner when a label comes first, in unscaled pixels.
const SPINNER_GAP: i16 = 8;

/// What follows the label and moves.
#[derive(Copy, Clone, Debug)]
pub enum Tail {
    /// Nothing: the label alone, breathing if the ink does.
    None,
    /// Up to this many dots after the label (`Loading`, `Loading.`,
    /// `Loading..`, ...). The box has room for all of them, so the label does
    /// not slide as they step.
    Dots(u8),
    /// A ring of twelve lights chasing each other round a square: the head burns
    /// at `lit`, a tail fades behind it, and every other light shows `unlit`. The
    /// ring is square on the display, whatever the pixel aspect of the mode (see
    /// [`Style::with_display`]), and its lights are drawn with the style's
    /// [`Blend`].
    Spinner {
        /// The head light at full strength.
        lit: Rgb,
        /// A light with no head near it, dim enough that the ring still reads as a ring.
        unlit: Rgb,
    },
    /// Animation the game draws itself, in a box of `width` x `height` pixels
    /// (the widget is as big as that plus its padding). `draw(sink, x, y, frame)`
    /// paints frame `frame` (`0..frames`) with its top-left at `(x, y)`, using
    /// [`rect`] or any GP0 packet; the box is already filled with the style's ground.
    Art {
        /// Width of the art in pixels.
        width: i16,
        /// Height of the art in pixels.
        height: i16,
        /// Frames before it repeats.
        frames: u8,
        /// Draws one frame.
        draw: fn(&mut dyn Sink, i16, i16, u8),
    },
}

impl Tail {
    /// Distinct steps before the tail repeats.
    const fn steps(self) -> u32 {
        match self {
            Tail::None => 1,
            Tail::Dots(n) => n as u32 + 1,
            Tail::Spinner { .. } => RING_CELLS as u32,
            Tail::Art { frames, .. } => frames as u32,
        }
    }

    /// Cells of the font's advance the tail takes (the dots; 0 otherwise).
    const fn cells(self) -> i32 {
        match self {
            Tail::Dots(n) => n as i32,
            _ => 0,
        }
    }
}

/// How the spinner's lights combine with what is under them (the GPU's
/// semi-transparency modes, set through the draw-mode word GP0(E1h)).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Blend {
    /// Opaque: a light replaces the pixels under it.
    Opaque,
    /// `B/2 + F/2`: half the light and half of what is behind it. A light is
    /// dimmer than its colour and nothing brightens past it.
    Average,
    /// `B + F`: the light's colour is added to what is behind it. A light burns
    /// at its full colour, a faint one only brightens its neighbourhood, and where
    /// two lights meet they add up.
    Additive,
    /// `B + F/4`: a quarter of the light added, for a faint glow.
    Glow,
}

impl Blend {
    /// The semi-transparency mode field of GP0(E1h), if the lights are blended.
    const fn mode(self) -> Option<u32> {
        match self {
            Blend::Opaque => None,
            Blend::Average => Some(0),
            Blend::Additive => Some(1),
            Blend::Glow => Some(3),
        }
    }
}

/// What the indicator looks like and where it sits.
///
/// Build one with [`Style::new`] (a `const fn`, so a `static` or `const` works)
/// and refine it with the `with_*` methods.
#[derive(Copy, Clone, Debug)]
pub struct Style {
    /// The glyph source. Any [`BitmapFont`] works; the label is laid out with
    /// the font's advances (per glyph for a proportional font), and the box always
    /// has room for the whole tail, so it does not change width as the tail steps.
    pub font: &'static BitmapFont,
    /// Integer pixel scale: every font pixel becomes a `scale`-square.
    pub scale: u8,
    /// The label, ASCII.
    pub label: &'static str,
    /// What follows the label.
    pub tail: Tail,
    /// The darkest ink of the breathing.
    pub ink_dim: Rgb,
    /// The brightest ink of the breathing. Equal to `ink_dim` for a steady ink.
    pub ink_bright: Rgb,
    /// The colour the box is filled with before the text, so each redraw
    /// erases the last one.
    pub ground: Rgb,
    /// A one-pixel line along the top edge of the box, if any.
    pub rule: Option<Rgb>,
    /// Horizontal space between the box edge and the content.
    pub pad_x: i16,
    /// Vertical space between the box edge and the content.
    pub pad_y: i16,
    /// Screen x the box is centred on.
    pub centre_x: i16,
    /// Screen y of the box's top edge.
    pub top: i16,
    /// The smallest the box may be, `(width, height)`: the content is centred in
    /// it. `(0, 0)` sizes the box to its content plus the padding.
    pub min_size: (i16, i16),
    /// VBlanks per tail step.
    pub step_ticks: u16,
    /// VBlanks per full breath (dim to bright and back). Even and at least
    /// twice [`LEVELS`]; a smaller value is raised to that.
    pub pulse_ticks: u16,
    /// How the spinner's lights blend with what is under them.
    pub blend: Blend,
    /// Pixels across per unit of height on a 4:3 screen, in 1/256ths, from the display
    /// mode (256 is square pixels). Set by [`Style::with_display`].
    across_q8: u16,
    /// Spinner dot size and spacing between dot centres, in unscaled display units.
    pub ring: (i16, i16),
}

impl Style {
    /// A style with sensible defaults: scale 1, three dots a third of a second
    /// apart at 60 Hz (20 VBlanks), a steady ink, a breath of 64 VBlanks if one
    /// is asked for, the box centred horizontally on a 320-wide screen near
    /// the foot of the picture.
    pub const fn new(
        font: &'static BitmapFont,
        label: &'static str,
        ink: Rgb,
        ground: Rgb,
    ) -> Self {
        Style {
            font,
            scale: 1,
            label,
            tail: Tail::Dots(3),
            ink_dim: ink,
            ink_bright: ink,
            ground,
            rule: None,
            pad_x: 12,
            pad_y: 4,
            centre_x: 160,
            top: 208,
            min_size: (0, 0),
            step_ticks: 20,
            pulse_ticks: 64,
            blend: Blend::Additive,
            across_q8: 256,
            ring: (RING_DOT, RING_PITCH),
        }
    }

    /// The same style breathing between `dim` and `bright`.
    pub const fn with_breath(mut self, dim: Rgb, bright: Rgb) -> Self {
        self.ink_dim = dim;
        self.ink_bright = bright;
        self
    }

    /// The same style with `tail` after the label.
    pub const fn with_tail(mut self, tail: Tail) -> Self {
        self.tail = tail;
        self
    }

    /// The same style with its box centred on `centre_x`, top edge at `top`.
    pub const fn at(mut self, centre_x: i16, top: i16) -> Self {
        self.centre_x = centre_x;
        self.top = top;
        self
    }

    /// The same style as a square `size` pixels tall in the bottom-right corner of
    /// a `screen_w` x `screen_h` picture, `margin` pixels in from the bottom edge:
    /// the corner widget. With an empty label and a [`Tail::Spinner`] it is the small
    /// black square with a ring of lights in it.
    ///
    /// The square is square on the screen. A mode whose pixels are not square (512
    /// wide at 240 lines on a 4:3 set, say) makes the box wider in pixels by the
    /// pixel aspect (see [`with_display`](Self::with_display), which this calls), and
    /// the side margin is stretched the same way so both margins look alike.
    pub const fn in_corner(mut self, size: i16, screen_w: i16, screen_h: i16, margin: i16) -> Self {
        self = self.with_display(screen_w, screen_h);
        let width = self.across(size as i32) as i16;
        self.min_size = (width, size);
        self.pad_x = 0;
        self.pad_y = 0;
        self.centre_x = screen_w - self.across(margin as i32) as i16 - width / 2;
        self.top = screen_h - margin - size;
        self
    }

    /// The same style told the display mode it is drawn in, `width` x `height`
    /// pixels. A 4:3 television shows any mode at 4:3, so a pixel is
    /// `(4 / 3) * height / width` times as wide as it is tall: square at 320 x 240
    /// and 640 x 480, wide at 256 x 240, narrow at 512 x 240. The spinner and the
    /// corner box are laid out in units one pixel tall and widened by that factor,
    /// so they are square on the screen, not in the framebuffer.
    #[must_use]
    pub const fn with_display(mut self, width: i16, height: i16) -> Self {
        let (w, h) = (
            if width < 1 { 1 } else { width as i32 },
            if height < 1 { 1 } else { height as i32 },
        );
        // 3 * w / (4 * h) in 1/256ths, rounded. A mode this far from 4:3 is not one a
        // PS1 has; the clamp keeps the factor in a u16 anyway.
        let q8 = (3 * w * 256 * 2 + 4 * h) / (8 * h);
        self.across_q8 = if q8 > u16::MAX as i32 {
            u16::MAX
        } else {
            q8 as u16
        };
        self
    }

    /// The same style with spinner dots `dot` units square, `pitch` units between
    /// the centres of neighbouring dots (units are one pixel tall before
    /// [`scaled`](Self::scaled); the ring is three pitches and a dot across).
    #[must_use]
    pub const fn with_ring(mut self, dot: i16, pitch: i16) -> Self {
        self.ring = (
            if dot < 1 { 1 } else { dot },
            if pitch < 1 { 1 } else { pitch },
        );
        self
    }

    /// The same style with the spinner's lights blended by `blend`.
    #[must_use]
    pub const fn with_blend(mut self, blend: Blend) -> Self {
        self.blend = blend;
        self
    }

    /// `units` of height as pixels across, rounded: what makes a square on a 4:3 screen.
    const fn across(&self, units: i32) -> i32 {
        (units * self.across_q8 as i32 + 128) >> 8
    }

    /// Side of the spinner's ring in units of height (before scale).
    const fn ring_side(&self) -> i32 {
        (RING_SIDE_CELLS as i32 - 1) * self.ring.1 as i32 + self.ring.0 as i32
    }

    /// The ring's width and height in pixels.
    #[must_use]
    pub const fn ring_size(&self) -> (i16, i16) {
        let side = self.ring_side() * self.scale as i32;
        (self.across(side) as i16, side as i16)
    }

    /// The same style at integer scale `scale` (at least 1).
    pub const fn scaled(mut self, scale: u8) -> Self {
        self.scale = if scale == 0 { 1 } else { scale };
        self
    }

    /// The same style with `rule` along the box's top edge.
    pub const fn with_rule(mut self, rule: Rgb) -> Self {
        self.rule = Some(rule);
        self
    }

    /// The same style with `pad_x` and `pad_y` between the content and the box edge.
    pub const fn padded(mut self, pad_x: i16, pad_y: i16) -> Self {
        self.pad_x = pad_x;
        self.pad_y = pad_y;
        self
    }

    /// The same style with the tail stepping every `step_ticks` VBlanks (at
    /// least 1) and a full breath taking `pulse_ticks`.
    pub const fn timed(mut self, step_ticks: u16, pulse_ticks: u16) -> Self {
        self.step_ticks = if step_ticks == 0 { 1 } else { step_ticks };
        self.pulse_ticks = pulse_ticks;
        self
    }

    /// The font's advance for ASCII `byte`, unscaled.
    fn advance(&self, byte: u8) -> u8 {
        self.font.glyph_advance(byte as char)
    }

    /// Pixel width of the content: the label, then the tail.
    fn content_width(&self) -> i32 {
        let label: i32 = self
            .label
            .bytes()
            .map(|byte| self.advance(byte) as i32)
            .sum();
        let dots = self.tail.cells() * self.advance(b'.') as i32;
        let text = (label + dots) * self.scale as i32;
        let gap = if self.label.is_empty() {
            0
        } else {
            SPINNER_GAP as i32 * self.scale as i32
        };
        match self.tail {
            Tail::Spinner { .. } => text + gap + self.ring_size().0 as i32,
            Tail::Art { width, .. } => text + gap + width as i32 * self.scale as i32,
            _ => text,
        }
    }

    /// Pixel height of the content: the label's row, or the spinner alone.
    fn content_height(&self) -> i32 {
        match self.tail {
            Tail::Spinner { .. } if self.label.is_empty() => self.ring_size().1 as i32,
            Tail::Art { height, .. } if self.label.is_empty() => height as i32 * self.scale as i32,
            _ => self.font.glyph_h as i32 * self.scale as i32,
        }
    }

    /// The box as `(x, y, width, height)` in screen pixels.
    pub fn bounds(&self) -> (i16, i16, i16, i16) {
        let width = (self.content_width() + 2 * self.pad_x as i32).max(self.min_size.0 as i32);
        let height = (self.content_height() + 2 * self.pad_y as i32).max(self.min_size.1 as i32);
        (
            (self.centre_x as i32 - width / 2) as i16,
            self.top,
            width as i16,
            height as i16,
        )
    }

    /// The breathing period, kept even and at least twice [`LEVELS`].
    const fn pulse(&self) -> u32 {
        let ticks = if self.pulse_ticks < 2 * LEVELS as u16 {
            2 * LEVELS as u16
        } else {
            self.pulse_ticks
        };
        (ticks & !1) as u32
    }

    /// The look at `tick` VBlanks after the indicator began.
    ///
    /// The tail steps once every `step_ticks` and repeats. The ink follows a
    /// triangle wave: dim, up to bright at half a breath, back to dim. With a
    /// steady ink (`ink_dim == ink_bright`) the level is held at 0, so the
    /// picture only changes when the tail steps.
    pub const fn look(&self, tick: u32) -> Look {
        let step = ((tick / self.step_ticks as u32) % self.tail.steps()) as u8;
        let level = if self.ink_dim.word() == self.ink_bright.word() {
            0
        } else {
            let period = self.pulse();
            let phase = tick % period;
            let half = period / 2;
            let up = if phase < half { phase } else { period - phase };
            (up * LEVELS as u32 / half) as u8
        };
        Look { step, level }
    }

    /// The ink colour at `level`.
    pub const fn ink(&self, level: u8) -> Rgb {
        self.ink_dim.toward(self.ink_bright, level)
    }
}

/// What the indicator shows at one tick.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// The tail step: dots lit for [`Tail::Dots`], the spinner frame for
    /// [`Tail::Spinner`], always 0 for [`Tail::None`].
    pub step: u8,
    /// Ink level, `0` (dim) to [`LEVELS`] (bright).
    pub level: u8,
}

/// The GP0 words that put the state the indicator's rectangles are drawn under
/// to its defaults: mask bit off (E6h), plain draw mode (E1h).
///
/// Call it once before the first draw if the program's GPU state is unknown.
/// The drawing area and offset are the caller's: they decide where in VRAM the
/// box lands, and a program that draws into a back buffer sets them to the
/// buffer the viewer is looking at.
pub fn prepare(sink: &mut dyn Sink) {
    sink.packet(&[0xe600_0000]);
    sink.packet(&[0xe100_0000]);
}

/// A running indicator.
#[derive(Copy, Clone, Debug)]
pub struct Indicator {
    style: Style,
    start: u32,
    shown: Option<Look>,
}

impl Indicator {
    /// An indicator that has not begun.
    pub const fn new(style: Style) -> Self {
        Indicator {
            style,
            start: 0,
            shown: None,
        }
    }

    /// The style it was made with.
    pub const fn style(&self) -> &Style {
        &self.style
    }

    /// Restart the animation at `tick` and forget what is on screen, so the
    /// next [`poll`](Self::poll) draws.
    pub fn begin(&mut self, tick: u32) {
        self.start = tick;
        self.shown = None;
    }

    /// Forget what is on screen (the picture was cleared or replaced), so the
    /// next [`poll`](Self::poll) draws again at once.
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    /// Draw if the picture at `tick` differs from what is on screen. Returns
    /// whether it drew.
    ///
    /// The first poll after [`new`](Self::new), [`begin`](Self::begin) or
    /// [`invalidate`](Self::invalidate) always draws. After that a poll draws
    /// only when the tail stepped or the ink moved a level, and a poll at the
    /// tick already shown does nothing. It never waits for a VBlank. Call it as
    /// often as is convenient between steps of a blocking load, with a tick
    /// that advances once per VBlank.
    pub fn poll(&mut self, sink: &mut dyn Sink, tick: u32) -> bool {
        // `wrapping_sub`: the VBlank counter wraps, the animation just goes on.
        let look = self.style.look(tick.wrapping_sub(self.start));
        if self.shown == Some(look) {
            return false;
        }
        self.draw(sink, look);
        self.shown = Some(look);
        true
    }

    /// Draw `look` unconditionally.
    pub fn draw(&self, sink: &mut dyn Sink, look: Look) {
        let style = &self.style;
        let (x, y, w, h) = style.bounds();
        rect(sink, x, y, w, h, style.ground);
        if let Some(rule) = style.rule {
            rect(sink, x, y, w, 1, rule);
        }
        let ink = style.ink(look.level);
        let scale = style.scale as i16;
        // The content sits centred in the box (which is its size plus the
        // padding unless `min_size` made it bigger).
        let text_top = y + (h - style.content_height() as i16) / 2;
        let mut pen = x + (w - style.content_width() as i16) / 2;
        for byte in style.label.bytes() {
            glyph(sink, style, pen, text_top, byte, ink);
            pen += style.advance(byte) as i16 * scale;
        }
        match style.tail {
            Tail::None => {}
            Tail::Dots(n) => {
                let mut dot = 0;
                while dot < n.min(look.step) {
                    glyph(sink, style, pen, text_top, b'.', ink);
                    pen += style.advance(b'.') as i16 * scale;
                    dot += 1;
                }
            }
            Tail::Spinner { lit, unlit } => {
                // Centred on the text's height, not sharing its baseline.
                let rows = style.content_height() as i16;
                let top = text_top + (rows - style.ring_size().1) / 2;
                let gap = if style.label.is_empty() {
                    0
                } else {
                    SPINNER_GAP
                };
                ring(sink, style, pen + gap * scale, top, look.step, lit, unlit);
            }
            Tail::Art { height, draw, .. } => {
                let rows = style.content_height() as i16;
                let top = text_top + (rows - height * scale) / 2;
                let gap = if style.label.is_empty() {
                    0
                } else {
                    SPINNER_GAP
                };
                draw(sink, pen + gap * scale, top, look.step);
            }
        }
    }
}

/// One solid rectangle, GP0(60h): the building block for a game's own [`Tail::Art`].
pub fn rect(sink: &mut dyn Sink, x: i16, y: i16, w: i16, h: i16, colour: Rgb) {
    if w <= 0 || h <= 0 {
        return;
    }
    sink.packet(&[
        0x6000_0000 | colour.word(),
        (y as u16 as u32) << 16 | x as u16 as u32,
        (h as u16 as u32) << 16 | w as u16 as u32,
    ]);
}

/// One solid semi-transparent rectangle, GP0(62h): blended with what is under it
/// by the mode last set with [`blend_mode`].
fn rect_blended(sink: &mut dyn Sink, x: i16, y: i16, w: i16, h: i16, colour: Rgb) {
    if w <= 0 || h <= 0 {
        return;
    }
    sink.packet(&[
        0x6200_0000 | colour.word(),
        (y as u16 as u32) << 16 | x as u16 as u32,
        (h as u16 as u32) << 16 | w as u16 as u32,
    ]);
}

/// GP0(E1h) with semi-transparency mode `mode` (0 to 3) and everything else at the
/// state [`prepare`] leaves.
fn blend_mode(sink: &mut dyn Sink, mode: Option<u32>) {
    sink.packet(&[0xe100_0000 | mode.unwrap_or(0) << 5]);
}

/// `colour` at `level` out of 256 of its strength.
const fn dimmed(colour: Rgb, level: u16) -> Rgb {
    const fn channel(value: u8, level: u16) -> u8 {
        (value as u32 * level as u32 / 256) as u8
    }
    Rgb(
        channel(colour.0, level),
        channel(colour.1, level),
        channel(colour.2, level),
    )
}

/// The cell `(col, row)` of the ring as a pixel rectangle `(x, y, w, h)` from the
/// ring's top left corner `(x0, y0)`.
fn ring_cell(style: &Style, x0: i16, y0: i16, col: u8, row: u8) -> (i16, i16, i16, i16) {
    let (dot, pitch) = (style.ring.0 as i32, style.ring.1 as i32);
    let scale = style.scale as i32;
    let left = style.across((col as i32 * pitch) * scale);
    let right = style.across((col as i32 * pitch + dot) * scale);
    let top = row as i32 * pitch * scale;
    let bottom = (row as i32 * pitch + dot) * scale;
    (
        x0 + left as i16,
        y0 + top as i16,
        (right - left) as i16,
        (bottom - top) as i16,
    )
}

/// One frame of the ring: every cell as an `unlit` dot, then the head light at cell
/// `frame` and its fading tail behind it (towards lower cell numbers, that is
/// counter-clockwise behind a clockwise head), each blended as the style says.
fn ring(sink: &mut dyn Sink, style: &Style, x: i16, y: i16, frame: u8, lit: Rgb, unlit: Rgb) {
    for &(col, row) in &RING {
        let (cx, cy, cw, ch) = ring_cell(style, x, y, col, row);
        rect(sink, cx, cy, cw, ch, unlit);
    }
    let mode = style.blend.mode();
    if mode.is_some() {
        blend_mode(sink, mode);
    }
    for (behind, &level) in TRAIL.iter().enumerate() {
        let cell = (frame as usize + RING_CELLS - behind) % RING_CELLS;
        let (col, row) = RING[cell];
        let (cx, cy, cw, ch) = ring_cell(style, x, y, col, row);
        if mode.is_some() {
            rect_blended(sink, cx, cy, cw, ch, dimmed(lit, level));
        } else {
            rect(
                sink,
                cx,
                cy,
                cw,
                ch,
                unlit.toward(lit, (level as u32 * LEVELS as u32 / 256) as u8),
            );
        }
    }
    if mode.is_some() {
        blend_mode(sink, None);
    }
}

/// One glyph as rectangles: each row's runs of set pixels, rows with the same
/// bits merged into one taller rectangle.
fn glyph(sink: &mut dyn Sink, style: &Style, x: i16, y: i16, code: u8, ink: Rgb) {
    let font = style.font;
    let Some(index) = font.glyph_index(code as char) else {
        return;
    };
    let scale = style.scale as i16;
    let width = font.glyph_w as u32;
    let mut row = 0u8;
    while row < font.glyph_h {
        let bits = font.glyph_row_packed(index, row);
        let mut span = 1u8;
        while row + span < font.glyph_h && font.glyph_row_packed(index, row + span) == bits {
            span += 1;
        }
        let mut col = 0u32;
        while col < width {
            if bits & (1 << col) == 0 {
                col += 1;
                continue;
            }
            let start = col;
            while col < width && bits & (1 << col) != 0 {
                col += 1;
            }
            rect(
                sink,
                x + start as i16 * scale,
                y + row as i16 * scale,
                (col - start) as i16 * scale,
                span as i16 * scale,
                ink,
            );
        }
        row += span;
    }
}

#[cfg(test)]
mod tests;
