//! Distance LOD for open floors: merged far quads.
//!
//! Open halls cost the same in any representation, the lever is fewer, bigger
//! far quads. For every block of `2^shift` by `2^shift` cells the cook finds
//! maximal rectangles of horizontal quads (same plane, same material, same
//! facing, UVs that continue linearly) and writes one merged quad per
//! rectangle into `LOD_QUADS`. Corners of a merged quad are corners of its
//! cell quads, so they already exist in `VERTICES`, and take their baked
//! colour and texel from the cell that owns the corner: the merge adds no
//! new bake. Every cell quad a merged quad stands in for is flagged
//! `QUAD_LOD_COVERED`; at runtime a block beyond `lod_far_cells` draws the
//! merged quads instead of the covered ones.
//!
//! A merged quad spans at most two cells (64 texels), so its UVs stay inside
//! one texture repeat and it draws with the same packet as a cell quad.

use std::collections::HashMap;

use psx_grid::{
    LodBlock, Quad, QUAD_AXIS, QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_LOD_COVERED, QUAD_TRI,
};

use super::render::{Tables, TILE_TEXELS};

/// Cells per block edge, as a shift.
pub const LOD_SHIFT: u8 = 2;
/// Most cells a merged quad spans along one edge. Two cells are 64 texels:
/// the span stays inside one texture repeat.
pub const MAX_SPAN: usize = 2;
/// Distance in sectors beyond which a block draws its merged quads.
pub const LOD_FAR_CELLS: u8 = 6;
// The cook tests pass this explicitly; Play leaves LOD off by default.

/// Merged output of one room.
#[derive(Default)]
pub struct Lod {
    pub blocks: Vec<LodBlock>,
    pub quads: Vec<Quad>,
}

/// Which corner of a cell a vertex sits at: 0 NW, 1 NE, 2 SE, 3 SW.
fn corner_of(p: [i16; 3], cell: [i32; 2], s: i32) -> Option<usize> {
    let dx = i32::from(p[0]) - cell[0] * s;
    let dz = i32::from(p[2]) - cell[1] * s;
    match (dx, dz) {
        (0, 0) => Some(0),
        (d, 0) if d == s => Some(1),
        (d, e) if d == s && e == s => Some(2),
        (0, e) if e == s => Some(3),
        _ => None,
    }
}

struct Candidate {
    quad: usize,
    /// For each cell corner [NW, NE, SE, SW], the quad corner index at it.
    at: [usize; 4],
}

/// Whether quad `q` is a mergeable full-cell horizontal quad of `cell`.
fn candidate(tables: &Tables, q: usize, cell: [i32; 2], s: i32) -> Option<Candidate> {
    let quad = &tables.quads[q];
    if quad.flags & QUAD_AXIS == 0
        || (quad.flags >> QUAD_AXIS_SHIFT) & 3 != 1
        || quad.flags & (QUAD_TRI | QUAD_DOUBLE | QUAD_LOD_COVERED) != 0
    {
        return None;
    }
    let y = tables.vertices[usize::from(quad.v[0])].y;
    let mut at = [usize::MAX; 4];
    for k in 0..4 {
        let v = tables.vertices[usize::from(quad.v[k])];
        if v.y != y {
            return None;
        }
        let corner = corner_of([v.x, v.y, v.z], cell, s)?;
        if at[corner] != usize::MAX {
            return None;
        }
        at[corner] = k;
    }
    Some(Candidate { quad: q, at })
}

/// Build the LOD tables of a room and flag the covered cell quads in place.
pub fn build(width: usize, depth: usize, s: i32, merge_far: bool, tables: &mut Tables) -> Lod {
    let block = 1usize << LOD_SHIFT;
    let (bw, bd) = (width.div_ceil(block), depth.div_ceil(block));
    let by_position: HashMap<[i16; 3], u16> = tables
        .vertices
        .iter()
        .enumerate()
        .map(|(i, v)| ([v.x, v.y, v.z], i as u16))
        .collect();
    let mut lod = Lod::default();
    let mut covered: Vec<usize> = Vec::new();
    for bz in 0..bd {
        for bx in 0..bw {
            let first = lod.quads.len();
            // Candidates per cell of the block, grouped by plane key.
            let mut keys: Vec<(i16, u8, u8)> = Vec::new();
            let mut cells: HashMap<(i16, u8, u8), Vec<Option<Candidate>>> = HashMap::new();
            for dz in 0..block {
                for dx in 0..block {
                    let (cx, cz) = (bx * block + dx, bz * block + dz);
                    if cx >= width || cz >= depth {
                        continue;
                    }
                    let rcell = tables.cells[cz * width + cx];
                    let range = usize::from(rcell.first)
                        ..usize::from(rcell.first) + usize::from(rcell.count);
                    for q in range.filter(|_| merge_far) {
                        let Some(c) = candidate(tables, q, [cx as i32, cz as i32], s) else {
                            continue;
                        };
                        let quad = &tables.quads[q];
                        let key = (
                            tables.vertices[usize::from(quad.v[0])].y,
                            quad.material,
                            quad.flags & psx_grid::QUAD_AXIS_NEG,
                        );
                        let slot = cells.entry(key).or_insert_with(|| {
                            keys.push(key);
                            (0..block * block).map(|_| None).collect()
                        });
                        slot[dz * block + dx] = Some(c);
                    }
                }
            }
            for key in keys {
                let grid = cells.get_mut(&key).expect("key was inserted");
                let mut claimed = vec![false; block * block];
                for dz in 0..block {
                    for dx in 0..block {
                        if claimed[dz * block + dx] || grid[dz * block + dx].is_none() {
                            continue;
                        }
                        let free =
                            |x: usize, z: usize, claimed: &[bool], grid: &[Option<Candidate>]| {
                                x < block
                                    && z < block
                                    && !claimed[z * block + x]
                                    && grid[z * block + x].is_some()
                            };
                        let mut w = 1;
                        while w < MAX_SPAN && free(dx + w, dz, &claimed, grid) {
                            w += 1;
                        }
                        let mut h = 1;
                        'rows: while h < MAX_SPAN {
                            for x in dx..dx + w {
                                if !free(x, dz + h, &claimed, grid) {
                                    break 'rows;
                                }
                            }
                            h += 1;
                        }
                        // Shrink until the rectangle's UVs are linear.
                        loop {
                            if w * h < 2 {
                                break;
                            }
                            if let Some(merged) = merge(
                                tables,
                                grid,
                                &by_position,
                                (bx * block + dx, bz * block + dz),
                                (w, h),
                                block,
                                dx,
                                dz,
                                s,
                            ) {
                                for z in dz..dz + h {
                                    for x in dx..dx + w {
                                        claimed[z * block + x] = true;
                                        if let Some(c) = &grid[z * block + x] {
                                            covered.push(c.quad);
                                        }
                                    }
                                }
                                lod.quads.push(merged);
                                break;
                            }
                            if h >= w && h > 1 {
                                h -= 1;
                            } else if w > 1 {
                                w -= 1;
                            } else {
                                break;
                            }
                        }
                        claimed[dz * block + dx] = true;
                    }
                }
            }
            let count = lod.quads.len() - first;
            // The block's vertical extent and sky flag come from its cells:
            // the sky test of the renderer uses them.
            let (mut lo, mut hi) = (i16::MAX, i16::MIN);
            let mut flags = 0u8;
            for dz in 0..block {
                for dx in 0..block {
                    let (cx, cz) = (bx * block + dx, bz * block + dz);
                    if cx >= width || cz >= depth {
                        continue;
                    }
                    let cell = tables.cells[cz * width + cx];
                    if cell.count > 0 {
                        lo = lo.min(cell.min_y);
                        hi = hi.max(cell.max_y);
                        if cell.flags & psx_grid::CELL_OPEN_SKY != 0 {
                            flags |= psx_grid::BLOCK_OPEN_SKY;
                        }
                    }
                }
            }
            lod.blocks.push(LodBlock {
                first: first as u16,
                count: count.min(255) as u8,
                flags,
                min_y: if lo > hi { 0 } else { lo },
                max_y: if lo > hi { 0 } else { hi },
            });
        }
    }
    for q in covered {
        tables.quads[q].flags |= QUAD_LOD_COVERED;
    }
    // Cells holding a covered quad say so.
    for (index, cell) in tables.cells.iter_mut().enumerate() {
        let _ = index;
        let range = usize::from(cell.first)..usize::from(cell.first) + usize::from(cell.count);
        if tables.quads[range]
            .iter()
            .any(|q| q.flags & QUAD_LOD_COVERED != 0)
        {
            cell.flags |= psx_grid::CELL_HAS_LOD;
        }
    }
    lod
}

/// Merge the `w` by `h` rectangle with top-left block cell `(dx, dz)`.
#[allow(clippy::too_many_arguments)]
fn merge(
    tables: &Tables,
    grid: &[Option<Candidate>],
    by_position: &HashMap<[i16; 3], u16>,
    cell0: (usize, usize),
    (w, h): (usize, usize),
    block: usize,
    dx: usize,
    dz: usize,
    s: i32,
) -> Option<Quad> {
    let at = |x: usize, z: usize| grid[(dz + z) * block + dx + x].as_ref();
    let first = at(0, 0)?;
    let proto = &tables.quads[first.quad];
    // Rectangle corners in cell terms: (x, z, cell corner index).
    let rect = [
        (0usize, 0usize, 0usize),
        (w - 1, 0, 1),
        (w - 1, h - 1, 2),
        (0, h - 1, 3),
    ];
    // Texel and colour at each rectangle corner, from the cell that owns it.
    let mut texel = [[0.0f64; 2]; 4];
    let mut colour = [0u32; 4];
    let mut index = [0u16; 4];
    for (k, &(x, z, corner)) in rect.iter().enumerate() {
        let c = at(x, z)?;
        let q = &tables.quads[c.quad];
        let qc = c.at[corner];
        texel[k] = tables.texels[c.quad][qc];
        colour[k] = q.color[qc];
        let v = tables.vertices[usize::from(q.v[qc])];
        index[k] = *by_position.get(&[v.x, v.y, v.z])?;
    }
    // Linearity: every cell corner's texel must lie on the rectangle's
    // bilinear map of its corner texels.
    let (ww, hh) = (w as f64, h as f64);
    for z in 0..h {
        for x in 0..w {
            let c = at(x, z)?;
            let q = &tables.quads[c.quad];
            for corner in 0..4 {
                let (ox, oz) = match corner {
                    0 => (0.0, 0.0),
                    1 => (1.0, 0.0),
                    2 => (1.0, 1.0),
                    _ => (0.0, 1.0),
                };
                let (u, v) = ((x as f64 + ox) / ww, (z as f64 + oz) / hh);
                let want = |axis: usize| {
                    texel[0][axis] * (1.0 - u) * (1.0 - v)
                        + texel[1][axis] * u * (1.0 - v)
                        + texel[2][axis] * u * v
                        + texel[3][axis] * (1.0 - u) * v
                };
                let have = tables.texels[c.quad][c.at[corner]];
                if (want(0) - have[0]).abs() > 1e-3 || (want(1) - have[1]).abs() > 1e-3 {
                    return None;
                }
            }
            let _ = q;
        }
    }
    // One whole-period shift for the rectangle. The span stays inside one
    // 64 texel repeat (two cells), so merged quads need no texture window and
    // use the same packet as a cell; the far edge clamps to 63 like a cell's.
    let mut uv = [[0u8; 2]; 4];
    for axis in 0..2 {
        let min = texel.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
        let shift = (min / TILE_TEXELS).floor() * TILE_TEXELS;
        for k in 0..4 {
            let value = (texel[k][axis] - shift).round();
            if !(0.0..=TILE_TEXELS + 0.5).contains(&value) {
                return None;
            }
            uv[k][axis] = value.min(TILE_TEXELS - 1.0) as u8;
        }
    }
    // Ring order of the prototype: the cell quad's corners walk NW,NE,SE,SW
    // in some rotation or reflection; reproduce it on the rectangle.
    let mut merged = Quad {
        d: proto.d,
        n: proto.n,
        material: proto.material,
        flags: proto.flags & !QUAD_LOD_COVERED,
        ..Quad::default()
    };
    for (slot, &quad_corner) in (0..4).zip(first_order(first).iter()) {
        let k = quad_corner;
        merged.v[slot] = index[k];
        merged.color[slot] = colour[k];
        merged.uv[slot] = u16::from(uv[k][0]) | (u16::from(uv[k][1]) << 8);
    }
    let _ = (cell0, s);
    Some(merged)
}

/// For each ring slot of the proto quad, which cell corner (NW, NE, SE, SW)
/// that slot sits at.
fn first_order(c: &Candidate) -> [usize; 4] {
    let mut order = [0usize; 4];
    for (corner, &slot) in c.at.iter().enumerate() {
        order[slot] = corner;
    }
    order
}
