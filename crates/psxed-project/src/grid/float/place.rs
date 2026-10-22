//! Placing slabs: the cube assets and the instances that stretch them.

use super::pack::Rect;
use crate::grid::edit::meshes::{add_asset, delete_meshes, place_mesh, MeshRefusal};
use crate::grid::mesh::{GridMesh, ImportProxy, ImportedMesh, MeshAsset, MeshKind, SCALE_RANGE};
use crate::grid::world::GridWorld;
use crate::ResourceId;

/// Name of the one sector cube.
pub const SMALL_NAME: &str = "Floating slab";
/// Name of the eight sector cube, for slabs too long for the small one.
pub const LARGE_NAME: &str = "Floating slab (large)";
/// Edge of the small cube, grid units.
const SMALL_UNIT: i32 = 1024;
/// Edge of the large cube, grid units.
const LARGE_UNIT: i32 = 8192;
/// Material slots of a slab asset: the top, the four sides, the underside.
const SLOT_COUNT: usize = 3;

/// Materials of the three faces of every slab.
#[derive(Clone, Copy, Debug, Default)]
pub struct SlabStyle {
    /// Top, sides and underside, in that order.
    pub materials: [Option<ResourceId>; SLOT_COUNT],
}

/// The unit cube as OBJ text, three materials in slot order.
fn cube_obj() -> String {
    "\
o slab
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
v 0 0 1
v 1 0 1
v 1 1 1
v 0 1 1
vt 0 0
vt 1 0
vt 1 1
vt 0 1
usemtl top
f 8/1 7/2 3/3 4/4
usemtl side
f 5/1 6/2 7/3 8/4
f 2/1 1/2 4/3 3/4
f 1/1 5/2 8/3 4/4
f 6/1 2/2 3/3 7/4
usemtl under
f 1/1 2/2 6/3 5/4
"
    .to_string()
}

fn cube_asset(name: &str, unit: i32) -> MeshAsset {
    let mesh = ImportedMesh::from_obj(
        &cube_obj(),
        f64::from(unit),
        "floating slab",
        ImportProxy::Slab,
    )
    .expect("the built-in cube is valid OBJ");
    MeshAsset::new(name, MeshKind::Imported(mesh))
}

/// One instance to place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Smallest cell along x.
    pub x: i32,
    /// Smallest cell along z.
    pub z: i32,
    /// Cells along x.
    pub width: i32,
    /// Cells along z.
    pub depth: i32,
    /// Underside, grid units.
    pub bottom: i32,
    /// Top, grid units.
    pub top: i32,
    /// Stretches the large cube.
    pub large: bool,
    /// Grid units along a cell.
    pub edge: i32,
}

impl Piece {
    /// Extent along x, y (up) and z, grid units.
    fn size(&self) -> [i32; 3] {
        [
            self.width * self.edge,
            self.top - self.bottom,
            self.depth * self.edge,
        ]
    }
}

/// Whether a cube of edge `unit` can be stretched to `size` on every axis.
fn stretches(unit: i32, size: [i32; 3]) -> bool {
    let (lo, hi) = (i32::from(SCALE_RANGE.0), i32::from(SCALE_RANGE.1));
    size.iter()
        .all(|&s| (s * 256 / unit) >= lo && (s * 256 / unit) <= hi)
}

/// The instances a rectangle of cells `edge` grid units on a side needs: one
/// when a cube stretches to it, else pieces of at most eight sectors cut
/// along the long axes.
///
/// # Errors
///
/// Returns a message when the slab is taller or thinner than a cube allows.
pub fn pieces(rect: Rect, edge: i32) -> Result<Vec<Piece>, String> {
    let whole = Piece {
        x: rect.x,
        z: rect.z,
        width: rect.width,
        depth: rect.depth,
        bottom: rect.bottom,
        top: rect.top,
        large: false,
        edge,
    };
    if stretches(SMALL_UNIT, whole.size()) {
        return Ok(vec![whole]);
    }
    if stretches(LARGE_UNIT, whole.size()) {
        return Ok(vec![Piece {
            large: true,
            ..whole
        }]);
    }
    let thickness = whole.size()[1];
    if !stretches(SMALL_UNIT, [edge, thickness, edge]) {
        return Err(format!(
            "a slab {thickness} grid units thick at cell [{}, {}] is outside what a cube stretches to",
            rect.x, rect.z
        ));
    }
    let most = (LARGE_UNIT / edge).max(1);
    let cut = |length: i32| -> Vec<(i32, i32)> {
        let n = (length + most - 1) / most;
        let (each, extra) = (length / n, length % n);
        let mut at = 0;
        (0..n)
            .map(|i| {
                let len = each + i32::from(i < extra);
                at += len;
                (at - len, len)
            })
            .collect()
    };
    let mut out = Vec::new();
    for (dz, depth) in cut(rect.depth) {
        for (dx, width) in cut(rect.width) {
            out.push(Piece {
                x: rect.x + dx,
                z: rect.z + dz,
                width,
                depth,
                ..whole
            });
        }
    }
    Ok(out)
}

/// Remove the slabs an earlier run placed. Returns how many.
pub fn remove_placed(world: &mut GridWorld) -> usize {
    let ours: Vec<u16> = world
        .mesh_assets
        .iter()
        .enumerate()
        .filter(|(_, a)| a.name == SMALL_NAME || a.name == LARGE_NAME)
        .map(|(i, _)| i as u16)
        .collect();
    let ids: Vec<u16> = world
        .rooms
        .iter()
        .flat_map(|r| &r.meshes)
        .filter(|m| ours.contains(&m.asset))
        .map(|m| m.id)
        .collect();
    delete_meshes(world, &ids)
}

/// Place `piece` in `room`. `origin` is the world position, grid units, of the
/// model's corner (cell `[0, 0]`).
///
/// # Errors
///
/// Returns the mesh edit's refusal.
pub fn place(
    world: &mut GridWorld,
    room: usize,
    piece: Piece,
    origin: [i32; 2],
    style: &SlabStyle,
) -> Result<u16, MeshRefusal> {
    let (name, unit) = if piece.large {
        (LARGE_NAME, LARGE_UNIT)
    } else {
        (SMALL_NAME, SMALL_UNIT)
    };
    let index = match world.mesh_assets.iter().position(|a| a.name == name) {
        Some(i) => i as u16,
        None => add_asset(world, cube_asset(name, unit))?,
    };
    let size = piece.size();
    let x = origin[0] + piece.x * piece.edge + size[0] / 2;
    let z = origin[1] + piece.z * piece.edge + size[2] / 2;
    let mut mesh = GridMesh::new(0, index, [x, piece.bottom, z]);
    mesh.scale = size.map(|s| (s * 256 / unit) as u16);
    for (slot, material) in style.materials.iter().enumerate() {
        mesh.set_material(slot, *material);
    }
    place_mesh(world, room, mesh)
}
