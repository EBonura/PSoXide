#!/usr/bin/env python3
"""Actor float above the floor from a grounding side-view replay, in engine units.

The fixture (gen_grounding_side_view) puts the gameplay camera on the floor
plane looking horizontally, with a 16-unit reference cube beside every actor.
World polygons above the horizon are therefore cube faces: per x cluster, the
cube's bottom edge is the floor line at that depth and (bottom - top) / 16 is
the pixel-per-unit scale. Every textured non-world primitive group (one per
actor texture page + CLUT) is then measured against the nearest cube.

    frontend launch ... --dump-hw frame.ppm --dump-draws draws.csv
    python3 tools/measure_side_view_grounding.py draws.csv [cube_units=16]
"""
import sys
from collections import defaultdict

draws_path = sys.argv[1]
cube_units = float(sys.argv[2]) if len(sys.argv) > 2 else 16.0

rows = []
for line in open(draws_path):
    p = line.strip().split(",")
    rows.append((int(p[0]), int(p[1], 16), [int(w, 16) for w in p[2:]]))
fills = [i for i, (idx, op, w) in enumerate(rows) if op == 0x02]
draws = [x for x in rows[fills[-1]:] if 0x20 <= x[1] <= 0x3F]


def vtx(w):
    x = w & 0xFFFF
    y = (w >> 16) & 0xFFFF
    return (x - 0x10000 if x >= 0x8000 else x, y - 0x10000 if y >= 0x8000 else y)


def decode(op, w):
    shaded = op & 0x10
    quad = op & 0x08
    tex = op & 0x04
    n = 4 if quad else 3
    out = []
    i = 1
    tp = cl = None
    for k in range(n):
        if k > 0 and shaded:
            i += 1
        out.append(vtx(w[i]))
        i += 1
        if tex:
            if k == 0:
                cl = (w[i] >> 16) & 0xFFFF
            if k == 1:
                tp = (w[i] >> 16) & 0xFFFF
            i += 1
    return out, tp, cl


groups = defaultdict(list)
for idx, op, w in draws:
    vs, tp, cl = decode(op, w)
    if tp is None:
        continue
    groups[(tp, cl)].append(vs)

# The world texture page is the group with the most primitives that reach
# below the horizon (the slab) and has cube faces above it.
def span(prims):
    ys = [p[1] for vs in prims for p in vs]
    xs = [p[0] for vs in prims for p in vs]
    return min(xs), max(xs), min(ys), max(ys)

print(f"{'tpage':>6} {'clut':>6} {'prims':>6} {'x-range':>12} {'y-range':>12}")
for key, prims in sorted(groups.items(), key=lambda kv: -len(kv[1])):
    x0, x1, y0, y1 = span(prims)
    print(f"{key[0]:>6x} {key[1]:>6x} {len(prims):>6} {x0:>5}..{x1:<5} {y0:>5}..{y1:<5}")

world_key = max(groups, key=lambda k: sum(1 for vs in groups[k] if max(p[1] for p in vs) >= 120))
world = groups[world_key]
cube_prims = [vs for vs in world if min(p[1] for p in vs) < 116 and max(p[1] for p in vs) <= 240]
clusters = []
for vs in sorted(cube_prims, key=lambda vs: sum(p[0] for p in vs) / len(vs)):
    cx = sum(p[0] for p in vs) / len(vs)
    if clusters and abs(clusters[-1]["cx"] - cx) < 40:
        clusters[-1]["prims"].append(vs)
        clusters[-1]["cx"] = sum(sum(p[0] for p in q) / len(q) for q in clusters[-1]["prims"]) / len(clusters[-1]["prims"])
    else:
        clusters.append({"cx": cx, "prims": [vs]})
print("\nreference cubes (world tpage %x):" % world_key[0])
for c in clusters:
    x0, x1, y0, y1 = span(c["prims"])
    c["top"], c["bottom"] = y0, y1
    c["scale"] = (y1 - y0) / cube_units
    print(f"  x {x0:>4}..{x1:<4} top y={y0:>4} bottom y={y1:>4} -> {c['scale']:.2f} px/unit, floor line y={y1}")

print("\nactors (lowest vertex against the nearest cube):")
for key, prims in sorted(groups.items(), key=lambda kv: -len(kv[1])):
    if key == world_key or len(prims) < 20:
        continue
    x0, x1, y0, y1 = span(prims)
    cx = (x0 + x1) / 2
    if not clusters:
        break
    c = min(clusters, key=lambda c: abs(c["cx"] - cx))
    float_units = (c["bottom"] - y1) / c["scale"]
    print(f"  tpage {key[0]:x} clut {key[1]:x}: {len(prims)} prims, x {x0}..{x1}, lowest y={y1}, "
          f"cube floor y={c['bottom']} -> float {float_units:+.2f} units ({c['bottom'] - y1:+d} px)")
