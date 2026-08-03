//! Prefabs: the editor's geometry clipboard, written to disk.
//!
//! A prefab is a bag of authored sectors plus the offsets that place them
//! relative to its own top-left corner. That is exactly what the editor's
//! duplicate clipboard already holds, so a prefab is that structure with a
//! name, serde derives, and enough context to survive the trip into a
//! different project.
//!
//! Nothing downstream learns the word. Stamping copies sectors into the
//! destination grid at author time; the cook and the runtime only ever see
//! ordinary geometry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ron::ser::PrettyConfig;
use serde::{Deserialize, Serialize};

use crate::{
    project_file_stem, projects_dir, GridDirection, GridFloorLink, GridSector,
    GridTriangleMaterialOverride, NodeId, ProjectDocument, ProjectIoError, ResourceId,
};

/// One authored cell, offset from the prefab's top-left corner.
///
/// `sector` is `None` for a hole: a selection is a rectangle, and the cells
/// inside it that were never painted must stay empty rather than inherit
/// whatever the destination had there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefabCell {
    /// `[x, z]` from the prefab's top-left corner.
    pub offset: [i32; 2],
    /// Authored sector, or `None` for an empty cell.
    pub sector: Option<GridSector>,
}

/// One stacked floor of a prefab.
///
/// Mirrors [`WorldGrid::floors_above`]: each floor is its own free footprint
/// with its own elevation, rather than a third axis on the cell offset. That
/// keeps per-floor elevation authorable, which a `[x, z, floor]` offset would
/// have thrown away.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefabFloor {
    /// Elevation above the prefab's base floor, in engine units.
    ///
    /// Applied only to floors the stamp has to create. Stamping onto a floor
    /// the destination already has must not move that floor's own geometry.
    #[serde(default)]
    pub relative_elevation: i32,
    /// Cells on this floor.
    pub cells: Vec<PrefabCell>,
}

/// A light the piece carries, so a stamped prefab is lit instead of black.
///
/// Stored as an integer cell offset rather than a world position, so it can be
/// rotated and flipped through exactly the same transform the geometry takes.
/// Height is in sectors to match the node transform's own unit, which keeps a
/// piece proportionally lit when stamped into a project with a different
/// `sector_size`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefabLight {
    /// Cell offset from the prefab's top-left corner.
    pub cell: [i32; 2],
    /// Height above the piece's base floor, in sectors.
    pub height_sectors: f32,
    /// RGB colour.
    pub color: [u8; 3],
    /// Intensity multiplier.
    pub intensity: f32,
    /// Falloff radius in sectors.
    pub radius: f32,
}

/// A reusable piece of world geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefab {
    /// Display name. The filename is derived from it via [`project_file_stem`].
    pub name: String,
    /// Cell size this piece was authored at.
    ///
    /// Footprints are portable between projects; absolute heights are not.
    /// Heights live in world units, so stamping a 1024-unit piece into a
    /// 1792-unit project keeps the wall heights while the floor plan grows
    /// underneath them. Recorded so the stamp can say so rather than quietly
    /// producing a squat room.
    pub sector_size: i32,
    /// Footprint in cells.
    pub width: i32,
    /// Footprint in cells.
    pub height: i32,
    /// True when the piece was captured from a primitive selection and should
    /// merge into the destination cells instead of replacing them.
    #[serde(default)]
    pub merge_primitives: bool,
    /// Material names for every id the cells reference.
    ///
    /// [`ResourceId`] is a per-project counter, so an id carried into another
    /// project would bind to whatever material happens to sit at that number.
    /// Stamping rebinds by name and clears what the destination does not have:
    /// an unassigned face is obviously wrong on screen, a silently rebound one
    /// is not.
    #[serde(default)]
    pub materials: BTreeMap<u64, String>,
    /// The geometry, base floor first.
    pub floors: Vec<PrefabFloor>,
    /// Lights the piece carries. Every generated piece gets one neutral light
    /// so it is visible the moment it is stamped; without it a sealed room
    /// cooks to a black box and reads as broken geometry.
    #[serde(default)]
    pub lights: Vec<PrefabLight>,
}

impl Prefab {
    /// Capture `floors` as a prefab, recording the name of every material they
    /// reference so another project can rebind them.
    ///
    /// Floor links are normalised on the way in: one that points at another
    /// floor of this same piece is rewritten to a self-relative address
    /// (`target_room: None`), and one that points anywhere else is dropped.
    /// A copied `NodeId` would otherwise address a room that does not exist in
    /// the destination project.
    pub fn capture(
        name: impl Into<String>,
        sector_size: i32,
        width: i32,
        height: i32,
        merge_primitives: bool,
        mut floors: Vec<PrefabFloor>,
        source_room: NodeId,
        source_base_floor: usize,
        project: &ProjectDocument,
    ) -> Self {
        let captured = floors.len();
        for sector in floors
            .iter_mut()
            .flat_map(|floor| floor.cells.iter_mut())
            .filter_map(|cell| cell.sector.as_mut())
        {
            for link in [sector.floor_above.as_mut(), sector.floor_below.as_mut()]
                .into_iter()
                .flatten()
            {
                let inside = link.target_room == Some(source_room)
                    && (link.target_floor as usize) >= source_base_floor
                    && ((link.target_floor as usize) - source_base_floor) < captured;
                *link = GridFloorLink {
                    target_room: None,
                    target_floor: if inside {
                        (link.target_floor as usize - source_base_floor) as u16
                    } else {
                        u16::MAX
                    },
                };
            }
            sector
                .floor_above
                .take_if(|link| link.target_floor == u16::MAX);
            sector
                .floor_below
                .take_if(|link| link.target_floor == u16::MAX);
        }

        let mut materials = BTreeMap::new();
        for sector in floors
            .iter()
            .flat_map(|floor| floor.cells.iter())
            .filter_map(|cell| cell.sector.as_ref())
        {
            for id in sector_material_ids(sector) {
                if let Some(resource_name) = project.resource_name(id) {
                    materials.insert(id.raw(), resource_name.to_string());
                }
            }
        }
        Self {
            name: name.into(),
            sector_size,
            width,
            height,
            merge_primitives,
            materials,
            floors,
            lights: Vec::new(),
        }
    }

    /// The prefab's floors with every material reference rebound to `project`
    /// and every self-relative floor link resolved against `room` at
    /// `base_floor`, plus how many material references found no match and were
    /// cleared.
    pub fn bound_floors(
        &self,
        project: &ProjectDocument,
        room: NodeId,
        base_floor: usize,
    ) -> (Vec<PrefabFloor>, usize) {
        let by_name: BTreeMap<String, ResourceId> = project
            .material_options()
            .into_iter()
            .map(|(id, name)| (name, id))
            .collect();
        // Cell, not a plain counter: the remap closure has to stay `Fn` so it
        // can be handed to `Option::and_then` per face.
        let unbound = std::cell::Cell::new(0usize);
        let mut floors = self.floors.clone();
        for sector in floors
            .iter_mut()
            .flat_map(|floor| floor.cells.iter_mut())
            .filter_map(|cell| cell.sector.as_mut())
        {
            remap_sector_materials(sector, &|id| {
                let bound = self.materials.get(&id.raw()).and_then(|name| {
                    // Same id under the same name is the same material: the
                    // common case of stamping back into the project that
                    // authored the piece, where no lookup is needed.
                    if project.resource_name(id) == Some(name.as_str()) {
                        return Some(id);
                    }
                    by_name.get(name).copied()
                });
                if bound.is_none() {
                    unbound.set(unbound.get() + 1);
                }
                bound
            });
            for link in [sector.floor_above.as_mut(), sector.floor_below.as_mut()]
                .into_iter()
                .flatten()
            {
                link.target_room = Some(room);
                link.target_floor = link.target_floor.saturating_add(base_floor as u16);
            }
        }
        (floors, unbound.into_inner())
    }

    /// Every cell across every floor, for callers that only care about the
    /// geometry (material scans, counts).
    pub fn cells(&self) -> impl Iterator<Item = &PrefabCell> {
        self.floors.iter().flat_map(|floor| floor.cells.iter())
    }

    /// Write the prefab as RON, creating parent directories.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ProjectIoError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let config = PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true);
        std::fs::write(path, ron::ser::to_string_pretty(self, config)?)?;
        Ok(())
    }

    /// Read a prefab from a RON file.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ProjectIoError> {
        Ok(ron::from_str(&std::fs::read_to_string(path)?)?)
    }
}

/// Where prefabs live: beside `projects/`.
///
/// Deriving from [`projects_dir`] means prefabs follow the same source-tree
/// vs per-user-data resolution and the same `PSOXIDE_PROJECTS_DIR` override,
/// and being outside any one project is the point: a corridor segment
/// authored in one level is worth stamping into the next.
pub fn prefabs_dir() -> PathBuf {
    projects_dir().with_file_name("prefabs")
}

/// File a prefab named `name` is stored at.
pub fn prefab_path(name: &str) -> PathBuf {
    prefabs_dir().join(format!("{}.ron", project_file_stem(name)))
}

/// Every `.ron` under [`prefabs_dir`], sorted. Missing directory is empty,
/// not an error: the first save creates it.
pub fn list_prefabs() -> std::io::Result<Vec<PathBuf>> {
    let root = prefabs_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "ron") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Every material id the sector references, floors and ceilings (including
/// their per-triangle overrides) and every wall.
pub fn sector_material_ids(sector: &GridSector) -> Vec<ResourceId> {
    let mut out = Vec::new();
    for face in [sector.floor.as_ref(), sector.ceiling.as_ref()]
        .into_iter()
        .flatten()
    {
        out.extend(face.material);
        for index in 0..2 {
            out.extend(
                face.triangle_overrides
                    .get(index)
                    .material
                    .and_then(GridTriangleMaterialOverride::material),
            );
        }
    }
    for direction in GridDirection::ALL {
        out.extend(
            sector
                .walls
                .get(direction)
                .iter()
                .filter_map(|w| w.material),
        );
    }
    out
}

/// Rewrite every material reference through `remap`. `None` clears it.
pub fn remap_sector_materials(
    sector: &mut GridSector,
    remap: &impl Fn(ResourceId) -> Option<ResourceId>,
) {
    for face in [sector.floor.as_mut(), sector.ceiling.as_mut()]
        .into_iter()
        .flatten()
    {
        face.material = face.material.and_then(remap);
        for index in 0..2 {
            let over = face.triangle_overrides.get_mut(index);
            if let Some(existing) = over
                .material
                .and_then(GridTriangleMaterialOverride::material)
            {
                over.material = Some(GridTriangleMaterialOverride::from_material(remap(existing)));
            }
        }
    }
    for direction in GridDirection::ALL {
        for wall in sector.walls.get_mut(direction) {
            wall.material = wall.material.and_then(remap);
        }
    }
}
