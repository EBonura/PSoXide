#!/usr/bin/env python3
"""Validate demo3 CD-streaming performance, smoothness, and memory budgets."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


STAGE_RE = re.compile(
    r"^\s{2}(.+?)\s+total=(\d+)\s+per_frame=([0-9.]+)\s+per_hit=([0-9.]+)\s+max_hit=(\d+)\s+hits=(\d+)"
)
COUNTER_RE = re.compile(r"^\s{2}(.+?)\s+total=(\d+)\s+per_frame=([0-9.]+)")
PACE_RE = re.compile(r"^\s{2}([a-z0-9_]+)=([^\s]+)")
PAYLOAD_RE = re.compile(r"payload=(\d+)B")
DISC_BYTES_RE = re.compile(r"wrote .+ \((\d+) bytes = \d+ sectors")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, type=Path)
    parser.add_argument("--generated-dir", required=True, type=Path)
    parser.add_argument("--exe", type=Path)
    parser.add_argument("--bin", dest="bin_path", type=Path)
    parser.add_argument("--label", default="demo3 disc stream")
    parser.add_argument("--expected-display-hash", required=True)
    parser.add_argument("--expected-vram-hash", required=True)
    parser.add_argument("--min-visual-frames", type=int, required=True)
    parser.add_argument("--min-room-cached-draws", type=int, default=1)
    parser.add_argument("--max-room-uncached-draws", type=int, default=0)
    parser.add_argument("--max-room-cache-fallbacks", type=int, default=0)
    parser.add_argument("--max-stream-failed-loads", type=int, default=0)
    parser.add_argument("--max-cd-load-max-hit", type=int, default=100_000)
    parser.add_argument("--max-cd-load-per-hit", type=int, default=10_000)
    parser.add_argument("--max-cd-sectors", type=int, default=80)
    parser.add_argument("--max-cd-bytes", type=int, default=140_000)
    parser.add_argument("--max-render-cycles-per-visual", type=int, default=3_100_000)
    parser.add_argument("--max-visual-lateness-vblanks", type=int, default=3)
    parser.add_argument("--max-deadline-miss-ratio", type=float, default=0.75)
    parser.add_argument("--stream-slot-bytes", type=int, default=32 * 1024)
    parser.add_argument("--stream-slot-count", type=int, default=6)
    parser.add_argument("--max-stream-slot-ram", type=int, default=192 * 1024)
    parser.add_argument("--max-stream-chunk-bytes", type=int, default=32 * 1024)
    parser.add_argument("--max-stream-chunks-total", type=int, default=384 * 1024)
    parser.add_argument("--max-texture-payload-bytes", type=int, default=64 * 1024)
    parser.add_argument("--max-exe-payload-bytes", type=int, default=560 * 1024)
    parser.add_argument("--max-exe-file-bytes", type=int, default=900 * 1024)
    parser.add_argument("--max-bin-file-bytes", type=int, default=1_250_000)
    return parser.parse_args()


def fail(message: str) -> None:
    raise SystemExit(f"[stream-budget] FAIL: {message}")


def parse_profile(text: str) -> dict[str, object]:
    profile: dict[str, object] = {
        "stages": {},
        "counters": {},
        "pacing": {},
        "display_hash": None,
        "vram_hash": None,
        "exe_payload_bytes": None,
        "disc_bytes": None,
    }
    section = None
    for line in text.splitlines():
        if line.startswith("display_fnv1a_64="):
            profile["display_hash"] = line.split("=", 1)[1].split()[0].lower()
        elif line.startswith("vram_fnv1a_64="):
            profile["vram_hash"] = line.split("=", 1)[1].split()[0].lower()
        elif "payload=" in line:
            if match := PAYLOAD_RE.search(line):
                profile["exe_payload_bytes"] = int(match.group(1))
        elif line.startswith("wrote "):
            if match := DISC_BYTES_RE.search(line):
                profile["disc_bytes"] = int(match.group(1))
        elif line == "guest_profile_pacing:":
            section = "pacing"
        elif line == "guest_profile_stages:":
            section = "stages"
        elif line == "guest_profile_counters:":
            section = "counters"
        elif section == "pacing":
            if match := PACE_RE.match(line):
                profile["pacing"][match.group(1)] = parse_number(match.group(2))
        elif section == "stages":
            if match := STAGE_RE.match(line):
                profile["stages"][match.group(1).strip()] = {
                    "total": int(match.group(2)),
                    "per_frame": float(match.group(3)),
                    "per_hit": float(match.group(4)),
                    "max_hit": int(match.group(5)),
                    "hits": int(match.group(6)),
                }
        elif section == "counters":
            if match := COUNTER_RE.match(line):
                profile["counters"][match.group(1).strip()] = {
                    "total": int(match.group(2)),
                    "per_frame": float(match.group(3)),
                }
    return profile


def parse_number(value: str) -> int | float | str:
    if value == "unknown":
        return value
    if "." in value:
        return float(value)
    try:
        return int(value)
    except ValueError:
        return value


def counter_total(profile: dict[str, object], name: str) -> int:
    return int(profile["counters"].get(name, {}).get("total", 0))


def stage(profile: dict[str, object], name: str) -> dict[str, float | int]:
    return profile["stages"].get(name, {})


def pacing_int(profile: dict[str, object], name: str) -> int:
    value = profile["pacing"].get(name, 0)
    return int(value) if isinstance(value, (int, float)) else 0


def validate_profile(args: argparse.Namespace, profile: dict[str, object]) -> list[str]:
    notes = []
    if profile["display_hash"] != args.expected_display_hash.lower():
        fail(
            f"{args.label}: display hash {profile['display_hash']} != {args.expected_display_hash}"
        )
    if profile["vram_hash"] != args.expected_vram_hash.lower():
        fail(f"{args.label}: VRAM hash {profile['vram_hash']} != {args.expected_vram_hash}")

    visual_frames = pacing_int(profile, "visual_frames")
    if visual_frames < args.min_visual_frames:
        fail(f"{args.label}: visual_frames {visual_frames} < {args.min_visual_frames}")

    render_per_visual = pacing_int(profile, "render_cycles_per_visual_frame")
    if render_per_visual > args.max_render_cycles_per_visual:
        fail(
            f"{args.label}: render cycles/visual {render_per_visual} > "
            f"{args.max_render_cycles_per_visual}"
        )

    max_lateness = pacing_int(profile, "visual_max_lateness_vblanks")
    if max_lateness > args.max_visual_lateness_vblanks:
        fail(
            f"{args.label}: max visual lateness {max_lateness} > "
            f"{args.max_visual_lateness_vblanks} vblanks"
        )
    misses = pacing_int(profile, "visual_deadline_misses")
    if visual_frames > 0 and misses / visual_frames > args.max_deadline_miss_ratio:
        fail(
            f"{args.label}: deadline miss ratio {misses}/{visual_frames} exceeds "
            f"{args.max_deadline_miss_ratio:.2f}"
        )

    cd_stage = stage(profile, "cd room chunk load")
    if not cd_stage:
        fail(f"{args.label}: missing cd room chunk load stage")
    if int(cd_stage["max_hit"]) > args.max_cd_load_max_hit:
        fail(
            f"{args.label}: CD load max_hit {cd_stage['max_hit']} > {args.max_cd_load_max_hit}"
        )
    if float(cd_stage["per_hit"]) > args.max_cd_load_per_hit:
        fail(
            f"{args.label}: CD load per_hit {cd_stage['per_hit']:.0f} > "
            f"{args.max_cd_load_per_hit}"
        )

    cd_sectors = counter_total(profile, "cd room chunk sectors")
    if cd_sectors > args.max_cd_sectors:
        fail(f"{args.label}: CD sectors {cd_sectors} > {args.max_cd_sectors}")
    cd_bytes = counter_total(profile, "cd room chunk bytes")
    if cd_bytes > args.max_cd_bytes:
        fail(f"{args.label}: CD bytes {cd_bytes} > {args.max_cd_bytes}")
    if cd_bytes <= 0:
        fail(f"{args.label}: no streamed room bytes were read")

    cached_draws = counter_total(profile, "room cached draws")
    if cached_draws < args.min_room_cached_draws:
        fail(f"{args.label}: room cached draws {cached_draws} < {args.min_room_cached_draws}")
    uncached_draws = counter_total(profile, "room uncached draws")
    if uncached_draws > args.max_room_uncached_draws:
        fail(f"{args.label}: room uncached draws {uncached_draws} > {args.max_room_uncached_draws}")
    cache_fallbacks = counter_total(profile, "room cache fallbacks")
    if cache_fallbacks > args.max_room_cache_fallbacks:
        fail(
            f"{args.label}: room cache fallbacks {cache_fallbacks} > "
            f"{args.max_room_cache_fallbacks}"
        )
    failed_loads = counter_total(profile, "room stream failed loads")
    if failed_loads > args.max_stream_failed_loads:
        fail(
            f"{args.label}: stream failed loads {failed_loads} > "
            f"{args.max_stream_failed_loads}"
        )

    exe_payload = profile.get("exe_payload_bytes")
    if exe_payload is not None:
        if int(exe_payload) > args.max_exe_payload_bytes:
            fail(
                f"{args.label}: EXE payload {exe_payload} > {args.max_exe_payload_bytes}; "
                "room data may have been embedded again"
            )
        notes.append(f"exe_payload={exe_payload}")
    disc_bytes = profile.get("disc_bytes")
    if disc_bytes is not None:
        if int(disc_bytes) > args.max_bin_file_bytes:
            fail(f"{args.label}: BIN size {disc_bytes} > {args.max_bin_file_bytes}")
        notes.append(f"bin_bytes={disc_bytes}")

    notes.append(f"render_per_visual={render_per_visual}")
    notes.append(f"cd_max_hit={cd_stage['max_hit']}")
    notes.append(f"cd_per_hit={cd_stage['per_hit']:.0f}")
    notes.append(f"cd_bytes={cd_bytes}")
    notes.append(f"cached_draws={cached_draws}")
    return notes


def validate_files(args: argparse.Namespace) -> list[str]:
    generated = args.generated_dir
    if not generated.exists():
        fail(f"generated dir does not exist: {generated}")
    chunks = sorted((generated / "stream_chunks").glob("room_*.psxc"))
    if not chunks:
        fail(f"no stream chunks under {generated / 'stream_chunks'}")
    chunk_sizes = [path.stat().st_size for path in chunks]
    max_chunk = max(chunk_sizes)
    total_chunks = sum(chunk_sizes)
    if max_chunk > args.max_stream_chunk_bytes:
        fail(f"largest stream chunk {max_chunk} > {args.max_stream_chunk_bytes}")
    if total_chunks > args.max_stream_chunks_total:
        fail(f"stream chunk total {total_chunks} > {args.max_stream_chunks_total}")

    slot_ram = args.stream_slot_bytes * args.stream_slot_count
    if slot_ram > args.max_stream_slot_ram:
        fail(f"stream slot RAM {slot_ram} > {args.max_stream_slot_ram}")
    if max_chunk > args.stream_slot_bytes:
        fail(f"largest stream chunk {max_chunk} does not fit {args.stream_slot_bytes}B slot")

    texture_payload = sum(path.stat().st_size for path in generated.glob("textures/*.psxt"))
    texture_payload += sum(path.stat().st_size for path in generated.glob("models/*/*.psxt"))
    if texture_payload > args.max_texture_payload_bytes:
        fail(f"texture payload bytes {texture_payload} > {args.max_texture_payload_bytes}")

    if args.exe and args.exe.exists():
        exe_bytes = args.exe.stat().st_size
        if exe_bytes > args.max_exe_file_bytes:
            fail(f"EXE file size {exe_bytes} > {args.max_exe_file_bytes}")
    if args.bin_path and args.bin_path.exists():
        bin_bytes = args.bin_path.stat().st_size
        if bin_bytes > args.max_bin_file_bytes:
            fail(f"BIN file size {bin_bytes} > {args.max_bin_file_bytes}")

    return [
        f"stream_chunks={len(chunks)}",
        f"max_chunk={max_chunk}",
        f"stream_chunk_total={total_chunks}",
        f"stream_slot_ram={slot_ram}",
        f"texture_payload={texture_payload}",
    ]


def main() -> None:
    args = parse_args()
    text = args.profile.read_text()
    profile = parse_profile(text)
    notes = validate_profile(args, profile)
    notes.extend(validate_files(args))
    print(f"[stream-budget] PASS {args.label}: " + ", ".join(notes))


if __name__ == "__main__":
    main()
