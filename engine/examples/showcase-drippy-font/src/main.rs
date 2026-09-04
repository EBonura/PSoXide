//! Native PS1 proof sheet for the Drippy Space font candidate.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_engine::{App, Config, Ctx, Scene};
use psx_font::{
    fonts::{BASIC, DRIPPY_SPACE, DRIPPY_SPACE_DISPLAY, KENNEY_PIXEL, ZEN_DOTS},
    FontAtlas,
};
use psx_vram::{Clut, TexDepth, Tpage};

const BASIC_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const BASIC_CLUT: Clut = Clut::new(320, 256);
const DRIPPY_TPAGE: Tpage = Tpage::new(384, 0, TexDepth::Bit4);
const DRIPPY_CLUT: Clut = Clut::new(384, 256);
const DRIPPY_DISPLAY_TPAGE: Tpage = Tpage::new(448, 0, TexDepth::Bit4);
const DRIPPY_DISPLAY_CLUT: Clut = Clut::new(448, 256);
const KENNEY_TPAGE: Tpage = Tpage::new(512, 0, TexDepth::Bit4);
const KENNEY_CLUT: Clut = Clut::new(512, 256);
const ZEN_TPAGE: Tpage = Tpage::new(576, 0, TexDepth::Bit4);
const ZEN_CLUT: Clut = Clut::new(576, 256);

struct Proof {
    basic: Option<FontAtlas>,
    drippy: Option<FontAtlas>,
    drippy_display: Option<FontAtlas>,
    kenney: Option<FontAtlas>,
    zen: Option<FontAtlas>,
}

impl Scene for Proof {
    fn init(&mut self, _ctx: &mut Ctx) {
        self.basic = Some(FontAtlas::upload(&BASIC, BASIC_TPAGE, BASIC_CLUT));
        self.drippy = Some(FontAtlas::upload(&DRIPPY_SPACE, DRIPPY_TPAGE, DRIPPY_CLUT));
        self.drippy_display = Some(FontAtlas::upload(
            &DRIPPY_SPACE_DISPLAY,
            DRIPPY_DISPLAY_TPAGE,
            DRIPPY_DISPLAY_CLUT,
        ));
        self.kenney = Some(FontAtlas::upload(&KENNEY_PIXEL, KENNEY_TPAGE, KENNEY_CLUT));
        self.zen = Some(FontAtlas::upload(&ZEN_DOTS, ZEN_TPAGE, ZEN_CLUT));
    }

    fn update(&mut self, _ctx: &mut Ctx) {}

    fn render(&mut self, ctx: &mut Ctx) {
        let basic = self.basic.as_ref().unwrap();
        let drippy = self.drippy.as_ref().unwrap();
        let drippy_display = self.drippy_display.as_ref().unwrap();
        let kenney = self.kenney.as_ref().unwrap();
        let zen = self.zen.as_ref().unwrap();

        if ctx.sim_tick.as_u32() % 360 >= 180 {
            render_cortex_contexts(basic, drippy_display, kenney);
            return;
        }

        basic.draw_text(8, 5, "DRIPPY SPACE // CORRECTED PSX PROOF", (118, 196, 188));
        basic.draw_text(
            8,
            18,
            "NATIVE RASTERS - NO BITMAP DOWNSCALE",
            (102, 104, 116),
        );

        basic.draw_text(8, 37, "20PX SOURCE / 16x20 CELL", (182, 119, 94));
        drippy.draw_text(16, 51, "CORTEX IGNITION", (92, 214, 193));
        drippy.draw_text(16, 78, "NEW GAME  SYSTEM", (235, 174, 132));

        basic.draw_text(8, 111, "28PX SOURCE / 21x23 CELL", (224, 145, 104));
        drippy_display.draw_text(16, 125, "CORTEX", (105, 207, 231));
        drippy_display.draw_text(16, 152, "IGNITION", (242, 101, 64));

        basic.draw_text(8, 185, "SMALL UI REMAINS PIXEL-NATIVE", (102, 104, 116));
        kenney.draw_text(16, 198, "NEW GAME", (232, 222, 212));
        zen.draw_text(154, 198, "SYSTEM", (232, 222, 212));

        basic.draw_text(8, 229, "NATIVE PS1 TEXTURE + QUAD PATH", (76, 106, 104));
    }
}

fn render_cortex_contexts(basic: &FontAtlas, drippy_display: &FontAtlas, kenney: &FontAtlas) {
    basic.draw_text(8, 5, "DRIPPY SPACE // CORTEX CONTEXTS", (118, 196, 188));
    basic.draw_text(8, 18, "DISPLAY FACE EVALUATION", (102, 104, 116));

    drippy_display.draw_text(84, 34, "CORTEX", (76, 202, 181));
    drippy_display.draw_text(69, 62, "IGNITION", (234, 82, 48));
    basic.draw_text(119, 90, "TECH DEMO", (122, 77, 72));

    basic.draw_text(18, 112, "MENU / KENNEY PIXEL", (102, 104, 116));
    kenney.draw_text(30, 126, "NEW GAME", (242, 226, 214));
    kenney.draw_text(30, 141, "SYSTEM", (218, 178, 158));
    kenney.draw_text(30, 156, "CREDITS", (218, 178, 158));

    basic.draw_text(174, 112, "HUD / KENNEY PIXEL", (102, 104, 116));
    kenney.draw_text(184, 128, "HRZ  078", (234, 82, 48));
    kenney.draw_text(184, 147, "ZTH  061", (76, 202, 181));

    basic.draw_text(18, 181, "WORLD MESSAGE / BODY FONT", (102, 104, 116));
    basic.draw_text(24, 196, "SIGNAL RECOVERED", (211, 184, 168));
    basic.draw_text(24, 211, "THE CITY REMEMBERS YOUR NAME.", (134, 117, 110));
    basic.draw_text(8, 231, "LIVE PS1 FONT ATLAS", (76, 106, 104));
}

#[no_mangle]
fn main() -> ! {
    let mut proof = Proof {
        basic: None,
        drippy: None,
        drippy_display: None,
        kenney: None,
        zen: None,
    };
    App::run(
        Config {
            clear_color: (5, 7, 11),
            ..Config::default()
        },
        &mut proof,
    );
}
