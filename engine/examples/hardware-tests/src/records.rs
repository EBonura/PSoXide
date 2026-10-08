// SPDX-License-Identifier: GPL-2.0-or-later
//! Timing records of the linear run, one function per area. Split out of the
//! old single `push_standard_records`; the probes themselves are unchanged.

use super::*;

/// CPU, RAM and the bus: instruction and memory-access costs.
pub(crate) fn push_cpu_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    push_timing_record(records, next, sample_timing(0x00, 0, timed_empty));
    push_timing_record(
        records,
        next,
        sample_timing(0x05, 16, || timed_multu_mflo(0x0000_07FF)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x06, 16, || timed_multu_mflo(0x000F_FFFF)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x07, 16, || timed_multu_mflo(0x1357_2468)),
    );
    push_timing_record(records, next, sample_timing(0x08, 8, timed_divu_mflo));
    push_timing_record(
        records,
        next,
        sample_timing(0x0A, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x0D, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_stores_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x1C, 1024, || call_uncached_timing(timed_icache_cold)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x1D, 1024, || call_uncached_timing(timed_icache_warm)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x42, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w0)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x43, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w1)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x44, 1, || {
            call_uncached_entry_timing(timed_icache_entry_cold, __hwtest_icache_entry_w2)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x45, 1, || {
            call_uncached_entry_timing(timed_icache_entry_warm, __hwtest_icache_entry_w0)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x46, 64, timed_untaken_branches),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x47, 64, || {
            timed_byte_load_hazards_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x48, 64, || {
            timed_half_load_hazards_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x49, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_byte_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4A, 64, || {
            let cached = (&raw const TIMING_WORD) as u32;
            timed_half_load_hazards_at(0xA000_0000 | (cached & 0x001F_FFFF))
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4B, 64, || timed_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4C, 64, || timed_half_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4D, 64, || timed_byte_load_hazards_at(0xBFC0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4E, 64, || timed_half_load_hazards_at(0x1F80_1DAE)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x4F, 64, || timed_load_hazards_at(0x1F80_1044)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x50, 64, || {
            timed_byte_stores_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x51, 64, || {
            timed_half_stores_at((&raw const TIMING_WORD) as u32)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x52, 64, || timed_byte_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x53, 64, || timed_half_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x54, 64, || timed_load_hazards_at(0x1F80_1800)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x55, 64, || timed_byte_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x56, 64, || timed_half_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x57, 64, || timed_load_hazards_at(0x1F00_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x58, 64, || timed_byte_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x59, 64, || timed_half_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5A, 64, || timed_load_hazards_at(0x1F80_2000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5B, 64, || timed_byte_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5C, 64, || timed_half_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5D, 64, || timed_load_hazards_at(0x1FA0_0000)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5E, 64, || timed_byte_load_hazards_at(0x1F80_1DAE)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x5F, 64, || timed_load_hazards_at(0x1F80_1DAC)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x60, 64, || timed_byte_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x61, 64, || timed_half_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x62, 64, || timed_load_hazards_at(0xFFFE_0130)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x63, 64, || timed_load_hazards_at(0x1F80_1010)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x64, 64, || timed_unaligned_word_loads_at(0x1F80_1DAA)),
    );
    let (refresh_period, refresh_stall) = sample_dram_refresh();
    push_timing_record(records, next, refresh_period);
    push_timing_record(records, next, refresh_stall);
}

/// Timers, the interrupt controller and the DMA controller.
pub(crate) fn push_irq_dma_timer_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    // Stay below the 16-bit root-counter wrap even when silicon RAM/MMIO is
    // slower than the emulator. Geometric points let the host fit a slope and
    // intercept instead of treating harness overhead as per-iteration cost.
    const SPINS: [u32; 4] = [64, 256, 1024, 4096];

    push_timing_record(
        records,
        next,
        sample_timing(0x0F, 64, || timed_load_hazards_at(psx_hw::irq::I_STAT)),
    );

    for (set, spin_count) in SPINS.into_iter().enumerate() {
        let base = 0x10 + set as u16 * 3;
        push_timing_record(
            records,
            next,
            sample_timing(base, spin_count as u16, || {
                timer_delta(timers::Timer::Timer2, 0, spin_count)
            }),
        );
        push_timing_record(
            records,
            next,
            sample_timing(base + 1, spin_count as u16, || {
                timer_delta(timers::Timer::Timer2, TIMER_MODE_CLOCK_SOURCE_2, spin_count)
            }),
        );
        push_timing_record(
            records,
            next,
            sample_timing(base + 2, spin_count as u16, || {
                timer_delta(timers::Timer::Timer0, TIMER_MODE_CLOCK_SOURCE_1, spin_count)
            }),
        );
    }

    push_timing_record(
        records,
        next,
        sample_timing(0x20, 0xFFFF, || {
            timer_delta(timers::Timer::Timer1, TIMER_MODE_CLOCK_SOURCE_1, 0x20000)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x30, 16, || timed_otc_dma_cycles(16)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x31, 64, || timed_otc_dma_cycles(64)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x32, 256, || timed_otc_dma_cycles(256)),
    );
}

/// GTE command latencies.
pub(crate) fn push_gte_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    push_timing_record(
        records,
        next,
        sample_timing(0x21, 16, timed_gte_rtps_commands),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x22, 8, timed_gte_rtpt_commands),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x23, 16, timed_gte_nclip_commands),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x24, 16, timed_gte_mvmva_commands),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x25, 4, timed_gte_ncdt_commands),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x26, 4, timed_gte_ncct_commands),
    );
}

/// GPU and MDEC records.
pub(crate) fn push_gpu_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    push_timing_record(
        records,
        next,
        sample_timing(0x0E, 64, || timed_load_hazards_at(0x1F80_1814)),
    );
    // MDEC. No coverage at all before now. Command number lives in bits 31..29:
    // 2 = set quant table, 3 = set scale table, 1 = decode.
    push_timing_record(
        records,
        next,
        // Set quant table, luma only: 64 bytes = 16 words.
        sample_timing(0xB0, 16, || timed_mdec(0x4000_0000, 16)),
    );
    push_timing_record(
        records,
        next,
        // Set quant table, luma + chroma: 128 bytes = 32 words.
        sample_timing(0xB1, 32, || timed_mdec(0x4000_0001, 32)),
    );
    push_timing_record(
        records,
        next,
        // Set scale (IDCT) table: 64 halfwords = 32 words.
        sample_timing(0xB2, 32, || timed_mdec(0x6000_0000, 32)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xB3, 1, timed_mdec_reset_settle),
    );
    // Decode-to-drained for one and four colour macroblocks. Two points, so
    // the host can separate per-macroblock cost from command setup.
    push_timing_record(
        records,
        next,
        sample_timing(0xB4, 1, || timed_mdec_decode(1)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xB5, 2, || timed_mdec_decode(2)),
    );
    push_timing_record(records, next, sample_timing(0x41, 1, timed_gpu_irq_settle));
    push_timing_record(
        records,
        next,
        sample_timing(0x66, 16, || timed_gpu_dma_block(16, 1)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x67, 64, || timed_gpu_dma_block(16, 4)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x68, 256, || timed_gpu_dma_block(16, 16)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x69, 256, || timed_gpu_dma_block(64, 4)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6A, 256, || timed_gpu_dma_block(256, 1)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6B, 258, timed_gpu_dma_linked_2x128),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6C, 272, || timed_gpu_line_batch(false, 16, 16)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6D, 2056, || timed_gpu_line_batch(false, 256, 8)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6E, 272, || timed_gpu_line_batch(true, 16, 16)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x6F, 2056, || timed_gpu_line_batch(true, 256, 8)),
    );
}

/// SPU DMA cost.
pub(crate) fn push_spu_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    push_timing_record(
        records,
        next,
        sample_timing(0x65, 512, timed_spu_dma_write_512_halfwords),
    );
}

/// Polled CD timing: seeks, throughput, command round trips, CD-DA contention.
pub(crate) fn push_cd_records(records: &mut [TimingRecord; TIMING_RECORD_COUNT], next: &mut usize) {
    push_timing_record(records, next, sample_timing(0x40, 1, timed_cdrom_getstat));
    // CD battery. Timed on Timer 1 HBlank ticks, not Timer 2 cycles, and each
    // record repeats through sample_timing so a retry or a bad block shows up
    // as a min/max spread instead of silently becoming the answer.
    push_timing_record(
        records,
        next,
        sample_timing(0x90, 1, || cd_seek_distance(1)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x91, 16, || cd_seek_distance(16)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x92, 128, || cd_seek_distance(128)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x93, 512, || cd_seek_distance(512)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x94, 8, || cd_read_throughput(false, 8)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x95, 8, || cd_read_throughput(true, 8)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x96, 1, || {
            cd_timed(|| psx_io::cd::try_status(CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x97, 1, || {
            cd_timed(|| psx_io::cd::try_set_mode(0, CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x98, 1, || {
            cd_timed(|| psx_io::cd::try_play_position(CD_SPINS).is_some())
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x99, 1, || {
            cd_timed(|| cd_command_until_complete_timed(psx_hw::cd::CMD_PAUSE, &[]))
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x9A, 1, || {
            cd_timed(|| cd_command_until_complete_timed(psx_hw::cd::CMD_INIT, &[]))
        }),
    );
    // CD-DA contention. 0x9B against 0x9C is the whole point: identical read
    // path, identical sector count, the only difference being live audio.
    push_timing_record(
        records,
        next,
        sample_timing(0x9B, 8, || cd_read_with_audio(true, 8)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0x9C, 8, || cd_read_with_audio(false, 8)),
    );
    push_timing_record(records, next, sample_timing(0x9D, 1, cd_play_start));
    // Seek sweep. Four distances proved too few to model: the console measured
    // +128 slower than +512, and no monotonic fit came within 2x of the middle
    // points. Ten distances with the same five repeats each make an outlier
    // visible AS an outlier rather than as the shape of the curve.
    push_timing_record(
        records,
        next,
        sample_timing(0xC0, 2, || cd_seek_distance(2)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC1, 4, || cd_seek_distance(4)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC2, 8, || cd_seek_distance(8)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC3, 32, || cd_seek_distance(32)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC4, 64, || cd_seek_distance(64)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC5, 256, || cd_seek_distance(256)),
    );
    // Backward seeks at the same distances. A drive settles differently
    // approaching from outside, and every existing record seeks forward only,
    // so a direction asymmetry would currently be invisible.
    push_timing_record(
        records,
        next,
        sample_timing(0xC6, 64, || cd_seek_backward(64)),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xC7, 256, || cd_seek_backward(256)),
    );
    push_timing_record(
        records,
        next,
        // Must establish playback itself: 0x9D pauses and mutes when it
        // finishes, so measuring "during CD-DA" without restarting audio would
        // silently measure the idle case instead.
        sample_timing(0x9E, 1, cd_getlocp_during_playback),
    );
}

/// Controller port timing.
pub(crate) fn push_sio_records(
    records: &mut [TimingRecord; TIMING_RECORD_COUNT],
    next: &mut usize,
) {
    // SIO: the same poll at four pacing configurations.
    push_timing_record(
        records,
        next,
        sample_timing(0xB6, 1, || {
            timed_pad_poll(PROBE_VARIANTS[0].1, PROBE_VARIANTS[0].2)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xB7, 1, || {
            timed_pad_poll(PROBE_VARIANTS[1].1, PROBE_VARIANTS[1].2)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xB8, 1, || {
            timed_pad_poll(PROBE_VARIANTS[2].1, PROBE_VARIANTS[2].2)
        }),
    );
    push_timing_record(
        records,
        next,
        sample_timing(0xB9, 1, || {
            timed_pad_poll(PROBE_VARIANTS[3].1, PROBE_VARIANTS[3].2)
        }),
    );
    // Setup-delay sweep. The console answered at setup 0, gave NO reply at 128,
    // and answered again at 384: non-monotonic, so the threshold cannot be read
    // off four points. Twelve evenly spaced delays bracket where a real pad
    // starts replying, which is the SCPH-1200 problem stated as a measurement.
    let mut sweep = 0usize;
    while sweep < SIO_SETUP_SWEEP.len() {
        let setup = SIO_SETUP_SWEEP[sweep];
        push_timing_record(
            records,
            next,
            sample_timing(0xD0 + sweep as u16, (setup / 8) as u16, move || {
                timed_pad_poll(setup, 0)
            }),
        );
        sweep += 1;
    }
}
