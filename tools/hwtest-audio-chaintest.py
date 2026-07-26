#!/usr/bin/env python3
"""Test the audio link against simulated capture-card damage.

The emulator's SPU output is a clean digital path, so decoding it proves the
encoding and framing but says nothing about surviving a real capture chain.
That chain resamples, band-limits, applies AGC, adds noise and sometimes clips.
This applies each of those to a known-good recording and checks the decoder
still recovers the payload, which converts "untested until we burn a disc" into
a concrete pass/fail matrix.

    python3 tools/hwtest-audio-chaintest.py build/hwtest-audio.wav

It cannot prove the real chain works. It can prove the decoder is not brittle
against the specific degradations a capture chain is known to introduce, which
is the part that would otherwise cost a burn to discover.
"""

from __future__ import annotations

import argparse
import pathlib
import struct
import subprocess
import sys
import wave

import numpy as np

DECODER = pathlib.Path(__file__).with_name("hwtest-audio-decode.py")


def read_wav(path: pathlib.Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as handle:
        rate = handle.getframerate()
        channels = handle.getnchannels()
        raw = handle.readframes(handle.getnframes())
    values = np.frombuffer(raw, dtype="<i2").astype(np.float64)
    if channels > 1:
        values = values[: values.size - values.size % channels]
        values = values.reshape(-1, channels).mean(axis=1)
    return values, rate


def write_wav(path: pathlib.Path, samples: np.ndarray, rate: int) -> None:
    clipped = np.clip(samples, -32768, 32767).astype("<i2")
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(rate)
        handle.writeframes(clipped.tobytes())


def resample(samples: np.ndarray, source: int, target: int) -> np.ndarray:
    count = int(samples.size * target / source)
    positions = np.arange(count) * (source / target)
    return np.interp(positions, np.arange(samples.size), samples)


def band_limit(samples: np.ndarray, taps: int) -> np.ndarray:
    """Crude low-pass: a moving average. Models a chain that rolls off the
    upper tone harder than the lower one, which is the asymmetry most likely to
    bias an FSK decision."""
    kernel = np.ones(taps) / taps
    return np.convolve(samples, kernel, mode="same")


CASES: list[tuple[str, callable]] = [
    ("baseline", lambda s, r: (s, r)),
    # The most likely real failure: OBS and most capture cards default to 48k.
    ("resample_48k", lambda s, r: (resample(s, r, 48000), 48000)),
    ("resample_32k", lambda s, r: (resample(s, r, 32000), 32000)),
    ("resample_96k", lambda s, r: (resample(s, r, 96000), 96000)),
    # Gain extremes: FSK should be immune, since it decides on which tone wins.
    ("gain_x0.05", lambda s, r: (s * 0.05, r)),
    ("gain_x8_clipped", lambda s, r: (np.clip(s * 8, -32768, 32767), r)),
    ("dc_offset", lambda s, r: (s + 3000, r)),
    ("band_limit_5tap", lambda s, r: (band_limit(s, 5), r)),
    ("band_limit_9tap", lambda s, r: (band_limit(s, 9), r)),
    ("noise_10pct", lambda s, r: (s + np.random.default_rng(1).normal(0, 0.10 * np.max(np.abs(s)), s.size), r)),
    ("noise_25pct", lambda s, r: (s + np.random.default_rng(2).normal(0, 0.25 * np.max(np.abs(s)), s.size), r)),
    (
        "worst_case_stack",
        lambda s, r: (
            band_limit(
                resample(s, r, 48000)
                + np.random.default_rng(3).normal(
                    0, 0.15 * np.max(np.abs(s)), resample(s, r, 48000).size
                ),
                5,
            )
            * 0.3,
            48000,
        ),
    ),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wav", help="known-good recording to degrade")
    parser.add_argument("--workdir", default=None)
    args = parser.parse_args()

    source = pathlib.Path(args.wav)
    samples, rate = read_wav(source)
    workdir = pathlib.Path(args.workdir) if args.workdir else source.parent / "chaintest"
    workdir.mkdir(parents=True, exist_ok=True)
    print(f"# source {source} ({samples.size / rate:.1f}s at {rate} Hz)")

    failures = 0
    for name, transform in CASES:
        degraded, out_rate = transform(samples, rate)
        path = workdir / f"{name}.wav"
        write_wav(path, degraded, out_rate)
        result = subprocess.run(
            [sys.executable, str(DECODER), str(path)],
            capture_output=True,
            text=True,
        )
        ok = result.returncode == 0
        detail = ""
        for line in result.stdout.splitlines():
            if line.startswith("# crc32="):
                detail = line.removeprefix("# crc32=")
        if not ok:
            failures += 1
        print(f"{'PASS' if ok else 'FAIL'}  {name:22} {detail}")

    print(f"# {len(CASES) - failures}/{len(CASES)} degradations decoded")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
