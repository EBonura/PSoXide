//! Journeys are the gate's own TOML. This module does not run them (the gate does); it lints
//! them for the harness's rules and turns the cross-emulator subset into a plan the ReARMed and
//! Redux adapters replay.
//!
//! Rules: every journey names its `pad` (the gate's default is a forced-analog pad, which hid a
//! defect once); the number of open-loop steps (fixed waits and frame-counted holds) is a ratchet
//! stored in the baseline; the smoke subset is closed-loop only: press until a RAM condition,
//! wait until a RAM condition, check RAM at a named checkpoint.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Value;

/// What the harness needs to know about one journey file.
#[derive(Debug, Clone)]
pub struct Info {
    /// File stem: the name the game file and the baseline use.
    pub id: String,
    pub path: PathBuf,
    /// `pad` as written, `None` when the journey relies on the gate's default.
    pub pad: Option<String>,
    /// Steps that wait or hold for a fixed number of video frames.
    pub open_loop: u32,
    /// Steps that wait or hold until a free-running clock reaches an absolute value: a fixed
    /// wait under another name, counted in the ratchet beside `open_loop`.
    pub clock_waits: u32,
    /// Disc paths the journey boots (first existing wins in the gate).
    pub discs: Vec<String>,
    /// The journey's own `name` (the gate names its golden folder after it).
    pub name: Option<String>,
}

/// Read a journey file far enough to lint it.
pub fn inspect(path: &Path, clocks: &[String]) -> Result<Info, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pad = value.get("pad").and_then(Value::as_str).map(str::to_string);
    let mut open_loop = 0;
    let mut clock_waits = 0;
    for list in [value.get("step")].into_iter().flatten().chain(
        value
            .get("block")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| b.get("step")),
    ) {
        open_loop += count_open_loop(list);
        clock_waits += count_clock_waits(list, clocks);
    }
    Ok(Info {
        id,
        path: path.to_path_buf(),
        pad,
        open_loop,
        clock_waits,
        discs: strings(value.get("disc")),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Open-loop steps in a step list, bodies included.
fn count_open_loop(steps: &Value) -> u32 {
    let Some(steps) = steps.as_array() else {
        return 0;
    };
    let mut count = 0;
    for step in steps {
        if is_open_loop(step) {
            count += 1;
        }
        if let Some(body) = step.get("body") {
            count += count_open_loop(body);
        }
    }
    count
}

/// The SDK's free-running clocks: they advance with time, not with game state.
pub const SDK_CLOCKS: &[&str] = &[
    "__psx_rt_vblank_count",
    "__psx_rt_present_kick_count",
    "__psx_rt_present_thread_kick_count",
];

/// Steps (bodies included) whose `wait_until` or `hold_until` compares a free-running clock
/// (`SDK_CLOCKS` plus the game's `clocks`) with an absolute value.
fn count_clock_waits(steps: &Value, clocks: &[String]) -> u32 {
    let Some(steps) = steps.as_array() else {
        return 0;
    };
    let mut count = 0;
    for step in steps {
        let on_clock = ["wait_until", "hold_until"]
            .iter()
            .filter_map(|k| step.get(k))
            .any(|c| names_clock(c, clocks));
        count += u32::from(on_clock);
        if let Some(body) = step.get("body") {
            count += count_clock_waits(body, clocks);
        }
    }
    count
}

/// Whether a condition (or any condition nested in it) reads a clock symbol.
fn names_clock(condition: &Value, clocks: &[String]) -> bool {
    match condition {
        Value::Table(t) => {
            let own = t.get("sym").and_then(Value::as_str).is_some_and(|s| {
                let s = s.rsplit("::").next().unwrap_or(s);
                SDK_CLOCKS.contains(&s) || clocks.iter().any(|c| c == s)
            });
            own || t.values().any(|v| names_clock(v, clocks))
        }
        Value::Array(items) => items.iter().any(|v| names_clock(v, clocks)),
        _ => false,
    }
}

/// A step is open-loop when it rests a fixed number of video frames (`wait`) or holds input for
/// one (`hold`, `repeat`, or the default 4-tick hold) instead of until a condition. Counting in
/// the game's own pad polls (`sync = "poll"`) is game time, so it is closed-loop.
fn is_open_loop(step: &Value) -> bool {
    let at_poll = step.get("sync").and_then(Value::as_str) == Some("poll");
    if at_poll {
        return false;
    }
    let waits = step
        .get("wait")
        .and_then(Value::as_integer)
        .is_some_and(|w| w > 0);
    let has_input = ["press", "lstick", "rstick", "other"].iter().any(|k| {
        step.get(k)
            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
    });
    let held_until = step.get("hold_until").is_some();
    waits || (has_input && !held_until)
}

/// The pad rule: explicit, and the forced-analog pad only where the game file allows it.
pub fn check_pad(info: &Info, forced_analog: &BTreeMap<String, String>) -> Result<(), String> {
    match info.pad.as_deref() {
        None => Err(format!(
            "{} has no `pad`: say `pad = \"dualshock\"` (starts digital, switches when the game \
             asks) or `pad = \"digital\"`; the gate's default is a forced-analog pad",
            info.id
        )),
        Some("analog") if !forced_analog.contains_key(&info.id) => Err(format!(
            "{} forces an analog pad: list it under [journeys] forced_analog with the reason, or \
             use dualshock",
            info.id
        )),
        Some("analog" | "dualshock" | "digital") => Ok(()),
        Some(other) => Err(format!("{}: unknown pad `{other}`", info.id)),
    }
}

// ---- the cross-emulator subset ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Smoke {
    name: String,
    #[serde(default)]
    title: Option<String>,
    disc: OneOrMany,
    symbols: OneOrMany,
    pad: String,
    #[serde(default)]
    max_ticks: Option<u64>,
    #[serde(rename = "step")]
    steps: Vec<SmokeStep>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn list(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SmokeStep {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    press: Vec<String>,
    #[serde(default)]
    lstick: Option<[u8; 2]>,
    #[serde(default)]
    hold_until: Option<Cond>,
    #[serde(default)]
    wait_until: Option<Cond>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    checkpoint: Option<String>,
    #[serde(default)]
    golden: Option<bool>,
    #[serde(default, rename = "assert")]
    asserts: Vec<Cond>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cond {
    kind: String,
    sym: String,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    size: Option<u8>,
    #[serde(default)]
    signed: bool,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    value: Option<i64>,
    #[serde(default)]
    value_hi: Option<i64>,
    #[serde(default)]
    note: Option<String>,
}

/// A RAM condition with its address resolved: `size` bytes at `addr`, compared with `op`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Probe {
    /// The condition as the journey wrote it, for messages.
    pub text: String,
    pub addr: u32,
    pub size: u8,
    pub signed: bool,
    pub op: String,
    pub value: i64,
    pub value_hi: i64,
}

impl Probe {
    /// The value the condition compares: `size` bytes of `raw`, signed when `signed`.
    pub fn value_of(&self, raw: u32) -> i64 {
        match (self.size, self.signed) {
            (1, false) => i64::from(raw as u8),
            (1, true) => i64::from(raw as u8 as i8),
            (2, false) => i64::from(raw as u16),
            (2, true) => i64::from(raw as u16 as i16),
            (_, false) => i64::from(raw),
            (_, true) => i64::from(raw as i32),
        }
    }

    /// The condition and the value read, for a failure message: `SYM eq 1 (read 6)`.
    pub fn describe(&self, raw: u32) -> String {
        format!("{} (read {})", self.text, self.value_of(raw))
    }

    /// Whether the condition holds for `raw`, the little-endian bytes read at `addr`.
    pub fn holds(&self, raw: u32) -> bool {
        let v = self.value_of(raw);
        match self.op.as_str() {
            "eq" => v == self.value,
            "ne" => v != self.value,
            "lt" => v < self.value,
            "le" => v <= self.value,
            "gt" => v > self.value,
            "ge" => v >= self.value,
            "between" => v >= self.value && v <= self.value_hi,
            "zero" => v == 0,
            "nonzero" => v != 0,
            _ => false,
        }
    }
}

/// What one plan step does while its condition is false.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Hold these buttons (and the left stick) until the condition holds, then release.
    Hold {
        buttons: Vec<String>,
        lstick: Option<[u8; 2]>,
    },
    /// Run with no input until the condition holds.
    Wait,
    /// Check now: every probe must hold (a named checkpoint).
    Check { name: String },
}

/// One resolved step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub label: String,
    pub action: Action,
    pub probes: Vec<Probe>,
    /// Frames before the step fails (0 for a check).
    pub timeout: u64,
}

/// A smoke journey, resolved for one disc and one pad: what the adapters replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub name: String,
    pub disc: PathBuf,
    /// `digital` or `dualshock`.
    pub pad: String,
    pub steps: Vec<PlanStep>,
    pub max_frames: u64,
    pub min_distinct: u32,
    /// Frames to keep running with no input after the last step (a boot run has no steps).
    #[serde(default)]
    pub idle_frames: u64,
    /// Count `min_distinct` only over the frames after this checkpoint (a boot run's proof that
    /// the game started), not over the whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_after: Option<String>,
}

const BUTTONS: &[&str] = &[
    "cross", "circle", "square", "triangle", "start", "select", "up", "down", "left", "right",
    "l1", "r1", "l2", "r2", "l3", "r3",
];
const OPS: &[&str] = &[
    "eq", "ne", "lt", "le", "gt", "ge", "between", "zero", "nonzero",
];

/// Load a smoke journey and resolve it against `repo` for `pad`. Anything outside the subset is
/// an error that names the key, so a smoke cannot mean less on one emulator than on another.
pub fn plan(repo: &Path, path: &Path, pad: &str, min_distinct: u32) -> Result<Plan, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let smoke: Smoke = toml::from_str(&text).map_err(|e| {
        format!(
            "{}: {e}\n  a smoke journey uses only press + hold_until, wait_until and checkpoints \
             with kind = \"ram\" asserts",
            path.display()
        )
    })?;
    let _ = &smoke.title;
    if !["digital", "dualshock"].contains(&smoke.pad.as_str()) {
        return Err(format!(
            "{}: a smoke journey runs pad = \"digital\" or \"dualshock\"",
            path.display()
        ));
    }
    let disc = smoke
        .disc
        .list()
        .first()
        .map(|d| repo.join(d))
        .ok_or_else(|| format!("{}: no disc", path.display()))?;
    let mut symbols = Symbols::default();
    for s in smoke.symbols.list() {
        symbols.load(&repo.join(&s))?;
    }
    let mut steps = Vec::new();
    for (i, step) in smoke.steps.iter().enumerate() {
        let at = |m: String| format!("{} step {}: {m}", path.display(), i + 1);
        let label = step
            .label
            .clone()
            .or_else(|| step.checkpoint.clone())
            .unwrap_or_else(|| format!("step {}", i + 1));
        let kinds = [
            step.hold_until.is_some(),
            step.wait_until.is_some(),
            step.checkpoint.is_some(),
        ];
        if kinds.iter().filter(|k| **k).count() != 1 {
            return Err(at(
                "a smoke step is exactly one of hold_until, wait_until or checkpoint".into(),
            ));
        }
        for b in &step.press {
            if !BUTTONS.contains(&b.as_str()) {
                return Err(at(format!("unknown button `{b}`")));
            }
        }
        let resolve = |c: &Cond| resolve(c, &symbols).map_err(at);
        let (action, probes, timeout) = if let Some(c) = &step.hold_until {
            if step.press.is_empty() && step.lstick.is_none() {
                return Err(at("hold_until needs press or lstick".into()));
            }
            let timeout = step
                .timeout
                .ok_or_else(|| at("hold_until needs timeout".into()))?;
            (
                Action::Hold {
                    buttons: step.press.clone(),
                    lstick: step.lstick,
                },
                vec![resolve(c)?],
                timeout,
            )
        } else if let Some(c) = &step.wait_until {
            if !step.press.is_empty() || step.lstick.is_some() {
                return Err(at("wait_until takes no input".into()));
            }
            let timeout = step
                .timeout
                .ok_or_else(|| at("wait_until needs timeout".into()))?;
            (Action::Wait, vec![resolve(c)?], timeout)
        } else {
            let name = step.checkpoint.clone().unwrap_or_default();
            if step.golden != Some(false) {
                return Err(at(
                    "a smoke checkpoint compares state, never pixels: set golden = false".into(),
                ));
            }
            if step.asserts.is_empty() {
                return Err(at("a smoke checkpoint needs at least one ram assert".into()));
            }
            if !step.press.is_empty() || step.lstick.is_some() || step.timeout.is_some() {
                return Err(at("a checkpoint takes no input and no timeout".into()));
            }
            let probes = step
                .asserts
                .iter()
                .map(resolve)
                .collect::<Result<Vec<_>, _>>()?;
            (Action::Check { name }, probes, 0)
        };
        steps.push(PlanStep {
            label,
            action,
            probes,
            timeout,
        });
    }
    let budget: u64 = steps.iter().map(|s| s.timeout).sum();
    Ok(Plan {
        name: smoke.name,
        disc,
        pad: pad.to_string(),
        steps,
        max_frames: smoke.max_ticks.unwrap_or(budget).min(budget.max(1)),
        min_distinct,
        idle_frames: 0,
        distinct_after: None,
    })
}

/// A condition written as a TOML table in the journey syntax (`{ kind = "ram", sym, offset,
/// size, signed, op, value, value_hi }`), resolved against `symbols`.
pub fn condition(table: &toml::Table, symbols: &Symbols) -> Result<Probe, String> {
    let c: Cond = Value::Table(table.clone())
        .try_into()
        .map_err(|e| format!("condition {table}: {e}"))?;
    resolve(&c, symbols)
}

fn resolve(c: &Cond, symbols: &Symbols) -> Result<Probe, String> {
    if c.kind != "ram" {
        return Err(format!(
            "assert kind `{}`: a smoke reads RAM only (kind = \"ram\")",
            c.kind
        ));
    }
    let op = c.op.clone().unwrap_or_else(|| "eq".into());
    if !OPS.contains(&op.as_str()) {
        return Err(format!("unknown op `{op}`"));
    }
    let size = c.size.unwrap_or(4);
    if !matches!(size, 1 | 2 | 4) {
        return Err("size must be 1, 2 or 4".into());
    }
    let value = match (op.as_str(), c.value) {
        ("zero" | "nonzero", _) => 0,
        (_, Some(v)) => v,
        (_, None) => return Err(format!("op `{op}` needs value")),
    };
    if op == "between" && c.value_hi.is_none() {
        return Err("between needs value_hi".into());
    }
    let base = symbols.resolve(&c.sym)?;
    let addr = i64::from(base) + c.offset.unwrap_or(0);
    let addr = u32::try_from(addr).map_err(|_| format!("{} + offset is out of range", c.sym))?;
    if addr % u32::from(size) != 0 {
        return Err(format!(
            "{} + {} is not {size}-byte aligned",
            c.sym,
            c.offset.unwrap_or(0)
        ));
    }
    let _ = &c.note;
    Ok(Probe {
        text: format!(
            "{}{} {op} {value}",
            c.sym,
            c.offset.map_or(String::new(), |o| format!("+{o}"))
        ),
        addr,
        size,
        signed: c.signed,
        op,
        value,
        value_hi: c.value_hi.unwrap_or(0),
    })
}

/// Symbol addresses by name, from lld maps and `address name` lists.
#[derive(Default, Debug)]
pub struct Symbols {
    by_name: BTreeMap<String, Vec<u32>>,
}

impl Symbols {
    /// Add every symbol of a map or a list.
    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "read symbols {}: {e} (build the artifact that writes it first)",
                path.display()
            )
        })?;
        if text.starts_with("             VMA") || text.contains("     VMA      LMA") {
            let map = psoxide_size::map::parse(&text);
            for section in &map.sections {
                for sym in &section.syms {
                    self.insert(&sym.name, sym.vma);
                }
            }
        } else {
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let (addr, name) = line.split_once(char::is_whitespace).ok_or_else(|| {
                    format!("{}:{}: expected `address name`", path.display(), n + 1)
                })?;
                let addr = u32::from_str_radix(addr.trim_start_matches("0x"), 16)
                    .map_err(|_| format!("{}:{}: bad address", path.display(), n + 1))?;
                self.insert(name.trim(), addr);
            }
        }
        Ok(())
    }

    fn insert(&mut self, name: &str, addr: u32) {
        // lld names a symbol in a numbered section `name (.0)`: both spellings resolve.
        let stripped = crate::stage::build::strip_section_suffix(name);
        if stripped != name {
            self.insert(stripped, addr);
        }
        let entry = self.by_name.entry(name.to_string()).or_default();
        if !entry.contains(&addr) {
            entry.push(addr);
        }
    }

    /// The address of `name`; two different addresses for one name is an error, never a pick.
    pub fn resolve(&self, name: &str) -> Result<u32, String> {
        match self.by_name.get(name).map(Vec::as_slice) {
            Some([addr]) => Ok(*addr),
            Some(many) if !many.is_empty() => Err(format!(
                "symbol `{name}` has {} addresses; name it unambiguously",
                many.len()
            )),
            _ => self.resolve_suffix(name),
        }
    }

    /// The gate's second rule: a unique `::name` path suffix (`STATE` finds `game::STATE`).
    fn resolve_suffix(&self, name: &str) -> Result<u32, String> {
        let suffix = format!("::{name}");
        let mut hits: Vec<(&String, u32)> = self
            .by_name
            .iter()
            .filter(|(k, _)| k.ends_with(&suffix))
            .flat_map(|(k, v)| v.iter().map(move |a| (k, *a)))
            .collect();
        hits.sort_by_key(|(_, a)| *a);
        hits.dedup_by_key(|(_, a)| *a);
        match hits.as_slice() {
            [(_, addr)] => Ok(*addr),
            [] => Err(format!("symbol `{name}` is not in the journey's symbols")),
            many => Err(format!(
                "symbol `{name}` is ambiguous: {}",
                many.iter()
                    .take(6)
                    .map(|(k, a)| format!("{k}@{a:#x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn fixed_waits_and_frame_holds_are_open_loop() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "j.toml",
            r#"
name = "j"
pad = "dualshock"
disc = "build/x.cue"
[[step]]
wait = 90
[[step]]
press = ["cross"]
hold = 4
[[step]]
press = ["cross"]
sync = "poll"
hold = 1
[[step]]
press = ["right"]
hold_until = { kind = "ram", sym = "S", op = "gt", value = 0 }
timeout = 60
[[step]]
wait_until = { kind = "ram", sym = "S", op = "eq", value = 1 }
[[step]]
wait = 0
checkpoint = "c"
[[step]]
label = "loop"
until = { kind = "ram", sym = "S", op = "eq", value = 2 }
max_iter = 3
timeout = 100
[[step.body]]
press = ["down"]
"#,
        );
        let info = inspect(&p, &[]).unwrap();
        assert_eq!(
            info.open_loop, 3,
            "wait 90, the 4-tick hold, the frame-counted body press"
        );
        assert_eq!(info.pad.as_deref(), Some("dualshock"));
        assert_eq!(info.discs, ["build/x.cue"]);
    }

    #[test]
    fn a_failure_names_the_value_read() {
        let p = Probe {
            text: "INTRO_MOVIE+0 eq 1".into(),
            addr: 0x8001_0000,
            size: 2,
            signed: true,
            op: "eq".into(),
            value: 1,
            value_hi: 0,
        };
        assert_eq!(p.describe(0xdead_0006), "INTRO_MOVIE+0 eq 1 (read 6)");
        assert_eq!(p.value_of(0x0000_ffff), -1);
    }

    #[test]
    fn a_wait_on_a_free_running_clock_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "j.toml",
            "name = \"j\"\npad = \"dualshock\"\n\
             [[step]]\nwait_until = { kind = \"ram\", sym = \"__psx_rt_vblank_count\", op = \"ge\", value = 300 }\n\
             [[step]]\npress = [\"start\"]\nhold_until = { kind = \"ram\", sym = \"game::FRAMES\", op = \"ge\", value = 9 }\n\
             [[step]]\nwait_until = { kind = \"ram\", sym = \"game::STATE\", value = 2 }\n",
        );
        assert_eq!(inspect(&p, &[]).unwrap().clock_waits, 1);
        assert_eq!(inspect(&p, &["FRAMES".into()]).unwrap().clock_waits, 2);
        assert_eq!(inspect(&p, &[]).unwrap().open_loop, 0);
    }

    #[test]
    fn symbols_resolve_by_section_stripped_name_and_unique_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "s.txt",
            "80010000 cs_psx::SIM_NOW (.0)\n80010010 cs_psx::world::ARENA\n80010020 a::X\n80010030 b::X\n",
        );
        let mut s = Symbols::default();
        s.load(&p).unwrap();
        assert_eq!(s.resolve("cs_psx::SIM_NOW").unwrap(), 0x8001_0000);
        assert_eq!(s.resolve("cs_psx::SIM_NOW (.0)").unwrap(), 0x8001_0000);
        assert_eq!(s.resolve("ARENA").unwrap(), 0x8001_0010);
        assert!(s.resolve("X").unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn the_pad_must_be_named() {
        let mut info = Info {
            id: "j".into(),
            path: PathBuf::new(),
            pad: None,
            open_loop: 0,
            clock_waits: 0,
            discs: Vec::new(),
            name: None,
        };
        let none = BTreeMap::new();
        assert!(check_pad(&info, &none).unwrap_err().contains("dualshock"));
        info.pad = Some("analog".into());
        assert!(check_pad(&info, &none).is_err());
        let allowed = BTreeMap::from([("j".to_string(), "pedal".to_string())]);
        assert!(check_pad(&info, &allowed).is_ok());
    }

    #[test]
    fn a_smoke_resolves_symbols_and_refuses_pixels() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "syms.txt", "0x800a139c GAME_STATE\n");
        let good = r#"
name = "smoke"
disc = "d.cue"
symbols = "syms.txt"
pad = "dualshock"
[[step]]
wait_until = { kind = "ram", sym = "GAME_STATE", offset = 16, op = "eq", value = 1 }
timeout = 3000
[[step]]
press = ["cross"]
hold_until = { kind = "ram", sym = "GAME_STATE", offset = 16, op = "eq", value = 2 }
timeout = 600
[[step]]
checkpoint = "select"
golden = false
[[step.assert]]
kind = "ram"
sym = "GAME_STATE"
offset = 16
value = 2
"#;
        let p = write(dir.path(), "smoke.toml", good);
        let plan = plan(dir.path(), &p, "digital", 60).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].probes[0].addr, 0x800a139c + 16);
        assert_eq!(plan.max_frames, 3600);
        assert_eq!(plan.pad, "digital");
        let bad = good.replace("golden = false\n", "");
        let p = write(dir.path(), "bad.toml", &bad);
        assert!(super::plan(dir.path(), &p, "digital", 60)
            .unwrap_err()
            .contains("golden = false"));
        let bad = good.replace("timeout = 600", "wait = 30");
        let p = write(dir.path(), "bad2.toml", &bad);
        assert!(super::plan(dir.path(), &p, "digital", 60).is_err());
    }

    #[test]
    fn probes_compare_with_size_and_sign() {
        let p = Probe {
            text: String::new(),
            addr: 0,
            size: 2,
            signed: true,
            op: "lt".into(),
            value: 0,
            value_hi: 0,
        };
        assert!(p.holds(0xffff));
        assert!(!p.holds(0x0001));
        let between = Probe {
            op: "between".into(),
            value: 3,
            value_hi: 5,
            size: 4,
            signed: false,
            ..p
        };
        assert!(between.holds(4) && !between.holds(6));
    }

    #[test]
    fn an_ambiguous_symbol_is_an_error() {
        let mut s = Symbols::default();
        s.insert("A", 1);
        s.insert("A", 2);
        s.insert("B", 3);
        assert!(s.resolve("A").is_err());
        assert_eq!(s.resolve("B"), Ok(3));
    }
}
