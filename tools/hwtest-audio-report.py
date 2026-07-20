#!/usr/bin/env python3
"""Decode and validate PSoXide PA1/PA2/PA3/PA4/PA5 audio QR payloads."""

from __future__ import annotations

import argparse
import base64
import binascii
import pathlib
import struct
import sys


PA1_STAGE_LABELS = (
    "idle_calibration",
    "readn_game_route_off",
    "readn_controller_muted",
    "readn_spu_route_on",
    "paused_post_read",
)
FIELD_COUNT = 10
PA1_BINARY_LEN = 220
PA2_STAGE_LABELS = (
    "idle_calibration",
    "game_bank_dma_upload",
    "game_voice_active",
    "game_voice_end_guard",
    "settled_dma_voice_active",
    "settled_dma_end_guard",
)
PA2_BINARY_LEN = 264
PA3_STAGE_LABELS = (
    "idle_calibration",
    "full_menu_bank",
    "full_to_light_t0a0",
    "map_voices_active",
    "unsafe_live_overwrite",
    "safe_stop_overwrite",
)
PA3_BINARY_LEN = 272
PA4_STAGE_LABELS = (
    "idle_calibration",
    "full_menu_bank",
    "voice16_natural_end",
    "handoff_or_init",
    "light_or_hold_a",
    "map_or_hold_b",
    "spu_readback",
)
PA4_VARIANTS = ("baseline", "safe0", "safe1", "safe2", "split")
PA4_BINARY_LEN = 320
PA5_STAGE_LABELS = (
    "bios_snapshot_calibration",
    "full_menu_bank",
    "voice16_natural_end",
    "spu_init_only",
    "light_bank_only",
    "reverb_reset_only",
    "map_bank_only",
    "spu_readback",
)
PA5_VARIANTS = ("control", "depth0", "depth2", "base0", "full0")
PA5_BINARY_LEN = 424


def extract_payload(value: str) -> str:
    markers = [
        marker
        for marker in (
            value.find("PA1/"),
            value.find("PA2/"),
            value.find("PA3/"),
            value.find("PA4/"),
            value.find("PA5/"),
        )
        if marker >= 0
    ]
    marker = min(markers) if markers else -1
    if marker < 0:
        raise ValueError("no PA1, PA2, PA3, PA4, or PA5 payload found")
    payload = value[marker:].split()[0]
    if "/C:" not in payload:
        raise ValueError("audio payload has no CRC suffix")
    return payload


def decode_payload(payload: str) -> dict[str, object]:
    if payload.startswith("PA5/"):
        return decode_pa5(payload)
    if payload.startswith("PA4/"):
        return decode_pa4(payload)
    if payload.startswith("PA3/"):
        return decode_pa3(payload)
    if payload.startswith("PA2/"):
        return decode_pa2(payload)
    return decode_pa1(payload)


def decode_pa1(payload: str) -> dict[str, object]:
    encoded, suffix_crc = payload[4:].rsplit("/C:", 1)
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PA1 Base64: {exc}") from exc
    if len(binary) != PA1_BINARY_LEN:
        raise ValueError(f"PA1 binary length {len(binary)} != {PA1_BINARY_LEN}")

    magic, version, stage_count, field_count, run, lba, stage_frames = struct.unpack_from(
        "<4sBBBBII", binary
    )
    if magic != b"PA1B" or version != 1:
        raise ValueError(f"unsupported PA1 schema: magic={magic!r} version={version}")
    if stage_count != len(PA1_STAGE_LABELS) or field_count != FIELD_COUNT:
        raise ValueError(
            f"unexpected PA1 dimensions: stages={stage_count} fields={field_count}"
        )

    embedded_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    calculated_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    try:
        displayed_crc = int(suffix_crc, 16)
    except ValueError as exc:
        raise ValueError(f"invalid PA1 CRC suffix: {suffix_crc!r}") from exc
    if embedded_crc != calculated_crc or displayed_crc != calculated_crc:
        raise ValueError(
            "PA1 CRC mismatch: "
            f"suffix={displayed_crc:08X} embedded={embedded_crc:08X} "
            f"calculated={calculated_crc:08X}"
        )

    stages: list[dict[str, object]] = []
    offset = 16
    for expected_stage, label in enumerate(PA1_STAGE_LABELS):
        values = struct.unpack_from(f"<{FIELD_COUNT}I", binary, offset)
        offset += FIELD_COUNT * 4
        (
            stage_tick,
            spu,
            cd_volume,
            cd_state,
            left_hash,
            right_hash,
            nonzero,
            peak,
            left_energy,
            right_energy,
        ) = values
        stage_id = stage_tick >> 24
        if stage_id != expected_stage:
            raise ValueError(f"PA1 stage order mismatch: got {stage_id}, want {expected_stage}")
        stages.append(
            {
                "name": label,
                "tick": stage_tick & 0x00FF_FFFF,
                "spucnt": spu >> 16,
                "spustat": spu & 0xFFFF,
                "cd_volume_left": cd_volume >> 16,
                "cd_volume_right": cd_volume & 0xFFFF,
                "cd_status": cd_state >> 24,
                "cd_irq": (cd_state >> 20) & 0x0F,
                "command_state": (cd_state >> 16) & 0x0F,
                "sectors": cd_state & 0xFFFF,
                "left_hash": left_hash,
                "right_hash": right_hash,
                "left_nonzero": nonzero >> 16,
                "right_nonzero": nonzero & 0xFFFF,
                "left_peak": peak >> 16,
                "right_peak": peak & 0xFFFF,
                "left_energy": left_energy,
                "right_energy": right_energy,
            }
        )

    return {
        "schema": "PA1",
        "run": run,
        "lba": lba,
        "stage_frames": stage_frames,
        "crc32": calculated_crc,
        "stages": stages,
    }


def decode_pa2(payload: str) -> dict[str, object]:
    encoded, suffix_crc = payload[4:].rsplit("/C:", 1)
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PA2 Base64: {exc}") from exc
    if len(binary) != PA2_BINARY_LEN:
        raise ValueError(f"PA2 binary length {len(binary)} != {PA2_BINARY_LEN}")

    magic, version, stage_count, field_count, run, layout_crc, target_meta, target_hash = (
        struct.unpack_from("<4sBBBBIII", binary)
    )
    if magic != b"PA2B" or version != 1:
        raise ValueError(f"unsupported PA2 schema: magic={magic!r} version={version}")
    if stage_count != len(PA2_STAGE_LABELS) or field_count != FIELD_COUNT:
        raise ValueError(
            f"unexpected PA2 dimensions: stages={stage_count} fields={field_count}"
        )

    embedded_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    calculated_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    try:
        displayed_crc = int(suffix_crc, 16)
    except ValueError as exc:
        raise ValueError(f"invalid PA2 CRC suffix: {suffix_crc!r}") from exc
    if embedded_crc != calculated_crc or displayed_crc != calculated_crc:
        raise ValueError(
            "PA2 CRC mismatch: "
            f"suffix={displayed_crc:08X} embedded={embedded_crc:08X} "
            f"calculated={calculated_crc:08X}"
        )

    stages: list[dict[str, object]] = []
    offset = 20
    for expected_stage, label in enumerate(PA2_STAGE_LABELS):
        values = struct.unpack_from(f"<{FIELD_COUNT}I", binary, offset)
        offset += FIELD_COUNT * 4
        (
            stage_tick,
            spu,
            volume,
            pitch_start,
            adsr,
            current_repeat,
            endx,
            timing,
            expected_tail_hash,
            observed_tail_hash,
        ) = values
        stage_id = stage_tick >> 24
        if stage_id != expected_stage:
            raise ValueError(f"PA2 stage order mismatch: got {stage_id}, want {expected_stage}")
        stages.append(
            {
                "name": label,
                "tick": stage_tick & 0x00FF_FFFF,
                "spucnt": spu >> 16,
                "spustat": spu & 0xFFFF,
                "volume_left": volume >> 16,
                "volume_right": volume & 0xFFFF,
                "pitch": pitch_start >> 16,
                "start": pitch_start & 0xFFFF,
                "adsr_low": adsr >> 16,
                "adsr_high": adsr & 0xFFFF,
                "current_volume": current_repeat >> 16,
                "repeat": current_repeat & 0xFFFF,
                "endx": endx,
                "voice15_ended": bool(endx & (1 << 15)),
                "max_mode_polls": timing >> 16,
                "max_drain_polls": timing & 0xFFFF,
                "expected_tail_hash": expected_tail_hash,
                "observed_tail_hash": observed_tail_hash,
                "tail_matches": expected_tail_hash == observed_tail_hash,
            }
        )

    return {
        "schema": "PA2",
        "run": run,
        "layout_crc": layout_crc,
        "target_rate": target_meta >> 16,
        "target_len": target_meta & 0xFFFF,
        "target_hash": target_hash,
        "crc32": calculated_crc,
        "stages": stages,
    }


def decode_pa3(payload: str) -> dict[str, object]:
    encoded, suffix_crc = payload[4:].rsplit("/C:", 1)
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PA3 Base64: {exc}") from exc
    if len(binary) != PA3_BINARY_LEN:
        raise ValueError(f"PA3 binary length {len(binary)} != {PA3_BINARY_LEN}")

    (
        magic,
        version,
        stage_count,
        field_count,
        run,
        layout_crc,
        full_bytes,
        light_bytes,
        map_bytes,
        readback_bytes,
    ) = struct.unpack_from("<4sBBBBIIIII", binary)
    if magic != b"PA3B" or version != 1:
        raise ValueError(f"unsupported PA3 schema: magic={magic!r} version={version}")
    if stage_count != len(PA3_STAGE_LABELS) or field_count != FIELD_COUNT:
        raise ValueError(
            f"unexpected PA3 dimensions: stages={stage_count} fields={field_count}"
        )

    embedded_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    calculated_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    try:
        displayed_crc = int(suffix_crc, 16)
    except ValueError as exc:
        raise ValueError(f"invalid PA3 CRC suffix: {suffix_crc!r}") from exc
    if embedded_crc != calculated_crc or displayed_crc != calculated_crc:
        raise ValueError(
            "PA3 CRC mismatch: "
            f"suffix={displayed_crc:08X} embedded={embedded_crc:08X} "
            f"calculated={calculated_crc:08X}"
        )

    stages: list[dict[str, object]] = []
    offset = 28
    for expected_stage, label in enumerate(PA3_STAGE_LABELS):
        values = struct.unpack_from(f"<{FIELD_COUNT}I", binary, offset)
        offset += FIELD_COUNT * 4
        (
            stage_tick,
            spu,
            endx,
            voice0,
            voice15,
            voice16,
            voice17,
            event_vblanks,
            expected_hash,
            observed_hash,
        ) = values
        stage_id = stage_tick >> 24
        if stage_id != expected_stage:
            raise ValueError(f"PA3 stage order mismatch: got {stage_id}, want {expected_stage}")

        def voice_state(value: int) -> dict[str, int]:
            return {"mixer_volume": value >> 16, "current_envelope": value & 0xFFFF}

        stages.append(
            {
                "name": label,
                "tick": stage_tick & 0x00FF_FFFF,
                "spucnt": spu >> 16,
                "spustat": spu & 0xFFFF,
                "endx": endx,
                "voice0": voice_state(voice0),
                "voice15": voice_state(voice15),
                "voice16": voice_state(voice16),
                "voice17": voice_state(voice17),
                "event_vblanks": event_vblanks,
                "expected_hash": expected_hash,
                "observed_hash": observed_hash,
                "readback_matches": expected_hash == observed_hash,
            }
        )

    return {
        "schema": "PA3",
        "run": run,
        "layout_crc": layout_crc,
        "full_bytes": full_bytes,
        "light_bytes": light_bytes,
        "map_bytes": map_bytes,
        "readback_bytes": readback_bytes,
        "crc32": calculated_crc,
        "stages": stages,
    }


def decode_pa4(payload: str) -> dict[str, object]:
    encoded, suffix_crc = payload[4:].rsplit("/C:", 1)
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PA4 Base64: {exc}") from exc
    if len(binary) != PA4_BINARY_LEN:
        raise ValueError(f"PA4 binary length {len(binary)} != {PA4_BINARY_LEN}")

    (
        magic,
        version,
        stage_count,
        field_count,
        run,
        layout_crc,
        variant,
        wait_vblanks,
        full_bytes,
        light_bytes,
        map_bytes,
        readback_bytes,
    ) = struct.unpack_from("<4sBBBBIIIIIII", binary)
    if magic != b"PA4B" or version not in (1, 2):
        raise ValueError(f"unsupported PA4 schema: magic={magic!r} version={version}")
    if stage_count != len(PA4_STAGE_LABELS) or field_count != FIELD_COUNT:
        raise ValueError(
            f"unexpected PA4 dimensions: stages={stage_count} fields={field_count}"
        )
    if variant >= len(PA4_VARIANTS):
        raise ValueError(f"unknown PA4 variant {variant}")

    embedded_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    calculated_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    try:
        displayed_crc = int(suffix_crc, 16)
    except ValueError as exc:
        raise ValueError(f"invalid PA4 CRC suffix: {suffix_crc!r}") from exc
    if embedded_crc != calculated_crc or displayed_crc != calculated_crc:
        raise ValueError(
            "PA4 CRC mismatch: "
            f"suffix={displayed_crc:08X} embedded={embedded_crc:08X} "
            f"calculated={calculated_crc:08X}"
        )

    stages: list[dict[str, object]] = []
    offset = 36
    for expected_stage, label in enumerate(PA4_STAGE_LABELS):
        values = struct.unpack_from(f"<{FIELD_COUNT}I", binary, offset)
        offset += FIELD_COUNT * 4
        (
            stage_tick,
            spu,
            endx,
            voice16_volume_envelope,
            voice16_pitch_start,
            voice16_adsr_repeat,
            nonzero_voice_mask,
            event_vblanks,
            expected_hash,
            observed_hash,
        ) = values
        stage_id = stage_tick >> 24
        if stage_id != expected_stage:
            raise ValueError(f"PA4 stage order mismatch: got {stage_id}, want {expected_stage}")
        stage = {
                "name": label,
                "tick": stage_tick & 0x00FF_FFFF,
                "spucnt": spu >> 16,
                "spustat": spu & 0xFFFF,
                "endx": endx,
                "voice16_volume": voice16_volume_envelope >> 16,
                "voice16_envelope": voice16_volume_envelope & 0xFFFF,
                "voice16_pitch": voice16_pitch_start >> 16,
                "voice16_start": voice16_pitch_start & 0xFFFF,
                "voice16_adsr": voice16_adsr_repeat >> 16,
                "voice16_repeat": voice16_adsr_repeat & 0xFFFF,
                "nonzero_voice_mask": nonzero_voice_mask,
                "event_vblanks": event_vblanks,
                "expected_hash": expected_hash if version == 1 or expected_stage == 6 else None,
                "observed_hash": observed_hash if version == 1 or expected_stage == 6 else None,
                "readback_matches": (
                    expected_hash == observed_hash if version == 1 or expected_stage == 6 else None
                ),
                "clock_before": expected_hash if version == 2 and expected_stage < 6 else None,
                "clock_after": observed_hash if version == 2 and expected_stage < 6 else None,
            }
        stages.append(stage)

    return {
        "schema": "PA4",
        "schema_version": version,
        "run": run,
        "layout_crc": layout_crc,
        "variant": PA4_VARIANTS[variant],
        "variant_code": variant,
        "wait_vblanks": wait_vblanks,
        "full_bytes": full_bytes,
        "light_bytes": light_bytes,
        "map_bytes": map_bytes,
        "readback_bytes": readback_bytes,
        "crc32": calculated_crc,
        "stages": stages,
    }


def decode_pa5(payload: str) -> dict[str, object]:
    encoded, suffix_crc = payload[4:].rsplit("/C:", 1)
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PA5 Base64: {exc}") from exc
    if len(binary) != PA5_BINARY_LEN:
        raise ValueError(f"PA5 binary length {len(binary)} != {PA5_BINARY_LEN}")

    (
        magic,
        version,
        stage_count,
        field_count,
        run,
        layout_crc,
        variant,
        wait_vblanks,
        full_bytes,
        light_bytes,
        map_bytes,
        readback_bytes,
    ) = struct.unpack_from("<4sBBBBIIIIIII", binary)
    if magic != b"PA5B" or version != 1:
        raise ValueError(f"unsupported PA5 schema: magic={magic!r} version={version}")
    if stage_count != len(PA5_STAGE_LABELS) or field_count != FIELD_COUNT:
        raise ValueError(
            f"unexpected PA5 dimensions: stages={stage_count} fields={field_count}"
        )
    if variant >= len(PA5_VARIANTS):
        raise ValueError(f"unknown PA5 variant {variant}")

    embedded_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    calculated_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    try:
        displayed_crc = int(suffix_crc, 16)
    except ValueError as exc:
        raise ValueError(f"invalid PA5 CRC suffix: {suffix_crc!r}") from exc
    if embedded_crc != calculated_crc or displayed_crc != calculated_crc:
        raise ValueError(
            "PA5 CRC mismatch: "
            f"suffix={displayed_crc:08X} embedded={embedded_crc:08X} "
            f"calculated={calculated_crc:08X}"
        )

    stages: list[dict[str, object]] = []
    offset = 36
    for expected_stage, label in enumerate(PA5_STAGE_LABELS):
        values = struct.unpack_from(f"<{FIELD_COUNT}I", binary, offset)
        offset += FIELD_COUNT * 4
        (
            stage_tick,
            spu,
            reverb_volume,
            reverb_base_ext_left,
            ext_right_eon_low,
            eon_high_cfg_nonzero,
            cfg_hash,
            event_vblanks,
            detail_a,
            detail_b,
        ) = values
        stage_id = stage_tick >> 24
        if stage_id != expected_stage:
            raise ValueError(f"PA5 stage order mismatch: got {stage_id}, want {expected_stage}")
        final = expected_stage + 1 == len(PA5_STAGE_LABELS)
        stages.append(
            {
                "name": label,
                "tick": stage_tick & 0x00FF_FFFF,
                "spucnt": spu >> 16,
                "spustat": spu & 0xFFFF,
                "reverb_volume_left": reverb_volume >> 16,
                "reverb_volume_right": reverb_volume & 0xFFFF,
                "reverb_base": reverb_base_ext_left >> 16,
                "external_volume_left": reverb_base_ext_left & 0xFFFF,
                "external_volume_right": ext_right_eon_low >> 16,
                "reverb_on_low": ext_right_eon_low & 0xFFFF,
                "reverb_on_high": eon_high_cfg_nonzero >> 16,
                "reverb_cfg_nonzero": eon_high_cfg_nonzero & 0xFFFF,
                "reverb_cfg_hash": cfg_hash,
                "event_vblanks": event_vblanks,
                "clock_before": None if final else detail_a,
                "clock_after": None if final else detail_b,
                "expected_hash": detail_a if final else None,
                "observed_hash": detail_b if final else None,
                "readback_matches": detail_a == detail_b if final else None,
            }
        )

    boot_reverb_cfg = struct.unpack_from("<32H", binary, offset)
    offset += 64
    if offset != len(binary) - 4:
        raise ValueError(f"PA5 layout ended at {offset}, expected {len(binary) - 4}")

    return {
        "schema": "PA5",
        "schema_version": version,
        "run": run,
        "layout_crc": layout_crc,
        "variant": PA5_VARIANTS[variant],
        "variant_code": variant,
        "wait_vblanks": wait_vblanks,
        "full_bytes": full_bytes,
        "light_bytes": light_bytes,
        "map_bytes": map_bytes,
        "readback_bytes": readback_bytes,
        "boot_reverb_cfg": boot_reverb_cfg,
        "crc32": calculated_crc,
        "stages": stages,
    }


def print_report(report: dict[str, object]) -> None:
    if report["schema"] == "PA5":
        print_pa5_report(report)
        return
    if report["schema"] == "PA4":
        print_pa4_report(report)
        return
    if report["schema"] == "PA3":
        print_pa3_report(report)
        return
    if report["schema"] == "PA2":
        print_pa2_report(report)
        return
    print(
        f"# PA1 run={report['run']} lba={report['lba']} "
        f"stage_frames={report['stage_frames']} crc={report['crc32']:08X}"
    )
    print(
        "stage                         tick SPUCNT SPUSTAT CDVOL_L CDVOL_R "
        "CDSTAT IRQ CMD SECT  NZ_L NZ_R PEAK_L PEAK_R ENERGY_L ENERGY_R HASH_L   HASH_R"
    )
    for stage in report["stages"]:
        print(
            f"{stage['name']:<29} {stage['tick']:>4} "
            f"{stage['spucnt']:04X}   {stage['spustat']:04X}    "
            f"{stage['cd_volume_left']:04X}    {stage['cd_volume_right']:04X}    "
            f"{stage['cd_status']:02X}     {stage['cd_irq']:02X}  "
            f"{stage['command_state']:02X}  {stage['sectors']:04X}  "
            f"{stage['left_nonzero']:>4} {stage['right_nonzero']:>4} "
            f"{stage['left_peak']:>6} {stage['right_peak']:>6} "
            f"{stage['left_energy']:>8} {stage['right_energy']:>8} "
            f"{stage['left_hash']:08X} {stage['right_hash']:08X}"
        )


def print_pa2_report(report: dict[str, object]) -> None:
    print(
        f"# PA2 run={report['run']} layout={report['layout_crc']:08X} "
        f"target={report['target_len']}B@{report['target_rate']}Hz "
        f"target_hash={report['target_hash']:08X} crc={report['crc32']:08X}"
    )
    print(
        "stage                         tick SPUCNT SPUSTAT VOL_L VOL_R PITCH START "
        "CURVOL REPEAT ENDX     V15_END MODE DRAIN TAIL"
    )
    for stage in report["stages"]:
        print(
            f"{stage['name']:<29} {stage['tick']:>4} "
            f"{stage['spucnt']:04X}   {stage['spustat']:04X}    "
            f"{stage['volume_left']:04X}  {stage['volume_right']:04X}  "
            f"{stage['pitch']:04X}  {stage['start']:04X}  "
            f"{stage['current_volume']:04X}   {stage['repeat']:04X}   "
            f"{stage['endx']:08X} {'yes' if stage['voice15_ended'] else ' no':>7} "
            f"{stage['max_mode_polls']:>4} {stage['max_drain_polls']:>5} "
            f"{'MATCH' if stage['tail_matches'] else 'MISMATCH'} "
            f"{stage['expected_tail_hash']:08X}/{stage['observed_tail_hash']:08X}"
        )


def print_pa3_report(report: dict[str, object]) -> None:
    print(
        f"# PA3 run={report['run']} layout={report['layout_crc']:08X} "
        f"full={report['full_bytes']}B light={report['light_bytes']}B "
        f"map={report['map_bytes']}B readback={report['readback_bytes']}B "
        f"crc={report['crc32']:08X}"
    )
    print(
        "stage                         tick SPUCNT SPUSTAT ENDX     VBLANKS "
        "V0(VOL/ENV) V15(VOL/ENV) V16(VOL/ENV) V17(VOL/ENV) READBACK"
    )
    for stage in report["stages"]:
        states = [stage[f"voice{voice}"] for voice in (0, 15, 16, 17)]
        formatted = " ".join(
            f"{state['mixer_volume']:04X}/{state['current_envelope']:04X}" for state in states
        )
        print(
            f"{stage['name']:<29} {stage['tick']:>4} "
            f"{stage['spucnt']:04X}   {stage['spustat']:04X}    "
            f"{stage['endx']:08X} {stage['event_vblanks']:>7} "
            f"{formatted} "
            f"{'MATCH' if stage['readback_matches'] else 'MISMATCH'} "
            f"{stage['expected_hash']:08X}/{stage['observed_hash']:08X}"
        )


def print_pa4_report(report: dict[str, object]) -> None:
    print(
        f"# PA4 run={report['run']} variant={report['variant']} "
        f"wait={report['wait_vblanks']} layout={report['layout_crc']:08X} "
        f"full={report['full_bytes']}B light={report['light_bytes']}B "
        f"map={report['map_bytes']}B readback={report['readback_bytes']}B "
        f"crc={report['crc32']:08X}"
    )
    print(
        "stage                         tick SPUCNT SPUSTAT ENDX     V16_VOL ENV  "
        "PITCH START ADSR REPEAT VOICES   VBLANKS CLOCK / READBACK"
    )
    for stage in report["stages"]:
        if stage["clock_before"] is not None:
            detail = f"CLOCK {stage['clock_before']:08X}->{stage['clock_after']:08X}"
        else:
            detail = (
                f"{'MATCH' if stage['readback_matches'] else 'MISMATCH'} "
                f"{stage['expected_hash']:08X}/{stage['observed_hash']:08X}"
            )
        print(
            f"{stage['name']:<29} {stage['tick']:>4} "
            f"{stage['spucnt']:04X}   {stage['spustat']:04X}    "
            f"{stage['endx']:08X} {stage['voice16_volume']:04X}    "
            f"{stage['voice16_envelope']:04X} {stage['voice16_pitch']:04X}  "
            f"{stage['voice16_start']:04X}  {stage['voice16_adsr']:04X} "
            f"{stage['voice16_repeat']:04X}  {stage['nonzero_voice_mask']:08X} "
            f"{stage['event_vblanks']:>7} "
            f"{detail}"
        )


def print_pa5_report(report: dict[str, object]) -> None:
    print(
        f"# PA5 run={report['run']} variant={report['variant']} "
        f"wait={report['wait_vblanks']} layout={report['layout_crc']:08X} "
        f"full={report['full_bytes']}B light={report['light_bytes']}B "
        f"map={report['map_bytes']}B readback={report['readback_bytes']}B "
        f"crc={report['crc32']:08X}"
    )
    print(
        "stage                         tick SPUCNT SPUSTAT RVOL_L RVOL_R BASE "
        "EXT_L EXT_R EON      CFG_N CFG_HASH VBLANKS CLOCK / READBACK"
    )
    for stage in report["stages"]:
        if stage["clock_before"] is not None:
            detail = f"CLOCK {stage['clock_before']:08X}->{stage['clock_after']:08X}"
        else:
            detail = (
                f"{'MATCH' if stage['readback_matches'] else 'MISMATCH'} "
                f"{stage['expected_hash']:08X}/{stage['observed_hash']:08X}"
            )
        eon = (stage["reverb_on_high"] << 16) | stage["reverb_on_low"]
        print(
            f"{stage['name']:<29} {stage['tick']:>4} "
            f"{stage['spucnt']:04X}   {stage['spustat']:04X}    "
            f"{stage['reverb_volume_left']:04X}   {stage['reverb_volume_right']:04X}   "
            f"{stage['reverb_base']:04X} {stage['external_volume_left']:04X}  "
            f"{stage['external_volume_right']:04X}  {eon:08X} "
            f"{stage['reverb_cfg_nonzero']:>5} {stage['reverb_cfg_hash']:08X} "
            f"{stage['event_vblanks']:>7} {detail}"
        )
    cfg = report["boot_reverb_cfg"]
    print("boot_reverb_cfg:")
    for index in range(0, len(cfg), 8):
        words = " ".join(f"{value:04X}" for value in cfg[index : index + 8])
        print(f"  {index:02d}: {words}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "payload_or_file",
        help="decoded PA1/... through PA5/... QR text, or a text file containing it",
    )
    args = parser.parse_args()
    source = args.payload_or_file
    path = pathlib.Path(source)
    if path.is_file():
        source = path.read_text(encoding="utf-8", errors="replace")
    try:
        print_report(decode_payload(extract_payload(source)))
    except (OSError, ValueError) as exc:
        print(f"hwtest-audio-report: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
