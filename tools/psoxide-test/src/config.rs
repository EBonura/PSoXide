//! The machine-local settings: where the tools, the library and the work directory are, and
//! the budget. Never committed; `config.example.toml` beside the crate shows every key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The emulator revision whose `psoxide-gate` this harness version runs journeys with
/// (PSoXide-emulator main, `--conformance=RULES`). A gate built from another revision is
/// refused, so every result names the runner it was judged by.
pub const GATE_REVISION: &str = "477d8b2149681a86513502485cc5c58d565fa1d5";

/// The user config.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Receipts, artifacts copies, logs and results go here.
    pub work: PathBuf,
    /// The games library the install stage writes to.
    #[serde(default)]
    pub library: Option<PathBuf>,
    #[serde(default)]
    pub budget: Budget,
    #[serde(default)]
    pub tools: Tools,
    /// Original game files by the name a game file uses for them.
    #[serde(default)]
    pub inputs: BTreeMap<String, PathBuf>,
}

/// How much runs at once.
#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Cargo jobs for builds (`CARGO_BUILD_JOBS`).
    #[serde(default = "three")]
    pub jobs: usize,
    /// Emulator processes at once (PSoXide gate, psoxide-pgo replays, ReARMed).
    #[serde(default = "two")]
    pub emulators: usize,
    /// PCSX-Redux processes at once (counted inside `emulators` too).
    #[serde(default = "one")]
    pub redux: usize,
}

fn one() -> usize {
    1
}
fn two() -> usize {
    2
}
fn three() -> usize {
    3
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            jobs: 3,
            emulators: 2,
            redux: 1,
        }
    }
}

/// Paths to the pinned components.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    /// `psoxide-gate` and the emulator revision it was built from.
    #[serde(default)]
    pub gate: Option<PinnedTool>,
    /// The PSoXide `frontend` binary `psoxide-pgo measure` replays tapes with.
    #[serde(default)]
    pub frontend: Option<PathBuf>,
    /// The PCSX-ReARMed libretro core (`pcsx_rearmed_libretro.dylib`).
    #[serde(default)]
    pub rearmed_core: Option<PathBuf>,
    /// The PCSX-Redux bundle's `Contents` directory (official build).
    #[serde(default)]
    pub redux: Option<PathBuf>,
}

/// A tool binary and the source revision it was built from.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedTool {
    pub path: PathBuf,
    pub revision: String,
}

impl Config {
    /// Read the config from `PSOXIDE_TEST_CONFIG` or `~/.config/psoxide-test/config.toml`.
    pub fn load() -> Result<Config, String> {
        let path = match std::env::var_os("PSOXIDE_TEST_CONFIG") {
            Some(p) => PathBuf::from(p),
            None => {
                let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
                Path::new(&home).join(".config/psoxide-test/config.toml")
            }
        };
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "read {}: {e}\n  write it from tools/psoxide-test/config.example.toml, or point \
                 PSOXIDE_TEST_CONFIG at one",
                path.display()
            )
        })?;
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if config.budget.jobs == 0 || config.budget.emulators == 0 || config.budget.redux == 0 {
            return Err(format!(
                "{}: every [budget] value must be at least 1",
                path.display()
            ));
        }
        Ok(config)
    }

    /// The gate binary, checked against the pinned revision.
    pub fn gate(&self) -> Result<&Path, String> {
        let gate = self.tools.gate.as_ref().ok_or(
            "no gate configured: set [tools.gate] path and revision in the psoxide-test config",
        )?;
        if !GATE_REVISION.starts_with(gate.revision.as_str()) || gate.revision.len() < 8 {
            return Err(format!(
                "the configured gate was built from emulator {} but this harness runs journeys \
                 with {}: build psoxide-gate at that revision (cargo build --release -p \
                 psoxide-gate in a PSoXide-emulator checkout of it) and point [tools.gate] at it",
                gate.revision,
                &GATE_REVISION[..12]
            ));
        }
        require_file(&gate.path, "[tools.gate] path")
    }

    /// A configured tool file, or an error naming the key to set.
    pub fn tool<'a>(&self, path: &'a Option<PathBuf>, key: &str) -> Result<&'a Path, String> {
        match path {
            Some(p) => require_file(p, key),
            None => Err(format!("{key} is not set in the psoxide-test config")),
        }
    }
}

fn require_file<'a>(path: &'a Path, key: &str) -> Result<&'a Path, String> {
    if path.exists() {
        Ok(path)
    } else {
        Err(format!("{key} {} does not exist", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_config_takes_the_default_budget() {
        let config: Config = toml::from_str("work = \"/w\"").unwrap();
        assert_eq!(config.budget.emulators, 2);
        assert_eq!(config.budget.redux, 1);
        assert!(config.tools.gate.is_none());
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(toml::from_str::<Config>("work = \"/w\"\nlibary = \"/x\"").is_err());
    }

    #[test]
    fn a_gate_from_another_revision_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("gate");
        std::fs::write(&bin, "").unwrap();
        let text = format!(
            "work = \"/w\"\n[tools.gate]\npath = \"{}\"\nrevision = \"eca75bc6\"",
            bin.display()
        );
        let config: Config = toml::from_str(&text).unwrap();
        assert!(config.gate().unwrap_err().contains("477d8b214968"));
        let text = text.replace("eca75bc6", "477d8b21");
        let config: Config = toml::from_str(&text).unwrap();
        assert!(config.gate().is_ok());
    }
}
