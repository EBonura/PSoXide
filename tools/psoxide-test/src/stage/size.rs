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
    let free = ram
        .free
        .ok_or("the map names no __heap_start / STACK_RESERVE, so free RAM is unknown")?;
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
    let free_ok = now.free.saturating_add(drop) >= base.free;
    let delta = |a: u32, b: u32| i64::from(a) - i64::from(b);
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
        Check::new(
            "free RAM",
            if free_ok {
                crate::report::Status::Pass
            } else {
                crate::report::Status::Fail
            },
            format!("{} ({:+})", now.free, delta(now.free, base.free)),
        )
        .baseline(base.free.to_string())
        .detail(if free_ok {
            String::new()
        } else {
            format!("free RAM fell under the baseline - {drop}")
        }),
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

/// The size stage.
pub fn run(ctx: &Ctx<'_>) -> StageResult {
    let result = |source, key: String, outcome| StageResult {
        name: "size".into(),
        source,
        key,
        outcome,
    };
    let Some(size) = &ctx.game.size else {
        return result(
            Source::Run,
            String::new(),
            Outcome {
                checks: vec![Check::skip("size", "the game file has no [size]")],
                ..Outcome::default()
            },
        );
    };
    let Some(map) = ctx
        .game
        .build(&size.build)
        .and_then(|b| b.map_path(&ctx.repo))
    else {
        return result(Source::Run, String::new(), Outcome::error("size", "no map"));
    };
    let mut key = Key::new("size");
    if let Err(e) = key.file("map", &map) {
        return result(Source::Run, String::new(), Outcome::error("size", e));
    }
    let base = ctx.baseline.as_ref().and_then(|b| b.size);
    key.part("baseline", &format!("{base:?}"));
    key.part(
        "tolerance",
        &format!("{} {}", size.text_max_growth, size.free_min_drop),
    );
    let key = key.finish();
    if let Some(outcome) = ctx.receipts.get("size", &key) {
        println!("RECEIPT HIT size {}", util::short(&key));
        return result(Source::Hit, key, outcome);
    }
    let outcome = match std::fs::read_to_string(&map)
        .map_err(|e| format!("read {}: {e}", map.display()))
        .and_then(|t| measure(&t))
    {
        Ok(now) => Outcome {
            checks: judge(now, base, size.text_max_growth, size.free_min_drop),
            facts: [
                ("text".to_string(), now.text.to_string()),
                ("data".to_string(), now.data.to_string()),
                ("bss".to_string(), now.bss.to_string()),
                ("free".to_string(), now.free.to_string()),
            ]
            .into(),
        },
        Err(e) => Outcome::error("size", e),
    };
    let _ = ctx.receipts.put("size", &key, &outcome);
    result(Source::Run, key, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Status;

    const BASE: SizeBase = SizeBase {
        text: 1000,
        data: 10,
        bss: 20,
        free: 500,
    };

    #[test]
    fn growth_fails_without_tolerance_and_passes_with_it() {
        let now = SizeBase { text: 1040, ..BASE };
        assert_eq!(judge(now, Some(BASE), 0, 0)[0].status, Status::Fail);
        assert_eq!(judge(now, Some(BASE), 64, 0)[0].status, Status::Pass);
    }

    #[test]
    fn free_ram_may_not_fall() {
        let now = SizeBase { free: 499, ..BASE };
        assert_eq!(judge(now, Some(BASE), 0, 0)[1].status, Status::Fail);
        let better = SizeBase {
            free: 900,
            text: 10,
            ..BASE
        };
        assert!(judge(better, Some(BASE), 0, 0)
            .iter()
            .all(|c| c.status == Status::Pass));
    }

    #[test]
    fn no_baseline_is_a_failure_that_says_what_to_do() {
        let checks = judge(BASE, None, 0, 0);
        assert!(checks[0].detail.contains("bless"));
    }
}
