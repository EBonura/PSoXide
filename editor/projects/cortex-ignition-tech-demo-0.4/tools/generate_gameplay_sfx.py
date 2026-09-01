#!/usr/bin/env python3
"""Generate Cortex Ignition's original compact gameplay SFX WAV bank."""

from pathlib import Path
import math
import struct
import wave

# 11.025 kHz is deliberate: the PS1 voice interpolator gives these short,
# noisy transients the desired grit while halving both EXE source bytes and
# resident SPU ADPCM against 22.05 kHz.
RATE = 11025
OUT = Path(__file__).resolve().parents[1] / "assets" / "audio" / "gameplay"


def noise(seed):
    state = seed
    while True:
        state = (1664525 * state + 1013904223) & 0xFFFFFFFF
        yield ((state >> 16) - 32768) / 32768.0


def render(name, seconds, seed, voice):
    count = int(RATE * seconds)
    rng = noise(seed)
    samples = []
    for i in range(count):
        t = i / RATE
        x = voice(t, i, next(rng), seconds)
        samples.append(max(-32767, min(32767, int(x * 25000))))
    OUT.mkdir(parents=True, exist_ok=True)
    with wave.open(str(OUT / name), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(RATE)
        wav.writeframes(struct.pack("<" + "h" * count, *samples))


def footstep(t, i, n, duration):
    env = math.exp(-32.0 * t)
    thud = math.sin(2 * math.pi * (92 - 34 * t / duration) * t)
    grit = n * math.exp(-52.0 * t)
    return env * (0.72 * thud + 0.28 * grit)


def light_hit(t, i, n, duration):
    env = math.exp(-28.0 * t)
    ring = math.sin(2 * math.pi * 970 * t) + 0.42 * math.sin(2 * math.pi * 1430 * t)
    return env * (0.38 * ring + 0.48 * n)


def heavy_hit(t, i, n, duration):
    env = math.exp(-18.0 * t)
    metal = math.sin(2 * math.pi * (180 - 70 * t / duration) * t)
    clang = math.sin(2 * math.pi * 690 * t) * math.exp(-12.0 * t)
    return env * (0.62 * metal + 0.22 * clang + 0.3 * n)


def player_damage(t, i, n, duration):
    env = math.exp(-22.0 * t)
    buzz = math.sin(2 * math.pi * (235 + 38 * math.sin(2 * math.pi * 31 * t)) * t)
    return env * (0.52 * buzz + 0.4 * n)


def enemy_death(t, i, n, duration):
    env = math.exp(-9.0 * t)
    fall = math.sin(2 * math.pi * (155 - 95 * t / duration) * t)
    rattle = n * (0.45 + 0.55 * math.sin(2 * math.pi * 43 * t) ** 2)
    return env * (0.62 * fall + 0.42 * rattle)


def main():
    render("footstep.wav", 0.04, 0xF007, footstep)
    render("light_hit.wav", 0.05, 0x117E, light_hit)
    render("heavy_hit.wav", 0.075, 0x4EAD, heavy_hit)
    render("player_damage.wav", 0.06, 0xDA6E, player_damage)
    render("enemy_death.wav", 0.10, 0xDEAD, enemy_death)


if __name__ == "__main__":
    main()
