use crate::centered_aspect_rect;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use psx_asset::{Animation, Texture};
use psxed_project::{
    model_import::resolve_path, AnimationClipCalibration, AnimationRole, NodeKind, ProjectDocument,
    ResourceData, ResourceId,
};

use crate::icons;
use crate::model_import_preview::{self, ImportPreviewOptions};
use crate::style::{STUDIO_ACCENT, STUDIO_BORDER, STUDIO_PANEL_DARK, STUDIO_TEXT_WEAK};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnimationPreviewQuality {
    Authoring,
    PsxOutput,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelAnimationViewerState {
    selected_model: Option<ResourceId>,
    selected_clip_path: Option<String>,
    last_clip_path: Option<String>,
    playing: bool,
    frame: f32,
    playback_speed: f32,
    yaw_q12: i32,
    pitch_q12: i32,
    radius: i32,
    show_animation_root: bool,
    show_bones: bool,
    preview_quality: AnimationPreviewQuality,
    cached_model: Option<CachedModelContext>,
    cached_clip: Option<CachedClipContext>,
    last_time_seconds: f64,
}

impl Default for ModelAnimationViewerState {
    fn default() -> Self {
        Self {
            selected_model: None,
            selected_clip_path: None,
            last_clip_path: None,
            playing: true,
            frame: 0.0,
            playback_speed: 1.0,
            yaw_q12: 340,
            pitch_q12: 350,
            radius: 0,
            show_animation_root: false,
            show_bones: false,
            preview_quality: AnimationPreviewQuality::Authoring,
            cached_model: None,
            cached_clip: None,
            last_time_seconds: 0.0,
        }
    }
}

impl ModelAnimationViewerState {
    pub(crate) const fn selected_model(&self) -> Option<ResourceId> {
        self.selected_model
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
        match &resource.data {
            ResourceData::Character(character) => {
                self.selected_model = character.model;
                self.selected_clip_path = self.preferred_model_clip_path(project);
                self.reset_clip_clock();
            }
            ResourceData::Model(_) => {
                self.selected_model = Some(id);
                self.selected_clip_path = self.preferred_model_clip_path(project);
                self.reset_clip_clock();
            }
            ResourceData::AnimationClip(clip) => {
                self.selected_model = clip
                    .target_model
                    .or_else(|| first_model_for_skeleton(project, clip.skeleton));
                self.selected_clip_path = Some(clip.psxanim_path.clone());
                self.reset_clip_clock();
            }
            ResourceData::AnimationSource(source) => {
                self.selected_model = source
                    .target_model
                    .or_else(|| first_model_for_skeleton(project, source.skeleton));
                self.selected_clip_path = Some(source.source_path.clone());
                self.reset_clip_clock();
            }
            ResourceData::AnimationSet(set) => {
                self.selected_model = first_model_for_skeleton(project, set.skeleton);
                self.selected_clip_path = self.preferred_model_clip_path(project);
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
    }

    fn invalidate_clip_cache(&mut self) {
        self.cached_clip = None;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    path: PathBuf,
    len: u64,
    modified_millis: u128,
}

impl FileStamp {
    fn read(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(&path).ok()?;
        let modified_millis = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        Some(Self {
            path,
            len: metadata.len(),
            modified_millis,
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
    let clip_context = selected_clip
        .as_ref()
        .and_then(|clip| load_clip_context_cached(project_root, state, clip));

    if state.last_clip_path.as_deref() != state.selected_clip_path.as_deref() {
        state.frame = 0.0;
        state.last_clip_path = state.selected_clip_path.clone();
        state.last_time_seconds = ui.input(|input| input.time);
    }
    let selected_model = state.selected_model;

    if resource_combo(
        ui,
        "Model",
        "animation-viewer-model",
        &mut state.selected_model,
        &model_options,
    ) {
        state.selected_clip_path = None;
        state.reset_clip_clock();
        state.invalidate_model_cache();
        state.invalidate_clip_cache();
        state.ensure_selection(project);
    }
    clip_combo(ui, state, &clip_options);
    ui.separator();
    let mut action = draw_playback_controls(
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
    action
}

pub(crate) fn draw_model_animation_viewer(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) {
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
    let clip_context = selected_clip
        .as_ref()
        .and_then(|clip| load_clip_context_cached(project_root, state, clip));

    draw_preview(
        ui,
        state,
        model_context.as_deref(),
        selected_clip.as_ref(),
        clip_context.as_deref(),
        preview_texture,
    );
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

fn draw_preview(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    selected_clip: Option<&ViewerClipOption>,
    clip: Option<&LoadedClipContext>,
    preview_texture: &mut Option<egui::TextureHandle>,
) {
    let size = ui.available_size();
    let size = Vec2::new(size.x.max(360.0), size.y.max(260.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::drag());
    if response.dragged() {
        let delta = ui.input(|input| input.pointer.delta());
        state.yaw_q12 = (state.yaw_q12 - (delta.x * 6.0) as i32).rem_euclid(4096);
        state.pitch_q12 = (state.pitch_q12 - (delta.y * 4.0) as i32).clamp(64, 960);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
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
        return;
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
        return;
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
        return;
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
        return;
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
        return;
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
        return;
    };

    let seconds = state.frame.max(0.0) as f64 / animation.sample_rate_hz.max(1) as f64;
    let render_size = preview_render_size(ui, rect, state.preview_quality);
    let texture_options = match state.preview_quality {
        AnimationPreviewQuality::Authoring => egui::TextureOptions::LINEAR,
        AnimationPreviewQuality::PsxOutput => egui::TextureOptions::NEAREST,
    };
    let image = model_import_preview::render_import_model_preview_with_orientation_at_size(
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
            show_bones: state.show_bones,
        },
        model_import_preview::euler_rotation_q12(model.authored_rotation_q12),
        render_size,
    );

    match image {
        Some(image) => {
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
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(selected)
            .width(90.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "(none)").clicked() {
                    *current = None;
                    changed = true;
                }
                for (id, name) in options {
                    if ui.selectable_label(*current == Some(*id), name).clicked() {
                        *current = Some(*id);
                        changed = true;
                    }
                }
            });
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
            .width(108.0)
            .show_ui(ui, |ui| {
                for option in options {
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

    for clip in project.resolved_model_animation_clips(model_id) {
        let (role, looping, origin) = clip
            .animation_resource
            .and_then(|id| project.resource(id).map(|resource| (id, resource)))
            .and_then(|(id, resource)| match &resource.data {
                ResourceData::AnimationClip(clip) => {
                    seen_resources.insert(id);
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
            label: clip.name,
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
        seen_paths.insert(clip.psxanim_path.clone());
        out.push(ViewerClipOption {
            label: resource.name.clone(),
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
        if seen_paths.contains(&source.source_path) {
            continue;
        }
        out.push(ViewerClipOption {
            label: resource.name.clone(),
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

#[derive(Debug, Clone)]
struct LoadedModelContext {
    model_bytes: Vec<u8>,
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

    if let Some(cached) = &state.cached_model {
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
        atlas,
        world_height: model_resource.world_height,
        collision_radius: model_resource.collision_radius,
        visual_scale_q8,
        default_visual_yaw_q12: model_resource.default_visual_yaw_q12,
        authored_rotation_q12,
        orientation_label,
    });
    state.cached_model = Some(CachedModelContext {
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

#[derive(Debug, Clone, Copy)]
struct LoadedAnimationStats {
    frame_count: u16,
    sample_rate_hz: u16,
}

fn load_clip_context_cached(
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    clip: &ViewerClipOption,
) -> Option<Arc<LoadedClipContext>> {
    if !clip.previewable {
        return None;
    }
    let path = resolve_path(&clip.path, Some(project_root));
    let stamp = FileStamp::read(path.clone())?;
    if let Some(cached) = &state.cached_clip {
        if cached.path == clip.path && cached.stamp == stamp {
            return Some(Arc::clone(&cached.context));
        }
    }
    let bytes = std::fs::read(path).ok()?;
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
        context: Arc::clone(&context),
    });
    Some(context)
}

fn is_cooked_animation_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".psxanim") && !path.contains("::")
}

fn decode_psxt_image(bytes: &[u8]) -> Option<ColorImage> {
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
            }),
        );
        let mut viewer = ModelAnimationViewerState::default();

        viewer.focus_resource(&project, clip);

        assert_eq!(viewer.selected_model(), Some(target_model));
        assert_eq!(viewer.selected_clip_path(), Some(clip_path));
    }

    #[test]
    fn frame_preview_restores_content_derived_radius_without_changing_angle() {
        let mut viewer = ModelAnimationViewerState::default();
        viewer.yaw_q12 = 1024;
        viewer.pitch_q12 = 128;
        viewer.radius = 4096;

        viewer.frame_preview();

        assert_eq!(viewer.radius, 0);
        assert_eq!(viewer.yaw_q12, 1024);
        assert_eq!(viewer.pitch_q12, 128);
    }
}
