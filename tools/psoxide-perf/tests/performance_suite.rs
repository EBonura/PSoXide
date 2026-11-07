//! Fail-closed replay/cache tests using a deterministic subprocess, no guest assets.

use std::path::{Path, PathBuf};

use psoxide_perf::performance_suite::{
    compare, parse_summary, plan, run_case, run_case_with, validate_result, write_json, SuiteError,
};
use psoxide_perf::pyjson::Json;

const DRIVER: &str = "fixture driver\n";

struct Suite {
    _temp: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    bindings: Json,
    case: Json,
}

fn s(text: &str) -> Json {
    Json::from(text)
}

fn strings(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|i| s(i)).collect())
}

fn tape(count: u32, extra: usize) -> Vec<u8> {
    let mut bytes = b"PXITAPE2".to_vec();
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend(std::iter::repeat_n(0u8, extra));
    bytes
}

fn suite() -> Suite {
    let temp = tempfile::tempdir().unwrap();
    let root = fs_canonical(temp.path());
    let store = root.join("store");
    std::fs::write(root.join("driver"), DRIVER).unwrap();
    std::fs::write(root.join("disc.bin"), b"disc").unwrap();
    std::fs::write(
        root.join("disc.cue"),
        "FILE \"disc.bin\" BINARY\n TRACK 01 MODE2/2352\n INDEX 01 00:00:00\n",
    )
    .unwrap();
    std::fs::write(root.join("tape"), tape(10, 60)).unwrap();
    let path = |name: &str| Json::Str(root.join(name).to_string_lossy().into_owned());
    let bindings = Json::object([
        ("driver", path("driver")),
        ("image", path("disc.cue")),
        ("tape", path("tape")),
        (
            "emulator",
            Json::from(env!("CARGO_BIN_EXE_perf-fixture-driver")),
        ),
    ]);
    let inputs = Json::object(
        ["driver", "image", "tape", "emulator"].map(|name| (name, Json::Str(format!("${name}")))),
    );
    let case = Json::object([
        ("id", s("fixture")),
        ("lane", s("acceptance")),
        (
            "build_provenance",
            Json::object([("directory", s("fixture")), ("source", s("test"))]),
        ),
        ("inputs", inputs),
        (
            "argv",
            strings(&[
                "{input.driver}",
                "--config-dir",
                "{out}/config",
                "--path",
                "{input.image}",
                "--input-tape",
                "{input.tape}",
                "--embedded-playtest",
                "--steps",
                "100",
                "--stop-at-poll",
                "5",
            ]),
        ),
        (
            "environment",
            Json::object([(
                "PSOXIDE_TEST_COUNT",
                Json::Str(root.join("count").to_string_lossy().into_owned()),
            )]),
        ),
        ("completion", Json::object([("poll", Json::Int(5))])),
        (
            "required_outputs",
            strings(&["frames/a.ppm", "frames/b.ppm", "audio.wav"]),
        ),
        (
            "quality",
            Json::object([
                (
                    "visual",
                    Json::object([
                        ("patterns", strings(&["frames/*.ppm"])),
                        ("scope", s("two fixture checkpoints")),
                    ]),
                ),
                (
                    "audio",
                    Json::object([
                        ("patterns", strings(&["audio.wav"])),
                        ("scope", s("fixture PCM")),
                    ]),
                ),
                (
                    "gameplay",
                    Json::object([
                        ("patterns", strings(&["state.json"])),
                        ("scope", s("fixture player state")),
                    ]),
                ),
            ]),
        ),
    ]);
    Suite {
        _temp: temp,
        root,
        store,
        bindings,
        case,
    }
}

fn fs_canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

impl Suite {
    fn run(&self, case: &Json) -> Result<Json, SuiteError> {
        run_case(case, &self.bindings, &self.store)
    }

    fn key(&self, case: &Json) -> String {
        plan(case, &self.bindings).unwrap().key
    }

    fn count(&self) -> String {
        std::fs::read_to_string(self.root.join("count")).unwrap()
    }

    fn set_env(case: &mut Json, name: &str, value: &str) {
        case.get_mut("environment").unwrap().set(name, s(value));
    }
}

fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap()
}

fn result_dir(result: &Json) -> PathBuf {
    PathBuf::from(text(result, "result"))
}

fn expect_value_error<T: std::fmt::Debug>(outcome: Result<T, SuiteError>, needle: &str) {
    match outcome {
        Err(SuiteError::Value(message)) => {
            assert!(
                message.contains(needle),
                "{message:?} does not contain {needle:?}"
            )
        }
        other => panic!("expected a ValueError containing {needle:?}, got {other:?}"),
    }
}

#[test]
fn hit_rehash_and_no_repeat_execution() {
    let suite = suite();
    std::env::set_var("PSOXIDE_UNDECLARED_TEST", "must not leak");
    let first = suite.run(&suite.case).unwrap();
    let second = suite.run(&suite.case).unwrap();
    std::env::remove_var("PSOXIDE_UNDECLARED_TEST");
    assert_eq!(
        (text(&first, "cache"), text(&second, "cache")),
        ("miss", "hit")
    );
    assert_eq!(suite.count(), "run\n");
    let verdict = compare(&result_dir(&first), &result_dir(&second)).unwrap();
    assert_eq!(text(&verdict, "acceptance"), "pass");
}

#[test]
fn transitive_disc_tamper_and_options_invalidate() {
    let suite = suite();
    let a = suite.key(&suite.case);
    std::fs::write(suite.root.join("disc.bin"), b"DISC").unwrap();
    let b = suite.key(&suite.case);
    assert_ne!(a, b);
    let mut other = suite.case.clone();
    Suite::set_env(&mut other, "PSOXIDE_EXPERIMENTAL_DMA_FIFO", "1");
    assert_ne!(b, suite.key(&other));
    let mut other = suite.case.clone();
    other
        .get_mut("build_provenance")
        .unwrap()
        .set("directory", s("different-path"));
    assert_ne!(b, suite.key(&other));
}

#[test]
fn same_size_cached_corruption_rejected() {
    let suite = suite();
    let result = suite.run(&suite.case).unwrap();
    std::fs::write(
        result_dir(&result).join("artifacts/frames/a.ppm"),
        b"FRAME1",
    )
    .unwrap();
    expect_value_error(suite.run(&suite.case), "artifact");
}

#[test]
fn partial_never_hit() {
    let suite = suite();
    let key = suite.key(&suite.case);
    std::fs::create_dir_all(suite.store.join("results").join(key)).unwrap();
    assert!(matches!(suite.run(&suite.case), Err(SuiteError::Io(_))));
}

#[test]
fn failure_and_cap_never_complete() {
    let suite = suite();
    for mode in ["fail", "cap"] {
        let mut case = suite.case.clone();
        Suite::set_env(&mut case, "PSOXIDE_TEST_MODE", mode);
        assert!(
            matches!(suite.run(&case), Err(SuiteError::Value(_))),
            "{mode}"
        );
        let key = suite.key(&case);
        assert!(!suite.store.join("results").join(key).exists());
    }
    assert_eq!(
        std::fs::read_dir(suite.store.join("failures"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn duplicate_concurrent_work_is_single_execution() {
    let suite = suite();
    let results: Vec<Json> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| suite.run(&suite.case).unwrap()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut caches: Vec<&str> = results.iter().map(|r| text(r, "cache")).collect();
    caches.sort();
    assert_eq!(caches, ["hit", "miss"]);
    assert_eq!(suite.count(), "run\n");
}

#[test]
fn comparison_rejects_different_runtime_options() {
    let suite = suite();
    let a = suite.run(&suite.case).unwrap();
    let mut other = suite.case.clone();
    Suite::set_env(&mut other, "PSOXIDE_EXPERIMENTAL_DMA_FIFO", "1");
    let b = suite.run(&other).unwrap();
    expect_value_error(compare(&result_dir(&a), &result_dir(&b)), "environment");
}

#[test]
fn quick_lane_cannot_pass_quality() {
    let suite = suite();
    let mut case = suite.case.clone();
    case.set("lane", s("quick"));
    let r = suite.run(&case).unwrap();
    let verdict = compare(&result_dir(&r), &result_dir(&r)).unwrap();
    assert_eq!(text(&verdict, "acceptance"), "not established");
    for (_, gate) in verdict.get("quality").and_then(Json::as_object).unwrap() {
        assert_eq!(text(gate, "status"), "unavailable");
    }
}

#[test]
fn missing_multiframe_evidence_not_pass() {
    let suite = suite();
    let mut case = suite.case.clone();
    case.get_mut("quality")
        .and_then(|q| q.get_mut("visual"))
        .unwrap()
        .set("minimum_count", Json::Int(3));
    let r = suite.run(&case).unwrap();
    let verdict = compare(&result_dir(&r), &result_dir(&r)).unwrap();
    assert_eq!(
        text(
            verdict.get("quality").unwrap().get("visual").unwrap(),
            "status"
        ),
        "unavailable"
    );
}

#[test]
fn receipt_summary_tamper_rejected() {
    let suite = suite();
    let r = suite.run(&suite.case).unwrap();
    let path = result_dir(&r).join("receipt.json");
    let mut receipt = Json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    receipt
        .get_mut("summary")
        .unwrap()
        .set("bus_cycles", Json::Int(1));
    write_json(&path, &receipt).unwrap();
    expect_value_error(validate_result(&result_dir(&r), None), "sealed artifact");
}

#[test]
fn literal_input_and_unknown_flag_rejected() {
    let suite = suite();
    let mut case = suite.case.clone();
    let argv = case.get_mut("argv").and_then(Json::as_array_mut).unwrap();
    let at = argv
        .iter()
        .position(|a| a.as_str() == Some("--path"))
        .unwrap()
        + 1;
    argv[at] = s("/unhashed/image.cue");
    expect_value_error(plan(&case, &suite.bindings), "hashed input");
    let mut case = suite.case.clone();
    case.get_mut("argv")
        .and_then(Json::as_array_mut)
        .unwrap()
        .push(s("--mystery"));
    expect_value_error(plan(&case, &suite.bindings), "unsupported option");
}

#[test]
fn comparison_rejects_changed_adapter_input() {
    let suite = suite();
    let mut case = suite.case.clone();
    case.get_mut("inputs").unwrap().set("adapter", s("$driver"));
    let a = suite.run(&case).unwrap();
    std::fs::write(
        suite.root.join("driver"),
        format!("{DRIVER}# changed semantic parser\n"),
    )
    .unwrap();
    let b = suite.run(&case).unwrap();
    expect_value_error(
        compare(&result_dir(&a), &result_dir(&b)),
        "non-candidate input",
    );
}

#[test]
fn poll_completion_rejects_tape_exhaustion() {
    let suite = suite();
    std::fs::write(suite.root.join("tape"), tape(5, 30)).unwrap();
    expect_value_error(suite.run(&suite.case), "strictly beyond");
}

#[test]
fn fault_with_successful_summary_rejected() {
    let suite = suite();
    std::fs::write(
        suite.root.join("driver"),
        format!("{DRIVER}print:[cli] step 10 failed: fixture fault\n"),
    )
    .unwrap();
    expect_value_error(suite.run(&suite.case), "guest fault");
}

#[test]
fn empty_quality_cannot_pass() {
    let suite = suite();
    let mut case = suite.case.clone();
    case.get_mut("quality").unwrap().set(
        "visual",
        Json::object([
            ("patterns", strings(&["missing*"])),
            ("minimum_count", Json::Int(0)),
            ("scope", s("none")),
        ]),
    );
    let r = suite.run(&case).unwrap();
    expect_value_error(compare(&result_dir(&r), &result_dir(&r)), "evidence floor");
}

#[test]
fn lock_wait_input_change_rejected() {
    let suite = suite();
    suite.run(&suite.case).unwrap();
    let outcome = run_case_with(&suite.case, &suite.bindings, &suite.store, &|| {
        std::fs::write(suite.root.join("disc.bin"), b"DISC").unwrap();
    });
    expect_value_error(outcome, "waiting for cache lock");
}

#[test]
fn cumulative_cycles_not_summed() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path();
    std::fs::write(
        out.join("stdout.txt"),
        "tick=10 cycles=40 stopped-at=10\nroute-ticks=2 port1-polls=5\nvram_fnv1a_64=1\ndisplay_fnv1a_64=2\n",
    )
    .unwrap();
    std::fs::write(
        out.join("cycles.csv"),
        "bus_cycles,issue_cycles\n20,5\n40,6\n",
    )
    .unwrap();
    let result = parse_summary(out).unwrap();
    assert_eq!(result.get("bus_cycles").and_then(Json::as_i64), Some(40));
    assert!(result
        .get("guest_cycle_columns")
        .unwrap()
        .py_eq(&Json::object([("issue_cycles", Json::Int(11))])));
}

#[test]
fn opaque_wrapper_and_config_rejected() {
    let mut suite = suite();
    let driver = suite.bindings.get("driver").cloned().unwrap();
    let real = suite.bindings.get("emulator").cloned().unwrap();
    suite.bindings.set("emulator", driver);
    std::fs::write(suite.root.join("driver"), "#!/usr/bin/env python3\n").unwrap();
    expect_value_error(plan(&suite.case, &suite.bindings), "wrapper");
    suite.bindings.set("emulator", real);
    let argv = suite
        .case
        .get_mut("argv")
        .and_then(Json::as_array_mut)
        .unwrap();
    let at = argv
        .iter()
        .position(|a| a.as_str() == Some("--config-dir"))
        .unwrap()
        + 1;
    argv[at] = s("/existing/config");
    expect_value_error(plan(&suite.case, &suite.bindings), "isolated");
}
