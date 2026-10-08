use super::PlaytestPackage;

fn pxbsp_surface_counts(package: &PlaytestPackage) -> Result<(usize, usize), String> {
    let world = &package.world_geometry;
    let mut map = psx_bsp::pxbsp_resident::PxbspResidentMap::with_capacity(world.bytes.len());
    map.load(0, &mut psx_bsp::SliceReader::new(&world.bytes))
        .map_err(|error| format!("performance envelope: invalid resident PXBSP: {error}"))?;
    let faces = map.faces().len();
    let triangles = map.faces().iter().fold(0usize, |total, face| {
        total.saturating_add((face.vertex_count.max(0) as usize).saturating_sub(2))
    });
    Ok((faces, triangles))
}

/// Warning-only, camera-independent upper envelope for cooked playtest work.
///
/// `room_surfaces` is a content guard: a recorded route must never exceed it.
/// `authored_triangles` is a workload comparator, not an emitted-primitive
/// bound. Likewise, `tr_packets_before_hw_split` is a planning figure for the
/// fixed one-level TR path, not a hard packet bound, because the
/// hardware-extent fallback can split a child further.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytestPerformanceEnvelope {
    pub max_single_room_pvs_surfaces: usize,
    pub room_surfaces: usize,
    pub authored_triangles: usize,
    pub tr_packets_before_hw_split: usize,
    pub prop_surfaces: usize,
}

pub fn playtest_performance_envelope(
    package: &PlaytestPackage,
) -> Result<PlaytestPerformanceEnvelope, String> {
    if package.rooms.is_empty() {
        return Ok(PlaytestPerformanceEnvelope::default());
    }

    let (room_surfaces, authored_triangles) = pxbsp_surface_counts(package)?;
    let mut prop_surfaces = package.image_props.len();
    for prop in &package.box_props {
        prop_surfaces = prop_surfaces.saturating_add(if prop.surface_count == 0 {
            psx_level::BOX_PROP_FACE_COUNT
        } else {
            usize::from(prop.surface_count)
        });
    }
    for prop in &package.cylinder_props {
        prop_surfaces = prop_surfaces.saturating_add(usize::from(prop.surface_count));
    }
    for prop in &package.arch_props {
        prop_surfaces = prop_surfaces.saturating_add(usize::from(prop.surface_count));
    }

    Ok(PlaytestPerformanceEnvelope {
        max_single_room_pvs_surfaces: room_surfaces,
        room_surfaces,
        authored_triangles,
        // One-level TR emits four children plus one crack-cover packet.
        // Hardware-extent splitting is reported separately by the runtime.
        tr_packets_before_hw_split: room_surfaces.saturating_mul(5),
        prop_surfaces,
    })
}
