//! Feature `vector-hud-proof`: the stance HUD drawn from the cooked SVG
//! (`assets/hud/stance_hud.psvg`) instead of the rectangle HUD, from the real
//! stance, vitality and energy state.
//!
//! The HUD's packets are built when its state changes (a stance swap, a lit
//! cell or crystal gained or lost) and drawn from the cache every frame, so
//! the per-frame CPU cost is the GPU feed and not a transform. Cycles per
//! frame are kept in [`MEASURE`], a RAM block tagged `PSVGMEAS` that a headless
//! run finds in `--dump-ram`. Off by default; the default guest is unchanged.

use psx_game_runtime::vitality::VitalityChannelId;
use psx_gpu::Gpu;
use psx_io::timers::{self, Timer};
use psx_vector::{include_vector, PacketCache, Scratch, VectorAsset};

use crate::stance_hud_layout::{draw_hud, Bar, HudIds, HudState, SCRATCH_VERTICES};

static HUD_ASSET: &[u8] = include_vector!("../assets/hud/stance_hud.psvg");

/// Horizon and Zenith colours, as the rectangle HUD uses them.
const HORIZON: [u8; 3] = [234, 82, 48];
const ZENITH: [u8; 3] = [76, 202, 181];

/// Where the HUD's canvas sits on screen.
const ORIGIN: (i32, i32) = (6, 6);

/// One measured quantity: an 8-byte name, a count and cycles.
#[repr(C)]
#[derive(Clone, Copy)]
struct Row {
    tag: [u8; 8],
    count: u32,
    cycles: u32,
}

/// `PSVGMEAS`, then the rows used and the frames drawn, then the rows.
#[repr(C)]
struct Measure {
    magic: [u8; 8],
    rows_used: u32,
    frames: u32,
    rows: [Row; 8],
}

#[used]
static mut MEASURE: Measure = Measure {
    magic: *b"PSVGMEAS",
    rows_used: 0,
    frames: 0,
    rows: [Row {
        tag: [0; 8],
        count: 0,
        cycles: 0,
    }; 8],
};

struct VectorHud {
    asset: Option<VectorAsset<'static>>,
    ids: Option<HudIds>,
    cache: PacketCache<64>,
    scratch: Scratch<SCRATCH_VERTICES>,
    built_for: Option<HudState>,
    builds: u32,
    build_max: u32,
    draw_min: u32,
}

static mut HUD: VectorHud = VectorHud {
    asset: None,
    ids: None,
    cache: PacketCache::new(),
    scratch: Scratch::new(),
    built_for: None,
    builds: 0,
    build_max: 0,
    draw_min: u32::MAX,
};

fn record(slot: usize, tag: &[u8; 8], count: u32, cycles: u32) {
    // SAFETY: single-threaded guest; MEASURE is only written here.
    unsafe {
        let m = &mut *core::ptr::addr_of_mut!(MEASURE);
        m.rows[slot] = Row {
            tag: *tag,
            count,
            cycles,
        };
        m.rows_used = m.rows_used.max(slot as u32 + 1);
    }
}

fn channel_colour(channel: VitalityChannelId) -> [u8; 3] {
    if channel == VitalityChannelId::One {
        HORIZON
    } else {
        ZENITH
    }
}

/// Cells lit by a Q12 share: rounded to the nearest tenth, and at least one
/// while any health remains.
fn cells(share_q12: u16) -> u8 {
    let tenths = (u32::from(share_q12) * 10 + 2048) >> 12;
    let tenths = tenths.min(10) as u8;
    if tenths == 0 && share_q12 > 0 {
        1
    } else {
        tenths
    }
}

/// Draw the HUD for `active` with the two health shares (Q12) and `energy`
/// (0 to 100).
pub(crate) fn draw(
    gpu: &mut Gpu,
    active: VitalityChannelId,
    active_share_q12: u16,
    inactive_share_q12: u16,
    energy: u16,
) {
    // SAFETY: single-threaded guest; HUD is only touched from here.
    let hud = unsafe { &mut *core::ptr::addr_of_mut!(HUD) };
    if hud.asset.is_none() {
        hud.asset = VectorAsset::parse(HUD_ASSET).ok();
        hud.ids = hud.asset.as_ref().and_then(HudIds::new);
    }
    let (Some(asset), Some(ids)) = (hud.asset.as_ref(), hud.ids.as_ref()) else {
        return;
    };
    let state = HudState {
        active: Bar {
            rgb: channel_colour(active),
            cells: cells(active_share_q12),
        },
        inactive: Bar {
            rgb: channel_colour(active.other()),
            cells: cells(inactive_share_q12),
        },
        energy: (energy / 10).min(10) as u8,
        back_cells: false,
    };
    if hud.built_for != Some(state) {
        let t = timers::counter(Timer::Timer2);
        hud.cache.clear();
        let scratch = &mut hud.scratch;
        let cache = &mut hud.cache;
        draw_hud(ids, ORIGIN, &state, &mut |group, place, style| {
            let _ = cache.push(asset, group, place, style, scratch, 0);
        });
        hud.built_for = Some(state);
        let dt = u32::from(timers::counter(Timer::Timer2).wrapping_sub(t)) * 8;
        hud.builds += 1;
        hud.build_max = hud.build_max.max(dt);
        record(0, b"build\0\0\0", hud.builds, dt);
        record(1, b"buildmax", hud.builds, hud.build_max);
    }
    // Let the GPU finish the scene first, so the draw below is timed alone:
    // its cycles are the HUD's own CPU feed plus the GPU time it waits for.
    let t = timers::counter(Timer::Timer2);
    gpu.wait_idle();
    let idle = u32::from(timers::counter(Timer::Timer2).wrapping_sub(t)) * 8;
    let t = timers::counter(Timer::Timer2);
    hud.cache.draw(gpu);
    gpu.wait_idle();
    let dt = u32::from(timers::counter(Timer::Timer2).wrapping_sub(t)) * 8;
    hud.draw_min = hud.draw_min.min(dt);
    record(2, b"scene\0\0\0", 0, idle);
    // Calibration: the same cache again, then one 4-pixel triangle.
    let t = timers::counter(Timer::Timer2);
    hud.cache.draw(gpu);
    gpu.wait_idle();
    record(
        5,
        b"again\0\0\0",
        hud.cache.len() as u32,
        u32::from(timers::counter(Timer::Timer2).wrapping_sub(t)) * 8,
    );
    let tiny = psx_gpu::prim::TriGouraud::new([(0, 0), (4, 0), (0, 4)], [(1, 1, 1); 3]);
    let t = timers::counter(Timer::Timer2);
    gpu.draw(&tiny);
    gpu.wait_idle();
    record(
        6,
        b"tiny\0\0\0\0",
        1,
        u32::from(timers::counter(Timer::Timer2).wrapping_sub(t)) * 8,
    );
    record(3, b"draw\0\0\0\0", hud.cache.len() as u32, dt);
    record(4, b"drawmin\0", hud.cache.len() as u32, hud.draw_min);
    // SAFETY: single-threaded guest.
    unsafe { (*core::ptr::addr_of_mut!(MEASURE)).frames += 1 };
}
