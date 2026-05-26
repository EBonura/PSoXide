//! Small VRAM upload helpers for editor-playtest.

use super::*;

/// Stamp the 0x8000 (semi-transparency-disable) bit on every
/// non-zero CLUT entry so opaque textures don't accidentally
/// trigger STP-bit blending.
pub(crate) fn upload_clut(rect: VramRect, bytes: &[u8]) {
    upload_clut_with_mode(rect, bytes, false);
}

/// Upload a CLUT for room/world materials. Imported room textures
/// are opaque until the material system grows an explicit alpha
/// control, so palette entry 0 must not punch holes in geometry.
pub(crate) fn upload_opaque_clut(rect: VramRect, bytes: &[u8]) {
    upload_clut_with_mode(rect, bytes, true);
}

/// Upload a CLUT for 8bpp model atlases. New alpha-aware atlases can
/// reserve palette index 0 for transparent gutter texels; legacy
/// atlases keep their old fully-opaque behaviour.
pub(crate) fn upload_model_clut(rect: VramRect, bytes: &[u8], transparent_index_zero: bool) {
    let mut marked = [0u8; 512];
    if bytes.len() > marked.len() || !bytes.len().is_multiple_of(2) {
        return;
    }

    let mut i = 0;
    while i < bytes.len() {
        let raw = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let index = i / 2;
        let pair = model_clut_entry_for_upload(index, raw, transparent_index_zero).to_le_bytes();
        marked[i] = pair[0];
        marked[i + 1] = pair[1];
        i += 2;
    }

    upload_bytes(rect, &marked[..bytes.len()]);
}

pub(crate) const fn model_clut_entry_for_upload(
    index: usize,
    raw: u16,
    transparent_index_zero: bool,
) -> u16 {
    if transparent_index_zero && index == 0 && raw == 0 {
        0
    } else {
        raw | 0x8000
    }
}

fn upload_clut_with_mode(rect: VramRect, bytes: &[u8], force_zero_opaque: bool) {
    let mut marked = [0u8; 512];
    if bytes.len() > marked.len() || !bytes.len().is_multiple_of(2) {
        return;
    }

    let mut i = 0;
    while i < bytes.len() {
        let raw = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let stamped = if raw == 0 && !force_zero_opaque {
            0
        } else {
            raw | 0x8000
        };
        let pair = stamped.to_le_bytes();
        marked[i] = pair[0];
        marked[i + 1] = pair[1];
        i += 2;
    }

    upload_bytes(rect, &marked[..bytes.len()]);
}
