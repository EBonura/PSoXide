//! Capture one hand-edited bay and repeat it.
//!
//! [`capture_bay`] reads a rectangle of whole sectors of an existing room (a
//! bay the author sculpted with the ordinary tools) into a [`BayTemplate`]:
//! every leaf inside it with its floor, ceiling, materials and authored walls,
//! heights taken relative to the bay's lowest floor so the bay can stand at
//! any height. [`repeat_bay`] lays copies of it end to end along X or Z, so
//! the bay repeats down a corridor exactly: leaf by leaf the copy equals the
//! source.
//!
//! **What it cannot capture.** Edge overrides (hidden or repainted derived
//! walls, the openings of the Opening verb, per-face UV and band
//! assignments), band sets, free planes and mesh instances (columns, arches),
//! lights, and columns of several storeys. A template is not rotated: it
//! repeats in the orientation it was cut in. The rectangle must be whole
//! sectors and no leaf may cross its edge. Derived walls are not stored
//! because they follow from the heights; the wall material each leaf owns is.

use serde::{Deserialize, Serialize};

use crate::grid::leaf::LeafKey;
use crate::grid::sector::{GridSector, WallEdge};
use crate::grid::surface::GridSplit;
use crate::grid::tree::normalize;
use crate::grid::world::GridWorld;
use crate::{ProjectDocument, ResourceData, ResourceId};

use super::style::Roles;

use super::super::rects::{grow_room_to, CellRect};

/// A bay read from a room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BayTemplate {
    /// Size in sectors `[x, z]`.
    pub size: [i32; 2],
    /// The lowest floor corner of the source, grid units; heights below are
    /// relative to it.
    pub reference: i32,
    /// Leaves with their corner in clicks from the bay's minimum corner.
    pub leaves: Vec<(LeafKey, GridSector)>,
    /// The name of every material the leaves use, as the source project
    /// called it. Empty for a bay whose materials were never named; such a
    /// bay keeps the ids it was cut with, which only mean something in that
    /// project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<(ResourceId, String)>,
}

impl BayTemplate {
    /// Every material id the leaves use, once, in first-use order.
    fn used_materials(&self) -> Vec<ResourceId> {
        let mut seen: Vec<ResourceId> = Vec::new();
        let mut note = |m: Option<ResourceId>| {
            if let Some(m) = m {
                if !seen.contains(&m) {
                    seen.push(m);
                }
            }
        };
        for (_, sector) in &self.leaves {
            for surf in sector.surfaces() {
                note(surf.tri_a.material);
                note(surf.tri_b.as_ref().and_then(|t| t.material));
            }
            note(sector.ground_wall_material());
            for edge in WallEdge::ALL {
                for wall in sector.walls(edge) {
                    note(wall.material);
                }
            }
        }
        seen
    }

    /// Record the name each material has in `project`, so the bay can be laid
    /// in another project by name. Ids the project does not hold stay
    /// unnamed.
    pub fn name_materials(&mut self, project: &ProjectDocument) {
        self.materials = self
            .used_materials()
            .into_iter()
            .filter_map(|id| {
                let found = project
                    .resources
                    .iter()
                    .find(|r| r.id == id && matches!(r.data, ResourceData::Material(_)))?;
                Some((id, found.name.clone()))
            })
            .collect();
    }

    /// The bay with its materials bound to `project`: a material whose name
    /// the project holds becomes that material; any other falls back to the
    /// project's role (floor surfaces to `roles.floor`, ceilings to
    /// `roles.ceiling`, walls and the wall material of a leaf to
    /// `roles.wall`), which may itself be none.
    pub fn bound_to(&self, project: &ProjectDocument, roles: Roles) -> Self {
        let by_name = |id: ResourceId| -> Option<ResourceId> {
            let (_, name) = self.materials.iter().find(|(m, _)| *m == id)?;
            project
                .resources
                .iter()
                .find(|r| &r.name == name && matches!(r.data, ResourceData::Material(_)))
                .map(|r| r.id)
        };
        let bind = |m: Option<ResourceId>, role: Option<ResourceId>| match m {
            Some(id) => by_name(id).or(role),
            None => None,
        };
        let mut out = self.clone();
        for (_, sector) in &mut out.leaves {
            if let Some(f) = sector.ground_floor_mut() {
                f.tri_a.material = bind(f.tri_a.material, roles.floor);
                if let Some(b) = f.tri_b.as_mut() {
                    b.material = bind(b.material, roles.floor);
                }
            }
            if let Some(c) = sector.ground_ceiling_mut() {
                c.tri_a.material = bind(c.tri_a.material, roles.ceiling);
                if let Some(b) = c.tri_b.as_mut() {
                    b.material = bind(b.material, roles.ceiling);
                }
            }
            let wall = bind(sector.ground_wall_material(), roles.wall);
            sector.set_ground_wall_material(wall);
            for edge in WallEdge::ALL {
                for w in sector.walls_mut(edge) {
                    w.material = bind(w.material, roles.wall);
                }
            }
        }
        out.materials.clear();
        out
    }

    /// The bay turned `quarter_turns` quarter turns, so a run that goes
    /// along +Z (as captured) can go along +X (1), -Z (2) or -X (3). A
    /// rotation keeps handedness: the bay is not mirrored. Refused for a bay
    /// with authored walls, void triangles, split-level surfaces, a second
    /// triangle of its own or a mapped texture, which a turn cannot carry
    /// over exactly.
    pub fn turned(&self, quarter_turns: u8) -> Result<Self, CaptureRefusal> {
        let mut out = self.clone();
        for _ in 0..quarter_turns % 4 {
            out = out.turned_once()?;
        }
        Ok(out)
    }

    fn turned_once(&self) -> Result<Self, CaptureRefusal> {
        let width_clicks = self.size[0] * 4;
        let mut leaves = Vec::with_capacity(self.leaves.len());
        for (key, sector) in &self.leaves {
            if sector.wall_count() > 0 {
                return Err(CaptureRefusal::NotTurnable);
            }
            let mut s = sector.clone();
            for surf in s.surfaces_mut() {
                if surf.heights_b.is_some()
                    || surf.tri_b.is_some()
                    || surf.void != 0
                    || !surf.tri_a.uv.is_identity()
                {
                    return Err(CaptureRefusal::NotTurnable);
                }
                let h = surf.heights;
                surf.heights = [h[1], h[2], h[3], h[0]];
                surf.split = match surf.split {
                    GridSplit::NwSe => GridSplit::NeSw,
                    GridSplit::NeSw => GridSplit::NwSe,
                };
            }
            let edge = key.edge_clicks();
            let turned = LeafKey::new(key.level, key.z, width_clicks - key.x - edge)
                .ok_or(CaptureRefusal::NotTurnable)?;
            leaves.push((turned, s));
        }
        Ok(Self {
            size: [self.size[1], self.size[0]],
            reference: self.reference,
            leaves,
            materials: self.materials.clone(),
        })
    }
}

/// Why a capture or a repeat was refused. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureRefusal {
    /// No such room.
    NoSuchRoom,
    /// The rectangle holds no leaf.
    Empty,
    /// A leaf crosses the edge of the rectangle.
    LeafCrossesEdge(LeafKey),
    /// A column of several storeys.
    Storeys,
    /// The room would pass its size cap.
    RoomTooLarge,
    /// The repeat count is zero.
    NoCopies,
    /// The bay cannot be turned to run another way.
    NotTurnable,
}

impl CaptureRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::Empty => "The rectangle holds no cells".to_string(),
            Self::LeafCrossesEdge(_) => {
                "A cell crosses the edge of the rectangle: split it to fit first".to_string()
            }
            Self::Storeys => "Columns of several storeys cannot be captured".to_string(),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
            Self::NoCopies => "Repeat at least once".to_string(),
            Self::NotTurnable => {
                "This bay has authored walls, void cells or mapped textures, so it cannot be turned: run it the way it was cut".to_string()
            }
        }
    }
}

pub(crate) fn shift(sector: &mut GridSector, dy: i32) {
    for s in sector.surfaces_mut() {
        for h in s.heights.iter_mut() {
            *h += dy;
        }
        if let Some(b) = s.heights_b.as_mut() {
            for h in b.iter_mut() {
                *h += dy;
            }
        }
    }
    for edge in WallEdge::ALL {
        for w in sector.walls_mut(edge) {
            for h in w.heights.iter_mut() {
                *h += dy;
            }
        }
    }
}

/// Read the bay `rect` (world sectors) of `room`.
pub fn capture_bay(
    world: &GridWorld,
    room: usize,
    rect: CellRect,
) -> Result<BayTemplate, CaptureRefusal> {
    let r = world.rooms.get(room).ok_or(CaptureRefusal::NoSuchRoom)?;
    let (lo, hi) = (
        [rect.min[0] * 4, rect.min[1] * 4],
        [rect.max[0] * 4 + 3, rect.max[1] * 4 + 3],
    );
    let mut found: Vec<(LeafKey, GridSector)> = Vec::new();
    for (key, sector) in r.leaves() {
        let end = key.end_click();
        let inside = key.x >= lo[0] && key.z >= lo[1] && end[0] - 1 <= hi[0] && end[1] - 1 <= hi[1];
        let touches = key.x <= hi[0] && key.z <= hi[1] && end[0] > lo[0] && end[1] > lo[1];
        if touches && !inside {
            return Err(CaptureRefusal::LeafCrossesEdge(key));
        }
        if inside {
            if !sector.is_single_span() {
                return Err(CaptureRefusal::Storeys);
            }
            found.push((key, sector.clone()));
        }
    }
    if found.is_empty() {
        return Err(CaptureRefusal::Empty);
    }
    let reference = found
        .iter()
        .filter_map(|(_, s)| {
            s.ground_floor()
                .map(|f| f.heights.into_iter().min().unwrap_or(0))
        })
        .min()
        .unwrap_or(0);
    let leaves = found
        .into_iter()
        .map(|(key, mut sector)| {
            shift(&mut sector, -reference);
            (
                LeafKey {
                    level: key.level,
                    x: key.x - lo[0],
                    z: key.z - lo[1],
                },
                sector,
            )
        })
        .collect();
    Ok(BayTemplate {
        size: [rect.width(), rect.depth()],
        reference,
        leaves,
        materials: Vec::new(),
    })
}

/// Which way copies go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Along {
    /// Towards +X.
    X,
    /// Towards +Z.
    Z,
}

/// Lay `count` copies of `template` end to end from the sector `origin`, the
/// source's lowest floor at `base`. Whatever stands in the copies' footprint
/// is replaced. Returns the leaves laid.
pub fn repeat_bay(
    world: &mut GridWorld,
    room: usize,
    template: &BayTemplate,
    origin: [i32; 2],
    along: Along,
    count: i32,
    base: i32,
) -> Result<usize, CaptureRefusal> {
    if count < 1 {
        return Err(CaptureRefusal::NoCopies);
    }
    let mut work = world.clone();
    let r = work.rooms.get_mut(room).ok_or(CaptureRefusal::NoSuchRoom)?;
    let step = match along {
        Along::X => [template.size[0], 0],
        Along::Z => [0, template.size[1]],
    };
    let rect = CellRect {
        min: origin,
        max: [
            origin[0] + step[0] * (count - 1) + template.size[0] - 1,
            origin[1] + step[1] * (count - 1) + template.size[1] - 1,
        ],
    };
    grow_room_to(r, rect).map_err(|_| CaptureRefusal::RoomTooLarge)?;
    // Clear the footprint: every leaf touching it goes.
    let (lo, hi) = (
        [rect.min[0] * 4, rect.min[1] * 4],
        [rect.max[0] * 4 + 3, rect.max[1] * 4 + 3],
    );
    let doomed: Vec<LeafKey> = r
        .leaves()
        .map(|(k, _)| k)
        .filter(|k| {
            let e = k.end_click();
            k.x <= hi[0] && k.z <= hi[1] && e[0] > lo[0] && e[1] > lo[1]
        })
        .collect();
    for k in doomed {
        r.remove_leaf(k);
    }
    let mut laid = 0;
    for n in 0..count {
        for (key, sector) in &template.leaves {
            let k = LeafKey {
                level: key.level,
                x: key.x + (origin[0] + step[0] * n) * 4,
                z: key.z + (origin[1] + step[1] * n) * 4,
            };
            let mut s = sector.clone();
            shift(&mut s, base);
            r.insert_leaf(k, s)
                .map_err(|_| CaptureRefusal::RoomTooLarge)?;
            laid += 1;
        }
    }
    normalize(r);
    *world = work;
    Ok(laid)
}
