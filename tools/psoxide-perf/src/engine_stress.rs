//! The engine-stress benchmark tools (`benchmarks/engine-stress`): the scenario
//! runner, the telemetry analyser, the route measurer and the outdoor area
//! calibration sweep. They drive `psxed-mcp` (through [`crate::mcp_client`]) to
//! author projects, `frontend build-project-disc` to bake them and
//! `frontend launch` to replay them, and keep every receipt next to the runs.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::mcp_client::{first_text, repo_root, Client};
use crate::pyjson::Json;
use crate::util::{relpath, round_int, round_places, usage_error, Error, IntTable, Result};

const HZ: i128 = 33_868_800;
const VBLANK: i128 = 571_240;

fn table_error<T>(message: &str) -> Result<T> {
    Err(Error(message.to_string()))
}

// ---------------------------------------------------------------- measure-route

/// `sorted(xs)[int((len(xs) - 1) * p)]`.
fn pct_index(len: usize, p: f64) -> usize {
    ((len - 1) as f64 * p) as usize
}

/// `round(statistics.mean(values), places)`: the mean of integers is an `int`
/// when it divides exactly, and `round()` keeps it one.
fn mean_rounded(values: &[i128], places: usize) -> Json {
    let sum: i128 = values.iter().sum();
    if sum % values.len() as i128 == 0 {
        Json::Int((sum / values.len() as i128) as i64)
    } else {
        Json::Float(round_places(mean_int(values), places))
    }
}

fn mean_int(values: &[i128]) -> f64 {
    values.iter().sum::<i128>() as f64 / values.len() as f64
}

/// Measure displayed cadence without guest instrumentation; enrich with
/// telemetry if present. Normal Play builds intentionally omit
/// emulator-telemetry. Missing counters are unknown, not zero. Performance
/// acceptance must use the normal guest; instrumented stage costs are
/// diagnostic and may perturb the workload being measured.
pub fn measure(root: &Path, first: i128, last: i128) -> Result<Json> {
    let route = IntTable::read(&root.join("route.csv"))?;
    let mut routes = Vec::new();
    for row in 0..route.rows.len() {
        let polls = route.at(row, "port1_polls")?;
        if first <= polls && polls <= last {
            routes.push(row);
        }
    }
    if routes.len() < 3 {
        return table_error("insufficient route samples");
    }
    let last_row = *routes.last().expect("non-empty");
    if route.at(last_row, "port1_polls")? < last - 4 {
        return table_error("requested poll window was not completed");
    }
    let mut flips = Vec::new();
    for &row in &routes {
        if route.at(row, "display_start_changed")? != 0 {
            flips.push(row);
        }
    }
    let mut intervals = Vec::new();
    for pair in flips.windows(2) {
        intervals.push(route.at(pair[1], "bus_cycles")? - route.at(pair[0], "bus_cycles")?);
    }
    if intervals.len() < 30 {
        return table_error("insufficient displayed frames");
    }
    let long = intervals.iter().filter(|&&i| i > 2 * VBLANK + 100).count() as i64;
    let mut sorted = intervals.clone();
    sorted.sort();
    let total: i128 = intervals.iter().sum();
    let p95 = sorted[pct_index(sorted.len(), 0.95)];
    let max = *sorted.last().expect("non-empty");
    let mut result = Json::object([
        (
            "first_poll",
            Json::Int(route.at(routes[0], "port1_polls")? as i64),
        ),
        (
            "last_poll",
            Json::Int(route.at(last_row, "port1_polls")? as i64),
        ),
        ("display_intervals", Json::from(intervals.len())),
        (
            "display_fps",
            Json::Float(round_places(
                (intervals.len() as i128 * HZ) as f64 / total as f64,
                3,
            )),
        ),
        (
            "interval_ms_p95",
            Json::Float(round_places(p95 as f64 / HZ as f64 * 1000.0, 3)),
        ),
        (
            "interval_ms_max",
            Json::Float(round_places(max as f64 / HZ as f64 * 1000.0, 3)),
        ),
        ("intervals_over_two_vblanks", Json::Int(long)),
        ("cadence_pass", Json::Bool(long == 0)),
        ("telemetry_available", Json::Bool(false)),
        ("primitive_overflows", Json::Null),
        ("measured_pass", Json::Null),
    ]);
    let lo = route.at(routes[0], "bus_cycles")?;
    let hi = route.at(last_row, "bus_cycles")?;
    let profile_path = root.join("profile.csv");
    let mut rows: Vec<usize> = Vec::new();
    let profile = if profile_path.exists() {
        Some(IntTable::read(&profile_path)?)
    } else {
        None
    };
    if let Some(profile) = &profile {
        for row in 0..profile.rows.len() {
            if lo <= profile.at(row, "start_bus_cycles")?
                && profile.at(row, "end_bus_cycles")? <= hi
                && profile.at(row, "render")? > 0
            {
                rows.push(row);
            }
        }
    }
    if let (Some(profile), false) = (&profile, rows.is_empty()) {
        let column = |name: &str| -> Result<Vec<i128>> {
            rows.iter().map(|&r| profile.at(r, name)).collect()
        };
        let overflows: i128 = column("room_submit_primitive_overflows")?.iter().sum();
        let mut render = column("visual_render_task")?;
        render.sort();
        let mut mean_cycles = Json::Object(Vec::new());
        for key in [
            "visual_render_task",
            "room",
            "player",
            "model_instances",
            "update",
        ] {
            mean_cycles.set(key, Json::Int(round_int(mean_int(&column(key)?))));
        }
        let actor_draws = column("model_instance_draws")?;
        let mut camera_available = false;
        for &r in &rows {
            for key in ["camera_x_biased", "camera_y_biased", "camera_z_biased"] {
                camera_available |= profile.at(r, key)? != 1_000_000;
            }
        }
        let mut camera_ranges = Json::Object(Vec::new());
        for key in [
            "camera_x_biased",
            "camera_y_biased",
            "camera_z_biased",
            "player_view_yaw_q12",
        ] {
            let values = column(key)?;
            camera_ranges.set(
                key,
                Json::Array(vec![
                    Json::Int(*values.iter().min().expect("non-empty") as i64),
                    Json::Int(*values.iter().max().expect("non-empty") as i64),
                ]),
            );
        }
        result.set("telemetry_available", Json::Bool(true));
        result.set("render_samples", Json::from(rows.len()));
        result.set("primitive_overflows", Json::Int(overflows as i64));
        result.set(
            "render_cycles_p95",
            Json::Int(render[pct_index(render.len(), 0.95)] as i64),
        );
        result.set("mean_cycles", mean_cycles);
        result.set(
            "max_actor_draws",
            Json::Int(*actor_draws.iter().max().expect("non-empty") as i64),
        );
        result.set("measured_pass", Json::Bool(long == 0 && overflows == 0));
        result.set(
            "camera_position_telemetry_available",
            Json::Bool(camera_available),
        );
        result.set("camera_ranges", camera_ranges);
    }
    Ok(result)
}

/// `psoxide-perf engine-stress measure-route RUN_DIR FIRST_POLL LAST_POLL`.
fn measure_command(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let usage = "psoxide-perf engine-stress measure-route RUN_DIR FIRST_POLL LAST_POLL";
    let [dir, first, last] = args else {
        return Ok(usage_error(usage, "expected RUN_DIR FIRST_POLL LAST_POLL"));
    };
    let (Ok(first), Ok(last)) = (first.parse::<i128>(), last.parse::<i128>()) else {
        return Ok(usage_error(
            usage,
            "FIRST_POLL and LAST_POLL must be integers",
        ));
    };
    writeln!(
        out,
        "{}",
        measure(Path::new(dir), first, last)?.dumps(Some(2), false)
    )?;
    Ok(0)
}

// ---------------------------------------------------------------------- analyse

const STAGES: [&str; 10] = [
    "visual_render_task",
    "update",
    "room",
    "player",
    "model_instances",
    "sky",
    "far_vista",
    "present",
    "ot_wait",
    "world_flush",
];
const COUNTS: [&str; 6] = [
    "room_surfaces_considered",
    "tri_primitives",
    "model_instance_draws",
    "room_submit_primitive_overflows",
    "cd_room_chunk_loads",
    "visual_deadline_misses",
];

fn stress_output() -> PathBuf {
    let root = match std::env::var_os("PSOXIDE_STRESS_OUTPUT") {
        Some(path) => PathBuf::from(path),
        None => repo_root().join("build/engine-stress"),
    };
    resolve(&root)
}

/// `Path.resolve()`: symlinks resolved where the path exists, otherwise absolute.
fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn percentile_f(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[(sorted.len() - 1).min(pct_index(sorted.len(), p))])
}

/// Analyse the final 1,200 telemetry rows of one run directory; align emulator
/// counters by bus-cycle window. FPS uses emulator display-start changes, not
/// guest render calls. Stage timings overlap; never add parent and child stages.
/// GPU cost is the emulator's estimate, not silicon timing.
pub fn analyse(path: &Path) -> Result<Json> {
    let log = fs::read_to_string(path.join("run.log"))?;
    let polls = Regex::new(r"port1-polls=(\d+)")
        .expect("static pattern")
        .captures(&log);
    let reached = polls
        .as_ref()
        .and_then(|c| c[1].parse::<i64>().ok())
        .is_some_and(|n| (5400..=5404).contains(&n));
    if !reached {
        return Err(Error(format!(
            "AssertionError: {}: poll target not reached",
            path.display()
        )));
    }
    let profile = IntTable::read(&path.join("profile.csv"))?;
    if profile.rows.len() < 1200 {
        return Err(Error(format!(
            "{}: incomplete telemetry ({})",
            path.display(),
            profile.rows.len()
        )));
    }
    let rows: Vec<usize> = (profile.rows.len() - 1200..profile.rows.len()).collect();
    let lo = profile.at(rows[0], "start_bus_cycles")?;
    let hi = profile.at(rows[rows.len() - 1], "end_bus_cycles")?;
    if rows
        .iter()
        .any(|&r| profile.at_or_zero(r, "cd_room_chunk_loads") != 0)
    {
        return Err(Error(format!(
            "AssertionError: {}: streaming in measured window",
            path.display()
        )));
    }
    let mut render = Vec::new();
    for &r in &rows {
        if profile.at(r, "render")? > 0 {
            render.push(r);
        }
    }
    if render.is_empty() {
        return Err(Error(format!("AssertionError: {}", path.display())));
    }
    let route = IntTable::read(&path.join("route.csv"))?;
    let mut routes = Vec::new();
    for r in 0..route.rows.len() {
        let cycles = route.at(r, "bus_cycles")?;
        if lo <= cycles && cycles <= hi {
            routes.push(r);
        }
    }
    let mut route_ticks = BTreeSet::new();
    for &r in &routes {
        route_ticks.insert(route.at(r, "route_tick")?);
    }
    let gpu_table = IntTable::read(&path.join("gpu.csv"))?;
    let mut gpu = Vec::new();
    for r in 0..gpu_table.rows.len() {
        if route_ticks.contains(&gpu_table.at(r, "route_tick")?) {
            gpu.push(r);
        }
    }
    if routes.len() <= 2 || gpu.is_empty() {
        return Err(Error(format!("AssertionError: {}", path.display())));
    }
    // Full route intervals only, avoiding the partially overlapping first interval.
    let routes = &routes[1..];
    let mut ticks = BTreeSet::new();
    for &r in routes {
        ticks.insert(route.at(r, "route_tick")?);
    }
    let mut kept = Vec::new();
    for &g in &gpu {
        if ticks.contains(&gpu_table.at(g, "route_tick")?) {
            kept.push(g);
        }
    }
    let gpu = kept;
    let mut wall: i128 = 0;
    for &r in routes {
        wall += route.at(r, "bus_cycle_delta")?;
    }
    let mut flips = Vec::new();
    for &r in routes {
        if route.at(r, "display_start_changed")? != 0 {
            flips.push(r);
        }
    }
    let mut intervals = Vec::new();
    for pair in flips.windows(2) {
        let delta = route.at(pair[1], "bus_cycles")? - route.at(pair[0], "bus_cycles")?;
        intervals.push(delta as f64 / HZ as f64 * 1000.0);
    }
    let audit = fs::read_to_string(
        path.parent()
            .expect("run dir has a parent")
            .join("audit.txt"),
    )?;
    let faces = Regex::new(r"(\d+) world faces")
        .expect("static pattern")
        .captures(&audit);
    let mean_of = |selected: &[usize], key: &str| -> f64 {
        mean_int(
            &selected
                .iter()
                .map(|&r| profile.at_or_zero(r, key))
                .collect::<Vec<_>>(),
        )
    };
    let mut stages = Json::Object(Vec::new());
    for key in STAGES {
        let source: &[usize] = if key == "update" { &rows } else { &render };
        stages.set(key, Json::Int(round_int(mean_of(source, key))));
    }
    let mut counts = Json::Object(Vec::new());
    for key in COUNTS {
        let values: Vec<i128> = rows.iter().map(|&r| profile.at_or_zero(r, key)).collect();
        let render_values: Vec<i128> = render.iter().map(|&r| profile.at_or_zero(r, key)).collect();
        counts.set(
            key,
            Json::object([
                ("mean", mean_rounded(&render_values, 2)),
                (
                    "max",
                    Json::Int(*values.iter().max().expect("non-empty") as i64),
                ),
                ("sum", Json::Int(values.iter().sum::<i128>() as i64)),
            ]),
        );
    }
    if wall == 0 {
        return Err(Error("division by zero".to_string()));
    }
    let gpu_sum = |key: &str| -> Result<i128> {
        let mut total = 0;
        for &g in &gpu {
            total += gpu_table.at(g, key)?;
        }
        Ok(total)
    };
    let per_flip = flips.len().max(1) as f64;
    let rounded_interval = |p: f64| match percentile_f(&intervals, p) {
        Some(v) => Json::Float(round_places(v, 2)),
        None => Json::Int(0),
    };
    let mut camera_ranges = Json::Object(Vec::new());
    for key in [
        "camera_x_biased",
        "camera_y_biased",
        "camera_z_biased",
        "player_view_yaw_q12",
    ] {
        let mut values = Vec::new();
        for &r in &render {
            values.push(profile.at(r, key)?);
        }
        camera_ranges.set(
            key,
            Json::Array(vec![
                Json::Int(*values.iter().min().expect("non-empty") as i64),
                Json::Int(*values.iter().max().expect("non-empty") as i64),
            ]),
        );
    }
    let mut render_cycles = Vec::new();
    for &r in &render {
        render_cycles.push(profile.at(r, "visual_render_task")?);
    }
    render_cycles.sort();
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    Ok(Json::object([
        (
            "scenario",
            Json::Str(name(path.parent().expect("run dir has a parent"))),
        ),
        ("dma", Json::Str(name(path))),
        ("profile_rows", Json::from(rows.len())),
        ("render_calls", Json::from(render.len())),
        ("bus_start", Json::Int(lo as i64)),
        ("bus_end", Json::Int(hi as i64)),
        (
            "seconds",
            Json::Float(round_places(wall as f64 / HZ as f64, 3)),
        ),
        (
            "first_poll",
            Json::Int(route.at(routes[0], "port1_polls")? as i64),
        ),
        (
            "last_poll",
            Json::Int(route.at(routes[routes.len() - 1], "port1_polls")? as i64),
        ),
        ("display_flips", Json::from(flips.len())),
        (
            "display_fps",
            Json::Float(round_places(
                (flips.len() as i128 * HZ) as f64 / wall as f64,
                2,
            )),
        ),
        ("frame_interval_ms_p50", rounded_interval(0.5)),
        ("frame_interval_ms_p95", rounded_interval(0.95)),
        (
            "render_cycles_p95",
            Json::Int(render_cycles[pct_index(render_cycles.len(), 0.95)] as i64),
        ),
        (
            "world_faces",
            match faces {
                Some(c) => Json::Int(
                    c[1].parse()
                        .map_err(|_| Error("bad face count".to_string()))?,
                ),
                None => Json::Null,
            },
        ),
        ("stages", stages),
        ("counts", counts),
        (
            "gpu_cycles_per_flip",
            Json::Int(round_int(gpu_sum("gpu_cycles")? as f64 / per_flip)),
        ),
        (
            "gpu_busy_fraction",
            Json::Float(round_places(gpu_sum("gpu_cycles")? as f64 / wall as f64, 3)),
        ),
        (
            "gpu_textured_tris_per_flip",
            Json::Float(round_places(gpu_sum("textured_tris")? as f64 / per_flip, 1)),
        ),
        (
            "gpu_textured_quads_per_flip",
            Json::Float(round_places(
                gpu_sum("textured_quads")? as f64 / per_flip,
                1,
            )),
        ),
        ("camera_ranges", camera_ranges),
    ]))
}

fn float_of(value: &Json) -> f64 {
    match value {
        Json::Float(f) => *f,
        Json::Int(i) => *i as f64,
        _ => 0.0,
    }
}

fn text_of(value: &Json) -> String {
    match value {
        Json::Int(i) => i.to_string(),
        Json::Null => "None".to_string(),
        other => other.dumps(None, false),
    }
}

/// `psoxide-perf engine-stress analyse`: summarise every completed run under the
/// output directory as `summary.json` and a Markdown table.
fn analyse_command(args: &[String], out: &mut dyn Write) -> Result<i32> {
    if let Some(extra) = args.first() {
        return Ok(usage_error(
            "psoxide-perf engine-stress analyse",
            &format!("unrecognized arguments: {extra}"),
        ));
    }
    let root = stress_output();
    let mut runs: Vec<PathBuf> = Vec::new();
    let mut scenarios: Vec<PathBuf> = fs::read_dir(&root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    scenarios.sort();
    for scenario in scenarios {
        let mut modes: Vec<PathBuf> = fs::read_dir(&scenario)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("profile.csv").is_file())
            .collect();
        modes.sort();
        runs.extend(modes);
    }
    let mut results = Vec::new();
    for run in runs {
        if !run.join("complete.json").exists() {
            continue;
        }
        results.push(analyse(&run)?);
    }
    if results.is_empty() {
        return Err(Error(format!(
            "No completed stress runs found in {}",
            root.display()
        )));
    }
    let summary = Json::Array(results.clone());
    fs::write(root.join("summary.json"), summary.dumps(Some(2), false))?;
    writeln!(
        out,
        "| Scenario | DMA | faces | display fps | p95 interval ms | render kcy | room kcy | models kcy | vista kcy | GPU busy | redraws |"
    )?;
    writeln!(
        out,
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    )?;
    for r in &results {
        let get = |k: &str| r.get(k).cloned().unwrap_or(Json::Null);
        let stage =
            |k: &str| float_of(&get("stages").get(k).cloned().unwrap_or(Json::Null)) / 1000.0;
        let overflows = get("counts")
            .get("room_submit_primitive_overflows")
            .and_then(|c| c.get("sum"))
            .cloned()
            .unwrap_or(Json::Null);
        writeln!(
            out,
            "| {} | {} | {} | {:.2} | {:.2} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1}% | {} |",
            text_of(&get("scenario")).trim_matches('"'),
            text_of(&get("dma")).trim_matches('"'),
            text_of(&get("world_faces")),
            float_of(&get("display_fps")),
            float_of(&get("frame_interval_ms_p95")),
            stage("visual_render_task"),
            stage("room"),
            stage("model_instances"),
            stage("far_vista"),
            float_of(&get("gpu_busy_fraction")) * 100.0,
            text_of(&overflows)
        )?;
    }
    Ok(0)
}

// ------------------------------------------------------------------ run helpers

fn sha(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Run `command` in `repo`, sending stdout and stderr to the file `log`;
/// a non-zero exit is an error (`check=True`).
fn run_logged(command: &[String], log: &Path, repo: &Path, env: &[(String, String)]) -> Result<()> {
    let file = fs::File::create(log)?;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .current_dir(repo)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file))
        .status()
        .map_err(|e| Error(format!("{}: {e}", command[0])))?;
    if !status.success() {
        return Err(Error(format!(
            "Command '{}' returned non-zero exit status {}.",
            command.join(" "),
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

fn env_with(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in extra {
        match env.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.to_string(),
            None => env.push((key.to_string(), value.to_string())),
        }
    }
    env
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

/// `shutil.copy2`: copy the bytes and the modification time.
fn copy2(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).map_err(|e| Error(format!("{}: {e}", from.display())))?;
    let modified = fs::metadata(from)?.modified()?;
    fs::File::options()
        .write(true)
        .open(to)?
        .set_modified(modified)?;
    Ok(())
}

fn git_output(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    if !output.status.success() {
        return Err(Error(format!("git {} failed", args.join(" "))));
    }
    String::from_utf8(output.stdout).map_err(|e| Error(e.to_string()))
}

/// A call recorder around [`Client::call`]: every call and result is kept for
/// `mcp.json`, and an `isError` result aborts the sequence.
struct Recorder<'a> {
    client: &'a mut Client,
    calls: Vec<Json>,
}

impl Recorder<'_> {
    fn call(&mut self, tool: &str, args: Json) -> Result<Json> {
        let result = self.client.call(tool, args.clone())?;
        self.calls.push(Json::object([
            ("tool", Json::from(tool)),
            ("args", args),
            ("result", result.clone()),
        ]));
        if result.get("isError").is_some_and(Json::truthy) {
            return Err(Error(result.dumps(None, false)));
        }
        Ok(result)
    }

    fn call0(&mut self, tool: &str) -> Result<Json> {
        self.call(tool, Json::Object(Vec::new()))
    }

    fn brush(&mut self, index: i64) -> Result<Json> {
        self.call("get_brush", Json::object([("brush", Json::Int(index))]))
    }
}

fn ints(values: &[i64]) -> Json {
    Json::Array(values.iter().map(|&v| Json::Int(v)).collect())
}

fn add_box(recorder: &mut Recorder<'_>, lo: [i64; 3], hi: [i64; 3], material: &str) -> Result<()> {
    recorder.call(
        "add_shape",
        Json::object([
            ("shape", Json::from("box")),
            ("min", ints(&lo)),
            ("max", ints(&hi)),
            ("material", Json::from(material)),
        ]),
    )?;
    Ok(())
}

fn set_face_uv(recorder: &mut Recorder<'_>, count: i64) -> Result<()> {
    recorder.call(
        "set_face_uv",
        Json::object([
            ("first", Json::Int(0)),
            ("count", Json::Int(count)),
            ("scale_percent", ints(&[400, 400])),
        ]),
    )?;
    Ok(())
}

fn text_blocks(result: &Json) -> String {
    result
        .get("content")
        .and_then(Json::as_array)
        .unwrap_or_default()
        .iter()
        .filter(|b| b.get("type").and_then(Json::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Json::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn disc_files(cue_text: &str, dir: &Path) -> Result<Json> {
    let mut files = Json::Object(Vec::new());
    for captures in Regex::new(r#"FILE "([^"]+)""#)
        .expect("static pattern")
        .captures_iter(cue_text)
    {
        files.set(&captures[1], Json::Str(sha(&dir.join(&captures[1]))?));
    }
    Ok(files)
}

// ------------------------------------------------------------------------- run

const SCENARIOS: [(&str, &str); 11] = [
    ("e0", "e0"),
    ("e1", "e1"),
    ("e2", "base"),
    ("small2", "rsmall"),
    ("scale4", "base"),
    ("outdoor", "base"),
    ("vista8", "base"),
    ("vista16", "base"),
    ("patch4k", "base"),
    ("patch4k_scale4", "base"),
    ("outdoor_sealed", "base"),
];

const OUTDOOR_BOXES: [([i64; 3], [i64; 3]); 5] = [
    ([-12288, -256, -8192], [12288, 0, 20480]),
    ([-12544, 0, -8448], [-12288, 768, 20736]),
    ([12288, 0, -8448], [12544, 768, 20736]),
    ([-12288, 0, -8448], [12288, 768, -8192]),
    ([-12288, 0, 20480], [12288, 768, 20736]),
];
const SKY_BOXES: [([i64; 3], [i64; 3]); 5] = [
    ([-12544, 6144, -8448], [12544, 6400, 20736]),
    ([-12544, 768, -8448], [-12288, 6144, 20736]),
    ([12288, 768, -8448], [12544, 6144, 20736]),
    ([-12288, 768, -8448], [12288, 6144, -8192]),
    ([-12288, 768, 20480], [12288, 6144, 20736]),
];

struct Stress {
    repo: PathBuf,
    root: PathBuf,
    frontend: PathBuf,
}

impl Stress {
    fn new() -> Stress {
        let repo = repo_root();
        Stress {
            root: stress_output(),
            frontend: repo.join("target/release/frontend"),
            repo,
        }
    }

    /// Author, bake and replay one scenario; refuses to overwrite a project.
    fn scenario(&self, name: &str) -> Result<()> {
        let fixture = SCENARIOS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| *f)
            .ok_or_else(|| Error(name.to_string()))?;
        let out = self.root.join(name);
        fs::create_dir_all(&out)?;
        let project = out.join("project");
        if project.exists() {
            return Err(Error(format!(
                "Refusing to overwrite {}",
                project.display()
            )));
        }
        fs::create_dir(&project)?;
        let fixtures = self.repo.join("benchmarks/engine-stress/fixtures");
        copy2(
            &fixtures.join(fixture).join("project.ron"),
            &project.join("project.ron"),
        )?;
        let assets = self.repo.join("editor/projects/default/assets");
        std::os::unix::fs::symlink(relpath(&assets, &project), project.join("assets"))?;

        let mut client = Client::for_project(&self.repo, &project)?;
        let mut recorder = Recorder {
            client: &mut client,
            calls: Vec::new(),
        };
        let authored = (|| -> Result<()> {
            recorder.call0("status")?;
            if name == "scale4" || name == "patch4k_scale4" {
                for i in 0..6 {
                    recorder.brush(i)?;
                }
                set_face_uv(&mut recorder, 6)?;
                recorder.call0("save")?;
            }
            if matches!(name, "outdoor" | "vista8" | "vista16" | "outdoor_sealed") {
                for i in 0..6 {
                    recorder.brush(i)?;
                }
                recorder.call(
                    "delete",
                    Json::object([("first", Json::Int(0)), ("count", Json::Int(6))]),
                )?;
                for (lo, hi) in OUTDOOR_BOXES {
                    add_box(&mut recorder, lo, hi, "DP City / Megastructure Wall")?;
                }
                for i in 0..5 {
                    recorder.brush(i)?;
                }
                set_face_uv(&mut recorder, 5)?;
                if name == "outdoor_sealed" {
                    for (lo, hi) in SKY_BOXES {
                        add_box(&mut recorder, lo, hi, "DP Fog City Cube Sky")?;
                    }
                }
                recorder.call0("save")?;
            }
            Ok(())
        })();
        let first_calls = std::mem::take(&mut recorder.calls);
        let closed = client.close();
        authored?;
        closed?;

        let p = project.join("project.ron");
        let mut text = fs::read_to_string(&p)?;
        assert_project(&text)?;
        if name.starts_with("patch4k") {
            let (replaced, count) = replace_once(
                &text,
                r"bsp_patch_extent: 2048",
                "bsp_patch_extent: 4096",
                None,
            )?;
            if count != 1 {
                return Err(Error("AssertionError".to_string()));
            }
            text = replaced;
            fs::write(&p, &text)?;
        }
        if let Some(segments) = name.strip_prefix("vista") {
            let n: i64 = segments
                .parse()
                .map_err(|_| Error("invalid literal for int()".to_string()))?;
            let (replaced, count) = replace_once(
                &text,
                r"far_vista: \(enabled: false, texture: None,",
                "far_vista: (enabled: true, texture: Some((21)),",
                None,
            )?;
            if count != 1 {
                return Err(Error("AssertionError".to_string()));
            }
            let (replaced, count) = replace_once(
                &replaced,
                r"(far_vista: .*?segments: )\d+",
                "",
                Some(&n.to_string()),
            )?;
            if count != 1 {
                return Err(Error("AssertionError".to_string()));
            }
            text = replaced;
            fs::write(&p, &text)?;
        }

        let mut client = Client::for_project(&self.repo, &project)?;
        let audit_call = Json::object([("depth", Json::from("full"))]);
        let audit = client.call("audit", audit_call.clone());
        let closed = client.close();
        let audit = audit?;
        closed?;
        let mut calls = first_calls;
        calls.push(Json::object([
            ("tool", Json::from("audit")),
            ("args", audit_call),
            ("result", audit.clone()),
        ]));
        if audit.get("isError").is_some_and(Json::truthy) {
            return Err(Error(audit.dumps(None, false)));
        }
        fs::write(
            out.join("mcp.json"),
            Json::Array(calls).dumps(Some(2), false),
        )?;
        fs::write(out.join("audit.txt"), text_blocks(&audit))?;
        copy2(&p, &out.join("project.ron"))?;
        let features = "cd-stream-bench emulator-telemetry";
        let env = env_with(&[("EDITOR_PLAYTEST_FEATURES", features)]);
        println!("{name}: build");
        let _ = std::io::stdout().flush();
        run_logged(
            &[
                path_arg(&self.frontend),
                "build-project-disc".into(),
                "--project".into(),
                path_arg(&project),
            ],
            &out.join("build.log"),
            &self.repo,
            &env,
        )?;
        let build_log = fs::read_to_string(out.join("build.log"))?;
        let cue_line = build_log
            .lines()
            .map(str::trim)
            .rfind(|l| l.ends_with(".cue"))
            .ok_or_else(|| Error("list index out of range".to_string()))?;
        let cue = PathBuf::from(cue_line);
        copy2(
            &self
                .repo
                .join("engine/examples/editor-playtest/generated/level_manifest.cooked.rs"),
            &out.join("level_manifest.cooked.rs"),
        )?;
        let cue_text = fs::read_to_string(&cue)?;
        let manifest = Json::object([
            ("scenario", Json::from(name)),
            ("project", Json::Str(path_arg(&project))),
            ("cue", Json::Str(path_arg(&cue))),
            ("features", Json::from(features)),
            ("project_sha256", Json::Str(sha(&p)?)),
            ("frontend_sha256", Json::Str(sha(&self.frontend)?)),
            (
                "editor_head",
                Json::Str(
                    git_output(&self.repo, &["rev-parse", "HEAD"])?
                        .trim()
                        .to_string(),
                ),
            ),
            ("editor_diff", Json::Str(git_output(&self.repo, &["diff"])?)),
            (
                "components",
                Json::parse(&fs::read_to_string(self.repo.join("components.lock.json"))?)?,
            ),
            ("cue_content", Json::Str(cue_text.clone())),
            (
                "disc_files",
                disc_files(&cue_text, cue.parent().expect("cue has a parent"))?,
            ),
        ]);
        fs::write(out.join("manifest.json"), manifest.dumps(Some(2), false))?;
        self.replay(&out, &cue)?;
        println!("{name}: done");
        Ok(())
    }

    /// Find a retained test disc after its project was moved out of the picker.
    fn resolve_retained_cue(&self, cue: &Path) -> Result<PathBuf> {
        if cue.exists() {
            return Ok(cue.to_path_buf());
        }
        let archive = self.repo.join("editor/archive/local-tests");
        let project = cue
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name);
        let mut matches = Vec::new();
        if let (Some(project), Some(file)) = (project, cue.file_name()) {
            let mut dirs: Vec<PathBuf> = fs::read_dir(&archive)
                .map(|entries| entries.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            dirs.sort();
            for dir in dirs {
                let candidate = dir.join(project).join("baked").join(file);
                if candidate.exists() {
                    matches.push(candidate);
                }
            }
        }
        match matches.len() {
            1 => Ok(matches.remove(0)),
            _ => Err(Error(format!(
                "Disc missing or archive location ambiguous: {}",
                cue.display()
            ))),
        }
    }

    /// Replay a retained scenario in both DMA models, verifying hashes first.
    fn replay(&self, out: &Path, cue: &Path) -> Result<()> {
        let cue = self.resolve_retained_cue(cue)?;
        let name = out
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest = Json::parse(&fs::read_to_string(out.join("manifest.json"))?)?;
        let expected_frontend = manifest
            .get("frontend_sha256")
            .and_then(Json::as_str)
            .unwrap_or("");
        if sha(&self.frontend)? != expected_frontend {
            return Err(Error(
                "AssertionError: Frontend changed: establish a new baseline".to_string(),
            ));
        }
        for (filename, expected) in manifest
            .get("disc_files")
            .and_then(Json::as_object)
            .unwrap_or_default()
        {
            if Some(sha(&cue.parent().expect("cue has a parent").join(filename))?.as_str())
                != expected.as_str()
            {
                return Err(Error(
                    "AssertionError: Disc changed: establish a new baseline".to_string(),
                ));
            }
        }
        let outcomes: Vec<Result<()>> = std::thread::scope(|scope| {
            let handles: Vec<_> = ["legacy", "fifo"]
                .into_iter()
                .map(|mode| {
                    let (name, cue) = (name.clone(), cue.clone());
                    scope.spawn(move || self.replay_mode(out, &name, &cue, mode))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("replay thread"))
                .collect()
        });
        outcomes.into_iter().collect()
    }

    fn replay_mode(&self, out: &Path, name: &str, cue: &Path, mode: &str) -> Result<()> {
        let dest = out.join(mode);
        fs::create_dir_all(&dest)?;
        let marker = dest.join("complete.json");
        if marker.exists() {
            fs::remove_file(&marker)?;
        }
        let fifo = if mode == "fifo" { "1" } else { "0" };
        let env = env_with(&[("PSOXIDE_EXPERIMENTAL_DMA_FIFO", fifo)]);
        let command: Vec<String> = vec![
            path_arg(&self.frontend),
            "launch".into(),
            "--path".into(),
            path_arg(cue),
            "--embedded-playtest".into(),
            "--stop-at-poll=5400".into(),
            "--steps=4860000000".into(),
            "--profile-log".into(),
            path_arg(&dest.join("profile.csv")),
            "--counter-log".into(),
            path_arg(&dest.join("counter.csv")),
            "--route-log".into(),
            path_arg(&dest.join("route.csv")),
            "--gpu-frame-stats-log".into(),
            path_arg(&dest.join("gpu.csv")),
            "--cd-command-log".into(),
            path_arg(&dest.join("cd.log")),
            "--route-screenshot-dir".into(),
            path_arg(&dest.join("shots")),
            "--route-screenshot-interval=1800".into(),
        ];
        fs::write(
            dest.join("command.json"),
            Json::Array(command.iter().cloned().map(Json::Str).collect()).dumps(Some(2), false),
        )?;
        println!("{name}: {mode}");
        let _ = std::io::stdout().flush();
        run_logged(&command, &dest.join("run.log"), &self.repo, &env)?;
        fs::write(
            marker,
            Json::object([
                ("dma_fifo", Json::from(fifo)),
                ("completed", Json::Bool(true)),
            ])
            .dumps(None, false),
        )?;
        Ok(())
    }
}

/// `re.subn(pattern, repl, text, count=1)`. `suffix` switches to the callback
/// form used for the segment count: keep group 1 and append `suffix`.
fn replace_once(
    text: &str,
    pattern: &str,
    replacement: &str,
    suffix: Option<&str>,
) -> Result<(String, usize)> {
    let regex = Regex::new(pattern).map_err(|e| Error(e.to_string()))?;
    let Some(found) = regex.captures(text) else {
        return Ok((text.to_string(), 0));
    };
    let whole = found.get(0).expect("group 0");
    let new = match suffix {
        Some(suffix) => format!("{}{suffix}", &found[1]),
        None => replacement.to_string(),
    };
    Ok((
        format!("{}{new}{}", &text[..whole.start()], &text[whole.end()..]),
        1,
    ))
}

fn assert_project(text: &str) -> Result<()> {
    let ok = text.contains("boot: Gameplay")
        && !text.contains("name: \"Loading\",\n            root:")
        && !text.contains("world_message: Some");
    if ok {
        Ok(())
    } else {
        Err(Error(
            "AssertionError: fixture project is not a plain Gameplay boot".to_string(),
        ))
    }
}

/// `psoxide-perf engine-stress run SCENARIO...`: reproduce the October 6
/// continuation; never overwrite the October 3 fixtures. Sequential builds share
/// editor-playtest/generated. Each scenario retains its project and disc.
fn run_command(args: &[String]) -> Result<i32> {
    let stress = Stress::new();
    for name in args {
        if !SCENARIOS.iter().any(|(n, _)| n == name) {
            return Err(Error(name.clone()));
        }
        let manifest = stress.root.join(name).join("manifest.json");
        if manifest.exists() {
            let recorded = Json::parse(&fs::read_to_string(&manifest)?)?;
            let cue = recorded
                .get("cue")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            stress.replay(&stress.root.join(name), Path::new(&cue))?;
        } else {
            stress.scenario(name)?;
        }
    }
    Ok(0)
}

// ------------------------------------------------------------- calibrate-areas

/// Outdoor authoring sweep, same two actors/camera/assets; only pillar count
/// changes. Generated projects stay under build. Retains MCP edits, disc hashes
/// and raw timing.
struct Calibration {
    repo: PathBuf,
    root: PathBuf,
    frontend: PathBuf,
}

impl Calibration {
    fn create(&self, n: i64) -> Result<(PathBuf, PathBuf)> {
        let out = self.root.join(format!("pillars-{n}"));
        let p = out.join("project");
        if p.exists() {
            return Err(Error(format!(
                "Refusing to overwrite evidence at {}; set PSOXIDE_AREA_OUTPUT to a new directory",
                p.display()
            )));
        }
        fs::create_dir_all(&p)?;
        let valley = self.repo.join("editor/projects/graybox-valley");
        let mut text = fs::read_to_string(valley.join("project.ron"))?;
        text = text.replace(
            "../default/assets/",
            &format!(
                "{}/",
                self.repo.join("editor/projects/default/assets").display()
            ),
        );
        text = text.replace(
            "assets/textures/picotron-grid-64.psxt",
            &valley
                .join("assets/textures/picotron-grid-64.psxt")
                .display()
                .to_string(),
        );
        fs::write(p.join("project.ron"), text)?;

        let mut client = Client::for_project(&self.repo, &p)?;
        let mut recorder = Recorder {
            client: &mut client,
            calls: Vec::new(),
        };
        let authored = (|| -> Result<()> {
            let status = first_text(&recorder.call0("status")?)?;
            let size: i64 = Regex::new(r"(\d+) brushes")
                .expect("static pattern")
                .captures(&status)
                .ok_or("'NoneType' object is not subscriptable")?[1]
                .parse()
                .map_err(|_| Error("bad brush count".to_string()))?;
            for i in 0..size {
                recorder.brush(i)?;
            }
            recorder.call(
                "delete",
                Json::object([("first", Json::Int(0)), ("count", Json::Int(size))]),
            )?;
            for name in ["Courtyard daylight", "Valley daylight", "Tower daylight"] {
                recorder.call("delete_node", Json::object([("node", Json::from(name))]))?;
            }
            let boxes: [([i64; 3], [i64; 3]); 5] = [
                ([-8192, -256, -8192], [8192, 0, 8192]),
                ([-8448, -256, -8448], [-8192, 8192, 8448]),
                ([8192, -256, -8448], [8448, 8192, 8448]),
                ([-8192, -256, -8448], [8192, 8192, -8192]),
                ([-8192, -256, 8192], [8192, 8192, 8448]),
            ];
            for (lo, hi) in boxes {
                add_box(&mut recorder, lo, hi, "Graybox / Original 64")?;
            }
            add_box(
                &mut recorder,
                [-8448, 8192, -8448],
                [8448, 8448, 8448],
                "DP Fog City Cube Sky",
            )?;
            for i in 0..n {
                let x = [-6144, -3072, 3072, 6144][(i % 4) as usize];
                let z = -2048 + (i / 4) * 2048;
                add_box(
                    &mut recorder,
                    [x - 384, 0, z - 384],
                    [x + 384, 3072, z + 384],
                    "Graybox / Cliff 64",
                )?;
            }
            for i in 0..6 + n {
                recorder.brush(i)?;
            }
            set_face_uv(&mut recorder, 6 + n)?;
            for (name, position) in [
                ("Aletha (Player)", [0, 0, -5120]),
                ("Intake Custodian", [-1280, 0, 0]),
                ("Intake Custodian Copy", [1280, 0, 1024]),
            ] {
                recorder.call(
                    "move_node",
                    Json::object([("node", Json::from(name)), ("position", ints(&position))]),
                )?;
            }
            recorder.call(
                "add_light",
                Json::object([
                    ("position", ints(&[0, 7168, 0])),
                    ("radius", Json::Int(24576)),
                    ("intensity", Json::Float(1.0)),
                    ("name", Json::from("Calibration daylight")),
                ]),
            )?;
            recorder.call0("save")?;
            let audit = recorder.call("audit", Json::object([("depth", Json::from("full"))]))?;
            let blocks = audit
                .get("content")
                .and_then(Json::as_array)
                .ok_or_else(|| Error("KeyError: 'content'".to_string()))?;
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Json::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Json::as_str))
                .collect();
            fs::write(out.join("audit.txt"), text.join("\n"))?;
            Ok(())
        })();
        let calls = std::mem::take(&mut recorder.calls);
        let closed = client.close();
        fs::write(
            out.join("mcp.json"),
            Json::Array(calls).dumps(Some(2), false),
        )?;
        authored?;
        closed?;

        let features = "cd-stream-bench emulator-telemetry";
        let env = env_with(&[("EDITOR_PLAYTEST_FEATURES", features)]);
        println!("build {n}");
        let _ = std::io::stdout().flush();
        run_logged(
            &[
                path_arg(&self.frontend),
                "build-project-disc".into(),
                "--project".into(),
                path_arg(&p),
            ],
            &out.join("build.log"),
            &self.repo,
            &env,
        )?;
        let build_log = fs::read_to_string(out.join("build.log"))?;
        let cue_line = build_log
            .lines()
            .rfind(|l| l.ends_with(".cue"))
            .ok_or_else(|| Error("list index out of range".to_string()))?;
        let cue = PathBuf::from(cue_line);
        let cue_text = fs::read_to_string(&cue)?;
        let manifest = Json::object([
            ("project_sha256", Json::Str(sha(&p.join("project.ron"))?)),
            ("frontend_sha256", Json::Str(sha(&self.frontend)?)),
            ("cue", Json::Str(path_arg(&cue))),
            (
                "disc_files",
                disc_files(&cue_text, cue.parent().expect("cue has a parent"))?,
            ),
            ("features", Json::from(features)),
        ]);
        fs::write(out.join("manifest.json"), manifest.dumps(Some(2), false))?;
        Ok((out, cue))
    }

    fn replay(&self, out: &Path, cue: &Path) -> Result<()> {
        let env = env_with(&[("PSOXIDE_EXPERIMENTAL_DMA_FIFO", "1")]);
        let command: Vec<String> = vec![
            path_arg(&self.frontend),
            "launch".into(),
            "--path".into(),
            path_arg(cue),
            "--embedded-playtest".into(),
            "--stop-at-poll=1800".into(),
            "--steps=2400000000".into(),
            "--profile-log".into(),
            path_arg(&out.join("profile.csv")),
            "--route-log".into(),
            path_arg(&out.join("route.csv")),
            "--gpu-frame-stats-log".into(),
            path_arg(&out.join("gpu.csv")),
            "--route-screenshot-dir".into(),
            path_arg(&out.join("shots")),
            "--route-screenshot-interval=900".into(),
        ];
        let name = out
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("replay {name}");
        let _ = std::io::stdout().flush();
        run_logged(&command, &out.join("run.log"), &self.repo, &env)?;
        println!("done {name}");
        Ok(())
    }
}

/// `psoxide-perf engine-stress calibrate-areas [PILLARS...]` (default 0 4 8 12).
fn calibrate_command(args: &[String]) -> Result<i32> {
    let repo = repo_root();
    let root = resolve(&match std::env::var_os("PSOXIDE_AREA_OUTPUT") {
        Some(path) => PathBuf::from(path),
        None => repo.join("build/area-calibration"),
    });
    let calibration = Calibration {
        frontend: repo.join("target/release/frontend"),
        repo,
        root,
    };
    let counts: Vec<i64> = if args.is_empty() {
        vec![0, 4, 8, 12]
    } else {
        args.iter()
            .map(|a| {
                a.parse()
                    .map_err(|_| Error(format!("invalid literal for int(): '{a}'")))
            })
            .collect::<Result<_>>()?
    };
    // Authoring and baking stay sequential (they share the staged guest);
    // replays run two at a time while the next project is being built.
    let slots = std::sync::Mutex::new(0usize);
    let ready = std::sync::Condvar::new();
    let results = std::sync::Mutex::new(Vec::<Result<()>>::new());
    let outcome = std::thread::scope(|scope| -> Result<()> {
        for n in counts {
            let (out, cue) = calibration.create(n)?;
            let mut running = slots.lock().expect("slot lock");
            while *running >= 2 {
                running = ready.wait(running).expect("slot wait");
            }
            *running += 1;
            drop(running);
            let (calibration, results, slots, ready) = (&calibration, &results, &slots, &ready);
            scope.spawn(move || {
                let outcome = calibration.replay(&out, &cue);
                results.lock().expect("results lock").push(outcome);
                *slots.lock().expect("slot lock") -= 1;
                ready.notify_all();
            });
        }
        Ok(())
    });
    outcome?;
    for result in results.into_inner().expect("results lock") {
        result?;
    }
    Ok(0)
}

const USAGE: &str = "psoxide-perf engine-stress run SCENARIO... | analyse | measure-route RUN_DIR FIRST LAST | calibrate-areas [PILLARS...]";

/// `psoxide-perf engine-stress <run|analyse|measure-route|calibrate-areas> ...`.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let Some(action) = args.first() else {
        return Ok(usage_error(
            USAGE,
            "the following arguments are required: action",
        ));
    };
    let rest = &args[1..];
    match action.as_str() {
        "run" => run_command(rest),
        "analyse" => analyse_command(rest, out),
        "measure-route" => measure_command(rest, out),
        "calibrate-areas" => calibrate_command(rest),
        other => Ok(usage_error(USAGE, &format!("invalid choice: '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    //! Normal Play must remain measurable without enabling the guest profiler.

    use super::*;

    fn fixture(root: &Path) {
        let mut text = String::from("port1_polls,bus_cycles,display_start_changed\n");
        for frame in 0..64i128 {
            text.push_str(&format!("{},{},1\n", 100 + 2 * frame, frame * 2 * VBLANK));
        }
        fs::write(root.join("route.csv"), text).unwrap();
    }

    #[test]
    fn normal_guest_cadence_needs_no_profile_and_counters_stay_unknown() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let r = measure(dir.path(), 100, 226).unwrap();
        let fps = match r.get("display_fps") {
            Some(Json::Float(f)) => *f,
            other => panic!("{other:?}"),
        };
        assert!((fps - HZ as f64 / (2 * VBLANK) as f64).abs() < 0.0005);
        assert!(matches!(r.get("cadence_pass"), Some(Json::Bool(true))));
        assert!(matches!(
            r.get("telemetry_available"),
            Some(Json::Bool(false))
        ));
        assert!(matches!(r.get("primitive_overflows"), Some(Json::Null)));
        assert!(matches!(r.get("measured_pass"), Some(Json::Null)));
    }

    #[test]
    fn empty_profile_does_not_force_instrumentation() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        fs::write(dir.path().join("profile.csv"), "guest_frame,render\n").unwrap();
        let r = measure(dir.path(), 100, 226).unwrap();
        assert!(matches!(
            r.get("telemetry_available"),
            Some(Json::Bool(false))
        ));
    }

    #[test]
    fn incomplete_poll_window_cannot_pass() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        assert!(measure(dir.path(), 100, 400).is_err());
    }

    #[test]
    fn project_patches_replace_once() {
        let (text, count) = replace_once(
            "a bsp_patch_extent: 2048 b bsp_patch_extent: 2048",
            "bsp_patch_extent: 2048",
            "bsp_patch_extent: 4096",
            None,
        )
        .unwrap();
        assert_eq!(
            (text.as_str(), count),
            ("a bsp_patch_extent: 4096 b bsp_patch_extent: 2048", 1)
        );
        let (text, _) = replace_once(
            "far_vista: (enabled: true, segments: 4, x)",
            r"(far_vista: .*?segments: )\d+",
            "",
            Some("16"),
        )
        .unwrap();
        assert_eq!(text, "far_vista: (enabled: true, segments: 16, x)");
    }
}
