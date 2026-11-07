use super::PlaytestPackage;

/// Warning-only, camera-independent figures for cooked playtest work.
///
/// `room_surfaces` is the largest room's cooked surface count and
/// `authored_triangles` the whole level's, both workload comparators rather
/// than emitted-primitive bounds: what a frame draws depends on the camera.
/// `prop_surfaces` counts the specialized prop surfaces every frame may draw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaytestPerformanceEnvelope {
    pub room_surfaces: usize,
    pub authored_triangles: usize,
    pub prop_surfaces: usize,
}

pub fn playtest_performance_envelope(package: &PlaytestPackage) -> PlaytestPerformanceEnvelope {
    let world = &package.world_geometry;
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

    PlaytestPerformanceEnvelope {
        room_surfaces: world.max_room_surfaces,
        authored_triangles: world.triangles,
        prop_surfaces,
    }
}
