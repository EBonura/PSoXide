//! Baking the far cards of a cooked level.
//!
//! Each room that draws anything gets a card ([`psx_grid::render::cards`]):
//! a vertical rectangle standing at the room's centre on its lowest
//! geometry, as wide as the room's horizontal reach and as tall as the room
//! is high. Its picture is rendered here from the room's own cooked quads,
//! once for each of [`VIEWS`] headings: every quad that faces the eye is
//! projected along the viewing direction onto the card's plane (a shear
//! orthographic projection, the eye pitched down by `PITCH_TAN_Q12`),
//! depth-tested against the others, shaded with the quad's material colour
//! times its baked light, and averaged down from [`SUPERSAMPLE`] samples a
//! texel. Texels nothing covers stay transparent.
//!
//! The picture is quantised to 15 colours plus transparent and stored as one
//! disc sector, exactly the `16 x 64` halfword VRAM rectangle of its slot:
//! 63 rows of 4-bit texels and, in the last row, the palette.

use psx_grid::render::{
    PICTURE_ROWS, PITCH_TAN_Q12, SLOT_BYTES, TEXELS, VIEWS, VIEW_COS_Q12, VIEW_SIN_Q12,
};
use psx_grid::{Quad, QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_TRI};

use super::CookedLevel;

/// Samples across and down one texel of a picture.
pub const SUPERSAMPLE: usize = 4;
/// Fewest samples of a texel's `SUPERSAMPLE * SUPERSAMPLE` that must be
/// covered for the texel to be opaque.
const COVERAGE_MIN: usize = SUPERSAMPLE * SUPERSAMPLE / 4;

const WIDE: usize = TEXELS as usize;
const HIGH: usize = PICTURE_ROWS as usize;

/// One room's card.
#[derive(Clone, Debug)]
pub struct BakedCard {
    /// Where it stands, world runtime units `[x, z]`.
    pub centre: [i32; 2],
    /// Height of its bottom edge.
    pub base: i32,
    /// Its height.
    pub height: i32,
    /// Half its width.
    pub half_width: i32,
    /// [`VIEWS`] sectors, one picture each in view order.
    pub sectors: Vec<u8>,
    /// The pictures as RGBA, `WIDE * HIGH` texels each, after quantising.
    pub preview: Vec<[u8; 4]>,
}

/// What the bake made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardsReport {
    /// Rooms with a card.
    pub cards: usize,
    /// Rooms that draw nothing.
    pub empty: usize,
    /// Bytes of pictures.
    pub bytes: usize,
    /// Range of the cards' heights and half widths, runtime units.
    pub height: (i32, i32),
    pub half_width: (i32, i32),
}

/// A triangle of the room in world space with its corner colours.
struct Triangle {
    position: [[f32; 3]; 3],
    colour: [[f32; 3]; 3],
    /// Index into the room's quads, for the facing test.
    quad: usize,
}

/// Corner colour: the material's mean colour times the baked light of the
/// corner (128 is neutral), channels 0 to 255.
fn shade(albedo: [f32; 3], colour: u32) -> [f32; 3] {
    [0, 1, 2].map(|i| albedo[i] * 255.0 * ((colour >> (8 * i)) & 255) as f32 / 128.0)
}

/// Whether `quad` faces an eye in direction `towards_eye` (a unit vector,
/// world axes `x`, `y`, `z`) from the card.
fn faces(quad: &Quad, towards_eye: [f32; 3]) -> bool {
    if quad.flags & QUAD_DOUBLE != 0 {
        return true;
    }
    if quad.flags & QUAD_AXIS != 0 {
        let axis = usize::from((quad.flags >> QUAD_AXIS_SHIFT) & 3);
        let c = towards_eye[axis.min(2)];
        return (if quad.flags & QUAD_AXIS_NEG != 0 {
            -c
        } else {
            c
        }) > 0.0;
    }
    let n = quad.n.map(f32::from);
    n[0] * towards_eye[0] + n[1] * towards_eye[1] + n[2] * towards_eye[2] > 0.0
}

/// The room's triangles in world space, and its quads.
fn triangles(cooked: &CookedLevel, albedo: &[[f32; 3]], room: usize) -> (Vec<Triangle>, Vec<Quad>) {
    let table = &cooked.tables[room];
    let origin = cooked.level.rooms[room].origin;
    let world = |v: u16| {
        let vtx = table.vertices[usize::from(v)];
        [
            (origin[0] + i32::from(vtx.x)) as f32,
            (origin[1] + i32::from(vtx.y)) as f32,
            (origin[2] + i32::from(vtx.z)) as f32,
        ]
    };
    let mut out = Vec::new();
    for (index, quad) in table.quads.iter().enumerate() {
        let slot = table
            .material_use
            .get(usize::from(quad.material))
            .map_or(0, |&m| usize::from(m));
        let base = albedo.get(slot).copied().unwrap_or([0.5; 3]);
        let corner = |k: usize| (world(quad.v[k]), shade(base, quad.color[k]));
        let (a, b, c) = (corner(0), corner(1), corner(2));
        out.push(Triangle {
            position: [a.0, b.0, c.0],
            colour: [a.1, b.1, c.1],
            quad: index,
        });
        if quad.flags & QUAD_TRI == 0 {
            let d = corner(3);
            out.push(Triangle {
                position: [a.0, c.0, d.0],
                colour: [a.1, c.1, d.1],
                quad: index,
            });
        }
    }
    (out, table.quads.clone())
}

/// The sample grid of one view: depth and colour per sample.
struct Raster {
    depth: Vec<f32>,
    colour: Vec<[f32; 3]>,
}

impl Raster {
    const WIDE: usize = WIDE * SUPERSAMPLE;
    const HIGH: usize = HIGH * SUPERSAMPLE;

    fn new() -> Self {
        Self {
            depth: vec![f32::INFINITY; Self::WIDE * Self::HIGH],
            colour: vec![[0.0; 3]; Self::WIDE * Self::HIGH],
        }
    }

    /// Fill a triangle given as `(x, y, depth)` sample coordinates.
    fn fill(&mut self, p: [[f32; 3]; 3], colour: [[f32; 3]; 3]) {
        let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| {
            (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
        };
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-6 {
            return;
        }
        let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
        let x0 = (lo(f32::min, 0) - 0.5).floor().max(0.0) as usize;
        let x1 = ((lo(f32::max, 0) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, Self::WIDE as isize) as usize;
        let y0 = (lo(f32::min, 1) - 0.5).floor().max(0.0) as usize;
        let y1 = ((lo(f32::max, 1) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, Self::HIGH as isize) as usize;
        for y in y0..y1 {
            let sy = y as f32 + 0.5;
            for x in x0..x1 {
                let sx = x as f32 + 0.5;
                let w0 = edge(p[1], p[2], sx, sy) / area;
                let w1 = edge(p[2], p[0], sx, sy) / area;
                let w2 = 1.0 - w0 - w1;
                // A sample on an edge belongs to both neighbours.
                let eps = -1e-4;
                if w0 < eps || w1 < eps || w2 < eps {
                    continue;
                }
                let depth = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
                let at = y * Self::WIDE + x;
                if depth < self.depth[at] {
                    self.depth[at] = depth;
                    self.colour[at] = [0, 1, 2]
                        .map(|i| w0 * colour[0][i] + w1 * colour[1][i] + w2 * colour[2][i]);
                }
            }
        }
    }

    /// The texel grid: mean colour of the covered samples, or `None`.
    fn texels(&self) -> Vec<Option<[f32; 3]>> {
        let mut out = vec![None; WIDE * HIGH];
        for ty in 0..HIGH {
            for tx in 0..WIDE {
                let (mut sum, mut count) = ([0.0f32; 3], 0usize);
                for sy in 0..SUPERSAMPLE {
                    for sx in 0..SUPERSAMPLE {
                        let at = (ty * SUPERSAMPLE + sy) * Self::WIDE + tx * SUPERSAMPLE + sx;
                        if self.depth[at].is_finite() {
                            count += 1;
                            for (total, c) in sum.iter_mut().zip(self.colour[at]) {
                                *total += c;
                            }
                        }
                    }
                }
                if count >= COVERAGE_MIN {
                    out[ty * WIDE + tx] = Some(sum.map(|c| c / count as f32));
                }
            }
        }
        out
    }
}

/// The sector of a picture: 63 rows of packed 4-bit texels, then the palette.
fn encode(indices: &[u8], palette: &[[u8; 3]]) -> Vec<u8> {
    let mut out = vec![0u8; SLOT_BYTES];
    for row in 0..HIGH {
        for pair in 0..WIDE / 2 {
            let (a, b) = (
                indices[row * WIDE + 2 * pair],
                indices[row * WIDE + 2 * pair + 1],
            );
            out[row * (WIDE / 2) + pair] = (a & 15) | (b << 4);
        }
    }
    let clut = HIGH * (WIDE / 2);
    for (k, c) in palette.iter().enumerate().take(16) {
        // Index 0 is transparent: the entry is 0. Every other entry has the
        // semi-transparency bit set, so it is opaque unless the primitive
        // blends, and is never 0 whatever its colour.
        let word: u16 = if k == 0 {
            0
        } else {
            0x8000
                | (u16::from(c[0]) >> 3)
                | (u16::from(c[1]) >> 3) << 5
                | (u16::from(c[2]) >> 3) << 10
        };
        out[clut + 2 * k..clut + 2 * k + 2].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// Bake one room's card, `None` when the room draws nothing.
pub fn bake_room(cooked: &CookedLevel, albedo: &[[f32; 3]], room: usize) -> Option<BakedCard> {
    let (tris, quads) = triangles(cooked, albedo, room);
    if tris.is_empty() {
        return None;
    }
    let record = &cooked.level.rooms[room];
    let sector = i32::from(cooked.level.sector_units);
    let width = usize::from(record.width);
    let owns = |cx: usize, cz: usize| {
        cooked.level.occupancy.get(room).is_none_or(|bits| {
            let bit = cz * width + cx;
            bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
        })
    };
    // The centre of the sectors the room owns.
    let (mut sx, mut sz, mut n) = (0i64, 0i64, 0i64);
    for cz in 0..usize::from(record.depth) {
        for cx in 0..width {
            if owns(cx, cz) {
                sx += i64::from(record.origin[0]) + (cx as i64) * i64::from(sector);
                sz += i64::from(record.origin[2]) + (cz as i64) * i64::from(sector);
                n += 1;
            }
        }
    }
    if n == 0 {
        return None;
    }
    let centre = [(sx / n) as i32 + sector / 2, (sz / n) as i32 + sector / 2];
    // Horizontal reach and vertical extent over everything the room draws,
    // so no quad falls outside any view.
    let (mut reach, mut low, mut high) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
    for t in &tris {
        for p in &t.position {
            let (dx, dz) = (p[0] - centre[0] as f32, p[2] - centre[1] as f32);
            reach = reach.max((dx * dx + dz * dz).sqrt());
            low = low.min(p[1]);
            high = high.max(p[1]);
        }
    }
    let tan = PITCH_TAN_Q12 as f32 / 4096.0;
    let half_width = reach.ceil().max(8.0);
    // The shear lifts what is behind the card's plane by up to `reach * tan`
    // and drops what is in front of it by as much.
    let base = (low - half_width * tan).floor();
    let top = (high + half_width * tan).ceil();
    let height = (top - base).max(8.0);
    let mut sectors = Vec::with_capacity(VIEWS * SLOT_BYTES);
    let mut preview = Vec::with_capacity(VIEWS * WIDE * HIGH);
    let pitch = f32::atan(tan);
    for view in 0..VIEWS {
        let (cos, sin) = (
            f32::from(VIEW_COS_Q12[view]) / 4096.0,
            f32::from(VIEW_SIN_Q12[view]) / 4096.0,
        );
        // From the card to the eye: out along the heading and up by the
        // pitch.
        let towards_eye = [cos * pitch.cos(), pitch.sin(), sin * pitch.cos()];
        let mut raster = Raster::new();
        let (sw, sh) = (Raster::WIDE as f32, Raster::HIGH as f32);
        for t in &tris {
            if !faces(&quads[t.quad], towards_eye) {
                continue;
            }
            // Sample coordinates: along the card's right (`(sin, -cos)`
            // in `(x, z)`: the right of a view looking along `-(cos, sin)`),
            // down from the card's top, and the depth behind its plane.
            let project = |p: [f32; 3]| {
                let (dx, dz) = (p[0] - centre[0] as f32, p[2] - centre[1] as f32);
                let u = dx * sin - dz * cos;
                let behind = -(dx * cos + dz * sin);
                let v = p[1] + behind * tan;
                [
                    (u + half_width) / (2.0 * half_width) * sw,
                    (base + height - v) / height * sh,
                    behind,
                ]
            };
            raster.fill(t.position.map(project), t.colour);
        }
        let texels = raster.texels();
        let rgba: Vec<[u8; 4]> = texels
            .iter()
            .map(|t| match t {
                Some(c) => [0, 1, 2].map(|i| c[i].clamp(0.0, 255.0) as u8).into_rgba(),
                None => [0, 0, 0, 0],
            })
            .collect();
        let (palette, indices) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
            .unwrap_or_else(|_| (vec![[0; 3]; 16], vec![0; WIDE * HIGH]));
        sectors.extend_from_slice(&encode(&indices, &palette));
        preview.extend(indices.iter().map(|&i| {
            if i == 0 {
                [0, 0, 0, 0]
            } else {
                let c = palette.get(usize::from(i)).copied().unwrap_or([0; 3]);
                // The colour the palette entry will show: 5 bits a channel.
                [c[0] & 0xf8, c[1] & 0xf8, c[2] & 0xf8, 255]
            }
        }));
    }
    Some(BakedCard {
        centre,
        base: base as i32,
        height: height as i32,
        half_width: half_width as i32,
        sectors,
        preview,
    })
}

/// Colour channels to RGBA with full alpha.
trait IntoRgba {
    fn into_rgba(self) -> [u8; 4];
}

impl IntoRgba for [u8; 3] {
    fn into_rgba(self) -> [u8; 4] {
        [self[0], self[1], self[2], 255]
    }
}

/// Cells across one side of the grid the cards' centres are placed on to
/// order them along a Hilbert curve.
const ORDER_GRID: u32 = 256;

/// Position of cell `(x, y)` along the Hilbert curve through an
/// `ORDER_GRID` square: cells close on the curve are close in the plane.
fn hilbert_index(mut x: u32, mut y: u32) -> u32 {
    let mut index = 0;
    let mut step = ORDER_GRID / 2;
    while step > 0 {
        let rx = u32::from(x & step != 0);
        let ry = u32::from(y & step != 0);
        index += step * step * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = ORDER_GRID - 1 - x;
                y = ORDER_GRID - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        step /= 2;
    }
    index
}

/// Rooms in one tile of the disc layout, and so the sectors between a room's
/// picture of one view and its picture of the next. A tile holds the eight
/// views of eight rooms that stand together, one view after another: the
/// eight pictures of one view are eight consecutive sectors, a single read.
pub const TILE_ROOMS: u32 = 8;

/// Where the pictures of the rooms go on the disc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscLayout {
    /// Per room, the sector (from the start of the pictures) of its picture
    /// of view 0, or [`u32::MAX`] for a room without a card. Its picture of
    /// view `k` is `TILE_ROOMS * k` sectors on.
    pub offsets: Vec<u32>,
    /// Sectors the pictures take.
    pub sectors: u32,
}

/// Lay the pictures out: the rooms ordered along a Hilbert curve through
/// their centres, cut into tiles of [`TILE_ROOMS`] consecutive rooms, so the
/// rooms of a tile stand together in the world and one read brings the
/// pictures of a whole tile for one view, and tiles that stand near each
/// other are near each other on the disc.
pub fn disc_layout(centres: &[Option<[i32; 2]>]) -> DiscLayout {
    let present: Vec<(usize, [i32; 2])> = centres
        .iter()
        .enumerate()
        .filter_map(|(room, centre)| centre.map(|centre| (room, centre)))
        .collect();
    let mut offsets = vec![u32::MAX; centres.len()];
    if present.is_empty() {
        return DiscLayout {
            offsets,
            sectors: 0,
        };
    }
    let lo = |k: usize| present.iter().map(|p| p.1[k]).min().unwrap_or(0);
    let hi = |k: usize| present.iter().map(|p| p.1[k]).max().unwrap_or(0);
    let side = i64::from((hi(0) - lo(0)).max(hi(1) - lo(1)).max(1));
    let cell =
        |value: i32, low: i32| (i64::from(value - low) * i64::from(ORDER_GRID - 1) / side) as u32;
    let mut keyed: Vec<(u32, usize)> = present
        .iter()
        .map(|&(room, centre)| {
            (
                hilbert_index(cell(centre[0], lo(0)), cell(centre[1], lo(1))),
                room,
            )
        })
        .collect();
    keyed.sort_unstable();
    let views = VIEWS as u32;
    for (position, &(_, room)) in keyed.iter().enumerate() {
        let (tile, place) = (position as u32 / TILE_ROOMS, position as u32 % TILE_ROOMS);
        offsets[room] = tile * TILE_ROOMS * views + place;
    }
    let tiles = (keyed.len() as u32).div_ceil(TILE_ROOMS);
    DiscLayout {
        offsets,
        sectors: tiles * TILE_ROOMS * views,
    }
}

/// Bake every room's card. `albedo` is the mean colour of each level
/// material in slot order (`cooked.materials`).
pub fn bake(cooked: &CookedLevel, albedo: &[[f32; 3]]) -> (Vec<Option<BakedCard>>, CardsReport) {
    let rooms = cooked.level.rooms.len();
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 4)
        .min(rooms.max(1));
    let mut cards: Vec<Option<BakedCard>> = vec![None; rooms];
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..rooms)
                        .step_by(threads)
                        .map(|room| (room, bake_room(cooked, albedo, room)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for worker in workers {
            for (room, card) in worker.join().expect("a card bake thread panicked") {
                cards[room] = card;
            }
        }
    });
    let mut report = CardsReport {
        height: (i32::MAX, 0),
        half_width: (i32::MAX, 0),
        ..CardsReport::default()
    };
    for card in &cards {
        match card {
            Some(card) => {
                report.cards += 1;
                report.bytes += card.sectors.len();
                report.height = (
                    report.height.0.min(card.height),
                    report.height.1.max(card.height),
                );
                report.half_width = (
                    report.half_width.0.min(card.half_width),
                    report.half_width.1.max(card.half_width),
                );
            }
            None => report.empty += 1,
        }
    }
    (cards, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cook::tests::cook;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::world::GridWorld;

    /// A floored room at `origin` (world sectors), its cells raised to the
    /// heights given per column (a row of `heights.len()` cells).
    fn strip(origin: [i32; 2], heights: &[i32]) -> GridRoom {
        let mut room = GridRoom::new_derived("strip", heights.len(), 1);
        room.origin = origin;
        for (x, &height) in heights.iter().enumerate() {
            room.sector_or_insert(x, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(height, None)));
        }
        room
    }

    /// The palette indices of a picture sector, row-major `TEXELS x
    /// PICTURE_ROWS`, and its palette.
    fn decode(sector: &[u8]) -> (Vec<u8>, Vec<u16>) {
        let mut indices = Vec::new();
        for row in 0..HIGH {
            for x in 0..WIDE {
                let byte = sector[row * (WIDE / 2) + x / 2];
                indices.push(if x % 2 == 0 { byte & 15 } else { byte >> 4 });
            }
        }
        let clut = HIGH * (WIDE / 2);
        let palette = (0..16)
            .map(|k| u16::from_le_bytes([sector[clut + 2 * k], sector[clut + 2 * k + 1]]))
            .collect();
        (indices, palette)
    }

    fn albedo() -> Vec<[f32; 3]> {
        vec![[0.5, 0.5, 0.5]; 8]
    }

    #[test]
    fn a_room_gets_a_card_standing_on_it_with_a_picture_for_every_heading() {
        let world = GridWorld {
            rooms: vec![strip([4, 2], &[0, 0, 4096, 4096])],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let card = bake_room(&cooked, &albedo(), 0).expect("the room draws something");
        let sector = i32::from(cooked.level.sector_units);
        // The room spans x = 4..8 sectors, z = 2..3: its centre is at the
        // middle of that.
        assert_eq!(card.centre, [6 * sector, 2 * sector + sector / 2]);
        assert!(card.half_width >= 2 * sector, "{}", card.half_width);
        assert!(card.height > 0 && card.base <= 0);
        assert_eq!(card.sectors.len(), VIEWS * SLOT_BYTES);
        assert_eq!(card.preview.len(), VIEWS * WIDE * HIGH);
        for view in 0..VIEWS {
            let (indices, palette) =
                decode(&card.sectors[view * SLOT_BYTES..(view + 1) * SLOT_BYTES]);
            // Index 0 is transparent: its palette entry is 0 and no other is.
            assert_eq!(palette[0], 0);
            assert!(palette[1..].iter().all(|&c| c & 0x8000 != 0), "{palette:?}");
            // Something is drawn, and not everything: the picture's corners
            // stay clear of a low strip of floor.
            let drawn = indices.iter().filter(|&&i| i != 0).count();
            assert!(drawn > 40 && drawn < indices.len(), "view {view}: {drawn}");
            assert_eq!(indices[0], 0, "view {view}: top-left corner");
        }
    }

    #[test]
    fn opposite_headings_see_opposite_faces_of_a_one_sided_wall() {
        // A tall block at the east end: its west wall faces -X.
        let world = GridWorld {
            rooms: vec![strip([0, 0], &[0, 0, 0, 8192])],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let card = bake_room(&cooked, &albedo(), 0).unwrap();
        let view = |k: usize| {
            let (indices, _) = decode(&card.sectors[k * SQ..(k + 1) * SQ]);
            indices.iter().filter(|&&i| i != 0).count()
        };
        const SQ: usize = SLOT_BYTES;
        // View 4 is the eye on the -X side, looking at the west face; view 0
        // is the eye on the +X side, seeing the block only from behind and
        // above.
        assert!(
            view(4) > view(0),
            "west face {} vs east side {}",
            view(4),
            view(0)
        );
    }

    #[test]
    fn rooms_on_a_lattice_are_laid_out_in_tiles_of_neighbours() {
        let mut centres = Vec::new();
        for z in 0..12 {
            for x in 0..16 {
                centres.push(Some([x * 1000, z * 1000]));
            }
        }
        centres[5] = None;
        let layout = disc_layout(&centres);
        let rooms = 12 * 16 - 1;
        assert_eq!(layout.offsets[5], u32::MAX);
        assert_eq!(
            layout.sectors,
            (rooms as u32).div_ceil(TILE_ROOMS) * TILE_ROOMS * VIEWS as u32
        );
        // Every picture of every room has a sector of its own.
        let mut taken = vec![false; layout.sectors as usize];
        for &offset in layout.offsets.iter().filter(|&&o| o != u32::MAX) {
            for view in 0..VIEWS as u32 {
                let sector = (offset + view * TILE_ROOMS) as usize;
                assert!(!taken[sector], "sector {sector} used twice");
                taken[sector] = true;
            }
        }
        // The rooms of a tile stand together: a few lattice steps across.
        let mut tiles: std::collections::BTreeMap<u32, Vec<usize>> = Default::default();
        for (room, &offset) in layout.offsets.iter().enumerate() {
            if offset != u32::MAX {
                tiles
                    .entry(offset / (TILE_ROOMS * VIEWS as u32))
                    .or_default()
                    .push(room);
            }
        }
        for rooms in tiles.values() {
            let (mut x0, mut x1, mut z0, mut z1) = (99, 0, 99, 0);
            for &room in rooms {
                let (x, z) = (room % 16, room / 16);
                x0 = x0.min(x);
                x1 = x1.max(x);
                z0 = z0.min(z);
                z1 = z1.max(z);
            }
            assert!((x1 - x0) + (z1 - z0) <= 9, "{rooms:?}");
        }
    }

    #[test]
    fn a_quad_faces_the_eye_it_looks_at() {
        let up = Quad {
            n: [0, 127, 0],
            ..Quad::default()
        };
        let eye_above = [0.0, 1.0, 0.0];
        let eye_below = [0.0, -1.0, 0.0];
        assert!(faces(&up, eye_above) && !faces(&up, eye_below));
        // An axis-aligned plane looks along its axis, negative when flagged.
        let east = Quad {
            flags: QUAD_AXIS,
            ..Quad::default()
        };
        assert!(faces(&east, [1.0, 0.0, 0.0]) && !faces(&east, [-1.0, 0.0, 0.0]));
        let west = Quad {
            flags: QUAD_AXIS | QUAD_AXIS_NEG,
            ..Quad::default()
        };
        assert!(faces(&west, [-1.0, 0.0, 0.0]) && !faces(&west, [1.0, 0.0, 0.0]));
        // A double sided quad faces everyone.
        let both = Quad {
            flags: QUAD_DOUBLE,
            ..Quad::default()
        };
        assert!(faces(&both, eye_below));
    }

    #[test]
    fn a_picture_decodes_back_to_the_colours_that_were_drawn() {
        let world = GridWorld {
            rooms: vec![strip([0, 0], &[0, 0])],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let card = bake_room(&cooked, &albedo(), 0).unwrap();
        let (indices, palette) = decode(&card.sectors[..SLOT_BYTES]);
        // The preview shows each texel at its palette entry's 5-bit colour.
        for (k, texel) in card.preview[..WIDE * HIGH].iter().enumerate() {
            let index = usize::from(indices[k]);
            if index == 0 {
                assert_eq!(texel[3], 0);
            } else {
                let word = palette[index];
                let c = |shift: u32| (((word >> shift) & 31) as u8) << 3;
                assert_eq!(&texel[..3], &[c(0), c(5), c(10)]);
            }
        }
    }
}
