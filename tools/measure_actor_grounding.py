#!/usr/bin/env python3
"""How far an actor's lowest rendered vertex sits above the floor, in world units.

The character's own texture page identifies its primitives, and the shadow
decal (drawn at the motor's floor point, with corners at +/- its radius)
gives a floor->screen homography, so no camera model is assumed. The vertical
scale comes from the character's known height.

    frontend launch ... --dump-draws draws.csv --counter-log run.log
    python3 tools/measure_actor_grounding.py draws.csv run.log [shadow_radius] [height]
"""
import sys, csv
import numpy as np
from collections import defaultdict
draws_path, log_path = sys.argv[1], sys.argv[2]
r = list(csv.DictReader(open(log_path)))[-1]
B = 1_000_000
px = int(r["player_x_biased"]) - B
pz = int(r["player_z_biased"]) - B
rows = []
for line in open(draws_path):
    p = line.strip().split(",")
    rows.append((int(p[0]), int(p[1], 16), [int(w, 16) for w in p[2:]]))
fills = [i for i, (idx, op, w) in enumerate(rows) if op == 0x02]
draws = [x for x in rows[fills[-1]:] if 0x20 <= x[1] <= 0x7F]
def vtx(w):
    x = w & 0xFFFF; y = (w >> 16) & 0xFFFF
    return (x - 0x10000 if x >= 0x8000 else x, y - 0x10000 if y >= 0x8000 else y)
def decode(op, w):
    shaded = op & 0x10; quad = op & 0x08; tex = op & 0x04
    n = 4 if quad else 3; out = []; i = 1; tp = cl = None
    for k in range(n):
        if k > 0 and shaded: i += 1
        out.append(vtx(w[i])); i += 1
        if tex:
            if k == 0: cl = (w[i] >> 16) & 0xFFFF
            if k == 1: tp = (w[i] >> 16) & 0xFFFF
            i += 1
    return out, tp, cl
g = defaultdict(list)
for idx, op, w in draws:
    vs, tp, cl = decode(op, w); g[(tp, cl)].append(vs)
best = max((k for k, v in g.items() if len(v) > 20
            and min(p[0] for vs in v for p in vs) >= 0
            and max(p[1] for vs in v for p in vs) <= 240),
           key=lambda k: len(g[k]))
char = [p for vs in g[best] for p in vs]
shadow = sorted({p for k, v in g.items() if k[0] == 6 for vs in v for p in vs})
cs = sorted(shadow, key=lambda c: c[1]); far = sorted(cs[:2]); near = sorted(cs[2:])
rr = int(sys.argv[3]) if len(sys.argv) > 3 else 15
world = [(px-rr, pz-rr), (px+rr, pz-rr), (px-rr, pz+rr), (px+rr, pz+rr)]
screen = [far[0], far[1], near[0], near[1]]
A = []
for (X, Z), (u, v) in zip(world, screen):
    A.append([X, Z, 1, 0, 0, 0, -u*X, -u*Z, -u]); A.append([0, 0, 0, X, Z, 1, -v*X, -v*Z, -v])
H = np.linalg.svd(np.array(A, float))[2][-1].reshape(3, 3)
m = lambda X, Z: (lambda p: (p[0]/p[2], p[1]/p[2]))(H @ np.array([X, Z, 1.0]))
fy = m(px, pz)[1]
lo = max(y for _, y in char); hi = min(y for _, y in char)
vert = (float(sys.argv[4]) if len(sys.argv) > 4 else 86.0) / (lo - hi)
print(f"{draws_path}: player=({px},{pz}) anim={r['player_anim_action']} bbox y{hi}..{lo} floor y={fy:.1f}")
print(f"  gap {fy-lo:+.1f}px  => FLOAT {(fy-lo)*vert:+.1f} world units  ({vert:.2f} u/px vertical)")

