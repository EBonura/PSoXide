//! `adopt-pgo` end to end: a git repository, a build command and a stand-in for
//! `psoxide-pgo measure` that scores the image the build left. No compiler, no emulator.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use psoxide_perf::adopt_pgo;

/// Prints `w.work_per_frame=` for the image after `--image`: 1100 for a slow profile, 1000 for
/// a fast one, plus the first poll of the window so the offsets differ.
const FAKE_MEASURE: &str = r#"#!/bin/sh
image=""; polls=""
while [ $# -gt 0 ]; do
  case "$1" in
    --image) image=$2; shift ;;
    --polls) polls=$2; shift ;;
  esac
  shift
done
from=${polls%%..*}
case "$(cat "$image")" in
  slow*) base=1100 ;;
  *) base=1000 ;;
esac
echo "w.flips=100"
echo "w.work_per_frame=$((base + from - 500))"
"#;

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    pgo: PathBuf,
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository with `committed` as the profile at HEAD and `working` in the tree.
fn repo(committed: &str, working: &str) -> Repo {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("game");
    std::fs::create_dir_all(root.join("pgo")).unwrap();
    std::fs::write(root.join("pgo/game.prof"), committed).unwrap();
    std::fs::write(root.join("pgo/hot-callsite-threshold"), "800\n").unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "ticket"]);
    std::fs::write(root.join("pgo/game.prof"), working).unwrap();
    std::fs::write(root.join("pgo/hot-callsite-threshold"), "500\n").unwrap();
    let pgo = dir.path().join("fake-measure.sh");
    std::fs::write(&pgo, FAKE_MEASURE).unwrap();
    std::fs::set_permissions(&pgo, std::fs::Permissions::from_mode(0o755)).unwrap();
    Repo {
        _dir: dir,
        root,
        pgo,
    }
}

fn adopt(repo: &Repo, record: &Path) -> String {
    let work = repo.root.parent().unwrap().join("work");
    let mut args: Vec<String> = [
        "--repo",
        repo.root.to_str().unwrap(),
        "--file",
        "pgo/game.prof",
        "--file",
        "pgo/hot-callsite-threshold",
        "--build",
        "cp pgo/game.prof image.cue",
        "--image",
        "image.cue",
        "--frontend",
        repo.pgo.to_str().unwrap(),
        "--pgo",
        repo.pgo.to_str().unwrap(),
        "--work",
        work.to_str().unwrap(),
        "--record",
        record.to_str().unwrap(),
        "--launch-arg",
        "--embedded-playtest",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for tape in ["de_dust2", "de_airstrip"] {
        args.extend(["--tape".into(), format!("tapes/{tape}.pxtape")]);
        args.extend(["--polls".into(), "500..1800".into()]);
    }
    let mut out = Vec::new();
    assert_eq!(adopt_pgo::run(&args, &mut out).unwrap(), 0);
    String::from_utf8(out).unwrap()
}

#[test]
fn a_slow_collection_is_kept_out_and_the_record_is_stable() {
    let repo = repo("fast profile\n", "slow profile\n");
    let record = repo.root.join("pgo/adoption.txt");
    let printed = adopt(&repo, &record);
    assert!(printed.contains("adopt-pgo: keep-incumbent"), "{printed}");
    let first = std::fs::read_to_string(&record).unwrap();
    assert!(first.contains("decision keep-incumbent\n"), "{first}");
    assert!(
        first.contains("candidate_vs_incumbent de_airstrip +10.12%"),
        "{first}"
    );
    // Offsets -48 -24 0 24 around poll 500: the stand-in adds from - 500.
    assert!(
        first.contains("score incumbent de_dust2 -48 952\n"),
        "{first}"
    );
    assert!(
        first.contains("mean candidate de_dust2 1088.0\n"),
        "{first}"
    );
    // The incumbent is back in the tree, every file of it, and was the last build.
    let tree = |path: &str| std::fs::read_to_string(repo.root.join(path)).unwrap();
    assert_eq!(tree("pgo/game.prof"), "fast profile\n");
    assert_eq!(tree("pgo/hot-callsite-threshold"), "800\n");
    assert_eq!(tree("image.cue"), "fast profile\n");
    // The same comparison again writes the same bytes.
    std::fs::write(repo.root.join("pgo/game.prof"), "slow profile\n").unwrap();
    std::fs::write(repo.root.join("pgo/hot-callsite-threshold"), "500\n").unwrap();
    adopt(&repo, &record);
    assert_eq!(std::fs::read_to_string(&record).unwrap(), first);
}

#[test]
fn a_fast_collection_is_adopted() {
    let repo = repo("slow profile\n", "fast profile\n");
    let record = repo.root.join("pgo/adoption.txt");
    let printed = adopt(&repo, &record);
    assert!(printed.contains("adopt-pgo: adopt"), "{printed}");
    let text = std::fs::read_to_string(&record).unwrap();
    assert!(
        text.contains("candidate_vs_incumbent geomean -9.19%"),
        "{text}"
    );
    let tree = |path: &str| std::fs::read_to_string(repo.root.join(path)).unwrap();
    assert_eq!(tree("pgo/game.prof"), "fast profile\n");
    assert_eq!(tree("pgo/hot-callsite-threshold"), "500\n");
}

#[test]
fn an_unchanged_ticket_builds_nothing() {
    let repo = repo("same\n", "same\n");
    std::fs::write(repo.root.join("pgo/hot-callsite-threshold"), "800\n").unwrap();
    let record = repo.root.join("pgo/adoption.txt");
    let printed = adopt(&repo, &record);
    assert!(printed.contains("adopt-pgo: unchanged"), "{printed}");
    assert!(!repo.root.join("image.cue").exists());
}
