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
