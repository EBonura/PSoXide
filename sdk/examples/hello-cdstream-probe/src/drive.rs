//! Reads, waits and drive commands the phases are built from.

use crate::clock;
use core::ptr::addr_of_mut;
use psx_cdstream::{
    Completion, Config, LeaseState, Outcome, Priority, Request, RequestState, Ticket,
};
use psx_hw::cd::irq as flag;
use psx_io::periph::Cd;
use psx_rt::interrupts::vblank_count;

pub const SECTOR_WORDS: usize = 512;
/// One long buffer for the single-request reads.
pub const BIG_SECTORS: usize = 256;
static mut BIG: [u32; BIG_SECTORS * SECTOR_WORDS] = [0; BIG_SECTORS * SECTOR_WORDS];
/// Four ring buffers for the sustained stream.
pub const RING_SECTORS: usize = 32;
static mut RING: [[u32; RING_SECTORS * SECTOR_WORDS]; 4] = [[0; RING_SECTORS * SECTOR_WORDS]; 4];

/// Longest wait for any one drive operation, in HBlanks (10 s).
pub const PATIENCE_HB: u32 = 10 * clock::HZ;
/// Failure code for a request that never finished or was refused.
pub const CODE_TIMEOUT: u32 = 0xFFFF_FFFF;
/// Failure code for a request that ended cancelled.
pub const CODE_CANCELLED: u32 = 0xFFFF_FFFE;

static mut FILE_LBA: u32 = 0;
static mut FILE_SECTORS: u32 = 0;

pub fn set_file(lba: u32, sectors: u32) {
    // SAFETY: written once before the transport runs.
    unsafe {
        FILE_LBA = lba;
        FILE_SECTORS = sectors;
    }
}

pub fn file_lba() -> u32 {
    // SAFETY: as `set_file`.
    unsafe { FILE_LBA }
}

pub fn file_sectors() -> u32 {
    // SAFETY: as `set_file`.
    unsafe { FILE_SECTORS }
}

pub fn big() -> *mut u32 {
    addr_of_mut!(BIG).cast::<u32>()
}

pub fn ring(slot: usize) -> *mut u32 {
    // SAFETY: `slot` < 4; the address is formed without a reference.
    unsafe {
        addr_of_mut!(RING)
            .cast::<[u32; RING_SECTORS * SECTOR_WORDS]>()
            .add(slot)
            .cast()
    }
}

/// The transport configuration the probe runs with.
pub fn config(double_speed: bool, pause_after_audio: bool) -> Config {
    Config {
        timeout_vblanks: 240,
        time_handler: true,
        double_speed,
        pause_after_audio,
        ..Config::DEFAULT
    }
}

// ------------------------------------------------------------------ pattern

/// Byte `index` of the benchmark file (a mirror of
/// `psx_iso::cd_stream_bench_expected_byte`, so the guest needs no host crate).
fn expected_byte(index: u32, sectors: u32) -> u8 {
    const MAGIC: &[u8; 8] = b"PSOXSTRM";
    if index < 8 {
        MAGIC[index as usize]
    } else if index < 12 {
        sectors.to_le_bytes()[(index - 8) as usize]
    } else {
        index
            .wrapping_mul(37)
            .wrapping_add(index >> 3)
            .wrapping_add(0x5D) as u8
    }
}

fn expected_word(word: u32, sectors: u32) -> u32 {
    let at = word * 4;
    u32::from(expected_byte(at, sectors))
        | u32::from(expected_byte(at + 1, sectors)) << 8
        | u32::from(expected_byte(at + 2, sectors)) << 16
        | u32::from(expected_byte(at + 3, sectors)) << 24
}

/// Do the `sectors` sectors at `buffer`, which hold file sectors
/// `file_sector..`, carry the pattern? `stride` checks one word in that many
/// (1 checks them all).
pub fn verify(buffer: *const u32, file_sector: u32, sectors: u32, stride: u32) -> bool {
    let total = file_sectors();
    let first_word = file_sector * SECTOR_WORDS as u32;
    let mut word = 0;
    while word < sectors * SECTOR_WORDS as u32 {
        // SAFETY: `word` is inside the `sectors` sectors the caller owns; the
        // request that filled them has finished.
        let got = unsafe { buffer.add(word as usize).read_volatile() };
        if got != expected_word(first_word + word, total) {
            return false;
        }
        word += stride;
    }
    true
}

// ------------------------------------------------------------------- reads

pub fn submit(lba: u32, sectors: u32, destination: *mut u32, priority: Priority) -> Option<Ticket> {
    // SAFETY: every destination is a static buffer of at least `sectors`
    // sectors that nothing touches until the request has finished (each phase
    // waits for it before it reads the buffer).
    let request = unsafe { Request::new_raw(lba, sectors, destination) };
    psx_cdstream::submit(request.with_priority(priority)).ok()
}

pub fn submit_file(file_sector: u32, sectors: u32, destination: *mut u32) -> Option<Ticket> {
    submit(
        file_lba() + file_sector,
        sectors,
        destination,
        Priority::NORMAL,
    )
}

/// Wait for a request to finish, spinning on `state`.
pub fn wait(ticket: Ticket) -> Option<Completion> {
    let start = clock::now();
    loop {
        if let RequestState::Finished(done) = psx_cdstream::state(ticket) {
            return Some(done);
        }
        if clock::since(start) > PATIENCE_HB {
            return None;
        }
    }
}

/// Wait until the drive is stopped and nothing is queued.
pub fn wait_idle() {
    let start = clock::now();
    while !(psx_cdstream::is_idle() && psx_cdstream::queued_count() == 0) {
        if clock::since(start) > PATIENCE_HB {
            return;
        }
    }
}

/// Wait `hblanks`, keeping the transport's upkeep running.
pub fn pause_for(hblanks: u32) {
    let start = clock::now();
    while clock::since(start) < hblanks {
        psx_cdstream::service();
    }
}

/// The failure code of a finished request: 0 when it was done in full.
pub fn code_of(done: Option<Completion>, sectors: u32) -> u32 {
    match done {
        Some(Completion {
            outcome: Outcome::Done,
            received,
            ..
        }) if received == sectors => 0,
        Some(Completion {
            outcome: Outcome::Failed(failure),
            ..
        }) => failure.code().max(1),
        Some(Completion {
            outcome: Outcome::Cancelled,
            ..
        }) => CODE_CANCELLED,
        _ => CODE_TIMEOUT,
    }
}

/// One timed read, in HBlanks from the submit.
#[derive(Clone, Copy)]
pub struct Timed {
    /// The data landed intact.
    pub ok: bool,
    /// 0, or why not (see [`code_of`]).
    pub code: u32,
    /// Until the first sector had landed.
    pub first: u32,
    /// Until the last sector had landed.
    pub done: u32,
    /// Until the drive had stopped again.
    pub idle: u32,
}

/// Read `sectors` sectors of the file from `file_sector` into `big()` and time
/// it from the submit. `check` verifies every byte (`stride` 1) afterwards.
pub fn read_timed(file_sector: u32, sectors: u32, check: bool) -> Timed {
    let mut timed = Timed {
        ok: false,
        code: CODE_TIMEOUT,
        first: 0,
        done: 0,
        idle: 0,
    };
    let t0 = clock::now();
    let Some(ticket) = submit_file(file_sector, sectors, big()) else {
        return timed;
    };
    let mut completion = None;
    loop {
        match psx_cdstream::state(ticket) {
            RequestState::Finished(done) => {
                timed.done = clock::since(t0);
                if timed.first == 0 {
                    timed.first = timed.done;
                }
                completion = Some(done);
                break;
            }
            RequestState::Active { received } if received >= 1 && timed.first == 0 => {
                timed.first = clock::since(t0);
            }
            _ => {}
        }
        if clock::since(t0) > PATIENCE_HB {
            break;
        }
    }
    while !psx_cdstream::is_idle() {
        if clock::since(t0) > 2 * PATIENCE_HB {
            break;
        }
    }
    timed.idle = clock::since(t0);
    timed.code = code_of(completion, sectors);
    timed.ok = timed.code == 0 && (!check || verify(big(), file_sector, sectors, 1));
    timed
}

// ------------------------------------------------------------------- audio

/// Ask for the drive on behalf of audio and collect the controller token.
/// Waits for any read in flight to stop. `None` if the lease never came.
pub fn acquire_audio() -> Option<Cd> {
    wait_idle();
    let _ = psx_cdstream::request_audio_lease();
    let start = clock::now();
    while psx_cdstream::lease_state() != LeaseState::Granted {
        if clock::since(start) > PATIENCE_HB {
            let _ = psx_cdstream::withdraw_audio_lease();
            return None;
        }
    }
    psx_cdstream::take_audio_lease()
}

/// Give the drive back to the transport.
pub fn release_audio(cd: Cd) {
    let _ = psx_cdstream::release_audio_lease(cd);
}

/// Drive status byte bits.
pub const STAT_MOTOR_ON: u8 = 0x02;
pub const STAT_PLAYING: u8 = 0x80;

/// GetStat, or 0xFF if the drive never answered.
pub fn stat(cd: &mut Cd) -> u8 {
    cd.status()
        .ok()
        .and_then(|r| r.bytes().first().copied())
        .unwrap_or(0xFF)
}

/// When a polled command's two responses arrived, in HBlanks from the
/// command byte.
#[derive(Clone, Copy)]
pub struct CommandTiming {
    /// The acknowledge (INT3).
    pub ack: u32,
    /// The completion (INT2), 0 if it never came inside the limit.
    pub complete: u32,
    /// The drive answered with an error (INT5).
    pub error: bool,
}

/// Send a command and time its acknowledge and completion by polling the
/// controller's flag register. `None` if the parameter FIFO never had room or
/// no acknowledge arrived inside `limit` HBlanks. With `wait_complete` clear
/// it returns at the acknowledge (the second response is left pending).
pub fn timed_command(
    cd: &mut Cd,
    command: u8,
    params: &[u8],
    wait_complete: bool,
    limit: u32,
) -> Option<CommandTiming> {
    cd.set_irq_enable_mask(0);
    cd.acknowledge_all_and_reset_parameters();
    cd.discard_response();
    cd.reset_parameter_fifo();
    for &param in params {
        if !cd.wait_parameter_room(4096) {
            return None;
        }
        cd.send_parameter_byte(param);
    }
    let t0 = clock::now();
    cd.send_command_byte(command);
    let mut timing = CommandTiming {
        ack: 0,
        complete: 0,
        error: false,
    };
    // The acknowledge.
    loop {
        match cd.irq_flag_value() {
            flag::ACKNOWLEDGE => {
                timing.ack = clock::since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::ACKNOWLEDGE);
                break;
            }
            flag::ERROR => {
                timing.ack = clock::since(t0);
                timing.error = true;
                cd.discard_response();
                cd.acknowledge_irq(flag::ACK_ALL);
                return Some(timing);
            }
            _ => {}
        }
        if clock::since(t0) > limit {
            cd.acknowledge_irq(flag::ACK_ALL);
            return None;
        }
    }
    if !wait_complete {
        return Some(timing);
    }
    // The completion.
    loop {
        match cd.irq_flag_value() {
            flag::COMPLETE => {
                timing.complete = clock::since(t0);
                cd.discard_response();
                cd.acknowledge_irq(flag::COMPLETE);
                break;
            }
            flag::ERROR => {
                timing.complete = clock::since(t0);
                timing.error = true;
                cd.discard_response();
                cd.acknowledge_irq(flag::ACK_ALL);
                break;
            }
            _ => {}
        }
        if clock::since(t0) > limit {
            cd.acknowledge_irq(flag::ACK_ALL);
            break;
        }
    }
    Some(timing)
}

/// Spin up to `limit` HBlanks, handing `vblank_count` to nobody; a plain delay
/// that keeps the clock widened.
pub fn delay(hblanks: u32) {
    let start = clock::now();
    let _ = vblank_count();
    while clock::since(start) < hblanks {}
}

/// Sort `values` and return `[min, median, max]` (zeros if empty).
pub fn spread(values: &mut [u32]) -> [u32; 3] {
    let n = values.len();
    if n == 0 {
        return [0; 3];
    }
    for i in 1..n {
        let mut j = i;
        while j > 0 && values[j - 1] > values[j] {
            values.swap(j - 1, j);
            j -= 1;
        }
    }
    [values[0], values[n / 2], values[n - 1]]
}
