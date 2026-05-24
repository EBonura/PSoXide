//! Tomb Raider level parsing and import helpers.
//!
//! This module intentionally mirrors the classic Tomb Raider model:
//! independent rooms, 1024-unit sector grids, and explicit 3D
//! visibility portals. The generated editor project is a bridge into
//! PSoXide, not a re-interpretation through the older single-map seam
//! partitioner.

use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;

use crate::{
    FarVistaSettings, GridDirection, GridHorizontalFace, GridVerticalFace, MaterialResource,
    NodeKind, PortalGeometry, ProjectDocument, ResourceData, ResourceId, SkySettings, Transform3,
    WorldCameraSettings, WorldCullingSettings, WorldGrid, WorldStreamingSettings,
};

pub const TR4_VERSION: u32 = 0x0034_5254;
pub const TR_SECTOR_SIZE: i32 = 1024;
pub const TR_HEIGHT_UNIT: i32 = 256;
pub const TR_NO_ROOM: u8 = 255;
pub const TR_NO_HEIGHT: i8 = -127;
pub const TR_IMPORT_TEXTURE_PATH: &str = "assets/textures/cobbles_1a.psxt";

#[derive(Debug)]
pub enum TrLevelError {
    Io(std::io::Error),
    UnsupportedVersion(u32),
    UnexpectedEof {
        offset: usize,
        needed: usize,
        len: usize,
    },
    InvalidChunk {
        label: &'static str,
        expected: u32,
        actual: usize,
    },
    Decompress {
        label: &'static str,
        source: std::io::Error,
    },
    InvalidValue(String),
}

impl std::fmt::Display for TrLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported TR level version 0x{version:08x}")
            }
            Self::UnexpectedEof {
                offset,
                needed,
                len,
            } => write!(
                f,
                "unexpected end of file at offset {offset} while reading {needed} byte(s) from {len} byte buffer"
            ),
            Self::InvalidChunk {
                label,
                expected,
                actual,
            } => write!(
                f,
                "{label} chunk decompressed to {actual} bytes, expected {expected}"
            ),
            Self::Decompress { label, source } => {
                write!(f, "failed to decompress {label} chunk: {source}")
            }
            Self::InvalidValue(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for TrLevelError {}

impl From<std::io::Error> for TrLevelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone)]
pub struct TrLevel {
    pub source_path: Option<PathBuf>,
    pub version: u32,
    pub texture_counts: TrTextureCounts,
    pub rooms: Vec<TrRoom>,
    pub level_data_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrTextureCounts {
    pub room_textiles: u16,
    pub object_textiles: u16,
    pub bump_textiles: u16,
}

#[derive(Debug, Clone)]
pub struct TrRoom {
    pub index: u16,
    pub info: TrRoomInfo,
    pub mesh: TrRoomMesh,
    pub portals: Vec<TrPortal>,
    pub num_z_sectors: u16,
    pub num_x_sectors: u16,
    pub sectors: Vec<TrRoomSector>,
    pub room_colour: u32,
    pub light_count: u16,
    pub static_mesh_count: u16,
    pub alternate_room: i16,
    pub flags: i16,
    pub water_scheme: u8,
    pub reverb_info: u8,
    pub alternate_group: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrRoomInfo {
    pub x: i32,
    pub z: i32,
    pub y_bottom: i32,
    pub y_top: i32,
}

#[derive(Debug, Clone, Default)]
pub struct TrRoomMesh {
    pub data_words: u32,
    pub vertices: Vec<TrRoomVertex>,
    pub rectangles: Vec<TrFace4>,
    pub triangles: Vec<TrFace3>,
    pub sprites: Vec<TrRoomSprite>,
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrRoomVertex {
    pub position: [i16; 3],
    pub lighting: i16,
    pub attributes: u16,
    pub colour: u16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrFace4 {
    pub vertices: [u16; 4],
    pub texture: u16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrFace3 {
    pub vertices: [u16; 3],
    pub texture: u16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrRoomSprite {
    pub vertex: i16,
    pub texture: i16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrPortal {
    pub adjoining_room: u16,
    pub normal: [i16; 3],
    pub vertices: [[i16; 3]; 4],
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TrRoomSector {
    pub floor_data_index: u16,
    pub box_index: u16,
    pub room_below: u8,
    pub floor: i8,
    pub room_above: u8,
    pub ceiling: i8,
}

#[derive(Debug, Clone, Default)]
pub struct TrImportReport {
    pub source_path: Option<PathBuf>,
    pub version: u32,
    pub rooms: usize,
    pub portals: usize,
    pub sectors: usize,
    pub mesh_vertices: usize,
    pub mesh_rectangles: usize,
    pub mesh_triangles: usize,
    pub room_textiles: u16,
    pub object_textiles: u16,
    pub bump_textiles: u16,
}

pub fn load_tr4(path: impl AsRef<Path>) -> Result<TrLevel, TrLevelError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let mut level = parse_tr4(&bytes)?;
    level.source_path = Some(path.to_path_buf());
    Ok(level)
}

pub fn parse_tr4(bytes: &[u8]) -> Result<TrLevel, TrLevelError> {
    let mut reader = Reader::new(bytes);
    let version = reader.u32()?;
    if version != TR4_VERSION {
        return Err(TrLevelError::UnsupportedVersion(version));
    }

    let texture_counts = TrTextureCounts {
        room_textiles: reader.u16()?,
        object_textiles: reader.u16()?,
        bump_textiles: reader.u16()?,
    };

    skip_compressed_chunk(&mut reader, "32-bit room/object textures")?;
    skip_compressed_chunk(&mut reader, "16-bit room/object textures")?;
    skip_compressed_chunk(&mut reader, "32-bit misc textures")?;
    let level_data = read_compressed_chunk(&mut reader, "level data")?;
    let level_data_bytes = level_data.len();
    let rooms = parse_tr4_level_data(&level_data)?;

    Ok(TrLevel {
        source_path: None,
        version,
        texture_counts,
        rooms,
        level_data_bytes,
    })
}

pub fn import_tr4_project(
    path: impl AsRef<Path>,
    project_name: impl Into<String>,
) -> Result<(ProjectDocument, TrImportReport), TrLevelError> {
    let level = load_tr4(path)?;
    let report = level.report();
    let project = level.to_project(project_name.into());
    Ok((project, report))
}

impl TrLevel {
    pub fn report(&self) -> TrImportReport {
        TrImportReport {
            source_path: self.source_path.clone(),
            version: self.version,
            rooms: self.rooms.len(),
            portals: self.rooms.iter().map(|room| room.portals.len()).sum(),
            sectors: self.rooms.iter().map(|room| room.sectors.len()).sum(),
            mesh_vertices: self.rooms.iter().map(|room| room.mesh.vertices.len()).sum(),
            mesh_rectangles: self
                .rooms
                .iter()
                .map(|room| room.mesh.rectangles.len())
                .sum(),
            mesh_triangles: self
                .rooms
                .iter()
                .map(|room| room.mesh.triangles.len())
                .sum(),
            room_textiles: self.texture_counts.room_textiles,
            object_textiles: self.texture_counts.object_textiles,
            bump_textiles: self.texture_counts.bump_textiles,
        }
    }

    pub fn to_project(&self, project_name: String) -> ProjectDocument {
        let mut project = ProjectDocument::new(project_name);
        let texture = project.add_resource(
            "TR Import Texture",
            ResourceData::Texture {
                psxt_path: TR_IMPORT_TEXTURE_PATH.to_string(),
            },
        );
        let material = project.add_resource(
            "TR Import Material",
            ResourceData::Material(MaterialResource::opaque(Some(texture))),
        );
        let world_id = project.active_scene().root;
        if let Some(world) = project.active_scene_mut().node_mut(world_id) {
            world.name = "Tomb Raider Level".to_string();
            world.kind = NodeKind::World {
                sector_size: TR_SECTOR_SIZE,
                sky: SkySettings::default(),
                far_vista: FarVistaSettings::default(),
                camera: WorldCameraSettings::default(),
                culling: WorldCullingSettings::default(),
                streaming: WorldStreamingSettings::default(),
            };
        }

        let mut room_nodes = Vec::with_capacity(self.rooms.len());
        for room in &self.rooms {
            let id = project.active_scene_mut().add_node(
                world_id,
                format!("TR Room {:03}", room.index),
                NodeKind::Room {
                    grid: room.to_world_grid(material),
                },
            );
            room_nodes.push(id);
        }

        for room in &self.rooms {
            let Some(&source_node) = room_nodes.get(room.index as usize) else {
                continue;
            };
            for (portal_index, portal) in room.portals.iter().enumerate() {
                let target_room = room_nodes.get(portal.adjoining_room as usize).copied();
                let geometry = room.portal_geometry(portal);
                let id = project.active_scene_mut().add_node(
                    source_node,
                    format!("TR Portal {:03}->{:03}", room.index, portal.adjoining_room),
                    NodeKind::Portal {
                        target_room,
                        target_entry: format!("room_{:03}", portal.adjoining_room),
                        entry_name: format!("room_{:03}_portal_{:02}", room.index, portal_index),
                        geometry: Some(geometry.clone()),
                    },
                );
                if let Some(node) = project.active_scene_mut().node_mut(id) {
                    node.transform = Transform3 {
                        translation: portal_centroid(&geometry.vertices),
                        ..Transform3::default()
                    };
                }
            }
        }

        project
    }
}

impl TrRoom {
    pub fn to_world_grid(&self, material: ResourceId) -> WorldGrid {
        let mut grid = WorldGrid::empty(self.num_x_sectors, self.num_z_sectors, TR_SECTOR_SIZE);
        grid.origin = [
            div_floor_i32(self.info.x, TR_SECTOR_SIZE),
            div_floor_i32(self.info.z, TR_SECTOR_SIZE),
        ];
        grid.ambient_color = tr_argb_to_rgb(self.room_colour);
        grid.fog_enabled = false;
        grid.atmosphere_enabled = self.water_scheme != 0;

        for x in 0..self.num_x_sectors {
            for z in 0..self.num_z_sectors {
                let Some(sector) = self.sector(x, z) else {
                    continue;
                };
                if sector.floor == TR_NO_HEIGHT && sector.ceiling == TR_NO_HEIGHT {
                    continue;
                }
                if let Some(editor_sector) = grid.ensure_sector(x, z) {
                    if sector.floor != TR_NO_HEIGHT {
                        editor_sector.floor = Some(GridHorizontalFace::flat(
                            tr_height_to_editor(sector.floor),
                            Some(material),
                        ));
                    }
                    if sector.ceiling != TR_NO_HEIGHT {
                        editor_sector.ceiling = Some(GridHorizontalFace::flat(
                            tr_height_to_editor(sector.ceiling),
                            Some(material),
                        ));
                    }
                }
            }
        }

        add_sector_boundary_walls(self, &mut grid, material);
        grid
    }

    pub fn sector(&self, x: u16, z: u16) -> Option<&TrRoomSector> {
        if x < self.num_x_sectors && z < self.num_z_sectors {
            self.sectors
                .get(x as usize * self.num_z_sectors as usize + z as usize)
        } else {
            None
        }
    }

    fn portal_geometry(&self, portal: &TrPortal) -> PortalGeometry {
        let mut vertices = [[0; 3]; 4];
        for (out, input) in vertices.iter_mut().zip(portal.vertices) {
            *out = self.local_vertex_to_editor(input);
        }
        PortalGeometry {
            normal: [
                i32::from(portal.normal[0]),
                -i32::from(portal.normal[1]),
                i32::from(portal.normal[2]),
            ],
            vertices,
        }
    }

    fn local_vertex_to_editor(&self, vertex: [i16; 3]) -> [i32; 3] {
        [
            self.info.x.saturating_add(i32::from(vertex[0])),
            -i32::from(vertex[1]),
            self.info.z.saturating_add(i32::from(vertex[2])),
        ]
    }
}

fn parse_tr4_level_data(bytes: &[u8]) -> Result<Vec<TrRoom>, TrLevelError> {
    let mut reader = Reader::new(bytes);
    let _unused = reader.u32()?;
    let room_count = reader.u16()?;
    let mut rooms = Vec::with_capacity(room_count as usize);
    for index in 0..room_count {
        rooms.push(parse_tr4_room(&mut reader, index)?);
    }
    Ok(rooms)
}

fn parse_tr4_room(reader: &mut Reader<'_>, index: u16) -> Result<TrRoom, TrLevelError> {
    let info = TrRoomInfo {
        x: reader.i32()?,
        z: reader.i32()?,
        y_bottom: reader.i32()?,
        y_top: reader.i32()?,
    };

    let data_words = reader.u32()?;
    let data_bytes = usize::try_from(data_words)
        .ok()
        .and_then(|words| words.checked_mul(2))
        .ok_or_else(|| TrLevelError::InvalidValue(format!("room {index} data is too large")))?;
    let data = reader.bytes(data_bytes)?.to_vec();
    let mesh = parse_room_mesh(data_words, &data);

    let portal_count = reader.u16()?;
    let mut portals = Vec::with_capacity(portal_count as usize);
    for _ in 0..portal_count {
        portals.push(TrPortal {
            adjoining_room: reader.u16()?,
            normal: [reader.i16()?, reader.i16()?, reader.i16()?],
            vertices: [
                [reader.i16()?, reader.i16()?, reader.i16()?],
                [reader.i16()?, reader.i16()?, reader.i16()?],
                [reader.i16()?, reader.i16()?, reader.i16()?],
                [reader.i16()?, reader.i16()?, reader.i16()?],
            ],
        });
    }

    let num_z_sectors = reader.u16()?;
    let num_x_sectors = reader.u16()?;
    let sector_count = usize::from(num_x_sectors)
        .checked_mul(usize::from(num_z_sectors))
        .ok_or_else(|| TrLevelError::InvalidValue(format!("room {index} has too many sectors")))?;
    let mut sectors = Vec::with_capacity(sector_count);
    for _ in 0..sector_count {
        sectors.push(TrRoomSector {
            floor_data_index: reader.u16()?,
            box_index: reader.u16()?,
            room_below: reader.u8()?,
            floor: reader.i8()?,
            room_above: reader.u8()?,
            ceiling: reader.i8()?,
        });
    }

    let room_colour = reader.u32()?;
    let light_count = reader.u16()?;
    reader.skip(usize::from(light_count) * 46)?;
    let static_mesh_count = reader.u16()?;
    reader.skip(usize::from(static_mesh_count) * 20)?;
    let alternate_room = reader.i16()?;
    let flags = reader.i16()?;
    let water_scheme = reader.u8()?;
    let reverb_info = reader.u8()?;
    let alternate_group = reader.u8()?;

    Ok(TrRoom {
        index,
        info,
        mesh,
        portals,
        num_z_sectors,
        num_x_sectors,
        sectors,
        room_colour,
        light_count,
        static_mesh_count,
        alternate_room,
        flags,
        water_scheme,
        reverb_info,
        alternate_group,
    })
}

fn parse_room_mesh(data_words: u32, bytes: &[u8]) -> TrRoomMesh {
    let mut reader = Reader::new(bytes);
    let parsed = (|| -> Result<TrRoomMesh, TrLevelError> {
        let vertex_count = nonnegative_count(reader.i16()?, "room vertex count")?;
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            vertices.push(TrRoomVertex {
                position: [reader.i16()?, reader.i16()?, reader.i16()?],
                lighting: reader.i16()?,
                attributes: reader.u16()?,
                colour: reader.u16()?,
            });
        }

        let rectangle_count = nonnegative_count(reader.i16()?, "room rectangle count")?;
        let mut rectangles = Vec::with_capacity(rectangle_count);
        for _ in 0..rectangle_count {
            rectangles.push(TrFace4 {
                vertices: [reader.u16()?, reader.u16()?, reader.u16()?, reader.u16()?],
                texture: reader.u16()?,
            });
        }

        let triangle_count = nonnegative_count(reader.i16()?, "room triangle count")?;
        let mut triangles = Vec::with_capacity(triangle_count);
        for _ in 0..triangle_count {
            triangles.push(TrFace3 {
                vertices: [reader.u16()?, reader.u16()?, reader.u16()?],
                texture: reader.u16()?,
            });
        }

        let sprite_count = nonnegative_count(reader.i16()?, "room sprite count")?;
        let mut sprites = Vec::with_capacity(sprite_count);
        for _ in 0..sprite_count {
            sprites.push(TrRoomSprite {
                vertex: reader.i16()?,
                texture: reader.i16()?,
            });
        }

        Ok(TrRoomMesh {
            data_words,
            vertices,
            rectangles,
            triangles,
            sprites,
            parse_error: None,
        })
    })();

    match parsed {
        Ok(mesh) => mesh,
        Err(error) => TrRoomMesh {
            data_words,
            parse_error: Some(error.to_string()),
            ..TrRoomMesh::default()
        },
    }
}

fn skip_compressed_chunk(reader: &mut Reader<'_>, label: &'static str) -> Result<(), TrLevelError> {
    let _uncompressed_size = reader.u32()?;
    let compressed_size = reader.u32()?;
    reader.skip(usize::try_from(compressed_size).map_err(|_| {
        TrLevelError::InvalidValue(format!("{label} chunk compressed size is too large"))
    })?)
}

fn read_compressed_chunk(
    reader: &mut Reader<'_>,
    label: &'static str,
) -> Result<Vec<u8>, TrLevelError> {
    let uncompressed_size = reader.u32()?;
    let compressed_size = reader.u32()?;
    let compressed = reader.bytes(usize::try_from(compressed_size).map_err(|_| {
        TrLevelError::InvalidValue(format!("{label} chunk compressed size is too large"))
    })?)?;

    let mut decoder = ZlibDecoder::new(compressed);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|source| TrLevelError::Decompress { label, source })?;
    if out.len() != uncompressed_size as usize {
        return Err(TrLevelError::InvalidChunk {
            label,
            expected: uncompressed_size,
            actual: out.len(),
        });
    }
    Ok(out)
}

fn add_sector_boundary_walls(room: &TrRoom, grid: &mut WorldGrid, material: ResourceId) {
    for x in 0..room.num_x_sectors {
        for z in 0..room.num_z_sectors {
            if !sector_has_floor(room.sector(x, z)) {
                continue;
            }
            add_boundary_wall_if_needed(room, grid, x, z, GridDirection::West, material);
            add_boundary_wall_if_needed(room, grid, x, z, GridDirection::South, material);
            add_boundary_wall_if_needed(room, grid, x, z, GridDirection::East, material);
            add_boundary_wall_if_needed(room, grid, x, z, GridDirection::North, material);
            add_ledge_wall_if_needed(room, grid, x, z, GridDirection::East, material);
            add_ledge_wall_if_needed(room, grid, x, z, GridDirection::North, material);
        }
    }
}

fn add_boundary_wall_if_needed(
    room: &TrRoom,
    grid: &mut WorldGrid,
    x: u16,
    z: u16,
    direction: GridDirection,
    material: ResourceId,
) {
    let Some((nx, nz)) = neighbour_cell(x, z, direction) else {
        add_full_height_wall(room, grid, x, z, direction, material);
        return;
    };
    if nx >= room.num_x_sectors
        || nz >= room.num_z_sectors
        || !sector_has_floor(room.sector(nx, nz))
    {
        add_full_height_wall(room, grid, x, z, direction, material);
    }
}

fn add_ledge_wall_if_needed(
    room: &TrRoom,
    grid: &mut WorldGrid,
    x: u16,
    z: u16,
    direction: GridDirection,
    material: ResourceId,
) {
    let Some((nx, nz)) = neighbour_cell(x, z, direction) else {
        return;
    };
    if nx >= room.num_x_sectors || nz >= room.num_z_sectors {
        return;
    }
    let Some(current_floor) = sector_floor_height(room.sector(x, z)) else {
        return;
    };
    let Some(neighbour_floor) = sector_floor_height(room.sector(nx, nz)) else {
        return;
    };
    if current_floor == neighbour_floor {
        return;
    }
    if current_floor < neighbour_floor {
        push_wall(
            grid,
            x,
            z,
            direction,
            current_floor,
            neighbour_floor,
            material,
        );
    } else {
        if let Some(opposite) = direction.opposite_cardinal() {
            push_wall(
                grid,
                nx,
                nz,
                opposite,
                neighbour_floor,
                current_floor,
                material,
            );
        }
    }
}

fn add_full_height_wall(
    room: &TrRoom,
    grid: &mut WorldGrid,
    x: u16,
    z: u16,
    direction: GridDirection,
    material: ResourceId,
) {
    let Some(floor) = sector_floor_height(room.sector(x, z)) else {
        return;
    };
    let ceiling = sector_ceiling_height(room.sector(x, z)).unwrap_or_else(|| {
        floor.saturating_add(
            room.info
                .y_bottom
                .saturating_sub(room.info.y_top)
                .unsigned_abs()
                .max(TR_SECTOR_SIZE as u32) as i32,
        )
    });
    if ceiling > floor {
        push_wall(grid, x, z, direction, floor, ceiling, material);
    }
}

fn push_wall(
    grid: &mut WorldGrid,
    x: u16,
    z: u16,
    direction: GridDirection,
    bottom: i32,
    top: i32,
    material: ResourceId,
) {
    if bottom == top {
        return;
    }
    let (bottom, top) = if bottom < top {
        (bottom, top)
    } else {
        (top, bottom)
    };
    if let Some(sector) = grid.ensure_sector(x, z) {
        sector
            .walls
            .get_mut(direction)
            .push(GridVerticalFace::flat(bottom, top, Some(material)));
    }
}

fn neighbour_cell(x: u16, z: u16, direction: GridDirection) -> Option<(u16, u16)> {
    match direction {
        GridDirection::North => z.checked_add(1).map(|nz| (x, nz)),
        GridDirection::East => x.checked_add(1).map(|nx| (nx, z)),
        GridDirection::South => z.checked_sub(1).map(|nz| (x, nz)),
        GridDirection::West => x.checked_sub(1).map(|nx| (nx, z)),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

fn sector_has_floor(sector: Option<&TrRoomSector>) -> bool {
    sector_floor_height(sector).is_some()
}

fn sector_floor_height(sector: Option<&TrRoomSector>) -> Option<i32> {
    let sector = sector?;
    (sector.floor != TR_NO_HEIGHT).then(|| tr_height_to_editor(sector.floor))
}

fn sector_ceiling_height(sector: Option<&TrRoomSector>) -> Option<i32> {
    let sector = sector?;
    (sector.ceiling != TR_NO_HEIGHT).then(|| tr_height_to_editor(sector.ceiling))
}

fn tr_height_to_editor(height: i8) -> i32 {
    -i32::from(height) * TR_HEIGHT_UNIT
}

fn tr_argb_to_rgb(argb: u32) -> [u8; 3] {
    [
        ((argb >> 16) & 0xff) as u8,
        ((argb >> 8) & 0xff) as u8,
        (argb & 0xff) as u8,
    ]
}

fn portal_centroid(vertices: &[[i32; 3]; 4]) -> [f32; 3] {
    let mut sum = [0i32; 3];
    for vertex in vertices {
        sum[0] = sum[0].saturating_add(vertex[0]);
        sum[1] = sum[1].saturating_add(vertex[1]);
        sum[2] = sum[2].saturating_add(vertex[2]);
    }
    [
        sum[0] as f32 / 4.0,
        sum[1] as f32 / 4.0,
        sum[2] as f32 / 4.0,
    ]
}

fn div_floor_i32(value: i32, divisor: i32) -> i32 {
    let mut q = value / divisor;
    let r = value % divisor;
    if r != 0 && ((r > 0) != (divisor > 0)) {
        q -= 1;
    }
    q
}

fn nonnegative_count(value: i16, label: &str) -> Result<usize, TrLevelError> {
    if value < 0 {
        return Err(TrLevelError::InvalidValue(format!(
            "{label} is negative: {value}"
        )));
    }
    Ok(value as usize)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn skip(&mut self, count: usize) -> Result<(), TrLevelError> {
        self.bytes(count).map(|_| ())
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], TrLevelError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(TrLevelError::UnexpectedEof {
                offset: self.offset,
                needed: count,
                len: self.bytes.len(),
            })?;
        if end > self.bytes.len() {
            return Err(TrLevelError::UnexpectedEof {
                offset: self.offset,
                needed: count,
                len: self.bytes.len(),
            });
        }
        let out = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, TrLevelError> {
        Ok(self.bytes(1)?[0])
    }

    fn i8(&mut self) -> Result<i8, TrLevelError> {
        Ok(self.u8()? as i8)
    }

    fn u16(&mut self) -> Result<u16, TrLevelError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn i16(&mut self) -> Result<i16, TrLevelError> {
        let bytes = self.bytes(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, TrLevelError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, TrLevelError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_coastal_ruins_when_available() {
        let Some(path) = local_coastal_ruins_path() else {
            return;
        };
        let level = load_tr4(&path).expect("local Coastal Ruins TR4 parses");
        let report = level.report();
        assert_eq!(report.version, TR4_VERSION);
        assert_eq!(report.rooms, 177);
        assert_eq!(report.portals, 490);
        assert_eq!(report.sectors, 5894);
        assert!(report.mesh_vertices > 0);
        assert!(report.mesh_rectangles > 0);
        let project = level.to_project("Coastal Ruins".to_string());
        let portal_nodes = project
            .active_scene()
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Portal { .. }))
            .count();
        assert_eq!(portal_nodes, report.portals);
    }

    fn local_coastal_ruins_path() -> Option<PathBuf> {
        std::env::var_os("PSXED_TR4_TEST_PATH")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                let home = std::env::var_os("HOME")?;
                let path = PathBuf::from(home).join(
                    "Library/Application Support/CrossOver/Bottles/Steam/drive_c/Program Files (x86)/Steam/steamapps/common/Tomb Raider (IV) The Last Revelation/data/alexhub2.tr4",
                );
                path.is_file().then_some(path)
            })
    }
}
