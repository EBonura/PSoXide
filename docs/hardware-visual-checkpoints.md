# Cortex Ignition V1 Hardware Visual Checkpoints

Build with `hardware-boot-visual` to show TV-visible boot checkpoints:

```sh
make cortex-ignition-v1-hardware-diagnostic-disc
```

Each checkpoint clears the framebuffer to the listed RGB color and draws
`CORTEX_IGNITION_V1` plus the message text. When reporting a hardware stop,
record the full message and color in `docs/hardware-burn-ledger.md`; the text is
the stable identifier.

## Engine Spine

| Message | RGB | Expected next | Source |
| --- | --- | --- | --- |
| `01 FRAMEBUFFER READY` | `160,0,0` | `02 CTX READY` | `engine/crates/psx-engine/src/app.rs` |
| `02 CTX READY` | `200,96,0` | `03 APP INIT BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `03 APP INIT BEGIN` | `180,180,0` | `04 MENU AUDIO BEGIN` or scene init work | `engine/crates/psx-engine/src/app.rs` |
| `13 APP INIT OK` | `0,120,0` | `20 UPDATE BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `20 UPDATE BEGIN` | `0,0,180` | `21 FRAME BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `21 FRAME BEGIN` | `220,0,0` | `22 PAD POLL BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `22 PAD POLL BEGIN` | `220,100,0` | `23 PAD POLL OK` | `engine/crates/psx-engine/src/app.rs` |
| `23 PAD POLL OK` | `220,220,0` | `29 UPDATE OK` | `engine/crates/psx-engine/src/app.rs` |
| `29 UPDATE OK` | `0,140,180` | `30 RENDER BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `30 RENDER BEGIN` | `120,0,180` | `31 CLEAR BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `31 CLEAR BEGIN` | `80,0,120` | `32 CLEAR OK` | `engine/crates/psx-engine/src/app.rs` |
| `32 CLEAR OK` | `80,40,160` | `33 SCENE RENDER BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `33 SCENE RENDER BEGIN` | `100,40,180` | GameApp render checkpoints | `engine/crates/psx-engine/src/app.rs` |
| `38 SCENE RENDER RETURNED` | `180,180,255` | `39 RENDER OK` | `engine/crates/psx-engine/src/app.rs` |
| `39 RENDER OK` | `220,220,220` | `40 PRESENT BEGIN` | `engine/crates/psx-engine/src/app.rs` |
| `40 PRESENT BEGIN` | `80,80,80` | `49 PRESENT OK` | `engine/crates/psx-engine/src/app.rs` |
| `49 PRESENT OK` | `0,220,0` | normal running frame loop | `engine/crates/psx-engine/src/app.rs` |

## GameApp Init

| Message | RGB | Expected next | Source |
| --- | --- | --- | --- |
| `04 MENU AUDIO BEGIN` | `255,80,0` | `05 MENU AUDIO OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `05 MENU AUDIO OK` | `255,0,80` | `06 SHARED ASSETS BEGIN` | `engine/crates/psx-engine/src/game_app.rs` |
| `06 SHARED ASSETS BEGIN` | `255,160,0` | `07 SHARED ASSETS OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `07 SHARED ASSETS OK` | `0,200,80` | `08 RESOURCES BEGIN` | `engine/crates/psx-engine/src/game_app.rs` |
| `08 RESOURCES BEGIN` | `255,0,255` | `09 RESOURCES OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `09 RESOURCES OK` | `80,255,80` | `11 OPTIONS BEGIN` or app init return | `engine/crates/psx-engine/src/game_app.rs` |
| `11 OPTIONS BEGIN` | `255,255,255` | `12 OPTIONS OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `12 OPTIONS OK` | `0,255,160` | `13 APP INIT OK` | `engine/crates/psx-engine/src/game_app.rs` |

## GameApp Render Branches

Some numeric prefixes are reused by mutually exclusive render branches. Treat
the whole message as the checkpoint key.

| Message | RGB | Expected next | Source |
| --- | --- | --- | --- |
| `34 GAMEAPP RENDER BEGIN` | `120,40,200` | loading, gameplay, UI, or transition branch | `engine/crates/psx-engine/src/game_app.rs` |
| `35 LOADING RENDER BEGIN` | `140,40,220` | `38 SCENE RENDER RETURNED` after loading render returns | `engine/crates/psx-engine/src/game_app.rs` |
| `35 TAG RESOLVE BEGIN` | `140,80,220` | `36 TAG RESOLVE OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `36 TAG RESOLVE OK` | `160,80,220` | gameplay/UI/transition render branch | `engine/crates/psx-engine/src/game_app.rs` |
| `37 GAMEPLAY RENDER BEGIN` | `180,80,220` | `38 GAMEPLAY RENDER OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `38 GAMEPLAY RENDER OK` | `200,80,220` | UI/transition branch or scene render return | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI RENDER BEGIN` | `200,120,220` | UI range/draw checkpoints | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI RANGE BEGIN` | `40,80,220` | `37 UI RANGE OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI RANGE OK` | `40,120,220` | `37 UI FOCUS OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI FOCUS OK` | `40,160,220` | `37 UI DRAW BEGIN` | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI DRAW BEGIN` | `40,200,220` | `37 UI DRAW OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `37 UI DRAW OK` | `80,220,220` | `38 UI RENDER OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `38 UI RENDER OK` | `220,120,220` | transition branch or scene render return | `engine/crates/psx-engine/src/game_app.rs` |
| `38 TRANSITION RENDER BEGIN` | `220,160,220` | `38 TRANSITION RENDER OK` | `engine/crates/psx-engine/src/game_app.rs` |
| `38 TRANSITION RENDER OK` | `220,200,220` | scene render return | `engine/crates/psx-engine/src/game_app.rs` |

## Maintenance

- Add new checkpoints only with a unique message string.
- If a hardware burn stops on a checkpoint, add that exact message and color to
  `docs/hardware-burn-ledger.md` before changing code.
- If an emulator disagrees with the hardware stop point, add a preburn or
  emulator assertion first, then change the emulator/engine/SDK code.

