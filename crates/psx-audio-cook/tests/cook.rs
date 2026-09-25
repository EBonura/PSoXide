//! Host tests for the shared SPU-ADPCM cooker.

use psx_audio_cook::{
    adpcm, adpcm_bytes, cook, cooked_len, legacy, metrics, psau, rate, resample, spu_play, wav,
    CookOptions, Effort, EncodeOptions, Looping, Wav,
};
use std::f64::consts::PI;

fn tone(rate: u32, secs: f64, freqs: &[f64]) -> Vec<f64> {
    (0..(rate as f64 * secs) as usize)
        .map(|i| {
            let t = i as f64 / rate as f64;
            freqs.iter().map(|f| (2.0 * PI * f * t).sin()).sum::<f64>() * 9000.0
                / freqs.len() as f64
        })
        .collect()
}

fn wav_of(rate: u32, samples: Vec<f64>) -> Wav {
    Wav {
        rate,
        samples,
        loop_start: None,
        loop_end: None,
        bits: 16,
    }
}

#[test]
fn decoder_clamps_history_and_maps_reserved_shifts_to_nine() {
    // Filter 1 (60/64 of the last sample), shift 0, every nibble +7: the sum
    // overshoots and must saturate at 0x7FFF, not wrap or keep growing.
    let mut block = [0x77u8; 16];
    block[0] = 0x10;
    block[1] = 0;
    let out = adpcm::decode(&block);
    assert_eq!(*out.last().unwrap(), 0x7FFF);
    // Shift 13 decodes as shift 9.
    let mut a = [0u8; 16];
    a[0] = 0x0D;
    a[2] = 0x01;
    let mut b = a;
    b[0] = 0x09;
    assert_eq!(adpcm::decode(&a), adpcm::decode(&b));
}

#[test]
fn trellis_is_never_worse_than_greedy_and_codes_a_tone_well() {
    let x = resample::to_i16(&tone(11_025, 1.0, &[220.0, 1330.0, 3100.0]));
    let greedy = adpcm::decode(&adpcm::encode(
        &x,
        None,
        &EncodeOptions {
            effort: Effort::Greedy,
            ..Default::default()
        },
    ));
    let trellis = adpcm::decode(&adpcm::encode(&x, None, &EncodeOptions::default()));
    let (g, t) = (metrics::snr_db(&x, &greedy), metrics::snr_db(&x, &trellis));
    assert!(g > 20.0, "greedy SNR {g}");
    assert!(t >= g - 0.05, "trellis {t} vs greedy {g}");
}

#[test]
fn encoder_matches_shipped_greedy_where_nothing_clips() {
    // Quiet input never reaches the clamp, so the exact-model greedy search
    // and psxed-audio's unclamped one pick the same blocks.
    let x = resample::to_i16(
        &tone(8_000, 0.5, &[300.0, 900.0])
            .iter()
            .map(|v| v * 0.2)
            .collect::<Vec<_>>(),
    );
    let ours = adpcm::encode(
        &x,
        None,
        &EncodeOptions {
            effort: Effort::Greedy,
            ..Default::default()
        },
    );
    let old = legacy::encode_psxed(&x);
    let strip = |v: &[u8]| {
        v.chunks(16)
            .flat_map(|b| {
                let mut b = b.to_vec();
                b[1] = 0;
                b
            })
            .collect::<Vec<u8>>()
    };
    assert_eq!(strip(&ours), strip(&old));
}

#[test]
fn whole_loops_fill_whole_blocks_and_wrap_seamlessly() {
    let w = wav_of(11_025, tone(11_025, 0.37, &[180.0, 700.0]));
    let mut o = CookOptions::one_shot(3_000);
    o.looping = Looping::Whole;
    let c = cook(&w, &o);
    assert_eq!(c.pcm.len() % 28, 0);
    assert_eq!(c.loop_block, Some(0));
    assert_eq!(c.adpcm[0] >> 4, 0, "re-entry block uses filter 0");
    assert_eq!(c.adpcm[1], adpcm::FLAG_LOOP_START);
    assert_eq!(
        c.adpcm[c.adpcm.len() - 15],
        adpcm::FLAG_END | adpcm::FLAG_REPEAT
    );
    // Playing the loop twice decodes to the first pass twice.
    let once = adpcm::decode(&c.adpcm);
    let twice = adpcm::decode(&[c.adpcm.clone(), c.adpcm.clone()].concat());
    assert_eq!(&twice[..once.len()], &once[..]);
    assert_eq!(&twice[once.len()..], &once[..]);
    assert_eq!(cooked_len(&w, 3_000, Looping::Whole), c.pcm.len());
}

#[test]
fn source_loops_start_on_a_block_boundary() {
    let mut w = wav_of(22_050, tone(22_050, 0.5, &[400.0]));
    w.loop_start = Some(3_001);
    let mut o = CookOptions::one_shot(8_000);
    o.looping = Looping::Source;
    let c = cook(&w, &o);
    let lb = c.loop_block.expect("loop");
    assert!(lb > 0);
    assert_eq!(c.adpcm[lb * 16 + 1], adpcm::FLAG_LOOP_START);
    assert_eq!(c.adpcm[lb * 16] >> 4, 0);
    assert_eq!(c.pcm.len() % 28, 0);
}

#[test]
fn one_shots_end_on_their_last_block() {
    let w = wav_of(11_025, tone(11_025, 0.2, &[500.0]));
    let c = cook(&w, &CookOptions::one_shot(6_000));
    assert_eq!(c.adpcm.len(), adpcm_bytes(c.pcm.len()));
    assert_eq!(c.adpcm[c.adpcm.len() - 15], adpcm::FLAG_END);
    assert!(c.adpcm.chunks(16).rev().skip(1).all(|b| b[1] == 0));
}

#[test]
fn resampler_keeps_phase_and_rejects_aliases() {
    let x = tone(11_025, 1.0, &[1000.0]);
    let y = resample::Sinc::new().resample(&x, 11_025, 6_000);
    let ideal: Vec<f64> = (0..y.len())
        .map(|j| (2.0 * PI * 1000.0 * j as f64 / 6000.0).sin() * 9000.0)
        .collect();
    assert!(metrics::si_snr_db(&ideal[500..5500], &y[500..5500]) > 60.0);
    // 4 kHz cannot exist at 6 kHz; it must be filtered, not folded to 2 kHz.
    let hi = tone(11_025, 1.0, &[4000.0]);
    let y = resample::Sinc::new().resample(&hi, 11_025, 6_000);
    let rms = |v: &[f64]| (v.iter().map(|a| a * a).sum::<f64>() / v.len() as f64).sqrt();
    assert!(20.0 * (rms(&y[500..5500]) / rms(&hi)).log10() < -60.0);
}

#[test]
fn gauss_compensation_flattens_playback_through_the_pass_band() {
    let taps = resample::gauss_compensation_taps();
    for i in 0..=40 {
        let u = i as f64 / 100.0;
        let c: f64 = taps
            .iter()
            .enumerate()
            .map(|(k, t)| t * (2.0 * PI * u * (k as f64 - (taps.len() / 2) as f64)).cos())
            .sum();
        let total = 20.0 * (c * spu_play::gauss_response(u)).abs().log10();
        assert!(total.abs() < 0.5, "u={u}: {total} dB");
    }
}

#[test]
fn psau_header_matches_the_runtime_layout() {
    let blob = psau(6_000, 100, &[0u8; 64]);
    assert_eq!(&blob[0..4], b"PSAU");
    assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), 1);
    assert_eq!(u16::from_le_bytes([blob[6], blob[7]]), 3); // MONO | ONE_SHOT
    assert_eq!(u32::from_le_bytes(blob[8..12].try_into().unwrap()), 20 + 64);
    assert_eq!(blob[12], 1);
    assert_eq!(blob[13], 1);
    assert_eq!(u32::from_le_bytes(blob[16..20].try_into().unwrap()), 6_000);
    assert_eq!(u32::from_le_bytes(blob[20..24].try_into().unwrap()), 100);
    assert_eq!(u32::from_le_bytes(blob[24..28].try_into().unwrap()), 4);
    assert_eq!(
        u32::from_le_bytes(blob[28..32].try_into().unwrap()),
        u32::MAX
    );
    assert_eq!(blob.len(), 32 + 64);
}

#[test]
fn band_loss_is_zero_for_content_below_the_cutoff_and_grows_with_bandwidth() {
    let low = tone(11_025, 1.0, &[150.0, 400.0]);
    let bright = tone(11_025, 1.0, &[150.0, 400.0, 3500.0, 4800.0]);
    let rates = [11_025, 6_000, 3_000];
    let l = rate::band_loss(&low, 11_025, &rates);
    let b = rate::band_loss(&bright, 11_025, &rates);
    assert_eq!(l[0], 0.0);
    assert!(l[1] < 1.0 && l[2] < 1.5, "{l:?}");
    assert!(b[1] > l[1] + 3.0 && b[2] >= b[1], "{b:?}");
}

#[test]
fn allocator_fits_the_budget_and_spends_it_where_it_matters() {
    let ladder = [11_025u32, 8_000, 5_000, 2_400];
    let bytes = |secs: f64| {
        ladder
            .iter()
            .map(|&r| adpcm_bytes((secs * r as f64) as usize))
            .collect::<Vec<_>>()
    };
    let rumble = rate::Candidate {
        bytes: bytes(4.0),
        loss: vec![0.0, 0.1, 0.3, 1.0],
        weight: 4.0,
        max_step: 3,
    };
    let voice = rate::Candidate {
        bytes: bytes(4.0),
        loss: vec![0.0, 3.0, 8.0, 14.0],
        weight: 4.0,
        max_step: 3,
    };
    let budget = bytes(4.0)[0] + bytes(4.0)[2] + 100;
    let steps = rate::allocate(&[rumble.clone(), voice.clone()], 0, budget).expect("fits");
    let total: usize = steps
        .iter()
        .zip([&rumble, &voice])
        .map(|(&s, c)| c.bytes[s])
        .sum();
    assert!(total <= budget);
    assert!(steps[0] > steps[1], "the rumble goes down first: {steps:?}");
    assert!(rate::allocate(&[rumble, voice], 0, 100).is_none());
}

#[test]
fn wav_reader_expands_8_bit_and_reads_smpl_loops() {
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&[1, 0, 1, 0]);
    f.extend_from_slice(&11_025u32.to_le_bytes());
    f.extend_from_slice(&11_025u32.to_le_bytes());
    f.extend_from_slice(&[1, 0, 8, 0]);
    f.extend_from_slice(b"data");
    f.extend_from_slice(&4u32.to_le_bytes());
    f.extend_from_slice(&[128, 255, 0, 128]);
    f.extend_from_slice(b"smpl");
    f.extend_from_slice(&60u32.to_le_bytes());
    let mut smpl = [0u8; 60];
    smpl[28..32].copy_from_slice(&1u32.to_le_bytes());
    smpl[44..48].copy_from_slice(&1u32.to_le_bytes());
    smpl[48..52].copy_from_slice(&2u32.to_le_bytes());
    f.extend_from_slice(&smpl);
    let w = wav::read(&f).unwrap();
    assert_eq!(w.rate, 11_025);
    assert_eq!(w.samples, vec![0.0, 127.0 * 256.0, -128.0 * 256.0, 0.0]);
    assert_eq!((w.loop_start, w.loop_end), (Some(1), Some(3)));
}

#[test]
fn cli_encodes_a_wav_to_psau_and_raw() {
    let dir = std::env::temp_dir().join(format!("psx-audio-cook-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let samples = resample::to_i16(&tone(11_025, 0.3, &[440.0]));
    let input = dir.join("in.wav");
    std::fs::write(&input, wav::write_mono16(11_025, &samples)).unwrap();
    let bin = env!("CARGO_BIN_EXE_psx-audio-cook");
    for (format, header) in [("psau", 32usize), ("raw", 0)] {
        let out = dir.join(format!("out.{format}"));
        let status = std::process::Command::new(bin)
            .args([
                "encode",
                input.to_str().unwrap(),
                out.to_str().unwrap(),
                "--rate",
                "6000",
                "--format",
                format,
                "--loop",
                "whole",
            ])
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        let bytes = std::fs::read(&out).unwrap();
        let adpcm = &bytes[header..];
        assert_eq!(adpcm.len() % 16, 0);
        assert_eq!(adpcm[1], adpcm::FLAG_LOOP_START);
        assert_eq!(
            adpcm[adpcm.len() - 15],
            adpcm::FLAG_END | adpcm::FLAG_REPEAT
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
