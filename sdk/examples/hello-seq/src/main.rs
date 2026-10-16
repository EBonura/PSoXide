//! `hello-seq` -- plays a cooked song through `psx-seq` and reports what the
//! sequencer cost.
//!
//! The song and its bank are cooked from the player's own cartridge by
//! `psx-seq-cook cook` and are never committed. Point `PSX_SEQ_DIR` at the
//! directory holding `song.psq` and `bank.psqb` when building:
//!
//! ```text
//! PSX_SEQ_DIR=/path/to/cooked cargo build --release ...
//! ```
//!
//! Run headless with `frontend launch --path hello-seq.exe --steps N
//! --dump-audio out.wav` and read the `hello-seq:` lines on the TTY. Every
//! 180 Hz update is timed on root counter 2 (the system clock over 8), so the
//! cost figures are the guest's own.
#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use psx_rt::tty;
use psx_seq::format::Bank;
use psx_seq::{Sequencer, UPDATE_HZ};
use psx_sfx::hw::Hw;
use psx_sfx::Sample;
use psx_spu::{Adsr, Pitch, Spu, SpuAddr, Voice, Volume, SILENCE_BLOCK};

static SONG: &[u8] = include_bytes!(concat!(env!("PSX_SEQ_DIR"), "/song.psq"));
static BANK: &[u8] = include_bytes!(concat!(env!("PSX_SEQ_DIR"), "/bank.psqb"));

/// Where the bank's ADPCM lands in SPU RAM.
const SPU_BASE: u32 = 0x1010;
/// Voices the music may use.
const VOICES: usize = 20;
/// Seconds of music to run before reporting.
const SECONDS: u32 = 40;

/// The song is patched in place by some sequences, so it lives in RAM.
static mut SONG_RAM: [u8; 16 * 1024] = [0; 16 * 1024];

const T2_COUNTER: *const u32 = (psx_hw::timers::block(2) + psx_hw::timers::COUNTER) as *const u32;
const T2_MODE: *mut u32 = (psx_hw::timers::block(2) + psx_hw::timers::MODE) as *mut u32;
const T2_SYSCLK_DIV8: u32 = 1 << 9;
/// Root counter 2 ticks per second (system clock 33.8688 MHz over 8).
const T2_HZ: u32 = 4_233_600;

fn now() -> u32 {
    // SAFETY: a read of root counter 2's current-value register.
    unsafe { read_volatile(T2_COUNTER) & 0xFFFF }
}

/// The SPU as [`Hw`]: each method is the register writes it names.
struct SpuHw {
    rate: [u32; 24],
}

impl SpuHw {
    fn pitch_for(&self, voice: u8, pitch_q12: u16) -> Pitch {
        let raw = (self.rate[voice as usize] * pitch_q12 as u32) / 44_100;
        Pitch::raw(raw.clamp(1, 0x3FFF) as u16)
    }
}

impl Hw for SpuHw {
    fn configure(&mut self, voice: u8, sample: &Sample, _looped: bool, pitch_q12: u16) {
        let v = Voice::new(voice);
        self.rate[voice as usize] = sample.sample_rate_hz();
        v.set_start_addr(sample.addr());
        // A one-shot parks on the silent block; a loop re-latches this from
        // the loop-start flag in its own data as it plays.
        v.set_loop_addr(SILENCE_BLOCK);
        v.set_adsr(Adsr::default_tone());
        v.set_pitch(self.pitch_for(voice, pitch_q12));
    }

    fn set_volume(&mut self, voice: u8, left: i16, right: i16) {
        Voice::new(voice).set_volume(Volume(left), Volume(right));
    }

    fn set_pitch(&mut self, voice: u8, pitch_q12: u16) {
        Voice::new(voice).set_pitch(self.pitch_for(voice, pitch_q12));
    }

    fn key_on(&mut self, mask: u32) {
        Voice::start(mask);
    }

    fn key_off(&mut self, mask: u32) {
        Voice::release(mask);
    }
}

fn print_dec(mut v: u32) {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    // SAFETY: ASCII digits.
    tty::print(unsafe { core::str::from_utf8_unchecked(&buf[i..]) });
}

fn line(label: &str, v: u32) {
    tty::print("hello-seq: ");
    tty::print(label);
    tty::print(" ");
    print_dec(v);
    tty::print("\n");
}

#[no_mangle]
fn main() {
    let Some(p) = psx_rt::Peripherals::take() else { return };
    let mut spu = Spu::new(p.spu_dma);
    let bank = match Bank::parse(BANK) {
        Ok(b) => b,
        Err(_) => {
            tty::print("hello-seq: bad bank\n");
            return;
        }
    };
    spu.upload_adpcm(SpuAddr::new(SPU_BASE), bank.adpcm());
    line("adpcm bytes", bank.adpcm().len() as u32);

    // SAFETY: single-threaded; this is the only reference to SONG_RAM.
    let ram: &'static mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(SONG_RAM).cast::<u8>(), SONG.len()) };
    ram.copy_from_slice(SONG);

    let mut seq: Sequencer<'_, 1, VOICES> = Sequencer::new((1u32 << VOICES) - 1);
    seq.set_bank(bank, SPU_BASE);
    if seq.start(0, ram).is_err() {
        tty::print("hello-seq: song refused\n");
        return;
    }
    let mut hw = SpuHw { rate: [0; 24] };

    // SAFETY: configure root counter 2 as system clock / 8, free running.
    unsafe { write_volatile(T2_MODE, T2_SYSCLK_DIV8) };
    let per_update = T2_HZ / UPDATE_HZ;
    let mut last = now();
    let mut acc: u32 = 0;
    let mut updates: u32 = 0;
    let total = SECONDS * UPDATE_HZ;
    let (mut spent, mut worst, mut worst_voices) = (0u32, 0u32, 0u32);
    while updates < total {
        let t = now();
        acc += t.wrapping_sub(last) & 0xFFFF;
        last = t;
        // catch up at most a dozen updates, as `pump` does
        let mut ran = 0;
        while acc >= per_update && ran < 12 {
            acc -= per_update;
            let a = now();
            seq.update(&mut hw);
            let dt = now().wrapping_sub(a) & 0xFFFF;
            spent += dt;
            worst = worst.max(dt);
            worst_voices = worst_voices.max(seq.voices_in_use());
            updates += 1;
            ran += 1;
        }
        if ran == 12 {
            acc = 0;
        }
    }
    line("updates", updates);
    line("avg cycles per update", spent * 8 / updates);
    line("worst cycles per update", worst * 8);
    line("peak voices", worst_voices);
    line("unsupported ops", seq.unsupported_ops());
    tty::print("hello-seq: done\n");
    loop {}
}
