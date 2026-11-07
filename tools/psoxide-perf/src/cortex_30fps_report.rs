//! Summarise cortex replay captures and compare lockstep visual hashes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::util::{
    dict_rows, group_float, group_thousands, parse_int, read_text, round_int, usage_error, Cli,
    Error, Result, Token,
};

const NTSC_HZ: f64 = 60.0;
const TWO_VBLANK_CYCLES: i64 = 1_128_960;

/// A CSV whose cells are all integers (`int(value or 0)`), with the column
/// order of its header.
pub struct IntRows {
    header: Vec<String>,
    rows: Vec<Vec<i64>>,
}

impl IntRows {
    fn column(&self, name: &str) -> Result<usize> {
        self.header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| Error(format!("KeyError: '{name}'")))
    }

    fn has(&self, name: &str) -> bool {
        self.header.iter().any(|h| h == name)
    }

    fn cell(&self, row: usize, name: &str) -> Result<i64> {
        Ok(self.rows[row][self.column(name)?])
    }
}

/// Older route logs can have a short initial row while the first pad poll is
/// still being assembled. An absent trailing field is semantically the same as
/// the counter's zero initial value.
pub fn integer_rows(path: &Path) -> Result<IntRows> {
    let (header, rows) = dict_rows(&read_text(path)?)?;
    let mut out = Vec::new();
    for row in &rows {
        let mut cells = Vec::new();
        for name in &header {
            let text = row.get(name).unwrap_or("");
            cells.push(if text.is_empty() { 0 } else { parse_int(text)? });
        }
        out.push(cells);
    }
    Ok(IntRows { header, rows: out })
}

/// Linear-interpolated percentile of integer samples, rounded half to even.
pub fn percentile(values: &[i64], fraction: f64) -> i64 {
    let mut ordered = values.to_vec();
    ordered.sort();
    let position = (ordered.len() - 1) as f64 * fraction;
    let lower = position as usize;
    let upper = (lower + 1).min(ordered.len() - 1);
    let weight = position - lower as f64;
    round_int(ordered[lower] as f64 + (ordered[upper] - ordered[lower]) as f64 * weight)
}

fn gameplay_start(rows: &IntRows) -> Result<usize> {
    let chunks = rows.column("room_active_chunks")?;
    let surfaces = rows.column("room_surfaces_considered")?;
    rows.rows
        .iter()
        .position(|r| r[chunks] > 0 && r[surfaces] > 0)
        .ok_or_else(|| Error("StopIteration".to_string()))
}

fn mean(values: &[i64]) -> Result<f64> {
    if values.is_empty() {
        return Err(Error("mean requires at least one data point".to_string()));
    }
    Ok(values.iter().sum::<i64>() as f64 / values.len() as f64)
}

fn mean_counter(rows: &IntRows, selected: &[usize], name: &str) -> Result<Option<f64>> {
    if selected.is_empty() || !rows.has(name) {
        return Ok(None);
    }
    let column = rows.column(name)?;
    let values: Vec<i64> = selected.iter().map(|&i| rows.rows[i][column]).collect();
    mean(&values).map(Some)
}

fn format_count(value: Option<f64>) -> String {
    value.map_or("n/a".to_string(), |v| format!("{v:.1}"))
}

/// One benchmark row, keyed by column title in display order.
pub type Summary = Vec<(&'static str, String)>;

/// Summarise one run directory (`<run>/profile.csv`, optional `route.csv`).
pub fn summarise(run_dir: &Path) -> Result<Summary> {
    let profile = integer_rows(&run_dir.join("profile.csv"))?;
    let start = gameplay_start(&profile)?;
    let gameplay: Vec<usize> = (start..profile.rows.len()).collect();
    let visual_frames = profile.column("visual_frames")?;
    let surfaces = profile.column("room_surfaces_considered")?;
    let render_col = profile.column("render")?;
    let update_col = profile.column("update")?;
    let visuals: Vec<usize> = gameplay
        .iter()
        .copied()
        .filter(|&i| profile.rows[i][visual_frames] > 0)
        .collect();
    let room_visuals: Vec<usize> = visuals
        .iter()
        .copied()
        .filter(|&i| profile.rows[i][surfaces] > 0)
        .collect();
    let render: Vec<i64> = visuals
        .iter()
        .map(|&i| profile.rows[i][render_col])
        .collect();
    // Visual period work: render plus every update since the previous visual.
    let mut period_work = Vec::new();
    let mut pending_updates = 0;
    for &i in &gameplay {
        pending_updates += profile.rows[i][update_col];
        if profile.rows[i][visual_frames] > 0 {
            period_work.push(profile.rows[i][render_col] + pending_updates);
            pending_updates = 0;
        }
    }
    let visual_count: i64 = gameplay
        .iter()
        .map(|&i| profile.rows[i][visual_frames])
        .sum();
    let within_budget = period_work
        .iter()
        .filter(|&&v| v <= TWO_VBLANK_CYCLES)
        .count();

    let mut icache_stalls_per_visual = None;
    let route_path = run_dir.join("route.csv");
    if route_path.exists() {
        let route = integer_rows(&route_path)?;
        if !route.rows.is_empty() && route.has("icache_refill_stall_cycles_delta") {
            let gameplay_start_cycles = profile.cell(start, "start_bus_cycles")?;
            let mut gameplay_stalls = 0;
            for row in 0..route.rows.len() {
                if route.cell(row, "bus_cycles")? >= gameplay_start_cycles {
                    gameplay_stalls += route.cell(row, "icache_refill_stall_cycles_delta")?;
                }
            }
            if visual_count == 0 {
                return Err(Error("division by zero".to_string()));
            }
            icache_stalls_per_visual = Some(gameplay_stalls as f64 / visual_count as f64);
        }
    }
    if gameplay.is_empty() || period_work.is_empty() || render.is_empty() {
        return Err(Error("division by zero or empty data".to_string()));
    }
    let parent_name = run_dir
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(vec![
        ("run", parent_name),
        (
            "fps",
            format!(
                "{:.2}",
                NTSC_HZ * visual_count as f64 / gameplay.len() as f64
            ),
        ),
        (
            "visuals/ticks",
            format!("{visual_count}/{}", gameplay.len()),
        ),
        ("render mean", group_float(mean(&render)?, 0)),
        ("render p95", group_thousands(percentile(&render, 0.95))),
        (
            "render max",
            group_thousands(*render.iter().max().expect("non-empty")),
        ),
        (
            "period <=2vb",
            format!(
                "{:.1}%",
                100.0 * within_budget as f64 / period_work.len() as f64
            ),
        ),
        ("I$ stalls", format_count(icache_stalls_per_visual)),
        (
            "surfaces",
            format_count(mean_counter(
                &profile,
                &room_visuals,
                "room_surfaces_considered",
            )?),
        ),
        (
            "TR candidates",
            format_count(mean_counter(
                &profile,
                &room_visuals,
                "room_surf_tr_subdivision_candidates",
            )?),
        ),
        (
            "TR submitted",
            format_count(mean_counter(
                &profile,
                &room_visuals,
                "room_surf_tr_subdivision_submitted",
            )?),
        ),
        (
            "primitives",
            format_count(mean_counter(&profile, &room_visuals, "tri_primitives")?),
        ),
    ])
}

/// Print summaries as a Markdown table.
pub fn print_table(summaries: &[Summary], out: &mut dyn Write) -> Result<()> {
    let columns: Vec<&str> = summaries[0].iter().map(|(k, _)| *k).collect();
    writeln!(out, "| {} |", columns.join(" | "))?;
    writeln!(out, "|{}|", vec!["---"; columns.len()].join("|"))?;
    for summary in summaries {
        let cells: Vec<&str> = summary.iter().map(|(_, v)| v.as_str()).collect();
        writeln!(out, "| {} |", cells.join(" | "))?;
    }
    Ok(())
}

fn visual_hashes(path: &Path) -> Result<BTreeMap<i64, String>> {
    let (_, rows) = dict_rows(&read_text(&path.join("visual-hashes.csv"))?)?;
    let mut hashes = BTreeMap::new();
    for row in &rows {
        let frame = parse_int(row.get("guest_frame").ok_or("KeyError: 'guest_frame'")?)?;
        let hash = row.get("display_hash").ok_or("KeyError: 'display_hash'")?;
        hashes.insert(frame, hash.to_string());
    }
    Ok(hashes)
}

/// Compare `visual-hashes.csv` of two runs by guest frame. Returns the first
/// differing frame when the runs are not equivalent.
pub fn compare_hashes(
    baseline: &Path,
    candidate: &Path,
    out: &mut dyn Write,
) -> Result<Option<i64>> {
    let base = visual_hashes(baseline)?;
    let cand = visual_hashes(candidate)?;
    let missing: Vec<i64> = base
        .keys()
        .filter(|k| !cand.contains_key(k))
        .copied()
        .collect();
    let extra: Vec<i64> = cand
        .keys()
        .filter(|k| !base.contains_key(k))
        .copied()
        .collect();
    let mismatches: Vec<i64> = base
        .iter()
        .filter(|(k, v)| cand.get(k).is_some_and(|c| c != *v))
        .map(|(k, _)| *k)
        .collect();
    let common = base.keys().filter(|k| cand.contains_key(k)).count();
    writeln!(
        out,
        "\nlockstep visual hashes: matched={} mismatched={} missing={} extra={}",
        common - mismatches.len(),
        mismatches.len(),
        missing.len(),
        extra.len()
    )?;
    if mismatches.is_empty() && missing.is_empty() && extra.is_empty() {
        return Ok(None);
    }
    let first = [&mismatches, &missing, &extra]
        .into_iter()
        .find(|list| !list.is_empty())
        .map(|list| list[0]);
    Ok(first)
}

const USAGE: &str = "psoxide-perf cortex-30fps-report RUN_DIR... [--compare-lockstep]";

/// `psoxide-perf cortex-30fps-report RUN_DIR... [--compare-lockstep]`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let mut runs: Vec<PathBuf> = Vec::new();
    let mut lockstep = false;
    let mut cli = Cli::new(args);
    while let Some(token) = cli.next_token() {
        match token {
            Token::Flag(flag) if flag == "--compare-lockstep" => lockstep = true,
            Token::Flag(flag) => {
                return Ok(usage_error(
                    USAGE,
                    &format!("unrecognized arguments: {flag}"),
                ))
            }
            Token::Positional(text) => runs.push(text.into()),
        }
    }
    if runs.is_empty() {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: runs",
        ));
    }
    let mut summaries = Vec::new();
    for path in &runs {
        summaries.push(summarise(path)?);
    }
    print_table(&summaries, out)?;
    if lockstep {
        if runs.len() != 2 {
            return Ok(usage_error(
                USAGE,
                "--compare-lockstep requires exactly two run directories",
            ));
        }
        if let Some(first) = compare_hashes(&runs[0], &runs[1], out)? {
            out.flush()?;
            eprintln!("visual equivalence failed at guest frame {first}");
            return Ok(1);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_interpolates_and_rounds_half_even() {
        assert_eq!(percentile(&[10, 20, 30, 40], 0.5), 25);
        assert_eq!(percentile(&[1, 2], 0.5), 2);
        assert_eq!(percentile(&[5], 0.95), 5);
        assert_eq!(percentile(&[100, 200, 300, 400, 500], 0.95), 480);
    }

    fn write_run(dir: &Path, hashes: &str) -> PathBuf {
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(
            run.join("profile.csv"),
            "room_active_chunks,room_surfaces_considered,visual_frames,render,update,start_bus_cycles\n\
             0,0,0,5,1,0\n1,10,1,100,10,100\n1,10,0,0,10,200\n1,12,1,300,20,300\n",
        )
        .unwrap();
        std::fs::write(run.join("visual-hashes.csv"), hashes).unwrap();
        run
    }

    #[test]
    fn summary_counts_visuals_and_budgeted_periods() {
        let dir = tempfile::tempdir().unwrap();
        let run = write_run(dir.path(), "guest_frame,display_hash\n1,aa\n");
        let summary = summarise(&run).unwrap();
        let get = |key: &str| summary.iter().find(|(k, _)| *k == key).unwrap().1.clone();
        assert_eq!(get("visuals/ticks"), "2/3");
        assert_eq!(get("fps"), "40.00");
        assert_eq!(get("render mean"), "200");
        assert_eq!(get("period <=2vb"), "100.0%");
        assert_eq!(get("I$ stalls"), "n/a");
        assert_eq!(get("surfaces"), "11.0");
    }

    #[test]
    fn lockstep_comparison_reports_the_first_difference() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_run(
            &dir.path().join("a"),
            "guest_frame,display_hash\n1,aa\n2,bb\n",
        );
        let b = write_run(
            &dir.path().join("b"),
            "guest_frame,display_hash\n1,aa\n2,cc\n3,dd\n",
        );
        let mut out = Vec::new();
        assert_eq!(compare_hashes(&a, &b, &mut out).unwrap(), Some(2));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\nlockstep visual hashes: matched=1 mismatched=1 missing=0 extra=1\n"
        );
    }
}
