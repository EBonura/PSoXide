//! Baking the far skin of a cooked level.
//!
//! The skin is the whole level seen from above: per owned sector the top
//! surface height and a colour (the material's mean texel colour lit by the
//! baked light of the highest horizontal surface), merged into a quadtree of
//! squares that do not cross a room, with a skirt on every edge that drops to
//! a lower neighbour. See [`psx_grid::SkinLeaf`] for the record and
//! `psx_grid::render::draw_skin` for how it is drawn.

use std::collections::BTreeMap;

use psx_grid::write::SkinBlock;
use psx_grid::{
    SkinHeader, SkinLeaf, QUAD_AXIS, QUAD_AXIS_SHIFT, SKIN_HEIGHT_STEPS, SKIN_MAX_SIZE_LOG2,
    SKIRT_STEPS,
};

use super::CookedLevel;

/// Knobs of the bake.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SkinSettings {
    /// Runtime units two sectors' tops may differ by and still merge, at the
    /// finest level of detail.
    pub height_tolerance: i32,
    /// Largest per-channel difference (of 255) two colours may have and
    /// still merge, at the finest level of detail.
    pub colour_tolerance: i32,
}

impl Default for SkinSettings {
    fn default() -> Self {
        Self {
            height_tolerance: 20,
            colour_tolerance: 24,
        }
    }
}

/// Levels of detail the bake makes: the finest, a medium one for rooms a
/// while off, a coarse one for the far field.
pub const SKIN_LODS: usize = 3;

/// What one level of detail merges: a multiple of the height tolerance, a
/// colour tolerance, and the smallest leaf (log2) it makes where a block is
/// whole.
struct Detail {
    height_tolerance: i32,
    colour_tolerance: i32,
    min_size_log2: u8,
}

fn details(settings: &SkinSettings) -> [Detail; SKIN_LODS] {
    [
        Detail {
            height_tolerance: settings.height_tolerance,
            colour_tolerance: settings.colour_tolerance,
            min_size_log2: 0,
        },
        Detail {
            height_tolerance: settings.height_tolerance * 3,
            colour_tolerance: settings.colour_tolerance + 16,
            min_size_log2: 1,
        },
        Detail {
            height_tolerance: settings.height_tolerance * 8,
            colour_tolerance: settings.colour_tolerance + 40,
            min_size_log2: 2,
        },
    ]
}

/// What the bake made.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SkinReport {
    pub sectors: usize,
    /// Leaves over every level of detail.
    pub leaves: usize,
    /// Leaves of each level of detail.
    pub per_lod: [usize; SKIN_LODS],
    /// Bytes the block adds to the core.
    pub bytes: usize,
    /// Finest-detail leaves by size, 1, 2, 4 and 8 sectors across.
    pub by_size: [usize; 4],
    /// Runtime units per height step.
    pub height_step: i32,
}

/// One owned sector: its room, its top and lowest vertex height (world
/// runtime units) and its colour.
#[derive(Copy, Clone)]
struct Column {
    room: u16,
    top: i32,
    low: i32,
    rgb: [i32; 3],
}

/// Colour of a quad: the material's mean colour times the mean baked light
/// of its corners (128 is neutral light), channels 0 to 255.
fn shade(albedo: [f32; 3], colours: [u32; 4]) -> [i32; 3] {
    let mut light = [0.0f32; 3];
    for c in colours {
        for (channel, sum) in light.iter_mut().enumerate() {
            *sum += ((c >> (8 * channel)) & 255) as f32 / 4.0;
        }
    }
    [0, 1, 2].map(|i| {
        (albedo[i] * 255.0 * light[i] / 128.0)
            .round()
            .clamp(0.0, 255.0) as i32
    })
}

/// The columns of every owned sector that draws something, by world sector.
fn columns(cooked: &CookedLevel, albedo: &[[f32; 3]]) -> BTreeMap<(i32, i32), Column> {
    let sector = i32::from(cooked.level.sector_units);
    let mut out = BTreeMap::new();
    for (room, record) in cooked.level.rooms.iter().enumerate() {
        let table = &cooked.tables[room];
        let width = usize::from(record.width);
        let owns = |cx: usize, cz: usize| {
            cooked.level.occupancy.get(room).is_none_or(|bits| {
                let bit = cz * width + cx;
                bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
            })
        };
        for cz in 0..usize::from(record.depth) {
            for cx in 0..width {
                if !owns(cx, cz) {
                    continue;
                }
                let Some(cell) = table.cells.get(cz * width + cx).filter(|c| c.count > 0) else {
                    continue;
                };
                let (mut top, mut low) = (i32::MIN, i32::MAX);
                // (top of the quad, colour) of the best horizontal quad and of
                // the best quad of any slope.
                let (mut flat, mut any) = ((i32::MIN, [128; 3]), (i32::MIN, [128; 3]));
                for q in usize::from(cell.first)..usize::from(cell.first) + usize::from(cell.count)
                {
                    let quad = &table.quads[q];
                    let ys = quad
                        .v
                        .map(|v| i32::from(table.vertices[usize::from(v)].y) + record.origin[1]);
                    let (quad_top, quad_low) = (
                        ys.into_iter().max().unwrap_or(0),
                        ys.into_iter().min().unwrap_or(0),
                    );
                    top = top.max(quad_top);
                    low = low.min(quad_low);
                    let slot = table
                        .material_use
                        .get(usize::from(quad.material))
                        .map_or(0, |&m| usize::from(m));
                    let rgb = shade(albedo.get(slot).copied().unwrap_or([0.5; 3]), quad.color);
                    let horizontal = if quad.flags & QUAD_AXIS != 0 {
                        (quad.flags >> QUAD_AXIS_SHIFT) & 3 == 1
                    } else {
                        quad.n[1] > 64
                    };
                    if horizontal && quad_top >= flat.0 {
                        flat = (quad_top, rgb);
                    }
                    if quad_top >= any.0 {
                        any = (quad_top, rgb);
                    }
                }
                let rgb = if flat.0 > i32::MIN { flat.1 } else { any.1 };
                let key = (
                    (record.origin[0] + cx as i32 * sector).div_euclid(sector),
                    (record.origin[2] + cz as i32 * sector).div_euclid(sector),
                );
                out.insert(
                    key,
                    Column {
                        room: room as u16,
                        top,
                        low,
                        rgb,
                    },
                );
            }
        }
    }
    out
}

fn rgb555(rgb: [i32; 3]) -> u16 {
    let c = |v: i32| (v.clamp(0, 255) >> 3) as u16;
    c(rgb[0]) | c(rgb[1]) << 5 | c(rgb[2]) << 10
}

/// Bake the skin of `cooked`. `albedo` is the mean colour of each level
/// material, in slot order (`cooked.materials`).
pub fn bake(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    settings: SkinSettings,
) -> Result<(SkinBlock, SkinReport), String> {
    let sector = i32::from(cooked.level.sector_units);
    let cols = columns(cooked, albedo);
    let Some((&(min_x, min_z), _)) = cols.iter().next() else {
        return Err("the skin has nothing to draw: no sector has a surface".to_string());
    };
    let (mut lo_x, mut hi_x, mut lo_z, mut hi_z) = (min_x, min_x, min_z, min_z);
    let (mut min_top, mut max_top) = (i32::MAX, i32::MIN);
    for (&(x, z), col) in &cols {
        lo_x = lo_x.min(x);
        hi_x = hi_x.max(x);
        lo_z = lo_z.min(z);
        hi_z = hi_z.max(z);
        min_top = min_top.min(col.top);
        max_top = max_top.max(col.top);
    }
    if hi_x - lo_x >= 256 || hi_z - lo_z >= 256 {
        return Err(format!(
            "the skin spans {} by {} sectors, at most 256 by 256",
            hi_x - lo_x + 1,
            hi_z - lo_z + 1
        ));
    }
    let world_limit = i32::from(i16::MAX);
    if (hi_x + 1) * sector > world_limit
        || (hi_z + 1) * sector > world_limit
        || lo_x * sector < -world_limit
        || lo_z * sector < -world_limit
        || max_top > world_limit
        || min_top < -world_limit
    {
        return Err("the skin reaches past 32767 runtime units from the world origin".to_string());
    }
    let height_step = ((max_top - min_top) + i32::from(SKIN_HEIGHT_STEPS) - 2)
        / (i32::from(SKIN_HEIGHT_STEPS) - 1);
    let height_step = height_step.max(1);
    let steps_of = |top: i32| ((top - min_top + height_step / 2) / height_step).clamp(0, 511);

    let rooms = cooked.level.rooms.len();
    let detail = details(&settings);
    let mut leaves: Vec<SkinLeaf> = Vec::new();
    let mut cell_first: Vec<u16> = Vec::with_capacity(rooms * SKIN_LODS + 1);
    // Leaf squares as (x, z, size log2, top height), for the skirts.
    let mut squares: Vec<(i32, i32, u8, i32)> = Vec::new();
    let mut by_size = [0usize; 4];
    let mut per_lod = [0usize; SKIN_LODS];
    for room in 0..rooms {
        let mine: Vec<(i32, i32)> = cols
            .iter()
            .filter(|(_, c)| usize::from(c.room) == room)
            .map(|(&k, _)| k)
            .collect();
        let mut blocks: Vec<(i32, i32)> = mine
            .iter()
            .map(|&(x, z)| (x.div_euclid(8) * 8, z.div_euclid(8) * 8))
            .collect();
        blocks.sort_unstable();
        blocks.dedup();
        for (lod, level) in detail.iter().enumerate() {
            cell_first
                .push(u16::try_from(leaves.len()).map_err(|_| "more than 65535 skin leaves")?);
            let before = leaves.len();
            for &(bx, bz) in &blocks {
                split(
                    &cols,
                    room as u16,
                    (bx, bz, SKIN_MAX_SIZE_LOG2),
                    level,
                    &mut |x, z, k, top, rgb| {
                        let place_x = (x - lo_x) as u8;
                        let place_z = (z - lo_z) as u8;
                        squares.push((x, z, k, steps_of(top) * height_step + min_top));
                        if lod == 0 {
                            by_size[usize::from(k)] += 1;
                        }
                        leaves.push(SkinLeaf::pack(
                            place_x,
                            place_z,
                            k,
                            steps_of(top) as u16,
                            rgb555(rgb),
                            [0; 4],
                        ));
                    },
                );
            }
            per_lod[lod] += leaves.len() - before;
        }
    }
    cell_first.push(u16::try_from(leaves.len()).map_err(|_| "more than 65535 skin leaves")?);

    // Skirts: down to the lowest neighbour across each edge.
    for (leaf, &(x, z, k, top)) in leaves.iter_mut().zip(&squares) {
        let n = 1i32 << k;
        let mut nibbles = [0u8; 4];
        let low_here = (0..n)
            .flat_map(|i| (0..n).map(move |j| (x + i, z + j)))
            .filter_map(|key| cols.get(&key).map(|c| c.low))
            .min()
            .unwrap_or(top);
        // The sector across edge `side` (-Z, +X, +Z, -X) beside the leaf's
        // `i`th sector along that edge.
        let neighbour = |side: usize, i: i32| match side {
            0 => (x + i, z - 1),
            1 => (x + n, z + i),
            2 => (x + i, z + n),
            _ => (x - 1, z + i),
        };
        for (side, nibble) in nibbles.iter_mut().enumerate() {
            let mut floor = i32::MAX;
            let mut missing = false;
            for i in 0..n {
                match cols.get(&neighbour(side, i)) {
                    Some(c) => floor = floor.min(c.top),
                    None => missing = true,
                }
            }
            if missing {
                floor = if floor == i32::MAX {
                    low_here
                } else {
                    floor.min(low_here)
                };
            }
            let drop = top - floor;
            if drop <= 0 {
                continue;
            }
            let need = ((drop + height_step - 1) / height_step) as u16;
            *nibble = SKIRT_STEPS
                .iter()
                .position(|&s| s >= need)
                .map_or(15, |p| p as u8);
        }
        *leaf = SkinLeaf::pack(
            leaf.x(),
            leaf.z(),
            leaf.size_log2(),
            leaf.height(),
            leaf.rgb555(),
            nibbles,
        );
    }

    let report = SkinReport {
        sectors: cols.len(),
        leaves: leaves.len(),
        per_lod,
        bytes: 12 + 2 * cell_first.len() + 8 * leaves.len(),
        by_size,
        height_step,
    };
    Ok((
        SkinBlock {
            header: SkinHeader {
                leaf_count: leaves.len() as u16,
                base_y: min_top as i16,
                origin: [lo_x as i16, lo_z as i16],
                height_step: u8::try_from(height_step).map_err(|_| "height step above 255")?,
                lod_count: SKIN_LODS as u8,
                reserved: [0; 2],
            },
            cell_first,
            leaves,
        },
        report,
    ))
}

/// Split the aligned block `(x, z, k)` of room `room` into leaves: a block
/// wholly owned by the room and flat and uniform enough becomes one leaf,
/// anything else its four quarters, down to single sectors.
fn split(
    cols: &BTreeMap<(i32, i32), Column>,
    room: u16,
    (x, z, k): (i32, i32, u8),
    detail: &Detail,
    leaf: &mut dyn FnMut(i32, i32, u8, i32, [i32; 3]),
) {
    let n = 1i32 << k;
    let mut found: Vec<&Column> = Vec::with_capacity((n * n) as usize);
    for dz in 0..n {
        for dx in 0..n {
            if let Some(c) = cols.get(&(x + dx, z + dz)).filter(|c| c.room == room) {
                found.push(c);
            }
        }
    }
    if found.is_empty() {
        return;
    }
    if found.len() == (n * n) as usize {
        let (lo, hi) = found.iter().fold((i32::MAX, i32::MIN), |(lo, hi), c| {
            (lo.min(c.top), hi.max(c.top))
        });
        let spread = (0..3)
            .map(|ch| {
                let (a, b) = found
                    .iter()
                    .fold((255, 0), |(a, b), c| (a.min(c.rgb[ch]), b.max(c.rgb[ch])));
                b - a
            })
            .max()
            .unwrap_or(0);
        // A whole block is a leaf at the smallest size this level of detail
        // makes, or when it is flat and uniform enough.
        if k == 0
            || k == detail.min_size_log2
            || (hi - lo <= detail.height_tolerance && spread <= detail.colour_tolerance)
        {
            let count = found.len() as i32;
            let mean_top = (found.iter().map(|c| c.top).sum::<i32>() + count / 2) / count;
            let rgb = [0, 1, 2]
                .map(|ch| (found.iter().map(|c| c.rgb[ch]).sum::<i32>() + count / 2) / count);
            leaf(x, z, k, mean_top, rgb);
            return;
        }
    }
    let h = n / 2;
    for (qx, qz) in [(0, 0), (h, 0), (0, h), (h, h)] {
        split(cols, room, (x + qx, z + qz, k - 1), detail, leaf);
    }
}
