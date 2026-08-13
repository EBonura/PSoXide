#!/usr/bin/env python3
"""Validate the editor blank-slate BSP acceptance artifact and headless run."""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import re
from dataclasses import dataclass
from pathlib import Path


SPAWN_X = 192
SPAWN_Z = 192
ROOM_INNER_MIN_Z = 64
PLAYER_RADIUS = 16
TELEMETRY_POSITION_BIAS = 1_000_000
EXPECTED_ROUTE_TICKS = 152
EXPECTED_PAD_POLLS = 122
EXPECTED_GUEST_FRAMES = 121
EXPECTED_VISUAL_FRAMES = 60
MIN_SKY_CYCLES = 100_000
EXPECTED_SKY_HITS = 57
EXPECTED_TRI_PRIMS = 1_053
EXPECTED_LAST_TRI_PRIMS = 19
EXPECTED_VRAM_HASH = "0xda4fdfcfe3c27a87"
EXPECTED_DISPLAY_HASH = "0xdf10b8075b9d718b"
EXPECTED_GPU_CENSUS: dict[str, int | str] = {
    "rows": 152,
    "commands": 3_393,
    "draws": 1_576,
    "fills": 60,
    "textured_tris": 927,
    "textured_quads": 555,
    "textured_rects": 84,
    "run_draw_words": 19_213,
    # Re-pinned when the authored panorama became available to the resident
    # PXBSP path. The brush world bytes and movement/triangle pins stay fixed;
    # the extra rows, draws and textured quads are the streamed cyclorama.
    "run_draw_hash": "0x9d2fff86ad1adfdc",
}
IMAGE_SUFFIXES = {".bmp", ".gif", ".jpeg", ".jpg", ".png", ".ppm", ".webp"}


@dataclass(frozen=True)
class ReplayEvidence:
    route_ticks: int
    pad_polls: int
    guest_frames: int
    sim_ticks: int
    visual_frames: int
    sky_cycles: int
    sky_hits: int
    tri_prims: int
    last_tri_prims: int
    player_x: int
    player_z: int
    vram_hash: str
    display_hash: str
    display_width: int
    display_height: int


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


def parse_replay(text: str, label: str) -> ReplayEvidence:
    route = re.search(r"^route-ticks=(\d+)\s+port1-polls=(\d+)$", text, re.MULTILINE)
    require(route is not None, f"{label} has no route/pad counters")
    tri = re.search(
        r"^\s*tri prims\s+total=(\d+)\s+per_frame=\d+\s+latest=(\d+)$",
        text,
        re.MULTILINE,
    )
    require(tri is not None, f"{label} has no triangle counter")
    display = re.search(
        r"^display_fnv1a_64=(0x[0-9a-f]+)\s+w=(\d+)\s+h=(\d+)$",
        text,
        re.MULTILINE,
    )
    require(display is not None, f"{label} has no display hash or dimensions")
    vram = re.search(r"^vram_fnv1a_64=(0x[0-9a-f]+)$", text, re.MULTILINE)
    require(vram is not None, f"{label} has no VRAM hash")
    player_x = match_int(
        text, r"^\s*player local x\s+.*latest=(\d+)$", f"{label} player X counter"
    )
    player_z = match_int(
        text, r"^\s*player local z\s+.*latest=(\d+)$", f"{label} player Z counter"
    )
    require("visual_budget_status=pass" in text, f"{label} visual budget did not pass")
    require("cadence_status=steady" in text, f"{label} cadence was not steady")
    sky = re.search(
        r"^\s*sky\s+total=(\d+).*?hits=(\d+)$", text, re.MULTILINE
    )
    require(sky is not None, f"{label} has no sky-stage profile evidence")
    return ReplayEvidence(
        route_ticks=int(route.group(1)),
        pad_polls=int(route.group(2)),
        guest_frames=match_int(
            text, r"^guest_profile_frames=(\d+)$", f"{label} guest frame count"
        ),
        sim_ticks=match_int(text, r"^\s*sim_ticks=(\d+)$", f"{label} sim tick count"),
        visual_frames=match_int(
            text, r"^\s*visual_frames=(\d+)$", f"{label} visual frame count"
        ),
        sky_cycles=int(sky.group(1)),
        sky_hits=int(sky.group(2)),
        tri_prims=int(tri.group(1)),
        last_tri_prims=int(tri.group(2)),
        player_x=player_x - TELEMETRY_POSITION_BIAS,
        player_z=player_z - TELEMETRY_POSITION_BIAS,
        vram_hash=vram.group(1),
        display_hash=display.group(1),
        display_width=int(display.group(2)),
        display_height=int(display.group(3)),
    )


def parse_gpu_census(data: bytes, label: str) -> tuple[int, dict[str, int | str]]:
    text = data.decode("utf-8")
    rows = list(csv.DictReader(io.StringIO(text)))
    require(bool(rows), f"{label} GPU census has no rows")
    totals: dict[str, int | str] = {
        "rows": len(rows),
        "commands": sum(int(row["commands"]) for row in rows),
        "draws": sum(int(row["draws"]) for row in rows),
        "fills": sum(int(row["fills"]) for row in rows),
        "textured_tris": sum(int(row["textured_tris"]) for row in rows),
        "textured_quads": sum(int(row["textured_quads"]) for row in rows),
        "textured_rects": sum(int(row["textured_rects"]) for row in rows),
        "run_draw_words": int(rows[-1]["run_draw_words"]),
        "run_draw_hash": rows[-1]["run_draw_hash"],
    }
    return len(rows), totals


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--pxbsp", type=Path, required=True)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--disc", type=Path, required=True)
    parser.add_argument("--log-a", type=Path, required=True)
    parser.add_argument("--log-b", type=Path, required=True)
    parser.add_argument("--gpu-a", type=Path, required=True)
    parser.add_argument("--gpu-b", type=Path, required=True)
    parser.add_argument("--artifact-root", type=Path, required=True)
    args = parser.parse_args()

    project = read_required(args.project).decode("utf-8")
    manifest = read_required(args.manifest).decode("utf-8")
    pxbsp = read_required(args.pxbsp)
    exe = read_required(args.exe)
    disc = read_required(args.disc)
    log_a = read_required(args.log_a).decode("utf-8")
    log_b = read_required(args.log_b).decode("utf-8")
    gpu_a = read_required(args.gpu_a)
    gpu_b = read_required(args.gpu_b)

    require(
        'name: "Editor Blank Playtest Acceptance"' in project,
        "exported project name is not deterministic",
    )
    require("aletha" not in project.lower(), "acceptance project still references Aletha")
    require(
        "assets/textures/courtyard_cobbles.psxt" in project
        and "assets/textures/courtyard_brick.psxt" in project,
        "acceptance project lost the neutral courtyard materials",
    )
    require("pub const PLAYTEST_USES_PXBSP: bool = true;" in manifest, "cook is not BSP")
    pxbsp_room = re.search(
        r'LevelRoomRecord \{ name: "PXBSP World".*?'
        r'sky: LevelSkyRecord \{.*?flags: 1,.*?'
        r'cloud_layer: LevelCloudLayerRecord \{ texture_asset: AssetId\((\d+)\)',
        manifest,
        re.DOTALL,
    )
    require(pxbsp_room is not None, "PXBSP room has no enabled authored sky panorama")
    sky_asset_id = int(pxbsp_room.group(1))
    sky_asset = re.search(
        rf"LevelAssetRecord \{{ id: AssetId\({sky_asset_id}\), "
        rf"kind: AssetKind::Texture, bytes: .*?, ram_bytes: (\d+), "
        rf"vram_bytes: (\d+), flags: asset_flags::STREAMED_GAMEPLAY_TRANSIENT \}}",
        manifest,
    )
    require(sky_asset is not None, "PXBSP sky is not a gameplay-streamed texture asset")
    sky_bytes = int(sky_asset.group(1))
    sky_vram_bytes = int(sky_asset.group(2))
    require(
        (sky_bytes, sky_vram_bytes) == (65_820, 65_792),
        f"unexpected 512x256 panorama envelope: {sky_bytes}/{sky_vram_bytes}",
    )
    require(
        re.search(
            rf"LevelWorldPackEntryRecord \{{ room: RoomIndex\({sky_asset_id}\), "
            rf"sector_offset: \d+, sector_count: \d+, byte_size: {sky_bytes}, "
            rf"checksum: \d+ \}}",
            manifest,
        )
        is not None,
        "PXBSP sky has no matching UI.PAK table entry",
    )
    gameplay_stage = re.search(
        r"pub const GAMEPLAY_PACK_MAX_CHUNK_BYTES: usize = (\d+);", manifest
    )
    require(
        gameplay_stage is not None and int(gameplay_stage.group(1)) >= sky_bytes,
        "gameplay streaming stage cannot hold the PXBSP sky",
    )
    room_vram = re.search(
        r"pub static ROOM_0_REQUIRED_VRAM: &\[AssetId\] = &\[(.*?)\];", manifest
    )
    require(
        room_vram is not None and f"AssetId({sky_asset_id})" in room_vram.group(1),
        "PXBSP room residency does not retain its sky texture",
    )
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
    require(exe[:8] == b"PS-X EXE", "runtime artifact is not a PlayStation executable")

    replay_a = parse_replay(log_a, "replay A")
    replay_b = parse_replay(log_b, "replay B")
    require(replay_a == replay_b, f"replay evidence drifted: {replay_a} != {replay_b}")
    require(
        replay_a.route_ticks == EXPECTED_ROUTE_TICKS,
        f"route tick pin drifted: {replay_a.route_ticks}",
    )
    require(
        replay_a.pad_polls == EXPECTED_PAD_POLLS,
        f"pad poll pin drifted: {replay_a.pad_polls}",
    )
    require(
        replay_a.guest_frames == EXPECTED_GUEST_FRAMES
        and replay_a.sim_ticks == EXPECTED_GUEST_FRAMES,
        f"guest/sim frame pin drifted: {replay_a.guest_frames}/{replay_a.sim_ticks}",
    )
    require(
        replay_a.visual_frames == EXPECTED_VISUAL_FRAMES,
        f"visual frame pin drifted: {replay_a.visual_frames}",
    )
    require(
        replay_a.sky_cycles >= MIN_SKY_CYCLES
        and replay_a.sky_hits == EXPECTED_SKY_HITS,
        "PXBSP panorama did not perform the expected rendered work: "
        f"{replay_a.sky_cycles} cycles/{replay_a.sky_hits} hits",
    )
    require(
        replay_a.tri_prims == EXPECTED_TRI_PRIMS
        and replay_a.last_tri_prims == EXPECTED_LAST_TRI_PRIMS,
        f"triangle counter pin drifted: {replay_a.tri_prims}/{replay_a.last_tri_prims}",
    )
    require(
        replay_a.vram_hash == EXPECTED_VRAM_HASH
        and replay_a.display_hash == EXPECTED_DISPLAY_HASH,
        f"render hash pin drifted: {replay_a.vram_hash}/{replay_a.display_hash}",
    )
    require(
        (replay_a.player_x, replay_a.player_z) != (SPAWN_X, SPAWN_Z),
        "held input never moved the player away from the authored spawn",
    )
    expected_wall_contact = (SPAWN_X, ROOM_INNER_MIN_Z + PLAYER_RADIUS)
    require(
        (replay_a.player_x, replay_a.player_z) == expected_wall_contact,
        "sustained forward input did not stop at the authored wall and player-radius boundary: "
        f"expected {expected_wall_contact}, got {(replay_a.player_x, replay_a.player_z)}",
    )
    require(
        (replay_a.display_width, replay_a.display_height) == (320, 240),
        f"unexpected display dimensions: {replay_a.display_width}x{replay_a.display_height}",
    )

    require(gpu_a == gpu_b, "GPU command census is not byte deterministic across replays")
    _, gpu_totals = parse_gpu_census(gpu_a, "replay A")
    require(
        gpu_totals == EXPECTED_GPU_CENSUS,
        f"GPU command census pin drifted: {gpu_totals}",
    )

    image_artifacts = sorted(
        path
        for path in args.artifact_root.rglob("*")
        if path.is_file() and path.suffix.lower() in IMAGE_SUFFIXES
    )
    require(
        not image_artifacts,
        "acceptance emitted image artifacts: " + ", ".join(map(str, image_artifacts)),
    )

    print("editor blank playtest check: PASS")
    print(f"  deterministic replays: 2 x {replay_a.route_ticks} route ticks")
    print(f"  guest/visual frames: {replay_a.guest_frames}/{replay_a.visual_frames}")
    print(
        f"  player XZ: ({SPAWN_X}, {SPAWN_Z}) -> "
        f"({replay_a.player_x}, {replay_a.player_z}) at wall"
    )
    print(f"  vram/display: {replay_a.vram_hash} / {replay_a.display_hash}")
    print(
        "  GPU census: "
        f"rows={gpu_totals['rows']} commands={gpu_totals['commands']} "
        f"draws={gpu_totals['draws']} fills={gpu_totals['fills']} "
        f"textured_tris={gpu_totals['textured_tris']} "
        f"textured_quads={gpu_totals['textured_quads']} "
        f"textured_rects={gpu_totals['textured_rects']} "
        f"draw_words={gpu_totals['run_draw_words']} "
        f"draw_hash={gpu_totals['run_draw_hash']}"
    )
    print("  image artifacts: 0")
    for label, path, data in [
        ("PXBSP", args.pxbsp, pxbsp),
        ("MIPS EXE", args.exe, exe),
        ("disc BIN", args.disc, disc),
    ]:
        print(f"  {label}: {len(data)} bytes sha256={sha256(data)} ({path})")


if __name__ == "__main__":
    main()
