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
    // score reads the rate from a PSAU header and writes the playback.
    let played = dir.join("played.wav");
    let out = std::process::Command::new(bin)
        .args([
            "score",
            input.to_str().unwrap(),
            dir.join("out.psau").to_str().unwrap(),
            "--play",
            played.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let line = String::from_utf8(out.stdout).unwrap();
    assert!(
        line.contains("\"fwsnrseg\":") && line.contains("\"rate\":6000"),
        "{line}"
    );
    let w = wav::read(&std::fs::read(&played).unwrap()).unwrap();
    assert_eq!(w.rate, 44_100);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn score_measures_shipped_bytes_like_the_cook_itself() {
    let src = wav_of(22_050, tone(22_050, 0.8, &[220.0, 1_700.0, 4_100.0]));
    let rate = 11_025;
    let new = cook(&src, &CookOptions::one_shot(rate));
    let s = psx_audio_cook::score(&src, &new.adpcm, rate, 0);
    // Same measurement as the playback path the rate study used.
    let reference = psx_audio_cook::reference_44k(&src);
    let played = psx_audio_cook::playback(&new);
    let n = reference.len().min(played.len());
    let direct = metrics::fw_snr_seg_db(&reference[..n], &played[..n], 11_025.0);
    assert!((s.fw_snr_seg_db - direct).abs() < 1e-9);
    // The previous pipeline's bytes at the same rate score lower.
    let pcm: Vec<i16> = src.samples.iter().map(|&v| v as i16).collect();
    let (_, old) = legacy::hl_cook(&pcm, src.rate, rate, 0.9);
    let o = psx_audio_cook::score(&src, &old, rate, 0);
    assert!(s.fw_snr_seg_db > o.fw_snr_seg_db);
    // A leading silent block, skipped, measures the same as without it.
    let mut padded = vec![0u8; adpcm::BLOCK_BYTES];
    padded.extend_from_slice(&new.adpcm);
    let p = psx_audio_cook::score(&src, &padded, rate, adpcm::BLOCK_SAMPLES);
    assert!((p.fw_snr_seg_db - s.fw_snr_seg_db).abs() < 1e-9);
}

#[test]
fn gauss_compensation_keeps_the_normalised_level() {
    // A full-scale sound with a lot of energy near the playback Nyquist (a
    // gunshot's crack): the pre-emphasis overshoots full scale. The level is
    // kept and the overshoot clamped, not paid for with the whole sound.
    let src = wav_of(
        22_050,
        tone(22_050, 0.5, &[300.0, 3_900.0, 4_700.0])
            .iter()
            .map(|v| v * 3.6)
            .collect(),
    );
    let rms =
        |v: &[i16]| (v.iter().map(|&x| (x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    let with = cook(&src, &CookOptions::one_shot(11_025));
    let mut plain = CookOptions::one_shot(11_025);
    plain.compensate_gauss = false;
    let without = cook(&src, &plain);
    let level = |c: &psx_audio_cook::Cooked| rms(&spu_play::play(&c.decoded(), c.rate));
    let db = 20.0 * (level(&with) / level(&without)).log10();
    assert!(db > -1.0, "compensation cost {db:.2} dB of playback level");
    assert!(
        with.pcm.iter().any(|&v| v == i16::MAX || v == i16::MIN),
        "overshoot is clamped"
    );
}

#[test]
fn normalisation_follows_the_source_not_what_survives_the_resample() {
    // A quiet 300 Hz tone under a loud 4.5 kHz one, cooked at 5 kHz: the
    // resampler removes the loud part, and the quiet tone must keep its
    // level (peak 0.9 of the source, not of what is left).
    let mut samples = tone(22_050, 0.4, &[4_500.0]);
    let quiet = tone(22_050, 0.4, &[300.0]);
    for (s, q) in samples.iter_mut().zip(&quiet) {
        *s = *s * 3.0 + q * 0.3;
    }
    let src = wav_of(22_050, samples.clone());
    let mut o = CookOptions::one_shot(5_000);
    o.compensate_gauss = false;
    let c = cook(&src, &o);
    let peak_src = samples.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let expected = 9_000.0 * 0.3 * 0.9 * 32_767.0 / peak_src;
    // Away from the ends, where the loud tone's abrupt start and stop leave
    // some band-limited energy.
    let steady = &c.pcm[200..c.pcm.len() - 200];
    let peak_out = steady.iter().map(|&v| (v as f64).abs()).fold(0.0, f64::max);
    assert!(
        (peak_out / expected - 1.0).abs() < 0.1,
        "quiet tone peak {peak_out:.0}, expected about {expected:.0}"
    );
}

#[test]
fn restart_keeps_the_length_and_starts_history_free() {
    // 1,000 samples: not whole blocks, so a whole loop would stretch it.
    let w = wav_of(11_025, tone(11_025, 1_000.0 / 11_025.0, &[900.0, 2_300.0]));
    let mut o = CookOptions::one_shot(11_025);
    o.looping = Looping::Restart;
    let c = cook(&w, &o);
    assert_eq!(c.pcm.len(), 1_000, "no stretch");
    assert_eq!(c.adpcm.len(), adpcm_bytes(1_000));
    assert_eq!(c.adpcm[0] >> 4, 0, "first block ignores history");
    assert_eq!(c.loop_block, None);
    assert_eq!(c.adpcm[c.adpcm.len() - 15], adpcm::FLAG_END);
}
