//! Dense photographable transport for the real-console result set.
//!
//! PX8 keeps the dense Base64 record used by the debug TTY and report parser,
//! but transports each page visually as a QR symbol. Page and binary CRC-32
//! values provide independent end-to-end validation after QR decoding.
//!
//! PX7 always carried everything: every conformance observation whether it
//! passed or not, plus every timing envelope and precision value. Those last
//! two are 71% of the payload and they are *characterisation* -- there is no
//! expected value to check them against, silicon is the reference. A routine
//! run does not need them, and as the suite grows towards covering every chip
//! the fixed cost of shipping them on every capture is what limits how many
//! cases can be added.
//!
//! So PX8 makes each block optional behind a flags byte. A conformance capture
//! carries the status bitmap and one record per FAILING case; a
//! characterisation capture carries the lot, exactly as PX7 did and in the same
//! field order, so the archived `docs/hardware-refs/px7-*` captures still
//! describe the same thing a full PX8 does. Pages are counted from the payload
//! rather than fixed, so a capture costs as many photographs as it has data.

use psx_font::FontAtlas;
use psx_gpu as gpu;
use psx_rt::tty;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

use crate::{hex2, hex8, section_report, Mode, ScanReport, TestResult, TimingReport};

/// Most pages a capture can need. Sized for the worst case a full
/// characterisation run can produce with every conformance case failing, which
/// is not a real console but is a real buffer.
pub(crate) const CAPTURE_PAGE_MAX: usize = 9;
const BASE64_CHARS_PER_LINE: usize = 36;
const BASE64_LINES_PER_PAGE: usize = 23;
const BASE64_CHARS_PER_PAGE: usize = BASE64_CHARS_PER_LINE * BASE64_LINES_PER_PAGE;
/// Largest symbol a page can need, and the size every buffer is cut for.
const QR_VERSION_MAX: Version = Version::new(20);
/// Smallest the encoder may choose. A conformance page is a quarter the size of
/// a characterisation page, and forcing it into a 97-module symbol anyway makes
/// it needlessly hard to photograph off a CRT -- which is the step that has cost
/// re-burns. Let the encoder pick, then scale the result up to fill the same
/// screen area, so fewer modules means physically bigger ones.
const QR_VERSION_MIN: Version = Version::new(10);
const QR_SIZE: usize = 97;
const QR_BUFFER_LEN: usize = QR_VERSION_MAX.buffer_len();
const QR_TEXT_MAX: usize = 9 + BASE64_CHARS_PER_PAGE + 3 + 8;
const QR_QUIET: i16 = 4;
/// Vertical room between the two header lines and the bottom of the screen.
const QR_AREA: i16 = 210;

/// Which blocks a capture carries, in the header's `flags` byte.
///
/// A reader must consult these before walking the body: an absent block
/// occupies no bytes at all rather than being zero-filled, which is the whole
/// point of the flags.
pub(crate) mod blocks {
    /// Packed 3-bit status per conformance case. Always present: without it a
    /// capture cannot say what ran, only what broke.
    pub const STATUS: u8 = 1 << 0;
    /// One record per failing case: id, expected, observed.
    pub const FAILURES: u8 = 1 << 1;
    /// Observed value for *every* case, passing or not.
    pub const OBSERVED: u8 = 1 << 2;
    /// Timing envelopes: min/median/max per record id.
    pub const TIMING: u8 = 1 << 3;
    /// Memory-control register snapshot.
    pub const MEMCTL: u8 = 1 << 4;
    /// Raw precision values.
    pub const PRECISION: u8 = 1 << 5;

    /// What a routine run emits: verdicts, and detail only where it failed.
    pub const CONFORMANCE: u8 = STATUS | FAILURES;
    /// What a reference-establishing run emits: everything PX7 carried.
    pub const FULL: u8 = STATUS | FAILURES | OBSERVED | TIMING | MEMCTL | PRECISION;
}

// PX8 binary layout, little-endian. Every block after the header is present
// only if its flag is set:
//   72-byte header (includes the suite version, not just the schema version)
//   [STATUS]    ceil(TEST_COUNT * 3 / 8) bytes of packed 3-bit statuses
//   [FAILURES]  u16 count, then count x (u16 id, u32 expected, u32 observed)
//   [OBSERVED]  TEST_COUNT x u32 conformance observations
//   [TIMING]    TIMING_RECORD_COUNT x (u8 id, u16 min, u16 median, u16 max)
//   [MEMCTL]    9 x u32 memory-control registers
//   [PRECISION] 192 x u32 raw precision values
//   u32 CRC-32 over every preceding byte
//
// The counts in the header are u16 where PX7 had u8. The suite is on its way to
// covering every chip in the console, and a case count that silently wraps at
// 256 is the kind of thing that is discovered from a decoded capture that makes
// no sense rather than from a build error.
//
// A failure record names its case by `TestSpec::id`, not by array position, so
// a capture archived today still points at the same test after the array grows.
const FAILURE_RECORD_LEN: usize = 2 + 4 + 4;
const STATUS_LEN: usize = (crate::TEST_COUNT * 3 + 7) / 8;
const HEADER_LEN: usize = 72;
/// Worst case: a full characterisation run in which every case also failed.
const BINARY_CAP: usize = HEADER_LEN
    + STATUS_LEN
    + 2
    + crate::TEST_COUNT * FAILURE_RECORD_LEN
    + crate::TEST_COUNT * 4
    + crate::TIMING_RECORD_COUNT * 7
    + crate::MEMORY_CONTROL_REGISTER_COUNT * 4
    + crate::PRECISION_VALUE_COUNT * 4
    + 4;
const BASE64_CAP: usize = (BINARY_CAP + 2) / 3 * 4;
const _: () = assert!(
    (BASE64_CAP + BASE64_CHARS_PER_PAGE - 1) / BASE64_CHARS_PER_PAGE <= CAPTURE_PAGE_MAX,
    "worst-case capture needs more pages than CAPTURE_PAGE_MAX"
);

pub(crate) struct PhotoCapture {
    /// The encoded binary, kept so the audio link can transmit the same bytes
    /// the QR pages carry. One payload, two independent readout paths.
    binary: [u8; BINARY_CAP],
    binary_len: u16,
    payload: [u8; BASE64_CAP],
    payload_len: u16,
    page_count: u8,
    flags: u8,
    failures: u16,
    binary_crc: u32,
    qr_modules: [u8; (QR_SIZE * QR_SIZE + 7) / 8],
    qr_size: u8,
}

impl PhotoCapture {
    pub(crate) const fn new() -> Self {
        Self {
            binary: [0; BINARY_CAP],
            binary_len: 0,
            payload: [0; BASE64_CAP],
            payload_len: 0,
            page_count: 0,
            flags: 0,
            failures: 0,
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
        flags: u8,
        page: usize,
    ) {
        let mut binary = [0u8; BINARY_CAP];
        let mut out = BinaryBuffer::new(&mut binary);

        out.push_bytes(b"PX8B");
        out.push_u8(4); // transport schema version
        // Suite version: what the record ids MEAN, as distinct from how the
        // bytes are laid out. Lets the host refuse a cross-version diff.
        out.push_u8(crate::SUITE_VERSION_MAJOR);
        out.push_u8(crate::SUITE_VERSION_MINOR);
        out.push_u8(flags);
        out.push_u8(conformance_run);
        out.push_u8(timing.summary.runs);
        out.push_u16(crate::TEST_COUNT as u16);
        out.push_u16(crate::TIMING_RECORD_COUNT as u16);
        out.push_u16(crate::MEMORY_CONTROL_REGISTER_COUNT as u16);
        out.push_u16(crate::PRECISION_VALUE_COUNT as u16);
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
        assert!(out.len() == HEADER_LEN, "PX8 header layout drift");

        if flags & blocks::STATUS != 0 {
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
        }

        let mut failures = 0u16;
        if flags & blocks::FAILURES != 0 {
            for result in results {
                if result.status.is_failure() {
                    failures += 1;
                }
            }
            out.push_u16(failures);
            for (index, result) in results.iter().enumerate() {
                if result.status.is_failure() {
                    out.push_u16(crate::TESTS[index].id);
                    out.push_u32(result.expected);
                    out.push_u32(result.observed);
                }
            }
        }

        if flags & blocks::OBSERVED != 0 {
            for result in results {
                out.push_u32(result.observed);
            }
        }
        if flags & blocks::TIMING != 0 {
            for record in timing.records {
                out.push_u8(record.id);
                out.push_u16(record.min);
                out.push_u16(record.med);
                out.push_u16(record.max);
            }
        }
        if flags & blocks::MEMCTL != 0 {
            for value in timing.memory_control {
                out.push_u32(value);
            }
        }
        if flags & blocks::PRECISION != 0 {
            for value in timing.precision {
                out.push_u32(value);
            }
        }

        let crc = crc32(out.bytes());
        out.push_u32(crc);
        let binary_len = out.len();
        drop(out);

        let encoded_len = base64_encode(&binary[..binary_len], &mut self.payload);
        self.binary_len = binary_len as u16;
        self.payload_len = encoded_len as u16;
        self.page_count =
            ((encoded_len + BASE64_CHARS_PER_PAGE - 1) / BASE64_CHARS_PER_PAGE).max(1) as u8;
        self.flags = flags;
        self.failures = failures;
        self.binary_crc = crc;
        self.binary = binary;
        self.encode_qr(page);

        self.print_page(page);
    }

    /// Never zero: page navigation divides by this, and it is read before the
    /// first capture exists.
    pub(crate) fn page_count(&self) -> usize {
        (self.page_count as usize).max(1)
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
        append(&mut text, &mut len, b"PX8/");
        append(
            &mut text,
            &mut len,
            hex2((page + 1) as u8).as_str().as_bytes(),
        );
        append(
            &mut text,
            &mut len,
            hex2(self.page_count as u8).as_str().as_bytes(),
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
            QR_VERSION_MIN,
            QR_VERSION_MAX,
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
    ///
    /// Sliced to the encoded length: the backing array is cut for the
    /// worst-case capture, and handing the whole thing to the audio link
    /// once made its frame outgrow SPU RAM, so it silently sent nothing.
    pub(crate) fn binary(&self) -> &[u8] {
        &self.binary[..self.binary_len as usize]
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
        if page >= self.page_count() {
            return;
        }
        tty::print("hardware-tests: px8 PX8/");
        tty::print(hex2((page + 1) as u8).as_str());
        tty::print(hex2(self.page_count as u8).as_str());
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
    // Which capture this is, because the two kinds are told apart by the
    // operator on the day and by the filename forever after. A conformance page
    // photographed and filed as a characterisation reference is a diff against
    // nothing.
    let (title, tint) = if capture.flags & blocks::OBSERVED != 0 {
        ("PX8 FULL", (255, 232, 128))
    } else if capture.failures == 0 {
        ("PX8 CONF - ALL PASS", (96, 240, 128))
    } else {
        ("PX8 CONF - FAILURES", (255, 128, 96))
    };
    font.draw_text(0, 0, title, tint);
    font.draw_text(208, 0, "PAGE", (140, 160, 190));
    font.draw_text(248, 0, hex2((page + 1) as u8).as_str(), (232, 236, 244));
    font.draw_text(264, 0, "/", (140, 160, 190));
    font.draw_text(
        272,
        0,
        hex2(capture.page_count).as_str(),
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

    if capture.qr_size == 0 {
        font.draw_text(80, 112, "QR ENCODE FAILED", (255, 96, 96));
        return;
    }

    let modules = capture.qr_size as i16;
    // Largest whole-pixel module that still fits. Whole pixels only: a
    // fractional scale puts module edges between pixels, and a QR read off a
    // photograph of a CRT has little enough contrast already.
    let scale = (QR_AREA / (modules + QR_QUIET * 2)).max(1);
    let total = (modules + QR_QUIET * 2) * scale;
    let left = (320 - total) / 2;
    let top = 28;
    gpu::draw_rect_flat(left, top, total as u16, total as u16, 255, 255, 255);
    let data_left = left + QR_QUIET * scale;
    let data_top = top + QR_QUIET * scale;
    let side = modules as usize;
    for y in 0..side {
        let mut x = 0usize;
        while x < side {
            while x < side && !capture.qr_module(x, y) {
                x += 1;
            }
            let first = x;
            while x < side && capture.qr_module(x, y) {
                x += 1;
            }
            if first < x {
                gpu::draw_rect_flat(
                    data_left + first as i16 * scale,
                    data_top + y as i16 * scale,
                    ((x - first) as i16 * scale) as u16,
                    scale as u16,
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
