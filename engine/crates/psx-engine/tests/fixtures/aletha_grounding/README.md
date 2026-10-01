Cooked Aletha (default project) for the `render3d` grounding regression test
`grounding_probe_player_lowest_vertex_matches_reference`.

Both files are `make cook-playtest` output for `editor/projects/default` at
editor c1e2c4bb, copied from `engine/examples/editor-playtest/generated/models/`:

| fixture | cooked as | sha1 |
| --- | --- | --- |
| `aletha_delivered.psxmdl` | `model_003_aletha_delivered/mesh.psxmdl` | `589b0d1554fcb9463e3d58934b0dd58a66af0e66` |
| `aletha_idle.psxanim` | `model_003_aletha_delivered/clip_21_aletha_idle.psxanim` | `6ddf5cd266bccfc10642b23d726fc420e415931a` |

The test used to read the cook output directly, which is gitignored, so it
failed in any checkout that had not cooked first. It checks the GTE vertex
chain against exact f64 math for one pose, so the fixture only needs to be a
real cooked character; refresh it when the cooked model or clip format
changes.
