# Tank Boss attack and death retries

Round two keeps `core_showcase_v1` Idle 2 and Hit 2 selected. This folder is
reserved for four replacement attacks and four replacement deaths. The death
prompt pool intentionally contains eight candidates so failed upright endings
can be rejected before the user-facing reel is assembled.

Final death reel sources:

- Death 1: seed `260827`, short-prompt pool C sample 0 (backward collapse)
- Death 2: seed `260828`, final pool F sample 7 (face-first trip)
- Death 3: seed `260828`, final pool G sample 10 (sideways impact)
- Death 4: seed `260826`, first pool B sample 11 (knees-to-topple)

The `death_pool_*` directories retain accepted and rejected generations for
review. Only the four copied into this folder's top-level `glb/`, `renders/`,
and `source_bvh/` participate in the round-two showcase.

Showcase numbering remains fixed: 1 top-left, 2 top-right, 3 bottom-left,
4 bottom-right.

## Final choices

The user selected Attack 1 and Death 2 on 2026-08-22. Together with
`core_showcase_v1` Idle 2 and Hit 2, the Tank Boss core-animation selection is
complete and ready for the engine import pass.
