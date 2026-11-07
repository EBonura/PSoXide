#!/usr/bin/env python3
"""Recover the PX8 capture pages from a console recording.

The QR pages are photographed off a TV, so a still frame is only readable if it
happens to land between the capture card's scaling and interlacing artifacts.
This scans every frame of a recording, tries several renderings of each, and
keeps any page whose CRC checks out.

    python3 tools/hwtest-video-qr.py capture.mov pages.txt

v2.0 shows one capture per run: a cover page ("FILM FROM HERE, n PAGES" and a
run id), then the pages in turn, forever. Every page carries the run id, so a
recording that spans several runs is sorted by run before anything is
combined; the binary CRC then confirms the set. `tools/hwtest-report.py
--compare recording.mov` does this and the diff against the silicon baselines
in one go.

Install zxing-cpp (`pip install zxing-cpp`). OpenCV's detector is the fallback
and is markedly weaker on a photographed CRT: on one console recording it read
3 of 5 pages after minutes of preprocessing, while zxing read all 5 in twenty
seconds from raw frames.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import itertools
import pathlib
import sys

import cv2
import numpy as np

try:
    import zxingcpp
except ImportError:  # pragma: no cover
    zxingcpp = None


def read_symbols(gray: np.ndarray) -> list[str]:
    """Decode every QR in a frame.

    zxing-cpp when available, because OpenCV's detector is markedly weaker on
    a photographed CRT: on one console recording it read 3 of 5 pages after
    four minutes of preprocessing variants, while zxing read all 5 in twenty
    seconds from the raw frames.
    """
    if zxingcpp is not None:
        found = [r.text for r in zxingcpp.read_barcodes(gray)]
        if found:
            return found
        big = cv2.resize(gray, None, fx=2, fy=2, interpolation=cv2.INTER_NEAREST)
        return [r.text for r in zxingcpp.read_barcodes(big)]
    detector = cv2.QRCodeDetector()
    out = []
    for image in renderings(gray):
        try:
            data, _, _ = detector.detectAndDecode(image)
        except cv2.error:
            continue
        if data:
            out.append(data)
            break
    return out


def renderings(gray: np.ndarray):
    """Several renderings of one frame.

    Which preprocessing recovers a symbol varies frame to frame, because the
    capture chain's softening is not uniform, so a few cheap attempts beat one
    clever one.
    """
    for scale in (2, 3, 4):
        big = cv2.resize(gray, None, fx=scale, fy=scale, interpolation=cv2.INTER_NEAREST)
        yield big
        yield cv2.threshold(big, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)[1]
        # Unsharp: scaling softens module edges and the detector needs the
        # transitions back.
        blur = cv2.GaussianBlur(big, (0, 0), 2)
        sharp = cv2.addWeighted(big, 1.8, blur, -0.8, 0)
        yield cv2.threshold(sharp, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)[1]
    # One interlace field only: merging fields smears fine module edges.
    for offset in (0, 1):
        field = gray[offset::2, :]
        big = cv2.resize(field, None, fx=3, fy=6, interpolation=cv2.INTER_NEAREST)
        yield cv2.threshold(big, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)[1]


PAGE_SCHEMAS = ("PX7", "PX8")


def scan(
    video: pathlib.Path, verbose: bool = True
) -> tuple[dict[int | None, dict[int, set[str]]], dict[int | None, int], str]:
    """Pages by run id, then page number; each run's page total; the schema."""
    capture = cv2.VideoCapture(str(video))
    runs: dict[int | None, dict[int, set[str]]] = {}
    totals: dict[int | None, int] = {}
    schema = ""
    frame_no = 0

    while True:
        ok, frame = capture.read()
        if not ok:
            break
        frame_no += 1
        gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
        # A capture page is mostly a large bright block; skip dark frames rather
        # than paying for preprocessing on the progress bar or the menu.
        if gray.mean() < 25:
            continue
        for data in read_symbols(gray):
            prefix = data.split("/", 1)[0]
            if prefix not in PAGE_SCHEMAS or "/C:" not in data:
                continue
            if schema and prefix != schema:
                continue
            body, claimed = data.rsplit("/C:", 1)
            try:
                _, page_field, chunk = body.split("/", 2)
                number, total = int(page_field[:2], 16), int(page_field[2:4], 16)
                run_id = int(page_field[4:], 16) if len(page_field) == 8 else None
                crc_ok = int(claimed, 16) == (binascii.crc32(chunk.encode()) & 0xFFFF_FFFF)
            except ValueError:
                continue
            if not crc_ok:
                continue
            schema = prefix
            totals[run_id] = total
            bucket = runs.setdefault(run_id, {}).setdefault(number, set())
            if chunk not in bucket:
                bucket.add(chunk)
                if verbose:
                    label = "" if run_id is None else f" run {run_id:04X}"
                    print(f"{schema} page {number}/{total}{label} at frame {frame_no}", flush=True)

    capture.release()
    if verbose:
        print(f"# frames scanned: {frame_no}")
    return runs, totals, schema


def combine(seen: dict[int, set[str]], total_pages: int) -> list[str] | None:
    """Pick one chunk per page such that the whole payload's CRC checks out.

    This is what separates runs: a mismatched set decodes to a binary whose
    trailing CRC does not match its own contents.
    """
    if sorted(seen) != list(range(1, total_pages + 1)):
        return None
    for combo in itertools.product(*(sorted(seen[n]) for n in range(1, total_pages + 1))):
        try:
            binary = base64.b64decode("".join(combo), validate=True)
        except binascii.Error:
            continue
        if len(binary) < 8:
            continue
        claimed = int.from_bytes(binary[-4:], "little")
        if claimed == (binascii.crc32(binary[:-4]) & 0xFFFF_FFFF):
            return list(combo)
    return None


def pages_from_video(video: pathlib.Path, verbose: bool = False) -> list[str]:
    """The page lines of the most complete run in a recording; raises
    ValueError when no run has all its pages with a matching binary CRC."""
    runs, totals, schema = scan(video, verbose)
    if not runs:
        raise ValueError(f"no PX7/PX8 page decoded from any frame of {video}")
    problems = []
    # Complete runs first, newest-looking last: a recording usually ends on
    # the run that was being filmed.
    for run_id, seen in sorted(runs.items(), key=lambda item: -len(item[1])):
        total = totals[run_id]
        missing = [n for n in range(1, total + 1) if n not in seen]
        label = "run ?" if run_id is None else f"run {run_id:04X}"
        if missing:
            problems.append(f"{label}: recovered {sorted(seen)} of {total}; missing {missing}")
            continue
        chosen = combine(seen, total)
        if chosen is None:
            problems.append(f"{label}: every page decoded but the binary CRC does not check out")
            continue
        return [
            f"{schema}/{n:02X}{total:02X}"
            + ("" if run_id is None else f"{run_id:04X}")
            + f"/{chunk}/C:{binascii.crc32(chunk.encode()) & 0xFFFF_FFFF:08X}"
            for n, chunk in enumerate(chosen, start=1)
        ]
    raise ValueError("; ".join(problems))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("video", help="recording of the console showing the capture pages")
    parser.add_argument("out", help="write the capture page lines here")
    args = parser.parse_args()
    try:
        lines = pages_from_video(pathlib.Path(args.video), verbose=True)
    except ValueError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    pathlib.Path(args.out).write_text("\n".join(lines) + "\n")
    print(f"# recovered all {len(lines)} pages -> {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
