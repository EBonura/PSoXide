//! The cook of the UV model: fit and world mappings land the expected
//! frames, the one-tile-per-primitive cut, material defaults, and the
//! reference levels cooking exactly as before.

use psx_grid::{Quad, RoomView};

use super::tests::{cook, cook_with, load_reference, TestBinder};
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
    room.sector_or_insert(0, 0).unwrap().set_floor(Some(floor));
    GridWorld {
        scale: GridScale::DEFAULT,
        rooms: vec![room],
        ..GridWorld::default()
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
fn without_the_window_fit_2x2_and_4x4_become_one_primitive_per_tile() {
    for n in [2u16, 4] {
        let cooked = cook_with(&floor_world(fit(n)), false).unwrap();
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
            assert_eq!(q.flags2, 0);
        }
        // The pieces tile the sector exactly: their plan areas add up.
        let total: i64 = t.quads.iter().map(|q| plan_area(t, q)).sum();
        assert_eq!(
            total,
            2 * 80 * 80,
            "twice the plan area of an 80 unit sector"
        );
    }
}

fn plan_area(t: &psx_grid::write::RoomChunk, q: &Quad) -> i64 {
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
}

#[test]
fn with_the_window_fit_2x2_and_4x4_are_one_repeating_primitive() {
    for n in [2u16, 4] {
        let cooked = cook(&floor_world(fit(n))).unwrap();
        let t = &cooked.tables[0];
        assert_eq!(t.quads.len(), 1, "fit {n}x{n}");
        assert_eq!(cooked.report.rooms[0].uv_split_quads, 0);
        let q = &t.quads[0];
        assert_eq!(q.flags2, psx_grid::QUAD2_REPEAT);
        // Raw UVs: the far edge is n tiles out, its last texel clamped.
        let far = (u32::from(n) * 64 - 1) as u8;
        let mut c = corners(q);
        c.sort();
        assert_eq!(c, vec![(0, 0), (0, far), (far, 0), (far, far)], "{q:?}");
        assert_eq!(plan_area(t, q), 2 * 80 * 80);
        // The material carries a texture shape the window can use.
        assert_eq!(
            psx_grid::tex_size(
                cooked.level.materials[usize::from(t.material_use[usize::from(q.material)])]
                    .tex_shape
            ),
            Some([64, 64])
        );
    }
}

#[test]
fn a_fit_above_four_tiles_is_cut_into_repeating_pieces() {
    let cooked = cook(&floor_world(FaceUv {
        mode: UvMode::fit(8, 8),
        ..FaceUv::default()
    }))
    .unwrap();
    let t = &cooked.tables[0];
    assert_eq!(t.quads.len(), 4);
    assert_eq!(cooked.report.rooms[0].uv_split_quads, 3);
    assert!(t.quads.iter().all(|q| q.flags2 == psx_grid::QUAD2_REPEAT));
    let total: i64 = t.quads.iter().map(|q| plan_area(t, q)).sum();
    assert_eq!(total, 2 * 80 * 80);
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
    // The window holds both tiles in one primitive; without it the face is
    // cut into four.
    let world = floor_world(dense);
    assert_eq!(cook(&world).unwrap().tables[0].quads.len(), 1);
    assert_eq!(cook_with(&world, false).unwrap().tables[0].quads.len(), 4);
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
            texture_repeat: true,
        },
        None,
    )
    .unwrap();
    assert_eq!(cooked.tables[0].quads.len(), 1, "inherits fit 2x2");
    assert_eq!(cooked.tables[0].quads[0].flags2, psx_grid::QUAD2_REPEAT);
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
            texture_repeat: true,
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

/// Primitives, tile cuts and repeating primitives of a cook.
fn census(world: &GridWorld, repeat: bool) -> (usize, usize, usize) {
    let cooked = cook_with(world, repeat).unwrap();
    let sum = |f: &dyn Fn(&super::RoomReport) -> usize| cooked.report.rooms.iter().map(f).sum();
    (
        sum(&|r| r.quads + r.triangles),
        sum(&|r| r.uv_split_quads),
        cooked
            .tables
            .iter()
            .flat_map(|t| &t.quads)
            .filter(|q| q.flags2 & psx_grid::QUAD2_REPEAT != 0)
            .count(),
    )
}

#[test]
fn the_reference_levels_cut_far_fewer_faces_through_the_window() {
    // Legacy faces are cut like any other, so a tall wall on the old
    // mapping matches what the preview wraps. One tile per primitive cuts
    // them often; four tiles through the window almost never.
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
        let (plain_total, plain_cuts, plain_repeat) = census(world, false);
        let (total, cuts, repeating) = census(world, true);
        eprintln!(
            "{name}: one tile per primitive {plain_total} primitives (+{plain_cuts} from cuts), \
             window {total} primitives (+{cuts} from cuts, {repeating} repeating)"
        );
        assert_eq!(plain_repeat, 0, "{name}");
        assert!(cuts <= plain_cuts, "{name}");
        assert_eq!(total - cuts, plain_total - plain_cuts, "{name}: same faces");
        let overflow: usize = cook(world)
            .unwrap()
            .report
            .rooms
            .iter()
            .map(|r| r.uv_overflow)
            .sum();
        assert_eq!(overflow, 0, "{name}");
    }
}
