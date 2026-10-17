//! Run the prototype codec over the levels: sizes, round trip, decode work.

use psx_grid::SECTOR_BYTES;

use crate::bits::{PackDec, PackEnc, RcDec, RcEnc};
use crate::codec::{code_room, ColourMode, Env, Params, LABELS};
use crate::load::Loaded;
use crate::tables::Room;

fn diff(a: &Room, b: &Room) -> String {
    macro_rules! chk {
        ($f:ident) => {
            if a.$f != b.$f {
                let i = a.$f.iter().zip(b.$f.iter()).position(|(x, y)| x != y);
                return format!(
                    "{} differs (len {} vs {}, first at {:?}: {:?} vs {:?})",
                    stringify!($f),
                    a.$f.len(),
                    b.$f.len(),
                    i,
                    i.map(|i| &a.$f[i]),
                    i.map(|i| &b.$f[i])
                );
            }
        };
    }
    chk!(sectors);
    chk!(exts);
    chk!(walls);
    chk!(splitbs);
    chk!(material_use);
    chk!(vertices);
    chk!(quads);
    chk!(cells);
    chk!(lod_blocks);
    if a.min_y != b.min_y || a.max_y != b.max_y || a.flags != b.flags {
        return "header fields differ".into();
    }
    "equal".into()
}

pub fn codec(levels: &[Loaded], verbose: bool) {
    println!("\n# Prototype codec (PGC-A): sizes in bytes, round trip checked\n");
    println!("level / room                raw chunk | range coder (pal / delta colour) | bit packed | decisions | raw bits");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let (mut raw_t, mut rc_t, mut pk_t) = (0usize, 0usize, 0usize);
        for r in 0..level.rooms() {
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
            if doc.y_offset % 64 != 0 {
                println!(
                    "{} room {r}: y offset off the fine grid, skipped",
                    level.name
                );
                continue;
            }
            let raw = level.room_bytes(r);
            let mut best_rc = (usize::MAX, 0u32, ColourMode::Palette);
            let mut rows = Vec::new();
            for colour in [
                ColourMode::Palette,
                ColourMode::PaletteDelta,
                ColourMode::PaletteNear,
                ColourMode::Delta,
            ] {
                for shift in [4u32, 5] {
                    let par = Params { colour, unit: 0 };
                    let mut enc = RcEnc::new(shift);
                    let out = match code_room(&mut enc, &env, par, Some(&src)) {
                        Ok(o) => o,
                        Err(e) => {
                            println!("{} room {r}: {e}", level.name);
                            continue;
                        }
                    };
                    let verdict = diff(&out, &src);
                    if verdict != "equal" {
                        println!(
                            "{} room {r} {colour:?}: reconstruction mismatch: {verdict}",
                            level.name
                        );
                        continue;
                    }
                    let (bytes, cost, ops) = enc.finish();
                    // Decode for real.
                    let denv = Env {
                        yoff_f: 0,
                        repeat_on: false,
                        courses: false,
                        open_wall_f: 0,
                        ..env_clone(&env)
                    };
                    let mut dec = RcDec::new(&bytes, shift);
                    let out2 = code_room(&mut dec, &denv, par, None).expect("decode");
                    let v2 = diff(&out2, &src);
                    if v2 != "equal" {
                        println!("{} room {r} {colour:?}: DECODE mismatch: {v2}", level.name);
                        continue;
                    }
                    rows.push((colour, shift, bytes.len(), cost, ops, dec.ops));
                    if bytes.len() < best_rc.0 {
                        best_rc = (bytes.len(), shift, colour);
                    }
                }
            }
            // Bit-packed variant with the best colour mode.
            let par = Params {
                colour: best_rc.2,
                unit: 0,
            };
            let mut pe = PackEnc::default();
            let _ = code_room(&mut pe, &env, par, Some(&src));
            let pack_bytes = pe.bits.len().div_ceil(8);
            let denv = Env {
                yoff_f: 0,
                repeat_on: false,
                courses: false,
                open_wall_f: 0,
                ..env_clone(&env)
            };
            let mut pd = PackDec::new(&pe.bits);
            let out3 = code_room(&mut pd, &denv, par, None).expect("pack decode");
            assert_eq!(diff(&out3, &src), "equal");
            let pal = rows
                .iter()
                .filter(|x| x.0 == ColourMode::PaletteNear)
                .map(|x| x.2)
                .min()
                .unwrap_or(0);
            let del = rows
                .iter()
                .filter(|x| x.0 == ColourMode::Delta)
                .map(|x| x.2)
                .min()
                .unwrap_or(0);
            let ops = rows
                .iter()
                .find(|x| x.0 == best_rc.2 && x.1 == best_rc.1)
                .map(|x| x.5);
            println!(
                "{:<14} room {r} {:>3}x{:<3} {:>8} | {:>7} / {:>7} ({:>4.1}% best) | {:>7} ({:>4.1}%) | {:>8} | {:>8}",
                level.name,
                src.width,
                src.depth,
                raw,
                pal,
                del,
                100.0 * best_rc.0 as f64 / raw as f64,
                pack_bytes,
                100.0 * pack_bytes as f64 / raw as f64,
                ops.map_or(0, |o| o.decisions),
                ops.map_or(0, |o| o.raw_bits)
            );
            if verbose {
                // One more encode with fresh counters for the coverage line.
                use std::sync::atomic::Ordering::Relaxed;
                for c in crate::codec::ITEM_COUNT.iter() {
                    c.store(0, Relaxed);
                }
                crate::codec::UV_EXPLICIT.store(0, Relaxed);
                crate::codec::NORMAL_STORED.store(0, Relaxed);
                let mut enc = RcEnc::new(best_rc.1);
                let _ = code_room(
                    &mut enc,
                    &env,
                    Params {
                        colour: best_rc.2,
                        unit: 0,
                    },
                    Some(&src),
                );
                let n: Vec<usize> = crate::codec::ITEM_COUNT
                    .iter()
                    .map(|c| c.load(Relaxed))
                    .collect();
                println!(
                    "      items: floor quad {} / tri {}, ceiling quad {} / tri {}, collision-wall piece {}, derived slot {}, explicit ring {}; walls skipped {} derived skipped {}; explicit uv {}; stored normals {}",
                    n[1], n[2] + n[3], n[4], n[5] + n[6], n[7], n[10], n[9], n[8], n[11],
                    crate::codec::UV_EXPLICIT.load(Relaxed),
                    crate::codec::NORMAL_STORED.load(Relaxed)
                );
            }
            if let Some(o) = ops {
                let probes: u64 = src
                    .quads
                    .iter()
                    .map(|q| {
                        if q.flags & psx_grid::QUAD_TRI != 0 {
                            3
                        } else {
                            4
                        }
                    })
                    .sum();
                let est = 30 * o.decisions
                    + 8 * o.raw_bits
                    + 25 * probes
                    + 120 * src.quads.len() as u64
                    + 40 * src.cells.len() as u64
                    + 50 * src.sectors.len() as u64;
                println!(
                    "      decode estimate (range coder): {est} cycles = {:.0} per quad, {:.0} per cell ({:.2} frames of 1,128,960)",
                    est as f64 / src.quads.len().max(1) as f64,
                    est as f64 / src.cells.len().max(1) as f64,
                    est as f64 / 1_128_960.0
                );
                // Bit packed: a decision is a shift and mask from a bit buffer.
                let pest = 6 * pe.ops.decisions
                    + 8 * pe.ops.raw_bits / 6
                    + 25 * probes
                    + 120 * src.quads.len() as u64
                    + 40 * src.cells.len() as u64
                    + 50 * src.sectors.len() as u64;
                println!(
                    "      decode estimate (bit packed):  {pest} cycles = {:.0} per quad, {:.0} per cell ({:.2} frames)",
                    pest as f64 / src.quads.len().max(1) as f64,
                    pest as f64 / src.cells.len().max(1) as f64,
                    pest as f64 / 1_128_960.0
                );
            }
            if verbose {
                if let Some(x) = rows.iter().find(|x| x.0 == best_rc.2 && x.1 == best_rc.1) {
                    let total: f64 = x.3.iter().sum();
                    for (i, name) in LABELS.iter().enumerate() {
                        println!(
                            "      {:<16} {:>8.0} B ({:>4.1}%)",
                            name,
                            x.3[i] / 8.0,
                            100.0 * x.3[i] / total
                        );
                    }
                }
            }
            raw_t += raw;
            rc_t += best_rc.0;
            pk_t += pack_bytes;
        }
        println!(
            "{:<14} total       {:>8} | {:>7} ({:>4.1}%) | {:>7} ({:>4.1}%)   pages: {} -> {} (rc)",
            level.name,
            raw_t,
            rc_t,
            100.0 * rc_t as f64 / raw_t.max(1) as f64,
            pk_t,
            100.0 * pk_t as f64 / raw_t.max(1) as f64,
            raw_t.div_ceil(SECTOR_BYTES),
            rc_t.div_ceil(SECTOR_BYTES)
        );
    }
}

fn env_clone<'a>(e: &Env<'a>) -> Env<'a> {
    Env {
        s: e.s,
        w: e.w,
        d: e.d,
        origin: e.origin,
        materials: e.materials,
        yoff_f: e.yoff_f,
        repeat_on: e.repeat_on,
        courses: e.courses,
        open_wall_f: e.open_wall_f,
    }
}

/// Size of the codec when the room is cut into independently decodable
/// units of `unit` sectors: neighbours and adaptive state reset at every
/// border, plus 4 bytes of coder flush and a 4 byte directory entry per unit.
pub fn units(levels: &[Loaded]) {
    println!("\n# Cost of independent decode units (bytes; flush 4 B + directory 4 B per unit included)\n");
    println!("level           whole room |  unit 16        |  unit 8         |  unit 4");
    for level in levels {
        let lv = level.level();
        let s = lv.sector_units() as i32;
        let mut row = String::new();
        let mut sizes = [0usize; 4];
        let mut psizes = [0usize; 4];
        let mut counts = [0usize; 4];
        for r in 0..level.rooms() {
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
            for (k, unit) in [0usize, 16, 8, 4].into_iter().enumerate() {
                let mut best = usize::MAX;
                for colour in [ColourMode::Palette, ColourMode::PaletteNear] {
                    for shift in [4u32, 5] {
                        let mut enc = RcEnc::new(shift);
                        if code_room(&mut enc, &env, Params { colour, unit }, Some(&src)).is_ok() {
                            let (b, _, _) = enc.finish();
                            best = best.min(b.len());
                        }
                    }
                }
                let n_units = if unit == 0 {
                    1
                } else {
                    src.width.div_ceil(unit) * src.depth.div_ceil(unit)
                };
                sizes[k] += best + if unit == 0 { 0 } else { 8 * n_units };
                counts[k] += n_units;
                let mut pbest = usize::MAX;
                for colour in [ColourMode::Palette, ColourMode::PaletteNear] {
                    let mut pe = crate::bits::PackEnc::default();
                    if code_room(&mut pe, &env, Params { colour, unit }, Some(&src)).is_ok() {
                        pbest = pbest.min(pe.bits.len().div_ceil(8));
                    }
                }
                psizes[k] += pbest + if unit == 0 { 0 } else { 4 * n_units };
            }
        }
        for k in 1..4 {
            row.push_str(&format!(
                "| {:>6} ({:+.0}%, {:>3}u) ",
                sizes[k],
                100.0 * (sizes[k] as f64 / sizes[0] as f64 - 1.0),
                counts[k]
            ));
        }
        let mut prow = String::new();
        for k in 1..4 {
            prow.push_str(&format!(
                "| {:>6} ({:+.0}%) ",
                psizes[k],
                100.0 * (psizes[k] as f64 / psizes[0] as f64 - 1.0)
            ));
        }
        println!("{:<14} {:>10} {row}", level.name, sizes[0]);
        println!("   bit packed  {:>10} {prow}", psizes[0]);
    }
}
