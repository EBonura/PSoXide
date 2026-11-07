//! Static instruction census of a PSX-EXE .text section.
//!
//! Decodes the MIPS-I instruction stream of a linked PSoXide guest executable and
//! reports the instruction mix, the modelled cycle split (loads vs stores vs
//! everything else), delay-slot waste, stack-relative memory traffic, and
//! cache-line alignment of branch targets.
//!
//! The cycle weights come from `emu/crates/emulator-core/src/bus/memory_timing.rs`
//! (silicon-calibrated on SCPH-9902): a KSEG0 main-RAM load is 1 issue + 6 stall
//! cycles, a store is 1 + 1, everything else is 1 plus its own stall class.
//!
//! ```text
//! psoxide-perf text-census build/examples/mipsel-sony-psx/release/editor-playtest.exe
//! ```
//!
//! `.text` extent is auto-detected from `jr $ra` density, then validated by
//! invalid-encoding rate. Both are printed so a bad guess is visible.

use std::io::Write;

use crate::util::{add_count, usage_error, Cli, Error, OrderedMap, Result, Token};

const RAM_LOAD_CYCLES: i64 = 7;
const RAM_STORE_CYCLES: i64 = 2;

fn load_op(op: u32) -> Option<&'static str> {
    Some(match op {
        0x23 => "lw",
        0x21 => "lh",
        0x25 => "lhu",
        0x20 => "lb",
        0x24 => "lbu",
        0x22 => "lwl",
        0x26 => "lwr",
        _ => return None,
    })
}

fn store_op(op: u32) -> Option<&'static str> {
    Some(match op {
        0x2B => "sw",
        0x29 => "sh",
        0x28 => "sb",
        0x2A => "swl",
        0x2E => "swr",
        _ => return None,
    })
}

fn other_op(op: u32) -> Option<&'static str> {
    Some(match op {
        0x09 => "addiu",
        0x0C => "andi",
        0x0D => "ori",
        0x0F => "lui",
        0x04 => "beq",
        0x05 => "bne",
        0x06 => "blez",
        0x07 => "bgtz",
        0x01 => "bcondz",
        0x02 => "j",
        0x03 => "jal",
        0x0A => "slti",
        0x0B => "sltiu",
        0x0E => "xori",
        0x08 => "addi",
        0x10 => "cop0",
        0x12 => "cop2",
        0x32 => "lwc2",
        0x3A => "swc2",
        _ => return None,
    })
}

fn special(funct: u32) -> Option<&'static str> {
    Some(match funct {
        0x00 => "sll",
        0x02 => "srl",
        0x03 => "sra",
        0x04 => "sllv",
        0x06 => "srlv",
        0x07 => "srav",
        0x08 => "jr",
        0x09 => "jalr",
        0x10 => "mfhi",
        0x12 => "mflo",
        0x11 => "mthi",
        0x13 => "mtlo",
        0x18 => "mult",
        0x19 => "multu",
        0x1A => "div",
        0x1B => "divu",
        0x20 => "add",
        0x21 => "addu",
        0x22 => "sub",
        0x23 => "subu",
        0x24 => "and",
        0x25 => "or",
        0x26 => "xor",
        0x27 => "nor",
        0x2A => "slt",
        0x2B => "sltu",
        0x0C => "syscall",
        0x0D => "break",
        _ => return None,
    })
}

const BRANCH_OPS: [u32; 7] = [0x04, 0x05, 0x06, 0x07, 0x01, 0x02, 0x03];
const COND_BRANCH_OPS: [u32; 5] = [0x04, 0x05, 0x06, 0x07, 0x01];

/// A mnemonic, or `None` when the encoding is not a MIPS-I instruction.
pub fn decode(word: u32) -> Option<&'static str> {
    if word == 0 {
        return Some("nop");
    }
    let op = word >> 26;
    if op == 0 {
        return special(word & 0x3F);
    }
    load_op(op)
        .or_else(|| store_op(op))
        .or_else(|| other_op(op))
}

/// Last 4 KiB window that still contains a `jr $ra`, plus one window of slack.
pub fn find_text_end(words: &[u32], base: u32) -> u32 {
    let window = 1024;
    let mut last = 0usize;
    let mut start = 0;
    while start < words.len() {
        let end = (start + window).min(words.len());
        if words[start..end].contains(&0x03E0_0008) {
            last = start + window;
        }
        start += window;
    }
    base.wrapping_add((last * 4) as u32)
}

/// Print the census of the PSX-EXE image `data` (named `path` in the report).
pub fn census(path: &str, data: &[u8], out: &mut dyn Write) -> Result<()> {
    if data.get(..8) != Some(b"PS-X EXE") {
        return Err(Error(format!("{path}: not a PSX-EXE")));
    }
    let word_at = |offset: usize| -> Result<u32> {
        data.get(offset..offset + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| Error("unpack_from requires a buffer of more bytes".to_string()))
    };
    let (base, size) = (word_at(0x18)?, word_at(0x1C)? as usize);
    let mut payload = Vec::with_capacity(size / 4);
    for i in 0..size / 4 {
        payload.push(word_at(0x800 + i * 4)?);
    }
    let end = find_text_end(&payload, base);
    let n = ((end - base) / 4) as usize;
    let words = &payload[..n.min(payload.len())];
    let n = words.len();
    let nf = n as f64;

    let mut mix: OrderedMap<&'static str, i64> = OrderedMap::new();
    let mut invalid = 0i64;
    let (mut sp_loads, mut sp_stores) = (0i64, 0i64);
    for &word in words {
        let Some(name) = decode(word) else {
            invalid += 1;
            add_count(&mut mix, "<invalid>", 1);
            continue;
        };
        add_count(&mut mix, name, 1);
        let op = word >> 26;
        if (word >> 21) & 0x1F == 29 {
            if load_op(op).is_some() {
                sp_loads += 1;
            } else if store_op(op).is_some() {
                sp_stores += 1;
            }
        }
    }
    let count = |name: &str| mix.iter().find(|(k, _)| *k == name).map_or(0, |(_, v)| *v);
    let load_names = ["lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr"];
    let store_names = ["sw", "sh", "sb", "swl", "swr"];
    let loads: i64 = load_names.iter().map(|m| count(m)).sum();
    let stores: i64 = store_names.iter().map(|m| count(m)).sum();
    let others = n as i64 - loads - stores;
    let cycles = loads * RAM_LOAD_CYCLES + stores * RAM_STORE_CYCLES + others;
    let div = |a: f64, b: f64| -> Result<f64> {
        if b == 0.0 {
            Err(Error("division by zero".to_string()))
        } else {
            Ok(a / b)
        }
    };

    writeln!(out, "image      {path}")?;
    writeln!(
        out,
        ".text      {base:#010x}..{end:#010x}  ({:.0} KiB, {n} instructions)",
        (end - base) as f64 / 1024.0
    )?;
    writeln!(
        out,
        "payload    {:.0} KiB total (text + data)",
        size as f64 / 1024.0
    )?;
    writeln!(
        out,
        "invalid    {invalid} ({:.2}%)  <- above ~1% means the .text guess is wrong",
        div(100.0 * invalid as f64, nf)?
    )?;
    writeln!(out)?;
    writeln!(out, "instruction mix")?;
    let mut ranked: Vec<&(&str, i64)> = mix.iter().collect();
    ranked.sort_by_key(|item| -item.1);
    for (name, c) in ranked.into_iter().take(24) {
        writeln!(
            out,
            "  {name:<10} {c:>7}  {:>5.2}%",
            div(100.0 * *c as f64, nf)?
        )?;
    }
    writeln!(out)?;
    let cyc = cycles as f64;
    writeln!(
        out,
        "modelled cycles {cycles} over {n} instructions = {:.2} CPI (excludes I-cache refill)",
        div(cyc, nf)?
    )?;
    writeln!(
        out,
        "  loads   {loads:>6} ({:>5.2}% of instructions) = {:>5.1}% of cycles",
        div(100.0 * loads as f64, nf)?,
        div(100.0 * (loads * RAM_LOAD_CYCLES) as f64, cyc)?
    )?;
    writeln!(
        out,
        "  stores  {stores:>6} ({:>5.2}%) = {:>5.1}% of cycles",
        div(100.0 * stores as f64, nf)?,
        div(100.0 * (stores * RAM_STORE_CYCLES) as f64, cyc)?
    )?;
    writeln!(
        out,
        "  other   {others:>6} ({:>5.2}%) = {:>5.1}% of cycles",
        div(100.0 * others as f64, nf)?,
        div(100.0 * others as f64, cyc)?
    )?;
    writeln!(out)?;
    let sp_cycles = sp_loads * RAM_LOAD_CYCLES + sp_stores * RAM_STORE_CYCLES;
    writeln!(out, "stack traffic ($sp-relative)")?;
    writeln!(
        out,
        "  loads   {sp_loads:>6} = {:.1}% of all loads",
        div(100.0 * sp_loads as f64, loads as f64)?
    )?;
    writeln!(
        out,
        "  stores  {sp_stores:>6} = {:.1}% of all stores",
        div(100.0 * sp_stores as f64, stores as f64)?
    )?;
    writeln!(
        out,
        "  cycles  {sp_cycles} = {:.1}% of modelled cycles",
        div(100.0 * sp_cycles as f64, cyc)?
    )?;
    writeln!(
        out,
        "  the same traffic against the scratchpad costs {} cycles",
        sp_loads + sp_stores
    )?;
    writeln!(out)?;

    let nops = count("nop");
    let (mut after_load, mut after_branch, mut after_mf) = (0i64, 0i64, 0i64);
    for i in 1..n {
        if words[i] != 0 {
            continue;
        }
        let (prev, op) = (words[i - 1], words[i - 1] >> 26);
        if load_op(op).is_some() {
            after_load += 1;
        } else if BRANCH_OPS.contains(&op) || (op == 0 && matches!(prev & 0x3F, 0x08 | 0x09)) {
            after_branch += 1;
        } else if op == 0 && matches!(prev & 0x3F, 0x10 | 0x12) {
            after_mf += 1;
        }
    }
    let branches: i64 = [
        "beq", "bne", "blez", "bgtz", "bcondz", "j", "jal", "jr", "jalr",
    ]
    .iter()
    .map(|m| count(m))
    .sum();
    writeln!(
        out,
        "delay slots: {nops} nops = {:.2}% of instructions, {:.1}% of cycles",
        div(100.0 * nops as f64, nf)?,
        div(100.0 * nops as f64, cyc)?
    )?;
    writeln!(
        out,
        "  in a branch/jump delay slot {after_branch} = {:.1}% of {branches} branches unfilled",
        div(100.0 * after_branch as f64, branches as f64)?
    )?;
    writeln!(out, "  padding a load-delay slot   {after_load}")?;
    writeln!(out, "  padding an mfhi/mflo hazard {after_mf}")?;
    writeln!(out)?;

    let mut line_pos = [0i64; 4];
    for (i, &word) in words.iter().enumerate() {
        if COND_BRANCH_OPS.contains(&(word >> 26)) {
            let offset = i64::from(word as u16 as i16);
            let target = i64::from(base) + (i as i64 + 1) * 4 + offset * 4;
            line_pos[(target.div_euclid(4)).rem_euclid(4) as usize] += 1;
        }
    }
    let total: i64 = line_pos.iter().sum();
    writeln!(
        out,
        "branch-target position inside the 4-word I-cache line (word0 = line aligned)"
    )?;
    for (k, c) in line_pos.iter().enumerate() {
        writeln!(
            out,
            "  word{k}: {c:>6}  {:>5.1}%",
            div(100.0 * *c as f64, total as f64)?
        )?;
    }
    writeln!(
        out,
        "  a uniform 25/25/25/25 split means nothing is cache-line aligned;"
    )?;
    writeln!(
        out,
        "  a tag miss fills only from the entry word to the end of the line."
    )?;
    Ok(())
}

const USAGE: &str = "psoxide-perf text-census [EXE]";
const DEFAULT_EXE: &str = "build/examples/mipsel-sony-psx/release/editor-playtest.exe";

/// `psoxide-perf text-census [EXE]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let mut exe: Option<String> = None;
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        match token {
            Token::Positional(text) if exe.is_none() => exe = Some(text),
            Token::Positional(text) | Token::Flag(text) => {
                return Ok(usage_error(
                    USAGE,
                    &format!("unrecognized arguments: {text}"),
                ));
            }
        }
    }
    let path = exe.unwrap_or_else(|| DEFAULT_EXE.to_string());
    let data = std::fs::read(&path).map_err(|e| Error(format!("{path}: {e}")))?;
    census(&path, &data, out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_recognises_the_mips_i_subset() {
        assert_eq!(decode(0), Some("nop"));
        assert_eq!(decode(0x03E0_0008), Some("jr"));
        assert_eq!(decode(0x8FBF_001C), Some("lw"));
        assert_eq!(decode(0xAFBF_001C), Some("sw"));
        assert_eq!(decode(0x27BD_FFE0), Some("addiu"));
        assert_eq!(decode(0xFFFF_FFFF), None);
    }

    #[test]
    fn text_end_extends_one_window_past_the_last_return() {
        let mut words = vec![0x2400_0001u32; 4096];
        words[1500] = 0x03E0_0008;
        // The `jr $ra` is in window 1 (words 1024..2048); the end is its window end.
        assert_eq!(find_text_end(&words, 0x8001_0000), 0x8001_0000 + 2048 * 4);
    }
}
