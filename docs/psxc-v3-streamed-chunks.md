# PSXC V3 Streamed Room Chunks

`.psxc` is the CD-streamable room chunk container. Version 3 keeps runtime
collision data, render surfaces, and optional per-cell render vertex lists
separate in the file and in the runtime reader.

There is no V1 compatibility path. Cooked play/build output should be rebuilt
when this format changes.

## Header

The header is 64 bytes.

| Offset | Field | Meaning |
|---:|---|---|
| 0 | magic | `PSXCHNK\0` |
| 8 | version | `3` |
| 12 | room | generated room/chunk id |
| 16 | total bytes | unpadded payload length |
| 20 | collision offset | start of collision payload |
| 24 | collision bytes | collision payload length |
| 28 | cells offset | cached render-cell table |
| 32 | cell count | number of render-cell records |
| 36 | vertices offset | cached render-vertex table |
| 40 | vertex count | number of render-vertex records |
| 44 | surfaces offset | cached render-surface table |
| 48 | surface count | number of render-surface records |
| 52 | cell vertices offset | `u16` cached vertex-index table grouped by cell |
| 56 | cell vertex count | number of `u16` cell vertex indices |
| 60 | flags | payload format flags |

## Payloads

The render path reads only:

- `LevelCachedRoomCellRecord`
- optional `u16` per-cell cached vertex indices
- `LevelCachedRoomVertexRecord`
- `LevelCachedRoomSurfaceRecord`

Cells always point at their surface range. They may also point at a unique
vertex-index range. When that range is empty, the runtime derives the visible
vertex set from the accepted cell's surfaces, keeping streamed chunks smaller
while still projecting only vertices referenced by visible, frustum-accepted
cells.

The collision path reads only the collision payload range. That payload is the
compact `PSXCOLL\0` collision-only format, flagged with
`STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT`. It stores:

- a 36-byte header with room dimensions, sector size, table counts, and ambient
  RGB for actor lighting
- one 44-byte sector record per grid cell
- one 20-byte wall record per collision wall
- optional 28-byte height-override records only when split-triangle heights
  differ from the corner-derived default

It does not contain materials, UVs, static-light records, render geometry, or
any full `.psxw` payload duplication. Render cache readers consume their own
tables and never parse collision bytes.

## Cook Report

`cook-playtest` prints streamed memory totals split by:

- collision bytes
- render cache bytes
- render cells / cell vertex indices / vertices / surfaces
- alignment padding
- CD sector padding

This is the budget check for deciding when cooked render data is worth the RAM
or sector cost.
