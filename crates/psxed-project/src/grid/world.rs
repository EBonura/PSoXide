//! The grid world document root and the scene's world-kind discriminator.

use serde::{Deserialize, Serialize};

use super::light::FacingShade;
use super::lighting::GlobalLight;
use super::room::GridRoom;
use super::units::{GridScale, SectorTexels, SECTOR_UNITS};
use crate::face_uv::UvMode;

/// A grid world: rooms on one shared sector lattice and the scale that maps
/// it into the editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridWorld {
    /// Grid units to editor units.
    #[serde(default)]
    pub scale: GridScale,
    /// Texels one sector edge spans in the default mapping a face takes
    /// when neither it nor its material says otherwise. A world stored
    /// without the setting keeps the 32 it was made with.
    #[serde(
        default = "SectorTexels::legacy",
        skip_serializing_if = "SectorTexels::is_legacy"
    )]
    pub texels_per_sector: SectorTexels,
    /// Rooms, in authoring order.
    #[serde(default)]
    pub rooms: Vec<GridRoom>,
    /// How much darker faces are by the way they face, in the editor
    /// preview and the cook alike. Off for a world made before the setting.
    #[serde(default, skip_serializing_if = "FacingShade::is_off")]
    pub shading: FacingShade,
    /// Ambient occlusion, sky light and bounced light gathered into the
    /// corner colours. Off for a world made before the setting.
    #[serde(default, skip_serializing_if = "GlobalLight::is_default")]
    pub global_light: GlobalLight,
    /// Draw, activation and prefetch radii, in sectors, written into the
    /// level core.
    #[serde(default, skip_serializing_if = "StreamRadii::is_default")]
    pub stream: StreamRadii,
    /// The mesh assets rooms place instances of (`docs/grid/meshes.md`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mesh_assets: Vec<super::mesh::MeshAsset>,
    /// Names the author gave storeys, by floor height in clicks (a storey
    /// is derived, so its name is kept against its height). Editor-only: the
    /// cook ignores it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub level_names: std::collections::BTreeMap<i32, String>,
    /// Materials the author added to the palette (created or imported from
    /// it) that the world does not use yet. Editor-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub palette_pins: Vec<crate::ResourceId>,
}

/// Radii in sectors, walked over the portal graph, that decide which rooms
/// are drawn, hold active entities and (once rooms stream) are resident.
/// They are the per-level knobs of `docs/grid/streaming-design.md` 3.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamRadii {
    /// A room whose walk distance from the camera exceeds this is neither
    /// drawn nor needed in memory. 0 draws every room that is reachable.
    pub draw_far: u8,
    /// Entities in rooms within this walk distance of the player think.
    pub active_radius: u8,
    /// Rooms between `draw_far` and `draw_far + lead_radius` are prefetched
    /// by a streamer and evicted first.
    pub lead_radius: u8,
    /// Stream and draw as a ring instead: the observer's room and every room
    /// one portal away, whatever `draw_far` and `lead_radius` say. For a
    /// world cut into even chunks ([`super::edit::chunks`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ring: bool,
    /// Stream by budgeted portal flood instead of a ring or walk radii
    /// ([`FloodSettings`]).
    #[serde(skip_serializing_if = "FloodSettings::is_default")]
    pub flood: FloodSettings,
}

/// Settings of the budgeted portal flood (`psx_grid::flood`), which keeps
/// resident the cells within a flood radius of the player and draws the ones
/// within a smaller radius from the camera.
///
/// The cook turns these into the level's flood block and checks them: the
/// pool the busiest position needs must fit the guest, and the quads of a
/// view at the draw radius must fit `quad_budget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FloodSettings {
    /// Whether the level streams by flood.
    pub on: bool,
    /// Cells within this many sectors (flood distance) of the camera are
    /// drawn, collide and run entities.
    pub near_radius: u8,
    /// Cells within this many sectors of the player are kept in memory, so
    /// the drawn set can grow into them without a wait. At least
    /// `near_radius`.
    pub res_radius: u8,
    /// Most quads the plan cone of a camera may hold at `near_radius`; the
    /// cook refuses a level whose worst position exceeds it. 0 only reports.
    pub quad_budget: u16,
    /// Also bake the far skin (the whole level as flat coloured top surfaces)
    /// and draw it for the cells that are not drawn whole. Needs `on`.
    pub skin: bool,
    /// Runtime units two sectors' tops may differ by and still merge into one
    /// skin leaf (the skin's one detail knob); 0 means the default, 20.
    pub skin_tolerance: u8,
    /// Draw the rooms beyond `near_radius` as upright pictures standing where
    /// the rooms stand (`psx_grid::render::cards`), baked at cook time from
    /// eight headings round each room. Needs `on` and a cube sky (the pictures
    /// take the sky atlas's VRAM; the sky is drawn as a plain gradient).
    pub cards: bool,
    /// Sectors from the camera out to which cards are shown; 0 means the
    /// default, 20.
    pub card_far: u8,
    /// Sectors from the camera inside which no card is drawn, drawn whole or
    /// not; 0 (the default) means none: a room that is not drawn whole shows its
    /// card at any distance, and one that is drawn whole fades its card in over
    /// the last sector before `near_radius`.
    pub card_near: u8,
    /// Layers of cards beyond `card_far` (0 to 2), each holding everything
    /// from its distance to the next layer's, the last one everything to the
    /// horizon ([`FloodSettings::card_layer_bands`]).
    pub card_layers: u8,
    /// Draw everything beyond the rooms drawn whole as depth-meshed panels baked at a
    /// lattice of eyes (`psx_grid::render::panels`) instead of cards: the lattice's spacing in
    /// sectors, 0 for none. Needs `on` and a cube sky (the pictures take the sky atlas's
    /// VRAM, as the cards do); `cards` is left off.
    pub panels: u8,
}

impl FloodSettings {
    /// Off, with the radii the Citadel was tuned at (hex 4 cells: the largest
    /// draw radius that holds its route above 25.5 fps).
    pub const DEFAULT: Self = Self {
        on: false,
        near_radius: 8,
        res_radius: 12,
        quad_budget: 0,
        skin: false,
        skin_tolerance: 0,
        cards: false,
        card_far: 0,
        card_near: 0,
        card_layers: 0,
        panels: 0,
    };

    /// Sectors out to which cards are shown.
    pub fn card_far_sectors(&self) -> i32 {
        if self.card_far > 0 {
            i32::from(self.card_far)
        } else {
            20
        }
    }

    /// Sectors inside which no card is drawn.
    pub fn card_near_sectors(&self) -> i32 {
        i32::from(self.card_near)
    }

    /// Sectors from the camera inside which the cook does not lay copies of
    /// the pictures beside their tiles: two sectors inside `near_radius`, where
    /// the rooms are drawn whole and a card is rarely asked for.
    pub fn card_copy_near_sectors(&self) -> i32 {
        (i32::from(self.near_radius) - 2).max(0)
    }

    /// The layers of cards beyond the room cards, from the nearest out:
    /// `(far, cell, depth)` in sectors. A layer's cards show out to `far` plan
    /// sectors (the next layer shows from there); its anchors stand `cell`
    /// sectors apart; its pictures hold everything behind the card out to `depth` sectors
    /// (`None`: to the horizon). The first layer starts where the room cards
    /// end ([`Self::card_far_sectors`]).
    pub fn card_layer_bands(&self) -> Vec<CardLayerBand> {
        let far = self.card_far_sectors();
        let all = [
            CardLayerBand {
                cell: far * 7 / 6,
                depth: Some(far),
            },
            CardLayerBand {
                cell: far * 2,
                depth: None,
            },
        ];
        all.into_iter()
            .take(usize::from(self.card_layers.min(2)))
            .collect()
    }

    /// Whether these are the defaults (the document then omits them).
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }
}

/// One layer of far cards beyond the room cards ([`FloodSettings::card_layer_bands`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardLayerBand {
    /// Sectors between the layer's anchors.
    pub cell: i32,
    /// Sectors behind a card its picture holds; `None` to the horizon.
    pub depth: Option<i32>,
}

impl Default for FloodSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl StreamRadii {
    /// Defaults. The draw distance is wider than any room of the reference
    /// levels, so it only bounds a world made of many rooms.
    pub const DEFAULT: Self = Self {
        draw_far: 48,
        active_radius: 32,
        lead_radius: 8,
        ring: false,
        flood: FloodSettings::DEFAULT,
    };

    /// Whether these are the defaults (the document then omits them).
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl Default for StreamRadii {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Axis-aligned box in editor units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorBounds {
    /// Smallest corner.
    pub min: [i32; 3],
    /// Largest corner.
    pub max: [i32; 3],
}

fn merge(bounds: Option<EditorBounds>, add: EditorBounds) -> EditorBounds {
    match bounds {
        None => add,
        Some(b) => EditorBounds {
            min: core::array::from_fn(|i| b.min[i].min(add.min[i])),
            max: core::array::from_fn(|i| b.max[i].max(add.max[i])),
        },
    }
}

impl GridWorld {
    /// An empty world at the default scale.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the rooms are chunks of one surface (a world cut into even
    /// chunks, [`super::edit::chunks`]) rather than separate volumes. Across
    /// the border of a chunk lies the next chunk, not open space, so derived
    /// walls are worked out against the sibling chunks too.
    pub fn is_chunked(&self) -> bool {
        self.stream.ring
    }

    /// Whether faces and collision of `room` are made from its leaves one by
    /// one (the path that looks across room borders) rather than as a dense
    /// grid of whole sectors. A room with leaves of several sizes always
    /// is; so is a chunk of a chunked world, unless a column of several
    /// spans needs the dense path.
    pub fn builds_from_leaves(&self, room: &GridRoom) -> bool {
        !room.leaves.is_sectors_only()
            || (self.is_chunked() && room.leaves.iter().all(|(_, s)| s.is_single_span()))
    }

    /// The mapping a face takes when neither it nor its material says
    /// otherwise: world aligned at [`Self::texels_per_sector`].
    pub fn default_uv(&self) -> UvMode {
        UvMode::world(self.texels_per_sector.get())
    }

    /// Editor units in one sector edge.
    pub fn sector_editor_units(&self) -> i32 {
        self.scale.sector_editor_units()
    }

    /// Editor-space position of a room's local corner `(0, 0)` at its
    /// vertical offset, `[x, y, z]`.
    pub fn room_origin_editor(&self, room: &GridRoom) -> [i32; 3] {
        [
            self.scale
                .to_editor(room.origin[0].saturating_mul(SECTOR_UNITS)),
            self.scale.to_editor(room.y_offset),
            self.scale
                .to_editor(room.origin[1].saturating_mul(SECTOR_UNITS)),
        ]
    }

    /// Every material the world's surfaces and walls use, once each, in
    /// first-use order.
    pub fn material_ids(&self) -> Vec<crate::ResourceId> {
        let mut ids = Vec::new();
        let mut note = |id: Option<crate::ResourceId>| {
            if let Some(id) = id {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        };
        for room in &self.rooms {
            for (_, sector) in room.leaves() {
                for span in sector.spans() {
                    note(span.wall_material);
                }
                for surface in sector.surfaces() {
                    note(surface.tri(0).material);
                    note(surface.tri(1).material);
                }
                for edge in super::sector::WallEdge::ALL {
                    for wall in sector.walls(edge) {
                        note(wall.material);
                    }
                }
            }
            for plane in &room.planes {
                note(plane.material);
            }
            for mesh in room.meshes.iter().filter(|m| m.enabled) {
                for slot in &mesh.materials {
                    note(*slot);
                }
            }
            // What derived walls and bands wear.
            note(room.defaults.wall_material);
            for (_, o) in room.edge_overrides.iter() {
                note(o.material);
            }
            for set in &room.band_sets {
                for band in &set.bands {
                    note(band.material);
                }
            }
        }
        ids
    }

    /// Number of sectors over all rooms.
    pub fn sector_count(&self) -> usize {
        self.rooms.iter().map(GridRoom::sector_count).sum()
    }

    /// Height of the highest floor at editor position `(x, z)` that is not
    /// above `y_limit` (editor units), interpolated over the surface's
    /// triangles. `None` where no room has a floor there. The limit picks
    /// the right floor where rooms stack.
    ///
    /// The interpolation is [`psx_grid::height::height_at_i32`], the same
    /// function the runtime collision uses (there in runtime units), so the
    /// editor and the game stand on the same surface.
    pub fn floor_height_at(&self, x: i32, z: i32, y_limit: i32) -> Option<i32> {
        let (gx, gz) = (self.scale.to_grid(x), self.scale.to_grid(z));
        let click = [
            gx.div_euclid(super::units::CLICK_UNITS),
            gz.div_euclid(super::units::CLICK_UNITS),
        ];
        let sector_edge = self.scale.sector_editor_units().max(1);
        let mut best: Option<i32> = None;
        for room in &self.rooms {
            // The leaf under the point, of any level, and every span of its
            // column: the highest floor not above the limit wins.
            let Some((key, sector)) = room.leaf_at(click) else {
                continue;
            };
            if !room.contains_leaf(key) {
                continue;
            }
            // A leaf's edge is `edge_clicks` quarters of a sector.
            let edge = (sector_edge * key.edge_clicks() / super::leaf::CLICKS_PER_SECTOR).max(1);
            let inv24 = psx_grid::height::weight_recip(edge as u32);
            // Offsets inside the leaf, in editor units.
            let origin = [
                self.scale
                    .to_editor(key.x.saturating_mul(super::units::CLICK_UNITS)),
                self.scale
                    .to_editor(key.z.saturating_mul(super::units::CLICK_UNITS)),
            ];
            let off = |v: i32, o: i32| (v - o).clamp(0, edge) as u32;
            let (ox, oz) = (off(x, origin[0]), off(z, origin[1]));
            for floor in sector.spans().iter().filter_map(|s| s.floor.as_ref()) {
                let nesw = matches!(floor.split, super::surface::GridSplit::NeSw);
                let second = !psx_grid::height::in_first_triangle(nesw, ox, oz, edge as u32);
                let corners = floor
                    .heights_for(usize::from(second))
                    .map(|h| self.scale.to_editor(room.y_offset + h));
                let y = psx_grid::height::height_at_i32(corners, nesw, ox, oz, edge as u32, inv24);
                if y <= y_limit && best.is_none_or(|b| y > b) {
                    best = Some(y);
                }
            }
        }
        best
    }

    /// Bounding box of every surface in editor units, or `None` for a world
    /// with no geometry. Used to frame the camera and size the depth range.
    pub fn editor_bounds(&self) -> Option<EditorBounds> {
        let mut bounds: Option<EditorBounds> = None;
        for (room_index, room) in self.rooms.iter().enumerate() {
            let origin = self.room_origin_editor(room);
            for (key, sector) in room.leaves() {
                let Some((low, high)) = sector.height_range() else {
                    continue;
                };
                let click = super::units::CLICK_UNITS;
                let n = key.edge_clicks();
                let x0 = self.scale.to_editor(key.x.saturating_mul(click));
                let z0 = self.scale.to_editor(key.z.saturating_mul(click));
                let x1 = self.scale.to_editor((key.x + n).saturating_mul(click));
                let z1 = self.scale.to_editor((key.z + n).saturating_mul(click));
                let y0 = origin[1] + self.scale.to_editor(low);
                let y1 = origin[1] + self.scale.to_editor(high);
                let cell = EditorBounds {
                    min: [x0, y0, z0],
                    max: [x1, y1, z1],
                };
                bounds = Some(merge(bounds, cell));
            }
            for plane in room.planes.iter().filter(|p| p.enabled) {
                let (lo, hi) = plane.bounds();
                let point = |v: [i32; 3]| {
                    [
                        self.scale.to_editor(v[0]),
                        origin[1] + self.scale.to_editor(v[1]),
                        self.scale.to_editor(v[2]),
                    ]
                };
                bounds = Some(merge(
                    bounds,
                    EditorBounds {
                        min: point(lo),
                        max: point(hi),
                    },
                ));
            }
            for mesh_index in 0..room.meshes.len() {
                let Some(placed) = super::mesh_faces::placed(self, room_index, mesh_index) else {
                    continue;
                };
                let Some((lo, hi)) = placed.mesh.bounds(&placed.built) else {
                    continue;
                };
                let point = |v: [i32; 3]| {
                    [
                        self.scale.to_editor(v[0]),
                        origin[1] + self.scale.to_editor(v[1]),
                        self.scale.to_editor(v[2]),
                    ]
                };
                bounds = Some(merge(
                    bounds,
                    EditorBounds {
                        min: point(lo),
                        max: point(hi),
                    },
                ));
            }
        }
        bounds
    }

    /// The id the next placed mesh takes: one above the highest in the world,
    /// so an id is never reused while its instance lives.
    pub fn next_mesh_id(&self) -> u16 {
        self.rooms
            .iter()
            .flat_map(|r| r.meshes.iter().map(|m| m.id))
            .max()
            .map_or(1, |id| id.saturating_add(1))
    }

    /// Room and index of the mesh instance with `id`.
    pub fn find_mesh(&self, id: u16) -> Option<(usize, usize)> {
        self.rooms
            .iter()
            .enumerate()
            .find_map(|(r, room)| room.meshes.iter().position(|m| m.id == id).map(|i| (r, i)))
    }
}

/// What a scene's world holds.
///
/// `Empty` is the default and the only value never written to disk, so a
/// scene with no sectors saves as it always has. `Grid` is the sector grid.
/// Files from the removed brush world carried `Brushes` here (it was never
/// written either), which the alias still reads as `Empty`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneWorld {
    /// No world authored yet.
    #[default]
    #[serde(alias = "Brushes")]
    Empty,
    /// Sector-grid world.
    Grid(GridWorld),
}

impl SceneWorld {
    /// Whether this is the default empty world.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// The grid world, if this is one.
    pub fn grid(&self) -> Option<&GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Empty => None,
        }
    }

    /// Mutable grid world, if this is one.
    pub fn grid_mut(&mut self) -> Option<&mut GridWorld> {
        match self {
            Self::Grid(grid) => Some(grid),
            Self::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::{GridSurface, GridWall};

    #[test]
    fn bounds_cover_surfaces_and_walls_in_editor_units() {
        let mut world = GridWorld::new();
        let mut room = GridRoom::new("a", 2, 1);
        room.origin = [1, -1];
        room.y_offset = 512;
        room.sector_or_insert(1, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(256, None)));
        room.sector_or_insert(1, 0)
            .unwrap()
            .north
            .push(GridWall::span(256, 2048, None));
        world.rooms.push(room);
        let b = world.editor_bounds().unwrap();
        // x: sector 2..3 at 1280 per sector; y: (512 + 256)..(512 + 2048)
        // scaled by 5/4; z: sector -1..0.
        assert_eq!(b.min, [2560, 960, -1280]);
        assert_eq!(b.max, [3840, 3200, 0]);
        assert_eq!(GridWorld::new().editor_bounds(), None);
    }

    #[test]
    fn floor_height_interpolates_both_triangles_and_respects_the_limit() {
        use crate::grid::surface::GridSplit;
        let mut room = GridRoom::new("a", 1, 1);
        let mut slope = GridSurface::flat(0, None);
        slope.heights = [0, 0, 1024, 1024]; // rises towards +z
        slope.split = GridSplit::NwSe;
        room.sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(slope));
        let mut upper = GridRoom::new("b", 1, 1);
        upper.y_offset = 2048;
        upper
            .sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room, upper],
            ..GridWorld::default()
        };
        // (512, 256) lies in the first triangle, (100, 900) in the second.
        assert_eq!(world.floor_height_at(512, 256, 10_000), Some(2048));
        assert_eq!(world.floor_height_at(512, 256, 1000), Some(256));
        assert_eq!(world.floor_height_at(100, 900, 1000), Some(900));
        assert_eq!(world.floor_height_at(5000, 0, 1000), None);
        assert_eq!(world.floor_height_at(512, 256, -5), None);
    }

    #[test]
    fn room_origin_applies_scale_and_offset() {
        let mut world = GridWorld::new();
        let mut room = GridRoom::new("a", 1, 1);
        room.origin = [-2, 3];
        room.y_offset = 1024;
        assert_eq!(world.room_origin_editor(&room), [-2560, 1280, 3840]);
        world.scale = GridScale { num: 9, den: 16 };
        assert_eq!(world.room_origin_editor(&room), [-1152, 576, 1728]);
    }

    #[test]
    fn only_the_empty_default_skips_serialisation() {
        assert!(SceneWorld::default().is_empty());
        assert!(!SceneWorld::Grid(GridWorld::new()).is_empty());
        assert!(SceneWorld::Empty.grid().is_none());
    }
}
