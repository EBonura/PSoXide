//! Phosphor icon codepoints (subset used by PSoXide UI).
//!
//! Keep this file small -- add codepoints only when a panel actually uses
//! them. The full Phosphor set is in `assets/fonts/Phosphor.ttf` (regular)
//! and `Phosphor-Fill.ttf` (solid, used for active toggles). Both share the
//! same codepoints. Names below match the upstream `ph-<name>` glyph.

#![allow(dead_code)]

use egui::{FontFamily, FontId, RichText};

pub const PLAY: char = '\u{e3d0}'; // ph-play
pub const PAUSE: char = '\u{e39e}'; // ph-pause
pub const SQUARE: char = '\u{e46c}'; // ph-stop
pub const SKIP_FORWARD: char = '\u{e5a6}'; // ph-skip-forward

pub const BUG: char = '\u{e5f4}'; // ph-bug
pub const TERMINAL: char = '\u{eae8}'; // ph-terminal-window
pub const HASH: char = '\u{e2a2}'; // ph-hash

pub const MONITOR: char = '\u{e32e}'; // ph-monitor
pub const MAXIMIZE: char = '\u{e1d0}'; // ph-corners-out
pub const MINIMIZE: char = '\u{e1ce}'; // ph-corners-in
pub const CPU: char = '\u{e610}'; // ph-cpu
pub const LAYERS: char = '\u{e466}'; // ph-stack
/// Texture-filter toggle -- a funnel reads as "filter".
pub const FILTER: char = '\u{e266}'; // ph-funnel
pub const HARD_DRIVE: char = '\u{e29e}'; // ph-hard-drive
pub const GAMEPAD_2: char = '\u{e26e}'; // ph-game-controller
pub const KEYBOARD: char = '\u{e2d8}'; // ph-keyboard
/// Wireframe toggle -- polygon outline.
pub const GRID: char = '\u{e6d0}'; // ph-polygon
pub const SAVE: char = '\u{e248}'; // ph-floppy-disk
pub const VOLUME_1: char = '\u{e44c}'; // ph-speaker-low
pub const VOLUME_2: char = '\u{e44a}'; // ph-speaker-high
pub const VOLUME_X: char = '\u{e45c}'; // ph-speaker-x

pub const EYE: char = '\u{e220}'; // ph-eye
pub const EYE_OFF: char = '\u{e224}'; // ph-eye-slash

/// Hide-toolbar chevron (slides the bar up out of view).
pub const CARET_UP: char = '\u{e13c}'; // ph-caret-up
/// Restore-toolbar chevron (floating tab pulls the bar back down).
pub const CARET_DOWN: char = '\u{e136}'; // ph-caret-down

/// Power-off / quit icon -- used for the rightmost Menu category
/// so "close the app" has its own place instead of hiding inside
/// Debug.
pub const POWER: char = '\u{e3da}'; // ph-power
/// Folder icon -- Projects/Editor category badge.
pub const FOLDER: char = '\u{e24a}'; // ph-folder
/// Refresh/rotate icon -- reset + rescan-library action.
pub const ROTATE_CCW: char = '\u{e038}'; // ph-arrow-counter-clockwise
/// Disc icon -- Games category badge + fast-boot toggle.
pub const DISC: char = '\u{e564}'; // ph-disc

/// Phosphor regular (outline) FontId at a given size.
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("phosphor".into()))
}

/// Phosphor fill (solid) FontId -- use for active toggle glyphs.
pub fn font_fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("phosphor-fill".into()))
}

/// Icon as RichText at a given size (regular weight).
pub fn text(ch: char, size: f32) -> RichText {
    RichText::new(ch.to_string()).font(font(size))
}
