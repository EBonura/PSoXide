//! Audit the hardware-test disc's measured instruction blocks in the LINKED EXE.
//!
//! Every timing probe brackets its measured interval with a pair of marker
//! words, `ori $zero, $zero, imm`: start = 0x34000000 | (id << 1), end =
//! start | 1. They write no register and no compiler emits them, so every such
//! word in the image is a marker. The markers exist so the final PS-X machine
//! code can be audited rather than trusted: a timing number only means what the
//! docs claim if the instructions between the markers are still the ones the
//! source asked for.
//!
//! This walks the linked EXE, pairs the markers, and digests the words between
//! each pair. Pin the output with --baseline and any change to a measured block
//! (an LLVM bump reordering a wrapper, an edit that lands inside the timed
//! window) shows up as a moved digest instead of a silently different cycle
//! count. Probes are discovered from the image, never from source text, so a
//! macro-generated probe or one in another module cannot fall out of the audit;
//! with --fail-on-change a probe that appears or disappears fails too.
//!
//! Names live in the baseline, keyed by id. A new id prints as probe_NN until
//! someone names it there.
//!
//! Layout tags (0x34008000 | n) are single non-executed words that pad the
//! I-cache entry targets. Their position within a 16-byte cache line is what
//! the entry probes depend on, so it is pinned the same way.

use std::collections::BTreeMap;
use std::io::Write;

use crate::util::{Error, Result};

const PSX_EXE_HEADER_BYTES: usize = 0x800;
const MARKER_MASK: u32 = 0xFFFF_0000;
const MARKER_BASE: u32 = 0x3400_0000;
const LAYOUT_BIT: u32 = 0x8000;
/// A measured block is tens to hundreds of instructions; 1024 words is slack.
const MAX_SPAN_WORDS: usize = 1024;

/// A malformed marker pairing (the script's `AuditError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditError(pub String);

pub fn exe_words(bytes: &[u8]) -> Vec<u32> {
    let body = bytes.get(PSX_EXE_HEADER_BYTES..).unwrap_or(&[]);
    body.chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect()
}

/// FNV-1a over the measured words, matching the disc's own hash style.
pub fn digest(values: &[u32]) -> u32 {
    let mut acc = 0x811C_9DC5u32;
    for &value in values {
        acc = (acc ^ value).wrapping_mul(0x0100_0193);
    }
    acc
}

pub type Spans = BTreeMap<u32, (usize, usize)>;
pub type Layout = BTreeMap<u32, usize>;

/// ({probe id: (start index, end index)}, {layout tag: word index}).
pub fn discover(words: &[u32]) -> std::result::Result<(Spans, Layout), AuditError> {
    let mut spans = Spans::new();
    let mut layout = Layout::new();
    let mut open: Option<(u32, usize)> = None;
    for (index, &word) in words.iter().enumerate() {
        if word & MARKER_MASK != MARKER_BASE {
            continue;
        }
        let low = word & 0xFFFF;
        if low & LAYOUT_BIT != 0 {
            let tag = low & !LAYOUT_BIT;
            if layout.contains_key(&tag) {
                return Err(AuditError(format!("layout tag {tag} appears twice")));
            }
            layout.insert(tag, index);
            continue;
        }
        let (probe_id, is_end) = (low >> 1, low & 1);
        if is_end == 0 {
            if let Some((open_id, _)) = open {
                return Err(AuditError(format!(
                    "probe {open_id:02} has no end marker before probe {probe_id:02} starts"
                )));
            }
            if spans.contains_key(&probe_id) {
                return Err(AuditError(format!("probe id {probe_id:02} is used twice")));
            }
            open = Some((probe_id, index));
            continue;
        }
        let Some((open_id, open_at)) = open else {
            return Err(AuditError(format!(
                "end marker for probe {probe_id:02} without its start"
            )));
        };
        if open_id != probe_id {
            return Err(AuditError(format!(
                "end marker for probe {probe_id:02} without its start"
            )));
        }
        if index - open_at > MAX_SPAN_WORDS {
            return Err(AuditError(format!(
                "probe {probe_id:02} spans {} words",
                index - open_at
            )));
        }
        spans.insert(probe_id, (open_at, index));
        open = None;
    }
    if let Some((open_id, _)) = open {
        return Err(AuditError(format!("probe {open_id:02} has no end marker")));
    }
    Ok((spans, layout))
}

/// `{key: (name, pinned value)}` in file order; key is `NN` for a probe, `Ln`
/// for a layout tag. A repeated key keeps its first position and its last
/// value, as a dict does.
pub fn parse_baseline(text: &str) -> Result<Vec<(String, (String, String))>> {
    let mut rows: Vec<(String, (String, String))> = Vec::new();
    for line in crate::util::splitlines(text) {
        if line.is_empty() || line.starts_with('#') || line.starts_with("id,") {
            continue;
        }
        let mut parts = line.splitn(3, ',');
        let (Some(key), Some(name), Some(pinned)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("not enough values to unpack (expected 3)");
        };
        let value = (name.to_string(), pinned.to_string());
        match rows.iter_mut().find(|(k, _)| k == key) {
            Some(existing) => existing.1 = value,
            None => rows.push((key.to_string(), value)),
        }
    }
    Ok(rows)
}

/// `psoxide-hwtest verify-machine-code <exe> [--baseline FILE] [--fail-on-change]`
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let mut exe = None;
    let mut baseline_path = None;
    let mut fail_on_change = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--baseline" => {
                i += 1;
                baseline_path = Some(args.get(i).ok_or("--baseline needs a value")?.clone());
            }
            "--fail-on-change" => fail_on_change = true,
            flag if flag.starts_with("--") => bail!("unrecognized arguments: {flag}"),
            path if exe.is_none() => exe = Some(path.to_string()),
            extra => bail!("unrecognized arguments: {extra}"),
        }
        i += 1;
    }
    let exe = exe.ok_or("the following arguments are required: exe")?;
    let bytes = std::fs::read(&exe).map_err(|e| Error(format!("{exe}: {e}")))?;
    let words = exe_words(&bytes);
    let (spans, layout) = match discover(&words) {
        Ok(found) => found,
        Err(AuditError(message)) => {
            writeln!(err, "FAIL: {message}")?;
            return Ok(2);
        }
    };
    if spans.is_empty() {
        writeln!(err, "FAIL: no marker-bracketed probes found in {exe}")?;
        return Ok(2);
    }
    let baseline: Vec<(String, (String, String))> = match &baseline_path {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| Error(format!("{path}: {e}")))?;
            parse_baseline(&String::from_utf8(bytes).map_err(|e| Error(e.to_string()))?)?
        }
        None => Vec::new(),
    };
    let lookup = |key: &str| baseline.iter().find(|(k, _)| k == key).map(|(_, v)| v);

    let mut current: Vec<(String, (String, String))> = Vec::new();
    for (probe_id, &(first, last)) in &spans {
        let measured = &words[first + 1..last];
        let key = format!("{probe_id:02}");
        let name = lookup(&key)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| format!("probe_{key}"));
        current.push((key, (name, format!("{},{:#010x}", measured.len(), digest(measured)))));
    }
    for (tag, &index) in &layout {
        let key = format!("L{tag}");
        let name = lookup(&key)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| format!("layout_{tag}"));
        current.push((key, (name, format!("line_word,{}", index % 4))));
    }

    writeln!(
        out,
        "# exe={exe} words={} probes={} layout_tags={}",
        words.len(),
        spans.len(),
        layout.len()
    )?;
    writeln!(
        out,
        "id,name,words,digest{}",
        if baseline_path.is_some() { ",changed" } else { "" }
    )?;
    let mut drift: Vec<String> = Vec::new();
    for (key, (name, value)) in &current {
        let mut row = format!("{key},{name},{value}");
        if baseline_path.is_some() {
            let prior = lookup(key);
            match prior {
                None => drift.push(format!("{key} ({name}): not in the baseline")),
                Some((_, pinned)) if pinned != value => {
                    drift.push(format!("{key} ({name}): {pinned} -> {value}"))
                }
                _ => {}
            }
            row += &format!(",{}", i32::from(prior.is_none_or(|(_, pinned)| pinned != value)));
        }
        writeln!(out, "{row}")?;
    }
    for (key, (name, _)) in &baseline {
        if !current.iter().any(|(k, _)| k == key) {
            drift.push(format!("{key} ({name}): pinned but no longer in the image"));
        }
    }
    writeln!(out, "# drift={}", drift.len())?;
    for entry in &drift {
        writeln!(out, "# drift: {entry}")?;
    }
    if !drift.is_empty() && fail_on_change {
        writeln!(err, "FAIL: {} measured block(s) changed", drift.len())?;
        return Ok(1);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: u32 = 0x3400_0000 | (7 << 1);
    const END: u32 = 0x3400_0001 | (7 << 1);

    #[test]
    fn a_span_is_found_and_digested_without_its_markers() {
        let body = [0x0109_0019u32, 0x0000_5012];
        let words = [0, START, body[0], body[1], END, 0x3400_8003];
        let (spans, layout) = discover(&words).unwrap();
        assert_eq!(spans.get(&7), Some(&(1, 4)));
        assert_eq!(layout.get(&3), Some(&5));
        assert_ne!(digest(&body), digest(&[body[1], body[0]]));
    }

    #[test]
    fn broken_marker_pairs_are_errors() {
        for words in [
            vec![START],                  // never closed
            vec![END],                    // never opened
            vec![START, START, END],      // reopened
            vec![START, END, START, END], // id reused
        ] {
            assert!(discover(&words).is_err(), "{words:?}");
        }
    }

    #[test]
    fn baseline_rows_keep_first_position_and_last_value() {
        let rows = parse_baseline("# c\nid,name,words,digest\n07,a,1,0x1\n08,b,2,0x2\n07,c,3,0x3\n").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("07".to_string(), ("c".to_string(), "3,0x3".to_string())));
        assert!(parse_baseline("bad line\n").is_err());
    }

    #[test]
    fn the_fnv_digest_matches_the_known_value() {
        assert_eq!(digest(&[]), 0x811C_9DC5);
        assert_eq!(digest(&[0]), 0x811C_9DC5u32.wrapping_mul(0x0100_0193));
    }
}
