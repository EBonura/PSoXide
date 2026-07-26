#!/usr/bin/env python3
"""Decode a hardware-test audio-link capture back into the PX7 payload.

The disc streams its whole capture out of the SPU as binary FSK and loops it
forever, so a recording made through a capture card carries the payload
continuously instead of in photographed QR stills. This recovers it.

    python3 tools/hwtest-audio-decode.py capture.wav --out payload.bin

Each bit is one ADPCM block: exactly 28 samples at 44.1 kHz. The two tones sit
on exact DFT bins of a 28-sample window (1575 Hz = bin 1, 3150 Hz = bin 2), so
deciding a bit is a comparison of two bin magnitudes and needs no filter
design. Every repetition in the recording is tried until one satisfies the
CRC, which is the point of looping the transmission: a glitch costs a
repetition rather than another burn.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import pathlib
import sys
import wave

import numpy as np

# Samples per bit at each transmit rate. The disc can be dropped to a slower
# rate on the console (SQUARE on the capture page) without a reburn, so the
# decoder tries every rate rather than being told which one was used.
RATE_SAMPLES_PER_BIT = (28, 56, 112, 224)
SYNC_WORD = 0x1ACF
PREAMBLE_BITS = 64
# Bin indices within one 28-sample window. Bit 1 is the lower tone.
BIN_ONE = 1
BIN_ZERO = 2
# A window whose combined tone energy falls below this fraction of the
# recording's own peak is silence rather than a bit. Relative, not absolute:
# capture-card gain and console volume vary by orders of magnitude, and a
# fixed floor would either reject a quiet capture entirely or admit noise in a
# loud one.
SILENCE_FRACTION = 0.10
# The link's bit clock IS the console's 44.1 kHz sample rate. Capture chains
# very often record at 48 kHz (OBS defaults to it), where a bit stops being a
# whole number of samples and the tones no longer land on exact DFT bins.
# Recordings are resampled to this rate before anything else.
LINK_RATE = 44100


def sliding_magnitudes(samples: np.ndarray, span: int) -> tuple[np.ndarray, np.ndarray]:
    """Magnitude of DFT bins 1 and 2 for a window starting at EVERY sample.

    A naive per-window DFT is O(n * span) and takes minutes on a couple of
    minutes of audio, which is unusable for an operator decoding a console
    recording. Multiplying the signal by the conjugate exponential and taking a
    length-`span` moving sum yields the same bins in O(n) via one cumulative
    sum per bin.
    """
    n = samples.size
    index = np.arange(n)
    out = []
    for bin_index in (BIN_ONE, BIN_ZERO):
        rotated = samples * np.exp(-2j * np.pi * bin_index * index / span)
        cumulative = np.concatenate(([0j], np.cumsum(rotated)))
        # Window [i, i+span) sum = cumulative[i+span] - cumulative[i].
        window_sums = cumulative[span:] - cumulative[:-span]
        out.append(np.abs(window_sums))
    return out[0], out[1]


class RateDecoder:
    """Bit decisions at one transmit rate, precomputed for every offset."""

    def __init__(self, samples: np.ndarray, span: int) -> None:
        self.span = span
        one, zero = sliding_magnitudes(samples, span)
        energy = one + zero
        peak = float(np.max(np.abs(samples))) if samples.size else 0.0
        self.floor = peak * span * SILENCE_FRACTION
        # -1 = silence / no signal, else the decided bit.
        self.bits = np.where(energy < self.floor, -1, (one > zero).astype(np.int8))
        self.valid = self.bits.size

    def bit(self, start: int) -> int:
        if start < 0 or start >= self.valid:
            return -1
        return int(self.bits[start])

    def read(self, start: int, count: int) -> list[int]:
        return [self.bit(start + i * self.span) for i in range(count)]


def bits_to_int(bits: list[int], start: int, count: int) -> int:
    value = 0
    for offset in range(count):
        value = (value << 1) | bits[start + offset]
    return value


def find_frames(decoder: RateDecoder, limit: int) -> list[int]:
    """Sample offsets where a preamble-then-sync sequence begins."""
    starts: list[int] = []
    span = decoder.span
    position = 0
    while position < decoder.valid - span * (PREAMBLE_BITS + 16) and len(starts) < limit:
        if decoder.bit(position) != 1:
            position += 1
            continue
        # Cheap gate: a real frame opens with alternating bits starting at 1.
        if decoder.read(position, 8) != [1, 0, 1, 0, 1, 0, 1, 0]:
            position += 1
            continue
        window = decoder.read(position, PREAMBLE_BITS + 16)
        if -1 in window:
            position += 1
            continue
        if any(window[i] != (1 if i % 2 == 0 else 0) for i in range(PREAMBLE_BITS)):
            position += 1
            continue
        if bits_to_int(window, PREAMBLE_BITS, 16) != SYNC_WORD:
            position += 1
            continue
        starts.append(position)
        position += span * PREAMBLE_BITS
    return starts


def frame_length(decoder: RateDecoder, start: int) -> int | None:
    bits = decoder.read(start + decoder.span * (PREAMBLE_BITS + 16), 16)
    if -1 in bits:
        return None
    length = bits_to_int(bits, 0, 16)
    return length if 1 <= length <= 65535 else None


def payload_bits(decoder: RateDecoder, start: int, length: int) -> list[int]:
    cursor = start + decoder.span * (PREAMBLE_BITS + 16 + 16)
    return decoder.read(cursor, length * 8 + 32)


def decode_frame(decoder: RateDecoder, start: int) -> bytes | None:
    length = frame_length(decoder, start)
    if length is None:
        return None
    bits = payload_bits(decoder, start, length)
    if -1 in bits:
        return None
    return check_payload(bits, length)


def check_payload(bits: list[int], length: int) -> bytes | None:
    payload = bytes(bits_to_int(bits, i * 8, 8) for i in range(length))
    claimed = bits_to_int(bits, length * 8, 32)
    if claimed != (binascii.crc32(payload) & 0xFFFF_FFFF):
        return None
    return payload


BASE64_CHARS_PER_PAGE = 828


def emit_pages(payload: bytes) -> str:
    encoded = base64.b64encode(payload).decode("ascii")
    chunks = [
        encoded[i : i + BASE64_CHARS_PER_PAGE]
        for i in range(0, len(encoded), BASE64_CHARS_PER_PAGE)
    ]
    lines = []
    for number, chunk in enumerate(chunks, start=1):
        crc = binascii.crc32(chunk.encode("ascii")) & 0xFFFF_FFFF
        lines.append(f"PX7/{number:02X}{len(chunks):02X}/{chunk}/C:{crc:08X}")
    return "\n".join(lines) + "\n"


def vote_payload(frames: list[list[int]], length: int) -> bytes | None:
    """Per-bit majority vote across repetitions.

    The transmission loops forever, so a marginal chain typically corrupts a
    DIFFERENT bit in each repetition. Decoding repetitions independently needs
    one flawless pass and can fail on all of them; voting per bit recovers a
    payload that no single repetition carried cleanly.
    """
    if not frames:
        return None
    needed = length * 8 + 32
    voted: list[int] = []
    for index in range(needed):
        ones = 0
        valid = 0
        for frame in frames:
            if index < len(frame) and frame[index] >= 0:
                valid += 1
                ones += frame[index]
        if valid == 0:
            return None
        voted.append(1 if ones * 2 > valid else 0)
    return check_payload(voted, length)


def resample_to_link_rate(samples: np.ndarray, rate: int) -> np.ndarray:
    """Linear-resample a recording to the link's 44.1 kHz bit clock.

    Linear interpolation is sufficient here because the decision is which of
    two tones dominates a window, not a faithful waveform reconstruction, and
    both tones sit far below Nyquist at any plausible capture rate.
    """
    if rate == LINK_RATE or samples.size == 0:
        return samples
    target = int(samples.size * LINK_RATE / rate)
    positions = np.arange(target) * (rate / LINK_RATE)
    return np.interp(positions, np.arange(samples.size), samples)


def read_wav(path: pathlib.Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as handle:
        if handle.getsampwidth() != 2:
            raise ValueError("expected 16-bit PCM")
        rate = handle.getframerate()
        channels = handle.getnchannels()
        raw = handle.readframes(handle.getnframes())
    values = np.frombuffer(raw, dtype="<i2").astype(np.float64)
    if channels > 1:
        # The link is mono; summing recovers it whether the capture chain
        # carried it on one side or both.
        values = values[: values.size - values.size % channels]
        values = values.reshape(-1, channels).sum(axis=1)
    return values, rate


def emit(payload: bytes, args) -> int:
    print(f"# recovered {len(payload)} bytes")
    print(f"# crc32={binascii.crc32(payload) & 0xFFFFFFFF:08X}")
    if args.out:
        pathlib.Path(args.out).write_bytes(payload)
        print(f"# wrote {args.out}")
    if args.emit_pages:
        pathlib.Path(args.emit_pages).write_text(emit_pages(payload))
        print(f"# wrote {args.emit_pages}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wav", help="recording containing the audio link")
    parser.add_argument("--out", help="write the recovered payload here")
    parser.add_argument(
        "--emit-pages",
        help="re-encode the payload as PX7 page lines for hwtest-report.py",
    )
    parser.add_argument(
        "--max-frames",
        type=int,
        default=6,
        help="candidate frames to consider per rate (default 6)",
    )
    args = parser.parse_args()

    samples, rate = read_wav(pathlib.Path(args.wav))
    duration = samples.size / max(rate, 1)
    if rate != LINK_RATE:
        print(f"# resampling {rate} Hz -> {LINK_RATE} Hz")
        samples = resample_to_link_rate(samples, rate)
    print(f"# samples={samples.size} rate={rate} ({duration:.1f}s)")

    for span in RATE_SAMPLES_PER_BIT:
        decoder = RateDecoder(samples, span)
        starts = find_frames(decoder, args.max_frames)
        if not starts:
            continue
        print(f"# rate {span} samples/bit: {len(starts)} frame(s) at {starts}")

        # A clean repetition needs no voting, so try each on its own first.
        collected: list[list[int]] = []
        length = 0
        for start in starts:
            payload = decode_frame(decoder, start)
            if payload is not None:
                return emit(payload, args)
            candidate = frame_length(decoder, start)
            if candidate is None:
                continue
            if length == 0:
                length = candidate
            if candidate != length:
                continue
            collected.append(payload_bits(decoder, start, length))

        if len(collected) >= 3:
            print(f"# no clean repetition; voting across {len(collected)}")
            voted = vote_payload(collected, length)
            if voted is not None:
                return emit(voted, args)

    print("FAIL: no frame recovered at any rate", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
