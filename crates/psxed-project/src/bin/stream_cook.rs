//! CLI: cook a project as a streamed PXBSP world (design 2026-10-08, M6/M8).
//!
//! Usage: stream-cook <project.ron> [--out DIR] [--release] [--global-pvs]
//!                    [--params COOK.ron] [--no-gate] [--quiet]
//!
//! A project that fits one region cooks exactly as the whole-map path does
//! and the tool says so. Otherwise it writes `world.pxbsp` (the resident
//! top), `regions.bin` (sector aligned payloads in disc order) and the cook
//! report into DIR, and prints the per-region census.
//!
//! The gates run on what the cook measured, not on the partitioner's
//! estimates: the visibility closure is the portal flow's, the payloads are
//! the encoded ones, and the skeleton is the real container. They use the
//! default design parameters unless `--params` names an override file. The
//! exit code is 1 when a gate fails, unless `--no-gate`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::brush_region::{CookOverrides, PartitionParams, StreamReport};
use psxed_project::brush_world::stream_cook::{cook_project_gated, CookedWorld, StreamPvs};
use psxed_project::brush_world::BrushWorldCookMode;
use psxed_project::ProjectDocument;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut source: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut mode = BrushWorldCookMode::Draft;
    let mut global_pvs = false;
    let mut params_path: Option<PathBuf> = None;
    let mut enforce = true;
    let mut quiet = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--release" => mode = BrushWorldCookMode::Release,
            "--global-pvs" => global_pvs = true,
            "--params" => params_path = args.next().map(PathBuf::from),
            "--no-gate" => enforce = false,
            "--quiet" => quiet = true,
            other if !other.starts_with("--") && source.is_none() => source = Some(other.into()),
            other => {
                eprintln!("[stream-cook] unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(path) = source else {
        eprintln!(
            "usage: stream-cook <project.ron> [--out DIR] [--release] [--global-pvs] [--params COOK.ron] [--no-gate] [--quiet]"
        );
        return ExitCode::from(2);
    };
    let project = match ProjectDocument::load_from_path(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("[stream-cook] {path}: {error:?}");
            return ExitCode::from(2);
        }
    };
    let root = Path::new(&path)
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let mut params = PartitionParams::default();
    if let Some(file) = &params_path {
        match std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|text| CookOverrides::from_ron_str(&text))
        {
            Ok(overrides) => overrides.apply(&mut params),
            Err(error) => {
                eprintln!("[stream-cook] {}: {error}", file.display());
                return ExitCode::from(2);
            }
        }
    }
    let pvs = if global_pvs {
        StreamPvs::Global
    } else {
        StreamPvs::Clustered {
            reach: params.vis_distance,
        }
    };
    let cooked = match cook_project_gated(&project, &root, mode, [0; 3], &params, pvs) {
        Ok(cooked) => cooked,
        Err(error) => {
            eprintln!("[stream-cook] {error}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "[stream-cook] partitioned in {:.1}s, cooked in {:.1}s",
        cooked.partition_seconds, cooked.cook_seconds
    );
    match &cooked.world {
        CookedWorld::Whole(world) => {
            println!(
                "one region: whole-map cook, {} bytes, empty StreamingIndex",
                world.pxbsp.bytes.len()
            );
            ExitCode::SUCCESS
        }
        CookedWorld::Streamed(world) => {
            let s = &world.stats;
            println!(
                "{} regions, container {} B, region pack {} B ({} sectors), top nodes {}, visible leaves {}, portals {}",
                s.regions.len(),
                s.container_bytes,
                s.pack_bytes,
                s.pack_bytes / 2048,
                s.top_nodes,
                s.visible_leaves,
                s.portals
            );
            let c = world.index.caps;
            println!(
                "slot caps: faces {} vertices {} planes {} marks {} nodes {} clip {} leaves {} vis {} B; widest PVS row {} B of 1024, max |V| {}",
                c.faces, c.vertices, c.planes, c.marks, c.nodes, c.clip_nodes, c.leaves, c.vis_bytes, s.widest_row_bytes, s.max_vis_count
            );
            let slot_bytes = c.faces as usize * 10
                + c.vertices as usize * 12
                + c.planes as usize * 12
                + c.marks as usize * 2
                + c.nodes as usize * 16
                + c.clip_nodes as usize * 6
                + c.leaves as usize * 14
                + c.vis_bytes as usize;
            println!(
                "slot {} B x {} regions = pool {} B; payload sum {} B; largest payload {} B",
                slot_bytes,
                s.regions.len(),
                slot_bytes * s.regions.len(),
                s.regions.iter().map(|r| r.payload_bytes).sum::<usize>(),
                s.regions.iter().map(|r| r.payload_bytes).max().unwrap_or(0)
            );
            if !quiet {
                for r in &s.regions {
                    println!(
                        "  #{}: {} B ({} sec)  faces {} verts {} planes {} marks {} leaves {} nodes {} clip {} vis {} B |V| {}",
                        r.id, r.payload_bytes, r.sectors, r.faces, r.vertices, r.planes, r.marks, r.leaves, r.nodes, r.clip_nodes, r.vis_bytes, r.vis_count
                    );
                }
            }
            let measured = cooked
                .measured
                .as_ref()
                .expect("a streamed cook is always re-gated");
            let report = StreamReport::build(&project.name, &cooked.input, measured);
            println!("--- gates on the cook's own numbers ---");
            print!("{}", report.text);
            if let Some(dir) = out {
                if std::fs::create_dir_all(&dir).is_err()
                    || std::fs::write(dir.join("world.pxbsp"), &world.container).is_err()
                    || std::fs::write(dir.join("regions.bin"), &world.region_pack).is_err()
                    || std::fs::write(dir.join("stream_report.txt"), &report.text).is_err()
                    || std::fs::write(dir.join("stream_report.json"), &report.json).is_err()
                {
                    eprintln!("[stream-cook] could not write {}", dir.display());
                    return ExitCode::from(2);
                }
            }
            if enforce && !report.passed {
                eprintln!("[stream-cook] gate FAILED (see the report above)");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
    }
}
