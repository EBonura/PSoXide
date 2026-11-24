//! Load time: how long the console takes from one game state to another, measured on the PSoXide
//! frontend in VBlanks (route ticks) and bus cycles. A `[[load]]` with no `from` is boot to the
//! game's own first frame; with one, a scene load (start and end named by the game, or by the
//! scene core's hand-off once its map names it). One replay per row, input-free or on a tape.
//! The bar: never slower than the baseline (by more than `max_growth` VBlanks with its reason),
//! and at install never slower than the installed build's facts. Runs inside the speed stage, so
//! a game without `[[load]]` has no new row.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::game::Load;
use crate::journey::Probe;
use crate::report::{Check, Outcome, Status};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// One load row's measure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadBase {
    /// Route ticks (one per VBlank period) from the start state to the end state.
    pub vblanks: u64,
    /// Bus cycles over the same span (the finer number; reported, not judged).
    pub cycles: u64,
}

/// What a `[[load]]` row measures, for the install facts: two rows are compared only when this
/// is equal.
pub fn identity(l: &Load) -> String {
    let conds = |c: &[toml::Table]| -> String {
        if c.is_empty() {
            "power-on".into()
        } else {
            c.iter()
                .map(|t| serde_json::to_string(t).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" and ")
        }
    };
    format!(
        "from {} until {} max_polls {} tape {} args {:?}",
        conds(&l.from),
        conds(&l.until),
        l.max_polls,
        l.tape.as_deref().unwrap_or("none"),
        l.launch_args
    )
}

/// Measure and judge every `[[load]]` row: checks and `load/<name>/<metric>` facts. `bless`
/// reports the numbers without judging them (they become the baseline).
pub fn run(
    ctx: &Ctx<'_>,
    frontend: &Path,
    bless: bool,
) -> (Vec<Check>, BTreeMap<String, String>, usize) {
    let mut checks = Vec::new();
    let mut facts = BTreeMap::new();
    let mut hits = 0;
    for l in &ctx.game.loads {
        let name = format!("load {} time", l.name);
        match measure(ctx, l, frontend) {
            Ok((m, hit)) => {
                hits += usize::from(hit);
                facts.insert(format!("load/{}/vblanks", l.name), m.vblanks.to_string());
                facts.insert(format!("load/{}/cycles", l.name), m.cycles.to_string());
                let base = ctx
                    .baseline
                    .as_ref()
                    .and_then(|b| b.load.get(&l.name).copied());
                checks.push(judge(&name, m, base, l.max_growth, bless));
            }
            Err(e) => checks.push(Check::fail(name, e)),
        }
    }
    (checks, facts, hits)
}

/// Judge one measure against the baseline: more VBlanks than the baseline plus `growth` fails.
pub fn judge(name: &str, now: LoadBase, base: Option<LoadBase>, growth: u64, bless: bool) -> Check {
    let value = format!("{} VBlanks ({} cycles)", now.vblanks, now.cycles);
    if bless {
        return Check::pass(name, value);
    }
    let Some(base) = base else {
        return Check::fail(
            name,
            format!("{value}; no [load] entry in the baseline: run `psoxide-test bless`"),
        );
    };
    let delta = i128::from(now.vblanks) - i128::from(base.vblanks);
    let cycles = i128::from(now.cycles) - i128::from(base.cycles);
    let value = format!("{value}, {delta:+} VBlanks ({cycles:+} cycles)");
    let c = if now.vblanks <= base.vblanks.saturating_add(growth) {
        Check::pass(name, value)
    } else {
        Check::new(name, Status::Fail, value).detail(format!(
            "slower than the baseline by {delta} VBlanks (allowed {growth}): find the cause (a \
             longer read, a new seek, more data) before blessing it"
        ))
    };
    c.baseline(format!("{} VBlanks ({} cycles)", base.vblanks, base.cycles))
}

fn measure(ctx: &Ctx<'_>, l: &Load, frontend: &Path) -> Result<(LoadBase, bool), String> {
    let build = ctx
        .game
        .build(&l.build)
        .ok_or_else(|| format!("unknown build `{}`", l.build))?;
    let symbols = super::build::symbols(&ctx.repo, build)
        .ok_or_else(|| format!("build `{}` declares no map", build.name))??;
    let resolve = |c: &[toml::Table]| -> Result<Vec<Probe>, String> {
        c.iter()
            .map(|t| crate::journey::condition(t, &symbols))
            .collect()
    };
    // Neither end counts before the program has started (an empty `from` is power-on, and
    // stays so).
    let from = resolve(&l.from)?;
    let from = if from.is_empty() {
        from
    } else {
        crate::journey::with_started(from, &symbols)?
    };
    let until = crate::journey::with_started(resolve(&l.until)?, &symbols)?;
    let disc = build.cue(&ctx.repo);
    let disc_digest = ctx
        .artifacts
        .get(&build.name)
        .and_then(|a| a.get(&build.disc.replace(".cue", ".bin")))
        .cloned()
        .unwrap_or_default();
    let tape = l.tape.as_ref().map(|t| ctx.repo.join(t));
    let mut key = Key::new("load");
    key.part("disc", &disc_digest);
    key.file("frontend", frontend)?;
    if let Some(tape) = &tape {
        key.file("tape", tape)?;
    }
    key.part(
        "probes",
        &serde_json::to_string(&(&from, &until)).map_err(|e| e.to_string())?,
    );
    key.part("limit", &format!("{} {:?}", l.max_polls, l.launch_args));
    let key = key.finish();
    let read = |o: &Outcome| -> Option<LoadBase> {
        Some(LoadBase {
            vblanks: o.facts.get("vblanks")?.parse().ok()?,
            cycles: o.facts.get("cycles")?.parse().ok()?,
        })
    };
    if let Some(found) = ctx.receipts.get("load", &key).as_ref().and_then(read) {
        println!("RECEIPT HIT load {} {}", l.name, util::short(&key));
        return Ok((found, true));
    }
    let dir = ctx.dir(&["load"])?;
    let stem = format!("{}-{}", l.name, util::short(&key));
    let route = dir.join(format!("{stem}.route.csv"));
    let log = dir.join(format!("{stem}.log"));
    let mut argv: Vec<String> = vec![
        frontend.display().to_string(),
        "launch".into(),
        "--path".into(),
        disc.display().to_string(),
    ];
    if let Some(tape) = &tape {
        argv.extend([
            "--input-tape".into(),
            tape.display().to_string(),
            "--input-tape-hold-last".into(),
        ]);
    }
    argv.extend([
        "--stop-at-poll".into(),
        l.max_polls.to_string(),
        "--steps".into(),
        super::speed::LOCATE_STEPS.into(),
        "--route-log".into(),
        route.display().to_string(),
    ]);
    let mut words: Vec<u32> = from.iter().chain(&until).map(|p| p.addr & !3).collect();
    words.sort_unstable();
    words.dedup();
    for word in &words {
        argv.push("--route-watch-u32".into());
        argv.push(format!("{word:#010x}"));
    }
    argv.extend(l.launch_args.iter().cloned());
    let env = ctx.child_env(&ctx.work.join("guest-stage"));
    let done = ctx.machine("emu", || {
        util::run_logged(&argv, &ctx.repo, &env, &log, None)
    })?;
    if done.code != 0 {
        return Err(format!(
            "the load replay exited {}; log {}",
            done.code,
            log.display()
        ));
    }
    let text =
        std::fs::read_to_string(&route).map_err(|e| format!("read {}: {e}", route.display()))?;
    let found = span(&text, &from, &until, l.max_polls)?;
    let outcome = Outcome {
        checks: vec![Check::pass("measured", "")],
        facts: BTreeMap::from([
            ("vblanks".to_string(), found.vblanks.to_string()),
            ("cycles".to_string(), found.cycles.to_string()),
        ]),
    };
    let _ = ctx.receipts.put("load", &key, &outcome);
    Ok((found, false))
}

/// From a route log with `ram_XXXXXXXX` columns: the ticks and bus cycles from the first row
/// where every `from` probe holds (the first row when `from` is empty: power-on) to the first
/// later row where every `until` probe holds.
pub fn span(
    route: &str,
    from: &[Probe],
    until: &[Probe],
    max_polls: u32,
) -> Result<LoadBase, String> {
    let mut lines = route.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| format!("the route log has no {name} column"))
    };
    let tick_at = column("route_tick")?;
    let cycles_at = column("bus_cycles")?;
    let words = |probes: &[Probe]| -> Result<Vec<(usize, u32)>, String> {
        probes
            .iter()
            .map(|p| {
                Ok((
                    column(&format!("ram_{:08x}", p.addr & !3))?,
                    (p.addr & 3) * 8,
                ))
            })
            .collect()
    };
    let (from_words, until_words) = (words(from)?, words(until)?);
    let text = |probes: &[Probe]| -> String {
        probes
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" and ")
    };
    let mut start: Option<(u64, u64)> = None;
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let number = |at: usize| -> Option<u64> { fields.get(at)?.parse().ok() };
        let (Some(tick), Some(cycles)) = (number(tick_at), number(cycles_at)) else {
            continue;
        };
        let all = |probes: &[Probe], at: &[(usize, u32)]| {
            probes
                .iter()
                .zip(at)
                .all(|(p, (col, shift))| number(*col).is_some_and(|v| p.holds((v as u32) >> shift)))
        };
        match start {
            None if all(from, &from_words) => {
                // Power-on is cycle 0 of tick 0, not the end of the first tick.
                start = Some(if from.is_empty() {
                    (0, 0)
                } else {
                    (tick, cycles)
                });
                if !from.is_empty() || !all(until, &until_words) {
                    continue;
                }
                return Ok(LoadBase {
                    vblanks: tick,
                    cycles,
                });
            }
            Some((t0, c0)) if tick > t0 && all(until, &until_words) => {
                return Ok(LoadBase {
                    vblanks: tick - t0,
                    cycles: cycles.saturating_sub(c0),
                });
            }
            _ => {}
        }
    }
    Err(match start {
        None => format!("`{}` never held before poll {max_polls}", text(from)),
        Some(_) => format!(
            "`{}` never held{} before poll {max_polls}",
            text(until),
            if from.is_empty() {
                String::new()
            } else {
                format!(" after `{}`", text(from))
            }
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(addr: u32, op: &str, value: i64) -> Probe {
        Probe {
            text: format!("{addr:x} {op} {value}"),
            addr,
            size: 4,
            signed: false,
            op: op.into(),
            value,
            value_hi: 0,
        }
    }

    const ROUTE: &str = "route_tick,port1_polls,bus_cycles,ram_80001000\n\
                         1,0,564480,0\n2,0,1128960,0\n3,1,1693440,1\n4,2,2257920,1\n\
                         5,3,2822400,2\n6,4,3386880,3\n";

    #[test]
    fn boot_counts_from_power_on_to_the_games_first_state() {
        let first = probe(0x8000_1000, "ge", 1);
        let m = span(ROUTE, &[], &[first], 10).unwrap();
        assert_eq!(
            m,
            LoadBase {
                vblanks: 3,
                cycles: 1_693_440
            }
        );
    }

    #[test]
    fn a_scene_load_counts_between_two_named_states() {
        let from = probe(0x8000_1000, "eq", 1);
        let until = probe(0x8000_1000, "ge", 3);
        let m = span(ROUTE, std::slice::from_ref(&from), &[until], 10).unwrap();
        assert_eq!(
            m,
            LoadBase {
                vblanks: 3,
                cycles: 3_386_880 - 1_693_440
            }
        );
        let never = probe(0x8000_1000, "eq", 9);
        assert!(span(ROUTE, &[from], &[never], 10)
            .unwrap_err()
            .contains("never held after"));
    }

    #[test]
    fn slower_than_the_baseline_fails_with_its_delta_and_bless_takes_any_number() {
        let base = LoadBase {
            vblanks: 362,
            cycles: 204_000_000,
        };
        let now = LoadBase {
            vblanks: 365,
            cycles: 205_000_000,
        };
        let c = judge("load boot time", now, Some(base), 0, false);
        assert_eq!(c.status, Status::Fail);
        assert!(c.value.contains("+3 VBlanks"), "{}", c.value);
        assert!(c.detail.contains("by 3 VBlanks"), "{}", c.detail);
        assert_eq!(judge("l", now, Some(base), 3, false).status, Status::Pass);
        let faster = LoadBase {
            vblanks: 300,
            ..now
        };
        assert_eq!(
            judge("l", faster, Some(base), 0, false).status,
            Status::Pass
        );
        assert_eq!(judge("l", now, None, 0, false).status, Status::Fail);
        assert_eq!(judge("l", now, None, 0, true).status, Status::Pass);
    }
}
