//! Flow-transition overlay rendering.
//!
//! Free functions over [`FlowTransition`] that paint the full-screen
//! transition covers (fade, block dissolve, glitch break) on top of the
//! composed frame. [`render_transition_overlay`] is the single entry
//! point the game-app driver calls; everything else is a private
//! helper. Integer-only and alloc-free like the rest of the crate.

use psx_gpu::{draw_quad_flat, draw_tri_flat_blended, material::BlendMode};
use psx_level::LevelTransitionKind;

use crate::game_app::FlowTransition;

pub(crate) fn render_transition_overlay(transition: FlowTransition) {
    match transition.spec.kind {
        LevelTransitionKind::None => {}
        LevelTransitionKind::Fade => render_transition_fade(transition),
        LevelTransitionKind::BlockDissolve => render_transition_blocks(transition),
        LevelTransitionKind::GlitchBreak => render_transition_glitch(transition),
    }
}

fn transition_progress_q8(transition: FlowTransition) -> u16 {
    let frames = transition.frames().max(1);
    ((u32::from(transition.elapsed.min(frames)) * 256) / u32::from(frames)) as u16
}

fn transition_coverage_q8(transition: FlowTransition) -> u16 {
    let frames = transition.frames().max(1);
    let switch_frame = transition.switch_frame().min(frames);
    let elapsed = transition.elapsed.min(frames);
    if elapsed <= switch_frame {
        if switch_frame == 0 {
            256
        } else {
            ((u32::from(elapsed) * 256) / u32::from(switch_frame)) as u16
        }
    } else {
        let reveal_frames = frames.saturating_sub(switch_frame).max(1);
        let remaining = frames.saturating_sub(elapsed);
        ((u32::from(remaining) * 256) / u32::from(reveal_frames)) as u16
    }
}

fn transition_color(transition: FlowTransition) -> (u8, u8, u8) {
    (
        transition.spec.color[0],
        transition.spec.color[1],
        transition.spec.color[2],
    )
}

fn draw_fullscreen(color: (u8, u8, u8)) {
    draw_quad_flat(
        [(0, 0), (320, 0), (0, 240), (320, 240)],
        color.0,
        color.1,
        color.2,
    );
}

fn draw_fullscreen_average(color: (u8, u8, u8)) {
    draw_tri_flat_blended(
        [(0, 0), (320, 0), (0, 240)],
        color.0,
        color.1,
        color.2,
        BlendMode::Average,
    );
    draw_tri_flat_blended(
        [(320, 0), (0, 240), (320, 240)],
        color.0,
        color.1,
        color.2,
        BlendMode::Average,
    );
}

fn draw_rect(x: i16, y: i16, w: u16, h: u16, color: (u8, u8, u8)) {
    if w == 0 || h == 0 {
        return;
    }
    let x1 = x.saturating_add(w.min(i16::MAX as u16) as i16);
    let y1 = y.saturating_add(h.min(i16::MAX as u16) as i16);
    draw_quad_flat(
        [(x, y), (x1, y), (x, y1), (x1, y1)],
        color.0,
        color.1,
        color.2,
    );
}

fn render_transition_fade(transition: FlowTransition) {
    let progress = transition_coverage_q8(transition);
    let color = transition_color(transition);
    if progress >= 244 {
        draw_fullscreen(color);
        return;
    }
    let passes = match progress {
        0..=63 => 0,
        64..=127 => 1,
        128..=191 => 2,
        _ => 3,
    };
    let mut i = 0;
    while i < passes {
        draw_fullscreen_average(color);
        i += 1;
    }
}

fn render_transition_blocks(transition: FlowTransition) {
    let progress = transition_coverage_q8(transition);
    let color = transition_color(transition);
    if progress >= 252 {
        draw_fullscreen(color);
        return;
    }
    let mut cell = 0u16;
    while cell < 300 {
        let noise = transition_noise(transition.spec.seed, cell, 0) & 0xff;
        if noise < progress {
            let x = ((cell % 20) * 16) as i16;
            let y = ((cell / 20) * 16) as i16;
            draw_rect(x, y, 16, 16, color);
        }
        cell += 1;
    }
}

fn render_transition_glitch(transition: FlowTransition) {
    let progress = transition_progress_q8(transition);
    let base = transition_color(transition);
    if progress >= 252 {
        draw_fullscreen(base);
        return;
    }

    let frame_seed = transition.elapsed;
    let tear_count = 2u16.saturating_add(progress / 14);
    let mut i = 0u16;
    while i < tear_count {
        let n = transition_noise(transition.spec.seed, i, frame_seed);
        let y = (n % 240) as i16;
        let h = 1 + ((n >> 8) % 5);
        let color = glitch_color(n, base, progress);
        draw_rect(0, y, 320, h as u16, color);
        i += 1;
    }

    let block_count = (u32::from(progress) * 42 / 256) as u16;
    let mut block = 0u16;
    while block < block_count {
        let n = transition_noise(transition.spec.seed ^ 0x3519, block, frame_seed);
        let x = ((n % 40) * 8) as i16;
        let y = ((((n >> 6) % 30) * 8) as i16).min(232);
        let size = if n & 0x1000 != 0 { 16 } else { 8 };
        let color = glitch_color(n.rotate_left(3), base, progress);
        draw_rect(x, y, size, size, color);
        block += 1;
    }

    if progress > 120 {
        let takeover = (progress - 120).min(136);
        let mut cell = 0u16;
        while cell < 300 {
            let n = transition_noise(transition.spec.seed ^ 0x6a27, cell, frame_seed / 2) & 0xff;
            if n < takeover {
                let x = ((cell % 20) * 16) as i16;
                let y = ((cell / 20) * 16) as i16;
                draw_rect(x, y, 16, 16, base);
            }
            cell += 1;
        }
    }
}

fn glitch_color(noise: u16, base: (u8, u8, u8), progress: u16) -> (u8, u8, u8) {
    if progress > 170 || noise & 0x3 == 0 {
        return base;
    }
    match (noise >> 3) & 0x7 {
        0 => (255, 255, 255),
        1 => (160, 220, 255),
        2 => (255, 80, 160),
        3 => (80, 255, 160),
        4 => (255, 220, 80),
        _ => base,
    }
}

fn transition_noise(seed: u16, index: u16, frame: u16) -> u16 {
    let mut x = seed ^ index.wrapping_mul(7477) ^ frame.wrapping_mul(101) ^ index.rotate_left(5);
    x ^= x << 7;
    x ^= x >> 9;
    x = x.wrapping_mul(1093).wrapping_add(0x9e37);
    x ^ (x >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::LevelTransition;

    #[test]
    fn transition_coverage_covers_then_reveals() {
        let spec = LevelTransition {
            kind: LevelTransitionKind::Fade,
            frames: 10,
            color: [0, 0, 0],
            seed: 0,
        };
        let mut transition = FlowTransition::new(1, None, spec);
        transition.elapsed = 0;
        assert_eq!(transition_coverage_q8(transition), 0);
        transition.elapsed = 5;
        assert_eq!(transition_coverage_q8(transition), 256);
        transition.elapsed = 10;
        assert_eq!(transition_coverage_q8(transition), 0);
    }
}
