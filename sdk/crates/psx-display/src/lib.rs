// SPDX-License-Identifier: GPL-2.0-or-later
//! Picture options shared by PSoXide games: BRIGHTNESS and SCREEN X/Y.
//!
//! Each option is a small value that steps left and right, reads `DEFAULT`
//! (or `CENTRE`) at the middle, and says how far it has moved in words:
//! `DARKER 2`, `BRIGHTER 4`, `LEFT 3`, `DOWN 1`. The crate holds the value
//! types, the step and clamp rules, the row text, the one-byte save
//! encoding, and the way each one is put on screen. It owns no hardware and
//! no heap: [`Brightness::overlay`] gives a plain packet for the game to
//! link, and [`ScreenOffset::apply_to`] edits a `DisplayConfig` the game
//! already has.
//!
//! The adoption guide, game by game, is `sdk/docs/DISPLAY-OPTIONS.md`.
//!
//! ```
//! use psx_display::{Brightness, ScreenOffset};
//! use psx_gpu::display::Resolution;
//!
//! // A row handler: right on the pad steps the setting, the row reads it.
//! let mut brightness = Brightness::DEFAULT;
//! assert_eq!(brightness.label().as_str(), "DEFAULT");
//! brightness = brightness.stepped(1);
//! assert_eq!(brightness.label().as_str(), "BRIGHTER 1");
//!
//! // Per frame: nothing at the default, one packet otherwise.
//! assert!(Brightness::DEFAULT.overlay(Resolution::R320X240).is_none());
//! assert!(brightness.overlay(Resolution::R320X240).is_some());
//!
//! // The save byte is the step in two's complement.
//! assert_eq!(Brightness::from_byte(brightness.to_byte()), brightness);
//! assert!(ScreenOffset::<16>::CENTRE.is_centre());
//! ```

#![no_std]
#![warn(missing_docs)]

mod brightness;
mod label;
mod offset;

pub use brightness::{Brightness, BrightnessOverlay};
pub use label::Label;
pub use offset::ScreenOffset;
