# Rust Mantis backward-walk auditions v1

Four local MoMask takes are retargeted onto the normalized Rust Mantis rig and
presented as seamless in-place loops. **Candidate 2 was selected** and is bound
to the live Rust Mantis as `WalkBackward`; the other three remain auditions.

The showcase layout is fixed: **1 top-left, 2 top-right, 3 bottom-left,
4 bottom-right**.

## Provenance

| Candidate | Prompt | Seed | Repeat | Source family |
| --- | --- | ---: | ---: | --- |
| 1 | A person walks backwards slowly and carefully. | 811 | 0 | `bk_a` |
| 2 | A person retreats backwards with steady steps. | 833 | 2 | `bk_c` |
| 3 | A person walks backward slowly. | 501 | 2 | `lat_back` |
| 4 | A person walks backwards slowly and carefully. | 811 | 1 | `bk_a` |

`build_candidates.py` copies the selected raw IK BVHs into `source_bvh/`,
removes generated facing drift, retargets them through the established Mantis
bridge, detects the final settled gait cycle, closes its seam, exports GLBs,
and renders six-second review clips. `assemble_showcase.py` produces the
labelled 2x2 comparison video and poster.
