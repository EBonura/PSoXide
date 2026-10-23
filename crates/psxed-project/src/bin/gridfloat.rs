//! Place the floating solids of a scanned model as slabs in a grid project.
//!
//! ```text
//! gridfloat <project_dir> <fine_hits.npy> <frame.json> [options]
//!   --material NAME        material of every face of every slab
//!   --top NAME --side NAME --under NAME
//!                          material per face (override --material)
//!   --origin X,Z           world sector of the model's corner (default 2,2)
//!   --clearance N          open space that makes a solid float, grid units
//!   --min-thickness N      thinnest solid kept, grid units
//!   --max-thickness N      thickest solid kept (a deck, not a mass), grid units
//!   --block N              quarter sectors per side of a fitting block (2)
//!   --block-cover PERCENT  share of a block's cells that must agree (50)
//!   --min-area N           smallest patch kept, in fitting blocks
//!   --tolerance N          height wobble allowed inside one slab, grid units
//!   --cover PERCENT        share of a cell's rays that must agree
//!   --rechunk SIZE         afterwards re-chunk the world into hexagons of this
//!                          centre-to-corner size in sectors, carrying the slabs
//!   --rim-cell PERCENT --rim-block PERCENT
//!                          cover that suffices next to a kept cell or block (15, 25)
//!   --cap-min-deficit N    height the ground fit must lose for a tower cap, grid units
//!   --cap-collide 0|1     whether tower caps collide (0)
//!   --cap-cover PERCENT --cap-block-cover PERCENT --cap-min-area CELLS --cap-reach SECTORS
//!                          tower caps: cover per cell and block, patch size that stands
//!                          alone, and distance from a bridge that makes any patch structure
//!   --no-collide           slabs draw but do not collide
//!   --caps-tsv PATH        write the cap cells kept and left out
//!   --no-caps              leave the lost tops of towers alone
//!   --dry-run              report without writing the project
//! ```
//!
//! `fine_hits.npy` is the `rows x columns x hits` float32 array of surface
//! heights from rays cast down the model, 16 to a sector; `frame.json` holds
//! the model's `ground` height and `sector` size. A previous run's slabs are
//! replaced.

use std::path::Path;
use std::process::ExitCode;

use psxed_project::grid::edit::chunks::{rechunk_hex, HexLayout};
use psxed_project::grid::float::caps::{cap_cells, CapReport};
use psxed_project::grid::float::columns::{coarsen, plan_cells, Frame, HitField, PlanCell};
use psxed_project::grid::float::cover::ground_tops;
use psxed_project::grid::float::npy::parse_f32;
use psxed_project::grid::float::place::SlabStyle;
use psxed_project::grid::float::{float_slabs, FloatConfig};
use psxed_project::{ProjectDocument, ResourceId};

/// The command line, parsed.
struct Args {
    project: String,
    hits: String,
    frame: String,
    options: Vec<(String, String)>,
    dry_run: bool,
    no_collide: bool,
    no_caps: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut options = Vec::new();
    let mut dry_run = false;
    let mut no_collide = false;
    let mut no_caps = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.strip_prefix("--") {
            Some("dry-run") => dry_run = true,
            Some("no-collide") => no_collide = true,
            Some("no-caps") => no_caps = true,
            Some(name) => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                options.push((name.to_string(), value));
            }
            None => positional.push(arg),
        }
    }
    let [project, hits, frame] = <[String; 3]>::try_from(positional)
        .map_err(|_| "usage: gridfloat <project_dir> <fine_hits.npy> <frame.json> [options]")?;
    Ok(Args {
        project,
        hits,
        frame,
        options,
        dry_run,
        no_collide,
        no_caps,
    })
}

impl Args {
    fn get(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, String> {
        self.get(name)
            .map(|v| {
                v.parse::<T>()
                    .map_err(|_| format!("--{name}: {v:?} is not a number"))
            })
            .transpose()
    }
}

fn config(args: &Args) -> Result<FloatConfig, String> {
    let mut config = FloatConfig::default();
    if let Some(v) = args.number("clearance")? {
        config.clearance = v;
    }
    if let Some(v) = args.number("min-thickness")? {
        config.min_thickness = v;
    }
    if let Some(v) = args.number("max-thickness")? {
        config.max_thickness = v;
    }
    if let Some(v) = args.number("block")? {
        config.block_cells = v;
    }
    if let Some(v) = args.number("block-cover")? {
        config.coarse_cover_percent = v;
    }
    if let Some(v) = args.number("min-area")? {
        config.min_area_cells = v;
    }
    if let Some(v) = args.number("tolerance")? {
        config.tolerance = v;
    }
    if let Some(v) = args.number("cover")? {
        config.min_cover_percent = v;
    }
    for (name, slot) in [
        ("rim-cell", &mut config.rim_cell_percent),
        ("rim-block", &mut config.rim_block_percent),
        ("cap-cover", &mut config.cap_cover_percent),
        ("cap-block-cover", &mut config.cap_block_percent),
    ] {
        if let Some(v) = args.number(name)? {
            *slot = v;
        }
    }
    if let Some(v) = args.number("cap-min-deficit")? {
        config.cap_min_deficit = v;
    }
    if let Some(v) = args.number("cap-min-area")? {
        config.cap_min_area = v;
    }
    if let Some(v) = args.number::<u8>("cap-collide")? {
        config.cap_collide = v != 0;
    }
    if let Some(v) = args.number("cap-reach")? {
        config.cap_reach = v;
    }
    if let Some(origin) = args.get("origin") {
        let (x, z) = origin
            .split_once(',')
            .ok_or("--origin takes X,Z world sectors")?;
        config.origin_sector = [
            x.trim().parse().map_err(|_| "--origin: bad X")?,
            z.trim().parse().map_err(|_| "--origin: bad Z")?,
        ];
    }
    Ok(config)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let config = config(&args)?;
    let frame = Frame::from_json(
        &std::fs::read_to_string(&args.frame).map_err(|e| format!("{}: {e}", args.frame))?,
    )?;
    let npy = parse_f32(&std::fs::read(&args.hits).map_err(|e| format!("{}: {e}", args.hits))?)?;
    let field = HitField::from_npy(npy)?;
    let cells = plan_cells(&field, &frame, &config);
    println!(
        "{} x {} rays, {} floating plan cell(s)",
        field.rows(),
        field.cols(),
        cells.len()
    );

    let dir = Path::new(&args.project);
    let mut project = ProjectDocument::load_from_path(dir.join("project.ron"))
        .map_err(|e| format!("load {}: {e}", args.project))?;
    let material = |option: &str| -> Result<Option<ResourceId>, String> {
        let Some(name) = args.get(option).or_else(|| args.get("material")) else {
            return Ok(None);
        };
        project
            .resources
            .iter()
            .find(|r| r.name == name)
            .map(|r| Some(r.id))
            .ok_or_else(|| format!("no material named {name:?}"))
    };
    let style = SlabStyle {
        materials: [material("top")?, material("side")?, material("under")?],
        collide: !args.no_collide,
    };
    let world = project
        .active_scene_mut()
        .world
        .grid_mut()
        .ok_or("the project has no grid world")?;
    let (caps, cap_report) = if args.no_caps {
        (Vec::new(), CapReport::default())
    } else {
        let tops = ground_tops(world);
        let near: Vec<_> = coarsen(&cells, config.block_cells, &config)
            .into_iter()
            .map(|c| PlanCell {
                x: c.x * config.block_cells,
                z: c.z * config.block_cells,
                ..c
            })
            .collect();
        cap_cells(&field, &frame, &config, &tops, &near)
    };
    let mut report = float_slabs(world, &cells, &caps, &config, &style)?;
    if let Some(path) = args.get("caps-tsv") {
        let mut text = String::new();
        for c in &caps {
            text += &format!("{}\t{}\t{}\t{}\tkept\n", c.x, c.z, c.bottom, c.top);
        }
        for [x, z] in &cap_report.scenery {
            text += &format!("{x}\t{z}\t0\t0\tscenery\n");
        }
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    report.caps = cap_report;
    print!("{}", report.summary());
    if let Some(size) = args.number::<f64>("rechunk")? {
        let moved = rechunk_hex(world, HexLayout { size }).map_err(|e| e.to_string())?;
        println!(
            "re-chunked into {} hexagon(s) of size {size}: {} leaf(s) moved",
            moved.rooms, moved.leaves
        );
    }
    if args.dry_run {
        println!("dry run: the project was not written");
        return Ok(());
    }
    project
        .save_to_path(dir.join("project.ron"))
        .map_err(|e| format!("save {}: {e}", args.project))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("gridfloat: {message}");
            ExitCode::FAILURE
        }
    }
}
