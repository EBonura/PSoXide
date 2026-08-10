use super::*;

#[test]
fn read_status_ready_bits_follow_command_pipeline() {
    let mut gpu = Gpu::new();
    let idle = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(idle & 0x1400_0000, 0x1400_0000);

    gpu.charge_busy(10_000);
    let busy = gpu.read32(GP1_ADDR).unwrap();
    // CPU-sourced rendering clears bit 26 (command ready) but leaves
    // bit 28 (DMA block ready) asserted on silicon.
    // Bit 27 (VRAM→CPU ready) is gated on an active transfer;
    // we don't have one here, so it's clear.
    assert_eq!(busy & 0x1400_0000, 0x1000_0000);
    assert_eq!(busy & 0x0800_0000, 0, "VRAM-send ready clear when idle");

    gpu.decay_busy(10_000);
    let settled = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(settled & 0x1400_0000, 0x1400_0000);

    gpu.charge_dma_busy(10_000);
    let dma_busy = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(dma_busy & 0x1400_0000, 0);
}

#[test]
fn dma_request_bit_follows_silicon_direction_rules() {
    let mut gpu = Gpu::new();

    gpu.write32(GP1_ADDR, 0x0400_0001); // FIFO direction
    assert_ne!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);
    gpu.charge_dma_busy(10_000);
    assert_ne!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);

    gpu.write32(GP1_ADDR, 0x0400_0002); // CPU -> GP0 mirrors bit 28
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);
    gpu.decay_busy(10_000);
    assert_ne!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);

    gpu.write32(GP1_ADDR, 0x0400_0003); // GPUREAD -> CPU mirrors bit 27
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);
    gpu.write32(GP0_ADDR, 0xC000_0000);
    gpu.write32(GP0_ADDR, 0);
    gpu.write32(GP0_ADDR, 0x0001_0001);
    assert_ne!(gpu.read32(GP1_ADDR).unwrap() & (1 << 25), 0);
}

#[test]
fn command_timing_geometry_clips_partial_quads_deterministically() {
    let gpu = Gpu::new();
    let full = [(0, 0), (320, 0), (0, 240)];
    let full_other = [(320, 0), (320, 240), (0, 240)];
    assert_eq!(
        gpu.timing_polygon_pixels(&full) + gpu.timing_polygon_pixels(&full_other),
        320 * 240
    );

    let quarter_off = [(-80, 0), (240, 0), (-80, 240)];
    let quarter_other = [(240, 0), (240, 240), (-80, 240)];
    assert_eq!(
        gpu.timing_polygon_pixels(&quarter_off) + gpu.timing_polygon_pixels(&quarter_other),
        240 * 240
    );
}

#[test]
fn full_screen_command_costs_match_silicon_rational_model() {
    assert_eq!(scale_gpu_pixels(320 * 240, 41, 512), 6_150);
    assert_eq!(scale_gpu_pixels(320 * 240, 135, 256), 40_500);
    assert_eq!(scale_gpu_pixels(320 * 240, 179, 64), 214_800);
}

#[test]
fn read_status_sets_vram_send_ready_during_download() {
    let mut gpu = Gpu::new();
    // Start a VRAM→CPU download via GP0 0xC0.
    gpu.write32(GP0_ADDR, 0xC000_0000); // header
    gpu.write32(GP0_ADDR, 0); // xy
    gpu.write32(GP0_ADDR, 0x0001_0001); // 1x1 size
    let stat = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(
        stat & 0x0800_0000,
        0x0800_0000,
        "bit 27 set during transfer"
    );
}

#[test]
fn gp1_reset_clears_dma_direction() {
    let mut gpu = Gpu::new();
    gpu.write32(GP1_ADDR, 0x0400_0002); // set DMA direction to 2
    let stat = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!((stat >> 29) & 3, 2);

    gpu.write32(GP1_ADDR, 0x0000_0000); // reset
    let stat = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!((stat >> 29) & 3, 0);
}

#[test]
fn gp0_irq_request_and_gp1_ack_toggle_gpustat_bit() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0x1F00_0000);
    assert_ne!(gpu.read32(GP1_ADDR).unwrap() & (1 << 24), 0);
    assert!(gpu.take_irq_requested());

    gpu.write32(GP1_ADDR, 0x0200_0000);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & (1 << 24), 0);
    assert!(gpu.take_irq_acknowledged());
}

#[test]
fn gp1_status_changes_land_between_first_and_second_reads() {
    let mut gpu = Gpu::new();
    gpu.write32_at(GP1_ADDR, 0x0400_0001, 100);
    assert_eq!((gpu.read32_at(GP1_ADDR, 106).unwrap() >> 29) & 3, 0);
    assert_eq!((gpu.read32_at(GP1_ADDR, 112).unwrap() >> 29) & 3, 1);

    gpu.write32(GP0_ADDR, 0x1f00_0000);
    gpu.write32_at(GP1_ADDR, 0x0200_0000, 200);
    assert_ne!(gpu.read32_at(GP1_ADDR, 206).unwrap() & (1 << 24), 0);
    assert_eq!(gpu.read32_at(GP1_ADDR, 212).unwrap() & (1 << 24), 0);
    assert!(gpu.take_irq_acknowledged());
    gpu.decay_busy(u64::MAX);

    gpu.write32_at(GP0_ADDR, 0x1f00_0000, 300);
    let first = gpu.read32_at(GP1_ADDR, 306).unwrap();
    assert_eq!(first & (1 << 24), 0);
    assert_eq!(first & ((1 << 26) | (1 << 28)), 0);
    assert_ne!(first & (1 << 25), 0);
    gpu.decay_busy(GP1_STATUS_LATCH_CYCLES);
    let second = gpu.read32_at(GP1_ADDR, 312).unwrap();
    assert_ne!(second & (1 << 24), 0);
    assert_eq!(second & ((1 << 26) | (1 << 28)), (1 << 26) | (1 << 28));
    assert!(gpu.take_irq_requested());
}

#[test]
fn gp1_display_disable_toggles_bit_23() {
    let mut gpu = Gpu::new();
    // Start disabled (reset state has bit 23 set).
    let stat_before = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(stat_before & (1 << 23), 1 << 23);

    // GP1(0x03) with bit 0 = 0: enable display.
    gpu.write32(GP1_ADDR, 0x0300_0000);
    let stat_enabled = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(stat_enabled & (1 << 23), 0);
}

#[test]
fn gp1_query_unknown_subops_preserve_read_latch_like_redux() {
    let mut gpu = Gpu::new();
    gpu.write32(GP1_ADDR, 0x0000_0000); // reset seeds Redux's dataRet latch
    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0x0000_0400);

    gpu.write32(GP1_ADDR, 0x1000_0007); // Redux CtrlQuery::Unknown
    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0x0000_0400);

    gpu.write32(GP0_ADDR, 0xE300_1234);
    gpu.write32(GP1_ADDR, 0x1000_0003);
    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0x0000_1234);

    gpu.write32(GP1_ADDR, 0x1000_0007);
    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0x0000_1234);
}

#[test]
fn gp1_extended_hres_zero_is_368_pixels() {
    let mut gpu = Gpu::new();
    gpu.write32(GP1_ADDR, 0x0704_0010); // standard 240-line v-range 0x10..0x100
    gpu.write32(GP1_ADDR, 0x0800_0060); // hres2=1, hres1=0
    assert_eq!(gpu.display_area().width, 368);

    gpu.write32(GP1_ADDR, 0x0800_0061); // hres2=1, hres1=1
    assert_eq!(gpu.display_area().width, 384);
}

#[test]
fn display_rgba8_pans_by_gp1_06_horizontal_offset() {
    let mut gpu = Gpu::new();
    gpu.write32(GP1_ADDR, 0x0500_0000); // display start (0, 0)
    gpu.write32(GP1_ADDR, 0x0704_0010); // standard 240-line v-range 0x10..0x100
    gpu.write32(GP1_ADDR, 0x0800_0001); // 320-wide, 240 lines
                                        // Mark VRAM column 0 red so we can see where it lands on the output.
    for y in 0..4 {
        gpu.vram.set_pixel(0, y, 0x001F); // 15bpp red
    }
    let px = |buf: &[u8], x: usize| (buf[x * 4], buf[x * 4 + 1], buf[x * 4 + 2]);

    // H-range at the standard centre (0x260): no pan, column 0 is red.
    gpu.write32(GP1_ADDR, 0x0600_0000 | 0x260 | (0xC60 << 12));
    assert_eq!(gpu.horizontal_display_offset_px(), 0);
    let (rgba, w, _h) = gpu.display_rgba8();
    assert_eq!(w, 320);
    assert_eq!(px(&rgba, 0), (255, 0, 0), "centred: column 0 = VRAM x=0");

    // H-range start 0x260 + 32*8 = 0x360 -> +32 px: the picture slides right,
    // so VRAM x=0 lands at output column 32 and columns 0..31 are black-fill.
    gpu.write32(GP1_ADDR, 0x0600_0000 | 0x360 | (0xD60 << 12));
    assert_eq!(gpu.horizontal_display_offset_px(), 32);
    let (rgba, _w, _h) = gpu.display_rgba8();
    assert_eq!(
        px(&rgba, 0),
        (0, 0, 0),
        "offset: exposed left edge is black"
    );
    assert_eq!(
        px(&rgba, 32),
        (255, 0, 0),
        "offset: VRAM x=0 now at column 32"
    );
}

#[test]
fn gp0_writes_are_accepted_without_effect() {
    let mut gpu = Gpu::new();
    let stat_before = gpu.read32(GP1_ADDR).unwrap();
    gpu.write32(GP0_ADDR, 0xE100_0000);
    let stat_after = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(stat_before, stat_after);
}

#[test]
fn rgb24_to_bgr15_white_reaches_0x7fff() {
    assert_eq!(rgb24_to_bgr15(0x00FFFFFF), 0x7FFF);
}

#[test]
fn rgb24_to_bgr15_primary_channels() {
    // Pure red (FF in low byte): bottom 5 bits set.
    assert_eq!(rgb24_to_bgr15(0x000000FF), 0x001F);
    // Pure green: middle 5.
    assert_eq!(rgb24_to_bgr15(0x0000FF00), 0x03E0);
    // Pure blue: top 5.
    assert_eq!(rgb24_to_bgr15(0x00FF0000), 0x7C00);
}

#[test]
fn gp0_fill_rect_writes_vram() {
    let mut gpu = Gpu::new();
    // Fill a 16×16 red rect at (0, 0).
    gpu.write32(GP0_ADDR, 0x0200_00FF); // 0x02 + red
    gpu.write32(GP0_ADDR, 0x0000_0000); // y=0, x=0
    gpu.write32(GP0_ADDR, 0x0010_0010); // h=16, w=16

    assert_eq!(gpu.vram.get_pixel(0, 0), 0x001F);
    assert_eq!(gpu.vram.get_pixel(15, 15), 0x001F);
    // Just outside stays zero.
    assert_eq!(gpu.vram.get_pixel(16, 0), 0);
    assert_eq!(gpu.vram.get_pixel(0, 16), 0);
}

#[test]
fn gp0_draw_mode_commands_are_accepted() {
    let mut gpu = Gpu::new();
    // Four draw-mode setters back-to-back, each 1 word.
    gpu.write32(GP0_ADDR, 0xE100_0000); // draw mode
    gpu.write32(GP0_ADDR, 0xE200_0000); // texture window
    gpu.write32(GP0_ADDR, 0xE300_0000); // drawing area TL
    gpu.write32(GP0_ADDR, 0xE400_0000); // drawing area BR
                                        // None of these should have stuck a packet in the FIFO.
                                        // (Implementation detail, but worth guarding against.)
}

#[test]
fn gpu_address_match_returns_none_off_port() {
    let mut gpu = Gpu::new();
    assert!(gpu.read32(0x1F80_1800).is_none());
    assert!(gpu.read32(0x1F80_1818).is_none());
}

#[test]
fn blend_mode_decodes_tpage_bits() {
    assert_eq!(BlendMode::from_tpage_bits(0), BlendMode::Average);
    assert_eq!(BlendMode::from_tpage_bits(1), BlendMode::Add);
    assert_eq!(BlendMode::from_tpage_bits(2), BlendMode::Sub);
    assert_eq!(BlendMode::from_tpage_bits(3), BlendMode::AddQuarter);
    // Higher bits are masked off.
    assert_eq!(BlendMode::from_tpage_bits(0b100), BlendMode::Average);
}

#[test]
fn blend_opaque_returns_foreground_unchanged() {
    assert_eq!(blend_pixel(0x1234, 0x5678, BlendMode::Opaque), 0x5678);
}

#[test]
fn blend_average_sums_then_halves() {
    // Mode 0 = `(B + F) >> 1` (PSX-SPX). Even+even is unaffected by the
    // sum-vs-per-operand distinction: (10 + 20) >> 1 = 15 per channel.
    let bg = 10 | (10 << 5) | (10 << 10);
    let fg = 20 | (20 << 5) | (20 << 10);
    let out = blend_pixel(bg, fg, BlendMode::Average);
    assert_eq!(out & 0x1F, 15);
    assert_eq!((out >> 5) & 0x1F, 15);
    assert_eq!((out >> 10) & 0x1F, 15);
}

#[test]
fn blend_average_odd_plus_odd_sums_then_halves() {
    // PS1 hardware halves the SUM: `(3 + 3) >> 1 = 3`, keeping the LSB
    // (a per-operand `(3>>1)+(3>>1) = 2` would be one step too dark).
    let bg = 3 | (3 << 5) | (3 << 10);
    let fg = 3 | (3 << 5) | (3 << 10);
    let out = blend_pixel(bg, fg, BlendMode::Average);
    assert_eq!(out & 0x1F, 3, "R: (3+3)>>1 = 3");
    assert_eq!((out >> 5) & 0x1F, 3, "G: same");
    assert_eq!((out >> 10) & 0x1F, 3, "B: same");

    // Asymmetric odd pair: BG=5, FG=3 → (5 + 3) >> 1 = 4
    // (the per-operand approximation gives 2 + 1 = 3).
    let bg = 5u16;
    let fg = 3u16;
    let out = blend_pixel(bg, fg, BlendMode::Average);
    assert_eq!(out & 0x1F, 4);
}

#[test]
fn blend_add_saturates_at_31() {
    let bg = 20 | (20 << 5) | (20 << 10);
    let fg = 20 | (20 << 5) | (20 << 10);
    let out = blend_pixel(bg, fg, BlendMode::Add);
    // 20+20 = 40 → clamps to 31 per channel.
    assert_eq!(out & 0x1F, 31);
    assert_eq!((out >> 5) & 0x1F, 31);
    assert_eq!((out >> 10) & 0x1F, 31);
}

#[test]
fn blend_sub_saturates_at_zero() {
    let bg = 5 | (10 << 5) | (15 << 10);
    let fg = 10 | (10 << 5) | (10 << 10);
    let out = blend_pixel(bg, fg, BlendMode::Sub);
    // R: 5-10 = -5 → 0. G: 10-10 = 0. B: 15-10 = 5.
    assert_eq!(out & 0x1F, 0);
    assert_eq!((out >> 5) & 0x1F, 0);
    assert_eq!((out >> 10) & 0x1F, 5);
}

#[test]
fn blend_add_quarter_adds_fractional_foreground() {
    let bg = 10 | (10 << 5) | (10 << 10);
    let fg = 20 | (20 << 5) | (20 << 10);
    let out = blend_pixel(bg, fg, BlendMode::AddQuarter);
    // BG + FG/4 → 10 + 5 = 15 per channel.
    assert_eq!(out & 0x1F, 15);
    assert_eq!((out >> 5) & 0x1F, 15);
    assert_eq!((out >> 10) & 0x1F, 15);
}

#[test]
fn blend_preserves_foreground_mask_bit() {
    // Mask bit (bit 15) must come from the foreground so semi-
    // transparent texels keep marking themselves.
    let bg = 0x0000;
    let fg = 0x8000 | 10;
    let out = blend_pixel(bg, fg, BlendMode::Average);
    assert_eq!(out & 0x8000, 0x8000);
}

#[test]
fn prim_helpers_decode_semi_trans_bit() {
    // 0x20 = opaque monochrome tri. 0x22 = semi-trans monochrome tri.
    // The opcode is in bits 24..=31; bit 25 of the word = bit 1 of op.
    assert!(!prim_is_semi_trans(0x2000_0000));
    assert!(prim_is_semi_trans(0x2200_0000));
    assert_eq!(
        prim_blend_mode(0x2000_0000, BlendMode::Add),
        BlendMode::Opaque
    );
    assert_eq!(prim_blend_mode(0x2200_0000, BlendMode::Add), BlendMode::Add);
}

#[test]
fn draw_mode_e1_extracts_blend_mode() {
    // GP0 0xE1: bits 5-6 select semi-transparency mode.
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE100_0020); // bits 5-6 = 01 → Add
    assert_eq!(gpu.tex_blend_mode, BlendMode::Add);
    gpu.write32(GP0_ADDR, 0xE100_0060); // bits 5-6 = 11 → AddQuarter
    assert_eq!(gpu.tex_blend_mode, BlendMode::AddQuarter);
}

#[test]
fn gp1_09_gates_e1_upper_texture_page_status_bit() {
    let mut gpu = Gpu::new();

    // With GP1(09h) disabled, E1 bit 11 is ignored and GPUSTAT.15 clears.
    gpu.write32(GP0_ADDR, 0xE100_0FFF);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x87FF, 0x07FF);

    // Enabling the upper address bit makes E1.11 visible as GPUSTAT.15.
    gpu.write32(GP1_ADDR, 0x0900_0001);
    gpu.write32(GP0_ADDR, 0xE100_0FFF);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x87FF, 0x87FF);

    // Disabling GP1(09h) alone preserves the already-latched draw mode;
    // the next E1 write clears bit 15 because bit 11 is no longer accepted.
    gpu.write32(GP1_ADDR, 0x0900_0000);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x8000, 0x8000);
    gpu.write32(GP0_ADDR, 0xE100_0000);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x8000, 0);
}

#[test]
fn polygon_tpage_updates_bits_0_to_8_and_11_only() {
    let mut gpu = Gpu::new();
    gpu.write32(GP1_ADDR, 0x0900_0001);

    // Seed every E1-visible bit, then a zero tpage must preserve only E1's
    // dither/draw-to-display bits 9 and 10.
    gpu.write32(GP0_ADDR, 0xE100_0FFF);
    gpu.apply_primitive_tpage(0x0000_0000);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x87FF, 0x0600);

    // Conversely, an all-ones tpage may set bits 0..8 and GPUSTAT.15 but
    // cannot turn on bits 9..10.
    gpu.write32(GP0_ADDR, 0xE100_0000);
    gpu.apply_primitive_tpage(0xFFFF_0000);
    assert_eq!(gpu.read32(GP1_ADDR).unwrap() & 0x87FF, 0x81FF);
}

#[test]
fn upper_texture_page_reads_absent_retail_vram_bank() {
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(0, 0, 0x1234);
    gpu.write32(GP1_ADDR, 0x0900_0001);
    gpu.write32(GP0_ADDR, 0xE100_0900); // 15bpp plus upper Y-address bit
    assert_eq!(gpu.tex_page_y, 512);
    assert_eq!(gpu.sample_texture(0, 0), None);

    // With GP1(09h) disabled the same E1 value addresses the lower bank.
    gpu.write32(GP1_ADDR, 0x0900_0000);
    gpu.write32(GP0_ADDR, 0xE100_0900);
    assert_eq!(gpu.tex_page_y, 0);
    assert_eq!(gpu.sample_texture(0, 0), Some(0x1234));
}

#[test]
fn modulate_tint_identity_at_0x80() {
    // tint 0x80 per channel = identity. Any texel passes unchanged.
    let texel = 0x1234; // arbitrary 15bpp
    let out = modulate_tint(texel, 0x80, 0x80, 0x80);
    assert_eq!(out, texel);
}

#[test]
fn modulate_tint_scales_each_channel() {
    // texel = (R=16, G=10, B=5) at bits (0..5), (5..10), (10..15).
    let texel: u16 = 16 | (10 << 5) | (5 << 10);
    // tint R=0xC0 (1.5×), G=0x40 (0.5×), B=0x80 (1.0×).
    let out = modulate_tint(texel, 0xC0, 0x40, 0x80);
    // Expected:
    //   R = 0xC0 * 16 / 0x80 = 192 * 16 / 128 = 24 → clamp 31 → 24
    //   G = 0x40 * 10 / 0x80 = 64 * 10 / 128 = 5
    //   B = 0x80 * 5 / 0x80 = 5
    assert_eq!(out & 0x1F, 24);
    assert_eq!((out >> 5) & 0x1F, 5);
    assert_eq!((out >> 10) & 0x1F, 5);
}

#[test]
fn modulate_tint_clamps_to_31_on_overbright() {
    // texel at max (31 each), tint at max (0xFF each) → should
    // clamp to 31 per channel.
    let texel: u16 = 31 | (31 << 5) | (31 << 10);
    let out = modulate_tint(texel, 0xFF, 0xFF, 0xFF);
    assert_eq!(out & 0x1F, 31);
    assert_eq!((out >> 5) & 0x1F, 31);
    assert_eq!((out >> 10) & 0x1F, 31);
}

#[test]
fn modulate_tint_preserves_mask_bit() {
    // Semi-transparent texel (bit 15 set) must keep bit 15 after
    // modulation so downstream blend logic still fires.
    let texel: u16 = 0x8000 | 10;
    let out = modulate_tint(texel, 0x80, 0x80, 0x80);
    assert_eq!(out & 0x8000, 0x8000);
}

#[test]
fn split_tint_extracts_rgb_channels() {
    // PSX tint word = 0xBBGGRR (the low 24 bits of a
    // textured-primitive cmd).
    assert_eq!(split_tint(0x00123456), (0x56, 0x34, 0x12));
    assert_eq!(split_tint(0x00FFFFFF), (0xFF, 0xFF, 0xFF));
    assert_eq!(split_tint(0x0080_8080), RAW_TEXTURE_TINT);
}

#[test]
fn gp0_e6_sets_mask_flags() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE600_0000); // both clear
    assert!(!gpu.mask_set_on_draw);
    assert!(!gpu.mask_check_before_draw);
    gpu.write32(GP0_ADDR, 0xE600_0001); // set-on-draw only
    assert!(gpu.mask_set_on_draw);
    assert!(!gpu.mask_check_before_draw);
    gpu.write32(GP0_ADDR, 0xE600_0003); // both on
    assert!(gpu.mask_set_on_draw);
    assert!(gpu.mask_check_before_draw);
    // GPUSTAT bits 11 (set-on-draw) + 12 (check-before-draw)
    // mirror the flag state.
    let stat = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(stat & 0x1800, 0x1800);
}

#[test]
fn plot_pixel_respects_set_mask_on_draw() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE600_0001); // set-on-draw
    gpu.plot_pixel(10, 10, 0x1234, BlendMode::Opaque);
    // The drawn pixel should have bit 15 forced to 1.
    assert_eq!(gpu.vram.get_pixel(10, 10) & 0x8000, 0x8000);
}

#[test]
fn plot_pixel_skips_when_mask_check_sees_masked_pixel() {
    let mut gpu = Gpu::new();
    // Pre-mark (20, 20) as masked.
    gpu.vram.set_pixel(20, 20, 0x8000 | 0x1234);
    gpu.write32(GP0_ADDR, 0xE600_0002); // check-before-draw
    gpu.plot_pixel(20, 20, 0x5678, BlendMode::Opaque);
    // Drop: original pixel survives.
    assert_eq!(gpu.vram.get_pixel(20, 20), 0x8000 | 0x1234);
}

#[test]
fn plot_pixel_draws_when_mask_check_sees_unmasked_pixel() {
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(30, 30, 0x1234); // mask bit clear
    gpu.write32(GP0_ADDR, 0xE600_0002); // check-before-draw
    gpu.plot_pixel(30, 30, 0x5678, BlendMode::Opaque);
    assert_eq!(gpu.vram.get_pixel(30, 30), 0x5678);
}

#[test]
fn paint_rect_span_path_preserves_blend_and_mask_rules() {
    let mut gpu = Gpu::new();
    let bg = 10 | (10 << 5) | (10 << 10);
    let fg = 20 | (20 << 5) | (20 << 10);
    gpu.vram.set_pixel(10, 10, bg);
    gpu.vram.set_pixel(11, 10, 0x8000 | 0x1234);
    gpu.write32(GP0_ADDR, 0xE600_0003); // set-on-draw + check-before-draw

    gpu.paint_rect(10, 10, 2, 1, fg, BlendMode::AddQuarter);

    assert_eq!(
        gpu.vram.get_pixel(10, 10),
        0x8000 | blend_pixel(bg, fg, BlendMode::AddQuarter)
    );
    assert_eq!(gpu.vram.get_pixel(11, 10), 0x8000 | 0x1234);
}

#[test]
fn gp1_reset_clears_mask_flags() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE600_0003); // both on
    gpu.write32(GP1_ADDR, 0x0000_0000); // GP1 reset
    assert!(!gpu.mask_set_on_draw);
    assert!(!gpu.mask_check_before_draw);
    let stat = gpu.read32(GP1_ADDR).unwrap();
    assert_eq!(stat & 0x1800, 0);
}

#[test]
fn gp0_e2_parses_texture_window_fields() {
    let mut gpu = Gpu::new();
    // mask_x = 3 (24 px), mask_y = 5 (40 px), off_x = 1 (8 px),
    // off_y = 2 (16 px).
    //
    //   bits 0..=4  : mask_x  = 3
    //   bits 5..=9  : mask_y  = 5
    //   bits 10..=14: off_x   = 1
    //   bits 15..=19: off_y   = 2
    let word = 0xE200_0000u32 | 3 | (5 << 5) | (1 << 10) | (2 << 15);
    gpu.write32(GP0_ADDR, word);
    assert_eq!(gpu.tex_window_mask_x, 24);
    assert_eq!(gpu.tex_window_mask_y, 40);
    assert_eq!(gpu.tex_window_offset_x, 8);
    assert_eq!(gpu.tex_window_offset_y, 16);
}

#[test]
fn texture_window_default_is_passthrough() {
    // Default window (all zeroes) must leave UV unchanged when
    // sampled; otherwise we'd break every game that doesn't
    // touch GP0 0xE2.
    let gpu = Gpu::new();
    assert_eq!(gpu.tex_window_mask_x, 0);
    assert_eq!(gpu.tex_window_mask_y, 0);
    assert_eq!(gpu.tex_window_offset_x, 0);
    assert_eq!(gpu.tex_window_offset_y, 0);
    // The sample-time formula is `u & !mask | offset & mask`;
    // with mask=0 every u passes through.
    let u: u16 = 0x5A;
    let mask: u16 = 0;
    let off: u16 = 0;
    assert_eq!((u & !mask) | (off & mask), u);
}

#[test]
fn textured_shaded_tri_packet_size_is_nine() {
    // 0x34..=0x37 is "textured + Gouraud-shaded triangle" --
    // 3 vertices × (colour+vertex+uv) = 9 words total.
    assert_eq!(gp0_packet_size(0x34), 9);
    assert_eq!(gp0_packet_size(0x37), 9);
}

#[test]
fn textured_shaded_quad_packet_size_is_twelve() {
    // 0x3C..=0x3F is "textured + Gouraud-shaded quad" --
    // 4 vertices × (colour+vertex+uv) = 12 words.
    assert_eq!(gp0_packet_size(0x3C), 12);
    assert_eq!(gp0_packet_size(0x3F), 12);
}

#[test]
fn gp0_e1_bit_9_toggles_dither_flag() {
    let mut gpu = Gpu::new();
    assert!(!gpu.dither_enabled);
    // E1h with bit 9 set → dither on.
    gpu.write32(GP0_ADDR, 0xE100_0000 | (1 << 9));
    assert!(gpu.dither_enabled);
    // E1h with bit 9 clear → dither off.
    gpu.write32(GP0_ADDR, 0xE100_0000);
    assert!(!gpu.dither_enabled);
}

#[test]
fn dither_rgb_matches_signed_matrix_truth_table() {
    // Hand-computed expected outputs for the PSX-SPX signed additive
    // 4×4 dither matrix. Per channel: clamp(value + offset, 0..=255)
    // then `>> 3`. Offset is indexed by `(y & 3) * 4 + (x & 3)`:
    //   row 0: -4  0 -3  1
    //   row 1:  2 -2  3 -1
    //   row 2: -3  1 -4  0
    //   row 3:  3 -1  2 -2
    //
    //   input r,g,b | (x,y) | offset | expected_r expected_g expected_b
    //   128,128,128 | (0,0) |  -4    |    15         15         15
    //   128,128,128 | (1,0) |   0    |    16         16         16
    //   120,120,120 | (0,0) |  -4    |    14         14         14  (rounds DOWN)
    //   126,126,126 | (2,1) |  +3    |    16         16         16  (rounds UP)
    //   255,255,255 | (3,3) |  -2    |    31         31         31  (clamp hi)
    //   0,0,0       | (0,0) |  -4    |     0          0          0  (clamp lo)
    //   7,7,7       | (1,0) |   0    |     0          0          0  (was 1 under old model)
    //   3,3,3       | (2,0) |  -3    |     0          0          0

    let check = |r: i32, g: i32, b: i32, x: i32, y: i32, er: u16, eg: u16, eb: u16| {
        let v = dither_rgb(r, g, b, x, y);
        assert_eq!(v & 0x1F, er, "R mismatch for ({r},{g},{b})@({x},{y})");
        assert_eq!((v >> 5) & 0x1F, eg, "G mismatch");
        assert_eq!((v >> 10) & 0x1F, eb, "B mismatch");
    };
    // offset -4 at (0,0): (128-4)>>3 = 15.
    check(128, 128, 128, 0, 0, 15, 15, 15);
    // offset 0 at (1,0): 128>>3 = 16 -> 15/16 checkerboard exists.
    check(128, 128, 128, 1, 0, 16, 16, 16);
    // offset -4: (120-4)>>3 = 14, below plain 120>>3 = 15.
    check(120, 120, 120, 0, 0, 14, 14, 14);
    // offset +3 at (2,1): (126+3)>>3 = 16, above plain 126>>3 = 15.
    check(126, 126, 126, 2, 1, 16, 16, 16);
    // Clamp guard: 255 + offset clamps to 255, still 31.
    check(255, 255, 255, 3, 3, 31, 31, 31);
    // Clamp guard: 0 + (-4) clamps to 0.
    check(0, 0, 0, 0, 0, 0, 0, 0);
    // offset 0: 7>>3 = 0 (the old round-up model returned 1 here).
    check(7, 7, 7, 1, 0, 0, 0, 0);
    // offset -3: (3-3)>>3 = 0.
    check(3, 3, 3, 2, 0, 0, 0, 0);
}

#[test]
fn dither_rgb_saturates_at_255() {
    // Pure 255 must never wrap -- tests the `rc < 0x1F` guard
    // across every coefficient position.
    for x in 0..4 {
        for y in 0..4 {
            let v = dither_rgb(255, 255, 255, x, y);
            assert_eq!(v & 0x1F, 31);
            assert_eq!((v >> 5) & 0x1F, 31);
            assert_eq!((v >> 10) & 0x1F, 31);
        }
    }
}

#[test]
fn mono_line_packet_size_is_three() {
    for op in 0x40..=0x47 {
        assert_eq!(gp0_packet_size(op), 3, "opcode 0x{op:02X}");
    }
}

#[test]
fn dither_stable_point_line_matches_one_pixel_rect() {
    let mut gpu = Gpu::new();
    gpu.gp0_push(0xE300_0000); // draw area top-left (0, 0)
    gpu.gp0_push(0xE400_0000 | 1023 | (511 << 10));
    gpu.gp0_push(0xE500_0000); // draw offset (0, 0)
    gpu.gp0_push(0xE100_0200); // dither enabled

    for channel in 0..32u32 {
        let tile_rgb = (channel << 3) * 0x0001_0101;
        let line_rgb = tile_rgb | 0x0004_0404;
        for y in 0..4u32 {
            for x in 0..4u32 {
                let tile_xy = ((y + 16) << 16) | (x + 16);
                let line_xy = ((y + 16) << 16) | (x + 24);

                gpu.gp0_push(0x6000_0000 | tile_rgb);
                gpu.gp0_push(tile_xy);
                gpu.gp0_push(0x0001_0001);

                gpu.gp0_push(0x4000_0000 | line_rgb);
                gpu.gp0_push(line_xy);
                gpu.gp0_push(line_xy);

                assert_eq!(
                    gpu.vram.get_pixel((x + 16) as u16, (y + 16) as u16),
                    gpu.vram.get_pixel((x + 24) as u16, (y + 16) as u16),
                    "5-bit channel {channel} at dither cell ({x}, {y})",
                );
            }
        }
    }
}

#[test]
fn shaded_line_packet_size_is_four() {
    for op in 0x50..=0x57 {
        assert_eq!(gp0_packet_size(op), 4, "opcode 0x{op:02X}");
    }
}

#[test]
fn polyline_start_packet_sizes_match_single() {
    for op in 0x48..=0x4F {
        assert_eq!(gp0_packet_size(op), 3);
    }
    for op in 0x58..=0x5F {
        assert_eq!(gp0_packet_size(op), 4);
    }
}

#[test]
fn cmd_log_captures_cpu_vram_upload_payload() {
    let mut gpu = Gpu::new();
    gpu.enable_cmd_log();

    gpu.gp0_push(0xA0_00_00_00);
    gpu.gp0_push(0x0000_0000); // x=0, y=0
    gpu.gp0_push(0x0001_0003); // w=3, h=1 -> 2 payload words
    gpu.gp0_push(0x2222_1111);
    gpu.gp0_push(0x4444_3333);

    assert_eq!(gpu.cmd_log.len(), 1);
    assert_eq!(gpu.cmd_log[0].opcode, 0xA0);
    assert_eq!(
        gpu.cmd_log[0].fifo,
        vec![
            0xA0_00_00_00,
            0x0000_0000,
            0x0001_0003,
            0x2222_1111,
            0x4444_3333
        ]
    );
    assert_eq!(gpu.vram.get_pixel(0, 0), 0x1111);
    assert_eq!(gpu.vram.get_pixel(1, 0), 0x2222);
    assert_eq!(gpu.vram.get_pixel(2, 0), 0x3333);
}

#[test]
fn gp1_command_buffer_reset_aborts_partial_vram_upload() {
    let mut gpu = Gpu::new();
    gpu.gp0_push(0xA0_00_00_00);
    gpu.gp0_push(0x0000_0000);
    gpu.gp0_push(0x0001_0004); // two payload words
    gpu.gp0_push(0x2222_1111); // leave one pending
    assert!(gpu.vram_upload_active());

    gpu.write32(GP1_ADDR, 0x0100_0000);
    assert!(!gpu.vram_upload_active());

    // The next word is a fresh GP0 NOP, not stale upload data.
    gpu.gp0_push(0);
    assert_eq!(gpu.vram.get_pixel(2, 0), 0);
    assert_eq!(gpu.vram.get_pixel(3, 0), 0);
}

#[test]
fn cpu_vram_upload_ignores_opcode_low_bits() {
    let mut gpu = Gpu::new();

    gpu.gp0_push(0xAA_12_34_56);
    gpu.gp0_push(0x0000_0000); // x=0, y=0
    gpu.gp0_push(0x0001_0002); // w=2, h=1 -> 1 payload word
    gpu.gp0_push(0x2222_1111);

    assert_eq!(gpu.vram.get_pixel(0, 0), 0x1111);
    assert_eq!(gpu.vram.get_pixel(1, 0), 0x2222);
}

#[test]
fn vram_cpu_download_ignores_opcode_low_bits() {
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(4, 5, 0xCAFE);

    gpu.gp0_push(0xD7_12_34_56);
    gpu.gp0_push(0x0005_0004); // x=4, y=5
    gpu.gp0_push(0x0001_0001); // w=1, h=1

    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0x0000_CAFE);
}

#[test]
fn cmd_log_drain_keeps_in_flight_upload_payload_attached() {
    let mut gpu = Gpu::new();
    gpu.enable_cmd_log();

    gpu.gp0_push(0xE1_00_00_00);
    gpu.gp0_push(0xA0_00_00_00);
    gpu.gp0_push(0x0000_0000); // x=0, y=0
    gpu.gp0_push(0x0001_0003); // w=3, h=1 -> 2 payload words
    gpu.gp0_push(0x2222_1111);

    let drained = gpu.drain_completed_cmd_log();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].opcode, 0xE1);
    assert_eq!(gpu.cmd_log.len(), 1);
    assert_eq!(gpu.cmd_log[0].opcode, 0xA0);
    assert_eq!(
        gpu.cmd_log[0].fifo,
        vec![0xA0_00_00_00, 0x0000_0000, 0x0001_0003, 0x2222_1111]
    );

    gpu.gp0_push(0x4444_3333);
    let drained = gpu.drain_completed_cmd_log();
    assert!(gpu.cmd_log.is_empty());
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].fifo,
        vec![
            0xA0_00_00_00,
            0x0000_0000,
            0x0001_0003,
            0x2222_1111,
            0x4444_3333
        ]
    );
}

#[test]
fn cmd_log_captures_mono_polyline_continuations() {
    let mut gpu = Gpu::new();
    gpu.enable_cmd_log();

    gpu.gp0_push(0x48_FF_FF_FF); // mono polyline start
    gpu.gp0_push(0x0000_0000); // v0
    gpu.gp0_push(0x0000_0005); // v1
    gpu.gp0_push(0x0000_000A); // continuation vertex
    gpu.gp0_push(0x0005_000A); // continuation vertex
    gpu.gp0_push(0x5555_5555); // terminator (not logged)

    assert!(gpu.polyline.is_none());
    assert_eq!(gpu.cmd_log.len(), 1);
    assert_eq!(gpu.cmd_log[0].opcode, 0x48);
    assert_eq!(
        gpu.cmd_log[0].fifo,
        vec![
            0x48_FF_FF_FF,
            0x0000_0000,
            0x0000_0005,
            0x0000_000A,
            0x0005_000A
        ]
    );
    // Terminated: the entry drains as a completed command.
    let drained = gpu.drain_completed_cmd_log();
    assert_eq!(drained.len(), 1);
    assert!(gpu.cmd_log.is_empty());
}

#[test]
fn cmd_log_captures_shaded_polyline_continuations() {
    let mut gpu = Gpu::new();
    gpu.enable_cmd_log();

    gpu.gp0_push(0x58_FF_00_00); // shaded polyline start, c0
    gpu.gp0_push(0x0000_0000); // v0
    gpu.gp0_push(0x0000_FF00); // c1
    gpu.gp0_push(0x0000_0008); // v1
    gpu.gp0_push(0x00FF_0000); // continuation colour
    gpu.gp0_push(0x0008_0008); // continuation vertex
    gpu.gp0_push(0x5000_5000); // terminator (not logged)

    assert!(gpu.polyline.is_none());
    assert_eq!(gpu.cmd_log.len(), 1);
    assert_eq!(gpu.cmd_log[0].opcode, 0x58);
    assert_eq!(
        gpu.cmd_log[0].fifo,
        vec![
            0x58_FF_00_00,
            0x0000_0000,
            0x0000_FF00,
            0x0000_0008,
            0x00FF_0000,
            0x0008_0008
        ]
    );
}

#[test]
fn cmd_log_drain_keeps_in_flight_polyline_attached() {
    let mut gpu = Gpu::new();
    gpu.enable_cmd_log();

    gpu.gp0_push(0xE1_00_00_00);
    gpu.gp0_push(0x48_FF_FF_FF); // mono polyline start
    gpu.gp0_push(0x0000_0000); // v0
    gpu.gp0_push(0x0000_0005); // v1
    gpu.gp0_push(0x0000_000A); // continuation vertex, still in flight

    let drained = gpu.drain_completed_cmd_log();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].opcode, 0xE1);
    assert_eq!(gpu.cmd_log.len(), 1);
    assert_eq!(gpu.cmd_log[0].opcode, 0x48);

    // Continuations after the drain keep appending to the retained
    // entry; the terminator releases it for the next drain.
    gpu.gp0_push(0x0005_000A);
    gpu.gp0_push(0x5555_5555);
    let drained = gpu.drain_completed_cmd_log();
    assert!(gpu.cmd_log.is_empty());
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].fifo,
        vec![
            0x48_FF_FF_FF,
            0x0000_0000,
            0x0000_0005,
            0x0000_000A,
            0x0005_000A
        ]
    );
}

#[test]
fn vram_copy_masks_coordinates_and_size_fields() {
    let mut gpu = Gpu::new();

    gpu.vram.set_pixel(2, 3, 0x1111);
    gpu.vram.set_pixel(3, 3, 0x2222);
    gpu.vram.set_pixel(4, 3, 0x3333);
    gpu.vram.set_pixel(5, 6, 0xAAAA);
    gpu.vram.set_pixel(6, 6, 0xBBBB);
    gpu.vram.set_pixel(7, 6, 0xCCCC);

    gpu.write32(GP0_ADDR, 0x80_00_00_00);
    // High coordinate bits must be ignored: src=(2,3), dst=(5,6).
    gpu.write32(GP0_ADDR, 0x0203_0402);
    gpu.write32(GP0_ADDR, 0x0206_0405);
    // High size bits must be ignored: width=2, height=1.
    gpu.write32(GP0_ADDR, 0x0201_0402);

    assert_eq!(gpu.vram.get_pixel(5, 6), 0x1111);
    assert_eq!(gpu.vram.get_pixel(6, 6), 0x2222);
    assert_eq!(
        gpu.vram.get_pixel(7, 6),
        0xCCCC,
        "masked copy width must not spill into the next pixel"
    );
}

#[test]
fn wireframe_toggle_makes_tri_draw_edges_only() {
    let mut gpu = Gpu::new();
    // Draw area: top-left (0, 0), bottom-right (1023, 511).
    // GP0 0xE3: x (bits 0..=9), y (bits 10..=18).
    gpu.write32(GP0_ADDR, 0xE3_00_00_00);
    // GP0 0xE4: right (bits 0..=9), bottom (bits 10..=18).
    gpu.write32(GP0_ADDR, 0xE4_00_00_00 | 0x3FF | (0x1FF << 10));
    gpu.wireframe_enabled = true;
    // Tiny triangle at (0,0), (4,0), (2,2). With wireframe
    // on, edges get drawn; interior stays zero.
    gpu.write32(GP0_ADDR, 0x20_FF_FF_FF);
    gpu.write32(GP0_ADDR, 0x0000_0000);
    gpu.write32(GP0_ADDR, 0x0000_0004);
    gpu.write32(GP0_ADDR, 0x0002_0002);
    // Corner pixels sit on edges -- must be lit.
    assert_ne!(gpu.vram.get_pixel(0, 0), 0, "corner (0,0)");
    assert_ne!(gpu.vram.get_pixel(4, 0), 0, "corner (4,0)");
    assert_ne!(gpu.vram.get_pixel(2, 2), 0, "corner (2,2)");
    // A fully-interior pixel at (2, 1) sits just inside the
    // triangle and on no edge -- must stay zero.
    assert_eq!(gpu.vram.get_pixel(2, 1), 0, "interior should be empty");
}

#[test]
fn mono_line_horizontal_plots_one_row() {
    let mut gpu = Gpu::new();
    // Draw area: full VRAM.
    gpu.write32(GP0_ADDR, 0xE3_00_00_00); // top-left 0,0
    gpu.write32(GP0_ADDR, 0xE4_00_03_FF); // bot-right 1023,0 (one row)
                                          // Mono line: white, from (0,0) to (9,0).
    gpu.write32(GP0_ADDR, 0x40_FF_FF_FF); // cmd + white
    gpu.write32(GP0_ADDR, 0x0000_0000); // v0 = (0, 0)
    gpu.write32(GP0_ADDR, 0x0000_0009); // v1 = (9, 0)
    for x in 0..=9u16 {
        let px = gpu.vram.get_pixel(x, 0);
        assert_ne!(px, 0, "pixel ({x}, 0) should be set");
    }
    assert_eq!(gpu.vram.get_pixel(10, 0), 0);
}

#[test]
fn mono_line_shallow_diagonal_matches_silicon_half_pixel_tie_break() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE3_00_00_00);
    gpu.write32(GP0_ADDR, 0xE4_00_0A_0A);
    gpu.write32(GP0_ADDR, 0x40_FF_FF_FF);
    gpu.write32(GP0_ADDR, 0x0000_0000);
    gpu.write32(GP0_ADDR, 0x0002_0004);

    for (x, y) in [(0, 0), (1, 1), (2, 1), (3, 2), (4, 2)] {
        assert_ne!(gpu.vram.get_pixel(x, y), 0, "pixel ({x}, {y})");
    }
    assert_eq!(
        gpu.vram.get_pixel(1, 0),
        0,
        "silicon's half-pixel DDA rounds the exact tie downward in screen space",
    );
}

#[test]
fn mono_polyline_end_sentinel_exits_receive_mode() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE3_00_00_00);
    gpu.write32(GP0_ADDR, 0xE4_00_03_FF);
    // Start polyline.
    gpu.write32(GP0_ADDR, 0x48_FF_FF_FF);
    gpu.write32(GP0_ADDR, 0x0000_0000); // v0
    gpu.write32(GP0_ADDR, 0x0000_0005); // v1
    assert!(gpu.polyline.is_some());
    // Another vertex.
    gpu.write32(GP0_ADDR, 0x0000_000A);
    assert!(gpu.polyline.is_some());
    // Terminator.
    gpu.write32(GP0_ADDR, 0x5000_5000);
    assert!(gpu.polyline.is_none());
}

#[test]
fn textured_shaded_tri_consumes_full_packet_without_panic() {
    // Smoke test: feeding a complete 9-word textured-shaded tri
    // packet must not panic or leave the FIFO partially full.
    let mut gpu = Gpu::new();
    // All vertices inside draw area, degenerate (zero-area) triangle
    // so we don't need to chase pixel output -- the dispatch path is
    // what we're testing.
    gpu.write32(GP0_ADDR, 0xE3_00_00_00); // draw area top-left 0,0
    gpu.write32(GP0_ADDR, 0xE4_00_03_FF); // draw area bottom-right 1023,0
    let words = [
        0x34_FF_FF_FFu32, // cmd + c0 = white
        0x0000_0000,      // v0 = (0, 0)
        0x0000_1020,      // uv0 + clut
        0x00FF_00FF,      // c1 = cyan
        0x0000_0000,      // v1 = (0, 0) (degenerate)
        0x0040_0000,      // uv1 + texpage
        0x00_00_FF_00,    // c2 = green
        0x0000_0000,      // v2 = (0, 0)
        0x0000_1020,      // uv2
    ];
    for w in words {
        gpu.write32(GP0_ADDR, w);
    }
    // FIFO must be empty -- the 9-word packet consumed cleanly.
    assert_eq!(gpu.gp0_expected, 0);
}

/// Studio models store authored triangle winding, so adjacent triangles that
/// form one quad generally need a cyclic vertex rotation before their shared
/// diagonal matches GP0(2Ch)'s native split. Cyclic rotation must preserve the
/// exact flat-textured raster, including tie-Y edges and semi-transparency.
///
/// The ordering table is LIFO: if the game inserts authored `tri0` and then
/// `tri1` into one bucket, the GPU receives `tri1` first. GP0(2Ch) likewise
/// rasterizes `(v1,v3,v2)` before `(v0,v1,v2)`. This test drives those exact
/// SDK packet structs and proves that one quad remains pixel-identical to the
/// two source triangles after arbitrary cyclic rotations.
#[test]
fn flat_textured_model_pair_cyclic_rotation_matches_quad_bitexact() {
    use psx_gpu::material::{BlendMode, TextureMaterial};
    use psx_gpu::prim::{QuadTexturedMaterial, TriTextured};

    const TPAGE: u16 = 0x0115;

    fn submit_packet<T>(gpu: &mut Gpu, packet: &T, words: u8) {
        // SAFETY: PSX primitive structs are repr(C), u32-aligned packets with
        // one OT tag followed by exactly WORDS GP0 data words.
        let raw = unsafe {
            core::slice::from_raw_parts((packet as *const T).cast::<u32>(), 1 + words as usize)
        };
        for &word in &raw[1..] {
            gpu.write32(GP0_ADDR, word);
        }
    }

    fn make_gpu() -> Gpu {
        let mut gpu = Gpu::new();
        gpu.write32(GP0_ADDR, 0xE300_0000);
        gpu.write32(GP0_ADDR, 0xE400_0000 | 0x3FF | (0x0FF << 10));
        // 15bpp texture page at (320,256), outside the draw area. Keep STP set
        // so the translucent cases exercise blending rather than overwrite.
        for y in 256..512u16 {
            for x in 320..640u16 {
                let color = (((x as u32) * 3 + (y as u32) * 5) & 0x7FFF) as u16;
                gpu.vram.set_pixel(x, y, color | 0x8001);
            }
        }
        gpu
    }

    fn rotate3<T: Copy>(values: [T; 3], amount: usize) -> [T; 3] {
        match amount % 3 {
            0 => values,
            1 => [values[1], values[2], values[0]],
            _ => [values[2], values[0], values[1]],
        }
    }

    let mut tris = make_gpu();
    let mut quad = make_gpu();
    let mut seed = 0x71C3_4A5Du32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed
    };

    for iteration in 0..2_000usize {
        // Jitter each corner inside its own quadrant. This produces projected
        // model-like convex quads and naturally covers horizontal/tie-Y edges.
        let bx = 12 + (next() % 430) as i16;
        let by = 8 + (next() % 150) as i16;
        let width = 8 + (next() % 135) as i16;
        let height = 8 + (next() % 78) as i16;
        let verts = [
            (bx + (next() % 5) as i16, by + (next() % 5) as i16),
            (bx + width - (next() % 5) as i16, by + (next() % 5) as i16),
            (
                bx + width - (next() % 5) as i16,
                by + height - (next() % 5) as i16,
            ),
            (bx + (next() % 5) as i16, by + height - (next() % 5) as i16),
        ];
        let uvs = [
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
        ];
        let blend = match iteration % 5 {
            0 => BlendMode::Opaque,
            1 => BlendMode::Average,
            2 => BlendMode::Add,
            3 => BlendMode::Subtract,
            _ => BlendMode::AddQuarter,
        };
        let tint = (
            (0x40 + next() % 0xC0) as u8,
            (0x40 + next() % 0xC0) as u8,
            (0x40 + next() % 0xC0) as u8,
        );
        let material = TextureMaterial::opaque(0, TPAGE, tint).with_blend_mode(blend);

        let min_x = verts.iter().map(|point| point.0).min().unwrap().max(0) as u16;
        let max_x = verts.iter().map(|point| point.0).max().unwrap().min(639) as u16;
        let min_y = verts.iter().map(|point| point.1).min().unwrap().max(0) as u16;
        let max_y = verts.iter().map(|point| point.1).max().unwrap().min(255) as u16;
        // Restore the same non-zero destination under each candidate so all
        // four blend modes are compared, not just transparent black.
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let background = (((x as u32) * 7 + (y as u32) * 11) & 0x7FFF) as u16 | 1;
                tris.vram.set_pixel(x, y, background);
                quad.vram.set_pixel(x, y, background);
            }
        }

        // Authored pair: tri0=(v0,v1,v2), tri1=(v1,v3,v2). Rotate each
        // independently as real HMD8 data does, then submit in OT traversal
        // order (tri1 before tri0).
        let tri0_rotation = (next() % 3) as usize;
        let tri0_verts = rotate3([verts[0], verts[1], verts[2]], tri0_rotation);
        let tri0_uvs = rotate3([uvs[0], uvs[1], uvs[2]], tri0_rotation);
        let tri1_rotation = (next() % 3) as usize;
        let tri1_verts = rotate3([verts[1], verts[3], verts[2]], tri1_rotation);
        let tri1_uvs = rotate3([uvs[1], uvs[3], uvs[2]], tri1_rotation);
        let tri1 = TriTextured::with_material_packet_texcoords(tri1_verts, tri1_uvs, material);
        let tri0 = TriTextured::with_material_packet_texcoords(tri0_verts, tri0_uvs, material);
        submit_packet(&mut tris, &tri1, TriTextured::WORDS);
        submit_packet(&mut tris, &tri0, TriTextured::WORDS);

        let one_quad = QuadTexturedMaterial::with_material(verts, uvs, material);
        submit_packet(&mut quad, &one_quad, QuadTexturedMaterial::WORDS);

        let mut differences = 0usize;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                differences += usize::from(tris.vram.get_pixel(x, y) != quad.vram.get_pixel(x, y));
            }
        }
        assert_eq!(
            differences, 0,
            "iteration {iteration}: cyclic source pair differs from quad; \
             verts={verts:?}, blend={blend:?}",
        );
    }
}

/// A room surface is a quad split into two triangles sharing the
/// `0`-`2` diagonal: `tri(v0,v1,v2) + tri(v0,v2,v3)`. The PS1 GPU quad
/// primitive (0x3C) splits on the `1`-`2` diagonal instead. Emitting
/// the quad with packet order `[v1,v0,v2,v3]` makes the hardware split
/// land on the original `0`-`2` edge, so a single quad rasterizes the
/// same two triangles the engine submits today. This proves it
/// bit-exactly across a battery of shapes -- crucially the
/// axis-aligned quads with horizontal edges (tie-Y), the case where
/// `setup_sections`' strict-`>` y-sort could resolve the swapped
/// vertex order differently and leak a 1-LSB seam.
#[test]
fn textured_gouraud_quad_matches_two_triangle_split_bitexact() {
    fn vert_word(x: i32, y: i32) -> u32 {
        ((x as u32) & 0x7FF) | (((y as u32) & 0x7FF) << 16)
    }
    fn uv_word(u: u8, v: u8, hi: u16) -> u32 {
        (u as u32) | ((v as u32) << 8) | ((hi as u32) << 16)
    }
    // tex_page_x = 5*64 = 320, tex_page_y = 256 (bit 4), 15bpp (bits
    // 7-8 = 2). Sampling from VRAM y >= 256 keeps the texture clear of
    // the draw region (y < 256) so no draw feeds back into a texel.
    const TEXPAGE: u16 = 0x0115;

    fn make_gpu() -> Gpu {
        let mut gpu = Gpu::new();
        gpu.write32(GP0_ADDR, 0xE300_0000); // draw area TL (0,0)
        gpu.write32(GP0_ADDR, 0xE400_0000 | 0x3FF | (0x1FF << 10)); // BR
                                                                    // Deterministic non-uniform 15bpp texture over the sample area.
        for y in 256..512u16 {
            for x in 256..640u16 {
                let p = (((x as u32) * 3 + (y as u32) * 5) & 0x7FFF) as u16 | 1;
                gpu.vram.set_pixel(x, y, p);
            }
        }
        gpu
    }

    struct Quad {
        v: [(i32, i32); 4],
        c: [u32; 4],
        uv: [(u8, u8); 4],
    }
    let quads = [
        // Axis-aligned rectangle: horizontal top (v0,v1) + bottom
        // (v3,v2) edges -- two tie-Y pairs, the room wall/floor case.
        Quad {
            v: [(20, 20), (120, 20), (120, 90), (20, 90)],
            c: [0xFF_FFFF, 0x00_00FF, 0x00_FF00, 0xFF_0000],
            uv: [(0, 0), (60, 0), (60, 40), (0, 40)],
        },
        // Trapezoid floor: horizontal near + far edges (tie-Y both).
        Quad {
            v: [(40, 140), (160, 140), (130, 190), (70, 190)],
            c: [0x80_8080, 0x40_C0FF, 0xFF_C040, 0x20_2020],
            uv: [(2, 2), (58, 2), (50, 38), (10, 38)],
        },
        // General quad, no two vertices share a Y.
        Quad {
            v: [(220, 30), (300, 55), (280, 150), (210, 110)],
            c: [0x12_3456, 0x65_4321, 0x00_FFFF, 0xFF_00FF],
            uv: [(5, 5), (40, 10), (55, 45), (8, 50)],
        },
        // Near-vertical skewed wall.
        Quad {
            v: [(340, 40), (380, 35), (385, 160), (345, 150)],
            c: [0xAA_BBCC, 0x11_2233, 0x44_5566, 0x77_8899],
            uv: [(0, 0), (20, 0), (20, 60), (0, 60)],
        },
        // Thin slab with a near-horizontal top edge (1px slope).
        Quad {
            v: [(420, 60), (520, 61), (515, 120), (425, 119)],
            c: [0xFF_FFFF, 0xFF_FFFF, 0x00_0000, 0x00_0000],
            uv: [(0, 0), (63, 0), (63, 30), (0, 30)],
        },
    ];

    for (qi, q) in quads.iter().enumerate() {
        // Path A: the quad's OWN two triangles, in the exact order the
        // 0x3C handler splits them -- [v1,v3,v2] then [v0,v1,v2]. Under
        // silicon-accurate center-sampling a DIFFERENT triangulation
        // (e.g. the engine's 0-2 split) is NOT bit-identical to the quad:
        // the order-dependent tl interpolation anchor differs along the
        // seam. So this asserts the true invariant -- the 0x3C packet
        // renders exactly its constituent 0x34 triangles.
        let mut a = make_gpu();
        for w in [
            0x3400_0000 | q.c[1],
            vert_word(q.v[1].0, q.v[1].1),
            uv_word(q.uv[1].0, q.uv[1].1, 0),
            q.c[3],
            vert_word(q.v[3].0, q.v[3].1),
            uv_word(q.uv[3].0, q.uv[3].1, TEXPAGE),
            q.c[2],
            vert_word(q.v[2].0, q.v[2].1),
            uv_word(q.uv[2].0, q.uv[2].1, 0),
        ] {
            a.write32(GP0_ADDR, w);
        }
        for w in [
            0x3400_0000 | q.c[0],
            vert_word(q.v[0].0, q.v[0].1),
            uv_word(q.uv[0].0, q.uv[0].1, 0),
            q.c[1],
            vert_word(q.v[1].0, q.v[1].1),
            uv_word(q.uv[1].0, q.uv[1].1, TEXPAGE),
            q.c[2],
            vert_word(q.v[2].0, q.v[2].1),
            uv_word(q.uv[2].0, q.uv[2].1, 0),
        ] {
            a.write32(GP0_ADDR, w);
        }

        // Path B: one quad packet in natural order [v0,v1,v2,v3].
        let mut b = make_gpu();
        let order = [0usize, 1, 2, 3];
        for w in [
            0x3C00_0000 | q.c[order[0]],
            vert_word(q.v[order[0]].0, q.v[order[0]].1),
            uv_word(q.uv[order[0]].0, q.uv[order[0]].1, 0),
            q.c[order[1]],
            vert_word(q.v[order[1]].0, q.v[order[1]].1),
            uv_word(q.uv[order[1]].0, q.uv[order[1]].1, TEXPAGE),
            q.c[order[2]],
            vert_word(q.v[order[2]].0, q.v[order[2]].1),
            uv_word(q.uv[order[2]].0, q.uv[order[2]].1, 0),
            q.c[order[3]],
            vert_word(q.v[order[3]].0, q.v[order[3]].1),
            uv_word(q.uv[order[3]].0, q.uv[order[3]].1, 0),
        ] {
            b.write32(GP0_ADDR, w);
        }

        let xs = q.v.iter().map(|p| p.0).min().unwrap().max(0) as u16;
        let xe = (q.v.iter().map(|p| p.0).max().unwrap().min(1023)) as u16;
        let ys = q.v.iter().map(|p| p.1).min().unwrap().max(0) as u16;
        let ye = (q.v.iter().map(|p| p.1).max().unwrap().min(511)) as u16;
        let mut diffs = 0usize;
        for y in ys..=ye {
            for x in xs..=xe {
                if a.vram.get_pixel(x, y) != b.vram.get_pixel(x, y) {
                    diffs += 1;
                }
            }
        }
        assert_eq!(
            diffs, 0,
            "quad {qi}: {diffs} pixels differ between two-triangle split and quad primitive",
        );
    }

    // Randomized sweep: jittered convex quads (perimeter order kept by
    // jittering each corner of a base rectangle within its own
    // bounds), the shape family the engine actually projects. Many
    // land near tie-Y / 1px-slope edges. Deterministic LCG, no host
    // float, so the sweep is reproducible.
    let mut seed: u32 = 0x1234_5678;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        seed
    };
    for it in 0..4000usize {
        // Keep the draw box clear of the texture (y >= 256) so no
        // sub-triangle ever samples a texel another just overwrote --
        // that feedback is draw-order sensitive and would be a test
        // artifact, not a rasterization difference.
        let bx = 30 + (next() % 360) as i32;
        let by = 30 + (next() % 140) as i32;
        let bw = 8 + (next() % 120) as i32;
        let bh = 8 + (next() % 70) as i32;
        // Corners jittered within their own half so the quad stays
        // convex and in clockwise perimeter order.
        let v = [
            (bx + (next() % 4) as i32, by + (next() % 4) as i32),
            (bx + bw - (next() % 4) as i32, by + (next() % 4) as i32),
            (bx + bw - (next() % 4) as i32, by + bh - (next() % 4) as i32),
            (bx + (next() % 4) as i32, by + bh - (next() % 4) as i32),
        ];
        let c = [
            next() & 0xFF_FFFF,
            next() & 0xFF_FFFF,
            next() & 0xFF_FFFF,
            next() & 0xFF_FFFF,
        ];
        let uv = [
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
            ((next() % 64) as u8, (next() % 64) as u8),
        ];

        // Path A: the quad's own split order [v1,v3,v2] then [v0,v1,v2].
        let mut a = make_gpu();
        for w in [
            0x3400_0000 | c[1],
            vert_word(v[1].0, v[1].1),
            uv_word(uv[1].0, uv[1].1, 0),
            c[3],
            vert_word(v[3].0, v[3].1),
            uv_word(uv[3].0, uv[3].1, TEXPAGE),
            c[2],
            vert_word(v[2].0, v[2].1),
            uv_word(uv[2].0, uv[2].1, 0),
        ] {
            a.write32(GP0_ADDR, w);
        }
        for w in [
            0x3400_0000 | c[0],
            vert_word(v[0].0, v[0].1),
            uv_word(uv[0].0, uv[0].1, 0),
            c[1],
            vert_word(v[1].0, v[1].1),
            uv_word(uv[1].0, uv[1].1, TEXPAGE),
            c[2],
            vert_word(v[2].0, v[2].1),
            uv_word(uv[2].0, uv[2].1, 0),
        ] {
            a.write32(GP0_ADDR, w);
        }

        let mut b = make_gpu();
        let o = [0usize, 1, 2, 3];
        for w in [
            0x3C00_0000 | c[o[0]],
            vert_word(v[o[0]].0, v[o[0]].1),
            uv_word(uv[o[0]].0, uv[o[0]].1, 0),
            c[o[1]],
            vert_word(v[o[1]].0, v[o[1]].1),
            uv_word(uv[o[1]].0, uv[o[1]].1, TEXPAGE),
            c[o[2]],
            vert_word(v[o[2]].0, v[o[2]].1),
            uv_word(uv[o[2]].0, uv[o[2]].1, 0),
            c[o[3]],
            vert_word(v[o[3]].0, v[o[3]].1),
            uv_word(uv[o[3]].0, uv[o[3]].1, 0),
        ] {
            b.write32(GP0_ADDR, w);
        }

        let xs = v.iter().map(|p| p.0).min().unwrap().max(0) as u16;
        let xe = (v.iter().map(|p| p.0).max().unwrap().min(1023)) as u16;
        let ys = v.iter().map(|p| p.1).min().unwrap().max(0) as u16;
        let ye = (v.iter().map(|p| p.1).max().unwrap().min(511)) as u16;
        let mut diffs = 0usize;
        for y in ys..=ye {
            for x in xs..=xe {
                if a.vram.get_pixel(x, y) != b.vram.get_pixel(x, y) {
                    diffs += 1;
                }
            }
        }
        assert_eq!(
            diffs, 0,
            "random quad {it}: {diffs} pixels differ (verts {v:?})"
        );
    }
}

/// The real `TriTexturedGouraud` packet (GP0 0x34) rasterizes, and the
/// engine's two-triangle split of a quad face is pixel-identical to the
/// `QuadTexturedGouraud` packet (0x3C) built from the same corners.
///
/// Unlike `textured_gouraud_quad_matches_two_triangle_split_bitexact`,
/// which hand-assembles GP0 words, this drives the *exact struct bytes
/// the engine emits* -- the same SDK constructors the `WorldRenderPass`
/// leaf paths call (`TriTexturedGouraud::with_material_packet_texcoords`
/// and `QuadTexturedGouraud::with_packet_material_packed_uv_words`),
/// including the leading GP0(E2) texture-window word that the
/// hand-assembled sweep omits. A regression here is exactly the
/// invisible-box symptom: `submit_textured_gouraud_triangle` reaches the
/// ordering table but paints nothing. Keeping it green is what lets the
/// per-triangle `TriTexturedGouraud` primitive (world geometry, image
/// props, subdivided cached-room surfaces) stay trusted instead of
/// being mistaken for a dead, broken type.
#[test]
fn tri_textured_gouraud_struct_rasterizes_and_matches_quad() {
    use psx_gpu::material::{TextureMaterial, TexturedGouraudPacketMaterial};
    use psx_gpu::prim::{QuadTexturedGouraud, TriTexturedGouraud};

    // Same texture-page layout as the GP0-word bit-exact sweep: 15bpp,
    // tex_page_x = 5*64 = 320, tex_page_y = 256, so sampling stays clear
    // of the y < 256 draw region (no draw feeds back into a texel).
    const TPAGE: u16 = 0x0115;

    // Feed a packet struct to the GPU as a GP0 word stream, skipping the
    // OT tag (word 0) and sending exactly `words` data words -- i.e. what
    // the DMA linked-list walker delivers for a tag of `(words << 24)`.
    fn submit_packet<T>(gpu: &mut Gpu, packet: &T, words: u8) {
        // SAFETY: every primitive struct is #[repr(C, align(4))] holding
        // (1 + WORDS) u32 fields with no padding; we read only those.
        let raw = unsafe {
            core::slice::from_raw_parts((packet as *const T).cast::<u32>(), 1 + words as usize)
        };
        for &w in &raw[1..] {
            gpu.write32(GP0_ADDR, w);
        }
    }

    fn make_gpu() -> Gpu {
        let mut gpu = Gpu::new();
        gpu.write32(GP0_ADDR, 0xE300_0000); // draw area TL (0,0)
        gpu.write32(GP0_ADDR, 0xE400_0000 | 0x3FF | (0x1FF << 10)); // BR
                                                                    // Deterministic non-zero 15bpp texture (low bit forced set, so no
                                                                    // texel resolves to transparent 0x0000) over the sample area.
        for y in 256..512u16 {
            for x in 256..640u16 {
                let p = (((x as u32) * 3 + (y as u32) * 5) & 0x7FFF) as u16 | 1;
                gpu.vram.set_pixel(x, y, p);
            }
        }
        gpu
    }

    // Opaque, non-raw textured-Gouraud material on the sample page. CLUT
    // is irrelevant at 15bpp; tint 0x80 is neutral (1.0) modulation.
    let material = TextureMaterial::opaque(0, TPAGE, (0x80, 0x80, 0x80));
    let packet_material = TexturedGouraudPacketMaterial::from_texture(material);

    // A clockwise-perimeter quad face, the shape box props project to.
    let verts: [(i16, i16); 4] = [(40, 40), (180, 48), (172, 150), (52, 140)];
    let uvs: [(u8, u8); 4] = [(0, 0), (60, 0), (60, 40), (0, 40)];
    let colors: [(u8, u8, u8); 4] = [
        (0xFF, 0xFF, 0xFF),
        (0x40, 0xC0, 0xFF),
        (0x20, 0xFF, 0x40),
        (0xFF, 0x40, 0x80),
    ];

    // Path A: the two leaf packets `submit_textured_gouraud_triangle`
    // emits when it splits the quad on the 0-2 diagonal.
    let mut a = make_gpu();
    let tri0 = TriTexturedGouraud::with_material_packet_texcoords(
        [verts[0], verts[1], verts[2]],
        [uvs[0], uvs[1], uvs[2]],
        [colors[0], colors[1], colors[2]],
        material,
    );
    let tri1 = TriTexturedGouraud::with_material_packet_texcoords(
        [verts[0], verts[2], verts[3]],
        [uvs[0], uvs[2], uvs[3]],
        [colors[0], colors[2], colors[3]],
        material,
    );
    submit_packet(&mut a, &tri0, TriTexturedGouraud::WORDS);
    submit_packet(&mut a, &tri1, TriTexturedGouraud::WORDS);

    // Path B: the single `QuadTexturedGouraud` packet
    // `submit_textured_gouraud_quad_prescreened_u8` emits, with the
    // [1,0,2,3] reorder that lands the hardware 1-2 diagonal on the
    // original 0-2 edge.
    let mut b = make_gpu();
    let uv_word = |uv: (u8, u8)| (uv.0 as u16) | ((uv.1 as u16) << 8);
    let quad = QuadTexturedGouraud::with_packet_material_packed_uv_words(
        [verts[1], verts[0], verts[2], verts[3]],
        [
            uv_word(uvs[1]),
            uv_word(uvs[0]),
            uv_word(uvs[2]),
            uv_word(uvs[3]),
        ],
        [colors[1], colors[0], colors[2], colors[3]],
        packet_material,
    );
    submit_packet(&mut b, &quad, QuadTexturedGouraud::WORDS);

    let xs = verts.iter().map(|p| p.0).min().unwrap().max(0) as u16;
    let xe = (verts.iter().map(|p| p.0).max().unwrap().min(1023)) as u16;
    let ys = verts.iter().map(|p| p.1).min().unwrap().max(0) as u16;
    let ye = (verts.iter().map(|p| p.1).max().unwrap().min(511)) as u16;

    // The triangle path must actually paint pixels -- the exact
    // assertion the invisible-box symptom would have failed -- and must
    // match the quad packet pixel-for-pixel.
    let mut tri_painted = 0usize;
    let mut diffs = 0usize;
    for y in ys..=ye {
        for x in xs..=xe {
            let pa = a.vram.get_pixel(x, y);
            if pa != 0 {
                tri_painted += 1;
            }
            if pa != b.vram.get_pixel(x, y) {
                diffs += 1;
            }
        }
    }
    assert!(
        tri_painted > 0,
        "TriTexturedGouraud (GP0 0x34) reached the GPU but rasterized no pixels",
    );
    assert_eq!(
        diffs, 0,
        "{diffs} pixels differ between the TriTexturedGouraud 0-2 split and \
             the QuadTexturedGouraud packet built from identical corners",
    );
}

// --- sample_texture transparency rules ---
//
// PSX convention: a texel is transparent when the **resolved
// 16-bit colour** is 0x0000. For 4bpp/8bpp that means
// `CLUT[idx] == 0`, not `idx == 0`. The BIOS TM-glyph regression
// was caused by the simpler `idx == 0` check rendering opaque
// black where the CLUT had deliberately-zero entries at non-zero
// indices to punch the letter cutouts.

// `sample_texture` now reads the CLUT from the cache, which a textured prim
// loads via `update_clut_if_needed(clut_word)`. clut_word -> address:
// clut_x = (word & 0x3F) * 16, clut_y = (word >> 6) & 0x1FF. So word 0x10
// -> x=0x100, word 0x20 -> x=0x200.
#[test]
fn sample_texture_4bpp_idx0_resolves_to_clut_entry() {
    // CLUT entry at index 0 is non-zero → idx==0 is NOT transparent:
    // only the resolved colour's 0x0000 is skipped.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 0; // 4bpp
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    // Texture word at (0, 0) -- all four texels point to CLUT idx 0.
    gpu.vram.set_pixel(0, 0, 0x0000);
    // CLUT row at (0x100, 0), entry 0 = red (0x001F).
    gpu.vram.set_pixel(0x100, 0, 0x001F);
    gpu.update_clut_if_needed(0x10); // load the CLUT cache from (0x100, 0)
                                     // u=0..3 all sample idx=0 → all must resolve to CLUT[0] = 0x001F.
    for u in 0..4u16 {
        assert_eq!(
            gpu.sample_texture(u, 0),
            Some(0x001F),
            "u={u}: CLUT[0]=0x001F should be opaque, not transparent",
        );
    }
}

#[test]
fn sample_texture_4bpp_nonzero_idx_with_zero_clut_is_transparent() {
    // The TM-glyph bug: CLUT entry at non-zero index is 0x0000
    // (deliberate punch-through). Hardware skips (transparent);
    // the bugged emulator would draw opaque black.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 0;
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    // Texture word at (0, 0): low nibble = 5 → idx 5.
    gpu.vram.set_pixel(0, 0, 0x0005);
    // CLUT at (0x200, 0): entry 5 is 0x0000 (punch-through), others non-zero.
    for e in 0..16u16 {
        gpu.vram
            .set_pixel(0x200 + e, 0, if e == 5 { 0x0000 } else { 0x7FFF });
    }
    gpu.update_clut_if_needed(0x20); // load the CLUT cache from (0x200, 0)
    assert_eq!(
        gpu.sample_texture(0, 0),
        None,
        "CLUT[5]=0x0000 should be transparent even though idx=5, not 0",
    );
}

#[test]
fn sample_texture_8bpp_zero_clut_is_transparent_regardless_of_idx() {
    // Same rule for 8bpp mode.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 1; // 8bpp
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    // Texture word at (0, 0) low byte = 42.
    gpu.vram.set_pixel(0, 0, 42);
    // CLUT[42] = 0x0000 → should be transparent.
    gpu.vram.set_pixel(0x100 + 42, 0, 0x0000);
    gpu.update_clut_if_needed(0x10);
    assert_eq!(gpu.sample_texture(0, 0), None);
    // Flip CLUT[42] non-zero in VRAM. Same clut word -> the cache stays stale
    // (faithful PS1 behaviour; index 42 lives in the 240-entry 8bpp-only
    // line), so force a reload to read the new palette.
    gpu.vram.set_pixel(0x100 + 42, 0, 0x1234);
    gpu.update_clut_if_needed(0x10);
    assert_eq!(
        gpu.sample_texture(0, 0),
        None,
        "in-place CLUT rewrite with an unchanged clut word must stay stale"
    );
    gpu.clut_line_b_reg = u32::MAX;
    gpu.update_clut_if_needed(0x10);
    assert_eq!(gpu.sample_texture(0, 0), Some(0x1234));
}

#[test]
fn sample_texture_15bpp_zero_is_transparent() {
    // Direct-colour mode: 0x0000 is transparent, anything else opaque. No CLUT.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 2; // 15bpp
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    gpu.vram.set_pixel(0, 0, 0x0000);
    assert_eq!(gpu.sample_texture(0, 0), None);
    gpu.vram.set_pixel(1, 0, 0x1234);
    assert_eq!(gpu.sample_texture(1, 0), Some(0x1234));
}

#[test]
fn clut_cache_serves_stale_palette_until_clut_word_changes() {
    // PS1 GPU CLUT-cache fidelity: the cache reloads only when the CLUT word
    // changes, NOT when the CLUT data in VRAM is overwritten. This reproduces
    // the PICO-8 `pal()` recolour-via-reupload bug -- a sprite whose CLUT is
    // re-uploaded to the SAME slot keeps sampling the old palette (Madeline's
    // hair stuck on the previous colour) until the clut word actually changes.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 0; // 4bpp
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    gpu.vram.set_pixel(0, 0, 0x0001); // texel -> CLUT idx 1
    gpu.vram.set_pixel(0x100 + 1, 0, 0x001F); // CLUT v1: red
    gpu.update_clut_if_needed(0x10);
    assert_eq!(
        gpu.sample_texture(0, 0),
        Some(0x001F),
        "first draw loads v1"
    );

    // Re-upload the CLUT to the SAME slot (v2: blue), same clut word.
    gpu.vram.set_pixel(0x100 + 1, 0, 0x7C00);
    gpu.update_clut_if_needed(0x10); // same word -> NO reload
    assert_eq!(
        gpu.sample_texture(0, 0),
        Some(0x001F),
        "stale cache keeps v1 -- the bug, faithfully reproduced",
    );

    // The game-side fix: recolour into a DIFFERENT CLUT slot so the clut word
    // changes, forcing the GPU to reload -> the new palette takes effect.
    gpu.vram.set_pixel(0x110 + 1, 0, 0x7C00); // v2 at a second slot (x=0x110)
    gpu.update_clut_if_needed(0x11); // word 0x11 -> x=0x110, reloads
    assert_eq!(
        gpu.sample_texture(0, 0),
        Some(0x7C00),
        "changing the clut word reloads the cache -> new palette",
    );
}

#[test]
fn gp0_clear_texture_cache_reloads_same_clut_word() {
    let mut gpu = Gpu::new();
    gpu.tex_depth = 0;
    gpu.vram.set_pixel(0, 0, 0x0001);
    gpu.vram.set_pixel(0x100 + 1, 0, 0x001F);
    gpu.update_clut_if_needed(0x10);
    assert_eq!(gpu.sample_texture(0, 0), Some(0x001F));

    gpu.vram.set_pixel(0x100 + 1, 0, 0x7C00);
    gpu.update_clut_if_needed(0x10);
    assert_eq!(gpu.sample_texture(0, 0), Some(0x001F));

    gpu.execute_gp0_single(0x0100_0000);
    gpu.update_clut_if_needed(0x10);
    assert_eq!(gpu.sample_texture(0, 0), Some(0x7C00));
}

#[test]
fn textured_rectangle_flip_uses_silicon_counter_origins() {
    let mut gpu = Gpu::new();
    gpu.tex_depth = 2;
    gpu.tex_rect_flip_x = true;
    gpu.tex_rect_flip_y = true;

    // X flip walks u0+1, u0, u0-1, u0-2. Y flip starts at v0 and
    // walks v0-1 on the next destination row (all arithmetic is 8-bit
    // after texture-window application in sample_texture).
    for (x, color) in [(1, 0x001F), (0, 0x03E0), (255, 0x7C00), (254, 0x7FFF)] {
        gpu.vram.set_pixel(x, 0, color);
    }
    for (x, color) in [(1, 0x4210), (0, 0x5294), (255, 0x6318), (254, 0x739C)] {
        gpu.vram.set_pixel(x, 255, color);
    }

    gpu.paint_textured_rect(10, 10, 4, 2, 0, 0, 0, false, (128, 128, 128));
    assert_eq!(
        [
            gpu.vram.get_pixel(10, 10),
            gpu.vram.get_pixel(11, 10),
            gpu.vram.get_pixel(12, 10),
            gpu.vram.get_pixel(13, 10),
        ],
        [0x001F, 0x03E0, 0x7C00, 0x7FFF]
    );
    assert_eq!(
        [
            gpu.vram.get_pixel(10, 11),
            gpu.vram.get_pixel(11, 11),
            gpu.vram.get_pixel(12, 11),
            gpu.vram.get_pixel(13, 11),
        ],
        [0x4210, 0x5294, 0x6318, 0x739C]
    );
}

#[test]
fn sample_texture_uv_wrap_at_256_per_psx_spx() {
    // Regression: the PS1's GPU walks U/V through an 8-bit counter,
    // so a tpage wraps every 256 texels horizontally and vertically.
    // The rasterizer adds a width-bounded `dx` to the base U/V and
    // can pass values >= 256 here; without the explicit `& 0xFF`
    // we read VRAM PAST the tpage edge and pull garbage from the
    // neighbouring tpage. Visible as smeared 2D sprites in pre-fight
    // loading screens (character portraits) and corrupt
    // BIOS dialog frames.
    //
    // Test: in 15bpp mode, place a recognisable colour at tpage
    // origin (u,v)=(0,0) and a different colour at host VRAM
    // (256,0) -- outside the tpage. Sampling at u=256 must return
    // the FIRST colour (wrap), not the second.
    let mut gpu = Gpu::new();
    gpu.tex_depth = 2; // 15bpp -- one VRAM word per texel
    gpu.tex_page_x = 0;
    gpu.tex_page_y = 0;
    gpu.vram.set_pixel(0, 0, 0x1111); // tpage origin
    gpu.vram.set_pixel(256, 0, 0x2222); // outside the tpage
    assert_eq!(
        gpu.sample_texture(256, 0),
        Some(0x1111),
        "u=256 should wrap to u=0 within the tpage"
    );
    assert_eq!(
        gpu.sample_texture(257, 0),
        None,
        "u=257 should wrap to u=1 → vram(1,0)=0 → transparent"
    );
    // V wrap: v=256 → v&0xFF=0 → vram(0,0)=0x1111. v=257 →
    // v&0xFF=1 → vram(0,1) (a different row), confirms v wraps too.
    gpu.vram.set_pixel(0, 1, 0x3333);
    assert_eq!(
        gpu.sample_texture(0, 256),
        Some(0x1111),
        "v=256 wraps to v=0"
    );
    assert_eq!(
        gpu.sample_texture(0, 257),
        Some(0x3333),
        "v=257 wraps to v=1"
    );
}

fn pack_test_vertex(x: i16, y: i16) -> u32 {
    ((x as u16) as u32) | (((y as u16) as u32) << 16)
}

fn pack_test_uv(u: u8, v: u8, extra: u16) -> u32 {
    (u as u32) | ((v as u32) << 8) | ((extra as u32) << 16)
}

fn prepare_opaque_15bpp_texture(gpu: &mut Gpu) -> u16 {
    // 15bpp texture page at x=0, y=256. Keeping texture source
    // away from the draw area makes these tests easier to reason
    // about: any low-screen pixel change came from the primitive.
    const TPAGE_15BPP_Y256: u16 = (1 << 4) | (2 << 7);
    gpu.write32(GP0_ADDR, 0xE3_00_00_00);
    gpu.write32(GP0_ADDR, 0xE4_00_00_00 | 0x3FF | (0x1FF << 10));
    gpu.vram.set_pixel(0, 256, 0x7FFF);
    TPAGE_15BPP_Y256
}

#[test]
fn extent_boundary_inclusive_keeps_triangle() {
    // Exactly 1023 / 511 deltas are *kept* -- only strictly greater
    // is dropped. This matches the hardware spec that the punch
    // list quotes ("|Δx| > 1023 or |Δy| > 511").
    assert!(!triangle_exceeds_hw_extent((0, 0), (1023, 0), (0, 511),));
}

#[test]
fn extent_one_pixel_over_horizontal_drops() {
    assert!(triangle_exceeds_hw_extent((0, 0), (1024, 0), (0, 0)));
}

#[test]
fn extent_one_pixel_over_vertical_drops() {
    assert!(triangle_exceeds_hw_extent((0, 0), (0, 512), (0, 0)));
}

#[test]
fn extent_check_uses_absolute_value() {
    // Negative deltas must trip the check too -- typically a vertex
    // at (-2000, 0) paired with (0, 0).
    assert!(triangle_exceeds_hw_extent((-2000, 0), (0, 0), (0, 0)));
    assert!(triangle_exceeds_hw_extent((0, 0), (0, 0), (0, -700)));
}

#[test]
fn extent_check_compares_every_edge() {
    // First two vertices coincide (|Δ|=0), but v0→v2 is huge.
    // A naive bounding-box check would still be 1024 wide and
    // catch this; what we want to confirm is that edge pairs
    // are visited even when one of them looks small.
    assert!(triangle_exceeds_hw_extent((0, 0), (0, 0), (1024, 0)));
    assert!(triangle_exceeds_hw_extent((1024, 0), (0, 0), (0, 0)));
}

#[test]
fn oversize_textured_triangle_is_dropped() {
    // This is the material-viewer lesson in miniature: projected
    // textured triangles can look mostly sane on screen but still
    // cross the PS1's per-edge extent limit. Hardware drops the
    // whole primitive; engines should split before submitting.
    let mut gpu = Gpu::new();
    let tpage = prepare_opaque_15bpp_texture(&mut gpu);

    gpu.write32(GP0_ADDR, 0x2500_0000); // raw textured triangle
    gpu.write32(GP0_ADDR, pack_test_vertex(-1000, 0));
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(24, 0)); // dx = 1024 -> drop
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, tpage));
    gpu.write32(GP0_ADDR, pack_test_vertex(24, 64));
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));

    assert_eq!(
        gpu.vram.get_pixel(20, 8),
        0,
        "oversize textured triangle should be skipped, not partially drawn",
    );
}

#[test]
fn legal_textured_triangle_one_pixel_under_extent_draws() {
    let mut gpu = Gpu::new();
    let tpage = prepare_opaque_15bpp_texture(&mut gpu);

    gpu.write32(GP0_ADDR, 0x2500_0000); // raw textured triangle
    gpu.write32(GP0_ADDR, pack_test_vertex(-999, 0));
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(24, 0)); // dx = 1023 -> keep
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, tpage));
    gpu.write32(GP0_ADDR, pack_test_vertex(24, 64));
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));

    assert_ne!(
        gpu.vram.get_pixel(20, 8),
        0,
        "triangle at the exact legal extent should still rasterise",
    );
}

#[test]
fn textured_quad_drops_only_the_oversize_split_half() {
    // Non-axis-aligned textured quads are split into two triangles.
    // As with real hardware, the extent rule applies to each half
    // independently; a bad second half must not erase the good one.
    let mut gpu = Gpu::new();
    let tpage = prepare_opaque_15bpp_texture(&mut gpu);

    gpu.write32(GP0_ADDR, 0x2D00_0000); // raw textured quad
    gpu.write32(GP0_ADDR, pack_test_vertex(10, 10)); // v0
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(30, 10)); // v1
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, tpage));
    gpu.write32(GP0_ADDR, pack_test_vertex(10, 30)); // v2
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(30, 522)); // v3: |dy v1->v3| = 512
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));

    assert_ne!(gpu.vram.get_pixel(14, 14), 0, "legal half should draw");
    assert_eq!(
        gpu.vram.get_pixel(28, 120),
        0,
        "oversize split half should be skipped independently",
    );
}

#[test]
fn axis_aligned_textured_quad_draws_right_to_left_order() {
    // One commercial title mirrors its second-player portrait by submitting an axis-
    // aligned textured quad whose first vertical edge is on the
    // right and second vertical edge is on the left. The fast path
    // must draw it instead of treating the negative X span as
    // "handled but empty".
    let mut gpu = Gpu::new();
    let tpage = prepare_opaque_15bpp_texture(&mut gpu);
    for y in 0..4 {
        for x in 0..4 {
            gpu.vram.set_pixel(x, 256 + y, 0x1000 + (y << 4) + x);
        }
    }

    gpu.write32(GP0_ADDR, 0x2D00_0000); // raw textured quad
    gpu.write32(GP0_ADDR, pack_test_vertex(4, 0)); // v0: top-right
    gpu.write32(GP0_ADDR, pack_test_uv(4, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(0, 0)); // v1: top-left
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, tpage));
    gpu.write32(GP0_ADDR, pack_test_vertex(4, 4)); // v2: bottom-right
    gpu.write32(GP0_ADDR, pack_test_uv(4, 4, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(0, 4)); // v3: bottom-left
    gpu.write32(GP0_ADDR, pack_test_uv(0, 4, 0));

    assert_eq!(gpu.vram.get_pixel(0, 0), 0x1000);
    assert_eq!(gpu.vram.get_pixel(1, 0), 0x1001);
    assert_eq!(gpu.vram.get_pixel(2, 0), 0x1002);
    assert_eq!(gpu.vram.get_pixel(3, 0), 0x1003);
}

#[test]
fn axis_aligned_textured_quad_draws_bottom_to_top_order() {
    let mut gpu = Gpu::new();
    let tpage = prepare_opaque_15bpp_texture(&mut gpu);
    for y in 0..4 {
        for x in 0..4 {
            gpu.vram.set_pixel(x, 256 + y, 0x1000 + (y << 4) + x);
        }
    }

    gpu.write32(GP0_ADDR, 0x2D00_0000); // raw textured quad
    gpu.write32(GP0_ADDR, pack_test_vertex(0, 4)); // v0: bottom-left
    gpu.write32(GP0_ADDR, pack_test_uv(0, 4, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(4, 4)); // v1: bottom-right
    gpu.write32(GP0_ADDR, pack_test_uv(4, 4, tpage));
    gpu.write32(GP0_ADDR, pack_test_vertex(0, 0)); // v2: top-left
    gpu.write32(GP0_ADDR, pack_test_uv(0, 0, 0));
    gpu.write32(GP0_ADDR, pack_test_vertex(4, 0)); // v3: top-right
    gpu.write32(GP0_ADDR, pack_test_uv(4, 0, 0));

    assert_eq!(gpu.vram.get_pixel(0, 0), 0x1000);
    assert_eq!(gpu.vram.get_pixel(1, 0), 0x1001);
    assert_eq!(gpu.vram.get_pixel(0, 3), 0x1030);
    assert_eq!(gpu.vram.get_pixel(1, 3), 0x1031);
}

#[test]
fn axis_aligned_textured_quad_uses_silicon_q12_uv_gradient() {
    let mut gpu = Gpu::new();
    gpu.tex_depth = 2;
    gpu.tex_page_y = 256;
    gpu.vram.set_pixel(0, 256, 0x001F);
    gpu.vram.set_pixel(1, 256, 0x03E0);

    assert!(gpu.rasterize_axis_aligned_textured_quad(
        (0, 0),
        (100, 0),
        (0, 1),
        (100, 1),
        (0, 0),
        (1, 0),
        (0, 0),
        (1, 0),
        0,
        false,
        RAW_TEXTURE_TINT,
    ));

    // floor(4096 / 100) = 40 Q12 units per pixel. Starting from
    // 0.5 means U reaches 1 at ceil(2048 / 40) = x52.
    assert_eq!(gpu.vram.get_pixel(51, 0), 0x001F);
    assert_eq!(gpu.vram.get_pixel(52, 0), 0x03E0);
    assert_eq!(gpu.vram.get_pixel(99, 0), 0x03E0);
}

#[test]
fn oversize_monochrome_triangle_is_dropped() {
    // Submit a triangle whose v0→v1 edge is 1500px wide via the
    // GP0 monochrome-tri command. Hardware drops it; we should
    // too -- VRAM stays untouched.
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0x2000_00FF); // 0x20 cmd + red
    gpu.write32(GP0_ADDR, 0x0000_0000); // v0 = (0, 0)
    gpu.write32(GP0_ADDR, 0x0000_05DC); // v1 = (1500, 0) -- 1500 > 1023
    gpu.write32(GP0_ADDR, 0x0064_0064); // v2 = (100, 100)
                                        // No pixel anywhere along the would-be triangle should be set.
    for x in [0u16, 50, 100, 500, 1000, 1500] {
        assert_eq!(
            gpu.vram.get_pixel(x, 0),
            0,
            "pixel ({x}, 0) was written despite oversize triangle",
        );
    }
}

#[test]
fn oversize_quad_drops_only_the_oversize_half() {
    // A four-vertex monochrome quad (GP0 0x28) splits into two
    // triangles: (v0,v1,v2) and (v1,v2,v3). Build one where the
    // first half is sane and the second half has v3 placed so
    // its Δy from v1 exceeds 511 -- only the bad half should be
    // culled.
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0x2800_00FF); // 0x28 + red
    gpu.write32(GP0_ADDR, 0x0000_0000); // v0 = (0, 0)
    gpu.write32(GP0_ADDR, 0x0000_0010); // v1 = (16, 0)
    gpu.write32(GP0_ADDR, 0x0010_0000); // v2 = (0, 16)
                                        // v3 = (16, 600) -- |v3.y - v1.y| = 600 > 511, second triangle drops.
    gpu.write32(GP0_ADDR, 0x0258_0010);
    // Sane half wrote pixels.
    assert_ne!(gpu.vram.get_pixel(1, 1), 0, "first half should rasterise");
    // Oversize half left no pixels in the only place its
    // bounding box could have reached (a row well below the
    // sane half).
    assert_eq!(
        gpu.vram.get_pixel(8, 300),
        0,
        "oversize half should not rasterise",
    );
}

// ---------------------------------------------------------------------
// Triangle rasterization vs REAL SILICON (hardware-tests GPU battery).
//
// The hardware-tests disc draws each primitive into an off-screen
// scratch (VRAM 512,256 size 96x96) through the exact GP0 + draw-offset
// path and FNV-1a hashes the VRAM read-back. On real PS1 hardware (burn
// 2026-06-09, ledger HWB-005) every TRIANGLE's hash diverged while every
// QUAD matched -> the emulator's triangle edge-coverage rule (copied
// from Redux's soft renderer) is not silicon. These tests replay the
// disc's exact path in-process so the rasterizer can be tuned against
// the recorded hardware hashes WITHOUT a burn per iteration.
const SCR_X: u16 = 512;
const SCR_Y: u16 = 256;
const SCR_W: u16 = 96;
const SCR_H: u16 = 96;

/// Replay the disc's `gpu_draw_and_hash`: fill the scratch black, set the
/// draw area + offset to the scratch, run `draw`, then FNV-1a the scratch
/// read-back row-major (the order GPUREAD streams, 2px/word).
fn replay_scratch_hash<F: FnOnce(&mut Gpu)>(draw: F) -> u32 {
    let mut gpu = Gpu::new();
    // gpu_fill(scratch, black) -- GP0 0x02 fill rect, absolute VRAM coords.
    gpu.write32(GP0_ADDR, 0x0200_0000);
    gpu.write32(GP0_ADDR, ((SCR_Y as u32) << 16) | SCR_X as u32);
    gpu.write32(GP0_ADDR, ((SCR_H as u32) << 16) | SCR_W as u32);
    // gpu_draw_env_scratch() -- draw area E3/E4 + draw offset E5 = scratch.
    let (x, y) = (SCR_X as u32, SCR_Y as u32);
    gpu.write32(GP0_ADDR, 0xE300_0000 | (x & 0x3FF) | ((y & 0x1FF) << 10));
    let (rx, ry) = (x + SCR_W as u32 - 1, y + SCR_H as u32 - 1);
    gpu.write32(GP0_ADDR, 0xE400_0000 | (rx & 0x3FF) | ((ry & 0x1FF) << 10));
    gpu.write32(GP0_ADDR, 0xE500_0000 | (x & 0x7FF) | ((y & 0x7FF) << 11));
    draw(&mut gpu);
    let mut hash = 0x811C_9DC5u32;
    for yy in SCR_Y..SCR_Y + SCR_H {
        for xx in SCR_X..SCR_X + SCR_W {
            let p = gpu.vram.get_pixel(xx, yy) as u32;
            hash = (hash ^ p).wrapping_mul(0x0100_0193);
        }
    }
    hash
}

/// Feed a primitive struct to the GPU as a GP0 word stream, skipping the
/// OT `tag` word (matches the disc's `gpu_send_prim`).
fn replay_send_prim<T>(gpu: &mut Gpu, p: &T, words: u8) {
    let base = (p as *const T).cast::<u32>();
    for i in 0..words as usize {
        let w = unsafe { core::ptr::read(base.add(1 + i)) };
        gpu.write32(GP0_ADDR, w);
    }
}

#[test]
fn flat_tri_replay_reproduces_disc_emulator_hash() {
    use psx_gpu::prim::TriFlat;
    // The disc's `test_gpu_draw_flat_tri` primitive, verbatim.
    let tri = TriFlat::new([(8, 8), (88, 16), (40, 88)], 0xc0, 0x40, 0x80);
    let h = replay_scratch_hash(|g| replay_send_prim(g, &tri, TriFlat::WORDS));
    // Target: REAL SILICON's recorded hash (HWB-005 detail line). The old
    // Redux rasterizer produced 0x495AFB4D here and FAILED on hardware.
    assert_eq!(
        h, 0x0412_1005,
        "replay {h:#010x} must match silicon 0x04121005"
    );
}

#[test]
fn flat_triangle_edge_cases_match_silicon() {
    use psx_gpu::prim::TriFlat;
    // Every flat-triangle GPU CHECKS case and its real-silicon hash (HWB-005
    // photos, high 7 nibbles == hash>>4; the OBS column clips the low nibble).
    // The Redux corner-sampled rasterizer failed ALL of these on hardware.
    type FlatTriCase = (&'static str, [(i16, i16); 3], (u8, u8, u8), u32);
    let cases: [FlatTriCase; 4] = [
        // case 105: vertex past the right edge.
        (
            "past-edge",
            [(8, 8), (88, 8), (300, 88)],
            (0xff, 0x80, 0x20),
            0x069F_C0E3,
        ),
        // case 106: negative X coordinate.
        (
            "neg-coord",
            [(8, 8), (-200, 40), (88, 88)],
            (0x20, 0xff, 0x80),
            0x0A3A_16BF,
        ),
        // case 107: X coordinate beyond the 11-bit packet range (wraps).
        (
            "coord-wrap",
            [(8, 48), (1500, 8), (48, 88)],
            (0x80, 0x20, 0xff),
            0x03DF_1731,
        ),
        // case 111: vertex past the bottom edge.
        (
            "past-bottom",
            [(8, 8), (88, 8), (40, 300)],
            (0x30, 0xe0, 0x60),
            0x062D_56B6,
        ),
    ];
    for (name, verts, (r, g, b), silicon_hi7) in cases {
        let tri = TriFlat::new(verts, r, g, b);
        let h = replay_scratch_hash(|gp| replay_send_prim(gp, &tri, TriFlat::WORDS));
        assert_eq!(
            h >> 4,
            silicon_hi7,
            "flat {name}: emulator {h:#010x} (>>4 {:#09x}) != silicon hi7 {silicon_hi7:#09x}",
            h >> 4
        );
    }
}

/// Upload the disc's 16x16 15bpp test texture to VRAM (768,256) and return
/// its tpage word (matches `gpu_upload_tex15` on the hardware-tests disc:
/// Tpage::new(768,256,Bit15).uv_tpage_word -> 0x011C).
fn replay_upload_tex15(gpu: &mut Gpu) -> u16 {
    for y in 0..16u16 {
        for x in 0..16u16 {
            let p = 0x8000u16 | (x << 10) | (y << 5) | ((x ^ y) & 0x1f);
            gpu.vram.set_pixel(768 + x, 256 + y, p);
        }
    }
    0x011C
}

#[test]
fn textured_gouraud_tris_replay_match_silicon() {
    use psx_gpu::prim::TriTexturedGouraud;
    let tpage = 0x011Cu16;
    let cols = [(0x80, 0x80, 0x80), (0xc0, 0x80, 0x40), (0x40, 0xc0, 0x80)];
    let uvs = [(0, 0), (15, 0), (8, 15)];
    // The player's exact primitive. (name, verts, silicon hi7).
    type TexturedTriCase = (&'static str, [(i16, i16); 3], u32);
    let cases: [TexturedTriCase; 3] = [
        // case 108: textured-gouraud tri (player prim), direct. hi7 0200A83.
        ("player", [(8, 8), (88, 16), (40, 88)], 0x0020_0A83),
        // case 112: textured-gouraud with a large span. hi7 C79F556.
        ("large-span", [(4, 4), (92, 8), (400, 90)], 0x0C79_F556),
        // case 113: same prim, the OT+DMA verts (rasterizes identically). hi7 6392570.
        ("ot-dma", [(6, 6), (90, 14), (40, 90)], 0x0639_2570),
    ];
    for (name, verts, silicon_hi7) in cases {
        let tri = TriTexturedGouraud::new(verts, uvs, cols, 0, tpage);
        let h = replay_scratch_hash(|g| {
            replay_upload_tex15(g);
            replay_send_prim(g, &tri, TriTexturedGouraud::WORDS);
        });
        assert_eq!(
            h >> 4,
            silicon_hi7,
            "tex-gouraud {name}: emulator {h:#010x} (>>4 {:#09x}) != silicon {silicon_hi7:#09x}",
            h >> 4
        );
    }
}

#[test]
fn gouraud_tri_replay_matches_silicon() {
    use psx_gpu::prim::TriGouraud;
    // case 102: per-vertex colours. Exercises the determinant-plane colour
    // interpolation, not just coverage. Silicon hi7 = 0x285AC60 (HWB-005).
    let tri = TriGouraud::new(
        [(8, 8), (88, 16), (40, 88)],
        [(0xf0, 0x00, 0x00), (0x00, 0xf0, 0x00), (0x00, 0x00, 0xf0)],
    );
    let h = replay_scratch_hash(|g| replay_send_prim(g, &tri, TriGouraud::WORDS));
    assert_eq!(
        h >> 4,
        0x0285_AC60,
        "gouraud tri {h:#010x} (>>4 {:#09x}) != silicon hi7 0x285AC60",
        h >> 4
    );
}

#[test]
fn flat_quad_replay_still_matches_silicon() {
    use psx_gpu::prim::QuadFlat;
    // The flat quad already PASSED on hardware (silicon == 0x79E53DC5). It
    // draws as two triangles through the SAME rasterizer the flat-tri fix
    // touched, so the new coverage rule must keep tiling the rectangle
    // exactly -- no gap/overlap on the shared diagonal.
    let q = QuadFlat::new([(8, 8), (88, 8), (8, 88), (88, 88)], 0x30, 0xc0, 0x60);
    let h = replay_scratch_hash(|g| replay_send_prim(g, &q, QuadFlat::WORDS));
    assert_eq!(
        h, 0x79E5_3DC5,
        "flat quad {h:#010x} must still match silicon 0x79E53DC5"
    );
}

// Commercial intro repro: the publisher logo screens are
// composited as GP0 0x7C textured 16x16 sprites from an 8bpp texpage at
// (256,256) with a CLUT at (512,304), per-sprite E1 0x6B4 + E2 0x04000
// (offset-only window, mask 0 = no-op) -- captured from the real game's
// cmd_log. The intro renders BLACK in both backends; this reconstructs the
// exact draw state to pin where the sample chain breaks.
#[test]
fn crash_intro_sprite_8bpp_clut_paints() {
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE300_0000); // draw area TL (0,0)
    gpu.write32(GP0_ADDR, 0xE400_0000 | 0x3FF | (0x1FF << 10)); // BR
    gpu.write32(GP0_ADDR, 0xE500_0000); // offset 0
                                        // 8bpp texture page content at (256,256..511): every index byte 0x42.
    for y in 256..512u16 {
        for x in 256..384u16 {
            gpu.vram.set_pixel(x, y, 0x4242);
        }
    }
    // CLUT at (512,304): entry 0x42 = opaque white.
    gpu.vram.set_pixel(512 + 0x42, 304, 0x7FFF);
    // Captured env: E1 texpage 0x6B4 (base 256,256, 8bpp), E2 0x04000.
    gpu.write32(GP0_ADDR, 0xE100_06B4);
    gpu.write32(GP0_ADDR, 0xE200_4000);
    // 0x7C sprite, neutral tint, at (10,10), uv (112,128), clut (512,304).
    gpu.write32(GP0_ADDR, 0x7C80_8080);
    gpu.write32(GP0_ADDR, (10 << 16) | 10);
    gpu.write32(GP0_ADDR, 0x4C20_8070); // clut 0x4C20, v=0x80, u=0x70
    let mut lit = 0;
    for y in 10..26u16 {
        for x in 10..26u16 {
            if gpu.vram.get_pixel(x, y) != 0 {
                lit += 1;
            }
        }
    }
    assert_eq!(
        lit, 256,
        "expected all 256 sprite pixels painted, got {lit}"
    );
}

// ---- gpu-dither faithfulness tests ----
#[test]
fn dither_rgb_produces_checkerboard_on_flat_midtone() {
    // A flat 24-bit mid-grey (128) must alternate 5-bit 15/16 across
    // a scanline -- the signed matrix rounds some cells DOWN. The
    // old brighten-only model returned a uniform 16 for the whole
    // tile, which is the artifact this fix removes.
    //   x=0 -> offset -4 -> (128-4)>>3 = 15
    //   x=1 -> offset  0 -> (128  )>>3 = 16
    //   x=2 -> offset -3 -> (128-3)>>3 = 15
    //   x=3 -> offset  1 -> (128+1)>>3 = 16
    let expected = [15u16, 16, 15, 16];
    for (x, &er) in expected.iter().enumerate() {
        let v = dither_rgb(128, 128, 128, x as i32, 0);
        assert_eq!(v & 0x1F, er, "R at x={x}");
        assert_eq!((v >> 5) & 0x1F, er, "G at x={x}");
        assert_eq!((v >> 10) & 0x1F, er, "B at x={x}");
    }
}

#[test]
fn dither_rgb_can_round_down() {
    // The signed model rounds DOWN where the old round-up model never
    // could: 120 at cell (0,0) has offset -4 -> (116)>>3 = 14, below
    // the plain truncation 120>>3 = 15.
    let v = dither_rgb(120, 120, 120, 0, 0);
    assert_eq!(v & 0x1F, 14, "negative offset must round the channel down");
    // And it rounds UP where the offset is positive: 126 at (2,1)
    // has offset +3 -> (129)>>3 = 16, above plain 126>>3 = 15.
    let v = dither_rgb(126, 126, 126, 2, 1);
    assert_eq!(v & 0x1F, 16, "positive offset must round the channel up");
}

#[test]
fn dither_rgb_clamps_both_ends() {
    // Max channel stays 31 for every cell (the +offset clamp guard),
    // and min channel stays 0 (the -offset clamp guard).
    for x in 0..4 {
        for y in 0..4 {
            let hi = dither_rgb(255, 255, 255, x, y);
            assert_eq!(hi & 0x1F, 31);
            assert_eq!((hi >> 5) & 0x1F, 31);
            assert_eq!((hi >> 10) & 0x1F, 31);
            let lo = dither_rgb(0, 0, 0, x, y);
            assert_eq!(lo, 0, "black must stay 0 at ({x},{y})");
        }
    }
}

#[test]
fn flat_tint_textured_poly_dither_varies_across_tile() {
    // Finding [7]: the flat-tint textured-poly path now feeds
    // modulate_tint_dithered when dither is enabled. Verify the
    // dithered modulation actually varies across the 4×4 tile for a
    // mid texel + flat tint, whereas the undithered modulation is a
    // single constant value -- this is exactly the difference the
    // call-site change exposes for non-Gouraud textured polygons.
    let texel = 16u16 | (16 << 5) | (16 << 10); // mid 5-bit grey
    let (tr, tg, tb) = (0xC0u32, 0xC0u32, 0xC0u32); // flat tint, not raw

    let flat = modulate_tint(texel, tr, tg, tb);
    let mut seen = std::collections::BTreeSet::new();
    for y in 0..4 {
        for x in 0..4 {
            seen.insert(modulate_tint_dithered(texel, tr, tg, tb, x, y));
        }
    }
    assert!(
        seen.len() > 1,
        "dithered flat-tint modulation must vary across the tile, got only {flat:#06x}"
    );
}

#[test]
fn modulate_tint_dithered_preserves_mask_bit() {
    // The texel's bit 15 must survive the dithered tint path so the
    // semi-transparency check downstream still sees it.
    let texel = 0x8000u16 | 16 | (16 << 5) | (16 << 10);
    let out = modulate_tint_dithered(texel, 0x80, 0x80, 0x80, 0, 0);
    assert_eq!(out & 0x8000, 0x8000);
}

// ---- gpu-misc faithfulness tests ----
#[test]
fn gp1_reset_clears_active_texture_window() {
    // [5] GP1(00) must clear the DECODED texture-window mask/offset, not
    // just the E2 readback latch -- PSX-SPX defines reset as GP0(E2)=0.
    let mut gpu = Gpu::new();
    // Full mask on both axes + non-zero offsets.
    let word = 0xE200_0000u32 | 0x1F | (0x1F << 5) | (0x1F << 10) | (0x1F << 15);
    gpu.write32(GP0_ADDR, word);
    assert_eq!(gpu.tex_window_mask_x, 0xF8);
    assert_eq!(gpu.tex_window_mask_y, 0xF8);
    assert_eq!(gpu.tex_window_offset_x, 0xF8);
    assert_eq!(gpu.tex_window_offset_y, 0xF8);

    gpu.write32(GP1_ADDR, 0x0000_0000); // GP1(00) reset
    assert_eq!(gpu.tex_window_mask_x, 0, "reset must clear mask_x");
    assert_eq!(gpu.tex_window_mask_y, 0, "reset must clear mask_y");
    assert_eq!(gpu.tex_window_offset_x, 0, "reset must clear offset_x");
    assert_eq!(gpu.tex_window_offset_y, 0, "reset must clear offset_y");
    // The E2 readback latch (GP1 0x10 sub-op 2) is now consistent: 0.
    gpu.write32(GP1_ADDR, 0x1000_0002);
    assert_eq!(gpu.read32(GP0_ADDR).unwrap(), 0);
}

#[test]
fn blend_average_sums_then_halves_over_full_sweep() {
    // [6] Mode 0 is `(B + F) >> 1` per PSX-SPX/PS1 hardware: sum first,
    // then halve. Sweep every 5-bit (B, F) pair on one channel.
    for b in 0..=31u16 {
        for f in 0..=31u16 {
            let out = blend_pixel(b, f, BlendMode::Average);
            let expect = (b + f) >> 1;
            assert_eq!(out & 0x1F, expect, "R: B={b} F={f}");
            // Same on green/blue lanes.
            let bg = b | (b << 5) | (b << 10);
            let fg = f | (f << 5) | (f << 10);
            let out3 = blend_pixel(bg, fg, BlendMode::Average);
            assert_eq!(out3 & 0x1F, expect, "R3: B={b} F={f}");
            assert_eq!((out3 >> 5) & 0x1F, expect, "G3: B={b} F={f}");
            assert_eq!((out3 >> 10) & 0x1F, expect, "B3: B={b} F={f}");
        }
    }
    // The two canonical odd+odd cases that were 1 LSB too dark before.
    assert_eq!(blend_pixel(3, 3, BlendMode::Average) & 0x1F, 3);
    assert_eq!(blend_pixel(5, 3, BlendMode::Average) & 0x1F, 4);
}

#[test]
fn cpu_vram_upload_honours_check_mask_bit() {
    // [8] With check-mask (E6h bit1) on, a CPU->VRAM upload must not
    // overwrite a destination pixel whose bit15 is already set.
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(0, 0, 0x8000); // mask bit only
    gpu.write32(GP0_ADDR, 0xE600_0002); // check-mask
    gpu.gp0_push(0xA0_00_00_00);
    gpu.gp0_push(0x0000_0000); // x=0, y=0
    gpu.gp0_push(0x0001_0001); // w=1, h=1 -> 1 payload word
    gpu.gp0_push(0x0000_001F); // upload red into (0,0); second pixel off-rect
    assert_eq!(
        gpu.vram.get_pixel(0, 0),
        0x8000,
        "protected pixel survives a masked upload"
    );
}

#[test]
fn cpu_vram_upload_honours_force_mask_bit() {
    // [8] With force-mask (E6h bit0) on, uploaded pixels get bit15 forced.
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE600_0001); // force-mask
    gpu.gp0_push(0xA0_00_00_00);
    gpu.gp0_push(0x0000_0000); // x=0, y=0
    gpu.gp0_push(0x0001_0001); // w=1, h=1
    gpu.gp0_push(0x0000_001F); // red, bit15 clear in the source word
    assert_eq!(
        gpu.vram.get_pixel(0, 0),
        0x801F,
        "force-mask must OR bit15 into the uploaded pixel"
    );
}

#[test]
fn vram_copy_honours_check_mask_bit() {
    // [8] Check-mask must also protect VRAM->VRAM copy destinations.
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(2, 3, 0x001F); // source = red
    gpu.vram.set_pixel(5, 6, 0x8000); // dest pre-masked
    gpu.write32(GP0_ADDR, 0xE600_0002); // check-mask
    gpu.write32(GP0_ADDR, 0x80_00_00_00);
    gpu.write32(GP0_ADDR, 0x0003_0002); // src (2,3)
    gpu.write32(GP0_ADDR, 0x0006_0005); // dst (5,6)
    gpu.write32(GP0_ADDR, 0x0001_0001); // 1x1
    assert_eq!(
        gpu.vram.get_pixel(5, 6),
        0x8000,
        "masked dest survives VRAM->VRAM copy"
    );
}

#[test]
fn vram_copy_honours_force_mask_bit() {
    // [8] Force-mask must OR bit15 into VRAM->VRAM copy destinations.
    let mut gpu = Gpu::new();
    gpu.vram.set_pixel(2, 3, 0x001F); // source = red, bit15 clear
    gpu.write32(GP0_ADDR, 0xE600_0001); // force-mask
    gpu.write32(GP0_ADDR, 0x80_00_00_00);
    gpu.write32(GP0_ADDR, 0x0003_0002); // src (2,3)
    gpu.write32(GP0_ADDR, 0x0006_0005); // dst (5,6)
    gpu.write32(GP0_ADDR, 0x0001_0001); // 1x1
    assert_eq!(
        gpu.vram.get_pixel(5, 6),
        0x801F,
        "force-mask must set bit15 on the copied pixel"
    );
}

/// Wireframe edge journal + toggle transitions.
///
/// - Toggling ON blacks out the framebuffer once (the last textured
///   frame would otherwise sit frozen behind the edges -- racing and
///   fighting scenes looked like wires over a stale photo).
/// - Edges over content the game draws DURING wireframe restore that
///   content bit-exact when they age out, not black (erase-to-black
///   scarred a never-repainted menu backdrop).
/// - Pixels the game overdraws keep the game's content.
/// - Toggling OFF restores what the on-clear removed.
#[test]
fn wireframe_erase_restores_background_not_black() {
    let xy = |x: u32, y: u32| (y << 16) | x;
    let mut gpu = Gpu::new();
    gpu.write32(GP0_ADDR, 0xE300_0000); // draw area top-left (0,0)
    gpu.write32(GP0_ADDR, 0xE400_0000 | (255 << 10) | 255); // bottom-right (255,255)

    // The pre-wireframe frame (what the on-transition must clear and
    // the off-transition must bring back).
    for y in 0..64u16 {
        for x in 0..64u16 {
            gpu.vram.set_pixel(x, y, 0x4321);
        }
    }

    gpu.wireframe_enabled = true;
    gpu.toggle_vblank_field(); // on-transition: framebuffer blacked out
    assert_eq!(
        gpu.vram.get_pixel(5, 5),
        0,
        "frozen frame cleared on toggle-on"
    );

    // The game repaints its background while wireframe is on (Crash
    // re-blits every frame). Edges must not scar it.
    for y in 0..64u16 {
        for x in 0..64u16 {
            gpu.vram.set_pixel(x, y, 0x1234);
        }
    }

    // Frame A: mono opaque tri over the background -- edges journaled.
    gpu.write32(GP0_ADDR, 0x20FF_FFFF);
    gpu.write32(GP0_ADDR, xy(5, 5));
    gpu.write32(GP0_ADDR, xy(40, 10));
    gpu.write32(GP0_ADDR, xy(10, 40));
    assert_ne!(gpu.vram.get_pixel(5, 5), 0x1234, "edge drawn over the bg");

    // The game also redraws part of its UI over one of our edge pixels
    // (leaderboard screens scroll their text rows). When that edge ages
    // out, the game's newer content must stand -- restoring the stale
    // saved value would leave ghost content.
    let (gx, gy) = (40u16, 10u16); // triangle vertex = guaranteed edge pixel
    assert_ne!(
        gpu.vram.get_pixel(gx, gy),
        0x1234,
        "vertex pixel is an edge"
    );
    gpu.vram.set_pixel(gx, gy, 0x5678); // game draws new content over it

    // The scene moves on: new geometry elsewhere, one vblank per render.
    // After two more renders frame A is two generations old and erased.
    for i in 0..3u32 {
        gpu.toggle_vblank_field();
        gpu.write32(GP0_ADDR, 0x20FF_FFFF);
        gpu.write32(GP0_ADDR, xy(100 + i, 100));
        gpu.write32(GP0_ADDR, xy(140 + i, 110));
        gpu.write32(GP0_ADDR, xy(110 + i, 140));
    }
    gpu.toggle_vblank_field();

    assert_eq!(
        gpu.vram.get_pixel(5, 5),
        0x1234,
        "aged-out edge restores the original background, not black"
    );
    assert_eq!(
        gpu.vram.get_pixel(gx, gy),
        0x5678,
        "content the game drew over an aged-out edge is preserved"
    );

    // Toggling wireframe off flushes live edges and undoes the
    // on-transition clear where nothing was drawn since.
    gpu.wireframe_enabled = false;
    gpu.toggle_vblank_field();
    for y in 0..64u16 {
        for x in 0..64u16 {
            if (x, y) == (gx, gy) {
                continue; // game-overdrawn pixel keeps the game's content
            }
            assert_eq!(gpu.vram.get_pixel(x, y), 0x1234, "flush at ({x},{y})");
        }
    }
    assert_eq!(gpu.vram.get_pixel(gx, gy), 0x5678);
    // Outside the game-repainted region, black canvas pixels return to
    // the pre-wireframe frame.
    assert_eq!(
        gpu.vram.get_pixel(200, 200),
        0,
        "untouched region was 0 before wireframe and stays 0"
    );
}

/// GP1(00h) is spec-equivalent to GP0(E1h..E6h)=0: the DECODED clip
/// rectangle and draw offset must reset, not just their readback
/// latches. The demo-disc IRQ probe painted nothing on a real console
/// because the emulator used to keep the previous wide-open area.
#[test]
fn gp1_reset_zeroes_the_decoded_draw_area_and_offset() {
    let mut gpu = Gpu::new();
    gpu.gp0_push(0x60FF_FFFF); // monochrome rect, white
    gpu.gp0_push(0x0010_0010); // at (16,16)
    gpu.gp0_push(0x0001_0001); // 1x1
    assert_ne!(gpu.vram.get_pixel(16, 16), 0, "pre-reset draw must land");

    gpu.apply_gp1_display(0x0000_0000); // GP1(00h) reset
    gpu.gp0_push(0x60FF_FFFF);
    gpu.gp0_push(0x0020_0020); // (32,32)
    gpu.gp0_push(0x0001_0001);
    assert_eq!(
        gpu.vram.get_pixel(32, 32),
        0,
        "after GP1(00h) the clip rect is the origin pixel; this draw must vanish"
    );

    gpu.gp0_push(0xE300_0000); // draw area top-left (0,0)
    gpu.gp0_push(0xE400_0000 | (511 << 10) | 1023); // bottom-right, full VRAM
    gpu.gp0_push(0x60FF_FFFF);
    gpu.gp0_push(0x0030_0030); // (48,48)
    gpu.gp0_push(0x0001_0001);
    assert_ne!(
        gpu.vram.get_pixel(48, 48),
        0,
        "an explicit E3/E4 after the reset draws again"
    );
}

/// The GP0 overflow diagnostic counts CPU words that arrive past the
/// 16-word FIFO while the GPU is busy, and resets its occupancy the
/// moment a write finds the GPU idle. Non-strict mode never rejects.
#[test]
fn unpaced_gp0_bursts_past_the_fifo_are_counted() {
    let mut gpu = Gpu::new();
    assert!(gpu.note_cpu_gp0_arrival(), "idle GPU accepts");
    gpu.charge_busy(1_000_000);
    for word in 0..Gpu::GP0_FIFO_DEPTH {
        assert!(gpu.note_cpu_gp0_arrival(), "word {word} fits the FIFO");
    }
    assert_eq!(gpu.gp0_overflow_count(), 0);
    assert!(gpu.note_cpu_gp0_arrival(), "non-strict mode still accepts");
    assert_eq!(gpu.gp0_overflow_count(), 1);

    gpu.decay_busy(2_000_000);
    assert!(gpu.note_cpu_gp0_arrival());
    gpu.charge_busy(10);
    assert!(
        gpu.note_cpu_gp0_arrival(),
        "occupancy reset after an idle write"
    );
    assert_eq!(
        gpu.gp0_overflow_count(),
        1,
        "no new overflow after the reset"
    );
}
