#!/usr/bin/env python3
"""Plot texture-warping correctness against CPU cost and locate the optimum.

Input is the CSV from `cargo run -p emulator-core --release --example texwarp`.

The correctness axis is mean texel sampling error, measured. The cost axis is
guest cycles per surface, computed from the two costs this repo has actually
measured on cortex (docs/engine-30fps-architecture-2026-07-26.md):

    cycles = prims * CYC_PRIM + unique_verts * CYC_VERT

CYC_PRIM = 1951 is the cortex_v3 per-emitted-primitive cost. CYC_VERT = 159 is
projection per unique vertex. Both are measured, not modelled. A second cost
scenario uses CYC_PRIM = 400, the per-primitive cost that doc argues a
template-patched emission path should reach, because it moves the optimum.

Three answers come out:
  * the Pareto frontier (nothing cheaper is also more correct)
  * the knee of that frontier, the budget-free optimum
  * the best point that actually fits cortex's per-surface frame budget

Usage: tools/texwarp_chart.py [results.csv] [out.png]
"""

import csv
import sys
from collections import defaultdict

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

# --- measured costs (docs/engine-30fps-architecture-2026-07-26.md) -----------
CYC_PRIM_NOW = 1951  # cortex_v3, cycles per emitted primitive
CYC_PRIM_FIXED = 400  # template-patched emission target from the same doc
CYC_VERT = 159  # projection, cycles per unique vertex

# --- budget -----------------------------------------------------------------
# cortex_v1's render mean sits at 850k against a ~915k budget, and the frame
# walks ~88.6 room surfaces. Give the whole render budget to surfaces: that is
# the ceiling, not a target.
RENDER_BUDGET_CYC = 915_000
SURFACES_PER_FRAME = 88.6
BUDGET_PER_SURFACE = RENDER_BUDGET_CYC / SURFACES_PER_FRAME

# Instrument noise floor: a head-on quad, which has zero true warp, still reads
# this much because the PS1 UV DDA truncates. Nothing can score below it.
NOISE_FLOOR = 0.69


def load(path):
    """Average each strategy over the warping scenes (head-on carries no warp)."""
    acc = defaultdict(lambda: defaultdict(float))
    n = defaultdict(int)
    for row in csv.DictReader(open(path)):
        if row["scene"].startswith("floor0"):
            continue
        s = row["strategy"]
        for k in ("prims", "uverts", "mean_texels", "p95_texels", "max_texels"):
            acc[s][k] += float(row[k])
        n[s] += 1
    return {
        s: {k: v / n[s] for k, v in d.items()} | {"name": s} for s, d in acc.items()
    }


def cost(rec, cyc_prim):
    """Guest cycles to draw one surface under a given per-primitive cost."""
    return rec["prims"] * cyc_prim + rec["uverts"] * CYC_VERT


def pareto(points):
    """Points with nothing both cheaper and more correct. Sorted by cost."""
    out, best = [], float("inf")
    for p in sorted(points, key=lambda p: p[0]):
        if p[1] < best:
            best = p[1]
            out.append(p)
    return out


def knee(front):
    """Frontier point furthest from the chord joining its two extremes.

    Both axes are normalised to [0,1] across the frontier first, so the answer
    does not depend on units. Cost is normalised in log space (1k -> 2k cycles
    is the same kind of decision as 10k -> 20k); error stays linear, because a
    texel of error is a texel wherever you are on the curve.
    """
    import math

    if len(front) < 3:
        return front[0]
    lx = [math.log10(p[0]) for p in front]
    ly = [p[1] for p in front]
    sx, sy = (max(lx) - min(lx)) or 1.0, (max(ly) - min(ly)) or 1.0
    nx = [(v - min(lx)) / sx for v in lx]
    ny = [(v - min(ly)) / sy for v in ly]
    x0, y0, x1, y1 = nx[0], ny[0], nx[-1], ny[-1]
    dx, dy = x1 - x0, y1 - y0
    norm = math.hypot(dx, dy) or 1.0
    dists = [abs(dx * (y0 - y) - (x0 - x) * dy) / norm for x, y in zip(nx, ny)]
    return front[dists.index(max(dists))]


def family(name):
    """Colour/marker group, so the plot reads as a comparison of schemes."""
    if name.startswith("adapt"):
        return "adaptive"
    if name.startswith("scr-1x") or name.startswith("obj-1x"):
        return "1D (single axis)"
    if name.startswith("scr-"):
        return "uniform, screen-space"
    if name.startswith("obj-"):
        return "uniform, object-space"
    return "no subdivision"


STYLE = {
    "adaptive": ("#e8543f", "o", 70),
    "uniform, screen-space": ("#2e86c1", "s", 45),
    "uniform, object-space": ("#7d3c98", "^", 45),
    "1D (single axis)": ("#28a745", "D", 40),
    "no subdivision": ("#8a8a8a", "X", 55),
}


def decorate(ax, pts, front, kneept, budget, zoom):
    """Draw one panel. `zoom` restricts to the sub-3-texel decision region."""
    kx, ky, kname = kneept
    lo, hi = (0.7, 3.0) if zoom else (0, 11.5)

    for fam, (colour, marker, size) in STYLE.items():
        sel = [p for p in pts if family(p[2]) == fam]
        if sel:
            ax.scatter(
                [p[0] for p in sel],
                [p[1] for p in sel],
                c=colour,
                marker=marker,
                s=size * (0.8 if zoom else 1.0),
                label=None if zoom else fam,
                alpha=0.85,
                zorder=3,
                edgecolors="none",
            )

    ax.plot(
        [p[0] for p in front],
        [p[1] for p in front],
        "-",
        c="#333",
        lw=1.2,
        alpha=0.55,
        zorder=2,
        label=None if zoom else "Pareto frontier",
    )

    # Label only frontier points, and only those visible in this panel.
    # Alternate the offset side so neighbours on a steep curve do not collide.
    visible = [p for p in front if lo <= p[1] <= hi]
    for i, (x, y, name) in enumerate(visible):
        up = i % 2 == 0
        ax.annotate(
            name,
            (x, y),
            textcoords="offset points",
            xytext=(6, 7 if up else -13),
            fontsize=7 if zoom else 7.5,
            color="#333",
        )

    ax.axhspan(lo, NOISE_FLOOR, color="#bbb", alpha=0.3, zorder=0)
    ax.axvline(BUDGET_PER_SURFACE, color="#c0392b", ls="--", lw=1.4, zorder=1)

    same = budget and abs(budget[0] - kx) < 1e-6
    ax.scatter([kx], [ky], s=260, facecolors="none", edgecolors="#e8543f", lw=2.0, zorder=4)
    if budget and not same:
        ax.scatter(
            [budget[0]], [budget[1]], s=380, facecolors="none",
            edgecolors="#c0392b", lw=2.0, zorder=4,
        )

    if not zoom:
        ax.text(
            BUDGET_PER_SURFACE,
            11.2,
            f" frame budget\n {BUDGET_PER_SURFACE:,.0f} cyc/surface",
            color="#c0392b",
            fontsize=8,
            va="top",
        )
        ax.set_xlabel("CPU cost, guest cycles per surface  (log)")
    else:
        label = (
            f"knee = best in budget:\n{kname}"
            if same
            else f"knee: {kname}\nbest in budget: {budget[2]}"
        )
        ax.annotate(
            label,
            (kx, ky),
            textcoords="offset points",
            xytext=(14, 30),
            fontsize=8.5,
            fontweight="bold",
            color="#c0392b",
            ha="left",
            arrowprops=dict(arrowstyle="->", color="#c0392b", lw=1.2),
        )
        ax.tick_params(labelsize=7)
        ax.patch.set_alpha(0.97)

    ax.set_xscale("log")
    ax.set_ylim(lo, hi)
    ax.grid(alpha=0.22, which="both")
    if zoom:
        # The 16x16 grids sit two decades right and are never a decision;
        # cropping them lets the frontier labels breathe.
        ax.set_xlim(min(p[0] for p in pts) * 0.55, BUDGET_PER_SURFACE * 12)
    else:
        ax.set_xlim(min(p[0] for p in pts) * 0.7, max(p[0] for p in pts) * 2.2)


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "/tmp/texwarp/results.csv"
    out = sys.argv[2] if len(sys.argv) > 2 else "/tmp/texwarp/tradeoff.png"
    recs = load(src)

    fig, axes = plt.subplots(1, 2, figsize=(16, 7.5), sharey=True)
    summary = {}

    for ax, (cyc_prim, title) in zip(
        axes,
        [
            (CYC_PRIM_NOW, f"cortex today ({CYC_PRIM_NOW} cyc/primitive)"),
            (CYC_PRIM_FIXED, f"with template emission ({CYC_PRIM_FIXED} cyc/primitive)"),
        ],
    ):
        pts = [(cost(r, cyc_prim), r["mean_texels"], r["name"]) for r in recs.values()]
        front = pareto(pts)
        kx, ky, kname = knee(front)

        affordable = [p for p in pts if p[0] <= BUDGET_PER_SURFACE]
        best_in_budget = min(affordable, key=lambda p: p[1]) if affordable else None

        decorate(ax, pts, front, (kx, ky, kname), best_in_budget, zoom=False)
        ax.set_title(title, fontsize=11)

        # The whole decision happens in the bottom 3 texels, where the linear
        # axis crushes eight frontier points together. Zoom it.
        inset = ax.inset_axes([0.30, 0.28, 0.67, 0.44])
        decorate(inset, pts, front, (kx, ky, kname), best_in_budget, zoom=True)

        summary[title] = (front, (kx, ky, kname), best_in_budget)

    axes[0].set_ylabel("texture error, mean texels  (lower is better)")
    axes[0].set_ylim(0, 11.5)
    axes[0].legend(loc="upper left", fontsize=8, framealpha=0.95)
    fig.suptitle(
        "PS1 texture warping: correctness vs CPU cost, measured\n"
        "insets zoom the sub-3-texel region where every real choice lives",
        fontsize=13,
        fontweight="bold",
    )
    fig.subplots_adjust(left=0.06, right=0.99, top=0.85, bottom=0.09, wspace=0.06)
    fig.savefig(out, dpi=150)
    print(f"chart: {out}\n")

    for title, (front, k, budget) in summary.items():
        print(f"=== {title} ===")
        print(f"{'strategy':<18}{'cyc/surface':>13}{'mean texels':>13}")
        for x, y, name in front:
            print(f"{name:<18}{x:>13,.0f}{y:>13.2f}")
        print(f"knee            : {k[2]}  ({k[0]:,.0f} cyc, {k[1]:.2f} texels)")
        if budget:
            print(
                f"best in budget  : {budget[2]}  ({budget[0]:,.0f} cyc, "
                f"{budget[1]:.2f} texels)"
            )
        print()


if __name__ == "__main__":
    main()
