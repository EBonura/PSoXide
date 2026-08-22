#!/usr/bin/env python3
"""Regression tests for the public web-disc delivery guard."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("verify-web-dist.py")
SPEC = importlib.util.spec_from_file_location("verify_web_dist", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify_web_dist = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_web_dist)


class PublicWebDiscTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.dist = Path(self.temporary_directory.name)
        manifest_lines = ["data demo-data.bin.gz 1 1 0"]
        for number, title in verify_web_dist.PUBLIC_TRACK_TITLES.items():
            filename = f"track-{number:02}.flac"
            manifest_lines.append(f"track {number} {filename} 1 1 0 {title}")
            (self.dist / filename).write_bytes(b"x")
        (self.dist / "demo-data.bin.gz").write_bytes(b"x")
        (self.dist / "web-manifest.txt").write_text(
            "\n".join(manifest_lines) + "\n", encoding="utf-8"
        )
        cue_lines = ['FILE "demo-disc.bin" BINARY']
        for number in range(1, 9):
            mode = "MODE2/2352" if number == 1 else "AUDIO"
            cue_lines.extend((f"  TRACK {number:02} {mode}", "    INDEX 01 00:00:00"))
        (self.dist / "demo-disc.cue").write_text(
            "\n".join(cue_lines) + "\n", encoding="utf-8"
        )

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def test_reviewed_public_layout_passes(self) -> None:
        self.assertEqual(verify_web_dist.verify(self.dist), (8, 8))
        verify_web_dist.verify_public_disc(self.dist)

    def test_half_life_title_is_rejected(self) -> None:
        manifest = self.dist / "web-manifest.txt"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "track 9 track-09.flac 1 1 0 HALF-LIFE\n",
            encoding="utf-8",
        )
        with self.assertRaises(verify_web_dist.VerificationError):
            verify_web_dist.verify_public_disc(self.dist)

    def test_unreviewed_track_layout_is_rejected(self) -> None:
        cue = self.dist / "demo-disc.cue"
        cue.write_text(
            cue.read_text(encoding="utf-8")
            + "  TRACK 09 AUDIO\n    INDEX 01 00:00:00\n",
            encoding="utf-8",
        )
        with self.assertRaises(verify_web_dist.VerificationError):
            verify_web_dist.verify_public_disc(self.dist)


if __name__ == "__main__":
    unittest.main()
