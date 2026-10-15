//! Measurements over a whole cartridge: what each sequence needs and how
//! much SPU RAM that is.

use crate::n64::aseq::{scan, Level, Scan};
use crate::n64::{AudioData, Font, SEQ_NAMES};
use crate::rom::{be_u32, Rom};
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// SPU-ADPCM bytes for `pcm` samples (28 per 16-byte block).
pub fn spu_bytes(pcm: usize) -> usize {
    pcm.div_ceil(28) * 16
}

/// Everything a sequence names, resolved to distinct samples.
pub struct Usage {
    /// The static scan.
    pub scan: Scan,
    /// Soundfont ids in table order.
    pub fonts: Vec<u8>,
    /// Distinct samples as `(audiotable offset, vadpcm bytes, pcm length)`.
    pub samples: BTreeSet<(u32, u32, usize)>,
    /// Instruments the scan named that the font does not define.
    pub missing: usize,
}

/// Resolve what sequence `id` uses against the fonts it lists.
pub fn usage(
    data: &AudioData,
    fonts: &mut BTreeMap<usize, Font>,
    id: usize,
) -> Result<Usage, Error> {
    let seq = data.sequence(id)?;
    let sc = scan(seq);
    let font_ids = data.fonts_of_sequence(id)?;
    let mut samples = BTreeSet::new();
    let mut missing = 0;
    let n = font_ids.len();
    let font_of = |slot: u8| -> Option<usize> {
        let slot = slot as usize;
        (slot < n).then(|| font_ids[n - 1 - slot] as usize)
    };
    let get = |fid: usize, fonts: &mut BTreeMap<usize, Font>| -> Result<(), Error> {
        if let std::collections::btree_map::Entry::Vacant(v) = fonts.entry(fid) {
            v.insert(data.font(fid)?);
        }
        Ok(())
    };
    for &(slot, inst) in &sc.instruments {
        let Some(fid) = font_of(slot) else {
            missing += 1;
            continue;
        };
        get(fid, fonts)?;
        let f = &fonts[&fid];
        match f.instruments.get(inst as usize).and_then(|i| i.as_ref()) {
            Some(i) => {
                for t in [&i.low, &i.normal, &i.high].into_iter().flatten() {
                    let s = &f.samples[t.sample];
                    samples.insert((s.table_off, s.size, s.pcm_len()));
                }
            }
            None => missing += 1,
        }
    }
    let mut drum_fonts: BTreeSet<u8> = sc.drum_keys.iter().map(|k| k.0).collect();
    // A drum layer whose keys the scan could not see still needs the kit.
    if sc.ops.keys().any(|&(l, o)| l == Level::Layer && o == 0xC6) && drum_fonts.is_empty() {
        drum_fonts.insert(0);
    }
    for slot in drum_fonts {
        let Some(fid) = font_of(slot) else { continue };
        get(fid, fonts)?;
        let f = &fonts[&fid];
        let keys: Vec<u8> = sc
            .drum_keys
            .iter()
            .filter(|k| k.0 == slot)
            .map(|k| k.1)
            .collect();
        for (k, d) in f.drums.iter().enumerate() {
            if !keys.is_empty() && !keys.contains(&(k as u8)) {
                continue;
            }
            if let Some(d) = d {
                let s = &f.samples[d.tuned.sample];
                samples.insert((s.table_off, s.size, s.pcm_len()));
            }
        }
    }
    Ok(Usage {
        scan: sc,
        fonts: font_ids,
        samples,
        missing,
    })
}

/// A scene's name and the `(sequence, nature ambience)` pairs its headers name.
pub type SceneMusic = Vec<(String, Vec<(u8, u8)>)>;

/// The music each scene's headers name: sound-settings commands in every
/// header of every scene file.
pub fn scene_music(rom: &Rom) -> Result<SceneMusic, Error> {
    let mut out = Vec::new();
    for (idx, name) in rom.names().iter().enumerate() {
        if !name.ends_with("_scene") {
            continue;
        }
        let Ok(file) = rom.file_at(idx) else { continue };
        let mut headers = vec![0usize];
        let mut found: Vec<(u8, u8)> = Vec::new();
        let mut h = 0;
        while h < headers.len() {
            let mut at = headers[h];
            h += 1;
            for _ in 0..64 {
                let Some(cmd) = file.get(at..at + 8) else {
                    break;
                };
                match cmd[0] {
                    0x14 => break,
                    0x15 => {
                        let pair = (cmd[7], cmd[6]);
                        if !found.contains(&pair) {
                            found.push(pair);
                        }
                    }
                    0x18 => {
                        let seg = be_u32(&file, at + 4)?;
                        if seg >> 24 == 2 {
                            let list = (seg & 0x00FF_FFFF) as usize;
                            for k in 0..3 {
                                let p = be_u32(&file, list + 4 * k).unwrap_or(0);
                                if p >> 24 == 2 && (p & 0x00FF_FFFF) as usize + 8 <= file.len() {
                                    let t = (p & 0x00FF_FFFF) as usize;
                                    if !headers.contains(&t) {
                                        headers.push(t);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                at += 8;
            }
        }
        out.push((name.clone(), found));
    }
    Ok(out)
}

fn kib(b: usize) -> String {
    format!("{:.1}", b as f64 / 1024.0)
}

/// Build the full report.
pub fn report(rom: &Rom) -> Result<String, Error> {
    let data = AudioData::load(rom)?;
    let mut fonts: BTreeMap<usize, Font> = BTreeMap::new();
    let mut o = String::new();
    let n_seq = data.seq_table.entries.len();
    writeln!(
        o,
        "# Survey: {} sequences, {} soundfonts, {} sample banks",
        n_seq,
        data.font_table.entries.len(),
        data.bank_table.entries.len()
    )
    .ok();
    writeln!(
        o,
        "audioseq {} B, audiobank {} B, audiotable {} B\n",
        data.audioseq.len(),
        data.audiobank.len(),
        data.audiotable.len()
    )
    .ok();

    // soundfonts
    writeln!(
        o,
        "## Soundfonts (distinct samples, VADPCM bytes, SPU-ADPCM bytes)"
    )
    .ok();
    writeln!(
        o,
        "font banks inst drums sfx samples vadpcm_KiB spu_KiB loops"
    )
    .ok();
    let mut font_spu = BTreeMap::new();
    for id in 0..data.font_table.entries.len() {
        let f = match data.font(id) {
            Ok(f) => f,
            Err(e) => {
                writeln!(o, "{id:3} ERR {e}").ok();
                continue;
            }
        };
        let vad: usize = f.samples.iter().map(|s| s.size as usize).sum();
        let spu: usize = f.samples.iter().map(|s| spu_bytes(s.pcm_len())).sum();
        let loops = f.samples.iter().filter(|s| s.looped.is_some()).count();
        writeln!(
            o,
            "{:3} {:02x}/{:02x} {:4} {:5} {:3} {:7} {:9} {:7} {:5}",
            id,
            f.banks[0],
            f.banks[1],
            f.instruments.iter().flatten().count(),
            f.drums.iter().flatten().count(),
            f.sfx.iter().flatten().count(),
            f.samples.len(),
            kib(vad),
            kib(spu),
            loops
        )
        .ok();
        font_spu.insert(id, spu);
        fonts.insert(id, f);
    }

    // sequences
    writeln!(o, "\n## Sequences").ok();
    writeln!(
        o,
        "id name size_B fonts used_samples used_spu_KiB whole_fonts_spu_KiB flags"
    )
    .ok();
    let mut usages: BTreeMap<usize, Usage> = BTreeMap::new();
    let mut op_total: BTreeMap<(Level, u8), (u32, usize)> = BTreeMap::new();
    for (id, name) in SEQ_NAMES.iter().enumerate().take(n_seq) {
        let u = match usage(&data, &mut fonts, id) {
            Ok(u) => u,
            Err(e) => {
                writeln!(o, "{id:3} {:18} ERR {e}", name).ok();
                continue;
            }
        };
        let size = data.sequence(id).map(<[u8]>::len).unwrap_or(0);
        let used: usize = u.samples.iter().map(|s| spu_bytes(s.2)).sum();
        let whole: usize = u
            .fonts
            .iter()
            .map(|&f| font_spu.get(&(f as usize)).copied().unwrap_or(0))
            .sum();
        let mut flags = String::new();
        if u.scan.self_modifying {
            flags.push_str(" selfmod");
        }
        if u.scan.dynamic_targets {
            flags.push_str(" dyn");
        }
        if u.scan.io {
            flags.push_str(" io");
        }
        if u.missing > 0 {
            write!(flags, " missing={}", u.missing).ok();
        }
        writeln!(
            o,
            "{:3} {:18} {:6} {:?} ch={} lay={} samples={} used={} whole={}{}",
            id,
            name,
            size,
            u.fonts,
            u.scan.channels.len(),
            u.scan.layers.len(),
            u.samples.len(),
            kib(used),
            kib(whole),
            flags
        )
        .ok();
        for (&k, &c) in &u.scan.ops {
            let e = op_total.entry(k).or_insert((0, 0));
            e.0 += c;
            e.1 += 1;
        }
        usages.insert(id, u);
    }

    // unions: what stays resident, and what each scene needs on top of it
    let union = |ids: &[usize]| -> BTreeSet<(u32, u32, usize)> {
        ids.iter()
            .filter_map(|i| usages.get(i))
            .flat_map(|u| u.samples.iter().copied())
            .collect()
    };
    let size_of =
        |set: &BTreeSet<(u32, u32, usize)>| -> usize { set.iter().map(|s| spu_bytes(s.2)).sum() };
    let field: Vec<usize> = (3..=23).collect();
    let fanfares = [32usize, 33, 34, 36, 43, 57, 59, 67];
    let songs = [37usize, 51, 52, 53, 54, 55, 61, 68, 69, 70, 71, 72, 73, 94];
    let fan_set = union(&fanfares);
    let song_set = union(&songs);
    let tier1 = union(&[34usize, 57, 43, 59]);
    let tier2 = union(&[32usize, 33, 36, 67]);
    let common = tier1.clone();
    let _ = &song_set;
    writeln!(o, "\n## Unions (SPU-ADPCM KiB, distinct samples)").ok();
    writeln!(
        o,
        "field themes 3..23: {} ({} samples)",
        kib(size_of(&union(&field))),
        union(&field).len()
    )
    .ok();
    writeln!(
        o,
        "fanfares {:?}: {} ({} samples)",
        fanfares,
        kib(size_of(&fan_set)),
        fan_set.len()
    )
    .ok();
    writeln!(
        o,
        "ocarina songs {:?}: {} ({} samples)",
        songs,
        kib(size_of(&song_set)),
        song_set.len()
    )
    .ok();
    writeln!(o, "stingers tier 1 [34, 57, 43, 59] (item get, small item, chest, event clear): {} ({} samples)", kib(size_of(&tier1)), tier1.len()).ok();
    writeln!(o, "stingers tier 2 [32, 33, 36, 67] (game over, boss clear, heart, medallion): {} ({} samples)", kib(size_of(&tier2)), tier2.len()).ok();
    let mut worst: Vec<(usize, String)> = Vec::new();
    for (name, found) in &scene_music(rom)? {
        let mut set = common.clone();
        for &(seq, _) in found {
            if seq == 2 {
                set.extend(union(&field));
            } else if let Some(u) = usages.get(&(seq as usize)) {
                set.extend(u.samples.iter().copied());
            }
        }
        worst.push((size_of(&set), name.clone()));
    }
    worst.sort_by_key(|w| std::cmp::Reverse(w.0));
    writeln!(o, "per scene, scene music + tier 1 stingers (top 12):").ok();
    for (b, n) in worst.iter().take(12) {
        writeln!(o, "  {n:24} {}", kib(*b)).ok();
    }
    let mut sizes: Vec<usize> = worst.iter().map(|w| w.0).collect();
    sizes.sort();
    if !sizes.is_empty() {
        writeln!(
            o,
            "scenes: {} median {} KiB max {} KiB",
            sizes.len(),
            kib(sizes[sizes.len() / 2]),
            kib(*sizes.last().unwrap())
        )
        .ok();
        for budget in [256usize, 320, 352, 384] {
            let over = sizes.iter().filter(|&&b| b > budget * 1024).count();
            writeln!(o, "  over {budget} KiB: {over} scenes").ok();
        }
    }

    // opcode histogram
    writeln!(
        o,
        "\n## Opcodes over all sequences (level opcode total_count sequences_using)"
    )
    .ok();
    for ((lv, op), (c, s)) in &op_total {
        if matches!(lv, Level::Layer) && *op < 0xC0 {
            continue;
        }
        writeln!(o, "{:?} {:02X} {:7} {:3}", lv, op, c, s).ok();
    }

    // opcodes the SPU engine cannot honour, by sequence
    writeln!(
        o,
        "\n## Opcodes the SPU engine drops, by sequence (music sequences only: ids 2..=108)"
    )
    .ok();
    let dropped: [(Level, u8, &str); 9] = [
        (Level::Chan, 0xB0, "ldfilter"),
        (Level::Chan, 0xB1, "freefilter"),
        (Level::Chan, 0xB3, "filter"),
        (Level::Chan, 0xBB, "combfilter"),
        (Level::Chan, 0xED, "gain"),
        (Level::Seq, 0xC4, "runseq"),
        (Level::Seq, 0xB0, "ldseq"),
        (Level::Seq, 0x60, "ldres"),
        (Level::Chan, 0x10, "ldsample"),
    ];
    for (lv, op, name) in dropped {
        let users: Vec<&str> = usages
            .iter()
            .filter(|(id, u)| (2..=108).contains(*id) && u.scan.ops.contains_key(&(lv, op)))
            .map(|(id, _)| SEQ_NAMES[*id])
            .collect();
        writeln!(
            o,
            "{name:11} ({lv:?} {op:02X}): {} {:?}",
            users.len(),
            users
        )
        .ok();
    }
    let io_users = usages
        .iter()
        .filter(|(id, u)| (2..=108).contains(*id) && u.scan.io)
        .count();
    let selfmod = usages
        .iter()
        .filter(|(id, u)| (2..=108).contains(*id) && u.scan.self_modifying)
        .count();
    writeln!(
        o,
        "sequences 2..=108 touching IO ports: {io_users}; self-modifying: {selfmod}"
    )
    .ok();

    // scenes
    writeln!(
        o,
        "\n## Scenes (music from the scene header; union of all headers)"
    )
    .ok();
    writeln!(o, "scene seq nature name bgm_spu_KiB").ok();
    let scenes = scene_music(rom)?;
    let mut by_seq: BTreeMap<u8, Vec<String>> = BTreeMap::new();
    for (name, found) in &scenes {
        let mut parts = Vec::new();
        for &(seq, nat) in found {
            let nm = SEQ_NAMES.get(seq as usize).copied().unwrap_or("-");
            let kb = usages
                .get(&(seq as usize))
                .map(|u| kib(u.samples.iter().map(|s| spu_bytes(s.2)).sum()))
                .unwrap_or_else(|| "-".into());
            parts.push(format!("{seq:#04x}/{nat:#04x} {nm} {kb}"));
            by_seq.entry(seq).or_default().push(name.clone());
        }
        writeln!(o, "{name:22} {}", parts.join(" | ")).ok();
    }
    writeln!(o, "\n## Scenes per sequence id").ok();
    for (seq, names) in &by_seq {
        writeln!(
            o,
            "{seq:#04x} {} -> {} scenes",
            SEQ_NAMES.get(*seq as usize).copied().unwrap_or("-"),
            names.len()
        )
        .ok();
    }
    Ok(o)
}
