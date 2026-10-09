//! Decode and validate PSoXide PA1/PA2/PA3/PA4/PA5 audio QR payloads.

use std::io::Write;

use crate::util::{base64_decode, crc32, parse_hex, split_words, Error, Reader, Result};

const PA1_STAGE_LABELS: [&str; 5] = [
    "idle_calibration",
    "readn_game_route_off",
    "readn_controller_muted",
    "readn_spu_route_on",
    "paused_post_read",
];
const FIELD_COUNT: usize = 10;
const PA1_BINARY_LEN: usize = 220;
const PA2_STAGE_LABELS: [&str; 6] = [
    "idle_calibration",
    "game_bank_dma_upload",
    "game_voice_active",
    "game_voice_end_guard",
    "settled_dma_voice_active",
    "settled_dma_end_guard",
];
const PA2_BINARY_LEN: usize = 264;
const PA3_STAGE_LABELS: [&str; 6] = [
    "idle_calibration",
    "full_menu_bank",
    "full_to_light_t0a0",
    "map_voices_active",
    "unsafe_live_overwrite",
    "safe_stop_overwrite",
];
const PA3_BINARY_LEN: usize = 272;
const PA4_STAGE_LABELS: [&str; 7] = [
    "idle_calibration",
    "full_menu_bank",
    "voice16_natural_end",
    "handoff_or_init",
    "light_or_hold_a",
    "map_or_hold_b",
    "spu_readback",
];
const PA4_VARIANTS: [&str; 5] = ["baseline", "safe0", "safe1", "safe2", "split"];
const PA4_BINARY_LEN: usize = 320;
const PA5_STAGE_LABELS: [&str; 8] = [
    "bios_snapshot_calibration",
    "full_menu_bank",
    "voice16_natural_end",
    "spu_init_only",
    "light_bank_only",
    "reverb_reset_only",
    "map_bank_only",
    "spu_readback",
];
const PA5_VARIANTS: [&str; 5] = ["control", "depth0", "depth2", "base0", "full0"];
const PA5_BINARY_LEN: usize = 424;
const MARKERS: [&str; 5] = ["PA1/", "PA2/", "PA3/", "PA4/", "PA5/"];

/// The first payload in `value`, from its marker to the next whitespace.
pub fn extract_payload(value: &str) -> Result<&str> {
    let Some(marker) = MARKERS.iter().filter_map(|m| value.find(m)).min() else {
        bail!("no PA1, PA2, PA3, PA4, or PA5 payload found");
    };
    let payload = split_words(&value[marker..])
        .first()
        .copied()
        .ok_or_else(|| Error("list index out of range".into()))?;
    ensure!(payload.contains("/C:"), "audio payload has no CRC suffix");
    Ok(payload)
}

/// Decode `NAME/<base64>/C:<crc>` and return (binary, crc).
///
/// The CRC is carried three times: as the text suffix, as the binary's last
/// word, and implicitly by the bytes themselves. All three must agree.
pub fn probe_binary(payload: &str, name: &str, expected_len: usize) -> Result<(Vec<u8>, u32)> {
    let rest = payload.get(4..).unwrap_or("");
    let (encoded, suffix_crc) = rest
        .rsplit_once("/C:")
        .ok_or_else(|| Error("not enough values to unpack (expected 2, got 1)".into()))?;
    let binary = base64_decode(encoded).map_err(|e| Error(format!("invalid {name} Base64: {e}")))?;
    ensure!(
        binary.len() == expected_len,
        "{name} binary length {} != {expected_len}",
        binary.len()
    );
    let embedded = u32::from_le_bytes(binary[binary.len() - 4..].try_into().expect("4"));
    let calculated = crc32(&binary[..binary.len() - 4]);
    let displayed = parse_hex(suffix_crc)
        .map_err(|_| Error(format!("invalid {name} CRC suffix: {suffix_crc:?}")))?;
    ensure!(
        embedded == calculated && displayed == u64::from(calculated),
        "{name} CRC mismatch: suffix={displayed:08X} embedded={embedded:08X} calculated={calculated:08X}"
    );
    Ok((binary, calculated))
}

/// One decoded stage: the ten words of a stage record.
type Stage = [u32; FIELD_COUNT];

fn stage_words(
    reader: &mut Reader,
    schema: &str,
    labels: &[&str],
) -> Result<Vec<Stage>> {
    let mut stages = Vec::new();
    for expected in 0..labels.len() {
        let mut words = [0u32; FIELD_COUNT];
        for word in words.iter_mut() {
            *word = reader.u32()?;
        }
        let stage_id = words[0] >> 24;
        ensure!(
            stage_id as usize == expected,
            "{schema} stage order mismatch: got {stage_id}, want {expected}"
        );
        stages.push(words);
    }
    Ok(stages)
}

fn check_dimensions(
    schema: &str,
    labels: &[&str],
    stage_count: u8,
    field_count: u8,
) -> Result<()> {
    ensure!(
        usize::from(stage_count) == labels.len() && usize::from(field_count) == FIELD_COUNT,
        "unexpected {schema} dimensions: stages={stage_count} fields={field_count}"
    );
    Ok(())
}

fn header(binary: &[u8]) -> Result<(Reader<'_>, [u8; 4], u8, u8, u8, u8)> {
    let mut reader = Reader::new(binary, 0);
    let magic: [u8; 4] = reader.bytes(4)?.try_into().expect("4");
    let version = reader.u8()?;
    let stage_count = reader.u8()?;
    let field_count = reader.u8()?;
    let run = reader.u8()?;
    Ok((reader, magic, version, stage_count, field_count, run))
}

fn bad_schema(schema: &str, magic: [u8; 4], version: u8) -> Error {
    let shown: String = magic.iter().map(|&b| b as char).collect();
    Error(format!(
        "unsupported {schema} schema: magic=b'{shown}' version={version}"
    ))
}

/// Everything the printers need, per schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Pa1 {
        run: u8,
        lba: u32,
        stage_frames: u32,
        crc: u32,
        stages: Vec<Stage>,
    },
    Pa2 {
        run: u8,
        layout_crc: u32,
        target_meta: u32,
        target_hash: u32,
        crc: u32,
        stages: Vec<Stage>,
    },
    Pa3 {
        run: u8,
        layout_crc: u32,
        sizes: [u32; 4],
        crc: u32,
        stages: Vec<Stage>,
    },
    Pa4 {
        version: u8,
        run: u8,
        layout_crc: u32,
        variant: usize,
        wait_vblanks: u32,
        sizes: [u32; 4],
        crc: u32,
        stages: Vec<Stage>,
    },
    Pa5 {
        version: u8,
        run: u8,
        layout_crc: u32,
        variant: usize,
        wait_vblanks: u32,
        sizes: [u32; 4],
        boot_reverb_cfg: Vec<u16>,
        crc: u32,
        stages: Vec<Stage>,
    },
}

pub fn decode_payload(payload: &str) -> Result<Report> {
    if payload.starts_with("PA5/") {
        decode_pa5(payload)
    } else if payload.starts_with("PA4/") {
        decode_pa4(payload)
    } else if payload.starts_with("PA3/") {
        decode_pa3(payload)
    } else if payload.starts_with("PA2/") {
        decode_pa2(payload)
    } else {
        decode_pa1(payload)
    }
}

fn decode_pa1(payload: &str) -> Result<Report> {
    let (binary, crc) = probe_binary(payload, "PA1", PA1_BINARY_LEN)?;
    let (mut reader, magic, version, stage_count, field_count, run) = header(&binary)?;
    let lba = reader.u32()?;
    let stage_frames = reader.u32()?;
    if &magic != b"PA1B" || version != 1 {
        return Err(bad_schema("PA1", magic, version));
    }
    check_dimensions("PA1", &PA1_STAGE_LABELS, stage_count, field_count)?;
    let stages = stage_words(&mut reader, "PA1", &PA1_STAGE_LABELS)?;
    Ok(Report::Pa1 {
        run,
        lba,
        stage_frames,
        crc,
        stages,
    })
}

fn decode_pa2(payload: &str) -> Result<Report> {
    let (binary, crc) = probe_binary(payload, "PA2", PA2_BINARY_LEN)?;
    let (mut reader, magic, version, stage_count, field_count, run) = header(&binary)?;
    let layout_crc = reader.u32()?;
    let target_meta = reader.u32()?;
    let target_hash = reader.u32()?;
    if &magic != b"PA2B" || version != 1 {
        return Err(bad_schema("PA2", magic, version));
    }
    check_dimensions("PA2", &PA2_STAGE_LABELS, stage_count, field_count)?;
    let stages = stage_words(&mut reader, "PA2", &PA2_STAGE_LABELS)?;
    Ok(Report::Pa2 {
        run,
        layout_crc,
        target_meta,
        target_hash,
        crc,
        stages,
    })
}

fn decode_pa3(payload: &str) -> Result<Report> {
    let (binary, crc) = probe_binary(payload, "PA3", PA3_BINARY_LEN)?;
    let (mut reader, magic, version, stage_count, field_count, run) = header(&binary)?;
    let layout_crc = reader.u32()?;
    let sizes = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
    if &magic != b"PA3B" || version != 1 {
        return Err(bad_schema("PA3", magic, version));
    }
    check_dimensions("PA3", &PA3_STAGE_LABELS, stage_count, field_count)?;
    let stages = stage_words(&mut reader, "PA3", &PA3_STAGE_LABELS)?;
    Ok(Report::Pa3 {
        run,
        layout_crc,
        sizes,
        crc,
        stages,
    })
}

fn decode_pa4(payload: &str) -> Result<Report> {
    let (binary, crc) = probe_binary(payload, "PA4", PA4_BINARY_LEN)?;
    let (mut reader, magic, version, stage_count, field_count, run) = header(&binary)?;
    let layout_crc = reader.u32()?;
    let variant = reader.u32()?;
    let wait_vblanks = reader.u32()?;
    let sizes = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
    if &magic != b"PA4B" || !(version == 1 || version == 2) {
        return Err(bad_schema("PA4", magic, version));
    }
    check_dimensions("PA4", &PA4_STAGE_LABELS, stage_count, field_count)?;
    ensure!(
        (variant as usize) < PA4_VARIANTS.len(),
        "unknown PA4 variant {variant}"
    );
    let stages = stage_words(&mut reader, "PA4", &PA4_STAGE_LABELS)?;
    Ok(Report::Pa4 {
        version,
        run,
        layout_crc,
        variant: variant as usize,
        wait_vblanks,
        sizes,
        crc,
        stages,
    })
}

fn decode_pa5(payload: &str) -> Result<Report> {
    let (binary, crc) = probe_binary(payload, "PA5", PA5_BINARY_LEN)?;
    let (mut reader, magic, version, stage_count, field_count, run) = header(&binary)?;
    let layout_crc = reader.u32()?;
    let variant = reader.u32()?;
    let wait_vblanks = reader.u32()?;
    let sizes = [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
    if &magic != b"PA5B" || version != 1 {
        return Err(bad_schema("PA5", magic, version));
    }
    check_dimensions("PA5", &PA5_STAGE_LABELS, stage_count, field_count)?;
    ensure!(
        (variant as usize) < PA5_VARIANTS.len(),
        "unknown PA5 variant {variant}"
    );
    let stages = stage_words(&mut reader, "PA5", &PA5_STAGE_LABELS)?;
    let boot_reverb_cfg = (0..32).map(|_| reader.u16()).collect::<Result<Vec<_>>>()?;
    ensure!(
        reader.offset == binary.len() - 4,
        "PA5 layout ended at {}, expected {}",
        reader.offset,
        binary.len() - 4
    );
    Ok(Report::Pa5 {
        version,
        run,
        layout_crc,
        variant: variant as usize,
        wait_vblanks,
        sizes,
        boot_reverb_cfg,
        crc,
        stages,
    })
}

fn detail(words: &Stage, final_stage: bool, clock: bool) -> String {
    // `clock`: the stage carries a clock pair in its last two words; otherwise a
    // readback hash pair.
    let (a, b) = (words[8], words[9]);
    if clock && !final_stage {
        format!("CLOCK {a:08X}->{b:08X}")
    } else {
        format!("{} {a:08X}/{b:08X}", if a == b { "MATCH" } else { "MISMATCH" })
    }
}

pub fn print_report(report: &Report, out: &mut dyn Write) -> Result<()> {
    match report {
        Report::Pa1 {
            run,
            lba,
            stage_frames,
            crc,
            stages,
        } => {
            writeln!(out, "# PA1 run={run} lba={lba} stage_frames={stage_frames} crc={crc:08X}")?;
            writeln!(
                out,
                "stage                         tick SPUCNT SPUSTAT CDVOL_L CDVOL_R CDSTAT IRQ CMD SECT  NZ_L NZ_R PEAK_L PEAK_R ENERGY_L ENERGY_R HASH_L   HASH_R"
            )?;
            for (w, name) in stages.iter().zip(PA1_STAGE_LABELS) {
                writeln!(
                    out,
                    "{name:<29} {:>4} {:04X}   {:04X}    {:04X}    {:04X}    {:02X}     {:02X}  {:02X}  {:04X}  {:>4} {:>4} {:>6} {:>6} {:>8} {:>8} {:08X} {:08X}",
                    w[0] & 0x00FF_FFFF,
                    w[1] >> 16,
                    w[1] & 0xFFFF,
                    w[2] >> 16,
                    w[2] & 0xFFFF,
                    w[3] >> 24,
                    (w[3] >> 20) & 0x0F,
                    (w[3] >> 16) & 0x0F,
                    w[3] & 0xFFFF,
                    w[6] >> 16,
                    w[6] & 0xFFFF,
                    w[7] >> 16,
                    w[7] & 0xFFFF,
                    w[8],
                    w[9],
                    w[4],
                    w[5]
                )?;
            }
        }
        Report::Pa2 {
            run,
            layout_crc,
            target_meta,
            target_hash,
            crc,
            stages,
        } => {
            writeln!(
                out,
                "# PA2 run={run} layout={layout_crc:08X} target={}B@{}Hz target_hash={target_hash:08X} crc={crc:08X}",
                target_meta & 0xFFFF,
                target_meta >> 16
            )?;
            writeln!(
                out,
                "stage                         tick SPUCNT SPUSTAT VOL_L VOL_R PITCH START CURVOL REPEAT ENDX     V15_END MODE DRAIN TAIL"
            )?;
            for (w, name) in stages.iter().zip(PA2_STAGE_LABELS) {
                writeln!(
                    out,
                    "{name:<29} {:>4} {:04X}   {:04X}    {:04X}  {:04X}  {:04X}  {:04X}  {:04X}   {:04X}   {:08X} {:>7} {:>4} {:>5} {} {:08X}/{:08X}",
                    w[0] & 0x00FF_FFFF,
                    w[1] >> 16,
                    w[1] & 0xFFFF,
                    w[2] >> 16,
                    w[2] & 0xFFFF,
                    w[3] >> 16,
                    w[3] & 0xFFFF,
                    w[5] >> 16,
                    w[5] & 0xFFFF,
                    w[6],
                    if w[6] & (1 << 15) != 0 { "yes" } else { " no" },
                    w[7] >> 16,
                    w[7] & 0xFFFF,
                    if w[8] == w[9] { "MATCH" } else { "MISMATCH" },
                    w[8],
                    w[9]
                )?;
            }
        }
        Report::Pa3 {
            run,
            layout_crc,
            sizes,
            crc,
            stages,
        } => {
            writeln!(
                out,
                "# PA3 run={run} layout={layout_crc:08X} full={}B light={}B map={}B readback={}B crc={crc:08X}",
                sizes[0], sizes[1], sizes[2], sizes[3]
            )?;
            writeln!(
                out,
                "stage                         tick SPUCNT SPUSTAT ENDX     VBLANKS V0(VOL/ENV) V15(VOL/ENV) V16(VOL/ENV) V17(VOL/ENV) READBACK"
            )?;
            for (w, name) in stages.iter().zip(PA3_STAGE_LABELS) {
                let formatted: Vec<String> = (3..7)
                    .map(|i| format!("{:04X}/{:04X}", w[i] >> 16, w[i] & 0xFFFF))
                    .collect();
                writeln!(
                    out,
                    "{name:<29} {:>4} {:04X}   {:04X}    {:08X} {:>7} {} {} {:08X}/{:08X}",
                    w[0] & 0x00FF_FFFF,
                    w[1] >> 16,
                    w[1] & 0xFFFF,
                    w[2],
                    w[7],
                    formatted.join(" "),
                    if w[8] == w[9] { "MATCH" } else { "MISMATCH" },
                    w[8],
                    w[9]
                )?;
            }
        }
        Report::Pa4 {
            version,
            run,
            layout_crc,
            variant,
            wait_vblanks,
            sizes,
            crc,
            stages,
        } => {
            writeln!(
                out,
                "# PA4 run={run} variant={} wait={wait_vblanks} layout={layout_crc:08X} full={}B light={}B map={}B readback={}B crc={crc:08X}",
                PA4_VARIANTS[*variant], sizes[0], sizes[1], sizes[2], sizes[3]
            )?;
            writeln!(
                out,
                "stage                         tick SPUCNT SPUSTAT ENDX     V16_VOL ENV  PITCH START ADSR REPEAT VOICES   VBLANKS CLOCK / READBACK"
            )?;
            for (index, (w, name)) in stages.iter().zip(PA4_STAGE_LABELS).enumerate() {
                // Version 2 reports clock stamps in the hash slots of every stage
                // but the last.
                let clock = *version == 2 && index < 6;
                let text = if clock {
                    format!("CLOCK {:08X}->{:08X}", w[8], w[9])
                } else {
                    format!(
                        "{} {:08X}/{:08X}",
                        if w[8] == w[9] { "MATCH" } else { "MISMATCH" },
                        w[8],
                        w[9]
                    )
                };
                writeln!(
                    out,
                    "{name:<29} {:>4} {:04X}   {:04X}    {:08X} {:04X}    {:04X} {:04X}  {:04X}  {:04X} {:04X}  {:08X} {:>7} {text}",
                    w[0] & 0x00FF_FFFF,
                    w[1] >> 16,
                    w[1] & 0xFFFF,
                    w[2],
                    w[3] >> 16,
                    w[3] & 0xFFFF,
                    w[4] >> 16,
                    w[4] & 0xFFFF,
                    w[5] >> 16,
                    w[5] & 0xFFFF,
                    w[6],
                    w[7]
                )?;
            }
        }
        Report::Pa5 {
            run,
            layout_crc,
            variant,
            wait_vblanks,
            sizes,
            boot_reverb_cfg,
            crc,
            stages,
            ..
        } => {
            writeln!(
                out,
                "# PA5 run={run} variant={} wait={wait_vblanks} layout={layout_crc:08X} full={}B light={}B map={}B readback={}B crc={crc:08X}",
                PA5_VARIANTS[*variant], sizes[0], sizes[1], sizes[2], sizes[3]
            )?;
            writeln!(
                out,
                "stage                         tick SPUCNT SPUSTAT RVOL_L RVOL_R BASE EXT_L EXT_R EON      CFG_N CFG_HASH VBLANKS CLOCK / READBACK"
            )?;
            for (index, (w, name)) in stages.iter().zip(PA5_STAGE_LABELS).enumerate() {
                let final_stage = index + 1 == PA5_STAGE_LABELS.len();
                let text = detail(w, final_stage, true);
                let eon = ((w[5] >> 16) << 16) | (w[4] & 0xFFFF);
                writeln!(
                    out,
                    "{name:<29} {:>4} {:04X}   {:04X}    {:04X}   {:04X}   {:04X} {:04X}  {:04X}  {eon:08X} {:>5} {:08X} {:>7} {text}",
                    w[0] & 0x00FF_FFFF,
                    w[1] >> 16,
                    w[1] & 0xFFFF,
                    w[2] >> 16,
                    w[2] & 0xFFFF,
                    w[3] >> 16,
                    w[3] & 0xFFFF,
                    w[4] >> 16,
                    w[5] & 0xFFFF,
                    w[6],
                    w[7]
                )?;
            }
            writeln!(out, "boot_reverb_cfg:")?;
            for (row, chunk) in boot_reverb_cfg.chunks(8).enumerate() {
                let words: Vec<String> = chunk.iter().map(|v| format!("{v:04X}")).collect();
                writeln!(out, "  {:02}: {}", row * 8, words.join(" "))?;
            }
        }
    }
    Ok(())
}

/// `psoxide-hwtest audio-report <payload-or-file>`
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let [source] = args else {
        bail!("the following arguments are required: payload_or_file");
    };
    let text = match std::fs::read(source) {
        Ok(bytes) if std::path::Path::new(source).is_file() => {
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => source.clone(),
    };
    print_report(&decode_payload(extract_payload(&text)?)?, out)?;
    Ok(0)
}
