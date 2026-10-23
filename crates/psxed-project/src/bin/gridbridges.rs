//! Census of the structures a model holds over its ground and which of them the
//! floating-slab conversion kept.
//!
//! Usage: `gridbridges <fine_hits.npy> <frame.json> [options]`
//!
//! ```text
//!   --origin X,Z        world sector of the model's corner (default 2,2)
//!   --clearance N --min-thickness N --max-thickness N --block N
//!   --block-cover PERCENT --cover PERCENT --tolerance N --min-area N
//!                       the converter's own settings (defaults are its defaults)
//!   --min-columns N     smallest structure listed (default 64 rays)
//!   --tsv PATH          one line per structure, for aiming cameras
//! ```
//!
//! Every ray of the model gives its solid intervals; those standing more than
//! the clearance over the ground run are the floating ones, taken here before
//! the converter's thickness limits. Neighbouring rays whose intervals overlap
//! join into one structure. For each structure the report says how much of it
//! the conversion covers and, for what it does not, which rule dropped it.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::process::ExitCode;

use psxed_project::grid::float::columns::{
    coarsen, floating_raw, plan_cells, Frame, HitField, PlanCell, Solid, COLUMNS_PER_SECTOR,
};
use psxed_project::grid::float::npy::parse_f32;
use psxed_project::grid::float::pack::components;
use psxed_project::grid::float::place::{LARGE_NAME, SMALL_NAME};
use psxed_project::grid::float::FloatConfig;
use psxed_project::ProjectDocument;

/// Why a floating solid of one ray is not in a slab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fate {
    Kept,
    /// Thinner than the minimum: a cable, a rail, a fin.
    Thin,
    /// Thicker than the maximum: a tower mass.
    Mass,
    /// Fewer rays of its quarter-sector cell agree than the cover asks.
    CellCover,
    /// Fewer cells of its block agree than the block cover asks.
    BlockCover,
    /// Its patch is under the minimum area.
    SmallPatch,
    /// The pipeline keeps it but no slab of the project covers it (a full room,
    /// a height the slab fitting averaged away).
    Unplaced,
}

impl Fate {
    const ALL: [Fate; 7] = [
        Fate::Kept,
        Fate::Thin,
        Fate::Mass,
        Fate::CellCover,
        Fate::BlockCover,
        Fate::SmallPatch,
        Fate::Unplaced,
    ];
    fn name(self) -> &'static str {
        match self {
            Fate::Kept => "kept",
            Fate::Thin => "thin (cable/rail)",
            Fate::Mass => "mass over max thickness",
            Fate::CellCover => "cell cover",
            Fate::BlockCover => "block cover",
            Fate::SmallPatch => "small patch",
            Fate::Unplaced => "no slab placed",
        }
    }
}

/// One floating solid of one ray.
struct Node {
    row: usize,
    col: usize,
    solid: Solid,
    fate: Fate,
}

/// A slab of the project, in world grid units: x, z centre, bottom, extent.
struct Slab {
    lo: [i32; 3],
    hi: [i32; 3],
}

fn slabs_of(dir: &str) -> Result<Vec<Slab>, String> {
    let doc = ProjectDocument::load_from_path(std::path::Path::new(dir).join("project.ron"))
        .map_err(|e| format!("load {dir}: {e:?}"))?;
    let world = doc
        .active_scene()
        .world
        .grid()
        .ok_or("the project has no grid world")?;
    let mut out = Vec::new();
    for room in &world.rooms {
        for mesh in &room.meshes {
            let Some(asset) = world.mesh_assets.get(usize::from(mesh.asset)) else {
                continue;
            };
            let unit = match asset.name.as_str() {
                SMALL_NAME => 1024,
                LARGE_NAME => 8192,
                _ => continue,
            };
            let size: [i32; 3] = mesh.scale.map(|s| i32::from(s) * unit / 256);
            let [x, y, z] = mesh.position;
            out.push(Slab {
                lo: [x - size[0] / 2, y + room.y_offset, z - size[2] / 2],
                hi: [
                    x + size[0] / 2,
                    y + room.y_offset + size[1],
                    z + size[2] / 2,
                ],
            });
        }
    }
    Ok(out)
}

fn agrees(list: Option<&Vec<(i32, i32)>>, solid: &Solid, tol: i32) -> bool {
    list.is_some_and(|l| {
        l.iter()
            .any(|&(b, t)| (b - solid.bottom).abs() <= tol && (t - solid.top).abs() <= tol)
    })
}

fn cell_map(cells: &[PlanCell]) -> HashMap<(i32, i32), Vec<(i32, i32)>> {
    let mut map: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    for c in cells {
        map.entry((c.x, c.z)).or_default().push((c.bottom, c.top));
    }
    map
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn run() -> Result<(), String> {
    let mut positional = Vec::new();
    let mut opts: HashMap<String, String> = HashMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.strip_prefix("--") {
            Some(name) => {
                let v = args
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                opts.insert(name.to_string(), v);
            }
            None => positional.push(a),
        }
    }
    let [hits_path, frame_path] = <[String; 2]>::try_from(positional)
        .map_err(|_| "usage: gridbridges <fine_hits.npy> <frame.json> [options]")?;
    let num = |name: &str| -> Result<Option<i32>, String> {
        opts.get(name)
            .map(|v| v.parse().map_err(|_| format!("--{name}: not a number")))
            .transpose()
    };
    let mut config = FloatConfig::default();
    if let Some(v) = num("clearance")? {
        config.clearance = v;
    }
    if let Some(v) = num("min-thickness")? {
        config.min_thickness = v;
    }
    if let Some(v) = num("max-thickness")? {
        config.max_thickness = v;
    }
    if let Some(v) = num("block")? {
        config.block_cells = v;
    }
    if let Some(v) = num("block-cover")? {
        config.coarse_cover_percent = v as u32;
    }
    if let Some(v) = num("cover")? {
        config.min_cover_percent = v as u32;
    }
    if let Some(v) = num("tolerance")? {
        config.tolerance = v;
    }
    if let Some(v) = num("min-area")? {
        config.min_area_cells = v as usize;
    }
    if let Some(o) = opts.get("origin") {
        let (x, z) = o.split_once(',').ok_or("--origin takes X,Z")?;
        config.origin_sector = [
            x.trim().parse().map_err(|_| "bad origin x")?,
            z.trim().parse().map_err(|_| "bad origin z")?,
        ];
    }
    let slabs = opts.get("project").map(|dir| slabs_of(dir)).transpose()?;
    if let Some(s) = &slabs {
        println!("{} slab(s) read from the project", s.len());
    }
    let min_columns = num("min-columns")?.unwrap_or(64) as usize;
    let frame = Frame::from_json(
        &std::fs::read_to_string(&frame_path).map_err(|e| format!("{frame_path}: {e}"))?,
    )?;
    let npy = parse_f32(&std::fs::read(&hits_path).map_err(|e| format!("{hits_path}: {e}"))?)?;
    let field = HitField::from_npy(npy)?;
    let (rows, cols) = (field.rows(), field.cols());

    // What the converter keeps, stage by stage.
    let fine = plan_cells(&field, &frame, &config);
    let fine_map = cell_map(&fine);
    let blocks = coarsen(&fine, config.block_cells, &config);
    let (labels, patches) = components(&blocks);
    let kept_blocks: Vec<PlanCell> = blocks
        .iter()
        .zip(&labels)
        .filter(|(_, &l)| patches[l].cells >= config.min_area_cells)
        .map(|(c, _)| *c)
        .collect();
    let block_map = cell_map(&blocks);
    let kept_map = cell_map(&kept_blocks);
    let cells_per_col = (crate::COLS_PER_CELL) as usize;
    let block_cols = cells_per_col * config.block_cells.max(1) as usize;

    // Every floating solid of every ray, with its fate.
    let mut nodes: Vec<Node> = Vec::new();
    let mut first: Vec<usize> = Vec::with_capacity(rows * cols + 1);
    for row in 0..rows {
        for col in 0..cols {
            first.push(nodes.len());
            for solid in floating_raw(field.column(row, col), &frame, &config) {
                let thick = solid.top - solid.bottom;
                let (cx, cz) = ((col / cells_per_col) as i32, (row / cells_per_col) as i32);
                let (bx, bz) = ((col / block_cols) as i32, (row / block_cols) as i32);
                let fate = if thick < config.min_thickness {
                    Fate::Thin
                } else if thick > config.max_thickness {
                    Fate::Mass
                } else if !agrees(fine_map.get(&(cx, cz)), &solid, config.tolerance) {
                    Fate::CellCover
                } else if !agrees(block_map.get(&(bx, bz)), &solid, config.tolerance) {
                    Fate::BlockCover
                } else if !agrees(kept_map.get(&(bx, bz)), &solid, config.tolerance) {
                    Fate::SmallPatch
                } else {
                    Fate::Kept
                };
                let fate = match (fate, &slabs) {
                    (Fate::Kept | Fate::Unplaced, Some(slabs)) => {
                        let x = config.origin_sector[0] * 1024 + col as i32 * 64 + 32;
                        let z = config.origin_sector[1] * 1024 + row as i32 * 64 + 32;
                        let mid = (solid.bottom + solid.top) / 2;
                        let hit = slabs.iter().any(|s| {
                            (s.lo[0]..s.hi[0]).contains(&x)
                                && (s.lo[2]..s.hi[2]).contains(&z)
                                && s.lo[1] - 256 <= mid
                                && mid <= s.hi[1] + 256
                        });
                        if hit {
                            Fate::Kept
                        } else {
                            Fate::Unplaced
                        }
                    }
                    (fate, _) => fate,
                };
                nodes.push(Node {
                    row,
                    col,
                    solid,
                    fate,
                });
            }
        }
    }
    first.push(nodes.len());

    // Structures: rays whose neighbouring solids overlap in height.
    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    for row in 0..rows {
        for col in 0..cols {
            let here = first[row * cols + col]..first[row * cols + col + 1];
            for (dr, dc) in [(0usize, 1usize), (1, 0), (1, 1)] {
                let (r2, c2) = (row + dr, col + dc);
                if r2 >= rows || c2 >= cols {
                    continue;
                }
                let there = first[r2 * cols + c2]..first[r2 * cols + c2 + 1];
                for a in here.clone() {
                    for b in there.clone() {
                        let (sa, sb) = (&nodes[a].solid, &nodes[b].solid);
                        if sa.bottom <= sb.top && sb.bottom <= sa.top {
                            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                            parent[ra.max(rb)] = ra.min(rb);
                        }
                    }
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let per = COLUMNS_PER_SECTOR as f64;
    let mut table: Vec<(usize, String)> = Vec::new();
    let mut tsv = String::new();
    let mut totals: BTreeMap<Fate, usize> = BTreeMap::new();
    let mut listed = 0usize;
    let mut listed_cols = 0usize;
    for members in groups.values() {
        let mut fates: BTreeMap<Fate, usize> = BTreeMap::new();
        let (mut r0, mut r1, mut c0, mut c1) = (usize::MAX, 0, usize::MAX, 0);
        let (mut low, mut high) = (i32::MAX, 0);
        let (mut thick_sum, mut thick_max, mut clr_min) = (0i64, 0, i32::MAX);
        for &i in members {
            let n = &nodes[i];
            *fates.entry(n.fate).or_default() += 1;
            *totals.entry(n.fate).or_default() += 1;
            r0 = r0.min(n.row);
            r1 = r1.max(n.row);
            c0 = c0.min(n.col);
            c1 = c1.max(n.col);
            low = low.min(n.solid.bottom);
            high = high.max(n.solid.top);
            let t = n.solid.top - n.solid.bottom;
            thick_sum += i64::from(t);
            thick_max = thick_max.max(t);
            clr_min = clr_min.min(n.solid.clearance);
        }
        if members.len() < min_columns {
            continue;
        }
        listed += 1;
        listed_cols += members.len();
        let size_x = (c1 - c0 + 1) as f64 / per;
        let size_z = (r1 - r0 + 1) as f64 / per;
        let area = members.len() as f64 / (per * per);
        let kept = fates.get(&Fate::Kept).copied().unwrap_or(0);
        let mut why = String::new();
        for fate in Fate::ALL.iter().skip(1) {
            if let Some(n) = fates.get(fate) {
                let _ = write!(
                    why,
                    " {}:{:.0}%",
                    fate.name(),
                    *n as f64 * 100.0 / members.len() as f64
                );
            }
        }
        let cx = (c0 + c1) as f64 / 2.0 / per + f64::from(config.origin_sector[0]);
        let cz = (r0 + r1) as f64 / 2.0 / per + f64::from(config.origin_sector[1]);
        let line = format!(
            "area {:6.1} s2  kept {:3.0}%  x {:5.1}..{:5.1} z {:5.1}..{:5.1} (extent {:4.1} x {:4.1})  y {:4.1}..{:4.1} s  thick mean {:3.1} max {:3.1}  clearance>= {:3.1}  lost:{}",
            area,
            kept as f64 * 100.0 / members.len() as f64,
            c0 as f64 / per + f64::from(config.origin_sector[0]),
            (c1 + 1) as f64 / per + f64::from(config.origin_sector[0]),
            r0 as f64 / per + f64::from(config.origin_sector[1]),
            (r1 + 1) as f64 / per + f64::from(config.origin_sector[1]),
            size_x,
            size_z,
            f64::from(low) / 1024.0,
            f64::from(high) / 1024.0,
            thick_sum as f64 / members.len() as f64 / 1024.0,
            f64::from(thick_max) / 1024.0,
            f64::from(clr_min) / 1024.0,
            if why.is_empty() { " nothing".to_string() } else { why }
        );
        let _ = writeln!(
            tsv,
            "{}\t{:.2}\t{:.1}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}",
            members.len(),
            area,
            kept as f64 * 100.0 / members.len() as f64,
            cx,
            cz,
            c0 as f64 / per + f64::from(config.origin_sector[0]),
            (c1 + 1) as f64 / per + f64::from(config.origin_sector[0]),
            r0 as f64 / per + f64::from(config.origin_sector[1]),
            (r1 + 1) as f64 / per + f64::from(config.origin_sector[1]),
            f64::from(low) / 1024.0,
            f64::from(high) / 1024.0,
            thick_sum as f64 / members.len() as f64 / 1024.0,
        );
        table.push((members.len(), line));
    }
    table.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    println!(
        "{} x {} rays; {} floating solids in {} structures; {listed} structures of >= {min_columns} rays ({listed_cols} solids)",
        rows,
        cols,
        nodes.len(),
        groups.len()
    );
    println!("fate of every floating solid (one per ray and level):");
    for fate in Fate::ALL {
        println!(
            "  {:28} {}",
            fate.name(),
            totals.get(&fate).copied().unwrap_or(0)
        );
    }
    for (k, (_, line)) in table.iter().enumerate() {
        println!("#{:<3} {line}", k + 1);
    }
    if let Some(path) = opts.get("columns") {
        let mut text = String::new();
        for n in &nodes {
            let _ = writeln!(
                text,
                "{}\t{}\t{}\t{}\t{}",
                n.col, n.row, n.fate as u8, n.solid.bottom, n.solid.top
            );
        }
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    if let Some(path) = opts.get("tsv") {
        std::fs::write(path, tsv).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

/// Rays along one side of a plan cell.
const COLS_PER_CELL: i32 = psxed_project::grid::float::columns::COLUMNS_PER_CELL as i32;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(m) => {
            eprintln!("gridbridges: {m}");
            ExitCode::FAILURE
        }
    }
}
