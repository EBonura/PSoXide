#!/usr/bin/env python3
"""Validate and assemble PSoXide hardware-test photo payloads.

PX5 is the dense two-page Base64 format and includes every conformance
observation/status, timing envelope, memory-control register, and startup scan
summary.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import pathlib
import struct
import sys
from dataclasses import dataclass


LABELS = {
    0x00: "timer2_empty_harness",
    0x01: "nop_block",
    0x02: "dependent_alu",
    0x03: "cached_load_hazard",
    0x04: "taken_branch_delay",
    0x05: "multu_mflo_small",
    0x06: "multu_mflo_medium",
    0x07: "multu_mflo_large",
    0x08: "divu_mflo",
    0x09: "scratchpad_load_hazard",
    0x0A: "uncached_ram_load_hazard",
    0x0B: "ram_store",
    0x0C: "scratchpad_store",
    0x0D: "uncached_ram_store",
    0x0E: "gpustat_read_hazard",
    0x0F: "irqstat_read_hazard",
    0x10: "spin_64_system",
    0x11: "spin_64_div8",
    0x12: "spin_64_dot",
    0x13: "spin_256_system",
    0x14: "spin_256_div8",
    0x15: "spin_256_dot",
    0x16: "spin_1024_system",
    0x17: "spin_1024_div8",
    0x18: "spin_1024_dot",
    0x19: "spin_4096_system",
    0x1A: "spin_4096_div8",
    0x1B: "spin_4096_dot",
    0x1C: "icache_cold_4k",
    0x1D: "icache_warm_4k",
    0x20: "timer1_hblank_long",
    0x21: "gte_rtps_commands",
    0x22: "gte_rtpt_commands",
    0x23: "gte_nclip_commands",
    0x24: "gte_mvmva_commands",
    0x25: "gte_ncdt_commands",
    0x26: "gte_ncct_commands",
    0x30: "dma_otc_16_words",
    0x31: "dma_otc_64_words",
    0x32: "dma_otc_256_words",
    0x40: "cdrom_getstat_ack",
    0x41: "gpu_irq1_settle",
    0x42: "icache_cold_entry_word0",
    0x43: "icache_cold_entry_word1",
    0x44: "icache_cold_entry_word2",
    0x45: "icache_warm_entry_word0",
    0x46: "branch_not_taken",
    0x47: "cached_ram_byte_load_hazard",
    0x48: "cached_ram_half_load_hazard",
    0x49: "uncached_ram_byte_load_hazard",
    0x4A: "uncached_ram_half_load_hazard",
    0x4B: "bios_rom_word_load_hazard",
    0x4C: "bios_rom_half_load_hazard",
    0x4D: "bios_rom_byte_load_hazard",
    0x4E: "spustat_half_load_hazard",
    0x4F: "sio_stat_word_load_hazard",
    0x50: "cached_ram_byte_store",
    0x51: "cached_ram_half_store",
    0x52: "cdrom_byte_load_hazard",
    0x53: "cdrom_half_load_hazard",
    0x54: "cdrom_word_load_hazard",
    0x55: "expansion1_byte_load_hazard",
    0x56: "expansion1_half_load_hazard",
    0x57: "expansion1_word_load_hazard",
    0x58: "expansion2_byte_load_hazard",
    0x59: "expansion2_half_load_hazard",
    0x5A: "expansion2_word_load_hazard",
    0x5B: "expansion3_byte_load_hazard",
    0x5C: "expansion3_half_load_hazard",
    0x5D: "expansion3_word_load_hazard",
    0x5E: "spustat_byte_load_hazard",
    0x5F: "spu_aligned_word_load_hazard",
    0x60: "cache_control_byte_load_hazard",
    0x61: "cache_control_half_load_hazard",
    0x62: "cache_control_word_load_hazard",
    0x63: "memory_control_word_load_hazard",
    0x64: "spu_unaligned_lwl_lwr_pair",
    0x65: "spu_dma_write_512_halfwords",
    0x66: "gpu_dma_block_16x1",
    0x67: "gpu_dma_block_16x4",
    0x68: "gpu_dma_block_16x16",
    0x69: "gpu_dma_block_64x4",
    0x6A: "gpu_dma_block_256x1",
    0x6B: "gpu_dma_linked_2x128",
    0x6C: "gpu_line_mono_16x16",
    0x6D: "gpu_line_mono_256x8",
    0x6E: "gpu_line_gouraud_16x16",
    0x6F: "gpu_line_gouraud_256x8",
    0x70: "dram_refresh_period_cycles",
    0x71: "dram_refresh_stall_cycles",
}

GTE_SETTLE_FIRST_CASE = 116
GTE_SETTLE_CASE_COUNT = 22

# Work is fixed by record ID, so PX5 stores only each timing minimum/maximum.
WORK_BY_ID = {
    0x00: 0,
    0x01: 128,
    0x02: 128,
    0x03: 64,
    0x04: 64,
    0x05: 16,
    0x06: 16,
    0x07: 16,
    0x08: 8,
    **{record_id: 64 for record_id in range(0x09, 0x13)},
    **{record_id: 256 for record_id in range(0x13, 0x16)},
    **{record_id: 1024 for record_id in range(0x16, 0x19)},
    **{record_id: 4096 for record_id in range(0x19, 0x1C)},
    0x1C: 1024,
    0x1D: 1024,
    0x20: 0xFFFF,
    0x21: 16,
    0x22: 8,
    0x23: 16,
    0x24: 16,
    0x25: 4,
    0x26: 4,
    0x30: 16,
    0x31: 64,
    0x32: 256,
    **{record_id: 1 for record_id in range(0x40, 0x46)},
    **{record_id: 64 for record_id in range(0x46, 0x65)},
    0x65: 512,
    0x66: 16,
    0x67: 64,
    0x68: 256,
    0x69: 256,
    0x6A: 256,
    0x6B: 258,
    0x6C: 272,
    0x6D: 2056,
    0x6E: 272,
    0x6F: 2056,
    0x70: 4096,
    0x71: 4096,
}
RECORD_IDS = tuple(WORK_BY_ID)


@dataclass(frozen=True)
class Record:
    record_id: int
    work: int
    minimum: int
    maximum: int


@dataclass(frozen=True)
class Px5Page:
    number: int
    total: int
    chunk: str
    crc: int


@dataclass(frozen=True)
class ScanSummary:
    status: int
    items: int
    digest: int
    aux: int
    run: int


@dataclass(frozen=True)
class Px5Capture:
    version: int
    conformance_run: int
    timing_run: int
    conformance_digest: int
    gte_digest: int
    timing_digest: int
    timing_aux: int
    scans: tuple[ScanSummary, ...]
    observations: tuple[int, ...]
    statuses: tuple[int, ...]
    records: tuple[Record, ...]
    memory_control: tuple[int, ...]
    binary_crc: int


PX5_TEST_COUNT = 173
PX5_SCAN_COUNT = 3
PX5_STATUS_BITS = 3
PX5_BINARY_LEN = 1_221
PX5_PAGE_COUNT = 2
STATUS_LABELS = {0: "PENDING", 1: "PASS", 2: "FAIL", 3: "WARN", 4: "INFO"}


def parse_px5_page(payload: str) -> Px5Page:
    payload = payload.strip()
    if not payload.startswith("PX5/"):
        raise ValueError("not a PX5 hardware payload")
    try:
        body, claimed_crc = payload.rsplit("/C:", 1)
        marker, page_field, chunk = body.split("/", 2)
    except ValueError as exc:
        raise ValueError("malformed PX5 page") from exc
    if marker != "PX5" or len(page_field) != 4 or not chunk:
        raise ValueError("malformed PX5 page header")
    actual_crc = binascii.crc32(chunk.encode("ascii")) & 0xFFFF_FFFF
    if int(claimed_crc, 16) != actual_crc:
        raise ValueError(
            f"PX5 page CRC mismatch: payload says {claimed_crc}, "
            f"calculated {actual_crc:08X}"
        )
    return Px5Page(
        int(page_field[:2], 16),
        int(page_field[2:], 16),
        chunk,
        actual_crc,
    )


def parse_px5_capture(payloads: list[str]) -> Px5Capture:
    pages = [parse_px5_page(payload) for payload in payloads]
    if not pages:
        raise ValueError("no PX5 payloads found")
    totals = {page.total for page in pages}
    if totals != {PX5_PAGE_COUNT}:
        raise ValueError(f"PX5 must declare exactly {PX5_PAGE_COUNT} pages")
    # The log can contain an early boot page followed by a freshly encoded
    # page after the pad state settles. Keep the last occurrence, matching the
    # state that is ultimately photographed.
    by_number = {page.number: page for page in pages}
    missing = sorted(set(range(1, PX5_PAGE_COUNT + 1)) - set(by_number))
    if missing:
        raise ValueError("missing PX5 page(s): " + ", ".join(map(str, missing)))

    encoded = "".join(by_number[number].chunk for number in range(1, PX5_PAGE_COUNT + 1))
    try:
        binary = base64.b64decode(encoded, validate=True)
    except binascii.Error as exc:
        raise ValueError(f"invalid PX5 Base64: {exc}") from exc
    if len(binary) != PX5_BINARY_LEN:
        raise ValueError(
            f"PX5 binary length is {len(binary)}, expected {PX5_BINARY_LEN}"
        )
    claimed_binary_crc = struct.unpack_from("<I", binary, len(binary) - 4)[0]
    actual_binary_crc = binascii.crc32(binary[:-4]) & 0xFFFF_FFFF
    if claimed_binary_crc != actual_binary_crc:
        raise ValueError(
            f"PX5 binary CRC mismatch: payload says {claimed_binary_crc:08X}, "
            f"calculated {actual_binary_crc:08X}"
        )

    if binary[:4] != b"PX5B":
        raise ValueError("PX5 binary magic mismatch")
    version = binary[4]
    if version != 1:
        raise ValueError(f"unsupported PX5 binary version {version}")
    conformance_run = binary[5]
    timing_run = binary[6]
    test_count = binary[7]
    timing_count = binary[8]
    memory_count = binary[9]
    status_bits = binary[10]
    scan_count = binary[11]
    expected_shape = (
        PX5_TEST_COUNT,
        len(RECORD_IDS),
        9,
        PX5_STATUS_BITS,
        PX5_SCAN_COUNT,
    )
    if (test_count, timing_count, memory_count, status_bits, scan_count) != expected_shape:
        raise ValueError("PX5 binary shape does not match schema version 1")

    conformance_digest, gte_digest, timing_digest, timing_aux = struct.unpack_from(
        "<IIII", binary, 12
    )
    offset = 28
    scans: list[ScanSummary] = []
    for _ in range(scan_count):
        status, items, digest, aux, run = struct.unpack_from("<BH I I B", binary, offset)
        scans.append(ScanSummary(status, items, digest, aux, run))
        offset += 12

    observations = struct.unpack_from(f"<{test_count}I", binary, offset)
    offset += test_count * 4

    packed_status_len = (test_count * status_bits + 7) // 8
    packed_statuses = binary[offset : offset + packed_status_len]
    offset += packed_status_len
    statuses: list[int] = []
    for index in range(test_count):
        bit = index * status_bits
        window = packed_statuses[bit // 8]
        if bit // 8 + 1 < len(packed_statuses):
            window |= packed_statuses[bit // 8 + 1] << 8
        status = (window >> (bit % 8)) & 0x7
        if status > 4:
            raise ValueError(f"invalid PX5 status {status} for case {index}")
        statuses.append(status)

    records: list[Record] = []
    for record_id in RECORD_IDS:
        minimum, maximum = struct.unpack_from("<HH", binary, offset)
        offset += 4
        records.append(Record(record_id, WORK_BY_ID[record_id], minimum, maximum))
    memory_control = struct.unpack_from(f"<{memory_count}I", binary, offset)
    offset += memory_count * 4
    if offset != len(binary) - 4:
        raise ValueError("PX5 binary parser did not consume the complete payload")

    return Px5Capture(
        version,
        conformance_run,
        timing_run,
        conformance_digest,
        gte_digest,
        timing_digest,
        timing_aux,
        tuple(scans),
        tuple(observations),
        tuple(statuses),
        tuple(records),
        tuple(memory_control),
        claimed_binary_crc,
    )


def payloads_from_paths(paths: list[str]) -> list[str]:
    prefix = "PX5/"

    def from_text(text: str) -> list[str]:
        found: list[str] = []
        for line in text.splitlines():
            position = line.find(prefix)
            if position >= 0:
                found.append(line[position:].split()[0])
        return found

    payloads: list[str] = []
    for value in paths:
        if value.startswith(prefix):
            payloads.append(value)
            continue
        text = pathlib.Path(value).read_text(encoding="utf-8")
        payloads.extend(from_text(text))
    if not paths:
        payloads.extend(from_text(sys.stdin.read()))
    return payloads


def print_px5_report(capture: Px5Capture, baseline: Px5Capture | None) -> int:
    print(
        f"# schema=PX5 pages={PX5_PAGE_COUNT} run={capture.timing_run:02X} "
        f"digest={capture.timing_digest:08X} records={len(capture.records)} "
        f"binary_crc={capture.binary_crc:08X}"
    )
    print(
        f"# conformance_run={capture.conformance_run:02X} "
        f"digest={capture.conformance_digest:08X} cases={len(capture.observations)}"
    )
    case_columns = "case,status,observed"
    if baseline is not None:
        case_columns += ",baseline_observed,changed"
    print(case_columns)
    for index, (status, observed) in enumerate(
        zip(capture.statuses, capture.observations)
    ):
        row = f"{index},{STATUS_LABELS[status]},0x{observed:08X}"
        if baseline is not None:
            prior = baseline.observations[index]
            row += f",0x{prior:08X},{int(prior != observed)}"
        print(row)

    columns = "id,label,work,min,max,jitter"
    if baseline is not None:
        columns += ",baseline_min,delta_min"
    print(columns)
    baseline_records = (
        {record.record_id: record for record in baseline.records}
        if baseline is not None
        else None
    )
    for record in capture.records:
        row = (
            f"{record.record_id:02X},{LABELS[record.record_id]},{record.work},"
            f"{record.minimum},{record.maximum},{record.maximum - record.minimum}"
        )
        if baseline_records is not None:
            prior = baseline_records[record.record_id]
            row += f",{prior.minimum},{record.minimum - prior.minimum:+d}"
        print(row)

    scan_names = ("cpu", "gte", "spu")
    print(
        "# scans="
        + ",".join(
            f"{name}:status={STATUS_LABELS[scan.status]}:items={scan.items}:"
            f"digest={scan.digest:08X}:aux={scan.aux:08X}:run={scan.run:02X}"
            for name, scan in zip(scan_names, capture.scans)
        )
    )
    names = (
        "exp1_base",
        "exp2_base",
        "exp1_delay",
        "exp3_delay",
        "bios_delay",
        "spu_delay",
        "cdrom_delay",
        "exp2_delay",
        "common_delay",
    )
    print(
        "# memory_control="
        + ",".join(
            f"{name}:0x{value:08X}"
            for name, value in zip(names, capture.memory_control)
        )
    )
    settle = capture.observations[
        GTE_SETTLE_FIRST_CASE : GTE_SETTLE_FIRST_CASE + GTE_SETTLE_CASE_COUNT
    ]
    print(
        f"# gte_settle_run={capture.conformance_run:02X} "
        f"digest={capture.gte_digest:08X} cases={len(settle)}"
    )
    print(
        "# gte_settle="
        + ",".join(
            f"{GTE_SETTLE_FIRST_CASE + offset}:0x{value:08X}"
            for offset, value in enumerate(settle)
        )
    )
    if baseline is not None:
        baseline_settle = baseline.observations[
            GTE_SETTLE_FIRST_CASE : GTE_SETTLE_FIRST_CASE + GTE_SETTLE_CASE_COUNT
        ]
        print(
            "# gte_settle_delta="
            + ",".join(
                f"{GTE_SETTLE_FIRST_CASE + offset}:{value - prior:+d}"
                for offset, (value, prior) in enumerate(zip(settle, baseline_settle))
            )
        )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--baseline",
        help="optional PX5 payload file to compare against",
    )
    parser.add_argument("payload_or_file", nargs="*")
    args = parser.parse_args()
    try:
        capture = parse_px5_capture(payloads_from_paths(args.payload_or_file))
        baseline = (
            parse_px5_capture(payloads_from_paths([args.baseline]))
            if args.baseline
            else None
        )
        return print_px5_report(capture, baseline)
    except (OSError, UnicodeError, ValueError) as exc:
        print(f"hwtest-report: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
