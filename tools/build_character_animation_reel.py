#!/usr/bin/env python3
"""Label and encode frames from the psxed-ui character reel renderer.

Usage:
  python tools/build_character_animation_reel.py \
    /tmp/psoxide-character-reel editor/projects/default/source_assets/characters/previews/all_character_animations_reel.mp4
"""

from __future__ import annotations

import csv
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


WIDTH = 960
HEIGHT = 720
FPS = 30
TITLE_SECONDS = 1.8
FONT_REGULAR = Path("/System/Library/Fonts/Supplemental/Arial.ttf")
FONT_BOLD = Path("/System/Library/Fonts/Supplemental/Arial Bold.ttf")


def run(*args: str) -> None:
    subprocess.run(args, check=True)


def display_name(value: str) -> str:
    return value.replace("_", " ").replace("  ", " ").strip()


def group_title(character: str) -> tuple[str, str]:
    if character == "Aletha":
        return "PLAYER", "Aletha"
    if character == "Light Enemy":
        return "LIGHT ENEMY", "Light Enemy"
    return "HEAVY ENEMY", "Heavy Enemy"


def title_card(path: Path, eyebrow: str, title: str, count: int) -> None:
    image = Image.new("RGB", (WIDTH, HEIGHT), (7, 10, 16))
    draw = ImageDraw.Draw(image)
    for x in range(0, WIDTH, 48):
        draw.line((x, 0, x, HEIGHT), fill=(16, 28, 38), width=1)
    for y in range(0, HEIGHT, 48):
        draw.line((0, y, WIDTH, y), fill=(16, 28, 38), width=1)
    draw.rectangle((86, 190, 98, 522), fill=(46, 226, 174))
    eyebrow_font = ImageFont.truetype(str(FONT_BOLD), 27)
    title_font = ImageFont.truetype(str(FONT_BOLD), 62)
    detail_font = ImageFont.truetype(str(FONT_REGULAR), 25)
    draw.text((132, 220), eyebrow, font=eyebrow_font, fill=(46, 226, 174))
    draw.text((128, 270), title, font=title_font, fill=(236, 241, 246))
    draw.text(
        (132, 374),
        f"{count} cooked animation{'s' if count != 1 else ''}",
        font=detail_font,
        fill=(150, 165, 180),
    )
    draw.text(
        (132, 424),
        "Engine preview • projectiles disabled",
        font=detail_font,
        fill=(106, 128, 145),
    )
    image.save(path)


def label_overlay(path: Path, row: dict[str, str], ordinal: int, total: int) -> None:
    image = Image.new("RGBA", (WIDTH, HEIGHT), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rectangle((0, 0, WIDTH, 112), fill=(3, 6, 10, 220))
    eyebrow, character = group_title(row["character"])
    eyebrow_font = ImageFont.truetype(str(FONT_BOLD), 20)
    clip_font = ImageFont.truetype(str(FONT_BOLD), 31)
    meta_font = ImageFont.truetype(str(FONT_REGULAR), 18)
    draw.text((24, 14), f"{eyebrow} / {character}", font=eyebrow_font, fill=(46, 226, 174, 255))
    clip = display_name(row["clip"])
    draw.text((22, 43), clip, font=clip_font, fill=(242, 246, 250, 255))
    meta = (
        f"{row['role']}  •  {row['stored_frames']} frames @ {row['sample_rate']} Hz"
        f"  •  {'loop' if row['looping'] == 'true' else 'one-shot'}"
    )
    draw.text((24, 83), meta, font=meta_font, fill=(151, 168, 183, 255))
    counter = f"{ordinal:02}/{total:02}"
    counter_box = draw.textbbox((0, 0), counter, font=eyebrow_font)
    draw.text(
        (WIDTH - (counter_box[2] - counter_box[0]) - 24, 16),
        counter,
        font=eyebrow_font,
        fill=(151, 168, 183, 255),
    )
    if row["clip"] == "gen_light_attack":
        note_font = ImageFont.truetype(str(FONT_BOLD), 18)
        note = "RUNTIME WEAPON MATERIALISATION"
        note_box = draw.textbbox((0, 0), note, font=note_font)
        x = WIDTH - (note_box[2] - note_box[0]) - 24
        draw.rectangle((x - 10, 76, WIDTH - 14, 105), fill=(8, 38, 29, 235))
        draw.text((x, 80), note, font=note_font, fill=(46, 255, 142, 255))
    image.save(path)


def encode_title(image: Path, output: Path) -> None:
    run(
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-loop",
        "1",
        "-framerate",
        str(FPS),
        "-t",
        str(TITLE_SECONDS),
        "-i",
        str(image),
        "-an",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        str(output),
    )


def encode_clip(frame_dir: Path, overlay: Path, output: Path) -> None:
    run(
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-framerate",
        str(FPS),
        "-i",
        str(frame_dir / "frame_%05d.png"),
        "-loop",
        "1",
        "-i",
        str(overlay),
        "-filter_complex",
        "[0:v]scale=960:720:flags=neighbor[base];[base][1:v]overlay=0:0:shortest=1,format=yuv420p[out]",
        "-map",
        "[out]",
        "-an",
        "-r",
        str(FPS),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        str(output),
    )


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: build_character_animation_reel.py <frame-dir> <output.mp4>")
    frame_root = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2]).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    work = frame_root / "encoded"
    work.mkdir(parents=True, exist_ok=True)
    with (frame_root / "manifest.tsv").open(newline="", encoding="utf-8") as source:
        rows = list(csv.DictReader(source, delimiter="\t"))

    group_counts: dict[str, int] = {}
    for row in rows:
        group_counts[row["character"]] = group_counts.get(row["character"], 0) + 1

    encoded: list[Path] = []
    current_character: str | None = None
    segment = 0
    for ordinal, row in enumerate(rows, start=1):
        character = row["character"]
        if character != current_character:
            eyebrow, title = group_title(character)
            card = work / f"{segment:03}_title.png"
            movie = work / f"{segment:03}_title.mp4"
            title_card(card, eyebrow, title, group_counts[character])
            encode_title(card, movie)
            encoded.append(movie)
            segment += 1
            current_character = character

        overlay = work / f"{segment:03}_overlay.png"
        movie = work / f"{segment:03}_clip.mp4"
        label_overlay(overlay, row, ordinal, len(rows))
        encode_clip(frame_root / row["frame_dir"], overlay, movie)
        encoded.append(movie)
        print(f"[{ordinal:02}/{len(rows):02}] {character} / {row['clip']}", flush=True)
        segment += 1

    concat = work / "concat.txt"
    concat.write_text("".join(f"file '{path.as_posix()}'\n" for path in encoded), encoding="utf-8")
    run(
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        str(concat),
        "-c",
        "copy",
        "-movflags",
        "+faststart",
        str(output),
    )
    print(f"wrote {output}")


if __name__ == "__main__":
    main()
