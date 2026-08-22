#!/usr/bin/env python3
"""Verify that a web build contains every streamed demo-disc asset."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


class VerificationError(Exception):
    """A required delivery file is absent or does not match the manifest."""


PUBLIC_TRACK_TITLES = {
    2: "KNUCKLE DUST",
    3: "RUSTED HAMMER",
    4: "CHAINSAW HEART",
    5: "NIGHT CRAWLER",
    6: "CORTEX IGNITION",
    7: "GH-PSX",
    8: "HARDWARE TESTS",
}


def manifest_assets(manifest: Path) -> list[tuple[str, int]]:
    """Return each manifest filename and its delivery size in bytes."""
    assets: list[tuple[str, int]] = []
    for line_number, line in enumerate(
        manifest.read_text(encoding="utf-8").splitlines(), start=1
    ):
        fields = line.split()
        if not fields:
            continue
        if fields[0] == "data" and len(fields) == 5:
            filename, delivery_bytes = fields[1], fields[2]
        elif fields[0] == "track" and len(fields) >= 7:
            filename, delivery_bytes = fields[2], fields[3]
        else:
            raise VerificationError(
                f"{manifest.name}:{line_number}: malformed manifest row"
            )
        if Path(filename).name != filename:
            raise VerificationError(
                f"{manifest.name}:{line_number}: unsafe asset name {filename!r}"
            )
        try:
            assets.append((filename, int(delivery_bytes)))
        except ValueError as error:
            raise VerificationError(
                f"{manifest.name}:{line_number}: invalid delivery size"
            ) from error
    if not assets:
        raise VerificationError(f"{manifest.name}: no delivery assets listed")
    return assets


def verify(dist: Path) -> tuple[int, int]:
    """Validate the CUE, manifest, and every file named by the manifest."""
    manifest = dist / "web-manifest.txt"
    cue = dist / "demo-disc.cue"
    for required in (manifest, cue):
        if not required.is_file():
            raise VerificationError(f"missing {required}")

    cue_text = cue.read_text(encoding="utf-8")
    if 'FILE "demo-disc.bin" BINARY' not in cue_text:
        raise VerificationError("demo-disc.cue does not reference demo-disc.bin")

    assets = manifest_assets(manifest)
    total_bytes = 0
    seen: set[str] = set()
    for filename, expected_bytes in assets:
        if filename in seen:
            raise VerificationError(f"duplicate manifest asset {filename}")
        seen.add(filename)
        path = dist / filename
        if not path.is_file():
            raise VerificationError(f"missing manifest asset {path}")
        actual_bytes = path.stat().st_size
        if actual_bytes != expected_bytes:
            raise VerificationError(
                f"{filename}: expected {expected_bytes} bytes, found {actual_bytes}"
            )
        total_bytes += actual_bytes
    return len(assets), total_bytes


def verify_public_disc(dist: Path) -> None:
    """Fail closed if the rolling web payload is not the reviewed public disc."""
    manifest_text = (dist / "web-manifest.txt").read_text(encoding="utf-8")
    cue_text = (dist / "demo-disc.cue").read_text(encoding="utf-8")
    if re.search(r"half[ -]?life", manifest_text, re.IGNORECASE):
        raise VerificationError("public web manifest names Half-Life content")

    actual_titles: dict[int, str] = {}
    for line_number, line in enumerate(manifest_text.splitlines(), start=1):
        fields = line.split()
        if fields and fields[0] == "track":
            if len(fields) < 7:
                raise VerificationError(
                    f"web-manifest.txt:{line_number}: track title is missing"
                )
            try:
                track_number = int(fields[1])
            except ValueError as error:
                raise VerificationError(
                    f"web-manifest.txt:{line_number}: invalid track number"
                ) from error
            actual_titles[track_number] = " ".join(fields[6:])
    if actual_titles != PUBLIC_TRACK_TITLES:
        raise VerificationError(
            "public web-disc audio layout changed; review and update the allowlist"
        )

    cue_tracks = [
        int(track)
        for track in re.findall(r"^[ \t]*TRACK[ \t]+([0-9]+)", cue_text, re.MULTILINE)
    ]
    if cue_tracks != list(range(1, 9)):
        raise VerificationError(
            "public demo-disc.cue must contain exactly tracks 1 through 8"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dist", type=Path, help="Trunk dist directory to verify")
    parser.add_argument(
        "--public-disc",
        action="store_true",
        help="require the reviewed public, no-Half-Life disc track layout",
    )
    args = parser.parse_args()
    try:
        asset_count, total_bytes = verify(args.dist)
        if args.public_disc:
            verify_public_disc(args.dist)
    except (OSError, VerificationError) as error:
        parser.error(str(error))
    print(
        f"web delivery verified: {asset_count} manifest assets, "
        f"{total_bytes / (1024 * 1024):.2f} MiB"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
