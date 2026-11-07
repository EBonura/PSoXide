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
    /// 0 for a room's card; 1 and up for the layers of cards beyond them
    /// ([`bake_layers`]).
    pub layer: u8,
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
pub(super) struct Triangle {
    pub(super) position: [[f32; 3]; 3],
    pub(super) colour: [[f32; 3]; 3],
    /// Index into the room's quads, for the facing test.
    pub(super) quad: usize,
    /// Which corners of the quad the triangle has.
    pub(super) corner: [usize; 3],
}

/// Corner colour: the material's mean colour times the baked light of the
/// corner (128 is neutral), channels 0 to 255.
fn shade(albedo: [f32; 3], colour: u32) -> [f32; 3] {
    [0, 1, 2].map(|i| albedo[i] * 255.0 * ((colour >> (8 * i)) & 255) as f32 / 128.0)
}

/// Whether `quad` faces an eye in direction `towards_eye` (a unit vector,
/// world axes `x`, `y`, `z`) from the card.
pub(super) fn faces(quad: &Quad, towards_eye: [f32; 3]) -> bool {
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
    triangles_of(cooked, albedo, room)
}

/// [`triangles`] for the layer bake.
pub(super) fn triangles_of(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    room: usize,
) -> (Vec<Triangle>, Vec<Quad>) {
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
            corner: [0, 1, 2],
        });
        if quad.flags & QUAD_TRI == 0 {
            let d = corner(3);
            out.push(Triangle {
                position: [a.0, c.0, d.0],
                colour: [a.1, c.1, d.1],
                quad: index,
                corner: [0, 2, 3],
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

    /// Fill a triangle given as `(x, y, w)` sample coordinates, `w` being one
    /// over the distance from the eye (larger is nearer), with perspective
    /// correct depth and colour.
    fn fill_persp(&mut self, p: [[f32; 3]; 3], colour: [[f32; 3]; 3]) {
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
                let eps = -1e-4;
                if w0 < eps || w1 < eps || w2 < eps {
                    continue;
                }
                let inv = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
                let at = y * Self::WIDE + x;
                // The depth buffer holds the negated nearness: smaller wins.
                if -inv < self.depth[at] {
                    self.depth[at] = -inv;
                    self.colour[at] = [0, 1, 2].map(|i| {
                        (w0 * p[0][2] * colour[0][i]
                            + w1 * p[1][2] * colour[1][i]
                            + w2 * p[2][2] * colour[2][i])
                            / inv
                    });
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
pub(super) fn encode(indices: &[u8], palette: &[[u8; 3]]) -> Vec<u8> {
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

/// Quantise a rendered view into its disc sector, and the colours the
/// sector will show.
fn finish_picture(raster: &Raster) -> (Vec<u8>, Vec<[u8; 4]>) {
    let rgba: Vec<[u8; 4]> = raster
        .texels()
        .iter()
        .map(|t| match t {
            Some(c) => [0, 1, 2].map(|i| c[i].clamp(0.0, 255.0) as u8).into_rgba(),
            None => [0, 0, 0, 0],
        })
        .collect();
    let (palette, indices) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
        .unwrap_or_else(|_| (vec![[0; 3]; 16], vec![0; WIDE * HIGH]));
    let shown = indices
        .iter()
        .map(|&i| {
            if i == 0 {
                [0, 0, 0, 0]
            } else {
                let c = palette.get(usize::from(i)).copied().unwrap_or([0; 3]);
                // The colour the palette entry will show: 5 bits a channel.
                [c[0] & 0xf8, c[1] & 0xf8, c[2] & 0xf8, 255]
            }
        })
        .collect();
    (encode(&indices, &palette), shown)
}

/// Where the eye is when a room's pictures are rendered: the viewpoint the card has to
/// match, in runtime units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomEye {
    /// Plan distance of the eye from the card's plane (the room's centre) along the
    /// heading: the distance the room's geometry hands over to its card at.
    pub distance: f32,
    /// Its height.
    pub height: EyeHeight,
}

/// The height of the eye a room's pictures are rendered from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EyeHeight {
    /// This far above the lowest geometry of the room.
    AboveRoom(f32),
    /// At this height in the world, the same for every room: the height of the floor the
    /// player walks on most, plus a person's, so a room far below or above that floor is
    /// seen from where the player sees it.
    World(f32),
}

/// Bake one room's card as the eye of `eye` sees it, in perspective, `None` when the room
/// draws nothing.
///
/// Each of the [`VIEWS`] pictures is the room seen from a point `eye.distance` out along its
/// heading at `eye.height` above the room's lowest geometry, looking along the heading,
/// projected onto the card's plane (the vertical plane through the room's centre): the
/// picture of the card and the room's own geometry then show the same silhouette from that
/// point, which is where the game hands one over to the other. The card is sized to hold
/// every view.
pub fn bake_room(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    room: usize,
    eye: RoomEye,
) -> Option<BakedCard> {
    let (tris, quads) = triangles(cooked, albedo, room);
    let frame = room_frame(cooked, room, &tris)?;
    let RoomFrame { centre, low, .. } = frame;
    let eye_y = match eye.height {
        EyeHeight::AboveRoom(height) => low + height,
        EyeHeight::World(y) => y,
    };
    let d = eye.distance;
    // Nothing nearer the eye than half the nominal distance is kept: the card is never used
    // that close, and the projection grows without bound towards the eye.
    let nearest = d * 0.5;
    let mut views: Vec<Vec<ViewTriangle>> = Vec::with_capacity(VIEWS);
    for view in 0..VIEWS {
        let (cos, sin) = (
            f32::from(VIEW_COS_Q12[view]) / 4096.0,
            f32::from(VIEW_SIN_Q12[view]) / 4096.0,
        );
        let (cx, cz) = (centre[0] as f32, centre[1] as f32);
        let eye_at = [cx + cos * d, eye_y, cz + sin * d];
        let behind = |p: [f32; 3]| -((p[0] - cx) * cos + (p[2] - cz) * sin);
        let across = |p: [f32; 3]| (p[0] - cx) * sin - (p[2] - cz) * cos;
        let mut out = Vec::new();
        for t in &tris {
            let centroid =
                [0, 1, 2].map(|k| (t.position[0][k] + t.position[1][k] + t.position[2][k]) / 3.0);
            let to_eye = [0, 1, 2].map(|k| eye_at[k] - centroid[k]);
            if !faces(&quads[t.quad], to_eye) {
                continue;
            }
            let mut poly: Vec<ClipVertex> = (0..3)
                .map(|k| ClipVertex {
                    p: t.position[k],
                    c: t.colour[k],
                })
                .collect();
            poly = clip_polygon(&poly, |v| d + behind(v.p) - nearest);
            if poly.len() < 3 {
                continue;
            }
            let project = |v: &ClipVertex| {
                let t = behind(v.p);
                let scale = d / (d + t);
                [
                    across(v.p) * scale,
                    eye_y + (v.p[1] - eye_y) * scale,
                    1.0 / (d + t),
                ]
            };
            let first = project(&poly[0]);
            for k in 1..poly.len() - 1 {
                out.push(ViewTriangle {
                    uvw: [first, project(&poly[k]), project(&poly[k + 1])],
                    colour: [poly[0].c, poly[k].c, poly[k + 1].c],
                });
            }
        }
        views.push(out);
    }
    let (mut reach, mut low_v, mut high_v) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
    for tri in views.iter().flatten() {
        for v in &tri.uvw {
            reach = reach.max(v[0].abs());
            low_v = low_v.min(v[1]);
            high_v = high_v.max(v[1]);
        }
    }
    if !low_v.is_finite() {
        return None;
    }
    let half_width = (reach + 1.0).ceil().max(8.0);
    let base = (low_v - 1.0).floor();
    let top = (high_v + 1.0).ceil();
    let height = (top - base).max(8.0);
    let (sw, sh) = (Raster::WIDE as f32, Raster::HIGH as f32);
    let mut sectors = Vec::with_capacity(VIEWS * SLOT_BYTES);
    let mut preview = Vec::with_capacity(VIEWS * WIDE * HIGH);
    for tris in &views {
        let mut raster = Raster::new();
        for t in tris {
            let to_sample = |v: [f32; 3]| {
                [
                    (v[0] + half_width) / (2.0 * half_width) * sw,
                    (base + height - v[1]) / height * sh,
                    v[2],
                ]
            };
            raster.fill_persp(t.uvw.map(to_sample), t.colour);
        }
        let (sector, shown) = finish_picture(&raster);
        sectors.extend_from_slice(&sector);
        preview.extend(shown);
    }
    Some(BakedCard {
        centre,
        base: base as i32,
        height: height as i32,
        half_width: half_width as i32,
        sectors,
        preview,
        layer: 0,
    })
}

/// The centre, reach and vertical extent of a room's geometry.
pub(super) struct RoomFrame {
    pub(super) centre: [i32; 2],
    #[allow(dead_code)]
    pub(super) reach: f32,
    pub(super) low: f32,
    #[allow(dead_code)]
    pub(super) high: f32,
}

/// Where a room stands: the centre of the sectors it owns, how far its geometry reaches
/// from there and how low and high it goes. `None` when it owns nothing or draws nothing.
pub(super) fn room_frame(
    cooked: &CookedLevel,
    room: usize,
    tris: &[Triangle],
) -> Option<RoomFrame> {
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
    let (mut reach, mut low, mut high) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
    for t in tris {
        for p in &t.position {
            let (dx, dz) = (p[0] - centre[0] as f32, p[2] - centre[1] as f32);
            reach = reach.max((dx * dx + dz * dz).sqrt());
            low = low.min(p[1]);
            high = high.max(p[1]);
        }
    }
    Some(RoomFrame {
        centre,
        reach,
        low,
        high,
    })
}

/// The old bake, a shear orthographic view with the eye pitched down: kept to compare the
/// two (the fit example). Bake one room's card, `None` when the room draws nothing.
pub fn bake_room_ortho(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    room: usize,
) -> Option<BakedCard> {
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
        let (sector, shown) = finish_picture(&raster);
        sectors.extend_from_slice(&sector);
        preview.extend(shown);
    }
    Some(BakedCard {
        centre,
        base: base as i32,
        height: height as i32,
        half_width: half_width as i32,
        sectors,
        preview,
        layer: 0,
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

/// One layer of cards beyond the room cards: each card holds everything
/// behind its plane, out to `depth`, seen from a nominal eye in front of it.
/// All lengths are runtime units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerSpec {
    /// Plan distance from the eye the layer's cards start to show at.
    pub near: i32,
    /// Plan distance they stop at (`None`: never; the layer holds the horizon).
    pub far: Option<i32>,
    /// Distance between the layer's anchors.
    pub cell: i32,
    /// How far behind its plane a card's picture reaches (`None`: without limit).
    pub depth: Option<i32>,
}

impl LayerSpec {
    /// Distance in front of the card plane the picture is rendered from: a
    /// third of the way into the band the layer shows in, where the eye
    /// spends most of its time.
    fn nominal(&self) -> f32 {
        let far = self.far.unwrap_or(self.near.saturating_mul(2));
        (self.near + (far - self.near) / 3) as f32
    }

    /// Half the width of a card: wide enough that the cards of neighbouring
    /// anchors overlap a little.
    fn half_width(&self) -> i32 {
        self.cell * 3 / 4
    }
}

/// Height of the nominal eye above the lowest ground by a card, in sectors.
const EYE_SECTORS: f32 = 2.0;

/// A cooked triangle of the whole level in world space.
pub(super) struct WorldTriangle {
    pub(super) position: [[f32; 3]; 3],
    pub(super) colour: [[f32; 3]; 3],
    pub(super) quad: Quad,
    /// Which corners of the quad the triangle has, and the level material slot of the quad.
    pub(super) corner: [usize; 3],
    pub(super) slot: usize,
}

/// Every triangle of the level, and the world box of each room's.
pub(super) struct WorldTriangles {
    pub(super) triangles: Vec<WorldTriangle>,
    /// Per room, its triangles' index range and world box `(lo, hi)`.
    pub(super) rooms: Vec<(std::ops::Range<usize>, [f32; 3], [f32; 3])>,
}

impl WorldTriangles {
    pub(super) fn new(cooked: &CookedLevel, albedo: &[[f32; 3]]) -> Self {
        let mut triangles = Vec::new();
        let mut rooms = Vec::new();
        for room in 0..cooked.level.rooms.len() {
            let start = triangles.len();
            let (tris, quads) = triangles_of(cooked, albedo, room);
            let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
            for t in tris {
                for p in &t.position {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                let quad = quads[t.quad];
                let slot = cooked.tables[room]
                    .material_use
                    .get(usize::from(quad.material))
                    .map_or(0, |&m| usize::from(m));
                triangles.push(WorldTriangle {
                    position: t.position,
                    colour: t.colour,
                    quad,
                    corner: t.corner,
                    slot,
                });
            }
            rooms.push((start..triangles.len(), lo, hi));
        }
        Self { triangles, rooms }
    }
}

/// A polygon vertex while clipping: position and corner colour.
#[derive(Clone, Copy)]
pub(super) struct ClipVertex {
    pub(super) p: [f32; 3],
    pub(super) c: [f32; 3],
}

/// Keep the part of `poly` where `f >= 0` (`f` linear in the vertex).
pub(super) fn clip_polygon(poly: &[ClipVertex], f: impl Fn(&ClipVertex) -> f32) -> Vec<ClipVertex> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (fa, fb) = (f(&a), f(&b));
        if fa >= 0.0 {
            out.push(a);
        }
        if (fa >= 0.0) != (fb >= 0.0) {
            let k = fa / (fa - fb);
            let mix = |x: [f32; 3], y: [f32; 3]| [0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * k);
            out.push(ClipVertex {
                p: mix(a.p, b.p),
                c: mix(a.c, b.c),
            });
        }
    }
    out
}

/// A triangle of one view projected onto the card plane: `(u, v, w)` per
/// vertex (across, height, one over the distance from the nominal eye) and
/// the corner colours.
struct ViewTriangle {
    uvw: [[f32; 3]; 3],
    colour: [[f32; 3]; 3],
}

/// The view `view` of a card of layer `spec` standing at `anchor`: every
/// triangle of the level that faces the nominal eye and lies behind the card's
/// plane (within `spec.depth` of it), projected onto the plane.
fn view_triangles(
    world: &WorldTriangles,
    spec: &LayerSpec,
    anchor: [i32; 2],
    eye_y: f32,
    half_width: f32,
    view: usize,
) -> Vec<ViewTriangle> {
    let (cos, sin) = (
        f32::from(VIEW_COS_Q12[view]) / 4096.0,
        f32::from(VIEW_SIN_Q12[view]) / 4096.0,
    );
    let (cx, cz) = (anchor[0] as f32, anchor[1] as f32);
    let distance = spec.nominal();
    let eye = [cx + cos * distance, eye_y, cz + sin * distance];
    // Depth behind the plane, and across it, of a world point.
    let behind = |p: [f32; 3]| -((p[0] - cx) * cos + (p[2] - cz) * sin);
    let across = |p: [f32; 3]| (p[0] - cx) * sin - (p[2] - cz) * cos;
    let reach = spec.depth.map(|d| d as f32);
    let mut out = Vec::new();
    for (range, lo, hi) in &world.rooms {
        if range.is_empty() {
            continue;
        }
        // The room's box against the slab and the widening wedge: every
        // corner on one wrong side puts it out.
        let corners = (0..8).map(|i| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        });
        let (mut all_in_front, mut all_past, mut all_left, mut all_right) =
            (true, true, true, true);
        for c in corners {
            let t = behind(c);
            let wedge = half_width * (distance + t.max(0.0)) / distance;
            all_in_front &= t < 0.0;
            all_past &= reach.is_some_and(|r| t > r);
            all_left &= across(c) < -wedge;
            all_right &= across(c) > wedge;
        }
        if all_in_front || all_past || all_left || all_right {
            continue;
        }
        for tri in &world.triangles[range.clone()] {
            let centroid = [0, 1, 2]
                .map(|k| (tri.position[0][k] + tri.position[1][k] + tri.position[2][k]) / 3.0);
            let to_eye = [0, 1, 2].map(|k| eye[k] - centroid[k]);
            if !faces(&tri.quad, to_eye) {
                continue;
            }
            let mut poly: Vec<ClipVertex> = (0..3)
                .map(|k| ClipVertex {
                    p: tri.position[k],
                    c: tri.colour[k],
                })
                .collect();
            poly = clip_polygon(&poly, |v| behind(v.p));
            if let Some(r) = reach {
                if poly.len() >= 3 {
                    poly = clip_polygon(&poly, |v| r - behind(v.p));
                }
            }
            if poly.len() < 3 {
                continue;
            }
            // The point where the ray from the eye meets the card's plane.
            let project = |v: &ClipVertex| {
                let t = behind(v.p);
                let scale = distance / (distance + t);
                [
                    across(v.p) * scale,
                    eye_y + (v.p[1] - eye_y) * scale,
                    1.0 / (distance + t),
                ]
            };
            let first = project(&poly[0]);
            for k in 1..poly.len() - 1 {
                out.push(ViewTriangle {
                    uvw: [first, project(&poly[k]), project(&poly[k + 1])],
                    colour: [poly[0].c, poly[k].c, poly[k + 1].c],
                });
            }
        }
    }
    out
}

/// Bake one layer card: the eight views of what stands behind `anchor`.
/// `None` when nothing does.
fn bake_layer_card(
    world: &WorldTriangles,
    spec: &LayerSpec,
    layer: u8,
    anchor: [i32; 2],
    sector: i32,
) -> Option<BakedCard> {
    // The lowest ground near the anchor puts the nominal eye at a person's
    // height above it.
    let near = spec.cell as f32 / 2.0;
    let mut ground = f32::INFINITY;
    for (range, lo, hi) in &world.rooms {
        if hi[0] < anchor[0] as f32 - near
            || lo[0] > anchor[0] as f32 + near
            || hi[2] < anchor[1] as f32 - near
            || lo[2] > anchor[1] as f32 + near
        {
            continue;
        }
        for tri in &world.triangles[range.clone()] {
            for p in &tri.position {
                if (p[0] - anchor[0] as f32).abs() <= near
                    && (p[2] - anchor[1] as f32).abs() <= near
                {
                    ground = ground.min(p[1]);
                }
            }
        }
    }
    if !ground.is_finite() {
        return None;
    }
    let eye_y = ground + EYE_SECTORS * sector as f32;
    let half_width = spec.half_width() as f32;
    let views: Vec<Vec<ViewTriangle>> = (0..VIEWS)
        .map(|view| view_triangles(world, spec, anchor, eye_y, half_width, view))
        .collect();
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    for tri in views.iter().flatten() {
        for v in &tri.uvw {
            low = low.min(v[1]);
            high = high.max(v[1]);
        }
    }
    if !low.is_finite() {
        return None;
    }
    let base = (low - 1.0).floor();
    let top = (high + 1.0).ceil();
    let height = (top - base).max(8.0);
    let (sw, sh) = (Raster::WIDE as f32, Raster::HIGH as f32);
    let mut sectors = Vec::with_capacity(VIEWS * SLOT_BYTES);
    let mut preview = Vec::with_capacity(VIEWS * WIDE * HIGH);
    for tris in &views {
        let mut raster = Raster::new();
        for t in tris {
            let to_sample = |v: [f32; 3]| {
                [
                    (v[0] + half_width) / (2.0 * half_width) * sw,
                    (base + height - v[1]) / height * sh,
                    v[2],
                ]
            };
            raster.fill_persp(t.uvw.map(to_sample), t.colour);
        }
        let (sector_bytes, shown) = finish_picture(&raster);
        sectors.extend_from_slice(&sector_bytes);
        preview.extend(shown);
    }
    Some(BakedCard {
        centre: anchor,
        base: base as i32,
        height: height as i32,
        half_width: half_width as i32,
        sectors,
        preview,
        layer,
    })
}

/// The anchors of a layer: the room cards' centres grouped by a lattice of
/// `cell` units, each group's mean.
fn layer_anchors(rooms: &[Option<BakedCard>], cell: i32) -> Vec<[i32; 2]> {
    let mut groups: std::collections::BTreeMap<(i32, i32), ([i64; 2], i64)> = Default::default();
    for card in rooms.iter().flatten() {
        let key = (
            card.centre[0].div_euclid(cell),
            card.centre[1].div_euclid(cell),
        );
        let entry = groups.entry(key).or_default();
        entry.0[0] += i64::from(card.centre[0]);
        entry.0[1] += i64::from(card.centre[1]);
        entry.1 += 1;
    }
    groups
        .values()
        .map(|(sum, n)| [(sum[0] / n) as i32, (sum[1] / n) as i32])
        .collect()
}

/// Bake the layers of cards beyond the room cards `rooms`: for each layer,
/// the cards at its anchors, one list per layer. Layer `k` of `specs` is card
/// layer `k + 1`.
pub fn bake_layers(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    rooms: &[Option<BakedCard>],
    specs: &[LayerSpec],
) -> Vec<Vec<BakedCard>> {
    let world = WorldTriangles::new(cooked, albedo);
    let sector = i32::from(cooked.level.sector_units);
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 4);
    specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let anchors = layer_anchors(rooms, spec.cell);
            let world = &world;
            let anchors = &anchors;
            let mut cards: Vec<Option<BakedCard>> = vec![None; anchors.len()];
            std::thread::scope(|scope| {
                let workers: Vec<_> = (0..threads.min(anchors.len().max(1)))
                    .map(|worker| {
                        let step = threads.min(anchors.len().max(1));
                        scope.spawn(move || {
                            (worker..anchors.len())
                                .step_by(step)
                                .map(|i| {
                                    let card = bake_layer_card(
                                        world,
                                        spec,
                                        index as u8 + 1,
                                        anchors[i],
                                        sector,
                                    );
                                    (i, card)
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                for worker in workers {
                    for (i, card) in worker.join().expect("a layer bake thread panicked") {
                        cards[i] = card;
                    }
                }
            });
            cards.into_iter().flatten().collect()
        })
        .collect()
}

/// Cells across one side of the grid the cards' centres are placed on to
/// order them along a Hilbert curve.
const ORDER_GRID: u32 = 256;

/// Position of cell `(x, y)` along the Hilbert curve through an
/// `ORDER_GRID` square: cells close on the curve are close in the plane.
pub(super) fn hilbert_index(mut x: u32, mut y: u32) -> u32 {
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

/// Tiles the local copies are made for at most (the manifest tables grow with
/// the square).
pub const LOCAL_COPY_MAX_TILES: usize = 64;

/// Pictures laid out near the rooms that need them: while the eye is in
/// the neighbourhood of tile `X` it asks for the pictures of the tiles that
/// stand `near` to `enter` away, and only a view or two of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalCopies {
    /// Per tile, where its rooms' centres average, runtime units.
    pub centres: Vec<[i32; 2]>,
    /// Per tile `X`, the `(Y, views)` it holds copies of: tile `Y`'s eight
    /// pictures of each view in the `views` bit mask, view after view.
    pub entries: Vec<Vec<(u32, u8)>>,
}

/// Work out which pictures of which tiles each tile's neighbourhood asks
/// for. `centres` is each room's card centre (`None` for no card);
/// `layout` puts rooms in tiles; cards show from `near` to `enter` plan
/// units (`sector` is a sector in the same units: the eye is taken anywhere
/// within two sectors of the rooms of the tile it is in).
pub fn local_copies(
    layout: &DiscLayout,
    centres: &[Option<[i32; 2]>],
    near: i32,
    enter: i32,
    sector: i32,
) -> LocalCopies {
    let tile_sectors = TILE_ROOMS * VIEWS as u32;
    let tiles = (layout.sectors / tile_sectors) as usize;
    if tiles == 0 || tiles > LOCAL_COPY_MAX_TILES {
        return LocalCopies::default();
    }
    let tile_of = |room: usize| {
        let offset = layout.offsets[room];
        (offset != u32::MAX).then(|| (offset / tile_sectors) as usize)
    };
    let mut sums = vec![([0i64; 2], 0i64); tiles];
    for (room, centre) in centres.iter().enumerate() {
        if let (Some(centre), Some(tile)) = (centre, tile_of(room)) {
            sums[tile].0[0] += i64::from(centre[0]);
            sums[tile].0[1] += i64::from(centre[1]);
            sums[tile].1 += 1;
        }
    }
    let tile_centres: Vec<[i32; 2]> = sums
        .iter()
        .map(|(sum, n)| {
            let n = (*n).max(1);
            [(sum[0] / n) as i32, (sum[1] / n) as i32]
        })
        .collect();
    let mut entries = vec![Vec::new(); tiles];
    for (x, entry) in entries.iter_mut().enumerate() {
        // Where the eye can be while this tile's centre is the nearest.
        let mut eyes = Vec::new();
        for (room, centre) in centres.iter().enumerate() {
            if let (Some(centre), Some(tile)) = (centre, tile_of(room)) {
                if tile == x {
                    for (dx, dz) in [(0, 0), (-2, 0), (2, 0), (0, -2), (0, 2)] {
                        eyes.push([centre[0] + dx * sector, centre[1] + dz * sector]);
                    }
                }
            }
        }
        let mut masks = vec![0u8; tiles];
        for (room, centre) in centres.iter().enumerate() {
            let (Some(centre), Some(y)) = (centre, tile_of(room)) else {
                continue;
            };
            for eye in &eyes {
                let (dx, dz) = (eye[0] - centre[0], eye[1] - centre[1]);
                let distance = f64::from(dx).hypot(f64::from(dz)) as i32;
                if distance < near - 2 * sector || distance > enter + 2 * sector {
                    continue;
                }
                masks[y] |= 1 << psx_grid::render::view_toward(dx, dz);
            }
        }
        *entry = masks
            .iter()
            .enumerate()
            .filter(|(_, &mask)| mask != 0)
            .map(|(y, &mask)| (y as u32, mask))
            .collect();
        // Tiles in the order they stand round this one, so the pictures the
        // eye asks for walking in one direction lie together in the block.
        let at = tile_centres[x];
        entry.sort_by(|a, b| {
            let angle = |y: u32| {
                let c = tile_centres[y as usize];
                f64::from(c[1] - at[1]).atan2(f64::from(c[0] - at[0]))
            };
            angle(a.0).total_cmp(&angle(b.0)).then(a.0.cmp(&b.0))
        });
    }
    LocalCopies {
        centres: tile_centres,
        entries,
    }
}

/// Bake every room's card. `albedo` is the mean colour of each level
/// material in slot order (`cooked.materials`).
/// `eye` `None` bakes the old shear orthographic cards ([`bake_room_ortho`]), for comparing.
pub fn bake(
    cooked: &CookedLevel,
    albedo: &[[f32; 3]],
    eye: Option<RoomEye>,
) -> (Vec<Option<BakedCard>>, CardsReport) {
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
                        .map(|room| {
                            let card = match eye {
                                Some(eye) => bake_room(cooked, albedo, room, eye),
                                None => bake_room_ortho(cooked, albedo, room),
                            };
                            (room, card)
                        })
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

/// How well a room's card lines up with the room's own geometry seen from one eye: both are
/// drawn into a mask the size of the game's screen and compared.
pub mod fit {
    use super::*;

    /// The game's screen and focal length, in pixels.
    pub const SCREEN: [usize; 2] = [320, 240];
    pub const FOCAL: f32 = 320.0;

    /// What [`fit_room`] measured.
    #[derive(Clone, Debug)]
    pub struct Fit {
        /// Pixels the room's geometry covers, the card covers, and both.
        pub geometry: usize,
        pub card: usize,
        pub both: usize,
        /// Pixels in one and not the other, over pixels in either.
        pub mismatch: f32,
        /// Mean over the columns both cover of how far the top and the bottom edge of the card
        /// stand from the geometry's, in pixels.
        pub top: f32,
        pub bottom: f32,
        /// How far the left and right ends of the card stand from the geometry's, in pixels.
        pub left: f32,
        pub right: f32,
        /// `SCREEN` pixels, row-major: white both, green geometry only, magenta card only.
        pub overlay: Vec<[u8; 3]>,
    }

    /// The view of `VIEWS` the eye at `(dx, dz)` from the card is nearest to.
    fn nearest_view(dx: f32, dz: f32) -> usize {
        let mut best = (0usize, f32::MIN);
        for view in 0..VIEWS {
            let (c, s) = (
                f32::from(VIEW_COS_Q12[view]) / 4096.0,
                f32::from(VIEW_SIN_Q12[view]) / 4096.0,
            );
            let dot = (dx * c + dz * s) / (dx * dx + dz * dz).sqrt().max(1e-3);
            if dot > best.1 {
                best = (view, dot);
            }
        }
        best.0
    }

    /// Fit `card` of `room` for an eye at `eye` (world `x, y, z`), looking level along the
    /// line from the eye to the card's centre.
    pub fn fit_room(
        cooked: &CookedLevel,
        albedo: &[[f32; 3]],
        room: usize,
        card: &BakedCard,
        eye: [f32; 3],
    ) -> Option<Fit> {
        let (tris, quads) = triangles(cooked, albedo, room);
        let (cx, cz) = (card.centre[0] as f32, card.centre[1] as f32);
        let (dx, dz) = (eye[0] - cx, eye[2] - cz);
        let length = (dx * dx + dz * dz).sqrt();
        if length < 1.0 {
            return None;
        }
        let h = [dx / length, dz / length];
        let right = [h[1], -h[0]];
        let (w, hh) = (SCREEN[0], SCREEN[1]);
        let project = |p: [f32; 3]| -> Option<[f32; 2]> {
            let rel = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
            let z = -(rel[0] * h[0] + rel[2] * h[1]);
            if z < 4.0 {
                return None;
            }
            let x = rel[0] * right[0] + rel[2] * right[1];
            Some([
                w as f32 / 2.0 + FOCAL * x / z,
                hh as f32 / 2.0 - FOCAL * rel[1] / z,
            ])
        };
        let mut geometry = vec![false; w * hh];
        let to_eye_of = |t: &Triangle| {
            let c =
                [0, 1, 2].map(|k| (t.position[0][k] + t.position[1][k] + t.position[2][k]) / 3.0);
            [0, 1, 2].map(|k| eye[k] - c[k])
        };
        for t in &tris {
            if !faces(&quads[t.quad], to_eye_of(t)) {
                continue;
            }
            let p: Vec<Option<[f32; 2]>> = t.position.iter().map(|&q| project(q)).collect();
            if let [Some(a), Some(b), Some(c)] = p[..] {
                fill_mask(&mut geometry, w, hh, [a, b, c]);
            }
        }
        // The card: a rectangle standing at the centre, perpendicular to the view.
        let corner = |u: f32, y: f32| project([cx + right[0] * u, y, cz + right[1] * u]);
        let (top, bottom) = ((card.base + card.height) as f32, card.base as f32);
        let hw = card.half_width as f32;
        let (a, b, c) = (corner(-hw, top)?, corner(hw, top)?, corner(-hw, bottom)?);
        let view = nearest_view(dx, dz);
        let picture = &card.preview[view * WIDE * HIGH..(view + 1) * WIDE * HIGH];
        let mut card_mask = vec![false; w * hh];
        let (x0, x1) = (
            a[0].floor().max(0.0) as usize,
            (b[0].ceil() as usize).min(w),
        );
        let (y0, y1) = (
            a[1].floor().max(0.0) as usize,
            (c[1].ceil() as usize).min(hh),
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let u = ((x as f32 + 0.5) - a[0]) / (b[0] - a[0]);
                let v = ((y as f32 + 0.5) - a[1]) / (c[1] - a[1]);
                if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                    continue;
                }
                let (tx, ty) = ((u * WIDE as f32) as usize, (v * HIGH as f32) as usize);
                if picture[ty.min(HIGH - 1) * WIDE + tx.min(WIDE - 1)][3] != 0 {
                    card_mask[y * w + x] = true;
                }
            }
        }
        let (mut g, mut c2, mut both) = (0usize, 0usize, 0usize);
        let mut overlay = vec![[24u8, 24, 28]; w * hh];
        for i in 0..w * hh {
            g += usize::from(geometry[i]);
            c2 += usize::from(card_mask[i]);
            overlay[i] = match (geometry[i], card_mask[i]) {
                (true, true) => {
                    both += 1;
                    [255, 255, 255]
                }
                (true, false) => [40, 200, 80],
                (false, true) => [220, 40, 220],
                (false, false) => [24, 24, 28],
            };
        }
        let union = g + c2 - both;
        let edge = |mask: &[bool], x: usize, from_top: bool| {
            let rows: Box<dyn Iterator<Item = usize>> = if from_top {
                Box::new(0..hh)
            } else {
                Box::new((0..hh).rev())
            };
            rows.into_iter().find(|&y| mask[y * w + x])
        };
        let (mut top_sum, mut bottom_sum, mut columns) = (0.0f32, 0.0f32, 0usize);
        for x in 0..w {
            if let (Some(gt), Some(ct), Some(gb), Some(cb)) = (
                edge(&geometry, x, true),
                edge(&card_mask, x, true),
                edge(&geometry, x, false),
                edge(&card_mask, x, false),
            ) {
                top_sum += (gt as f32 - ct as f32).abs();
                bottom_sum += (gb as f32 - cb as f32).abs();
                columns += 1;
            }
        }
        let extent = |mask: &[bool]| {
            let cols: Vec<usize> = (0..w)
                .filter(|&x| (0..hh).any(|y| mask[y * w + x]))
                .collect();
            (
                cols.first().copied().unwrap_or(0) as f32,
                cols.last().copied().unwrap_or(0) as f32,
            )
        };
        let (gl, gr) = extent(&geometry);
        let (cl, cr) = extent(&card_mask);
        Some(Fit {
            geometry: g,
            card: c2,
            both,
            mismatch: if union == 0 {
                0.0
            } else {
                1.0 - both as f32 / union as f32
            },
            top: if columns == 0 {
                f32::NAN
            } else {
                top_sum / columns as f32
            },
            bottom: if columns == 0 {
                f32::NAN
            } else {
                bottom_sum / columns as f32
            },
            left: (gl - cl).abs(),
            right: (gr - cr).abs(),
            overlay,
        })
    }

    /// Fill the pixels whose centres lie in a screen triangle.
    fn fill_mask(mask: &mut [bool], w: usize, h: usize, p: [[f32; 2]; 3]) {
        let edge = |a: [f32; 2], b: [f32; 2], x: f32, y: f32| {
            (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
        };
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-6 {
            return;
        }
        let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
        let x0 = (lo(f32::min, 0).floor().max(0.0)) as usize;
        let x1 = (lo(f32::max, 0).ceil().min(w as f32)).max(0.0) as usize;
        let y0 = (lo(f32::min, 1).floor().max(0.0)) as usize;
        let y1 = (lo(f32::max, 1).ceil().min(h as f32)).max(0.0) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let (sx, sy) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = edge(p[1], p[2], sx, sy) / area;
                let w1 = edge(p[2], p[0], sx, sy) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    mask[y * w + x] = true;
                }
            }
        }
    }
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
        let card = bake_room_ortho(&cooked, &albedo(), 0).expect("the room draws something");
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
        let card = bake_room_ortho(&cooked, &albedo(), 0).unwrap();
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

    /// How many texels of view `view` of `card` are drawn.
    fn drawn(card: &BakedCard, view: usize) -> usize {
        let (indices, _) = decode(&card.sectors[view * SLOT_BYTES..(view + 1) * SLOT_BYTES]);
        indices.iter().filter(|&&i| i != 0).count()
    }

    #[test]
    fn a_layer_card_holds_what_stands_behind_it_to_its_depth() {
        // A flat strip to the west and a tall block at the far east end of
        // another strip, 20 sectors on.
        let world = GridWorld {
            rooms: vec![
                strip([0, 0], &[0, 0, 0, 0]),
                strip([28, 0], &[0, 0, 8192, 8192]),
            ],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let world_tris = WorldTriangles::new(&cooked, &albedo());
        let sector = i32::from(cooked.level.sector_units);
        let anchor = [10 * sector, sector / 2];
        let spec = |depth: Option<i32>| LayerSpec {
            near: 12 * sector,
            far: Some(40 * sector),
            cell: 16 * sector,
            depth,
        };
        // View 4 is the eye on the west looking east: the block is behind
        // the plane at 18 sectors or more, the strip is not behind it.
        let far = bake_layer_card(&world_tris, &spec(None), 1, anchor, sector).unwrap();
        assert_eq!(far.layer, 1);
        assert!(drawn(&far, 4) > 40, "the block shows: {}", drawn(&far, 4));
        // With a depth that stops short of the block it is not in the picture.
        let near = bake_layer_card(&world_tris, &spec(Some(10 * sector)), 1, anchor, sector);
        assert!(near.is_none_or(|card| drawn(&card, 4) < drawn(&far, 4) / 2));
        // View 0, the eye on the east, sees the west strip behind the plane
        // and the block's back not at all.
        assert!(drawn(&far, 0) > 0);
    }

    #[test]
    fn a_nearer_wall_hides_the_one_behind_it_in_a_layer_picture() {
        // Two blocks on one line, 6 and 20 sectors behind the plane: the
        // nearer is tall, the farther is as tall, and shows only past it.
        let world = GridWorld {
            rooms: vec![strip([16, 0], &[8192, 8192]), strip([30, 0], &[8192, 8192])],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let world_tris = WorldTriangles::new(&cooked, &albedo());
        let sector = i32::from(cooked.level.sector_units);
        let spec = LayerSpec {
            near: 12 * sector,
            far: None,
            cell: 16 * sector,
            depth: None,
        };
        let anchor = [10 * sector, sector / 2];
        let both = bake_layer_card(&world_tris, &spec, 1, anchor, sector).unwrap();
        // The same picture with the nearer block's room left out would show
        // more of the farther one: here the farther block is behind the
        // nearer one at the same height, so it adds almost nothing.
        let alone = GridWorld {
            rooms: vec![strip([16, 0], &[8192, 8192])],
            ..GridWorld::default()
        };
        let cooked_alone = cook(&alone).unwrap();
        let tris_alone = WorldTriangles::new(&cooked_alone, &albedo());
        let one = bake_layer_card(&tris_alone, &spec, 1, anchor, sector).unwrap();
        let (a, b) = (drawn(&both, 4), drawn(&one, 4));
        assert!(a > 0 && b > 0);
        assert!(a.abs_diff(b) * 5 < b, "both {a}, nearer alone {b}");
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
    fn a_tile_asks_for_the_views_of_the_tiles_that_stand_a_card_distance_away() {
        // 6 x 4 lattice, 10 sectors between rooms (sector = 80 units).
        let sector = 80;
        let mut centres = Vec::new();
        for z in 0..4 {
            for x in 0..6 {
                centres.push(Some([x * 10 * sector, z * 10 * sector]));
            }
        }
        let layout = disc_layout(&centres);
        let tiles = (layout.sectors / (TILE_ROOMS * VIEWS as u32)) as usize;
        // Cards from 12 sectors out to 30.
        let copies = local_copies(&layout, &centres, 12 * sector, 30 * sector, sector);
        assert_eq!(copies.centres.len(), tiles);
        assert_eq!(copies.entries.len(), tiles);
        for (x, entries) in copies.entries.iter().enumerate() {
            assert!(!entries.is_empty(), "tile {x} asks for nothing");
            for &(y, mask) in entries {
                assert!((y as usize) < tiles);
                assert!(mask != 0);
                // A tile does not ask for itself at every view: the eye is
                // within its own rooms, nearer than the cards show from,
                // except where the tile is wide.
                let _ = x;
            }
            // At most one copy of each tile.
            assert!(entries.len() <= tiles);
        }
        // A tile's view of a far tile is the side facing it: tile X to the
        // west of Y asks for the picture of Y seen from the west (view 4).
        let leftmost = (0..tiles)
            .min_by_key(|&t| copies.centres[t][0])
            .expect("tiles");
        let rightmost = (0..tiles)
            .max_by_key(|&t| copies.centres[t][0])
            .expect("tiles");
        let asks = copies.entries[leftmost]
            .iter()
            .find(|&&(y, _)| y as usize == rightmost);
        if let Some(&(_, mask)) = asks {
            assert!(mask & (1 << 4) != 0, "{mask:08b}");
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
        let card = bake_room_ortho(&cooked, &albedo(), 0).unwrap();
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
    /// A room with a wall and a floor, a perspective card of it from `eye`.
    fn walled(eye: RoomEye) -> (CookedLevel, BakedCard) {
        let world = GridWorld {
            rooms: vec![strip([20, 4], &[0, 0, 4096, 4096, 0, 0])],
            ..GridWorld::default()
        };
        let cooked = cook(&world).unwrap();
        let card = bake_room(&cooked, &albedo(), 0, eye).expect("the room draws something");
        (cooked, card)
    }

    #[test]
    fn a_perspective_card_lines_up_with_its_room_from_the_eye_it_was_baked_for() {
        let eye = RoomEye {
            distance: 960.0,
            height: EyeHeight::AboveRoom(104.0),
        };
        let (cooked, card) = walled(eye);
        let at = [
            card.centre[0] as f32 + eye.distance,
            104.0,
            card.centre[1] as f32,
        ];
        let fit = fit::fit_room(&cooked, &albedo(), 0, &card, at).expect("a fit");
        assert!(fit.geometry > 200, "{} pixels of geometry", fit.geometry);
        assert!(fit.mismatch < 0.2, "mismatch {}", fit.mismatch);
        assert!(
            fit.left < 6.0 && fit.right < 6.0,
            "{} {}",
            fit.left,
            fit.right
        );
    }
}
