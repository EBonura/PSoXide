//! Replay JaCzekanski's gte-fuzz console-captured log directly against
//! psx_gte_core -- a real-hardware conformance run with no guest program,
//! no disc, no pad. The log (ps1-tests `gte_valid_0xc0ffee_50.log`) holds,
//! per test: all 64 GTE registers written before the command (`> r[n]`),
//! the command with its field bits, and all 64 registers the REAL CONSOLE
//! held afterwards (`< r[n]`). Registers are written in 0..=63 order via
//! the same write path the console used (MTC2/CTC2 semantics: SXYP push,
//! IR sign-extension, FLAG masking), so a mismatch is either a write-
//! semantics or a compute divergence -- both exactly what we validate.
//!
//! Usage: gte_fuzz_replay <log path>

use psx_gte_core::Gte;

fn parse_cmd(line: &str) -> Option<u32> {
    // "GTE 0x01 RTPS (sf=0, lm=1, tx=2, vx=1, mx=0)"
    let op = u32::from_str_radix(line.split("0x").nth(1)?.get(..2)?, 16).ok()?;
    let field = |name: &str| -> u32 {
        line.split(&format!("{name}="))
            .nth(1)
            .and_then(|s| s.chars().next())
            .and_then(|c| c.to_digit(10))
            .unwrap_or(0)
    };
    let (sf, lm, tx, vx, mx) = (field("sf"), field("lm"), field("tx"), field("vx"), field("mx"));
    Some(0x4A00_0000 | op | (sf << 19) | (mx << 17) | (vx << 15) | (tx << 13) | (lm << 10))
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: gte_fuzz_replay <log>");
    let log = std::fs::read_to_string(&path).expect("log readable");

    let mut inputs: Vec<(u8, u32)> = Vec::with_capacity(64);
    let mut expected: Vec<(u8, u32)> = Vec::with_capacity(64);
    let mut cmd: Option<u32> = None;
    let mut cmd_line = String::new();
    let mut section = String::new();
    let mut test_no = 0u32;

    let mut total = 0u64;
    let mut failed_tests = 0u64;
    let mut reg_mismatches: std::collections::BTreeMap<u8, u64> = Default::default();
    let mut op_fail: std::collections::BTreeMap<String, u64> = Default::default();
    let mut op_total: std::collections::BTreeMap<String, u64> = Default::default();
    let mut shown = 0;

    let finish_test = |inputs: &mut Vec<(u8, u32)>,
                           expected: &mut Vec<(u8, u32)>,
                           cmd: &mut Option<u32>,
                           cmd_line: &mut String,
                           section: &str,
                           test_no: u32,
                           total: &mut u64,
                           failed_tests: &mut u64,
                           reg_mismatches: &mut std::collections::BTreeMap<u8, u64>,
                           op_fail: &mut std::collections::BTreeMap<String, u64>,
                           op_total: &mut std::collections::BTreeMap<String, u64>,
                           shown: &mut i32| {
        let Some(instr) = cmd.take() else {
            inputs.clear();
            expected.clear();
            return;
        };
        if expected.is_empty() {
            inputs.clear();
            return;
        }
        *total += 1;
        *op_total.entry(section.to_string()).or_insert(0) += 1;
        let mut gte = Gte::new();
        for &(idx, value) in inputs.iter() {
            if idx < 32 {
                gte.write_data(idx, value);
            } else {
                gte.write_control(idx - 32, value);
            }
        }
        gte.execute(instr);
        let mut bad: Vec<(u8, u32, u32)> = Vec::new();
        for &(idx, want) in expected.iter() {
            let got = if idx < 32 {
                gte.read_data(idx)
            } else {
                gte.read_control(idx - 32)
            };
            if got != want {
                bad.push((idx, want, got));
            }
        }
        if !bad.is_empty() {
            *failed_tests += 1;
            *op_fail.entry(section.to_string()).or_insert(0) += 1;
            for &(idx, _, _) in &bad {
                *reg_mismatches.entry(idx).or_insert(0) += 1;
            }
            if *shown < 6 {
                *shown += 1;
                println!("FAIL {section} test {test_no}  [{cmd_line}]");
                for (idx, want, got) in bad.iter().take(8) {
                    println!("   r[{idx}] console={want:#010x} ours={got:#010x}");
                }
            }
        }
        inputs.clear();
        expected.clear();
        cmd_line.clear();
    };

    for line in log.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("> r[") {
            if let Some((idx, val)) = parse_reg(rest) {
                inputs.push((idx, val));
            }
        } else if let Some(rest) = line.strip_prefix("< r[") {
            if let Some((idx, val)) = parse_reg(rest) {
                expected.push((idx, val));
            }
        } else if line.starts_with("GTE 0x") && line.contains("sf=") {
            cmd = parse_cmd(line);
            cmd_line = line.to_string();
        } else if line.starts_with("Test ") || line.starts_with("---") || line.starts_with("====") {
            finish_test(
                &mut inputs,
                &mut expected,
                &mut cmd,
                &mut cmd_line,
                &section,
                test_no,
                &mut total,
                &mut failed_tests,
                &mut reg_mismatches,
                &mut op_fail,
                &mut op_total,
                &mut shown,
            );
            if let Some(rest) = line.strip_prefix("Test ") {
                test_no = rest.trim().parse().unwrap_or(0);
            }
            if line.starts_with("---") {
                section = line
                    .trim_start_matches('-')
                    .trim()
                    .split(" (seed")
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    finish_test(
        &mut inputs,
        &mut expected,
        &mut cmd,
        &mut cmd_line,
        &section,
        test_no,
        &mut total,
        &mut failed_tests,
        &mut reg_mismatches,
        &mut op_fail,
        &mut op_total,
        &mut shown,
    );

    println!("\n==== gte_fuzz_replay summary ====");
    println!("tests: {total}  failed: {failed_tests}");
    println!("-- per op (fail/total):");
    for (op, t) in &op_total {
        let f = op_fail.get(op).copied().unwrap_or(0);
        if f > 0 {
            println!("   {op}: {f}/{t}");
        }
    }
    println!("-- mismatching registers (count):");
    for (idx, n) in &reg_mismatches {
        println!("   r[{idx}]: {n}");
    }
    if failed_tests == 0 {
        println!("ALL TESTS MATCH THE REAL CONSOLE");
    }
}

fn parse_reg(rest: &str) -> Option<(u8, u32)> {
    // "12] = 0xdeadbeef"
    let (idx, val) = rest.split_once("] = 0x")?;
    Some((idx.trim().parse().ok()?, u32::from_str_radix(val.trim(), 16).ok()?))
}
