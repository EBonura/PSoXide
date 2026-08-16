//! Scan (and with --apply, prune) resources in a project that nothing
//! reachable from its scenes/UI references. Run from editor/:
//!   cargo run -p psxed-project --bin prune_starter_resources -- [--project <dir>] [--list] [--apply]
//!
//! Targeted mode (no reachability pass, for catalogue projects like default
//! whose resources are meant to be unplaced): delete by cooked asset path
//! prefix and/or by exact resource name, plus the files they own.
//!   cargo run -p psxed-project --bin prune_starter_resources -- --project <dir> \
//!       --delete-asset-prefix assets/models/old_model/ --delete-name "Old Set" [--apply]

use psxed_project::{ProjectDocument, ResourceData, ResourceId};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn seed_ids_from_ron(ron: &str, out: &mut HashSet<u64>) {
    // Field-name-qualified id references in serialized scene/UI data.
    // Covers every NodeKind variant, grid faces, wall segments and
    // triangle overrides without enumerating them.
    let fields = [
        "mesh",
        "material",
        "materials",
        "model",
        "character",
        "weapon",
        "texture",
    ];
    for field in fields {
        let needle = format!("{field}:");
        let mut rest = ron;
        while let Some(pos) = rest.find(&needle) {
            rest = &rest[pos + needle.len()..];
            if field == "materials" {
                // BoxProp stores one optional material per face in a tuple.
                // Limit scanning to that balanced tuple so later fields are
                // never attributed to it accidentally.
                let Some(open) = rest.find('(') else { continue };
                let tuple = &rest[open..];
                let mut depth = 0usize;
                let mut end = tuple.len();
                for (index, ch) in tuple.char_indices() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = index + 1;
                            break;
                        }
                    }
                }
                let mut values = &tuple[..end];
                while let Some(value_pos) = values.find("Some((") {
                    values = &values[value_pos + 6..];
                    let digits: String =
                        values.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(id) = digits.parse::<u64>() {
                        out.insert(id);
                    }
                }
            } else if let Some(value) = rest.trim_start().strip_prefix("Some(") {
                // Ids are newtype structs, so RON prints Some((42)).
                let digits: String = value
                    .chars()
                    .skip_while(|c| *c == '(' || c.is_whitespace())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(id) = digits.parse::<u64>() {
                    out.insert(id);
                }
            }
        }
    }
}

fn keep_id(keep: &mut HashSet<u64>, id: Option<ResourceId>) {
    if let Some(id) = id {
        keep.insert(id.raw());
    }
}

fn main() {
    let apply = std::env::args().any(|a| a == "--apply");
    let project_dir = std::env::args()
        .skip_while(|a| a != "--project")
        .nth(1)
        .unwrap_or_else(|| "projects/default".to_string());
    let mut project =
        ProjectDocument::load_from_path(std::path::Path::new(&project_dir).join("project.ron"))
            .expect("load project");
    project.normalize_loaded();

    let flag_values = |flag: &str| -> Vec<String> {
        let args: Vec<String> = std::env::args().collect();
        args.iter()
            .enumerate()
            .filter(|(_, a)| a.as_str() == flag)
            .filter_map(|(i, _)| args.get(i + 1).cloned())
            .collect()
    };
    let delete_prefixes = flag_values("--delete-asset-prefix");
    let delete_names = flag_values("--delete-name");
    if !delete_prefixes.is_empty() || !delete_names.is_empty() {
        delete_targeted(
            &mut project,
            &PathBuf::from(&project_dir),
            &delete_prefixes,
            &delete_names,
            apply,
        );
        return;
    }

    let mut keep: HashSet<u64> = HashSet::new();
    for scene in &project.scenes {
        let ron = ron::ser::to_string_pretty(scene, ron::ser::PrettyConfig::default())
            .expect("scene serializes");
        seed_ids_from_ron(&ron, &mut keep);
    }
    for ui in &project.ui_scenes {
        let ron = ron::ser::to_string_pretty(ui, ron::ser::PrettyConfig::default())
            .expect("ui scene serializes");
        seed_ids_from_ron(&ron, &mut keep);
    }

    // Resource-to-resource closure.
    loop {
        let before = keep.len();
        for r in &project.resources {
            if !keep.contains(&r.id.raw()) {
                continue;
            }
            match &r.data {
                ResourceData::Model(m) => keep_id(&mut keep, m.skeleton),
                ResourceData::AnimationSource(a) => {
                    keep_id(&mut keep, a.skeleton);
                    keep_id(&mut keep, a.target_model);
                }
                ResourceData::AnimationClip(c) => {
                    keep_id(&mut keep, c.skeleton);
                    keep_id(&mut keep, c.target_model);
                    keep_id(&mut keep, c.source);
                }
                // Skeleton-scoped clips are how a model finds its clips at
                // cook time (Model has no clip list), so a clip on a kept
                // skeleton is reachable even when no set binds it.
                ResourceData::Skeleton(_) => {
                    let skeleton = r.id;
                    for other in &project.resources {
                        if let ResourceData::AnimationClip(clip) = &other.data {
                            if clip.skeleton == Some(skeleton) {
                                keep.insert(other.id.raw());
                            }
                        }
                    }
                }
                ResourceData::AnimationSet(s) => {
                    keep_id(&mut keep, s.skeleton);
                    for clip in [
                        s.idle_clip,
                        s.walk_clip,
                        s.run_clip,
                        s.turn_clip,
                        s.roll_clip,
                        s.backstep_clip,
                    ] {
                        keep_id(&mut keep, clip);
                    }
                    for clip in &s.clips {
                        keep.insert(clip.raw());
                    }
                    for binding in &s.action_clips {
                        keep.insert(binding.clip.raw());
                    }
                }
                ResourceData::Character(c) => {
                    keep_id(&mut keep, c.model);
                    keep_id(&mut keep, c.animation_set);
                    keep_id(&mut keep, c.material);
                }
                ResourceData::Weapon(w) => keep_id(&mut keep, w.model),
                _ => {}
            }
        }
        if keep.len() == before {
            break;
        }
    }

    // Report by kind.
    let kind = |data: &ResourceData| match data {
        ResourceData::Material(_) => "Material",
        ResourceData::Model(_) => "Model",
        ResourceData::Skeleton(_) => "Skeleton",
        ResourceData::AnimationSource(_) => "AnimationSource",
        ResourceData::AnimationClip(_) => "AnimationClip",
        ResourceData::AnimationSet(_) => "AnimationSet",
        ResourceData::Character(_) => "Character",
        _ => "Other",
    };
    let mut rows: Vec<(&str, usize, usize)> = Vec::new();
    for r in &project.resources {
        let k = kind(&r.data);
        let kept = keep.contains(&r.id.raw());
        match rows.iter_mut().find(|(name, ..)| *name == k) {
            Some(row) => {
                if kept {
                    row.1 += 1
                } else {
                    row.2 += 1
                }
            }
            None => rows.push((k, kept as usize, !kept as usize)),
        }
    }
    if std::env::args().any(|a| a == "--list") {
        for r in &project.resources {
            if !keep.contains(&r.id.raw()) {
                println!("orphan {:>5} {:<14} {}", r.id.raw(), kind(&r.data), r.name);
            }
        }
    }
    println!("{:<16} {:>5} {:>7}", "kind", "kept", "orphan");
    for (name, kept, orphan) in &rows {
        println!("{name:<16} {kept:>5} {orphan:>7}");
    }

    // Kept file paths, resolved against the project dir.
    let project_dir = PathBuf::from(&project_dir);
    let mut kept_files: HashSet<PathBuf> = HashSet::new();
    let mut path = |p: &str| {
        if p.is_empty() {
            return;
        }
        let raw = Path::new(p);
        let resolved = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            project_dir.join(raw)
        };
        if let Ok(canon) = resolved.canonicalize() {
            kept_files.insert(canon);
        }
    };
    for r in &project.resources {
        if !keep.contains(&r.id.raw()) {
            continue;
        }
        match &r.data {
            ResourceData::Material(m) => {
                if let Some(p) = &m.psxt_path {
                    path(p);
                }
            }
            ResourceData::Model(m) => {
                path(&m.model_path);
                if let Some(p) = &m.texture_path {
                    path(p);
                }
                if let Some(p) = &m.source_path {
                    path(p);
                }
            }
            ResourceData::AnimationClip(c) => path(&c.psxanim_path),
            ResourceData::AnimationSource(a) => path(&a.source_path),
            _ => {}
        }
    }

    let mut orphan_files: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![project_dir.join("assets")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(canon) = p.canonicalize() {
                // Only cooked artifacts are candidates: audio, source
                // art (png/fbx/blend) and anything else is referenced
                // through channels this scan does not trace, so it is
                // never deletable here.
                let cooked = matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("psxt" | "psxmdl" | "psxanim")
                );
                if cooked && !kept_files.contains(&canon) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    orphan_files.push((p, size));
                }
            }
        }
    }
    let orphan_bytes: u64 = orphan_files.iter().map(|(_, s)| *s).sum();
    println!(
        "orphan files: {} ({} KB)",
        orphan_files.len(),
        orphan_bytes / 1024
    );

    if apply {
        project.resources.retain(|r| keep.contains(&r.id.raw()));
        project
            .save_to_path(project_dir.join("project.ron"))
            .expect("save project");
        for (p, _) in &orphan_files {
            let _ = std::fs::remove_file(p);
        }
        println!("applied: resources pruned, orphan files deleted");
    }
}

/// Delete resources whose cooked asset path starts with one of `prefixes`
/// or whose name is in `names`, then delete files under those prefixes that
/// no surviving resource references. Prints what it would do; writes only
/// with `apply`.
fn delete_targeted(
    project: &mut ProjectDocument,
    project_dir: &Path,
    prefixes: &[String],
    names: &[String],
    apply: bool,
) {
    let path_hits = |data: &ResourceData| -> bool {
        let paths: Vec<&str> = match data {
            ResourceData::Model(m) => vec![m.model_path.as_str()],
            ResourceData::AnimationClip(c) => vec![c.psxanim_path.as_str()],
            _ => Vec::new(),
        };
        paths
            .iter()
            .any(|path| prefixes.iter().any(|prefix| path.starts_with(prefix.as_str())))
    };
    let doomed: Vec<ResourceId> = project
        .resources
        .iter()
        .filter(|r| path_hits(&r.data) || names.iter().any(|n| n == &r.name))
        .map(|r| r.id)
        .collect();
    for id in &doomed {
        if let Some(r) = project.resources.iter().find(|r| r.id == *id) {
            println!("delete {:>5} {}", r.id.raw(), r.name);
        }
    }
    project.resources.retain(|r| !doomed.contains(&r.id));
    // Files under the prefixes not referenced by any surviving resource.
    let mut referenced: HashSet<PathBuf> = HashSet::new();
    for r in &project.resources {
        let mut push = |raw: &str| {
            referenced.insert(project_dir.join(raw));
        };
        match &r.data {
            ResourceData::Model(m) => {
                push(&m.model_path);
                if let Some(t) = &m.texture_path {
                    push(t);
                }
            }
            ResourceData::AnimationClip(c) => push(&c.psxanim_path),
            _ => {}
        }
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for prefix in prefixes {
        let root = project_dir.join(prefix.trim_end_matches('/'));
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if !referenced.contains(&p) {
                    files.push(p);
                }
            }
        }
    }
    println!(
        "targeted: {} resources, {} files under {:?}",
        doomed.len(),
        files.len(),
        prefixes
    );
    if apply {
        project
            .save_to_path(project_dir.join("project.ron"))
            .expect("save project");
        for p in &files {
            let _ = std::fs::remove_file(p);
        }
        for prefix in prefixes {
            let _ = std::fs::remove_dir(project_dir.join(prefix.trim_end_matches('/')));
        }
        println!("applied");
    }
}

#[cfg(test)]
mod tests {
    use super::seed_ids_from_ron;
    use std::collections::HashSet;

    #[test]
    fn box_prop_material_array_seeds_its_resources() {
        let ron = "BoxProp(materials: (Some((983)), Some((983)), Some((1316))))";
        let mut ids = HashSet::new();

        seed_ids_from_ron(ron, &mut ids);

        assert_eq!(ids, HashSet::from([983, 1316]));
    }
}
