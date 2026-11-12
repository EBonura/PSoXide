//! Packets of repeating and plain quads, on the host GTE: a `REPEAT` quad
//! carries its material's window with raw UVs on the straight path and as a
//! windowed packet on the near-plane path, a plain quad adds the texture
//! origin and resets the window.

use super::*;
use crate::render::projection::{load_view_rotation, ViewProjection};
use crate::view::RoomView;
use crate::write::{aligned_copy, words_as_bytes, RoomChunk};
use alloc::vec;
use alloc::vec::Vec;
use psx_gte::math::{Mat3I16, Vec3I32 as GteVec3I32};

const WINDOW: u32 = 0xE200_0000 | 0b11000 | (0b11000 << 5) | (16 << 10) | (8 << 15);
const ORIGIN: u16 = 0x4080;

fn material() -> GridMaterial {
    GridMaterial {
        tpage: 0x1234,
        clut: 0x5678,
        uv_origin: ORIGIN,
        command_bits: 0,
        resident: true,
        window_word: WINDOW,
        scroll: 0,
    }
}

/// One cell with a wall on the plane `x = 80`, facing -X: a plain quad
/// first, then a repeating one with the given raw UV corner `far`.
fn chunk(far: u8) -> Vec<u8> {
    chunk_with(far, 0)
}

/// [`chunk`] with the wall turned: its far end (`z = 80`) is `slant` units
/// farther from the eye along X than its near end, and it shows both faces.
fn chunk_with(far: u8, slant: i16) -> Vec<u8> {
    chunk_in_range(far, slant, (0, 80))
}

/// [`chunk_with`] whose cell claims the vertical extent `range`, as the cook
/// would record it for geometry up there.
fn chunk_in_range(far: u8, slant: i16, range: (i16, i16)) -> Vec<u8> {
    let wall = |first: usize| {
        let mut quad = Quad {
            v: [0, 1, 2, 3],
            color: [0x00_80_80_80; 4],
            d: -80,
            n: [-127, 0, 0],
            material: 0,
            flags: if slant == 0 {
                QUAD_AXIS | QUAD_AXIS_NEG | (0 << QUAD_AXIS_SHIFT)
            } else {
                QUAD_DOUBLE
            },
            ..Quad::default()
        };
        if first == 1 {
            quad.flags2 = QUAD2_REPEAT;
            quad.uv = [
                0,
                u16::from(far),
                u16::from(far) | (u16::from(far) << 8),
                u16::from(far) << 8,
            ];
        } else {
            quad.uv = [0, 63, 63 | (63 << 8), 63 << 8];
        }
        quad
    };
    let vtx = |z: i16, y: i16| Vtx {
        x: 80 + slant * z / 80,
        y,
        z,
        pad: 0,
    };
    RoomChunk {
        width: 1,
        depth: 1,
        min_y: 0,
        max_y: 80,
        sectors: vec![Sector {
            flags: SECTOR_HAS_FLOOR,
            ext: NO_EXT,
            ..Sector::default()
        }],
        vertices: vec![vtx(0, 0), vtx(80, 0), vtx(80, 80), vtx(0, 80)],
        quads: vec![wall(0), wall(1)],
        cells: vec![RCell {
            first: 0,
            count: 2,
            flags: 0,
            min_y: range.0,
            max_y: range.1,
        }],
        material_use: vec![0],
        ..RoomChunk::default()
    }
    .to_bytes()
}

/// Draw the chunk from `eye_x` (room-local, looking along +X) and return
/// the packet words.
fn draw(far: u8, eye_x: i32) -> (GridFrame, Vec<u32>) {
    draw_from(far, [eye_x, 40, 40])
}

/// [`draw`] from an eye anywhere.
fn draw_from(far: u8, eye: [i32; 3]) -> (GridFrame, Vec<u32>) {
    let (frame, out, _) = draw_screens(chunk(far), eye);
    (frame, out)
}

/// [`draw_from`] that also returns the corner cache (`sxy`, `sz`).
fn draw_screens(chunk: Vec<u8>, eye: [i32; 3]) -> (GridFrame, Vec<u32>, Vec<(u32, u16)>) {
    draw_screens_with(chunk, eye, 0)
}

/// [`draw_screens`] with the level's visual switches at `switches`.
fn draw_screens_with(
    chunk: Vec<u8>,
    eye: [i32; 3],
    switches: u32,
) -> (GridFrame, Vec<u32>, Vec<(u32, u16)>) {
    draw_chunk(chunk, eye, switches, material())
}

/// [`draw_screens_with`] through the given material binding.
fn draw_chunk(
    chunk: Vec<u8>,
    eye: [i32; 3],
    switches: u32,
    material: GridMaterial,
) -> (GridFrame, Vec<u32>, Vec<(u32, u16)>) {
    let words = aligned_copy(&chunk);
    let bytes = words_as_bytes(&words, words.len() * 4);
    let room = RoomView::parse(bytes).expect("room parses");
    room.validate_full(1).expect("room validates");
    psx_gte::host::reset();
    scene::set_screen_offset(160 << 16, 120 << 16);
    scene::set_projection_plane(160);
    scene::set_average_z_weights(0x155, 0x100);
    let view = load_view_rotation(
        GteVec3I32::new(eye[0] << 12, eye[1] << 12, eye[2] << 12),
        Mat3I16::IDENTITY,
    );
    let projection = ViewProjection {
        focal_length: 160,
        ..ViewProjection::DEFAULT
    };
    let frustum = FrustumPlanes::from_view(&view.rotation, view.translation, projection);
    let camera = RoomCamera {
        frustum: &frustum,
        view: &view,
        camera: eye,
        screen: [320, 240],
        focal_length: 160,
        sector: 80,
        switches,
    };
    let mut renderer = GridRenderer::for_rooms(core::iter::once(&room));
    let mut out = vec![0u32; 4096];
    let frame = renderer.draw_room(&room, &camera, &[material], &mut out, false);
    out.truncate(frame.packet_words);
    let screens = renderer.screen.iter().map(|s| (s.sxy, s.sz)).collect();
    (frame, out, screens)
}

/// Packets of a stream as `(tag, data words)`; `tag >> 24` is the data count.
fn packets(stream: &[u32]) -> Vec<&[u32]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let size = (stream[at] >> 24) as usize;
        out.push(&stream[at..at + 1 + size]);
        at += 1 + size;
    }
    out
}

#[test]
fn a_repeat_quad_carries_the_window_and_raw_uvs_and_a_plain_quad_resets_it() {
    let (frame, stream) = draw(255, -120);
    assert_eq!(frame.packets, 2, "{frame:?}");
    let packets = packets(&stream);
    assert_eq!(packets.len(), 2);
    let reset = TextureWindow::NONE.word();
    // Both quads sit on one plane and one depth; take them by their window.
    let plain = packets
        .iter()
        .find(|p| p[1] == reset)
        .expect("a plain packet");
    let repeat = packets
        .iter()
        .find(|p| p[1] == WINDOW)
        .expect("a repeating packet");
    // Words 4, 7, 10, 13 hold the UVs (corner 1, 0, 2, 3 order) low half.
    let uv = |p: &[u32], at: usize| (p[at] & 0xffff) as u16;
    // Plain: origin added to 63 (u) and 63 | 63 << 8.
    assert_eq!(uv(plain, 4), 63u16.wrapping_add(ORIGIN));
    assert_eq!(uv(plain, 7), 0u16.wrapping_add(ORIGIN));
    // Repeating: raw, nothing added.
    assert_eq!(uv(repeat, 4), 255);
    assert_eq!(uv(repeat, 7), 0);
    assert_eq!(uv(repeat, 10), 255 | (255 << 8));
    assert_eq!(uv(repeat, 13), 255 << 8);
    // Same clut and tpage words either way.
    assert_eq!(repeat[4] >> 16, 0x5678);
    assert_eq!(repeat[7] >> 16, 0x1234);
    assert_eq!(repeat.len(), plain.len(), "no packet grows");
}

#[test]
fn a_repeat_quad_on_a_material_without_a_window_is_skipped_not_drawn_wrong() {
    let words = aligned_copy(&chunk(255));
    let bytes = words_as_bytes(&words, words.len() * 4);
    let room = RoomView::parse(bytes).unwrap();
    assert!(GridMaterial::default()
        .addressing(&room.render.quads[1], 0)
        .is_none());
    let plain = GridMaterial::default().addressing(&room.render.quads[0], 7);
    assert_eq!(plain, Some((0, 7)));
}

#[test]
fn near_plane_repeat_quads_take_the_windowed_packet_shape() {
    // The wall is 10 units ahead of the eye, inside the zone the straight
    // path cannot project exactly, so the quads take the shared path.
    let (frame, stream) = draw(255, 70);
    assert!(frame.robust_quads >= 1, "{frame:?}");
    let packets = packets(&stream);
    let reset = TextureWindow::NONE.word();
    let windowed = |p: &[u32]| p.contains(&WINDOW);
    let repeating: Vec<_> = packets.iter().filter(|p| windowed(p)).collect();
    assert!(!repeating.is_empty(), "no packet carries the window");
    for p in &repeating {
        // The windowed shape: window word in front, reset behind.
        assert_eq!(p[1], WINDOW);
        assert_eq!(*p.last().unwrap(), reset);
    }
    // No plain packet carries the window.
    assert!(packets.iter().any(|p| !windowed(p)));
}

#[test]
fn a_quad_seen_head_on_is_not_cut() {
    let (frame, _) = draw(255, -120);
    assert_eq!(frame.packets, 2, "{frame:?}");
}

#[test]
fn a_quad_that_runs_away_from_the_eye_is_cut_where_it_bends() {
    // A wall 80 wide whose far end is 80 units farther than its near end,
    // seen from close by so that it runs off the screen: the texture would
    // bend visibly.
    let (flat, _, _) = draw_screens(chunk(255), [40, 40, 40]);
    let (bent, stream, screens) = draw_screens(chunk_with(255, 80), [40, 40, 40]);
    let off = |(sxy, _): &(u32, u16)| {
        let (x, y) = (*sxy as u16 as i16, (*sxy >> 16) as u16 as i16);
        !(0..320).contains(&x) || !(0..240).contains(&y)
    };
    assert!(screens.iter().take(4).any(off), "{screens:x?}");
    assert!(bent.packets > flat.packets, "{bent:?} {flat:?}");
    // Every piece is a complete packet and none runs off the stream.
    let all = packets(&stream);
    assert_eq!(all.iter().map(|p| p.len()).sum::<usize>(), stream.len());
    assert_eq!(all.len() as u32, bent.packets);
}

#[test]
fn only_a_repeating_quad_is_cut_a_plain_one_bends_whole() {
    // The slanted wall holds a plain quad and a repeating one on the same
    // corners. The repeating one spans four tiles, so its texture bends
    // four times as far in texels and it alone is tested.
    let (_, stream, _) = draw_screens(chunk_with(255, 80), [40, 40, 40]);
    let all = packets(&stream);
    let plain = all.iter().filter(|p| p[1] != WINDOW).count();
    let repeating = all.len() - plain;
    assert_eq!(plain, 1, "the plain quad is one packet");
    assert!(repeating >= 2, "the repeating quad is cut: {repeating}");
}

#[test]
fn a_cell_wholly_above_or_below_the_view_is_not_queued() {
    // The eye looks along +X from 40 units up; the frustum there is a few
    // hundred units tall.
    let eye = [-120, 40, 40];
    let (seen, stream, _) = draw_screens(chunk_in_range(63, 0, (0, 80)), eye);
    assert_eq!(seen.cells_visible, 1, "{seen:?}");
    assert!(seen.packets > 0);
    for range in [(9_000, 9_080), (-9_080, -9_000)] {
        let (frame, _, _) = draw_screens(chunk_in_range(63, 0, range), eye);
        assert_eq!(frame.cells_visible, 0, "{range:?}: {frame:?}");
        assert_eq!(frame.packets, 0, "{range:?}");
    }
    // A cell that merely reaches into the view stays.
    let (reaching, again, _) = draw_screens(chunk_in_range(63, 0, (-9_000, 40)), eye);
    assert_eq!(reaching.cells_visible, 1, "{reaching:?}");
    assert_eq!(
        again, stream,
        "the recorded range does not change the packets"
    );
}

/// [`chunk`] with its two quads moved out of the cell into one free plane
/// whose run carries `flags`.
fn plane_chunk(flags: u8) -> Vec<u8> {
    let words = aligned_copy(&chunk(255));
    let bytes = words_as_bytes(&words, words.len() * 4);
    let room = RoomView::parse(bytes).expect("room parses");
    RoomChunk {
        width: 1,
        depth: 1,
        min_y: 0,
        max_y: 80,
        sectors: room.col.sectors.to_vec(),
        vertices: room.render.vertices.to_vec(),
        quads: room.render.quads.to_vec(),
        cells: vec![RCell {
            first: 0,
            count: 0,
            flags: 0,
            min_y: 0,
            max_y: 0,
        }],
        plane_refs: vec![PlaneRef {
            quad: 0,
            count: 2,
            flags,
            x0: 0,
            z0: 0,
            x1: 0,
            z1: 0,
            min_y: 0,
            max_y: 80,
        }],
        material_use: vec![0],
        ..RoomChunk::default()
    }
    .to_bytes()
}

#[test]
fn a_switched_plane_is_drawn_only_while_its_slot_says_so() {
    let eye = [-120, 40, 40];
    let packets = |flags: u8, switches: u32| {
        draw_screens_with(plane_chunk(flags), eye, switches)
            .0
            .packets
    };
    assert_eq!(packets(0, 0), 2, "an unswitched plane always draws");
    let on = visual_switch::encode(5, false);
    assert_eq!(packets(on, 0), 0, "slot 5 is off, so the plane is hidden");
    assert_eq!(
        packets(on, 1 << 4),
        0,
        "another slot being on changes nothing"
    );
    assert_eq!(packets(on, 1 << 5), 2, "slot 5 on shows it");
    let off = visual_switch::encode(5, true);
    assert_eq!(
        packets(off, 0),
        2,
        "an inverted plane shows while the slot is off"
    );
    assert_eq!(packets(off, 1 << 5), 0);
}

/// The packet of the repeating quad when the material scrolls by `offset`
/// (raw, as the level's UV motion gives it), and the plain quad's packet.
fn scrolled(far: u8, offset: [u8; 2], eye: [i32; 3]) -> (Vec<u32>, Vec<u32>) {
    let mut bound = material();
    bound.set_scroll(offset);
    let (frame, stream, _) = draw_chunk(chunk(far), eye, 0, bound);
    assert_eq!(frame.packets, 2, "{frame:?}");
    let packets = packets(&stream);
    let repeat = packets.iter().find(|p| p[1] == WINDOW).expect("repeat");
    let plain = packets
        .iter()
        .find(|p| p[1] == TextureWindow::NONE.word())
        .expect("plain");
    (repeat.to_vec(), plain.to_vec())
}

/// Corner UVs of a quad packet as `(u, v)`, in the packet's 1, 0, 2, 3 order.
fn corner_uvs(p: &[u32]) -> [(u8, u8); 4] {
    [4, 7, 10, 13].map(|at| (p[at] as u8, (p[at] >> 8) as u8))
}

#[test]
fn set_scroll_reduces_the_offset_to_the_materials_tile() {
    let mut m = material();
    m.set_scroll([200, 70]);
    assert_eq!(m.scroll, (200 % 64) | ((70 % 64) << 8));
    m.set_scroll([64, 128]);
    assert_eq!(m.scroll, 0);
    m.window_word = 0;
    m.set_scroll([5, 5]);
    assert_eq!(m.scroll, 0, "a material with no window stays still");
    // A 128 by 32 window: masks 0b10000 (x) and 0b11100 (y).
    m.window_word = 0xE200_0000 | 0b10000 | (0b11100 << 5);
    m.set_scroll([255, 255]);
    assert_eq!(m.scroll, 127 | (31 << 8));
}

#[test]
fn a_scrolling_repeat_quad_moves_with_the_offset_and_wraps_with_the_tile() {
    // Three tiles of a 64 texel texture: the widest a scrolling material is
    // cooked to, so every offset fits.
    let far = 191;
    let eye = [-120, 40, 40];
    let (still, still_plain) = scrolled(far, [0, 0], eye);
    assert_eq!(corner_uvs(&still), [(far, 0), (0, 0), (far, far), (0, far)]);
    let mut lows = Vec::new();
    for offset in 0..=255u8 {
        let (repeat, plain) = scrolled(far, [offset, 0], eye);
        let uv = corner_uvs(&repeat);
        // Corner 0 holds the lowest u: it sits at the offset modulo the tile.
        assert_eq!(uv[1].0, offset % 64, "offset {offset}");
        // The span is kept, whole, inside the byte.
        assert_eq!(uv[0].0 - uv[1].0, far, "offset {offset}");
        // The other axis does not move.
        assert_eq!(uv[1].1, 0);
        assert_eq!(uv[3].1, far);
        // A plain quad never scrolls.
        assert_eq!(plain, still_plain);
        lows.push(uv[1].0);
    }
    // One full tile later the picture is where it began.
    assert_eq!(lows[64], lows[0]);
    assert_eq!(lows[10], 10);
    // Both axes at once.
    let (both, _) = scrolled(far, [3, 7], eye);
    let uv = corner_uvs(&both);
    assert_eq!((uv[1].0, uv[1].1), (3, 7));
    assert_eq!(uv[2], (3 + far, 7 + far));
}

#[test]
fn a_repeat_quad_too_wide_to_shift_stays_still_instead_of_tearing() {
    // 255 texels wide: no shift fits a byte, so the axis holds still.
    let (repeat, _) = scrolled(255, [10, 0], [-120, 40, 40]);
    assert_eq!(
        corner_uvs(&repeat),
        [(255, 0), (0, 0), (255, 255), (0, 255)]
    );
}

#[test]
fn a_scrolling_quad_on_the_near_plane_path_still_draws() {
    let mut bound = material();
    bound.set_scroll([13, 21]);
    let (still, ..) = draw_chunk(chunk(191), [70, 40, 40], 0, material());
    let (moving, stream, _) = draw_chunk(chunk(191), [70, 40, 40], 0, bound);
    assert!(moving.robust_quads >= 1, "{moving:?}");
    assert_eq!(moving.packets, still.packets);
    let packets = packets(&stream);
    assert!(packets.iter().any(|p| p[1] == WINDOW));
}
