//! Cooked-model bundle registration + source model import.
//!
//! Two entry points feed the same end product --
//! [`ResourceData::Model`] -- from different sources:
//!
//! * [`register_cooked_model_bundle`] adopts an existing
//!   `bundle_dir/` containing one cooked `.psxmdl`, optionally a
//!   `.psxt` atlas, and any number of `.psxanim` clips. Use this
//!   when the assets ship with the repo or were cooked elsewhere.
//!
//! * [`import_glb_model`] runs GLB/glTF/FBX sources through the
//!   rigid-model cooker, drops the
//!   cooked outputs under `project/assets/models/<safe_name>/`,
//!   then registers them. Use this for fresh authoring.
//!
//! Both paths validate every blob through `psx_asset` parsers and
//! confirm animation joint counts match the model's joint count
//! before creating the resource -- bad bundles never produce a
//! half-broken Model entry.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::{
    default_model_collision_radius_for_height, AnimationClipBakeKind, AnimationClipResource,
    AnimationRole, AnimationSetResource, AnimationSourceResource, CharacterAnimationAction,
    ModelAnimationClip, ModelResource, ProjectDocument, ResourceData, ResourceId, SkeletonResource,
};

pub use psxed_format::texture::Depth as TextureDepth;
pub use psxed_gltf::{RigidModelConfig, RigidModelPackage, RigidModelReport};

/// Header-derived statistics about a `.psxmdl` blob, suitable
/// for editor inspector display. Computed by walking the model
/// vertex table once for bounds; everything else is a header
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStats {
    /// On-disk byte count.
    pub model_bytes: usize,
    /// Joint count from the model header.
    pub joint_count: u16,
    /// Part / submesh count.
    pub part_count: u16,
    /// Vertex count.
    pub vertex_count: u16,
    /// Face (triangle) count.
    pub face_count: u16,
    /// Material slots referenced by parts.
    pub material_count: u16,
    /// Local-to-world Q12 scale stored in the header.
    pub local_to_world_q12: u16,
    /// Largest vertex count across all parts (sizes the
    /// runtime's per-part scratch buffer).
    pub max_part_vertices: u16,
    /// Texture footprint declared by the header (used to size
    /// the atlas allocator).
    pub texture_width: u16,
    /// Header-declared texture height.
    pub texture_height: u16,
    /// AABB minimum from the parsed vertex positions, in
    /// model-local units. Defaults to `[0, 0, 0]` when the
    /// model has no vertices.
    pub bounds_min: [i32; 3],
    /// AABB maximum.
    pub bounds_max: [i32; 3],
}

/// Per-clip metadata. `valid_for_model` is `true` when the
/// clip's joint count matches the model it's bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationClipStats {
    /// Display name.
    pub name: String,
    /// On-disk byte count.
    pub bytes: usize,
    /// Joint count from the clip header.
    pub joint_count: u16,
    /// Frame count from the clip header.
    pub frame_count: u16,
    /// Sample rate in Hz from the clip header.
    pub sample_rate_hz: u16,
    /// `false` when the clip's joint count differs from the
    /// owning model -- the inspector flags this and the cooker
    /// refuses such bundles.
    pub valid_for_model: bool,
}

/// Atlas texture stats. `depth` is `4`, `8`, or `15`; anything
/// else means the editor doesn't fully understand the depth
/// (the inspector flags accordingly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTextureStats {
    /// On-disk byte count.
    pub bytes: usize,
    /// Texel width.
    pub width: u16,
    /// Texel height.
    pub height: u16,
    /// Bits per pixel -- `4`, `8`, or `15`.
    pub depth: u8,
    /// CLUT entry count (`16` for 4bpp, `256` for 8bpp, `0`
    /// for direct 15bpp).
    pub clut_entries: u16,
}

/// Compute [`ModelStats`] from cooked `.psxmdl` bytes. Walks
/// every vertex once for the AABB; cheap relative to the rest
/// of the editor's per-frame budget.
pub fn model_stats_from_bytes(bytes: &[u8]) -> Result<ModelStats, ModelImportError> {
    let model =
        psx_asset::Model::from_bytes(bytes).map_err(|e| ModelImportError::InvalidModel {
            path: PathBuf::new(),
            detail: format!("{:?}", e),
        })?;

    let mut max_part_vertices: u16 = 0;
    for i in 0..model.part_count() {
        if let Some(part) = model.part(i) {
            if part.vertex_count() > max_part_vertices {
                max_part_vertices = part.vertex_count();
            }
        }
    }

    let mut bounds_min = [i32::MAX, i32::MAX, i32::MAX];
    let mut bounds_max = [i32::MIN, i32::MIN, i32::MIN];
    let mut any = false;
    for i in 0..model.vertex_count() {
        if let Some(v) = model.vertex(i) {
            let p = v.position;
            let xyz = [p.x as i32, p.y as i32, p.z as i32];
            for (axis, value) in xyz.iter().enumerate() {
                if *value < bounds_min[axis] {
                    bounds_min[axis] = *value;
                }
                if *value > bounds_max[axis] {
                    bounds_max[axis] = *value;
                }
            }
            any = true;
        }
    }
    if !any {
        bounds_min = [0, 0, 0];
        bounds_max = [0, 0, 0];
    }

    Ok(ModelStats {
        model_bytes: bytes.len(),
        joint_count: model.joint_count(),
        part_count: model.part_count(),
        vertex_count: model.vertex_count(),
        face_count: model.face_count(),
        material_count: model.material_count(),
        local_to_world_q12: model.local_to_world_q12(),
        max_part_vertices,
        texture_width: model.texture_width(),
        texture_height: model.texture_height(),
        bounds_min,
        bounds_max,
    })
}

/// Compute [`AnimationClipStats`] from `.psxanim` bytes plus
/// the owning model's joint count. `name` is supplied by the
/// caller (usually copied from the [`ModelAnimationClip`]).
pub fn animation_stats_from_bytes(
    name: impl Into<String>,
    bytes: &[u8],
    model_joint_count: u16,
) -> Result<AnimationClipStats, ModelImportError> {
    let anim = psx_asset::Animation::from_bytes(bytes).map_err(|e| {
        ModelImportError::InvalidAnimation {
            path: PathBuf::new(),
            detail: format!("{:?}", e),
        }
    })?;
    Ok(AnimationClipStats {
        name: name.into(),
        bytes: bytes.len(),
        joint_count: anim.joint_count(),
        frame_count: anim.frame_count(),
        sample_rate_hz: anim.sample_rate_hz(),
        valid_for_model: anim.joint_count() == model_joint_count,
    })
}

/// Compute [`ModelTextureStats`] from `.psxt` bytes.
pub fn texture_stats_from_bytes(bytes: &[u8]) -> Result<ModelTextureStats, ModelImportError> {
    let texture =
        psx_asset::Texture::from_bytes(bytes).map_err(|e| ModelImportError::InvalidTexture {
            path: PathBuf::new(),
            detail: format!("{:?}", e),
        })?;
    let depth = match texture.depth() {
        psxed_format::texture::Depth::Bit4 => 4,
        psxed_format::texture::Depth::Bit8 => 8,
        psxed_format::texture::Depth::Bit15 => 15,
    };
    Ok(ModelTextureStats {
        bytes: bytes.len(),
        width: texture.width(),
        height: texture.height(),
        depth,
        clut_entries: texture.clut_entries(),
    })
}

/// Resolve a `psxt`/`psxanim`/`psxmdl` path against an optional
/// project root. Mirrors the lookup order used by other resources
/// (absolute → project-relative).
pub fn resolve_path(stored: &str, project_root: Option<&Path>) -> PathBuf {
    if Path::new(stored).is_absolute() {
        PathBuf::from(stored)
    } else if let Some(root) = project_root {
        root.join(stored)
    } else {
        PathBuf::from(stored)
    }
}

/// Failure modes for [`register_cooked_model_bundle`] and
/// [`import_glb_model`]. Each variant carries the offending path
/// or detail so the editor's status line can point at the cause
/// without re-walking the bundle.
#[derive(Debug)]
pub enum ModelImportError {
    /// `bundle_dir` is not a directory or could not be read.
    BundleNotADirectory(PathBuf),
    /// `bundle_dir` contains zero `.psxmdl` files.
    NoModelFile(PathBuf),
    /// `bundle_dir` contains more than one `.psxmdl`.
    MultipleModelFiles {
        /// Each candidate `.psxmdl` path discovered.
        paths: Vec<PathBuf>,
    },
    /// More than one `.psxt` was found in the bundle directory.
    /// The current schema binds a model to exactly one atlas, so
    /// the registrar rejects ambiguous bundles rather than
    /// guessing.
    MultipleTextureFiles {
        /// Each candidate `.psxt` path discovered.
        paths: Vec<PathBuf>,
    },
    /// `psx_asset::Model::from_bytes` rejected the model bytes.
    InvalidModel {
        /// Path that failed to parse.
        path: PathBuf,
        /// Diagnostic message (parse error rendered as a string).
        detail: String,
    },
    /// `psx_asset::Texture::from_bytes` rejected the atlas bytes.
    InvalidTexture {
        /// Path that failed to parse.
        path: PathBuf,
        /// Diagnostic message.
        detail: String,
    },
    /// `psx_asset::Animation::from_bytes` rejected an animation
    /// blob.
    InvalidAnimation {
        /// Clip path that failed.
        path: PathBuf,
        /// Diagnostic message.
        detail: String,
    },
    /// An animation's joint count differs from the model's joint
    /// count -- they would render scrambled frames at runtime.
    JointCountMismatch {
        /// Clip path.
        path: PathBuf,
        /// Joints declared by the clip header.
        animation_joints: u16,
        /// Joints declared by the model header.
        model_joints: u16,
    },
    /// Filesystem error reading or writing a bundle file.
    Io {
        /// Path the IO error originated at.
        path: PathBuf,
        /// Underlying error message.
        detail: String,
    },
    /// Source model conversion failed inside `psxed_gltf`.
    GlbConversionFailed {
        /// Source model path.
        source: PathBuf,
        /// Diagnostic message.
        detail: String,
    },
    /// A model needs its original source to bake new retargeted
    /// animation clips, but that path is not known.
    MissingModelSource {
        /// Model resource id.
        model: ResourceId,
    },
    /// A source model + animation import produced no cooked clips.
    NoCookedAnimationClips {
        /// Animation source path.
        source: PathBuf,
    },
    /// `output_name` produced a path that already exists and
    /// holds non-bundle content. The caller should pick a fresh
    /// name or remove the directory first.
    OutputExists(PathBuf),
}

impl std::fmt::Display for ModelImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BundleNotADirectory(path) => {
                write!(f, "{} is not a directory", path.display())
            }
            Self::NoModelFile(path) => {
                write!(f, "no .psxmdl found in {}", path.display())
            }
            Self::MultipleModelFiles { paths } => {
                write!(f, "multiple .psxmdl files in bundle: {}", paths_list(paths))
            }
            Self::MultipleTextureFiles { paths } => {
                write!(f, "multiple .psxt files in bundle: {}", paths_list(paths))
            }
            Self::InvalidModel { path, detail } => {
                write!(f, "{}: invalid .psxmdl: {detail}", path.display())
            }
            Self::InvalidTexture { path, detail } => {
                write!(f, "{}: invalid .psxt: {detail}", path.display())
            }
            Self::InvalidAnimation { path, detail } => {
                write!(f, "{}: invalid .psxanim: {detail}", path.display())
            }
            Self::JointCountMismatch {
                path,
                animation_joints,
                model_joints,
            } => write!(
                f,
                "{}: animation has {animation_joints} joints, model has {model_joints}",
                path.display()
            ),
            Self::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
            Self::GlbConversionFailed { source, detail } => {
                write!(f, "{}: model conversion failed: {detail}", source.display())
            }
            Self::MissingModelSource { model } => write!(
                f,
                "model resource #{} has no original source path; reimport it or set the source path in the Model inspector",
                model.raw()
            ),
            Self::NoCookedAnimationClips { source } => {
                write!(f, "{} produced no cooked animation clips", source.display())
            }
            Self::OutputExists(path) => write!(
                f,
                "output directory {} exists with conflicting content",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ModelImportError {}

fn paths_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Adopt an existing cooked model bundle as a [`ResourceData::Model`]
/// inside `project`. `bundle_dir` must contain exactly one
/// `.psxmdl`, at most one `.psxt`, and any number of `.psxanim`
/// clips. Stored paths are project-relative whenever
/// `project_root` is supplied and `bundle_dir` lives under it;
/// otherwise the absolute paths are kept.
///
/// Returns the new resource's id. The project gains a single
/// `Model` resource named `display_name`.
pub fn register_cooked_model_bundle(
    project: &mut ProjectDocument,
    bundle_dir: &Path,
    display_name: &str,
    project_root: Option<&Path>,
) -> Result<ResourceId, ModelImportError> {
    let mut psxmdl: Vec<PathBuf> = Vec::new();
    let mut psxt: Vec<PathBuf> = Vec::new();
    let mut psxanim: Vec<PathBuf> = Vec::new();

    let read = std::fs::read_dir(bundle_dir).map_err(|e| {
        if matches!(e.kind(), std::io::ErrorKind::NotFound)
            || matches!(e.kind(), std::io::ErrorKind::NotADirectory)
        {
            ModelImportError::BundleNotADirectory(bundle_dir.to_path_buf())
        } else {
            ModelImportError::Io {
                path: bundle_dir.to_path_buf(),
                detail: e.to_string(),
            }
        }
    })?;
    for entry in read {
        let entry = entry.map_err(|e| ModelImportError::Io {
            path: bundle_dir.to_path_buf(),
            detail: e.to_string(),
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some("psxmdl") => psxmdl.push(path),
            Some("psxt") => psxt.push(path),
            Some("psxanim") => psxanim.push(path),
            _ => {}
        }
    }

    if psxmdl.is_empty() {
        return Err(ModelImportError::NoModelFile(bundle_dir.to_path_buf()));
    }
    if psxmdl.len() > 1 {
        psxmdl.sort();
        return Err(ModelImportError::MultipleModelFiles { paths: psxmdl });
    }
    if psxt.len() > 1 {
        psxt.sort();
        return Err(ModelImportError::MultipleTextureFiles { paths: psxt });
    }

    psxanim.sort();
    let model_path = psxmdl.pop().unwrap();
    let texture_path = psxt.pop();

    // Validate the model + texture + every animation. Failure
    // here means the resource is never created -- we never leave
    // a half-broken `Model` entry in `project.resources`.
    let model_bytes = std::fs::read(&model_path).map_err(|e| ModelImportError::Io {
        path: model_path.clone(),
        detail: e.to_string(),
    })?;
    let model =
        psx_asset::Model::from_bytes(&model_bytes).map_err(|e| ModelImportError::InvalidModel {
            path: model_path.clone(),
            detail: format!("{:?}", e),
        })?;
    let model_joint_count = model.joint_count();
    let skeleton = SkeletonResource::from_model(&model);
    let skeleton_id = find_or_add_skeleton(project, display_name, skeleton);

    if let Some(tex) = &texture_path {
        let bytes = std::fs::read(tex).map_err(|e| ModelImportError::Io {
            path: tex.clone(),
            detail: e.to_string(),
        })?;
        psx_asset::Texture::from_bytes(&bytes).map_err(|e| ModelImportError::InvalidTexture {
            path: tex.clone(),
            detail: format!("{:?}", e),
        })?;
    }

    let mut clips: Vec<ModelAnimationClip> = Vec::with_capacity(psxanim.len());
    for path in &psxanim {
        let bytes = std::fs::read(path).map_err(|e| ModelImportError::Io {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        let anim = psx_asset::Animation::from_bytes(&bytes).map_err(|e| {
            ModelImportError::InvalidAnimation {
                path: path.clone(),
                detail: format!("{:?}", e),
            }
        })?;
        if anim.joint_count() != model_joint_count {
            return Err(ModelImportError::JointCountMismatch {
                path: path.clone(),
                animation_joints: anim.joint_count(),
                model_joints: model_joint_count,
            });
        }
        clips.push(ModelAnimationClip {
            name: clip_name_from_path(path),
            psxanim_path: relativise(path, project_root),
            calibration: Default::default(),
        });
    }

    let default_clip = if clips.is_empty() {
        None
    } else {
        Some(default_clip_index(&clips))
    };

    let model_resource = ModelResource {
        model_path: relativise(&model_path, project_root),
        source_path: None,
        texture_path: texture_path.as_ref().map(|p| relativise(p, project_root)),
        skeleton: Some(skeleton_id),
        clips,
        // Prefer an authored idle clip for first preview/runtime
        // playback. Alphabetical bundle order often puts one-shot
        // clips like "dead" before "idle", which makes index 0 a bad
        // default even though the clip list itself is valid.
        default_clip,
        preview_clip: default_clip,
        world_height: 1024,
        collision_radius: default_model_collision_radius_for_height(1024),
        scale_q8: [crate::MODEL_SCALE_ONE_Q8; 3],
        attachments: Vec::new(),
    };

    let model_id = project.add_resource(display_name, ResourceData::Model(model_resource.clone()));
    let animation_ids =
        register_animation_clip_resources(project, skeleton_id, model_id, &model_resource);
    if !animation_ids.is_empty() {
        register_animation_set_resource(project, display_name, skeleton_id, &animation_ids);
    }
    Ok(model_id)
}

/// Convert a `.glb`, `.gltf`, or `.fbx` source through the rigid-model
/// cooker, write the cooked outputs under
/// `project_root/assets/models/<safe_name>/`, then register that
/// directory as a [`ResourceData::Model`].
///
/// Existing bundle directories are accepted only when they
/// contain exactly the same kinds of files this importer
/// produces -- anything else and the import refuses rather than
/// clobbering user data.
pub fn import_glb_model(
    project: &mut ProjectDocument,
    source_path: &Path,
    output_name: &str,
    project_root: &Path,
    config: psxed_gltf::RigidModelConfig,
) -> Result<ResourceId, ModelImportError> {
    import_model_with_animation_sources(
        project,
        source_path,
        &[],
        output_name,
        project_root,
        config,
    )
}

/// Convert a model source plus optional standalone FBX animation takes
/// through the rigid-model cooker and register the cooked bundle.
pub fn import_model_with_animation_sources(
    project: &mut ProjectDocument,
    source_path: &Path,
    extra_animation_paths: &[PathBuf],
    output_name: &str,
    project_root: &Path,
    config: psxed_gltf::RigidModelConfig,
) -> Result<ResourceId, ModelImportError> {
    let package = convert_rigid_model_source(source_path, extra_animation_paths, &config)?;

    let safe = safe_dir_name(output_name);
    let bundle_dir = project_root.join("assets").join("models").join(&safe);
    prepare_import_bundle_dir(&bundle_dir)?;

    let model_path = bundle_dir.join(format!("{safe}.psxmdl"));
    std::fs::write(&model_path, &package.model).map_err(|e| ModelImportError::Io {
        path: model_path.clone(),
        detail: e.to_string(),
    })?;

    if let Some(texture) = &package.texture {
        let texture_path = bundle_dir.join(format!("{safe}.psxt"));
        std::fs::write(&texture_path, texture).map_err(|e| ModelImportError::Io {
            path: texture_path,
            detail: e.to_string(),
        })?;
    }

    let mut used_clip_stems = BTreeSet::new();
    for clip in &package.clips {
        let clip_stem = unique_clip_stem(
            &mut used_clip_stems,
            &format!("{}_{}", safe, clip.sanitized_name),
        );
        let clip_path = bundle_dir.join(format!("{clip_stem}.psxanim"));
        std::fs::write(&clip_path, &clip.bytes).map_err(|e| ModelImportError::Io {
            path: clip_path,
            detail: e.to_string(),
        })?;
    }

    let model_id =
        register_cooked_model_bundle(project, &bundle_dir, output_name, Some(project_root))?;
    if let Some(resource) = project.resource_mut(model_id) {
        if let ResourceData::Model(model) = &mut resource.data {
            model.source_path = Some(relativise(source_path, Some(project_root)));
            model.world_height = config.world_height;
            model.collision_radius = default_model_collision_radius_for_height(config.world_height);
        }
    }
    Ok(model_id)
}

fn unique_clip_stem(used: &mut BTreeSet<String>, base: &str) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn prepare_import_bundle_dir(bundle_dir: &Path) -> Result<(), ModelImportError> {
    if let Err(e) = std::fs::create_dir_all(bundle_dir) {
        return Err(ModelImportError::Io {
            path: bundle_dir.to_path_buf(),
            detail: e.to_string(),
        });
    }

    // Reject pre-existing non-bundle content rather than silently
    // merging, but clear old cooked bundle files so reimporting the
    // same model cannot retain a stale atlas or obsolete clips.
    if let Ok(read) = std::fs::read_dir(bundle_dir) {
        let mut cooked_files = Vec::new();
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ok = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("psxmdl") | Some("psxt") | Some("psxanim")
                );
                if !ok {
                    return Err(ModelImportError::OutputExists(bundle_dir.to_path_buf()));
                }
                cooked_files.push(path);
            }
        }
        for path in cooked_files {
            if let Err(e) = std::fs::remove_file(&path) {
                return Err(ModelImportError::Io {
                    path,
                    detail: e.to_string(),
                });
            }
        }
    }

    Ok(())
}

/// Convert a GLB/glTF/FBX into the cooked model package without
/// writing files or mutating a project. The editor uses this for
/// the import preview dialog so authors can inspect the baked
/// model, atlas, and animation clips before committing the bundle.
pub fn preview_glb_model(
    source_path: &Path,
    config: psxed_gltf::RigidModelConfig,
) -> Result<psxed_gltf::RigidModelPackage, ModelImportError> {
    preview_model_with_animation_sources(source_path, &[], config)
}

/// Convert a model source plus optional standalone FBX animation takes
/// without writing files or mutating a project.
pub fn preview_model_with_animation_sources(
    source_path: &Path,
    extra_animation_paths: &[PathBuf],
    config: psxed_gltf::RigidModelConfig,
) -> Result<psxed_gltf::RigidModelPackage, ModelImportError> {
    convert_rigid_model_source(source_path, extra_animation_paths, &config)
}

/// Bake one raw animation source against an existing model's original
/// source and register the resulting cooked clip as a target-specific
/// animation resource. The cooked model bytes from the conversion are
/// intentionally discarded; this action only adds an animation clip.
pub fn bake_animation_source_for_model(
    project: &mut ProjectDocument,
    model_id: ResourceId,
    source_id: ResourceId,
    model_source_path: &Path,
    animation_source_path: &Path,
    project_root: &Path,
    config: psxed_gltf::RigidModelConfig,
) -> Result<ResourceId, ModelImportError> {
    let (model_name, model_path, skeleton) = {
        let resource = project
            .resource(model_id)
            .ok_or(ModelImportError::MissingModelSource { model: model_id })?;
        let ResourceData::Model(model) = &resource.data else {
            return Err(ModelImportError::MissingModelSource { model: model_id });
        };
        (
            resource.name.clone(),
            model.model_path.clone(),
            model.skeleton,
        )
    };
    let (source_name, source_meta) = {
        let resource =
            project
                .resource(source_id)
                .ok_or(ModelImportError::NoCookedAnimationClips {
                    source: animation_source_path.to_path_buf(),
                })?;
        let ResourceData::AnimationSource(source) = &resource.data else {
            return Err(ModelImportError::NoCookedAnimationClips {
                source: animation_source_path.to_path_buf(),
            });
        };
        (resource.name.clone(), source.clone())
    };
    let existing_clip = project.resources.iter().find_map(|resource| {
        let ResourceData::AnimationClip(clip) = &resource.data else {
            return None;
        };
        (clip.source == Some(source_id) && clip.target_model == Some(model_id))
            .then(|| (resource.id, clip.psxanim_path.clone()))
    });

    let package = convert_rigid_model_source(
        model_source_path,
        &[animation_source_path.to_path_buf()],
        &config,
    )?;
    let clip = package
        .clips
        .last()
        .ok_or_else(|| ModelImportError::NoCookedAnimationClips {
            source: animation_source_path.to_path_buf(),
        })?;

    let cooked_model_path = resolve_path(&model_path, Some(project_root));
    let bundle_dir = cooked_model_path
        .parent()
        .ok_or_else(|| ModelImportError::Io {
            path: cooked_model_path.clone(),
            detail: "model path has no parent directory".to_string(),
        })?;
    std::fs::create_dir_all(bundle_dir).map_err(|e| ModelImportError::Io {
        path: bundle_dir.to_path_buf(),
        detail: e.to_string(),
    })?;

    let model_prefix = cooked_model_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(safe_dir_name)
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| safe_dir_name(&model_name));
    let clip_path = if let Some((_, existing_path)) = &existing_clip {
        resolve_path(existing_path, Some(project_root))
    } else {
        unique_animation_clip_path(
            bundle_dir,
            &format!("{}_{}", model_prefix, clip.sanitized_name),
        )
    };
    let model_bytes = std::fs::read(&cooked_model_path).map_err(|e| ModelImportError::Io {
        path: cooked_model_path.clone(),
        detail: e.to_string(),
    })?;
    let model =
        psx_asset::Model::from_bytes(&model_bytes).map_err(|e| ModelImportError::InvalidModel {
            path: cooked_model_path.clone(),
            detail: format!("{:?}", e),
        })?;
    let animation = psx_asset::Animation::from_bytes(&clip.bytes).map_err(|e| {
        ModelImportError::InvalidAnimation {
            path: clip_path.clone(),
            detail: format!("{:?}", e),
        }
    })?;
    if animation.joint_count() != model.joint_count() {
        return Err(ModelImportError::JointCountMismatch {
            path: clip_path,
            animation_joints: animation.joint_count(),
            model_joints: model.joint_count(),
        });
    }
    std::fs::write(&clip_path, &clip.bytes).map_err(|e| ModelImportError::Io {
        path: clip_path.clone(),
        detail: e.to_string(),
    })?;

    let clip_name = if !source_meta.clip_name.trim().is_empty() {
        source_meta.clip_name.trim().to_string()
    } else if !source_name.trim().is_empty() {
        source_name
    } else {
        clip.sanitized_name.clone()
    };
    let role = if matches!(source_meta.role, AnimationRole::Generic) {
        AnimationRole::guess_from_name(&clip_name)
    } else {
        source_meta.role
    };
    let stored_path = relativise(&clip_path, Some(project_root));
    let resource_name = format!("{model_name} / {clip_name}");
    let clip_resource = AnimationClipResource {
        psxanim_path: stored_path,
        skeleton,
        source: Some(source_id),
        target_model: Some(model_id),
        bake: AnimationClipBakeKind::Retargeted,
        role,
        looping: source_meta.looping,
        tags: source_meta.tags,
        calibration: Default::default(),
    };
    if let Some((existing_id, _)) = existing_clip {
        if let Some(resource) = project.resource_mut(existing_id) {
            resource.name = resource_name;
            resource.data = ResourceData::AnimationClip(clip_resource);
        }
        Ok(existing_id)
    } else {
        Ok(project.add_resource(resource_name, ResourceData::AnimationClip(clip_resource)))
    }
}

fn convert_rigid_model_source(
    source_path: &Path,
    extra_animation_paths: &[PathBuf],
    config: &psxed_gltf::RigidModelConfig,
) -> Result<psxed_gltf::RigidModelPackage, ModelImportError> {
    let is_fbx = source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fbx"));
    let result = if !extra_animation_paths.is_empty() {
        psxed_gltf::convert_rigid_model_path_with_animation_paths(
            source_path,
            extra_animation_paths,
            config,
        )
    } else if is_fbx {
        psxed_gltf::convert_fbx_rigid_model_path(source_path, config)
    } else {
        psxed_gltf::convert_rigid_model_path(source_path, config)
    };
    result.map_err(|e| ModelImportError::GlbConversionFailed {
        source: source_path.to_path_buf(),
        detail: format!("{e}"),
    })
}

fn unique_animation_clip_path(bundle_dir: &Path, stem: &str) -> PathBuf {
    let mut candidate = bundle_dir.join(format!("{stem}.psxanim"));
    let mut index = 2usize;
    while candidate.exists() {
        candidate = bundle_dir.join(format!("{stem}_{index}.psxanim"));
        index += 1;
    }
    candidate
}

/// Derive a clip display name from a `.psxanim` path. Strips the
/// extension and any leading `<model>_` prefix when one of the
/// canonical stems is recognised, so a bundle called
/// `obsidian_wraith` produces clip names like `idle` rather than
/// `obsidian_wraith_idle`.
fn clip_name_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    // Bundle-prefix stripping: pick the longest known model
    // prefix that the stem starts with. This is heuristic -- when
    // we don't recognise the prefix we keep the full stem so the
    // user can rename in the inspector.
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let prefix_len = parent_name.len() + 1;
    if !parent_name.is_empty()
        && stem.len() > prefix_len
        && stem.starts_with(parent_name)
        && stem.as_bytes().get(parent_name.len()) == Some(&b'_')
    {
        return stem[prefix_len..].to_string();
    }
    stem.to_string()
}

fn default_clip_index(clips: &[ModelAnimationClip]) -> u16 {
    if let Some(index) = clips
        .iter()
        .position(|clip| clip.name.eq_ignore_ascii_case("idle"))
    {
        return index as u16;
    }
    if let Some(index) = clips
        .iter()
        .position(|clip| clip.name.to_ascii_lowercase().contains("idle"))
    {
        return index as u16;
    }
    0
}

fn find_or_add_skeleton(
    project: &mut ProjectDocument,
    display_name: &str,
    skeleton: SkeletonResource,
) -> ResourceId {
    if let Some(existing) = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Skeleton(existing)
                if !existing.signature.is_empty() && existing.signature == skeleton.signature =>
            {
                Some(resource.id)
            }
            _ => None,
        })
    {
        return existing;
    }

    let mut skeleton = skeleton;
    if skeleton.note.trim().is_empty() {
        skeleton.note = format!("Imported from {display_name}");
    }
    project.add_resource(
        format!("{display_name} Skeleton"),
        ResourceData::Skeleton(skeleton),
    )
}

fn register_animation_clip_resources(
    project: &mut ProjectDocument,
    skeleton_id: ResourceId,
    model_id: ResourceId,
    model: &ModelResource,
) -> Vec<ResourceId> {
    let mut ids = Vec::new();
    for clip in &model.clips {
        let existing = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::AnimationClip(existing)
                    if existing.psxanim_path == clip.psxanim_path
                        && existing.skeleton == Some(skeleton_id) =>
                {
                    Some(resource.id)
                }
                _ => None,
            });
        if let Some(id) = existing {
            ids.push(id);
            continue;
        }
        let role = AnimationRole::guess_from_name(&clip.name);
        let source_id =
            find_or_add_animation_source_for_clip(project, skeleton_id, model_id, clip, role);
        let id = project.add_resource(
            clip.name.clone(),
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: clip.psxanim_path.clone(),
                skeleton: Some(skeleton_id),
                source: Some(source_id),
                target_model: Some(model_id),
                bake: AnimationClipBakeKind::ModelNative,
                role,
                looping: !matches!(
                    role,
                    AnimationRole::Roll
                        | AnimationRole::Backstep
                        | AnimationRole::Attack
                        | AnimationRole::Hit
                        | AnimationRole::Death
                ),
                tags: role_tag_list(role),
                calibration: clip.calibration,
            }),
        );
        ids.push(id);
    }
    ids
}

fn find_or_add_animation_source_for_clip(
    project: &mut ProjectDocument,
    skeleton_id: ResourceId,
    model_id: ResourceId,
    clip: &ModelAnimationClip,
    role: AnimationRole,
) -> ResourceId {
    if let Some(existing) = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::AnimationSource(source)
                if source.source_path == clip.psxanim_path
                    && source.clip_name == clip.name
                    && source.target_model == Some(model_id) =>
            {
                Some(resource.id)
            }
            _ => None,
        })
    {
        return existing;
    }

    let mut source =
        AnimationSourceResource::from_path(clip.psxanim_path.clone(), clip.name.clone());
    source.skeleton = Some(skeleton_id);
    source.target_model = Some(model_id);
    source.role = role;
    source.looping = !matches!(
        role,
        AnimationRole::Roll
            | AnimationRole::Backstep
            | AnimationRole::Attack
            | AnimationRole::Hit
            | AnimationRole::Death
    );
    source.tags = role_tag_list(role);
    project.add_resource(
        format!("{} Source", clip.name),
        ResourceData::AnimationSource(source),
    )
}

fn register_animation_set_resource(
    project: &mut ProjectDocument,
    display_name: &str,
    skeleton_id: ResourceId,
    animation_ids: &[ResourceId],
) -> ResourceId {
    let mut set = AnimationSetResource {
        skeleton: Some(skeleton_id),
        ..AnimationSetResource::default()
    };
    for id in animation_ids {
        let Some(resource) = project.resource(*id) else {
            continue;
        };
        let ResourceData::AnimationClip(clip) = &resource.data else {
            continue;
        };
        let action = CharacterAnimationAction::guess_from_name(&resource.name).or_else(|| {
            CharacterAnimationAction::ALL
                .iter()
                .copied()
                .find(|action| action.role_hint() == Some(clip.role))
        });
        if let Some(action) = action {
            if set.action_clip(action).is_none() {
                set.set_action_clip(action, Some(*id));
            }
        }
        if !set.clips.contains(id) {
            set.clips.push(*id);
        }
    }
    let set_name = format!("{display_name} Animation Set");
    if let Some(existing_id) = project.resources.iter().find_map(|resource| {
        let ResourceData::AnimationSet(existing) = &resource.data else {
            return None;
        };
        (resource.name == set_name && existing.skeleton == Some(skeleton_id)).then_some(resource.id)
    }) {
        if let Some(resource) = project.resource_mut(existing_id) {
            if let ResourceData::AnimationSet(existing) = &mut resource.data {
                merge_animation_set(existing, &set);
            }
        }
        existing_id
    } else {
        project.add_resource(set_name, ResourceData::AnimationSet(set))
    }
}

fn merge_animation_set(target: &mut AnimationSetResource, source: &AnimationSetResource) {
    if target.skeleton.is_none() {
        target.skeleton = source.skeleton;
    }
    for role in [
        AnimationRole::Idle,
        AnimationRole::Walk,
        AnimationRole::Run,
        AnimationRole::Turn,
        AnimationRole::Roll,
        AnimationRole::Backstep,
    ] {
        let source_clip = source.role_clip(role);
        if source_clip.is_some() {
            if let Some(target_slot) = target.role_clip_mut(role) {
                if target_slot.is_none() {
                    *target_slot = source_clip;
                }
            }
        }
    }
    for binding in &source.action_clips {
        if target.action_clip(binding.action).is_none() {
            target.set_action_clip(binding.action, Some(binding.clip));
        }
    }
    for clip in &source.clips {
        if !target.clips.contains(clip) {
            target.clips.push(*clip);
        }
    }
}

fn role_tag_list(role: AnimationRole) -> Vec<String> {
    if matches!(role, AnimationRole::Generic) {
        Vec::new()
    } else {
        vec![role.label().to_ascii_lowercase()]
    }
}

/// Convert `path` into a relative-to-project string when
/// `project_root` is provided and `path` lives under it. Falls
/// back to an absolute path so the editor can still find the
/// file regardless of where the project moves later.
fn relativise(path: &Path, project_root: Option<&Path>) -> String {
    if let Some(root) = project_root {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

/// Sanitise a user-supplied resource name into a filesystem-safe
/// directory name (lowercase ASCII alphanumerics + underscores).
fn safe_dir_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectDocument;

    /// Layout of a synthetic bundle dir for tests -- caller
    /// passes byte slices for each file kind, helper writes them
    /// next to a fresh tempdir.
    fn make_bundle(
        tag: &str,
        model_bytes: Option<&[u8]>,
        models_count: usize,
        textures: &[&[u8]],
        animations: &[(&str, &[u8])],
    ) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "psxed-model-import-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(bytes) = model_bytes {
            for i in 0..models_count {
                std::fs::write(dir.join(format!("model_{i}.psxmdl")), bytes).unwrap();
            }
        }
        for (i, bytes) in textures.iter().enumerate() {
            std::fs::write(dir.join(format!("atlas_{i}.psxt")), bytes).unwrap();
        }
        for (name, bytes) in animations {
            std::fs::write(dir.join(format!("{name}.psxanim")), bytes).unwrap();
        }
        dir
    }

    fn obsidian_wraith_dir() -> PathBuf {
        // The obsidian wraith assets live in the repo root's
        // `assets/models/obsidian_wraith/` and are exercised by
        // showcase-model.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("assets")
            .join("models")
            .join("obsidian_wraith")
    }

    #[test]
    fn registers_obsidian_wraith_bundle() {
        let mut project = ProjectDocument::starter();
        let dir = obsidian_wraith_dir();
        let id = register_cooked_model_bundle(&mut project, &dir, "Obsidian Wraith", None)
            .expect("bundle registers");
        let resource = project.resource(id).expect("resource exists");
        let ResourceData::Model(model) = &resource.data else {
            panic!("expected Model resource, got {:?}", resource.data);
        };
        assert!(model.model_path.ends_with("obsidian_wraith.psxmdl"));
        assert!(model.texture_path.is_some());
        assert!(!model.clips.is_empty(), "expected at least one clip");
        // Clips are sorted by file name, but default/preview should
        // prefer the idle clip instead of blindly picking slot 0.
        let mut sorted_names: Vec<&str> = model.clips.iter().map(|c| c.name.as_str()).collect();
        sorted_names.sort();
        assert!(sorted_names.contains(&"idle"));
        assert_eq!(model.default_clip, Some(3));
        assert_eq!(model.preview_clip, Some(3));
    }

    #[test]
    fn import_bundle_preparation_removes_stale_cooked_files() {
        let dir = make_bundle(
            "stale-cooked-files",
            Some(b"PSMDbogus"),
            1,
            &[b"old atlas"],
            &[("old_idle", b"old animation")],
        );

        prepare_import_bundle_dir(&dir).expect("bundle files are safe to clear");

        assert!(!dir.join("model_0.psxmdl").exists());
        assert!(!dir.join("atlas_0.psxt").exists());
        assert!(!dir.join("old_idle.psxanim").exists());
        assert!(dir.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_bundle_preparation_rejects_non_bundle_files() {
        let dir = make_bundle("stale-non-bundle-files", Some(b"PSMDbogus"), 1, &[], &[]);
        std::fs::write(dir.join("notes.txt"), b"do not delete").unwrap();

        match prepare_import_bundle_dir(&dir) {
            Err(ModelImportError::OutputExists(path)) => assert_eq!(path, dir),
            other => panic!("expected OutputExists, got {other:?}"),
        }
        assert!(dir.join("model_0.psxmdl").exists());
        assert!(dir.join("notes.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_clip_stem_preserves_duplicate_animation_names() {
        let mut used = BTreeSet::new();
        assert_eq!(
            unique_clip_stem(&mut used, "knight_running"),
            "knight_running"
        );
        assert_eq!(
            unique_clip_stem(&mut used, "knight_running"),
            "knight_running_2"
        );
        assert_eq!(
            unique_clip_stem(&mut used, "knight_running"),
            "knight_running_3"
        );
    }

    #[test]
    fn no_model_file_fails() {
        let mut project = ProjectDocument::starter();
        let dir = make_bundle("no-model", None, 0, &[], &[]);
        match register_cooked_model_bundle(&mut project, &dir, "Empty", None) {
            Err(ModelImportError::NoModelFile(_)) => {}
            other => panic!("expected NoModelFile, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiple_models_fail() {
        let mut project = ProjectDocument::starter();
        // Two models -- content doesn't matter because the
        // duplicate detection happens before parsing.
        let bogus = b"PSMDbogus";
        let dir = make_bundle("multi-model", Some(bogus), 2, &[], &[]);
        match register_cooked_model_bundle(&mut project, &dir, "Multi", None) {
            Err(ModelImportError::MultipleModelFiles { paths }) => {
                assert_eq!(paths.len(), 2);
            }
            other => panic!("expected MultipleModelFiles, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_model_bytes_fail() {
        let mut project = ProjectDocument::starter();
        let dir = make_bundle("bad-model", Some(b"NOTAPSXMDL"), 1, &[], &[]);
        match register_cooked_model_bundle(&mut project, &dir, "Bad", None) {
            Err(ModelImportError::InvalidModel { .. }) => {}
            other => panic!("expected InvalidModel, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relativise_under_project_root_is_relative() {
        let root = PathBuf::from("/tmp/proj");
        let path = PathBuf::from("/tmp/proj/assets/models/x.psxmdl");
        assert_eq!(relativise(&path, Some(&root)), "assets/models/x.psxmdl");
        // No root → absolute kept.
        let abs = relativise(&path, None);
        assert_eq!(abs, "/tmp/proj/assets/models/x.psxmdl");
    }

    #[test]
    fn safe_dir_name_strips_punctuation() {
        assert_eq!(safe_dir_name("Obsidian Wraith"), "obsidian_wraith");
        assert_eq!(safe_dir_name("hooded-wretch"), "hooded_wretch");
        assert_eq!(safe_dir_name("!!!"), "model");
    }

    #[test]
    fn model_stats_from_obsidian_wraith() {
        let dir = obsidian_wraith_dir();
        let model_bytes = std::fs::read(dir.join("obsidian_wraith.psxmdl")).unwrap();
        let stats = model_stats_from_bytes(&model_bytes).expect("parse");
        assert!(stats.joint_count > 0);
        assert!(stats.part_count > 0);
        assert!(stats.vertex_count > 0);
        assert!(stats.face_count > 0);
        // AABB must span at least one unit on every axis after
        // walking real vertices.
        assert!(stats.bounds_max[0] >= stats.bounds_min[0]);
        assert!(stats.bounds_max[1] >= stats.bounds_min[1]);
        assert!(stats.bounds_max[2] >= stats.bounds_min[2]);
    }

    #[test]
    fn animation_stats_match_obsidian_wraith() {
        let dir = obsidian_wraith_dir();
        let model_bytes = std::fs::read(dir.join("obsidian_wraith.psxmdl")).unwrap();
        let model_stats = model_stats_from_bytes(&model_bytes).unwrap();
        let idle = std::fs::read(dir.join("obsidian_wraith_idle.psxanim")).unwrap();
        let stats =
            animation_stats_from_bytes("idle", &idle, model_stats.joint_count).expect("parse");
        assert!(stats.valid_for_model);
        assert!(stats.frame_count > 0);
        assert!(stats.sample_rate_hz > 0);
    }

    #[test]
    fn animation_stats_flag_joint_mismatch() {
        let dir = obsidian_wraith_dir();
        let idle = std::fs::read(dir.join("obsidian_wraith_idle.psxanim")).unwrap();
        let stats = animation_stats_from_bytes("idle", &idle, 999).expect("parse");
        assert!(!stats.valid_for_model);
    }

    #[test]
    fn texture_stats_detect_8bpp_atlas() {
        let dir = obsidian_wraith_dir();
        let bytes = std::fs::read(dir.join("obsidian_wraith_128x128_8bpp.psxt")).unwrap();
        let stats = texture_stats_from_bytes(&bytes).expect("parse");
        assert_eq!(stats.depth, 8);
        assert_eq!(stats.width, 128);
        assert_eq!(stats.height, 128);
        assert_eq!(stats.clut_entries, 256);
    }
}
