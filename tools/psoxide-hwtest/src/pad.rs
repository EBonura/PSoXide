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
    Axis, Failure, Handover, Header, IdRun, ModeRun, PollRun, PollView, Soak, Txn, Window, Wire,
    HANDOVER_POLLS_MAX, ID_RUNS, MODE_RUNS, SOAK_FAILS, SOAK_POLLS_MAX, SOAK_RUNS, TXN_MAX,
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
    ensure!(schema == record::SCHEMA, "schema {schema}, this reader knows {}", record::SCHEMA);
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
            if h.kind == 0 { "a pad answered" } else { "NOTHING ANSWERED" },
            h.suite_major,
            h.suite_minor,
            rev(h),
            h.timer_hz,
            h.ack_timeout,
            if h.flags & 1 != 0 { ", RECORD TRUNCATED" } else { "" }
        ),
    ));
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
            s.kind + 1,
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
        out.push((format!("soak.{}.ack", s.kind), format!("soak {} ACK rise-ticks/width per byte: {}; acked histogram {:?}; latch-only {}", s.kind + 1, rise.join(" "), s.acked_hist, s.latch_polls)));
        for f in &s.fails {
            let _ = f;
        }
        for f in s.fails.iter().take(usize::from(s.fail_n)) {
            out.push((
                format!("soak.{}.fail.{}", s.kind, f.poll),
                format!(
                    "soak {} failure at poll {}: wire {} class {} acked {} early {} irq-after-clear {}, reported {} buttons {:04X}",
                    s.kind + 1,
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

fn mode_name(mode: u8) -> &'static str {
    ["none", "digital", "analog", "negcon", "config", "unknown"].get(usize::from(mode)).copied().unwrap_or("?")
}

/// Print the full report of one record: the summary, then the transaction log.
pub fn print_record(rec: &PadRecord, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "== CONTROLLER {} (port {}) ==", rec.header.record_no, rec.header.port + 1)?;
    for (_, line) in summarise(rec) {
        writeln!(out, "{line}")?;
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

    #[test]
    fn the_constants_the_guest_pages_use_match_this_reader() {
        assert_eq!(CHARS_PER_PAGE, 828);
        assert_eq!(PAGE_SCHEMA, "PXC");
        assert_eq!(HEADER_LEN, 32);
    }
}
