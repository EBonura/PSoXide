//! The tables the tools keep by hand (record names, work counts, block flags,
//! memory-control names) must agree with the guest source they mirror.

use std::path::{Path, PathBuf};

use psoxide_hwtest::report::{
    cd_pause_rows, console_rows, fmt_g, label, list_busy_labels, record_label, work, Record,
    CD_PAUSE_META_FIRST, CD_PAUSE_TRIAL_FIRST, CD_PAUSE_WATCH_FIRST, CONSOLE_KERNEL,
    CONSOLE_KERNEL_FIRST, CONSOLE_WIDTHS, LIST_BUSY_FIRST_CASE, MEMORY_CONTROL_NAMES,
    PX7_RECORD_UNUSED, PX8_BLOCK_FAILURES, PX8_BLOCK_MEMCTL, PX8_BLOCK_OBSERVED,
    PX8_BLOCK_PRECISION, PX8_BLOCK_STATUS, PX8_BLOCK_TIMING,
};
use psoxide_hwtest::tables::{LABELS, WORK_BY_ID};
use psoxide_hwtest::v2::v2_records;
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
fn console_records_match_the_guest() {
    let source = read("console_tests.rs");
    let mut groups = Vec::new();
    for name in [
        "KERNEL",
        "WIDTH",
        "INTERLACE",
        "XA",
        "CDCOST",
        "CDHANDOFF",
        "CDMOTOR",
        "XA_MEASURE",
    ] {
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
        record(0x320, 900, 910, 1250),
        record(0x322, 15, 20, 31),
        record(0x32A, 0xFFF6, 0, 12),
        record(0x32C, 0xFFFF, 3, 0xFFFD),
        record(0x32E, 40, 5000, 3300),
    ];
    let rows = console_rows(&records);
    let has = |text: &str| assert!(rows.iter().any(|row| row == text), "missing {text:?}");
    has("console_kernel,enter_cs_net,493");
    has("console_kernel,exit_cs_net,482");
    has("console_kernel,runtime_vblank_gaps,24");
    has("console_width,256,0xD780,608,3168");
    has("console_width,320,0xFFFF,0,0");
    has("console_xa,getlocp_updating,1");
    has("console_xa,no_loop_seen,0");
    has("console_xa,loop_gap_ms,min=343 med=344 max=344");
    has("console_xa,loop_period_ms,none");
    has("console_xa_measure,start_to_sound_ms,min=900 med=910 max=1250");
    has("console_xa_measure,play_call_ms,min=1.5 med=2 max=3.1");
    has("console_xa_measure,resume_head_moved_ms,min=-10 med=0 max=12");
    has("console_xa_measure,head_minus_sysclk_ms_at_60s,none");
    has("console_xa_measure,head_minus_sysclk_ms_at_180s,-3");
    has("console_xa_measure,start_to_head_ms,missing");
    has("console_xa_measure,sysclocks_per_hblank_line,2033");
    assert!(console_rows(&[]).is_empty());
}

#[test]
fn g_format_follows_python() {
    assert_eq!(fmt_g(0.0), "0");
    assert_eq!(fmt_g(2.0), "2");
    assert_eq!(fmt_g(0.15), "0.15");
    assert_eq!(fmt_g(1234567.0), "1.23457e+06");
    assert_eq!(fmt_g(123456.0), "123456");
    assert_eq!(fmt_g(0.00001), "1e-05");
    assert_eq!(fmt_g(-3.0), "-3");
    assert_eq!(fmt_g(6553.5), "6553.5");
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

/// `/// rec name: field, field, field (...)` lines of the guest, as
/// (name, fields). Fields are compared lowercase: the guest spells some hex
/// digits in capitals.
fn guest_record_docs() -> Vec<(String, [String; 3])> {
    let docs = Regex::new(r"(?m)^\s*///\s+rec\s+(\w+):\s*([^()\n]+?)\s*(?:\(|$)").unwrap();
    docs.captures_iter(&guest_source())
        .map(|found| {
            let fields: Vec<String> = found[2]
                .split(',')
                .map(|field| field.trim().to_lowercase())
                .collect();
            let fields: [String; 3] = fields
                .try_into()
                .unwrap_or_else(|f: Vec<String>| panic!("rec {}: {} fields", &found[1], f.len()));
            (found[1].to_string(), fields)
        })
        .collect()
}

/// Whether table label `label` belongs to the guest's family `name`: the
/// guest documents `sb4_hash` once for `sb4_hash_0` to `sb4_hash_4`.
fn in_family(label: &str, name: &str) -> bool {
    label == name
        || label
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('_'))
}

#[test]
fn every_documented_guest_record_is_in_the_v2_table() {
    let docs = guest_record_docs();
    assert!(docs.len() > 90, "guest rec docs not found: {}", docs.len());
    let mut missing = Vec::new();
    for (name, fields) in &docs {
        let known = v2_records().values().any(|entry| {
            in_family(&entry.label, name)
                && entry.fields.each_ref().map(|f| f.to_lowercase()) == *fields
        });
        if !known {
            missing.push(format!("{name}: {fields:?}"));
        }
    }
    assert!(missing.is_empty(), "no v2 table entry for: {missing:#?}");
}

#[test]
fn every_v2_family_is_mentioned_by_the_guest() {
    // The guest documents some families in a module comment rather than in a
    // `/// rec` line (the MDEC probe, the rumble answers), so the check here is
    // only that the family name, with its numbering dropped, still appears.
    let source = guest_source();
    // Names the table gives to records the guest documents under a shorter
    // name: `handoff` (ten areas) and the `sb1_audit` record that 0x431 reuses
    // for the upload readback.
    let renamed = |label: &str| label.starts_with("handoff_area_") || label == "sb1_upload";
    let mut orphans = Vec::new();
    for (id, entry) in v2_records().iter().filter(|(_, e)| !renamed(&e.label)) {
        let mut parts: Vec<&str> = entry.label.split('_').collect();
        let mentioned = loop {
            if source.contains(&parts.join("_")) {
                break true;
            }
            if parts.len() <= 2 {
                break false;
            }
            parts.pop();
        };
        if !mentioned {
            orphans.push(format!("{id:03X} {}", entry.label));
        }
    }
    assert!(
        orphans.is_empty(),
        "not mentioned by the guest: {orphans:#?}"
    );
}

#[test]
fn v2_table_never_shadows_a_timing_record() {
    // A v2 entry may rename an older label's id, but it must not claim an id
    // the standing timing battery measures (those carry a work count).
    for id in v2_records().keys() {
        if let Some(work) = work(*id) {
            assert_eq!(work, 0, "v2 record {id:03X} sits on a timed record");
        }
    }
}

#[test]
fn every_known_record_fits_the_guest_record_slots() {
    let slots: usize = capture_one(
        r"const TIMING_RECORD_COUNT: usize = (\d+);",
        &read("main.rs"),
    )
    .parse()
    .unwrap();
    let mut ids: Vec<u32> = LABELS.iter().map(|(id, _)| u32::from(*id)).collect();
    ids.extend(v2_records().keys());
    ids.sort_unstable();
    ids.dedup();
    assert!(
        ids.len() <= slots,
        "{} record ids named, {slots} slots",
        ids.len()
    );
    assert!(ids.iter().all(|id| record_label(*id) != "unlabelled"));
}

#[test]
fn cd_pause_ids_match_the_guest() {
    let source = read("cd_pause.rs");
    let id = |name: &str| {
        hex(&capture_one(
            &format!(r"const {name}: u16 = (0x[0-9A-Fa-f]+);"),
            &source,
        ))
    };
    assert_eq!(id("CD_PAUSE_RECORD"), CD_PAUSE_META_FIRST);
    assert_eq!(id("META_LIMITS"), CD_PAUSE_META_FIRST + 1);
    assert_eq!(id("META_FAILS"), CD_PAUSE_META_FIRST + 2);
    assert_eq!(id("TRIAL_BASE"), CD_PAUSE_TRIAL_FIRST);
    assert_eq!(id("WATCH_BASE"), CD_PAUSE_WATCH_FIRST);
    let count = |name: &str| -> u32 {
        capture_one(&format!(r"const {name}: usize = (\d+);"), &source)
            .parse()
            .unwrap()
    };
    let (speeds, states, trials, watches, events) = (
        count("SPEEDS"),
        count("STATES"),
        count("TRIALS"),
        count("WATCHES"),
        count("WATCH_EVENTS"),
    );
    assert_eq!((speeds, states, trials, watches, events), (2, 5, 3, 4, 8));
    // The record total is three meta, four per cell and nine per watch.
    assert!(source.contains("3 + SPEEDS * STATES * (TRIALS + 1) + WATCHES * (WATCH_EVENTS + 1)"));
    let mut ids = vec![
        CD_PAUSE_META_FIRST,
        CD_PAUSE_META_FIRST + 1,
        CD_PAUSE_META_FIRST + 2,
    ];
    // Four records per cell: the trials, then the acknowledge summary.
    ids.extend((0..speeds * states * (trials + 1)).map(|k| CD_PAUSE_TRIAL_FIRST + k));
    // Sixteen ids per watch: the events, then the summary at +8.
    for watch in 0..watches {
        ids.extend((0..=events).map(|k| CD_PAUSE_WATCH_FIRST + 16 * watch + k));
    }
    for id in &ids {
        assert!(
            label(*id).is_some_and(|l| l.starts_with("cd_pause_")),
            "{id:03X}"
        );
        assert_eq!(work(*id), Some(0), "{id:03X}");
    }
    // The last id the guest can emit is the last the tables name.
    assert_eq!(ids.iter().max(), Some(&0x3B8));
    let named = LABELS
        .iter()
        .filter(|(id, name)| (0x340..0x3C0).contains(id) && name.starts_with("cd_pause_"))
        .count();
    assert_eq!(named, ids.len());
}

#[test]
fn cd_pause_state_and_event_names_match_the_guest() {
    let source = read("cd_pause.rs");
    let names = capture_one(r"const STATE_NAMES: \[&str; \w+\] = \[([^\]]*)\];", &source);
    let found: Vec<&str> = names
        .split(',')
        .map(|n| n.trim().trim_matches('"'))
        .collect();
    assert_eq!(found, ["IDLE", "SETTLED", "FIRST", "SEEK", "FAR"]);
    // Outcome and flag codes are documented on the guest's Trial and Event.
    assert!(source.contains("0 completed, 1 no INT2 in time, 2 drive error, 3 no acknowledge"));
    assert!(source.contains("1 sector, 2 complete, 3 acknowledge, 5 error, 7 gave up"));
}

/// A trial record as the guest builds it: lo, hi of the clocks, then the
/// packed outcome, sectors and status.
fn trial(id: u32, clocks: Option<u32>, outcome: u32, sectors: u32, status: u32) -> Record {
    let (lo, hi) = match clocks {
        Some(c) => (i64::from(c & 0xFFFF), i64::from(c >> 16)),
        None => (0xFFFF, 0xFFFF),
    };
    record(id, lo, hi, i64::from(outcome | sectors << 2 | status << 8))
}

#[test]
fn cd_pause_rows_unpack_trials_acks_and_watches() {
    let mut records = vec![
        record(0x340, 52, 52, 56),
        record(0x341, 7812, 1953, 34),
        record(0x342, 2, 1, 0),
    ];
    // 1x IDLE (cell 0): three completions, the second one slow.
    records.push(trial(0x350, Some(26_320), 0, 0, 0x02));
    records.push(trial(0x351, Some(70_000), 0, 3, 0x42));
    records.push(trial(0x352, Some(26_264), 0, 0, 0x02));
    records.push(record(0x353, 602, 604, 610));
    // 2x FAR (cell 9): never completes, drive error, never acknowledged.
    records.push(trial(0x374, None, 1, 15, 0x00));
    records.push(trial(0x375, None, 2, 0, 0x03));
    records.push(trial(0x376, None, 3, 0, 0x00));
    records.push(record(0x377, 0xFFFF, 0xFFFF, 0xFFFF));
    // 2x EARLY watch (watch 3): acknowledge, then INT2 after one GetStat.
    records.push(record(0x3B0, 150, 3 << 8 | 0x02, 0));
    records.push(record(0x3B1, 205, 2 << 8 | 0x02, 1));
    records.push(record(0x3B2, 0, 0, 0));
    records.push(record(0x3B8, 2, 1, 1));
    // 1x SPACED watch (watch 0) that gave up at the limit.
    records.push(record(0x380, 150, 3 << 8 | 0x02, 0));
    records.push(record(0x381, 62_500, 7 << 8, 5));
    records.push(record(0x388, 2, 5, 0));
    let rows = cd_pause_rows(&records);
    let has = |text: &str| {
        assert!(
            rows.iter().any(|row| row == text),
            "missing {text:?}\n{rows:#?}"
        )
    };

    has("cd_pause,loop_pass_clocks,min=52 med=52 max=56");
    has("cd_pause,int2_limit_clocks_about,7999488");
    has("cd_pause,int3_limit_clocks_about,1999872");
    has("cd_pause,trials_run,34");
    has("cd_pause,setup_failures,2");
    has("cd_pause,cleanup_failures,1");
    has("cd_pause_trial,1x,IDLE,0,26320,0.777,completed,0,0x02");
    has("cd_pause_trial,1x,IDLE,1,70000,2.067,completed,3,0x42");
    has("cd_pause_trial,2x,FAR,0,never,never,no INT2 within the limit,15,0x00");
    has("cd_pause_trial,2x,FAR,1,never,never,drive error,0,0x03");
    has("cd_pause_trial,2x,FAR,2,never,never,no acknowledge,0,0x00");
    has("cd_pause_ack,1x,IDLE,19264,19328,19520,0.571");
    has("cd_pause_ack,2x,FAR,none,none,none,none");
    has("cd_pause_watch,2x,EARLY,0,19200,0.567,acknowledge_int3,0x02,0");
    has("cd_pause_watch,2x,EARLY,1,26240,0.775,complete_int2,0x02,1");
    has("cd_pause_watch,1x,SPACED,1,8000000,236.206,gave_up_at_limit,0x00,5");
    has("cd_pause_watch_summary,2x,EARLY,2,1,yes");
    has("cd_pause_watch_summary,1x,SPACED,2,5,no");
    // Unused event slots and cells without records print nothing.
    assert!(!rows
        .iter()
        .any(|row| row.starts_with("cd_pause_watch,2x,EARLY,2,")));
    assert!(!rows
        .iter()
        .any(|row| row.starts_with("cd_pause_trial,1x,SETTLED")));
    assert!(cd_pause_rows(&[]).is_empty());
}
