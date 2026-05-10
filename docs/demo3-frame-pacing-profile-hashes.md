# Demo3 Frame Pacing Profile And Hash Policy

This note records how to benchmark demo3 frame pacing and how to read hashes
once the runtime uses 60Hz control with paced 20Hz visuals.

## Commands

Reference every-rendered-frame profiles:

```sh
make profile-demo3 PROFILE_DEMO3_HW=/tmp/psoxide-demo3-worker6-reference.ppm
make profile-demo3-forward PROFILE_DEMO3_FORWARD_HW=/tmp/psoxide-demo3-worker6-reference-forward.ppm
```

Paced 20Hz profiles:

```sh
make profile-demo3-paced20 PROFILE_DEMO3_PACED20_HW=/tmp/psoxide-demo3-worker6-paced20.ppm
make profile-demo3-paced20-forward PROFILE_DEMO3_PACED20_FORWARD_HW=/tmp/psoxide-demo3-worker6-paced20-forward.ppm
```

The paced targets pass both `--guest-visual-frames` and `--guest-frames`.
`--guest-visual-frames` stops on the `VISUAL_FRAMES` telemetry counter once the
cadence runtime emits it. `--guest-frames` is a fallback for unpaced runtimes
that only emit frame-begin markers.

## Hash Policy

Old every-rendered-frame hashes are pre-pacing references only:

| Capture | Display hash | VRAM hash |
|---|---|---|
| demo3 default | `0x807c5debd2e9bf8a` | `0xa5c5a996b781b8b0` |
| demo3 hold-forward | `0xc10fd4b6892df758` | `0x736412dbc4da1148` |

Paced 20Hz hashes must be compared against paced 20Hz captures, because the
sampled visual frame may differ from the old every-frame capture. A hash change
is acceptable only when the profile proves the capture stopped at a different
visual sample and the screenshot has no visible regression.

## Telemetry Contract

The runtime cadence layer should emit:

| Counter | Meaning |
|---|---|
| `sim ticks` | fixed simulation/control ticks run |
| `visual frames` | rendered visual frames produced |
| `visual skipped vblanks` | held-display ticks or skipped visual slots |
| `visual deadline misses` | visual frames late for the target cadence |
| `visual interval vblanks` | configured interval, emitted once per frame marker |
| `visual max lateness vblanks` | lateness for each visual frame; host reports max |

The CLI reports `guest_profile_frame_meaning=frame_begin_markers` so
`guest_profile_frames` is not confused with rendered visual frames.

## Budget

NTSC budget:

| Budget | Cycles |
|---|---:|
| 1 VBlank | `564480` |
| 3 VBlanks / 20Hz visual | `1693440` |

For paced 20Hz captures, use `render_cycles_per_visual_frame` and
`paced20_budget_status` from `guest_profile_pacing`. `pass` means the measured
rendered visual frame cost fits inside the 3-VBlank budget; `fail` means it
does not.

## Worker 6 Paced Captures

These captures use the paced target commands above. The hashes match the old
reference images for these stop points, but they are now paced-cadence
baselines because the stop condition is rendered visual frames.

| Capture | Display hash | VRAM hash | Screenshot | Visual frames | Sim ticks | Render cycles / visual | Status |
|---|---|---|---|---:|---:|---:|---|
| paced20 default | `0x807c5debd2e9bf8a` | `0xa5c5a996b781b8b0` | `/tmp/psoxide-demo3-worker6-paced20.ppm` | 60 | 451 | 3,479,132 | fail |
| paced20 hold-forward | `0xc10fd4b6892df758` | `0x736412dbc4da1148` | `/tmp/psoxide-demo3-worker6-paced20-forward.ppm` | 80 | 792 | 2,562,982 | fail |

Both captures miss the 3-VBlank visual budget (`1,693,440` cycles), so the
current runtime is measurable but not yet accepted for steady 20Hz visual
pacing.
