//! Generates the constant tables `psx-seq` embeds, from their closed-form
//! definitions. The runtime has no floating point, so the tables are written
//! out as integer literals; `psx-seq`'s tests recompute them from the same
//! formulas to prove the literals match.

use std::fmt::Write;

fn q16(x: f64) -> u32 {
    (x * 65536.0).round() as u32
}

fn array(out: &mut String, name: &str, ty: &str, vals: &[String], per_line: usize) {
    writeln!(out, "pub const {name}: [{ty}; {}] = [", vals.len()).ok();
    for chunk in vals.chunks(per_line) {
        writeln!(out, "    {},", chunk.join(", ")).ok();
    }
    writeln!(out, "];\n").ok();
}

/// Rust source for the tables.
pub fn tables() -> String {
    let mut o = String::new();
    let pitch: Vec<String> = (0..128)
        .map(|i| q16(2f64.powf((i as f64 - 39.0) / 12.0)).to_string())
        .collect();
    array(&mut o, "PITCH_Q16", "u32", &pitch, 8);
    let oct: Vec<String> = (0..256)
        .map(|i| {
            q16(if i == 0 {
                0.5
            } else {
                0.5 * 2f64.powf((i as f64 - 1.0) / 127.0)
            })
            .to_string()
        })
        .collect();
    array(&mut o, "BEND_OCTAVE_Q16", "u32", &oct, 8);
    let semi: Vec<String> = (0..256)
        .map(|i| {
            let k = if i == 0 { 1.0 } else { i as f64 };
            q16(2f64.powf((k - 128.0) / 127.0 * 2.0 / 12.0)).to_string()
        })
        .collect();
    array(&mut o, "BEND_TWO_SEMITONES_Q16", "u32", &semi, 8);
    let pan: Vec<String> = (0..128)
        .map(|i| {
            let v = (i as f64 * std::f64::consts::PI / (2.0 * 127.0)).cos();
            ((v * 32768.0).round() as u32).to_string()
        })
        .collect();
    array(&mut o, "PAN_Q15", "u16", &pan, 16);
    let sine: Vec<String> = (0..64)
        .map(|i| {
            let v = 32767.0 * (std::f64::consts::TAU * i as f64 / 64.0).sin();
            (v.trunc() as i32).to_string()
        })
        .collect();
    array(&mut o, "VIBRATO_SINE", "i16", &sine, 8);
    o
}
