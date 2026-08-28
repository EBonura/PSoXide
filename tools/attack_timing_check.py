#!/usr/bin/env python3
"""Read the attacks out of a headless counter log and check they play whole.

The contract is simple now: R1/R2 trigger Horizon light/heavy and L1/L2 trigger
Zenith light/heavy, each directly and without a shoulder chord. Each plays its
clip from frame 0 to the end. That is exactly what is easy to break silently,
so it is what this asserts, from the
`player_anim_phase_q12` counter (sampled where rendering samples the pose)
rather than from a video.

    python3 tools/attack_timing_check.py target/tmp-fp/verify/atk1.csv

The counter log comes from a headless replay of an input tape that presses
each attack once: R1 (bit 11), R2 (bit 9), L1 (bit 10), then L2 (bit 8), on a
tape that presses CROSS (bit 14) to leave the menu. Leave enough room between
presses for the clips to finish.

Fails when a clip starts partway in, skips or repeats frames, or is cut off
before its strike or its last frame. Frame counts and strike frames come from
tools/psxanim_profile.py on the cooked clips.
"""

from __future__ import annotations

import csv
import sys
from pathlib import Path

# action index -> (name, cooked frames, strike frame). Horizon is on the right
# shoulder pair, Zenith on the left; strike frames measured with
# tools/psxanim_profile.py on the cooked clips.
ATTACKS = {
    6: ("horizon light", 37, 25),
    7: ("horizon heavy", 63, 38),
    30: ("zenith light", 57, 28),
    31: ("zenith heavy", 64, 33),
}


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    rows = list(csv.DictReader(open(Path(sys.argv[1]).expanduser())))
    if not rows or "player_anim_phase_q12" not in rows[0]:
        raise SystemExit("no player_anim_phase_q12 column: rebuild the guest and the frontend")

    failures: list[str] = []
    current = None
    for row in rows:
        tick = int(row["guest_frame"])
        action = int(row["player_anim_action"])
        frame = int(row["player_anim_phase_q12"]) >> 12
        if action not in ATTACKS:
            current = close(current, failures)
            continue
        if current is None or current["action"] != action:
            current = close(current, failures)
            name, frames, strike = ATTACKS[action]
            current = {
                "action": action,
                "name": name,
                "frames": frames,
                "strike": strike,
                "start_tick": tick,
                "start_frame": frame,
                "last": frame,
                "jumps": [],
            }
            continue
        if frame > current["last"] + 1 or frame < current["last"]:
            current["jumps"].append((tick, current["last"], frame))
        current["last"] = frame
        current["end_tick"] = tick
    close(current, failures)

    if failures:
        for line in failures:
            print(f"[verdict] FAIL: {line}")
        sys.exit(1)
    print("[verdict] OK")


def close(attack, failures: list) -> None:
    if attack is None:
        return None
    ticks = attack.get("end_tick", attack["start_tick"]) - attack["start_tick"] + 1
    print(
        f"{attack['name']:<6} t={attack['start_tick']:<5} frames "
        f"{attack['start_frame']}..{attack['last']} of {attack['frames'] - 1}  "
        f"{ticks / 60:.2f} s"
    )
    if attack["start_frame"] != 0:
        failures.append(
            f"{attack['name']}: started at frame {attack['start_frame']}, not 0, "
            "so the front of the performance never played"
        )
    if attack["last"] < attack["strike"]:
        failures.append(
            f"{attack['name']}: ended at frame {attack['last']}, before its strike "
            f"at {attack['strike']}"
        )
    elif attack["last"] < attack["frames"] - 2:
        failures.append(
            f"{attack['name']}: ended at frame {attack['last']} of {attack['frames'] - 1}, "
            "so the recovery was cut"
        )
    for tick, before, after in attack["jumps"]:
        failures.append(
            f"{attack['name']}: phase jumped at t={tick} (frame {before} -> {after})"
        )
    return None


main()
