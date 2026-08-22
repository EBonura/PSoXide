"""Assemble labelled 2x2 category grids into the Tank Boss audition reel."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


SCRIPT_DIR = Path(__file__).resolve().parent
HERE = Path(os.environ.get("CANDIDATE_ROOT", str(SCRIPT_DIR)))
RENDERS = HERE / "renders"
ASSEMBLED = HERE / "assembled"
PREVIEWS = Path(os.environ.get("PREVIEW_DIR", str(HERE.parents[1] / "previews")))
SHOWCASE_STEM = os.environ.get("SHOWCASE_STEM", "tank_boss_core_animation_showcase_v1")
SHOWCASE = PREVIEWS / f"{SHOWCASE_STEM}.mp4"
POSTER = PREVIEWS / f"{SHOWCASE_STEM}_poster.png"
CATEGORIES = [
    item.strip()
    for item in os.environ.get("SHOWCASE_CATEGORIES", "idle,attack,hit,death").split(",")
    if item.strip()
]
SUBTITLE = os.environ.get("SHOWCASE_SUBTITLE", "AI ANIMATION AUDITIONS")
FONT = Path("/System/Library/Fonts/SFNS.ttf")
BOLD = Path("/System/Library/Fonts/Supplemental/Arial Bold.ttf")
SIZE = 1024


def run(*args: str):
    subprocess.run(args, check=True)


def font(path: Path, size: int):
    return ImageFont.truetype(str(path), size)


def centered(draw, xy, text, face, fill):
    box = draw.textbbox((0, 0), text, font=face)
    width = box[2] - box[0]
    draw.text((xy[0] - width / 2, xy[1]), text, font=face, fill=fill)


def overlay(category: str, path: Path):
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image, "RGBA")
    draw.rectangle((0, 0, SIZE, 66), fill=(7, 10, 16, 225))
    centered(draw, (SIZE / 2, 11), f"{category.upper()} - CHOOSE 1, 2, 3, OR 4", font(BOLD, 38), (245, 247, 250, 255))
    draw.rectangle((508, 66, 516, SIZE), fill=(0, 0, 0, 190))
    draw.rectangle((0, 508, SIZE, 516), fill=(0, 0, 0, 190))
    positions = [(16, 78), (528, 78), (16, 528), (528, 528)]
    for number, (x, y) in enumerate(positions, 1):
        draw.rounded_rectangle((x, y, x + 154, y + 50), radius=10, fill=(6, 8, 12, 220), outline=(192, 43, 48, 255), width=3)
        draw.text((x + 16, y + 6), f"{category.upper()} {number}", font=font(BOLD, 27), fill=(255, 255, 255, 255))
    image.save(path)


def intro(path: Path):
    image = Image.new("RGB", (SIZE, SIZE), (8, 11, 17))
    draw = ImageDraw.Draw(image)
    draw.rectangle((0, 0, 18, SIZE), fill=(162, 32, 39))
    draw.rectangle((1006, 0, SIZE, SIZE), fill=(162, 32, 39))
    centered(draw, (SIZE / 2, 320), "TANK BOSS", font(BOLD, 72), (245, 247, 250))
    centered(draw, (SIZE / 2, 410), SUBTITLE, font(BOLD, 44), (204, 210, 220))
    labels = {"idle": "IDLES", "attack": "ATTACKS", "hit": "HIT REACTIONS", "death": "DEATHS"}
    summary = "  •  ".join(f"4 {labels.get(category, category.upper())}" for category in CATEGORIES)
    centered(draw, (SIZE / 2, 525), summary, font(BOLD, 27), (232, 235, 240))
    centered(draw, (SIZE / 2, 590), "Reply with one number per category", font(FONT, 30), (173, 181, 194))
    image.save(path)


def category_grid(category: str, effect: str) -> Path:
    overlay_path = ASSEMBLED / f"{category}_overlay.png"
    overlay(category, overlay_path)
    output = ASSEMBLED / f"{category}_grid.mp4"
    inputs = []
    for number in range(1, 5):
        inputs += ["-i", str(RENDERS / f"{category}_{number:02d}.mp4")]
    inputs += ["-loop", "1", "-framerate", "30", "-i", str(overlay_path)]
    filters = []
    for index in range(4):
        filters.append(f"[{index}:v]fps=30,scale=512:512,{effect}[v{index}]")
    filters.append("[v0][v1][v2][v3]xstack=inputs=4:layout=0_0|512_0|0_512|512_512:fill=black[grid]")
    filters.append("[grid][4:v]overlay=0:0:shortest=1,format=yuv420p[out]")
    run(
        "ffmpeg", "-y", *inputs,
        "-filter_complex", ";".join(filters),
        "-map", "[out]", "-an", "-r", "30",
        "-c:v", "libx264", "-crf", "18", "-preset", "medium",
        "-movflags", "+faststart", str(output),
    )
    return output


ASSEMBLED.mkdir(parents=True, exist_ok=True)
PREVIEWS.mkdir(parents=True, exist_ok=True)
intro_png = ASSEMBLED / "intro.png"
intro(intro_png)
intro_mp4 = ASSEMBLED / "intro.mp4"
run(
    "ffmpeg", "-y", "-loop", "1", "-framerate", "30", "-i", str(intro_png),
    "-t", "2.2", "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p",
    "-crf", "18", "-movflags", "+faststart", str(intro_mp4),
)

effects = {
    "idle": "tpad=start_duration=0.35:stop_mode=clone:stop_duration=0.65",
    "attack": "tpad=start_mode=clone:start_duration=0.65:stop_mode=clone:stop_duration=1.15",
    "hit": "setpts=1.25*PTS,tpad=start_mode=clone:start_duration=0.6:stop_mode=clone:stop_duration=1.0",
    "death": "tpad=start_mode=clone:start_duration=0.65:stop_mode=clone:stop_duration=1.35",
}
grids = [category_grid(category, effects[category]) for category in CATEGORIES]

inputs = [item for path in [intro_mp4, *grids] for item in ("-i", str(path))]
stream_count = len(grids) + 1
streams = "".join(f"[{index}:v]" for index in range(stream_count))
run(
    "ffmpeg", "-y", *inputs,
    "-filter_complex", f"{streams}concat=n={stream_count}:v=1:a=0,format=yuv420p[out]",
    "-map", "[out]", "-an", "-r", "30", "-c:v", "libx264", "-crf", "18",
    "-preset", "medium", "-movflags", "+faststart", str(SHOWCASE),
)

# Poster: one representative frame from every category grid.
poster_times = {"idle": "1.0", "attack": "1.8", "hit": "1.2", "death": "2.1"}
poster_inputs = [
    item
    for category, path in zip(CATEGORIES, grids)
    for item in ("-ss", poster_times[category], "-i", str(path))
]
poster_filters = [f"[{index}:v]scale=512:512[v{index}]" for index in range(len(grids))]
if len(grids) == 2:
    poster_filters.append("[v0][v1]xstack=inputs=2:layout=0_0|512_0[out]")
elif len(grids) == 4:
    poster_filters.append("[v0][v1][v2][v3]xstack=inputs=4:layout=0_0|512_0|0_512|512_512[out]")
else:
    raise RuntimeError("Poster layout supports two or four categories")
run(
    "ffmpeg", "-y", *poster_inputs,
    "-filter_complex", ";".join(poster_filters),
    "-map", "[out]", "-frames:v", "1", "-update", "1", str(POSTER),
)
print("TANK_BOSS_SHOWCASE", SHOWCASE)
print("TANK_BOSS_SHOWCASE_POSTER", POSTER)
