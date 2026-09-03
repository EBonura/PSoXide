#!/usr/bin/env python3
"""Decimate a cooked PSMD model in place of its source: collapse short edges
inside each rigid part, never across joints or texture seams, and rewrite the
vertex, face and palette-bank tables. Header, joints, materials, quantisation
scale and flags are untouched, so the model's clips, atlas and colours keep
working. Meant for the in-game before/after Manny asked for; a proper tool
would weigh error with quadrics.

Usage: psmd_decimate.py in.psxmdl out.psxmdl [--ratio 0.85] [--uv-tolerance 6] [--blend-threshold 64]
"""
import argparse, math, struct, sys
from pathlib import Path

ASSET_HEADER = 12
MODEL_HEADER = 16
JOINT = 4
MATERIAL = 8
PART = 16
VERTEX = 8
FACE = 12
FLAG_FACE_PALETTE_BANKS = None  # resolved from the file: banks table present iff length matches


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("source", type=Path)
    ap.add_argument("output", type=Path)
    ap.add_argument("--ratio", type=float, default=0.85, help="target faces per part as a fraction")
    ap.add_argument("--uv-tolerance", type=int, default=6, help="max texel distance between merged corners")
    ap.add_argument("--blend-threshold", type=int, default=0,
                    help="snap vertices whose secondary-bone weight (0..255) is at or below this to a single bone")
    a = ap.parse_args()
    data = bytearray(a.source.read_bytes())
    if data[:4] != b"PSMD":
        sys.exit("not a PSMD model")
    jc, pc, vc, fc, mc, tw, th, l2w = struct.unpack_from("<8H", data, ASSET_HEADER)
    off = ASSET_HEADER + MODEL_HEADER
    joints = bytes(data[off:off + jc * JOINT]); off += jc * JOINT
    materials = bytes(data[off:off + mc * MATERIAL]); off += mc * MATERIAL
    parts = [list(struct.unpack_from("<6H", data, off + i * PART)) + [bytes(data[off + i * PART + 12:off + (i + 1) * PART])] for i in range(pc)]
    off += pc * PART
    verts = [list(struct.unpack_from("<3hBB", data, off + i * VERTEX)) for i in range(vc)]; off += vc * VERTEX
    faces = [list(struct.unpack_from("<HBBHBBHBB", data, off + i * FACE)) for i in range(fc)]; off += fc * FACE
    bank_bytes = (fc + 3) // 4
    banks = None
    if len(data) - off == bank_bytes:
        packed = data[off:off + bank_bytes]
        banks = [(packed[i // 4] >> ((i & 3) * 2)) & 3 for i in range(fc)]
    elif len(data) != off:
        sys.exit(f"unexpected trailing {len(data) - off} bytes")

    def face_idx(f): return (f[0], f[3], f[6])
    def face_uv(f): return ((f[1], f[2]), (f[4], f[5]), (f[7], f[8]))

    total_before = sum(p[4] for p in parts)
    blended_before = sum(1 for v in verts if v[4] != 0 and v[3] != 255)
    if a.blend_threshold:
        for v in verts:
            if v[4] != 0 and v[3] != 255 and v[4] <= a.blend_threshold:
                v[3], v[4] = 255, 0
    blended_after = sum(1 for v in verts if v[4] != 0 and v[3] != 255)
    # Skinned models let a part's faces reach seam vertices owned by another
    # part, so positions live in one global table; only vertices a part owns
    # (same joint, same blend record) may collapse into each other.
    verts = [v[:] for v in verts]
    dead = set()
    part_faces = []
    for p in parts:
        joint, fv, pvc, ff, pfc, mat, pad = p
        owned = range(fv, fv + pvc)
        pfaces = [(faces[i][:], banks[i] if banks else 0) for i in range(ff, ff + pfc)]
        target = math.ceil(pfc * a.ratio)

        def uvs_at(v):
            out = set()
            for f, _ in pfaces:
                idx = face_idx(f); uv = face_uv(f)
                for k in range(3):
                    if idx[k] == v:
                        out.add(uv[k])
            return out

        def live_faces():
            return [(f, b) for f, b in pfaces if len(set(face_idx(f))) == 3]

        while len(live_faces()) > target:
            best = None
            for f, _ in live_faces():
                idx = face_idx(f)
                for k in range(3):
                    va, vb = idx[k], idx[(k + 1) % 3]
                    if va == vb or va not in owned or vb not in owned: continue
                    A, B = verts[va], verts[vb]
                    if (A[3], A[4]) != (B[3], B[4]): continue
                    d = (A[0] - B[0]) ** 2 + (A[1] - B[1]) ** 2 + (A[2] - B[2]) ** 2
                    if best is not None and d >= best[0]: continue
                    ua, ub = uvs_at(va), uvs_at(vb)
                    ok = all(any(abs(u[0] - w[0]) <= a.uv_tolerance and abs(u[1] - w[1]) <= a.uv_tolerance for w in ua) for u in ub)
                    if ok: best = (d, va, vb)
            if best is None:
                break
            _, va, vb = best
            A, B = verts[va], verts[vb]
            verts[va] = [(A[0] + B[0]) // 2, (A[1] + B[1]) // 2, (A[2] + B[2]) // 2, A[3], A[4]]
            for f, _ in pfaces:
                for k in (0, 3, 6):
                    if f[k] == vb: f[k] = va
            dead.add(vb)
        part_faces.append([(f, b) for f, b in pfaces if len(set(face_idx(f))) == 3])
    # Every vertex still referenced by any face survives; a collapsed vertex
    # may also be dropped if nothing references it. Part ranges stay
    # contiguous because parts own consecutive ranges and order is preserved.
    referenced = {i for kept in part_faces for f, _ in kept for i in face_idx(f)}
    remap = {}
    new_verts = []
    for i in range(vc):
        if i in referenced or (i not in dead and any(p[1] <= i < p[1] + p[2] for p in parts) and False):
            remap[i] = len(new_verts); new_verts.append(verts[i])
    new_faces, new_banks, new_parts = [], [], []
    for p, kept in zip(parts, part_faces):
        joint, fv, pvc, ff, pfc, mat, pad = p
        owned_alive = [i for i in range(fv, fv + pvc) if i in remap]
        first_v = remap[owned_alive[0]] if owned_alive else len(new_verts)
        first_f = len(new_faces)
        for f, b in kept:
            g = f[:]
            for k in (0, 3, 6): g[k] = remap[g[k]]
            new_faces.append(g); new_banks.append(b)
        new_parts.append([joint, first_v, len(owned_alive), first_f, len(kept), mat, pad])

    out = bytearray(data[:ASSET_HEADER])
    hdr = struct.pack("<8H", jc, len(new_parts), len(new_verts), len(new_faces), mc, tw, th, l2w)
    body = bytearray(hdr) + joints + materials
    for p in new_parts:
        body += struct.pack("<6H", *p[:6]) + p[6]
    for v in new_verts:
        body += struct.pack("<3hBB", *v)
    for f in new_faces:
        body += struct.pack("<HBBHBBHBB", *f)
    if banks is not None:
        packed = bytearray((len(new_faces) + 3) // 4)
        for i, b in enumerate(new_banks):
            packed[i // 4] |= (b & 3) << ((i & 3) * 2)
        body += packed
    struct.pack_into("<I", out, 8, len(body))
    out += body
    a.output.parent.mkdir(parents=True, exist_ok=True)
    a.output.write_bytes(out)
    print(f"{a.source.name}: faces {total_before} -> {len(new_faces)}, vertices {vc} -> {len(new_verts)} (ratio {a.ratio}), blended vertices {blended_before} -> {blended_after} (threshold {a.blend_threshold})")


if __name__ == "__main__":
    main()
