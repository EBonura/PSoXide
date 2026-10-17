//! Generic compressors on the raw chunks, and the lossy colour variants.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::bits::RcEnc;
use crate::codec::{code_room, ColourMode, Env, Params};
use crate::load::Loaded;
use crate::tables::Room;

pub fn deflate(bytes: &[u8], level: u32) -> usize {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(bytes).unwrap();
    e.finish().unwrap().len()
}

/// Size of `bytes` in the LZ4 block format with a greedy matcher (a real
/// decoder-compatible stream length: tokens, literal runs, 2 byte offsets).
pub fn lz4_size(bytes: &[u8]) -> usize {
    const MIN: usize = 4;
    let mut table = vec![usize::MAX; 1 << 16];
    let mut out = 0usize;
    let mut anchor = 0usize;
    let mut i = 0usize;
    let hash = |b: &[u8]| -> usize {
        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        (v.wrapping_mul(2654435761) >> 16) as usize
    };
    let ext = |n: usize| -> usize {
        if n >= 15 {
            1 + (n - 15) / 255
        } else {
            0
        }
    };
    while i + MIN + 5 <= bytes.len() {
        let h = hash(&bytes[i..]);
        let cand = table[h];
        table[h] = i;
        if cand != usize::MAX && i - cand < 65536 && bytes[cand..cand + MIN] == bytes[i..i + MIN] {
            let mut len = MIN;
            while i + len < bytes.len() - 5 && bytes[cand + len] == bytes[i + len] {
                len += 1;
            }
            let lit = i - anchor;
            out += 1 + ext(lit) + lit + 2 + ext(len - MIN);
            i += len;
            anchor = i;
        } else {
            i += 1;
        }
    }
    let lit = bytes.len() - anchor;
    out + 1 + ext(lit) + lit
}

pub fn quantise(room: &mut Room, bits: u32) {
    let drop = 8 - bits;
    let q = |v: u32| -> u32 {
        let hi = v >> drop;
        // Replicate the top bits so full scale stays full scale.
        let mut out = hi << drop;
        let mut shift = drop;
        while shift > 0 {
            out |= hi >> (bits.saturating_sub(shift).min(bits));
            shift = shift.saturating_sub(bits);
            if bits == 0 {
                break;
            }
        }
        out & 0xff
    };
    for q4 in room.quads.iter_mut() {
        for c in &mut q4.color {
            *c = q(*c & 0xff) | (q((*c >> 8) & 0xff) << 8) | (q((*c >> 16) & 0xff) << 16);
        }
    }
}

pub fn run(levels: &[Loaded]) {
    println!("\n# Generic compressors on the raw chunks, and lossy colour\n");
    println!("level           raw      deflate-6   deflate-9   lz4-like  | PGC-A rc   +deflate-9 | lossy colour: 6 bit  5 bit  4 bit");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let (mut raw, mut d6, mut d9, mut l4) = (0, 0, 0, 0);
        let (mut rc, mut rcd) = (0, 0);
        let mut lossy = [0usize; 3];
        for r in 0..level.rooms() {
            let bytes = level.room_raw(r);
            raw += bytes.len();
            d6 += deflate(&bytes, 6);
            d9 += deflate(&bytes, 9);
            l4 += lz4_size(&bytes);
            let src = Room::from_view(&level.room(r), s);
            let doc = &level.world.rooms[r];
            let rec = &lv.rooms[r];
            let env = Env {
                s,
                w: src.width,
                d: src.depth,
                origin: [rec.origin[0] / s, rec.origin[2] / s],
                materials: lv.materials,
                yoff_f: doc.y_offset / 64,
                repeat_on: src
                    .quads
                    .iter()
                    .any(|q| q.flags2 & psx_grid::QUAD2_REPEAT != 0),
                courses: doc.wall_mode == psxed_project::grid::WallMode::Authored
                    && doc.band_sets.is_empty(),
                open_wall_f: doc.defaults.open_wall / 64,
            };
            let best = |room: &Room| -> Vec<u8> {
                let mut best: Option<Vec<u8>> = None;
                for colour in [
                    ColourMode::Palette,
                    ColourMode::PaletteDelta,
                    ColourMode::PaletteNear,
                ] {
                    for shift in [4u32, 5] {
                        let mut enc = RcEnc::new(shift);
                        if code_room(&mut enc, &env, Params { colour, unit: 0 }, Some(room)).is_ok()
                        {
                            let (b, _, _) = enc.finish();
                            if best.as_ref().is_none_or(|x| b.len() < x.len()) {
                                best = Some(b);
                            }
                        }
                    }
                }
                best.unwrap_or_default()
            };
            let b = best(&src);
            rc += b.len();
            rcd += deflate(&b, 9);
            for (k, bits) in [6u32, 5, 4].into_iter().enumerate() {
                let mut q = src.clone();
                quantise(&mut q, bits);
                lossy[k] += best(&q).len();
            }
        }
        println!(
            "{:<14} {:>8} {:>10} {:>11} {:>10}  | {:>8} {:>10}  | {:>17} {:>6} {:>6}",
            level.name, raw, d6, d9, l4, rc, rcd, lossy[0], lossy[1], lossy[2]
        );
    }
}
