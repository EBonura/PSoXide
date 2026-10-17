//! Bands on hand-built worlds: faces of derived and authored walls, the
//! editing rules, neighbour stitching and the cook.

use super::bands::{Anchor, BandSet, BandV, NO_BANDS};
use super::derive::{edge_key, DerivedKind};
use super::edit::bands::{
    band_at, clear_bands, edit_bands, effective_set, paint_band, run_faces, BandScope,
};
use super::edit::FaceRef;
use super::faces::{all_faces, GridFace, GridFaceKind};
use super::room::GridRoom;
use super::sector::WallEdge;
use super::surface::{GridSurface, GridWall};
use super::units::GridScale;
use super::world::GridWorld;
use crate::face_uv::{FaceUv, UvMode};
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A derived `width` by `depth` room, floor 0, ceiling 2048, wall material
/// 9, scale 1/1 (a sector is 1024 editor units).
fn hall(width: usize, depth: usize) -> GridWorld {
    let mut room = GridRoom::new_derived("hall", width, depth);
    room.defaults.wall_material = Some(rid(9));
    for z in 0..depth {
        for x in 0..width {
            let s = room.sector_or_insert(x, z).unwrap();
            s.floor = Some(GridSurface::flat(0, Some(rid(1))));
            s.ceiling = Some(GridSurface::flat(2048, Some(rid(2))));
        }
    }
    GridWorld {
        scale: GridScale { num: 1, den: 1 },
        rooms: vec![room],
        ..GridWorld::default()
    }
}

/// trim 0..256 in material 1, body in 2, frieze 1792..2048 in 3.
fn trim_body_frieze() -> BandSet {
    let mut set = BandSet::single("walls", Some(rid(2)));
    assert!(set.insert_line(0, Anchor::Bottom(256)));
    assert!(set.insert_line(1, Anchor::Top(256)));
    set.set_band_material(0, Some(rid(1)));
    set.set_band_material(2, Some(rid(3)));
    set
}

fn north_wall(cell: i32) -> FaceRef {
    FaceRef::derived(0, [cell, 0], WallEdge::North, DerivedKind::Closure)
}

fn walls_on_north(faces: &[GridFace], cell: i32) -> Vec<&GridFace> {
    faces
        .iter()
        .filter(|f| {
            f.cell == [cell, 0]
                && matches!(
                    f.kind,
                    GridFaceKind::Derived {
                        edge: WallEdge::North,
                        ..
                    }
                )
        })
        .collect()
}

fn y_span(face: &GridFace) -> (i32, i32) {
    (
        face.grid.iter().map(|p| p[1]).min().unwrap(),
        face.grid.iter().map(|p| p[1]).max().unwrap(),
    )
}

fn area(face: &GridFace) -> i64 {
    // Wall faces lie in a vertical plane; use the plan length times height
    // through the shoelace of (distance along the wall, y).
    let along = |p: &[i32; 3]| i64::from(p[0]) + i64::from(p[2]);
    let n = face.grid.len();
    (0..n)
        .map(|i| {
            let (a, b) = (face.grid[i], face.grid[(i + 1) % n]);
            along(&a) * i64::from(b[1]) - along(&b) * i64::from(a[1])
        })
        .sum::<i64>()
        .abs()
        / 2
}

#[test]
fn a_room_default_set_cuts_every_derived_wall_into_three_materials() {
    let mut world = hall(3, 2);
    world.rooms[0].band_sets.push(trim_body_frieze());
    world.rooms[0].defaults.bands = Some(0);
    let faces = all_faces(&world);
    let wall = walls_on_north(&faces, 1);
    assert_eq!(wall.len(), 3, "{wall:?}");
    let mut by_height: Vec<(&GridFace, (i32, i32))> =
        wall.iter().map(|f| (*f, y_span(f))).collect();
    by_height.sort_by_key(|(_, s)| s.0);
    assert_eq!(
        by_height.iter().map(|(_, s)| *s).collect::<Vec<_>>(),
        vec![(0, 256), (256, 1792), (1792, 2048)]
    );
    assert_eq!(
        by_height
            .iter()
            .map(|(f, _)| f.material)
            .collect::<Vec<_>>(),
        vec![Some(rid(1)), Some(rid(2)), Some(rid(3))]
    );
    // Together they are the wall: 1024 by 2048.
    assert_eq!(wall.iter().map(|f| area(f)).sum::<i64>(), 1024 * 2048);
    // All three face the room.
    assert!(wall.iter().all(|f| f.normal[2] > 0.99));
}

#[test]
fn a_band_without_a_material_keeps_the_walls_own() {
    let mut world = hall(1, 1);
    let mut set = BandSet::single("s", None);
    set.insert_line(0, Anchor::Bottom(512));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    let faces = all_faces(&world);
    let wall = walls_on_north(&faces, 0);
    assert_eq!(wall.len(), 2);
    assert!(wall.iter().all(|f| f.material == Some(rid(9))));
}

#[test]
fn bands_are_a_per_run_choice_and_the_default_can_be_opted_out_of() {
    let mut world = hall(3, 1);
    // Band only the middle segment of the north wall: a run of one (the
    // neighbours differ from it in nothing, so the run is all three cells
    // unless the assignment splits it).
    let face = north_wall(1);
    assert_eq!(run_faces(&world, face).len(), 3);
    assert!(edit_bands(&mut world, face, BandScope::Run, |set| {
        set.insert_line(0, Anchor::Bottom(256))
    }));
    let faces = all_faces(&world);
    // The whole run took the set: every segment has two faces.
    for cell in 0..3 {
        assert_eq!(walls_on_north(&faces, cell).len(), 2, "cell {cell}");
    }
    // The south wall is another run and stays plain.
    let south: Vec<_> = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                GridFaceKind::Derived {
                    edge: WallEdge::South,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(south.len(), 3);
    // A room default plus an explicit none on one run.
    let mut set = BandSet::single("d", None);
    set.insert_line(0, Anchor::Top(256));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(1);
    assert!(clear_bands(&mut world, face, BandScope::Run));
    let faces = all_faces(&world);
    assert_eq!(walls_on_north(&faces, 1).len(), 1, "opted out");
    let south_faces = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                GridFaceKind::Derived {
                    edge: WallEdge::South,
                    ..
                }
            )
        })
        .count();
    assert_eq!(south_faces, 6, "the default bands the other runs");
    assert_eq!(
        world.rooms[0]
            .edge_overrides
            .iter()
            .next()
            .unwrap()
            .1
            .band_set,
        Some(NO_BANDS)
    );
}

#[test]
fn editing_a_shared_set_in_run_scope_copies_it_and_room_scope_edits_in_place() {
    let mut world = hall(2, 2);
    let mut set = BandSet::single("shared", Some(rid(5)));
    set.insert_line(0, Anchor::Bottom(256));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    let north = north_wall(0);
    // Run scope: the run gets its own copy, the default keeps the original.
    assert!(paint_band(
        &mut world,
        north,
        0,
        Some(rid(6)),
        BandScope::Run
    ));
    assert_eq!(world.rooms[0].band_sets.len(), 2);
    assert_eq!(world.rooms[0].band_sets[0].bands[0].material, Some(rid(5)));
    assert_eq!(world.rooms[0].band_sets[1].bands[0].material, Some(rid(6)));
    assert_eq!(effective_set(&world, north).map(|(i, _)| i), Some(1));
    // Painting the same band again edits the now private copy in place.
    assert!(paint_band(
        &mut world,
        north,
        0,
        Some(rid(7)),
        BandScope::Run
    ));
    assert_eq!(world.rooms[0].band_sets.len(), 2);
    assert_eq!(world.rooms[0].band_sets[1].bands[0].material, Some(rid(7)));
    // A no-op leaves the document alone.
    let before = world.clone();
    assert!(!paint_band(
        &mut world,
        north,
        0,
        Some(rid(7)),
        BandScope::Run
    ));
    assert_eq!(world, before);
    // Room scope edits what the face uses, wherever it is shared.
    let south = FaceRef::derived(0, [0, 1], WallEdge::South, DerivedKind::Closure);
    assert!(paint_band(
        &mut world,
        south,
        1,
        Some(rid(8)),
        BandScope::Room
    ));
    assert_eq!(world.rooms[0].band_sets.len(), 2);
    assert_eq!(world.rooms[0].band_sets[0].bands[1].material, Some(rid(8)));
    // Dropping the run's own set (back to the default's) prunes it.
    assert!(clear_bands(&mut world, north, BandScope::Run));
    assert!(clear_bands(&mut world, north, BandScope::Room));
    assert!(world.rooms[0].band_sets.is_empty() || world.rooms[0].defaults.bands.is_none());
}

#[test]
fn unreferenced_sets_are_dropped_and_the_indices_behind_them_move_down() {
    let mut world = hall(1, 2);
    let a = north_wall(0);
    let b = FaceRef::derived(0, [0, 1], WallEdge::South, DerivedKind::Closure);
    for (face, material) in [(a, 1), (b, 2)] {
        assert!(edit_bands(&mut world, face, BandScope::Run, |set| {
            set.set_band_material(0, Some(rid(material)))
        }));
    }
    assert_eq!(world.rooms[0].band_sets.len(), 2);
    // Take the first run's bands away: set 0 goes, set 1 becomes set 0.
    assert!(clear_bands(&mut world, a, BandScope::Run));
    assert_eq!(world.rooms[0].band_sets.len(), 1);
    assert_eq!(effective_set(&world, b).map(|(i, _)| i), Some(0));
    assert_eq!(
        effective_set(&world, b).unwrap().1.bands[0].material,
        Some(rid(2))
    );
}

#[test]
fn a_sloped_riser_gets_a_trim_that_follows_it_and_an_absolute_line_breaks_it() {
    // A platform whose top rises across the sector: the riser's top slopes.
    let mut world = hall(2, 1);
    {
        let room = &mut world.rooms[0];
        let platform = room.sector_or_insert(1, 0).unwrap();
        let mut floor = GridSurface::flat(0, Some(rid(1)));
        floor.heights = [512, 512, 1024, 1024];
        platform.floor = Some(floor);
    }
    let riser = FaceRef::derived(0, [0, 0], WallEdge::East, DerivedKind::Riser);
    assert!(riser.exists(&world));
    let mut set = BandSet::single("s", Some(rid(4)));
    set.insert_line(0, Anchor::Bottom(128));
    set.set_band_material(0, Some(rid(5)));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    let faces = all_faces(&world);
    let on: Vec<&GridFace> = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                GridFaceKind::Derived {
                    kind: DerivedKind::Riser,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(on.len(), 2, "trim and body");
    let total: i64 = on.iter().map(|f| area(f)).sum();
    // The riser runs from 0 to the platform's 512..1024 slope: a trapezoid
    // 1024 wide, mean height 768.
    assert_eq!(total, 1024 * 768);
    // An absolute line across the slope cuts the body in two.
    let mut set = world.rooms[0].band_sets[0].clone();
    set.insert_line(1, Anchor::Abs(768));
    world.rooms[0].band_sets[0] = set;
    let faces = all_faces(&world);
    let on: Vec<&GridFace> = faces
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                GridFaceKind::Derived {
                    kind: DerivedKind::Riser,
                    ..
                }
            )
        })
        .collect();
    assert!(on.len() >= 4, "{} faces", on.len());
    assert!(on.iter().all(|f| (3..=4).contains(&f.points.len())));
    assert_eq!(on.iter().map(|f| area(f)).sum::<i64>(), 1024 * 768);
}

#[test]
fn a_stack_of_authored_walls_shares_one_trim_at_its_base() {
    let mut world = hall(1, 1);
    {
        let room = &mut world.rooms[0];
        room.wall_mode = super::overrides::WallMode::Authored;
        let sector = room.sector_or_insert(0, 0).unwrap();
        let mut lower = GridWall::span(0, 1024, Some(rid(9)));
        let mut upper = GridWall::span(1024, 2048, Some(rid(9)));
        lower.band_set = Some(0);
        upper.band_set = Some(0);
        sector.walls_mut(WallEdge::North).push(lower);
        sector.walls_mut(WallEdge::North).push(upper);
        let mut set = BandSet::single("s", Some(rid(2)));
        set.insert_line(0, Anchor::Bottom(256));
        set.set_band_material(0, Some(rid(1)));
        room.band_sets.push(set);
    }
    let faces = all_faces(&world);
    let walls: Vec<&GridFace> = faces
        .iter()
        .filter(|f| matches!(f.kind, GridFaceKind::Wall { .. }))
        .collect();
    // The trim (0..256) is once at the bottom of the stack; the lower
    // segment's body above it and the whole upper segment are the body.
    let trim: Vec<_> = walls
        .iter()
        .filter(|f| f.material == Some(rid(1)))
        .collect();
    assert_eq!(trim.len(), 1);
    assert_eq!(y_span(trim[0]), (0, 256));
    let body_area: i64 = walls
        .iter()
        .filter(|f| f.material == Some(rid(2)))
        .map(|f| area(f))
        .sum();
    assert_eq!(body_area, 1024 * (2048 - 256));
}

/// Whether any vertex of any face lies strictly inside an edge of another
/// face of the list: a T-junction.
fn t_junctions(faces: &[&GridFace]) -> Vec<String> {
    let mut found = Vec::new();
    for (i, a) in faces.iter().enumerate() {
        for (j, b) in faces.iter().enumerate() {
            if i == j {
                continue;
            }
            let n = b.grid.len();
            for k in 0..n {
                let (p, q) = (b.grid[k], b.grid[(k + 1) % n]);
                for v in &a.grid {
                    if *v == p || *v == q {
                        continue;
                    }
                    let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
                    let w = [v[0] - p[0], v[1] - p[1], v[2] - p[2]];
                    let cross = [
                        i64::from(d[1]) * i64::from(w[2]) - i64::from(d[2]) * i64::from(w[1]),
                        i64::from(d[2]) * i64::from(w[0]) - i64::from(d[0]) * i64::from(w[2]),
                        i64::from(d[0]) * i64::from(w[1]) - i64::from(d[1]) * i64::from(w[0]),
                    ];
                    let within = (0..3).all(|c| {
                        let (lo, hi) = (p[c].min(q[c]), p[c].max(q[c]));
                        v[c] >= lo && v[c] <= hi
                    });
                    if cross == [0; 3] && within {
                        found.push(format!("{v:?} inside {p:?}-{q:?}"));
                    }
                }
            }
        }
    }
    found
}

#[test]
fn a_banded_wall_with_differing_neighbours_has_no_t_junction() {
    let mut world = hall(3, 1);
    let mut a = BandSet::single("a", Some(rid(2)));
    a.insert_line(0, Anchor::Bottom(256));
    a.insert_line(1, Anchor::Top(256));
    let mut b = BandSet::single("b", Some(rid(3)));
    b.insert_line(0, Anchor::Bottom(512));
    b.insert_line(1, Anchor::Top(640));
    // Segment 0 uses set a, segment 1 set b, segment 2 is plain. Different
    // materials on each keep them separate runs.
    {
        let room = &mut world.rooms[0];
        room.band_sets.push(a);
        room.band_sets.push(b);
        room.edge_overrides
            .edit(edge_key([0, 0], WallEdge::North), |o| {
                o.band_set = Some(0);
                o.material = Some(rid(21));
            });
        room.edge_overrides
            .edit(edge_key([1, 0], WallEdge::North), |o| {
                o.band_set = Some(1);
                o.material = Some(rid(22));
            });
        room.edge_overrides
            .edit(edge_key([2, 0], WallEdge::North), |o| {
                o.band_set = Some(NO_BANDS);
                o.material = Some(rid(23));
            });
    }
    let faces = all_faces(&world);
    let wall: Vec<&GridFace> = (0..3).flat_map(|c| walls_on_north(&faces, c)).collect();
    assert!(wall.len() > 6, "stitching adds triangles: {}", wall.len());
    // Banded wall and neighbours: every vertex on the shared vertical
    // edges is a vertex of both sides.
    assert_eq!(t_junctions(&wall), Vec::<String>::new());
    // Same total area as the three plain segments.
    assert_eq!(wall.iter().map(|f| area(f)).sum::<i64>(), 3 * 1024 * 2048);
}

#[test]
fn rooms_without_bands_get_no_stitching_and_derived_walls_are_not_cut_into_courses() {
    let world = hall(2, 2);
    let faces = all_faces(&world);
    // One face per outline edge, four sectors tall in nothing: 8 edges.
    let closure = faces
        .iter()
        .filter(|f| matches!(f.kind, GridFaceKind::Derived { .. }))
        .count();
    assert_eq!(closure, 8);
    assert!(faces
        .iter()
        .filter(|f| matches!(f.kind, GridFaceKind::Derived { .. }))
        .all(|f| f.points.len() == 4));
}

#[test]
fn a_fit_band_fits_exactly_between_its_lines_and_from_bottom_starts_on_the_line() {
    let mut world = hall(1, 1);
    let mut set = BandSet::single("s", Some(rid(2)));
    set.insert_line(0, Anchor::Top(256));
    // The frieze: fit 2 across, 1 between its lines.
    set.bands[1].uv = FaceUv {
        mode: UvMode::fit(2, 1),
        ..FaceUv::default()
    };
    // The body: world mapping from the bottom.
    set.bands[0].v = BandV::FromBottom;
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    let faces = all_faces(&world);
    let wall = walls_on_north(&faces, 0);
    assert_eq!(wall.len(), 2);
    let frieze = wall.iter().find(|f| y_span(f) == (1792, 2048)).unwrap();
    let v: Vec<f64> = frieze.texels.iter().map(|t| t[1]).collect();
    let (lo, hi) = (
        v.iter().cloned().fold(f64::MAX, f64::min),
        v.iter().cloned().fold(f64::MIN, f64::max),
    );
    assert!((hi - lo - 64.0).abs() < 1e-6, "one tile high: {lo}..{hi}");
    let u: Vec<f64> = frieze.texels.iter().map(|t| t[0]).collect();
    let span_u =
        u.iter().cloned().fold(f64::MIN, f64::max) - u.iter().cloned().fold(f64::MAX, f64::min);
    assert!((span_u - 128.0).abs() < 1e-6, "two tiles across: {span_u}");
    // The body's V is zero on its bottom line: the lowest texel row is the
    // one at the base, and rising is minus.
    let body = wall.iter().find(|f| y_span(f) == (0, 1792)).unwrap();
    let bottom_v: Vec<f64> = body
        .grid
        .iter()
        .zip(&body.texels)
        .filter(|(p, _)| p[1] == 0)
        .map(|(_, t)| t[1])
        .collect();
    assert!(bottom_v.iter().all(|v| v.abs() < 1e-9), "{bottom_v:?}");
}

#[test]
fn band_at_finds_the_band_under_a_point_and_band_sets_round_trip_through_ron() {
    let mut world = hall(1, 1);
    world.rooms[0].band_sets.push(trim_body_frieze());
    world.rooms[0].defaults.bands = Some(0);
    let wall = north_wall(0);
    assert_eq!(band_at(&world, wall, 500, 100), Some(0));
    assert_eq!(band_at(&world, wall, 500, 1000), Some(1));
    assert_eq!(band_at(&world, wall, 500, 1900), Some(2));
    assert_eq!(band_at(&world, wall, 500, 2500), None);
    // Document round trip.
    let text = ron::to_string(&world).unwrap();
    let back: GridWorld = ron::from_str(&text).unwrap();
    assert_eq!(back, world);
    assert_eq!(all_faces(&back).len(), all_faces(&world).len());
}

#[test]
fn a_hole_in_a_stack_of_walls_keeps_its_bands_on_the_pieces_that_stand() {
    // A wall with a hole: 0..512 and 1024..2048 stand, 512..1024 is open.
    // The trim sits at the base of the stack, the frieze at its top.
    let mut world = hall(1, 1);
    {
        let room = &mut world.rooms[0];
        room.wall_mode = super::overrides::WallMode::Authored;
        let sector = room.sector_or_insert(0, 0).unwrap();
        for (lo, hi) in [(0, 512), (1024, 2048)] {
            let mut wall = GridWall::span(lo, hi, Some(rid(9)));
            wall.band_set = Some(0);
            sector.walls_mut(WallEdge::North).push(wall);
        }
        let mut set = BandSet::single("s", Some(rid(2)));
        set.insert_line(0, Anchor::Bottom(256));
        set.insert_line(1, Anchor::Top(256));
        set.set_band_material(0, Some(rid(1)));
        set.set_band_material(2, Some(rid(3)));
        room.band_sets.push(set);
    }
    let faces = all_faces(&world);
    let walls: Vec<&GridFace> = faces
        .iter()
        .filter(|f| matches!(f.kind, GridFaceKind::Wall { .. }))
        .collect();
    let by = |m: u64| -> Vec<(i32, i32)> {
        let mut v: Vec<(i32, i32)> = walls
            .iter()
            .filter(|f| f.material == Some(rid(m)))
            .map(|f| y_span(f))
            .collect();
        v.sort();
        v
    };
    assert_eq!(by(1), vec![(0, 256)], "trim at the base of the stack");
    assert_eq!(by(3), vec![(1792, 2048)], "frieze at the top of the stack");
    assert_eq!(
        by(2),
        vec![(256, 512), (1024, 1792)],
        "body only where it stands"
    );
    // Nothing in the hole.
    assert!(walls
        .iter()
        .all(|f| y_span(f).1 <= 512 || y_span(f).0 >= 1024));
    let total: i64 = walls.iter().map(|f| area(f)).sum();
    assert_eq!(total, 1024 * (512 + 1024));
}

#[test]
fn the_editor_helpers_place_lines_in_order_and_clamp_a_drag_between_neighbours() {
    use super::edit::bands::{anchor_for, insert_index, move_line};
    let mut world = hall(1, 1);
    let wall = north_wall(0);
    // A wall 2048 tall: the lower quarter follows the floor, the upper the
    // ceiling, the rest is a datum.
    assert_eq!(
        anchor_for(&world, wall, 500, 100),
        Some(Anchor::Bottom(100))
    );
    assert_eq!(anchor_for(&world, wall, 500, 1000), Some(Anchor::Abs(1000)));
    assert_eq!(anchor_for(&world, wall, 500, 1900), Some(Anchor::Top(148)));
    // Lines at 512 and 1536.
    let mut set = BandSet::single("s", None);
    set.insert_line(0, Anchor::Bottom(512));
    set.insert_line(1, Anchor::Top(512));
    world.rooms[0].band_sets.push(set);
    world.rooms[0].defaults.bands = Some(0);
    assert_eq!(insert_index(&world, wall, 500, 100), 0);
    assert_eq!(insert_index(&world, wall, 500, 1000), 1);
    assert_eq!(insert_index(&world, wall, 500, 1900), 2);
    // Dragging the lower line far past the upper one stops at it.
    assert!(move_line(&mut world, wall, 0, 500, 1800, BandScope::Room));
    let lines = world.rooms[0].band_sets[0].lines.clone();
    assert_eq!(lines[0], Anchor::Bottom(1536), "{lines:?}");
    // Dragging below the bottom stops at the bottom.
    assert!(move_line(&mut world, wall, 0, 500, -300, BandScope::Room));
    assert_eq!(world.rooms[0].band_sets[0].lines[0], Anchor::Bottom(0));
}

#[test]
fn band_set_problems_are_reported_without_blocking_the_cook() {
    use super::limits::{validate, GridIssueKind, Severity};
    let mut world = hall(1, 1);
    // A set with the wrong number of bands, and a wall naming a set that
    // does not exist.
    world.rooms[0].band_sets.push(BandSet {
        name: "bad".to_string(),
        lines: vec![Anchor::Abs(100)],
        bands: Vec::new(),
    });
    world.rooms[0].defaults.bands = Some(7);
    let issues = validate(&world);
    let kinds: Vec<_> = issues.iter().map(|i| i.kind.clone()).collect();
    assert!(
        kinds.contains(&GridIssueKind::BadBandSet { index: 0 }),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&GridIssueKind::BandSetMissing { index: 7 }),
        "{kinds:?}"
    );
    assert!(issues.iter().all(|i| i.severity() == Severity::Warning));
    // The wall draws plain meanwhile.
    assert_eq!(walls_on_north(&all_faces(&world), 0).len(), 1);
}
