//! The per-game file, `psoxide-test.toml` at the repo root: what to build, which artifacts the
//! stages read, the bars and windows, the journeys, the smoke, the host checks, the install.
//! It declares; the harness decides how. Unknown keys are errors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The fixed path of the game file inside a repo.
pub const GAME_FILE: &str = "psoxide-test.toml";
/// The fixed path of the baseline beside it.
pub const BASELINE_FILE: &str = "psoxide-test.baseline.toml";

/// The whole game file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub schema: u32,
    /// Report and library name.
    pub name: String,
    /// `owner/name` on GitHub, for the clean-clone stage.
    pub repository: String,
    #[serde(rename = "build")]
    pub builds: Vec<Build>,
    /// One `[[size]]` per program whose link is judged (a repo may build several).
    #[serde(default, rename = "size")]
    pub sizes: Vec<Size>,
    #[serde(default, rename = "speed")]
    pub windows: Vec<Window>,
    pub journeys: Option<Journeys>,
    pub cross: Option<Cross>,
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
    /// One `[[clean]]` per program whose bytes a fresh clone must reproduce (a single `[clean]`
    /// table is still accepted).
    #[serde(default, rename = "clean", deserialize_with = "one_or_many")]
    pub cleans: Vec<Clean>,
    /// One `[[install]]` per library entry the repo produces.
    #[serde(default, rename = "install")]
    pub installs: Vec<Install>,
    /// Original game files by `{input:NAME}` name -> the content digest they must have
    /// (`psoxide-test` prints it as `input:NAME` in a run's build facts). A configured input with
    /// other bytes fails the build before any command runs. Optional per name.
    #[serde(default)]
    pub originals: BTreeMap<String, String>,
    /// Where the SDK pin is written when it is not in components.lock.json or sdk.lock.json.
    #[serde(default)]
    pub sdk: Option<SdkPin>,
    /// Results this verdict depends on: one per program a disc image carries. Each must be a
    /// complete full-tier PASS whose judged file is the member the image holds byte for byte.
    #[serde(default, rename = "requires")]
    pub requires: Vec<Requires>,
    /// Deepest-stack bars: how deep the stack has been by a checkpoint of a journey.
    #[serde(default, rename = "stack")]
    pub stacks: Vec<Stack>,
}

/// One deepest-stack measure: RAM saved at a journey's checkpoint, read below the stack top.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    /// The journey (file stem) and the checkpoint where RAM is read (late in the journey: the
    /// figure is the deepest the stack has been by then).
    pub journey: String,
    pub checkpoint: String,
    /// The build whose link map gives the stack top (`STACK_INIT`, which a program hosted
    /// under the scene core links lower with `RAM_SIZE_OVERRIDE`). Unset: `top`, else the SDK
    /// linker script's stock 0x801FFF00. Not both.
    #[serde(default)]
    pub build: Option<String>,
    /// The initial stack pointer and the reserve below it (default: the SDK linker script's
    /// 0x801FFF00 and 0x8000; a program that overrides STACK_RESERVE says so here).
    #[serde(default)]
    pub top: Option<u32>,
    #[serde(default)]
    pub reserve: Option<u32>,
    /// Bytes the depth may grow over the baseline (default 0; needs `reason`).
    #[serde(default)]
    pub max_growth: u32,
    #[serde(default)]
    pub reason: Option<String>,
}

/// One result another repo's verdict stands on (a member of a disc image).
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// The member's `id` in the build's members manifest.
    pub id: String,
    /// The content hash (sha256, 64 hex) of the member's result file: results are named by their
    /// content, so the hash pins the verdict and the bytes it judged.
    pub result: String,
    /// `<build>/<file>` as the member's result facts name the judged artifact the image must carry
    /// (its executable), e.g. `ship/nitroxide.exe`.
    pub file: String,
}

/// An SDK pin written in a script: the hex revision right after `after` in `file`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkPin {
    pub file: String,
    pub after: String,
    #[serde(default = "sdk_repository")]
    pub repository: String,
    /// The guest tree of that revision, recorded (optional; checked when present).
    #[serde(default)]
    pub guest_tree: Option<String>,
}

fn sdk_repository() -> String {
    "EBonura/PSoXide".into()
}

/// One build: commands run in order at the repo root, then files collected into `out`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub name: String,
    /// Output directory, relative to the repo root; `{out}` in a command is its absolute path.
    pub out: String,
    pub commands: Vec<Vec<String>>,
    /// Destination name in `out` -> source path relative to the repo root. A source ending in
    /// `*` takes the newest file with that prefix, which must have been written by this build.
    #[serde(default)]
    pub collect: BTreeMap<String, String>,
    /// Extra environment for the commands. `{input:NAME}` is the path the user config gives
    /// the original game files of that name (`[inputs]`); a missing one is an error naming it.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables removed from the commands' environment (a build that must not see them).
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// The disc's cue inside `out`. Empty for a build that writes no disc (a cook another
    /// build reads); such a build cannot be played, measured or installed.
    #[serde(default)]
    pub disc: String,
    /// Substrings of the build's logs that make it a failure although every command exited 0
    /// (a driver that only warns when it links without its placement layout).
    #[serde(default)]
    pub fail_on: Vec<String>,
    /// Least share, in percent, of a psoxide-pgo layout or sample profile that must bind onto
    /// this build (default 100); any other value needs `bind_reason`.
    #[serde(default)]
    pub bind_min: Option<f64>,
    #[serde(default)]
    pub bind_reason: Option<String>,
    /// `false` for a build that links without its placement layout or sample profile on purpose
    /// (a test disc on another feature set): psoxide-pgo's refusal lines and bind lines are then
    /// reported as a skip with `layout_reason` instead of failing the build.
    #[serde(default)]
    pub layout: Option<bool>,
    #[serde(default)]
    pub layout_reason: Option<String>,
    /// The committed psoxide-pgo layout profiles this build links with, relative to the repo
    /// root. Installing it needs each one to carry psoxide-pgo's search stamp (`install`,
    /// stage/install.rs). Empty: every tracked `*.layout` file that is a layout profile. Not
    /// written into the receipt key when empty, so a game file without it keeps its keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout_files: Vec<String>,
    /// The link map inside `out` (size and symbols read it).
    #[serde(default)]
    pub map: Option<String>,
    /// The ordering file the link followed, inside `out`, with the `.placed` sidecar
    /// `psoxide-pgo place` writes beside it. The build stage runs `psoxide-pgo followed` on it
    /// against `map`; the install build needs it when it links with a searched layout (a
    /// driver that links by itself and falls back to plain order would ship a stamp the disc
    /// lacks). Not written into the receipt key when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// Other programs this build links beside its own `map` (the scene core's prebuilt image,
    /// a boot scene), each with its own link map inside `out`. The harness writes
    /// `images.sym` beside them: every image symbol as `<image>:<name>`, a symbol list the gate
    /// and the adapters read, so two links that share SDK names never make a name ambiguous.
    /// The build's own `map` keeps its unprefixed names. `[[size]] image` judges one of them.
    /// Not written into the receipt key when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Image>,
    /// A manifest the build writes into `out` naming the programs it carries (a disc image):
    /// a JSON array of `{ index, id, exe_sha256, map, words? }`, `map` relative to `out`. The
    /// harness writes `members.sym` beside it (every member symbol as `<slug>:<name>`) and checks
    /// each entry against `[[requires]]`.
    #[serde(default)]
    pub members: Option<String>,
    /// Path prefixes of the files whose bytes form the receipt key. Empty: every file git would
    /// hand a clone, minus `exclude`. A cook lists only its sources and cooker here.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Path prefixes left out of the default key (journeys and the harness files never change a
    /// build).
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
}

/// One more program of a build, with its own link.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Image {
    /// The symbol prefix (`core` gives `core:__psx_rt_vblank_count`): lowercase letters,
    /// digits and `-`.
    pub name: String,
    /// Its link map, inside `out`.
    pub map: String,
    /// Its executable inside `out`, when the build collects it (the install names it among the
    /// disc's programs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

fn default_exclude() -> Vec<String> {
    ["tests/", GAME_FILE, BASELINE_FILE]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// The size bars.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub build: String,
    /// One of the build's `images` instead of its own `map`: its own bar, its own baseline
    /// entry `<build>:<image>`.
    #[serde(default)]
    pub image: Option<String>,
    /// An absolute ceiling on `.text + .data + .bss` of the measured link, judged on every run
    /// with or without a baseline (a resident image with a fixed window, such as the scene
    /// core's 10,240 B).
    #[serde(default)]
    pub max_bytes: Option<u32>,
    /// Bytes `.text` may grow over the baseline (default 0).
    #[serde(default)]
    pub text_max_growth: u32,
    /// Bytes free RAM may fall under the baseline (default 0).
    #[serde(default)]
    pub free_min_drop: u32,
    /// Required when either tolerance is not 0.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One speed window on a poll-bound tape.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub name: String,
    pub build: String,
    /// Poll-bound tape the window replays, relative to the repo root.
    #[serde(default)]
    pub tape: Option<String>,
    /// `[from, to]` in the game's own pad polls of the tape.
    #[serde(default)]
    pub polls: Option<[u32; 2]>,
    /// The window starts when this game-state condition first holds (the journey condition
    /// syntax: `{ kind = "ram", sym, offset, op, value }`). Accepted in the schema; the measuring
    /// tool cannot start a window on a condition yet, so a window that uses it fails with that
    /// message instead of being skipped.
    #[serde(default)]
    pub from: Option<toml::Table>,
    /// The window ends when this condition first holds after `from`.
    #[serde(default)]
    pub until: Option<toml::Table>,
    /// Conditions only: the locating replay stops at this poll; `until` must have held by then.
    #[serde(default)]
    pub max_polls: Option<u32>,
    /// Anchor the window on game state instead of power-on: RAM conditions (journey syntax,
    /// on the build's map and `images` symbols) that hold together once the game itself runs.
    /// The harness finds the poll they first hold at in an input-free replay, feeds the tape
    /// from there (the tape's polls, `polls`, `from`/`until` and `max_polls` all count from that
    /// poll), and the window's identity names the anchor, never the poll it was found at. So a
    /// splash or boot core in front of the game moves the anchor, not the tape or the window.
    /// Without it the window counts from power-on, as before.
    #[serde(default, deserialize_with = "one_or_many")]
    pub anchor: Vec<toml::Table>,
    /// With `anchor`: the input-free replay looking for it stops at this poll.
    #[serde(default)]
    pub anchor_max_polls: Option<u32>,
    /// Phase shifts of the window, in polls; must contain 0. The quick tier runs 0 only.
    #[serde(default = "zero_offset")]
    pub offsets: Vec<i32>,
    /// Extra frontend arguments the replay needs.
    #[serde(default)]
    pub launch_args: Vec<String>,
    /// Hardware models measured as well, in the full tier, each its own row `<name>@<model>`
    /// with its own baseline (e.g. `["mister"]`: the MiSTer core's slower GPU and CD timing).
    #[serde(default)]
    pub models: Vec<String>,
    /// Keep the tape's last pad sample on every poll after the tape ends (the frontend's
    /// `--input-tape-hold-last`), so a window may outlive its tape (a map load after it).
    #[serde(default)]
    pub hold_last: bool,
    /// The landing profile's speed slack for this window, in thousandths of the blessed mean
    /// (default 50 = 5%): work per frame may rise and frames presented may drop by that much
    /// beyond the measured phase spread. The install profile (the default) ignores it.
    #[serde(default)]
    pub landing_work_slack_permille: Option<u32>,
}

/// `landing_work_slack_permille` when a window does not set it: 5%.
pub const DEFAULT_LANDING_SLACK_PERMILLE: u32 = 50;

impl Window {
    /// The landing profile's slack, in thousandths (default 5%).
    pub fn landing_slack_permille(&self) -> u32 {
        self.landing_work_slack_permille
            .unwrap_or(DEFAULT_LANDING_SLACK_PERMILLE)
    }

    /// The frontend arguments the replay gets: the declared ones, plus hold-last.
    pub fn frontend_args(&self) -> Vec<String> {
        let mut args = self.launch_args.clone();
        if self.hold_last {
            args.push("--input-tape-hold-last".into());
        }
        args
    }

    /// The row names this window measures: itself (the console model), then one per extra model.
    pub fn rows(&self) -> Vec<(String, Option<&str>)> {
        std::iter::once((self.name.clone(), None))
            .chain(
                self.models
                    .iter()
                    .map(|m| (format!("{}@{m}", self.name), Some(m.as_str()))),
            )
            .collect()
    }
}

fn zero_offset() -> Vec<i32> {
    vec![0]
}

/// The journeys every model runs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journeys {
    /// Glob (or list of globs) of journey files, relative to the repo root.
    #[serde(deserialize_with = "one_or_many")]
    pub files: Vec<String>,
    /// Journey names (file stems) the quick tier runs on `real`.
    #[serde(default)]
    pub precheck: Vec<String>,
    /// Conformance rules enforced on every run.
    #[serde(default = "default_conformance")]
    pub conformance: Vec<String>,
    /// Journeys allowed the gate's forced-analog pad: name -> reason.
    #[serde(default)]
    pub forced_analog: BTreeMap<String, String>,
    /// Also run the hardware renderer at 1x and 3x on every checkpoint (the gate's `--strict`)
    /// on every model instead of the CPU frame alone (`--no-hw`).
    #[serde(default)]
    pub hw: bool,
    /// Where a journey's goldens are, `{name}` being its `name` (default: the gate's
    /// tests/golden/<name>), e.g. one golden set per region.
    #[serde(default)]
    pub golden_dir: Option<String>,
    /// Journeys that run on some models only, each with the reason; the others are reported as
    /// SKIP with that reason on every run.
    #[serde(default)]
    pub only_on: BTreeMap<String, OnlyOn>,
    /// Journeys that keep the CPU frame only although `hw = true`: name -> reason.
    #[serde(default)]
    pub hw_off: BTreeMap<String, String>,
    /// The game's own free-running clocks (symbols), beside the SDK's: a wait or hold until one
    /// reaches an absolute value is a fixed wait and counts in the open-loop ratchet.
    #[serde(default)]
    pub clocks: Vec<String>,
    /// Journey templates (file stems) played once per `[[requires]]` entry instead of as they
    /// are: `{member}`, `{id}`, `{index}`, `{repo}` and `{word:N}` are substituted. A smoke of
    /// the same stem is instanced the same way on the other emulators.
    #[serde(default)]
    pub instances: Vec<String>,
    /// Journeys also played with other pads (the gate's `--pad`): journey -> pads, each an extra
    /// row `<journey>@<model>/<pad>` on every model; the ratchet counts the file once.
    #[serde(default)]
    pub pads: BTreeMap<String, Vec<String>>,
}

/// A journey's model subset.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnlyOn {
    pub models: Vec<String>,
    pub reason: String,
}

/// The fleet's enforced conformance rules, defined here once. A game file may list more (or a
/// documented subset with its reason); a rule the gate gains is added here in one line.
fn default_conformance() -> Vec<String> {
    vec![
        "cd-ack-partial".into(),
        "cd-ack-param-reset".into(),
        "mdec-status-before-feed".into(),
    ]
}

/// The cross-emulator smoke.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cross {
    /// Smoke journeys (gate TOML, adapter subset), relative to the repo root.
    pub smoke: Vec<String>,
    /// Pads every smoke runs with on the other emulators.
    #[serde(default = "default_pads")]
    pub pads: Vec<String>,
    /// Fewest distinct pictures a run must present (a hang presents few).
    #[serde(default = "default_min_distinct")]
    pub min_distinct: u32,
    /// A run of another build with no input and no state (a shipping disc whose state blocks
    /// are compiled out): it must boot, present `min_distinct` pictures and not pause or crash.
    #[serde(default)]
    pub boot: Option<Boot>,
}

/// The input-free boot run of `[cross]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boot {
    pub build: String,
    /// Frames the run has (with `until`: frames the condition has to hold by).
    pub frames: u64,
    /// The game-state proof that the game itself started: RAM conditions (the journey syntax,
    /// resolved against the build's map and its `images` symbols) that hold together once the
    /// game reached its own first frame or title. Without it the run only counts pictures,
    /// which a splash or boot core in front of the game passes alone.
    #[serde(default, deserialize_with = "one_or_many")]
    pub until: Vec<toml::Table>,
    /// With `until`: frames the run goes on after the conditions held; `min_distinct` is
    /// counted over those frames only, so pictures drawn before the game do not count.
    #[serde(default)]
    pub after: Option<u64>,
}

fn default_pads() -> Vec<String> {
    vec!["digital".into(), "dualshock".into()]
}
fn default_min_distinct() -> u32 {
    60
}

/// A host check: a command that must exit 0.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    /// argv run at the repo root. With `build`, `{out}` and `{disc}` are that build's output
    /// directory and cue, and the receipt follows its artifacts: a check that drives an older
    /// frontend or a game's own script against the built disc.
    pub command: Vec<String>,
    #[serde(default)]
    pub build: Option<String>,
    /// Working directory, relative to the repo root (default: the root), for a check that
    /// relies on its own `.cargo/config.toml`.
    #[serde(default)]
    pub dir: Option<String>,
    /// Extra environment; `{input:NAME}` and `{tool:NAME}` (frontend, gate, rearmed_core,
    /// redux) resolve from the user config, as in a build's `env`. The same two placeholders
    /// work in `command`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables the command must not see (the harness's own CARGO_BUILD_JOBS included).
    #[serde(default)]
    pub env_remove: Vec<String>,
    /// Run once per hardware model, with PSOXIDE_HARDWARE set (default: once, unset).
    #[serde(default)]
    pub models: Vec<String>,
    /// A number the command prints, judged like a bar and stored by `bless`.
    #[serde(default)]
    pub metric: Option<Metric>,
    /// Host checks run in parallel inside the CPU budget; those naming the same group run one
    /// after another (they share a build tree or start an emulator).
    #[serde(default)]
    pub exclusive: Option<String>,
    /// Emulators the command starts by itself: it holds that many machine-wide emulator
    /// slots while it runs (and should share an `exclusive` group with other such checks).
    #[serde(default)]
    pub emulators: Option<usize>,
    /// Path prefixes (repo-relative) of everything the check reads. With them its receipt is keyed
    /// on those files alone (plus `uses`), so a commit that changes nothing it reads (an SDK pin
    /// move, a journey edit) does not run it again. Empty: the whole tree, as before.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// What else the check reads: `sdk` (the hydrated SDK's guest sources: .psoxide/sdk, crates,
    /// engine), `gate` (the pinned gate revision), `frontend` (the configured frontend binary).
    #[serde(default)]
    pub uses: Vec<String>,
}

/// A number read from a host check's output: the first integer after `key=` or `key:`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub key: String,
    /// `lower` or `higher`: the direction that is better, judged against the baseline.
    #[serde(default)]
    pub better: Option<String>,
    /// How far the worse direction may go past the baseline (default 0; needs `reason`).
    #[serde(default)]
    pub tolerance: u64,
    #[serde(default)]
    pub reason: Option<String>,
    /// Absolute floor and ceiling, judged on every run with or without a baseline.
    #[serde(default)]
    pub min: Option<u64>,
    #[serde(default)]
    pub max: Option<u64>,
}

/// The clean-clone proof.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clean {
    pub build: String,
    /// Self-verifying caches APFS-cloned from the working tree into the fresh clone.
    #[serde(default)]
    pub seed: Vec<String>,
    /// Files of `out` that must be byte-identical (default: the disc's cue and bin).
    #[serde(default)]
    pub compare: Vec<String>,
    /// Builds run first in the clone, in order (a cook the build reads).
    #[serde(default)]
    pub before: Vec<String>,
    /// Fetch the whole history instead of one commit (a profile bound by git blob needs it).
    #[serde(default)]
    pub history: bool,
}

/// A table or an array of tables.
fn one_or_many<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        Many(Vec<T>),
        One(T),
    }
    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(t) => vec![t],
    })
}

/// The library install.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub build: String,
    /// The library entry (default: the game name). Files go flat into the library as `<name>.bin` and its sidecars.
    #[serde(default)]
    pub name: Option<String>,
}

impl Game {
    /// Read and check `<repo>/psoxide-test.toml`.
    pub fn load(repo: &Path) -> Result<Game, String> {
        let path = repo.join(GAME_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "read {}: {e}\n  a game is tested from its own {GAME_FILE}; MIGRATE.md says how \
                 to write one",
                path.display()
            )
        })?;
        Game::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse and validate the text of a game file.
    pub fn parse(text: &str) -> Result<Game, String> {
        let game: Game = toml::from_str(text).map_err(|e| e.to_string())?;
        game.validate()?;
        Ok(game)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!("schema {} is not 1", self.schema));
        }
        if !self.repository.contains('/') {
            return Err("repository must be `owner/name`".into());
        }
        if self.builds.is_empty() {
            return Err("at least one [[build]] is needed".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for b in &self.builds {
            if !names.insert(b.name.as_str()) {
                return Err(format!("duplicate build `{}`", b.name));
            }
            if b.commands.is_empty() || b.commands.iter().any(Vec::is_empty) {
                return Err(format!("build `{}`: every command needs a program", b.name));
            }
            if b.out.is_empty() || b.out.starts_with('/') || b.out.contains("..") {
                return Err(format!(
                    "build `{}`: out must be a path inside the repo",
                    b.name
                ));
            }
        }
        let known = |name: &str, what: &str| {
            if names.contains(name) {
                Ok(())
            } else {
                Err(format!(
                    "{what} names build `{name}`, which no [[build]] declares"
                ))
            }
        };
        for b in &self.builds {
            let mut seen = std::collections::BTreeSet::new();
            for image in &b.images {
                let valid = !image.name.is_empty()
                    && image
                        .name
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
                if !valid || !seen.insert(image.name.as_str()) {
                    return Err(format!(
                        "build `{}`: image `{}` needs a unique name of lowercase letters, digits \
                         and `-` (its symbol prefix)",
                        b.name, image.name
                    ));
                }
                if b.map.is_none() {
                    return Err(format!(
                        "build `{}`: images sit beside the build's own `map`: declare it",
                        b.name
                    ));
                }
            }
        }
        for size in &self.sizes {
            known(&size.build, "[[size]]")?;
            if self.build(&size.build).is_some_and(|b| b.map.is_none()) {
                return Err(format!("[size]: build `{}` declares no map", size.build));
            }
            if let Some(image) = &size.image {
                if self
                    .build(&size.build)
                    .is_some_and(|b| !b.images.iter().any(|i| &i.name == image))
                {
                    return Err(format!(
                        "[[size]]: build `{}` declares no image `{image}`",
                        size.build
                    ));
                }
            }
            if (size.text_max_growth != 0 || size.free_min_drop != 0)
                && size.reason.as_deref().is_none_or(str::is_empty)
            {
                return Err("[size]: a tolerance other than 0 needs `reason`".into());
            }
        }
        for w in &self.windows {
            known(&w.build, "[[speed]]")?;
            if let Some(m) = w
                .models
                .iter()
                .find(|m| !crate::stage::FULL_MODELS.contains(&m.as_str()) || *m == "real")
            {
                return Err(format!(
                    "[[speed]] `{}`: models lists `{m}`; name another full-tier model (real is \
                     always measured)",
                    w.name
                ));
            }
            if w.hold_last && w.tape.is_none() {
                return Err(format!("[[speed]] `{}`: hold_last needs a tape", w.name));
            }
            if !w.offsets.contains(&0) {
                return Err(format!("[[speed]] `{}`: offsets must contain 0", w.name));
            }
            if w.landing_slack_permille() > 1000 {
                return Err(format!(
                    "[[speed]] `{}`: landing_work_slack_permille is thousandths (at most 1000)",
                    w.name
                ));
            }
            match (w.anchor.is_empty(), w.anchor_max_polls) {
                (true, None) => {}
                (false, Some(_)) => {
                    if w.tape.is_none() {
                        return Err(format!(
                            "[[speed]] `{}`: an anchored window replays a tape fed from its \
                             anchor: declare tape",
                            w.name
                        ));
                    }
                    if w.launch_args.iter().any(|a| a == "--press") {
                        return Err(format!(
                            "[[speed]] `{}`: a --press schedule counts frames from power-on, \
                             which an anchor does not move: transcribe it into the tape",
                            w.name
                        ));
                    }
                    if self.build(&w.build).is_some_and(|b| b.map.is_none()) {
                        return Err(format!(
                            "[[speed]] `{}`: build `{}` declares no map to resolve the anchor",
                            w.name, w.build
                        ));
                    }
                }
                (false, None) => {
                    return Err(format!(
                        "[[speed]] `{}`: anchor needs anchor_max_polls (where the replay looking \
                         for it gives up)",
                        w.name
                    ))
                }
                (true, Some(_)) => {
                    return Err(format!(
                        "[[speed]] `{}`: anchor_max_polls without anchor",
                        w.name
                    ))
                }
            }
            match (&w.tape, w.polls, &w.from, &w.until) {
                (Some(_), Some([a, b]), None, None) if a < b => {}
                (Some(_), Some(_), None, None) => {
                    return Err(format!("[[speed]] `{}`: polls must be [from, to]", w.name))
                }
                (_, None, Some(_), Some(_)) => {
                    if w.max_polls.is_none() {
                        return Err(format!(
                            "[[speed]] `{}`: a window by conditions needs max_polls (where the \
                             locating replay gives up)",
                            w.name
                        ));
                    }
                    if self.build(&w.build).is_some_and(|b| b.map.is_none()) {
                        return Err(format!(
                            "[[speed]] `{}`: build `{}` declares no map to resolve the conditions",
                            w.name, w.build
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "[[speed]] `{}`: a window is tape + polls, or from + until conditions",
                        w.name
                    ))
                }
            }
        }
        for h in &self.hosts {
            if let Some(b) = &h.build {
                known(b, "[[host]]")?;
            }
            if h.command.is_empty() {
                return Err(format!("[[host]] `{}` has no command", h.name));
            }
            if let Some(u) = h
                .uses
                .iter()
                .find(|u| !["sdk", "gate", "frontend"].contains(&u.as_str()))
            {
                return Err(format!(
                    "[[host]] `{}`: uses `{u}` is not sdk, gate or frontend",
                    h.name
                ));
            }
            if let Some(m) = &h.metric {
                if !matches!(m.better.as_deref(), None | Some("lower" | "higher")) {
                    return Err(format!(
                        "[[host]] `{}`: metric better is lower or higher",
                        h.name
                    ));
                }
                if m.tolerance != 0 && m.reason.as_deref().is_none_or(str::is_empty) {
                    return Err(format!(
                        "[[host]] `{}`: a metric tolerance needs `reason`",
                        h.name
                    ));
                }
            }
        }
        for st in &self.stacks {
            if st.max_growth != 0 && st.reason.as_deref().is_none_or(str::is_empty) {
                return Err(format!(
                    "[[stack]] `{}`: max_growth other than 0 needs `reason`",
                    st.journey
                ));
            }
            if self.journeys.is_none() {
                return Err("[[stack]] measures a journey: declare [journeys]".into());
            }
            if let Some(b) = &st.build {
                known(b, "[[stack]]")?;
                if st.top.is_some() {
                    return Err(format!(
                        "[[stack]] `{}`: build reads the stack top from that link; top is for a \
                         program without one: name one of them",
                        st.journey
                    ));
                }
                if self.build(b).is_some_and(|b| b.map.is_none()) {
                    return Err(format!(
                        "[[stack]] `{}`: build `{b}` declares no map to read STACK_INIT from",
                        st.journey
                    ));
                }
            }
        }
        let manifests = self.builds.iter().filter(|b| b.members.is_some()).count();
        if manifests > 1 {
            return Err("only one [[build]] may declare `members`".into());
        }
        if !self.requires.is_empty() && manifests == 0 {
            return Err(
                "[[requires]] needs a [[build]] that declares `members` (the manifest of \
                        the programs it carries)"
                    .into(),
            );
        }
        let mut ids = std::collections::BTreeSet::new();
        for r in &self.requires {
            if !ids.insert(r.id.as_str()) {
                return Err(format!("[[requires]] `{}` is listed twice", r.id));
            }
            if r.result.len() != 64 || !r.result.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "[[requires]] `{}`: result must be the 64-hex sha256 of a result file",
                    r.id
                ));
            }
            if !r.file.contains('/') {
                return Err(format!(
                    "[[requires]] `{}`: file is `<build>/<file>` as the member's result names it",
                    r.id
                ));
            }
        }
        if let Some(j) = &self.journeys {
            if !j.instances.is_empty() && self.requires.is_empty() {
                return Err(
                    "[journeys] instances are played once per [[requires]] entry: \
                            declare [[requires]]"
                        .into(),
                );
            }
            for (id, pads) in &j.pads {
                for pad in pads {
                    match pad.as_str() {
                        "digital" | "dualshock" => {}
                        "analog" if j.forced_analog.contains_key(id) => {}
                        "analog" => {
                            return Err(format!(
                                "[journeys] pads `{id}`: analog needs the journey in forced_analog \
                                 with its reason"
                            ))
                        }
                        other => {
                            return Err(format!(
                            "[journeys] pads `{id}`: `{other}` is not digital, dualshock or analog"
                        ))
                        }
                    }
                }
            }
            for (id, only) in &j.only_on {
                if only.models.is_empty() || only.reason.trim().is_empty() {
                    return Err(format!(
                        "[journeys] only_on `{id}`: needs models and a reason"
                    ));
                }
            }
        }
        if let Some(cross) = &self.cross {
            if let Some(boot) = &cross.boot {
                known(&boot.build, "[cross] boot")?;
                match (boot.until.is_empty(), boot.after) {
                    (true, None) => {}
                    (false, Some(_)) => {
                        if self.build(&boot.build).is_some_and(|b| b.map.is_none()) {
                            return Err(format!(
                                "[cross] boot: build `{}` declares no map to resolve `until`",
                                boot.build
                            ));
                        }
                    }
                    (false, None) => {
                        return Err("[cross] boot: until needs `after` (the frames the game is \
                                    watched once it started)"
                            .into())
                    }
                    (true, Some(_)) => {
                        return Err("[cross] boot: after counts from `until`: declare it".into())
                    }
                }
            }
            for pad in &cross.pads {
                if !["digital", "dualshock"].contains(&pad.as_str()) {
                    return Err(format!("[cross] pads: `{pad}` is not digital or dualshock"));
                }
            }
        }
        for b in &self.builds {
            if b.layout == Some(false) && b.layout_reason.as_deref().is_none_or(str::is_empty) {
                return Err(format!(
                    "build `{}`: layout = false needs `layout_reason`",
                    b.name
                ));
            }
        }
        for b in &self.builds {
            if let Some(min) = b.bind_min {
                if !(0.0..=100.0).contains(&min) {
                    return Err(format!("build `{}`: bind_min is a percentage", b.name));
                }
                if min < 100.0 && b.bind_reason.as_deref().is_none_or(str::is_empty) {
                    return Err(format!(
                        "build `{}`: bind_min under 100 needs `bind_reason`",
                        b.name
                    ));
                }
            }
        }
        let needs_disc = |name: &str, what: &str| match self.build(name) {
            Some(b) if b.disc.is_empty() => Err(format!(
                "{what} names build `{name}`, which declares no disc"
            )),
            _ => Ok(()),
        };
        for w in &self.windows {
            needs_disc(&w.build, "[[speed]]")?;
        }
        if let Some(boot) = self.cross.as_ref().and_then(|c| c.boot.as_ref()) {
            needs_disc(&boot.build, "[cross] boot")?;
        }
        for i in &self.installs {
            known(&i.build, "[[install]]")?;
            needs_disc(&i.build, "[[install]]")?;
        }
        for c in &self.cleans {
            known(&c.build, "[[clean]]")?;
            for b in &c.before {
                known(b, "[[clean]] before")?;
            }
            if c.compare.is_empty() {
                needs_disc(&c.build, "[[clean]] without compare")?;
            }
        }
        Ok(())
    }

    /// The build of this name.
    pub fn build(&self, name: &str) -> Option<&Build> {
        self.builds.iter().find(|b| b.name == name)
    }
}

impl Build {
    /// The output directory.
    pub fn out_dir(&self, repo: &Path) -> PathBuf {
        repo.join(&self.out)
    }

    /// The disc cue.
    pub fn cue(&self, repo: &Path) -> PathBuf {
        self.out_dir(repo).join(&self.disc)
    }

    /// The link map, when declared.
    pub fn map_path(&self, repo: &Path) -> Option<PathBuf> {
        self.map.as_ref().map(|m| self.out_dir(repo).join(m))
    }

    /// The image of this name.
    pub fn image(&self, name: &str) -> Option<&Image> {
        self.images.iter().find(|i| i.name == name)
    }
}

impl Size {
    /// The baseline and fact name of this bar: the build, or `<build>:<image>`.
    pub fn key(&self) -> String {
        match &self.image {
            Some(image) => format!("{}:{image}", self.build),
            None => self.build.clone(),
        }
    }

    /// The link map this bar measures.
    pub fn map_path(&self, game: &Game, repo: &Path) -> Option<PathBuf> {
        let build = game.build(&self.build)?;
        match &self.image {
            Some(name) => Some(build.out_dir(repo).join(&build.image(name)?.map)),
            None => build.map_path(repo),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
schema = 1
name = "Game"
repository = "o/game"
[[build]]
name = "ship"
out = "build/ship"
commands = [["make", "build"]]
disc = "Game.cue"
map = "link.map"
"#;

    #[test]
    fn extra_pads_and_hold_last_are_checked() {
        let j = "[journeys]\nfiles = \"tests/*.toml\"\n";
        let ok = format!("{MINIMAL}{j}pads = {{ menu = [\"digital\"] }}\n");
        assert_eq!(
            Game::parse(&ok).unwrap().journeys.unwrap().pads["menu"],
            ["digital"]
        );
        let analog = format!("{MINIMAL}{j}pads = {{ menu = [\"analog\"] }}\n");
        assert!(Game::parse(&analog).unwrap_err().contains("forced_analog"));
        let w = "[[speed]]\nname = \"w\"\nbuild = \"ship\"\ntape = \"t.pxtape\"\npolls = [1, 9]\n\
                 hold_last = true\nmodels = [\"mister\"]\n";
        let game = Game::parse(&format!("{MINIMAL}{w}")).unwrap();
        assert_eq!(game.windows[0].frontend_args(), ["--input-tape-hold-last"]);
        assert_eq!(game.windows[0].rows()[1].0, "w@mister");
        let bad = format!("{MINIMAL}{}", w.replace("mister", "pentium"));
        assert!(Game::parse(&bad).unwrap_err().contains("pentium"));
    }

    #[test]
    fn clean_is_a_table_or_an_array_and_a_cook_has_no_disc() {
        let one = format!("{MINIMAL}[clean]\nbuild = \"ship\"\n");
        assert_eq!(Game::parse(&one).unwrap().cleans.len(), 1);
        let cook =
            "[[build]]\nname = \"cook\"\nout = \"build/cook\"\ncommands = [[\"make\", \"cook\"]]\n";
        let two = format!(
            "{MINIMAL}{cook}[[clean]]\nbuild = \"ship\"\nbefore = [\"cook\"]\nhistory = true\n\
             [[clean]]\nbuild = \"cook\"\ncompare = [\"data.bin\"]\n"
        );
        let game = Game::parse(&two).unwrap();
        assert_eq!(game.cleans.len(), 2);
        assert_eq!(game.cleans[0].before, ["cook"]);
        let bad = format!("{MINIMAL}{cook}[[install]]\nbuild = \"cook\"\n");
        assert!(Game::parse(&bad).unwrap_err().contains("no disc"));
        let reasonless = MINIMAL.replace("map = ", "bind_min = 99.0\nmap = ");
        assert!(Game::parse(&reasonless)
            .unwrap_err()
            .contains("bind_reason"));
    }

    #[test]
    fn a_stack_bar_reads_its_top_from_a_build_with_a_map() {
        let j = "[journeys]\nfiles = \"tests/*.toml\"\n";
        let st = "[[stack]]\njourney = \"play\"\ncheckpoint = \"deep\"\n";
        let ok = format!("{MINIMAL}{j}{st}build = \"ship\"\n");
        assert_eq!(
            Game::parse(&ok).unwrap().stacks[0].build.as_deref(),
            Some("ship")
        );
        let both = format!("{ok}top = 0x801FD700\n");
        assert!(Game::parse(&both).unwrap_err().contains("name one of them"));
        let unknown = format!("{MINIMAL}{j}{st}build = \"gate\"\n");
        assert!(Game::parse(&unknown).unwrap_err().contains("`gate`"));
        let mapless = MINIMAL.replace("map = \"link.map\"\n", "");
        let mapless = format!("{mapless}{j}{st}build = \"ship\"\n");
        assert!(Game::parse(&mapless).unwrap_err().contains("no map"));
        // Unchanged for a game that names neither.
        let plain = format!("{MINIMAL}{j}{st}");
        assert!(Game::parse(&plain).unwrap().stacks[0].build.is_none());
    }

    #[test]
    fn a_boot_run_with_until_needs_after_and_a_map() {
        let c = "[cross]\nsmoke = []\n[cross.boot]\nbuild = \"ship\"\nframes = 3000\n";
        // Today's form is unchanged.
        let plain = Game::parse(&format!("{MINIMAL}{c}")).unwrap();
        assert!(plain.cross.unwrap().boot.unwrap().until.is_empty());
        let until = "until = { kind = \"ram\", sym = \"GAME_FRAMES\", op = \"ge\", value = 1 }\n";
        assert!(Game::parse(&format!("{MINIMAL}{c}{until}"))
            .unwrap_err()
            .contains("after"));
        let ok = Game::parse(&format!("{MINIMAL}{c}{until}after = 600\n")).unwrap();
        let boot = ok.cross.unwrap().boot.unwrap();
        assert_eq!((boot.until.len(), boot.after), (1, Some(600)));
        // Several conditions (a guard against bytes of another program at the same address).
        let two = "until = [{ kind = \"ram\", sym = \"A\", value = 1 }, { kind = \"ram\", sym = \"B\", value = 2 }]\nafter = 60\n";
        assert_eq!(
            Game::parse(&format!("{MINIMAL}{c}{two}"))
                .unwrap()
                .cross
                .unwrap()
                .boot
                .unwrap()
                .until
                .len(),
            2
        );
        assert!(Game::parse(&format!("{MINIMAL}{c}after = 60\n"))
            .unwrap_err()
            .contains("until"));
        let mapless = MINIMAL.replace("map = \"link.map\"\n", "");
        assert!(Game::parse(&format!("{mapless}{c}{until}after = 600\n"))
            .unwrap_err()
            .contains("no map"));
    }

    #[test]
    fn images_are_named_and_a_size_bar_can_judge_one() {
        let images = "images = [{ name = \"core\", map = \"core.map\", exe = \"core.exe\" }]\n";
        let text = format!("{MINIMAL}{images}[[size]]\nbuild = \"ship\"\n[[size]]\nbuild = \"ship\"\nimage = \"core\"\nmax_bytes = 10240\n");
        let game = Game::parse(&text).unwrap();
        assert_eq!(game.builds[0].images[0].exe.as_deref(), Some("core.exe"));
        assert_eq!(game.sizes[0].key(), "ship");
        assert_eq!(game.sizes[1].key(), "ship:core");
        let repo = Path::new("/r");
        assert_eq!(
            game.sizes[1].map_path(&game, repo),
            Some(PathBuf::from("/r/build/ship/core.map"))
        );
        assert_eq!(
            game.sizes[0].map_path(&game, repo),
            Some(PathBuf::from("/r/build/ship/link.map"))
        );
        let unknown = text.replace("image = \"core\"", "image = \"boot\"");
        assert!(Game::parse(&unknown)
            .unwrap_err()
            .contains("no image `boot`"));
        let bad = format!("{MINIMAL}images = [{{ name = \"Core\", map = \"c.map\" }}]\n");
        assert!(Game::parse(&bad).unwrap_err().contains("lowercase"));
        let twice = format!(
            "{MINIMAL}images = [{{ name = \"core\", map = \"a.map\" }}, {{ name = \"core\", map = \"b.map\" }}]\n"
        );
        assert!(Game::parse(&twice).unwrap_err().contains("unique"));
        // A build without images serializes as before (its receipt key is unchanged).
        let plain = Game::parse(MINIMAL).unwrap();
        assert!(!serde_json::to_string(&plain.builds[0])
            .unwrap()
            .contains("images"));
    }

    #[test]
    fn a_minimal_file_loads() {
        let game = Game::parse(MINIMAL).unwrap();
        assert_eq!(game.builds[0].exclude.len(), 3);
    }

    #[test]
    fn a_size_tolerance_needs_a_reason() {
        let text = format!("{MINIMAL}[[size]]\nbuild = \"ship\"\ntext_max_growth = 64\n");
        assert!(Game::parse(&text).unwrap_err().contains("reason"));
        let text = format!("{text}reason = \"measured\"\n");
        assert!(Game::parse(&text).is_ok());
    }

    #[test]
    fn a_stage_must_name_a_declared_build() {
        let text = format!("{MINIMAL}[[size]]\nbuild = \"gate\"\n");
        assert!(Game::parse(&text).unwrap_err().contains("`gate`"));
    }

    #[test]
    fn a_window_needs_offset_zero_and_unknown_keys_fail() {
        let w = "[[speed]]\nname = \"t\"\nbuild = \"ship\"\ntape = \"t.pxtape\"\npolls = [1, 9]\n";
        assert!(Game::parse(&format!("{MINIMAL}{w}offsets = [12]\n")).is_err());
        assert!(Game::parse(&format!("{MINIMAL}{w}")).is_ok());
        assert!(Game::parse(&format!("{MINIMAL}{w}offset = [0]\n")).is_err());
        let c = "[[speed]]\nname = \"c\"\nbuild = \"ship\"\nfrom = { kind = \"ram\", sym = \"S\", value = 3 }\nuntil = { kind = \"ram\", sym = \"S\", value = 4 }\n";
        assert!(
            Game::parse(&format!("{MINIMAL}{c}"))
                .unwrap_err()
                .contains("max_polls"),
            "a window by conditions needs a limit"
        );
        assert!(
            Game::parse(&format!("{MINIMAL}{c}max_polls = 900\n")).is_ok(),
            "a window by conditions loads"
        );
        let half = "[[speed]]\nname = \"c\"\nbuild = \"ship\"\nfrom = { kind = \"ram\", sym = \"S\", value = 3 }\n";
        assert!(Game::parse(&format!("{MINIMAL}{half}")).is_err());
    }

    #[test]
    fn an_anchored_window_needs_a_tape_a_limit_and_no_press_schedule() {
        let w = "[[speed]]\nname = \"t\"\nbuild = \"ship\"\ntape = \"t.pxtape\"\npolls = [1, 9]\n";
        let anchor = "anchor = { kind = \"ram\", sym = \"GAME_FRAMES\", op = \"ge\", value = 1 }\n";
        assert!(Game::parse(&format!("{MINIMAL}{w}")).unwrap().windows[0]
            .anchor
            .is_empty());
        assert!(Game::parse(&format!("{MINIMAL}{w}{anchor}"))
            .unwrap_err()
            .contains("anchor_max_polls"));
        let ok = Game::parse(&format!("{MINIMAL}{w}{anchor}anchor_max_polls = 2000\n")).unwrap();
        assert_eq!(ok.windows[0].anchor.len(), 1);
        let press = format!(
            "{MINIMAL}{w}{anchor}anchor_max_polls = 2000\nlaunch_args = [\"--press\", \"1400:cross:20\"]\n"
        );
        assert!(Game::parse(&press).unwrap_err().contains("--press"));
        assert!(Game::parse(&format!("{MINIMAL}{w}anchor_max_polls = 9\n"))
            .unwrap_err()
            .contains("without anchor"));
    }
}
