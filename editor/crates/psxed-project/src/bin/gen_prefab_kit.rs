//! Generate the starter prefab kit, composed by hand rather than at runtime.
//!
//! Shaped after Bloodborne's chalice dungeons, where a layer is two linking
//! corridors plus a main area, a boss room and an optional vertical link, and
//! where variety comes from combination rather than from unique geometry. The
//! room taxonomy below (crossroads, canyon, balcony room, single and double
//! staircase, well, treasure room) follows the Tomb Prospectors' catalogue of
//! the real thing. The grid is less limiting than it looks: `dropped_corner`
//! plus a diagonal wall gives 45-degree faces, so the octagonal and circular
//! chambers of the catalogue are approximable rather than off the table.
//!
//! The socket contract, which is why pieces fit together:
//! - A socket is the absence of a perimeter wall on that edge.
//! - `door` is one cell wide, `gate` is two.
//! - Every piece authors its base floor at y=0, so any piece mates with any
//!   other and the stamp's PgUp/PgDn lift handles the rest.
//! - Every other perimeter edge carries a wall. Two pieces stamped back to back
//!   collide on that edge and the stamp's seam pass drops the incoming side.
//!
//! Socket *direction* is free here in a way it is not in a 3D kit: two sockets
//! meet only when the cells either side of one grid edge both leave it open, so
//! "facing each other" is a consequence of the grid rather than metadata to
//! validate.
//!
//! Diagonals are the third trick. A chamfered cell drops one corner, which
//! forces the surviving split, closes the cut with a diagonal wall, and deletes
//! the two cardinal edges that met there. Chamfering four corners of a square
//! gives an octagon; chamfering a two-cell band gives a rounder one.
//!
//! Interior holes are the other trick worth knowing. A cell simply left out of
//! the set becomes a pillar, because the perimeter rule walls every edge whose
//! neighbour is absent. Pillared halls and balcony rings are therefore holes,
//! not special cases.

use std::collections::HashSet;

use psxed_project::*;

/// Cell size the kit is authored at. Matches the starter project and six of
/// the thirteen projects in the tree; cortex_v1's 1664 is the outlier.
const SECTOR: i32 = 1792;
/// Wall height, and by coincidence the width of a two-cell gate.
const WALL: i32 = SECTOR * 2;

#[derive(Clone, Copy)]
struct Socket(i32, i32, GridDirection);

/// A chamfered cell: the quadrant at `corner` is cut away, leaving a 45-degree
/// face closed by a diagonal wall.
///
/// Dropping a corner forces the surviving diagonal split (drop NE or SW keeps
/// NW-SE; drop NW or SE keeps NE-SW), and the two cardinal edges that met at
/// that corner stop existing, so the perimeter rule must skip them. Diagonals
/// never enter the duplicate-edge map, because they are internal to one cell
/// and cannot collide with a neighbour's claim.
#[derive(Clone, Copy)]
struct Chamfer(i32, i32, Corner);

/// The two cardinal edges that meet at `corner` and vanish with it.
fn cut_edges(corner: Corner) -> [GridDirection; 2] {
    match corner {
        Corner::NW => [GridDirection::North, GridDirection::West],
        Corner::NE => [GridDirection::North, GridDirection::East],
        Corner::SE => [GridDirection::South, GridDirection::East],
        Corner::SW => [GridDirection::South, GridDirection::West],
    }
}

/// Wall direction that closes the cut face.
fn cut_wall(corner: Corner) -> GridDirection {
    match corner.surviving_split() {
        GridSplit::NorthWestSouthEast => GridDirection::NorthWestSouthEast,
        GridSplit::NorthEastSouthWest => GridDirection::NorthEastSouthWest,
    }
}

/// Chamfer the four corner cells of a `w` x `d` block at `(0, 0)`.
fn corners_of(w: i32, d: i32) -> Vec<Chamfer> {
    vec![
        Chamfer(0, 0, Corner::SW),
        Chamfer(w - 1, 0, Corner::SE),
        Chamfer(0, d - 1, Corner::NW),
        Chamfer(w - 1, d - 1, Corner::NE),
    ]
}

fn door(x: i32, z: i32, d: GridDirection) -> Vec<Socket> {
    vec![Socket(x, z, d)]
}

/// Two adjacent cells opening on the same side. Runs along the edge, so it
/// spans X for a north/south gate and Z for an east/west one.
fn gate(x: i32, z: i32, d: GridDirection) -> Vec<Socket> {
    match d {
        GridDirection::North | GridDirection::South => {
            vec![Socket(x, z, d), Socket(x + 1, z, d)]
        }
        _ => vec![Socket(x, z, d), Socket(x, z + 1, d)],
    }
}

fn rect(x0: i32, z0: i32, w: i32, d: i32) -> Vec<(i32, i32)> {
    (x0..x0 + w)
        .flat_map(move |x| (z0..z0 + d).map(move |z| (x, z)))
        .collect()
}

/// `cells` with `holes` removed. Each hole becomes a pillar.
fn less(cells: Vec<(i32, i32)>, holes: &[(i32, i32)]) -> Vec<(i32, i32)> {
    cells.into_iter().filter(|c| !holes.contains(c)).collect()
}

/// Perimeter of a `w` x `d` rectangle: a balcony ring open over its middle.
fn ring(w: i32, d: i32) -> Vec<(i32, i32)> {
    less(
        rect(0, 0, w, d),
        &rect(1, 1, (w - 2).max(0), (d - 2).max(0)),
    )
}

struct Mats {
    floor: Option<ResourceId>,
    wall: Option<ResourceId>,
    ceiling: Option<ResourceId>,
}

/// Build one floor: wall every perimeter edge except the declared sockets.
fn floor_of(
    cells: &[(i32, i32)],
    sockets: &[Socket],
    height: &dyn Fn(i32, i32) -> i32,
    ceiling: bool,
    m: &Mats,
    relative_elevation: i32,
) -> PrefabFloor {
    floor_chamfered(cells, sockets, &[], height, ceiling, m, relative_elevation)
}

#[allow(clippy::too_many_arguments)]
fn floor_chamfered(
    cells: &[(i32, i32)],
    sockets: &[Socket],
    chamfers: &[Chamfer],
    height: &dyn Fn(i32, i32) -> i32,
    ceiling: bool,
    m: &Mats,
    relative_elevation: i32,
) -> PrefabFloor {
    let present: HashSet<(i32, i32)> = cells.iter().copied().collect();
    // ponytail: linear scan. A piece has a handful of sockets, and
    // GridDirection is deliberately not Hash.
    let is_socket =
        |x: i32, z: i32, d: GridDirection| sockets.iter().any(|s| s.0 == x && s.1 == z && s.2 == d);
    let mut out = Vec::new();
    for &(x, z) in cells {
        let y = height(x, z);
        let mut sector = GridSector::with_floor(y, m.floor);
        if ceiling {
            sector.ceiling = Some(GridHorizontalFace::flat(
                relative_elevation + WALL,
                m.ceiling,
            ));
        }
        let chamfer = chamfers.iter().find(|c| c.0 == x && c.1 == z).map(|c| c.2);
        if let Some(corner) = chamfer {
            if let Some(f) = sector.floor.as_mut() {
                f.drop_corner(corner);
            }
            if let Some(c) = sector.ceiling.as_mut() {
                c.drop_corner(corner);
            }
            sector
                .walls
                .get_mut(cut_wall(corner))
                .push(GridVerticalFace::flat(
                    y,
                    relative_elevation + WALL,
                    m.wall,
                ));
        }
        for direction in GridDirection::CARDINAL {
            // An edge that met the dropped corner no longer exists, so it can
            // carry neither a wall nor a socket.
            if chamfer.is_some_and(|c| cut_edges(c).contains(&direction)) {
                continue;
            }
            let neighbour = step(x, z, direction);
            if present.contains(&neighbour) {
                // Two present cells at different floor heights leave a riser
                // between them. Without a wall there it is a hole you can see
                // through, which is what made the stairs look like floating
                // plates. The HIGHER cell authors it, so the shared edge is
                // claimed exactly once and the cooker's duplicate-wall check
                // stays satisfied.
                let neighbour_y = height(neighbour.0, neighbour.1);
                if neighbour_y < y {
                    sector.walls.get_mut(direction).push(GridVerticalFace::flat(
                        neighbour_y,
                        y,
                        m.wall,
                    ));
                }
                continue;
            }
            if is_socket(x, z, direction) {
                continue;
            }
            sector.walls.get_mut(direction).push(GridVerticalFace::flat(
                y,
                relative_elevation + WALL,
                m.wall,
            ));
        }
        out.push(PrefabCell {
            offset: [x, z],
            sector: Some(sector),
        });
    }
    PrefabFloor {
        relative_elevation,
        cells: out,
    }
}

fn step(x: i32, z: i32, d: GridDirection) -> (i32, i32) {
    match d {
        GridDirection::North => (x, z + 1),
        GridDirection::South => (x, z - 1),
        GridDirection::East => (x + 1, z),
        GridDirection::West => (x - 1, z),
        _ => (x, z),
    }
}

/// Link cell `(x, z)` on floor `lower` upward to the same cell on `lower + 1`.
fn stack_link(floors: &mut [PrefabFloor], lower: usize, x: i32, z: i32) {
    let up = (lower + 1) as u16;
    if let Some(cell) = floors[lower].cells.iter_mut().find(|c| c.offset == [x, z]) {
        if let Some(s) = cell.sector.as_mut() {
            s.floor_above = Some(GridFloorLink {
                target_room: Some(NodeId::ROOT),
                target_floor: up,
            });
        }
    }
    if let Some(cell) = floors[lower + 1]
        .cells
        .iter_mut()
        .find(|c| c.offset == [x, z])
    {
        if let Some(s) = cell.sector.as_mut() {
            s.floor_below = Some(GridFloorLink {
                target_room: Some(NodeId::ROOT),
                target_floor: lower as u16,
            });
        }
    }
}

fn main() {
    let project = ProjectDocument::starter();
    let by_name: std::collections::BTreeMap<String, ResourceId> = project
        .material_options()
        .into_iter()
        .map(|(id, name)| (name, id))
        .collect();
    let pick = |name: &str| {
        let id = by_name.get(name).copied();
        assert!(id.is_some(), "starter project has no {name}");
        id
    };
    // Picked by eye off a decoded contact sheet, not by name: three that read
    // apart at PS1 resolution. Mottled round stones underfoot, dark rectangular
    // masonry on the walls, pale brick overhead.
    let m = Mats {
        floor: pick("COBBLES_1A Material"),
        wall: pick("BLOCK_1A Material"),
        ceiling: pick("BRICK_1A Material"),
    };
    let flat = |_: i32, _: i32| 0;
    // Cell, not a plain counter: two emit closures capture it at once.
    let written = std::cell::Cell::new(0usize);
    let written_paths: std::cell::RefCell<Vec<std::path::PathBuf>> = Default::default();
    let mut one = |name: &str, cells: Vec<(i32, i32)>, sockets: Vec<Socket>| {
        emit(
            name,
            vec![floor_of(&cells, &sockets, &flat, true, &m, 0)],
            &project,
            &written,
            &written_paths,
        );
    };

    // ---- Connectors. Transit, and where a portal seam naturally goes. ----
    one(
        "Connector Straight",
        rect(0, 0, 1, 3),
        cat(&[
            door(0, 0, GridDirection::South),
            door(0, 2, GridDirection::North),
        ]),
    );
    one(
        "Connector Long",
        rect(0, 0, 1, 6),
        cat(&[
            door(0, 0, GridDirection::South),
            door(0, 5, GridDirection::North),
        ]),
    );
    one(
        "Connector Corner",
        vec![(0, 0), (0, 1), (1, 1)],
        cat(&[
            door(0, 0, GridDirection::South),
            door(1, 1, GridDirection::East),
        ]),
    );
    // A T and a crossroads: the chalice "crossroads" rooms, which are what turn
    // a corridor chain into a branching layer.
    one(
        "Connector T",
        vec![(1, 0), (1, 1), (0, 1), (2, 1)],
        cat(&[
            door(1, 0, GridDirection::South),
            door(0, 1, GridDirection::West),
            door(2, 1, GridDirection::East),
        ]),
    );
    one(
        "Connector Crossroads",
        vec![(1, 0), (0, 1), (1, 1), (2, 1), (1, 2)],
        cat(&[
            door(1, 0, GridDirection::South),
            door(1, 2, GridDirection::North),
            door(0, 1, GridDirection::West),
            door(2, 1, GridDirection::East),
        ]),
    );

    // ---- Main areas. ----
    // Sockets deliberately off-centre on some pieces: dead-centre everywhere
    // means any two pieces only ever meet in one alignment.
    one(
        "Arena 5x5",
        rect(0, 0, 5, 5),
        cat(&[
            door(2, 4, GridDirection::North),
            door(2, 0, GridDirection::South),
            door(0, 2, GridDirection::West),
            gate(4, 1, GridDirection::East),
        ]),
    );
    one(
        "Hall Pillared 7x7",
        less(rect(0, 0, 7, 7), &[(2, 2), (4, 2), (2, 4), (4, 4)]),
        cat(&[
            gate(2, 6, GridDirection::North),
            gate(2, 0, GridDirection::South),
            door(0, 3, GridDirection::West),
            door(6, 3, GridDirection::East),
        ]),
    );
    // The "canyon": long, and off-centre side doors so it reads directional.
    one(
        "Canyon 3x9",
        rect(0, 0, 3, 9),
        cat(&[
            door(1, 8, GridDirection::North),
            door(1, 0, GridDirection::South),
            door(0, 2, GridDirection::West),
            door(2, 6, GridDirection::East),
        ]),
    );
    one(
        "Crossroads Hall 5x5",
        less(rect(0, 0, 5, 5), &[(1, 1), (3, 1), (1, 3), (3, 3)]),
        cat(&[
            door(2, 4, GridDirection::North),
            door(2, 0, GridDirection::South),
            door(0, 2, GridDirection::West),
            door(4, 2, GridDirection::East),
        ]),
    );
    one(
        "Boss Arena 9x9",
        rect(0, 0, 9, 9),
        cat(&[gate(3, 0, GridDirection::South)]),
    );

    // ---- Diagonals. Chamfered corners give 45-degree faces, which is how
    // the catalogue's octagonal and circular chambers become expressible. ----
    let mut octagon = |name: &str,
                       cells: Vec<(i32, i32)>,
                       sockets: Vec<Socket>,
                       chamfers: Vec<Chamfer>| {
        emit(
            name,
            vec![floor_chamfered(
                &cells, &sockets, &chamfers, &flat, true, &m, 0,
            )],
            &project,
            &written,
            &written_paths,
        );
    };
    octagon(
        "Octagon Chamber 5x5",
        rect(0, 0, 5, 5),
        cat(&[
            door(2, 4, GridDirection::North),
            door(2, 0, GridDirection::South),
            door(0, 2, GridDirection::West),
            door(4, 2, GridDirection::East),
        ]),
        corners_of(5, 5),
    );
    // Rotunda: a two-cell corner cut instead of one, so the wall reads as a
    // shallower curve. The cell inside each cut is removed outright; the two on
    // the diagonal are chamfered.
    octagon(
        "Rotunda 9x9",
        less(
            rect(0, 0, 9, 9),
            &[(0, 0), (8, 0), (0, 8), (8, 8)],
        ),
        cat(&[
            gate(3, 8, GridDirection::North),
            gate(3, 0, GridDirection::South),
            door(0, 4, GridDirection::West),
            door(8, 4, GridDirection::East),
        ]),
        vec![
            Chamfer(1, 0, Corner::SW),
            Chamfer(0, 1, Corner::SW),
            Chamfer(7, 0, Corner::SE),
            Chamfer(8, 1, Corner::SE),
            Chamfer(1, 8, Corner::NW),
            Chamfer(0, 7, Corner::NW),
            Chamfer(7, 8, Corner::NE),
            Chamfer(8, 7, Corner::NE),
        ],
    );
    // A 45-degree passage. Width is measured along the band, so a W-cell band
    // is only W*cos(45) ~= 0.7W cells clear across the diagonal, and the two
    // flank chamfers eat into that again: at W=2 the result is under one cell
    // wide and reads pinched. W=3 gives roughly two cells clear, wider than a
    // straight connector, which is what a diagonal run wants to be.
    const DIAG_W: i32 = 3;
    const DIAG_L: i32 = 4;
    let mut diag_cells = Vec::new();
    let mut diag_chamfers = Vec::new();
    for z in 0..DIAG_L {
        for x in z..z + DIAG_W {
            diag_cells.push((x, z));
            // Chamfer the flanks, but leave both end rows square so the piece
            // still mates with orthogonal connectors.
            if x == z && z > 0 {
                diag_chamfers.push(Chamfer(x, z, Corner::NW));
            }
            if x == z + DIAG_W - 1 && z < DIAG_L - 1 {
                diag_chamfers.push(Chamfer(x, z, Corner::SE));
            }
        }
    }
    octagon(
        "Connector Diagonal",
        diag_cells,
        cat(&[
            gate(0, 0, GridDirection::South),
            gate(DIAG_L, DIAG_L - 1, GridDirection::North),
        ]),
        diag_chamfers,
    );

    // ---- Terminals. ----
    one(
        "Treasure Alcove",
        rect(0, 0, 2, 2),
        cat(&[door(0, 0, GridDirection::South)]),
    );
    one(
        "Lantern Chamber",
        rect(0, 0, 3, 3),
        cat(&[
            door(1, 0, GridDirection::South),
            door(1, 2, GridDirection::North),
        ]),
    );

    // ---- Vertical. ----
    // The engine refuses to step up more than STEP_UP_HEIGHT (640, see
    // character_motor.rs), so WALL/4 = 896 was not merely steep, it was
    // impassable. 512 sits comfortably under the limit and divides a storey
    // into seven risers, which is why the runs below are long.
    const STEP: i32 = 512;
    const RISERS: i32 = WALL / STEP;
    assert_eq!(STEP % HEIGHT_QUANTUM, 0, "steps must land on the quantum");
    assert!(STEP <= 640, "steps must stay under the engine's step-up limit");
    assert_eq!(RISERS * STEP, WALL, "risers must land exactly on the storey");
    let step_h = STEP;

    // Single stair: a straight flight climbing a storey. Six risers on the run
    // plus the final one through the floor link. Long by necessity, so this is
    // the grand staircase and the switchback below is the compact option.
    let climb = move |_: i32, z: i32| z * STEP;
    let run_len = RISERS; // z = 0..RISERS-1, topping out one riser below the storey
    let mut floors = vec![
        floor_of(
            &rect(0, 0, 1, run_len),
            &door(0, 0, GridDirection::South),
            &climb,
            false,
            &m,
            0,
        ),
        floor_of(
            &rect(0, run_len - 1, 1, 2),
            &door(0, run_len, GridDirection::North),
            &flat,
            true,
            &m,
            WALL,
        ),
    ];
    stack_link(&mut floors, 0, 0, run_len - 1);
    emit("Stair Run", floors, &project, &written, &written_paths);

    // Switchback: the same seven risers folded into a 3x5 footprint instead of
    // a 1x8 corridor. Two flights either side of a landing.
    let flight_a: Vec<(i32, i32)> = (0..5).map(|z| (0, z)).collect();
    let landing: Vec<(i32, i32)> = vec![(1, 4), (2, 4)];
    let flight_b: Vec<(i32, i32)> = vec![(2, 3), (2, 2)];
    let mut switch_cells = flight_a.clone();
    switch_cells.extend(landing.iter().copied());
    switch_cells.extend(flight_b.iter().copied());
    let switch_height = move |x: i32, z: i32| {
        if x == 0 {
            // Up the west flank, south to north.
            z * STEP
        } else if z == 4 {
            // The half-landing.
            4 * STEP
        } else {
            // Back down the east flank, north to south, still climbing.
            (4 + (4 - z)) * STEP
        }
    };
    let mut floors = vec![
        floor_chamfered(
            &switch_cells,
            &door(0, 0, GridDirection::South),
            &[],
            &switch_height,
            false,
            &m,
            0,
        ),
        floor_of(
            &rect(2, 1, 1, 2),
            &door(2, 1, GridDirection::South),
            &flat,
            true,
            &m,
            WALL,
        ),
    ];
    stack_link(&mut floors, 0, 2, 2);
    emit("Stair Switchback", floors, &project, &written, &written_paths);

    // Double stair: two flights up the flanks of a hall to a shared gallery.
    // The centre aisle stays at floor level, and the riser walls the flights
    // now author double as the balustrade that was missing before.
    let hall = rect(0, 0, 5, RISERS);
    let mut floors = vec![
        floor_of(
            &hall,
            &cat(&[gate(1, 0, GridDirection::South)]),
            &move |x, z| if x == 0 || x == 4 { z * STEP } else { 0 },
            false,
            &m,
            0,
        ),
        floor_of(
            &rect(0, RISERS - 1, 5, 1),
            &door(2, RISERS - 1, GridDirection::North),
            &flat,
            true,
            &m,
            WALL,
        ),
    ];
    stack_link(&mut floors, 0, 0, RISERS - 1);
    stack_link(&mut floors, 0, 4, RISERS - 1);
    emit("Stair Double", floors, &project, &written, &written_paths);

    // Balcony room: open hall below, a ring gallery above overlooking it. The
    // ring's hole is what makes it a balcony rather than a second storey.
    let mut floors = vec![
        floor_of(
            &rect(0, 0, 7, 7),
            &cat(&[
                gate(2, 0, GridDirection::South),
                door(0, 3, GridDirection::West),
            ]),
            &flat,
            false,
            &m,
            0,
        ),
        floor_of(
            &ring(7, 7),
            &door(3, 6, GridDirection::North),
            &flat,
            true,
            &m,
            WALL,
        ),
    ];
    // No floor link: the gallery is reached by mating a stair piece onto its
    // upper socket. A link here would claim you can walk up a storey in the
    // corner, which is what the generation-time check caught.
    emit("Balcony Hall 7x7", floors, &project, &written, &written_paths);

    // Spiral stair: the ring of a 3x3 block is eight cells, and a storey
    // divides into eight risers of 448, well under the step-up limit. The
    // earlier "well shaft" linked floors with a bare 3584 drop and no way up,
    // which the generation-time check rejected.
    const SPIRAL: i32 = WALL / 8;
    assert_eq!(SPIRAL % HEIGHT_QUANTUM, 0, "spiral risers stay on the quantum");
    assert!(SPIRAL <= 640, "spiral risers stay climbable");
    // Ring order, counter-clockwise from the south-west corner.
    let spiral_order = [
        (0, 0),
        (1, 0),
        (2, 0),
        (2, 1),
        (2, 2),
        (1, 2),
        (0, 2),
        (0, 1),
    ];
    let spiral_height = move |x: i32, z: i32| {
        spiral_order
            .iter()
            .position(|c| *c == (x, z))
            .map(|i| i as i32 * SPIRAL)
            .unwrap_or(0)
    };
    let ring_cells: Vec<(i32, i32)> = spiral_order.to_vec();
    let mut floors = vec![
        floor_of(
            &ring_cells,
            &door(0, 0, GridDirection::South),
            &spiral_height,
            false,
            &m,
            0,
        ),
        floor_of(
            &rect(0, 1, 1, 2),
            &door(0, 2, GridDirection::North),
            &flat,
            true,
            &m,
            WALL,
        ),
    ];
    // Top of the ring is one riser below the storey, so the link is climbable.
    stack_link(&mut floors, 0, 0, 1);
    emit("Spiral Stair 3x3", floors, &project, &written, &written_paths);

    // Renaming a piece orphans its old file, because pieces are written by
    // name. Warn rather than prune: this directory also holds prefabs saved by
    // hand from the editor, and deleting those would be unforgivable.
    let mine: std::collections::HashSet<std::path::PathBuf> =
        written_paths.take().into_iter().collect();
    for path in list_prefabs().unwrap_or_default() {
        if !mine.contains(&path) {
            eprintln!(
                "note: {} was not written by this run -- stale rename, or authored by hand?",
                path.display()
            );
        }
    }
    println!("\n{} prefabs written to {}", written.get(), prefabs_dir().display());
}

fn cat(groups: &[Vec<Socket>]) -> Vec<Socket> {
    groups.iter().flatten().copied().collect()
}

fn emit(
    name: &str,
    floors: Vec<PrefabFloor>,
    project: &ProjectDocument,
    written: &std::cell::Cell<usize>,
    written_paths: &std::cell::RefCell<Vec<std::path::PathBuf>>,
) {
    let w = floors
        .iter()
        .flat_map(|f| f.cells.iter())
        .map(|c| c.offset[0])
        .max()
        .unwrap_or(0)
        + 1;
    let d = floors
        .iter()
        .flat_map(|f| f.cells.iter())
        .map(|c| c.offset[1])
        .max()
        .unwrap_or(0)
        + 1;
    let mut piece = Prefab::capture(name, SECTOR, w, d, false, floors, NodeId::ROOT, 0, project);
    // One neutral light per piece, at the centre of the footprint and half a
    // storey up. Without it a sealed room cooks to a black box, which reads as
    // broken geometry rather than as an unlit room.
    // Nearest authored cell to the footprint centre, not the centre itself: a
    // ring or an L has a hole there, and a light over the hole is outside the
    // cooked room and gets dropped.
    let centre = piece
        .floors
        .first()
        .and_then(|floor| {
            floor
                .cells
                .iter()
                .filter(|c| c.sector.is_some())
                .min_by_key(|c| {
                    let dx = c.offset[0] - w / 2;
                    let dz = c.offset[1] - d / 2;
                    dx * dx + dz * dz
                })
                .map(|c| c.offset)
        })
        .unwrap_or([w / 2, d / 2]);
    piece.lights.push(PrefabLight {
        cell: centre,
        height_sectors: 1.0,
        color: [255, 255, 255],
        intensity: 1.0,
        // Cover the piece with a little spill, in sectors.
        radius: (w.max(d) as f32 / 2.0) + 1.0,
    });
    let path = prefab_path(&piece.name);
    piece.save_to_path(&path).expect("prefab saves");
    written_paths.borrow_mut().push(path.clone());

    let cells = piece.cells().filter(|c| c.sector.is_some()).count();
    let mut walls = 0usize;
    let mut sockets = 0usize;
    for floor in &piece.floors {
        let present: HashSet<(i32, i32)> = floor
            .cells
            .iter()
            .filter(|c| c.sector.is_some())
            .map(|c| (c.offset[0], c.offset[1]))
            .collect();
        for cell in &floor.cells {
            let Some(s) = cell.sector.as_ref() else {
                continue;
            };
            // Diagonals are walls too; counting only cardinals hid every
            // chamfer face and reported an octagon as mostly open.
            for direction in GridDirection::ALL {
                walls += s.walls.get(direction).len();
            }
            let dropped = s.floor.as_ref().and_then(|f| f.dropped_corner);
            for direction in GridDirection::CARDINAL {
                // An edge deleted by a chamfer is not a socket. It is nothing.
                if dropped.is_some_and(|c| cut_edges(c).contains(&direction)) {
                    continue;
                }
                if s.walls.get(direction).is_empty()
                    && !present.contains(&step(cell.offset[0], cell.offset[1], direction))
                {
                    sockets += 1;
                }
            }
        }
    }
    validate(&piece);
    println!(
        "{:<22} {w}x{d}  {} floor(s)  {cells:>3} cells  {walls:>3} walls  {sockets:>2} socket edges",
        piece.name,
        piece.floors.len()
    );
    written.set(written.get() + 1);
}

/// The engine's step-up limit. Anything taller is not a steep step, it is a
/// wall the player cannot pass (`character_motor.rs::STEP_UP_HEIGHT`).
const STEP_UP_HEIGHT: i32 = 640;

/// Two rules that are invisible in a plan and expensive to find in-game, so
/// they are asserted at generation time rather than left to a test that would
/// rot as the kit changes.
///
/// 1. Every height change between adjacent cells must be climbable.
/// 2. Every height change must be covered by a wall. An uncovered riser is a
///    hole you see straight through, which is what made the first stairs look
///    like floating plates.
fn validate(piece: &Prefab) {
    for (fi, floor) in piece.floors.iter().enumerate() {
        for cell in &floor.cells {
            let Some(sector) = cell.sector.as_ref() else {
                continue;
            };
            let Some(here) = sector.floor.as_ref() else {
                continue;
            };
            let (x, z) = (cell.offset[0], cell.offset[1]);
            let y = here.heights[0];

            // Vertical travel through a floor link has to be climbable too.
            // The original stair topped out 896 below its landing.
            if let Some(link) = sector.floor_above {
                if let Some(above) = piece.floors.get(link.target_floor as usize) {
                    if let Some(landing) = above
                        .cells
                        .iter()
                        .find(|c| c.offset == [x, z])
                        .and_then(|c| c.sector.as_ref())
                        .and_then(|s| s.floor.as_ref())
                    {
                        let rise = (above.relative_elevation + landing.heights[0])
                            - (floor.relative_elevation + y);
                        assert!(
                            rise <= STEP_UP_HEIGHT,
                            "{}: floor link at ({x}, {z}) rises {rise}, over the {STEP_UP_HEIGHT} limit",
                            piece.name
                        );
                    }
                }
            }

            for direction in GridDirection::CARDINAL {
                let (nx, nz) = step(x, z, direction);
                let Some(neighbour) = floor
                    .cells
                    .iter()
                    .find(|c| c.offset == [nx, nz])
                    .and_then(|c| c.sector.as_ref())
                    .and_then(|s| s.floor.as_ref())
                else {
                    continue;
                };
                let low = neighbour.heights[0];
                let drop = y - low;
                if drop <= 0 {
                    continue;
                }
                // Deliberately no step-height assert here. A walled riser under
                // STEP_UP_HEIGHT is a climbable step, a taller one is a
                // barrier, and both are legitimate: the flanks of a double
                // staircase drop a full storey into the aisle on purpose. Step
                // height is guaranteed instead by construction, since the runs
                // are authored in STEP units and STEP is asserted under the
                // limit. What is never legitimate is an uncovered riser.
                let covered = sector.walls.get(direction).iter().any(|w| {
                    w.heights.iter().copied().min().unwrap_or(i32::MAX) <= low
                        && w.heights.iter().copied().max().unwrap_or(i32::MIN) >= y
                });
                assert!(
                    covered,
                    "{}: floor {fi} riser at ({x}, {z}) {direction:?} drops {drop} with no wall covering it",
                    piece.name
                );
            }
        }
    }
}
