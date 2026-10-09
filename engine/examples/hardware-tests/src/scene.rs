// SPDX-License-Identifier: GPL-2.0-or-later
//! The suite's screens: a four-row menu, the linear run's capture pages, the
//! controller test and the memory-card diagnostic.
//!
//! Nothing here measures anything. The run itself is `run.rs`; it blocks the
//! frame loop and returns here with a finished capture.

use super::*;
use crate::controller_test::ControllerTest;
use crate::photo::PhotoCapture;
use crate::run::Run;
use hello_memcard_recovery::Diagnostic as MemoryCardDiagnostic;
use psx_engine::{Ctx, Scene};

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Menu,
    ControllerTest,
    MemoryCard,
    /// The capture, as pages to film.
    Capture,
}

const MENU: [&str; 4] = [
    // Row 0 is pinned: `make hwtest-run` selects it by firing CROSS at a fixed
    // tick with the cursor still at its boot position.
    "RUN HARDWARE TEST",
    "CONTROLLER TEST (P1 + P2)",
    "MEMORY CARD (AT OWN RISK)",
    "VIEW LAST CAPTURE",
];
const MENU_TOP: i16 = 64;
const MENU_ROW_PITCH: i16 = 14;
/// Frames each capture page (and the cover) stays up before the next one.
const PAGE_FRAMES: u32 = 150;
/// Frames auto-advance stays off after a manual page turn.
const MANUAL_HOLD: u32 = 1500;

pub(crate) struct HardwareTests {
    font: Option<FontAtlas>,
    mode: Mode,
    menu_cursor: usize,
    run: Run,
    capture: PhotoCapture,
    have_capture: bool,
    /// The run id the last capture's pages carry in their headers, fixed when
    /// the capture was encoded (the cover shows this one, not a fresh mix).
    capture_run_id: u16,
    /// 0 is the cover; 1..=page_count are the QR pages.
    view: usize,
    view_frames: u32,
    manual_until: u32,
    clock: u32,
    controller_test: ControllerTest,
    memory_card: MemoryCardDiagnostic,
    /// The memory-card test touches the operator's real card and has had
    /// limited testing on silicon. Nothing talks to the card until the
    /// warning screen is accepted, and entering the mode always re-asks.
    memcard_armed: bool,
}

impl HardwareTests {
    pub(crate) fn new() -> Self {
        Self {
            font: None,
            mode: Mode::Menu,
            menu_cursor: 0,
            run: Run::new(),
            capture: PhotoCapture::new(),
            have_capture: false,
            capture_run_id: 0,
            view: 0,
            view_frames: 0,
            manual_until: 0,
            clock: 0,
            controller_test: ControllerTest::new(),
            memory_card: MemoryCardDiagnostic::new(),
            memcard_armed: false,
        }
    }

    fn open_menu(&mut self) {
        self.mode = Mode::Menu;
    }

    fn open_capture(&mut self) {
        self.mode = Mode::Capture;
        self.view = 0;
        self.view_frames = 0;
        self.manual_until = 0;
    }

    /// The whole run, then the capture.
    fn start_run(&mut self, ctx: &mut Ctx) {
        let skip_risky = ctx.is_held(button::L2);
        self.run.write_cards = ctx.is_held(button::L1) && ctx.is_held(button::R1);
        tty::println("hardware-tests: run begins");
        run::execute(&mut self.run, ctx, ctx.pad, skip_risky, &mut self.capture);
        // The run left its own picture and font; the scene's are back below.
        self.font = Some(ui::upload_font());
        // One id, taken once: the page headers and the cover both show it.
        let id = self.new_run_id();
        self.capture_run_id = id;
        self.run.timing.summary.runs = (id >> 8) as u8;
        self.capture.encode(
            &self.run.timing,
            &self.run.results,
            id as u8,
            self.run.scans,
        );
        self.have_capture = true;
        tty::println(if run::all_clean(&self.run) {
            "hardware-tests: run complete, every area clean, silent"
        } else {
            "hardware-tests: run complete, NOT CLEAN"
        });
        // Last: every page to the TTY again, so a headless log ends with the
        // complete set and nothing after it.
        for page in 0..self.capture.page_count() {
            self.capture.print_page(page);
        }
        self.open_capture();
        self.capture.render_page(0);
    }

    /// A 16-bit id for this run, so pages from different runs in one
    /// recording can be told apart before any CRC is checked.
    fn new_run_id(&self) -> u16 {
        let h = mix32(
            self.run.timing.summary.hash,
            psx_rt::interrupts::vblank_count(),
        );
        (h ^ (h >> 16)) as u16
    }
}

impl Scene for HardwareTests {
    fn init(&mut self, _ctx: &mut Ctx) {
        enable_cop2_for_diagnostics();
        self.font = Some(ui::upload_font());
        tty::println("hardware-tests: main menu ready");
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.clock = self.clock.wrapping_add(1);
        match self.mode {
            Mode::ControllerTest => {
                if self.controller_test.update(ctx.pad) {
                    self.open_menu();
                }
                // START and SELECT are ordinary testable buttons here; the
                // deliberate hold gesture owns navigation.
            }
            Mode::MemoryCard => self.update_memory_card(ctx),
            Mode::Menu => {
                let rows = MENU.len();
                if ctx.just_pressed(button::UP) {
                    self.menu_cursor = (self.menu_cursor + rows - 1) % rows;
                }
                if ctx.just_pressed(button::DOWN) {
                    self.menu_cursor = (self.menu_cursor + 1) % rows;
                }
                if ctx.just_pressed(button::CROSS) {
                    match self.menu_cursor {
                        0 => self.start_run(ctx),
                        1 => {
                            self.controller_test.start();
                            self.mode = Mode::ControllerTest;
                        }
                        2 => {
                            self.memory_card = MemoryCardDiagnostic::new();
                            self.memcard_armed = false;
                            self.mode = Mode::MemoryCard;
                        }
                        _ => {
                            if self.have_capture {
                                self.open_capture();
                                self.capture.render_page(0);
                            }
                        }
                    }
                }
            }
            Mode::Capture => self.update_capture(ctx),
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        let Some(font) = self.font.as_ref() else {
            return;
        };
        match self.mode {
            Mode::Menu => {
                draw_test_pattern(ctx.sim_tick.as_u32());
                self.draw_menu(font);
            }
            Mode::Capture => {
                if self.view == 0 {
                    draw_test_pattern(ctx.sim_tick.as_u32());
                    self.draw_cover(font);
                } else {
                    photo::draw_capture_page(font, &self.capture, self.view - 1);
                }
            }
            Mode::ControllerTest => self.controller_test.draw(font),
            Mode::MemoryCard => {
                if self.memcard_armed {
                    self.memory_card.draw(font);
                } else {
                    draw_memcard_warning(font);
                }
            }
        }
    }
}

impl HardwareTests {
    fn update_memory_card(&mut self, ctx: &mut Ctx) {
        if !self.memcard_armed {
            // No card traffic of any kind behind the warning: the scan
            // starts reading the card on its first step, and consent has
            // to come before the first read, not before the first write.
            // CIRCLE accepts rather than CROSS, because CROSS is what
            // just selected the menu row and a bounced press must not
            // blow through a risk gate.
            if ctx.just_pressed(button::CIRCLE) {
                self.memcard_armed = true;
            } else if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
                self.open_menu();
            }
            return;
        }
        // The engine's controller poll has fully completed before this card
        // transaction starts. Pad and card share SIO0, but are never accessed
        // concurrently.
        self.memory_card.scan_step();
        if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
            self.open_menu();
            return;
        }
        if ctx.just_pressed(button::LEFT) {
            self.memory_card.page_left();
        }
        if ctx.just_pressed(button::RIGHT) {
            self.memory_card.page_right();
        }
        self.memory_card.guarded_write(
            ctx.is_held(button::L1),
            ctx.is_held(button::R1),
            ctx.just_pressed(button::CROSS),
        );
    }

    fn update_capture(&mut self, ctx: &mut Ctx) {
        if ctx.just_pressed(button::START) || ctx.just_pressed(button::TRIANGLE) {
            self.open_menu();
            return;
        }
        let views = self.capture.page_count() + 1;
        let mut turned = false;
        if ctx.just_pressed(button::LEFT) {
            self.view = (self.view + views - 1) % views;
            turned = true;
        } else if ctx.just_pressed(button::RIGHT) {
            self.view = (self.view + 1) % views;
            turned = true;
        }
        if turned {
            self.manual_until = self.clock + MANUAL_HOLD;
            self.view_frames = 0;
        } else if self.clock >= self.manual_until {
            // Every page in turn, forever, so one recording holds the whole
            // set several times over.
            self.view_frames += 1;
            if self.view_frames >= PAGE_FRAMES {
                self.view_frames = 0;
                self.view = (self.view + 1) % views;
                turned = true;
            }
        }
        if turned && self.view > 0 {
            self.capture.render_page(self.view - 1);
        }
    }

    fn draw_menu(&self, font: &FontAtlas) {
        font.draw_text(8, 6, "PS1 HARDWARE TESTS", (232, 236, 244));
        font.draw_text(
            320 - 8 - SUITE_VERSION.len() as i16 * 8,
            6,
            SUITE_VERSION,
            (112, 136, 170),
        );
        font.draw_text(8, 20, "MAIN MENU", (255, 232, 128));
        let mut y = MENU_TOP;
        for (row, label) in MENU.iter().enumerate() {
            let selected = row == self.menu_cursor;
            if selected {
                font.draw_text(10, y, ">", (255, 232, 128));
            }
            let colour = if selected {
                (255, 232, 128)
            } else {
                (176, 190, 210)
            };
            font.draw_text(22, y, label, colour);
            if row == 3 && !self.have_capture {
                font.draw_text(22, y + 9, "NONE YET - RUN FIRST", (112, 136, 170));
            }
            y += MENU_ROW_PITCH;
        }
        font.draw_text(
            8,
            196,
            "THE RUN TAKES THE WHOLE DISC TO ITSELF.",
            (140, 160, 190),
        );
        font.draw_text(8, 207, "HOLD L2 WHEN STARTING TO SKIP THE", (140, 160, 190));
        font.draw_text(8, 217, "STEPS THAT CAN HANG A CONSOLE.", (140, 160, 190));
        font.draw_text(8, 228, "UP/DOWN SELECT   CROSS RUN", (140, 160, 190));
    }

    /// Page 0: what to film, and whether the run came back clean.
    fn draw_cover(&self, font: &FontAtlas) {
        let pages = self.capture.page_count();
        let mut line = ui::Line::new();
        line.s("FILM FROM HERE, ").u(pages as u32).s(" PAGES");
        font.draw_text(8, 56, line.as_str(), (255, 232, 128));
        let mut line = ui::Line::new();
        line.s("RUN ID ").s(hex4(self.capture_run_id).as_str());
        font.draw_text(8, 72, line.as_str(), (232, 236, 244));
        font.draw_text(
            8,
            92,
            "EACH PAGE STAYS UP ABOUT TWO SECONDS,",
            (150, 170, 200),
        );
        font.draw_text(
            8,
            102,
            "THEN THE SET REPEATS. FILM ALL OF THEM.",
            (150, 170, 200),
        );
        font.draw_text(
            8,
            112,
            "LEFT/RIGHT TURNS PAGES, START: MENU.",
            (150, 170, 200),
        );
        let [pass, fail, warn, info] = report::tally(&self.run.results);
        let mut line = ui::Line::new();
        line.s("PASS ")
            .u(pass as u32)
            .s(" FAIL ")
            .u(fail as u32)
            .s(" WARN ")
            .u(warn as u32);
        font.draw_text(8, 132, line.as_str(), (220, 224, 230));
        let mut line = ui::Line::new();
        line.s("INFO ").u(info as u32);
        font.draw_text(8, 144, line.as_str(), (220, 224, 230));
        if run::all_clean(&self.run) {
            font.draw_text(8, 164, "EVERY AREA LEFT CLEAN STATE", Status::Pass.color());
            font.draw_text(8, 176, "SILENCE CHECK PASSED", Status::Pass.color());
        } else {
            font.draw_text(8, 164, "AN AREA OR THE SILENCE CHECK", Status::Warn.color());
            font.draw_text(
                8,
                176,
                "IS NOT CLEAN: SEE RECORDS 410",
                Status::Warn.color(),
            );
        }
        if self.run.skip_risky {
            font.draw_text(
                8,
                196,
                "RISKY STEPS WERE SKIPPED (L2)",
                Status::Warn.color(),
            );
        }
    }
}

fn hex4(value: u16) -> report::Hex4 {
    report::hex4(value)
}

/// The consent screen in front of the memory-card test. It blocks ALL card
/// traffic, not just writes: the scan reads the card from its first step,
/// and "use at your own risk" said after the first read is theatre.
fn draw_memcard_warning(font: &FontAtlas) {
    probe_gpu!(gpu);
    gpu.draw(&QuadFlat::rect((8, 32), (304, 20), (200, 24, 24)));
    font.draw_text(104, 38, "!!  WARNING  !!", (255, 240, 96));
    let body: [&str; 5] = [
        "THIS TEST HAS ONLY HAD LIMITED TESTING",
        "ON REAL HARDWARE. IT READS AND WRITES",
        "YOUR MEMORY CARD, AND I CANNOT",
        "GUARANTEE IT WILL NOT CORRUPT YOUR",
        "SAVES. USE AT YOUR OWN RISK.",
    ];
    let mut y = 64;
    for line in body {
        font.draw_text(8, y, line, (232, 236, 244));
        y += 14;
    }
    font.draw_text(
        8,
        y + 8,
        "IF YOU HAVE A SPARE CARD, USE IT.",
        (255, 216, 96),
    );
    font.draw_text(8, y + 36, "CIRCLE = I ACCEPT THE RISK", (96, 240, 128));
    font.draw_text(
        8,
        y + 50,
        "TRIANGLE OR START = BACK TO MENU",
        (150, 170, 200),
    );
}

fn draw_test_pattern(_tick: u32) {
    probe_gpu!(gpu);
    gpu.draw(&QuadFlat::new(
        [(0, 0), (320, 0), (0, 47), (320, 47)],
        12,
        18,
        36,
    ));
    gpu.draw(&QuadFlat::new(
        [(0, 188), (320, 188), (0, 240), (320, 240)],
        8,
        12,
        28,
    ));
    gpu.draw(&LineMono::new(0, 48, 319, 48, 60, 80, 110));
    gpu.draw(&LineMono::new(0, 187, 319, 187, 60, 80, 110));
}

#[cfg(target_arch = "mips")]
fn enable_cop2_for_diagnostics() {
    let mut sr: u32;
    // SAFETY: COP0 status read-modify-write to enable COP2; nops cover the
    // CP0 hazard.
    unsafe {
        core::arch::asm!("mfc0 $8, $12", lateout("$8") sr);
        sr |= 0x4000_0000;
        core::arch::asm!(
            "mtc0 $8, $12",
            "nop",
            "nop",
            "nop",
            in("$8") sr,
            options(nostack, nomem, preserves_flags),
        );
    }
}

#[cfg(not(target_arch = "mips"))]
fn enable_cop2_for_diagnostics() {}

#[no_mangle]
fn main() -> ! {
    // Capture the BIOS-owned state (exception vector, SPU reverb) before any
    // SDK or engine path can initialise anything.
    boot::capture();
    // Before App::run: the engine's clock replaces the BIOS exception vector.
    kernel_timing::snapshot_vector();
    let mut suite = HardwareTests::new();
    let config = Config {
        screen_w: SCREEN_W as u16,
        screen_h: SCREEN_H as u16,
        video_mode: VideoMode::Ntsc,
        resolution: Resolution::R320X240,
        clear_color: (6, 8, 18),
        ..Config::default()
    };
    App::run(config, &mut suite);
}
