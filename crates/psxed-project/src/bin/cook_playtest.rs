//! CLI: cook the editor's starter project (or a named project
//! file) into the playtest example's `generated/` directory.
//!
//! The editor's Play action calls the same
//! `psxed_project::playtest::cook_to_dir` underneath before the
//! frontend builds and side-loads the runtime. This bin exists so CI
//! scripts and the Makefile can drive the cook without spinning up the
//! full GUI.
//!
//! Usage:
//!   cook-playtest                     -- cook the embedded starter
//!   cook-playtest <project.ron>       -- cook the named project
//!
//! Exit codes: 0 success, 1 on validation errors, 2 on I/O.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::{
    default_project_dir,
    playtest::{
        build_package, default_generated_dir, playtest_performance_envelope, write_cook_result,
    },
    ProjectDocument,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, project_root) = match args.first() {
        None => (ProjectDocument::starter(), default_project_dir()),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => match ProjectDocument::from_ron_str(&text) {
                Ok(p) => {
                    // Texture `psxt_path`s are stored relative to
                    // the project file, so anchor the project root
                    // at its parent directory.
                    let root = Path::new(path)
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    (p, root)
                }
                Err(e) => {
                    eprintln!("[cook-playtest] {path}: parse failed: {e}");
                    return ExitCode::from(2);
                }
            },
            Err(e) => {
                eprintln!("[cook-playtest] {path}: {e}");
                return ExitCode::from(2);
            }
        },
    };

    let dir = default_generated_dir();
    // Build the package once and write that same package: cooking is the slow
    // part, and the summary below needs the package too.
    let started = std::time::Instant::now();
    let (package, report) = build_package(&project, &project_root);
    eprintln!(
        "[cook-playtest] timing: package {} ms",
        started.elapsed().as_millis()
    );
    match write_cook_result(package.as_ref(), &dir).map(|()| report) {
        Ok(report) => {
            for warn in &report.warnings {
                eprintln!("[cook-playtest] warning: {warn}");
            }
            if !report.is_ok() {
                for err in &report.errors {
                    eprintln!("[cook-playtest] error: {err}");
                }
                return ExitCode::from(1);
            }
            // Surface package counts in the success line: a quick read on
            // what landed in generated/.
            if let Some(package) = package {
                // The exact cooked resource envelope. The editor already shows
                // this after Play; printing it here means a CLI cook, a gate
                // log and a budget review all quote the same numbers instead
                // of three people measuring file sizes by hand.
                println!(
                    "[cook-playtest] Budget: {}",
                    psxed_project::playtest::cooked_playtest_budgets(&project, &package)
                        .concise_summary()
                );
                let world = &package.world_geometry;
                println!(
                    "[cook-playtest] World: {} faces  {} B level core  {} B room chunks",
                    world.faces,
                    world.core.len(),
                    world.rooms.len(),
                );
                println!(
                    "[cook-playtest] {}",
                    world.summary.trim_end().replace('\n', "\n[cook-playtest] ")
                );
                // Per-room residency counts: deduped texture assets (world
                // textures + model atlases) are VRAM-required; model meshes
                // + clips bump RAM.
                let mut total_ram_refs: usize = 0;
                let mut total_vram_refs: usize = 0;
                for (i, r) in package.rooms.iter().enumerate() {
                    let mut ram_seen: Vec<usize> = Vec::new();
                    let mut vram_seen: Vec<usize> = Vec::new();
                    if i == 0 {
                        for &texture in &package.world_geometry.texture_asset_indices {
                            if !vram_seen.contains(&texture) {
                                vram_seen.push(texture);
                            }
                        }
                    }
                    if let Some(sky_texture) = r.sky.texture_asset_index {
                        if !vram_seen.contains(&sky_texture) {
                            vram_seen.push(sky_texture);
                        }
                    }
                    let i_u16 = i as u16;
                    let mut seen_models: Vec<u16> = Vec::new();
                    for inst in &package.model_instances {
                        if inst.room != i_u16 || seen_models.contains(&inst.model) {
                            continue;
                        }
                        seen_models.push(inst.model);
                        let model = &package.models[inst.model as usize];
                        if !ram_seen.contains(&model.mesh_asset_index) {
                            ram_seen.push(model.mesh_asset_index);
                        }
                        if let Some(atlas) = model.texture_asset_index {
                            if !vram_seen.contains(&atlas) {
                                vram_seen.push(atlas);
                            }
                        }
                        let cf = model.clip_first as usize;
                        let cc = model.clip_count as usize;
                        for clip in &package.model_clips[cf..cf + cc] {
                            if !ram_seen.contains(&clip.animation_asset_index) {
                                ram_seen.push(clip.animation_asset_index);
                            }
                        }
                    }
                    for prop in &package.image_props {
                        if prop.room == i_u16 && !vram_seen.contains(&prop.texture_asset_index) {
                            vram_seen.push(prop.texture_asset_index);
                        }
                    }
                    total_ram_refs += ram_seen.len();
                    total_vram_refs += vram_seen.len();
                }
                println!(
                    "[cook-playtest] Rooms: {}  Assets: {}  Textures: {}  Models: {}  Model instances: {}  RAM residency refs: {}  VRAM residency refs: {}  Entities: {}",
                    package.rooms.len(),
                    package.assets.len(),
                    package.texture_asset_count(),
                    package.models.len(),
                    package.model_instances.len(),
                    total_ram_refs,
                    total_vram_refs,
                    package.entities.len(),
                );
                // Session-resident payloads, reported every cook so the trend
                // is visible well before the ceiling. Over it, the cook has
                // already refused and this line never prints.
                let resident = psxed_project::playtest::audit_resident_assets(&package);
                println!(
                    "[cook-playtest] Resident assets: {}B of {}B ({}%) across {} payloads{}",
                    resident.total_bytes,
                    resident.cap_bytes,
                    resident.percent_of_cap(),
                    resident.entries.len(),
                    resident
                        .by_owner()
                        .iter()
                        .map(|(owner, bytes)| format!(" [{owner} {bytes}B]"))
                        .collect::<String>(),
                );
                let envelope = playtest_performance_envelope(&package);
                let packet_capacity =
                    psxed_project::playtest::cooked_manifest_packet_capacity(&package);
                println!(
                    "[cook-playtest] 30 FPS envelope: largest-room-surfaces<={} authored-tris<={} prop-surfaces<={} packet-capacity={}",
                    envelope.room_surfaces,
                    envelope.authored_triangles,
                    envelope.prop_surfaces,
                    packet_capacity,
                );
                if envelope.prop_surfaces > packet_capacity {
                    eprintln!(
                        "[cook-playtest] warning: prop surfaces {} exceed the runtime packet capacity {}; recorded-view validation is required",
                        envelope.prop_surfaces, packet_capacity,
                    );
                }
            }
            println!("[cook-playtest] wrote → {}", dir.display());
            println!("[cook-playtest] Build: make build-editor-playtest");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[cook-playtest] write failed: {e}");
            ExitCode::from(2)
        }
    }
}
