#!/usr/bin/env python3
"""Objective animation check: find glitches instead of squinting at videos.

Every joint's world rotation is sampled per frame; the per-frame angular step
(deg/frame) and its change frame to frame (deg/frame^2, the jerk) are the two
numbers that matter. A human reads a jerk spike as a "glitch", a pop, a
tremble. The script reports the worst frames and the worst joints by name, so
a failure points at what to fix.

With `--against SOURCE` it compares a result to the clip it came from: the
source is the ground truth, and any jerk the result has that the source does
not is damage introduced by the pipeline (retarget, assembly, resampling).

    blender --background --factory-startup --python tools/anim_validate.py -- \
        --clip result.glb [--take NAME] [--against source.glb] [--fps 30] \
        [--jerk-limit 12]

Exit code 1 when the clip exceeds the limit (or exceeds the source by more
than the tolerance), so it can gate a pipeline step.
"""

from __future__ import annotations

import argparse
import math
import sys
from pathlib import Path

import bpy
from mathutils import Vector


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clip", required=True, type=Path, help="GLB/FBX to check")
    parser.add_argument("--take", help="Action name inside the file")
    parser.add_argument("--against", type=Path, help="Source clip to compare against")
    parser.add_argument("--against-take", help="Action name inside the source")
    parser.add_argument("--fps", type=int, default=30)
    parser.add_argument(
        "--jerk-limit",
        type=float,
        default=12.0,
        help="Max acceptable mean-of-worst-joint jerk in deg/frame^2",
    )
    parser.add_argument(
        "--tolerance",
        type=float,
        default=2.0,
        help="With --against: how many times the source's jerk is acceptable",
    )
    parser.add_argument("--top", type=int, default=5, help="How many offenders to list")
    return parser.parse_args(argv)


def load(path: Path, take: str | None):
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete()
    for action in list(bpy.data.actions):
        bpy.data.actions.remove(action)
    suffix = path.suffix.lower()
    if suffix == ".fbx":
        bpy.ops.import_scene.fbx(filepath=str(path), use_anim=True)
    elif suffix in (".glb", ".gltf"):
        bpy.ops.import_scene.gltf(filepath=str(path))
    else:
        raise SystemExit(f"unsupported clip {path}")
    rigs = [o for o in bpy.data.objects if o.type == "ARMATURE"]
    if not rigs:
        raise SystemExit(f"no armature in {path}")
    rig = rigs[0]
    if rig.animation_data is None:
        rig.animation_data_create()
    for track in rig.animation_data.nla_tracks:
        track.mute = True
    actions = list(bpy.data.actions)
    if take:
        actions = [a for a in actions if a.name == take or a.name.startswith(take)]
        if not actions:
            raise SystemExit(f"take {take!r} not in {path}")
    rig.animation_data.action = actions[0]
    return rig, actions[0]


def sample(rig, action, fps: int):
    """Per-frame world rotation of every bone, as quaternions."""
    first, last = int(action.frame_range[0]), int(action.frame_range[1])
    names = [b.name for b in rig.pose.bones]
    frames = []
    for frame in range(first, last + 1):
        bpy.context.scene.frame_set(frame)
        bpy.context.view_layer.update()
        frames.append(
            {
                name: (rig.matrix_world @ rig.pose.bones[name].matrix)
                .to_quaternion()
                .normalized()
                for name in names
            }
        )
    return names, frames, first


def metrics(names, frames):
    """Angular step per frame and its change (jerk), per joint."""
    steps = {name: [] for name in names}
    for a, b in zip(frames, frames[1:]):
        for name in names:
            qa, qb = a[name], b[name]
            # Quaternion double cover: to_quaternion() may hand back q or -q
            # from frame to frame, which reads as a 180 degree jump. Align the
            # signs before measuring.
            if qa.dot(qb) < 0.0:
                qb = qb.copy()
                qb.negate()
            angle = math.degrees(qa.rotation_difference(qb).angle)
            if angle > 180.0:
                angle = 360.0 - angle
            steps[name].append(angle)
    jerks = {
        name: [abs(y - x) for x, y in zip(series, series[1:])]
        for name, series in steps.items()
    }
    return steps, jerks


def report(label, names, frames, first, top):
    steps, jerks = metrics(names, frames)
    worst_joint = sorted(
        ((max(series, default=0.0), name) for name, series in jerks.items()),
        reverse=True,
    )
    per_frame = []
    count = len(frames) - 2
    for index in range(max(count, 0)):
        value, name = max(
            ((jerks[n][index], n) for n in names if index < len(jerks[n])),
            default=(0.0, ""),
        )
        per_frame.append((value, first + index + 1, name))
    worst_frames = sorted(per_frame, reverse=True)[:top]
    peak = worst_frames[0][0] if worst_frames else 0.0
    mean = sum(v for v, _, _ in per_frame) / max(len(per_frame), 1)
    print(f"[{label}] frames={len(frames)} peak_jerk={peak:.1f} mean_jerk={mean:.2f} deg/frame^2")
    for value, frame, name in worst_frames:
        print(f"[{label}]   frame {frame:4} {name:<22} jerk {value:6.1f}")
    for value, name in worst_joint[:top]:
        print(f"[{label}]   joint {name:<22} worst {value:6.1f}")
    return peak, mean, per_frame


def main() -> None:
    args = parse_args()
    rig, action = load(args.clip.expanduser(), args.take)
    names, frames, first = sample(rig, action, args.fps)
    peak, mean, per_frame = report("clip", names, frames, first, args.top)

    verdict_ok = True
    if args.against:
        rig2, action2 = load(args.against.expanduser(), args.against_take)
        names2, frames2, first2 = sample(rig2, action2, args.fps)
        # Compare only joints the two rigs share (retargets rename).
        shared = [n for n in names2 if n in names]
        src_peak, src_mean, _ = report("source", names2, frames2, first2, args.top)
        print(
            f"[compare] shared joints {len(shared)}  clip/source peak {peak / max(src_peak, 1e-6):.2f}x"
            f"  mean {mean / max(src_mean, 1e-6):.2f}x"
        )
        if mean > src_mean * args.tolerance:
            print(
                f"[verdict] FAIL: the result is {mean / max(src_mean, 1e-6):.2f}x jerkier than its"
                f" source (tolerance {args.tolerance}x)"
            )
            verdict_ok = False
    if mean > args.jerk_limit:
        print(f"[verdict] FAIL: mean jerk {mean:.2f} over limit {args.jerk_limit}")
        verdict_ok = False
    if verdict_ok:
        print("[verdict] OK")
    else:
        sys.exit(1)


main()
