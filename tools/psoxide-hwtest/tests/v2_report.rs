//! The v2 record rows and verdicts, the run id on the capture pages, and the
//! small commands (silence check, emulator baseline, silicon compare) on
//! hand-built input.

use psoxide_hwtest::compare::{
    check_silence, compare_to_silicon, SiliconReference, EXACT_RECORDS, REDEFINED_IN_V2,
};
use psoxide_hwtest::report::{
    parse_capture_page, record_label, run_id_text, Capture, Record, PX8_BLOCK_STATUS,
};
use psoxide_hwtest::util::crc32;
use psoxide_hwtest::v2::{v2_records, v2_rows, v2_verdicts};
use psoxide_hwtest::v24::{describe_field, field_rule_expected, uv_rows_expected};

fn record(id: u32, low: i64, mid: i64, high: i64) -> Record {
    Record {
        record_id: id,
        work: 0,
        minimum: low,
        maximum: high,
        median: mid,
    }
}

fn capture(major: u8, records: Vec<Record>) -> Capture {
    Capture {
        schema: "PX8".into(),
        version: 4,
        run_id: Some(0xFC98),
        flags: PX8_BLOCK_STATUS,
        failures: Vec::new(),
        page_count: 1,
        suite_major: major,
        suite_minor: 7,
        conformance_run: 0x98,
        timing_run: 0xFC,
        conformance_digest: 0,
        gte_digest: 0,
        timing_digest: 0,
        timing_aux: 0,
        scans: Vec::new(),
        observations: Vec::new(),
        statuses: Vec::new(),
        records,
        memory_control: Vec::new(),
        precision: Vec::new(),
        binary_crc: 0,
    }
}

/// Ten clean handoffs, no lost records, a silent end.
fn clean_run() -> Vec<Record> {
    let mut records: Vec<Record> = (0..10).map(|a| record(0x410 + a, 0x3F, 0x1FF, 0)).collect();
    records.push(record(0x41E, 0, 0, 0));
    records.push(record(0x41A, 0x7F, 0, 0));
    records
}

fn page(field: &str, chunk: &str) -> String {
    format!("PX8/{field}/{chunk}/C:{:08X}", crc32(chunk.as_bytes()))
}

#[test]
fn page_field_may_carry_the_run_id() {
    let with_run = parse_capture_page(&page("0105FC98", "AAAA")).unwrap();
    assert_eq!(
        (with_run.number, with_run.total, with_run.run_id),
        (1, 5, Some(0xFC98))
    );
    let without = parse_capture_page(&page("0105", "AAAA")).unwrap();
    assert_eq!(
        (without.number, without.total, without.run_id),
        (1, 5, None)
    );
    assert!(parse_capture_page(&page("0105FC", "AAAA")).is_err());
}

#[test]
fn run_id_is_both_run_counters_from_suite_two() {
    assert_eq!(run_id_text(&capture(2, Vec::new())), "FC98");
    assert_eq!(run_id_text(&capture(1, Vec::new())), "FC");
}

#[test]
fn v2_table_expands_the_loops() {
    let table = v2_records();
    assert_eq!(table[&0x41E].label, "record_integrity");
    assert_eq!(table[&0x415].label, "handoff_area_5");
    assert_eq!(table[&0x4A9].label, "handoff_stage_safe2_1");
    assert_eq!(table[&0x620].label, "sio_pad_ack_p2_b0");
    assert_eq!(table[&0x6C8].label, "sio_card_ack_p2_b0");
    assert_eq!(table[&0x6B3].label, "tick_loss_t0_sysclk_14_instr");
    assert_eq!(table[&0x797].label, "dma_matrix_state_cd_2x_2340_sw");
    assert_eq!(table[&0x862].label, "list_abort_1");
    assert_eq!(table[&0x844].label, "field_rule_cost_1");
    assert_eq!(table[&0x8E1].label, "uv_window_row_0");
    assert_eq!(table[&0x8E1].fields[0], "depth_and_window");
    assert_eq!(
        table[&0x773].fields[2],
        "questions_with_buttons_never_released_mask"
    );
    assert_eq!(table[&0x775].fields[2], "answer_frame_stimulus_5");
    // The second pad's pass repeats 0x760-0x776 at +0x1E0.
    assert_eq!(table[&0x940].label, "rumble_config_p1_pass2");
    assert_eq!(table[&0x956].label, "rumble_answer_frames_c_pass2");
    assert_eq!(table[&0x709].label, "mdec_dma_loop_sw");
    assert_eq!(table[&0x705].label, "cd_dma_loop_2x_2048_sw");
    assert_eq!(table[&0xA18].label, "clut_rewrite_24");
}

#[test]
fn record_label_prefers_the_v2_table() {
    assert_eq!(record_label(0x41E), "record_integrity");
    assert_eq!(record_label(0x00), "timer2_empty_harness");
    assert_eq!(record_label(0x350), "cd_pause_1x_idle_trial0_int2");
    assert_eq!(record_label(0xFFF), "unlabelled");
}

#[test]
fn v2_rows_name_every_field() {
    let rows = v2_rows(&capture(
        2,
        vec![record(0x41B, 0, 69, 1172), record(0x350, 1, 2, 3)],
    ));
    assert_eq!(
        rows,
        [
            "v2,record,field,value",
            "v2,41B_run_info,skipped_risky,0",
            "v2,41B_run_info,steps,69",
            "v2,41B_run_info,records_taken,1172",
        ]
    );
    assert!(v2_rows(&capture(2, Vec::new())).is_empty());
}

#[test]
fn v2_verdicts_are_the_runs_own_pass_criteria() {
    assert!(v2_verdicts(&capture(2, clean_run())).is_empty());
    // A suite before v2.0 has no such criteria.
    assert!(v2_verdicts(&capture(1, Vec::new())).is_empty());

    let mut records = clean_run();
    records[3] = record(0x413, 0x2F, 0, 0x0101);
    records[10] = record(0x41E, 1, 2, 68);
    records[11] = record(0x41A, 0x3F, 1200, 3);
    let problems = v2_verdicts(&capture(2, records));
    assert_eq!(
        problems,
        [
            "area 3: handoff flags 0x2F (clean is 0x3F), busy/active 0x0101",
            "record integrity: 1 duplicate ids, 2 clamped fields, 68 dropped records",
            "final silence flags 0x3F (silent is 0x7F), capture peak 1200, voices with envelope 3",
        ]
    );

    let problems = v2_verdicts(&capture(2, Vec::new()));
    assert_eq!(problems.len(), 11);
    assert_eq!(problems[0], "area 0: no handoff record");
    assert_eq!(problems[10], "no final silence record");
}

#[test]
fn silicon_compare_names_regressions_and_tolerates_timing_noise() {
    let mut reference = SiliconReference {
        cases: Default::default(),
        records: Default::default(),
        precision: vec![1, 2],
        sources: Vec::new(),
    };
    reference.cases.insert(0, (1, 5));
    reference.cases.insert(1, (2, 6));
    reference.cases.insert(2, (1, 7));
    reference.records.insert(0x10, record(0x10, 100, 100, 100));
    reference.records.insert(0x11, record(0x11, 100, 100, 100));
    reference.records.insert(0x2C4, record(0x2C4, 1, 2, 3));
    reference.records.insert(0x2D0, record(0x2D0, 1, 2, 3));

    let mut run = capture(
        2,
        vec![
            record(0x10, 109, 0, 0),
            record(0x11, 120, 0, 0),
            record(0x2C4, 1, 2, 4),
            record(0x2D0, 9, 9, 9),
        ],
    );
    run.records.extend(clean_run());
    run.statuses = vec![2, 1, 1];
    run.observations = vec![5, 6, 8];
    run.precision = vec![1, 3];
    let result = compare_to_silicon(&run, &reference);
    assert_eq!(
        result.regressions,
        ["case 0: FAIL (passed on silicon), observed 0x00000005"]
    );
    assert_eq!(result.improvements, ["case 1: passes (silicon FAIL)"]);
    let precision = psoxide_hwtest::report::precision_label(1).unwrap();
    assert_eq!(
        result.mismatches,
        [
            "case 2: 0x00000008, silicon 0x00000007 (PASS vs PASS)".to_string(),
            format!(
                "record 011 {}: min/med/max 120/0/0, silicon 100/100/100",
                record_label(0x11)
            ),
            format!(
                "record 2C4 {}: min/med/max 1/2/4, silicon 1/2/3",
                record_label(0x2C4)
            ),
            format!("precision 001 {precision}: 0x00000003, silicon 0x00000002"),
        ]
    );
    assert_eq!(
        (
            result.shared_cases,
            result.shared_records,
            result.shared_precision
        ),
        (3, 3, 2)
    );
    assert!(REDEFINED_IN_V2.contains(&0x2D3));
    assert!(EXACT_RECORDS.contains(&0x2C4));
}

#[test]
fn field_rule_descriptors_cover_the_table() {
    let expected = field_rule_expected();
    assert_eq!(expected.len(), 83);
    assert_eq!(
        describe_field(expected[0]),
        "fill bit10=0 field=0 480i inside plain"
    );
    // The last four rows are the display-blanked variant.
    assert!(describe_field(expected[82]).ends_with("display_blanked"));
    assert_eq!(uv_rows_expected().len(), 38);
    assert_eq!(uv_rows_expected()[18], (0, 32, 8));
}

/// A 16-bit PCM WAV of `seconds` at 1000 Hz, mono, with a burst of non-zero
/// samples from `loud_from` to `loud_to` seconds.
fn wav(path: &std::path::Path, seconds: u32, loud_from: u32, loud_to: u32) {
    let rate = 1000u32;
    let samples: Vec<i16> = (0..seconds * rate)
        .map(|n| {
            if (loud_from * rate..loud_to * rate).contains(&n) {
                1000
            } else {
                0
            }
        })
        .collect();
    let data_len = samples.len() as u32 * 2;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    for sample in samples {
        bytes.extend(sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn silence_check_measures_the_tail() {
    let dir = std::env::temp_dir().join(format!("psoxide-hwtest-silence-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tail.wav");
    wav(&path, 10, 2, 4);

    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(check_silence(&path, 3.0, &mut out, &mut err).unwrap(), 0);
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("# audio: 10.0 s, 1 non-silent span(s)"),
        "{text}"
    );
    assert!(text.contains("# sound 2.0-4.0 s"), "{text}");
    assert!(
        text.contains("# silent tail: 6.0 s (needs 3.0 s: the capture pages)"),
        "{text}"
    );
    assert!(text.ends_with("silent at the capture pages\n"));
    assert!(err.is_empty());

    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(check_silence(&path, 7.0, &mut out, &mut err).unwrap(), 1);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "FAIL: sound continues until 4.0 s of 10.0 s\n"
    );

    // Sound to the very end is a failure at any tail.
    wav(&path, 4, 1, 4);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(check_silence(&path, 0.5, &mut out, &mut err).unwrap(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
