//! `hello-cdda` -- play Red Book audio from track 2.
//!
//! Build a mixed-mode disc with `mkisopsx --cdda-track`; this demo
//! assumes track 2 is `GONCHAROV`.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_io::cdrom;
use psx_pad::{button, poll_port1, ButtonState};
use psx_spu::{self as spu, CdVolume, Volume};
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const TRACK_GONCHAROV: u8 = 2;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Playback {
    Playing,
    Paused,
    Stopped,
    Muted,
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);

    spu::init();
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);

    cdrom::set_mode(cdrom::MODE_DOUBLE_SPEED | cdrom::MODE_CDDA);
    cdrom::demute();
    cdrom::play_track(TRACK_GONCHAROV);

    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    let mut prev_pad = ButtonState::NONE;
    let mut playback = Playback::Playing;

    loop {
        let pad = poll_port1().buttons;
        if pressed(pad, prev_pad, button::START) {
            cdrom::demute();
            cdrom::play_track(TRACK_GONCHAROV);
            playback = Playback::Playing;
        }
        if pressed(pad, prev_pad, button::CROSS) {
            cdrom::pause();
            playback = Playback::Paused;
        }
        if pressed(pad, prev_pad, button::SQUARE) {
            cdrom::stop();
            playback = Playback::Stopped;
        }
        if pressed(pad, prev_pad, button::TRIANGLE) {
            cdrom::mute();
            playback = Playback::Muted;
        }
        if pressed(pad, prev_pad, button::CIRCLE) {
            cdrom::demute();
            playback = Playback::Playing;
        }
        prev_pad = pad;

        let bg = match playback {
            Playback::Playing => (10, 24, 34),
            Playback::Paused => (28, 24, 12),
            Playback::Stopped => (24, 12, 16),
            Playback::Muted => (18, 16, 28),
        };
        fb.clear(bg.0, bg.1, bg.2);

        font.draw_text(4, 4, "hello-cdda", (200, 200, 200));
        font.draw_text(4, 18, "TRACK 02: GONCHAROV", (120, 190, 230));
        font.draw_text(4, 42, "START    play from top", (130, 130, 130));
        font.draw_text(4, 54, "CROSS    pause", (130, 130, 130));
        font.draw_text(4, 66, "SQUARE   stop", (130, 130, 130));
        font.draw_text(4, 78, "TRIANGLE mute", (130, 130, 130));
        font.draw_text(4, 90, "CIRCLE   demute", (130, 130, 130));

        let (status, tint) = match playback {
            Playback::Playing => ("PLAYING", (80, 220, 150)),
            Playback::Paused => ("PAUSED ", (230, 190, 80)),
            Playback::Stopped => ("STOPPED", (230, 100, 100)),
            Playback::Muted => ("MUTED  ", (170, 140, 230)),
        };
        font.draw_text(4, 122, "CDDA:", (160, 160, 160));
        font.draw_text(56, 122, status, tint);
        font.draw_text(4, 220, "mixed-mode disc audio stream", (110, 110, 110));

        gpu::draw_sync();
        gpu::vsync();
        fb.swap();
    }
}

fn pressed(now: ButtonState, prev: ButtonState, button: u16) -> bool {
    now.is_held(button) && !prev.is_held(button)
}
