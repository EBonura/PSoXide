//! Tests of styles, profiles, vaults and styled runs.

use std::collections::{BTreeMap, BTreeSet};

use crate::grid::edit::CellRect;
use crate::grid::world::GridWorld;

use super::corridor::{LegShape, Waypoint};
use super::room::RoomTarget;
use super::room_tests::empty;
use super::style::{self, BayKind, Curve, Profile, Roles, Spandrel, Style, Vault};
use super::styled::{
    lay_styled_corridor, lay_styled_room, StyleRefusal, StyledCorridor, StyledRoom,
};

fn corridor(style: Style, len: i32, width: i32) -> StyledCorridor {
    StyledCorridor {
        target: RoomTarget::New("run".into()),
        waypoints: vec![Waypoint::at_height([0, 0], 0), Waypoint::at([len, 0])],
        width,
        base: 0,
        leg: LegShape::Ramp,
        style,
        roles: Roles::default(),
    }
}

#[test]
fn a_profile_is_a_small_table_of_slopes_on_the_lattice() {
    let f = style::scifi_rib().floor;
    let samples: Vec<i32> = (0..=8).map(|k| f.sample(8, k)).collect();
    assert_eq!(samples, vec![0, 0, 0, 128, 128, 128, 0, 0, 0], "_/-\\_");
    // A barrel: zero at the sides, the rise at the middle, symmetric.
    let barrel = Profile::Arc {
        curve: Curve::Round,
        rise: 1024,
    };
    let b: Vec<i32> = (0..=8).map(|k| barrel.sample(8, k)).collect();
    assert_eq!((b[0], b[4], b[8]), (0, 1024, 0));
    for k in 0..=8 {
        assert_eq!(b[k], b[8 - k]);
        assert_eq!(b[k] % 64, 0);
    }
    // A V gutter is two points.
    let v = Profile::Points(vec![(0, 0), (500, -128), (1000, 0)]);
    assert_eq!(v.sample(4, 2), -128);
    // A pointed barrel is leaner at the shoulder than a round one.
    let p = Profile::Arc {
        curve: Curve::Pointed,
        rise: 1024,
    };
    assert!(p.sample(8, 2) <= barrel.sample(8, 2));
}

#[test]
fn vaults_have_the_shapes_they_are_named_for() {
    let groin = Vault::Groin {
        curve: Curve::Round,
        rise: 1024,
    };
    // Crown at the middle of every edge and the centre, spring at the corners.
    assert_eq!(groin.offset(8, 8, 4, 4), 1024);
    assert_eq!(groin.offset(8, 8, 4, 0), 1024);
    assert_eq!(groin.offset(8, 8, 0, 0), 0);
    // The groin is the higher barrel: along a diagonal it dips to the ridge.
    assert!(groin.offset(8, 8, 2, 2) < groin.offset(8, 8, 4, 4));
    let dome = Vault::Dome {
        pointed: false,
        rise: 2048,
        spandrel: Spandrel::Flat,
    };
    assert_eq!(dome.offset(16, 16, 8, 8), 2048);
    assert_eq!(
        dome.offset(16, 16, 0, 8),
        0,
        "the circle meets the edge at the spring"
    );
    assert_eq!(dome.offset(16, 16, 0, 0), 0, "flat spandrel");
    // Rings: 8 distinct heights from the rim to the crown at 1 click.
    let rings: BTreeSet<i32> = (0..=8).map(|k| dome.offset(16, 16, 8, 8 - k)).collect();
    assert!(rings.len() >= 6, "{rings:?}");
    // Symmetric over the four quadrants.
    for (x, z) in [(3, 5), (6, 2), (1, 7)] {
        let h = dome.offset(16, 16, x, z);
        assert_eq!(h, dome.offset(16, 16, 16 - x, z));
        assert_eq!(h, dome.offset(16, 16, x, 16 - z));
    }
    let sloped = Vault::Dome {
        pointed: true,
        rise: 3072,
        spandrel: Spandrel::Sloped { raise: 512 },
    };
    assert_eq!(sloped.offset(16, 16, 0, 0), 512);
    assert!(
        sloped.offset(16, 16, 8, 8) >= 2944,
        "a pointed crown reaches the rise"
    );
    let coffer = Vault::Coffer {
        depth: 128,
        steps: 3,
    };
    let line: Vec<i32> = (0..=8).map(|k| coffer.offset(8, 8, k, 4)).collect();
    assert_eq!(line, vec![0, 128, 256, 384, 384, 384, 256, 128, 0]);
}

#[test]
fn every_built_in_style_round_trips_through_ron_and_checks() {
    for s in style::built_in() {
        s.check().unwrap();
        let back = Style::from_ron(&s.to_ron()).unwrap();
        assert_eq!(back, s, "{}", s.name);
    }
    assert!(Style::from_ron("(nonsense").is_err());
}

/// Every vertex of every floor (or ceiling) leaf, by world click: the set of
/// heights seen there.
fn vertex_heights(w: &GridWorld, room: usize, ceiling: bool) -> BTreeMap<[i32; 2], BTreeSet<i32>> {
    let mut out: BTreeMap<[i32; 2], BTreeSet<i32>> = BTreeMap::new();
    for (key, s) in w.rooms[room].leaves() {
        let surf = if ceiling {
            s.ground_ceiling()
        } else {
            s.ground_floor()
        };
        let Some(surf) = surf else { continue };
        let n = key.edge_clicks();
        let corners = [[0, 0], [n, 0], [n, n], [0, n]];
        for (i, c) in corners.iter().enumerate() {
            out.entry([key.x + c[0], key.z + c[1]])
                .or_default()
                .insert(surf.heights[i]);
        }
    }
    out
}

#[test]
fn styled_runs_tile_without_cracks_and_sit_on_the_lattice() {
    for (style, width, check_ceiling) in [
        (style::scifi_rib(), 2, false),
        (style::gothic_nave(), 2, false),
        (style::cloister(), 2, true),
        (style::crypt(), 2, false),
        (style::plain(), 1, true),
    ] {
        let mut w = empty();
        let name = style.name.clone();
        let out = lay_styled_corridor(&mut w, &corridor(style, 10, width))
            .unwrap_or_else(|e| panic!("{name}: {}", e.describe()));
        let room = &w.rooms[out.room];
        for (key, s) in room.leaves() {
            for surf in s.surfaces() {
                for h in surf.heights {
                    assert_eq!(h % 64, 0, "{name} {key:?}");
                }
            }
        }
        // Inside the leg: the plain landings at the two ends meet it in a
        // step (a derived riser), which is not a crack.
        let inside = |v: &[i32; 2]| v[0] > 8 && v[0] < 36 && v[1] >= 0;
        for (v, hs) in vertex_heights(&w, out.room, false) {
            if inside(&v) {
                assert_eq!(hs.len(), 1, "{name}: floor cracks at {v:?}: {hs:?}");
            }
        }
        if check_ceiling {
            for (v, hs) in vertex_heights(&w, out.room, true) {
                if inside(&v) {
                    assert_eq!(hs.len(), 1, "{name}: ceiling cracks at {v:?}: {hs:?}");
                }
            }
        }
        assert!(crate::grid::tree::check(room).is_empty(), "{name}");
        crate::grid::cook::tests::cook(&w).unwrap_or_else(|e| panic!("{name} cooks: {e}"));
    }
}

#[test]
fn only_as_many_leaves_as_the_profile_needs() {
    let mut w = empty();
    let plain = lay_styled_corridor(&mut w, &corridor(style::plain(), 10, 1)).unwrap();
    // A plain corridor is whole sectors, nothing finer.
    assert!(w.rooms[plain.room].leaves().all(|(k, _)| k.level == 2));
    let mut w = empty();
    let rib = lay_styled_corridor(&mut w, &corridor(style::scifi_rib(), 10, 2)).unwrap();
    let by_level: Vec<usize> = (2..=4)
        .map(|l| {
            w.rooms[rib.room]
                .leaves()
                .filter(|(k, _)| k.level == l)
                .count()
        })
        .collect();
    assert!(by_level[2] > 0 && by_level[0] > 0, "{by_level:?}");
}

#[test]
fn the_rhythm_repeats_and_a_rib_narrows_and_lowers() {
    let mut w = empty();
    let out = lay_styled_corridor(&mut w, &corridor(style::scifi_rib(), 12, 2)).unwrap();
    let room = &w.rooms[out.room];
    // The leg is cells x 2..=10 (the landings take x 0..=1 and 11..=12);
    // rhythm cycle 4 clicks: along 0 (wx = 8) is a rib.
    let ceil_at = |wx: i32, wz: i32| {
        room.leaf_at([wx, wz])
            .and_then(|(_, s)| s.ground_ceiling().map(|c| c.heights))
    };
    // Across clicks 0 and 7 are inset in a rib, so the cell is void there.
    assert!(
        room.leaf_at([8, 0]).is_none(),
        "rib inset leaves the edge void"
    );
    assert!(room.leaf_at([9, 0]).is_some(), "panel keeps the full width");
    let rib = ceil_at(8, 4).unwrap();
    let panel = ceil_at(9, 4).unwrap();
    assert!(
        rib.iter().max() < panel.iter().max(),
        "the rib is lower: {rib:?} {panel:?}"
    );
    // The pattern repeats: along 4 and along 8 look alike.
    assert_eq!(ceil_at(8, 4), ceil_at(12, 4));
    assert_eq!(ceil_at(9, 5), ceil_at(13, 5));
}

#[test]
fn refusals_leave_the_document_alone() {
    let mut w = empty();
    let before = w.clone();
    let mut low = style::plain();
    low.headroom = 1024;
    low.rhythm[0].ceiling_offset = -512;
    assert!(matches!(
        lay_styled_corridor(&mut w, &corridor(low, 6, 1)),
        Err(StyleRefusal::TooLow { .. })
    ));
    let mut steep = style::plain();
    steep.floor = Profile::Points(vec![(0, 0), (250, 512), (500, 0)]);
    assert!(matches!(
        lay_styled_corridor(&mut w, &corridor(steep, 6, 1)),
        Err(StyleRefusal::FloorTooSteep { .. })
    ));
    let mut bad = style::plain();
    bad.rhythm = Vec::<BayKind>::new();
    assert!(matches!(
        lay_styled_corridor(&mut w, &corridor(bad, 6, 1)),
        Err(StyleRefusal::BadStyle(_))
    ));
    let mut step = style::plain();
    step.rhythm = vec![BayKind::plain("a", 4), {
        let mut b = BayKind::plain("b", 4);
        b.floor_offset = 1024;
        b
    }];
    assert!(matches!(
        lay_styled_corridor(&mut w, &corridor(step, 6, 1)),
        Err(StyleRefusal::StepTooTall { .. })
    ));
    assert!(w == before);
}

#[test]
fn a_domed_chamber_has_its_crown_in_the_middle_and_four_piers() {
    let mut w = empty();
    let s = style::domed_chamber();
    let out = lay_styled_room(
        &mut w,
        &StyledRoom {
            target: RoomTarget::New("dome".into()),
            rect: CellRect::from_corners([0, 0], [3, 3]),
            base: 0,
            style: s,
            roles: Roles::default(),
        },
    )
    .unwrap();
    assert_eq!(out.piers, 4);
    let room = &w.rooms[out.room];
    let ceil = |wx: i32, wz: i32| {
        room.leaf_at([wx, wz])
            .and_then(|(_, s)| s.ground_ceiling().map(|c| c.heights))
            .unwrap()
    };
    let centre = ceil(7, 7)[2].max(ceil(8, 8)[0]);
    assert!(
        centre >= 2560 + 2816,
        "the crown reaches the rise: {centre}"
    );
    // Corners sit at the spandrel's raise over the spring line.
    assert_eq!(ceil(0, 0)[0], 2560 + 512);
    // Mirror symmetry: the west and east quarter hold the same heights.
    assert_eq!(ceil(3, 5)[0], ceil(12, 5)[1]);
    crate::grid::cook::tests::cook(&w).expect("a dome cooks");
}

#[test]
fn the_six_graybox_roles_are_six_different_materials_in_a_new_project() {
    let mut project = crate::ProjectDocument::new("t");
    crate::grid::graybox_set::add_to(&mut project);
    let r = Roles::graybox(&project);
    let all = [r.floor, r.wall, r.ceiling, r.trim, r.accent, r.ground];
    assert!(all.iter().all(Option::is_some), "{all:?}");
    let distinct: BTreeSet<String> = all.iter().map(|m| format!("{m:?}")).collect();
    assert_eq!(distinct.len(), 6);
}

#[test]
fn the_walkability_analysis_reads_styled_corridors_with_void_insets() {
    use crate::grid::walk::{analyse, Spawn};
    for (style, width) in [
        (style::scifi_rib(), 2),
        (style::crypt(), 2),
        (style::gothic_nave(), 2),
    ] {
        let name = style.name.clone();
        let mut w = empty();
        let out = lay_styled_corridor(&mut w, &corridor(style, 10, width)).unwrap();
        let a = analyse(
            &w,
            &[Spawn {
                room: out.room,
                cell: [0, 0],
            }],
            &psx_engine::WalkLimits::DEFAULT,
        )
        .unwrap();
        // 12 by 2 sectors: every one stands and every one is reached.
        assert_eq!(a.rooms[out.room].standable_count(), 24, "{name}");
        assert_eq!(a.counts()[4], 0, "{name}: unreachable");
    }
}

#[test]
fn a_solid_lintel_over_a_doorway_does_not_block_the_walk_analysis() {
    use super::opening::{cut_opening, OpeningKind, OpeningParams, OpeningProfile};
    use super::room::{lay_room, Footprint, LeafPolicy, RoomParams};
    use crate::grid::edit::CellRect as R;
    use crate::grid::sector::WallEdge;
    use crate::grid::walk::{analyse, Spawn};
    let mut w = empty();
    for (x0, n) in [(0, "a"), (4, "b")] {
        let mut p = RoomParams::new(
            Footprint::Rect(R::from_corners([x0, 0], [x0 + 3, 2])),
            n,
            3072,
        );
        p.leaves = LeafPolicy::Sectors;
        lay_room(&mut w, &p).unwrap();
    }
    let door = OpeningParams {
        profile: OpeningProfile::Rectangular,
        room: 0,
        cell: [3, 1],
        side: WallEdge::East,
        from: 4,
        to: 8,
        kind: OpeningKind::Door,
        top: 2048,
        material: None,
    };
    let spawn = Spawn {
        room: 0,
        cell: [0, 1],
    };
    let before = analyse(&w, &[spawn], &psx_engine::WalkLimits::DEFAULT).unwrap();
    assert_eq!(before.counts()[4], 12, "room b is walled off");
    cut_opening(&mut w, &door).unwrap();
    // The lintel is a solid wall record, above the body.
    let lintels = w.rooms[0]
        .leaves()
        .flat_map(|(_, s)| s.walls(WallEdge::East).to_vec())
        .filter(|x| x.solid)
        .count();
    assert!(lintels > 0);
    let after = analyse(&w, &[spawn], &psx_engine::WalkLimits::DEFAULT).unwrap();
    assert_eq!(after.counts()[4], 0, "the door is open");
}
