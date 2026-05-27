//! `game-magikaaaaaarp-pong` -- magikAAAAArp-themed Pong with a
//! large rotating album cube.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate psx_rt;

use psx_asset::Texture;
use psx_engine::{button, sfx, Angle, App, Config, Ctx, Scene, SimTick};
use psx_font::{fonts::BASIC_8X16, FontAtlas};
use psx_gpu::material::TextureMaterial;
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::{QuadGouraud, QuadTexturedMaterial, RectFlat};
use psx_io::cdrom;
use psx_spu::{self as spu, CdVolume, SpuAddr, Voice, Volume};
use psx_vram::{upload_bytes, Clut, TexDepth, Tpage, VramRect};

#[cfg(target_arch = "mips")]
fn game_trace(message: &str) {
    psx_rt::tty::println(message);
}

#[cfg(not(target_arch = "mips"))]
fn game_trace(_message: &str) {}

const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;
const TITLE: &[u8; 15] = b"MAGIKAAAAARPONG";
const TITLE_X: i16 = 100;
const TITLE_Y: i16 = 6;
const TITLE_GLYPH_W: i16 = 8;
const TITLE_WAVE_PERIOD_TICKS: u32 = 360;
const TITLE_WAVE_ACTIVE_TICKS: u32 = 120;
const TITLE_WAVE_FADE_TICKS: u32 = 30;
const TITLE_WAVE_MAX_PX: i16 = 5;
const SCORE_Y: i16 = 18;
const PAPER: (u8, u8, u8) = (228, 236, 232);
const INK: (u8, u8, u8) = (8, 9, 9);
const MUTED_INK: (u8, u8, u8) = (62, 69, 69);
const SPECTRUM_LOW: (u8, u8, u8) = (126, 135, 132);
const SPECTRUM_HIGH: (u8, u8, u8) = (205, 21, 23);

const BORDER_H: u16 = 3;
const PLAYFIELD_TOP: i16 = 36;
const PLAYFIELD_BOT: i16 = SCREEN_H - BORDER_H as i16;

const PADDLE_W: u16 = 8;
const PADDLE_H: u16 = 56;
const PADDLE_MARGIN: i16 = 10;
const PADDLE_SPEED: i16 = 4;

const AI_SPEED: i16 = 2;
const AI_HYSTERESIS: i16 = 10;
const AI_REACTION_TICKS: u32 = 3;

const BALL_SIZE: u16 = 42;
const BALL_START_VX: i16 = 2;
const BALL_START_VY: i16 = 1;
const BALL_MAX_SPEED: i16 = 4;
const WIN_SCORE: u8 = 5;
const TRACK_GONCHAROV: u8 = 2;
const GONCHAROV_LOOP_TICKS: u32 = 233 * 60;
const GONCHAROV_START_DELAY_TICKS: u32 = 30;
const GONCHAROV_RETRY_TICKS: u32 = 60;
const CDROM_COMMAND_SPINS: u32 = 16_384;
const CUBE_MAX_SCALE_Q12: i32 = 4096;
const CUBE_MIN_SCALE_Q12: i32 = CUBE_MAX_SCALE_Q12 / 3;
const CUBE_PULSE_PERIOD_TICKS: u32 = 360;

const CUBE_HALF: i32 = 34;
const CUBE_Z: i32 = 180;
const CUBE_PROJECTION: i32 = 190;
const CUBE_UVS: [(u8, u8); 4] = [(0, 0), (127, 0), (0, 127), (127, 127)];
const CUBE_SPIN_X_STEP: Angle = Angle::from_raw_q16(0x00D0);
const CUBE_SPIN_Y_STEP: Angle = Angle::from_raw_q16(0x0130);
const CUBE_SPIN_Z_STEP: Angle = Angle::from_raw_q16(0x0058);
const SCORE_FLYBY_SIZE: i16 = 74;
const SCORE_FLYBY_Y: i16 = 120;
const SCORE_FLYBY_DURATION_TICKS: u16 = 150;
const SCORE_FLYBY_SPIN_STEP: Angle = Angle::from_raw_q16(0x0180);
const SCORE_FLYBY_OT_SLOT: usize = 4;

const SPECTRUM_BANDS: usize = 16;
const SPECTRUM_FRAME_COUNT: usize = 6990;
const SPECTRUM_TICKS_PER_FRAME: u32 = 2;
const SPECTRUM_X0: i16 = 22;
const SPECTRUM_BASE_Y: i16 = PLAYFIELD_BOT - 10;
const SPECTRUM_BAR_W: u16 = 10;
const SPECTRUM_BAR_PITCH: i16 = 18;
const SPECTRUM_MAX_H: u16 = 76;
const SPECTRUM_OT_SLOT: usize = 15;
const CENTER_DASH_RECTS: usize = 14;
const CENTER_RECTS_START: usize = 2;
const PADDLE_RECT_START: usize = CENTER_RECTS_START + CENTER_DASH_RECTS;

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const CUBE_TPAGE: Tpage = Tpage::new(640, 0, TexDepth::Bit8);
const CUBE_CLUT: Clut = Clut::new(640, 256);

const SPU_SAMPLE_BASE: SpuAddr = SpuAddr::new(0x1010);
const VOICE_WALL: Voice = Voice::V0;
const VOICE_PADDLE: Voice = Voice::V1;
const VOICE_SCORE: Voice = Voice::V2;
const VOICE_FLYBY: Voice = Voice::V3;

const SFX_BANK: [sfx::Sample<'static>; 4] = [
    sfx::Sample {
        voice: VOICE_WALL,
        bytes: include_bytes!("../../../../assets/audio/freesfx/psau/ui_beep.psau"),
        volume: Volume::linear(1, 18),
    },
    sfx::Sample {
        voice: VOICE_PADDLE,
        bytes: include_bytes!("../../../../assets/audio/freesfx/psau/hit_punch.psau"),
        volume: Volume::linear(1, 16),
    },
    sfx::Sample {
        voice: VOICE_SCORE,
        bytes: include_bytes!("../../../../assets/audio/freesfx/psau/pickup_coin.psau"),
        volume: Volume::linear(1, 18),
    },
    sfx::Sample {
        voice: VOICE_FLYBY,
        bytes: include_bytes!("../../../../assets/audio/freesfx/psau/swoosh.psau"),
        volume: Volume::linear(1, 10),
    },
];

static mut OT: OrderingTable<16> = OrderingTable::new();
static SPECTRUM_DATA: &[u8] = include_bytes!("../assets/goncharov_spectrum_16x30hz.bin");
static mut RECTS: [RectFlat; 24] = [const { RectFlat::new(0, 0, 0, 0, 0, 0, 0) }; 24];
static mut SPECTRUM_QUADS: [QuadGouraud; SPECTRUM_BANDS] = [const {
    QuadGouraud::new(
        [(0, 0), (0, 0), (0, 0), (0, 0)],
        [(0, 0, 0), (0, 0, 0), (0, 0, 0), (0, 0, 0)],
    )
}; SPECTRUM_BANDS];
static mut CUBE_QUADS: [QuadTexturedMaterial; 6] = [const {
    QuadTexturedMaterial::with_material(
        [(0, 0), (0, 0), (0, 0), (0, 0)],
        CUBE_UVS,
        TextureMaterial::opaque(0, 0, (0, 0, 0)),
    )
}; 6];
static mut SCORE_FLYBY_QUAD: QuadTexturedMaterial = QuadTexturedMaterial::with_material(
    [(0, 0), (0, 0), (0, 0), (0, 0)],
    CUBE_UVS,
    TextureMaterial::opaque(0, 0, (0, 0, 0)),
);

#[derive(Copy, Clone, PartialEq, Eq)]
enum CddaStartStep {
    SetMode,
    Demute,
    Play,
}

struct MagikaaaaaarpPong {
    p1_y: i16,
    p2_y: i16,
    p1_score: u8,
    p2_score: u8,
    ball_x: i16,
    ball_y: i16,
    ball_vx: i16,
    ball_vy: i16,
    winner: u8,
    serve_dir: i8,
    spin_x: Angle,
    spin_y: Angle,
    spin_z: Angle,
    score_flyby_tick: u16,
    score_flyby_dir: i8,
    score_flyby_spin: Angle,
    cdda_started: bool,
    cdda_start_step: CddaStartStep,
    cdda_started_tick: u32,
    cdda_next_retry_tick: u32,
    cdda_wait_logged: bool,
    font: Option<FontAtlas>,
}

impl MagikaaaaaarpPong {
    const fn new() -> Self {
        Self {
            p1_y: 0,
            p2_y: 0,
            p1_score: 0,
            p2_score: 0,
            ball_x: 0,
            ball_y: 0,
            ball_vx: 0,
            ball_vy: 0,
            winner: 0,
            serve_dir: 1,
            spin_x: Angle::ZERO,
            spin_y: Angle::ZERO,
            spin_z: Angle::ZERO,
            score_flyby_tick: SCORE_FLYBY_DURATION_TICKS,
            score_flyby_dir: 0,
            score_flyby_spin: Angle::ZERO,
            cdda_started: false,
            cdda_start_step: CddaStartStep::SetMode,
            cdda_started_tick: 0,
            cdda_next_retry_tick: GONCHAROV_START_DELAY_TICKS,
            cdda_wait_logged: false,
            font: None,
        }
    }

    fn reset_match(&mut self) {
        self.p1_y = (SCREEN_H - PADDLE_H as i16) / 2;
        self.p2_y = (SCREEN_H - PADDLE_H as i16) / 2;
        self.p1_score = 0;
        self.p2_score = 0;
        self.winner = 0;
        self.serve_dir = 1;
        self.spin_x = Angle::ZERO;
        self.spin_y = Angle::ZERO;
        self.spin_z = Angle::ZERO;
        self.score_flyby_tick = SCORE_FLYBY_DURATION_TICKS;
        self.score_flyby_dir = 0;
        self.score_flyby_spin = Angle::ZERO;
        self.reset_ball();
    }

    fn maybe_start_goncharov(&mut self, tick: u32) {
        if self.cdda_started {
            if tick.saturating_sub(self.cdda_started_tick) >= GONCHAROV_LOOP_TICKS {
                self.cdda_started = false;
                self.cdda_start_step = CddaStartStep::SetMode;
                self.cdda_next_retry_tick = tick;
                self.cdda_wait_logged = false;
            } else {
                return;
            }
        }
        if tick < self.cdda_next_retry_tick {
            return;
        }

        trace_cdda_step(self.cdda_start_step);
        if issue_cdda_step(self.cdda_start_step) {
            trace_cdda_step_ack(self.cdda_start_step);
            self.cdda_wait_logged = false;
            match self.cdda_start_step {
                CddaStartStep::SetMode => {
                    self.cdda_start_step = CddaStartStep::Demute;
                    self.cdda_next_retry_tick = tick.saturating_add(2);
                }
                CddaStartStep::Demute => {
                    self.cdda_start_step = CddaStartStep::Play;
                    self.cdda_next_retry_tick = tick.saturating_add(2);
                }
                CddaStartStep::Play => {
                    self.cdda_started = true;
                    self.cdda_started_tick = tick;
                    self.cdda_start_step = CddaStartStep::SetMode;
                    game_trace("magikarp: cdda ok");
                }
            }
        } else {
            self.cdda_next_retry_tick = tick.saturating_add(GONCHAROV_RETRY_TICKS);
            if !self.cdda_wait_logged {
                game_trace("magikarp: cdda busy");
                self.cdda_wait_logged = true;
            }
        }
    }

    fn reset_ball(&mut self) {
        self.ball_x = (SCREEN_W - BALL_SIZE as i16) / 2;
        self.ball_y = (SCREEN_H - BALL_SIZE as i16) / 2 + 8;
        self.ball_vx = BALL_START_VX * self.serve_dir as i16;
        self.ball_vy = BALL_START_VY;
    }

    fn check_win_and_reset(&mut self) {
        if self.p1_score >= WIN_SCORE {
            self.winner = 1;
        } else if self.p2_score >= WIN_SCORE {
            self.winner = 2;
        }
        self.reset_ball();
    }

    fn advance_cube_spin(&mut self) {
        self.spin_x = self.spin_x.add(CUBE_SPIN_X_STEP);
        self.spin_y = self.spin_y.add(CUBE_SPIN_Y_STEP);
        self.spin_z = self.spin_z.add(CUBE_SPIN_Z_STEP);
    }

    fn start_score_flyby(&mut self, dir: i8) {
        self.score_flyby_tick = 0;
        self.score_flyby_dir = dir;
        self.score_flyby_spin = Angle::ZERO;
        sfx::play(VOICE_FLYBY);
    }

    fn advance_score_flyby(&mut self) {
        if self.score_flyby_tick >= SCORE_FLYBY_DURATION_TICKS {
            return;
        }
        self.score_flyby_tick += 1;
        self.score_flyby_spin = self.score_flyby_spin.add(SCORE_FLYBY_SPIN_STEP);
    }

    fn ball_center_y(&self) -> i16 {
        self.ball_y + BALL_SIZE as i16 / 2
    }

    fn ball_hitbox(&self, size: i16) -> BallHitbox {
        let inset = ((BALL_SIZE as i16 - size) / 2).max(0);
        let left = self.ball_x + inset;
        let top = self.ball_y + inset;
        BallHitbox {
            left,
            top,
            right: left + size,
            bottom: top + size,
            size,
            inset,
        }
    }
}

impl Scene for MagikaaaaaarpPong {
    fn init(&mut self, _ctx: &mut Ctx) {
        game_trace("magikarp: init");
        spu::init();
        game_trace("magikarp: spu ok");
        sfx::upload_samples(SPU_SAMPLE_BASE, &SFX_BANK);
        game_trace("magikarp: sfx ok");
        spu::set_cd_volume(CdVolume::linear(1, 4), CdVolume::linear(1, 4));
        spu::enable_cd_audio(true);
        game_trace("magikarp: cd route ok");
        game_trace("magikarp: cdda deferred");
        assert!(SPECTRUM_DATA.len() == SPECTRUM_FRAME_COUNT * SPECTRUM_BANDS);
        game_trace("magikarp: spectrum ok");

        self.font = Some(FontAtlas::upload(&BASIC_8X16, FONT_TPAGE, FONT_CLUT));
        game_trace("magikarp: font ok");
        upload_cube_texture();
        game_trace("magikarp: texture ok");

        self.reset_match();
        game_trace("magikarp: init ok");
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let tick = ctx.sim_tick.as_u32();
        self.advance_cube_spin();
        self.advance_score_flyby();
        self.maybe_start_goncharov(tick);
        let cube_scale = cube_scale_q12(tick);
        let ball_size = ball_size_for_scale(cube_scale);

        if self.winner != 0 {
            if ctx.just_pressed(button::START) {
                self.reset_match();
            }
            return;
        }

        if ctx.is_held(button::UP) {
            self.p1_y -= PADDLE_SPEED;
        }
        if ctx.is_held(button::DOWN) {
            self.p1_y += PADDLE_SPEED;
        }
        clamp_paddle(&mut self.p1_y);

        if tick % AI_REACTION_TICKS == 0 {
            let target = ai_target_y(tick, self.ball_vx, self.ball_center_y());
            let paddle_mid = self.p2_y + PADDLE_H as i16 / 2;
            if target < paddle_mid - AI_HYSTERESIS {
                self.p2_y -= AI_SPEED;
            } else if target > paddle_mid + AI_HYSTERESIS {
                self.p2_y += AI_SPEED;
            }
        }
        clamp_paddle(&mut self.p2_y);

        self.ball_x += self.ball_vx;
        self.ball_y += self.ball_vy;

        let mut hitbox = self.ball_hitbox(ball_size);
        if hitbox.top <= PLAYFIELD_TOP {
            self.ball_y = PLAYFIELD_TOP - hitbox.inset;
            self.ball_vy = self.ball_vy.abs();
            sfx::play(VOICE_WALL);
            hitbox = self.ball_hitbox(ball_size);
        } else if hitbox.bottom >= PLAYFIELD_BOT {
            self.ball_y = PLAYFIELD_BOT - hitbox.size - hitbox.inset;
            self.ball_vy = -self.ball_vy.abs();
            sfx::play(VOICE_WALL);
            hitbox = self.ball_hitbox(ball_size);
        }

        let left_face = PADDLE_MARGIN + PADDLE_W as i16;
        if self.ball_vx < 0
            && hitbox.left <= left_face
            && hitbox.right >= PADDLE_MARGIN
            && hitbox.bottom >= self.p1_y
            && hitbox.top <= self.p1_y + PADDLE_H as i16
        {
            self.ball_x = left_face - hitbox.inset;
            self.ball_vx = bounce_speed(-self.ball_vx);
            self.ball_vy = spin_from_paddle(hitbox.center_y(), self.p1_y, self.ball_vy);
            sfx::play(VOICE_PADDLE);
            hitbox = self.ball_hitbox(ball_size);
        }

        let right_face = SCREEN_W - PADDLE_MARGIN - PADDLE_W as i16;
        if self.ball_vx > 0
            && hitbox.right >= right_face
            && hitbox.left <= SCREEN_W - PADDLE_MARGIN
            && hitbox.bottom >= self.p2_y
            && hitbox.top <= self.p2_y + PADDLE_H as i16
        {
            self.ball_x = right_face - hitbox.size - hitbox.inset;
            self.ball_vx = -bounce_speed(self.ball_vx);
            self.ball_vy = spin_from_paddle(hitbox.center_y(), self.p2_y, self.ball_vy);
            sfx::play(VOICE_PADDLE);
            hitbox = self.ball_hitbox(ball_size);
        }

        if hitbox.right < 0 {
            self.p2_score = self.p2_score.saturating_add(1);
            self.serve_dir = -1;
            self.start_score_flyby(-1);
            sfx::play(VOICE_SCORE);
            self.check_win_and_reset();
        } else if hitbox.left > SCREEN_W {
            self.p1_score = self.p1_score.saturating_add(1);
            self.serve_dir = 1;
            self.start_score_flyby(1);
            sfx::play(VOICE_SCORE);
            self.check_win_and_reset();
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        self.build_frame_ot(ctx.sim_tick.as_u32());
        unsafe { OT.submit() };
        self.draw_hud(ctx.sim_tick.as_u32());
    }
}

impl MagikaaaaaarpPong {
    fn build_frame_ot(&self, sim_tick: u32) {
        let ot = unsafe { &mut OT };
        let rects = unsafe { &mut RECTS };
        ot.clear();

        self.add_spectrum_to_ot(ot, sim_tick);

        rects[0] = RectFlat::new(
            0,
            PLAYFIELD_TOP - 1,
            SCREEN_W as u16,
            BORDER_H,
            INK.0,
            INK.1,
            INK.2,
        );
        rects[1] = RectFlat::new(
            0,
            PLAYFIELD_BOT,
            SCREEN_W as u16,
            BORDER_H,
            INK.0,
            INK.1,
            INK.2,
        );
        ot.add(1, &mut rects[0], RectFlat::WORDS);
        ot.add(1, &mut rects[1], RectFlat::WORDS);

        let mut y = PLAYFIELD_TOP + 7;
        let mut idx = CENTER_RECTS_START;
        while y + 8 <= PLAYFIELD_BOT - 4 && idx < CENTER_RECTS_START + CENTER_DASH_RECTS {
            rects[idx] = RectFlat::new(
                (SCREEN_W - 4) / 2,
                y,
                4,
                8,
                MUTED_INK.0,
                MUTED_INK.1,
                MUTED_INK.2,
            );
            ot.add(2, &mut rects[idx], RectFlat::WORDS);
            y += 16;
            idx += 1;
        }

        rects[PADDLE_RECT_START] = RectFlat::new(
            PADDLE_MARGIN,
            self.p1_y,
            PADDLE_W,
            PADDLE_H,
            INK.0,
            INK.1,
            INK.2,
        );
        rects[PADDLE_RECT_START + 1] = RectFlat::new(
            SCREEN_W - PADDLE_MARGIN - PADDLE_W as i16,
            self.p2_y,
            PADDLE_W,
            PADDLE_H,
            INK.0,
            INK.1,
            INK.2,
        );
        ot.add(5, &mut rects[PADDLE_RECT_START], RectFlat::WORDS);
        ot.add(5, &mut rects[PADDLE_RECT_START + 1], RectFlat::WORDS);

        self.add_cube_to_ot(ot, cube_scale_q12(sim_tick));
        self.add_score_flyby_to_ot(ot);
    }

    fn add_spectrum_to_ot(&self, ot: &mut OrderingTable<16>, tick: u32) {
        let elapsed = if self.cdda_started {
            Some(tick.saturating_sub(self.cdda_started_tick))
        } else {
            None
        };
        let offset = elapsed
            .map(|tick| {
                let frame = ((tick % GONCHAROV_LOOP_TICKS) / SPECTRUM_TICKS_PER_FRAME) as usize;
                (frame % SPECTRUM_FRAME_COUNT) * SPECTRUM_BANDS
            })
            .unwrap_or(0);
        let quads = unsafe { &mut SPECTRUM_QUADS };

        let mut band = 0;
        while band < SPECTRUM_BANDS {
            let amp = elapsed
                .map(|_| SPECTRUM_DATA[offset + band] as u16)
                .unwrap_or(0);
            let h = 2 + ((amp * SPECTRUM_MAX_H) / 255);
            let x = SPECTRUM_X0 + (band as i16 * SPECTRUM_BAR_PITCH);
            let y = SPECTRUM_BASE_Y - h as i16;
            let top = spectrum_top_color(amp);
            let bottom = spectrum_bottom_color(top);
            quads[band] = QuadGouraud::new(
                [
                    (x, y),
                    (x + SPECTRUM_BAR_W as i16, y),
                    (x, SPECTRUM_BASE_Y),
                    (x + SPECTRUM_BAR_W as i16, SPECTRUM_BASE_Y),
                ],
                [top, top, bottom, bottom],
            );
            ot.add(SPECTRUM_OT_SLOT, &mut quads[band], QuadGouraud::WORDS);
            band += 1;
        }
    }

    fn add_cube_to_ot(&self, ot: &mut OrderingTable<16>, scale_q12: i32) {
        let cx = self.ball_x + BALL_SIZE as i16 / 2;
        let cy = self.ball_y + BALL_SIZE as i16 / 2;
        let quads = unsafe { &mut CUBE_QUADS };
        let mut projected = [ScreenVertex::ZERO; 8];

        let mut i = 0;
        while i < CUBE_VERTS.len() {
            let scaled = scale_vec(CUBE_VERTS[i], scale_q12);
            let rotated = rotate_vec(scaled, self.spin_x, self.spin_y, self.spin_z);
            let z = rotated.z + CUBE_Z;
            if z <= 24 {
                return;
            }
            projected[i] = project_vertex(rotated, z, cx, cy);
            i += 1;
        }

        let mut quad_idx = 0;
        i = 0;
        while i < CUBE_FACES.len() && quad_idx < quads.len() {
            let face = CUBE_FACES[i];
            let normal = rotate_vec(face.normal, self.spin_x, self.spin_y, self.spin_z);
            if normal.z < -96 {
                let [a, b, c, d] = face.indices;
                let avg_z = (projected[a].z + projected[b].z + projected[c].z + projected[d].z) / 4;
                let tint = cube_face_tint(normal.z);
                quads[quad_idx] = QuadTexturedMaterial::with_material(
                    [
                        (projected[a].x, projected[a].y),
                        (projected[b].x, projected[b].y),
                        (projected[c].x, projected[c].y),
                        (projected[d].x, projected[d].y),
                    ],
                    CUBE_UVS,
                    TextureMaterial::opaque(
                        CUBE_CLUT.uv_clut_word(),
                        CUBE_TPAGE.uv_tpage_word(0),
                        tint,
                    ),
                );
                let slot = cube_depth_slot(avg_z);
                ot.add(slot, &mut quads[quad_idx], QuadTexturedMaterial::WORDS);
                quad_idx += 1;
            }
            i += 1;
        }
    }

    fn add_score_flyby_to_ot(&self, ot: &mut OrderingTable<16>) {
        if self.score_flyby_tick >= SCORE_FLYBY_DURATION_TICKS || self.score_flyby_dir == 0 {
            return;
        }

        let quad = unsafe { &mut SCORE_FLYBY_QUAD };
        let center_x = score_flyby_x(self.score_flyby_tick, self.score_flyby_dir);
        let half = SCORE_FLYBY_SIZE as i32 / 2;
        let sin = self.score_flyby_spin.sin_q12();
        let cos = self.score_flyby_spin.cos_q12();
        let corners = [(-half, -half), (half, -half), (-half, half), (half, half)];
        let mut verts = [(0, 0); 4];
        let mut i = 0;
        while i < corners.len() {
            let (x, y) = corners[i];
            verts[i] = (
                clamp_i16(center_x as i32 + ((x * cos - y * sin) >> 12)),
                clamp_i16(SCORE_FLYBY_Y as i32 + ((x * sin + y * cos) >> 12)),
            );
            i += 1;
        }

        *quad = QuadTexturedMaterial::with_material(
            verts,
            CUBE_UVS,
            TextureMaterial::opaque(
                CUBE_CLUT.uv_clut_word(),
                CUBE_TPAGE.uv_tpage_word(0),
                (128, 128, 128),
            ),
        );
        ot.add(SCORE_FLYBY_OT_SLOT, quad, QuadTexturedMaterial::WORDS);
    }

    fn draw_hud(&self, sim_tick: u32) {
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let p1 = digit_str(self.p1_score);
        let p2 = digit_str(self.p2_score);

        draw_title(font, sim_tick);
        font.draw_text(24, SCORE_Y, "P1", INK);
        font.draw_text(60, SCORE_Y, p1.as_str(), INK);
        font.draw_text(SCREEN_W - 24 - 8 * 2, SCORE_Y, "P2", INK);
        font.draw_text(SCREEN_W - 60 - 8, SCORE_Y, p2.as_str(), INK);

        if self.winner != 0 {
            let msg = if self.winner == 1 {
                "YOU WIN!"
            } else {
                "AI WINS!"
            };
            font.draw_text((SCREEN_W - 8 * 7) / 2, 108, msg, INK);
            font.draw_text(
                (SCREEN_W - 17 * 8) / 2,
                132,
                "START to play again",
                MUTED_INK,
            );
        }
    }
}

#[derive(Copy, Clone)]
struct Vec3 {
    x: i32,
    y: i32,
    z: i32,
}

impl Vec3 {
    const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }
}

#[derive(Copy, Clone)]
struct ScreenVertex {
    x: i16,
    y: i16,
    z: i32,
}

impl ScreenVertex {
    const ZERO: Self = Self { x: 0, y: 0, z: 0 };
}

#[derive(Copy, Clone)]
struct BallHitbox {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
    size: i16,
    inset: i16,
}

impl BallHitbox {
    fn center_y(self) -> i16 {
        self.top + self.size / 2
    }
}

#[derive(Copy, Clone)]
struct CubeFace {
    indices: [usize; 4],
    normal: Vec3,
}

const CUBE_VERTS: [Vec3; 8] = [
    Vec3::new(-CUBE_HALF, -CUBE_HALF, -CUBE_HALF),
    Vec3::new(CUBE_HALF, -CUBE_HALF, -CUBE_HALF),
    Vec3::new(-CUBE_HALF, CUBE_HALF, -CUBE_HALF),
    Vec3::new(CUBE_HALF, CUBE_HALF, -CUBE_HALF),
    Vec3::new(-CUBE_HALF, -CUBE_HALF, CUBE_HALF),
    Vec3::new(CUBE_HALF, -CUBE_HALF, CUBE_HALF),
    Vec3::new(-CUBE_HALF, CUBE_HALF, CUBE_HALF),
    Vec3::new(CUBE_HALF, CUBE_HALF, CUBE_HALF),
];

const N: i32 = 4096;
const CUBE_FACES: [CubeFace; 6] = [
    CubeFace {
        indices: [0, 1, 2, 3],
        normal: Vec3::new(0, 0, -N),
    },
    CubeFace {
        indices: [5, 4, 7, 6],
        normal: Vec3::new(0, 0, N),
    },
    CubeFace {
        indices: [4, 0, 6, 2],
        normal: Vec3::new(-N, 0, 0),
    },
    CubeFace {
        indices: [1, 5, 3, 7],
        normal: Vec3::new(N, 0, 0),
    },
    CubeFace {
        indices: [4, 5, 0, 1],
        normal: Vec3::new(0, -N, 0),
    },
    CubeFace {
        indices: [2, 3, 6, 7],
        normal: Vec3::new(0, N, 0),
    },
];

fn cube_scale_q12(tick: u32) -> i32 {
    let phase = Angle::per_frames(CUBE_PULSE_PERIOD_TICKS).mul_tick(SimTick::from_u32(tick));
    let eased = phase.cos_q12() + 4096;
    CUBE_MIN_SCALE_Q12 + ((CUBE_MAX_SCALE_Q12 - CUBE_MIN_SCALE_Q12) * eased) / 8192
}

fn ball_size_for_scale(scale_q12: i32) -> i16 {
    (((BALL_SIZE as i32 * scale_q12) + 2048) >> 12).clamp(1, BALL_SIZE as i32) as i16
}

fn scale_vec(v: Vec3, scale_q12: i32) -> Vec3 {
    Vec3 {
        x: (v.x * scale_q12) >> 12,
        y: (v.y * scale_q12) >> 12,
        z: (v.z * scale_q12) >> 12,
    }
}

fn rotate_vec(v: Vec3, ax: Angle, ay: Angle, az: Angle) -> Vec3 {
    let sx = ax.sin_q12();
    let cx = ax.cos_q12();
    let sy = ay.sin_q12();
    let cy = ay.cos_q12();
    let sz = az.sin_q12();
    let cz = az.cos_q12();

    let y1 = ((v.y * cx) - (v.z * sx)) >> 12;
    let z1 = ((v.y * sx) + (v.z * cx)) >> 12;
    let x1 = v.x;

    let x2 = ((x1 * cy) + (z1 * sy)) >> 12;
    let z2 = ((-x1 * sy) + (z1 * cy)) >> 12;
    let y2 = y1;

    Vec3 {
        x: ((x2 * cz) - (y2 * sz)) >> 12,
        y: ((x2 * sz) + (y2 * cz)) >> 12,
        z: z2,
    }
}

fn project_vertex(v: Vec3, z: i32, cx: i16, cy: i16) -> ScreenVertex {
    let x = cx as i32 + (v.x * CUBE_PROJECTION) / z;
    let y = cy as i32 + (v.y * CUBE_PROJECTION) / z;
    ScreenVertex {
        x: clamp_i16(x),
        y: clamp_i16(y),
        z,
    }
}

fn cube_face_tint(normal_z: i32) -> (u8, u8, u8) {
    let front = (-normal_z).clamp(0, 4096);
    let shade = 82 + ((front * 46) >> 12) as u8;
    (shade, shade, shade)
}

fn spectrum_top_color(amp: u16) -> (u8, u8, u8) {
    let t = amp.saturating_sub(32) * 255 / 223;
    blend_color(SPECTRUM_LOW, SPECTRUM_HIGH, t.min(255))
}

fn spectrum_bottom_color(top: (u8, u8, u8)) -> (u8, u8, u8) {
    blend_color(PAPER, top, 72)
}

fn blend_color(a: (u8, u8, u8), b: (u8, u8, u8), t: u16) -> (u8, u8, u8) {
    (
        blend_channel(a.0, b.0, t),
        blend_channel(a.1, b.1, t),
        blend_channel(a.2, b.2, t),
    )
}

fn blend_channel(a: u8, b: u8, t: u16) -> u8 {
    let delta = b as i32 - a as i32;
    (a as i32 + (delta * t as i32) / 255).clamp(0, 255) as u8
}

fn cube_depth_slot(avg_z: i32) -> usize {
    let slot = avg_z / 24;
    slot.clamp(6, 11) as usize
}

fn score_flyby_x(tick: u16, dir: i8) -> i16 {
    let margin = SCORE_FLYBY_SIZE + 8;
    let span = SCREEN_W + margin * 2;
    let offset = ((span as i32 * tick as i32) / SCORE_FLYBY_DURATION_TICKS as i32) as i16;
    if dir > 0 {
        -margin + offset
    } else {
        SCREEN_W + margin - offset
    }
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn clamp_paddle(y: &mut i16) {
    let min = PLAYFIELD_TOP + 2;
    let max = PLAYFIELD_BOT - PADDLE_H as i16 - 2;
    if *y < min {
        *y = min;
    }
    if *y > max {
        *y = max;
    }
}

fn bounce_speed(mut speed: i16) -> i16 {
    if speed.abs() < BALL_MAX_SPEED {
        if speed >= 0 {
            speed = (speed + 1).min(BALL_MAX_SPEED);
        } else {
            speed = (speed - 1).max(-BALL_MAX_SPEED);
        }
    }
    speed
}

fn spin_from_paddle(ball_center_y: i16, paddle_y: i16, prev_vy: i16) -> i16 {
    let relative = ball_center_y - (paddle_y + PADDLE_H as i16 / 2);
    let h = PADDLE_H as i16 / 2;
    if relative < -h / 3 {
        -2
    } else if relative > h / 3 {
        2
    } else if prev_vy == 0 {
        1
    } else {
        prev_vy.signum()
    }
}

fn ai_target_y(tick: u32, ball_vx: i16, ball_center_y: i16) -> i16 {
    if ball_vx <= 0 {
        return (PLAYFIELD_TOP + PLAYFIELD_BOT) / 2;
    }
    let offsets = [-24, 16, -12, 28];
    ball_center_y + offsets[((tick / 90) & 3) as usize]
}

fn upload_cube_texture() {
    let texture =
        Texture::from_bytes(include_bytes!("../assets/magikaaaaaarp_album.psxt")).expect("cube");
    upload_bytes(
        VramRect::new(
            CUBE_TPAGE.x(),
            CUBE_TPAGE.y(),
            texture.halfwords_per_row(),
            texture.height(),
        ),
        texture.pixel_bytes(),
    );
    upload_opaque_clut(
        VramRect::new(CUBE_CLUT.x(), CUBE_CLUT.y(), texture.clut_entries(), 1),
        texture.clut_bytes(),
    );
}

fn upload_opaque_clut(rect: VramRect, bytes: &[u8]) {
    let mut marked = [0u8; 512];
    assert!(bytes.len() <= marked.len());
    assert!(bytes.len() % 2 == 0);

    let mut i = 0;
    while i < bytes.len() {
        let mut color = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if color == 0 {
            color = 1;
        }
        marked[i..i + 2].copy_from_slice(&color.to_le_bytes());
        i += 2;
    }
    upload_bytes(rect, &marked[..bytes.len()]);
}

fn trace_cdda_step(step: CddaStartStep) {
    match step {
        CddaStartStep::SetMode => game_trace("magikarp: cdda setmode"),
        CddaStartStep::Demute => game_trace("magikarp: cdda demute"),
        CddaStartStep::Play => game_trace("magikarp: cdda play"),
    }
}

fn trace_cdda_step_ack(step: CddaStartStep) {
    match step {
        CddaStartStep::SetMode => game_trace("magikarp: cdda setmode ack"),
        CddaStartStep::Demute => game_trace("magikarp: cdda demute ack"),
        CddaStartStep::Play => game_trace("magikarp: cdda play ack"),
    }
}

fn issue_cdda_step(step: CddaStartStep) -> bool {
    match step {
        CddaStartStep::SetMode => {
            cdrom::try_set_mode(cdrom::MODE_CDDA, CDROM_COMMAND_SPINS).is_some()
        }
        CddaStartStep::Demute => cdrom::try_demute(CDROM_COMMAND_SPINS).is_some(),
        CddaStartStep::Play => {
            cdrom::try_play_track(TRACK_GONCHAROV, CDROM_COMMAND_SPINS).is_some()
        }
    }
}

fn draw_title(font: &FontAtlas, tick: u32) {
    let amp = title_wave_amplitude(tick);
    if amp == 0 {
        font.draw_text(TITLE_X, TITLE_Y, "MAGIKAAAAARPONG", INK);
        return;
    }

    let base = Angle::per_frames(54).mul_tick(SimTick::from_u32(tick));
    let mut i = 0;
    while i < TITLE.len() {
        let phase = base.add(Angle::from_q12(((i as u16) * 248) & 0x0FFF));
        let y = TITLE_Y + (((phase.sin_q12() * amp as i32) >> 12) as i16);
        let glyph = ByteStr([TITLE[i]]);
        font.draw_text(TITLE_X + i as i16 * TITLE_GLYPH_W, y, glyph.as_str(), INK);
        i += 1;
    }
}

fn title_wave_amplitude(tick: u32) -> i16 {
    let phase = tick % TITLE_WAVE_PERIOD_TICKS;
    if phase >= TITLE_WAVE_ACTIVE_TICKS {
        return 0;
    }

    let envelope = if phase < TITLE_WAVE_FADE_TICKS {
        phase
    } else {
        let out = TITLE_WAVE_ACTIVE_TICKS - phase;
        if out < TITLE_WAVE_FADE_TICKS {
            out
        } else {
            TITLE_WAVE_FADE_TICKS
        }
    };
    ((envelope * TITLE_WAVE_MAX_PX as u32) / TITLE_WAVE_FADE_TICKS) as i16
}

fn digit_str(n: u8) -> DigitStr {
    let c = if n < 10 { b'0' + n } else { b'X' };
    DigitStr([c])
}

struct DigitStr([u8; 1]);
impl DigitStr {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

struct ByteStr([u8; 1]);
impl ByteStr {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

#[no_mangle]
fn main() -> ! {
    let config = Config {
        clear_color: PAPER,
        ..Config::default()
    };
    let mut game = MagikaaaaaarpPong::new();
    App::run(config, &mut game);
}
