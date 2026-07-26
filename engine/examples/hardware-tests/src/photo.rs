//! Dense photographable transport for the complete real-console result set.
//!
//! PX7 keeps the dense Base64 record used by the debug TTY and report parser,
//! but transports each page visually as a QR symbol. Four scans preserve all
//! 173 conformance observations and statuses, all 128 timing envelopes, the
//! memory-control snapshot, startup scan summaries, and 128 raw precision
//! values without manual character transcription. Page and binary CRC-32
//! values provide independent end-to-end validation after QR decoding.

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::tty;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, section_report, Mode, ScanReport, TestResult, TimingReport};

pub(crate) const CAPTURE_PAGE_COUNT: usize = 5;
const BASE64_CHARS_PER_LINE: usize = 36;
const BASE64_LINES_PER_PAGE: usize = 23;
const BASE64_CHARS_PER_PAGE: usize = BASE64_CHARS_PER_LINE * BASE64_LINES_PER_PAGE;
const QR_VERSION: Version = Version::new(20);
const QR_SIZE: usize = 97;
const QR_BUFFER_LEN: usize = QR_VERSION.buffer_len();
const QR_TEXT_MAX: usize = 9 + BASE64_CHARS_PER_PAGE + 3 + 8;
const QR_SCALE: i16 = 2;
const QR_QUIET: i16 = 4;

// PX7 binary layout, little-endian:
//   66-byte header (includes the suite version, not just the schema version)
//   173 x u32 complete conformance observations
//   173 x packed 3-bit statuses (65 bytes)
//   176 x (u8 id, u16 minimum, u16 median, u16 maximum) timing records
//   9 x u32 memory-control registers
//   192 x u32 raw precision values
//   u32 CRC-32 over every preceding byte
// PX6 carried 90 four-byte records in 1,733 bytes over three pages, with each
// record's identity implied by its POSITION. PX7 writes the id explicitly, so
// an unused slot is skippable and adding a probe cannot silently shift every
// later record's meaning. With the median column and room for the CD battery
// that is 2,269 bytes / 3,028 Base64 characters: a fourth page at the same
// proven Version-20-L geometry (4 x 828 = 3,312 characters of capacity).
const BINARY_LEN: usize = 2_863;
const BASE64_LEN: usize = 3_820;

pub(crate) struct PhotoCapture {
    /// The encoded binary, kept so the audio link can transmit the same bytes
    /// the QR pages carry. One payload, two independent readout paths.
    binary: [u8; BINARY_LEN],
    payload: [u8; BASE64_LEN],
    payload_len: u16,
    binary_crc: u32,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
}

impl PhotoCapture {
    pub(crate) const fn new() -> Self {
        Self {
            binary: [0; BINARY_LEN],
            payload: [0; BASE64_LEN],
            payload_len: 0,
            binary_crc: 0,
            qr_modules: [0; (QR_SIZE * QR_SIZE + 7) / 8],
            qr_size: 0,
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

        out.push_bytes(b"PX7B");
        out.push_u8(3); // transport schema version
        // Suite version: what the record ids MEAN, as distinct from how the
        // bytes are laid out. Lets the host refuse a cross-version diff.
        out.push_u8(crate::SUITE_VERSION_MAJOR);
        out.push_u8(crate::SUITE_VERSION_MINOR);
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
            out.push_u8(record.id);
            out.push_u16(record.min);
            out.push_u16(record.med);
            out.push_u16(record.max);
        }
        for value in timing.memory_control {
            out.push_u32(value);
        }
        for value in timing.precision {
            out.push_u32(value);
        }

        let crc = crc32(out.bytes());
        out.push_u32(crc);
        let binary_len = out.len();
        drop(out);
        assert!(binary_len == BINARY_LEN, "PX7 binary layout drift");

        let encoded_len = base64_encode(&binary[..binary_len], &mut self.payload);
        assert!(encoded_len == BASE64_LEN, "PX7 Base64 layout drift");
        self.payload_len = encoded_len as u16;
        self.binary_crc = crc;
        self.binary = binary;
        self.encode_qr(page);

        self.print_page(page);
    }

    /// Re-render an already-encoded capture at a different page.
    ///
    /// Paging must NOT rebuild the payload. Some observations are live (the pad
    /// poll is refreshed every frame from the controller), so re-encoding per
    /// page gave each QR a different payload while only the last page's CRC
    /// described the bytes it was computed over. A five-page capture could
    /// therefore never reconstruct, which is exactly what console captures did:
    /// every page decoded cleanly and the whole-binary CRC still failed.
    pub(crate) fn render_page(&mut self, page: usize) {
        self.encode_qr(page);
        self.print_page(page);
    }

    fn encode_qr(&mut self, page: usize) {
        let mut text = [0u8; QR_TEXT_MAX];
        let mut len = 0usize;
        append(&mut text, &mut len, b"PX7/");
        append(
            &mut text,
            &mut len,
            hex2((page + 1) as u8).as_str().as_bytes(),
        );
        append(
            &mut text,
            &mut len,
            hex2(CAPTURE_PAGE_COUNT as u8).as_str().as_bytes(),
        );
        append(&mut text, &mut len, b"/");
        append(&mut text, &mut len, self.page_chunk(page).as_bytes());
        append(&mut text, &mut len, b"/C:");
        append(
            &mut text,
            &mut len,
            hex8(self.page_crc(page)).digits().as_bytes(),
        );

        let encoded = unsafe { core::str::from_utf8_unchecked(&text[..len]) };
        let mut temp = [0u8; QR_BUFFER_LEN];
        let mut output = [0u8; QR_BUFFER_LEN];
        let Ok(qr) = QrCode::encode_text(
            encoded,
            &mut temp,
            &mut output,
            QrCodeEcc::Low,
            QR_VERSION,
            QR_VERSION,
            None,
            false,
        ) else {
            self.qr_size = 0;
            return;
        };

        self.qr_modules.fill(0);
        self.qr_size = qr.size() as u8;
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                if qr.get_module(x, y) {
                    let bit = y as usize * QR_SIZE + x as usize;
                    self.qr_modules[bit / 8] |= 1 << (bit & 7);
                }
            }
        }
    }

    /// The exact bytes the QR pages encode, for the audio link.
    pub(crate) fn binary(&self) -> &[u8] {
        &self.binary
    }

    fn qr_module(&self, x: usize, y: usize) -> bool {
        let bit = y * QR_SIZE + x;
        self.qr_modules[bit / 8] & (1 << (bit & 7)) != 0
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

    pub(crate) fn print_page(&self, page: usize) {
        if page >= CAPTURE_PAGE_COUNT {
            return;
        }
        tty::print("hardware-tests: px7 PX7/");
        tty::print(hex2((page + 1) as u8).as_str());
        tty::print(hex2(CAPTURE_PAGE_COUNT as u8).as_str());
        tty::print("/");
        tty::print(self.page_chunk(page));
        tty::print("/C:");
        tty::println(hex8(self.page_crc(page)).digits());
    }
}

pub(crate) fn draw_capture_page(font: &FontAtlas, capture: &PhotoCapture, page: usize) {
    // Title is shortened deliberately: the full string ran into the PAGE
    // counter at x=208, and this header is what the operator reads off the TV
    // to label a capture. A garbled page number is how captures get misfiled.
    font.draw_text(0, 0, "PX7 CAPTURE", (255, 232, 128));
    font.draw_text(208, 0, "PAGE", (140, 160, 190));
    font.draw_text(248, 0, hex2((page + 1) as u8).as_str(), (232, 236, 244));
    font.draw_text(264, 0, "/", (140, 160, 190));
    font.draw_text(
        272,
        0,
        hex2(CAPTURE_PAGE_COUNT as u8).as_str(),
        (232, 236, 244),
    );
    // Labels are abbreviated so the navigation hint fits on the same line.
    // START opening the menu has to be visible somewhere the operator is
    // already looking, and this screen is where they spend the whole capture.
    // Positions assume the 8px advance this font actually has. Labels are
    // abbreviated so the navigation hint fits on the same line: START opening
    // the menu has to be visible where the operator is already looking, and
    // this is the screen they spend the whole capture on.
    font.draw_text(0, 10, "PG", (140, 160, 190));
    font.draw_text(
        20,
        10,
        hex8(capture.page_crc(page)).digits(),
        (96, 240, 128),
    );
    font.draw_text(88, 10, "ALL", (140, 160, 190));
    font.draw_text(116, 10, hex8(capture.binary_crc).digits(), (96, 240, 128));
    font.draw_text(186, 10, "L/R", (150, 170, 200));
    font.draw_text(216, 10, "START MENU", (255, 232, 128));

    if capture.qr_size as usize != QR_SIZE {
        font.draw_text(80, 112, "QR ENCODE FAILED", (255, 96, 96));
        return;
    }

    let total = (QR_SIZE as i16 + QR_QUIET * 2) * QR_SCALE;
    let left = (320 - total) / 2;
    let top = 28;
    gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
    let data_left = left + QR_QUIET * QR_SCALE;
    let data_top = top + QR_QUIET * QR_SCALE;
    for y in 0..QR_SIZE {
        let mut x = 0usize;
        while x < QR_SIZE {
            while x < QR_SIZE && !capture.qr_module(x, y) {
                x += 1;
            }
            let first = x;
            while x < QR_SIZE && capture.qr_module(x, y) {
                x += 1;
            }
            if first < x {
                gpu::draw_rect_flat(
                    data_left + first as i16 * QR_SCALE,
                    data_top + y as i16 * QR_SCALE,
                    ((x - first) as i16 * QR_SCALE) as u16,
                    QR_SCALE as u16,
                    0,
                    0,
                    0,
                );
            }
        }
    }
}

fn append(target: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let end = *len + bytes.len();
    target[*len..end].copy_from_slice(bytes);
    *len = end;
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
        assert!(self.len < self.bytes.len(), "PX7 binary overflow");
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
        assert!(target + 4 <= output.len(), "PX7 Base64 overflow");
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

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
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
