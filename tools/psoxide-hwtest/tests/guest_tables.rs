//! The tables the tools keep by hand (record names, work counts, block flags,
//! memory-control names) must agree with the guest source they mirror.

use std::path::{Path, PathBuf};

use psoxide_hwtest::report::{
    console_rows, fmv_rows, label, list_busy_labels, work, Record, CONSOLE_KERNEL,
    CONSOLE_KERNEL_FIRST, CONSOLE_WIDTHS, FMV_FIELDS, FMV_FIRST_RECORD, FMV_SETUP_ERRORS,
    LIST_BUSY_FIRST_CASE, MEMORY_CONTROL_NAMES, PX7_RECORD_UNUSED, PX8_BLOCK_FAILURES,
    PX8_BLOCK_MEMCTL, PX8_BLOCK_OBSERVED, PX8_BLOCK_PRECISION, PX8_BLOCK_STATUS, PX8_BLOCK_TIMING,
};
use psoxide_hwtest::tables::{LABELS, WORK_BY_ID};
use regex::Regex;

fn guest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../engine/examples/hardware-tests/src")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(guest_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn guest_source() -> String {
    let mut names: Vec<_> = std::fs::read_dir(guest_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    names.sort();
    names
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn hex(text: &str) -> u32 {
    u32::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap()
}

fn int(text: &str) -> u32 {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) => u32::from_str_radix(digits, 16).unwrap(),
        None => text.parse().unwrap(),
    }
}

fn capture_one(pattern: &str, source: &str) -> String {
    Regex::new(pattern)
        .unwrap()
        .captures(source)
        .unwrap_or_else(|| panic!("no match for {pattern}"))[1]
        .to_string()
}

fn record(id: u32, low: i64, mid: i64, high: i64) -> Record {
    Record {
        record_id: id,
        work: 0,
        minimum: low,
        maximum: high,
        median: mid,
    }
}

#[test]
fn labels_and_work_cover_the_same_ids() {
    let labels: Vec<u16> = LABELS.iter().map(|(id, _)| *id).collect();
    let works: Vec<u16> = WORK_BY_ID.iter().map(|(id, _)| *id).collect();
    assert_eq!(labels, works);
    assert!(label(PX7_RECORD_UNUSED).is_none());
}

#[test]
fn literal_guest_records_match_the_host_tables() {
    let source = guest_source();
    let calls =
        Regex::new(r"sample_timing\(\s*(0x[0-9A-Fa-f]{2,3}),\s*(\d+|0x[0-9A-Fa-f]+),").unwrap();
    let literal: Vec<_> = calls.captures_iter(&source).collect();
    assert!(literal.len() > 100, "guest timing call sites not found");
    for found in literal {
        let (id, expected) = (hex(&found[1]), int(&found[2]));
        assert!(label(id).is_some(), "record {id:02X} has no label");
        assert_eq!(work(id), Some(expected), "record {id:02X}");
    }
}

#[test]
fn perf_probe_table_matches_the_host_tables() {
    // perf_probes.rs drives its records from one table of `probe(0xID, work,
    // ...)` rows instead of literal call sites.
    let rows = Regex::new(r"\b(?:ab_probe|probe|case)\(\s*(0x[0-9A-Fa-f]{2,3}),\s*(\d+),").unwrap();
    let source = guest_source();
    let found: Vec<_> = rows.captures_iter(&source).collect();
    assert!(found.len() > 40, "perf probe table not found");
    for row in found {
        let (id, expected) = (hex(&row[1]), int(&row[2]));
        assert!(label(id).is_some(), "record {id:02X} has no label");
        assert_eq!(work(id), Some(expected), "record {id:02X}");
    }
}

#[test]
fn record_slots_hold_the_largest_scope() {
    let source = guest_source();
    let slots: u32 = capture_one(r"const TIMING_RECORD_COUNT: usize = (\d+);", &source)
        .parse()
        .unwrap();
    let table: Vec<u32> = ["SAFE", "LEVERS", "EXTENDED", "SHAPES", "RISKY", "CASES"]
        .iter()
        .map(|name| {
            capture_one(&format!(r"const {name}: \[\w+; (\d+)\]"), &source)
                .parse()
                .unwrap()
        })
        .collect();
    let dma_pairs = 6;
    let retired = LABELS
        .iter()
        .filter(|(_, name)| name.starts_with("v122_only_"))
        .count() as u32;
    // The FMV STREAM TEST's records join any scope once it has run.
    let fmv = FMV_FIELDS.len() as u32;
    // The v1.26 MDEC DIAGNOSTIC's records replace an earlier battery's slots
    // when it runs, so they are not part of the standing battery.
    let mdec = LABELS
        .iter()
        .filter(|(id, _)| (0x200..0x2B0).contains(id))
        .count() as u32;
    // The v1.27 console cases' records likewise join a capture only once run.
    let console = LABELS
        .iter()
        .filter(|(id, _)| (0x2C0..0x2F0).contains(id))
        .count() as u32;
    // What is left is the standing battery, which has not changed size.
    let tables: u32 = table.iter().sum();
    let standing = LABELS.len() as u32 - tables - dma_pairs - retired - fmv - mdec - console;
    assert_eq!(standing, 151);
    // The standard scope takes the standing battery, SAFE and LEVERS.
    assert!(standing + table[0] + table[1] + fmv <= slots);
    assert!(tables + dma_pairs + fmv <= slots);
}

#[test]
fn console_records_match_the_guest() {
    let source = read("console_tests.rs");
    let mut groups = Vec::new();
    for name in ["KERNEL", "WIDTH", "INTERLACE", "XA"] {
        let first = hex(&capture_one(
            &format!(r"const {name}_RECORD: u16 = (0x[0-9A-Fa-f]+);"),
            &source,
        ));
        let count: u32 = capture_one(&format!(r"const {name}_COUNT: usize = (\d+);"), &source)
            .parse()
            .unwrap();
        groups.push((first, count));
        for id in first..first + count {
            assert!(label(id).is_some(), "record {id:03X}");
            assert_eq!(work(id), Some(0), "record {id:03X}");
        }
    }
    assert_eq!(
        groups[0],
        (CONSOLE_KERNEL_FIRST, CONSOLE_KERNEL.len() as u32)
    );
    assert_eq!(groups[1].1, CONSOLE_WIDTHS.len() as u32);
    // The slots the guest reserves are exactly the records it can emit.
    let slots = capture_one(r"const RECORD_SLOTS: usize = ([^;]+);", &source)
        .matches("_COUNT")
        .count();
    assert_eq!(slots, groups.len());
}

#[test]
fn console_rows_unpack_each_case() {
    let records = vec![
        record(0x2C0, 492, 498, 538),
        record(0x2C1, 481, 487, 505),
        record(0x2C2, 5, 5, 14),
        record(0x2C3, 981, 993, 1112),
        record(0x2C4, 24, 24, 1 | 2 | 4 | (24 << 8)),
        record(0x2C5, 90, 90, 103),
        record(0x2D0, 0xD780, 608, 3168),
        record(0x2D1, 0xFFFF, 0, 0),
        record(0x2E0, 0b11111, 6, 45),
        record(0x2E1, 343, 344, 344),
        record(0x2E2, 0xFFFF, 0xFFFF, 0xFFFF),
        record(0x2E3, 376, 2918, 13),
    ];
    let rows = console_rows(&records);
    let has = |text: &str| assert!(rows.iter().any(|row| row == text), "missing {text:?}");
    has("console_kernel,enter_cs_net,493");
    has("console_kernel,exit_cs_net,482");
    has("console_kernel,runtime_vblank_gaps,24");
    has("console_width,256,0xD780,608,3168");
    has("console_width,320,not shown");
    has("console_xa,getlocp_updating,1");
    has("console_xa,no_loop_seen,0");
    has("console_xa,loop_gap_ms,min=343 med=344 max=344");
    has("console_xa,loop_period_ms,none");
    assert!(console_rows(&[]).is_empty());
}

#[test]
fn fmv_records_match_the_guest() {
    let source = read("fmv_test.rs");
    let first = hex(&capture_one(
        r"const FIRST_RECORD: u16 = (0x[0-9A-Fa-f]+);",
        &source,
    ));
    let count: u32 = capture_one(r"const RECORD_COUNT: usize = (\d+);", &source)
        .parse()
        .unwrap();
    assert_eq!(first, FMV_FIRST_RECORD);
    assert_eq!(count as usize, FMV_FIELDS.len());
    for id in first..first + count {
        assert!(label(id).is_some(), "record {id:03X}");
    }
    let errors = Regex::new(r"(?s)const SETUP_ERRORS: \[&str; \d+\] = \[(.*?)\];")
        .unwrap()
        .captures(&source)
        .expect("SETUP_ERRORS")[1]
        .to_string();
    let names: Vec<String> = Regex::new(r#""([^"]+)""#)
        .unwrap()
        .captures_iter(&errors)
        .map(|c| c[1].to_string())
        .collect();
    assert_eq!(names, FMV_SETUP_ERRORS[1..]);
}

#[test]
fn fmv_rows_rederive_the_verdict() {
    let mut fields = [
        (1, 9826, 9826),
        (0, 0, 0),
        (0, 0, 0xFFFF),
        (889, 234, 4450),
        (40, 30, 20),
        (12253, 1, 0),
    ];
    let build = |fields: &[(i64, i64, i64)]| -> Vec<Record> {
        fields
            .iter()
            .enumerate()
            .map(|(index, (low, mid, high))| {
                record(FMV_FIRST_RECORD + index as u32, *low, *mid, *high)
            })
            .collect()
    };
    let rows = fmv_rows(&build(&fields));
    assert_eq!(rows[0], "# fmv=PASS criteria=PASS");
    assert!(rows.contains(&"fmv,first_error_lba,none".to_string()));
    assert!(rows.contains(&"fmv,setup_error,none".to_string()));
    // One lost sector fails the criteria even if the guest said PASS.
    fields[1] = (1, 0, 0);
    let rows = fmv_rows(&build(&fields));
    assert_eq!(rows[0], "# fmv=PASS criteria=FAIL");
    assert!(rows.contains(&"# fmv verdict disagrees with its own counters".to_string()));
    assert!(fmv_rows(&[]).is_empty());
}

#[test]
fn no_label_claims_an_unused_slot_marker() {
    assert!(label(0xFF).is_none());
    assert!(LABELS.iter().all(|(id, _)| *id != 0xFFFF));
}

#[test]
fn list_busy_labels_match_the_battery() {
    // Cases are named by index on the host, so the labels must start where the
    // guest's list_busy_probes entries start and cover all of them.
    let source = read("main.rs");
    let battery = &source[source
        .find("const TESTS: [TestSpec; TEST_COUNT]")
        .expect("TESTS table")..];
    let runs = Regex::new(r"run: ([\w:]+),").unwrap();
    let busy: Vec<usize> = runs
        .captures_iter(battery)
        .enumerate()
        .filter(|(_, c)| c[1].starts_with("list_busy_probes::"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(busy[0], LIST_BUSY_FIRST_CASE);
    assert_eq!(busy.len(), list_busy_labels().len());
    assert_eq!(busy, (busy[0]..busy[0] + busy.len()).collect::<Vec<_>>());
}

#[test]
fn block_flags_match_photo_rs() {
    let photo = read("photo.rs");
    for (name, flag) in [
        ("STATUS", PX8_BLOCK_STATUS),
        ("FAILURES", PX8_BLOCK_FAILURES),
        ("OBSERVED", PX8_BLOCK_OBSERVED),
        ("TIMING", PX8_BLOCK_TIMING),
        ("MEMCTL", PX8_BLOCK_MEMCTL),
        ("PRECISION", PX8_BLOCK_PRECISION),
    ] {
        let shift: u32 = capture_one(&format!(r"pub const {name}: u8 = 1 << (\d+);"), &photo)
            .parse()
            .unwrap();
        assert_eq!(flag, 1 << shift, "{name}");
    }
}

#[test]
fn every_memory_control_value_has_a_name() {
    let count: usize = capture_one(
        r"const MEMORY_CONTROL_REGISTERS: \[u32; (\d+)\]",
        &guest_source(),
    )
    .parse()
    .unwrap();
    assert!(count <= MEMORY_CONTROL_NAMES.len());
}
