//! The one command that decodes a recording (or a page file) and diffs it
//! against the last silicon baselines, plus the two small helpers the emulator
//! run uses: the silence check on the dumped audio and the emulator baseline
//! file.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::audio_decode::read_wav_file;
use crate::report::{
    parse_capture, parse_capture_page, payloads_from_paths, precision_label, print_report,
    record_label, run_id_text, Capture, Record, ReportOptions, STATUS_LABELS,
};
use crate::util::{Error, Result};
use crate::v2::v2_verdicts;

/// Oldest first: where two baselines carry the same record, the later one wins.
pub const SILICON_REFERENCES: [(&str, &str); 2] = [
    (
        "docs/hardware-refs/px8-silicon-2026-09-23-v1.24-full.txt",
        "v1.24 full",
    ),
    (
        "docs/hardware-refs/px8-silicon-2026-10-08-v1.28-cdstream.txt",
        "v1.28 CD stream",
    ),
];
/// Records whose meaning v2.0 changed; they cannot be compared across the bump.
/// Every other id that exists in both means what it meant (the v2.0 bump is
/// MAJOR because records were removed and regrouped, not because these were
/// redefined).
pub const REDEFINED_IN_V2: std::ops::Range<u32> = 0x2D0..0x2D8;
/// Records that carry flags or counts: exact match, not a timing tolerance.
pub const EXACT_RECORDS: [u32; 8] = [0x2C4, 0x2E0, 0x2F7, 0x306, 0x308, 0x30A, 0x30B, 0x315];
pub const TIMING_TOLERANCE: f64 = 0.10;
pub const TIMING_SLACK: i64 = 8;

pub struct SiliconReference {
    /// Case index to (status, observed).
    pub cases: HashMap<usize, (u8, u32)>,
    pub records: HashMap<u32, Record>,
    pub precision: Vec<u32>,
    pub sources: Vec<String>,
}

/// The editor root the silicon baselines live under: the working directory
/// when it holds `docs/hardware-refs` (the Makefile runs from there), else the
/// checkout this binary was built from.
pub fn default_root() -> PathBuf {
    let here = PathBuf::from(".");
    if here.join("docs/hardware-refs").is_dir() {
        return here;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn load_silicon_reference(root: &Path) -> Result<SiliconReference> {
    let mut reference = SiliconReference {
        cases: HashMap::new(),
        records: HashMap::new(),
        precision: Vec::new(),
        sources: Vec::new(),
    };
    for (relative, label) in SILICON_REFERENCES {
        let path = root.join(relative);
        if !path.exists() {
            continue;
        }
        let capture = parse_capture(&payloads_from_paths(&[path
            .to_string_lossy()
            .into_owned()])?)?;
        reference.sources.push(format!(
            "{label} (v{}.{})",
            capture.suite_major, capture.suite_minor
        ));
        for (index, (&status, &observed)) in capture
            .statuses
            .iter()
            .zip(&capture.observations)
            .enumerate()
        {
            reference.cases.insert(index, (status, observed));
        }
        for record in &capture.records {
            reference.records.insert(record.record_id, record.clone());
        }
        if !capture.precision.is_empty() {
            reference.precision = capture.precision.clone();
        }
    }
    ensure!(
        !reference.sources.is_empty(),
        "no silicon baseline found under docs/hardware-refs"
    );
    Ok(reference)
}

#[derive(Debug, Default)]
pub struct Comparison {
    pub regressions: Vec<String>,
    pub mismatches: Vec<String>,
    pub improvements: Vec<String>,
    pub shared_cases: usize,
    pub shared_records: usize,
    pub shared_precision: usize,
}

/// Regressions and mismatches of `capture` against the silicon baselines.
///
/// A regression is something that is wrong on its own terms or worse than
/// silicon was: a conformance case that passed on silicon and does not now, an
/// area that did not hand off clean, noise at the capture pages. A mismatch is
/// a value that differs from silicon's: the list of what an emulator capture
/// still gets wrong, or what moved between two silicon runs.
pub fn compare_to_silicon(capture: &Capture, reference: &SiliconReference) -> Comparison {
    let mut result = Comparison {
        regressions: v2_verdicts(capture)
            .into_iter()
            .map(|problem| format!("run check: {problem}"))
            .collect(),
        ..Comparison::default()
    };
    for (index, &status) in capture.statuses.iter().enumerate() {
        let Some(&(silicon_status, silicon_observed)) = reference.cases.get(&index) else {
            continue;
        };
        result.shared_cases += 1;
        let observed = capture.observations.get(index).copied();
        let name = format!("case {index}");
        let label = |status: u8| STATUS_LABELS[usize::from(status)];
        if silicon_status == 1 && (status == 2 || status == 3) {
            let seen = observed.map_or(String::new(), |value| format!(", observed 0x{value:08X}"));
            result.regressions.push(format!(
                "{name}: {} (passed on silicon){seen}",
                label(status)
            ));
        } else if (silicon_status == 2 || silicon_status == 3) && status == 1 {
            result.improvements.push(format!(
                "{name}: passes (silicon {})",
                label(silicon_status)
            ));
        }
        if let Some(observed) = observed {
            if observed != silicon_observed {
                result.mismatches.push(format!(
                    "{name}: 0x{observed:08X}, silicon 0x{silicon_observed:08X} ({} vs {})",
                    label(status),
                    label(silicon_status)
                ));
            }
        }
    }
    for record in &capture.records {
        let Some(silicon) = reference.records.get(&record.record_id) else {
            continue;
        };
        if REDEFINED_IN_V2.contains(&record.record_id) {
            continue;
        }
        result.shared_records += 1;
        let moved = if EXACT_RECORDS.contains(&record.record_id) {
            (record.minimum, record.median, record.maximum)
                != (silicon.minimum, silicon.median, silicon.maximum)
        } else {
            let allowed = TIMING_SLACK.max((silicon.minimum as f64 * TIMING_TOLERANCE) as i64);
            (record.minimum - silicon.minimum).abs() > allowed
        };
        if moved {
            result.mismatches.push(format!(
                "record {:03X} {}: min/med/max {}/{}/{}, silicon {}/{}/{}",
                record.record_id,
                record_label(record.record_id),
                record.minimum,
                record.median,
                record.maximum,
                silicon.minimum,
                silicon.median,
                silicon.maximum
            ));
        }
    }
    for (index, (&value, &silicon_value)) in capture
        .precision
        .iter()
        .zip(&reference.precision)
        .enumerate()
    {
        result.shared_precision += 1;
        if value != silicon_value {
            result.mismatches.push(format!(
                "precision {index:03} {}: 0x{value:08X}, silicon 0x{silicon_value:08X}",
                precision_label(index).unwrap_or_default()
            ));
        }
    }
    result
}

/// `compare INPUT [--report] [--no-fail]`.
pub fn compare_command(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let (mut show_report, mut allow_failure) = (false, false);
    let mut inputs = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--report" => show_report = true,
            "--no-fail" => allow_failure = true,
            flag if flag.starts_with("--") => {
                return Err(Error(format!("unrecognized arguments: {flag}")))
            }
            input => inputs.push(input.to_string()),
        }
    }
    ensure!(inputs.len() == 1, "compare takes exactly one INPUT");
    let source = &inputs[0];
    let lower = source.to_lowercase();
    if [".mov", ".mp4", ".mkv", ".avi", ".m4v"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        bail!(
            "{source} is a video: this tool does not decode recordings. Run tools/hwtest-video-qr.py {source} pages.txt first and pass pages.txt"
        );
    }
    let capture = parse_capture(&payloads_from_paths(std::slice::from_ref(source))?)?;
    let reference = load_silicon_reference(&default_root())?;
    if show_report {
        print_report(&capture, None, ReportOptions::default(), out, err)?;
    }
    let result = compare_to_silicon(&capture, &reference);
    writeln!(
        out,
        "# compare: capture v{}.{} run={} pages={} crc={:08X}",
        capture.suite_major,
        capture.suite_minor,
        run_id_text(&capture),
        capture.page_count,
        capture.binary_crc
    )?;
    writeln!(out, "# silicon: {}", reference.sources.join(", "))?;
    writeln!(
        out,
        "# compared: {} cases, {} records, {} precision values; records redefined in v2.0 are skipped",
        result.shared_cases, result.shared_records, result.shared_precision
    )?;
    for line in &result.regressions {
        writeln!(out, "REGRESSION {line}")?;
    }
    for line in &result.improvements {
        writeln!(out, "IMPROVED {line}")?;
    }
    for line in &result.mismatches {
        writeln!(out, "MISMATCH {line}")?;
    }
    writeln!(
        out,
        "# summary: regressions={} mismatches={} improvements={}",
        result.regressions.len(),
        result.mismatches.len(),
        result.improvements.len()
    )?;
    Ok(i32::from(!result.regressions.is_empty() && !allow_failure))
}

// ---------------------------------------------------------------------------
// Silence of the capture pages, from the audio the emulator dumped.

/// `check-silence WAV [--tail-seconds S]`.
pub fn check_silence_command(
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let mut tail_seconds = 3.0f64;
    let mut inputs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tail-seconds" => {
                i += 1;
                let value = args.get(i).ok_or("--tail-seconds needs a value")?;
                tail_seconds = value
                    .parse()
                    .map_err(|_| Error(format!("invalid float value: {value:?}")))?;
            }
            flag if flag.starts_with("--") => {
                return Err(Error(format!("unrecognized arguments: {flag}")))
            }
            input => inputs.push(input.to_string()),
        }
        i += 1;
    }
    ensure!(inputs.len() == 1, "check-silence takes exactly one WAV");
    check_silence(Path::new(&inputs[0]), tail_seconds, out, err)
}

pub fn check_silence(
    path: &Path,
    tail_seconds: f64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    let wav = read_wav_file(path)?;
    let channels = usize::from(wav.channels.max(1));
    let rate = f64::from(wav.rate);
    let frames = wav.samples.len() / channels;
    let duration = frames as f64 / rate;
    // Non-silent spans, in seconds, merged when closer than 0.25 s.
    let window = (wav.rate / 20).max(1) as usize;
    let mut loud = Vec::new();
    for start in (0..frames).step_by(window) {
        let end = ((start + window) * channels).min(wav.samples.len());
        if wav.samples[start * channels..end].iter().any(|&s| s != 0) {
            loud.push(start as f64 / rate);
        }
    }
    let mut spans: Vec<[f64; 2]> = Vec::new();
    for t in loud {
        match spans.last_mut() {
            Some(last) if t - last[1] <= 0.25 => last[1] = t + 0.05,
            _ => spans.push([t, t + 0.05]),
        }
    }
    writeln!(
        out,
        "# audio: {duration:.1} s, {} non-silent span(s)",
        spans.len()
    )?;
    for [begin, end] in &spans {
        writeln!(out, "# sound {begin:.1}-{end:.1} s")?;
    }
    let last_sound = spans.last().map_or(0.0, |span| span[1]);
    let silent_tail = duration - last_sound;
    writeln!(
        out,
        "# silent tail: {silent_tail:.1} s (needs {tail_seconds:.1} s: the capture pages)"
    )?;
    if silent_tail < tail_seconds {
        writeln!(
            err,
            "FAIL: sound continues until {last_sound:.1} s of {duration:.1} s"
        )?;
        return Ok(1);
    }
    writeln!(out, "silent at the capture pages")?;
    Ok(0)
}

// ---------------------------------------------------------------------------

/// Today's date in UTC as `YYYY-MM-DD`.
fn utc_date() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (seconds / 86_400) as i64;
    // Days since 1970-01-01 to a civil date (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// `emulator-baseline LOG [--git X] [--exe-sha X] [--steps N]`.
pub fn emulator_baseline_command(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let (mut git, mut exe_sha, mut steps) = (
        "unknown".to_string(),
        "unknown".to_string(),
        "unknown".to_string(),
    );
    let mut logs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let slot = match args[i].as_str() {
            "--git" => Some(&mut git),
            "--exe-sha" => Some(&mut exe_sha),
            "--steps" => Some(&mut steps),
            flag if flag.starts_with("--") => {
                return Err(Error(format!("unrecognized arguments: {flag}")))
            }
            input => {
                logs.push(input.to_string());
                None
            }
        };
        if let Some(slot) = slot {
            i += 1;
            *slot = args
                .get(i)
                .ok_or_else(|| Error(format!("{} needs a value", args[i - 1])))?
                .clone();
        }
        i += 1;
    }
    ensure!(logs.len() == 1, "emulator-baseline takes exactly one LOG");
    let pages = payloads_from_paths(&logs)?;
    // Refuse a log that does not decode.
    parse_capture(&pages)?;
    writeln!(out, "# PSoXide hardware-test emulator baseline")?;
    writeln!(out, "#")?;
    writeln!(
        out,
        "# SOURCE: PSoXide EMULATOR, headless. This is NOT a silicon capture."
    )?;
    writeln!(
        out,
        "#   It detects emulator-side drift only. It is not hardware truth and"
    )?;
    writeln!(out, "#   must never be cited as a console measurement.")?;
    writeln!(out, "#")?;
    writeln!(out, "# captured:  {}", utc_date())?;
    writeln!(out, "# git:       {git}")?;
    writeln!(out, "# guest exe: sha256:{exe_sha}")?;
    writeln!(
        out,
        "# emulator:  frontend launch --steps {steps} (one CROSS pulse on RUN HARDWARE TEST)"
    )?;
    writeln!(out, "#")?;
    let mut last: std::collections::BTreeMap<u32, &String> = std::collections::BTreeMap::new();
    for line in &pages {
        last.insert(parse_capture_page(line)?.number, line);
    }
    for line in last.values() {
        writeln!(out, "{line}")?;
    }
    Ok(0)
}
