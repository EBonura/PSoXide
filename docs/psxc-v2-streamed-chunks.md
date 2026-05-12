# PSXC V2 Streamed Room Chunks

`.psxc` is the CD-streamable room chunk container. Version 2 has one job:
keep the runtime collision payload and render payloads separate in the file
and in the runtime reader.

There is no V1 compatibility path. Cooked play/build output should be rebuilt
when this format changes.

## Header

The header is 64 bytes.

| Offset | Field | Meaning |
|---:|---|---|
| 0 | magic | `PSXCHNK\0` |
| 8 | version | `2` |
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
| 52 | reserved | zero |
| 56 | reserved | zero |
| 60 | flags | payload format flags |

## Payloads

The render path reads only:

- `LevelCachedRoomCellRecord`
- `LevelCachedRoomVertexRecord`
- `LevelCachedRoomSurfaceRecord`

The collision path reads only the collision payload range. Today that payload
is a stripped `.psxw` room blob: it keeps the exact sector, wall, and horizontal
override collision data, but drops the render-only static surface-light table.
It is flagged with `STREAMED_ROOM_CHUNK_FLAG_COLLISION_PSXW` and
`STREAMED_ROOM_CHUNK_FLAG_COLLISION_STRIPPED_LIGHTS`.

The point of V2 is that the collision payload can become an even smaller
collision-only blob later without changing render cache readers.

## Cook Report

`cook-playtest` prints streamed memory totals split by:

- collision bytes
- render cache bytes
- render cells / vertices / surfaces
- alignment padding
- CD sector padding

This is the budget check for deciding when cooked render data is worth the RAM
or sector cost.
