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
    /// cooked-position welding. Zero disables pruning.
    ///
    /// Meshy-style character exports can contain one- or two-triangle
    /// loose scraps that survive normal vertex welding. This pass runs
    /// after the model precision bounds are known so it uses the same
    /// quantised positions as the final `.psxmdl`.
    pub prune_detached_face_islands: u16,
    /// Include standalone animation sources when choosing model/pose
    /// precision bounds. Full bundle imports should keep this enabled
    /// so every generated clip matches the generated model. Add-on
    /// clip bakes should disable it so the new `.psxanim` remains
    /// compatible with the already-cooked target model.
    pub extra_animations_affect_bounds: bool,
}

impl Default for RigidModelConfig {
    fn default() -> Self {
        Self {
            texture_width: 128,
            texture_height: 128,
            texture_depth: psxed_format::texture::Depth::Bit8,
            animation_fps: 15,
            world_height: DEFAULT_MODEL_WORLD_HEIGHT,
            normalize_root_translation: false,
            strip_animation_scale: true,
            prune_detached_face_islands: 4,
            extra_animations_affect_bounds: true,
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
    /// Empty when the source has no animations. One entry per glTF
    /// animation, in source order, each with a filesystem-safe name
    /// derived from `gltf::Animation::name`.
    pub clips: Vec<CookedClip>,
    /// Cooked `.psxt` base-colour texture, if present.
    pub texture: Option<Vec<u8>>,
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

    let mesh_node = document
        .nodes()
        .find(|node| node.mesh().is_some() && node.skin().is_some())
        .ok_or(Error::MissingSkinnedMesh)?;
    let mesh = mesh_node.mesh().ok_or(Error::MissingSkinnedMesh)?;
    let skin = mesh_node.skin().ok_or(Error::MissingSkinnedMesh)?;
    let joints: Vec<usize> = skin.joints().map(|joint| joint.index()).collect();
    if joints.is_empty() {
        return Err(Error::BadSkin("skin has no joints"));
    }
    ensure_u16("joints", joints.len())?;

    let parents = build_parent_indices(document);
    let base_trs = collect_base_trs(document);
    let root_joint_nodes = root_joint_nodes(&joints, &parents);
    let inverse_bind_matrices = read_inverse_bind_matrices(&skin, buffers, joints.len());
    if inverse_bind_matrices.len() != joints.len() {
        return Err(Error::BadSkin(
            "inverse bind matrix count does not match joint count",
        ));
    }

    let mut source = read_skinned_mesh(&mesh, buffers, joints.len())?;
    if source.faces.is_empty() {
        return Err(Error::Empty);
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

    let texture = cook_base_color_texture(&mesh, buffers, cfg)?;
    let material_color = first_material_base_color(&mesh);
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        &parents,
        &joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
    )?;
    let clips = cook_all_animations(
        document,
        buffers,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        &bounds,
        extra_fbx_animation_scenes,
        extra_gltf_animation_scenes,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
    )?;

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

fn convert_fbx_rigid_model_scene(
    scene: &ufbx::Scene,
    source_path: Option<&Path>,
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    convert_fbx_rigid_model_scene_with_extra_animations(scene, source_path, &[], cfg)
}

fn convert_fbx_rigid_model_scene_with_extra_animations(
    scene: &ufbx::Scene,
    source_path: Option<&Path>,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    if cfg.animation_fps == 0 {
        return Err(Error::BadSkin("animation sample rate must be non-zero"));
    }

    let (mesh_node, mesh, skin) = fbx_skinned_mesh(scene)?;
    let node_indices = fbx_node_indices(scene);
    let parents = fbx_parent_indices(scene, &node_indices);
    let base_trs = fbx_base_trs(scene);

    let mut joints = Vec::new();
    let mut inverse_bind_matrices = Vec::new();
    for cluster in &skin.clusters {
        let Some(bone_node) = cluster.bone_node.as_deref() else {
            return Err(Error::BadSkin("FBX skin cluster has no bone node"));
        };
        let joint_index = fbx_node_index(&node_indices, bone_node)?;
        joints.push(joint_index);
        inverse_bind_matrices.push(fbx_matrix_to_mat4(cluster.geometry_to_bone));
    }
    if joints.is_empty() {
        return Err(Error::BadSkin("FBX skin has no joints"));
    }
    ensure_u16("joints", joints.len())?;

    let root_joint_nodes = root_joint_nodes(&joints, &parents);
    let mut source = read_fbx_skinned_mesh(mesh, skin, joints.len())?;
    if source.faces.is_empty() {
        return Err(Error::Empty);
    }
    assign_face_joints(&mut source, joints.len());
    let bounds_extra_animation_scenes = if cfg.extra_animations_affect_bounds {
        extra_animation_scenes
    } else {
        &[]
    };

    let precision_bounds = collect_fbx_precision_bounds(
        scene,
        bounds_extra_animation_scenes,
        &source,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
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
        let precision_bounds = collect_fbx_precision_bounds(
            scene,
            bounds_extra_animation_scenes,
            &source,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            cfg.animation_fps,
            cfg.normalize_root_translation,
            cfg.strip_animation_scale,
        )?;
        let bounds = ModelBounds::from_min_max(
            precision_bounds.min,
            precision_bounds.max,
            MODEL_LOCAL_COORD_LIMIT,
        )?;
        return finish_fbx_rigid_model_scene(
            scene,
            cfg,
            source_path,
            source,
            bounds,
            precision_bounds,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            mesh_node,
            mesh,
            extra_animation_scenes,
        );
    }

    finish_fbx_rigid_model_scene(
        scene,
        cfg,
        source_path,
        source,
        bounds,
        precision_bounds,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        mesh_node,
        mesh,
        extra_animation_scenes,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_fbx_rigid_model_scene(
    scene: &ufbx::Scene,
    cfg: &RigidModelConfig,
    source_path: Option<&Path>,
    source: SkinnedSourceMesh,
    bounds: ModelBounds,
    precision_bounds: PrecisionBounds,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    _mesh_node: &ufbx::Node,
    mesh: &ufbx::Mesh,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
) -> Result<RigidModelPackage, Error> {
    let local_height = bounds.encoded_axis_size(precision_bounds.min[1], precision_bounds.max[1]);
    let local_to_world_q12 = choose_local_to_world_q12(local_height, cfg.world_height);
    let material_color = first_fbx_material_base_color(mesh);
    let texture = cook_fbx_base_color_texture(mesh, source_path, material_color, cfg)?;
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        parents,
        joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
    )?;
    let clips = cook_all_fbx_animations(
        scene,
        extra_animation_scenes,
        parents,
        base_trs,
        root_joint_nodes,
        joints,
        inverse_bind_matrices,
        &bounds,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
    )?;

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
        report,
    })
}

fn fbx_skinned_mesh<'a>(
    scene: &'a ufbx::Scene,
) -> Result<(&'a ufbx::Node, &'a ufbx::Mesh, &'a ufbx::SkinDeformer), Error> {
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.as_deref() else {
            continue;
        };
        let Some(skin) = mesh.skin_deformers.as_ref().first().map(AsRef::as_ref) else {
            continue;
        };
        if mesh.num_vertices > 0 && mesh.num_faces > 0 {
            return Ok((node, mesh, skin));
        }
    }
    Err(Error::MissingFbxSkinnedMesh)
}

fn fbx_node_indices(scene: &ufbx::Scene) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for (index, node) in scene.nodes.as_ref().iter().enumerate() {
        out.insert(node.as_ref() as *const ufbx::Node as usize, index);
    }
    out
}

fn fbx_node_index(node_indices: &HashMap<usize, usize>, node: &ufbx::Node) -> Result<usize, Error> {
    node_indices
        .get(&(node as *const ufbx::Node as usize))
        .copied()
        .ok_or(Error::BadSkin("FBX node is not in the scene node table"))
}

fn fbx_parent_indices(
    scene: &ufbx::Scene,
    node_indices: &HashMap<usize, usize>,
) -> Vec<Option<usize>> {
    scene
        .nodes
        .iter()
        .map(|node| {
            node.parent
                .as_deref()
                .and_then(|parent| fbx_node_index(node_indices, parent).ok())
        })
        .collect()
}

fn fbx_base_trs(scene: &ufbx::Scene) -> Vec<Trs> {
    scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.local_transform))
        .collect()
}

fn fbx_transform_to_trs(transform: ufbx::Transform) -> Trs {
    Trs {
        translation: fbx_vec3(transform.translation),
        rotation: fbx_quat(transform.rotation),
        scale: fbx_vec3(transform.scale),
    }
}

fn fbx_vec3(v: ufbx::Vec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

fn fbx_vec2(v: ufbx::Vec2) -> [f32; 2] {
    [v.x as f32, v.y as f32]
}

fn fbx_quat(q: ufbx::Quat) -> [f32; 4] {
    [q.x as f32, q.y as f32, q.z as f32, q.w as f32]
}

fn fbx_matrix_to_mat4(m: ufbx::Matrix) -> [[f32; 4]; 4] {
    [
        [m.m00 as f32, m.m10 as f32, m.m20 as f32, 0.0],
        [m.m01 as f32, m.m11 as f32, m.m21 as f32, 0.0],
        [m.m02 as f32, m.m12 as f32, m.m22 as f32, 0.0],
        [m.m03 as f32, m.m13 as f32, m.m23 as f32, 1.0],
    ]
}

fn read_fbx_skinned_mesh(
    mesh: &ufbx::Mesh,
    skin: &ufbx::SkinDeformer,
    joint_count: usize,
) -> Result<SkinnedSourceMesh, Error> {
    if !mesh.vertex_position.exists {
        return Err(Error::MissingAttribute {
            primitive_index: 0,
            attribute: "POSITION",
        });
    }

    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    let mut triangulated = Vec::new();
    for face in mesh.faces.iter().copied() {
        triangulated.clear();
        ufbx::triangulate_face_vec(&mut triangulated, mesh, face);
        for tri in triangulated.chunks_exact(3) {
            let mut indices = [0usize; 3];
            for (corner, fbx_index) in tri.iter().copied().enumerate() {
                let fbx_index = fbx_index as usize;
                let Some(source_vertex_index) = mesh
                    .vertex_indices
                    .get(fbx_index)
                    .copied()
                    .map(|index| index as usize)
                else {
                    return Err(Error::BadSkin("FBX face index is outside vertex table"));
                };
                let position = fbx_vec3(mesh.vertex_position[fbx_index]);
                let normal = if mesh.vertex_normal.exists {
                    normalize3(fbx_vec3(mesh.vertex_normal[fbx_index]))
                } else {
                    [0.0, 1.0, 0.0]
                };
                let uv = if mesh.vertex_uv.exists {
                    fbx_vec2(mesh.vertex_uv[fbx_index])
                } else {
                    [0.0, 0.0]
                };
                let (joints, weights) = fbx_skin_weights(skin, source_vertex_index, joint_count);
                let cleaned_joints = joint_indices_or_zero(joints, weights);
                indices[corner] = source.vertices.len();
                source.vertices.push(SourceVertex {
                    position,
                    normal,
                    uv,
                    joints: cleaned_joints,
                    weights,
                    dominant_joint: dominant_vertex_joint(cleaned_joints, weights),
                });
            }
            normal_faces.push(indices);
            source.faces.push(SourceFace {
                indices: [indices[0], indices[2], indices[1]],
                joint: 0,
            });
        }
    }
    rebuild_source_normals(&mut source, &normal_faces);
    Ok(source)
}

fn fbx_skin_weights(
    skin: &ufbx::SkinDeformer,
    vertex_index: usize,
    joint_count: usize,
) -> ([u16; 4], [f32; 4]) {
    let mut influences: Vec<(u16, f32)> = Vec::new();
    if let Some(vertex) = skin.vertices.get(vertex_index) {
        let begin = vertex.weight_begin as usize;
        let end = begin.saturating_add(vertex.num_weights as usize);
        for weight in skin.weights.get(begin..end).unwrap_or(&[]) {
            let joint = weight.cluster_index as usize;
            if joint < joint_count && weight.weight > 0.0 {
                influences.push((joint as u16, weight.weight as f32));
            }
        }
    }
    influences.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    let mut total = 0.0f32;
    for (slot, (joint, weight)) in influences.into_iter().take(4).enumerate() {
        joints[slot] = joint;
        weights[slot] = weight;
        total += weight;
    }
    if total > 0.0 {
        for weight in &mut weights {
            *weight /= total;
        }
    } else {
        weights[0] = 1.0;
    }
    (joints, weights)
}

#[allow(clippy::too_many_arguments)]
fn collect_fbx_precision_bounds(
    scene: &ufbx::Scene,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<PrecisionBounds, Error> {
    let mut bounds = BoundsAccumulator::new();
    include_pose_bounds(
        &mut bounds,
        base_trs,
        source,
        parents,
        joints,
        inverse_bind_matrices,
        strip_animation_scale,
    );

    for stack in &scene.anim_stacks {
        let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
            continue;
        };
        let frame_count = ((max_time - min_time) * fps as f64).round() as usize + 1;
        ensure_u16("animation frames", frame_count)?;
        for frame in 0..frame_count {
            let time = (min_time + frame as f64 / fps as f64).min(max_time);
            let mut frame_trs = evaluate_fbx_frame_trs(scene, &stack.anim, time);
            if normalize_root_translation {
                restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
            }
            include_pose_bounds(
                &mut bounds,
                &frame_trs,
                source,
                parents,
                joints,
                inverse_bind_matrices,
                strip_animation_scale,
            );
        }
    }

    for extra in extra_animation_scenes {
        let mapping = fbx_animation_node_mapping(scene, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let frame_count = ((max_time - min_time) * fps as f64).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f64 / fps as f64).min(max_time);
                let mut frame_trs = evaluate_mapped_fbx_frame_trs(
                    extra.scene,
                    &stack.anim,
                    time,
                    parents,
                    base_trs,
                    &mapping,
                );
                if normalize_root_translation {
                    restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
                }
                include_pose_bounds(
                    &mut bounds,
                    &frame_trs,
                    source,
                    parents,
                    joints,
                    inverse_bind_matrices,
                    strip_animation_scale,
                );
            }
        }
    }

    bounds.finish()
}

fn fbx_stack_time_range(stack: &ufbx::AnimStack) -> Option<(f64, f64)> {
    let min_time = stack.time_begin;
    let max_time = stack.time_end;
    if min_time.is_finite() && max_time.is_finite() && max_time >= min_time {
        Some((min_time, max_time))
    } else {
        None
    }
}

fn evaluate_fbx_frame_trs(scene: &ufbx::Scene, anim: &ufbx::Anim, time: f64) -> Vec<Trs> {
    scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.evaluate_transform(anim, time)))
        .collect()
}

fn evaluate_mapped_fbx_frame_trs(
    animation_scene: &ufbx::Scene,
    anim: &ufbx::Anim,
    time: f64,
    target_parents: &[Option<usize>],
    base_trs: &[Trs],
    mapping: &[Option<usize>],
) -> Vec<Trs> {
    let evaluated: Vec<Trs> = animation_scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.evaluate_transform(anim, time)))
        .collect();
    let animation_base = fbx_base_trs(animation_scene);
    let node_indices = fbx_node_indices(animation_scene);
    let source_parents = fbx_parent_indices(animation_scene, &node_indices);
    retarget_mapped_frame_trs(
        target_parents,
        base_trs,
        &source_parents,
        &animation_base,
        &evaluated,
        mapping,
    )
}

fn evaluate_mapped_gltf_frame_trs(
    channels: &[AnimationChannel],
    time: f32,
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    mapping: &[Option<usize>],
    copy_full_local_trs: bool,
) -> Vec<Trs> {
    let mut source_pose_trs = source_base_trs.to_vec();
    for channel in channels {
        channel.apply(time, &mut source_pose_trs);
    }

    if !copy_full_local_trs {
        return retarget_mapped_frame_trs(
            target_parents,
            target_base_trs,
            source_parents,
            source_base_trs,
            &source_pose_trs,
            mapping,
        );
    }

    let mut out = target_base_trs.to_vec();
    for (target_index, source_index) in mapping.iter().copied().enumerate() {
        let Some(source_index) = source_index else {
            continue;
        };
        let (Some(target), Some(source)) =
            (out.get_mut(target_index), source_pose_trs.get(source_index))
        else {
            continue;
        };
        *target = *source;
    }
    out
}

fn retarget_mapped_frame_trs(
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    source_pose_trs: &[Trs],
    mapping: &[Option<usize>],
) -> Vec<Trs> {
    let source_base_global = compute_global_rotations(source_parents, source_base_trs);
    let source_pose_global = compute_global_rotations(source_parents, source_pose_trs);
    let target_base_global = compute_global_rotations(target_parents, target_base_trs);

    let mut out = target_base_trs.to_vec();
    let mut target_pose_global = vec![identity_quat(); target_base_trs.len()];
    let mut done = vec![false; target_base_trs.len()];
    for index in 0..target_base_trs.len() {
        retarget_target_node_rotation(
            index,
            target_parents,
            target_base_trs,
            &target_base_global,
            &source_base_global,
            &source_pose_global,
            mapping,
            &mut out,
            &mut target_pose_global,
            &mut done,
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn retarget_target_node_rotation(
    index: usize,
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    target_base_global: &[[f32; 4]],
    source_base_global: &[[f32; 4]],
    source_pose_global: &[[f32; 4]],
    mapping: &[Option<usize>],
    out: &mut [Trs],
    target_pose_global: &mut [[f32; 4]],
    done: &mut [bool],
) -> [f32; 4] {
    if done[index] {
        return target_pose_global[index];
    }
    let parent_global = if let Some(parent) = target_parents[index] {
        retarget_target_node_rotation(
            parent,
            target_parents,
            target_base_trs,
            target_base_global,
            source_base_global,
            source_pose_global,
            mapping,
            out,
            target_pose_global,
            done,
        )
    } else {
        identity_quat()
    };
    let local_rotation = mapping
        .get(index)
        .copied()
        .flatten()
        .and_then(|source_index| {
            let source_base = source_base_global.get(source_index).copied()?;
            let source_pose = source_pose_global.get(source_index).copied()?;
            let target_base = target_base_global.get(index).copied()?;
            let local_delta = quat_mul(quat_inverse(source_base), source_pose);
            let desired_global = quat_mul(target_base, local_delta);
            Some(quat_mul(quat_inverse(parent_global), desired_global))
        })
        .unwrap_or(target_base_trs[index].rotation);
    out[index].translation = target_base_trs[index].translation;
    out[index].rotation = local_rotation;
    out[index].scale = target_base_trs[index].scale;
    let global = quat_mul(parent_global, local_rotation);
    target_pose_global[index] = global;
    done[index] = true;
    global
}

fn fbx_animation_node_mapping(
    model_scene: &ufbx::Scene,
    animation_scene: &ufbx::Scene,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for (index, node) in animation_scene.nodes.iter().enumerate() {
        let key = node_match_key(&node.element.name);
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(index);
        }
    }
    model_scene
        .nodes
        .iter()
        .map(|node| {
            let key = node_match_key(&node.element.name);
            animation_nodes.get(&key).copied()
        })
        .collect()
}

fn gltf_fbx_animation_node_mapping(
    document: &gltf::Document,
    animation_scene: &ufbx::Scene,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for (index, node) in animation_scene.nodes.iter().enumerate() {
        let key = node_match_key(&node.element.name);
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(index);
        }
    }
    document
        .nodes()
        .map(|node| {
            let key = node.name().map(node_match_key).unwrap_or_default();
            animation_nodes.get(&key).copied()
        })
        .collect()
}

fn gltf_gltf_animation_node_mapping(
    model_document: &gltf::Document,
    animation_document: &gltf::Document,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for node in animation_document.nodes() {
        let key = node.name().map(node_match_key).unwrap_or_default();
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(node.index());
        }
    }
    model_document
        .nodes()
        .map(|node| {
            let key = node.name().map(node_match_key).unwrap_or_default();
            animation_nodes.get(&key).copied()
        })
        .collect()
}

fn validate_fbx_animation_mapping(
    animation_scene: &ufbx::Scene,
    mapping: &[Option<usize>],
) -> Result<(), Error> {
    if animation_scene.anim_stacks.is_empty() {
        Ok(())
    } else {
        let mapped = mapping.iter().filter(|entry| entry.is_some()).count();
        if mapped >= 6 {
            Ok(())
        } else if mapped == 0 {
            Err(Error::BadSkin(
                "FBX animation shares no named skeleton nodes with the model",
            ))
        } else {
            Err(Error::BadSkin(
                "FBX animation maps too few humanoid skeleton nodes to retarget safely",
            ))
        }
    }
}

fn validate_gltf_animation_mapping(
    animation_document: &gltf::Document,
    mapping: &[Option<usize>],
) -> Result<(), Error> {
    if animation_document.animations().next().is_none() {
        Ok(())
    } else {
        let mapped = mapping.iter().filter(|entry| entry.is_some()).count();
        if mapped >= 6 {
            Ok(())
        } else if mapped == 0 {
            Err(Error::BadSkin(
                "glTF animation shares no named skeleton nodes with the model",
            ))
        } else {
            Err(Error::BadSkin(
                "glTF animation maps too few humanoid skeleton nodes to retarget safely",
            ))
        }
    }
}

fn mapped_local_binds_match(
    target_base_trs: &[Trs],
    source_base_trs: &[Trs],
    mapping: &[Option<usize>],
) -> bool {
    let mut compared = 0usize;
    for (target_index, source_index) in mapping.iter().copied().enumerate() {
        let Some(source_index) = source_index else {
            continue;
        };
        let (Some(target), Some(source)) = (
            target_base_trs.get(target_index),
            source_base_trs.get(source_index),
        ) else {
            return false;
        };
        compared += 1;
        if !vec3_close(target.translation, source.translation, 0.01)
            || !vec3_close(target.scale, source.scale, 0.001)
            || !quat_close_same_orientation(target.rotation, source.rotation, 0.999)
        {
            return false;
        }
    }
    compared >= 6
}

fn node_match_key(name: &str) -> String {
    let raw = name
        .rsplit([':', '|'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_lowercase();
    let compact: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    match raw.as_str() {
        "spine_01" | "spine 01" => return "spine1".to_string(),
        "spine_02" | "spine 02" => return "spine2".to_string(),
        "spine_03" | "spine 03" => return "spine3".to_string(),
        _ => {}
    }
    match compact.as_str() {
        "rootnode" | "root" => "root".to_string(),
        "armature" => "armature".to_string(),
        "hips" | "pelvis" => "hips".to_string(),
        "spine" => "spine1".to_string(),
        "spine1" | "spine01" => "spine2".to_string(),
        "spine2" | "spine02" | "spine3" | "spine03" => "spine3".to_string(),
        "neck" | "neck01" => "neck".to_string(),
        "head" => "head".to_string(),
        "leftshoulder" | "claviclel" | "leftclavicle" | "clavicleleft" => {
            "leftshoulder".to_string()
        }
        "rightshoulder" | "clavicler" | "rightclavicle" | "clavicleright" => {
            "rightshoulder".to_string()
        }
        "leftarm" | "shoulderl" | "leftupperarm" | "upperarml" | "upperarmleft" => {
            "leftarm".to_string()
        }
        "rightarm" | "shoulderr" | "rightupperarm" | "upperarmr" | "upperarmright" => {
            "rightarm".to_string()
        }
        "leftforearm" | "elbowl" | "leftlowerarm" | "lowerarml" | "lowerarmleft" => {
            "leftforearm".to_string()
        }
        "rightforearm" | "elbowr" | "rightlowerarm" | "lowerarmr" | "lowerarmright" => {
            "rightforearm".to_string()
        }
        "lefthand" | "handl" => "lefthand".to_string(),
        "righthand" | "handr" => "righthand".to_string(),
        "leftupleg" | "leftupperleg" | "upperlegl" | "thighl" | "thighleft" => {
            "leftupleg".to_string()
        }
        "rightupleg" | "rightupperleg" | "upperlegr" | "thighr" | "thighright" => {
            "rightupleg".to_string()
        }
        "leftleg" | "leftlowerleg" | "lowerlegl" | "calfl" | "calfleft" => "leftleg".to_string(),
        "rightleg" | "rightlowerleg" | "lowerlegr" | "calfr" | "calfright" => {
            "rightleg".to_string()
        }
        "leftfoot" | "anklel" | "footl" | "footleft" => "leftfoot".to_string(),
        "rightfoot" | "ankler" | "footr" | "footright" => "rightfoot".to_string(),
        "lefttoebase" | "lefttoe" | "toesl" | "balll" | "ballleft" => "lefttoebase".to_string(),
        "righttoebase" | "righttoe" | "toesr" | "ballr" | "ballright" => "righttoebase".to_string(),
        _ => compact,
    }
}

#[allow(clippy::too_many_arguments)]
fn cook_all_fbx_animations(
    scene: &ufbx::Scene,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<Vec<CookedClip>, Error> {
    let mut clips = Vec::new();
    for (index, stack) in scene.anim_stacks.iter().enumerate() {
        let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
            continue;
        };
        let Some(bytes) = cook_fbx_animation_bytes(
            scene,
            &stack.anim,
            parents,
            base_trs,
            root_joint_nodes,
            joints,
            inverse_bind_matrices,
            bounds,
            min_time,
            max_time,
            fps,
            normalize_root_translation,
            strip_animation_scale,
            None,
        )?
        else {
            continue;
        };
        let frames = animation_frame_count_from_bytes(&bytes);
        let raw_name = fbx_stack_source_name(stack, None);
        clips.push(CookedClip {
            source_name: raw_name.clone(),
            sanitized_name: sanitize_clip_name(raw_name.as_deref(), index),
            bytes,
            frames,
        });
    }
    let mut clip_index = clips.len();
    for extra in extra_animation_scenes {
        let mapping = fbx_animation_node_mapping(scene, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let Some(bytes) = cook_fbx_animation_bytes(
                extra.scene,
                &stack.anim,
                parents,
                base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                Some(&mapping),
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = fbx_stack_source_name(stack, extra.fallback_name);
            clips.push(CookedClip {
                source_name: raw_name.clone(),
                sanitized_name: sanitize_clip_name(raw_name.as_deref(), clip_index),
                bytes,
                frames,
            });
            clip_index += 1;
        }
    }
    Ok(clips)
}

fn fbx_stack_source_name(stack: &ufbx::AnimStack, fallback_name: Option<&str>) -> Option<String> {
    let stack_name = stack.element.name.trim();
    if !stack_name.is_empty() && stack_name != "Take 001" && stack_name != "mixamo.com" {
        Some(stack_name.to_string())
    } else {
        fallback_name.map(str::to_string)
    }
}

#[allow(clippy::too_many_arguments)]
fn cook_fbx_animation_bytes(
    scene: &ufbx::Scene,
    anim: &ufbx::Anim,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f64,
    max_time: f64,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    node_mapping: Option<&[Option<usize>]>,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f64).round() as usize + 1;
    ensure_u16("animation frames", frame_count)?;

    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + frame_count * joints.len() * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    append_asset_header(
        &mut out,
        psxed_format::animation::MAGIC,
        psxed_format::animation::VERSION,
        0,
        payload_len,
    )?;
    append_u16(&mut out, ensure_u16("joints", joints.len())?);
    append_u16(&mut out, ensure_u16("animation frames", frame_count)?);
    append_u16(&mut out, fps);
    append_u16(&mut out, 0);

    for frame in 0..frame_count {
        let time = (min_time + frame as f64 / fps as f64).min(max_time);
        let mut frame_trs = if let Some(mapping) = node_mapping {
            evaluate_mapped_fbx_frame_trs(scene, anim, time, parents, base_trs, mapping)
        } else {
            evaluate_fbx_frame_trs(scene, anim, time)
        };
        if normalize_root_translation {
            restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
        }
        let locals: Vec<[[f32; 4]; 4]> = frame_trs.iter().map(|trs| trs.matrix()).collect();
        let globals = compute_global_matrices(parents, &locals);
        for (joint_index, node_index) in joints.iter().copied().enumerate() {
            let mut skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                skin = strip_pose_scale(skin);
            }
            append_pose_record(&mut out, &skin, bounds);
        }
    }

    Ok(Some(out))
}

fn first_fbx_material_base_color(mesh: &ufbx::Mesh) -> [u8; 4] {
    let Some(material) = mesh.materials.as_ref().first().map(AsRef::as_ref) else {
        return [255, 255, 255, 255];
    };
    let map = if material.pbr.base_color.has_value {
        &material.pbr.base_color
    } else {
        &material.fbx.diffuse_color
    };
    if map.has_value {
        [
            linear_to_u8(map.value_vec4.x as f32),
            linear_to_u8(map.value_vec4.y as f32),
            linear_to_u8(map.value_vec4.z as f32),
            linear_to_u8(map.value_vec4.w as f32),
        ]
    } else {
        [255, 255, 255, 255]
    }
}

fn cook_fbx_base_color_texture(
    mesh: &ufbx::Mesh,
    source_path: Option<&Path>,
    fallback_color: [u8; 4],
    cfg: &RigidModelConfig,
) -> Result<Option<Vec<u8>>, Error> {
    let tex_cfg = psxed_tex::Config {
        width: cfg.texture_width,
        height: cfg.texture_height,
        depth: cfg.texture_depth,
        crop: psxed_tex::CropMode::None,
        resampler: psxed_tex::Resampler::Lanczos3,
        transparent_index_zero: true,
    };
    for material in &mesh.materials {
        let texture = material
            .pbr
            .base_color
            .texture
            .as_deref()
            .or_else(|| material.fbx.diffuse_color.texture.as_deref());
        let Some(texture) = texture else {
            continue;
        };
        if !texture.content.is_empty() {
            return psxed_tex::convert(&texture.content, &tex_cfg)
                .map(Some)
                .map_err(Error::TextureCook);
        }
        for filename in [
            &texture.absolute_filename,
            &texture.filename,
            &texture.relative_filename,
        ] {
            if filename.is_empty() {
                continue;
            }
            if let Some(path) = resolve_fbx_texture_path(Path::new(filename.as_ref()), source_path)
            {
                if let Ok(bytes) = std::fs::read(path) {
                    return psxed_tex::convert(&bytes, &tex_cfg)
                        .map(Some)
                        .map_err(Error::TextureCook);
                }
            }
        }
    }

    if let Some(path) = source_path.and_then(find_companion_fbx_texture) {
        if let Ok(bytes) = std::fs::read(path) {
            return psxed_tex::convert(&bytes, &tex_cfg)
                .map(Some)
                .map_err(Error::TextureCook);
        }
    }

    let bmp = solid_color_bmp(
        cfg.texture_width.max(1) as u32,
        cfg.texture_height.max(1) as u32,
        fallback_color,
    );
    psxed_tex::convert(&bmp, &tex_cfg)
        .map(Some)
        .map_err(Error::TextureCook)
}

fn resolve_fbx_texture_path(texture_path: &Path, source_path: Option<&Path>) -> Option<PathBuf> {
    if texture_path.exists() {
        return Some(texture_path.to_path_buf());
    }
    if texture_path.is_absolute() {
        return None;
    }
    let source_dir = source_path.and_then(Path::parent)?;
    let candidate = source_dir.join(texture_path);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

fn find_companion_fbx_texture(source_path: &Path) -> Option<PathBuf> {
    let stem = source_path.file_stem()?.to_str()?;
    let source_dir = source_path.parent()?;
    let mut search_dirs = vec![source_dir.to_path_buf()];
    if let Some(parent) = source_dir.parent() {
        search_dirs.push(parent.join(format!("{stem}_obj")));
        search_dirs.push(parent.join(format!("{stem}_fbx")));
    }
    for dir in search_dirs {
        for ext in ["png", "jpg", "jpeg", "bmp", "tga", "webp"] {
            let candidate = dir.join(format!("{stem}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn solid_color_bmp(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let row_stride = (width * 3).div_ceil(4) * 4;
    let pixel_bytes = row_stride * height;
    let file_size = 14 + 40 + pixel_bytes;
    let mut out = Vec::with_capacity(file_size as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&(14u32 + 40u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixel_bytes.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    let pad = (row_stride - width * 3) as usize;
    for _ in 0..height {
        for _ in 0..width {
            out.push(color[2]);
            out.push(color[1]);
            out.push(color[0]);
        }
        out.extend(std::iter::repeat(0u8).take(pad));
    }
    out
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
    /// face-grouping pass and the seam-duplication step both see the
    /// same per-vertex choice.
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

#[allow(clippy::too_many_arguments)]
fn collect_precision_bounds(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<PrecisionBounds, Error> {
    let mut bounds = BoundsAccumulator::new();
    include_pose_bounds(
        &mut bounds,
        base_trs,
        source,
        parents,
        joints,
        inverse_bind_matrices,
        strip_animation_scale,
    );

    for animation in document.animations() {
        let channels = read_animation_channels(&animation, buffers)?;
        if channels.is_empty() {
            continue;
        }

        let Some((min_time, max_time)) = channel_time_range(&channels) else {
            continue;
        };

        let duration = max_time - min_time;
        let frame_count = (duration * fps as f32).round() as usize + 1;
        ensure_u16("animation frames", frame_count)?;
        for frame in 0..frame_count {
            let time = (min_time + frame as f32 / fps as f32).min(max_time);
            let mut frame_trs = base_trs.to_vec();
            for channel in &channels {
                channel.apply(time, &mut frame_trs);
            }
            if normalize_root_translation {
                restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
            }
            include_pose_bounds(
                &mut bounds,
                &frame_trs,
                source,
                parents,
                joints,
                inverse_bind_matrices,
                strip_animation_scale,
            );
        }
    }

    for extra in extra_fbx_animation_scenes {
        let mapping = gltf_fbx_animation_node_mapping(document, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let frame_count = ((max_time - min_time) * fps as f64).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f64 / fps as f64).min(max_time);
                let mut frame_trs = evaluate_mapped_fbx_frame_trs(
                    extra.scene,
                    &stack.anim,
                    time,
                    parents,
                    base_trs,
                    &mapping,
                );
                if normalize_root_translation {
                    restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
                }
                include_pose_bounds(
                    &mut bounds,
                    &frame_trs,
                    source,
                    parents,
                    joints,
                    inverse_bind_matrices,
                    strip_animation_scale,
                );
            }
        }
    }

    for extra in extra_gltf_animation_scenes {
        let mapping = gltf_gltf_animation_node_mapping(document, extra.document);
        validate_gltf_animation_mapping(extra.document, &mapping)?;
        let source_parents = build_parent_indices(extra.document);
        let source_base_trs = collect_base_trs(extra.document);
        let copy_full_local_trs = mapped_local_binds_match(base_trs, &source_base_trs, &mapping);
        for animation in extra.document.animations() {
            let channels = read_animation_channels(&animation, extra.buffers)?;
            if channels.is_empty() {
                continue;
            }
            let Some((min_time, max_time)) = channel_time_range(&channels) else {
                continue;
            };
            let duration = max_time - min_time;
            let frame_count = (duration * fps as f32).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f32 / fps as f32).min(max_time);
                let mut frame_trs = evaluate_mapped_gltf_frame_trs(
                    &channels,
                    time,
                    parents,
                    base_trs,
                    &source_parents,
                    &source_base_trs,
                    &mapping,
                    copy_full_local_trs,
                );
                if normalize_root_translation {
                    restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
                }
                include_pose_bounds(
                    &mut bounds,
                    &frame_trs,
                    source,
                    parents,
                    joints,
                    inverse_bind_matrices,
                    strip_animation_scale,
                );
            }
        }
    }

    bounds.finish()
}

fn channel_time_range(channels: &[AnimationChannel]) -> Option<(f32, f32)> {
    let mut min_time = f32::INFINITY;
    let mut max_time = f32::NEG_INFINITY;
    for channel in channels {
        for time in &channel.times {
            min_time = min_time.min(*time);
            max_time = max_time.max(*time);
        }
    }
    if min_time.is_finite() && max_time.is_finite() && max_time >= min_time {
        Some((min_time, max_time))
    } else {
        None
    }
}

fn include_pose_bounds(
    bounds: &mut BoundsAccumulator,
    trs: &[Trs],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    strip_animation_scale: bool,
) {
    let locals: Vec<[[f32; 4]; 4]> = trs.iter().map(|trs| trs.matrix()).collect();
    let globals = compute_global_matrices(parents, &locals);
    let skins: Vec<[[f32; 4]; 4]> = joints
        .iter()
        .copied()
        .enumerate()
        .map(|(joint_index, node_index)| {
            let skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                strip_pose_scale(skin)
            } else {
                skin
            }
        })
        .collect();

    for face in &source.faces {
        let skin = skins[face.joint as usize];
        for vertex_index in face.indices {
            bounds.include(transform_point(
                &skin,
                source.vertices[vertex_index].position,
            ));
        }
    }
}

fn choose_local_to_world_q12(local_height: i32, world_height: u16) -> u16 {
    if local_height <= 0 || world_height == 0 {
        return psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12;
    }
    let local_height = local_height as u32;
    let numerator = world_height as u32 * 4096;
    ((numerator + local_height / 2) / local_height).clamp(1, u16::MAX as u32) as u16
}

fn read_skinned_mesh(
    mesh: &gltf::Mesh<'_>,
    buffers: &[gltf::buffer::Data],
    joint_count: usize,
) -> Result<SkinnedSourceMesh, Error> {
    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    for primitive in mesh.primitives() {
        let primitive_index = primitive.index();
        if primitive.mode() != Mode::Triangles
            && primitive.mode() != Mode::TriangleStrip
            && primitive.mode() != Mode::TriangleFan
        {
            return Err(Error::UnsupportedMode {
                mode: primitive.mode(),
            });
        }
        let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "POSITION",
            })?
            .collect();
        let normals: Option<Vec<[f32; 3]>> = reader
            .read_normals()
            .map(|iter| iter.map(normalize3).collect());
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "TEXCOORD_0",
            })?
            .into_f32()
            .collect();
        let joints: Vec<[u16; 4]> = reader
            .read_joints(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "JOINTS_0",
            })?
            .into_u16()
            .collect();
        let weights: Vec<[f32; 4]> = reader
            .read_weights(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "WEIGHTS_0",
            })?
            .into_f32()
            .collect();

        let vertex_count = positions.len();
        if normals
            .as_ref()
            .is_some_and(|normals| normals.len() != vertex_count)
            || uvs.len() != vertex_count
            || joints.len() != vertex_count
            || weights.len() != vertex_count
        {
            return Err(Error::BadSkin("primitive attribute counts differ"));
        }
        for (joint_indices, vertex_weights) in joints.iter().zip(&weights) {
            if joint_indices
                .iter()
                .zip(vertex_weights)
                .any(|(joint, weight)| *weight > 0.0 && *joint as usize >= joint_count)
            {
                return Err(Error::BadSkin(
                    "vertex joint index outside skin joint table",
                ));
            }
        }

        let base = source.vertices.len();
        source.vertices.extend((0..vertex_count).map(|index| {
            let cleaned_joints = joint_indices_or_zero(joints[index], weights[index]);
            SourceVertex {
                position: positions[index],
                normal: normals
                    .as_ref()
                    .map(|normals| normals[index])
                    .unwrap_or([0.0, 1.0, 0.0]),
                uv: uvs[index],
                joints: cleaned_joints,
                weights: weights[index],
                dominant_joint: dominant_vertex_joint(cleaned_joints, weights[index]),
            }
        }));

        let local_indices: Vec<u32> = if let Some(indices) = reader.read_indices() {
            indices.into_u32().collect()
        } else {
            (0..vertex_count as u32).collect()
        };
        for face in triangulate_indices(&local_indices, primitive.mode())? {
            let a = checked_index(face[0], vertex_count)? + base;
            let b = checked_index(face[1], vertex_count)? + base;
            let c = checked_index(face[2], vertex_count)? + base;
            normal_faces.push([a, b, c]);
            // glTF uses CCW front faces. The engine/GTE render
            // path culls by positive screen-space NCLIP after the
            // PS1-style Y projection flip, so cook imported models
            // into that convention once instead of making every
            // runtime submitter special-case glTF winding.
            source.faces.push(SourceFace {
                indices: [a, c, b],
                joint: 0,
            });
        }
    }
    // Normals stay in the source surface convention: only the cooked
    // packet index order is converted for GTE/NCLIP culling.
    rebuild_source_normals(&mut source, &normal_faces);
    Ok(source)
}

fn rebuild_source_normals(source: &mut SkinnedSourceMesh, normal_faces: &[[usize; 3]]) {
    let mut normals = vec![[0.0f32; 3]; source.vertices.len()];
    for face in normal_faces {
        let a = source.vertices[face[0]].position;
        let b = source.vertices[face[1]].position;
        let c = source.vertices[face[2]].position;
        let normal = cross3(sub3(b, a), sub3(c, a));
        if length_sq3(normal) <= 0.000001 {
            continue;
        }
        for index in *face {
            normals[index][0] += normal[0];
            normals[index][1] += normal[1];
            normals[index][2] += normal[2];
        }
    }

    for (vertex, normal) in source.vertices.iter_mut().zip(normals) {
        if length_sq3(normal) > 0.000001 {
            vertex.normal = normalize3(normal);
        } else {
            vertex.normal = normalize3(vertex.normal);
        }
    }
}

fn prune_detached_face_islands(
    source: &mut SkinnedSourceMesh,
    bounds: &ModelBounds,
    max_faces: usize,
) -> usize {
    if max_faces == 0 || source.faces.len() <= 1 {
        return 0;
    }

    let mut faces_by_position: BTreeMap<[i16; 3], Vec<usize>> = BTreeMap::new();
    for (face_index, face) in source.faces.iter().enumerate() {
        for vertex_index in face.indices {
            let key = model_vertex_position_key_for_source(source.vertices[vertex_index], bounds);
            let faces = faces_by_position.entry(key).or_default();
            if faces.last().copied() != Some(face_index) {
                faces.push(face_index);
            }
        }
    }

    let mut adjacency = vec![Vec::new(); source.faces.len()];
    for faces in faces_by_position.values() {
        for (offset, face_a) in faces.iter().copied().enumerate() {
            for face_b in faces.iter().copied().skip(offset + 1) {
                adjacency[face_a].push(face_b);
                adjacency[face_b].push(face_a);
            }
        }
    }

    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut seen = vec![false; source.faces.len()];
    for start in 0..source.faces.len() {
        if seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        seen[start] = true;
        while let Some(face_index) = stack.pop() {
            component.push(face_index);
            for next in adjacency[face_index].iter().copied() {
                if !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        components.push(component);
    }

    if components.len() <= 1 {
        return 0;
    }
    let Some((largest_index, largest)) = components
        .iter()
        .enumerate()
        .max_by_key(|(_, component)| component.len())
    else {
        return 0;
    };
    if largest.len() <= max_faces {
        return 0;
    }

    let mut keep = vec![true; source.faces.len()];
    let mut removed = 0usize;
    for (component_index, component) in components.iter().enumerate() {
        if component_index == largest_index || component.len() > max_faces {
            continue;
        }
        for face_index in component {
            keep[*face_index] = false;
            removed += 1;
        }
    }
    if removed == 0 {
        return 0;
    }

    source.faces = source
        .faces
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, face)| keep[index].then_some(face))
        .collect();
    removed
}

fn joint_indices_or_zero(joints: [u16; 4], weights: [f32; 4]) -> [u16; 4] {
    let mut out = joints;
    for i in 0..4 {
        if weights[i] <= 0.0 {
            out[i] = 0;
        }
    }
    out
}

fn dominant_vertex_joint(joints: [u16; 4], weights: [f32; 4]) -> u16 {
    let mut best = 0u16;
    let mut best_weight = f32::NEG_INFINITY;
    for influence in 0..4 {
        let weight = weights[influence];
        if weight > best_weight {
            best_weight = weight;
            best = joints[influence];
        }
    }
    best
}

/// Group faces by per-vertex bone choice. Each vertex has already
/// picked its dominant bone in `read_skinned_mesh`. A face is bound
/// to whichever bone owns the **majority** of its three corners.
///
/// On a 2-1 split the majority vertex pair wins, leaving the third
/// corner as the only "pulled" vertex bound to a foreign bone -- much
/// less visible than the previous face-level binding, which could
/// pull all three corners together. On a 3-way disagreement we fall
/// back to summed-weight scoring so the choice still reflects the
/// face's overall bias.
fn assign_face_joints(source: &mut SkinnedSourceMesh, joint_count: usize) {
    let mut scores = vec![0.0f32; joint_count];
    for face in &mut source.faces {
        let bones = [
            source.vertices[face.indices[0]].dominant_joint,
            source.vertices[face.indices[1]].dominant_joint,
            source.vertices[face.indices[2]].dominant_joint,
        ];

        if bones[0] == bones[1] || bones[0] == bones[2] {
            face.joint = bones[0];
            continue;
        }
        if bones[1] == bones[2] {
            face.joint = bones[1];
            continue;
        }

        scores.fill(0.0);
        for vertex_index in face.indices {
            let vertex = source.vertices[vertex_index];
            for influence in 0..4 {
                let weight = vertex.weights[influence].max(0.0);
                if weight > 0.0 {
                    let joint = vertex.joints[influence] as usize;
                    if joint < scores.len() {
                        scores[joint] += weight;
                    }
                }
            }
        }
        let mut best_joint = 0usize;
        let mut best_score = f32::NEG_INFINITY;
        for (joint, score) in scores.iter().copied().enumerate() {
            if score > best_score {
                best_joint = joint;
                best_score = score;
            }
        }
        face.joint = best_joint as u16;
    }
}

fn read_inverse_bind_matrices(
    skin: &gltf::Skin<'_>,
    buffers: &[gltf::buffer::Data],
    joint_count: usize,
) -> Vec<[[f32; 4]; 4]> {
    let reader = skin.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
    reader
        .read_inverse_bind_matrices()
        .map(|iter| iter.collect())
        .unwrap_or_else(|| vec![identity_matrix(); joint_count])
}

fn build_parent_indices(document: &gltf::Document) -> Vec<Option<usize>> {
    let mut parents = vec![None; document.nodes().count()];
    for node in document.nodes() {
        for child in node.children() {
            parents[child.index()] = Some(node.index());
        }
    }
    parents
}

fn root_joint_nodes(joints: &[usize], parents: &[Option<usize>]) -> Vec<usize> {
    let mut is_joint = vec![false; parents.len()];
    for node in joints.iter().copied() {
        if let Some(slot) = is_joint.get_mut(node) {
            *slot = true;
        }
    }
    joints
        .iter()
        .copied()
        .filter(|node| {
            parents
                .get(*node)
                .copied()
                .flatten()
                .and_then(|parent| is_joint.get(parent).copied())
                != Some(true)
        })
        .collect()
}

fn collect_base_trs(document: &gltf::Document) -> Vec<Trs> {
    document
        .nodes()
        .map(|node| {
            let (translation, rotation, scale) = node.transform().decomposed();
            Trs {
                translation,
                rotation,
                scale,
            }
        })
        .collect()
}

fn compute_global_matrices(
    parents: &[Option<usize>],
    locals: &[[[f32; 4]; 4]],
) -> Vec<[[f32; 4]; 4]> {
    let mut globals = vec![identity_matrix(); locals.len()];
    let mut done = vec![false; locals.len()];
    for index in 0..locals.len() {
        compute_global_matrix(index, parents, locals, &mut globals, &mut done);
    }
    globals
}

fn compute_global_matrix(
    index: usize,
    parents: &[Option<usize>],
    locals: &[[[f32; 4]; 4]],
    globals: &mut [[[f32; 4]; 4]],
    done: &mut [bool],
) -> [[f32; 4]; 4] {
    if done[index] {
        return globals[index];
    }
    let global = if let Some(parent) = parents[index] {
        let parent_global = compute_global_matrix(parent, parents, locals, globals, done);
        mul_matrix(&parent_global, &locals[index])
    } else {
        locals[index]
    };
    globals[index] = global;
    done[index] = true;
    global
}

fn compute_global_rotations(parents: &[Option<usize>], trs: &[Trs]) -> Vec<[f32; 4]> {
    let mut globals = vec![identity_quat(); trs.len()];
    let mut done = vec![false; trs.len()];
    for index in 0..trs.len() {
        compute_global_rotation(index, parents, trs, &mut globals, &mut done);
    }
    globals
}

fn compute_global_rotation(
    index: usize,
    parents: &[Option<usize>],
    trs: &[Trs],
    globals: &mut [[f32; 4]],
    done: &mut [bool],
) -> [f32; 4] {
    if done[index] {
        return globals[index];
    }
    let global = if let Some(parent) = parents[index] {
        let parent_global = compute_global_rotation(parent, parents, trs, globals, done);
        quat_mul(parent_global, trs[index].rotation)
    } else {
        normalize4(trs[index].rotation)
    };
    globals[index] = global;
    done[index] = true;
    global
}

#[allow(clippy::too_many_arguments)]
fn cook_model_blob(
    source: &SkinnedSourceMesh,
    bounds: &ModelBounds,
    parents: &[Option<usize>],
    joints: &[usize],
    material_color: [u8; 4],
    texture_width: u16,
    texture_height: u16,
    local_to_world_q12: u16,
) -> Result<(Vec<u8>, usize, usize), Error> {
    let mut node_to_joint = vec![None; parents.len()];
    for (joint_index, node_index) in joints.iter().copied().enumerate() {
        node_to_joint[node_index] = Some(joint_index as u16);
    }

    let mut joint_records = Vec::new();
    for node_index in joints.iter().copied() {
        let parent = parents[node_index]
            .and_then(|parent| node_to_joint[parent])
            .unwrap_or(psxed_format::model::NO_JOINT);
        joint_records.push(parent);
    }

    let mut canonical_by_position: BTreeMap<[i16; 3], u16> = BTreeMap::new();
    let mut canonical_vertices: Vec<CookedModelVertex> = Vec::new();
    let mut vertex_face_joints: BTreeMap<u16, BTreeSet<u16>> = BTreeMap::new();
    let mut grouped_faces: BTreeMap<u16, Vec<[CookedFaceCorner; 3]>> = BTreeMap::new();

    for face in &source.faces {
        let mut out_face = [CookedFaceCorner {
            vertex_index: 0,
            uv: (0, 0),
        }; 3];
        for (corner, out_corner) in out_face.iter_mut().enumerate() {
            let vertex = source.vertices[face.indices[corner]];
            let primary_joint = vertex.dominant_joint;
            let record = encode_model_vertex(vertex, primary_joint, bounds);
            let key = model_vertex_position_key(&record);
            let temp_index = if let Some(index) = canonical_by_position.get(&key) {
                *index
            } else {
                let index = ensure_u16("vertices", canonical_vertices.len())?;
                canonical_by_position.insert(key, index);
                canonical_vertices.push(CookedModelVertex {
                    source: vertex,
                    primary_joint,
                    record,
                });
                index
            };
            vertex_face_joints
                .entry(temp_index)
                .or_default()
                .insert(face.joint);
            *out_corner = CookedFaceCorner {
                vertex_index: temp_index,
                uv: (
                    uv_to_u8(vertex.uv[0], texture_width),
                    uv_to_u8(vertex.uv[1], texture_height),
                ),
            };
        }
        grouped_faces.entry(face.joint).or_default().push(out_face);
    }

    let face_joints: BTreeSet<u16> = grouped_faces.keys().copied().collect();
    for (index, vertex) in canonical_vertices.iter_mut().enumerate() {
        if face_joints.contains(&vertex.primary_joint) {
            continue;
        }
        let fallback = vertex_face_joints
            .get(&(index as u16))
            .and_then(|joints| joints.iter().next().copied());
        if let Some(primary_joint) = fallback {
            vertex.primary_joint = primary_joint;
            vertex.record = encode_model_vertex(vertex.source, primary_joint, bounds);
        }
    }

    let mut vertices_by_joint: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
    for (index, vertex) in canonical_vertices.iter().enumerate() {
        vertices_by_joint
            .entry(vertex.primary_joint)
            .or_default()
            .push(ensure_u16("vertices", index)?);
    }

    let part_joints = face_joints;

    let mut temp_to_final = vec![u16::MAX; canonical_vertices.len()];
    let mut vertex_ranges: BTreeMap<u16, (u16, u16)> = BTreeMap::new();
    let mut cooked_vertices: Vec<[u8; psxed_format::model::VERTEX_RECORD_SIZE]> = Vec::new();
    let mut blend_skin = false;

    for joint in &part_joints {
        let first_vertex = ensure_u16("part first vertex", cooked_vertices.len())?;
        if let Some(indices) = vertices_by_joint.get(joint) {
            for temp_index in indices {
                let final_index = ensure_u16("vertices", cooked_vertices.len())?;
                temp_to_final[*temp_index as usize] = final_index;
                let record = canonical_vertices[*temp_index as usize].record;
                if record[7] != 0 {
                    blend_skin = true;
                }
                cooked_vertices.push(record);
            }
        }
        let vertex_count = ensure_u16(
            "part vertices",
            cooked_vertices.len() - first_vertex as usize,
        )?;
        vertex_ranges.insert(*joint, (first_vertex, vertex_count));
    }

    let mut part_records = Vec::new();
    let mut cooked_faces: Vec<[CookedFaceCorner; 3]> = Vec::new();

    for joint in part_joints {
        let (first_vertex, vertex_count) = vertex_ranges.get(&joint).copied().unwrap_or((0, 0));
        let first_face = cooked_faces.len();
        if let Some(faces) = grouped_faces.get(&joint) {
            for face in faces {
                let mut out_face = *face;
                for corner in &mut out_face {
                    let final_index = temp_to_final[corner.vertex_index as usize];
                    if final_index == u16::MAX {
                        return Err(Error::BadSkin("model face references an un-emitted vertex"));
                    }
                    corner.vertex_index = final_index;
                }
                cooked_faces.push(out_face);
            }
        }
        let face_count = cooked_faces.len() - first_face;
        part_records.push((
            joint,
            first_vertex,
            vertex_count,
            ensure_u16("part first face", first_face)?,
            ensure_u16("part faces", face_count)?,
            0u16,
        ));
    }

    ensure_u16("vertices", cooked_vertices.len())?;
    ensure_u16("faces", cooked_faces.len())?;
    ensure_u16("parts", part_records.len())?;

    let payload_len = psxed_format::model::ModelHeader::SIZE
        + joint_records.len() * psxed_format::model::JointRecord::SIZE
        + psxed_format::model::MaterialRecord::SIZE
        + part_records.len() * psxed_format::model::PartRecord::SIZE
        + cooked_vertices.len() * psxed_format::model::VERTEX_RECORD_SIZE
        + cooked_faces.len() * psxed_format::model::FACE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    let mut model_flags =
        psxed_format::model::flags::HAS_UVS | psxed_format::model::flags::RIGID_SKINNED;
    if blend_skin {
        model_flags |= psxed_format::model::flags::BLEND_SKIN;
    }
    append_asset_header(
        &mut out,
        psxed_format::model::MAGIC,
        psxed_format::model::VERSION,
        model_flags,
        payload_len,
    )?;
    append_u16(&mut out, ensure_u16("joints", joint_records.len())?);
    append_u16(&mut out, ensure_u16("parts", part_records.len())?);
    append_u16(&mut out, ensure_u16("vertices", cooked_vertices.len())?);
    append_u16(&mut out, ensure_u16("faces", cooked_faces.len())?);
    append_u16(&mut out, 1);
    append_u16(&mut out, texture_width);
    append_u16(&mut out, texture_height);
    append_u16(&mut out, local_to_world_q12);

    for parent in joint_records {
        append_u16(&mut out, parent);
        append_u16(&mut out, 0);
    }
    append_u16(&mut out, 0);
    append_u16(&mut out, 0);
    out.extend_from_slice(&material_color);

    for (joint, first_vertex, vertex_count, first_face, face_count, material_index) in &part_records
    {
        append_u16(&mut out, *joint);
        append_u16(&mut out, *first_vertex);
        append_u16(&mut out, *vertex_count);
        append_u16(&mut out, *first_face);
        append_u16(&mut out, *face_count);
        append_u16(&mut out, *material_index);
        append_u32(&mut out, 0);
    }
    for vertex in &cooked_vertices {
        out.extend_from_slice(vertex);
    }
    for face in &cooked_faces {
        for corner in face {
            append_u16(&mut out, corner.vertex_index);
            out.push(corner.uv.0);
            out.push(corner.uv.1);
        }
    }

    Ok((out, cooked_vertices.len(), part_records.len()))
}

fn model_vertex_position_key(record: &[u8; psxed_format::model::VERTEX_RECORD_SIZE]) -> [i16; 3] {
    [
        i16::from_le_bytes([record[0], record[1]]),
        i16::from_le_bytes([record[2], record[3]]),
        i16::from_le_bytes([record[4], record[5]]),
    ]
}

fn model_vertex_position_key_for_source(vertex: SourceVertex, bounds: &ModelBounds) -> [i16; 3] {
    let position = bounds.normalize_point(vertex.position);
    [
        q12_i16(position[0]),
        q12_i16(position[1]),
        q12_i16(position[2]),
    ]
}

fn encode_model_vertex(
    vertex: SourceVertex,
    primary_joint: u16,
    bounds: &ModelBounds,
) -> [u8; psxed_format::model::VERTEX_RECORD_SIZE] {
    let position = model_vertex_position_key_for_source(vertex, bounds);
    let (joint1_byte, blend_byte) = blend_slot_for_vertex(vertex, primary_joint);
    let mut out = [0u8; psxed_format::model::VERTEX_RECORD_SIZE];
    write_i16(&mut out, 0, position[0]);
    write_i16(&mut out, 2, position[1]);
    write_i16(&mut out, 4, position[2]);
    out[6] = joint1_byte;
    out[7] = blend_byte;
    out
}

/// Threshold below which a secondary bone's relative weight is dropped.
///
/// `weight1 / (weight0 + weight1)` smaller than this contributes a
/// blend so subtle the runtime CPU path costs more than the visual
/// difference -- better to stay on the single-bone GTE fast path.
const BLEND_DROP_THRESHOLD: f32 = 0.04;

/// Pick the secondary bone + blend byte for a vertex, given the part
/// it ended up in.
///
/// `joint0` is implicit at runtime (it is the part's bone), so we only
/// store `joint1` and a relative weight in 0..=255. VERSION 4 stores
/// each skinned point once under its dominant bone, so the secondary
/// bone is simply the highest remaining influence after
/// `primary_joint`.
fn blend_slot_for_vertex(vertex: SourceVertex, primary_joint: u16) -> (u8, u8) {
    let mut weight0 = 0.0f32;
    let mut weight1 = 0.0f32;
    let mut joint1: Option<u16> = None;

    if vertex.dominant_joint != primary_joint && vertex.weights[0] > 0.0 {
        joint1 = Some(vertex.dominant_joint);
    }

    for i in 0..4 {
        let w = vertex.weights[i].max(0.0);
        if w == 0.0 {
            continue;
        }
        let j = vertex.joints[i];
        if j == primary_joint {
            weight0 += w;
        } else if Some(j) == joint1 {
            weight1 += w;
        } else if joint1.is_none() {
            joint1 = Some(j);
            weight1 = w;
        } else if w > weight1 && Some(j) != joint1 {
            joint1 = Some(j);
            weight1 = w;
        }
    }

    let Some(j1) = joint1 else {
        return (psxed_format::model::NO_JOINT8, 0);
    };
    let total = weight0 + weight1;
    if total <= 0.0 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    let blend = weight1 / total;
    if blend < BLEND_DROP_THRESHOLD {
        return (psxed_format::model::NO_JOINT8, 0);
    }

    let blend_byte = (blend * 255.0).round().clamp(0.0, 255.0) as u8;
    if blend_byte == 0 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    let joint1_byte = if j1 < 255 {
        j1 as u8
    } else {
        psxed_format::model::NO_JOINT8
    };
    if joint1_byte == psxed_format::model::NO_JOINT8 {
        return (psxed_format::model::NO_JOINT8, 0);
    }
    (joint1_byte, blend_byte)
}

fn cook_base_color_texture(
    mesh: &gltf::Mesh<'_>,
    buffers: &[gltf::buffer::Data],
    cfg: &RigidModelConfig,
) -> Result<Option<Vec<u8>>, Error> {
    for primitive in mesh.primitives() {
        let Some(info) = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_texture()
        else {
            continue;
        };
        let source = info.texture().source();
        let Source::View { view, .. } = source.source() else {
            return Err(Error::UnsupportedImageSource);
        };
        let buffer = &buffers[view.buffer().index()].0;
        let start = view.offset();
        let end = start + view.length();
        let bytes = buffer
            .get(start..end)
            .ok_or(Error::UnsupportedImageSource)?;
        let tex_cfg = psxed_tex::Config {
            width: cfg.texture_width,
            height: cfg.texture_height,
            depth: cfg.texture_depth,
            crop: psxed_tex::CropMode::None,
            resampler: psxed_tex::Resampler::Lanczos3,
            transparent_index_zero: true,
        };
        return psxed_tex::convert(bytes, &tex_cfg)
            .map(Some)
            .map_err(Error::TextureCook);
    }
    Ok(None)
}

fn first_material_base_color(mesh: &gltf::Mesh<'_>) -> [u8; 4] {
    if let Some(primitive) = mesh.primitives().next() {
        let color = primitive
            .material()
            .pbr_metallic_roughness()
            .base_color_factor();
        [
            linear_to_u8(color[0]),
            linear_to_u8(color[1]),
            linear_to_u8(color[2]),
            linear_to_u8(color[3]),
        ]
    } else {
        [255, 255, 255, 255]
    }
}

#[allow(clippy::too_many_arguments)]
fn cook_all_animations(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<Vec<CookedClip>, Error> {
    let mut clips = Vec::new();
    for (index, animation) in document.animations().enumerate() {
        let channels = read_animation_channels(&animation, buffers)?;
        if channels.is_empty() {
            continue;
        }
        let Some((min_time, max_time)) = channel_time_range(&channels) else {
            continue;
        };
        let Some(bytes) = cook_animation_bytes(
            &channels,
            parents,
            base_trs,
            root_joint_nodes,
            joints,
            inverse_bind_matrices,
            bounds,
            min_time,
            max_time,
            fps,
            normalize_root_translation,
            strip_animation_scale,
        )?
        else {
            continue;
        };
        let frames = animation_frame_count_from_bytes(&bytes);
        let raw_name = animation.name().map(|s| s.to_string());
        clips.push(CookedClip {
            source_name: raw_name.clone(),
            sanitized_name: sanitize_clip_name(raw_name.as_deref(), index),
            bytes,
            frames,
        });
    }
    let mut clip_index = clips.len();
    for extra in extra_fbx_animation_scenes {
        let mapping = gltf_fbx_animation_node_mapping(document, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let Some(bytes) = cook_fbx_animation_bytes(
                extra.scene,
                &stack.anim,
                parents,
                base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                Some(&mapping),
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = fbx_stack_source_name(stack, extra.fallback_name);
            clips.push(CookedClip {
                source_name: raw_name.clone(),
                sanitized_name: sanitize_clip_name(raw_name.as_deref(), clip_index),
                bytes,
                frames,
            });
            clip_index += 1;
        }
    }
    for extra in extra_gltf_animation_scenes {
        let mapping = gltf_gltf_animation_node_mapping(document, extra.document);
        validate_gltf_animation_mapping(extra.document, &mapping)?;
        let source_parents = build_parent_indices(extra.document);
        let source_base_trs = collect_base_trs(extra.document);
        let copy_full_local_trs = mapped_local_binds_match(base_trs, &source_base_trs, &mapping);
        for animation in extra.document.animations() {
            let channels = read_animation_channels(&animation, extra.buffers)?;
            if channels.is_empty() {
                continue;
            }
            let Some((min_time, max_time)) = channel_time_range(&channels) else {
                continue;
            };
            let Some(bytes) = cook_mapped_gltf_animation_bytes(
                &channels,
                parents,
                base_trs,
                &source_parents,
                &source_base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                &mapping,
                copy_full_local_trs,
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = gltf_animation_source_name(&animation, extra.fallback_name);
            clips.push(CookedClip {
                source_name: raw_name.clone(),
                sanitized_name: sanitize_clip_name(raw_name.as_deref(), clip_index),
                bytes,
                frames,
            });
            clip_index += 1;
        }
    }
    Ok(clips)
}

#[allow(clippy::too_many_arguments)]
fn cook_animation_bytes(
    channels: &[AnimationChannel],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f32,
    max_time: f32,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f32).round() as usize + 1;
    ensure_u16("animation frames", frame_count)?;

    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + frame_count * joints.len() * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    append_asset_header(
        &mut out,
        psxed_format::animation::MAGIC,
        psxed_format::animation::VERSION,
        0,
        payload_len,
    )?;
    append_u16(&mut out, ensure_u16("joints", joints.len())?);
    append_u16(&mut out, ensure_u16("animation frames", frame_count)?);
    append_u16(&mut out, fps);
    append_u16(&mut out, 0);

    for frame in 0..frame_count {
        let time = (min_time + frame as f32 / fps as f32).min(max_time);
        let mut frame_trs = base_trs.to_vec();
        for channel in channels {
            channel.apply(time, &mut frame_trs);
        }
        if normalize_root_translation {
            restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
        }
        let locals: Vec<[[f32; 4]; 4]> = frame_trs.iter().map(|trs| trs.matrix()).collect();
        let globals = compute_global_matrices(parents, &locals);
        for (joint_index, node_index) in joints.iter().copied().enumerate() {
            let mut skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                skin = strip_pose_scale(skin);
            }
            append_pose_record(&mut out, &skin, bounds);
        }
    }

    Ok(Some(out))
}

#[allow(clippy::too_many_arguments)]
fn cook_mapped_gltf_animation_bytes(
    channels: &[AnimationChannel],
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f32,
    max_time: f32,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    mapping: &[Option<usize>],
    copy_full_local_trs: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f32).round() as usize + 1;
    ensure_u16("animation frames", frame_count)?;

    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + frame_count * joints.len() * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    append_asset_header(
        &mut out,
        psxed_format::animation::MAGIC,
        psxed_format::animation::VERSION,
        0,
        payload_len,
    )?;
    append_u16(&mut out, ensure_u16("joints", joints.len())?);
    append_u16(&mut out, ensure_u16("animation frames", frame_count)?);
    append_u16(&mut out, fps);
    append_u16(&mut out, 0);

    for frame in 0..frame_count {
        let time = (min_time + frame as f32 / fps as f32).min(max_time);
        let mut frame_trs = evaluate_mapped_gltf_frame_trs(
            channels,
            time,
            target_parents,
            target_base_trs,
            source_parents,
            source_base_trs,
            mapping,
            copy_full_local_trs,
        );
        if normalize_root_translation {
            restore_root_translations(&mut frame_trs, target_base_trs, root_joint_nodes);
        }
        let locals: Vec<[[f32; 4]; 4]> = frame_trs.iter().map(|trs| trs.matrix()).collect();
        let globals = compute_global_matrices(target_parents, &locals);
        for (joint_index, node_index) in joints.iter().copied().enumerate() {
            let mut skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                skin = strip_pose_scale(skin);
            }
            append_pose_record(&mut out, &skin, bounds);
        }
    }

    Ok(Some(out))
}

fn gltf_animation_source_name(
    animation: &gltf::Animation<'_>,
    fallback_name: Option<&str>,
) -> Option<String> {
    animation
        .name()
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .or_else(|| fallback_name.map(str::to_string))
}

fn sanitize_clip_name(source: Option<&str>, fallback_index: usize) -> String {
    let raw = source.unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        format!("clip{fallback_index}")
    } else {
        trimmed
    }
}

#[derive(Clone, Debug)]
struct AnimationChannel {
    node_index: usize,
    interpolation: Interpolation,
    times: Vec<f32>,
    values: ChannelValues,
}

#[derive(Clone, Debug)]
enum ChannelValues {
    Translation(Vec<[f32; 3]>),
    Rotation(Vec<[f32; 4]>),
    Scale(Vec<[f32; 3]>),
}

impl AnimationChannel {
    fn apply(&self, time: f32, trs: &mut [Trs]) {
        let Some(target) = trs.get_mut(self.node_index) else {
            return;
        };
        match &self.values {
            ChannelValues::Translation(values) => {
                target.translation = sample_vec3(&self.times, values, time, self.interpolation);
            }
            ChannelValues::Rotation(values) => {
                target.rotation = sample_quat(&self.times, values, time, self.interpolation);
            }
            ChannelValues::Scale(values) => {
                target.scale = sample_vec3(&self.times, values, time, self.interpolation);
            }
        }
    }
}

fn restore_root_translations(frame_trs: &mut [Trs], base_trs: &[Trs], root_joint_nodes: &[usize]) {
    for node_index in root_joint_nodes.iter().copied() {
        let Some(frame) = frame_trs.get_mut(node_index) else {
            continue;
        };
        let Some(base) = base_trs.get(node_index) else {
            continue;
        };
        frame.translation = base.translation;
    }
}

fn read_animation_channels(
    animation: &gltf::Animation<'_>,
    buffers: &[gltf::buffer::Data],
) -> Result<Vec<AnimationChannel>, Error> {
    let mut channels = Vec::new();
    for channel in animation.channels() {
        let channel_index = channel.index();
        let interpolation = channel.sampler().interpolation();
        if interpolation == Interpolation::CubicSpline {
            return Err(Error::UnsupportedAnimationInterpolation {
                channel_index,
                interpolation,
            });
        }
        let reader = channel.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let times: Vec<f32> = reader
            .read_inputs()
            .ok_or(Error::MissingAnimationInputs { channel_index })?
            .collect();
        let outputs = reader
            .read_outputs()
            .ok_or(Error::MissingAnimationOutputs { channel_index })?;
        let values = match (channel.target().property(), outputs) {
            (Property::Translation, gltf::animation::util::ReadOutputs::Translations(values)) => {
                ChannelValues::Translation(values.collect())
            }
            (Property::Rotation, gltf::animation::util::ReadOutputs::Rotations(values)) => {
                ChannelValues::Rotation(values.into_f32().collect())
            }
            (Property::Scale, gltf::animation::util::ReadOutputs::Scales(values)) => {
                ChannelValues::Scale(values.collect())
            }
            (Property::MorphTargetWeights, _) => continue,
            _ => return Err(Error::AnimationTypeMismatch { channel_index }),
        };
        channels.push(AnimationChannel {
            node_index: channel.target().node().index(),
            interpolation,
            times,
            values,
        });
    }
    Ok(channels)
}

fn sample_vec3(
    times: &[f32],
    values: &[[f32; 3]],
    time: f32,
    interpolation: Interpolation,
) -> [f32; 3] {
    let (a, b, t) = sample_segment(times, time);
    if interpolation == Interpolation::Step || a == b {
        return values[a];
    }
    lerp3(values[a], values[b], t)
}

fn sample_quat(
    times: &[f32],
    values: &[[f32; 4]],
    time: f32,
    interpolation: Interpolation,
) -> [f32; 4] {
    let (a, b, t) = sample_segment(times, time);
    if interpolation == Interpolation::Step || a == b {
        return normalize4(values[a]);
    }
    nlerp_quat(values[a], values[b], t)
}

fn sample_segment(times: &[f32], time: f32) -> (usize, usize, f32) {
    if times.len() <= 1 || time <= times[0] {
        return (0, 0, 0.0);
    }
    let last = times.len() - 1;
    if time >= times[last] {
        return (last, last, 0.0);
    }
    for index in 0..last {
        let t0 = times[index];
        let t1 = times[index + 1];
        if time >= t0 && time <= t1 {
            let span = (t1 - t0).max(0.000001);
            return (index, index + 1, ((time - t0) / span).clamp(0.0, 1.0));
        }
    }
    (last, last, 0.0)
}

fn compose_trs(translation: [f32; 3], rotation: [f32; 4], scale: [f32; 3]) -> [[f32; 4]; 4] {
    let [x, y, z, w] = normalize4(rotation);
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;

    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r02 = 2.0 * (xz + wy);
    let r10 = 2.0 * (xy + wz);
    let r11 = 1.0 - 2.0 * (xx + zz);
    let r12 = 2.0 * (yz - wx);
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);

    [
        [r00 * scale[0], r10 * scale[0], r20 * scale[0], 0.0],
        [r01 * scale[1], r11 * scale[1], r21 * scale[1], 0.0],
        [r02 * scale[2], r12 * scale[2], r22 * scale[2], 0.0],
        [translation[0], translation[1], translation[2], 1.0],
    ]
}

fn strip_pose_scale(mut matrix: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    for column in matrix.iter_mut().take(3) {
        let len_sq = column[0] * column[0] + column[1] * column[1] + column[2] * column[2];
        if len_sq > 0.000001 {
            let inv_len = len_sq.sqrt().recip();
            column[0] *= inv_len;
            column[1] *= inv_len;
            column[2] *= inv_len;
        }
    }
    matrix
}

fn append_pose_record(out: &mut Vec<u8>, skin_matrix: &[[f32; 4]; 4], bounds: &ModelBounds) {
    for column in skin_matrix.iter().take(3) {
        for value in column.iter().take(3) {
            append_i16(out, q12_i16(*value));
        }
    }
    let center_in_pose = transform_point(skin_matrix, bounds.center);
    let translation = [
        (center_in_pose[0] - bounds.center[0]) / bounds.extent,
        (center_in_pose[1] - bounds.center[1]) / bounds.extent,
        (center_in_pose[2] - bounds.center[2]) / bounds.extent,
    ];
    append_i32(out, q12_i32(translation[0]));
    append_i32(out, q12_i32(translation[1]));
    append_i32(out, q12_i32(translation[2]));
}

fn animation_frame_count_from_bytes(bytes: &[u8]) -> usize {
    if bytes.len()
        < psxed_format::AssetHeader::SIZE + psxed_format::animation::AnimationHeader::SIZE
    {
        return 0;
    }
    u16::from_le_bytes([bytes[14], bytes[15]]) as usize
}

fn append_asset_header(
    out: &mut Vec<u8>,
    magic: [u8; 4],
    version: u16,
    flags: u16,
    payload_len: usize,
) -> Result<(), Error> {
    if payload_len > u32::MAX as usize {
        return Err(Error::TooMany {
            kind: "payload bytes",
            count: payload_len,
            max: u32::MAX as usize,
        });
    }
    out.extend_from_slice(&magic);
    append_u16(out, version);
    append_u16(out, flags);
    append_u32(out, payload_len as u32);
    Ok(())
}

fn append_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_i16(out: &mut [u8], offset: usize, value: i16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn ensure_u16(kind: &'static str, count: usize) -> Result<u16, Error> {
    if count > u16::MAX as usize {
        Err(Error::TooMany {
            kind,
            count,
            max: u16::MAX as usize,
        })
    } else {
        Ok(count as u16)
    }
}

fn q12_i16(value: f32) -> i16 {
    (value * 4096.0)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn q12_i32(value: f32) -> i32 {
    (value * 4096.0)
        .round()
        .clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

fn uv_to_u8(value: f32, size: u16) -> u8 {
    let max_coord = size.saturating_sub(1).min(255) as f32;
    (value.clamp(0.0, 1.0) * max_coord)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length_sq3(v: [f32; 3]) -> f32 {
    v[0] * v[0] + v[1] * v[1] + v[2] * v[2]
}

fn vec3_close(a: [f32; 3], b: [f32; 3], epsilon: f32) -> bool {
    (a[0] - b[0]).abs() <= epsilon
        && (a[1] - b[1]).abs() <= epsilon
        && (a[2] - b[2]).abs() <= epsilon
}

fn quat_close_same_orientation(a: [f32; 4], b: [f32; 4], min_abs_dot: f32) -> bool {
    let a = normalize4(a);
    let b = normalize4(b);
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    dot.abs() >= min_abs_dot
}

fn nlerp_quat(mut a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        a = [-a[0], -a[1], -a[2], -a[3]];
    }
    normalize4([
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ])
}

fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = normalize4(a);
    let [bx, by, bz, bw] = normalize4(b);
    normalize4([
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ])
}

fn quat_inverse(q: [f32; 4]) -> [f32; 4] {
    let [x, y, z, w] = normalize4(q);
    [-x, -y, -z, w]
}

const fn identity_quat() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= 0.000001 {
        [0.0, 1.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn normalize4(v: [f32; 4]) -> [f32; 4] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
    if len <= 0.000001 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len, v[3] / len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_quat_close(actual: [f32; 4], expected: [f32; 4]) {
        let direct = actual
            .iter()
            .zip(expected)
            .map(|(a, e)| (a - e).abs())
            .fold(0.0f32, f32::max);
        let flipped = actual
            .iter()
            .zip(expected)
            .map(|(a, e)| (a + e).abs())
            .fold(0.0f32, f32::max);
        assert!(
            direct.min(flipped) < 0.0001,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn quat_z_degrees(degrees: f32) -> [f32; 4] {
        let radians = degrees.to_radians() * 0.5;
        [0.0, 0.0, radians.sin(), radians.cos()]
    }

    fn quat_x_degrees(degrees: f32) -> [f32; 4] {
        let radians = degrees.to_radians() * 0.5;
        [radians.sin(), 0.0, 0.0, radians.cos()]
    }

    fn quat_y_degrees(degrees: f32) -> [f32; 4] {
        let radians = degrees.to_radians() * 0.5;
        [0.0, radians.sin(), 0.0, radians.cos()]
    }

    #[test]
    fn humanoid_node_match_key_aliases_synty_and_meshy_bones() {
        assert_eq!(node_match_key("LeftUpLeg"), node_match_key("UpperLeg_L"));
        assert_eq!(node_match_key("LeftUpLeg"), node_match_key("thigh_l"));
        assert_eq!(node_match_key("LeftLeg"), node_match_key("LowerLeg_L"));
        assert_eq!(node_match_key("LeftLeg"), node_match_key("calf_l"));
        assert_eq!(node_match_key("LeftFoot"), node_match_key("Ankle_L"));
        assert_eq!(node_match_key("LeftFoot"), node_match_key("foot_l"));
        assert_eq!(node_match_key("LeftToeBase"), node_match_key("ball_l"));
        assert_eq!(node_match_key("LeftShoulder"), node_match_key("clavicle_l"));
        assert_eq!(node_match_key("LeftArm"), node_match_key("Shoulder_L"));
        assert_eq!(node_match_key("LeftArm"), node_match_key("upperarm_l"));
        assert_eq!(node_match_key("LeftForeArm"), node_match_key("Elbow_L"));
        assert_eq!(node_match_key("LeftForeArm"), node_match_key("lowerarm_l"));
        assert_eq!(node_match_key("LeftHand"), node_match_key("Hand_L"));
        assert_eq!(node_match_key("Neck"), node_match_key("neck_01"));
        assert_eq!(node_match_key("Spine"), node_match_key("Spine_01"));
        assert_eq!(node_match_key("Spine01"), node_match_key("Spine_02"));
        assert_eq!(node_match_key("Spine02"), node_match_key("Spine_03"));
        assert_ne!(node_match_key("Armature"), node_match_key("Root"));
    }

    #[test]
    fn fbx_companion_texture_search_finds_meshy_obj_export_sibling() {
        let root = std::env::temp_dir().join(format!(
            "psxed-gltf-fbx-texture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let pack = root.join("Sword and Shield Pack");
        let sibling = root.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture_obj");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let source = pack.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture.fbx");
        let texture = sibling.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture.png");
        std::fs::write(&source, b"fbx").unwrap();
        std::fs::write(&texture, b"png").unwrap();

        assert_eq!(find_companion_fbx_texture(&source), Some(texture));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retarget_mapped_frame_trs_keeps_target_offsets_and_child_inheritance() {
        let target_parents = vec![None, Some(0)];
        let source_parents = vec![None, Some(0)];
        let target_base = vec![
            Trs {
                translation: [1.0, 2.0, 3.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [4.0, 5.0, 6.0],
                rotation: quat_z_degrees(10.0),
                scale: [1.0, 1.0, 1.0],
            },
        ];
        let source_base = vec![
            Trs {
                translation: [100.0, 200.0, 300.0],
                rotation: identity_quat(),
                scale: [2.0, 2.0, 2.0],
            },
            Trs {
                translation: [400.0, 500.0, 600.0],
                rotation: identity_quat(),
                scale: [3.0, 3.0, 3.0],
            },
        ];
        let source_pose = vec![
            Trs {
                translation: [120.0, 240.0, 360.0],
                rotation: quat_z_degrees(45.0),
                scale: [2.0, 2.0, 2.0],
            },
            Trs {
                translation: [420.0, 540.0, 660.0],
                rotation: quat_z_degrees(20.0),
                scale: [3.0, 3.0, 3.0],
            },
        ];
        let mapping = vec![Some(0), None];

        let retargeted = retarget_mapped_frame_trs(
            &target_parents,
            &target_base,
            &source_parents,
            &source_base,
            &source_pose,
            &mapping,
        );

        assert_eq!(retargeted[0].translation, target_base[0].translation);
        assert_eq!(retargeted[1].translation, target_base[1].translation);
        assert_eq!(retargeted[0].scale, target_base[0].scale);
        assert_eq!(retargeted[1].scale, target_base[1].scale);
        assert_quat_close(retargeted[0].rotation, quat_z_degrees(45.0));
        assert_quat_close(retargeted[1].rotation, target_base[1].rotation);
    }

    #[test]
    fn retarget_mapped_frame_trs_applies_delta_in_source_rest_basis() {
        let target_parents = vec![None];
        let source_parents = vec![None];
        let source_base_rotation = quat_x_degrees(90.0);
        let target_base_rotation = quat_y_degrees(90.0);
        let source_local_delta = quat_z_degrees(35.0);
        let target_base = vec![Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: target_base_rotation,
            scale: [1.0, 1.0, 1.0],
        }];
        let source_base = vec![Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: source_base_rotation,
            scale: [1.0, 1.0, 1.0],
        }];
        let source_pose = vec![Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: quat_mul(source_base_rotation, source_local_delta),
            scale: [1.0, 1.0, 1.0],
        }];
        let mapping = vec![Some(0)];

        let retargeted = retarget_mapped_frame_trs(
            &target_parents,
            &target_base,
            &source_parents,
            &source_base,
            &source_pose,
            &mapping,
        );

        assert_quat_close(
            retargeted[0].rotation,
            quat_mul(target_base_rotation, source_local_delta),
        );
    }

    #[test]
    fn imports_minimal_glb_triangle() {
        let glb = minimal_triangle_glb();
        let psxm = convert_slice(&glb, &Config::default()).unwrap();
        let mesh = psx_asset::Mesh::from_bytes(&psxm).unwrap();
        assert_eq!(mesh.vert_count(), 3);
        assert_eq!(mesh.face_count(), 1);
        assert!(mesh.has_face_colors());
        assert!(mesh.has_normals());
        assert_eq!(mesh.face_color(0), Some((64, 128, 255)));
    }

    #[test]
    fn triangle_strip_gets_triangulated() {
        let faces = triangulate_indices(&[0, 1, 2, 3], Mode::TriangleStrip).unwrap();
        assert_eq!(faces, vec![[0, 1, 2], [2, 1, 3]]);
    }

    #[test]
    fn model_precision_scale_targets_world_height() {
        let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], 30_000.0).unwrap();
        let local_height = bounds.encoded_axis_size(0.0, 4.0);
        assert_eq!(local_height, 60_000);
        assert_eq!(choose_local_to_world_q12(local_height, 1024), 70);
    }

    #[test]
    fn native_model_normals_use_source_winding_after_engine_face_flip() {
        let mut source = SkinnedSourceMesh {
            vertices: vec![
                test_source_vertex([0.0, 0.0, 0.0]),
                test_source_vertex([1.0, 0.0, 0.0]),
                test_source_vertex([0.0, 1.0, 0.0]),
            ],
            faces: vec![SourceFace {
                indices: [0, 2, 1],
                joint: 0,
            }],
        };

        rebuild_source_normals(&mut source, &[[0, 1, 2]]);

        assert_eq!(source.faces[0].indices, [0, 2, 1]);
        for vertex in &source.vertices {
            assert!((vertex.normal[0] - 0.0).abs() < 0.0001);
            assert!((vertex.normal[1] - 0.0).abs() < 0.0001);
            assert!((vertex.normal[2] - 1.0).abs() < 0.0001);
        }
    }

    #[test]
    fn native_model_compacts_duplicate_part_vertices() {
        let source = SkinnedSourceMesh {
            vertices: vec![
                test_source_vertex([0.0, 0.0, 0.0]),
                test_source_vertex([1.0, 0.0, 0.0]),
                test_source_vertex([0.0, 1.0, 0.0]),
            ],
            faces: vec![
                SourceFace {
                    indices: [0, 1, 2],
                    joint: 0,
                },
                SourceFace {
                    indices: [0, 2, 1],
                    joint: 1,
                },
            ],
        };
        let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], 30_000.0).unwrap();

        let (bytes, vertices, parts) = cook_model_blob(
            &source,
            &bounds,
            &[None, None],
            &[0, 1],
            [255, 255, 255, 255],
            128,
            128,
            psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12,
        )
        .unwrap();

        let model = psx_asset::Model::from_bytes(&bytes).unwrap();
        assert_eq!(vertices, 3);
        assert_eq!(parts, 2);
        assert_eq!(model.part(0).unwrap().vertex_count(), 3);
        assert_eq!(model.part(1).unwrap().vertex_count(), 0);
        assert_eq!(model.face(0).unwrap().corners[0].vertex_index, 0);
        assert_eq!(model.face(1).unwrap().corners[0].vertex_index, 0);
        assert_eq!(model.face(1).unwrap().corners[1].vertex_index, 2);
        assert_eq!(model.face(1).unwrap().corners[2].vertex_index, 1);
    }

    #[test]
    fn native_model_prunes_small_detached_cooked_position_islands() {
        let mut source = SkinnedSourceMesh {
            vertices: vec![
                test_source_vertex([0.0, 0.0, 0.0]),
                test_source_vertex([1.0, 0.0, 0.0]),
                test_source_vertex([1.0, 1.0, 0.0]),
                test_source_vertex([0.0, 1.0, 0.0]),
                test_source_vertex([4.0, 0.0, 0.0]),
                test_source_vertex([4.5, 0.0, 0.0]),
                test_source_vertex([4.0, 0.5, 0.0]),
            ],
            faces: vec![
                SourceFace {
                    indices: [0, 1, 2],
                    joint: 0,
                },
                SourceFace {
                    indices: [0, 2, 3],
                    joint: 0,
                },
                SourceFace {
                    indices: [4, 5, 6],
                    joint: 0,
                },
            ],
        };
        let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [4.5, 1.0, 0.0], 30_000.0).unwrap();

        let removed = prune_detached_face_islands(&mut source, &bounds, 1);

        assert_eq!(removed, 1);
        assert_eq!(
            source
                .faces
                .iter()
                .map(|face| face.indices)
                .collect::<Vec<_>>(),
            vec![[0, 1, 2], [0, 2, 3]]
        );
    }

    #[test]
    fn native_model_reassigns_vertex_only_joints_to_face_part() {
        let mut foreign_joint_vertex = test_source_vertex([0.0, 0.0, 0.0]);
        foreign_joint_vertex.joints = [1, 0, 0, 0];
        foreign_joint_vertex.dominant_joint = 1;
        let source = SkinnedSourceMesh {
            vertices: vec![
                foreign_joint_vertex,
                SourceVertex {
                    position: [1.0, 0.0, 0.0],
                    ..foreign_joint_vertex
                },
                SourceVertex {
                    position: [0.0, 1.0, 0.0],
                    ..foreign_joint_vertex
                },
            ],
            faces: vec![SourceFace {
                indices: [0, 1, 2],
                joint: 0,
            }],
        };
        let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], 30_000.0).unwrap();

        let (bytes, vertices, parts) = cook_model_blob(
            &source,
            &bounds,
            &[None, None],
            &[0, 1],
            [255, 255, 255, 255],
            128,
            128,
            psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12,
        )
        .unwrap();

        let model = psx_asset::Model::from_bytes(&bytes).unwrap();
        assert_eq!(vertices, 3);
        assert_eq!(parts, 1);
        assert_eq!(model.part_count(), 1);
        assert_eq!(model.part(0).unwrap().joint_index(), 0);
        assert_eq!(model.part(0).unwrap().vertex_count(), 3);
        assert_eq!(model.part(0).unwrap().face_count(), 1);
    }

    #[test]
    fn root_translation_normalization_restores_bind_pose_translation() {
        let base = vec![
            Trs {
                translation: [0.25, 1.0, -0.5],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [2.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0, 1.0, 1.0],
            },
        ];
        let mut frame = base.clone();
        frame[0].translation = [-4.0, 3.0, 9.0];
        frame[1].translation = [8.0, 1.0, 2.0];

        restore_root_translations(&mut frame, &base, &[0]);

        assert_eq!(frame[0].translation, base[0].translation);
        assert_eq!(frame[1].translation, [8.0, 1.0, 2.0]);
    }

    #[test]
    fn cooked_animation_pose_scale_is_stripped_when_enabled() {
        let channels = vec![AnimationChannel {
            node_index: 0,
            interpolation: Interpolation::Linear,
            times: vec![0.0, 1.0],
            values: ChannelValues::Scale(vec![[2.0, 2.0, 2.0], [2.0, 2.0, 2.0]]),
        }];
        let parents = [None];
        let base_trs = [Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        }];
        let joints = [0usize];
        let inverse_bind_matrices = [identity_matrix()];
        let bounds =
            ModelBounds::from_min_max([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0], 30_000.0).unwrap();

        let stripped = cook_animation_bytes(
            &channels,
            &parents,
            &base_trs,
            &joints,
            &joints,
            &inverse_bind_matrices,
            &bounds,
            0.0,
            1.0,
            1,
            false,
            true,
        )
        .unwrap()
        .unwrap();
        let kept = cook_animation_bytes(
            &channels,
            &parents,
            &base_trs,
            &joints,
            &joints,
            &inverse_bind_matrices,
            &bounds,
            0.0,
            1.0,
            1,
            false,
            false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(first_pose_matrix_component(&stripped, 0), 4096);
        assert_eq!(first_pose_matrix_component(&stripped, 4), 4096);
        assert_eq!(first_pose_matrix_component(&stripped, 8), 4096);
        assert_eq!(first_pose_matrix_component(&kept, 0), 8192);
    }

    #[test]
    fn mapped_gltf_same_bind_preserves_local_translation_keys() {
        let target_parents = [None, Some(0), Some(1), Some(2), Some(3), Some(4)];
        let source_parents = [None, Some(0), Some(1), Some(2), Some(3), Some(4)];
        let base = [
            Trs {
                translation: [0.0, 0.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [0.0, 10.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [0.0, 20.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [0.0, 30.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [0.0, 40.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
            Trs {
                translation: [0.0, 50.0, 0.0],
                rotation: identity_quat(),
                scale: [1.0, 1.0, 1.0],
            },
        ];
        let mapping = [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];
        let channels = [AnimationChannel {
            node_index: 1,
            interpolation: Interpolation::Linear,
            times: vec![0.0, 1.0],
            values: ChannelValues::Translation(vec![[0.0, 10.0, 0.0], [0.0, 24.0, 0.0]]),
        }];

        assert!(mapped_local_binds_match(&base, &base, &mapping));
        let copied = evaluate_mapped_gltf_frame_trs(
            &channels,
            1.0,
            &target_parents,
            &base,
            &source_parents,
            &base,
            &mapping,
            true,
        );
        assert_eq!(copied[1].translation, [0.0, 24.0, 0.0]);

        let retargeted = evaluate_mapped_gltf_frame_trs(
            &channels,
            1.0,
            &target_parents,
            &base,
            &source_parents,
            &base,
            &mapping,
            false,
        );
        assert_eq!(retargeted[1].translation, [0.0, 10.0, 0.0]);
    }

    #[test]
    fn pose_record_round_trips_encoded_model_space() {
        let bounds =
            ModelBounds::from_min_max([-2.0, -3.0, -4.0], [4.0, 5.0, 6.0], 30_000.0).unwrap();
        let skin = compose_trs([3.0, -2.0, 1.0], quat_z_degrees(90.0), [1.0, 1.0, 1.0]);
        let payload_len = psxed_format::animation::AnimationHeader::SIZE
            + psxed_format::animation::POSE_RECORD_SIZE;
        let mut bytes = Vec::new();
        append_asset_header(
            &mut bytes,
            psxed_format::animation::MAGIC,
            psxed_format::animation::VERSION,
            0,
            payload_len,
        )
        .unwrap();
        append_u16(&mut bytes, 1);
        append_u16(&mut bytes, 1);
        append_u16(&mut bytes, 15);
        append_u16(&mut bytes, 0);
        append_pose_record(&mut bytes, &skin, &bounds);

        let animation = psx_asset::Animation::from_bytes(&bytes).unwrap();
        let pose = animation.pose(0, 0).unwrap();
        let source = [1.0, 2.0, 3.0];
        let encoded = bounds.normalize_point(source).map(q12_i32);
        let actual = [
            (((pose.matrix[0][0] as i32) * encoded[0]
                + (pose.matrix[1][0] as i32) * encoded[1]
                + (pose.matrix[2][0] as i32) * encoded[2])
                >> 12)
                + pose.translation.x,
            (((pose.matrix[0][1] as i32) * encoded[0]
                + (pose.matrix[1][1] as i32) * encoded[1]
                + (pose.matrix[2][1] as i32) * encoded[2])
                >> 12)
                + pose.translation.y,
            (((pose.matrix[0][2] as i32) * encoded[0]
                + (pose.matrix[1][2] as i32) * encoded[1]
                + (pose.matrix[2][2] as i32) * encoded[2])
                >> 12)
                + pose.translation.z,
        ];
        let expected = bounds
            .normalize_point(transform_point(&skin, source))
            .map(q12_i32);
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() <= 2,
                "axis {axis}: actual {} expected {}",
                actual[axis],
                expected[axis]
            );
        }
    }

    #[test]
    fn root_joint_nodes_skips_children_of_other_skin_joints() {
        let parents = vec![None, Some(0), Some(1), Some(0)];
        assert_eq!(root_joint_nodes(&[1, 2, 3], &parents), vec![1, 3]);
    }

    fn minimal_triangle_glb() -> Vec<u8> {
        let mut bin = Vec::new();
        for f in [
            0.0f32, 0.0, 0.0, //
            1.0, 0.0, 0.0, //
            0.0, 1.0, 0.0,
        ] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
        for i in [0u16, 1, 2] {
            bin.extend_from_slice(&i.to_le_bytes());
        }

        let json = format!(
            r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"mesh": 0}}],
  "buffers": [{{"byteLength": {}}}],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 36, "target": 34962}},
    {{"buffer": 0, "byteOffset": 36, "byteLength": 6, "target": 34963}}
  ],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
     "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}},
    {{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}}
  ],
  "materials": [
    {{"pbrMetallicRoughness": {{"baseColorFactor": [0.25, 0.5, 1.0, 1.0]}}}}
  ],
  "meshes": [
    {{"primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1, "material": 0, "mode": 4}}]}}
  ]
}}"#,
            bin.len()
        );
        let json = padded(json.into_bytes(), b' ');
        let bin = padded(bin, 0);

        let total_len = 12 + 8 + json.len() + 8 + bin.len();
        let mut out = Vec::with_capacity(total_len);
        out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total_len as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // JSON
        out.extend_from_slice(&json);
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // BIN
        out.extend_from_slice(&bin);
        out
    }

    fn padded(mut bytes: Vec<u8>, pad: u8) -> Vec<u8> {
        while !bytes.len().is_multiple_of(4) {
            bytes.push(pad);
        }
        bytes
    }

    fn first_pose_matrix_component(bytes: &[u8], component: usize) -> i16 {
        let offset = psxed_format::AssetHeader::SIZE
            + psxed_format::animation::AnimationHeader::SIZE
            + component * 2;
        i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn test_source_vertex(position: [f32; 3]) -> SourceVertex {
        SourceVertex {
            position,
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            dominant_joint: 0,
        }
    }
}
