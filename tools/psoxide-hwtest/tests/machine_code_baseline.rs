//! The newest pinned machine-code baseline must keep parsing.

use std::path::Path;

use psoxide_hwtest::machine_code::parse_baseline;

#[test]
fn the_pinned_baseline_parses() {
    let refs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/hardware-refs");
    let version = |name: &str| -> Vec<u32> {
        name.split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().unwrap())
            .collect()
    };
    let newest = std::fs::read_dir(&refs)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("hwtest-machine-code-v") && name.ends_with(".txt"))
        .max_by_key(|name| version(name.trim_end_matches(".txt")))
        .expect("a baseline");
    let rows = parse_baseline(&std::fs::read_to_string(refs.join(newest)).unwrap()).unwrap();
    let probe = rows.iter().find(|(key, _)| key == "07").expect("probe 07");
    assert_eq!(probe.1 .0, "timed_multu_mflo");
}

/// A suite version bump pins its machine-code baseline in the same change: `make hwtest-verify-code`
/// refuses to run without it, and nothing else in `make test` noticed that three bumps had not.
#[test]
fn the_suite_version_has_its_machine_code_baseline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let main = std::fs::read_to_string(root.join("engine/examples/hardware-tests/src/main.rs")).unwrap();
    let version = main
        .lines()
        .find_map(|line| line.strip_prefix("const SUITE_VERSION: &str = \"HWTEST v")?.strip_suffix("\";"))
        .expect("SUITE_VERSION in hardware-tests main.rs");
    let baseline = root.join(format!("docs/hardware-refs/hwtest-machine-code-v{version}.txt"));
    assert!(
        baseline.is_file(),
        "hardware-tests is v{version} and {} does not exist: pin it with `make hwtest-verify-code`'s instructions",
        baseline.display()
    );
}
