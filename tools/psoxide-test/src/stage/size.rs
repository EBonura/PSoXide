//! Size: ONE measure, `psoxide_size::ram::read` of the build's link map (the `.text`, `.data` and
//! `.bss` output sections and `heap_free`), the same code `psoxide-size ram` prints, so the bar and
//! the tool cannot disagree. Text may not grow and free RAM may not fall unless the game file
//! allows it with a reason.

use crate::baseline::SizeBase;
use crate::report::{Check, Outcome, Source, StageResult};
use crate::stage::Ctx;
use crate::util::{self, Key};

/// Measure a map.
pub fn measure(map_text: &str) -> Result<SizeBase, String> {
    let map = psoxide_size::map::parse(map_text);
    let ram = psoxide_size::ram::read(&map);
    let free = ram.free;
    Ok(SizeBase {
        text: ram.text_total,
        data: ram.data_total,
        bss: ram.bss_total,
        free,
    })
}

/// Judge `now` against `base` with the game's tolerances.
pub fn judge(now: SizeBase, base: Option<SizeBase>, growth: u32, drop: u32) -> Vec<Check> {
    let Some(base) = base else {
        return vec![Check::fail(
            "size baseline",
            "no [size] in psoxide-test.baseline.toml: run `psoxide-test bless` on the reference build",
        )];
    };
    let text_ok = now.text <= base.text.saturating_add(growth);
    let delta = |a: u32, b: u32| i64::from(a) - i64::from(b);
    let free = match (now.free, base.free) {
        (Some(n), Some(b)) => {
            let ok = n.saturating_add(drop) >= b;
            Check::new(
                "free RAM",
                if ok {
                    crate::report::Status::Pass
                } else {
                    crate::report::Status::Fail
                },
                format!("{n} ({:+})", delta(n, b)),
            )
            .baseline(b.to_string())
            .detail(if ok {
                String::new()
            } else {
                format!("free RAM fell under the baseline - {drop}")
            })
        }
        (Some(_), None) => Check::fail(
            "free RAM",
            "the map now names a heap but the baseline has no free RAM: bless again",
        ),
        (None, Some(b)) => Check::fail(
            "free RAM",
            format!("the baseline had {b} B free but this map names no heap"),
        ),
        (None, None) => Check::skip(
            "free RAM",
            "not measured: the map names no __heap_start / STACK_RESERVE (a program with its own \
             allocator); declare the figure the program reports as a [[host]] metric (for \
             example heap_free from a boot run, better = \"higher\", min = the floor)",
        ),
    };
    vec![
        Check::new(
            "text",
            if text_ok {
                crate::report::Status::Pass
            } else {
                crate::report::Status::Fail
            },
            format!("{} ({:+})", now.text, delta(now.text, base.text)),
        )
        .baseline(base.text.to_string())
        .detail(if text_ok {
            String::new()
        } else {
            format!("text grew past the baseline + {growth}")
        }),
        free,
        Check::pass(
            "data",
            format!("{} ({:+})", now.data, delta(now.data, base.data)),
        ),
        Check::pass(
            "bss",
            format!("{} ({:+})", now.bss, delta(now.bss, base.bss)),
        ),
    ]
}

/// The absolute ceiling of a `[[size]]` with `max_bytes`: `.text + .data + .bss` of the link.
pub fn ceiling(now: SizeBase, max: u32) -> Check {
    let span = u64::from(now.text) + u64::from(now.data) + u64::from(now.bss);
    let value = format!(
        "{span} B of {max} (text {} + data {} + bss {})",
        now.text, now.data, now.bss
    );
    if span <= u64::from(max) {
        Check::pass(
            "span",
            format!("{value}, {} B spare", u64::from(max) - span),
        )
    } else {
        Check::new("span", crate::report::Status::Fail, value)
            .detail(format!("{} B over the ceiling", span - u64::from(max)))
    }
}

/// The size stage: one link per `[[size]]` entry, checks named after its build when there are
/// several.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let mut checks = Vec::new();
    let mut facts = std::collections::BTreeMap::new();
    let mut hits = 0;
    for size in &ctx.game.sizes {
        let prefix = if ctx.game.sizes.len() > 1 {
            format!("{} ", size.key())
        } else {
            String::new()
        };
        match one(ctx, size) {
            Ok((outcome, hit)) => {
                hits += usize::from(hit);
                for mut c in outcome.checks {
                    c.name = format!("{prefix}{}", c.name);
                    checks.push(c);
                }
                for (k, v) in outcome.facts {
                    facts.insert(format!("{}/{k}", size.key()), v);
                }
            }
            Err(e) => checks.push(Check::fail(format!("{prefix}size"), e)),
        }
    }
    if ctx.game.sizes.is_empty() {
        checks.push(Check::skip("size", "the game file has no [[size]]"));
    }
    StageResult {
        name: "size".into(),
        source: if hits > 0 && hits == ctx.game.sizes.len() {
            Source::Hit
        } else {
            Source::Run
        },
        key: String::new(),
        outcome: Outcome { checks, facts },
    }
}

fn one(ctx: &Ctx<'_>, size: &crate::game::Size) -> Result<(Outcome, bool), String> {
    let map = size.map_path(ctx.game, &ctx.repo).ok_or("no map")?;
    let mut key = Key::new("size");
    key.file("map", &map)?;
    let base = ctx
        .baseline
        .as_ref()
        .and_then(|b| b.size.get(&size.key()).copied());
    key.part("baseline", &format!("{base:?}"));
    let base_map = ctx
        .baseline
        .as_ref()
        .and_then(|b| b.maps.get(&size.key()).cloned());
    key.part("baseline map", base_map.as_deref().unwrap_or(""));
    if let Some(digest) = &base_map {
        key.part(
            "baseline map kept",
            &stored_map(ctx, digest).is_file().to_string(),
        );
    }
    key.part(
        "tolerance",
        &format!("{} {}", size.text_max_growth, size.free_min_drop),
    );
    // Only a bar with a ceiling has it, so every other bar keeps its key.
    if let Some(max) = size.max_bytes {
        key.part("max bytes", &max.to_string());
    }
    let key = key.finish();
    if let Some(outcome) = ctx.receipts.get("size", &key) {
        println!("RECEIPT HIT size {} {}", size.key(), util::short(&key));
        return Ok((outcome, true));
    }
    let text = std::fs::read_to_string(&map).map_err(|e| format!("read {}: {e}", map.display()))?;
    let now = measure(&text)?;
    let mut checks = judge(now, base, size.text_max_growth, size.free_min_drop);
    if let Some(max) = size.max_bytes {
        checks.push(ceiling(now, max));
    }
    if let (Some(base), Some(digest)) = (base, &base_map) {
        if base.text != now.text {
            checks.push(explain(&stored_map(ctx, digest), digest, &text));
        }
    }
    let outcome = Outcome {
        checks,
        facts: [
            ("text".to_string(), now.text.to_string()),
            ("data".to_string(), now.data.to_string()),
            ("bss".to_string(), now.bss.to_string()),
            (
                "free".to_string(),
                now.free.map_or("not measured".into(), |f| f.to_string()),
            ),
        ]
        .into(),
    };
    let _ = ctx.receipts.put("size", &key, &outcome);
    Ok((outcome, false))
}

/// Where `bless` keeps a blessed link map: `<work>/<game>/maps/<sha256>.map`.
pub fn stored_map(ctx: &Ctx<'_>, digest: &str) -> std::path::PathBuf {
    ctx.work.join("maps").join(format!("{digest}.map"))
}

/// Keep the map at `path` in the work tree under its digest; returns the digest.
pub fn keep_map(ctx: &Ctx<'_>, path: &std::path::Path) -> Result<String, String> {
    let digest = util::sha256_file(path)?;
    let to = stored_map(ctx, &digest);
    if !to.is_file() {
        ctx.dir(&["maps"])?;
        std::fs::copy(path, &to).map_err(|e| format!("keep {}: {e}", path.display()))?;
    }
    Ok(digest)
}

/// How many functions the size explanation names in each direction.
const EXPLAIN_TOP: usize = 12;

/// The `.text` input sections of a map by function (the first symbol of each section, or the
/// section name), sizes summed per name.
fn text_by_function(map_text: &str) -> std::collections::BTreeMap<String, u32> {
    let map = psoxide_size::map::parse(map_text);
    let mut out = std::collections::BTreeMap::new();
    for section in map.sections.iter().filter(|s| s.out == ".text") {
        let name = section
            .syms
            .first()
            .map_or_else(|| section.name.clone(), |s| s.name.clone());
        *out.entry(name).or_insert(0) += section.size;
    }
    out
}

/// Per function, `(name, old size, new size)` for every function whose `.text` size changed,
/// largest growth first, then largest shrink.
pub fn symdiff(old_map: &str, new_map: &str) -> Vec<(String, u32, u32)> {
    let old = text_by_function(old_map);
    let new = text_by_function(new_map);
    let mut rows: Vec<(String, u32, u32)> = old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|n| {
            (
                n.clone(),
                old.get(n).copied().unwrap_or(0),
                new.get(n).copied().unwrap_or(0),
            )
        })
        .filter(|(_, a, b)| a != b)
        .collect();
    rows.sort_by_key(|(n, a, b)| (std::cmp::Reverse(i64::from(*b) - i64::from(*a)), n.clone()));
    rows
}

/// The sub-report under a `.text` change: which functions grew and shrank against the blessed
/// map. Informational (PASS); the bar itself is the `text` check.
fn explain(stored: &std::path::Path, digest: &str, new_map: &str) -> Check {
    let Ok(old_map) = std::fs::read_to_string(stored) else {
        return Check::skip(
            "text by function",
            format!(
                "the blessed map {} is not in {}: bless on this machine to keep it",
                util::short(digest),
                stored
                    .parent()
                    .map_or(String::new(), |p| p.display().to_string())
            ),
        );
    };
    let rows = symdiff(&old_map, new_map);
    let grew: i64 = rows
        .iter()
        .map(|(_, a, b)| i64::from(*b) - i64::from(*a))
        .filter(|d| *d > 0)
        .sum();
    let shrank: i64 = rows
        .iter()
        .map(|(_, a, b)| i64::from(*b) - i64::from(*a))
        .filter(|d| *d < 0)
        .sum();
    let line = |(n, a, b): &(String, u32, u32)| {
        format!("{:+} {n} ({a} -> {b})", i64::from(*b) - i64::from(*a))
    };
    let mut detail: Vec<String> = rows
        .iter()
        .filter(|(_, a, b)| b > a)
        .take(EXPLAIN_TOP)
        .map(line)
        .collect();
    detail.extend(
        rows.iter()
            .rev()
            .filter(|(_, a, b)| b < a)
            .take(EXPLAIN_TOP)
            .map(line),
    );
    Check::pass(
        "text by function",
        format!(
            "{} functions changed: {grew:+} grown, {shrank:+} shrunk",
            rows.len()
        ),
    )
    .detail(detail.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Status;

    const OLD_MAP: &str = "     VMA      LMA     Size Align Out     In      Symbol
80010000 80010000      100    16 .text
80010000 80010000       40     4         a.o:(.text.f)
80010000 80010000       40     1                 game::f
80010040 80010040       20     4         a.o:(.text.g)
80010040 80010040       20     1                 game::g
80010060 80010060       a0     4         a.o:(.text.h)
80010060 80010060       a0     1                 game::h
";
    const NEW_MAP: &str = "     VMA      LMA     Size Align Out     In      Symbol
80010000 80010000      120    16 .text
80010000 80010000       60     4         a.o:(.text.f)
80010000 80010000       60     1                 game::f
80010060 80010060       a0     4         a.o:(.text.h)
80010060 80010060       a0     1                 game::h
80010100 80010100       10     4         a.o:(.text.k)
80010100 80010100       10     1                 game::k
";

    #[test]
    fn symdiff_names_grown_new_and_removed_functions() {
        let rows = symdiff(OLD_MAP, NEW_MAP);
        assert_eq!(
            rows,
            vec![
                ("game::f".to_string(), 0x40, 0x60),
                ("game::k".to_string(), 0, 0x10),
                ("game::g".to_string(), 0x20, 0),
            ]
        );
    }

    const BASE: SizeBase = SizeBase {
        text: 1000,
        data: 10,
        bss: 20,
        free: Some(500),
    };

    #[test]
    fn growth_fails_without_tolerance_and_passes_with_it() {
        let now = SizeBase { text: 1040, ..BASE };
        assert_eq!(judge(now, Some(BASE), 0, 0)[0].status, Status::Fail);
        assert_eq!(judge(now, Some(BASE), 64, 0)[0].status, Status::Pass);
    }

    #[test]
    fn free_ram_may_not_fall() {
        let now = SizeBase {
            free: Some(499),
            ..BASE
        };
        assert_eq!(judge(now, Some(BASE), 0, 0)[1].status, Status::Fail);
        let better = SizeBase {
            free: Some(900),
            text: 10,
            ..BASE
        };
        assert!(judge(better, Some(BASE), 0, 0)
            .iter()
            .all(|c| c.status == Status::Pass));
    }

    #[test]
    fn a_ceiling_is_judged_on_text_data_and_bss_together() {
        // The scene core's window (CORE-ABI): 8,192 B, 0x801FDF00..0x801FFF00.
        let core = SizeBase {
            text: 7000,
            data: 200,
            bss: 900,
            free: None,
        };
        let c = ceiling(core, 8192);
        assert_eq!(c.status, Status::Pass);
        assert!(c.value.contains("8100 B of 8192") && c.value.contains("92 B spare"));
        let over = SizeBase { text: 7100, ..core };
        let c = ceiling(over, 8192);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("8 B over"), "{}", c.detail);
        assert_eq!(ceiling(over, 8200).status, Status::Pass);
    }

    #[test]
    fn no_baseline_is_a_failure_that_says_what_to_do() {
        let checks = judge(BASE, None, 0, 0);
        assert!(checks[0].detail.contains("bless"));
    }
}
