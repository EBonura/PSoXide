use super::{streamed_room_chunk_memory_report, PlaytestPackage};

/// Warning-only, camera-independent upper envelope for cooked playtest work.
///
/// This deliberately combines the heaviest `visible_chunk_limit` rooms even
/// when that exact combination may not be reachable through the portal graph.
/// It is therefore safe as a room-surface content guard: a recorded route must
/// never exceed `room_surfaces`. `authored_triangles` is a workload comparator,
/// not an emitted-primitive bound. Likewise, `tr_packets_before_hw_split` is a
/// planning figure for the fixed one-level TR path, not a hard packet bound,
/// because the hardware-extent fallback can split a child further.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytestPerformanceEnvelope {
    pub visible_room_limit: usize,
    pub resident_room_limit: usize,
    pub max_single_room_pvs_surfaces: usize,
    pub room_surfaces: usize,
    pub authored_triangles: usize,
    pub tr_packets_before_hw_split: usize,
    pub prop_surfaces: usize,
    pub resident_payload_bytes: usize,
    pub resident_stream_bytes: usize,
}

pub fn playtest_performance_envelope(
    package: &PlaytestPackage,
) -> Result<PlaytestPerformanceEnvelope, String> {
    let room_count = package.rooms.len();
    if room_count == 0 {
        return Ok(PlaytestPerformanceEnvelope::default());
    }

    let visible_room_limit = package
        .rooms
        .iter()
        .map(|room| usize::from(room.visible_chunk_limit.max(1)))
        .max()
        .unwrap_or(1)
        .min(room_count);
    let resident_room_limit = package
        .rooms
        .iter()
        .map(|room| usize::from(room.resident_chunk_limit.max(1)))
        .max()
        .unwrap_or(1)
        .min(room_count);

    let mut pvs_surfaces = Vec::with_capacity(room_count);
    let mut authored_triangles = vec![0usize; room_count];
    let mut prop_surfaces = vec![0usize; room_count];
    for chunk in &package.chunks {
        if let Some(total) = authored_triangles.get_mut(chunk.room as usize) {
            *total = total.saturating_add(chunk.triangles);
        }
    }
    for prop in &package.image_props {
        if let Some(total) = prop_surfaces.get_mut(prop.room as usize) {
            *total = total.saturating_add(1);
        }
    }
    for prop in &package.box_props {
        if let Some(total) = prop_surfaces.get_mut(prop.room as usize) {
            let surfaces = if prop.surface_count == 0 {
                psx_level::BOX_PROP_FACE_COUNT
            } else {
                usize::from(prop.surface_count)
            };
            *total = total.saturating_add(surfaces);
        }
    }
    for prop in &package.cylinder_props {
        if let Some(total) = prop_surfaces.get_mut(prop.room as usize) {
            *total = total.saturating_add(usize::from(prop.surface_count));
        }
    }
    for prop in &package.arch_props {
        if let Some(total) = prop_surfaces.get_mut(prop.room as usize) {
            *total = total.saturating_add(usize::from(prop.surface_count));
        }
    }

    for room in 0..room_count {
        pvs_surfaces.push(max_room_pvs_surfaces(package, room)?);
    }
    let max_single_room_pvs_surfaces = pvs_surfaces.iter().copied().max().unwrap_or_default();
    let room_surfaces = sum_largest(&mut pvs_surfaces, visible_room_limit);
    let authored_triangles = sum_largest(&mut authored_triangles, visible_room_limit);
    let prop_surfaces = sum_largest(&mut prop_surfaces, visible_room_limit);

    let stream = streamed_room_chunk_memory_report(package)?;
    let mut payloads = stream
        .chunks
        .iter()
        .map(|chunk| chunk.payload_bytes)
        .collect::<Vec<_>>();
    let mut stream_bytes = stream
        .chunks
        .iter()
        .map(|chunk| chunk.stream_bytes)
        .collect::<Vec<_>>();

    Ok(PlaytestPerformanceEnvelope {
        visible_room_limit,
        resident_room_limit,
        max_single_room_pvs_surfaces,
        room_surfaces,
        authored_triangles,
        // One-level TR emits four children plus one crack-cover packet.
        // Hardware-extent splitting is reported separately by the runtime.
        tr_packets_before_hw_split: room_surfaces.saturating_mul(5),
        prop_surfaces,
        resident_payload_bytes: sum_largest(&mut payloads, resident_room_limit),
        resident_stream_bytes: sum_largest(&mut stream_bytes, resident_room_limit),
    })
}

fn max_room_pvs_surfaces(package: &PlaytestPackage, room: usize) -> Result<usize, String> {
    let cache = package
        .room_surface_caches
        .iter()
        .find(|cache| cache.room as usize == room)
        .ok_or_else(|| format!("performance envelope: room {room} has no surface cache"))?;
    let cache_first = cache.cell_first as usize;
    let cache_end = cache_first
        .checked_add(cache.cell_count as usize)
        .ok_or_else(|| format!("performance envelope: room {room} cache range overflow"))?;
    let cache_cells = package
        .room_cache_cells
        .get(cache_first..cache_end)
        .ok_or_else(|| format!("performance envelope: room {room} cache range is invalid"))?;

    let Some(visibility) = package
        .room_visibility
        .iter()
        .find(|visibility| visibility.room as usize == room)
    else {
        return Ok(cache.surface_count as usize);
    };
    if visibility.pvs_count == 0 {
        return Ok(cache.surface_count as usize);
    }
    let cell_first = visibility.cell_first as usize;
    let cell_end = cell_first
        .checked_add(visibility.cell_count as usize)
        .ok_or_else(|| format!("performance envelope: room {room} visibility range overflow"))?;
    let cells = package
        .visibility_cells
        .get(cell_first..cell_end)
        .ok_or_else(|| format!("performance envelope: room {room} visibility range is invalid"))?;
    let pvs_first = visibility.pvs_first as usize;
    let pvs_end = pvs_first
        .checked_add(visibility.pvs_count as usize)
        .ok_or_else(|| format!("performance envelope: room {room} PVS range overflow"))?;
    let pvs_records = package
        .visibility_pvs
        .get(pvs_first..pvs_end)
        .ok_or_else(|| format!("performance envelope: room {room} PVS range is invalid"))?;

    let mut maximum = 0usize;
    for pvs in pvs_records {
        let byte_first = pvs.byte_first as usize;
        let byte_end = byte_first
            .checked_add(pvs.byte_count as usize)
            .ok_or_else(|| format!("performance envelope: room {room} PVS bits overflow"))?;
        let bits = package
            .visibility_pvs_bits
            .get(byte_first..byte_end)
            .ok_or_else(|| format!("performance envelope: room {room} PVS bits are invalid"))?;
        let mut surfaces = 0usize;
        for (index, cell) in cells.iter().enumerate() {
            if bits
                .get(index / 8)
                .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
            {
                let cache_cell =
                    cache_cells
                        .get(cell.cache_cell_index as usize)
                        .ok_or_else(|| {
                            format!(
                            "performance envelope: room {room} cell {} references cache cell {}",
                            index, cell.cache_cell_index
                        )
                        })?;
                surfaces = surfaces.saturating_add(cache_cell.surface_count as usize);
            }
        }
        maximum = maximum.max(surfaces);
    }
    Ok(maximum)
}

fn sum_largest(values: &mut [usize], count: usize) -> usize {
    values.sort_unstable_by(|a, b| b.cmp(a));
    values
        .iter()
        .take(count)
        .fold(0usize, |sum, value| sum.saturating_add(*value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playtest::{
        PlaytestCachedRoomCell, PlaytestRoomSurfaceCache, PlaytestRoomVisibility,
        PlaytestVisibilityCell, PlaytestVisibilityPvs,
    };

    #[test]
    fn pvs_envelope_sums_only_set_cell_surfaces() {
        let mut package = PlaytestPackage::default();
        package.room_surface_caches.push(PlaytestRoomSurfaceCache {
            room: 0,
            cell_first: 0,
            cell_count: 2,
            cell_vertex_first: 0,
            cell_vertex_count: 0,
            vertex_first: 0,
            vertex_count: 0,
            surface_first: 0,
            surface_count: 10,
        });
        package
            .room_cache_cells
            .extend([sample_cache_cell(3), sample_cache_cell(7)]);
        package.room_visibility.push(PlaytestRoomVisibility {
            room: 0,
            cell_first: 0,
            cell_count: 2,
            pvs_first: 0,
            pvs_count: 2,
        });
        package
            .visibility_cells
            .extend([sample_visibility_cell(0), sample_visibility_cell(1)]);
        package.visibility_pvs.extend([
            PlaytestVisibilityPvs {
                byte_first: 0,
                byte_count: 1,
            },
            PlaytestVisibilityPvs {
                byte_first: 1,
                byte_count: 1,
            },
        ]);
        package.visibility_pvs_bits.extend([0b01, 0b11]);

        assert_eq!(max_room_pvs_surfaces(&package, 0).unwrap(), 10);
    }

    fn sample_cache_cell(surface_count: u16) -> PlaytestCachedRoomCell {
        PlaytestCachedRoomCell {
            x: 0,
            z: 0,
            min_y: 0,
            max_y: 0,
            visibility_center: [0; 3],
            visibility_radius: 0,
            surface_first: 0,
            surface_count,
            vertex_first: 0,
            vertex_count: 0,
        }
    }

    fn sample_visibility_cell(cache_cell_index: u16) -> PlaytestVisibilityCell {
        PlaytestVisibilityCell {
            room: 0,
            x: 0,
            z: 0,
            min_y: 0,
            max_y: 0,
            portal_mask: 0,
            blocker_mask: 0,
            cache_cell_index,
            flags: 0,
        }
    }
}
