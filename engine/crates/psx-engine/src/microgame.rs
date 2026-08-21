// SPDX-License-Identifier: GPL-2.0-or-later
//! Small-game front end shared by the demo-disc arcade games.
//!
//! The shell owns only presentation flow and common preferences. The game
//! still owns its simulation and renderer, and reacts to [`MicrogameAction`]
//! events at clean boundaries.

use psx_font::FontAtlas;
use psx_settings::Profile;

use crate::{button, Ctx};

const TITLE_ROWS: usize = 3;
const PAUSE_ROWS: usize = 3;

/// Visible shell state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MicrogameScreen {
    /// Title and common settings.
    Title,
    /// Simulation is live; the shell draws no overlay.
    Playing,
    /// Simulation is held behind the pause panel.
    Paused,
    /// Round result and persistent best score.
    Results,
}

/// Boundary event returned by [`MicrogameShell::update`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MicrogameAction {
    /// No simulation transition this tick.
    None,
    /// Start a fresh game from the title.
    Start,
    /// Resume the held game.
    Resume,
    /// Restart the current game from scratch.
    Restart,
    /// Return to the title screen.
    ReturnToTitle,
}

/// Shared title, pause, results, difficulty and high-score controller.
pub struct MicrogameShell<const SCORES: usize> {
    profile: Profile<0, SCORES>,
    screen: MicrogameScreen,
    title_row: usize,
    pause_row: usize,
    result_score: u32,
    dirty: bool,
}

impl<const SCORES: usize> MicrogameShell<SCORES> {
    /// Create the shell with caller-provided shipped defaults.
    pub const fn new(profile: Profile<0, SCORES>) -> Self {
        Self {
            profile,
            screen: MicrogameScreen::Title,
            title_row: 0,
            pause_row: 0,
            result_score: 0,
            dirty: false,
        }
    }

    /// Replace defaults with a valid persisted profile.
    pub fn set_profile(&mut self, mut profile: Profile<0, SCORES>) {
        profile.sanitize();
        if SCORES > 0 {
            profile.difficulty = profile.difficulty.min((SCORES - 1) as u8);
        } else {
            profile.difficulty = 0;
        }
        self.profile = profile;
        self.dirty = false;
    }

    /// Current persistent profile.
    pub const fn profile(&self) -> &Profile<0, SCORES> {
        &self.profile
    }

    /// Current visible shell state.
    pub const fn screen(&self) -> MicrogameScreen {
        self.screen
    }

    /// Selected difficulty index.
    pub const fn difficulty(&self) -> usize {
        self.profile.difficulty as usize
    }

    /// Whether the game simulation should advance.
    pub const fn is_playing(&self) -> bool {
        matches!(self.screen, MicrogameScreen::Playing)
    }

    /// Consume the persistence-dirty flag.
    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    /// Request another persistence attempt after a transient card failure.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Advance front-end input and return a simulation boundary event.
    pub fn update(&mut self, ctx: &Ctx) -> MicrogameAction {
        match self.screen {
            MicrogameScreen::Title => self.update_title(ctx),
            MicrogameScreen::Playing => {
                if ctx.just_pressed(button::START) {
                    self.pause_row = 0;
                    self.screen = MicrogameScreen::Paused;
                }
                MicrogameAction::None
            }
            MicrogameScreen::Paused => self.update_pause(ctx),
            MicrogameScreen::Results => {
                if ctx.just_pressed(button::CIRCLE) {
                    self.screen = MicrogameScreen::Title;
                    MicrogameAction::ReturnToTitle
                } else if ctx.just_pressed(button::CROSS) || ctx.just_pressed(button::START) {
                    self.screen = MicrogameScreen::Playing;
                    MicrogameAction::Restart
                } else {
                    MicrogameAction::None
                }
            }
        }
    }

    fn update_title(&mut self, ctx: &Ctx) -> MicrogameAction {
        if ctx.just_pressed(button::UP) {
            self.title_row = (self.title_row + TITLE_ROWS - 1) % TITLE_ROWS;
        } else if ctx.just_pressed(button::DOWN) {
            self.title_row = (self.title_row + 1) % TITLE_ROWS;
        }
        let step = if ctx.just_pressed(button::RIGHT) {
            1
        } else if ctx.just_pressed(button::LEFT) {
            -1
        } else {
            0
        };
        if step != 0 {
            match self.title_row {
                1 if SCORES > 0 => {
                    let count = SCORES as i16;
                    self.profile.difficulty =
                        ((self.profile.difficulty as i16 + step + count) % count) as u8;
                    self.dirty = true;
                }
                2 => {
                    self.profile.sfx_volume = step_percent(self.profile.sfx_volume, step);
                    self.dirty = true;
                }
                _ => {}
            }
        }
        if ctx.just_pressed(button::CROSS) || ctx.just_pressed(button::START) {
            if self.title_row == 0 {
                self.screen = MicrogameScreen::Playing;
                return MicrogameAction::Start;
            }
            // X on a setting behaves like Right.
            match self.title_row {
                1 if SCORES > 0 => {
                    self.profile.difficulty = (self.profile.difficulty + 1) % SCORES as u8;
                    self.dirty = true;
                }
                2 => {
                    self.profile.sfx_volume = step_percent(self.profile.sfx_volume, 1);
                    self.dirty = true;
                }
                _ => {}
            }
        }
        MicrogameAction::None
    }

    fn update_pause(&mut self, ctx: &Ctx) -> MicrogameAction {
        if ctx.just_pressed(button::START) || ctx.just_pressed(button::CIRCLE) {
            self.screen = MicrogameScreen::Playing;
            return MicrogameAction::Resume;
        }
        if ctx.just_pressed(button::UP) {
            self.pause_row = (self.pause_row + PAUSE_ROWS - 1) % PAUSE_ROWS;
        } else if ctx.just_pressed(button::DOWN) {
            self.pause_row = (self.pause_row + 1) % PAUSE_ROWS;
        }
        if !ctx.just_pressed(button::CROSS) {
            return MicrogameAction::None;
        }
        match self.pause_row {
            0 => {
                self.screen = MicrogameScreen::Playing;
                MicrogameAction::Resume
            }
            1 => {
                self.screen = MicrogameScreen::Playing;
                MicrogameAction::Restart
            }
            _ => {
                self.screen = MicrogameScreen::Title;
                MicrogameAction::ReturnToTitle
            }
        }
    }

    /// End the round, update the selected high-score slot, and open results.
    pub fn finish(&mut self, score: u32) {
        self.result_score = score;
        if self.profile.submit_score(self.difficulty(), score) {
            self.dirty = true;
        }
        self.screen = MicrogameScreen::Results;
    }

    /// Draw the shell over the game's existing frame.
    pub fn draw(&self, font: &FontAtlas, title: &str) {
        match self.screen {
            MicrogameScreen::Playing => {}
            MicrogameScreen::Title => self.draw_title(font, title),
            MicrogameScreen::Paused => self.draw_pause(font),
            MicrogameScreen::Results => self.draw_results(font),
        }
    }

    fn draw_title(&self, font: &FontAtlas, title: &str) {
        panel(28, 32, 264, 184);
        centered(font, 48, title, (255, 226, 132));
        centered(font, 67, "PSOXIDE ARCADE", (112, 184, 232));
        let best = self
            .profile
            .high_scores
            .get(self.difficulty())
            .copied()
            .unwrap_or(0);
        let labels = ["PLAY", "DIFFICULTY", "SFX VOLUME"];
        let mut row = 0;
        while row < TITLE_ROWS {
            let y = 94 + row as i16 * 22;
            if row == self.title_row {
                psx_gpu::draw_rect_flat(52, y - 3, 216, 18, 36, 58, 90);
            }
            font.draw_text(64, y, labels[row], row_color(row == self.title_row));
            match row {
                1 => font.draw_text(208, y, difficulty_name(self.difficulty()), (236, 236, 176)),
                2 => draw_percent(font, 224, y, self.profile.sfx_volume),
                _ => {}
            }
            row += 1;
        }
        font.draw_text(64, 174, "BEST", (136, 156, 190));
        draw_number(font, 216, 174, best, (245, 231, 146));
        centered(font, 196, "X SELECT   D-PAD ADJUST", (142, 148, 166));
    }

    fn draw_pause(&self, font: &FontAtlas) {
        panel(68, 65, 184, 112);
        centered(font, 78, "PAUSED", (255, 226, 132));
        let labels = ["RESUME", "RESTART", "TITLE"];
        for (row, label) in labels.iter().enumerate() {
            let y = 106 + row as i16 * 20;
            if row == self.pause_row {
                psx_gpu::draw_rect_flat(88, y - 3, 144, 18, 36, 58, 90);
            }
            centered(font, y, label, row_color(row == self.pause_row));
        }
    }

    fn draw_results(&self, font: &FontAtlas) {
        panel(52, 66, 216, 112);
        centered(font, 80, "ROUND COMPLETE", (255, 226, 132));
        font.draw_text(76, 111, "SCORE", (150, 176, 212));
        draw_number(font, 196, 111, self.result_score, (248, 236, 154));
        font.draw_text(76, 133, "BEST", (150, 176, 212));
        let best = self
            .profile
            .high_scores
            .get(self.difficulty())
            .copied()
            .unwrap_or(0);
        draw_number(font, 196, 133, best, (248, 236, 154));
        centered(font, 157, "X REPLAY   O TITLE", (142, 148, 166));
    }
}

fn step_percent(value: u8, step: i16) -> u8 {
    let value = value as i16 + step * 25;
    if value < 25 {
        100
    } else if value > 100 {
        25
    } else {
        value as u8
    }
}

fn difficulty_name(value: usize) -> &'static str {
    match value {
        0 => "EASY",
        1 => "NORMAL",
        _ => "HARD",
    }
}

fn panel(x: i16, y: i16, w: u16, h: u16) {
    psx_gpu::draw_rect_flat(x - 2, y - 2, w + 4, h + 4, 3, 7, 16);
    psx_gpu::draw_rect_flat(x, y, w, h, 12, 20, 38);
    psx_gpu::draw_rect_flat(x, y, w, 2, 78, 116, 158);
    psx_gpu::draw_rect_flat(x, y + h as i16 - 2, w, 2, 5, 9, 20);
}

fn row_color(selected: bool) -> (u8, u8, u8) {
    if selected {
        (255, 238, 170)
    } else {
        (188, 202, 222)
    }
}

fn centered(font: &FontAtlas, y: i16, text: &str, color: (u8, u8, u8)) {
    let x = (320 - text.len() as i16 * 8) / 2;
    font.draw_text(x, y, text, color);
}

fn draw_percent(font: &FontAtlas, x: i16, y: i16, value: u8) {
    let mut bytes = [b' '; 4];
    let len = if value == 100 {
        bytes.copy_from_slice(b"100%");
        4
    } else {
        bytes[0] = b'0' + value / 10;
        bytes[1] = b'0' + value % 10;
        bytes[2] = b'%';
        3
    };
    let text = unsafe { core::str::from_utf8_unchecked(&bytes[..len]) };
    font.draw_text(x - len as i16 * 8, y, text, (236, 236, 176));
}

fn draw_number(font: &FontAtlas, x: i16, y: i16, value: u32, color: (u8, u8, u8)) {
    let mut bytes = [b'0'; 10];
    let mut value = value;
    let mut start = bytes.len() - 1;
    bytes[start] = b'0' + (value % 10) as u8;
    value /= 10;
    while value != 0 && start > 0 {
        start -= 1;
        bytes[start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let text = unsafe { core::str::from_utf8_unchecked(&bytes[start..]) };
    font.draw_text(x - text.len() as i16 * 8, y, text, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_pad::{ButtonState, PadMode, PadState};

    fn ctx(current: u16, previous: u16) -> Ctx {
        Ctx::new(
            crate::SimTick::ZERO,
            crate::VisualFrame::ZERO,
            crate::VideoHz::NTSC,
            PadState {
                buttons: ButtonState::from_bits(current),
                mode: PadMode::Digital,
                ..PadState::NONE
            },
            PadState {
                buttons: ButtonState::from_bits(previous),
                mode: PadMode::Digital,
                ..PadState::NONE
            },
            psx_gpu::framebuf::FrameBuffer::new(320, 240),
        )
    }

    #[test]
    fn title_pause_results_flow_is_explicit() {
        let mut shell = MicrogameShell::<3>::new(Profile::new(psx_pad::ActionMap::new([])));
        assert_eq!(shell.update(&ctx(button::CROSS, 0)), MicrogameAction::Start);
        assert!(shell.is_playing());
        assert_eq!(shell.update(&ctx(button::START, 0)), MicrogameAction::None);
        assert_eq!(shell.screen(), MicrogameScreen::Paused);
        assert_eq!(
            shell.update(&ctx(button::START, 0)),
            MicrogameAction::Resume
        );
        shell.finish(42);
        assert_eq!(shell.profile().high_scores[1], 42);
        assert_eq!(
            shell.update(&ctx(button::CIRCLE, 0)),
            MicrogameAction::ReturnToTitle
        );
    }
}
