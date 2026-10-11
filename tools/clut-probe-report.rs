// SPDX-License-Identifier: GPL-2.0-or-later
//! Standalone, dependency-free PX8 verifier for the direct CLUT observations.
//! Build with `rustc --edition=2021 tools/clut-probe-report.rs -o clut-probe-report`.

use std::collections::BTreeMap;

#[derive(Debug)]
struct Capture {
    version: (u8, u8),
    crc: u32,
    pages: usize,
    records: BTreeMap<u16, [u16; 3]>,
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(4) {
        return Err("base64 length".into());
    }
    let mut out = Vec::new();
    let count = text.len() / 4;
    for (index, chunk) in text.as_bytes().chunks_exact(4).enumerate() {
        let mut value = 0u32;
        let mut padding = 0;
        for (position, byte) in chunk.iter().enumerate() {
            let digit = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if index + 1 == count && position >= 2 => {
                    padding += 1;
                    0
                }
                _ => return Err("base64 character".into()),
            };
            if padding != 0 && *byte != b'=' {
                return Err("base64 padding".into());
            }
            value = (value << 6) | u32::from(digit);
        }
        if padding > 2
            || (padding == 1 && value & 0xFF != 0)
            || (padding == 2 && value & 0xFFFF != 0)
        {
            return Err("base64 trailing bits".into());
        }
        out.push((value >> 16) as u8);
        if padding < 2 {
            out.push((value >> 8) as u8);
        }
        if padding == 0 {
            out.push(value as u8);
        }
    }
    Ok(out)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn skip(&mut self, len: usize) -> Result<(), String> {
        self.at = self.at.checked_add(len).ok_or("payload overflow")?;
        if self.at > self.bytes.len() {
            return Err("short payload".into());
        }
        Ok(())
    }
    fn u16(&mut self) -> Result<u16, String> {
        let start = self.at;
        self.skip(2)?;
        Ok(u16::from_le_bytes(
            self.bytes[start..self.at].try_into().unwrap(),
        ))
    }
}

fn parse(input: &str) -> Result<Capture, String> {
    let mut pages = BTreeMap::new();
    let mut identity = None;
    for line in input.lines() {
        let Some(start) = line.find("PX8/") else {
            continue;
        };
        let page = line[start..]
            .split_whitespace()
            .next()
            .ok_or("empty page")?;
        let (body, crc_text) = page.rsplit_once("/C:").ok_or("page CRC missing")?;
        let mut fields = body.splitn(3, '/');
        let _ = fields.next();
        let header = fields.next().ok_or("page header missing")?;
        let chunk = fields.next().ok_or("page chunk missing")?;
        if header.len() != 8 || !header.is_ascii() {
            return Err("page header".into());
        }
        let number = usize::from_str_radix(&header[..2], 16).map_err(|_| "page number")?;
        let total = usize::from_str_radix(&header[2..4], 16).map_err(|_| "page total")?;
        let run = u16::from_str_radix(&header[4..], 16).map_err(|_| "run id")?;
        if number == 0 || number > total {
            return Err("page range".into());
        }
        if identity.is_some_and(|old| old != (total, run)) {
            return Err("mixed page totals or run ids".into());
        }
        identity = Some((total, run));
        let claimed = u32::from_str_radix(crc_text, 16).map_err(|_| "page CRC")?;
        if crc32(chunk.as_bytes()) != claimed {
            return Err("page CRC mismatch".into());
        }
        if pages.insert(number, chunk).is_some_and(|old| old != chunk) {
            return Err("conflicting duplicate page".into());
        }
    }
    let (total, _) = identity.ok_or("no PX8 pages")?;
    if pages.len() != total {
        return Err("incomplete PX8 pages".into());
    }
    let encoded: String = pages.values().copied().collect();
    let bytes = base64(&encoded)?;
    if bytes.len() < 76 || &bytes[..5] != b"PX8B\x04" {
        return Err("binary header".into());
    }
    let end = bytes.len() - 4;
    let crc = u32::from_le_bytes(bytes[end..].try_into().unwrap());
    if crc32(&bytes[..end]) != crc {
        return Err("binary CRC mismatch".into());
    }
    let flags = bytes[7];
    let counts = |at| usize::from(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()));
    let mut cursor = Cursor {
        bytes: &bytes[..end],
        at: 36 + usize::from(bytes[19]) * 12,
    };
    cursor.skip(0)?;
    if flags & 1 != 0 {
        cursor.skip((counts(10) * usize::from(bytes[18])).div_ceil(8))?;
    }
    if flags & 2 != 0 {
        let count = usize::from(cursor.u16()?);
        cursor.skip(count * 10)?;
    }
    if flags & 4 != 0 {
        cursor.skip(counts(10) * 4)?;
    }
    let mut records = BTreeMap::new();
    if flags & 8 != 0 {
        for _ in 0..counts(12) {
            let start = cursor.at;
            cursor.skip(1)?;
            let id = u16::from(cursor.bytes[start]);
            let values = [cursor.u16()?, cursor.u16()?, cursor.u16()?];
            if id != 255 && records.insert(id, values).is_some() {
                return Err("duplicate record id".into());
            }
        }
    }
    if flags & 16 != 0 {
        cursor.skip(counts(14) * 4)?;
    }
    if flags & 32 != 0 {
        cursor.skip(counts(16) * 4)?;
    }
    if flags & 64 != 0 {
        let count = cursor.u16()?;
        for _ in 0..count {
            let id = cursor.u16()?;
            let values = [cursor.u16()?, cursor.u16()?, cursor.u16()?];
            if records.insert(id, values).is_some() {
                return Err("duplicate record id".into());
            }
        }
    }
    if cursor.at != end {
        return Err("binary trailing data".into());
    }
    Ok(Capture {
        version: (bytes[5], bytes[6]),
        crc,
        pages: total,
        records,
    })
}

const GROUPS: [&str; 5] = [
    "4bpp_rect_1",
    "8bpp_rect_1",
    "8bpp_rect_17",
    "4bpp_tri_1",
    "8bpp_tri_17",
];
const VARIANTS: [&str; 5] = [
    "same_queued",
    "same_drained",
    "gp0_clear",
    "changed_word",
    "changed_then_return",
];

fn report(capture: &Capture) -> Result<(), String> {
    if capture.version != (2, 5) {
        return Err("expected suite v2.5".into());
    }
    if capture.records.get(&0x41E) != Some(&[0, 0, 0]) {
        return Err("record integrity missing or failed".into());
    }
    println!(
        "suite=2.5 pages={} binary_crc={:08X} records={}",
        capture.pages,
        capture.crc,
        capture.records.len()
    );
    println!("id,sample,variant,first_555,second_555,descriptor_flags,observation");
    let mut errors = Vec::new();
    for (group, name) in GROUPS.iter().enumerate() {
        for (variant, operation) in VARIANTS.iter().enumerate() {
            let id = 0xA00 + (group * 5 + variant) as u16;
            let values = capture
                .records
                .get(&id)
                .ok_or_else(|| format!("missing row {id:03X}"))?;
            let [first, second, flags] = *values;
            let descriptor = (group * 8 + variant) as u16;
            let observation = match second {
                0x001F => "stale_red",
                0x7C00 => "fresh_blue",
                _ => "unexpected",
            };
            println!(
                "{id:03X},{name},{operation},{first:04X},{second:04X},{flags:04X},{observation}"
            );
            if first != 0x001F
                || flags != descriptor
                || !matches!(second, 0x001F | 0x7C00)
                || (variant >= 3 && second != 0x7C00)
            {
                errors.push(format!("invalid row {id:03X}"));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(", "))
    }
}

fn main() -> std::process::ExitCode {
    let result = (|| {
        let path = std::env::args()
            .nth(1)
            .ok_or("usage: clut-probe-report <PX8 pages or TTY log>")?;
        let input = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        report(&parse(&input)?)
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clut-probe-report: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crc_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
    #[test]
    fn base64_padding_is_strict() {
        assert_eq!(base64("AQ==").unwrap(), [1]);
        assert_eq!(base64("AQI=").unwrap(), [1, 2]);
        assert_eq!(base64("AQID").unwrap(), [1, 2, 3]);
        for bad in ["A", "A===", "AQ=A", "AR==", "AQJ=", "AQ==AQ=="] {
            assert!(base64(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn bad_and_incomplete_pages_are_rejected() {
        assert!(parse("PX8/0101ABCD/AAAA/C:00000000")
            .unwrap_err()
            .contains("CRC"));
        let chunk = "AAAA";
        let page = format!("PX8/0102ABCD/{chunk}/C:{:08X}", crc32(chunk.as_bytes()));
        assert!(parse(&page).unwrap_err().contains("incomplete"));
    }
    #[test]
    fn observation_does_not_assume_stale_or_fresh() {
        for colour in [0x001F, 0x7C00] {
            let mut records = BTreeMap::from([(0x41E, [0, 0, 0])]);
            for group in 0..5 {
                for variant in 0..5 {
                    let second = if variant >= 3 { 0x7C00 } else { colour };
                    records.insert(
                        0xA00 + group * 5 + variant,
                        [0x001F, second, group * 8 + variant],
                    );
                }
            }
            let mut capture = Capture {
                version: (2, 5),
                crc: 0,
                pages: 1,
                records,
            };
            assert!(report(&capture).is_ok());
            capture.records.get_mut(&0xA00).unwrap()[2] |= 1 << 8;
            assert!(report(&capture).is_err());
        }
    }
}
