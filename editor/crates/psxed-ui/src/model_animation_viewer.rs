use std::{collections::HashSet, path::Path};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use psx_asset::{Animation, Texture};
use psxed_project::{
    model_import::resolve_path, AnimationRole, ProjectDocument, ResourceData, ResourceId,
};

use crate::icons;
use crate::model_import_preview::{self, ImportPreviewOptions};
use crate::style::{STUDIO_BORDER, STUDIO_PANEL_DARK, STUDIO_TEXT_WEAK};

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
    preview_in_place: bool,
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
            preview_in_place: true,
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
                self.selected_clip_path = Some(clip.psxanim_path.clone());
                self.reset_clip_clock();
            }
            ResourceData::AnimationSource(source) => {
                self.selected_clip_path = Some(source.source_path.clone());
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
            let model = project
                .resource(model_id)
                .and_then(|resource| match &resource.data {
                    ResourceData::Model(model) => Some(model),
                    _ => None,
                })?;
            model
                .effective_preview_clip()
                .and_then(|index| model.clips.get(index as usize))
                .map(|clip| clip.psxanim_path.clone())
                .or_else(|| model.clips.first().map(|clip| clip.psxanim_path.clone()))
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
    previewable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipOrigin {
    Model,
    Target,
    Library,
    Source,
}

impl ClipOrigin {
    const fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Target => "target",
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
}

pub(crate) fn draw_model_animation_viewer(
    ui: &mut egui::Ui,
    project: &ProjectDocument,
    project_root: &Path,
    state: &mut ModelAnimationViewerState,
    preview_texture: &mut Option<egui::TextureHandle>,
) -> Option<AnimationViewerAction> {
    state.ensure_selection(project);
    let mut action = None;

    let model_options =
        collect_resource_options(project, |data| matches!(data, ResourceData::Model(_)));
    let clip_options = state
        .selected_model
        .map(|id| build_clip_options(project, id))
        .unwrap_or_default();

    ui.vertical(|ui| {
        ui.horizontal_wrapped(|ui| {
            if resource_combo(
                ui,
                "Model",
                "animation-viewer-model",
                &mut state.selected_model,
                &model_options,
            ) {
                state.selected_clip_path = None;
                state.reset_clip_clock();
                state.ensure_selection(project);
            }
            clip_combo(ui, state, &clip_options);
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

        let selected_model = state.selected_model;
        action = draw_playback_controls(
            ui,
            state,
            selected_model,
            selected_clip.as_ref(),
            clip_context.as_ref().and_then(|clip| clip.animation_stats),
        );

        let preview_height = ui.available_height().max(320.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), preview_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                draw_preview(
                    ui,
                    state,
                    model_context.as_ref(),
                    selected_clip.as_ref(),
                    clip_context.as_ref(),
                    preview_texture,
                );
            },
        );
    });
    action
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

    ui.horizontal(|ui| {
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
            draw_overlay_toggles(ui, state);
            return;
        };
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
        draw_overlay_toggles(ui, state);
    });
    action
}

fn draw_overlay_toggles(ui: &mut egui::Ui, state: &mut ModelAnimationViewerState) {
    ui.separator();
    ui.toggle_value(
        &mut state.show_bones,
        icons::label(icons::WAYPOINT, "Bones"),
    )
    .on_hover_text("Draw the cooked skeleton overlay");
    ui.toggle_value(
        &mut state.show_animation_root,
        icons::label(icons::CIRCLE_DOT, "Anchor"),
    )
    .on_hover_text("Draw the body-derived preview anchor");
    ui.toggle_value(
        &mut state.preview_in_place,
        icons::label(icons::MOVE, "In-place"),
    )
    .on_hover_text("Preview with root-motion translation removed");
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
    let image = model_import_preview::render_import_model_preview_with_options(
        &model.model_bytes,
        &clip.bytes,
        atlas,
        ImportPreviewOptions {
            world_height: model.world_height as i32,
            time_seconds: seconds,
            yaw_q12: state.yaw_q12.rem_euclid(4096) as u16,
            pitch_q12: state.pitch_q12.rem_euclid(4096) as u16,
            radius: state.radius,
            focus_on_animated_bounds: true,
            preview_in_place: state.preview_in_place,
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
            .width(280.0)
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
        ui.label(RichText::new("Animation").color(STUDIO_TEXT_WEAK));
        let selected = state
            .selected_clip_path
            .as_ref()
            .and_then(|path| options.iter().find(|option| option.path == *path))
            .map(|option| option.label.as_str())
            .unwrap_or("(none)");
        egui::ComboBox::from_id_salt("animation-viewer-clip")
            .selected_text(selected)
            .width(520.0)
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
                    let origin = if clip.target_model == Some(model_id) {
                        ClipOrigin::Target
                    } else {
                        ClipOrigin::Library
                    };
                    seen_resources.insert(id);
                    Some((clip.role, clip.looping, origin))
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let role = AnimationRole::guess_from_name(&clip.name);
                (role, role.matches_looping_default(), ClipOrigin::Model)
            });
        seen_paths.insert(clip.psxanim_path.clone());
        out.push(ViewerClipOption {
            label: clip.name,
            path: clip.psxanim_path,
            origin,
            role,
            looping,
            resource: clip.animation_resource,
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
            origin: if clip.target_model == Some(model_id) {
                ClipOrigin::Target
            } else {
                ClipOrigin::Library
            },
            role: clip.role,
            looping: clip.looping,
            resource: Some(resource.id),
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
            previewable: is_cooked_animation_path(&source.source_path),
        });
    }

    out
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

struct LoadedModelContext {
    model_bytes: Vec<u8>,
    atlas: Option<ColorImage>,
    world_height: u16,
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
    let atlas = model_resource
        .texture_path
        .as_ref()
        .and_then(|path| std::fs::read(resolve_path(path, Some(project_root))).ok())
        .and_then(|bytes| decode_psxt_image(&bytes));
    Some(LoadedModelContext {
        model_bytes,
        atlas,
        world_height: model_resource.world_height,
    })
}

struct LoadedClipContext {
    bytes: Vec<u8>,
    animation_stats: Option<LoadedAnimationStats>,
}

#[derive(Debug, Clone, Copy)]
struct LoadedAnimationStats {
    frame_count: u16,
    sample_rate_hz: u16,
}

fn load_clip_context(project_root: &Path, clip: &ViewerClipOption) -> Option<LoadedClipContext> {
    if !clip.previewable {
        return None;
    }
    let path = resolve_path(&clip.path, Some(project_root));
    let bytes = std::fs::read(path).ok()?;
    let animation_stats =
        Animation::from_bytes(&bytes)
            .ok()
            .map(|animation| LoadedAnimationStats {
                frame_count: animation.frame_count(),
                sample_rate_hz: animation.sample_rate_hz(),
            });
    Some(LoadedClipContext {
        bytes,
        animation_stats,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_project::{
        default_model_collision_radius_for_height, AnimationClipBakeKind, AnimationClipResource,
        AnimationSourceProvider, AnimationSourceResource, ModelAnimationClip, ModelResource,
        ProjectDocument, SkeletonResource, MODEL_SCALE_ONE_Q8,
    };

    fn test_model(path: &str, skeleton: ResourceId) -> ModelResource {
        ModelResource {
            model_path: path.to_string(),
            source_path: None,
            texture_path: None,
            skeleton: Some(skeleton),
            clips: Vec::new(),
            default_clip: None,
            preview_clip: None,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024),
            scale_q8: [MODEL_SCALE_ONE_Q8; 3],
            attachments: Vec::new(),
        }
    }

    #[test]
    fn clip_options_include_target_library_and_source_entries() {
        let mut project = ProjectDocument::new("Animation Viewer Test");
        let skeleton = project.add_resource(
            "Humanoid Skeleton",
            ResourceData::Skeleton(SkeletonResource {
                joint_count: 24,
                parents: vec![None],
                signature: "psx-parent-v1:24:root".to_string(),
                note: String::new(),
            }),
        );
        let model_a = project.add_resource(
            "Knight",
            ResourceData::Model(test_model("assets/models/knight.psxmdl", skeleton)),
        );
        let model_b = project.add_resource(
            "Wraith",
            ResourceData::Model(test_model("assets/models/wraith.psxmdl", skeleton)),
        );

        let target_clip = project.add_resource(
            "Knight Native Walk",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/models/knight/walk.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: Some(model_a),
                bake: AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Walk,
                looping: true,
                tags: vec!["meshy".to_string()],
            }),
        );
        let shared_clip = project.add_resource(
            "Mixamo Idle",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/animations/mixamo/idle.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: AnimationClipBakeKind::LegacyShared,
                role: AnimationRole::Idle,
                looping: true,
                tags: vec!["mixamo".to_string()],
            }),
        );
        let other_model_clip = project.add_resource(
            "Wraith Native Attack",
            ResourceData::AnimationClip(AnimationClipResource {
                psxanim_path: "assets/models/wraith/attack.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: Some(model_b),
                bake: AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Attack,
                looping: false,
                tags: vec!["meshy".to_string()],
            }),
        );
        let source = project.add_resource(
            "Synty Sword Slash",
            ResourceData::AnimationSource(AnimationSourceResource {
                source_path:
                    "ANIMATION_Sword_Combat_SourceFiles_v5.zip::SourceFiles/Animations/slash.fbx"
                        .to_string(),
                clip_name: "slash".to_string(),
                provider: AnimationSourceProvider::Synty,
                skeleton: None,
                target_model: None,
                role: AnimationRole::Attack,
                looping: false,
                tags: vec!["synty".to_string(), "sword".to_string()],
            }),
        );

        let options = build_clip_options(&project, model_a);
        let target_index = options
            .iter()
            .position(|option| option.resource == Some(target_clip))
            .expect("target clip should be listed");
        let shared_index = options
            .iter()
            .position(|option| option.resource == Some(shared_clip))
            .expect("shared library clip should be listed");
        let other_model = options
            .iter()
            .find(|option| option.resource == Some(other_model_clip))
            .expect("other model cooked clip should still be visible");
        let source_only = options
            .iter()
            .find(|option| option.resource == Some(source))
            .expect("catalogued source should be visible");

        assert!(
            target_index < shared_index,
            "target-native clips should remain easy to find before generic clips",
        );
        assert_eq!(other_model.origin, ClipOrigin::Library);
        assert!(other_model.previewable);
        assert_eq!(source_only.origin, ClipOrigin::Source);
        assert!(!source_only.previewable);
    }

    #[test]
    fn model_selection_prefers_model_preview_clip() {
        let mut project = ProjectDocument::new("Animation Viewer Test");
        let skeleton = project.add_resource(
            "Humanoid Skeleton",
            ResourceData::Skeleton(SkeletonResource {
                joint_count: 24,
                parents: vec![None],
                signature: "psx-parent-v1:24:root".to_string(),
                note: String::new(),
            }),
        );
        let mut model = test_model("assets/models/knight.psxmdl", skeleton);
        model.clips = vec![
            ModelAnimationClip {
                name: "a_tpose".to_string(),
                psxanim_path: "assets/models/knight/a_tpose.psxanim".to_string(),
            },
            ModelAnimationClip {
                name: "idle".to_string(),
                psxanim_path: "assets/models/knight/idle.psxanim".to_string(),
            },
        ];
        model.default_clip = Some(1);
        model.preview_clip = Some(1);
        let model_id = project.add_resource("Knight", ResourceData::Model(model));

        let mut state = ModelAnimationViewerState::default();
        state.focus_resource(&project, model_id);

        assert_eq!(
            state.selected_clip_path.as_deref(),
            Some("assets/models/knight/idle.psxanim")
        );
    }

    #[test]
    fn clip_options_do_not_duplicate_baked_source_paths() {
        let mut project = ProjectDocument::new("Animation Viewer Test");
        let skeleton = project.add_resource(
            "Humanoid Skeleton",
            ResourceData::Skeleton(SkeletonResource {
                joint_count: 24,
                parents: vec![None],
                signature: "psx-parent-v1:24:root".to_string(),
                note: String::new(),
            }),
        );
        let mut model = test_model("assets/models/knight.psxmdl", skeleton);
        model.clips = vec![ModelAnimationClip {
            name: "idle".to_string(),
            psxanim_path: "assets/models/knight/idle.psxanim".to_string(),
        }];
        let model_id = project.add_resource("Knight", ResourceData::Model(model));
        project.add_resource(
            "Idle Source",
            ResourceData::AnimationSource(AnimationSourceResource {
                source_path: "assets/models/knight/idle.psxanim".to_string(),
                clip_name: "idle".to_string(),
                provider: AnimationSourceProvider::Unknown,
                skeleton: Some(skeleton),
                target_model: Some(model_id),
                role: AnimationRole::Idle,
                looping: true,
                tags: vec![],
            }),
        );

        let options = build_clip_options(&project, model_id);
        let matching_paths = options
            .iter()
            .filter(|option| option.path == "assets/models/knight/idle.psxanim")
            .count();

        assert_eq!(matching_paths, 1);
    }
}
