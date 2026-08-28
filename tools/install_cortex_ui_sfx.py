#!/usr/bin/env python3
"""Install the Cortex UI sound palette into empty project UI bindings.

This intentionally never replaces a non-empty binding.  It is safe to rerun
after adding controls: only newly-authored silent buttons and sliders receive
the shared defaults.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path


EMPTY = "sfx: (focus: [], activate: [], nudge: [], limit: [])"
ROOT = "assets/audio/ui"


def cue(filename: str, volume: int, pitch: int = 4096) -> str:
    return f'(wav_path: "{ROOT}/{filename}", volume: {volume}, pitch_q12: {pitch})'


FOCUS = cue("ui_navigate.wav", 38)
LEGACY_FOCUS = ", ".join(
    (
        cue("ui_nav_01.wav", 38),
        cue("ui_nav_02.wav", 36),
        cue("ui_nav_03.wav", 38),
    )
)


def button_binding(name: str) -> str:
    if name in {"Back", "Return To Title"}:
        activate = cue("ui_back.wav", 54)
    elif name in {
        "Inventory Player Tab",
        "Inventory System Tab",
        "System Player Tab",
        "System Tab",
    }:
        activate = cue("ui_tab_shift.wav", 50)
    elif name in {
        "Horizon Empty Boost",
        "Horizon Full Boost",
        "Zenith Empty Boost",
        "Zenith Full Boost",
    }:
        activate = cue("ui_socket.wav", 58)
    elif name == "Remove Socketed Module":
        activate = cue("ui_unsocket.wav", 56)
    else:
        activate = cue("ui_confirm.wav", 54)
    return f"sfx: (focus: [{FOCUS}], activate: [{activate}], nudge: [], limit: [])"


def slider_binding() -> str:
    nudge = cue("ui_slider_tick.wav", 36)
    limit = cue("ui_limit.wav", 44)
    return f"sfx: (focus: [{FOCUS}], activate: [], nudge: [{nudge}], limit: [{limit}])"


def install(path: Path) -> tuple[int, int, int]:
    source = path.read_text()
    output: list[str] = []
    button_count = 0
    slider_count = 0
    migrated_focus = 0
    name_pattern = re.compile(r'name: "([^"]+)"')
    for line in source.splitlines(keepends=True):
        legacy_pool = f"focus: [{LEGACY_FOCUS}]"
        if legacy_pool in line:
            line = line.replace(legacy_pool, f"focus: [{FOCUS}]", 1)
            migrated_focus += 1
        if EMPTY not in line:
            output.append(line)
            continue
        match = name_pattern.search(line)
        if not match:
            output.append(line)
            continue
        name = match.group(1)
        if "kind: Button(" in line:
            line = line.replace(EMPTY, button_binding(name), 1)
            button_count += 1
        elif "kind: Slider(" in line:
            line = line.replace(EMPTY, slider_binding(), 1)
            slider_count += 1
        output.append(line)
    path.write_text("".join(output))
    return button_count, slider_count, migrated_focus


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("projects", type=Path, nargs="+")
    args = parser.parse_args()
    for project in args.projects:
        buttons, sliders, migrated = install(project)
        print(
            f"{project}: installed {buttons} button and {sliders} slider bindings; "
            f"migrated {migrated} focus bindings"
        )


if __name__ == "__main__":
    main()
