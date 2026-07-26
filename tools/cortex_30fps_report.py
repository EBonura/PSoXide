#!/usr/bin/env python3
"""Summarise cortex replay captures and compare lockstep visual hashes."""

from __future__ import annotations

import argparse
import csv
import statistics
from pathlib import Path

NTSC_HZ = 60.0
TWO_VBLANK_CYCLES = 1_128_960


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def integer_rows(path: Path) -> list[dict[str, int]]:
    # Older route logs can have a short initial row while the first pad poll is
    # still being assembled. DictReader represents the absent trailing field
    # as None; it is semantically the same as the counter's zero initial value.
    return [{key: int(value or 0) for key, value in row.items()} for row in read_csv(path)]


def percentile(values: list[int], fraction: float) -> int:
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    weight = position - lower
    return round(ordered[lower] + (ordered[upper] - ordered[lower]) * weight)


def gameplay_rows(rows: list[dict[str, int]]) -> list[dict[str, int]]:
    start = next(
        index
        for index, row in enumerate(rows)
        if row["room_active_chunks"] > 0 and row["room_surfaces_considered"] > 0
    )
    return rows[start:]


def visual_period_work(rows: list[dict[str, int]]) -> list[int]:
    periods: list[int] = []
    pending_updates = 0
    for row in rows:
        pending_updates += row["update"]
        if row["visual_frames"] > 0:
            periods.append(row["render"] + pending_updates)
            pending_updates = 0
    return periods


def mean_counter(rows: list[dict[str, int]], name: str) -> float | None:
    if not rows or name not in rows[0]:
        return None
    return statistics.mean(row[name] for row in rows)


def format_count(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.1f}"


def summarise(run_dir: Path) -> dict[str, str]:
    all_profile_rows = integer_rows(run_dir / "profile.csv")
    gameplay = gameplay_rows(all_profile_rows)
    visuals = [row for row in gameplay if row["visual_frames"] > 0]
    room_visuals = [row for row in visuals if row["room_surfaces_considered"] > 0]
    render = [row["render"] for row in visuals]
    period_work = visual_period_work(gameplay)
    visual_count = sum(row["visual_frames"] for row in gameplay)
    within_budget = sum(value <= TWO_VBLANK_CYCLES for value in period_work)
    icache_stalls_per_visual: float | None = None
    route_path = run_dir / "route.csv"
    if route_path.exists():
        route = integer_rows(route_path)
        if route and "icache_refill_stall_cycles_delta" in route[0]:
            gameplay_start_cycles = gameplay[0]["start_bus_cycles"]
            gameplay_stalls = sum(
                row["icache_refill_stall_cycles_delta"]
                for row in route
                if row["bus_cycles"] >= gameplay_start_cycles
            )
            icache_stalls_per_visual = gameplay_stalls / visual_count

    return {
        "run": run_dir.parent.name,
        "fps": f"{NTSC_HZ * visual_count / len(gameplay):.2f}",
        "visuals/ticks": f"{visual_count}/{len(gameplay)}",
        "render mean": f"{statistics.mean(render):,.0f}",
        "render p95": f"{percentile(render, 0.95):,}",
        "render max": f"{max(render):,}",
        "period <=2vb": f"{100.0 * within_budget / len(period_work):.1f}%",
        "I$ stalls": format_count(icache_stalls_per_visual),
        "surfaces": format_count(
            mean_counter(room_visuals, "room_surfaces_considered")
        ),
        "TR candidates": format_count(
            mean_counter(room_visuals, "room_surf_tr_subdivision_candidates")
        ),
        "TR submitted": format_count(
            mean_counter(room_visuals, "room_surf_tr_subdivision_submitted")
        ),
        "primitives": format_count(mean_counter(room_visuals, "tri_primitives")),
    }


def print_table(summaries: list[dict[str, str]]) -> None:
    columns = list(summaries[0])
    print("| " + " | ".join(columns) + " |")
    print("|" + "|".join("---" for _ in columns) + "|")
    for summary in summaries:
        print("| " + " | ".join(summary[column] for column in columns) + " |")


def visual_hashes(path: Path) -> dict[int, str]:
    return {
        int(row["guest_frame"]): row["display_hash"]
        for row in read_csv(path / "visual-hashes.csv")
    }


def compare_hashes(baseline: Path, candidate: Path) -> None:
    baseline_hashes = visual_hashes(baseline)
    candidate_hashes = visual_hashes(candidate)
    baseline_frames = set(baseline_hashes)
    candidate_frames = set(candidate_hashes)
    missing = sorted(baseline_frames - candidate_frames)
    extra = sorted(candidate_frames - baseline_frames)
    mismatches = sorted(
        frame
        for frame in baseline_frames & candidate_frames
        if baseline_hashes[frame] != candidate_hashes[frame]
    )
    print(
        "\nlockstep visual hashes: "
        f"matched={len(baseline_frames & candidate_frames) - len(mismatches)} "
        f"mismatched={len(mismatches)} missing={len(missing)} extra={len(extra)}"
    )
    if mismatches or missing or extra:
        first = (mismatches or missing or extra)[0]
        raise SystemExit(f"visual equivalence failed at guest frame {first}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("runs", nargs="+", type=Path)
    parser.add_argument(
        "--compare-lockstep",
        action="store_true",
        help="compare visual-hashes.csv for the first two runs by guest frame",
    )
    args = parser.parse_args()

    print_table([summarise(path) for path in args.runs])
    if args.compare_lockstep:
        if len(args.runs) != 2:
            parser.error("--compare-lockstep requires exactly two run directories")
        compare_hashes(args.runs[0], args.runs[1])


if __name__ == "__main__":
    main()
