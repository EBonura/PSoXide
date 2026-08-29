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
const TIMELINE_MIN_PREVIEW_HEIGHT: f32 = 120.0;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationGizmoHandle {
    Axis(CapsuleEditAxis),
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WeaponTransformTarget {
    CharacterSocket,
    WeaponGrip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterHand {
    Right,
    Left,
}

impl CharacterHand {
    const ALL: [Self; 2] = [Self::Right, Self::Left];

    const fn label(self) -> &'static str {
        match self {
            Self::Right => "Right hand",
            Self::Left => "Left hand",
        }
    }

    const fn socket_name(self) -> &'static str {
        match self {
            Self::Right => "right_hand_grip",
            Self::Left => "left_hand_grip",
        }
    }

    fn attachment(self) -> psxed_project::AttachmentSocket {
        match self {
            Self::Right => psxed_project::AttachmentSocket::right_hand_grip(),
            Self::Left => psxed_project::AttachmentSocket::left_hand_grip(),
        }
    }
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
    combat_capsules_visible: bool,
    show_pose_corrections: bool,
    show_attachment_sockets: bool,
    show_moveset: bool,
    selected_combat_capsule: usize,
    selected_attachment_socket: usize,
    selected_weapon_track: usize,
    preview_weapon: Option<ResourceId>,
    assignment_weapon: Option<ResourceId>,
    selected_pose_joint: u16,
    selected_pose_joints: Vec<u16>,
    pose_marquee_origin: Option<Pos2>,
    selected_action: CharacterAnimationAction,
    combat_preview_selection: Option<(ResourceId, usize, CharacterAnimationAction)>,
    capsule_edit_tool: CapsuleEditTool,
    capsule_edit_axis: CapsuleEditAxis,
    gizmo_drag_handle: Option<AnimationGizmoHandle>,
    gizmo_drag_pose_frame: Option<u16>,
    gizmo_drag_fractional_units: f32,
    weapon_transform_target: WeaponTransformTarget,
    timeline_height: f32,
    timeline_resize_origin: Option<f32>,
    timeline_pixels_per_frame: f32,
    preview_quality: AnimationPreviewQuality,
    cached_model: Option<CachedModelContext>,
    cached_weapon_models: Vec<CachedModelContext>,
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
            combat_capsules_visible: true,
            show_pose_corrections: false,
            show_attachment_sockets: false,
            show_moveset: false,
            selected_combat_capsule: 0,
            selected_attachment_socket: 0,
            selected_weapon_track: 0,
            preview_weapon: None,
            assignment_weapon: None,
            selected_pose_joint: 0,
            selected_pose_joints: vec![0],
            pose_marquee_origin: None,
            selected_action: CharacterAnimationAction::Idle,
            combat_preview_selection: None,
            capsule_edit_tool: CapsuleEditTool::Move,
            capsule_edit_axis: CapsuleEditAxis::X,
            gizmo_drag_handle: None,
            gizmo_drag_pose_frame: None,
            gizmo_drag_fractional_units: 0.0,
            weapon_transform_target: WeaponTransformTarget::CharacterSocket,
            timeline_height: TIMELINE_DEFAULT_HEIGHT,
            timeline_resize_origin: None,
            timeline_pixels_per_frame: 12.0,
            preview_quality: AnimationPreviewQuality::Authoring,
            cached_model: None,
            cached_weapon_models: Vec::new(),
            cached_material: None,
            cached_clip: None,
            last_time_seconds: 0.0,
        }
    }
}

impl ModelAnimationViewerState {
    #[cfg(test)]
    pub(crate) fn preview_is_playing(&self) -> bool {
        self.playing
    }

    #[cfg(test)]
    pub(crate) fn clear_preview_weapon_cache(&mut self) {
        self.cached_weapon_models.clear();
    }

    #[cfg(test)]
    pub(crate) fn preview_weapon_model_count(&self) -> usize {
        self.cached_weapon_models.len()
    }

    #[cfg(test)]
    pub(crate) fn weapon_authoring_overlays_are_visible(&self) -> bool {
        self.show_attachment_sockets
    }

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
                self.weapon_transform_target = WeaponTransformTarget::CharacterSocket;
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
                self.assignment_weapon = Some(id);
                self.weapon_transform_target = WeaponTransformTarget::WeaponGrip;
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

    /// Swap only the model rendered by Animation Studio. The selected
    /// Character remains the authoring context for its moveset and combat
    /// volumes, while the active clip, scrub frame, and playback state remain
    /// untouched so compatible loadout variants can be compared pose-for-pose.
    fn switch_preview_model(&mut self, project: &ProjectDocument, model_id: ResourceId) -> bool {
        if self.selected_model == Some(model_id)
            || !compatible_preview_model_options(
                project,
                self.selected_model,
                self.selected_clip_path.as_deref(),
            )
            .iter()
            .any(|(candidate, _)| *candidate == model_id)
        {
            return false;
        }

        self.selected_model = Some(model_id);
        self.invalidate_model_cache();
        // Clip decoding also carries a stamp for the model used to establish
        // preview bounds, so a model swap must refresh it even when the clip
        // path itself did not change.
        self.invalidate_clip_cache();
        true
    }

    fn reset_clip_clock(&mut self) {
        self.frame = 0.0;
        self.last_clip_path = None;
    }

    fn invalidate_model_cache(&mut self) {
        self.cached_model = None;
        self.cached_weapon_models.clear();
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
        let entering_pose =
            mode == AnimationStudioMode::Pose && self.studio_mode() != AnimationStudioMode::Pose;
        let entering_combat = mode == AnimationStudioMode::Combat
            && self.studio_mode() != AnimationStudioMode::Combat;
        self.show_moveset = mode == AnimationStudioMode::Moveset;
        self.show_pose_corrections = mode == AnimationStudioMode::Pose;
        self.show_attachment_sockets = mode == AnimationStudioMode::Weapon;
        self.show_combat_capsules = mode == AnimationStudioMode::Combat;
        if entering_pose {
            self.playing = false;
        }
        if entering_combat {
            self.combat_preview_selection = None;
        }
        if mode != AnimationStudioMode::Pose {
            self.pose_marquee_origin = None;
        }
    }

    fn select_pose_joint(&mut self, joint: u16, additive: bool) {
        if !additive {
            self.selected_pose_joints.clear();
        }
        if !self.selected_pose_joints.contains(&joint) {
            self.selected_pose_joints.push(joint);
        }
        self.selected_pose_joint = joint;
    }

    fn select_pose_joints(&mut self, joints: &[u16], additive: bool) {
        if joints.is_empty() {
            return;
        }
        if !additive {
            self.selected_pose_joints.clear();
        }
        for &joint in joints {
            if !self.selected_pose_joints.contains(&joint) {
                self.selected_pose_joints.push(joint);
            }
        }
        if let Some(&joint) = joints.last() {
            self.selected_pose_joint = joint;
        }
    }

    fn constrain_pose_selection(&mut self, joint_count: u16) {
        if joint_count == 0 {
            self.selected_pose_joint = 0;
            self.selected_pose_joints.clear();
            return;
        }
        self.selected_pose_joint = self.selected_pose_joint.min(joint_count - 1);
        self.selected_pose_joints
            .retain(|joint| *joint < joint_count);
        if !self
            .selected_pose_joints
            .contains(&self.selected_pose_joint)
        {
            self.selected_pose_joints.push(self.selected_pose_joint);
        }
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
        CharacterAnimationAction::AUTHORABLE
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
    let preview_model_options = compatible_preview_model_options(
        project,
        state.selected_model,
        state.selected_clip_path.as_deref(),
    );
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
    let playback_action_context = state
        .selected_character
        .and_then(|character| timeline_action_context(project, character, state.selected_action))
        .filter(|context| {
            selected_clip.as_ref().and_then(|clip| clip.resource) == Some(context.clip)
        });

    if state.last_clip_path.as_deref() != state.selected_clip_path.as_deref() {
        state.frame = 0.0;
        state.last_clip_path = state.selected_clip_path.clone();
        state.last_time_seconds = ui.input(|input| input.time);
    }
    let selected_model = state.selected_model;

    let mut action = None;
    let mut authored_speed_update = None;
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
            let mut model_selection = state.selected_model;
            if resource_combo(
                ui,
                "Model",
                "animation-viewer-model",
                &mut model_selection,
                &model_options,
            ) {
                let switched_compatibly = model_selection
                    .is_some_and(|model_id| state.switch_preview_model(project, model_id));
                if switched_compatibly {
                    preview_texture.take();
                } else if state.selected_model != model_selection {
                    // Crossing to a different skeleton remains a deliberate
                    // context change and keeps the established reset behavior.
                    state.selected_model = model_selection;
                    state.selected_clip_path = None;
                    state.clip_filter.clear();
                    state.reset_clip_clock();
                    state.invalidate_model_cache();
                    state.invalidate_clip_cache();
                    state.ensure_selection(project);
                }
            }
        } else if preview_model_options.len() > 1 {
            let mut preview_model = state.selected_model;
            if preview_model_combo(ui, &mut preview_model, &preview_model_options)
                && preview_model
                    .is_some_and(|model_id| state.switch_preview_model(project, model_id))
            {
                preview_texture.take();
            }
        }
        clip_combo(ui, state, &clip_options);
        ui.separator();
        let playback = draw_playback_controls(
            ui,
            state,
            selected_model,
            selected_clip.as_ref(),
            clip_context.as_ref().and_then(|clip| clip.animation_stats),
            playback_action_context.map(|context| context.options),
        );
        action = playback.action;
        authored_speed_update = playback.authored_speed_q8;
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
    if let (Some(context), Some(speed_q8)) = (playback_action_context, authored_speed_update) {
        let mut options = context.options;
        options.speed_q8 = speed_q8;
        if store_timeline_action_options(project, context, state.selected_action, options) {
            action = Some(AnimationViewerAction::ProjectChanged);
        }
    }
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
            .on_hover_text("Define the character's hands and assign swords to them");
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

fn preview_combat_capsules(
    capsules: &[psxed_project::CharacterCombatCapsule],
    selected: usize,
    visible: bool,
) -> Vec<model_import_preview::PreviewCombatCapsule> {
    if !visible {
        return Vec::new();
    }
    capsules
        .iter()
        .enumerate()
        .map(|(index, capsule)| {
            let projectile = match capsule.role {
                psxed_project::CombatCapsuleRole::ProjectileEmitter {
                    charge_start_frame,
                    active_start_frame,
                    tint_rgb,
                    ..
                } => Some(model_import_preview::PreviewProjectileCue {
                    charge_start_frame,
                    release_frame: active_start_frame,
                    core_color: Color32::from_rgb(
                        tint_rgb[0].saturating_add(96),
                        tint_rgb[1].saturating_add(40),
                        tint_rgb[2].saturating_add(40),
                    ),
                    glow_color: Color32::from_rgb(tint_rgb[0], tint_rgb[1], tint_rgb[2]),
                }),
                _ => None,
            };
            model_import_preview::PreviewCombatCapsule {
                joint: capsule.joint,
                start: capsule.capsule.start,
                end: capsule.capsule.end,
                radius: capsule.capsule.radius,
                color: match capsule.role {
                    psxed_project::CombatCapsuleRole::Hurtbox => Color32::from_rgb(76, 196, 224),
                    psxed_project::CombatCapsuleRole::Hitbox { .. } => {
                        Color32::from_rgb(238, 102, 82)
                    }
                    psxed_project::CombatCapsuleRole::ProjectileEmitter { .. } => {
                        Color32::from_rgb(214, 118, 255)
                    }
                },
                selected: index == selected,
                projectile,
            }
        })
        .collect()
}

fn combat_preview_action_window(
    capsules: &[psxed_project::CharacterCombatCapsule],
    selected: usize,
    current_action: CharacterAnimationAction,
) -> Option<(CharacterAnimationAction, u16)> {
    let action_window = |capsule: &psxed_project::CharacterCombatCapsule| match capsule.role {
        psxed_project::CombatCapsuleRole::Hitbox {
            action,
            active_start_frame,
            ..
        }
        | psxed_project::CombatCapsuleRole::ProjectileEmitter {
            action,
            active_start_frame,
            ..
        } => Some((action, active_start_frame)),
        psxed_project::CombatCapsuleRole::Hurtbox => None,
    };

    capsules
        .get(selected)
        .and_then(action_window)
        .or_else(|| {
            capsules
                .iter()
                .filter_map(action_window)
                .find(|(action, _)| *action == current_action)
        })
        .or_else(|| capsules.iter().find_map(action_window))
}

pub(crate) fn draw_model_animation_viewer(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) -> bool {
    state.ensure_selection(project);
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
    let combat_studio_open = state.show_combat_capsules && character_id.is_some();
    if let Some(character_id) = combat_studio_open.then_some(character_id).flatten() {
        if let Some((action, active_start_frame)) = combat_preview_action_window(
            &capsules,
            state.selected_combat_capsule,
            state.selected_action,
        ) {
            let selection = (character_id, state.selected_combat_capsule, action);
            if state.combat_preview_selection != Some(selection) {
                state.selected_action = action;
                if let Some(path) = character_action_clip_path(project, character_id, action) {
                    if state.selected_clip_path.as_deref() != Some(path.as_str()) {
                        state.selected_clip_path = Some(path);
                        state.invalidate_clip_cache();
                        state.last_clip_path = state.selected_clip_path.clone();
                        state.last_time_seconds = ui.input(|input| input.time);
                    }
                }
                state.frame = active_start_frame as f32;
                state.playing = false;
                state.combat_preview_selection = Some(selection);
            }
        }
    }
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
    let preview_in_place = character_id
        .and_then(|character| timeline_action_context(project, character, state.selected_action))
        .filter(|context| {
            selected_clip.as_ref().and_then(|clip| clip.resource) == Some(context.clip)
        })
        .map(|context| context.options.in_place)
        .or_else(|| selected_clip.as_ref().map(|clip| clip.calibration.in_place))
        .unwrap_or(false);

    let material_layer = preview_material_layer_cached(project, project_root, state);
    let character_material =
        material_layer.as_ref().map(
            |(atlas, motion)| model_import_preview::PreviewMaterialLayer {
                atlas,
                motion: *motion,
            },
        );
    let combat_overlays_visible = combat_studio_open && state.combat_capsules_visible;
    let preview_capsules = preview_combat_capsules(
        &capsules,
        state.selected_combat_capsule,
        combat_overlays_visible,
    );
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
    let attachment_model_id = state.selected_model.filter(|id| {
        project
            .resource(*id)
            .is_some_and(|resource| matches!(resource.data, ResourceData::Model(_)))
    });
    let sockets = attachment_model_id
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model.attachments.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let parsed_attachment_model = model_context
        .as_ref()
        .and_then(|context| psx_asset::Model::from_bytes(&context.model_bytes).ok());
    let resolved_socket_translation = |socket: &psxed_project::AttachmentSocket| {
        parsed_attachment_model
            .as_ref()
            .map(|model| {
                psxed_project::model_import::attachment_socket_bind_translation(model, socket)
            })
            .unwrap_or(socket.translation)
    };
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
                translation: resolved_socket_translation(socket),
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
    struct LoadedPreviewWeapon {
        track_index: Option<usize>,
        context: Arc<LoadedModelContext>,
        socket: psxed_project::AttachmentSocket,
        grip_translation: [i32; 3],
        grip_rotation_q12: [i16; 3],
        materialization_q12: u16,
    }

    let weapon_fallback_atlas = ColorImage {
        size: [4, 4],
        pixels: vec![Color32::from_rgb(168, 168, 176); 16],
    };
    let frame_count = clip_context
        .as_ref()
        .and_then(|clip| clip.animation_stats)
        .map(|stats| stats.frame_count)
        .unwrap_or(1);
    let action_weapon_tracks = attachment_model_id
        .map(|_| character_action_weapon_tracks(project, character_id, state.selected_action))
        .unwrap_or_default();
    let mut loaded_preview_weapons = Vec::new();
    let default_equipment = character_id
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character.default_equipment.clone()),
            _ => None,
        })
        .unwrap_or_default();
    for binding in &default_equipment {
        // An authored action track is the per-action override for a socket.
        if action_weapon_tracks
            .iter()
            .any(|(_, track)| track.character_socket == binding.character_socket)
        {
            continue;
        }
        let Some(weapon_id) = binding.weapon else {
            continue;
        };
        let Some((model_id, grip_translation, grip_rotation_q12)) = project
            .resource(weapon_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Weapon(weapon) => weapon
                    .model
                    .map(|model_id| (model_id, weapon.grip.translation, weapon.grip.rotation_q12)),
                _ => None,
            })
        else {
            continue;
        };
        let Some(socket) = sockets
            .iter()
            .find(|socket| socket.name == binding.character_socket)
            .cloned()
        else {
            continue;
        };
        let Some(context) = load_weapon_model_context(project, project_root, state, model_id)
        else {
            continue;
        };
        loaded_preview_weapons.push(LoadedPreviewWeapon {
            track_index: None,
            context,
            socket,
            grip_translation,
            grip_rotation_q12,
            materialization_q12: 4096,
        });
    }
    for (track_index, track) in &action_weapon_tracks {
        let Some((model_id, grip_translation, grip_rotation_q12)) = project
            .resource(track.weapon)
            .and_then(|resource| match &resource.data {
                ResourceData::Weapon(weapon) => weapon
                    .model
                    .map(|model_id| (model_id, weapon.grip.translation, weapon.grip.rotation_q12)),
                _ => None,
            })
        else {
            continue;
        };
        let Some(socket) = sockets
            .iter()
            .find(|socket| socket.name == track.character_socket)
            .cloned()
        else {
            continue;
        };
        let Some(context) = load_weapon_model_context(project, project_root, state, model_id)
        else {
            continue;
        };
        loaded_preview_weapons.push(LoadedPreviewWeapon {
            track_index: Some(*track_index),
            context,
            socket,
            grip_translation,
            grip_rotation_q12,
            materialization_q12: if combat_studio_open {
                4096
            } else {
                preview_weapon_materialization_q12(track, state.frame, frame_count)
            },
        });
    }
    if loaded_preview_weapons.is_empty() && socket_model_id.is_some() {
        let fallback_weapon = state.preview_weapon.and_then(|weapon_id| {
            project.resource(weapon_id).and_then(|resource| {
                let ResourceData::Weapon(weapon) = &resource.data else {
                    return None;
                };
                weapon
                    .model
                    .map(|model_id| (model_id, weapon.grip.translation, weapon.grip.rotation_q12))
            })
        });
        if let (Some((model_id, grip_translation, grip_rotation_q12)), Some(socket)) =
            (fallback_weapon, sockets.get(preview_socket_index).cloned())
        {
            if let Some(context) = load_weapon_model_context(project, project_root, state, model_id)
            {
                loaded_preview_weapons.push(LoadedPreviewWeapon {
                    track_index: None,
                    context,
                    socket,
                    grip_translation,
                    grip_rotation_q12,
                    materialization_q12: 4096,
                });
            }
        }
    }
    let equipped_weapons = loaded_preview_weapons
        .iter()
        .map(|weapon| model_import_preview::PreviewEquippedWeapon {
            model_bytes: &weapon.context.model_bytes,
            atlas_banks: weapon
                .context
                .atlas
                .as_deref()
                .unwrap_or_else(|| std::slice::from_ref(&weapon_fallback_atlas)),
            socket_joint: weapon.socket.joint,
            socket_translation: resolved_socket_translation(&weapon.socket),
            socket_rotation_q12: weapon.socket.rotation_q12,
            grip_translation: weapon.grip_translation,
            grip_rotation_q12: weapon.grip_rotation_q12,
            materialization_q12: weapon.materialization_q12,
            wireframe_materialization: preview_weapon_uses_materialization_wireframe(
                weapon.track_index,
                weapon.materialization_q12,
            ),
            show_grip_gizmo: socket_model_id.is_some()
                && state.weapon_transform_target == WeaponTransformTarget::WeaponGrip
                && weapon.track_index == Some(state.selected_weapon_track),
        })
        .collect::<Vec<_>>();
    let selected_joint = if state.show_pose_corrections {
        Some(state.selected_pose_joint)
    } else if socket_model_id.is_some() {
        sockets.get(preview_socket_index).map(|socket| socket.joint)
    } else {
        combat_overlays_visible
            .then(|| {
                capsules
                    .get(state.selected_combat_capsule)
                    .map(|capsule| capsule.joint)
            })
            .flatten()
    };
    let joint_picking =
        combat_overlays_visible || pose_clip_id.is_some() || socket_model_id.is_some();
    let moveset_open = state.show_moveset && character_id.is_some();
    let authoring_panel_open = combat_studio_open || joint_picking || moveset_open;
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
    let max_timeline_height = animation_timeline_height_limit(total_height);
    state.timeline_height = state.timeline_height.clamp(
        TIMELINE_MIN_HEIGHT.min(max_timeline_height),
        max_timeline_height,
    );
    let preview_height = (total_height - state.timeline_height - 7.0).max(120.0);

    let mut preview_interaction = PreviewInteraction::default();
    let mut editor_changed = false;
    fixed_height_studio_region(
        ui,
        "animation-studio-preview-region",
        preview_height,
        |ui| {
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
                                &equipped_weapons,
                                character_material.as_ref(),
                                preview_in_place,
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
                                        editor_changed |= draw_moveset_capability_matrix(
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
                    &equipped_weapons,
                    character_material.as_ref(),
                    preview_in_place,
                    None,
                    false,
                );
            }
        },
    );

    draw_timeline_splitter(ui, state, max_timeline_height);
    fixed_height_studio_region(
        ui,
        "animation-studio-timeline-region",
        state.timeline_height,
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
    if let Some(joints) = preview_interaction.marquee_joints.as_deref() {
        if state.show_pose_corrections {
            state.select_pose_joints(joints, preview_interaction.joint_selection_additive);
        }
    } else if let Some(joint) = preview_interaction.clicked_joint {
        if state.show_pose_corrections {
            state.select_pose_joint(joint, preview_interaction.joint_selection_additive);
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
            CapsuleEditTool::Rotate => preview_interaction
                .gizmo_rotate_q12
                .map(AxisEditDelta::Rotate),
            CapsuleEditTool::Resize => None,
        };
        if let Some(delta) = pose_delta {
            let frame = state
                .gizmo_drag_pose_frame
                .unwrap_or_else(|| state.frame.round().max(0.0) as u16);
            let selected_joints = state.selected_pose_joints.clone();
            changed |= manipulate_pose_corrections(
                project,
                clip_id,
                &selected_joints,
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
            CapsuleEditTool::Rotate => preview_interaction
                .gizmo_rotate_q12
                .map(AxisEditDelta::Rotate),
            CapsuleEditTool::Resize => None,
        };
        if let Some(delta) = socket_delta {
            changed |= if state.weapon_transform_target == WeaponTransformTarget::WeaponGrip {
                state.preview_weapon.is_some_and(|weapon_id| {
                    manipulate_selected_weapon_grip(
                        project,
                        weapon_id,
                        state.capsule_edit_axis,
                        delta,
                    )
                })
            } else {
                manipulate_selected_socket(
                    project,
                    model_id,
                    state.selected_attachment_socket,
                    state.capsule_edit_axis,
                    delta,
                )
            };
            preview_texture.take();
        }
    } else if let Some(character_id) = character_id {
        let capsule_delta = match state.capsule_edit_tool {
            CapsuleEditTool::Move => preview_interaction
                .gizmo_move_units
                .map(CapsuleGizmoDelta::Move),
            CapsuleEditTool::Rotate => preview_interaction
                .gizmo_rotate_q12
                .map(CapsuleGizmoDelta::Rotate),
            CapsuleEditTool::Resize => preview_interaction
                .gizmo_resize_units
                .map(CapsuleGizmoDelta::ResizeAxis)
                .or_else(|| {
                    preview_interaction
                        .gizmo_radius_units
                        .map(CapsuleGizmoDelta::ResizeRadius)
                }),
        };
        if let Some(delta) = capsule_delta {
            changed |= manipulate_selected_capsule(
                project,
                character_id,
                state.selected_combat_capsule,
                state.capsule_edit_axis,
                delta,
            );
            preview_texture.take();
        }
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
    kind: TimelineCombatKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineCombatKind {
    Hitbox,
    Projectile,
}

#[derive(Debug, Clone)]
struct TimelineWeaponTrack {
    index: usize,
    name: String,
    socket: String,
    fully_visible_frame: u16,
    hidden_frame: u16,
    transition_frames: u16,
    trail: Option<psxed_project::WeaponTrailConfig>,
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
    let tracks = character_action_weapon_tracks(project, character_id, state.selected_action);
    let indices = tracks.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    if indices.is_empty() {
        return None;
    }
    if !indices.contains(&state.selected_weapon_track) {
        state.selected_weapon_track = indices[0];
    }
    let track = tracks
        .into_iter()
        .find_map(|(index, track)| (index == state.selected_weapon_track).then_some(track))?;
    state.preview_weapon = Some(track.weapon);
    if let Some(index) = sockets
        .iter()
        .position(|socket| socket.name == track.character_socket)
    {
        state.selected_attachment_socket = index;
    }
    Some(track)
}

fn character_action_weapon_tracks(
    project: &ProjectDocument,
    character_id: Option<ResourceId>,
    action: CharacterAnimationAction,
) -> Vec<(usize, psxed_project::WeaponAppearanceTrack)> {
    let Some(set_id) = character_animation_set_id(project, character_id) else {
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
                    .map(|(index, track)| (index, track.clone()))
                    .collect(),
            )
        })
        .unwrap_or_default()
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

fn preview_weapon_uses_materialization_wireframe(
    authored_track: Option<usize>,
    materialization_q12: u16,
) -> bool {
    authored_track.is_some() && (1..4096).contains(&materialization_q12)
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

fn animation_timeline_height_limit(total_height: f32) -> f32 {
    (total_height - TIMELINE_MIN_PREVIEW_HEIGHT - 7.0)
        .max(TIMELINE_MIN_HEIGHT.min(total_height * 0.45))
}

fn fixed_height_studio_region<R>(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let size = Vec2::new(ui.available_width().max(0.0), height.max(0.0));
    let parent_clip = ui.clip_rect();
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.expand_to_include_rect(rect);
    child.set_clip_rect(rect.intersect(parent_clip));
    add_contents(&mut child)
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
                                    for action in CharacterAnimationAction::AUTHORABLE {
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
                                state
                                    .selected_pose_joints
                                    .contains(&key.joint)
                                    .then_some(key.frame)
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
            let trail_track_count = weapon_tracks
                .iter()
                .filter(|track| track.trail.is_some())
                .count();
            let track_count = 1
                + usize::from(pose_track)
                + usize::from(action_context.is_some()) * 2
                + weapon_tracks.len()
                + trail_track_count
                + hitboxes.len();
            let content_height =
                TIMELINE_RULER_HEIGHT + track_count.max(1) as f32 * TIMELINE_TRACK_HEIGHT;
            let pose_key_detail = format!("{} sampled frames", animation.frame_count);

            let mut action_range_update = None;
            let mut push_range_update = None;
            let mut weapon_updates = Vec::new();
            let mut weapon_trail_updates = Vec::new();
            let mut hitbox_updates = Vec::new();

            egui::ScrollArea::vertical()
                .id_salt("animation-timeline-lanes-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
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
                                        &if state.selected_pose_joints.len() > 1 {
                                            format!(
                                                "{} joints · {} keys",
                                                state.selected_pose_joints.len(),
                                                pose_key_frames.len()
                                            )
                                        } else {
                                            format!(
                                                "Joint {} · {} keys",
                                                state.selected_pose_joint,
                                                pose_key_frames.len()
                                            )
                                        },
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
                                    let hand = character_hand_for_socket(&track.socket)
                                        .map(CharacterHand::label)
                                        .unwrap_or(track.socket.as_str());
                                    let hidden = if track.hidden_frame
                                        == psxed_project::ACTION_FRAME_END_FULL
                                    {
                                        "clip end".to_string()
                                    } else {
                                        format!("frame {}", track.hidden_frame)
                                    };
                                    let response = timeline_track_label(
                                        ui,
                                        &track.name,
                                        &format!(
                                            "{} · visible {} · gone {} · {}f transition",
                                            hand,
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
                                    if let Some(trail) = &track.trail {
                                        let trail_end = if trail.end_frame
                                            == psxed_project::ACTION_FRAME_END_FULL
                                        {
                                            "clip end".to_string()
                                        } else {
                                            format!("frame {}", trail.end_frame)
                                        };
                                        let response = timeline_track_label(
                                            ui,
                                            "Blade trail",
                                            &format!(
                                                "frame {} to {} · {}f history · {} segments",
                                                trail.start_frame,
                                                trail_end,
                                                trail.history_frames,
                                                trail.segments,
                                            ),
                                            Color32::from_rgb(255, 132, 62),
                                            state.selected_weapon_track == track.index,
                                        );
                                        if response.clicked() {
                                            state.selected_weapon_track = track.index;
                                            state.set_studio_mode(AnimationStudioMode::Weapon);
                                        }
                                    }
                                }
                                for hitbox in &hitboxes {
                                    let (detail, color) = match hitbox.kind {
                                        TimelineCombatKind::Hitbox => (
                                            "damage window".to_string(),
                                            Color32::from_rgb(238, 102, 82),
                                        ),
                                        TimelineCombatKind::Projectile => (
                                            format!(
                                                "charge {} · shot {}",
                                                hitbox.start, hitbox.end
                                            ),
                                            Color32::from_rgb(62, 214, 198),
                                        ),
                                    };
                                    let response = timeline_track_label(
                                        ui,
                                        &hitbox.name,
                                        &detail,
                                        color,
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

                                            let push_start = context
                                                .options
                                                .push_frame_start
                                                .min(action_max_frame);
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
                                                &format!(
                                                    "animation-timeline-weapon-{}",
                                                    track.index
                                                ),
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
                                            if let Some(trail) = &track.trail {
                                                let trail_end = if trail.end_frame
                                                    == psxed_project::ACTION_FRAME_END_FULL
                                                {
                                                    action_max_frame
                                                } else {
                                                    trail.end_frame.min(action_max_frame)
                                                }
                                                .max(trail.start_frame.min(action_max_frame));
                                                let response = draw_timeline_range_lane(
                                                    ui,
                                                    &format!(
                                                        "animation-timeline-weapon-trail-{}",
                                                        track.index
                                                    ),
                                                    content_width,
                                                    action_max_frame,
                                                    state.timeline_pixels_per_frame,
                                                    state.frame,
                                                    trail.start_frame.min(action_max_frame),
                                                    trail_end,
                                                    Color32::from_rgb(255, 132, 62),
                                                    true,
                                                );
                                                if let Some(frame) = response.seek_frame {
                                                    state.frame = frame as f32;
                                                    state.playing = false;
                                                    state.selected_weapon_track = track.index;
                                                }
                                                if let Some((start, end)) = response.range {
                                                    weapon_trail_updates.push((
                                                        track.index,
                                                        start,
                                                        end,
                                                    ));
                                                    state.selected_weapon_track = track.index;
                                                }
                                            }
                                        }

                                        for hitbox in &hitboxes {
                                            let color = match hitbox.kind {
                                                TimelineCombatKind::Hitbox => {
                                                    Color32::from_rgb(238, 102, 82)
                                                }
                                                TimelineCombatKind::Projectile => {
                                                    Color32::from_rgb(62, 214, 198)
                                                }
                                            };
                                            let response = draw_timeline_range_lane(
                                                ui,
                                                &format!(
                                                    "animation-timeline-hitbox-{}",
                                                    hitbox.index
                                                ),
                                                content_width,
                                                action_max_frame,
                                                state.timeline_pixels_per_frame,
                                                state.frame,
                                                hitbox.start.min(action_max_frame),
                                                hitbox
                                                    .end
                                                    .min(action_max_frame)
                                                    .max(hitbox.start.min(action_max_frame)),
                                                color,
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
            for (index, start, end) in weapon_trail_updates {
                changed |= store_timeline_weapon_trail_range(
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
    CharacterAnimationAction::AUTHORABLE
        .into_iter()
        .find(|action| {
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
                        let (capsule_action, active_start_frame, active_end_frame, kind) =
                            match capsule.role {
                                psxed_project::CombatCapsuleRole::Hitbox {
                                    action,
                                    active_start_frame,
                                    active_end_frame,
                                    ..
                                } => (
                                    action,
                                    active_start_frame,
                                    active_end_frame,
                                    TimelineCombatKind::Hitbox,
                                ),
                                psxed_project::CombatCapsuleRole::ProjectileEmitter {
                                    action,
                                    charge_start_frame,
                                    active_start_frame,
                                    ..
                                } => (
                                    action,
                                    charge_start_frame,
                                    active_start_frame,
                                    TimelineCombatKind::Projectile,
                                ),
                                psxed_project::CombatCapsuleRole::Hurtbox => return None,
                            };
                        (capsule_action == action).then(|| TimelineHitbox {
                            index,
                            name: capsule.name.clone(),
                            start: active_start_frame,
                            end: active_end_frame.max(active_start_frame),
                            kind,
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
                        trail: track.trail.clone(),
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
    expected_action: CharacterAnimationAction,
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
    match &mut capsule.role {
        psxed_project::CombatCapsuleRole::Hitbox {
            action,
            active_start_frame,
            active_end_frame,
            ..
        } => {
            if *action != expected_action
                || (*active_start_frame == start && *active_end_frame == end)
            {
                return false;
            }
            *active_start_frame = start;
            *active_end_frame = end.max(start);
            true
        }
        psxed_project::CombatCapsuleRole::ProjectileEmitter {
            action,
            charge_start_frame,
            active_start_frame,
            active_end_frame,
            ..
        } => {
            if *action != expected_action
                || (*charge_start_frame == start
                    && *active_start_frame == end
                    && *active_end_frame == end)
            {
                return false;
            }
            *charge_start_frame = start.min(end);
            *active_start_frame = end.max(start);
            *active_end_frame = *active_start_frame;
            true
        }
        psxed_project::CombatCapsuleRole::Hurtbox => false,
    }
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

fn store_timeline_weapon_trail_range(
    project: &mut ProjectDocument,
    character_id: Option<ResourceId>,
    index: usize,
    action: CharacterAnimationAction,
    start_frame: u16,
    end_frame: u16,
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
    let Some(trail) = track.trail.as_mut() else {
        return false;
    };
    if track.action != action || (trail.start_frame == start_frame && trail.end_frame == end_frame)
    {
        return false;
    }
    trail.start_frame = start_frame;
    trail.end_frame = end_frame;
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
    project: &mut ProjectDocument,
    character_id: ResourceId,
    state: &mut ModelAnimationViewerState,
    clip_options: &[ViewerClipOption],
) -> bool {
    let mut changed = false;
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
        return false;
    };
    let binding_options = compatible_moveset_clip_options(project, character_id);
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
                            ui.vertical(|ui| {
                                let mut selected = row.clip;
                                if searchable_picker(
                                    ui,
                                    ("moveset-action-binding", row.action.to_index()),
                                    &mut selected,
                                    &motion_label,
                                    &binding_options,
                                    SearchablePickerConfig::optional("Disabled")
                                        .with_width(motion_width)
                                        .with_popup_min_width(motion_width.max(280.0))
                                        .with_search_hint("Search compatible clips…"),
                                ) && store_moveset_action_clip(
                                    project,
                                    character_id,
                                    row.action,
                                    selected,
                                ) {
                                    changed = true;
                                    state.selected_action = row.action;
                                    if let Some(clip) = selected.and_then(|clip| {
                                        clip_options.iter().find(|option| {
                                            option.resource == Some(clip) && option.previewable
                                        })
                                    }) {
                                        state.selected_clip_path = Some(clip.path.clone());
                                        state.invalidate_clip_cache();
                                        state.reset_clip_clock();
                                    }
                                }
                                ui.label(RichText::new(motion_hint).small().color(STUDIO_TEXT_WEAK));
                            });
                            ui.end_row();
                        }
                    });
            });
    }
    changed
}

fn compatible_moveset_clip_options(
    project: &ProjectDocument,
    character_id: ResourceId,
) -> Vec<(ResourceId, String)> {
    let Some((model_id, set_id)) = project.resource(character_id).and_then(|resource| {
        let ResourceData::Character(character) = &resource.data else {
            return None;
        };
        Some((character.model, character.animation_set?))
    }) else {
        return Vec::new();
    };
    let set_skeleton = project.resource(set_id).and_then(|resource| {
        let ResourceData::AnimationSet(set) = &resource.data else {
            return None;
        };
        set.skeleton
    });
    let mut options = project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::AnimationClip(clip) = &resource.data else {
                return None;
            };
            let model_matches = clip
                .target_model
                .is_none_or(|target| Some(target) == model_id);
            let skeleton_matches =
                set_skeleton.is_none_or(|skeleton| clip.skeleton == Some(skeleton));
            (model_matches && skeleton_matches).then(|| (resource.id, resource.name.clone()))
        })
        .collect::<Vec<_>>();
    options.sort_by_key(|(_, label)| label.to_lowercase());
    options
}

fn store_moveset_action_clip(
    project: &mut ProjectDocument,
    character_id: ResourceId,
    action: CharacterAnimationAction,
    clip_id: Option<ResourceId>,
) -> bool {
    if clip_id.is_some_and(|clip| {
        !compatible_moveset_clip_options(project, character_id)
            .iter()
            .any(|(candidate, _)| *candidate == clip)
    }) {
        return false;
    }
    let Some(set_id) = character_animation_set_id(project, Some(character_id)) else {
        return false;
    };
    let Some(resource) = project.resource_mut(set_id) else {
        return false;
    };
    let ResourceData::AnimationSet(set) = &mut resource.data else {
        return false;
    };
    if set.action_clip(action) == clip_id {
        return false;
    }
    set.set_action_clip(action, clip_id);
    true
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
        RichText::new(
            "Click a highlighted joint or drag a box around several, then author sparse corrections.",
        )
            .small()
            .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);

    if joint_count == 0 {
        ui.weak("The selected model has no cooked joints.");
        return false;
    }
    state.constrain_pose_selection(joint_count);
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
            state.select_pose_joint(selected.unwrap_or(state.selected_pose_joint), false);
        }
    });
    if state.selected_pose_joints.len() > 1 {
        ui.label(
            RichText::new(format!(
                "{} joints selected · the viewport gizmo edits all of them",
                state.selected_pose_joints.len()
            ))
            .small()
            .color(Color32::from_rgb(174, 116, 232)),
        );
    }
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
    let projectile_options: Vec<_> = project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::Projectile(projectile) = &resource.data else {
                return None;
            };
            Some((resource.id, resource.name.clone(), projectile.clone()))
        })
        .collect();
    ui.horizontal(|ui| {
        ui.heading("Combat Volumes");
        let (icon, label) = if state.combat_capsules_visible {
            (icons::EYE_OFF, "Hide capsules")
        } else {
            (icons::EYE, "Show capsules")
        };
        if ui
            .button(icons::label(icon, label))
            .on_hover_text("Show or hide combat capsules in the animation preview")
            .clicked()
        {
            state.combat_capsules_visible = !state.combat_capsules_visible;
        }
    });
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
                        charge_start_frame: 4,
                        active_start_frame: 8,
                        active_end_frame: 8,
                        projectile: None,
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
            CapsuleEditTool::Move => {
                "Drag a coloured handle to move along that bone-local axis."
            }
            CapsuleEditTool::Rotate => {
                "Drag a coloured handle to rotate around that bone-local axis."
            }
            CapsuleEditTool::Resize => {
                "Drag a coloured handle to resize along that local axis, or drag the white centre to change radius."
            }
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
                charge_start_frame: 4,
                active_start_frame: 8,
                active_end_frame: 8,
                projectile: None,
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
                for candidate in psxed_project::CharacterAnimationAction::AUTHORABLE {
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
        charge_start_frame,
        active_start_frame,
        active_end_frame,
        projectile,
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
                for candidate in psxed_project::CharacterAnimationAction::AUTHORABLE {
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
        let selected_projectile_name = projectile
            .and_then(|selected| {
                projectile_options
                    .iter()
                    .find(|(id, _, _)| *id == selected)
                    .map(|(_, name, _)| name.as_str())
            })
            .unwrap_or("Legacy inline projectile");
        egui::ComboBox::from_label("Projectile profile")
            .selected_text(selected_projectile_name)
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(projectile, None, "Legacy inline projectile")
                    .changed();
                for (id, name, _) in &projectile_options {
                    changed |= ui.selectable_value(projectile, Some(*id), name).changed();
                }
            });
        changed |= combat_u16_editor(ui, "Charge start", charge_start_frame, 0, u16::MAX);
        changed |= combat_u16_editor(ui, "Release frame", active_start_frame, 0, u16::MAX);
        if *charge_start_frame > *active_start_frame {
            *charge_start_frame = *active_start_frame;
            changed = true;
        }
        if *active_end_frame != *active_start_frame {
            *active_end_frame = *active_start_frame;
            changed = true;
        }
        if ui
            .button(icons::label(icons::PLAY, "Test charge + fire"))
            .on_hover_text("Seek to the charge start and play through the exact release frame")
            .clicked()
        {
            state.frame = f32::from(*charge_start_frame);
            state.playing = true;
            state.last_time_seconds = 0.0;
            ui.ctx().request_repaint();
        }
        changed |= combat_u16_editor(ui, "Minimum range", min_range, 0, u16::MAX);
        changed |= combat_u16_editor(ui, "Maximum range", max_range, 1, u16::MAX);
        if *max_range < *min_range {
            *max_range = *min_range;
            changed = true;
        }
        if let Some((_, name, profile)) = projectile
            .and_then(|selected| projectile_options.iter().find(|(id, _, _)| *id == selected))
        {
            ui.group(|ui| {
                ui.strong(name);
                ui.label(format!(
                    "{} · radius {} · speed {} · damage {} / poise {}",
                    profile.damage_channel.label(),
                    profile.radius,
                    profile.speed,
                    profile.damage,
                    profile.poise_damage
                ));
                ui.horizontal(|ui| {
                    ui.label("Core");
                    let mut core = profile.core_color;
                    ui.color_edit_button_srgb(&mut core);
                    ui.label("Glow");
                    let mut glow = profile.glow_color;
                    ui.color_edit_button_srgb(&mut glow);
                    ui.label("Impact");
                    let mut impact = profile.impact_color;
                    ui.color_edit_button_srgb(&mut impact);
                });
                ui.label(
                    RichText::new("Edit gameplay and visual tuning on the Projectile resource.")
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
            });
        } else {
            egui::CollapsingHeader::new("Legacy inline projectile")
                .default_open(true)
                .show(ui, |ui| {
                    changed |= combat_u16_editor(ui, "Speed / tick", speed, 1, 8192);
                    changed |= combat_u16_editor(ui, "Lifetime ticks", lifetime_ticks, 1, 3600);
                    changed |= combat_u16_editor(ui, "Damage", damage, 1, 9999);
                    changed |= combat_u16_editor(ui, "Poise damage", poise_damage, 0, 9999);
                    ui.horizontal(|ui| {
                        ui.label("Tint");
                        changed |= ui.color_edit_button_srgb(tint_rgb).changed();
                    });
                });
        }
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

fn character_hand_for_socket(socket_name: &str) -> Option<CharacterHand> {
    CharacterHand::ALL
        .into_iter()
        .find(|hand| socket_name == hand.socket_name())
}

fn weapon_appearance_pair_is_used(
    tracks: &[psxed_project::WeaponAppearanceTrack],
    action: CharacterAnimationAction,
    weapon: ResourceId,
    socket_name: &str,
    except_index: Option<usize>,
) -> bool {
    tracks.iter().enumerate().any(|(index, track)| {
        Some(index) != except_index
            && track.action == action
            && track.weapon == weapon
            && track.character_socket == socket_name
    })
}

fn weapon_assignment_timing_label(
    track: &psxed_project::WeaponAppearanceTrack,
    max_frame: u16,
) -> String {
    let hidden = if track.hidden_frame == psxed_project::ACTION_FRAME_END_FULL {
        "clip end".to_string()
    } else {
        format!("frame {}", track.hidden_frame.min(max_frame))
    };
    format!("visible frame {} to {hidden}", track.fully_visible_frame)
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
        ui.label(RichText::new("Sword assignments").strong());
        ui.colored_label(
            Color32::from_rgb(220, 160, 80),
            "Select a Character with an Animation Set to assign swords to its hands.",
        );
        return false;
    };

    let all_tracks = project
        .resource(set_id)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => Some(set.weapon_appearance_tracks.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let track_options = all_tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| track.action == state.selected_action)
        .map(|(index, track)| (index, track.clone()))
        .collect::<Vec<_>>();
    if !track_options
        .iter()
        .any(|(index, _)| *index == state.selected_weapon_track)
    {
        if let Some((index, _)) = track_options.first() {
            state.selected_weapon_track = *index;
        }
    }

    ui.horizontal(|ui| {
        ui.label(RichText::new("Sword assignments").strong());
        ui.label(
            RichText::new(state.selected_action.label())
                .small()
                .color(STUDIO_ACCENT),
        );
    });
    ui.label(
        RichText::new(
            "Choose a sword, then assign it to either hand. Use both buttons to equip both hands.",
        )
        .small()
        .color(STUDIO_TEXT_WEAK),
    );
    let current_frame = (state.frame.round().max(0.0) as u16).min(max_frame);

    if !weapon_options
        .iter()
        .any(|(weapon, _)| Some(*weapon) == state.assignment_weapon)
    {
        state.assignment_weapon = state
            .preview_weapon
            .filter(|weapon| weapon_options.iter().any(|(id, _)| id == weapon))
            .or_else(|| weapon_options.first().map(|(weapon, _)| *weapon));
    }
    let mut assignment_weapon = state.assignment_weapon;
    let assignment_weapon_label = assignment_weapon
        .and_then(|weapon| {
            weapon_options
                .iter()
                .find(|(id, _)| *id == weapon)
                .map(|(_, name)| name.as_str())
        })
        .unwrap_or("Choose sword");
    ui.horizontal(|ui| {
        ui.label("Sword");
        if searchable_picker(
            ui,
            "animation-new-weapon-assignment-resource",
            &mut assignment_weapon,
            assignment_weapon_label,
            &weapon_options,
            SearchablePickerConfig::required().with_width(180.0),
        ) {
            state.assignment_weapon = assignment_weapon;
        }
    });

    let mut add_track = None;
    ui.horizontal(|ui| {
        for hand in CharacterHand::ALL {
            let hand_defined = socket_names.iter().any(|name| name == hand.socket_name());
            let already_assigned = assignment_weapon.is_some_and(|weapon| {
                weapon_appearance_pair_is_used(
                    &all_tracks,
                    state.selected_action,
                    weapon,
                    hand.socket_name(),
                    None,
                )
            });
            let enabled = assignment_weapon.is_some() && hand_defined && !already_assigned;
            let help = if !hand_defined {
                format!("Define the {} below before assigning a sword", hand.label())
            } else if already_assigned {
                format!("This sword is already assigned to the {}", hand.label())
            } else {
                format!(
                    "Assign this sword to the {} at the current frame",
                    hand.label()
                )
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(icons::label(icons::PLUS, hand.label())),
                )
                .on_hover_text(help)
                .clicked()
            {
                add_track =
                    assignment_weapon.map(|weapon| (weapon, hand.socket_name().to_string()));
            }
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
                character_socket: character_socket.clone(),
                fully_visible_frame: current_frame,
                hidden_frame: psxed_project::ACTION_FRAME_END_FULL,
                transition_frames: psxed_project::WEAPON_APPEARANCE_DEFAULT_TRANSITION_FRAMES,
                trail: None,
            });
        state.selected_weapon_track = set.weapon_appearance_tracks.len() - 1;
        state.preview_weapon = Some(weapon);
        if let Some(index) = socket_names
            .iter()
            .position(|name| name == &character_socket)
        {
            state.selected_attachment_socket = index;
        }
        return true;
    }

    if track_options.is_empty() {
        ui.label(
            RichText::new("No swords are assigned to this action yet.")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        return false;
    }

    ui.add_space(4.0);
    ui.label(RichText::new("Current assignments").strong());
    let mut remove_track = None;
    for (index, track) in &track_options {
        let weapon_name = weapon_options
            .iter()
            .find(|(weapon, _)| *weapon == track.weapon)
            .map(|(_, name)| name.as_str())
            .unwrap_or("Missing sword");
        let hand_name = character_hand_for_socket(&track.character_socket)
            .map(CharacterHand::label)
            .unwrap_or(track.character_socket.as_str());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .small_button(icons::label(icons::TRASH, "Remove"))
                    .on_hover_text(format!("Remove {weapon_name} from the {hand_name}"))
                    .clicked()
                {
                    remove_track = Some(*index);
                }
                if ui
                    .selectable_label(
                        state.selected_weapon_track == *index,
                        format!("{hand_name}: {weapon_name}"),
                    )
                    .clicked()
                {
                    state.selected_weapon_track = *index;
                    state.preview_weapon = Some(track.weapon);
                    if let Some(socket_index) = socket_names
                        .iter()
                        .position(|name| name == &track.character_socket)
                    {
                        state.selected_attachment_socket = socket_index;
                    }
                }
            });
            ui.label(
                RichText::new(weapon_assignment_timing_label(track, max_frame))
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            );
        });
        ui.add_space(3.0);
    }
    if let Some(index) = remove_track {
        let Some(resource) = project.resource_mut(set_id) else {
            return false;
        };
        let ResourceData::AnimationSet(set) = &mut resource.data else {
            return false;
        };
        if index < set.weapon_appearance_tracks.len() {
            set.weapon_appearance_tracks.remove(index);
            if let Some((next_index, next)) = set
                .weapon_appearance_tracks
                .iter()
                .enumerate()
                .find(|(_, track)| track.action == state.selected_action)
            {
                state.selected_weapon_track = next_index;
                state.preview_weapon = Some(next.weapon);
            } else {
                state.selected_weapon_track = 0;
                state.preview_weapon = None;
            }
            return true;
        }
    }

    ui.separator();
    ui.label(RichText::new("Edit selected assignment").strong());
    let Some((_, selected_snapshot)) = track_options
        .iter()
        .find(|(index, _)| *index == state.selected_weapon_track)
    else {
        return false;
    };
    let safe_weapon_options = weapon_options
        .iter()
        .filter(|(weapon, _)| {
            !weapon_appearance_pair_is_used(
                &all_tracks,
                state.selected_action,
                *weapon,
                &selected_snapshot.character_socket,
                Some(state.selected_weapon_track),
            )
        })
        .cloned()
        .collect::<Vec<_>>();

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
        ui.label("Sword");
        if searchable_picker(
            ui,
            "animation-weapon-appearance-resource",
            &mut selected_weapon,
            weapon_options
                .iter()
                .find(|(id, _)| *id == track.weapon)
                .map(|(_, name)| name.as_str())
                .unwrap_or("Missing sword"),
            &safe_weapon_options,
            SearchablePickerConfig::required().with_width(180.0),
        ) {
            if let Some(weapon) = selected_weapon {
                track.weapon = weapon;
                state.preview_weapon = Some(weapon);
                state.assignment_weapon = Some(weapon);
                changed = true;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Hand");
        for hand in CharacterHand::ALL {
            let hand_defined = socket_names.iter().any(|name| name == hand.socket_name());
            let pair_used = weapon_appearance_pair_is_used(
                &all_tracks,
                state.selected_action,
                track.weapon,
                hand.socket_name(),
                Some(state.selected_weapon_track),
            );
            let enabled = hand_defined && !pair_used;
            ui.add_enabled_ui(enabled, |ui| {
                if ui
                    .selectable_value(
                        &mut track.character_socket,
                        hand.socket_name().to_string(),
                        hand.label(),
                    )
                    .changed()
                {
                    if let Some(index) = socket_names
                        .iter()
                        .position(|name| name == hand.socket_name())
                    {
                        state.selected_attachment_socket = index;
                    }
                    changed = true;
                }
            })
            .response
            .on_disabled_hover_text(if !hand_defined {
                format!("Define the {} below first", hand.label())
            } else {
                format!("This sword is already assigned to the {}", hand.label())
            });
        }
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

    ui.separator();
    ui.label(RichText::new("Blade trail").strong());
    let mut trail_enabled = track.trail.is_some();
    if ui
        .checkbox(&mut trail_enabled, "Emit a PS1 Gouraud ribbon")
        .changed()
    {
        track.trail = trail_enabled.then(|| {
            let mut trail = psxed_project::WeaponTrailConfig::default();
            trail.start_frame = current_frame;
            trail.end_frame = current_frame.saturating_add(8).min(max_frame);
            trail
        });
        changed = true;
    }
    if let Some(trail) = track.trail.as_mut() {
        ui.label(
            RichText::new(
                "The orange lane controls emission. History reaches backward through real sampled hand poses, so the ribbon follows the authored swing.",
            )
            .small()
            .color(STUDIO_TEXT_WEAK),
        );
        ui.horizontal(|ui| {
            ui.label("Starts");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut trail.start_frame)
                        .range(0..=max_frame)
                        .prefix("Frame "),
                )
                .changed();
            if ui.small_button("Start at playhead").clicked() {
                trail.start_frame = current_frame;
                changed = true;
            }
        });
        let mut trail_to_clip_end = trail.end_frame == psxed_project::ACTION_FRAME_END_FULL;
        if ui
            .checkbox(&mut trail_to_clip_end, "Emit until clip end")
            .changed()
        {
            trail.end_frame = if trail_to_clip_end {
                psxed_project::ACTION_FRAME_END_FULL
            } else {
                current_frame.max(trail.start_frame).min(max_frame)
            };
            changed = true;
        }
        if !trail_to_clip_end {
            ui.horizontal(|ui| {
                ui.label("Stops");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut trail.end_frame)
                            .range(trail.start_frame..=max_frame)
                            .prefix("Frame "),
                    )
                    .changed();
                if ui.small_button("End at playhead").clicked() {
                    trail.end_frame = current_frame.max(trail.start_frame);
                    changed = true;
                }
            });
            if trail.end_frame < trail.start_frame {
                trail.end_frame = trail.start_frame;
                changed = true;
            }
        }
        ui.horizontal(|ui| {
            ui.label("Arc history");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut trail.history_frames)
                        .range(1..=32)
                        .suffix(" frames"),
                )
                .changed();
            ui.label("Segments");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut trail.segments)
                        .range(1..=psxed_project::WEAPON_TRAIL_MAX_SEGMENTS),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Blend");
            egui::ComboBox::from_id_salt("animation-weapon-trail-blend")
                .selected_text(match trail.blend_mode {
                    psxed_project::WeaponTrailBlendMode::Average => "Average",
                    psxed_project::WeaponTrailBlendMode::Add => "Add",
                    psxed_project::WeaponTrailBlendMode::Subtract => "Subtract",
                    psxed_project::WeaponTrailBlendMode::AddQuarter => "Add quarter",
                })
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(
                            &mut trail.blend_mode,
                            psxed_project::WeaponTrailBlendMode::Average,
                            "Average",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut trail.blend_mode,
                            psxed_project::WeaponTrailBlendMode::Add,
                            "Add",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut trail.blend_mode,
                            psxed_project::WeaponTrailBlendMode::Subtract,
                            "Subtract",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut trail.blend_mode,
                            psxed_project::WeaponTrailBlendMode::AddQuarter,
                            "Add quarter",
                        )
                        .changed();
                });
        });
        ui.horizontal(|ui| {
            ui.label("Hilt");
            changed |= ui.color_edit_button_srgb(&mut trail.root_color).changed();
            ui.label("Tip");
            changed |= ui.color_edit_button_srgb(&mut trail.tip_color).changed();
        });
    }
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

fn suggested_hand_joint(joint_names: Option<&[String]>, hand: CharacterHand) -> Option<u16> {
    let suffix = match hand {
        CharacterHand::Right => "righthand",
        CharacterHand::Left => "lefthand",
    };
    joint_names?.iter().enumerate().find_map(|(index, name)| {
        let normalized = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        normalized
            .ends_with(suffix)
            .then_some(index.try_into().unwrap_or(u16::MAX))
    })
}

fn hand_assignment_count(
    project: &ProjectDocument,
    model_id: ResourceId,
    hand: CharacterHand,
) -> usize {
    let animation_sets = project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::Character(character) = &resource.data else {
                return None;
            };
            (character.model == Some(model_id))
                .then_some(character.animation_set)
                .flatten()
        })
        .collect::<HashSet<_>>();
    animation_sets
        .into_iter()
        .filter_map(|set_id| project.resource(set_id))
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => Some(set),
            _ => None,
        })
        .map(|set| {
            set.weapon_appearance_tracks
                .iter()
                .filter(|track| track.character_socket == hand.socket_name())
                .count()
        })
        .sum()
}

fn draw_character_hand_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    model_id: ResourceId,
    state: &mut ModelAnimationViewerState,
    joint_names: Option<&[String]>,
) -> bool {
    let sockets = project
        .resource(model_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => Some(model.attachments.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let hand_is_defined = |hand: CharacterHand| {
        sockets
            .iter()
            .any(|attachment| attachment.name == hand.socket_name())
    };
    let any_hand_missing = CharacterHand::ALL
        .into_iter()
        .any(|hand| !hand_is_defined(hand));
    let hand_status = CharacterHand::ALL
        .into_iter()
        .map(|hand| {
            format!(
                "{} {}",
                match hand {
                    CharacterHand::Right => "Right",
                    CharacterHand::Left => "Left",
                },
                if hand_is_defined(hand) {
                    "ready"
                } else {
                    "missing"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let mut define_hand = None;
    let mut remove_hand = None;
    egui::CollapsingHeader::new(format!("Hands · {hand_status}"))
        .default_open(any_hand_missing)
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Define each hand once on the rig. Select it here, then click the matching hand joint in the preview or use the gizmo to fine-tune it.",
                )
                .small()
                .color(STUDIO_TEXT_WEAK),
            );
            for hand in CharacterHand::ALL {
                let socket_index = sockets
                    .iter()
                    .position(|attachment| attachment.name == hand.socket_name());
                let assignment_count = hand_assignment_count(project, model_id, hand);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(hand.label()).strong());
                        if let Some(index) = socket_index {
                            let attachment = &sockets[index];
                            ui.label(
                                RichText::new(crate::inspector_character_ui::joint_label(
                                    attachment.joint,
                                    joint_names,
                                ))
                                .small()
                                .color(STUDIO_TEXT_WEAK),
                            );
                            if ui
                                .selectable_label(
                                    state.selected_attachment_socket == index,
                                    "Select",
                                )
                                .clicked()
                            {
                                state.selected_attachment_socket = index;
                                state.weapon_transform_target =
                                    WeaponTransformTarget::CharacterSocket;
                            }
                            let response = ui
                                .add_enabled(
                                    assignment_count == 0,
                                    egui::Button::new(icons::label(
                                        icons::TRASH,
                                        "Remove hand",
                                    )),
                                )
                                .on_hover_text(if assignment_count == 0 {
                                    format!("Remove the {} definition", hand.label())
                                } else {
                                    format!(
                                        "Remove its {assignment_count} sword assignment(s) first"
                                    )
                                });
                            if response.clicked() {
                                remove_hand = Some(index);
                            }
                        } else if ui
                            .button(icons::label(
                                icons::PLUS,
                                &format!("Define {}", hand.label()),
                            ))
                            .clicked()
                        {
                            define_hand = Some(hand);
                        }
                    });
                    if socket_index.is_none() {
                        ui.label(
                            RichText::new("Not defined")
                                .small()
                                .color(Color32::from_rgb(220, 160, 80)),
                        );
                    }
                });
                ui.add_space(3.0);
            }
        });

    if let Some(index) = remove_hand {
        let Some(resource) = project.resource_mut(model_id) else {
            return false;
        };
        let ResourceData::Model(model) = &mut resource.data else {
            return false;
        };
        if index < model.attachments.len() {
            model.attachments.remove(index);
            state.selected_attachment_socket = state
                .selected_attachment_socket
                .min(model.attachments.len().saturating_sub(1));
            return true;
        }
    }
    if let Some(hand) = define_hand {
        let joint = suggested_hand_joint(joint_names, hand).unwrap_or(0);
        let Some(resource) = project.resource_mut(model_id) else {
            return false;
        };
        let ResourceData::Model(model) = &mut resource.data else {
            return false;
        };
        let mut attachment = hand.attachment();
        attachment.joint = joint;
        model.attachments.push(attachment);
        state.selected_attachment_socket = model.attachments.len() - 1;
        state.weapon_transform_target = WeaponTransformTarget::CharacterSocket;
        return true;
    }

    false
}

fn draw_attachment_socket_editor(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    model_id: ResourceId,
    character_id: Option<ResourceId>,
    state: &mut ModelAnimationViewerState,
    _model: Option<&LoadedModelContext>,
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
            "Define hands, assign swords for this action, then align them in the preview.",
        )
        .small()
        .color(STUDIO_TEXT_WEAK),
    );
    ui.add_space(6.0);

    let joint_names = model_skeleton_joint_names(project, model_id);
    changed |= draw_character_hand_editor(ui, project, model_id, state, joint_names.as_deref());
    ui.separator();

    changed |= draw_weapon_appearance_editor(ui, project, model_id, character_id, state, max_frame);
    ui.separator();

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
            ui.label("Preview sword");
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
        .on_hover_text("Preview this sword in the currently selected hand");
    }

    if state.preview_weapon.is_none() {
        state.weapon_transform_target = WeaponTransformTarget::CharacterSocket;
    }
    ui.horizontal(|ui| {
        ui.label("Viewport gizmo");
        ui.selectable_value(
            &mut state.weapon_transform_target,
            WeaponTransformTarget::CharacterSocket,
            "Hand point",
        )
        .on_hover_text("Move the selected hand point on the character");
        ui.add_enabled_ui(state.preview_weapon.is_some(), |ui| {
            ui.selectable_value(
                &mut state.weapon_transform_target,
                WeaponTransformTarget::WeaponGrip,
                "Sword grip",
            )
            .on_hover_text("Move the alignment point authored on the selected sword");
        });
    });
    draw_axis_gizmo_controls(ui, state, false);

    if let Some(weapon_id) = state.preview_weapon {
        changed |= draw_weapon_grip_editor(ui, project, weapon_id);
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
    socket.translation_space = psxed_project::AttachmentSocketTranslationSpace::JointOffset;
    true
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum AxisEditDelta {
    Translate(i32),
    Rotate(i32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CapsuleGizmoDelta {
    Move(i32),
    Rotate(i32),
    ResizeAxis(i32),
    ResizeRadius(i32),
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
        AxisEditDelta::Rotate(amount) => amount,
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

fn manipulate_pose_corrections(
    project: &mut ProjectDocument,
    clip_id: ResourceId,
    joints: &[u16],
    frame: u16,
    axis: CapsuleEditAxis,
    delta: AxisEditDelta,
) -> bool {
    let mut changed = false;
    for &joint in joints {
        changed |= manipulate_pose_correction(project, clip_id, joint, frame, axis, delta);
    }
    changed
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
        AxisEditDelta::Rotate(amount) => {
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

fn manipulate_selected_weapon_grip(
    project: &mut ProjectDocument,
    weapon_id: ResourceId,
    axis: CapsuleEditAxis,
    delta: AxisEditDelta,
) -> bool {
    let Some(resource) = project.resource_mut(weapon_id) else {
        return false;
    };
    let ResourceData::Weapon(weapon) = &mut resource.data else {
        return false;
    };
    let index = axis.index();
    match delta {
        AxisEditDelta::Translate(amount) => {
            if amount == 0 {
                return false;
            }
            // The grip is subtracted from the socket transform at runtime, so
            // invert the authored delta to make the viewport handle follow the
            // pointer in the same direction as the character-socket handle.
            weapon.grip.translation[index] = weapon.grip.translation[index]
                .saturating_sub(amount)
                .clamp(i16::MIN as i32, i16::MAX as i32);
        }
        AxisEditDelta::Rotate(amount) => {
            if amount == 0 {
                return false;
            }
            let turned = i32::from(weapon.grip.rotation_q12[index]).saturating_sub(amount);
            weapon.grip.rotation_q12[index] = ((turned + 2048).rem_euclid(4096) - 2048) as i16;
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
    axis: CapsuleEditAxis,
    delta: CapsuleGizmoDelta,
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
    match delta {
        CapsuleGizmoDelta::Move(amount) => {
            if amount == 0 {
                return false;
            }
            let index = axis.index();
            capsule.start[index] =
                compact_capsule_coord(capsule.start[index].saturating_add(amount));
            capsule.end[index] = compact_capsule_coord(capsule.end[index].saturating_add(amount));
        }
        CapsuleGizmoDelta::Rotate(turn_q12) => {
            let angle = turn_q12 as f32 * std::f32::consts::TAU / 4096.0;
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
        CapsuleGizmoDelta::ResizeRadius(radius_delta) => {
            if radius_delta == 0 {
                return false;
            }
            let radius = i32::from(capsule.radius)
                .saturating_add(radius_delta)
                .clamp(1, 8192);
            capsule.radius = radius as u16;
        }
        CapsuleGizmoDelta::ResizeAxis(amount) => {
            if amount == 0 {
                return false;
            }
            let index = axis.index();
            let start_amount = amount / 2;
            let end_amount = amount - start_amount;
            capsule.start[index] =
                compact_capsule_coord(capsule.start[index].saturating_sub(start_amount));
            capsule.end[index] =
                compact_capsule_coord(capsule.end[index].saturating_add(end_amount));
        }
    }
    true
}

fn compact_capsule_coord(value: i32) -> i32 {
    value.clamp(i16::MIN as i32, i16::MAX as i32)
}

#[derive(Debug, Default, Clone)]
struct PreviewInteraction {
    clicked_joint: Option<u16>,
    joint_selection_additive: bool,
    marquee_joints: Option<Vec<u16>>,
    gizmo_move_units: Option<i32>,
    gizmo_rotate_q12: Option<i32>,
    gizmo_resize_units: Option<i32>,
    gizmo_radius_units: Option<i32>,
}

#[derive(Default)]
struct PlaybackControlsResponse {
    action: Option<AnimationViewerAction>,
    authored_speed_q8: Option<u16>,
}

fn draw_playback_controls(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    selected_model: Option<ResourceId>,
    clip: Option<&ViewerClipOption>,
    animation: Option<LoadedAnimationStats>,
    action_options: Option<CharacterActionOptions>,
) -> PlaybackControlsResponse {
    let mut action = None;
    let authored_speed_q8 = action_options.map(|options| {
        options.speed_q8.clamp(
            psxed_project::ACTION_SPEED_MIN_Q8,
            psxed_project::ACTION_SPEED_MAX_Q8,
        )
    });
    let playback_speed = authored_speed_q8
        .map(|speed| f32::from(speed) / 256.0)
        .unwrap_or_else(|| state.playback_speed.max(0.0));
    let now = ui.input(|input| input.time);
    if state.last_time_seconds <= 0.0 {
        state.last_time_seconds = now;
    }
    if let Some(animation) = animation {
        let frame_count = animation.frame_count.max(1);
        let (frame_start, frame_end) = action_options
            .map(|options| action_playback_frame_bounds(frame_count, options))
            .unwrap_or((0, frame_count.saturating_sub(1)));
        if state.playing {
            let delta = (now - state.last_time_seconds).max(0.0) as f32;
            let delta_frames = delta * animation.sample_rate_hz as f32 * playback_speed;
            let (frame, playing) = match action_options {
                Some(options) => advance_animation_playback_in_range(
                    state.frame,
                    delta_frames,
                    frame_start,
                    frame_end,
                    options.looping,
                ),
                None => advance_animation_playback(
                    state.frame,
                    delta_frames,
                    frame_count,
                    clip.is_none_or(|clip| clip.looping),
                ),
            };
            state.frame = frame;
            state.playing = playing;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        state.frame = state.frame.clamp(frame_start as f32, frame_end as f32);
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
        return PlaybackControlsResponse {
            action,
            authored_speed_q8: None,
        };
    };
    if !crate::editor_helpers::widget_owns_keyboard_shortcuts(ui.ctx())
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Space))
    {
        state.playing = !state.playing;
        state.last_time_seconds = now;
    }
    let (min_frame, max_frame) = action_options
        .map(|options| action_playback_frame_bounds(animation.frame_count.max(1), options))
        .unwrap_or((0, animation.frame_count.saturating_sub(1)));
    let frame = state.frame.round() as u16;
    if ui
        .button(icons::text(icons::CHEVRON_LEFT, 14.0))
        .on_hover_text("Previous frame")
        .clicked()
    {
        state.frame = frame.saturating_sub(1).max(min_frame) as f32;
        state.playing = false;
    }
    if ui
        .button(if state.playing {
            icons::text(icons::SQUARE, 14.0)
        } else {
            icons::text(icons::PLAY, 14.0)
        })
        .on_hover_text(if state.playing {
            "Pause (Space)"
        } else {
            "Play (Space)"
        })
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
            ui.add(egui::Slider::new(&mut timeline_frame, min_frame..=max_frame).show_value(false))
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
        "Action frame {frame} in {min_frame}..{max_frame} · {} Hz",
        animation.sample_rate_hz
    ));
    let mut authored_speed_update = None;
    if let Some(options) = action_options {
        ui.label(
            RichText::new("Action speed")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        let mut speed = f32::from(authored_speed_q8.unwrap_or(256)) / 256.0;
        if ui
            .add(
                egui::DragValue::new(&mut speed)
                    .speed(0.01)
                    .range(0.25..=4.0)
                    .fixed_decimals(2)
                    .suffix("×"),
            )
            .on_hover_text(
                "Saved speed for this action. Preview, attack duration, hit windows, and in-game playback use this value.",
            )
            .changed()
        {
            authored_speed_update = Some((speed * 256.0).round().clamp(
                psxed_project::ACTION_SPEED_MIN_Q8 as f32,
                psxed_project::ACTION_SPEED_MAX_Q8 as f32,
            ) as u16);
        }
        let metrics_speed = authored_speed_update.unwrap_or(options.speed_q8).clamp(
            psxed_project::ACTION_SPEED_MIN_Q8,
            psxed_project::ACTION_SPEED_MAX_Q8,
        );
        let effective_fps = f32::from(animation.sample_rate_hz) * f32::from(metrics_speed) / 256.0;
        let duration = action_duration_seconds(animation, options, metrics_speed);
        ui.label(
            RichText::new(format!("{effective_fps:.1} fps · {duration:.2} s"))
                .monospace()
                .small()
                .color(STUDIO_TEXT_WEAK),
        )
        .on_hover_text("Effective sampled rate and duration of the selected action range");
    } else {
        ui.label(
            RichText::new("Preview speed")
                .small()
                .color(STUDIO_TEXT_WEAK),
        );
        ui.add(
            egui::DragValue::new(&mut state.playback_speed)
                .speed(0.05)
                .range(0.25..=4.0)
                .fixed_decimals(2)
                .suffix("×"),
        )
        .on_hover_text(
            "Temporary preview-only speed; select a character action to author runtime speed",
        );
    }
    PlaybackControlsResponse {
        action,
        authored_speed_q8: authored_speed_update,
    }
}

fn action_duration_seconds(
    animation: LoadedAnimationStats,
    options: CharacterActionOptions,
    speed_q8: u16,
) -> f32 {
    let last = animation.frame_count.saturating_sub(2);
    let start = options.frame_start.min(last);
    let end = if options.frame_end == psxed_project::ACTION_FRAME_END_FULL {
        last
    } else {
        options.frame_end.min(last)
    }
    .max(start);
    let frames = end.saturating_sub(start).saturating_add(1);
    let speed = f32::from(speed_q8.max(1)) / 256.0;
    f32::from(frames) / (f32::from(animation.sample_rate_hz.max(1)) * speed)
}

fn action_playback_frame_bounds(frame_count: u16, options: CharacterActionOptions) -> (u16, u16) {
    let final_unique_frame = frame_count.saturating_sub(2);
    let start = options.frame_start.min(final_unique_frame);
    let end = if options.frame_end == psxed_project::ACTION_FRAME_END_FULL {
        final_unique_frame
    } else {
        options.frame_end.min(final_unique_frame)
    };
    (start, end.max(start))
}

fn advance_animation_playback_in_range(
    frame: f32,
    delta_frames: f32,
    first_frame: u16,
    last_frame: u16,
    looping: bool,
) -> (f32, bool) {
    let first = first_frame as f32;
    let last = last_frame.max(first_frame) as f32;
    let frame = frame.clamp(first, last);
    if !looping {
        let next = (frame + delta_frames.max(0.0)).min(last);
        return (next, next < last);
    }
    let cycle = (last - first + 1.0).max(1.0);
    (
        first + (frame - first + delta_frames.max(0.0)).rem_euclid(cycle),
        true,
    )
}

fn advance_animation_playback(
    frame: f32,
    delta_frames: f32,
    frame_count: u16,
    looping: bool,
) -> (f32, bool) {
    let last_frame = frame_count.saturating_sub(1) as f32;
    if !looping {
        let next = (frame + delta_frames.max(0.0)).min(last_frame);
        return (next, next < last_frame);
    }
    let cycle = last_frame.max(1.0);
    ((frame + delta_frames.max(0.0)).rem_euclid(cycle), true)
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
    equipped_weapons: &[model_import_preview::PreviewEquippedWeapon<'_>],
    character_material: Option<&model_import_preview::PreviewMaterialLayer<'_>>,
    preview_in_place: bool,
    selected_joint: Option<u16>,
    joint_picking: bool,
) -> PreviewInteraction {
    let size = ui.available_size();
    let size = Vec2::new(size.x.max(360.0), size.y.max(TIMELINE_MIN_PREVIEW_HEIGHT));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let pointer_delta = ui.input(|input| input.pointer.delta());
    let primary_edits_content = state.show_pose_corrections
        || !combat_capsules.is_empty()
        || !sockets.is_empty()
        || equipped_weapons.iter().any(|weapon| weapon.show_grip_gizmo);
    let orbiting = response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
        || (!joint_picking
            && !primary_edits_content
            && response.dragged_by(egui::PointerButton::Primary));
    if orbiting {
        let delta = pointer_delta;
        state.yaw_q12 = (state.yaw_q12 - (delta.x * 6.0) as i32).rem_euclid(4096);
        state.pitch_q12 = (state.pitch_q12 - (delta.y * 4.0) as i32).clamp(64, 960);
    }
    let primary_drag_delta = (response.dragged_by(egui::PointerButton::Primary)
        && pointer_delta != Vec2::ZERO)
        .then_some(pointer_delta);
    let primary_down = ui.input(|input| input.pointer.primary_down());
    if !primary_down {
        state.gizmo_drag_handle = None;
        state.gizmo_drag_pose_frame = None;
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
    let render = model_import_preview::render_import_model_preview_with_equipment_set_at_size(
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
            preview_in_place,
            pose_offset: selected_clip.calibration.offset,
            show_animation_root: state.show_animation_root,
            show_collision_guides: false,
            show_bones: state.show_bones || joint_picking,
        },
        model_import_preview::euler_rotation_q12(model.authored_rotation_q12),
        render_size,
        combat_capsules,
        sockets,
        equipped_weapons,
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
    let mut joint_selection_additive = false;
    let mut marquee_joints = None;
    let mut gizmo_move_units = None;
    let mut gizmo_rotate_q12 = None;
    let mut gizmo_resize_units = None;
    let mut gizmo_radius_units = None;
    match render {
        Some(render) => {
            let viewport_gizmo = if state.show_pose_corrections {
                render.selected_joint_gizmo
            } else if !combat_capsules.is_empty() {
                render.selected_combat_gizmo
            } else if equipped_weapons.iter().any(|weapon| weapon.show_grip_gizmo) {
                render.selected_weapon_grip_gizmo
            } else {
                render.selected_socket_gizmo
            };
            let center_handle_enabled =
                !combat_capsules.is_empty() && state.capsule_edit_tool == CapsuleEditTool::Resize;
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
            if let (Some(gizmo), Some(cue)) = (
                render.selected_combat_gizmo,
                combat_capsules
                    .iter()
                    .find(|capsule| capsule.selected)
                    .and_then(|capsule| capsule.projectile),
            ) {
                let origin = preview_image_to_screen(gizmo.origin, preview_rect, render_size);
                let aim = preview_image_to_screen(gizmo.axis_ends[2], preview_rect, render_size);
                draw_projectile_studio_preview(&painter, origin, aim, state.frame, cue);
            }
            if state.show_pose_corrections {
                for &joint in &state.selected_pose_joints {
                    let Some(point) = render
                        .joint_screen_positions
                        .get(joint as usize)
                        .and_then(|point| *point)
                    else {
                        continue;
                    };
                    let screen = preview_image_to_screen(point, preview_rect, render_size);
                    painter.circle_stroke(
                        screen,
                        if joint == state.selected_pose_joint {
                            8.0
                        } else {
                            6.0
                        },
                        Stroke::new(
                            if joint == state.selected_pose_joint {
                                2.0
                            } else {
                                1.5
                            },
                            Color32::from_rgb(198, 132, 255),
                        ),
                    );
                }
            }
            let hovered_handle = response
                .hovered()
                .then(|| response.interact_pointer_pos())
                .flatten()
                .and_then(|pointer| {
                    viewport_gizmo.and_then(|gizmo| {
                        pick_animation_gizmo_handle(
                            pointer,
                            preview_rect,
                            render_size,
                            gizmo,
                            center_handle_enabled,
                        )
                    })
                });
            if response.drag_started_by(egui::PointerButton::Primary) {
                let pressed_handle = viewport_gizmo.and_then(|gizmo| {
                    ui.input(|input| input.pointer.press_origin())
                        .and_then(|pointer| {
                            pick_animation_gizmo_handle(
                                pointer,
                                preview_rect,
                                render_size,
                                gizmo,
                                center_handle_enabled,
                            )
                        })
                });
                state.gizmo_drag_handle = pressed_handle;
                state.pose_marquee_origin =
                    if state.show_pose_corrections && pressed_handle.is_none() {
                        ui.input(|input| input.pointer.press_origin())
                            .filter(|origin| preview_rect.contains(*origin))
                    } else {
                        None
                    };
                if let Some(AnimationGizmoHandle::Axis(axis)) = pressed_handle {
                    state.capsule_edit_axis = axis;
                }
                if pressed_handle.is_some() {
                    state.playing = false;
                    state.gizmo_drag_pose_frame = state
                        .show_pose_corrections
                        .then_some(state.frame.round().max(0.0) as u16);
                    state.gizmo_drag_fractional_units = 0.0;
                }
            }
            if response.clicked() {
                if let Some(AnimationGizmoHandle::Axis(axis)) = hovered_handle {
                    state.capsule_edit_axis = axis;
                }
            }
            if let Some(gizmo) = viewport_gizmo {
                draw_axis_gizmo_overlay(
                    &painter,
                    preview_rect,
                    render_size,
                    gizmo,
                    hovered_handle,
                    state.gizmo_drag_handle,
                );
            }
            if response.hovered() {
                ui.ctx()
                    .set_cursor_icon(if state.gizmo_drag_handle.is_some() {
                        egui::CursorIcon::Grabbing
                    } else if hovered_handle.is_some() {
                        egui::CursorIcon::PointingHand
                    } else if joint_picking {
                        egui::CursorIcon::Crosshair
                    } else {
                        egui::CursorIcon::Grab
                    });
            }
            if let (Some(delta), Some(handle), Some(gizmo)) =
                (primary_drag_delta, state.gizmo_drag_handle, viewport_gizmo)
            {
                let modifiers = ui.input(|input| input.modifiers);
                let speed = if modifiers.shift {
                    0.25
                } else if modifiers.command || modifiers.ctrl {
                    4.0
                } else {
                    1.0
                };
                match handle {
                    AnimationGizmoHandle::Axis(axis) => {
                        state.capsule_edit_axis = axis;
                        match state.capsule_edit_tool {
                            CapsuleEditTool::Move | CapsuleEditTool::Resize => {
                                if let Some(units) = axis_gizmo_drag_units(
                                    delta,
                                    preview_rect,
                                    render_size,
                                    gizmo,
                                    axis,
                                ) {
                                    let accumulated =
                                        units * speed + state.gizmo_drag_fractional_units;
                                    let rounded = accumulated.round() as i32;
                                    state.gizmo_drag_fractional_units =
                                        accumulated - rounded as f32;
                                    if rounded != 0 {
                                        if state.capsule_edit_tool == CapsuleEditTool::Resize {
                                            gizmo_resize_units = Some(rounded);
                                        } else {
                                            gizmo_move_units = Some(rounded);
                                        }
                                    }
                                }
                            }
                            CapsuleEditTool::Rotate => {
                                if let Some(pixels) = axis_gizmo_drag_pixels(
                                    delta,
                                    preview_rect,
                                    render_size,
                                    gizmo,
                                    axis,
                                ) {
                                    let accumulated =
                                        pixels * 12.0 * speed + state.gizmo_drag_fractional_units;
                                    let rounded = accumulated.round() as i32;
                                    state.gizmo_drag_fractional_units =
                                        accumulated - rounded as f32;
                                    gizmo_rotate_q12 = (rounded != 0).then_some(rounded);
                                }
                            }
                        }
                    }
                    AnimationGizmoHandle::Center => {
                        let accumulated =
                            (delta.x - delta.y) * 2.0 * speed + state.gizmo_drag_fractional_units;
                        let rounded = accumulated.round() as i32;
                        state.gizmo_drag_fractional_units = accumulated - rounded as f32;
                        gizmo_radius_units = (rounded != 0).then_some(rounded);
                    }
                }
            }
            if response.clicked() && joint_picking && hovered_handle.is_none() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    clicked_joint = nearest_preview_joint(
                        pointer,
                        preview_rect,
                        render_size,
                        &render.joint_screen_positions,
                    );
                    joint_selection_additive = ui.input(|input| {
                        input.modifiers.shift || input.modifiers.command || input.modifiers.ctrl
                    });
                }
            }
            if state.show_pose_corrections {
                if let Some(origin) = state.pose_marquee_origin {
                    let current = response
                        .interact_pointer_pos()
                        .or_else(|| ui.input(|input| input.pointer.latest_pos()))
                        .unwrap_or(origin);
                    let selection_rect =
                        Rect::from_two_pos(origin, current).intersect(preview_rect);
                    if selection_rect.is_positive() {
                        painter.rect_filled(
                            selection_rect,
                            0.0,
                            Color32::from_rgba_unmultiplied(174, 116, 232, 28),
                        );
                        painter.rect_stroke(
                            selection_rect,
                            0.0,
                            Stroke::new(1.0, Color32::from_rgb(198, 132, 255)),
                            StrokeKind::Inside,
                        );
                    }
                    if response.drag_stopped_by(egui::PointerButton::Primary) {
                        let selected = preview_joints_in_rect(
                            selection_rect,
                            preview_rect,
                            render_size,
                            &render.joint_screen_positions,
                        );
                        if !selected.is_empty() {
                            marquee_joints = Some(selected);
                            joint_selection_additive = ui.input(|input| {
                                input.modifiers.shift
                                    || input.modifiers.command
                                    || input.modifiers.ctrl
                            });
                        }
                        state.pose_marquee_origin = None;
                    }
                }
                if !primary_down && !response.drag_stopped_by(egui::PointerButton::Primary) {
                    state.pose_marquee_origin = None;
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
        joint_selection_additive,
        marquee_joints,
        gizmo_move_units,
        gizmo_rotate_q12,
        gizmo_resize_units,
        gizmo_radius_units,
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

fn draw_projectile_studio_preview(
    painter: &egui::Painter,
    origin: Pos2,
    aim: Pos2,
    frame: f32,
    cue: model_import_preview::PreviewProjectileCue,
) {
    let charge_start = f32::from(cue.charge_start_frame);
    let release = f32::from(cue.release_frame);
    if frame < charge_start || frame > release + 10.0 {
        return;
    }
    let mut direction = aim - origin;
    if direction.length_sq() < 1.0 {
        direction = Vec2::new(1.0, 0.0);
    } else {
        direction = direction.normalized();
    }
    let perpendicular = Vec2::new(-direction.y, direction.x);
    if frame < release {
        let duration = (release - charge_start).max(1.0);
        let progress = ((frame - charge_start) / duration).clamp(0.0, 1.0);
        let radius = 11.0 - progress * 5.0;
        let points = [
            origin - direction * radius,
            origin + perpendicular * radius,
            origin + direction * radius,
            origin - perpendicular * radius,
        ];
        for index in 0..4 {
            painter.line_segment(
                [points[index], points[(index + 1) & 3]],
                Stroke::new(2.0, cue.glow_color.linear_multiply(0.65 + progress * 0.35)),
            );
        }
        painter.circle_filled(origin, 2.0 + progress * 2.0, cue.core_color);
        for sign in [-1.0f32, 1.0] {
            painter.line_segment(
                [
                    origin + perpendicular * sign * (radius + 4.0),
                    origin + perpendicular * sign * (radius + 8.0),
                ],
                Stroke::new(1.5, cue.glow_color.linear_multiply(0.75)),
            );
        }
        return;
    }

    let age = (frame - release).clamp(0.0, 10.0);
    let head = origin + direction * (14.0 + age * 17.0);
    let tail = head - direction * 28.0;
    painter.line_segment(
        [tail, head],
        Stroke::new(8.0, cue.glow_color.linear_multiply(0.42)),
    );
    painter.line_segment([tail, head], Stroke::new(2.5, cue.core_color));
    for ghost in 1..=3 {
        let fade = 0.42 / ghost as f32;
        let ghost_head = tail - direction * (ghost as f32 * 12.0);
        painter.line_segment(
            [ghost_head - direction * 18.0, ghost_head],
            Stroke::new((5 - ghost) as f32, cue.glow_color.linear_multiply(fade)),
        );
    }
    if age <= 2.0 {
        let radius = 9.0 + age * 2.0;
        for axis in [direction, perpendicular] {
            painter.line_segment(
                [origin - axis * radius, origin + axis * radius],
                Stroke::new(2.0, cue.core_color),
            );
        }
    }
    if age >= 7.0 {
        let radius = 4.0 + (age - 7.0) * 3.0;
        let impact = cue.glow_color.linear_multiply((10.0 - age).max(0.15) / 3.0);
        let points = [
            head - direction * radius,
            head + perpendicular * radius,
            head + direction * radius,
            head - perpendicular * radius,
        ];
        for index in 0..4 {
            painter.line_segment(
                [points[index], points[(index + 1) & 3]],
                Stroke::new(2.0, impact),
            );
        }
    }
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

fn pick_animation_gizmo_handle(
    pointer: Pos2,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
    center_handle_enabled: bool,
) -> Option<AnimationGizmoHandle> {
    let origin = preview_image_to_screen(gizmo.origin, preview_rect, render_size);
    if center_handle_enabled && pointer.distance(origin) <= crate::GIZMO_AXIS_PICK_RADIUS {
        return Some(AnimationGizmoHandle::Center);
    }
    pick_axis_gizmo(pointer, preview_rect, render_size, gizmo).map(AnimationGizmoHandle::Axis)
}

fn draw_axis_gizmo_overlay(
    painter: &egui::Painter,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
    hovered_handle: Option<AnimationGizmoHandle>,
    active_handle: Option<AnimationGizmoHandle>,
) {
    let axes = axis_gizmo_screen_axes(preview_rect, render_size, gizmo);
    let Some(origin) = axes.first().map(|axis| axis.start) else {
        return;
    };
    let center_highlighted = hovered_handle == Some(AnimationGizmoHandle::Center)
        || active_handle == Some(AnimationGizmoHandle::Center);
    painter.circle_filled(
        origin,
        if center_highlighted { 7.0 } else { 4.0 },
        if center_highlighted {
            STUDIO_ACCENT
        } else {
            Color32::from_rgb(235, 242, 248)
        },
    );
    for screen_axis in axes {
        let handle = AnimationGizmoHandle::Axis(screen_axis.axis);
        let highlighted = hovered_handle == Some(handle) || active_handle == Some(handle);
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

fn axis_gizmo_drag_pixels(
    pointer_delta: Vec2,
    preview_rect: Rect,
    render_size: [usize; 2],
    gizmo: model_import_preview::PreviewAxisGizmo,
    axis: CapsuleEditAxis,
) -> Option<f32> {
    let screen_axis = axis_gizmo_screen_axes(preview_rect, render_size, gizmo)
        .into_iter()
        .find(|candidate| candidate.axis == axis)?;
    let direction = screen_axis.end - screen_axis.start;
    let length = direction.length();
    (length >= 4.0).then(|| pointer_delta.dot(direction / length))
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

fn preview_joints_in_rect(
    selection: Rect,
    preview_rect: Rect,
    render_size: [usize; 2],
    joints: &[Option<[f32; 2]>],
) -> Vec<u16> {
    joints
        .iter()
        .enumerate()
        .filter_map(|(index, point)| {
            let point = preview_image_to_screen((*point)?, preview_rect, render_size);
            selection
                .contains(point)
                .then_some(index.min(u16::MAX as usize) as u16)
        })
        .collect()
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

fn preview_model_combo(
    ui: &mut egui::Ui,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Loadout").color(STUDIO_TEXT_WEAK))
            .on_hover_text(
                "Swap the compatible model shown in the preview without changing the clip or timeline position",
            );
        let selected = current
            .and_then(|id| options.iter().find(|(rid, _)| *rid == id))
            .map(|(_, name)| name.as_str())
            .unwrap_or("(unavailable)");
        changed |= searchable_picker(
            ui,
            "animation-viewer-preview-model",
            current,
            selected,
            options,
            SearchablePickerConfig::required()
                .with_width(152.0)
                .with_popup_min_width(360.0)
                .with_search_hint("Search compatible loadouts…"),
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

/// Models shown by the non-destructive Loadout picker must share the active
/// model's skeleton. When the selected path belongs to a cooked clip resource,
/// it must also resolve for the candidate; this prevents a target-specific
/// animation from being silently previewed against the wrong bind pose.
fn compatible_preview_model_options(
    project: &ProjectDocument,
    selected_model: Option<ResourceId>,
    selected_clip_path: Option<&str>,
) -> Vec<(ResourceId, String)> {
    let Some((selected_model_id, selected_skeleton)) = selected_model.and_then(|model_id| {
        project.resource(model_id).and_then(|resource| {
            let ResourceData::Model(model) = &resource.data else {
                return None;
            };
            Some((model_id, model.skeleton))
        })
    }) else {
        return Vec::new();
    };

    let selected_clip_is_cooked = selected_clip_path.is_some_and(|path| {
        project.resources.iter().any(|resource| {
            matches!(
                &resource.data,
                ResourceData::AnimationClip(clip) if clip.psxanim_path == path
            )
        })
    });

    project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::Model(model) = &resource.data else {
                return None;
            };
            if resource.id != selected_model_id
                && (selected_skeleton.is_none() || model.skeleton != selected_skeleton)
            {
                return None;
            }
            if selected_clip_is_cooked
                && selected_clip_path.is_some_and(|path| {
                    !project
                        .resolved_model_animation_clips(resource.id)
                        .iter()
                        .any(|clip| clip.psxanim_path == path)
                })
            {
                return None;
            }
            Some((resource.id, resource.name.clone()))
        })
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
    let mut baked_sources = HashSet::new();
    let authoring_labels = collect_animation_clip_authoring_labels(project);
    let model_skeleton = project.resource(model_id).and_then(|resource| {
        let ResourceData::Model(model) = &resource.data else {
            return None;
        };
        model.skeleton
    });

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
            .and_then(|id| project.resource(id))
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => {
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
        let ResourceData::AnimationSource(source) = &resource.data else {
            continue;
        };
        if source.skeleton != model_skeleton
            || source
                .target_model
                .is_some_and(|target_model| target_model != model_id)
        {
            continue;
        }
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
    atlas: Option<Vec<ColorImage>>,
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

/// Weapon previews keep a small resource-keyed cache so dual-wield actions
/// do not decode the same two models again on every editor frame.
fn load_weapon_model_context(
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    id: ResourceId,
) -> Option<Arc<LoadedModelContext>> {
    let mut cached = state
        .cached_weapon_models
        .iter()
        .position(|entry| entry.resource == id)
        .map(|index| state.cached_weapon_models.remove(index));
    let context = load_model_context_into(project, project_root, &mut cached, id)?;
    if let Some(cached) = cached {
        if state.cached_weapon_models.len() >= 8 {
            state.cached_weapon_models.remove(0);
        }
        state.cached_weapon_models.push(cached);
    }
    Some(context)
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
        .and_then(|bytes| decode_psxt_palette_banks(&bytes));
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
    decode_psxt_palette_banks(bytes)?.into_iter().next()
}

pub(crate) fn decode_psxt_palette_banks(bytes: &[u8]) -> Option<Vec<ColorImage>> {
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

    if clut_entries == 0 {
        let mut pixels = Vec::with_capacity(pixel_count);
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
        return Some(vec![ColorImage {
            size: [width, height],
            pixels,
        }]);
    }

    let entries_per_bank = if clut_entries == 256 {
        256
    } else if (16..=64).contains(&clut_entries) && clut_entries.is_multiple_of(16) {
        16
    } else {
        return None;
    };
    let bank_count = clut_entries / entries_per_bank;
    let mut banks = Vec::with_capacity(bank_count);
    for bank in 0..bank_count {
        let palette = &palette[bank * entries_per_bank..(bank + 1) * entries_per_bank];
        let mut pixels = Vec::with_capacity(pixel_count);
        if entries_per_bank == 16 {
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
        } else {
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
        }
        if pixels.len() != pixel_count {
            return None;
        }
        banks.push(ColorImage {
            size: [width, height],
            pixels,
        });
    }
    Some(banks)
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
        options.speed_q8 = 384;
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
    fn moveset_binding_authoring_assigns_and_disables_compatible_clips() {
        let (mut project, character, animation_set, clip) = timeline_fixture();
        assert!(compatible_moveset_clip_options(&project, character)
            .iter()
            .any(|(candidate, _)| *candidate == clip));

        assert!(store_moveset_action_clip(
            &mut project,
            character,
            CharacterAnimationAction::LightAttack,
            Some(clip),
        ));
        let ResourceData::AnimationSet(set) = &project.resource(animation_set).unwrap().data else {
            panic!("animation set expected");
        };
        assert_eq!(
            set.action_clip(CharacterAnimationAction::LightAttack),
            Some(clip)
        );

        assert!(store_moveset_action_clip(
            &mut project,
            character,
            CharacterAnimationAction::LightAttack,
            None,
        ));
        let ResourceData::AnimationSet(set) = &project.resource(animation_set).unwrap().data else {
            panic!("animation set expected");
        };
        assert_eq!(set.action_clip(CharacterAnimationAction::LightAttack), None);
    }

    #[test]
    fn playback_stops_one_shots_and_wraps_looping_clips() {
        assert_eq!(
            advance_animation_playback(8.0, 4.0, 10, false),
            (9.0, false)
        );
        assert_eq!(advance_animation_playback(8.0, 4.0, 10, true), (3.0, true));
        assert_eq!(advance_animation_playback(0.0, 2.0, 1, false), (0.0, false));

        let mut options = CharacterActionOptions::for_action(CharacterAnimationAction::HeavyAttack);
        options.frame_start = 2;
        options.frame_end = 5;
        assert_eq!(action_playback_frame_bounds(10, options), (2, 5));
        assert_eq!(
            advance_animation_playback_in_range(0.0, 1.0, 2, 5, false),
            (3.0, true)
        );
        assert_eq!(
            advance_animation_playback_in_range(5.0, 1.0, 2, 5, false),
            (5.0, false)
        );
        assert_eq!(
            advance_animation_playback_in_range(5.0, 1.0, 2, 5, true),
            (2.0, true)
        );
    }

    #[test]
    fn action_duration_reports_saved_speed_and_selected_frame_range() {
        let animation = LoadedAnimationStats {
            frame_count: 11,
            sample_rate_hz: 10,
        };
        let mut options = CharacterActionOptions::for_action(CharacterAnimationAction::LightAttack);
        assert!((action_duration_seconds(animation, options, 256) - 1.0).abs() < 0.001);
        assert!((action_duration_seconds(animation, options, 512) - 0.5).abs() < 0.001);

        options.frame_start = 2;
        options.frame_end = 5;
        assert!((action_duration_seconds(animation, options, 256) - 0.4).abs() < 0.001);
    }

    #[test]
    fn combo_weapon_preview_collects_every_authored_lane() {
        let (mut project, character, animation_set, _) = timeline_fixture();
        let light = project.add_resource(
            "Sword1 Light",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        let heavy = project.add_resource(
            "Sword1 Heavy",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        let ResourceData::AnimationSet(set) =
            &mut project.resource_mut(animation_set).unwrap().data
        else {
            panic!("animation set expected");
        };
        set.weapon_appearance_tracks = vec![
            psxed_project::WeaponAppearanceTrack {
                action: CharacterAnimationAction::ComboAttack,
                weapon: light,
                character_socket: "right_hand_grip".to_string(),
                fully_visible_frame: 25,
                hidden_frame: 44,
                transition_frames: 8,
                trail: None,
            },
            psxed_project::WeaponAppearanceTrack {
                action: CharacterAnimationAction::ComboAttack,
                weapon: heavy,
                character_socket: "right_hand_grip".to_string(),
                fully_visible_frame: 44,
                hidden_frame: psxed_project::ACTION_FRAME_END_FULL,
                transition_frames: 8,
                trail: None,
            },
            psxed_project::WeaponAppearanceTrack {
                action: CharacterAnimationAction::ComboAttack,
                weapon: light,
                character_socket: "left_hand_grip".to_string(),
                fully_visible_frame: 35,
                hidden_frame: psxed_project::ACTION_FRAME_END_FULL,
                transition_frames: 8,
                trail: None,
            },
        ];

        let tracks = character_action_weapon_tracks(
            &project,
            Some(character),
            CharacterAnimationAction::ComboAttack,
        );
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[1].1.weapon, heavy);
        assert_eq!(tracks[2].1.character_socket, "left_hand_grip");
    }

    #[test]
    fn hand_authoring_finds_named_left_and_right_hand_joints() {
        let joints = vec![
            "mixamorig:Hips".to_string(),
            "mixamorig:RightHand".to_string(),
            "Armature_LeftHand".to_string(),
        ];
        assert_eq!(
            suggested_hand_joint(Some(&joints), CharacterHand::Right),
            Some(1)
        );
        assert_eq!(
            suggested_hand_joint(Some(&joints), CharacterHand::Left),
            Some(2)
        );
    }

    #[test]
    fn sword_assignments_allow_both_hands_but_reject_the_same_pair_twice() {
        let mut project = ProjectDocument::new("two-hand-assignments");
        let sword = project.add_resource(
            "Sword",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        let tracks = vec![psxed_project::WeaponAppearanceTrack {
            action: CharacterAnimationAction::ComboAttack,
            weapon: sword,
            character_socket: CharacterHand::Right.socket_name().to_string(),
            fully_visible_frame: 0,
            hidden_frame: psxed_project::ACTION_FRAME_END_FULL,
            transition_frames: 0,
            trail: None,
        }];

        assert!(weapon_appearance_pair_is_used(
            &tracks,
            CharacterAnimationAction::ComboAttack,
            sword,
            CharacterHand::Right.socket_name(),
            None,
        ));
        assert!(!weapon_appearance_pair_is_used(
            &tracks,
            CharacterAnimationAction::ComboAttack,
            sword,
            CharacterHand::Left.socket_name(),
            None,
        ));
    }

    #[test]
    fn timeline_can_expand_past_all_combo_lanes_and_scroll_when_shorter() {
        let combo_lane_content = TIMELINE_RULER_HEIGHT + 7.0 * TIMELINE_TRACK_HEIGHT;
        assert!(
            animation_timeline_height_limit(400.0) >= combo_lane_content + 32.0,
            "a 400px workspace should resize far enough for the timeline header and seven lanes"
        );
        assert_eq!(TIMELINE_MIN_PREVIEW_HEIGHT, 120.0);
    }

    #[test]
    fn overflowing_authoring_controls_cannot_push_the_timeline_out_of_its_slot() {
        let reserved_for = |content_height| {
            let context = egui::Context::default();
            let mut reserved_height = None;
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..egui::RawInput::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let before = ui.cursor().top();
                        fixed_height_studio_region(ui, "fixed-height-test", 120.0, |ui| {
                            ui.allocate_space(egui::vec2(100.0, content_height));
                        });
                        reserved_height = Some(ui.cursor().top() - before);
                    });
                },
            );
            reserved_height.expect("studio region should be drawn")
        };

        let compact = reserved_for(20.0);
        let overflowing = reserved_for(500.0);
        assert!(
            (overflowing - compact).abs() < 0.01,
            "overflowing controls grew the parent from {compact}px to {overflowing}px"
        );
    }

    #[test]
    fn entering_pose_authoring_pauses_playback() {
        let mut viewer = ModelAnimationViewerState::default();
        assert!(viewer.playing);
        viewer.set_studio_mode(AnimationStudioMode::Pose);
        assert!(!viewer.playing);
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
    fn projectile_timeline_lane_edits_charge_to_exact_release() {
        let (mut project, character, _, _) = timeline_fixture();
        let ResourceData::Character(profile) = &mut project.resource_mut(character).unwrap().data
        else {
            panic!("character resource expected");
        };
        profile
            .combat_capsules
            .push(psxed_project::CharacterCombatCapsule {
                name: "Choir Needle Muzzle".to_string(),
                role: psxed_project::CombatCapsuleRole::ProjectileEmitter {
                    action: CharacterAnimationAction::LightAttack,
                    charge_start_frame: 4,
                    active_start_frame: 9,
                    active_end_frame: 9,
                    projectile: None,
                    speed: 112,
                    lifetime_ticks: 180,
                    min_range: 512,
                    max_range: 4096,
                    damage: 18,
                    poise_damage: 8,
                    tint_rgb: [62, 214, 198],
                },
                ..psxed_project::CharacterCombatCapsule::default()
            });
        let index = profile.combat_capsules.len() - 1;

        let lanes = timeline_hitboxes(&project, character, CharacterAnimationAction::LightAttack);
        let lane = lanes.iter().find(|lane| lane.index == index).unwrap();
        assert_eq!(lane.kind, TimelineCombatKind::Projectile);
        assert_eq!((lane.start, lane.end), (4, 9));
        assert!(store_timeline_hitbox_range(
            &mut project,
            Some(character),
            index,
            CharacterAnimationAction::LightAttack,
            6,
            12,
        ));
        let ResourceData::Character(profile) = &project.resource(character).unwrap().data else {
            unreachable!();
        };
        let psxed_project::CombatCapsuleRole::ProjectileEmitter {
            charge_start_frame,
            active_start_frame,
            active_end_frame,
            ..
        } = profile.combat_capsules[index].role
        else {
            unreachable!();
        };
        assert_eq!(charge_start_frame, 6);
        assert_eq!(active_start_frame, 12);
        assert_eq!(active_end_frame, 12);
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
    fn loadout_switches_compatible_models_without_moving_the_timeline() {
        let mut project = ProjectDocument::new("animation-loadouts");
        let skeleton = project.add_resource(
            "Mantis Rig",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 2,
                parents: vec![None, Some(0)],
                signature: "mantis-rig".to_string(),
                note: String::new(),
                joint_names: vec!["Root".to_string(), "LeftForeArm".to_string()],
            }),
        );
        let other_skeleton = project.add_resource(
            "Other Rig",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "other-rig".to_string(),
                note: String::new(),
                joint_names: vec!["Root".to_string()],
            }),
        );
        let make_model = |path: &str, skeleton| {
            ResourceData::Model(psxed_project::ModelResource {
                model_path: path.to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            })
        };
        let artigli = project.add_resource(
            "Light Enemy / Artigli",
            make_model("assets/mantis_artigli.psxmdl", skeleton),
        );
        let light = project.add_resource(
            "Light Enemy / Clawless Body",
            make_model("assets/mantis_clawless.psxmdl", skeleton),
        );
        let heavy = project.add_resource(
            "Light Enemy / Alternate Body",
            make_model("assets/mantis_alternate.psxmdl", skeleton),
        );
        let foreign = project.add_resource(
            "Unrelated Enemy",
            make_model("assets/unrelated.psxmdl", other_skeleton),
        );
        let shared_path = "assets/mantis_attack.psxanim";
        project.add_resource(
            "Mantis Attack",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: shared_path.to_string(),
                skeleton: Some(skeleton),
                target_model: None,
                source: None,
                bake: psxed_project::AnimationClipBakeKind::LegacyShared,
                role: AnimationRole::Attack,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );
        let character = project.add_resource(
            "Light Enemy / Artigli",
            ResourceData::Character(psxed_project::CharacterResource {
                model: Some(artigli),
                ..psxed_project::CharacterResource::defaults()
            }),
        );

        let options = compatible_preview_model_options(&project, Some(artigli), Some(shared_path));
        assert_eq!(
            options.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![artigli, light, heavy]
        );
        assert!(!options.iter().any(|(id, _)| *id == foreign));

        let mut viewer = ModelAnimationViewerState {
            selected_model: Some(artigli),
            selected_character: Some(character),
            selected_clip_path: Some(shared_path.to_string()),
            last_clip_path: Some(shared_path.to_string()),
            playing: false,
            frame: 13.5,
            ..Default::default()
        };

        assert!(viewer.switch_preview_model(&project, heavy));
        assert_eq!(viewer.selected_model, Some(heavy));
        assert_eq!(viewer.selected_character, Some(character));
        assert_eq!(viewer.selected_clip_path(), Some(shared_path));
        assert_eq!(viewer.last_clip_path.as_deref(), Some(shared_path));
        assert_eq!(viewer.frame, 13.5);
        assert!(!viewer.playing);
        assert!(!viewer.switch_preview_model(&project, foreign));
        assert_eq!(viewer.selected_model, Some(heavy));
    }

    #[test]
    fn loadout_hides_models_that_cannot_resolve_a_targeted_clip() {
        let mut project = ProjectDocument::new("targeted-loadout");
        let skeleton = project.add_resource(
            "Rig",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "rig".to_string(),
                note: String::new(),
                joint_names: vec!["Root".to_string()],
            }),
        );
        let make_model = |path: &str| {
            ResourceData::Model(psxed_project::ModelResource {
                model_path: path.to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            })
        };
        let target = project.add_resource("Target", make_model("assets/target.psxmdl"));
        let sibling = project.add_resource("Sibling", make_model("assets/sibling.psxmdl"));
        let clip_path = "assets/target_only.psxanim";
        project.add_resource(
            "Target Only",
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: clip_path.to_string(),
                skeleton: Some(skeleton),
                target_model: Some(target),
                source: None,
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Idle,
                looping: true,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            }),
        );

        let options = compatible_preview_model_options(&project, Some(target), Some(clip_path));
        assert_eq!(options, vec![(target, "Target".to_string())]);
        assert!(!options.iter().any(|(id, _)| *id == sibling));
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
    fn shared_enemy_atlas_decodes_all_four_model_palette_banks() {
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/default");
        let atlas_bytes =
            std::fs::read(project_root.join("assets/models/shared_enemy_01/shared_enemy_01.psxt"))
                .expect("shared enemy atlas");
        let banks = decode_psxt_palette_banks(&atlas_bytes).expect("decode palette banks");
        assert_eq!(banks.len(), 4);
        assert!(banks.iter().all(|bank| bank.size == [256, 256]));

        for (model_path, clip_path, world_height) in [
            (
                "assets/models/rust_mantis/rust_mantis.psxmdl",
                "assets/animations/rust_mantis_starter/idle.psxanim",
                1024,
            ),
            (
                "assets/models/tank_boss_animated_model/tank_boss_animated_model.psxmdl",
                "assets/animations/tank_boss_ai/idle.psxanim",
                1536,
            ),
        ] {
            let model_bytes = std::fs::read(project_root.join(model_path)).expect("enemy model");
            let model = psx_asset::Model::from_bytes(&model_bytes).expect("parse enemy model");
            assert_eq!(model.palette_bank_count(), 4, "{model_path}");
            let clip_bytes = std::fs::read(project_root.join(clip_path)).expect("enemy idle clip");
            let render = |atlases: &[ColorImage]| {
                model_import_preview::render_import_model_preview_with_equipment_set_at_size(
                    &model_bytes,
                    &clip_bytes,
                    atlases,
                    ImportPreviewOptions {
                        world_height,
                        visual_scale_q8: psxed_project::MODEL_SCALE_ONE_Q8,
                        visual_yaw_q12: 0,
                        collision_radius: 192,
                        time_seconds: 0.0,
                        yaw_q12: 340,
                        pitch_q12: 350,
                        radius: 0,
                        focus_on_animated_bounds: true,
                        preview_in_place: true,
                        pose_offset: [0, 0, 0],
                        show_animation_root: false,
                        show_collision_guides: false,
                        show_bones: false,
                    },
                    model_import_preview::euler_rotation_q12([0; 3]),
                    [320, 240],
                    &[],
                    &[],
                    &[],
                    None,
                    None,
                    None,
                )
                .expect("banked enemy preview")
                .image
            };
            let banked = render(&banks);
            let bank_zero_only = render(std::slice::from_ref(&banks[0]));
            assert_ne!(
                banked.pixels, bank_zero_only.pixels,
                "{model_path} must select its authored palette bank per face"
            );
        }
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
    fn pose_marquee_selects_every_visible_joint_inside_the_box() {
        let preview = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(640.0, 480.0));
        let joints = vec![
            Some([100.0, 100.0]),
            Some([108.0, 100.0]),
            Some([200.0, 180.0]),
            None,
        ];
        let selection = Rect::from_min_max(Pos2::new(290.0, 240.0), Pos2::new(325.0, 260.0));

        assert_eq!(
            preview_joints_in_rect(selection, preview, [320, 240], &joints),
            vec![0, 1]
        );
    }

    #[test]
    fn combat_preview_uses_the_selected_attack_or_first_authored_attack() {
        let hurtbox = psxed_project::CharacterCombatCapsule::default();
        let hitbox = psxed_project::CharacterCombatCapsule {
            role: psxed_project::CombatCapsuleRole::Hitbox {
                action: CharacterAnimationAction::HeavyAttack,
                active_start_frame: 17,
                active_end_frame: 23,
                damage: 20,
                poise_damage: 10,
            },
            ..Default::default()
        };
        let light_hitbox = psxed_project::CharacterCombatCapsule {
            role: psxed_project::CombatCapsuleRole::Hitbox {
                action: CharacterAnimationAction::LightAttack,
                active_start_frame: 8,
                active_end_frame: 12,
                damage: 10,
                poise_damage: 5,
            },
            ..Default::default()
        };
        let capsules = [hurtbox, hitbox, light_hitbox];

        assert_eq!(
            combat_preview_action_window(&capsules, 1, CharacterAnimationAction::Idle),
            Some((CharacterAnimationAction::HeavyAttack, 17))
        );
        assert_eq!(
            combat_preview_action_window(&capsules, 0, CharacterAnimationAction::LightAttack),
            Some((CharacterAnimationAction::LightAttack, 8))
        );
        let preview = preview_combat_capsules(&capsules, 1, true);
        assert_eq!(preview.len(), 3);
        assert!(preview[1].selected);
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
            CapsuleEditAxis::Y,
            CapsuleGizmoDelta::Move(10),
        ));
        assert!(manipulate_selected_capsule(
            &mut project,
            character,
            0,
            CapsuleEditAxis::X,
            CapsuleGizmoDelta::ResizeAxis(20),
        ));
        assert!(manipulate_selected_capsule(
            &mut project,
            character,
            0,
            CapsuleEditAxis::X,
            CapsuleGizmoDelta::ResizeRadius(8),
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
    fn clip_picker_lists_only_the_selected_models_compatible_animation_catalogue() {
        let mut project = ProjectDocument::new("animation-association");
        let light_skeleton = project.add_resource(
            "Light Enemy Skeleton",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "light-enemy".to_string(),
                note: String::new(),
                joint_names: vec!["Root".to_string()],
            }),
        );
        let heavy_skeleton = project.add_resource(
            "Heavy Enemy Skeleton",
            ResourceData::Skeleton(psxed_project::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "heavy-enemy".to_string(),
                note: String::new(),
                joint_names: vec!["Root".to_string()],
            }),
        );
        let model_data = |path: &str, skeleton| {
            ResourceData::Model(psxed_project::ModelResource {
                model_path: path.to_string(),
                source_path: None,
                texture_path: None,
                skeleton: Some(skeleton),
                world_height: 1024,
                collision_radius: 192,
                scale_q8: [psxed_project::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            })
        };
        let light = project.add_resource(
            "Light Enemy",
            model_data("assets/light.psxmdl", light_skeleton),
        );
        let light_variant = project.add_resource(
            "Light Enemy / Clawless Body",
            model_data("assets/light_clawless.psxmdl", light_skeleton),
        );
        let heavy = project.add_resource(
            "Heavy Enemy",
            model_data("assets/heavy.psxmdl", heavy_skeleton),
        );
        let clip_data = |path: &str, skeleton, target_model| {
            ResourceData::AnimationClip(psxed_project::AnimationClipResource {
                psxanim_path: path.to_string(),
                skeleton: Some(skeleton),
                target_model,
                source: None,
                bake: psxed_project::AnimationClipBakeKind::Retargeted,
                role: AnimationRole::Attack,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
                pose_corrections: Vec::new(),
            })
        };
        let light_shared = project.add_resource(
            "Light Enemy / Attack",
            clip_data("assets/light_attack.psxanim", light_skeleton, None),
        );
        let light_targeted = project.add_resource(
            "Light Enemy / Targeted",
            clip_data("assets/light_targeted.psxanim", light_skeleton, Some(light)),
        );
        let sibling_targeted = project.add_resource(
            "Light Enemy / Variant Only",
            clip_data(
                "assets/light_variant.psxanim",
                light_skeleton,
                Some(light_variant),
            ),
        );
        let heavy_clip = project.add_resource(
            "Heavy Enemy / Attack",
            clip_data("assets/heavy_attack.psxanim", heavy_skeleton, None),
        );
        let source_data = |path: &str, skeleton, target_model| {
            ResourceData::AnimationSource(psxed_project::AnimationSourceResource {
                source_path: path.to_string(),
                clip_name: String::new(),
                provider: psxed_project::AnimationSourceProvider::Other,
                skeleton: Some(skeleton),
                target_model,
                role: AnimationRole::Attack,
                looping: false,
                tags: Vec::new(),
            })
        };
        let light_source = project.add_resource(
            "Light Raw",
            source_data("source/light.glb", light_skeleton, None),
        );
        let light_targeted_source = project.add_resource(
            "Light Targeted Raw",
            source_data("source/light_targeted.glb", light_skeleton, Some(light)),
        );
        let sibling_source = project.add_resource(
            "Sibling Raw",
            source_data(
                "source/light_variant.glb",
                light_skeleton,
                Some(light_variant),
            ),
        );
        let heavy_source = project.add_resource(
            "Heavy Raw",
            source_data("source/heavy.glb", heavy_skeleton, Some(heavy)),
        );

        let option_resources = build_clip_options(&project, light)
            .into_iter()
            .filter_map(|option| option.resource)
            .collect::<Vec<_>>();

        assert_eq!(
            option_resources,
            vec![
                light_targeted,
                light_shared,
                light_source,
                light_targeted_source
            ]
        );
        for excluded in [sibling_targeted, heavy_clip, sibling_source, heavy_source] {
            assert!(!option_resources.contains(&excluded));
        }
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
                    translation_space: psxed_project::AttachmentSocketTranslationSpace::JointOffset,
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
                AxisEditDelta::Rotate(120),
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
    fn pose_gizmo_applies_the_same_edit_to_every_marquee_selected_joint() {
        let (mut project, clip) = project_with_pose_clip();
        assert!(manipulate_pose_corrections(
            &mut project,
            clip,
            &[2, 4, 7],
            9,
            CapsuleEditAxis::Y,
            AxisEditDelta::Translate(14),
        ));

        let ResourceData::AnimationClip(animation) = &project.resource(clip).unwrap().data else {
            panic!("animation clip expected");
        };
        assert_eq!(animation.pose_corrections.len(), 3);
        assert_eq!(
            animation
                .pose_corrections
                .iter()
                .map(|key| (key.joint, key.frame, key.translation))
                .collect::<Vec<_>>(),
            vec![(2, 9, [0, 14, 0]), (4, 9, [0, 14, 0]), (7, 9, [0, 14, 0]),]
        );
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
            AxisEditDelta::Rotate(120),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Y,
            AxisEditDelta::Rotate(3600),
        ));
        assert!(manipulate_selected_socket(
            &mut project,
            id,
            0,
            CapsuleEditAxis::Z,
            AxisEditDelta::Rotate(-240),
        ));
        // 1024 + 300 * 12 = 4624, wrapping one full turn to 528.
        assert_eq!(first_socket(&project, id).rotation_q12, [120, 528, 3856]);
    }

    #[test]
    fn weapon_grip_gizmo_edits_the_weapon_alignment_target() {
        let mut project = ProjectDocument::new("weapon-grip-gizmo");
        let weapon = project.add_resource(
            "Sword",
            ResourceData::Weapon(psxed_project::WeaponResource::default()),
        );
        assert!(manipulate_selected_weapon_grip(
            &mut project,
            weapon,
            CapsuleEditAxis::X,
            AxisEditDelta::Translate(24),
        ));
        assert!(manipulate_selected_weapon_grip(
            &mut project,
            weapon,
            CapsuleEditAxis::Z,
            AxisEditDelta::Rotate(128),
        ));
        let ResourceData::Weapon(weapon) = &project.resource(weapon).unwrap().data else {
            panic!("weapon resource expected");
        };
        assert_eq!(weapon.grip.translation[0], -24);
        assert_eq!(weapon.grip.rotation_q12[2], -128);
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
    fn combat_resize_center_handle_wins_over_axes_at_the_origin() {
        let preview = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
        let gizmo = model_import_preview::PreviewAxisGizmo {
            origin: [100.0, 100.0],
            axis_ends: [[150.0, 100.0], [100.0, 125.0], [75.0, 75.0]],
            local_axis_units: 200.0,
        };
        assert_eq!(
            pick_animation_gizmo_handle(Pos2::new(201.0, 199.0), preview, [320, 240], gizmo, true,),
            Some(AnimationGizmoHandle::Center)
        );
        assert_eq!(
            pick_animation_gizmo_handle(Pos2::new(250.0, 201.0), preview, [320, 240], gizmo, true,),
            Some(AnimationGizmoHandle::Axis(CapsuleEditAxis::X))
        );
    }

    #[test]
    fn hidden_combat_capsules_are_removed_from_the_preview_only() {
        let capsules = vec![
            psxed_project::CharacterCombatCapsule::default(),
            psxed_project::CharacterCombatCapsule {
                name: "Attack Hitbox".to_string(),
                role: psxed_project::CombatCapsuleRole::Hitbox {
                    action: CharacterAnimationAction::LightAttack,
                    active_start_frame: 3,
                    active_end_frame: 6,
                    damage: 25,
                    poise_damage: 25,
                },
                ..psxed_project::CharacterCombatCapsule::default()
            },
        ];

        let visible = preview_combat_capsules(&capsules, 1, true);
        assert_eq!(visible.len(), 2);
        assert!(!visible[0].selected);
        assert!(visible[1].selected);
        assert!(preview_combat_capsules(&capsules, 1, false).is_empty());
        assert_eq!(capsules.len(), 2, "preview visibility must not delete data");
    }

    #[test]
    fn rotation_drag_uses_the_selected_axis_screen_direction() {
        let preview = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
        let gizmo = model_import_preview::PreviewAxisGizmo {
            origin: [100.0, 100.0],
            axis_ends: [[150.0, 100.0], [100.0, 125.0], [75.0, 75.0]],
            local_axis_units: 200.0,
        };
        assert_eq!(
            axis_gizmo_drag_pixels(
                Vec2::new(0.0, 10.0),
                preview,
                [320, 240],
                gizmo,
                CapsuleEditAxis::X,
            )
            .unwrap()
            .round() as i32,
            0
        );
        assert_eq!(
            axis_gizmo_drag_pixels(
                Vec2::new(0.0, 10.0),
                preview,
                [320, 240],
                gizmo,
                CapsuleEditAxis::Y,
            )
            .unwrap()
            .round() as i32,
            10
        );
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
            trail: None,
        };
        assert_eq!(preview_weapon_materialization_q12(&track, 7.0, 30), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 8.0, 30), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 10.0, 30), 2048);
        assert_eq!(preview_weapon_materialization_q12(&track, 12.0, 30), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 22.0, 30), 2048);
        assert_eq!(preview_weapon_materialization_q12(&track, 24.0, 30), 0);
    }

    #[test]
    fn fully_materialized_weapon_uses_its_texture_instead_of_the_transition_cage() {
        assert!(!preview_weapon_uses_materialization_wireframe(Some(0), 0));
        assert!(preview_weapon_uses_materialization_wireframe(Some(0), 2048));
        assert!(!preview_weapon_uses_materialization_wireframe(
            Some(0),
            4096
        ));
        assert!(!preview_weapon_uses_materialization_wireframe(None, 2048));
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
            trail: None,
        };
        assert_eq!(preview_weapon_materialization_q12(&track, 5.99, 20), 0);
        assert_eq!(preview_weapon_materialization_q12(&track, 6.0, 20), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 8.99, 20), 4096);
        assert_eq!(preview_weapon_materialization_q12(&track, 9.0, 20), 0);
    }
}
