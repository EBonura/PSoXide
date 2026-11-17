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
    /// Disc paths the journey boots (first existing wins in the gate).
    pub discs: Vec<String>,
}

/// Read a journey file far enough to lint it.
pub fn inspect(path: &Path) -> Result<Info, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pad = value.get("pad").and_then(Value::as_str).map(str::to_string);
    let mut open_loop = 0;
    for list in [value.get("step")].into_iter().flatten().chain(
        value
            .get("block")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| b.get("step")),
    ) {
        open_loop += count_open_loop(list);
    }
    Ok(Info {
        id,
        path: path.to_path_buf(),
        pad,
        open_loop,
        discs: strings(value.get("disc")),
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
    /// Whether the condition holds for `raw`, the little-endian bytes read at `addr`.
    pub fn holds(&self, raw: u32) -> bool {
        let v: i64 = match (self.size, self.signed) {
            (1, false) => i64::from(raw as u8),
            (1, true) => i64::from(raw as u8 as i8),
            (2, false) => i64::from(raw as u16),
            (2, true) => i64::from(raw as u16 as i16),
            (_, false) => i64::from(raw),
            (_, true) => i64::from(raw as i32),
        };
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
    })
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
            _ => Err(format!("symbol `{name}` is not in the journey's symbols")),
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
        let info = inspect(&p).unwrap();
        assert_eq!(
            info.open_loop, 3,
            "wait 90, the 4-tick hold, the frame-counted body press"
        );
        assert_eq!(info.pad.as_deref(), Some("dualshock"));
        assert_eq!(info.discs, ["build/x.cue"]);
    }

    #[test]
    fn the_pad_must_be_named() {
        let mut info = Info {
            id: "j".into(),
            path: PathBuf::new(),
            pad: None,
            open_loop: 0,
            discs: Vec::new(),
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
