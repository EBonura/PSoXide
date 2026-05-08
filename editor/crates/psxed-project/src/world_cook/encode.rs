//! Binary `.psxw` encoding for cooked world grids.

use super::*;

pub(super) fn encode_cooked_world_grid_psxw(
    cooked: &CookedWorldGrid,
) -> Result<Vec<u8>, WorldGridCookError> {
    if cooked.sectors.len() > u16::MAX as usize {
        return Err(WorldGridCookError::TooManySectors {
            count: cooked.sectors.len(),
        });
    }
    if cooked.materials.len() > u16::MAX as usize {
        return Err(WorldGridCookError::TooManyMaterials {
            count: cooked.materials.len(),
        });
    }

    let mut sector_records = Vec::with_capacity(cooked.sectors.len() * world::SectorRecord::SIZE);
    let mut wall_records = Vec::new();
    let mut horizontal_override_records = Vec::new();

    for (sector_index, sector) in cooked.sectors.iter().enumerate() {
        let first_wall_index = wall_records.len() / world::WallRecord::SIZE;
        let first_wall = checked_u16(
            first_wall_index,
            WorldGridCookError::TooManyWalls {
                count: first_wall_index,
            },
        )?;
        let wall_start = wall_records.len() / world::WallRecord::SIZE;
        if let Some(sector) = sector {
            encode_sector_walls(sector, &mut wall_records)?;
        }
        let wall_end = wall_records.len() / world::WallRecord::SIZE;
        let wall_count = checked_u16(
            wall_end - wall_start,
            WorldGridCookError::TooManyWalls {
                count: wall_end - wall_start,
            },
        )?;
        if let Some(sector) = sector {
            encode_sector_horizontal_overrides(
                sector_index,
                sector,
                &mut horizontal_override_records,
            )?;
        }
        encode_sector_record(sector.as_ref(), first_wall, wall_count, &mut sector_records);
    }

    let wall_record_count = wall_records.len() / world::WallRecord::SIZE;
    let horizontal_override_count = checked_u16(
        horizontal_override_records.len() / world::HorizontalOverrideRecord::SIZE,
        WorldGridCookError::EncodedWorldTooLarge {
            bytes: horizontal_override_records.len(),
        },
    )?;
    let surface_light_records = if cooked.static_vertex_lighting {
        encode_surface_lights(cooked)
    } else {
        Vec::new()
    };
    let surface_light_count = checked_u16(
        surface_light_records.len() / world::SurfaceLightRecord::SIZE,
        WorldGridCookError::EncodedWorldTooLarge {
            bytes: surface_light_records.len(),
        },
    )?;

    let payload_len = world::WorldHeader::SIZE
        + sector_records.len()
        + wall_records.len()
        + horizontal_override_records.len()
        + surface_light_records.len();
    if payload_len > u32::MAX as usize {
        return Err(WorldGridCookError::EncodedWorldTooLarge { bytes: payload_len });
    }

    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    out.extend_from_slice(&world::MAGIC);
    out.extend_from_slice(&world::VERSION.to_le_bytes());
    out.extend_from_slice(&world::flags::RESERVED.to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());

    out.extend_from_slice(&cooked.width.to_le_bytes());
    out.extend_from_slice(&cooked.depth.to_le_bytes());
    out.extend_from_slice(&cooked.sector_size.to_le_bytes());
    out.extend_from_slice(&(cooked.sectors.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cooked.materials.len() as u16).to_le_bytes());
    out.extend_from_slice(&(wall_record_count as u16).to_le_bytes());
    out.extend_from_slice(&cooked.ambient_color);
    let mut world_flags = 0u8;
    if cooked.fog_enabled {
        world_flags |= world::world_flags::FOG_ENABLED;
    }
    if cooked.static_vertex_lighting {
        world_flags |= world::world_flags::STATIC_VERTEX_LIGHTING;
    }
    out.push(world_flags);
    out.extend_from_slice(&surface_light_count.to_le_bytes());
    out.extend_from_slice(&horizontal_override_count.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&sector_records);
    out.extend_from_slice(&wall_records);
    out.extend_from_slice(&horizontal_override_records);
    out.extend_from_slice(&surface_light_records);
    Ok(out)
}

fn checked_u16(value: usize, error: WorldGridCookError) -> Result<u16, WorldGridCookError> {
    if value > u16::MAX as usize {
        Err(error)
    } else {
        Ok(value as u16)
    }
}

fn encode_sector_record(
    sector: Option<&CookedGridSector>,
    first_wall: u16,
    wall_count: u16,
    out: &mut Vec<u8>,
) {
    let mut flags = 0u8;
    let mut floor_split = world::split::NORTH_WEST_SOUTH_EAST;
    let mut ceiling_split = world::split::NORTH_WEST_SOUTH_EAST;
    let mut floor_material = world::NO_MATERIAL;
    let mut ceiling_material = world::NO_MATERIAL;
    let mut floor_heights = [0; 4];
    let mut ceiling_heights = [0; 4];
    let mut floor_uvs = world::FLOOR_UVS;
    let mut ceiling_uvs = world::FLOOR_UVS;

    if let Some(sector) = sector {
        if let Some(floor) = sector.floor {
            flags |= world::sector_flags::HAS_FLOOR;
            if floor.walkable {
                flags |= world::sector_flags::FLOOR_WALKABLE;
            }
            floor_split = floor.split.psxw_id();
            floor_material = floor.material;
            floor_heights = floor.heights;
            floor_uvs = floor.uvs;
        }
        if let Some(ceiling) = sector.ceiling {
            flags |= world::sector_flags::HAS_CEILING;
            if ceiling.walkable {
                flags |= world::sector_flags::CEILING_WALKABLE;
            }
            ceiling_split = ceiling.split.psxw_id();
            ceiling_material = ceiling.material;
            ceiling_heights = ceiling.heights;
            ceiling_uvs = ceiling.uvs;
        }
    }

    out.push(flags);
    out.push(floor_split);
    out.push(ceiling_split);
    out.push(0);
    out.extend_from_slice(&floor_material.to_le_bytes());
    out.extend_from_slice(&ceiling_material.to_le_bytes());
    out.extend_from_slice(&first_wall.to_le_bytes());
    out.extend_from_slice(&wall_count.to_le_bytes());
    for height in floor_heights {
        out.extend_from_slice(&height.to_le_bytes());
    }
    for height in ceiling_heights {
        out.extend_from_slice(&height.to_le_bytes());
    }
    encode_uvs(floor_uvs, out);
    encode_uvs(ceiling_uvs, out);
}

fn encode_sector_horizontal_overrides(
    sector_index: usize,
    sector: &CookedGridSector,
    out: &mut Vec<u8>,
) -> Result<(), WorldGridCookError> {
    let sector_index = checked_u16(
        sector_index,
        WorldGridCookError::TooManySectors {
            count: sector_index,
        },
    )?;
    if let Some(floor) = sector.floor {
        if horizontal_face_requires_override(floor) {
            encode_horizontal_override(sector_index, world::horizontal_surface::FLOOR, floor, out);
        }
    }
    if let Some(ceiling) = sector.ceiling {
        if horizontal_face_requires_override(ceiling) {
            encode_horizontal_override(
                sector_index,
                world::horizontal_surface::CEILING,
                ceiling,
                out,
            );
        }
    }
    Ok(())
}

fn horizontal_face_requires_override(face: CookedGridHorizontalFace) -> bool {
    let [a, b] = face.triangles;
    !a.visible
        || !b.visible
        || a.material != face.material
        || b.material != face.material
        || a.uvs != face.uvs
        || b.uvs != face.uvs
        || a.walkable != face.walkable
        || b.walkable != face.walkable
}

fn encode_horizontal_override(
    sector_index: u16,
    surface: u8,
    face: CookedGridHorizontalFace,
    out: &mut Vec<u8>,
) {
    out.extend_from_slice(&sector_index.to_le_bytes());
    out.push(surface);
    out.push(horizontal_flags(face.triangles));
    out.extend_from_slice(&face.triangles[0].material.to_le_bytes());
    out.extend_from_slice(&face.triangles[1].material.to_le_bytes());
    encode_uvs(face.triangles[0].uvs, out);
    encode_uvs(face.triangles[1].uvs, out);
}

fn horizontal_flags(triangles: [CookedGridHorizontalTriangle; 2]) -> u8 {
    let mut flags = 0u8;
    if triangles[0].visible {
        flags |= world::horizontal_flags::TRI_A_PRESENT;
    }
    if triangles[1].visible {
        flags |= world::horizontal_flags::TRI_B_PRESENT;
    }
    if triangles[0].visible && triangles[0].walkable {
        flags |= world::horizontal_flags::TRI_A_WALKABLE;
    }
    if triangles[1].visible && triangles[1].walkable {
        flags |= world::horizontal_flags::TRI_B_WALKABLE;
    }
    flags
}

fn encode_sector_walls(
    sector: &CookedGridSector,
    out: &mut Vec<u8>,
) -> Result<(), WorldGridCookError> {
    for (direction, walls) in [
        (GridDirection::North, sector.walls.north.as_slice()),
        (GridDirection::East, sector.walls.east.as_slice()),
        (GridDirection::South, sector.walls.south.as_slice()),
        (GridDirection::West, sector.walls.west.as_slice()),
        (
            GridDirection::NorthWestSouthEast,
            sector.walls.north_west_south_east.as_slice(),
        ),
        (
            GridDirection::NorthEastSouthWest,
            sector.walls.north_east_south_west.as_slice(),
        ),
    ] {
        for wall in walls {
            if out.len() / world::WallRecord::SIZE >= u16::MAX as usize {
                return Err(WorldGridCookError::TooManyWalls {
                    count: (out.len() / world::WallRecord::SIZE) + 1,
                });
            }
            out.push(direction_id(direction));
            out.push(if wall.solid {
                world::wall_flags::SOLID
            } else {
                0
            });
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&wall.material.to_le_bytes());
            out.extend_from_slice(&wall.shape.to_le_bytes());
            for height in wall.heights {
                out.extend_from_slice(&height.to_le_bytes());
            }
            encode_uvs(wall.uvs, out);
        }
    }
    Ok(())
}

fn encode_surface_lights(cooked: &CookedWorldGrid) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        (cooked.sectors.len() * 2 + cooked.wall_count() as usize) * world::SurfaceLightRecord::SIZE,
    );
    for sector in &cooked.sectors {
        encode_light(
            sector
                .as_ref()
                .and_then(|sector| sector.floor.map(|face| face.baked_vertex_rgb))
                .unwrap_or(DEFAULT_BAKED_VERTEX_RGB),
            &mut out,
        );
        encode_light(
            sector
                .as_ref()
                .and_then(|sector| sector.ceiling.map(|face| face.baked_vertex_rgb))
                .unwrap_or(DEFAULT_BAKED_VERTEX_RGB),
            &mut out,
        );
    }
    for sector in cooked.sectors.iter().flatten() {
        for walls in [
            sector.walls.north.as_slice(),
            sector.walls.east.as_slice(),
            sector.walls.south.as_slice(),
            sector.walls.west.as_slice(),
            sector.walls.north_west_south_east.as_slice(),
            sector.walls.north_east_south_west.as_slice(),
        ] {
            for wall in walls {
                encode_light(wall.baked_vertex_rgb, &mut out);
            }
        }
    }
    out
}

fn encode_uvs(uvs: [(u8, u8); 4], out: &mut Vec<u8>) {
    for (u, v) in uvs {
        out.push(u);
        out.push(v);
    }
}

fn encode_light(vertex_rgb: [[u8; 3]; 4], out: &mut Vec<u8>) {
    for rgb in vertex_rgb {
        out.extend_from_slice(&rgb);
    }
}

const fn direction_id(direction: GridDirection) -> u8 {
    match direction {
        GridDirection::North => world::direction::NORTH,
        GridDirection::East => world::direction::EAST,
        GridDirection::South => world::direction::SOUTH,
        GridDirection::West => world::direction::WEST,
        GridDirection::NorthWestSouthEast => world::direction::NORTH_WEST_SOUTH_EAST,
        GridDirection::NorthEastSouthWest => world::direction::NORTH_EAST_SOUTH_WEST,
    }
}
