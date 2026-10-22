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
//!   --min-area N           smallest patch kept, plan cells of a quarter sector
//!   --tolerance N          height wobble allowed inside one slab, grid units
//!   --cover PERCENT        share of a cell's rays that must agree
//!   --dry-run              report without writing the project
//! ```
//!
//! `fine_hits.npy` is the `rows x columns x hits` float32 array of surface
//! heights from rays cast down the model, 16 to a sector; `frame.json` holds
//! the model's `ground` height and `sector` size. A previous run's slabs are
//! replaced.

use std::path::Path;
use std::process::ExitCode;

use psxed_project::grid::float::columns::{plan_cells, Frame, HitField};
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
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut options = Vec::new();
    let mut dry_run = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.strip_prefix("--") {
            Some("dry-run") => dry_run = true,
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
    if let Some(v) = args.number("min-area")? {
        config.min_area_cells = v;
    }
    if let Some(v) = args.number("tolerance")? {
        config.tolerance = v;
    }
    if let Some(v) = args.number("cover")? {
        config.min_cover_percent = v;
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
    };
    let world = project
        .active_scene_mut()
        .world
        .grid_mut()
        .ok_or("the project has no grid world")?;
    let report = float_slabs(world, &cells, &config, &style)?;
    print!("{}", report.summary());
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
