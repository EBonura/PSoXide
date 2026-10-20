//! The faces of a banded wall: the pieces [`super::band_geom`] cuts, with a
//! material and a texture mapping per band and the neighbour stitching that
//! keeps the shared vertical edges crack free.
//!
//! One routine serves derived and authored walls. The caller describes the
//! wall in `(u, y)` (position along the host edge's walking direction in
//! grid units, room relative height) and how a position maps to the world;
//! this file turns it into faces.

use crate::face_uv::UvMode;
use crate::ResourceId;

use super::band_geom::{self, Piece, Span};
use super::bands::{self, BandSet, BandV};
use super::derive;
use super::faces::{editor_point, Builder, Corner, GridFaceKind};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::uv::{self, Frame, Mapper};

/// A wall to cut into bands.
pub(super) struct BandedWall<'a> {
    pub kind: GridFaceKind,
    /// The wall's own region.
    pub span: Span,
    /// What the anchors measure against (the whole edge for a stack of
    /// authored segments).
    pub reference: Span,
    pub set: &'a BandSet,
    /// The wall's own material, for a band that names none.
    pub material: Option<ResourceId>,
    pub toward: [f64; 3],
    pub double_sided: bool,
    /// Heights the walls across the start and end edges cut those edges at.
    pub neighbours: [Vec<i32>; 2],
    /// Smallest and largest `u_grid` of the frame a fit mapping measures
    /// against: the run for a derived wall, the record for an authored one.
    pub run: [f64; 2],
    /// Position along the edge in the signed grid coordinate texels use.
    pub u_grid: &'a dyn Fn(i32) -> f64,
    /// World grid `[x, z]` of position `u`.
    pub point: &'a dyn Fn(i32) -> [i32; 2],
}

impl Builder<'_> {
    /// Push the faces of `wall`, one set of pieces per band.
    pub(super) fn banded(&mut self, wall: &BandedWall<'_>) {
        let mut pieces = band_geom::bands(&wall.span, &wall.reference, wall.set);
        for (end, u) in [(0, wall.span.u[0]), (1, wall.span.u[1])] {
            band_geom::stitch_edge(&mut pieces, u, &wall.neighbours[end]);
        }
        for piece in &pieces {
            self.band_piece(wall, piece);
        }
    }

    fn band_piece(&mut self, wall: &BandedWall<'_>, piece: &Piece) {
        let band = &wall.set.bands[piece.band];
        let material = band.material.or(wall.material);
        let default = self.default_uv(material);
        let mode = uv::resolve(band.uv.mode, default);
        let (scale, y_offset) = (self.scale, self.y_offset);
        // The band's own lines at a position: its lowest and highest
        // corner there.
        let range_at = |u: i32| {
            let ys = piece.ring.iter().filter(|c| c[0] == u).map(|c| c[1]);
            let (lo, hi) = ys.fold((i32::MAX, i32::MIN), |(lo, hi), y| (lo.min(y), hi.max(y)));
            (lo, hi)
        };
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        // The frame a mapping measures against: a fit counts tiles across
        // the run and between the band's own lines (0 at the upper line to 1
        // at the lower); a world mapping works in the wall's own
        // coordinates (`t` is minus the height).
        let t_world = |y: i32| -f64::from(y_offset + y);
        let (top, bottom) = (
            wall.reference.top[0].max(wall.reference.top[1]),
            wall.reference.bottom[0].min(wall.reference.bottom[1]),
        );
        let frame = match mode {
            UvMode::Fit { .. } => Frame {
                min: [wall.run[0], 0.0],
                max: [wall.run[1], 1.0],
            },
            _ => Frame {
                min: [wall.run[0], t_world(top)],
                max: [wall.run[1], t_world(bottom)],
            },
        };
        let centre = {
            let c = frame.centre();
            [c[0] * texel, c[1] * texel]
        };
        let mapper = Mapper::new(&band.uv, default, frame, centre);
        self.legacy_uv = mapper.is_legacy();
        self.frame = frame;
        let point_of = |u: i32, y: i32| -> [f64; 2] {
            let (lo, hi) = range_at(u);
            match mode {
                UvMode::Fit { .. } => {
                    let height = f64::from(hi - lo);
                    let down = if height > 0.0 {
                        f64::from(hi - y) / height
                    } else {
                        0.0
                    };
                    [(wall.u_grid)(u), down]
                }
                _ => {
                    let t = match band.v {
                        BandV::World => t_world(y),
                        BandV::FromBottom => f64::from(lo - y),
                        BandV::FromTop => f64::from(hi - y),
                    };
                    [(wall.u_grid)(u), t]
                }
            }
        };
        let corner = |c: [i32; 2]| {
            let [px, pz] = (wall.point)(c[0]);
            Corner {
                p: editor_point(scale, y_offset, px, c[1], pz),
                g: [px, y_offset + c[1], pz],
                uv: mapper.texel(point_of(c[0], c[1])),
            }
        };
        let first = self.out.len();
        for ring in band_geom::fan(&piece.ring) {
            let corners: Vec<Corner> = ring.iter().map(|c| corner(*c)).collect();
            self.push(wall.kind, corners, wall.toward, material, wall.double_sided);
        }
        let band = u8::try_from(piece.band).ok();
        for face in &mut self.out[first..] {
            face.band = band;
        }
    }
}

/// A plain (unbanded) wall ring in `(u, y)` with the neighbours' cut heights
/// put into its end edges, fanned into rings of at most four corners. With
/// nothing to insert it is the ring itself.
pub(super) fn stitched_rings(
    ring: Vec<[i32; 2]>,
    ends: [i32; 2],
    neighbours: &[Vec<i32>; 2],
) -> Vec<Vec<[i32; 2]>> {
    if neighbours.iter().all(Vec::is_empty) {
        return vec![ring];
    }
    let mut pieces = [Piece { band: 0, ring }];
    for (end, u) in ends.into_iter().enumerate() {
        band_geom::stitch_edge(&mut pieces, u, &neighbours[end]);
    }
    let [piece] = pieces;
    band_geom::fan(&piece.ring)
}

/// The unit step of a wall's walking direction in cells.
fn along(edge: WallEdge) -> Option<[i32; 2]> {
    match edge {
        WallEdge::North => Some([1, 0]),
        WallEdge::East => Some([0, 1]),
        WallEdge::South => Some([-1, 0]),
        WallEdge::West => Some([0, -1]),
        _ => None,
    }
}

/// Heights at which the walls of `cell`'s `edge` have a vertex on their
/// vertical edge at the `start` (u = 0) or the end of the wall. Derived and
/// authored walls both count, banded or not.
fn edge_heights(room: &GridRoom, cell: [i32; 2], edge: WallEdge, start: bool) -> Vec<i32> {
    let mut spans: Vec<(Span, Option<&BandSet>)> = Vec::new();
    for wall in derive::edge_walls(room, cell, edge) {
        if wall.hidden {
            continue;
        }
        if let Some(span) = Span::from_ring(&wall.ring) {
            let set = bands::resolve(room, wall.band_set).map(|(_, s)| s);
            spans.push((span, set));
        }
    }
    if let Some(sector) = sector_at(room, cell) {
        for wall in sector.walls(edge) {
            if wall.is_degenerate() {
                continue;
            }
            let span = Span::from_heights(wall.heights, SECTOR_UNITS);
            let set = bands::resolve(room, wall.band_set).map(|(_, s)| s);
            spans.push((span, set));
        }
    }
    let mut out = Vec::new();
    for (span, set) in spans {
        let u = if start { span.u[0] } else { span.u[1] };
        match set {
            Some(set) => {
                let pieces = band_geom::bands(&span, &span, set);
                out.extend(band_geom::heights_at(&pieces, u));
            }
            None => {
                let i = usize::from(!start);
                out.extend([span.bottom[i], span.top[i]]);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn sector_at(room: &GridRoom, cell: [i32; 2]) -> Option<&GridSector> {
    let [x, z] = room.local(cell)?;
    room.sector(x, z)
}

/// What the walls across the start and the end edge of the wall on `edge`
/// of `cell` cut those edges at, when the room has bands (no bands, no
/// stitching, so a room made before them cooks as it always did).
pub(super) fn neighbour_heights(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> [Vec<i32>; 2] {
    let Some(step) = along(edge) else {
        return [Vec::new(), Vec::new()];
    };
    if !bands::room_has_bands(room) {
        return [Vec::new(), Vec::new()];
    }
    let before = [cell[0] - step[0], cell[1] - step[1]];
    let after = [cell[0] + step[0], cell[1] + step[1]];
    [
        // Across the start edge is the previous cell's end.
        edge_heights(room, before, edge, false),
        edge_heights(room, after, edge, true),
    ]
}
