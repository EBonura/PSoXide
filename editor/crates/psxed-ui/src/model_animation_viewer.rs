use std::path::Path;

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use psx_asset::{Animation, Model, Texture};
use psxed_project::{
    model_import::resolve_path, AnimationRole, ProjectDocument, ResourceData, ResourceId,
};

use crate::icons;
use crate::model_import_preview::{self, ImportPreviewOptions};
use crate::style::{STUDIO_BORDER, STUDIO_PANEL_DARK, STUDIO_TEXT_WEAK};

#[derive(Debug, Clone)]
pub(crate) struct ModelAnimationViewerState {
    selected_character: Option<ResourceId>,
    selected_model: Option<ResourceId>,
    selected_animation_set: Option<ResourceId>,
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
    show_info: bool,
    last_time_seconds: f64,
}

impl Default for ModelAnimationViewerState {
    fn default() -> Self {
        Self {
            selected_character: None,
            selected_model: None,
            selected_animation_set: None,
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
            show_info: false,
            last_time_seconds: 0.0,
        }
    }
}

impl ModelAnimationViewerState {
    pub(crate) fn focus_resource(&mut self, project: &ProjectDocument, id: ResourceId) {
        let Some(resource) = project.resource(id) else {
            return;
        };
        match &resource.data {
            ResourceData::Character(character) => {
                self.selected_character = Some(id);
                self.selected_model = character.model;
                self.selected_animation_set = character.animation_set;
                self.selected_clip_path =
                    role_clip_path(project, character.animation_set, AnimationRole::Idle)
                        .or_else(|| self.first_model_clip_path(project));
                self.reset_clip_clock();
            }
            ResourceData::Model(_) => {
                self.selected_character = None;
                self.selected_model = Some(id);
                self.selected_clip_path = self.first_model_clip_path(project);
                self.reset_clip_clock();
            }
            ResourceData::AnimationSet(set) => {
                self.selected_animation_set = Some(id);
                self.selected_clip_path = role_clip_path(project, Some(id), AnimationRole::Idle)
                    .or_else(|| {
                        set.walk_clip
                            .and_then(|clip| animation_clip_path(project, clip))
                    })
                    .or_else(|| self.first_model_clip_path(project));
                self.reset_clip_clock();
            }
            ResourceData::AnimationClip(clip) => {
                self.selected_clip_path = Some(clip.psxanim_path.clone());
                self.reset_clip_clock();
            }
            _ => {}
        }
        self.ensure_selection(project);
    }

    fn ensure_selection(&mut self, project: &ProjectDocument) {
        if self.selected_character.is_some_and(|id| {
            !matches!(
                project.resource(id).map(|r| &r.data),
                Some(ResourceData::Character(_))
            )
        }) {
            self.selected_character = None;
        }
        if self.selected_model.is_some_and(|id| {
            !matches!(
                project.resource(id).map(|r| &r.data),
                Some(ResourceData::Model(_))
            )
        }) {
            self.selected_model = None;
        }
        if self.selected_animation_set.is_some_and(|id| {
            !matches!(
                project.resource(id).map(|r| &r.data),
                Some(ResourceData::AnimationSet(_))
            )
        }) {
            self.selected_animation_set = None;
        }

        if self.selected_model.is_none() {
            self.selected_model = first_model_id(project);
        }
        if self.selected_animation_set.is_none() {
            self.selected_animation_set = first_animation_set_id(project);
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
            self.selected_clip_path =
                role_clip_path(project, self.selected_animation_set, AnimationRole::Idle)
                    .filter(|path| clip_options.iter().any(|clip| clip.path == *path))
                    .or_else(|| clip_options.first().map(|clip| clip.path.clone()));
            self.reset_clip_clock();
        }
    }

    fn first_model_clip_path(&self, project: &ProjectDocument) -> Option<String> {
        self.selected_model.and_then(|model| {
            build_clip_options(project, model)
                .first()
                .map(|clip| clip.path.clone())
        })
    }

    fn reset_clip_clock(&mut self) {
        self.frame = 0.0;
        self.last_clip_path = None;
    }
}

#[derive(Clone)]
struct ViewerClipOption {
    label: String,
    path: String,
    origin: ClipOrigin,
    role: AnimationRole,
    looping: bool,
    resource: Option<ResourceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipOrigin {
    Baked,
    Library,
}

impl ClipOrigin {
    const fn label(self) -> &'static str {
        match self {
            Self::Baked => "baked",
            Self::Library => "library",
        }
    }
}

pub(crate) fn draw_model_animation_viewer(
    ui: &mut egui::Ui,
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) {
    state.ensure_selection(project);

    let character_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Character(_)));
    let model_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Model(_)));
    let set_options = collect_resource_options(project, |data| {
        matches!(data, ResourceData::AnimationSet(_))
    });
    let clip_options = state
        .selected_model
        .map(|id| build_clip_options(project, id))
        .unwrap_or_default();

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            if resource_combo(
                ui,
                "Profile",
                "animation-viewer-character",
                &mut state.selected_character,
                &character_options,
            ) {
                if let Some(id) = state.selected_character {
                    state.focus_resource(project, id);
                }
            }
            ui.separator();
            if resource_combo(
                ui,
                "Model",
                "animation-viewer-model",
                &mut state.selected_model,
                &model_options,
            ) {
                state.selected_character = None;
                state.selected_clip_path = None;
                state.reset_clip_clock();
                state.ensure_selection(project);
            }
            if resource_combo(
                ui,
                "Clip Role Map",
                "animation-viewer-set",
                &mut state.selected_animation_set,
                &set_options,
            ) {
                state.selected_clip_path =
                    role_clip_path(project, state.selected_animation_set, AnimationRole::Idle);
                state.reset_clip_clock();
                state.ensure_selection(project);
            }
        });

        ui.horizontal_wrapped(|ui| {
            clip_combo(ui, state, &clip_options);
            if let Some(set_id) = state.selected_animation_set {
                for role in [
                    AnimationRole::Idle,
                    AnimationRole::Walk,
                    AnimationRole::Run,
                    AnimationRole::Turn,
                ] {
                    let Some(path) = role_clip_path(project, Some(set_id), role) else {
                        continue;
                    };
                    if ui
                        .small_button(role.label())
                        .on_hover_text(format!("Preview the role map's {} clip", role.label()))
                        .clicked()
                    {
                        state.selected_clip_path = Some(path);
                        state.reset_clip_clock();
                    }
                }
            }
            ui.separator();
            ui.checkbox(&mut state.show_animation_root, "Root");
            ui.checkbox(&mut state.show_bones, "Bones")
                .on_hover_text("Draws mesh-owned joint centers; cooked models do not yet store exact bind-pose bone pivots.");
            ui.checkbox(&mut state.show_info, "Info");
        });

        ui.separator();

        let model_context = state
            .selected_model
            .and_then(|id| load_model_context(project, project_root, id));
        let selected_clip = state
            .selected_clip_path
            .as_ref()
            .and_then(|path| clip_options.iter().find(|clip| clip.path == *path))
            .cloned();
        let clip_context = selected_clip
            .as_ref()
            .and_then(|clip| load_clip_context(project_root, clip));

        if state.last_clip_path.as_deref() != state.selected_clip_path.as_deref() {
            state.frame = 0.0;
            state.last_clip_path = state.selected_clip_path.clone();
            state.last_time_seconds = ui.input(|input| input.time);
        }

        draw_playback_controls(
            ui,
            state,
            clip_context.as_ref().and_then(|clip| clip.animation_stats),
        );

        let info_height = if state.show_info { 150.0 } else { 0.0 };
        let preview_height = (ui.available_height() - info_height).max(320.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), preview_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                draw_preview(
                    ui,
                    state,
                    model_context.as_ref(),
                    clip_context.as_ref(),
                    preview_texture,
                );
            },
        );

        if state.show_info {
            ui.separator();
            draw_diagnostics(
                ui,
                project,
                state,
                model_context.as_ref(),
                selected_clip.as_ref(),
                clip_context.as_ref(),
            );
        }
    });
}

fn draw_playback_controls(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    animation: Option<LoadedAnimationStats>,
) {
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

    ui.horizontal(|ui| {
        if ui
            .button(if state.playing {
                icons::label(icons::PLAY, "Pause")
            } else {
                icons::label(icons::PLAY, "Play")
            })
            .clicked()
        {
            state.playing = !state.playing;
            state.last_time_seconds = now;
        }
        let Some(animation) = animation else {
            ui.weak("No clip loaded");
            return;
        };
        let max_frame = animation.frame_count.saturating_sub(1).max(1);
        let mut frame = state.frame.round() as u16;
        if ui
            .add(egui::Slider::new(&mut frame, 0..=max_frame).text("Frame"))
            .changed()
        {
            state.frame = frame as f32;
            state.playing = false;
        }
        ui.label(
            RichText::new(format!(
                "{} / {} @ {} Hz",
                frame,
                animation.frame_count.saturating_sub(1),
                animation.sample_rate_hz
            ))
            .monospace()
            .color(STUDIO_TEXT_WEAK),
        );
        ui.add(
            egui::Slider::new(&mut state.playback_speed, 0.1..=2.0)
                .text("Speed")
                .step_by(0.1),
        );
    });
}

fn draw_preview(
    ui: &mut egui::Ui,
    state: &mut ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    clip: Option<&LoadedClipContext>,
    preview_texture: &mut Option<egui::TextureHandle>,
) {
    let size = ui.available_size();
    let size = Vec2::new(size.x.max(360.0), size.y.max(260.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::drag());
    if response.dragged() {
        let delta = ui.input(|input| input.pointer.delta());
        state.yaw_q12 = (state.yaw_q12 + (delta.x * 6.0) as i32).rem_euclid(4096);
        state.pitch_q12 = (state.pitch_q12 + (delta.y * 4.0) as i32).clamp(64, 960);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        let scroll = ui.input(|input| input.raw_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            let current = effective_radius(state, model);
            state.radius = (current - (scroll * 8.0) as i32).clamp(256, 8192);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, STUDIO_PANEL_DARK);

    let (Some(model), Some(clip)) = (model, clip) else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Select a model and animation clip",
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
    let image = model_import_preview::render_import_model_preview_with_options(
        &model.model_bytes,
        &clip.bytes,
        atlas,
        ImportPreviewOptions {
            world_height: model.world_height as i32,
            time_seconds: seconds,
            yaw_q12: state.yaw_q12.rem_euclid(4096) as u16,
            pitch_q12: state.pitch_q12.rem_euclid(4096) as u16,
            radius: effective_radius(state, Some(model)),
            show_animation_root: state.show_animation_root,
            show_bones: state.show_bones,
        },
    );

    match image {
        Some(image) => {
            let texture_id = match preview_texture {
                Some(handle) => {
                    handle.set(image, egui::TextureOptions::NEAREST);
                    handle.id()
                }
                None => {
                    let handle = ui.ctx().load_texture(
                        "model-animation-viewer-preview",
                        image,
                        egui::TextureOptions::NEAREST,
                    );
                    let id = handle.id();
                    *preview_texture = Some(handle);
                    id
                }
            };
            let preview_rect = centered_aspect_rect(
                rect.shrink(8.0),
                model_import_preview::PREVIEW_WIDTH as f32
                    / model_import_preview::PREVIEW_HEIGHT as f32,
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

fn draw_diagnostics(
    ui: &mut egui::Ui,
    project: &ProjectDocument,
    state: &ModelAnimationViewerState,
    model: Option<&LoadedModelContext>,
    clip: Option<&ViewerClipOption>,
    clip_context: Option<&LoadedClipContext>,
) {
    egui::ScrollArea::vertical()
        .id_salt("animation-viewer-diagnostics")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(RichText::new("Diagnostics").strong());
            ui.separator();

            if let Some(model) = model {
                metric(ui, "Model", &model.name);
                if let Some(parsed) = &model.model_stats {
                    metric(ui, "Joints", &parsed.joint_count.to_string());
                    metric(ui, "Parts", &parsed.part_count.to_string());
                    metric(
                        ui,
                        "Bone Points",
                        &format!("{}/{}", parsed.overlay_joint_points, parsed.joint_count),
                    );
                    metric(ui, "Vertices", &parsed.vertex_count.to_string());
                    metric(ui, "Faces", &parsed.face_count.to_string());
                } else {
                    warning(ui, "Model parse failed.");
                }
                if model.atlas.is_none() {
                    warning(ui, "Atlas is missing or failed to parse.");
                }
            } else {
                warning(ui, "No model selected.");
            }

            ui.add_space(8.0);
            if let Some(clip) = clip {
                metric(ui, "Clip", &clip.label);
                metric(ui, "Origin", clip.origin.label());
                metric(ui, "Role", clip.role.label());
                metric(ui, "Looping", if clip.looping { "yes" } else { "no" });
                if let Some(resource_id) = clip.resource {
                    metric(ui, "Resource", &format!("#{}", resource_id.raw()));
                }
            } else {
                warning(ui, "No clip selected.");
            }

            if let Some(clip_context) = clip_context {
                if let Some(animation) = &clip_context.animation_stats {
                    metric(ui, "Frames", &animation.frame_count.to_string());
                    metric(
                        ui,
                        "Sample Rate",
                        &format!("{} Hz", animation.sample_rate_hz),
                    );
                    metric(ui, "Clip Joints", &animation.joint_count.to_string());
                    if let Some(model) = model.and_then(|ctx| ctx.model_stats.as_ref()) {
                        if model.joint_count != animation.joint_count {
                            warning(
                                ui,
                                &format!(
                                    "Joint mismatch: model {} vs clip {}.",
                                    model.joint_count, animation.joint_count
                                ),
                            );
                        }
                    }
                    if animation.identity_first_pose {
                        warning(ui, "First pose is bind/identity; this is risky for idle.");
                    }
                } else {
                    warning(ui, "Animation parse failed.");
                }
            }

            if let (Some(model_id), Some(clip)) = (state.selected_model, clip) {
                if let Some(resource_id) = clip.resource {
                    let model_skeleton =
                        project
                            .resource(model_id)
                            .and_then(|resource| match &resource.data {
                                ResourceData::Model(model) => model.skeleton,
                                _ => None,
                            });
                    let clip_skeleton =
                        project
                            .resource(resource_id)
                            .and_then(|resource| match &resource.data {
                                ResourceData::AnimationClip(clip) => clip.skeleton,
                                _ => None,
                            });
                    if model_skeleton != clip_skeleton {
                        warning(ui, "Skeleton resource differs from the selected model.");
                    }
                }
            }
        });
}

fn metric(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK));
        ui.label(RichText::new(value).monospace());
    });
}

fn warning(ui: &mut egui::Ui, text: &str) {
    ui.colored_label(Color32::from_rgb(220, 160, 80), text);
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
            .width(180.0)
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
            .width(280.0)
            .show_ui(ui, |ui| {
                for option in options {
                    if ui
                        .selectable_label(
                            state.selected_clip_path.as_deref() == Some(option.path.as_str()),
                            format!("{} · {}", option.label, option.origin.label()),
                        )
                        .clicked()
                    {
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

fn first_animation_set_id(project: &ProjectDocument) -> Option<ResourceId> {
    project.resources.iter().find_map(|resource| {
        matches!(resource.data, ResourceData::AnimationSet(_)).then_some(resource.id)
    })
}

fn build_clip_options(project: &ProjectDocument, model_id: ResourceId) -> Vec<ViewerClipOption> {
    project
        .resolved_model_animation_clips(model_id)
        .into_iter()
        .map(|clip| {
            let (role, looping) = clip
                .animation_resource
                .and_then(|id| project.resource(id))
                .and_then(|resource| match &resource.data {
                    ResourceData::AnimationClip(clip) => Some((clip.role, clip.looping)),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    (
                        AnimationRole::guess_from_name(&clip.name),
                        AnimationRole::guess_from_name(&clip.name).matches_looping_default(),
                    )
                });
            ViewerClipOption {
                label: clip.name,
                path: clip.psxanim_path,
                origin: if clip.animation_resource.is_some() {
                    ClipOrigin::Library
                } else {
                    ClipOrigin::Baked
                },
                role,
                looping,
                resource: clip.animation_resource,
            }
        })
        .collect()
}

trait AnimationRoleLoopingDefault {
    fn matches_looping_default(self) -> bool;
}

impl AnimationRoleLoopingDefault for AnimationRole {
    fn matches_looping_default(self) -> bool {
        matches!(
            self,
            AnimationRole::Idle | AnimationRole::Walk | AnimationRole::Run | AnimationRole::Turn
        )
    }
}

fn animation_clip_path(project: &ProjectDocument, id: ResourceId) -> Option<String> {
    project
        .resource(id)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) => Some(clip.psxanim_path.clone()),
            _ => None,
        })
}

fn role_clip_path(
    project: &ProjectDocument,
    set_id: Option<ResourceId>,
    role: AnimationRole,
) -> Option<String> {
    let set = set_id
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => Some(set),
            _ => None,
        })?;
    set.role_clip(role)
        .and_then(|clip_id| animation_clip_path(project, clip_id))
}

struct LoadedModelContext {
    name: String,
    model_bytes: Vec<u8>,
    model_stats: Option<LoadedModelStats>,
    atlas: Option<ColorImage>,
    world_height: u16,
}

struct LoadedModelStats {
    joint_count: u16,
    part_count: u16,
    overlay_joint_points: u16,
    vertex_count: u16,
    face_count: u16,
}

fn load_model_context(
    project: &ProjectDocument,
    project_root: &Path,
    id: ResourceId,
) -> Option<LoadedModelContext> {
    let resource = project.resource(id)?;
    let ResourceData::Model(model_resource) = &resource.data else {
        return None;
    };
    let model_path = resolve_path(&model_resource.model_path, Some(project_root));
    let model_bytes = std::fs::read(model_path).ok()?;
    let model_stats = Model::from_bytes(&model_bytes)
        .ok()
        .map(|model| LoadedModelStats {
            joint_count: model.joint_count(),
            part_count: model.part_count(),
            overlay_joint_points: count_mesh_owned_joint_points(&model),
            vertex_count: model.vertex_count(),
            face_count: model.face_count(),
        });
    let atlas = model_resource
        .texture_path
        .as_ref()
        .and_then(|path| std::fs::read(resolve_path(path, Some(project_root))).ok())
        .and_then(|bytes| decode_psxt_image(&bytes));
    Some(LoadedModelContext {
        name: resource.name.clone(),
        model_bytes,
        model_stats,
        atlas,
        world_height: model_resource.world_height,
    })
}

fn count_mesh_owned_joint_points(model: &Model<'_>) -> u16 {
    let mut has_vertices = vec![false; model.joint_count() as usize];
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let joint = part.joint_index() as usize;
        if joint < has_vertices.len() && part.vertex_count() > 0 {
            has_vertices[joint] = true;
        }
    }
    has_vertices.iter().filter(|has| **has).count() as u16
}

struct LoadedClipContext {
    bytes: Vec<u8>,
    animation_stats: Option<LoadedAnimationStats>,
}

#[derive(Debug, Clone, Copy)]
struct LoadedAnimationStats {
    joint_count: u16,
    frame_count: u16,
    sample_rate_hz: u16,
    identity_first_pose: bool,
}

fn load_clip_context(project_root: &Path, clip: &ViewerClipOption) -> Option<LoadedClipContext> {
    let path = resolve_path(&clip.path, Some(project_root));
    let bytes = std::fs::read(path).ok()?;
    let animation_stats =
        Animation::from_bytes(&bytes)
            .ok()
            .map(|animation| LoadedAnimationStats {
                joint_count: animation.joint_count(),
                frame_count: animation.frame_count(),
                sample_rate_hz: animation.sample_rate_hz(),
                identity_first_pose: is_identity_first_pose(&animation),
            });
    Some(LoadedClipContext {
        bytes,
        animation_stats,
    })
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

fn is_identity_first_pose(animation: &Animation<'_>) -> bool {
    if animation.frame_count() != 1 {
        return false;
    }
    for joint in 0..animation.joint_count() {
        let Some(pose) = animation.pose(0, joint) else {
            return false;
        };
        let identity = [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
        if pose.matrix != identity {
            return false;
        }
        if pose.translation.x != 0 || pose.translation.y != 0 || pose.translation.z != 0 {
            return false;
        }
    }
    true
}

fn effective_radius(state: &ModelAnimationViewerState, model: Option<&LoadedModelContext>) -> i32 {
    if state.radius > 0 {
        state.radius
    } else {
        model
            .map(|model| (model.world_height as i32).saturating_mul(3) / 2)
            .unwrap_or(1536)
    }
    .clamp(256, 8192)
}

fn centered_aspect_rect(container: Rect, aspect: f32) -> Rect {
    let size = container.size();
    if size.x <= 0.0 || size.y <= 0.0 || aspect <= 0.0 {
        return container;
    }
    let (width, height) = if size.x / size.y > aspect {
        (size.y * aspect, size.y)
    } else {
        (size.x, size.x / aspect)
    };
    Rect::from_center_size(container.center(), Vec2::new(width, height))
}
