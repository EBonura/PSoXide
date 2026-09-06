//! Scatter the already shaded body packets, then return them to the live pose.

use super::{DashWireVisual, PrimitivePacketArena, ProjectedVertex, TriTextured};

/// Packet geometry stays in its original depth plane, preserving world occlusion.
///
/// # Safety
/// The supplied arena range must contain only the freshly submitted textured
/// player body, before wireframe or equipment packets are appended.
pub(super) unsafe fn scatter_body_packets(
    arena: &mut PrimitivePacketArena<'_>,
    first_slot: usize,
    projected: &[ProjectedVertex],
    visual: DashWireVisual,
) {
    let distance = match visual {
        DashWireVisual::Converting { progress_q8 } => i32::from(progress_q8),
        // Linear approach keeps the travelling pieces visible throughout
        // reconstruction instead of collapsing most distance in its first frames.
        DashWireVisual::Restoring { progress_q8 } => 255 - i32::from(progress_q8),
        _ => return,
    };
    if distance == 0 {
        return;
    }
    let mut top = i16::MAX;
    let mut bottom = i16::MIN;
    for point in projected
        .iter()
        .filter(|point| **point != ProjectedVertex::INVALID)
    {
        top = top.min(point.sy);
        bottom = bottom.max(point.sy);
    }
    let height = (i32::from(bottom) - i32::from(top)).clamp(16, 240);
    let end = arena.used_slots();
    let mut index = 0;
    // SAFETY: the caller guarantees a contiguous range of body TriTextureds.
    unsafe {
        arena.mutate_typed_slots::<TriTextured>(first_slot, end, |triangle| {
            scatter_triangle(triangle, index, height, distance);
            index += 1;
        });
    }
}

fn scatter_triangle(triangle: &mut TriTextured, index: u32, height: i32, distance: i32) {
    if distance == 0 {
        return;
    }
    let corners = [triangle.v0, triangle.v1, triangle.v2]
        .map(|word| (i32::from(word as i16), i32::from((word >> 16) as i16)));
    let cx = corners.iter().map(|p| p.0).sum::<i32>() / 3;
    let cy = corners.iter().map(|p| p.1).sum::<i32>() / 3;
    // Eight stable scattering directions, shared by small groups of facets.
    // No per-particle state, additional posing, or new textures are required.
    const DIRECTIONS: [(i32, i32); 8] = [
        (256, 0),
        (181, 181),
        (0, 256),
        (-181, 181),
        (-256, 0),
        (-181, -181),
        (0, -256),
        (181, -181),
    ];
    let (dx, dy) = DIRECTIONS[(index.wrapping_mul(5) & 7) as usize];
    let radius = height * distance / 640;
    let dx = dx * radius >> 8;
    let dy = dy * radius >> 8;
    let scale = 256 - distance * distance / 255;
    let moved = corners.map(|(x, y)| {
        (
            cx + dx + ((x - cx) * scale >> 8),
            cy + dy + ((y - cy) * scale >> 8),
        )
    });
    let safe = moved
        .iter()
        .all(|&(x, y)| (-1023..=1023).contains(&x) && (-1023..=1023).contains(&y))
        && moved.iter().map(|p| p.0).max().unwrap() - moved.iter().map(|p| p.0).min().unwrap()
            <= 1023
        && moved.iter().map(|p| p.1).max().unwrap() - moved.iter().map(|p| p.1).min().unwrap()
            <= 511;
    if !safe || distance >= 250 {
        // Degenerate polygons preserve the DMA chain without touching its tags.
        triangle.v1 = triangle.v0;
        triangle.v2 = triangle.v0;
        return;
    }
    let packed =
        moved.map(|(x, y)| u32::from(x as i16 as u16) | (u32::from(y as i16 as u16) << 16));
    [triangle.v0, triangle.v1, triangle.v2] = packed;
    // Uploaded model palettes already mark nonzero entries for STP blending.
    // Additive modulation lets a fragment fade all the way to the background,
    // while retaining its original texture and per-face crystal colour.
    // Return small groups to opacity near attachment, avoiding a whole-body
    // brightness jump when the last translucent frame becomes solid.
    if distance < 64 && (index.wrapping_mul(37) & 63) >= distance as u32 {
        return;
    }
    let strength = 255 - distance;
    let base = triangle.color_cmd;
    let channel = |shift: u32| (((base >> shift) & 255) as i32 * strength / 255) as u32;
    triangle.color_cmd =
        ((base | 0x0200_0000) & 0xfe00_0000) | channel(0) | (channel(8) << 8) | (channel(16) << 16);
    triangle.uv1_tpage = (triangle.uv1_tpage & !(3 << 21)) | (1 << 21);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> TriTextured {
        TriTextured {
            tag: 0x08001234,
            tex_window: 0xe2000000,
            color_cmd: 0x246080a0,
            v0: 100 | (100 << 16),
            v1: 110 | (100 << 16),
            v2: 105 | (112 << 16),
            uv0_clut: 0x78201234,
            uv1_tpage: 0x01985678,
            uv2: 0x0000abcd,
        }
    }

    #[test]
    fn fragments_preserve_texture_lighting_ratios_and_dma_links() {
        let mut fragment = triangle();
        let original = triangle();
        scatter_triangle(&mut fragment, 3, 80, 128);
        assert_eq!(fragment.tag, original.tag);
        assert_eq!(fragment.tex_window, original.tex_window);
        assert_eq!(fragment.uv0_clut, original.uv0_clut);
        assert_eq!(fragment.uv2, original.uv2);
        assert_eq!(
            fragment.uv1_tpage & !(3 << 21),
            original.uv1_tpage & !(3 << 21)
        );
        assert_eq!(fragment.color_cmd >> 24, 0x26);
        assert_eq!(fragment.color_cmd & 0xffffff, 0x2f3f4f);
        assert_ne!(fragment.v0, original.v0);
        scatter_triangle(&mut fragment, 3, 80, 255);
        assert_eq!(fragment.v0, fragment.v1);
        assert_eq!(fragment.v0, fragment.v2);
        let mut restored = triangle();
        scatter_triangle(&mut restored, 3, 80, 0);
        assert_eq!(restored.color_cmd, original.color_cmd);
        assert_eq!(restored.v0, original.v0);
        assert_eq!(restored.uv1_tpage, original.uv1_tpage);
    }
}
