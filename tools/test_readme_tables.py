#!/usr/bin/env python3
"""Fails when a crate or example directory is missing from a README table, or a table row names one that no longer exists."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def dirs(path):
    return {p.name for p in (ROOT / path).iterdir() if p.is_dir()}


def listed(readme, pattern):
    return set(re.findall(pattern, (ROOT / readme).read_text(), re.M))


class ReadmeTables(unittest.TestCase):
    def check(self, readme, pattern, path):
        have, want = listed(readme, pattern), dirs(path)
        self.assertEqual(want - have, set(), f"{readme}: missing from table")
        self.assertEqual(have - want, set(), f"{readme}: listed but not in {path}")

    def test_sdk_crates(self):
        self.check("sdk/README.md", r"^\| \[`([\w-]+)`\]\(crates/", "sdk/crates")

    def test_sdk_examples(self):
        self.check("sdk/README.md", r"^\| `([\w-]+)` \|", "sdk/examples")

    def test_host_crates(self):
        self.check("crates/README.md", r"^\| \[`([\w-]+)`\]\(", "crates")


if __name__ == "__main__":
    unittest.main()
