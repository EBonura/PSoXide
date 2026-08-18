#!/usr/bin/env python3
"""Read a charge attack out of a headless counter log and check its timing.

Watching a video cannot answer "did the windup play, and for how long"; the
`player_anim_phase_q12` counter can, because it is sampled where rendering
samples the pose. Per attack this prints the press, the commit, the frame the
strike lands on and the windup that was actually visible, and it FAILS on the
two defects that shipped by eye: a commit that lands on or past the strike (no
windup at all) and a phase that jumps backwards (the swing replays).

    python3 tools/charge_timing_check.py target/tmp-fp/verify/charge8.csv

Strike frames come from tools/psxanim_profile.py on the cooked clips.
"""

from __future__ import annotations

import csv
import sys
from pathlib import Path

ATTACKS = {6: ("light", 25), 7: ("heavy", 38), 8: ("combo", 45)}
MIN_WINDUP_MS = 200


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    rows = list(csv.DictReader(open(Path(sys.argv[1]).expanduser())))
    if "player_anim_phase_q12" not in (rows[0] if rows else {}):
        raise SystemExit("no player_anim_phase_q12 column: rebuild the guest and the frontend")

    samples = [
        (
            int(r["guest_frame"]),
            int(r["player_anim_action"]),
            int(r["player_anim_phase_q12"]) >> 12,
        )
        for r in rows
    ]

    failures = []
    attack = None  # (press_tick, action, last_frame, strike_tick, commit_tick)
    for tick, action, frame in samples:
        if action not in ATTACKS:
            if attack:
                report(attack, failures, superseded=False)
                attack = None
            continue
        name, strike = ATTACKS[action]
        if attack is None or attack["action"] != action:
            if attack:
                # An escalating hold abandons the lower level mid-windup; that
                # is the design, so only a level that RAN to its end owes a
                # strike.
                report(attack, failures, superseded=True)
            attack = {
                "press": tick,
                "action": action,
                "name": name,
                "strike": strike,
                "commit": tick,
                "commit_frame": frame,
                "strike_tick": None,
                "last": frame,
                "back": [],
            }
            continue
        if frame > attack["last"] + 1:
            attack["commit"] = tick
            attack["commit_frame"] = frame
        if frame < attack["last"] - 1:
            attack["back"].append((tick, attack["last"], frame))
        if attack["last"] < strike <= frame and attack["strike_tick"] is None:
            attack["strike_tick"] = tick
        attack["last"] = frame
    if attack:
        report(attack, failures, superseded=False)

    if failures:
        for line in failures:
            print(f"[verdict] FAIL: {line}")
        sys.exit(1)
    print("[verdict] OK")


def report(attack: dict, failures: list, superseded: bool) -> None:
    windup_ms = None
    if attack["strike_tick"] is not None:
        windup_ms = (attack["strike_tick"] - attack["commit"]) / 60 * 1000
    print(
        f"{attack['name']:<6} press t={attack['press']:<5} commit t={attack['commit']:<5} "
        f"at frame {attack['commit_frame']:<3} strike frame {attack['strike']:<3} "
        + (
            f"visible windup {windup_ms:.0f} ms"
            if windup_ms is not None
            else ("escalated before its strike" if superseded else "STRIKE NEVER PLAYED")
        )
    )
    if attack["strike_tick"] is None and not superseded:
        failures.append(f"{attack['name']}: the clip ended before its strike frame")
    elif windup_ms is not None and windup_ms < MIN_WINDUP_MS:
        failures.append(
            f"{attack['name']}: only {windup_ms:.0f} ms of windup after the commit "
            f"(min {MIN_WINDUP_MS} ms), it reads as an instant hit"
        )
    for tick, before, after in attack["back"]:
        failures.append(
            f"{attack['name']}: phase jumped backwards at t={tick} "
            f"(frame {before} -> {after}), the swing replays"
        )


main()
