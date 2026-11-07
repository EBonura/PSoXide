//! Read-only Quake v9 chain completion and canonical presentation-window adapter.
//!
//! Contract mirrors quake-psx host/quake-build/main.rs at 24908826:
//! read_probe_version, validate_e1m1_chain_probe, full_level_render_metrics.
//! No asset paths, guest writes, or benchmark-building side effects.

use std::path::Path;

use crate::pyjson::Json;
use crate::util::{csv_records, dict_rows, parse_int, Error, Result};

/// Names of the 34 little-endian `u32` probe fields, in memory order.
pub const FIELDS: [&str; 34] = [
    "magic",
    "version",
    "complete",
    "phase",
    "failure_code",
    "failure_map",
    "failure_entity",
    "failure_detail",
    "total_frames",
    "maps_loaded",
    "maps_validated",
    "transitions",
    "weapon_selected",
    "weapon_fired",
    "weapon_animated",
    "monster_present",
    "monster_animated",
    "monster_state_bounds",
    "monster_attack",
    "monster_pain",
    "monster_death",
    "boss",
    "current_map",
    "route_index",
    "last_health",
    "state_ranges",
    "valid_state_ranges",
    "map_loads",
    "stage_frames",
    "shock_count",
    "intermission_state",
    "player_state",
    "weapon_pickups",
    "target_edges",
];

/// The values a complete probe must hold.
pub const EXPECTED: [(&str, u32); 11] = [
    ("version", 9),
    ("failure_code", 0),
    ("complete", 1),
    ("phase", 0x51),
    ("maps_loaded", 6),
    ("maps_validated", 6),
    ("current_map", 2),
    ("route_index", 60),
    ("map_loads", 2),
    ("transitions", 1),
    ("player_state", 0x7fff),
];

const PROBE_BYTES: usize = FIELDS.len() * 4;

fn field(probe: &[u32; 34], name: &str) -> u32 {
    probe[FIELDS
        .iter()
        .position(|f| *f == name)
        .expect("known probe field")]
}

/// Python's `OSError` text: `[Errno 2] No such file or directory: 'path'`.
fn os_error(path: &Path, error: &std::io::Error) -> Error {
    let text = error.to_string();
    let reason = text.split(" (os error").next().unwrap_or(&text);
    Error(format!(
        "[Errno {}] {reason}: '{}'",
        error.raw_os_error().unwrap_or(0),
        path.display()
    ))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| os_error(path, &e))
}

fn read_string(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| os_error(path, &e))
}

fn value_error<T>(message: &str) -> Result<T> {
    Err(Error(message.to_string()))
}

/// Inspect a run directory (`ram.bin`, `route.csv`, `cd.csv`). `Err` is the
/// Python `ValueError`/`OSError` family: the caller reports the run as incomplete.
pub fn inspect(out: &Path) -> Result<Json> {
    let ram = read_bytes(&out.join("ram.bin"))?;
    let mut magic = Vec::new();
    magic.extend_from_slice(&0x5150_5358u32.to_le_bytes());
    magic.extend_from_slice(&9u32.to_le_bytes());
    let hits: Vec<usize> = if ram.len() >= PROBE_BYTES {
        (0..=ram.len() - PROBE_BYTES)
            .step_by(4)
            .filter(|&i| ram[i..i + 8] == magic[..])
            .collect()
    } else {
        Vec::new()
    };
    if hits.len() != 1 {
        return Err(Error(format!(
            "expected exactly one aligned complete v9 probe; found {}",
            hits.len()
        )));
    }
    let mut probe = [0u32; 34];
    for (i, slot) in probe.iter_mut().enumerate() {
        let at = hits[0] + i * 4;
        *slot = u32::from_le_bytes([ram[at], ram[at + 1], ram[at + 2], ram[at + 3]]);
    }
    let mut failures = Json::Object(Vec::new());
    for (name, expected) in EXPECTED {
        let actual = field(&probe, name);
        if actual != expected {
            failures.set(
                name,
                Json::object([
                    ("actual", Json::from(i64::from(actual))),
                    ("expected", Json::from(i64::from(expected))),
                ]),
            );
        }
    }
    if field(&probe, "weapon_selected") & 7 != 7 {
        failures.set(
            "mover_sound_mask",
            Json::from(i64::from(field(&probe, "weapon_selected"))),
        );
    }
    if field(&probe, "target_edges") < 4 {
        failures.set(
            "target_edges_minimum4",
            Json::from(i64::from(field(&probe, "target_edges"))),
        );
    }
    let (_, route) = dict_rows(&read_string(&out.join("route.csv"))?)?;
    let cd: Vec<Vec<String>> = csv_records(&read_string(&out.join("cd.csv"))?)
        .into_iter()
        .skip(1)
        .collect();
    let reads: Vec<i64> = cd
        .iter()
        .filter(|r| r.len() >= 2 && r[1] == "0x06")
        .map(|r| parse_int(&r[0]))
        .collect::<Result<_>>()?;
    if reads.len() < 2 {
        return value_error("fewer than two ReadN sessions");
    }
    // Rust `max_by_key` chooses the last pair on ties.
    let (start, end) = reads
        .windows(2)
        .enumerate()
        .max_by_key(|(i, w)| ((w[1] - w[0]).max(0), *i))
        .map(|(_, w)| (w[0], w[1]))
        .expect("at least one pair");
    let column = |row: &crate::util::DictRow, name: &str| -> Result<i64> {
        parse_int(
            row.get(name)
                .ok_or_else(|| Error(format!("KeyError: '{name}'")))?,
        )
    };
    let mut flips = Vec::new();
    for row in &route {
        if column(row, "display_start_changed")? == 1 {
            let cycles = column(row, "bus_cycles")?;
            if start < cycles && cycles < end {
                flips.push(cycles);
            }
        }
    }
    if flips.len() < 2 || flips[flips.len() - 1] <= flips[0] {
        return value_error("invalid gameplay presentation interval");
    }
    let last = route
        .last()
        .ok_or_else(|| Error("list index out of range".to_string()))?;
    let polls = column(last, "port1_polls")?;
    if polls <= 0 {
        failures.set("controller_polls", Json::Int(polls));
    }
    let complete = failures.as_object().is_none_or(<[_]>::is_empty);
    let elapsed = flips[flips.len() - 1] - flips[0];
    let intervals = flips.len() as i64 - 1;
    let probe_json = Json::Object(
        FIELDS
            .iter()
            .zip(probe)
            .map(|(name, value)| (name.to_string(), Json::from(i64::from(value))))
            .collect(),
    );
    Ok(Json::object([
        ("complete", Json::Bool(complete)),
        (
            "evidence",
            Json::object([
                ("probe", probe_json),
                ("ram_offset", Json::from(hits[0])),
                ("failures", failures),
                ("final_polls", Json::Int(polls)),
                (
                    "readn_window_cycles",
                    Json::Array(vec![Json::Int(start), Json::Int(end)]),
                ),
            ]),
        ),
        (
            "gameplay",
            Json::object([
                (
                    "status",
                    Json::from(if complete { "observed" } else { "unavailable" }),
                ),
                (
                    "basis",
                    Json::from("guest v9 completion probe, not pixel validation"),
                ),
                (
                    "scope",
                    Json::from("fixed-step diagnostic E1M1 route and natural E1M2 transition"),
                ),
            ]),
        ),
        (
            "metrics",
            Json::object([
                ("presentations", Json::from(flips.len())),
                ("presentation_intervals", Json::Int(intervals)),
                ("elapsed_bus_cycles", Json::Int(elapsed)),
                (
                    "fps_x1000",
                    Json::Int(
                        (i128::from(intervals) * 33_868_800 * 1000 / i128::from(elapsed)) as i64,
                    ),
                ),
                (
                    "fps",
                    Json::Float(intervals as f64 * 33_868_800.0 / elapsed as f64),
                ),
                ("host_seconds_is_performance", Json::Bool(false)),
            ]),
        ),
    ]))
}

/// The adapter's command line: `quake-chain-adapter RUN_DIR RESOLVED_INPUTS_JSON`.
/// Prints one line of sorted-key JSON and returns the process exit status.
pub fn main_with(args: &[String], out: &mut dyn std::io::Write) -> i32 {
    let outcome = (|| -> Result<Json> {
        if args.len() != 2 {
            return value_error("usage: quake-chain-adapter RUN_DIR RESOLVED_INPUTS_JSON");
        }
        Json::parse(&read_string(Path::new(&args[1]))?)?; // Runner binds and hashes inputs.
        inspect(Path::new(&args[0]))
    })();
    let result = outcome.unwrap_or_else(|error| {
        Json::object([
            ("complete", Json::Bool(false)),
            (
                "evidence",
                Json::object([("error", Json::Str(error.to_string()))]),
            ),
            (
                "gameplay",
                Json::object([("status", Json::from("unavailable"))]),
            ),
            ("metrics", Json::Object(Vec::new())),
        ])
    });
    let _ = writeln!(out, "{}", result.dumps(None, true));
    if matches!(result.get("complete"), Some(Json::Bool(true))) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        data: Vec<u8>,
    }

    fn pack(values: &[(&str, u32)]) -> Vec<u8> {
        let mut probe = [0u32; 34];
        for (name, value) in values {
            probe[FIELDS.iter().position(|f| f == name).unwrap()] = *value;
        }
        probe.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let mut values: Vec<(&str, u32)> = EXPECTED.to_vec();
        values.extend([
            ("magic", 0x5150_5358),
            ("weapon_selected", 7),
            ("target_edges", 4),
        ]);
        let data = pack(&values);
        std::fs::write(dir.path().join("ram.bin"), &data).unwrap();
        std::fs::write(
            dir.path().join("route.csv"),
            "bus_cycles,display_start_changed,port1_polls\n100,1,1\n200,1,2\n300,1,3\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cd.csv"),
            "cycle,command\n50,0x06\n350,0x06\n",
        )
        .unwrap();
        Fixture { dir, data }
    }

    impl Fixture {
        fn write(&self, data: &[u8]) {
            std::fs::write(self.dir.path().join("ram.bin"), data).unwrap();
        }
        fn complete(&self) -> bool {
            matches!(
                inspect(self.dir.path()).unwrap().get("complete"),
                Some(Json::Bool(true))
            )
        }
    }

    #[test]
    fn full_contract_and_interval_denominator() {
        let f = fixture();
        let result = inspect(f.dir.path()).unwrap();
        assert!(matches!(result.get("complete"), Some(Json::Bool(true))));
        let metrics = result.get("metrics").unwrap();
        assert_eq!(metrics.get("presentations").and_then(Json::as_i64), Some(3));
        assert_eq!(
            metrics.get("presentation_intervals").and_then(Json::as_i64),
            Some(2)
        );
        assert_eq!(
            metrics.get("elapsed_bus_cycles").and_then(Json::as_i64),
            Some(200)
        );
    }

    #[test]
    fn each_required_field_rejects() {
        let f = fixture();
        for (key, expected) in EXPECTED {
            let mut data = f.data.clone();
            let at = FIELDS.iter().position(|n| *n == key).unwrap() * 4;
            data[at..at + 4].copy_from_slice(&(expected + 1).to_le_bytes());
            f.write(&data);
            if key == "version" {
                assert!(inspect(f.dir.path()).is_err(), "{key}");
            } else {
                assert!(!f.complete(), "{key}");
            }
        }
    }

    #[test]
    fn sound_and_edge_masks() {
        let f = fixture();
        for (key, value) in [("weapon_selected", 3u32), ("target_edges", 3)] {
            let mut data = f.data.clone();
            let at = FIELDS.iter().position(|n| *n == key).unwrap() * 4;
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
            f.write(&data);
            assert!(!f.complete(), "{key}");
        }
    }

    #[test]
    fn duplicate_probe_rejects() {
        let f = fixture();
        f.write(&[f.data.clone(), f.data.clone()].concat());
        assert!(inspect(f.dir.path()).is_err());
    }

    #[test]
    fn truncated_probe_rejects() {
        let f = fixture();
        f.write(&f.data[..f.data.len() - 1]);
        assert!(inspect(f.dir.path()).is_err());
    }

    #[test]
    fn missing_gameplay_window_rejects() {
        let f = fixture();
        std::fs::write(f.dir.path().join("cd.csv"), "cycle,command\n50,0x06\n").unwrap();
        assert!(inspect(f.dir.path()).is_err());
    }
}
