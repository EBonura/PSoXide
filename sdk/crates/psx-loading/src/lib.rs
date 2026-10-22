// SPDX-License-Identifier: GPL-2.0-or-later
//! An animated LOADING indicator for the stretches where a program is blocked
//! on the disc.
//!
//! A load that blocks for seconds (a chain-load, a pack read, a sound bank
//! streamed to the SPU) leaves the picture frozen, and a frozen picture reads
//! as a hang. This crate draws the one thing such a picture needs: a label
//! (`Loading`) with a tail that steps (trailing dots, or a small dot spinner)
//! and an ink that breathes, so the screen visibly moves while the CPU is busy
//! elsewhere.
//!
//! The adoption guide, game by game, is `sdk/docs/LOADING-INDICATOR.md`.
//!
//! The look every PSoXide game uses is the **corner widget**: a small black
//! square in the bottom-right corner of the picture with a little animation in it
//! and no text ([`Style::in_corner`] with an empty label). The animation is the
//! game's own: the built-in dot spinner in the game's colours ([`Tail::Spinner`])
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

/// The braille "dots" spinner (cli-spinners), as its 2x3 dot grid: each entry's
/// low six bits are the lit dots, bits 0 to 2 the left column top to bottom and
/// bits 3 to 5 the right column.
const SPINNER_FRAMES: [u8; 10] = [0x0b, 0x19, 0x39, 0x38, 0x3c, 0x34, 0x26, 0x27, 0x07, 0x0f];
/// Spinner dot size, pitch and the gap before it, in unscaled pixels.
const SPINNER_DOT: i16 = 3;
const SPINNER_PITCH: i16 = 5;
const SPINNER_GAP: i16 = 8;
/// The spinner grid: two columns of three dots.
const SPINNER_W: i16 = SPINNER_PITCH + SPINNER_DOT;
const SPINNER_H: i16 = 2 * SPINNER_PITCH + SPINNER_DOT;

/// What follows the label and moves.
#[derive(Copy, Clone, Debug)]
pub enum Tail {
    /// Nothing: the label alone, breathing if the ink does.
    None,
    /// Up to this many dots after the label (`Loading`, `Loading.`,
    /// `Loading..`, ...). The box has room for all of them, so the label does
    /// not slide as they step.
    Dots(u8),
    /// A small spinner after the label: a 2x3 grid of dots, a rotating subset lit.
    Spinner {
        /// A lit dot.
        lit: Rgb,
        /// An unlit dot, dim enough that the grid still reads as a spinner.
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
    /// Size of the non-text tail in unscaled pixels, if it has one.
    const fn art_size(self) -> Option<(i16, i16)> {
        match self {
            Tail::Spinner { .. } => Some((SPINNER_W, SPINNER_H)),
            Tail::Art { width, height, .. } => Some((width, height)),
            _ => None,
        }
    }

    /// Distinct steps before the tail repeats.
    const fn steps(self) -> u32 {
        match self {
            Tail::None => 1,
            Tail::Dots(n) => n as u32 + 1,
            Tail::Spinner { .. } => SPINNER_FRAMES.len() as u32,
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

    /// The same style as a square `size` pixels across in the bottom-right corner of
    /// a `screen_w` x `screen_h` picture, `margin` pixels in from the edges: the
    /// corner widget. With an empty label and a [`Tail::Spinner`] it is the small
    /// black square with a spinner in it.
    pub const fn in_corner(mut self, size: i16, screen_w: i16, screen_h: i16, margin: i16) -> Self {
        self.min_size = (size, size);
        self.pad_x = 0;
        self.pad_y = 0;
        self.centre_x = screen_w - margin - size / 2;
        self.top = screen_h - margin - size;
        self
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
        match self.tail.art_size() {
            Some((width, _)) => {
                let gap = if self.label.is_empty() {
                    0
                } else {
                    SPINNER_GAP
                };
                text + (gap + width) as i32 * self.scale as i32
            }
            None => text,
        }
    }

    /// Pixel height of the content: the label's row, or the spinner alone.
    fn content_height(&self) -> i32 {
        let rows = match self.tail.art_size() {
            Some((_, height)) if self.label.is_empty() => height as i32,
            _ => self.font.glyph_h as i32,
        };
        rows * self.scale as i32
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
                let top = text_top + (rows - SPINNER_H * scale) / 2;
                let gap = if style.label.is_empty() {
                    0
                } else {
                    SPINNER_GAP
                };
                spinner(sink, pen + gap * scale, top, scale, look.step, lit, unlit);
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

/// One spinner frame: the 2x3 grid, every dot drawn lit or unlit.
fn spinner(sink: &mut dyn Sink, x: i16, y: i16, scale: i16, frame: u8, lit: Rgb, unlit: Rgb) {
    let mask = SPINNER_FRAMES[frame as usize % SPINNER_FRAMES.len()];
    let mut dot = 0;
    while dot < 6 {
        let col = (dot / 3) as i16;
        let row = (dot % 3) as i16;
        let colour = if mask & (1 << dot) != 0 { lit } else { unlit };
        rect(
            sink,
            x + col * SPINNER_PITCH * scale,
            y + row * SPINNER_PITCH * scale,
            SPINNER_DOT * scale,
            SPINNER_DOT * scale,
            colour,
        );
        dot += 1;
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
