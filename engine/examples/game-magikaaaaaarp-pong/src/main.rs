//! `game-magikaaaaaarp-pong` -- magikAAAAArp-themed Pong with a
//! large rotating album cube.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate psx_rt;

use psx_asset::Texture;
use psx_engine::{button, sfx, Angle, App, Config, Ctx, Scene};
use psx_font::{fonts::BASIC_8X16, FontAtlas};
use psx_gpu::material::TextureMaterial;
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::{QuadTexturedMaterial, RectFlat};
use psx_spu::{self as spu, SpuAddr, Voice, Volume};
use psx_vram::{upload_bytes, Clut, TexDepth, Tpage, VramRect};

const SCREEN_W: i16 = 320;
const SCREEN_H: i16 = 240;

const BORDER_H: u16 = 3;
const PLAYFIELD_TOP: i16 = 36;
const PLAYFIELD_BOT: i16 = SCREEN_H - BORDER_H as i16;

const PADDLE_W: u16 = 8;
const PADDLE_H: u16 = 56;
const PADDLE_MARGIN: i16 = 10;
const PADDLE_SPEED: i16 = 4;

const AI_SPEED: i16 = 3;
const AI_HYSTERESIS: i16 = 4;

const BALL_SIZE: u16 = 42;
const BALL_START_VX: i16 = 2;
const BALL_START_VY: i16 = 1;
const BALL_MAX_SPEED: i16 = 4;
const WIN_SCORE: u8 = 5;

const CUBE_HALF: i32 = 34;
const CUBE_Z: i32 = 180;
const CUBE_PROJECTION: i32 = 190;
const CUBE_UVS: [(u8, u8); 4] = [(0, 0), (63, 0), (0, 63), (63, 63)];

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const CUBE_TPAGE: Tpage = Tpage::new(640, 0, TexDepth::Bit4);
const CUBE_CLUT: Clut = Clut::new(640, 256);

const SPU_SAMPLE_BASE: SpuAddr = SpuAddr::new(0x1010);
const VOICE_WALL: Voice = Voice::V0;
const VOICE_PADDLE: Voice = Voice::V1;
const VOICE_SCORE: Voice = Voice::V2;

const SFX_BANK: [sfx::Sample<'static>; 3] = [
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
];

static mut OT: OrderingTable<16> = OrderingTable::new();
static mut RECTS: [RectFlat; 24] = [const { RectFlat::new(0, 0, 0, 0, 0, 0, 0) }; 24];
static mut CUBE_QUADS: [QuadTexturedMaterial; 6] = [const {
    QuadTexturedMaterial::with_material(
        [(0, 0), (0, 0), (0, 0), (0, 0)],
        CUBE_UVS,
        TextureMaterial::opaque(0, 0, (0, 0, 0)),
    )
}; 6];

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
        self.reset_ball();
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
        self.spin_x = self.spin_x.add(Angle::from_raw_q16(0x01A0));
        self.spin_y = self.spin_y.add(Angle::from_raw_q16(0x0260));
        self.spin_z = self.spin_z.add(Angle::from_raw_q16(0x00B0));
    }
}

impl Scene for MagikaaaaaarpPong {
    fn init(&mut self, _ctx: &mut Ctx) {
        spu::init();
        sfx::upload_samples(SPU_SAMPLE_BASE, &SFX_BANK);

        self.font = Some(FontAtlas::upload(&BASIC_8X16, FONT_TPAGE, FONT_CLUT));
        upload_cube_texture();

        self.reset_match();
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.advance_cube_spin();

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

        let target = self.ball_y + BALL_SIZE as i16 / 2;
        let paddle_mid = self.p2_y + PADDLE_H as i16 / 2;
        if target < paddle_mid - AI_HYSTERESIS {
            self.p2_y -= AI_SPEED;
        } else if target > paddle_mid + AI_HYSTERESIS {
            self.p2_y += AI_SPEED;
        }
        clamp_paddle(&mut self.p2_y);

        self.ball_x += self.ball_vx;
        self.ball_y += self.ball_vy;

        if self.ball_y <= PLAYFIELD_TOP {
            self.ball_y = PLAYFIELD_TOP;
            self.ball_vy = self.ball_vy.abs();
            sfx::play(VOICE_WALL);
        } else if self.ball_y + BALL_SIZE as i16 >= PLAYFIELD_BOT {
            self.ball_y = PLAYFIELD_BOT - BALL_SIZE as i16;
            self.ball_vy = -self.ball_vy.abs();
            sfx::play(VOICE_WALL);
        }

        let left_face = PADDLE_MARGIN + PADDLE_W as i16;
        if self.ball_vx < 0
            && self.ball_x <= left_face
            && self.ball_x + BALL_SIZE as i16 >= PADDLE_MARGIN
            && self.ball_y + BALL_SIZE as i16 >= self.p1_y
            && self.ball_y <= self.p1_y + PADDLE_H as i16
        {
            self.ball_x = left_face;
            self.ball_vx = bounce_speed(-self.ball_vx);
            self.ball_vy = spin_from_paddle(self.ball_y, self.p1_y, self.ball_vy);
            sfx::play(VOICE_PADDLE);
        }

        let right_face = SCREEN_W - PADDLE_MARGIN - PADDLE_W as i16;
        if self.ball_vx > 0
            && self.ball_x + BALL_SIZE as i16 >= right_face
            && self.ball_x <= SCREEN_W - PADDLE_MARGIN
            && self.ball_y + BALL_SIZE as i16 >= self.p2_y
            && self.ball_y <= self.p2_y + PADDLE_H as i16
        {
            self.ball_x = right_face - BALL_SIZE as i16;
            self.ball_vx = -bounce_speed(self.ball_vx);
            self.ball_vy = spin_from_paddle(self.ball_y, self.p2_y, self.ball_vy);
            sfx::play(VOICE_PADDLE);
        }

        if (self.ball_x + BALL_SIZE as i16) < 0 {
            self.p2_score = self.p2_score.saturating_add(1);
            self.serve_dir = -1;
            sfx::play(VOICE_SCORE);
            self.check_win_and_reset();
        } else if self.ball_x > SCREEN_W {
            self.p1_score = self.p1_score.saturating_add(1);
            self.serve_dir = 1;
            sfx::play(VOICE_SCORE);
            self.check_win_and_reset();
        }
    }

    fn render(&mut self, _ctx: &mut Ctx) {
        self.build_frame_ot();
        unsafe { OT.submit() };
        self.draw_hud();
    }
}

impl MagikaaaaaarpPong {
    fn build_frame_ot(&self) {
        let ot = unsafe { &mut OT };
        let rects = unsafe { &mut RECTS };
        ot.clear();

        rects[0] = RectFlat::new(
            0,
            PLAYFIELD_TOP - 1,
            SCREEN_W as u16,
            BORDER_H,
            120,
            132,
            176,
        );
        rects[1] = RectFlat::new(0, PLAYFIELD_BOT, SCREEN_W as u16, BORDER_H, 120, 132, 176);
        ot.add(1, &mut rects[0], RectFlat::WORDS);
        ot.add(1, &mut rects[1], RectFlat::WORDS);

        let mut y = PLAYFIELD_TOP + 7;
        let mut idx = 2;
        while y + 8 <= PLAYFIELD_BOT - 4 && idx < 16 {
            rects[idx] = RectFlat::new((SCREEN_W - 4) / 2, y, 4, 8, 88, 104, 148);
            ot.add(2, &mut rects[idx], RectFlat::WORDS);
            y += 16;
            idx += 1;
        }

        rects[16] = RectFlat::new(PADDLE_MARGIN, self.p1_y, PADDLE_W, PADDLE_H, 246, 246, 252);
        rects[17] = RectFlat::new(
            SCREEN_W - PADDLE_MARGIN - PADDLE_W as i16,
            self.p2_y,
            PADDLE_W,
            PADDLE_H,
            246,
            246,
            252,
        );
        ot.add(5, &mut rects[16], RectFlat::WORDS);
        ot.add(5, &mut rects[17], RectFlat::WORDS);

        self.add_cube_to_ot(ot);
    }

    fn add_cube_to_ot(&self, ot: &mut OrderingTable<16>) {
        let cx = self.ball_x + BALL_SIZE as i16 / 2;
        let cy = self.ball_y + BALL_SIZE as i16 / 2;
        let quads = unsafe { &mut CUBE_QUADS };
        let mut projected = [ScreenVertex::ZERO; 8];

        let mut i = 0;
        while i < CUBE_VERTS.len() {
            let rotated = rotate_vec(CUBE_VERTS[i], self.spin_x, self.spin_y, self.spin_z);
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

    fn draw_hud(&self) {
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let p1 = digit_str(self.p1_score);
        let p2 = digit_str(self.p2_score);

        font.draw_text(92, 6, "magikAAAAArp PONG", (255, 226, 112));
        font.draw_text(24, 24, "P1", (150, 196, 255));
        font.draw_text(60, 24, p1.as_str(), (226, 230, 248));
        font.draw_text(SCREEN_W - 24 - 8 * 2, 24, "P2", (255, 174, 150));
        font.draw_text(SCREEN_W - 60 - 8, 24, p2.as_str(), (226, 230, 248));

        if self.winner != 0 {
            let msg = if self.winner == 1 {
                "YOU WIN!"
            } else {
                "AI WINS!"
            };
            font.draw_text((SCREEN_W - 8 * 7) / 2, 108, msg, (255, 226, 112));
            font.draw_text(
                (SCREEN_W - 17 * 8) / 2,
                132,
                "START to play again",
                (176, 184, 216),
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

fn cube_depth_slot(avg_z: i32) -> usize {
    let slot = avg_z / 24;
    slot.clamp(6, 11) as usize
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

fn spin_from_paddle(ball_y: i16, paddle_y: i16, prev_vy: i16) -> i16 {
    let relative = (ball_y + BALL_SIZE as i16 / 2) - (paddle_y + PADDLE_H as i16 / 2);
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

#[no_mangle]
fn main() -> ! {
    let config = Config {
        clear_color: (7, 9, 22),
        ..Config::default()
    };
    let mut game = MagikaaaaaarpPong::new();
    App::run(config, &mut game);
}
