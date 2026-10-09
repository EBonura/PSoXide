//! Archived captures in docs/hardware-refs must keep parsing to the same
//! summary, so a parser edit cannot silently orphan a console run.

use std::path::{Path, PathBuf};

use psoxide_hwtest::report::{parse_capture, payloads_from_paths};

fn refs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/hardware-refs")
}

/// file -> (schema, suite minor, timing records, whole-binary CRC)
const ARCHIVED: [(&str, &str, u8, usize, u32); 21] = [
    ("px7-emulator-v1.5.txt", "PX7", 5, 151, 0x5245A296),
    ("px7-emulator-v1.6.txt", "PX7", 6, 151, 0x056F8EC8),
    ("px7-emulator-v1.7.txt", "PX7", 6, 151, 0x39FFC9DD),
    ("px7-emulator-v1.8.txt", "PX7", 8, 151, 0xEE090F6D),
    ("px7-silicon-2026-07-26.txt", "PX7", 4, 131, 0xB7EFA355),
    (
        "px7-silicon-2026-07-31-v1.6-partial.txt",
        "PX7",
        6,
        55,
        0x4AFD3764,
    ),
    (
        "px7-silicon-2026-07-31-v1.7-full.txt",
        "PX7",
        6,
        151,
        0xB0A8FC88,
    ),
    ("px7-silicon-v1.5-2026-07-26.txt", "PX7", 5, 151, 0x0A28B12F),
    ("px8-emulator-v1.14.txt", "PX8", 14, 0, 0x6D93820A),
    ("px8-emulator-v1.15.txt", "PX8", 15, 0, 0x964DA405),
    ("px8-emulator-v1.16.txt", "PX8", 16, 0, 0x624AEC02),
    ("px8-emulator-v1.17.txt", "PX8", 17, 0, 0x7B1CED9C),
    ("px8-emulator-v1.18.txt", "PX8", 18, 0, 0x1D984FC5),
    ("px8-emulator-v1.19.txt", "PX8", 19, 0, 0xC7704842),
    ("px8-emulator-v1.20.txt", "PX8", 20, 0, 0x752E799B),
    (
        "px8-silicon-2026-08-07-v1.17-full.txt",
        "PX8",
        17,
        151,
        0x5C8F0460,
    ),
    (
        "px8-silicon-2026-09-17-v1.22-full.txt",
        "PX8",
        22,
        179,
        0xE0F65995,
    ),
    (
        "px8-silicon-2026-09-17-v1.22-perf-sweep.txt",
        "PX8",
        22,
        87,
        0x7F53654B,
    ),
    (
        "px8-silicon-2026-09-17-v1.22-perf-ab.txt",
        "PX8",
        22,
        108,
        0x0A7B83CE,
    ),
    (
        "px8-silicon-2026-09-17-v1.23-perf-sweep.txt",
        "PX8",
        23,
        116,
        0x1C081CF8,
    ),
    (
        "px8-silicon-2026-09-17-v1.23-perf-ab.txt",
        "PX8",
        23,
        137,
        0x8C571D68,
    ),
];

fn archived() -> impl Iterator<Item = &'static (&'static str, &'static str, u8, usize, u32)> {
    ARCHIVED.iter()
}

#[test]
fn every_archived_capture_is_listed() {
    let on_disk: Vec<String> = std::fs::read_dir(refs())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    for (name, ..) in archived() {
        assert!(
            on_disk.iter().any(|file| file == name),
            "listed capture {name} missing from docs/hardware-refs"
        );
    }
}

#[test]
fn archived_captures_parse_to_the_same_summary() {
    for (name, schema, minor, records, crc) in archived() {
        let path = refs().join(name).to_string_lossy().into_owned();
        let capture = parse_capture(&payloads_from_paths(&[path]).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(capture.schema, *schema, "{name}");
        assert_eq!(capture.suite_minor, *minor, "{name}");
        assert_eq!(capture.records.len(), *records, "{name}");
        assert_eq!(capture.binary_crc, *crc, "{name}");
    }
}

#[test]
fn a_corrupted_page_is_rejected() {
    let path = refs()
        .join("px8-emulator-v1.20.txt")
        .to_string_lossy()
        .into_owned();
    let page = payloads_from_paths(&[path]).unwrap().remove(0);
    let (body, crc) = page.rsplit_once("/C:").unwrap();
    let last = body.chars().last().unwrap();
    let flipped = format!(
        "{}{}",
        &body[..body.len() - 1],
        if last != 'A' { 'A' } else { 'B' }
    );
    assert!(parse_capture(&[format!("{flipped}/C:{crc}")]).is_err());
}
