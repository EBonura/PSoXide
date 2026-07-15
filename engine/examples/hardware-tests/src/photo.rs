//! Dense photographable transport for the complete real-console result set.
//!
//! The known 8x8 font is clearly readable in a phone photograph, so PX5 uses
//! the screen for line-numbered Base64 instead of spending most of the pixels
//! on QR geometry and forward error correction. Two photographs preserve all
//! 173 conformance observations and statuses, all 90 timing envelopes, the
//! memory-control snapshot, and the startup scan summaries. CRC-32 detects a
//! transcription error without reducing the visible data density.

use psx_font::FontAtlas;
use psx_rt::tty;

use crate::{hex2, hex8, section_report, Mode, ScanReport, TestResult, TimingReport};

pub(crate) const CAPTURE_PAGE_COUNT: usize = 2;
const BASE64_CHARS_PER_LINE: usize = 36;
const BASE64_LINES_PER_PAGE: usize = 23;
const BASE64_CHARS_PER_PAGE: usize = BASE64_CHARS_PER_LINE * BASE64_LINES_PER_PAGE;

// PX5 binary layout, little-endian:
//   64-byte header
//   173 x u32 complete conformance observations
//   173 x packed 3-bit statuses (65 bytes)
//   90 x (u16 minimum, u16 maximum) timing records
//   9 x u32 memory-control registers
//   u32 CRC-32 over every preceding byte
// The resulting 1221 bytes are divisible by three and encode to exactly 1628
// Base64 characters, comfortably split over the two text pages.
const BINARY_LEN: usize = 1_221;
const BASE64_LEN: usize = 1_628;

pub(crate) struct PhotoCapture {
    payload: [u8; BASE64_LEN],
    payload_len: u16,
    binary_crc: u32,
}

impl PhotoCapture {
    pub(crate) const fn new() -> Self {
        Self {
            payload: [0; BASE64_LEN],
            payload_len: 0,
            binary_crc: 0,
        }
    }

    pub(crate) fn encode(
        &mut self,
        timing: &TimingReport,
        results: &[TestResult; crate::TEST_COUNT],
        conformance_run: u8,
        scans: [ScanReport; 3],
        page: usize,
    ) {
        let mut binary = [0u8; BINARY_LEN];
        let mut out = BinaryBuffer::new(&mut binary);

        out.push_bytes(b"PX5B");
        out.push_u8(1); // schema version
        out.push_u8(conformance_run);
        out.push_u8(timing.summary.runs);
        out.push_u8(crate::TEST_COUNT as u8);
        out.push_u8(crate::TIMING_RECORD_COUNT as u8);
        out.push_u8(crate::MEMORY_CONTROL_REGISTER_COUNT as u8);
        out.push_u8(3); // status bits per conformance case
        out.push_u8(scans.len() as u8);
        out.push_u32(section_report(Mode::AllChecks, results).hash);
        out.push_u32(section_report(Mode::GteChecks, results).hash);
        out.push_u32(timing.summary.hash);
        out.push_u32(timing.summary.aux);

        for scan in scans {
            out.push_u8(scan.status.code() as u8);
            out.push_u16(scan.items);
            out.push_u32(scan.hash);
            out.push_u32(scan.aux);
            out.push_u8(scan.runs);
        }

        for result in results {
            out.push_u32(result.observed);
        }

        let mut status_acc = 0u32;
        let mut status_bits = 0u8;
        for result in results {
            status_acc |= result.status.code() << status_bits;
            status_bits += 3;
            while status_bits >= 8 {
                out.push_u8(status_acc as u8);
                status_acc >>= 8;
                status_bits -= 8;
            }
        }
        if status_bits != 0 {
            out.push_u8(status_acc as u8);
        }

        for record in timing.records {
            out.push_u16(record.min);
            out.push_u16(record.max);
        }
        for value in timing.memory_control {
            out.push_u32(value);
        }

        let crc = crc32(out.bytes());
        out.push_u32(crc);
        let binary_len = out.len();
        drop(out);
        assert!(binary_len == BINARY_LEN, "PX5 binary layout drift");

        let encoded_len = base64_encode(&binary[..binary_len], &mut self.payload);
        assert!(encoded_len == BASE64_LEN, "PX5 Base64 layout drift");
        self.payload_len = encoded_len as u16;
        self.binary_crc = crc;

        self.print_page(page);
    }

    fn payload(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.payload[..self.payload_len as usize]) }
    }

    fn page_chunk(&self, page: usize) -> &str {
        let payload = self.payload();
        let first = page
            .saturating_mul(BASE64_CHARS_PER_PAGE)
            .min(payload.len());
        let end = (first + BASE64_CHARS_PER_PAGE).min(payload.len());
        &payload[first..end]
    }

    fn page_crc(&self, page: usize) -> u32 {
        crc32(self.page_chunk(page).as_bytes())
    }

    fn print_page(&self, page: usize) {
        if page >= CAPTURE_PAGE_COUNT {
            return;
        }
        tty::print("hardware-tests: px5 PX5/");
        tty::print(hex2((page + 1) as u8).as_str());
        tty::print(hex2(CAPTURE_PAGE_COUNT as u8).as_str());
        tty::print("/");
        tty::print(self.page_chunk(page));
        tty::print("/C:");
        tty::println(hex8(self.page_crc(page)).digits());
    }
}

pub(crate) fn draw_capture_page(font: &FontAtlas, capture: &PhotoCapture, page: usize) {
    font.draw_text(8, 28, "PX5 BASE64 COMPLETE CAPTURE", (255, 232, 128));
    font.draw_text(224, 28, "PAGE", (140, 160, 190));
    font.draw_text(264, 28, hex2((page + 1) as u8).as_str(), (232, 236, 244));
    font.draw_text(280, 28, "/", (140, 160, 190));
    font.draw_text(
        288,
        28,
        hex2(CAPTURE_PAGE_COUNT as u8).as_str(),
        (232, 236, 244),
    );

    let chunk = capture.page_chunk(page);
    let mut row = 0usize;
    while row < BASE64_LINES_PER_PAGE {
        let first = row * BASE64_CHARS_PER_LINE;
        if first >= chunk.len() {
            break;
        }
        let end = (first + BASE64_CHARS_PER_LINE).min(chunk.len());
        let y = 38 + row as i16 * 8;
        font.draw_text(0, y, hex2(row as u8).as_str(), (120, 176, 255));
        font.draw_text(16, y, " ", (140, 160, 190));
        font.draw_text(24, y, &chunk[first..end], (232, 236, 244));
        row += 1;
    }

    font.draw_text(0, 224, "PAGE CRC", (140, 160, 190));
    font.draw_text(
        72,
        224,
        hex8(capture.page_crc(page)).digits(),
        (96, 240, 128),
    );
    font.draw_text(152, 224, "FULL CRC", (140, 160, 190));
    font.draw_text(224, 224, hex8(capture.binary_crc).digits(), (96, 240, 128));
    font.draw_text(
        0,
        232,
        "LEFT/RIGHT PAGE  PHOTOGRAPH BOTH PAGES",
        (150, 170, 200),
    );
}

struct BinaryBuffer<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> BinaryBuffer<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn push_u8(&mut self, value: u8) {
        assert!(self.len < self.bytes.len(), "PX5 binary overflow");
        self.bytes[self.len] = value;
        self.len += 1;
    }

    fn push_u16(&mut self, value: u16) {
        self.push_bytes(&value.to_le_bytes());
    }

    fn push_u32(&mut self, value: u32) {
        self.push_bytes(&value.to_le_bytes());
    }

    fn push_bytes(&mut self, values: &[u8]) {
        for &value in values {
            self.push_u8(value);
        }
    }
}

fn base64_encode(input: &[u8], output: &mut [u8]) -> usize {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut source = 0usize;
    let mut target = 0usize;
    while source < input.len() {
        let remaining = input.len() - source;
        let a = input[source];
        let b = if remaining > 1 { input[source + 1] } else { 0 };
        let c = if remaining > 2 { input[source + 2] } else { 0 };
        assert!(target + 4 <= output.len(), "PX5 Base64 overflow");
        output[target] = ALPHABET[(a >> 2) as usize];
        output[target + 1] = ALPHABET[(((a & 0x03) << 4) | (b >> 4)) as usize];
        output[target + 2] = if remaining > 1 {
            ALPHABET[(((b & 0x0F) << 2) | (c >> 6)) as usize]
        } else {
            b'='
        };
        output[target + 3] = if remaining > 2 {
            ALPHABET[(c & 0x3F) as usize]
        } else {
            b'='
        };
        source += remaining.min(3);
        target += 4;
    }
    target
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        let mut bit = 0;
        while bit < 8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            bit += 1;
        }
    }
    !crc
}
