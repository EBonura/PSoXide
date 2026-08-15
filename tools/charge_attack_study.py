#!/usr/bin/env python3
"""Author a continuous summon-and-charge attack with release-selected power.

The first second is one shared timeline.  Its opening is sampled from the
recorded light-attack performance, including the short downward right-fist
thrust.  Releasing during the first 0.5 seconds flows into the light strike;
after 0.5 seconds it flows into the heavy strike.  There is no held summon
pose and no combo branch.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import bpy

sys.path.insert(0, str(Path(__file__).resolve().parent))
import attack_study as attack  # noqa: E402
import locomotion_study as locomotion  # noqa: E402


PREVIEW_FPS = 30
SHARED_CHARGE_FRAMES = 30
STAGE_FRAMES = 15
SPAWN_FRAME = 10
LIGHT_RELEASE_FRAME = 13
HEAVY_RELEASE_FRAME = 24
FULL_RELEASE_FRAME = 30

# The old study established these as the starts of the committed strikes.  We
# now preserve the video-derived strike itself while replacing the staged lead.
LIGHT_STRIKE_RANGE = (30, 68)
HEAVY_STRIKE_RANGE = (50, 116)


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--light-motion", required=True, type=Path)
    parser.add_argument("--heavy-motion", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--asset-fps", type=int, default=15)
    return parser.parse_args(argv)


def build_shared_charge_poses(
    scene: bpy.types.Scene,
    target: bpy.types.Object,
    light_source: bpy.types.Object,
    light_mapping: list[tuple[str, str]],
    heavy_source: bpy.types.Object,
    heavy_mapping: list[tuple[str, str]],
) -> list[dict]:
    """Build a moving one-second charge without introducing a pose hold."""

    poses: list[dict] = []

    # Stage one is the recorded opening, time-compressed to 0.5 seconds.  This
    # is the user's short downward fist thrust, not a pose invented by us.
    for index in range(STAGE_FRAMES):
        progress = index / max(1, STAGE_FRAMES - 1)
        source_frame = LIGHT_STRIKE_RANGE[0] * progress
        poses.append(
            attack.source_pose(scene, light_source, light_mapping, source_frame)
        )

    # Stage two keeps moving toward the recorded heavy-ready pose.  Blending
    # progressively from the exact end of stage one guarantees that crossing
    # the 0.5-second threshold never pops or settles into a static stance.
    stage_one_end = poses[-1]
    heavy_preparation_start = 20.0
    heavy_preparation_end = float(HEAVY_STRIKE_RANGE[0])
    for index in range(STAGE_FRAMES):
        progress = (index + 1) / STAGE_FRAMES
        heavy_frame = heavy_preparation_start + (
            heavy_preparation_end - heavy_preparation_start
        ) * progress
        recorded_pose = attack.source_pose(
            scene, heavy_source, heavy_mapping, heavy_frame
        )
        poses.append(
            attack.interpolate_pose(
                stage_one_end, recorded_pose, locomotion.smoothstep(progress)
            )
        )
    return poses


def build_release_action(
    *,
    scene: bpy.types.Scene,
    target: bpy.types.Object,
    charge_poses: list[dict],
    release_frame: int,
    strike_source: bpy.types.Object,
    strike_mapping: list[tuple[str, str]],
    strike_range: tuple[int, int],
    action_name: str,
    recovery_frames: int,
) -> tuple[bpy.types.Action, dict]:
    """Flow from the current charge sample into a committed recorded strike."""

    release_frame = max(1, min(release_frame, len(charge_poses)))
    charge_prefix = charge_poses[:release_frame]
    transition_frames = 4
    strike_start, strike_end = strike_range
    strike_frames = strike_end - strike_start + 1

    locomotion.apply_generated_idle_pose(target, 0.02)
    idle_pose = locomotion.capture_basis_pose(target)
    last_charge_pose = charge_prefix[-1]
    final_strike_pose = attack.source_pose(
        scene, strike_source, strike_mapping, strike_end
    )
    total_frames = (
        len(charge_prefix) + strike_frames + recovery_frames
    )

    if target.animation_data is None:
        target.animation_data_create()
    output_action = bpy.data.actions.new(action_name)
    target.animation_data.action = output_action

    for output_index in range(total_frames):
        output_frame = output_index + 1
        if output_index < len(charge_prefix):
            pose = charge_prefix[output_index]
        elif output_index < len(charge_prefix) + strike_frames:
            strike_index = output_index - len(charge_prefix)
            recorded_pose = attack.source_pose(
                scene,
                strike_source,
                strike_mapping,
                strike_start + strike_index,
            )
            if strike_index < transition_frames:
                amount = locomotion.smoothstep(
                    (strike_index + 1) / transition_frames
                )
                pose = attack.interpolate_pose(
                    last_charge_pose, recorded_pose, amount
                )
            else:
                pose = recorded_pose
        else:
            recovery_index = output_index - len(charge_prefix) - strike_frames
            amount = locomotion.smoothstep(
                (recovery_index + 1) / max(1, recovery_frames)
            )
            pose = attack.interpolate_pose(final_strike_pose, idle_pose, amount)
        attack.apply_pose(target, pose)
        attack.key_pose(target, output_frame)

    for fcurve in output_action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"

    strike_output_start = len(charge_prefix) + 1
    recovery_start = len(charge_prefix) + strike_frames + 1
    scene.frame_start = 1
    scene.frame_end = total_frames
    metadata = {
        "action": action_name,
        "preview_fps": PREVIEW_FPS,
        "release_frame_30hz": release_frame,
        "release_time_seconds": round(release_frame / PREVIEW_FPS, 3),
        "spawn_weapon_frame_30hz": SPAWN_FRAME,
        "spawn_weapon_time_seconds": round(SPAWN_FRAME / PREVIEW_FPS, 3),
        "shared_charge_frames_used": release_frame,
        "strike_source_range": list(strike_range),
        "splits_30hz": {
            "shared_charge": [1, release_frame],
            "release_blend": [
                strike_output_start,
                strike_output_start + transition_frames - 1,
            ],
            "recorded_strike": [
                strike_output_start,
                strike_output_start + strike_frames - 1,
            ],
            "recovery": [recovery_start, total_frames],
        },
        "output_frame_range": [1, total_frames],
    }
    return output_action, metadata


def build_charge_only_action(
    target: bpy.types.Object, charge_poses: list[dict]
) -> bpy.types.Action:
    if target.animation_data is None:
        target.animation_data_create()
    action = bpy.data.actions.new("charge_shared_1s")
    target.animation_data.action = action
    for index, pose in enumerate(charge_poses):
        attack.apply_pose(target, pose)
        attack.key_pose(target, index + 1)
    for fcurve in action.fcurves:
        for keyframe in fcurve.keyframe_points:
            keyframe.interpolation = "LINEAR"
    return action


def render_and_export(
    *,
    target: bpy.types.Object,
    action: bpy.types.Action,
    output_dir: Path,
    stem: str,
    render: bool,
) -> dict[str, str]:
    files: dict[str, str] = {}
    locomotion.activate_action(target, action)
    bpy.context.scene.frame_start = int(action.frame_range[0])
    bpy.context.scene.frame_end = int(action.frame_range[1])
    if render:
        mp4 = output_dir / f"{stem}.mp4"
        bpy.context.scene.render.filepath = str(mp4)
        bpy.context.scene.frame_set(1)
        bpy.ops.render.render(animation=True)
        files["preview"] = str(mp4)
    fbx = output_dir / f"{stem}.fbx"
    locomotion.configure_export_stack(target, action)
    locomotion.export_animation(target, fbx)
    files["fbx"] = str(fbx)
    return files


def main() -> None:
    args = parse_args()
    for path in (args.target, args.light_motion, args.heavy_motion):
        if not path.is_file():
            raise FileNotFoundError(path)
    if args.asset_fps <= 0 or PREVIEW_FPS % args.asset_fps != 0:
        raise ValueError("Asset FPS must divide the 30 Hz authoring rate")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    bpy.ops.wm.read_factory_settings(use_empty=True)
    target = locomotion.import_target(args.target)
    light_source, light_action = attack.import_source(args.light_motion)
    light_source.name = "Charge_Light_Source"
    heavy_source, heavy_action = attack.import_source(args.heavy_motion)
    heavy_source.name = "Charge_Heavy_Source"
    light_mapping = locomotion.compatible_bones(light_source, target)
    heavy_mapping = locomotion.compatible_bones(heavy_source, target)

    scene = bpy.context.scene
    charge_poses = build_shared_charge_poses(
        scene,
        target,
        light_source,
        light_mapping,
        heavy_source,
        heavy_mapping,
    )
    charge_action = build_charge_only_action(target, charge_poses)

    branch_specs = [
        (
            "charge_light_release",
            LIGHT_RELEASE_FRAME,
            light_source,
            light_mapping,
            LIGHT_STRIKE_RANGE,
            10,
            "light",
        ),
        (
            "charge_heavy_release",
            HEAVY_RELEASE_FRAME,
            heavy_source,
            heavy_mapping,
            HEAVY_STRIKE_RANGE,
            16,
            "heavy",
        ),
        (
            "charge_full_release",
            FULL_RELEASE_FRAME,
            heavy_source,
            heavy_mapping,
            HEAVY_STRIKE_RANGE,
            16,
            "heavy_max",
        ),
    ]
    branches: list[tuple[str, bpy.types.Action, dict]] = []
    for (
        stem,
        release_frame,
        source,
        mapping,
        strike_range,
        recovery_frames,
        power,
    ) in branch_specs:
        action, metadata = build_release_action(
            scene=scene,
            target=target,
            charge_poses=charge_poses,
            release_frame=release_frame,
            strike_source=source,
            strike_mapping=mapping,
            strike_range=strike_range,
            action_name=stem,
            recovery_frames=recovery_frames,
        )
        metadata["power_result"] = power
        branches.append((stem, action, metadata))

    # Configure the stage once and keep both source performers invisible.
    locomotion.configure_render(target, light_source, args.output_dir / "preview.mp4")
    heavy_source.hide_render = True
    heavy_source.hide_viewport = True
    for child in heavy_source.children_recursive:
        child.hide_render = True
        child.hide_viewport = True
    attack.add_preview_weapon(target, SPAWN_FRAME)

    charge_files = render_and_export(
        target=target,
        action=charge_action,
        output_dir=args.output_dir,
        stem="charge_shared_1s",
        render=True,
    )
    result = {
        "contract": {
            "input": "hold one attack button; release commits",
            "light_window_seconds": [0.0, 0.5],
            "heavy_window_seconds": [0.5, 1.0],
            "maximum_charge_seconds": 1.0,
            "stage_duration_seconds": 0.5,
            "spawn_weapon_frame_30hz": SPAWN_FRAME,
            "spawn_weapon_time_seconds": round(SPAWN_FRAME / PREVIEW_FPS, 3),
            "head_focus": "enemy throughout; no authored glance at weapon",
            "flourish": "minimal; the recorded hand continuation supplies it",
            "combo": False,
        },
        "shared_charge": {
            "frames_30hz": SHARED_CHARGE_FRAMES,
            "files": charge_files,
        },
        "branches": [],
        "asset_fps": args.asset_fps,
        "preview_weapon": "visible in MP4 only; not exported in FBX",
        "horizontal_root_motion": "removed; gameplay owns displacement",
    }
    for stem, action, metadata in branches:
        metadata["files"] = render_and_export(
            target=target,
            action=action,
            output_dir=args.output_dir,
            stem=stem,
            render=True,
        )
        output_end = metadata["output_frame_range"][1]
        metadata["events_asset_frames_zero_based"] = {
            "spawn_weapon": attack.sampled_frame_at_or_after(
                SPAWN_FRAME, output_end, args.asset_fps
            ),
            "release": attack.sampled_frame_at_or_after(
                metadata["release_frame_30hz"], output_end, args.asset_fps
            ),
        }
        result["branches"].append(metadata)

    metadata_path = args.output_dir / "charge_attack_contract.json"
    metadata_path.write_text(json.dumps(result, indent=2) + "\n")
    blend_path = args.output_dir / "charge_attack_study.blend"
    bpy.ops.wm.save_as_mainfile(filepath=str(blend_path))
    result["blend"] = str(blend_path)
    print("CHARGE_ATTACK_STUDY", json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
