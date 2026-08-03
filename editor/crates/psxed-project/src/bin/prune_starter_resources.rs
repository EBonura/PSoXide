//! Scan (and with --apply, prune) resources in the default project that
//! nothing reachable from its scenes/UI references. Run from editor/:
//!   cargo run -p psxed-project --bin prune_starter_resources [-- --apply]

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
                }
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
