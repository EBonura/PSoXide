//! Upload the cooked map textures to VRAM once at load, using the SDK's `psx-vram` atlas and CLUT
//! allocators, and hand back a per-texture material the renderer feeds straight into
//! `TriTextured`.
//!
//! Layout: 4-bit texture pages in the texture region X=320.. (bands Y=0 and Y=256); CLUTs in the
//! free block X=0..319 Y=480..511, which is what is left under the two 320x240 framebuffers at
//! X=0..319 Y=0..479.
//!
//! The X bound on the CLUT block is load-bearing, not cosmetic. Texture band 1 spans Y=256..511,
//! so the CLUT rows sit *inside* it and only their X keeps the two apart. [`VramState::new`]
//! proves the separation at compile time for the port's page count.
//!
//! The port owns one [`VramState`] static and picks its page count: the allocator pages cover
//! X=320..960 in band 0 and X=320..896 in band 1, and the last band-1 page at X=960 is never
//! offered because HUD pixels, the pre-scaled gameplay font and their CLUTs live there at fixed
//! addresses. A port that keeps further pages for itself passes a smaller count.

use psx_gpu::material::{TextureMaterial, TextureWindow, TexturedGouraudPacketMaterial};
use psx_vram::{
    upload_bytes, ClutRowAllocator, TextureDepth, TexturePage, TextureWindowAtlas, VramRect,
};

use core::ptr;

/// First texture-page column (after the framebuffers).
const TEX_X0: u16 = 320;
/// Pages per band: X=320,384,..,960.
const COLS: u16 = 11;
/// Y=480..511: every row under the framebuffers, all of it clear of the texture region because
/// the block stops at [`CLUT_MAX_X`].
const CLUT_ROWS: usize = 32;
const CLUT_BASE_Y: u16 = 480;
/// The CLUT block ends where the texture region begins. `ClutRowAllocator` has no reservation
/// primitive, so `upload_one` discards and thereby retires any slot at or beyond this X; the
/// allocation naturally continues on the next row. This also keeps world palettes off the fixed
/// HUD and font page at X=960..1023.
const CLUT_MAX_X: u16 = TEX_X0;
/// Usable 16-entry CLUTs: 20 slots a row over 32 rows. A port asserts this covers every texture
/// slot table that can be resident at once.
pub const CLUT_CAPACITY: usize = (CLUT_MAX_X / 16) as usize * CLUT_ROWS;
const VRAM_W: u16 = 1024;
const VRAM_H: u16 = 512;

/// Does any CLUT slot the allocator can hand out land inside one of `pages` texture pages?
///
/// `ClutRowAllocator` walks rows in order and slots left to right within a row, so slot `s` is
/// always at X = `s * 16`. Every slot below [`CLUT_MAX_X`] is reachable, and the block's rows are
/// `CLUT_BASE_Y .. CLUT_BASE_Y + CLUT_ROWS`. A texture page is 64 halfwords wide and 256 rows
/// tall at its `tpage` origin. Const-evaluated, so it costs no image bytes.
const fn clut_block_hits_a_texture_page(pages: usize) -> bool {
    let mut slot = 0u16;
    while slot * 16 < CLUT_MAX_X {
        let x = slot * 16;
        let mut page = 0usize;
        while page < pages {
            let px = TEX_X0 + (page as u16 % COLS) * 64;
            let py: u16 = if page / (COLS as usize) == 0 { 0 } else { 256 };
            let rows_overlap = py < CLUT_BASE_Y + CLUT_ROWS as u16 && CLUT_BASE_Y < py + 256;
            let cols_overlap = x < px + 64 && px < x + 16;
            if rows_overlap && cols_overlap {
                return true;
            }
            page += 1;
        }
        slot += 1;
    }
    false
}

const _: () = assert!(
    CLUT_BASE_Y as usize + CLUT_ROWS <= VRAM_H as usize,
    "the CLUT block runs off the bottom of VRAM"
);

/// One uploaded texture: the material the renderer feeds to its packets, plus what it needs to
/// order them.
#[derive(Copy, Clone)]
pub struct TexSlot {
    /// The texture's draw state.
    pub material: TextureMaterial,
    /// The same state in the Gouraud packet's word layout.
    pub packet: TexturedGouraudPacketMaterial,
    /// Whether the upload fitted VRAM.
    pub valid: bool,
    /// A solid single-colour texture (every texel index 0), for example GoldSrc's `black`
    /// backdrop wall. These sit nearly coplanar with the real geometry in front of them; without
    /// a Z-buffer the per-triangle OT can tie-break the wrong way and the backdrop occludes the
    /// detail. The renderer biases these to the back of the OT so the foreground always wins (a
    /// backdrop is, by definition, behind everything near it).
    pub backdrop: bool,
}

/// The material of an empty slot (mid grey).
pub const EMPTY_MATERIAL_PUB: TextureMaterial = TextureMaterial::opaque(0, 0, (128, 128, 128));
const EMPTY_MATERIAL: TextureMaterial = EMPTY_MATERIAL_PUB;

/// A slot with nothing uploaded.
pub const EMPTY_SLOT: TexSlot = TexSlot {
    material: EMPTY_MATERIAL,
    packet: TexturedGouraudPacketMaterial::from_texture(EMPTY_MATERIAL),
    valid: false,
    backdrop: false,
};

/// All-zero twin of [`EMPTY_SLOT`], used only so the slot tables land in `.bss` rather than
/// `.data`; the real default is installed at boot. A non-zero initializer is stored in the
/// executable and copied to RAM at load, where `.bss` costs neither.
pub const ZERO_SLOT: TexSlot = TexSlot {
    material: TextureMaterial::opaque(0, 0, (0, 0, 0)),
    packet: TexturedGouraudPacketMaterial {
        tex_window_word: 0,
        color0_command_word: 0,
        clut_high_word: 0,
        tpage_high_word: 0,
    },
    valid: false,
    backdrop: false,
};

/// A snapshot of both allocators, taken once the textures that stay resident are uploaded.
pub type Checkpoint<const PAGES: usize> = (TextureWindowAtlas<PAGES>, ClutRowAllocator<CLUT_ROWS>);

/// One entry of an HLTX texture blob. `u16 w | u16 h | u16 clut[16] | u8 pix4[w * h / 2]` unless
/// the top bit of `w` is set, which marks the sky's 8 bit form: `u16 w | u16 h | u16 clut[256] |
/// u8 pix8[w * h]`. With bit 14 also set the 256 entry palette is left out and the entry uses the
/// one carried by the nearest earlier 8 bit entry of the chunk. Room textures are at most 128
/// wide, so neither bit is ever set by a 4 bit entry.
pub struct TextureEntry<'a> {
    /// Width in texels.
    pub width: u16,
    /// Height in texels.
    pub height: u16,
    /// The sky's 8 bit form.
    pub eight_bit: bool,
    /// An 8 bit entry that reuses the chunk's earlier palette.
    pub shared_clut: bool,
    /// The palette bytes (empty when shared).
    pub clut: &'a [u8],
    /// The pixel bytes.
    pub pixels: &'a [u8],
    /// The entry's total size in the blob.
    pub bytes: usize,
}

const TEXTURE_ENTRY_8BIT: u16 = 0x8000;
const TEXTURE_ENTRY_SHARED_CLUT: u16 = 0x4000;
const TEXTURE_ENTRY_WIDTH_MASK: u16 = 0x3fff;
const TEXTURE_CLUT_256_BYTES: usize = 512;

/// One bounded entry from an HLTX texture blob. A shared 8-bit palette is valid only after this
/// same blob uploaded its own palette (`shared_clut_ready`).
#[must_use]
pub fn parse_texture_entry(data: &[u8], shared_clut_ready: bool) -> Option<TextureEntry<'_>> {
    let header = data.get(..4)?;
    let flags = u16::from_le_bytes([header[0], header[1]]);
    let width = flags & TEXTURE_ENTRY_WIDTH_MASK;
    let height = u16::from_le_bytes([header[2], header[3]]);
    let eight_bit = flags & TEXTURE_ENTRY_8BIT != 0;
    let shared_clut = eight_bit && flags & TEXTURE_ENTRY_SHARED_CLUT != 0;
    // The 8-bit allocator owns exactly half a page per sky face.
    if eight_bit && (width != 128 || height != 128 || (shared_clut && !shared_clut_ready)) {
        return None;
    }
    let clut_bytes = if eight_bit {
        if shared_clut {
            0
        } else {
            TEXTURE_CLUT_256_BYTES
        }
    } else {
        32
    };
    let pixels_start = 4usize.checked_add(clut_bytes)?;
    let pixels_len = (width as usize).checked_mul(height as usize)? / if eight_bit { 1 } else { 2 };
    let bytes = pixels_start.checked_add(pixels_len)?;
    let entry = data.get(..bytes)?;
    Some(TextureEntry {
        width,
        height,
        eight_bit,
        shared_clut,
        clut: &entry[4..pixels_start],
        pixels: &entry[pixels_start..bytes],
        bytes,
    })
}

/// The allocators and the small state the uploads keep between calls. A port declares one
/// `static mut` of this and calls the methods through it.
pub struct VramState<const PAGES: usize> {
    atlas: TextureWindowAtlas<PAGES>,
    cluts: ClutRowAllocator<CLUT_ROWS>,
    /// Viewmodel-pool checkpoint: both allocators are `Copy`, so they are snapshotted once the
    /// map's map/HUD/sprite/enemy/default-weapon textures are all uploaded. Restoring frees
    /// exactly the switched-viewmodel textures loaded afterwards.
    checkpoint: Option<Checkpoint<PAGES>>,
    /// Where the room's 8 bit (sky) textures go. Each takes half of a whole texture page (128
    /// texels is 64 VRAM halfwords, the page's full width), so the atlas, which places 4 bit
    /// windows, hands over empty pages and two faces share one. Page (plus one) with its upper
    /// half still free; zero marks it unset, which keeps it in `.bss`.
    sky_open_page: u16,
    /// The shared 256 entry palette's CLUT word (never zero: the CLUT rows are at Y 480).
    sky_clut: u16,
}

impl<const PAGES: usize> VramState<PAGES> {
    const SEPARATE: () = assert!(
        !clut_block_hits_a_texture_page(PAGES),
        "a reachable CLUT slot overlaps a texture page"
    );

    /// Empty allocators.
    #[must_use]
    pub const fn new() -> Self {
        let () = Self::SEPARATE;
        Self {
            atlas: TextureWindowAtlas::new(),
            cluts: ClutRowAllocator::new(CLUT_BASE_Y),
            checkpoint: None,
            sky_open_page: 0,
            sky_clut: 0,
        }
    }

    /// Snapshot the allocators (call after all non-viewmodel textures are resident).
    pub fn checkpoint(&mut self) {
        self.checkpoint = Some((self.atlas, self.cluts));
    }

    /// Restore the allocators to the checkpoint, freeing every switched-viewmodel texture. No-op
    /// if no checkpoint was taken this map.
    pub fn restore(&mut self) {
        if let Some((a, c)) = self.checkpoint {
            self.atlas = a;
            self.cluts = c;
        }
    }

    /// The allocators as they are now, for a port that nests a second mark above the first.
    #[must_use]
    pub fn snapshot(&self) -> Checkpoint<PAGES> {
        (self.atlas, self.cluts)
    }

    /// Put the allocators back to `mark`.
    pub fn restore_from(&mut self, mark: Checkpoint<PAGES>) {
        self.atlas = mark.0;
        self.cluts = mark.1;
    }

    fn reset(&mut self) {
        self.atlas = TextureWindowAtlas::new();
        self.cluts = ClutRowAllocator::new(CLUT_BASE_Y);
        self.checkpoint = None; // invalidate the previous map's viewmodel checkpoint
        self.sky_open_page = 0;
        self.sky_clut = 0;
    }

    /// Upload a streamed map texture chunk: `magic "HLTX" | u32 n_texs | texture blob`, after
    /// resetting the allocators for the new map.
    ///
    /// Returns `(n_texs, failed_uploads)` so the caller can report the same counters as the
    /// legacy inline map-texture path.
    ///
    /// # Safety
    ///
    /// `slots` must be valid for `slot_len` writes.
    pub unsafe fn upload_tex_chunk_raw(
        &mut self,
        data: &[u8],
        slots: *mut TexSlot,
        slot_len: usize,
    ) -> Option<(usize, usize)> {
        let data = unsafe { canonical_ram_bytes(data) };
        if data.len() < 8 || data.get(0..4)? != b"HLTX" {
            return None;
        }
        let n_texs = rd_u32(data, 4)? as usize;
        self.reset();
        let failed = unsafe { self.upload_tex_blob_raw(&data[8..], n_texs, slots, slot_len) };
        Some((n_texs, failed))
    }

    /// Upload a streamed texture chunk without resetting the atlas. Use this for model textures
    /// after the room's material chunk has established the frame's VRAM allocation state.
    ///
    /// # Safety
    ///
    /// `slots` must be valid for `slot_len` writes.
    pub unsafe fn upload_tex_chunk_append_raw(
        &mut self,
        data: &[u8],
        slots: *mut TexSlot,
        slot_len: usize,
    ) -> Option<(usize, usize)> {
        let data = unsafe { canonical_ram_bytes(data) };
        if data.len() < 8 || data.get(0..4)? != b"HLTX" {
            return None;
        }
        let n_texs = rd_u32(data, 4)? as usize;
        let failed = unsafe { self.upload_tex_blob_raw(&data[8..], n_texs, slots, slot_len) };
        Some((n_texs, failed))
    }

    /// Upload only the textures selected by `used_mask`, compacting their slots and writing the
    /// old-to-new slot mapping into `remap`. This keeps studio bodygroups that are not present in
    /// the current map from consuming scarce texture slots or VRAM. Studio model chunks are
    /// limited to 32 textures when using this path so the selection and remap stay stack-cheap on
    /// PS1.
    ///
    /// # Safety
    ///
    /// `slots` must be valid for `slot_len` writes.
    pub unsafe fn upload_tex_chunk_append_masked_raw(
        &mut self,
        data: &[u8],
        slots: *mut TexSlot,
        slot_len: usize,
        used_mask: u32,
        remap: &mut [u8; 32],
    ) -> Option<(usize, usize)> {
        let data = unsafe { canonical_ram_bytes(data) };
        if data.len() < 8 || data.get(0..4)? != b"HLTX" {
            return None;
        }
        let n_texs = rd_u32(data, 4)? as usize;
        if n_texs > remap.len() {
            return None;
        }
        remap.fill(u8::MAX);
        let valid_mask = if n_texs == 32 {
            u32::MAX
        } else {
            (1u32 << n_texs) - 1
        };
        let selected = (used_mask & valid_mask).count_ones() as usize;
        if selected > slot_len {
            return None;
        }

        let slots = canonical_ram_mut(slots);
        let blob = &data[8..];
        let mut off = 0usize;
        let mut out = 0usize;
        let mut failed = 0usize;
        for i in 0..n_texs {
            if off + 36 > blob.len() {
                return None;
            }
            let w = u16::from_le_bytes([blob[off], blob[off + 1]]);
            let h = u16::from_le_bytes([blob[off + 2], blob[off + 3]]);
            let clut = &blob[off + 4..off + 36];
            let pix_len = (w as usize * h as usize) / 2;
            let pix_off = off + 36;
            if pix_off + pix_len > blob.len() {
                return None;
            }
            let pix = &blob[pix_off..pix_off + pix_len];
            off = pix_off + pix_len;
            if used_mask & (1u32 << i) == 0 {
                continue;
            }
            remap[i] = out as u8;
            let slot = match self.upload_one(w, h, clut, pix, false) {
                Some(s) => s,
                None => {
                    failed += 1;
                    EMPTY_SLOT
                }
            };
            unsafe {
                ptr::write(slots.add(out), slot);
            }
            out += 1;
        }
        Some((out, failed))
    }

    /// Raw-pointer variant for filling `static mut` slot tables without creating references to
    /// those statics. Returns the count that did not fit VRAM.
    ///
    /// # Safety
    ///
    /// `slots` must be valid for `slot_len` writes.
    #[optimize(size)]
    pub unsafe fn upload_tex_blob_raw(
        &mut self,
        data: &[u8],
        n_texs: usize,
        slots: *mut TexSlot,
        slot_len: usize,
    ) -> usize {
        let data = unsafe { canonical_ram_bytes(data) };
        let slots = canonical_ram_mut(slots);
        let mut off = 0usize;
        let mut failed = 0usize;
        let mut shared_clut_ready = false;
        // A following HLTX chunk must establish its own 8-bit palette.
        self.sky_clut = 0;
        for i in 0..n_texs {
            let Some(entry) = data
                .get(off..)
                .and_then(|remaining| parse_texture_entry(remaining, shared_clut_ready))
            else {
                failed += 1;
                break;
            };
            off += entry.bytes;
            if entry.eight_bit && !entry.shared_clut {
                shared_clut_ready = false;
                self.sky_clut = 0;
            }
            if i >= slot_len {
                continue;
            }
            let slot = match self.upload_one(
                entry.width,
                entry.height,
                entry.clut,
                entry.pixels,
                entry.eight_bit,
            ) {
                Some(s) => s,
                None => {
                    failed += 1;
                    EMPTY_SLOT
                }
            };
            if entry.eight_bit && !entry.shared_clut && slot.valid {
                shared_clut_ready = true;
            }
            unsafe {
                ptr::write(slots.add(i), slot);
            }
        }
        failed
    }

    /// Upload one texture: 4 bit into an atlas window, or (`eight`) an 8 bit sky face into half a
    /// page of its own. `clut_bytes` is empty for a sky face that shares the palette an earlier
    /// one uploaded.
    #[optimize(size)]
    fn upload_one(
        &mut self,
        w: u16,
        h: u16,
        clut_bytes: &[u8],
        pix: &[u8],
        eight: bool,
    ) -> Option<TexSlot> {
        let (page, u0, v0) = if eight {
            // The second face of a page goes under the first.
            if self.sky_open_page != 0 {
                let page = self.sky_open_page - 1;
                self.sky_open_page = 0;
                (page, 0, 128)
            } else {
                let page = self.atlas.reserve_empty_page()? as u16;
                self.sky_open_page = page + 1;
                (page, 0, 0)
            }
        } else {
            let pl = self.atlas.allocate(w, h)?;
            (pl.page_index(), pl.origin_u(), pl.origin_v())
        };
        let tpage_x = TEX_X0 + (page % COLS) * 64;
        let tpage_y = if page / COLS == 0 { 0 } else { 256 };
        let (depth, entries, per_halfword) = if eight {
            (TextureDepth::Bit8, 256, 2)
        } else {
            (TextureDepth::Bit4, 16, 4)
        };
        let tpage = TexturePage::new(tpage_x, tpage_y, depth);
        // 4-bit pixels pack 4 texels per VRAM halfword -> w/4 halfwords wide; 8-bit pixels pack 2.
        let vram_w = w / per_halfword;
        let vram_x = tpage_x + u0 as u16 / per_halfword;
        let vram_y = tpage_y + v0 as u16;
        if !rect_fits_vram(vram_x, vram_y, vram_w, h) {
            return None;
        }
        upload_bytes(VramRect::new(vram_x, vram_y, vram_w, h), pix);

        let clut_word = if clut_bytes.is_empty() {
            self.sky_clut
        } else {
            let clut = loop {
                let candidate = self.cluts.alloc(entries)?;
                if candidate.x() + entries <= CLUT_MAX_X {
                    break candidate;
                }
                // Leaving the rest of the row marked occupied retires it, so the next allocation
                // continues on the following row. Everything at or past `CLUT_MAX_X` belongs to a
                // texture page or the fixed HUD page.
            };
            if !rect_fits_vram(clut.x(), clut.y(), entries, 1) {
                return None;
            }
            upload_bytes(VramRect::new(clut.x(), clut.y(), entries, 1), clut_bytes);
            if eight {
                self.sky_clut = clut.uv_word();
            }
            clut.uv_word()
        };
        if clut_word == 0 {
            return None;
        }

        let win = TextureWindow::power_of_two_tile(u0, v0, w as u8, h as u8);
        let material = TextureMaterial::opaque(clut_word, tpage.uv_word(0), (128, 128, 128))
            .with_texture_window(win);
        let packet = TexturedGouraudPacketMaterial::from_texture(material);
        // Every texel index 0 -> a solid single-colour fill (the `black` backdrop, now
        // grey-lifted by the cook). Flag it for OT back-biasing.
        let backdrop = !pix.is_empty() && pix.iter().all(|&b| b == 0);
        Some(TexSlot {
            material,
            packet,
            valid: true,
            backdrop,
        })
    }
}

impl<const PAGES: usize> Default for VramState<PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

#[inline(always)]
fn rd_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *d.get(o)?,
        *d.get(o + 1)?,
        *d.get(o + 2)?,
        *d.get(o + 3)?,
    ]))
}

#[inline(always)]
fn canonical_ram_const<T>(p: *const T) -> *const T {
    #[cfg(target_arch = "mips")]
    {
        (((p as usize) & 0x001f_ffff) | 0x8000_0000) as *const T
    }
    #[cfg(not(target_arch = "mips"))]
    {
        p
    }
}

#[inline(always)]
fn canonical_ram_mut<T>(p: *mut T) -> *mut T {
    canonical_ram_const(p as *const T) as *mut T
}

#[inline(always)]
unsafe fn canonical_ram_bytes(data: &[u8]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(canonical_ram_const(data.as_ptr()), data.len()) }
}

#[inline(always)]
fn rect_fits_vram(x: u16, y: u16, w: u16, h: u16) -> bool {
    w > 0
        && h > 0
        && (x as u32 + w as u32) <= VRAM_W as u32
        && (y as u32 + h as u32) <= VRAM_H as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_entry_bounds_the_full_palette_and_pixels() {
        let mut sky = [0u8; 4 + TEXTURE_CLUT_256_BYTES + 128 * 128];
        sky[..2].copy_from_slice(&(128 | TEXTURE_ENTRY_8BIT).to_le_bytes());
        sky[2..4].copy_from_slice(&128u16.to_le_bytes());
        assert!(parse_texture_entry(&sky[..36], false).is_none());
        assert!(parse_texture_entry(&sky[..515], false).is_none());
        assert!(parse_texture_entry(&sky[..sky.len() - 1], false).is_none());
        let parsed = parse_texture_entry(&sky, false).unwrap();
        assert_eq!(parsed.clut.len(), 512);
        assert_eq!(parsed.pixels.len(), 128 * 128);
        assert_eq!(parsed.bytes, sky.len());
    }

    #[test]
    fn sky_entry_rejects_page_overlap_and_foreign_shared_palette() {
        let mut sky = [0u8; 4 + TEXTURE_CLUT_256_BYTES + 128 * 128];
        sky[..2].copy_from_slice(&(256 | TEXTURE_ENTRY_8BIT).to_le_bytes());
        sky[2..4].copy_from_slice(&128u16.to_le_bytes());
        assert!(parse_texture_entry(&sky, false).is_none());
        sky[..2].copy_from_slice(&(128 | TEXTURE_ENTRY_8BIT).to_le_bytes());
        sky[2..4].copy_from_slice(&129u16.to_le_bytes());
        assert!(parse_texture_entry(&sky, false).is_none());

        sky[..2]
            .copy_from_slice(&(128 | TEXTURE_ENTRY_8BIT | TEXTURE_ENTRY_SHARED_CLUT).to_le_bytes());
        sky[2..4].copy_from_slice(&128u16.to_le_bytes());
        let shared = &sky[..4 + 128 * 128];
        assert!(parse_texture_entry(shared, false).is_none());
        assert!(parse_texture_entry(shared, true).unwrap().clut.is_empty());
    }

    #[test]
    fn no_page_count_a_port_picks_lets_the_clut_block_touch_a_page() {
        assert!(!clut_block_hits_a_texture_page(21));
        assert!(!clut_block_hits_a_texture_page(20));
    }
}
