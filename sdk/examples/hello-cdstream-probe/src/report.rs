//! Everything the probe learned, as text: screen pages for reading off a
//! photograph, and one compact ASCII payload for the QR codes and the TTY.
//!
//! The payload is `key=v,v,v;` records so a phone's QR reader returns text a
//! person (or a script) can read without a decoder. Times are HBlanks
//! (63.56 us each) unless a key says otherwise; see `BURN.md` for the keys.

use core::ptr::addr_of_mut;

/// Characters per screen line (a 10-pixel-wide, 16-pixel-tall mono font).
pub const LINE_CHARS: usize = 32;
/// Lines per page, title included: 13 sixteen-pixel lines inside the picture
/// a CRT's overscan leaves.
pub const PAGE_LINES: usize = 13;
/// Pages the probe fills.
pub const MAX_PAGES: usize = 10;
const PAYLOAD_BYTES: usize = 1536;
/// Characters of payload in one QR code.
pub const QR_CHARS: usize = 170;

/// One line of fixed-width text being built.
#[derive(Clone, Copy)]
pub struct Text {
    bytes: [u8; LINE_CHARS],
    len: usize,
}

impl Text {
    pub const fn new() -> Self {
        Text {
            bytes: [b' '; LINE_CHARS],
            len: 0,
        }
    }

    pub fn s(&mut self, text: &str) -> &mut Self {
        for &b in text.as_bytes() {
            if self.len < LINE_CHARS {
                self.bytes[self.len] = b;
                self.len += 1;
            }
        }
        self
    }

    pub fn u(&mut self, mut value: u32) -> &mut Self {
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.s(core::str::from_utf8(&digits[at..]).unwrap_or("?"))
    }

    /// `value` in tenths as `12.3`.
    pub fn tenths(&mut self, value: u32) -> &mut Self {
        self.u(value / 10).s(".").u(value % 10)
    }

    /// Right-align `value` in `width` columns.
    pub fn pad_u(&mut self, value: u32, width: usize) -> &mut Self {
        let mut digits = 1;
        let mut rest = value / 10;
        while rest != 0 {
            digits += 1;
            rest /= 10;
        }
        for _ in digits..width {
            self.s(" ");
        }
        self.u(value)
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

#[derive(Clone, Copy)]
struct Page {
    lines: [Text; PAGE_LINES],
    count: usize,
    /// Colour code of the title.
    title_ok: bool,
}

const EMPTY_PAGE: Page = Page {
    lines: [Text::new(); PAGE_LINES],
    count: 0,
    title_ok: true,
};

/// The whole report.
pub struct Report {
    pages: [Page; MAX_PAGES],
    page_count: usize,
    payload_len: usize,
}

/// The payload, in RAM where a dump can find it.
#[no_mangle]
static mut CDPROBE_PAYLOAD: [u8; PAYLOAD_BYTES] = [0; PAYLOAD_BYTES];

pub static mut REPORT: Report = Report {
    pages: [EMPTY_PAGE; MAX_PAGES],
    page_count: 0,
    payload_len: 0,
};

/// The one report. The probe has a single thread of control and nothing
/// holds this reference across a call that takes another.
pub fn report() -> &'static mut Report {
    // SAFETY: see above.
    unsafe { &mut *addr_of_mut!(REPORT) }
}

impl Report {
    /// Begin a page. `ok` colours its title.
    pub fn page(&mut self, title: &str, ok: bool) {
        if self.page_count < MAX_PAGES {
            self.page_count += 1;
        }
        let page = &mut self.pages[self.page_count - 1];
        page.count = 0;
        page.title_ok = ok;
        let mut t = Text::new();
        t.s(title);
        page.lines[0] = t;
        page.count = 1;
    }

    /// Add a line to the current page.
    pub fn line(&mut self, text: &Text) {
        if self.page_count == 0 {
            return;
        }
        let page = &mut self.pages[self.page_count - 1];
        if page.count < PAGE_LINES {
            page.lines[page.count] = *text;
            page.count += 1;
        }
    }

    pub fn page_total(&self) -> usize {
        self.page_count
    }

    pub fn page_line(&self, page: usize, line: usize) -> Option<&str> {
        let page = self.pages.get(page)?;
        (line < page.count).then(|| page.lines[line].as_str())
    }

    pub fn page_ok(&self, page: usize) -> bool {
        self.pages.get(page).is_none_or(|p| p.title_ok)
    }

    fn payload_push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.payload_len < PAYLOAD_BYTES {
                // SAFETY: single thread; `payload_len` is in range.
                unsafe { (*addr_of_mut!(CDPROBE_PAYLOAD))[self.payload_len] = b };
                self.payload_len += 1;
            }
        }
    }

    /// Append `key=v,v,...;` to the payload.
    pub fn kv(&mut self, key: &str, values: &[u32]) {
        self.payload_push(key.as_bytes());
        self.payload_push(b"=");
        for (i, &value) in values.iter().enumerate() {
            if i != 0 {
                self.payload_push(b",");
            }
            let mut digits = [0u8; 10];
            let mut at = digits.len();
            let mut rest = value;
            loop {
                at -= 1;
                digits[at] = b'0' + (rest % 10) as u8;
                rest /= 10;
                if rest == 0 {
                    break;
                }
            }
            self.payload_push(&digits[at..]);
        }
        self.payload_push(b";");
    }

    /// The payload text so far.
    pub fn payload(&self) -> &'static str {
        // SAFETY: only `payload_push` writes, ASCII only, from this thread.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                addr_of_mut!(CDPROBE_PAYLOAD).cast::<u8>(),
                self.payload_len,
            )
        };
        core::str::from_utf8(bytes).unwrap_or("?")
    }

    /// How many QR codes carry the payload.
    pub fn qr_count(&self) -> usize {
        self.payload_len.div_ceil(QR_CHARS).max(1)
    }

    /// The `index`th slice of the payload, cut at a record boundary where it
    /// can be.
    pub fn qr_chunk(&self, index: usize) -> &'static str {
        let all = self.payload();
        let mut start = 0;
        let mut n = 0;
        while start < all.len() {
            let mut end = (start + QR_CHARS).min(all.len());
            if end < all.len() {
                if let Some(cut) = all[start..end].rfind(';') {
                    end = start + cut + 1;
                }
            }
            if n == index {
                return &all[start..end];
            }
            start = end;
            n += 1;
        }
        ""
    }
}

/// CRC-32 (IEEE, reflected): the one `gzip` stores and `crc32` prints, so a
/// payload can be checked with no special tool.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The numbers the summary page quotes. Zero means not measured.
#[derive(Clone, Copy)]
pub struct Headline {
    pub seek_forward_ms: [u32; 6],
    pub rate_x10: [u32; 2],
    pub lost_permille: [u32; 2],
    pub pio_us: [u32; 2],
    pub handler_us: u32,
    pub lease_ms: u32,
    pub pause_idle_ms10: u32,
    pub first_after_audio_ms: u32,
    pub resume_ms: u32,
    /// 0 no, 1 yes, 2 not asked: heard, resumed, stopped (recovery), stopped (bare).
    pub answers: [u32; 4],
    /// 0 ok, 1 failed, 2 not run.
    pub stop_at_once: u32,
    pub stop_settled_ms: u32,
}

pub static mut HEAD: Headline = Headline {
    seek_forward_ms: [0; 6],
    rate_x10: [0; 2],
    lost_permille: [0; 2],
    pio_us: [0; 2],
    handler_us: 0,
    lease_ms: 0,
    pause_idle_ms10: 0,
    first_after_audio_ms: 0,
    resume_ms: 0,
    answers: [2; 4],
    stop_at_once: 2,
    stop_settled_ms: 0,
};

/// The one headline record (single thread of control, see [`report`]).
pub fn head() -> &'static mut Headline {
    // SAFETY: as `report`.
    unsafe { &mut *addr_of_mut!(HEAD) }
}
