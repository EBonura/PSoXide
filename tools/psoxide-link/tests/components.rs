//! Locked component exports and local edit protection, against real Git
//! repositories in a scratch directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use psoxide_link::components::{materialize, Outcome};

struct Fixture {
    scratch: PathBuf,
    source: PathBuf,
    root: PathBuf,
    revision: String,
}

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

impl Fixture {
    fn new(name: &str) -> Self {
        let scratch = std::env::temp_dir().join(format!(
            "psoxide-components-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&scratch);
        let source = scratch.join("source");
        let root = scratch.join("consumer");
        for path in [&source, &root] {
            fs::create_dir_all(path).unwrap();
            git(path, &["init", "-q"]);
        }
        fs::create_dir(source.join("sdk")).unwrap();
        fs::write(source.join("sdk/input.rs"), "committed source").unwrap();
        git(&source, &["add", "."]);
        git(
            &source,
            &[
                "-c",
                "user.name=Component test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "Fixture",
            ],
        );
        let revision = git(&source, &["rev-parse", "HEAD"]).trim().to_string();
        let fixture = Self {
            scratch,
            source,
            root,
            revision,
        };
        fixture.write_lock("example/sdk", &["sdk"]);
        fixture
    }

    fn write_lock(&self, repository: &str, paths: &[&str]) {
        let lock = serde_json::json!({"schema": 1, "components": {"sdk": {
            "repository": repository, "revision": self.revision, "paths": paths}}});
        fs::write(self.root.join("components.lock.json"), lock.to_string()).unwrap();
    }

    fn bootstrap(&self, check: bool) -> psoxide_link::Result<Outcome> {
        let sources = BTreeMap::from([("sdk".to_string(), self.source.clone())]);
        materialize(&self.root, &sources, check, None)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

#[test]
fn exports_locked_content_and_checks_offline() {
    let fixture = Fixture::new("export");
    fs::write(fixture.source.join("sdk/input.rs"), "unstaged source").unwrap();
    assert_eq!(fixture.bootstrap(false).unwrap(), Outcome::Bootstrapped(1));
    assert_eq!(
        fs::read_to_string(fixture.root.join("sdk/input.rs")).unwrap(),
        "committed source"
    );
    assert_eq!(fixture.bootstrap(true).unwrap(), Outcome::Current);
}

#[test]
fn refuses_modified_imports() {
    let fixture = Fixture::new("modified");
    fixture.bootstrap(false).unwrap();
    fs::write(fixture.root.join("sdk/input.rs"), "local edits").unwrap();
    let error = fixture.bootstrap(false).unwrap_err().to_string();
    assert!(error.contains("Imported file changed"), "{error}");
    assert_eq!(
        fs::read_to_string(fixture.root.join("sdk/input.rs")).unwrap(),
        "local edits"
    );
}

#[test]
fn refuses_owned_file_collision() {
    let fixture = Fixture::new("owned");
    fs::create_dir(fixture.root.join("sdk")).unwrap();
    fs::write(fixture.root.join("sdk/input.rs"), "owned").unwrap();
    git(&fixture.root, &["add", "sdk/input.rs"]);
    let error = fixture.bootstrap(false).unwrap_err().to_string();
    assert!(error.contains("owned file"), "{error}");
}

#[test]
fn missing_locked_path_fails_before_writing() {
    let fixture = Fixture::new("missing");
    fixture.write_lock("example/sdk", &["sdk", "missing"]);
    assert!(fixture.bootstrap(false).is_err());
    assert!(!fixture.root.join("sdk").exists());
}

#[test]
fn changed_lock_requires_refresh() {
    let fixture = Fixture::new("relock");
    fixture.bootstrap(false).unwrap();
    fixture.write_lock("example/renamed", &["sdk"]);
    let error = fixture.bootstrap(true).unwrap_err().to_string();
    assert!(error.contains("not bootstrapped"), "{error}");
}

#[test]
fn imported_files_are_written_fresh() {
    // A repin must not leave a file dated before the objects Cargo built
    // from the previous revision.
    let fixture = Fixture::new("mtime");
    let before = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
    fixture.bootstrap(false).unwrap();
    let modified = fs::metadata(fixture.root.join("sdk/input.rs"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(modified >= before);
}

#[test]
fn dropped_imports_are_deleted() {
    let mut fixture = Fixture::new("dropped");
    fs::create_dir(fixture.source.join("tools")).unwrap();
    fs::write(fixture.source.join("tools/extra.rs"), "extra").unwrap();
    git(&fixture.source, &["add", "."]);
    git(
        &fixture.source,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qm",
            "Two",
        ],
    );
    fixture.revision = git(&fixture.source, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    fixture.write_lock("example/sdk", &["sdk", "tools"]);
    assert_eq!(fixture.bootstrap(false).unwrap(), Outcome::Bootstrapped(2));
    fixture.write_lock("example/sdk", &["sdk"]);
    assert_eq!(fixture.bootstrap(false).unwrap(), Outcome::Bootstrapped(1));
    assert!(!fixture.root.join("tools/extra.rs").exists());
}
