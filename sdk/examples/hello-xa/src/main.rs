//! `hello-xa` -- play XA-ADPCM songs from one interleaved file.
//!
//! The disc carries `SONGS.XA`: four generated test songs as the channels of
//! one 37.8 kHz stereo file, interleaved for single speed (every fourth
//! sector is one song). `make hello-xa-disc` builds it with
//! `psx-audio-cook xa-encode` and `mkisopsx --xa-file`.
//!
//! CROSS plays the next song from the top, SQUARE restarts this one, CIRCLE
//! stops it or starts it again, UP and DOWN change the volume. Songs loop.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_fmv::iso;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_io::cd::xa::{DriveSpeed, XaEvent, XaFile, XaPlayer};
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_pad::{button, poll_port1, ButtonState};
use psx_spu::{self as spu, CdVolume, Volume};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

/// The song file's name on the disc, and the numbers `xa-encode` printed in
/// its manifest: file number 1, single speed, four channels.
const SONGS_FILE: &str = "SONGS.XA";
const FILE_NUMBER: u8 = 1;
const SPEED: DriveSpeed = DriveSpeed::Single;
const SONG_NAMES: [&str; 4] = ["A MAJOR PAD", "G MAJOR HIGH", "BLIPS", "WHISTLE"];

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

/// Read one data sector (the directory lookup before any music plays).
fn read_one(lba: u32) -> Option<&'static [u8]> {
    // SAFETY: single-threaded use of the reader and its sector buffer.
    unsafe {
        let reader = &mut *addr_of_mut!(READER);
        if !reader.start_read(lba) {
            return None;
        }
        let ok = reader.read_sector(&mut *addr_of_mut!(SECTOR));
        reader.stop();
        ok.then(|| {
            core::slice::from_raw_parts(addr_of_mut!(SECTOR) as *const u8, SECTOR_WORDS * 4)
        })
    }
}

/// The song file's position on the disc, looked up by name so the program
/// does not bake in an LBA.
fn find_songs() -> Option<XaFile> {
    // SAFETY: nothing else drives the controller yet.
    if !unsafe { (*addr_of_mut!(READER)).prepare() } {
        return None;
    }
    let (root, _) = iso::root_directory(read_one(iso::PVD_LBA)?)?;
    let (lba, size) = iso::find_in_directory(read_one(root)?, SONGS_FILE)?;
    Some(XaFile::from_directory_entry(lba, size, FILE_NUMBER, SPEED))
}

/// Decimal digits of `value` into `buf`, as text.
fn number(buf: &mut [u8; 10], mut value: u32) -> &str {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[at..]).unwrap_or("?")
}

#[no_mangle]
fn main() {
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    spu::init();
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);

    let file = find_songs();
    let mut player = XaPlayer::new(peripherals.cd);
    let mut song = 0u8;
    let mut volume = 0x80u8;
    let mut loops = 0u32;
    let mut error = file.is_none();
    if let Some(file) = file {
        error = player.play(file.song(song), true).is_err();
        player.set_volume(volume, volume);
    }

    let mut previous = ButtonState::NONE;
    loop {
        let pad = poll_port1().buttons;
        if let Some(file) = file {
            if pad.pressed_since(previous, button::CROSS) {
                song = (song + 1) % SONG_NAMES.len() as u8;
                error = player.play(file.song(song), true).is_err();
            }
            if pad.pressed_since(previous, button::SQUARE) {
                error = player.play(file.song(song), true).is_err();
            }
            if pad.pressed_since(previous, button::CIRCLE) {
                if player.is_playing() {
                    player.stop();
                } else {
                    error = player.play(file.song(song), true).is_err();
                }
            }
            if pad.pressed_since(previous, button::UP) {
                volume = volume.saturating_add(16);
                player.set_volume(volume, volume);
            }
            if pad.pressed_since(previous, button::DOWN) {
                volume = volume.saturating_sub(16);
                player.set_volume(volume, volume);
            }
        }
        previous = pad;
        if player.poll() == XaEvent::Looped {
            loops += 1;
        }

        let bg = if error { (34, 10, 10) } else { (10, 24, 34) };
        fb.clear(&mut gpu, bg);
        font.draw_text(4, 4, "hello-xa", (200, 200, 200));
        font.draw_text(4, 18, "XA-ADPCM 37.8K STEREO, 4 SONGS", (120, 190, 230));
        font.draw_text(4, 42, "CROSS    next song", (130, 130, 130));
        font.draw_text(4, 54, "SQUARE   restart", (130, 130, 130));
        font.draw_text(4, 66, "CIRCLE   stop or play", (130, 130, 130));
        font.draw_text(4, 78, "UP/DOWN  volume", (130, 130, 130));
        let mut digits = [0u8; 10];
        font.draw_text(4, 110, "SONG", (160, 160, 160));
        font.draw_text(48, 110, number(&mut digits, song as u32), (240, 240, 240));
        font.draw_text(64, 110, SONG_NAMES[song as usize], (240, 240, 240));
        let (state, tint) = match (error, player.is_playing(), player.is_streaming()) {
            (true, _, _) => ("ERROR", (230, 100, 100)),
            (_, false, _) => ("STOPPED", (230, 190, 80)),
            (_, true, false) => ("SEEKING", (170, 140, 230)),
            (_, true, true) => ("PLAYING", (80, 220, 150)),
        };
        font.draw_text(4, 124, "STATE", (160, 160, 160));
        font.draw_text(48, 124, state, tint);
        font.draw_text(4, 138, "TIME", (160, 160, 160));
        font.draw_text(48, 138, number(&mut digits, player.elapsed_millis()), (240, 240, 240));
        font.draw_text(4, 152, "LOOPS", (160, 160, 160));
        font.draw_text(48, 152, number(&mut digits, loops), (240, 240, 240));
        font.draw_text(4, 166, "VOLUME", (160, 160, 160));
        font.draw_text(56, 166, number(&mut digits, volume as u32), (240, 240, 240));

        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);
    }
}
