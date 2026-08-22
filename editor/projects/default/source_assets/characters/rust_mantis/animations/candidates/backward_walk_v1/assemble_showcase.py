"""Assemble the four Rust Mantis backward-walk auditions into one grid."""

from __future__ import annotations

import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


HERE = Path(__file__).resolve().parent
RENDERS = HERE / "renders"
ASSEMBLED = HERE / "assembled"
PREVIEWS = HERE.parents[1] / "previews"
SHOWCASE = PREVIEWS / "rust_mantis_backward_walk_showcase_v1.mp4"
POSTER = PREVIEWS / "rust_mantis_backward_walk_showcase_v1_poster.png"
FONT = Path("/System/Library/Fonts/SFNS.ttf")
BOLD = Path("/System/Library/Fonts/Supplemental/Arial Bold.ttf")
SIZE = 1024


def run(*args: str) -> None:
    subprocess.run(args, check=True)


def font(path: Path, size: int):
    return ImageFont.truetype(str(path), size)


def centered(draw, xy, label, face, fill) -> None:
    box = draw.textbbox((0, 0), label, font=face)
    draw.text((xy[0] - (box[2] - box[0]) / 2, xy[1]), label, font=face, fill=fill)


def make_overlay(path: Path) -> None:
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image, "RGBA")
    draw.rectangle((0, 0, SIZE, 66), fill=(7, 10, 16, 225))
    centered(
        draw,
        (SIZE / 2, 11),
        "BACKWARD WALK - CHOOSE 1, 2, 3, OR 4",
        font(BOLD, 35),
        (245, 247, 250, 255),
    )
    draw.rectangle((508, 66, 516, SIZE), fill=(0, 0, 0, 190))
    draw.rectangle((0, 508, SIZE, 516), fill=(0, 0, 0, 190))
    positions = [(16, 78), (528, 78), (16, 528), (528, 528)]
    for number, (x, y) in enumerate(positions, 1):
        draw.rounded_rectangle(
            (x, y, x + 202, y + 50),
            radius=10,
            fill=(6, 8, 12, 220),
            outline=(185, 78, 38, 255),
            width=3,
        )
        draw.text(
            (x + 14, y + 7),
            f"BACKWARD {number}",
            font=font(BOLD, 25),
            fill=(255, 255, 255, 255),
        )
    image.save(path)


def make_intro(path: Path) -> None:
    image = Image.new("RGB", (SIZE, SIZE), (8, 11, 17))
    draw = ImageDraw.Draw(image)
    draw.rectangle((0, 0, 18, SIZE), fill=(151, 62, 30))
    draw.rectangle((1006, 0, SIZE, SIZE), fill=(151, 62, 30))
    centered(draw, (SIZE / 2, 315), "RUST MANTIS", font(BOLD, 70), (245, 247, 250))
    centered(draw, (SIZE / 2, 410), "BACKWARD WALK AUDITIONS", font(BOLD, 42), (204, 210, 220))
    centered(draw, (SIZE / 2, 525), "4 LOCAL AI MOTION CANDIDATES", font(BOLD, 28), (232, 235, 240))
    centered(draw, (SIZE / 2, 590), "Reply with 1, 2, 3, or 4", font(FONT, 30), (173, 181, 194))
    image.save(path)


ASSEMBLED.mkdir(parents=True, exist_ok=True)
PREVIEWS.mkdir(parents=True, exist_ok=True)
overlay = ASSEMBLED / "overlay.png"
intro_png = ASSEMBLED / "intro.png"
intro_mp4 = ASSEMBLED / "intro.mp4"
grid = ASSEMBLED / "backward_grid.mp4"
make_overlay(overlay)
make_intro(intro_png)

run(
    "ffmpeg", "-y", "-loop", "1", "-framerate", "30", "-i", str(intro_png),
    "-t", "2.0", "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18",
    "-movflags", "+faststart", str(intro_mp4),
)

inputs = []
for number in range(1, 5):
    inputs += ["-i", str(RENDERS / f"backward_{number:02d}.mp4")]
inputs += ["-loop", "1", "-framerate", "30", "-i", str(overlay)]
filters = [f"[{index}:v]fps=30,scale=512:512[v{index}]" for index in range(4)]
filters += [
    "[v0][v1][v2][v3]xstack=inputs=4:layout=0_0|512_0|0_512|512_512:fill=black[grid]",
    "[grid][4:v]overlay=0:0:shortest=1,format=yuv420p[out]",
]
run(
    "ffmpeg", "-y", *inputs,
    "-filter_complex", ";".join(filters),
    "-map", "[out]", "-an", "-r", "30", "-c:v", "libx264", "-crf", "18",
    "-preset", "medium", "-movflags", "+faststart", str(grid),
)

run(
    "ffmpeg", "-y", "-i", str(intro_mp4), "-i", str(grid),
    "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0,format=yuv420p[out]",
    "-map", "[out]", "-an", "-r", "30", "-c:v", "libx264", "-crf", "18",
    "-preset", "medium", "-movflags", "+faststart", str(SHOWCASE),
)

run(
    "ffmpeg", "-y", "-ss", "3.0", "-i", str(grid), "-frames:v", "1", "-update", "1", str(POSTER)
)
print("MANTIS_BACKWARD_SHOWCASE", SHOWCASE)
print("MANTIS_BACKWARD_SHOWCASE_POSTER", POSTER)
