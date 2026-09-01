//! Collapse Character resources that differ only by what they carry into one
//! Character with named loadouts.
//!
//! ```sh
//! cargo run -p psxed-project --bin merge_character_loadouts -- <project.ron> \
//!     --keep "Light Enemy / Artigli" \
//!     --fold "Light Enemy / Light Weapon=Light Weapon" \
//!     --fold "Light Enemy / Heavy Weapon=Heavy Weapon"
//! # add --write to apply; without it this only reports
//! ```
//!
//! Characters are named rather than numbered because resource ids are an
//! implementation detail of `project.ron` and the names are what the Studio
//! shows.
//!
//! The project modelled "Light Enemy / Artigli", "/ Light Weapon" and
//! "/ Heavy Weapon" as three whole Characters sharing a model, a skeleton and
//! an animation set. Combat capsules authored on one did not reach the others,
//! so the Studio offered three near-identical entries and only one of them had
//! the hitboxes. Folding them turns the difference back into what it is: a
//! per-placement choice of equipment.
//!
//! Every scene placement of a folded Character is repointed at the kept one
//! with the matching loadout index, so the level keeps spawning what it spawned.
//! The kept Character's own capsules, behaviour and tuning win outright, which
//! is why `--keep` should name the one that was actually authored.

use std::collections::HashMap;
use std::path::PathBuf;

use psxed_project::{CharacterLoadout, NodeKind, ProjectDocument, ResourceData, ResourceId};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect(
        "usage: merge_character_loadouts <project.ron> --keep <name> --fold <name>=<loadout>",
    ));
    let (mut keep_name, mut folds, mut write) = (None, Vec::new(), false);
    let mut name_the_default = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--keep" => keep_name = Some(args.next().expect("--keep <character name>")),
            "--name-default" => {
                name_the_default = Some(args.next().expect("--name-default <loadout name>"));
            }
            "--fold" => {
                let spec = args.next().expect("--fold <character name>=<loadout name>");
                let (character, loadout) = spec
                    .split_once('=')
                    .expect("--fold <character name>=<loadout name>");
                folds.push((character.to_string(), loadout.to_string()));
            }
            "--write" => write = true,
            other => panic!("unknown flag {other}"),
        }
    }
    let keep_name = keep_name.expect("--keep <character name>");

    let mut project = ProjectDocument::load_from_path(&path).expect("load project");

    let character_named = |project: &ProjectDocument, wanted: &str| -> ResourceId {
        let mut found = project.resources.iter().filter(|resource| {
            resource.name == wanted && matches!(resource.data, ResourceData::Character(_))
        });
        let first = found
            .next()
            .unwrap_or_else(|| panic!("no Character named {wanted:?}"));
        assert!(
            found.next().is_none(),
            "more than one Character named {wanted:?}; names must be unique to fold by name",
        );
        first.id
    };
    let name_of = |project: &ProjectDocument, id: ResourceId| {
        project
            .resource(id)
            .map_or_else(|| "(missing)".to_string(), |resource| resource.name.clone())
    };
    let keep = character_named(&project, &keep_name);

    // Read every folded Character's equipment out before mutating anything:
    // the kept Character is itself a candidate for being read here.
    let mut appended: Vec<(ResourceId, u16)> = Vec::new();
    let (mut loadouts, kept_equipment) = match &project.resource(keep).expect("kept character").data
    {
        ResourceData::Character(character) => (
            character.loadouts.clone(),
            character.default_equipment.clone(),
        ),
        _ => panic!("--keep is not a Character"),
    };
    // The kept Character's own equipment is a loadout like any other. Naming
    // it means every placement reads as a deliberate choice instead of the
    // picker showing a bare "Default" for the form that was actually authored.
    if let Some(name) = &name_the_default {
        assert!(
            loadouts.is_empty(),
            "--name-default only makes sense before any loadout exists",
        );
        println!(
            "name default {:<28} -> loadout 0 {name:?} ({} binding(s))",
            "",
            kept_equipment.len()
        );
        loadouts.push(CharacterLoadout {
            name: name.clone(),
            equipment: kept_equipment,
        });
    }
    for (character_name, name) in &folds {
        let id = character_named(&project, character_name);
        assert_ne!(id, keep, "--fold {character_name:?} is also --keep");
        let ResourceData::Character(character) =
            &project.resource(id).expect("folded character").data
        else {
            unreachable!("character_named filters on Character");
        };
        println!(
            "fold {:>4} {:<28} -> loadout {} {:?} ({} binding(s))",
            id.raw(),
            character_name,
            loadouts.len(),
            name,
            character.default_equipment.len(),
        );
        appended.push((id, loadouts.len() as u16));
        loadouts.push(CharacterLoadout {
            name: name.clone(),
            equipment: character.default_equipment.clone(),
        });
    }

    let index: HashMap<ResourceId, u16> = appended.iter().copied().collect();

    // Repoint placements. A folded Character that something still references
    // after this would be silently dropped by the delete below, so count.
    let mut repointed = 0usize;
    for scene in &mut project.scenes {
        let controllers: Vec<_> = scene
            .nodes()
            .iter()
            .filter(|node| {
                matches!(&node.kind, NodeKind::CharacterController { character: Some(id), .. }
                    if index.contains_key(id))
            })
            .map(|node| node.id)
            .collect();
        for node_id in controllers {
            let Some(node) = scene.node_mut(node_id) else {
                continue;
            };
            if let NodeKind::CharacterController {
                character: Some(id),
                loadout,
                ..
            } = &mut node.kind
            {
                if let Some(selected) = index.get(id) {
                    *id = keep;
                    *loadout = Some(*selected);
                    repointed += 1;
                }
            }
        }
    }

    match &mut project.resource_mut(keep).expect("kept character").data {
        ResourceData::Character(character) => character.loadouts = loadouts,
        _ => unreachable!("checked above"),
    }

    let folded: Vec<ResourceId> = appended.iter().map(|(id, _)| *id).collect();
    let mut still_referenced = Vec::new();
    for id in &folded {
        let count = project.resource_reference_count(*id);
        if count == 0 {
            project.resources.retain(|resource| resource.id != *id);
        } else {
            still_referenced.push((*id, count));
        }
    }

    println!(
        "\nkeep {} {:?}: {} loadout(s), {repointed} placement(s) repointed, {} resource(s) removed",
        keep.raw(),
        name_of(&project, keep),
        match &project.resource(keep).expect("kept character").data {
            ResourceData::Character(character) => character.loadouts.len(),
            _ => 0,
        },
        folded.len() - still_referenced.len(),
    );
    for (id, count) in &still_referenced {
        println!(
            "  kept {}: still referenced {count} time(s), not removed",
            id.raw()
        );
    }

    if write {
        project.save_to_path(&path).expect("save project");
        println!("wrote {}", path.display());
    } else {
        println!("dry run; pass --write to apply");
    }
}
