//! One UV model for every grid face.
//!
//! A face's texture placement is a [`FaceUv`]: a *mode* that lays the texture
//! over the face's *frame*, quarter turns and flips on that frame, then the
//! fine offset, free rotation and scale.
//!
//! * **World** gives a texel per world position, so neighbouring faces line up
//!   and a bigger face tiles more. 32 texels per sector (half a 64 texel
//!   tile) is the mapping every older face has and stays bit for bit what it
//!   was.
//! * **Fit** puts a fixed number of tiles across the whole face: 2 by 2 is
//!   "2x2 on this face", whatever its size.
//!
//! The frame is the logical face the user selects: the sector square of a
//! floor or ceiling, the wall record of an authored wall, the whole *run* of
//! a derived wall (so "8x1" repeats eight times along the whole run, not per
//! cell) and the rectangle of a plane. Frames are measured in the face's own
//! grid-unit coordinates: `(x, z)` for floors, `(distance along the viewing
//! direction, minus height)` for walls, `(s, t)` from the top-left corner for
//! planes. The viewer sees `s` growing to the right and `t` growing down.
//!
//! [`report`] describes the result in the terms the Inspector prints and the
//! cook acts on: how many tiles land on the face and how many primitives the
//! four-tiles-per-primitive limit cuts it into.

use crate::face_uv::{FaceUv, UvMode};

use super::units::SECTOR_UNITS;

/// Texels one material repeats over (the grid renderer tiles 64 by 64
/// textures; the cook warns for other sizes).
pub const TILE_TEXELS: f64 = 64.0;

/// Most tiles per axis one primitive holds when it repeats its texture
/// through the GPU texture window (see [`super::uv_split`]).
pub const MAX_TILES_PER_PRIMITIVE: u32 = super::uv_split::MAX_TILES_PER_PRIMITIVE as u32;

/// The rectangle of grid units a face's texture is measured against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Smallest `(s, t)`.
    pub min: [f64; 2],
    /// Largest `(s, t)`.
    pub max: [f64; 2],
}

impl Frame {
    /// The frame holding every point of `points`.
    pub fn around(points: impl IntoIterator<Item = [f64; 2]>) -> Self {
        let mut frame = Self {
            min: [f64::MAX; 2],
            max: [f64::MIN; 2],
        };
        for p in points {
            for (axis, value) in p.into_iter().enumerate() {
                frame.min[axis] = frame.min[axis].min(value);
                frame.max[axis] = frame.max[axis].max(value);
            }
        }
        frame
    }

    /// Extent along each axis.
    pub fn size(&self) -> [f64; 2] {
        [self.max[0] - self.min[0], self.max[1] - self.min[1]]
    }

    /// Centre point.
    pub fn centre(&self) -> [f64; 2] {
        [
            (self.min[0] + self.max[0]) / 2.0,
            (self.min[1] + self.max[1]) / 2.0,
        ]
    }
}

/// The mode a face actually uses: its own, else its material's default, else
/// the legacy world mapping.
pub fn resolve(mode: UvMode, material_default: Option<UvMode>) -> UvMode {
    match mode {
        UvMode::Inherit => match material_default {
            Some(UvMode::Inherit) | None => UvMode::LEGACY,
            Some(other) => other,
        },
        other => other,
    }
}

/// `frame` widened by a face's [`FaceUv::grow`].
pub fn widen(frame: Frame, grow: [[i32; 2]; 2]) -> Frame {
    Frame {
        min: [
            frame.min[0] - f64::from(grow[0][0]),
            frame.min[1] - f64::from(grow[0][1]),
        ],
        max: [
            frame.max[0] + f64::from(grow[1][0]),
            frame.max[1] + f64::from(grow[1][1]),
        ],
    }
}

/// Texel density per grid unit of a world mode.
fn density(texels_per_sector: [u16; 2]) -> [f64; 2] {
    texels_per_sector.map(|t| f64::from(t) / f64::from(SECTOR_UNITS))
}

/// Maps points of one face to texel coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Mapper {
    mode: UvMode,
    turns: u8,
    flip: [bool; 2],
    frame: Frame,
    fine: FaceUv,
    legacy_centre: [f64; 2],
}

impl Mapper {
    /// A mapper for a face with placement `uv` and `frame`. `legacy_centre`
    /// is the texel the fine rotation and scale turn about for a plain world
    /// mapping, which older faces computed their own way.
    pub fn new(
        uv: &FaceUv,
        material_default: Option<UvMode>,
        frame: Frame,
        legacy_centre: [f64; 2],
    ) -> Self {
        Self {
            mode: resolve(uv.mode, material_default),
            turns: uv.turns & 3,
            flip: uv.flip,
            frame: widen(frame, uv.grow),
            fine: *uv,
            legacy_centre,
        }
    }

    /// Whether this is the legacy mapping exactly: world aligned at 32
    /// texels per sector with no turn and no flip.
    pub fn is_legacy(&self) -> bool {
        self.mode == UvMode::LEGACY && self.turns == 0 && self.flip == [false; 2]
    }

    /// Whether this is the plain legacy mapping (world mode, no turn, no
    /// flip).
    fn plain_world(&self) -> Option<[f64; 2]> {
        match self.mode {
            UvMode::World { texels_per_sector } if self.turns == 0 && self.flip == [false; 2] => {
                Some(density(texels_per_sector))
            }
            _ => None,
        }
    }

    /// Texel of frame fractions `(a, b)` (each `0..=1`, as the viewer sees
    /// them) in a fit mapping, before the fine controls.
    fn fit_texel(&self, repeats_q4: [u16; 2], a: f64, b: f64) -> [f64; 2] {
        let (a, b) = (
            if self.flip[0] { 1.0 - a } else { a },
            if self.flip[1] { 1.0 - b } else { b },
        );
        let rh = f64::from(repeats_q4[0]) / 16.0;
        let rv = f64::from(repeats_q4[1]) / 16.0;
        let [u, v] = match self.turns {
            0 => [a * rh, b * rv],
            1 => [b * rv, (1.0 - a) * rh],
            2 => [(1.0 - a) * rh, (1.0 - b) * rv],
            _ => [(1.0 - b) * rv, a * rh],
        };
        [u * TILE_TEXELS, v * TILE_TEXELS]
    }

    fn base(&self, p: [f64; 2]) -> [f64; 2] {
        if let Some(k) = self.plain_world() {
            return [p[0] * k[0], p[1] * k[1]];
        }
        match self.mode {
            UvMode::World { texels_per_sector } => {
                let k = density(texels_per_sector);
                let c = self.frame.centre();
                let mut d = [p[0] - c[0], p[1] - c[1]];
                for (axis, flipped) in self.flip.iter().enumerate() {
                    if *flipped {
                        d[axis] = -d[axis];
                    }
                }
                let [du, dv] = match self.turns {
                    0 => [d[0] * k[0], d[1] * k[1]],
                    1 => [d[1] * k[0], -d[0] * k[1]],
                    2 => [-d[0] * k[0], -d[1] * k[1]],
                    _ => [-d[1] * k[0], d[0] * k[1]],
                };
                [c[0] * k[0] + du, c[1] * k[1] + dv]
            }
            UvMode::Fit { repeats_q4 } => {
                let size = self.frame.size();
                let frac = |axis: usize| {
                    if size[axis] > 0.0 {
                        (p[axis] - self.frame.min[axis]) / size[axis]
                    } else {
                        0.0
                    }
                };
                self.fit_texel(repeats_q4, frac(0), frac(1))
            }
            UvMode::Inherit => p,
        }
    }

    /// The texel the fine rotation and scale turn about.
    fn centre(&self) -> [f64; 2] {
        if self.plain_world().is_some() {
            return self.legacy_centre;
        }
        self.base(self.frame.centre())
    }

    /// Texel coordinates of the point `p` of the face.
    pub fn texel(&self, p: [f64; 2]) -> [f64; 2] {
        let raw = self.base(p);
        if self.fine.fine_is_identity() {
            return raw;
        }
        let centre = self.centre();
        let local = [raw[0] - centre[0], raw[1] - centre[1]];
        let linear = self.fine.apply_linear(local);
        [
            linear[0] + centre[0] + f64::from(self.fine.offset_texels[0]),
            linear[1] + centre[1] + f64::from(self.fine.offset_texels[1]),
        ]
    }
}

/// What the tiling of one face comes to, for the Inspector and the cook.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvReport {
    /// The mode in force after the material default.
    pub mode: UvMode,
    /// Whether the mode came from the material.
    pub from_material: bool,
    /// Tiles across the face as the viewer sees it: horizontal, vertical.
    pub tiles: [f32; 2],
    /// Primitives the face needs along the texture's two axes.
    pub pieces: [u32; 2],
    /// Primitives added by the split: `pieces[0] * pieces[1] - 1`.
    pub quads_added: u32,
    /// The texture starts on a tile boundary on both axes, so cuts fall on
    /// the tile edges.
    pub phase_ok: bool,
    /// The fine rotation is not a quarter turn, so the counts are an
    /// estimate (the cook counts exactly).
    pub approximate: bool,
    /// For a fit whose frame was widened over a patch of faces: the repeats
    /// across the whole patch, horizontal and vertical.
    pub patch: Option<[f32; 2]>,
}

impl UvReport {
    /// The one-line description the Inspector prints.
    pub fn effective_text(&self) -> String {
        let [h, v] = self.tiles;
        let tiles = |t: f32| {
            if (t - t.round()).abs() < 0.005 {
                format!("{}", t.round() as i64)
            } else {
                format!("{t:.2}").trim_end_matches('0').to_string()
            }
        };
        match self.mode {
            UvMode::Fit { .. } if self.patch.is_some() => {
                let [ph, pv] = self.patch.unwrap_or([0.0; 2]);
                format!(
                    "{} x {} on this face, part of {} x {} across its patch",
                    tiles(h),
                    tiles(v),
                    tiles(ph),
                    tiles(pv)
                )
            }
            UvMode::Fit { .. } => format!("{} x {} on this face", tiles(h), tiles(v)),
            _ => format!("{} x {} tiles on this face", tiles(h), tiles(v)),
        }
    }

    /// The warning about the split, if the face is cut.
    pub fn split_warning(&self) -> Option<String> {
        (self.quads_added > 0).then(|| {
            format!(
                "the cook cuts this face into {} primitives (+{}): a primitive holds at most {} tiles per axis",
                self.quads_added + 1,
                self.quads_added,
                MAX_TILES_PER_PRIMITIVE
            )
        })
    }
}

/// Describe the tiling of a face with placement `uv` and a frame of `size`
/// grid units whose lower corner sits at `origin`. `material_default` is the
/// material's grid tiling.
pub fn report(
    uv: &FaceUv,
    material_default: Option<UvMode>,
    origin: [f64; 2],
    size: [f64; 2],
) -> UvReport {
    let mode = resolve(uv.mode, material_default);
    let from_material =
        uv.mode == UvMode::Inherit && matches!(material_default, Some(m) if m != UvMode::Inherit);
    let turned = uv.turns & 1 == 1;
    // Texel extent and start along the texture's U and V.
    let (tiles, start, span) = match mode {
        UvMode::Fit { repeats_q4 } => {
            let (rh, rv) = (
                f64::from(repeats_q4[0]) / 16.0,
                f64::from(repeats_q4[1]) / 16.0,
            );
            // The face's share of the frame it is fitted to (all of it
            // unless the frame was widened over a patch), per screen axis,
            // with the flips applied.
            let whole = Frame {
                min: origin,
                max: [origin[0] + size[0], origin[1] + size[1]],
            };
            let frame = widen(whole, uv.grow);
            let frame_size = frame.size();
            let share = |axis: usize| -> [f64; 2] {
                if frame_size[axis] <= 0.0 {
                    return [0.0, 1.0];
                }
                let lo = (whole.min[axis] - frame.min[axis]) / frame_size[axis];
                let hi = (whole.max[axis] - frame.min[axis]) / frame_size[axis];
                if uv.flip[axis] {
                    [1.0 - hi, 1.0 - lo]
                } else {
                    [lo, hi]
                }
            };
            let (a, b) = (share(0), share(1));
            let reverse = |r: [f64; 2]| [1.0 - r[1], 1.0 - r[0]];
            // Texel ranges (in tiles) along the texture's U and V.
            let (u, v) = match uv.turns & 3 {
                0 => ([a[0] * rh, a[1] * rh], [b[0] * rv, b[1] * rv]),
                1 => ([b[0] * rv, b[1] * rv], {
                    let r = reverse(a);
                    [r[0] * rh, r[1] * rh]
                }),
                2 => (
                    {
                        let r = reverse(a);
                        [r[0] * rh, r[1] * rh]
                    },
                    {
                        let r = reverse(b);
                        [r[0] * rv, r[1] * rv]
                    },
                ),
                _ => (
                    {
                        let r = reverse(b);
                        [r[0] * rv, r[1] * rv]
                    },
                    [a[0] * rh, a[1] * rh],
                ),
            };
            let tiles = [(rh * (a[1] - a[0])) as f32, (rv * (b[1] - b[0])) as f32];
            (
                tiles,
                [u[0] * TILE_TEXELS, v[0] * TILE_TEXELS],
                [(u[1] - u[0]) * TILE_TEXELS, (v[1] - v[0]) * TILE_TEXELS],
            )
        }
        UvMode::World { texels_per_sector } => {
            let k = density(texels_per_sector);
            // Tiles across the screen: a quarter turn swaps which density
            // runs along the screen's horizontal.
            let (kh, kv) = if turned { (k[1], k[0]) } else { (k[0], k[1]) };
            let tiles = [
                (size[0] * kh / TILE_TEXELS) as f32,
                (size[1] * kv / TILE_TEXELS) as f32,
            ];
            let span = if turned {
                [size[1] * k[0], size[0] * k[1]]
            } else {
                [size[0] * k[0], size[1] * k[1]]
            };
            (tiles, [origin[0] * k[0], origin[1] * k[1]], span)
        }
        UvMode::Inherit => ([0.0; 2], [0.0; 2], [0.0; 2]),
    };
    let scale = |axis: usize| {
        (f64::from(uv.scale_q8[axis]) / 256.0)
            .abs()
            .max(1.0 / 256.0)
    };
    let (span, approximate) = (
        [span[0] / scale(0), span[1] / scale(1)],
        uv.rotation_deg.rem_euclid(90) != 0,
    );
    let begin = [
        start[0] / scale(0) + f64::from(uv.offset_texels[0]),
        start[1] / scale(1) + f64::from(uv.offset_texels[1]),
    ];
    // Primitives along one axis: the tiles the span touches (counted from
    // the tile its start lies in), in groups of the per-primitive limit.
    let piece = |s: f64, b: f64| -> u32 {
        if s <= 0.0 {
            return 1;
        }
        let phase = b.rem_euclid(TILE_TEXELS);
        let reach = phase + s;
        // A tiny tolerance so an exact tile does not count as one more.
        let touched = ((reach - 0.5) / TILE_TEXELS).ceil().max(1.0) as u32;
        touched.div_ceil(MAX_TILES_PER_PRIMITIVE)
    };
    let pieces = [piece(span[0], begin[0]), piece(span[1], begin[1])];
    UvReport {
        mode,
        from_material,
        tiles,
        pieces,
        quads_added: pieces[0] * pieces[1] - 1,
        phase_ok: begin.iter().all(|b| {
            b.rem_euclid(TILE_TEXELS) < 1e-6 || (TILE_TEXELS - b.rem_euclid(TILE_TEXELS)) < 1e-6
        }),
        approximate,
        patch: (matches!(mode, UvMode::Fit { .. }) && uv.grow != [[0; 2]; 2]).then(|| {
            let UvMode::Fit { repeats_q4 } = mode else {
                return [0.0; 2];
            };
            repeats_q4.map(|q| f32::from(q) / 16.0)
        }),
    }
}

/// The generated face of the document that `face` (and `tri`) names.
fn find_face(
    world: &super::world::GridWorld,
    defaults: &dyn super::faces::UvDefaults,
    face: super::edit::FaceRef,
    tri: Option<u8>,
) -> Option<super::faces::GridFace> {
    use super::edit::{FaceTarget, SurfaceKind};
    use super::faces::{cell_faces_with, GridFaceKind};
    let room = world.rooms.get(face.room)?;
    let [x, z] = room.local(face.cell())?;
    let mut faces = Vec::new();
    cell_faces_with(world, defaults, face.room, x, z, &mut faces);
    faces.into_iter().find(|f| match (face.target, f.kind) {
        (FaceTarget::Surface(SurfaceKind::Floor), GridFaceKind::Floor { triangle }) => {
            triangle.is_none() || triangle == Some(tri.unwrap_or(0))
        }
        (FaceTarget::Surface(SurfaceKind::Ceiling), GridFaceKind::Ceiling { triangle }) => {
            triangle.is_none() || triangle == Some(tri.unwrap_or(0))
        }
        (FaceTarget::Wall { edge, index }, GridFaceKind::Wall { edge: e, index: i }) => {
            edge == e && index == i
        }
        (FaceTarget::Derived { edge, kind }, GridFaceKind::Derived { edge: e, kind: k }) => {
            edge == e && kind == k
        }
        _ => false,
    })
}

/// The frame a face's texture is measured against before any widening: its
/// own rectangle of grid units. `None` when the face does not exist.
pub fn own_frame_of(
    world: &super::world::GridWorld,
    defaults: &dyn super::faces::UvDefaults,
    face: super::edit::FaceRef,
    tri: Option<u8>,
) -> Option<Frame> {
    find_face(world, defaults, face, tri).map(|f| f.frame)
}

/// The tiling report of one face of the document, as the Inspector shows
/// it. `tri` picks a triangle of a split surface (`None` the first). `None`
/// when the face does not exist.
pub fn report_for(
    world: &super::world::GridWorld,
    defaults: &dyn super::faces::UvDefaults,
    face: super::edit::FaceRef,
    tri: Option<u8>,
) -> Option<UvReport> {
    use super::edit::{material_of, uv_of};
    let found = find_face(world, defaults, face, tri)?;
    let uv = uv_of(world, face, tri)?;
    let default = material_of(world, face, tri).and_then(|id| defaults.default_uv(id));
    Some(report(&uv, default, found.frame.min, found.frame.size()))
}

/// The tiling defaults of a project's materials.
pub struct ProjectUvDefaults<'a>(pub &'a crate::ProjectDocument);

impl super::faces::UvDefaults for ProjectUvDefaults<'_> {
    fn default_uv(&self, material: crate::ResourceId) -> Option<UvMode> {
        match &self.0.resource(material)?.data {
            crate::ResourceData::Material(m) if !m.grid_uv.is_inherit() => Some(m.grid_uv),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            min: [1024.0, 2048.0],
            max: [2048.0, 3072.0],
        }
    }

    fn mapper(uv: FaceUv) -> Mapper {
        Mapper::new(&uv, None, frame(), [48.0 * 1.0 + 0.0, 80.0])
    }

    #[test]
    fn the_default_is_the_legacy_world_mapping_bit_for_bit() {
        let m = mapper(FaceUv::default());
        let texel = 32.0 / 1024.0;
        for p in [[1024.0, 2048.0], [1500.0, 2500.0], [-77.0, 3000.0]] {
            assert_eq!(m.texel(p), [p[0] * texel, p[1] * texel]);
        }
    }

    #[test]
    fn fit_puts_whole_tiles_across_the_frame() {
        for (h, v) in [(1u16, 1u16), (2, 2), (4, 4)] {
            let m = mapper(FaceUv {
                mode: UvMode::fit(h, v),
                ..FaceUv::default()
            });
            assert_eq!(m.texel([1024.0, 2048.0]), [0.0, 0.0]);
            assert_eq!(
                m.texel([2048.0, 3072.0]),
                [f64::from(h) * 64.0, f64::from(v) * 64.0]
            );
            assert_eq!(
                m.texel([1536.0, 2560.0]),
                [f64::from(h) * 32.0, f64::from(v) * 32.0]
            );
        }
    }

    #[test]
    fn fractional_repeats_and_per_axis_repeats() {
        let m = mapper(FaceUv {
            mode: UvMode::Fit {
                repeats_q4: [24, 48],
            },
            ..FaceUv::default()
        });
        assert_eq!(m.texel([2048.0, 3072.0]), [96.0, 192.0]);
    }

    #[test]
    fn turns_rotate_the_texture_clockwise_on_the_face() {
        // 2 tiles across, 1 tile down, turned a quarter: the texture's U
        // now runs down the face and V runs leftwards.
        let uv = |turns| FaceUv {
            mode: UvMode::Fit {
                repeats_q4: [32, 16],
            },
            turns,
            ..FaceUv::default()
        };
        let top_left = [1024.0, 2048.0];
        let top_right = [2048.0, 2048.0];
        let bottom_left = [1024.0, 3072.0];
        let m = mapper(uv(1));
        assert_eq!(m.texel(top_left), [0.0, 128.0]);
        assert_eq!(m.texel(top_right), [0.0, 0.0]);
        assert_eq!(m.texel(bottom_left), [64.0, 128.0]);
        // Four quarter turns are the identity.
        let plain = mapper(uv(0));
        let full = Mapper::new(&uv(4), None, frame(), [0.0; 2]);
        for p in [top_left, top_right, bottom_left] {
            assert_eq!(plain.texel(p), full.texel(p));
        }
        // Two turns reverse both axes.
        let m2 = mapper(uv(2));
        assert_eq!(m2.texel(top_left), [128.0, 64.0]);
    }

    #[test]
    fn flips_mirror_in_screen_terms() {
        let m = mapper(FaceUv {
            mode: UvMode::fit(1, 1),
            flip: [true, false],
            ..FaceUv::default()
        });
        assert_eq!(m.texel([1024.0, 2048.0]), [64.0, 0.0]);
        assert_eq!(m.texel([2048.0, 3072.0]), [0.0, 64.0]);
    }

    #[test]
    fn world_turns_keep_the_frame_centre_fixed() {
        let plain = mapper(FaceUv::default());
        let turned = mapper(FaceUv {
            turns: 1,
            mode: UvMode::LEGACY,
            ..FaceUv::default()
        });
        let c = frame().centre();
        assert_eq!(plain.texel(c), turned.texel(c));
        let moved = turned.texel([c[0] + 512.0, c[1]]);
        let at = plain.texel(c);
        // A quarter turn clockwise sends +s into -V.
        assert_eq!(moved, [at[0], at[1] - 16.0]);
    }

    #[test]
    fn the_fine_controls_apply_after_the_mode() {
        let m = mapper(FaceUv {
            mode: UvMode::fit(1, 1),
            offset_texels: [5, -3],
            ..FaceUv::default()
        });
        assert_eq!(m.texel([1024.0, 2048.0]), [5.0, -3.0]);
        let scaled = mapper(FaceUv {
            mode: UvMode::fit(1, 1),
            scale_q8: [512, 256],
            ..FaceUv::default()
        });
        // Twice as large: half the texels across, about the centre.
        assert_eq!(scaled.texel([1024.0, 2048.0])[0], 16.0);
    }

    #[test]
    fn the_material_default_fills_in_for_inherit_only() {
        let default = Some(UvMode::fit(2, 2));
        assert_eq!(resolve(UvMode::Inherit, default), UvMode::fit(2, 2));
        assert_eq!(resolve(UvMode::Inherit, None), UvMode::LEGACY);
        assert_eq!(resolve(UvMode::LEGACY, default), UvMode::LEGACY);
        let r = report(&FaceUv::default(), default, [0.0; 2], [1024.0; 2]);
        assert!(r.from_material);
        assert_eq!(r.effective_text(), "2 x 2 on this face");
    }

    #[test]
    fn the_report_counts_tiles_and_cuts() {
        let size = [1024.0, 1024.0];
        let fit = |h, v| {
            report(
                &FaceUv {
                    mode: UvMode::fit(h, v),
                    ..FaceUv::default()
                },
                None,
                [0.0; 2],
                size,
            )
        };
        assert_eq!((fit(1, 1).pieces, fit(1, 1).quads_added), ([1, 1], 0));
        assert!(fit(1, 1).split_warning().is_none());
        // A primitive holds four tiles per axis through the texture window.
        assert_eq!((fit(2, 2).pieces, fit(2, 2).quads_added), ([1, 1], 0));
        assert_eq!(fit(4, 4).quads_added, 0);
        assert!(fit(4, 4).split_warning().is_none());
        assert_eq!(fit(5, 1).quads_added, 1);
        assert_eq!(fit(8, 1).quads_added, 1);
        assert_eq!(fit(8, 8).quads_added, 3);
        assert_eq!(fit(16, 1).quads_added, 3);
        assert_eq!(fit(2, 2).effective_text(), "2 x 2 on this face");
        assert!(fit(8, 8)
            .split_warning()
            .unwrap()
            .contains("4 primitives (+3)"));
        // The legacy mapping: half a tile per sector, no cut.
        let legacy = report(&FaceUv::default(), None, [0.0; 2], size);
        assert_eq!(legacy.quads_added, 0);
        assert_eq!(legacy.effective_text(), "0.5 x 0.5 tiles on this face");
        // 128 texels per sector is two tiles per sector.
        let dense = report(
            &FaceUv {
                mode: UvMode::World {
                    texels_per_sector: [128, 128],
                },
                ..FaceUv::default()
            },
            None,
            [0.0; 2],
            size,
        );
        assert_eq!(dense.pieces, [1, 1]);
        // An offset of half a tile makes a one-tile face touch two tiles,
        // which a repeating primitive still holds.
        let shifted = report(
            &FaceUv {
                mode: UvMode::fit(1, 1),
                offset_texels: [32, 0],
                ..FaceUv::default()
            },
            None,
            [0.0; 2],
            size,
        );
        assert_eq!(shifted.pieces, [1, 1]);
        assert!(!shifted.phase_ok);
        // Four tiles from a half tile phase touch five: two primitives.
        let late = report(
            &FaceUv {
                mode: UvMode::fit(4, 1),
                offset_texels: [32, 0],
                ..FaceUv::default()
            },
            None,
            [0.0; 2],
            size,
        );
        assert_eq!(late.pieces, [2, 1]);
    }
}
