//! `showcase-vector`: the cooked-SVG runtime at native resolution, and its
//! cycle meter.
//!
//! One frame shows the test gallery (every cooker feature), a rotated and
//! scaled group, a clipped group, a faded group, and the Cortex stance HUD in
//! two states. Every draw is timed with root counter 2 (system clock / 8) and
//! the figures are written to [`MEASURE`], a RAM block tagged with a magic
//! header that a headless run finds in `--dump-ram`.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate psx_rt;

#[path = "../../editor-playtest/src/stance_hud_layout.rs"]
mod hud;

#[cfg(feature = "hud-sprites")]
#[path = "../../editor-playtest/src/stance_hud_packets.rs"]
mod hud_packets;
#[cfg(feature = "hud-sprites")]
#[path = "../../editor-playtest/src/stance_hud_sprites.rs"]
mod hud_sprites;

use hud::{draw_hud, Bar, HudIds, HudState, SCRATCH_VERTICES};
use psx_engine::{App, Config, Ctx, OtFrame, Scene};
use psx_gpu::material::BlendMode;
use psx_gpu::ot::OrderingTable;
use psx_io::timers::{self, Timer};
use psx_vector::{
    include_vector, BlendOverride, ClipRect, Emit, PacketCache, Placement, Scratch, Style,
    VectorAsset, VectorPacket,
};

static GALLERY: &[u8] = include_vector!("../assets/gallery.psvg");
static STANCE_HUD: &[u8] = include_vector!("../../editor-playtest/assets/hud/stance_hud.psvg");

static mut SCRATCH: Scratch<SCRATCH_VERTICES> = Scratch::new();

/// Ordering table and the cached HUD packets, linked into it every frame.
static mut OT: OrderingTable<16> = OrderingTable::new();
static mut CACHE: PacketCache<128> = PacketCache::new();

/// The sprite HUDs: eight for the grid frame (two stances by four levels), the first one alone for the
/// cycle measurements.
#[cfg(feature = "hud-sprites")]
static mut SPRITE_HUDS: [hud_packets::HudPackets; 8] =
    [const { hud_packets::HudPackets::new() }; 8];

/// The health of the two bars at game frame `t` of the 64 frame timeline: Horizon drains from 10 to
/// 0 a cell a frame and refills, then Zenith does the same from 8, so a swap and both the patched
/// drain and the refill show in one loop.
fn demo_health(t: u32) -> [u8; 2] {
    let horizon = match t {
        0..=3 => 10,
        4..=13 => 13 - t as u8,
        14..=15 => 0,
        _ => 10,
    };
    let zenith = match t {
        0..=35 => 8,
        36..=43 => 43 - t as u8,
        44..=47 => 0,
        _ => 8,
    };
    [horizon, zenith]
}

/// One size of the bar on its own, for the `HUD_SIZE` grid frame.
#[cfg(feature = "hud-sprites")]
static mut SIZE_HUDS: [hud_packets::AlonePackets; 8] =
    [const { hud_packets::AlonePackets::new() }; 8];

/// Draw the inactive stance's cells on the back bar (variants that have them).
const BACK: bool = option_env!("HUD_BACK").is_some();

/// `hud-grid` build: only the grid of HUD states is drawn.
const GRID: bool = cfg!(feature = "hud-grid");

/// `hud-sprites` build: the sprite HUD stands in for the vector one.
const SPRITES: bool = cfg!(feature = "hud-sprites");

/// `hud-anim` build: only the animated sprite HUD, one HUD on the screen, swapping stance over and over.
const ANIM: bool = cfg!(feature = "hud-anim");

/// `HUD_SWAP` at build time: 0, or the stance a swap grid swaps to (1 Zenith, 2 Horizon). The grid
/// then shows one column of swap frames 0, 2, 4 and 6 instead of the fill levels.
const SWAP_GRID: i32 = env_px(option_env!("HUD_SWAP"), 0);

/// `HUD_SIZE` at build time: the size of the bar the grid frame shows alone, or `NO_SIZE`.
const NO_SIZE: i32 = 99;
const SIZE_GRID: i32 = env_px(option_env!("HUD_SIZE"), NO_SIZE);

const fn env_px(s: Option<&str>, default: i32) -> i32 {
    match s {
        Some(s) => {
            let b = s.as_bytes();
            let (mut i, mut v) = (0, 0i32);
            while i < b.len() {
                v = v * 10 + (b[i] - b'0') as i32;
                i += 1;
            }
            v
        }
        None => default,
    }
}
/// The clear colour: navy, or `HUD_BG_R`, `HUD_BG_G`, `HUD_BG_B` at build time, to check how
/// translucent parts blend with a lighter scene.
const CLEAR: (u8, u8, u8) = (
    env_px(option_env!("HUD_BG_R"), 16) as u8,
    env_px(option_env!("HUD_BG_G"), 24) as u8,
    env_px(option_env!("HUD_BG_B"), 40) as u8,
);
/// Left edge of the Horizon and Zenith columns of the grid, in pixels.
const GRID_X: [i32; 2] = [
    env_px(option_env!("HUD_GRID_X0"), 2),
    env_px(option_env!("HUD_GRID_X1"), 162),
];

const HORIZON: [u8; 3] = [234, 82, 48];
const ZENITH: [u8; 3] = [76, 202, 181];

/// One timed draw: an 8-byte tag, the triangles emitted, and the cycles.
#[repr(C)]
#[derive(Clone, Copy)]
struct Row {
    tag: [u8; 8],
    triangles: u32,
    cycles: u32,
}

/// The result block. `magic` is `PSVGMEAS` followed by the row count.
#[repr(C)]
struct Measure {
    magic: [u8; 8],
    rows_used: u32,
    frames: u32,
    rows: [Row; 48],
}

static mut MEASURE: Measure = Measure {
    magic: *b"PSVGMEAS",
    rows_used: 0,
    frames: 0,
    rows: [Row {
        tag: [0; 8],
        triangles: 0,
        cycles: 0,
    }; 48],
};

/// Cycles of a timer read pair, subtracted from every row.
static mut OVERHEAD: u32 = 0;

struct Demo {
    frames: u32,
    cached_state: Option<HudState>,
    #[cfg(feature = "hud-sprites")]
    sprite_state: Option<hud_sprites::HudState>,
    ids: Option<HudIds>,
    gallery: Option<VectorAsset<'static>>,
    hud: Option<VectorAsset<'static>>,
}

#[inline(always)]
fn ticks() -> u16 {
    timers::counter(Timer::Timer2)
}

/// Record `cycles` for `tag` in the next row (or update the row of that tag).
fn record(tag: &str, triangles: u32, ticks8: u16) {
    // SAFETY: single-threaded guest; MEASURE is only touched here and in init.
    let m = unsafe { &mut MEASURE };
    let mut t = [0u8; 8];
    for (d, s) in t.iter_mut().zip(tag.bytes()) {
        *d = s;
    }
    let cycles = (ticks8 as u32 * 8).saturating_sub(unsafe { OVERHEAD });
    let used = m.rows_used as usize;
    let slot = m.rows[..used]
        .iter()
        .position(|r| r.tag == t)
        .unwrap_or(used);
    if slot < m.rows.len() {
        m.rows[slot] = Row {
            tag: t,
            triangles,
            cycles,
        };
        if slot == used {
            m.rows_used += 1;
        }
    }
}

impl Scene for Demo {
    fn init(&mut self, _ctx: &mut Ctx) {
        // Timer 2 on system clock / 8, free running.
        timers::set_mode(Timer::Timer2, 0x0200);
        let (a, b) = (ticks(), ticks());
        // SAFETY: single-threaded guest.
        unsafe { OVERHEAD = (b.wrapping_sub(a) as u32 * 8).min(64) };
        #[cfg(feature = "hud-sprites")]
        hud_packets::HudVram::SHOWCASE.upload();
        self.gallery = VectorAsset::parse(GALLERY).ok();
        self.hud = VectorAsset::parse(STANCE_HUD).ok();
        self.ids = self.hud.as_ref().and_then(HudIds::new);
    }

    fn update(&mut self, _ctx: &mut Ctx) {}

    fn render(&mut self, ctx: &mut Ctx) {
        // SAFETY: single-threaded guest; the table and cache are only used
        // here, and `submit` waits for the walk before anything is rebuilt.
        let (ot_table, cache, scratch) = unsafe { (&mut OT, &mut CACHE, &mut SCRATCH) };
        let mut ot = OtFrame::begin(ot_table);
        let frame = self.frames;
        self.frames += 1;
        if let (false, Some(ids), Some(asset)) =
            (GRID || SPRITES, self.ids.as_ref(), self.hud.as_ref())
        {
            // The HUD state flips every 48 frames; the packets are rebuilt
            // only then, and linked every frame.
            let state = if (frame / 48) % 2 == 0 {
                HudState {
                    active: Bar {
                        rgb: ZENITH,
                        cells: 9,
                    },
                    inactive: Bar {
                        rgb: HORIZON,
                        cells: 6,
                    },
                    energy: 4,
                    back_cells: BACK,
                }
            } else {
                HudState {
                    active: Bar {
                        rgb: HORIZON,
                        cells: 6,
                    },
                    inactive: Bar {
                        rgb: ZENITH,
                        cells: 9,
                    },
                    energy: 7,
                    back_cells: BACK,
                }
            };
            if self.cached_state != Some(state) {
                let t = ticks();
                cache.clear();
                draw_hud(ids, (6, 160), &state, &mut |g, p, s| {
                    let _ = cache.push(asset, g, p, s, scratch, 8);
                });
                self.cached_state = Some(state);
                record("c_build", cache.len() as u32, ticks().wrapping_sub(t));
            }
            // Every frame links the cached HUD except one in 64, which links
            // nothing, so the difference in the walk is the GPU time of the HUD.
            if frame % 64 != 63 {
                let t = ticks();
                for (slot, packet) in cache.links() {
                    let words = packet.words();
                    // SAFETY: the cache is a `static`, so the packets outlive
                    // the walk; nothing touches them until `submit` returns.
                    unsafe { ot.add_raw(slot, (packet as *mut VectorPacket).cast::<u32>(), words) };
                }
                record("c_link", cache.len() as u32, ticks().wrapping_sub(t));
            }
        }
        let mut walk = if frame % 64 != 63 {
            "walk_hud"
        } else {
            "walk_off"
        };
        #[cfg(feature = "hud-sprites")]
        {
            walk = self.sprite_hud(&mut ot, frame);
        }
        let t = ticks();
        ot.submit(ctx.gpu_dma());
        let dt = ticks().wrapping_sub(t);
        record(walk, 0, dt);
    }

    fn render_overlay(&mut self, ctx: &mut Ctx) {
        let gpu = ctx.gpu();
        // SAFETY: single-threaded guest; SCRATCH is only used from here.
        let scratch = unsafe { &mut SCRATCH };
        if GRID || ANIM {
            if !SPRITES {
                self.draw_grid(gpu, scratch);
            }
            return;
        }
        let (Some(gallery), Some(hud_asset)) = (self.gallery.as_ref(), self.hud.as_ref()) else {
            return;
        };

        // Baseline: two timer reads and nothing between.
        let t0 = ticks();
        record("empty", 0, ticks().wrapping_sub(t0));

        // The gallery, group by group at the authored position, offset (8, 6).
        let place = Placement::at(8, 6);
        let t_all = ticks();
        let mut total = 0u32;
        for id in gallery.groups() {
            let t = ticks();
            let n = gallery
                .draw_group(gpu, id, &place, &Style::DEFAULT, scratch)
                .unwrap_or(0);
            let dt = ticks().wrapping_sub(t);
            total += n;
            record(gallery.group_name(id), n, dt);
        }
        record("gallery", total, ticks().wrapping_sub(t_all));

        // Rotation and scale: the star at three poses on the right.
        if let Some(star) = gallery.group("star") {
            let t = ticks();
            let mut n = 0;
            for (i, (scale, angle)) in [(4096u16, 0u16), (3072, 256), (2048, 1024)]
                .iter()
                .enumerate()
            {
                let p = Placement::at(180 + 44 * i as i32, 6)
                    .scaled(*scale)
                    .rotated(*angle);
                n += gallery
                    .draw_group(gpu, star, &p, &Style::DEFAULT, scratch)
                    .unwrap_or(0);
            }
            record("star3", n, ticks().wrapping_sub(t));
        }

        // Clip: the blob through a window, tinted.
        if let Some(blob) = gallery.group("blob") {
            let clip = ClipRect {
                x0: 188,
                y0: 70,
                x1: 262,
                y1: 100,
            };
            let style = Style::tinted([255, 220, 160]).clipped(clip);
            let t = ticks();
            let n = gallery
                .draw_group(gpu, blob, &Placement::at(100, -4), &style, scratch)
                .unwrap_or(0);
            record("clip", n, ticks().wrapping_sub(t));
        }

        // Fade: the rects forced to average blend over the gradient.
        if let (Some(rects), Some(grad)) = (gallery.group("rects"), gallery.group("gradient")) {
            let p = Placement::at(268, 70);
            let _ = gallery.draw_group(gpu, grad, &p, &Style::DEFAULT, scratch);
            let fade = Style {
                blend: BlendOverride::Force(BlendMode::Average),
                ..Style::DEFAULT
            };
            let t = ticks();
            let n = gallery
                .draw_group(gpu, rects, &Placement::at(268, 66), &fade, scratch)
                .unwrap_or(0);
            record("fade", n, ticks().wrapping_sub(t));
        }

        // The HUD straight to the GPU in two states, and once more into a
        // sink that discards, to split CPU cost from GPU time.
        if let Some(ids) = self.ids.as_ref() {
            let state_a = HudState {
                active: Bar {
                    rgb: HORIZON,
                    cells: 6,
                },
                inactive: Bar {
                    rgb: ZENITH,
                    cells: 9,
                },
                energy: 6,
                back_cells: BACK,
            };
            let state_max = HudState {
                active: Bar {
                    rgb: HORIZON,
                    cells: 10,
                },
                inactive: Bar {
                    rgb: ZENITH,
                    cells: 10,
                },
                energy: 10,
                back_cells: BACK,
            };
            let t = ticks();
            let mut n = 0;
            draw_hud(ids, (6, 122), &state_a, &mut |g, p, s| {
                n += hud_asset.draw_group(gpu, g, p, s, scratch).unwrap_or(0);
            });
            record("hud_a", n, ticks().wrapping_sub(t));

            let t = ticks();
            let mut n = 0;
            draw_hud(ids, (6, 198), &state_max, &mut |g, p, s| {
                n += hud_asset.draw_group(gpu, g, p, s, scratch).unwrap_or(0);
            });
            record("hud_max", n, ticks().wrapping_sub(t));

            let t = ticks();
            let mut n = 0u32;
            draw_hud(ids, (6, 122), &state_a, &mut |g, p, s| {
                let mut count = 0u32;
                let _ = hud_asset.emit_group(g, p, s, scratch, &mut |e| {
                    count += match e {
                        Emit::Triangle(_) => 1,
                        Emit::Quad(_) => 2,
                        Emit::Blend(_) => 0,
                    };
                });
                n += count;
            });
            record("cpu_a", n, ticks().wrapping_sub(t));
        }

        // CPU-only microbenchmarks on the blob (36 triangles, 38 vertices): the
        // emit loop with a counting sink, varying one input at a time.
        if let Some(blob) = gallery.group("blob") {
            let around = ClipRect {
                x0: -1000,
                y0: -1000,
                x1: 1000,
                y1: 1000,
            };
            let part = ClipRect {
                x0: 100,
                y0: 70,
                x1: 130,
                y1: 100,
            };
            let rot = Placement::at(8, 6).rotated(300);
            let scl = Placement::at(8, 6).scaled(3000);
            let base = Placement::at(8, 6);
            let tint = Style::tinted([200, 160, 120]);
            let sat = Style {
                saturation: 100,
                ..Style::DEFAULT
            };
            let both = Style {
                saturation: 100,
                tint: [200, 160, 120],
                ..Style::DEFAULT
            };
            let cases: [(&str, &Placement, Style); 9] = [
                ("m_plain", &base, Style::DEFAULT),
                ("m_scale", &scl, Style::DEFAULT),
                ("m_rot", &rot, Style::DEFAULT),
                ("m_tint", &base, tint),
                ("m_sat", &base, sat),
                ("m_both", &base, both),
                ("m_clip0", &base, Style::DEFAULT.clipped(around)),
                ("m_clip1", &base, Style::DEFAULT.clipped(part)),
                ("m_cliptn", &base, tint.clipped(part)),
            ];
            // Eight calls each, averaged: the first call pays the instruction
            // cache refill, the rest show the steady cost a HUD sees.
            for (tag, place, style) in cases {
                let mut count = 0u32;
                let t = ticks();
                for _ in 0..8 {
                    count = 0;
                    let _ = gallery.emit_group(blob, place, &style, scratch, &mut |e| {
                        count += match e {
                            Emit::Triangle(_) => 1,
                            Emit::Quad(_) => 2,
                            Emit::Blend(_) => 0,
                        };
                    });
                }
                record(tag, count, ticks().wrapping_sub(t) / 8);
            }
        }
        // Fixed cost of one group draw: a one-quad group, 16 calls, counting sink.
        if let Some(gem) = hud_asset.group("energy_0") {
            for (tag, style) in [
                ("g_plain", Style::DEFAULT),
                ("g_tint", Style::tinted([200, 160, 120])),
            ] {
                let t = ticks();
                let mut count = 0u32;
                for _ in 0..16 {
                    let _ = hud_asset.emit_group(
                        gem,
                        &Placement::at(8, 6),
                        &style,
                        scratch,
                        &mut |e| {
                            count += matches!(e, Emit::Quad(_)) as u32;
                        },
                    );
                }
                record(tag, count, ticks().wrapping_sub(t) / 16);
            }
        }
        // Calibration, as in the Cortex proof: one 4-pixel triangle with the GPU
        // idle before and after.
        gpu.wait_idle();
        let tiny = psx_gpu::prim::TriGouraud::new([(0, 0), (4, 0), (0, 4)], [(1, 1, 1); 3]);
        let t = ticks();
        gpu.draw(&tiny);
        gpu.wait_idle();
        record("tiny", 1, ticks().wrapping_sub(t));
        // SAFETY: single-threaded guest.
        unsafe { MEASURE.frames += 1 };
    }
}

#[no_mangle]
fn main() -> ! {
    let mut demo = Demo {
        frames: 0,
        cached_state: None,
        #[cfg(feature = "hud-sprites")]
        sprite_state: None,
        ids: None,
        gallery: None,
        hud: None,
    };
    let config = Config {
        clear_color: CLEAR,
        ..Config::default()
    };
    App::run(config, &mut demo);
}

impl Demo {
    /// The sprite HUD: in the grid build eight HUDs built once and linked every frame; otherwise one
    /// HUD on a timeline of rests and swaps, with the link cycles recorded.
    #[cfg(feature = "hud-sprites")]
    fn sprite_hud(&mut self, ot: &mut OtFrame<'_, 16>, frame: u32) -> &'static str {
        use hud_packets::HudVram;
        use hud_sprites::{HudState as SpriteState, Stance};
        // SAFETY: single-threaded guest; the packets are only touched here, and `submit` waits for the
        // walk before anything is rebuilt.
        let huds = unsafe { &mut SPRITE_HUDS };
        if GRID {
            if SIZE_GRID != NO_SIZE {
                // SAFETY: as above.
                let alone = unsafe { &mut SIZE_HUDS };
                if frame == 0 {
                    for (i, hud) in alone.iter_mut().enumerate() {
                        let (col, row) = (i / 4, i % 4);
                        let stance = if col == 0 {
                            Stance::Horizon
                        } else {
                            Stance::Zenith
                        };
                        let origin = (160 * col as i32, 8 + 40 * row as i32);
                        let level = [10u8, 6, 3, 0][row];
                        hud.build(
                            &HudVram::SHOWCASE,
                            origin,
                            stance,
                            SIZE_GRID as usize,
                            level,
                        );
                    }
                }
                for hud in alone.iter_mut() {
                    // SAFETY: a `static`, unmodified until the walk ends.
                    hud.link(|ptr, words| unsafe { ot.add_raw(8, ptr, words) });
                }
                return "walk_hud";
            }
            for (i, hud) in huds.iter_mut().enumerate() {
                let (col, row) = (i / 4, i % 4);
                let state = if SWAP_GRID != 0 {
                    SpriteState {
                        stance: if SWAP_GRID == 1 {
                            Stance::Zenith
                        } else {
                            Stance::Horizon
                        },
                        health: [7, 5],
                        energy: [8, 3],
                        swap: [0u8, 2, 4, 6][row],
                    }
                } else {
                    let level = [10u8, 6, 3, 0][row];
                    let stance = if col == 0 {
                        Stance::Horizon
                    } else {
                        Stance::Zenith
                    };
                    let mut health = [4, 4];
                    health[col] = level;
                    SpriteState {
                        stance,
                        health,
                        energy: [level, level],
                        swap: 0,
                    }
                };
                if SWAP_GRID != 0 && col != 0 {
                    continue;
                }
                let origin = (GRID_X[col], 8 + 40 * row as i32);
                if frame == 0 {
                    let t = ticks();
                    hud.build(&HudVram::SHOWCASE, origin, state.health, state.energy);
                    if i == 0 {
                        record("c_build", 0, ticks().wrapping_sub(t));
                    }
                }
                // SAFETY: a `static`, unmodified until the walk ends.
                hud.link(&state, |ptr, tag| unsafe {
                    ot.add_raw_tag_unchecked(8, ptr, tag)
                });
            }
            return "walk_hud";
        }
        // A timeline of 64 game frames (the game runs at 30 frames a second, two screen frames each),
        // over and over: at rest on Horizon, a swap to Zenith, at rest on Zenith, a swap back. Frame
        // 63 links nothing, so that the walk without the HUD can be timed.
        let frame = frame / 2;
        let t = frame % 64;
        let (stance, swap) = match t {
            24..=30 => (Stance::Zenith, (t - 23) as u8),
            31..=55 => (Stance::Zenith, 0),
            56..=62 => (Stance::Horizon, (t - 55) as u8),
            _ => (Stance::Horizon, 0),
        };
        let state = SpriteState {
            stance,
            health: demo_health(t),
            energy: [8, 3],
            swap,
        };
        let origin = if ANIM { (8, 100) } else { (6, 160) };
        let hud = &mut huds[0];
        if self.sprite_state.is_none() {
            let t0 = ticks();
            hud.build(&HudVram::SHOWCASE, origin, state.health, state.energy);
            record("c_build", 0, ticks().wrapping_sub(t0));
            self.sprite_state = Some(state);
        }
        // Health changes by patching the cells' palettes, timed: a drain by one cell a frame (one
        // cell each), and a refill of a whole bar.
        let before = demo_health((t + 63) % 64);
        if state.health != before {
            let changed: u32 = state
                .health
                .iter()
                .zip(before)
                .map(|(a, b)| a.abs_diff(b) as u32)
                .sum();
            let t0 = ticks();
            hud.set_levels(&HudVram::SHOWCASE, state.health, state.energy);
            record(
                if changed == 1 { "c_pat1" } else { "c_pat10" },
                0,
                ticks().wrapping_sub(t0),
            );
        }
        if t != 63 {
            let t0 = ticks();
            // SAFETY: a `static`, unmodified until the walk ends.
            hud.link(&state, |ptr, tag| unsafe {
                ot.add_raw_tag_unchecked(8, ptr, tag)
            });
            if swap == 0 {
                record("c_link", 0, ticks().wrapping_sub(t0));
            } else if swap == 4 {
                record("sw_link", 0, ticks().wrapping_sub(t0));
            }
        }
        match (t, swap) {
            (63, _) => "walk_off",
            (_, 4) => "walk_sw",
            (_, 0) => "walk_hud",
            _ => "walk_mid",
        }
    }

    /// Two stance columns by four health/energy levels, drawn straight to the GPU at native
    /// resolution. The back bar shows four inactive cells where a variant draws them.
    fn draw_grid(&self, gpu: &mut psx_gpu::Gpu, scratch: &mut Scratch<SCRATCH_VERTICES>) {
        let (Some(ids), Some(asset)) = (self.ids.as_ref(), self.hud.as_ref()) else {
            return;
        };
        const LEVELS: [u8; 4] = [10, 6, 3, 0];
        for (col, (front, back)) in [(HORIZON, ZENITH), (ZENITH, HORIZON)].iter().enumerate() {
            for (row, &level) in LEVELS.iter().enumerate() {
                let state = HudState {
                    active: Bar {
                        rgb: *front,
                        cells: level,
                    },
                    inactive: Bar {
                        rgb: *back,
                        cells: 4,
                    },
                    energy: level,
                    back_cells: BACK,
                };
                let origin = (GRID_X[col], 8 + 40 * row as i32);
                draw_hud(ids, origin, &state, &mut |g, p, s| {
                    let _ = asset.draw_group(gpu, g, p, s, scratch);
                });
            }
        }
    }
}
