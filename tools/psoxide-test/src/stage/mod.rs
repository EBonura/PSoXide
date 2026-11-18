//! The stages, in the order every game runs them, and what they share.

pub mod build;
pub mod clean;
pub mod cross;
pub mod host;
pub mod install;
pub mod journeys;
pub mod size;
pub mod speed;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::baseline::Baseline;
use crate::config::Config;
use crate::game::Game;
use crate::receipt::Store;
use crate::report::Timing;
use crate::util;

/// How much of the pipeline a run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// The pre-check: build, size, one window at offset 0, the precheck journeys on `real`,
    /// the ReARMed smoke.
    Quick,
    /// Everything a landing needs.
    Full,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Quick => "quick",
            Tier::Full => "full",
        }
    }
}

/// The hardware models of the full tier, spelled as `--hardware` spells them: the console, the
/// MiSTer core with its two GP0(1Fh) behaviours, and a strict row where PSoXide refuses what the
/// stricter third-party emulators refuse (CD acknowledges other than 07h/1Fh), far cheaper than a
/// Redux run. When the pinned gate gains another strict option (`mdec-req=hidden`), it joins that
/// row here and nowhere else.
pub const FULL_MODELS: &[&str] = &[
    "real",
    "mister",
    "mister,gpu-irq=never",
    "real,cd-ack=strict",
];

/// Everything a stage reads.
pub struct Ctx<'a> {
    pub repo: PathBuf,
    pub game: &'a Game,
    pub baseline: Option<Baseline>,
    pub config: &'a Config,
    /// `<config.work>/<game name>`.
    pub work: PathBuf,
    pub receipts: Store,
    pub tier: Tier,
    /// Per build: file name in `out` -> SHA-256, filled by the build stage.
    pub artifacts: BTreeMap<String, BTreeMap<String, String>>,
    pub timing: Mutex<Vec<Timing>>,
    /// Content digests of the original game files (`originals.rs`), cached by stat.
    pub originals: Mutex<crate::originals::Cache>,
}

impl Ctx<'_> {
    /// Record the wall time of one job for the timing sidecar.
    pub fn timed<R>(&self, what: &str, work: impl FnOnce() -> R) -> R {
        let start = util::now_seconds();
        let r = work();
        let end = util::now_seconds();
        self.timing
            .lock()
            .expect("timing lock")
            .push(crate::report::Timing {
                what: what.to_string(),
                start,
                end,
            });
        r
    }

    /// Run one emulator job inside the machine-wide budget (`kind` "emu" or "redux").
    pub fn machine<R>(&self, kind: &str, work: impl FnOnce() -> R) -> R {
        let b = &self.config.budget;
        let count = if kind == "redux" {
            b.machine_redux
        } else {
            b.machine_emulators
        };
        crate::sched::machine_slot(b.shared_dir.as_deref(), kind, count, work)
    }

    /// A directory under the work tree, created.
    pub fn dir(&self, parts: &[&str]) -> Result<PathBuf, String> {
        let mut path = self.work.clone();
        for p in parts {
            path.push(p);
        }
        std::fs::create_dir_all(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
        Ok(path)
    }

    /// The environment every child the harness starts gets: the cargo budget, a private guest
    /// stage dir, the GPU linked-list check, and no inherited `CARGO_TARGET_DIR` (a shared target
    /// across trees has overwritten builds before).
    pub fn child_env(&self, stage_root: &Path) -> Vec<(String, Option<String>)> {
        vec![
            (
                "CARGO_BUILD_JOBS".into(),
                Some(self.config.budget.jobs.to_string()),
            ),
            (
                "PSOXIDE_GUEST_STAGE_ROOT".into(),
                Some(stage_root.display().to_string()),
            ),
            ("PSOXIDE_CHECK_GPU_LL_FIFO".into(), Some("1".into())),
            ("CARGO_TARGET_DIR".into(), None),
        ]
    }

    /// `child_env` plus the build's own `env`, with `{input:NAME}` resolved from the user
    /// config's `[inputs]`.
    pub fn build_env(
        &self,
        build: &crate::game::Build,
        stage_root: &Path,
    ) -> Result<Vec<(String, Option<String>)>, String> {
        self.command_env(stage_root, &build.env, &build.env_remove)
    }

    /// `child_env`, minus `remove`, plus `extra` with its placeholders resolved.
    pub fn command_env(
        &self,
        stage_root: &Path,
        extra: &BTreeMap<String, String>,
        remove: &[String],
    ) -> Result<Vec<(String, Option<String>)>, String> {
        let mut env = self.child_env(stage_root);
        for (name, value) in extra {
            env.push((name.clone(), Some(self.resolve(value)?)));
        }
        for name in remove {
            env.push((name.clone(), None));
        }
        Ok(env)
    }

    /// The content digest of the original game files `name`, checked against the game file's
    /// `[originals]` when it names them.
    pub fn input_digest(&self, name: &str) -> Result<String, String> {
        let path = self.config.inputs.get(name).ok_or_else(|| {
            format!(
                "the build needs the original game files `{name}`: set [inputs] {name} = \"/path\" \
                 in the psoxide-test config"
            )
        })?;
        let digest = self
            .originals
            .lock()
            .map_err(|_| "input digest cache lock".to_string())?
            .digest(path)?;
        if let Some(want) = self.game.originals.get(name) {
            if *want != digest {
                return Err(format!(
                    "original game files `{name}` at {} have content {} but the game file's \
                     [originals] requires {}: point [inputs] {name} at the right files",
                    path.display(),
                    util::short(&digest),
                    util::short(want)
                ));
            }
        }
        Ok(digest)
    }

    /// `value` as a receipt key sees it: `{input:NAME}` becomes the content digest of those
    /// files (never their path), `{tool:NAME}` the configured tool path.
    pub fn key_text(&self, value: &str) -> Result<String, String> {
        let mut keyed = value.to_string();
        for name in crate::originals::names(value) {
            let digest = self.input_digest(&name)?;
            keyed = keyed.replace(
                &format!("{{input:{name}}}"),
                &format!("<original {name} {digest}>"),
            );
        }
        self.resolve(&keyed)
    }

    /// Every `{input:NAME}` a build's commands and env use, with its content digest.
    pub fn build_originals(
        &self,
        build: &crate::game::Build,
    ) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        for text in build.env.values().chain(build.commands.iter().flatten()) {
            for name in crate::originals::names(text) {
                if let std::collections::btree_map::Entry::Vacant(slot) = out.entry(name) {
                    let digest = self.input_digest(slot.key())?;
                    slot.insert(digest);
                }
            }
        }
        Ok(out)
    }

    /// Resolve `{input:NAME}` and `{tool:NAME}` placeholders from the user config.
    pub fn resolve(&self, value: &str) -> Result<String, String> {
        let value = resolve_inputs(value, &self.config.inputs)?;
        let tools = &self.config.tools;
        let mut out = value.clone();
        for (name, path) in [
            ("frontend", tools.frontend.clone()),
            ("gate", tools.gate.as_ref().map(|g| g.path.clone())),
            ("rearmed_core", tools.rearmed_core.clone()),
            ("redux", tools.redux.clone()),
        ] {
            let token = format!("{{tool:{name}}}");
            if out.contains(&token) {
                let path = path.ok_or_else(|| {
                    format!("`{value}` needs {{tool:{name}}}: set it under [tools] in the config")
                })?;
                out = out.replace(&token, &path.display().to_string());
            }
        }
        if let Some(at) = out.find("{tool:") {
            return Err(format!("unknown tool placeholder in `{}`", &out[at..]));
        }
        Ok(out)
    }
}

/// Replace every `{input:NAME}` in `value` with the configured path of that input.
pub fn resolve_inputs(value: &str, inputs: &BTreeMap<String, PathBuf>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find("{input:") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 7..];
        let end = tail
            .find('}')
            .ok_or_else(|| format!("unclosed {{input: in `{value}`"))?;
        let name = &tail[..end];
        let path = inputs.get(name).ok_or_else(|| {
            format!(
                "the build needs the original game files `{name}`: set [inputs] {name} = \"/path\" \
                 in the psoxide-test config"
            )
        })?;
        out.push_str(&path.display().to_string());
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The SDK revision a game pins, from its components lock (the SDK's hydration convention).
pub fn sdk_pin(repo: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(repo.join("components.lock.json")) else {
        return "unknown".into();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.pointer("/components/sdk/revision")
                .and_then(|r| r.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_resolve_or_name_what_is_missing() {
        let inputs = BTreeMap::from([(
            "original-disc".to_string(),
            PathBuf::from("/games/original"),
        )]);
        assert_eq!(
            resolve_inputs("{input:original-disc}/data", &inputs).unwrap(),
            "/games/original/data"
        );
        assert!(resolve_inputs("{input:other}", &inputs)
            .unwrap_err()
            .contains("[inputs] other"));
        assert_eq!(resolve_inputs("plain", &inputs).unwrap(), "plain");
    }
}
