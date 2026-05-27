#!/usr/bin/env python3
"""Run a built PSX disc under DuckStation and assert TTY progress markers."""

from __future__ import annotations

import argparse
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path


DEFAULT_EXPECT = (
    "psx-rt: main",
    "magikarp: init ok",
    "psx-engine: present ok",
    "magikarp: cdda setmode",
    "magikarp: cdda setmode ack",
    "magikarp: cdda demute",
    "magikarp: cdda demute ack",
    "magikarp: cdda play",
    "magikarp: cdda play ack",
    "magikarp: cdda ok",
)

IMPORTANT_PATTERNS = (
    "I/TTY:",
    "E(",
    "W(",
    "V/PerfMon:",
    "V/AudioStream:",
)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def duckstation_binary_from_app(app: Path) -> Path | None:
    macos = app / "Contents" / "MacOS"
    for name in ("DuckStation", "duckstation-qt", "duckstation"):
        candidate = macos / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    return None


def discover_duckstation(explicit: str | None) -> Path:
    if explicit:
        candidate = Path(explicit).expanduser()
        if candidate.suffix == ".app":
            binary = duckstation_binary_from_app(candidate)
            if binary:
                return binary
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
        raise SystemExit(f"DuckStation binary not executable: {candidate}")

    for name in ("DuckStation", "duckstation-qt", "duckstation"):
        found = shutil.which(name)
        if found:
            return Path(found)

    app_candidates = [
        Path("/Applications/DuckStation.app"),
        Path.home() / "Applications" / "DuckStation.app",
        Path.home() / "Downloads" / "DuckStation.app",
    ]

    if sys.platform == "darwin" and shutil.which("mdfind"):
        try:
            found_apps = subprocess.run(
                ["mdfind", 'kMDItemFSName == "DuckStation.app"'],
                check=False,
                capture_output=True,
                text=True,
            ).stdout.splitlines()
            app_candidates.extend(Path(path) for path in found_apps)
        except OSError:
            pass

    seen: set[Path] = set()
    for app in app_candidates:
        app = app.expanduser()
        if app in seen:
            continue
        seen.add(app)
        binary = duckstation_binary_from_app(app)
        if binary:
            return binary

    raise SystemExit(
        "DuckStation not found. Set DUCKSTATION_BIN=/path/to/DuckStation "
        "or pass --duckstation /path/to/DuckStation.app."
    )


def reader_thread(pipe, lines: "queue.Queue[str]") -> None:
    try:
        for line in iter(pipe.readline, ""):
            lines.put(line)
    finally:
        try:
            pipe.close()
        except OSError:
            pass


def important_tail(log_lines: list[str], count: int = 80) -> list[str]:
    important = [
        line.rstrip()
        for line in log_lines
        if any(pattern in line for pattern in IMPORTANT_PATTERNS)
    ]
    return important[-count:]


def build_command(args: argparse.Namespace, duckstation: Path, cue: Path) -> list[str]:
    command = [
        str(duckstation),
        "-batch",
        "-fastboot",
        "-nofullscreen",
        "-earlyconsole",
    ]
    if not args.gui:
        command.append("-nogui")
    command.extend(["--", str(cue)])
    return command


def run_harness(args: argparse.Namespace) -> int:
    root = repo_root()
    cue = Path(args.cue).expanduser()
    if not cue.is_absolute():
        cue = root / cue
    if not cue.is_file():
        raise SystemExit(f"Disc cue not found: {cue}")

    duckstation = discover_duckstation(args.duckstation or os.environ.get("DUCKSTATION_BIN"))
    log_path = Path(args.log).expanduser()
    if not log_path.is_absolute():
        log_path = root / log_path
    log_path.parent.mkdir(parents=True, exist_ok=True)

    expected = [] if args.no_default_expect else list(DEFAULT_EXPECT)
    expected.extend(args.expect or [])

    command = build_command(args, duckstation, cue)
    print("DuckStation:", duckstation)
    print("Disc:       ", cue)
    print("Log:        ", log_path)
    print("Command:    ", " ".join(command))

    process = subprocess.Popen(
        command,
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert process.stdout is not None

    lines: "queue.Queue[str]" = queue.Queue()
    thread = threading.Thread(target=reader_thread, args=(process.stdout, lines), daemon=True)
    thread.start()

    seen: set[str] = set()
    all_lines: list[str] = []
    deadline = time.monotonic() + args.timeout
    complete_deadline: float | None = None

    with log_path.open("w", encoding="utf-8", errors="replace") as log:
        while time.monotonic() < deadline:
            if process.poll() is not None and lines.empty():
                break

            try:
                line = lines.get(timeout=0.1)
            except queue.Empty:
                if complete_deadline is not None and time.monotonic() >= complete_deadline:
                    break
                continue

            all_lines.append(line)
            log.write(line)
            log.flush()

            for marker in expected:
                if marker in line:
                    seen.add(marker)

            if expected and len(seen) == len(expected) and complete_deadline is None:
                complete_deadline = time.monotonic() + args.settle

            if complete_deadline is not None and time.monotonic() >= complete_deadline:
                break

        while not lines.empty():
            line = lines.get_nowait()
            all_lines.append(line)
            log.write(line)

    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)

    missing = [marker for marker in expected if marker not in seen]
    if missing:
        print("\nMissing DuckStation markers:")
        for marker in missing:
            print(f"  - {marker}")
        print("\nImportant log tail:")
        for line in important_tail(all_lines):
            print(line)
        return 1

    print("\nDuckStation markers observed:")
    for marker in expected:
        print(f"  ok {marker}")
    return 0


def parse_args() -> argparse.Namespace:
    root = repo_root()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cue",
        default=root / "build/examples/mipsel-sony-psx/release/game-magikaaaaaarp-pong.cue",
        help="Disc .cue to boot.",
    )
    parser.add_argument(
        "--duckstation",
        default=None,
        help="DuckStation executable or .app path. Defaults to DUCKSTATION_BIN, PATH, then common macOS app paths.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=45.0,
        help="Maximum wall-clock seconds to let DuckStation run.",
    )
    parser.add_argument(
        "--settle",
        type=float,
        default=0.75,
        help="Seconds to keep running after all markers are observed.",
    )
    parser.add_argument(
        "--log",
        default=root / "build/duckstation-harness/game-magikaaaaaarp-pong.log",
        help="Captured DuckStation stdout/stderr log path.",
    )
    parser.add_argument(
        "--expect",
        action="append",
        help="Additional marker required in the DuckStation log.",
    )
    parser.add_argument(
        "--no-default-expect",
        action="store_true",
        help="Only require markers supplied by --expect.",
    )
    parser.add_argument(
        "--gui",
        action="store_true",
        help="Show DuckStation's main window instead of using -nogui.",
    )
    return parser.parse_args()


if __name__ == "__main__":
    raise SystemExit(run_harness(parse_args()))
