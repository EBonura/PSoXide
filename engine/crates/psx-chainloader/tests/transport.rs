//! Legacy transport and fault-order oracle. No PSX hardware or retail assets required.
#[path = "oracle/legacy.rs"]
mod legacy;
use psx_chainloader::{LoadedExe, Memory, Observer, Reader, SECTOR_WORDS};
use std::{cell::RefCell, rc::Rc};

type Trace = Rc<RefCell<Vec<String>>>;
#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    Prepare,
    Seek(usize),
    Read(usize),
    EmptyHeader,
    Magic,
    Bounds,
    Checksum,
}
struct Cd {
    trace: Trace,
    header: [u32; 512],
    data: Vec<[u32; 512]>,
    fault: Fault,
    starts: usize,
    reads: usize,
    at: u32,
}
impl Reader for Cd {
    unsafe fn prepare(&mut self) -> bool {
        self.trace.borrow_mut().push("prepare".into());
        !matches!(self.fault, Fault::Prepare)
    }
    unsafe fn start_read_seek_first(&mut self, lba: u32, limit: u32) -> bool {
        assert_eq!(limit, 4_000_000);
        self.trace.borrow_mut().push(format!("seek:{lba}"));
        self.starts += 1;
        self.at = lba;
        !matches!(self.fault,Fault::Seek(n) if n==self.starts)
    }
    unsafe fn read_sector(&mut self, out: &mut [u32; 512]) -> bool {
        self.trace.borrow_mut().push(format!("read:{}", self.at));
        self.reads += 1;
        if matches!(self.fault,Fault::Read(n) if n==self.reads) {
            return false;
        }
        if self.at == 24 {
            assert!(out.iter().all(|x| *x == 0), "header scrub precedes reader");
            if !matches!(self.fault, Fault::EmptyHeader) {
                *out = self.header;
            }
        } else {
            *out = self.data[(self.at - 25) as usize];
        }
        self.at += 1;
        true
    }
    unsafe fn stop(&mut self) {
        self.trace.borrow_mut().push("stop".into());
    }
}
struct Ram(Vec<[u32; 512]>);
// SAFETY: stable backing vector, sized for every tested rounded extent, never resized.
unsafe impl Memory for Ram {
    unsafe fn payload(&mut self, _: u32) -> *mut [u32; 512] {
        self.0.as_mut_ptr()
    }
}
struct Events(Trace);
impl Observer for Events {
    fn stage_ok(&mut self, s: u32) {
        self.0.borrow_mut().push(format!("stage:{s}"));
    }
    fn progress(&mut self, n: u32, t: u32) {
        self.0.borrow_mut().push(format!("progress:{n}/{t}"));
    }
    fn finish(&mut self) {
        self.0.borrow_mut().push("finish".into());
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    result: Result<LoadedExe, (u32, u32)>,
    trace: Vec<String>,
    ram: Vec<[u32; 512]>,
}
fn hash(bytes: impl Iterator<Item = u8>) -> u32 {
    bytes.fold(0x811c9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x01000193)
    })
}
fn run(old: bool, size: usize, fault: Fault, address: u32) -> Outcome {
    let trace = Trace::default();
    let count = size.div_ceil(2048);
    let data: Vec<[u32; 512]> = (0..count)
        .map(|s| std::array::from_fn(|i| ((s * 512 + i) as u32).wrapping_mul(0x719337b)))
        .collect();
    let mut checksum = hash(
        data.iter()
            .flat_map(|s| s.iter().flat_map(|w| w.to_le_bytes()))
            .take(size),
    );
    if matches!(fault, Fault::Checksum) {
        checksum ^= 1;
    }
    let mut header = [0u32; 512];
    header[0] = 0x582d5350;
    header[1] = 0x45584520;
    header[4] = 0x80010000;
    header[5] = 0x80071234;
    header[6] = address;
    header[7] = size as u32;
    header[12] = 0xfffffff0;
    header[13] = 0x30;
    if matches!(fault, Fault::Magic) {
        header[0] = 0x45504f4e;
    }
    if matches!(fault, Fault::Bounds) {
        header[6] = 0x801f0000;
    }
    let mut cd = Cd {
        trace: trace.clone(),
        header,
        data,
        fault,
        starts: 0,
        reads: 0,
        at: 0,
    };
    let mut ram = Ram(vec![[0xdeadbeef; 512]; count.max(1)]);
    let mut events = Events(trace.clone());
    let mut scratch = [u32::MAX; SECTOR_WORDS];
    // SAFETY: this host adapter maps every tested address to stable, nonaliasing RAM;
    // no MMIO executes. Deliberate legacy malformed-header cases cannot escape it.
    let result = unsafe {
        if old {
            legacy::legacy_try_load(
                &mut cd,
                24,
                &mut scratch,
                checksum,
                &mut ram,
                &mut events,
                0x801f0000,
            )
        } else {
            psx_chainloader::try_load(
                &mut cd,
                24,
                &mut scratch,
                checksum,
                &mut ram,
                &mut events,
                0x801f0000,
            )
        }
    };
    let trace = trace.borrow().clone();
    Outcome {
        result,
        trace,
        ram: ram.0,
    }
}
fn pair(size: usize, fault: Fault) -> Outcome {
    let old = run(true, size, fault, 0x80010000);
    let new = run(false, size, fault, 0x80010000);
    assert_eq!(old, new, "size={size} fault={fault:?}");
    new
}
#[test]
fn legacy_and_shared_match_every_chunk_boundary_and_byte_count() {
    for size in [0, 1, 2047, 2048, 2049, 64 * 2048, 65 * 2048, 130 * 2048] {
        let x = pair(size, Fault::None);
        assert_eq!(
            x.result,
            Ok(LoadedExe {
                pc0: 0x80010000,
                gp0: 0x80071234,
                sp: 0x20
            })
        );
        assert_eq!(x.trace.last().unwrap(), "stage:7");
    }
}
#[test]
fn full_130_sector_trace_stops_before_each_progress_and_verifies_after_finish() {
    let x = pair(130 * 2048, Fault::None);
    assert_eq!(
        x.trace
            .iter()
            .filter(|s| s.starts_with("seek"))
            .cloned()
            .collect::<Vec<_>>(),
        ["seek:24", "seek:25", "seek:89", "seek:153"]
    );
    for (i, s) in x.trace.iter().enumerate() {
        if s.starts_with("progress") {
            assert_eq!(x.trace[i - 1], "stop");
        }
    }
    assert_eq!(
        &x.trace[x.trace.len() - 3..],
        ["finish", "stage:6", "stage:7"]
    );
}
#[test]
fn failures_retain_exact_stage_detail_stop_and_no_later_events() {
    for (fault, want) in [
        (Fault::Prepare, (1, 0)),
        (Fault::Seek(1), (2, 24)),
        (Fault::Read(1), (3, 24)),
        (Fault::EmptyHeader, (4, 0)),
        (Fault::Magic, (4, 0x45504f4e)),
        (Fault::Bounds, (5, 0x801f0000)),
        (Fault::Seek(2), (2, 25)),
        (Fault::Seek(3), (2, 89)),
        (Fault::Read(2), (6, 0)),
        (Fault::Read(66), (6, 64)),
        (Fault::Read(131), (6, 129)),
    ] {
        let x = pair(130 * 2048, fault);
        assert_eq!(x.result, Err(want));
        assert!(!x.trace.iter().any(|s| s == "stage:7"));
        if want.0 == 5 {
            assert_eq!(
                x.trace.last().unwrap(),
                "stage:4",
                "legacy bounds failure does not stop"
            );
        }
    }
}
#[test]
fn checksum_failure_returns_actual_ram_hash_after_reading_complete_payload() {
    let x = pair(130 * 2048, Fault::Checksum);
    let actual = hash(
        x.ram
            .iter()
            .flat_map(|s| s.iter().flat_map(|w| w.to_le_bytes())),
    );
    assert_eq!(x.result, Err((7, actual)));
    assert_eq!(x.trace.last().unwrap(), "stage:6");
}
#[test]
fn legacy_logical_bounds_are_not_misrepresented_as_arbitrary_header_validation() {
    for address in [0x8000ffff, 0x801f0001, u32::MAX] {
        assert!(run(false, 0, Fault::None, address).result.is_err());
    }
    for (address, size) in [(0x801f0000, 0), (0x801effff, 1), (0x80010001, 0)] {
        let a = run(true, size, Fault::None, address);
        let b = run(false, size, Fault::None, address);
        assert_eq!(a, b);
        assert!(
            b.result.is_ok(),
            "legacy accepts logical extent even without rounded/align gate"
        );
    }
}
