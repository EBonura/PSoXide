#!/usr/bin/env python3
"""Bake a compact music spectrum table from a 44.1 kHz PCM WAV.

Output is raw u8 amplitudes laid out frame-major:

    frame0_band0, frame0_band1, ..., frame1_band0, ...

The runtime intentionally stays dumb: index by tick, draw bars.
"""

from __future__ import annotations

import argparse
import array
import math
import pathlib
import sys
import wave


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=pathlib.Path)
    parser.add_argument("-o", "--output", type=pathlib.Path, required=True)
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument("--bands", type=int, default=16)
    parser.add_argument("--seconds", type=float, default=None)
    parser.add_argument("--window", type=int, default=512)
    parser.add_argument("--min-hz", type=float, default=80.0)
    parser.add_argument("--max-hz", type=float, default=8000.0)
    return parser.parse_args()


def read_pcm16(path: pathlib.Path) -> tuple[array.array, int, int, int]:
    with wave.open(str(path), "rb") as wav:
        channels = wav.getnchannels()
        width = wav.getsampwidth()
        rate = wav.getframerate()
        frames = wav.getnframes()
        if width != 2:
            raise SystemExit(f"{path}: expected 16-bit PCM WAV, got {width * 8}-bit")
        raw = wav.readframes(frames)

    pcm = array.array("h")
    pcm.frombytes(raw)
    if sys.byteorder != "little":
        pcm.byteswap()
    return pcm, channels, rate, frames


def log_spaced_frequencies(count: int, min_hz: float, max_hz: float) -> list[float]:
    if count <= 0:
        raise SystemExit("--bands must be positive")
    if min_hz <= 0 or max_hz <= min_hz:
        raise SystemExit("--min-hz/--max-hz must form a positive range")
    if count == 1:
        return [(min_hz + max_hz) * 0.5]

    ratio = max_hz / min_hz
    return [min_hz * (ratio ** (i / (count - 1))) for i in range(count)]


def frame_samples(
    pcm: array.array,
    channels: int,
    start: int,
    window: list[float],
) -> list[float]:
    out: list[float] = []
    total_frames = len(pcm) // channels
    for i, weight in enumerate(window):
        src = start + i
        if src >= total_frames:
            out.append(0.0)
            continue

        base = src * channels
        if channels == 1:
            sample = pcm[base]
        else:
            sample = (pcm[base] + pcm[base + 1]) * 0.5
        out.append((sample / 32768.0) * weight)
    return out


def goertzel_power(samples: list[float], rate: int, freq: float) -> float:
    n = len(samples)
    k = max(1, min(n // 2 - 1, round((n * freq) / rate)))
    omega = (2.0 * math.pi * k) / n
    coeff = 2.0 * math.cos(omega)
    s_prev = 0.0
    s_prev2 = 0.0
    for sample in samples:
        s = sample + coeff * s_prev - s_prev2
        s_prev2 = s_prev
        s_prev = s
    return s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    idx = max(0, min(len(values) - 1, round((len(values) - 1) * q)))
    return sorted(values)[idx]


def normalize_and_smooth(powers: list[list[float]]) -> bytes:
    if not powers:
        return b""

    frames = len(powers)
    bands = len(powers[0])
    floors = [0.0] * bands
    ceilings = [1.0] * bands
    for band in range(bands):
        values = [math.log1p(powers[frame][band]) for frame in range(frames)]
        floors[band] = percentile(values, 0.10)
        ceilings[band] = max(floors[band] + 0.001, percentile(values, 0.96))

    prev = [0.0] * bands
    out = bytearray(frames * bands)
    for frame in range(frames):
        for band in range(bands):
            value = math.log1p(powers[frame][band])
            norm = (value - floors[band]) / (ceilings[band] - floors[band])
            norm = max(0.0, min(1.0, norm))
            target = math.sqrt(norm)
            if target > prev[band]:
                smoothed = prev[band] * 0.35 + target * 0.65
            else:
                smoothed = prev[band] * 0.78 + target * 0.22
            prev[band] = smoothed
            out[frame * bands + band] = max(0, min(255, round(smoothed * 255.0)))
    return bytes(out)


def main() -> None:
    args = parse_args()
    if args.fps <= 0:
        raise SystemExit("--fps must be positive")
    if args.window <= 8 or args.window & (args.window - 1):
        raise SystemExit("--window must be a power of two greater than 8")

    pcm, channels, rate, total_frames = read_pcm16(args.input)
    seconds = args.seconds if args.seconds is not None else total_frames / rate
    frame_count = max(1, int(seconds * args.fps))
    hop = rate / args.fps
    freqs = log_spaced_frequencies(args.bands, args.min_hz, args.max_hz)
    window = [
        0.5 - 0.5 * math.cos((2.0 * math.pi * i) / (args.window - 1))
        for i in range(args.window)
    ]

    powers: list[list[float]] = []
    for frame in range(frame_count):
        start = int(frame * hop)
        samples = frame_samples(pcm, channels, start, window)
        powers.append([goertzel_power(samples, rate, freq) for freq in freqs])

    baked = normalize_and_smooth(powers)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(baked)
    print(
        f"[bake-spectrum] {args.input} -> {args.output} "
        f"({frame_count} frames x {args.bands} bands @ {args.fps} Hz, {len(baked)} bytes)"
    )


if __name__ == "__main__":
    main()
