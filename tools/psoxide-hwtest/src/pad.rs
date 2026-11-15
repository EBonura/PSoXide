//! `pad-report` and `pad-compare`: the controller survey's records (`PXCB`, pages `PXC/...`).
//!
//! The record layout is `engine/examples/hardware-tests/src/pad_record.rs`, compiled into this
//! crate by path, so the tests encode with the code the console runs and the reader here is held
//! to it. A page file is what `tools/hwtest-video-qr.py` writes (one `PXC/...` line per page);
//! one file can hold several controllers, each its own run id.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::util::{base64_decode, crc32, Error, Reader, Result};

#[path = "../../../engine/examples/hardware-tests/src/pad_record.rs"]
pub mod record;

use record::{
    Axis, Counters, Failure, Handover, Header, IdRun, LabResult, ModeRun, NoteRun, PollNote,
    PollRun, PollView, PowerOn, Soak, Txn, Window, Wire, HANDOVER_POLLS_MAX, ID_RUNS, LAB_MAX,
    MODE_RUNS, POWER_RUNS, SOAK_FAILS, SOAK_POLLS_MAX, SOAK_RUNS, TXN_MAX,
};

/// One decoded controller record.
pub struct PadRecord {
    /// Header fields.
    pub header: Header,
    /// Transactions of the raw survey, in order.
    pub txns: Vec<Txn>,
    /// Guided windows, in order.
    pub windows: Vec<Window>,
    /// SDK soaks.
    pub soaks: Vec<Soak>,
    /// Hand-overs.
    pub handovers: Vec<Handover>,
    /// The power-on section, in a kind-2 record.
    pub power: Option<Box<PowerOn>>,
}

/// Page lines to binaries, one per run id: every page's CRC and the binary's CRC are checked.
pub fn assemble_pages(text: &str) -> Result<Vec<(u16, Vec<u8>)>> {
    let mut runs: BTreeMap<u16, (usize, BTreeMap<usize, String>)> = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|l| l.starts_with("PXC/")) {
        let body = line.rsplit_once("/C:").ok_or_else(|| Error(format!("no CRC in {line:.40}")))?;
        let mut parts = body.0.splitn(3, '/');
        let (_, head, chunk) = (parts.next(), parts.next(), parts.next());
        let (Some(head), Some(chunk)) = (head, chunk) else {
            return Err(Error(format!("malformed page {line:.40}")));
        };
        ensure!(head.len() == 8, "page header {head:?} is not 8 hex digits");
        let number = usize::from_str_radix(&head[0..2], 16).map_err(|e| Error(e.to_string()))?;
        let total = usize::from_str_radix(&head[2..4], 16).map_err(|e| Error(e.to_string()))?;
        let run = u16::from_str_radix(&head[4..8], 16).map_err(|e| Error(e.to_string()))?;
        let claimed = u32::from_str_radix(body.1, 16).map_err(|e| Error(e.to_string()))?;
        if claimed != crc32(chunk.as_bytes()) {
            continue; // a damaged page: another copy of it may still be in the file
        }
        let entry = runs.entry(run).or_insert((total, BTreeMap::new()));
        entry.0 = total;
        entry.1.insert(number, chunk.to_string());
    }
    let mut out = Vec::new();
    for (run, (total, pages)) in runs {
        let missing: Vec<usize> = (1..=total).filter(|n| !pages.contains_key(n)).collect();
        ensure!(missing.is_empty(), "run {run:04X}: pages {missing:?} of {total} missing");
        let mut b64 = String::new();
        for n in 1..=total {
            b64.push_str(&pages[&n]);
        }
        let binary = base64_decode(&b64)?;
        ensure!(binary.len() >= 8, "run {run:04X}: record too short");
        let (body, tail) = binary.split_at(binary.len() - 4);
        let claimed = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        ensure!(claimed == crc32(body), "run {run:04X}: binary CRC does not check out");
        out.push((run, binary));
    }
    Ok(out)
}

fn read_ids(r: &mut Reader) -> Result<([IdRun; ID_RUNS], u8)> {
    let n = r.u8()?;
    ensure!(usize::from(n) <= ID_RUNS, "{n} ID runs");
    let mut runs = [IdRun::default(); ID_RUNS];
    for run in runs.iter_mut().take(usize::from(n)) {
        run.id = r.u8()?;
        run.count = r.u16()?;
    }
    Ok((runs, n))
}

fn read_modes(r: &mut Reader) -> Result<([ModeRun; MODE_RUNS], u8)> {
    let n = r.u8()?;
    ensure!(usize::from(n) <= MODE_RUNS, "{n} mode runs");
    let mut runs = [ModeRun::default(); MODE_RUNS];
    for run in runs.iter_mut().take(usize::from(n)) {
        let byte = r.u8()?;
        run.mode = byte & 0x7F;
        run.analog = byte & 0x80 != 0;
        run.count = r.u16()?;
    }
    Ok((runs, n))
}

fn read_view(r: &mut Reader) -> Result<PollView> {
    let mut v = PollView::ZERO;
    v.n = r.u8()?;
    v.rx.copy_from_slice(r.bytes(9)?);
    v.mode = r.u8()?;
    v.flags = r.u8()?;
    v.buttons = r.u16()?;
    v.sticks.copy_from_slice(r.bytes(4)?);
    v.class = r.u8()?;
    v.acked = r.u8()?;
    v.early = r.u8()?;
    v.irq_after_clear = r.u8()? != 0;
    Ok(v)
}

fn read_txn(r: &mut Reader) -> Result<Txn> {
    let mut t = Txn::EMPTY;
    t.step = r.u8()?;
    let ps = r.u8()?;
    t.prof = ps & 0x0F;
    t.seq = ps >> 4;
    t.flags = r.u8()?;
    t.sent = r.u8()?;
    t.got = r.u8()?;
    ensure!(usize::from(t.sent) <= TXN_MAX, "transaction of {} bytes", t.sent);
    for i in 0..usize::from(t.sent) {
        t.tx[i] = r.u8()?;
        t.rx[i] = r.u8()?;
        t.rise[i] = r.u16()?;
        t.width[i] = r.u8()?;
    }
    Ok(t)
}

fn read_window(r: &mut Reader) -> Result<Window> {
    let mut w = Window::EMPTY;
    w.phase = r.u8()?;
    w.frames_planned = r.u16()?;
    w.frames_run = r.u16()?;
    w.raw_ok = r.u16()?;
    w.raw_bad = r.u16()?;
    (w.raw_ids, w.raw_id_n) = read_ids(r)?;
    w.raw_or = r.u16()?;
    w.raw_and = r.u16()?;
    for a in &mut w.raw_axis {
        *a = Axis { min: r.u8()?, max: r.u8()?, samples: [0; 4] };
        a.samples.copy_from_slice(r.bytes(4)?);
    }
    w.sdk_ok = r.u16()?;
    w.sdk_bad = r.u16()?;
    w.sdk_stale = r.u16()?;
    w.sdk_conn = r.u16()?;
    w.sdk_analog = r.u16()?;
    (w.sdk_modes, w.sdk_mode_n) = read_modes(r)?;
    w.sdk_or = r.u16()?;
    w.sdk_and = r.u16()?;
    w.sdk_min.copy_from_slice(r.bytes(4)?);
    w.sdk_max.copy_from_slice(r.bytes(4)?);
    Ok(w)
}

fn read_soak(r: &mut Reader) -> Result<Soak> {
    let mut s = Soak::EMPTY;
    s.kind = r.u8()?;
    s.polls = r.u16()?;
    s.ok = r.u16()?;
    s.failed = r.u16()?;
    s.stale = r.u16()?;
    s.irq_failed = r.u16()?;
    s.config_txns = r.u16()?;
    s.vblanks = r.u16()?;
    s.poll_min = r.u16()?;
    s.poll_max = r.u16()?;
    for c in &mut s.classes {
        *c = r.u16()?;
    }
    for c in &mut s.acked_hist {
        *c = r.u16()?;
    }
    for c in &mut s.rise_min {
        *c = r.u16()?;
    }
    for c in &mut s.rise_max {
        *c = r.u16()?;
    }
    s.width_min.copy_from_slice(r.bytes(9)?);
    s.width_max.copy_from_slice(r.bytes(9)?);
    s.latch_polls = r.u16()?;
    s.failed_bits.copy_from_slice(r.bytes(SOAK_POLLS_MAX / 8)?);
    (s.ids, s.id_n) = read_ids(r)?;
    s.run_n = r.u8()?;
    ensure!(usize::from(s.run_n) <= SOAK_RUNS, "{} soak runs", s.run_n);
    for i in 0..usize::from(s.run_n) {
        s.runs[i] = PollRun { count: r.u16()?, view: read_view(r)? };
    }
    s.runs_dropped = r.u16()?;
    s.fail_n = r.u8()?;
    ensure!(usize::from(s.fail_n) <= SOAK_FAILS, "{} failures", s.fail_n);
    for i in 0..usize::from(s.fail_n) {
        let poll = r.u16()?;
        let view = read_view(r)?;
        let mut wire = Wire::NONE;
        for b in 0..9 {
            wire.rise[b] = r.u16()?;
        }
        wire.width.copy_from_slice(r.bytes(9)?);
        s.fails[i] = Failure { poll, view, wire };
    }
    Ok(s)
}

fn read_note(r: &mut Reader) -> Result<PollNote> {
    let mut n = PollNote::default();
    n.lab = r.u8()?;
    n.first_class = r.u8()?;
    n.first_id = r.u8()?;
    n.first_died = r.u8()?;
    n.class = r.u8()?;
    n.id = r.u8()?;
    n.n = r.u8()?;
    n.acked = r.u8()?;
    n.died = r.u8()?;
    n.tries = r.u8()?;
    n.fresh = r.u8()?;
    n.cfg = r.u8()?;
    n.exits = r.u8()?;
    n.rx.copy_from_slice(r.bytes(9)?);
    n.mode = r.u8()?;
    n.flags = r.u8()?;
    n.buttons = r.u16()?;
    n.sticks.copy_from_slice(r.bytes(4)?);
    n.fold = r.u8()?;
    n.why = r.u8()?;
    Ok(n)
}

fn read_poweron(r: &mut Reader) -> Result<Box<PowerOn>> {
    let mut p = Box::new(PowerOn::EMPTY);
    p.polls = r.u16()?;
    p.run_n = r.u16()?;
    p.dropped = r.u16()?;
    p.frames = r.u32()?;
    ensure!(usize::from(p.run_n) <= POWER_RUNS, "{} power-on runs", p.run_n);
    p.counters = Counters {
        ok: r.u32()?,
        bad: r.u32()?,
        nak: r.u32()?,
        retry: r.u32()?,
        handshakes: r.u32()?,
        cfg: r.u32()?,
        exits: r.u32()?,
        exits_f3: r.u32()?,
    };
    p.lab_n = r.u8()?;
    ensure!(usize::from(p.lab_n) <= LAB_MAX, "{} lab results", p.lab_n);
    for i in 0..usize::from(p.lab_n) {
        let mut l = LabResult::default();
        l.phase = r.u8()?;
        l.frames = r.u16()?;
        l.polls = r.u16()?;
        l.ok = r.u16()?;
        l.bad = r.u16()?;
        l.first_bad = r.u16()?;
        l.btn_or = r.u16()?;
        l.last_id = r.u8()?;
        l.last_mode = r.u8()?;
        l.last_flags = r.u8()?;
        l.raw_ok = r.u8()?;
        l.raw_total = r.u8()?;
        l.status45.copy_from_slice(r.bytes(6)?);
        l.status45_set = r.u8()?;
        l.fold_hits = r.u16()?;
        p.lab[i] = l;
    }
    for i in 0..usize::from(p.run_n) {
        let count = r.u16()?;
        let hold_min = r.u16()?;
        let hold_max = r.u16()?;
        p.runs[i] = NoteRun { count, note: read_note(r)?, hold_min, hold_max };
    }
    Ok(p)
}

fn read_handover(r: &mut Reader) -> Result<Handover> {
    let mut h = Handover::EMPTY;
    h.variant = r.u8()?;
    let m = r.u8()?;
    h.launcher_mode = m & 0x7F;
    h.launcher_analog = m & 0x80 != 0;
    h.first_poll_vblanks = r.u16()?;
    h.polls = r.u16()?;
    h.failed = r.u16()?;
    h.config_polls = r.u16()?;
    h.first_analog = r.u16()?;
    h.failed_bits.copy_from_slice(r.bytes(HANDOVER_POLLS_MAX / 8)?);
    (h.ids, h.id_n) = read_ids(r)?;
    (h.modes, h.mode_n) = read_modes(r)?;
    h.sample_n = r.u8()?;
    ensure!(usize::from(h.sample_n) <= record::HANDOVER_SAMPLES, "{} samples", h.sample_n);
    for i in 0..usize::from(h.sample_n) {
        h.samples[i] = read_view(r)?;
    }
    h.last = read_view(r)?;
    Ok(h)
}

/// Decode one record binary (CRC trailer included and checked).
pub fn parse_record(binary: &[u8]) -> Result<PadRecord> {
    ensure!(binary.len() >= record::HEADER_LEN + 4, "record of {} bytes", binary.len());
    let (body, tail) = binary.split_at(binary.len() - 4);
    ensure!(
        u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) == crc32(body),
        "record CRC does not check out"
    );
    let mut r = Reader::new(body, 0);
    ensure!(r.bytes(4)? == record::MAGIC, "not a PXCB record");
    let schema = r.u8()?;
    ensure!(
        (record::SCHEMA_MIN..=record::SCHEMA).contains(&schema),
        "schema {schema}, this reader knows {} to {}",
        record::SCHEMA_MIN,
        record::SCHEMA
    );
    let header = Header {
        suite_major: r.u8()?,
        suite_minor: r.u8()?,
        record_no: r.u8()?,
        boot_id: r.u8()?,
        port: r.u8()?,
        kind: r.u8()?,
        flags: r.u8()?,
        sdk_rev: {
            let mut rev = [0u8; 8];
            rev.copy_from_slice(r.bytes(8)?);
            rev
        },
        timer_hz: {
            let _timer = r.u8()?;
            let _reserved = r.u8()?;
            r.u32()?
        },
        ack_timeout: r.u16()?,
    };
    let _ = r.bytes(4)?;
    let mut rec = PadRecord {
        header,
        txns: Vec::new(),
        windows: Vec::new(),
        soaks: Vec::new(),
        handovers: Vec::new(),
        power: None,
    };
    while r.offset < body.len() {
        let tag = r.u8()?;
        let len = usize::from(r.u16()?);
        let section = r.bytes(len)?;
        let mut s = Reader::new(section, 0);
        match tag {
            record::TAG_TXN => {
                while s.offset < section.len() {
                    rec.txns.push(read_txn(&mut s)?);
                }
            }
            record::TAG_WINDOW => {
                while s.offset < section.len() {
                    rec.windows.push(read_window(&mut s)?);
                }
            }
            record::TAG_SOAK => rec.soaks.push(read_soak(&mut s)?),
            record::TAG_HANDOVER => rec.handovers.push(read_handover(&mut s)?),
            record::TAG_POWERON => rec.power = Some(read_poweron(&mut s)?),
            other => return Err(Error(format!("unknown section tag {other}"))),
        }
    }
    Ok(rec)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

fn rev(h: &Header) -> String {
    String::from_utf8_lossy(&h.sdk_rev).into_owned()
}

/// Whether a transaction read as a full reply from a pad.
pub fn txn_ok(t: &Txn) -> bool {
    t.got >= 3 && t.got == t.sent && t.flags & 7 == 0 && t.rx[2] == 0x5A
}

/// The number a soak is called by: kind 3 is the one that runs first, soak 0.
fn soak_label(kind: u8) -> u8 {
    if kind == 3 {
        0
    } else {
        kind + 1
    }
}

/// A line of the per-controller summary: a key the comparison matches on, and the text.
pub type Line = (String, String);

/// The plain-text summary of one record: every verdict line, keyed for the comparison.
pub fn summarise(rec: &PadRecord) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let h = &rec.header;
    out.push((
        "record".into(),
        format!(
            "controller {} on port {}, {}, suite v{}.{}, SDK {}, timer 0 = system clock {} Hz, ACK timeout {} ticks{}",
            h.record_no,
            h.port + 1,
            match h.kind {
                0 => "a pad answered",
                2 => "POWER-ON RECORD (the menu's first polls)",
                _ => "NOTHING ANSWERED",
            },
            h.suite_major,
            h.suite_minor,
            rev(h),
            h.timer_hz,
            h.ack_timeout,
            if h.flags & 1 != 0 { ", RECORD TRUNCATED" } else { "" }
        ),
    ));
    if let Some(p) = &rec.power {
        power_lines(p, &mut out);
    }
    // Raw survey.
    for p in 0..4u8 {
        let ts: Vec<&Txn> = rec.txns.iter().filter(|t| t.prof == p).collect();
        if ts.is_empty() {
            continue;
        }
        let ok = ts.iter().filter(|t| txn_ok(t)).count();
        let missing = ts.iter().filter(|t| t.flags & 2 != 0).count();
        let mut rise = (u16::MAX, 0u16);
        let mut width = (u8::MAX, 0u8);
        for t in &ts {
            for i in 0..usize::from(t.sent) {
                if t.rise[i] != 0xFFFF {
                    rise = (rise.0.min(t.rise[i]), rise.1.max(t.rise[i]));
                    if t.width[i] != 0xFF && t.width[i] != 0 {
                        width = (width.0.min(t.width[i]), width.1.max(t.width[i]));
                    }
                }
            }
        }
        let first_bad = ts.iter().find(|t| !txn_ok(t)).map(|t| {
            format!("; first failure {} at byte {} (flags {:02X})", record::step_name(t.step), t.got, t.flags)
        });
        out.push((
            format!("raw.{}", record::PROFILE_NAMES[usize::from(p)]),
            format!(
                "raw {:<6}: {ok}/{} transactions complete, {missing} with a missing ACK, ACK rise {}-{} ticks, width {}-{}{}",
                record::PROFILE_NAMES[usize::from(p)],
                ts.len(),
                if rise.0 == u16::MAX { 0 } else { rise.0 },
                rise.1,
                if width.0 == u8::MAX { 0 } else { width.0 },
                width.1,
                first_bad.unwrap_or_default()
            ),
        ));
    }
    if let Some(t) = rec.txns.iter().find(|t| t.step == 1) {
        out.push(("raw.powerup".into(), format!("ID as powered: {:02X}, reply {}", t.rx[1], hex(&t.rx[..usize::from(t.got)]))));
    }
    for (step, name) in [(3u8, "status 45"), (4, "const 46 p0"), (5, "const 46 p1"), (6, "const 47"), (7, "const 4C p0"), (8, "const 4C p1")] {
        if let Some(t) = rec.txns.iter().find(|t| t.step == step && t.prof == 2).or_else(|| rec.txns.iter().find(|t| t.step == step)) {
            out.push((format!("raw.{name}"), format!("{name}: {}", hex(&t.rx[..usize::from(t.got)]))));
        }
    }
    // Polls after each config exit: how many until the reply is stable.
    for (step, label) in [(13u8, "after exit 1 (analog)"), (17, "after exit 2 (digital)"), (21, "after exit 3 (analog)")] {
        for p in 0..4u8 {
            let polls: Vec<&Txn> = rec.txns.iter().filter(|t| t.step == step && t.prof == p).collect();
            if polls.is_empty() {
                continue;
            }
            let last = polls.last().map(|t| t.rx[1]).unwrap_or(0xFF);
            let mut stable_from = polls.len();
            while stable_from > 0 && polls[stable_from - 1].rx[1] == last && txn_ok(polls[stable_from - 1]) {
                stable_from -= 1;
            }
            out.push((
                format!("raw.stable.{step}.{p}"),
                format!(
                    "{:<6} polls {label}: IDs {}; stable from poll {} (last ID {last:02X})",
                    record::PROFILE_NAMES[usize::from(p)],
                    polls.iter().map(|t| format!("{:02X}", t.rx[1])).collect::<Vec<_>>().join(" "),
                    if stable_from == polls.len() { "NEVER".to_string() } else { stable_from.to_string() }
                ),
            ));
        }
    }
    // Windows.
    for w in &rec.windows {
        let name = record::WINDOW_NAMES.get(usize::from(w.phase)).copied().unwrap_or("?");
        let spread = |lo: u8, hi: u8| hi.saturating_sub(lo);
        let raw_spread = w.raw_axis.iter().map(|a| spread(a.min, a.max)).max().unwrap_or(0);
        let sdk_spread = (0..4).map(|a| spread(w.sdk_min[a], w.sdk_max[a])).max().unwrap_or(0);
        let mut verdict = Vec::new();
        if w.raw_or != w.sdk_or {
            verdict.push(format!("BUTTONS DIFFER raw {:04X} sdk {:04X}", w.raw_or, w.sdk_or));
        }
        if raw_spread >= 0x40 && sdk_spread < 0x20 {
            verdict.push("STICKS MOVE ON THE WIRE BUT THE SDK REPORTS THEM DEAD".to_string());
        }
        if w.raw_ok > 0 && w.sdk_ok == 0 {
            verdict.push("SDK NEVER COMPLETED A POLL".to_string());
        }
        out.push((
            format!("window.{}", w.phase),
            format!(
                "window {} {name}: raw ok {} bad {}, buttons or {:04X} and {:04X}, stick spread {raw_spread:02X} (RX {:02X}-{:02X} RY {:02X}-{:02X} LX {:02X}-{:02X} LY {:02X}-{:02X}); sdk ok {} bad {} stale {}, buttons {:04X}, stick spread {sdk_spread:02X}, analog frames {}/{}, IDs {}{}",
                w.phase + 1,
                w.raw_ok,
                w.raw_bad,
                w.raw_or,
                w.raw_and,
                w.raw_axis[0].min, w.raw_axis[0].max, w.raw_axis[1].min, w.raw_axis[1].max,
                w.raw_axis[2].min, w.raw_axis[2].max, w.raw_axis[3].min, w.raw_axis[3].max,
                w.sdk_ok,
                w.sdk_bad,
                w.sdk_stale,
                w.sdk_or,
                w.sdk_analog,
                w.frames_run,
                w.raw_ids[..usize::from(w.raw_id_n)].iter().map(|r| format!("{:02X}x{}", r.id, r.count)).collect::<Vec<_>>().join(","),
                if verdict.is_empty() { String::new() } else { format!("; {}", verdict.join("; ")) }
            ),
        ));
    }
    // Soaks.
    for s in &rec.soaks {
        let mut modal = 0usize;
        for (i, c) in s.acked_hist.iter().enumerate() {
            if *c > s.acked_hist[modal] {
                modal = i;
            }
        }
        let last = s.runs[usize::from(s.run_n.saturating_sub(1))].view;
        let mut line = format!(
            "soak {} polls {}: ok {}",
            soak_label(s.kind),
            s.polls,
            s.classes[0]
        );
        if s.classes[1] != 0 {
            let _ = write!(line, ", short {} (died at byte {modal})", s.classes[1]);
        }
        let _ = write!(
            line,
            ", garbled {}, none {}, no exchange {}; reported {} {} buttons {:04X} sticks {}, stale {}, failed under a VBlank {}, config transactions {}, poll {}-{} ticks, {} ID runs {}, {} state runs (+{} dropped)",
            s.classes[2],
            s.classes[3],
            s.classes[4],
            mode_name(last.mode),
            if last.flags & 2 != 0 { "analog()" } else { "not analog()" },
            last.buttons,
            hex(&last.sticks),
            s.stale,
            s.irq_failed,
            s.config_txns,
            s.poll_min,
            s.poll_max,
            s.id_n,
            s.ids[..usize::from(s.id_n)].iter().map(|r| format!("{:02X}x{}", r.id, r.count)).collect::<Vec<_>>().join(","),
            s.run_n,
            s.runs_dropped
        );
        out.push((format!("soak.{}", s.kind), line));
        let rise: Vec<String> = (0..9)
            .filter(|&b| s.rise_max[b] != 0)
            .map(|b| format!("b{b}:{}-{}/{}-{}", s.rise_min[b], s.rise_max[b], s.width_min[b], s.width_max[b]))
            .collect();
        out.push((format!("soak.{}.ack", s.kind), format!("soak {} ACK rise-ticks/width per byte: {}; acked histogram {:?}; latch-only {}", soak_label(s.kind), rise.join(" "), s.acked_hist, s.latch_polls)));
        for f in &s.fails {
            let _ = f;
        }
        for f in s.fails.iter().take(usize::from(s.fail_n)) {
            out.push((
                format!("soak.{}.fail.{}", s.kind, f.poll),
                format!(
                    "soak {} failure at poll {}: wire {} class {} acked {} early {} irq-after-clear {}, reported {} buttons {:04X}",
                    soak_label(s.kind),
                    f.poll,
                    hex(&f.view.rx[..usize::from(f.view.n.min(9))]),
                    f.view.class,
                    f.view.acked,
                    f.view.early,
                    f.view.irq_after_clear,
                    mode_name(f.view.mode),
                    f.view.buttons
                ),
            ));
        }
    }
    // Hand-overs.
    for h in &rec.handovers {
        out.push((
            format!("handover.{}", h.variant),
            format!(
                "hand-over {}: launcher {} analog() {}; game first poll took {} frames; first analog report at poll {}; ID F3 (config) polls {}; failed {} of {}; IDs {}; modes {}; last wire {} reports {} buttons {:04X} sticks {}",
                record::HANDOVER_NAMES[usize::from(h.variant).min(1)],
                mode_name(h.launcher_mode),
                h.launcher_analog,
                h.first_poll_vblanks,
                if h.first_analog == 0xFFFF { "NEVER".to_string() } else { h.first_analog.to_string() },
                h.config_polls,
                h.failed,
                h.polls,
                h.ids[..usize::from(h.id_n)].iter().map(|r| format!("{:02X}x{}", r.id, r.count)).collect::<Vec<_>>().join(","),
                h.modes[..usize::from(h.mode_n)].iter().map(|m| format!("{}{}x{}", mode_name(m.mode), if m.analog { "+A" } else { "" }, m.count)).collect::<Vec<_>>().join(","),
                hex(&h.last.rx[..usize::from(h.last.n.min(9))]),
                mode_name(h.last.mode),
                h.last.buttons,
                hex(&h.last.sticks)
            ),
        ));
    }
    out
}

/// Microseconds a hold of `ticks` HBlank counts is (63.56 us each; the counter's step is the
/// resolution).
fn hold_us(ticks: u16) -> u32 {
    u32::from(ticks) * 6356 / 100
}

fn note_text(n: &PollNote) -> String {
    format!(
        "{} ID {:02X} acked {} died {} first-try {} ID {:02X} died {} tries {} {} cfg {} exits {} wire {} reports {}{} buttons {:04X} sticks {} fold {} ({})",
        record::NOTE_CLASS_NAMES.get(usize::from(n.class)).copied().unwrap_or("?"),
        n.id,
        n.acked,
        if n.died == 0xFF { "-".to_string() } else { n.died.to_string() },
        record::NOTE_CLASS_NAMES.get(usize::from(n.first_class)).copied().unwrap_or("?"),
        n.first_id,
        if n.first_died == 0xFF { "-".to_string() } else { n.first_died.to_string() },
        n.tries,
        if n.fresh & 1 != 0 { "fresh-select" } else { "held-select" },
        n.cfg,
        n.exits,
        hex(&n.rx[..usize::from(n.n.min(9))]),
        mode_name(n.mode),
        if n.flags & 2 != 0 { " analog-locked" } else { "" },
        n.buttons,
        hex(&n.sticks),
        if n.fold == 0 { "none".to_string() } else { format!("{:04b}", n.fold) },
        record::FOLD_WHY.get(usize::from(n.why)).copied().unwrap_or("?"),
    )
}

/// The power-on record's verdict lines: first poll, counters, one line per lab phase.
fn power_lines(p: &PowerOn, out: &mut Vec<Line>) {
    let c = &p.counters;
    out.push((
        "power.counters".into(),
        format!(
            "power-on record: {} polls in {} runs (+{} dropped), {} frames at close; since boot: ok {} bad {} address NAKs {} retries {} handshakes {} config packets {} exits {} exits left in F3 {}",
            p.polls, p.run_n, p.dropped, p.frames, c.ok, c.bad, c.nak, c.retry, c.handshakes, c.cfg, c.exits, c.exits_f3
        ),
    ));
    if let Some(first) = p.runs.first().filter(|_| p.run_n > 0) {
        out.push(("power.poll0".into(), format!("poll 0 (the handshake from power): {}", note_text(&first.note))));
    }
    // From which poll on the SDK said analog and locked, and the first poll that completed.
    let mut at = 0u32;
    let mut analog_at = None;
    let mut ok_at = None;
    for r in &p.runs[..usize::from(p.run_n)] {
        if ok_at.is_none() && r.note.class == 0 {
            ok_at = Some(at);
        }
        if analog_at.is_none() && r.note.flags & 2 != 0 {
            analog_at = Some(at);
        }
        at += u32::from(r.count);
    }
    let fmt = |v: Option<u32>| v.map_or("NEVER".to_string(), |v| v.to_string());
    out.push((
        "power.first".into(),
        format!("first complete poll: {}; first poll reported analog and locked: {}", fmt(ok_at), fmt(analog_at)),
    ));
    let mut seen = 0u16;
    let mut sticks = false;
    for r in &p.runs[..usize::from(p.run_n)] {
        seen |= r.note.buttons;
        sticks |= r.note.fold != 0;
    }
    out.push((
        "power.buttons".into(),
        format!("buttons seen in the first {} polls: {seen:04X}; the stick fold gave a d-pad bit: {}", p.polls, if sticks { "yes" } else { "never" }),
    ));
    for l in &p.lab[..usize::from(p.lab_n)] {
        let name = record::LAB_NAMES.get(usize::from(l.phase)).copied().unwrap_or("?");
        let mut line = format!(
            "lab {name}: {} frames, polls {} ok {} bad {} first-try bad {}, buttons seen {:04X}{}, last ID {:02X} {}{}, fold hits {}",
            l.frames,
            l.polls,
            l.ok,
            l.bad,
            l.first_bad,
            l.btn_or,
            if l.btn_or == 0 { " (NONE: nothing pressed, or the pad does not answer)" } else { "" },
            l.last_id,
            mode_name(l.last_mode),
            if l.last_flags & 2 != 0 { " analog-locked" } else { "" },
            l.fold_hits
        );
        if l.raw_total != 0 {
            let _ = write!(line, "; own commands {}/{} complete", l.raw_ok, l.raw_total);
        }
        if l.status45_set != 0 {
            let _ = write!(
                line,
                "; STATUS 45 reply {} ({})",
                hex(&l.status45),
                if l.status45 == [0x01, 0x02, 0x01, 0x02, 0x01, 0x00] { "as a good DualShock answers" } else { "NOT the usual 01 02 01 02 01 00" }
            );
        }
        out.push((format!("lab.{}", l.phase), line));
    }
}

fn mode_name(mode: u8) -> &'static str {
    ["none", "digital", "analog", "negcon", "config", "unknown"].get(usize::from(mode)).copied().unwrap_or("?")
}

/// Print the full report of one record: the summary, then the transaction log.
pub fn print_record(rec: &PadRecord, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "== CONTROLLER {} (port {}) ==", rec.header.record_no, rec.header.port + 1)?;
    for (_, line) in summarise(rec) {
        writeln!(out, "{line}")?;
    }
    if let Some(p) = &rec.power {
        writeln!(out, "-- power-on polls, run-length coded (polls / lab phase / hold of /CS before the poll in us) --")?;
        let mut at = 0u32;
        for r in &p.runs[..usize::from(p.run_n)] {
            writeln!(
                out,
                "polls {at}-{} lab {} hold {}-{} us: {}",
                at + u32::from(r.count) - 1,
                if r.note.lab == 0xFF { "after".to_string() } else { (r.note.lab + 1).to_string() },
                hold_us(r.hold_min),
                hold_us(r.hold_max),
                note_text(&r.note)
            )?;
            at += u32::from(r.count);
        }
    }
    writeln!(out, "-- raw transaction log (tx / rx / ack rise ticks / ack width ticks) --")?;
    for t in &rec.txns {
        let mut s = format!(
            "{:<6} {:<26}{} flags {:02X} sent {:>2} got {:>2} |",
            record::PROFILE_NAMES[usize::from(t.prof & 3)],
            record::step_name(t.step),
            if t.seq > 0 || t.step == 13 || t.step == 17 || t.step == 21 { format!(" #{}", t.seq) } else { String::new() },
            t.flags,
            t.sent,
            t.got
        );
        for i in 0..usize::from(t.sent) {
            let rise = if t.rise[i] == 0xFFFF { "--".to_string() } else { t.rise[i].to_string() };
            let width = match t.width[i] {
                0xFF => "L".to_string(),
                0 => "-".to_string(),
                w => w.to_string(),
            };
            let rx = if i < usize::from(t.got) { format!("{:02X}", t.rx[i]) } else { "..".to_string() };
            let _ = write!(s, " {:02X}>{rx}@{rise}/{width}", t.tx[i]);
        }
        writeln!(out, "{s}")?;
    }
    Ok(())
}

/// Diff the summaries of two controllers.
pub fn compare(a: &PadRecord, b: &PadRecord, out: &mut dyn Write) -> Result<u32> {
    let (sa, sb) = (summarise(a), summarise(b));
    let ma: BTreeMap<&str, &str> = sa.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mb: BTreeMap<&str, &str> = sb.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    writeln!(
        out,
        "== DIFF controller {} (port {}) against controller {} (port {}) ==",
        a.header.record_no,
        a.header.port + 1,
        b.header.record_no,
        b.header.port + 1
    )?;
    let mut keys: Vec<&str> = ma.keys().chain(mb.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    let mut diffs = 0;
    for key in keys {
        if key == "record" {
            continue;
        }
        match (ma.get(key), mb.get(key)) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                diffs += 1;
                writeln!(out, "DIFF {key}\n  A: {x}\n  B: {y}")?;
            }
            (Some(x), None) => {
                diffs += 1;
                writeln!(out, "ONLY A {key}: {x}")?;
            }
            (None, Some(y)) => {
                diffs += 1;
                writeln!(out, "ONLY B {key}: {y}")?;
            }
            (None, None) => {}
        }
    }
    writeln!(out, "{diffs} lines differ")?;
    Ok(diffs)
}

fn load(paths: &[String]) -> Result<Vec<PadRecord>> {
    let mut records = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|e| Error(format!("{path}: {e}")))?;
        for (_, binary) in assemble_pages(&text)? {
            records.push(parse_record(&binary)?);
        }
    }
    records.sort_by_key(|r| (r.header.boot_id, r.header.record_no));
    Ok(records)
}

/// `psoxide-hwtest pad-report PAGEFILE ...`: every controller's report, then the diff of each
/// pair of consecutive controllers.
pub fn report_command(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let records = load(args)?;
    ensure!(!records.is_empty(), "no PXC record found in the page files");
    for r in &records {
        print_record(r, out)?;
    }
    for pair in records.windows(2) {
        compare(&pair[0], &pair[1], out)?;
    }
    Ok(0)
}

/// `psoxide-hwtest pad-compare A B`: the diff of the first controller of each page file.
pub fn compare_command(args: &[String], out: &mut dyn Write) -> Result<i32> {
    ensure!(args.len() == 2, "pad-compare needs two page files");
    let a = load(&args[..1])?;
    let b = load(&args[1..])?;
    ensure!(!a.is_empty() && !b.is_empty(), "no PXC record in one of the files");
    compare(&a[0], &b[0], out)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::record::*;
    use super::*;

    fn sample_txn(step: u8, prof: u8, id: u8, short: bool) -> Txn {
        let mut t = Txn::EMPTY;
        t.step = step;
        t.prof = prof;
        t.seq = 1;
        let len = if id == 0x41 { 5 } else { 9 };
        t.sent = if short { 3 } else { len };
        t.got = t.sent;
        let reply = [0xFF, id, 0x5A, 0xFF, 0xFF, 0x80, 0x7F, 0x80, 0x80];
        for i in 0..usize::from(t.sent) {
            t.tx[i] = if i == 1 { 0x42 } else { 0 };
            t.rx[i] = reply[i];
            t.rise[i] = if i + 1 < usize::from(len) { 1600 + i as u16 } else { 0xFFFF };
            t.width[i] = 90;
        }
        if short {
            t.flags = 2;
        }
        t
    }

    fn view(class: u8, mode: u8, buttons: u16) -> PollView {
        let mut v = PollView::ZERO;
        v.n = 9;
        v.rx = [0xFF, 0x73, 0x5A, 0xFF, 0xFF, 0x80, 0x81, 0x7F, 0x80];
        v.mode = mode;
        v.flags = 3;
        v.buttons = buttons;
        v.sticks = [0x80, 0x81, 0x7F, 0x80];
        v.class = class;
        v.acked = 8;
        v
    }

    /// A record built with the guest's own encoder: header, two transactions, a window, a soak
    /// and a hand-over.
    fn sample_record(port: u8, no: u8, short: bool) -> Vec<u8> {
        let mut buf = vec![0u8; 8192];
        let mut w = Writer::new(&mut buf);
        put_header(
            &mut w,
            &Header {
                suite_major: 2,
                suite_minor: 21,
                record_no: no,
                boot_id: 0xA5,
                port,
                kind: 0,
                flags: 0,
                sdk_rev: *b"9bb3045b",
                timer_hz: 33_868_800,
                ack_timeout: 40_000,
            },
        );
        let sec = w.begin_section(TAG_TXN);
        put_txn(&mut w, &sample_txn(1, 0, 0x73, false));
        put_txn(&mut w, &sample_txn(13, 2, 0x73, short));
        w.end_section(sec);
        let mut win = Window::EMPTY;
        win.phase = 3;
        win.frames_planned = 360;
        win.frames_run = 360;
        win.raw_ok = 360;
        win.raw_or = 0x4000;
        win.raw_axis[2] = Axis { min: 0x10, max: 0xF0, samples: [1, 2, 3, 4] };
        win.sdk_ok = 360;
        win.sdk_or = 0x4000;
        win.sdk_min = [0x80; 4];
        win.sdk_max = [0x80; 4];
        push_id(&mut win.raw_ids, &mut win.raw_id_n, 0x73);
        push_mode(&mut win.sdk_modes, &mut win.sdk_mode_n, 2, true);
        let sec = w.begin_section(TAG_WINDOW);
        put_window(&mut w, &win);
        w.end_section(sec);
        let mut soak = Soak::EMPTY;
        soak.kind = 1;
        for i in 0..300u16 {
            let bad = short && i > 10;
            let mut v = view(u8::from(bad), 2, 0);
            if bad {
                v.n = 5;
                v.acked = 3;
            }
            soak.note(v, &Wire::NONE, bad, bad, false, 900);
        }
        let sec = w.begin_section(TAG_SOAK);
        put_soak(&mut w, &soak);
        w.end_section(sec);
        let mut ho = Handover::EMPTY;
        ho.variant = 1;
        ho.launcher_mode = 2;
        ho.launcher_analog = true;
        for i in 0..20 {
            ho.note(i, view(0, 2, 0), false);
        }
        let sec = w.begin_section(TAG_HANDOVER);
        put_handover(&mut w, &ho);
        w.end_section(sec);
        assert!(!w.overflow);
        let mut bytes = w.bytes().to_vec();
        let crc = crc32(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes
    }

    /// Pages as the guest cuts them: 621 bytes of binary a page, `PXC/PPTTRRRR/<b64>/C:crc`.
    fn pages(binary: &[u8], run: u16) -> String {
        let step = CHARS_PER_PAGE / 4 * 3;
        let total = binary.len().div_ceil(step);
        let mut text = String::new();
        for (i, chunk) in binary.chunks(step).enumerate() {
            let b64 = crate::util::base64_encode(chunk);
            text.push_str(&format!(
                "PXC/{:02X}{:02X}{:04X}/{}/C:{:08X}\n",
                i + 1,
                total,
                run,
                b64,
                crc32(b64.as_bytes())
            ));
        }
        text
    }

    #[test]
    fn a_record_survives_encode_pages_and_decode() {
        let binary = sample_record(0, 1, false);
        let text = pages(&binary, 0xA501);
        let got = assemble_pages(&text).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 0xA501);
        assert_eq!(got[0].1, binary);
        let rec = parse_record(&got[0].1).unwrap();
        assert_eq!(rec.header.record_no, 1);
        assert_eq!(rec.header.timer_hz, 33_868_800);
        assert_eq!(&rec.header.sdk_rev, b"9bb3045b");
        assert_eq!(rec.txns.len(), 2);
        assert_eq!(rec.txns[0].rx[1], 0x73);
        assert_eq!(rec.txns[1].seq, 1);
        assert_eq!(rec.windows[0].raw_axis[2].max, 0xF0);
        assert_eq!(rec.soaks[0].classes[0], 300);
        assert_eq!(rec.soaks[0].run_n, 1);
        assert_eq!(rec.handovers[0].launcher_mode, 2);
        assert!(rec.handovers[0].launcher_analog);
    }

    #[test]
    fn a_damaged_page_is_refused_and_a_second_copy_repairs_it() {
        let binary = sample_record(0, 1, false);
        let good = pages(&binary, 0xA501);
        let mut bad = good.clone();
        bad = bad.replacen("PXC/01", "PXC/01", 1);
        let idx = bad.find('/').unwrap() + 12;
        bad.replace_range(idx..idx + 1, if &bad[idx..idx + 1] == "A" { "B" } else { "A" });
        assert!(assemble_pages(&bad).is_err());
        assert!(assemble_pages(&format!("{bad}{good}")).is_ok());
    }

    #[test]
    fn a_record_with_a_wrong_binary_crc_is_refused() {
        let mut binary = sample_record(0, 1, false);
        let n = binary.len();
        binary[n - 1] ^= 1;
        let text = pages(&binary, 0xA501);
        assert!(assemble_pages(&text).is_err());
    }

    #[test]
    fn two_controllers_in_one_file_are_told_apart_and_diffed() {
        let a = sample_record(0, 1, false);
        let b = sample_record(1, 2, true);
        let text = format!("{}{}", pages(&a, 0xA501), pages(&b, 0xA502));
        let got = assemble_pages(&text).unwrap();
        assert_eq!(got.len(), 2);
        let ra = parse_record(&got[0].1).unwrap();
        let rb = parse_record(&got[1].1).unwrap();
        let mut out = Vec::new();
        let diffs = compare(&ra, &rb, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(diffs >= 2, "{out}");
        assert!(out.contains("soak.1"), "{out}");
        assert!(out.contains("died at byte 3"), "{out}");
    }

    #[test]
    fn the_report_prints_the_survey_the_windows_and_the_verdict_lines() {
        let rec = parse_record(&sample_record(0, 1, true)).unwrap();
        let mut out = Vec::new();
        print_record(&rec, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("CONTROLLER 1"));
        assert!(out.contains("STICKS MOVE ON THE WIRE BUT THE SDK REPORTS THEM DEAD"), "{out}");
        assert!(out.contains("short 289 (died at byte 3)"), "{out}");
        assert!(out.contains("POLL 42 AFTER EXIT"), "{out}");
    }

    #[test]
    fn an_overflowing_record_is_flagged_not_cut_silently() {
        let mut buf = [0u8; 16];
        let mut w = Writer::new(&mut buf);
        w.put(&[1; 12]);
        assert!(!w.overflow);
        w.put(&[2; 8]);
        assert!(w.overflow);
        w.u8(3);
        assert_eq!(w.len(), 12);
    }

    fn power_note(lab: u8, class: u8, buttons: u16, analog: bool) -> PollNote {
        let mut n = PollNote::default();
        n.lab = lab;
        n.first_class = class;
        n.first_id = if class == 5 { 0xFF } else { 0x41 };
        n.first_died = if class == 0 { 0xFF } else { 3 };
        n.class = class;
        n.id = 0x73;
        n.n = 9;
        n.acked = if class == 0 { 8 } else { 3 };
        n.died = n.first_died;
        n.tries = 1;
        n.rx = [0xFF, 0x73, 0x5A, 0xFF, 0xFF, 0x80, 0x80, 0x80, 0x80];
        n.mode = 2;
        n.flags = if analog { 7 } else { 5 };
        n.buttons = buttons;
        n.sticks = [0x80; 4];
        n.why = 1;
        n
    }

    fn sample_power_record() -> Vec<u8> {
        let mut p = PowerOn::EMPTY;
        // The handshake poll, then polls that complete, one that did not, then a pushed stick.
        let mut first = power_note(0, 0, 0, false);
        first.cfg = 3;
        first.exits = 1;
        first.fresh = 1 | (2 << 1);
        p.note(first, 2);
        for i in 0..40 {
            p.note(power_note(0, 0, 0, true), 260 + i % 3);
        }
        p.note(power_note(0, 5, 0, true), 262);
        for _ in 0..10 {
            let mut n = power_note(1, 0, 0x0800, true);
            n.fold = 2;
            n.why = 0;
            p.note(n, 0);
        }
        p.counters = Counters { ok: 50, bad: 1, nak: 1, retry: 1, handshakes: 1, cfg: 3, exits: 1, exits_f3: 0 };
        p.frames = 70;
        let mut lab = LabResult::default();
        lab.phase = 4;
        lab.frames = 480;
        lab.polls = 480;
        lab.ok = 470;
        lab.bad = 10;
        lab.first_bad = 10;
        lab.btn_or = 0x0800;
        lab.last_id = 0x73;
        lab.last_mode = 2;
        lab.last_flags = 7;
        lab.raw_ok = 3;
        lab.raw_total = 3;
        lab.status45 = [0x01, 0x02, 0x01, 0x02, 0x01, 0x00];
        lab.status45_set = 1;
        p.lab_result(lab);
        let mut buf = vec![0u8; 11 * 1024];
        let mut w = Writer::new(&mut buf);
        put_header(
            &mut w,
            &Header {
                suite_major: 2,
                suite_minor: 22,
                record_no: 0,
                boot_id: 0xA5,
                port: 0,
                kind: 2,
                flags: 0,
                sdk_rev: *b"6e25f2ae",
                timer_hz: 33_868_800,
                ack_timeout: 40_000,
            },
        );
        let sec = w.begin_section(TAG_POWERON);
        put_poweron(&mut w, &p);
        w.end_section(sec);
        assert!(!w.overflow);
        let mut bytes = w.bytes().to_vec();
        let crc = crc32(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn the_power_on_record_survives_pages_and_decodes_with_its_lab_lines() {
        let binary = sample_power_record();
        let got = assemble_pages(&pages(&binary, 0xA500)).unwrap();
        assert_eq!(got.len(), 1);
        let rec = parse_record(&got[0].1).unwrap();
        assert_eq!(rec.header.kind, 2);
        let p = rec.power.as_ref().expect("power section");
        assert_eq!(p.polls, 52);
        // Four runs: the handshake poll (it differs by its config packets), 40 complete polls,
        // the failed one, the ten with a fold.
        assert_eq!(p.run_n, 4);
        assert_eq!(p.runs[1].count, 40);
        assert_eq!(p.runs[1].hold_min, 260);
        assert_eq!(p.runs[1].hold_max, 262);
        assert_eq!(p.runs[0].note.cfg, 3);
        assert_eq!(p.runs[2].note.class, 5);
        assert_eq!(p.runs[3].note.fold, 2);
        assert_eq!(p.counters.exits, 1);
        assert_eq!(p.lab_n, 1);
        assert_eq!(p.lab[0].status45, [1, 2, 1, 2, 1, 0]);
        let mut out = Vec::new();
        print_record(&rec, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("POWER-ON RECORD"), "{out}");
        assert!(out.contains("poll 0 (the handshake from power)"), "{out}");
        assert!(out.contains("ADDR NOT ACKED"), "{out}");
        assert!(out.contains("as a good DualShock answers"), "{out}");
        assert!(out.contains("first poll reported analog and locked: 1"), "{out}");
        assert!(out.contains("polls 1-40 lab 1"), "{out}");
        assert!(out.contains("fold 0010"), "{out}");
    }

    #[test]
    fn the_power_on_notes_are_run_length_coded_and_stop_at_600() {
        let mut p = PowerOn::EMPTY;
        for _ in 0..700 {
            p.note(power_note(0, 0, 0, true), 262);
        }
        assert_eq!(p.polls, POWER_POLLS);
        assert_eq!(p.run_n, 1);
        assert_eq!(p.runs[0].count, 600);
        // A table of 150 runs is full: the next different poll is dropped and counted.
        let mut q = PowerOn::EMPTY;
        for i in 0..200u16 {
            q.note(power_note(0, 0, i, true), 1);
        }
        assert_eq!(usize::from(q.run_n), POWER_RUNS);
        assert_eq!(q.dropped, 50);
        // Worst case still fits a record.
        let mut buf = vec![0u8; 11 * 1024];
        let mut w = Writer::new(&mut buf);
        put_poweron(&mut w, &q);
        assert!(!w.overflow, "{} bytes", w.len());
    }

    #[test]
    fn a_v2_21_record_with_schema_1_still_decodes_and_soak_kind_3_is_soak_0() {
        let mut binary = sample_record(0, 1, false);
        binary[4] = 1;
        let n = binary.len();
        let crc = crc32(&binary[..n - 4]);
        binary[n - 4..].copy_from_slice(&crc.to_le_bytes());
        assert!(parse_record(&binary).is_ok());
        assert_eq!(soak_label(3), 0);
        assert_eq!(soak_label(0), 1);
        assert_eq!(NOTE_LEN, 32);
    }

    #[test]
    fn the_constants_the_guest_pages_use_match_this_reader() {
        assert_eq!(CHARS_PER_PAGE, 828);
        assert_eq!(PAGE_SCHEMA, "PXC");
        assert_eq!(HEADER_LEN, 32);
    }
}
