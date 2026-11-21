//! The v2.x records (the linear run): field names per record id, the rows that
//! print them and the run's own pass criteria.
//!
//! The guest documents each record in the `/// rec` comment above its id;
//! `tests/guest_tables.rs` keeps this table equal to those comments. Each entry
//! is a label and the names of the (min, median, max) fields.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::report::{Capture, Record};

/// A record's label and the names of its three fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Entry {
    pub label: String,
    pub fields: [String; 3],
}

pub const FIELD_RULE_FIRST: u32 = 0x800;
pub const FIELD_RULE_COUNT: u32 = 83;
pub const FIELD_RULE_TIMED_FROM: u32 = 67;
pub const FIELD_RULE_TIMED_UNTIL: u32 = 75;
pub const UV_WINDOW_FIRST: u32 = 0x8E0;
pub const UV_WINDOW_ROWS: usize = 38;
/// Handoff flags of an area that left the machine clean.
pub const V2_HANDOFF_CLEAN: i64 = 0x3F;
/// Final silence flags of a run that ended silent.
pub const V2_SILENT: i64 = 0x7F;

struct Builder {
    map: BTreeMap<u32, V2Entry>,
}

impl Builder {
    /// Later assignments replace earlier ones, as the dictionary they mirror.
    fn put(&mut self, id: u32, label: impl Into<String>, fields: [&str; 3]) {
        self.map.insert(
            id,
            V2Entry {
                label: label.into(),
                fields: fields.map(str::to_string),
            },
        );
    }

    fn put_owned(&mut self, id: u32, label: impl Into<String>, fields: [String; 3]) {
        self.map.insert(
            id,
            V2Entry {
                label: label.into(),
                fields,
            },
        );
    }
}

const ACK: [&str; 3] = ["ack_rise_cycles", "ack_width_cycles", "byte_done_cycles"];
const ANSWER: [&str; 3] = ["replies_0_and_1", "replies_2_and_3", "ack_mask_and_flags"];
const TICK_CASES: [&str; 4] = ["t2_sysclk", "t0_sysclk", "t2_sysclk_div8", "t0_dotclock"];
const DMA_MATRIX_STATES: [&str; 10] = [
    "cd_1x_2048_lw",
    "cd_1x_2048_sw",
    "cd_1x_2340_lw",
    "cd_1x_2340_sw",
    "cd_2x_2048_lw",
    "cd_2x_2048_sw",
    "cd_2x_2340_lw",
    "cd_2x_2340_sw",
    "mdec_lw",
    "mdec_sw",
];

fn build() -> BTreeMap<u32, V2Entry> {
    let mut b = Builder {
        map: BTreeMap::new(),
    };
    b.put(
        0x400,
        "boot_vector",
        ["bios_stub_standard", "vector_hash_lo", "vector_hash_hi"],
    );
    b.put(
        0x401,
        "boot_reverb_a",
        ["spucnt", "spustat", "reverb_volume_left"],
    );
    b.put(
        0x402,
        "boot_reverb_b",
        ["reverb_volume_right", "reverb_work_base", "eon_low"],
    );
    b.put(
        0x403,
        "boot_reverb_c",
        [
            "config_hash_low",
            "config_hash_high",
            "config_nonzero_words",
        ],
    );
    b.put(
        0x404,
        "post_init_reverb",
        ["volume_left", "volume_right", "work_base"],
    );
    b.put(
        0x405,
        "boot_irq",
        ["i_stat_found", "i_mask_found", "cop0_sr_low16_found"],
    );
    b.put(
        0x406,
        "boot_dma",
        ["dpcr_low16_found", "dpcr_high16_found", "dicr_low16_found"],
    );
    b.put(
        0x407,
        "boot_dma_b",
        [
            "dicr_high16_found",
            "dma_channels_busy_mask",
            "cop0_sr_high16_found",
        ],
    );
    b.put(
        0x408,
        "boot_timers",
        [
            "timer0_mode_found",
            "timer1_mode_found",
            "timer2_mode_found",
        ],
    );
    b.put(
        0x409,
        "boot_spu_volumes",
        [
            "main_volume_lr_found",
            "cd_volume_lr_found",
            "sio0_ctrl_found",
        ],
    );
    b.put(
        0x40A,
        "boot_vector_decode",
        [
            "kernel_stub_target_low16_or_ffff",
            "vector_word0_low16",
            "sio0_stat_low16_found",
        ],
    );
    b.put(
        0x41A,
        "silence_final",
        [
            "flags_0x7f_is_silent",
            "cd_capture_peak",
            "voices_with_envelope",
        ],
    );
    b.put(
        0x41B,
        "run_info",
        ["skipped_risky", "steps", "records_taken"],
    );
    b.put(
        0x41C,
        "handoff_baseline",
        [
            "irq_mask_baseline_bits_0_to_10",
            "dpcr_baseline_low_half",
            "dpcr_baseline_high_half",
        ],
    );
    b.put(
        0x41D,
        "handoff_irq_raw",
        [
            "irq_baseline_raw_low_half",
            "irq_baseline_raw_high_half",
            "irq_last_handoff_raw_high_half",
        ],
    );
    b.put(
        0x41E,
        "record_integrity",
        ["duplicate_ids", "clamped_fields", "dropped_records"],
    );
    b.put(
        0x41F,
        "record_integrity_ids",
        ["first_clamped_id", "last_clamped_id", "first_duplicate_id"],
    );
    b.put(
        0x960,
        "pad_choice",
        ["model_code", "raw_buttons", "answer_frame"],
    );
    b.put(
        0x961,
        "pad_choice_second",
        ["model_code", "raw_buttons", "answer_frame"],
    );
    b.put(
        0x962,
        "pad_passes",
        ["flags", "config_ports", "answer_flags"],
    );
    b.put(
        0x420,
        "mdec_setup",
        ["worked_mask", "driver_notes", "failing_steps"],
    );
    b.put(
        0x421,
        "mdec_probe",
        ["words_of_128", "all_equal", "first_word_low"],
    );
    b.put(
        0x422,
        "mdec_probe_word",
        [
            "first_word_high",
            "decode_to_request_clocks",
            "busy_run_words",
        ],
    );
    b.put(
        0x423,
        "mdec_status",
        ["after_reset_high", "after_tables_high", "after_probe_high"],
    );
    b.put(
        0x424,
        "mdec_trace_idle",
        ["samples", "last_status_high", "last_change_clocks"],
    );
    b.put(
        0x425,
        "mdec_trace_busy",
        ["samples", "last_status_high", "last_change_clocks"],
    );
    b.put(0x426, "mdec_timeout", ["chcr_low", "bcr_high", "madr_low"]);
    b.put(
        0x430,
        "sb1_audit",
        [
            "blocks_or_loop_starts",
            "flags_or_and_last_or_readback_low",
            "first_end_block_or_readback_high",
        ],
    );
    b.put(
        0x431,
        "sb1_upload",
        ["loop_starts", "readback_fnv_low", "readback_fnv_high"],
    );
    for k in 0..5 {
        b.put(
            0x432 + k,
            format!("sb1_stage_{}", k + 1),
            [
                "envelope_at_frame_32",
                "envelope_at_frame_100",
                "endx_bit_per_checkpoint",
            ],
        );
    }
    for k in 0..10 {
        b.put(
            0x410 + k,
            format!("handoff_area_{k}"),
            [
                "clean_flags_0x3f_is_clean",
                "interrupt_mask",
                "voices_active_high_dma_busy_mask_low",
            ],
        );
    }
    for k in 0..22 {
        b.put(
            0x440 + k,
            format!("sb2_tone_{k}"),
            ["late_envelope", "repeat_address", "start_address"],
        );
        b.put(
            0x460 + k,
            format!("sb2_early_{k}"),
            ["early_envelope_or_endx", "late_pitch", "table_word"],
        );
    }
    for k in 0..7 {
        b.put(
            0x480 + k,
            format!("sb2_ram_{k}"),
            ["mismatching_words", "first_bad_index", "word_read_there"],
        );
    }
    for k in 0..5 {
        b.put(
            0x490 + k,
            format!("sb4_hash_{k}"),
            [
                "ring_half_crc_low",
                "ring_half_crc_high",
                "first_nonzero_sample",
            ],
        );
        b.put(
            0x49A + k,
            format!("sb4_state_{k}"),
            ["spustat", "envelope", "raw_sample_16"],
        );
        b.put(
            0x510 + k,
            format!("cd_route_stage_{k}"),
            ["capture_peak_left", "capture_peak_right", "state_word"],
        );
        let detail: [(&str, [&str; 3]); 6] = [
            ("cd_route_regs", ["spucnt", "spustat", "cd_status_and_irq"]),
            (
                "cd_route_volume",
                ["cd_volume_left", "cd_volume_right", "sectors_serviced"],
            ),
            (
                "cd_route_left_hash",
                ["left_hash_low", "left_hash_high", "left_nonzero_samples"],
            ),
            (
                "cd_route_right_hash",
                ["right_hash_low", "right_hash_high", "right_nonzero_samples"],
            ),
            (
                "cd_route_left_energy",
                ["left_energy_low", "left_energy_high", "left_samples"],
            ),
            (
                "cd_route_right_energy",
                ["right_energy_low", "right_energy_high", "right_samples"],
            ),
        ];
        for (d, (name, fields)) in detail.into_iter().enumerate() {
            b.put(0x520 + 8 * k + d as u32, format!("{name}_{k}"), fields);
        }
    }
    for slot in 0..2u32 {
        for k in 0..7 {
            b.put(
                0x4A0 + slot * 8 + k,
                format!("handoff_stage_{}_{k}", ["baseline", "safe2"][slot as usize]),
                [
                    "voices_with_volume_low",
                    "blocking_event_vblanks_or_readback_match",
                    "endx_low_or_readback_hash_low",
                ],
            );
        }
    }
    for k in 0..8 {
        b.put(
            0x500 + k,
            format!("cl2_variant_{k}"),
            [
                "ok_bits_and_data_match",
                "diag_or_fifo_wait_low",
                "drive_state_high",
            ],
        );
    }
    // SIO0 measurements (src/sio_timing.rs) and the Timer 1 rate (src/timer1_rate.rs).
    for port in 0..2u32 {
        let p = port + 1;
        b.put(
            0x600 + port,
            format!("sio_setup_p{p}"),
            [
                "first_ok_delay_cycles",
                "last_failed_delay_cycles",
                "ok_mask_per_delay",
            ],
        );
        for k in 0..9 {
            b.put(0x610 + 16 * port + k, format!("sio_pad_ack_p{p}_b{k}"), ACK);
        }
        for k in 0..4 {
            b.put(0x6C0 + 8 * port + k, format!("sio_card_ack_p{p}_b{k}"), ACK);
        }
        b.put(0x630 + port, format!("sio_pad_answer_p{p}"), ANSWER);
        b.put(
            0x6E0 + port,
            format!("sio_latch_unarmed_p{p}"),
            ["irq_after_byte_mask", "ack_mask", "flags"],
        );
        b.put(
            0x6E2 + port,
            format!("sio_arm_mid_packet_p{p}"),
            [
                "polls_clean_and_tried",
                "first_deviation",
                "irq_after_byte_mask",
            ],
        );
        b.put(0x634 + port, format!("sio_card_answer_p{p}"), ANSWER);
        for load in 0..2u32 {
            let base = 0x640 + 4 * (2 * port + load);
            let tag = format!("p{p}_{}", ["idle", "loaded"][load as usize]);
            b.put(
                base,
                format!("sio_mix_count_{tag}"),
                ["pad_ok_and_tried", "card_ok_and_tried", "error_counts"],
            );
            b.put(
                base + 1,
                format!("sio_mix_card_{tag}"),
                [
                    "card_frame_hblanks_min",
                    "card_frame_hblanks_med",
                    "card_frame_hblanks_max",
                ],
            );
            b.put(
                base + 2,
                format!("sio_mix_pad_{tag}"),
                [
                    "pad_poll_cycles_min",
                    "pad_poll_cycles_med",
                    "pad_poll_cycles_max",
                ],
            );
        }
    }
    for (k, name) in TICK_CASES.iter().enumerate() {
        for (shape, label) in ["6_instr", "14_instr"].iter().enumerate() {
            b.put(
                0x6B0 + 2 * k as u32 + shape as u32,
                format!("tick_loss_{name}_{label}"),
                [
                    "reference_hblanks",
                    "ticks_seen_kilo",
                    "ticks_per_hblank_x16",
                ],
            );
        }
    }
    for (group, tag) in ["spu", "otc", "gpu_block", "gpu_list"].iter().enumerate() {
        let group = group as u32;
        b.put(
            0x780 + 2 * group,
            format!("dma_end_{tag}"),
            ["timeout_flags", "chcr_high_half", "device_status"],
        );
        b.put(
            0x781 + 2 * group,
            format!("dma_end_registers_{tag}"),
            ["madr_low_half", "bcr_high_half", "bcr_low_half"],
        );
    }
    b.put(
        0x788,
        "spu_mode_wait",
        [
            "iterations_first",
            "iterations_longest",
            "mode_never_matched",
        ],
    );
    for (k, state) in DMA_MATRIX_STATES.iter().enumerate() {
        b.put(
            0x790 + k as u32,
            format!("dma_matrix_state_{state}"),
            ["chcr_high_half", "madr_low_half", "device_status"],
        );
    }
    b.put(
        0x650,
        "timer1_free",
        [
            "hblanks_in_window",
            "frames_in_window",
            "spins_without_vblank",
        ],
    );
    b.put(
        0x651,
        "timer1_polled",
        [
            "hblanks_in_window",
            "distinct_values_seen",
            "largest_step_between_reads",
        ],
    );
    b.put(
        0x652,
        "timer1_polled_reads",
        ["reads_low", "reads_high", "steps_larger_than_one"],
    );
    for port in 0..2u32 {
        let p = port + 1;
        let base = 0x760 + 3 * port;
        b.put(
            base,
            format!("rumble_config_p{p}"),
            ["pad_id_plain", "pad_id_in_config_mode", "config_flags"],
        );
        b.put(
            base + 1,
            format!("rumble_mapping_p{p}"),
            [
                "old_mapping_bytes_0_1",
                "old_mapping_bytes_2_3",
                "old_mapping_bytes_4_5",
            ],
        );
        b.put(
            base + 2,
            format!("rumble_after_p{p}"),
            [
                "id_and_5a_after_motor_poll",
                "buttons_in_that_poll",
                "id_after_stop_all",
            ],
        );
        b.put(
            0x767 + port,
            format!("rumble_poll_cost_p{p}"),
            ["poll_cycles_idle", "poll_cycles_motors_on", "polls_each"],
        );
    }
    for port in 0..2u32 {
        let tag = format!("p{}", port + 1);
        b.put(
            0x769 + 2 * port,
            format!("rumble_enter_a_{tag}"),
            ["last_attempt_replies_1_2", "replies_3_4", "replies_5_6"],
        );
        b.put(
            0x76A + 2 * port,
            format!("rumble_enter_b_{tag}"),
            [
                "last_attempt_replies_7_8",
                "attempts_made",
                "first_attempt_replies_1_2",
            ],
        );
        b.put(
            0x76D + port,
            format!("pad_identity_{tag}"),
            ["reply_id_and_5a", "reply_buttons", "reply_sticks_01"],
        );
        b.put(
            0x76F + port,
            format!("pad_model_{tag}"),
            [
                "query_replies_3_4",
                "query_replies_5_6",
                "query_replies_7_8",
            ],
        );
    }
    for (group, tag) in ["p1_idle", "p1_loaded", "p2_idle", "p2_loaded"]
        .iter()
        .enumerate()
    {
        let group = group as u32;
        b.put(
            0x6D0 + 3 * group,
            format!("sio_mix_fault_{tag}"),
            ["round_and_kind", "fault_and_exchange", "exchanges_and_acks"],
        );
        b.put(
            0x6D1 + 3 * group,
            format!("sio_mix_fault_prefix_a_{tag}"),
            [
                "response_bytes_0_1",
                "response_bytes_2_3",
                "response_bytes_4_5",
            ],
        );
        b.put(
            0x6D2 + 3 * group,
            format!("sio_mix_fault_prefix_b_{tag}"),
            [
                "response_bytes_6_7",
                "response_bytes_8_9",
                "failures_of_any_kind",
            ],
        );
    }
    for (case, tag) in ["spins", "open_1_frame", "open_30_frames", "open_0", "open_card"]
        .iter()
        .enumerate()
    {
        b.put(
            0x6F0 + case as u32,
            format!("sio_select_early_{tag}"),
            ["polls", "id_and_magic", "poll_cycles_median"],
        );
    }
    for (case, tag) in ["unmasked", "masked", "first_poll"].iter().enumerate() {
        b.put(
            0x6F5 + case as u32,
            format!("pad_handshake_irq_{tag}"),
            ["rounds", "bytes_seen", "packets"],
        );
    }
    for (case, tag) in [
        "digital_driver_digital_pad",
        "digital_driver_analog_pad",
        "full_driver_digital_pad",
    ]
    .iter()
    .enumerate()
    {
        b.put(
            0x6FC + case as u32,
            format!("pad_first_poll_{tag}"),
            ["first_poll_worst", "id_and_mode", "rounds"],
        );
    }
    b.put(
        0x970,
        "present_path_frames",
        ["frames_flipped", "parts_kicked", "tears_recoveries_bad_markers"],
    );
    b.put(
        0x971,
        "present_path_flip_lines",
        ["earliest_line", "latest_line", "lines_per_frame"],
    );
    b.put(
        0x972,
        "present_path_kicks",
        ["thread_kicks", "handler_kicks", "waiting_edges"],
    );
    b.put(
        0x973,
        "present_path_timing",
        ["late_frames", "worst_submit_to_flip", "draw_done_fallbacks"],
    );
    b.put(
        0x974,
        "present_cd_reads_quiet",
        ["sectors", "errors_and_retries", "unused"],
    );
    b.put(
        0x975,
        "present_cd_reads_shown",
        ["sectors", "errors_and_retries", "tears_recoveries_lost"],
    );
    b.put(
        0x976,
        "present_cache_flush",
        ["frames_flipped", "parts_kicked", "tears_recoveries_faults"],
    );
    // Retired in v2.19 (the step is gone); the names stay so captures of v2.14 to v2.18 decode.
    for (case, tag) in ["mixed_waits", "mixed_early", "single_waits", "single_early"]
        .iter()
        .enumerate()
    {
        b.put(
            0x6F8 + case as u32,
            format!("cd_stop_{tag}"),
            ["sectors", "errors_and_retries", "time_16_hblanks"],
        );
    }
    // v2.20: companions of the records that clamp at 0xFFFF.
    b.put(0x653, "timer1_polled_wide", ["hblanks_low", "hblanks_high", "distinct_low"]);
    b.put(0x654, "timer1_polled_wide_rest", ["distinct_high", "largest_step", "unused"]);
    for k in 0..4 {
        b.put(
            0x874 + 2 * k,
            format!("list_abort_wide_{k}"),
            ["words_at_stop_low", "words_at_stop_high", "words_after_wait_low"],
        );
        b.put(
            0x875 + 2 * k,
            format!("list_abort_wide_rest_{k}"),
            ["words_after_wait_high", "chcr_after_stop_high_half", "unused"],
        );
    }
    // v2.24: SPU bulk upload (engine/examples/hardware-tests/src/spu_bulk.rs).
    b.put(0x9B0, "spu_bulk_rate_512", ["clocks", "polls", "halfwords"]);
    b.put(0x9B1, "spu_bulk_old_path", ["landed_bytes_low", "landed_bytes_high", "flags"]);
    b.put(0x9B2, "spu_bulk_old_path_check", ["hblanks", "matching_kb", "cleared_kb"]);
    b.put(0x9B3, "spu_bulk_new_path", ["landed_bytes_low", "landed_bytes_high", "flags"]);
    b.put(0x9B4, "spu_bulk_new_path_check", ["hblanks", "matching_kb", "cleared_kb"]);
    b.put(0x9B5, "spu_bulk_rate_bank", ["hblanks", "polls_low", "polls_high"]);
    b.put(0x9B6, "spu_bulk_abort", ["read_16b_at_abort", "read_16b_after_wait", "flags"]);
    b.put(0x9B7, "spu_bulk_abort_check", ["landed_16b", "bcr_blocks_at_abort", "bcr_blocks_after"]);
    b.put(0x9B8, "spu_bulk_source", ["changed", "hash_low", "window_fits"]);
    // v2.25: manual-write (PIO) upload, old unpaced against paced (spu_bulk.rs).
    b.put(0x9B9, "spu_bulk_pio_old", ["first_differing_low", "first_differing_high", "hblanks"]);
    b.put(0x9BA, "spu_bulk_pio_old_check", ["differing_bytes_low", "differing_bytes_high", "unused"]);
    b.put(0x9BB, "spu_bulk_pio_new", ["first_differing_low", "first_differing_high", "hblanks"]);
    b.put(0x9BC, "spu_bulk_pio_new_check", ["differing_bytes_low", "differing_bytes_high", "flags"]);
    // v2.25 (audit rows): SPU FIFO overfill (spu_v225.rs).
    for (k, n) in [32u32, 33, 48, 64].into_iter().enumerate() {
        b.put(
            0xA20 + k as u32,
            format!("spu_fifo_push_{n}"),
            ["halfwords_landed", "index_at_slot0_and_last_high", "first_wrong_slot_bit15_timeout"],
        );
    }
    b.put(0xA24, "spu_fifo_two_bursts", ["halfwords_landed", "index_at_slot0_and_last_high", "first_wrong_slot_bit15_timeout"]);
    // v2.25: key-on after key-off, reverb, IRQ latch (spu_v225.rs).
    for (k, clocks) in [0u32, 768, 1_536, 3_072].into_iter().enumerate() {
        b.put(
            0xA28 + k as u32,
            format!("spu_kon_after_koff_{clocks}clk"),
            ["took_lost_neither_nibbles", "level_median", "level_max"],
        );
    }
    b.put(0xA2C, "spu_reverb_start", ["timeout", "hblanks_low", "hblanks_high"]);
    b.put(0xA2D, "spu_reverb_check", ["nonzero_words_in_area", "registers_differing", "reverb_bit_after_stop"]);
    b.put(0xA2E, "spu_irq_latch", ["hblanks_to_latch", "i_stat_bit9", "stat_bit6_after_ack"]);
    b.put(0xA2F, "spu_irq_latch_again", ["hblanks_rearmed", "hblanks_disabled", "unused"]);
    // v2.25: memory card test save and commit timing (card_save.rs), port 1 then port 2.
    for (port, p) in [(0u32, 1u32), (1, 2)] {
        let base = 0xA30 + 8 * port;
        b.put(base, format!("card_save_p{p}"), ["flags_error_high", "free_blocks", "save_block"]);
        b.put(base + 1, format!("card_save_time_p{p}"), ["write_hblanks", "write_vblanks", "delete_hblanks"]);
        b.put(base + 2, format!("card_save_flag_p{p}"), ["flag_before", "flag_after_write", "raw_write_status"]);
        b.put(base + 3, format!("card_commit_p{p}"), ["hblanks_min", "hblanks_median", "hblanks_max"]);
        b.put(base + 4, format!("card_commit_reads_p{p}"), ["reads_first_round", "flag_first_read", "missed_refused"]);
    }
    // v2.25: CD data FIFO and outer-radius chunk reads (cd_v225.rs).
    for (k, place) in ["inner", "middle", "outer"].into_iter().enumerate() {
        b.put(0xA40 + 2 * k as u32, format!("cd_fifo_{place}"), ["div8_clk_min", "div8_clk_median", "div8_clk_max"]);
        b.put(
            0xA41 + 2 * k as u32,
            format!("cd_fifo_{place}_counts"),
            ["sectors", "over15ms_over100ms_high", "int5_timeouts_high"],
        );
    }
    for (k, place) in ["60s", "20s", "3s"].into_iter().enumerate() {
        b.put(0xA46 + 2 * k as u32, format!("cd_outer_chunks_{place}"), ["clean", "after_retry", "never"]);
        b.put(
            0xA47 + 2 * k as u32,
            format!("cd_outer_chunks_{place}_errors"),
            ["wrong_sectors", "int5_high_timeouts_low", "first_error_code"],
        );
    }
    b.put(0xA4C, "cd_outer_retries", ["right_on_second_attempt", "third_or_never", "sweep_once_low_twice_high"]);
    // v2.25 (lane cs-cd): drain cost, drain after idle, implicit seek against seek-first (cd_drain.rs).
    b.put(0xAA0, "cd_drain_cost", ["sectors", "sectors_per_s_x10", "wrong_state_high"]);
    b.put(0xAA1, "cd_drain_cost_cpu", ["clk_lost_per_sector", "longest_gap_clk", "idle_longest_gap_clk"]);
    for (k, s) in [0u32, 5, 15, 30].into_iter().enumerate() {
        b.put(0xAA2 + k as u32, format!("cd_drain_after_idle_{s}s"), ["vblanks_to_first", "vblanks_total", "state"]);
    }
    for (p, place) in ["inner", "middle", "outer"].into_iter().enumerate() {
        for (m, method) in ["implicit", "seek_first"].into_iter().enumerate() {
            let id = 0xAA8 + 2 * (2 * p as u32 + m as u32);
            b.put(id, format!("cd_seek_{place}_{method}"), ["delivered", "wrong", "first_wrong"]);
            b.put(id + 1, format!("cd_seek_{place}_{method}_detail"), ["shift_plus_128", "failures", "hblanks_div16"]);
        }
    }
    // v2.25: Stop then read at once, Pause during SeekL (cd_idle.rs).
    for k in 0..3u32 {
        b.put(0xA50 + k, format!("cd_stop_read_at_once_{k}"), ["hblanks_low", "hblanks_high", "outcome"]);
    }
    b.put(0xA53, "cd_pause_in_seek", ["refused", "accepted", "seek_refused"]);
    // v2.25: heavy textured list walk (gpu_heavy.rs).
    for (k, n) in [256u32, 512, 1_024].into_iter().enumerate() {
        b.put(0xA60 + 2 * k as u32, format!("gpu_heavy_walk_{n}"), ["passes_low", "passes_high", "hblanks"]);
        b.put(
            0xA61 + 2 * k as u32,
            format!("gpu_heavy_walk_{n}_bounds"),
            ["hblanks_to_idle", "past_sdk_500k", "percent_of_8_vblanks"],
        );
    }
    // v2.25: 480i fill after VBlank with E1 bit 10 set (field_rule.rs).
    for k in 0..4u32 {
        b.put(0xA68 + k, format!("field_rule_bit10_after_vblank_{k}"), ["row_mask", "descriptor", "flags"]);
    }
    b.put(0xA6C, "cache_flush_dma_wedged", ["hblanks_low", "hblanks_high", "flags"]);
    // v2.25: pad polls under a forced interrupt (pad_polls.rs).
    for (k, name) in ["shipped", "forced_irq", "forced_irq_masked"].into_iter().enumerate() {
        b.put(0xA80 + k as u32, format!("pad_polls_{name}"), ["answered", "retried", "silent_most_high"]);
    }
    b.put(0xA83, "pad_menu_from_boot", ["menu_frames", "frames_unanswered", "first_answer_mode_high"]);
    // v2.25: the present queue with heavy frames (present_path.rs run_heavy), as 0x970-0x973.
    b.put(0xA90, "present_heavy", ["flipped", "kicks", "health"]);
    b.put(0xA91, "present_heavy_lines", ["first_line", "last_line", "lines_per_frame"]);
    b.put(0xA92, "present_heavy_kicks", ["thread_kicks", "handler_kicks", "skips"]);
    b.put(0xA93, "present_heavy_late", ["late", "worst_vblanks", "fallbacks"]);
    // v2.24: 480i field sequence (engine/examples/hardware-tests/src/field_rule.rs run_sequence).
    for k in 0..8u32 {
        b.put(0x9D0 + k, format!("field_seq_blank_{k}"), ["row_mask", "bit31_at_and_after", "field_number"]);
        b.put(0x9D8 + k, format!("field_seq_active_{k}"), ["row_mask", "bit31_at_and_after", "field_number"]);
    }
    b.put(0x9E0, "field_seq_redraw_hblanks", ["min", "median", "max"]);
    b.put(0x9E1, "field_seq_answers", ["answer_bits", "blank_odd_skipped", "active_odd_skipped"]);
    // v2.24: CD read after idle (engine/examples/hardware-tests/src/cd_idle.rs).
    for (case, tag) in ["pause_5s", "pause_10s", "pause_15s", "pause_30s", "stop_5s"]
        .iter()
        .enumerate()
    {
        b.put(
            0x9C0 + 2 * case as u32,
            format!("cd_idle_{tag}"),
            ["hblanks_low", "hblanks_high", "outcome"],
        );
        b.put(
            0x9C1 + 2 * case as u32,
            format!("cd_idle_{tag}_spins"),
            ["spins_low", "spins_high", "retry_hblanks"],
        );
    }
    b.put(0x9CA, "cd_idle_bound", ["tenth_clocks_per_spin", "data_poll_ms", "flags"]);
    // v2.20: the XA clock case's raw records, fields that cannot clamp.
    for (k, mark) in ["60", "120", "180"].iter().enumerate() {
        b.put(
            0x330 + k as u32,
            format!("xa_clock_raw_head_{mark}s"),
            ["head_ms_low", "head_ms_high", "polls"],
        );
        b.put(
            0x333 + k as u32,
            format!("xa_clock_raw_clock_{mark}s"),
            ["clock_ms_low", "clock_ms_high", "head0_ms"],
        );
    }
    b.put(0x336, "xa_clock_raw_poll_cost_x10_ms", ["min", "median", "max"]);
    b.put(0x337, "xa_clock_raw_poll_counts", ["polls", "over_100ms", "over_1s"]);
    b.put(0x338, "xa_clock_raw_elapsed", ["elapsed_ms_low", "elapsed_ms_high", "passes_4096"]);
    // v2.20: the NCLIP variants, twelve raw MAC0 readings in each of four variants.
    for (variant, name) in ["built", "filled", "extra_nop", "zeroed"].iter().enumerate() {
        for (k, what) in [
            "pos", "neg", "ref1", "probe1", "ref2", "probe2", "ref3", "probe3", "ref4", "probe4", "ref6", "probe6",
        ]
        .iter()
        .enumerate()
        {
            b.put(
                0x980 + 12 * variant as u32 + k as u32,
                format!("gte_nclip_{name}_{what}"),
                ["mac0_low", "mac0_high", "flag_bits"],
            );
        }
    }
    b.put(
        0x7A0,
        "dma_rekick_first",
        [
            "bcr_after_first_high_half",
            "bcr_after_first_low_half",
            "flags",
        ],
    );
    b.put(
        0x7A1,
        "dma_rekick_second",
        [
            "bcr_after_second_high_half",
            "bcr_after_second_low_half",
            "flags",
        ],
    );
    b.put(
        0x7A2,
        "dma_rekick_progress",
        [
            "blocks_past_source",
            "bound_iterations_used",
            "madr_low_half_at_the_bound",
        ],
    );
    b.put(
        0x766,
        "rumble_operator",
        [
            "answers_two_bits_each",
            "stimuli_asked",
            "port_tested_and_api_enabled_in_bit_8",
        ],
    );
    for (k, tag) in ["a", "b", "c"].iter().enumerate() {
        let k = k as u32;
        let third = |what: &str, last: &str| {
            if k < 2 {
                format!("{what}_{}", 3 * k + 2)
            } else {
                last.to_string()
            }
        };
        b.put_owned(
            0x771 + k,
            format!("rumble_answer_buttons_{tag}"),
            [
                format!("raw_buttons_stimulus_{}", 3 * k),
                format!("raw_buttons_stimulus_{}", 3 * k + 1),
                third(
                    "raw_buttons_stimulus",
                    "questions_with_buttons_never_released_mask",
                ),
            ],
        );
        b.put_owned(
            0x774 + k,
            format!("rumble_answer_frames_{tag}"),
            [
                format!("answer_frame_stimulus_{}", 3 * k),
                format!("answer_frame_stimulus_{}", 3 * k + 1),
                third("answer_frame_stimulus", "questions_with_early_button_mask"),
            ],
        );
    }
    b.put(
        0x777,
        "spu_ram_prescrub",
        ["fold_0x1010_4kb", "fold_0x3000_6kb", "fold_0x30000_4kb"],
    );
    b.put(
        0x778,
        "spu_ram_postscrub",
        ["fold_0x1010_4kb", "fold_0x3000_6kb", "fold_0x30000_4kb"],
    );
    // ---- v2.4 -----------------------------------------------------------
    // The 480i draw rule: self-describing records (row mask, descriptor, flags).
    for k in 0..FIELD_RULE_COUNT {
        if !(FIELD_RULE_TIMED_FROM..FIELD_RULE_TIMED_UNTIL).contains(&k) {
            b.put(
                FIELD_RULE_FIRST + k,
                format!("field_rule_{k}"),
                ["row_mask", "descriptor", "flags"],
            );
        } else {
            b.put(
                FIELD_RULE_FIRST + k,
                format!("field_rule_cost_{}", k - FIELD_RULE_TIMED_FROM),
                ["clocks_div8", "descriptor", "flags"],
            );
        }
    }
    for k in 0..4 {
        b.put(
            0x860 + 2 * k,
            format!("list_abort_{k}"),
            [
                "words_of_the_node_landed",
                "words_of_the_next_node_landed",
                "flags",
            ],
        );
        b.put(
            0x861 + 2 * k,
            format!("list_abort_progress_{k}"),
            [
                "madr_words_at_stop",
                "madr_words_after_wait",
                "chcr_after_stop_low_half",
            ],
        );
    }
    for k in 0..6 {
        b.put(
            0x868 + 2 * k,
            format!("block_fifo_{k}"),
            ["words_landed", "words_sent", "flags"],
        );
        b.put(
            0x869 + 2 * k,
            format!("block_fifo_time_{k}"),
            [
                "clocks_low_half",
                "clocks_high_half",
                "gpustat_high_half_after",
            ],
        );
    }
    for port in 0..2u32 {
        let base = 0x880 + 0x20 * port;
        let t = format!("p{}", port + 1);
        for i in 0..10 {
            b.put(base + i, format!("card_proto_head_{t}_b{i}"), ACK);
        }
        let spread = ["min_cycles", "median_cycles", "max_cycles"];
        b.put(base + 0x0A, format!("card_proto_data_rise_{t}"), spread);
        b.put(base + 0x0B, format!("card_proto_data_width_{t}"), spread);
        b.put(base + 0x0C, format!("card_proto_tail_checksum_{t}"), ACK);
        b.put(base + 0x0D, format!("card_proto_tail_terminator_{t}"), ACK);
        b.put(
            base + 0x0E,
            format!("card_proto_total_{t}"),
            ["span_low_half", "span_high_half", "flags"],
        );
        b.put(
            base + 0x0F,
            format!("card_proto_replies_{t}"),
            ["replies_0_and_1", "replies_2_and_3", "replies_4_and_5"],
        );
        b.put(
            base + 0x10,
            format!("card_proto_replies_b_{t}"),
            ["replies_6_and_7", "replies_8_and_9", "replies_138_and_139"],
        );
        for i in 0..6 {
            b.put(
                base + 0x11 + i,
                format!("card_proto_write_head_{t}_b{i}"),
                ACK,
            );
        }
        b.put(
            base + 0x17,
            format!("card_proto_write_data_rise_{t}"),
            spread,
        );
        b.put(
            base + 0x18,
            format!("card_proto_write_data_width_{t}"),
            spread,
        );
        for i in 0..4 {
            b.put(
                base + 0x19 + i,
                format!("card_proto_write_tail_{t}_b{}", 134 + i),
                ACK,
            );
        }
        b.put(
            base + 0x1D,
            format!("card_proto_write_total_{t}"),
            ["span_low_half", "span_high_half", "flags"],
        );
        b.put(
            base + 0x1E,
            format!("card_proto_write_replies_{t}"),
            [
                "replies_134_and_135",
                "replies_136_and_137",
                "status_after_low_half",
            ],
        );
    }
    b.put(
        0x8C0,
        "calibration_gpu",
        ["cycles_low_half", "cycles_high_half", "triangles"],
    );
    b.put(
        0x8C1,
        "calibration_cpu",
        ["cycles_low_half", "cycles_high_half", "iterations_over_16"],
    );
    b.put(
        0x8C2,
        "calibration_both",
        ["cycles_low_half", "cycles_high_half", "flags"],
    );
    b.put(
        0x8C3,
        "calibration_both_split",
        [
            "cpu_done_low_half",
            "cpu_done_high_half",
            "wait_after_cpu_low_half",
        ],
    );
    b.put(
        0x8C4,
        "calibration_frames_one",
        ["frames_taken", "cycles_low_half", "cycles_high_half"],
    );
    b.put(
        0x8C5,
        "calibration_frames_four",
        ["frames_taken", "cycles_low_half", "cycles_high_half"],
    );
    for k in 0..48 {
        b.put(
            UV_WINDOW_FIRST + 2 * k,
            format!("uv_window_{k}"),
            ["clocks_min", "clocks_med", "clocks_max"],
        );
        b.put(
            UV_WINDOW_FIRST + 2 * k + 1,
            format!("uv_window_row_{k}"),
            ["depth_and_window", "uv_span_texels", "origin_u_and_v"],
        );
    }
    // The second pad's pass repeats 0x760-0x776 at +0x1E0.
    let first_pass: Vec<(u32, V2Entry)> = b
        .map
        .range(0x760..=0x776)
        .map(|(id, entry)| (*id, entry.clone()))
        .collect();
    for (id, entry) in first_pass {
        b.map.insert(
            id + 0x1E0,
            V2Entry {
                label: format!("{}_pass2", entry.label),
                fields: entry.fields,
            },
        );
    }
    // Direct CLUT rewrite observations (src/clut_rewrite.rs, 25 records).
    for k in 0..25 {
        b.put(
            0xA00 + k,
            format!("clut_rewrite_{k}"),
            ["first_pixel", "second_pixel", "descriptor_flags"],
        );
    }
    for (speed, speed_tag) in ["1x", "2x"].iter().enumerate() {
        for (size, size_tag) in ["2048", "2340"].iter().enumerate() {
            for (lp, loop_tag) in ["lw", "sw"].iter().enumerate() {
                b.put(
                    0x700 + (speed * 4 + size * 2 + lp) as u32,
                    format!("cd_dma_loop_{speed_tag}_{size_tag}_{loop_tag}"),
                    [
                        "loop_idle_clocks",
                        "loop_during_clocks",
                        "transfer_clocks_div32",
                    ],
                );
            }
        }
    }
    for (lp, loop_tag) in ["lw", "sw"].iter().enumerate() {
        b.put(
            0x708 + lp as u32,
            format!("mdec_dma_loop_{loop_tag}"),
            [
                "loop_idle_clocks",
                "loop_during_clocks",
                "transfer_clocks_div32",
            ],
        );
    }
    b.map
}

fn table() -> &'static BTreeMap<u32, V2Entry> {
    static TABLE: OnceLock<BTreeMap<u32, V2Entry>> = OnceLock::new();
    TABLE.get_or_init(build)
}

/// Every v2.x record this tool knows, by id.
pub fn v2_records() -> &'static BTreeMap<u32, V2Entry> {
    table()
}

pub fn v2_entry(record_id: u32) -> Option<&'static V2Entry> {
    table().get(&record_id)
}

/// Every v2.0 record the capture carries, one row per field.
pub fn v2_rows(capture: &Capture) -> Vec<String> {
    let mut rows = Vec::new();
    for record in &capture.records {
        let Some(entry) = v2_entry(record.record_id) else {
            continue;
        };
        if rows.is_empty() {
            rows.push("v2,record,field,value".to_string());
        }
        for (name, value) in
            entry
                .fields
                .iter()
                .zip([record.minimum, record.median, record.maximum])
        {
            rows.push(format!(
                "v2,{:03X}_{},{name},{value}",
                record.record_id, entry.label
            ));
        }
    }
    rows
}

/// The run's own pass criteria, re-derived from the records: every area
/// handoff clean, no record lost, the final silence check passed. An empty
/// list means all good.
pub fn v2_verdicts(capture: &Capture) -> Vec<String> {
    if capture.suite_major < 2 {
        return Vec::new();
    }
    let by_id: std::collections::HashMap<u32, &Record> = capture
        .records
        .iter()
        .map(|record| (record.record_id, record))
        .collect();
    let mut problems = Vec::new();
    for area in 0..10u32 {
        let Some(record) = by_id.get(&(0x410 + area)) else {
            problems.push(format!("area {area}: no handoff record"));
            continue;
        };
        if record.minimum != V2_HANDOFF_CLEAN || record.maximum != 0 {
            problems.push(format!(
                "area {area}: handoff flags 0x{:02X} (clean is 0x{V2_HANDOFF_CLEAN:02X}), busy/active 0x{:04X}",
                record.minimum, record.maximum
            ));
        }
    }
    if let Some(integrity) = by_id.get(&0x41E) {
        if integrity.minimum != 0 || integrity.median != 0 || integrity.maximum != 0 {
            problems.push(format!(
                "record integrity: {} duplicate ids, {} clamped fields, {} dropped records",
                integrity.minimum, integrity.median, integrity.maximum
            ));
        }
    }
    match by_id.get(&0x41A) {
        None => problems.push("no final silence record".to_string()),
        Some(silence) if silence.minimum != V2_SILENT => problems.push(format!(
            "final silence flags 0x{:02X} (silent is 0x{V2_SILENT:02X}), capture peak {}, voices with envelope {}",
            silence.minimum, silence.median, silence.maximum
        )),
        Some(_) => {}
    }
    problems
}
