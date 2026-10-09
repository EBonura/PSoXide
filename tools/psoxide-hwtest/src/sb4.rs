//! Decode and diff SB4 capture-ring payloads.
//!
//! Input is any text containing an `SB4/<base64>/C:<crc>` line: the emulator's
//! TTY log, or the decoded text of a photographed QR. With --baseline it
//! compares every segment against a previous capture and exits 1 naming each
//! field that moved, which is what lets `make hwtest-sb4` gate emulator drift
//! the way `make hwtest-diff` gates the conformance battery.
//!
//! The run byte is excluded from comparison: it counts reruns within a boot,
//! not behaviour.

use std::io::Write;

use crate::util::{base64_decode, crc32, parse_hex, splitlines, Error, Reader, Result};

const MAGIC: &[u8; 4] = b"SB4B";
const SEGMENT_LABELS: [&str; 5] = ["SQUARE", "IMPULSE", "ENVRAMP", "NOISE", "VOICE3"];
/// Bit 15 of the stat field marks a half-flag wait that timed out; the rest of
/// that row describes nothing.
const STAT_TIMEOUT: u16 = 0x8000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub label: String,
    pub stat: u16,
    pub envx: u16,
    pub first: u16,
    pub hash: u32,
    pub raw: Vec<i16>,
}

impl Segment {
    pub fn timed_out(&self) -> bool {
        self.stat & STAT_TIMEOUT != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub schema: u8,
    pub run: u8,
    pub segments: Vec<Segment>,
    pub crc: u32,
}

fn extract_payload(text: &str) -> Result<&str> {
    let Some(at) = text.find("SB4/") else {
        bail!("no SB4/ payload found");
    };
    Ok(splitlines(&text[at..]).first().copied().unwrap_or(""))
}

pub fn parse(text: &str) -> Result<Capture> {
    let payload = extract_payload(text)?;
    let rest = payload.strip_prefix("SB4/").unwrap_or(payload);
    let Some((body, claimed)) = rest.rsplit_once("/C:") else {
        bail!("not enough values to unpack (expected 2, got 1)");
    };
    let raw = base64_decode(body)?;
    ensure!(raw.len() >= 12, "SB4 payload is truncated");
    ensure!(raw[..4] == *MAGIC, "bad magic {:?}", &raw[..4]);
    let (schema, segment_count, raw_count, run) = (raw[4], raw[5], raw[6], raw[7]);
    ensure!(schema == 1, "unknown SB4 schema {schema}");
    let crc = u32::from_le_bytes(raw[raw.len() - 4..].try_into().expect("4"));
    let actual = crc32(&raw[..raw.len() - 4]);
    ensure!(
        crc == actual,
        "binary CRC mismatch: payload {crc:08X}, calculated {actual:08X}"
    );
    ensure!(
        parse_hex(claimed)? == u64::from(crc),
        "text CRC disagrees with binary CRC"
    );
    let mut segments = Vec::new();
    let mut reader = Reader::new(&raw, 8);
    for index in 0..usize::from(segment_count) {
        let stat = reader.u16()?;
        let envx = reader.u16()?;
        let first = reader.u16()?;
        let hash = reader.u32()?;
        let samples = (0..raw_count)
            .map(|_| reader.i16())
            .collect::<Result<Vec<_>>>()?;
        let label = match SEGMENT_LABELS.get(index) {
            Some(label) => label.to_string(),
            None => format!("SEG{index:02X}"),
        };
        segments.push(Segment {
            label,
            stat,
            envx,
            first,
            hash,
            raw: samples,
        });
    }
    Ok(Capture {
        schema,
        run,
        segments,
        crc,
    })
}

pub fn report(capture: &Capture, out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "# schema=SB4v{} run={:02X} crc={:08X}",
        capture.schema, capture.run, capture.crc
    )?;
    writeln!(out, "segment,stat,envx,first,hash")?;
    for seg in &capture.segments {
        let note = if seg.timed_out() { " TIMEOUT" } else { "" };
        writeln!(
            out,
            "{},{:04X},{:04X},{},{:08X}{note}",
            seg.label, seg.stat, seg.envx, seg.first, seg.hash
        )?;
    }
    for seg in &capture.segments {
        let samples: Vec<String> = seg.raw.iter().map(i16::to_string).collect();
        writeln!(out, "# {} raw: {}", seg.label, samples.join(" "))?;
    }
    Ok(())
}

pub fn diff(current: &Capture, baseline: &Capture, out: &mut dyn Write) -> Result<i32> {
    let mut drift = 0;
    if current.segments.len() != baseline.segments.len() {
        writeln!(
            out,
            "DRIFT segment count: {} vs baseline {}",
            current.segments.len(),
            baseline.segments.len()
        )?;
        drift += 1;
    }
    for (seg, base) in current.segments.iter().zip(&baseline.segments) {
        let label = &seg.label;
        if seg.stat != base.stat {
            writeln!(out, "DRIFT {label}.stat: {:04X} was {:04X}", seg.stat, base.stat)?;
            drift += 1;
        }
        if seg.envx != base.envx {
            writeln!(out, "DRIFT {label}.envx: {:04X} was {:04X}", seg.envx, base.envx)?;
            drift += 1;
        }
        if seg.first != base.first {
            writeln!(out, "DRIFT {label}.first: {:04X} was {:04X}", seg.first, base.first)?;
            drift += 1;
        }
        if seg.hash != base.hash {
            writeln!(out, "DRIFT {label}.hash: {:08X} was {:08X}", seg.hash, base.hash)?;
            drift += 1;
        }
        if seg.raw != base.raw {
            let moved: Vec<usize> = seg
                .raw
                .iter()
                .zip(&base.raw)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, _)| i)
                .collect();
            let Some(first) = moved.first() else {
                bail!("list index out of range");
            };
            writeln!(
                out,
                "DRIFT {label}.raw: {} sample(s), first at {first}",
                moved.len()
            )?;
            drift += 1;
        }
    }
    writeln!(out, "# drift={drift}")?;
    Ok(i32::from(drift != 0))
}

fn read_capture(path: &str) -> Result<Capture> {
    let bytes = std::fs::read(path).map_err(|e| Error(format!("{path}: {e}")))?;
    parse(&String::from_utf8(bytes).map_err(|e| Error(e.to_string()))?)
}

/// `psoxide-hwtest sb4-report <capture> [--baseline FILE] [--fail-on-change]`
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let mut capture = None;
    let mut baseline = None;
    let mut fail_on_change = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--baseline" => {
                i += 1;
                baseline = Some(args.get(i).ok_or("--baseline needs a value")?.clone());
            }
            "--fail-on-change" => fail_on_change = true,
            flag if flag.starts_with("--") => bail!("unrecognized arguments: {flag}"),
            path if capture.is_none() => capture = Some(path.to_string()),
            extra => bail!("unrecognized arguments: {extra}"),
        }
        i += 1;
    }
    let capture = capture.ok_or("the following arguments are required: capture")?;
    let current = read_capture(&capture)?;
    report(&current, out)?;
    let Some(baseline) = baseline else {
        return Ok(0);
    };
    let baseline = read_capture(&baseline)?;
    let result = diff(&current, &baseline, out)?;
    Ok(if fail_on_change { result } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::base64_encode;

    pub fn payload(segments: &[(u16, u16, u16, u32, Vec<i16>)], run: u8) -> String {
        let raw_count = segments.first().map_or(0, |s| s.4.len()) as u8;
        let mut raw = b"SB4B".to_vec();
        raw.extend([1, segments.len() as u8, raw_count, run]);
        for (stat, envx, first, hash, samples) in segments {
            raw.extend(stat.to_le_bytes());
            raw.extend(envx.to_le_bytes());
            raw.extend(first.to_le_bytes());
            raw.extend(hash.to_le_bytes());
            for sample in samples {
                raw.extend(sample.to_le_bytes());
            }
        }
        let crc = crc32(&raw);
        raw.extend(crc.to_le_bytes());
        format!("boot log\nSB4/{}/C:{crc:08X}\ntrailer", base64_encode(&raw))
    }

    #[test]
    fn a_capture_round_trips_and_reports() {
        let text = payload(
            &[(0x8001, 2, 3, 0xDEAD_BEEF, vec![1, -2, 3]), (0, 0, 0, 7, vec![0, 0, 0])],
            9,
        );
        let capture = parse(&text).unwrap();
        assert_eq!(capture.segments[0].label, "SQUARE");
        assert!(capture.segments[0].timed_out());
        let mut out = Vec::new();
        report(&capture, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("SQUARE,8001,0002,3,DEADBEEF TIMEOUT"));
        assert!(out.contains("# SQUARE raw: 1 -2 3"));
    }

    #[test]
    fn drift_names_each_moved_field() {
        let base = parse(&payload(&[(1, 2, 3, 4, vec![5, 6])], 1)).unwrap();
        let moved = parse(&payload(&[(1, 2, 3, 9, vec![5, 7])], 2)).unwrap();
        let mut out = Vec::new();
        assert_eq!(diff(&moved, &base, &mut out).unwrap(), 1);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("DRIFT SQUARE.hash: 00000009 was 00000004"));
        assert!(out.contains("DRIFT SQUARE.raw: 1 sample(s), first at 1"));
        assert!(out.contains("# drift=2"));
        let mut quiet = Vec::new();
        assert_eq!(diff(&base, &base, &mut quiet).unwrap(), 0);
    }

    #[test]
    fn corrupt_payloads_are_errors() {
        assert!(parse("nothing here").is_err());
        let good = payload(&[(1, 2, 3, 4, vec![5])], 1);
        assert!(parse(&good.replacen("/C:", "/C:F", 1)).is_err());
        let flipped = good.replacen("SB4/", "SB4/A", 1);
        assert!(parse(&flipped).is_err());
    }
}
