use std::collections::BTreeSet;
use std::mem::size_of;
use std::path::Path;

use psx_bsp::pxbsp::{PxbspIndex, PxbspLumpKind};
use psx_bsp::SliceReader;

use super::{
    playtest_performance_envelope, streamed_room_chunk_memory_report, PlaytestAssetKind,
    PlaytestPackage, PlaytestValidationTarget, PlaytestWorldGeometry,
};
use crate::brush_world::BrushWorldCookMode;
use crate::{NodeKind, ProjectDocument, ResourceData};

/// Fixed renderer visibility scratch. One decompressed PVS row must fit.
pub const PLAYTEST_PVS_ROW_LIMIT_BYTES: usize = 1024;
/// Primitive packet arena instantiated by the normal editor-playtest runtime.
pub const PLAYTEST_PACKET_LIMIT: usize = 1536;
/// Residency table capacities instantiated by the normal editor-playtest runtime.
pub const PLAYTEST_RAM_ASSET_SLOT_LIMIT: usize = 128;
pub const PLAYTEST_VRAM_ASSET_SLOT_LIMIT: usize = 64;
/// Physical console ceilings. These byte rows are envelopes, not linker or
/// allocator proofs; the MIPS link and runtime allocator remain authoritative.
pub const PLAYTEST_RAM_PHYSICAL_BYTES: usize = 2 * 1024 * 1024;
pub const PLAYTEST_VRAM_PHYSICAL_BYTES: usize = 1024 * 512 * 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestBudgetStage {
    AuthoredEstimate,
    Cooked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestBudgetKind {
    Bsp,
    Pvs,
    Lighting,
    Textures,
    Ram,
    Vram,
    Packets,
}

impl PlaytestBudgetKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bsp => "BSP",
            Self::Pvs => "PVS",
            Self::Lighting => "Lighting",
            Self::Textures => "Textures",
            Self::Ram => "RAM",
            Self::Vram => "VRAM",
            Self::Packets => "Packets",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestBudgetIssue {
    pub kind: PlaytestBudgetKind,
    pub used: usize,
    pub limit: usize,
    pub unit: &'static str,
    pub target: Option<PlaytestValidationTarget>,
    pub action: &'static str,
}

impl PlaytestBudgetIssue {
    pub fn message(&self) -> String {
        format!(
            "{} envelope {}{} exceeds {}{}; {}",
            self.kind.label(),
            self.used,
            self.unit,
            self.limit,
            self.unit,
            self.action,
        )
    }
}

/// Host-side PSX resource envelope shown before Play and after the exact cook.
/// Byte totals are deliberately kept alongside table/packet limits: payload
/// bytes reveal growth, while the slot/link/runtime checks remain the actual
/// hard gates for the complete executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestBudgetReport {
    pub stage: PlaytestBudgetStage,
    pub mode: BrushWorldCookMode,
    pub bsp_bytes: usize,
    pub pvs_bytes: usize,
    pub pvs_row_bytes: usize,
    pub light_bytes: usize,
    pub texture_bytes: usize,
    pub ram_bytes: usize,
    pub ram_asset_slots: usize,
    pub vram_bytes: usize,
    pub vram_asset_slots: usize,
    pub packet_count: usize,
    pub issues: Vec<PlaytestBudgetIssue>,
}

impl PlaytestBudgetReport {
    pub fn first_actionable_issue(&self) -> Option<&PlaytestBudgetIssue> {
        self.issues.iter().find(|issue| issue.target.is_some())
    }

    pub fn concise_summary(&self) -> String {
        format!(
            "{}: BSP {}B, PVS {}B (row {}/{}B), light {}B, textures {}B, RAM {}/{}B ({} slots), VRAM {}/{}B ({} slots), packets {}/{}",
            self.mode.label(),
            self.bsp_bytes,
            self.pvs_bytes,
            self.pvs_row_bytes,
            PLAYTEST_PVS_ROW_LIMIT_BYTES,
            self.light_bytes,
            self.texture_bytes,
            self.ram_bytes,
            PLAYTEST_RAM_PHYSICAL_BYTES,
            self.ram_asset_slots,
            self.vram_bytes,
            PLAYTEST_VRAM_PHYSICAL_BYTES,
            self.vram_asset_slots,
            self.packet_count,
            PLAYTEST_PACKET_LIMIT,
        )
    }
}

/// Cheap, deterministic authoring estimate used in the Play menu before a
/// compile. It does not claim to predict CSG splitting; the cooked report
/// replaces it after Play with exact emitted-lump and asset figures.
pub fn estimate_playtest_budgets(
    project: &ProjectDocument,
    project_root: &Path,
) -> PlaytestBudgetReport {
    let scene = project.active_scene();
    let solved = scene
        .brushes
        .iter()
        .map(|brush| brush.solve())
        .collect::<Vec<_>>();
    let face_count = solved
        .iter()
        .map(|brush| brush.polygons.iter().flatten().count())
        .sum::<usize>();
    let vertex_count = solved
        .iter()
        .flat_map(|brush| brush.polygons.iter().flatten())
        .map(|polygon| polygon.verts.len())
        .sum::<usize>();
    let packet_count = solved
        .iter()
        .flat_map(|brush| brush.polygons.iter().flatten())
        .map(|polygon| polygon.verts.len().saturating_sub(2))
        .sum::<usize>();
    let leaf_count = face_count.saturating_add(1);
    let pvs_row_bytes = leaf_count.saturating_sub(1).div_ceil(8);
    let pvs_bytes = leaf_count.saturating_mul(pvs_row_bytes);
    let directory_bytes = 8usize.saturating_add(16 * 12);
    let bsp_bytes = directory_bytes
        .saturating_add(vertex_count.saturating_mul(12))
        .saturating_add(face_count.saturating_mul(14 + 14 + 2 + 34 + 6))
        .saturating_add(leaf_count.saturating_mul(26))
        .saturating_add(pvs_bytes)
        .saturating_add(32);
    let light_count = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::PointLight { .. }))
        .count();
    let light_bytes = light_count
        .saturating_mul(size_of::<super::PlaytestLight>())
        .saturating_add(match project.bsp_cook_mode {
            BrushWorldCookMode::Draft => 0,
            BrushWorldCookMode::Release => vertex_count.saturating_mul(3),
        });
    let texture_paths = authored_texture_paths(project);
    let texture_bytes = texture_paths
        .iter()
        .filter_map(|path| {
            let path = Path::new(path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                project_root.join(path)
            };
            std::fs::metadata(resolved)
                .ok()
                .map(|metadata| metadata.len() as usize)
        })
        .sum();
    let ram_asset_slots = usize::from(!scene.brushes.is_empty());
    let vram_asset_slots = texture_paths.len();
    let mut report = PlaytestBudgetReport {
        stage: PlaytestBudgetStage::AuthoredEstimate,
        mode: project.bsp_cook_mode,
        bsp_bytes,
        pvs_bytes,
        pvs_row_bytes,
        light_bytes,
        texture_bytes,
        ram_bytes: bsp_bytes,
        ram_asset_slots,
        vram_bytes: texture_bytes,
        vram_asset_slots,
        packet_count,
        issues: Vec::new(),
    };
    attach_budget_issues(project, &mut report);
    report
}

/// Exact post-cook envelope derived from emitted PXBSP lumps, reachable assets,
/// streamed-room residency, and the conservative renderer packet planner.
pub fn cooked_playtest_budgets(
    project: &ProjectDocument,
    package: &PlaytestPackage,
) -> PlaytestBudgetReport {
    let mut bsp_bytes = 0usize;
    let mut pvs_bytes = package.visibility_pvs_bits.len()
        + package.visibility_pvs.len() * size_of::<super::PlaytestVisibilityPvs>();
    let mut pvs_row_bytes = package
        .visibility_pvs
        .iter()
        .map(|pvs| usize::from(pvs.byte_count))
        .max()
        .unwrap_or(0);
    let mut light_bytes = package.lights.len() * size_of::<super::PlaytestLight>();
    let mut bsp_packets = 0usize;

    if let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry {
        bsp_bytes = world.bytes.len();
        if let Ok(index) = PxbspIndex::read(&mut SliceReader::new(&world.bytes)) {
            let visibility = index.lump(PxbspLumpKind::Visibility);
            let vertices = index.lump(PxbspLumpKind::Vertices);
            let leaves = index.lump(PxbspLumpKind::Leaves);
            let faces = index.lump(PxbspLumpKind::Faces);
            pvs_bytes = visibility.len as usize;
            pvs_row_bytes = (leaves.len as usize / 26).saturating_sub(1).div_ceil(8);
            light_bytes = light_bytes.saturating_add(vertices.len as usize / 12 * 3);
            let face_start = faces.offset as usize;
            let face_end = faces.end() as usize;
            if let Some(face_bytes) = world.bytes.get(face_start..face_end) {
                bsp_packets = face_bytes
                    .chunks_exact(14)
                    .map(|face| {
                        usize::from(u16::from_le_bytes([face[8], face[9]])).saturating_sub(2)
                    })
                    .sum();
            }
        }
    }

    let texture_assets = package
        .assets
        .iter()
        .filter(|asset| asset.kind == PlaytestAssetKind::Texture)
        .collect::<Vec<_>>();
    let texture_bytes = texture_assets.iter().map(|asset| asset.bytes.len()).sum();
    let vram_bytes = texture_assets
        .iter()
        .map(|asset| {
            psx_asset::Texture::from_bytes(&asset.bytes)
                .map(|texture| texture.pixel_bytes().len() + texture.clut_bytes().len())
                .unwrap_or(asset.bytes.len())
        })
        .sum();
    let non_texture_bytes = package
        .assets
        .iter()
        .filter(|asset| asset.kind != PlaytestAssetKind::Texture)
        .map(|asset| asset.bytes.len())
        .sum::<usize>();
    let stream_bytes = streamed_room_chunk_memory_report(package)
        .map(|stream| stream.totals.stream_bytes)
        .unwrap_or(0);
    let ram_bytes = bsp_bytes
        .saturating_add(non_texture_bytes)
        .saturating_add(stream_bytes);
    let ram_asset_slots = package
        .assets
        .iter()
        .filter(|asset| asset.kind != PlaytestAssetKind::Texture)
        .count()
        .saturating_add(usize::from(bsp_bytes != 0));
    let vram_asset_slots = texture_assets.len();
    let envelope = playtest_performance_envelope(package).unwrap_or_default();
    let packet_count = if bsp_packets == 0 {
        envelope
            .tr_packets_before_hw_split
            .saturating_add(envelope.prop_surfaces)
    } else {
        bsp_packets.saturating_add(envelope.prop_surfaces)
    };
    let mut report = PlaytestBudgetReport {
        stage: PlaytestBudgetStage::Cooked,
        mode: package.bsp_cook_mode,
        bsp_bytes,
        pvs_bytes,
        pvs_row_bytes,
        light_bytes,
        texture_bytes,
        ram_bytes,
        ram_asset_slots,
        vram_bytes,
        vram_asset_slots,
        packet_count,
        issues: Vec::new(),
    };
    attach_budget_issues(project, &mut report);
    report
}

fn authored_texture_paths(project: &ProjectDocument) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for resource in &project.resources {
        match &resource.data {
            ResourceData::Texture { psxt_path } => {
                if !psxt_path.trim().is_empty() {
                    paths.insert(psxt_path.clone());
                }
            }
            ResourceData::Material(material) => {
                for path in material.version_texture_paths() {
                    if !path.trim().is_empty() {
                        paths.insert(path.to_string());
                    }
                }
            }
            ResourceData::Model(model) => {
                if let Some(path) = model
                    .texture_path
                    .as_ref()
                    .filter(|path| !path.trim().is_empty())
                {
                    paths.insert(path.clone());
                }
            }
            _ => {}
        }
    }
    paths
}

fn attach_budget_issues(project: &ProjectDocument, report: &mut PlaytestBudgetReport) {
    let geometry_target = project
        .active_scene()
        .brushes
        .iter()
        .enumerate()
        .max_by_key(|(_, brush)| brush.faces.len())
        .map(|(brush, _)| PlaytestValidationTarget::Brush { brush, face: None })
        .or_else(|| {
            project
                .active_scene()
                .nodes()
                .iter()
                .find(|node| matches!(node.kind, NodeKind::Section { .. }))
                .map(|node| PlaytestValidationTarget::Node(node.id))
        });
    let texture_target = project
        .resources
        .iter()
        .find(|resource| {
            matches!(
                resource.data,
                ResourceData::Material(_) | ResourceData::Texture { .. }
            )
        })
        .map(|resource| PlaytestValidationTarget::Resource(resource.id));
    let ram_target = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Model(_)))
        .map(|resource| PlaytestValidationTarget::Resource(resource.id))
        .or(geometry_target);

    if report.pvs_row_bytes > PLAYTEST_PVS_ROW_LIMIT_BYTES {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Pvs,
            used: report.pvs_row_bytes,
            limit: PLAYTEST_PVS_ROW_LIMIT_BYTES,
            unit: "B/row",
            target: geometry_target,
            action: "split visibility with portals or simplify the focused world geometry",
        });
    }
    if report.ram_asset_slots > PLAYTEST_RAM_ASSET_SLOT_LIMIT {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Ram,
            used: report.ram_asset_slots,
            limit: PLAYTEST_RAM_ASSET_SLOT_LIMIT,
            unit: " slots",
            target: ram_target,
            action: "remove or stream assets beginning with the focused model/world source",
        });
    }
    if report.ram_bytes > PLAYTEST_RAM_PHYSICAL_BYTES {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Ram,
            used: report.ram_bytes,
            limit: PLAYTEST_RAM_PHYSICAL_BYTES,
            unit: "B",
            target: ram_target,
            action: "reduce resident payloads; the MIPS linker is the final executable RAM gate",
        });
    }
    if report.vram_asset_slots > PLAYTEST_VRAM_ASSET_SLOT_LIMIT {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Vram,
            used: report.vram_asset_slots,
            limit: PLAYTEST_VRAM_ASSET_SLOT_LIMIT,
            unit: " slots",
            target: texture_target,
            action: "atlas or remove reachable textures beginning with the focused material",
        });
    }
    if report.vram_bytes > PLAYTEST_VRAM_PHYSICAL_BYTES {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Vram,
            used: report.vram_bytes,
            limit: PLAYTEST_VRAM_PHYSICAL_BYTES,
            unit: "B",
            target: texture_target,
            action: "reduce texture depth/size or atlas the focused material's texture",
        });
    }
    if report.packet_count > PLAYTEST_PACKET_LIMIT {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Packets,
            used: report.packet_count,
            limit: PLAYTEST_PACKET_LIMIT,
            unit: " packets",
            target: geometry_target,
            action: "split visibility or simplify the focused brush/room",
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;

    #[test]
    fn estimated_packet_overflow_points_at_largest_brush() {
        let mut project = ProjectDocument::new("budget focus");
        for index in 0..300 {
            project
                .active_scene_mut()
                .brushes
                .push(Brush::cuboid([index * 4, 0, 0], [index * 4 + 2, 2, 2]));
        }
        let report = estimate_playtest_budgets(&project, Path::new("."));
        let issue = report
            .issues
            .iter()
            .find(|issue| issue.kind == PlaytestBudgetKind::Packets)
            .expect("packet issue");
        assert_eq!(
            issue.target,
            Some(PlaytestValidationTarget::Brush {
                brush: 299,
                face: None,
            })
        );
        assert!(issue.message().contains("simplify the focused brush/room"));
    }
}
