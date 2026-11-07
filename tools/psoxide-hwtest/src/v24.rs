//! The v2.4 measurements, one table per new measurement, with the sanity checks
//! that say whether the harness (not the machine) did what it meant to. A
//! failed check is a harness fault; a surprising value that passes every check
//! is a measurement.

use std::collections::HashMap;
use std::io::Write;

use crate::report::{label, parse_capture, payloads_from_paths, Capture, Record};
use crate::util::{Error, Result};
use crate::v2::{
    v2_entry, FIELD_RULE_FIRST, FIELD_RULE_TIMED_FROM, FIELD_RULE_TIMED_UNTIL, UV_WINDOW_FIRST,
};

const FIELD_CMDS: [&str; 8] = [
    "fill",
    "rect",
    "tris",
    "texquad",
    "line",
    "copy",
    "upload",
    "texquad_tpage10",
];
const FIELD_MODES: [&str; 4] = ["480i", "480p", "240i", "480i_24bpp"];
const FIELD_VARIANTS: [&str; 8] = [
    "plain",
    "clip_top_odd",
    "offset_odd",
    "start_y_odd",
    "at_vblank",
    "cost_fill",
    "cost_rect",
    "display_blanked",
];

/// The descriptor of each field-rule record, in id order, as the guest builds
/// the table (src/field_rule.rs).
pub fn field_rule_expected() -> Vec<i64> {
    let d = |cmd: i64, bit10: i64, field: i64, mode: i64, outside: i64, variant: i64| -> i64 {
        let field_bits = match field {
            0 => 0,
            1 => 1,
            _ => 3,
        };
        cmd | bit10 << 4 | field_bits << 5 | mode << 7 | outside << 9 | variant << 10
    };
    let mut rows = Vec::new();
    for bit10 in 0..2 {
        for field in 0..2 {
            for cmd in 0..7 {
                rows.push(d(cmd, bit10, field, 0, 0, 0));
            }
        }
    }
    for field in 0..2 {
        for cmd in 0..3 {
            rows.push(d(cmd, 0, field, 0, 1, 0));
        }
    }
    for mode in 1..4 {
        for cmd in 0..7 {
            rows.push(d(cmd, 0, 2, mode, 0, 0));
        }
    }
    for field in 0..2 {
        rows.push(d(7, 0, field, 0, 0, 0));
    }
    for variant in 1..3 {
        for field in 0..2 {
            rows.push(d(1, 0, field, 0, 0, variant));
        }
    }
    for field in 0..2 {
        rows.push(d(0, 0, field, 0, 0, 3));
    }
    for _ in 0..4 {
        rows.push(d(0, 0, 2, 0, 0, 4));
    }
    for (cmd, variant) in [(0, 5), (1, 6)] {
        for bit10 in 0..2 {
            for field in 0..2 {
                rows.push(d(cmd, bit10, field, 0, 0, variant));
            }
        }
    }
    for field in 0..2 {
        for cmd in [0, 1, 2, 4] {
            rows.push(d(cmd, 0, field, 0, 0, 7));
        }
    }
    rows
}

pub fn describe_field(descriptor: i64) -> String {
    let cmd = FIELD_CMDS.get((descriptor & 15) as usize).unwrap_or(&"?");
    let bit10 = (descriptor >> 4) & 1;
    let field = match (descriptor >> 5) & 3 {
        0 => "0",
        1 => "1",
        3 => "any",
        _ => "?",
    };
    let mode = FIELD_MODES[((descriptor >> 7) & 3) as usize];
    let place = if (descriptor >> 9) & 1 != 0 {
        "outside"
    } else {
        "inside"
    };
    let variant = FIELD_VARIANTS[((descriptor >> 10) & 7) as usize];
    format!("{cmd} bit10={bit10} field={field} {mode} {place} {variant}")
}

/// (depth_and_window, span, origin_u_and_v) per uv-window row, as the guest
/// builds them (src/workload.rs).
pub fn uv_rows_expected() -> Vec<(i64, i64, i64)> {
    let mut rows = Vec::new();
    for depth in 0..3 {
        for span in [8, 16, 32, 64, 128, 255] {
            rows.push((depth, span, 0));
        }
    }
    for depth in 0..3 {
        for (u, v) in [(8, 0), (0, 8), (100, 100), (223, 223)] {
            rows.push((depth, 32, u | v << 8));
        }
    }
    for depth in [0, 2] {
        for window in [1, 2, 4, 8] {
            rows.push((depth | window << 4, 63, 0));
        }
    }
    rows
}

type Triple = (i64, i64, i64);

struct Records<'a> {
    by_id: HashMap<u32, &'a Record>,
    faults: Vec<String>,
}

impl Records<'_> {
    /// The record's three fields; a missing record is a fault.
    fn triple(&mut self, id: u32) -> Option<Triple> {
        match self.by_id.get(&id) {
            Some(record) => Some((record.minimum, record.median, record.maximum)),
            None => {
                self.faults.push(format!("record {id:03X} missing"));
                None
            }
        }
    }
}

/// Print the v2.4 measurements and check them. Returns the number of harness
/// faults found.
pub fn v24_report(capture: &Capture, out: &mut dyn Write) -> Result<usize> {
    let mut rec = Records {
        by_id: capture
            .records
            .iter()
            .map(|record| (record.record_id, record))
            .collect(),
        faults: Vec::new(),
    };
    macro_rules! say {
        ($($arg:tt)*) => {
            writeln!(out, $($arg)*)?
        };
    }

    let mut ids: Vec<u32> = capture.records.iter().map(|r| r.record_id).collect();
    ids.sort_unstable();
    let mut distinct = ids.clone();
    distinct.dedup();
    for id in distinct {
        let times = ids.iter().filter(|&&other| other == id).count();
        if times > 1 {
            rec.faults
                .push(format!("record {id:03X} appears {times} times"));
        }
        if v2_entry(id).is_none() && label(id).is_none() && id >= 0x400 {
            rec.faults
                .push(format!("record {id:03X} has no name in the table"));
        }
    }

    say!("v24,integrity");
    if let Some(t) = rec.triple(0x41E) {
        say!(
            "v24,record_integrity,duplicates={},clamped_fields={},dropped={}",
            t.0,
            t.1,
            t.2
        );
        if t.0 != 0 || t.1 != 0 || t.2 != 0 {
            rec.faults
                .push(format!("record integrity ({}, {}, {})", t.0, t.1, t.2));
        }
    }

    // ---- the 480i draw rule
    say!("v24,field_rule,id,cmd,bit10,field_asked,mode,place,variant,row_mask,flags");
    let expected = field_rule_expected();
    for (k, &want) in expected.iter().enumerate() {
        let record_id = FIELD_RULE_FIRST + k as u32;
        let Some((mask, descriptor, flags)) = rec.triple(record_id) else {
            continue;
        };
        if descriptor != want {
            rec.faults.push(format!(
                "field rule {record_id:03X}: descriptor 0x{descriptor:04X}, expected 0x{want:04X}"
            ));
        }
        let timed = (FIELD_RULE_TIMED_FROM..FIELD_RULE_TIMED_UNTIL).contains(&(k as u32));
        let shown = if timed {
            format!("clocks_div8={mask}")
        } else {
            format!("mask=0x{mask:04X}")
        };
        let (s1, s2) = (flags & 1, (flags >> 1) & 1);
        let attempts = ((flags >> 2) & 7) + 1;
        let (unclean, undrained, unmatched) =
            ((flags >> 5) & 1, (flags >> 6) & 1, (flags >> 7) & 1);
        say!(
            "v24,field_rule,{record_id:03X},{},{shown},field {s1}->{s2} attempts {attempts} bit19={} bit22={} bit21={} bit23={}{}{}{}",
            describe_field(descriptor).replace(' ', ","),
            (flags >> 8) & 1,
            (flags >> 9) & 1,
            (flags >> 10) & 1,
            (flags >> 11) & 1,
            if unclean != 0 { " UNCLEAN" } else { "" },
            if undrained != 0 { " UNDRAINED" } else { "" },
            if unmatched != 0 { " FIELD_NEVER_MATCHED" } else { "" },
        );
        if unclean != 0 || undrained != 0 {
            rec.faults.push(format!(
                "field rule {record_id:03X}: unclean={unclean} undrained={undrained}"
            ));
        }
        if !timed && (descriptor & 15) == 6 && mask != 0xFFFF {
            rec.faults.push(format!(
                "field rule {record_id:03X}: the upload control left mask 0x{mask:04X}"
            ));
        }
        if s1 != s2 && !timed {
            rec.faults.push(format!(
                "field rule {record_id:03X}: the field flipped under the command"
            ));
        }
    }
    // The table people will read: mask by command, bit 10 and field asked.
    let mut grid: HashMap<(i64, i64, i64), i64> = HashMap::new();
    for (k, &want) in expected.iter().take(28).enumerate() {
        if let Some(record) = rec.by_id.get(&(FIELD_RULE_FIRST + k as u32)) {
            grid.insert(
                (want & 15, (want >> 4) & 1, (want >> 5) & 3),
                record.minimum,
            );
        }
    }
    say!("v24,field_rule_table,cmd,bit10=0 field0,bit10=0 field1,bit10=1 field0,bit10=1 field1");
    for (cmd, name) in FIELD_CMDS.iter().enumerate().take(7) {
        let cells: Vec<String> = [(0, 0), (0, 1), (1, 0), (1, 1)]
            .iter()
            .map(|&(b, f)| match grid.get(&(cmd as i64, b, f)) {
                Some(value) => format!("0x{value:04X}"),
                None => "-".to_string(),
            })
            .collect();
        say!("v24,field_rule_table,{name},{}", cells.join(","));
    }

    // ---- stop mid-node, block FIFO
    say!("v24,list_abort,run,delay_clocks,node_words_landed,next_node_words_landed,flags,madr_words_at_stop,madr_words_after");
    let delays: [Option<i64>; 4] = [None, Some(6000), Some(20000), Some(60000)];
    for k in 0..4u32 {
        let (a, b) = (rec.triple(0x860 + 2 * k), rec.triple(0x861 + 2 * k));
        let (Some(a), Some(b)) = (a, b) else {
            continue;
        };
        let flags = a.2;
        let delay = (flags >> 8) * 256;
        say!(
            "v24,list_abort,{k},{}(~{delay}),{},{},0x{:02X} (A prefix {}, B prefix {}, busy at stop {}, busy after {}, gpu idle {}, dirty {}, readfail {}),{},{}",
            delays[k as usize].map_or("none".to_string(), |d| d.to_string()),
            a.0,
            a.1,
            flags & 0xFF,
            flags & 1,
            (flags >> 1) & 1,
            (flags >> 2) & 1,
            (flags >> 3) & 1,
            (flags >> 4) & 1,
            (flags >> 5) & 1,
            (flags >> 6) & 1,
            b.0,
            b.1
        );
        if (flags >> 5) & 3 != 0 {
            rec.faults.push(format!(
                "list abort {k}: destination dirty or read failed (flags 0x{flags:04X})"
            ));
        }
        if k == 0 && (a.0 != 48 || a.1 != 8) {
            rec.faults.push(format!(
                "list abort control landed {}/48 and {}/8 words: the list does not complete",
                a.0, a.1
            ));
        }
    }
    say!("v24,block_fifo,run,words_sent,block,heavy,words_landed,flags,clocks,gpustat_hi");
    let runs: [(i64, i64, i64); 6] = [
        (64, 16, 0),
        (64, 16, 1),
        (256, 16, 0),
        (256, 16, 1),
        (64, 8, 0),
        (64, 8, 1),
    ];
    for k in 0..6u32 {
        let (a, b) = (rec.triple(0x868 + 2 * k), rec.triple(0x869 + 2 * k));
        let (Some(a), Some(b)) = (a, b) else {
            continue;
        };
        let (words, block, heavy) = runs[k as usize];
        let flags = a.2;
        let clocks = b.0 | b.1 << 16;
        say!(
            "v24,block_fifo,{k},{words},{block},{heavy},{}/{},prefix={} busy_at_bound={} idle={},{clocks},0x{:04X}",
            a.0,
            a.1,
            flags & 1,
            (flags >> 1) & 1,
            (flags >> 2) & 1,
            b.2
        );
        if a.1 != words - 3 || (flags >> 3) & 1 != heavy || ((flags >> 4) & 15) * 4 != block {
            rec.faults.push(format!(
                "block fifo {k}: record does not describe run {:?}: {a:?}",
                runs[k as usize]
            ));
        }
    }

    // ---- memory card protocol
    for port in 0..2u32 {
        let base = 0x880 + 0x20 * port;
        let Some(total) = rec.triple(base + 0x0E) else {
            continue;
        };
        let flags = total.2;
        let empty = (flags >> 2) & 1;
        let p = port + 1;
        say!(
            "v24,card_proto_p{p},empty={empty},good={},checksum={},terminator_acked={},flag_byte=0x{:02X},good_reads={},span_clocks={}",
            flags & 1,
            (flags >> 1) & 1,
            (flags >> 3) & 1,
            (flags >> 4) & 0xFF,
            (flags >> 12) & 7,
            total.0 | total.1 << 16
        );
        if empty == 0 {
            for i in 0..10 {
                if let Some(h) = rec.triple(base + i) {
                    say!(
                        "v24,card_proto_p{p}_head_b{i},rise={},width={},done={}",
                        h.0,
                        h.1,
                        h.2
                    );
                }
            }
            for (name, offset) in [("data_rise", 0x0A), ("data_width", 0x0B)] {
                if let Some(h) = rec.triple(base + offset) {
                    say!(
                        "v24,card_proto_p{p}_{name},min={},med={},max={}",
                        h.0,
                        h.1,
                        h.2
                    );
                }
            }
            for (name, offset) in [("checksum", 0x0C), ("terminator", 0x0D)] {
                if let Some(h) = rec.triple(base + offset) {
                    say!(
                        "v24,card_proto_p{p}_{name},rise={},width={},done={}",
                        h.0,
                        h.1,
                        h.2
                    );
                }
            }
            if let Some(w) = rec.triple(base + 0x1D) {
                let reasons: Vec<&str> = [
                    (1, "not_asked"),
                    (2, "slot_empty_or_read_not_good"),
                    (3, "checksum_mismatch"),
                    (4, "card_error_flag"),
                    (5, "reads_differ"),
                ]
                .iter()
                .filter(|(bit, _)| (w.2 >> bit) & 1 != 0)
                .map(|(_, name)| *name)
                .collect();
                say!(
                    "v24,card_proto_p{p}_write_total,span={},flags=0x{:04X},status_byte=0x{:02X},written={},not_written_because={}",
                    w.0 | w.1 << 16,
                    w.2,
                    w.2 >> 8,
                    if reasons.is_empty() { "yes" } else { "no" },
                    if reasons.is_empty() { "-".to_string() } else { reasons.join("+") }
                );
            }
        } else if let Some(h) = rec.triple(base) {
            say!(
                "v24,card_proto_p{p}_empty_byte0,rise={},width={},done={}",
                h.0,
                h.1,
                h.2
            );
        }
    }

    // ---- calibration
    let mut values: Vec<(&str, i64)> = Vec::new();
    for (k, name) in ["gpu_only", "cpu_only", "both"].iter().enumerate() {
        if let Some(t) = rec.triple(0x8C0 + k as u32) {
            values.push((name, t.0 | t.1 << 16));
        }
    }
    let split = rec.triple(0x8C3);
    let frames = [rec.triple(0x8C4), rec.triple(0x8C5)];
    let mut line = "v24,calibration,".to_string();
    line += &values
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    if let Some(s) = split {
        line += &format!(",cpu_done={},wait_after={}", s.0 | s.1 << 16, s.2);
    }
    say!("{line}");
    for (name, t) in ["one_list_per_frame", "four_lists_per_frame"]
        .iter()
        .zip(frames)
    {
        if let Some(t) = t {
            say!(
                "v24,calibration_frames,{name},frames={},clocks={}",
                t.0,
                t.1 | t.2 << 16
            );
        }
    }
    if values.len() == 3 {
        let (g, c, b) = (values[0].1, values[1].1, values[2].1);
        if g + c > 0 && g.max(c) > 0 {
            say!(
                "v24,calibration_ratio,both/(gpu+cpu)={:.3},both/max={:.3}",
                b as f64 / (g + c) as f64,
                b as f64 / g.max(c) as f64
            );
        }
        if !(g > 0
            && c > 0
            && b as f64 >= g.max(c) as f64 * 0.9
            && b as f64 <= (g + c) as f64 * 1.1)
        {
            rec.faults.push(format!(
                "calibration times not coherent: gpu {g} cpu {c} both {b}"
            ));
        }
    }
    if let Some(t) = rec.triple(0x8C2) {
        if t.2 != 0 {
            rec.faults
                .push("calibration: a wait for the list ran out".to_string());
        }
    }

    // ---- UV windows
    say!("v24,uv_window,row,depth,window,span,origin_u,origin_v,clocks_min,med,max");
    for (k, (depth_window, span, origin)) in uv_rows_expected().into_iter().enumerate() {
        let timing = rec.triple(UV_WINDOW_FIRST + 2 * k as u32);
        let row = rec.triple(UV_WINDOW_FIRST + 2 * k as u32 + 1);
        let (Some(timing), Some(row)) = (timing, row) else {
            continue;
        };
        if row != (depth_window, span, origin) {
            rec.faults.push(format!(
                "uv window row {k}: descriptor {row:?}, expected {:?}",
                (depth_window, span, origin)
            ));
        }
        say!(
            "v24,uv_window,{k},{},{}*8,{span},{},{},{},{},{}",
            depth_window & 15,
            depth_window >> 4,
            origin & 255,
            origin >> 8,
            timing.0,
            timing.1,
            timing.2
        );
        if timing.0 == 0 || timing.2 == 0xFFFF {
            rec.faults
                .push(format!("uv window row {k}: timing {timing:?}"));
        }
    }

    // ---- pads
    let model = |code: i64| -> Option<&'static str> {
        match code {
            0 => Some("no answer"),
            1 => Some("SCPH-1200"),
            2 => Some("SCPH-110"),
            3 => Some("other"),
            4 => Some("none/digital"),
            _ => None,
        }
    };
    for (k, offset) in [0x960u32, 0x961].iter().enumerate() {
        if let Some(t) = rec.triple(*offset) {
            say!(
                "v24,pad_choice_{},model={},raw_buttons=0x{:04X},frame={}",
                k + 1,
                model(t.0).unwrap_or("?"),
                t.1,
                t.2
            );
            if model(t.0).is_none() {
                rec.faults
                    .push(format!("pad choice {offset:03X}: code {}", t.0));
            }
        }
    }
    if let Some(t) = rec.triple(0x962) {
        say!(
            "v24,pad_passes,second_ran={},first_config={},second_config={},ports=0x{:04X},answer_flags=0x{:X}",
            t.0 & 1,
            (t.0 >> 1) & 1,
            (t.0 >> 2) & 1,
            t.1,
            t.2
        );
        let second_present = (0x940..0x957).any(|id| rec.by_id.contains_key(&id));
        if (t.0 & 1 != 0) != second_present {
            rec.faults
                .push("pad passes: second pass flag and second pass records disagree".to_string());
        }
    }
    for (pass_index, offset) in [0u32, 0x1E0].iter().enumerate() {
        for (base, name) in [
            (0x76Du32, "identity_p1"),
            (0x76E, "identity_p2"),
            (0x766, "operator"),
        ] {
            let t = if pass_index == 0 || rec.by_id.contains_key(&(base + offset)) {
                rec.triple(base + offset)
            } else {
                None
            };
            if let Some(t) = t {
                say!(
                    "v24,pad_pass{}_{name},0x{:04X},0x{:04X},0x{:04X}",
                    pass_index + 1,
                    t.0,
                    t.1,
                    t.2
                );
            }
        }
        for k in 0..6u32 {
            if let Some(a) = rec.by_id.get(&(0x771 + k + offset)) {
                say!(
                    "v24,pad_pass{}_answers_{:03X},{},{},{}",
                    pass_index + 1,
                    0x771 + k,
                    a.minimum,
                    a.median,
                    a.maximum
                );
            }
        }
    }
    for fault in &rec.faults {
        say!("v24,FAULT,{fault}");
    }
    say!("v24,faults,{}", rec.faults.len());
    Ok(rec.faults.len())
}

/// `v24-report PAYLOAD_OR_FILE ...`: exit non-zero on a harness fault.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    if let Some(flag) = args.iter().find(|arg| arg.starts_with("--")) {
        return Err(Error(format!("unrecognized arguments: {flag}")));
    }
    let capture = parse_capture(&payloads_from_paths(args)?)?;
    Ok(i32::from(v24_report(&capture, out)? != 0))
}
