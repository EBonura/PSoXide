#!/usr/bin/env python3
"""Promote the curated DP atlas cells to semantic Cortex texture names."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parent
SELECTION = ROOT / "selected_tiles.json"


def find_tile(extracted: Path, folder: str, tile_id: str) -> Path:
    matches = list((extracted / folder).glob(f"*/{tile_id}.png"))
    if len(matches) != 1:
        raise SystemExit(f"expected one {folder} source for {tile_id}, found {len(matches)}")
    return matches[0]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("extracted", type=Path)
    args = parser.parse_args()
    extracted = args.extracted.resolve()
    selection = json.loads(SELECTION.read_text())

    selected_root = ROOT / "selected"
    if selected_root.exists():
        shutil.rmtree(selected_root)

    for entry in selection:
        for folder in ("32", "64_nearest", "64_scale2x"):
            source = find_tile(extracted, folder, entry["tile_id"])
            destination = selected_root / folder / f"{entry['name']}.png"
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)

    columns = 4
    cell = 84
    rows = (len(selection) + columns - 1) // columns
    sheet = Image.new("RGB", (columns * cell, rows * cell), (34, 38, 40))
    draw = ImageDraw.Draw(sheet)
    for index, entry in enumerate(selection):
        source = selected_root / "64_scale2x" / f"{entry['name']}.png"
        with Image.open(source) as opened:
            image = opened.convert("RGBA")
        x = (index % columns) * cell + 10
        y = (index // columns) * cell
        checker = Image.new("RGB", (64, 64), (78, 78, 78))
        checker.paste(image.convert("RGB"), mask=image.getchannel("A"))
        sheet.paste(checker, (x, y))
        label = entry["name"].removeprefix("dp_bsp_")[:14]
        draw.text((x, y + 66), label, fill=(225, 225, 225))
    sheet.save(selected_root / "contact_scale2x.png", optimize=True)
    print(f"promoted {len(selection)} selected tiles into {selected_root}")


if __name__ == "__main__":
    main()
