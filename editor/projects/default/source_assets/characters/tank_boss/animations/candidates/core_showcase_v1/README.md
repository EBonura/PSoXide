# Tank Boss core animation auditions v1

This set contains four numbered candidates for each missing core state:

- `idle_01`–`idle_04`: seamless 4-second loops
- `attack_01`–`attack_04`: one-shot primary attacks with recovery
- `hit_01`–`hit_04`: short hit reactions with recovery
- `death_01`–`death_04`: one-shot collapses with a held final pose

The showcase uses a fixed layout in every category: **1 top-left, 2 top-right,
3 bottom-left, 4 bottom-right**. Candidate GLBs are intentionally not registered
with the Tank Boss character until a take is selected.

## Provenance

Idle, attack, and hit candidates use MoMask seed `260822` and `prompts.txt`.
Death candidates were selected from literal-prompt retries after rejecting takes
that finished upright:

- Death 1: seed `260825`, retry 3 sample 0
- Death 2: seed `260823`, retry 1 sample 0
- Death 3: seed `260824`, retry 2 sample 2
- Death 4: seed `260824`, retry 2 sample 3

Raw IK BVHs are retained in `source_bvh/`, textured retargeted candidates in
`glb/`, and individual review renders in `renders/`. `build_candidates.py` and
`assemble_showcase.py` reproduce the retarget/render and labelled reel stages.
