use super::*;
use crate::{UiTransition, UiTransitionKind, UiVisibilityCondition};

pub(crate) fn active_far_vista_panel_count(
    texture_panels: &[Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    segments: u8,
) -> usize {
    texture_panels
        .iter()
        .rposition(Option::is_some)
        .map(|index| index + 1)
        .unwrap_or(0)
        .min(segments as usize)
        .min(FAR_VISTA_TEXTURE_PANEL_COUNT)
}

/// Cook every authored UI scene into one shared node pool plus a
/// parallel scene table, and derive the default game flow.
///
/// Each scene's nodes are flattened in hierarchy order and appended
/// to the shared pool; the scene's `node_first` records the pool
/// offset of its block. Parent indices stored on each cooked node
/// are made relative to the shared pool (scene-local index plus the
/// scene's offset), so the runtime can address any scene's nodes
/// through a single table.
///
/// The default flow lists one composed UI-only scene state per cooked UI scene
/// followed by the built-in gameplay state, and
/// enters the first UI scene when one exists. With no UI scenes the
/// flow is a single `Gameplay` state entered at index `0`.
pub(crate) fn cook_ui_nodes(
    project: &ProjectDocument,
    project_root: &Path,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    // Collects the project-global UI source files this cook touched: CD-DA
    // and SFX `.wav`s plus UI image textures. They are not reachable from any
    // room, so a room-driven walk misses them entirely and a project copied
    // without them boots to a placeholder menu with no audio.
    used_ui_source_paths: &mut Vec<String>,
    report: &mut PlaytestValidationReport,
) -> (
    Vec<PlaytestUiNode>,
    Vec<PlaytestUiPaint>,
    Vec<PlaytestUiScene>,
    Vec<PlaytestUiSfxSample>,
    Vec<PlaytestUiSfxCue>,
    PlaytestGameFlow,
    Vec<PlaytestCddaTrack>,
) {
    let mut ui_nodes: Vec<PlaytestUiNode> = Vec::new();
    let mut ui_paints: Vec<PlaytestUiPaint> = Vec::new();
    let mut ui_scenes: Vec<PlaytestUiScene> = Vec::new();
    let mut ui_sfx_samples: Vec<PlaytestUiSfxSample> = Vec::new();
    let mut ui_sfx_cues: Vec<PlaytestUiSfxCue> = Vec::new();
    let mut ui_sfx_sample_for_wav: HashMap<String, u16> = HashMap::new();
    let mut cdda_tracks: Vec<PlaytestCddaTrack> = Vec::new();
    let mut cdda_track_for_wav: HashMap<String, u8> = HashMap::new();
    let mut ui_image_texture_for_path: HashMap<String, CookedUiImageTexture> = HashMap::new();

    for scene in &project.ui_scenes {
        let node_first = ui_nodes.len().min(u16::MAX as usize) as u16;
        cook_ui_scene_nodes(
            scene,
            node_first,
            project,
            project_root,
            texture_asset_for_path,
            assets,
            report,
            &mut ui_image_texture_for_path,
            &mut cdda_tracks,
            &mut cdda_track_for_wav,
            &mut ui_sfx_samples,
            &mut ui_sfx_cues,
            &mut ui_sfx_sample_for_wav,
            &mut ui_paints,
            &mut ui_nodes,
        );
        let node_count = (ui_nodes.len() - node_first as usize).min(u16::MAX as usize) as u16;
        ui_scenes.push(PlaytestUiScene {
            id: (scene.id.raw() & u16::MAX as u64) as u16,
            name: scene.name.clone(),
            node_first,
            node_count,
            focus_style: scene.focus_style,
        });
    }

    let game_flow = cook_game_flow(project, &ui_scenes);

    // Harvest from the same dedupe maps the cook used, so the shipped set
    // cannot drift from what was actually read.
    used_ui_source_paths.extend(ui_sfx_sample_for_wav.keys().cloned());
    used_ui_source_paths.extend(cdda_track_for_wav.keys().cloned());
    used_ui_source_paths.extend(ui_image_texture_for_path.keys().cloned());
    used_ui_source_paths.sort();
    used_ui_source_paths.dedup();

    (
        ui_nodes,
        ui_paints,
        ui_scenes,
        ui_sfx_samples,
        ui_sfx_cues,
        game_flow,
        cdda_tracks,
    )
}

pub(crate) fn cook_game_flow(
    project: &ProjectDocument,
    ui_scenes: &[PlaytestUiScene],
) -> PlaytestGameFlow {
    if project.scene_states.is_empty() {
        return PlaytestGameFlow::default();
    }

    let mut scene_states = Vec::with_capacity(project.scene_states.len());
    let mut states = Vec::with_capacity(project.scene_states.len());
    for authored in &project.scene_states {
        let state_id = cook_scene_state_id(authored.id);
        let ui_scene = authored
            .ui_scene
            .and_then(|id| {
                ui_scenes
                    .iter()
                    .find(|scene| u64::from(scene.id) == id.raw())
                    .map(|scene| scene.id)
            })
            .unwrap_or(psx_level::UI_SCENE_NONE);
        let world = match authored.world {
            crate::SceneWorldLayer::None => PlaytestWorldLayer::None,
            crate::SceneWorldLayer::Gameplay => PlaytestWorldLayer::Gameplay,
        };
        let mut flags = 0;
        if authored.ui_input {
            flags |= psx_level::scene_state_flags::UI_INPUT;
        }
        if authored.pause_world {
            flags |= psx_level::scene_state_flags::PAUSE_WORLD;
        }
        scene_states.push(PlaytestSceneState {
            id: state_id,
            name: authored.name.clone(),
            world,
            ui_scene,
            flags,
        });
        states.push(PlaytestFlowState::SceneState { state: state_id });
    }

    let gameplay_index = scene_states
        .iter()
        .position(|state| state.world == PlaytestWorldLayer::Gameplay)
        .unwrap_or(0) as u16;
    let entry = match project.boot {
        crate::BootTarget::SceneState(id) => scene_states
            .iter()
            .position(|state| state.id == cook_scene_state_id(id))
            .map(|index| index as u16)
            .unwrap_or(gameplay_index),
        crate::BootTarget::Gameplay => gameplay_index,
        crate::BootTarget::UiScene(scene_id) => {
            let cooked_ui_scene = (scene_id.raw() & u16::MAX as u64) as u16;
            scene_states
                .iter()
                .position(|state| state.ui_scene == cooked_ui_scene)
                .map(|index| index as u16)
                .unwrap_or(gameplay_index)
        }
    };

    PlaytestGameFlow {
        states,
        scene_states,
        entry,
    }
}

pub(crate) fn cook_scene_state_id(id: crate::SceneStateId) -> u16 {
    (id.raw() & u16::MAX as u64) as u16
}

/// Flatten one scene's nodes into the shared `out` pool. `node_first`
/// is the pool offset of this scene's block; parent indices are
/// rebased onto the shared pool.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_ui_scene_nodes(
    scene: &crate::UiScene,
    _node_first: u16,
    project: &ProjectDocument,
    project_root: &Path,
    _texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
    ui_image_texture_for_path: &mut HashMap<String, CookedUiImageTexture>,
    cdda_tracks: &mut Vec<PlaytestCddaTrack>,
    cdda_track_for_wav: &mut HashMap<String, u8>,
    ui_sfx_samples: &mut Vec<PlaytestUiSfxSample>,
    ui_sfx_cues: &mut Vec<PlaytestUiSfxCue>,
    ui_sfx_sample_for_wav: &mut HashMap<String, u16>,
    ui_paints: &mut Vec<PlaytestUiPaint>,
    out: &mut Vec<PlaytestUiNode>,
) {
    let ordered_ids = scene.hierarchy_node_ids();
    let mut runtime_index_for_id: HashMap<UiNodeId, u16> = HashMap::new();

    for id in ordered_ids {
        let Some(node) = scene.node(id) else {
            continue;
        };
        let parent = node
            .parent
            .and_then(|id| runtime_index_for_id.get(&id).copied());
        let runtime_index = out.len().min(u16::MAX as usize) as u16;
        runtime_index_for_id.insert(id, runtime_index);

        if let UiNodeKind::Image {
            rect,
            texture,
            tint,
            effect,
        } = &node.kind
        {
            cook_ui_image_node(
                &node.name,
                parent,
                rect,
                *texture,
                *tint,
                *effect,
                project,
                project_root,
                ui_image_texture_for_path,
                assets,
                report,
                out,
            );
            continue;
        }

        let (
            x,
            y,
            width,
            height,
            color,
            background,
            accent,
            color_paint,
            background_paint,
            accent_paint,
            value,
            max,
            texture_asset,
            text,
            tag,
            action,
            option,
            flags,
            font,
            font_scale,
            letter_spacing,
        ) = match &node.kind {
            UiNodeKind::Canvas { width, height } => (
                0,
                0,
                (*width).max(1),
                (*height).max(1),
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                None,
                None,
                None,
                UiValueBinding::ConstantQ12(0),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                psx_level::UI_OPTION_NONE,
                0,
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Group { rect } => (
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                None,
                None,
                None,
                UiValueBinding::ConstantQ12(0),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                psx_level::UI_OPTION_NONE,
                ui_node_flags(rect.anchor, UiTextAlign::Left, false),
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Rect {
                rect,
                color,
                gradient,
            } => (
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                *color,
                [0, 0, 0],
                [0, 0, 0],
                cook_ui_paint(*color, *gradient, ui_paints),
                None,
                None,
                UiValueBinding::ConstantQ12(0),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                psx_level::UI_OPTION_NONE,
                ui_node_flags(rect.anchor, UiTextAlign::Left, false),
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Label {
                rect,
                text,
                random_message,
                messages,
                tag,
                align,
                wrap,
                font,
                font_scale,
                letter_spacing,
                color,
                gradient,
                effect: _,
            } => {
                let candidates = messages
                    .iter()
                    .map(String::as_str)
                    .filter(|message| !message.trim().is_empty())
                    .collect::<Vec<_>>();
                let use_random = *random_message && !candidates.is_empty();
                let cooked_text = if use_random {
                    candidates.join("\u{1f}")
                } else {
                    text.clone()
                };
                let mut flags = ui_node_flags(rect.anchor, *align, *wrap);
                if use_random {
                    flags |= psx_level::ui_node_flags::TEXT_RANDOM_MESSAGE;
                }
                (
                    rect.x,
                    rect.y,
                    rect.width.max(1),
                    rect.height.max(1),
                    *color,
                    [0, 0, 0],
                    [0, 0, 0],
                    cook_ui_paint(*color, *gradient, ui_paints),
                    None,
                    None,
                    UiValueBinding::ConstantQ12(0),
                    UiValueBinding::ConstantQ12(0),
                    None,
                    cooked_text,
                    tag.clone(),
                    PlaytestUiAction::default(),
                    psx_level::UI_OPTION_NONE,
                    flags,
                    font.runtime_index(),
                    clamp_ui_font_scale(*font_scale),
                    (*letter_spacing).clamp(MIN_UI_LETTER_SPACING, MAX_UI_LETTER_SPACING),
                )
            }
            UiNodeKind::Image { .. } => unreachable!("image nodes are handled above"),
            UiNodeKind::Bar {
                rect,
                value,
                max,
                fill,
                fill_gradient,
                background,
                background_gradient,
            } => (
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                *fill,
                *background,
                [0, 0, 0],
                cook_ui_paint(*fill, *fill_gradient, ui_paints),
                cook_ui_paint(*background, *background_gradient, ui_paints),
                None,
                *value,
                *max,
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                psx_level::UI_OPTION_NONE,
                ui_node_flags(rect.anchor, UiTextAlign::Left, false),
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Button {
                rect,
                label,
                align,
                font,
                font_scale,
                letter_spacing,
                color,
                background_gradient,
                text_color,
                text_gradient,
                transparent,
                action,
                ..
            } => (
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                *color,
                [0, 0, 0],
                *text_color,
                cook_ui_paint(*color, *background_gradient, ui_paints),
                None,
                cook_ui_paint(*text_color, *text_gradient, ui_paints),
                UiValueBinding::ConstantQ12(0),
                UiValueBinding::ConstantQ12(0),
                None,
                label.clone(),
                String::new(),
                cook_ui_action(*action),
                psx_level::UI_OPTION_NONE,
                ui_node_flags(rect.anchor, *align, false)
                    | if *transparent {
                        psx_level::ui_node_flags::BUTTON_TRANSPARENT
                    } else {
                        0
                    },
                font.runtime_index(),
                clamp_ui_font_scale(*font_scale),
                (*letter_spacing).clamp(MIN_UI_LETTER_SPACING, MAX_UI_LETTER_SPACING),
            ),
            UiNodeKind::Slider {
                rect,
                option,
                track,
                track_gradient,
                fill,
                fill_gradient,
                knob,
                knob_gradient,
                ..
            } => (
                rect.x,
                rect.y,
                rect.width.max(1),
                rect.height.max(1),
                *track,
                *fill,
                *knob,
                cook_ui_paint(*track, *track_gradient, ui_paints),
                cook_ui_paint(*fill, *fill_gradient, ui_paints),
                cook_ui_paint(*knob, *knob_gradient, ui_paints),
                UiValueBinding::ConstantQ12(0),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                cook_option_id(*option),
                ui_node_flags(rect.anchor, UiTextAlign::Left, false),
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Music {
                wav_path,
                volume,
                volume_option,
                playback_speed_q12,
                loop_track,
            } => (
                0,
                0,
                1,
                1,
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                None,
                None,
                None,
                volume_option
                    .map(UiValueBinding::Option)
                    .unwrap_or_else(|| UiValueBinding::ConstantQ12(i32::from((*volume).min(100)))),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                PlaytestUiAction::default(),
                cook_cdda_track_number(
                    wav_path,
                    *playback_speed_q12,
                    project_root,
                    cdda_tracks,
                    cdda_track_for_wav,
                    report,
                ),
                if *loop_track {
                    psx_level::ui_node_flags::MUSIC_LOOP
                } else {
                    0
                },
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
            UiNodeKind::Timer {
                millis,
                skippable,
                action,
            } => (
                0,
                0,
                1,
                1,
                [0, 0, 0],
                [0, 0, 0],
                [0, 0, 0],
                None,
                None,
                None,
                // Delay in vblank ticks (NTSC 60 Hz), the clock the runtime
                // timer pass counts in.
                UiValueBinding::ConstantQ12((millis.saturating_mul(60) / 1000).max(1) as i32),
                UiValueBinding::ConstantQ12(0),
                None,
                String::new(),
                String::new(),
                cook_ui_action(*action),
                psx_level::UI_OPTION_NONE,
                if *skippable {
                    psx_level::ui_node_flags::TIMER_SKIPPABLE
                } else {
                    0
                },
                0,
                default_ui_font_scale(),
                default_ui_letter_spacing(),
            ),
        };
        let (sfx_first, sfx_count) = cook_ui_node_sfx(
            &node.kind,
            project_root,
            ui_sfx_samples,
            ui_sfx_cues,
            ui_sfx_sample_for_wav,
            report,
        );
        let visual_rect = node.kind.rect();
        let rotation_degrees = visual_rect.map(|rect| rect.rotation_degrees).unwrap_or(0);
        let flags = visual_rect
            .map(|rect| flags | ui_rect_transform_flags(rect))
            .unwrap_or(flags)
            | ui_visibility_flags(node.visible_when);
        out.push(PlaytestUiNode {
            parent,
            kind: node.kind.clone(),
            x,
            y,
            width,
            height,
            color,
            background,
            accent,
            color_paint,
            background_paint,
            accent_paint,
            value,
            max,
            texture_asset,
            // Labels carry their sheen through; everything else non-image
            // stays static.
            image_effect: match &node.kind {
                UiNodeKind::Label { effect, .. } => *effect,
                _ => UiImageEffect::None,
            },
            text,
            tag,
            action,
            option,
            rotation_degrees,
            flags,
            sfx_first,
            sfx_count,
            font,
            font_scale,
            letter_spacing,
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_ui_image_node(
    name: &str,
    parent: Option<u16>,
    rect: &UiRect,
    texture: Option<ResourceId>,
    tint: [u8; 3],
    effect: UiImageEffect,
    project: &ProjectDocument,
    project_root: &Path,
    ui_image_texture_for_path: &mut HashMap<String, CookedUiImageTexture>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
    out: &mut Vec<PlaytestUiNode>,
) {
    let Some(texture_id) = texture else {
        out.push(cooked_ui_image_node(parent, *rect, tint, effect, None));
        return;
    };
    let Some(cooked) = cook_ui_image_texture_asset(
        project,
        project_root,
        texture_id,
        &format!("UI image '{name}'"),
        ui_image_texture_for_path,
        assets,
        report,
    ) else {
        out.push(cooked_ui_image_node(parent, *rect, tint, effect, None));
        return;
    };
    if cooked.fragments.len() <= 1 {
        out.push(cooked_ui_image_node(
            parent,
            *rect,
            tint,
            effect,
            cooked
                .fragments
                .first()
                .map(|fragment| fragment.asset_index),
        ));
        return;
    }

    let group_index = out.len().min(u16::MAX as usize) as u16;
    out.push(cooked_ui_group_node(parent, *rect));

    let mut screen_x = 0u16;
    for (index, fragment) in cooked.fragments.iter().enumerate() {
        let remaining = rect.width.max(1).saturating_sub(screen_x);
        let screen_w = if index + 1 == cooked.fragments.len() {
            remaining.max(1)
        } else {
            scale_ui_fragment_width(rect.width.max(1), fragment.width, cooked.width)
                .min(remaining)
                .max(1)
        };
        let child_rect = UiRect::new(
            screen_x.min(i16::MAX as u16) as i16,
            0,
            screen_w,
            rect.height.max(1),
        );
        out.push(cooked_ui_image_node(
            Some(group_index),
            child_rect,
            tint,
            effect,
            Some(fragment.asset_index),
        ));
        screen_x = screen_x.saturating_add(screen_w);
    }
}

pub(crate) fn scale_ui_fragment_width(
    screen_width: u16,
    fragment_width: u16,
    texture_width: u16,
) -> u16 {
    if texture_width == 0 {
        return screen_width.max(1);
    }
    ((u32::from(screen_width) * u32::from(fragment_width)) / u32::from(texture_width))
        .min(u32::from(u16::MAX)) as u16
}

pub(crate) fn cook_ui_paint(
    from: [u8; 3],
    gradient: Option<UiGradient>,
    ui_paints: &mut Vec<PlaytestUiPaint>,
) -> Option<u16> {
    let gradient = gradient?;
    if gradient.to == from {
        return None;
    }
    let paint = PlaytestUiPaint {
        from,
        to: gradient.to,
        direction: gradient.direction,
    };
    if let Some(index) = ui_paints.iter().position(|candidate| *candidate == paint) {
        return Some(index.min(u16::MAX as usize) as u16);
    }
    let index = ui_paints.len().min(u16::MAX as usize) as u16;
    ui_paints.push(paint);
    Some(index)
}

pub(crate) fn cooked_ui_group_node(parent: Option<u16>, rect: UiRect) -> PlaytestUiNode {
    PlaytestUiNode {
        parent,
        kind: UiNodeKind::Group { rect },
        x: rect.x,
        y: rect.y,
        width: rect.width.max(1),
        height: rect.height.max(1),
        color: [0, 0, 0],
        background: [0, 0, 0],
        accent: [0, 0, 0],
        color_paint: None,
        background_paint: None,
        accent_paint: None,
        value: UiValueBinding::ConstantQ12(0),
        max: UiValueBinding::ConstantQ12(0),
        texture_asset: None,
        image_effect: UiImageEffect::None,
        text: String::new(),
        tag: String::new(),
        action: PlaytestUiAction::default(),
        option: psx_level::UI_OPTION_NONE,
        rotation_degrees: rect.rotation_degrees,
        flags: ui_node_flags(rect.anchor, UiTextAlign::Left, false) | ui_rect_transform_flags(rect),
        sfx_first: psx_level::UI_SFX_NONE,
        sfx_count: 0,
        font: 0,
        font_scale: default_ui_font_scale(),
        letter_spacing: default_ui_letter_spacing(),
    }
}

pub(crate) fn cooked_ui_image_node(
    parent: Option<u16>,
    rect: UiRect,
    tint: [u8; 3],
    effect: UiImageEffect,
    texture_asset: Option<usize>,
) -> PlaytestUiNode {
    PlaytestUiNode {
        parent,
        kind: UiNodeKind::Image {
            rect,
            texture: None,
            tint,
            effect,
        },
        x: rect.x,
        y: rect.y,
        width: rect.width.max(1),
        height: rect.height.max(1),
        color: tint,
        background: [0, 0, 0],
        accent: [0, 0, 0],
        color_paint: None,
        background_paint: None,
        accent_paint: None,
        value: UiValueBinding::ConstantQ12(0),
        max: UiValueBinding::ConstantQ12(0),
        texture_asset,
        image_effect: effect,
        text: String::new(),
        tag: String::new(),
        action: PlaytestUiAction::default(),
        option: psx_level::UI_OPTION_NONE,
        rotation_degrees: rect.rotation_degrees,
        flags: ui_node_flags(rect.anchor, UiTextAlign::Left, false) | ui_rect_transform_flags(rect),
        sfx_first: psx_level::UI_SFX_NONE,
        sfx_count: 0,
        font: 0,
        font_scale: default_ui_font_scale(),
        letter_spacing: default_ui_letter_spacing(),
    }
}

pub(crate) fn cook_ui_image_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    texture_id: ResourceId,
    context: &str,
    ui_image_texture_for_path: &mut HashMap<String, CookedUiImageTexture>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<CookedUiImageTexture> {
    let Some(texture_resource) = find_resource(project, texture_id) else {
        report.warn(format!(
            "{context}: texture resource #{} is missing; using placeholder",
            texture_id.raw()
        ));
        return None;
    };
    let (texture_key, bytes) = match material_texture_bytes(project, texture_resource, project_root)
    {
        Ok(Some(source)) => source,
        Ok(None) => {
            report.warn(format!(
                "{context}: material '{}' has no texture; using placeholder",
                texture_resource.name
            ));
            return None;
        }
        Err(msg) => {
            report.warn(format!("{context}: {msg}; using placeholder"));
            return None;
        }
    };
    if let Some(existing) = ui_image_texture_for_path.get(&texture_key).cloned() {
        return Some(existing);
    }
    let texture = match psx_asset::Texture::from_bytes(&bytes) {
        Ok(texture) => texture,
        Err(error) => {
            report.warn(format!(
                "{context}: texture '{}' parse failed: {error:?}; using placeholder",
                texture_resource.name
            ));
            return None;
        }
    };
    if texture.depth() != TextureDepth::Bit4 || texture.clut_entries() != 16 {
        report.warn(format!(
            "{context}: texture '{}' must be 4bpp with one 16-colour CLUT for UI images; using placeholder",
            texture_resource.name
        ));
        return None;
    }
    if texture.width() == 0
        || texture.height() == 0
        || texture.height() > UI_LARGE_IMAGE_MAX_DIMENSION
    {
        report.warn(format!(
            "{context}: texture '{}' has unsupported UI dimensions {}x{}; using placeholder",
            texture_resource.name,
            texture.width(),
            texture.height()
        ));
        return None;
    }

    let fragment_count = ui_image_fragment_count(texture.width());
    let nominal_width = texture.width().div_ceil(fragment_count);
    let mut fragments = Vec::with_capacity(fragment_count as usize);
    let mut source_x = 0u16;
    let mut fragment_index = 0u16;
    while source_x < texture.width() {
        let width = nominal_width.min(texture.width() - source_x).max(1);
        let fragment_bytes = if source_x == 0 && width == texture.width() {
            bytes.clone()
        } else {
            encode_ui_image_fragment_psxt(&texture, source_x, width)?
        };
        let asset_index = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes: fragment_bytes,
            filename: format!("ui_image_{:04}_{fragment_index:02}.psxt", texture_id.raw()),
            source_label: if fragment_count == 1 {
                texture_resource.name.clone()
            } else {
                format!(
                    "{} strip {} of {}",
                    texture_resource.name,
                    fragment_index + 1,
                    fragment_count
                )
            },
            // UI image textures are CD-streamed: empty baked bytes plus
            // a UI.PAK payload loaded on demand on menu entry.
            streamed_class: StreamedClass::UiImage,
        });
        fragments.push(CookedUiImageFragment { asset_index, width });
        source_x = source_x.saturating_add(width);
        fragment_index = fragment_index.saturating_add(1);
    }

    let cooked = CookedUiImageTexture {
        width: texture.width(),
        fragments,
    };
    ui_image_texture_for_path.insert(texture_key, cooked.clone());
    Some(cooked)
}

pub(crate) fn ui_image_fragment_count(width: u16) -> u16 {
    if width <= UI_LARGE_IMAGE_MAX_DIMENSION {
        1
    } else {
        width.div_ceil(UI_LARGE_IMAGE_STRIP_WIDTH).max(1)
    }
}

pub(crate) fn encode_ui_image_fragment_psxt(
    texture: &psx_asset::Texture<'_>,
    source_x: u16,
    width: u16,
) -> Option<Vec<u8>> {
    let height = texture.height();
    let src_width = texture.width();
    let src_hw_per_row = TextureHeader::halfwords_per_row(TextureDepth::Bit4, src_width);
    let dst_hw_per_row = TextureHeader::halfwords_per_row(TextureDepth::Bit4, width);
    let mut dst_halfwords = vec![0u16; usize::from(dst_hw_per_row) * usize::from(height)];

    let src_pixels = texture.pixel_bytes();
    let mut y = 0u16;
    while y < height {
        let mut x = 0u16;
        while x < width {
            let source_pixel_x = source_x.checked_add(x)?;
            let src_hw_index = usize::from(y)
                .checked_mul(usize::from(src_hw_per_row))?
                .checked_add(usize::from(source_pixel_x / 4))?;
            let src_byte = src_hw_index.checked_mul(2)?;
            let source_hw = u16::from_le_bytes([
                *src_pixels.get(src_byte)?,
                *src_pixels.get(src_byte.checked_add(1)?)?,
            ]);
            let palette_index = (source_hw >> ((source_pixel_x & 3) * 4)) & 0x000f;

            let dst_hw_index = usize::from(y)
                .checked_mul(usize::from(dst_hw_per_row))?
                .checked_add(usize::from(x / 4))?;
            dst_halfwords[dst_hw_index] |= palette_index << ((x & 3) * 4);
            x = x.saturating_add(1);
        }
        y = y.saturating_add(1);
    }

    Some(assemble_ui_image_psxt(
        width,
        height,
        texture.flags(),
        &dst_halfwords,
        texture.clut_bytes(),
    ))
}

pub(crate) fn assemble_ui_image_psxt(
    width: u16,
    height: u16,
    flags: u16,
    pixel_hw: &[u16],
    clut_bytes: &[u8],
) -> Vec<u8> {
    let pixel_bytes = (pixel_hw.len() * 2) as u32;
    let clut_len = clut_bytes.len() as u32;
    let payload_len = TextureHeader::SIZE as u32 + pixel_bytes + clut_len;
    let mut out = Vec::with_capacity(AssetHeader::SIZE + payload_len as usize);
    out.extend_from_slice(&TEXTURE_MAGIC);
    out.extend_from_slice(&TEXTURE_VERSION.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.push(TextureDepth::Bit4 as u8);
    out.push(0);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(&pixel_bytes.to_le_bytes());
    out.extend_from_slice(&clut_len.to_le_bytes());
    for hw in pixel_hw {
        out.extend_from_slice(&hw.to_le_bytes());
    }
    out.extend_from_slice(clut_bytes);
    out
}

pub(crate) fn cook_cdda_track_number(
    wav_path: &str,
    playback_speed_q12: u16,
    project_root: &Path,
    cdda_tracks: &mut Vec<PlaytestCddaTrack>,
    cdda_track_for_wav: &mut HashMap<String, u8>,
    report: &mut PlaytestValidationReport,
) -> u16 {
    let trimmed = wav_path.trim();
    if trimmed.is_empty() {
        return psx_level::UI_OPTION_NONE;
    }
    if !trimmed
        .rsplit_once('.')
        .map(|(_, ext)| ext.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        report.warn(format!(
            "CD-DA music source '{trimmed}' is not a .wav file and was skipped"
        ));
        return psx_level::UI_OPTION_NONE;
    }
    let abs = resolve_path(trimmed, project_root);
    if !abs.is_file() {
        report.warn(format!(
            "CD-DA music source '{}' does not exist and was skipped",
            abs.display()
        ));
        return psx_level::UI_OPTION_NONE;
    }
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    let speed = playback_speed_q12.max(1);
    let key = format!("{}#{speed}", abs.display());
    if let Some(track) = cdda_track_for_wav.get(&key) {
        return u16::from(*track);
    }
    if cdda_tracks.len() >= 98 {
        report.warn("CD-DA music track limit reached; extra music sources were skipped");
        return psx_level::UI_OPTION_NONE;
    }
    let track = (cdda_tracks.len() + 2) as u8;
    cdda_tracks.push(PlaytestCddaTrack {
        track,
        wav_path: abs.to_string_lossy().into_owned(),
        playback_speed_q12: speed,
    });
    cdda_track_for_wav.insert(key, track);
    u16::from(track)
}

pub(crate) fn cook_ui_node_sfx(
    kind: &UiNodeKind,
    project_root: &Path,
    samples: &mut Vec<PlaytestUiSfxSample>,
    cues: &mut Vec<PlaytestUiSfxCue>,
    sample_for_wav: &mut HashMap<String, u16>,
    report: &mut PlaytestValidationReport,
) -> (u16, u8) {
    let first = cues.len().min(u16::MAX as usize) as u16;
    match kind {
        UiNodeKind::Button { sfx, .. } => {
            cook_ui_sfx_event(
                &sfx.focus,
                psx_level::LevelUiSfxEvent::Focus,
                project_root,
                samples,
                cues,
                sample_for_wav,
                report,
            );
            cook_ui_sfx_event(
                &sfx.activate,
                psx_level::LevelUiSfxEvent::Activate,
                project_root,
                samples,
                cues,
                sample_for_wav,
                report,
            );
        }
        UiNodeKind::Slider { sfx, .. } => {
            cook_ui_sfx_event(
                &sfx.focus,
                psx_level::LevelUiSfxEvent::Focus,
                project_root,
                samples,
                cues,
                sample_for_wav,
                report,
            );
            cook_ui_sfx_event(
                &sfx.nudge,
                psx_level::LevelUiSfxEvent::SliderNudge,
                project_root,
                samples,
                cues,
                sample_for_wav,
                report,
            );
            cook_ui_sfx_event(
                &sfx.limit,
                psx_level::LevelUiSfxEvent::SliderLimit,
                project_root,
                samples,
                cues,
                sample_for_wav,
                report,
            );
        }
        _ => {}
    }
    let count = cues.len().saturating_sub(first as usize);
    if count == 0 {
        (psx_level::UI_SFX_NONE, 0)
    } else {
        (first, count.min(u8::MAX as usize) as u8)
    }
}

pub(crate) fn cook_ui_sfx_event(
    authored: &[UiSfxCue],
    event: psx_level::LevelUiSfxEvent,
    project_root: &Path,
    samples: &mut Vec<PlaytestUiSfxSample>,
    cues: &mut Vec<PlaytestUiSfxCue>,
    sample_for_wav: &mut HashMap<String, u16>,
    report: &mut PlaytestValidationReport,
) {
    for cue in authored {
        let Some(sample) =
            cook_ui_sfx_sample_index(&cue.wav_path, project_root, samples, sample_for_wav, report)
        else {
            continue;
        };
        cues.push(PlaytestUiSfxCue {
            sample,
            event,
            volume_percent: cue.volume.min(100),
            pitch_q12: cue.pitch_q12.clamp(1, 0x3FFF),
            flags: 0,
        });
    }
}

pub(crate) fn cook_ui_sfx_sample_index(
    wav_path: &str,
    project_root: &Path,
    samples: &mut Vec<PlaytestUiSfxSample>,
    sample_for_wav: &mut HashMap<String, u16>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    let trimmed = wav_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .rsplit_once('.')
        .map(|(_, ext)| ext.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        report.warn(format!(
            "UI SFX source '{trimmed}' is not a .wav file and was skipped"
        ));
        return None;
    }
    let abs = resolve_path(trimmed, project_root);
    if !abs.is_file() {
        report.warn(format!(
            "UI SFX source '{}' does not exist and was skipped",
            abs.display()
        ));
        return None;
    }
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    let key = abs.to_string_lossy().into_owned();
    if let Some(index) = sample_for_wav.get(&key) {
        return Some(*index);
    }
    if samples.len() >= u16::MAX as usize {
        report.warn("UI SFX sample limit reached; extra SFX sources were skipped");
        return None;
    }
    let bytes = match std::fs::read(&abs) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.warn(format!("UI SFX source '{}': {error}", abs.display()));
            return None;
        }
    };
    let psau = match psxed_audio::cook_sfx_from_wav(&bytes) {
        Ok(psau) => psau,
        Err(error) => {
            report.warn(format!(
                "UI SFX source '{}' could not be cooked: {error}",
                abs.display()
            ));
            return None;
        }
    };
    let index = samples.len() as u16;
    samples.push(PlaytestUiSfxSample {
        bytes: psau,
        filename: format!("ui_sfx_{index:03}.psau"),
        source_path: key.clone(),
    });
    sample_for_wav.insert(key, index);
    Some(index)
}

pub(crate) fn ui_node_flags(anchor: UiAnchor, align: UiTextAlign, wrap: bool) -> u16 {
    let mut flags = anchor.runtime_bits() & psx_level::ui_node_flags::ANCHOR_MASK;
    flags |= (align.runtime_bits() << psx_level::ui_node_flags::TEXT_ALIGN_SHIFT)
        & psx_level::ui_node_flags::TEXT_ALIGN_MASK;
    if wrap {
        flags |= psx_level::ui_node_flags::TEXT_WRAP;
    }
    flags
}

pub(crate) fn ui_rect_transform_flags(rect: UiRect) -> u16 {
    let mut flags = 0;
    if rect.flip_x {
        flags |= psx_level::ui_node_flags::FLIP_X;
    }
    if rect.flip_y {
        flags |= psx_level::ui_node_flags::FLIP_Y;
    }
    flags
}

pub(crate) fn ui_visibility_flags(condition: UiVisibilityCondition) -> u16 {
    match condition {
        UiVisibilityCondition::Always => 0,
        UiVisibilityCondition::AnalogInactive => psx_level::ui_node_flags::ANALOG_INACTIVE_ONLY,
        UiVisibilityCondition::LoadingComplete => psx_level::ui_node_flags::LOADING_COMPLETE_ONLY,
    }
}

/// Lower an authored [`UiAction`] to a cooked [`PlaytestUiAction`].
/// `GotoScene` resolves the target [`crate::UiSceneId`] to a cooked
/// scene id by taking its low 16 bits, matching how `cook_ui_nodes`
/// assigns each [`PlaytestUiScene::id`].
pub(crate) fn cook_ui_action(action: UiAction) -> PlaytestUiAction {
    match action {
        UiAction::GotoState(state) => PlaytestUiAction::GotoState {
            state: cook_scene_state_id(state),
        },
        UiAction::TransitionToState { state, transition } => PlaytestUiAction::TransitionToState {
            state: cook_scene_state_id(state),
            transition: cook_ui_transition(transition),
        },
        UiAction::GotoScene(scene) => PlaytestUiAction::GotoScene {
            scene: (scene.raw() & u16::MAX as u64) as u16,
        },
        UiAction::TransitionToScene { scene, transition } => PlaytestUiAction::TransitionToScene {
            scene: (scene.raw() & u16::MAX as u64) as u16,
            transition: cook_ui_transition(transition),
        },
        UiAction::StartGameplay => PlaytestUiAction::StartGameplay,
        UiAction::StartGameplayTransition { transition } => {
            PlaytestUiAction::StartGameplayTransition {
                transition: cook_ui_transition(transition),
            }
        }
        UiAction::Back => PlaytestUiAction::Back,
        UiAction::SetOption { option, delta } => PlaytestUiAction::SetOption {
            option: cook_option_id(option),
            delta,
        },
        UiAction::Game(id) => PlaytestUiAction::Game { id },
    }
}

pub(crate) fn cook_ui_transition(transition: UiTransition) -> PlaytestTransition {
    let kind = match transition.kind {
        UiTransitionKind::None => PlaytestTransitionKind::None,
        UiTransitionKind::Fade => PlaytestTransitionKind::Fade,
        UiTransitionKind::BlockDissolve => PlaytestTransitionKind::BlockDissolve,
        UiTransitionKind::GlitchBreak => PlaytestTransitionKind::GlitchBreak,
    };
    PlaytestTransition {
        kind,
        frames: transition.frames,
        color: transition.color,
        seed: transition.seed,
    }
}

/// Pack an authored [`OptionId`] into the runtime's compact `u16`
/// slot, clamping to the low 16 bits.
pub(crate) fn cook_option_id(option: OptionId) -> u16 {
    (option.raw() & u16::MAX as u32) as u16
}

/// Flatten every authored [`OptionDef`] into a cooked [`PlaytestOption`]
/// the runtime store can seed from. Each [`OptionKind`] collapses to a
/// bounded integer triple: an `IntRange` maps directly, an `Enum` becomes
/// `[0, variants - 1]` step `1` (an empty variant list yields a degenerate
/// `[0, 0]`), and a `Bool` becomes `[0, 1]` step `1`. The id is the same
/// low-16-bit packing sliders and `SetOption` actions cook to, so a slider
/// or button resolves its bound option by matching ids at runtime.
pub(crate) fn cook_options(project: &ProjectDocument) -> Vec<PlaytestOption> {
    project
        .options
        .iter()
        .map(|option| {
            let (min, max, step, default) = match &option.kind {
                OptionKind::IntRange {
                    min,
                    max,
                    step,
                    default,
                } => (*min, *max, *step, *default),
                OptionKind::Enum { variants, default } => {
                    let last = variants.len().saturating_sub(1) as i32;
                    (0, last, 1, *default as i32)
                }
                OptionKind::Bool { default } => (0, 1, 1, *default as i32),
            };
            PlaytestOption {
                id: cook_option_id(option.id),
                min,
                max,
                step,
                default,
            }
        })
        .collect()
}
