"""Integration checks for locked component exports and local edit protection."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("bootstrap", Path(__file__).with_name("bootstrap-components.py"))
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name) / "source"
        self.root = Path(self.temp.name) / "consumer"
        for path in [self.source, self.root]:
            path.mkdir()
            self.git(path, "init", "-q")
        (self.source / "sdk").mkdir()
        (self.source / "sdk/input.rs").write_text("committed source")
        self.git(self.source, "add", ".")
        self.git(self.source, "-c", "user.name=Component test", "-c", "user.email=test@example.invalid", "commit", "-qm", "Fixture")
        revision = self.git(self.source, "rev-parse", "HEAD").strip()
        self.lock = {"schema": 1, "components": {"sdk": {"repository": "example/sdk", "revision": revision, "paths": ["sdk"]}}}
        self.write_lock()

    def git(self, path, *args):
        return subprocess.check_output(["git", "-C", str(path), *args], text=True)

    def write_lock(self):
        (self.root / "components.lock.json").write_text(json.dumps(self.lock))

    def run_bootstrap(self, check=False):
        with contextlib.redirect_stdout(io.StringIO()):
            bootstrap.materialize(self.root, {"sdk": str(self.source)}, check)

    def test_exports_locked_content_and_checks_offline(self):
        (self.source / "sdk/input.rs").write_text("unstaged source")
        self.run_bootstrap()
        self.assertEqual((self.root / "sdk/input.rs").read_text(), "committed source")
        self.run_bootstrap(check=True)

    def test_refuses_modified_imports(self):
        self.run_bootstrap()
        (self.root / "sdk/input.rs").write_text("local edits")
        with self.assertRaisesRegex(RuntimeError, "Imported file changed"):
            self.run_bootstrap()
        self.assertEqual((self.root / "sdk/input.rs").read_text(), "local edits")

    def test_refuses_owned_file_collision(self):
        (self.root / "sdk").mkdir()
        (self.root / "sdk/input.rs").write_text("owned")
        self.git(self.root, "add", "sdk/input.rs")
        with self.assertRaisesRegex(RuntimeError, "owned file"):
            self.run_bootstrap()

    def test_missing_locked_path_fails_before_writing(self):
        self.lock["components"]["sdk"]["paths"].append("missing")
        self.write_lock()
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_bootstrap()
        self.assertFalse((self.root / "sdk").exists())

    def test_changed_lock_requires_refresh(self):
        self.run_bootstrap()
        self.lock["components"]["sdk"]["repository"] = "example/renamed"
        self.write_lock()
        with self.assertRaisesRegex(RuntimeError, "not bootstrapped"):
            self.run_bootstrap(check=True)


if __name__ == "__main__":
    unittest.main()
