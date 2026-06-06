use super::*;

fn write_adpcm_block(spu: &mut Spu, byte_addr: u32, block: &[u8; 16]) {
    for i in 0..8 {
        let lo = block[i * 2] as u16;
        let hi = block[i * 2 + 1] as u16;
        spu.ram[((byte_addr as usize + i * 2) / 2) & (SPU_RAM_HALFWORDS - 1)] = lo | (hi << 8);
    }
}

// -- address range --

#[test]
fn contains_covers_whole_range() {
    assert!(Spu::contains(SPU_BASE));
    assert!(Spu::contains(SPU_END - 1));
    assert!(!Spu::contains(SPU_END));
    assert!(!Spu::contains(SPU_BASE - 1));
}

// -- register round-trip --

#[test]
fn spucnt_round_trip_and_spustat_mirror() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 0x8010);
    assert_eq!(s.spucnt(), 0x8010);
    // SPUSTAT lower 6 bits mirror SPUCNT lower 6 bits.
    assert_eq!(s.spustat() & 0x3F, 0x10);
}

#[test]
fn spustat_read_only_drops_writes() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 0x8010);
    s.write16(SPUSTAT, 0xFFFF);
    assert_eq!(s.spustat() & 0x3F, 0x10);
}

#[test]
fn voice_bank_round_trips_volume_pitch_start_loop() {
    let mut s = Spu::new();
    let base = VOICE_BASE + 5 * 16;
    s.write16(base + voice_offset::VOLUME_L, 0x3FFF);
    s.write16(base + voice_offset::VOLUME_R, 0x1234);
    s.write16(base + voice_offset::PITCH, 0x1000);
    s.write16(base + voice_offset::START_ADDR, 0x0020);
    s.write16(base + voice_offset::REPEAT_ADDR, 0x0040);
    s.write16(base + voice_offset::ADSR_LO, 0x80FF);
    s.write16(base + voice_offset::ADSR_HI, 0x1F20);
    assert_eq!(s.read16(base + voice_offset::VOLUME_L), 0x3FFF);
    assert_eq!(s.read16(base + voice_offset::VOLUME_R), 0x1234);
    assert_eq!(s.read16(base + voice_offset::PITCH), 0x1000);
    assert_eq!(s.read16(base + voice_offset::START_ADDR), 0x0020);
    assert_eq!(s.read16(base + voice_offset::REPEAT_ADDR), 0x0040);
    assert_eq!(s.read16(base + voice_offset::ADSR_LO), 0x80FF);
    assert_eq!(s.read16(base + voice_offset::ADSR_HI), 0x1F20);
}

#[test]
fn voice_adsr_current_reads_live_envelope() {
    // ENVX exposes the live envelope level (hardware behavior). Games
    // poll it -- e.g. a commercial title spins until every voice reaches 0
    // before advancing past its intro -- so it must reflect the real
    // envelope, not a pinned constant. See `read_voice_reg`.
    let mut s = Spu::new();
    s.voices[0].phase = AdsrPhase::Attack;
    s.voices[0].envelope = 0x4000;
    assert_eq!(s.read16(VOICE_BASE + voice_offset::ADSR_CURRENT), 0x4000);
    s.voices[0].envelope = 0;
    assert_eq!(s.read16(VOICE_BASE + voice_offset::ADSR_CURRENT), 0);
}

#[test]
fn current_main_volume_regs_do_not_mirror_main_volume() {
    let mut s = Spu::new();
    s.write16(MAIN_VOL_L, 0x3FFF);
    s.write16(MAIN_VOL_R, 0x2000);

    assert_eq!(s.read16(MAIN_VOL_L), 0x3FFF);
    assert_eq!(s.read16(MAIN_VOL_R), 0x2000);
    assert_eq!(s.read16(CURRENT_MAIN_VOL_L), 0);
    assert_eq!(s.read16(CURRENT_MAIN_VOL_R), 0);

    s.write16(CURRENT_MAIN_VOL_L, 0x1111);
    s.write16(CURRENT_MAIN_VOL_R, 0x2222);
    assert_eq!(s.read16(CURRENT_MAIN_VOL_L), 0x1111);
    assert_eq!(s.read16(CURRENT_MAIN_VOL_R), 0x2222);
    assert_eq!(s.read16(MAIN_VOL_L), 0x3FFF);
    assert_eq!(s.read16(MAIN_VOL_R), 0x2000);
}

// -- KON / KOFF / ENDX --

#[test]
fn kon_queues_and_apply_starts_voice() {
    let mut s = Spu::new();
    s.write16(KON_LO, 0x0005); // voices 0 and 2
    assert_eq!(s.kon_pending, 0x5);
    s.apply_kon_koff();
    assert_eq!(s.voices[0].phase, AdsrPhase::Attack);
    assert_eq!(s.voices[2].phase, AdsrPhase::Attack);
    assert_eq!(s.voices[1].phase, AdsrPhase::Off);
}

#[test]
fn kon_koff_reads_round_trip_after_tick_drains_pending() {
    // Real hardware: KON/KOFF reads return what was written, even
    // after the SPU has consumed the pending bits internally. The
    // BIOS's SPU-init probe writes 0xFFFF to KOFF then reads it
    // back -- we must echo the written value.
    let mut s = Spu::new();
    s.write16(KON_LO, 0xFFFF);
    s.write16(KON_HI, 0x00FF);
    s.write16(KOFF_LO, 0xFFFF);
    s.write16(KOFF_HI, 0x00FF);
    // Drain pending by ticking the SPU.
    s.tick_sample(0);
    // Reads must still return the raw written values.
    assert_eq!(s.read16(KON_LO), 0xFFFF);
    assert_eq!(s.read16(KON_HI), 0x00FF);
    assert_eq!(s.read16(KOFF_LO), 0xFFFF);
    assert_eq!(s.read16(KOFF_HI), 0x00FF);
}

#[test]
fn koff_queues_and_transitions_to_release() {
    let mut s = Spu::new();
    s.voices[3].phase = AdsrPhase::Sustain;
    s.write16(KOFF_LO, 1 << 3);
    s.apply_kon_koff();
    assert_eq!(s.voices[3].phase, AdsrPhase::Release);
}

#[test]
fn kon_wins_over_same_sample_koff_for_redux_parity() {
    let mut s = Spu::new();
    s.write16(KOFF_LO, 0x0001);
    s.write16(KON_LO, 0x0001);
    s.tick_sample(SAMPLE_CYCLES);

    assert_eq!(
        s.voices[0].phase,
        AdsrPhase::Attack,
        "Redux StartSound clears Stop, so same-batch KON must not immediately release"
    );
}

#[test]
fn endx_write_one_clears_bits() {
    let mut s = Spu::new();
    s.endx_latched = 0xFFFF_FFFF;
    s.write16(ENDX_LO, 0x00F0);
    assert_eq!(s.endx_latched & 0xFFFF, 0xFF0F);
}

#[test]
fn kon_clears_endx_for_started_voices() {
    let mut s = Spu::new();
    s.endx_latched = 0xFFFF_FFFF;
    s.write16(KON_LO, 0x0003);
    // KON queue path clears ENDX immediately for those bits.
    assert_eq!(s.endx_latched & 0x3, 0);
    // Other bits untouched.
    assert_eq!(s.endx_latched >> 2, 0x3FFF_FFFF);
}

// -- Transfer FIFO --

#[test]
fn transfer_fifo_writes_into_spu_ram_and_advances() {
    let mut s = Spu::new();
    s.write16(TRANSFER_ADDR, 0x0010); // 0x10 * 8 = 0x80 bytes
    s.write16(TRANSFER_FIFO, 0xBEEF);
    s.write16(TRANSFER_FIFO, 0xCAFE);
    assert_eq!(s.ram[0x80 >> 1], 0xBEEF);
    assert_eq!(s.ram[(0x80 >> 1) + 1], 0xCAFE);
    // Transfer addr advanced by 4 bytes.
    assert_eq!(s.transfer_addr, 0x84);
}

#[test]
fn dma_write_fills_spu_ram_contiguously() {
    let mut s = Spu::new();
    s.write16(TRANSFER_ADDR, 0); // byte addr 0
    let payload: Vec<u16> = (0..16).map(|i| 0x1000 + i).collect();
    s.dma_write(&payload);
    for (i, w) in payload.iter().enumerate() {
        assert_eq!(s.ram[i], *w);
    }
}

// -- IRQ on address match --

#[test]
fn irq_fires_when_transfer_hits_irq_addr_with_enable() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 1 << 6); // IRQ enable
    s.write16(IRQ_ADDR, 0x0010); // 0x10 * 8 = 0x80 bytes
    s.write16(TRANSFER_ADDR, 0x0010);
    s.write16(TRANSFER_FIFO, 0xAAAA);
    assert!(s.take_irq_pending());
    // Subsequent read clears the pending flag.
    assert!(!s.take_irq_pending());
    // STATUS bit 6 is latched.
    assert_ne!(s.spustat() & (1 << 6), 0);
}

#[test]
fn irq_does_not_fire_without_enable_bit() {
    let mut s = Spu::new();
    s.write16(IRQ_ADDR, 0x0010);
    s.write16(TRANSFER_ADDR, 0x0010);
    s.write16(TRANSFER_FIFO, 0xAAAA);
    assert!(!s.take_irq_pending());
}

#[test]
fn decoded_buffer_irq_fires_in_low_capture_banks() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 1 << 6); // IRQ enable
    s.write16(IRQ_ADDR, 0x0080); // 0x80 * 8 = 0x400 bytes

    s.tick_sample(0);

    assert!(s.take_irq_pending());
    assert_ne!(s.spustat() & (1 << 6), 0);
}

#[test]
fn decoded_buffer_irq_fires_at_zero_irq_address() {
    // Address 0 (CD-left capture buffer start) is a valid SPU IRQ
    // target. CTR arms its SCEA advance-timer SPU IRQ there; gating
    // it off (the old behavior) froze the game on the SCEA screen.
    // SPUCNT bit 6 already guards against firing before a game arms.
    let mut s = Spu::new();
    s.write16(SPUCNT, 1 << 6); // IRQ enable
    s.write16(IRQ_ADDR, 0x0000);

    s.tick_sample(0);

    assert!(s.take_irq_pending());
    assert_ne!(s.spustat() & (1 << 6), 0);
}

#[test]
fn clearing_spucnt_irq_enable_acks_status_bit() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 1 << 6); // IRQ enable
    s.write16(IRQ_ADDR, 0x0010);
    s.write16(TRANSFER_ADDR, 0x0010);
    s.write16(TRANSFER_FIFO, 0x1234);
    assert_ne!(s.spustat & (1 << 6), 0);
    s.write16(SPUCNT, 0); // drop IRQ enable
    assert_eq!(s.spustat & (1 << 6), 0);
}

// -- DMA enable gating --

#[test]
fn dma_transfer_enabled_reads_spucnt_bits_5_4() {
    let mut s = Spu::new();
    s.write16(SPUCNT, 0); // Stop
    assert!(!s.dma_transfer_enabled());
    s.write16(SPUCNT, 1 << 4); // ManualWrite
    assert!(!s.dma_transfer_enabled());
    s.write16(SPUCNT, 2 << 4); // DMA write
    assert!(s.dma_transfer_enabled());
    s.write16(SPUCNT, 3 << 4); // DMA read
    assert!(s.dma_transfer_enabled());
}

// -- ADPCM decoder --

#[test]
fn adpcm_silence_block_decodes_to_zero_samples() {
    let mut s = Spu::new();
    // Shift=0, predictor=0, flags=0. All zero block = 28 zero samples.
    write_adpcm_block(&mut s, 0x20, &[0; 16]);
    s.voices[0].current_addr = 0x20;
    s.decode_next_block(0);
    assert_eq!(s.voices[0].sample_buf, [0; 28]);
    assert_eq!(s.voices[0].current_addr, 0x30);
}

#[test]
fn adpcm_decode_uses_redux_shift_direction() {
    let mut s = Spu::new();
    let mut block = [0u8; 16];
    // Predictor 0, shift 0. The first packed byte contains two
    // signed 4-bit samples: +1 then +2.
    block[0] = 0x00;
    block[2] = 0x21;
    write_adpcm_block(&mut s, 0x20, &block);
    s.voices[0].current_addr = 0x20;

    s.decode_next_block(0);

    assert_eq!(s.voices[0].sample_buf[0], 0x1000);
    assert_eq!(s.voices[0].sample_buf[1], 0x2000);
}

#[test]
fn adpcm_decode_keeps_unclamped_redux_predictor_history() {
    let mut s = Spu::new();
    let mut block = [0u8; 16];
    block[0] = 0x10; // predictor 1, shift 0
    block[2..].fill(0x77);
    write_adpcm_block(&mut s, 0x20, &block);
    s.voices[0].current_addr = 0x20;

    s.decode_next_block(0);

    assert!(
        s.voices[0].sample_buf[1] > i16::MAX as i32,
        "decoded ADPCM block should not clamp before interpolation"
    );
    assert!(
        s.voices[0].s_1 > i16::MAX as i32,
        "ADPCM filter history should match Redux's unclamped i32 path"
    );
}

#[test]
fn adpcm_flag_1_2_loops_back_to_loop_addr() {
    let mut s = Spu::new();
    s.voices[0].loop_addr = 0x100;
    s.voices[0].current_addr = 0x20;
    let mut block = [0u8; 16];
    block[1] = 0x3; // flag 1 (end) + flag 2 (repeat)
    write_adpcm_block(&mut s, 0x20, &block);
    s.decode_next_block(0);
    assert_eq!(s.voices[0].current_addr, 0x100);
}

#[test]
fn adpcm_end_flag_loops_only_on_exact_0x03_for_redux_parity() {
    let mut s = Spu::new();
    s.voices[0].loop_addr = 0x100;
    s.voices[0].current_addr = 0x20;
    let mut block = [0u8; 16];
    block[1] = 0x7; // Redux checks flags == 3, not merely bit 1 set.
    write_adpcm_block(&mut s, 0x20, &block);

    s.decode_next_block(0);

    assert_eq!(s.voices[0].current_addr, 0x30);
    assert!(s.voices[0].stop_after_block);
}

#[test]
fn adpcm_flag_1_alone_stops_voice() {
    let mut s = Spu::new();
    s.voices[0].current_addr = 0x40;
    s.voices[0].phase = AdsrPhase::Attack;
    let mut block = [0u8; 16];
    block[1] = 0x1; // flag 1 only
    write_adpcm_block(&mut s, 0x40, &block);
    s.decode_next_block(0);
    assert_eq!(s.voices[0].phase, AdsrPhase::Attack);
    assert!(s.voices[0].stop_after_block);
    assert_ne!(s.endx_latched & 1, 0);
}

#[test]
fn adpcm_stop_flag_turns_voice_off_after_final_block_is_consumed() {
    let mut s = Spu::new();
    s.voices[0].phase = AdsrPhase::Attack;
    s.voices[0].envelope = 0x7FFF;
    s.voices[0].sample_pos = 0x10000;
    s.voices[0].sample_index = ADPCM_SAMPLES_PER_BLOCK;
    s.voices[0].stop_after_block = true;

    let out = s.fetch_voice_sample(0);

    assert_eq!(out, 0);
    assert_eq!(s.voices[0].phase, AdsrPhase::Off);
    assert_eq!(s.voices[0].envelope, 0);
    assert!(!s.voices[0].stop_after_block);
}

#[test]
fn adpcm_flag_4_updates_loop_addr_when_unlocked() {
    let mut s = Spu::new();
    s.voices[0].current_addr = 0x80;
    let mut block = [0u8; 16];
    block[1] = 0x4; // flag 4 = loop-start
    write_adpcm_block(&mut s, 0x80, &block);
    s.decode_next_block(0);
    assert_eq!(s.voices[0].loop_addr, 0x80);
}

#[test]
fn adpcm_flag_4_ignored_when_software_locked_loop_addr() {
    let mut s = Spu::new();
    s.voices[0].current_addr = 0x80;
    s.voices[0].loop_addr = 0xAAA0;
    s.voices[0].loop_addr_locked = true;
    let mut block = [0u8; 16];
    block[1] = 0x4;
    write_adpcm_block(&mut s, 0x80, &block);
    s.decode_next_block(0);
    assert_eq!(s.voices[0].loop_addr, 0xAAA0);
}

// -- ADSR envelope --

#[test]
fn adsr_attack_linear_ramps_envelope_up() {
    let mut s = Spu::new();
    // Linear attack, rate=0 (fastest linear rate).
    s.voices[0].adsr.attack_rate = 0;
    s.voices[0].adsr.attack_exp = false;
    s.voices[0].phase = AdsrPhase::Attack;
    // After a single step, envelope should have risen from 0.
    s.voices[0].step_envelope();
    assert!(
        s.voices[0].envelope > 0,
        "env after 1 step: {}",
        s.voices[0].envelope
    );
}

#[test]
fn adsr_attack_saturates_and_transitions_to_decay() {
    let mut s = Spu::new();
    s.voices[0].adsr.attack_rate = 0;
    s.voices[0].adsr.attack_exp = false;
    s.voices[0].phase = AdsrPhase::Attack;
    // Force envelope near max and step once -- should transition.
    s.voices[0].envelope = 0x7FFE;
    s.voices[0].step_envelope();
    assert_eq!(s.voices[0].envelope, 0x7FFF);
    assert_eq!(s.voices[0].phase, AdsrPhase::Decay);
}

#[test]
fn adsr_decay_reaches_sustain_and_transitions() {
    let mut s = Spu::new();
    s.voices[0].adsr.decay_rate = 0;
    s.voices[0].adsr.sustain_level = 0;
    s.voices[0].adsr.release_exp = true;
    s.voices[0].phase = AdsrPhase::Decay;
    s.voices[0].envelope = 0x7FFF;
    for _ in 0..10000 {
        s.voices[0].step_envelope();
        if s.voices[0].phase == AdsrPhase::Sustain {
            break;
        }
    }
    assert_eq!(s.voices[0].phase, AdsrPhase::Sustain);
}

#[test]
fn adsr_decay_uses_release_mode_bit_for_redux_parity() {
    let mut linear = Voice::default();
    linear.adsr.decay_rate = 0;
    linear.adsr.release_exp = false;
    linear.phase = AdsrPhase::Decay;
    linear.envelope = 0x7000;
    linear.step_envelope();

    let mut exponential = linear.clone();
    exponential.adsr.release_exp = true;
    exponential.envelope = 0x7000;
    exponential.envelope_sub = 0;
    exponential.step_envelope();

    assert_eq!(linear.envelope, 0x7000 + envelope_numerator_decrease(0));
    assert_ne!(
        linear.envelope, exponential.envelope,
        "Redux decay switches formula based on ADSR release-mode bit"
    );
}

#[test]
fn adsr_release_linear_decays_to_zero_and_stops_voice() {
    let mut s = Spu::new();
    s.voices[0].adsr.release_rate = 0;
    s.voices[0].adsr.release_exp = false;
    s.voices[0].phase = AdsrPhase::Release;
    s.voices[0].envelope = 0x1000;
    for _ in 0..10000 {
        s.voices[0].step_envelope();
        if s.voices[0].phase == AdsrPhase::Off {
            break;
        }
    }
    assert_eq!(s.voices[0].phase, AdsrPhase::Off);
    assert_eq!(s.voices[0].envelope, 0);
}

#[test]
fn adsr_release_stops_on_underflow_not_exact_zero_for_redux_parity() {
    let mut voice = Voice::default();
    voice.adsr.release_rate = 0;
    voice.adsr.release_exp = false;
    voice.phase = AdsrPhase::Release;
    voice.envelope = -envelope_numerator_decrease(0);

    voice.step_envelope();
    assert_eq!(voice.envelope, 0);
    assert_eq!(voice.phase, AdsrPhase::Release);

    voice.step_envelope();
    assert_eq!(voice.envelope, 0);
    assert_eq!(voice.phase, AdsrPhase::Off);
}

#[test]
fn adsr_mix_uses_redux_ten_bit_volume() {
    assert_eq!(apply_adsr_volume(0x4000, 31), 0);
    assert_eq!(apply_adsr_volume(1023, 0x7FFF), 1023);
    assert_eq!(apply_adsr_volume(-1023, 0x7FFF), -1023);
}

// -- Output mixing --

#[test]
fn tick_sample_pushes_to_audio_queue() {
    let mut s = Spu::new();
    assert_eq!(s.audio_queue_len(), 0);
    s.tick_sample(SAMPLE_CYCLES);
    assert_eq!(s.audio_queue_len(), 1);
    assert_eq!(s.samples_produced(), 1);
}

#[test]
fn stress_test_many_voices_at_high_pitch_no_panic() {
    // Regression / stress test: exercises the voice advance
    // loop at maximum pitch (which triggers the block-
    // boundary fraction leak) across many voices and many
    // samples. Would have caught the Gaussian OOB before the
    // Crash 1 run did.
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.write16(SPUCNT, SPUCNT_UNMUTE);
    // Seed SPU RAM with a repeating ADPCM block so each voice
    // has something to decode.
    let mut block = [0u8; 16];
    block[0] = 0x0C; // shift=0x0C, filter=0
    block[1] = 0x02; // flag 2 = repeat
                     // Loop this one block for the first 0x1000 bytes of RAM.
    for base in (0..0x1000).step_by(16) {
        for (i, block_byte) in block.iter().enumerate() {
            let idx = (base + i) / 2;
            let byte = *block_byte as u16;
            if (base + i) & 1 == 0 {
                s.ram[idx] = (s.ram[idx] & 0xFF00) | byte;
            } else {
                s.ram[idx] = (s.ram[idx] & 0x00FF) | (byte << 8);
            }
        }
    }
    // Configure all 24 voices: max pitch, KON, loud volume.
    for v in 0..NUM_VOICES {
        let base = VOICE_BASE + (v as u32) * 16;
        s.write16(base + voice_offset::VOLUME_L, 0x3FFF);
        s.write16(base + voice_offset::VOLUME_R, 0x3FFF);
        s.write16(base + voice_offset::PITCH, 0x3FFF);
        s.write16(base + voice_offset::START_ADDR, 0);
        s.write16(base + voice_offset::ADSR_LO, 0x00FF);
        s.write16(base + voice_offset::ADSR_HI, 0x0000);
    }
    s.write16(KON_LO, 0xFFFF);
    s.write16(KON_HI, 0x00FF);
    // Tick through one NTSC frame's worth of samples. If any
    // voice's state goes out of bounds we'd panic here.
    for _ in 0..735 {
        s.tick_sample(SAMPLE_CYCLES);
    }
    // Output should contain 735 samples and no crashes.
    assert_eq!(s.samples_produced(), 735);
}

#[test]
fn silent_spu_outputs_zero() {
    let mut s = Spu::new();
    // 0x3FFF = max static volume (bits 0..=13 all set, bits
    // 14/15 clear → no phase-invert, no sweep).
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.tick_sample(SAMPLE_CYCLES);
    let out = s.drain_audio();
    assert_eq!(out, vec![(0, 0)]);
}

#[test]
fn silent_voice_contributes_zero_even_with_main_volume() {
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    for _ in 0..10 {
        s.tick_sample(SAMPLE_CYCLES);
    }
    let out = s.drain_audio();
    assert!(out.iter().all(|&(l, r)| l == 0 && r == 0));
}

#[test]
fn cd_audio_input_routes_through_cd_volume() {
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.write16(SPUCNT, SPUCNT_CD_AUDIO_ENABLE);
    s.write16(CD_VOL_L, 0x3FFF);
    s.write16(CD_VOL_R, 0x3FFF);
    // Push one stereo sample and tick.
    s.feed_cd_audio(&[(0x4000, 0x4000)]);
    s.tick_sample(SAMPLE_CYCLES);
    let out = s.drain_audio();
    // Main mix should be nonzero since CD input was nonzero.
    assert_eq!(out.len(), 1);
    let (l, r) = out[0];
    assert!(l > 0, "left should carry CD input: {l}");
    assert!(r > 0, "right should carry CD input: {r}");
}

#[test]
fn main_volume_writes_are_ignored_for_redux_mixer_parity() {
    let mut s = Spu::new();
    s.main_vol_l.write(0);
    s.main_vol_r.write(0);
    s.write16(SPUCNT, SPUCNT_CD_AUDIO_ENABLE);
    s.write16(CD_VOL_L, 0x7FFF);
    s.write16(CD_VOL_R, 0x7FFF);

    s.feed_cd_audio(&[(0x4000, 0x4000)]);
    s.tick_sample(SAMPLE_CYCLES);
    let (l, r) = s.drain_audio()[0];

    assert!(
        l > 0 && r > 0,
        "Redux keeps main-volume registers but does not scale the dry mix: {l}/{r}"
    );
}

#[test]
fn cd_audio_input_respects_spucnt_route_enable() {
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.write16(CD_VOL_L, 0x7FFF);
    s.write16(CD_VOL_R, 0x7FFF);

    s.feed_cd_audio(&[(0x4000, 0x4000)]);
    s.tick_sample(SAMPLE_CYCLES);
    let (mut l, mut r) = s.drain_audio()[0];
    assert_eq!(
        (l, r),
        (0, 0),
        "CD input must not route while SPUCNT bit 0 is clear"
    );

    s.write16(SPUCNT, SPUCNT_CD_AUDIO_ENABLE);
    s.feed_cd_audio(&[(0x4000, 0x4000)]);
    s.tick_sample(SAMPLE_CYCLES);
    (l, r) = s.drain_audio()[0];
    assert!(l > 0, "CD input should route when SPUCNT bit 0 is set: {l}");
    assert!(r > 0, "CD input should route when SPUCNT bit 0 is set: {r}");
}

#[test]
fn cd_audio_volume_is_signed_q15_not_voice_sweep_volume() {
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.write16(SPUCNT, SPUCNT_CD_AUDIO_ENABLE);
    s.write16(CD_VOL_L, 0x7FFF);
    s.write16(CD_VOL_R, 0x8000);

    s.feed_cd_audio(&[(0x4000, 0x4000)]);
    s.tick_sample(SAMPLE_CYCLES);
    let (l, r) = s.drain_audio()[0];

    assert!(l > 0, "0x7fff CD volume must be positive: {l}");
    assert!(r < 0, "0x8000 CD volume must be negative: {r}");
    assert!(
        l.unsigned_abs() > 0x3000,
        "0x7fff should be near unity, not half-scale/phase-inverted: {l}"
    );
}

fn write_reverb_cfg(s: &mut Spu, reg: usize, value: u16) {
    s.write16(REVERB_CFG_BASE + (reg as u32 * 2), value);
}

fn configure_passthrough_reverb(s: &mut Spu) {
    use reverb_reg::*;

    write_reverb_cfg(s, IIR_ALPHA, 0x7FFF);
    write_reverb_cfg(s, ACC_COEF_A, 0x7FFF);
    write_reverb_cfg(s, IN_COEF_L, 0x7FFF);
    write_reverb_cfg(s, IN_COEF_R, 0x7FFF);

    // Separate L/R and A/B destinations so the test fixture doesn't
    // stomp one channel with another while using a tiny synthetic
    // preset. Real games write full preset tables here.
    write_reverb_cfg(s, IIR_DEST_A0, 0);
    write_reverb_cfg(s, IIR_DEST_A1, 1);
    write_reverb_cfg(s, IIR_DEST_B0, 2);
    write_reverb_cfg(s, IIR_DEST_B1, 3);
    write_reverb_cfg(s, ACC_SRC_A0, 0);
    write_reverb_cfg(s, ACC_SRC_A1, 1);
    write_reverb_cfg(s, MIX_DEST_A0, 0);
    write_reverb_cfg(s, MIX_DEST_A1, 1);
    write_reverb_cfg(s, MIX_DEST_B0, 2);
    write_reverb_cfg(s, MIX_DEST_B1, 3);
}

#[test]
fn reverb_base_roundtrips_and_resets_work_cursor() {
    let mut s = Spu::new();
    s.write16(REVERB_BASE, 0x1000);
    assert_eq!(s.read16(REVERB_BASE), 0x1000);
    assert_eq!(s.reverb_base, 0x8000);
    assert_eq!(s.reverb.curr_addr, 0x4000);

    s.reverb.curr_addr = 0x5000;
    s.write16(REVERB_BASE, 0x1200);
    assert_eq!(s.read16(REVERB_BASE), 0x1200);
    assert_eq!(s.reverb.curr_addr, 0x4800);

    s.write16(REVERB_BASE, 0x0200);
    assert_eq!(s.read16(REVERB_BASE), 0x0200);
    assert_eq!(s.reverb_base, 0);
    assert_eq!(s.reverb.curr_addr, 0);
}

#[test]
fn reverb_address_wrap_below_base_matches_redux() {
    let mut s = Spu::new();
    s.write16(REVERB_BASE, 0x1000);
    s.reverb.curr_addr = s.reverb_base_halfword();

    assert_eq!(s.reverb_ram_index(-1, 0), 0x3FFFB);
}

#[test]
fn reverb_network_turns_bus_input_into_wet_output() {
    let mut s = Spu::new();
    s.write16(REVERB_BASE, 0x1000);
    s.write16(REVERB_VOL_L, 0x7FFF);
    s.write16(REVERB_VOL_R, 0x7FFF);
    s.write16(SPUCNT, SPUCNT_REVERB_MASTER_ENABLE);
    configure_passthrough_reverb(&mut s);

    let mut heard_wet = false;
    for _ in 0..8 {
        let (l, r) = s.mix_reverb(0x4000, 0x4000);
        heard_wet |= l != 0 || r != 0;
    }

    assert!(heard_wet, "reverb bus input never reached wet output");
}

#[test]
fn reverb_output_depth_uses_redux_q14_unity() {
    assert_eq!(Spu::scale_reverb_output(0x4000, 0x4000), 0x4000);
    assert_eq!(Spu::scale_reverb_output(0x4000, 0x3FFF), 0x3FFF);
    assert_eq!(Spu::scale_reverb_output(0x4000, 0xC000u16 as i16), -0x4000);
}

#[test]
fn reverb_hold_sample_matches_redux_left_right_asymmetry() {
    let mut s = Spu::new();
    s.write16(REVERB_BASE, 0x1000);
    s.reverb.process_this_sample = false;
    s.reverb.last_l = 10;
    s.reverb.wet_l = 30;
    s.reverb.last_r = 100;
    s.reverb.wet_r = 300;

    assert_eq!(s.mix_reverb(0, 0), (10, 300));
}

#[test]
fn eon_voice_reverb_mixes_after_main_volume() {
    let mut s = Spu::new();
    s.main_vol_l.write(0);
    s.main_vol_r.write(0);
    s.write16(REVERB_BASE, 0x1000);
    s.write16(REVERB_VOL_L, 0x7FFF);
    s.write16(REVERB_VOL_R, 0x7FFF);
    s.write16(SPUCNT, SPUCNT_UNMUTE | SPUCNT_REVERB_MASTER_ENABLE);
    s.write16(EON_LO, 0x0001);
    configure_passthrough_reverb(&mut s);

    s.voices[0].vol_l.write(0x3FFF);
    s.voices[0].vol_r.write(0x3FFF);
    s.voices[0].phase = AdsrPhase::Sustain;
    s.voices[0].envelope = 0x7FFF;
    s.voices[0].raw_pitch = 0x1000;
    s.voices[0].sample_buf = [0x4000; ADPCM_SAMPLES_PER_BLOCK];
    s.voices[0].sample_index = 0;
    s.voices[0].sample_pos = 0;
    s.voices[0].interp_ring = [0x4000; 4];

    for n in 0..12 {
        s.tick_sample(n * SAMPLE_CYCLES);
    }
    let out = s.drain_audio();

    assert!(
        out.iter().any(|&(l, r)| l != 0 || r != 0),
        "EON voice should be audible through reverb even with dry main volume at zero"
    );
}

#[test]
fn volume_envelope_static_mode_snaps_level_on_write() {
    let mut env = VolumeEnvelope::new();
    env.write(0x3FFF); // near-unity gain
    assert_eq!(env.current, 0x3FFF);
    env.write(0x4100); // Redux masks static negative phase to magnitude.
    assert_eq!(env.current, 0x0100);
}

#[test]
fn volume_envelope_sweep_mode_uses_redux_immediate_gain() {
    let mut env = VolumeEnvelope::new();
    env.write(0x8000);
    assert_eq!(env.current, 0);

    env.write(0x807F);
    assert_eq!(env.current, 0x3000);

    for _ in 0..10 {
        env.tick();
    }
    assert_eq!(env.current, 0x3000);
}

#[test]
fn volume_envelope_sweep_decrease_uses_redux_immediate_gain() {
    let mut env = VolumeEnvelope::new();
    env.write(0x8000 | (1 << 13) | 0x007F);
    assert_eq!(env.current, 0x1000);

    for _ in 0..10 {
        env.tick();
    }
    assert_eq!(env.current, 0x1000);
}

#[test]
fn volume_envelope_static_tick_is_noop() {
    let mut env = VolumeEnvelope::new();
    env.write(0x2000);
    env.tick();
    assert_eq!(env.current, 0x2000);
}

#[test]
fn gaussian_interp_of_silence_is_silence() {
    let out = gauss_interpolate([0, 0, 0, 0], 0);
    assert_eq!(out, 0);
}

#[test]
fn gaussian_interp_nonzero_input_produces_output() {
    // All four samples at max positive -- output should be non-
    // zero and in range.
    let out = gauss_interpolate([0x7FFF, 0x7FFF, 0x7FFF, 0x7FFF], 0x800);
    assert!(out > 0);
}

#[test]
fn gaussian_interp_handles_frac_past_0x10000() {
    // Defensive clamp: the caller keeps the remainder below one
    // source sample, but out-of-range values still shouldn't
    // index past the end of the coefficient table.
    for frac in [0x10000, 0x10004, 0x1FFFF, 0xFFFF_FFFF] {
        let _ = gauss_interpolate([0, 0, 0, 0], frac);
        let _ = gauss_interpolate([0x1234, 0x5678, -0x100, 0x7FFF], frac);
    }
}

#[test]
fn interpolation_ring_preserves_previous_block_tail() {
    let mut voice = Voice::default();
    voice.push_interpolation_sample(10);
    voice.push_interpolation_sample(20);
    voice.push_interpolation_sample(30);
    voice.push_interpolation_sample(40);
    voice.push_interpolation_sample(50);
    assert_eq!(voice.interpolation_window(), [20, 30, 40, 50]);
}

#[test]
fn xa_decoder_silent_block_stays_silent() {
    let mut state = XaDecoderState::new();
    let data = [0u16; 14];
    let mut out = [0i16; 28];
    xa_decode_block(&mut state, 0x00, &data, &mut out, 1);
    assert!(out.iter().all(|&s| s == 0));
}

#[test]
fn xa_decoder_nonzero_block_produces_output() {
    let mut state = XaDecoderState::new();
    let mut data = [0u16; 14];
    // Fill with non-zero pattern to exercise the filter.
    for (i, w) in data.iter_mut().enumerate() {
        *w = (i as u16) * 0x1234;
    }
    let mut out = [0i16; 28];
    xa_decode_block(&mut state, 0x01, &data, &mut out, 1);
    assert!(
        out.iter().any(|&s| s != 0),
        "some samples should be nonzero"
    );
}

// -- Volume register decoding --

#[test]
fn volume_envelope_write_static_level() {
    let mut env = VolumeEnvelope::new();
    env.write(0x3FFF);
    assert_eq!(env.current, 0x3FFF);
    env.write(0x4100);
    assert_eq!(env.current, 0x0100);
}

// -- Output buffer cap --

#[test]
fn audio_queue_caps_at_max() {
    let mut s = Spu::new();
    for _ in 0..(OUTPUT_BUFFER_CAP + 100) {
        s.tick_sample(SAMPLE_CYCLES);
    }
    assert!(s.audio_queue_len() <= OUTPUT_BUFFER_CAP);
}

// -- Noise generator (Dr. Hell algorithm) --

#[test]
fn noise_seed_is_one() {
    // The LFSR feedback table NoiseWaveAdd[0] = 1, so a zero
    // seed would still flip the low bit on first step. But
    // hardware/Redux start at 1 -- keep the same so traces
    // line up if/when we wire SPU into the parity oracle.
    let s = Spu::new();
    assert_eq!(s.noise_val, 1);
}

#[test]
fn noise_advances_when_clock_set() {
    // noise_clock = (spucnt >> 8) & 0x3F. clock>>2 = bits 13:10
    // of spucnt. Set those four bits to 0xF for the fastest
    // shift rate: threshold = (0x8000 >> 15) << 16 = 0x10000.
    // Per-sample increment is 0x10000 + NOISE_FREQ_ADD[step],
    // so the LFSR shifts at least once per tick.
    let mut s = Spu::new();
    s.write16(SPUCNT, 0x3C00);
    let v0 = s.noise_val;
    s.noise_tick();
    assert_ne!(s.noise_val, v0, "noise should shift at fastest rate");
}

#[test]
fn noise_period_grows_with_shift() {
    // At shift=0 the LFSR shifts roughly once every 0x8000
    // counter-units (0x8000 / 0x10000 per sample → many samples).
    // Verify it does NOT shift in a single tick at slow rate.
    let mut s = Spu::new();
    s.write16(SPUCNT, 0x0000); // shift = 0
    let v0 = s.noise_val;
    s.noise_tick();
    // Single tick adds 0x10000 < 0x8000_0000 -- no shift.
    assert_eq!(s.noise_val, v0);
}

// -- FMod / pitch modulation --

#[test]
fn fmod_modulator_voice_suppressed_from_lr_mix() {
    // Voice 0 = modulator (its sample feeds voice 1's pitch).
    // Voice 1 = modulated. Both are configured to emit a known
    // non-zero sample; only voice 1 should reach the audible mix.
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.write16(SPUCNT, SPUCNT_UNMUTE);

    // Mark voice 1 as pitch-modulated by voice 0.
    s.write16(PMON_LO, 0x0002);

    // Configure both voices: full envelope, full volume,
    // last_sample seeded directly so we don't depend on ADPCM.
    for v in 0..2 {
        let base = VOICE_BASE + (v as u32) * 16;
        s.write16(base + voice_offset::VOLUME_L, 0x3FFF);
        s.write16(base + voice_offset::VOLUME_R, 0x3FFF);
        s.voices[v].phase = AdsrPhase::Sustain;
        s.voices[v].envelope = 0x7FFF;
        s.voices[v].last_sample = 0x4000;
        // Block decode of zeros -- voice mixes its envelope * sample.
        s.voices[v].sample_buf = [0x4000; ADPCM_SAMPLES_PER_BLOCK];
        s.voices[v].sample_index = 0;
    }

    s.tick_sample(SAMPLE_CYCLES);
    let (l, r) = s.drain_audio()[0];

    // Voice 0 (modulator) should NOT contribute. Voice 1 alone
    // would produce one full-scale sample's worth of output.
    // Bound it: total must be < 2× single-voice level.
    let voice_only = (0x4000_i32 * 0x3FFF) >> 14;
    assert!(
        (l as i32) < voice_only * 3 / 2,
        "voice 0 leaked into L: l={l}"
    );
    assert!(
        (r as i32) < voice_only * 3 / 2,
        "voice 0 leaked into R: r={r}"
    );
    // And greater than zero -- voice 1 still played.
    assert!(l > 0);
    assert!(r > 0);
}

#[test]
fn spucnt_mute_zeroes_voice_sample_history() {
    let mut s = Spu::new();
    s.main_vol_l.write(0x3FFF);
    s.main_vol_r.write(0x3FFF);
    s.voices[0].phase = AdsrPhase::Sustain;
    s.voices[0].envelope = 0x7FFF;
    s.voices[0].sample_buf = [0x4000; ADPCM_SAMPLES_PER_BLOCK];
    s.voices[0].sample_index = 0;

    s.tick_sample(SAMPLE_CYCLES);
    let (l, r) = s.drain_audio()[0];

    assert_eq!((l, r), (0, 0));
    assert_eq!(s.voices[0].interpolation_window(), [0, 0, 0, 0]);
}

#[test]
fn fmod_modulator_still_updates_last_sample() {
    // Even though voice 0's L/R is suppressed, its last_sample
    // must still update so voice 1's FMod reads the right value.
    let mut s = Spu::new();
    s.write16(PMON_LO, 0x0002);
    s.voices[0].phase = AdsrPhase::Sustain;
    s.voices[0].envelope = 0x7FFF;
    s.voices[0].sample_buf = [0x1234; ADPCM_SAMPLES_PER_BLOCK];
    s.voices[0].sample_index = 0;
    s.voices[1].raw_pitch = 0x1000;
    s.voices[1].phase = AdsrPhase::Sustain;
    s.voices[1].envelope = 0x7FFF;

    s.tick_sample(SAMPLE_CYCLES);
    // last_sample for voice 0 should be approximately envelope *
    // sample (saturated), not zero.
    assert!(
        s.voices[0].last_sample != 0,
        "modulator's last_sample was zeroed"
    );
}

#[test]
fn noise_value_substitutes_for_voice_sample() {
    // Voice 5 is in noise mode; fetch_voice_sample returns
    // noise_val unchanged when the voice is active.
    let mut s = Spu::new();
    s.noise_on = 1 << 5;
    s.noise_val = 0x1234;
    s.voices[5].phase = AdsrPhase::Attack;
    let out = s.fetch_voice_sample(5);
    assert_eq!(out, 0x1234);
}

#[test]
fn off_noise_voice_stays_silent() {
    let mut s = Spu::new();
    s.noise_on = 1 << 5;
    s.noise_val = 0x1234;
    s.voices[5].phase = AdsrPhase::Off;
    let out = s.fetch_voice_sample(5);
    assert_eq!(out, 0);
}
