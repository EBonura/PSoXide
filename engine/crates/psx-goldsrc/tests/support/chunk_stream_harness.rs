#![feature(optimize_attribute)]
#![allow(dead_code, unused_imports, static_mut_refs)]
use std::cell::RefCell;
mod old;
mod shared;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Prepare,
    Start(u32),
    Read(u32),
    Stop,
    Ready(u8, u32),
    Fallback(u32),
    Invalidate,
    Hook(usize, usize),
    Value(String),
    Output(Vec<u8>),
}
#[derive(Clone, Default)]
struct Fault {
    prepare: bool,
    start: bool,
    read_at: usize,
    wait: u32,
    error: bool,
}
#[derive(Default)]
struct Device {
    bytes: Vec<u8>,
    events: Vec<Event>,
    fault: Fault,
    reads: usize,
}
thread_local! {static DEVICE:RefCell<Device>=RefCell::new(Device::default());}
fn event(e: Event) {
    DEVICE.with(|d| d.borrow_mut().events.push(e));
}
fn output(d: &[u32]) {
    let bytes = unsafe { std::slice::from_raw_parts(d.as_ptr().cast::<u8>(), d.len() * 4) };
    event(Event::Output(bytes.to_vec()));
}
fn value<T: std::fmt::Debug>(x: T) {
    event(Event::Value(format!("{x:?}")));
}
fn fault(f: Fault) {
    DEVICE.with(|d| {
        let mut d = d.borrow_mut();
        d.fault = f;
        d.reads = 0;
    });
}
fn hook(done: usize, needed: usize) {
    event(Event::Hook(done, needed));
}
pub type RenderPacketScratch = [u32; 384];
pub static mut PRIMITIVE_PACKETS: RenderPacketScratch = [0; 384];
pub mod room_budget {
    pub const PACK_CACHE_ENTRIES: usize = 128;
}
pub unsafe fn invalidate_world_packet_cache() {
    event(Event::Invalidate);
}
pub mod fake {
    use super::*;
    pub const SECTOR_WORDS: usize = 512;
    pub struct Reader {
        lba: u32,
    }
    impl Reader {
        pub const fn new() -> Self {
            Self { lba: 0 }
        }
        pub fn prepare(&mut self) -> bool {
            event(Event::Prepare);
            DEVICE.with(|d| !d.borrow().fault.prepare)
        }
        pub fn start_read(&mut self, lba: u32) -> bool {
            event(Event::Start(lba));
            self.lba = lba;
            DEVICE.with(|d| !d.borrow().fault.start)
        }
        pub fn read_sector(&mut self, out: &mut [u32; 512]) -> bool {
            event(Event::Read(self.lba));
            let result = DEVICE.with(|d| {
                let mut d = d.borrow_mut();
                d.reads += 1;
                if d.fault.read_at == d.reads {
                    return false;
                }
                let off = (self.lba.saturating_sub(1024)) as usize * 2048;
                if off + 2048 > d.bytes.len() {
                    return false;
                }
                for (i, v) in out.iter_mut().enumerate() {
                    *v = u32::from_le_bytes(
                        d.bytes[off + i * 4..off + i * 4 + 4].try_into().unwrap(),
                    );
                }
                true
            });
            self.lba += 1;
            result
        }
        pub fn stop(&mut self) {
            event(Event::Stop);
        }
    }
    pub fn ready() -> Result<bool, psx_io::cdrom::SectorPollError> {
        DEVICE.with(|d| {
            let mut d = d.borrow_mut();
            let code = if d.fault.error {
                2
            } else if d.fault.wait > 0 {
                d.fault.wait -= 1;
                0
            } else {
                1
            };
            if let Some(Event::Ready(old, n)) = d.events.last_mut() {
                if *old == code {
                    *n += 1;
                } else {
                    d.events.push(Event::Ready(code, 1));
                }
            } else {
                d.events.push(Event::Ready(code, 1));
            }
            match code {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(psx_io::cdrom::SectorPollError),
            }
        })
    }
    pub fn find_entry(
        _rd: &mut Reader,
        _lba: u32,
        id: u32,
        _scratch: &mut [u32; 512],
    ) -> Option<psx_pack::PackEntry> {
        event(Event::Fallback(id));
        DEVICE.with(|d| {
            let d = d.borrow();
            psx_pack::find_chunk(&d.bytes, &psx_pack::parse_header(&d.bytes)?, id)
        })
    }
}
impl shared::ChunkReader for fake::Reader {
    unsafe fn prepare(&mut self) -> bool {
        self.prepare()
    }
    unsafe fn start_read(&mut self, lba: u32) -> bool {
        self.start_read(lba)
    }
    unsafe fn read_sector(&mut self, dst: &mut [u32; 512]) -> bool {
        self.read_sector(dst)
    }
    unsafe fn stop(&mut self) {
        self.stop()
    }
    unsafe fn ready(&mut self) -> Result<bool, psx_io::cdrom::SectorPollError> {
        fake::ready()
    }
    unsafe fn find_entry(
        &mut self,
        l: u32,
        id: u32,
        s: &mut [u32; 512],
    ) -> Option<psx_pack::PackEntry> {
        fake::find_entry(self, l, id, s)
    }
}
struct Arena;
unsafe impl shared::PacketArena for Arena {
    const CAPACITY: usize = 128;
    unsafe fn storage() -> *mut u8 {
        core::ptr::addr_of_mut!(PRIMITIVE_PACKETS).cast()
    }
    unsafe fn invalidate_render_cache() {
        invalidate_world_packet_cache()
    }
}
trait Api {
    fn new() -> Self;
    fn invalidate(&mut self);
    fn prime(&mut self, id: u32, n: usize);
    fn hook(&mut self);
    fn load(&mut self, id: u32, d: &mut [u32]) -> Option<usize>;
    fn decoded(&mut self, id: u32, d: &mut [u32]) -> Option<(usize, usize)>;
    fn begin(&mut self, id: u32, d: &mut [u32]) -> bool;
    fn pump(&mut self) -> String;
    fn active(&mut self) -> bool;
    fn abort(&mut self);
}
struct Legacy;
impl Api for Legacy {
    fn new() -> Self {
        unsafe { old::reset() };
        Self
    }
    fn invalidate(&mut self) {
        unsafe { old::invalidate_cache() }
    }
    fn prime(&mut self, id: u32, n: usize) {
        old::prime_persistent_entries(id, n)
    }
    fn hook(&mut self) {
        old::set_sector_hook(Some(hook));
    }
    fn load(&mut self, id: u32, d: &mut [u32]) -> Option<usize> {
        old::load_chunk(id, d)
    }
    fn decoded(&mut self, id: u32, d: &mut [u32]) -> Option<(usize, usize)> {
        old::load_chunk_decompressed(id, d).map(|x| (x.stored_len, x.raw_len))
    }
    fn begin(&mut self, id: u32, d: &mut [u32]) -> bool {
        unsafe { old::stream_begin(id, d.as_mut_ptr(), d.len()) }
    }
    fn pump(&mut self) -> String {
        format!("{:?}", unsafe { old::stream_pump() })
    }
    fn active(&mut self) -> bool {
        old::stream_active()
    }
    fn abort(&mut self) {
        old::stream_abort()
    }
}
struct New(shared::CachedStreamer<fake::Reader, Arena, PERSIST_CAPACITY>);
impl Api for New {
    fn new() -> Self {
        Self(unsafe { shared::CachedStreamer::new(fake::Reader::new()) })
    }
    fn invalidate(&mut self) {
        unsafe { self.0.invalidate_cache() }
    }
    fn prime(&mut self, id: u32, n: usize) {
        self.0.prime_persistent_entries(id, n)
    }
    fn hook(&mut self) {
        self.0.set_sector_hook(Some(hook));
    }
    fn load(&mut self, id: u32, d: &mut [u32]) -> Option<usize> {
        self.0.load_chunk(id, d)
    }
    fn decoded(&mut self, id: u32, d: &mut [u32]) -> Option<(usize, usize)> {
        self.0
            .load_chunk_decompressed(id, d)
            .map(|x| (x.stored_len, x.raw_len))
    }
    fn begin(&mut self, id: u32, d: &mut [u32]) -> bool {
        unsafe { self.0.stream_begin(id, d.as_mut_ptr(), d.len()) }
    }
    fn pump(&mut self) -> String {
        format!("{:?}", unsafe { self.0.stream_pump() })
    }
    fn active(&mut self) -> bool {
        self.0.stream_active()
    }
    fn abort(&mut self) {
        self.0.stream_abort()
    }
}
fn pack(count: usize) -> Vec<u8> {
    let hs = (28 + count * 24).div_ceil(2048);
    let mut all = vec![0; hs * 2048];
    all[..8].copy_from_slice(b"PSOXWPAK");
    for (off, val) in [
        (8, 1),
        (12, count as u32),
        (20, hs as u32),
        (24, (count * 24) as u32),
    ] {
        all[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }
    for i in 0..count {
        let data = if i % 7 == 0 {
            b"HLZC\x03\0\0\0\x30abc".to_vec()
        } else if i % 9 == 0 {
            vec![]
        } else {
            vec![(i % 255) as u8; 2048 * (i % 3) + 19]
        };
        let sec = all.len() / 2048;
        let n = data.len().div_ceil(2048);
        let at = 28 + i * 24;
        for (off, val) in [
            (0, 1000 + i as u32),
            (4, sec as u32),
            (8, n as u32),
            (12, data.len() as u32),
            (16, psx_pack::fnv1a32(&data)),
        ] {
            all[at + off..at + off + 4].copy_from_slice(&val.to_le_bytes());
        }
        all.extend(&data);
        all.resize((sec + n) * 2048, 0);
    }
    let total = (all.len() / 2048) as u32;
    all[16..20].copy_from_slice(&total.to_le_bytes());
    all
}
fn run<T: Api>(bytes: &[u8]) -> Vec<Event> {
    DEVICE.with(|d| {
        *d.borrow_mut() = Device {
            bytes: bytes.to_vec(),
            ..Device::default()
        }
    });
    unsafe { PRIMITIVE_PACKETS = [0; 384] };
    let mut a = T::new();
    let mut dst = vec![0xa5a5a5a5; 4096];
    a.hook();
    value(a.pump());
    value(a.active());
    for id in [1000, 1001, 1084, 1000, 9999] {
        value(a.load(id, &mut dst));
        output(&dst);
    }
    a.prime(1000, PERSIST_CAPACITY + 4);
    a.invalidate();
    for id in [1000, 1000 + PERSIST_CAPACITY as u32, 1000] {
        value(a.begin(id, &mut dst));
        value(a.pump());
        output(&dst);
        for _ in 0..70 {
            let r = a.pump();
            let done = !a.active();
            value(r);
            if done {
                break;
            }
        }
        output(&dst);
        a.invalidate();
    }
    value(a.begin(1001, &mut dst));
    value(a.begin(1002, &mut dst));
    value(a.load(1003, &mut dst));
    output(&dst);
    value(a.active());
    value(a.decoded(1000, &mut dst));
    output(&dst);
    value(a.load(1001, &mut []));
    value(a.begin(1001, &mut []));
    a.abort();
    a.abort();
    for f in [
        Fault {
            prepare: true,
            ..Fault::default()
        },
        Fault {
            start: true,
            ..Fault::default()
        },
        Fault {
            read_at: 1,
            ..Fault::default()
        },
        Fault {
            wait: 3000,
            ..Fault::default()
        },
        Fault {
            wait: u32::MAX,
            ..Fault::default()
        },
        Fault {
            error: true,
            ..Fault::default()
        },
    ] {
        fault(Fault::default());
        value(a.begin(1001, &mut dst));
        fault(f);
        value(a.pump());
        for _ in 0..65 {
            let r = a.pump();
            let done = !a.active();
            value(r);
            if done {
                break;
            }
        }
        output(&dst);
        a.abort();
        fault(Fault::default());
        value(a.load(1001, &mut dst));
        output(&dst);
    }
    a.invalidate();
    fault(Fault {
        prepare: true,
        ..Fault::default()
    });
    value(a.load(1080, &mut dst));
    fault(Fault::default());
    value(a.load(1080, &mut dst));
    output(&dst);
    DEVICE.with(|d| std::mem::take(&mut d.borrow_mut().events))
}
fn boundary_contract<T: Api>() {
    DEVICE.with(|d| {
        *d.borrow_mut() = Device {
            bytes: pack(90),
            ..Device::default()
        }
    });
    let mut a = T::new();
    let mut dst = vec![0xa5a5a5a5; 4096];
    assert!(a.begin(1001, &mut dst));
    DEVICE.with(|d| d.borrow_mut().events.clear());
    let untouched = dst.clone();
    assert_eq!(a.pump(), "InFlight");
    assert_eq!(dst, untouched, "first pump must yield without writing");
    DEVICE.with(|d| {
        assert!(
            d.borrow().events.is_empty(),
            "first pump must not poll/read"
        )
    });
    assert_eq!(a.pump(), "InFlight");
    DEVICE.with(|d| {
        assert_eq!(
            d.borrow()
                .events
                .iter()
                .filter(|e| matches!(e, Event::Read(_)))
                .count(),
            1
        )
    });
    assert_eq!(
        a.pump(),
        "Done(ChunkLoad { stored_len: 2067, raw_len: 2067 })"
    );
    assert!(!a.active());
    for f in [
        Fault {
            prepare: true,
            ..Fault::default()
        },
        Fault {
            start: true,
            ..Fault::default()
        },
    ] {
        fault(f);
        assert!(!a.begin(1001, &mut dst));
        assert!(!a.active());
        fault(Fault::default());
        assert!(a.begin(1001, &mut dst));
        a.abort();
    }
    fault(Fault::default());
    assert!(a.begin(1001, &mut dst));
    assert_eq!(a.pump(), "InFlight");
    fault(Fault {
        wait: u32::MAX,
        ..Fault::default()
    });
    for _ in 0..63 {
        assert_eq!(a.pump(), "InFlight");
        assert!(a.active());
    }
    assert_eq!(a.pump(), "Failed");
    assert!(!a.active());
}
fn main() {
    boundary_contract::<Legacy>();
    boundary_contract::<New>();

    let mut cases = vec![pack(1), pack(90), pack(129)];
    let mut bad = pack(90);
    bad[0] = 0;
    cases.push(bad);
    let mut truncated = pack(90);
    truncated.truncate(2048);
    cases.push(truncated);
    let mut total = 0;
    for (i, p) in cases.iter().enumerate() {
        let old = run::<Legacy>(p);
        let new = run::<New>(p);
        assert_eq!(old, new, "transport/return/destination mismatch case{i}");
        total += old.len();
    }
    println!("PASS capacity{}: {} exact transport/return/buffer observations across cache-hit/miss/collision, straddled entries, oversized fallback, invalidation, compressed/raw, cancel/restart, just-started yield, idle64 and readiness/error cases",PERSIST_CAPACITY,total);
}
