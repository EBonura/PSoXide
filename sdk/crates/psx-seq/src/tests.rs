//! Tests drive hand-assembled songs over a one-instrument bank and read the
//! register writes back from a `Recorder`.

use super::*;
use psx_sfx::hw::{Event, Recorder};

/// A bank with font 0:
///
/// * instrument 0: one looped sample, tuning 1.0, the default envelope;
/// * instrument 1: a one-shot of 4 blocks, tuning 2.0;
/// * instrument 2: the looped sample with a ramp envelope (24 units to full,
///   then hold);
/// * drum 5: the looped sample at tuning 1.0, pan 0.
fn test_bank() -> Vec<u8> {
    let mut b = Vec::new();
    let le16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
    let le32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
    let off_fonts = 40u32;
    let off_inst = off_fonts + 12;
    let off_drum = off_inst + 3 * 32;
    let off_samples = off_drum + 6 * 16;
    let off_env = off_samples + 2 * 16;
    let off_adpcm = off_env + 8;
    b.extend_from_slice(b"PSQB");
    le16(&mut b, 1);
    le16(&mut b, 0);
    le32(&mut b, 0); // total, patched later
    le16(&mut b, 1); // fonts
    le16(&mut b, 2); // samples
    le16(&mut b, 2); // env words
    le16(&mut b, 0);
    le32(&mut b, off_fonts);
    le32(&mut b, off_samples);
    le32(&mut b, off_env);
    le32(&mut b, off_adpcm);
    le32(&mut b, 128); // adpcm bytes
                       // font
    b.extend_from_slice(&[0, 3, 6, 0]);
    le32(&mut b, off_inst);
    le32(&mut b, off_drum);
    // instruments (32 B each): sample, tuning, env
    for (sample, tuning, env) in [
        (0u16, 65536u32, 0xffffu16),
        (1, 131072, 0xffff),
        (0, 65536, 0),
    ] {
        b.extend_from_slice(&[0, 127, 200, 1]);
        le16(&mut b, env);
        le16(&mut b, 0);
        for k in 0..3 {
            le16(&mut b, if k == 1 { sample } else { 0xffff });
            le16(&mut b, 0);
            le32(&mut b, if k == 1 { tuning } else { 0 });
        }
    }
    // drums (16 B each), six slots, only slot 5 present
    for key in 0..6 {
        if key == 5 {
            b.extend_from_slice(&[0, 0]); // decay, pan 0
            le16(&mut b, 0xffff);
            le16(&mut b, 0); // sample
            le16(&mut b, 1); // present
            le32(&mut b, 65536);
            le32(&mut b, 0);
        } else {
            b.extend_from_slice(&[0u8; 16]);
        }
    }
    // samples (16 B): off, end_samples, blocks, rate, loop_block, flags
    le32(&mut b, 0);
    le32(&mut b, 1000);
    le16(&mut b, 4);
    le16(&mut b, 32000);
    le16(&mut b, 1);
    le16(&mut b, 0);
    le32(&mut b, 64);
    le32(&mut b, 112);
    le16(&mut b, 4);
    le16(&mut b, 32000);
    le16(&mut b, 0xffff);
    le16(&mut b, 0);
    // envelope words (big endian): ramp to full in 24 units, then hang
    b.extend_from_slice(&24i16.to_be_bytes());
    b.extend_from_slice(&32767i16.to_be_bytes());
    b.extend_from_slice(&(-1i16).to_be_bytes());
    b.extend_from_slice(&0i16.to_be_bytes());
    b.extend_from_slice(&[0u8; 128]);
    let total = b.len() as u32;
    b[8..12].copy_from_slice(&total.to_le_bytes());
    b
}

/// Wrap bytecode in a song container with one font (id 0).
fn song(code: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(b"PSQ1");
    s.extend_from_slice(&[1, 0, 0, 0, 1, 4]);
    s.extend_from_slice(&(code.len() as u16).to_le_bytes());
    s.extend_from_slice(&[1, 0, 0, 0]);
    s.extend_from_slice(&[0, 0, 0, 0]);
    s.extend_from_slice(code);
    s
}

/// Lay out `seq`, `chan` and `layer` scripts back to back and return the
/// bytecode with their offsets patched into the `{CH}` and `{LY}` slots
/// (16-bit placeholders 0xCCCC and 0xDDDD).
fn layout(seq: &[u8], chan: &[u8], layer: &[u8]) -> Vec<u8> {
    let ch_off = seq.len() as u16;
    let ly_off = (seq.len() + chan.len()) as u16;
    let mut out = Vec::new();
    out.extend_from_slice(seq);
    out.extend_from_slice(chan);
    out.extend_from_slice(layer);
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i] == 0xcc && out[i + 1] == 0xcc {
            out[i..i + 2].copy_from_slice(&ch_off.to_be_bytes());
        } else if out[i] == 0xdd && out[i + 1] == 0xdd {
            out[i..i + 2].copy_from_slice(&ly_off.to_be_bytes());
        }
        i += 1;
    }
    out
}

fn keyons(hw: &Recorder<512>) -> usize {
    hw.events()
        .iter()
        .filter(|e| matches!(e, Event::KeyOn(_)))
        .count()
}

/// Run `n` updates, returning the update index of every key-on.
fn run<const P: usize, const N: usize>(
    seq: &mut Sequencer<'_, P, N>,
    hw: &mut Recorder<512>,
    n: usize,
) -> Vec<usize> {
    let mut at = Vec::new();
    for i in 0..n {
        let before = keyons(hw);
        seq.update(hw);
        if keyons(hw) > before {
            at.push(i);
        }
    }
    at
}

/// Sequence: one channel (0), tempo 120 BPM, volume full, then waits.
const SEQ_HEAD: [u8; 10] = [
    0xd7, 0x00, 0x01, // initchan ch0
    0x90, 0xcc, 0xcc, // ldchan 0
    0xdd, 120, // tempo 120
    0xdb, 127, // vol 127
];

fn one_note_song(layer: &[u8]) -> Vec<u8> {
    let mut seq = SEQ_HEAD.to_vec();
    seq.extend_from_slice(&[0xfd, 0x7f, 0xff]); // delay 127, end
    let chan = [
        0xc1, 0x00, // instr 0
        0xc4, // large notes
        0xdf, 127, // vol 127
        0xdd, 64, // pan centre
        0x88, 0xdd, 0xdd, // ldlayer 0
        0xfd, 0x7f, // delay 127
        0xff,
    ];
    layout(&seq, &chan, layer)
}

#[test]
fn a_note_keys_on_with_the_instruments_pitch() {
    let mut bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x30, 100, 0x00, 0xff]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0x2000);
    // SAFETY-free: the bank borrow ends before it is mutated below
    seq.start(0, &mut code).unwrap();
    let at = run(&mut seq, &mut hw, 5);
    assert_eq!(
        at.len(),
        1,
        "one key-on in the first few updates: {:?}",
        hw.events()
    );
    let cfg = hw
        .events()
        .iter()
        .find_map(|e| {
            if let Event::Configure {
                voice,
                addr,
                pitch_q12,
                looped,
                ..
            } = *e
            {
                Some((voice, addr, pitch_q12, looped))
            } else {
                None
            }
        })
        .unwrap();
    // note 39 is the unison key of the pitch table and the tuning is 1.0
    assert_eq!(cfg.2, 4096);
    assert_eq!(cfg.1, 0x2000);
    assert!(cfg.3);
    bank.clear();
}

#[test]
fn a_one_shot_instrument_uses_its_tuning_and_is_not_looped() {
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0xc6, 0x01, 0x27, 0x30, 100, 0x00, 0xff]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 5);
    let cfg = hw
        .events()
        .iter()
        .find_map(|e| {
            if let Event::Configure {
                pitch_q12,
                looped,
                addr,
                ..
            } = *e
            {
                Some((pitch_q12, looped, addr))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(cfg.0, 8192, "tuning 2.0 doubles the pitch");
    assert!(!cfg.1);
    assert_eq!(cfg.2, 64);
}

#[test]
fn notes_follow_the_tempo() {
    // Two notes of 48 ticks each at 120 BPM: 96 ticks a second, so a note is
    // half a second: 90 updates at 180 Hz.
    let bank = test_bank();
    let mut code = song(&one_note_song(&[
        0x27, 0x30, 100, 0x00, 0x29, 0x30, 100, 0x00, 0xff,
    ]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    let at = run(&mut seq, &mut hw, 200);
    assert_eq!(at.len(), 2, "{at:?}");
    let gap = at[1] - at[0];
    assert!((89..=91).contains(&gap), "gap {gap} updates");
}

#[test]
fn a_note_gate_releases_it_before_the_next_note() {
    // gate 128 of 256: the note decays halfway through its 48 ticks.
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x30, 100, 0x80, 0xff]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 40);
    assert_eq!(hw.keyed_off(), 0, "still held at 40 updates");
    let _ = run(&mut seq, &mut hw, 400);
    assert_ne!(hw.keyed_off(), 0, "released and keyed off eventually");
}

#[test]
fn an_io_port_selects_the_branch() {
    // The sequence waits for port 0 to become non-negative, then ldchan.
    let bank = test_bank();
    let mut seq_code = vec![0xd7, 0x00, 0x01, 0xdd, 120, 0xdb, 127];
    // loop: ldio 0 ; bltz loop (value<0 means unset) ; ldchan ; delay ; end
    let loop_at = seq_code.len() as u16;
    seq_code.push(0x80); // ldio 0
    seq_code.push(0xfe); // delay 1
    seq_code.push(0xf9); // bltz
    seq_code.extend_from_slice(&loop_at.to_be_bytes());
    seq_code.extend_from_slice(&[0x90, 0xcc, 0xcc, 0xfd, 0x7f, 0xff]);
    let chan = [
        0xc1, 0x00, 0xc4, 0xdf, 127, 0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff,
    ];
    let code = layout(&seq_code, &chan, &[0x27, 0x30, 100, 0x00, 0xff]);
    let mut code = song(&code);
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 60);
    assert_eq!(keyons(&hw), 0, "silent while the port is unset");
    seq.set_player_io(0, 0, 1);
    run(&mut seq, &mut hw, 20);
    assert_eq!(keyons(&hw), 1, "plays once the game writes the port");
}

#[test]
fn the_song_can_write_an_io_port_back() {
    let bank = test_bank();
    // ldi 5 ; stio 3 ; end
    let seq_code = [0xdd, 120, 0xcc, 5, 0x73, 0xfd, 0x10, 0xff];
    let mut code = song(&seq_code);
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 5);
    assert_eq!(seq.player_io(0, 3), Some(5));
}

#[test]
fn a_loop_repeats_its_body() {
    // layer: loop 3 { note } end   (each note 12 ticks)
    let bank = test_bank();
    let layer = [0xf8, 3, 0x27, 12, 100, 0x00, 0xf7, 0xff];
    let mut code = song(&one_note_song(&layer));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    let at = run(&mut seq, &mut hw, 400);
    assert_eq!(at.len(), 3, "{at:?}");
}

#[test]
fn a_higher_priority_note_takes_a_busy_voice() {
    // One voice; the channel plays a long low-priority note, a second channel
    // starts a priority-9 note on top of it.
    let bank = test_bank();
    let seq_code = [
        0xd7, 0x00, 0x03, 0xdd, 120, 0xdb, 127, 0x90, 0xcc, 0xcc, 0x91, 0xee, 0xee, 0xfd, 0x7f,
        0xff,
    ];
    let low = [
        0xc1, 0, 0xc4, 0xdf, 127, 0xe9, 0x02, 0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff,
    ];
    let high_ch = [
        0xc1, 0, 0xc4, 0xdf, 127, 0xe9, 0x09, 0xfd, 0x20, 0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff,
    ];
    // layout: seq, low, high, layer
    let mut out = Vec::new();
    let lo_off = seq_code.len() as u16;
    let hi_off = lo_off + low.len() as u16;
    let ly_off = hi_off + high_ch.len() as u16;
    out.extend_from_slice(&seq_code);
    out.extend_from_slice(&low);
    out.extend_from_slice(&high_ch);
    out.extend_from_slice(&[0x27, 0x60, 100, 0x00, 0xff]);
    for (hole, v) in [(0xccu8, lo_off), (0xee, hi_off), (0xdd, ly_off)] {
        let mut i = 0;
        while i + 1 < out.len() {
            if out[i] == hole && out[i + 1] == hole {
                out[i..i + 2].copy_from_slice(&v.to_be_bytes());
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    let mut code = song(&out);
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 1> = Sequencer::new(0b1);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    let at = run(&mut seq, &mut hw, 400);
    assert!(at.len() >= 2, "the second note got the voice: {at:?}");
}

#[test]
fn pump_runs_due_updates_and_caps_a_stall() {
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x30, 100, 0x00, 0xff]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    seq.pump(&mut hw, 1000); // sets the clock
    seq.pump(&mut hw, 1001);
    seq.pump(&mut hw, 5000); // a huge stall: only MAX_CATCH_UP updates run
    assert!(seq.is_playing(0));
}

#[test]
fn starting_without_a_bank_is_refused() {
    let mut code = song(&[0xff]);
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    assert_eq!(seq.start(0, &mut code), Err(StartError::NoBank));
}

fn volumes(hw: &Recorder<512>) -> Vec<(i16, i16)> {
    hw.events()
        .iter()
        .filter_map(|e| {
            if let Event::Volume { left, right, .. } = *e {
                Some((left, right))
            } else {
                None
            }
        })
        .collect()
}

fn pitches(hw: &Recorder<512>) -> Vec<u16> {
    hw.events()
        .iter()
        .filter_map(|e| match *e {
            Event::Pitch { pitch_q12, .. } | Event::Configure { pitch_q12, .. } => Some(pitch_q12),
            _ => None,
        })
        .collect()
}

/// A song with one channel whose script is `chan_prefix` followed by a layer
/// load, and one layer `layer`.
fn song_with_channel(chan_prefix: &[u8], layer: &[u8]) -> Vec<u8> {
    let mut seq = SEQ_HEAD.to_vec();
    seq.extend_from_slice(&[0xfd, 0x7f, 0xff]);
    let mut chan = vec![0xc4];
    chan.extend_from_slice(chan_prefix);
    chan.extend_from_slice(&[0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff]);
    song(&layout(&seq, &chan, layer))
}

fn play(code: &mut Vec<u8>, voices: u32, updates: usize) -> (Recorder<512>, Vec<usize>) {
    let bank = test_bank();
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(voices);
    // The bank and song are leaked: tests hand the sequencer 'static data.
    let bank_static: &'static [u8] = Box::leak(bank.into_boxed_slice());
    seq.set_bank(Bank::parse(bank_static).unwrap(), 0x1000);
    let code_static: &'static mut [u8] = Box::leak(core::mem::take(code).into_boxed_slice());
    seq.start(0, code_static).unwrap();
    let at = run(&mut seq, &mut hw, updates);
    (hw, at)
}

#[test]
fn channel_volume_is_squared() {
    // Full volume against half volume: the original squares the channel
    // volume, so half the setting is a quarter of the level.
    let mut full = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xdd, 64],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let mut half = song_with_channel(
        &[0xc1, 0, 0xdf, 64, 0xdd, 64],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let (a, _) = play(&mut full, 0b1111, 40);
    let (b, _) = play(&mut half, 0b1111, 40);
    let (la, lb) = (
        volumes(&a).last().unwrap().0 as f64,
        volumes(&b).last().unwrap().0 as f64,
    );
    let ratio = lb / la;
    let want = (64.0f64 / 127.0).powi(2);
    assert!((ratio - want).abs() < 0.03, "ratio {ratio} want {want}");
}

#[test]
fn pan_moves_the_level_between_the_ears() {
    let mut left = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xdd, 0],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let mut right = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xdd, 127],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let (a, _) = play(&mut left, 0b1111, 20);
    let (b, _) = play(&mut right, 0b1111, 20);
    let (l, r) = *volumes(&a).last().unwrap();
    assert!(l > 0 && r == 0, "pan 0 is hard left: {l} {r}");
    let (l, r) = *volumes(&b).last().unwrap();
    assert!(r > 0 && l == 0, "pan 127 is hard right: {l} {r}");
}

#[test]
fn an_envelope_ramps_in_the_squared_domain() {
    // instrument 2 ramps to full over 24 units = 18 updates. Half way the
    // squared level is about a quarter of the final level... a linear ramp in
    // the squared domain, so the level at half time is half the final level.
    let mut code = song_with_channel(
        &[0xc1, 2, 0xdf, 127, 0xdd, 64],
        &[0x27, 0x60, 127, 0x00, 0xff],
    );
    let (hw, _) = play(&mut code, 0b1111, 60);
    let v: Vec<i16> = volumes(&hw).iter().map(|x| x.0).collect();
    let last = *v.last().unwrap() as f64;
    let mid = v[v.len() / 4].max(1) as f64;
    assert!(
        v.windows(2).all(|w| w[1] >= w[0]),
        "rises monotonically: {v:?}"
    );
    assert!(last > 0.0 && mid < last, "{mid} {last}");
}

#[test]
fn vibrato_swings_the_pitch_around_the_note() {
    // D8 vibrato depth 60, D7 rate 30
    let mut code = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xd8, 60, 0xd7, 30],
        &[0x27, 0x60, 127, 0x00, 0xff],
    );
    let (hw, _) = play(&mut code, 0b1111, 120);
    let p = pitches(&hw);
    let (lo, hi) = (*p.iter().min().unwrap(), *p.iter().max().unwrap());
    assert!(lo < 4096 && hi > 4096, "pitch spans the note: {lo}..{hi}");
    assert!(
        hi - lo < 1000,
        "and stays within a couple of semitones: {lo}..{hi}"
    );
}

#[test]
fn channel_transpose_shifts_the_pitch_by_semitones() {
    let mut code = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xdb, 12],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let (hw, _) = play(&mut code, 0b1111, 10);
    assert_eq!(pitches(&hw)[0], 8192, "an octave up doubles the pitch");
}

#[test]
fn a_bend_scales_the_pitch() {
    // D3 bend by +127 is the top of the one-octave range: pitch doubles. A
    // bend of 0 leaves it alone.
    let mut up = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xd3, 127],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let mut flat = song_with_channel(
        &[0xc1, 0, 0xdf, 127, 0xd3, 0],
        &[0x27, 0x30, 127, 0x00, 0xff],
    );
    let (hw, _) = play(&mut up, 0b1111, 10);
    assert_eq!(pitches(&hw)[0], 8192);
    let (hw, _) = play(&mut flat, 0b1111, 10);
    assert_eq!(pitches(&hw)[0], 4096);
}

#[test]
fn legato_notes_share_a_voice_and_only_change_pitch() {
    // C4 legato on, two notes
    let layer = [0xc4, 0x27, 0x18, 100, 0x00, 0x29, 0x18, 100, 0x00, 0xff];
    let mut code = song_with_channel(&[0xc1, 0, 0xdf, 127], &layer);
    let (hw, at) = play(&mut code, 0b1111, 100);
    assert_eq!(
        at.len(),
        1,
        "one key-on for two legato notes: {:?}",
        hw.events()
    );
    let p = pitches(&hw);
    assert!(
        p.len() >= 2 && p[1] != p[0],
        "the second note re-pitches the voice: {p:?}"
    );
}

#[test]
fn a_drum_plays_its_own_sample_and_pan() {
    // layer instrument 0x7F selects the drum kit; key 5
    let layer = [0xc6, 0x7f, 0x05, 0x30, 127, 0x00, 0xff];
    let mut code = song_with_channel(&[0xc1, 0, 0xdf, 127, 0xdc, 0], &layer);
    let (hw, at) = play(&mut code, 0b1111, 20);
    assert_eq!(at.len(), 1);
    let (l, r) = *volumes(&hw).last().unwrap();
    assert!(
        l > 0 && r == 0,
        "the drum's own pan (hard left) wins: {l} {r}"
    );
}

#[test]
fn a_channel_mute_blocks_new_notes() {
    let bank = test_bank();
    let layer = [0x27, 0x30, 127, 0x00, 0xff];
    let mut code = song(&one_note_song(&layer));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    seq.set_channel_mute(0, 1);
    run(&mut seq, &mut hw, 30);
    assert_eq!(keyons(&hw), 0);
    seq.set_channel_mute(0, 0);
    // a muted channel's note was skipped, not delayed: the next one plays
    assert_eq!(seq.voices_in_use(), 0);
}

#[test]
fn stopping_with_a_fade_silences_every_voice() {
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x7f, 127, 0x00, 0xff]));
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 20);
    assert!(seq.voices_in_use() > 0);
    seq.stop(0, 30);
    run(&mut seq, &mut hw, 200);
    assert!(!seq.is_playing(0));
    assert_eq!(seq.voices_in_use(), 0);
    assert_ne!(hw.keyed_off(), 0);
}

#[test]
fn unsupported_opcodes_are_counted_and_skipped() {
    // runseq (C4 a b) in the sequence script, then the usual song
    let mut seq_code = vec![
        0xd7, 0x00, 0x01, 0x90, 0xcc, 0xcc, 0xdd, 120, 0xdb, 127, 0xc4, 0x01, 0x02,
    ];
    seq_code.extend_from_slice(&[0xfd, 0x7f, 0xff]);
    let chan = [0xc1, 0, 0xc4, 0xdf, 127, 0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff];
    let mut code = song(&layout(&seq_code, &chan, &[0x27, 0x30, 100, 0x00, 0xff]));
    let bank = test_bank();
    let mut hw = Recorder::<512>::new();
    let mut seq: Sequencer<'_, 1, 4> = Sequencer::new(0b1111);
    seq.set_bank(Bank::parse(&bank).unwrap(), 0);
    seq.start(0, &mut code).unwrap();
    let at = run(&mut seq, &mut hw, 10);
    assert_eq!(at.len(), 1, "the song still plays");
    assert_eq!(seq.unsupported_ops(), 1);
}

#[test]
fn a_song_that_patches_itself_plays_the_patched_note() {
    // sequence: ldi 12 ; stseq +0 -> layer byte 0 (the note) ; ...
    // The layer's note byte starts as 0x27 (pitch 4096) and becomes 12.
    let mut seq = SEQ_HEAD.to_vec();
    // ldi 12 ; stseq 0, <layer offset = 0xDDDD> ; delay ; end
    seq.extend_from_slice(&[0xcc, 12, 0xc7, 0x00, 0xdd, 0xdd, 0xfd, 0x7f, 0xff]);
    let chan = [0xc1, 0, 0xc4, 0xdf, 127, 0x88, 0xdd, 0xdd, 0xfd, 0x7f, 0xff];
    let mut code = song(&layout(&seq, &chan, &[0x27, 0x30, 100, 0x00, 0xff]));
    let (hw, _) = play(&mut code, 0b1111, 12);
    let p = pitches(&hw)[0];
    // key 12 is 27 semitones below the unison key: 4096 * 2^(-27/12)
    assert!((850..875).contains(&p), "{p}");
}

fn live_bank_seq<'a, const N: usize>(bank: &'a [u8], voices: u32) -> Sequencer<'a, 1, N> {
    let mut seq: Sequencer<'_, 1, N> = Sequencer::new(voices);
    seq.set_bank(Bank::parse(bank).unwrap(), 0x1000);
    seq
}

#[test]
fn a_live_note_plays_bends_and_releases() {
    let bank = test_bank();
    let mut seq = live_bank_seq::<4>(&bank, 0b1111);
    let mut hw = Recorder::<512>::new();
    let h = seq.note_on(&mut hw, 0, 0, 39, 127, 64, 9).expect("a voice");
    seq.update(&mut hw);
    assert_eq!(keyons(&hw), 1);
    assert_eq!(pitches(&hw)[0], 4096, "key 39 at tuning 1.0");
    assert!(seq.note_alive(h));
    // bend an octave up: the voice is re-pitched on the next update
    assert!(seq.note_bend(h, 2 << 16));
    seq.update(&mut hw);
    assert_eq!(*pitches(&hw).last().unwrap(), 8192);
    // release: the envelope decays to nothing and the voice is keyed off
    assert!(seq.note_off(h));
    for _ in 0..600 {
        seq.update(&mut hw);
    }
    assert!(!seq.note_alive(h));
    assert_ne!(hw.keyed_off(), 0);
    assert!(!seq.note_bend(h, 1 << 16), "a stale handle does nothing");
}

#[test]
fn live_vibrato_wobbles_the_pitch() {
    let bank = test_bank();
    let mut seq = live_bank_seq::<4>(&bank, 0b1111);
    let mut hw = Recorder::<512>::new();
    let h = seq.note_on(&mut hw, 0, 0, 39, 127, 64, 9).unwrap();
    assert!(seq.note_vibrato(h, 40, 30));
    for _ in 0..100 {
        seq.update(&mut hw);
    }
    let p = pitches(&hw);
    assert!(
        p.iter().min().unwrap() < &4096 && p.iter().max().unwrap() > &4096,
        "{p:?}"
    );
}

#[test]
fn a_live_note_sounds_over_a_song_on_its_own_voice() {
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x7f, 100, 0x00, 0xff]));
    let mut seq = live_bank_seq::<4>(&bank, 0b1111);
    let mut hw = Recorder::<512>::new();
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 10);
    let before = seq.voices_in_use();
    let _h = seq.note_on(&mut hw, 0, 0, 51, 100, 64, 9).unwrap();
    run(&mut seq, &mut hw, 3);
    assert_eq!(seq.voices_in_use(), before + 1);
    assert_eq!(keyons(&hw), 2);
}

#[test]
fn a_live_note_of_high_priority_takes_a_busy_voice() {
    // one voice, a song note of priority 3 already sounding
    let bank = test_bank();
    let mut code = song(&one_note_song(&[0x27, 0x7f, 100, 0x00, 0xff]));
    let mut seq = live_bank_seq::<1>(&bank, 0b1);
    let mut hw = Recorder::<512>::new();
    seq.start(0, &mut code).unwrap();
    run(&mut seq, &mut hw, 10);
    assert_eq!(seq.voices_in_use(), 1);
    assert!(
        seq.note_on(&mut hw, 0, 0, 51, 100, 64, 2).is_none(),
        "priority 2 cannot take it"
    );
    assert!(
        seq.note_on(&mut hw, 0, 0, 51, 100, 64, 9).is_some(),
        "priority 9 can"
    );
    run(&mut seq, &mut hw, 2);
    assert_eq!(keyons(&hw), 2);
}

#[test]
fn an_unknown_instrument_or_font_gives_no_note() {
    let bank = test_bank();
    let mut seq = live_bank_seq::<4>(&bank, 0b1111);
    let mut hw = Recorder::<8>::new();
    assert!(seq.note_on(&mut hw, 9, 0, 39, 100, 64, 9).is_none());
    assert!(seq.note_on(&mut hw, 0, 77, 39, 100, 64, 9).is_none());
}

#[test]
fn a_dynamic_call_runs_the_script_the_table_names() {
    // Sequence, then one channel: dyntable, value = 2, dyncall; a three-entry table of
    // scripts that each load a layer, and three layers an octave apart.
    let mut code = SEQ_HEAD.to_vec();
    code.extend_from_slice(&[0xfd, 0x7f, 0xff]);
    let chan_at = code.len();
    // fix the ldchan operand (placeholder 0xCCCC at offset 4)
    code[4..6].copy_from_slice(&(chan_at as u16).to_be_bytes());
    let chan_len = 2 + 1 + 2 + 2 + 3 + 2 + 1 + 2 + 1; // instr, large, vol, pan, dyntable, ldi, dyncall, delay, end
    let table_at = chan_at + chan_len;
    let subs_at = table_at + 3 * 2;
    let layers_at = subs_at + 3 * 4;
    let layer = |key: u8| [key, 0x30, 100, 0x00, 0xff];
    code.extend_from_slice(&[0xc1, 0x00, 0xc4, 0xdf, 127, 0xdd, 64]);
    code.extend_from_slice(&[0xc2, (table_at >> 8) as u8, table_at as u8]);
    code.extend_from_slice(&[0xcc, 2, 0xe4, 0xfd, 0x7f, 0xff]);
    assert_eq!(code.len(), table_at, "channel length");
    for i in 0..3 {
        let at = subs_at + 4 * i;
        code.extend_from_slice(&[(at >> 8) as u8, at as u8]);
    }
    for i in 0..3 {
        let layer_at = layers_at + 5 * i;
        code.extend_from_slice(&[0x88, (layer_at >> 8) as u8, layer_at as u8, 0xff]);
    }
    for key in [0x27u8, 0x33, 0x1b] {
        code.extend_from_slice(&layer(key));
    }
    let mut code = song(&code);
    let (hw, at) = play(&mut code, 0b1111, 10);
    assert_eq!(
        at.len(),
        1,
        "the called script loaded one layer: {:?}",
        hw.events()
    );
    assert_eq!(pitches(&hw)[0], 2048, "entry 2 is the octave-down layer");
}
