//! Quake's view-direction layered-sky projection.
//!
//! Sky brush polygons define only the aperture through which the sky is seen.
//! Their authored surface UVs must not make the sky appear attached to nearby
//! geometry. This module is kept renderer-neutral so PSoXide and Quake-PSX can
//! use one integer projection and one seam-safe packet UV policy.
//!
//! Ported from `quake-core/src/sky.rs` at Quake-PSX revision
//! `e32f6f66cff1759954f224846ce0b326c3d55d30` (GPL-2, same authorship).

use psx_math::int32::isqrt_i32;

use psx_engine::ClassicAffineSubmit;
use psx_gpu::material::TextureWindow;
use psx_gpu::prim::QuadTextured;
use psx_gte::math::Mat3I16;

const MATERIAL_TICKS_PER_SECOND: u32 = 60;
const SKY_BACKGROUND_CYCLE_SECONDS: u32 = 16;
const SKY_FOREGROUND_CYCLE_SECONDS: u32 = 8;
const SKY_COLUMNS: usize = 10;
const SKY_ROWS: usize = 12;
const SKY_CELLS: usize = SKY_COLUMNS * SKY_ROWS;
const SKY_OT_SLOT: u32 = 2047;
const SKY_QUAD_WORDS: usize = 10;
const SKY_WINDOW_PACKET_WORDS: usize = 2;
const SKY_WINDOW_PACKET_COUNT: usize = 3;

/// Packet words required by the constant-cost two-layer screen lattice.
pub const VIEW_RAY_SKY_PACKET_WORDS: usize =
    SKY_CELLS * 2 * SKY_QUAD_WORDS + SKY_WINDOW_PACKET_COUNT * SKY_WINDOW_PACKET_WORDS;

/// One staged GP0(E2) selector for a complete sky layer.
#[repr(C, align(4))]
struct SkyWindowPacket {
    tag: u32,
    command: u32,
}

impl SkyWindowPacket {
    const fn new(command: u32) -> Self {
        Self {
            tag: (1 << 24) | SKY_OT_SLOT,
            command,
        }
    }
}

const _: () = assert!(
    core::mem::size_of::<SkyWindowPacket>()
        == SKY_WINDOW_PACKET_WORDS * core::mem::size_of::<u32>()
);
const _: () =
    assert!(core::mem::size_of::<QuadTextured>() == SKY_QUAD_WORDS * core::mem::size_of::<u32>());

/// Return signed material-relative texel coordinates for a Quake sky ray.
///
/// Keeping this signed until a small raster cell is emitted is important.
/// Casting a whole dome corner to `u8` can cross the byte seam and make the
/// PS1 interpolate through most of the texture between adjacent vertices.
pub fn directional_texel(mut direction: [i32; 3], layer_width: u8) -> [i32; 2] {
    direction[2] = direction[2].saturating_mul(3);

    // Keep the squared length inside i32 without changing the direction.
    while direction[0]
        .unsigned_abs()
        .max(direction[1].unsigned_abs())
        .max(direction[2].unsigned_abs())
        > 16_000
    {
        direction[0] >>= 1;
        direction[1] >>= 1;
        direction[2] >>= 1;
    }

    let length_squared = direction[0]
        .saturating_mul(direction[0])
        .saturating_add(direction[1].saturating_mul(direction[1]))
        .saturating_add(direction[2].saturating_mul(direction[2]));
    let length = isqrt_i32(length_squared).max(1);
    let denominator = length * 128;
    let project = |component: i32| {
        // Original Quake uses `6 * 63 / length` against a 128-texel layer.
        // Preserve that projection while scaling it to the selected sky mip.
        let numerator = component * 378 * i32::from(layer_width);
        numerator / denominator
    };

    [project(direction[0]), project(direction[1])]
}

/// Recover a world-space viewing ray from one screen coordinate.
///
/// `world_to_view_q12` is the rotation already loaded for the frame. Its
/// transpose is sufficient because Quake's coordinate conversion has a
/// uniform scale, which disappears during sky normalisation.
pub fn screen_view_ray(
    screen: [i16; 2],
    center: [i16; 2],
    projection: i16,
    world_to_view_q12: [[i16; 3]; 3],
) -> [i32; 3] {
    let camera = [
        i64::from(screen[0] - center[0]),
        i64::from(screen[1] - center[1]),
        i64::from(projection),
    ];
    let mut world = [0i32; 3];
    for axis in 0..3 {
        let value = camera[0] * i64::from(world_to_view_q12[0][axis])
            + camera[1] * i64::from(world_to_view_q12[1][axis])
            + camera[2] * i64::from(world_to_view_q12[2][axis]);
        world[axis] = (value >> 12) as i32;
    }
    world
}

/// Convert a PSoXide Y-up world vector back to Quake's Z-up sky basis.
///
/// The brush importer maps `(quake_x, quake_y, quake_z)` to
/// `(world_x, world_y, world_z) = (quake_x, quake_z, -quake_y)`.
#[inline]
pub const fn quake_direction_from_y_up(direction: [i32; 3]) -> [i32; 3] {
    [direction[0], -direction[2], direction[1]]
}

/// Rebase four signed sky samples into packet UV bytes without changing the
/// local projection gradient.
///
/// The texture window repeats every `period` texels, but that does not make
/// the shortest wrapped delta the correct affine gradient. Keep the original
/// signed deltas and translate the whole packet by complete periods until all
/// four coordinates fit in GP0's byte UVs.
pub fn packet_quad_uv(
    samples: [[i32; 2]; 4],
    atlas: [u8; 2],
    period: [u8; 2],
    scroll: [u8; 2],
) -> [[u8; 2]; 4] {
    let mut output = [[0u8; 2]; 4];
    for axis in 0..2 {
        let period = i32::from(period[axis]).max(1);
        let scroll = i32::from(scroll[axis]);
        let sample_anchor = samples[0][axis];
        let anchor = sample_anchor + i32::from(atlas[axis]) + scroll;
        let mut values = [0i32; 4];
        values[0] = anchor;
        for index in 1..4 {
            values[index] = anchor + samples[index][axis] - sample_anchor;
        }

        let mut minimum = values[0];
        let mut maximum = values[0];
        for value in &values[1..] {
            minimum = minimum.min(*value);
            maximum = maximum.max(*value);
        }
        while minimum < 0 {
            for value in &mut values {
                *value += period;
            }
            minimum += period;
            maximum += period;
        }
        while maximum > 255 {
            for value in &mut values {
                *value -= period;
            }
            minimum -= period;
            maximum -= period;
        }
        debug_assert!(minimum >= 0 && maximum <= 255);
        for index in 0..4 {
            output[index][axis] = values[index] as u8;
        }
    }
    output
}

/// Return material-relative UV bytes for a layered Quake sky vertex.
///
/// This helper is retained for parity tests and tools. The runtime background
/// uses [`screen_view_ray`] so packet cost does not grow with sky-face count.
pub fn directional_uv(
    vertex_units: [i16; 3],
    camera_origin_q12: [i32; 3],
    layer_width: u8,
) -> [u8; 2] {
    let direction = [
        i32::from(vertex_units[0]).saturating_sub(camera_origin_q12[0] >> 12),
        i32::from(vertex_units[1]).saturating_sub(camera_origin_q12[1] >> 12),
        i32::from(vertex_units[2]).saturating_sub(camera_origin_q12[2] >> 12),
    ];
    let projected = directional_texel(direction, layer_width);
    [projected[0] as u8, projected[1] as u8]
}

/// Draw Quake's two sky layers as a bounded view-ray background.
///
/// Visible sky brushes select this material but emit no geometry. The caller
/// appends the returned tagged packets after world geometry so prepend-only OT
/// insertion executes the sky first, behind opaque world surfaces.
///
/// `layer_size` describes one of the two adjacent square atlas halves.
/// `screen_size` and `screen_center` use framebuffer pixel coordinates.
///
/// # Safety
///
/// `output` must have room for [`VIEW_RAY_SKY_PACKET_WORDS`] writable `u32`
/// values and must be aligned for the packet structs written here.
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_view_ray_layered_sky(
    texture_page: u16,
    clut: u16,
    atlas_origin: [u8; 2],
    layer_size: [u8; 2],
    view_rotation: Mat3I16,
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    material_tick: u32,
    output: *mut u32,
) -> ClassicAffineSubmit {
    let width = layer_size[0].clamp(8, 128);
    let height = layer_size[1].clamp(8, 128);
    debug_assert!(width.is_power_of_two());
    debug_assert!(height.is_power_of_two());
    debug_assert!(atlas_origin[0].is_multiple_of(width));
    debug_assert!(atlas_origin[1].is_multiple_of(height));
    debug_assert!(u16::from(atlas_origin[0]) + u16::from(width) * 2 <= 256);
    debug_assert!(u16::from(atlas_origin[1]) + u16::from(height) <= 256);
    let foreground_window =
        TextureWindow::power_of_two_tile(atlas_origin[0], atlas_origin[1], width, height);
    let background_origin = [atlas_origin[0].wrapping_add(width), atlas_origin[1]];
    let background_window =
        TextureWindow::power_of_two_tile(background_origin[0], background_origin[1], width, height);
    let scroll = |cycle_seconds: u32| {
        ((u64::from(material_tick) * u64::from(width)
            / u64::from(MATERIAL_TICKS_PER_SECOND * cycle_seconds))
            & 0xff) as u8
    };
    let foreground_scroll = [
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
    ];
    let background_scroll = [
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
    ];

    let screen_width = screen_size[0].max(1);
    let screen_height = screen_size[1].max(1);
    let mut samples = [[[0i32; 2]; SKY_COLUMNS + 1]; SKY_ROWS + 1];
    for (row, sample_row) in samples.iter_mut().enumerate() {
        let y = (row * screen_height as usize / SKY_ROWS) as i16;
        for (column, sample) in sample_row.iter_mut().enumerate() {
            let x = (column * screen_width as usize / SKY_COLUMNS) as i16;
            let world_ray =
                screen_view_ray([x, y], screen_center, projection.max(1), view_rotation.m);
            *sample = directional_texel(quake_direction_from_y_up(world_ray), width);
        }
    }

    let mut next = output;
    // The tagged stream is linked by prepending packets. Stage the reset first
    // so it executes after both sky layers and before ordinary world geometry.
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(TextureWindow::NONE.word()));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }

    let mut emit_layer = |atlas: [u8; 2], window: TextureWindow, scroll: [u8; 2]| {
        for row in 0..SKY_ROWS {
            let y0 = (row * screen_height as usize / SKY_ROWS) as i16;
            let y1 = ((row + 1) * screen_height as usize / SKY_ROWS) as i16;
            for column in 0..SKY_COLUMNS {
                let x0 = (column * screen_width as usize / SKY_COLUMNS) as i16;
                let x1 = ((column + 1) * screen_width as usize / SKY_COLUMNS) as i16;
                let vertices = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
                let cell_samples = [
                    samples[row][column],
                    samples[row][column + 1],
                    samples[row + 1][column],
                    samples[row + 1][column + 1],
                ];
                let uv = packet_quad_uv(cell_samples, atlas, [width, height], scroll)
                    .map(|[u, v]| (u, v));
                unsafe {
                    let mut quad =
                        QuadTextured::new(vertices, uv, clut, texture_page, (0x80, 0x80, 0x80));
                    quad.tag = ((QuadTextured::WORDS as u32) << 24) | SKY_OT_SLOT;
                    next.cast::<QuadTextured>().write(quad);
                    next = next.add(SKY_QUAD_WORDS);
                }
            }
        }
        unsafe {
            next.cast::<SkyWindowPacket>()
                .write(SkyWindowPacket::new(window.word()));
            next = next.add(SKY_WINDOW_PACKET_WORDS);
        }
    };

    // Foreground is staged first, then background. Equal-slot OT prepending
    // reverses their execution so the opaque background draws first and the
    // masked foreground overlays it.
    emit_layer(atlas_origin, foreground_window, foreground_scroll);
    emit_layer(background_origin, background_window, background_scroll);

    ClassicAffineSubmit {
        next_packet: next,
        packets: (SKY_CELLS * 2 + SKY_WINDOW_PACKET_COUNT) as u32,
        hardware_triangles: (SKY_CELLS * 4) as u32,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::{
        directional_texel, directional_uv, packet_quad_uv, quake_direction_from_y_up,
        screen_view_ray, submit_view_ray_layered_sky, VIEW_RAY_SKY_PACKET_WORDS,
    };
    use psx_gte::math::Mat3I16;

    #[test]
    fn distance_does_not_create_sky_parallax() {
        assert_eq!(
            directional_uv([100, 0, 0], [0, 0, 0], 64),
            directional_uv([200, 0, 0], [0, 0, 0], 64)
        );
    }

    #[test]
    fn translating_camera_and_aperture_together_keeps_the_sky_fixed() {
        let original = directional_uv([100, -40, 25], [0, 0, 0], 64);
        let translated =
            directional_uv([1_100, -540, 225], [1_000 << 12, -500 << 12, 200 << 12], 64);
        assert_eq!(translated, original);
    }

    #[test]
    fn quake_basis_center_ray_points_forward() {
        let basis = [[0, -0x3000, 0], [0, 0, -0x3000], [0x3000, 0, 0]];
        assert_eq!(
            screen_view_ray([160, 120], [160, 120], 160, basis),
            [480, 0, 0]
        );
        assert_eq!(directional_texel([480, 0, 0], 64), [189, 0]);
    }

    #[test]
    fn y_up_vectors_convert_back_to_the_imported_quake_basis() {
        assert_eq!(quake_direction_from_y_up([5, 7, -11]), [5, 11, 7]);
    }

    #[test]
    fn packet_uv_crosses_a_tile_seam_locally() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [0, 0],
        );
        assert_eq!(uv, [[61, 36], [67, 36], [61, 42], [67, 42]]);
        assert_eq!(uv[1][0] - uv[0][0], 6);
    }

    #[test]
    fn scrolling_cannot_reintroduce_a_packet_seam() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [98, 151],
        );
        for axis in 0..2 {
            let minimum = uv.iter().map(|sample| sample[axis]).min().unwrap();
            let maximum = uv.iter().map(|sample| sample[axis]).max().unwrap();
            assert!(maximum - minimum <= 6);
        }
    }

    #[test]
    fn more_than_half_period_gradient_keeps_its_direction() {
        let uv = packet_quad_uv(
            [[372, 0], [366, -66], [376, 0], [371, -63]],
            [0, 0],
            [128, 128],
            [0, 0],
        );

        assert_eq!(uv, [[244, 128], [238, 62], [248, 128], [243, 65]]);
        assert_eq!(i16::from(uv[1][1]) - i16::from(uv[0][1]), -66);
    }

    #[test]
    fn view_ray_background_has_constant_tagged_packet_cost() {
        let mut packets = vec![0u32; VIEW_RAY_SKY_PACKET_WORDS];
        let submitted = unsafe {
            submit_view_ray_layered_sky(
                0x0105,
                0x1234,
                [0, 0],
                [128, 128],
                Mat3I16 {
                    m: [[0, 0, 0x1000], [0, -0x1000, 0], [0x1000, 0, 0]],
                },
                [320, 240],
                [160, 120],
                160,
                0,
                packets.as_mut_ptr(),
            )
        };
        assert_eq!(
            unsafe { submitted.next_packet.offset_from(packets.as_ptr()) as usize },
            VIEW_RAY_SKY_PACKET_WORDS
        );
        assert_eq!(submitted.packets, 243);
        assert_eq!(submitted.hardware_triangles, 480);
        assert_eq!(packets[0] >> 24, 1);
        assert_eq!(packets[0] & 0x00ff_ffff, 2047);
        assert_eq!(packets[2] >> 24, 9);
        assert_eq!(packets[2] & 0x00ff_ffff, 2047);
    }
}
