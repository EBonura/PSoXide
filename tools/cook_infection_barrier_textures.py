#!/usr/bin/env python3
"""Cook the infection-barrier grate textures from their raw art.

The barriers are 4bpp PSXT textures: 16 CLUT entries, one of which is spent on
index-zero transparency, leaving 15 usable colours. The art is mostly near-black
structure with a narrow luminous accent, which is exactly the case the editor's
plain median-cut quantiser handles worst, so this tool owns the whole path from
the 1254x1254 raw painting down to the cooked blob.

Two stages:

1. Cutout. The raw art sits on a near-white studio background. Background
   coverage is computed at full resolution and downsampled alongside
   coverage-premultiplied colour, so the dark structure never picks up white
   bleed from the gaps it borders. Alpha is a hard 50% coverage cut because the
   PS1 has no partial alpha.

2. Quantise. Weighted k-means in Oklab: clusters are seeded by splitting on
   population-weighted variance (not raw channel range, which is what makes
   median-cut spend its whole budget on a handful of near-identical bright
   accents), then refined by Lloyd iterations. Palette entries are snapped to
   RGB555 before indices are assigned, colliding entries are reclaimed by
   re-splitting the worst cluster, and an opaque black lands on 0x8000 rather
   than 0x0000 so it does not render as a hole.

Deterministic: same inputs produce byte-identical output.
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image

REPO = Path(__file__).resolve().parent.parent
PROJECT = REPO / "editor/projects/cortex-ignition-tech-demo-0.3"
SOURCE_DIR = PROJECT / "source_assets/textures"
ASSET_DIR = PROJECT / "assets/textures"

# Anything whose darkest channel is above this is studio background, not art.
BACKGROUND_MIN_CHANNEL = 238
# PS1 has no partial alpha, so coverage becomes a hard cut.
COVERAGE_CUT = 0.5
CLUT_ENTRIES = 16
PSXT_MAGIC = b"PSXT"
PSXT_VERSION = 1
DEPTH_4BPP = 4
FLAG_INDEX_ZERO_TRANSPARENT = 0x0001


# --------------------------------------------------------------------------
# Oklab
# --------------------------------------------------------------------------


def srgb_to_linear(c: np.ndarray) -> np.ndarray:
    c = c / 255.0
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def linear_to_srgb(c: np.ndarray) -> np.ndarray:
    c = np.clip(c, 0.0, 1.0)
    out = np.where(c <= 0.0031308, c * 12.92, 1.055 * (c ** (1 / 2.4)) - 0.055)
    return np.clip(out * 255.0, 0, 255)


def rgb_to_oklab(rgb: np.ndarray) -> np.ndarray:
    lin = srgb_to_linear(rgb.astype(np.float64))
    r, g, b = lin[..., 0], lin[..., 1], lin[..., 2]
    l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b
    m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b
    s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b
    l_, m_, s_ = np.cbrt(l), np.cbrt(m), np.cbrt(s)
    return np.stack(
        [
            0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
            1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
            0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
        ],
        axis=-1,
    )


def oklab_to_rgb(lab: np.ndarray) -> np.ndarray:
    L, a, b = lab[..., 0], lab[..., 1], lab[..., 2]
    l_ = L + 0.3963377774 * a + 0.2158037573 * b
    m_ = L - 0.1055613458 * a - 0.0638541728 * b
    s_ = L - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_**3, m_**3, s_**3
    lin = np.stack(
        [
            +4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
            -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
            -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
        ],
        axis=-1,
    )
    return linear_to_srgb(lin)


# --------------------------------------------------------------------------
# RGB555
# --------------------------------------------------------------------------


def rgb_to_555(rgb: np.ndarray) -> np.ndarray:
    q = (np.clip(np.rint(rgb), 0, 255).astype(np.int32) >> 3) & 0x1F
    return (q[..., 0] | (q[..., 1] << 5) | (q[..., 2] << 10)).astype(np.int32)


def rgb555_to_rgb(word: int) -> tuple[int, int, int]:
    def expand(v: int) -> int:
        v &= 0x1F
        return (v << 3) | (v >> 2)

    return expand(word), expand(word >> 5), expand(word >> 10)


# --------------------------------------------------------------------------
# Stage 1: cutout
# --------------------------------------------------------------------------


def resample_plane(plane: np.ndarray, size: int, filt) -> np.ndarray:
    img = Image.fromarray(plane.astype(np.float32), mode="F")
    return np.asarray(img.resize((size, size), filt), dtype=np.float64)


def make_cutout(raw_path: Path, size: int, filt) -> Image.Image:
    """Downsample raw art to `size` while keeping background out of the colour."""
    raw = np.asarray(Image.open(raw_path).convert("RGB"), dtype=np.float64)
    coverage_hi = (raw.min(axis=2) <= BACKGROUND_MIN_CHANNEL).astype(np.float64)

    coverage = np.clip(resample_plane(coverage_hi, size, filt), 0.0, 1.0)
    premult = np.stack(
        [
            resample_plane(raw[..., c] * coverage_hi, size, filt)
            for c in range(3)
        ],
        axis=-1,
    )

    safe = np.maximum(coverage, 1e-4)[..., None]
    colour = np.clip(premult / safe, 0.0, 255.0)
    colour[coverage < 1e-3] = 0.0

    alpha = np.where(coverage >= COVERAGE_CUT, 255, 0).astype(np.uint8)
    rgba = np.concatenate(
        [np.rint(colour).astype(np.uint8), alpha[..., None]], axis=-1
    )
    return Image.fromarray(rgba, mode="RGBA")


# --------------------------------------------------------------------------
# Stage 2: quantise
# --------------------------------------------------------------------------


def snap_words(centres: np.ndarray) -> np.ndarray:
    """Oklab centres -> the RGB555 words the CLUT will really hold.

    A palette entry of 0x0000 renders fully transparent on the PS1, so an opaque
    black has to land on 0x8000 (mask bit set). Blend mode on these materials is
    Opaque, which makes the mask bit inert at draw time.
    """
    words = rgb_to_555(oklab_to_rgb(centres))
    return np.where(words == 0, 0x8000, words)


def weighted_split_init(lab: np.ndarray, weight: np.ndarray, k: int) -> np.ndarray:
    """Seed clusters by repeatedly splitting the highest-SSE cluster.

    Median-cut picks the box with the widest raw channel range and ignores how
    many pixels sit in it, which is why a handful of loud accent texels can eat
    most of the palette. Splitting on population-weighted squared error instead
    puts entries where the picture actually is.
    """
    labels = np.zeros(len(lab), dtype=np.int32)
    clusters = 1
    while clusters < k:
        best_id, best_sse = -1, -1.0
        for cid in range(clusters):
            sel = labels == cid
            w = weight[sel]
            if w.sum() < 2 or sel.sum() < 2:
                continue
            pts = lab[sel]
            mean = (pts * w[:, None]).sum(axis=0) / w.sum()
            sse = float((w[:, None] * (pts - mean) ** 2).sum())
            if sse > best_sse:
                best_sse, best_id = sse, cid
        if best_id < 0 or best_sse <= 0.0:
            break

        sel = np.flatnonzero(labels == best_id)
        pts, w = lab[sel], weight[sel]
        mean = (pts * w[:, None]).sum(axis=0) / w.sum()
        centred = pts - mean
        # Principal axis via a few power iterations on the weighted covariance.
        cov = (centred * w[:, None]).T @ centred / w.sum()
        axis = np.array([1.0, 0.0, 0.0])
        for _ in range(32):
            nxt = cov @ axis
            norm = np.linalg.norm(nxt)
            if norm < 1e-12:
                break
            axis = nxt / norm
        proj = centred @ axis
        order = np.argsort(proj, kind="stable")
        cw = np.cumsum(w[order])
        cut = int(np.searchsorted(cw, cw[-1] / 2.0))
        cut = min(max(cut, 1), len(order) - 1)
        labels[sel[order[cut:]]] = clusters
        clusters += 1
    return labels


def lloyd(lab: np.ndarray, weight: np.ndarray, centres: np.ndarray, rounds: int):
    for _ in range(rounds):
        d = ((lab[:, None, :] - centres[None, :, :]) ** 2).sum(axis=2)
        labels = np.argmin(d, axis=1)
        moved = False
        for cid in range(len(centres)):
            sel = labels == cid
            if not sel.any():
                continue
            w = weight[sel]
            new = (lab[sel] * w[:, None]).sum(axis=0) / w.sum()
            if not np.allclose(new, centres[cid]):
                moved = True
            centres[cid] = new
        if not moved:
            break
    return centres


def palette_cost(lab: np.ndarray, weight: np.ndarray, words: list[int]) -> float:
    """Population-weighted squared Oklab error of a candidate CLUT."""
    pal = rgb_to_oklab(np.array([rgb555_to_rgb(w) for w in words], dtype=np.float64))
    d = ((lab[:, None, :] - pal[None, :, :]) ** 2).sum(axis=2).min(axis=1)
    return float((d * weight).sum())


def neighbour_words(word: int) -> list[int]:
    """The 3x3x3 RGB555 lattice neighbourhood of `word`, mask bit preserved."""
    mask = word & 0x8000
    r, g, b = word & 0x1F, (word >> 5) & 0x1F, (word >> 10) & 0x1F
    out = []
    for dr in (-1, 0, 1):
        for dg in (-1, 0, 1):
            for db in (-1, 0, 1):
                nr, ng, nb = r + dr, g + dg, b + db
                if not (0 <= nr < 32 and 0 <= ng < 32 and 0 <= nb < 32):
                    continue
                candidate = nr | (ng << 5) | (nb << 10) | mask
                out.append(0x8000 if candidate == 0 else candidate)
    return out


def choose_palette(colours: np.ndarray, counts: np.ndarray, k: int) -> list[int]:
    """Return `k` distinct RGB555 words covering `colours` weighted by `counts`."""
    lab = rgb_to_oklab(colours.astype(np.float64))
    weight = counts.astype(np.float64)

    labels = weighted_split_init(lab, weight, k)
    n = int(labels.max()) + 1
    centres = np.zeros((n, 3))
    for cid in range(n):
        sel = labels == cid
        w = weight[sel]
        centres[cid] = (lab[sel] * w[:, None]).sum(axis=0) / w.sum()
    centres = lloyd(lab, weight, centres, rounds=40)

    # Snap to the 15-bit grid the CLUT actually stores. Distinct Oklab centres
    # can land on the same word, which is how the old cooker ended up with dead
    # entries; every collision is refilled with the input colour that the
    # surviving palette serves worst, so the entry is never wasted.
    snapped = snap_words(centres)
    words: list[int] = []
    for word in snapped:
        if int(word) not in words:
            words.append(int(word))

    colour_words = snap_words(rgb_to_oklab(colours.astype(np.float64)))
    while len(words) < k:
        pal_lab = rgb_to_oklab(
            np.array([rgb555_to_rgb(w) for w in words], dtype=np.float64)
        )
        d = ((lab[:, None, :] - pal_lab[None, :, :]) ** 2).sum(axis=2).min(axis=1)
        cost = d * weight
        order = np.argsort(-cost, kind="stable")
        for i in order:
            candidate = int(colour_words[i])
            if candidate not in words:
                words.append(candidate)
                break
        else:  # fewer than k distinct RGB555 colours exist in the source
            filler = 0x8000
            while filler in words:
                filler += 1
            words.append(filler)

    # Polish on the real CLUT grid. Each entry tries its Lloyd mean plus the
    # 3x3x3 RGB555 neighbourhood around itself, and a move is taken only when it
    # strictly lowers the population-weighted error of the whole palette, so the
    # sweep can only improve on the snapped k-means result.
    cost = palette_cost(lab, weight, words)
    for _ in range(8):
        pal_lab = rgb_to_oklab(
            np.array([rgb555_to_rgb(w) for w in words], dtype=np.float64)
        )
        labels = np.argmin(
            ((lab[:, None, :] - pal_lab[None, :, :]) ** 2).sum(axis=2), axis=1
        )
        improved = False
        for cid in range(len(words)):
            candidates = set(neighbour_words(words[cid]))
            sel = labels == cid
            if sel.any():
                w = weight[sel]
                mean = (lab[sel] * w[:, None]).sum(axis=0) / w.sum()
                candidates.add(int(snap_words(mean[None, :])[0]))
            best_word, best_cost = words[cid], cost
            for candidate in sorted(candidates):
                if candidate == words[cid] or candidate in words:
                    continue
                trial = list(words)
                trial[cid] = candidate
                trial_cost = palette_cost(lab, weight, trial)
                if trial_cost < best_cost - 1e-12:
                    best_word, best_cost = candidate, trial_cost
            if best_word != words[cid]:
                words[cid], cost, improved = best_word, best_cost, True
        if not improved:
            break

    return sorted(words)


BAYER8 = (
    np.array(
        [
            [0, 32, 8, 40, 2, 34, 10, 42],
            [48, 16, 56, 24, 50, 18, 58, 26],
            [12, 44, 4, 36, 14, 46, 6, 38],
            [60, 28, 52, 20, 62, 30, 54, 22],
            [3, 35, 11, 43, 1, 33, 9, 41],
            [51, 19, 59, 27, 49, 17, 57, 25],
            [15, 47, 7, 39, 13, 45, 5, 37],
            [63, 31, 55, 23, 61, 29, 53, 21],
        ],
        dtype=np.float64,
    )
    / 64.0
    - 0.5
)


def assign_indices(
    rgb: np.ndarray, opaque: np.ndarray, palette_rgb: np.ndarray, dither: float
) -> np.ndarray:
    """Nearest palette entry per pixel, measured in Oklab, index 0 = clear."""
    h, w = opaque.shape
    lab = rgb_to_oklab(rgb.astype(np.float64))
    if dither > 0.0:
        tile = np.tile(BAYER8, ((h + 7) // 8, (w + 7) // 8))[:h, :w]
        lab = lab + tile[..., None] * dither * np.array([1.0, 0.35, 0.35])
    pal_lab = rgb_to_oklab(palette_rgb.astype(np.float64))
    flat = lab.reshape(-1, 3)
    d = ((flat[:, None, :] - pal_lab[None, :, :]) ** 2).sum(axis=2)
    idx = (np.argmin(d, axis=1) + 1).astype(np.uint8).reshape(h, w)
    idx[~opaque] = 0
    return idx


# --------------------------------------------------------------------------
# PSXT container
# --------------------------------------------------------------------------


def write_psxt(path: Path, indices: np.ndarray, palette_words: list[int]) -> bytes:
    h, w = indices.shape
    hw_per_row = (w + 3) // 4
    pixels = np.zeros((h, hw_per_row), dtype=np.uint16)
    for x in range(w):
        pixels[:, x // 4] |= (indices[:, x].astype(np.uint16) & 0xF) << ((x & 3) * 4)
    pixel_bytes = pixels.astype("<u2").tobytes()
    clut_bytes = struct.pack("<%dH" % len(palette_words), *palette_words)

    payload = 16 + len(pixel_bytes) + len(clut_bytes)
    blob = bytearray()
    blob += PSXT_MAGIC
    blob += struct.pack("<HHI", PSXT_VERSION, FLAG_INDEX_ZERO_TRANSPARENT, payload)
    blob += struct.pack(
        "<BBHHHII",
        DEPTH_4BPP,
        0,
        w,
        h,
        len(palette_words),
        len(pixel_bytes),
        len(clut_bytes),
    )
    blob += pixel_bytes
    blob += clut_bytes
    path.write_bytes(blob)
    return bytes(blob)


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------


def cook(
    raw_path: Path,
    cutout_path: Path,
    psxt_path: Path,
    size: int,
    dither: float,
    filt,
    preview_path: Path | None = None,
) -> None:
    cutout = make_cutout(raw_path, size, filt)
    cutout.save(cutout_path)

    arr = np.asarray(cutout, dtype=np.int32)
    rgb, alpha = arr[..., :3], arr[..., 3]
    opaque = alpha >= 128
    flat = rgb[opaque]
    if len(flat) == 0:
        raise SystemExit(f"{raw_path.name}: cutout is fully transparent")

    packed = (flat[:, 0] << 16) | (flat[:, 1] << 8) | flat[:, 2]
    uniq, counts = np.unique(packed, return_counts=True)
    colours = np.stack([(uniq >> 16) & 0xFF, (uniq >> 8) & 0xFF, uniq & 0xFF], axis=-1)

    palette_words = choose_palette(colours, counts, CLUT_ENTRIES - 1)
    palette_rgb = np.array([rgb555_to_rgb(w) for w in palette_words], dtype=np.int32)

    indices = assign_indices(rgb, opaque, palette_rgb, dither)
    write_psxt(psxt_path, indices, [0x0000] + palette_words)

    if preview_path is not None:
        full = np.array([(0, 0, 0)] + [tuple(c) for c in palette_rgb], dtype=np.uint8)
        out = full[indices]
        a = np.where(indices == 0, 0, 255).astype(np.uint8)
        Image.fromarray(
            np.concatenate([out, a[..., None]], axis=-1), mode="RGBA"
        ).save(preview_path)

    used = np.bincount(indices.reshape(-1), minlength=CLUT_ENTRIES)
    dead = int((used[1:] == 0).sum())
    print(f"{psxt_path.name}: {size}x{size} 4bpp, dead entries {dead}/15")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--size", type=int, default=128)
    ap.add_argument(
        "--dither",
        type=float,
        default=0.0,
        help="ordered-dither amplitude in Oklab L units (0 disables)",
    )
    ap.add_argument("--filter", choices=["lanczos", "box"], default="lanczos")
    ap.add_argument("--suffix", default="v4", help="output asset suffix")
    ap.add_argument("--out-dir", type=Path, default=None)
    ap.add_argument("--preview-dir", type=Path, default=None)
    args = ap.parse_args()

    filt = Image.LANCZOS if args.filter == "lanczos" else Image.BOX
    asset_dir = args.out_dir or ASSET_DIR
    asset_dir.mkdir(parents=True, exist_ok=True)

    if args.preview_dir is not None:
        args.preview_dir.mkdir(parents=True, exist_ok=True)

    for name in ("horizon", "zenith"):
        raw = SOURCE_DIR / f"infection_barrier_{name}_grate_raw_v3.png"
        if not raw.exists():
            raise SystemExit(f"missing raw art: {raw}")
        cook(
            raw_path=raw,
            cutout_path=SOURCE_DIR
            / f"infection_barrier_{name}_grate_cutout_{args.suffix}.png",
            psxt_path=asset_dir / f"infection_barrier_{name}_grate_{args.suffix}.psxt",
            size=args.size,
            dither=args.dither,
            filt=filt,
            preview_path=None
            if args.preview_dir is None
            else args.preview_dir / f"decoded-{name}-{args.suffix}.png",
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
