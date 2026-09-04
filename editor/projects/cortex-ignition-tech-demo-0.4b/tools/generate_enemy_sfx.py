#!/usr/bin/env python3
"""Original short mechanical enemy cues; no external samples."""
import math
from generate_gameplay_sfx import render


def step(t, i, noise, duration):
    impact = math.exp(-48 * t) * (0.6 * math.sin(2 * math.pi * 88 * t) + 0.25 * noise)
    metal = 0.18 * math.exp(-32 * t) * math.sin(2 * math.pi * 1327 * t)
    servo = 0.1 * math.sin(math.pi * t / duration) ** 2 * math.sin(2 * math.pi * (410 * t - 650 * t * t))
    return impact + metal + servo


def idle(t, i, noise, duration):
    envelope = math.sin(math.pi * t / duration) ** 2
    mod = math.sin(2 * math.pi * 37 * t)
    carrier = math.sin(2 * math.pi * (340 * t - 160 * t * t) + 1.6 * mod)
    chirp = math.sin(2 * math.pi * (910 * t + 240 * t * t))
    return envelope * (0.3 * carrier + 0.1 * chirp + 0.035 * noise)


if __name__ == "__main__":
    render("enemy_footstep.wav", 0.12, 0xE117, step, rate=8000)
    render("enemy_idle.wav", 0.30, 0x1D1E, idle, rate=8000)
