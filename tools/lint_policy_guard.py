#!/usr/bin/env python3
"""Keep workspace lint policy identical across PSoXide Cargo workspaces."""

from __future__ import annotations

import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFESTS = [
    ROOT / "Cargo.toml",
    ROOT / "engine/Cargo.toml",
    ROOT / "editor/Cargo.toml",
    ROOT / "emu/Cargo.toml",
    ROOT / "sdk/Cargo.toml",
]
SECTIONS = ["workspace.lints.rust", "workspace.lints.clippy"]


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def extract_section(path: Path, section: str) -> list[str] | None:
    lines = path.read_text(encoding="utf-8").splitlines()
    header = f"[{section}]"
    for index, line in enumerate(lines):
        if line.strip() != header:
            continue
        body: list[str] = []
        cursor = index + 1
        while cursor < len(lines):
            candidate = lines[cursor]
            if candidate.startswith("["):
                break
            if candidate.strip():
                body.append(candidate.rstrip())
            cursor += 1
        return body
    return None


def main() -> int:
    violations: list[str] = []
    reference_path = MANIFESTS[0]
    reference = {
        section: extract_section(reference_path, section)
        for section in SECTIONS
    }
    for section, body in reference.items():
        if body is None:
            violations.append(f"{relative(reference_path)} missing [{section}]")

    for manifest in MANIFESTS[1:]:
        for section in SECTIONS:
            expected = reference.get(section)
            actual = extract_section(manifest, section)
            if actual is None:
                violations.append(f"{relative(manifest)} missing [{section}]")
                continue
            if expected is not None and actual != expected:
                violations.append(
                    f"{relative(manifest)} [{section}] differs from "
                    f"{relative(reference_path)}"
                )

    if violations:
        print("lint policy guard failed.", file=sys.stderr)
        print(
            "Keep [workspace.lints.rust] and [workspace.lints.clippy] "
            "identical in every Cargo workspace manifest.",
            file=sys.stderr,
        )
        print("", file=sys.stderr)
        for violation in violations:
            print(violation, file=sys.stderr)
        return 1

    print(f"lint policy guard: ok ({len(MANIFESTS)} manifests)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
