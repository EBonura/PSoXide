#!/usr/bin/env python3
"""Prototype and measure per-UV-region 4bpp palette banks for a PSX model.

The tool keeps connected or overlapping UV islands in one palette bank, then
optimises one to four independent 15-colour opaque palettes. Metrics are
calculated only on opaque texels covered by model UV triangles; transparent
index zero is preserved separately.
"""

from __future__ import annotations

import argparse
import csv
import math
import struct
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont


ASSET_HEADER_SIZE = 12
TEXTURE_HEADER_SIZE = 16
MODEL_HEADER_SIZE = 16
JOINT_RECORD_SIZE = 4
MATERIAL_RECORD_SIZE = 8
PART_RECORD_SIZE = 16
VERTEX_RECORD_SIZE = 8
FACE_RECORD_SIZE = 12


@dataclass(frozen=True)
class Texture:
    width: int
    height: int
    flags: int
    rgb555: np.ndarray
    rgba: np.ndarray


@dataclass(frozen=True)
class Face:
    vertices: tuple[int, int, int]
    uvs: tuple[tuple[int, int], tuple[int, int], tuple[int, int]]


class DisjointSet:
    def __init__(self, size: int) -> None:
        self.parent = list(range(size))

    def find(self, item: int) -> int:
        while self.parent[item] != item:
            self.parent[item] = self.parent[self.parent[item]]
            item = self.parent[item]
        return item

    def union(self, left: int, right: int) -> None:
        left = self.find(left)
        right = self.find(right)
        if left != right:
            self.parent[right] = left


def read_u16(data: bytes, offset: int) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def read_u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def rgb555_to_rgb8(words: np.ndarray) -> np.ndarray:
    words = words.astype(np.uint16)
    channels = np.stack(
        [words & 31, (words >> 5) & 31, (words >> 10) & 31], axis=-1
    ).astype(np.uint16)
    return ((channels * 255 + 15) // 31).astype(np.uint8)


def rgb8_to_rgb555(rgb: np.ndarray) -> np.ndarray:
    values = ((rgb.astype(np.uint16) * 31 + 127) // 255).clip(0, 31)
    return (values[..., 0] | (values[..., 1] << 5) | (values[..., 2] << 10)).astype(
        np.uint16
    )


def decode_psxt(path: Path) -> Texture:
    data = path.read_bytes()
    if data[:4] != b"PSXT":
        raise ValueError(f"{path}: not a PSXT texture")
    flags = read_u16(data, 6)
    depth = data[12]
    width = read_u16(data, 14)
    height = read_u16(data, 16)
    clut_entries = read_u16(data, 18)
    pixel_bytes = read_u32(data, 20)
    clut_bytes = read_u32(data, 24)
    pixel_start = ASSET_HEADER_SIZE + TEXTURE_HEADER_SIZE
    palette_start = pixel_start + pixel_bytes
    palette = np.frombuffer(
        data[palette_start : palette_start + clut_bytes], dtype="<u2"
    ).copy()
    if len(palette) != clut_entries:
        raise ValueError(f"{path}: truncated CLUT")
    packed = data[pixel_start:palette_start]
    if depth == 8:
        row_bytes = (width + 1) // 2 * 2
        rows = np.frombuffer(packed, dtype=np.uint8).reshape(height, row_bytes)
        indices = rows[:, :width]
    elif depth == 4:
        row_bytes = (width + 3) // 4 * 2
        rows = np.frombuffer(packed, dtype=np.uint8).reshape(height, row_bytes)
        indices = np.empty((height, width), dtype=np.uint8)
        for x in range(width):
            value = rows[:, x // 2]
            indices[:, x] = (value >> (4 * (x & 1))) & 15
    else:
        raise ValueError(f"{path}: only indexed PSXT input is supported")
    words = palette[indices]
    rgba = np.empty((height, width, 4), dtype=np.uint8)
    rgba[..., :3] = rgb555_to_rgb8(words)
    transparent = (indices == 0) if flags & 1 else np.zeros_like(indices, dtype=bool)
    rgba[..., 3] = np.where(transparent, 0, 255).astype(np.uint8)
    return Texture(width, height, flags, words, rgba)


def decode_model(path: Path) -> tuple[int, int, list[Face]]:
    data = path.read_bytes()
    if data[:4] != b"PSMD":
        raise ValueError(f"{path}: not a PSMD model")
    base = ASSET_HEADER_SIZE
    joint_count, part_count, vertex_count, face_count, material_count = struct.unpack_from(
        "<5H", data, base
    )
    texture_width, texture_height = struct.unpack_from("<2H", data, base + 10)
    face_start = (
        base
        + MODEL_HEADER_SIZE
        + joint_count * JOINT_RECORD_SIZE
        + material_count * MATERIAL_RECORD_SIZE
        + part_count * PART_RECORD_SIZE
        + vertex_count * VERTEX_RECORD_SIZE
    )
    faces: list[Face] = []
    for face_index in range(face_count):
        offset = face_start + face_index * FACE_RECORD_SIZE
        corners = [struct.unpack_from("<HBB", data, offset + corner * 4) for corner in range(3)]
        faces.append(
            Face(
                tuple(corner[0] for corner in corners),
                tuple((corner[1], corner[2]) for corner in corners),
            )
        )
    return texture_width, texture_height, faces


def triangle_mask(face: Face, width: int, height: int) -> np.ndarray:
    points = np.asarray(face.uvs, dtype=np.float64)
    min_x = max(0, int(math.floor(float(points[:, 0].min()))))
    max_x = min(width - 1, int(math.ceil(float(points[:, 0].max()))))
    min_y = max(0, int(math.floor(float(points[:, 1].min()))))
    max_y = min(height - 1, int(math.ceil(float(points[:, 1].max()))))
    mask = np.zeros((height, width), dtype=bool)
    if min_x > max_x or min_y > max_y:
        return mask
    xs, ys = np.meshgrid(
        np.arange(min_x, max_x + 1, dtype=np.float64) + 0.5,
        np.arange(min_y, max_y + 1, dtype=np.float64) + 0.5,
    )
    a, b, c = points
    denominator = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1])
    if abs(denominator) > 1e-9:
        wa = ((b[1] - c[1]) * (xs - c[0]) + (c[0] - b[0]) * (ys - c[1])) / denominator
        wb = ((c[1] - a[1]) * (xs - c[0]) + (a[0] - c[0]) * (ys - c[1])) / denominator
        wc = 1.0 - wa - wb
        inside = (wa >= -1e-9) & (wb >= -1e-9) & (wc >= -1e-9)
        mask[min_y : max_y + 1, min_x : max_x + 1] = inside
    for u, v in face.uvs:
        mask[min(height - 1, v), min(width - 1, u)] = True
    return mask


def face_components(
    faces: list[Face], width: int, height: int
) -> tuple[list[np.ndarray], list[int]]:
    sets = DisjointSet(len(faces))
    edge_owner: dict[tuple[tuple[int, int, int], tuple[int, int, int]], int] = {}
    for face_index, face in enumerate(faces):
        corners = [
            (face.vertices[index], face.uvs[index][0], face.uvs[index][1]) for index in range(3)
        ]
        for edge_index in range(3):
            edge = tuple(sorted((corners[edge_index], corners[(edge_index + 1) % 3])))
            owner = edge_owner.get(edge)
            if owner is None:
                edge_owner[edge] = face_index
            else:
                sets.union(owner, face_index)

    masks = [triangle_mask(face, width, height) for face in faces]
    texel_owner = np.full((height, width), -1, dtype=np.int32)
    for face_index, mask in enumerate(masks):
        previous = np.unique(texel_owner[mask])
        for owner in previous:
            if owner >= 0:
                sets.union(face_index, int(owner))
        empty = mask & (texel_owner < 0)
        texel_owner[empty] = face_index

    grouped: dict[int, np.ndarray] = {}
    for face_index, mask in enumerate(masks):
        root = sets.find(face_index)
        if root not in grouped:
            grouped[root] = np.zeros((height, width), dtype=bool)
        grouped[root] |= mask
    ordered = sorted(grouped.items(), key=lambda item: int(item[1].sum()), reverse=True)
    root_to_component = {root: index for index, (root, _) in enumerate(ordered)}
    face_component = [root_to_component[sets.find(index)] for index in range(len(faces))]
    return [mask for _, mask in ordered], face_component


def srgb_to_lab(rgb: np.ndarray) -> np.ndarray:
    value = rgb.astype(np.float64) / 255.0
    linear = np.where(value <= 0.04045, value / 12.92, ((value + 0.055) / 1.055) ** 2.4)
    xyz = linear @ np.asarray(
        [
            [0.4124564, 0.2126729, 0.0193339],
            [0.3575761, 0.7151522, 0.1191920],
            [0.1804375, 0.0721750, 0.9503041],
        ]
    )
    xyz /= np.asarray([0.95047, 1.0, 1.08883])
    epsilon = 216.0 / 24389.0
    kappa = 24389.0 / 27.0
    f = np.where(xyz > epsilon, np.cbrt(xyz), (kappa * xyz + 16.0) / 116.0)
    return np.stack(
        [116.0 * f[..., 1] - 16.0, 500.0 * (f[..., 0] - f[..., 1]), 200.0 * (f[..., 1] - f[..., 2])],
        axis=-1,
    )


def palette_for_histogram(
    unique_words: np.ndarray,
    unique_lab: np.ndarray,
    weights: np.ndarray,
    colour_count: int = 15,
) -> np.ndarray:
    active = weights > 0
    words = unique_words[active]
    labs = unique_lab[active]
    weights = weights[active].astype(np.float64)
    if len(words) == 0:
        return np.zeros(colour_count, dtype=np.uint16)
    if len(words) <= colour_count:
        order = np.argsort(-weights, kind="stable")
        result = words[order]
        return np.pad(result, (0, colour_count - len(result)), mode="edge")

    selected = [int(np.argmax(weights))]
    nearest = np.sum((labs - labs[selected[0]]) ** 2, axis=1)
    for _ in range(1, colour_count):
        score = nearest * np.sqrt(weights / weights.max())
        candidate = int(np.argmax(score))
        selected.append(candidate)
        nearest = np.minimum(nearest, np.sum((labs - labs[candidate]) ** 2, axis=1))
    centres = labs[selected].copy()

    for _ in range(30):
        distances = np.sum((labs[:, None, :] - centres[None, :, :]) ** 2, axis=2)
        assignments = distances.argmin(axis=1)
        updated = centres.copy()
        for cluster in range(colour_count):
            members = assignments == cluster
            if not members.any():
                continue
            mean = np.average(labs[members], axis=0, weights=weights[members])
            member_indices = np.flatnonzero(members)
            medoid = member_indices[int(np.argmin(np.sum((labs[members] - mean) ** 2, axis=1)))]
            updated[cluster] = labs[medoid]
        if np.array_equal(updated, centres):
            break
        centres = updated

    nearest_words = []
    for centre in centres:
        nearest_words.append(words[int(np.argmin(np.sum((labs - centre) ** 2, axis=1)))])
    return np.asarray(nearest_words, dtype=np.uint16)


def component_histograms(
    texture: Texture, components: list[np.ndarray]
) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    opaque_words = texture.rgb555[texture.rgba[..., 3] != 0]
    unique_words = np.unique(opaque_words)
    unique_rgb = rgb555_to_rgb8(unique_words)
    unique_lab = srgb_to_lab(unique_rgb)
    word_to_index = {int(word): index for index, word in enumerate(unique_words)}
    histograms = np.zeros((len(components), len(unique_words)), dtype=np.float64)
    for component_index, component in enumerate(components):
        words, counts = np.unique(
            texture.rgb555[component & (texture.rgba[..., 3] != 0)], return_counts=True
        )
        for word, count in zip(words, counts, strict=True):
            histograms[component_index, word_to_index[int(word)]] = count
    features = np.zeros((len(components), 6), dtype=np.float64)
    for component_index, histogram in enumerate(histograms):
        if histogram.sum() == 0:
            continue
        mean = np.average(unique_lab, axis=0, weights=histogram)
        spread = np.sqrt(np.average((unique_lab - mean) ** 2, axis=0, weights=histogram))
        features[component_index] = np.concatenate((mean, spread))
    scale = np.std(features, axis=0)
    features /= np.where(scale > 1e-6, scale, 1.0)
    return unique_words, unique_lab, histograms, features


def balance_perceptual_cells(histograms: np.ndarray, unique_lab: np.ndarray) -> np.ndarray:
    """Stop a large neutral family from hiding errors in smaller hue families."""
    total = histograms.sum(axis=0)
    hue = np.degrees(np.arctan2(unique_lab[:, 2], unique_lab[:, 1])) % 360.0
    chroma = np.sqrt(unique_lab[:, 1] ** 2 + unique_lab[:, 2] ** 2)
    lightness_bin = np.minimum((unique_lab[:, 0] // 20.0).astype(np.int32), 4)
    hue_bin = (hue // 30.0).astype(np.int32)
    hue_bin[chroma < 6.0] = 12
    cells = hue_bin * 5 + lightness_bin
    cell_counts = np.asarray([total[cells == cell].sum() for cell in range(65)])
    nonempty = cell_counts[cell_counts > 0]
    if len(nonempty) == 0:
        return histograms
    target = float(np.median(nonempty))
    factors = np.ones(len(unique_lab), dtype=np.float64)
    for index, cell in enumerate(cells):
        count = float(cell_counts[cell])
        if count > 0:
            factors[index] = np.clip(math.sqrt(target / count), 0.75, 3.0)
    return histograms * factors[None, :]


def palette_costs(
    palettes: list[np.ndarray],
    unique_words: np.ndarray,
    unique_lab: np.ndarray,
    histograms: np.ndarray,
) -> np.ndarray:
    costs = np.empty((len(histograms), len(palettes)), dtype=np.float64)
    for bank, palette in enumerate(palettes):
        palette_lab = srgb_to_lab(rgb555_to_rgb8(palette))
        errors = np.min(
            np.sum((unique_lab[:, None, :] - palette_lab[None, :, :]) ** 2, axis=2), axis=1
        )
        costs[:, bank] = histograms @ errors
    return costs


def seeded_assignments(
    features: np.ndarray, weights: np.ndarray, bank_count: int, seed: int
) -> np.ndarray:
    rng = np.random.default_rng(seed)
    if bank_count == 1:
        return np.zeros(len(features), dtype=np.int32)
    centres = [int(rng.choice(len(features), p=weights / weights.sum()))]
    nearest = np.sum((features - features[centres[0]]) ** 2, axis=1)
    for _ in range(1, bank_count):
        probability = nearest * np.sqrt(weights)
        if probability.sum() <= 1e-9:
            remaining = [index for index in range(len(features)) if index not in centres]
            centres.append(remaining[0])
        else:
            centres.append(int(rng.choice(len(features), p=probability / probability.sum())))
        nearest = np.minimum(nearest, np.sum((features - features[centres[-1]]) ** 2, axis=1))
    distance = np.stack(
        [np.sum((features - features[centre]) ** 2, axis=1) for centre in centres], axis=1
    )
    return distance.argmin(axis=1).astype(np.int32)


def optimise_banks(
    bank_count: int,
    unique_words: np.ndarray,
    unique_lab: np.ndarray,
    histograms: np.ndarray,
    features: np.ndarray,
) -> tuple[np.ndarray, list[np.ndarray], float]:
    weights = histograms.sum(axis=1)
    best: tuple[np.ndarray, list[np.ndarray], float] | None = None
    restart_count = 1 if bank_count == 1 else 48
    for seed in range(restart_count):
        assignments = seeded_assignments(features, weights, bank_count, seed)
        for bank in range(bank_count):
            if not np.any(assignments == bank):
                assignments[int(np.argmax(weights))] = bank
        for _ in range(30):
            palettes = [
                palette_for_histogram(
                    unique_words,
                    unique_lab,
                    histograms[assignments == bank].sum(axis=0),
                )
                for bank in range(bank_count)
            ]
            costs = palette_costs(palettes, unique_words, unique_lab, histograms)
            updated = costs.argmin(axis=1).astype(np.int32)
            for bank in range(bank_count):
                if not np.any(updated == bank):
                    donor = int(np.argmax(np.min(costs, axis=1)))
                    updated[donor] = bank
            if np.array_equal(updated, assignments):
                break
            assignments = updated
        palettes = [
            palette_for_histogram(
                unique_words,
                unique_lab,
                histograms[assignments == bank].sum(axis=0),
            )
            for bank in range(bank_count)
        ]
        costs = palette_costs(palettes, unique_words, unique_lab, histograms)
        objective = float(sum(costs[index, bank] for index, bank in enumerate(assignments)))
        if best is None or objective < best[2]:
            best = assignments.copy(), palettes, objective
    assert best is not None
    return best


def reconstruct(
    texture: Texture,
    components: list[np.ndarray],
    assignments: np.ndarray,
    palettes: list[np.ndarray],
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    bank_map = np.full((texture.height, texture.width), -1, dtype=np.int8)
    for component, bank in zip(components, assignments, strict=True):
        bank_map[component] = bank
    covered = bank_map >= 0
    indices = np.zeros((texture.height, texture.width), dtype=np.uint8)
    output = np.zeros_like(texture.rgba)
    output[..., 3] = texture.rgba[..., 3]
    source_lab = srgb_to_lab(texture.rgba[..., :3])
    for bank, palette in enumerate(palettes):
        mask = bank_map == bank
        if not mask.any():
            continue
        palette_lab = srgb_to_lab(rgb555_to_rgb8(palette))
        distances = np.sum((source_lab[mask, None, :] - palette_lab[None, :, :]) ** 2, axis=2)
        selected = distances.argmin(axis=1).astype(np.uint8)
        indices[mask] = selected + 1
        output[mask, :3] = rgb555_to_rgb8(palette[selected])
    transparent = texture.rgba[..., 3] == 0
    indices[transparent] = 0
    output[transparent] = 0
    unused = ~covered & ~transparent
    if unused.any():
        palette = palettes[0]
        palette_lab = srgb_to_lab(rgb555_to_rgb8(palette))
        selected = np.argmin(
            np.sum((source_lab[unused, None, :] - palette_lab[None, :, :]) ** 2, axis=2), axis=1
        ).astype(np.uint8)
        indices[unused] = selected + 1
        output[unused, :3] = rgb555_to_rgb8(palette[selected])
    return output, indices, bank_map


def local_ssim(source_l: np.ndarray, output_l: np.ndarray, mask: np.ndarray) -> float:
    scores = []
    c1 = (0.01 * 100.0) ** 2
    c2 = (0.03 * 100.0) ** 2
    for y in range(0, source_l.shape[0], 8):
        for x in range(0, source_l.shape[1], 8):
            window = mask[y : y + 8, x : x + 8]
            if int(window.sum()) < 16:
                continue
            left = source_l[y : y + 8, x : x + 8][window]
            right = output_l[y : y + 8, x : x + 8][window]
            left_mean = float(left.mean())
            right_mean = float(right.mean())
            left_var = float(left.var())
            right_var = float(right.var())
            covariance = float(np.mean((left - left_mean) * (right - right_mean)))
            numerator = (2 * left_mean * right_mean + c1) * (2 * covariance + c2)
            denominator = (left_mean**2 + right_mean**2 + c1) * (left_var + right_var + c2)
            scores.append(numerator / denominator if denominator else 1.0)
    return float(np.mean(scores)) if scores else 1.0


def edge_correlation(source_l: np.ndarray, output_l: np.ndarray, mask: np.ndarray) -> float:
    pairs = []
    for dy, dx in ((0, 1), (1, 0)):
        left_mask = mask[: mask.shape[0] - dy or None, : mask.shape[1] - dx or None]
        right_mask = mask[dy:, dx:]
        valid = left_mask & right_mask
        if valid.any():
            left_source = source_l[: source_l.shape[0] - dy or None, : source_l.shape[1] - dx or None]
            right_source = source_l[dy:, dx:]
            left_output = output_l[: output_l.shape[0] - dy or None, : output_l.shape[1] - dx or None]
            right_output = output_l[dy:, dx:]
            pairs.append((np.abs(right_source - left_source)[valid], np.abs(right_output - left_output)[valid]))
    if not pairs:
        return 1.0
    source_edges = np.concatenate([pair[0] for pair in pairs])
    output_edges = np.concatenate([pair[1] for pair in pairs])
    if source_edges.std() < 1e-9 or output_edges.std() < 1e-9:
        return 1.0 if np.allclose(source_edges, output_edges) else 0.0
    return float(np.corrcoef(source_edges, output_edges)[0, 1])


def metrics(texture: Texture, output: np.ndarray, bank_map: np.ndarray) -> dict[str, float]:
    mask = (bank_map >= 0) & (texture.rgba[..., 3] != 0)
    source_rgb = texture.rgba[..., :3]
    output_rgb = output[..., :3]
    source_lab = srgb_to_lab(source_rgb)
    output_lab = srgb_to_lab(output_rgb)
    delta_e = np.sqrt(np.sum((source_lab[mask] - output_lab[mask]) ** 2, axis=1))
    hue = np.degrees(np.arctan2(source_lab[..., 2], source_lab[..., 1])) % 360.0
    chroma = np.sqrt(source_lab[..., 1] ** 2 + source_lab[..., 2] ** 2)
    brown_mask = mask & (hue >= 20.0) & (hue <= 100.0) & (chroma >= 6.0) & (source_lab[..., 0] <= 75.0)
    brown_delta_e = np.sqrt(
        np.sum((source_lab[brown_mask] - output_lab[brown_mask]) ** 2, axis=1)
    )
    error = source_rgb[mask].astype(np.float64) - output_rgb[mask].astype(np.float64)
    mse = float(np.mean(error**2))
    source_l = source_lab[..., 0]
    output_l = output_lab[..., 0]
    return {
        "covered_texels": float(mask.sum()),
        "mean_delta_e76": float(delta_e.mean()),
        "p95_delta_e76": float(np.percentile(delta_e, 95)),
        "near_jnd_percent": float(np.mean(delta_e <= 2.3) * 100.0),
        "delta_e_le_5_percent": float(np.mean(delta_e <= 5.0) * 100.0),
        "rgb_psnr_db": float(99.0 if mse == 0 else 10.0 * math.log10(255.0**2 / mse)),
        "local_luma_ssim": local_ssim(source_l, output_l, mask),
        "edge_correlation": edge_correlation(source_l, output_l, mask),
        "brown_texels": float(brown_mask.sum()),
        "brown_mean_delta_e76": float(brown_delta_e.mean()) if len(brown_delta_e) else 0.0,
        "brown_p95_delta_e76": float(np.percentile(brown_delta_e, 95)) if len(brown_delta_e) else 0.0,
        "brown_near_jnd_percent": float(np.mean(brown_delta_e <= 2.3) * 100.0)
        if len(brown_delta_e)
        else 100.0,
    }


def pack_4bpp(indices: np.ndarray) -> bytes:
    height, width = indices.shape
    row_bytes = (width + 3) // 4 * 2
    packed = np.zeros((height, row_bytes), dtype=np.uint8)
    for x in range(width):
        packed[:, x // 2] |= indices[:, x] << (4 * (x & 1))
    return packed.tobytes()


def write_psxt(
    path: Path, texture: Texture, indices: np.ndarray, palettes: list[np.ndarray]
) -> None:
    pixels = pack_4bpp(indices)
    palette_words = np.concatenate(
        [np.concatenate((np.asarray([0], dtype=np.uint16), palette)) for palette in palettes]
    ).astype("<u2")
    clut = palette_words.tobytes()
    payload_len = TEXTURE_HEADER_SIZE + len(pixels) + len(clut)
    header = struct.pack("<4sHHI", b"PSXT", 1, texture.flags | 1, payload_len)
    texture_header = struct.pack(
        "<BBHHHII",
        4,
        0,
        texture.width,
        texture.height,
        len(palette_words),
        len(pixels),
        len(clut),
    )
    path.write_bytes(header + texture_header + pixels + clut)


def write_model_face_banks(path: Path, source_path: Path, face_banks: list[int]) -> None:
    data = bytearray(source_path.read_bytes())
    face_count = read_u16(data, ASSET_HEADER_SIZE + 6)
    if face_count != len(face_banks):
        raise ValueError("model face count changed while assigning palette banks")
    flags = read_u16(data, 6)
    existing_bank_bytes = (face_count + 3) // 4 if flags & (1 << 5) else 0
    if existing_bank_bytes:
        del data[-existing_bank_bytes:]
    packed = bytearray((face_count + 3) // 4)
    for face_index, bank in enumerate(face_banks):
        packed[face_index // 4] |= (bank & 3) << ((face_index & 3) * 2)
    data[4:6] = struct.pack("<H", 5)
    data[6:8] = struct.pack("<H", flags | (1 << 5))
    data.extend(packed)
    data[8:12] = struct.pack("<I", len(data) - ASSET_HEADER_SIZE)
    path.write_bytes(data)


def write_banked_model(
    path: Path, source_path: Path, face_component: list[int], assignments: np.ndarray
) -> None:
    write_model_face_banks(
        path,
        source_path,
        [int(assignments[component_index]) for component_index in face_component],
    )


def decode_model_face_banks(path: Path, face_count: int) -> list[int]:
    data = path.read_bytes()
    flags = read_u16(data, 6)
    if flags & (1 << 5) == 0:
        return [0] * face_count
    packed_bytes = (face_count + 3) // 4
    packed = data[-packed_bytes:]
    return [(packed[index // 4] >> ((index & 3) * 2)) & 3 for index in range(face_count)]


def reuse_face_bank_layout(
    layout_model: Path,
    target_model: Path,
    output_model: Path,
    width: int,
    height: int,
) -> None:
    _, _, layout_faces = decode_model(layout_model)
    layout_banks = decode_model_face_banks(layout_model, len(layout_faces))
    layout_map = np.full((height, width), -1, dtype=np.int8)
    conflicts = 0
    for face, bank in zip(layout_faces, layout_banks, strict=True):
        mask = triangle_mask(face, width, height)
        conflicts += int(np.sum(mask & (layout_map >= 0) & (layout_map != bank)))
        layout_map[mask & (layout_map < 0)] = bank

    _, _, target_faces = decode_model(target_model)
    target_banks = []
    ambiguous_faces = 0
    for face in target_faces:
        mask = triangle_mask(face, width, height)
        covered = layout_map[mask & (layout_map >= 0)]
        if len(covered) == 0:
            target_banks.append(0)
            continue
        counts = np.bincount(covered, minlength=4)
        if np.count_nonzero(counts) > 1:
            ambiguous_faces += 1
        target_banks.append(int(np.argmax(counts)))
    write_model_face_banks(output_model, target_model, target_banks)
    print(
        f"reused layout faces={len(target_faces)} source_conflict_texels={conflicts} "
        f"ambiguous_target_faces={ambiguous_faces} banks={max(target_banks, default=0) + 1}"
    )


def checker_composite(rgba: np.ndarray) -> Image.Image:
    height, width = rgba.shape[:2]
    y, x = np.indices((height, width))
    checker = np.where(((x // 8 + y // 8) & 1)[..., None] == 0, 64, 96).astype(np.uint8)
    checker = np.repeat(checker, 3, axis=2)
    alpha = rgba[..., 3:4].astype(np.float64) / 255.0
    rgb = (rgba[..., :3] * alpha + checker * (1.0 - alpha)).astype(np.uint8)
    return Image.fromarray(rgb, "RGB")


def labelled_panel(image: Image.Image, label: str, scale: int = 4) -> Image.Image:
    image = image.resize((image.width * scale, image.height * scale), Image.Resampling.NEAREST)
    panel = Image.new("RGB", (image.width, image.height + 42), (22, 24, 28))
    panel.paste(image, (0, 42))
    ImageDraw.Draw(panel).text((12, 13), label, fill=(238, 240, 244), font=ImageFont.load_default())
    return panel


def save_comparison(
    path: Path,
    texture: Texture,
    candidates: dict[int, tuple[np.ndarray, np.ndarray, np.ndarray, dict[str, float]]],
    baseline: tuple[np.ndarray, np.ndarray, dict[str, float]] | None,
) -> None:
    panels = [labelled_panel(checker_composite(texture.rgba), "Original 8bpp (256 colours)")]
    if baseline is not None:
        baseline_output, baseline_bank_map, baseline_result = baseline
        display = baseline_output.copy()
        unused = baseline_bank_map < 0
        display[unused, :3] = ((texture.rgba[unused, :3].astype(np.uint16) + 96) // 2).astype(np.uint8)
        label = (
            f"Current 4bpp: mean dE {baseline_result['mean_delta_e76']:.2f}, "
            f"brown {baseline_result['brown_mean_delta_e76']:.2f}"
        )
        panels.append(labelled_panel(checker_composite(display), label))
    for bank_count in sorted(candidates):
        output, _, bank_map, result = candidates[bank_count]
        display = output.copy()
        unused = bank_map < 0
        display[unused, :3] = ((texture.rgba[unused, :3].astype(np.uint16) + 96) // 2).astype(np.uint8)
        display[unused, 3] = texture.rgba[unused, 3]
        label = (
            f"{bank_count} bank{'s' if bank_count != 1 else ''}: "
            f"mean dE {result['mean_delta_e76']:.2f}, brown {result['brown_mean_delta_e76']:.2f}"
        )
        panels.append(labelled_panel(checker_composite(display), label))
    width = sum(panel.width for panel in panels)
    sheet = Image.new("RGB", (width, max(panel.height for panel in panels)), (12, 14, 18))
    x = 0
    for panel in panels:
        sheet.paste(panel, (x, 0))
        x += panel.width
    sheet.save(path)


def save_bank_map(path: Path, bank_map: np.ndarray, bank_count: int) -> None:
    colours = np.asarray(
        [[58, 127, 214], [224, 154, 55], [93, 190, 105], [201, 77, 107]], dtype=np.uint8
    )
    image = np.full((*bank_map.shape, 3), 32, dtype=np.uint8)
    for bank in range(bank_count):
        image[bank_map == bank] = colours[bank]
    Image.fromarray(image, "RGB").resize(
        (bank_map.shape[1] * 4, bank_map.shape[0] * 4), Image.Resampling.NEAREST
    ).save(path)


def save_focused_comparison(
    path: Path,
    texture: Texture,
    candidate: tuple[np.ndarray, np.ndarray, np.ndarray, dict[str, float]],
    baseline: tuple[np.ndarray, np.ndarray, dict[str, float]] | None,
) -> None:
    output, _, bank_map, result = candidate
    panels = [labelled_panel(checker_composite(texture.rgba), "8bpp reference", 6)]
    if baseline is not None:
        baseline_output, _, baseline_result = baseline
        panels.append(
            labelled_panel(
                checker_composite(baseline_output),
                f"Current 4bpp | brown dE {baseline_result['brown_mean_delta_e76']:.2f}",
                6,
            )
        )
    panels.append(
        labelled_panel(
            checker_composite(output),
            f"4-bank 4bpp | brown dE {result['brown_mean_delta_e76']:.2f}",
            6,
        )
    )
    sheet = Image.new(
        "RGB", (sum(panel.width for panel in panels), max(panel.height for panel in panels)), (12, 14, 18)
    )
    x = 0
    for panel in panels:
        sheet.paste(panel, (x, 0))
        x += panel.width
    sheet.save(path)

    source_lab = srgb_to_lab(texture.rgba[..., :3])
    output_lab = srgb_to_lab(output[..., :3])
    error = np.sqrt(np.sum((source_lab - output_lab) ** 2, axis=2))
    heat = np.zeros((texture.height, texture.width, 3), dtype=np.uint8)
    used = (bank_map >= 0) & (texture.rgba[..., 3] != 0)
    scaled = np.clip(error / 12.0, 0.0, 1.0)
    heat[..., 0] = (scaled * 255).astype(np.uint8)
    heat[..., 1] = ((1.0 - scaled) * 180).astype(np.uint8)
    heat[..., 2] = ((1.0 - scaled) * 64).astype(np.uint8)
    heat[~used] = 24
    Image.fromarray(heat, "RGB").resize(
        (texture.width * 6, texture.height * 6), Image.Resampling.NEAREST
    ).save(path.with_name("mantis-4-bank-error-map.png"))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--texture", required=True, type=Path, help="8bpp reference PSXT")
    parser.add_argument("--model", required=True, type=Path, help="PSMD model using the atlas")
    parser.add_argument(
        "--additional-model",
        action="append",
        default=[],
        type=Path,
        help="another PSMD model that must share the same atlas and palette banks",
    )
    parser.add_argument("--baseline", type=Path, help="existing 4bpp PSXT to score beside candidates")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--max-banks", type=int, default=4, choices=range(1, 5))
    parser.add_argument("--reuse-layout-model", type=Path)
    parser.add_argument("--output-model", type=Path)
    parser.add_argument(
        "--additional-output-model",
        action="append",
        default=[],
        type=Path,
        help="banked output matching each --additional-model, in the same order",
    )
    parser.add_argument(
        "--unbalanced",
        action="store_true",
        help="disable perceptual hue/lightness cell balancing during optimisation",
    )
    parser.add_argument(
        "--brown-priority",
        type=float,
        default=1.0,
        help="extra optimisation weight for warm-dark texels; metrics remain unweighted",
    )
    args = parser.parse_args()

    texture = decode_psxt(args.texture)
    if len(args.additional_output_model) not in (0, len(args.additional_model)):
        raise ValueError(
            "provide one --additional-output-model per --additional-model, or none"
        )
    model_paths = [args.model, *args.additional_model]
    output_model_paths: list[Path | None] = [args.output_model]
    if args.additional_output_model:
        output_model_paths.extend(args.additional_output_model)
    else:
        output_model_paths.extend([None] * len(args.additional_model))

    faces: list[Face] = []
    face_ranges: list[tuple[int, int]] = []
    vertex_offset = 0
    for model_path in model_paths:
        model_width, model_height, model_faces = decode_model(model_path)
        if (model_width, model_height) != (texture.width, texture.height):
            raise ValueError(f"{model_path}: model and texture dimensions do not match")
        first_face = len(faces)
        faces.extend(
            Face(
                tuple(vertex + vertex_offset for vertex in face.vertices),
                face.uvs,
            )
            for face in model_faces
        )
        face_ranges.append((first_face, len(faces)))
        vertex_offset += max(
            (vertex for face in model_faces for vertex in face.vertices), default=-1
        ) + 1
    if args.reuse_layout_model is not None:
        if args.additional_model:
            raise ValueError("--reuse-layout-model does not accept --additional-model")
        if args.output_model is None:
            raise ValueError("--reuse-layout-model requires --output-model")
        reuse_face_bank_layout(
            args.reuse_layout_model,
            args.model,
            args.output_model,
            texture.width,
            texture.height,
        )
        return
    components, face_component = face_components(faces, texture.width, texture.height)
    unique_words, unique_lab, histograms, features = component_histograms(texture, components)
    optimisation_histograms = (
        histograms if args.unbalanced else balance_perceptual_cells(histograms, unique_lab)
    )
    if args.brown_priority <= 0:
        raise ValueError("--brown-priority must be positive")
    hue = np.degrees(np.arctan2(unique_lab[:, 2], unique_lab[:, 1])) % 360.0
    chroma = np.sqrt(unique_lab[:, 1] ** 2 + unique_lab[:, 2] ** 2)
    brown_colours = (
        (hue >= 20.0)
        & (hue <= 100.0)
        & (chroma >= 6.0)
        & (unique_lab[:, 0] <= 75.0)
    )
    optimisation_histograms[:, brown_colours] *= args.brown_priority
    args.output_dir.mkdir(parents=True, exist_ok=True)

    coverage_map = np.full((texture.height, texture.width), -1, dtype=np.int8)
    for component in components:
        coverage_map[component] = 0
    baseline = None
    if args.baseline is not None:
        baseline_texture = decode_psxt(args.baseline)
        if (baseline_texture.width, baseline_texture.height) != (texture.width, texture.height):
            raise ValueError("baseline and reference texture dimensions do not match")
        baseline_result = metrics(texture, baseline_texture.rgba, coverage_map)
        baseline_result.update(
            {
                "banks": 0.0,
                "components": float(len(components)),
                "estimated_psxt_bytes": float(args.baseline.stat().st_size),
                "objective": -1.0,
            }
        )
        baseline = baseline_texture.rgba, coverage_map, baseline_result

    candidates: dict[int, tuple[np.ndarray, np.ndarray, np.ndarray, dict[str, float]]] = {}
    rows = []
    for bank_count in range(1, args.max_banks + 1):
        assignments, palettes, objective = optimise_banks(
            bank_count, unique_words, unique_lab, optimisation_histograms, features
        )
        output, indices, bank_map = reconstruct(texture, components, assignments, palettes)
        result = metrics(texture, output, bank_map)
        result.update(
            {
                "banks": float(bank_count),
                "components": float(len(components)),
                "estimated_psxt_bytes": float(ASSET_HEADER_SIZE + TEXTURE_HEADER_SIZE + len(pack_4bpp(indices)) + 32 * bank_count),
                "objective": objective,
            }
        )
        candidates[bank_count] = output, indices, bank_map, result
        rows.append(result)
        checker_composite(output).resize(
            (texture.width * 4, texture.height * 4), Image.Resampling.NEAREST
        ).save(args.output_dir / f"mantis-{bank_count}-bank-reconstruction.png")
        save_bank_map(args.output_dir / f"mantis-{bank_count}-bank-map.png", bank_map, bank_count)
        write_psxt(
            args.output_dir / f"mantis-{bank_count}-bank-candidate.psxt",
            texture,
            indices,
            palettes,
        )
        primary_first, primary_end = face_ranges[0]
        write_banked_model(
            args.output_dir / f"mantis-{bank_count}-bank-candidate.psxmdl",
            args.model,
            face_component[primary_first:primary_end],
            assignments,
        )
        if bank_count == args.max_banks:
            for model_path, output_model_path, (first_face, end_face) in zip(
                model_paths, output_model_paths, face_ranges, strict=True
            ):
                if output_model_path is None:
                    continue
                write_banked_model(
                    output_model_path,
                    model_path,
                    face_component[first_face:end_face],
                    assignments,
                )

    save_comparison(
        args.output_dir / "mantis-palette-bank-comparison.png", texture, candidates, baseline
    )
    save_focused_comparison(
        args.output_dir / "mantis-atlas-focused-comparison.png",
        texture,
        candidates[args.max_banks],
        baseline,
    )
    with (args.output_dir / "mantis-palette-bank-metrics.csv").open("w", newline="") as file:
        output_rows = ([baseline[2]] if baseline is not None else []) + rows
        writer = csv.DictWriter(file, fieldnames=list(output_rows[0].keys()))
        writer.writeheader()
        writer.writerows(output_rows)
    print(
        f"models={len(model_paths)} faces={len(faces)} components={len(components)} "
        f"covered={int(rows[0]['covered_texels'])}"
    )
    for row in rows:
        print(
            f"banks={int(row['banks'])} bytes={int(row['estimated_psxt_bytes'])} "
            f"mean_dE76={row['mean_delta_e76']:.3f} p95={row['p95_delta_e76']:.3f} "
            f"dE<=2.3={row['near_jnd_percent']:.1f}% dE<=5={row['delta_e_le_5_percent']:.1f}% "
            f"PSNR={row['rgb_psnr_db']:.2f}dB SSIM={row['local_luma_ssim']:.4f} "
            f"edge_corr={row['edge_correlation']:.4f} brown_mean={row['brown_mean_delta_e76']:.3f} "
            f"brown_p95={row['brown_p95_delta_e76']:.3f}"
        )
    if baseline is not None:
        row = baseline[2]
        print(
            f"baseline bytes={int(row['estimated_psxt_bytes'])} mean_dE76={row['mean_delta_e76']:.3f} "
            f"p95={row['p95_delta_e76']:.3f} brown_mean={row['brown_mean_delta_e76']:.3f} "
            f"brown_p95={row['brown_p95_delta_e76']:.3f}"
        )


if __name__ == "__main__":
    main()
