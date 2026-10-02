//! The data importers: committed sources turned into the JSON the pages
//! read. CI regenerates all three and fails on any difference.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;

use super::{read_text, site_root, write};
use crate::obj;
use crate::pyjson::{dumps, round, Json};

const BLOB: &str = "https://github.com/EBonura/PSoXide/tree/main/sdk/";

/// The text of a `## heading` section, up to the next `## `.
fn section<'a>(text: &'a str, heading: &str) -> Result<&'a str, String> {
    let (_, rest) = text
        .split_once(heading)
        .ok_or_else(|| format!("no {heading:?} section"))?;
    Ok(rest.split_once("\n## ").map_or(rest, |(body, _)| body))
}

/// A markdown table row's cells.
fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

/// Rows of the first table in `## heading`, below its header and rule.
fn table(text: &str, heading: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(section(text, &format!("## {heading}"))?
        .lines()
        .filter(|l| l.starts_with('|'))
        .skip(2)
        .map(cells)
        .collect())
}

fn name_of(cell: &str) -> Result<String, String> {
    Regex::new("`([^`]+)`")
        .expect("valid regex")
        .captures(cell)
        .map(|c| c[1].to_string())
        .ok_or_else(|| format!("no `name` in {cell:?}"))
}

/// Point relative README links into sdk/ on GitHub. This is
/// `re.sub(r"\]\((?!https?://)([^)]+)\)", ...)`, scanned the way Python
/// scans it: a `](` whose target is absolute is skipped one character at a
/// time, not as a whole match.
fn absolute(markdown: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let bytes = markdown.as_bytes();
    while i < bytes.len() {
        if markdown[i..].starts_with("](") {
            let target_start = i + 2;
            let rest = &markdown[target_start..];
            let external = rest.starts_with("http://") || rest.starts_with("https://");
            let end = rest.find(')');
            if let (false, Some(end)) = (external, end) {
                if end > 0 {
                    out.push_str("](");
                    out.push_str(BLOB);
                    out.push_str(&rest[..end]);
                    out.push(')');
                    i = target_start + end + 1;
                    continue;
                }
            }
        }
        let c = markdown[i..].chars().next().expect("in bounds");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn pair(row: &[String]) -> Result<(&str, &str), String> {
    match row {
        [a, b] => Ok((a, b)),
        _ => Err(format!("expected two cells, got {row:?}")),
    }
}

/// `site import-sdk`: the SDK README's crate and example tables.
pub fn sdk() -> Result<(), String> {
    let site = site_root();
    let text = read_text(&site.join("../sdk/README.md"))?;
    let mut crates = Vec::new();
    for row in table(&text, "Crates")? {
        let (cell, purpose) = pair(&row)?;
        let name = name_of(cell)?;
        crates.push(obj! {"name" => name.clone(), "url" => format!("{BLOB}crates/{name}"), "purpose" => absolute(purpose)});
    }
    let mut examples = Vec::new();
    for row in table(&text, "Examples")? {
        let (cell, shows) = pair(&row)?;
        let name = name_of(cell)?;
        examples.push(obj! {"name" => name.clone(), "url" => format!("{BLOB}examples/{name}"), "shows" => absolute(shows)});
    }
    let (n_crates, n_examples) = (crates.len(), examples.len());
    let data = obj! {"source" => "sdk/README.md", "crates" => Json::Arr(crates), "examples" => Json::Arr(examples)};
    write(
        &site.join("data/sdk.json"),
        dumps(&data, Some(2), false) + "\n",
    )?;
    println!("{n_crates} crates, {n_examples} examples -> data/sdk.json");
    Ok(())
}

const TIERS: [&str; 7] = [
    "none", "boots", "intro", "menu", "in-game", "playable", "perfect",
];

/// `site import-compat [MD]`: the emulator's "Commercial discs" table.
pub fn compat(args: &[String]) -> Result<(), String> {
    let site = site_root();
    let src = args
        .first()
        .map_or_else(|| site.join("data/compat-source.md"), PathBuf::from);
    let text = read_text(&src)?;
    let run_on = Regex::new(r"(?m)^Run on: (\S+)")
        .expect("valid regex")
        .captures(&text)
        .map(|c| c[1].to_string());
    let lines: Vec<&str> = section(&text, "## Commercial discs")?
        .lines()
        .filter(|l| l.starts_with('|'))
        .collect();
    let header = cells(lines.first().ok_or("no compatibility table")?);
    let mut games = Vec::new();
    let mut tiers = Vec::new();
    for line in lines.iter().skip(2) {
        // dict(zip(header, cells)): a repeated column keeps its last value.
        let row: BTreeMap<&str, String> =
            header.iter().map(String::as_str).zip(cells(line)).collect();
        let field = |key: &str| {
            row.get(key)
                .cloned()
                .ok_or_else(|| format!("no {key} column"))
        };
        let tier = field("PSoXide")?;
        if !TIERS.contains(&tier.as_str()) {
            return Err(format!("unknown tier '{tier}' in {}", field("Game")?));
        }
        tiers.push(tier.clone());
        games.push(obj! {
            "name" => field("Game")?,
            "serial" => field("Serial")?,
            "region" => field("Region")?,
            "tier" => tier,
            "bios_tier" => field("Real BIOS (recorded)")?,
            "fmv" => field("FMV")?,
            "issues" => field("Known issues")?,
            "measured_on" => field("Measured on")?,
            "evidence" => field("Evidence")?,
        });
    }
    let counts: Vec<(String, Json)> = TIERS
        .iter()
        .map(|t| {
            (
                t.to_string(),
                Json::from(tiers.iter().filter(|x| x == t).count()),
            )
        })
        .collect();
    let counts_repr = counts
        .iter()
        .map(|(t, n)| format!("'{t}': {}", dumps(n, None, true)))
        .collect::<Vec<_>>()
        .join(", ");
    let total = games.len();
    let data = obj! {
        "source" => "PSoXide-emulator docs/COMPATIBILITY.md",
        "run_on" => run_on,
        "tiers" => TIERS.to_vec(),
        "counts" => Json::Obj(counts),
        "total" => total,
        "games" => Json::Arr(games),
    };
    write(
        &site.join("data/compat.json"),
        dumps(&data, Some(1), true) + "\n",
    )?;
    println!("wrote data/compat.json: {total} games, {{{counts_repr}}}");
    Ok(())
}

/// Python's `csv.DictReader` over the default (excel) dialect.
fn csv_rows(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut started = false;
    while let Some(c) = chars.next() {
        started = true;
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                // A blank line is skipped, as csv.reader does.
                if !(record.len() == 1 && record[0].is_empty()) {
                    records.push(std::mem::take(&mut record));
                }
                record.clear();
                started = false;
            }
            _ => field.push(c),
        }
    }
    if started {
        record.push(field);
        records.push(record);
    }
    let mut rows = records.into_iter();
    let Some(header) = rows.next() else {
        return Vec::new();
    };
    rows.map(|values| header.iter().cloned().zip(values).collect())
        .collect()
}

/// Python's `float()` on a CSV cell; `None` for an empty one.
fn num(value: Option<&String>) -> Result<Option<f64>, String> {
    match value.map(|v| v.trim()) {
        None | Some("") => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| format!("could not convert string to float: '{v}'")),
    }
}

/// `sum()` of floats in CPython 3.12+: Neumaier-compensated.
fn py_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let (mut total, mut compensation) = (0.0f64, 0.0f64);
    for x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            compensation += (total - t) + x;
        } else {
            compensation += (x - t) + total;
        }
        total = t;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

fn geomean(values: &[f64]) -> f64 {
    (py_sum(values.iter().map(|v| v.ln())) / values.len() as f64).exp()
}

/// The smallest round number >= x, used as a chart axis end. Python's
/// `m * 10 ** exp` is an integer when both are, so the type follows it.
fn nice_ceiling(x: f64) -> Json {
    let exp = x.log10().floor() as i32;
    for (m, whole) in [
        (1.0, true),
        (1.5, false),
        (2.0, true),
        (2.5, false),
        (3.0, true),
        (4.0, true),
        (5.0, true),
        (6.0, true),
        (8.0, true),
        (10.0, true),
    ] {
        let value = m * 10f64.powf(f64::from(exp));
        if value >= x {
            return if whole && exp >= 0 {
                Json::Int(value as i64)
            } else {
                Json::Float(value)
            };
        }
    }
    Json::Null
}

const NTSC_FIELD_HZ: f64 = 59.826;

const GAMES: [(&str, &str); 6] = [
    ("crash", "Crash Bandicoot"),
    ("tekken3", "Tekken 3"),
    ("wipeout2097", "WipEout 2097"),
    ("re2", "Resident Evil 2"),
    ("mslugx", "Metal Slug X"),
    ("hl", "Half-Life (homebrew disc)"),
];

/// Averages use the five commercial games every emulator booted. Beetle PSX
/// could not boot the Half-Life disc, so it stays out of the mean.
const AVERAGE_OVER: [&str; 5] = ["crash", "tekken3", "wipeout2097", "re2", "mslugx"];

/// Which harness rows appear on the page, in order: id in the CSV, display
/// name, build, CPU core, role. Anything else in the CSV is ignored.
const EMULATORS: [(&str, &str, &str, &str, &str); 9] = [
    (
        "psoxide-22ae5d6",
        "PSoXide",
        "22ae5d6",
        "interpreter",
        "current",
    ),
    (
        "psoxide-9eff8c8",
        "PSoXide",
        "9eff8c8",
        "interpreter",
        "history",
    ),
    (
        "psoxide-aacadfd",
        "PSoXide",
        "aacadfd",
        "interpreter",
        "history",
    ),
    (
        "duckstation-interp",
        "DuckStation",
        "459a6fc",
        "interpreter",
        "peer",
    ),
    (
        "duckstation-rec",
        "DuckStation",
        "459a6fc",
        "recompiler",
        "peer",
    ),
    (
        "beetle-interp",
        "Beetle PSX",
        "0.9.44.1 (ee042b7)",
        "interpreter",
        "peer",
    ),
    (
        "beetle-dynarec",
        "Beetle PSX",
        "0.9.44.1 (ee042b7)",
        "recompiler",
        "peer",
    ),
    (
        "redux-interp",
        "PCSX-Redux",
        "1b7a4f1d",
        "interpreter",
        "peer",
    ),
    (
        "redux-dynarec",
        "PCSX-Redux",
        "1b7a4f1d",
        "recompiler",
        "peer",
    ),
];

const METRICS: [(&str, &str); 3] = [
    ("fps", "eff_fps_per_cpu_s"),
    ("cpu", "rt_cpu_pct"),
    ("mem", "rt_footprint_mb"),
];

fn max(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().fold(f64::NEG_INFINITY, f64::max)
}

/// `site import-emu-bench [CSV]`: select, rename and average the harness
/// summary. The page never computes anything the harness didn't measure.
pub fn emu_bench(args: &[String]) -> Result<(), String> {
    let site = site_root();
    let src = args.first().map_or_else(
        || site.join("data/emu-bench/summary-final.csv"),
        PathBuf::from,
    );
    let rows =
        csv_rows(&std::fs::read_to_string(&src).map_err(|e| format!("{}: {e}", src.display()))?);
    let mut by_emu: BTreeMap<&str, BTreeMap<&str, &BTreeMap<String, String>>> = BTreeMap::new();
    for row in &rows {
        let emu = row.get("emu").map_or("", String::as_str);
        let game = row.get("game").map_or("", String::as_str);
        by_emu.entry(emu).or_default().insert(game, row);
    }

    // (psoxide, role, mean) per emulator, kept for the axis maxima.
    let mut means: Vec<(bool, &str, BTreeMap<&str, f64>)> = Vec::new();
    let mut emulators = Vec::new();
    for (emu_id, name, build, core, role) in EMULATORS {
        let games = by_emu
            .get(emu_id)
            .filter(|g| !g.is_empty())
            .ok_or_else(|| format!("{}: no rows for {emu_id}", src.display()))?;
        let mut per_game = Vec::new();
        for (game_id, _) in GAMES {
            let Some(r) = games.get(game_id) else {
                per_game.push(obj! {"game" => game_id, "measured" => false});
                continue;
            };
            let mut entry = vec![
                ("game".to_string(), Json::from(game_id)),
                ("measured".to_string(), Json::from(true)),
            ];
            for (key, col) in METRICS {
                entry.push((key.to_string(), num(r.get(col))?.into()));
                entry.push((
                    format!("{key}_min"),
                    num(r.get(&format!("{col}_min")))?.into(),
                ));
                entry.push((
                    format!("{key}_max"),
                    num(r.get(&format!("{col}_max")))?.into(),
                ));
                let n = r
                    .get(&format!("{col}_n"))
                    .and_then(|n| n.trim().parse::<i64>().ok())
                    .ok_or_else(|| format!("{emu_id} {game_id}: bad {col}_n"))?;
                entry.push((format!("{key}_n"), Json::Int(n)));
            }
            per_game.push(Json::Obj(entry));
        }
        let mut mean = BTreeMap::new();
        let mut mean_json = Vec::new();
        for (key, col) in METRICS {
            let values = AVERAGE_OVER
                .iter()
                .map(|g| {
                    num(games.get(g).and_then(|r| r.get(col)))?
                        .ok_or_else(|| format!("{emu_id} {g}: no {col}"))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let value = round(geomean(&values), 2);
            mean.insert(key, value);
            mean_json.push((key.to_string(), Json::Float(value)));
        }
        let headroom = round(mean["fps"] / NTSC_FIELD_HZ, 1);
        mean.insert("headroom", headroom);
        mean_json.push(("headroom".to_string(), Json::Float(headroom)));
        let psoxide = name == "PSoXide";
        means.push((psoxide, role, mean));
        emulators.push(obj! {
            "id" => emu_id,
            "name" => name,
            "build" => build,
            "core" => core,
            "role" => role,
            "psoxide" => psoxide,
            "needs_bios" => !psoxide,
            "label" => if psoxide { format!("{name} {build}") } else { format!("{name} ({core})") },
            "mean" => Json::Obj(mean_json),
            "per_game" => Json::Arr(per_game),
        });
    }

    let peer_means: Vec<&BTreeMap<&str, f64>> = means
        .iter()
        .filter(|(_, role, _)| *role != "history")
        .map(|(_, _, m)| m)
        .collect();
    let history_means: Vec<&BTreeMap<&str, f64>> = means
        .iter()
        .filter(|(psoxide, _, _)| *psoxide)
        .map(|(_, _, m)| m)
        .collect();
    let over = |keys: &[&str], axis: bool, rows: &[&BTreeMap<&str, f64>]| {
        Json::Obj(
            keys.iter()
                .map(|k| {
                    let m = max(rows.iter().map(|mean| mean[k]));
                    (
                        k.to_string(),
                        if axis {
                            nice_ceiling(m)
                        } else {
                            Json::Float(m)
                        },
                    )
                })
                .collect(),
        )
    };
    let source_csv = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let count = emulators.len();
    let data = obj! {
        "source_csv" => source_csv,
        "field_hz" => NTSC_FIELD_HZ,
        "games" => Json::Arr(GAMES.iter().map(|(g, n)| obj! {"id" => *g, "name" => *n}).collect()),
        "average_over" => AVERAGE_OVER.to_vec(),
        "emulators" => Json::Arr(emulators),
        "max" => over(&["fps", "cpu", "mem", "headroom"], false, &peer_means),
        "axis" => over(&["fps", "cpu", "mem", "headroom"], true, &peer_means),
        "axis_history" => over(&["fps", "cpu", "mem"], true, &history_means),
        "max_history" => over(&["fps", "mem"], false, &history_means),
    };
    write(
        &site.join("data/emu-bench.json"),
        dumps(&data, Some(1), true) + "\n",
    )?;
    println!(
        "wrote data/emu-bench.json from {} ({count} emulators)",
        display(&src)
    );
    Ok(())
}

/// `str(Path)`.
fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::float_repr;

    #[test]
    fn relative_links_point_at_github() {
        assert_eq!(
            absolute("see [x](crates/a) and [y](https://e.org/b)"),
            format!("see [x]({BLOB}crates/a) and [y](https://e.org/b)")
        );
        assert_eq!(
            absolute("[](https://a.b/c](d)"),
            format!("[](https://a.b/c]({BLOB}d)")
        );
        assert_eq!(absolute("[x]()"), "[x]()");
    }

    #[test]
    fn csv_handles_quotes_and_blank_lines() {
        let rows = csv_rows("a,b\n\"x, y\",\"q\"\"z\"\n\n1,2\r\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["a"], "x, y");
        assert_eq!(rows[0]["b"], "q\"z");
        assert_eq!(rows[1]["b"], "2");
    }

    #[test]
    fn sum_is_compensated_like_cpython() {
        // math.fsum-like result that naive left-to-right addition misses.
        assert_eq!(py_sum([1e16, 1.0, -1e16]), 1.0);
        assert_eq!(py_sum([0.1; 10]), 1.0);
    }

    #[test]
    fn axis_ends_keep_python_number_types() {
        assert_eq!(nice_ceiling(73.0), Json::Int(80));
        assert_eq!(nice_ceiling(140.0), Json::Float(150.0));
        assert_eq!(
            float_repr(match nice_ceiling(0.27) {
                Json::Float(f) => f,
                other => panic!("{other:?}"),
            }),
            "0.30000000000000004"
        );
    }
}
