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
    # A readable blade contact needs a body after the initial transient. The
    # old 50 ms tick disappeared under music before the player registered it.
    strike = math.exp(-46.0 * t) * n
    body = math.exp(-15.0 * t) * math.sin(2 * math.pi * (510 - 150 * t / duration) * t)
    ring = math.exp(-9.0 * t) * math.sin(2 * math.pi * 1280 * t)
    return 0.62 * strike + 0.42 * body + 0.20 * ring


def heavy_hit(t, i, n, duration):
    strike = math.exp(-34.0 * t) * n
    body = math.exp(-8.5 * t) * math.sin(2 * math.pi * (175 - 85 * t / duration) * t)
    clang = math.exp(-7.0 * t) * math.sin(2 * math.pi * 610 * t)
    return 0.72 * strike + 0.68 * body + 0.24 * clang


def weapon_swing(t, i, n, duration):
    # A short forward-moving air cut. Its peak arrives after a small rise so
    # the attack reads as motion rather than another contact click.
    phase = min(1.0, t / duration)
    env = math.sin(math.pi * phase) ** 1.6
    flutter = 0.72 + 0.28 * math.sin(2 * math.pi * (34 + 42 * phase) * t)
    edge = math.sin(2 * math.pi * (920 - 610 * phase) * t)
    return env * flutter * (0.70 * n + 0.18 * edge)


def projectile_charge(t, i, n, duration):
    phase = min(1.0, t / duration)
    rise = math.sin(0.5 * math.pi * phase) ** 1.4
    tail = min(1.0, (duration - t) * 28.0)
    carrier = math.sin(2 * math.pi * (125 + 430 * phase * phase) * t)
    harmonic = math.sin(2 * math.pi * (370 + 650 * phase) * t)
    pulse = 0.70 + 0.30 * math.sin(2 * math.pi * (8 + 12 * phase) * t)
    return rise * tail * pulse * (0.48 * carrier + 0.22 * harmonic + 0.10 * n)


def projectile_launch(t, i, n, duration):
    snap = math.exp(-48.0 * t) * n
    fall = math.sin(2 * math.pi * (760 - 570 * t / duration) * t)
    core = math.sin(2 * math.pi * (245 - 95 * t / duration) * t)
    return 0.68 * snap + math.exp(-10.0 * t) * (0.44 * fall + 0.50 * core)


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
    render("light_hit.wav", 0.18, 0x117E, light_hit)
    render("heavy_hit.wav", 0.22, 0x4EAD, heavy_hit)
    render("weapon_swing.wav", 0.16, 0x5A17, weapon_swing)
    render("projectile_charge.wav", 0.35, 0xC4A6, projectile_charge)
    render("projectile_launch.wav", 0.18, 0xB017, projectile_launch)
    render("player_damage.wav", 0.06, 0xDA6E, player_damage)
    render("enemy_death.wav", 0.10, 0xDEAD, enemy_death)


if __name__ == "__main__":
    main()
