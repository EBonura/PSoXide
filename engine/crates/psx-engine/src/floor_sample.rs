//! Shared sector floor-height sampling.
//!
//! `character_motor` (player feet) and `third_person_camera` (camera
//! floor probes) both interpolate a floor height from a sector's quad
//! corner heights, split diagonal and per-triangle heights. The two
//! must agree exactly or the camera measures a different floor than
//! the one the player stands on, so the interpolation lives here once.

const SPLIT_NE_SW: u8 = psx_asset::WORLD_SPLIT_NORTH_EAST_SOUTH_WEST;

/// Overlay a floor triangle's explicit heights onto the sector's
/// fallback quad corner heights `[nw, ne, se, sw]`.
pub(crate) fn triangle_heights_to_quad(
    mut fallback: [i32; 4],
    split: u8,
    triangle: usize,
    heights: [i32; 3],
) -> [i32; 4] {
    let corners = psx_asset::world_topology::split_triangles(split)[triangle.min(1)];
    fallback[corners[0]] = heights[0];
    fallback[corners[1]] = heights[1];
    fallback[corners[2]] = heights[2];
    fallback
}

/// Interpolate the floor height at a local position inside a sector
/// from the quad corner heights `[nw, ne, se, sw]`, walking the
/// triangle that the split diagonal puts the point in.
pub(crate) fn height_at_local(
    heights: [i32; 4],
    split: u8,
    local_x: i32,
    local_z: i32,
    sector: i32,
) -> i32 {
    let u = local_x.clamp(0, sector);
    let v = local_z.clamp(0, sector);
    let [nw, ne, se, sw] = heights;
    if split == SPLIT_NE_SW {
        if u + v <= sector {
            nw.saturating_add(mul_sector(ne.saturating_sub(nw), u, sector))
                .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
        } else {
            sw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
                .saturating_add(mul_sector(ne.saturating_sub(se), sector - v, sector))
        }
    } else if v <= u {
        nw.saturating_add(mul_sector(ne.saturating_sub(nw), u - v, sector))
            .saturating_add(mul_sector(se.saturating_sub(nw), v, sector))
    } else {
        nw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
            .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
    }
}

/// `delta * amount / sector` without overflowing the intermediate
/// product: the whole and remainder parts of `delta / sector` scale
/// separately, so the partial products stay inside i32 for any height
/// delta a room can encode.
fn mul_sector(delta: i32, amount: i32, sector: i32) -> i32 {
    if sector <= 0 {
        0
    } else {
        let whole = (delta / sector).saturating_mul(amount);
        let remainder = delta % sector;
        whole.saturating_add(remainder.saturating_mul(amount) / sector)
    }
}
