//! The chunk owner against a scripted drive: the table is cached only when it
//! was read whole, failed reads are tried once more from a fresh seek, and a
//! session hands its sectors to the interrupt and ends with the reader's Pause.

use psx_goldsrc::chunk_stream::{
    CachedStreamer, ChunkReader, DrainPoll, PacketArena, StreamPump, CACHE_ENTRY_BYTES, PACK_LBA,
    READ_ATTEMPTS, STREAM_TIMEOUT_VBLANKS,
};
use psx_pack::{cd::SECTOR_WORDS, entry_location, ENTRY_BYTES, HEADER_BYTES, SECTOR_BYTES};
use std::cell::{Cell, RefCell};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Start(u32),
    Read(u32),
    Stop,
    Scan(u32),
    DrainStart {
        sectors: u32,
        bytes: u32,
        timeout: u32,
    },
    DrainCancel,
}

#[derive(Default)]
struct Drive {
    pack: Vec<u8>,
    events: Vec<Event>,
    /// 1-based numbers of `read_sector` calls that fail.
    failing_reads: Vec<usize>,
    reads: usize,
    drain: Option<DrainPoll>,
}

thread_local! {
    static DRIVE: RefCell<Drive> = RefCell::new(Drive::default());
    static ARENA: Cell<*mut u8> = Cell::new(
        Box::leak(vec![0u8; 128 * CACHE_ENTRY_BYTES].into_boxed_slice()).as_mut_ptr()
    );
}

fn with<R>(f: impl FnOnce(&mut Drive) -> R) -> R {
    DRIVE.with(|d| f(&mut d.borrow_mut()))
}

fn events() -> Vec<Event> {
    with(|d| std::mem::take(&mut d.events))
}

struct Reader {
    lba: u32,
}

impl ChunkReader for Reader {
    unsafe fn prepare(&mut self) -> bool {
        true
    }
    unsafe fn start_read(&mut self, lba: u32) -> bool {
        self.lba = lba;
        with(|d| d.events.push(Event::Start(lba)));
        true
    }
    unsafe fn read_sector(&mut self, dst: &mut [u32; SECTOR_WORDS]) -> bool {
        let lba = self.lba;
        self.lba += 1;
        with(|d| {
            d.events.push(Event::Read(lba));
            d.reads += 1;
            if d.failing_reads.contains(&d.reads) {
                return false;
            }
            let at = (lba - PACK_LBA) as usize * SECTOR_BYTES;
            if at + SECTOR_BYTES > d.pack.len() {
                return false;
            }
            for (i, word) in dst.iter_mut().enumerate() {
                let b = &d.pack[at + i * 4..at + i * 4 + 4];
                *word = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            true
        })
    }
    unsafe fn stop(&mut self) {
        with(|d| d.events.push(Event::Stop));
    }
    unsafe fn find_entry(
        &mut self,
        _pack_lba: u32,
        id: u32,
        _scratch: &mut [u32; SECTOR_WORDS],
    ) -> Option<psx_pack::PackEntry> {
        with(|d| {
            d.events.push(Event::Scan(id));
            psx_pack::find_chunk(&d.pack, &psx_pack::parse_header(&d.pack)?, id)
        })
    }
    unsafe fn drain_start(
        &mut self,
        _dst: *mut u32,
        sectors: u32,
        byte_len: u32,
        timeout_vblanks: u32,
    ) -> bool {
        with(|d| {
            d.events.push(Event::DrainStart {
                sectors,
                bytes: byte_len,
                timeout: timeout_vblanks,
            });
            d.drain = Some(DrainPoll::Running);
        });
        true
    }
    fn drain_poll(&mut self) -> DrainPoll {
        with(|d| d.drain.unwrap_or(DrainPoll::Failed))
    }
    fn drain_cancel(&mut self) {
        with(|d| {
            d.events.push(Event::DrainCancel);
            d.drain = None;
        });
    }
}

struct Arena;
unsafe impl PacketArena for Arena {
    const CAPACITY: usize = 128;
    unsafe fn storage() -> *mut u8 {
        ARENA.with(Cell::get)
    }
    unsafe fn invalidate_render_cache() {}
}

type Owner = CachedStreamer<Reader, Arena, 8>;

fn owner() -> Owner {
    // SAFETY: each test owns its own drive, arena and owner on its own thread.
    unsafe { Owner::new(Reader { lba: 0 }) }
}

/// A pack of `count` chunks with ids 100.. whose payloads are `sizes[i]` bytes
/// of the value `i + 1`, laid out after a header that spans `header_sectors`.
fn pack(sizes: &[usize]) -> Vec<u8> {
    let count = sizes.len() as u32;
    let table_end = HEADER_BYTES + sizes.len() * ENTRY_BYTES;
    let header_sectors = table_end.div_ceil(SECTOR_BYTES) as u32;
    let mut bytes = vec![0u8; header_sectors as usize * SECTOR_BYTES];
    let mut sector = header_sectors;
    let mut payloads = Vec::new();
    for (i, &size) in sizes.iter().enumerate() {
        let (s, within) = entry_location(i as u32);
        let at = s as usize * SECTOR_BYTES + within;
        let sectors = size.div_ceil(SECTOR_BYTES) as u32;
        for (k, v) in [100 + i as u32, sector, sectors, size as u32, 0, 0]
            .iter()
            .enumerate()
        {
            bytes[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut payload = vec![(i + 1) as u8; size];
        payload.resize(sectors as usize * SECTOR_BYTES, 0);
        payloads.extend(payload);
        sector += sectors;
    }
    bytes[0..8].copy_from_slice(b"PSOXWPAK");
    for (k, v) in [
        1,
        count,
        sector,
        header_sectors,
        (sizes.len() * ENTRY_BYTES) as u32,
    ]
    .iter()
    .enumerate()
    {
        bytes[8 + k * 4..12 + k * 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes.extend(payloads);
    bytes
}

fn setup(sizes: &[usize], failing_reads: &[usize]) {
    with(|d| {
        *d = Drive {
            pack: pack(sizes),
            failing_reads: failing_reads.to_vec(),
            ..Drive::default()
        }
    });
}

fn table_reads(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| **e == Event::Start(PACK_LBA))
        .count()
}

/// 90 entries put the table across two header sectors.
const MANY: [usize; 90] = [3000; 90];

#[test]
fn a_whole_table_is_read_once_and_cached() {
    setup(&MANY, &[]);
    let mut o = owner();
    let mut dst = vec![0u32; 1024];
    assert_eq!(o.load_chunk(100, &mut dst), Some(3000));
    assert_eq!(o.load_chunk(189, &mut dst), Some(3000));
    let e = events();
    assert_eq!(table_reads(&e), 1);
    assert!(!e.iter().any(|e| matches!(e, Event::Scan(_))));
    assert_eq!(dst[0], 0x5a5a_5a5a, "chunk 189 holds the byte 90");
}

#[test]
fn a_table_cut_short_is_not_cached_and_is_read_again() {
    // Read 2 is the table's second sector: the first build stops at the entry that straddles it, 84 of 90.
    setup(&MANY, &[2]);
    let mut o = owner();
    let mut dst = vec![0u32; 1024];
    assert_eq!(o.load_chunk(189, &mut dst), Some(3000));
    assert_eq!(table_reads(&events()), 2, "the short table was read again");
    // Cached now: a later lookup reads no table.
    assert_eq!(o.load_chunk(100, &mut dst), Some(3000));
    assert_eq!(table_reads(&events()), 0);
}

#[test]
fn a_table_that_never_reads_whole_falls_back_to_the_scan_each_time() {
    setup(&MANY, &[2, 4]);
    let mut o = owner();
    let mut dst = vec![0u32; 1024];
    assert_eq!(o.load_chunk(189, &mut dst), Some(3000));
    let e = events();
    assert_eq!(table_reads(&e), READ_ATTEMPTS as usize);
    assert!(e.contains(&Event::Scan(189)));
    // Nothing short was cached: the next lookup tries the table again.
    assert_eq!(o.load_chunk(100, &mut dst), Some(3000));
    assert_eq!(table_reads(&events()), 1);
}

#[test]
fn a_payload_read_that_fails_is_tried_once_more_from_a_fresh_seek() {
    // Reads 1-2 are the table; read 4 is the payload's second sector.
    setup(&MANY, &[4]);
    let mut o = owner();
    let mut dst = vec![0u32; 1024];
    assert_eq!(o.load_chunk(100, &mut dst), Some(3000));
    let payload = PACK_LBA + 2;
    let starts = events()
        .iter()
        .filter(|e| **e == Event::Start(payload))
        .count();
    assert_eq!(starts, 2);
    assert_eq!(dst[0], 0x0101_0101);
    assert_eq!(dst[749], 0x0101_0101, "the last stored word of 3000 bytes");
}

#[test]
fn a_payload_that_fails_every_attempt_is_none() {
    setup(&MANY, &[3, 5]);
    let mut o = owner();
    let mut dst = vec![0u32; 1024];
    assert_eq!(o.load_chunk(100, &mut dst), None);
}

#[test]
fn an_entry_sector_is_bounded_and_retried() {
    setup(&[3 * SECTOR_BYTES, 3000], &[2]);
    let mut o = owner();
    let mut page = [0u32; SECTOR_WORDS];
    // Read 1 is the table, read 2 (the page) fails once.
    assert!(o.load_entry_sector(100, 3 * SECTOR_BYTES, 2, &mut page));
    assert_eq!(page[0], 0x0101_0101);
    let header_sectors = 1;
    let lba = PACK_LBA + header_sectors + 2;
    assert_eq!(
        events().iter().filter(|e| **e == Event::Start(lba)).count(),
        2
    );
    assert!(
        !o.load_entry_sector(100, 3 * SECTOR_BYTES, 3, &mut page),
        "past the entry"
    );
    assert!(
        !o.load_entry_sector(100, 2 * SECTOR_BYTES, 0, &mut page),
        "wrong size"
    );
    assert!(
        !o.load_entry_sector(101, 3000, 0, &mut page),
        "not whole sectors"
    );
}

#[test]
fn a_session_hands_its_sectors_to_the_interrupt_and_ends_with_a_pause() {
    setup(&[5000], &[]);
    let mut o = owner();
    let mut dst = vec![0u32; 2048];
    assert!(unsafe { o.stream_begin(100, dst.as_mut_ptr(), dst.len()) });
    assert!(o.stream_active());
    let e = events();
    assert_eq!(
        &e[e.len() - 2..],
        &[
            Event::Start(PACK_LBA + 1),
            Event::DrainStart {
                sectors: 3,
                bytes: 5000,
                timeout: STREAM_TIMEOUT_VBLANKS
            }
        ]
    );
    assert_eq!(unsafe { o.stream_pump() }, StreamPump::InFlight);
    with(|d| d.drain = Some(DrainPoll::Done));
    // A raw chunk passes the decoder untouched.
    let done = unsafe { o.stream_pump() };
    assert!(matches!(done, StreamPump::Done(l) if l.stored_len == 5000 && l.raw_len == 5000));
    assert_eq!(events(), [Event::DrainCancel, Event::Stop]);
    assert!(!o.stream_active());
    assert_eq!(unsafe { o.stream_pump() }, StreamPump::Idle);
}

#[test]
fn a_failed_session_closes_and_a_blocking_load_supersedes_an_open_one() {
    setup(&[5000, 3000], &[]);
    let mut o = owner();
    let mut dst = vec![0u32; 2048];
    assert!(unsafe { o.stream_begin(100, dst.as_mut_ptr(), dst.len()) });
    with(|d| d.drain = Some(DrainPoll::Failed));
    assert_eq!(unsafe { o.stream_pump() }, StreamPump::Failed);
    assert!(!o.stream_active());
    events();
    assert!(unsafe { o.stream_begin(100, dst.as_mut_ptr(), dst.len()) });
    events();
    assert_eq!(o.load_chunk(101, &mut dst), Some(3000));
    let e = events();
    assert_eq!(&e[..2], &[Event::DrainCancel, Event::Stop]);
    assert!(!o.stream_active());
}

#[test]
fn the_session_timeout_covers_a_first_sector_after_idle() {
    // The console has taken 4.4 s to a first sector after idle: 264 VBlanks.
    assert!(STREAM_TIMEOUT_VBLANKS >= 600);
}
