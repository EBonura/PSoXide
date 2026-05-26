#!/usr/bin/env python3
"""Reject expensive scalar types from PS1 runtime Rust code.

The runtime target should stay on fixed-point 16/32-bit integer math. This guard
intentionally does not reject `u8`/`i8`, because those are valid packed hardware
and asset formats, and it does not reject `usize`/`isize`, because they are the
native 32-bit index width on the PS1 target.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

FLOAT_TYPES = {"f32", "f64"}
FORBIDDEN_TYPE_RE = re.compile(r"\b(f32|f64|u64|i64|u128|i128)\b")
FLOAT_LITERAL_RE = re.compile(
    r"(?<![\w.])\d+\.\d+(?:[eE][+-]?\d+)?(?:_?f(?:32|64))?\b"
    r"|(?<![\w.])\d+[eE][+-]?\d+(?:_?f(?:32|64))?\b"
)
ALLOW_LINE_MARKER = "psx-numeric-allow-line"
ALLOW_NEXT_LINE_MARKER = "psx-numeric-allow-next-line"


def runtime_roots() -> list[Path]:
    roots = [
        ROOT / "engine/crates/psx-engine/src",
        ROOT / "engine/crates/psx-level/src",
    ]
    roots.extend(sorted((ROOT / "engine/examples").glob("*/src")))
    for crate_src in sorted((ROOT / "sdk/crates").glob("*/src")):
        if crate_src.parent.name == "psx-gte-core":
            continue
        roots.append(crate_src)
    return roots


def runtime_files() -> list[Path]:
    seen: set[Path] = set()
    files: list[Path] = []
    for root in runtime_roots():
        if not root.exists():
            continue
        for path in sorted(root.rglob("*.rs")):
            if path in seen:
                continue
            seen.add(path)
            files.append(path)
    return files


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def replace_preserving_newlines(chars: list[str], start: int, end: int) -> None:
    for idx in range(start, end):
        if chars[idx] != "\n":
            chars[idx] = " "


def raw_string_end(src: str, start: int) -> int | None:
    if src[start] != "r":
        return None
    idx = start + 1
    while idx < len(src) and src[idx] == "#":
        idx += 1
    if idx >= len(src) or src[idx] != '"':
        return None
    hashes = idx - start - 1
    marker = '"' + ("#" * hashes)
    end = src.find(marker, idx + 1)
    if end < 0:
        return len(src)
    return end + len(marker)


def char_literal_end(src: str, start: int) -> int | None:
    if src[start] != "'" or start + 1 >= len(src):
        return None
    next_char = src[start + 1]
    if next_char.isalpha() or next_char == "_":
        return None
    idx = start + 1
    if src[idx] == "\\":
        idx += 2
    else:
        idx += 1
    if idx < len(src) and src[idx] == "'":
        return idx + 1
    return None


def blank_comments_and_literals(src: str) -> str:
    chars = list(src)
    idx = 0
    while idx < len(src):
        raw_end = raw_string_end(src, idx)
        if raw_end is not None:
            replace_preserving_newlines(chars, idx, raw_end)
            idx = raw_end
            continue

        if src.startswith("//", idx):
            end = src.find("\n", idx)
            if end < 0:
                end = len(src)
            replace_preserving_newlines(chars, idx, end)
            idx = end
            continue

        if src.startswith("/*", idx):
            depth = 1
            end = idx + 2
            while end < len(src) and depth > 0:
                if src.startswith("/*", end):
                    depth += 1
                    end += 2
                elif src.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            replace_preserving_newlines(chars, idx, end)
            idx = end
            continue

        if src[idx] == '"':
            end = idx + 1
            escaped = False
            while end < len(src):
                current = src[end]
                if escaped:
                    escaped = False
                elif current == "\\":
                    escaped = True
                elif current == '"':
                    end += 1
                    break
                end += 1
            replace_preserving_newlines(chars, idx, end)
            idx = end
            continue

        char_end = char_literal_end(src, idx)
        if char_end is not None:
            replace_preserving_newlines(chars, idx, char_end)
            idx = char_end
            continue

        idx += 1
    return "".join(chars)


def line_col(src: str, offset: int) -> tuple[int, int]:
    line = src.count("\n", 0, offset) + 1
    line_start = src.rfind("\n", 0, offset)
    if line_start < 0:
        return line, offset + 1
    return line, offset - line_start


def report_context(src: str, line: int) -> str:
    lines = src.splitlines()
    if 1 <= line <= len(lines):
        return lines[line - 1].strip()
    return ""


def allowed_lines(src: str) -> set[int]:
    allowed: set[int] = set()
    for number, line in enumerate(src.splitlines(), start=1):
        if ALLOW_LINE_MARKER in line:
            allowed.add(number)
        if ALLOW_NEXT_LINE_MARKER in line:
            allowed.add(number + 1)
    return allowed


def main() -> int:
    violations: list[str] = []
    allowed_hit_count = 0
    checked_files = 0

    for path in runtime_files():
        checked_files += 1
        rel = relative(path)
        src = path.read_text(encoding="utf-8")
        code = blank_comments_and_literals(src)
        allowed = allowed_lines(src)

        for match in FORBIDDEN_TYPE_RE.finditer(code):
            token = match.group(1)
            line, col = line_col(code, match.start())
            if line in allowed:
                allowed_hit_count += 1
                continue
            kind = "float type" if token in FLOAT_TYPES else "wide integer type"
            violations.append(
                f"{rel}:{line}:{col}: forbidden {kind} `{token}`\n"
                f"    {report_context(src, line)}"
            )

        for match in FLOAT_LITERAL_RE.finditer(code):
            line, col = line_col(code, match.start())
            if line in allowed:
                allowed_hit_count += 1
                continue
            violations.append(
                f"{rel}:{line}:{col}: forbidden float literal `{match.group(0)}`\n"
                f"    {report_context(src, line)}"
            )

    if violations:
        print("PS1 runtime numeric guard failed.", file=sys.stderr)
        print(
            "Runtime Rust must avoid f32/f64, float literals, and 64/128-bit "
            "integer types unless explicitly allowlisted.",
            file=sys.stderr,
        )
        print(
            "Use fixed-point 16/32-bit integer math, or move host-only code out "
            "of runtime source roots.",
            file=sys.stderr,
        )
        print("", file=sys.stderr)
        print(
            f"Use `// {ALLOW_NEXT_LINE_MARKER}: reason` only for a reviewed "
            "exception that cannot move out of runtime code.",
            file=sys.stderr,
        )
        print("", file=sys.stderr)
        for violation in violations:
            print(violation, file=sys.stderr)
        return 1

    print(
        f"runtime numeric guard: ok "
        f"({checked_files} files, {allowed_hit_count} line-level allow hits)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
