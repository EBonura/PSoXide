//! Write the combat verb coverage of every Character in some projects as
//! Markdown: for each Animation Set, the kits it gives its Characters and the
//! verbs those kits expect that it does not bind, with the stand-in that
//! plays meanwhile. It is the authoring to-do list for missing clips.
//!
//! ```text
//! cargo run -p psxed-project --example verb_coverage -- <project-dir>... > verb-coverage.md
//! ```
//!
//! Read-only: it loads each `project.ron` and changes nothing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use psx_level::combat_verbs::{self, CombatKit, CombatKits, VerbBindings, VerbCoverage};
use psxed_project::verbs::CharacterVerbBindings;
use psxed_project::{CharacterAnimationAction, ProjectDocument, ResourceData};

/// One Animation Set as its Characters use it.
struct SetCoverage {
    characters: Vec<String>,
    kits: CombatKits,
    shoot: CharacterAnimationAction,
    bound: usize,
    /// Per kit, the verbs missing and what plays instead.
    gaps: Vec<(CombatKit, Vec<String>)>,
}

fn label(action: psx_level::CharacterAnimationAction) -> &'static str {
    CharacterAnimationAction::from_level(action).label()
}

fn coverage(bindings: &impl VerbBindings) -> (CombatKits, Vec<(CombatKit, Vec<String>)>) {
    let kits = CombatKits::from_bindings(bindings);
    let mut gaps: Vec<(CombatKit, Vec<String>)> = Vec::new();
    combat_verbs::for_each_gap(bindings, |verb, cover| {
        let text = match cover {
            VerbCoverage::Fallback(found) => {
                format!("{} (plays {} meanwhile)", label(verb.action), label(found))
            }
            _ => format!("{} (no stand-in)", label(verb.action)),
        };
        match gaps.iter_mut().find(|(kit, _)| *kit == verb.kit) {
            Some((_, list)) => list.push(text),
            None => gaps.push((verb.kit, vec![text])),
        }
    });
    (kits, gaps)
}

fn kits_line(kits: CombatKits, shoot: CharacterAnimationAction) -> String {
    let mut has = vec!["core".to_string()];
    let mut lacks = Vec::new();
    for (kit, name) in [
        (CombatKit::Melee, "melee"),
        (CombatKit::Ranged, "ranged"),
        (CombatKit::Swap, "swap"),
        (CombatKit::Mobility, "mobility"),
    ] {
        if kits.has(kit) {
            if kit == CombatKit::Ranged && shoot != CharacterAnimationAction::RangedAttack {
                has.push(format!("ranged (shot authored on {})", shoot.label()));
            } else {
                has.push(name.to_string());
            }
        } else {
            lacks.push(format!("no {name}"));
        }
    }
    let mut line = has.join(", ");
    if !lacks.is_empty() {
        line.push_str("; ");
        line.push_str(&lacks.join(", "));
    }
    line
}

fn project_sets(project: &ProjectDocument) -> BTreeMap<String, SetCoverage> {
    let mut sets: BTreeMap<String, SetCoverage> = BTreeMap::new();
    for resource in &project.resources {
        let ResourceData::Character(character) = &resource.data else {
            continue;
        };
        let bindings = CharacterVerbBindings::new(project, character);
        let Some(set_id) = character.animation_set else {
            continue;
        };
        let set_name = project
            .resource(set_id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("#{}", set_id.raw()));
        let (kits, gaps) = coverage(&bindings);
        let shoot = character.shoot_action();
        // Characters on one set share an entry unless they shoot from
        // different actions (the shot is a Character property).
        let key = format!("{set_name}\u{0}{shoot:?}");
        let bound = CharacterAnimationAction::ALL
            .iter()
            .filter(|action| bindings.bound(action.to_level()))
            .count();
        sets.entry(key)
            .or_insert_with(|| SetCoverage {
                characters: Vec::new(),
                kits,
                shoot,
                bound,
                gaps,
            })
            .characters
            .push(resource.name.clone());
    }
    sets
}

fn main() {
    let dirs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        eprintln!("usage: verb_coverage <project-dir>...");
        std::process::exit(2);
    }
    let mut out = String::new();
    // Identical sets across projects are written once, with the projects
    // that carry them.
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut bodies: BTreeMap<String, (String, String)> = BTreeMap::new();
    for dir in &dirs {
        let project_name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let project = match ProjectDocument::load_from_path(dir.join("project.ron")) {
            Ok(project) => project,
            Err(error) => {
                eprintln!("{}: {error}", dir.display());
                std::process::exit(1);
            }
        };
        for (key, set) in project_sets(&project) {
            let set_name = key.split('\u{0}').next().unwrap_or_default();
            let mut body = String::new();
            let _ = writeln!(
                body,
                "Characters: {}. Verbs bound: {} of {} contract verbs \
                 ({} action slots bound in all).\n\nKits: {}.\n",
                set.characters.join(", "),
                combat_verbs::COMBAT_VERBS
                    .iter()
                    .filter(|verb| !verb.optional)
                    .filter(|verb| set.kits.has(verb.kit))
                    .count()
                    - set.gaps.iter().map(|(_, list)| list.len()).sum::<usize>(),
                combat_verbs::COMBAT_VERBS
                    .iter()
                    .filter(|verb| !verb.optional)
                    .filter(|verb| set.kits.has(verb.kit))
                    .count(),
                set.bound,
                kits_line(set.kits, set.shoot),
            );
            if set.gaps.is_empty() {
                let _ = writeln!(body, "Nothing missing in the kits it has.\n");
            }
            for (kit, list) in &set.gaps {
                let _ = writeln!(body, "- {} kit missing: {}", kit.label(), list.join(", "));
            }
            let _ = writeln!(body);
            let signature = format!("{key}\u{0}{body}");
            seen.entry(signature.clone())
                .or_default()
                .push(project_name.clone());
            bodies.insert(signature, (set_name.to_string(), body));
        }
    }
    let _ = writeln!(
        out,
        "# Combat verb coverage\n\n\
         Generated by `cargo run -p psxed-project --example verb_coverage -- projects/*`. \
         For each Animation Set and the Characters on it: the kits they have and the \
         contract verbs those kits expect that the set does not bind. A kit the \
         Character does not have at all is not listed verb by verb. The stand-in is \
         the documented fallback that plays until the clip exists. The Zenith melee \
         strikes are optional and never listed.\n"
    );
    for (signature, projects) in &seen {
        let (set_name, body) = &bodies[signature];
        let _ = writeln!(out, "## {set_name}\n\nProjects: {}.\n", projects.join(", "));
        out.push_str(body);
    }
    print!("{out}");
}
