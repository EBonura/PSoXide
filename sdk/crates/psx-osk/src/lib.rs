// SPDX-License-Identifier: GPL-2.0-or-later
//! A PS4-style on-screen keyboard: QWERTY, a Shift toggle for case, a symbols
//! page, a wide Space bar, plus Backspace and OK keys, with a boxed selection.
//! The D-pad moves the highlight; the caller feeds it [`Keyboard::activate`].
//!
//! Promoted from PSXcel, which built it for cell text entry; any game that
//! names saves, enters high scores, or takes chat text wants the same thing
//! (zelda3-psx hand-rolled a name-entry grid; gh-psx will want score names).
//! Nav between rows maps by horizontal key centre so a wide key (Space)
//! lines up sensibly with the narrow keys above it.
//!
//! Rendering assumes a 320-wide display and psx-font's 8x8 glyph cell; the
//! keyboard occupies the bottom [`PANEL_H`] pixels. Colors come from a
//! caller-supplied [`Palette`], so any game theme drops in.
//!
//! ```ignore
//! let mut kb = Keyboard::new();
//! // per frame:
//! if pad.repeats(button::LEFT, 14, 3) { kb.step(Dir::Left); }
//! if pad.just_pressed(button::CROSS) {
//!     match kb.activate() {
//!         Action::Insert(ch) => text.push(ch),
//!         Action::Backspace => text.pop(),
//!         Action::Commit => return Some(text),
//!         Action::None => {}
//!     }
//! }
//! kb.draw(&font, &Palette::GRAY, "X:Type  SQ:Del  R2:OK");
//! ```

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use psx_font::FontAtlas;
use psx_gpu::draw_rect_flat;

/// What pressing the highlighted key does. Toggles (Shift/Sym) return `None`.
pub enum Action {
    /// Nothing observable (a page/case toggle happened internally).
    None,
    /// Insert this ASCII byte at the caret.
    Insert(u8),
    /// Delete one character before the caret.
    Backspace,
    /// The OK key: accept the text.
    Commit,
}

/// D-pad direction for [`Keyboard::step`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Move the highlight left (wraps).
    Left,
    /// Move the highlight right (wraps).
    Right,
    /// Move the highlight up (wraps, centre-aligned).
    Up,
    /// Move the highlight down (wraps, centre-aligned).
    Down,
}

/// Colors for the keyboard panel; all `(r, g, b)`.
#[derive(Copy, Clone, Debug)]
pub struct Palette {
    /// Panel background behind the keys.
    pub panel: (u8, u8, u8),
    /// Idle key face.
    pub key: (u8, u8, u8),
    /// Latched toggle key face (Shift/Sym while active).
    pub hot: (u8, u8, u8),
    /// Highlighted (selected) key face.
    pub accent: (u8, u8, u8),
    /// Label color on the highlighted key.
    pub accent_text: (u8, u8, u8),
    /// Label color on idle keys.
    pub text: (u8, u8, u8),
    /// Hint-line color.
    pub dim: (u8, u8, u8),
}

impl Palette {
    /// A neutral gray scheme that reads on any background.
    pub const GRAY: Self = Self {
        panel: (24, 24, 28),
        key: (52, 52, 60),
        hot: (90, 70, 30),
        accent: (200, 200, 210),
        accent_text: (16, 16, 20),
        text: (220, 220, 225),
        dim: (140, 140, 150),
    };
}

#[derive(Copy, Clone)]
enum Key {
    Ch(u8),
    Shift,
    Sym,
    Space,
    Del,
    Ok,
}

// Four character rows per page, ten keys each; row 4 is the function row.
const LETTERS: [&[u8; 10]; 4] = [b"1234567890", b"QWERTYUIOP", b"ASDFGHJKL'", b"ZXCVBNM.,:"];
const SYMBOLS: [&[u8; 10]; 4] = [b"1234567890", b"+-*/=()%^$", b".,:;!?@#&|", b"<>[]{}\"'\\`"];

// Function row: (key, unit_start, unit_span) inside the 10-unit column grid.
const FROW: [(Key, i16, i16); 5] = [
    (Key::Shift, 0, 2),
    (Key::Sym, 2, 2),
    (Key::Space, 4, 3),
    (Key::Del, 7, 1),
    (Key::Ok, 8, 2),
];

const ROWS: usize = 5;
const X0: i16 = 8;
const UW: i16 = 30; // unit width (10 units = 300px)
const KH: i16 = 18; // key height
/// Height of the whole keyboard panel (keys + hint line).
pub const PANEL_H: i16 = ROWS as i16 * KH + 14;
/// Top edge of the keyboard panel on a 240-line display.
pub const Y0: i16 = 240 - ROWS as i16 * KH - 14;

/// The keyboard state machine: highlight position + page/case toggles.
pub struct Keyboard {
    row: usize,
    col: usize,
    shift: bool,
    sym: bool,
}

impl Keyboard {
    /// Fresh keyboard homed on `Q`, lowercase, letters page (PS4 default).
    pub const fn new() -> Self {
        Keyboard {
            row: 1,
            col: 0,
            shift: false,
            sym: false,
        }
    }

    fn row_len(&self, r: usize) -> usize {
        if r == 4 {
            FROW.len()
        } else {
            10
        }
    }

    /// (unit_start, unit_span) of key (r, c).
    fn span(&self, r: usize, c: usize) -> (i16, i16) {
        if r == 4 {
            let (_, s, w) = FROW[c];
            (s, w)
        } else {
            (c as i16, 1)
        }
    }

    /// Doubled centre column (avoids halves) for row-to-row alignment.
    fn centre2(&self, r: usize, c: usize) -> i16 {
        let (s, w) = self.span(r, c);
        2 * s + w
    }

    fn key_at(&self, r: usize, c: usize) -> Key {
        if r == 4 {
            FROW[c].0
        } else {
            let page = if self.sym { &SYMBOLS } else { &LETTERS };
            Key::Ch(page[r][c])
        }
    }

    fn cased(&self, c: u8) -> u8 {
        if !self.shift && c.is_ascii_uppercase() {
            c + 32
        } else {
            c
        }
    }

    /// Step the highlight one cell, wrapping across every edge.
    pub fn step(&mut self, dir: Dir) {
        let n = self.row_len(self.row);
        match dir {
            Dir::Left => self.col = (self.col + n - 1) % n,
            Dir::Right => self.col = (self.col + 1) % n,
            Dir::Up | Dir::Down => {
                let nr = if dir == Dir::Up {
                    (self.row + ROWS - 1) % ROWS
                } else {
                    (self.row + 1) % ROWS
                };
                let want = self.centre2(self.row, self.col);
                self.row = nr;
                self.col = self.nearest_col(nr, want);
            }
        }
    }

    fn nearest_col(&self, r: usize, want2: i16) -> usize {
        let mut best = 0;
        let mut best_d = i16::MAX;
        for c in 0..self.row_len(r) {
            let d = (self.centre2(r, c) - want2).abs();
            if d < best_d {
                best_d = d;
                best = c;
            }
        }
        best
    }

    /// Activate the highlighted key.
    pub fn activate(&mut self) -> Action {
        match self.key_at(self.row, self.col) {
            Key::Ch(c) => Action::Insert(self.cased(c)),
            Key::Space => Action::Insert(b' '),
            Key::Del => Action::Backspace,
            Key::Ok => Action::Commit,
            Key::Shift => {
                self.shift = !self.shift;
                Action::None
            }
            Key::Sym => {
                self.sym = !self.sym;
                Action::None
            }
        }
    }

    /// Draw the panel, keys, and `hint` line with the caller's palette.
    /// Immediate GP0 quads + psx-font text; call after the scene draw.
    pub fn draw(&self, font: &FontAtlas, p: &Palette, hint: &str) {
        draw_rect_flat(
            0,
            Y0 - 14,
            320,
            (240 - (Y0 - 14)) as u16,
            p.panel.0,
            p.panel.1,
            p.panel.2,
        );
        font.draw_text(6, Y0 - 12, hint, p.dim);

        for r in 0..ROWS {
            for c in 0..self.row_len(r) {
                let (s, w) = self.span(r, c);
                let x = X0 + s * UW;
                let y = Y0 + (r as i16) * KH;
                let kw = w * UW;
                let sel = r == self.row && c == self.col;
                let key = self.key_at(r, c);

                let active = matches!((key, self.shift), (Key::Shift, true))
                    || matches!((key, self.sym), (Key::Sym, true));
                let bg = if sel {
                    p.accent
                } else if active {
                    p.hot
                } else {
                    p.key
                };
                draw_rect_flat(
                    x + 1,
                    y + 1,
                    (kw - 2) as u16,
                    (KH - 2) as u16,
                    bg.0,
                    bg.1,
                    bg.2,
                );

                let mut buf = [0u8; 6];
                let label: &[u8] = match key {
                    Key::Ch(ch) => {
                        buf[0] = self.cased(ch);
                        &buf[..1]
                    }
                    Key::Shift => b"SH",
                    Key::Sym => {
                        if self.sym {
                            b"ABC"
                        } else {
                            b"@#"
                        }
                    }
                    Key::Space => b"SPACE",
                    Key::Del => b"DEL",
                    Key::Ok => b"OK",
                };
                let tint = if sel { p.accent_text } else { p.text };
                let tw = (label.len() as i16) * 8;
                font.draw_text(x + kw / 2 - tw / 2, y + (KH - 8) / 2, str_of(label), tint);
            }
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

fn str_of(b: &[u8]) -> &str {
    // Labels are ASCII (the key tables + the function-key words).
    core::str::from_utf8(b).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_of(a: Action) -> Option<u8> {
        match a {
            Action::Insert(c) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn homes_on_q_lowercase() {
        let mut kb = Keyboard::new();
        assert_eq!(insert_of(kb.activate()), Some(b'q'));
    }

    #[test]
    fn shift_uppercases_until_toggled_off() {
        let mut kb = Keyboard::new();
        // Navigate to the function row's Shift key: home is (1,0); Up wraps
        // to the function row (row 0 is digits, so go down 3 to row 4 col 0).
        for _ in 0..3 {
            kb.step(Dir::Down);
        }
        assert!(matches!(kb.activate(), Action::None)); // Shift on
        for _ in 0..3 {
            kb.step(Dir::Up);
        }
        assert_eq!(insert_of(kb.activate()), Some(b'Q'));
    }

    #[test]
    fn sym_page_swaps_tables() {
        let mut kb = Keyboard::new();
        for _ in 0..3 {
            kb.step(Dir::Down); // to function row
        }
        kb.step(Dir::Right); // Shift -> Sym
        assert!(matches!(kb.activate(), Action::None));
        kb.step(Dir::Up); // back into the grid, symbols page
        let got = insert_of(kb.activate()).unwrap();
        assert!(SYMBOLS.iter().any(|row| row.contains(&got)));
    }

    #[test]
    fn horizontal_wrap() {
        let mut kb = Keyboard::new();
        kb.step(Dir::Left); // from col 0 wraps to col 9
        assert_eq!(insert_of(kb.activate()), Some(b'p'));
    }

    #[test]
    fn space_del_ok_actions() {
        let mut kb = Keyboard::new();
        for _ in 0..3 {
            kb.step(Dir::Down);
        }
        // Function row: Shift(0-2) Sym(2-4) Space(4-7) Del(7-8) Ok(8-10)
        kb.step(Dir::Right);
        kb.step(Dir::Right);
        assert_eq!(insert_of(kb.activate()), Some(b' '));
        kb.step(Dir::Right);
        assert!(matches!(kb.activate(), Action::Backspace));
        kb.step(Dir::Right);
        assert!(matches!(kb.activate(), Action::Commit));
    }

    #[test]
    fn vertical_nav_is_centre_aligned() {
        let mut kb = Keyboard::new();
        // Park on Space (function row, wide key), then go Up: should land
        // near the middle of the letter row above (col ~4-6), not col 2.
        for _ in 0..3 {
            kb.step(Dir::Down);
        }
        kb.step(Dir::Right);
        kb.step(Dir::Right); // Space
        kb.step(Dir::Up);
        let got = insert_of(kb.activate()).unwrap();
        assert!(b"vbnm".contains(&got), "landed on {}", got as char);
    }
}
