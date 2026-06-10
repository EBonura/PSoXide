//! Path and name helpers shared by the texture and model importers.

use std::path::Path;

/// Render a path relative to the project root when possible, else the
/// full path. Import records store project-relative paths so projects
/// stay relocatable.
pub(crate) fn relativise(path: &Path, project_root: Option<&Path>) -> String {
    if let Some(root) = project_root {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

/// Sanitise a user-supplied resource name into a filesystem-safe stem
/// (lowercase ASCII alphanumerics, runs of anything else collapse to a
/// single underscore). `fallback` is used when nothing survives.
pub(crate) fn sanitize_name(name: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn relativise_under_project_root_is_relative() {
        let root = PathBuf::from("/tmp/proj");
        let path = PathBuf::from("/tmp/proj/assets/models/x.psxmdl");
        assert_eq!(relativise(&path, Some(&root)), "assets/models/x.psxmdl");
        // No root → absolute kept.
        let abs = relativise(&path, None);
        assert_eq!(abs, "/tmp/proj/assets/models/x.psxmdl");
    }

    #[test]
    fn sanitize_name_strips_punctuation() {
        assert_eq!(sanitize_name("Brick Wall", "texture"), "brick_wall");
        assert_eq!(sanitize_name("floor-tile_01", "texture"), "floor_tile_01");
        assert_eq!(sanitize_name("Obsidian Wraith", "model"), "obsidian_wraith");
        assert_eq!(sanitize_name("hooded-wretch", "model"), "hooded_wretch");
        assert_eq!(sanitize_name("!!!", "texture"), "texture");
        assert_eq!(sanitize_name("!!!", "model"), "model");
    }
}
