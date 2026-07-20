use std::{env, fs, path::PathBuf};

// Sanitised replicas of hl-psx's Hazard Course resident/core and t0a0
// per-map bank layouts.  Only rates, decoded lengths, ADPCM block counts,
// and loop ownership are retained; no Valve audio samples are embedded.
// This preserves the exact 508 KiB back-to-back upload workload that ran on
// the real console while making the fixture safe to keep in PSoXide.
const CORE_LAYOUT: &[(u32, u32, u32, bool)] = &[
    (5000, 28, 1, false),
    (11025, 13887, 496, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (11025, 3791, 136, false),
    (8000, 5551, 199, false),
    (11025, 14347, 513, false),
    (8000, 4351, 156, false),
    (5000, 28, 1, false),
    (8000, 3439, 123, false),
    (8000, 6524, 233, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 1197, 43, false),
    (11025, 7263, 260, false),
    (8000, 11816, 422, false),
    (8000, 7244, 259, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 6080, 218, false),
    (8000, 4817, 173, false),
    (11025, 7731, 277, false),
    (8000, 2131, 77, false),
    (8000, 1763, 63, false),
    (11025, 2373, 85, false),
    (5000, 839, 30, false),
    (5000, 28, 1, false),
    (8000, 6360, 228, false),
    (8000, 19314, 690, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 4310, 154, false),
    (8000, 5675, 203, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 319, 12, false),
    (5000, 28, 1, false),
    (8000, 25224, 901, false),
    (8000, 28848, 1031, false),
    (5000, 4007, 144, true),
    (5000, 10308, 369, true),
    (5000, 335, 12, false),
    (11025, 14046, 502, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 1498, 54, false),
    (5000, 3813, 137, false),
    (5000, 1691, 61, false),
    (5000, 2991, 107, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 1346, 49, false),
    (5000, 2844, 102, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 2657, 95, false),
    (5000, 2634, 95, false),
    (5000, 229, 9, false),
    (5000, 599, 22, false),
    (5000, 917, 33, false),
];

// The exact resident-bank transition used when Hazard Course is selected:
// the menu initially owns the full chunk 3000 bank, then game startup calls
// spu::init() and replaces it with the light chunk 3050 profile before t0a0's
// map bank is appended. PA2 accidentally used chunk 3051 instead.
const FULL_LAYOUT: &[(u32, u32, u32, bool)] = &[
    (11025, 6604, 236, false),
    (11025, 13887, 496, false),
    (11025, 11452, 409, false),
    (11025, 11727, 419, false),
    (11025, 6069, 217, false),
    (11025, 16048, 574, false),
    (11025, 4703, 168, false),
    (11025, 3791, 136, false),
    (8000, 5551, 199, false),
    (11025, 14347, 513, false),
    (8000, 4351, 156, false),
    (11025, 9441, 338, false),
    (8000, 3439, 123, false),
    (8000, 6524, 233, false),
    (8000, 800, 29, false),
    (8000, 800, 29, false),
    (5000, 1197, 43, false),
    (11025, 7263, 260, false),
    (8000, 11816, 422, false),
    (8000, 7244, 259, false),
    (8000, 20101, 718, false),
    (11025, 9470, 339, false),
    (8000, 6080, 218, false),
    (8000, 4817, 173, false),
    (11025, 7731, 277, false),
    (8000, 2131, 77, false),
    (8000, 1763, 63, false),
    (11025, 2373, 85, false),
    (5000, 839, 30, false),
    (8000, 6111, 219, false),
    (8000, 6360, 228, false),
    (8000, 19314, 690, false),
    (8000, 4342, 156, false),
    (8000, 10371, 371, false),
    (8000, 4310, 154, false),
    (8000, 5675, 203, false),
    (8000, 1962, 71, false),
    (8000, 10462, 374, false),
    (8000, 5623, 201, false),
    (8000, 12226, 437, false),
    (8000, 6197, 222, false),
    (8000, 30157, 1078, false),
    (8000, 5537, 198, false),
    (8000, 319, 12, false),
    (8000, 33618, 1201, false),
    (8000, 25224, 901, false),
    (8000, 28848, 1031, false),
    (5000, 4007, 144, true),
    (5000, 10308, 369, true),
    (5000, 335, 12, false),
    (11025, 14046, 502, false),
    (11025, 15149, 542, false),
    (11025, 7618, 273, false),
    (5000, 1498, 54, false),
    (5000, 3813, 137, false),
    (5000, 1691, 61, false),
    (5000, 2991, 107, false),
    (5000, 2882, 103, false),
    (5000, 2865, 103, false),
    (5000, 1346, 49, false),
    (5000, 2844, 102, false),
    (5000, 2257, 81, false),
    (5000, 3033, 109, false),
    (5000, 2657, 95, false),
    (5000, 2634, 95, false),
    (5000, 229, 9, false),
    (5000, 599, 22, false),
    (5000, 917, 33, false),
];

const LIGHT_LAYOUT: &[(u32, u32, u32, bool)] = &[
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (11025, 14347, 513, false),
    (8000, 4351, 156, false),
    (5000, 28, 1, false),
    (8000, 3439, 123, false),
    (8000, 6524, 233, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 1197, 43, false),
    (11025, 7263, 260, false),
    (8000, 11816, 422, false),
    (8000, 7244, 259, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 6080, 218, false),
    (8000, 4817, 173, false),
    (11025, 7731, 277, false),
    (8000, 2131, 77, false),
    (8000, 1763, 63, false),
    (5000, 28, 1, false),
    (5000, 839, 30, false),
    (5000, 28, 1, false),
    (8000, 6360, 228, false),
    (8000, 19314, 690, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 4310, 154, false),
    (8000, 5675, 203, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (8000, 319, 12, false),
    (5000, 28, 1, false),
    (8000, 25224, 901, false),
    (8000, 28848, 1031, false),
    (5000, 4007, 144, true),
    (5000, 10308, 369, true),
    (5000, 335, 12, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 1498, 54, false),
    (5000, 3813, 137, false),
    (5000, 1691, 61, false),
    (5000, 2991, 107, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 28, 1, false),
    (5000, 2634, 95, false),
    (5000, 229, 9, false),
    (5000, 599, 22, false),
    (5000, 917, 33, false),
];

const MAP_LAYOUT: &[(u32, u32, u32, bool)] = &[
    (6000, 45418, 1623, false),
    (6000, 56634, 2023, false),
    (3200, 46977, 1678, false),
    (6000, 56997, 2036, false),
    (11025, 29933, 1070, false),
    (8000, 40902, 1461, false),
    (4000, 47883, 1711, false),
    (4000, 54376, 1942, false),
    (8000, 40930, 1462, false),
    (6000, 35131, 1255, false),
    (3200, 48517, 1733, false),
    (6000, 65423, 2337, false),
    (6000, 42570, 1521, false),
    (1400, 7052, 252, true),
    (2400, 3012, 108, false),
    (2400, 1445, 52, false),
    (2400, 1366, 49, false),
    (2400, 1824, 66, false),
    (1400, 2565, 92, true),
    (2400, 1606, 58, false),
    (2400, 3284, 118, false),
    (1400, 2191, 79, true),
    (2400, 1659, 60, false),
    (2400, 933, 34, false),
    (2400, 1781, 64, false),
    (2400, 2094, 75, false),
];

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn psau(rate: u32, samples: u32, blocks: u32, looped: bool, tone: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + blocks as usize * 16);
    out.extend_from_slice(b"PSAU");
    push_u16(&mut out, 1); // asset version
    push_u16(&mut out, 3); // MONO | ONE_SHOT wrapper
    push_u32(&mut out, 20 + blocks * 16);
    out.extend_from_slice(&[1, 1, 0, 0]); // SPU ADPCM, mono
    push_u32(&mut out, rate);
    push_u32(&mut out, samples);
    push_u32(&mut out, blocks);
    push_u32(&mut out, u32::MAX); // wrapper remains one-shot

    for block in 0..blocks {
        let is_last = block + 1 == blocks;
        let use_tone = tone || (blocks == MAP_LAYOUT[0].2 && block < 64);
        // Filter 0 / shift 0 plus alternating max nibbles is an obvious marker;
        // filter 0 / shift 12 with zero nibbles is exact digital silence.
        out.push(if use_tone { 0x00 } else { 0x0C });
        let flag = if looped {
            if blocks == 1 {
                0x07
            } else if block == 0 {
                0x04
            } else if is_last {
                0x03
            } else {
                0
            }
        } else if is_last {
            0x01
        } else {
            0
        };
        out.push(flag);
        out.extend_from_slice(&[if use_tone { 0x78 } else { 0 }; 14]);
    }
    out
}

fn hsfx(layout: &[(u32, u32, u32, bool)], guard_index: Option<usize>) -> Vec<u8> {
    let blobs: Vec<Vec<u8>> = layout
        .iter()
        .enumerate()
        .map(|(index, &(rate, samples, blocks, looped))| {
            // Map sample 0 begins with a short marker. Sample 1 is the loud
            // overrun guard: it is heard only if sample 0's end block was not
            // committed and voice 15 walks into the following allocation.
            let tone = guard_index == Some(index);
            psau(rate, samples, blocks, looped, tone)
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"HSFX");
    push_u32(&mut out, blobs.len() as u32);
    let mut offset = 8 + blobs.len() * 8;
    for blob in &blobs {
        push_u32(&mut out, offset as u32);
        push_u32(&mut out, blob.len() as u32);
        offset += blob.len();
    }
    for blob in blobs {
        out.extend_from_slice(&blob);
    }
    out
}

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }
    !crc
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let core = hsfx(CORE_LAYOUT, None);
    let map = hsfx(MAP_LAYOUT, Some(1));
    fs::write(out_dir.join("pa2_core.hsfx"), &core).expect("write PA2 core fixture");
    fs::write(out_dir.join("pa2_map.hsfx"), &map).expect("write PA2 map fixture");
    fs::write(
        out_dir.join("pa2_meta.rs"),
        format!(
            "pub const PA2_LAYOUT_CRC: u32 = 0x{:08X};\n\
             pub const PA2_CORE_COUNT: u8 = {};\n\
             pub const PA2_MAP_COUNT: u8 = {};\n\
             pub const PA2_CORE_LAYOUT: &[(u32, u32, u32, bool)] = &{:?};\n\
             pub const PA2_MAP_LAYOUT: &[(u32, u32, u32, bool)] = &{:?};\n",
            crc32(&[&core, &map]),
            CORE_LAYOUT.len(),
            MAP_LAYOUT.len(),
            CORE_LAYOUT,
            MAP_LAYOUT,
        ),
    )
    .expect("write PA2 metadata");

    let pa3_full = hsfx(FULL_LAYOUT, Some(16));
    let pa3_light = hsfx(LIGHT_LAYOUT, None);
    let pa3_map = hsfx(MAP_LAYOUT, None);
    fs::write(
        out_dir.join("pa3_meta.rs"),
        format!(
            "pub const PA3_LAYOUT_CRC: u32 = 0x{:08X};\n\
             pub const PA3_FULL_BYTES: u32 = {};\n\
             pub const PA3_LIGHT_BYTES: u32 = {};\n\
             pub const PA3_MAP_BYTES: u32 = {};\n\
             pub const PA3_FULL_LAYOUT: &[(u32, u32, u32, bool)] = &{:?};\n\
             pub const PA3_LIGHT_LAYOUT: &[(u32, u32, u32, bool)] = &{:?};\n\
             pub const PA3_MAP_LAYOUT: &[(u32, u32, u32, bool)] = &{:?};\n",
            crc32(&[&pa3_full, &pa3_light, &pa3_map]),
            FULL_LAYOUT.iter().map(|entry| entry.2 * 16).sum::<u32>(),
            LIGHT_LAYOUT.iter().map(|entry| entry.2 * 16).sum::<u32>(),
            MAP_LAYOUT.iter().map(|entry| entry.2 * 16).sum::<u32>(),
            FULL_LAYOUT,
            LIGHT_LAYOUT,
            MAP_LAYOUT,
        ),
    )
    .expect("write PA3 metadata");
    println!("cargo:rerun-if-changed=build.rs");
}
