//! Far panels: the level beyond the real geometry as depth-meshed pictures baked at the
//! lattice's eyes.
//!
//! A panel is the level seen from a bake eye `B` along one of eight headings, in a pinhole view
//! whose focal length matches the game's own (so one texel is about one screen pixel). The
//! picture is cut into tiles of [`TILE_W`] by [`TILE_H`] 4-bit texels (one disc sector each,
//! palette in the row under the picture); a tile with nothing in it is not stored. Beside it
//! the bake keeps a *depth mesh*: the distance along the heading, at every vertex of a grid of
//! cells laid over the picture. The game draws each cell as one textured quad whose corners
//! stand at `B + depth * ray(u, v)` and projects them from the real eye, so the picture
//! moves with the right parallax when the eye leaves `B`.
//!
//! This file holds what the cook needs to bake a panel and what the fit example needs to judge
//! one: a pinhole renderer for the level, the panel bake and a host model of the game's draw.

use psxed_tex::quantize_rgba_with_transparent_zero;

use super::cards::{clip_polygon, faces, room_frame, triangles_of, ClipVertex, WorldTriangles};
use super::CookedLevel;
use psx_asset::Texture;
use psxed_format::texture::Depth;

/// Texels across a tile.
pub const TILE_W: usize = 64;
/// Texel rows of a tile (the row below holds its palette).
pub const TILE_H: usize = 63;
/// Nearest depth (camera space, runtime units) kept, as a fraction of a sector.
const NEAR_SECTORS: f32 = 0.04;

/// A texture decoded to RGBA, with a chain of box filtered smaller levels (a level's texel is
/// the mean of the opaque texels of four of the one before), so a far surface samples a level
/// whose texels are about the size of the bake's samples.
#[derive(Clone, Debug)]
pub struct TextureImage {
    levels: Vec<(usize, usize, Vec<[u8; 4]>)>,
}

impl TextureImage {
    /// Decode the bytes of a PSXT; `None` when they are not one.
    pub fn from_psxt(psxt: &[u8]) -> Option<Self> {
        let texture = Texture::from_bytes(psxt).ok()?;
        let (width, height) = (usize::from(texture.width()), usize::from(texture.height()));
        if width == 0 || height == 0 {
            return None;
        }
        let pixels = texture.pixel_bytes();
        let clut = texture.clut_bytes();
        let stride = usize::from(texture.halfwords_per_row()) * 2;
        let cut_out = texture.is_index_zero_transparent();
        let colour555 = |lo: u8, hi: u8| -> [u8; 3] {
            let v = u32::from(u16::from_le_bytes([lo, hi]));
            let expand = |c: u32| ((c << 3) | (c >> 2)) as u8;
            [
                expand(v & 31),
                expand((v >> 5) & 31),
                expand((v >> 10) & 31),
            ]
        };
        let mut base = Vec::with_capacity(width * height);
        for y in 0..height {
            let row = pixels.get(y * stride..)?;
            for x in 0..width {
                let texel = match texture.depth() {
                    Depth::Bit4 => {
                        let byte = *row.get(x / 2)?;
                        let index = usize::from(if x % 2 == 0 { byte & 15 } else { byte >> 4 });
                        let c = colour555(*clut.get(index * 2)?, *clut.get(index * 2 + 1)?);
                        [
                            c[0],
                            c[1],
                            c[2],
                            if cut_out && index == 0 { 0 } else { 255 },
                        ]
                    }
                    Depth::Bit8 => {
                        let index = usize::from(*row.get(x)?);
                        let c = colour555(*clut.get(index * 2)?, *clut.get(index * 2 + 1)?);
                        [
                            c[0],
                            c[1],
                            c[2],
                            if cut_out && index == 0 { 0 } else { 255 },
                        ]
                    }
                    _ => {
                        let (lo, hi) = (*row.get(x * 2)?, *row.get(x * 2 + 1)?);
                        let c = colour555(lo, hi);
                        [
                            c[0],
                            c[1],
                            c[2],
                            if cut_out && u16::from_le_bytes([lo, hi]) == 0 {
                                0
                            } else {
                                255
                            },
                        ]
                    }
                };
                base.push(texel);
            }
        }
        let mut levels = vec![(width, height, base)];
        while levels.last().is_some_and(|l| l.0 > 1 || l.1 > 1) {
            let (w, h, prev) = levels.last().expect("a level");
            let (nw, nh) = ((*w / 2).max(1), (*h / 2).max(1));
            let mut next = Vec::with_capacity(nw * nh);
            for y in 0..nh {
                for x in 0..nw {
                    let (mut sum, mut n) = ([0u32; 3], 0u32);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = ((2 * x + dx).min(*w - 1), (2 * y + dy).min(*h - 1));
                        let t = prev[sy * *w + sx];
                        if t[3] != 0 {
                            for k in 0..3 {
                                sum[k] += u32::from(t[k]);
                            }
                            n += 1;
                        }
                    }
                    next.push(match sum.map(|c| c.checked_div(n)) {
                        [Some(r), Some(g), Some(b)] => [r as u8, g as u8, b as u8, 255],
                        _ => [0, 0, 0, 0],
                    });
                }
            }
            levels.push((nw, nh, next));
        }
        Some(Self { levels })
    }

    /// Texels across level 0.
    pub fn width(&self) -> usize {
        self.levels[0].0
    }

    /// The texel at `(u, v)` of level 0 texels, wrapping, taken from level `lod`; `None` where
    /// the texture is transparent.
    fn sample(&self, lod: usize, u: f32, v: f32) -> Option<[u8; 3]> {
        let lod = lod.min(self.levels.len() - 1);
        let (w, h, texels) = &self.levels[lod];
        let scale = 1.0 / (1usize << lod) as f32;
        let x = ((u * scale).floor() as i64).rem_euclid(*w as i64) as usize;
        let y = ((v * scale).floor() as i64).rem_euclid(*h as i64) as usize;
        let t = texels[y * w + x];
        (t[3] != 0).then_some([t[0], t[1], t[2]])
    }

    /// The level whose texels are about `texels_per_sample` of level 0's across.
    fn level_for(&self, texels_per_sample: f32) -> usize {
        if texels_per_sample <= 1.0 {
            0
        } else {
            (texels_per_sample.log2().floor() as usize).min(self.levels.len() - 1)
        }
    }
}

/// The textures of the level's materials, by material slot, and their tints.
#[derive(Clone, Debug, Default)]
pub struct TextureSet {
    /// Per slot, the decoded texture (`None` for an untextured material).
    pub images: Vec<Option<TextureImage>>,
    /// Per slot, the material's modulation (`1.0` is neutral).
    pub tint: Vec<[f32; 3]>,
}

/// A camera as the GTE holds it: the rows of the view rotation take a world offset to camera
/// `(right, down, forward)`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: [f32; 3],
    pub rows: [[f32; 3]; 3],
    pub focal: f32,
    pub width: usize,
    pub height: usize,
    /// Principal point in pixels.
    pub cx: f32,
    pub cy: f32,
}

impl Camera {
    /// World point to camera space.
    pub fn to_camera(&self, p: [f32; 3]) -> [f32; 3] {
        let d = [p[0] - self.eye[0], p[1] - self.eye[1], p[2] - self.eye[2]];
        self.rows.map(|r| r[0] * d[0] + r[1] * d[1] + r[2] * d[2])
    }

    /// Camera point (depth positive) to the pixel it lands on.
    pub fn pixel(&self, c: [f32; 3]) -> [f32; 2] {
        [
            self.cx + self.focal * c[0] / c[2],
            self.cy + self.focal * c[1] / c[2],
        ]
    }

    /// The camera the game has, from a rotation in Q12 rows and the eye.
    pub fn game(eye: [f32; 3], q12: [[i16; 3]; 3], focal: f32) -> Self {
        Self {
            eye,
            rows: q12.map(|r| {
                // The game scales the rotation (its world is a few times the GTE's); only the
                // direction of each row matters here.
                let v = r.map(f32::from);
                let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1.0);
                v.map(|c| c / length)
            }),
            focal,
            width: 320,
            height: 240,
            cx: 160.0,
            cy: 120.0,
        }
    }
}

/// Samples of a render: colour, nearness and the room of the nearest surface.
pub struct Samples {
    pub width: usize,
    pub height: usize,
    pub colour: Vec<[f32; 3]>,
    /// One over the depth of the nearest surface, 0 where nothing is.
    pub inverse: Vec<f32>,
    pub room: Vec<u16>,
}

impl Samples {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            colour: vec![[0.0; 3]; width * height],
            inverse: vec![0.0; width * height],
            room: vec![u16::MAX; width * height],
        }
    }

    /// Fill a triangle given as `(x, y, 1/z)` per vertex.
    fn fill(&mut self, p: [[f32; 3]; 3], colour: [[f32; 3]; 3], room: u16) {
        let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| {
            (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
        };
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-9 {
            return;
        }
        let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
        let x0 = (lo(f32::min, 0) - 0.5).floor().max(0.0) as usize;
        let x1 = ((lo(f32::max, 0) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, self.width as isize) as usize;
        let y0 = (lo(f32::min, 1) - 0.5).floor().max(0.0) as usize;
        let y1 = ((lo(f32::max, 1) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, self.height as isize) as usize;
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
                let at = y * self.width + x;
                if inv > self.inverse[at] {
                    self.inverse[at] = inv;
                    self.room[at] = room;
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
}

impl Samples {
    /// Fill a triangle given as `(x, y, 1/z)` per vertex with the texels of `image`: the
    /// corner texel coordinates `uv` and modulation factors `factor` interpolated with
    /// perspective, the sample taken from level `lod`.
    #[allow(clippy::too_many_arguments)]
    fn fill_textured(
        &mut self,
        p: [[f32; 3]; 3],
        factor: [[f32; 3]; 3],
        uv: [[f32; 2]; 3],
        image: &TextureImage,
        lod: usize,
        room: u16,
    ) {
        let edge = |a: [f32; 3], b: [f32; 3], x: f32, y: f32| {
            (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
        };
        let area = edge(p[0], p[1], p[2][0], p[2][1]);
        if area.abs() < 1e-9 {
            return;
        }
        let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
        let x0 = (lo(f32::min, 0) - 0.5).floor().max(0.0) as usize;
        let x1 = ((lo(f32::max, 0) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, self.width as isize) as usize;
        let y0 = (lo(f32::min, 1) - 0.5).floor().max(0.0) as usize;
        let y1 = ((lo(f32::max, 1) - 0.5).ceil().max(-1.0) as isize + 1)
            .clamp(0, self.height as isize) as usize;
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
                let at = y * self.width + x;
                if inv <= self.inverse[at] {
                    continue;
                }
                let (a, b, c) = (w0 * p[0][2] / inv, w1 * p[1][2] / inv, w2 * p[2][2] / inv);
                let u = a * uv[0][0] + b * uv[1][0] + c * uv[2][0];
                let v = a * uv[0][1] + b * uv[1][1] + c * uv[2][1];
                let Some(texel) = image.sample(lod, u, v) else {
                    continue;
                };
                self.inverse[at] = inv;
                self.room[at] = room;
                self.colour[at] = [0, 1, 2].map(|i| {
                    f32::from(texel[i]) * (a * factor[0][i] + b * factor[1][i] + c * factor[2][i])
                });
            }
        }
    }
}

/// The triangles of the level in world space.
pub struct World {
    tris: WorldTriangles,
    textures: Option<TextureSet>,
    /// Plan centre of each room's owned sectors (`None` for a room that draws nothing).
    pub centres: Vec<Option<[f32; 2]>>,
    /// Lowest and highest point of the level.
    pub low: f32,
    pub high: f32,
    /// A sector in runtime units.
    pub sector: f32,
}

impl World {
    pub fn new(cooked: &CookedLevel, albedo: &[[f32; 3]], textures: Option<TextureSet>) -> Self {
        let tris = WorldTriangles::new(cooked, albedo);
        let rooms = cooked.level.rooms.len();
        let mut centres = Vec::with_capacity(rooms);
        for room in 0..rooms {
            let (t, _) = triangles_of(cooked, albedo, room);
            centres.push(
                room_frame(cooked, room, &t).map(|f| [f.centre[0] as f32, f.centre[1] as f32]),
            );
        }
        let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
        for t in &tris.triangles {
            for p in &t.position {
                low = low.min(p[1]);
                high = high.max(p[1]);
            }
        }
        Self {
            tris,
            textures,
            centres,
            low,
            high,
            sector: cooked.level.sector_units as f32,
        }
    }

    pub fn rooms(&self) -> usize {
        self.tris.rooms.len()
    }

    /// Triangles in the level.
    pub fn triangles(&self) -> usize {
        self.tris.triangles.len()
    }

    /// Render the rooms `keep` names, as seen by `cam`, with `ss * ss` samples a pixel.
    pub fn render(&self, cam: &Camera, ss: usize, keep: &dyn Fn(usize) -> bool) -> Samples {
        let mut out = Samples::new(cam.width * ss, cam.height * ss);
        let near = self.sector * NEAR_SECTORS;
        let scale = ss as f32;
        for (room, (range, lo, hi)) in self.tris.rooms.iter().enumerate() {
            if range.is_empty() || !keep(room) {
                continue;
            }
            // Cull the room's box: every corner behind the near plane, or all off one side.
            let mut behind = true;
            let (mut left, mut right, mut above, mut below) = (true, true, true, true);
            for i in 0..8 {
                let corner = [
                    if i & 1 == 0 { lo[0] } else { hi[0] },
                    if i & 2 == 0 { lo[1] } else { hi[1] },
                    if i & 4 == 0 { lo[2] } else { hi[2] },
                ];
                let c = cam.to_camera(corner);
                behind &= c[2] < near;
                if c[2] >= near {
                    let p = cam.pixel(c);
                    left &= p[0] < 0.0;
                    right &= p[0] > cam.width as f32;
                    above &= p[1] < 0.0;
                    below &= p[1] > cam.height as f32;
                } else {
                    left = false;
                    right = false;
                    above = false;
                    below = false;
                }
            }
            if behind || left || right || above || below {
                continue;
            }
            for tri in &self.tris.triangles[range.clone()] {
                let centroid = [0, 1, 2]
                    .map(|k| (tri.position[0][k] + tri.position[1][k] + tri.position[2][k]) / 3.0);
                let to_eye = [0, 1, 2].map(|k| cam.eye[k] - centroid[k]);
                if !faces(&tri.quad, to_eye) {
                    continue;
                }
                let image = self
                    .textures
                    .as_ref()
                    .and_then(|t| t.images.get(tri.slot))
                    .and_then(Option::as_ref);
                if let (Some(image), Some(set)) = (image, self.textures.as_ref()) {
                    let tint = set.tint.get(tri.slot).copied().unwrap_or([1.0; 3]);
                    self.render_textured(&mut out, cam, tri, image, tint, near, scale, room);
                    continue;
                }
                let mut poly: Vec<ClipVertex> = (0..3)
                    .map(|k| ClipVertex {
                        p: tri.position[k],
                        c: tri.colour[k],
                    })
                    .collect();
                poly = clip_polygon(&poly, |v| cam.to_camera(v.p)[2] - near);
                if poly.len() < 3 {
                    continue;
                }
                let project = |v: &ClipVertex| {
                    let c = cam.to_camera(v.p);
                    let p = cam.pixel(c);
                    [p[0] * scale, p[1] * scale, 1.0 / c[2]]
                };
                let first = project(&poly[0]);
                for k in 1..poly.len() - 1 {
                    out.fill(
                        [first, project(&poly[k]), project(&poly[k + 1])],
                        [poly[0].c, poly[k].c, poly[k + 1].c],
                        room as u16,
                    );
                }
            }
        }
        out
    }
}

impl World {
    /// Draw a textured triangle into `out`: clipped at the near plane, projected, and filled
    /// from the level of the texture whose texels match the samples' size.
    #[allow(clippy::too_many_arguments)]
    fn render_textured(
        &self,
        out: &mut Samples,
        cam: &Camera,
        tri: &super::cards::WorldTriangle,
        image: &TextureImage,
        tint: [f32; 3],
        near: f32,
        scale: f32,
        room: usize,
    ) {
        #[derive(Clone, Copy)]
        struct V {
            p: [f32; 3],
            f: [f32; 3],
            uv: [f32; 2],
        }
        let light = |k: usize| {
            let word = tri.quad.color[tri.corner[k]];
            [0, 1, 2].map(|i| ((word >> (8 * i)) & 255) as f32 / 128.0 * tint[i])
        };
        let uv = |k: usize| {
            let word = tri.quad.uv[tri.corner[k]];
            [f32::from(word & 255), f32::from(word >> 8)]
        };
        let mut poly: Vec<V> = (0..3)
            .map(|k| V {
                p: tri.position[k],
                f: light(k),
                uv: uv(k),
            })
            .collect();
        // Clip against the near plane.
        let mut clipped = Vec::with_capacity(4);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (fa, fb) = (cam.to_camera(a.p)[2] - near, cam.to_camera(b.p)[2] - near);
            if fa >= 0.0 {
                clipped.push(a);
            }
            if (fa >= 0.0) != (fb >= 0.0) {
                let k = fa / (fa - fb);
                let mix3 = |x: [f32; 3], y: [f32; 3]| [0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * k);
                clipped.push(V {
                    p: mix3(a.p, b.p),
                    f: mix3(a.f, b.f),
                    uv: [0, 1].map(|i| a.uv[i] + (b.uv[i] - a.uv[i]) * k),
                });
            }
        }
        poly = clipped;
        if poly.len() < 3 {
            return;
        }
        let project = |v: &V| {
            let c = cam.to_camera(v.p);
            let p = cam.pixel(c);
            [p[0] * scale, p[1] * scale, 1.0 / c[2]]
        };
        let first = project(&poly[0]);
        for k in 1..poly.len() - 1 {
            let screen = [first, project(&poly[k]), project(&poly[k + 1])];
            let uvs = [poly[0].uv, poly[k].uv, poly[k + 1].uv];
            // Texels a sample covers: the square root of the triangle's texel area over its
            // sample area.
            let tex_area = ((uvs[1][0] - uvs[0][0]) * (uvs[2][1] - uvs[0][1])
                - (uvs[1][1] - uvs[0][1]) * (uvs[2][0] - uvs[0][0]))
                .abs();
            let sample_area = ((screen[1][0] - screen[0][0]) * (screen[2][1] - screen[0][1])
                - (screen[1][1] - screen[0][1]) * (screen[2][0] - screen[0][0]))
                .abs();
            let lod = if sample_area > 1e-6 {
                image.level_for((tex_area / sample_area).sqrt())
            } else {
                0
            };
            out.fill_textured(
                screen,
                [poly[0].f, poly[k].f, poly[k + 1].f],
                uvs,
                image,
                lod,
                room as u16,
            );
        }
    }
}

/// The picture of a render at screen size: mean colour of the covered samples, coverage
/// `0..=1`, depth of the nearest sample and its room.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f32; 3]>,
    pub coverage: Vec<f32>,
    pub depth: Vec<f32>,
    pub room: Vec<u16>,
}

impl Samples {
    /// Average `ss * ss` samples into one pixel.
    pub fn resolve(&self, ss: usize) -> Picture {
        let (w, h) = (self.width / ss, self.height / ss);
        let mut pic = Picture {
            width: w,
            height: h,
            rgb: vec![[0.0; 3]; w * h],
            coverage: vec![0.0; w * h],
            depth: vec![f32::INFINITY; w * h],
            room: vec![u16::MAX; w * h],
        };
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut count, mut best) = ([0.0f32; 3], 0usize, 0.0f32);
                for sy in 0..ss {
                    for sx in 0..ss {
                        let at = (y * ss + sy) * self.width + x * ss + sx;
                        if self.inverse[at] > 0.0 {
                            count += 1;
                            for (total, c) in sum.iter_mut().zip(self.colour[at]) {
                                *total += c;
                            }
                            if self.inverse[at] > best {
                                best = self.inverse[at];
                                pic.room[y * w + x] = self.room[at];
                            }
                        }
                    }
                }
                let at = y * w + x;
                if count > 0 {
                    pic.rgb[at] = sum.map(|c| c / count as f32);
                    pic.coverage[at] = count as f32 / (ss * ss) as f32;
                    pic.depth[at] = 1.0 / best;
                }
            }
        }
        pic
    }
}

/// The shape of a panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelShape {
    /// Focal length in texels.
    pub focal: f32,
    pub tile_cols: usize,
    pub tile_rows: usize,
    /// Texel row of the horizon.
    pub horizon: f32,
    /// Mesh cells per tile, across and down.
    pub cells: [usize; 2],
}

impl PanelShape {
    pub fn width(&self) -> usize {
        self.tile_cols * TILE_W
    }
    pub fn height(&self) -> usize {
        self.tile_rows * TILE_H
    }
    /// Mesh vertices across and down.
    pub fn mesh(&self) -> [usize; 2] {
        [
            self.tile_cols * self.cells[0] + 1,
            self.tile_rows * self.cells[1] + 1,
        ]
    }
}

/// Which way the camera's right and down point, relative to forward: fixed by the game's
/// rotation (checked against its own matrices by the fit example).
pub const RIGHT_SIGN: f32 = 1.0;
/// World `y` of the camera's down axis (`+1` when `y` grows downwards in camera rows).
pub const DOWN_Y: f32 = -1.0;

/// The camera of heading `k` at `eye` for a panel of `shape`: the game's own pinhole, looking
/// along `k * 45` degrees in the `(x, z)` plane, the horizon `shape.horizon` rows from the top.
pub fn panel_camera(eye: [f32; 3], heading: usize, shape: &PanelShape) -> Camera {
    let a = heading as f32 * std::f32::consts::FRAC_PI_4;
    let (c, s) = (a.cos(), a.sin());
    let forward = [c, 0.0, s];
    let right = [-s * RIGHT_SIGN, 0.0, c * RIGHT_SIGN];
    let down = [0.0, DOWN_Y, 0.0];
    Camera {
        eye,
        rows: [right, down, forward],
        focal: shape.focal,
        width: shape.width(),
        height: shape.height(),
        cx: shape.width() as f32 / 2.0,
        cy: shape.horizon,
    }
}

/// How the depth at a mesh vertex is picked from the samples around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthRule {
    Min,
    Median,
    Max,
}

/// One baked panel.
#[derive(Clone, Debug)]
pub struct Panel {
    pub eye: [f32; 3],
    pub heading: usize,
    pub shape: PanelShape,
    /// Per tile (row-major), its shown RGBA after the 16-colour quantiser, `None` when empty.
    pub tiles: Vec<Option<Vec<[u8; 4]>>>,
    /// Depth along the heading at each mesh vertex (row-major).
    pub depth: Vec<f32>,
    /// Per cell (`tile * cells + cy * CELLS_X + cx`, tiles column-major as the game has
    /// them), the one room whose geometry is all the cell shows, `CELL_EMPTY` for a cell with
    /// nothing in it, `CELL_MIXED` for any other.
    pub rooms: Vec<u8>,
}

/// Bake the panel of `heading` at `eye`: the rooms `keep` names, seen with `ss * ss` samples a
/// texel.
pub fn bake_panel(
    world: &World,
    eye: [f32; 3],
    heading: usize,
    shape: &PanelShape,
    keep: &dyn Fn(usize) -> bool,
    ss: usize,
    rule: DepthRule,
) -> Panel {
    let cam = panel_camera(eye, heading, shape);
    let samples = world.render(&cam, ss, keep);
    let pic = samples.resolve(ss);
    let w = shape.width();
    let mut tiles = Vec::with_capacity(shape.tile_cols * shape.tile_rows);
    for ty in 0..shape.tile_rows {
        for tx in 0..shape.tile_cols {
            let mut rgba = Vec::with_capacity(TILE_W * TILE_H);
            let mut any = false;
            for y in 0..TILE_H {
                for x in 0..TILE_W {
                    let at = (ty * TILE_H + y) * w + tx * TILE_W + x;
                    // A texel any sample of which hits geometry is drawn: at a texel of several
                    // pixels a thin tower would otherwise fall under a quarter coverage and vanish.
                    if pic.coverage[at] > 0.0 {
                        any = true;
                        let c = pic.rgb[at].map(|v| v.clamp(0.0, 255.0) as u8);
                        rgba.push([c[0], c[1], c[2], 255]);
                    } else {
                        rgba.push([0, 0, 0, 0]);
                    }
                }
            }
            if !any {
                tiles.push(None);
                continue;
            }
            let (palette, indices) = quantize_rgba_with_transparent_zero(&rgba, 16)
                .unwrap_or_else(|_| (vec![[0; 3]; 16], vec![0; TILE_W * TILE_H]));
            let shown = indices
                .iter()
                .map(|&i| {
                    if i == 0 {
                        [0, 0, 0, 0]
                    } else {
                        let c = palette.get(usize::from(i)).copied().unwrap_or([0; 3]);
                        [c[0] & 0xf8, c[1] & 0xf8, c[2] & 0xf8, 255]
                    }
                })
                .collect();
            tiles.push(Some(shown));
        }
    }
    // Depth mesh: for each vertex the depths of the covered samples within half a cell.
    let [mw, mh] = shape.mesh();
    let cell_w = (TILE_W / shape.cells[0]) as f32;
    let cell_h = (TILE_H / shape.cells[1]) as f32;
    let mut depth = vec![f32::NAN; mw * mh];
    for j in 0..mh {
        for i in 0..mw {
            let (u, v) = (i as f32 * cell_w, j as f32 * cell_h);
            let (x0, x1) = (
                ((u - cell_w / 2.0) * ss as f32).max(0.0) as usize,
                (((u + cell_w / 2.0) * ss as f32) as usize).min(samples.width),
            );
            let (y0, y1) = (
                ((v - cell_h / 2.0) * ss as f32).max(0.0) as usize,
                (((v + cell_h / 2.0) * ss as f32) as usize).min(samples.height),
            );
            let mut zs = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    let inv = samples.inverse[y * samples.width + x];
                    if inv > 0.0 {
                        zs.push(1.0 / inv);
                    }
                }
            }
            if zs.is_empty() {
                continue;
            }
            zs.sort_by(f32::total_cmp);
            depth[j * mw + i] = match rule {
                DepthRule::Min => zs[0],
                DepthRule::Median => zs[zs.len() / 2],
                DepthRule::Max => zs[zs.len() - 1],
            };
        }
    }
    // Vertices with nothing around them take the mean of their neighbours, repeatedly, then
    // a far default.
    for _ in 0..4 {
        let snapshot = depth.clone();
        for j in 0..mh {
            for i in 0..mw {
                if !snapshot[j * mw + i].is_nan() {
                    continue;
                }
                let (mut sum, mut n) = (0.0f32, 0);
                for (di, dj) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let (a, b) = (i as i32 + di, j as i32 + dj);
                    if a < 0 || b < 0 || a >= mw as i32 || b >= mh as i32 {
                        continue;
                    }
                    let d = snapshot[b as usize * mw + a as usize];
                    if !d.is_nan() {
                        sum += d;
                        n += 1;
                    }
                }
                if n > 0 {
                    depth[j * mw + i] = sum / n as f32;
                }
            }
        }
    }
    for d in &mut depth {
        if d.is_nan() {
            *d = 500.0 * world.sector;
        }
    }
    // The room of each cell: the rooms of the covered samples in its box.
    let mut rooms = vec![shared::CELL_EMPTY; shared::CELLS];
    for tx in 0..shape.tile_cols {
        for ty in 0..shape.tile_rows {
            let tile = tx * shape.tile_rows + ty;
            for cy in 0..shape.cells[1] {
                for cx in 0..shape.cells[0] {
                    let (x0, x1) = (
                        (tx * TILE_W + cx * (TILE_W / shape.cells[0])) * ss,
                        (tx * TILE_W + (cx + 1) * (TILE_W / shape.cells[0])) * ss,
                    );
                    let (y0, y1) = (
                        (ty * TILE_H + cy * (TILE_H / shape.cells[1])) * ss,
                        (ty * TILE_H + (cy + 1) * (TILE_H / shape.cells[1])) * ss,
                    );
                    let mut room: Option<u16> = None;
                    let mut mixed = false;
                    for y in y0..y1.min(samples.height) {
                        for x in x0..x1.min(samples.width) {
                            let at = y * samples.width + x;
                            if samples.inverse[at] > 0.0 {
                                match room {
                                    None => room = Some(samples.room[at]),
                                    Some(r) if r != samples.room[at] => mixed = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                    let k = tile * shape.cells[0] * shape.cells[1] + cy * shape.cells[0] + cx;
                    rooms[k] = match room {
                        None => shared::CELL_EMPTY,
                        Some(r) if !mixed && r < u16::from(shared::CELL_MIXED) => r as u8,
                        Some(_) => shared::CELL_MIXED,
                    };
                }
            }
        }
    }
    Panel {
        eye,
        heading,
        shape: *shape,
        tiles,
        depth,
        rooms,
    }
}

/// A flat screen layer the host draw writes into.
pub struct Layer {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f32; 3]>,
    pub covered: Vec<bool>,
    /// For each quad drawn: its depth and the screen pixels one texel of it takes (the square
    /// root of screen area over texel area).
    pub density: Vec<(f32, f32)>,
}

/// Twice the area of a polygon given as points.
fn area(p: &[[f32; 2]]) -> f32 {
    let mut sum = 0.0;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        sum += a[0] * b[1] - a[1] * b[0];
    }
    (sum / 2.0).abs()
}

impl Layer {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgb: vec![[0.0; 3]; width * height],
            covered: vec![false; width * height],
            density: Vec::new(),
        }
    }
}

/// Draw `panel` for the camera `cam` as the game would: every cell of every present tile one
/// textured quad at its depth, painter ordered far to near, nearest-texel sampling with the
/// GPU's affine interpolation. `flat_depth` replaces every depth with one value (the panel as
/// a flat picture).
pub fn draw_panel(
    layer: &mut Layer,
    cam: &Camera,
    panel: &Panel,
    columns: Option<(f32, f32)>,
    flat_depth: Option<f32>,
    near: f32,
) {
    let shape = &panel.shape;
    let pc = panel_camera(panel.eye, panel.heading, shape);
    let [mw, _] = shape.mesh();
    let cell_w = (TILE_W / shape.cells[0]) as f32;
    let cell_h = (TILE_H / shape.cells[1]) as f32;
    let vertex = |i: usize, j: usize| -> [f32; 3] {
        let z = flat_depth.unwrap_or(panel.depth[j * mw + i]);
        let (u, v) = (i as f32 * cell_w, j as f32 * cell_h);
        let ray = [0, 1, 2].map(|k| {
            pc.rows[2][k]
                + (u - pc.cx) / pc.focal * pc.rows[0][k]
                + (v - pc.cy) / pc.focal * pc.rows[1][k]
        });
        [
            panel.eye[0] + z * ray[0],
            panel.eye[1] + z * ray[1],
            panel.eye[2] + z * ray[2],
        ]
    };
    struct Cell {
        z: f32,
        screen: [[f32; 2]; 4],
        uv: [[f32; 2]; 4],
        tile: usize,
    }
    let mut cells = Vec::new();
    for ty in 0..shape.tile_rows {
        for tx in 0..shape.tile_cols {
            let tile = ty * shape.tile_cols + tx;
            if panel.tiles[tile].is_none() {
                continue;
            }
            for cy in 0..shape.cells[1] {
                for cx in 0..shape.cells[0] {
                    let (i, j) = (tx * shape.cells[0] + cx, ty * shape.cells[1] + cy);
                    // A column band limit: the cell's centre azimuth in the panel's own
                    // camera, as `tan` of the angle off the heading.
                    if let Some((lo, hi)) = columns {
                        let u = (i as f32 + 0.5) * cell_w;
                        let t = (u - pc.cx) / pc.focal;
                        // Angle off the heading.
                        let angle = t.atan();
                        if angle < lo || angle >= hi {
                            continue;
                        }
                    }
                    let corners = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                    let cams: Vec<[f32; 3]> = corners
                        .iter()
                        .map(|&(a, b)| cam.to_camera(vertex(a, b)))
                        .collect();
                    if cams.iter().any(|c| c[2] < near) {
                        continue;
                    }
                    let screen = [0, 1, 2, 3].map(|k| cam.pixel(cams[k]));
                    // Off-screen cells are skipped.
                    let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                    for p in &screen {
                        x0 = x0.min(p[0]);
                        x1 = x1.max(p[0]);
                        y0 = y0.min(p[1]);
                        y1 = y1.max(p[1]);
                    }
                    if x1 < 0.0 || y1 < 0.0 || x0 > cam.width as f32 || y0 > cam.height as f32 {
                        continue;
                    }
                    let z = (cams[0][2] + cams[1][2] + cams[2][2] + cams[3][2]) / 4.0;
                    // Texel coordinates inside the tile.
                    let uv = [
                        [cx as f32 * cell_w, cy as f32 * cell_h],
                        [(cx + 1) as f32 * cell_w, cy as f32 * cell_h],
                        [(cx + 1) as f32 * cell_w, (cy + 1) as f32 * cell_h],
                        [cx as f32 * cell_w, (cy + 1) as f32 * cell_h],
                    ];
                    cells.push(Cell {
                        z,
                        screen,
                        uv,
                        tile,
                    });
                }
            }
        }
    }
    cells.sort_by(|a, b| b.z.total_cmp(&a.z));
    for cell in &cells {
        let tile = panel.tiles[cell.tile].as_ref().expect("present tile");
        let texels = area(&cell.uv);
        if texels > 0.0 {
            layer
                .density
                .push((cell.z, (area(&cell.screen) / texels).sqrt()));
        }
        for tri in [[0usize, 1, 2], [0, 2, 3]] {
            fill_textured(
                layer,
                [0, 1, 2].map(|k| cell.screen[tri[k]]),
                [0, 1, 2].map(|k| cell.uv[tri[k]]),
                tile,
            );
        }
    }
}

/// Fill a screen triangle with texels of a tile, interpolating the texel coordinates linearly in
/// screen space as the PS1 GPU does; transparent texels leave the layer alone.
fn fill_textured(layer: &mut Layer, p: [[f32; 2]; 3], uv: [[f32; 2]; 3], tile: &[[u8; 4]]) {
    let edge = |a: [f32; 2], b: [f32; 2], x: f32, y: f32| {
        (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
    };
    let area = edge(p[0], p[1], p[2][0], p[2][1]);
    if area.abs() < 1e-9 {
        return;
    }
    let lo = |f: fn(f32, f32) -> f32, k: usize| f(f(p[0][k], p[1][k]), p[2][k]);
    let x0 = (lo(f32::min, 0) - 0.5).floor().max(0.0) as usize;
    let x1 = ((lo(f32::max, 0) - 0.5).ceil().max(-1.0) as isize + 1).clamp(0, layer.width as isize)
        as usize;
    let y0 = (lo(f32::min, 1) - 0.5).floor().max(0.0) as usize;
    let y1 = ((lo(f32::max, 1) - 0.5).ceil().max(-1.0) as isize + 1).clamp(0, layer.height as isize)
        as usize;
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
            let u = w0 * uv[0][0] + w1 * uv[1][0] + w2 * uv[2][0];
            let v = w0 * uv[0][1] + w1 * uv[1][1] + w2 * uv[2][1];
            let (tu, tv) = (
                (u.floor() as isize).clamp(0, TILE_W as isize - 1) as usize,
                (v.floor() as isize).clamp(0, TILE_H as isize - 1) as usize,
            );
            let t = tile[tv * TILE_W + tu];
            if t[3] == 0 {
                continue;
            }
            let at = y * layer.width + x;
            layer.rgb[at] = [f32::from(t[0]), f32::from(t[1]), f32::from(t[2])];
            layer.covered[at] = true;
        }
    }
}

/// Draw one textured quad (corners clockwise from the top left) with `tile` as its picture.
pub fn draw_textured_quad(
    layer: &mut Layer,
    screen: [[f32; 2]; 4],
    uv: [[f32; 2]; 4],
    tile: &[[u8; 4]],
    depth: f32,
) {
    let texels = area(&uv);
    if texels > 0.0 {
        layer.density.push((depth, (area(&screen) / texels).sqrt()));
    }
    for tri in [[0usize, 1, 2], [0, 2, 3]] {
        fill_textured(
            layer,
            [0, 1, 2].map(|k| screen[tri[k]]),
            [0, 1, 2].map(|k| uv[tri[k]]),
            tile,
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The level's panels: what the cook writes and the game reads.
// ---------------------------------------------------------------------------------------------

use psx_grid::render as shared;

/// A bake eye: where a panel set is rendered from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeSpec {
    /// The room the eye stands in (the flood that finds the rooms drawn whole starts there).
    pub room: usize,
    /// Position, runtime units.
    pub eye: [f32; 3],
}

/// One panel of the disc, as bytes.
#[derive(Clone, Debug, Default)]
pub struct DiscPanel {
    /// Tile columns' row masks (bit `row` of `cols[col]` is set when that tile is stored).
    pub cols: [u8; shared::COLS],
    /// The header sector, then each stored tile's sector in column-major order.
    pub sectors: Vec<u8>,
}

impl DiscPanel {
    /// Sectors the panel takes (the header and its tiles).
    pub fn sector_count(&self) -> usize {
        self.sectors.len() / psx_grid::render::SLOT_BYTES
    }
}

/// The depth byte of a depth along the heading, runtime units.
fn depth_byte_of(depth: f32, sector: f32) -> u8 {
    let sixteenths = (depth / sector * (1 << shared::DEPTH_SECTOR_SHIFT) as f32).round();
    shared::depth_byte(sixteenths.clamp(0.0, 1.0e6) as u32)
}

/// The game's panel shape for a projection plane of `focal` pixels.
pub fn game_shape(focal: f32) -> PanelShape {
    PanelShape {
        focal,
        tile_cols: shared::COLS,
        tile_rows: shared::ROWS,
        horizon: shared::HORIZON_ROW as f32,
        cells: [shared::CELLS_X, shared::CELLS_Y],
    }
}

/// Pack a baked panel as the disc holds it.
pub fn to_disc(panel: &Panel, sector: f32) -> DiscPanel {
    let shape = &panel.shape;
    let mut out = DiscPanel::default();
    let bytes = psx_grid::render::SLOT_BYTES;
    let mut header = vec![0u8; bytes];
    for (k, depth) in panel.depth.iter().enumerate().take(shared::MESH) {
        header[k] = depth_byte_of(*depth, sector);
    }
    header[shared::HEADER_ROOMS_AT..shared::HEADER_ROOMS_AT + shared::CELLS]
        .copy_from_slice(&panel.rooms);
    out.sectors.extend_from_slice(&header);
    for col in 0..shape.tile_cols {
        for row in 0..shape.tile_rows {
            let Some(rgba) = panel.tiles[row * shape.tile_cols + col].as_ref() else {
                continue;
            };
            out.cols[col] |= 1 << row;
            let (palette, indices) = quantize_rgba_with_transparent_zero(rgba, 16)
                .unwrap_or_else(|_| (vec![[0; 3]; 16], vec![0; TILE_W * TILE_H]));
            out.sectors
                .extend_from_slice(&super::cards::encode(&indices, &palette));
        }
    }
    out
}

/// Every panel of every eye, baked in parallel: `HEADINGS` per eye, `None` where the heading
/// shows nothing beyond the rooms drawn whole.
///
/// `exclude` says, per eye, which rooms the game draws whole from anywhere within `slack` of
/// the eye (they are left out of its panels).
pub fn bake_eyes(
    world: &World,
    eyes: &[EyeSpec],
    focal: f32,
    exclude: &(dyn Fn(&EyeSpec) -> Vec<bool> + Sync),
) -> Vec<[Option<DiscPanel>; shared::HEADINGS]> {
    let shape = game_shape(focal);
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 4)
        .min(eyes.len().max(1));
    let mut out: Vec<[Option<DiscPanel>; shared::HEADINGS]> =
        (0..eyes.len()).map(|_| Default::default()).collect();
    let shape = &shape;
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..eyes.len())
                        .step_by(threads)
                        .map(|i| {
                            let spec = &eyes[i];
                            let drawn = exclude(spec);
                            let keep = |room: usize| !drawn.get(room).copied().unwrap_or(false);
                            let mut set: [Option<DiscPanel>; shared::HEADINGS] = Default::default();
                            for (heading, slot) in set.iter_mut().enumerate() {
                                let panel = bake_panel(
                                    world,
                                    spec.eye,
                                    heading,
                                    shape,
                                    &keep,
                                    2,
                                    DepthRule::Median,
                                );
                                if panel.tiles.iter().any(Option::is_some) {
                                    *slot = Some(to_disc(&panel, world.sector));
                                }
                            }
                            (i, set)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for worker in workers {
            for (i, set) in worker.join().expect("a panel bake thread panicked") {
                out[i] = set;
            }
        }
    });
    out
}

/// Each eye's panels as one block of sectors: a sector that lists which tiles each heading
/// stores (`HEADINGS` times `COLS` row masks), then the panels one after another, each its
/// header sector and its stored tiles. An eye with no panel has an empty block.
pub fn eye_blocks(panels: &[[Option<DiscPanel>; shared::HEADINGS]]) -> Vec<Vec<u8>> {
    panels
        .iter()
        .map(|set| {
            if set.iter().all(Option::is_none) {
                return Vec::new();
            }
            let mut block = vec![0u8; shared::SLOT_BYTES];
            for (heading, panel) in set.iter().enumerate() {
                if let Some(panel) = panel {
                    block[heading * shared::COLS..(heading + 1) * shared::COLS]
                        .copy_from_slice(&panel.cols);
                }
            }
            // The headers of the panels, then the tiles of the rows on screen with the camera
            // level, panel after panel, then the other rows' tiles: a view is a stretch of the
            // disc, and the drive seeks once for it (the guest derives every sector from the
            // list's row masks, see `panel_stream::EyeList::parse`).
            let bytes = shared::SLOT_BYTES;
            for panel in set.iter().flatten() {
                block.extend_from_slice(&panel.sectors[..bytes]);
            }
            for outer in [false, true] {
                for panel in set.iter().flatten() {
                    let mut at = 1;
                    for col in 0..shared::COLS {
                        for row in 0..shared::ROWS {
                            if panel.cols[col] >> row & 1 == 0 {
                                continue;
                            }
                            let central = shared::CENTRAL_ROWS >> row & 1 != 0;
                            if central != outer {
                                block.extend_from_slice(
                                    &panel.sectors[at * bytes..(at + 1) * bytes],
                                );
                            }
                            at += 1;
                        }
                    }
                }
            }
            block
        })
        .collect()
}

/// The order rooms and eyes' blocks go in the pack: along a Hilbert curve through their plan
/// positions, so the drive at a room is near the panels of the eyes that stand with it.
/// Returns `(is_eye, index)`; room 0 is left to the caller (it goes first). `None` centres
/// (rooms that draw nothing) go last.
pub fn pack_order(
    room_centres: &[Option<[i32; 2]>],
    eye_centres: &[Option<[i32; 2]>],
) -> Vec<(bool, usize)> {
    let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
    for c in room_centres.iter().chain(eye_centres).flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1) as f32;
    let key = |c: &[i32; 2]| {
        let cell = |v: i32, lo: i32| (((v - lo) as f32 / span) * 255.0) as u32;
        super::cards::hilbert_index(cell(c[0], lo[0]), cell(c[1], lo[1]))
    };
    let mut items: Vec<(u32, bool, usize)> = Vec::new();
    let mut loose = Vec::new();
    for (room, c) in room_centres.iter().enumerate().skip(1) {
        match c {
            Some(c) => items.push((key(c), false, room)),
            None => loose.push((false, room)),
        }
    }
    for (eye, c) in eye_centres.iter().enumerate() {
        if let Some(c) = c {
            // An eye's panels follow the rooms that share its curve cell.
            items.push((key(c), true, eye));
        }
    }
    items.sort_by_key(|&(k, is_eye, i)| (k, is_eye, i));
    items
        .into_iter()
        .map(|(_, e, i)| (e, i))
        .chain(loose)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc_panel(cols: [u8; shared::COLS], tiles: usize) -> DiscPanel {
        DiscPanel {
            cols,
            sectors: vec![7u8; (1 + tiles) * shared::SLOT_BYTES],
        }
    }

    #[test]
    fn an_eye_block_lists_the_tiles_of_each_heading_then_holds_the_panels() {
        let mut set: [Option<DiscPanel>; shared::HEADINGS] = Default::default();
        set[0] = Some(disc_panel([3, 0, 0, 0, 1], 3));
        set[5] = Some(disc_panel([0, 0, 127, 0, 0], 7));
        let blocks = eye_blocks(&[set, Default::default()]);
        assert_eq!(blocks.len(), 2);
        // The list sector, then the two panels (a header and their tiles each).
        assert_eq!(blocks[0].len(), (1 + 4 + 8) * shared::SLOT_BYTES);
        assert_eq!(&blocks[0][0..5], &[3, 0, 0, 0, 1]);
        assert_eq!(
            &blocks[0][5 * shared::COLS..5 * shared::COLS + 5],
            &[0, 0, 127, 0, 0]
        );
        assert!(blocks[0][5..5 * shared::COLS].iter().all(|&b| b == 0));
        // An eye with nothing to show has no block.
        assert!(blocks[1].is_empty());
    }

    #[test]
    fn rooms_and_eyes_go_in_the_pack_along_one_curve_and_room_zero_is_left_to_the_caller() {
        let rooms = [Some([0, 0]), Some([100, 100]), None, Some([4000, 4000])];
        let eyes = [Some([120, 90]), None];
        let order = pack_order(&rooms, &eyes);
        // Room 0 is not listed, the room with no centre goes last, an eye with no block is
        // left out, and the eye stands beside the room it is near.
        assert!(!order.contains(&(false, 0)));
        assert_eq!(order.last(), Some(&(false, 2)));
        assert!(!order.iter().any(|&(is_eye, i)| is_eye && i == 1));
        let at = |item: (bool, usize)| order.iter().position(|&o| o == item).unwrap();
        assert!(at((true, 0)).abs_diff(at((false, 1))) == 1, "{order:?}");
        assert!(at((false, 3)) > at((false, 1)));
    }

    #[test]
    fn a_depth_in_runtime_units_round_trips_through_its_byte() {
        let sector = 80.0;
        for sectors in [3.0f32, 12.5, 24.0, 60.0, 100.0] {
            let byte = depth_byte_of(sectors * sector, sector);
            let back = f32::from(psx_grid::render::depth_units(byte) as u16)
                / (1 << shared::DEPTH_SECTOR_SHIFT) as f32;
            assert!(
                (back - sectors).abs() <= sectors * 0.02,
                "{sectors}: {back}"
            );
        }
    }

    #[test]
    fn a_texture_has_a_chain_of_filtered_levels_and_wraps() {
        // 4 by 4 texels, 8 bit: the left half index 1 (red), the right half index 2 (blue),
        // index 0 transparent nowhere.
        let mut pixels = vec![0u8; 16];
        for y in 0..4 {
            for x in 0..4 {
                pixels[y * 4 + x] = if x < 2 { 1 } else { 2 };
            }
        }
        let palette = [[0, 0, 0], [248, 0, 0], [0, 0, 248]];
        let psxt = psxed_tex::encode_indexed_psxt(
            4,
            4,
            psxed_format::texture::Depth::Bit8,
            &pixels,
            &palette,
            false,
        )
        .expect("a texture");
        let image = TextureImage::from_psxt(&psxt).expect("decoded");
        assert_eq!(image.width(), 4);
        // Level 0 keeps the halves; wrapping reads the other side.
        assert_eq!(image.sample(0, 0.5, 0.5).map(|c| c[0] > 200), Some(true));
        assert_eq!(image.sample(0, 3.5, 0.5).map(|c| c[2] > 200), Some(true));
        assert_eq!(image.sample(0, 4.5, 0.5).map(|c| c[0] > 200), Some(true));
        assert_eq!(image.sample(0, -0.5, 0.5).map(|c| c[2] > 200), Some(true));
        // The coarsest level is the mean of both.
        let mean = image.sample(2, 0.0, 0.0).expect("a texel");
        assert!(mean[0] > 100 && mean[2] > 100, "{mean:?}");
        // A surface that packs four texels to a sample reads two levels down.
        assert_eq!(image.level_for(4.0), 2);
        assert_eq!(image.level_for(0.5), 0);
    }
}
