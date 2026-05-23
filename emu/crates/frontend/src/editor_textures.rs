//! Editor preview textures.
//!
//! Generates or uploads a 4bpp texture per project Material resource
//! into the editor `HwRenderer`'s VRAM. A cache keyed on `ResourceId`
//! records the resulting (tpage, CLUT) words so the editor preview can
//! emit `TriTextured` packets that sample the right region.
//!
//! Why procedural: real cooked textures from project PNGs land later
//! once the editor's asset pipeline is hooked up end-to-end. Until
//! then the preview already needs *something* in each material's
//! tpage so the texture-tint render path is exercised -- and for the
//! Sims-style build flow, "stone-vs-brick-vs-wood" patterns convey
//! material identity better than flat colours.
//!
//! VRAM layout:
//!
//! ```text
//!   y = 0      ▶ 320×240 frame buffer (sub-rect the editor paints)
//!   y = 0      ▶ tpages 5..15  -- 4bpp room material atlas pages,
//!                packed on the GP0(E2) 8-texel texture-window grid
//!                starting at x = 320
//!   y = 480    ▶ CLUT row, 16 halfwords per palette
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use psx_asset::Texture;
use psx_gpu::material::TextureWindow;
use psx_gpu_render::{VRAM_HEIGHT, VRAM_WIDTH};
use psx_vram::TextureWindowAtlas;
use psxed_project::streaming::collect_scene_resource_use;
use psxed_project::{
    MaterialResource, NodeKind, ProjectDocument, Resource, ResourceData, ResourceId,
};

const ROOM_TPAGE_HALFWORDS: usize = 64;
const ROOM_TPAGE_TEXEL_HEIGHT: usize = 256;
const ROOM_TILE_TEXELS: u16 = 64;
const ROOM_FIRST_TPAGE: u8 = 5;
const ROOM_LAST_TPAGE: u8 = 15;
const ROOM_TPAGE_COUNT: usize = (ROOM_LAST_TPAGE - ROOM_FIRST_TPAGE + 1) as usize;
const ROOM_CLUT_Y: u16 = 480;
const ROOM_CLUT_HALFWORDS: u16 = 16;
const SHADOW_TEXTURE_SIZE: usize = 64;
const SHADOW_TPAGE_INDEX: u16 = 15;
const SHADOW_TPAGE_X: u16 = SHADOW_TPAGE_INDEX * 64;
const SHADOW_TPAGE_Y_BLOCK: u16 = 1;
const SHADOW_TPAGE_Y: u16 = 256;
const SHADOW_CLUT_X: u16 = 1008;
const SHADOW_CLUT_Y: u16 = 479;

/// Cached tpage/CLUT for one Material resource.
#[derive(Debug, Clone, Copy)]
pub struct MaterialSlot {
    /// Packed `uv_tpage_word` value the prim format wants in vertex
    /// 1's UV high half.
    pub tpage_word: u16,
    /// Packed `uv_clut_word` value the prim format wants in vertex
    /// 0's UV high half.
    pub clut_word: u16,
    /// GP0(E2) texture-window state constraining UV repetition to the
    /// allocated atlas rectangle.
    pub texture_window: TextureWindow,
    /// Width of the material's texture window in texels.
    pub texture_width: u8,
    /// Height of the material's texture window in texels.
    pub texture_height: u8,
}

/// Cache row keeping the slot assigned to a material.
#[derive(Debug, Clone)]
struct CacheEntry {
    slot: MaterialSlot,
}

/// Owns the editor renderer's VRAM mirror plus the per-material
/// texture cache.
///
/// VRAM regions:
///
/// * `y = 0`,  tpages 0..4   -- framebuffer (editor paints x=0..320).
/// * `y = 0`,  tpages 5..15  -- 4bpp room material textures,
///   packed on an 8-texel texture-window grid.
/// * `y = 256`, tpage row 1   -- 8bpp model atlases, packed
///   left-to-right by halfwords. Disjoint from the room region.
/// * `y = 480`                -- 4bpp CLUTs, 16 halfwords each.
/// * `y = 481..`              -- 8bpp CLUTs, 256 halfwords each.
///
/// Each Model resource maps to one cache entry keyed by its
/// `ResourceId`; same for Material resources. The two halves of
/// the cache use disjoint VRAM regions so a model atlas upload
/// never overwrites a room material and vice versa.
pub struct EditorTextures {
    vram: Box<[u16]>,
    cache: HashMap<ResourceId, CacheEntry>,
    model_cache: HashMap<ResourceId, ModelAtlasCacheEntry>,
    shadow_slot: MaterialSlot,
    /// Ordered material ids, texture paths, and fallback names used to
    /// populate the limited room-material VRAM band. Scene-used
    /// materials and active far-vista panels are prioritized ahead
    /// of unused library resources.
    room_signature: Vec<PreviewTextureSignature>,
    /// 8-texel-grid allocator for room textures inside tpages 5..15.
    room_allocator: TextureWindowAtlas<ROOM_TPAGE_COUNT>,
    /// Halfword X-coord of the next free 4bpp CLUT slot. Each
    /// 4bpp CLUT is 16 halfwords wide.
    next_clut_x: u16,
    /// Halfword X-coord cursor inside the 8bpp model atlas tpage
    /// row at y=256. Each atlas advances this by its halfword
    /// stride.
    next_model_tpage_x: u16,
    /// Y-coord of the next free 8bpp CLUT row. Steps down by 1
    /// per uploaded atlas (256-entry CLUTs are one row each).
    next_model_clut_y: u16,
}

/// Model atlas cache entry keyed by the Model resource id and signed by
/// the atlas path so editing `texture_path` triggers a re-upload on the
/// next `refresh_models` call.
#[derive(Debug, Clone)]
struct ModelAtlasCacheEntry {
    slot: MaterialSlot,
    /// Atlas path that produced this slot; empty when the model
    /// has no atlas (no slot is uploaded in that case -- entry
    /// just records the empty signature so we don't re-walk).
    signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewTextureSignature {
    id: ResourceId,
    path: String,
    cache_signature: String,
    name: String,
    force_zero_opaque: bool,
    allow_procedural_fallback: bool,
}

impl EditorTextures {
    pub fn new() -> Self {
        let shadow_slot = MaterialSlot {
            tpage_word: pack_tpage_word(SHADOW_TPAGE_INDEX, SHADOW_TPAGE_Y_BLOCK),
            clut_word: pack_clut_word(SHADOW_CLUT_X, SHADOW_CLUT_Y),
            texture_window: TextureWindow::NONE,
            texture_width: 64,
            texture_height: 64,
        };
        let mut textures = Self {
            vram: vec![0u16; (VRAM_WIDTH * VRAM_HEIGHT) as usize].into_boxed_slice(),
            cache: HashMap::new(),
            model_cache: HashMap::new(),
            shadow_slot,
            room_signature: Vec::new(),
            room_allocator: TextureWindowAtlas::new(),
            next_clut_x: 0,
            // Model atlases live at the tpage row at y=256.
            // Cursor advances by halfword stride per atlas.
            next_model_tpage_x: 0,
            // 8bpp CLUTs sit just below the 4bpp band.
            next_model_clut_y: 481,
        };
        textures.upload_shadow_texture();
        textures
    }

    /// Borrow the VRAM array for `HwRenderer::render_frame`.
    pub fn vram_words(&self) -> &[u16] {
        &self.vram
    }

    /// Look up a material's texture slot, or `None` if it hasn't
    /// been uploaded (the editor preview should fall back to flat
    /// shading in that case).
    pub fn slot(&self, id: ResourceId) -> Option<MaterialSlot> {
        self.cache.get(&id).map(|e| e.slot)
    }

    /// Look up a Model resource's atlas slot, or `None` if no
    /// atlas has been uploaded for that model (no atlas path,
    /// or upload failed).
    pub fn model_atlas_slot(&self, id: ResourceId) -> Option<MaterialSlot> {
        self.model_cache.get(&id).map(|e| e.slot)
    }

    /// Dedicated 4bpp circular shadow texture used by the editor
    /// preview's model-grounding decals.
    pub fn shadow_slot(&self) -> MaterialSlot {
        self.shadow_slot
    }

    /// Walk scene-used Materials first, then active far-vista panels,
    /// then unused library Materials, and ensure as many as fit have
    /// textures in VRAM.
    ///
    /// Resolution order per material:
    ///
    /// 1. Follow `material.texture` to a `ResourceData::Texture`.
    /// 2. Resolve `psxt_path` (absolute as-is, otherwise against
    ///    `project_root`).
    /// 3. `fs::read` and parse via [`psx_asset::Texture::from_bytes`].
    /// 4. On any failure, fall back to a name-keyed procedural
    ///    pattern (brick / stone / wood / metal / glass / default
    ///    checker) so the preview is never blank.
    ///
    /// The editor preview's room-material region is intentionally small,
    /// so authored scene materials and far-vista panels must get slots
    /// before unused resource-library entries.
    pub fn refresh(&mut self, project: &ProjectDocument, project_root: &Path) {
        let plan = preview_texture_upload_plan(project, project_root);
        let room_signature: Vec<PreviewTextureSignature> = plan
            .iter()
            .map(|item| {
                PreviewTextureSignature {
                    id: item.id,
                    path: item.signature.clone(),
                    cache_signature: item.cache_signature.clone(),
                    name: item.name.clone(),
                    force_zero_opaque: item.force_zero_opaque,
                    allow_procedural_fallback: item.allow_procedural_fallback,
                }
            })
            .collect();
        if self.room_signature == room_signature {
            return;
        }

        self.clear_room_materials();
        self.room_signature = room_signature;

        for item in plan {
            if !self.has_room_material_slot() {
                // Out of slots. Don't insert a stale cache entry.
                break;
            }
            let slot = self
                .upload_real_psxt_with_clut_mode(
                    &item.signature,
                    project_root,
                    item.force_zero_opaque,
                )
                .or_else(|| {
                    item.allow_procedural_fallback
                        .then(|| self.upload_procedural(&item.name))
                        .flatten()
                });
            let Some(slot) = slot else {
                break;
            };
            self.cache.insert(item.id, CacheEntry { slot });
        }
    }

    fn has_room_material_slot(&self) -> bool {
        self.next_clut_x + ROOM_CLUT_HALFWORDS <= VRAM_WIDTH as u16
    }

    fn clear_room_materials(&mut self) {
        self.cache.clear();
        self.room_allocator.clear();
        self.next_clut_x = 0;

        let x0 = ROOM_FIRST_TPAGE as usize * ROOM_TPAGE_HALFWORDS;
        let x1 = (ROOM_LAST_TPAGE as usize + 1) * ROOM_TPAGE_HALFWORDS;
        for row in 0..ROOM_TPAGE_TEXEL_HEIGHT {
            let base = row * VRAM_WIDTH as usize;
            self.vram[base + x0..base + x1].fill(0);
        }

        let clut_base = ROOM_CLUT_Y as usize * VRAM_WIDTH as usize;
        self.vram[clut_base..clut_base + VRAM_WIDTH as usize].fill(0);
    }

    /// Read `path` and upload the parsed PSXT into the next free
    /// tpage / CLUT slot. Returns `None` if the path is empty, the
    /// file can't be read, the blob fails to parse, or the depth
    /// is unsupported by the editor preview path (only 4bpp + 8bpp
    /// indexed for now -- the runtime supports 15bpp but editor's
    /// procedural fallback covers any holes).
    fn upload_real_psxt_with_clut_mode(
        &mut self,
        path: &str,
        project_root: &Path,
        force_zero_opaque: bool,
    ) -> Option<MaterialSlot> {
        if path.is_empty() {
            return None;
        }
        let abs = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            project_root.join(path)
        };
        let bytes = std::fs::read(&abs).ok()?;
        let texture = Texture::from_bytes(&bytes).ok()?;
        // PSX UVs are 8-bit so anything >256 wouldn't be addressable
        // from a single primitive anyway; reject taller-than-256
        // textures rather than silently producing wrong UVs.
        u8::try_from(texture.width()).ok()?;
        u8::try_from(texture.height()).ok()?;
        if texture.clut_entries() != 16 {
            return None;
        }
        let texture_width = room_texture_window_size(texture.width())?;
        let texture_height = room_texture_window_size(texture.height())?;
        let texture_width_halfwords = u16::from(texture_width) / 4;
        let texture_height_rows = u16::from(texture_height);
        if texture.halfwords_per_row() > texture_width_halfwords
            || texture.height() > texture_height_rows
        {
            return None;
        }
        let pixel_bytes = texture.pixel_bytes();
        let pixel_len = (texture.halfwords_per_row() as usize)
            .saturating_mul(texture.height() as usize)
            .saturating_mul(2);
        if pixel_bytes.len() != pixel_len {
            return None;
        }
        let clut_x = self.next_clut_x;
        let clut_y = ROOM_CLUT_Y;
        if clut_x + ROOM_CLUT_HALFWORDS > VRAM_WIDTH as u16 {
            return None;
        }

        let placement = self
            .room_allocator
            .allocate(u16::from(texture_width), u16::from(texture_height))?;
        let tpage_index = u16::from(ROOM_FIRST_TPAGE).checked_add(placement.page_index())?;
        let tpage_x = tpage_index.checked_mul(64)?;
        if !self.upload_compact_4bpp_texture(
            tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?,
            u16::from(placement.origin_v()),
            texture_width_halfwords,
            texture_height_rows,
            &texture,
        ) {
            return None;
        }

        // CLUT halfwords: 16 entries for 4bpp. The editor allocates
        // one 4bpp-sized CLUT band per material; 8bpp + 15bpp
        // follow-up since they need a wider band. Detect via the
        // declared CLUT entry count rather than the depth enum so
        // the only psx-asset surface this file touches is `Texture`.
        let clut_bytes = texture.clut_bytes();
        let transparent_index_zero = texture.index_zero_transparent();
        if !clut_bytes.is_empty() {
            for i in 0..16 {
                let off = i * 2;
                if off + 1 >= clut_bytes.len() {
                    break;
                }
                let raw = u16::from_le_bytes([clut_bytes[off], clut_bytes[off + 1]]);
                // Legacy room textures keep index 0 opaque. Cooked
                // PSXT files that explicitly opt into transparent
                // zero must keep raw CLUT 0 so the preview matches
                // the runtime cutout path.
                let marked = if transparent_index_zero && i == 0 && raw == 0 {
                    0
                } else if force_zero_opaque {
                    opaque_room_clut_entry(raw)
                } else {
                    raw
                };
                let vram_idx = (clut_y as usize) * VRAM_WIDTH as usize + clut_x as usize + i;
                self.vram[vram_idx] = marked;
            }
        }

        let slot = MaterialSlot {
            tpage_word: pack_tpage_word(tpage_index, 0),
            clut_word: pack_clut_word(clut_x, clut_y),
            texture_window: TextureWindow::power_of_two_tile(
                placement.origin_u(),
                placement.origin_v(),
                texture_width,
                texture_height,
            ),
            texture_width,
            texture_height,
        };
        self.next_clut_x += ROOM_CLUT_HALFWORDS;
        Some(slot)
    }

    fn upload_compact_4bpp_texture(
        &mut self,
        texture_x: u16,
        texture_y: u16,
        max_width_halfwords: u16,
        max_height: u16,
        texture: &Texture,
    ) -> bool {
        let halfwords_per_row = texture.halfwords_per_row() as usize;
        let height_px = texture.height() as usize;
        if max_width_halfwords == 0
            || max_height == 0
            || halfwords_per_row == 0
            || halfwords_per_row > max_width_halfwords as usize
            || height_px == 0
            || height_px > max_height as usize
        {
            return false;
        }

        let pixel_bytes = texture.pixel_bytes();
        if pixel_bytes.len() != halfwords_per_row * height_px * 2 {
            return false;
        }
        for row in 0..height_px {
            for hw in 0..halfwords_per_row {
                let off = (row * halfwords_per_row + hw) * 2;
                let word = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]);
                let vram_idx =
                    (texture_y as usize + row) * VRAM_WIDTH as usize + texture_x as usize + hw;
                self.vram[vram_idx] = word;
            }
        }
        true
    }

    /// Stamp a name-keyed procedural pattern (brick / stone / wood /
    /// metal / glass / default checker) into the next free slot.
    /// Returns `None` only when the preview room texture band is full.
    fn upload_procedural(&mut self, material_name: &str) -> Option<MaterialSlot> {
        let pattern = pattern_for_name(material_name);
        let clut_x = self.next_clut_x;
        let clut_y = ROOM_CLUT_Y;
        if clut_x + ROOM_CLUT_HALFWORDS > VRAM_WIDTH as u16 {
            return None;
        }
        let placement = self
            .room_allocator
            .allocate(ROOM_TILE_TEXELS, ROOM_TILE_TEXELS)?;
        let tpage_index = u16::from(ROOM_FIRST_TPAGE).checked_add(placement.page_index())?;
        let tpage_x = tpage_index.checked_mul(64)?;
        self.upload_4bpp(
            tpage_x.checked_add(u16::from(placement.origin_u()) / 4)?,
            u16::from(placement.origin_v()),
            &pattern.pixels,
        );
        self.upload_clut(clut_x, clut_y, &pattern.palette);
        let slot = MaterialSlot {
            tpage_word: pack_tpage_word(tpage_index, 0),
            clut_word: pack_clut_word(clut_x, clut_y),
            texture_window: TextureWindow::power_of_two_tile(
                placement.origin_u(),
                placement.origin_v(),
                64,
                64,
            ),
            texture_width: 64,
            texture_height: 64,
        };
        self.next_clut_x += ROOM_CLUT_HALFWORDS;
        Some(slot)
    }

    /// Pack a 4bpp 64x64 pattern at `(x, y)` (halfword coords).
    /// Each halfword carries four 4bpp pixels (low nibble = leftmost).
    fn upload_4bpp(&mut self, texture_x: u16, texture_y: u16, pixels: &[u8]) {
        let source_texels_per_row = 64usize;
        let source_rows = 64usize;
        let source_halfwords_per_row = source_texels_per_row / 4;
        for row in 0..source_rows {
            for hw in 0..source_halfwords_per_row {
                let src = row * source_texels_per_row + hw * 4;
                let p0 = pixels[src] & 0x0F;
                let p1 = pixels[src + 1] & 0x0F;
                let p2 = pixels[src + 2] & 0x0F;
                let p3 = pixels[src + 3] & 0x0F;
                let word =
                    (p0 as u16) | ((p1 as u16) << 4) | ((p2 as u16) << 8) | ((p3 as u16) << 12);
                let vram_idx =
                    (texture_y as usize + row) * VRAM_WIDTH as usize + texture_x as usize + hw;
                self.vram[vram_idx] = word;
            }
        }
    }

    fn upload_clut(&mut self, clut_x: u16, clut_y: u16, palette: &[u16; 16]) {
        for (i, &entry) in palette.iter().enumerate() {
            let vram_idx = (clut_y as usize) * VRAM_WIDTH as usize + clut_x as usize + i;
            self.vram[vram_idx] = entry;
        }
    }

    fn upload_shadow_texture(&mut self) {
        let shadow = shadow_pattern();
        let source_halfwords_per_row = SHADOW_TEXTURE_SIZE / 4;
        for row in 0..SHADOW_TEXTURE_SIZE {
            for hw in 0..source_halfwords_per_row {
                let src = row * SHADOW_TEXTURE_SIZE + hw * 4;
                let word = (shadow.pixels[src] as u16)
                    | ((shadow.pixels[src + 1] as u16) << 4)
                    | ((shadow.pixels[src + 2] as u16) << 8)
                    | ((shadow.pixels[src + 3] as u16) << 12);
                let vram_idx = (SHADOW_TPAGE_Y as usize + row) * VRAM_WIDTH as usize
                    + SHADOW_TPAGE_X as usize
                    + hw;
                self.vram[vram_idx] = word;
            }
        }
        for (i, &entry) in shadow.palette.iter().enumerate() {
            let vram_idx =
                (SHADOW_CLUT_Y as usize) * VRAM_WIDTH as usize + SHADOW_CLUT_X as usize + i;
            self.vram[vram_idx] = entry;
        }
    }

    /// Walk every Model resource and ensure its atlas (if any)
    /// is uploaded into the dedicated 8bpp model atlas region.
    /// Models without `texture_path` get an empty cache entry so
    /// the walk doesn't repeatedly try to resolve them.
    pub fn refresh_models(&mut self, project: &ProjectDocument, project_root: &Path) {
        for resource in &project.resources {
            let ResourceData::Model(model) = &resource.data else {
                continue;
            };
            let signature = model.texture_path.clone().unwrap_or_default();
            if self
                .model_cache
                .get(&resource.id)
                .is_some_and(|entry| entry.signature == signature)
            {
                continue;
            }
            if signature.is_empty() {
                // No atlas to upload; record an empty signature
                // so subsequent refreshes skip cleanly.
                self.model_cache.remove(&resource.id);
                continue;
            }
            let abs = if Path::new(&signature).is_absolute() {
                PathBuf::from(&signature)
            } else {
                project_root.join(&signature)
            };
            let Some(slot) = self.upload_model_atlas_psxt(&abs) else {
                self.model_cache.remove(&resource.id);
                continue;
            };
            self.model_cache
                .insert(resource.id, ModelAtlasCacheEntry { slot, signature });
        }
    }

    /// Read an 8bpp `.psxt` atlas and upload pixels + 256-entry
    /// CLUT into the dedicated model VRAM region. Returns `None`
    /// on missing file, parse failure, unsupported depth (only
    /// 8bpp is allowed), or VRAM exhaustion.
    fn upload_model_atlas_psxt(&mut self, abs: &Path) -> Option<MaterialSlot> {
        let bytes = std::fs::read(abs).ok()?;
        let texture = Texture::from_bytes(&bytes).ok()?;
        if texture.clut_entries() != 256 {
            // Only 8bpp atlases supported in this region -- 4bpp
            // model atlases would belong in the room-material
            // path which we leave alone here.
            return None;
        }
        let halfwords_per_row = texture.halfwords_per_row();
        let height_px = texture.height();
        let pixel_bytes = texture.pixel_bytes();

        // PSX tpage word can only address tpage *bases*: each
        // page base is 64-halfword aligned, identified
        // by `tpage_index = base_x / 64`. There's no per-atlas
        // base-X offset inside a page, so an atlas placed at a
        // non-64-halfword boundary would sample from the wrong
        // page. 8bpp UVs can address 256 texels horizontally,
        // which occupies 128 VRAM halfwords; smaller atlases still
        // reserve one 64-halfword base column to keep their UVs local.
        const MODEL_TPAGE_ALIGNMENT_HALFWORDS: u16 = 64;
        const MODEL_TPAGE_MAX_HALFWORDS: u16 = 128;
        if texture.width() == 0
            || texture.width() > 256
            || height_px == 0
            || height_px > 256
            || halfwords_per_row > MODEL_TPAGE_MAX_HALFWORDS
        {
            return None;
        }
        let expected_pixel_bytes = (halfwords_per_row as usize)
            .saturating_mul(height_px as usize)
            .saturating_mul(2);
        if pixel_bytes.len() != expected_pixel_bytes {
            return None;
        }

        let slot_halfwords = if halfwords_per_row <= MODEL_TPAGE_ALIGNMENT_HALFWORDS {
            MODEL_TPAGE_ALIGNMENT_HALFWORDS
        } else {
            MODEL_TPAGE_MAX_HALFWORDS
        };
        let aligned_tpage_x = align_up_to(self.next_model_tpage_x, MODEL_TPAGE_ALIGNMENT_HALFWORDS);
        if aligned_tpage_x >= SHADOW_TPAGE_X
            || aligned_tpage_x.saturating_add(slot_halfwords) > SHADOW_TPAGE_X
        {
            return None;
        }
        if aligned_tpage_x as u32 + slot_halfwords as u32 > VRAM_WIDTH {
            return None;
        }
        if self.next_model_clut_y as usize >= VRAM_HEIGHT as usize {
            return None;
        }
        let tpage_x = aligned_tpage_x;
        let tpage_y: u16 = 256;
        let clut_y = self.next_model_clut_y;

        // Pixels.
        if pixel_bytes.len() < (halfwords_per_row as usize) * (height_px as usize) * 2 {
            return None;
        }
        for row in 0..height_px as usize {
            for hw in 0..halfwords_per_row as usize {
                let off = (row * halfwords_per_row as usize + hw) * 2;
                let word = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]);
                let vram_idx =
                    (tpage_y as usize + row) * VRAM_WIDTH as usize + tpage_x as usize + hw;
                self.vram[vram_idx] = word;
            }
        }

        // CLUT: 256 halfwords on a single row. Alpha-aware model
        // atlases keep palette index 0 transparent; legacy atlases
        // stay fully opaque so existing cooked dark textures do not
        // gain holes.
        let clut_bytes = texture.clut_bytes();
        if clut_bytes.len() < 512 {
            return None;
        }
        let transparent_index_zero = texture.index_zero_transparent();
        for i in 0..256 {
            let off = i * 2;
            let raw = u16::from_le_bytes([clut_bytes[off], clut_bytes[off + 1]]);
            let marked = model_atlas_clut_entry(i, raw, transparent_index_zero);
            let vram_idx = (clut_y as usize) * VRAM_WIDTH as usize + i;
            self.vram[vram_idx] = marked;
        }

        // Tpage word: tpage row at y=256 → tpage_y_block = 1.
        // Tpage X is `tpage_x / 64` (always 0..15 within a row).
        // 8bpp depth bit pattern is `1` (4bpp = 0).
        let tpage_index = tpage_x / MODEL_TPAGE_ALIGNMENT_HALFWORDS;
        let slot = MaterialSlot {
            tpage_word: pack_8bpp_tpage_word(tpage_index, 1),
            clut_word: pack_clut_word(0, clut_y),
            texture_window: TextureWindow::NONE,
            texture_width: texture.width().min(u16::from(u8::MAX)) as u8,
            texture_height: texture.height().min(u16::from(u8::MAX)) as u8,
        };

        // Advance to the next aligned slot. 128-texel 8bpp atlases
        // consume one base column; 256-texel atlases consume two.
        self.next_model_tpage_x = aligned_tpage_x.saturating_add(slot_halfwords);
        self.next_model_clut_y = self.next_model_clut_y.saturating_add(1);
        Some(slot)
    }
}

#[derive(Debug, Clone)]
struct PreviewTextureUploadPlan {
    id: ResourceId,
    name: String,
    signature: String,
    cache_signature: String,
    force_zero_opaque: bool,
    allow_procedural_fallback: bool,
}

fn preview_texture_upload_plan(
    project: &ProjectDocument,
    project_root: &Path,
) -> Vec<PreviewTextureUploadPlan> {
    let scene_material_ids = collect_scene_resource_use(project).materials;
    let mut plan = Vec::new();
    for id in scene_material_ids {
        push_material_resource_upload(project, id, true, project_root, &mut plan);
    }

    push_image_prop_material_uploads(project, project_root, &mut plan);
    push_far_vista_texture_uploads(project, project_root, &mut plan);

    for resource in &project.resources {
        if matches!(&resource.data, ResourceData::Material(_)) {
            push_material_resource_upload(project, resource.id, true, project_root, &mut plan);
        }
    }

    plan
}

fn push_material_resource_upload(
    project: &ProjectDocument,
    id: ResourceId,
    force_zero_opaque: bool,
    project_root: &Path,
    plan: &mut Vec<PreviewTextureUploadPlan>,
) {
    if let Some(item) = plan.iter_mut().find(|item| item.id == id) {
        item.force_zero_opaque &= force_zero_opaque;
        return;
    }
    let Some(resource) = project.resource(id) else {
        return;
    };
    let ResourceData::Material(material) = &resource.data else {
        return;
    };
    let signature = texture_path(project, material).unwrap_or_default();
    let cache_signature = texture_cache_signature(project_root, &signature);
    plan.push(PreviewTextureUploadPlan {
        id,
        name: resource.name.clone(),
        signature,
        cache_signature,
        force_zero_opaque,
        allow_procedural_fallback: true,
    });
}

fn push_image_prop_material_uploads(
    project: &ProjectDocument,
    project_root: &Path,
    plan: &mut Vec<PreviewTextureUploadPlan>,
) {
    for node in project.active_scene().nodes() {
        let NodeKind::ImageProp {
            material: Some(material_id),
            ..
        } = &node.kind
        else {
            continue;
        };
        push_material_resource_upload(project, *material_id, false, project_root, plan);
    }
}

fn push_far_vista_texture_uploads(
    project: &ProjectDocument,
    project_root: &Path,
    plan: &mut Vec<PreviewTextureUploadPlan>,
) {
    for node in project.active_scene().nodes() {
        let NodeKind::World { far_vista, .. } = &node.kind else {
            continue;
        };
        if !far_vista.enabled {
            continue;
        }
        let assigned_panels = far_vista.texture_panels.iter().any(Option::is_some);
        if assigned_panels {
            for texture_id in far_vista.texture_panels.iter().flatten() {
                push_texture_resource_upload(project, *texture_id, project_root, plan);
            }
        } else if let Some(texture_id) = far_vista.texture {
            push_texture_resource_upload(project, texture_id, project_root, plan);
        }
    }
}

fn push_texture_resource_upload(
    project: &ProjectDocument,
    id: ResourceId,
    project_root: &Path,
    plan: &mut Vec<PreviewTextureUploadPlan>,
) {
    if plan.iter().any(|item| item.id == id) {
        return;
    }
    let Some(resource) = project.resource(id) else {
        return;
    };
    let ResourceData::Texture { psxt_path } = &resource.data else {
        return;
    };
    let cache_signature = texture_cache_signature(project_root, psxt_path);
    plan.push(PreviewTextureUploadPlan {
        id,
        name: resource.name.clone(),
        signature: psxt_path.clone(),
        cache_signature,
        force_zero_opaque: false,
        allow_procedural_fallback: false,
    });
}

fn texture_cache_signature(project_root: &Path, stored: &str) -> String {
    if stored.is_empty() {
        return "empty".to_string();
    }
    let abs = if Path::new(stored).is_absolute() {
        PathBuf::from(stored)
    } else {
        project_root.join(stored)
    };
    let Ok(metadata) = std::fs::metadata(&abs) else {
        return format!("{stored}|missing");
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
    match modified {
        Some(modified) => format!(
            "{stored}|{}|{}|{}",
            metadata.len(),
            modified.as_secs(),
            modified.subsec_nanos()
        ),
        None => format!("{stored}|{}|unknown-mtime", metadata.len()),
    }
}

/// Resolve a Material's texture link to the underlying `.psxt`
/// path string, or `None` if the link is missing or the linked
/// resource isn't a Texture.
fn texture_path(project: &ProjectDocument, material: &MaterialResource) -> Option<String> {
    let tex_id = material.texture?;
    let resource: &Resource = project.resource(tex_id)?;
    match &resource.data {
        ResourceData::Texture { psxt_path } => Some(psxt_path.clone()),
        _ => None,
    }
}

/// One generated procedural texture: `pixels` are raw 4bpp indices
/// (0..15), one byte per pixel; `palette` is the 16-entry CLUT in
/// PSX BGR555 format.
struct ProceduralTexture {
    pixels: Vec<u8>,
    palette: [u16; 16],
}

fn pattern_for_name(name: &str) -> ProceduralTexture {
    let lower = name.to_ascii_lowercase();
    if lower.contains("brick") {
        brick_pattern()
    } else if lower.contains("floor") || lower.contains("stone") {
        stone_pattern()
    } else if lower.contains("glass") {
        glass_pattern()
    } else if lower.contains("wood") {
        wood_pattern()
    } else if lower.contains("metal") {
        metal_pattern()
    } else {
        default_pattern()
    }
}

/// Stamp a 64×64 brick wall: terra-cotta bricks separated by dark
/// mortar, alternating rows offset by half a brick.
fn brick_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x30, 0x18, 0x10)), // 0: mortar (dark)
        psx_555((0xC8, 0x70, 0x40)), // 1: brick base
        psx_555((0xB0, 0x60, 0x38)), // 2: brick darker
        psx_555((0xD8, 0x88, 0x58)), // 3: brick highlight
        psx_555((0x40, 0x20, 0x18)), // 4: deep shadow
        psx_555((0x50, 0x28, 0x18)),
        psx_555((0x60, 0x30, 0x20)),
        psx_555((0x70, 0x40, 0x28)),
        psx_555((0x80, 0x48, 0x30)),
        psx_555((0x90, 0x50, 0x38)),
        psx_555((0xA0, 0x58, 0x38)),
        psx_555((0xC0, 0x68, 0x40)),
        psx_555((0xD0, 0x78, 0x48)),
        psx_555((0xE0, 0x90, 0x60)),
        psx_555((0xF0, 0xA0, 0x70)),
        psx_555((0xFF, 0xB8, 0x88)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64 {
        let brick_row = y / 8;
        let row_offset = if brick_row % 2 == 0 { 0 } else { 8 };
        for x in 0..64 {
            let local_y = y % 8;
            let local_x = (x + row_offset) % 16;
            let nibble = if local_y == 0 || local_y == 7 || local_x == 0 || local_x == 15 {
                0 // mortar
            } else if local_y == 1 || local_x == 1 || local_x == 14 {
                2 // brick edge shadow
            } else if local_y == 6 {
                3 // brick top highlight (because wider on top in side-light)
            } else {
                1 // brick base
            };
            pixels[y * 64 + x] = nibble;
        }
    }
    ProceduralTexture { pixels, palette }
}

/// 64×64 stone-tile floor: sand-toned squares with slightly darker
/// grout. The grout grid runs every 16 px so a single tile occupies
/// a quarter of the texture.
fn stone_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x60, 0x58, 0x4C)), // 0: grout
        psx_555((0xB6, 0xAC, 0x96)), // 1: stone base
        psx_555((0xA0, 0x96, 0x80)), // 2: stone darker
        psx_555((0xC8, 0xBC, 0xA8)), // 3: stone lighter
        psx_555((0x90, 0x88, 0x70)),
        psx_555((0x98, 0x90, 0x78)),
        psx_555((0xA8, 0x9E, 0x88)),
        psx_555((0xB0, 0xA6, 0x90)),
        psx_555((0xB8, 0xAE, 0x98)),
        psx_555((0xC0, 0xB6, 0xA0)),
        psx_555((0xC8, 0xBE, 0xA8)),
        psx_555((0xD0, 0xC4, 0xB0)),
        psx_555((0xD8, 0xCC, 0xB8)),
        psx_555((0xE0, 0xD4, 0xC0)),
        psx_555((0x88, 0x80, 0x6C)),
        psx_555((0x70, 0x68, 0x58)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64usize {
        for x in 0..64usize {
            let lx = x % 16;
            let ly = y % 16;
            let nibble: u8 = if lx == 0 || ly == 0 {
                0 // grout
            } else if lx == 1 || ly == 1 {
                2 // shadow on the inside of the grout
            } else if lx == 15 || ly == 15 {
                3 // highlight on the opposite edge
            } else {
                // Speckle the interior so it's not flat -- pseudo-
                // random nibble derived from coords stays stable.
                let h = ((x as u32).wrapping_mul(73) ^ (y as u32).wrapping_mul(151)) & 0x07;
                if h < 2 {
                    2
                } else if h > 5 {
                    3
                } else {
                    1
                }
            };
            pixels[y * 64 + x] = nibble;
        }
    }
    ProceduralTexture { pixels, palette }
}

fn glass_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x10, 0x20, 0x30)),
        psx_555((0x30, 0x60, 0x90)),
        psx_555((0x40, 0x78, 0xA8)),
        psx_555((0x50, 0x88, 0xB8)),
        psx_555((0x60, 0x98, 0xC8)),
        psx_555((0x70, 0xA8, 0xD0)),
        psx_555((0x80, 0xB0, 0xD8)),
        psx_555((0x88, 0xB8, 0xE0)),
        psx_555((0x90, 0xC0, 0xE8)),
        psx_555((0x98, 0xC8, 0xF0)),
        psx_555((0xA0, 0xD0, 0xF8)),
        psx_555((0xB0, 0xD8, 0xFF)),
        psx_555((0xC0, 0xE0, 0xFF)),
        psx_555((0x20, 0x40, 0x68)),
        psx_555((0x28, 0x48, 0x70)),
        psx_555((0x18, 0x30, 0x58)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64 {
        for x in 0..64 {
            let dx = (x as i32) - 32;
            let dy = (y as i32) - 32;
            let r = (dx * dx + dy * dy) as u32;
            // Shade by distance to centre -- soft cyan disc-like glow.
            let nibble = (12u32.saturating_sub(r / 96)).min(12) as u8;
            pixels[y * 64 + x] = nibble;
        }
    }
    ProceduralTexture { pixels, palette }
}

fn wood_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x30, 0x18, 0x10)),
        psx_555((0x90, 0x60, 0x40)),
        psx_555((0x80, 0x50, 0x30)),
        psx_555((0xA0, 0x70, 0x48)),
        psx_555((0x70, 0x48, 0x28)),
        psx_555((0x60, 0x38, 0x20)),
        psx_555((0x50, 0x30, 0x18)),
        psx_555((0x40, 0x28, 0x18)),
        psx_555((0xB0, 0x80, 0x50)),
        psx_555((0xC0, 0x90, 0x58)),
        psx_555((0xD0, 0xA0, 0x68)),
        psx_555((0xA8, 0x78, 0x48)),
        psx_555((0x98, 0x68, 0x40)),
        psx_555((0x88, 0x58, 0x38)),
        psx_555((0x78, 0x50, 0x30)),
        psx_555((0x68, 0x48, 0x28)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64 {
        for x in 0..64 {
            let plank = y / 16;
            let in_plank_y = y % 16;
            let nibble = if in_plank_y == 0 {
                0 // plank seam
            } else {
                // Grain -- pseudo-random nibble per (plank, x); same
                // x in same plank gives same value so vertical lines.
                let seed = (plank as u32).wrapping_mul(31).wrapping_add(x as u32);
                let h = (seed.wrapping_mul(2654435761) >> 28) as u8;
                (h & 0x0F).max(1)
            };
            pixels[y * 64 + x] = nibble;
        }
    }
    ProceduralTexture { pixels, palette }
}

fn metal_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x30, 0x30, 0x30)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x90, 0x90, 0x90)),
        psx_555((0xA0, 0xA0, 0xA0)),
        psx_555((0xB0, 0xB0, 0xB0)),
        psx_555((0xC0, 0xC0, 0xC0)),
        psx_555((0xD0, 0xD0, 0xD0)),
        psx_555((0xE0, 0xE0, 0xE0)),
        psx_555((0x70, 0x70, 0x70)),
        psx_555((0x60, 0x60, 0x60)),
        psx_555((0x50, 0x50, 0x50)),
        psx_555((0x40, 0x40, 0x40)),
        psx_555((0xF0, 0xF0, 0xF0)),
        psx_555((0xFF, 0xFF, 0xFF)),
        psx_555((0xC8, 0xC8, 0xC8)),
        psx_555((0xB8, 0xB8, 0xB8)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64usize {
        for x in 0..64usize {
            // Faint horizontal brushed-metal stripes.
            let stripe = (y / 2) & 1;
            let base = if stripe == 0 { 4u32 } else { 5u32 };
            let h = ((x as u32).wrapping_mul(101) ^ (y as u32).wrapping_mul(13)) & 0x03;
            pixels[y * 64 + x] = (base + h) as u8;
        }
    }
    ProceduralTexture { pixels, palette }
}

fn default_pattern() -> ProceduralTexture {
    let palette = [
        psx_555((0x40, 0x40, 0x40)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0xA0, 0xA0, 0xA0)),
        psx_555((0xC0, 0xC0, 0xC0)),
        psx_555((0xE0, 0xE0, 0xE0)),
        psx_555((0xFF, 0x80, 0xFF)), // hot-pink for "missing pattern" debug
        psx_555((0xFF, 0xFF, 0x80)),
        psx_555((0x80, 0xFF, 0xFF)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
        psx_555((0x80, 0x80, 0x80)),
    ];
    let mut pixels = vec![0u8; 64 * 64];
    for y in 0..64 {
        for x in 0..64 {
            let nibble = if (x / 4 + y / 4) & 1 == 0 { 1 } else { 3 };
            pixels[y * 64 + x] = nibble;
        }
    }
    ProceduralTexture { pixels, palette }
}

fn shadow_pattern() -> ProceduralTexture {
    let bayer: [[u8; 8]; 8] = [
        [0, 48, 12, 60, 3, 51, 15, 63],
        [32, 16, 44, 28, 35, 19, 47, 31],
        [8, 56, 4, 52, 11, 59, 7, 55],
        [40, 24, 36, 20, 43, 27, 39, 23],
        [2, 50, 14, 62, 1, 49, 13, 61],
        [34, 18, 46, 30, 33, 17, 45, 29],
        [10, 58, 6, 54, 9, 57, 5, 53],
        [42, 26, 38, 22, 41, 25, 37, 21],
    ];
    let mut pixels = vec![0u8; SHADOW_TEXTURE_SIZE * SHADOW_TEXTURE_SIZE];
    let center = (SHADOW_TEXTURE_SIZE as f32 - 1.0) * 0.5;
    let radius = 30.0f32;
    let core_radius = 12.0f32;
    for y in 0..SHADOW_TEXTURE_SIZE {
        for x in 0..SHADOW_TEXTURE_SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let d = (dx * dx + dy * dy).sqrt();
            let idx = if d >= radius {
                0
            } else {
                let t = (1.0 - d / radius).max(0.0);
                if d <= core_radius {
                    5
                } else {
                    let coverage = ((t.powf(0.50) * 1.12).min(0.98) * 64.0).round() as u8;
                    if bayer[y & 7][x & 7] >= coverage {
                        0
                    } else {
                        (1.0 + t.powf(0.65) * 4.0).round().clamp(1.0, 5.0) as u8
                    }
                }
            };
            pixels[y * SHADOW_TEXTURE_SIZE + x] = idx;
        }
    }
    let mut palette = [0u16; 16];
    for (i, entry) in palette.iter_mut().enumerate().skip(1) {
        let v = (1 + i / 4).clamp(1, 5) as u8;
        *entry = psx_555((v << 3, v << 3, v << 3)) | 0x8000;
    }
    ProceduralTexture { pixels, palette }
}

/// Convert a 24-bit RGB triple to PSX 15bpp BGR555. Bit 15 (the STP
/// flag) stays 0; semitransparency hits later via the polygon
/// translucency bit, not the per-texel STP bit.
fn psx_555(rgb: (u8, u8, u8)) -> u16 {
    let r5 = (rgb.0 >> 3) as u16;
    let g5 = (rgb.1 >> 3) as u16;
    let b5 = (rgb.2 >> 3) as u16;
    (b5 << 10) | (g5 << 5) | r5
}

fn opaque_room_clut_entry(raw: u16) -> u16 {
    raw | 0x8000
}

fn model_atlas_clut_entry(index: usize, raw: u16, transparent_index_zero: bool) -> u16 {
    if transparent_index_zero && index == 0 && raw == 0 {
        0
    } else {
        raw | 0x8000
    }
}

/// Pack a (tpage_index, tpage_y_block) pair into the GP0
/// uv1-high-half tpage word format. 4bpp depth, blend bits 0,
/// matching `psx_vram::Tpage::uv_tpage_word(0)`.
fn pack_tpage_word(tpage_index: u16, tpage_y_block: u16) -> u16 {
    let depth = 0u16; // 4bpp
    let semi_trans = 0u16; // 0.5*bg + 0.5*fg blend bits
    (tpage_index & 0xF) | (tpage_y_block << 4) | (semi_trans << 5) | (depth << 7)
}

/// Same as `pack_tpage_word` but with the 8bpp depth bit set --
/// used for model atlas slots which always live in the 8bpp
/// model VRAM region.
fn pack_8bpp_tpage_word(tpage_index: u16, tpage_y_block: u16) -> u16 {
    let depth = 1u16; // 8bpp
    let semi_trans = 0u16;
    (tpage_index & 0xF) | (tpage_y_block << 4) | (semi_trans << 5) | (depth << 7)
}

/// Pack a (clut_x_in_halfwords, clut_y) pair into the GP0
/// uv0-high-half CLUT word format.
fn pack_clut_word(clut_x_halfwords: u16, clut_y: u16) -> u16 {
    let cx = (clut_x_halfwords / 16) & 0x3F;
    let cy = clut_y & 0x1FF;
    cx | (cy << 6)
}

fn room_texture_window_size(size: u16) -> Option<u8> {
    if size < 8 || size > ROOM_TILE_TEXELS || !size.is_power_of_two() || size % 8 != 0 {
        return None;
    }
    u8::try_from(size).ok()
}

/// Round `value` up to the next multiple of `boundary`.
/// `boundary` must be a power of two for the bitmask path; the
/// general formula handles arbitrary moduli but the model
/// atlas allocator only ever passes 64.
fn align_up_to(value: u16, boundary: u16) -> u16 {
    if boundary == 0 {
        return value;
    }
    let rem = value % boundary;
    if rem == 0 {
        value
    } else {
        value.saturating_add(boundary - rem)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        align_up_to, model_atlas_clut_entry, opaque_room_clut_entry, preview_texture_upload_plan,
        EditorTextures, SHADOW_CLUT_X, SHADOW_CLUT_Y, SHADOW_TPAGE_X, SHADOW_TPAGE_Y,
    };
    use psx_gpu::material::TextureWindow;
    use psx_gpu_render::VRAM_WIDTH;
    use psxed_project::{
        FarVistaSettings, MaterialResource, NodeKind, ProjectDocument, ResourceData, WorldGrid,
    };
    use std::path::{Path, PathBuf};

    fn add_material(
        project: &mut ProjectDocument,
        name: impl Into<String>,
    ) -> psxed_project::ResourceId {
        project.add_resource(name, ResourceData::Material(MaterialResource::opaque(None)))
    }

    fn write_test_8bpp_psxt(width: u16, height: u16) -> PathBuf {
        let halfwords_per_row = width.div_ceil(2);
        let pixel_bytes = u32::from(halfwords_per_row) * u32::from(height) * 2;
        let clut_bytes = 256u32 * 2;
        let payload_len = 16 + pixel_bytes + clut_bytes;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PSXT");
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&payload_len.to_le_bytes());
        bytes.push(8);
        bytes.push(0);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&256u16.to_le_bytes());
        bytes.extend_from_slice(&pixel_bytes.to_le_bytes());
        bytes.extend_from_slice(&clut_bytes.to_le_bytes());
        for _ in 0..(pixel_bytes / 2) {
            bytes.extend_from_slice(&0x3412u16.to_le_bytes());
        }
        for index in 0..256u16 {
            bytes.extend_from_slice(&(0x8000 | index).to_le_bytes());
        }

        let path = std::env::temp_dir().join(format!(
            "psoxide-editor-model-atlas-{}-{width}x{height}.psxt",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("write temporary PSXT");
        path
    }

    #[test]
    fn align_up_to_handles_aligned_and_misaligned_values() {
        assert_eq!(align_up_to(0, 64), 0);
        assert_eq!(align_up_to(1, 64), 64);
        assert_eq!(align_up_to(63, 64), 64);
        assert_eq!(align_up_to(64, 64), 64);
        assert_eq!(align_up_to(65, 64), 128);
        assert_eq!(align_up_to(127, 64), 128);
        assert_eq!(align_up_to(128, 64), 128);
        // Boundary 0 is a no-op (defensive -- the allocator
        // only ever passes 64).
        assert_eq!(align_up_to(33, 0), 33);
    }

    #[test]
    fn procedural_room_texture_upload_is_compact_and_windowed() {
        let mut pixels = vec![0u8; 64 * 64];
        for row in 0..64usize {
            for hw in 0..16usize {
                let base = ((row ^ hw) & 0x0F) as u8;
                let src = row * 64 + hw * 4;
                pixels[src] = base;
                pixels[src + 1] = base.wrapping_add(1) & 0x0F;
                pixels[src + 2] = base.wrapping_add(2) & 0x0F;
                pixels[src + 3] = base.wrapping_add(3) & 0x0F;
            }
        }

        let mut textures = EditorTextures::new();
        let tpage_x = 320u16;
        textures.upload_4bpp(tpage_x, 0, &pixels);

        let word_at = |row: usize, hw: usize| -> u16 {
            textures.vram[row * VRAM_WIDTH as usize + tpage_x as usize + hw]
        };
        assert_ne!(word_at(63, 15), 0);
        assert_eq!(word_at(64, 0), 0);
        assert_eq!(word_at(0, 16), 0);

        let slot = textures
            .upload_procedural("stone")
            .expect("procedural texture fits");
        assert_eq!(
            slot.texture_window.word(),
            TextureWindow::power_of_two_tile(0, 0, 64, 64).word()
        );
    }

    #[test]
    fn imported_room_clut_keeps_palette_zero_opaque() {
        assert_eq!(opaque_room_clut_entry(0), 0x8000);
        assert_eq!(opaque_room_clut_entry(0x1234), 0x9234);
    }

    #[test]
    fn imported_model_clut_preserves_flagged_index_zero_transparency() {
        assert_eq!(model_atlas_clut_entry(0, 0, true), 0);
        assert_eq!(model_atlas_clut_entry(0, 0, false), 0x8000);
        assert_eq!(model_atlas_clut_entry(1, 0, true), 0x8000);
        assert_eq!(model_atlas_clut_entry(2, 0x0001, true), 0x8001);
        assert_eq!(model_atlas_clut_entry(3, 0x001F, true), 0x801F);
        assert_eq!(model_atlas_clut_entry(4, 0x0421, true), 0x8421);
    }

    #[test]
    fn model_atlas_upload_accepts_full_8bpp_page_width() {
        let path = write_test_8bpp_psxt(256, 16);
        let mut textures = EditorTextures::new();

        let slot = textures
            .upload_model_atlas_psxt(&path)
            .expect("256-texel 8bpp model atlas fits in one PSX texture page");

        assert_eq!(slot.tpage_word & 0x0F, 0);
        assert_eq!((slot.tpage_word >> 7) & 0x03, 1);
        assert_eq!(slot.texture_width, u8::MAX);
        assert_eq!(slot.texture_height, 16);
        assert_eq!(textures.next_model_tpage_x, 128);
        let last_uploaded_word = (256usize + 15) * VRAM_WIDTH as usize + 127;
        assert_eq!(textures.vram[last_uploaded_word], 0x3412);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn editor_shadow_texture_survives_room_material_refresh() {
        let mut textures = EditorTextures::new();
        let shadow_tex_idx =
            (SHADOW_TPAGE_Y as usize + 31) * VRAM_WIDTH as usize + SHADOW_TPAGE_X as usize + 7;
        let shadow_clut_idx = SHADOW_CLUT_Y as usize * VRAM_WIDTH as usize + SHADOW_CLUT_X as usize;

        let texture_word = textures.vram[shadow_tex_idx];
        let clut_word = textures.vram[shadow_clut_idx + 1];
        assert_ne!(texture_word, 0);
        assert_ne!(clut_word, 0);

        let project = ProjectDocument::new("preview");
        textures.refresh(&project, Path::new("."));

        assert_eq!(textures.vram[shadow_tex_idx], texture_word);
        assert_eq!(textures.vram[shadow_clut_idx + 1], clut_word);
    }

    #[test]
    fn refresh_prioritizes_scene_materials_over_resource_order() {
        let mut project = ProjectDocument::new("preview");
        let unused: Vec<_> = (0..80)
            .map(|index| add_material(&mut project, format!("Unused {index}")))
            .collect();
        let used = add_material(&mut project, "Active Late Material");
        let grid = WorldGrid::stone_room(1, 1, 1024, Some(used), Some(used));

        let scene = project.active_scene_mut();
        scene.add_node(scene.root, "Room", NodeKind::Room { grid });

        let plan = preview_texture_upload_plan(&project, Path::new("."));
        assert_eq!(plan.first().map(|item| item.id), Some(used));

        let mut textures = EditorTextures::new();
        textures.refresh(&project, Path::new("."));

        assert!(textures.slot(used).is_some());
        assert!(textures.slot(unused[70]).is_none());
    }

    #[test]
    fn upload_plan_includes_far_vista_panel_textures() {
        let mut project = ProjectDocument::new("preview");
        let panel = project.add_resource(
            "Vista Panel",
            ResourceData::Texture {
                psxt_path: "vista.psxt".to_string(),
            },
        );
        let mut far_vista = FarVistaSettings {
            enabled: true,
            ..FarVistaSettings::default()
        };
        far_vista.texture_panels[0] = Some(panel);

        let scene = project.active_scene_mut();
        scene.add_node(
            scene.root,
            "World",
            NodeKind::World {
                sector_size: 1024,
                sky: Default::default(),
                far_vista,
                camera: Default::default(),
                culling: Default::default(),
                streaming: Default::default(),
            },
        );

        let plan = preview_texture_upload_plan(&project, Path::new("."));
        let item = plan
            .iter()
            .find(|item| item.id == panel)
            .expect("far-vista panel texture is uploaded for preview");
        assert_eq!(item.signature, "vista.psxt");
        assert!(!item.force_zero_opaque);
        assert!(!item.allow_procedural_fallback);
    }

    #[test]
    fn upload_plan_prioritizes_far_vista_panels_before_unused_materials() {
        let mut project = ProjectDocument::new("preview");
        let used = add_material(&mut project, "Active Material");
        let unused: Vec<_> = (0..80)
            .map(|index| add_material(&mut project, format!("Unused {index}")))
            .collect();
        let panel = project.add_resource(
            "Vista Panel",
            ResourceData::Texture {
                psxt_path: "vista.psxt".to_string(),
            },
        );
        let grid = WorldGrid::stone_room(1, 1, 1024, Some(used), Some(used));
        let mut far_vista = FarVistaSettings {
            enabled: true,
            ..FarVistaSettings::default()
        };
        far_vista.texture_panels[0] = Some(panel);

        let scene = project.active_scene_mut();
        scene.add_node(scene.root, "Room", NodeKind::Room { grid });
        scene.add_node(
            scene.root,
            "World",
            NodeKind::World {
                sector_size: 1024,
                sky: Default::default(),
                far_vista,
                camera: Default::default(),
                culling: Default::default(),
                streaming: Default::default(),
            },
        );

        let plan = preview_texture_upload_plan(&project, Path::new("."));
        let panel_index = plan
            .iter()
            .position(|item| item.id == panel)
            .expect("far-vista panel texture is uploaded for preview");
        let first_unused_index = plan
            .iter()
            .position(|item| item.id == unused[0])
            .expect("unused material remains in the upload plan");

        assert_eq!(plan.first().map(|item| item.id), Some(used));
        assert!(panel_index < first_unused_index);
    }
}
