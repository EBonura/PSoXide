#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""Build the synthetic STR movie the hello-fmv console test streams.

Everything is generated: an FFmpeg test pattern with temporal noise on top
(so every frame fills the whole 2x sector budget) and a stereo beep track
(left 440 Hz, right 660 Hz, 100 ms at the start of every second, in step
with the pattern's seconds counter). psxavenc encodes it as a 320x240,
15 fps, BS v2 STR with interleaved 37.8 kHz stereo XA-ADPCM, 2336-byte
sectors, XA file 1 / channel 0.

Then every video sector's STR header gets test fields in bytes 20..32 of
its 2048-byte payload (over the BS header copy, which players do not
need):

    20..22  video sector ordinal (0-based, audio sectors skipped)
    22..24  total video sectors in the file
    24..28  sector index within the file (audio sectors counted)
    28..32  checksum of payload bytes 32..2048

The checksum is `h = rotl(h, 5) + w` over the 504 little-endian words,
seeded with `0x9E3779B9 ^ ordinal`; hello-fmv recomputes it for every
sector it reads, so a corrupt, shifted or misplaced PIO read shows up as
BAD and a skipped one as LOST.

Usage:
    fmv_test_movie.py --psxavenc PATH --out MOVIE.STR [--seconds 75]
Writes MOVIE.STR (2336-byte sectors, for `mkisopsx --xa-file`) and
MOVIE.STR.json with the counts.
"""

import argparse
import json
import pathlib
import struct
import subprocess
import tempfile

XA_SECTOR = 2336
SEED = 0x9E3779B9


def checksum(payload: bytes, ordinal: int) -> int:
    h = (SEED ^ ordinal) & 0xFFFFFFFF
    for (w,) in struct.iter_unpack("<I", payload[32:2048]):
        h = (((h << 5) | (h >> 27)) + w) & 0xFFFFFFFF
    return h


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--psxavenc", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--seconds", type=int, default=75)
    ap.add_argument("--noise", type=int, default=40, help="FFmpeg noise strength")
    args = ap.parse_args()

    out = pathlib.Path(args.out)
    with tempfile.TemporaryDirectory(dir=out.parent) as tmp:
        src = pathlib.Path(tmp) / "src.mkv"
        raw = pathlib.Path(tmp) / "raw.str"
        beep = "0.5*sin(2*PI*{f}*t)*lt(mod(t\\,1)\\,0.1)"
        subprocess.run(
            [
                "ffmpeg", "-v", "error", "-y",
                "-f", "lavfi", "-i",
                f"testsrc=size=320x240:rate=15:duration={args.seconds},"
                f"noise=alls={args.noise}:allf=t+u",
                "-f", "lavfi", "-i",
                f"aevalsrc={beep.format(f=440)}|{beep.format(f=660)}"
                f":s=37800:d={args.seconds}",
                "-c:v", "ffv1", "-c:a", "pcm_s16le", "-shortest", str(src),
            ],
            check=True,
        )
        subprocess.run(
            [
                args.psxavenc, "-q", "-t", "str", "-v", "v2",
                "-s", "320x240", "-r", "15", "-x", "2",
                "-f", "37800", "-c", "2", "-b", "4", "-F", "1", "-C", "0",
                str(src), str(raw),
            ],
            check=True,
        )
        data = bytearray(raw.read_bytes())

    assert len(data) % XA_SECTOR == 0, "psxavenc -t str writes 2336-byte sectors"
    sectors = len(data) // XA_SECTOR

    def is_video(i: int) -> bool:
        s = data[i * XA_SECTOR : (i + 1) * XA_SECTOR]
        return s[2] & 0x04 == 0 and s[8:12] == b"\x60\x01\x01\x80"

    video = [i for i in range(sectors) if is_video(i)]
    audio = sum(1 for i in range(sectors) if data[i * XA_SECTOR + 2] & 0x04)
    frames = set()
    for ordinal, i in enumerate(video):
        base = i * XA_SECTOR + 8
        payload = data[base : base + 2048]
        frames.add(struct.unpack_from("<I", payload, 8)[0])
        struct.pack_into("<HHI", payload, 20, ordinal, len(video), i)
        struct.pack_into("<I", payload, 28, checksum(bytes(payload), ordinal))
        data[base : base + 2048] = payload
    out.write_bytes(data)
    info = {
        "sectors": sectors,
        "video_sectors": len(video),
        "audio_sectors": audio,
        "frames": len(frames),
        "seconds": args.seconds,
    }
    pathlib.Path(str(out) + ".json").write_text(json.dumps(info, indent=2) + "\n")
    print(json.dumps(info))


if __name__ == "__main__":
    main()
