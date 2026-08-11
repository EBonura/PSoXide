#!/usr/bin/env python3
"""Validate the editor blank-slate BSP acceptance artifact and headless run."""

from __future__ import annotations

import argparse
import hashlib
import re
from pathlib import Path


SPAWN_X = 192
SPAWN_Z = 192
TELEMETRY_POSITION_BIAS = 1_000_000


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"editor blank playtest check failed: {message}")


def read_required(path: Path) -> bytes:
    require(path.is_file(), f"missing {path}")
    data = path.read_bytes()
    require(bool(data), f"empty {path}")
    return data


def match_int(text: str, pattern: str, label: str) -> int:
    match = re.search(pattern, text, re.MULTILINE)
    require(match is not None, f"headless log has no {label}")
    return int(match.group(1))


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--pxbsp", type=Path, required=True)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--disc", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()

    project = read_required(args.project).decode("utf-8")
    manifest = read_required(args.manifest).decode("utf-8")
    pxbsp = read_required(args.pxbsp)
    exe = read_required(args.exe)
    disc = read_required(args.disc)
    log = read_required(args.log).decode("utf-8")

    require(
        'name: "Editor Blank Playtest Acceptance"' in project,
        "exported project name is not deterministic",
    )
    require("pub const PLAYTEST_USES_PXBSP: bool = true;" in manifest, "cook is not BSP")
    mover_ids = re.search(
        r"pub static PXBSP_MOVER_NODE_IDS: &\[u32\] = &\[(\d+)\];", manifest
    )
    require(mover_ids is not None, "expected exactly one authored brush Door mover")
    require(
        "PlayerSpawnRecord { room: RoomIndex(0), x: 192, y: 65, z: 192" in manifest,
        "authored Player Spawn record is missing",
    )
    box_props = re.search(
        r"pub static BOX_PROPS:.*?= &\[(.*?)\n\];", manifest, re.DOTALL
    )
    require(box_props is not None, "cooked Box Prop table is missing")
    require(box_props.group(1).count("LevelBoxPropRecord {") == 1, "expected one cooked Box Prop")
    require(
        "x: 1536, y: 65, z: 1536" in box_props.group(1)
        and "flags: 1" in box_props.group(1),
        "Box Prop placement/collision record is missing",
    )
    require(manifest.count("PointLightRecord {") == 1, "expected one cooked Point Light")
    require(
        "PointLightRecord { room: RoomIndex(0), x: 512, y: 320, z: 512" in manifest,
        "authored Point Light record is missing",
    )
    require(pxbsp[:4] == b"PXB%", "cooked world has no PXBSP magic")
    guest_frames = match_int(log, r"^guest_profile_frames=(\d+)$", "guest frame count")
    visual_frames = match_int(log, r"^\s*visual_frames=(\d+)$", "visual frame count")
    player_x_biased = match_int(
        log, r"^\s*player local x\s+.*latest=(\d+)$", "player X counter"
    )
    player_z_biased = match_int(
        log, r"^\s*player local z\s+.*latest=(\d+)$", "player Z counter"
    )
    player_x = player_x_biased - TELEMETRY_POSITION_BIAS
    player_z = player_z_biased - TELEMETRY_POSITION_BIAS
    require(guest_frames >= 120, f"only {guest_frames} guest frames completed")
    require(visual_frames >= 60, f"only {visual_frames} visual frames completed")
    require(
        (player_x, player_z) != (SPAWN_X, SPAWN_Z),
        "held input never moved the player away from the authored spawn",
    )
    require("visual_budget_status=pass" in log, "visual budget status did not pass")
    require("cadence_status=steady" in log, "guest cadence was not steady")
    vram_hash = re.search(r"^vram_fnv1a_64=(0x[0-9a-f]+)$", log, re.MULTILINE)
    display_hash = re.search(r"^display_fnv1a_64=(0x[0-9a-f]+)", log, re.MULTILINE)
    require(vram_hash is not None, "headless log has no VRAM hash")
    require(display_hash is not None, "headless log has no display hash")

    print("editor blank playtest check: PASS")
    print(f"  guest/visual frames: {guest_frames}/{visual_frames}")
    print(f"  player XZ: ({SPAWN_X}, {SPAWN_Z}) -> ({player_x}, {player_z})")
    print(f"  vram/display: {vram_hash.group(1)} / {display_hash.group(1)}")
    for label, path, data in [
        ("PXBSP", args.pxbsp, pxbsp),
        ("MIPS EXE", args.exe, exe),
        ("disc BIN", args.disc, disc),
    ]:
        print(f"  {label}: {len(data)} bytes sha256={sha256(data)} ({path})")


if __name__ == "__main__":
    main()
