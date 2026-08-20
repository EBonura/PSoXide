// SPDX-License-Identifier: GPL-2.0-or-later
//! `psxed` -- PSoXide content-pipeline CLI.
//!
//! Cooks source assets into the compact binary formats the PS1
//! runtime consumes via `include_bytes!`. Invoked by hand, by a
//! `build.rs` hook, or from `make assets`.
//!
//! # Subcommands
//!
//! ## `obj` -- Wavefront OBJ → `.psxm`
//!
//! ```bash
//! psxed obj SRC.obj -o DST.psxm [options]
//!
//! Options:
//!   --decimate-grid N   Vertex-cluster to N^3 cells. Omit for no decimation.
//!   --palette NAME      Face-colour palette: warm (default), cool, green.
//!   --no-colors         Skip the face-colour table.
//!   --compute-normals   Add per-vertex normals for lit rendering.
//! ```
//!
//! ## `glb` -- glTF/GLB → `.psxm`
//!
//! ```bash
//! psxed glb SRC.glb -o DST.psxm [options]
//!
//! Options:
//!   --decimate-grid N       Vertex-cluster to N^3 cells. Omit for no decimation.
//!   --palette NAME          Fallback face-colour palette: warm (default), cool, green.
//!   --no-colors             Skip the face-colour table.
//!   --no-normals            Skip computed per-vertex normals.
//!   --no-material-colors    Ignore glTF material base colours and use palette cycling.
//! ```
//!
//! ## `glb-model`/`fbx-model` -- model source → `.psxmdl` + `.psxanim` + `.psxt`
//!
//! ```bash
//! psxed glb-model SRC.glb --out-dir assets/models/name --name name [options]
//! psxed fbx-model SRC.fbx --out-dir assets/models/name --name name [options]
//!
//! Options:
//!   --texture-size WxH    Target texture dimensions (default 128x128).
//!   --texture-depth 4|8|15  Target texture depth (default 4).
//!   --anim-fps N          Fixed animation sample rate (default 15).
//!   --world-height N      Suggested engine/world height (default 1024).
//!   --center-animation-root
//!                         Freeze root-joint translation while sampling clips.
//!   --animation PATH     Add a standalone FBX animation take. Repeatable.
//!   --prune-detached-islands N
//!                         Drop cooked-position detached islands up to N faces (default 4).
//! ```
//!
//! ## `tex` -- PNG/JPG → `.psxt`
//!
//! ```bash
//! psxed tex SRC.{png,jpg,bmp} -o DST.psxt [options]
//!
//! Options:
//!   --size WxH           Target texel dimensions (default 64x64).
//!   --depth 4|8|15       Bits per texel (default 4 = 16-colour CLUT).
//!   --crop X,Y,W,H       Crop window on the source, pre-resize.
//!   --transparent-index-zero
//!                        Reserve palette index 0 for source-alpha transparency.
//!   --resample nearest|triangle|lanczos3  (default lanczos3)
//! ```
//!
//! ## `audio-pack` -- WAV zip manifest → `.psau`
//!
//! ```bash
//! psxed audio-pack assets/audio/freesfx.selection.json \
//!     --zip /path/to/FreeSFX.zip --out-dir build/audio
//! ```
//!
//! ## Future subcommands
//!
//! - `font`  -- TTF or bitmap → psx-font atlas
//! - `scene` -- edit a .pscene JSON and cook it into runtime format

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let result = match args[1].as_str() {
        "glb" | "gltf" => run_glb(&args[2..]),
        "glb-model" | "gltf-model" | "fbx-model" => run_glb_model(&args[2..]),
        "model-cluster" => run_model_cluster(&args[2..]),
        "obj" => run_obj(&args[2..]),
        "tex" => run_tex(&args[2..]),
        "audio-pack" | "audio" => run_audio_pack(&args[2..]),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        cmd => Err(format!("unknown subcommand: {cmd}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("psxed: {msg}");
            ExitCode::from(1)
        }
    }
}

fn run_model_cluster(args: &[String]) -> Result<(), String> {
    let mut input = None;
    let mut output = None;
    let mut cell_size = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i).ok_or("expected path after --output")?,
                ));
            }
            "--cell-size" => {
                i += 1;
                cell_size = Some(
                    args.get(i)
                        .ok_or("expected N after --cell-size")?
                        .parse::<i32>()
                        .map_err(|_| "invalid --cell-size value".to_string())?,
                );
            }
            arg if arg.starts_with('-') => return Err(format!("unknown flag: {arg}\n\n{USAGE}")),
            arg => {
                if input.replace(PathBuf::from(arg)).is_some() {
                    return Err(format!("unexpected positional argument: {arg}"));
                }
            }
        }
        i += 1;
    }
    let input = input.ok_or("missing input .psxmdl path")?;
    let output = output.ok_or("missing --output path")?;
    let cell_size = cell_size.ok_or("missing --cell-size N")?;
    if cell_size < 2 {
        return Err("--cell-size must be at least 2".to_string());
    }
    let bytes = std::fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let clustered = cluster_model_blob(&bytes, cell_size)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&output, &clustered.bytes)
        .map_err(|e| format!("write {}: {e}", output.display()))?;
    eprintln!(
        "[psxed model-cluster] {} -> {} (vertices {} -> {}, faces {} -> {}, cell {})",
        input.display(),
        output.display(),
        clustered.source_vertices,
        clustered.vertices,
        clustered.source_faces,
        clustered.faces,
        cell_size,
    );
    Ok(())
}

struct ClusteredModel {
    bytes: Vec<u8>,
    source_vertices: usize,
    vertices: usize,
    source_faces: usize,
    faces: usize,
}

#[derive(Copy, Clone)]
struct ClusteredPart {
    joint: u16,
    first_vertex: u16,
    vertex_count: u16,
    first_face: u16,
    face_count: u16,
    material: u16,
}

fn cluster_model_blob(bytes: &[u8], cell_size: i32) -> Result<ClusteredModel, String> {
    let model = psx_asset::Model::from_bytes(bytes)
        .map_err(|error| format!("invalid .psxmdl: {error:?}"))?;
    let mut old_to_new = vec![u16::MAX; model.vertex_count() as usize];
    let mut vertices = Vec::with_capacity(model.vertex_count() as usize);
    let mut sums: Vec<[i64; 4]> = Vec::with_capacity(model.vertex_count() as usize);
    let mut parts = Vec::with_capacity(model.part_count() as usize);

    for part_index in 0..model.part_count() {
        let part = model.part(part_index).ok_or("missing model part")?;
        let first_vertex = u16::try_from(vertices.len()).map_err(|_| "too many vertices")?;
        let mut clusters = BTreeMap::new();
        let end = part
            .first_vertex()
            .checked_add(part.vertex_count())
            .ok_or("part vertex range overflow")?;
        for old_index in part.first_vertex()..end {
            let vertex = model.vertex(old_index).ok_or("missing model vertex")?;
            let key = (
                i32::from(vertex.position.x).div_euclid(cell_size),
                i32::from(vertex.position.y).div_euclid(cell_size),
                i32::from(vertex.position.z).div_euclid(cell_size),
                vertex.joint1,
                vertex.blend,
            );
            let new_index = if let Some(index) = clusters.get(&key).copied() {
                index
            } else {
                let index = u16::try_from(vertices.len()).map_err(|_| "too many vertices")?;
                clusters.insert(key, index);
                vertices.push(vertex);
                sums.push([0; 4]);
                index
            };
            old_to_new[old_index as usize] = new_index;
            let sum = &mut sums[new_index as usize];
            sum[0] += i64::from(vertex.position.x);
            sum[1] += i64::from(vertex.position.y);
            sum[2] += i64::from(vertex.position.z);
            sum[3] += 1;
        }
        parts.push(ClusteredPart {
            joint: part.joint_index(),
            first_vertex,
            vertex_count: u16::try_from(vertices.len() - first_vertex as usize)
                .map_err(|_| "part has too many vertices")?,
            first_face: 0,
            face_count: 0,
            material: part.material_index(),
        });
    }
    if old_to_new.contains(&u16::MAX) {
        return Err("model parts do not cover every vertex".to_string());
    }
    for (vertex, sum) in vertices.iter_mut().zip(&sums) {
        let count = sum[3].max(1);
        vertex.position.x = (sum[0] / count) as i16;
        vertex.position.y = (sum[1] / count) as i16;
        vertex.position.z = (sum[2] / count) as i16;
    }

    let mut faces = Vec::with_capacity(model.face_count() as usize);
    for (part_index, out_part) in parts.iter_mut().enumerate() {
        let part = model.part(part_index as u16).ok_or("missing model part")?;
        out_part.first_face = u16::try_from(faces.len()).map_err(|_| "too many faces")?;
        let end = part
            .first_face()
            .checked_add(part.face_count())
            .ok_or("part face range overflow")?;
        for face_index in part.first_face()..end {
            let mut face = model.face(face_index).ok_or("missing model face")?;
            for corner in &mut face.corners {
                corner.vertex_index = old_to_new[corner.vertex_index as usize];
            }
            let indices = [
                face.corners[0].vertex_index,
                face.corners[1].vertex_index,
                face.corners[2].vertex_index,
            ];
            if indices[0] == indices[1] || indices[1] == indices[2] || indices[2] == indices[0] {
                continue;
            }
            faces.push(face);
        }
        out_part.face_count = u16::try_from(faces.len() - out_part.first_face as usize)
            .map_err(|_| "part has too many faces")?;
    }

    let payload_len = psxed_format::model::ModelHeader::SIZE
        + model.joint_count() as usize * psxed_format::model::JointRecord::SIZE
        + model.material_count() as usize * psxed_format::model::MaterialRecord::SIZE
        + parts.len() * psxed_format::model::PartRecord::SIZE
        + vertices.len() * psxed_format::model::VERTEX_RECORD_SIZE
        + faces.len() * psxed_format::model::FACE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    out.extend_from_slice(&psxed_format::model::MAGIC);
    push_u16(&mut out, psxed_format::model::VERSION);
    push_u16(&mut out, model.flags());
    push_u32(&mut out, payload_len as u32);
    push_u16(&mut out, model.joint_count());
    push_u16(&mut out, model.part_count());
    push_u16(&mut out, vertices.len() as u16);
    push_u16(&mut out, faces.len() as u16);
    push_u16(&mut out, model.material_count());
    push_u16(&mut out, model.texture_width());
    push_u16(&mut out, model.texture_height());
    push_u16(&mut out, model.local_to_world_q12());
    for index in 0..model.joint_count() {
        let joint = model.joint(index).ok_or("missing model joint")?;
        push_u16(
            &mut out,
            joint.parent().unwrap_or(psxed_format::model::NO_JOINT),
        );
        push_u16(&mut out, 0);
    }
    for index in 0..model.material_count() {
        let material = model.material(index).ok_or("missing model material")?;
        push_u16(&mut out, material.texture_index());
        push_u16(&mut out, material.flags());
        out.extend_from_slice(&material.base_color());
    }
    for part in &parts {
        push_u16(&mut out, part.joint);
        push_u16(&mut out, part.first_vertex);
        push_u16(&mut out, part.vertex_count);
        push_u16(&mut out, part.first_face);
        push_u16(&mut out, part.face_count);
        push_u16(&mut out, part.material);
        push_u32(&mut out, 0);
    }
    for vertex in &vertices {
        push_i16(&mut out, vertex.position.x);
        push_i16(&mut out, vertex.position.y);
        push_i16(&mut out, vertex.position.z);
        out.push(vertex.joint1);
        out.push(vertex.blend);
    }
    for face in &faces {
        for corner in face.corners {
            push_u16(&mut out, corner.vertex_index);
            out.push(corner.uv.0);
            out.push(corner.uv.1);
        }
    }
    psx_asset::Model::from_bytes(&out)
        .map_err(|error| format!("clustered model failed validation: {error:?}"))?;
    Ok(ClusteredModel {
        source_vertices: model.vertex_count() as usize,
        vertices: vertices.len(),
        source_faces: model.face_count() as usize,
        faces: faces.len(),
        bytes: out,
    })
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

const USAGE: &str = "\
psxed -- PSoXide content pipeline

USAGE:
    psxed <subcommand> [arguments]

SUBCOMMANDS:
    glb     Convert a glTF/.glb scene mesh to .psxm format
    glb-model
            Convert a GLB/glTF/FBX model to .psxmdl/.psxanim/.psxt
    model-cluster
            Conservatively merge nearby same-skin vertices in a .psxmdl
    obj     Convert a Wavefront .obj mesh to .psxm format
    tex     Convert a PNG/JPG/BMP image to .psxt format
    audio-pack
            Cook WAV entries from a source zip manifest to .psau
    help    Show this message

OBJ SUBCOMMAND:
    psxed obj <input.obj> -o <output.psxm>
                          [--decimate-grid N]
                          [--palette warm|cool|green]
                          [--no-colors]
                          [--compute-normals]

GLB SUBCOMMAND:
    psxed glb <input.glb|input.gltf> -o <output.psxm>
                          [--decimate-grid N]
                          [--palette warm|cool|green]
                          [--no-colors]
                          [--no-normals]
                          [--no-material-colors]

GLB-MODEL SUBCOMMAND:
    psxed glb-model <input.glb|input.gltf|input.fbx> --out-dir <directory>
                          [--name asset_name]
                          [--texture-size WxH]     (default 128x128)
                          [--texture-depth 4|8|15] (default 4)
                          [--anim-fps N]           (default 15)
                          [--world-height N]       (default 1024)
                          [--center-animation-root]
                          [--fixed-model-bounds]   (extra clips do not change model quantization bounds)
                          [--animation <anim.fbx>] (repeatable)
                          [--keep-bones]            (disable default finger/end-bone collapse)
                          [--collapse-bones name,name,...]
                          [--prune-detached-islands N] (default 4)

MODEL-CLUSTER SUBCOMMAND:
    psxed model-cluster <input.psxmdl> -o <output.psxmdl> --cell-size N

TEX SUBCOMMAND:
    psxed tex <input.png|.jpg|.bmp> -o <output.psxt>
                          [--size WxH]            (default 64x64)
                          [--depth 4|8|15]        (default 4)
                          [--crop X,Y,W,H]        (overrides centre-square)
                          [--no-crop]             (resize-stretch the full source)
                          [--transparent-index-zero]
                          [--resample nearest|triangle|lanczos3]

    The default crop is centre-square: the largest square that
    fits in the source, positioned at its centre. This avoids
    aspect distortion on arbitrary-aspect photographs. Pass
    --crop X,Y,W,H for manual control, or --no-crop to disable.

AUDIO-PACK SUBCOMMAND:
    psxed audio-pack <manifest.json> --zip <source.zip> --out-dir <directory>
                          [--no-preview]

EXAMPLES:
    psxed glb /path/to/model.glb -o assets/model.psxm \\
        --decimate-grid 6
    psxed glb-model /path/to/character.glb --out-dir assets/models/character \\
        --name character --texture-size 128x128 --texture-depth 8 --anim-fps 15 \\
        --world-height 1024
    psxed obj vendor/teapot.obj -o build/teapot.psxm --palette cool
    psxed tex /path/to/brick.jpg -o assets/brick.psxt \\
        --size 128x128 --depth 4 --resample lanczos3
    psxed audio-pack assets/audio/freesfx.selection.json \\
        --zip /path/to/FreeSFX.zip --out-dir build/audio
";

fn run_glb(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut decimate_grid: Option<u32> = None;
    let mut palette = psxed_gltf::Palette::Warm;
    let mut include_face_colors = true;
    let mut include_normals = true;
    let mut use_material_colors = true;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-o" | "--output" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "expected path after -o".to_string())?,
                ));
            }
            "--decimate-grid" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected N after --decimate-grid".to_string())?;
                decimate_grid = Some(
                    val.parse()
                        .map_err(|_| format!("invalid grid value: {val}"))?,
                );
            }
            "--palette" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected palette name".to_string())?;
                palette = parse_gltf_palette(val)?;
            }
            "--no-colors" => {
                include_face_colors = false;
            }
            "--compute-normals" => {
                include_normals = true;
            }
            "--no-normals" => {
                include_normals = false;
            }
            "--material-colors" => {
                use_material_colors = true;
            }
            "--no-material-colors" => {
                use_material_colors = false;
            }
            a if a.starts_with('-') => {
                return Err(format!("unknown flag: {a}\n\n{USAGE}"));
            }
            _ => {
                if input.is_none() {
                    input = Some(PathBuf::from(a));
                } else {
                    return Err(format!("unexpected positional argument: {a}"));
                }
            }
        }
        i += 1;
    }

    let input = input.ok_or("missing input GLB/glTF path")?;
    let output = output.ok_or("missing -o output path")?;
    let cfg = psxed_gltf::Config {
        decimate_grid,
        palette,
        include_face_colors,
        include_normals,
        use_material_colors,
    };
    let psxm = psxed_gltf::convert_path(&input, &cfg).map_err(|e| format!("convert: {e}"))?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&output, &psxm).map_err(|e| format!("write {}: {e}", output.display()))?;

    eprintln!(
        "[psxed glb] {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        psxm.len()
    );
    Ok(())
}

fn parse_gltf_palette(name: &str) -> Result<psxed_gltf::Palette, String> {
    match name {
        "warm" => Ok(psxed_gltf::Palette::Warm),
        "cool" => Ok(psxed_gltf::Palette::Cool),
        "green" => Ok(psxed_gltf::Palette::Green),
        other => Err(format!("unknown palette: {other}")),
    }
}

fn parse_size(value: &str, flag: &str) -> Result<(u16, u16), String> {
    let (w, h) = value
        .split_once('x')
        .ok_or_else(|| format!("{flag} expects WxH, got: {value}"))?;
    let width = w.parse().map_err(|_| format!("invalid width: {w}"))?;
    let height = h.parse().map_err(|_| format!("invalid height: {h}"))?;
    Ok((width, height))
}

fn parse_depth(value: &str) -> Result<psxed_format::texture::Depth, String> {
    match value {
        "4" => Ok(psxed_format::texture::Depth::Bit4),
        "8" => Ok(psxed_format::texture::Depth::Bit8),
        "15" => Ok(psxed_format::texture::Depth::Bit15),
        other => Err(format!("invalid bit-depth: {other} (expected 4, 8, 15)")),
    }
}

fn default_asset_name(input: &std::path::Path) -> String {
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("model");
    let mut out = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed.to_string()
    }
}

fn run_glb_model(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut texture_width: u16 = 128;
    let mut texture_height: u16 = 128;
    let mut texture_depth = psxed_format::texture::Depth::Bit4;
    let mut animation_fps: u16 = 15;
    let mut world_height: u16 = 1024;
    let mut normalize_root_translation = false;
    let mut force_single_bind = false;
    let mut double_sided = false;
    let mut extra_animations_affect_bounds = true;
    let mut animation_paths: Vec<PathBuf> = Vec::new();
    let mut collapse_bone_patterns = psxed_gltf::default_collapse_bone_patterns();
    let mut prune_detached_face_islands =
        psxed_gltf::RigidModelConfig::default().prune_detached_face_islands;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--out-dir" | "--output-dir" => {
                i += 1;
                out_dir =
                    Some(PathBuf::from(args.get(i).ok_or_else(|| {
                        "expected directory after --out-dir".to_string()
                    })?));
            }
            "--name" => {
                i += 1;
                name = Some(
                    args.get(i)
                        .ok_or_else(|| "expected asset name after --name".to_string())?
                        .to_string(),
                );
            }
            "--texture-size" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected WxH after --texture-size".to_string())?;
                let (w, h) = parse_size(val, "--texture-size")?;
                texture_width = w;
                texture_height = h;
            }
            "--texture-depth" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected bit-depth after --texture-depth".to_string())?;
                texture_depth = parse_depth(val)?;
            }
            "--anim-fps" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected N after --anim-fps".to_string())?;
                animation_fps = val
                    .parse()
                    .map_err(|_| format!("invalid --anim-fps value: {val}"))?;
            }
            "--world-height" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected N after --world-height".to_string())?;
                world_height = val
                    .parse()
                    .map_err(|_| format!("invalid --world-height value: {val}"))?;
            }
            "--center-animation-root" | "--normalize-root-translation" => {
                normalize_root_translation = true;
            }
            "--single-bind" | "--force-single-bind" => {
                force_single_bind = true;
            }
            "--double-sided" => {
                double_sided = true;
            }
            "--fixed-model-bounds" => {
                extra_animations_affect_bounds = false;
            }
            "--animation" | "--anim" => {
                i += 1;
                animation_paths
                    .push(PathBuf::from(args.get(i).ok_or_else(|| {
                        "expected path after --animation".to_string()
                    })?));
            }
            "--keep-bones" => {
                collapse_bone_patterns.clear();
            }
            "--collapse-bones" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    "expected comma-separated names after --collapse-bones".to_string()
                })?;
                collapse_bone_patterns = value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "--prune-detached-islands" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected N after --prune-detached-islands".to_string())?;
                prune_detached_face_islands = val
                    .parse()
                    .map_err(|_| format!("invalid --prune-detached-islands value: {val}"))?;
            }
            "--no-prune-detached-islands" => {
                prune_detached_face_islands = 0;
            }
            a if a.starts_with('-') => {
                return Err(format!("unknown flag: {a}\n\n{USAGE}"));
            }
            _ => {
                if input.is_none() {
                    input = Some(PathBuf::from(a));
                } else {
                    return Err(format!("unexpected positional argument: {a}"));
                }
            }
        }
        i += 1;
    }

    let input = input.ok_or("missing input GLB/glTF/FBX path")?;
    let out_dir = out_dir.ok_or("missing --out-dir directory")?;
    let name = name.unwrap_or_else(|| default_asset_name(&input));
    let cfg = psxed_gltf::RigidModelConfig {
        texture_width,
        texture_height,
        texture_depth,
        animation_fps,
        world_height,
        normalize_root_translation,
        strip_animation_scale: true,
        prune_detached_face_islands,
        extra_animations_affect_bounds,
        force_single_bind,
        double_sided,
        ignore_embedded_animations: false,
        collapse_bone_patterns,
    };
    let package = convert_rigid_model_source(&input, &animation_paths, &cfg)
        .map_err(|e| format!("convert: {e}"))?;

    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let model_path = out_dir.join(format!("{name}.psxmdl"));
    std::fs::write(&model_path, &package.model)
        .map_err(|e| format!("write {}: {e}", model_path.display()))?;

    let mut clip_paths: Vec<(std::path::PathBuf, &psxed_gltf::CookedClip)> =
        Vec::with_capacity(package.clips.len());
    for clip in &package.clips {
        let path = out_dir.join(format!("{name}_{}.psxanim", clip.sanitized_name));
        std::fs::write(&path, &clip.bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        clip_paths.push((path, clip));
    }

    let texture_path = if let Some(texture) = &package.texture {
        let path = out_dir.join(format!(
            "{name}_{}x{}_{}bpp.psxt",
            texture_width, texture_height, texture_depth as u8
        ));
        std::fs::write(&path, texture).map_err(|e| format!("write {}: {e}", path.display()))?;
        Some(path)
    } else {
        None
    };

    eprintln!(
        "[psxed model] {} -> {} ({} src verts, {} cooked verts, {} faces, {} parts, {} joints)",
        input.display(),
        model_path.display(),
        package.report.source_vertices,
        package.report.cooked_vertices,
        package.report.faces,
        package.report.parts,
        package.report.joints,
    );
    eprintln!(
        "[psxed model] precision local_height={} local_to_world_q12={} target_world_height={}",
        package.report.local_height, package.report.local_to_world_q12, world_height
    );
    if clip_paths.is_empty() {
        eprintln!("[psxed model] no animations in source");
    } else {
        eprintln!(
            "[psxed model] {} clips @ {}Hz, {} bytes total",
            clip_paths.len(),
            animation_fps,
            package.report.animation_bytes
        );
        for (path, clip) in &clip_paths {
            let label = clip.source_name.as_deref().unwrap_or(&clip.sanitized_name);
            eprintln!(
                "[psxed model]   {} ({} frames, {} bytes) <- {}",
                path.display(),
                clip.frames,
                clip.bytes.len(),
                label
            );
        }
    }
    if let Some(path) = texture_path {
        eprintln!(
            "[psxed model] texture {} ({}x{} {}bpp, {} bytes)",
            path.display(),
            texture_width,
            texture_height,
            texture_depth as u8,
            package.report.texture_bytes
        );
    }
    Ok(())
}

fn convert_rigid_model_source(
    input: &Path,
    animation_paths: &[PathBuf],
    cfg: &psxed_gltf::RigidModelConfig,
) -> Result<psxed_gltf::RigidModelPackage, psxed_gltf::Error> {
    let is_fbx = input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fbx"));
    if is_fbx && !animation_paths.is_empty() {
        psxed_gltf::convert_fbx_rigid_model_path_with_animation_paths(input, animation_paths, cfg)
    } else if is_fbx {
        psxed_gltf::convert_fbx_rigid_model_path(input, cfg)
    } else if !animation_paths.is_empty() {
        Err(psxed_gltf::Error::FbxImport(
            "extra FBX animation takes currently require an FBX model source".to_string(),
        ))
    } else {
        psxed_gltf::convert_rigid_model_path(input, cfg)
    }
}

fn run_obj(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut decimate_grid: Option<u32> = None;
    let mut palette = psxed_obj::Palette::Warm;
    let mut include_face_colors = true;
    let mut include_normals = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-o" | "--output" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "expected path after -o".to_string())?,
                ));
            }
            "--decimate-grid" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected N after --decimate-grid".to_string())?;
                decimate_grid = Some(
                    val.parse()
                        .map_err(|_| format!("invalid grid value: {val}"))?,
                );
            }
            "--palette" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected palette name".to_string())?;
                palette = match val.as_str() {
                    "warm" => psxed_obj::Palette::Warm,
                    "cool" => psxed_obj::Palette::Cool,
                    "green" => psxed_obj::Palette::Green,
                    other => return Err(format!("unknown palette: {other}")),
                };
            }
            "--no-colors" => {
                include_face_colors = false;
            }
            "--compute-normals" => {
                include_normals = true;
            }
            a if a.starts_with('-') => {
                return Err(format!("unknown flag: {a}\n\n{USAGE}"));
            }
            _ => {
                // First positional = input path.
                if input.is_none() {
                    input = Some(PathBuf::from(a));
                } else {
                    return Err(format!("unexpected positional argument: {a}"));
                }
            }
        }
        i += 1;
    }

    let input = input.ok_or("missing input OBJ path")?;
    let output = output.ok_or("missing -o output path")?;

    let obj_bytes = std::fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let cfg = psxed_obj::Config {
        decimate_grid,
        palette,
        include_face_colors,
        include_normals,
    };
    let psxm = psxed_obj::convert(&obj_bytes, &cfg).map_err(|e| format!("convert: {e}"))?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&output, &psxm).map_err(|e| format!("write {}: {e}", output.display()))?;

    // One-line status so `make assets` logs stay legible.
    eprintln!(
        "[psxed obj] {} → {} ({} bytes)",
        input.display(),
        output.display(),
        psxm.len()
    );
    Ok(())
}

fn run_tex(args: &[String]) -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut width: u16 = 64;
    let mut height: u16 = 64;
    let mut depth = psxed_format::texture::Depth::Bit4;
    let mut crop = psxed_tex::CropMode::CentreSquare;
    let mut resampler = psxed_tex::Resampler::Lanczos3;
    let mut transparent_index_zero = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-o" | "--output" => {
                i += 1;
                output = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "expected path after -o".to_string())?,
                ));
            }
            "--size" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected WxH after --size".to_string())?;
                let (w, h) = val
                    .split_once('x')
                    .ok_or_else(|| format!("--size expects WxH, got: {val}"))?;
                width = w.parse().map_err(|_| format!("invalid width: {w}"))?;
                height = h.parse().map_err(|_| format!("invalid height: {h}"))?;
            }
            "--depth" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected bit-depth".to_string())?;
                depth = match val.as_str() {
                    "4" => psxed_format::texture::Depth::Bit4,
                    "8" => psxed_format::texture::Depth::Bit8,
                    "15" => psxed_format::texture::Depth::Bit15,
                    other => {
                        return Err(format!("invalid --depth: {other} (expected 4, 8, 15)"));
                    }
                };
            }
            "--crop" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected X,Y,W,H after --crop".to_string())?;
                let parts: Vec<&str> = val.split(',').collect();
                if parts.len() != 4 {
                    return Err(format!("--crop expects X,Y,W,H, got: {val}"));
                }
                let mut nums = [0u32; 4];
                for (j, p) in parts.iter().enumerate() {
                    nums[j] = p
                        .parse()
                        .map_err(|_| format!("invalid crop component: {p}"))?;
                }
                crop = psxed_tex::CropMode::Explicit(psxed_tex::CropRect {
                    x: nums[0],
                    y: nums[1],
                    w: nums[2],
                    h: nums[3],
                });
            }
            "--no-crop" => {
                crop = psxed_tex::CropMode::None;
            }
            "--transparent-index-zero" | "--alpha" => {
                transparent_index_zero = true;
            }
            "--resample" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| "expected resampler name".to_string())?;
                resampler = match val.as_str() {
                    "nearest" => psxed_tex::Resampler::Nearest,
                    "triangle" => psxed_tex::Resampler::Triangle,
                    "lanczos3" => psxed_tex::Resampler::Lanczos3,
                    other => {
                        return Err(format!(
                            "invalid --resample: {other} (expected nearest|triangle|lanczos3)"
                        ));
                    }
                };
            }
            a if a.starts_with('-') => {
                return Err(format!("unknown flag: {a}\n\n{USAGE}"));
            }
            _ => {
                if input.is_none() {
                    input = Some(PathBuf::from(a));
                } else {
                    return Err(format!("unexpected positional argument: {a}"));
                }
            }
        }
        i += 1;
    }

    let input = input.ok_or("missing input image path")?;
    let output = output.ok_or("missing -o output path")?;

    let src_bytes = std::fs::read(&input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let cfg = psxed_tex::Config {
        width,
        height,
        depth,
        crop,
        resampler,
        transparent_index_zero,
    };
    let psxt = psxed_tex::convert(&src_bytes, &cfg).map_err(|e| format!("convert: {e}"))?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(&output, &psxt).map_err(|e| format!("write {}: {e}", output.display()))?;

    eprintln!(
        "[psxed tex] {} → {} ({}×{} {}bpp, {} bytes)",
        input.display(),
        output.display(),
        width,
        height,
        depth as u8,
        psxt.len(),
    );
    Ok(())
}

fn run_audio_pack(args: &[String]) -> Result<(), String> {
    let mut manifest: Option<PathBuf> = None;
    let mut archive: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut write_preview_wav = true;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--zip" | "--archive" => {
                i += 1;
                archive = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "expected path after --zip".to_string())?,
                ));
            }
            "--out-dir" | "--output-dir" => {
                i += 1;
                out_dir =
                    Some(PathBuf::from(args.get(i).ok_or_else(|| {
                        "expected directory after --out-dir".to_string()
                    })?));
            }
            "--no-preview" => {
                write_preview_wav = false;
            }
            "-h" | "--help" => {
                return Err(USAGE.to_string());
            }
            a if a.starts_with('-') => {
                return Err(format!("unknown flag: {a}\n\n{USAGE}"));
            }
            _ => {
                if manifest.is_none() {
                    manifest = Some(PathBuf::from(a));
                } else {
                    return Err(format!("unexpected positional argument: {a}"));
                }
            }
        }
        i += 1;
    }

    let manifest = manifest.ok_or("missing manifest.json path")?;
    let archive = archive.ok_or("missing --zip source archive")?;
    let out_dir = out_dir.ok_or("missing --out-dir directory")?;
    let options = psxed_audio::PackOptions { write_preview_wav };
    let report = psxed_audio::import_pack(&manifest, &archive, &out_dir, &options)
        .map_err(|e| format!("audio-pack: {e}"))?;

    eprintln!(
        "[psxed audio] {} sounds -> {} ({} Hz, peak {:.2})",
        report.sounds.len(),
        out_dir.display(),
        report.target_sample_rate_hz,
        report.normalize_peak,
    );
    Ok(())
}
