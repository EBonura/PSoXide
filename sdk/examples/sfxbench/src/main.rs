//! `sfxbench` -- what the `psx-sfx` bookkeeping costs, in CPU cycles.
//!
//! Times the per-emitter placement chain (`space::gains`), the mixer's per-voice
//! level update, its idle tick and a stealing `play`, each over many calls
//! against a no-op hardware, and prints cycles per call on the TTY. Root counter
//! 2 runs at the system clock divided by eight and is read around each batch, so
//! the numbers are the guest's own and need no profiler. An empty batch with the
//! same input shuffling is measured first and subtracted.
//!
//! Run headless with `frontend launch --path sfxbench.exe --steps 400000000`
//! and read the `sfxbench:` lines. SPU register writes are not part of this:
//! the hardware is [`psx_sfx::hw::NullHw`], so the figures are the bookkeeping
//! alone and the real per-voice cost is these plus the SPU writes.
#![no_std]
#![no_main]

extern crate psx_rt;

use core::hint::black_box;
use core::ptr::{read_volatile, write_volatile};
use psx_rt::tty;
use psx_sfx::hw::NullHw;
use psx_sfx::mixer::{Class, Lane, Mixer, Play, Policy, Sound, SoundId};
use psx_sfx::space::{
    gains, Center, Curve, Emitter, Listener, ListenerRule, Listeners, PanLaw, Space,
};
use psx_sfx::Sample;
use psx_spu::SpuAddr;

/// Calls per batch. 128 calls of at most a few thousand cycles stay well inside
/// the 16-bit counter (524,288 cycles).
const BATCH: u32 = 128;

/// Batches averaged per figure.
const REPS: u32 = 8;

const T2_COUNTER: *const u32 = (psx_hw::timers::block(2) + psx_hw::timers::COUNTER) as *const u32;
const T2_MODE: *mut u32 = (psx_hw::timers::block(2) + psx_hw::timers::MODE) as *mut u32;
const T2_SYSCLK_DIV8: u32 = 1 << 9;

fn now() -> u32 {
    // SAFETY: a read of root counter 2's current-value register.
    unsafe { read_volatile(T2_COUNTER) & 0xFFFF }
}

/// Counter ticks (8 CPU cycles each) for `REPS` batches of `BATCH` calls of
/// `body(i)`.
fn ticks(mut body: impl FnMut(u32)) -> u32 {
    let mut total = 0u32;
    for _ in 0..REPS {
        let start = now();
        for i in 0..BATCH {
            body(i);
        }
        total += now().wrapping_sub(start) & 0xFFFF;
    }
    total
}

fn print_dec(mut v: u32) {
    let mut buf = [0u8; 10];
    let mut n = 0;
    if v == 0 {
        buf[0] = b'0';
        n = 1;
    }
    while v > 0 {
        buf[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    let mut out = [0u8; 10];
    for i in 0..n {
        out[i] = buf[n - 1 - i];
    }
    // SAFETY: only ASCII digits were written.
    tty::print(unsafe { core::str::from_utf8_unchecked(&out[..n]) });
}

/// Print cycles per call: (total - baseline) * 8 / (REPS * BATCH).
fn report(label: &str, total_ticks: u32, base_ticks: u32) {
    let cycles = total_ticks.saturating_sub(base_ticks) * 8 / (REPS * BATCH);
    tty::print("sfxbench: ");
    tty::print(label);
    tty::print(" cycles_per_call=");
    print_dec(cycles);
    tty::print("\n");
}

/// Sixteen emitter positions from near to far, so the isqrt and the curve see a
/// spread of magnitudes rather than one constant.
const POS: [[i32; 3]; 16] = [
    [120, 10, 300],
    [-800, 40, 1500],
    [2500, -60, -900],
    [90, 0, 60],
    [-3000, 200, 4000],
    [700, -20, -2200],
    [15, 5, 15],
    [5200, 0, 100],
    [-100, 90, -4800],
    [1800, 300, 1200],
    [-2600, -100, -50],
    [60, 0, 3400],
    [4100, 10, 4100],
    [-700, 0, 700],
    [330, 20, -330],
    [9000, 0, -9000],
];

fn sound(lane: u8, class: Class, looped: bool) -> Sound {
    let s = Sample::resident(SpuAddr::new(0x1010), 22050, 400);
    if looped {
        Sound::looped(SoundId(1), s, lane).with_class(class)
    } else {
        Sound::one_shot(SoundId(1), s, lane).with_class(class)
    }
}

#[no_mangle]
fn main() {
    // SAFETY: counter 2's mode register; nothing else uses the counter here.
    unsafe { write_volatile(T2_MODE, T2_SYSCLK_DIV8) };

    let curve = Curve::linear_clip(0x3FFF, 6000);
    let one = Listeners::new([Listener::new([0, 0, 0], 300)]);
    let two = Listeners::new([
        Listener::new([0, 0, 0], 300),
        Listener::new([1200, 0, 900], 3900),
    ]);
    let equal = Space::new(0, PanLaw::EqualPower(Center::Unity));
    let sum = Space::new(0, PanLaw::Sum);
    let off = Space::new(0, PanLaw::Off);
    let culled = Space::new(0, PanLaw::Off).with_cull(0x3000);

    // Baseline: the same index shuffling and a black_box, no work.
    let mut sink = 0i32;
    let base = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        sink = sink.wrapping_add(p[0]);
    });
    black_box(sink);

    let mut acc = 0i32;
    let mut run = |label: &str, space: &Space, rule: ListenerRule, two_ears: bool| {
        let t = ticks(|i| {
            let e = Emitter::new(black_box(POS[(i & 15) as usize]));
            let g = if two_ears {
                gains(space, &two, rule, &e, &curve)
            } else {
                gains(space, &one, rule, &e, &curve)
            };
            if let Some(g) = g {
                acc = acc.wrapping_add(g.left as i32 + g.right as i32);
            }
        });
        report(label, t, base);
    };
    run("gains 1 listener, EqualPower", &equal, ListenerRule::Loudest, false);
    run("gains 1 listener, Sum", &sum, ListenerRule::Loudest, false);
    run("gains 1 listener, Off (no trig, no divide)", &off, ListenerRule::Loudest, false);
    run("gains 2 listeners Loudest, EqualPower", &equal, ListenerRule::Loudest, true);
    run("gains 2 listeners Nearest, EqualPower", &equal, ListenerRule::Nearest, true);
    run("gains 2 listeners Centroid, EqualPower", &equal, ListenerRule::Centroid, true);
    run("gains 1 listener, culled by curve", &culled, ListenerRule::Loudest, false);
    black_box(acc);

    // The pieces the placement chain is built from, for attribution.
    let mut acc2 = 0i32;
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(psx_math::int32::isqrt_i32(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]));
    });
    report("  isqrt_i32 of a 3D squared length", t, base);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(psx_math::int32::isqrt_u64((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]) as u64) as i32);
    });
    report("  isqrt_u64 of the same", t, base);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(psx_math::sin_q12(p[0] as u16) + psx_math::cos_q12(p[0] as u16));
    });
    report("  sin_q12 + cos_q12", t, base);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(black_box(p[0] * 3000) / black_box(p[2] | 1));
    });
    report("  one 32-bit divide plus a multiply", t, base);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(curve.level(black_box(p[0] + p[2])) as i32);
    });
    report("  Curve::level (Ramp)", t, base);
    let hyper = Curve::inverse(0x3FFF, 800);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        acc2 = acc2.wrapping_add(hyper.level(black_box(p[0] + p[2])) as i32);
    });
    report("  Curve::level (Hyper)", t, base);
    let t = ticks(|i| {
        let p = black_box(POS[(i & 15) as usize]);
        let q = black_box([5, 6, 7]);
        let d = [p[0].wrapping_sub(q[0]) >> 1, p[1].wrapping_sub(q[1]) >> 1, p[2].wrapping_sub(q[2]) >> 1];
        let d = d.map(|v| v.clamp(-16383, 16383));
        acc2 = acc2.wrapping_add(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
    });
    report("  three axis offsets, clamps and squares", t, base);
    black_box(acc2);

    // The mixer, over a no-op chip.
    let mut hw = NullHw;
    let mut mixer = Mixer::new(
        [
            Lane::new(0x00_00FF, Policy::Smart),
            Lane::new(0xFF_FF00, Policy::FirstFree),
        ],
        60,
    );
    let engine = sound(0, Class::Normal, true);
    let mut handles = [None; 8];
    for (i, h) in handles.iter_mut().enumerate() {
        *h = mixer.play(&mut hw, &engine, Play::new(1000 + i as i16, 1000));
    }

    let t = ticks(|i| {
        let l = black_box(500 + (i & 31) as i16 * 40);
        if let Some(h) = handles[(i & 7) as usize] {
            mixer.update(&mut hw, h, l, 1000 - l / 2);
        }
    });
    report("Mixer::update (levels changed)", t, base);

    let mut clock = 0u32;
    let t = ticks(|_| {
        clock = clock.wrapping_add(1);
        mixer.tick(&mut hw, black_box(clock));
    });
    report("Mixer::tick (loops only, nothing due)", t, base);

    // A full lane of low-priority one-shots, then a more important one steals.
    let mut shots = Mixer::new([Lane::new(0x00_00FF, Policy::Smart)], 60);
    let low = sound(0, Class::Low, false);
    for _ in 0..8 {
        shots.play(&mut hw, &low, Play::new(500, 500).with_ticks(1000));
    }
    let high = sound(0, Class::High, false);
    let t = ticks(|i| {
        shots.play(
            &mut hw,
            &high,
            Play::new(black_box(2000 + i as i16), 2000).with_ticks(1000),
        );
    });
    report("Mixer::play (Smart, lane full, steals)", t, base);

    // The whole per-emitter update a game does for a held loop: place it, then
    // push the levels to its voice.
    let t = ticks(|i| {
        let e = Emitter::new(black_box(POS[(i & 15) as usize]));
        if let (Some(g), Some(h)) = (
            gains(&equal, &one, ListenerRule::Loudest, &e, &curve),
            handles[(i & 7) as usize],
        ) {
            mixer.update(&mut hw, h, g.left, g.right);
        }
    });
    report("gains + Mixer::update (full emitter update)", t, base);

    tty::println("sfxbench: done");
    loop {}
}
