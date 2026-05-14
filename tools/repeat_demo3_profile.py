#!/usr/bin/env python3
"""Run repeated demo3 profiles and report median guest metrics."""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
EXAMPLE_OUT = REPO / "build/examples/mipsel-sony-psx/release"
BIN = EXAMPLE_OUT / "editor-playtest.bin"
EXE = EXAMPLE_OUT / "editor-playtest.exe"

PACE_RE = re.compile(r"^\s{2}([a-z0-9_]+)=([^\s]+)")
STAGE_RE = re.compile(
    r"^\s{2}(.+?)\s+total=(\d+)\s+per_frame=([0-9.]+)\s+per_hit=([0-9.]+)\s+max_hit=(\d+)\s+hits=(\d+)"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--out-dir", type=Path, default=Path("/tmp/psoxide-demo3-repeat"))
    parser.add_argument("--mode", choices=("default", "forward", "both"), default="both")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument(
        "--frontend-bin",
        type=Path,
        help="Existing frontend binary to launch; falls back to cargo run when missing.",
    )
    parser.add_argument("--visual-frames", type=int, default=60)
    parser.add_argument("--forward-visual-frames", type=int, default=80)
    parser.add_argument("--guest-frames", type=int, default=720)
    parser.add_argument("--forward-guest-frames", type=int, default=1200)
    parser.add_argument("--steps", type=int, default=360_000_000)
    parser.add_argument("--forward-steps", type=int, default=600_000_000)
    parser.add_argument("--expected-default-display-hash")
    parser.add_argument("--expected-default-vram-hash")
    parser.add_argument("--expected-forward-display-hash")
    parser.add_argument("--expected-forward-vram-hash")
    return parser.parse_args()


def run(cmd: list[str], cwd: Path = REPO) -> str:
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    if proc.returncode != 0:
        sys.stdout.write(proc.stdout)
        raise SystemExit(proc.returncode)
    return proc.stdout


def build_demo3() -> None:
    run(["make", "cook-playtest", "PROJECT=projects/demo3/project.ron"])
    run(["make", "build-editor-playtest"])
    run(
        [
            "cargo",
            "run",
            "--release",
            "--",
            "--exe",
            f"../../{EXE.relative_to(REPO)}",
            "--out",
            f"../../{BIN.relative_to(REPO)}",
            "--volume",
            "PSOXIDE",
            "--cdtest-sectors",
            "32",
            "--world-pack-rooms-dir",
            "../../engine/examples/editor-playtest/generated/stream_chunks",
            "--world-pack-order-file",
            "../../engine/examples/editor-playtest/generated/world_pack_order.txt",
        ],
        cwd=REPO / "tools/mkisopsx",
    )


def parse_profile(text: str) -> dict[str, object]:
    profile: dict[str, object] = {
        "display_hash": "",
        "vram_hash": "",
        "pacing": {},
        "stages": {},
    }
    section = ""
    for line in text.splitlines():
        if line.startswith("display_fnv1a_64="):
            profile["display_hash"] = line.split("=", 1)[1].split()[0].lower()
        elif line.startswith("vram_fnv1a_64="):
            profile["vram_hash"] = line.split("=", 1)[1].split()[0].lower()
        elif line == "guest_profile_pacing:":
            section = "pacing"
        elif line == "guest_profile_stages:":
            section = "stages"
        elif line.startswith("guest_profile_"):
            section = ""
        elif section == "pacing":
            if match := PACE_RE.match(line):
                profile["pacing"][match.group(1)] = parse_number(match.group(2))
        elif section == "stages":
            if match := STAGE_RE.match(line):
                profile["stages"][match.group(1).strip()] = {
                    "per_hit": float(match.group(4)),
                    "max_hit": int(match.group(5)),
                    "hits": int(match.group(6)),
                }
    return profile


def parse_number(value: str) -> int | float | str:
    if value == "unknown":
        return value
    try:
        return float(value) if "." in value else int(value)
    except ValueError:
        return value


def frontend_args(mode: str, args: argparse.Namespace, run_index: int) -> list[str]:
    forward = mode == "forward"
    dump = args.out_dir / f"demo3-{mode}-{run_index + 1}.ppm"
    frontend_bin = args.frontend_bin
    if frontend_bin is not None and not frontend_bin.is_absolute():
        frontend_bin = REPO / frontend_bin
    if frontend_bin is not None and frontend_bin.exists():
        cmd = [str(frontend_bin), "launch", "--path", f"../{BIN.relative_to(REPO)}"]
    else:
        cmd = [
            "cargo",
            "run",
            "-p",
            "frontend",
            "--release",
            "--",
            "launch",
            "--path",
            f"../{BIN.relative_to(REPO)}",
        ]
    cmd.extend(
        [
            "--guest-visual-frames",
            str(args.forward_visual_frames if forward else args.visual_frames),
            "--guest-frames",
            str(args.forward_guest_frames if forward else args.guest_frames),
            "--steps",
            str(args.forward_steps if forward else args.steps),
            "--dump-hw",
            str(dump),
            "--dump-hash",
            "--dump-guest-profile",
        ]
    )
    if forward:
        cmd.append("--hold-forward")
    return cmd


def expected_hashes(mode: str, args: argparse.Namespace) -> tuple[str | None, str | None]:
    if mode == "forward":
        return args.expected_forward_display_hash, args.expected_forward_vram_hash
    return args.expected_default_display_hash, args.expected_default_vram_hash


def run_profile(mode: str, args: argparse.Namespace, run_index: int) -> dict[str, object]:
    text = run(frontend_args(mode, args, run_index), cwd=REPO / "emu")
    log = args.out_dir / f"demo3-{mode}-{run_index + 1}.log"
    log.write_text(text)
    profile = parse_profile(text)
    expected_display, expected_vram = expected_hashes(mode, args)
    if expected_display and profile["display_hash"] != expected_display.lower():
        raise SystemExit(
            f"{mode} run {run_index + 1}: display hash {profile['display_hash']} != {expected_display}"
        )
    if expected_vram and profile["vram_hash"] != expected_vram.lower():
        raise SystemExit(
            f"{mode} run {run_index + 1}: VRAM hash {profile['vram_hash']} != {expected_vram}"
        )
    return profile


def metric(profile: dict[str, object], name: str) -> float:
    value = profile["pacing"].get(name, 0)
    return float(value) if isinstance(value, (int, float)) else 0.0


def stage_per_hit(profile: dict[str, object], name: str) -> float:
    stage = profile["stages"].get(name, {})
    value = stage.get("per_hit", 0)
    return float(value) if isinstance(value, (int, float)) else 0.0


def median(values: list[float]) -> float:
    return statistics.median(values) if values else 0.0


def print_summary(mode: str, profiles: list[dict[str, object]]) -> None:
    display_hashes = sorted({str(profile["display_hash"]) for profile in profiles})
    vram_hashes = sorted({str(profile["vram_hash"]) for profile in profiles})
    print(f"\n{mode}:")
    print(f"  display_hashes={', '.join(display_hashes)}")
    print(f"  vram_hashes={', '.join(vram_hashes)}")
    print(
        "  median "
        f"render/visual={median([metric(p, 'render_cycles_per_visual_frame') for p in profiles]):.0f} "
        f"misses={median([metric(p, 'visual_deadline_misses') for p in profiles]):.0f} "
        f"max_late={median([metric(p, 'visual_max_lateness_vblanks') for p in profiles]):.0f}"
    )
    print(
        "  median stages "
        f"room={median([stage_per_hit(p, 'room') for p in profiles]):.0f} "
        f"room_surface={median([stage_per_hit(p, 'room surface draw') for p in profiles]):.0f} "
        f"player={median([stage_per_hit(p, 'player') for p in profiles]):.0f} "
        f"model_faces={median([stage_per_hit(p, 'textured model faces') for p in profiles]):.0f}"
    )


def main() -> None:
    args = parse_args()
    args.runs = max(args.runs, 1)
    args.out_dir.mkdir(parents=True, exist_ok=True)
    if not args.skip_build:
        build_demo3()

    modes = ["default", "forward"] if args.mode == "both" else [args.mode]
    for mode in modes:
        profiles = []
        for run_index in range(args.runs):
            print(f"{mode} run {run_index + 1}/{args.runs}")
            profiles.append(run_profile(mode, args, run_index))
        print_summary(mode, profiles)


if __name__ == "__main__":
    main()
