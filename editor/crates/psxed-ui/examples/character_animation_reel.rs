//! Render every cooked clip associated with the three default-project actors.
//!
//! The output is a directory of numbered PNG-frame folders plus `manifest.tsv`.
//! A video tool can label and concatenate the folders without reimplementing
//! the editor's cooked-model, material-layer, socket, or grip math.
//!
//! Usage:
//!   cargo run -p psxed-ui --example character_animation_reel -- \
//!     editor/projects/default/project.ron /tmp/psoxide-character-reel \
//!     [character-name [clip-name ...]]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use egui::{Color32, ColorImage};
use image::{ImageBuffer, Rgba};
use psx_asset::{Animation, Texture};
use psxed_project::{MaterialAnimationMode, ProjectDocument, Resource, ResourceData, ResourceId};
use psxed_ui::model_import_preview::{
    render_import_model_preview_with_equipment_banks, ImportPreviewOptions,
    PreviewEquippedWeapon, PreviewMaterialLayer,
};

const OUTPUT_FPS: u16 = 30;
const MIN_LOOP_SECONDS: f64 = 2.0;
const MIN_ONE_SHOT_SECONDS: f64 = 1.0;
const FINAL_HOLD_SECONDS: f64 = 0.35;

struct CharacterPreview<'a> {
    name: &'a str,
    model_id: ResourceId,
    model: &'a psxed_project::ModelResource,
    model_bytes: Vec<u8>,
    atlas_banks: Vec<ColorImage>,
    material_atlas: Option<ColorImage>,
    material_motion: psx_level::LevelMaterialUvMotion,
}

struct WeaponPreview {
    model_bytes: Vec<u8>,
    atlas: ColorImage,
    socket_joint: u16,
    socket_translation: [i32; 3],
    socket_rotation_q12: [i16; 3],
    grip_translation: [i32; 3],
    grip_rotation_q12: [i16; 3],
    persistent: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: character_animation_reel <project.ron> <frame-output-dir> \
                 [character-name [clip-name ...]]";
    let project_path = PathBuf::from(args.next().expect(usage));
    let out_root = PathBuf::from(args.next().expect(usage));
    let character_filter = args.next();
    let clip_filters: Vec<String> = args.collect();
    let project_root = project_path.parent().expect("project path has a parent");
    let project = ProjectDocument::load_from_path(&project_path).expect("load project");
    std::fs::create_dir_all(&out_root).expect("create frame output directory");

    let mut manifest = String::from(
        "index\tcharacter\tclip\trole\tlooping\tstored_frames\tsample_rate\tdisplay_seconds\tframe_dir\n",
    );
    let mut segment_index = 0usize;

    let character_names = project
        .resources
        .iter()
        .filter(|resource| matches!(resource.data, ResourceData::Character(_)))
        .map(|resource| resource.name.as_str())
        .collect::<Vec<_>>();
    for character_name in character_names {
        if character_filter
            .as_deref()
            .is_some_and(|filter| !character_name.eq_ignore_ascii_case(filter))
        {
            continue;
        }
        let preview = load_character_preview(&project, project_root, character_name);
        let weapon = if character_name == "Aletha" {
            load_light_weapon_preview(&project, project_root, &preview)
        } else {
            load_default_weapon_preview(&project, project_root, &preview)
        };
        let mut clips: Vec<&Resource> = project
            .resources
            .iter()
            .filter(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => {
                    clip.target_model == Some(preview.model_id)
                        || (clip.target_model.is_none()
                            && clip.skeleton.is_some()
                            && clip.skeleton == preview.model.skeleton)
                }
                _ => false,
            })
            .collect();
        if !clip_filters.is_empty() {
            clips.retain(|resource| {
                clip_filters
                    .iter()
                    .any(|filter| resource.name.eq_ignore_ascii_case(filter))
            });
        }
        clips.sort_by(|a, b| a.name.cmp(&b.name));

        for resource in clips {
            let ResourceData::AnimationClip(clip) = &resource.data else {
                unreachable!();
            };
            let clip_path = resolve(project_root, &clip.psxanim_path);
            let clip_bytes = std::fs::read(&clip_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", clip_path.display()));
            let animation = Animation::from_bytes(&clip_bytes).expect("parse cooked animation");
            let cycle_seconds = f64::from(animation.frame_count().saturating_sub(1))
                / f64::from(animation.sample_rate_hz());
            let display_seconds = if clip.looping {
                cycle_seconds.max(MIN_LOOP_SECONDS)
            } else {
                cycle_seconds.max(MIN_ONE_SHOT_SECONDS) + FINAL_HOLD_SECONDS
            };
            let directory_name = format!(
                "{:03}_{}_{}",
                segment_index,
                slug(preview.name),
                slug(&resource.name)
            );
            let frame_dir = out_root.join(&directory_name);
            std::fs::create_dir_all(&frame_dir).expect("create clip frame directory");
            render_clip(
                &preview,
                &resource.name,
                clip,
                &clip_bytes,
                animation,
                display_seconds,
                &frame_dir,
                weapon.as_ref(),
            );
            writeln!(
                manifest,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}",
                segment_index,
                preview.name,
                resource.name,
                clip.role.label(),
                clip.looping,
                animation.frame_count(),
                animation.sample_rate_hz(),
                display_seconds,
                directory_name,
            )
            .unwrap();
            println!(
                "[{segment_index:03}] {} / {} ({:.2}s)",
                preview.name, resource.name, display_seconds
            );
            segment_index += 1;
        }
    }

    std::fs::write(out_root.join("manifest.tsv"), manifest).expect("write reel manifest");
    println!("rendered {segment_index} animation segments");
}

#[allow(clippy::too_many_arguments)]
fn render_clip(
    preview: &CharacterPreview<'_>,
    clip_name: &str,
    clip: &psxed_project::AnimationClipResource,
    clip_bytes: &[u8],
    animation: Animation<'_>,
    display_seconds: f64,
    frame_dir: &Path,
    light_weapon: Option<&WeaponPreview>,
) {
    let preview_yaw_q12 = std::env::var("PSXED_REEL_YAW_Q12")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(700);
    let preview_pitch_q12 = std::env::var("PSXED_REEL_PITCH_Q12")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(200);
    let frame_total = (display_seconds * f64::from(OUTPUT_FPS)).ceil() as usize;
    let cycle_seconds = f64::from(animation.frame_count().saturating_sub(1))
        / f64::from(animation.sample_rate_hz());
    let final_pose_time = f64::from(animation.frame_count().saturating_sub(1))
        / f64::from(animation.sample_rate_hz())
        - 0.5 / f64::from(animation.sample_rate_hz());
    let material_layer = preview
        .material_atlas
        .as_ref()
        .map(|atlas| PreviewMaterialLayer {
            atlas,
            motion: preview.material_motion,
        });

    for frame in 0..frame_total {
        let display_time = frame as f64 / f64::from(OUTPUT_FPS);
        let sample_time = if clip.looping || display_time < cycle_seconds {
            display_time
        } else {
            final_pose_time.max(0.0)
        };
        let weapon_overlay = light_weapon.and_then(|weapon| {
            let materialization = if weapon.persistent {
                Some(4096)
            } else {
                runtime_light_weapon_materialization(clip_name, animation, sample_time)
            };
            materialization.map(
                |materialization_q12| PreviewEquippedWeapon {
                    model_bytes: &weapon.model_bytes,
                    atlas_banks: std::slice::from_ref(&weapon.atlas),
                    socket_joint: weapon.socket_joint,
                    socket_translation: weapon.socket_translation,
                    socket_rotation_q12: weapon.socket_rotation_q12,
                    grip_translation: weapon.grip_translation,
                    grip_rotation_q12: weapon.grip_rotation_q12,
                    materialization_q12,
                    wireframe_materialization: !weapon.persistent,
                    show_grip_gizmo: false,
                },
            )
        });
        let options = ImportPreviewOptions {
            world_height: i32::from(preview.model.world_height),
            visual_scale_q8: preview.model.scale_q8[1].max(1),
            visual_yaw_q12: preview.model.default_visual_yaw_q12,
            collision_radius: i32::from(preview.model.collision_radius),
            time_seconds: sample_time,
            yaw_q12: preview_yaw_q12,
            pitch_q12: preview_pitch_q12,
            radius: 0,
            focus_on_animated_bounds: true,
            preview_in_place: clip.calibration.in_place,
            pose_offset: clip.calibration.offset,
            show_animation_root: false,
            show_collision_guides: false,
            show_bones: false,
        };
        let image = render_import_model_preview_with_equipment_banks(
            &preview.model_bytes,
            clip_bytes,
            &preview.atlas_banks,
            options,
            weapon_overlay.as_ref(),
            material_layer.as_ref(),
        )
        .unwrap_or_else(|| panic!("render {} / {clip_name}", preview.name));
        save_png(&image, &frame_dir.join(format!("frame_{frame:05}.png")));
    }
}

/// The first mapped player light attack owns the exact runtime weapon beat:
/// eight cooked frames of growth into frame 25, then the mirrored dissolve at
/// the end. Other clips remain body-only so the reel does not invent equipment
/// behavior that gameplay never requests.
fn runtime_light_weapon_materialization(
    clip_name: &str,
    animation: Animation<'_>,
    time_seconds: f64,
) -> Option<u16> {
    if clip_name != "gen_light_attack" {
        return None;
    }
    const THROW_FRAME: u32 = 25;
    const MATERIALISE_FRAMES: u32 = 8;
    let phase_q12 = (time_seconds.max(0.0) * f64::from(animation.sample_rate_hz()) * 4096.0) as u32;
    let open_q12 = THROW_FRAME
        .saturating_mul(4096)
        .saturating_sub(MATERIALISE_FRAMES << 12);
    let until_q12 = u32::from(animation.frame_count().saturating_sub(1)).saturating_mul(4096);
    if phase_q12 < open_q12 || phase_q12 >= until_q12 {
        return Some(0);
    }
    let rising = (phase_q12 - open_q12) / MATERIALISE_FRAMES;
    let falling = (until_q12 - phase_q12) / MATERIALISE_FRAMES;
    Some(rising.min(falling).min(4096) as u16)
}

fn load_character_preview<'a>(
    project: &'a ProjectDocument,
    project_root: &Path,
    name: &'a str,
) -> CharacterPreview<'a> {
    let character_resource = find_resource(project, name, |data| {
        matches!(data, ResourceData::Character(_))
    });
    let ResourceData::Character(character) = &character_resource.data else {
        unreachable!();
    };
    let model_id = character.model.expect("character has a model");
    let model_resource = project.resource(model_id).expect("model resource exists");
    let ResourceData::Model(model) = &model_resource.data else {
        panic!("character model id is not a Model");
    };
    let model_bytes = std::fs::read(resolve(project_root, &model.model_path)).expect("read model");
    let atlas_path = model.texture_path.as_deref().expect("model has an atlas");
    let atlas_banks = decode_atlas_banks(
        &std::fs::read(resolve(project_root, atlas_path)).expect("read model atlas"),
    );
    let (material_atlas, material_motion) = character
        .material
        .and_then(|material_id| {
            let resource = project.resource(material_id)?;
            let ResourceData::Material(material) = &resource.data else {
                return None;
            };
            let (_, bytes) =
                psxed_project::resolve_material_texture_psxt(project, material_id, project_root)
                    .ok()??;
            let motion = if material.animation.mode == MaterialAnimationMode::UvScroll {
                let scroll = material.animation.uv_scroll;
                psx_level::LevelMaterialUvMotion {
                    enabled: scroll.enabled,
                    speed_u_q8: scroll.speed_u_q8,
                    speed_v_q8: scroll.speed_v_q8,
                    phase_u: scroll.phase_u,
                    phase_v: scroll.phase_v,
                }
            } else {
                psx_level::LevelMaterialUvMotion::default()
            };
            Some((decode_atlas(&bytes), motion))
        })
        .map_or(
            (None, psx_level::LevelMaterialUvMotion::default()),
            |(atlas, motion)| (Some(atlas), motion),
        );

    CharacterPreview {
        name,
        model_id,
        model,
        model_bytes,
        atlas_banks,
        material_atlas,
        material_motion,
    }
}

fn load_light_weapon_preview(
    project: &ProjectDocument,
    project_root: &Path,
    character: &CharacterPreview<'_>,
) -> Option<WeaponPreview> {
    let weapon_resource = find_resource(project, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let ResourceData::Weapon(weapon) = &weapon_resource.data else {
        return None;
    };
    let weapon_model_id = weapon.model?;
    let ResourceData::Model(weapon_model) = &project.resource(weapon_model_id)?.data else {
        return None;
    };
    let socket = character
        .model
        .attachments
        .iter()
        .find(|socket| socket.name == weapon.default_character_socket)?;
    let atlas_path = weapon_model.texture_path.as_deref()?;
    Some(WeaponPreview {
        model_bytes: std::fs::read(resolve(project_root, &weapon_model.model_path)).ok()?,
        atlas: decode_atlas(&std::fs::read(resolve(project_root, atlas_path)).ok()?),
        socket_joint: socket.joint,
        socket_translation: socket.translation,
        socket_rotation_q12: socket.rotation_q12,
        grip_translation: weapon.grip.translation,
        grip_rotation_q12: weapon.grip.rotation_q12,
        persistent: false,
    })
}

fn load_default_weapon_preview(
    project: &ProjectDocument,
    project_root: &Path,
    character: &CharacterPreview<'_>,
) -> Option<WeaponPreview> {
    let character_resource = find_resource(project, character.name, |data| {
        matches!(data, ResourceData::Character(_))
    });
    let ResourceData::Character(character_profile) = &character_resource.data else {
        return None;
    };
    let binding = character_profile
        .default_equipment
        .iter()
        .find(|binding| binding.weapon.is_some())?;
    let weapon_id = binding.weapon?;
    let ResourceData::Weapon(weapon) = &project.resource(weapon_id)?.data else {
        return None;
    };
    let weapon_model_id = weapon.model?;
    let ResourceData::Model(weapon_model) = &project.resource(weapon_model_id)?.data else {
        return None;
    };
    let socket = character
        .model
        .attachments
        .iter()
        .find(|socket| socket.name == binding.character_socket)?;
    let parsed_model = psx_asset::Model::from_bytes(&character.model_bytes).ok()?;
    let atlas_path = weapon_model.texture_path.as_deref()?;
    Some(WeaponPreview {
        model_bytes: std::fs::read(resolve(project_root, &weapon_model.model_path)).ok()?,
        atlas: decode_atlas(&std::fs::read(resolve(project_root, atlas_path)).ok()?),
        socket_joint: socket.joint,
        socket_translation: psxed_project::model_import::attachment_socket_bind_translation(
            &parsed_model,
            socket,
        ),
        socket_rotation_q12: socket.rotation_q12,
        grip_translation: weapon.grip.translation,
        grip_rotation_q12: weapon.grip.rotation_q12,
        persistent: true,
    })
}

fn find_resource<'a>(
    project: &'a ProjectDocument,
    name: &str,
    matches_kind: impl Fn(&ResourceData) -> bool,
) -> &'a Resource {
    project
        .resources
        .iter()
        .find(|resource| resource.name == name && matches_kind(&resource.data))
        .unwrap_or_else(|| panic!("missing resource {name:?}"))
}

fn resolve(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn decode_atlas(bytes: &[u8]) -> ColorImage {
    decode_atlas_banks(bytes)
        .into_iter()
        .next()
        .expect("texture has at least one palette bank")
}

fn decode_atlas_banks(bytes: &[u8]) -> Vec<ColorImage> {
    let texture = Texture::from_bytes(bytes).expect("parse psxt");
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let pixels = texture.pixel_bytes();
    let clut = texture.clut_bytes();
    let row_bytes = usize::from(texture.halfwords_per_row()) * 2;
    let entries_per_bank = match texture.depth() as u8 {
        4 => 16,
        8 => 256,
        15 => 0,
        _ => unreachable!("Texture rejects unsupported depths"),
    };
    let bank_count = if entries_per_bank == 0 {
        1
    } else {
        (usize::from(texture.clut_entries()) / entries_per_bank).max(1)
    };
    (0..bank_count)
        .map(|bank| {
            let mut decoded = vec![Color32::BLACK; width * height];
            for y in 0..height {
                for x in 0..width {
                    let raw = match texture.depth() as u8 {
                4 => {
                    let packed = pixels[y * row_bytes + x / 2];
                    let index = if x & 1 == 0 {
                        packed & 0x0f
                    } else {
                        packed >> 4
                    };
                    let offset = (bank * entries_per_bank + usize::from(index)) * 2;
                    u16::from_le_bytes([clut[offset], clut[offset + 1]])
                }
                8 => {
                    let offset =
                        (bank * entries_per_bank + usize::from(pixels[y * row_bytes + x])) * 2;
                    u16::from_le_bytes([clut[offset], clut[offset + 1]])
                }
                15 => {
                    let offset = y * row_bytes + x * 2;
                    u16::from_le_bytes([pixels[offset], pixels[offset + 1]])
                }
                _ => unreachable!("Texture rejects unsupported depths"),
            };
                    decoded[y * width + x] = Color32::from_rgb(
                        ((raw & 31) * 255 / 31) as u8,
                        (((raw >> 5) & 31) * 255 / 31) as u8,
                        (((raw >> 10) & 31) * 255 / 31) as u8,
                    );
                }
            }
            ColorImage {
                size: [width, height],
                pixels: decoded,
            }
        })
        .collect()
}

fn save_png(image: &ColorImage, path: &Path) {
    let [width, height] = image.size;
    let mut rgba = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width as u32, height as u32);
    for (index, pixel) in image.pixels.iter().enumerate() {
        rgba.put_pixel(
            (index % width) as u32,
            (index / width) as u32,
            Rgba([pixel.r(), pixel.g(), pixel.b(), pixel.a()]),
        );
    }
    rgba.save(path).expect("save reel frame");
}
