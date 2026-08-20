use std::collections::BTreeSet;
use std::mem::size_of;
use std::path::Path;

#[cfg(test)]
use psx_bsp::pxbsp::PxbspVersion;
use psx_bsp::pxbsp::{material_flags, PxbspIndex, PxbspLumpKind};
use psx_bsp::sky::VIEW_RAY_SKY_PACKET_WORDS;
#[cfg(test)]
use psx_bsp::RecordSlice;
use psx_bsp::{BrushModel, ClipNode, CookedRecord, Face, Leaf, Node, Plane, SliceReader, Vertex};
use psx_engine::PRIMITIVE_PACKET_SLOT_WORDS;

use super::{
    playtest_performance_envelope, streamed_room_chunk_memory_report, PlaytestAssetKind,
    PlaytestPackage, PlaytestValidationTarget, PlaytestWorldGeometry, StreamedClass,
};
use crate::brush_world::BrushWorldCookMode;
use crate::{NodeKind, ProjectDocument, ResourceData};

/// Fixed renderer visibility scratch. One decompressed PVS row must fit.
pub const PLAYTEST_PVS_ROW_LIMIT_BYTES: usize = 1024;
/// Baseline primitive packet arena of the normal editor-playtest runtime.
/// The cooked manifest derives the actual per-project capacity from the
/// conservative packet envelope, between this floor and
/// [`PLAYTEST_PACKET_CAPACITY_CEILING`].
pub const PLAYTEST_PACKET_LIMIT: usize = 1536;
/// Largest packet arena the runtime may be asked to instantiate. Content
/// whose envelope exceeds this must be restructured, not silently degraded.
pub const PLAYTEST_PACKET_CAPACITY_CEILING: usize = 4096;
/// Resident PXBSP shares RAM with the complete topology and collision image.
/// Its renderer writes tagged packets directly and therefore cannot afford the
/// grid renderer's 4K packet scratch on a 2 MB machine. The worst-view cook
/// estimate remains visible as a warning, while the runtime keeps a bounded
/// arena and degrades by dropping over-cap geometry instead of failing to link.
pub const PLAYTEST_PXBSP_PACKET_CAPACITY_CEILING: usize = PLAYTEST_PACKET_LIMIT;

/// Primitive-arena slots reserved when any Quake layered-sky aperture is
/// visible. The retained renderer emits one tightly packed, constant-cost
/// view-ray background instead of drawing the source sky polygons.
const VIEW_RAY_SKY_PACKET_SLOTS: usize =
    VIEW_RAY_SKY_PACKET_WORDS.div_ceil(PRIMITIVE_PACKET_SLOT_WORDS);

/// Per-project primitive arena capacity for a cooked packet envelope:
/// the envelope rounded up to a 64-packet step, floored at the baseline
/// arena and capped at the runtime ceiling.
pub const fn derived_packet_capacity(envelope_packets: usize) -> usize {
    let rounded = envelope_packets.div_ceil(64) * 64;
    if rounded < PLAYTEST_PACKET_LIMIT {
        PLAYTEST_PACKET_LIMIT
    } else if rounded > PLAYTEST_PACKET_CAPACITY_CEILING {
        PLAYTEST_PACKET_CAPACITY_CEILING
    } else {
        rounded
    }
}
/// Residency table capacities instantiated by the normal editor-playtest runtime.
pub const PLAYTEST_RAM_ASSET_SLOT_LIMIT: usize = 128;
pub const PLAYTEST_VRAM_ASSET_SLOT_LIMIT: usize = 64;
/// Physical console ceilings. These byte rows are envelopes, not linker or
/// allocator proofs; the MIPS link and runtime allocator remain authoritative.
pub const PLAYTEST_RAM_PHYSICAL_BYTES: usize = 2 * 1024 * 1024;
pub const PLAYTEST_VRAM_PHYSICAL_BYTES: usize = 1024 * 512 * 2;

/// The runtime holds session-resident payloads in whole 2 KiB disc pages, and
/// the cooked manifest turns their total into `PERSISTENT_ASSET_PAGE_COUNT`.
/// Budgeting in the same unit keeps the audit's figure identical to the
/// constant the guest is actually compiled with.
pub const PLAYTEST_RESIDENT_ASSET_PAGE_BYTES: usize = 2048;

/// Ceiling, in 2 KiB pages, on assets that stay resident for the whole
/// gameplay session: every model mesh, atlas and animation clip cooked as
/// `StreamedClass::PersistentGameplay`.
///
/// A canary, not the real gate. The real gate is the MIPS link, because the
/// arena is guest `.bss`, and until now the only thing that ever reported the
/// limit was a linker overflow after the fact, naming a section rather than a
/// clip.
///
/// Measured 2026-08-19 by raising `PERSISTENT_ASSET_PAGE_COUNT` in the
/// generated manifest and relinking: 354 pages (724,992 B) fits, 355 overflows
/// the RAM region by 368 bytes. The linker script gives static sections
/// `2M - 64K BIOS - 32K stack` = 1,998,848 B, so that ceiling is really "what
/// is left after this build's code and other arenas", and it moves when they
/// do. The cap sits 10 pages under the measured point so ordinary code growth
/// cannot win the race and put the linker back in front of the audit.
///
/// ponytail: one global number, because every model clip is currently
/// `PersistentGameplay` and so resident for the entire session. When per-scene
/// residency lands this becomes a per-scene figure. Re-measure by bisecting the
/// manifest constant against the link; do not trust this value after a large
/// code change.
pub const PLAYTEST_RESIDENT_ASSET_PAGES: usize = 344;

/// Measured link ceiling behind [`PLAYTEST_RESIDENT_ASSET_PAGES`], kept so the
/// margin is visible rather than folded into one rounded number.
pub const PLAYTEST_RESIDENT_ASSET_MEASURED_PAGES: usize = 354;

/// Resident ceiling in bytes.
pub const PLAYTEST_RESIDENT_ASSET_BYTES: usize =
    PLAYTEST_RESIDENT_ASSET_PAGES * PLAYTEST_RESIDENT_ASSET_PAGE_BYTES;

/// How close to the ceiling counts as worth saying out loud. Catching it at
/// 101% is too late to be useful; the point of the audit is hearing about it
/// while there is still room to act.
pub const PLAYTEST_RESIDENT_ASSET_WARN_PERCENT: usize = 90;

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
    ResidentAssets,
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
            Self::ResidentAssets => "Resident assets",
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
    /// Packet limit this report was judged against: the derivation ceiling
    /// for authored estimates, the exact derived arena capacity after a cook.
    pub packet_limit: usize,
    /// Payloads that stay in RAM for the whole gameplay session. Zero on the
    /// authored estimate, which has no cooked asset table to measure; the
    /// number is only exact after a cook, which is where the gate lives.
    pub resident_asset_bytes: usize,
    pub issues: Vec<PlaytestBudgetIssue>,
}

impl PlaytestBudgetReport {
    pub fn first_actionable_issue(&self) -> Option<&PlaytestBudgetIssue> {
        self.issues.iter().find(|issue| issue.target.is_some())
    }

    pub fn concise_summary(&self) -> String {
        format!(
            "{}: BSP {}B, PVS {}B (row {}/{}B), light {}B, textures {}B, RAM {}/{}B ({} slots), VRAM {}/{}B ({} slots), packets {}/{}, resident {}/{}B",
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
            self.packet_limit,
            self.resident_asset_bytes,
            PLAYTEST_RESIDENT_ASSET_BYTES,
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
        .saturating_add(vertex_count.saturating_mul(Vertex::SIZE))
        .saturating_add(face_count.saturating_mul(
            Plane::SIZE + Face::SIZE + size_of::<u16>() + Node::SIZE + ClipNode::SIZE,
        ))
        .saturating_add(leaf_count.saturating_mul(Leaf::SIZE))
        .saturating_add(pvs_bytes)
        .saturating_add(BrushModel::SIZE);
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
        // Nothing to measure yet: clips are only word-padded and classified
        // during the cook, and guessing from the authored resources would put
        // a number here that the cook then contradicts.
        resident_asset_bytes: 0,
        // Before a cook the arena can still grow to the ceiling, so only
        // content the derivation cannot cover is an authoring problem.
        packet_limit: PLAYTEST_PACKET_CAPACITY_CEILING,
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
            pvs_bytes = visibility.len as usize;
            let leaf_size = PxbspLumpKind::Leaves
                .record_size(index.version())
                .expect("PXBSP leaves are fixed records") as usize;
            pvs_row_bytes = (leaves.len as usize / leaf_size)
                .saturating_sub(1)
                .div_ceil(8);
            light_bytes = light_bytes.saturating_add(vertices.len as usize / Vertex::SIZE * 3);
            bsp_packets = cooked_bsp_packets(package);
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
    let packet_count = cooked_packet_count(package, bsp_packets);
    let packet_limit = if matches!(package.world_geometry, PlaytestWorldGeometry::Pxbsp(_)) {
        derived_packet_capacity(packet_count).min(PLAYTEST_PXBSP_PACKET_CAPACITY_CEILING)
    } else {
        derived_packet_capacity(packet_count)
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
        packet_limit,
        resident_asset_bytes: audit_resident_assets(package).total_bytes,
        issues: Vec::new(),
    };
    attach_budget_issues(project, &mut report);
    report
}

/// Emitted-face PXBSP packet count, or zero when the package is not PXBSP or
/// the world bytes fail to parse.
fn cooked_bsp_packets(package: &PlaytestPackage) -> usize {
    let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
        return 0;
    };
    let mut map = psx_bsp::pxbsp_resident::PxbspResidentMap::with_capacity(world.bytes.len());
    if map.load(0, &mut SliceReader::new(&world.bytes)).is_err() {
        return 0;
    }
    let faces = map.faces();
    let materials = map.materials();
    let is_layered_sky = |surface: usize| -> bool {
        faces
            .get(surface)
            .and_then(|face| materials.get(face.texture as usize))
            .is_some_and(|material| material.flags & material_flags::LAYERED_SKY != 0)
    };
    let packet_slots_of = |surface: usize| -> usize {
        if is_layered_sky(surface) {
            0
        } else {
            faces.get(surface).map_or(0, |face| {
                (face.vertex_count.max(0) as usize).saturating_sub(2)
            })
        }
    };
    let mut total: usize = (0..faces.len()).map(packet_slots_of).sum();
    if (0..faces.len()).any(is_layered_sky) {
        total = total.saturating_add(VIEW_RAY_SKY_PACKET_SLOTS);
    }
    // The packet arena only ever holds ONE frame's draw, and a frame
    // draws the PVS of one leaf, not the whole map. Size from the worst
    // leaf's deduped visible-surface triangles; the map total (which
    // light subdivision inflates freely) only measures content, and
    // deriving the arena from it is what pushed lit worlds out of RAM.
    let leaves = map.leaves();
    let marks = map.mark_surfaces();
    let leaf_count = leaves.len();
    let mut row = vec![0u8; leaf_count / 8 + 16];
    let mut seen = vec![false; faces.len()];
    let mut worst = 0usize;
    for leaf_index in 1..leaf_count {
        let Some(visible_leaves) = map.leaf_visibility_into(leaf_index, &mut row) else {
            continue;
        };
        for flag in seen.iter_mut() {
            *flag = false;
        }
        let mut frame_packet_slots = 0usize;
        let mut frame_has_layered_sky = false;
        for bit in 0..visible_leaves {
            if row[bit / 8] & (1 << (bit % 8)) == 0 {
                continue;
            }
            let Some(visible) = leaves.get(bit + 1) else {
                continue;
            };
            let first = usize::from(visible.first_mark_surface);
            for mark in first..first.saturating_add(usize::from(visible.mark_surface_count)) {
                let Some(surface) = marks.get(mark).map(usize::from) else {
                    continue;
                };
                if surface < seen.len() && !seen[surface] {
                    seen[surface] = true;
                    frame_has_layered_sky |= is_layered_sky(surface);
                    frame_packet_slots =
                        frame_packet_slots.saturating_add(packet_slots_of(surface));
                }
            }
        }
        if frame_has_layered_sky {
            frame_packet_slots = frame_packet_slots.saturating_add(VIEW_RAY_SKY_PACKET_SLOTS);
        }
        worst = worst.max(frame_packet_slots);
    }
    if worst == 0 {
        total
    } else {
        worst
    }
}

#[cfg(test)]
fn pxbsp_face_packets(version: PxbspVersion, bytes: &[u8]) -> Option<usize> {
    match version {
        PxbspVersion::V4 | PxbspVersion::V5 => {
            RecordSlice::<Face>::new(bytes)?
                .iter()
                .try_fold(0usize, |total, face| {
                    let vertex_count = usize::try_from(face.vertex_count).ok()?;
                    total.checked_add(vertex_count.saturating_sub(2))
                })
        }
        PxbspVersion::V1 => {
            let faces = bytes.chunks_exact(14);
            if !faces.remainder().is_empty() {
                return None;
            }
            faces.fold(Some(0usize), |total, face| {
                let vertex_count = usize::try_from(i16::from_le_bytes([face[8], face[9]])).ok()?;
                total?.checked_add(vertex_count.saturating_sub(2))
            })
        }
    }
}

fn cooked_packet_count(package: &PlaytestPackage, bsp_packets: usize) -> usize {
    let envelope = playtest_performance_envelope(package).unwrap_or_default();
    if bsp_packets == 0 {
        envelope
            .tr_packets_before_hw_split
            .saturating_add(envelope.prop_surfaces)
    } else {
        bsp_packets.saturating_add(envelope.prop_surfaces)
    }
}

/// Per-project primitive arena capacity the generated manifest publishes for
/// this cooked package.
pub fn cooked_manifest_packet_capacity(package: &PlaytestPackage) -> usize {
    let capacity =
        derived_packet_capacity(cooked_packet_count(package, cooked_bsp_packets(package)));
    if matches!(package.world_geometry, PlaytestWorldGeometry::Pxbsp(_)) {
        capacity.min(PLAYTEST_PXBSP_PACKET_CAPACITY_CEILING)
    } else {
        capacity
    }
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
    if report.packet_count > report.packet_limit {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::Packets,
            used: report.packet_count,
            limit: report.packet_limit,
            unit: " packets",
            target: geometry_target,
            action: "split visibility or simplify the focused brush/room",
        });
    }
    if report.resident_asset_bytes > PLAYTEST_RESIDENT_ASSET_BYTES {
        report.issues.push(PlaytestBudgetIssue {
            kind: PlaytestBudgetKind::ResidentAssets,
            used: report.resident_asset_bytes,
            limit: PLAYTEST_RESIDENT_ASSET_BYTES,
            unit: "B",
            target: ram_target,
            action: "shorten or drop animation clips; every clip stays resident for the \
                     whole session",
        });
    }
}

/// One resident payload, as the audit reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentAssetEntry {
    /// Owning model, parsed out of the asset's diagnostic label.
    pub owner: String,
    /// Cooked filename, so an offender can be found on disc.
    pub filename: String,
    /// Bytes as the runtime arena counts them, word-padded.
    pub bytes: usize,
    /// What the payload is.
    pub kind: PlaytestAssetKind,
}

/// What stays in RAM for the whole gameplay session, and how close that is to
/// the ceiling.
///
/// hl-psx re-measures this on every cook and fails the build when a growing
/// model would hide an actor on hardware. We had the same information arriving
/// as a linker error, which is both later and less specific: it names a
/// section, not a clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentAssetAudit {
    /// Total resident bytes, matching what sizes the runtime asset arena.
    pub total_bytes: usize,
    /// Ceiling this was judged against.
    pub cap_bytes: usize,
    /// Every resident payload, largest first.
    pub entries: Vec<ResidentAssetEntry>,
}

impl ResidentAssetAudit {
    /// Pages the runtime arena actually reserves, which is the number the
    /// cooked manifest emits as `PERSISTENT_ASSET_PAGE_COUNT` and therefore the
    /// number the linker sees. Judging raw bytes would under-report by up to a
    /// page and disagree with the guest build.
    pub fn resident_pages(&self) -> usize {
        self.total_bytes
            .div_ceil(PLAYTEST_RESIDENT_ASSET_PAGE_BYTES)
    }

    /// Over the ceiling, so the cook must refuse.
    pub fn over_cap(&self) -> bool {
        self.resident_pages() > PLAYTEST_RESIDENT_ASSET_PAGES
    }

    /// Close enough to the ceiling to be worth a line of output.
    pub fn near_cap(&self) -> bool {
        self.percent_of_cap() >= PLAYTEST_RESIDENT_ASSET_WARN_PERCENT
    }

    /// Share of the ceiling used, rounded down.
    pub fn percent_of_cap(&self) -> usize {
        if PLAYTEST_RESIDENT_ASSET_PAGES == 0 {
            return 0;
        }
        self.resident_pages().saturating_mul(100) / PLAYTEST_RESIDENT_ASSET_PAGES
    }

    /// Resident bytes per owning model, largest first. This is the view that
    /// answers "which character is eating the budget".
    pub fn by_owner(&self) -> Vec<(String, usize)> {
        let mut totals: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for entry in &self.entries {
            *totals.entry(entry.owner.as_str()).or_default() += entry.bytes;
        }
        let mut rows: Vec<(String, usize)> = totals
            .into_iter()
            .map(|(owner, bytes)| (owner.to_string(), bytes))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows
    }

    /// One line for the cook log.
    pub fn summary(&self) -> String {
        format!(
            "resident assets {} B in {} pages of {} ({}%), {} payloads",
            self.total_bytes,
            self.resident_pages(),
            PLAYTEST_RESIDENT_ASSET_PAGES,
            self.percent_of_cap(),
            self.entries.len(),
        )
    }

    /// The breakdown, owners first and then the biggest individual payloads.
    /// `clip_limit` bounds the payload list so a 43-clip character does not
    /// bury the owner totals.
    pub fn breakdown(&self, clip_limit: usize) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (owner, bytes) in self.by_owner() {
            let _ = write!(out, "\n  {owner}: {bytes} B");
        }
        if self.entries.len() > 1 {
            let _ = write!(out, "\n  largest payloads:");
            for entry in self.entries.iter().take(clip_limit) {
                let _ = write!(out, "\n    {} {} B", entry.filename, entry.bytes);
            }
            if self.entries.len() > clip_limit {
                let _ = write!(
                    out,
                    "\n    ... and {} more",
                    self.entries.len() - clip_limit
                );
            }
        }
        out
    }
}

/// Measure what the cooked package keeps resident for the whole session.
///
/// Counts exactly what sizes the runtime arena: `PersistentGameplay` payloads,
/// word-padded, the same sum the manifest turns into
/// `PERSISTENT_ASSET_PAGE_COUNT`. Anything streamed per room or per menu is
/// deliberately absent, since it does not compete for this ceiling.
pub fn audit_resident_assets(package: &PlaytestPackage) -> ResidentAssetAudit {
    let mut entries: Vec<ResidentAssetEntry> = package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == StreamedClass::PersistentGameplay)
        .map(|asset| ResidentAssetEntry {
            owner: resident_asset_owner(&asset.source_label),
            filename: asset.filename.clone(),
            bytes: asset.bytes.len().next_multiple_of(4),
            kind: asset.kind,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.filename.cmp(&b.filename))
    });
    ResidentAssetAudit {
        total_bytes: entries.iter().map(|entry| entry.bytes).sum(),
        cap_bytes: PLAYTEST_RESIDENT_ASSET_BYTES,
        entries,
    }
}

/// Pull the owning model out of an asset's diagnostic label. Clips are labelled
/// `"<model> / <clip>"` and atlases `"<model> atlas"`, so both collapse onto the
/// model that carries them.
fn resident_asset_owner(source_label: &str) -> String {
    let head = source_label
        .split_once(" / ")
        .map(|(owner, _)| owner)
        .unwrap_or(source_label);
    head.strip_suffix(" atlas").unwrap_or(head).to_string()
}

/// Refuse a package whose session-resident payloads exceed the ceiling.
///
/// Returns the full breakdown as the error, because the useful form of this
/// failure is "this clip is 12 KB over", not "the build is too big".
pub fn validate_resident_assets(package: &PlaytestPackage) -> Result<ResidentAssetAudit, String> {
    let audit = audit_resident_assets(package);
    if !audit.over_cap() {
        return Ok(audit);
    }
    let over_pages = audit.resident_pages() - PLAYTEST_RESIDENT_ASSET_PAGES;
    Err(format!(
        "session-resident assets need {} pages, {} over the {}-page ceiling \
         ({} B used, {} B over).\n\
         Every model mesh, atlas and animation clip is cooked PersistentGameplay, so all \
         of it sits in guest .bss for the whole session; the MIPS link is what fails next \
         (measured ceiling {} pages).{}\n\
         Shorten or drop clips, or scope them per scene. Nothing was written; the \
         previously generated output is intact.",
        audit.resident_pages(),
        over_pages,
        PLAYTEST_RESIDENT_ASSET_PAGES,
        audit.total_bytes,
        over_pages * PLAYTEST_RESIDENT_ASSET_PAGE_BYTES,
        PLAYTEST_RESIDENT_ASSET_MEASURED_PAGES,
        audit.breakdown(12),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use crate::playtest::build_package;

    #[test]
    fn estimated_packet_overflow_points_at_largest_brush() {
        let mut project = ProjectDocument::new("budget focus");
        // Enough cuboids to overflow even the derivation ceiling: only
        // content the runtime arena cannot grow to cover is an issue.
        for index in 0..400 {
            project
                .active_scene_mut()
                .brushes
                .push(Brush::cuboid([index * 4, 0, 0], [index * 4 + 2, 2, 2]));
        }
        let report = estimate_playtest_budgets(&project, Path::new("."));
        assert_eq!(report.packet_limit, PLAYTEST_PACKET_CAPACITY_CEILING);
        let issue = report
            .issues
            .iter()
            .find(|issue| issue.kind == PlaytestBudgetKind::Packets)
            .expect("packet issue");
        assert_eq!(
            issue.target,
            Some(PlaytestValidationTarget::Brush {
                brush: 399,
                face: None,
            })
        );
        assert!(issue.message().contains("simplify the focused brush/room"));
    }

    #[test]
    fn derived_packet_capacity_clamps_and_rounds() {
        assert_eq!(derived_packet_capacity(0), PLAYTEST_PACKET_LIMIT);
        assert_eq!(derived_packet_capacity(1536), PLAYTEST_PACKET_LIMIT);
        assert_eq!(derived_packet_capacity(1537), 1600);
        assert_eq!(derived_packet_capacity(2015), 2048);
        assert_eq!(
            derived_packet_capacity(9999),
            PLAYTEST_PACKET_CAPACITY_CEILING
        );
        assert_eq!(PRIMITIVE_PACKET_SLOT_WORDS, 14);
        assert_eq!(VIEW_RAY_SKY_PACKET_SLOTS, 172);
    }

    /// A resident payload as the cook emits it: model clips carry
    /// `"<model> / <clip>"` labels, atlases `"<model> atlas"`.
    fn resident_asset(
        kind: PlaytestAssetKind,
        source_label: &str,
        filename: &str,
        bytes: usize,
    ) -> super::super::PlaytestAsset {
        super::super::PlaytestAsset {
            kind,
            bytes: vec![0u8; bytes],
            filename: filename.to_string(),
            source_label: source_label.to_string(),
            streamed_class: StreamedClass::PersistentGameplay,
        }
    }

    /// The audit only reads `package.assets`, so cook the tracked fixture for
    /// a valid package and swap its asset table for the case under test.
    fn package_with_assets(assets: Vec<super::super::PlaytestAsset>) -> PlaytestPackage {
        let project = ProjectDocument::from_ron_str(include_str!(
            "../../../../archive/fixtures/brush-open-courtyard/project.ron"
        ))
        .expect("tracked project");
        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../archive/fixtures/brush-open-courtyard");
        let (package, report) = build_package(&project, &fixture_dir);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let mut package = package.expect("fixture cooks");
        package.assets = assets;
        package
    }

    #[test]
    fn resident_audit_groups_clips_under_their_model_and_ignores_streamed_assets() {
        let mut assets = vec![
            resident_asset(
                PlaytestAssetKind::ModelMesh,
                "Aletha",
                "aletha/mesh.psxmdl",
                4_000,
            ),
            resident_asset(
                PlaytestAssetKind::Texture,
                "Aletha atlas",
                "aletha/atlas.psxt",
                2_000,
            ),
            resident_asset(
                PlaytestAssetKind::ModelAnimation,
                "Aletha / run",
                "aletha/clip_00_run.psxanim",
                9_000,
            ),
            resident_asset(
                PlaytestAssetKind::ModelAnimation,
                "Rust Mantis / idle",
                "mantis/clip_00_idle.psxanim",
                5_000,
            ),
        ];
        // A room texture streams per room, so it must not count against the
        // session-resident ceiling.
        let mut streamed = resident_asset(
            PlaytestAssetKind::Texture,
            "Room atlas",
            "rooms/atlas.psxt",
            500_000,
        );
        streamed.streamed_class = StreamedClass::Gameplay;
        assets.push(streamed);

        let audit = audit_resident_assets(&package_with_assets(assets));

        assert_eq!(audit.total_bytes, 4_000 + 2_000 + 9_000 + 5_000);
        assert_eq!(audit.entries.len(), 4, "streamed asset must be excluded");
        assert_eq!(
            audit.by_owner(),
            vec![
                ("Aletha".to_string(), 15_000),
                ("Rust Mantis".to_string(), 5_000),
            ],
            "clips and atlas must fold into the model that carries them"
        );
        // Largest first, so the breakdown opens with the offender.
        assert_eq!(audit.entries[0].filename, "aletha/clip_00_run.psxanim");
        assert!(!audit.over_cap());
    }

    #[test]
    fn resident_audit_counts_the_padding_the_arena_pays() {
        // The manifest sizes the arena from word-padded lengths, so an audit
        // reading raw lengths would under-report every odd-sized clip.
        let audit = audit_resident_assets(&package_with_assets(vec![resident_asset(
            PlaytestAssetKind::ModelAnimation,
            "Aletha / run",
            "aletha/clip_00_run.psxanim",
            4_001,
        )]));
        assert_eq!(audit.total_bytes, 4_004);
    }

    #[test]
    fn resident_audit_refuses_over_the_ceiling_and_names_the_offender() {
        let over = PLAYTEST_RESIDENT_ASSET_BYTES + 12 * 1024;
        let package = package_with_assets(vec![
            resident_asset(
                PlaytestAssetKind::ModelAnimation,
                "Aletha / run",
                "aletha/clip_00_run.psxanim",
                over - 1_000,
            ),
            resident_asset(
                PlaytestAssetKind::ModelAnimation,
                "Rust Mantis / idle",
                "mantis/clip_00_idle.psxanim",
                1_000,
            ),
        ]);

        let audit = audit_resident_assets(&package);
        assert!(audit.over_cap());
        assert!(audit.near_cap());

        let message = validate_resident_assets(&package).expect_err("must refuse");
        assert!(
            message.contains("12288 B over"),
            "the overage is the actionable number: {message}"
        );
        assert!(
            message.contains("aletha/clip_00_run.psxanim"),
            "must name the biggest payload: {message}"
        );
        assert!(
            message.contains("Nothing was written"),
            "must promise a clean no-op: {message}"
        );
    }

    #[test]
    fn resident_audit_warns_before_it_refuses() {
        // The point of the audit is hearing about the ceiling early, so the
        // warn band must fire while the cook still succeeds.
        let near = PLAYTEST_RESIDENT_ASSET_BYTES / 100 * PLAYTEST_RESIDENT_ASSET_WARN_PERCENT;
        let package = package_with_assets(vec![resident_asset(
            PlaytestAssetKind::ModelAnimation,
            "Aletha / run",
            "aletha/clip_00_run.psxanim",
            near,
        )]);
        let audit = validate_resident_assets(&package).expect("under the ceiling");
        assert!(audit.near_cap(), "{}", audit.summary());
        assert!(!audit.over_cap(), "{}", audit.summary());
    }

    #[test]
    fn current_pxbsp_budget_decodes_face_counts_and_leaf_stride() {
        let project = ProjectDocument::from_ron_str(include_str!(
            "../../../../archive/fixtures/brush-open-courtyard/project.ron"
        ))
        .expect("tracked PXBSP project");
        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../archive/fixtures/brush-open-courtyard");
        let (package, report) = build_package(&project, &fixture_dir);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let package = package.expect("cooked PXBSP package");
        let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
            panic!("tracked fixture must cook PXBSP");
        };
        let index = PxbspIndex::read(&mut SliceReader::new(&world.bytes)).expect("PXBSP index");
        assert_eq!(index.version(), PxbspVersion::V5);
        let faces = index.lump(PxbspLumpKind::Faces);
        assert_eq!(faces.len % Face::SIZE as u32, 0);
        let face_bytes = &world.bytes[faces.offset as usize..faces.end() as usize];
        let face_count = face_bytes.len() / Face::SIZE;
        assert!(face_count > 0);
        // The compact face parser counts vertex_count - 2 packets per face
        // (a legacy 14-byte parse of these records read light-style bytes as
        // the count and inflated wildly); every packet total stays bounded
        // by three triangles per face and the worst-leaf sizing never
        // exceeds the map total.
        let total = pxbsp_face_packets(PxbspVersion::V5, face_bytes).expect("compact faces");
        assert!(
            total >= face_count && total <= face_count * 3,
            "{total} for {face_count} faces"
        );
        let worst_leaf = cooked_bsp_packets(&package);
        assert!(
            worst_leaf > 0 && worst_leaf <= total,
            "{worst_leaf} vs {total}"
        );
        let budget = cooked_playtest_budgets(&project, &package);
        assert_eq!(
            budget.packet_count,
            cooked_packet_count(&package, worst_leaf)
        );
        let leaves = index.lump(PxbspLumpKind::Leaves).len as usize
            / PxbspLumpKind::Leaves.record_size(PxbspVersion::V5).unwrap() as usize;
        assert_eq!(budget.pvs_row_bytes, leaves.saturating_sub(1).div_ceil(8));
    }
}
