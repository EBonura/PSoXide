//! Host-side measurement for the compact grid level study
//! (`docs/grid/compact-format.md`).
//!
//! Cooks the reference grid projects through the real Play path
//! (`playtest::build_package`, lights and all), then:
//!
//! * `breakdown`: bytes per section and per visible feature of the current
//!   PGL representation, plus what is redundant in it;
//! * `codec`: a prototype compact encoder and decoder (round trip checked
//!   against the cooked tables) and the sizes it reaches;
//! * `tree`: quadtree and height-field statistics on the same levels.
//!
//! ```text
//! cargo run --release -p psxed-project --example grid_compact -- [breakdown|codec|tree|all] [project...]
//! ```
//!
//! Nothing here is shipped. The numbers it prints are the only claim.

// A measurement tool: index loops over small fixed arrays read better than
// iterator chains here, and the nested tuple types are local.
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

mod baselines;
mod bits;
mod codec;
mod codec_report;
mod fields;
mod load;
mod probe;
mod report;
mod tables;
mod tree;

use load::Loaded;

const REFERENCE: [&str; 6] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map_or("all", String::as_str).to_string();
    let names: Vec<String> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        let mut v: Vec<String> = REFERENCE.iter().map(|s| (*s).to_string()).collect();
        v.push("grid-starter".to_string());
        v
    };
    let mut levels = Vec::new();
    for name in &names {
        match Loaded::load(name) {
            Ok(level) => levels.push(level),
            Err(error) => eprintln!("{name}: {error}"),
        }
    }
    if matches!(mode.as_str(), "breakdown" | "all") {
        report::breakdown(&levels);
    }
    if matches!(mode.as_str(), "probe" | "all") {
        probe::probe(&levels);
    }
    if matches!(mode.as_str(), "rquads" | "fields" | "all") {
        fields::resident_quads(&levels);
    }
    if matches!(mode.as_str(), "portals" | "fields" | "all") {
        fields::portals(&levels);
    }
    if matches!(mode.as_str(), "fields" | "all") {
        fields::fields(&levels, "grid-cathedral");
    }
    if matches!(mode.as_str(), "codec" | "codecv" | "all") {
        codec_report::codec(&levels, mode == "codecv");
    }
    if matches!(mode.as_str(), "baselines" | "all") {
        baselines::run(&levels);
    }
    if matches!(mode.as_str(), "tree" | "all") {
        tree::run(&levels);
        tree::resident(&levels);
    }
    if matches!(mode.as_str(), "units" | "all") {
        codec_report::units(&levels);
    }
    if mode == "walls" {
        for level in &levels {
            println!("== {}", level.name);
            probe::dump_walls(level, 25);
        }
    }
    if matches!(mode.as_str(), "probe2" | "all") {
        probe::probe2(&levels);
    }
}
