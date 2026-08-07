#!/usr/bin/env python3
"""Recover PX7/PX8 capture pages and probe payloads from a console recording.

The QR pages are photographed off a TV, so a still frame is only readable if it
happens to land between the capture card's scaling and interlacing artifacts.
This scans every frame of a recording, tries several renderings of each, and
keeps any page whose CRC checks out.

    python3 tools/hwtest-video-qr.py capture.mov pages.txt

Pages are grouped by RUN. A recording usually contains more than one pass (a
reboot, or RERUN STARTUP TESTS), and those pass different measurements under the
same page numbers, so combining a page 1 from one run with a page 3 from another
produces a payload that fails its binary CRC. Every distinct chunk seen for a
page is kept, and the combination satisfying the whole-binary CRC is written.

Targeted-probe QRs (PA1-PA5, SB1-SB4) seen in the same recording are written as
sidecar files next to the output: pages-pa5.txt, pages-sb4.txt, and so on, one
payload line each, ready for tools/hwtest-audio-report.py /
tools/hwtest-sb4-report.py. A probe payload is accepted when its decoded
binary's trailing CRC32 checks out (the wire text's /C: field is not the chunk
CRC for probes, so the binary self-check is the validation that matters).

Install zxing-cpp (`pip install zxing-cpp`). OpenCV's detector is the fallback
and is markedly weaker on a photographed CRT: on one console recording it read
3 of 5 pages after minutes of preprocessing, while zxing read all 5 in twenty
seconds from raw frames.

Known limit: SB2's symbol is denser than the rest and has never decoded from a
640x480 capture; use its TTY mirror or give it a paged transport before relying
on video for SB2.
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
PROBE_PREFIXES = tuple(f"PA{n}" for n in range(1, 6)) + tuple(f"SB{n}" for n in range(1, 5))


def probe_payload_valid(chunk: str) -> bool:
    """A probe payload is valid when its decoded binary's trailing CRC32
    matches the rest of the binary. The /C: field on the wire is not the
    chunk CRC for probes, so this is the check that matters."""
    try:
        binary = base64.b64decode(chunk + "=" * (-len(chunk) % 4))
    except binascii.Error:
        return False
    if len(binary) < 8:
        return False
    claimed = int.from_bytes(binary[-4:], "little")
    return claimed == (binascii.crc32(binary[:-4]) & 0xFFFF_FFFF)


def scan(
    video: pathlib.Path, verbose: bool = True
) -> tuple[dict[int, set[str]], int | None, str, dict[str, set[str]]]:
    capture = cv2.VideoCapture(str(video))
    seen: dict[int, set[str]] = {}
    probes: dict[str, set[str]] = {}
    total_pages: int | None = None
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
            if prefix in PROBE_PREFIXES and "/C:" in data:
                chunk = data.rsplit("/C:", 1)[0].split("/", 1)[1]
                if probe_payload_valid(chunk):
                    bucket = probes.setdefault(prefix, set())
                    if data not in bucket:
                        bucket.add(data)
                        if verbose:
                            print(f"probe {prefix} at frame {frame_no}", flush=True)
                continue
            if prefix not in PAGE_SCHEMAS:
                continue
            if schema and prefix != schema:
                if verbose:
                    print(
                        f"# skipping {prefix} page at frame {frame_no}: "
                        f"recording already yielded {schema} pages",
                        flush=True,
                    )
                continue
            body, claimed = data.rsplit("/C:", 1)
            _, page_field, chunk = body.split("/", 2)
            number, total = int(page_field[:2], 16), int(page_field[2:], 16)
            if int(claimed, 16) != (binascii.crc32(chunk.encode()) & 0xFFFF_FFFF):
                continue
            schema = prefix
            total_pages = total
            bucket = seen.setdefault(number, set())
            if chunk not in bucket:
                bucket.add(chunk)
                if verbose:
                    print(f"{schema} page {number}/{total} at frame {frame_no}", flush=True)

    capture.release()
    if verbose:
        print(f"# frames scanned: {frame_no}")
    return seen, total_pages, schema, probes


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


def write_probe_files(out: pathlib.Path, probes: dict[str, set[str]]) -> None:
    for prefix, payloads in sorted(probes.items()):
        sidecar = out.with_name(f"{out.stem}-{prefix.lower()}{out.suffix or '.txt'}")
        sidecar.write_text("\n".join(sorted(payloads)) + "\n")
        extra = "" if len(payloads) == 1 else f" ({len(payloads)} distinct payloads)"
        print(f"# probe {prefix} -> {sidecar}{extra}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("video", help="recording of the console showing the capture pages")
    parser.add_argument("out", help="write capture page lines here (probe sidecars go next to it)")
    args = parser.parse_args()

    out = pathlib.Path(args.out)
    seen, total_pages, schema, probes = scan(pathlib.Path(args.video))
    write_probe_files(out, probes)
    if not seen or total_pages is None:
        print("FAIL: no PX7/PX8 page decoded from any frame", file=sys.stderr)
        return 1

    missing = [n for n in range(1, total_pages + 1) if n not in seen]
    if missing:
        print(
            f"FAIL: recovered {sorted(seen)} of {total_pages}; missing {missing}.\n"
            "      Some symbols do not survive the capture chain. Use the audio\n"
            "      readout (tools/hwtest-audio-decode.py) for a complete payload.",
            file=sys.stderr,
        )
        return 1

    chosen = combine(seen, total_pages)
    if chosen is None:
        print(
            "FAIL: every page decoded, but no combination satisfies the payload\n"
            "      CRC. Either the recording spans several runs with no single\n"
            "      run showing all pages, or it was made with a disc older than\n"
            "      HWTEST v1.4, which rebuilt the payload on every page change so\n"
            "      the pages never described one consistent capture.",
            file=sys.stderr,
        )
        return 1

    lines = [
        f"{schema}/{n:02X}{total_pages:02X}/{chunk}/C:"
        f"{binascii.crc32(chunk.encode()) & 0xFFFF_FFFF:08X}"
        for n, chunk in enumerate(chosen, start=1)
    ]
    out.write_text("\n".join(lines) + "\n")
    print(f"# recovered all {total_pages} {schema} pages -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
