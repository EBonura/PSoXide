#!/usr/bin/env python3
"""Turn an emu-bench summary CSV into data/emu-bench.json for the comparison page.

Usage:
    python3 scripts/import_emu_bench.py [path/to/summary.csv]

With no argument it reads data/emu-bench/summary-final.csv (a verbatim copy of
the harness output). Copy a newer summary over that file, rerun this script and
commit both. The page never computes anything the harness didn't measure: this
script only selects rows, renames them for display and takes geometric means.
"""

import csv
import json
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CSV = ROOT / "data" / "emu-bench" / "summary-final.csv"
OUT = ROOT / "data" / "emu-bench.json"

# NTSC field rate the harness paces to. Headroom = frames per CPU-second / this.
NTSC_FIELD_HZ = 59.826

GAMES = [
    ("crash", "Crash Bandicoot"),
    ("tekken3", "Tekken 3"),
    ("wipeout2097", "WipEout 2097"),
    ("re2", "Resident Evil 2"),
    ("mslugx", "Metal Slug X"),
    ("hl", "Half-Life (homebrew disc)"),
]
# Averages use the five commercial games every emulator booted. Beetle PSX
# could not boot the Half-Life disc, so it stays out of the mean.
AVERAGE_OVER = ["crash", "tekken3", "wipeout2097", "re2", "mslugx"]

# Which harness rows appear on the page, in order. Anything else in the CSV
# (branch builds, loop-variant controls) is ignored.
EMULATORS = [
    # id in CSV,          display name,  build,     cpu core,      role
    ("psoxide-22ae5d6", "PSoXide", "22ae5d6", "interpreter", "current"),
    ("psoxide-9eff8c8", "PSoXide", "9eff8c8", "interpreter", "history"),
    ("psoxide-aacadfd", "PSoXide", "aacadfd", "interpreter", "history"),
    ("duckstation-interp", "DuckStation", "459a6fc", "interpreter", "peer"),
    ("duckstation-rec", "DuckStation", "459a6fc", "recompiler", "peer"),
    ("beetle-interp", "Beetle PSX", "0.9.44.1 (ee042b7)", "interpreter", "peer"),
    ("beetle-dynarec", "Beetle PSX", "0.9.44.1 (ee042b7)", "recompiler", "peer"),
    ("redux-interp", "PCSX-Redux", "1b7a4f1d", "interpreter", "peer"),
    ("redux-dynarec", "PCSX-Redux", "1b7a4f1d", "recompiler", "peer"),
]

METRICS = {
    "fps": "eff_fps_per_cpu_s",
    "cpu": "rt_cpu_pct",
    "mem": "rt_footprint_mb",
}


def num(value):
    return float(value) if value not in ("", None) else None


def nice_ceiling(x):
    """Smallest round number >= x, used as a chart axis end."""
    exp = math.floor(math.log10(x))
    for m in (1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10):
        if m * 10 ** exp >= x:
            return m * 10 ** exp


def geomean(values):
    return math.exp(sum(math.log(v) for v in values) / len(values))


def main():
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CSV
    rows = list(csv.DictReader(src.open(newline="")))
    by_emu = {}
    for row in rows:
        by_emu.setdefault(row["emu"], {})[row["game"]] = row

    emulators = []
    for emu_id, name, build, core, role in EMULATORS:
        games = by_emu.get(emu_id)
        if not games:
            sys.exit(f"{src}: no rows for {emu_id}")
        per_game = []
        for game_id, game_name in GAMES:
            r = games.get(game_id)
            if r is None:
                per_game.append({"game": game_id, "measured": False})
                continue
            entry = {"game": game_id, "measured": True}
            for key, col in METRICS.items():
                entry[key] = num(r[col])
                entry[key + "_min"] = num(r[col + "_min"])
                entry[key + "_max"] = num(r[col + "_max"])
                entry[key + "_n"] = int(r[col + "_n"])
            per_game.append(entry)

        mean = {}
        for key, col in METRICS.items():
            mean[key] = round(geomean([float(games[g][col]) for g in AVERAGE_OVER]), 2)
        mean["headroom"] = round(mean["fps"] / NTSC_FIELD_HZ, 1)

        emulators.append({
            "id": emu_id,
            "name": name,
            "build": build,
            "core": core,
            "role": role,
            "psoxide": name == "PSoXide",
            "needs_bios": name != "PSoXide",
            "label": f"{name} {build}" if name == "PSoXide" else f"{name} ({core})",
            "mean": mean,
            "per_game": per_game,
        })

    peers = [e for e in emulators if e["role"] != "history"]
    data = {
        "source_csv": src.name,
        "field_hz": NTSC_FIELD_HZ,
        "games": [{"id": g, "name": n} for g, n in GAMES],
        "average_over": AVERAGE_OVER,
        "emulators": emulators,
        "max": {
            k: max(e["mean"][k] for e in peers) for k in ("fps", "cpu", "mem", "headroom")
        },
        "axis": {
            k: nice_ceiling(max(e["mean"][k] for e in peers)) for k in ("fps", "cpu", "mem", "headroom")
        },
        "axis_history": {
            k: nice_ceiling(max(e["mean"][k] for e in emulators if e["psoxide"])) for k in ("fps", "cpu", "mem")
        },
        "max_history": {
            k: max(e["mean"][k] for e in emulators if e["psoxide"]) for k in ("fps", "mem")
        },
    }
    OUT.write_text(json.dumps(data, indent=1) + "\n")
    print(f"wrote {OUT.relative_to(ROOT)} from {src} ({len(emulators)} emulators)")


if __name__ == "__main__":
    main()
