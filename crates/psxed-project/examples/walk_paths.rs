//! Cook once and run independent collision-motor probes against that package.
//! Input: one path per line, whitespace-separated authored x y z triples.
use psxed_project::{brush_walk::walk_player_hull, playtest::build_package, ProjectDocument};
use std::{error::Error, path::Path};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: walk_paths project.ron paths.txt".into());
    }
    let path = Path::new(&args[0]);
    let project = ProjectDocument::from_ron_str(&std::fs::read_to_string(path)?)?;
    let (package, validation) = build_package(&project, path.parent().unwrap_or(Path::new(".")));
    let package = package.ok_or_else(|| format!("cook failed: {:?}", validation.errors))?;
    for (index, line) in std::fs::read_to_string(&args[1])?.lines().enumerate() {
        let coordinates: Vec<i32> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        if coordinates.len() < 6 || coordinates.len() % 3 != 0 {
            return Err(format!("invalid path {index}").into());
        }
        let points: Vec<[i32; 3]> = coordinates
            .chunks_exact(3)
            .map(|p| [p[0], p[1], p[2]])
            .collect();
        let result = walk_player_hull(&package, &points, 188, 1024, 96)?;
        println!(
            "{} {} {} {} {} {}",
            index,
            result.reached_end,
            result.final_position[0],
            result.final_position[1],
            result.final_position[2],
            result.legs_completed
        );
    }
    Ok(())
}
