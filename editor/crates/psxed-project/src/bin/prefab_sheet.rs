//! Draw every prefab in the library as a top-down plan, one SVG.
//!
//! In-engine screenshots are taken at eye level inside the piece, which shows
//! wall texture and tells you nothing about the layout. This reads the saved
//! `.ron` files, so what it draws is what will actually stamp.
//!
//! Reading of the plan:
//! - grey fill: an authored cell, darker as the floor gets higher
//! - thick dark line: a wall
//! - thick green line: a socket, meaning a perimeter edge with no wall, which
//!   is the only place another piece can mate
//! - `^` / `v`: a floor link up or down

use std::collections::HashSet;

use psxed_project::*;

const CELL: i32 = 44;
const PAD: i32 = 26;
const GAP: i32 = 34;

fn main() {
    let mut paths = list_prefabs().expect("prefab directory readable");
    paths.sort();
    if paths.is_empty() {
        eprintln!("no prefabs in {}", prefabs_dir().display());
        return;
    }

    let mut panels: Vec<(Prefab, i32, i32)> = Vec::new();
    for path in &paths {
        let prefab = Prefab::load_from_path(path).expect("prefab loads");
        // Floors are drawn side by side, so a two-floor piece is twice as wide.
        let w = prefab.floors.len() as i32 * (prefab.width * CELL + GAP) - GAP;
        let h = prefab.height * CELL;
        panels.push((prefab, w, h));
    }
    // Wide enough for the caption line, not just the widest plan: sized off
    // the panels alone, the labels ran off the canvas.
    // Shelf-pack into columns. Sixteen pieces in one column is a 3000px strip
    // nobody can read; the widest piece sets the column width so plans stay at
    // one scale and can be compared by eye.
    let col_w = panels.iter().map(|p| p.1).max().unwrap_or(0).max(300) + GAP;
    let cols = 3.min(panels.len() as i32).max(1);
    let rows = (panels.len() as i32 + cols - 1) / cols;
    let mut col_h = vec![0i32; cols as usize];
    for (i, p) in panels.iter().enumerate() {
        col_h[i / rows as usize] += p.2 + 62;
    }
    let total_w = col_w * cols + PAD * 2;
    let total_h = col_h.iter().copied().max().unwrap_or(0) + PAD * 2 + 20;

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_w}" height="{total_h}" viewBox="0 0 {total_w} {total_h}" font-family="ui-monospace,Menlo,monospace">
<rect width="100%" height="100%" fill="#15151a"/>
"##
    );

    let mut y = PAD;
    let mut col = 0usize;
    let mut in_col = 0i32;
    for (prefab, _, panel_h) in &panels {
        if in_col == rows {
            col += 1;
            in_col = 0;
            y = PAD;
        }
        in_col += 1;
        let col_x = PAD + col as i32 * col_w;
        svg.push_str(&format!(
            r##"<text x="{col_x}" y="{}" fill="#e8e8ee" font-size="15" font-weight="600">{}</text>
<text x="{col_x}" y="{}" fill="#8b8b99" font-size="11">{}x{} cells - {} floor(s) - sector {} - {} cells authored</text>
"##,
            y + 13,
            escape(&prefab.name),
            y + 29,
            prefab.width,
            prefab.height,
            prefab.floors.len(),
            prefab.sector_size,
            prefab.cells().filter(|c| c.sector.is_some()).count(),
        ));
        let top = y + 40;

        for (fi, floor) in prefab.floors.iter().enumerate() {
            let ox = col_x + fi as i32 * (prefab.width * CELL + GAP);
            let present: HashSet<(i32, i32)> = floor
                .cells
                .iter()
                .filter(|c| c.sector.as_ref().is_some_and(GridSector::has_geometry))
                .map(|c| (c.offset[0], c.offset[1]))
                .collect();

            let heights: Vec<i32> = floor
                .cells
                .iter()
                .filter_map(|c| c.sector.as_ref())
                .filter_map(|s| s.floor.as_ref())
                .map(|f| f.heights[0])
                .collect();
            let (lo, hi) = (
                heights.iter().copied().min().unwrap_or(0),
                heights.iter().copied().max().unwrap_or(0),
            );

            for cell in &floor.cells {
                let Some(sector) = cell.sector.as_ref().filter(|s| s.has_geometry()) else {
                    continue;
                };
                let (cx, cz) = (cell.offset[0], cell.offset[1]);
                // +Z is north, so flip for screen space where +Y is down.
                let px = ox + cx * CELL;
                let py = top + (prefab.height - 1 - cz) * CELL;

                let h = sector.floor.as_ref().map_or(lo, |f| f.heights[0]);
                let t = if hi > lo { (h - lo) * 100 / (hi - lo) } else { 0 };
                let shade = 68 - t * 30 / 100;
                // A dropped corner makes the cell a triangle, so draw it as
                // one. Screen north is up, so NW is the top-left corner.
                let dropped = sector.floor.as_ref().and_then(|f| f.dropped_corner);
                let pt = |c: Corner| match c {
                    Corner::NW => (px, py),
                    Corner::NE => (px + CELL, py),
                    Corner::SE => (px + CELL, py + CELL),
                    Corner::SW => (px, py + CELL),
                };
                match dropped {
                    None => svg.push_str(&format!(
                        r##"<rect x="{px}" y="{py}" width="{CELL}" height="{CELL}" fill="hsl(30 8% {shade}%)" stroke="#2a2a33"/>
"##
                    )),
                    Some(c) => {
                        let live: Vec<(i32, i32)> = CORNER_CYCLE
                            .iter()
                            .filter(|k| **k != c)
                            .map(|k| pt(*k))
                            .collect();
                        let pts: Vec<String> =
                            live.iter().map(|(x, y)| format!("{x},{y}")).collect();
                        svg.push_str(&format!(
                            r##"<polygon points="{}" fill="hsl(30 8% {shade}%)" stroke="#2a2a33"/>
"##,
                            pts.join(" ")
                        ));
                        // The hypotenuse is the diagonal wall closing the cut.
                        let (a, b) = adjacent_corners(c);
                        let (ax, ay) = pt(a);
                        let (bx, by) = pt(b);
                        svg.push_str(&format!(
                            r##"<line x1="{ax}" y1="{ay}" x2="{bx}" y2="{by}" stroke="#3b3b46" stroke-width="7" stroke-linecap="round"/>
"##
                        ));
                    }
                }
                if hi > lo {
                    svg.push_str(&format!(
                        r##"<text x="{}" y="{}" fill="#c9c9d4" font-size="9" text-anchor="middle">{h}</text>
"##,
                        px + CELL / 2,
                        py + CELL / 2 + 3
                    ));
                }
                if sector.floor_above.is_some() {
                    svg.push_str(&label(px + CELL / 2, py + 13, "^", "#7fd4a0"));
                }
                if sector.floor_below.is_some() {
                    svg.push_str(&label(px + CELL / 2, py + CELL - 6, "v", "#7fd4a0"));
                }

                for direction in GridDirection::CARDINAL {
                    let neighbour = match direction {
                        GridDirection::North => (cx, cz + 1),
                        GridDirection::South => (cx, cz - 1),
                        GridDirection::East => (cx + 1, cz),
                        GridDirection::West => (cx - 1, cz),
                        _ => continue,
                    };
                    // An edge deleted by a chamfer is neither wall nor socket.
                    if dropped.is_some_and(|c| cut_edges(c).contains(&direction)) {
                        continue;
                    }
                    let interior = present.contains(&neighbour);
                    let walled = !sector.walls.get(direction).is_empty();
                    if interior && !walled {
                        continue;
                    }
                    // A perimeter edge with no wall is a socket, and it is the
                    // only thing another piece can dock against.
                    let (colour, width) = if walled {
                        ("#3b3b46", 7)
                    } else {
                        ("#54d98c", 7)
                    };
                    let (x1, y1, x2, y2) = match direction {
                        GridDirection::North => (px, py, px + CELL, py),
                        GridDirection::South => (px, py + CELL, px + CELL, py + CELL),
                        GridDirection::West => (px, py, px, py + CELL),
                        GridDirection::East => (px + CELL, py, px + CELL, py + CELL),
                        _ => continue,
                    };
                    svg.push_str(&format!(
                        r##"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{colour}" stroke-width="{width}" stroke-linecap="square"/>
"##
                    ));
                }
            }
            if prefab.floors.len() > 1 {
                svg.push_str(&format!(
                    r##"<text x="{}" y="{}" fill="#8b8b99" font-size="10">floor {fi}  +{}</text>
"##,
                    ox,
                    top + prefab.height * CELL + 13,
                    floor.relative_elevation
                ));
            }
        }
        y = top + panel_h + 22;
    }

    svg.push_str(
        r##"<text x="26" y="99%" fill="#8b8b99" font-size="11">green = socket (open perimeter edge)   dark = wall   ^v = floor link   number = floor height</text>
</svg>
"##,
    );

    // Generated artifact, so it goes to the gitignored build directory rather
    // than sitting in the prefab library beside the hand-editable .ron files.
    // An explicit path argument overrides.
    let out = match std::env::args().nth(1) {
        Some(path) => std::path::PathBuf::from(path),
        // Canonicalise first: prefabs_dir() carries `..` components, and
        // lexical parent() on those walks the wrong way (it lands in
        // editor/crates/build, not the repo root).
        None => std::fs::canonicalize(prefabs_dir())
            .ok()
            .and_then(|p| p.parent().and_then(|e| e.parent()).map(|r| r.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("build")
            .join("prefab-sheet.svg"),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("output directory");
    }
    std::fs::write(&out, svg).expect("sheet writes");
    println!("{} prefabs -> {}", panels.len(), out.display());
}

fn label(x: i32, y: i32, text: &str, fill: &str) -> String {
    format!(
        r##"<text x="{x}" y="{y}" fill="{fill}" font-size="13" font-weight="700" text-anchor="middle">{text}</text>
"##
    )
}

const CORNER_CYCLE: [Corner; 4] = [Corner::NW, Corner::NE, Corner::SE, Corner::SW];

/// The two corners either side of `corner` in cycle order. They are the ends of
/// the diagonal left behind when `corner` is dropped.
fn adjacent_corners(corner: Corner) -> (Corner, Corner) {
    let i = CORNER_CYCLE.iter().position(|c| *c == corner).unwrap_or(0);
    (CORNER_CYCLE[(i + 3) % 4], CORNER_CYCLE[(i + 1) % 4])
}

/// The two cardinal edges that meet at `corner` and vanish with it.
fn cut_edges(corner: Corner) -> [GridDirection; 2] {
    match corner {
        Corner::NW => [GridDirection::North, GridDirection::West],
        Corner::NE => [GridDirection::North, GridDirection::East],
        Corner::SE => [GridDirection::South, GridDirection::East],
        Corner::SW => [GridDirection::South, GridDirection::West],
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}
