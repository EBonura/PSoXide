//! TRANSITIONAL: collision, activation and camera geometry derived from a grid
//! document as PXBSP brushes. This module and `playtest/grid_world.rs` are the
//! whole bridge between the grid document and the BSP cook; both go when grid
//! worlds get their own level format and native grid collision.
//!
//! The PXBSP stays the source of collision while the grid renderer draws the
//! world (plan decision D3), so the cook derives it from the document and
//! never from anything the author edits: every solid wall segment and every
//! walkable floor or ceiling triangle becomes a thin convex brush that sits
//! behind its visible side, the same shape the bonnie importer authored. A
//! wall with `solid` off and a surface with `walkable` off get no brush, so
//! they are drawn but passed through.
//!
//! A sky lid over the whole world closes the hull for the cook's leak check
//! (bonnie levels leave their roofs open).

use crate::brush::{Brush, BrushContents, BrushFace, FaceUv, Plane, BRUSH_EDIT_EXTENT_LIMIT};
use crate::grid::faces::{all_faces, GridFace, GridFaceKind};
use crate::grid::GridWorld;
use crate::ResourceId;

use super::SLAB_THICKNESS;

/// Thickness of a wall slab, editor units.
const WALL_THICKNESS: i32 = 72;

/// What the derivation produced.
#[derive(Debug, Default)]
pub struct DerivedCollision {
    /// Thin solid brushes, floors and ceilings first.
    pub brushes: Vec<Brush>,
    /// The sky lid, when the world has any brush to cover.
    pub lid: Option<Brush>,
    /// Faces left out because the author turned collision off.
    pub passable: usize,
    /// Faces whose slab did not solve to a valid brush (zero area, past the
    /// brush extent).
    pub rejected: usize,
}

/// Whether the face collides, from the flag on the surface or wall it was
/// made from.
fn collides(world: &GridWorld, face: &GridFace) -> bool {
    let Some(room) = world.rooms.get(face.room) else {
        return false;
    };
    let Some([x, z]) = room.local(face.cell) else {
        return false;
    };
    let Some(sector) = room.sector(x, z) else {
        return false;
    };
    match face.kind {
        GridFaceKind::Floor { .. } => sector.floor.as_ref().is_some_and(|s| s.walkable),
        GridFaceKind::Ceiling { .. } => sector.ceiling.as_ref().is_some_and(|s| s.walkable),
        GridFaceKind::Wall { edge, index } => {
            sector.walls(edge).get(index).is_some_and(|wall| wall.solid)
        }
    }
}

/// The polygon to thicken for a run of faces made from one wall segment: a
/// rectangular wall comes out of the face builder as one quad per sector of
/// height, which is one slab for collision.
fn merged_ring(run: &[&GridFace]) -> Vec<[i32; 3]> {
    if run.len() == 1 {
        return run[0].points.clone();
    }
    let all = run.iter().flat_map(|f| f.points.iter());
    let low = all.clone().map(|p| p[1]).min().unwrap_or(0);
    let high = all.map(|p| p[1]).max().unwrap_or(0);
    // The two vertical edges of the first band give the wall's endpoints.
    let first = &run[0].points;
    let mut ends: Vec<[i32; 2]> = Vec::new();
    for p in first {
        let xz = [p[0], p[2]];
        if !ends.contains(&xz) {
            ends.push(xz);
        }
    }
    let (a, b) = (ends[0], ends[1]);
    vec![
        [a[0], low, a[1]],
        [b[0], low, b[1]],
        [b[0], high, b[1]],
        [a[0], high, a[1]],
    ]
}

/// Thicken a planar convex polygon into a brush that lies behind its visible
/// side. Degenerate input (collinear points, a wall with no height) gives
/// `None`.
fn slab(
    poly: &[[i32; 3]],
    visible: [f64; 3],
    thickness: i32,
    material: Option<ResourceId>,
) -> Option<Brush> {
    let mut front: Vec<[i32; 3]> = Vec::with_capacity(poly.len());
    for p in poly {
        if front.last() != Some(p) && front.first() != Some(p) {
            front.push(*p);
        }
    }
    if front.len() < 3 {
        return None;
    }
    let offset = visible.map(|v| -(v * f64::from(thickness)).round() as i32);
    let back: Vec<[i32; 3]> = front
        .iter()
        .map(|p| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
        .collect();
    let count = (front.len() * 2) as i64;
    let mut sum = [0i64; 3];
    for p in front.iter().chain(back.iter()) {
        for axis in 0..3 {
            sum[axis] += i64::from(p[axis]);
        }
    }
    let inside = sum.map(|v| (v / count) as i32);

    let mut faces = Vec::new();
    let mut add = |mut points: [[i32; 3]; 3]| -> bool {
        let Some(plane) = Plane::from_points(points) else {
            return false;
        };
        if plane.side(inside) > 0 {
            points.swap(1, 2);
        }
        faces.push(BrushFace {
            points,
            material,
            uv: FaceUv::default(),
            edge_repair: None,
        });
        true
    };
    if !add([front[0], front[1], front[2]]) || !add([back[0], back[1], back[2]]) {
        return None;
    }
    for i in 0..front.len() {
        let j = (i + 1) % front.len();
        add([front[i], front[j], back[j]]);
    }
    let brush = Brush {
        faces,
        contents: BrushContents::Solid,
        mover: None,
        group: None,
    };
    let solved = brush.solve();
    (solved.is_valid() && solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT)).then_some(brush)
}

/// A sky-material slab over every brush at their highest point.
fn sky_lid(brushes: &[Brush], material: Option<ResourceId>) -> Option<Brush> {
    if brushes.is_empty() {
        return None;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for brush in brushes {
        let solved = brush.solve();
        for axis in 0..3 {
            min[axis] = min[axis].min(solved.min[axis]);
            max[axis] = max[axis].max(solved.max[axis]);
        }
    }
    let top = max[1].ceil() as i32;
    let mut lid = Brush::cuboid(
        [min[0].floor() as i32, top, min[2].floor() as i32],
        [
            max[0].ceil() as i32,
            top + SLAB_THICKNESS as i32,
            max[2].ceil() as i32,
        ],
    );
    for face in &mut lid.faces {
        face.material = material;
    }
    let solved = lid.solve();
    (solved.is_valid() && solved.within_extent(BRUSH_EDIT_EXTENT_LIMIT)).then_some(lid)
}

/// Tiny solid cubes far below the world that carry the materials no slab
/// shows (faces with collision off, hidden slab sides). The grid pack binds
/// its textures through the PXBSP material table, which lists only the
/// materials some brush face shows.
fn material_anchors(world: &GridWorld, brushes: &[Brush]) -> Vec<Brush> {
    let used: Vec<Option<ResourceId>> = brushes
        .iter()
        .flat_map(|b| b.faces.iter().map(|f| f.material))
        .collect();
    let missing: Vec<ResourceId> = world
        .material_ids()
        .into_iter()
        .filter(|id| !used.contains(&Some(*id)))
        .collect();
    let mut low = i32::MAX;
    let (mut x0, mut z0) = (i32::MAX, i32::MAX);
    for brush in brushes {
        let solved = brush.solve();
        low = low.min(solved.min[1].floor() as i32);
        x0 = x0.min(solved.min[0].floor() as i32);
        z0 = z0.min(solved.min[2].floor() as i32);
    }
    if missing.is_empty() || low == i32::MAX {
        return Vec::new();
    }
    let top = low - 1024;
    missing
        .into_iter()
        .enumerate()
        .map(|(i, id)| {
            let x = x0 + 64 * i as i32;
            let mut cube = Brush::cuboid([x, top - 32, z0], [x + 32, top, z0 + 32]);
            for face in &mut cube.faces {
                face.material = Some(id);
            }
            cube
        })
        .collect()
}

/// Derive the collision brushes of `world`. `sky` is the material of the lid
/// (a sky aperture material when the project has one).
pub fn derive(world: &GridWorld, sky: Option<ResourceId>) -> DerivedCollision {
    let faces = all_faces(world);
    let mut out = DerivedCollision::default();
    let mut floors = Vec::new();
    let mut walls = Vec::new();
    let mut index = 0;
    while index < faces.len() {
        let face = &faces[index];
        // A run of bands of one wall segment shares room, cell and kind.
        let mut end = index + 1;
        if matches!(face.kind, GridFaceKind::Wall { .. }) {
            while end < faces.len()
                && faces[end].room == face.room
                && faces[end].cell == face.cell
                && faces[end].kind == face.kind
            {
                end += 1;
            }
        }
        let run: Vec<&GridFace> = faces[index..end].iter().collect();
        index = end;
        if !collides(world, face) {
            out.passable += run.len();
            continue;
        }
        let ring = merged_ring(&run);
        let (thickness, list) = if matches!(face.kind, GridFaceKind::Wall { .. }) {
            (WALL_THICKNESS, &mut walls)
        } else {
            (SLAB_THICKNESS as i32, &mut floors)
        };
        match slab(&ring, face.normal, thickness, face.material) {
            Some(brush) => list.push(brush),
            None => out.rejected += run.len(),
        }
    }
    out.brushes = floors;
    out.brushes.append(&mut walls);
    let anchors = material_anchors(world, &out.brushes);
    out.brushes.extend(anchors);
    out.lid = sky_lid(&out.brushes, sky);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::GridScale;

    fn world(configure: impl FnOnce(&mut GridRoom)) -> GridWorld {
        let mut room = GridRoom::new("r", 2, 2);
        configure(&mut room);
        GridWorld {
            scale: GridScale { num: 21, den: 16 },
            rooms: vec![room],
        }
    }

    #[test]
    fn a_floor_and_a_tall_wall_become_one_brush_each() {
        let w = world(|r| {
            let sector = r.sector_or_insert(0, 0).unwrap();
            sector.floor = Some(GridSurface::flat(0, None));
            // Three sectors tall: three bands as faces, one slab here.
            sector.north.push(GridWall::span(0, 3072, None));
        });
        let derived = derive(&w, None);
        assert_eq!(derived.brushes.len(), 2);
        assert_eq!(derived.rejected, 0);
        assert!(derived.lid.is_some());
        let wall = derived.brushes[1].solve();
        // 3 sectors at 1344 / 1024 per grid unit = 4032 high.
        assert_eq!(wall.max[1] - wall.min[1], 4032.0);
    }

    #[test]
    fn a_floor_slab_sits_below_its_surface() {
        let w = world(|r| {
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        let solid = derive(&w, None).brushes[0].solve();
        assert_eq!(solid.max[1], 0.0);
        assert_eq!(solid.min[1], -144.0);
    }

    #[test]
    fn collision_flags_leave_passable_surfaces_out() {
        let w = world(|r| {
            let sector = r.sector_or_insert(0, 0).unwrap();
            let mut floor = GridSurface::flat(0, None);
            floor.walkable = false;
            sector.floor = Some(floor);
            let mut wall = GridWall::span(0, 1024, None);
            wall.solid = false;
            sector.east.push(wall);
            sector.west.push(GridWall::span(0, 1024, None));
        });
        let derived = derive(&w, None);
        assert_eq!(derived.brushes.len(), 1, "only the solid west wall");
        assert_eq!(derived.passable, 2);
    }

    #[test]
    fn a_split_floor_gets_one_slab_per_triangle() {
        let w = world(|r| {
            let mut floor = GridSurface::flat(0, None);
            floor.heights = [0, 0, 0, 256];
            r.sector_or_insert(0, 0).unwrap().floor = Some(floor);
        });
        assert_eq!(derive(&w, None).brushes.len(), 2);
    }

    #[test]
    fn a_material_no_slab_shows_gets_an_anchor_below_the_world() {
        let mut project = crate::ProjectDocument::new("t");
        let mut add = |name: &str| {
            project.add_resource(
                name,
                crate::ResourceData::Material(crate::MaterialResource::opaque(None)),
            )
        };
        let (shown, hidden) = (add("shown"), add("hidden"));
        let w = world(|r| {
            let sector = r.sector_or_insert(0, 0).unwrap();
            sector.floor = Some(GridSurface::flat(0, Some(shown)));
            let mut ghost = GridWall::span(0, 1024, Some(hidden));
            ghost.solid = false;
            sector.east.push(ghost);
        });
        let derived = derive(&w, None);
        assert_eq!(derived.brushes.len(), 2, "the floor slab and one anchor");
        let anchor = derived.brushes[1].solve();
        assert!(anchor.max[1] < -144.0, "below the floor slab");
        assert!(derived.brushes[1]
            .faces
            .iter()
            .all(|f| f.material == Some(hidden)));
    }

    #[test]
    fn an_empty_world_has_no_lid() {
        let derived = derive(&GridWorld::new(), None);
        assert!(derived.brushes.is_empty() && derived.lid.is_none());
    }
}
