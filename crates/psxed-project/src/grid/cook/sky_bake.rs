//! Baked distance: the world beyond a chunk's ring, as a cube sky.
//!
//! A level that streams as a ring of chunks draws only the chunk the player
//! is in and its neighbours. Everything farther is baked into the sky from
//! the chunk's centre: every texel of the six-face cube atlas the runtime
//! already draws as a sky casts a ray through the level's own collision, with
//! the ring left out, and shades what it hits with the level's fog. The seam
//! where the live geometry ends is therefore the jagged outline of the ring,
//! and the fog continues across it.
//!
//! The rays use the runtime's collision, so the bake sees exactly the
//! geometry the game has. A hit takes the mean colour of the material of the
//! surface it lands on ([`WorldColours`]: the floor, ceiling or derived wall
//! of the document leaf there), shaded by a key light, then the fog.

use psx_engine::sky::{CubeFace, CUBE_SKY_FACE_HEIGHT, CUBE_SKY_FACE_WIDTH};
use psx_engine::{CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, RoomPoint};
use psx_grid::collision::GridTraceProvider;
use psx_grid::write::{aligned_copy, words_as_bytes};
use psx_grid::{CollisionView, LevelView, ResidentRooms, RoomView};

use std::collections::HashMap;

use super::CookedLevel;
use crate::grid::leaf::CLICKS_PER_SECTOR;
use crate::grid::sector::GridSector;
use crate::grid::world::GridWorld;
use crate::ResourceId;

/// What a hit is made of: the document's surfaces under a world point,
/// coloured by their materials' mean texel colour.
pub struct WorldColours<'a> {
    world: &'a GridWorld,
    /// Runtime units per sector.
    sector: i32,
    /// World sector of `rooms[0]`.
    origin: [i32; 2],
    width: usize,
    /// Per world sector, the room holding a leaf there (`u16::MAX`: none).
    rooms: Vec<u16>,
    /// Mean texel colour per material, channels 0..=1.
    albedo: HashMap<Option<ResourceId>, [f32; 3]>,
}

impl<'a> WorldColours<'a> {
    /// Colours of `world` cooked at `sector` runtime units a sector, with the
    /// mean colour of each material (a material missing from `albedo` reads
    /// mid grey).
    pub fn new(
        world: &'a GridWorld,
        sector: i32,
        albedo: HashMap<Option<ResourceId>, [f32; 3]>,
    ) -> Self {
        let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
        for room in &world.rooms {
            lo = [lo[0].min(room.origin[0]), lo[1].min(room.origin[1])];
            hi = [
                hi[0].max(room.origin[0] + room.width as i32),
                hi[1].max(room.origin[1] + room.depth as i32),
            ];
        }
        let width = (hi[0] - lo[0]).max(0) as usize;
        let depth = (hi[1] - lo[1]).max(0) as usize;
        let mut rooms = vec![u16::MAX; width * depth];
        for (index, room) in world.rooms.iter().enumerate() {
            for (key, _) in room.leaves() {
                let (a, b) = key.sector_range();
                for z in a[1]..=b[1] {
                    for x in a[0]..=b[0] {
                        let cell = (z - lo[1]) as usize * width + (x - lo[0]) as usize;
                        if rooms[cell] == u16::MAX {
                            rooms[cell] = index as u16;
                        }
                    }
                }
            }
        }
        Self {
            world,
            sector,
            origin: lo,
            width,
            rooms,
            albedo,
        }
    }

    /// The materials a world uses, to fill the table [`Self::new`] takes.
    pub fn materials(world: &GridWorld) -> Vec<Option<ResourceId>> {
        let mut out = vec![None];
        let mut add = |m: Option<ResourceId>| {
            if !out.contains(&m) {
                out.push(m);
            }
        };
        for room in &world.rooms {
            add(room.defaults.wall_material);
            for (_, sector) in room.leaves() {
                for surface in [sector.ground_floor(), sector.ground_ceiling()]
                    .into_iter()
                    .flatten()
                {
                    add(surface.tri_a.material);
                    add(surface.tri_b.and_then(|t| t.material));
                }
                add(sector.ground_wall_material());
            }
        }
        out
    }

    /// The leaf under world runtime point `(x, z)` and its room.
    fn leaf(&self, x: i32, z: i32) -> Option<(usize, &'a GridSector)> {
        let click = [
            (x * CLICKS_PER_SECTOR).div_euclid(self.sector),
            (z * CLICKS_PER_SECTOR).div_euclid(self.sector),
        ];
        let cell = [
            click[0].div_euclid(CLICKS_PER_SECTOR) - self.origin[0],
            click[1].div_euclid(CLICKS_PER_SECTOR) - self.origin[1],
        ];
        if cell[0] < 0 || cell[1] < 0 || cell[0] as usize >= self.width {
            return None;
        }
        let room = usize::from(
            *self
                .rooms
                .get(cell[1] as usize * self.width + cell[0] as usize)?,
        );
        let (_, sector) = self.world.rooms.get(room)?.leaf_at(click)?;
        Some((room, sector))
    }

    /// Base colour (0..=255 a channel) of the surface hit at `at` with
    /// normal `n`: a floor's or ceiling's material, or a wall's, chosen the
    /// way the cook chooses a derived wall's (the sector behind it, the one
    /// in front, the room's default, then the floor behind it).
    pub fn colour(&self, at: [i32; 3], n: [f64; 3]) -> Option<[f64; 3]> {
        let step = |k: f64| {
            (
                at[0] + (n[0] * k).round() as i32,
                at[2] + (n[2] * k).round() as i32,
            )
        };
        let material = if n[1] > 0.5 {
            let (x, z) = step(-1.0);
            self.leaf(x, z)?.1.ground_floor()?.tri_a.material
        } else if n[1] < -0.5 {
            let (x, z) = step(-1.0);
            self.leaf(x, z)?.1.ground_ceiling()?.tri_a.material
        } else {
            let (bx, bz) = step(-2.0);
            let (fx, fz) = step(2.0);
            let behind = self.leaf(bx, bz);
            let front = self.leaf(fx, fz);
            behind
                .and_then(|(_, s)| s.ground_wall_material())
                .or_else(|| front.and_then(|(_, s)| s.ground_wall_material()))
                .or_else(|| {
                    behind
                        .or(front)
                        .and_then(|(room, _)| self.world.rooms[room].defaults.wall_material)
                })
                .or_else(|| behind.and_then(|(_, s)| s.ground_floor()?.tri_a.material))
        };
        let albedo = self.albedo.get(&material).copied().unwrap_or([0.5; 3]);
        Some(albedo.map(|c| f64::from(c) * 255.0))
    }
}

/// Faces of the atlas in its left-to-right order.
const FACES: [CubeFace; 6] = [
    CubeFace::PositiveX,
    CubeFace::NegativeX,
    CubeFace::PositiveY,
    CubeFace::NegativeY,
    CubeFace::PositiveZ,
    CubeFace::NegativeZ,
];

/// Eye height over the floor at the chunk's centre, runtime units: about a
/// standing body's eyes.
const EYE_HEIGHT: i32 = 60;

/// How far a ray looks, runtime units. Points stay well inside the
/// collision coordinate limit.
const REACH: f64 = 9000.0;

/// Horizon colour of a chunk without fog.
const NO_FOG_HORIZON: [f64; 3] = [96.0, 104.0, 120.0];

/// A baked chunk sky.
pub struct ChunkSky {
    /// The chunk baked from.
    pub chunk: usize,
    /// The chunk and its neighbours, left out of the bake.
    pub ring: Vec<usize>,
    /// Where the rays start, world runtime units.
    pub eye: [i32; 3],
    /// Colour of every texel, face-major (`face * 256 * 256 + y * 256 + x`).
    pub rgb: Vec<[u8; 3]>,
    /// Rays that hit geometry.
    pub hits: usize,
}

impl ChunkSky {
    /// The atlas as one 1536x256 row-major image, faces side by side.
    pub fn atlas_rgb(&self) -> Vec<[u8; 3]> {
        let (w, h) = (
            usize::from(CUBE_SKY_FACE_WIDTH),
            usize::from(CUBE_SKY_FACE_HEIGHT),
        );
        let mut out = vec![[0; 3]; 6 * w * h];
        for face in 0..6 {
            for y in 0..h {
                for x in 0..w {
                    out[y * 6 * w + face * w + x] = self.rgb[(face * h + y) * w + x];
                }
            }
        }
        out
    }

    /// The atlas as a 4bpp PSXT texture with one 16-colour palette per face,
    /// the shape the cube sky takes.
    pub fn to_psxt(&self) -> Result<Vec<u8>, String> {
        self.to_psxt_with(&shared_palettes(core::slice::from_ref(self))?)
    }

    /// [`Self::to_psxt`] over the given six face palettes (see
    /// [`shared_palettes`]): every texel takes the nearest colour of its
    /// face's palette.
    pub fn to_psxt_with(&self, palettes: &[Vec<[u8; 3]>; 6]) -> Result<Vec<u8>, String> {
        let (w, h) = (
            usize::from(CUBE_SKY_FACE_WIDTH),
            usize::from(CUBE_SKY_FACE_HEIGHT),
        );
        let mut indices = vec![0u8; 6 * w * h];
        for (face, palette) in palettes.iter().enumerate() {
            for y in 0..h {
                for x in 0..w {
                    let texel = self.rgb[(face * h + y) * w + x];
                    indices[y * 6 * w + face * w + x] = nearest(palette, texel);
                }
            }
        }
        psxed_tex::encode_indexed_psxt_with_clut_rows(
            CUBE_SKY_FACE_WIDTH * 6,
            CUBE_SKY_FACE_HEIGHT,
            psxed_format::texture::Depth::Bit4,
            &indices,
            palettes.as_slice(),
            false,
        )
        .map_err(|e| format!("encode the sky: {e}"))
    }
}

/// One 16-colour palette per cube face for a whole set of chunk skies. With
/// every chunk's sky on the same palettes, the runtime can stream a new sky's
/// texels over the shown one row by row: there is no palette to swap, so no
/// frame shows texels under the wrong colours, and no second copy of the sky
/// is needed in VRAM.
pub fn shared_palettes(skies: &[ChunkSky]) -> Result<[Vec<[u8; 3]>; 6], String> {
    let (w, h) = (
        usize::from(CUBE_SKY_FACE_WIDTH),
        usize::from(CUBE_SKY_FACE_HEIGHT),
    );
    // A sample of every sky's face, kept to about a quarter million texels.
    let stride = (skies.len() * w * h / 262_144).max(1);
    let mut palettes: [Vec<[u8; 3]>; 6] = Default::default();
    for (face, palette) in palettes.iter_mut().enumerate() {
        let sample: Vec<[u8; 3]> = skies
            .iter()
            .flat_map(|sky| sky.rgb[face * w * h..(face + 1) * w * h].iter().copied())
            .step_by(stride)
            .collect();
        *palette = psxed_tex::quantize_rgb(&sample, 16)
            .map_err(|e| format!("face {face}: {e}"))?
            .0;
    }
    Ok(palettes)
}

/// Index of the colour of `palette` nearest `c`.
fn nearest(palette: &[[u8; 3]], c: [u8; 3]) -> u8 {
    let distance = |p: &[u8; 3]| {
        (0..3)
            .map(|i| (i32::from(p[i]) - i32::from(c[i])).pow(2))
            .sum::<i32>()
    };
    palette
        .iter()
        .enumerate()
        .min_by_key(|(_, p)| distance(p))
        .map_or(0, |(i, _)| i as u8)
}

/// Direction through face-local `(u, v)` in `-1..=1` of `face`: the inverse
/// of [`psx_engine::sky::cube_face_uv_q12`].
pub fn cube_direction(face: CubeFace, u: f64, v: f64) -> [f64; 3] {
    match face {
        CubeFace::PositiveX => [1.0, -v, -u],
        CubeFace::NegativeX => [-1.0, -v, u],
        CubeFace::PositiveY => [u, 1.0, v],
        CubeFace::NegativeY => [u, -1.0, -v],
        CubeFace::PositiveZ => [u, -v, 1.0],
        CubeFace::NegativeZ => [-u, -v, -1.0],
    }
}

fn point_trace(
    provider: &mut GridTraceProvider<'_, '_>,
    a: [i32; 3],
    b: [i32; 3],
) -> CollisionTrace {
    let mut out = CollisionTrace::unobstructed(RoomPoint::from_array(b));
    provider.trace_into(
        CollisionTraceQuery::point(RoomPoint::from_array(a), RoomPoint::from_array(b)),
        &mut out,
    );
    out
}

/// Bake the sky of room `chunk` of `cooked`: the chunk and every room one
/// portal away are left out, the rest is traced from the chunk's centre.
pub fn bake_chunk_sky(cooked: &CookedLevel, chunk: usize) -> Result<ChunkSky, String> {
    bake_chunk_sky_with(cooked, chunk, None)
}

/// [`bake_chunk_sky`] with hits coloured by `colours` (flat graybox blues
/// without).
pub fn bake_chunk_sky_with(
    cooked: &CookedLevel,
    chunk: usize,
    colours: Option<&WorldColours<'_>>,
) -> Result<ChunkSky, String> {
    let rooms = cooked.level.rooms.len();
    if chunk >= rooms {
        return Err(format!("room {chunk} does not exist ({rooms} rooms)"));
    }
    let chunk_words: Vec<Vec<u32>> = cooked.chunks.iter().map(|c| aligned_copy(c)).collect();
    let views = chunk_words
        .iter()
        .zip(&cooked.chunks)
        .map(|(words, bytes)| {
            RoomView::parse(words_as_bytes(words, bytes.len()))
                .map(|view| Some(view.col))
                .map_err(|e| format!("room chunk does not parse: {e:?}"))
        })
        .collect::<Result<Vec<Option<CollisionView<'_>>>, String>>()?;
    let core_words = aligned_copy(&cooked.core);
    let level = LevelView::parse(words_as_bytes(&core_words, cooked.core.len()))
        .map_err(|e| format!("level core does not parse: {e:?}"))?;
    let mut ring = vec![chunk];
    for portal in level.room_portals(chunk) {
        let dest = usize::from(portal.dest);
        if !ring.contains(&dest) {
            ring.push(dest);
        }
    }
    let s = i32::from(cooked.level.sector_units);
    let record = cooked.level.rooms[chunk];
    let (cx, cz) = (
        record.origin[0] + i32::from(record.width) * s / 2,
        record.origin[2] + i32::from(record.depth) * s / 2,
    );
    // The eye stands on whatever is under the chunk's centre in the whole
    // level.
    let eye_y = {
        let set = ResidentRooms::from_collisions(level, &views)
            .ok_or("the level's rooms do not fit a resident set")?;
        let world = set.collision();
        let mut provider = GridTraceProvider::new(&world);
        let top = record.origin[1] + i32::from(record.max_y) + EYE_HEIGHT;
        let bottom = record.origin[1] + i32::from(record.min_y) - EYE_HEIGHT;
        let down = point_trace(&mut provider, [cx, top, cz], [cx, bottom, cz]);
        if down.hit() && !down.start_solid {
            down.end.y + EYE_HEIGHT
        } else {
            record.origin[1] + EYE_HEIGHT
        }
    };
    let eye = [cx, eye_y, cz];
    // The level without the ring: its rooms stay resident (their chunks are
    // what they are) but their footprints move far outside the level, where
    // no ray reaches. Leaving them out of the resident set instead would make
    // them stand in as solid boxes around the eye.
    let mut core = cooked.level.clone();
    for &room in &ring {
        core.rooms[room].origin = [40_000, 30_000, 40_000];
    }
    let cut_bytes = core.try_to_bytes()?;
    let cut_words = aligned_copy(&cut_bytes);
    let cut = LevelView::parse(words_as_bytes(&cut_words, cut_bytes.len()))
        .map_err(|e| format!("cut level core does not parse: {e:?}"))?;
    let set = ResidentRooms::from_collisions(cut, &views)
        .ok_or("the level's rooms do not fit a resident set")?;
    let world = set.collision();
    let mut provider = GridTraceProvider::new(&world);

    // A room without fog bakes with no haze over a neutral horizon colour.
    let fogged = record.flags & psx_grid::ROOM_FOG != 0;
    let fog = if fogged {
        record.fog_rgb.map(f64::from)
    } else {
        NO_FOG_HORIZON
    };
    let (fog_near, fog_far) = if fogged {
        (f64::from(record.fog_near), f64::from(record.fog_far))
    } else {
        (f64::MAX, f64::MAX)
    };
    let light = {
        let l = [0.4f64, 0.8, 0.3];
        let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        [l[0] / n, l[1] / n, l[2] / n]
    };
    let (w, h) = (
        usize::from(CUBE_SKY_FACE_WIDTH),
        usize::from(CUBE_SKY_FACE_HEIGHT),
    );
    let mut rgb = vec![[0u8; 3]; 6 * w * h];
    let mut hits = 0;
    for (index, face) in FACES.into_iter().enumerate() {
        for y in 0..h {
            for x in 0..w {
                let u = (x as f64 + 0.5) / w as f64 * 2.0 - 1.0;
                let v = (y as f64 + 0.5) / h as f64 * 2.0 - 1.0;
                let d = cube_direction(face, u, v);
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                let d = d.map(|c| c / len);
                let end = [
                    eye[0] + (d[0] * REACH) as i32,
                    eye[1] + (d[1] * REACH) as i32,
                    eye[2] + (d[2] * REACH) as i32,
                ];
                let t = point_trace(&mut provider, eye, end);
                let colour = if t.hit() && !t.start_solid {
                    hits += 1;
                    let distance = REACH * f64::from(t.fraction_q12) / 4096.0;
                    let n = t.normal_q12.map(|c| f64::from(c) / 4096.0);
                    let lambert = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.0);
                    let at = [t.end.x, t.end.y, t.end.z];
                    let base = colours.and_then(|c| c.colour(at, n)).unwrap_or(
                        // Tops a little lighter than sides, as the graybox reads.
                        if n[1] > 0.7 {
                            [70.0, 92.0, 150.0]
                        } else {
                            [44.0, 58.0, 110.0]
                        },
                    );
                    let lit = base.map(|c| c * (0.45 + 0.55 * lambert));
                    let k = if fogged {
                        ((distance - fog_near) / (fog_far - fog_near).max(1.0)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    [0, 1, 2].map(|i| lit[i] + (fog[i] - lit[i]) * k)
                } else if d[1] < 0.0 {
                    // Below the horizon with nothing there: the void under
                    // the level, darker than the fog.
                    fog.map(|c| c * 0.6)
                } else {
                    let k = d[1].clamp(0.0, 1.0).powf(0.6);
                    let top = [fog[0] * 0.35, fog[1] * 0.45, fog[2] * 0.7];
                    [0, 1, 2].map(|i| fog[i] + (top[i] - fog[i]) * k)
                };
                rgb[(index * h + y) * w + x] = colour.map(|c| c.clamp(0.0, 255.0) as u8);
            }
        }
    }
    Ok(ChunkSky {
        chunk,
        ring,
        eye,
        rgb,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cook::tests::cook;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::world::GridWorld;
    use psx_engine::sky::{cube_face, cube_face_uv_q12};

    #[test]
    fn cube_directions_land_back_on_their_face_and_texel() {
        for face in FACES {
            for (u, v) in [(0.0, 0.0), (0.5, -0.25), (-0.9, 0.9)] {
                let d = cube_direction(face, u, v);
                let q = d.map(|c| (c * 4096.0) as i32);
                assert_eq!(cube_face(q), face);
                let uv = cube_face_uv_q12(q, face);
                assert!((f64::from(uv[0]) / 4096.0 - u).abs() < 0.01, "{face:?} u");
                assert!((f64::from(uv[1]) / 4096.0 - v).abs() < 0.01, "{face:?} v");
            }
        }
    }

    /// A floored room at `origin` (world sectors), its cells raised to
    /// `height`.
    fn block(origin: [i32; 2], w: usize, d: usize, height: i32) -> GridRoom {
        let mut room = GridRoom::new_derived("block", w, d);
        room.origin = origin;
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
    fn the_bake_sees_far_rooms_and_not_its_own_ring() {
        // A floor to stand on, a raised block beside it (its neighbour, so
        // in the ring), and a walled tower far to the east (not in the ring).
        let mut tower = block([40, 0], 2, 2, 0);
        // A roofless room closes its edges only with an open wall height.
        tower.defaults.open_wall = 8192;
        let world = GridWorld {
            rooms: vec![block([0, 0], 2, 2, 0), block([2, 0], 2, 2, 8192), tower],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let sky = bake_chunk_sky(&cooked, 0).unwrap();
        assert!(
            sky.ring.contains(&1) && !sky.ring.contains(&2),
            "{:?}",
            sky.ring
        );
        let (w, h) = (256, 256);
        let texel = |face: usize, x: usize, y: usize| sky.rgb[(face * h + y) * w + x];
        // Looking east (+X, face 0) just above the horizon meets the far
        // block; looking west (-X, face 1) there is only sky. The near block
        // sits east too, but it is in the ring.
        let east = texel(0, 128, 120);
        let west = texel(1, 128, 120);
        assert_ne!(east, west);
        assert!(sky.hits > 0);
        let psxt = sky.to_psxt().unwrap();
        let texture = psx_asset::Texture::from_bytes(&psxt).unwrap();
        assert_eq!([texture.width(), texture.height()], [1536, 256]);
        assert_eq!(texture.clut_entries(), 96);
    }
}
