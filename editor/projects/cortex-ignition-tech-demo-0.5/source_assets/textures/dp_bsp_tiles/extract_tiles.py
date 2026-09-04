#!/usr/bin/env python3
"""Slice the DP platform atlases into audited 32px and 64px tiles."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import shutil
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageStat


TILE_SIZE = 32
UPSCALED_SIZE = 64


@dataclass
class Tile:
    tile_id: str
    image: Image.Image
    classification: str
    alpha_coverage: float
    left_right_error: float
    top_bottom_error: float
    colour_count: int
    dominant_colour_fraction: float
    mean_luma: float
    luma_stddev: float
    entropy: float
    teal_fraction: float
    green_fraction: float
    blue_fraction: float
    magenta_fraction: float
    rust_fraction: float
    occurrences: list[dict[str, object]] = field(default_factory=list)


def edge_error(image: Image.Image) -> tuple[float, float]:
    rgba = image.convert("RGBA")
    width, height = rgba.size
    left_right = 0.0
    top_bottom = 0.0
    for y in range(height):
        left = rgba.getpixel((0, y))
        right = rgba.getpixel((width - 1, y))
        left_right += sum(abs(a - b) for a, b in zip(left, right)) / 4
    for x in range(width):
        top = rgba.getpixel((x, 0))
        bottom = rgba.getpixel((x, height - 1))
        top_bottom += sum(abs(a - b) for a, b in zip(top, bottom)) / 4
    return left_right / height, top_bottom / width


def classify(image: Image.Image) -> tuple[str, float]:
    alpha = image.getchannel("A")
    values = list(alpha.getdata())
    visible = sum(value > 0 for value in values)
    coverage = visible / len(values)
    if visible == 0:
        return "blank", coverage
    if min(values) == 255:
        return "opaque", coverage
    return "cutout", coverage


def visual_stats(
    image: Image.Image,
) -> tuple[int, float, float, float, float, float, float, float, float, float]:
    rgba = image.convert("RGBA")
    colours = rgba.getcolors(maxcolors=TILE_SIZE * TILE_SIZE) or []
    colour_count = len(colours)
    dominant = max((count for count, _ in colours), default=0) / (TILE_SIZE * TILE_SIZE)
    luma = rgba.convert("L")
    stats = ImageStat.Stat(luma)
    accents = {"teal": 0, "green": 0, "blue": 0, "magenta": 0, "rust": 0}
    visible = 0
    for red, green, blue, alpha in rgba.getdata():
        if alpha == 0:
            continue
        visible += 1
        if green > red * 1.25 and blue > red * 1.2 and abs(green - blue) <= 28 and max(green, blue) >= 35:
            accents["teal"] += 1
        if green > red * 1.45 and green > blue * 1.2 and green >= 35:
            accents["green"] += 1
        if blue > red * 1.35 and blue > green * 1.2 and blue >= 35:
            accents["blue"] += 1
        if red > green * 1.3 and blue > green * 1.25 and max(red, blue) >= 35:
            accents["magenta"] += 1
        if red > green * 1.2 and green > blue * 1.12 and red >= 35:
            accents["rust"] += 1
    scale = max(visible, 1)
    return (
        colour_count,
        dominant,
        stats.mean[0],
        stats.stddev[0],
        luma.entropy(),
        accents["teal"] / scale,
        accents["green"] / scale,
        accents["blue"] / scale,
        accents["magenta"] / scale,
        accents["rust"] / scale,
    )


def atlas_sources(pack_root: Path) -> list[tuple[str, Path]]:
    sources = []
    for set_dir in sorted(pack_root.glob("DP_Set*_v1.0")):
        atlas = set_dir / "_PNG" / "1. main platforms.png"
        if atlas.is_file():
            set_name = set_dir.name.removeprefix("DP_").removesuffix("_v1.0").lower()
            sources.append((set_name, atlas))
    return sources


def contact_sheet(tiles: list[Tile], destination: Path, columns: int = 12) -> None:
    if not tiles:
        return
    cell = 72
    label_height = 8
    rows = (len(tiles) + columns - 1) // columns
    sheet = Image.new("RGB", (columns * cell, rows * cell), (40, 43, 45))
    draw = ImageDraw.Draw(sheet)
    for index, tile in enumerate(tiles):
        x = (index % columns) * cell
        y = (index // columns) * cell
        checker = Image.new("RGB", (UPSCALED_SIZE, UPSCALED_SIZE), (90, 90, 90))
        checker_draw = ImageDraw.Draw(checker)
        for cy in range(0, UPSCALED_SIZE, 8):
            for cx in range(0, UPSCALED_SIZE, 8):
                if (cx // 8 + cy // 8) % 2:
                    checker_draw.rectangle((cx, cy, cx + 7, cy + 7), fill=(55, 55, 55))
        enlarged = tile.image.resize((UPSCALED_SIZE, UPSCALED_SIZE), Image.Resampling.NEAREST)
        checker.paste(enlarged.convert("RGB"), mask=enlarged.getchannel("A"))
        sheet.paste(checker, (x + 4, y))
        draw.text((x + 4, y + UPSCALED_SIZE), tile.tile_id[-7:], fill=(230, 230, 230))
    sheet.save(destination, optimize=True)


def scale2x(source: Image.Image) -> Image.Image:
    """Apply the classic Scale2x/EPX pixel-art enlargement algorithm."""
    image = source.convert("RGBA")
    width, height = image.size
    output = Image.new("RGBA", (width * 2, height * 2))
    for y in range(height):
        for x in range(width):
            centre = image.getpixel((x, y))
            north = image.getpixel((x, max(0, y - 1)))
            west = image.getpixel((max(0, x - 1), y))
            east = image.getpixel((min(width - 1, x + 1), y))
            south = image.getpixel((x, min(height - 1, y + 1)))
            if north != south and west != east:
                pixels = (
                    west if west == north else centre,
                    east if north == east else centre,
                    west if west == south else centre,
                    east if south == east else centre,
                )
            else:
                pixels = (centre, centre, centre, centre)
            output.putpixel((x * 2, y * 2), pixels[0])
            output.putpixel((x * 2 + 1, y * 2), pixels[1])
            output.putpixel((x * 2, y * 2 + 1), pixels[2])
            output.putpixel((x * 2 + 1, y * 2 + 1), pixels[3])
    return output


def upscale_comparison(tiles: list[Tile], destination: Path, columns: int = 6) -> None:
    if not tiles:
        return
    cell_width = UPSCALED_SIZE * 2 + 12
    cell_height = UPSCALED_SIZE + 12
    rows = (len(tiles) + columns - 1) // columns
    sheet = Image.new("RGB", (columns * cell_width, rows * cell_height), (40, 43, 45))
    draw = ImageDraw.Draw(sheet)
    for index, tile in enumerate(tiles):
        x = (index % columns) * cell_width
        y = (index // columns) * cell_height
        nearest = tile.image.resize((UPSCALED_SIZE, UPSCALED_SIZE), Image.Resampling.NEAREST)
        enhanced = scale2x(tile.image)
        sheet.paste(nearest.convert("RGB"), (x, y), nearest.getchannel("A"))
        sheet.paste(enhanced.convert("RGB"), (x + UPSCALED_SIZE + 4, y), enhanced.getchannel("A"))
        draw.text((x, y + UPSCALED_SIZE + 1), tile.tile_id, fill=(230, 230, 230))
    sheet.save(destination, optimize=True)


def extract(pack_root: Path, output_root: Path) -> None:
    sources = atlas_sources(pack_root)
    if not sources:
        raise SystemExit(f"no DP main-platform atlases found beneath {pack_root}")

    if output_root.exists():
        shutil.rmtree(output_root)
    output_root.mkdir(parents=True)

    unique: dict[str, Tile] = {}
    atlas_summary = []
    for set_name, atlas_path in sources:
        with Image.open(atlas_path) as source:
            atlas = source.convert("RGBA")
        width, height = atlas.size
        if width % TILE_SIZE or height % TILE_SIZE:
            raise SystemExit(f"{atlas_path} is not aligned to a {TILE_SIZE}px grid")
        columns = width // TILE_SIZE
        rows = height // TILE_SIZE
        atlas_summary.append(
            {
                "set": set_name,
                "source": str(atlas_path),
                "size": [width, height],
                "grid": [columns, rows],
            }
        )
        for grid_y in range(rows):
            for grid_x in range(columns):
                left = grid_x * TILE_SIZE
                top = grid_y * TILE_SIZE
                image = atlas.crop((left, top, left + TILE_SIZE, top + TILE_SIZE))
                category, coverage = classify(image)
                if category == "blank":
                    continue
                digest = hashlib.sha256(image.tobytes()).hexdigest()[:12]
                tile_id = f"dp_{digest}"
                occurrence = {
                    "set": set_name,
                    "atlas": atlas_path.name,
                    "grid": [grid_x, grid_y],
                    "pixel": [left, top],
                }
                if digest in unique:
                    unique[digest].occurrences.append(occurrence)
                    continue
                horizontal, vertical = edge_error(image)
                (
                    colour_count,
                    dominant,
                    mean_luma,
                    luma_stddev,
                    entropy,
                    teal,
                    green,
                    blue,
                    magenta,
                    rust,
                ) = visual_stats(image)
                unique[digest] = Tile(
                    tile_id=tile_id,
                    image=image,
                    classification=category,
                    alpha_coverage=coverage,
                    left_right_error=horizontal,
                    top_bottom_error=vertical,
                    colour_count=colour_count,
                    dominant_colour_fraction=dominant,
                    mean_luma=mean_luma,
                    luma_stddev=luma_stddev,
                    entropy=entropy,
                    teal_fraction=teal,
                    green_fraction=green,
                    blue_fraction=blue,
                    magenta_fraction=magenta,
                    rust_fraction=rust,
                    occurrences=[occurrence],
                )

    tiles = sorted(unique.values(), key=lambda tile: tile.tile_id)
    for tile in tiles:
        originals = output_root / "32" / tile.classification
        nearest_dir = output_root / "64_nearest" / tile.classification
        scale2x_dir = output_root / "64_scale2x" / tile.classification
        originals.mkdir(parents=True, exist_ok=True)
        nearest_dir.mkdir(parents=True, exist_ok=True)
        scale2x_dir.mkdir(parents=True, exist_ok=True)
        tile.image.save(originals / f"{tile.tile_id}.png", optimize=True)
        tile.image.resize(
            (UPSCALED_SIZE, UPSCALED_SIZE), Image.Resampling.NEAREST
        ).save(nearest_dir / f"{tile.tile_id}.png", optimize=True)
        scale2x(tile.image).save(scale2x_dir / f"{tile.tile_id}.png", optimize=True)

    repeat_candidates = sorted(
        (
            tile
            for tile in tiles
            if tile.classification == "opaque"
            and tile.left_right_error <= 18
            and tile.top_bottom_error <= 18
        ),
        key=lambda tile: (tile.left_right_error + tile.top_bottom_error, tile.tile_id),
    )
    opaque = [tile for tile in tiles if tile.classification == "opaque"]
    cutout = [tile for tile in tiles if tile.classification == "cutout"]
    surface_candidates = [
        tile
        for tile in opaque
        if tile.dominant_colour_fraction <= 0.78
        and tile.colour_count >= 4
        and tile.luma_stddev >= 4.0
        and tile.left_right_error <= 30
        and tile.top_bottom_error <= 30
    ]
    surface_candidates.sort(
        key=lambda tile: (
            -(
                tile.entropy
                + math.log2(tile.colour_count)
                + (1.0 - tile.dominant_colour_fraction) * 3.0
                - (tile.left_right_error + tile.top_bottom_error) / 40.0
            ),
            tile.tile_id,
        )
    )
    teal_structure = sorted(
        (tile for tile in surface_candidates if tile.teal_fraction >= 0.04),
        key=lambda tile: (-tile.teal_fraction, -tile.entropy, tile.tile_id),
    )
    signal_tiles = sorted(
        (
            tile
            for tile in surface_candidates
            if tile.green_fraction + tile.blue_fraction + tile.magenta_fraction >= 0.015
        ),
        key=lambda tile: (
            -(tile.green_fraction + tile.blue_fraction + tile.magenta_fraction),
            -tile.entropy,
            tile.tile_id,
        ),
    )
    dark_structure = sorted(
        (
            tile
            for tile in surface_candidates
            if tile.teal_fraction < 0.04
            and tile.green_fraction + tile.blue_fraction + tile.magenta_fraction < 0.015
        ),
        key=lambda tile: (-tile.entropy, tile.left_right_error + tile.top_bottom_error, tile.tile_id),
    )
    cutout_details = sorted(
        (
            tile
            for tile in cutout
            if 0.2 <= tile.alpha_coverage <= 0.92
            and tile.colour_count >= 4
            and tile.luma_stddev >= 5.0
        ),
        key=lambda tile: (-tile.entropy, -tile.alpha_coverage, tile.tile_id),
    )

    curated: list[tuple[str, Tile]] = []
    curated_ids: set[str] = set()

    def add_curated(category: str, candidates: list[Tile], count: int) -> None:
        for tile in candidates:
            if tile.tile_id in curated_ids:
                continue
            curated.append((category, tile))
            curated_ids.add(tile.tile_id)
            if sum(existing == category for existing, _ in curated) == count:
                break

    add_curated("teal_structure", teal_structure, 12)
    add_curated("signal", signal_tiles, 12)
    add_curated("dark_structure", dark_structure, 12)
    add_curated("cutout_detail", cutout_details, 12)

    for category, tile in curated:
        for folder, image in (
            ("32", tile.image),
            (
                "64_nearest",
                tile.image.resize((UPSCALED_SIZE, UPSCALED_SIZE), Image.Resampling.NEAREST),
            ),
            ("64_scale2x", scale2x(tile.image)),
        ):
            destination = output_root / "curated" / folder / category
            destination.mkdir(parents=True, exist_ok=True)
            image.save(destination / f"{tile.tile_id}.png", optimize=True)
    contact_sheet(opaque, output_root / "contact_opaque.png")
    contact_sheet(cutout, output_root / "contact_cutout.png")
    contact_sheet(repeat_candidates, output_root / "contact_repeat_candidates.png")
    contact_sheet(surface_candidates[:120], output_root / "contact_surface_candidates.png")
    contact_sheet(teal_structure[:72], output_root / "contact_teal_structure.png")
    contact_sheet(signal_tiles[:72], output_root / "contact_signal_tiles.png")
    contact_sheet(dark_structure[:72], output_root / "contact_dark_structure.png")
    upscale_comparison(surface_candidates[:48], output_root / "contact_upscale_compare.png")
    contact_sheet([tile for _, tile in curated], output_root / "contact_curated.png")
    upscale_comparison(
        [tile for _, tile in curated], output_root / "contact_curated_compare.png"
    )

    manifest = {
        "tile_size": TILE_SIZE,
        "upscaled_size": UPSCALED_SIZE,
        "upscale_filters": ["nearest", "scale2x_epx"],
        "atlases": atlas_summary,
        "unique_nonblank_tiles": len(tiles),
        "opaque_tiles": len(opaque),
        "cutout_tiles": len(cutout),
        "repeat_candidates": len(repeat_candidates),
        "surface_candidates": len(surface_candidates),
        "teal_structure_candidates": len(teal_structure),
        "signal_candidates": len(signal_tiles),
        "dark_structure_candidates": len(dark_structure),
        "curated_tiles": [
            {"category": category, "id": tile.tile_id} for category, tile in curated
        ],
        "tiles": [
            {
                "id": tile.tile_id,
                "classification": tile.classification,
                "alpha_coverage": round(tile.alpha_coverage, 4),
                "edge_error": {
                    "left_right": round(tile.left_right_error, 2),
                    "top_bottom": round(tile.top_bottom_error, 2),
                },
                "visual": {
                    "colour_count": tile.colour_count,
                    "dominant_colour_fraction": round(tile.dominant_colour_fraction, 4),
                    "mean_luma": round(tile.mean_luma, 2),
                    "luma_stddev": round(tile.luma_stddev, 2),
                    "entropy": round(tile.entropy, 3),
                    "accent_fractions": {
                        "teal": round(tile.teal_fraction, 4),
                        "green": round(tile.green_fraction, 4),
                        "blue": round(tile.blue_fraction, 4),
                        "magenta": round(tile.magenta_fraction, 4),
                        "rust": round(tile.rust_fraction, 4),
                    },
                },
                "occurrences": tile.occurrences,
            }
            for tile in tiles
        ],
    }
    (output_root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"{len(sources)} atlases -> {len(tiles)} unique nonblank tiles: "
        f"{len(opaque)} opaque, {len(cutout)} cutout, "
        f"{len(repeat_candidates)} low-seam repeat candidates, "
        f"{len(surface_candidates)} surface candidates"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("pack_root", type=Path)
    parser.add_argument("output_root", type=Path)
    args = parser.parse_args()
    extract(args.pack_root.resolve(), args.output_root.resolve())


if __name__ == "__main__":
    main()
