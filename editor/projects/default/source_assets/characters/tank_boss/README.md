# Tank Boss source

`Enemy_02.fbx` is the project copy of the artist-delivered boss model. The
delivery calls this character "Tank". `Diffuse_Enemy01_256px.png` is the
matching 256 x 256 source texture; the FBX also embeds it.

Current PSoXide import settings:

- 1536-unit world height
- 128 x 128, 8bpp cooked atlas
- 12 Hz animation sampling
- default detail-bone collapse, retaining 21 joints

The FBX contains one static take named
`Armature_heavy|mixamo_import_raw`. It is useful for the authored rest
transforms, but it must not be retained as a multi-frame clip. Equivalent
endpoint quaternions have opposite signs, so interpolation can cross a zero
quaternion and make the model disappear. The registered `Tank Boss / Rest
Pose` is the first frame of that take only.

Import future boss animations as separate animation sources against this FBX
and keep the cooked clips target-specific to `Tank Boss Model`.
