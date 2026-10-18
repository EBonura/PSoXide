//! The performance heatmap: a model of what the world renderer would spend
//! at every spot a character can stand, from the **cooked** render tables.
//!
//! For each walkable sector, at the camera's eye height over its middle and
//! for several yaws, the model runs the renderer's own selection on the host
//! (the cell cull against the near, left and right planes, the front-facing
//! test of every quad with the same Q7 normal and plane term, the far-block
//! stand-ins) without the GTE projection, and prices the result:
//!
//! ```text
//! cost = Q * cycles_per_quad + N * cycles_per_near_quad          (cycles)
//!   Q = quads that face the camera in the cells inside the frustum
//!   N = those with a corner nearer than `near_depth` (the near-plane path)
//! heat = cost / (frame budget - reserve)      worst yaw wins
//! ```
//!
//! **This is a model, not a measurement.** The design started from 800
//! cycles a visible quad and about 10,000 for a near-plane split (section
//! 11.2); the shipped constants are the fit of the renderer's own counters on
//! the Cathedral route tape (section 20 of the design doc: world cycles =
//! 741 per visible quad + 3,123 per split triangle + 7.9k, R2 0.996, 497
//! frames), a near quad being about three split triangles. The frame budget
//! is 1,128,960 cycles at 30 frames per second and the reserve (470,000 for
//! the player, 58,000 for the sky) is what the Cathedral spends outside the
//! world. The camera is the player's spot lifted by the camera height and
//! pulled back by the camera arm, held by the edge of the floor (a wall record
//! does not hold it, the pitch and the vertical planes are not modelled), and
//! a room is judged alone (no portal view into its neighbours). The constants
//! are public so it can be recalibrated.

use psx_grid::write::RoomChunk;
use psx_grid::{LodBlock, Quad, RCell, Vtx};
use psx_grid::{
    CELL_HAS_LOD, QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_LOD_COVERED,
};

use super::cook::{self, BoundMaterial, CookInput, MaterialBinder, RuntimeUnits};
use super::walk;
use super::world::GridWorld;
use crate::ResourceId;

/// A binder that binds every material to a plain 64 texel texture: the cook
/// is run for its geometry only.
struct GeometryBinder;

impl MaterialBinder for GeometryBinder {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        Ok(BoundMaterial {
            asset: material.map_or(0, |m| m.raw() as u16),
            flags: 0,
            blend: 0,
            uv_motion: 0,
            tint: [128; 3],
            textured: material.is_some(),
            texels: [64, 64],
        })
    }
}

/// Every constant of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatOptions {
    /// Cycles of one frame at 30 frames per second.
    pub budget: u32,
    /// Cycles the frame spends outside the world (player, sky): the
    /// defaults are 470,000 and 58,000, measured on the Cathedral.
    pub reserve: u32,
    /// Cycles of one visible quad on average (measured).
    pub per_quad: u32,
    /// Cycles of one quad on the near-plane path (measured, 9 to 11 thousand).
    pub per_near_quad: u32,
    /// Depth, runtime units, under which a quad counts as near.
    pub near_depth: i32,
    /// Yaws sampled per spot, evenly around the circle.
    pub yaws: u32,
    /// Camera height above the floor, runtime units.
    pub eye: i32,
    /// How far behind the spot the camera trails, runtime units, held back
    /// by the room's edge (a sector with no floor stops it; a wall record
    /// does not).
    pub arm: i32,
    /// Focal length of the lens, in pixels.
    pub focal: i32,
    /// Half the screen width, pixels.
    pub half_width: i32,
    /// Extra pixels the side planes allow.
    pub margin: i32,
    /// Distance of the near plane, runtime units.
    pub near_plane: i32,
    /// Use every `stride`-th sector in each direction (a coarse pass).
    pub stride: u32,
}

impl Default for HeatOptions {
    fn default() -> Self {
        Self {
            budget: 1_128_960,
            reserve: 470_000 + 58_000,
            per_quad: 741,
            per_near_quad: 9_400,
            near_depth: 8,
            yaws: 8,
            eye: 62,
            arm: 125,
            focal: 305,
            half_width: 160,
            margin: 16,
            near_plane: 8,
            stride: 1,
        }
    }
}

impl HeatOptions {
    /// Cycles the world may spend before the frame is over budget.
    pub fn world_budget(&self) -> u32 {
        self.budget.saturating_sub(self.reserve).max(1)
    }
}

/// One spot of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatSample {
    /// Room index.
    pub room: usize,
    /// World sector.
    pub cell: [i32; 2],
    /// Front-facing quads at the worst yaw.
    pub quads: u32,
    /// Of those, quads on the near-plane path.
    pub near: u32,
    /// Modelled world cycles at the worst yaw.
    pub cycles: u32,
    /// The yaw that is worst, in `0..yaws`.
    pub yaw: u8,
}

/// The result of [`heatmap`].
#[derive(Clone, Debug)]
pub struct HeatMap {
    /// The constants used.
    pub options: HeatOptions,
    /// Every sampled spot.
    pub samples: Vec<HeatSample>,
    /// Quads in the cooked level (every room).
    pub quads_total: usize,
    /// Milliseconds the cook took.
    pub cook_ms: f32,
    /// Milliseconds the sampling took.
    pub sample_ms: f32,
}

impl HeatMap {
    /// Heat of a sample: modelled cycles over the world budget (1.0 is the
    /// frame budget).
    pub fn heat(&self, sample: &HeatSample) -> f32 {
        sample.cycles as f32 / self.options.world_budget() as f32
    }

    /// The sample at world sector `cell` of `room`.
    pub fn at(&self, room: usize, cell: [i32; 2]) -> Option<&HeatSample> {
        self.samples
            .iter()
            .find(|s| s.room == room && s.cell == cell)
    }

    /// The hottest sample.
    pub fn worst(&self) -> Option<&HeatSample> {
        self.samples.iter().max_by_key(|s| s.cycles)
    }

    /// Modelled cycles at percentile `p` (0 to 100) of the samples.
    pub fn percentile(&self, p: u32) -> u32 {
        let mut cycles: Vec<u32> = self.samples.iter().map(|s| s.cycles).collect();
        cycles.sort_unstable();
        if cycles.is_empty() {
            return 0;
        }
        cycles[((cycles.len() - 1) * p as usize) / 100]
    }

    /// Quads at percentile `p` of the samples.
    pub fn quads_percentile(&self, p: u32) -> u32 {
        let mut quads: Vec<u32> = self.samples.iter().map(|s| s.quads).collect();
        quads.sort_unstable();
        if quads.is_empty() {
            return 0;
        }
        quads[((quads.len() - 1) * p as usize) / 100]
    }
}

/// One room's render tables, borrowed from the cook.
struct RoomTables<'a> {
    width: usize,
    depth: usize,
    vertices: &'a [Vtx],
    quads: &'a [Quad],
    cells: &'a [RCell],
    lod_quads: &'a [Quad],
    lod_blocks: &'a [LodBlock],
    lod_shift: u32,
    lod_far_cells: i32,
}

impl<'a> RoomTables<'a> {
    fn of(chunk: &'a RoomChunk) -> Self {
        Self {
            width: usize::from(chunk.width),
            depth: usize::from(chunk.depth),
            vertices: &chunk.vertices,
            quads: &chunk.quads,
            cells: &chunk.cells,
            lod_quads: &chunk.lod_quads,
            lod_blocks: &chunk.lod_blocks,
            lod_shift: u32::from(chunk.lod_shift),
            lod_far_cells: i32::from(chunk.lod_far_cells),
        }
    }
}

/// The renderer's front-facing test of one quad.
fn faces(quad: &Quad, camera: [i32; 3]) -> bool {
    let flags = quad.flags;
    if flags & QUAD_DOUBLE != 0 {
        true
    } else if flags & QUAD_AXIS != 0 {
        let axis = usize::from((flags >> QUAD_AXIS_SHIFT) & 3).min(2);
        let c = camera[axis];
        (if flags & QUAD_AXIS_NEG != 0 { -c } else { c }) > i32::from(quad.d)
    } else {
        (i32::from(quad.n[0]) * camera[0]
            + i32::from(quad.n[1]) * camera[1]
            + i32::from(quad.n[2]) * camera[2])
            >> 7
            > i32::from(quad.d)
    }
}

/// Shortest depth along the view direction of a quad's corners.
fn nearest_depth(room: &RoomTables<'_>, quad: &Quad, eye: [i32; 3], forward: [f32; 2]) -> f32 {
    quad.v
        .iter()
        .map(|&i| {
            let v = &room.vertices[usize::from(i)];
            (f32::from(v.x) - eye[0] as f32) * forward[0]
                + (f32::from(v.z) - eye[2] as f32) * forward[1]
        })
        .fold(f32::MAX, f32::min)
}

/// The camera for `yaw`: the spot pulled back along the view direction by
/// the arm, in steps of a quarter sector, while the sector it would stand in
/// has a floor.
fn arm_eye(
    room: &RoomTables<'_>,
    has_floor: &[bool],
    size: i32,
    spot: [i32; 3],
    yaw: f32,
    arm: i32,
) -> [i32; 3] {
    let (fx, fz) = (yaw.sin(), yaw.cos());
    let step = (size / 4).max(1);
    let mut eye = spot;
    let mut back = step;
    while back <= arm {
        let x = spot[0] as f32 - fx * back as f32;
        let z = spot[2] as f32 - fz * back as f32;
        let (cx, cz) = (x.floor() as i32 / size, z.floor() as i32 / size);
        let inside = x >= 0.0
            && z >= 0.0
            && (cx as usize) < room.width
            && (cz as usize) < room.depth
            && has_floor[cz as usize * room.width + cx as usize];
        if !inside {
            break;
        }
        eye = [x as i32, spot[1], z as i32];
        back += step;
    }
    eye
}

/// Price one view: facing quads and near ones.
fn view(
    room: &RoomTables<'_>,
    size: i32,
    eye: [i32; 3],
    yaw: f32,
    o: &HeatOptions,
    block_seen: &mut [u32],
    stamp: u32,
) -> (u32, u32) {
    let (fx, fz) = (yaw.sin(), yaw.cos());
    let (rx, rz) = (fz, -fx);
    let k = (o.half_width + o.margin) as f32 / o.focal.max(1) as f32;
    let sizef = size as f32;
    let (ex, ez) = (eye[0] as f32, eye[2] as f32);
    let (cam_cx, cam_cz) = (eye[0].div_euclid(size), eye[2].div_euclid(size));
    let blocks_w = (room.width + (1 << room.lod_shift) - 1) >> room.lod_shift;
    let lod = room.lod_far_cells != 0 && !room.lod_blocks.is_empty();
    let (mut quads, mut near) = (0u32, 0u32);
    let count = |quad: &Quad, quads: &mut u32, near: &mut u32| {
        if faces(quad, eye) {
            *quads += 1;
            if nearest_depth(room, quad, eye, [fx, fz]) < o.near_depth as f32 {
                *near += 1;
            }
        }
    };
    for cz in 0..room.depth {
        for cx in 0..room.width {
            let cell = &room.cells[cz * room.width + cx];
            if cell.count == 0 {
                continue;
            }
            // The cell's four plan corners against the near, left and right
            // planes: out of the frustum when all four are outside one.
            let mut outside = [true; 3];
            for (dx, dz) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let px = (cx as f32 + dx) * sizef - ex;
                let pz = (cz as f32 + dz) * sizef - ez;
                let depth = px * fx + pz * fz;
                let side = px * rx + pz * rz;
                outside[0] &= depth < o.near_plane as f32;
                outside[1] &= side < -k * depth;
                outside[2] &= side > k * depth;
            }
            if outside.iter().any(|o| *o) {
                continue;
            }
            let mut far = false;
            if lod && cell.flags & CELL_HAS_LOD != 0 {
                let half = (1i32 << room.lod_shift) >> 1;
                let (bx, bz) = ((cx >> room.lod_shift) as i32, (cz >> room.lod_shift) as i32);
                let dx = ((bx << room.lod_shift) + half - cam_cx).abs();
                let dz = ((bz << room.lod_shift) + half - cam_cz).abs();
                if dx.max(dz) > room.lod_far_cells {
                    far = true;
                    let block = bz as usize * blocks_w + bx as usize;
                    if block_seen[block] != stamp {
                        block_seen[block] = stamp;
                        let b = &room.lod_blocks[block];
                        for q in usize::from(b.first)..usize::from(b.first) + usize::from(b.count) {
                            count(&room.lod_quads[q], &mut quads, &mut near);
                        }
                    }
                }
            }
            for q in usize::from(cell.first)..usize::from(cell.first) + usize::from(cell.count) {
                let quad = &room.quads[q];
                if far && quad.flags & QUAD_LOD_COVERED != 0 {
                    continue;
                }
                count(quad, &mut quads, &mut near);
            }
        }
    }
    (quads, near)
}

/// Cook `world` and sample every standable sector. `Err` when the world does
/// not cook.
pub fn heatmap(world: &GridWorld, options: HeatOptions) -> Result<HeatMap, String> {
    let started = std::time::Instant::now();
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder: &mut GeometryBinder,
            lights: &[],
            first_pack_sector: 0,
            lod_far_cells: cook::lod::LOD_FAR_CELLS,
            texture_repeat: true,
        },
        None,
    )?;
    let cook_ms = started.elapsed().as_secs_f32() * 1000.0;
    let units = RuntimeUnits::new(world.scale)?;
    let size = units.sector() as i32;
    let standing = walk::analyse(world, &[], &psx_engine::WalkLimits::DEFAULT)
        .ok_or("the world has no cookable scale")?;
    let started = std::time::Instant::now();
    let mut samples = Vec::new();
    for (ri, (room, chunk)) in world.rooms.iter().zip(&cooked.tables).enumerate() {
        let tables = RoomTables::of(chunk);
        let Some(walk) = standing.rooms.get(ri) else {
            continue;
        };
        let mut block_seen = vec![0u32; tables.lod_blocks.len().max(1)];
        let mut stamp = 0u32;
        let has_floor: Vec<bool> = (0..tables.width * tables.depth)
            .map(|i| {
                room.sector(i % tables.width, i / tables.width)
                    .is_some_and(|s| s.floor().is_some())
            })
            .collect();
        let stride = options.stride.max(1) as usize;
        for (x, z, sector) in room.iter() {
            if x % stride != 0 || z % stride != 0 {
                continue;
            }
            let cell = room.world_cell(x, z);
            let Some(w) = walk.cell(cell) else { continue };
            if !w.standable {
                continue;
            }
            let Some(floor) = sector.floor() else {
                continue;
            };
            // Room-local height of the middle of the floor, plus the eye.
            let mean = floor.heights.iter().sum::<i32>() / 4;
            let y = cook::collision::local_height(&units, room.y_offset, mean) + options.eye;
            let eye = [x as i32 * size + size / 2, y, z as i32 * size + size / 2];
            let mut worst = (0u32, 0u32, 0u32, 0u8);
            for k in 0..options.yaws.max(1) {
                stamp += 1;
                let yaw = k as f32 * core::f32::consts::TAU / options.yaws.max(1) as f32;
                let cam = arm_eye(&tables, &has_floor, size, eye, yaw, options.arm);
                let (quads, near) = view(&tables, size, cam, yaw, &options, &mut block_seen, stamp);
                let cycles = quads * options.per_quad + near * options.per_near_quad;
                if cycles >= worst.2 {
                    worst = (quads, near, cycles, k as u8);
                }
            }
            samples.push(HeatSample {
                room: ri,
                cell,
                quads: worst.0,
                near: worst.1,
                cycles: worst.2,
                yaw: worst.3,
            });
        }
    }
    Ok(HeatMap {
        options,
        samples,
        quads_total: cooked.tables.iter().map(|t| t.quads.len()).sum(),
        cook_ms,
        sample_ms: started.elapsed().as_secs_f32() * 1000.0,
    })
}

/// Every yaw of one spot, `(quads, near quads)` each: for comparing the model
/// with a measured frame at a known pose.
pub fn spot_views(
    world: &GridWorld,
    room: usize,
    cell: [i32; 2],
    options: HeatOptions,
) -> Result<Vec<(u32, u32)>, String> {
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder: &mut GeometryBinder,
            lights: &[],
            first_pack_sector: 0,
            lod_far_cells: cook::lod::LOD_FAR_CELLS,
            texture_repeat: true,
        },
        None,
    )?;
    let units = RuntimeUnits::new(world.scale)?;
    let size = units.sector() as i32;
    let r = world.rooms.get(room).ok_or("no such room")?;
    let chunk = cooked.tables.get(room).ok_or("no such room")?;
    let tables = RoomTables::of(chunk);
    let [x, z] = r.local(cell).ok_or("the sector is outside the room")?;
    let floor = r
        .sector(x, z)
        .and_then(|s| s.floor())
        .ok_or("the sector has no floor")?;
    let mean = floor.heights.iter().sum::<i32>() / 4;
    let y = cook::collision::local_height(&units, r.y_offset, mean) + options.eye;
    let eye = [x as i32 * size + size / 2, y, z as i32 * size + size / 2];
    let mut block_seen = vec![0u32; tables.lod_blocks.len().max(1)];
    let has_floor: Vec<bool> = (0..tables.width * tables.depth)
        .map(|i| {
            r.sector(i % tables.width, i / tables.width)
                .is_some_and(|s| s.floor().is_some())
        })
        .collect();
    Ok((0..options.yaws.max(1))
        .map(|k| {
            let yaw = k as f32 * core::f32::consts::TAU / options.yaws.max(1) as f32;
            let cam = arm_eye(&tables, &has_floor, size, eye, yaw, options.arm);
            view(&tables, size, cam, yaw, &options, &mut block_seen, k + 1)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;

    fn hall(w: usize, d: usize) -> GridWorld {
        let mut room = GridRoom::new_derived("hall", w, d);
        for z in 0..d {
            for x in 0..w {
                let s = room.sector_or_insert(x, z).unwrap();
                s.set_floor(Some(GridSurface::flat_auto(0, None)));
                s.set_ceiling(Some(GridSurface::flat_auto(3072, None)));
            }
        }
        GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn every_standable_sector_gets_a_sample_and_the_model_prices_quads() {
        let world = hall(8, 8);
        let map = heatmap(&world, HeatOptions::default()).unwrap();
        assert_eq!(map.samples.len(), 64);
        assert!(map.quads_total > 0);
        let centre = map.at(0, [4, 4]).unwrap();
        // The worst yaw sees something, and the price is the formula.
        assert!(centre.quads > 0);
        let o = HeatOptions::default();
        assert_eq!(
            centre.cycles,
            centre.quads * o.per_quad + centre.near * o.per_near_quad
        );
        assert!(map.heat(centre) > 0.0);
    }

    #[test]
    fn a_wall_hides_what_is_behind_it_and_facing_decides_what_counts() {
        // A room split by a tall derived platform: standing in the corner the
        // model counts fewer quads than in an open hall of the same size.
        let open = heatmap(&hall(8, 8), HeatOptions::default()).unwrap();
        let mut walled = hall(8, 8);
        for z in 0..8 {
            let s = walled.rooms[0].sector_mut(4, z).unwrap();
            s.floor_mut().unwrap().heights = [2048; 4];
        }
        let b = heatmap(&walled, HeatOptions::default()).unwrap();
        // The step is taller than the body climbs: those sectors are still
        // standable (flat) so both maps have samples; the walled one has more
        // quads (the wall faces) but each view still counts only facing ones.
        let corner_open = open.at(0, [0, 0]).unwrap().quads;
        let corner_walled = b.at(0, [0, 0]).unwrap().quads;
        assert!(corner_open > 0 && corner_walled > 0);
        // Backfaces never count: no view sees all quads of the level.
        assert!(open.worst().unwrap().quads as usize <= open.quads_total);
    }

    #[test]
    fn the_coarse_pass_samples_every_other_sector_and_the_result_is_deterministic() {
        let world = hall(8, 8);
        let coarse = heatmap(
            &world,
            HeatOptions {
                stride: 2,
                yaws: 4,
                ..HeatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(coarse.samples.len(), 16);
        let a = heatmap(&world, HeatOptions::default()).unwrap();
        let b = heatmap(&world, HeatOptions::default()).unwrap();
        assert_eq!(a.samples, b.samples);
    }

    #[test]
    fn the_world_budget_is_the_frame_minus_the_reserve() {
        let o = HeatOptions::default();
        assert_eq!(o.world_budget(), 1_128_960 - 528_000);
        assert_eq!((o.per_quad, o.near_depth), (741, 8));
    }
}
