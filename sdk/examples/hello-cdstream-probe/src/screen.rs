//! The display and the pad: report pages in large text, a yes/no prompt that
//! takes any controller, and the QR pages.

use crate::report::{crc32, report, Text, PAGE_LINES};
use core::ptr::addr_of_mut;
use psx_font::{fonts::SHARE_TECH_MONO, FontAtlas};
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::prim::QuadFlat;
use psx_gpu::Gpu;
use psx_io::periph::ControllerPort;
use psx_pad::{button, poll_on, ButtonState, Port};
use psx_rt::interrupts::{vblank_count, wait_vblank};
use psx_vram::{Clut, TextureDepth, TexturePage};
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const WHITE: (u8, u8, u8) = (235, 235, 245);
const GREEN: (u8, u8, u8) = (90, 235, 120);
const RED: (u8, u8, u8) = (240, 90, 90);
const AMBER: (u8, u8, u8) = (240, 200, 80);
const CYAN: (u8, u8, u8) = (110, 200, 235);

/// Pixels between lines: the 16-pixel mono font, unscaled.
const PITCH: i16 = 16;
/// Top margin, clear of a CRT's overscan.
const TOP: i16 = 10;

const QR_MAX_VERSION: Version = Version::new(14);
const QR_MIN_VERSION: Version = Version::new(4);
const QR_BUFFER_LEN: usize = QR_MAX_VERSION.buffer_len();
const QR_MAX_SIZE: usize = 73;
const QR_QUIET: i16 = 4;

static mut QR_TEMP: [u8; QR_BUFFER_LEN] = [0; QR_BUFFER_LEN];
static mut QR_OUT: [u8; QR_BUFFER_LEN] = [0; QR_BUFFER_LEN];
static mut QR_MODULES: [u8; (QR_MAX_SIZE * QR_MAX_SIZE).div_ceil(8)] =
    [0; (QR_MAX_SIZE * QR_MAX_SIZE).div_ceil(8)];
/// The QR text, with its framing, in RAM for a dump.
#[no_mangle]
static mut CDPROBE_QR_TEXT: [u8; 384] = [0; 384];

pub struct Screen {
    gpu: Gpu,
    fb: DoubleBuffer,
    font: FontAtlas,
    port: ControllerPort,
    prev: ButtonState,
    qr_size: usize,
}

impl Screen {
    pub fn new(gpu_dma: psx_io::periph::GpuDma, port: ControllerPort) -> Self {
        let mut gpu = Gpu::new(
            gpu_dma,
            DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
        );
        let fb = DoubleBuffer::new(Resolution::R320X240);
        gpu.set_draw_area((0, 0), (319, 239));
        gpu.set_draw_offset((0, 0));
        let font = FontAtlas::upload(&SHARE_TECH_MONO, FONT_TPAGE, FONT_CLUT);
        Screen {
            gpu,
            fb,
            font,
            port,
            prev: ButtonState::NONE,
            qr_size: 0,
        }
    }

    fn begin(&mut self, background: (u8, u8, u8)) {
        self.fb.clear(&mut self.gpu, background);
    }

    fn end(&mut self) {
        self.gpu.wait_idle();
        wait_vblank();
        self.fb.swap(&mut self.gpu);
    }

    fn text(&self, x: i16, y: i16, text: &str, tint: (u8, u8, u8)) {
        self.font.draw_text(x, y, text, tint);
    }

    /// Buttons newly pressed since the last call.
    fn pressed(&mut self) -> ButtonState {
        let now = poll_on(&mut self.port, Port::One).buttons;
        let fresh = ButtonState::from_bits(now.bits() & !self.prev.bits());
        self.prev = now;
        fresh
    }

    /// Forget presses made while nothing was listening: show the screen for
    /// half a second, then wait for every button to be let go.
    fn arm(&mut self, draw: &mut dyn FnMut(&mut Self)) {
        for _ in 0..30 {
            draw(self);
            let _ = self.pressed();
        }
        for _ in 0..180 {
            draw(self);
            if self.pressed_state() == 0 {
                break;
            }
        }
        self.prev = ButtonState::NONE;
    }

    fn pressed_state(&mut self) -> u16 {
        let now = poll_on(&mut self.port, Port::One).buttons;
        self.prev = now;
        now.bits()
    }

    /// Show a status screen for one frame: a title and up to 13 lines.
    pub fn status(&mut self, title: &str, lines: &[&str]) {
        self.begin((12, 24, 40));
        self.text(4, TOP, title, CYAN);
        for (i, line) in lines.iter().take(PAGE_LINES - 1).enumerate() {
            self.text(4, TOP + PITCH * (i as i16 + 1) + 4, line, WHITE);
        }
        self.end();
    }

    /// Wait for CROSS or START (any pad), with a message.
    pub fn wait_start(&mut self, title: &str, lines: &[&str]) {
        loop {
            self.status(title, lines);
            let fresh = self.pressed();
            if fresh.is_held(button::CROSS) || fresh.is_held(button::START) {
                return;
            }
        }
    }

    /// Ask a yes/no question. CROSS (X) is yes, CIRCLE (O) is no. Waits as
    /// long as it takes; the sound the question is about keeps playing.
    pub fn ask(&mut self, title: &str, lines: &[&str]) -> bool {
        self.arm(&mut |screen| screen.draw_question(title, lines));
        loop {
            self.draw_question(title, lines);
            let fresh = self.pressed();
            if fresh.is_held(button::CROSS) {
                return true;
            }
            if fresh.is_held(button::CIRCLE) {
                return false;
            }
        }
    }

    /// Show report page `page` for `frames` VBlanks.
    pub fn show_page(&mut self, page: usize, frames: u32) {
        let end = vblank_count().wrapping_add(frames);
        while (end.wrapping_sub(vblank_count()) as i32) > 0 {
            self.draw_report_page(page);
        }
    }

    fn draw_question(&mut self, title: &str, lines: &[&str]) {
        self.begin((40, 24, 12));
        self.text(4, TOP, title, AMBER);
        for (i, line) in lines.iter().take(PAGE_LINES - 3).enumerate() {
            self.text(4, TOP + PITCH * (i as i16 + 1) + 4, line, WHITE);
        }
        self.text(4, TOP + PITCH * 11, "X = YES     O = NO", GREEN);
        self.end();
    }

    fn draw_report_page(&mut self, page: usize) {
        let r = report();
        let ok = r.page_ok(page);
        self.begin(if ok { (10, 28, 24) } else { (48, 18, 18) });
        for line in 0..PAGE_LINES {
            let Some(text) = r.page_line(page, line) else {
                break;
            };
            let tint = if line == 0 {
                if ok {
                    GREEN
                } else {
                    RED
                }
            } else {
                WHITE
            };
            self.text(4, TOP + PITCH * line as i16, text, tint);
        }
        self.end();
    }

    /// Encode QR chunk `index` and remember the symbol for drawing.
    fn prepare_qr(&mut self, index: usize) {
        let r = report();
        let count = r.qr_count();
        let chunk = r.qr_chunk(index);
        let mut text = Text2::new();
        text.push("CDP1/");
        text.push_u(index as u32 + 1);
        text.push("/");
        text.push_u(count as u32);
        text.push("/");
        text.push(chunk);
        let crc = crc32(text.bytes());
        text.push("/C:");
        text.push_hex(crc);
        // SAFETY: single thread; these statics are only used here.
        let (temp, out, modules) = unsafe {
            (
                &mut *addr_of_mut!(QR_TEMP),
                &mut *addr_of_mut!(QR_OUT),
                &mut *addr_of_mut!(QR_MODULES),
            )
        };
        text.store();
        let encoded = core::str::from_utf8(text.bytes()).unwrap_or("");
        self.qr_size = 0;
        let Ok(qr) = QrCode::encode_text(
            encoded,
            temp,
            out,
            QrCodeEcc::Medium,
            QR_MIN_VERSION,
            QR_MAX_VERSION,
            None,
            false,
        ) else {
            return;
        };
        modules.fill(0);
        let size = qr.size() as usize;
        for y in 0..size {
            for x in 0..size {
                if qr.get_module(x as i32, y as i32) {
                    let bit = y * size + x;
                    modules[bit / 8] |= 1 << (bit & 7);
                }
            }
        }
        self.qr_size = size;
    }

    fn draw_qr_page(&mut self, index: usize) {
        let count = report().qr_count();
        self.begin((20, 20, 24));
        let mut title = Text::new();
        title.s("QR ").u(index as u32 + 1).s(" OF ").u(count as u32);
        self.text(4, 4, title.as_str(), CYAN);
        if self.qr_size == 0 {
            self.text(4, 40, "QR ENCODE FAILED", RED);
            self.end();
            return;
        }
        let size = self.qr_size;
        let scale = (200 / (size as i16 + QR_QUIET * 2)).max(1);
        let total = (size as i16 + QR_QUIET * 2) * scale;
        let left = (320 - total) / 2;
        let top = 22;
        self.gpu.draw(&QuadFlat::rect(
            (left, top),
            (total as u16, total as u16),
            (255, 255, 255),
        ));
        // SAFETY: single thread; read-only here.
        let modules = unsafe { &*addr_of_mut!(QR_MODULES) };
        let dark = |x: usize, y: usize| {
            let bit = y * size + x;
            modules[bit / 8] & (1 << (bit & 7)) != 0
        };
        let data_left = left + QR_QUIET * scale;
        let data_top = top + QR_QUIET * scale;
        for y in 0..size {
            let mut x = 0;
            while x < size {
                while x < size && !dark(x, y) {
                    x += 1;
                }
                let first = x;
                while x < size && dark(x, y) {
                    x += 1;
                }
                if first < x {
                    self.gpu.draw(&QuadFlat::rect(
                        (
                            data_left + first as i16 * scale,
                            data_top + y as i16 * scale,
                        ),
                        (((x - first) as i16 * scale) as u16, scale as u16),
                        (0, 0, 0),
                    ));
                }
            }
        }
        self.end();
    }

    /// The end: flip through the report pages and QR codes with the pad.
    /// Never returns. LEFT/RIGHT, SQUARE/CROSS and L1/R1 all turn pages.
    pub fn browse(&mut self, start: usize) -> ! {
        let report_pages = report().page_total();
        let qr_pages = report().qr_count();
        let total = report_pages + qr_pages;
        let mut shown = usize::MAX;
        let mut current = start.min(total - 1);
        loop {
            if shown != current {
                shown = current;
                if current >= report_pages {
                    self.prepare_qr(current - report_pages);
                }
            }
            if current < report_pages {
                self.draw_report_page(current);
            } else {
                self.draw_qr_page(current - report_pages);
            }
            let fresh = self.pressed();
            let next = fresh.is_held(button::RIGHT)
                || fresh.is_held(button::CROSS)
                || fresh.is_held(button::R1);
            let back = fresh.is_held(button::LEFT)
                || fresh.is_held(button::SQUARE)
                || fresh.is_held(button::L1);
            if next {
                current = (current + 1) % total;
            } else if back {
                current = (current + total - 1) % total;
            }
        }
    }
}

/// A small text buffer for the QR string.
struct Text2 {
    bytes: [u8; 384],
    len: usize,
}

impl Text2 {
    const fn new() -> Self {
        Text2 {
            bytes: [0; 384],
            len: 0,
        }
    }

    fn push(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = b;
                self.len += 1;
            }
        }
    }

    fn push_u(&mut self, mut value: u32) {
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
        self.push(core::str::from_utf8(&digits[at..]).unwrap_or("?"));
    }

    fn push_hex(&mut self, value: u32) {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        for i in 0..8 {
            let nibble = ((value >> ((7 - i) * 4)) & 15) as usize;
            if self.len < self.bytes.len() {
                self.bytes[self.len] = DIGITS[nibble];
                self.len += 1;
            }
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Copy to the dump buffer.
    fn store(&self) {
        // SAFETY: single thread; the buffer is only written here.
        unsafe {
            let dump = &mut *addr_of_mut!(CDPROBE_QR_TEXT);
            dump.fill(0);
            let n = self.len.min(dump.len());
            dump[..n].copy_from_slice(&self.bytes[..n]);
        }
    }
}
