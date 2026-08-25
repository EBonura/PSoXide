use crate::centered_aspect_rect;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use psx_asset::{Animation, Texture};
use psxed_project::{
    model_import::resolve_path, AnimationClipCalibration, AnimationPoseCorrectionKey,
    AnimationRole, CharacterActionOptions, CharacterAnimationAction, NodeKind, ProjectDocument,
    ResourceData, ResourceId,
};

use crate::editor_helpers::{
    animation_source_authoring_label, collect_animation_clip_authoring_labels,
    distance_to_segment_2d,
};
use crate::icons;
use crate::model_import_preview::{self, ImportPreviewOptions};
use crate::searchable_picker::{searchable_picker, SearchablePickerConfig};
use crate::style::{
    STUDIO_ACCENT, STUDIO_ACCENT_DIM, STUDIO_BORDER, STUDIO_BORDER_DARK, STUDIO_DOCK, STUDIO_HOVER,
    STUDIO_INPUT, STUDIO_PANEL_DARK, STUDIO_PANEL_HEADER, STUDIO_SELECTION, STUDIO_TEXT,
    STUDIO_TEXT_WEAK,
};

const TIMELINE_DEFAULT_HEIGHT: f32 = 206.0;
const TIMELINE_MIN_HEIGHT: f32 = 132.0;
const TIMELINE_MIN_PREVIEW_HEIGHT: f32 = 250.0;
const TIMELINE_TRACK_LABEL_WIDTH: f32 = 172.0;
const TIMELINE_RULER_HEIGHT: f32 = 28.0;
const TIMELINE_TRACK_HEIGHT: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationPreviewQuality {
    Authoring,
    PsxOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationStudioMode {
    Preview,
    Moveset,
    Pose,
    Weapon,
    Combat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapsuleEditTool {
    Move,
    Rotate,
    Resize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapsuleEditAxis {
    X,
    Y,
    Z,
}

impl CapsuleEditAxis {
    const ALL: [Self; 3] = [Self::X, Self::Y, Self::Z];

    const fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }

    const fn color(self) -> Color32 {
        match self {
            Self::X => Color32::from_rgb(240, 92, 92),
            Self::Y => Color32::from_rgb(110, 226, 110),
            Self::Z => Color32::from_rgb(116, 160, 255),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ModelAnimationViewerState {
    selected_model: Option<ResourceId>,
    selected_character: Option<ResourceId>,
    selected_clip_path: Option<String>,
    clip_filter: String,
    last_clip_path: Option<String>,
    playing: bool,
    frame: f32,
    playback_speed: f32,
    yaw_q12: i32,
    pitch_q12: i32,
    radius: i32,
    show_animation_root: bool,
    show_bones: bool,
    show_combat_capsules: bool,
    show_pose_corrections: bool,
    show_attachment_sockets: bool,
    show_moveset: bool,
    selected_combat_capsule: usize,
    selected_attachment_socket: usize,
    selected_weapon_track: usize,
    preview_weapon: Option<ResourceId>,
    selected_pose_joint: u16,
    selected_action: CharacterAnimationAction,
    capsule_edit_tool: CapsuleEditTool,
    capsule_edit_axis: CapsuleEditAxis,
    gizmo_drag_axis: Option<CapsuleEditAxis>,
    gizmo_drag_fractional_units: f32,
    timeline_height: f32,
    timeline_resize_origin: Option<f32>,
    timeline_pixels_per_frame: f32,
    preview_quality: AnimationPreviewQuality,
    cached_model: Option<CachedModelContext>,
    cached_weapon_model: Option<CachedModelContext>,
    cached_material: Option<CachedMaterialLayer>,
    cached_clip: Option<CachedClipContext>,
    last_time_seconds: f64,
}

impl Default for ModelAnimationViewerState {
    fn default() -> Self {
        Self {
            selected_model: None,
            selected_character: None,
            selected_clip_path: None,
            clip_filter: String::new(),
            last_clip_path: None,
            playing: true,
            frame: 0.0,
            playback_speed: 1.0,
            yaw_q12: 340,
            pitch_q12: 350,
            radius: 0,
            show_animation_root: false,
            show_bones: false,
            show_combat_capsules: false,
            show_pose_corrections: false,
            show_attachment_sockets: false,
            show_moveset: false,
            selected_combat_capsule: 0,
            selected_attachment_socket: 0,
            selected_weapon_track: 0,
            preview_weapon: None,
            selected_pose_joint: 0,
            selected_action: CharacterAnimationAction::Idle,
            capsule_edit_tool: CapsuleEditTool::Move,
            capsule_edit_axis: CapsuleEditAxis::X,
            gizmo_drag_axis: None,
            gizmo_drag_fractional_units: 0.0,
            timeline_height: TIMELINE_DEFAULT_HEIGHT,
            timeline_resize_origin: None,
            timeline_pixels_per_frame: 12.0,
            preview_quality: AnimationPreviewQuality::Authoring,
            cached_model: None,
            cached_weapon_model: None,
            cached_material: None,
            cached_clip: None,
            last_time_seconds: 0.0,
        }
    }
}

impl ModelAnimationViewerState {
    pub(crate) const fn selected_model(&self) -> Option<ResourceId> {
        self.selected_model
    }

    pub(crate) const fn selected_weapon(&self) -> Option<ResourceId> {
        self.preview_weapon
    }

    pub(crate) fn selected_clip_path(&self) -> Option<&str> {
        self.selected_clip_path.as_deref()
    }

    /// Return the preview to its content-derived framing while preserving the
    /// user's current viewing angle. This is the Animation workspace's `.`
    /// (frame selected) behavior.
    pub(crate) fn frame_preview(&mut self) {
        self.radius = 0;
    }

    pub(crate) fn focus_resource(&mut self, project: &ProjectDocument, id: ResourceId) {
        let Some(resource) = project.resource(id) else {
            return;
        };
        // Focusing is also the resource inspector's live-refresh hook. Clear
        // decoded contexts even when the resource keeps the same path so an
        // in-place recook cannot leave the viewer holding stale bytes.
        self.invalidate_model_cache();
        self.invalidate_clip_cache();
        match &resource.data {
            ResourceData::Character(character) => {
                self.selected_character = Some(id);
                self.set_studio_mode(AnimationStudioMode::Preview);
                self.selected_action = CharacterAnimationAction::Idle;
                self.selected_model = character.model;
                self.selected_clip_path =
                    character_action_clip_path(project, id, self.selected_action)
                        .or_else(|| self.preferred_model_clip_path(project));
                self.reset_clip_clock();
            }
            ResourceData::Model(_) => {
                self.selected_character = None;
                self.set_studio_mode(AnimationStudioMode::Preview);
                self.selected_model = Some(id);
                self.selected_clip_path = self.preferred_model_clip_path(project);
                self.reset_clip_clock();
            }
            ResourceData::AnimationClip(clip) => {
                self.selected_character = None;
                self.set_studio_mode(AnimationStudioMode::Pose);
                self.selected_model = clip
                    .target_model
                    .or_else(|| first_model_for_skeleton(project, clip.skeleton));
                self.selected_clip_path = Some(clip.psxanim_path.clone());
                self.reset_clip_clock();
            }
            ResourceData::AnimationSource(source) => {
                self.selected_character = None;
                self.set_studio_mode(AnimationStudioMode::Preview);
                self.selected_model = source
                    .target_model
                    .or_else(|| first_model_for_skeleton(project, source.skeleton));
                self.selected_clip_path =
                    baked_clip_path_for_source(project, id, self.selected_model)
                        .or_else(|| Some(source.source_path.clone()));
                self.reset_clip_clock();
            }
            ResourceData::AnimationSet(set) => {
                self.selected_character = project.resources.iter().find_map(|resource| {
                    let ResourceData::Character(character) = &resource.data else {
                        return None;
                    };
                    (character.animation_set == Some(id)).then_some(resource.id)
                });
                self.set_studio_mode(AnimationStudioMode::Preview);
                self.selected_action = CharacterAnimationAction::Idle;
                self.selected_model = self
                    .selected_character
                    .and_then(|character_id| project.resource(character_id))
                    .and_then(|resource| {
                        let ResourceData::Character(character) = &resource.data else {
                            return None;
                        };
                        character.model
                    })
                    .or_else(|| first_model_for_skeleton(project, set.skeleton));
                self.selected_clip_path = self
                    .selected_character
                    .and_then(|character_id| {
                        character_action_clip_path(project, character_id, self.selected_action)
                    })
                    .or_else(|| self.preferred_model_clip_path(project));
                self.reset_clip_clock();
            }
            ResourceData::Weapon(weapon) => {
                self.preview_weapon = Some(id);
                self.selected_character = project.resources.iter().find_map(|resource| {
                    let ResourceData::Character(character) = &resource.data else {
                        return None;
                    };
                    let model_has_socket = character.model.is_some_and(|model_id| {
                        project.resource(model_id).is_some_and(|resource| {
                            let ResourceData::Model(model) = &resource.data else {
                                return false;
                            };
                            model
                                .attachments
                                .iter()
                                .any(|socket| socket.name == weapon.default_character_socket)
                        })
                    });
                    model_has_socket.then_some(resource.id)
                });
                self.selected_model = self
                    .selected_character
                    .and_then(|character_id| project.resource(character_id))
                    .and_then(|resource| {
                        let ResourceData::Character(character) = &resource.data else {
                            return None;
                        };
                        character.model
                    });
                if let Some(character_id) = self.selected_character {
                    if let Some((action, _)) = character_weapon_tracks(project, character_id)
                        .find(|(_, track)| track.weapon == id)
                    {
                        self.selected_action = action;
                    }
                }
                self.set_studio_mode(AnimationStudioMode::Weapon);
                self.selected_clip_path = self
                    .selected_character
                    .and_then(|character_id| {
                        character_action_clip_path(project, character_id, self.selected_action)
                    })
                    .or_else(|| self.preferred_model_clip_path(project));
                self.reset_clip_clock();
            }
            _ => {}
        }
        self.ensure_selection(project);
    }

    fn ensure_selection(&mut self, project: &ProjectDocument) {
        if self.selected_model.is_some_and(|id| {
            !matches!(
                project.resource(id).map(|r| &r.data),
                Some(ResourceData::Model(_))
            )
        }) {
            self.selected_model = None;
        }

        if self.selected_model.is_none() {
            self.selected_model = first_model_id(project);
        }

        let clip_options = self
            .selected_model
            .map(|model| build_clip_options(project, model))
            .unwrap_or_default();
        let selected_clip_still_exists = self
            .selected_clip_path
            .as_ref()
            .is_some_and(|path| clip_options.iter().any(|clip| clip.path == *path));
        if !selected_clip_still_exists {
            self.selected_clip_path = self
                .preferred_model_clip_path(project)
                .or_else(|| clip_options.first().map(|clip| clip.path.clone()));
            self.reset_clip_clock();
        }
    }

    fn preferred_model_clip_path(&self, project: &ProjectDocument) -> Option<String> {
        self.selected_model.and_then(|model_id| {
            project
                .resolved_model_animation_clips(model_id)
                .first()
                .map(|clip| clip.psxanim_path.clone())
        })
    }

    fn reset_clip_clock(&mut self) {
        self.frame = 0.0;
        self.last_clip_path = None;
    }

    fn invalidate_model_cache(&mut self) {
        self.cached_model = None;
        self.cached_weapon_model = None;
        self.cached_material = None;
    }

    fn invalidate_clip_cache(&mut self) {
        self.cached_clip = None;
    }

    fn studio_mode(&self) -> AnimationStudioMode {
        if self.show_moveset {
            AnimationStudioMode::Moveset
        } else if self.show_pose_corrections {
            AnimationStudioMode::Pose
        } else if self.show_attachment_sockets {
            AnimationStudioMode::Weapon
        } else if self.show_combat_capsules {
            AnimationStudioMode::Combat
        } else {
            AnimationStudioMode::Preview
        }
    }

    fn set_studio_mode(&mut self, mode: AnimationStudioMode) {
        self.show_moveset = mode == AnimationStudioMode::Moveset;
        self.show_pose_corrections = mode == AnimationStudioMode::Pose;
        self.show_attachment_sockets = mode == AnimationStudioMode::Weapon;
        self.show_combat_capsules = mode == AnimationStudioMode::Combat;
    }
}

fn first_model_for_skeleton(
    project: &ProjectDocument,
    skeleton: Option<ResourceId>,
) -> Option<ResourceId> {
    let skeleton = skeleton?;
    project.resources.iter().find_map(|resource| {
        let ResourceData::Model(model) = &resource.data else {
            return None;
        };
        (model.skeleton == Some(skeleton)).then_some(resource.id)
    })
}

fn character_action_clip_path(
    project: &ProjectDocument,
    character_id: ResourceId,
    action: CharacterAnimationAction,
) -> Option<String> {
    let set_id = project.resource(character_id).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        character.animation_set
    })?;
    let clip_id = project.resource(set_id).and_then(|resource| {
        let ResourceData::AnimationSet(set) = &resource.data else {
            return None;
        };
        set.action_clip(action)
    })?;
    project.resource(clip_id).and_then(|resource| {
        let ResourceData::AnimationClip(clip) = &resource.data else {
            return None;
        };
        Some(clip.psxanim_path.clone())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MovesetCapabilityStatus {
    Ready,
    Missing,
    Disabled,
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MovesetBindingSource {
    Action,
    LegacyRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MovesetCapabilityRow {
    pub(crate) action: CharacterAnimationAction,
    pub(crate) status: MovesetCapabilityStatus,
    pub(crate) clip: Option<ResourceId>,
    pub(crate) clip_name: Option<String>,
    pub(crate) binding_source: Option<MovesetBindingSource>,
    pub(crate) visual_fallback_action: Option<CharacterAnimationAction>,
    pub(crate) visual_fallback_clip: Option<ResourceId>,
    pub(crate) visual_fallback_name: Option<String>,
}

/// Ordered visual fallbacks used by `RuntimeCharacter::clip_for` when an
/// action is forced without owning a clip. These do not enable gameplay: the
/// capability remains disabled until the action itself resolves a clip.
fn moveset_visual_fallbacks(
    action: CharacterAnimationAction,
) -> [Option<CharacterAnimationAction>; 4] {
    use CharacterAnimationAction as A;
    match action {
        A::Idle => [None, None, None, None],
        A::Walk => [Some(A::Idle), None, None, None],
        A::Run => [Some(A::Walk), Some(A::Idle), None, None],
        A::Turn => [Some(A::Idle), None, None, None],
        A::Roll => [Some(A::Run), Some(A::Walk), Some(A::Idle), None],
        A::Backstep => [Some(A::Roll), Some(A::Walk), Some(A::Idle), None],
        A::LightAttack => [Some(A::ComboAttack), Some(A::Idle), None, None],
        A::HeavyAttack => [Some(A::LightAttack), Some(A::Idle), None, None],
        A::ComboAttack => [Some(A::LightAttack), Some(A::Idle), None, None],
        A::Block | A::HitReact | A::Death | A::Intro => [Some(A::Idle), None, None, None],
        A::WalkBackward | A::StrafeLeft | A::StrafeRight => {
            [Some(A::Walk), Some(A::Idle), None, None]
        }
        A::DashLeft | A::DashRight => [Some(A::Roll), Some(A::Walk), Some(A::Idle), None],
        A::Stun => [Some(A::HitReact), Some(A::Idle), None, None],
        A::StunRecovery => [Some(A::Stun), Some(A::Idle), None, None],
        A::HitReactAlt => [Some(A::HitReact), Some(A::Idle), None, None],
        A::AltLightAttack => [Some(A::LightAttack), Some(A::Idle), None, None],
        A::AltHeavyAttack => [Some(A::HeavyAttack), Some(A::Idle), None, None],
        A::AltComboAttack => [Some(A::ComboAttack), Some(A::Idle), None, None],
        A::WalkWindup | A::WalkWinddown => [Some(A::Walk), Some(A::Idle), None, None],
        A::WalkWinddownAlt => [Some(A::WalkWinddown), Some(A::Walk), Some(A::Idle), None],
        A::RunWindup | A::RunWinddown => [Some(A::Run), Some(A::Walk), Some(A::Idle), None],
        A::RunWinddownAlt => [
            Some(A::RunWinddown),
            Some(A::Run),
            Some(A::Walk),
            Some(A::Idle),
        ],
        // Vertical attacks deliberately do not borrow another attack. Idle is
        // only the renderer's safe pose if an external caller forces one.
        A::VertLightAttack | A::VertHeavyAttack | A::VertComboAttack => {
            [Some(A::Idle), None, None, None]
        }
    }
}

fn valid_animation_clip(
    project: &ProjectDocument,
    clip: Option<ResourceId>,
) -> Option<(ResourceId, String)> {
    let clip = clip?;
    let resource = project.resource(clip)?;
    matches!(resource.data, ResourceData::AnimationClip(_)).then(|| (clip, resource.name.clone()))
}

/// Build the read-only matrix from the same Animation Set action resolution
/// used by the cooker. A missing optional row is disabled even when a visual
/// fallback exists; that distinction is the point of the matrix.
pub(crate) fn moveset_capability_rows(
    project: &ProjectDocument,
    character_id: ResourceId,
) -> Option<Vec<MovesetCapabilityRow>> {
    let set_id = project.resource(character_id).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        character.animation_set
    })?;
    let set = project.resource(set_id).and_then(|resource| {
        let ResourceData::AnimationSet(set) = &resource.data else {
            return None;
        };
        Some(set)
    })?;

    Some(
        CharacterAnimationAction::ALL
            .into_iter()
            .map(|action| {
                let resolved_clip = set.action_clip(action);
                let valid_clip = valid_animation_clip(project, resolved_clip);
                let binding_source = valid_clip.as_ref().map(|_| {
                    if set.action_binding(action).is_some() {
                        MovesetBindingSource::Action
                    } else {
                        MovesetBindingSource::LegacyRole
                    }
                });
                let (visual_fallback_action, visual_fallback_clip, visual_fallback_name) =
                    moveset_visual_fallbacks(action)
                        .into_iter()
                        .flatten()
                        .find_map(|fallback| {
                            valid_animation_clip(project, set.action_clip(fallback))
                                .map(|(clip, name)| (Some(fallback), Some(clip), Some(name)))
                        })
                        .unwrap_or((None, None, None));
                let status = if valid_clip.is_some() {
                    MovesetCapabilityStatus::Ready
                } else if resolved_clip.is_some() {
                    MovesetCapabilityStatus::Broken
                } else if action.required_for_player() {
                    MovesetCapabilityStatus::Missing
                } else {
                    MovesetCapabilityStatus::Disabled
                };
                let (clip, clip_name) = valid_clip
                    .map(|(clip, name)| (Some(clip), Some(name)))
                    .unwrap_or((resolved_clip, None));
                MovesetCapabilityRow {
                    action,
                    status,
                    clip,
                    clip_name,
                    binding_source,
                    visual_fallback_action,
                    visual_fallback_clip,
                    visual_fallback_name,
                }
            })
            .collect(),
    )
}

fn character_weapon_tracks(
    project: &ProjectDocument,
    character_id: ResourceId,
) -> impl Iterator<
    Item = (
        CharacterAnimationAction,
        &psxed_project::WeaponAppearanceTrack,
    ),
> {
    project
        .resource(character_id)
        .and_then(|resource| {
            let ResourceData::Character(character) = &resource.data else {
                return None;
            };
            character.animation_set
        })
        .and_then(|set_id| project.resource(set_id))
        .and_then(|resource| {
            let ResourceData::AnimationSet(set) = &resource.data else {
                return None;
            };
            Some(set.weapon_appearance_tracks.as_slice())
        })
        .unwrap_or_default()
        .iter()
        .map(|track| (track.action, track))
}

fn baked_clip_path_for_source(
    project: &ProjectDocument,
    source_id: ResourceId,
    target_model: Option<ResourceId>,
) -> Option<String> {
    project.resources.iter().find_map(|resource| {
        let ResourceData::AnimationClip(clip) = &resource.data else {
            return None;
        };
        (clip.source == Some(source_id)
            && target_model.is_none_or(|model| clip.target_model == Some(model)))
        .then(|| clip.psxanim_path.clone())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

impl FileStamp {
    fn read(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        Some(Self {
            path,
            len: metadata.len(),
            modified,
        })
    }
}

#[derive(Debug, Clone)]
struct CachedModelContext {
    resource: ResourceId,
    model_stamp: FileStamp,
    atlas_stamp: Option<FileStamp>,
    authored_rotation_q12: [u16; 3],
    world_height: u16,
    collision_radius: u16,
    visual_scale_q8: u16,
    context: Arc<LoadedModelContext>,
}

#[derive(Debug, Clone)]
struct CachedClipContext {
    path: String,
    stamp: FileStamp,
    pose_corrections: Vec<AnimationPoseCorrectionKey>,
    model_stamp: Option<FileStamp>,
    context: Arc<LoadedClipContext>,
}

#[derive(Clone)]
struct ViewerClipOption {
    label: String,
    path: String,
    origin: ClipOrigin,
    role: AnimationRole,
    looping: bool,
    resource: Option<ResourceId>,
    model_clip_index: Option<usize>,
    calibration: AnimationClipCalibration,
    previewable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipOrigin {
    Model,
    Library,
    Source,
}

impl ClipOrigin {
    const fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Library => "library",
            Self::Source => "source",
        }
    }
}

pub(crate) enum AnimationViewerAction {
    BakeSourceForModel {
        model_id: ResourceId,
        source_id: ResourceId,
    },
    ProjectChanged,
}

pub(crate) fn draw_model_animation_viewer_toolbar(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) -> Option<AnimationViewerAction> {
    state.ensure_selection(project);

    let character_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Character(_)));
    let model_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Model(_)));
    let clip_options = state
        .selected_model
        .map(|id| build_clip_options(project, id))
        .unwrap_or_default();
    let model_context = state
        .selected_model
        .and_then(|id| load_model_context_cached(project, project_root, state, id));
    let mut selected_clip = state
        .selected_clip_path
        .as_ref()
        .and_then(|path| clip_options.iter().find(|clip| clip.path == *path))
        .cloned();
    let clip_context = selected_clip.as_ref().and_then(|clip| {
        load_clip_context_cached(project, project_root, state, clip, model_context.as_deref())
    });

    if state.last_clip_path.as_deref() != state.selected_clip_path.as_deref() {
        state.frame = 0.0;
        state.last_clip_path = state.selected_clip_path.clone();
        state.last_time_seconds = ui.input(|input| input.time);
    }
    let selected_model = state.selected_model;

    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if resource_combo(
            ui,
            "Character",
            "animation-viewer-character",
            &mut state.selected_character,
            &character_options,
        ) {
            if let Some(character_id) = state.selected_character {
                state.selected_action = CharacterAnimationAction::Idle;
                state.selected_model = project.resource(character_id).and_then(|resource| {
                    let ResourceData::Character(character) = &resource.data else {
                        return None;
                    };
                    character.model
                });
                state.selected_clip_path =
                    character_action_clip_path(project, character_id, state.selected_action);
                state.clip_filter.clear();
                state.reset_clip_clock();
                state.invalidate_model_cache();
                state.invalidate_clip_cache();
                state.ensure_selection(project);
            }
        }
        if state.selected_character.is_none() {
            if resource_combo(
                ui,
                "Model",
                "animation-viewer-model",
                &mut state.selected_model,
                &model_options,
            ) {
                state.selected_clip_path = None;
                state.clip_filter.clear();
                state.reset_clip_clock();
                state.invalidate_model_cache();
                state.invalidate_clip_cache();
                state.ensure_selection(project);
            }
        }
        clip_combo(ui, state, &clip_options);
        ui.separator();
        action = draw_playback_controls(
            ui,
            state,
            selected_model,
            selected_clip.as_ref(),
            clip_context.as_ref().and_then(|clip| clip.animation_stats),
        );
        ui.separator();
        if draw_clip_calibration_menu(ui, project, selected_model, selected_clip.as_mut()) {
            preview_texture.take();
            if action.is_none() {
                action = Some(AnimationViewerAction::ProjectChanged);
            }
        }
        ui.separator();
        draw_preview_toolbar(ui, state, model_context.as_deref());
    });
    let character_available = state.selected_character.is_some_and(|id| {
        project
            .resource(id)
            .is_some_and(|resource| matches!(resource.data, ResourceData::Character(_)))
    });
    let pose_available = selected_clip.as_ref().is_some_and(|clip| {
        clip.resource.is_some_and(|id| {
            project
                .resource(id)
                .is_some_and(|resource| matches!(resource.data, ResourceData::AnimationClip(_)))
        })
    });
    let mut studio_mode = state.studio_mode();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Studio").color(STUDIO_TEXT_WEAK));
        ui.selectable_value(
            &mut studio_mode,
            AnimationStudioMode::Preview,
            icons::label(icons::PLAY, "Preview"),
        )
        .on_hover_text("Playback and scrub without authoring overlays");
        ui.add_enabled_ui(character_available, |ui| {
            ui.selectable_value(
                &mut studio_mode,
                AnimationStudioMode::Moveset,
                icons::label(icons::LAYERS, "Moveset"),
            )
            .on_hover_text(
                "Audit which gameplay actions are enabled, missing, or visually falling back",
            );
        });
        ui.add_enabled_ui(pose_available, |ui| {
            ui.selectable_value(
                &mut studio_mode,
                AnimationStudioMode::Pose,
                icons::label(icons::WAYPOINT, "Pose"),
            )
            .on_hover_text("Add sparse joint corrections at sampled frames");
        });
        ui.add_enabled_ui(state.selected_model.is_some(), |ui| {
            ui.selectable_value(
                &mut studio_mode,
                AnimationStudioMode::Weapon,
                icons::label(icons::MAP_PIN, "Weapon"),
            )
            .on_hover_text("Place sockets and grips, then author weapon visibility beats");
        });
        ui.add_enabled_ui(character_available, |ui| {
            ui.selectable_value(
                &mut studio_mode,
                AnimationStudioMode::Combat,
                icons::label(icons::SCAN, "Combat"),
            )
            .on_hover_text("Place hurtboxes, hitboxes, and projectile emitters");
        });
    });
    if (studio_mode == AnimationStudioMode::Moveset && !character_available)
        || (studio_mode == AnimationStudioMode::Pose && !pose_available)
        || (studio_mode == AnimationStudioMode::Weapon && state.selected_model.is_none())
        || (studio_mode == AnimationStudioMode::Combat && !character_available)
    {
        studio_mode = AnimationStudioMode::Preview;
    }
    state.set_studio_mode(studio_mode);
    action
}

pub(crate) fn draw_model_animation_viewer(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) -> bool {
    state.ensure_selection(project);
    let clip_options = state
        .selected_model
        .map(|id| build_clip_options(project, id))
        .unwrap_or_default();
    let model_context = state
        .selected_model
        .and_then(|id| load_model_context_cached(project, project_root, state, id));
    let selected_clip = state
        .selected_clip_path
        .as_ref()
        .and_then(|path| clip_options.iter().find(|clip| clip.path == *path))
        .cloned();
    let clip_context = selected_clip.as_ref().and_then(|clip| {
        load_clip_context_cached(project, project_root, state, clip, model_context.as_deref())
    });

    let material_layer = preview_material_layer_cached(project, project_root, state);
    let character_material =
        material_layer.as_ref().map(
            |(atlas, motion)| model_import_preview::PreviewMaterialLayer {
                atlas,
                motion: *motion,
            },
        );
    let character_id = state.selected_character.filter(|id| {
        project
            .resource(*id)
            .is_some_and(|resource| matches!(resource.data, ResourceData::Character(_)))
    });
    let capsules = character_id
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character.combat_capsules.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if state.selected_combat_capsule >= capsules.len() {
        state.selected_combat_capsule = capsules.len().saturating_sub(1);
    }
    let preview_capsules = if state.show_combat_capsules {
        capsules
            .iter()
            .enumerate()
            .map(
                |(index, capsule)| model_import_preview::PreviewCombatCapsule {
                    joint: capsule.joint,
                    start: capsule.capsule.start,
                    end: capsule.capsule.end,
                    radius: capsule.capsule.radius,
                    color: match capsule.role {
                        psxed_project::CombatCapsuleRole::Hurtbox => {
                            Color32::from_rgb(76, 196, 224)
                        }
                        psxed_project::CombatCapsuleRole::Hitbox { .. } => {
                            Color32::from_rgb(238, 102, 82)
                        }
                        psxed_project::CombatCapsuleRole::ProjectileEmitter { .. } => {
                            Color32::from_rgb(214, 118, 255)
                        }
                    },
                    selected: index == state.selected_combat_capsule,
                },
            )
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let pose_clip_id = state
        .show_pose_corrections
        .then(|| {
            selected_clip.as_ref().and_then(|clip| {
                clip.resource.filter(|id| {
                    project.resource(*id).is_some_and(|resource| {
                        matches!(resource.data, ResourceData::AnimationClip(_))
                    })
                })
            })
        })
        .flatten();
    let socket_model_id = state
        .show_attachment_sockets
        .then_some(state.selected_model)
        .flatten()
        .filter(|id| {
            project
                .resource(*id)
                .is_some_and(|resource| matches!(resource.data, ResourceData::Model(_)))
        });
    let sockets = socket_model_id
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model.attachments.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if state.selected_attachment_socket >= sockets.len() {
        state.selected_attachment_socket = sockets.len().saturating_sub(1);
    }
    let selected_weapon_track = if socket_model_id.is_some() {
        sync_weapon_studio_selection(project, state, character_id, &sockets)
    } else {
        None
    };
    let preview_socket_index = selected_weapon_track
        .as_ref()
        .and_then(|track| {
            sockets
                .iter()
                .position(|socket| socket.name == track.character_socket)
        })
        .unwrap_or(state.selected_attachment_socket);
    let preview_sockets = if socket_model_id.is_some() {
        sockets
            .iter()
            .enumerate()
            .map(|(index, socket)| model_import_preview::PreviewSocket {
                joint: socket.joint,
                translation: socket.translation,
                rotation_q12: socket.rotation_q12,
                selected: index == preview_socket_index,
                gizmo_mode: if state.capsule_edit_tool == CapsuleEditTool::Rotate {
                    model_import_preview::PreviewGizmoMode::Rotate
                } else {
                    model_import_preview::PreviewGizmoMode::Translate
                },
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    // The equipped-weapon overlay rides the SELECTED socket, the same
    // authoring loop the runtime resolves by name at cook time.
    let weapon_grip = socket_model_id
        .and(state.preview_weapon)
        .and_then(|weapon_id| project.resource(weapon_id))
        .and_then(|resource| match &resource.data {
            ResourceData::Weapon(weapon) => weapon
                .model
                .map(|model_id| (model_id, weapon.grip.translation, weapon.grip.rotation_q12)),
            _ => None,
        })
        .zip(sockets.get(preview_socket_index).cloned());
    let weapon_model_context = weapon_grip.as_ref().and_then(|((model_id, _, _), _)| {
        load_weapon_model_context(project, project_root, state, *model_id)
    });
    let weapon_fallback_atlas = ColorImage {
        size: [4, 4],
        pixels: vec![Color32::from_rgb(168, 168, 176); 16],
    };
    let weapon_materialization_q12 = selected_weapon_track
        .as_ref()
        .map(|track| {
            preview_weapon_materialization_q12(
                track,
                state.frame,
                clip_context
                    .as_ref()
                    .and_then(|clip| clip.animation_stats)
                    .map(|stats| stats.frame_count)
                    .unwrap_or(1),
            )
        })
        .unwrap_or(4096);
    let equipped_weapon = weapon_grip.as_ref().zip(weapon_model_context.as_ref()).map(
        |(((_, grip_translation, grip_rotation_q12), socket), context)| {
            model_import_preview::PreviewEquippedWeapon {
                model_bytes: &context.model_bytes,
                atlas: context.atlas.as_ref().unwrap_or(&weapon_fallback_atlas),
                socket_joint: socket.joint,
                socket_translation: socket.translation,
                socket_rotation_q12: socket.rotation_q12,
                grip_translation: *grip_translation,
                grip_rotation_q12: *grip_rotation_q12,
                materialization_q12: weapon_materialization_q12,
                wireframe_materialization: selected_weapon_track.is_some(),
            }
        },
    );
    let selected_joint = if state.show_pose_corrections {
        Some(state.selected_pose_joint)
    } else if socket_model_id.is_some() {
        sockets.get(preview_socket_index).map(|socket| socket.joint)
    } else {
        state
            .show_combat_capsules
            .then(|| {
                capsules
                    .get(state.selected_combat_capsule)
                    .map(|capsule| capsule.joint)
            })
            .flatten()
    };
    let joint_picking = (state.show_combat_capsules && character_id.is_some())
        || pose_clip_id.is_some()
        || socket_model_id.is_some();
    let moveset_open = state.show_moveset && character_id.is_some();
    let authoring_panel_open = joint_picking || moveset_open;
    let assigned_action_clip = character_id.and_then(|character_id| {
        capsules
            .get(state.selected_combat_capsule)
            .and_then(|capsule| match capsule.role {
                psxed_project::CombatCapsuleRole::Hitbox { action, .. }
                | psxed_project::CombatCapsuleRole::ProjectileEmitter { action, .. } => {
                    Some(action)
                }
                psxed_project::CombatCapsuleRole::Hurtbox => None,
            })
            .and_then(|action| assigned_action_clip(project, character_id, action, &clip_options))
    });

    let total_height = ui.available_height();
    let max_timeline_height = (total_height - TIMELINE_MIN_PREVIEW_HEIGHT - 7.0)
        .max(TIMELINE_MIN_HEIGHT.min(total_height * 0.45));
    state.timeline_height = state.timeline_height.clamp(
        TIMELINE_MIN_HEIGHT.min(max_timeline_height),
        max_timeline_height,
    );
    let preview_height = (total_height - state.timeline_height - 7.0).max(120.0);

    let mut preview_interaction = PreviewInteraction::default();
    let mut editor_changed = false;
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), preview_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            // `allocate_ui_with_layout` constrains the child, but advances the
            // parent by the child's used height. Reserve the full preview slot
            // so the bottom timeline stays docked instead of leaving dead space
            // beneath it when the rendered image preserves a smaller aspect ratio.
            ui.set_min_height(preview_height);
            if authoring_panel_open {
                ui.horizontal(|ui| {
                    ui.set_min_height(preview_height);
                    let editor_width = if moveset_open {
                        500.0f32.min((ui.available_width() * 0.48).max(360.0))
                    } else {
                        310.0f32.min((ui.available_width() * 0.38).max(260.0))
                    };
                    // Reserve the separator plus both horizontal item gaps.
                    // Subtracting only one gap placed the authoring panel a
                    // few pixels beyond the CentralPanel clip rectangle: its
                    // labels were painted, but controls near the right edge
                    // could not receive pointer input.
                    let split_gutter = ui.spacing().item_spacing.x * 2.0 + 8.0;
                    let preview_width =
                        (ui.available_width() - editor_width - split_gutter).max(360.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(preview_width, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            preview_interaction = draw_preview(
                                ui,
                                state,
                                model_context.as_deref(),
                                selected_clip.as_ref(),
                                clip_context.as_deref(),
                                preview_texture,
                                &preview_capsules,
                                &preview_sockets,
                                equipped_weapon.as_ref(),
                                character_material.as_ref(),
                                selected_joint,
                                joint_picking,
                            );
                        },
                    );
                    ui.separator();
                    ui.allocate_ui_with_layout(
                        Vec2::new(editor_width, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("animation-studio-authoring-panel")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if let Some(character_id) =
                                        moveset_open.then_some(character_id).flatten()
                                    {
                                        draw_moveset_capability_matrix(
                                            ui,
                                            project,
                                            character_id,
                                            state,
                                            &clip_options,
                                        );
                                    } else if let Some(clip_id) = pose_clip_id {
                                        let joint_count = model_context
                                            .as_deref()
                                            .and_then(|model| {
                                                psx_asset::Model::from_bytes(&model.model_bytes)
                                                    .ok()
                                            })
                                            .map(|model| model.joint_count())
                                            .unwrap_or(0);
                                        let max_frame = clip_context
                                            .as_ref()
                                            .and_then(|clip| clip.animation_stats)
                                            .map(|stats| stats.frame_count.saturating_sub(1))
                                            .unwrap_or(0);
                                        editor_changed |= draw_pose_correction_editor(
                                            ui,
                                            project,
                                            clip_id,
                                            state,
                                            joint_count,
                                            max_frame,
                                        );
                                    } else if let Some(model_id) = socket_model_id {
                                        editor_changed |= draw_attachment_socket_editor(
                                            ui,
                                            project,
                                            model_id,
                                            character_id,
                                            state,
                                            model_context.as_deref(),
                                            clip_context
                                                .as_ref()
                                                .and_then(|clip| clip.animation_stats)
                                                .map(|stats| stats.frame_count.saturating_sub(1))
                                                .unwrap_or(0),
                                        );
                                    } else if let Some(character_id) = character_id {
                                        editor_changed |= draw_combat_capsule_editor(
                                            ui,
                                            project,
                                            character_id,
                                            state,
                                            model_context.as_deref(),
                                            assigned_action_clip.as_ref(),
                                        );
                                    }
                                });
                        },
                    );
                });
            } else {
                preview_interaction = draw_preview(
                    ui,
                    state,
                    model_context.as_deref(),
                    selected_clip.as_ref(),
                    clip_context.as_deref(),
                    preview_texture,
                    &[],
                    &[],
                    None,
                    character_material.as_ref(),
                    None,
                    false,
                );
            }
        },
    );

    draw_timeline_splitter(ui, state, max_timeline_height);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), state.timeline_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_height(state.timeline_height);
            editor_changed |= draw_animation_timeline(
                ui,
                project,
                state,
                character_id,
                &clip_options,
                selected_clip.as_ref(),
                clip_context.as_ref().and_then(|clip| clip.animation_stats),
            );
        },
    );

    let mut changed = false;
    if let Some(joint) = preview_interaction.clicked_joint {
        if state.show_pose_corrections {
            state.selected_pose_joint = joint;
        } else if let Some(model_id) = socket_model_id {
            changed |= attach_selected_socket_to_joint(
                project,
                model_id,
                state.selected_attachment_socket,
                joint,
            );
            preview_texture.take();
        } else if let Some(character_id) = character_id {
            changed |= attach_selected_capsule_to_joint(
                project,
                character_id,
                state.selected_combat_capsule,
                joint,
                model_context.as_deref(),
            );
            preview_texture.take();
        }
    }
    if let Some(clip_id) = pose_clip_id {
        let pose_delta = match state.capsule_edit_tool {
            CapsuleEditTool::Move => preview_interaction
                .gizmo_move_units
                .map(AxisEditDelta::Translate),
            CapsuleEditTool::Rotate => preview_interaction.edit_delta.map(AxisEditDelta::Rotate),
            CapsuleEditTool::Resize => None,
        };
        if let Some(delta) = pose_delta {
            let frame = state.frame.round().max(0.0) as u16;
            changed |= manipulate_pose_correction(
                project,
                clip_id,
                state.selected_pose_joint,
                frame,
                state.capsule_edit_axis,
                delta,
            );
            preview_texture.take();
        }
    } else if let Some(model_id) = socket_model_id {
        let socket_delta = match state.capsule_edit_tool {
            CapsuleEditTool::Move => preview_interaction
                .gizmo_move_units
                .map(AxisEditDelta::Translate),
            CapsuleEditTool::Rotate => preview_interaction.edit_delta.map(AxisEditDelta::Rotate),
            CapsuleEditTool::Resize => None,
        };
        if let Some(delta) = socket_delta {
            changed |= manipulate_selected_socket(
                project,
                model_id,
                state.selected_attachment_socket,
                state.capsule_edit_axis,
                delta,
            );
            preview_texture.take();
        }
    } else if let (Some(character_id), Some(delta)) = (character_id, preview_interaction.edit_delta)
    {
        changed |= manipulate_selected_capsule(
            project,
            character_id,
            state.selected_combat_capsule,
            state.capsule_edit_tool,
            state.capsule_edit_axis,
            delta,
        );
        preview_texture.take();
    }
    // The editor mutates the project in place and reports whether persistence
    // state must be updated by the workspace.
    changed || editor_changed
}

#[derive(Debug, Clone, Copy)]
struct TimelineActionContext {
    animation_set: ResourceId,
    clip: ResourceId,
    options: CharacterActionOptions,
}

#[derive(Debug, Clone)]
struct TimelineHitbox {
    index: usize,
    name: String,
    start: u16,
    end: u16,
}

#[derive(Debug, Clone)]
struct TimelineWeaponTrack {
    index: usize,
    name: String,
    socket: String,
    fully_visible_frame: u16,
    hidden_frame: u16,
    transition_frames: u16,
}

fn character_animation_set_id(
    project: &ProjectDocument,
    character_id: Option<ResourceId>,
) -> Option<ResourceId> {
    project.resource(character_id?).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        character.animation_set
    })
}

fn sync_weapon_studio_selection(
    project: &ProjectDocument,
    state: &mut ModelAnimationViewerState,
    character_id: Option<ResourceId>,
    sockets: &[psxed_project::AttachmentSocket],
) -> Option<psxed_project::WeaponAppearanceTrack> {
    let set_id = character_animation_set_id(project, character_id)?;
    let set = project.resource(set_id).and_then(|resource| {
        let ResourceData::AnimationSet(set) = &resource.data else {
            return None;
        };
        Some(set)
    })?;
    let indices = set
        .weapon_appearance_tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| (track.action == state.selected_action).then_some(index))
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return None;
    }
    if !indices.contains(&state.selected_weapon_track) {
        state.selected_weapon_track = indices[0];
    }
    let track = set
        .weapon_appearance_tracks
        .get(state.selected_weapon_track)?
        .clone();
    state.preview_weapon = Some(track.weapon);
    if let Some(index) = sockets
        .iter()
        .position(|socket| socket.name == track.character_socket)
    {
        state.selected_attachment_socket = index;
    }
    Some(track)
}

fn preview_weapon_materialization_q12(
    track: &psxed_project::WeaponAppearanceTrack,
    frame: f32,
    frame_count: u16,
) -> u16 {
    let phase_q12 = (frame.max(0.0) * 4096.0).round() as u32;
    let last_frame = frame_count.saturating_sub(1);
    let hidden = if track.hidden_frame == psxed_project::ACTION_FRAME_END_FULL {
        last_frame
    } else {
        track.hidden_frame.min(last_frame)
    };
    let visible_q12 = u32::from(track.fully_visible_frame) << 12;
    let hidden_q12 = u32::from(hidden) << 12;
    if track.transition_frames == 0 {
        return if phase_q12 >= visible_q12 && phase_q12 < hidden_q12 {
            4096
        } else {
            0
        };
    }
    let transition = u32::from(track.transition_frames);
    let start_q12 = visible_q12.saturating_sub(transition << 12);
    if phase_q12 < start_q12 || phase_q12 >= hidden_q12 {
        return 0;
    }
    let rising = (phase_q12 - start_q12) / transition;
    let falling = (hidden_q12 - phase_q12) / transition;
    rising.min(falling).min(4096) as u16
}

fn draw_timeline_splitter(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    max_timeline_height: f32,
) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 7.0),
        Sense::click_and_drag(),
    );
    let color = if response.hovered() || response.dragged() {
        STUDIO_ACCENT
    } else {
        STUDIO_BORDER_DARK
    };
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        Stroke::new(if response.dragged() { 2.0 } else { 1.0 }, color),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if response.drag_started() {
        state.timeline_resize_origin = Some(state.timeline_height);
    }
    if response.dragged() {
        let origin = state
            .timeline_resize_origin
            .unwrap_or(state.timeline_height);
        state.timeline_height = (origin - response.drag_delta().y).clamp(
            TIMELINE_MIN_HEIGHT.min(max_timeline_height),
            max_timeline_height,
        );
    }
    if response.drag_stopped() {
        state.timeline_resize_origin = None;
    }
}

fn draw_animation_timeline(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    state: &mut ModelAnimationViewerState,
    character_id: Option<ResourceId>,
    clip_options: &[ViewerClipOption],
    selected_clip: Option<&ViewerClipOption>,
    animation: Option<LoadedAnimationStats>,
) -> bool {
    let available = Vec2::new(ui.available_width(), ui.available_height());
    let mut changed = false;
    egui::Frame::new()
        .fill(STUDIO_DOCK)
        .stroke(Stroke::new(1.0, STUDIO_BORDER_DARK))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |ui| {
            ui.set_min_size(available);

            if state.studio_mode() != AnimationStudioMode::Moveset {
                if let (Some(character_id), Some(clip_resource)) =
                    (character_id, selected_clip.and_then(|clip| clip.resource))
                {
                    if let Some(mapped) =
                        timeline_action_for_clip(project, character_id, clip_resource)
                    {
                        state.selected_action = mapped;
                    }
                }
            }

            let mut action_changed = false;
            egui::Frame::new()
                .fill(STUDIO_PANEL_HEADER)
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Timeline").strong());
                        ui.label(
                            RichText::new("sampled frames")
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                        );
                        if character_id.is_some() {
                            ui.separator();
                            ui.label(RichText::new("Action").color(STUDIO_TEXT_WEAK));
                            egui::ComboBox::from_id_salt("animation-timeline-action")
                                .selected_text(state.selected_action.label())
                                .width(126.0)
                                .show_ui(ui, |ui| {
                                    for action in CharacterAnimationAction::ALL {
                                        action_changed |= ui
                                            .selectable_value(
                                                &mut state.selected_action,
                                                action,
                                                action.label(),
                                            )
                                            .changed();
                                    }
                                });
                        }
                        if let Some(animation) = animation {
                            ui.separator();
                            ui.label(
                                RichText::new(format!(
                                    "{} frames  ·  {} Hz",
                                    animation.frame_count, animation.sample_rate_hz
                                ))
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Fit").clicked() {
                                let frames = animation
                                    .map(|animation| animation.frame_count.saturating_sub(1).max(1))
                                    .unwrap_or(1);
                                let canvas =
                                    (available.x - TIMELINE_TRACK_LABEL_WIDTH - 24.0).max(120.0);
                                state.timeline_pixels_per_frame =
                                    (canvas / frames as f32).clamp(2.0, 40.0);
                            }
                            if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                                state.timeline_pixels_per_frame =
                                    (state.timeline_pixels_per_frame * 1.25).min(40.0);
                            }
                            if ui.small_button("−").on_hover_text("Zoom out").clicked() {
                                state.timeline_pixels_per_frame =
                                    (state.timeline_pixels_per_frame / 1.25).max(2.0);
                            }
                        });
                    });
                });

            if action_changed {
                if let Some(context) = character_id
                    .and_then(|id| timeline_action_context(project, id, state.selected_action))
                {
                    if let Some(clip) = clip_options
                        .iter()
                        .find(|clip| clip.resource == Some(context.clip) && clip.previewable)
                    {
                        state.selected_clip_path = Some(clip.path.clone());
                        state.invalidate_clip_cache();
                        state.reset_clip_clock();
                    }
                }
            }

            let Some(animation) = animation else {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Bake or select a cooked clip to expose its sampled frames.")
                            .color(STUDIO_TEXT_WEAK),
                    );
                });
                return;
            };

            let max_frame = animation.frame_count.saturating_sub(1);
            let action_max_frame = animation.frame_count.saturating_sub(2);
            let selected_clip_resource = selected_clip.and_then(|clip| clip.resource);
            let action_context = character_id
                .and_then(|id| timeline_action_context(project, id, state.selected_action))
                .filter(|context| selected_clip_resource == Some(context.clip));
            let hitboxes = character_id
                .map(|id| timeline_hitboxes(project, id, state.selected_action))
                .unwrap_or_default();
            let weapon_tracks = character_id
                .map(|id| timeline_weapon_tracks(project, id, state.selected_action))
                .unwrap_or_default();
            let pose_track = state.show_pose_corrections
                && selected_clip_resource.is_some_and(|id| {
                    project.resource(id).is_some_and(|resource| {
                        matches!(resource.data, ResourceData::AnimationClip(_))
                    })
                });
            let pose_key_frames = selected_clip_resource
                .and_then(|id| project.resource(id))
                .and_then(|resource| match &resource.data {
                    ResourceData::AnimationClip(clip) => Some(
                        clip.pose_corrections
                            .iter()
                            .filter_map(|key| {
                                (key.joint == state.selected_pose_joint).then_some(key.frame)
                            })
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();

            let canvas_available =
                (ui.available_width() - TIMELINE_TRACK_LABEL_WIDTH - 2.0).max(180.0);
            let content_width = canvas_available
                .max(24.0 + max_frame.max(1) as f32 * state.timeline_pixels_per_frame);
            let track_count = 1
                + usize::from(pose_track)
                + usize::from(action_context.is_some()) * 2
                + weapon_tracks.len()
                + hitboxes.len();
            let content_height =
                TIMELINE_RULER_HEIGHT + track_count.max(1) as f32 * TIMELINE_TRACK_HEIGHT;
            let pose_key_detail = format!("{} baked pose keys", animation.frame_count);

            let mut action_range_update = None;
            let mut push_range_update = None;
            let mut weapon_updates = Vec::new();
            let mut hitbox_updates = Vec::new();

            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(TIMELINE_TRACK_LABEL_WIDTH, content_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        timeline_track_label(ui, "TRACKS", "", STUDIO_TEXT_WEAK, false);
                        timeline_track_label(
                            ui,
                            "Imported motion",
                            &pose_key_detail,
                            STUDIO_ACCENT,
                            false,
                        );
                        if pose_track {
                            timeline_track_label(
                                ui,
                                "Pose correction",
                                &format!(
                                    "Joint {} · {} keys",
                                    state.selected_pose_joint,
                                    pose_key_frames.len()
                                ),
                                Color32::from_rgb(174, 116, 232),
                                true,
                            );
                        }
                        if action_context.is_some() {
                            timeline_track_label(
                                ui,
                                "Action range",
                                state.selected_action.label(),
                                STUDIO_ACCENT,
                                false,
                            );
                            timeline_track_label(
                                ui,
                                "Root push",
                                "authored movement",
                                Color32::from_rgb(220, 171, 78),
                                false,
                            );
                        }
                        for track in &weapon_tracks {
                            let hidden =
                                if track.hidden_frame == psxed_project::ACTION_FRAME_END_FULL {
                                    "clip end".to_string()
                                } else {
                                    format!("frame {}", track.hidden_frame)
                                };
                            let response = timeline_track_label(
                                ui,
                                &track.name,
                                &format!(
                                    "{} · visible {} · gone {} · {}f transition",
                                    track.socket,
                                    track.fully_visible_frame,
                                    hidden,
                                    track.transition_frames,
                                ),
                                Color32::from_rgb(86, 224, 156),
                                state.show_attachment_sockets
                                    && state.selected_weapon_track == track.index,
                            );
                            if response.clicked() {
                                state.selected_weapon_track = track.index;
                                state.set_studio_mode(AnimationStudioMode::Weapon);
                            }
                        }
                        for hitbox in &hitboxes {
                            let response = timeline_track_label(
                                ui,
                                &hitbox.name,
                                "damage window",
                                Color32::from_rgb(238, 102, 82),
                                state.selected_combat_capsule == hitbox.index,
                            );
                            if response.clicked() {
                                state.selected_combat_capsule = hitbox.index;
                                state.set_studio_mode(AnimationStudioMode::Combat);
                            }
                        }
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    Vec2::new(canvas_available, content_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("animation-timeline-scroll")
                            .max_height(content_height)
                            .min_scrolled_height(content_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(content_width);
                                if let Some(frame) = draw_timeline_ruler(
                                    ui,
                                    content_width,
                                    max_frame,
                                    state.timeline_pixels_per_frame,
                                    state.frame,
                                ) {
                                    state.frame = frame as f32;
                                    state.playing = false;
                                }
                                if let Some(frame) = draw_timeline_pose_keys_lane(
                                    ui,
                                    content_width,
                                    max_frame,
                                    state.timeline_pixels_per_frame,
                                    state.frame,
                                ) {
                                    state.frame = frame as f32;
                                    state.playing = false;
                                }

                                if pose_track {
                                    if let Some(frame) = draw_timeline_sparse_keys_lane(
                                        ui,
                                        content_width,
                                        max_frame,
                                        state.timeline_pixels_per_frame,
                                        state.frame,
                                        &pose_key_frames,
                                        Color32::from_rgb(174, 116, 232),
                                    ) {
                                        state.frame = frame as f32;
                                        state.playing = false;
                                    }
                                }

                                if let Some(context) = action_context {
                                    let mut action_start =
                                        context.options.frame_start.min(action_max_frame);
                                    let mut action_end = if context.options.frame_end
                                        == psxed_project::ACTION_FRAME_END_FULL
                                    {
                                        action_max_frame
                                    } else {
                                        context.options.frame_end.min(action_max_frame)
                                    }
                                    .max(action_start);
                                    let action_response = draw_timeline_range_lane(
                                        ui,
                                        "animation-timeline-action-range",
                                        content_width,
                                        action_max_frame,
                                        state.timeline_pixels_per_frame,
                                        state.frame,
                                        action_start,
                                        action_end,
                                        STUDIO_ACCENT,
                                        true,
                                    );
                                    if let Some(frame) = action_response.seek_frame {
                                        state.frame = frame as f32;
                                        state.playing = false;
                                    }
                                    if let Some((start, end)) = action_response.range {
                                        action_start = start;
                                        action_end = end;
                                        action_range_update =
                                            Some((context, action_start, action_end));
                                    }

                                    let push_start =
                                        context.options.push_frame_start.min(action_max_frame);
                                    let push_end = if context.options.push_frame_end
                                        == psxed_project::ACTION_FRAME_END_FULL
                                    {
                                        action_max_frame
                                    } else {
                                        context.options.push_frame_end.min(action_max_frame)
                                    }
                                    .max(push_start);
                                    let push_response = draw_timeline_range_lane(
                                        ui,
                                        "animation-timeline-push-range",
                                        content_width,
                                        action_max_frame,
                                        state.timeline_pixels_per_frame,
                                        state.frame,
                                        push_start,
                                        push_end,
                                        Color32::from_rgb(220, 171, 78),
                                        true,
                                    );
                                    if let Some(frame) = push_response.seek_frame {
                                        state.frame = frame as f32;
                                        state.playing = false;
                                    }
                                    if let Some((start, end)) = push_response.range {
                                        push_range_update = Some((context, start, end));
                                    }
                                }

                                for track in &weapon_tracks {
                                    let hidden = if track.hidden_frame
                                        == psxed_project::ACTION_FRAME_END_FULL
                                    {
                                        action_max_frame
                                    } else {
                                        track.hidden_frame.min(action_max_frame)
                                    }
                                    .max(track.fully_visible_frame.min(action_max_frame));
                                    let response = draw_timeline_range_lane(
                                        ui,
                                        &format!("animation-timeline-weapon-{}", track.index),
                                        content_width,
                                        action_max_frame,
                                        state.timeline_pixels_per_frame,
                                        state.frame,
                                        track.fully_visible_frame.min(action_max_frame),
                                        hidden,
                                        Color32::from_rgb(86, 224, 156),
                                        true,
                                    );
                                    if let Some(frame) = response.seek_frame {
                                        state.frame = frame as f32;
                                        state.playing = false;
                                        state.selected_weapon_track = track.index;
                                    }
                                    if let Some((start, end)) = response.range {
                                        weapon_updates.push((track.index, start, end));
                                        state.selected_weapon_track = track.index;
                                    }
                                }

                                for hitbox in &hitboxes {
                                    let response = draw_timeline_range_lane(
                                        ui,
                                        &format!("animation-timeline-hitbox-{}", hitbox.index),
                                        content_width,
                                        action_max_frame,
                                        state.timeline_pixels_per_frame,
                                        state.frame,
                                        hitbox.start.min(action_max_frame),
                                        hitbox
                                            .end
                                            .min(action_max_frame)
                                            .max(hitbox.start.min(action_max_frame)),
                                        Color32::from_rgb(238, 102, 82),
                                        true,
                                    );
                                    if let Some(frame) = response.seek_frame {
                                        state.frame = frame as f32;
                                        state.playing = false;
                                        state.selected_combat_capsule = hitbox.index;
                                    }
                                    if let Some((start, end)) = response.range {
                                        hitbox_updates.push((hitbox.index, start, end));
                                        state.selected_combat_capsule = hitbox.index;
                                    }
                                }
                            });
                    },
                );
            });

            if let Some((context, start, end)) = action_range_update {
                let mut options = context.options;
                options.frame_start = start;
                options.frame_end = if end == action_max_frame {
                    psxed_project::ACTION_FRAME_END_FULL
                } else {
                    end
                };
                changed |=
                    store_timeline_action_options(project, context, state.selected_action, options);
            }
            if let Some((context, start, end)) = push_range_update {
                let mut options = context.options;
                options.push_frame_start = start;
                options.push_frame_end = if end == action_max_frame {
                    psxed_project::ACTION_FRAME_END_FULL
                } else {
                    end
                };
                changed |=
                    store_timeline_action_options(project, context, state.selected_action, options);
            }
            for (index, start, end) in weapon_updates {
                changed |= store_timeline_weapon_range(
                    project,
                    character_id,
                    index,
                    state.selected_action,
                    start,
                    if end == action_max_frame {
                        psxed_project::ACTION_FRAME_END_FULL
                    } else {
                        end
                    },
                );
            }
            for (index, start, end) in hitbox_updates {
                changed |= store_timeline_hitbox_range(
                    project,
                    character_id,
                    index,
                    state.selected_action,
                    start,
                    end,
                );
            }
        });
    changed
}

fn timeline_track_label(
    ui: &mut egui::Ui,
    title: &str,
    detail: &str,
    color: Color32,
    selected: bool,
) -> egui::Response {
    let height = if title == "TRACKS" {
        TIMELINE_RULER_HEIGHT
    } else {
        TIMELINE_TRACK_HEIGHT
    };
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(TIMELINE_TRACK_LABEL_WIDTH, height),
        Sense::click(),
    );
    let fill = if selected {
        STUDIO_SELECTION
    } else if response.hovered() {
        STUDIO_HOVER
    } else {
        STUDIO_PANEL_DARK
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, STUDIO_BORDER_DARK),
    );
    ui.painter().text(
        egui::pos2(
            rect.left() + 9.0,
            rect.center().y - if detail.is_empty() { 0.0 } else { 6.0 },
        ),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(if title == "TRACKS" { 10.0 } else { 12.0 }),
        if title == "TRACKS" {
            STUDIO_TEXT_WEAK
        } else {
            STUDIO_TEXT
        },
    );
    if !detail.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + 9.0, rect.center().y + 7.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(9.5),
            color,
        );
    }
    response
}

fn timeline_x(rect: Rect, frame: u16, pixels_per_frame: f32) -> f32 {
    rect.left() + 12.0 + frame as f32 * pixels_per_frame
}

fn timeline_frame_at(rect: Rect, x: f32, max_frame: u16, pixels_per_frame: f32) -> u16 {
    let frame = ((x - rect.left() - 12.0) / pixels_per_frame.max(1.0)).round() as i32;
    frame.clamp(0, i32::from(max_frame)) as u16
}

fn draw_timeline_ruler(
    ui: &mut egui::Ui,
    width: f32,
    max_frame: u16,
    pixels_per_frame: f32,
    playhead: f32,
) -> Option<u16> {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, TIMELINE_RULER_HEIGHT), Sense::click());
    ui.painter().rect_filled(rect, 0.0, STUDIO_INPUT);
    let tick_step = if pixels_per_frame >= 22.0 {
        1
    } else if pixels_per_frame >= 10.0 {
        5
    } else if pixels_per_frame >= 5.0 {
        10
    } else {
        20
    };
    for frame in (0..=max_frame).step_by(tick_step) {
        let x = timeline_x(rect, frame, pixels_per_frame);
        ui.painter().line_segment(
            [
                egui::pos2(x, rect.bottom() - 7.0),
                egui::pos2(x, rect.bottom()),
            ],
            Stroke::new(1.0, STUDIO_BORDER),
        );
        ui.painter().text(
            egui::pos2(x + 3.0, rect.top() + 5.0),
            Align2::LEFT_TOP,
            frame.to_string(),
            FontId::monospace(9.0),
            STUDIO_TEXT_WEAK,
        );
    }
    let playhead_x = timeline_x(rect, playhead.round().max(0.0) as u16, pixels_per_frame);
    ui.painter().line_segment(
        [
            egui::pos2(playhead_x, rect.top()),
            egui::pos2(playhead_x, rect.bottom()),
        ],
        Stroke::new(2.0, STUDIO_ACCENT),
    );
    if response.clicked() {
        response
            .interact_pointer_pos()
            .map(|pointer| timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame))
    } else {
        None
    }
}

fn draw_timeline_pose_keys_lane(
    ui: &mut egui::Ui,
    width: f32,
    max_frame: u16,
    pixels_per_frame: f32,
    playhead: f32,
) -> Option<u16> {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, TIMELINE_TRACK_HEIGHT),
        Sense::click_and_drag(),
    );
    ui.painter().rect_filled(rect, 0.0, STUDIO_PANEL_DARK);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, STUDIO_BORDER_DARK),
    );
    let rail_y = rect.center().y;
    ui.painter().line_segment(
        [
            egui::pos2(timeline_x(rect, 0, pixels_per_frame), rail_y),
            egui::pos2(timeline_x(rect, max_frame, pixels_per_frame), rail_y),
        ],
        Stroke::new(2.0, STUDIO_ACCENT_DIM),
    );

    // At close zoom every baked pose sample is a discrete key. When zoomed
    // far out, coalesce markers only enough to keep them visually separable;
    // pointer scrubbing still addresses every source frame.
    let marker_step = (6.0 / pixels_per_frame.max(1.0)).ceil().max(1.0) as usize;
    let selected_frame = playhead.round().clamp(0.0, max_frame as f32) as u16;
    for frame in (0..=max_frame).step_by(marker_step) {
        let center = egui::pos2(timeline_x(rect, frame, pixels_per_frame), rail_y);
        let selected = frame == selected_frame;
        let radius = if selected { 5.0 } else { 3.5 };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(center.x, center.y - radius),
                egui::pos2(center.x + radius, center.y),
                egui::pos2(center.x, center.y + radius),
                egui::pos2(center.x - radius, center.y),
            ],
            if selected { STUDIO_TEXT } else { STUDIO_ACCENT },
            Stroke::new(1.0, STUDIO_PANEL_DARK),
        ));
    }

    let playhead_x = timeline_x(rect, selected_frame, pixels_per_frame);
    ui.painter().line_segment(
        [
            egui::pos2(playhead_x, rect.top()),
            egui::pos2(playhead_x, rect.bottom()),
        ],
        Stroke::new(1.0, STUDIO_ACCENT),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() || response.dragged() {
        response
            .interact_pointer_pos()
            .map(|pointer| timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame))
    } else {
        None
    }
}

fn draw_timeline_sparse_keys_lane(
    ui: &mut egui::Ui,
    width: f32,
    max_frame: u16,
    pixels_per_frame: f32,
    playhead: f32,
    key_frames: &[u16],
    color: Color32,
) -> Option<u16> {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, TIMELINE_TRACK_HEIGHT),
        Sense::click_and_drag(),
    );
    ui.painter().rect_filled(rect, 0.0, STUDIO_PANEL_DARK);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, STUDIO_BORDER_DARK),
    );
    let rail_y = rect.center().y;
    ui.painter().line_segment(
        [
            egui::pos2(timeline_x(rect, 0, pixels_per_frame), rail_y),
            egui::pos2(timeline_x(rect, max_frame, pixels_per_frame), rail_y),
        ],
        Stroke::new(1.0, STUDIO_BORDER),
    );
    let selected_frame = playhead.round().clamp(0.0, max_frame as f32) as u16;
    for frame in key_frames.iter().copied() {
        let center = egui::pos2(
            timeline_x(rect, frame.min(max_frame), pixels_per_frame),
            rail_y,
        );
        let radius = if frame == selected_frame { 6.0 } else { 4.5 };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(center.x, center.y - radius),
                egui::pos2(center.x + radius, center.y),
                egui::pos2(center.x, center.y + radius),
                egui::pos2(center.x - radius, center.y),
            ],
            if frame == selected_frame {
                STUDIO_TEXT
            } else {
                color
            },
            Stroke::new(1.0, STUDIO_PANEL_DARK),
        ));
    }
    let playhead_x = timeline_x(rect, selected_frame, pixels_per_frame);
    ui.painter().line_segment(
        [
            egui::pos2(playhead_x, rect.top()),
            egui::pos2(playhead_x, rect.bottom()),
        ],
        Stroke::new(1.0, STUDIO_ACCENT),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() || response.dragged() {
        response
            .interact_pointer_pos()
            .map(|pointer| timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame))
    } else {
        None
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct TimelineLaneResponse {
    seek_frame: Option<u16>,
    range: Option<(u16, u16)>,
}

fn draw_timeline_range_lane(
    ui: &mut egui::Ui,
    id_salt: &str,
    width: f32,
    max_frame: u16,
    pixels_per_frame: f32,
    playhead: f32,
    start: u16,
    end: u16,
    color: Color32,
    editable: bool,
) -> TimelineLaneResponse {
    let id = ui.make_persistent_id(id_salt);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, TIMELINE_TRACK_HEIGHT),
        Sense::click_and_drag(),
    );
    ui.painter().rect_filled(rect, 0.0, STUDIO_PANEL_DARK);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, STUDIO_BORDER_DARK),
    );
    let rail = Rect::from_min_max(
        egui::pos2(timeline_x(rect, 0, pixels_per_frame), rect.center().y - 7.0),
        egui::pos2(
            timeline_x(rect, max_frame, pixels_per_frame).max(rect.left() + 13.0),
            rect.center().y + 7.0,
        ),
    );
    ui.painter().rect_filled(rail, 3.0, STUDIO_INPUT);
    let start_x = timeline_x(rect, start.min(max_frame), pixels_per_frame);
    let end_x = timeline_x(rect, end.min(max_frame), pixels_per_frame);
    let active = Rect::from_min_max(
        egui::pos2(start_x.min(end_x), rail.top()),
        egui::pos2((start_x.max(end_x) + 1.0).min(rail.right()), rail.bottom()),
    );
    ui.painter().rect_filled(active, 3.0, color);
    if editable {
        for x in [start_x, end_x] {
            ui.painter().line_segment(
                [
                    egui::pos2(x, rail.top() - 3.0),
                    egui::pos2(x, rail.bottom() + 3.0),
                ],
                Stroke::new(2.0, STUDIO_TEXT),
            );
        }
    }
    let playhead_x = timeline_x(rect, playhead.round().max(0.0) as u16, pixels_per_frame);
    ui.painter().line_segment(
        [
            egui::pos2(playhead_x, rect.top()),
            egui::pos2(playhead_x, rect.bottom()),
        ],
        Stroke::new(1.0, STUDIO_ACCENT),
    );

    let mut result = TimelineLaneResponse::default();
    if response.clicked() {
        result.seek_frame = response
            .interact_pointer_pos()
            .map(|pointer| timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame));
    }
    if editable && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if editable && response.drag_started() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frame = timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame);
            let use_start = frame.abs_diff(start) <= frame.abs_diff(end);
            ui.memory_mut(|memory| memory.data.insert_temp(id, use_start));
        }
    }
    if editable && response.dragged() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frame = timeline_frame_at(rect, pointer.x, max_frame, pixels_per_frame);
            let use_start = ui
                .memory_mut(|memory| memory.data.get_temp::<bool>(id))
                .unwrap_or(true);
            let range = if use_start {
                (frame.min(end), end)
            } else {
                (start, frame.max(start))
            };
            result.range = Some(range);
            result.seek_frame = Some(frame);
        }
    }
    if response.drag_stopped() {
        ui.memory_mut(|memory| memory.data.remove::<bool>(id));
    }
    result
}

fn timeline_action_context(
    project: &ProjectDocument,
    character_id: ResourceId,
    action: CharacterAnimationAction,
) -> Option<TimelineActionContext> {
    let animation_set = project.resource(character_id).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        character.animation_set
    })?;
    let set = project.resource(animation_set).and_then(|resource| {
        let ResourceData::AnimationSet(set) = &resource.data else {
            return None;
        };
        Some(set)
    })?;
    let clip = set.action_clip(action)?;
    let options = set
        .action_binding(action)
        .and_then(|binding| binding.options)
        .unwrap_or_else(|| CharacterActionOptions::for_action(action));
    Some(TimelineActionContext {
        animation_set,
        clip,
        options,
    })
}

fn timeline_action_for_clip(
    project: &ProjectDocument,
    character_id: ResourceId,
    clip: ResourceId,
) -> Option<CharacterAnimationAction> {
    CharacterAnimationAction::ALL.into_iter().find(|action| {
        timeline_action_context(project, character_id, *action)
            .is_some_and(|context| context.clip == clip)
    })
}

fn timeline_hitboxes(
    project: &ProjectDocument,
    character_id: ResourceId,
    action: CharacterAnimationAction,
) -> Vec<TimelineHitbox> {
    project
        .resource(character_id)
        .and_then(|resource| {
            let ResourceData::Character(character) = &resource.data else {
                return None;
            };
            Some(
                character
                    .combat_capsules
                    .iter()
                    .enumerate()
                    .filter_map(|(index, capsule)| {
                        let (capsule_action, active_start_frame, active_end_frame) =
                            match capsule.role {
                                psxed_project::CombatCapsuleRole::Hitbox {
                                    action,
                                    active_start_frame,
                                    active_end_frame,
                                    ..
                                }
                                | psxed_project::CombatCapsuleRole::ProjectileEmitter {
                                    action,
                                    active_start_frame,
                                    active_end_frame,
                                    ..
                                } => (action, active_start_frame, active_end_frame),
                                psxed_project::CombatCapsuleRole::Hurtbox => return None,
                            };
                        (capsule_action == action).then(|| TimelineHitbox {
                            index,
                            name: capsule.name.clone(),
                            start: active_start_frame,
                            end: active_end_frame.max(active_start_frame),
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn timeline_weapon_tracks(
    project: &ProjectDocument,
    character_id: ResourceId,
    action: CharacterAnimationAction,
) -> Vec<TimelineWeaponTrack> {
    let Some(set_id) = character_animation_set_id(project, Some(character_id)) else {
        return Vec::new();
    };
    project
        .resource(set_id)
        .and_then(|resource| {
            let ResourceData::AnimationSet(set) = &resource.data else {
                return None;
            };
            Some(
                set.weapon_appearance_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| track.action == action)
                    .map(|(index, track)| TimelineWeaponTrack {
                        index,
                        name: project
                            .resource(track.weapon)
                            .map(|resource| resource.name.clone())
                            .unwrap_or_else(|| format!("Missing weapon #{}", track.weapon.raw())),
                        socket: track.character_socket.clone(),
                        fully_visible_frame: track.fully_visible_frame,
                        hidden_frame: track.hidden_frame,
                        transition_frames: track.transition_frames,
                    })
                    .collect(),
            )
        })
        .unwrap_or_default()
}

fn store_timeline_action_options(
    project: &mut ProjectDocument,
    context: TimelineActionContext,
    action: CharacterAnimationAction,
    options: CharacterActionOptions,
) -> bool {
    if options == context.options {
        return false;
    }
    let Some(resource) = project.resource_mut(context.animation_set) else {
        return false;
    };
    let ResourceData::AnimationSet(set) = &mut resource.data else {
        return false;
    };
    if let Some(binding) = set
        .action_clips
        .iter_mut()
        .find(|binding| binding.action == action)
    {
        binding.options = Some(options);
    } else {
        set.action_clips
            .push(psxed_project::AnimationActionBinding {
                action,
                clip: context.clip,
                options: Some(options),
            });
    }
    true
}

fn store_timeline_hitbox_range(
    project: &mut ProjectDocument,
    character_id: Option<ResourceId>,
    index: usize,
    action: CharacterAnimationAction,
    start: u16,
    end: u16,
) -> bool {
    let Some(character_id) = character_id else {
        return false;
    };
    let Some(resource) = project.resource_mut(character_id) else {
        return false;
    };
    let ResourceData::Character(character) = &mut resource.data else {
        return false;
    };
    let Some(capsule) = character.combat_capsules.get_mut(index) else {
        return false;
    };
    let (capsule_action, active_start_frame, active_end_frame) = match &mut capsule.role {
        psxed_project::CombatCapsuleRole::Hitbox {
            action,
            active_start_frame,
            active_end_frame,
            ..
        }
        | psxed_project::CombatCapsuleRole::ProjectileEmitter {
            action,
            active_start_frame,
            active_end_frame,
            ..
        } => (action, active_start_frame, active_end_frame),
        psxed_project::CombatCapsuleRole::Hurtbox => return false,
    };
    if *capsule_action != action || (*active_start_frame == start && *active_end_frame == end) {
        return false;
    }
    *active_start_frame = start;
    *active_end_frame = end.max(start);
    true
}

fn store_timeline_weapon_range(
    project: &mut ProjectDocument,
    character_id: Option<ResourceId>,
    index: usize,
    action: CharacterAnimationAction,
    fully_visible_frame: u16,
    hidden_frame: u16,
) -> bool {
    let Some(set_id) = character_animation_set_id(project, character_id) else {
        return false;
    };
    let Some(resource) = project.resource_mut(set_id) else {
        return false;
    };
    let ResourceData::AnimationSet(set) = &mut resource.data else {
        return false;
    };
    let Some(track) = set.weapon_appearance_tracks.get_mut(index) else {
        return false;
    };
    if track.action != action
        || (track.fully_visible_frame == fully_visible_frame && track.hidden_frame == hidden_frame)
    {
        return false;
    }
    track.fully_visible_frame = fully_visible_frame;
    track.hidden_frame = hidden_frame;
    true
}

const MOVESET_ACTION_GROUPS: &[(&str, &[CharacterAnimationAction])] = &[
    (
        "Core locomotion",
        &[
            CharacterAnimationAction::Idle,
            CharacterAnimationAction::Walk,
            CharacterAnimationAction::Run,
            CharacterAnimationAction::Turn,
            CharacterAnimationAction::Intro,
        ],
    ),
    (
        "Directional movement",
        &[
            CharacterAnimationAction::WalkBackward,
            CharacterAnimationAction::StrafeLeft,
            CharacterAnimationAction::StrafeRight,
            CharacterAnimationAction::Roll,
            CharacterAnimationAction::Backstep,
            CharacterAnimationAction::DashLeft,
            CharacterAnimationAction::DashRight,
        ],
    ),
    (
        "Combat",
        &[
            CharacterAnimationAction::LightAttack,
            CharacterAnimationAction::HeavyAttack,
            CharacterAnimationAction::ComboAttack,
            CharacterAnimationAction::Block,
            CharacterAnimationAction::VertLightAttack,
            CharacterAnimationAction::VertHeavyAttack,
            CharacterAnimationAction::VertComboAttack,
        ],
    ),
    (
        "Damage and recovery",
        &[
            CharacterAnimationAction::HitReact,
            CharacterAnimationAction::HitReactAlt,
            CharacterAnimationAction::Stun,
            CharacterAnimationAction::StunRecovery,
            CharacterAnimationAction::Death,
        ],
    ),
    (
        "Alternate weapon",
        &[
            CharacterAnimationAction::AltLightAttack,
            CharacterAnimationAction::AltHeavyAttack,
            CharacterAnimationAction::AltComboAttack,
        ],
    ),
    (
        "Locomotion transitions",
        &[
            CharacterAnimationAction::WalkWindup,
            CharacterAnimationAction::WalkWinddown,
            CharacterAnimationAction::WalkWinddownAlt,
            CharacterAnimationAction::RunWindup,
            CharacterAnimationAction::RunWinddown,
            CharacterAnimationAction::RunWinddownAlt,
        ],
    ),
];

fn moveset_status_label(status: MovesetCapabilityStatus) -> (&'static str, Color32) {
    match status {
        MovesetCapabilityStatus::Ready => ("Ready", Color32::from_rgb(86, 224, 156)),
        MovesetCapabilityStatus::Missing => ("Missing", Color32::from_rgb(238, 102, 82)),
        MovesetCapabilityStatus::Disabled => ("Disabled", STUDIO_TEXT_WEAK),
        MovesetCapabilityStatus::Broken => ("Broken", Color32::from_rgb(245, 156, 76)),
    }
}

fn draw_moveset_capability_matrix(
    ui: &mut egui::Ui,
    project: &ProjectDocument,
    character_id: ResourceId,
    state: &mut ModelAnimationViewerState,
    clip_options: &[ViewerClipOption],
) {
    ui.heading("Moveset Matrix");
    ui.label(
        RichText::new("Ready means the action owns valid motion. A visual fallback only keeps a forced state renderable; it does not give a disabled action gameplay capability.")
            .small()
            .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(8.0);

    let Some(rows) = moveset_capability_rows(project, character_id) else {
        ui.colored_label(
            Color32::from_rgb(238, 102, 82),
            "Assign a valid Animation Set to audit this character's moveset.",
        );
        return;
    };
    let ready = rows
        .iter()
        .filter(|row| row.status == MovesetCapabilityStatus::Ready)
        .count();
    let missing = rows
        .iter()
        .filter(|row| row.status == MovesetCapabilityStatus::Missing)
        .count();
    let disabled = rows
        .iter()
        .filter(|row| row.status == MovesetCapabilityStatus::Disabled)
        .count();
    let broken = rows
        .iter()
        .filter(|row| row.status == MovesetCapabilityStatus::Broken)
        .count();

    egui::Frame::new()
        .fill(STUDIO_PANEL_HEADER)
        .stroke(Stroke::new(1.0, STUDIO_BORDER))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{ready} ready"))
                        .strong()
                        .color(Color32::from_rgb(86, 224, 156)),
                );
                ui.label(RichText::new(format!("{disabled} disabled")).color(STUDIO_TEXT_WEAK));
                if missing > 0 {
                    ui.label(
                        RichText::new(format!("{missing} required missing"))
                            .strong()
                            .color(Color32::from_rgb(238, 102, 82)),
                    );
                } else {
                    ui.label(
                        RichText::new("core requirements ready")
                            .color(Color32::from_rgb(86, 224, 156)),
                    );
                }
                if broken > 0 {
                    ui.label(
                        RichText::new(format!("{broken} broken"))
                            .strong()
                            .color(Color32::from_rgb(245, 156, 76)),
                    );
                }
            });
        });

    ui.add_space(8.0);
    let action_width = 122.0;
    let status_width = 62.0;
    let motion_width = (ui.available_width() - action_width - status_width - 42.0).max(120.0);
    ui.horizontal(|ui| {
        ui.add_sized(
            [action_width, 18.0],
            egui::Label::new(RichText::new("ACTION").small().color(STUDIO_TEXT_WEAK)),
        );
        ui.add_sized(
            [status_width, 18.0],
            egui::Label::new(RichText::new("STATUS").small().color(STUDIO_TEXT_WEAK)),
        );
        ui.add_sized(
            [motion_width, 18.0],
            egui::Label::new(
                RichText::new("MOTION / VISUAL FALLBACK")
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            ),
        );
    });
    ui.separator();

    for (group_index, (group, actions)) in MOVESET_ACTION_GROUPS.iter().enumerate() {
        let default_open = group_index < 4;
        egui::CollapsingHeader::new(*group)
            .id_salt(("moveset-capability-group", group_index))
            .default_open(default_open)
            .show(ui, |ui| {
                egui::Grid::new(("moveset-capability-grid", group_index))
                    .num_columns(3)
                    .striped(true)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for action in *actions {
                            let Some(row) = rows.iter().find(|row| row.action == *action) else {
                                continue;
                            };
                            let action_response = ui.add_sized(
                                [action_width, 20.0],
                                egui::Button::new(row.action.label())
                                    .selected(state.selected_action == row.action),
                            );
                            if action_response
                                .on_hover_text("Select this action and preview its assigned motion or visual fallback")
                                .clicked()
                            {
                                state.selected_action = row.action;
                                let preview_clip = row.clip.or(row.visual_fallback_clip);
                                if let Some(clip) = preview_clip.and_then(|clip| {
                                    clip_options.iter().find(|option| {
                                        option.resource == Some(clip) && option.previewable
                                    })
                                }) {
                                    state.selected_clip_path = Some(clip.path.clone());
                                    state.invalidate_clip_cache();
                                    state.reset_clip_clock();
                                }
                            }

                            let (status, color) = moveset_status_label(row.status);
                            let status_hint = match row.status {
                                MovesetCapabilityStatus::Ready => {
                                    "This action owns a resolved clip and does not borrow motion."
                                }
                                MovesetCapabilityStatus::Missing => {
                                    "This core action is required but has no resolved clip."
                                }
                                MovesetCapabilityStatus::Disabled => {
                                    "No clip is assigned, so the gameplay action is disabled."
                                }
                                MovesetCapabilityStatus::Broken => {
                                    "The action points to a missing or non-animation resource."
                                }
                            };
                            ui.add_sized(
                                [status_width, 20.0],
                                egui::Label::new(RichText::new(status).small().color(color)),
                            )
                            .on_hover_text(status_hint);

                            let clip_label = row.clip_name.as_deref().unwrap_or("—");
                            let clip_hint = match row.binding_source {
                                Some(MovesetBindingSource::Action) => {
                                    format!("{clip_label}\nExplicit action binding")
                                }
                                Some(MovesetBindingSource::LegacyRole) => {
                                    format!("{clip_label}\nResolved from a legacy role slot")
                                }
                                None if row.status == MovesetCapabilityStatus::Broken => {
                                    "Assigned resource is missing or has the wrong type".to_string()
                                }
                                None => "No clip assigned".to_string(),
                            };
                            let (motion_label, motion_hint) = if row.status
                                == MovesetCapabilityStatus::Ready
                            {
                                (clip_label.to_string(), clip_hint)
                            } else if let (Some(fallback), Some(name)) =
                                (row.visual_fallback_action, row.visual_fallback_name.as_deref())
                            {
                                (
                                    format!("Visual: {} · {name}", fallback.label()),
                                    format!(
                                        "Renderer fallback only: {} does not become an enabled action.",
                                        row.action.label()
                                    ),
                                )
                            } else if row.status == MovesetCapabilityStatus::Broken {
                                ("Invalid assigned resource".to_string(), clip_hint)
                            } else {
                                (
                                    "—".to_string(),
                                    "No motion or visual fallback resolves".to_string(),
                                )
                            };
                            ui.add_sized(
                                [motion_width, 20.0],
                                egui::Label::new(motion_label).truncate(),
                            )
                            .on_hover_text(motion_hint);
                            ui.end_row();
                        }
                    });
            });
    }
}

fn draw_pose_correction_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    clip_id: ResourceId,
    state: &mut ModelAnimationViewerState,
    joint_count: u16,
    max_frame: u16,
) -> bool {
    ui.heading("Pose Keys");
    ui.label(
        RichText::new("Select a highlighted joint, then add sparse visual corrections.")
            .small()
            .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);

    if joint_count == 0 {
        ui.weak("The selected model has no cooked joints.");
        return false;
    }
    state.selected_pose_joint = state.selected_pose_joint.min(joint_count.saturating_sub(1));
    let mut changed = false;
    let joint_names = state
        .selected_model
        .and_then(|model_id| model_skeleton_joint_names(project, model_id));
    let joint_options = (0..joint_count)
        .map(|joint| {
            (
                joint,
                crate::inspector_character_ui::joint_label(joint, joint_names.as_deref()),
            )
        })
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Joint").color(STUDIO_TEXT_WEAK));
        let mut selected = Some(state.selected_pose_joint);
        if searchable_picker(
            ui,
            "animation-pose-correction-joint",
            &mut selected,
            &crate::inspector_character_ui::joint_label(
                state.selected_pose_joint,
                joint_names.as_deref(),
            ),
            &joint_options,
            SearchablePickerConfig::required()
                .with_width(132.0)
                .with_search_hint("Search joints…"),
        ) {
            state.selected_pose_joint = selected.unwrap_or(state.selected_pose_joint);
        }
    });
    draw_axis_gizmo_controls(ui, state, true);

    let frame = (state.frame.round().max(0.0) as u16).min(max_frame);
    let Some(resource) = project.resource_mut(clip_id) else {
        return false;
    };
    let ResourceData::AnimationClip(clip) = &mut resource.data else {
        return false;
    };
    let joint_key_count = clip
        .pose_corrections
        .iter()
        .filter(|key| key.joint == state.selected_pose_joint)
        .count();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Frame {frame} / {max_frame}")).monospace());
        ui.label(
            RichText::new(format!("{joint_key_count} keys"))
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
    });

    let mut key_index = clip
        .pose_corrections
        .iter()
        .position(|key| key.joint == state.selected_pose_joint && key.frame == frame);
    ui.horizontal(|ui| {
        if key_index.is_none()
            && ui
                .button(icons::label(icons::PLUS, "Add key"))
                .on_hover_text("Add a correction key at the current sampled frame")
                .clicked()
        {
            let mut key = psxed_project::sample_pose_correction(
                &clip.pose_corrections,
                state.selected_pose_joint,
                frame,
            );
            key.frame = frame;
            key.joint = state.selected_pose_joint;
            clip.pose_corrections.push(key);
            clip.pose_corrections
                .sort_by_key(|key| (key.joint, key.frame));
            key_index = clip
                .pose_corrections
                .iter()
                .position(|key| key.joint == state.selected_pose_joint && key.frame == frame);
            changed = true;
        }
        if let Some(index) = key_index {
            if ui
                .button(icons::label(icons::TRASH, "Delete key"))
                .clicked()
            {
                clip.pose_corrections.remove(index);
                key_index = None;
                changed = true;
            }
        }
    });

    let Some(index) = key_index else {
        ui.add_space(8.0);
        ui.label(
            RichText::new("No key at this frame. Existing keys hold at the ends and interpolate between frames.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        return changed;
    };

    let key = &mut clip.pose_corrections[index];
    ui.add_space(5.0);
    ui.separator();
    ui.label(RichText::new("Rotation delta").strong());
    ui.horizontal(|ui| {
        for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK));
            let mut degrees = q12_delta_to_degrees(key.rotation_q12[axis]);
            if ui
                .add(
                    egui::DragValue::new(&mut degrees)
                        .speed(0.5)
                        .range(-180.0..=180.0)
                        .suffix("°"),
                )
                .changed()
            {
                key.rotation_q12[axis] = degrees_to_q12_delta(degrees);
                changed = true;
            }
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new("Translation delta").strong());
    ui.horizontal(|ui| {
        for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
            ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK));
            changed |= ui
                .add(
                    egui::DragValue::new(&mut key.translation[axis])
                        .speed(2.0)
                        .range(-8192..=8192),
                )
                .changed();
        }
    });
    if ui.small_button("Reset key values").clicked() {
        key.rotation_q12 = [0; 3];
        key.translation = [0; 3];
        changed = true;
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("Corrections are folded into sampled animation matrices during build; runtime cost is unchanged.")
            .small()
            .color(STUDIO_TEXT_WEAK),
    );
    changed
}

fn draw_axis_gizmo_controls(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    creates_pose_key: bool,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Viewport");
        ui.selectable_value(&mut state.capsule_edit_tool, CapsuleEditTool::Move, "Move");
        ui.selectable_value(
            &mut state.capsule_edit_tool,
            CapsuleEditTool::Rotate,
            "Rotate",
        );
    });
    ui.horizontal(|ui| {
        ui.label("Axis");
        for axis in CapsuleEditAxis::ALL {
            ui.selectable_value(&mut state.capsule_edit_axis, axis, axis.label());
        }
    });
    let help = match (state.capsule_edit_tool, creates_pose_key) {
        (CapsuleEditTool::Rotate, true) => {
            "Hover a coloured axis until it brightens, then drag it to rotate the selected joint. The first drag creates a correction key at this frame."
        }
        (CapsuleEditTool::Rotate, false) => {
            "Hover a coloured axis until it brightens, then drag it to rotate around that axis."
        }
        (_, true) => {
            "Hover a coloured axis until it brightens, then drag along it to move the selected joint. The first drag creates a correction key at this frame. Hold Shift for precision or Cmd/Ctrl for 4× movement."
        }
        (_, false) => {
            "Hover a coloured axis until it brightens, then drag along it. Motion follows its local direction at every zoom. Hold Shift for precision or Cmd/Ctrl for 4× movement."
        }
    };
    ui.label(RichText::new(help).small().color(STUDIO_TEXT_WEAK));
}

fn q12_delta_to_degrees(value: i16) -> f32 {
    f32::from(value) * 360.0 / 4096.0
}

fn degrees_to_q12_delta(value: f32) -> i16 {
    (value.clamp(-180.0, 180.0) * 4096.0 / 360.0)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn draw_combat_capsule_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    character_id: ResourceId,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    assigned_action_clip: Option<&ViewerClipOption>,
) -> bool {
    let mut changed = false;
    let joint_names = project
        .resource(character_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => character.model,
            _ => None,
        })
        .and_then(|model_id| model_skeleton_joint_names(project, model_id));
    ui.heading("Combat Volumes");
    ui.label(
        RichText::new(
            "Select a capsule, then click a highlighted body joint to attach and fit it.",
        )
        .small()
        .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);

    let Some(resource) = project.resource_mut(character_id) else {
        ui.colored_label(Color32::from_rgb(220, 120, 100), "Character is missing");
        return false;
    };
    let ResourceData::Character(character) = &mut resource.data else {
        return false;
    };

    let capsule_options = character
        .combat_capsules
        .iter()
        .enumerate()
        .map(|(index, capsule)| {
            let role = match capsule.role {
                psxed_project::CombatCapsuleRole::Hurtbox => "Hurt",
                psxed_project::CombatCapsuleRole::Hitbox { .. } => "Hit",
                psxed_project::CombatCapsuleRole::ProjectileEmitter { .. } => "Shot",
            };
            (index, format!("{} · {role}", capsule.name))
        })
        .collect::<Vec<_>>();
    ui.horizontal_wrapped(|ui| {
        let selected_label = character
            .combat_capsules
            .get(state.selected_combat_capsule)
            .map(|capsule| capsule.name.as_str())
            .unwrap_or("No volume");
        let mut selected = character
            .combat_capsules
            .get(state.selected_combat_capsule)
            .map(|_| state.selected_combat_capsule);
        if searchable_picker(
            ui,
            "animation-combat-capsule",
            &mut selected,
            selected_label,
            &capsule_options,
            SearchablePickerConfig::required()
                .with_width(176.0)
                .with_search_hint("Search combat volumes…"),
        ) {
            state.selected_combat_capsule = selected.unwrap_or(state.selected_combat_capsule);
        }
        if ui
            .small_button(icons::text(icons::TRASH, 13.0))
            .on_hover_text("Remove selected combat volume")
            .clicked()
            && state.selected_combat_capsule < character.combat_capsules.len()
        {
            character
                .combat_capsules
                .remove(state.selected_combat_capsule);
            state.selected_combat_capsule = state
                .selected_combat_capsule
                .min(character.combat_capsules.len().saturating_sub(1));
            changed = true;
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.button(icons::label(icons::PLUS, "Hurtbox")).clicked() {
            character
                .combat_capsules
                .push(psxed_project::CharacterCombatCapsule::default());
            state.selected_combat_capsule = character.combat_capsules.len() - 1;
            changed = true;
        }
        if ui.button(icons::label(icons::PLUS, "Hitbox")).clicked() {
            character
                .combat_capsules
                .push(psxed_project::CharacterCombatCapsule {
                    name: "Attack Hitbox".to_string(),
                    role: psxed_project::CombatCapsuleRole::Hitbox {
                        action: psxed_project::CharacterAnimationAction::LightAttack,
                        active_start_frame: 8,
                        active_end_frame: 14,
                        damage: 25,
                        poise_damage: 25,
                    },
                    ..psxed_project::CharacterCombatCapsule::default()
                });
            state.selected_combat_capsule = character.combat_capsules.len() - 1;
            changed = true;
        }
        if ui.button(icons::label(icons::PLUS, "Projectile")).clicked() {
            character
                .combat_capsules
                .push(psxed_project::CharacterCombatCapsule {
                    name: "Projectile Muzzle".to_string(),
                    capsule: psxed_project::JointCapsule {
                        start: [0; 3],
                        end: [0; 3],
                        radius: 48,
                    },
                    role: psxed_project::CombatCapsuleRole::ProjectileEmitter {
                        action: psxed_project::CharacterAnimationAction::LightAttack,
                        active_start_frame: 8,
                        active_end_frame: 12,
                        speed: 160,
                        lifetime_ticks: 180,
                        min_range: 512,
                        max_range: 4096,
                        damage: 20,
                        poise_damage: 10,
                        tint_rgb: [120, 210, 255],
                    },
                    ..psxed_project::CharacterCombatCapsule::default()
                });
            state.selected_combat_capsule = character.combat_capsules.len() - 1;
            changed = true;
        }
    });

    let Some(capsule) = character
        .combat_capsules
        .get_mut(state.selected_combat_capsule)
    else {
        ui.add_space(10.0);
        ui.weak("Add a receiving hurtbox or a damage-dealing hitbox to begin.");
        return changed;
    };
    ui.separator();
    changed |= ui.text_edit_singleline(&mut capsule.name).changed();

    ui.horizontal(|ui| {
        ui.label("Viewport");
        ui.selectable_value(&mut state.capsule_edit_tool, CapsuleEditTool::Move, "Move");
        ui.selectable_value(
            &mut state.capsule_edit_tool,
            CapsuleEditTool::Rotate,
            "Rotate",
        );
        ui.selectable_value(
            &mut state.capsule_edit_tool,
            CapsuleEditTool::Resize,
            "Resize",
        );
    });
    ui.horizontal(|ui| {
        ui.label("Local axis");
        ui.selectable_value(&mut state.capsule_edit_axis, CapsuleEditAxis::X, "X");
        ui.selectable_value(&mut state.capsule_edit_axis, CapsuleEditAxis::Y, "Y");
        ui.selectable_value(&mut state.capsule_edit_axis, CapsuleEditAxis::Z, "Z");
    });
    ui.label(
        RichText::new(match state.capsule_edit_tool {
            CapsuleEditTool::Move => "Left-drag moves along the selected bone-local axis.",
            CapsuleEditTool::Rotate => "Left-drag rotates around the selected bone-local axis.",
            CapsuleEditTool::Resize => "Left/right changes radius; up/down changes segment length.",
        })
        .small()
        .color(STUDIO_TEXT_WEAK),
    );

    let mut role_kind = match capsule.role {
        psxed_project::CombatCapsuleRole::Hurtbox => 0,
        psxed_project::CombatCapsuleRole::Hitbox { .. } => 1,
        psxed_project::CombatCapsuleRole::ProjectileEmitter { .. } => 2,
    };
    ui.horizontal(|ui| {
        ui.label("Role");
        egui::ComboBox::from_id_salt("animation-combat-capsule-role")
            .selected_text(match role_kind {
                0 => "Receive damage (Hurtbox)",
                1 => "Deal damage (Hitbox)",
                _ => "Release projectile (Emitter)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut role_kind, 0, "Receive damage (Hurtbox)");
                ui.selectable_value(&mut role_kind, 1, "Deal damage (Hitbox)");
                ui.selectable_value(&mut role_kind, 2, "Release projectile (Emitter)");
            });
    });
    match (role_kind, capsule.role) {
        (0, role) if !matches!(role, psxed_project::CombatCapsuleRole::Hurtbox) => {
            capsule.role = psxed_project::CombatCapsuleRole::Hurtbox;
            changed = true;
        }
        (1, role) if !matches!(role, psxed_project::CombatCapsuleRole::Hitbox { .. }) => {
            capsule.role = psxed_project::CombatCapsuleRole::Hitbox {
                action: psxed_project::CharacterAnimationAction::LightAttack,
                active_start_frame: 8,
                active_end_frame: 14,
                damage: 25,
                poise_damage: 25,
            };
            changed = true;
        }
        (2, role)
            if !matches!(
                role,
                psxed_project::CombatCapsuleRole::ProjectileEmitter { .. }
            ) =>
        {
            capsule.capsule.end = capsule.capsule.start;
            capsule.role = psxed_project::CombatCapsuleRole::ProjectileEmitter {
                action: psxed_project::CharacterAnimationAction::LightAttack,
                active_start_frame: 8,
                active_end_frame: 12,
                speed: 160,
                lifetime_ticks: 180,
                min_range: 512,
                max_range: 4096,
                damage: 20,
                poise_damage: 10,
                tint_rgb: [120, 210, 255],
            };
            changed = true;
        }
        _ => {}
    }

    let joint_count = model
        .and_then(|model| psx_asset::Model::from_bytes(&model.model_bytes).ok())
        .map(|model| model.joint_count())
        .unwrap_or(0);
    ui.horizontal(|ui| {
        ui.label("Body joint");
        let max = joint_count.saturating_sub(1);
        changed |= ui
            .add(egui::DragValue::new(&mut capsule.joint).range(0..=max))
            .changed();
        if let Some(name) = joint_names
            .as_deref()
            .and_then(|names| names.get(capsule.joint as usize))
            .filter(|name| !name.trim().is_empty())
        {
            ui.label(RichText::new(name.as_str()).small().color(STUDIO_TEXT_WEAK));
        }
        if ui
            .button(icons::label(icons::SCAN, "Refit"))
            .on_hover_text("Fit this capsule around the vertices controlled by its joint")
            .clicked()
        {
            if let Some(model) = model {
                if let Some(fit) = model_import_preview::fit_capsule_to_joint(
                    &model.model_bytes,
                    capsule.joint,
                    model.visual_scale_q8,
                ) {
                    capsule.capsule.start = fit.start;
                    capsule.capsule.end = fit.end;
                    capsule.capsule.radius = fit.radius;
                    changed = true;
                }
            }
        }
    });
    if joint_count == 0 {
        ui.colored_label(Color32::from_rgb(220, 160, 80), "No cooked rig is loaded");
    } else {
        ui.label(
            RichText::new(format!(
                "Joint {} of {} · click the model to reattach",
                capsule.joint, joint_count
            ))
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
    }

    ui.add_space(6.0);
    ui.label(RichText::new("Joint-local shape").strong());
    if matches!(
        capsule.role,
        psxed_project::CombatCapsuleRole::ProjectileEmitter { .. }
    ) {
        changed |= combat_vec3_editor(ui, "Muzzle offset", &mut capsule.capsule.start);
        if capsule.capsule.end != capsule.capsule.start {
            capsule.capsule.end = capsule.capsule.start;
            changed = true;
        }
        ui.label(
            RichText::new(
                "Projectile emitters are spheres; the point is both muzzle and collision center.",
            )
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
    } else {
        changed |= combat_vec3_editor(ui, "Start", &mut capsule.capsule.start);
        changed |= combat_vec3_editor(ui, "End", &mut capsule.capsule.end);
    }
    ui.horizontal(|ui| {
        ui.label("Radius");
        changed |= ui
            .add(
                egui::DragValue::new(&mut capsule.capsule.radius)
                    .range(1..=8192)
                    .speed(2.0),
            )
            .changed();
    });

    if let psxed_project::CombatCapsuleRole::Hitbox {
        action,
        active_start_frame,
        active_end_frame,
        damage,
        poise_damage,
    } = &mut capsule.role
    {
        ui.separator();
        egui::ComboBox::from_label("Action")
            .selected_text(action.label())
            .show_ui(ui, |ui| {
                for candidate in psxed_project::CharacterAnimationAction::ALL {
                    changed |= ui
                        .selectable_value(action, candidate, candidate.label())
                        .changed();
                }
            });
        ui.horizontal(|ui| {
            ui.label("Assigned clip");
            if let Some(clip) = assigned_action_clip {
                ui.label(RichText::new(&clip.label).color(STUDIO_TEXT_WEAK));
                if ui
                    .small_button(icons::label(icons::PLAY, "Preview"))
                    .on_hover_text("Switch the Animation Studio preview to this action's clip")
                    .clicked()
                {
                    state.selected_clip_path = Some(clip.path.clone());
                    state.invalidate_clip_cache();
                    state.reset_clip_clock();
                }
            } else {
                ui.colored_label(
                    Color32::from_rgb(220, 160, 80),
                    "No clip assigned in the Animation Set",
                );
            }
        });
        changed |= combat_u16_editor(ui, "Active start", active_start_frame, 0, u16::MAX);
        changed |= combat_u16_editor(ui, "Active end", active_end_frame, 0, u16::MAX);
        if *active_end_frame < *active_start_frame {
            *active_end_frame = *active_start_frame;
            changed = true;
        }
        changed |= combat_u16_editor(ui, "Damage", damage, 1, 9999);
        changed |= combat_u16_editor(ui, "Poise damage", poise_damage, 0, 9999);
    }

    if let psxed_project::CombatCapsuleRole::ProjectileEmitter {
        action,
        active_start_frame,
        active_end_frame,
        speed,
        lifetime_ticks,
        min_range,
        max_range,
        damage,
        poise_damage,
        tint_rgb,
    } = &mut capsule.role
    {
        ui.separator();
        egui::ComboBox::from_label("Action")
            .selected_text(action.label())
            .show_ui(ui, |ui| {
                for candidate in psxed_project::CharacterAnimationAction::ALL {
                    changed |= ui
                        .selectable_value(action, candidate, candidate.label())
                        .changed();
                }
            });
        ui.horizontal(|ui| {
            ui.label("Assigned clip");
            if let Some(clip) = assigned_action_clip {
                ui.label(RichText::new(&clip.label).color(STUDIO_TEXT_WEAK));
                if ui
                    .small_button(icons::label(icons::PLAY, "Preview"))
                    .on_hover_text("Switch the Animation Studio preview to this action's clip")
                    .clicked()
                {
                    state.selected_clip_path = Some(clip.path.clone());
                    state.invalidate_clip_cache();
                    state.reset_clip_clock();
                }
            } else {
                ui.colored_label(
                    Color32::from_rgb(220, 160, 80),
                    "No clip assigned in the Animation Set",
                );
            }
        });
        changed |= combat_u16_editor(ui, "Release start", active_start_frame, 0, u16::MAX);
        changed |= combat_u16_editor(ui, "Release end", active_end_frame, 0, u16::MAX);
        if *active_end_frame < *active_start_frame {
            *active_end_frame = *active_start_frame;
            changed = true;
        }
        changed |= combat_u16_editor(ui, "Speed / tick", speed, 1, 8192);
        changed |= combat_u16_editor(ui, "Lifetime ticks", lifetime_ticks, 1, 3600);
        changed |= combat_u16_editor(ui, "Minimum range", min_range, 0, u16::MAX);
        changed |= combat_u16_editor(ui, "Maximum range", max_range, 1, u16::MAX);
        if *max_range < *min_range {
            *max_range = *min_range;
            changed = true;
        }
        changed |= combat_u16_editor(ui, "Damage", damage, 1, 9999);
        changed |= combat_u16_editor(ui, "Poise damage", poise_damage, 0, 9999);
        ui.horizontal(|ui| {
            ui.label("Tint");
            changed |= ui.color_edit_button_srgb(tint_rgb).changed();
        });
    }

    changed
}

fn assigned_action_clip(
    project: &ProjectDocument,
    character_id: ResourceId,
    action: psxed_project::CharacterAnimationAction,
    clip_options: &[ViewerClipOption],
) -> Option<ViewerClipOption> {
    let character = project.resource(character_id).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        Some(character)
    })?;
    let clip_id = character.animation_set.and_then(|set_id| {
        project.resource(set_id).and_then(|resource| {
            let ResourceData::AnimationSet(set) = &resource.data else {
                return None;
            };
            set.action_clip(action)
        })
    })?;
    clip_options
        .iter()
        .find(|clip| clip.resource == Some(clip_id) && clip.previewable)
        .cloned()
}

fn combat_vec3_editor(ui: &mut egui::Ui, label: &str, value: &mut [i32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        for (axis, prefix) in ["X ", "Y ", "Z "].iter().enumerate() {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut value[axis])
                        .prefix(*prefix)
                        .range(-32768..=32767)
                        .speed(2.0),
                )
                .changed();
        }
    });
    changed
}

fn combat_u16_editor(ui: &mut egui::Ui, label: &str, value: &mut u16, min: u16, max: u16) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).range(min..=max))
            .changed()
    })
    .inner
}

/// Captured source bone names for a model's skeleton, when the
/// skeleton resource has them (imports since joint-name capture).
fn model_skeleton_joint_names(
    project: &ProjectDocument,
    model_id: ResourceId,
) -> Option<Vec<String>> {
    let skeleton_id = match &project.resource(model_id)?.data {
        ResourceData::Model(model) => model.skeleton?,
        _ => return None,
    };
    match &project.resource(skeleton_id)?.data {
        ResourceData::Skeleton(skeleton) if !skeleton.joint_names.is_empty() => {
            Some(skeleton.joint_names.clone())
        }
        _ => None,
    }
}

fn next_weapon_appearance_pair(
    project: &ProjectDocument,
    set_id: ResourceId,
    action: CharacterAnimationAction,
    preferred_weapon: Option<ResourceId>,
    preferred_socket: &str,
    weapon_options: &[(ResourceId, String)],
    socket_names: &[String],
) -> Option<(ResourceId, String)> {
    let used = project
        .resource(set_id)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => Some(
                set.weapon_appearance_tracks
                    .iter()
                    .filter(|track| track.action == action)
                    .map(|track| (track.weapon, track.character_socket.clone()))
                    .collect::<HashSet<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |weapon: ResourceId, socket: String| {
        let pair = (weapon, socket);
        if seen.insert(pair.clone()) {
            candidates.push(pair);
        }
    };

    if let Some(weapon) = preferred_weapon {
        push(weapon, preferred_socket.to_string());
    }
    // A weapon's declared default hand is the most useful automatic second
    // beat (for example light sword -> heavy sword in the same hand).
    for (weapon_id, _) in weapon_options {
        let socket = project
            .resource(*weapon_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Weapon(weapon) => Some(weapon.default_character_socket.clone()),
                _ => None,
            })
            .unwrap_or_else(|| preferred_socket.to_string());
        push(*weapon_id, socket);
    }
    if let Some(weapon) = preferred_weapon {
        for socket in socket_names {
            push(weapon, socket.clone());
        }
    }
    for (weapon_id, _) in weapon_options {
        for socket in socket_names {
            push(*weapon_id, socket.clone());
        }
    }

    candidates.into_iter().find(|pair| !used.contains(pair))
}

fn draw_weapon_appearance_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    model_id: ResourceId,
    character_id: Option<ResourceId>,
    state: &mut ModelAnimationViewerState,
    max_frame: u16,
) -> bool {
    let weapon_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Weapon(_)));
    let socket_names = project
        .resource(model_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(
                model
                    .attachments
                    .iter()
                    .map(|socket| socket.name.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let Some(set_id) = character_animation_set_id(project, character_id) else {
        ui.label(RichText::new("Visibility beat").strong());
        ui.colored_label(
            Color32::from_rgb(220, 160, 80),
            "Select a Character with an Animation Set to author appearance timing.",
        );
        return false;
    };

    let track_options = project
        .resource(set_id)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => Some(
                set.weapon_appearance_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| track.action == state.selected_action)
                    .map(|(index, track)| {
                        let weapon_name = weapon_options
                            .iter()
                            .find(|(id, _)| *id == track.weapon)
                            .map(|(_, name)| name.as_str())
                            .unwrap_or("Missing weapon");
                        (
                            index,
                            format!("{} · {}", weapon_name, track.character_socket),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    if !track_options
        .iter()
        .any(|(index, _)| *index == state.selected_weapon_track)
    {
        if let Some((index, _)) = track_options.first() {
            state.selected_weapon_track = *index;
        }
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("Visibility beat").strong());
        ui.label(
            RichText::new(state.selected_action.label())
                .small()
                .color(STUDIO_ACCENT),
        );
    });
    let current_frame = (state.frame.round().max(0.0) as u16).min(max_frame);
    let default_weapon = state
        .preview_weapon
        .or_else(|| weapon_options.first().map(|(id, _)| *id));
    let default_socket = socket_names
        .get(state.selected_attachment_socket)
        .cloned()
        .or_else(|| {
            default_weapon.and_then(|weapon_id| {
                project.resource(weapon_id).and_then(|resource| {
                    let ResourceData::Weapon(weapon) = &resource.data else {
                        return None;
                    };
                    Some(weapon.default_character_socket.clone())
                })
            })
        })
        .unwrap_or_else(|| "right_hand_grip".to_string());
    let add_pair = next_weapon_appearance_pair(
        project,
        set_id,
        state.selected_action,
        default_weapon,
        &default_socket,
        &weapon_options,
        &socket_names,
    );
    let add_help = add_pair.as_ref().map_or_else(
        || "Every available weapon/socket pair already has a beat for this action".to_string(),
        |(weapon, socket)| {
            format!(
                "Add a {} visibility beat on '{}' at the current frame",
                project.resource_name(*weapon).unwrap_or("weapon"),
                socket
            )
        },
    );
    let mut add_track = None;
    let mut delete_track = false;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                add_pair.is_some(),
                egui::Button::new(icons::label(icons::PLUS, "Add beat")),
            )
            .on_hover_text(add_help)
            .clicked()
        {
            add_track = add_pair.clone();
        }
        if ui
            .add_enabled(
                !track_options.is_empty(),
                egui::Button::new(icons::label(icons::TRASH, "Delete")),
            )
            .clicked()
        {
            delete_track = true;
        }
    });
    if let Some((weapon, character_socket)) = add_track {
        let Some(resource) = project.resource_mut(set_id) else {
            return false;
        };
        let ResourceData::AnimationSet(set) = &mut resource.data else {
            return false;
        };
        set.weapon_appearance_tracks
            .push(psxed_project::WeaponAppearanceTrack {
                action: state.selected_action,
                weapon,
                character_socket,
                fully_visible_frame: current_frame,
                hidden_frame: psxed_project::ACTION_FRAME_END_FULL,
                transition_frames: psxed_project::WEAPON_APPEARANCE_DEFAULT_TRANSITION_FRAMES,
            });
        state.selected_weapon_track = set.weapon_appearance_tracks.len() - 1;
        state.preview_weapon = Some(weapon);
        return true;
    }
    if delete_track {
        let Some(resource) = project.resource_mut(set_id) else {
            return false;
        };
        let ResourceData::AnimationSet(set) = &mut resource.data else {
            return false;
        };
        if state.selected_weapon_track < set.weapon_appearance_tracks.len() {
            set.weapon_appearance_tracks
                .remove(state.selected_weapon_track);
            state.selected_weapon_track = state.selected_weapon_track.saturating_sub(1);
            return true;
        }
    }

    if track_options.is_empty() {
        ui.label(
            RichText::new("No weapon beat for this action. Add one at the playhead.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        return false;
    }
    let selected_label = track_options
        .iter()
        .find(|(index, _)| *index == state.selected_weapon_track)
        .map(|(_, label)| label.as_str())
        .unwrap_or("Select beat");
    egui::ComboBox::from_id_salt("animation-weapon-appearance-track-select")
        .selected_text(selected_label)
        .width(250.0)
        .show_ui(ui, |ui| {
            for (index, label) in &track_options {
                ui.selectable_value(&mut state.selected_weapon_track, *index, label);
            }
        });

    let Some(resource) = project.resource_mut(set_id) else {
        return false;
    };
    let ResourceData::AnimationSet(set) = &mut resource.data else {
        return false;
    };
    let Some(track) = set
        .weapon_appearance_tracks
        .get_mut(state.selected_weapon_track)
    else {
        return false;
    };
    let mut changed = false;
    let mut selected_weapon = Some(track.weapon);
    ui.horizontal(|ui| {
        ui.label("Weapon");
        if searchable_picker(
            ui,
            "animation-weapon-appearance-resource",
            &mut selected_weapon,
            weapon_options
                .iter()
                .find(|(id, _)| *id == track.weapon)
                .map(|(_, name)| name.as_str())
                .unwrap_or("Missing weapon"),
            &weapon_options,
            SearchablePickerConfig::required().with_width(180.0),
        ) {
            if let Some(weapon) = selected_weapon {
                track.weapon = weapon;
                state.preview_weapon = Some(weapon);
                changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Hand socket");
        egui::ComboBox::from_id_salt("animation-weapon-appearance-socket")
            .selected_text(&track.character_socket)
            .show_ui(ui, |ui| {
                for name in &socket_names {
                    changed |= ui
                        .selectable_value(&mut track.character_socket, name.clone(), name)
                        .changed();
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Fully visible");
        changed |= ui
            .add(
                egui::DragValue::new(&mut track.fully_visible_frame)
                    .range(0..=max_frame)
                    .prefix("Frame "),
            )
            .changed();
        if ui.small_button("Use playhead").clicked() {
            track.fully_visible_frame = current_frame;
            changed = true;
        }
    });
    let mut hide_at_clip_end = track.hidden_frame == psxed_project::ACTION_FRAME_END_FULL;
    if ui
        .checkbox(&mut hide_at_clip_end, "Gone at clip end")
        .changed()
    {
        track.hidden_frame = if hide_at_clip_end {
            psxed_project::ACTION_FRAME_END_FULL
        } else {
            current_frame
                .max(track.fully_visible_frame.saturating_add(1))
                .min(max_frame)
        };
        changed = true;
    }
    if !hide_at_clip_end {
        ui.horizontal(|ui| {
            ui.label("Gone by");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut track.hidden_frame)
                        .range(0..=max_frame)
                        .prefix("Frame "),
                )
                .changed();
            if ui.small_button("Use playhead").clicked() {
                track.hidden_frame = current_frame;
                changed = true;
            }
        });
        let min_hidden = track.fully_visible_frame.saturating_add(1).min(max_frame);
        if track.hidden_frame < min_hidden {
            track.hidden_frame = min_hidden;
            changed = true;
        }
    }
    ui.horizontal(|ui| {
        ui.label("Transition");
        changed |= ui
            .add(
                egui::DragValue::new(&mut track.transition_frames)
                    .range(0..=max_frame.max(64))
                    .suffix(" frames"),
            )
            .changed();
    });
    let starts_at = track
        .fully_visible_frame
        .saturating_sub(track.transition_frames);
    ui.label(
        RichText::new(format!(
            "Starts appearing at frame {starts_at}; scrub the timeline to preview the exact runtime materialisation."
        ))
        .small()
        .color(STUDIO_TEXT_WEAK),
    );
    changed
}

fn draw_weapon_grip_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    weapon_id: ResourceId,
) -> bool {
    let Some(resource) = project.resource_mut(weapon_id) else {
        return false;
    };
    let ResourceData::Weapon(weapon) = &mut resource.data else {
        return false;
    };
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Weapon grip"))
        .default_open(true)
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Fine-tunes which point on the weapon is aligned to the hand socket.",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
            ui.horizontal(|ui| {
                ui.label("Name");
                changed |= ui.text_edit_singleline(&mut weapon.grip.name).changed();
            });
            changed |= crate::inspector_character_ui::int_vec3_editor(
                ui,
                "Position",
                &mut weapon.grip.translation,
                -32768,
                32767,
                4.0,
            );
            changed |= crate::inspector_character_ui::q12_rotation_editor(
                ui,
                "Rotation",
                &mut weapon.grip.rotation_q12,
            );
        });
    changed
}

fn draw_attachment_socket_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    model_id: ResourceId,
    character_id: Option<ResourceId>,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    max_frame: u16,
) -> bool {
    let mut changed = false;
    // Sockets have no Resize concept; fall back before the tool row
    // renders so the UI never shows a dead selection.
    if state.capsule_edit_tool == CapsuleEditTool::Resize {
        state.capsule_edit_tool = CapsuleEditTool::Move;
    }
    ui.heading("Weapon Studio");
    ui.label(
        RichText::new(
            "Choose a visibility beat, scrub to its timing, then align the character socket and weapon grip in the same preview.",
        )
        .small()
        .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);

    changed |= draw_weapon_appearance_editor(ui, project, model_id, character_id, state, max_frame);
    ui.separator();

    let joint_count = model
        .and_then(|model| psx_asset::Model::from_bytes(&model.model_bytes).ok())
        .map(|model| model.joint_count());
    let joint_names = model_skeleton_joint_names(project, model_id);

    let weapon_options: Vec<(ResourceId, String)> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Weapon(_) => Some((resource.id, resource.name.clone())),
            _ => None,
        })
        .collect();
    let appearance_controls_weapon = character_animation_set_id(project, character_id)
        .and_then(|set_id| project.resource(set_id))
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => set
                .weapon_appearance_tracks
                .get(state.selected_weapon_track),
            _ => None,
        })
        .is_some_and(|track| track.action == state.selected_action);
    if !appearance_controls_weapon {
        ui.horizontal(|ui| {
            ui.label("Preview weapon");
            let selected_label = state
                .preview_weapon
                .and_then(|id| weapon_options.iter().find(|(option, _)| *option == id))
                .map(|(_, name)| name.as_str())
                .unwrap_or("None");
            egui::ComboBox::from_id_salt("animation-socket-preview-weapon")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.preview_weapon, None, "None");
                    for (id, name) in &weapon_options {
                        ui.selectable_value(&mut state.preview_weapon, Some(*id), name);
                    }
                });
        })
        .response
        .on_hover_text("Render this Weapon riding the selected socket, composed like Play");
    }

    if let Some(weapon_id) = state.preview_weapon {
        changed |= draw_weapon_grip_editor(ui, project, weapon_id);
    }

    ui.separator();
    ui.label(RichText::new("Character socket").strong());
    ui.label(
        RichText::new(
            "Select a socket, then click a highlighted body joint to attach it. Drag in the viewport to place it.",
        )
        .small()
        .color(STUDIO_TEXT_WEAK),
    );

    let Some(resource) = project.resource_mut(model_id) else {
        ui.colored_label(Color32::from_rgb(220, 120, 100), "Model is missing");
        return false;
    };
    let ResourceData::Model(model_resource) = &mut resource.data else {
        return false;
    };
    let sockets = &mut model_resource.attachments;

    let socket_options = sockets
        .iter()
        .enumerate()
        .map(|(index, socket)| {
            (
                index,
                format!(
                    "{} · {}",
                    socket.name,
                    crate::inspector_character_ui::joint_label(
                        socket.joint,
                        joint_names.as_deref()
                    )
                ),
            )
        })
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        let selected_label = sockets
            .get(state.selected_attachment_socket)
            .map(|socket| socket.name.as_str())
            .unwrap_or("No socket");
        let mut selected = sockets
            .get(state.selected_attachment_socket)
            .map(|_| state.selected_attachment_socket);
        if searchable_picker(
            ui,
            "animation-attachment-socket",
            &mut selected,
            selected_label,
            &socket_options,
            SearchablePickerConfig::required()
                .with_width(176.0)
                .with_search_hint("Search sockets…"),
        ) {
            state.selected_attachment_socket = selected.unwrap_or(state.selected_attachment_socket);
        }
    });

    draw_axis_gizmo_controls(ui, state, false);
    ui.separator();

    changed |= crate::inspector_character_ui::attachment_socket_list_editor(
        ui,
        sockets,
        joint_count,
        joint_names.as_deref(),
    );
    if state.selected_attachment_socket >= sockets.len() {
        state.selected_attachment_socket = sockets.len().saturating_sub(1);
    }
    changed
}

fn attach_selected_socket_to_joint(
    project: &mut ProjectDocument,
    model_id: ResourceId,
    socket_index: usize,
    joint: u16,
) -> bool {
    let Some(resource) = project.resource_mut(model_id) else {
        return false;
    };
    let ResourceData::Model(model) = &mut resource.data else {
        return false;
    };
    let Some(socket) = model.attachments.get_mut(socket_index) else {
        return false;
    };
    socket.joint = joint;
    // A click on a new joint means "re-anchor here": the old bone's
    // offset is meaningless on the new bone, so drop it and let the
    // drag tools re-author it. The rotation survives because grip
    // orientation is usually rig-wide, not per-bone.
    socket.translation = [0, 0, 0];
    true
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum AxisEditDelta {
    Translate(i32),
    Rotate(Vec2),
}

fn manipulate_pose_correction(
    project: &mut ProjectDocument,
    clip_id: ResourceId,
    joint: u16,
    frame: u16,
    axis: CapsuleEditAxis,
    delta: AxisEditDelta,
) -> bool {
    let edit_amount = match delta {
        AxisEditDelta::Translate(amount) => amount,
        AxisEditDelta::Rotate(delta) => (delta.x * 12.0).round() as i32,
    };
    if edit_amount == 0 {
        return false;
    }
    let Some(resource) = project.resource_mut(clip_id) else {
        return false;
    };
    let ResourceData::AnimationClip(clip) = &mut resource.data else {
        return false;
    };
    let index = clip
        .pose_corrections
        .iter()
        .position(|key| key.joint == joint && key.frame == frame)
        .unwrap_or_else(|| {
            let mut key =
                psxed_project::sample_pose_correction(&clip.pose_corrections, joint, frame);
            key.joint = joint;
            key.frame = frame;
            clip.pose_corrections.push(key);
            clip.pose_corrections
                .sort_by_key(|key| (key.joint, key.frame));
            clip.pose_corrections
                .iter()
                .position(|key| key.joint == joint && key.frame == frame)
                .expect("inserted pose correction key")
        });
    let key = &mut clip.pose_corrections[index];
    let component = axis.index();
    match delta {
        AxisEditDelta::Translate(_) => {
            key.translation[component] = key.translation[component]
                .saturating_add(edit_amount)
                .clamp(-8192, 8192);
        }
        AxisEditDelta::Rotate(_) => {
            let turned = i32::from(key.rotation_q12[component]).saturating_add(edit_amount);
            key.rotation_q12[component] = ((turned + 2048).rem_euclid(4096) - 2048) as i16;
        }
    }
    true
}

fn manipulate_selected_socket(
    project: &mut ProjectDocument,
    model_id: ResourceId,
    socket_index: usize,
    axis: CapsuleEditAxis,
    delta: AxisEditDelta,
) -> bool {
    let Some(resource) = project.resource_mut(model_id) else {
        return false;
    };
    let ResourceData::Model(model) = &mut resource.data else {
        return false;
    };
    let Some(socket) = model.attachments.get_mut(socket_index) else {
        return false;
    };
    match delta {
        AxisEditDelta::Rotate(delta) => {
            // 12 q12 turn-units per pixel is the capsule editor's 0.018
            // radians per pixel expressed in turns.
            let amount = (delta.x * 12.0).round() as i32;
            if amount == 0 {
                return false;
            }
            let index = axis.index();
            let turned = (i32::from(socket.rotation_q12[index]) + amount).rem_euclid(4096);
            socket.rotation_q12[index] = turned as i16;
        }
        AxisEditDelta::Translate(amount) => {
            if amount == 0 {
                return false;
            }
            let index = axis.index();
            socket.translation[index] = socket.translation[index]
                .saturating_add(amount)
                .clamp(i16::MIN as i32, i16::MAX as i32);
        }
    }
    true
}

fn attach_selected_capsule_to_joint(
    project: &mut ProjectDocument,
    character_id: ResourceId,
    capsule_index: usize,
    joint: u16,
    model: Option<&LoadedModelContext>,
) -> bool {
    let Some(resource) = project.resource_mut(character_id) else {
        return false;
    };
    let ResourceData::Character(character) = &mut resource.data else {
        return false;
    };
    let Some(capsule) = character.combat_capsules.get_mut(capsule_index) else {
        return false;
    };
    capsule.joint = joint;
    if let Some(model) = model {
        if let Some(fit) = model_import_preview::fit_capsule_to_joint(
            &model.model_bytes,
            joint,
            model.visual_scale_q8,
        ) {
            capsule.capsule.start = fit.start;
            capsule.capsule.end = fit.end;
            capsule.capsule.radius = fit.radius;
        }
    }
    true
}

fn manipulate_selected_capsule(
    project: &mut ProjectDocument,
    character_id: ResourceId,
    capsule_index: usize,
    tool: CapsuleEditTool,
    axis: CapsuleEditAxis,
    delta: Vec2,
) -> bool {
    let Some(resource) = project.resource_mut(character_id) else {
        return false;
    };
    let ResourceData::Character(character) = &mut resource.data else {
        return false;
    };
    let Some(volume) = character.combat_capsules.get_mut(capsule_index) else {
        return false;
    };
    let capsule = &mut volume.capsule;
    match tool {
        CapsuleEditTool::Move => {
            let amount = ((delta.x - delta.y) * 2.0).round() as i32;
            if amount == 0 {
                return false;
            }
            let index = axis.index();
            capsule.start[index] =
                compact_capsule_coord(capsule.start[index].saturating_add(amount));
            capsule.end[index] = compact_capsule_coord(capsule.end[index].saturating_add(amount));
        }
        CapsuleEditTool::Rotate => {
            let angle = delta.x * 0.018;
            if angle.abs() < f32::EPSILON {
                return false;
            }
            let center = [
                (capsule.start[0] + capsule.end[0]) as f32 * 0.5,
                (capsule.start[1] + capsule.end[1]) as f32 * 0.5,
                (capsule.start[2] + capsule.end[2]) as f32 * 0.5,
            ];
            let sin = angle.sin();
            let cos = angle.cos();
            for point in [&mut capsule.start, &mut capsule.end] {
                let mut local = [
                    point[0] as f32 - center[0],
                    point[1] as f32 - center[1],
                    point[2] as f32 - center[2],
                ];
                let (a, b) = match axis {
                    CapsuleEditAxis::X => (1, 2),
                    CapsuleEditAxis::Y => (0, 2),
                    CapsuleEditAxis::Z => (0, 1),
                };
                let old_a = local[a];
                let old_b = local[b];
                local[a] = old_a * cos - old_b * sin;
                local[b] = old_a * sin + old_b * cos;
                for component in 0..3 {
                    point[component] = compact_capsule_coord(
                        (center[component] + local[component]).round() as i32,
                    );
                }
            }
        }
        CapsuleEditTool::Resize => {
            let radius_delta = (delta.x * 2.0).round() as i32;
            let radius = i32::from(capsule.radius)
                .saturating_add(radius_delta)
                .clamp(1, 8192);
            capsule.radius = radius as u16;

            let length_delta = (-delta.y * 2.0).round();
            if length_delta != 0.0 {
                let mut direction = [
                    (capsule.end[0] - capsule.start[0]) as f32,
                    (capsule.end[1] - capsule.start[1]) as f32,
                    (capsule.end[2] - capsule.start[2]) as f32,
                ];
                let length = (direction[0] * direction[0]
                    + direction[1] * direction[1]
                    + direction[2] * direction[2])
                    .sqrt();
                if length <= f32::EPSILON {
                    direction[axis.index()] = 1.0;
                } else {
                    for component in &mut direction {
                        *component /= length;
                    }
                }
                // `component` indexes direction plus both capsule ends.
                #[allow(clippy::needless_range_loop)]
                for component in 0..3 {
                    let half = direction[component] * length_delta * 0.5;
                    capsule.start[component] = compact_capsule_coord(
                        (capsule.start[component] as f32 - half).round() as i32,
                    );
                    capsule.end[component] = compact_capsule_coord(
                        (capsule.end[component] as f32 + half).round() as i32,
                    );
                }
            }
            if radius_delta == 0 && length_delta == 0.0 {
                return false;
            }
        }
    }
    true
}

fn compact_capsule_coord(value: i32) -> i32 {
    value.clamp(i16::MIN as i32, i16::MAX as i32)
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviewInteraction {
    clicked_joint: Option<u16>,
    edit_delta: Option<Vec2>,
    gizmo_move_units: Option<i32>,
}

fn draw_playback_controls(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    selected_model: Option<ResourceId>,
    clip: Option<&ViewerClipOption>,
    animation: Option<LoadedAnimationStats>,
) -> Option<AnimationViewerAction> {
    let mut action = None;
    let now = ui.input(|input| input.time);
    if state.last_time_seconds <= 0.0 {
        state.last_time_seconds = now;
    }
    if let Some(animation) = animation {
        let frame_count = animation.frame_count.max(1);
        if state.playing {
            let delta = (now - state.last_time_seconds).max(0.0) as f32;
            state.frame += delta * animation.sample_rate_hz as f32 * state.playback_speed.max(0.0);
            let cycle = frame_count.saturating_sub(1).max(1) as f32;
            while state.frame >= cycle {
                state.frame -= cycle;
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        state.frame = state.frame.clamp(0.0, frame_count.saturating_sub(1) as f32);
    }
    state.last_time_seconds = now;

    let Some(animation) = animation else {
        ui.add_enabled(false, egui::Button::new(icons::label(icons::PLAY, "Play")));
        if let Some(clip) = clip.filter(|clip| !clip.previewable) {
            ui.weak("Source is not baked yet");
            if let (Some(model_id), Some(source_id)) = (selected_model, clip.resource) {
                if ui
                    .button(icons::label(icons::PLUS, "Bake for Model"))
                    .clicked()
                {
                    action = Some(AnimationViewerAction::BakeSourceForModel {
                        model_id,
                        source_id,
                    });
                }
            }
        } else {
            ui.weak("No cooked animation loaded");
        }
        return action;
    };
    let max_frame = animation.frame_count.saturating_sub(1).max(1);
    let frame = state.frame.round() as u16;
    if ui
        .button(icons::text(icons::CHEVRON_LEFT, 14.0))
        .on_hover_text("Previous frame")
        .clicked()
    {
        state.frame = frame.saturating_sub(1) as f32;
        state.playing = false;
    }
    if ui
        .button(if state.playing {
            icons::text(icons::SQUARE, 14.0)
        } else {
            icons::text(icons::PLAY, 14.0)
        })
        .on_hover_text(if state.playing { "Pause" } else { "Play" })
        .clicked()
    {
        state.playing = !state.playing;
        state.last_time_seconds = now;
    }
    if ui
        .button(icons::text(icons::CHEVRON_RIGHT, 14.0))
        .on_hover_text("Next frame")
        .clicked()
    {
        state.frame = frame.saturating_add(1).min(max_frame) as f32;
        state.playing = false;
    }
    let timeline_width = ui.available_width().clamp(80.0, 110.0);
    let mut timeline_frame = state.frame.round() as u16;
    let timeline_changed = ui
        .scope(|ui| {
            ui.spacing_mut().slider_width = timeline_width;
            ui.add(egui::Slider::new(&mut timeline_frame, 0..=max_frame).show_value(false))
                .changed()
        })
        .inner;
    if timeline_changed {
        state.frame = timeline_frame as f32;
        state.playing = false;
    }
    ui.label(
        RichText::new(format!("{frame}/{max_frame}"))
            .monospace()
            .color(STUDIO_TEXT_WEAK),
    )
    .on_hover_text(format!(
        "Frame {frame} of {max_frame} · {} Hz",
        animation.sample_rate_hz
    ));
    ui.add(
        egui::DragValue::new(&mut state.playback_speed)
            .speed(0.05)
            .range(0.1..=2.0)
            .fixed_decimals(1)
            .suffix("×"),
    )
    .on_hover_text("Playback speed");
    action
}

fn draw_clip_calibration_menu(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    selected_model: Option<ResourceId>,
    clip: Option<&mut ViewerClipOption>,
) -> bool {
    let in_place = clip
        .as_deref()
        .is_some_and(|clip| clip.calibration.in_place);
    let current = if in_place { "In-place" } else { "Root motion" };
    let menu = egui::menu::menu_custom_button(
        ui,
        egui::Button::new(icons::text(icons::MOVE, 14.0))
            .selected(in_place)
            .min_size(Vec2::new(30.0, 23.0)),
        |ui| {
            ui.set_min_width(330.0);
            ui.horizontal(|ui| draw_selected_clip_calibration(ui, project, selected_model, clip))
                .inner
        },
    );
    let changed = menu.inner.unwrap_or(false);
    menu.response
        .on_hover_text(format!("Root-motion placement · {current}"));
    changed
}

fn draw_selected_clip_calibration(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    selected_model: Option<ResourceId>,
    clip: Option<&mut ViewerClipOption>,
) -> bool {
    let Some(clip) = clip else {
        return false;
    };
    let editable = clip_calibration_editable(project, selected_model, clip);
    let mut calibration = clip.calibration;
    let mut changed = false;
    ui.label(RichText::new("Root").color(STUDIO_TEXT_WEAK));
    changed |= ui
        .add_enabled(
            editable,
            egui::Checkbox::new(&mut calibration.in_place, "In-place"),
        )
        .on_hover_text("Cancels this clip's root translation while previewing and at runtime")
        .changed();
    for (axis, label) in ["X", "Y", "Z"].iter().enumerate() {
        ui.label(RichText::new(*label).color(STUDIO_TEXT_WEAK));
        changed |= ui
            .add_enabled(
                editable,
                egui::DragValue::new(&mut calibration.offset[axis])
                    .speed(4.0)
                    .range(-8192..=8192),
            )
            .changed();
    }
    if ui
        .add_enabled(
            editable,
            egui::Button::new(icons::text(icons::ROTATE_CCW, 13.0)),
        )
        .on_hover_text("Reset root motion placement")
        .clicked()
    {
        calibration = AnimationClipCalibration::default();
        changed = true;
    }
    if !editable {
        ui.weak("Bake source to edit placement");
    }
    if changed && store_clip_calibration(project, selected_model, clip, calibration) {
        clip.calibration = calibration;
        return true;
    }
    false
}

fn clip_calibration_editable(
    project: &ProjectDocument,
    selected_model: Option<ResourceId>,
    clip: &ViewerClipOption,
) -> bool {
    if let Some(resource_id) = clip.resource {
        if project
            .resource(resource_id)
            .is_some_and(|resource| matches!(&resource.data, ResourceData::AnimationClip(_)))
        {
            return true;
        }
    }
    selected_model.is_some()
        && clip.model_clip_index.is_some()
        && selected_model.is_some_and(|model_id| {
            project
                .resource(model_id)
                .is_some_and(|resource| matches!(&resource.data, ResourceData::Model(_)))
        })
}

fn store_clip_calibration(
    project: &mut ProjectDocument,
    selected_model: Option<ResourceId>,
    clip: &ViewerClipOption,
    calibration: AnimationClipCalibration,
) -> bool {
    if let Some(resource_id) = clip.resource {
        if let Some(resource) = project.resource_mut(resource_id) {
            if let ResourceData::AnimationClip(animation) = &mut resource.data {
                animation.calibration = calibration;
                return true;
            }
        }
    }
    // Clips live on AnimationClip resources now; without a backing
    // resource there is nowhere to store calibration.
    let _ = (selected_model, clip);
    false
}

fn draw_preview_toolbar(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
) {
    egui::ComboBox::from_id_salt("animation-camera-preset")
        .selected_text(icons::text(icons::ROTATE_3D, 14.0))
        .width(28.0)
        .show_ui(ui, |ui| {
            if ui.button("Perspective").clicked() {
                set_camera_preset(state, 340, 350);
                ui.close_menu();
            }
            if ui.button("Front").clicked() {
                set_camera_preset(state, 0, 96);
                ui.close_menu();
            }
            if ui.button("Side").clicked() {
                set_camera_preset(state, 1024, 96);
                ui.close_menu();
            }
            if ui.button("Top").clicked() {
                set_camera_preset(state, 0, 900);
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(icons::label(icons::ROTATE_CCW, "Reset orbit"))
                .clicked()
            {
                state.yaw_q12 = 340;
                state.pitch_q12 = 350;
                state.radius = 0;
                ui.close_menu();
            }
        });
    egui::ComboBox::from_id_salt("animation-preview-quality")
        .selected_text(match state.preview_quality {
            AnimationPreviewQuality::Authoring => "HQ",
            AnimationPreviewQuality::PsxOutput => "PS1",
        })
        .width(48.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut state.preview_quality,
                AnimationPreviewQuality::Authoring,
                "Authoring",
            )
            .on_hover_text("High-resolution authoring preview with smooth display scaling");
            ui.selectable_value(
                &mut state.preview_quality,
                AnimationPreviewQuality::PsxOutput,
                "PS1 Output",
            )
            .on_hover_text("Exact 320 × 240 preview with nearest-neighbour scaling");
        });
    ui.toggle_value(&mut state.show_bones, icons::text(icons::WAYPOINT, 14.0))
        .on_hover_text("Draw the cooked skeleton overlay");
    ui.toggle_value(
        &mut state.show_animation_root,
        icons::text(icons::CIRCLE_DOT, 14.0),
    )
    .on_hover_text("Draw the body-derived preview anchor");
    if let Some(model) = model {
        ui.label(icons::text(icons::CIRCLE_DOT, 13.0).color(STUDIO_ACCENT))
            .on_hover_text(format!(
                "{}\nUsing the same authored presentation transform as the 3D view",
                model.orientation_label
            ));
    }
}

fn set_camera_preset(state: &mut ModelAnimationViewerState, yaw_q12: i32, pitch_q12: i32) {
    state.yaw_q12 = yaw_q12;
    state.pitch_q12 = pitch_q12;
    state.radius = 0;
}

#[allow(clippy::too_many_arguments)]
fn draw_preview(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    selected_clip: Option<&ViewerClipOption>,
    clip: Option<&LoadedClipContext>,
    preview_texture: &mut Option<egui::TextureHandle>,
    combat_capsules: &[model_import_preview::PreviewCombatCapsule],
    sockets: &[model_import_preview::PreviewSocket],
    equipped_weapon: Option<&model_import_preview::PreviewEquippedWeapon<'_>>,
    character_material: Option<&model_import_preview::PreviewMaterialLayer<'_>>,
    selected_joint: Option<u16>,
    joint_picking: bool,
) -> PreviewInteraction {
    let size = ui.available_size();
    let size = Vec2::new(size.x.max(360.0), size.y.max(260.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let pointer_delta = ui.input(|input| input.pointer.delta());
    let orbiting = response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
        || (!joint_picking && response.dragged_by(egui::PointerButton::Primary));
    if orbiting {
        let delta = pointer_delta;
        state.yaw_q12 = (state.yaw_q12 - (delta.x * 6.0) as i32).rem_euclid(4096);
        state.pitch_q12 = (state.pitch_q12 - (delta.y * 4.0) as i32).clamp(64, 960);
    }
    let primary_drag_delta = (response.dragged_by(egui::PointerButton::Primary)
        && pointer_delta != Vec2::ZERO)
        .then_some(pointer_delta);
    let mut edit_delta = (!combat_capsules.is_empty()
        && response.dragged_by(egui::PointerButton::Primary)
        && pointer_delta != Vec2::ZERO)
        .then_some(pointer_delta);
    let primary_down = ui.input(|input| input.pointer.primary_down());
    if !primary_down {
        state.gizmo_drag_axis = None;
        state.gizmo_drag_fractional_units = 0.0;
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(if joint_picking {
            egui::CursorIcon::Crosshair
        } else {
            egui::CursorIcon::Grab
        });
        let scroll = ui.input(|input| input.raw_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            let current = effective_radius(state, model);
            state.radius = (current - (scroll * 8.0) as i32).clamp(640, 8192);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, STUDIO_PANEL_DARK);

    let Some(model) = model else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Select a model",
            FontId::proportional(14.0),
            STUDIO_TEXT_WEAK,
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    };
    let Some(selected_clip) = selected_clip else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Select an animation",
            FontId::proportional(14.0),
            STUDIO_TEXT_WEAK,
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    };
    if !selected_clip.previewable {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Animation source needs baking before preview",
            FontId::proportional(14.0),
            STUDIO_TEXT_WEAK,
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    }
    let Some(clip) = clip else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Animation clip failed to load",
            FontId::proportional(14.0),
            Color32::from_rgb(220, 120, 100),
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    };
    let Some(atlas) = model.atlas.as_ref() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Model atlas is missing",
            FontId::proportional(14.0),
            Color32::from_rgb(220, 120, 100),
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    };
    let Some(animation) = clip.animation_stats.as_ref() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Animation parse failed",
            FontId::proportional(14.0),
            Color32::from_rgb(220, 120, 100),
        );
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, STUDIO_BORDER),
            StrokeKind::Inside,
        );
        return PreviewInteraction::default();
    };

    let seconds = state.frame.max(0.0) as f64 / animation.sample_rate_hz.max(1) as f64;
    let render_size = preview_render_size(ui, rect, state.preview_quality);
    let texture_options = match state.preview_quality {
        AnimationPreviewQuality::Authoring => egui::TextureOptions::LINEAR,
        AnimationPreviewQuality::PsxOutput => egui::TextureOptions::NEAREST,
    };
    let render = model_import_preview::render_import_model_preview_with_combat_capsules_at_size(
        &model.model_bytes,
        &clip.bytes,
        atlas,
        ImportPreviewOptions {
            world_height: model.world_height as i32,
            visual_scale_q8: model.visual_scale_q8,
            visual_yaw_q12: model.default_visual_yaw_q12,
            collision_radius: model.collision_radius as i32,
            time_seconds: seconds,
            yaw_q12: state.yaw_q12.rem_euclid(4096) as u16,
            pitch_q12: state.pitch_q12.rem_euclid(4096) as u16,
            radius: state.radius,
            focus_on_animated_bounds: true,
            preview_in_place: selected_clip.calibration.in_place,
            pose_offset: selected_clip.calibration.offset,
            show_animation_root: state.show_animation_root,
            show_collision_guides: false,
            show_bones: state.show_bones || joint_picking,
        },
        model_import_preview::euler_rotation_q12(model.authored_rotation_q12),
        render_size,
        combat_capsules,
        sockets,
        equipped_weapon,
        character_material,
        selected_joint,
        state.show_pose_corrections.then_some(
            if state.capsule_edit_tool == CapsuleEditTool::Rotate {
                model_import_preview::PreviewGizmoMode::Rotate
            } else {
                model_import_preview::PreviewGizmoMode::Translate
            },
        ),
    );

    let mut clicked_joint = None;
    let mut gizmo_move_units = None;
    match render {
        Some(render) => {
            let viewport_gizmo = render.selected_socket_gizmo.or(render.selected_joint_gizmo);
            let image = render.image;
            let texture_id = match preview_texture {
                Some(handle) => {
                    handle.set(image, texture_options);
                    handle.id()
                }
                None => {
                    let handle = ui.ctx().load_texture(
                        "model-animation-viewer-preview",
                        image,
                        texture_options,
                    );
                    let id = handle.id();
                    *preview_texture = Some(handle);
                    id
                }
            };
            let preview_rect = centered_aspect_rect(
                rect.shrink(8.0),
                render_size[0] as f32 / render_size[1] as f32,
            );
            painter.image(
                texture_id,
                preview_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            let hovered_socket_axis = response
                .hovered()
                .then(|| response.interact_pointer_pos())
                .flatten()
                .and_then(|pointer| {
                    viewport_gizmo.and_then(|gizmo| {
                        pick_axis_gizmo(pointer, preview_rect, render_size, gizmo)
                    })
                });
            if response.drag_started_by(egui::PointerButton::Primary) {
                let pressed_axis = viewport_gizmo.and_then(|gizmo| {
                    ui.input(|input| input.pointer.press_origin())
                        .and_then(|pointer| {
                            pick_axis_gizmo(pointer, preview_rect, render_size, gizmo)
                        })
                });
                state.gizmo_drag_axis = pressed_axis;
                if let Some(axis) = pressed_axis {
                    state.capsule_edit_axis = axis;
                    state.gizmo_drag_fractional_units = 0.0;
                }
            }
            if response.clicked() {
                if let Some(axis) = hovered_socket_axis {
                    state.capsule_edit_axis = axis;
                }
            }
            if let Some(gizmo) = viewport_gizmo {
                draw_axis_gizmo_overlay(
                    &painter,
                    preview_rect,
                    render_size,
                    gizmo,
                    hovered_socket_axis,
                    state.gizmo_drag_axis,
                );
            }
            if response.hovered() {
                ui.ctx()
                    .set_cursor_icon(if state.gizmo_drag_axis.is_some() {
                        egui::CursorIcon::Grabbing
                    } else if hovered_socket_axis.is_some() {
                        egui::CursorIcon::PointingHand
                    } else if joint_picking {
                        egui::CursorIcon::Crosshair
                    } else {
                        egui::CursorIcon::Grab
                    });
            }
            if let (Some(delta), Some(axis), Some(gizmo)) =
                (primary_drag_delta, state.gizmo_drag_axis, viewport_gizmo)
            {
                state.capsule_edit_axis = axis;
                match state.capsule_edit_tool {
                    CapsuleEditTool::Move => {
                        let modifiers = ui.input(|input| input.modifiers);
                        let speed = if modifiers.shift {
                            0.25
                        } else if modifiers.command || modifiers.ctrl {
                            4.0
                        } else {
                            1.0
                        };
                        if let Some(units) =
                            axis_gizmo_drag_units(delta, preview_rect, render_size, gizmo, axis)
                        {
                            let accumulated = units * speed + state.gizmo_drag_fractional_units;
                            let rounded = accumulated.round() as i32;
                            state.gizmo_drag_fractional_units = accumulated - rounded as f32;
                            gizmo_move_units = (rounded != 0).then_some(rounded);
                        }
                    }
                    CapsuleEditTool::Rotate => edit_delta = Some(delta),
                    CapsuleEditTool::Resize => {}
                }
            }
            if response.clicked() && joint_picking && hovered_socket_axis.is_none() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    clicked_joint = nearest_preview_joint(
                        pointer,
                        preview_rect,
                        render_size,
                        &render.joint_screen_positions,
                    );
                }
            }
        }
        None => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Preview render failed",
                FontId::proportional(14.0),
                Color32::from_rgb(220, 120, 100),
            );
        }
    }
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, STUDIO_BORDER),
        StrokeKind::Inside,
    );
    PreviewInteraction {
        clicked_joint,
        edit_delta,
        gizmo_move_units,
    }
}

#[derive(Clone, Copy, Debug)]
struct AxisGizmoScreenAxis {
    axis: CapsuleEditAxis,
    start: Pos2,
    end: Pos2,
}

fn preview_image_to_screen(point: [f32; 2], preview_rect: Rect, render_size: [usize; 2]) -> Pos2 {
    Pos2::new(
        preview_rect.left() + point[0] / render_size[0].max(1) as f32 * preview_rect.width(),
        preview_rect.top() + point[1] / render_size[1].max(1) as f32 * preview_rect.height(),
    )
}

fn axis_gizmo_screen_axes(
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
) -> Vec<AxisGizmoScreenAxis> {
    let start = preview_image_to_screen(gizmo.origin, preview_rect, render_size);
    CapsuleEditAxis::ALL
        .into_iter()
        .filter_map(|axis| {
            let end =
                preview_image_to_screen(gizmo.axis_ends[axis.index()], preview_rect, render_size);
            ((end - start).length_sq() >= 64.0).then_some(AxisGizmoScreenAxis { axis, start, end })
        })
        .collect()
}

fn pick_axis_gizmo(
    pointer: Pos2,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
) -> Option<CapsuleEditAxis> {
    axis_gizmo_screen_axes(preview_rect, render_size, gizmo)
        .into_iter()
        .filter_map(|screen_axis| {
            let distance = distance_to_segment_2d(pointer, screen_axis.start, screen_axis.end)
                .min(pointer.distance(screen_axis.end));
            (distance <= crate::GIZMO_AXIS_PICK_RADIUS).then_some((distance, screen_axis.axis))
        })
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, axis)| axis)
}

fn draw_axis_gizmo_overlay(
    painter: &egui::Painter,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
    hovered_axis: Option<CapsuleEditAxis>,
    active_axis: Option<CapsuleEditAxis>,
) {
    let axes = axis_gizmo_screen_axes(preview_rect, render_size, gizmo);
    let Some(origin) = axes.first().map(|axis| axis.start) else {
        return;
    };
    painter.circle_filled(origin, 4.0, Color32::from_rgb(235, 242, 248));
    for screen_axis in axes {
        let highlighted =
            hovered_axis == Some(screen_axis.axis) || active_axis == Some(screen_axis.axis);
        let color = crate::gizmo::gizmo_highlight_color(screen_axis.axis.color(), highlighted);
        painter.line_segment(
            [screen_axis.start, screen_axis.end],
            Stroke::new(crate::gizmo::gizmo_axis_stroke_width(highlighted), color),
        );
        painter.circle_filled(
            screen_axis.end,
            crate::gizmo::gizmo_axis_handle_radius(highlighted),
            color,
        );
        let label_offset = (screen_axis.end - screen_axis.start).normalized() * 12.0;
        painter.text(
            screen_axis.end + label_offset,
            Align2::CENTER_CENTER,
            screen_axis.axis.label(),
            FontId::monospace(12.0),
            color,
        );
    }
}

fn axis_gizmo_drag_units(
    pointer_delta: Vec2,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
    axis: CapsuleEditAxis,
) -> Option<f32> {
    let scale = Vec2::new(
        preview_rect.width() / render_size[0].max(1) as f32,
        preview_rect.height() / render_size[1].max(1) as f32,
    );
    let end = gizmo.axis_ends[axis.index()];
    let screen_axis = Vec2::new(
        (end[0] - gizmo.origin[0]) * scale.x,
        (end[1] - gizmo.origin[1]) * scale.y,
    );
    let length_sq = screen_axis.length_sq();
    if length_sq < 16.0 {
        return None;
    }
    Some(pointer_delta.dot(screen_axis) * gizmo.local_axis_units / length_sq)
}

fn nearest_preview_joint(
    pointer: Pos2,
    preview_rect: Rect,
    render_size: [usize; 2],
    joints: &[Option<[f32; 2]>],
) -> Option<u16> {
    let mut best: Option<(u16, f32)> = None;
    for (index, joint) in joints.iter().enumerate() {
        let Some([x, y]) = joint else {
            continue;
        };
        let screen = Pos2::new(
            preview_rect.left() + (*x / render_size[0].max(1) as f32) * preview_rect.width(),
            preview_rect.top() + (*y / render_size[1].max(1) as f32) * preview_rect.height(),
        );
        let distance_sq = pointer.distance_sq(screen);
        if distance_sq <= 20.0 * 20.0 && best.is_none_or(|(_, best)| distance_sq < best) {
            best = Some((index.min(u16::MAX as usize) as u16, distance_sq));
        }
    }
    best.map(|(joint, _)| joint)
}

fn preview_render_size(ui: &egui::Ui, rect: Rect, quality: AnimationPreviewQuality) -> [usize; 2] {
    if quality == AnimationPreviewQuality::PsxOutput {
        return [
            model_import_preview::PREVIEW_WIDTH,
            model_import_preview::PREVIEW_HEIGHT,
        ];
    }

    let available = centered_aspect_rect(rect.shrink(8.0), 4.0 / 3.0);
    let pixels_per_point = ui.ctx().pixels_per_point().max(1.0);
    let mut width = (available.width() * pixels_per_point).round() as usize;
    let mut height = (available.height() * pixels_per_point).round() as usize;
    width = width.clamp(
        model_import_preview::PREVIEW_WIDTH,
        model_import_preview::AUTHORING_PREVIEW_MAX_WIDTH,
    );
    height = height.clamp(
        model_import_preview::PREVIEW_HEIGHT,
        model_import_preview::AUTHORING_PREVIEW_MAX_HEIGHT,
    );
    let aspect_height = width.saturating_mul(3) / 4;
    if aspect_height <= model_import_preview::AUTHORING_PREVIEW_MAX_HEIGHT {
        height = aspect_height.max(model_import_preview::PREVIEW_HEIGHT);
    } else {
        width = height.saturating_mul(4) / 3;
    }
    [width, height]
}

fn resource_combo(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &'static str,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK));
        let selected = current
            .and_then(|id| options.iter().find(|(rid, _)| *rid == id))
            .map(|(_, name)| name.as_str())
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            id_salt,
            current,
            selected,
            options,
            SearchablePickerConfig::optional("(none)")
                .with_width(120.0)
                .with_popup_min_width(360.0),
        );
    });
    changed
}

fn clip_combo(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    options: &[ViewerClipOption],
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Clip").color(STUDIO_TEXT_WEAK));
        let selected = state
            .selected_clip_path
            .as_ref()
            .and_then(|path| options.iter().find(|option| option.path == *path))
            .map(|option| option.label.as_str())
            .unwrap_or("(none)");
        egui::ComboBox::from_id_salt("animation-viewer-clip")
            .selected_text(selected)
            .width(164.0)
            .height(420.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(460.0);
                ui.add(
                    egui::TextEdit::singleline(&mut state.clip_filter)
                        .hint_text("Search filename, take, role, or collection…")
                        .desired_width(f32::INFINITY),
                );
                let matching = options
                    .iter()
                    .filter(|option| animation_viewer_option_matches(option, &state.clip_filter))
                    .count();
                ui.label(
                    RichText::new(format!("{matching} of {} imported clips", options.len()))
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
                ui.separator();
                let filter = state.clip_filter.clone();
                for option in options
                    .iter()
                    .filter(|option| animation_viewer_option_matches(option, &filter))
                {
                    let selected = state.selected_clip_path.as_deref() == Some(option.path.as_str());
                    let suffix = if option.previewable {
                        option.origin.label().to_string()
                    } else {
                        format!("{} · source only", option.origin.label())
                    };
                    let response = ui.selectable_label(
                        selected,
                        format!("{} · {}", option.label, suffix),
                    );
                    let resource = option
                        .resource
                        .map(|id| format!("resource #{}", id.raw()))
                        .unwrap_or_else(|| "model-local clip".to_string());
                    let response = response.on_hover_text(format!(
                        "{} · {} · {} · {}",
                        option.origin.label(),
                        option.role.label(),
                        if option.looping { "looping" } else { "one-shot" },
                        resource,
                    ));
                    let response = if option.previewable {
                        response
                    } else {
                        response.on_hover_text(
                            "Catalogued source only. Bake or retarget it before previewing on this model.",
                        )
                    };
                    if response.clicked() {
                        state.selected_clip_path = Some(option.path.clone());
                        state.reset_clip_clock();
                    }
                }
            });
    });
}

fn animation_viewer_option_matches(option: &ViewerClipOption, filter: &str) -> bool {
    let filter = filter.trim().to_ascii_lowercase();
    if filter.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {}",
        option.label,
        option.path,
        option.role.label(),
        option.origin.label()
    )
    .to_ascii_lowercase();
    filter
        .split_whitespace()
        .all(|term| haystack.contains(term))
}

fn collect_resource_options(
    project: &ProjectDocument,
    matches: impl Fn(&ResourceData) -> bool,
) -> Vec<(ResourceId, String)> {
    project
        .resources
        .iter()
        .filter(|resource| matches(&resource.data))
        .map(|resource| (resource.id, resource.name.clone()))
        .collect()
}

fn first_model_id(project: &ProjectDocument) -> Option<ResourceId> {
    project
        .resources
        .iter()
        .find_map(|resource| matches!(resource.data, ResourceData::Model(_)).then_some(resource.id))
}

fn build_clip_options(project: &ProjectDocument, model_id: ResourceId) -> Vec<ViewerClipOption> {
    let mut out = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut seen_resources = HashSet::new();
    let mut baked_sources = HashSet::new();
    let authoring_labels = collect_animation_clip_authoring_labels(project);

    for clip in project.resolved_model_animation_clips(model_id) {
        let label = clip.animation_resource.map_or_else(
            || clip.name.clone(),
            |clip_id| {
                authoring_labels
                    .get(&clip_id)
                    .cloned()
                    .unwrap_or_else(|| clip.name.clone())
            },
        );
        let (role, looping, origin) = clip
            .animation_resource
            .and_then(|id| project.resource(id).map(|resource| (id, resource)))
            .and_then(|(id, resource)| match &resource.data {
                ResourceData::AnimationClip(clip) => {
                    seen_resources.insert(id);
                    if let Some(source) = clip.source {
                        baked_sources.insert(source);
                    }
                    Some((clip.role, clip.looping, ClipOrigin::Library))
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let role = AnimationRole::guess_from_name(&clip.name);
                (role, role_loops_by_default(role), ClipOrigin::Model)
            });
        seen_paths.insert(clip.psxanim_path.clone());
        out.push(ViewerClipOption {
            label,
            path: clip.psxanim_path,
            origin,
            role,
            looping,
            resource: clip.animation_resource,
            model_clip_index: clip.model_clip_index,
            calibration: clip.calibration,
            previewable: true,
        });
    }

    for resource in &project.resources {
        let ResourceData::AnimationClip(clip) = &resource.data else {
            continue;
        };
        if seen_resources.contains(&resource.id) || seen_paths.contains(&clip.psxanim_path) {
            continue;
        }
        seen_resources.insert(resource.id);
        if let Some(source) = clip.source {
            baked_sources.insert(source);
        }
        seen_paths.insert(clip.psxanim_path.clone());
        out.push(ViewerClipOption {
            label: authoring_labels
                .get(&resource.id)
                .cloned()
                .unwrap_or_else(|| resource.name.clone()),
            path: clip.psxanim_path.clone(),
            origin: ClipOrigin::Library,
            role: clip.role,
            looping: clip.looping,
            resource: Some(resource.id),
            model_clip_index: None,
            calibration: clip.calibration,
            previewable: true,
        });
    }

    for resource in &project.resources {
        let ResourceData::AnimationSource(source) = &resource.data else {
            continue;
        };
        if baked_sources.contains(&resource.id) {
            continue;
        }
        if seen_paths.contains(&source.source_path) {
            continue;
        }
        out.push(ViewerClipOption {
            label: animation_source_authoring_label(source, &resource.name),
            path: source.source_path.clone(),
            origin: ClipOrigin::Source,
            role: source.role,
            looping: source.looping,
            resource: Some(resource.id),
            model_clip_index: None,
            calibration: AnimationClipCalibration::default(),
            previewable: is_cooked_animation_path(&source.source_path),
        });
    }

    out
}

/// Roles that default to looping playback in the animation viewer.
fn role_loops_by_default(role: AnimationRole) -> bool {
    matches!(
        role,
        AnimationRole::Idle | AnimationRole::Walk | AnimationRole::Run | AnimationRole::Turn
    )
}

/// Cached resolved character material: atlas image plus UV motion,
/// invalidated by the resolver's cache key (which embeds generated
/// settings, so Material Lab edits refresh the preview immediately).
#[derive(Debug, Clone)]
struct CachedMaterialLayer {
    material: ResourceId,
    key: String,
    atlas: Arc<ColorImage>,
    motion: psx_level::LevelMaterialUvMotion,
}

#[derive(Debug, Clone)]
struct LoadedModelContext {
    model_bytes: Vec<u8>,
    model_stamp: FileStamp,
    atlas: Option<ColorImage>,
    world_height: u16,
    collision_radius: u16,
    visual_scale_q8: u16,
    default_visual_yaw_q12: i16,
    authored_rotation_q12: [u16; 3],
    orientation_label: String,
}

fn load_model_context_cached(
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    id: ResourceId,
) -> Option<Arc<LoadedModelContext>> {
    load_model_context_into(project, project_root, &mut state.cached_model, id)
}

/// The material the previewed character renders with in Play: the
/// selected Character's override first, else the material on a scene
/// ModelRenderer showing the selected model (same fallback the
/// orientation probe uses). None means the model's own atlas.
fn preview_material_id(
    project: &ProjectDocument,
    state: &ModelAnimationViewerState,
) -> Option<ResourceId> {
    if let Some(character_id) = state.selected_character {
        if let Some(resource) = project.resource(character_id) {
            if let ResourceData::Character(character) = &resource.data {
                if let Some(material) = character.material {
                    return Some(material);
                }
            }
        }
    }
    let model_id = state.selected_model?;
    let scene = project.active_scene();
    for node in scene.nodes() {
        if let NodeKind::ModelRenderer {
            model: Some(model),
            material: Some(material),
            ..
        } = &node.kind
        {
            if *model == model_id {
                return Some(*material);
            }
        }
    }
    None
}

fn material_uv_motion(
    project: &ProjectDocument,
    material_id: ResourceId,
) -> psx_level::LevelMaterialUvMotion {
    let fallback = psx_level::LevelMaterialUvMotion::default();
    let Some(resource) = project.resource(material_id) else {
        return fallback;
    };
    let ResourceData::Material(material) = &resource.data else {
        return fallback;
    };
    if material.animation.mode != psxed_project::MaterialAnimationMode::UvScroll {
        return fallback;
    }
    let scroll = material.animation.uv_scroll;
    psx_level::LevelMaterialUvMotion {
        enabled: scroll.enabled,
        speed_u_q8: scroll.speed_u_q8,
        speed_v_q8: scroll.speed_v_q8,
        phase_u: scroll.phase_u,
        phase_v: scroll.phase_v,
    }
}

/// Resolve and cache the preview material layer (any texture mode,
/// including Generated, via the cook's own resolver).
fn preview_material_layer_cached(
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
) -> Option<(Arc<ColorImage>, psx_level::LevelMaterialUvMotion)> {
    let material_id = preview_material_id(project, state)?;
    let (key, bytes) =
        psxed_project::resolve_material_texture_psxt(project, material_id, project_root)
            .ok()
            .flatten()?;
    if let Some(cached) = &state.cached_material {
        if cached.material == material_id && cached.key == key {
            return Some((Arc::clone(&cached.atlas), cached.motion));
        }
    }
    let atlas = Arc::new(decode_psxt_image(&bytes)?);
    let motion = material_uv_motion(project, material_id);
    state.cached_material = Some(CachedMaterialLayer {
        material: material_id,
        key,
        atlas: Arc::clone(&atlas),
        motion,
    });
    Some((atlas, motion))
}

/// The weapon-preview overlay keeps its own cache slot so switching
/// between weapon and character never thrashes either decode.
fn load_weapon_model_context(
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    id: ResourceId,
) -> Option<Arc<LoadedModelContext>> {
    load_model_context_into(project, project_root, &mut state.cached_weapon_model, id)
}

fn load_model_context_into(
    project: &ProjectDocument,
    project_root: &Path,
    cache: &mut Option<CachedModelContext>,
    id: ResourceId,
) -> Option<Arc<LoadedModelContext>> {
    let resource = project.resource(id)?;
    let ResourceData::Model(model_resource) = &resource.data else {
        return None;
    };
    let model_path = resolve_path(&model_resource.model_path, Some(project_root));
    let model_stamp = FileStamp::read(model_path.clone())?;
    let atlas_path = model_resource
        .texture_path
        .as_ref()
        .map(|path| resolve_path(path, Some(project_root)));
    let atlas_stamp = atlas_path.clone().and_then(FileStamp::read);
    let (authored_rotation_q12, orientation_label) = model_scene_orientation(project, id)
        .unwrap_or_else(|| {
            (
                [
                    0,
                    (model_resource.default_visual_yaw_q12 as i32).rem_euclid(4096) as u16,
                    0,
                ],
                "Model import orientation".to_string(),
            )
        });
    let visual_scale_q8 = model_resource.scale_q8[1].max(1);

    if let Some(cached) = cache.as_ref() {
        if cached.resource == id
            && cached.model_stamp == model_stamp
            && cached.atlas_stamp == atlas_stamp
            && cached.authored_rotation_q12 == authored_rotation_q12
            && cached.world_height == model_resource.world_height
            && cached.collision_radius == model_resource.collision_radius
            && cached.visual_scale_q8 == visual_scale_q8
        {
            return Some(Arc::clone(&cached.context));
        }
    }

    let model_bytes = std::fs::read(model_path).ok()?;
    let atlas = atlas_path
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| decode_psxt_image(&bytes));
    let context = Arc::new(LoadedModelContext {
        model_bytes,
        model_stamp: model_stamp.clone(),
        atlas,
        world_height: model_resource.world_height,
        collision_radius: model_resource.collision_radius,
        visual_scale_q8,
        default_visual_yaw_q12: model_resource.default_visual_yaw_q12,
        authored_rotation_q12,
        orientation_label,
    });
    *cache = Some(CachedModelContext {
        resource: id,
        model_stamp,
        atlas_stamp,
        authored_rotation_q12,
        world_height: model_resource.world_height,
        collision_radius: model_resource.collision_radius,
        visual_scale_q8,
        context: Arc::clone(&context),
    });
    Some(context)
}

fn model_scene_orientation(
    project: &ProjectDocument,
    model_id: ResourceId,
) -> Option<([u16; 3], String)> {
    let scene = project.active_scene();
    for node in scene.nodes() {
        let rotation_degrees = match &node.kind {
            NodeKind::MeshInstance {
                mesh: Some(resource),
                ..
            } if *resource == model_id => node.transform.rotation_degrees,
            NodeKind::Entity => {
                let renderer_yaw = node.children.iter().find_map(|child_id| {
                    let child = scene.node(*child_id)?;
                    match &child.kind {
                        NodeKind::ModelRenderer {
                            model: Some(resource),
                            ..
                        } if *resource == model_id => Some(child.transform.rotation_degrees[1]),
                        _ => None,
                    }
                });
                let Some(renderer_yaw) = renderer_yaw else {
                    continue;
                };
                [
                    node.transform.rotation_degrees[0],
                    node.transform.rotation_degrees[1] + renderer_yaw,
                    node.transform.rotation_degrees[2],
                ]
            }
            _ => continue,
        };
        let rotation_q12 = rotation_degrees.map(psxed_project::spatial::euler_degrees_to_q12);
        return Some((
            rotation_q12,
            format!(
                "Scene orientation · {} · X {:+.0}°  Y {:+.0}°  Z {:+.0}°",
                node.name, rotation_degrees[0], rotation_degrees[1], rotation_degrees[2]
            ),
        ));
    }
    None
}

#[derive(Debug, Clone)]
struct LoadedClipContext {
    bytes: Vec<u8>,
    animation_stats: Option<LoadedAnimationStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadedAnimationStats {
    frame_count: u16,
    sample_rate_hz: u16,
}

fn load_clip_context_cached(
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    clip: &ViewerClipOption,
    model: Option<&LoadedModelContext>,
) -> Option<Arc<LoadedClipContext>> {
    if !clip.previewable {
        return None;
    }
    let path = resolve_path(&clip.path, Some(project_root));
    let stamp = FileStamp::read(path.clone())?;
    let pose_corrections = clip
        .resource
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationClip(animation) => Some(animation.pose_corrections.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let model_stamp = model.map(|model| model.model_stamp.clone());
    if let Some(cached) = &state.cached_clip {
        if cached.path == clip.path
            && cached.stamp == stamp
            && cached.pose_corrections == pose_corrections
            && cached.model_stamp == model_stamp
        {
            return Some(Arc::clone(&cached.context));
        }
    }
    let base_bytes = std::fs::read(path).ok()?;
    let bytes = if pose_corrections.is_empty() {
        base_bytes
    } else {
        match (model, Animation::from_bytes(&base_bytes)) {
            (Some(model), Ok(animation)) => {
                match psx_asset::Model::from_bytes(&model.model_bytes) {
                    Ok(parsed_model) => psxed_project::bake_animation_pose_corrections(
                        &parsed_model,
                        &animation,
                        &pose_corrections,
                    ),
                    Err(_) => base_bytes,
                }
            }
            _ => base_bytes,
        }
    };
    let animation_stats =
        Animation::from_bytes(&bytes)
            .ok()
            .map(|animation| LoadedAnimationStats {
                frame_count: animation.frame_count(),
                sample_rate_hz: animation.sample_rate_hz(),
            });
    let context = Arc::new(LoadedClipContext {
        bytes,
        animation_stats,
    });
    state.cached_clip = Some(CachedClipContext {
        path: clip.path.clone(),
        stamp,
        pose_corrections,
        model_stamp,
        context: Arc::clone(&context),
    });
    Some(context)
}

fn is_cooked_animation_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".psxanim") && !path.contains("::")
}

pub(crate) fn decode_psxt_image(bytes: &[u8]) -> Option<ColorImage> {
    let texture = Texture::from_bytes(bytes).ok()?;
    let width = texture.width() as usize;
    let height = texture.height() as usize;
    let clut_entries = texture.clut_entries() as usize;
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = width.checked_mul(height)?;
    let pixel_bytes = texture.pixel_bytes();
    let clut_bytes = texture.clut_bytes();
    if clut_entries > 0 && clut_bytes.len() < clut_entries * 2 {
        return None;
    }
    let palette: Vec<Color32> = (0..clut_entries)
        .map(|i| {
            let raw = u16::from_le_bytes([clut_bytes[i * 2], clut_bytes[i * 2 + 1]]) & 0x7FFF;
            let r5 = (raw & 0x1F) as u8;
            let g5 = ((raw >> 5) & 0x1F) as u8;
            let b5 = ((raw >> 10) & 0x1F) as u8;
            Color32::from_rgb(
                (r5 << 3) | (r5 >> 2),
                (g5 << 3) | (g5 >> 2),
                (b5 << 3) | (b5 >> 2),
            )
        })
        .collect();

    let mut pixels = Vec::with_capacity(pixel_count);
    if clut_entries == 0 {
        for i in 0..pixel_count {
            let off = i * 2;
            if off + 1 >= pixel_bytes.len() {
                return None;
            }
            let raw = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]) & 0x7FFF;
            let r5 = (raw & 0x1F) as u8;
            let g5 = ((raw >> 5) & 0x1F) as u8;
            let b5 = ((raw >> 10) & 0x1F) as u8;
            pixels.push(Color32::from_rgb(
                (r5 << 3) | (r5 >> 2),
                (g5 << 3) | (g5 >> 2),
                (b5 << 3) | (b5 >> 2),
            ));
        }
    } else if clut_entries == 16 {
        let halfwords_per_row = width.div_ceil(4);
        for row in 0..height {
            for hw in 0..halfwords_per_row {
                let off = (row * halfwords_per_row + hw) * 2;
                if off + 1 >= pixel_bytes.len() {
                    break;
                }
                let word = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]);
                for nibble in 0..4 {
                    let texel = (word >> (nibble * 4)) & 0xF;
                    if hw * 4 + nibble < width {
                        pixels.push(palette[texel as usize]);
                    }
                }
            }
        }
    } else if clut_entries == 256 {
        let halfwords_per_row = width.div_ceil(2);
        for row in 0..height {
            for hw in 0..halfwords_per_row {
                let off = (row * halfwords_per_row + hw) * 2;
                if off + 1 >= pixel_bytes.len() {
                    break;
                }
                let lo = pixel_bytes[off] as usize;
                let hi = pixel_bytes[off + 1] as usize;
                if hw * 2 < width {
                    pixels.push(palette[lo]);
                }
                if hw * 2 + 1 < width {
                    pixels.push(palette[hi]);
                }
            }
        }
    } else {
        return None;
    }
    (pixels.len() == pixel_count).then_some(ColorImage {
        size: [width, height],
        pixels,
    })
}

fn effective_radius(state: &ModelAnimationViewerState, model: Option<&LoadedModelContext>) -> i32 {
    if state.radius > 0 {
        state.radius
    } else {
        model
            .map(|model| (model.world_height as i32).saturating_mul(3) / 2)
            .unwrap_or(1536)
    }
    .clamp(640, 8192)
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    fn timeline_fixture() -> (ProjectDocument, ResourceId, ResourceId, ResourceId) {
        let mut project = ProjectDocument::new("animation-timeline");
        let clip = project.add_resource(
            "Idle",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: "assets/idle.psxanim".to_string(),
                skeleton: None,
                target_model: None,
                source: None,
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Idle,
                looping: true,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );
        let animation_set = project.add_resource(
            "Fighter Animations",
            ResourceData::AnimationSet(psxed_project::AnimationSetResource {
                idle_clip: Some(clip),
                ..psxed_project::AnimationSetResource::defaults()
            }),
        );
        let character = project.add_resource(
            "Fighter Profile",
            ResourceData::Character(psxed_project::CharacterResource {
                animation_set: Some(animation_set),
                combat_capsules: vec![
                    psxed_project::CharacterCombatCapsule::default(),
                    psxed_project::CharacterCombatCapsule {
                        name: "Right Hand".to_string(),
                        role: psxed_project::CombatCapsuleRole::Hitbox {
                            action: CharacterAnimationAction::LightAttack,
                            active_start_frame: 3,
                            active_end_frame: 7,
                            damage: 20,
                            poise_damage: 10,
                        },
                        ..psxed_project::CharacterCombatCapsule::default()
                    },
                    psxed_project::CharacterCombatCapsule {
                        name: "Heavy Blade".to_string(),
                        role: psxed_project::CombatCapsuleRole::Hitbox {
                            action: CharacterAnimationAction::HeavyAttack,
                            active_start_frame: 8,
                            active_end_frame: 12,
                            damage: 40,
                            poise_damage: 20,
                        },
                        ..psxed_project::CharacterCombatCapsule::default()
                    },
                ],
                ..psxed_project::CharacterResource::defaults()
            }),
        );
        (project, character, animation_set, clip)
    }

    #[test]
    fn timeline_projects_legacy_action_slot_and_persists_an_explicit_override() {
        let (mut project, character, animation_set, clip) = timeline_fixture();
        let context = timeline_action_context(&project, character, CharacterAnimationAction::Idle)
            .expect("legacy idle slot is projected into the timeline");

        assert_eq!(context.animation_set, animation_set);
        assert_eq!(context.clip, clip);
        assert_eq!(
            context.options,
            CharacterActionOptions::for_action(CharacterAnimationAction::Idle)
        );
        assert_eq!(
            timeline_action_for_clip(&project, character, clip),
            Some(CharacterAnimationAction::Idle)
        );

        let mut options = context.options;
        options.frame_start = 4;
        options.frame_end = 12;
        assert!(store_timeline_action_options(
            &mut project,
            context,
            CharacterAnimationAction::Idle,
            options,
        ));

        let ResourceData::AnimationSet(set) = &project.resource(animation_set).unwrap().data else {
            panic!("animation set expected");
        };
        assert_eq!(set.idle_clip, Some(clip));
        assert_eq!(
            set.action_binding(CharacterAnimationAction::Idle)
                .and_then(|binding| binding.options),
            Some(options)
        );
    }

    #[test]
    fn timeline_hitbox_lane_updates_only_its_matching_action_volume() {
        let (mut project, character, _, _) = timeline_fixture();
        let hitboxes =
            timeline_hitboxes(&project, character, CharacterAnimationAction::LightAttack);
        assert_eq!(hitboxes.len(), 1);
        assert_eq!(hitboxes[0].index, 1);
        assert_eq!((hitboxes[0].start, hitboxes[0].end), (3, 7));

        assert!(store_timeline_hitbox_range(
            &mut project,
            Some(character),
            1,
            CharacterAnimationAction::LightAttack,
            5,
            9,
        ));
        assert!(!store_timeline_hitbox_range(
            &mut project,
            Some(character),
            1,
            CharacterAnimationAction::HeavyAttack,
            1,
            2,
        ));

        let ResourceData::Character(profile) = &project.resource(character).unwrap().data else {
            panic!("character resource expected");
        };
        let psxed_project::CombatCapsuleRole::Hitbox {
            active_start_frame,
            active_end_frame,
            ..
        } = profile.combat_capsules[1].role
        else {
            panic!("hitbox expected");
        };
        assert_eq!((active_start_frame, active_end_frame), (5, 9));
    }

    #[test]
    fn timeline_frame_mapping_is_clamped_and_round_trips() {
        let rect = Rect::from_min_size(Pos2::new(100.0, 40.0), Vec2::new(600.0, 28.0));
        let pixels_per_frame = 12.0;

        for frame in [0, 1, 12, 38] {
            assert_eq!(
                timeline_frame_at(
                    rect,
                    timeline_x(rect, frame, pixels_per_frame),
                    38,
                    pixels_per_frame,
                ),
                frame
            );
        }
        assert_eq!(timeline_frame_at(rect, 0.0, 38, pixels_per_frame), 0);
        assert_eq!(timeline_frame_at(rect, 9999.0, 38, pixels_per_frame), 38);
    }

    #[test]
    fn focusing_targeted_clip_selects_its_model_and_exact_path() {
        let mut project = ProjectDocument::new("animation-focus");
        let skeleton = project.add_resource(
            "Humanoid",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "test-humanoid".to_string(),
                note: String::new(),
                joint_names: Vec::new(),
            }),
        );
        let add_model = |project: &mut ProjectDocument, name: &str| {
            project.add_resource(
                name,
                ResourceData::Model(psxed_project::ModelResource {
                    model_path: format!("assets/{name}.psxmdl"),
                    source_path: None,
                    texture_path: None,
                    skeleton: Some(skeleton),
                    world_height: 1024,
                    collision_radius: 192,
                    scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                    default_visual_yaw_q12: 0,
                    attachments: Vec::new(),
                }),
            )
        };
        let _other_model = add_model(&mut project, "Other");
        let target_model = add_model(&mut project, "CI Player");
        let clip_path = "assets/stand_to_roll.psxanim";
        let clip = project.add_resource(
            "Stand To Roll",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: clip_path.to_string(),
                skeleton: Some(skeleton),
                target_model: Some(target_model),
                source: None,
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Roll,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );
        let mut viewer = ModelAnimationViewerState::default();

        viewer.focus_resource(&project, clip);

        assert_eq!(viewer.selected_model(), Some(target_model));
        assert_eq!(viewer.selected_clip_path(), Some(clip_path));
    }

    #[test]
    fn frame_preview_restores_content_derived_radius_without_changing_angle() {
        let mut viewer = ModelAnimationViewerState {
            yaw_q12: 1024,
            pitch_q12: 128,
            radius: 4096,
            ..Default::default()
        };

        viewer.frame_preview();

        assert_eq!(viewer.radius, 0);
        assert_eq!(viewer.yaw_q12, 1024);
        assert_eq!(viewer.pitch_q12, 128);
    }

    #[test]
    fn nearest_preview_joint_uses_visible_pick_radius_and_nearest_point() {
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(640.0, 480.0));
        let joints = vec![Some([100.0, 100.0]), Some([108.0, 100.0]), None];
        let pointer = Pos2::new(100.0 + 216.0, 50.0 + 200.0);

        assert_eq!(
            nearest_preview_joint(pointer, rect, [320, 240], &joints),
            Some(1)
        );
        assert_eq!(
            nearest_preview_joint(Pos2::new(700.0, 500.0), rect, [320, 240], &joints),
            None
        );
    }

    #[test]
    fn focusing_character_enables_rig_volume_authoring_context() {
        let mut project = ProjectDocument::new("combat-volume-focus");
        let model = project.add_resource(
            "Fighter",
            ResourceData::Model(psxed_project::ModelResource {
                model_path: "assets/fighter.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: None,
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            }),
        );
        let character = project.add_resource(
            "Fighter Profile",
            ResourceData::Character(psxed_project::CharacterResource {
                model: Some(model),
                ..psxed_project::CharacterResource::defaults()
            }),
        );
        let mut viewer = ModelAnimationViewerState::default();

        viewer.focus_resource(&project, character);

        assert_eq!(viewer.selected_character, Some(character));
        assert_eq!(viewer.selected_model, Some(model));
    }

    #[test]
    fn capsule_viewport_manipulation_edits_joint_local_geometry() {
        let mut project = ProjectDocument::new("combat-volume-manipulation");
        let character = project.add_resource(
            "Fighter Profile",
            ResourceData::Character(psxed_project::CharacterResource {
                combat_capsules: vec![psxed_project::CharacterCombatCapsule {
                    name: "Arm".to_string(),
                    joint: 3,
                    capsule: psxed_project::JointCapsule {
                        start: [-100, 0, 0],
                        end: [100, 0, 0],
                        radius: 40,
                    },
                    role: psxed_project::CombatCapsuleRole::Hurtbox,
                }],
                ..psxed_project::CharacterResource::defaults()
            }),
        );

        assert!(manipulate_selected_capsule(
            &mut project,
            character,
            0,
            CapsuleEditTool::Move,
            CapsuleEditAxis::Y,
            Vec2::new(5.0, 0.0),
        ));
        assert!(manipulate_selected_capsule(
            &mut project,
            character,
            0,
            CapsuleEditTool::Resize,
            CapsuleEditAxis::X,
            Vec2::new(4.0, -10.0),
        ));

        let ResourceData::Character(profile) = &project.resource(character).unwrap().data else {
            panic!("character resource expected");
        };
        let capsule = &profile.combat_capsules[0].capsule;
        assert_eq!(capsule.radius, 48);
        assert_eq!(capsule.start, [-110, 10, 0]);
        assert_eq!(capsule.end, [110, 10, 0]);
    }

    #[test]
    fn imported_mixamo_take_is_named_and_searchable_by_source_filename() {
        let mut project = ProjectDocument::new("animation-library");
        let skeleton = project.add_resource(
            "Humanoid",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "test-humanoid".to_string(),
                note: String::new(),
                joint_names: Vec::new(),
            }),
        );
        let model = project.add_resource(
            "CI Player",
            ResourceData::Model(psxed_project::ModelResource {
                model_path: "assets/ci_player.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            }),
        );
        let source = project.add_resource(
            "Opaque Mixamo Take Source",
            ResourceData::AnimationSource(psxed_project::AnimationSourceResource {
                source_path: "source_assets/animations/mixamo/Standing Melee Attack Downward.fbx"
                    .to_string(),
                clip_name: "Armature|mixamo.com.003".to_string(),
                provider: psxed_project::AnimationSourceProvider::Mixamo,
                skeleton: Some(skeleton),
                target_model: Some(model),
                role: AnimationRole::Attack,
                looping: false,
                tags: vec!["mixamo".to_string()],
            }),
        );
        assert!(
            crate::resource_browser::resource_can_open_in_animation_viewer(
                &project.resource(source).expect("source exists").data
            )
        );
        let clip = project.add_resource(
            "CI Player / Armature|mixamo.com.003",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: "assets/attack.psxanim".to_string(),
                skeleton: Some(skeleton),
                target_model: Some(model),
                source: Some(source),
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Attack,
                looping: false,
                tags: vec!["mixamo".to_string()],
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );

        let options = build_clip_options(&project, model);
        let option = options
            .iter()
            .find(|option| option.path == "assets/attack.psxanim")
            .expect("baked Mixamo take is listed");

        assert_eq!(
            option.label,
            "Standing Melee Attack Downward — Armature|mixamo.com.003"
        );
        assert!(animation_viewer_option_matches(option, "standing downward"));
        assert!(!options.iter().any(|option| option.resource == Some(source)));

        let mut viewer = ModelAnimationViewerState::default();
        viewer.focus_resource(&project, source);
        assert_eq!(viewer.selected_clip_path(), Some("assets/attack.psxanim"));
        assert!(project.resource(clip).is_some());
    }

    #[test]
    fn changing_pose_keys_regenerates_the_immediate_preview_clip() {
        let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../projects/default/project.ron");
        let project_root = project_path.parent().expect("project directory");
        let mut project = ProjectDocument::load_from_path(&project_path).expect("default parses");
        let model = project
            .resources
            .iter()
            .find_map(|resource| {
                (resource.name == "Aletha Delivered"
                    && matches!(resource.data, ResourceData::Model(_)))
                .then_some(resource.id)
            })
            .expect("Aletha Delivered model exists");
        let skeleton = match &project.resource(model).unwrap().data {
            ResourceData::Model(m) => m.skeleton,
            _ => unreachable!(),
        };
        let clip = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::AnimationClip(animation)
                    if animation.skeleton == skeleton && resource.name == "aletha_idle" =>
                {
                    Some(resource.id)
                }
                _ => None,
            })
            .expect("Aletha Delivered idle exists");
        let mut viewer = ModelAnimationViewerState::default();
        viewer.focus_resource(&project, clip);
        let model_context = load_model_context_cached(&project, project_root, &mut viewer, model)
            .expect("model preview loads");
        let option = build_clip_options(&project, model)
            .into_iter()
            .find(|option| option.resource == Some(clip))
            .expect("clip option exists");
        let base = load_clip_context_cached(
            &project,
            project_root,
            &mut viewer,
            &option,
            Some(&model_context),
        )
        .expect("base preview loads");

        let ResourceData::AnimationClip(animation) = &mut project.resource_mut(clip).unwrap().data
        else {
            panic!("animation clip expected");
        };
        animation.pose_corrections.push(AnimationPoseCorrectionKey {
            frame: 0,
            joint: 0,
            rotation_q12: [0, 256, 0],
            translation: [8, 0, 0],
        });
        let corrected = load_clip_context_cached(
            &project,
            project_root,
            &mut viewer,
            &option,
            Some(&model_context),
        )
        .expect("corrected preview loads");

        assert_ne!(base.bytes, corrected.bytes);
        assert_eq!(base.animation_stats, corrected.animation_stats);
    }

    fn project_with_socket_model() -> (ProjectDocument, ResourceId) {
        let mut project = ProjectDocument::new("socket-editor");
        let id = project.add_resource(
            "Socket Test Model",
            ResourceData::Model(psxed_project::ModelResource {
                model_path: "assets/models/test/test.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: None,
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: vec![psxed_project::AttachmentSocket {
                    name: "right_hand_grip".to_string(),
                    joint: 3,
                    translation: [10, 20, 30],
                    rotation_q12: [0, 1024, 0],
                }],
            }),
        );
        (project, id)
    }

    fn first_socket(project: &ProjectDocument, id: ResourceId) -> psxed_project::AttachmentSocket {
        match &project.resource(id).expect("model resource").data {
            ResourceData::Model(model) => model.attachments[0].clone(),
            other => panic!("expected Model, got {other:?}"),
        }
    }

    fn project_with_pose_clip() -> (ProjectDocument, ResourceId) {
        let mut project = ProjectDocument::new("pose-gizmo");
        let clip = project.add_resource(
            "Pose Test Clip",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: "assets/pose_test.psxanim".to_string(),
                skeleton: None,
                target_model: None,
                source: None,
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Idle,
                looping: true,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );
        (project, clip)
    }

    #[test]
    fn pose_gizmo_creates_one_key_and_edits_every_move_and_rotation_axis() {
        let (mut project, clip) = project_with_pose_clip();
        assert!(manipulate_pose_correction(
            &mut project,
            clip,
            4,
            7,
            CapsuleEditAxis::X,
            AxisEditDelta::Translate(12),
        ));
        assert!(manipulate_pose_correction(
            &mut project,
            clip,
            4,
            7,
            CapsuleEditAxis::Y,
            AxisEditDelta::Translate(-6),
        ));
        assert!(manipulate_pose_correction(
            &mut project,
            clip,
            4,
            7,
            CapsuleEditAxis::Z,
            AxisEditDelta::Translate(3),
        ));
        for axis in CapsuleEditAxis::ALL {
            assert!(manipulate_pose_correction(
                &mut project,
                clip,
                4,
                7,
                axis,
                AxisEditDelta::Rotate(Vec2::new(10.0, 0.0)),
            ));
        }
        let ResourceData::AnimationClip(animation) = &project.resource(clip).unwrap().data else {
            panic!("animation clip expected");
        };
        assert_eq!(animation.pose_corrections.len(), 1);
        let key = animation.pose_corrections[0];
        assert_eq!((key.joint, key.frame), (4, 7));
        assert_eq!(key.translation, [12, -6, 3]);
        assert_eq!(key.rotation_q12, [120, 120, 120]);
    }

    #[test]
    fn zero_pose_gizmo_drag_does_not_create_a_key() {
        let (mut project, clip) = project_with_pose_clip();
        assert!(!manipulate_pose_correction(
            &mut project,
            clip,
            2,
            5,
            CapsuleEditAxis::Z,
            AxisEditDelta::Translate(0),
        ));
        let ResourceData::AnimationClip(animation) = &project.resource(clip).unwrap().data else {
            panic!("animation clip expected");
        };
        assert!(animation.pose_corrections.is_empty());
    }

    #[test]
    fn attach_socket_reanchors_on_the_new_joint() {
        let (mut project, id) = project_with_socket_model();
        assert!(attach_selected_socket_to_joint(&mut project, id, 0, 9));
        let socket = first_socket(&project, id);
        assert_eq!(socket.joint, 9);
        assert_eq!(
            socket.translation,
            [0, 0, 0],
            "attaching re-anchors the offset at the new bone"
        );
        assert_eq!(
            socket.rotation_q12,
            [0, 1024, 0],
            "grip orientation survives re-attachment"
        );
        assert!(
            !attach_selected_socket_to_joint(&mut project, id, 5, 9),
            "an out-of-range socket index is a no-op"
        );
    }

    #[test]
    fn manipulate_socket_moves_along_each_selected_axis() {
        let (mut project, id) = project_with_socket_model();
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::X,
            AxisEditDelta::Translate(5),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Y,
            AxisEditDelta::Translate(20),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Z,
            AxisEditDelta::Translate(-10),
        ));
        assert_eq!(first_socket(&project, id).translation, [15, 40, 20]);
        assert!(
            !manipulate_selected_socket(
                &mut project,
                id,
                0,
                CapsuleEditAxis::Y,
                AxisEditDelta::Translate(0),
            ),
            "a zero drag is a no-op"
        );
    }

    #[test]
    fn manipulate_socket_rotates_each_axis_in_q12_turns_and_wraps() {
        let (mut project, id) = project_with_socket_model();
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::X,
            AxisEditDelta::Rotate(Vec2::new(10.0, 0.0)),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Y,
            AxisEditDelta::Rotate(Vec2::new(300.0, 0.0)),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Z,
            AxisEditDelta::Rotate(Vec2::new(-20.0, 0.0)),
        ));
        // 1024 + 300 * 12 = 4624, wrapping one full turn to 528.
        assert_eq!(first_socket(&project, id).rotation_q12, [120, 528, 3856]);
    }

    #[test]
    fn socket_drag_projects_onto_all_visible_axes_at_preview_scale() {
        let gizmo = model_import_preview::PreviewAxisGizmo {
            origin: [100.0, 100.0],
            axis_ends: [[150.0, 100.0], [100.0, 125.0], [75.0, 75.0]],
            local_axis_units: 200.0,
        };
        let preview = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
        assert_eq!(
            axis_gizmo_drag_units(
                Vec2::new(20.0, 12.0),
                preview,
                [320, 240],
                gizmo,
                CapsuleEditAxis::X,
            )
            .unwrap()
            .round() as i32,
            40,
            "only the component parallel to the doubled-width X axis contributes"
        );
        assert_eq!(
            axis_gizmo_drag_units(
                Vec2::new(20.0, 10.0),
                preview,
                [320, 240],
                gizmo,
                CapsuleEditAxis::Y,
            )
            .unwrap()
            .round() as i32,
            40,
            "the same local motion remains consistent on the vertical axis"
        );
        assert_eq!(
            axis_gizmo_drag_units(
                Vec2::new(-10.0, -10.0),
                preview,
                [320, 240],
                gizmo,
                CapsuleEditAxis::Z,
            )
            .unwrap()
            .round() as i32,
            40,
            "the diagonal Z handle responds to drag projected along its screen direction"
        );
    }

    #[test]
    fn socket_gizmo_hover_picks_each_visible_axis_and_ignores_edge_on_axes() {
        let preview = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
        let gizmo = model_import_preview::PreviewAxisGizmo {
            origin: [100.0, 100.0],
            axis_ends: [[150.0, 100.0], [100.0, 125.0], [75.0, 75.0]],
            local_axis_units: 200.0,
        };
        assert_eq!(
            pick_axis_gizmo(Pos2::new(250.0, 201.0), preview, [320, 240], gizmo),
            Some(CapsuleEditAxis::X)
        );
        assert_eq!(
            pick_axis_gizmo(Pos2::new(201.0, 225.0), preview, [320, 240], gizmo),
            Some(CapsuleEditAxis::Y)
        );
        assert_eq!(
            pick_axis_gizmo(Pos2::new(175.0, 175.0), preview, [320, 240], gizmo),
            Some(CapsuleEditAxis::Z)
        );
        assert_eq!(
            pick_axis_gizmo(Pos2::new(400.0, 350.0), preview, [320, 240], gizmo),
            None
        );

        let edge_on_z = model_import_preview::PreviewAxisGizmo {
            axis_ends: [[150.0, 100.0], [100.0, 125.0], [100.5, 100.5]],
            ..gizmo
        };
        assert!(axis_gizmo_screen_axes(preview, [320, 240], edge_on_z)
            .iter()
            .all(|axis| axis.axis != CapsuleEditAxis::Z));
    }

    #[test]
    fn weapon_appearance_preview_uses_authored_sampled_frame_ramps() {
        let mut project = ProjectDocument::new("weapon-ramp-test");
        let weapon = project.add_resource(
            "Sword",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        let track = psxed_project::WeaponAppearanceTrack {
            action: CharacterAnimationAction::LightAttack,
            weapon,
            character_socket: "right_hand_grip".to_string(),
            fully_visible_frame: 12,
            hidden_frame: 24,
            transition_frames: 4,
        };
        assert_eq!(preview_weapon_materialization_q12(&track, 7.0, 30), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 8.0, 30), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 10.0, 30), 2048);
        assert_eq!(preview_weapon_materialization_q12(&track, 12.0, 30), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 22.0, 30), 2048);
        assert_eq!(preview_weapon_materialization_q12(&track, 24.0, 30), 0);
    }

    #[test]
    fn instant_weapon_appearance_preview_cuts_on_authored_frames() {
        let mut project = ProjectDocument::new("weapon-cut-test");
        let weapon = project.add_resource(
            "Sword",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        let track = psxed_project::WeaponAppearanceTrack {
            action: CharacterAnimationAction::LightAttack,
            weapon,
            character_socket: "right_hand_grip".to_string(),
            fully_visible_frame: 6,
            hidden_frame: 9,
            transition_frames: 0,
        };
        assert_eq!(preview_weapon_materialization_q12(&track, 5.99, 20), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 6.0, 20), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 8.99, 20), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 9.0, 20), 0);
    }
}
