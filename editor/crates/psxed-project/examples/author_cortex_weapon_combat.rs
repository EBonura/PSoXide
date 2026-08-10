//! Deterministically author the `cortex_v1` weapon/combat convergence data.
//!
//! The reference project supplies joint names only after exact skeleton
//! compatibility checks. Geometry and attack timing are then authored into
//! the target project as runtime-cooked truth; no editor-only sidecar is used.
//!
//! Usage:
//!   cargo run -p psxed-project --example author_cortex_weapon_combat -- \
//!       <cortex_v1/project.ron> <cortex_anim/project.ron>

use std::path::{Path, PathBuf};

use psxed_project::{
    AttachmentSocket, CharacterAnimationAction, CharacterCombatCapsule, CombatCapsuleRole,
    JointCapsule, ProjectDocument, ResourceData, ResourceId, WeaponHitShape, WeaponHitbox,
};

const PLAYER_MODEL: &str = "Aletha Delivered";
const PLAYER_CHARACTER: &str = "Aletha";
const ENEMY_MODEL: &str = "Rust Mantis";
const ENEMY_CHARACTER: &str = "Rust Mantis Enemy";
const RIGHT_HAND_SOCKET: &str = "right_hand_grip";

fn migrated_grip_offset(
    socket_offset: i32,
    character_scale_q12: u16,
    visual_scale_q8: u16,
    weapon_scale_q12: u16,
) -> i32 {
    let effective_character_scale =
        (u32::from(character_scale_q12) * u32::from(visual_scale_q8) + 128) / 256;
    let numerator = i64::from(socket_offset) * i64::from(effective_character_scale);
    let denominator = i64::from(weapon_scale_q12.max(1));
    ((numerator + denominator / 2) / denominator) as i32
}

const PLAYER_TORSO: CharacterCombatCapsule = CharacterCombatCapsule {
    name: String::new(),
    joint: 8,
    capsule: JointCapsule {
        start: [0, -12_000, 0],
        end: [0, 12_000, 0],
        radius: 180,
    },
    role: CombatCapsuleRole::Hurtbox,
};

const ENEMY_TORSO: CharacterCombatCapsule = CharacterCombatCapsule {
    name: String::new(),
    joint: 3,
    capsule: JointCapsule {
        // The placed Mantis renders at visual_scale_q8=512. Its effective
        // 160/4096 pose scale turns this 16k segment plus radius into a
        // 993-unit receiving volume, matching its 1024-unit body.
        start: [0, -8_000, 0],
        end: [0, 8_000, 0],
        radius: 184,
    },
    role: CombatCapsuleRole::Hurtbox,
};

fn model_id(project: &ProjectDocument, name: &str) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| resource.name == name && matches!(resource.data, ResourceData::Model(_)))
        .map(|resource| resource.id)
        .unwrap_or_else(|| panic!("missing Model resource {name:?}"))
}

fn character_id(project: &ProjectDocument, name: &str) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| {
            resource.name == name && matches!(resource.data, ResourceData::Character(_))
        })
        .map(|resource| resource.id)
        .unwrap_or_else(|| panic!("missing Character resource {name:?}"))
}

fn skeleton_id(project: &ProjectDocument, model: ResourceId) -> ResourceId {
    let ResourceData::Model(model) = &project.resource(model).expect("model resource").data else {
        unreachable!()
    };
    model.skeleton.expect("model skeleton")
}

fn transfer_joint_names(
    target: &mut ProjectDocument,
    reference: &ProjectDocument,
    model_name: &str,
) {
    let target_skeleton = skeleton_id(target, model_id(target, model_name));
    let reference_skeleton = skeleton_id(reference, model_id(reference, model_name));
    let ResourceData::Skeleton(reference_data) = &reference
        .resource(reference_skeleton)
        .expect("reference skeleton")
        .data
    else {
        unreachable!()
    };
    assert_eq!(
        reference_data.joint_names.len(),
        usize::from(reference_data.joint_count),
        "reference {model_name} has incomplete joint names"
    );
    let reference_names = reference_data.joint_names.clone();
    let reference_count = reference_data.joint_count;
    let reference_parents = reference_data.parents.clone();
    let reference_signature = reference_data.signature.clone();

    let ResourceData::Skeleton(target_data) = &mut target
        .resource_mut(target_skeleton)
        .expect("target skeleton")
        .data
    else {
        unreachable!()
    };
    assert_eq!(target_data.joint_count, reference_count);
    assert_eq!(target_data.parents, reference_parents);
    assert_eq!(target_data.signature, reference_signature);
    if target_data.joint_names != reference_names {
        target_data.joint_names = reference_names;
        println!("{model_name}: transferred compatible joint names");
    }
}

fn named_joint(project: &ProjectDocument, model: ResourceId, leaf_name: &str) -> u16 {
    let skeleton = skeleton_id(project, model);
    let ResourceData::Skeleton(skeleton) = &project.resource(skeleton).unwrap().data else {
        unreachable!()
    };
    skeleton
        .joint_names
        .iter()
        .position(|name| {
            name.rsplit([':', '|', '/'])
                .next()
                .is_some_and(|leaf| leaf.eq_ignore_ascii_case(leaf_name))
        })
        .and_then(|index| u16::try_from(index).ok())
        .unwrap_or_else(|| panic!("{leaf_name:?} absent from model skeleton"))
}

fn author_hand_socket(project: &mut ProjectDocument, model_name: &str) {
    let id = model_id(project, model_name);
    let joint = named_joint(project, id, "RightHand");
    let ResourceData::Model(model) = &mut project.resource_mut(id).unwrap().data else {
        unreachable!()
    };
    let socket = AttachmentSocket {
        name: RIGHT_HAND_SOCKET.to_string(),
        joint,
        // The character socket is the anatomical hand. Art-axis and pivot
        // correction belong to each Weapon grip, whose model scale differs.
        translation: [0, 0, 0],
        rotation_q12: [0, 0, 0],
    };
    match model
        .attachments
        .iter_mut()
        .find(|attachment| attachment.name == RIGHT_HAND_SOCKET)
    {
        Some(existing) => *existing = socket,
        None => model.attachments.push(socket),
    }
    println!("{model_name}: {RIGHT_HAND_SOCKET} -> named joint {joint}");
}

fn attack_capsule(
    name: &str,
    joint: u16,
    action: CharacterAnimationAction,
    active_start_frame: u16,
    active_end_frame: u16,
    damage: u16,
    poise_damage: u16,
) -> CharacterCombatCapsule {
    CharacterCombatCapsule {
        name: name.to_string(),
        joint,
        capsule: JointCapsule {
            // Aletha's 70/4096 model scale and visual_scale_q8=360 produce
            // an effective 98/4096, so this follows the visible blade over
            // about 598 world units beyond the grip.
            start: [0, -5_000, 0],
            end: [0, -30_000, 0],
            radius: 72,
        },
        role: CombatCapsuleRole::Hitbox {
            action,
            active_start_frame,
            active_end_frame,
            damage,
            poise_damage,
        },
    }
}

fn upsert_capsule(volumes: &mut Vec<CharacterCombatCapsule>, volume: CharacterCombatCapsule) {
    match volumes
        .iter_mut()
        .find(|existing| existing.name == volume.name)
    {
        Some(existing) => *existing = volume,
        None => volumes.push(volume),
    }
}

fn author_combat_capsules(project: &mut ProjectDocument) {
    let player_model = model_id(project, PLAYER_MODEL);
    let player_hand = named_joint(project, player_model, "RightHand");
    assert_eq!(player_hand, 13, "Aletha Delivered hand index changed");
    let player = character_id(project, PLAYER_CHARACTER);
    let ResourceData::Character(player) = &mut project.resource_mut(player).unwrap().data else {
        unreachable!()
    };
    let mut torso = PLAYER_TORSO.clone();
    torso.name = "Torso Hurtbox".to_string();
    upsert_capsule(&mut player.combat_capsules, torso);
    // Inclusive windows come from the real 12 Hz clip samples: Light A's
    // hand travels through frames 3-6, Heavy through 6-10, Light B through
    // 2-6. Windup/recovery frames remain non-damaging.
    upsert_capsule(
        &mut player.combat_capsules,
        attack_capsule(
            "Light Sword Active",
            player_hand,
            CharacterAnimationAction::LightAttack,
            3,
            6,
            25,
            25,
        ),
    );
    upsert_capsule(
        &mut player.combat_capsules,
        attack_capsule(
            "Heavy Sword Active",
            player_hand,
            CharacterAnimationAction::HeavyAttack,
            6,
            10,
            38,
            50,
        ),
    );
    upsert_capsule(
        &mut player.combat_capsules,
        attack_capsule(
            "Combo Sword Active",
            player_hand,
            CharacterAnimationAction::ComboAttack,
            2,
            6,
            30,
            30,
        ),
    );

    let enemy = character_id(project, ENEMY_CHARACTER);
    let ResourceData::Character(enemy) = &mut project.resource_mut(enemy).unwrap().data else {
        unreachable!()
    };
    let mut torso = ENEMY_TORSO.clone();
    torso.name = "Torso Hurtbox".to_string();
    upsert_capsule(&mut enemy.combat_capsules, torso);
    // No Mantis hitbox is fabricated here: its current animation set has no
    // attack clip, so there is no authoritative frame clock to window yet.
}

fn author_weapon_data(project: &mut ProjectDocument) {
    let Some(light_id) = project
        .resources
        .iter()
        .find(|resource| {
            resource.name == "Sword1 Light" && matches!(resource.data, ResourceData::Weapon(_))
        })
        .map(|resource| resource.id)
    else {
        println!("Sword1 Light not imported yet; weapon grip pass deferred");
        return;
    };
    let ResourceData::Weapon(light) = &mut project.resource_mut(light_id).unwrap().data else {
        unreachable!()
    };
    // This preserves the proven visual placement of the old -6000 Aletha
    // socket, but expresses it in the weapon's own scale. Aletha's authored
    // visual_scale_q8=360 turns 70/4096 into 98/4096, while the sword uses
    // 39/4096: 6000 * 98 / 39 = 15077. Art-axis correction belongs here too.
    light.grip.translation = [0, migrated_grip_offset(6_000, 70, 360, 39), 0];
    light.grip.rotation_q12 = [0, -1024, 0];
    let blade = WeaponHitbox {
        name: "Light Blade Active".to_string(),
        shape: WeaponHitShape::Capsule {
            start: [0, -5_000, 0],
            end: [0, -30_000, 0],
            radius: 72,
        },
        active_start_frame: 3,
        active_end_frame: 6,
    };
    match light
        .hitboxes
        .iter_mut()
        .find(|hitbox| hitbox.name == blade.name || **hitbox == WeaponHitbox::default())
    {
        Some(existing) => *existing = blade,
        None => light.hitboxes.push(blade),
    }

    if let Some(heavy_id) = project
        .resources
        .iter()
        .find(|resource| {
            resource.name == "Sword1 Heavy" && matches!(resource.data, ResourceData::Weapon(_))
        })
        .map(|resource| resource.id)
    {
        let ResourceData::Weapon(heavy) = &mut project.resource_mut(heavy_id).unwrap().data else {
            unreachable!()
        };
        // Preserve the old placement across the Mantis instance's
        // visual_scale_q8=512 (effective 160/4096) and the heavy sword's
        // 52/4096 scale: 6000 * 160 / 52 = 18462. Do not invent an active
        // window without the Mantis' missing attack clip.
        heavy.grip.translation = [0, migrated_grip_offset(6_000, 80, 512, 52), 0];
        heavy.grip.rotation_q12 = [0, -1024, 0];
        heavy
            .hitboxes
            .retain(|hitbox| *hitbox != WeaponHitbox::default());
    }
}

fn stand_mantis_upright(project: &mut ProjectDocument) {
    for scene in &mut project.scenes {
        let ids = scene
            .nodes()
            .iter()
            .filter(|node| node.name == "Mantis Enemy")
            .map(|node| node.id)
            .collect::<Vec<_>>();
        for id in ids {
            if let Some(node) = scene.node_mut(id) {
                node.transform.rotation_degrees[0] = 0.0;
            }
        }
    }
}

fn assert_action_window(
    project: &ProjectDocument,
    root: &Path,
    action: CharacterAnimationAction,
    end_frame: u16,
) {
    let character = project
        .resource(character_id(project, PLAYER_CHARACTER))
        .unwrap();
    let ResourceData::Character(character) = &character.data else {
        unreachable!()
    };
    let set = project.resource(character.animation_set.unwrap()).unwrap();
    let ResourceData::AnimationSet(set) = &set.data else {
        unreachable!()
    };
    let clip_id = set
        .action_clips
        .iter()
        .find(|binding| binding.action == action)
        .map(|binding| binding.clip)
        .unwrap_or_else(|| panic!("player has no {action:?} clip"));
    let clip = project.resource(clip_id).unwrap();
    let ResourceData::AnimationClip(clip) = &clip.data else {
        unreachable!()
    };
    let bytes = std::fs::read(root.join(&clip.psxanim_path)).expect("read action clip");
    let animation = psx_asset::Animation::from_bytes(&bytes).expect("parse action clip");
    assert!(
        end_frame < animation.frame_count(),
        "{action:?} window ends at {end_frame}, clip has {} frames",
        animation.frame_count()
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: author_cortex_weapon_combat <target-project.ron> <reference-project.ron>";
    let target_path = PathBuf::from(args.next().expect(usage));
    let reference_path = PathBuf::from(args.next().expect(usage));
    assert!(args.next().is_none(), "{usage}");
    let root = target_path.parent().expect("target project root");
    let backup = root
        .join("logs")
        .join("project.ron.pre-weapon-combat-authority.bak");
    if !backup.exists() {
        std::fs::create_dir_all(backup.parent().unwrap()).expect("logs dir");
        std::fs::copy(&target_path, &backup).expect("backup target project");
        println!("backup: {}", backup.display());
    }

    let mut target = ProjectDocument::load_from_path(&target_path).expect("load target project");
    let reference =
        ProjectDocument::load_from_path(&reference_path).expect("load reference project");
    transfer_joint_names(&mut target, &reference, PLAYER_MODEL);
    transfer_joint_names(&mut target, &reference, ENEMY_MODEL);
    author_hand_socket(&mut target, PLAYER_MODEL);
    author_hand_socket(&mut target, ENEMY_MODEL);
    author_combat_capsules(&mut target);
    author_weapon_data(&mut target);
    stand_mantis_upright(&mut target);

    assert_action_window(&target, root, CharacterAnimationAction::LightAttack, 6);
    assert_action_window(&target, root, CharacterAnimationAction::HeavyAttack, 10);
    assert_action_window(&target, root, CharacterAnimationAction::ComboAttack, 6);
    target
        .save_to_path(&target_path)
        .expect("save target project");
    println!("saved {}", target_path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grip_migration_preserves_old_world_offsets_across_model_scales() {
        assert_eq!(migrated_grip_offset(6_000, 70, 360, 39), 15_077);
        assert_eq!(migrated_grip_offset(6_000, 80, 512, 52), 18_462);
    }
}
