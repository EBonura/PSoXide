//! `hello-audio` -- the audio equivalent of hello-input.
//!
//! Face buttons, d-pad directions, and START trigger cooked `.psau`
//! gameplay SFX imported by `psxed audio-pack`:
//!
//! | Button   | Sample    |
//! |----------|-----------|
//! | CROSS    | Jump      |
//! | CIRCLE   | Coin      |
//! | TRIANGLE | Swoosh    |
//! | SQUARE   | Punch     |
//! | D-pad    | UI / step |
//! | START    | Explosion |
//!
//! Visual feedback:
//! - Background flashes the colour of whichever voice just triggered.
//! - Text rows show each button's state (playing / silent) + its
//!   configured pitch and waveform name.
//!
//! Proves the whole pad → SPU → DAC path: the controller IRQ
//! delivers button events, pad state drives key-on / key-off
//! writes to SPU voice registers, and the SPU's internal mixer
//! streams audio out through the bus' sample collector.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_asset::Audio;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_pad::{button, poll_port1, ButtonState};
use psx_spu::{self as spu, Adsr, SpuAddr, Voice, Volume};
use psx_vram::{Clut, TexDepth, Tpage};

/// Font atlas tpage -- past the 320-wide display buffers.
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

static JUMP_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/jump.psau");
static COIN_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/pickup_coin.psau");
static SWOOSH_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/swoosh.psau");
static PUNCH_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/hit_punch.psau");
static UI_SELECT_SFX: &[u8] =
    include_bytes!("../../../../assets/audio/freesfx/psau/ui_select.psau");
static METAL_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/hit_metal.psau");
static BEEP_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/ui_beep.psau");
static FOOTSTEP_SFX: &[u8] = include_bytes!("../../../../assets/audio/freesfx/psau/footstep.psau");
static EXPLOSION_SFX: &[u8] =
    include_bytes!("../../../../assets/audio/freesfx/psau/explosion_short.psau");

/// SPU RAM addresses. We park samples starting at 0x1010 -- the BIOS
/// convention is to leave 0x0000..0x1000 for system use, with the
/// required "zero" block at 0x1000.
const SPU_SAMPLE_BASE: u32 = 0x1010;
const FLASH_FRAMES: u8 = 12;
const SFX_COUNT: usize = 9;

struct SfxChannel {
    voice: Voice,
    button: u16,
    bytes: &'static [u8],
    volume: Volume,
    label: &'static str,
    tint: (u8, u8, u8),
}

const SFX_CHANNELS: [SfxChannel; SFX_COUNT] = [
    SfxChannel {
        voice: Voice::V0,
        button: button::CROSS,
        bytes: JUMP_SFX,
        volume: Volume::linear(1, 8),
        label: "CROSS    : jump",
        tint: (80, 160, 220),
    },
    SfxChannel {
        voice: Voice::V1,
        button: button::CIRCLE,
        bytes: COIN_SFX,
        volume: Volume::linear(1, 10),
        label: "CIRCLE   : coin",
        tint: (220, 170, 80),
    },
    SfxChannel {
        voice: Voice::V2,
        button: button::TRIANGLE,
        bytes: SWOOSH_SFX,
        volume: Volume::linear(1, 10),
        label: "TRIANGLE : swoosh",
        tint: (80, 220, 140),
    },
    SfxChannel {
        voice: Voice::V3,
        button: button::SQUARE,
        bytes: PUNCH_SFX,
        volume: Volume::linear(1, 10),
        label: "SQUARE   : punch",
        tint: (220, 90, 90),
    },
    SfxChannel {
        voice: Voice::V4,
        button: button::UP,
        bytes: UI_SELECT_SFX,
        volume: Volume::linear(1, 12),
        label: "UP       : select",
        tint: (140, 180, 240),
    },
    SfxChannel {
        voice: Voice::V5,
        button: button::RIGHT,
        bytes: METAL_SFX,
        volume: Volume::linear(1, 12),
        label: "RIGHT    : metal",
        tint: (180, 180, 190),
    },
    SfxChannel {
        voice: Voice::V6,
        button: button::DOWN,
        bytes: BEEP_SFX,
        volume: Volume::linear(1, 12),
        label: "DOWN     : beep",
        tint: (120, 220, 230),
    },
    SfxChannel {
        voice: Voice::V7,
        button: button::LEFT,
        bytes: FOOTSTEP_SFX,
        volume: Volume::linear(1, 8),
        label: "LEFT     : footstep",
        tint: (170, 130, 90),
    },
    SfxChannel {
        voice: Voice::new(8),
        button: button::START,
        bytes: EXPLOSION_SFX,
        volume: Volume::linear(1, 14),
        label: "START    : explosion",
        tint: (230, 120, 70),
    },
];

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);

    // Audio side init: SPU on, unmuted, main volume full.
    spu::init();

    // Upload each cooked PSAU sample into SPU RAM and configure its
    // voice once. PSAU payloads are ADPCM blocks, so advancing by the
    // byte length keeps the next start address aligned.
    let mut next_addr = SPU_SAMPLE_BASE;
    for ch in SFX_CHANNELS.iter() {
        let audio = Audio::from_bytes(ch.bytes).expect("psau sample");
        let addr = SpuAddr::new(next_addr);
        spu::upload_adpcm(addr, audio.adpcm_bytes());
        ch.voice
            .configure_sample(addr, audio.sample_rate_hz(), ch.volume, Adsr::sample());
        next_addr += audio.adpcm_bytes().len() as u32;
    }

    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    // Edge-detect pad state so we only key_on / key_off on transitions
    // -- otherwise we'd retrigger the attack phase every frame, which
    // sounds like a constant click.
    let mut prev_pad = ButtonState::NONE;
    let mut flashes = [0u8; SFX_COUNT];

    loop {
        let pad = poll_port1().buttons;

        // Compute which one-shot channels are newly pressed.
        let mut on_mask: u32 = 0;
        for (i, ch) in SFX_CHANNELS.iter().enumerate() {
            let now = pad.is_held(ch.button);
            let was = prev_pad.is_held(ch.button);
            if now && !was {
                on_mask |= ch.voice.mask();
                flashes[i] = FLASH_FRAMES;
            }
        }
        if on_mask != 0 {
            Voice::key_on(on_mask);
        }
        prev_pad = pad;
        for flash in flashes.iter_mut() {
            if *flash != 0 {
                *flash -= 1;
            }
        }

        // Visual feedback -- tint the background with the sum of
        // currently-active channel colours for a low-effort "you
        // can see what you hear" cue.
        let (r, g, b) = mix_background(&flashes);
        fb.clear(r, g, b);

        // Header.
        font.draw_text(4, 4, "hello-audio", (200, 200, 200));
        font.draw_text(
            4,
            14,
            "Press a mapped button to trigger a sample.",
            (140, 140, 140),
        );

        // Per-channel row: label + trigger indicator.
        let mut y: i16 = 32;
        for (i, ch) in SFX_CHANNELS.iter().enumerate() {
            let (line_tint, state_text) = if flashes[i] != 0 {
                (ch.tint, "PLAYING")
            } else {
                ((100, 100, 100), "silent ")
            };
            font.draw_text(4, y, ch.label, line_tint);
            font.draw_text(4 + 8 * 24, y, state_text, line_tint);
            y = y.wrapping_add(12);
        }

        // Footer hint.
        font.draw_text(
            4,
            220,
            "face buttons + d-pad + start trigger samples",
            (120, 120, 120),
        );

        gpu::draw_sync();
        gpu::vsync();
        fb.swap();
    }
}

/// Mix a background tint from recently-triggered samples. Additive
/// blend keeps simultaneous hits visible without covering the text.
fn mix_background(flashes: &[u8; SFX_COUNT]) -> (u8, u8, u8) {
    let mut r: u16 = 8;
    let mut g: u16 = 8;
    let mut b: u16 = 20;
    for (i, ch) in SFX_CHANNELS.iter().enumerate() {
        if flashes[i] != 0 {
            r = r.saturating_add((ch.tint.0 >> 2) as u16);
            g = g.saturating_add((ch.tint.1 >> 2) as u16);
            b = b.saturating_add((ch.tint.2 >> 2) as u16);
        }
    }
    (r.min(255) as u8, g.min(255) as u8, b.min(255) as u8)
}
