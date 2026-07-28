use super::*;

fn raw_sector(header: [u8; 4], subheader: [u8; 4], payload_fill: u8) -> Vec<u8> {
    let mut raw = vec![0u8; psx_iso::SECTOR_BYTES];
    raw[12..16].copy_from_slice(&header);
    raw[16..20].copy_from_slice(&subheader);
    raw[20..24].copy_from_slice(&subheader);
    let payload_start = if header[3] == 1 { 16 } else { 24 };
    raw[payload_start..payload_start + 2048].fill(payload_fill);
    raw
}

/// Play `track` and wait out the seek, leaving the drive actually producing
/// audio. `cmd_play` on its own only starts the head moving: see
/// [`super::timing::cdda_seek_cycles`].
fn play_and_arrive(cd: &mut CdRom, track: u8) {
    cd.cmd_play(&[track]);
    let arrives = cd.cdda_seek_done_at.expect("Play arms a seek");
    cd.tick(arrives + 1);
}

fn multitrack_disc_with_pregap() -> Disc {
    Disc::from_tracks(vec![
        psx_iso::Track {
            number: 1,
            track_type: psx_iso::TrackType::Data,
            start_lba: 0,
            sector_count: 10,
            pregap: 0,
            file_pregap: 0,
            bytes: vec![0u8; psx_iso::SECTOR_BYTES * 10],
        },
        psx_iso::Track {
            number: 2,
            track_type: psx_iso::TrackType::Audio,
            start_lba: 12,
            sector_count: 4,
            pregap: 2,
            file_pregap: 0,
            bytes: vec![0u8; psx_iso::SECTOR_BYTES * 4],
        },
    ])
}

fn cdda_disc() -> Disc {
    let mut audio = vec![0u8; psx_iso::SECTOR_BYTES * 2];
    for sample in 0..(CDDA_SAMPLES_PER_SECTOR * 2) {
        let left = 1000i16.saturating_add(sample as i16);
        let right = (-1000i16).saturating_sub(sample as i16);
        let off = sample * CDDA_BYTES_PER_SAMPLE;
        audio[off..off + 2].copy_from_slice(&left.to_le_bytes());
        audio[off + 2..off + 4].copy_from_slice(&right.to_le_bytes());
    }

    Disc::from_tracks(vec![
        psx_iso::Track {
            number: 1,
            track_type: psx_iso::TrackType::Data,
            start_lba: 0,
            sector_count: 10,
            pregap: 0,
            file_pregap: 0,
            bytes: vec![0u8; psx_iso::SECTOR_BYTES * 10],
        },
        psx_iso::Track {
            number: 2,
            track_type: psx_iso::TrackType::Audio,
            start_lba: 12,
            sector_count: 2,
            pregap: 2,
            file_pregap: 0,
            bytes: audio,
        },
    ])
}

#[test]
fn contains_covers_4_bytes() {
    for off in 0..4 {
        assert!(CdRom::contains(BASE + off));
    }
    assert!(!CdRom::contains(BASE - 1));
    assert!(!CdRom::contains(BASE + 4));
}

#[test]
fn index_write_is_masked() {
    let mut cd = CdRom::new();
    cd.write8(BASE, 0xFF);
    // The status byte readback has index in low 2 bits.
    assert_eq!(cd.read8(BASE) & 0x3, 3);
}

#[test]
fn parameter_fifo_roundtrips() {
    let mut cd = CdRom::new();
    cd.write8(BASE + 2, 0xAB);
    cd.write8(BASE + 2, 0xCD);
    assert_eq!(cd.params.len(), 2);
    // FIFO not-empty bit cleared, not-full bit still set.
    let s = cd.read8(BASE);
    assert_eq!(s & status_bit::PARAM_FIFO_EMPTY, 0);
    assert!(s & status_bit::PARAM_FIFO_NOT_FULL != 0);
}

#[test]
fn response_fifo_pop_returns_pushed_bytes() {
    let mut cd = CdRom::new();
    cd.responses.push_back(0x11);
    cd.responses.push_back(0x22);
    assert_eq!(cd.read8(BASE + 1), 0x11);
    assert_eq!(cd.read8(BASE + 1), 0x22);
    // Empty pop reads as zero -- matches Redux.
    assert_eq!(cd.read8(BASE + 1), 0);
}

#[test]
fn data_fifo_read_without_transfer_request_returns_zero_and_keeps_buffered_sector() {
    let mut cd = CdRom::new();
    cd.data_fifo.extend([0x12, 0x34]);
    cd.data_fifo_ready = true;

    assert_ne!(cd.read8(BASE) & status_bit::DATA_FIFO_NOT_EMPTY, 0);
    assert_eq!(cd.read8(BASE + 2), 0);
    assert_eq!(cd.data_fifo_len(), 2);
    assert_eq!(cd.data_fifo.front().copied(), Some(0x12));
    assert_eq!(cd.read8(BASE) & status_bit::DATA_FIFO_NOT_EMPTY, 0);
}

#[test]
fn request_register_bit7_arms_transfer_until_sector_buffer_drains() {
    let mut cd = CdRom::new();
    cd.data_fifo.extend([0x12, 0x34]);
    cd.data_fifo_ready = true;

    cd.write8(BASE + 3, 0x80);
    assert_eq!(cd.read8(BASE + 2), 0x12);
    assert_ne!(cd.read8(BASE) & status_bit::DATA_FIFO_NOT_EMPTY, 0);
    assert_eq!(cd.read8(BASE + 2), 0x34);
    assert_eq!(cd.read8(BASE) & status_bit::DATA_FIFO_NOT_EMPTY, 0);
    assert!(!cd.data_transfer_active);
    assert_eq!(cd.read8(BASE + 2), 0);
}

#[test]
fn irq_mask_write_reports_wake_for_latched_unmasked_irq() {
    let mut cd = CdRom::new();
    cd.irq_flag = IrqType::DataReady as u8;
    cd.irq_mask = 0;
    cd.index = 1;

    assert!(!cd.write8_at(BASE + 2, 0, 0));
    assert!(cd.write8_at(BASE + 2, 1, 0));
}

#[test]
fn new_command_discards_unread_response_and_sets_busy_bit() {
    let mut cd = CdRom::new();
    cd.responses.push_back(0x55);

    cd.queue_command(0x01, 123);

    assert!(
        cd.responses.is_empty(),
        "new command should replace old packet"
    );
    assert_ne!(
        cd.read8(BASE) & status_bit::TRANSMISSION_BUSY,
        0,
        "status bit 7 should latch while the command is pending"
    );
}

#[test]
fn delivered_irq_packet_replaces_stale_bytes_and_clears_busy_bit() {
    let mut cd = CdRom::new();
    cd.command_busy = true;
    cd.responses.push_back(0x11);
    cd.insert_pending_event(PendingEvent {
        command: 0x01,
        deadline: 100,
        irq: IrqType::Acknowledge,
        bytes: vec![0xAA, 0xBB],
        followup: None,
    });

    assert!(cd.tick(101));
    assert_eq!(cd.read8(BASE + 1), 0xAA);
    assert_eq!(cd.read8(BASE + 1), 0xBB);
    assert_eq!(cd.read8(BASE + 1), 0);
    assert_eq!(cd.read8(BASE) & status_bit::TRANSMISSION_BUSY, 0);
}

#[test]
fn irq_ack_clears_only_written_bits() {
    let mut cd = CdRom::new();
    cd.irq_flag = 0x1F;
    // Select index 1 then write-1-to-clear bit 2.
    cd.write8(BASE, 1);
    cd.write8(BASE + 3, 0x04);
    assert_eq!(cd.irq_flag, 0x1B);
}

#[test]
fn cold_boot_is_closed_lid_no_disc() {
    // Cold boot state for "Please insert disc" path: lid closed
    // (SHELL_OPEN cleared) so Init succeeds, motor off until
    // Init runs. No disc (GetID returns the no-disc response).
    let cd = CdRom::new();
    assert_eq!(cd.drive_status & drive_status_bit::SHELL_OPEN, 0);
    assert!(!cd.motor_on);
    assert!(!cd.disc_present);
}

#[test]
fn init_only_queues_ack_and_latches_shell_open() {
    let mut cd = CdRom::new();
    cd.scheduling_cycle = 1_000;

    cd.cmd_init();

    assert_eq!(cd.pending.len(), 1);
    let ack = cd.pending.front().expect("Init ACK pending");
    assert_eq!(ack.irq, IrqType::Acknowledge);
    assert!(ack.followup.is_none(), "Init should not invent INT2");
    assert!(cd.motor_on, "Init should spin the motor up");
    assert_eq!(
        cd.drive_status & drive_status_bit::SHELL_OPEN,
        drive_status_bit::SHELL_OPEN
    );
    assert!(cd.seek_done, "Init resets the seek latch to DONE");
}

#[test]
fn init_ack_bootstraps_lid_rescan_state_machine() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES])));
    cd.scheduling_cycle = 1_000;

    cd.cmd_init();
    assert!(cd.lid_bootstrap_pending);
    assert_eq!(cd.drive_state, DriveState::RescanCd);

    let ack_cycle = 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1;
    assert!(cd.tick(ack_cycle));
    assert!(!cd.lid_bootstrap_pending);
    assert_eq!(cd.lid_deadline, Some(ack_cycle + LID_BOOTSTRAP_CYCLES));

    let rescan_cycle = ack_cycle + LID_BOOTSTRAP_CYCLES + 1;
    assert!(!cd.tick(rescan_cycle));
    assert_eq!(cd.drive_state, DriveState::PrepareCd);
    assert_eq!(
        cd.lid_deadline,
        Some(rescan_cycle + LID_PREPARE_SPINUP_CYCLES)
    );

    let prepare_cycle = rescan_cycle + LID_PREPARE_SPINUP_CYCLES + 1;
    assert!(!cd.tick(prepare_cycle));
    assert_eq!(cd.drive_state, DriveState::Standby);
    assert_ne!(cd.drive_status & drive_status_bit::SEEKING, 0);

    let settle_cycle = prepare_cycle + LID_PREPARE_SEEK_CYCLES + 1;
    assert!(!cd.tick(settle_cycle));
    assert_eq!(cd.drive_state, DriveState::Standby);
    assert_eq!(cd.drive_status & drive_status_bit::SEEKING, 0);
}

#[test]
fn getstat_reports_then_clears_shell_open_sticky_bit() {
    let mut cd = CdRom::new();
    cd.drive_status |= drive_status_bit::SHELL_OPEN;
    cd.scheduling_cycle = 1_000;

    cd.cmd_getstat();

    assert_eq!(cd.drive_status & drive_status_bit::SHELL_OPEN, 0);
    assert!(cd.tick(1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1));
    let stat = cd.read8(BASE + 1);
    assert_eq!(
        stat & drive_status_bit::SHELL_OPEN,
        drive_status_bit::SHELL_OPEN
    );
    assert_eq!(cd.read8(BASE + 1), 0);
}

#[test]
fn mounted_getstat_fifth_request_crosses_maintenance_sweep() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0; psx_iso::SECTOR_BYTES])));

    for request in 1..=5 {
        cd.pending.clear();
        cd.scheduling_cycle = request * 100_000;
        cd.cmd_getstat();
        let deadline = cd.pending.front().expect("GetStat response").deadline;
        let extra = if request == 5 {
            GETSTAT_MAINTENANCE_CYCLES
        } else {
            0
        };
        assert_eq!(
            deadline,
            request * 100_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + extra
        );
    }
}

#[test]
fn getlocl_without_disc_returns_error() {
    let mut cd = CdRom::new();
    // No disc.
    cd.cmd_get_loc_l();
    // First tick rebases the pending event's relative deadline
    // to absolute; second tick past the deadline delivers it.
    cd.tick(0);
    cd.tick(10_000_000);
    let first = cd.responses.front().copied().unwrap_or(0);
    assert_ne!(first & drive_status_bit::ERROR, 0);
}

#[test]
fn getlocl_without_prior_sector_returns_error_even_with_disc() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES])));

    cd.cmd_get_loc_l();
    cd.tick(0);
    cd.tick(10_000_000);

    let first = cd.read8(BASE + 1);
    assert_ne!(first & drive_status_bit::ERROR, 0);
    assert_eq!(
        cd.read8(BASE + 1),
        0,
        "invalid-header error should be a 1-byte reply"
    );
}

#[test]
fn getlocl_returns_last_sector_header_and_subheader() {
    let mut cd = CdRom::new();
    let header = [0x12, 0x34, 0x56, 0x02];
    let subheader = [0xAA, 0xBB, 0xCC, 0xDD];
    cd.insert_disc(Some(Disc::from_bin(raw_sector(header, subheader, 0x6C))));

    cd.load_next_sector();
    assert_eq!(cd.data_fifo_len(), 2048);
    assert_eq!(cd.data_fifo.front().copied(), Some(0x6C));

    cd.cmd_get_loc_l();
    cd.tick(0);
    cd.tick(10_000_000);

    for expected in header.into_iter().chain(subheader) {
        assert_eq!(cd.read8(BASE + 1), expected);
    }
}

#[test]
fn load_next_sector_whole_sector_mode_returns_2340_bytes() {
    let mut cd = CdRom::new();
    let header = [0x01, 0x02, 0x03, 0x02];
    let subheader = [0x10, 0x20, 0x30, 0x40];
    let raw = raw_sector(header, subheader, 0xAB);
    cd.mode = 0x20;
    cd.insert_disc(Some(Disc::from_bin(raw.clone())));

    cd.load_next_sector();

    assert_eq!(cd.data_fifo_len(), 2340);
    let first_bytes: Vec<u8> = cd.data_fifo.iter().take(12).copied().collect();
    assert_eq!(first_bytes, raw[12..24].to_vec());
}

#[test]
fn load_next_sector_whole_sector_mode1_keeps_full_raw_payload() {
    let mut cd = CdRom::new();
    let mut raw = raw_sector([0x00, 0x02, 0x00, 0x01], [0; 4], 0x5D);
    raw[2064..2352].fill(0xE7);
    cd.mode = 0x20;
    cd.insert_disc(Some(Disc::from_bin(raw.clone())));

    cd.load_next_sector();

    assert_eq!(cd.data_fifo_len(), 2340);
    let bytes: Vec<u8> = cd.data_fifo.iter().copied().collect();
    assert_eq!(bytes, raw[12..12 + 2340].to_vec());
}

#[test]
fn load_next_sector_mode1_uses_payload_after_header() {
    let mut cd = CdRom::new();
    let mut raw = raw_sector([0x00, 0x02, 0x00, 0x01], [0; 4], 0x00);
    raw[16..16 + 2048].fill(0x5D);
    raw[24..24 + 2048].fill(0xA7);
    cd.insert_disc(Some(Disc::from_bin(raw)));

    cd.load_next_sector();

    assert_eq!(cd.data_fifo_len(), 2048);
    assert_eq!(cd.data_fifo.front().copied(), Some(0x5D));
}

#[test]
fn load_next_sector_skips_audio_track_during_data_read() {
    // A data read (ReadN/ReadS) that advances into a Red Book audio track
    // delivers no data and raises no DataReady.
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));

    cd.read_lba = 12; // track 2 (audio) INDEX 01
    let raise = cd.load_next_sector();
    assert!(!raise, "audio-track sector must not raise DataReady");
    assert_eq!(cd.data_fifo_len(), 0, "audio-track sector delivers no data");

    // Control: a data-track sector still delivers its 2048-byte payload.
    cd.read_lba = 0; // track 1 (data)
    let raise = cd.load_next_sector();
    assert!(raise, "data-track sector must raise DataReady");
    assert_eq!(cd.data_fifo_len(), 2048);
}

#[test]
fn load_next_sector_sets_ready_latch_but_leaves_transfer_disarmed() {
    let mut cd = CdRom::new();
    let raw = raw_sector([0x00, 0x02, 0x00, 0x02], [0; 4], 0x6C);
    cd.insert_disc(Some(Disc::from_bin(raw)));

    cd.load_next_sector();

    assert!(cd.data_fifo_ready);
    assert!(!cd.data_transfer_active);
    assert_ne!(cd.read8(BASE) & status_bit::DATA_FIFO_NOT_EMPTY, 0);
    assert_eq!(cd.pop_data_byte(), 0);
    assert_eq!(cd.data_fifo_len(), 2048);
    assert_eq!(cd.data_fifo.front().copied(), Some(0x6C));
}

#[test]
fn sector_read_cycles_match_redux_initial_and_stream_cadence() {
    let mut cd = CdRom::new();
    // Default mode = 0x80 → double-speed.
    assert_eq!(cd.initial_sector_read_cycles(), CD_READ_TIME);
    assert_eq!(cd.sector_read_cycles(), CD_READ_TIME / 2);
    // Flipping bit 7 off via SetMode gives single-speed (2×).
    cd.cmd_setmode(&[0x00]);
    assert_eq!(cd.initial_sector_read_cycles(), CD_READ_TIME * 2);
    assert_eq!(cd.sector_read_cycles(), CD_READ_TIME);
    // Setting other bits without bit 7 stays single-speed.
    cd.cmd_setmode(&[0x60]);
    assert_eq!(cd.initial_sector_read_cycles(), CD_READ_TIME * 2);
    assert_eq!(cd.sector_read_cycles(), CD_READ_TIME);
    // Back to double-speed when bit 7 returns.
    cd.cmd_setmode(&[0x80]);
    assert_eq!(cd.initial_sector_read_cycles(), CD_READ_TIME);
    assert_eq!(cd.sector_read_cycles(), CD_READ_TIME / 2);
}

#[test]
fn read_command_uses_initial_delay_then_steady_stream_delay() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 4])));
    cd.scheduling_cycle = 1_000;

    cd.cmd_read();
    assert_eq!(cd.pending.len(), 1); // command ACK only; first DataReady is chained off it
    assert_eq!(
        cd.pending.front().map(|ev| ev.irq),
        Some(IrqType::Acknowledge)
    );

    assert!(cd.tick(1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1));
    let first_data_ready = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::DataReady)
        .expect("first DataReady scheduled from ACK fire time");
    assert_eq!(
        first_data_ready.deadline,
        1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1 + CD_READ_TIME
    );
    cd.irq_flag = 0;
    cd.responses.clear();

    assert!(cd.tick(1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1 + CD_READ_TIME + 1));
    let next_data_ready = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::DataReady)
        .expect("steady DataReady scheduled");
    assert_eq!(
        next_data_ready.deadline,
        1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1 + CD_READ_TIME + 1 + CD_READ_TIME / 2
    );
}

/// A handler that has not run yet does not slow the disc down. The sector is
/// delivered on schedule with the CPU's CDROM interrupt line still pending,
/// because the platter is turning either way.
#[test]
fn dataready_arrives_on_schedule_even_with_the_cpu_irq_still_pending() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 4])));
    cd.scheduling_cycle = 1_000;

    cd.cmd_read();
    let ack_cycle = 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1;
    assert!(cd.tick(ack_cycle));
    cd.irq_flag = 0;
    cd.responses.clear();

    let first_due = ack_cycle + CD_READ_TIME + 1;
    assert!(cd.tick_with_irq_pending(first_due, true));
    assert_eq!(cd.irq_flag, IrqType::DataReady as u8);
}

/// Software that never acknowledges keeps getting sectors read at it, and
/// once the ring is full the oldest are lost. This is the failure the whole
/// model exists to expose: nothing stalls, nothing errors, the stream just
/// quietly develops a hole.
#[test]
fn a_handler_that_never_runs_loses_sectors_rather_than_stalling_the_drive() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 64])));
    cd.scheduling_cycle = 1_000;

    cd.cmd_read();
    let ack_cycle = 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1;
    assert!(cd.tick(ack_cycle));
    cd.irq_flag = 0;
    cd.responses.clear();

    // Never acknowledge and never read a byte, so the interrupt line stays
    // held and the notifications stop, while the disc keeps turning.
    let mut at = ack_cycle;
    for _ in 0..(SECTOR_BUFFERS * 3) {
        at += CD_READ_TIME + 1;
        cd.tick_with_irq_pending(at, true);
    }

    assert!(cd.reading, "the drive keeps reading regardless");
    assert!(
        cd.dropped_sectors > 0,
        "sectors should have been lost, none were"
    );
    assert!(
        cd.waiting_sectors.len() <= SECTOR_BUFFERS - 1,
        "the ring must stay bounded, held {}",
        cd.waiting_sectors.len()
    );
}

#[test]
fn xa_audio_sector_suppresses_dataready_irq_but_keeps_streaming() {
    let mut cd = CdRom::new();
    let xa_sector = raw_sector([0x00, 0x02, 0x00, 0x02], [0x07, 0x02, 0x24, 0x01], 0);
    let data_sector = raw_sector([0x00, 0x02, 0x01, 0x02], [0x07, 0x02, 0x00, 0x00], 0x5A);
    let mut disc = xa_sector;
    disc.extend(data_sector);
    cd.insert_disc(Some(Disc::from_bin(disc)));
    cd.mode = 0xC0; // double-speed + STRSND/XA enable
    cd.setloc_msf = (0x00, 0x02, 0x00); // LBA 0
    cd.scheduling_cycle = 1_000;

    cd.cmd_read();
    assert!(cd.tick(1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1));
    cd.irq_flag = 0;
    cd.responses.clear();

    assert!(
        !cd.tick(1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1 + CD_READ_TIME + 1),
        "Redux suppresses DataReady IRQs for STRSND XA audio sectors"
    );
    assert_eq!(cd.irq_flag, 0);
    assert!(cd.responses.is_empty());
    assert!(
        cd.cd_audio_queue_len() > 0,
        "XA sector should still feed decoded samples to the SPU"
    );
    assert_eq!(cd.irq_type_counts[IrqType::DataReady as usize], 0);
    let next_data_ready = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::DataReady)
        .expect("suppressed audio sector should still chain the read stream");
    assert_eq!(
        next_data_ready.deadline,
        1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + 1 + CD_READ_TIME + 1 + CD_READ_TIME / 2
    );
}

#[test]
fn pause_on_spun_up_drive_uses_short_followup_delay() {
    let mut cd = CdRom::new();
    cd.motor_on = true;
    cd.reading = true;
    cd.scheduling_cycle = 1_000;
    cd.insert_pending_event(PendingEvent {
        command: 0x06,
        deadline: 50_000,
        irq: IrqType::DataReady,
        bytes: vec![0x20],
        followup: None,
    });

    cd.cmd_pause();
    assert_eq!(
        cd.pending.len(),
        1,
        "Pause should cancel the in-flight read chain"
    );

    let ack_deadline = 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES;
    assert!(cd.tick(ack_deadline + 1));
    assert_eq!(
        cd.read8(BASE + 1),
        drive_status_bit::MOTOR_ON,
        "Pause ACK should report the stopped read state"
    );
    cd.irq_flag = 0;
    let pause_complete = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::Complete)
        .expect("pause completion chained off ACK");
    assert_eq!(
        pause_complete.deadline,
        ack_deadline + 1 + PAUSE_COMPLETE_CYCLES_STANDBY
    );
}

#[test]
fn seek_command_uses_short_followup_after_drive_has_seeked_once() {
    let mut cd = CdRom::new();
    cd.seek_done = true;
    cd.scheduling_cycle = 1_000;

    cd.cmd_seek();

    let ack_deadline = 1_000 + FIRST_RESPONSE_CYCLES;
    assert!(cd.tick(ack_deadline + 1));
    let seek_complete = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::Complete)
        .expect("seek completion chained off ACK");
    assert_eq!(seek_complete.deadline, ack_deadline + 1 + 0x800);
}

#[test]
fn setloc_far_target_clears_seek_done_and_marks_pending() {
    let mut cd = CdRom::new();
    cd.seek_done = true;
    cd.read_lba = 200;

    cd.cmd_setloc(&[0x00, 0x02, 0x16]);

    assert!(
        !cd.seek_done,
        "far SetLoc should force the next seek slow-path"
    );
    assert!(
        cd.setloc_pending,
        "SetLoc should latch until Read/Play consumes it"
    );
}

#[test]
fn read_command_cancels_old_stream_and_rearms_first_sector() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 32])));
    cd.reading = true;
    cd.scheduling_cycle = 1_000;
    cd.setloc_msf = (0x00, 0x02, 0x16);
    cd.setloc_pending = true;
    cd.insert_pending_event(PendingEvent {
        command: 0x06,
        deadline: 50_000,
        irq: IrqType::DataReady,
        bytes: vec![0x20],
        followup: None,
    });

    cd.cmd_read();

    assert!(
        cd.pending.iter().all(|ev| ev.irq != IrqType::DataReady),
        "stale DataReady events from the previous stream must be cancelled"
    );
    let ack = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::Acknowledge)
        .expect("ReadN ACK present");
    let followup = ack.followup.as_ref().expect("first sector chained off ACK");
    assert_eq!(followup.irq, IrqType::DataReady);
    assert_eq!(followup.delay, cd.initial_sector_read_cycles());
}

#[test]
fn relocated_read_stretches_second_sector_gap() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 64])));
    cd.scheduling_cycle = 1_000;
    cd.setloc_msf = (0x00, 0x02, 0x16);
    cd.setloc_pending = true;

    cd.cmd_read();

    let ack_deadline = 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES;
    assert!(cd.tick(ack_deadline + 1));
    cd.irq_flag = 0;

    let first_sector_deadline = ack_deadline + 1 + CD_READ_TIME;
    assert!(cd.tick(first_sector_deadline + 1));
    let next_sector = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::DataReady)
        .expect("read stream should continue after first sector");
    assert_eq!(
        next_sector.deadline,
        first_sector_deadline + 1 + (CD_READ_TIME / 2) * 30
    );
    assert!(
        !cd.location_changed,
        "the long-gap latch should clear once it has stretched one sector"
    );
}

#[test]
fn gettn_single_track_disc_reports_one_to_one() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 10])));

    cd.cmd_get_tn();
    cd.tick(10_000_000);

    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0x01);
    assert_eq!(cd.read8(BASE + 1), 0x01);
}

#[test]
fn gettd_track_one_reports_data_start() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 10])));

    cd.cmd_get_td(&[0x01]);
    cd.tick(10_000_000);

    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_ne!(cd.read8(BASE) & status_bit::RESPONSE_FIFO_NOT_EMPTY, 0);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE) & status_bit::RESPONSE_FIFO_NOT_EMPTY, 0);
}

#[test]
fn gettd_track_zero_reports_leadout_minute_second() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(Disc::from_bin(vec![0u8; psx_iso::SECTOR_BYTES * 10])));

    cd.cmd_get_td(&[0x00]);
    cd.tick(10_000_000);

    let _stat = cd.read8(BASE + 1);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_ne!(cd.read8(BASE) & status_bit::RESPONSE_FIFO_NOT_EMPTY, 0);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE) & status_bit::RESPONSE_FIFO_NOT_EMPTY, 0);
}

#[test]
fn getlocp_reports_index0_and_index1_for_pregap_tracks() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(multitrack_disc_with_pregap()));

    cd.read_lba = 10;
    cd.cmd_get_loc_p();
    cd.tick(10_000_000);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x01);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x10);
    cd.irq_flag = 0;

    cd.read_lba = 12;
    cd.cmd_get_loc_p();
    cd.tick(20_000_000);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x01);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x12);
}

#[test]
fn gettn_reports_last_track_for_multitrack_disc() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(multitrack_disc_with_pregap()));

    cd.cmd_get_tn();
    cd.tick(10_000_000);

    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0x01);
    assert_eq!(cd.read8(BASE + 1), 0x02);
}

#[test]
fn gettd_reports_track_start_and_leadout_for_multitrack_disc() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(multitrack_disc_with_pregap()));

    cd.cmd_get_td(&[0x02]);
    cd.tick(10_000_000);
    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    cd.irq_flag = 0;

    cd.cmd_get_td(&[0x00]);
    cd.tick(20_000_000);
    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x02);
    assert_eq!(cd.read8(BASE + 1), 0x00);
}

#[test]
fn play_track_param_seeks_to_requested_track_start() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(multitrack_disc_with_pregap()));

    play_and_arrive(&mut cd, 0x02);

    assert_eq!(cd.read_lba, 12);
    assert_eq!(
        cd.drive_status & drive_status_bit::PLAYING,
        drive_status_bit::PLAYING
    );
}

#[test]
fn cdda_play_streams_pcm_samples() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));

    play_and_arrive(&mut cd, 0x02);
    cd.pump_cdda_samples(2);

    assert_eq!(cd.read_lba, 12);
    assert_eq!(cd.cdda_sample_index, 2);
    assert_eq!(cd.drain_cd_audio(), vec![(1000, -1000), (1001, -1001)]);
}

#[test]
fn cdda_play_advances_one_lba_per_sector_frame() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));

    play_and_arrive(&mut cd, 0x02);
    cd.pump_cdda_samples(CDDA_SAMPLES_PER_SECTOR);

    assert_eq!(cd.read_lba, 13);
    assert_eq!(cd.cdda_sample_index, 0);
    assert_eq!(cd.cd_audio_queue_len(), CDDA_SAMPLES_PER_SECTOR);
}

#[test]
fn cdda_play_stops_at_audio_track_end() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));

    play_and_arrive(&mut cd, 0x02);
    cd.pump_cdda_samples(CDDA_SAMPLES_PER_SECTOR * 2 + 1);

    assert_eq!(cd.read_lba, 14);
    assert_eq!(cd.cdda_sample_index, 0);
    assert_eq!(cd.drive_status & drive_status_bit::PLAYING, 0);
}

#[test]
fn command_during_cdda_playback_acks_later_than_when_idle() {
    // Baseline: GetStat while the drive is idle acks at the flat first-response
    // delay (what every emulator models, and what makes the bug invisible).
    let mut idle = CdRom::new();
    idle.insert_disc(Some(cdda_disc()));
    idle.queue_command(0x01, 1_000); // GetStat, not playing
    let idle_ack = idle
        .pending
        .iter()
        .find(|ev| ev.command == 0x01 && ev.irq == IrqType::Acknowledge)
        .expect("idle GetStat ack pending");
    assert_eq!(idle_ack.deadline, 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES);

    // During CD-DA playback the same GetStat acks CDDA_BUSY_RESPONSE_CYCLES
    // later -- the busy single controller modelled. This is the latency that
    // makes "poll the drive every frame while music plays" stall on silicon,
    // so the engine must not do it (see the menu CD-DA loop).
    let mut playing = CdRom::new();
    playing.insert_disc(Some(cdda_disc()));
    playing.push_param(0x02);
    // Play track 2. Play only arms a seek; the busy penalty models a
    // controller actually streaming audio, so wait for the head to arrive
    // before measuring it.
    playing.queue_command(0x03, 2_000);
    let arrives = playing.cdda_seek_done_at.expect("Play arms a seek");
    playing.complete_cdda_seek(arrives);
    assert_ne!(playing.drive_status & drive_status_bit::PLAYING, 0);
    // The Play command itself was issued while idle, so its own ack is NOT
    // penalised (only commands during an already-playing drive are).
    let play_ack = playing
        .pending
        .iter()
        .find(|ev| ev.command == 0x03 && ev.irq == IrqType::Acknowledge)
        .expect("play ack pending");
    assert_eq!(play_ack.deadline, 2_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES);

    playing.queue_command(0x01, arrives + 1_000); // GetStat while playing
    let busy_ack = playing
        .pending
        .iter()
        .find(|ev| ev.command == 0x01 && ev.irq == IrqType::Acknowledge)
        .expect("playing GetStat ack pending");
    assert_eq!(
        busy_ack.deadline,
        arrives + 1_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES + CDDA_BUSY_RESPONSE_CYCLES
    );
}

#[test]
fn getstat_after_cdda_track_end_is_prompt_and_reports_stopped() {
    // The engine loops a menu track by polling until the drive reports stopped,
    // then re-playing. That only works if, once the track ends, GetStat answers
    // promptly (no busy penalty -- the drive is no longer streaming) AND reports
    // PLAYING clear. This is the counterpart to the during-playback penalty.
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));
    cd.push_param(0x02);
    cd.queue_command(0x03, 1_000); // Play track 2
    cd.pump_cdda_samples(CDDA_SAMPLES_PER_SECTOR * 2 + 1); // run past track end
    assert_eq!(cd.drive_status & drive_status_bit::PLAYING, 0);

    cd.queue_command(0x01, 2_000); // GetStat after the track ended
    let ack = cd
        .pending
        .iter()
        .find(|ev| ev.command == 0x01 && ev.irq == IrqType::Acknowledge)
        .expect("post-end GetStat ack pending");
    assert_eq!(ack.deadline, 2_000 + FIRST_RESPONSE_WITH_MEDIA_CYCLES);
    assert_eq!(ack.bytes[0] & drive_status_bit::PLAYING, 0);
}

#[test]
fn setfilter_and_getparam_roundtrip_filter_state() {
    let mut cd = CdRom::new();
    cd.mode = 0xE8;

    cd.cmd_set_filter(&[0x12, 0x34]);
    assert_eq!(cd.xa_filter_file, 0x12);
    assert_eq!(cd.xa_filter_channel, 0x34);
    cd.pending.clear();
    cd.responses.clear();
    cd.irq_flag = 0;

    cd.cmd_get_param();
    cd.tick(10_000_000);
    assert_eq!(cd.read8(BASE + 1), cd.stat_byte());
    assert_eq!(cd.read8(BASE + 1), 0xE8);
    assert_eq!(cd.read8(BASE + 1), 0x00);
    assert_eq!(cd.read8(BASE + 1), 0x12);
    assert_eq!(cd.read8(BASE + 1), 0x34);
}

#[test]
fn mute_and_demute_commands_flip_latch() {
    let mut cd = CdRom::new();
    assert!(!cd.muted);

    cd.cmd_mute(true);
    cd.tick(10_000_000);
    assert!(cd.muted);

    cd.pending.clear();
    cd.responses.clear();
    cd.irq_flag = 0;

    cd.cmd_mute(false);
    cd.tick(20_000_000);
    assert!(!cd.muted);
}

#[test]
fn xa_decode_silent_stereo_sector_has_full_frame_count() {
    let mut raw = vec![0u8; psx_iso::SECTOR_BYTES];
    raw[15] = 2;
    raw[18] = 0x24;
    raw[19] = 0x01; // 4-bit stereo, 37.8 kHz

    let mut left = crate::spu::XaDecoderState::new();
    let mut right = crate::spu::XaDecoderState::new();
    let coding = parse_xa_coding(raw[19]).expect("valid XA coding");
    let samples = decode_xa_audio_sector(&raw, coding, &mut left, &mut right)
        .expect("common 4-bit stereo XA should decode");

    assert_eq!(samples.len(), 2352);
    assert!(samples.iter().all(|&(l, r)| l == 0 && r == 0));
}

#[test]
fn xa_decode_silent_mono_sector_has_full_frame_count() {
    let mut raw = vec![0u8; psx_iso::SECTOR_BYTES];
    raw[15] = 2;
    raw[18] = 0x24;
    raw[19] = 0x00; // 4-bit mono, 37.8 kHz

    let mut left = crate::spu::XaDecoderState::new();
    let mut right = crate::spu::XaDecoderState::new();
    let coding = parse_xa_coding(raw[19]).expect("valid XA coding");
    let samples = decode_xa_audio_sector(&raw, coding, &mut left, &mut right)
        .expect("4-bit mono XA should decode");

    assert_eq!(samples.len(), 4704);
    assert!(samples.iter().all(|&(l, r)| l == 0 && r == 0));
}

#[test]
fn xa_decode_uses_stream_coding_not_each_sector_byte() {
    let mut raw = vec![0u8; psx_iso::SECTOR_BYTES];
    raw[15] = 2;
    raw[18] = 0x24;
    raw[19] = 0x0c; // invalid if reparsed; Redux ignores this mid-stream.

    let mut left = crate::spu::XaDecoderState::new();
    let mut right = crate::spu::XaDecoderState::new();
    let coding = XaCoding {
        stereo: true,
        freq: 37_800,
        nbits: 4,
    };
    let samples = decode_xa_audio_sector(&raw, coding, &mut left, &mut right)
        .expect("stream coding should drive decode after the first sector");

    assert_eq!(samples.len(), 2352);
    assert!(samples.iter().all(|&(l, r)| l == 0 && r == 0));
}

#[test]
fn motor_on_command_sets_motor_flag() {
    let mut cd = CdRom::new();
    assert!(!cd.motor_on);

    cd.cmd_motor_on();
    cd.tick(10_000_000);

    assert!(cd.motor_on);
    assert_eq!(
        cd.read8(BASE + 1) & drive_status_bit::MOTOR_ON,
        drive_status_bit::MOTOR_ON
    );
}

#[test]
fn reset_cancels_read_and_completes_with_motor_on() {
    let mut cd = CdRom::new();
    cd.motor_on = true;
    cd.reading = true;
    cd.data_fifo.push_back(0xAB);
    cd.pending.push_back(PendingEvent {
        command: 0x06,
        deadline: 123,
        irq: IrqType::DataReady,
        bytes: vec![0x20],
        followup: None,
    });

    cd.cmd_reset();
    assert!(cd.tick(FIRST_RESPONSE_CYCLES + 1));
    assert_eq!(cd.irq_flag, IrqType::Acknowledge as u8);
    cd.write8(BASE, 1);
    cd.write8(BASE + 3, 0x1F);
    assert_eq!(cd.irq_flag, 0);
    assert!(cd.tick(FIRST_RESPONSE_CYCLES + RESET_SECOND_RESPONSE_CYCLES + 2));
    assert_eq!(cd.irq_flag, IrqType::Complete as u8);

    assert!(cd.motor_on);
    assert!(!cd.reading);
    assert!(cd.data_fifo.is_empty());
    assert_eq!(
        cd.pending.len(),
        0,
        "reset ACK and completion should both be delivered"
    );
    assert_eq!(
        cd.read8(BASE + 1) & drive_status_bit::MOTOR_ON,
        drive_status_bit::MOTOR_ON
    );
}

#[test]
fn repeated_reset_before_ack_reschedules_one_long_completion() {
    let mut cd = CdRom::new();
    cd.last_command = 0x0A;
    cd.scheduling_cycle = 100;
    cd.cmd_reset();

    cd.scheduling_cycle = 1_000;
    cd.cmd_reset();
    assert_eq!(
        count_pending_command_irq(&cd, 0x0A, IrqType::Acknowledge),
        1
    );
    assert_eq!(count_pending_command_irq(&cd, 0x0A, IrqType::Complete), 1);
    let ack = cd
        .pending
        .iter()
        .find(|event| event.command == 0x0A && event.irq == IrqType::Acknowledge)
        .expect("reset ACK");
    assert_eq!(ack.deadline, 1_000 + FIRST_RESPONSE_CYCLES);
    assert_eq!(
        ack.followup.as_ref().map(|f| (f.command, f.irq, f.delay)),
        Some((0x0A, IrqType::Complete, RESET_SECOND_RESPONSE_CYCLES))
    );

    assert!(!cd.tick(100 + FIRST_RESPONSE_CYCLES + 1));
    assert!(cd.tick(1_000 + FIRST_RESPONSE_CYCLES + 1));
    cd.irq_flag = 0;
    cd.responses.clear();
    assert_eq!(count_pending_command_irq(&cd, 0x0A, IrqType::Complete), 1);
}

#[test]
fn repeated_reset_while_completion_pending_publishes_completion() {
    let mut cd = CdRom::new();
    cd.last_command = 0x0A;
    cd.scheduling_cycle = 100;
    cd.cmd_reset();

    assert!(cd.tick(100 + FIRST_RESPONSE_CYCLES + 1));
    cd.irq_flag = 0;
    cd.responses.clear();

    cd.scheduling_cycle = 2_000_000;
    cd.cmd_reset();
    assert_eq!(
        count_pending_command_irq(&cd, 0x0A, IrqType::Acknowledge),
        0
    );
    assert_eq!(count_pending_command_irq(&cd, 0x0A, IrqType::Complete), 1);
    let complete = cd
        .pending
        .iter()
        .find(|event| event.command == 0x0A && event.irq == IrqType::Complete)
        .expect("reset completion after pending completion");
    assert_eq!(complete.deadline, 2_000_000 + FIRST_RESPONSE_CYCLES);
    assert!(complete.followup.is_none());
}

#[test]
fn pending_events_are_kept_sorted_by_deadline() {
    let mut cd = CdRom::new();
    cd.insert_pending_event(PendingEvent {
        command: 0x06,
        deadline: 300,
        irq: IrqType::DataReady,
        bytes: vec![0x20],
        followup: None,
    });
    cd.insert_pending_event(PendingEvent {
        command: 0x01,
        deadline: 100,
        irq: IrqType::Acknowledge,
        bytes: vec![0x00],
        followup: None,
    });
    cd.insert_pending_event(PendingEvent {
        command: 0x01,
        deadline: 200,
        irq: IrqType::Complete,
        bytes: vec![0x00],
        followup: None,
    });

    let deadlines = cd.pending.iter().map(|ev| ev.deadline).collect::<Vec<_>>();
    assert_eq!(deadlines, vec![100, 200, 300]);
}

#[test]
fn followup_chains_to_latest_ack_even_with_later_events_present() {
    let mut cd = CdRom::new();
    cd.insert_pending_event(PendingEvent {
        command: 0x06,
        deadline: 300,
        irq: IrqType::DataReady,
        bytes: vec![0x20],
        followup: None,
    });
    cd.insert_pending_event(PendingEvent {
        command: 0x01,
        deadline: 100,
        irq: IrqType::Acknowledge,
        bytes: vec![0x00],
        followup: None,
    });

    cd.chain_followup(IrqType::Complete, vec![0x01], 77);

    let ack = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::Acknowledge)
        .expect("ack event present");
    let data_ready = cd
        .pending
        .iter()
        .find(|ev| ev.irq == IrqType::DataReady)
        .expect("later event present");
    assert_eq!(
        ack.followup
            .as_ref()
            .map(|f| (f.delay, f.irq, f.bytes.clone())),
        Some((77, IrqType::Complete, vec![0x01]))
    );
    assert!(
        data_ready.followup.is_none(),
        "later event must stay untouched"
    );
}

#[test]
fn disc_region_code_uses_license_sector_text() {
    use psx_iso::{SECTOR_BYTES, SECTOR_USER_DATA_OFFSET};

    let mut bytes = vec![0u8; SECTOR_BYTES * 6];
    let license = b"Licensed by Sony Computer Entertainment Europe for PlayStation";
    let off = 4 * SECTOR_BYTES + SECTOR_USER_DATA_OFFSET;
    bytes[off..off + license.len()].copy_from_slice(license);

    let disc = Disc::from_bin(bytes);
    assert_eq!(disc_region_code(&disc), *b"SCEE");
}

fn count_pending_command_irq(cd: &CdRom, command: u8, irq: IrqType) -> usize {
    cd.pending
        .iter()
        .map(|event| {
            usize::from(event.command == command && event.irq == irq)
                + usize::from(
                    event
                        .followup
                        .as_ref()
                        .is_some_and(|followup| followup.command == command && followup.irq == irq),
                )
        })
        .sum()
}

/// The bug this whole model exists for. A guest that polls GetStat after Play
/// and reads "not playing" as "the track finished" restarts its music for as
/// long as the seek lasts. With Play declaring the drive playing at once, the
/// poll answered correctly by accident and no headless check could show it.
///
/// Costed a real one on the demo disc: menu music at the far end of a 660 MB
/// disc, polled twice a second, restarting forever on hardware while every
/// capture here looked perfect.
#[test]
fn play_reports_seeking_with_no_audio_until_the_head_arrives() {
    let mut cd = CdRom::new();
    cd.insert_disc(Some(cdda_disc()));

    cd.cmd_play(&[0x02]);
    let arrives = cd.cdda_seek_done_at.expect("Play arms a seek");

    // On the way: seeking, NOT playing, and producing nothing.
    cd.tick(arrives - 1);
    assert_ne!(cd.drive_status & drive_status_bit::SEEKING, 0);
    assert_eq!(cd.drive_status & drive_status_bit::PLAYING, 0);
    cd.pump_cdda_samples(4);
    assert!(
        cd.drain_cd_audio().is_empty(),
        "no audio before the head lands"
    );

    // Arrived: playing, not seeking, and audible.
    cd.tick(arrives + 1);
    assert_eq!(cd.drive_status & drive_status_bit::SEEKING, 0);
    assert_ne!(cd.drive_status & drive_status_bit::PLAYING, 0);
    cd.pump_cdda_samples(4);
    assert_eq!(cd.drain_cd_audio().len(), 4);
}

/// Stopping a drive that is still on its way has to cancel the journey. Left
/// armed, the abandoned Play would land later and start playing a track the
/// guest already gave up on.
#[test]
fn stopping_mid_seek_cancels_the_journey() {
    for stop in [CdRom::cmd_stop, CdRom::cmd_pause, CdRom::cmd_init] {
        let mut cd = CdRom::new();
        cd.insert_disc(Some(cdda_disc()));
        cd.cmd_play(&[0x02]);
        let arrives = cd.cdda_seek_done_at.expect("Play arms a seek");

        stop(&mut cd);
        assert_eq!(cd.cdda_seek_done_at, None);

        cd.tick(arrives + 1);
        assert_eq!(cd.drive_status & drive_status_bit::PLAYING, 0);
    }
}
