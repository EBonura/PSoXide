//! Receipts: a passed outcome stored under its content key. A stage whose key is unchanged reuses
//! the stored outcome and says so; a failed outcome is never stored, so a red always re-runs.

use std::path::PathBuf;

use crate::report::Outcome;

/// The receipt directory of one work tree.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    fn path(&self, stage: &str, key: &str) -> PathBuf {
        self.dir.join(stage).join(format!("{key}.json"))
    }

    /// The stored outcome for `key`, if one passed before.
    pub fn get(&self, stage: &str, key: &str) -> Option<Outcome> {
        let text = std::fs::read_to_string(self.path(stage, key)).ok()?;
        let outcome: Outcome = serde_json::from_str(&text).ok()?;
        outcome.passed().then_some(outcome)
    }

    /// Keep a passed outcome. A failed one is dropped.
    pub fn put(&self, stage: &str, key: &str, outcome: &Outcome) -> Result<(), String> {
        if !outcome.passed() {
            return Ok(());
        }
        let path = self.path(stage, key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(outcome).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Check;

    #[test]
    fn only_a_pass_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let red = Outcome::error("x", "boom");
        store.put("size", "k1", &red).unwrap();
        assert!(store.get("size", "k1").is_none());
        let green = Outcome {
            checks: vec![Check::pass("text", "1")],
            ..Outcome::default()
        };
        store.put("size", "k2", &green).unwrap();
        assert_eq!(store.get("size", "k2"), Some(green));
        assert!(store.get("speed", "k2").is_none());
    }
}
