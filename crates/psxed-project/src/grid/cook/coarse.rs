//! Baking the coarse band of a cooked level.
//!
//! Each room's quads are merged by vertex clustering: the corners of every
//! quad snap to a three-dimensional grid of `cluster` runtime units, the
//! corners in one grid cell become one vertex (their mean position, their
//! mean colour: the material's mean texel colour times the baked light of the
//! corner), and a triangle survives when its three corners land in three
//! different cells. Duplicate triangles are dropped. A room whose mesh would
//! pass [`COARSE_MAX_VERTICES`] vertices is clustered again with a coarser
//! grid until it fits. See `psx_grid::render::CoarseRenderer` for the draw.

use std::collections::{BTreeMap, BTreeSet};

use psx_grid::write::CoarseBlock;
use psx_grid::{CoarseHeader, CoarseVertex, QUAD_TRI};

use super::CookedLevel;

/// Cluster size of a room's mesh when the settings do not say, runtime units
/// per sector times two.
pub const DEFAULT_CLUSTER_SECTORS: f32 = 2.0;

/// What the bake made.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoarseReport {
    pub vertices: usize,
    pub triangles: usize,
    /// Triangles before merging (two a quad).
    pub source_triangles: usize,
    /// Most vertices and triangles of one room.
    pub max_vertices: usize,
    pub max_triangles: usize,
    /// Rooms that needed a coarser grid to fit the vertex limit.
    pub regridded: usize,
    /// Bytes the block adds to the core.
    pub bytes: usize,
}

/// One quad corner: world position and colour.
type Sample = ([f32; 3], [f32; 3]);

/// A room's triangles as indices into `samples` (corner samples, one per
/// quad corner so each keeps its own colour).
fn gather(cooked: &CookedLevel, albedo: &[[f32; 3]], room: usize) -> (Vec<Sample>, Vec<[usize; 3]>) {
    let table = &cooked.tables[room];
    let origin = cooked.level.rooms[room].origin;
    let mut samples = Vec::new();
    let mut triangles = Vec::new();
    for quad in &table.quads {
        let slot = table
            .material_use
            .get(usize::from(quad.material))
            .map_or(0, |&m| usize::from(m));
        let base = albedo.get(slot).copied().unwrap_or([0.5; 3]);
        let first = samples.len();
        let corners = if quad.flags & QUAD_TRI != 0 { 3 } else { 4 };
        for k in 0..corners {
            let v = table.vertices[usize::from(quad.v[k])];
            let position = [
                (origin[0] + i32::from(v.x)) as f32,
                (origin[1] + i32::from(v.y)) as f32,
                (origin[2] + i32::from(v.z)) as f32,
            ];
            let colour = [0, 1, 2]
                .map(|i| base[i] * 255.0 * ((quad.color[k] >> (8 * i)) & 255) as f32 / 128.0);
            samples.push((position, colour));
        }
        triangles.push([first, first + 1, first + 2]);
        if corners == 4 {
            triangles.push([first, first + 2, first + 3]);
        }
    }
    (samples, triangles)
}

/// Cluster one room's samples with grid cells of `cell` units.
fn cluster(
    samples: &[Sample],
    triangles: &[[usize; 3]],
    cell: f32,
) -> (Vec<CoarseVertex>, Vec<[u8; 3]>) {
    // Cell of each sample, then its vertex number in first-seen order.
    let key = |p: [f32; 3]| [0, 1, 2].map(|i| (p[i] / cell).floor() as i32);
    let mut cells: BTreeMap<[i32; 3], usize> = BTreeMap::new();
    let mut sums: Vec<([f32; 3], [f32; 3], f32)> = Vec::new();
    let mut of_sample = Vec::with_capacity(samples.len());
    for (position, colour) in samples {
        let index = *cells.entry(key(*position)).or_insert_with(|| {
            sums.push(([0.0; 3], [0.0; 3], 0.0));
            sums.len() - 1
        });
        let entry = &mut sums[index];
        for i in 0..3 {
            entry.0[i] += position[i];
            entry.1[i] += colour[i];
        }
        entry.2 += 1.0;
        of_sample.push(index);
    }
    let vertices = sums
        .iter()
        .map(|(position, colour, n)| {
            let c = |v: f32| (v / n).clamp(0.0, 255.0) as u16 >> 3;
            CoarseVertex {
                x: (position[0] / n).round() as i16,
                y: (position[1] / n).round() as i16,
                z: (position[2] / n).round() as i16,
                rgb: c(colour[0]) | c(colour[1]) << 5 | c(colour[2]) << 10,
            }
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for t in triangles {
        let c = t.map(|s| of_sample[s]);
        if c[0] == c[1] || c[1] == c[2] || c[0] == c[2] {
            continue;
        }
        let mut sorted = c;
        sorted.sort_unstable();
        if seen.insert(sorted) {
            out.push(c.map(|v| v as u8));
        }
    }
    (vertices, out)
}

/// Bake the coarse mesh of every room. `albedo` is the mean colour of each
/// level material in slot order (`cooked.materials`); `cluster_sectors` the
/// grid cell in sectors; `far_sectors` and `overlap_sectors` go into the
/// header for the draw.
pub fn bake(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    cluster_sectors: f32,
    far_sectors: u8,
    overlap_sectors: u8,
) -> Result<(CoarseBlock, CoarseReport), String> {
    let sector = f32::from(cooked.level.sector_units);
    let limit = i32::from(i16::MAX);
    let mut block = CoarseBlock {
        vertex_first: vec![0],
        triangle_first: vec![0],
        ..CoarseBlock::default()
    };
    let mut report = CoarseReport::default();
    for room in 0..cooked.level.rooms.len() {
        let (samples, triangles) = gather(cooked, albedo, room);
        report.source_triangles += triangles.len();
        let mut cell = (cluster_sectors * sector).max(1.0);
        let (mut vertices, mut tris) = cluster(&samples, &triangles, cell);
        let mut grew = false;
        while vertices.len() > psx_grid::COARSE_MAX_VERTICES {
            grew = true;
            cell *= 1.25;
            if cell > sector * 64.0 {
                return Err(format!("room {room}: no grid fits its mesh in 255 vertices"));
            }
            (vertices, tris) = cluster(&samples, &triangles, cell);
        }
        report.regridded += usize::from(grew);
        if vertices.iter().any(|v| {
            [v.x, v.y, v.z].iter().any(|&c| i32::from(c).abs() >= limit)
        }) {
            return Err(format!(
                "room {room}'s coarse mesh reaches past 32767 runtime units from the world origin"
            ));
        }
        report.max_vertices = report.max_vertices.max(vertices.len());
        report.max_triangles = report.max_triangles.max(tris.len());
        block.vertices.extend(vertices);
        block.triangles.extend(tris);
        let (v, t) = (block.vertices.len(), block.triangles.len());
        block.vertex_first.push(u16::try_from(v).map_err(|_| "coarse vertices above 65535")?);
        block.triangle_first.push(u16::try_from(t).map_err(|_| "coarse triangles above 65535")?);
    }
    block.header = CoarseHeader {
        vertex_count: block.vertices.len() as u16,
        triangle_count: block.triangles.len() as u16,
        far_sectors,
        overlap_sectors,
        reserved: [0; 2],
    };
    report.vertices = block.vertices.len();
    report.triangles = block.triangles.len();
    report.bytes = 8
        + 4 * block.vertex_first.len()
        + 8 * block.vertices.len()
        + 3 * block.triangles.len();
    Ok((block, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cook::tests::cook;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::world::GridWorld;

    /// A floored room of `w` by `d` sectors at height `height`.
    fn flat(w: usize, d: usize, height: i32) -> GridRoom {
        let mut room = GridRoom::new_derived("flat", w, d);
        for z in 0..d {
            for x in 0..w {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(height, None)));
            }
        }
        room
    }

    #[test]
    fn a_flat_room_merges_into_a_few_triangles_with_the_same_look() {
        let cooked = cook(&GridWorld {
            rooms: vec![flat(8, 8, 0)],
            ..GridWorld::default()
        })
        .unwrap();
        let (block, report) = bake(&cooked, &vec![[0.5; 3]; 8], 4.0, 14, 2).unwrap();
        // 64 cells of two triangles each become a handful.
        assert_eq!(report.source_triangles, 128);
        assert!(report.triangles > 0 && report.triangles < 24, "{}", report.triangles);
        assert_eq!(block.vertex_first, vec![0, block.vertices.len() as u16]);
        assert_eq!(block.triangle_first, vec![0, block.triangles.len() as u16]);
        assert!(block
            .triangles
            .iter()
            .all(|t| t.iter().all(|&c| usize::from(c) < block.vertices.len())));
        // The floor stays at its height.
        assert!(block.vertices.iter().all(|v| v.y.abs() < 8), "{:?}", block.vertices);
        // And round-trips through the core.
        let mut level = cooked.level.clone();
        level.coarse = Some(block);
        let words = psx_grid::write::aligned_copy(&level.try_to_bytes().unwrap());
        let view =
            psx_grid::LevelView::parse(psx_grid::write::words_as_bytes(&words, words.len() * 4))
                .unwrap();
        assert!(view.coarse.is_some() && view.validate_full().is_ok());
    }

    #[test]
    fn a_finer_grid_keeps_more_of_the_room() {
        let cooked = cook(&GridWorld {
            rooms: vec![flat(8, 8, 0)],
            ..GridWorld::default()
        })
        .unwrap();
        let albedo = vec![[0.5; 3]; 8];
        let coarse = bake(&cooked, &albedo, 4.0, 14, 2).unwrap().1.triangles;
        let fine = bake(&cooked, &albedo, 1.0, 14, 2).unwrap().1.triangles;
        assert!(fine > coarse, "{fine} against {coarse}");
    }
}
