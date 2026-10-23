extern crate std;

use std::vec::Vec;

use psx_font::fonts::BASIC_8X16;

use super::*;

const INK: Rgb = Rgb(200, 120, 20);
const GROUND: Rgb = Rgb(5, 5, 7);

#[derive(Default)]
struct Record(Vec<Vec<u32>>);

impl Sink for Record {
    fn packet(&mut self, words: &[u32]) {
        self.0.push(words.to_vec());
    }
}

fn style() -> Style {
    Style::new(&BASIC_8X16, "Loading", INK, GROUND)
}

fn breathing() -> Style {
    style().with_breath(Rgb(100, 60, 10), Rgb(255, 180, 40))
}

fn spinning() -> Style {
    style()
        .with_tail(Tail::Spinner {
            lit: Rgb(255, 214, 96),
            unlit: Rgb(40, 34, 20),
        })
        .timed(2, 64)
}

#[test]
fn the_box_has_room_for_the_label_and_every_dot() {
    // (7 letters + 3 dots) cells of 8 pixels, 12 pixels of padding each side.
    let (x, y, w, h) = style().bounds();
    assert_eq!((w, h), (80 + 24, 16 + 8));
    assert_eq!((x, y), (160 - 104 / 2, 208));
}

#[test]
fn the_spinner_box_is_the_label_a_gap_and_the_grid() {
    // 56 + 8 + 18 of content, 12 either side: a 7-letter label with a ring.
    let (x, y, w, h) = spinning().bounds();
    assert_eq!((w, h), (82 + 24, 24));
    assert_eq!((x, y), (160 - 106 / 2, 208));
    let (_, _, none, _) = style().with_tail(Tail::None).bounds();
    assert_eq!(none, 56 + 24);
}

#[test]
fn scale_multiplies_the_content_not_the_padding() {
    let (_, _, w, h) = style().scaled(2).bounds();
    assert_eq!((w, h), (160 + 24, 32 + 8));
    assert_eq!(style().scaled(0).scale, 1);
}

#[test]
fn the_dots_step_and_wrap() {
    let s = style();
    let ticks = s.step_ticks as u32;
    let steps: Vec<u8> = (0..4).map(|n| s.look(n * ticks).step).collect();
    assert_eq!(steps, [0, 1, 2, 3]);
    assert_eq!(s.look(4 * ticks).step, 0);
    // Constant within a step.
    assert_eq!(s.look(ticks - 1).step, 0);
}

#[test]
fn the_ring_runs_its_twelve_lights_and_wraps() {
    let s = spinning();
    let steps: Vec<u8> = (0..13).map(|n| s.look(n * 2).step).collect();
    assert_eq!(steps, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0]);
}

#[test]
fn a_label_alone_never_steps() {
    let s = style().with_tail(Tail::None);
    assert!((0..500).all(|t| s.look(t).step == 0));
}

#[test]
fn the_ink_breathes_as_a_triangle_wave() {
    let s = breathing();
    let period = s.pulse();
    let levels: Vec<u8> = (0..period).map(|t| s.look(t).level).collect();
    assert_eq!(levels[0], 0);
    assert_eq!(levels[(period / 2) as usize], LEVELS);
    // Symmetric about the peak, and never jumps more than a level.
    for t in 1..period {
        assert_eq!(levels[t as usize], levels[(period - t) as usize]);
    }
    for pair in levels.windows(2) {
        assert!(pair[0].abs_diff(pair[1]) <= 1);
    }
    assert_eq!(s.look(period).level, 0, "the breath repeats");
}

#[test]
fn a_steady_ink_changes_the_picture_only_when_the_tail_steps() {
    let s = style();
    assert!((0..200).all(|t| s.look(t).level == 0));
    let mut indicator = Indicator::new(s);
    let mut sink = Record::default();
    let draws = (0..4 * s.step_ticks as u32)
        .filter(|&t| indicator.poll(&mut sink, t))
        .count();
    assert_eq!(draws, 4, "one draw per dot step and nothing between");
}

#[test]
fn the_ink_endpoints_are_the_two_colours() {
    let s = breathing();
    assert_eq!(s.ink(0), Rgb(100, 60, 10));
    assert_eq!(s.ink(LEVELS), Rgb(255, 180, 40));
    // And it can breathe downward too (bright channel below dim).
    let down = style().with_breath(Rgb(255, 0, 0), Rgb(0, 0, 0));
    assert_eq!(down.ink(LEVELS / 2), Rgb(128, 0, 0));
}

#[test]
fn a_poll_draws_once_per_change_and_never_twice_at_one_tick() {
    let mut indicator = Indicator::new(breathing());
    let mut sink = Record::default();
    // The ink rises half a level a tick: level 3 at ticks 6 and 7, 4 at tick 8.
    assert!(indicator.poll(&mut sink, 6));
    let after_first = sink.0.len();
    assert!(!indicator.poll(&mut sink, 6));
    assert!(!indicator.poll(&mut sink, 7));
    assert_eq!(sink.0.len(), after_first);
    assert!(indicator.poll(&mut sink, 8));
}

#[test]
fn begin_restarts_the_animation_and_redraws() {
    let mut indicator = Indicator::new(style());
    let mut sink = Record::default();
    indicator.begin(100);
    indicator.poll(&mut sink, 100);
    assert!(!indicator.poll(&mut sink, 101));
    indicator.begin(500);
    assert!(indicator.poll(&mut sink, 500));
    let drawn = sink.0.len();
    indicator.invalidate();
    assert!(indicator.poll(&mut sink, 500));
    assert!(sink.0.len() > drawn);
}

#[test]
fn the_tick_counter_may_wrap() {
    let mut indicator = Indicator::new(breathing());
    let mut sink = Record::default();
    indicator.begin(u32::MAX - 3);
    assert!(indicator.poll(&mut sink, u32::MAX - 3));
    // Four ticks later the counter has wrapped to 0: the ink is two levels up.
    assert!(indicator.poll(&mut sink, 0));
    assert_eq!(indicator.shown, Some(indicator.style().look(4)));
}

fn decode(packet: &[u32]) -> (i32, i32, i32, i32, u32) {
    assert_eq!(packet.len(), 3);
    assert!(matches!(packet[0] >> 24, 0x60 | 0x62));
    (
        (packet[1] & 0xffff) as i16 as i32,
        (packet[1] >> 16) as i16 as i32,
        (packet[2] & 0xffff) as i32,
        (packet[2] >> 16) as i32,
        packet[0] & 0x00ff_ffff,
    )
}

#[test]
fn a_draw_fills_the_box_first_then_the_rule_then_ink() {
    let s = style().with_rule(Rgb(24, 21, 16));
    let (bx, by, bw, bh) = s.bounds();
    let mut indicator = Indicator::new(s);
    let mut sink = Record::default();
    indicator.poll(&mut sink, 0);
    let first = decode(&sink.0[0]);
    assert_eq!(
        first,
        (bx as i32, by as i32, bw as i32, bh as i32, GROUND.word())
    );
    let second = decode(&sink.0[1]);
    assert_eq!(
        second,
        (bx as i32, by as i32, bw as i32, 1, Rgb(24, 21, 16).word())
    );
    for packet in &sink.0[2..] {
        assert_eq!(decode(packet).4, INK.word());
    }
}

#[test]
fn every_rectangle_stays_inside_the_box() {
    for scale in 1..=3 {
        for tail in [
            Tail::None,
            Tail::Dots(3),
            Tail::Spinner {
                lit: Rgb(255, 214, 96),
                unlit: Rgb(40, 34, 20),
            },
        ] {
            let s = breathing().scaled(scale).padded(6, 3).with_tail(tail);
            let (bx, by, bw, bh) = s.bounds();
            for step in 0..tail.steps() as u8 {
                let mut sink = Record::default();
                Indicator::new(s).draw(&mut sink, Look { step, level: 5 });
                for packet in sink.0.iter().filter(|p| p.len() == 3) {
                    let (x, y, w, h, _) = decode(packet);
                    assert!(w > 0 && h > 0);
                    assert!(
                        x >= bx as i32 && x + w <= (bx + bw) as i32,
                        "x {x}+{w} in box"
                    );
                    assert!(
                        y >= by as i32 && y + h <= (by + bh) as i32,
                        "y {y}+{h} in box"
                    );
                }
            }
        }
    }
}

#[test]
fn more_dots_draw_more_ink_and_the_label_stays_put() {
    let indicator = Indicator::new(style());
    let mut none = Record::default();
    let mut all = Record::default();
    indicator.draw(&mut none, Look { step: 0, level: 0 });
    indicator.draw(&mut all, Look { step: 3, level: 0 });
    assert!(all.0.len() > none.0.len());
    // Same prefix: the label's rectangles are identical.
    assert_eq!(all.0[..none.0.len()], none.0[..]);
}

/// The rectangles of a frame, without the draw-mode packets between them.
fn rects(sink: &Record) -> Vec<(i32, i32, i32, i32, u32)> {
    sink.0
        .iter()
        .filter(|p| p.len() == 3)
        .map(|p| decode(p))
        .collect()
}

#[test]
fn the_ring_is_twelve_dots_with_a_bright_head_and_a_fading_tail() {
    let s = corner().with_blend(Blend::Opaque);
    let lit = Rgb(255, 214, 96).word();
    let unlit = Rgb(40, 34, 20).word();
    let mut sink = Record::default();
    Indicator::new(s).draw(&mut sink, Look { step: 5, level: 0 });
    let all = rects(&sink);
    // The plate, twelve dots, five lights over them (the head and four behind it).
    assert_eq!(all.len(), 1 + RING_CELLS + TRAIL.len());
    let dots = &all[1..1 + RING_CELLS];
    assert!(dots.iter().all(|d| d.4 == unlit && d.2 == 3 && d.3 == 3));
    // Cell 5 is the head, at the full lit colour; the others behind it dim away.
    let lights = &all[1 + RING_CELLS..];
    assert_eq!(lights[0].4, lit);
    assert_eq!((lights[0].0, lights[0].1), (dots[5].0, dots[5].1));
    for (n, light) in lights.iter().enumerate() {
        assert_eq!((light.0, light.1), (dots[5 - n].0, dots[5 - n].1));
    }
    let strength = |c: u32| (c & 0xff) as i32;
    assert!(lights
        .windows(2)
        .all(|w| strength(w[0].4) >= strength(w[1].4)));
    // The ring: dots 5 apart in both directions, the head running clockwise.
    assert_eq!(dots[1].0 - dots[0].0, 5);
    assert_eq!(dots[RING_CELLS - 1].1 - dots[0].1, 5);
    let (bx, by, _, bh) = s.bounds();
    assert_eq!(dots[0].1, by as i32 + (bh as i32 - 18) / 2);
    let _ = bx;
}

#[test]
fn blended_lights_set_the_draw_mode_around_the_lights_only() {
    for (blend, mode) in [
        (Blend::Average, 0u32),
        (Blend::Additive, 1),
        (Blend::Glow, 3),
    ] {
        let mut sink = Record::default();
        Indicator::new(spinning().with_blend(blend)).draw(&mut sink, Look { step: 2, level: 0 });
        let packets = &sink.0;
        let on = packets
            .iter()
            .position(|p| p.len() == 1)
            .expect("a draw-mode packet before the lights");
        assert_eq!(packets[on], [0xe100_0000 | mode << 5]);
        // Everything before it is the opaque plate and dots (0x60), everything
        // between the two draw-mode packets is a semi-transparent light (0x62).
        assert!(packets[..on].iter().all(|p| p[0] >> 24 == 0x60));
        let off = packets.len() - 1;
        assert_eq!(packets[off], [0xe100_0000], "back to the plain draw mode");
        assert!(packets[on + 1..off].iter().all(|p| p[0] >> 24 == 0x62));
        assert_eq!(packets[on + 1..off].len(), TRAIL.len());
    }
    // Opaque lights need no draw-mode packets at all.
    let mut sink = Record::default();
    Indicator::new(spinning().with_blend(Blend::Opaque))
        .draw(&mut sink, Look { step: 2, level: 0 });
    assert!(sink.0.iter().all(|p| p.len() == 3 && p[0] >> 24 == 0x60));
}

/// The bounding box of the ring's dots (the rectangles after the plate) in pixels.
fn ring_extent(sink: &Record) -> (i32, i32, i32, i32) {
    let dots = &rects(sink)[1..1 + RING_CELLS];
    let left = dots.iter().map(|d| d.0).min().unwrap();
    let right = dots.iter().map(|d| d.0 + d.2).max().unwrap();
    let top = dots.iter().map(|d| d.1).min().unwrap();
    let bottom = dots.iter().map(|d| d.1 + d.3).max().unwrap();
    (left, top, right - left, bottom - top)
}

#[test]
fn the_ring_and_its_box_are_square_on_a_4_3_screen_in_every_mode() {
    // Width and height of a pixel on a 4:3 set, from the mode: (4/3) * height / width.
    for (w, h) in [
        (320, 240),
        (256, 240),
        (368, 240),
        (512, 240),
        (640, 240),
        (640, 480),
    ] {
        let style = Style::new(&BASIC_8X16, "", INK, Rgb(0, 0, 0))
            .with_tail(Tail::Spinner {
                lit: Rgb(255, 214, 96),
                unlit: Rgb(40, 34, 20),
            })
            .in_corner(24, w, h, 12);
        let par = 4.0 * h as f64 / (3.0 * w as f64);
        let (_, _, box_w, box_h) = style.bounds();
        assert_eq!(box_h, 24);
        assert!(
            (box_w as f64 * par - 24.0).abs() <= 0.5 * par.max(1.0) + 0.01,
            "{w}x{h}: box {box_w}x{box_h} px is {:.2} wide on screen",
            box_w as f64 * par
        );
        let mut sink = Record::default();
        Indicator::new(style).draw(&mut sink, Look { step: 3, level: 0 });
        let (_, _, ring_w, ring_h) = ring_extent(&sink);
        assert_eq!(ring_h, 18);
        assert!(
            (ring_w as f64 * par - ring_h as f64).abs() <= 0.5 * par.max(1.0) + 0.01,
            "{w}x{h}: ring {ring_w}x{ring_h} px is {:.2} wide on screen",
            ring_w as f64 * par
        );
        // The ring is centred in the box, and the box keeps equal margins to the
        // right edge and the bottom edge on the screen.
        let (bx, by, bw, bh) = style.bounds();
        let (rx, ry, rw, rh) = ring_extent(&sink);
        assert!(((rx - bx as i32) - (bx as i32 + bw as i32 - rx - rw)).abs() <= 1);
        assert!(((ry - by as i32) - (by as i32 + bh as i32 - ry - rh)).abs() <= 1);
        let right_margin = (w - (bx + bw)) as f64 * par;
        let bottom_margin = (h - (by + bh)) as f64;
        assert!(
            (right_margin - bottom_margin).abs() <= par.max(1.0),
            "{w}x{h}: margins {right_margin:.1} right, {bottom_margin} bottom"
        );
    }
}

#[test]
fn a_glyph_is_drawn_as_merged_runs() {
    // 'L' in the 8x16 VGA font is a vertical stem and a foot: a few tall
    // rectangles, not a rectangle per pixel row.
    let s = Style::new(&BASIC_8X16, "L", INK, GROUND);
    let mut sink = Record::default();
    Indicator::new(s).draw(&mut sink, Look { step: 0, level: 0 });
    let ink: Vec<_> = sink.0[1..].iter().map(|p| decode(p)).collect();
    assert!(
        ink.len() <= 8,
        "an L is {} rectangles, not one per pixel row",
        ink.len()
    );
    assert!(
        ink.iter().any(|&(_, _, _, h, _)| h >= 6),
        "the stem merges into tall rectangles"
    );
}

#[test]
fn prepare_resets_the_mask_and_draw_mode() {
    let mut sink = Record::default();
    prepare(&mut sink);
    assert_eq!(sink.0, [[0xe600_0000], [0xe100_0000]]);
}

#[test]
fn the_packet_words_are_pinned() {
    // One tiny indicator, drawn once: the words are the contract with the GPU.
    let s = Style::new(&BASIC_8X16, "I", Rgb(255, 255, 255), Rgb(0, 0, 0))
        .padded(0, 0)
        .at(8, 0);
    let mut sink = Record::default();
    Indicator::new(s).draw(&mut sink, Look { step: 0, level: 0 });
    // Box: 1 cell + 3 dots = 32 wide, 16 tall, centred on x = 8.
    let (bx, _, bw, bh) = s.bounds();
    assert_eq!((bx, bw, bh), (8 - 16, 32, 16));
    assert_eq!(
        sink.0[0],
        [0x6000_0000, bx as u16 as u32, (16u32 << 16) | 32]
    );
}

#[test]
fn a_proportional_font_is_laid_out_by_glyph_advance() {
    use psx_font::fonts::ORBITRON;
    let s = Style::new(&ORBITRON, "ILL", INK, GROUND)
        .with_tail(Tail::None)
        .padded(0, 0);
    let (_, _, w, _) = s.bounds();
    let want: u16 = "ILL"
        .chars()
        .map(|c| ORBITRON.glyph_advance(c) as u16)
        .sum();
    assert_eq!(w as u16, want);
    // Every rectangle still lands in the box.
    let (bx, _, bw, _) = s.bounds();
    let mut sink = Record::default();
    Indicator::new(s).draw(&mut sink, Look { step: 0, level: 0 });
    for packet in &sink.0 {
        let (x, _, rw, _, _) = decode(packet);
        assert!(x >= bx as i32 && x + rw <= (bx + bw) as i32);
    }
}

fn corner() -> Style {
    Style::new(&BASIC_8X16, "", INK, Rgb(0, 0, 0))
        .with_tail(Tail::Spinner {
            lit: Rgb(255, 214, 96),
            unlit: Rgb(40, 34, 20),
        })
        .in_corner(24, 320, 240, 12)
        .timed(2, 64)
}

#[test]
fn the_corner_widget_is_a_square_in_the_bottom_right() {
    assert_eq!(corner().bounds(), (284, 204, 24, 24));
    // The label-less ring is centred in it: 18 square.
    let mut sink = Record::default();
    Indicator::new(corner()).draw(&mut sink, Look { step: 0, level: 0 });
    let all = rects(&sink);
    assert_eq!(all[0], (284, 204, 24, 24, 0));
    assert_eq!(ring_extent(&sink), (284 + 3, 204 + 3, 18, 18));
}

#[test]
fn the_corner_widget_animates_without_text() {
    let s = corner();
    let mut indicator = Indicator::new(s);
    let mut sink = Record::default();
    // One spinner frame every two VBlanks, nothing in between.
    let draws = (0..20u32).filter(|&t| indicator.poll(&mut sink, t)).count();
    assert_eq!(draws, 10, "one frame every two VBlanks");
}

fn blocks(sink: &mut dyn Sink, x: i16, y: i16, frame: u8) {
    // Three 4x4 squares in a row; the lit one walks along them.
    for i in 0..3i16 {
        let colour = if i == frame as i16 {
            Rgb(255, 0, 0)
        } else {
            Rgb(60, 0, 0)
        };
        rect(sink, x + i * 6, y, 4, 4, colour);
    }
}

#[test]
fn a_game_can_supply_its_own_animation() {
    let s = Style::new(&BASIC_8X16, "", INK, Rgb(0, 0, 0))
        .with_tail(Tail::Art {
            width: 16,
            height: 4,
            frames: 3,
            draw: blocks,
        })
        .in_corner(24, 320, 240, 12)
        .timed(5, 64);
    assert_eq!(s.bounds(), (284, 204, 24, 24));
    assert_eq!(s.look(0).step, 0);
    assert_eq!(s.look(5).step, 1);
    assert_eq!(s.look(15).step, 0, "three frames, then it repeats");
    let mut sink = Record::default();
    Indicator::new(s).draw(&mut sink, Look { step: 1, level: 0 });
    let squares: Vec<_> = sink.0[1..].iter().map(|p| decode(p)).collect();
    assert_eq!(squares.len(), 3);
    // Centred: 16 wide in 24, 4 tall in 24.
    assert_eq!((squares[0].0, squares[0].1), (284 + 4, 204 + 10));
    assert_eq!(squares[1].4, Rgb(255, 0, 0).word());
    assert_eq!(squares[0].4, Rgb(60, 0, 0).word());
}
