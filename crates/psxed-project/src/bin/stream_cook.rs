//! CLI: cook a project as a streamed PXBSP world (design 2026-10-08, M6).
//!
//! Usage: stream-cook <project.ron> [--out DIR] [--release]
//!
//! A project that fits one region cooks exactly as the whole-map path does
//! and the tool says so. Otherwise it writes `world.pxbsp` (the resident
//! top) and `regions.bin` (sector aligned payloads in disc order) into DIR
//! and prints the per-region census.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::brush_world::stream_cook::{
    cook_project_streamed, partition_params_from_env, CookedWorld,
};
use psxed_project::brush_world::BrushWorldCookMode;
use psxed_project::ProjectDocument;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut source: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut mode = BrushWorldCookMode::Draft;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--release" => mode = BrushWorldCookMode::Release,
            other if !other.starts_with("--") && source.is_none() => source = Some(other.into()),
            other => {
                eprintln!("[stream-cook] unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(path) = source else {
        eprintln!("usage: stream-cook <project.ron> [--out DIR] [--release]");
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
    let params = match partition_params_from_env() {
        Ok(params) => params,
        Err(error) => {
            eprintln!("[stream-cook] PSXED_STREAM_PARAMS: {error}");
            return ExitCode::from(2);
        }
    };
    match cook_project_streamed(&project, &root, mode, [0; 3], &params) {
        Ok(CookedWorld::Whole(world)) => {
            println!(
                "one region: whole-map cook, {} bytes, empty StreamingIndex",
                world.pxbsp.bytes.len()
            );
            ExitCode::SUCCESS
        }
        Ok(CookedWorld::Streamed(world)) => {
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
            for r in &s.regions {
                println!(
                    "  #{}: {} B ({} sec)  faces {} verts {} planes {} marks {} leaves {} nodes {} clip {} vis {} B |V| {}",
                    r.id, r.payload_bytes, r.sectors, r.faces, r.vertices, r.planes, r.marks, r.leaves, r.nodes, r.clip_nodes, r.vis_bytes, r.vis_count
                );
            }
            if let Some(dir) = out {
                if std::fs::create_dir_all(&dir).is_err()
                    || std::fs::write(dir.join("world.pxbsp"), &world.container).is_err()
                    || std::fs::write(dir.join("regions.bin"), &world.region_pack).is_err()
                {
                    eprintln!("[stream-cook] could not write {}", dir.display());
                    return ExitCode::from(2);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[stream-cook] {error}");
            ExitCode::from(1)
        }
    }
}
