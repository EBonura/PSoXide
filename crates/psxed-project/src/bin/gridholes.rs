//! Census of the walls a grid world is missing.
//!
//! Usage: `gridholes <project dir> [tsv out]`
//!
//! Counts the derived walls that stand against a leaf of another room and
//! that each room taken alone would leave out (see
//! [`psxed_project::grid::leaf_derive::seam_walls`]), and lists them by rule,
//! by height and by pair of rooms. With a second argument the walls are
//! written there, one per line, for aiming cameras at them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use psxed_project::grid::leaf_derive::{open_edges, seam_walls};
use psxed_project::ProjectDocument;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: gridholes <project dir> [seam tsv out] [open edge tsv out]");
        return ExitCode::from(2);
    };
    let doc = match ProjectDocument::load_from_path(std::path::Path::new(dir).join("project.ron")) {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!("cannot load {dir}: {err:?}");
            return ExitCode::FAILURE;
        }
    };
    let Some(world) = doc.active_scene().world.grid() else {
        eprintln!("{dir} has no grid world");
        return ExitCode::FAILURE;
    };
    let walls = seam_walls(world);
    println!(
        "{} rooms, chunked {}, {} walls missing across room borders",
        world.rooms.len(),
        world.is_chunked(),
        walls.len()
    );
    let mut by_kind: BTreeMap<String, (usize, i64)> = BTreeMap::new();
    let mut by_height: BTreeMap<i32, usize> = BTreeMap::new();
    let mut by_room: BTreeMap<usize, usize> = BTreeMap::new();
    let mut tsv = String::new();
    for w in &walls {
        let area = i64::from(w.len) * i64::from(w.y[1] - w.y[0]);
        let entry = by_kind.entry(format!("{:?}", w.wall.kind)).or_default();
        entry.0 += 1;
        entry.1 += area;
        // Height buckets of one sector.
        *by_height.entry((w.y[1] - w.y[0]) / 1024).or_default() += 1;
        *by_room.entry(w.room).or_default() += 1;
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}",
            w.room, w.wall.host[0], w.wall.host[1], w.len, w.wall.edge, w.wall.kind, w.y[0], w.y[1]
        );
    }
    for (kind, (n, area)) in &by_kind {
        println!(
            "  {kind}: {n} walls, {} sector-squares",
            area / (1024 * 1024)
        );
    }
    println!("  height in sectors (floor of): {by_height:?}");
    println!("  {} rooms host at least one of them", by_room.len());
    let open = open_edges(world);
    let mut by_floor: BTreeMap<(String, i32), usize> = BTreeMap::new();
    let mut open_tsv = String::new();
    for e in &open {
        // Height above the base the converter gives the void (-1024).
        *by_floor
            .entry((
                format!("{:?}", e.kind),
                (e.floor[0].max(e.floor[1]) + 1024) / 1024,
            ))
            .or_default() += 1;
        let _ = writeln!(
            open_tsv,
            "{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}",
            e.room, e.host.x, e.host.z, e.edge, e.kind, e.u[0], e.u[1], e.floor[0], e.floor[1]
        );
    }
    println!("{} floor edges end against nothing", open.len());
    for ((kind, sectors), n) in &by_floor {
        println!("  {kind}, floor {sectors} sector(s) above the base: {n}");
    }
    if let Some(out) = args.get(2) {
        if let Err(err) = std::fs::write(out, open_tsv) {
            eprintln!("cannot write {out}: {err}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(out) = args.get(1) {
        if let Err(err) = std::fs::write(out, tsv) {
            eprintln!("cannot write {out}: {err}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
