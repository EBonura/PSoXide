use std::collections::BTreeSet;
use std::mem::size_of;
use std::path::Path;

#[cfg(test)]
use psx_engine::PRIMITIVE_PACKET_SLOT_WORDS;

use super::{
    playtest_performance_envelope, PlaytestAssetKind, PlaytestInteractableKind, PlaytestPackage,
    PlaytestValidationTarget, StreamedClass,
};
use crate::{NodeKind, ProjectDocument, ResourceData};

/// Baseline primitive packet arena of the normal editor-playtest runtime.
/// The cooked manifest derives the actual per-project capacity from the
/// conservative packet envelope, between this floor and
/// [`PLAYTEST_PACKET_CAPACITY_CEILING`].
pub const PLAYTEST_PACKET_LIMIT: usize = 1536;
/// Largest packet arena the runtime may be asked to instantiate. Content
/// whose envelope exceeds this must be restructured, not silently degraded.
pub const PLAYTEST_PACKET_CAPACITY_CEILING: usize = 4096;
/// The runtime keeps a bounded packet arena at this size. The world renderer
/// writes its packets straight into it and, with the whole topology and
/// collision image resident, cannot afford a larger scratch on a 2 MB machine;
/// it degrades by dropping over-cap geometry instead of failing to link.
pub const PLAYTEST_WORLD_PACKET_CAPACITY_CEILING: usize = PLAYTEST_PACKET_LIMIT;

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
/// gameplay session. Model animations use
/// `StreamedClass::PersistentGameplay`; meshes and atlases are staged during
/// loading, decoded or uploaded, and then discarded.
///
/// A canary, not the real gate. The real gate is the MIPS link, because the
/// arena is guest `.bss`, and until now the only thing that ever reported the
/// limit was a linker overflow after the fact, naming a section rather than a
/// clip.
///
/// Re-measured 2026-08-31 the way this doc prescribes, by raising
/// `PERSISTENT_ASSET_PAGE_COUNT` in the generated manifest and relinking:
/// 491 pages fits, 492 overflows. The previous figure (354 measured, 344 cap)
/// had gone stale in the direction that matters: the cook passed a 331-page
/// package that the linker then rejected by 71,212 B, so the audit was waving
/// through builds it exists to stop and every failure surfaced as an opaque
/// `.bss will not fit` instead of a budget message.
///
/// The ceiling moved up, not down, because the guest freed ~250 KiB in the
/// same session: opt-level "s" plus -disable-mips-df-backward-search, and
/// band-streaming the cube sky out of the 192 KiB load/render overlay. The linker script gives static sections
/// `2M - 64K BIOS - 32K stack` = 1,998,848 B, so that ceiling is really "what
/// is left after this build's code and other arenas", and it moves when they
/// do. The cap sits 10 pages under the measured point so ordinary code growth
/// cannot win the race and put the linker back in front of the audit.
///
/// One global number, because every model clip is currently
/// `PersistentGameplay` and so resident for the entire session. When per-scene
/// residency lands this becomes a per-scene figure. Re-measure by bisecting the
/// manifest constant against the link; do not trust this value after a large
/// code change.
pub const PLAYTEST_RESIDENT_ASSET_PAGES: usize = 481;

/// Measured link ceiling behind [`PLAYTEST_RESIDENT_ASSET_PAGES`], kept so the
/// margin is visible rather than folded into one rounded number.
pub const PLAYTEST_RESIDENT_ASSET_MEASURED_PAGES: usize = 491;

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
    Textures,
    Ram,
    Vram,
    Packets,
    ResidentAssets,
}

impl PlaytestBudgetKind {
    pub const fn label(self) -> &'static str {
        match self {
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
    /// Faces in the grid world (authored count before a cook, cooked quads
    /// and triangles after).
    pub world_faces: usize,
    /// Cooked level core plus room chunks; zero before a cook.
    pub world_bytes: usize,
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
            "World {} faces {}B, light {}B, textures {}B, RAM {}/{}B ({} slots), VRAM {}/{}B ({} slots), packets {}/{}, resident {}/{}B",
            self.world_faces,
            self.world_bytes,
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
/// cook. It counts authored faces and reachable textures; the cooked report
/// replaces it after Play with exact emitted sizes and asset figures.
pub fn estimate_playtest_budgets(
    project: &ProjectDocument,
    project_root: &Path,
) -> PlaytestBudgetReport {
    let scene = project.active_scene();
    let world_faces = scene
        .world
        .grid()
        .map_or(0, |world| crate::grid::faces::all_faces(world).len());
    let light_count = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::PointLight { .. }))
        .count();
    let light_bytes = light_count.saturating_mul(size_of::<super::PlaytestLight>());
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
    let vram_asset_slots = texture_paths.len();
    let mut report = PlaytestBudgetReport {
        stage: PlaytestBudgetStage::AuthoredEstimate,
        world_faces,
        world_bytes: 0,
        light_bytes,
        texture_bytes,
        ram_bytes: 0,
        ram_asset_slots: 0,
        vram_bytes: texture_bytes,
        vram_asset_slots,
        packet_count: 0,
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

/// Exact post-cook envelope derived from the emitted level, reachable assets
/// and the conservative packet planner.
pub fn cooked_playtest_budgets(
    project: &ProjectDocument,
    package: &PlaytestPackage,
) -> PlaytestBudgetReport {
    let light_bytes = package.lights.len() * size_of::<super::PlaytestLight>();
    let world = &package.world_geometry;
    let world_bytes = world.core.len() + world.rooms.len();

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
    let ram_bytes = world_bytes.saturating_add(non_texture_bytes);
    let ram_asset_slots = package
        .assets
        .iter()
        .filter(|asset| asset.kind != PlaytestAssetKind::Texture)
        .count()
        .saturating_add(usize::from(world_bytes != 0));
    let vram_asset_slots = texture_assets.len();
    let packet_count = cooked_packet_count(package);
    let packet_limit =
        derived_packet_capacity(packet_count).min(PLAYTEST_WORLD_PACKET_CAPACITY_CEILING);
    let mut report = PlaytestBudgetReport {
        stage: PlaytestBudgetStage::Cooked,
        world_faces: world.faces,
        world_bytes,
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

fn cooked_packet_count(package: &PlaytestPackage) -> usize {
    let envelope = playtest_performance_envelope(package);
    // Each physical Archive Beacon submits 24 Gouraud triangles (one visible
    // shell face, six extrusion sides, and four pivot sides) plus six perimeter
    // lines. Count every cooked POI conservatively because a
    // streamed/portal frame may draw several active rooms into the same arena.
    let poi_marker_packets = package
        .interactables
        .iter()
        .filter(|record| record.kind == PlaytestInteractableKind::PointOfInterest)
        .count()
        .saturating_mul(30);
    envelope.prop_surfaces.saturating_add(poi_marker_packets)
}

/// Per-project primitive arena capacity the generated manifest publishes for
/// this cooked package.
pub fn cooked_manifest_packet_capacity(package: &PlaytestPackage) -> usize {
    derived_packet_capacity(cooked_packet_count(package))
        .min(PLAYTEST_WORLD_PACKET_CAPACITY_CEILING)
}

fn authored_texture_paths(project: &ProjectDocument) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for resource in &project.resources {
        match &resource.data {
            ResourceData::Texture { psxt_path } if !psxt_path.trim().is_empty() => {
                paths.insert(psxt_path.clone());
            }
            ResourceData::Texture { .. } => {}
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
    // The grid is one document, not a node, so a world-level overflow has no
    // single scene object to select.
    let geometry_target: Option<PlaytestValidationTarget> = None;
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
            action: "simplify the focused room or split it with portals",
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
         Model animation clips remain resident for the whole session; model meshes are \
         decoded from loading scratch and atlases are uploaded to VRAM, then both staging \
         payloads are discarded. The MIPS link is what fails next (measured ceiling {} \
         pages).{}\n\
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
    use crate::playtest::build_package;

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
        let fixture_dir = crate::projects_dir().join("grid-cave");
        let project = ProjectDocument::load_from_path(fixture_dir.join("project.ron"))
            .expect("tracked project");
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
                "Light Enemy / idle",
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
                ("Light Enemy".to_string(), 5_000),
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
                "Light Enemy / idle",
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
}
