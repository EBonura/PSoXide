//! Host-side package schema for embedded editor play mode.

use crate::{
    MaterialAnimation, MaterialFaceSidedness, NodeId, PsxBlendMode, ResourceId,
    RuntimeDepthSortMode, RuntimeRoomDrawOrderMode, RuntimeTextureSplitMode, SkyCycloramaQuad,
    UiGradientDirection, UiNodeKind, UiValueBinding,
};

/// Number of cooked character animation action slots.
pub const PLAYTEST_CHARACTER_ACTION_COUNT: usize = psx_level::CHARACTER_ANIMATION_ACTION_COUNT;

/// Cook-time string interner (hl-psx style): authored names become
/// deterministic u16 ids in first-intern order (the scene walk is
/// deterministic), starting at 1; empty/whitespace names intern to
/// `psx_level::LOGIC_NAME_NONE`. The strings never reach the runtime.
#[derive(Default)]
pub(crate) struct NameInterner {
    ids: std::collections::HashMap<String, u16>,
}

impl NameInterner {
    /// Intern `name`, returning its stable id.
    pub(crate) fn intern(&mut self, name: &str) -> u16 {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return psx_level::LOGIC_NAME_NONE;
        }
        if let Some(&id) = self.ids.get(trimmed) {
            return id;
        }
        let id = u16::try_from(self.ids.len() + 1).unwrap_or(u16::MAX);
        self.ids.insert(trimmed.to_string(), id);
        id
    }

    /// Number of distinct interned names.
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Generated subdirectory inside the playtest example that
/// receives manifest source + `rooms/` + `textures/`. Stable
/// so the example's `include!` paths don't move.
pub const GENERATED_DIRNAME: &str = "generated";

/// Tracked placeholder manifest. Kept buildable in source
/// control so a fresh clone can compile before any project cook.
pub const MANIFEST_FILENAME: &str = "level_manifest.rs";

/// Ignored cooked Rust-source manifest written by editor Play /
/// `make cook-playtest`.
pub const COOKED_MANIFEST_FILENAME: &str = "level_manifest.cooked.rs";
/// Generated raw CD-DA payload list for mixed-mode playtest discs.
/// Tracks are listed in disc order (`track 2`, `track 3`, ...).
pub const CDDA_TRACKS_FILENAME: &str = "cdda_tracks.txt";

/// Cooked WORLD.PAK room ordering hint consumed by disc builders.
pub const WORLD_PACK_ORDER_FILENAME: &str = "world_pack_order.txt";

/// Cooked UI.PAK chunk ordering hint consumed by disc builders.
/// One streamed UI asset index per line, in pack order.
pub const UI_PACK_ORDER_FILENAME: &str = "ui_pack_order.txt";

/// Subdirectory inside `generated/` that holds cooked `.psxw`
/// blobs.
pub const ROOMS_DIRNAME: &str = "rooms";

/// Subdirectory inside `generated/` that holds CD-streamable room
/// chunks. Each `.psxc` stores a collision payload plus the cooked
/// render cache for that room.
pub const STREAM_CHUNKS_DIRNAME: &str = "stream_chunks";

/// Subdirectory inside `generated/` that holds CD-streamable UI
/// image chunks. Each `ui_NNN.psxt` is the raw texture payload for
/// one streamed UI image asset, with no chunk header.
pub const UI_STREAM_CHUNKS_DIRNAME: &str = "ui_stream_chunks";

/// Subdirectory inside `generated/` that holds copied `.psxt`
/// texture blobs.
pub const TEXTURES_DIRNAME: &str = "textures";

/// Subdirectory inside `generated/` that holds per-model
/// folders (`model_NNN/`) carrying mesh + atlas + animation
/// blobs. One subfolder per unique [`ResourceData::Model`]
/// referenced by any placed [`NodeKind::MeshInstance`].
pub const MODELS_DIRNAME: &str = "models";

/// Subdirectory inside `generated/` that holds cooked UI SFX `.psau`
/// blobs selected by button/slider cue pools.
pub const UI_SFX_DIRNAME: &str = "ui_sfx";

/// Subdirectory inside `generated/` that holds cooked raw CD-DA
/// track payloads selected by menu music nodes.
pub const CDDA_TRACKS_DIRNAME: &str = "cdda_tracks";

/// Coarse asset class -- mirrors [`psx_level::AssetKind`] but
/// stays host-side `String`/`Vec` friendly. Converted to the
/// runtime enum at write time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestAssetKind {
    /// Cooked `.psxw` room blob.
    RoomWorld,
    /// Cooked `.psxt` texture blob (room atlas or model atlas).
    Texture,
    /// Cooked `.psxmdl` mesh blob.
    ModelMesh,
    /// Cooked `.psxanim` skeletal animation clip.
    ModelAnimation,
}

/// Geometry provider selected for the normal embedded-Play lifecycle.
///
/// Gameplay tables remain common to both variants. The distinction only
/// chooses how the static world, visibility, and collision are supplied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PlaytestWorldGeometry {
    /// Legacy sector-grid rooms backed by cooked `.psxw` assets.
    #[default]
    Grid,
    /// One resident PXBSP world plus its authored brush-mover links.
    Pxbsp(PlaytestPxbspWorld),
}

/// Resident PXBSP payload and the deterministic authored mover mapping needed
/// by normal gameplay logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestPxbspWorld {
    /// Complete PXBSP file emitted by the brush compiler.
    pub bytes: Vec<u8>,
    /// Cooker-proven upper bound for both runtime PVS face chains.
    pub max_visible_faces: usize,
    /// Exact authored body envelopes used for collision hulls one and two.
    pub body_hulls: [psx_bsp::collision_provider::CookedBodyHull; 2],
    /// Texture assets referenced by the resident world's material table.
    /// These stay in the ordinary room-residency contract so the shared VRAM
    /// owner cannot evict them while the BSP world is active.
    pub texture_asset_indices: Vec<usize>,
    /// Brush submodels in authored-node order.
    pub movers: Vec<PlaytestPxbspMover>,
    /// Quake-style pointfile path; empty when the world is sealed.
    pub leak_path: Vec<[i32; 3]>,
}

/// Link from an authored Door logic node to its PXBSP brush submodel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestPxbspMover {
    /// Stable authored [`crate::NodeId`] value.
    pub node: u32,
    /// Model zero is the static world; mover models start at one.
    pub model_index: u16,
}

/// Streaming class of a [`PlaytestAsset`]. A streamed asset's baked
/// manifest static is empty bytes (under `cd-stream-bench`) and its
/// payload is packed into the parallel UI.PAK that the runtime loads
/// on demand, keeping it out of the guest's baked `.data`. The class
/// distinguishes which transient staging buffer the runtime loads
/// through, or whether the payload receives stable session-lifetime
/// storage for parsed models and animations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamedClass {
    /// Baked into the guest `.data` via `include_bytes!`; never streamed.
    None,
    /// Menu UI image. Streamed off UI.PAK and staged through the small
    /// per-image UI staging buffer, loaded on menu entry.
    UiImage,
    /// Gameplay-scoped texture (e.g. the sky panorama). Streamed off
    /// UI.PAK and staged through a larger transient buffer, loaded on
    /// gameplay entry and freed on gameplay exit.
    Gameplay,
    /// Model mesh, atlas, or animation loaded during the initial loading
    /// screen and kept in stable RAM for the complete gameplay session.
    PersistentGameplay,
}

impl StreamedClass {
    /// `true` when the asset is CD-streamed (any non-`None` class).
    pub fn is_streamed(self) -> bool {
        !matches!(self, StreamedClass::None)
    }
}

/// One asset destined for the master table. Owns its bytes so
/// callers can write them out to the generated tree without
/// reaching back into the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestAsset {
    /// Asset class -- drives extension + loader.
    pub kind: PlaytestAssetKind,
    /// Backing payload.
    pub bytes: Vec<u8>,
    /// Filename inside the kind's subdirectory (e.g.
    /// `room_000.psxw`). Stable across runs because asset order
    /// is deterministic.
    pub filename: String,
    /// Diagnostic label -- display name of the source resource
    /// or room. Surfaces in cook reports and stays out of the
    /// runtime contract.
    pub source_label: String,
    /// CD-streaming class. [`StreamedClass::None`] bakes the payload
    /// into the guest `.data`; the other variants route the payload to
    /// UI.PAK keyed by asset index, with the variant selecting the
    /// runtime lifetime and destination arena.
    pub streamed_class: StreamedClass,
}

impl PlaytestAsset {
    /// `true` when this asset's payload is CD-streamed off the asset pack.
    pub fn is_streamed(&self) -> bool {
        self.streamed_class.is_streamed()
    }
}

/// One room's residency-aware record. Carries indices into
/// [`PlaytestPackage::assets`] and [`PlaytestPackage::materials`]
/// so the writer can resolve `AssetId`s deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestRoom {
    /// Display name lifted from the editor scene tree.
    pub name: String,
    /// Index into [`PlaytestPackage::assets`] of the room's `RoomWorld`
    /// asset. Resident PXBSP worlds use `None`: their geometry, collision,
    /// and PVS live in [`PlaytestWorldGeometry::Pxbsp`], not a dummy PSXW.
    pub world_asset_index: Option<usize>,
    /// Host-baked 4bpp environment map used by reflective model materials in
    /// this runtime room. `None` when the project has no reflective materials.
    pub reflection_probe_asset_index: Option<usize>,
    /// Editor-side `WorldGrid::origin[0]` (diagnostic only).
    pub origin_x: i32,
    /// Editor-side `WorldGrid::origin[1]`.
    pub origin_z: i32,
    /// Room vertical placement in engine units, from the Room node's
    /// authored `Transform3::translation[1]`. Diagnostic only for now,
    /// mirroring `origin_x` / `origin_z`: the cooker still normalizes
    /// geometry to array-rooted at ground level.
    pub origin_y: i32,
    /// Engine units per sector.
    pub sector_size: i32,
    /// Camera-space far plane used for room/actor rendering.
    pub draw_distance: i32,
    /// Runtime room activation radius in world sectors.
    pub chunk_activation_radius_sectors: i32,
    /// Cooked PVS traversal radius in room cells.
    pub visibility_radius: u16,
    /// Runtime room residency budget inherited from the World node.
    pub resident_chunk_limit: u8,
    /// Runtime room visible/drawable budget inherited from the World node.
    pub visible_chunk_limit: u8,
    /// Downward acceleration inherited from the World node, in engine units
    /// per fixed 60 Hz tick squared.
    pub gravity_per_tick: i32,
    /// First index into [`PlaytestPackage::materials`] for this
    /// room's slice.
    pub material_first: u16,
    /// Number of material records in the slice. Matches the
    /// cooked `.psxw`'s material count exactly.
    pub material_count: u16,
    /// First directed portal sourced from this room.
    pub portal_first: u16,
    /// Number of directed portals sourced from this room.
    pub portal_count: u8,
    /// First nearby room index. Reserved for portal streaming coherence.
    pub near_room_first: u16,
    /// Number of nearby room indices.
    pub near_room_count: u8,
    /// First overlapped room index. Reserved for stacked-room coherence.
    pub overlapped_room_first: u16,
    /// Number of overlapped room indices.
    pub overlapped_room_count: u8,
    /// Fog/depth-cue far colour.
    pub fog_rgb: [u8; 3],
    /// Fog start distance in engine units.
    pub fog_near: i32,
    /// Fog end distance in engine units.
    pub fog_far: i32,
    /// Base colour for the cheap screen-space room atmosphere pass.
    pub atmosphere_rgb: [u8; 3],
    /// Number of screen-space particles to draw for this room.
    pub atmosphere_density: u8,
    /// Base vertical particle speed, in 1/16 pixel-per-vblank units.
    pub atmosphere_fall_speed_q4: i16,
    /// Base horizontal particle speed, in 1/16 pixel-per-vblank units.
    pub atmosphere_wind_speed_q4: i16,
    /// Resolved world sky for this cooked room.
    pub sky: PlaytestSky,
    /// Resolved far-vista ring for this cooked room.
    pub far_vista: PlaytestFarVista,
    /// Resolved world camera for this cooked room.
    pub camera: PlaytestCamera,
    /// Room flags mirrored into the runtime manifest.
    pub flags: u16,
}

/// One cooked runtime room emitted from an authored map or manual portal split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestChunk {
    /// Owning runtime room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Stable editor Room node id, truncated for compact runtime
    /// diagnostics.
    pub authored_room: u32,
    /// Stable order inside the authored Room's manual portal-room plan.
    pub chunk_index: u16,
    /// Runtime room origin X in authored grid sectors.
    pub origin_x: i32,
    /// Runtime room origin Z in authored grid sectors.
    pub origin_z: i32,
    /// Runtime room width in sectors.
    pub width: u16,
    /// Runtime room depth in sectors.
    pub depth: u16,
    /// Cardinal manual portal-neighbour rooms. `None` means no link.
    pub neighbours: [Option<u16>; 4],
    /// Estimated triangle count from the runtime room budget.
    pub triangles: usize,
    /// Estimated base `.psxw` byte count.
    pub psxw_bytes: usize,
    /// Estimated static-lit `.psxw` byte count.
    pub static_lit_bytes: usize,
    /// Number of populated cells in the cooked runtime room.
    pub populated_cells: u16,
    /// Runtime flags.
    pub flags: u16,
}

/// One directed portal between cooked runtime rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestRoomPortal {
    /// Source room in [`PlaytestPackage::rooms`].
    pub source_room: u16,
    /// Destination room in [`PlaytestPackage::rooms`].
    pub destination_room: u16,
    /// Wall/floor/ceiling kind. Demo7 emits wall portals (`0`).
    pub kind: u8,
    /// Source-facing portal normal.
    pub normal: [i16; 3],
    /// World-space portal rectangle vertices `[BL, BR, TR, TL]`.
    pub vertices: [[i32; 3]; 4],
}

/// Runtime floor-link metadata copied into compact collision sector records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestRoomFloorLink {
    /// Owning runtime room in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Sector X inside the cooked runtime room.
    pub x: u16,
    /// Sector Z inside the cooked runtime room.
    pub z: u16,
    /// Runtime room reached by moving upward through this sector.
    pub above_room: Option<u16>,
    /// Runtime room reached by moving downward through this sector.
    pub below_room: Option<u16>,
}

/// One water-covered runtime sector. Records are sorted by
/// `(room, x, z)` so the runtime can binary-search the player's current cell
/// without scanning authored volumes or geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestWaterCell {
    /// Owning runtime room.
    pub room: u16,
    /// Runtime-room-local sector X.
    pub x: u16,
    /// Runtime-room-local sector Z.
    pub z: u16,
    /// Optional texture asset for the visible water surface.
    pub texture_asset_index: Option<usize>,
    /// PSX semi-transparency code for the surface.
    pub blend_mode: u8,
    /// Material modulation tint.
    pub tint_rgb: [u8; 3],
    /// Material animation preserved for the water-surface render pass.
    pub animation: MaterialAnimation,
    /// Horizontal surface in runtime-room-local engine units.
    pub surface_y: i32,
    /// Terrain depth below the surface at the sector centre.
    pub depth: u16,
    /// Depth at which entering this cell starts water death.
    pub lethal_depth: u16,
    /// Movement speed retained while wading, as a percentage.
    pub movement_percent: u8,
    /// Ticks from lethal submersion to respawn.
    pub death_delay_ticks: u8,
    /// Required submersion before the lethal sequence begins.
    pub death_submerge_depth: u16,
}

/// Resolved sky values written into one runtime room record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestSky {
    /// Zenith colour.
    pub top_rgb: [u8; 3],
    /// Colour at the authored horizon line.
    pub horizon_rgb: [u8; 3],
    /// Colour at the bottom of the frame.
    pub bottom_rgb: [u8; 3],
    /// Horizon line as a percentage of screen height.
    pub horizon_percent: u8,
    /// Angular thickness of the horizon colour band.
    pub horizon_thickness_percent: u8,
    /// Horizontal cyclorama subdivisions.
    pub skybox_columns: u8,
    /// Vertical cyclorama subdivisions.
    pub skybox_rows: u8,
    /// Runtime sky flags.
    pub flags: u16,
    /// Texture asset used by the selected scene sky projection.
    pub texture_asset_index: Option<usize>,
    /// Cooked panorama/cyclorama backdrop geometry.
    pub cyclorama_quads: Vec<SkyCycloramaQuad>,
    /// Cloud-layer parameters used to generate the cooked cyclorama.
    pub cloud_layer: PlaytestCloudLayer,
}

/// Resolved cloud-layer values written into one runtime room record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCloudLayer {
    /// Cooked 4bpp multi-CLUT sky panorama texture used by the runtime.
    pub texture_asset_index: Option<usize>,
    /// Cloud highlight colour.
    pub color_rgb: [u8; 3],
    /// Cloud coverage density 0..=255.
    pub density: u8,
    /// Cyclorama cloud-band vertical bias.
    pub altitude: u16,
    /// Cyclorama cloud-band width.
    pub extent: u16,
    /// Noise/tile repeats across the cloud layer.
    pub tile_count: u8,
    /// Reserved cloud scroll speed.
    pub scroll_speed: [i16; 2],
    /// Perlin generator seed.
    pub noise_seed: u32,
    /// Runtime cloud-layer flags.
    pub flags: u16,
}

/// Resolved far-vista values written into one runtime room record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestFarVista {
    /// Optional indices into [`PlaytestPackage::assets`] for
    /// transparent texture panels. Empty means placeholder cards.
    /// A one-entry list is repeated; a multi-entry list maps across
    /// ring segments in order.
    pub texture_asset_indices: Vec<Option<usize>>,
    /// Radius from camera/player in engine units.
    pub radius: i32,
    /// Ring height in engine units.
    pub height: i32,
    /// Bottom-edge offset from camera height in engine units.
    pub vertical_offset: i32,
    /// Number of cards around the cylinder.
    pub segments: u8,
    /// World yaw rotation in degrees.
    pub rotation_degrees: i16,
    /// Resolved tint.
    pub tint_rgb: [u8; 3],
    /// Runtime far-vista flags.
    pub flags: u16,
}

/// Resolved third-person camera values written into one runtime room record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCamera {
    /// Preferred trailing distance from focus to camera.
    pub distance: i32,
    /// Camera origin height above the player origin.
    pub height: i32,
    /// Look-at height above the player origin.
    pub target_height: i32,
    /// Additional lock-on camera elevation as a percentage of camera height.
    pub lock_rise_percent: u8,
    /// Minimum camera origin height above the sampled floor.
    pub min_floor_clearance: i32,
    /// Manual orbit input speed level. Higher values turn faster.
    pub orbit_speed_level: u8,
    /// Camera origin follow lag shift. Lower values move faster.
    pub position_lag_shift: u8,
    /// Camera focus follow lag shift. Lower values move faster.
    pub focus_lag_shift: u8,
    /// Collision boom recovery lag shift. Lower values move faster.
    pub distance_lag_shift: u8,
}

/// Per-room slice into generated visibility cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestRoomVisibility {
    /// Owning room index.
    pub room: u16,
    /// First index into [`PlaytestPackage::visibility_cells`].
    pub cell_first: u16,
    /// Number of visibility cells for this room.
    pub cell_count: u16,
    /// First index into [`PlaytestPackage::visibility_pvs`].
    pub pvs_first: u32,
    /// Number of PVS records for this room.
    pub pvs_count: u16,
}

/// One cooked position-cell PVS bitset slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestVisibilityPvs {
    /// First byte in [`PlaytestPackage::visibility_pvs_bits`].
    pub byte_first: u32,
    /// Number of bitset bytes.
    pub byte_count: u16,
}

/// One cooked room grid cell with precomputed visibility metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestVisibilityCell {
    /// Owning room index.
    pub room: u16,
    /// Cell X coordinate inside the cooked `.psxw`.
    pub x: u16,
    /// Cell Z coordinate inside the cooked `.psxw`.
    pub z: u16,
    /// Minimum surface height in room-local engine units.
    pub min_y: i32,
    /// Maximum surface height in room-local engine units.
    pub max_y: i32,
    /// Cardinal portal/open-edge mask.
    pub portal_mask: u8,
    /// Cardinal full-height solid-blocker mask.
    pub blocker_mask: u8,
    /// Room-local index into [`PlaytestPackage::room_cache_cells`]
    /// relative to the owning room cache's `cell_first`.
    pub cache_cell_index: u16,
    /// Runtime flags.
    pub flags: u16,
}

/// Per-room slice into generated cached room geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestRoomSurfaceCache {
    /// Owning room index.
    pub room: u16,
    /// First index into [`PlaytestPackage::room_cache_cells`].
    pub cell_first: u32,
    /// Number of cached cell records for this room.
    pub cell_count: u16,
    /// First index into [`PlaytestPackage::room_cache_cell_vertices`].
    /// A zero count means runtime derives the visible vertex set
    /// from each cell's surface range.
    pub cell_vertex_first: u32,
    /// Number of per-cell cached vertex indices for this room.
    pub cell_vertex_count: u16,
    /// First index into [`PlaytestPackage::room_cache_vertices`].
    pub vertex_first: u32,
    /// Number of cached vertex records for this room.
    pub vertex_count: u16,
    /// First index into [`PlaytestPackage::room_cache_surfaces`].
    pub surface_first: u32,
    /// Number of cached surface records for this room.
    pub surface_count: u16,
}

/// Cached populated-cell header generated for editor-playtest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCachedRoomCell {
    /// Grid X coordinate inside the cooked room.
    pub x: u16,
    /// Grid Z coordinate inside the cooked room.
    pub z: u16,
    /// Minimum authored surface height in room-local engine units.
    pub min_y: i32,
    /// Maximum authored surface height in room-local engine units.
    pub max_y: i32,
    /// Precomputed visibility center as `[x, y, z]`.
    pub visibility_center: [i32; 3],
    /// Precomputed visibility radius.
    pub visibility_radius: i32,
    /// First cached surface in this room-local cell.
    pub surface_first: u16,
    /// Number of cached surfaces in this cell.
    pub surface_count: u16,
    /// First room-local vertex index entry for this cell.
    pub vertex_first: u16,
    /// Number of unique cached vertices referenced by this cell.
    pub vertex_count: u16,
}

/// Cached deduplicated room vertex generated for editor-playtest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCachedRoomVertex {
    /// Room-local X coordinate.
    pub x: i32,
    /// Room-local Y coordinate.
    pub y: i32,
    /// Room-local Z coordinate.
    pub z: i32,
}

/// Cached room surface generated for editor-playtest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCachedRoomSurface {
    /// Local room material slot referenced by this surface.
    pub material_slot: u16,
    /// Indices into the room-local cached vertex stream.
    pub vertex_indices: [u16; 4],
    /// Sector X coordinate for lighting-sample reconstruction.
    pub sample_sx: u16,
    /// Sector Z coordinate for lighting-sample reconstruction.
    pub sample_sz: u16,
    /// Surface ordinal for lighting-sample reconstruction.
    pub sample_ordinal: u16,
    /// Packed low 16 bits of each packet UV word: `u | v << 8`.
    pub uv_words: [u16; 4],
    /// Cached baked RGB values.
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    /// Packed surface kind plus cached render flags.
    pub kind_flags: u8,
    /// Runtime wall direction when this is a wall surface.
    pub wall_direction: u8,
    /// Authored diagonal split id for floors/ceilings.
    pub split: u8,
    /// Split-triangle index, or the whole-quad sentinel.
    pub triangle_index: u8,
}

/// One material slot binding. Lifted from
/// [`CookedWorldMaterial`] and pinned to its owning room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestMaterial {
    /// Owning room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Cooked-world local material slot -- matches the slot value
    /// stored in the `.psxw`.
    pub local_slot: u16,
    /// Index into [`PlaytestPackage::assets`] of the texture
    /// asset bound at this slot.
    pub texture_asset_index: usize,
    /// Per-material modulation tint.
    pub tint_rgb: [u8; 3],
    /// PS1 blend equation used by this room material.
    pub blend_mode: PsxBlendMode,
    /// One-pass room-material animation recipe.
    pub animation: MaterialAnimation,
    /// Which side(s) of faces using this material should render.
    pub face_sidedness: MaterialFaceSidedness,
}

/// One animation clip bound to a [`PlaytestModel`]. Carries
/// pre-resolved indices into the master asset table so the
/// writer can emit `LevelModelClipRecord`s without re-walking
/// the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestModelClip {
    /// Owning model index in [`PlaytestPackage::models`].
    pub model: u16,
    /// Display name surfaced in debug HUDs.
    pub name: String,
    /// Index into [`PlaytestPackage::assets`] of the cooked
    /// `.psxanim` blob.
    pub animation_asset_index: usize,
    /// Standalone Animation Clip resource this model clip resolved from.
    pub animation_resource: Option<ResourceId>,
    /// First/last source frames retained by still-end trimming, before any
    /// error-budget resampling. Animation Studio event frames are mapped from
    /// this authored domain into the final cooked frame count.
    pub source_frame_first: u16,
    pub source_frame_last: u16,
    pub source_frame_count: u16,
    pub cooked_frame_count: u16,
}

/// Bounds-table slice for one global model clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestModelClipBounds {
    /// Owning model index in [`PlaytestPackage::models`].
    pub model: u16,
    /// Global clip index in [`PlaytestPackage::model_clips`].
    pub clip: u16,
    /// First index in [`PlaytestPackage::model_frame_bounds`].
    pub first_frame: u16,
    /// Number of frame-bound records for this clip.
    pub frame_count: u16,
    /// Grounding floor in raw model-local units.
    pub floor_y: i32,
    /// Additional model-local pose offset in cooked pose units.
    pub pose_offset: [i32; 3],
    /// See `psx_level::model_clip_flags`.
    pub flags: u16,
}

/// Conservative local-space sphere for one animation frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestModelFrameBounds {
    /// Model-local center in engine world units.
    pub center: [i32; 3],
    /// Conservative radius in engine world units.
    pub radius: i32,
    /// Grounding floor in raw model-local units.
    pub floor_y: i32,
}

/// One named model attachment socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestModelSocket {
    /// Owning model index.
    pub model: u16,
    /// Socket name used by Equipment records.
    pub name: String,
    /// Joint index in the cooked model skeleton.
    pub joint: u16,
    /// Local translation relative to the joint pose.
    pub translation: [i32; 3],
    /// Local Euler rotation in Q12 turns: X/Y/Z.
    pub rotation_q12: [i16; 3],
}

/// One cooked PSX model included in the playtest package. A
/// [`ResourceData::Model`] referenced by any placed instance is
/// promoted into one `PlaytestModel`; multiple instances share
/// the same record (deduplicated by source `ResourceId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestModel {
    /// Display name lifted from the editor resource.
    pub name: String,
    /// Source resource id -- used to deduplicate instances and
    /// to resolve per-instance clip overrides back to clip
    /// indices within this model's slice.
    pub source_resource: ResourceId,
    /// Index into [`PlaytestPackage::assets`] of the cooked
    /// `.psxmdl` blob.
    pub mesh_asset_index: usize,
    /// Index into [`PlaytestPackage::assets`] of the atlas
    /// `.psxt` blob. Always `Some` for placed models -- the
    /// playtest cooker rejects instances of models without an
    /// atlas. Kept as `Option` so the schema can later carry
    /// untextured author-time bundles unchanged.
    pub texture_asset_index: Option<usize>,
    /// First index into [`PlaytestPackage::model_clips`] for
    /// this model's clip slice.
    pub clip_first: u16,
    /// Number of clips in this model's slice. Matches the
    /// editor resource's clip count exactly.
    pub clip_count: u16,
    /// Default clip index *within this model's slice*.
    /// Cooker validation guarantees this is `< clip_count`,
    /// so the runtime always has a clip to play.
    pub default_clip: u16,
    /// First index in [`PlaytestPackage::model_sockets`].
    pub socket_first: u16,
    /// Number of sockets on this model.
    pub socket_count: u16,
    /// World-space height (engine units) -- propagated from the
    /// editor resource.
    pub world_height: u16,
    /// Authored coarse collision radius (engine units) used by
    /// the playtest actor-cylinder blocker.
    pub collision_radius: u16,
}

/// Material override cooked for one placed model instance or the
/// player character. A missing texture keeps the model's baked atlas
/// while still applying blend mode, tint, and face sidedness.
/// Mirrors [`psx_level::LevelModelMaterialOverride`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestModelMaterialOverride {
    /// Index into [`PlaytestPackage::assets`] of an optional covering
    /// `.psxt` texture. `None` keeps the model atlas.
    pub texture_asset_index: Option<usize>,
    /// Authored PS1 blend mode.
    pub blend_mode: crate::PsxBlendMode,
    /// Authored modulation tint.
    pub tint_rgb: [u8; 3],
    /// Independent movement for layer 1.
    pub motion: crate::MaterialUvMotion,
    /// Optional independently blended second texture pass.
    pub secondary_layer: Option<PlaytestModelSecondaryLayer>,
    /// Room-probe controls retained for manifest flag packing. The texture
    /// itself is selected from the actor's current runtime room.
    pub reflection_probe: Option<crate::ReflectionProbeMaterial>,
    /// Authored face sidedness.
    pub face_sidedness: crate::MaterialFaceSidedness,
}

/// Cooked host-side description of a model material's second pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestModelSecondaryLayer {
    /// Index into [`PlaytestPackage::assets`] of the 4bpp layer texture, or
    /// `None` when this layer samples the active room probe.
    pub texture_asset_index: Option<usize>,
    /// Independent authored PS1 blend mode.
    pub blend_mode: crate::PsxBlendMode,
    /// Independent modulation tint.
    pub tint_rgb: [u8; 3],
    /// Signed Q8 texels-per-second UV scroll and initial phase.
    pub motion: crate::MaterialUvMotion,
    /// Active-room probe controls when this layer is reflective.
    pub reflection_probe: Option<crate::ReflectionProbeMaterial>,
}

/// One placed model instance. Coordinates are room-local
/// engine units (the same space cooked rooms live in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestModelInstance {
    /// Owning room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Model index in [`PlaytestPackage::models`].
    pub model: u16,
    /// Per-instance clip override, or [`MODEL_CLIP_INHERIT`]
    /// to use the model's `default_clip`.
    pub clip: u16,
    /// Static pose frame within `clip`, or [`MODEL_INSTANCE_POSE_ANIMATE`]
    /// to advance the clip normally.
    pub pose_frame: u16,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Yaw, PSX angle units.
    pub yaw: i16,
    /// Render-only yaw from the Model Renderer component, PSX angle units.
    pub visual_yaw: i16,
    /// Render-only pitch from the entity transform, PSX angle units.
    /// The runtime composes `Rz(roll) * Ry(yaw + visual_yaw) * Rx(pitch)`,
    /// matching the attachment-socket Euler convention.
    pub pitch: i16,
    /// Render-only roll from the entity transform, PSX angle units.
    pub roll: i16,
    /// Render-only model offset from the authored floor anchor,
    /// in entity-local engine units.
    pub visual_offset: [i16; 3],
    /// Render-only uniform scale in Q8 fixed point (`256 = 1.0`).
    pub visual_scale_q8: u16,
    /// Covering material replacing the model's cooked atlas, or
    /// `None` to render the atlas (the default path).
    pub material_override: Option<PlaytestModelMaterialOverride>,
    /// Reserved.
    pub flags: u16,
}

/// Sentinel for [`PlaytestModelInstance::clip`] meaning
/// "inherit model default" -- same value as
/// [`psx_level::MODEL_CLIP_INHERIT`].
pub const MODEL_CLIP_INHERIT: u16 = 0xFFFF;

/// Sentinel for [`PlaytestModelInstance::pose_frame`] meaning "play the
/// clip" -- same value as [`psx_level::MODEL_INSTANCE_POSE_ANIMATE`].
pub const MODEL_INSTANCE_POSE_ANIMATE: u16 = 0xFFFF;

/// One material-backed flat image prop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestImageProp {
    /// Owning room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Index into [`PlaytestPackage::assets`] of the prop texture.
    pub texture_asset_index: usize,
    /// Bottom-center room-local X.
    pub x: i32,
    /// Bottom Y.
    pub y: i32,
    /// Bottom-center room-local Z.
    pub z: i32,
    /// Static pitch, PSX angle units.
    pub pitch: i16,
    /// Static yaw, PSX angle units.
    pub yaw: i16,
    /// Static roll, PSX angle units.
    pub roll: i16,
    /// Quad width in engine units.
    pub width: u16,
    /// Quad height in engine units.
    pub height: u16,
    /// Material modulation tint.
    pub tint_rgb: [u8; 3],
    /// Baked static light base per quad vertex, in perimeter order.
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    /// Conservative room-local collision AABB minimum.
    pub collision_min: [i32; 3],
    /// Conservative room-local collision AABB maximum.
    pub collision_max: [i32; 3],
    /// Runtime flags.
    pub flags: u16,
}

/// One material-backed editable box prop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestBoxProp {
    /// Owning room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Per-face texture asset indices. `None` skips that face.
    pub texture_asset_indices: [Option<usize>; psx_level::BOX_PROP_FACE_COUNT],
    /// Per-face material blend codes using `model_override_blend` values.
    pub blend_modes: [u8; psx_level::BOX_PROP_FACE_COUNT],
    /// Final per-face PS1 UV coordinates in face perimeter order.
    pub uvs: [[(u8, u8); 4]; psx_level::BOX_PROP_FACE_COUNT],
    /// Bottom-center room-local X.
    pub x: i32,
    /// Bottom Y.
    pub y: i32,
    /// Bottom-center room-local Z.
    pub z: i32,
    /// Room-floor Y directly beneath the prop (the floor-anchored
    /// height), baked so fragments and falling boxes settle on the
    /// ground rather than the prop's own elevated bottom.
    pub ground_y: i32,
    /// Static pitch, PSX angle units.
    pub pitch: i16,
    /// Static yaw, PSX angle units.
    pub yaw: i16,
    /// Static roll, PSX angle units.
    pub roll: i16,
    /// Editable local vertices, bottom ring then top ring.
    pub vertices: [[i16; 3]; psx_level::BOX_PROP_VERTEX_COUNT],
    /// Conservative room-local collision AABB minimum after authored resize
    /// and rotation. The guest consumes these cooked bounds directly.
    pub collision_min: [i32; 3],
    /// Conservative room-local collision AABB maximum.
    pub collision_max: [i32; 3],
    /// First generated quad in [`PlaytestPackage::box_prop_surfaces`].
    pub surface_first: u16,
    /// Number of generated erosion quads. Zero selects legacy cage faces.
    pub surface_count: u16,
    /// Material modulation tint per face.
    pub tint_rgb: [[u8; 3]; psx_level::BOX_PROP_FACE_COUNT],
    /// Baked static light base per face vertex.
    pub baked_vertex_rgb: [[(u8, u8, u8); 4]; psx_level::BOX_PROP_FACE_COUNT],
    /// Runtime flags.
    pub flags: u16,
}

/// One generated BoxProp quad baked into room-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestBoxPropSurface {
    pub vertices: [[i32; 3]; 4],
    pub center: [i32; 3],
    pub normal: [i32; 3],
    pub uv_q8: [[u8; 2]; 4],
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    pub source_face: u8,
    pub flags: u8,
}

/// One cooked low-poly procedural radial prop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCylinderProp {
    pub room: u16,
    pub texture_asset_indices: [Option<usize>; psx_level::CYLINDER_PROP_MATERIAL_COUNT],
    pub blend_modes: [u8; psx_level::CYLINDER_PROP_MATERIAL_COUNT],
    pub uvs: [[(u8, u8); 4]; psx_level::CYLINDER_PROP_MATERIAL_COUNT],
    pub tint_rgb: [[u8; 3]; psx_level::CYLINDER_PROP_MATERIAL_COUNT],
    pub surface_first: u16,
    pub surface_count: u16,
    pub center: [i32; 3],
    pub cull_radius: i32,
    pub bounds_min: [i32; 3],
    pub bounds_max: [i32; 3],
    pub flags: u16,
}

/// One generated CylinderProp triangle or quad in room-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCylinderPropSurface {
    pub vertices: [[i32; 3]; 4],
    pub center: [i32; 3],
    pub normal: [i32; 3],
    /// Final cooked GPU UV coordinates; the field name is retained for
    /// manifest/schema compatibility.
    pub uv_q8: [[u8; 2]; 4],
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    pub material_slot: u8,
    pub vertex_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestArchProp {
    pub room: u16,
    pub texture_asset_indices: [Option<usize>; psx_level::ARCH_PROP_MATERIAL_COUNT],
    pub blend_modes: [u8; psx_level::ARCH_PROP_MATERIAL_COUNT],
    pub uvs: [[(u8, u8); 4]; psx_level::ARCH_PROP_MATERIAL_COUNT],
    pub tint_rgb: [[u8; 3]; psx_level::ARCH_PROP_MATERIAL_COUNT],
    pub surface_first: u16,
    pub surface_count: u16,
    pub collision_first: u16,
    pub collision_count: u8,
    pub center: [i32; 3],
    pub cull_radius: i32,
    pub flags: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestArchPropSurface {
    pub vertices: [[i32; 3]; 4],
    pub center: [i32; 3],
    pub normal: [i32; 3],
    pub uv_q8: [[u8; 2]; 4],
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    pub material_slot: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestArchPropCollision {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

/// Cooked button action, ready for manifest emission. Mirrors
/// [`psx_level::LevelUiAction`]: the authored `GotoScene(UiSceneId)`
/// is resolved to a cooked [`PlaytestUiScene::id`] at cook time, and
/// the option/game ids are carried as compact integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaytestUiAction {
    /// Switch to the cooked composed scene state with this id.
    GotoState {
        /// Target [`PlaytestSceneState::id`].
        state: u16,
    },
    /// Switch to a cooked composed scene state with a full-screen transition.
    TransitionToState {
        /// Target [`PlaytestSceneState::id`].
        state: u16,
        /// Transition effect.
        transition: PlaytestTransition,
    },
    /// Switch to the cooked UI scene with this id.
    GotoScene {
        /// Target [`PlaytestUiScene::id`].
        scene: u16,
    },
    /// Switch to a cooked UI scene with a full-screen transition.
    TransitionToScene {
        /// Target [`PlaytestUiScene::id`].
        scene: u16,
        /// Transition effect.
        transition: PlaytestTransition,
    },
    /// Enter the gameplay/level simulation.
    StartGameplay,
    /// Enter gameplay with a full-screen transition.
    StartGameplayTransition {
        /// Transition effect.
        transition: PlaytestTransition,
    },
    /// Return to the previous menu/scene.
    #[default]
    Back,
    /// Adjust a project option by a signed delta.
    SetOption {
        /// Target option id.
        option: u16,
        /// Signed step.
        delta: i32,
    },
    /// Game-specific action dispatched by opaque id.
    Game {
        /// Caller-defined id.
        id: u16,
    },
}

/// Cooked full-screen transition settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestTransition {
    /// Effect variant.
    pub kind: PlaytestTransitionKind,
    /// Duration in visual frames.
    pub frames: u16,
    /// Overlay colour.
    pub color: [u8; 3],
    /// Deterministic noise seed.
    pub seed: u16,
}

impl PlaytestTransition {
    /// No transition.
    pub const NONE: Self = Self {
        kind: PlaytestTransitionKind::None,
        frames: 0,
        color: [0, 0, 0],
        seed: 0,
    };
}

/// Cooked transition effect kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestTransitionKind {
    /// No transition.
    None,
    /// Darken toward the transition colour.
    Fade,
    /// Random block cover.
    BlockDissolve,
    /// Digital glitch break.
    GlitchBreak,
}

/// One cooked UI gradient paint. Nodes reference these by small indices
/// when one of their color roles needs something richer than a solid fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestUiPaint {
    /// Near/top/left colour.
    pub from: [u8; 3],
    /// Far/bottom/right colour.
    pub to: [u8; 3],
    /// Gradient direction.
    pub direction: UiGradientDirection,
}

/// One cooked screen-space UI node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestUiNode {
    /// Parent index in [`PlaytestPackage::ui_nodes`], or `None` for the root canvas.
    pub parent: Option<u16>,
    /// Node kind.
    pub kind: UiNodeKind,
    /// Left edge in canvas pixels.
    pub x: i16,
    /// Top edge in canvas pixels.
    pub y: i16,
    /// Width in canvas pixels.
    pub width: u16,
    /// Height in canvas pixels.
    pub height: u16,
    /// Primary colour: fill for `Rect`/`Bar`/`Button`, text tint for
    /// `Label`, track colour for `Slider`.
    pub color: [u8; 3],
    /// Secondary colour: `Bar` background or `Slider` fill.
    pub background: [u8; 3],
    /// Tertiary colour, currently the `Slider` knob.
    pub accent: [u8; 3],
    /// Optional paint override for [`Self::color`].
    pub color_paint: Option<u16>,
    /// Optional paint override for [`Self::background`].
    pub background_paint: Option<u16>,
    /// Optional paint override for [`Self::accent`].
    pub accent_paint: Option<u16>,
    /// Current value binding for `Bar`.
    pub value: UiValueBinding,
    /// Maximum value binding for `Bar`.
    pub max: UiValueBinding,
    /// Texture asset index for `Image` or a sprite-strip `Bar`, or `None`.
    pub texture_asset: Option<usize>,
    /// Animated image vertex-colour effect preset.
    pub image_effect: crate::UiImageEffect,
    /// Text for `Label`/`Button`.
    pub text: String,
    /// Runtime lookup tag for dynamic labels. Empty means untagged.
    pub tag: String,
    /// Action fired by a `Button`. Ignored by other kinds.
    pub action: PlaytestUiAction,
    /// Project option a `Slider` binds to; sprite-strip frame count for a
    /// textured `Bar`; otherwise [`psx_level::UI_OPTION_NONE`].
    pub option: u16,
    /// Clockwise visual rotation around this node's centre, in degrees.
    pub rotation_degrees: i16,
    /// Runtime flags.
    pub flags: u16,
    /// First index into [`PlaytestPackage::ui_sfx_cues`], or
    /// [`psx_level::UI_SFX_NONE`] when the node has no SFX.
    pub sfx_first: u16,
    /// Number of SFX cues belonging to this node.
    pub sfx_count: u8,
    /// Authored font choice index for text-bearing nodes. The manifest writer
    /// compacts this into the runtime font-table selector.
    pub font: u8,
    /// Q8 font scale for text-bearing nodes (`256` = 1.0x).
    pub font_scale: u16,
    /// Extra signed screen pixels inserted between adjacent glyphs.
    pub letter_spacing: i8,
}

/// One cooked UI SFX sample. Written to `generated/ui_sfx/` and
/// referenced by [`PlaytestUiSfxCue::sample`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestUiSfxSample {
    /// Cooked `.psau` bytes.
    pub bytes: Vec<u8>,
    /// Filename inside the generated UI SFX directory.
    pub filename: String,
    /// Source WAV path for diagnostics.
    pub source_path: String,
}

/// One cooked SFX cue owned by a UI node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestUiSfxCue {
    /// Index into [`PlaytestPackage::ui_sfx_samples`].
    pub sample: u16,
    /// Runtime event that triggers this cue.
    pub event: psx_level::LevelUiSfxEvent,
    /// Per-play volume as a percentage of full voice volume.
    pub volume_percent: u8,
    /// Pitch multiplier in Q12 (`4096` = source pitch).
    pub pitch_q12: u16,
    /// Reserved runtime flags.
    pub flags: u16,
}

/// One cooked UI scene addressing a contiguous block of
/// [`PlaytestPackage::ui_nodes`]. Mirrors
/// [`psx_level::LevelUiScene`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestUiScene {
    /// Stable authored scene id.
    pub id: u16,
    /// Display name.
    pub name: String,
    /// First node index into [`PlaytestPackage::ui_nodes`].
    pub node_first: u16,
    /// Number of nodes belonging to this scene.
    pub node_count: u16,
    /// Authored focus-ring style, copied through to the cooked scene.
    pub focus_style: crate::ui_types::UiFocusStyle,
}

/// One WAV source assigned to a cooked CD-DA track number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestCddaTrack {
    /// 1-based disc track number. Track 1 is the data track, so CD-DA starts
    /// at track 2.
    pub track: u8,
    /// Source WAV path used to cook the generated raw CD-DA payload.
    pub wav_path: String,
    /// Baked playback-speed multiplier in Q12 (`4096` = 1.0x).
    pub playback_speed_q12: u16,
}

/// One cooked project option, ready for manifest emission. Mirrors
/// [`psx_level::LevelOptionDef`]: the authored [`crate::OptionKind`] is
/// flattened to a bounded integer triple at cook time (an enum becomes
/// `[0, variants - 1]` step `1`, a bool becomes `[0, 1]` step `1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestOption {
    /// Stable option id, low 16 bits of the authored [`crate::OptionId`].
    pub id: u16,
    /// Inclusive minimum value.
    pub min: i32,
    /// Inclusive maximum value.
    pub max: i32,
    /// Step applied per slider nudge.
    pub step: i32,
    /// Initial runtime value.
    pub default: i32,
}

/// Runtime world layer attached to a composed scene state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestWorldLayer {
    /// No 3D/gameplay world layer.
    None,
    /// Project gameplay world layer.
    Gameplay,
}

/// One composed runtime scene state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestSceneState {
    /// Stable cooked state id.
    pub id: u16,
    /// Display/debug name.
    pub name: String,
    /// Optional world layer.
    pub world: PlaytestWorldLayer,
    /// Optional cooked UI scene id, or [`psx_level::UI_SCENE_NONE`].
    pub ui_scene: u16,
    /// Runtime flags from [`psx_level::scene_state_flags`].
    pub flags: u16,
    /// Cooked START target state id, or [`psx_level::SCENE_STATE_NONE`].
    pub start_state: u16,
}

impl PlaytestSceneState {
    /// Built-in gameplay-only state used by projects with no authored
    /// frontend and as the final "Play" target for menu-driven projects.
    pub fn gameplay() -> Self {
        Self {
            id: 0,
            name: "Gameplay".to_string(),
            world: PlaytestWorldLayer::Gameplay,
            ui_scene: psx_level::UI_SCENE_NONE,
            flags: 0,
            start_state: psx_level::SCENE_STATE_NONE,
        }
    }
}

/// One cooked game-flow state. Mirrors [`psx_level::FlowState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestFlowState {
    /// Enter the cooked composed scene state with this id.
    SceneState {
        /// Target [`PlaytestSceneState::id`].
        state: u16,
    },
    /// Show a UI scene by its [`PlaytestUiScene::id`].
    UiScene {
        /// Target scene id.
        scene: u16,
    },
    /// Run the gameplay/level simulation.
    Gameplay,
}

/// Cooked game-state flow. Mirrors [`psx_level::GameFlow`]: an
/// addressable state table plus the entry index into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestGameFlow {
    /// Flow state table.
    pub states: Vec<PlaytestFlowState>,
    /// Composed scene states referenced by [`Self::states`].
    pub scene_states: Vec<PlaytestSceneState>,
    /// Index into `states` of the starting state.
    pub entry: u16,
}

impl Default for PlaytestGameFlow {
    /// A project with no authored UI scenes starts straight in
    /// gameplay.
    fn default() -> Self {
        Self {
            states: vec![PlaytestFlowState::SceneState { state: 0 }],
            scene_states: vec![PlaytestSceneState::gameplay()],
            entry: 0,
        }
    }
}

/// Compact rig-attached combat capsule ready for manifest emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCombatCapsule {
    pub joint: u8,
    pub flags: u8,
    pub action: u8,
    pub start: [i16; 3],
    pub end: [i16; 3],
    pub radius: u16,
    pub active_start_frame: u16,
    pub active_end_frame: u16,
    pub damage: u16,
    pub poise_damage: u16,
    pub projectile_speed: u16,
    pub projectile_lifetime_ticks: u16,
    pub projectile_min_range: u16,
    pub projectile_max_range: u16,
    pub projectile_tint_rgb: [u8; 3],
}

/// Weapon-local hit shape, ready for manifest emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestWeaponHitShape {
    /// Box hit volume.
    Box {
        /// Local center.
        center: [i32; 3],
        /// Half extents.
        half_extents: [u16; 3],
    },
    /// Capsule hit volume.
    Capsule {
        /// Local start.
        start: [i32; 3],
        /// Local end.
        end: [i32; 3],
        /// Radius.
        radius: u16,
    },
}

/// One weapon hitbox and active animation-frame window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestWeaponHitbox {
    /// Display name.
    pub name: String,
    /// Local shape.
    pub shape: PlaytestWeaponHitShape,
    /// First active frame.
    pub active_start_frame: u16,
    /// Last active frame.
    pub active_end_frame: u16,
}

/// Cooked weapon resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestWeapon {
    /// Display name.
    pub name: String,
    /// Source resource id.
    pub source_resource: ResourceId,
    /// Optional visual model index.
    pub model: Option<u16>,
    /// Character socket this weapon expects by default.
    pub default_character_socket: String,
    /// Weapon-local grip/pivot name.
    pub grip_name: String,
    /// Weapon-local grip translation.
    pub grip_translation: [i32; 3],
    /// Weapon-local grip rotation, Q12 turns.
    pub grip_rotation_q12: [i16; 3],
    /// First index in [`PlaytestPackage::weapon_hitboxes`].
    pub hitbox_first: u16,
    /// Number of hitboxes.
    pub hitbox_count: u16,
    /// Melee arc reach from the wielder's origin, engine units.
    pub arc_reach: u16,
    /// Melee arc half-width, PSX angle units (cooked from authored
    /// degrees: `deg * 4096 / 360`).
    pub arc_half_angle: u16,
    /// Damage per light-attack connection.
    pub damage: u16,
    /// Poise damage per light-attack connection.
    pub poise_damage: u16,
}

/// Cooked Equipment component on an Entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestEquipment {
    /// Owning room.
    pub room: u16,
    /// Weapon index.
    pub weapon: u16,
    /// Parent entity room-local X.
    pub x: i32,
    /// Parent entity room-local Y.
    pub y: i32,
    /// Parent entity room-local Z.
    pub z: i32,
    /// Parent entity yaw.
    pub yaw: i16,
    /// Character socket to follow.
    pub character_socket: String,
    /// Host entity's model-instance index, or `u16::MAX` for the
    /// player (no bound instance).
    pub model_instance: u16,
    /// Weapon grip/pivot to align.
    pub weapon_grip: String,
    /// Runtime flags. Bit 0 = follows the live player controller.
    pub flags: u16,
}

/// Cooked weapon-visibility beat matching one character action and one
/// equipped weapon/socket pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestWeaponAppearance {
    pub character: u16,
    pub action: crate::CharacterAnimationAction,
    pub weapon: u16,
    pub character_socket: String,
    pub fully_visible_frame: u16,
    pub hidden_frame: u16,
    pub transition_frames: u16,
}

/// One placed point light, room-local engine units. Mirrors
/// [`psx_level::PointLightRecord`] one-for-one -- intensity is
/// already quantised to Q8.8 so the cook output is a direct
/// copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestLight {
    /// Room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Cutoff distance in engine units. Cooker rejects `0`.
    pub radius: u16,
    /// Brightness multiplier in Q8.8 (`256` = 1.0). Derived
    /// from the editor's `f32` intensity at cook time.
    pub intensity_q8: u16,
    /// 8-bit RGB tint.
    pub color: [u8; 3],
}

/// One placed point-projected particle emitter, room-local engine
/// units. Mirrors [`psx_level::ParticleEmitterRecord`] so the
/// generated manifest can copy fields directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestParticleEmitter {
    /// Room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Hard live-particle cap.
    pub max_particles: u16,
    /// Spawn rate in particles/second, Q8 fixed point.
    pub spawn_rate_q8: u16,
    /// Particle lifetime in 60 Hz frames.
    pub lifetime_frames: u8,
    /// Particle size at birth, in engine units before projection.
    pub start_size: u16,
    /// Particle size at death, in engine units before projection.
    pub end_size: u16,
    /// 8-bit RGB tint at birth.
    pub start_color: [u8; 3],
    /// 8-bit RGB tint at death.
    pub end_color: [u8; 3],
    /// PS1 semi-transparency mode code: 0 average, 1 add, 2 subtract, 3 add-quarter.
    pub blend_mode: u8,
    /// Base velocity in Q4.4 engine units per 60 Hz frame.
    pub base_velocity_q4: [i16; 3],
    /// Random velocity spread in Q4.4 engine units per 60 Hz frame.
    pub random_velocity_q4: [u16; 3],
    /// Constant acceleration in Q4.4 engine units per 60 Hz frame.
    pub acceleration_q4: [i16; 3],
    /// Random spawn offset radius, in engine units.
    pub spawn_radius: u16,
    /// Runtime flags. Bit 0 = enabled.
    pub flags: u16,
}

/// Cooked interaction behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestInteractableKind {
    /// Show a message overlay.
    Message,
    /// Update the in-memory checkpoint.
    Checkpoint,
}

/// Text payload used by an interactable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestInteractableMessage {
    /// Header/title line.
    pub title: String,
    /// Body text.
    pub body: String,
}

/// One placed gameplay interaction, room-local engine units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestInteractable {
    /// Owning room.
    pub room: u16,
    /// Runtime behavior.
    pub kind: PlaytestInteractableKind,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Yaw, PSX angle units.
    pub yaw: i16,
    /// Interaction radius in XZ engine units.
    pub radius: u16,
    /// Prompt shown while in range.
    pub prompt: String,
    /// Index into [`PlaytestPackage::interactable_messages`], or
    /// [`psx_level::INTERACTABLE_MESSAGE_NONE`].
    pub message: u16,
    /// Index of the paired record in [`PlaytestPackage::logic`] (the
    /// cook emits both from one authored component).
    pub logic: u16,
    /// Stable authored checkpoint id. Empty for message-only records.
    pub checkpoint_id: String,
    /// Runtime flags from [`psx_level::interactable_flags`].
    pub flags: u16,
}

/// One cooked logic entity (hl-psx `LogicEnt`-shaped). Mirrors
/// `psx_level::LevelLogicRecord`; see that type for field semantics.
/// All names are already interned to u16 ids -- the strings die here,
/// on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestLogic {
    /// Owning room.
    pub room: u16,
    /// Behavior selector from `psx_level::logic_kind`.
    pub kind: u8,
    /// Kind-specific authored flag bits.
    pub spawnflags: u16,
    /// Interned record name, or `psx_level::LOGIC_NAME_NONE`.
    pub targetname: u16,
    /// Interned fire target, or `psx_level::LOGIC_NAME_NONE`.
    pub target: u16,
    /// Interned kill target, or `psx_level::LOGIC_NAME_NONE`.
    pub killtarget: u16,
    /// Interned multisource gate, or `psx_level::LOGIC_NAME_NONE`.
    pub master: u16,
    /// 60 Hz ticks between triggering and firing `target`.
    pub delay_ticks: u16,
    /// Re-arm delay in 60 Hz ticks; negative = fire once.
    pub wait_ticks: i16,
    /// First kind-specific argument.
    pub arg0: u16,
    /// Second kind-specific argument.
    pub arg1: u16,
    /// Kind-defined entity link, or `psx_level::LOGIC_LINK_NONE`.
    pub link: u16,
    /// Index into [`PlaytestPackage::interactable_messages`], or
    /// `psx_level::INTERACTABLE_MESSAGE_NONE`.
    pub message: u16,
    /// Room-local origin X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local origin Z.
    pub z: i32,
    /// Trigger AABB minimum corner, room-local.
    pub min: [i32; 3],
    /// Trigger AABB maximum corner.
    pub max: [i32; 3],
    /// Runtime flags from `psx_level::logic_flags`.
    pub flags: u16,
}

/// One placed souls-like game entity. Mirrors
/// `psx_level::LevelGameEntityRecord`; see that type for semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestGameEntity {
    /// Owning room.
    pub room: u16,
    /// Interned archetype tag (the Character resource name).
    pub kind: u16,
    /// Interned name logic can target, or `psx_level::LOGIC_NAME_NONE`.
    pub targetname: u16,
    /// Index into [`PlaytestPackage::model_instances`], or
    /// `psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE`.
    pub model_instance: u16,
    /// Model-local clip index for the Idle state, resolved at cook
    /// from the Character's AnimationSet roles (like all state
    /// clips; missing roles already fell back at cook).
    pub idle_clip: u16,
    /// Initial acquisition one-shot (Intro/activation action).
    pub alert_clip: u16,
    /// In-place player-tracking turn loop.
    pub turn_clip: u16,
    /// Patrol (walk role) clip.
    pub walk_clip: u16,
    /// Retreat (walk-backward action) clip.
    pub walk_backward_clip: u16,
    /// Circle-left (strafe-left action) clip.
    pub strafe_left_clip: u16,
    /// Circle-right (strafe-right action) clip.
    pub strafe_right_clip: u16,
    /// Aggro (run role) clip.
    pub run_clip: u16,
    /// Windup/Attack/Recover one-shot clip.
    pub attack_clip: u16,
    /// Staggered (hit-react role) one-shot clip.
    pub stagger_clip: u16,
    /// Death one-shot clip.
    pub death_clip: u16,
    /// First rig-attached volume in [`PlaytestPackage::combat_capsules`].
    pub combat_capsule_first: u16,
    /// Number of rig-attached volumes.
    pub combat_capsule_count: u8,
    /// Room-local spawn X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local spawn Z.
    pub z: i32,
    /// Spawn yaw, PSX angle units.
    pub yaw: i16,
    /// Body cylinder radius, engine units (Character-bound).
    pub radius: u16,
    /// Body cylinder height, engine units (Character-bound).
    pub height: u16,
    /// Patrol speed, engine units per 60 Hz tick (Character walk).
    pub walk_speed: i32,
    /// Chase speed, engine units per 60 Hz tick (Character run).
    pub run_speed: i32,
    /// Patrol anchor one, room-local (== spawn when unauthored).
    pub patrol: [i32; 3],
    /// 60 Hz ticks idled at a reached patrol anchor.
    pub patrol_wait_ticks: u16,
    /// XZ aggro radius in engine units.
    pub aggro_radius: u16,
    /// Initial combat reaction delay in 60 Hz ticks.
    pub reaction_ticks: u8,
    /// Desired non-attacker distance in engine units.
    pub preferred_distance: u16,
    /// Half-width of the desired-distance band.
    pub spacing_tolerance: u16,
    /// Hold/circle decision cadence in 60 Hz ticks.
    pub decision_interval_ticks: u8,
    /// Percent chance that an in-band decision circles.
    pub circle_chance: u8,
    /// Relative combat-director attack priority.
    pub attack_priority: u8,
    /// Local post-attack cooldown in 60 Hz ticks.
    pub attack_cooldown_ticks: u8,
    /// Shared director delay after this entity attacks.
    pub group_attack_delay_ticks: u8,
    /// Attack windup ticks.
    pub windup_ticks: u8,
    /// Post-attack recovery ticks.
    pub recovery_ticks: u8,
    /// Closest ranged-attack distance. Zero for melee entities.
    pub attack_min_range: u16,
    /// Furthest ranged-attack distance. Zero for melee entities.
    pub attack_max_range: u16,
    /// Poise pool.
    pub poise: u16,
    /// Touch/melee damage.
    pub touch_damage: u16,
    /// Health pool at spawn.
    pub max_health: u16,
    /// Runtime flags from `psx_level::game_entity_flags`.
    pub flags: u16,
}

/// Player spawn record. Coordinates are room-local engine units
/// (the same space the cooked `.psxw` lives in -- array-rooted at
/// world `(0, 0)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestSpawn {
    /// Room index in [`PlaytestPackage::rooms`].
    pub room: u16,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Yaw in PSX angle units.
    pub yaw: i16,
    /// Reserved flags. Bit 0 = "spawn enabled".
    pub flags: u16,
}

/// Coarse runtime kind for [`PlaytestEntity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestEntityKind {
    /// Visual marker (debug cube).
    Marker,
    /// Static mesh instance pinned by `resource_slot`.
    StaticMesh,
}

/// Cooked character record. Mirrors
/// [`psx_level::LevelCharacterRecord`] one-to-one -- the writer
/// emits the static slice from this struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestCharacter {
    /// Source resource id (so the writer can dedupe + cross-link).
    pub source_resource: ResourceId,
    /// Index into [`PlaytestPackage::models`].
    pub model: u16,
    /// Optional clip index per [`crate::CharacterAnimationAction`]
    /// slot, within the model's runtime clip slice.
    pub action_clips: [u16; PLAYTEST_CHARACTER_ACTION_COUNT],
    /// Per-action playback flags matching [`Self::action_clips`].
    pub action_flags: [u8; PLAYTEST_CHARACTER_ACTION_COUNT],
    /// Per-action playback speed in Q8 fixed point (`256 = 1.0x`),
    /// matching [`Self::action_clips`].
    pub action_speeds: [u16; PLAYTEST_CHARACTER_ACTION_COUNT],
    /// Inclusive playback frame window per action.
    pub action_frame_ranges:
        [psx_level::CharacterActionFrameRange; PLAYTEST_CHARACTER_ACTION_COUNT],
    /// Forward push per action.
    pub action_pushes: [psx_level::CharacterActionPush; PLAYTEST_CHARACTER_ACTION_COUNT],
    /// First rig-attached volume in [`PlaytestPackage::combat_capsules`].
    pub combat_capsule_first: u16,
    /// Number of rig-attached volumes.
    pub combat_capsule_count: u8,
    /// Render-only model offset from the player/controller root,
    /// in entity-local engine units.
    pub visual_offset: [i16; 3],
    /// Render-only yaw from the Model Renderer component, PSX angle units.
    pub visual_yaw: i16,
    /// Render-only uniform scale in Q8 fixed point (`256 = 1.0`).
    pub visual_scale_q8: u16,
    /// Gravity multiplier in Q8 fixed point (`256 = 1.0x`).
    pub weight_q8: u16,
    /// Capsule radius in engine units.
    pub radius: u16,
    /// Capsule height in engine units.
    pub height: u16,
    /// Walk speed (engine units / 60 Hz frame).
    pub walk_speed: i32,
    /// Run speed (engine units / 60 Hz frame).
    pub run_speed: i32,
    /// Turn speed (degrees / second).
    pub turn_speed_degrees_per_second: u16,
    /// Maximum stamina in Q12-style arbitrary units.
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    pub sprint_min_q12: i32,
    /// Stamina spent per sprinting 60 Hz frame.
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint 60 Hz frame.
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    pub roll_cost_q12: i32,
    /// Roll travel speed in engine units per 60 Hz frame.
    pub roll_speed: i32,
    /// Frames where the roll keeps moving.
    pub roll_active_frames: u8,
    /// Recovery frames after roll movement ends.
    pub roll_recovery_frames: u8,
    /// Invulnerable frames from roll start.
    pub roll_invulnerable_frames: u8,
    /// Legacy quickstep stamina cost retained for package compatibility.
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in engine units per 60 Hz frame.
    pub backstep_speed: i32,
    /// Legacy quickstep active movement frames.
    pub backstep_active_frames: u8,
    /// Legacy quickstep recovery frames.
    pub backstep_recovery_frames: u8,
    /// Legacy quickstep invulnerability frames.
    pub backstep_invulnerable_frames: u8,
    /// Camera follow distance (engine units).
    pub camera_distance: i32,
    /// Camera vertical offset above the character origin.
    pub camera_height: i32,
    /// Vertical offset of the camera's look-at target.
    pub camera_target_height: i32,
    /// Covering material replacing the model's cooked atlas, or
    /// `None` to render the atlas (the default path).
    pub material_override: Option<PlaytestModelMaterialOverride>,
}

/// Cooked player-controller record. Always paired with a
/// [`PlaytestSpawn`] in the same package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestPlayerController {
    /// Resolved spawn -- same data the manifest's `PLAYER_SPAWN`
    /// carries.
    pub spawn: PlaytestSpawn,
    /// Character index in [`PlaytestPackage::characters`].
    pub character: u16,
}

/// Sentinel used in [`PlaytestCharacter::run_clip`] /
/// [`PlaytestCharacter::turn_clip`] when the role wasn't
/// authored. Mirrors [`psx_level::CHARACTER_CLIP_NONE`].
pub const CHARACTER_CLIP_NONE: u16 = u16::MAX;

/// One non-spawn entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaytestEntity {
    /// Owning room index.
    pub room: u16,
    /// Entity kind.
    pub kind: PlaytestEntityKind,
    /// Room-local X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Room-local Z.
    pub z: i32,
    /// Yaw, PSX angle units.
    pub yaw: i16,
    /// Resource slot (0 if unused).
    pub resource_slot: u16,
    /// Reserved flags.
    pub flags: u16,
}

/// Cooked playtest scene, ready to write to disk. Holds
/// everything the generated manifest needs to render: assets,
/// per-room metadata, per-room material slices, models, model
/// instances, and residency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaytestPackage {
    /// Project-authoritative BSP compiler quality used for this package.
    pub bsp_cook_mode: crate::brush_world::BrushWorldCookMode,
    /// Static-world provider used by the normal Play lifecycle.
    pub world_geometry: PlaytestWorldGeometry,
    /// Project-relative paths of every source texture the cook actually
    /// reached, deduplicated.
    ///
    /// A project's `assets/` directory is a library, not a working set:
    /// cortex_v5 carries 704 `.psxt` files and its level reaches 3. This is the
    /// reachable set, taken from the same map the cook uses to dedupe texture
    /// references, so it cannot disagree with what was actually cooked. Used to
    /// produce a shippable copy of a project containing only what it needs, and
    /// as an orphaned-asset report.
    pub used_texture_paths: Vec<String>,
    /// Project-relative paths of every source model mesh and animation clip the
    /// cook actually reached. Same purpose and same guarantee as
    /// [`Self::used_texture_paths`]: taken from the maps the cook itself keeps,
    /// so it cannot name a file the cook did not use, nor miss one it did.
    pub used_model_paths: Vec<String>,
    /// Project-global UI source files: CD-DA and SFX `.wav`s plus UI image
    /// textures. Not reachable from any room, so they need copying wholesale
    /// rather than by reachability.
    pub used_ui_paths: Vec<String>,
    /// Cached-room depth sorting mode selected by the project.
    pub runtime_depth_sort_mode: RuntimeDepthSortMode,
    /// Runtime room triangle subdivision scope.
    pub runtime_texture_split_mode: RuntimeTextureSplitMode,
    /// Runtime active-room draw ordering policy.
    pub runtime_room_draw_order_mode: RuntimeRoomDrawOrderMode,
    /// Projected edge threshold for runtime room surface subdivision.
    pub runtime_texture_split_max_edge: u16,
    /// Master asset table -- rooms first, then room textures,
    /// then per-model assets (mesh + atlas + clips), in
    /// deterministic order.
    pub assets: Vec<PlaytestAsset>,
    /// Cooked rooms with material-slice metadata.
    pub rooms: Vec<PlaytestRoom>,
    /// Runtime chunk metadata, one record per cooked room.
    pub chunks: Vec<PlaytestChunk>,
    /// Directed runtime room portal graph.
    pub room_portals: Vec<PlaytestRoomPortal>,
    /// Runtime floor links, indexed by `(room, x, z)` and copied into streamed collision chunks.
    pub room_floor_links: Vec<PlaytestRoomFloorLink>,
    /// Sorted water-sector lookup table.
    pub water_cells: Vec<PlaytestWaterCell>,
    /// Reserved near-room index table for room coherence / streaming.
    pub room_near_rooms: Vec<u16>,
    /// Reserved overlapped-room index table for stacked-room coherence.
    pub room_overlapped_rooms: Vec<u16>,
    /// Material records ordered as `(room, local_slot)`.
    pub materials: Vec<PlaytestMaterial>,
    /// Per-room visibility slices.
    pub room_visibility: Vec<PlaytestRoomVisibility>,
    /// Per-cell visibility metadata.
    pub visibility_cells: Vec<PlaytestVisibilityCell>,
    /// Per-visibility-cell PVS bitset slices.
    pub visibility_pvs: Vec<PlaytestVisibilityPvs>,
    /// Flattened PVS bitset bytes.
    pub visibility_pvs_bits: Vec<u8>,
    /// Per-room room-surface cache slices.
    pub room_surface_caches: Vec<PlaytestRoomSurfaceCache>,
    /// Flattened cached room cell records.
    pub room_cache_cells: Vec<PlaytestCachedRoomCell>,
    /// Flattened room-local vertex-index lists per cached cell.
    pub room_cache_cell_vertices: Vec<u16>,
    /// Flattened cached room vertex records.
    pub room_cache_vertices: Vec<PlaytestCachedRoomVertex>,
    /// Flattened cached room surface records.
    pub room_cache_surfaces: Vec<PlaytestCachedRoomSurface>,
    /// Cooked model bundles, deduplicated across instances.
    pub models: Vec<PlaytestModel>,
    /// Per-model clip records ordered as `(model, clip_index)`.
    pub model_clips: Vec<PlaytestModelClip>,
    /// Per-global-clip bounds slices.
    pub model_clip_bounds: Vec<PlaytestModelClipBounds>,
    /// Flattened per-frame model bounds.
    pub model_frame_bounds: Vec<PlaytestModelFrameBounds>,
    /// Per-model socket records ordered as `(model, socket_index)`.
    pub model_sockets: Vec<PlaytestModelSocket>,
    /// Placed model instances, room-local coordinates.
    pub model_instances: Vec<PlaytestModelInstance>,
    /// Placed flat image props, room-local coordinates.
    pub image_props: Vec<PlaytestImageProp>,
    /// Placed editable box props, room-local coordinates.
    pub box_props: Vec<PlaytestBoxProp>,
    /// Generated directional-erosion surfaces sliced by [`Self::box_props`].
    pub box_prop_surfaces: Vec<PlaytestBoxPropSurface>,
    /// Placed low-poly procedural radial props.
    pub cylinder_props: Vec<PlaytestCylinderProp>,
    /// Generated surfaces sliced by [`Self::cylinder_props`].
    pub cylinder_prop_surfaces: Vec<PlaytestCylinderPropSurface>,
    /// Placed tile-native arches.
    pub arch_props: Vec<PlaytestArchProp>,
    /// Generated quads sliced by [`Self::arch_props`].
    pub arch_prop_surfaces: Vec<PlaytestArchPropSurface>,
    /// Conservative curved-band collision boxes sliced by [`Self::arch_props`].
    pub arch_prop_collisions: Vec<PlaytestArchPropCollision>,
    /// Cooked screen-space UI nodes for every scene, concatenated
    /// into one shared pool. [`Self::ui_scenes`] slices this pool.
    pub ui_nodes: Vec<PlaytestUiNode>,
    /// Cooked UI gradient paints referenced by [`Self::ui_nodes`].
    pub ui_paints: Vec<PlaytestUiPaint>,
    /// Addressable cooked UI scene table indexing [`Self::ui_nodes`].
    pub ui_scenes: Vec<PlaytestUiScene>,
    /// Cooked UI SFX samples, deduplicated by source WAV path.
    pub ui_sfx_samples: Vec<PlaytestUiSfxSample>,
    /// Cooked UI SFX cues, sliced by [`PlaytestUiNode::sfx_first`] /
    /// [`PlaytestUiNode::sfx_count`].
    pub ui_sfx_cues: Vec<PlaytestUiSfxCue>,
    /// Cooked game-state flow definition.
    pub game_flow: PlaytestGameFlow,
    /// Cooked project options, flattened to bounded integer ranges.
    /// Sliders and `SetOption` actions reference these by id.
    pub options: Vec<PlaytestOption>,
    /// WAV sources baked as CD-DA tracks in the playtest/export disc image.
    pub cdda_tracks: Vec<PlaytestCddaTrack>,
    /// Compact rig-attached hurtboxes and action-gated hitboxes.
    pub combat_capsules: Vec<PlaytestCombatCapsule>,
    /// Weapon hitboxes, shared by [`Self::weapons`].
    pub weapon_hitboxes: Vec<PlaytestWeaponHitbox>,
    /// Cooked Weapon resources, deduplicated by source resource id.
    pub weapons: Vec<PlaytestWeapon>,
    /// Equipment components placed in rooms.
    pub equipment: Vec<PlaytestEquipment>,
    /// Animation-authored visibility beats for equipped weapons.
    pub weapon_appearances: Vec<PlaytestWeaponAppearance>,
    /// Placed point lights, room-local coordinates.
    pub lights: Vec<PlaytestLight>,
    /// Placed point-projected particle emitters.
    pub particle_emitters: Vec<PlaytestParticleEmitter>,
    /// Text payloads referenced by placed interactables.
    pub interactable_messages: Vec<PlaytestInteractableMessage>,
    /// Placed gameplay interactables.
    pub interactables: Vec<PlaytestInteractable>,
    /// Cooked logic entities (phase-3 event graph). Interactables
    /// emit one of these alongside their `PlaytestInteractable` so
    /// the graph can reference them; trigger/relay/door authoring
    /// nodes land with the next editor slice.
    pub logic: Vec<PlaytestLogic>,
    /// Placed souls-like game entities (enemies).
    pub game_entities: Vec<PlaytestGameEntity>,
    /// Single player spawn -- required.
    pub spawn: Option<PlaytestSpawn>,
    /// Cooked Character resources used by player / future
    /// gameplay. Currently only the player spawn references
    /// these, but the slice ships in the manifest unconditionally
    /// so the runtime can table-drive any future controllers.
    pub characters: Vec<PlaytestCharacter>,
    /// Resolved player controller -- `Some` when a player spawn
    /// was authored *and* a Character was assigned (or
    /// auto-picked). The runtime falls back to a debug camera
    /// when this is `None`.
    pub player_controller: Option<PlaytestPlayerController>,
    /// Optional entity markers (legacy, non-Model MeshInstance).
    pub entities: Vec<PlaytestEntity>,
}

impl PlaytestPackage {
    /// Number of `RoomWorld` entries in [`Self::assets`].
    pub fn room_asset_count(&self) -> usize {
        self.assets
            .iter()
            .filter(|a| a.kind == PlaytestAssetKind::RoomWorld)
            .count()
    }

    /// Number of `Texture` entries in [`Self::assets`].
    pub fn texture_asset_count(&self) -> usize {
        self.assets
            .iter()
            .filter(|a| a.kind == PlaytestAssetKind::Texture)
            .count()
    }

    /// Number of `ModelMesh` entries in [`Self::assets`].
    pub fn model_mesh_asset_count(&self) -> usize {
        self.assets
            .iter()
            .filter(|a| a.kind == PlaytestAssetKind::ModelMesh)
            .count()
    }

    /// Number of `ModelAnimation` entries in [`Self::assets`].
    pub fn model_animation_asset_count(&self) -> usize {
        self.assets
            .iter()
            .filter(|a| a.kind == PlaytestAssetKind::ModelAnimation)
            .count()
    }
}

/// Cooked memory footprint for one streamed room chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytestStreamChunkMemory {
    /// Owning room/chunk id.
    pub room: u16,
    /// Number of CD sectors occupied by the sector-aligned chunk.
    pub sector_count: usize,
    /// Unpadded `.psxc` payload bytes.
    pub payload_bytes: usize,
    /// Sector-aligned stream bytes.
    pub stream_bytes: usize,
    /// Fixed chunk header bytes.
    pub header_bytes: usize,
    /// Collision payload bytes.
    pub collision_bytes: usize,
    /// Cached cell table bytes consumed by the render path.
    pub render_cell_bytes: usize,
    /// Cached vertex table bytes consumed by the render path.
    pub render_vertex_bytes: usize,
    /// Per-cell cached vertex-index bytes consumed by the render path.
    pub render_cell_vertex_bytes: usize,
    /// Cached surface table bytes consumed by the render path.
    pub render_surface_bytes: usize,
    /// Total render-cache bytes.
    pub render_cache_bytes: usize,
    /// In-payload alignment padding between sections.
    pub alignment_padding_bytes: usize,
    /// Padding at the end of the file to fill CD sectors.
    pub sector_padding_bytes: usize,
}

/// Summed memory footprint for streamed room chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytestStreamMemoryTotals {
    pub sector_count: usize,
    pub payload_bytes: usize,
    pub stream_bytes: usize,
    pub header_bytes: usize,
    pub collision_bytes: usize,
    pub render_cell_bytes: usize,
    pub render_vertex_bytes: usize,
    pub render_cell_vertex_bytes: usize,
    pub render_surface_bytes: usize,
    pub render_cache_bytes: usize,
    pub alignment_padding_bytes: usize,
    pub sector_padding_bytes: usize,
}

/// Full streamed-room memory report generated at cook time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaytestStreamMemoryReport {
    pub chunks: Vec<PlaytestStreamChunkMemory>,
    pub totals: PlaytestStreamMemoryTotals,
    pub largest_chunk: Option<PlaytestStreamChunkMemory>,
}

/// Outcome of validating a project for playtest. Errors block
/// cooking; warnings are surfaced but not fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaytestValidationTarget {
    /// Brush and optional authored face index in the active scene.
    Brush { brush: usize, face: Option<usize> },
    /// Scene-tree node responsible for the failure.
    Node(NodeId),
    /// Project resource responsible for the failure.
    Resource(ResourceId),
}

/// One hard error, with the authoring object responsible when the cook can
/// derive one. Per-error rather than per-report: the diagnostics panel lists
/// every failure, and an author who reads row four wants to jump to row four's
/// brush, not to whatever row one happened to blame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaytestValidationError {
    pub message: String,
    /// Authoring object the editor can select and frame. `None` when the
    /// failure is a whole-map limit with no single offender.
    pub target: Option<PlaytestValidationTarget>,
}

impl PlaytestValidationError {
    /// `true` when the message contains `needle`. Diagnostics assertions ask
    /// this constantly, and spelling it out keeps them reading as prose.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

impl std::fmt::Display for PlaytestValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaytestValidationReport {
    /// Hard errors. Embedded Play must refuse to launch when this
    /// list is non-empty.
    pub errors: Vec<PlaytestValidationError>,
    /// Soft warnings. Surface in the editor status line but
    /// don't block cooking.
    pub warnings: Vec<String>,
}

impl PlaytestValidationReport {
    /// `true` when there are zero hard errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// First authoring object any hard error can blame. The single-target
    /// convenience the auto-focus-after-a-failing-cook flow has always used.
    pub fn focus_target(&self) -> Option<PlaytestValidationTarget> {
        self.errors.iter().find_map(|error| error.target)
    }

    /// Just the messages, for joins, logs and `contains` assertions.
    pub fn error_messages(&self) -> Vec<&str> {
        self.errors
            .iter()
            .map(|error| error.message.as_str())
            .collect()
    }

    pub(super) fn error(&mut self, msg: impl Into<String>) {
        self.errors.push(PlaytestValidationError {
            message: msg.into(),
            target: None,
        });
    }

    pub(super) fn error_at(&mut self, target: PlaytestValidationTarget, msg: impl Into<String>) {
        self.errors.push(PlaytestValidationError {
            message: msg.into(),
            target: Some(target),
        });
    }

    /// Push an error whose target may or may not be derivable, without
    /// forcing every call site to branch.
    pub(super) fn error_maybe_at(
        &mut self,
        target: Option<PlaytestValidationTarget>,
        msg: impl Into<String>,
    ) {
        self.errors.push(PlaytestValidationError {
            message: msg.into(),
            target,
        });
    }

    /// Run `body`, then stamp `target` onto every error it pushed that does
    /// not already name one.
    ///
    /// The per-kind cook helpers only receive a node NAME, so they cannot
    /// build a `Node` target themselves, and threading a `NodeId` through
    /// every helper signature would touch far more code than it is worth.
    /// The caller loop already holds the node, so it blames it here. A helper
    /// that DID derive a more precise target (a specific resource, say) keeps
    /// it: only `None` targets are filled in.
    pub(super) fn blaming<R>(
        &mut self,
        target: PlaytestValidationTarget,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let before = self.errors.len();
        let result = body(self);
        for error in &mut self.errors[before..] {
            error.target.get_or_insert(target);
        }
        result
    }

    pub(super) fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}
