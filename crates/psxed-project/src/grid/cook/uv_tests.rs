//! The cook of the UV model: fit and world mappings land the expected
//! frames, the one-tile-per-primitive cut, material defaults, and the
//! reference levels cooking exactly as before.

use psx_grid::{Quad, RoomView};

use super::tests::{cook, load_reference, TestBinder};
use super::{cook_level, BoundMaterial, CookInput, MaterialBinder};
use crate::face_uv::{FaceUv, UvMode};
use crate::grid::room::GridRoom;
use crate::grid::surface::GridSurface;
use crate::grid::units::GridScale;
use crate::grid::world::GridWorld;
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

fn floor_world(uv: FaceUv) -> GridWorld {
    let mut room = GridRoom::new("r", 1, 1);
    let mut floor = GridSurface::flat(0, Some(rid(7)));
    floor.tri_a.uv = uv;
    room.sector_or_insert(0, 0).unwrap().floor = Some(floor);
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
    }
}

fn corners(q: &Quad) -> Vec<(u8, u8)> {
    q.uv.iter()
        .map(|uv| ((uv & 0xff) as u8, (uv >> 8) as u8))
        .collect()
}

fn fit(n: u16) -> FaceUv {
    FaceUv {
        mode: UvMode::fit(n, n),
        ..FaceUv::default()
    }
}

#[test]
fn fit_one_by_one_is_one_primitive_holding_one_whole_tile() {
    let cooked = cook(&floor_world(fit(1))).unwrap();
    let t = &cooked.tables[0];
    assert_eq!(t.quads.len(), 1);
    let mut c = corners(&t.quads[0]);
    c.sort();
    // The far edge stops one texel short of the tile, as it always has.
    assert_eq!(c, vec![(0, 0), (0, 63), (63, 0), (63, 63)]);
    assert_eq!(cooked.report.rooms[0].uv_split_quads, 0);
}

#[test]
fn fit_2x2_and_4x4_become_one_primitive_per_tile() {
    for n in [2u16, 4] {
        let cooked = cook(&floor_world(fit(n))).unwrap();
        let t = &cooked.tables[0];
        assert_eq!(t.quads.len(), usize::from(n * n), "fit {n}x{n}");
        assert_eq!(
            cooked.report.rooms[0].uv_split_quads,
            usize::from(n * n) - 1
        );
        assert_eq!(cooked.report.rooms[0].faces, 1);
        // Every piece is one whole tile.
        for q in &t.quads {
            let mut c = corners(q);
            c.sort();
            assert_eq!(c, vec![(0, 0), (0, 63), (63, 0), (63, 63)], "{q:?}");
        }
        // The pieces tile the sector exactly: their plan areas add up.
        let area = |q: &Quad| -> i64 {
            let p = |k: usize| {
                let v = t.vertices[usize::from(q.v[k])];
                (i64::from(v.x), i64::from(v.z))
            };
            (0..4)
                .map(|k| {
                    let (a, b) = (p(k), p((k + 1) % 4));
                    a.0 * b.1 - b.0 * a.1
                })
                .sum::<i64>()
                .abs()
        };
        let total: i64 = t.quads.iter().map(area).sum();
        assert_eq!(
            total,
            2 * 80 * 80,
            "twice the plan area of an 80 unit sector"
        );
    }
}

#[test]
fn world_mode_follows_the_density() {
    // The default: half a tile per sector, one primitive.
    assert_eq!(
        cook(&floor_world(FaceUv::default())).unwrap().tables[0]
            .quads
            .len(),
        1
    );
    // 128 texels per sector is two tiles per sector.
    let dense = FaceUv {
        mode: UvMode::World {
            texels_per_sector: [128, 128],
        },
        ..FaceUv::default()
    };
    assert_eq!(cook(&floor_world(dense)).unwrap().tables[0].quads.len(), 4);
}

/// A binder that gives material 7 a tiling default.
struct Defaulting(UvMode);

impl MaterialBinder for Defaulting {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        TestBinder.bind(material)
    }

    fn uv_default(&self, material: ResourceId) -> Option<UvMode> {
        (material == rid(7)).then_some(self.0)
    }
}

#[test]
fn a_material_default_applies_to_inherit_faces_only() {
    let world = floor_world(FaceUv::default());
    let mut binder = Defaulting(UvMode::fit(2, 2));
    let cooked = cook_level(
        CookInput {
            world: &world,
            binder: &mut binder,
            lights: &[],
            first_pack_sector: 100,
            lod_far_cells: 0,
        },
        None,
    )
    .unwrap();
    assert_eq!(cooked.tables[0].quads.len(), 4, "inherits fit 2x2");
    // A face that names its own mode ignores the default.
    let own = floor_world(FaceUv {
        mode: UvMode::LEGACY,
        ..FaceUv::default()
    });
    let mut binder = Defaulting(UvMode::fit(2, 2));
    let cooked = cook_level(
        CookInput {
            world: &own,
            binder: &mut binder,
            lights: &[],
            first_pack_sector: 100,
            lod_far_cells: 0,
        },
        None,
    )
    .unwrap();
    assert_eq!(cooked.tables[0].quads.len(), 1);
}

#[test]
fn the_cooked_pieces_read_back_and_validate() {
    let cooked = cook(&floor_world(fit(4))).unwrap();
    let words = psx_grid::write::aligned_copy(&cooked.chunks[0]);
    let view = RoomView::parse(psx_grid::write::words_as_bytes(
        &words,
        cooked.chunks[0].len(),
    ))
    .unwrap();
    view.validate_full(cooked.level.materials.len()).unwrap();
}

#[test]
fn the_reference_levels_cook_with_no_tile_cuts() {
    for name in [
        "grid-cathedral",
        "grid-level008",
        "grid-dungeon",
        "grid-cave",
        "grid-sewers",
        "grid-west",
    ] {
        let project = load_reference(name).unwrap();
        let world = project.active_scene().world.grid().unwrap();
        let cooked = cook(world).unwrap();
        let cuts: usize = cooked.report.rooms.iter().map(|r| r.uv_split_quads).sum();
        let overflow: usize = cooked.report.rooms.iter().map(|r| r.uv_overflow).sum();
        let quads: usize = cooked
            .report
            .rooms
            .iter()
            .map(|r| r.quads + r.triangles)
            .sum();
        eprintln!("{name}: {quads} primitives, {cuts} from tile cuts, {overflow} uv overflow");
        assert_eq!(cuts, 0, "{name}");
    }
}
