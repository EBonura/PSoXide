//! The deepest stack a journey reaches, from data the gate already writes: one extra gate run of
//! the journey on `real` with `--trace-failures --trace-checkpoint <checkpoint>` saves main RAM at
//! that checkpoint (`checkpoint-NNNN/ram.bin`). Emulated RAM starts zeroed and the stack grows
//! down from `top`, so the lowest non-zero word inside the reserve below `top` is the deepest
//! the stack has been by then (a word the program wrote and left non-zero; a frame that only ever
//! held zeros is not seen, so the figure is a floor of the true depth).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::game::Stack;
use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// The SDK linker script's initial stack pointer (`RAM_BASE + RAM_SIZE - 0x100`).
pub const SDK_STACK_TOP: u32 = 0x801f_ff00;
/// The SDK linker script's default `STACK_RESERVE`.
pub const SDK_STACK_RESERVE: u32 = 0x8000;
const RAM_BASE: u32 = 0x8000_0000;

/// Depth in bytes from `top` down to the lowest non-zero 32-bit word in `[top - reserve, top)`.
/// `Ok(0)` when the whole reserve is zero.
pub fn depth(ram: &[u8], top: u32, reserve: u32) -> Result<u32, String> {
    let top_off = top
        .checked_sub(RAM_BASE)
        .map(|o| o as usize)
        .filter(|&o| o <= ram.len() && o % 4 == 0)
        .ok_or_else(|| format!("stack top {top:#x} is not a word inside the RAM dump"))?;
    let floor = top_off
        .checked_sub(reserve as usize)
        .ok_or_else(|| format!("reserve {reserve:#x} reaches below RAM"))?;
    let lowest = (floor..top_off)
        .step_by(4)
        .find(|&o| ram[o..o + 4] != [0, 0, 0, 0]);
    Ok(lowest.map_or(0, |o| (top_off - o) as u32))
}

/// The stack stage: one check per `[[stack]]`.
pub fn run(ctx: &Ctx<'_>, bless: bool) -> StageResult {
    let mut checks = Vec::new();
    let mut facts = BTreeMap::new();
    let mut hits = 0;
    for s in &ctx.game.stacks {
        let name = format!("deepest stack {}@{}", s.journey, s.checkpoint);
        match measure(ctx, s) {
            Ok((d, hit)) => {
                hits += usize::from(hit);
                facts.insert(format!("stack/{}", s.journey), d.to_string());
                checks.push(judge(ctx, s, &name, d, bless));
            }
            Err(e) => checks.push(Check::fail(name, e)),
        }
    }
    StageResult {
        name: "stack".into(),
        source: match hits {
            0 => Source::Run,
            n if n == ctx.game.stacks.len() => Source::Hit,
            _ => Source::Mixed,
        },
        key: String::new(),
        outcome: Outcome { checks, facts },
    }
}

fn judge(ctx: &Ctx<'_>, s: &Stack, name: &str, d: u32, bless: bool) -> Check {
    let reserve = s.reserve.unwrap_or(SDK_STACK_RESERVE);
    let value = format!(
        "{d} bytes of the {reserve}-byte reserve ({} free)",
        reserve - d.min(reserve)
    );
    if d >= reserve.saturating_sub(4) {
        return Check::fail(
            name,
            format!(
                "{value}: the lowest word of the reserve is written, the stack may have overflowed \
                 into the heap"
            ),
        );
    }
    if bless {
        return Check::pass(name, value);
    }
    match ctx
        .baseline
        .as_ref()
        .and_then(|b| b.stack.get(&s.journey).copied())
    {
        None => Check::fail(
            name,
            format!(
                "{value}; no [stack] entry for {} in the baseline: run `psoxide-test bless`",
                s.journey
            ),
        ),
        Some(base) if d > base + s.max_growth => Check::fail(
            name,
            format!(
                "{value}: deeper than the baseline {base} by {} bytes (allowed growth {})",
                d - base,
                s.max_growth
            ),
        ),
        Some(base) => {
            let mut c = Check::pass(name, value);
            c.baseline = base.to_string();
            c
        }
    }
}

/// The journey file of this id among the game's journeys.
fn journey_path(ctx: &Ctx<'_>, id: &str) -> Result<PathBuf, String> {
    let spec = ctx
        .game
        .journeys
        .as_ref()
        .ok_or("[[stack]] needs [journeys]")?;
    super::journeys::files(&ctx.repo, &spec.files)?
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s == id))
        .ok_or_else(|| format!("no journey `{id}` among [journeys] files"))
}

fn measure(ctx: &Ctx<'_>, s: &Stack) -> Result<(u32, bool), String> {
    let gate = ctx.config.gate()?.to_path_buf();
    let path = journey_path(ctx, &s.journey)?;
    let info = crate::journey::inspect(&path, &[])?;
    let spec = ctx
        .game
        .journeys
        .as_ref()
        .ok_or("[[stack]] needs [journeys]")?;
    let conformance = format!("--conformance={}", spec.conformance.join(","));
    let top = stack_top(ctx, s)?;
    let reserve = s.reserve.unwrap_or(SDK_STACK_RESERVE);
    let mut key = Key::new("stack");
    key.file("journey", &path)?;
    for disc in &info.discs {
        let cue = ctx.repo.join(disc);
        key.file("bin", &cue.with_extension("bin"))?;
    }
    for b in &ctx.game.builds {
        let list = b.out_dir(&ctx.repo).join(super::build::SYMBOLS_FILE);
        if list.is_file() {
            key.file(&format!("symbols {}", b.name), &list)?;
        }
        let images = b.out_dir(&ctx.repo).join(super::build::IMAGE_SYMBOLS);
        if !b.images.is_empty() && images.is_file() {
            key.file(&format!("images {}", b.name), &images)?;
        }
    }
    key.part("gate", crate::config::GATE_REVISION);
    key.part(
        "window",
        &format!("{} {top:#x} {reserve:#x} {conformance}", s.checkpoint),
    );
    let key = key.finish();
    if let Some(o) = ctx.receipts.get("stack", &key) {
        println!("RECEIPT HIT stack {} {}", s.journey, util::short(&key));
        let d = o
            .facts
            .get("depth")
            .and_then(|v| v.parse().ok())
            .ok_or("stack receipt without a depth")?;
        return Ok((d, true));
    }
    let out = ctx.dir(&["stack", &s.journey])?;
    // A run starts clean: the RAM found afterwards is this run's.
    std::fs::remove_dir_all(&out).map_err(|e| format!("clear {}: {e}", out.display()))?;
    std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let log = ctx.dir(&["stack"])?.join(format!("{}.log", s.journey));
    let argv: Vec<String> = vec![
        gate.display().to_string(),
        "run".into(),
        path.display().to_string(),
        "--no-hw".into(),
        "--hardware".into(),
        "real".into(),
        "--trace-failures".into(),
        "--trace-checkpoint".into(),
        s.checkpoint.clone(),
        "--out".into(),
        out.join("report").display().to_string(),
        conformance,
    ];
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = ctx.machine("emu", || {
        util::run_logged(&argv, &ctx.repo, &env, &log, None)
    })?;
    if done.code != 0 {
        return Err(format!(
            "the journey failed in the stack run (gate exited {}); log {}",
            done.code,
            log.display()
        ));
    }
    let ram = find_ram(&out).ok_or_else(|| {
        format!(
            "the gate saved no RAM for checkpoint `{}` under {} (is it a checkpoint of {}?); log {}",
            s.checkpoint,
            out.display(),
            s.journey,
            log.display()
        )
    })?;
    let bytes = std::fs::read(&ram).map_err(|e| format!("read {}: {e}", ram.display()))?;
    let d = depth(&bytes, top, reserve)?;
    let outcome = Outcome {
        checks: vec![Check::pass("measured", "")],
        facts: BTreeMap::from([("depth".to_string(), d.to_string())]),
    };
    ctx.receipts.put("stack", &key, &outcome)?;
    Ok((d, false))
}

/// The stack top a `[[stack]]` measures down from: the `STACK_INIT` its build's link map
/// gives, else `top`, else the stock SDK value (a game that declares neither keeps its key).
fn stack_top(ctx: &Ctx<'_>, s: &Stack) -> Result<u32, String> {
    let Some(name) = &s.build else {
        return Ok(s.top.unwrap_or(SDK_STACK_TOP));
    };
    let map = ctx
        .game
        .build(name)
        .and_then(|b| b.map_path(&ctx.repo))
        .ok_or_else(|| format!("[[stack]] build `{name}` declares no map"))?;
    top_from_map(&map)
}

/// `STACK_INIT` of the link whose map is at `path`.
pub fn top_from_map(path: &Path) -> Result<u32, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    psoxide_size::map::parse(&text)
        .stack_init()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The one `ram.bin` the traced run saved.
fn find_ram(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().is_some_and(|n| n == "ram.bin") {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_is_the_lowest_written_word_below_the_top() {
        let mut ram = vec![0u8; 2 * 1024 * 1024];
        let top = SDK_STACK_TOP;
        let off = (top - RAM_BASE) as usize;
        assert_eq!(depth(&ram, top, SDK_STACK_RESERVE).unwrap(), 0);
        ram[off - 4] = 1;
        ram[off - 0x400] = 7;
        assert_eq!(depth(&ram, top, SDK_STACK_RESERVE).unwrap(), 0x400);
        // Heap data below the reserve is not stack.
        ram[off - SDK_STACK_RESERVE as usize - 8] = 9;
        assert_eq!(depth(&ram, top, SDK_STACK_RESERVE).unwrap(), 0x400);
        assert!(depth(&ram, 0x7000_0000, 16).is_err());
    }

    #[test]
    fn the_top_comes_from_the_builds_map_so_a_hosted_link_is_measured_from_its_own_top() {
        let dir = tempfile::tempdir().unwrap();
        let lines = concat!(
            "     VMA      LMA     Size Align Out     In      Symbol\n",
            "       0        0        0     1 RAM_BASE = 0x80000000\n",
            "       0        0        0     1 RAM_SIZE = DEFINED(RAM_SIZE_OVERRIDE) ? RAM_SIZE_OVERRIDE : 2M\n",
            "       0        0        0     1 STACK_INIT = RAM_BASE + RAM_SIZE - 0x100\n",
        );
        let stock = dir.path().join("stock.map");
        std::fs::write(&stock, lines).unwrap();
        assert_eq!(top_from_map(&stock), Ok(SDK_STACK_TOP));
        let hosted = dir.path().join("hosted.map");
        std::fs::write(
            &hosted,
            format!("{lines}       0        0        0     1 RAM_SIZE_OVERRIDE = 0x1FE000\n"),
        )
        .unwrap();
        let top = top_from_map(&hosted).unwrap();
        assert_eq!(top, 0x801f_df00);
        // Measured from the hosted top, a frame 0x200 B under it is 0x200 deep; measured from the
        // stock constant, the same RAM would read 0x2000 B (the core window) deeper than it is.
        let mut ram = vec![0u8; 2 * 1024 * 1024];
        ram[(top - RAM_BASE) as usize - 0x200] = 1;
        assert_eq!(depth(&ram, top, SDK_STACK_RESERVE).unwrap(), 0x200);
        assert_eq!(
            depth(&ram, SDK_STACK_TOP, SDK_STACK_RESERVE).unwrap(),
            SDK_STACK_TOP - top + 0x200
        );
        assert!(top_from_map(&dir.path().join("missing.map")).is_err());
    }
}
