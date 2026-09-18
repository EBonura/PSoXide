#!/usr/bin/env python3
"""Rebuild the WAV view of a CD-DA track from its raw sector-padded PCM.

A `.track*.cdda` already IS the audio: it is the WAV's `data` chunk followed by
zero bytes padding it out to a whole 2352-byte CD sector. Tracking both forms
cost 39 MB for one song, so only the `.cdda` is committed and anything that
wants a RIFF file (`psoxide-dev bake-spectrum`) reconstructs one here.

The exact padding length comes from the sibling `.cdda.json` (`pad_bytes`), so
the recovered `data` chunk is byte-identical to the WAV that used to be
tracked. Only the WAV's `LIST` metadata chunk is lost, which nothing reads.
"""

import argparse
import struct
import sys
from pathlib import Path

SECTOR_BYTES = 2352


def wav_bytes(pcm: bytes, rate: int, channels: int, bits: int) -> bytes:
    """Wrap raw little-endian PCM in a canonical 44-byte RIFF/WAVE header."""
    block_align = channels * bits // 8
    byte_rate = rate * block_align
    return b"".join(
        (
            b"RIFF",
            struct.pack("<I", 36 + len(pcm)),
            b"WAVEfmt ",
            struct.pack("<IHHIIHH", 16, 1, channels, rate, byte_rate, block_align, bits),
            b"data",
            struct.pack("<I", len(pcm)),
            pcm,
        )
    )


def pcm_from_cdda(raw: bytes, pad_bytes: int) -> bytes:
    """Drop exactly the sector padding the cook recorded.

    The padding cannot be recovered by scanning for trailing zeros: this track
    ends in 56 KB of real silence, and trimming that clipped the audio. The
    sibling `.cdda.json` records `pad_bytes`, which is the only exact answer.
    """
    if len(raw) % SECTOR_BYTES:
        print(
            f"warning: {len(raw)} bytes is not a whole number of {SECTOR_BYTES}-byte sectors",
            file=sys.stderr,
        )
    if not 0 <= pad_bytes <= len(raw):
        raise ValueError(f"pad_bytes {pad_bytes} out of range for {len(raw)} bytes")
    return raw[: len(raw) - pad_bytes]


def pad_bytes_for(cdda: Path, override: int | None) -> int:
    """`--pad-bytes` wins; otherwise read it from the sibling manifest."""
    if override is not None:
        return override
    manifest = cdda.with_suffix("").with_suffix(".cdda.json")
    if not manifest.is_file():
        manifest = cdda.parent / f"{cdda.name.split('.')[0]}.cdda.json"
    if manifest.is_file():
        import json

        track = json.loads(manifest.read_text()).get("outputs", {}).get("cdda_track", {})
        if "pad_bytes" in track:
            return int(track["pad_bytes"])
    raise SystemExit(
        f"no pad_bytes for {cdda}: pass --pad-bytes, or record it in {manifest.name}"
    )


def demo() -> None:
    """Self-check: a padded round trip recovers the original PCM exactly."""
    pcm = bytes(range(256)) * 8  # 2048 bytes, whole 4-byte frames
    pad = SECTOR_BYTES - len(pcm) % SECTOR_BYTES
    padded = pcm + b"\0" * pad
    assert len(padded) % SECTOR_BYTES == 0
    assert pcm_from_cdda(padded, pad) == pcm
    # Audio that genuinely ends in silence must survive: only the recorded
    # padding is removed, never a trailing-zero scan.
    quiet = pcm + b"\0" * 512
    quiet_pad = SECTOR_BYTES - len(quiet) % SECTOR_BYTES
    assert pcm_from_cdda(quiet + b"\0" * quiet_pad, quiet_pad) == quiet
    header = wav_bytes(pcm, 44100, 2, 16)
    assert header[:4] == b"RIFF" and header[8:12] == b"WAVE"
    assert header[44:] == pcm
    assert struct.unpack("<I", header[40:44])[0] == len(pcm)
    print("wav_from_cdda self-check OK")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cdda", nargs="?", type=Path, help="raw .cdda track")
    parser.add_argument("-o", "--output", type=Path, help="WAV to write")
    parser.add_argument("--rate", type=int, default=44100)
    parser.add_argument("--channels", type=int, default=2)
    parser.add_argument("--bits", type=int, default=16)
    parser.add_argument("--pad-bytes", type=int, default=None, help="override the manifest")
    parser.add_argument("--self-check", action="store_true", help="run the round-trip check")
    args = parser.parse_args()
    if args.self_check:
        demo()
        return 0
    if not args.cdda or not args.output:
        parser.error("cdda and -o are required unless --self-check is given")
    pcm = pcm_from_cdda(args.cdda.read_bytes(), pad_bytes_for(args.cdda, args.pad_bytes))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(wav_bytes(pcm, args.rate, args.channels, args.bits))
    print(f"{args.cdda} -> {args.output} ({len(pcm)} PCM bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
