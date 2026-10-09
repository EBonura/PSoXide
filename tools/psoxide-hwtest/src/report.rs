//! Validate and assemble PSoXide hardware-test photo payloads.
//!
//! PX7 carries a per-record median and explicit record ids, so a probe can be
//! added without shifting the meaning of every later record. PX8 adds per-block
//! flags and a variable page count. PX5 and PX6 captures are no longer parsed:
//! their timing records were positional and no such capture is archived.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use crate::tables::{LABELS, LAYOUT_IMMUNE_RECORDS, WORK_BY_ID};
use crate::util::{
    base64_decode, crc32, parse_hex, split_words, splitlines, Error, Reader, Result,
};

// PX7: explicit per-record ids and a median column. The slot count comes from
// the header: early v1.5 captures carried 144 slots, later ones 176.
pub const PX7_PRECISION_COUNT: usize = 192;
pub const PX7_STATUS_BITS: u8 = 3;
pub const PX7_SCAN_COUNT: u8 = 3;
pub const PX7_MEMORY_CONTROL_COUNT: usize = 9;
pub const PX7_RECORD_UNUSED: u32 = 0xFF;
// PX8: per-block flags. A routine capture carries verdicts and one record per
// FAILING case; a characterisation capture carries every block PX7 did, in the
// same field order, so an archived px7-* reference still describes the same
// run a full PX8 does. Counts widened to u16 and the page count is no longer
// fixed.
pub const PX8_BLOCK_STATUS: u8 = 1 << 0;
pub const PX8_BLOCK_FAILURES: u8 = 1 << 1;
pub const PX8_BLOCK_OBSERVED: u8 = 1 << 2;
pub const PX8_BLOCK_TIMING: u8 = 1 << 3;
pub const PX8_BLOCK_MEMCTL: u8 = 1 << 4;
pub const PX8_BLOCK_PRECISION: u8 = 1 << 5;
pub const PX8_BLOCK_TIMING_EXT: u8 = 1 << 6;
pub const SCHEMAS: [&str; 2] = ["PX7", "PX8"];
/// In capture order. The first nine are 0x1F801000..0x1F801020; later suites
/// append registers that are not on that linear run. A value past the end of
/// this table still prints, under a positional name, rather than vanishing.
pub const MEMORY_CONTROL_NAMES: [&str; 11] = [
    "exp1_base",
    "exp2_base",
    "exp1_delay",
    "exp3_delay",
    "bios_delay",
    "spu_delay",
    "cdrom_delay",
    "exp2_delay",
    "common_delay",
    "ram_size",
    "cache_control",
];
pub const STATUS_LABELS: [&str; 5] = ["PENDING", "PASS", "FAIL", "WARN", "INFO"];

pub const GTE_SETTLE_FIRST_CASE: usize = 116;
pub const GTE_SETTLE_CASE_COUNT: usize = 22;

// v1.24 list-busy probe (list_busy_probes.rs, case ids 0xD3-0xE9). Conformance
// cases travel by index, so these name indices 211-233. Stamps are Timer 2
// clocks from the DMA kick, 0xFFFFFFFF for an event never seen. A packed loop
// case carries walk iterations in bits 0-15 and idle clocks for 256 iterations
// in bits 16-31.
pub const LIST_BUSY_FIRST_CASE: usize = 211;
const LIST_BUSY_KINDS: [&str; 4] = ["empty", "cheap", "expensive", "packed"];
const LIST_BUSY_EVENTS: [&str; 4] = [
    "chcr_clear",
    "gp0_1f_irq",
    "gpustat28_settled",
    "gpustat26_settled",
];
const LIST_BUSY_TAIL: [&str; 7] = [
    "packed_list_pixels_match",
    "alu_loop_iterations",
    "alu_loop_walk_clocks",
    "ram_load_loop_iterations",
    "ram_load_loop_walk_clocks",
    "scratchpad_load_loop_iterations",
    "scratchpad_load_loop_walk_clocks",
];
const LIST_BUSY_NOT_SEEN: u32 = 0xFFFF_FFFF;

pub fn list_busy_labels() -> Vec<String> {
    let mut labels = Vec::new();
    for kind in LIST_BUSY_KINDS {
        for event in LIST_BUSY_EVENTS {
            labels.push(format!("{kind}_list_{event}"));
        }
    }
    labels.extend(LIST_BUSY_TAIL.iter().map(|label| label.to_string()));
    labels
}

// v1.25 FMV STREAM TEST records: (min, median, max) field names per record.
pub const FMV_FIRST_RECORD: u32 = 0x1F0;
pub const FMV_FIELDS: [[&str; 3]; 6] = [
    ["pass", "good_sectors", "total_sectors"],
    ["lost_sectors", "bad_sectors", "dropped_frames"],
    ["cd_errors", "decode_errors", "first_error_lba"],
    ["frames_shown", "frames_late", "vblanks"],
    ["kcyc_vlc", "kcyc_mdec_upload", "kcyc_wait"],
    ["last_good_lba", "runs", "setup_error"],
];
/// first_error_lba when nothing went wrong.
const FMV_NO_ERROR_LBA: i64 = 0xFFFF;
pub const FMV_SETUP_ERRORS: [&str; 6] = [
    "none",
    "cd prepare",
    "MOVIE.STR not found",
    "cd xa mode",
    "mdec tables",
    "cd start",
];

// v1.26 MDEC DIAGNOSTIC (src/fmv_diag.rs, `records` documents the layout).
const MDEC_SEQUENCES: [&str; 6] = [
    "A control v1.25",
    "B settle then enable",
    "C fixed delay",
    "D cpu tables",
    "E psn00bsdk order",
    "F sdk driver",
];
const MDEC_STEPS: [&str; 7] = [
    "none",
    "reset settle",
    "quant upload",
    "scale upload",
    "idle after tables",
    "probe dma0 in",
    "probe dma1 out",
];
const MDEC_SNAPSHOTS: [&str; 6] = [
    "before_reset",
    "after_reset",
    "after_enable",
    "after_command",
    "after_tables",
    "after_probe",
];
const MDEC_STOPS: [&str; 5] = ["end", "stall", "wedged", "cd error", "setup"];
const MDEC_TRACES: [&str; 3] = ["from_idle", "from_busy", "from_busy_with_enable"];
const MDEC_NEVER: i64 = 0xFFFF;

// v1.27 CONSOLE TESTS (src/console_tests.rs): the field names of each record.
pub const CONSOLE_KERNEL_FIRST: u32 = 0x2C0;
pub const CONSOLE_KERNEL: [[&str; 3]; 6] = [
    ["enter_cs_min", "enter_cs_med", "enter_cs_max"],
    ["exit_cs_min", "exit_cs_med", "exit_cs_max"],
    ["empty_call_min", "empty_call_med", "empty_call_max"],
    ["bios_vblank_min", "bios_vblank_med", "bios_vblank_max"],
    ["bios_vblank_gaps", "bios_vblank_events_ready", "flags"],
    ["runtime_vblank_min", "runtime_vblank_med", "runtime_vblank_max"],
];
pub const CONSOLE_WIDTHS: [u32; 6] = [256, 320, 368, 384, 512, 640];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub record_id: u32,
    pub work: u32,
    pub minimum: i64,
    pub maximum: i64,
    /// -1 means "not carried"; every supported schema carries a median.
    pub median: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturePage {
    pub schema: String,
    pub number: u32,
    pub total: u32,
    pub chunk: String,
    pub crc: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    pub status: u8,
    pub items: u32,
    pub digest: u32,
    pub aux: u32,
    pub run: u8,
}

/// One case a capture spent bytes on because it did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub test_id: u32,
    pub expected: u32,
    pub observed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub schema: String,
    pub version: u8,
    /// Which blocks the capture carried. Older schemas always carried them all.
    pub flags: u8,
    pub failures: Vec<Failure>,
    /// How many pages this capture actually took. Fixed per schema before PX8.
    pub page_count: u32,
    /// Suite version: what the record ids MEAN.
    pub suite_major: u8,
    pub suite_minor: u8,
    pub conformance_run: u8,
    pub timing_run: u8,
    pub conformance_digest: u32,
    pub gte_digest: u32,
    pub timing_digest: u32,
    pub timing_aux: u32,
    pub scans: Vec<ScanSummary>,
    pub observations: Vec<u32>,
    pub statuses: Vec<u8>,
    pub records: Vec<Record>,
    pub memory_control: Vec<u32>,
    pub precision: Vec<u32>,
    pub binary_crc: u32,
}

pub fn label(record_id: u32) -> Option<&'static str> {
    LABELS
        .binary_search_by_key(&(record_id as u64), |(id, _)| u64::from(*id))
        .ok()
        .map(|at| LABELS[at].1)
}

pub fn work(record_id: u32) -> Option<u32> {
    WORK_BY_ID
        .binary_search_by_key(&(record_id as u64), |(id, _)| u64::from(*id))
        .ok()
        .map(|at| WORK_BY_ID[at].1)
}

fn layout_immune(record_id: u32) -> bool {
    LAYOUT_IMMUNE_RECORDS.contains(&(record_id as u16))
}

pub fn memory_control_name(index: usize) -> String {
    match MEMORY_CONTROL_NAMES.get(index) {
        Some(name) => name.to_string(),
        None => format!("register_{index:02}"),
    }
}

fn starts_with_schema(text: &str) -> bool {
    SCHEMAS.iter().any(|name| {
        text.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

pub fn parse_capture_page(payload: &str) -> Result<CapturePage> {
    let payload = payload.trim();
    ensure!(
        starts_with_schema(payload),
        "not a PX7/PX8 hardware payload"
    );
    let malformed = || Error("malformed capture page".into());
    let (body, claimed_crc) = payload.rsplit_once("/C:").ok_or_else(malformed)?;
    let mut parts = body.splitn(3, '/');
    let (marker, page_field, chunk) = match (parts.next(), parts.next(), parts.next()) {
        (Some(marker), Some(page_field), Some(chunk)) => (marker, page_field, chunk),
        _ => return Err(malformed()),
    };
    ensure!(
        SCHEMAS.contains(&marker) && page_field.len() == 4 && !chunk.is_empty(),
        "malformed capture page header"
    );
    ensure!(chunk.is_ascii(), "non-ASCII capture page");
    let actual_crc = crc32(chunk.as_bytes());
    ensure!(
        parse_hex(claimed_crc)? == u64::from(actual_crc),
        "{marker} page CRC mismatch: payload says {claimed_crc}, calculated {actual_crc:08X}"
    );
    Ok(CapturePage {
        schema: marker.to_string(),
        number: parse_hex(&page_field[..2])? as u32,
        total: parse_hex(&page_field[2..])? as u32,
        chunk: chunk.to_string(),
        crc: actual_crc,
    })
}

pub fn parse_capture(payloads: &[String]) -> Result<Capture> {
    let pages: Vec<CapturePage> = payloads
        .iter()
        .map(|payload| parse_capture_page(payload))
        .collect::<Result<_>>()?;
    ensure!(!pages.is_empty(), "no PX7/PX8 payloads found");
    let mut schemas: Vec<&str> = pages.iter().map(|page| page.schema.as_str()).collect();
    schemas.sort_unstable();
    schemas.dedup();
    ensure!(schemas.len() == 1, "capture mixes schema versions");
    let schema = schemas[0].to_string();
    let mut totals: Vec<u32> = pages.iter().map(|page| page.total).collect();
    totals.sort_unstable();
    totals.dedup();
    // The page count is whatever the pages agree it is: a capture costs as many
    // pages as it has data.
    ensure!(
        totals.len() == 1,
        "{schema} pages disagree about how many pages there are"
    );
    let page_count = totals[0];
    // The log can contain an early boot page followed by a freshly encoded page
    // after the pad state settles. Keep the last occurrence, matching the state
    // that is ultimately photographed.
    let mut by_number: HashMap<u32, &CapturePage> = HashMap::new();
    for page in &pages {
        by_number.insert(page.number, page);
    }
    let missing: Vec<String> = (1..=page_count)
        .filter(|number| !by_number.contains_key(number))
        .map(|number| number.to_string())
        .collect();
    ensure!(
        missing.is_empty(),
        "missing {schema} page(s): {}",
        missing.join(", ")
    );

    let encoded: String = (1..=page_count)
        .map(|number| by_number[&number].chunk.as_str())
        .collect();
    let binary = base64_decode(&encoded).map_err(|e| Error(format!("invalid {schema} Base64: {e}")))?;
    ensure!(binary.len() >= 4, "{schema} binary is truncated");
    let claimed_binary_crc = u32::from_le_bytes(binary[binary.len() - 4..].try_into().expect("4"));
    let actual_binary_crc = crc32(&binary[..binary.len() - 4]);
    ensure!(
        claimed_binary_crc == actual_binary_crc,
        "{schema} binary CRC mismatch: payload says {claimed_binary_crc:08X}, calculated {actual_binary_crc:08X}"
    );

    ensure!(
        binary.len() >= 5 && binary[..4] == *format!("{schema}B").as_bytes(),
        "{schema} binary magic mismatch"
    );
    let version = binary[4];
    let expected_version = if schema == "PX7" { 3 } else { 4 };
    ensure!(
        version == expected_version,
        "unsupported {schema} binary version {version}"
    );
    // PX7 inserted the suite version after the schema version, so every later
    // header field shifts by two bytes. PX8 adds a flags byte and widens the
    // four counts to u16.
    let mut flags = PX8_BLOCK_STATUS
        | PX8_BLOCK_OBSERVED
        | PX8_BLOCK_TIMING
        | PX8_BLOCK_MEMCTL
        | PX8_BLOCK_PRECISION;
    let mut header = Reader::new(&binary, 5);
    let suite_major = header.u8()?;
    let suite_minor = header.u8()?;
    let (conformance_run, timing_run, test_count, timing_count, memory_count, status_bits, scan_count);
    let mut precision_count = 0usize;
    let digest_offset;
    if schema == "PX8" {
        flags = header.u8()?;
        conformance_run = header.u8()?;
        timing_run = header.u8()?;
        test_count = usize::from(header.u16()?);
        timing_count = usize::from(header.u16()?);
        memory_count = usize::from(header.u16()?);
        precision_count = usize::from(header.u16()?);
        status_bits = header.u8()?;
        scan_count = header.u8()?;
        digest_offset = 20;
    } else {
        conformance_run = header.u8()?;
        timing_run = header.u8()?;
        test_count = usize::from(header.u8()?);
        timing_count = usize::from(header.u8()?);
        memory_count = usize::from(header.u8()?);
        status_bits = header.u8()?;
        scan_count = header.u8()?;
        ensure!(
            (memory_count, status_bits, scan_count)
                == (PX7_MEMORY_CONTROL_COUNT, PX7_STATUS_BITS, PX7_SCAN_COUNT),
            "PX7 binary shape does not match schema version {version}"
        );
        digest_offset = 14;
    }
    let mut reader = Reader::new(&binary, digest_offset);
    let conformance_digest = reader.u32()?;
    let gte_digest = reader.u32()?;
    let timing_digest = reader.u32()?;
    let timing_aux = reader.u32()?;
    let mut scans = Vec::new();
    for _ in 0..scan_count {
        let status = reader.u8()?;
        let items = u32::from(reader.u16()?);
        let digest = reader.u32()?;
        let aux = reader.u32()?;
        let run = reader.u8()?;
        scans.push(ScanSummary {
            status,
            items,
            digest,
            aux,
            run,
        });
    }

    // PX8 writes the status bitmap first and the observations last, because the
    // bitmap is the block a routine capture always has and the observations are
    // the block it usually omits. Older schemas had one fixed order.
    let mut statuses: Vec<u8> = Vec::new();
    let mut failures = Vec::new();
    let mut observations: Vec<u32> = Vec::new();
    let mut packed_statuses: &[u8] = &[];
    let packed_status_len = (test_count * usize::from(status_bits)).div_ceil(8);
    if schema == "PX8" {
        if flags & PX8_BLOCK_STATUS != 0 {
            packed_statuses = reader.bytes(packed_status_len)?;
        }
        if flags & PX8_BLOCK_FAILURES != 0 {
            let failure_count = reader.u16()?;
            for _ in 0..failure_count {
                let test_id = u32::from(reader.u16()?);
                let expected = reader.u32()?;
                let observed = reader.u32()?;
                failures.push(Failure {
                    test_id,
                    expected,
                    observed,
                });
            }
        }
        if flags & PX8_BLOCK_OBSERVED != 0 {
            observations = reader.u32s(test_count)?;
        }
    } else {
        observations = reader.u32s(test_count)?;
        packed_statuses = reader.bytes(packed_status_len)?;
    }
    if !packed_statuses.is_empty() {
        for index in 0..test_count {
            let bit = index * usize::from(status_bits);
            let mut window = u32::from(packed_statuses[bit / 8]);
            if bit / 8 + 1 < packed_statuses.len() {
                window |= u32::from(packed_statuses[bit / 8 + 1]) << 8;
            }
            let status = ((window >> (bit % 8)) & 0x7) as u8;
            ensure!(status <= 4, "invalid {schema} status {status} for case {index}");
            statuses.push(status);
        }
    }

    let mut records: Vec<Record> = Vec::new();
    let push_record = |records: &mut Vec<Record>, record_id: u32, minimum: u16, median: u16, maximum: u16| {
        records.push(Record {
            record_id,
            work: work(record_id).unwrap_or(0),
            minimum: i64::from(minimum),
            maximum: i64::from(maximum),
            median: i64::from(median),
        });
    };
    if flags & PX8_BLOCK_TIMING != 0 {
        // Ids are explicit, so an unfilled slot is skipped rather than shifting
        // every later record's meaning.
        for _ in 0..timing_count {
            let record_id = u32::from(reader.u8()?);
            let minimum = reader.u16()?;
            let median = reader.u16()?;
            let maximum = reader.u16()?;
            if record_id == PX7_RECORD_UNUSED {
                continue;
            }
            push_record(&mut records, record_id, minimum, median, maximum);
        }
    }
    let mut memory_control = Vec::new();
    if schema != "PX8" || flags & PX8_BLOCK_MEMCTL != 0 {
        memory_control = reader.u32s(memory_count)?;
    }
    let mut precision = Vec::new();
    if schema == "PX8" {
        if flags & PX8_BLOCK_PRECISION != 0 {
            precision = reader.u32s(precision_count)?;
        }
    } else {
        precision = reader.u32s(PX7_PRECISION_COUNT)?;
    }
    if schema == "PX8" && flags & PX8_BLOCK_TIMING_EXT != 0 {
        // Records whose id does not fit a byte. Same fields, wider id.
        let extended_count = reader.u16()?;
        for _ in 0..extended_count {
            let record_id = u32::from(reader.u16()?);
            let minimum = reader.u16()?;
            let median = reader.u16()?;
            let maximum = reader.u16()?;
            push_record(&mut records, record_id, minimum, median, maximum);
        }
    }
    ensure!(
        reader.offset == binary.len() - 4,
        "{schema} binary parser did not consume the complete payload"
    );

    Ok(Capture {
        schema,
        version,
        flags,
        failures,
        page_count,
        suite_major,
        suite_minor,
        conformance_run,
        timing_run,
        conformance_digest,
        gte_digest,
        timing_digest,
        timing_aux,
        scans,
        observations,
        statuses,
        records,
        memory_control,
        precision,
        binary_crc: claimed_binary_crc,
    })
}

fn payloads_from_text(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in splitlines(text) {
        let position = SCHEMAS.iter().filter_map(|name| line.find(&format!("{name}/"))).min();
        if let Some(position) = position {
            if let Some(word) = split_words(&line[position..]).first() {
                found.push(word.to_string());
            }
        }
    }
    found
}

/// Payload lines out of the given values (a payload itself, or a file holding
/// some); with no values, standard input.
pub fn payloads_from_paths(paths: &[String]) -> Result<Vec<String>> {
    let mut payloads = Vec::new();
    for value in paths {
        if starts_with_schema(value) {
            payloads.push(value.clone());
            continue;
        }
        let bytes = std::fs::read(value).map_err(|e| Error(format!("{value}: {e}")))?;
        let text = String::from_utf8(bytes).map_err(|e| Error(e.to_string()))?;
        payloads.extend(payloads_from_text(&text));
    }
    if paths.is_empty() {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        payloads.extend(payloads_from_text(&text));
    }
    Ok(payloads)
}

pub fn list_busy_rows(capture: &Capture) -> Vec<String> {
    let labels = list_busy_labels();
    let end = LIST_BUSY_FIRST_CASE + labels.len();
    if capture.observations.len() < end {
        return Vec::new();
    }
    let mut rows = vec!["list_busy,label,value".to_string()];
    for (offset, label) in labels.iter().enumerate() {
        let index = LIST_BUSY_FIRST_CASE + offset;
        let value = capture.observations[index];
        if label == "packed_list_pixels_match" {
            let status = capture.statuses.get(index).copied().unwrap_or(0);
            rows.push(format!("{index},{label},{}", STATUS_LABELS[usize::from(status)]));
        } else if let Some(loop_name) = label.strip_suffix("_iterations") {
            rows.push(format!("{index},{loop_name}_walk_iterations,{}", value & 0xFFFF));
            rows.push(format!("{index},{loop_name}_idle_clocks_per_256,{}", value >> 16));
        } else if value == LIST_BUSY_NOT_SEEN {
            rows.push(format!("{index},{label},never"));
        } else {
            rows.push(format!("{index},{label},{value}"));
        }
    }
    rows
}

fn by_id(records: &[Record]) -> HashMap<u32, &Record> {
    records.iter().map(|record| (record.record_id, record)).collect()
}

fn triple(record: &Record) -> (i64, i64, i64) {
    (record.minimum, record.median, record.maximum)
}

/// MDEC1 status decoded per psx-spx.
fn mdec_status_text(value: u64) -> String {
    let flags: Vec<&str> = [
        (31, "out_empty"),
        (30, "in_full"),
        (29, "busy"),
        (28, "in_req"),
        (27, "out_req"),
    ]
    .iter()
    .filter(|(bit, _)| value >> bit & 1 != 0)
    .map(|(_, name)| *name)
    .collect();
    let block = value >> 16 & 7;
    let remaining = value & 0xFFFF;
    let flags = if flags.is_empty() { "-".to_string() } else { flags.join(" ") };
    format!("0x{value:08X} [{flags} block={block} words-1={remaining:#06x}]")
}

fn mdec_stream(by_id: &HashMap<u32, &Record>, first: u32, records: u32) -> Vec<i64> {
    let mut halves = Vec::new();
    for k in 0..records {
        let Some(record) = by_id.get(&(first + k)) else {
            break;
        };
        halves.extend([record.minimum, record.median, record.maximum]);
    }
    halves
}

fn shr(value: u64, count: u64) -> u64 {
    if count >= 64 {
        0
    } else {
        value >> count
    }
}

/// The v1.26 MDEC DIAGNOSTIC and its playbacks, unpacked. Empty unless the
/// capture carries them.
pub fn mdec_diag_rows(records: &[Record]) -> Vec<String> {
    let by_id = by_id(records);
    let Some(overview) = by_id.get(&0x260) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let chosen = overview.minimum;
    rows.push(format!(
        "# mdec_diag chosen={} runs={} batteries={}",
        if chosen == 0xFFFF {
            "none".to_string()
        } else {
            MDEC_SEQUENCES.get(chosen as usize).map_or_else(|| "?".to_string(), |s| s.to_string())
        },
        overview.median,
        overview.maximum
    ));
    rows.push("mdec_diag,sequence,field,value".to_string());
    let clocks = |v: i64| if v == MDEC_NEVER { "never".to_string() } else { v.to_string() };
    for (v, name) in MDEC_SEQUENCES.iter().enumerate() {
        let h = mdec_stream(&by_id, 0x200 + 0x10 * v as u32, 14);
        if h.len() < 27 {
            rows.push(format!("mdec_diag,{name},missing,{} halfwords", h.len()));
            continue;
        }
        let word = |i: usize| (h[i] | h[i + 1] << 16) as u64;
        let (mask, runs) = ((h[0] & 0xFF) as u64, (h[0] >> 8) as u64);
        let run_fails = word(9);
        let fails: Vec<&str> = (0..runs)
            .map(|r| MDEC_STEPS[(shr(run_fails, 4 * r) & 0xF).min(MDEC_STEPS.len() as u64 - 1) as usize])
            .collect();
        rows.push(format!(
            "mdec_diag,{name},worked,{}/{runs} runs_1_to_8={}",
            mask.count_ones(),
            format!("{mask:08b}").chars().rev().collect::<String>()
        ));
        rows.push(format!("mdec_diag,{name},per_run_fail,{}", fails.join(" | ")));
        rows.push(format!(
            "mdec_diag,{name},detail_run,{} fail={}",
            (h[1] >> 8) + 1,
            MDEC_STEPS[((h[1] & 0xFF) as usize).min(MDEC_STEPS.len() - 1)]
        ));
        rows.push(format!("mdec_diag,{name},settle_clocks,{}", clocks(h[2])));
        rows.push(format!("mdec_diag,{name},request_clocks,{}", clocks(h[3])));
        rows.push(format!("mdec_diag,{name},probe_request_clocks,{}", clocks(h[4])));
        if v == 5 {
            rows.push(format!(
                "mdec_diag,{name},sdk_driver,enable_writes={} cpu_uploads={} reset_settled={}",
                h[5] & 0xFF,
                h[5] >> 8 & 0x7F,
                h[5] >> 15
            ));
        }
        let (taken, rescue) = (h[6] & 0xFF, h[6] >> 8);
        for (slot, snapshot) in MDEC_SNAPSHOTS.iter().enumerate() {
            let value = word(11 + 2 * slot);
            let text = if taken >> slot & 1 != 0 {
                mdec_status_text(value)
            } else {
                "not read".to_string()
            };
            rows.push(format!("mdec_diag,{name},status_{snapshot},{text}"));
        }
        rows.push(format!(
            "mdec_diag,{name},probe,words={}/128 flat={} first=0x{:08X}",
            h[7] & 0x7FFF,
            h[7] >> 15,
            word(23)
        ));
        if v == 0 {
            let how = match rescue {
                0 => "not tried (no timeout)".to_string(),
                1 => "freed the stuck DMA".to_string(),
                2 => "did not free it".to_string(),
                other => other.to_string(),
            };
            let status = if rescue != 0 {
                format!(" status={}", mdec_status_text(word(25)))
            } else {
                String::new()
            };
            rows.push(format!("mdec_diag,{name},late_enable,{how}{status}"));
        }
        if h[8] != 0 && h.len() >= 39 {
            let w: Vec<u64> = (0..6).map(|i| word(27 + 2 * i)).collect();
            let (chcr, bcr, madr, dpcr, dicr, kick) = (w[0], w[1], w[2], w[3], w[4], w[5]);
            let moved = ((madr & 0xFF_FFFF) as i64 - (kick & 0xFF_FFFF) as i64).div_euclid(4);
            rows.push(format!(
                "mdec_diag,{name},dma_timeout,chcr=0x{chcr:08X} bcr=0x{bcr:08X} madr=0x{madr:08X} kick_madr=0x{kick:08X} words_moved={moved} blocks_left={} dpcr=0x{dpcr:08X} dicr=0x{dicr:08X}",
                bcr >> 16
            ));
        }
    }
    for (v, name) in MDEC_SEQUENCES.iter().enumerate() {
        let h = mdec_stream(&by_id, 0x270 + 4 * v as u32, 4);
        if h.len() < 12 {
            continue;
        }
        let (stop, passed, setup) = (h[0] & 0xF, h[0] >> 4 & 1, h[0] >> 8);
        let setup_text = match FMV_SETUP_ERRORS.get(setup as usize) {
            Some(text) => text.to_string(),
            None => setup.to_string(),
        };
        rows.push(format!(
            "mdec_play,{name},{},stop={} setup_error={setup_text} sectors={}/{} lost={} bad={} dropped={} decode_errors={} cd_errors={} first_error_lba={} last_good_lba={} shown={} late={}",
            if passed != 0 { "PASS" } else { "FAIL" },
            MDEC_STOPS[(stop as usize).min(4)],
            h[3],
            h[4],
            h[5],
            h[6],
            h[7],
            h[8],
            h[9],
            if h[10] == 0xFFFF { "none".to_string() } else { h[10].to_string() },
            h[11],
            h[1],
            h[2]
        ));
    }
    for (t, name) in MDEC_TRACES.iter().enumerate() {
        let h = mdec_stream(&by_id, 0x290 + 5 * t as u32, 5);
        if h.len() < 13 {
            continue;
        }
        let samples: Vec<String> = (0..(h[0].min(4) as usize))
            .map(|i| {
                format!(
                    "{}clk:{}",
                    h[1 + 3 * i],
                    mdec_status_text((h[2 + 3 * i] | h[3 + 3 * i] << 16) as u64)
                )
            })
            .collect();
        rows.push(format!("mdec_reset_trace,{name},{}", samples.join(" -> ")));
    }
    let h = mdec_stream(&by_id, 0x2A0, 3);
    if h.len() >= 9 {
        let (cpu_sum, dma_sum) = ((h[5] | h[6] << 16) as u64, (h[7] | h[8] << 16) as u64);
        rows.push(format!(
            "mdec_frame_control,read={} cpu_ok={} dma_ok={} sums_equal={} rle_words={} expected={} cpu_words={} dma_words={} cpu_sum=0x{cpu_sum:08X} dma_sum=0x{dma_sum:08X}",
            h[0] & 1,
            h[0] >> 1 & 1,
            h[0] >> 2 & 1,
            h[0] >> 3 & 1,
            h[1],
            h[2],
            h[3],
            h[4]
        ));
    }
    rows
}

/// The FMV STREAM TEST result, unpacked, with its verdict re-derived from the
/// pass criteria. Empty unless the test ran before the capture encoded.
pub fn fmv_rows(records: &[Record]) -> Vec<String> {
    let by_id = by_id(records);
    let ids = FMV_FIRST_RECORD..FMV_FIRST_RECORD + FMV_FIELDS.len() as u32;
    if !ids.clone().all(|id| by_id.contains_key(&id)) {
        return Vec::new();
    }
    let mut fields: Vec<(&str, i64)> = Vec::new();
    for (id, names) in ids.zip(FMV_FIELDS.iter()) {
        let record = by_id[&id];
        for (name, value) in names.iter().zip([record.minimum, record.median, record.maximum]) {
            fields.push((name, value));
        }
    }
    let get = |name: &str| fields.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or(0);
    let host_pass = get("total_sectors") > 0
        && get("good_sectors") == get("total_sectors")
        && get("lost_sectors") == 0
        && get("bad_sectors") == 0
        && get("dropped_frames") == 0
        && get("cd_errors") == 0
        && get("decode_errors") == 0
        && get("setup_error") == 0;
    let verdict = if get("pass") != 0 { "PASS" } else { "FAIL" };
    let mut rows = vec![format!(
        "# fmv={verdict} criteria={}",
        if host_pass { "PASS" } else { "FAIL" }
    )];
    if (get("pass") != 0) != host_pass {
        rows.push("# fmv verdict disagrees with its own counters".to_string());
    }
    rows.push("fmv,field,value".to_string());
    for (name, value) in &fields {
        if *name == "pass" {
            continue;
        }
        let text = if *name == "first_error_lba" && *value == FMV_NO_ERROR_LBA {
            "none".to_string()
        } else if *name == "setup_error" {
            match FMV_SETUP_ERRORS.get(*value as usize) {
                Some(text) if *value >= 0 => text.to_string(),
                _ => format!("code {value}"),
            }
        } else {
            value.to_string()
        };
        rows.push(format!("fmv,{name},{text}"));
    }
    rows
}

/// The v1.27 CONSOLE TESTS results, unpacked. Empty unless a case ran before
/// the capture encoded. Nothing here is a verdict: each case is read off the
/// screen (and the film of it), and these are the numbers behind it.
pub fn console_rows(records: &[Record]) -> Vec<String> {
    let by_id = by_id(records);
    let mut rows: Vec<String> = Vec::new();
    let triple_of = |id: u32| triple(by_id[&id]);

    if (0..CONSOLE_KERNEL.len() as u32).all(|k| by_id.contains_key(&(CONSOLE_KERNEL_FIRST + k))) {
        let mut fields: Vec<(&str, i64)> = Vec::new();
        for (k, names) in CONSOLE_KERNEL.iter().enumerate() {
            let (a, b, c) = triple_of(CONSOLE_KERNEL_FIRST + k as u32);
            for (name, value) in names.iter().zip([a, b, c]) {
                fields.push((name, value));
            }
        }
        let flags_at = fields.iter().position(|(n, _)| *n == "flags").expect("flags field");
        let flags = fields.remove(flags_at).1;
        let get = |name: &str| fields.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or(0);
        let harness = get("empty_call_med");
        rows.push("console_kernel,field,value".to_string());
        for (name, value) in &fields {
            rows.push(format!("console_kernel,{name},{value}"));
        }
        rows.push(format!("console_kernel,enter_cs_net,{}", (get("enter_cs_med") - harness).max(0)));
        rows.push(format!("console_kernel,exit_cs_net,{}", (get("exit_cs_med") - harness).max(0)));
        rows.push(format!("console_kernel,standard_vector,{}", flags & 1));
        rows.push(format!("console_kernel,event_opened,{}", (flags >> 1) & 1));
        rows.push(format!("console_kernel,bios_loop_finished,{}", (flags >> 2) & 1));
        rows.push(format!("console_kernel,runtime_vblank_gaps,{}", flags >> 8));
    }
    for (k, width) in CONSOLE_WIDTHS.iter().enumerate() {
        let id = 0x2D0 + k as u32;
        if by_id.contains_key(&id) {
            let (status, x1, x2) = triple_of(id);
            if rows.last().is_none_or(|row| !row.starts_with("console_width")) {
                rows.push("console_width,pixels,gpustat_high16,gp1_06_x1,gp1_06_x2".to_string());
            }
            let shown = if status == 0xFFFF {
                "not shown".to_string()
            } else {
                format!("0x{status:04X},{x1},{x2}")
            };
            rows.push(format!("console_width,{width},{shown}"));
        }
    }
    if by_id.contains_key(&0x2D6) && by_id.contains_key(&0x2D7) {
        let (status, flips, frames) = triple_of(0x2D6);
        let (interlaced, tall, low) = triple_of(0x2D7);
        rows.push("console_interlace,field,value".to_string());
        rows.push(format!("console_interlace,gpustat,0x{status:04X}{low:04X}"));
        rows.push(format!("console_interlace,field_parity_changes,{flips}"));
        rows.push(format!("console_interlace,frames_sampled,{frames}"));
        rows.push(format!("console_interlace,frames_with_interlace_bit,{interlaced}"));
        rows.push(format!("console_interlace,frames_with_480_bit,{tall}"));
    }
    if (0..4).all(|k| by_id.contains_key(&(0x2E0 + k))) {
        let (flags, loops, seconds) = triple_of(0x2E0);
        let names = [
            "file_found",
            "play_refused",
            "head_in_song",
            "looped",
            "getlocp_updating",
            "no_loop_seen",
        ];
        rows.push("console_xa,field,value".to_string());
        for (bit, name) in names.iter().enumerate() {
            rows.push(format!("console_xa,{name},{}", (flags >> bit) & 1));
        }
        rows.push(format!("console_xa,loops,{loops}"));
        rows.push(format!("console_xa,seconds_run,{seconds}"));
        for (id, name) in [(0x2E1, "loop_gap_ms"), (0x2E2, "loop_period_ms")] {
            let (lo, med, hi) = triple_of(id);
            let text = if lo == 0xFFFF {
                "none".to_string()
            } else {
                format!("min={lo} med={med} max={hi}")
            };
            rows.push(format!("console_xa,{name},{text}"));
        }
        let (first, positions, stall) = triple_of(0x2E3);
        rows.push(format!("console_xa,first_start_ms,{first}"));
        rows.push(format!("console_xa,head_positions_seen,{positions}"));
        rows.push(format!("console_xa,longest_unchanged_head_ms,{stall}"));
    }
    rows
}

pub fn precision_label(index: usize) -> Result<String> {
    let fixed = match index {
        0 => "spu_delay_boot",
        1 => "spu_ctrl_stat_boot",
        18 => "spu_single_stop_mode_polls",
        35 => "spu_four_stop_mode_polls",
        36 => "spu_delay_forced_stable",
        37 => "spu_stable_single_block_hash",
        38 => "spu_stable_four_block_hash",
        43 => "gpu_after_irq_clear",
        44 => "gpu_irq_set_read0",
        45 => "gpu_irq_set_read1",
        46 => "gpu_irq_set_read2",
        47 => "gpu_irq_clear_read0",
        48 => "gpu_irq_clear_read1",
        61 => "timer_target_mode_initial",
        62 => "timer_target_counter_initial",
        63 => "timer_target_counter_after",
        64 => "timer_target_mode_read0",
        65 => "timer_target_mode_read1",
        66 => "timer_target_istat",
        67 => "timer_wrap_mode_initial",
        68 => "timer_wrap_counter_initial",
        69 => "timer_wrap_counter_after",
        70 => "timer_wrap_mode_read0",
        71 => "timer_wrap_mode_read1",
        72 => "timer_wrap_istat",
        _ => "",
    };
    if !fixed.is_empty() {
        return Ok(fixed.to_string());
    }
    Ok(match index {
        2..=17 => format!("spu_boot_single_block_word_{:02}", index - 2),
        19..=34 => format!("spu_boot_four_block_word_{:02}", index - 19),
        39..=42 => format!("spu_fifo_read_word_{:02}", index - 39),
        49..=60 => {
            let offset = index - 49;
            format!("gpu_dma_dir_{}_read{}", offset / 3, offset % 3)
        }
        73..=90 => format!("gte_nclip_scene_a_settle_gap{}_mac0", index - 26),
        91..=96 => {
            let offset = index - 91;
            let mode = if offset < 3 { "immediate" } else { "settled" };
            format!("gte_op_full_{mode}_mac{}", offset % 3 + 1)
        }
        97..=104 => format!("spu_voice0_offset_{:02X}_write_ffff", (index - 97) * 2),
        105 => "otc_chcr_before_start".to_string(),
        106..=111 => format!("otc_chcr_read{}", index - 106),
        112 => "otc_madr_after".to_string(),
        113 => "otc_bcr_after".to_string(),
        114 => "otc_remaining_busy_polls".to_string(),
        115 => "otc_first_word".to_string(),
        116 => "otc_last_word".to_string(),
        117..=119 => format!("gte_nclip_scene_{}_mac0", index - 117),
        120..=123 => format!("gte_rtpt_e_then_nclip_a_run{}", index - 120),
        124 => "gte_rtpt_e_then_nclip_a_sequence".to_string(),
        125 => "gte_rtpt_e_then_nclip_b_sequence".to_string(),
        126 => "gte_rtpt_e_then_nclip_c_sequence".to_string(),
        127 => "gte_nclip_a_after_c_sequence".to_string(),
        // PX7 additions: console identity, then bit-exact raster hashes.
        128..=131 => format!("bios_date_word_{:02}", index - 128),
        132..=135 => format!("bios_version_word_{:02}", index - 132),
        136 => "gpustat_at_rest".to_string(),
        137 => "mdec_status_after_reset".to_string(),
        138..=159 => format!("raster_hash_{:02}", index - 138),
        160..=191 => format!("reserved_{index:03}"),
        _ => bail!("unknown precision index {index}"),
    })
}

/// Options of `print_report`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReportOptions {
    pub fail_on_change: bool,
    pub layout_immune_only: bool,
}

/// Print the report on `out` (and the failure line on `err`); returns the
/// process exit code.
pub fn print_report(
    capture: &Capture,
    baseline: Option<&Capture>,
    options: ReportOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    macro_rules! say {
        ($($arg:tt)*) => {
            writeln!(out, $($arg)*)?
        };
    }
    // Every baseline difference lands here so the summary can name what moved
    // instead of only reporting that something did.
    let mut drift: Vec<String> = Vec::new();
    let mut layout_drift = 0;
    say!(
        "# schema={} suite=v{}.{} pages={} run={:02X} digest={:08X} records={} binary_crc={:08X}",
        capture.schema,
        capture.suite_major,
        capture.suite_minor,
        capture.page_count,
        capture.timing_run,
        capture.timing_digest,
        capture.records.len(),
        capture.binary_crc
    );
    let mut tallies: BTreeMap<&str, usize> = BTreeMap::new();
    for &status in &capture.statuses {
        *tallies.entry(STATUS_LABELS[usize::from(status)]).or_insert(0) += 1;
    }
    let tally_text: Vec<String> = tallies
        .iter()
        .map(|(label, count)| format!("{}={count}", label.to_lowercase()))
        .collect();
    say!(
        "# conformance_run={:02X} digest={:08X} cases={} {}",
        capture.conformance_run,
        capture.conformance_digest,
        capture.statuses.len(),
        tally_text.join(" ")
    );

    // A conformance capture spends its bytes only on cases that did not pass, so
    // this is the whole result. Printed before the per-case table because on
    // such a capture the table is empty.
    if !capture.failures.is_empty() {
        say!("failure_id,expected,observed");
        for failure in &capture.failures {
            say!(
                "{:#06x},0x{:08X},0x{:08X}",
                failure.test_id,
                failure.expected,
                failure.observed
            );
            if let Some(baseline) = baseline {
                if !baseline.failures.contains(failure) {
                    drift.push(format!(
                        "new failure {:#06x}: expected 0x{:08X} observed 0x{:08X}",
                        failure.test_id, failure.expected, failure.observed
                    ));
                }
            }
        }
    } else if capture.flags & PX8_BLOCK_FAILURES != 0 {
        say!("# no failures");
    }
    if let Some(baseline) = baseline {
        for failure in &baseline.failures {
            if !capture.failures.contains(failure) {
                drift.push(format!("failure {:#06x} no longer reproduces", failure.test_id));
            }
        }
    }

    let mut case_columns = "case,status,observed".to_string();
    if baseline.is_some() {
        case_columns += ",baseline_observed,changed";
    }
    say!("{case_columns}");
    for (index, (&status, &observed)) in capture.statuses.iter().zip(&capture.observations).enumerate() {
        let mut row = format!("{index},{},0x{observed:08X}", STATUS_LABELS[usize::from(status)]);
        if let Some(baseline) = baseline {
            let Some(&prior) = baseline.observations.get(index) else {
                // A MINOR bump appends cases; the baseline has nothing to say
                // about them.
                row += ",absent,n/a";
                say!("{row}");
                continue;
            };
            row += &format!(",0x{prior:08X},{}", i32::from(prior != observed));
            if prior != observed {
                drift.push(format!("case {index}: 0x{prior:08X} -> 0x{observed:08X}"));
            }
        }
        say!("{row}");
    }

    let has_median = capture.records.iter().any(|record| record.median >= 0);
    let mut columns = "id,label,work,min,max,jitter".to_string();
    if has_median {
        columns += ",median";
    }
    if baseline.is_some() {
        columns += ",baseline_min,delta_min";
    }
    say!("{columns}");
    let baseline_records: Option<HashMap<u32, &Record>> =
        baseline.map(|baseline| by_id(&baseline.records));
    for record in &capture.records {
        let name = label(record.record_id).unwrap_or("unlabelled");
        let mut row = format!(
            "{:02X},{name},{},{},{},{}",
            record.record_id,
            record.work,
            record.minimum,
            record.maximum,
            record.maximum - record.minimum
        );
        if has_median {
            row += &format!(",{}", record.median);
        }
        if let Some(baseline_records) = &baseline_records {
            match baseline_records.get(&record.record_id) {
                None => {
                    // The operator can skip the rest of the timing scan (START),
                    // leaving later records absent from a silicon capture. That
                    // is a partial capture, not corruption: compare what exists.
                    row += ",absent,n/a";
                }
                Some(prior) => {
                    row += &format!(",{},{:+}", prior.minimum, record.minimum - prior.minimum);
                    let tolerated = options.layout_immune_only && !layout_immune(record.record_id);
                    if prior.minimum != record.minimum && tolerated {
                        layout_drift += 1;
                    } else if prior.minimum != record.minimum {
                        drift.push(format!(
                            "timing {:02X} ({name}): min {} -> {}",
                            record.record_id, prior.minimum, record.minimum
                        ));
                    }
                }
            }
        }
        say!("{row}");
    }

    let scan_names = ["cpu", "gte", "spu"];
    let scans: Vec<String> = scan_names
        .iter()
        .zip(&capture.scans)
        .map(|(name, scan)| {
            format!(
                "{name}:status={}:items={}:digest={:08X}:aux={:08X}:run={:02X}",
                STATUS_LABELS[usize::from(scan.status)],
                scan.items,
                scan.digest,
                scan.aux,
                scan.run
            )
        })
        .collect();
    say!("# scans={}", scans.join(","));
    let memory: Vec<String> = capture
        .memory_control
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{}:0x{value:08X}", memory_control_name(index)))
        .collect();
    say!("# memory_control={}", memory.join(","));
    if !capture.precision.is_empty() {
        let baseline_precision: &[u32] = baseline.map_or(&[], |baseline| &baseline.precision);
        say!(
            "precision,label,value{}",
            if baseline_precision.is_empty() { "" } else { ",baseline_value,changed" }
        );
        for (index, &value) in capture.precision.iter().enumerate() {
            let name = precision_label(index)?;
            let mut row = format!("{index:03},{name},0x{value:08X}");
            if !baseline_precision.is_empty() {
                let Some(&prior) = baseline_precision.get(index) else {
                    bail!("list index out of range");
                };
                row += &format!(",0x{prior:08X},{}", i32::from(prior != value));
                if prior != value {
                    drift.push(format!("precision {index:03} ({name}): 0x{prior:08X} -> 0x{value:08X}"));
                }
            }
            say!("{row}");
        }
    }
    for row in list_busy_rows(capture) {
        say!("{row}");
    }
    for row in fmv_rows(&capture.records) {
        say!("{row}");
    }
    for row in mdec_diag_rows(&capture.records) {
        say!("{row}");
    }
    for row in console_rows(&capture.records) {
        say!("{row}");
    }
    let slice = |values: &[u32]| -> Vec<u32> {
        let start = GTE_SETTLE_FIRST_CASE.min(values.len());
        let end = (GTE_SETTLE_FIRST_CASE + GTE_SETTLE_CASE_COUNT).min(values.len());
        values[start..end].to_vec()
    };
    let settle = slice(&capture.observations);
    say!(
        "# gte_settle_run={:02X} digest={:08X} cases={}",
        capture.conformance_run,
        capture.gte_digest,
        settle.len()
    );
    let settle_text: Vec<String> = settle
        .iter()
        .enumerate()
        .map(|(offset, value)| format!("{}:0x{value:08X}", GTE_SETTLE_FIRST_CASE + offset))
        .collect();
    say!("# gte_settle={}", settle_text.join(","));
    if let Some(baseline) = baseline {
        let baseline_settle = slice(&baseline.observations);
        let delta: Vec<String> = settle
            .iter()
            .zip(&baseline_settle)
            .enumerate()
            .map(|(offset, (&value, &prior))| {
                format!("{}:{:+}", GTE_SETTLE_FIRST_CASE + offset, i64::from(value) - i64::from(prior))
            })
            .collect();
        say!("# gte_settle_delta={}", delta.join(","));
    }
    if baseline.is_some() {
        if options.layout_immune_only {
            say!("# layout_drift_tolerated={layout_drift}");
        }
        say!("# drift={}", drift.len());
        for entry in &drift {
            say!("# drift: {entry}");
        }
        if !drift.is_empty() && options.fail_on_change {
            writeln!(
                err,
                "FAIL: {} value(s) moved against the baseline. Re-baseline deliberately with `make hwtest-baseline` if this is intended.",
                drift.len()
            )?;
            return Ok(1);
        }
    }
    Ok(0)
}
