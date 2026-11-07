//! Static instruction-mix census of a PSX-EXE built by this stack.
//!
//! The guests link with `--oformat=binary` (no symbols), so this works on the
//! flat PSX-EXE: strip the 2 KiB header, disassemble with binutils' MIPS-I
//! disassembler, find the .text/.data boundary heuristically (the linker script
//! places .text first, then .data/.rodata), and count what the compiler emitted.
//!
//! ```text
//! psoxide-perf instr-census <file.exe> [--name label] [--dis out.dis]
//! ```
//!
//! Counts are STATIC (linked text), not dynamic (retired). Use them to pick
//! targets and to sanity-check codegen; use the emulator's counters for cost.
//! Requires `mipsel-none-elf-objdump` (brew install mipsel-none-elf-binutils).

use std::io::Write;
use std::process::Command;

use regex::Regex;

use crate::util::{add_count, usage_error, Cli, Error, OrderedMap, Result, Token};

const OBJDUMP: &str = "mipsel-none-elf-objdump";
const LOAD_ADDR: u64 = 0x8001_0000;
const LOADS: [&str; 7] = ["lw", "lh", "lhu", "lb", "lbu", "lwl", "lwr"];
const STORES: [&str; 5] = ["sw", "sh", "sb", "swl", "swr"];
const COND_BRANCHES: [&str; 9] = [
    "beq", "bne", "beqz", "bnez", "blez", "bgtz", "bltz", "bgez", "b",
];
const JUMPS: [&str; 4] = ["j", "jal", "jr", "jalr"];

/// One disassembled word: address, raw word, mnemonic, operand text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Load address of the word.
    pub addr: u64,
    /// The raw instruction word.
    pub word: u32,
    /// objdump's mnemonic.
    pub mnemonic: String,
    /// objdump's operand text.
    pub operands: String,
}

/// Parse objdump `-D` output into instruction rows.
pub fn parse_disassembly(text: &str) -> Vec<Row> {
    let pattern =
        Regex::new(r"^\s*([0-9a-f]{8}):\s+([0-9a-f]{8})\s+(\S+)\s*(.*)$").expect("static pattern");
    crate::util::splitlines(text)
        .into_iter()
        .filter_map(|line| {
            let c = pattern.captures(line)?;
            Some(Row {
                addr: u64::from_str_radix(&c[1], 16).ok()?,
                word: u32::from_str_radix(&c[2], 16).ok()?,
                mnemonic: c[3].to_string(),
                operands: c[4].to_string(),
            })
        })
        .collect()
}

fn disassemble(path: &str, dis_out: Option<&str>) -> Result<(Vec<u8>, Vec<Row>)> {
    let file = std::fs::read(path).map_err(|e| Error(format!("{path}: {e}")))?;
    let data = file.get(0x800..).unwrap_or(&[]).to_vec();
    let raw = tempfile::Builder::new().suffix(".raw").tempfile()?;
    std::fs::write(raw.path(), &data)?;
    let output = Command::new(OBJDUMP)
        .args(["-D", "-b", "binary", "-m", "mips:3000", "-EL"])
        .arg(format!("--adjust-vma={LOAD_ADDR:#x}"))
        .arg(raw.path())
        .output()
        .map_err(|e| Error(format!("{OBJDUMP}: {e}")))?;
    if !output.status.success() {
        return Err(Error(format!("{OBJDUMP} exited with {}", output.status)));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if let Some(dis_out) = dis_out {
        std::fs::write(dis_out, &text)?;
    }
    Ok((data, parse_disassembly(&text)))
}

/// First window dense in undecodable words marks the start of .data.
pub fn text_end(rows: &[Row]) -> usize {
    let (window, bad_threshold) = (256usize, 16usize);
    if rows.len() > window {
        for i in (0..rows.len() - window).step_by(64) {
            let bad = rows[i..(i + window).min(rows.len())]
                .iter()
                .filter(|r| {
                    r.mnemonic == ".word" || r.mnemonic == "jalx" || r.mnemonic.starts_with("c3")
                })
                .count();
            if bad >= bad_threshold {
                return i;
            }
        }
    }
    rows.len()
}

fn is_branch(mnemonic: &str) -> bool {
    COND_BRANCHES.contains(&mnemonic) || JUMPS.contains(&mnemonic)
}

/// Write the census of `rows` (the full disassembly) for a payload of `data_len` bytes.
pub fn census_report(rows: &[Row], data_len: usize, name: &str, out: &mut dyn Write) -> Result<()> {
    let text = &rows[..text_end(rows)];
    let n = text.len();
    let mut counter: OrderedMap<String, i64> = OrderedMap::new();
    for row in text {
        add_count(&mut counter, row.mnemonic.clone(), 1);
    }
    let count = |mnemonic: &str| counter.get(&mnemonic.to_string()).copied().unwrap_or(0);
    let mut cop2: OrderedMap<&'static str, i64> = OrderedMap::new();
    let (mut nop, mut nop_bd, mut nop_ld, mut nop_cop, mut nop_other) =
        (0i64, 0i64, 0i64, 0i64, 0i64);
    let (mut sext16, mut spill, mut lui) = (0i64, 0i64, 0i64);
    let mut frames: Vec<i64> = Vec::new();
    let (mut mult_then_mf, mut load_then_nop, mut branch_then_nop) = (0i64, 0i64, 0i64);
    let mut prev: Option<&Row> = None;
    for (i, row) in text.iter().enumerate() {
        let next = text.get(i + 1);
        let (word, mn, ops) = (row.word, row.mnemonic.as_str(), row.operands.as_str());
        if word >> 26 == 0x12 {
            if word & 0x0200_0000 != 0 {
                add_count(&mut cop2, "cofun", 1);
            } else {
                let label = match (word >> 21) & 0x1F {
                    0 => "mfc2",
                    2 => "cfc2",
                    4 => "mtc2",
                    6 => "ctc2",
                    _ => "other",
                };
                add_count(&mut cop2, label, 1);
            }
        }
        if word == 0 {
            nop += 1;
            if let Some(p) = prev {
                let (pm, pw) = (p.mnemonic.as_str(), p.word);
                if is_branch(pm) {
                    nop_bd += 1;
                } else if LOADS.contains(&pm) || (pw >> 26 == 0x10 && (pw >> 21) & 0x1F == 0) {
                    nop_ld += 1;
                } else if pw >> 26 == 0x12 || (pw == 0 && i >= 2 && text[i - 2].word >> 26 == 0x12)
                {
                    nop_cop += 1;
                } else {
                    nop_other += 1;
                }
            }
        }
        if mn == "lui" {
            lui += 1;
        }
        if mn == "sll"
            && ops.ends_with(",0x10")
            && next.is_some_and(|x| x.mnemonic == "sra" && x.operands.ends_with(",0x10"))
        {
            sext16 += 1;
        }
        if (mn == "sw" || mn == "lw") && ops.contains("(sp)") {
            spill += 1;
        }
        if mn == "addiu" && ops.starts_with("sp,sp,-") {
            let digits = ops.split('-').nth(1).unwrap_or("");
            frames.push(
                digits
                    .trim()
                    .parse()
                    .map_err(|_| Error(format!("invalid literal for int(): {digits:?}")))?,
            );
        }
        if (mn == "mult" || mn == "multu")
            && next.is_some_and(|x| x.mnemonic == "mflo" || x.mnemonic == "mfhi")
        {
            mult_then_mf += 1;
        }
        if LOADS.contains(&mn) && next.is_some_and(|x| x.word == 0) {
            load_then_nop += 1;
        }
        if is_branch(mn) && next.is_some_and(|x| x.word == 0) {
            branch_then_nop += 1;
        }
        prev = Some(row);
    }

    let loads: i64 = LOADS.iter().map(|m| count(m)).sum();
    let stores: i64 = STORES.iter().map(|m| count(m)).sum();
    let branches: i64 = COND_BRANCHES
        .iter()
        .chain(JUMPS.iter())
        .map(|m| count(m))
        .sum();
    let muldiv = count("mult") + count("multu") + count("div") + count("divu");
    let denominator = n.max(1) as f64;
    let pct = |v: i64| format!("{:.1}%", 100.0 * v as f64 / denominator);
    let share = |v: i64, of: i64| format!("{:.1}", 100.0 * v as f64 / of.max(1) as f64);
    writeln!(
        out,
        "## {name}: payload {data_len} B, text~{} B ({n} instrs), data~{} B",
        n * 4,
        data_len as i64 - n as i64 * 4
    )?;
    writeln!(
        out,
        "- nop {nop} ({}): branch-delay {nop_bd}, load-delay {nop_ld}, GTE/COP gaps {nop_cop}, other {nop_other}",
        pct(nop)
    )?;
    writeln!(
        out,
        "- branches+jumps {branches}, delay slot is nop {branch_then_nop} ({}%)",
        share(branch_then_nop, branches)
    )?;
    writeln!(
        out,
        "- loads {loads} ({}), followed by nop {load_then_nop} ({}%); stores {stores} ({})",
        pct(loads),
        share(load_then_nop, loads),
        pct(stores)
    )?;
    writeln!(
        out,
        "- sp-relative lw/sw {spill} ({}); unaligned lwl/lwr/swl/swr {}",
        pct(spill),
        count("lwl") + count("lwr") + count("swl") + count("swr")
    )?;
    writeln!(
        out,
        "- mult/multu/div/divu {}/{}/{}/{} (total {muldiv}); mult immediately followed by mflo/mfhi {mult_then_mf}",
        count("mult"),
        count("multu"),
        count("div"),
        count("divu")
    )?;
    let cop2_repr = cop2
        .iter()
        .map(|(k, v)| format!("'{k}': {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "- COP2 {{{cop2_repr}}}")?;
    writeln!(
        out,
        "- jal {} jalr {} jr {}; lui {lui} ({}); sll/sra 16 sign-extend pairs {sext16}; andi {}; li {}; move {}",
        count("jal"),
        count("jalr"),
        count("jr"),
        pct(lui),
        count("andi"),
        count("li"),
        count("move")
    )?;
    if !frames.is_empty() {
        frames.sort();
        writeln!(
            out,
            "- stack frames {}: median {} B, p90 {} B, max {} B, >=128 B {}",
            frames.len(),
            frames[frames.len() / 2],
            frames[(frames.len() as f64 * 0.9) as usize],
            frames[frames.len() - 1],
            frames.iter().filter(|f| **f >= 128).count()
        )?;
    }
    // `Counter.most_common(14)`: descending count, ties in first-seen order.
    let mut ranked: Vec<&(String, i64)> = counter.iter().collect();
    ranked.sort_by_key(|item| -item.1);
    let top = ranked
        .iter()
        .take(14)
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "- top: {top}")?;
    Ok(())
}

const USAGE: &str = "psoxide-perf instr-census EXE [--name LABEL] [--dis OUT.dis]";

/// `psoxide-perf instr-census EXE [--name LABEL] [--dis OUT.dis]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let (mut exe, mut name, mut dis) = (None, None, None);
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        let step: std::result::Result<(), String> = match token {
            Token::Flag(flag) => match flag.as_str() {
                "--name" => cli.value(&flag).map(|v| name = Some(v)),
                "--dis" => cli.value(&flag).map(|v| dis = Some(v)),
                other => Err(format!("unrecognized arguments: {other}")),
            },
            Token::Positional(text) if exe.is_none() => {
                exe = Some(text);
                Ok(())
            }
            Token::Positional(text) => Err(format!("unrecognized arguments: {text}")),
        };
        if let Err(message) = step {
            return Ok(usage_error(USAGE, &message));
        }
    }
    let Some(exe) = exe else {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: exe",
        ));
    };
    let (data, rows) = disassemble(&exe, dis.as_deref())?;
    census_report(&rows, data.len(), name.as_deref().unwrap_or(&exe), out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(addr: u64, word: u32, mnemonic: &str, operands: &str) -> Row {
        Row {
            addr,
            word,
            mnemonic: mnemonic.to_string(),
            operands: operands.to_string(),
        }
    }

    #[test]
    fn objdump_lines_are_parsed() {
        let text = "\nDisassembly of section .data:\n\n80010000 <.data>:\n80010000:\t27bdffe0 \taddiu\tsp,sp,-32\n80010004:\t00000000 \tnop\n";
        let rows = parse_disassembly(text);
        assert_eq!(
            rows,
            vec![
                row(0x8001_0000, 0x27bd_ffe0, "addiu", "sp,sp,-32"),
                row(0x8001_0004, 0, "nop", "")
            ]
        );
    }

    #[test]
    fn census_counts_delay_slot_nops_and_frames() {
        let rows = vec![
            row(0x8001_0000, 0x27bd_ffe0, "addiu", "sp,sp,-32"),
            row(0x8001_0004, 0x8fbf_001c, "lw", "ra,28(sp)"),
            row(0x8001_0008, 0, "nop", ""),
            row(0x8001_000c, 0x03e0_0008, "jr", "ra"),
            row(0x8001_0010, 0, "nop", ""),
        ];
        let mut out = Vec::new();
        census_report(&rows, 20, "t", &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("## t: payload 20 B, text~20 B (5 instrs), data~0 B"),
            "{text}"
        );
        assert!(
            text.contains("- nop 2 (40.0%): branch-delay 1, load-delay 1, GTE/COP gaps 0, other 0"),
            "{text}"
        );
        assert!(text.contains("- sp-relative lw/sw 1 (20.0%)"), "{text}");
        assert!(
            text.contains("- stack frames 1: median 32 B, p90 32 B, max 32 B, >=128 B 0"),
            "{text}"
        );
        assert!(text.contains("- COP2 {}"), "{text}");
    }
}
