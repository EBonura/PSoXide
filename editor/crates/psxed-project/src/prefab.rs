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

/// The starter kit, embedded so it travels with the binary.
///
/// [`prefabs_dir`] resolves to the source tree only when the source tree is
/// there; a shipped build or a `PSOXIDE_PROJECTS_DIR` override lands on an
/// empty directory instead, so a fresh install had a prefab browser with
/// nothing in it. Embedding the kit is what makes "every new project starts
/// with the prefabs" true off the source tree as well as on it.
const PREFAB_KIT: &[(&str, &str)] = &[
    ("arena_5x5", include_str!("../../../prefabs/arena_5x5.ron")),
    (
        "balcony_hall_7x7",
        include_str!("../../../prefabs/balcony_hall_7x7.ron"),
    ),
    (
        "boss_arena_9x9",
        include_str!("../../../prefabs/boss_arena_9x9.ron"),
    ),
    (
        "canyon_3x9",
        include_str!("../../../prefabs/canyon_3x9.ron"),
    ),
    (
        "connector_corner",
        include_str!("../../../prefabs/connector_corner.ron"),
    ),
    (
        "connector_crossroads",
        include_str!("../../../prefabs/connector_crossroads.ron"),
    ),
    (
        "connector_diagonal",
        include_str!("../../../prefabs/connector_diagonal.ron"),
    ),
    (
        "connector_long",
        include_str!("../../../prefabs/connector_long.ron"),
    ),
    (
        "connector_straight",
        include_str!("../../../prefabs/connector_straight.ron"),
    ),
    (
        "connector_t",
        include_str!("../../../prefabs/connector_t.ron"),
    ),
    (
        "crossroads_hall_5x5",
        include_str!("../../../prefabs/crossroads_hall_5x5.ron"),
    ),
    (
        "hall_pillared_7x7",
        include_str!("../../../prefabs/hall_pillared_7x7.ron"),
    ),
    (
        "lantern_chamber",
        include_str!("../../../prefabs/lantern_chamber.ron"),
    ),
    (
        "octagon_chamber_5x5",
        include_str!("../../../prefabs/octagon_chamber_5x5.ron"),
    ),
    (
        "rotunda_9x9",
        include_str!("../../../prefabs/rotunda_9x9.ron"),
    ),
    (
        "spiral_stair_3x3",
        include_str!("../../../prefabs/spiral_stair_3x3.ron"),
    ),
    (
        "stair_double",
        include_str!("../../../prefabs/stair_double.ron"),
    ),
    ("stair_run", include_str!("../../../prefabs/stair_run.ron")),
    (
        "stair_switchback",
        include_str!("../../../prefabs/stair_switchback.ron"),
    ),
    (
        "treasure_alcove",
        include_str!("../../../prefabs/treasure_alcove.ron"),
    ),
];

/// Names of every embedded kit piece.
pub fn prefab_kit_names() -> impl Iterator<Item = &'static str> {
    PREFAB_KIT.iter().map(|(name, _)| *name)
}

/// Embedded body for a kit piece, if it is one.
pub fn prefab_kit_body(name: &str) -> Option<&'static str> {
    PREFAB_KIT
        .iter()
        .find(|(kit_name, _)| *kit_name == name)
        .map(|(_, body)| *body)
}

/// Write any kit piece that is not on disk yet, creating the directory.
///
/// Missing pieces are restored rather than overwriting, so an edited or
/// deleted piece stays the user's decision for as long as the file exists,
/// and a file the user removed comes back on the next listing. That is the
/// trade for having the kit always available; deleting a stock piece for good
/// is not currently expressible.
pub fn ensure_prefab_kit() -> std::io::Result<()> {
    let root = prefabs_dir();
    std::fs::create_dir_all(&root)?;
    for (name, body) in PREFAB_KIT {
        let path = root.join(format!("{name}.ron"));
        if !path.exists() {
            std::fs::write(&path, body)?;
            continue;
        }

        // The original embedded kit shipped completely open. Upgrade only
        // that recognisable legacy shape: a stock piece with no ceilings at
        // all. Copying ceilings cell-by-cell preserves any walls, floors, or
        // lights the user changed locally, and the presence of even one
        // authored ceiling makes the migration leave the file alone.
        let Ok(mut existing) = Prefab::load_from_path(&path) else {
            continue;
        };
        let Ok(current) = ron::from_str::<Prefab>(body) else {
            continue;
        };
        if upgrade_legacy_open_kit_prefab(&mut existing, &current) {
            existing
                .save_to_path(&path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
    }
    Ok(())
}

/// Add the current stock ceilings to an older all-open copy of the same kit
/// piece. Returns whether the existing prefab changed.
fn upgrade_legacy_open_kit_prefab(existing: &mut Prefab, current: &Prefab) -> bool {
    if existing.name != current.name
        || existing.sector_size != current.sector_size
        || existing.width != current.width
        || existing.height != current.height
        || existing.floors.len() != current.floors.len()
    {
        return false;
    }

    let mut populated = 0usize;
    let mut has_ceiling = false;
    for sector in existing.cells().filter_map(|cell| cell.sector.as_ref()) {
        populated += 1;
        has_ceiling |= sector.ceiling.is_some();
    }
    if populated == 0 || has_ceiling {
        return false;
    }

    let mut changed = false;
    for (floor_index, floor) in existing.floors.iter_mut().enumerate() {
        let Some(current_floor) = current.floors.get(floor_index) else {
            continue;
        };
        for cell in &mut floor.cells {
            let Some(sector) = cell.sector.as_mut() else {
                continue;
            };
            let Some(ceiling) = current_floor
                .cells
                .iter()
                .find(|candidate| candidate.offset == cell.offset)
                .and_then(|candidate| candidate.sector.as_ref())
                .and_then(|sector| sector.ceiling.as_ref())
            else {
                continue;
            };
            if let Some(material) = ceiling.material {
                if let Some(name) = current.materials.get(&material.raw()) {
                    existing
                        .materials
                        .entry(material.raw())
                        .or_insert_with(|| name.clone());
                }
            }
            sector.ceiling = Some(ceiling.clone());
            changed = true;
        }
    }
    changed
}

/// Every `.ron` under [`prefabs_dir`], sorted.
///
/// Seeds the embedded starter kit first, so every project sees the stock
/// pieces whether or not it was created from the source tree. A seed failure
/// is not fatal: a read-only prefabs directory should still list whatever is
/// already there.
pub fn list_prefabs() -> std::io::Result<Vec<PathBuf>> {
    let _ = ensure_prefab_kit();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_open_stock_prefab_gains_current_ceilings_without_replacing_geometry() {
        let current: Prefab = ron::from_str(
            prefab_kit_body("arena_5x5").expect("arena is embedded in the stock kit"),
        )
        .expect("embedded arena parses");
        let mut legacy = current.clone();
        for sector in legacy
            .floors
            .iter_mut()
            .flat_map(|floor| floor.cells.iter_mut())
            .filter_map(|cell| cell.sector.as_mut())
        {
            sector.ceiling = None;
        }
        let custom_wall_height = 4096;
        let wall = legacy
            .floors
            .iter_mut()
            .flat_map(|floor| floor.cells.iter_mut())
            .filter_map(|cell| cell.sector.as_mut())
            .flat_map(|sector| sector.walls.north.iter_mut())
            .next()
            .expect("arena has a north wall");
        wall.heights[2] = custom_wall_height;

        assert!(upgrade_legacy_open_kit_prefab(&mut legacy, &current));
        assert_eq!(
            legacy
                .cells()
                .filter_map(|cell| cell.sector.as_ref())
                .filter(|sector| sector.ceiling.is_some())
                .count(),
            current
                .cells()
                .filter_map(|cell| cell.sector.as_ref())
                .filter(|sector| sector.ceiling.is_some())
                .count()
        );
        assert_eq!(
            legacy
                .cells()
                .filter_map(|cell| cell.sector.as_ref())
                .flat_map(|sector| sector.walls.north.iter())
                .next()
                .expect("custom wall survives")
                .heights[2],
            custom_wall_height
        );
        assert!(
            !upgrade_legacy_open_kit_prefab(&mut legacy, &current),
            "the roof migration only runs once"
        );
    }
}
