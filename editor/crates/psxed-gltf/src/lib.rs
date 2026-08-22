// SPDX-License-Identifier: GPL-2.0-or-later
//! glTF/GLB -> PSXM mesh converter.
//!
//! This is a host-side importer only. The editor/content pipeline can
//! afford a modern glTF parser; the PS1 runtime should keep consuming
//! compact cooked `.psxm` blobs through `psx-asset`.
//!
//! Current scope is deliberately conservative:
//! - mesh primitives from the default scene, or all scenes if no default exists
//! - node transforms baked into vertex positions
//! - triangle, triangle-strip, and triangle-fan primitives
//! - material base colours baked into the PSXM face-colour table
//! - optional vertex-cluster decimation and computed normals
//!
//! Textures/UVs are parsed by glTF but not emitted yet because the
//! current PSXM runtime format has no UV/material table. That is the
//! next format bump once we are ready for textured imported models.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use gltf::animation::{Interpolation, Property};
use gltf::image::Source;
use gltf::mesh::Mode;

pub use psxed_obj::Palette;

mod animation;
mod fbx;
mod math;
mod model_blob;
mod source;

use animation::*;
use fbx::*;
use math::*;
use model_blob::*;
use source::*;

const MODEL_LOCAL_COORD_LIMIT: f32 = 30_000.0;
const DEFAULT_MODEL_WORLD_HEIGHT: u16 = 1024;

/// Conversion configuration for glTF/GLB imports.
#[derive(Debug, Clone)]
pub struct Config {
    /// If `Some(n)`, run vertex-cluster decimation into `n x n x n`
    /// cells. Keep this `None` for hand-authored low-poly meshes.
    pub decimate_grid: Option<u32>,
    /// Fallback palette used when material colours are disabled or
    /// unavailable.
    pub palette: Palette,
    /// Include a face-colour table in the cooked PSXM.
    pub include_face_colors: bool,
    /// Compute per-vertex normals for lit engine render passes.
    pub include_normals: bool,
    /// Use glTF material `baseColorFactor` as per-face colours.
    pub use_material_colors: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            decimate_grid: None,
            palette: Palette::Warm,
            include_face_colors: true,
            include_normals: true,
            use_material_colors: true,
        }
    }
}

/// Errors raised while importing a glTF/GLB mesh.
#[derive(Debug)]
pub enum Error {
    /// glTF parser/importer failure.
    Import(gltf::Error),
    /// FBX parser/importer failure.
    FbxImport(String),
    /// A primitive had no POSITION attribute.
    MissingPositions { primitive_index: usize },
    /// A primitive mode cannot be represented as triangles.
    UnsupportedMode {
        /// glTF primitive mode.
        mode: Mode,
    },
    /// Primitive index points past its POSITION accessor.
    BadIndex {
        /// Invalid local index from the primitive.
        index: u32,
        /// Number of vertices in the primitive POSITION stream.
        vertex_count: usize,
    },
    /// No triangles survived import/decimation.
    Empty,
    /// Cooked PSXM encoding failed.
    Cook(psxed_obj::Error),
    /// Texture conversion failed.
    TextureCook(psxed_tex::Error),
    /// The GLB has no skinned mesh suitable for `.psxmdl` cooking.
    MissingSkinnedMesh,
    /// The FBX has no skinned mesh suitable for `.psxmdl` cooking.
    MissingFbxSkinnedMesh,
    /// A skinned primitive is missing data needed by the native model cooker.
    MissingAttribute {
        /// glTF primitive index.
        primitive_index: usize,
        /// Attribute or table name.
        attribute: &'static str,
    },
    /// A count exceeds the cooked format's current index range.
    TooMany {
        /// Kind of item that exceeded the limit.
        kind: &'static str,
        /// Actual count.
        count: usize,
        /// Maximum supported count.
        max: usize,
    },
    /// A skin references data inconsistent with the mesh or animation.
    BadSkin(&'static str),
    /// Base-color texture source is not embedded in the GLB.
    UnsupportedImageSource,
    /// Animation channel input keyframes are missing.
    MissingAnimationInputs {
        /// glTF channel index.
        channel_index: usize,
    },
    /// Animation channel output values are missing.
    MissingAnimationOutputs {
        /// glTF channel index.
        channel_index: usize,
    },
    /// Animation channel interpolation is not supported by the cooker.
    UnsupportedAnimationInterpolation {
        /// glTF channel index.
        channel_index: usize,
        /// Interpolation mode.
        interpolation: Interpolation,
    },
    /// Animation channel property and output accessor type do not match.
    AnimationTypeMismatch {
        /// glTF channel index.
        channel_index: usize,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Import(error) => write!(f, "glTF import failed: {error}"),
            Self::FbxImport(error) => write!(f, "FBX import failed: {error}"),
            Self::MissingPositions { primitive_index } => {
                write!(f, "primitive #{primitive_index} is missing POSITION data")
            }
            Self::UnsupportedMode { mode } => {
                write!(f, "unsupported primitive mode: {mode:?}")
            }
            Self::BadIndex {
                index,
                vertex_count,
            } => write!(
                f,
                "primitive index {index} points past {vertex_count} POSITION vertices"
            ),
            Self::Empty => write!(f, "glTF scene contains no importable triangles"),
            Self::Cook(error) => write!(f, "PSXM encode failed: {error}"),
            Self::TextureCook(error) => write!(f, "PSXT encode failed: {error}"),
            Self::MissingSkinnedMesh => write!(f, "glTF scene contains no skinned mesh"),
            Self::MissingFbxSkinnedMesh => write!(f, "FBX scene contains no skinned mesh"),
            Self::MissingAttribute {
                primitive_index,
                attribute,
            } => write!(
                f,
                "skinned primitive #{primitive_index} is missing {attribute} data"
            ),
            Self::TooMany { kind, count, max } => {
                write!(f, "too many {kind}: {count} exceeds limit {max}")
            }
            Self::BadSkin(reason) => write!(f, "invalid skin: {reason}"),
            Self::UnsupportedImageSource => {
                write!(f, "base-color texture must be embedded in the GLB")
            }
            Self::MissingAnimationInputs { channel_index } => {
                write!(f, "animation channel #{channel_index} is missing input keys")
            }
            Self::MissingAnimationOutputs { channel_index } => {
                write!(f, "animation channel #{channel_index} is missing output values")
            }
            Self::UnsupportedAnimationInterpolation {
                channel_index,
                interpolation,
            } => write!(
                f,
                "animation channel #{channel_index} uses unsupported interpolation {interpolation:?}"
            ),
            Self::AnimationTypeMismatch { channel_index } => {
                write!(f, "animation channel #{channel_index} has mismatched output type")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<gltf::Error> for Error {
    fn from(value: gltf::Error) -> Self {
        Self::Import(value)
    }
}

impl From<psxed_obj::Error> for Error {
    fn from(value: psxed_obj::Error) -> Self {
        Self::Cook(value)
    }
}

impl From<psxed_tex::Error> for Error {
    fn from(value: psxed_tex::Error) -> Self {
        Self::TextureCook(value)
    }
}

#[derive(Default)]
struct CollectedMesh {
    verts: Vec<[f32; 3]>,
    faces: Vec<[usize; 3]>,
    face_colors: Vec<(u8, u8, u8)>,
}

/// Convert a `.gltf` or `.glb` file to cooked PSXM bytes.
pub fn convert_path(path: impl AsRef<Path>, cfg: &Config) -> Result<Vec<u8>, Error> {
    let (document, buffers, _images) = gltf::import(path)?;
    convert_document(&document, &buffers, cfg)
}

/// Convert an in-memory `.glb` or self-contained `.gltf` blob.
pub fn convert_slice(bytes: &[u8], cfg: &Config) -> Result<Vec<u8>, Error> {
    let (document, buffers, _images) = gltf::import_slice(bytes)?;
    convert_document(&document, &buffers, cfg)
}

fn convert_document(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    cfg: &Config,
) -> Result<Vec<u8>, Error> {
    let mut collected = CollectedMesh::default();
    let identity = identity_matrix();

    let visited_scene_mesh = if let Some(scene) = document.default_scene() {
        for node in scene.nodes() {
            visit_node(node, &identity, buffers, &mut collected)?;
        }
        !collected.faces.is_empty()
    } else {
        for scene in document.scenes() {
            for node in scene.nodes() {
                visit_node(node, &identity, buffers, &mut collected)?;
            }
        }
        !collected.faces.is_empty()
    };

    // Some authoring/export paths keep meshes unattached from scenes.
    // Import them at identity so users still get a useful asset.
    if !visited_scene_mesh {
        for mesh in document.meshes() {
            read_mesh(mesh, &identity, buffers, &mut collected)?;
        }
    }

    if collected.faces.is_empty() {
        return Err(Error::Empty);
    }

    let verts = psxed_obj::normalise(&collected.verts);
    let (verts, faces, face_colors) = if let Some(grid) = cfg.decimate_grid {
        psxed_obj::cluster_decimate_with_face_data(
            &verts,
            &collected.faces,
            &collected.face_colors,
            grid,
        )
    } else {
        (verts, collected.faces, collected.face_colors)
    };

    if faces.is_empty() {
        return Err(Error::Empty);
    }

    let normals_vec = if cfg.include_normals {
        Some(psxed_obj::compute_vertex_normals(&verts, &faces))
    } else {
        None
    };
    let normals = normals_vec.as_deref();

    let palette = if cfg.include_face_colors && cfg.use_material_colors {
        Palette::Custom(face_colors)
    } else {
        cfg.palette.clone()
    };
    let cook_cfg = psxed_obj::Config {
        decimate_grid: None,
        palette,
        include_face_colors: cfg.include_face_colors,
        include_normals: cfg.include_normals,
    };
    psxed_obj::encode_psxm(&verts, &faces, normals, &cook_cfg).map_err(Error::Cook)
}

fn visit_node(
    node: gltf::Node<'_>,
    parent: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    out: &mut CollectedMesh,
) -> Result<(), Error> {
    let local = node.transform().matrix();
    let world = mul_matrix(parent, &local);
    if let Some(mesh) = node.mesh() {
        read_mesh(mesh, &world, buffers, out)?;
    }
    for child in node.children() {
        visit_node(child, &world, buffers, out)?;
    }
    Ok(())
}

fn read_mesh(
    mesh: gltf::Mesh<'_>,
    transform: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    out: &mut CollectedMesh,
) -> Result<(), Error> {
    for primitive in mesh.primitives() {
        let primitive_index = primitive.index();
        let material_color = base_color_rgb(&primitive.material());
        let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or(Error::MissingPositions { primitive_index })?
            .map(|p| transform_point(transform, p))
            .collect();
        let vertex_count = positions.len();
        let base = out.verts.len();
        out.verts.extend(positions);

        let local_indices: Vec<u32> = if let Some(indices) = reader.read_indices() {
            indices.into_u32().collect()
        } else {
            (0..vertex_count as u32).collect()
        };
        let local_faces = triangulate_indices(&local_indices, primitive.mode())?;
        for face in local_faces {
            let a = checked_index(face[0], vertex_count)? + base;
            let b = checked_index(face[1], vertex_count)? + base;
            let c = checked_index(face[2], vertex_count)? + base;
            out.faces.push([a, b, c]);
            out.face_colors.push(material_color);
        }
    }
    Ok(())
}

fn triangulate_indices(indices: &[u32], mode: Mode) -> Result<Vec<[u32; 3]>, Error> {
    let mut faces = Vec::new();
    match mode {
        Mode::Triangles => {
            for tri in indices.chunks_exact(3) {
                faces.push([tri[0], tri[1], tri[2]]);
            }
        }
        Mode::TriangleStrip => {
            for i in 0..indices.len().saturating_sub(2) {
                let tri = if i & 1 == 0 {
                    [indices[i], indices[i + 1], indices[i + 2]]
                } else {
                    [indices[i + 1], indices[i], indices[i + 2]]
                };
                if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
                    faces.push(tri);
                }
            }
        }
        Mode::TriangleFan => {
            for i in 1..indices.len().saturating_sub(1) {
                let tri = [indices[0], indices[i], indices[i + 1]];
                if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
                    faces.push(tri);
                }
            }
        }
        other => return Err(Error::UnsupportedMode { mode: other }),
    }
    Ok(faces)
}

fn checked_index(index: u32, vertex_count: usize) -> Result<usize, Error> {
    if index as usize >= vertex_count {
        Err(Error::BadIndex {
            index,
            vertex_count,
        })
    } else {
        Ok(index as usize)
    }
}

fn base_color_rgb(material: &gltf::Material<'_>) -> (u8, u8, u8) {
    let color = material.pbr_metallic_roughness().base_color_factor();
    (
        linear_to_u8(color[0]),
        linear_to_u8(color[1]),
        linear_to_u8(color[2]),
    )
}

fn linear_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn identity_matrix() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

// glTF matrices are column-major. Keep the same representation here:
// m[column][row].
fn mul_matrix(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] =
                a[0][r] * b[c][0] + a[1][r] * b[c][1] + a[2][r] * b[c][2] + a[3][r] * b[c][3];
        }
    }
    out
}

fn transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Conversion configuration for GLB -> native textured/skinned assets.
#[derive(Debug, Clone)]
pub struct RigidModelConfig {
    /// Target texture width in texels.
    pub texture_width: u16,
    /// Target texture height in texels.
    pub texture_height: u16,
    /// Target PSX texture colour depth.
    pub texture_depth: psxed_format::texture::Depth,
    /// Fixed animation sample rate in Hz.
    pub animation_fps: u16,
    /// Suggested world-space height for this model in engine units.
    ///
    /// The cooker still extracts maximum model-local precision
    /// automatically; this value only determines the recommended
    /// local-to-world scale stored in the `.psxmdl` header.
    pub world_height: u16,
    /// Restore root-joint translations to their bind-pose value
    /// while sampling clips. Meshy-style exports often bake noisy
    /// hip/root location keys into otherwise in-place clips; this
    /// option removes that root-motion bias at import time.
    pub normalize_root_translation: bool,
    /// Remove animated scale from cooked joint pose matrices.
    ///
    /// The runtime format treats sampled pose matrices as rigid
    /// transforms. Baking glTF scale keys into those matrices makes
    /// the whole model visibly grow or shrink when switching clips, so
    /// imported animations strip basis scale by default.
    pub strip_animation_scale: bool,
    /// Drop detached mesh islands with at most this many faces after
    /// cooked-position welding. Zero (the default) disables pruning.
    ///
    /// Hand-authored low-poly models legitimately contain small separate
    /// pieces (armor plates, detail bits), so pruning is OFF by default
    /// and only useful for Meshy-style exports that carry one- or
    /// two-triangle auto-generated scraps. This pass runs after the model
    /// precision bounds are known so it uses the same quantised positions
    /// as the final `.psxmdl`.
    pub prune_detached_face_islands: u16,
    /// Collapse every vertex to a pure single-bone bind before cooking,
    /// dropping all secondary skin influences. The result has no
    /// `BLEND_SKIN` vertices and stays on the GTE single-bone fast path
    /// -- the rigid representation preferred for PS1 characters.
    pub force_single_bind: bool,
    /// Cook the model double-sided: set the `DOUBLE_SIDED` header flag so
    /// the runtime renders both face windings (no backface culling).
    /// Use for hollow / open-faced models that would otherwise show
    /// through. Costs roughly 2x submitted faces, so keep it off by
    /// default and enable per model.
    pub double_sided: bool,
    /// Ignore animation takes embedded in the model FBX itself.
    ///
    /// When set, the model's own `anim_stacks` are excluded from both the
    /// precision-bounds computation and clip cooking. Vertex positions
    /// then quantize to the BIND extent instead of the (much larger)
    /// animated extent, which otherwise collapses small bind features
    /// (e.g. a head squashed flat) when the FBX ships with wild test
    /// takes baked in. Clips are expected to come from separate pack
    /// files. Keep off for legacy single-file model+animation imports.
    pub ignore_embedded_animations: bool,
    /// Include standalone animation sources when choosing model/pose
    /// precision bounds. Full bundle imports should keep this enabled
    /// so every generated clip matches the generated model. Add-on
    /// clip bakes should disable it so the new `.psxanim` remains
    /// compatible with the already-cooked target model.
    pub extra_animations_affect_bounds: bool,
    /// Case-insensitive bone-name fragments whose matching subtrees are
    /// collapsed into their nearest retained ancestor before cooking.
    ///
    /// This removes detail joints that are too expensive for the PS1 target
    /// while preserving their vertices through weight reassignment. An empty
    /// list keeps every source bone.
    pub collapse_bone_patterns: Vec<String>,
}

/// Default detail-bone patterns removed from imported character rigs.
pub fn default_collapse_bone_patterns() -> Vec<String> {
    [
        "handindex",
        "handthumb",
        "handmiddle",
        "handring",
        "handpinky",
        "headtop_end",
        "toe_end",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

impl Default for RigidModelConfig {
    fn default() -> Self {
        Self {
            texture_width: 128,
            texture_height: 128,
            texture_depth: psxed_format::texture::Depth::Bit4,
            animation_fps: 15,
            world_height: DEFAULT_MODEL_WORLD_HEIGHT,
            normalize_root_translation: false,
            strip_animation_scale: true,
            prune_detached_face_islands: 0,
            extra_animations_affect_bounds: true,
            force_single_bind: false,
            double_sided: false,
            ignore_embedded_animations: false,
            collapse_bone_patterns: default_collapse_bone_patterns(),
        }
    }
}

/// Output package from the native model cooker.
#[derive(Debug, Clone)]
pub struct RigidModelPackage {
    /// Cooked `.psxmdl` bytes.
    pub model: Vec<u8>,
    /// Cooked `.psxanim` bytes per source animation clip.
    ///
    /// When the source has no supported animations, the importer emits
    /// a generated one-frame `bind_pose` clip so placed models remain
    /// compatible with the runtime model path.
    pub clips: Vec<CookedClip>,
    /// Cooked `.psxt` base-colour texture. Sources with no texture
    /// receive a solid atlas from the first material base colour.
    pub texture: Option<Vec<u8>>,
    /// Source bone names in cooked-joint order (post-collapse). The
    /// cooked `.psxmdl` stores only indices; these survive into the
    /// editor's SkeletonResource so joint pickers can say
    /// "9 · RightHand" instead of "Joint 9". Unnamed source nodes
    /// contribute empty strings.
    pub joint_names: Vec<String>,
    /// Counts and byte sizes useful for build logs and tests.
    pub report: RigidModelReport,
}

/// One cooked animation clip ready to write to disk.
#[derive(Debug, Clone)]
pub struct CookedClip {
    /// Original glTF clip name, if the source provided one.
    pub source_name: Option<String>,
    /// Filesystem-safe name suitable as a filename suffix.
    pub sanitized_name: String,
    /// Cooked `.psxanim` bytes for this clip.
    pub bytes: Vec<u8>,
    /// Number of sampled frames in the cooked clip.
    pub frames: usize,
}

/// Summary of a native model import.
#[derive(Debug, Clone)]
pub struct RigidModelReport {
    /// Number of source vertices before rigid part duplication.
    pub source_vertices: usize,
    /// Number of cooked vertices after per-joint duplication.
    pub cooked_vertices: usize,
    /// Number of cooked triangles.
    pub faces: usize,
    /// Number of rigid parts.
    pub parts: usize,
    /// Number of skin joints.
    pub joints: usize,
    /// Per-clip frame count, one entry per cooked clip.
    pub clip_frames: Vec<(String, usize)>,
    /// Cooked animated model height in model-local units.
    pub local_height: usize,
    /// Suggested model-local to world-space scale, Q12.
    pub local_to_world_q12: u16,
    /// Cooked model byte length.
    pub model_bytes: usize,
    /// Total cooked animation byte length across all clips.
    pub animation_bytes: usize,
    /// Cooked texture byte length, or zero when no texture exists.
    pub texture_bytes: usize,
}

/// Convert a `.glb` or `.gltf` file into native model/animation/texture blobs.
pub fn convert_rigid_model_path(
    path: impl AsRef<Path>,
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let (document, buffers, _images) = gltf::import(path)?;
    convert_rigid_model_document(&document, &buffers, cfg)
}

/// Convert a `.glb`, `.gltf`, or `.fbx` model plus standalone FBX
/// animation takes into native model/animation/texture blobs.
pub fn convert_rigid_model_path_with_animation_paths(
    path: impl AsRef<Path>,
    animation_paths: &[PathBuf],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let path = path.as_ref();
    let (fbx_animation_paths, gltf_animation_paths): (Vec<_>, Vec<_>) = animation_paths
        .iter()
        .cloned()
        .partition(|path| is_fbx_path(path));
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fbx"))
    {
        if !gltf_animation_paths.is_empty() {
            return Err(Error::BadSkin(
                "FBX model sources cannot use glTF/GLB extra animation sources yet",
            ));
        }
        return convert_fbx_rigid_model_path_with_animation_paths(path, &fbx_animation_paths, cfg);
    }

    let (document, buffers, _images) = gltf::import(path)?;
    let fbx_animation_scenes = load_extra_fbx_animation_scenes(&fbx_animation_paths)?;
    let fbx_sources = fbx_extra_animation_sources(&fbx_animation_scenes);
    let gltf_animation_scenes = load_extra_gltf_animation_scenes(&gltf_animation_paths)?;
    let gltf_sources = gltf_extra_animation_sources(&gltf_animation_scenes);
    convert_rigid_model_document_with_extra_animations(
        &document,
        &buffers,
        &fbx_sources,
        &gltf_sources,
        cfg,
    )
}

/// Convert a binary/ascii FBX file into native model/animation/texture blobs.
pub fn convert_fbx_rigid_model_path(
    path: impl AsRef<Path>,
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let path = path.as_ref();
    let scene = load_fbx_scene(path)?;
    convert_fbx_rigid_model_scene(&scene, Some(path), cfg)
}

/// Convert an FBX model plus standalone FBX animation takes into native blobs.
pub fn convert_fbx_rigid_model_path_with_animation_paths(
    path: impl AsRef<Path>,
    animation_paths: &[PathBuf],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let path = path.as_ref();
    let scene = load_fbx_scene(path)?;
    let animation_scenes = load_extra_fbx_animation_scenes(animation_paths)?;
    let extra_sources = fbx_extra_animation_sources(&animation_scenes);
    convert_fbx_rigid_model_scene_with_extra_animations(&scene, Some(path), &extra_sources, cfg)
}

fn load_extra_fbx_animation_scenes(
    animation_paths: &[PathBuf],
) -> Result<Vec<(ufbx::SceneRoot, Option<String>)>, Error> {
    let mut animation_scenes = Vec::with_capacity(animation_paths.len());
    for path in animation_paths {
        animation_scenes.push((
            load_fbx_scene(path)?,
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string),
        ));
    }
    Ok(animation_scenes)
}

fn fbx_extra_animation_sources(
    animation_scenes: &[(ufbx::SceneRoot, Option<String>)],
) -> Vec<FbxExtraAnimationScene<'_>> {
    animation_scenes
        .iter()
        .map(|(scene, fallback_name)| FbxExtraAnimationScene {
            scene,
            fallback_name: fallback_name.as_deref(),
        })
        .collect()
}

fn load_extra_gltf_animation_scenes(
    animation_paths: &[PathBuf],
) -> Result<Vec<LoadedGltfExtraAnimationScene>, Error> {
    let mut animation_scenes = Vec::with_capacity(animation_paths.len());
    for path in animation_paths {
        let (document, buffers, _images) = gltf::import(path)?;
        animation_scenes.push(LoadedGltfExtraAnimationScene {
            document,
            buffers,
            fallback_name: path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string),
        });
    }
    Ok(animation_scenes)
}

fn gltf_extra_animation_sources(
    animation_scenes: &[LoadedGltfExtraAnimationScene],
) -> Vec<GltfExtraAnimationScene<'_>> {
    animation_scenes
        .iter()
        .map(|scene| GltfExtraAnimationScene {
            document: &scene.document,
            buffers: &scene.buffers,
            fallback_name: scene.fallback_name.as_deref(),
        })
        .collect()
}

fn is_fbx_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fbx"))
}

fn load_fbx_scene(path: &Path) -> Result<ufbx::SceneRoot, Error> {
    let filename = path.to_string_lossy();
    ufbx::load_file(
        &filename,
        ufbx::LoadOpts {
            target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
            target_unit_meters: 1.0,
            clean_skin_weights: true,
            generate_missing_normals: true,
            load_external_files: true,
            ignore_missing_external_files: true,
            filename: ufbx::StringOpt::Ref(&filename),
            ..Default::default()
        },
    )
    .map_err(|error| Error::FbxImport(format!("{error:?}")))
}

/// Convert an in-memory `.glb` into native model/animation/texture blobs.
pub fn convert_rigid_model_slice(
    bytes: &[u8],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let (document, buffers, _images) = gltf::import_slice(bytes)?;
    convert_rigid_model_document(&document, &buffers, cfg)
}

fn convert_rigid_model_document(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    convert_rigid_model_document_with_extra_animations(document, buffers, &[], &[], cfg)
}

fn convert_rigid_model_document_with_extra_animations(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    if cfg.animation_fps == 0 {
        return Err(Error::BadSkin("animation sample rate must be non-zero"));
    }

    let Some(mesh_node) = document
        .nodes()
        .find(|node| node.mesh().is_some() && node.skin().is_some())
    else {
        if !extra_fbx_animation_scenes.is_empty() || !extra_gltf_animation_scenes.is_empty() {
            return Err(Error::BadSkin(
                "extra animation sources require a skinned source model",
            ));
        }
        return convert_static_rigid_model_document(document, buffers, cfg);
    };
    let mesh = mesh_node.mesh().ok_or(Error::MissingSkinnedMesh)?;
    let skin = mesh_node.skin().ok_or(Error::MissingSkinnedMesh)?;
    let mut joints: Vec<usize> = skin.joints().map(|joint| joint.index()).collect();
    if joints.is_empty() {
        return Err(Error::BadSkin("skin has no joints"));
    }
    ensure_u16("joints", joints.len())?;

    let parents = build_parent_indices(document);
    let base_trs = collect_base_trs(document);
    let mut inverse_bind_matrices = read_inverse_bind_matrices(&skin, buffers, joints.len());
    if inverse_bind_matrices.len() != joints.len() {
        return Err(Error::BadSkin(
            "inverse bind matrix count does not match joint count",
        ));
    }

    let mut source = read_skinned_mesh(&mesh, buffers, joints.len())?;
    if source.faces.is_empty() {
        return Err(Error::Empty);
    }
    let node_names: Vec<String> = document
        .nodes()
        .map(|node| node.name().unwrap_or("").to_string())
        .collect();
    collapse_bone_subtrees(
        &mut source,
        &mut joints,
        &mut inverse_bind_matrices,
        &parents,
        &node_names,
        &cfg.collapse_bone_patterns,
    )?;
    let root_joint_nodes = root_joint_nodes(&joints, &parents);
    if cfg.force_single_bind {
        collapse_to_single_bind(&mut source);
    }
    assign_face_joints(&mut source, joints.len());
    let bounds_extra_fbx_animation_scenes = if cfg.extra_animations_affect_bounds {
        extra_fbx_animation_scenes
    } else {
        &[]
    };
    let bounds_extra_gltf_animation_scenes = if cfg.extra_animations_affect_bounds {
        extra_gltf_animation_scenes
    } else {
        &[]
    };
    let precision_bounds = collect_precision_bounds(
        document,
        buffers,
        &source,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        bounds_extra_fbx_animation_scenes,
        bounds_extra_gltf_animation_scenes,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
    )?;
    let bounds = ModelBounds::from_min_max(
        precision_bounds.min,
        precision_bounds.max,
        MODEL_LOCAL_COORD_LIMIT,
    )?;
    if prune_detached_face_islands(
        &mut source,
        &bounds,
        cfg.prune_detached_face_islands as usize,
    ) > 0
    {
        if source.faces.is_empty() {
            return Err(Error::Empty);
        }
        let precision_bounds = collect_precision_bounds(
            document,
            buffers,
            &source,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            bounds_extra_fbx_animation_scenes,
            bounds_extra_gltf_animation_scenes,
            cfg.animation_fps,
            cfg.normalize_root_translation,
            cfg.strip_animation_scale,
        )?;
        let bounds = ModelBounds::from_min_max(
            precision_bounds.min,
            precision_bounds.max,
            MODEL_LOCAL_COORD_LIMIT,
        )?;
        return finish_rigid_model_document(
            document,
            buffers,
            cfg,
            source,
            bounds,
            precision_bounds,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            extra_fbx_animation_scenes,
            extra_gltf_animation_scenes,
            mesh,
        );
    }

    finish_rigid_model_document(
        document,
        buffers,
        cfg,
        source,
        bounds,
        precision_bounds,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        extra_fbx_animation_scenes,
        extra_gltf_animation_scenes,
        mesh,
    )
}

struct StaticGltfModelSource<'a> {
    source: SkinnedSourceMesh,
    mesh: gltf::Mesh<'a>,
}

fn convert_static_rigid_model_document(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let StaticGltfModelSource { mut source, mesh } =
        collect_static_gltf_model_source(document, buffers)?;
    let precision_bounds = collect_static_precision_bounds(&source)?;
    let bounds = ModelBounds::from_min_max(
        precision_bounds.min,
        precision_bounds.max,
        MODEL_LOCAL_COORD_LIMIT,
    )?;
    if prune_detached_face_islands(
        &mut source,
        &bounds,
        cfg.prune_detached_face_islands as usize,
    ) > 0
    {
        if source.faces.is_empty() {
            return Err(Error::Empty);
        }
        let precision_bounds = collect_static_precision_bounds(&source)?;
        let bounds = ModelBounds::from_min_max(
            precision_bounds.min,
            precision_bounds.max,
            MODEL_LOCAL_COORD_LIMIT,
        )?;
        return finish_static_rigid_model_document(
            buffers,
            cfg,
            source,
            bounds,
            precision_bounds,
            mesh,
        );
    }

    finish_static_rigid_model_document(buffers, cfg, source, bounds, precision_bounds, mesh)
}

fn collect_static_gltf_model_source<'a>(
    document: &'a gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Result<StaticGltfModelSource<'a>, Error> {
    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    let mut first_mesh = None;
    let identity = identity_matrix();

    let visited_scene_mesh = if let Some(scene) = document.default_scene() {
        for node in scene.nodes() {
            visit_static_gltf_node(
                node,
                &identity,
                buffers,
                &mut source,
                &mut normal_faces,
                &mut first_mesh,
            )?;
        }
        !source.faces.is_empty()
    } else {
        for scene in document.scenes() {
            for node in scene.nodes() {
                visit_static_gltf_node(
                    node,
                    &identity,
                    buffers,
                    &mut source,
                    &mut normal_faces,
                    &mut first_mesh,
                )?;
            }
        }
        !source.faces.is_empty()
    };

    if !visited_scene_mesh {
        for mesh in document.meshes() {
            remember_static_gltf_mesh(&mut first_mesh, &mesh);
            read_static_gltf_mesh(&mesh, &identity, buffers, &mut source, &mut normal_faces)?;
        }
    }

    if source.faces.is_empty() {
        return Err(Error::Empty);
    }
    rebuild_source_normals(&mut source, &normal_faces);
    let mesh = first_mesh.ok_or(Error::Empty)?;
    Ok(StaticGltfModelSource { source, mesh })
}

fn visit_static_gltf_node<'a>(
    node: gltf::Node<'a>,
    parent: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    source: &mut SkinnedSourceMesh,
    normal_faces: &mut Vec<[usize; 3]>,
    first_mesh: &mut Option<gltf::Mesh<'a>>,
) -> Result<(), Error> {
    let local = node.transform().matrix();
    let world = mul_matrix(parent, &local);
    if let Some(mesh) = node.mesh() {
        remember_static_gltf_mesh(first_mesh, &mesh);
        read_static_gltf_mesh(&mesh, &world, buffers, source, normal_faces)?;
    }
    for child in node.children() {
        visit_static_gltf_node(child, &world, buffers, source, normal_faces, first_mesh)?;
    }
    Ok(())
}

fn remember_static_gltf_mesh<'a>(first_mesh: &mut Option<gltf::Mesh<'a>>, mesh: &gltf::Mesh<'a>) {
    if first_mesh.is_none() {
        *first_mesh = Some(mesh.clone());
    }
}

fn finish_static_rigid_model_document(
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
    source: SkinnedSourceMesh,
    bounds: ModelBounds,
    precision_bounds: PrecisionBounds,
    mesh: gltf::Mesh<'_>,
) -> Result<RigidModelPackage, Error> {
    let local_height = bounds.encoded_axis_size(precision_bounds.min[1], precision_bounds.max[1]);
    let local_to_world_q12 = choose_local_to_world_q12(local_height, cfg.world_height);
    let material_color = first_material_base_color(&mesh);
    let texture = Some(match cook_base_color_texture(&mesh, buffers, cfg)? {
        Some(texture) => texture,
        None => cook_solid_base_color_texture(material_color, cfg)?,
    });
    let parents = [None];
    let joints = [0usize];
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        &parents,
        &joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
        cfg.double_sided,
    )?;
    let clips = vec![bind_pose_clip(joints.len(), &bounds, cfg.animation_fps)?];
    let animation_bytes = clips.iter().map(|c| c.bytes.len()).sum();
    let clip_frames = clips
        .iter()
        .map(|c| (c.sanitized_name.clone(), c.frames))
        .collect();
    let report = RigidModelReport {
        source_vertices: source.vertices.len(),
        cooked_vertices,
        faces: source.faces.len(),
        parts,
        joints: joints.len(),
        clip_frames,
        local_height: local_height.max(0) as usize,
        local_to_world_q12,
        model_bytes: model.len(),
        animation_bytes,
        texture_bytes: texture.as_ref().map_or(0, Vec::len),
    };

    Ok(RigidModelPackage {
        model,
        clips,
        texture,
        joint_names: vec!["root".to_string()],
        report,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish_rigid_model_document(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
    source: SkinnedSourceMesh,
    bounds: ModelBounds,
    precision_bounds: PrecisionBounds,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    mesh: gltf::Mesh<'_>,
) -> Result<RigidModelPackage, Error> {
    let local_height = bounds.encoded_axis_size(precision_bounds.min[1], precision_bounds.max[1]);
    let local_to_world_q12 = choose_local_to_world_q12(local_height, cfg.world_height);

    let material_color = first_material_base_color(&mesh);
    let texture = Some(match cook_base_color_texture(&mesh, buffers, cfg)? {
        Some(texture) => texture,
        None => cook_solid_base_color_texture(material_color, cfg)?,
    });
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        parents,
        joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
        cfg.double_sided,
    )?;
    let mut clips = cook_all_animations(
        document,
        buffers,
        parents,
        base_trs,
        root_joint_nodes,
        joints,
        inverse_bind_matrices,
        &bounds,
        extra_fbx_animation_scenes,
        extra_gltf_animation_scenes,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
    )?;
    if clips.is_empty() {
        clips.push(bind_pose_clip(joints.len(), &bounds, cfg.animation_fps)?);
    }

    let animation_bytes = clips.iter().map(|c| c.bytes.len()).sum();
    let clip_frames = clips
        .iter()
        .map(|c| (c.sanitized_name.clone(), c.frames))
        .collect();
    let report = RigidModelReport {
        source_vertices: source.vertices.len(),
        cooked_vertices,
        faces: source.faces.len(),
        parts,
        joints: joints.len(),
        clip_frames,
        local_height: local_height.max(0) as usize,
        local_to_world_q12,
        model_bytes: model.len(),
        animation_bytes,
        texture_bytes: texture.as_ref().map_or(0, Vec::len),
    };

    let gltf_node_names: Vec<Option<String>> = document
        .nodes()
        .map(|node| node.name().map(str::to_string))
        .collect();
    let joint_names = joints
        .iter()
        .map(|&node| {
            gltf_node_names
                .get(node)
                .cloned()
                .flatten()
                .unwrap_or_default()
        })
        .collect();
    Ok(RigidModelPackage {
        model,
        clips,
        texture,
        joint_names,
        report,
    })
}

#[derive(Clone, Copy)]
struct FbxExtraAnimationScene<'a> {
    scene: &'a ufbx::Scene,
    fallback_name: Option<&'a str>,
}

struct LoadedGltfExtraAnimationScene {
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    fallback_name: Option<String>,
}

#[derive(Clone, Copy)]
struct GltfExtraAnimationScene<'a> {
    document: &'a gltf::Document,
    buffers: &'a [gltf::buffer::Data],
    fallback_name: Option<&'a str>,
}

#[derive(Clone, Copy, Debug)]
struct Trs {
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

impl Trs {
    fn matrix(&self) -> [[f32; 4]; 4] {
        compose_trs(self.translation, self.rotation, self.scale)
    }
}

#[derive(Clone, Copy, Debug)]
struct SourceVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    joints: [u16; 4],
    weights: [f32; 4],
    /// Bone this vertex follows under rigid skinning. Picked as the
    /// joint with the highest weight in `weights`. Pre-computed so the
    /// face-grouping and face-corner UV seam assignment both see the same
    /// per-vertex choice.
    dominant_joint: u16,
}

#[derive(Clone, Copy, Debug)]
struct SourceFace {
    indices: [usize; 3],
    joint: u16,
}

#[derive(Default)]
struct SkinnedSourceMesh {
    vertices: Vec<SourceVertex>,
    faces: Vec<SourceFace>,
}

#[derive(Clone, Copy)]
struct CookedModelVertex {
    source: SourceVertex,
    primary_joint: u16,
    record: [u8; psxed_format::model::VERTEX_RECORD_SIZE],
}

#[derive(Clone, Copy)]
struct CookedFaceCorner {
    vertex_index: u16,
    uv: (u8, u8),
}

#[derive(Clone, Copy)]
struct ModelBounds {
    center: [f32; 3],
    extent: f32,
}

impl ModelBounds {
    fn from_min_max(min: [f32; 3], max: [f32; 3], coord_limit: f32) -> Result<Self, Error> {
        if !min.iter().all(|v| v.is_finite()) || !max.iter().all(|v| v.is_finite()) {
            return Err(Error::Empty);
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let mut half_extent = 0.0f32;
        for axis in 0..3 {
            half_extent = half_extent.max((max[axis] - min[axis]).abs() * 0.5);
        }
        let coord_limit = coord_limit.clamp(1.0, i16::MAX as f32);
        let extent = half_extent * 4096.0 / coord_limit;
        Ok(Self {
            center,
            extent: extent.max(0.0001),
        })
    }

    fn normalize_point(&self, p: [f32; 3]) -> [f32; 3] {
        [
            (p[0] - self.center[0]) / self.extent,
            (p[1] - self.center[1]) / self.extent,
            (p[2] - self.center[2]) / self.extent,
        ]
    }

    fn encoded_axis_size(&self, min: f32, max: f32) -> i32 {
        let size = ((max - min).abs() / self.extent * 4096.0).round();
        if !size.is_finite() {
            return 0;
        }
        size.max(1.0).min(i32::MAX as f32) as i32
    }
}

#[derive(Clone, Copy, Debug)]
struct PrecisionBounds {
    min: [f32; 3],
    max: [f32; 3],
}

struct BoundsAccumulator {
    min: [f32; 3],
    max: [f32; 3],
    any: bool,
}

impl BoundsAccumulator {
    const fn new() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            any: false,
        }
    }

    fn include(&mut self, p: [f32; 3]) {
        if !p.iter().all(|v| v.is_finite()) {
            return;
        }
        self.any = true;
        for (axis, value) in p.iter().copied().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    fn finish(self) -> Result<PrecisionBounds, Error> {
        if !self.any {
            return Err(Error::Empty);
        }
        Ok(PrecisionBounds {
            min: self.min,
            max: self.max,
        })
    }
}

#[cfg(test)]
mod tests;
