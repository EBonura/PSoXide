#!/usr/bin/env python3
"""Regression fixtures for artifacts the previous filename audit missed."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("sony-material-audit.py")
spec = importlib.util.spec_from_file_location("audit", SCRIPT)
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class AuditTests(unittest.TestCase):
    def test_uppercase_path_and_renamed_blob(self):
        self.assertTrue(audit.reasons("assets/My BIOS.BIN", 524288, b""))
        self.assertTrue(audit.reasons("opaque.data", 524288, b""))
        self.assertTrue(audit.reasons("bios/SCPH1001.BIN", 1, b""))

    def test_vendor_marker_in_executable_header(self):
        header = bytearray(2048)
        header[:8] = b"PS-X EXE"
        marker = b"Sony Computer Entertainment Inc."
        header[0x4c:0x4c+len(marker)] = marker
        self.assertTrue(audit.reasons("demo.exe", 800000, header))
        header[0x4c:] = bytes(2048 - 0x4c)
        self.assertFalse(audit.reasons("demo.exe", 800000, header))
        self.assertFalse(audit.reasons("sdk/target-mipsel-sony-psx.txt", 128, marker))

    def test_history_finds_deleted_tagged_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            def git(*args):
                return subprocess.run(["git", "-C", directory, *args], check=True, capture_output=True)
            git("init", "-q")
            git("config", "user.name", "Audit fixture")
            git("config", "user.email", "fixture@example.invalid")
            (repo / "SCPH1001.BIN").write_bytes(bytes(524288))
            git("add", ".")
            git("commit", "-qm", "fixture")
            git("tag", "published-fixture")
            git("rm", "SCPH1001.BIN")
            git("commit", "-qm", "remove fixture")
            self.assertFalse(list(audit.scan_working(repo)))
            self.assertTrue(any(audit.reasons(*entry) for entry in audit.scan_history(repo)))

    def test_missing_repository_fails(self):
        result = subprocess.run(["python3", str(SCRIPT), "--repo", "/does-not-exist"], capture_output=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
