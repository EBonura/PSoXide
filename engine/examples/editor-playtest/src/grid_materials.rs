//! Binding a grid room's materials to VRAM: the room's `MATERIAL_USE` table
//! names level materials, each resolves to a texture slot through the
//! playtest VRAM owner, and the room is drawable once all of them are ready.

use alloc::vec::Vec;
use psx_asset::Texture;
use psx_gpu::material::TextureWindow;
use psx_grid::render::GridMaterial;
use psx_grid::{LevelView, ResidentRooms};
use psx_level::{find_asset_of_kind, AssetId, AssetKind};

use crate::generated::ASSETS;
use crate::{ensure_room_texture_uploaded, ensure_texture_uploaded};

/// Resolve one level material to its VRAM binding. Returns `None` while the
/// texture upload is still queued.
fn resolve_material(material: &psx_grid::Material) -> Option<GridMaterial> {
    let asset_id = AssetId(material.texture_asset);
    let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture).unwrap_or_else(|| {
        panic!(
            "grid material references missing texture asset {}",
            material.texture_asset
        )
    });
    assert!(
        !asset.bytes.is_empty(),
        "grid texture asset {} has no baked bytes",
        material.texture_asset
    );
    let texture = Texture::from_bytes(asset.bytes).unwrap_or_else(|_| {
        panic!(
            "grid texture asset {} is not a valid PSXT",
            material.texture_asset
        )
    });
    let slot = if texture.is_index_zero_transparent() || material.blend != 0 {
        ensure_texture_uploaded(asset_id, asset.bytes)
    } else {
        ensure_room_texture_uploaded(asset_id, asset.bytes)
    }?;
    if !slot.ready {
        return None;
    }
    let [u, v] = slot.texture_window.origin_texels();
    // Blend code in tpage bits 5-6: average 0, add 1, subtract 2, add-quarter 3.
    let (blend_bits, command_bits) = match material.blend {
        1 => (0u16, 0x0200_0000u32),
        2 => (1 << 5, 0x0200_0000),
        3 => (2 << 5, 0x0200_0000),
        4 => (3 << 5, 0x0200_0000),
        _ => (0, 0),
    };
    Some(GridMaterial {
        tpage: (slot.tpage_word & !0x0060) | blend_bits,
        clut: slot.clut_word,
        uv_origin: u16::from(u) | (u16::from(v) << 8),
        command_bits,
        // A tile spans 64 page-local texels; the packed add in the packet
        // writer must not carry between bytes.
        resident: u16::from(u) + 64 <= 256 && u16::from(v) + 64 <= 256,
        // The window that wraps raw UVs back onto this texture. A texture
        // without a window placement (a whole page) cannot repeat; its
        // `REPEAT` quads are skipped, not drawn wrong.
        window_word: if slot.texture_window == TextureWindow::NONE {
            0
        } else {
            slot.texture_window.word()
        },
    })
}

/// A level material wanted by a room that is not ready yet, and its binding
/// once the texture is resident.
#[derive(Clone, Copy, Default)]
pub(super) struct Pending {
    needed: bool,
    binding: Option<GridMaterial>,
}

/// Bind the materials of every resident room that has none yet. Each level
/// material the pending rooms use is resolved once, in level order (the order
/// the textures are queued for upload and given VRAM slots in, whichever
/// rooms ask for them), then copied into every room's own table: entry `i`
/// of a slot's table is the binding of its `MATERIAL_USE[i]`. A room is
/// ready, and drawn, once every entry is; an upload still queued leaves it
/// unready and the next call tries again. Returns whether every resident
/// room is ready.
pub(super) fn bind_pending(
    level: &LevelView<'static>,
    resident: &mut ResidentRooms<'static>,
    scratch: &mut Vec<Pending>,
) -> bool {
    scratch.clear();
    scratch.resize(level.materials.len(), Pending::default());
    let mut any = false;
    for slot in resident.iter().filter(|slot| !slot.materials_ready) {
        any = true;
        for &id in slot.render.map_or(&[][..], |render| render.material_use) {
            if let Some(entry) = scratch.get_mut(usize::from(id)) {
                entry.needed = true;
            }
        }
    }
    if !any {
        return true;
    }
    for (material, entry) in level.materials.iter().zip(scratch.iter_mut()) {
        if entry.needed {
            entry.binding = resolve_material(material);
        }
    }
    let mut all_ready = true;
    for slot in resident.iter_mut().filter(|slot| !slot.materials_ready) {
        let Some(render) = slot.render else {
            all_ready = false;
            continue;
        };
        let used = render.material_use;
        slot.materials.clear();
        let mut ready = true;
        for &id in used {
            match scratch.get(usize::from(id)).and_then(|entry| entry.binding) {
                Some(bound) => slot.materials.push(bound),
                None => {
                    slot.materials.push(GridMaterial::default());
                    ready = false;
                }
            }
        }
        slot.materials_ready = ready;
        all_ready &= ready;
    }
    all_ready
}
