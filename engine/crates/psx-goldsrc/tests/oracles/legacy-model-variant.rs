fn map_model_variant_chunk(map_index: usize, ty: usize) -> Option<u32> {
    let offsets = &room_budget::MODEL_VARIANT_OFFSETS;
    if map_index + 1 >= offsets.len() || ty >= N_MODEL_TYPES {
        return None;
    }
    let mut entry = offsets[map_index] as usize;
    let end = offsets[map_index + 1] as usize;
    while entry < end {
        let offset = entry * 3;
        let bytes = &room_budget::MODEL_VARIANT_BYTES;
        if offset + 2 >= bytes.len() {
            return None;
        }
        let entry_ty = bytes[offset] as usize;
        if entry_ty == ty {
            return Some(u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]) as u32);
        }
        if entry_ty > ty {
            return None;
        }
        entry += 1;
    }
    None
}
