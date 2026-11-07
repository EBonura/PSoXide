//! Legacy transport and fault-order oracle. No PSX hardware or retail assets required.
#[path = "oracle/legacy.rs"]
mod legacy;
use psx_chainloader::{
    check_payload, Conflict, LoadedExe, Memory, Observer, Reader, Span, RAM_END, SECTOR_WORDS,
};
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
    /// The drive cannot prove its last stop (new transport only).
    Proof,
}
struct Cd {
    trace: Trace,
    header: [u32; 512],
    data: Vec<[u32; 512]>,
    fault: Fault,
    starts: usize,
    reads: usize,
    at: u32,
    /// Trace length at each proved stop: where in the sequence it happened.
    proofs: Vec<usize>,
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
    unsafe fn stop_proved(&mut self) -> bool {
        // Traced as a stop, so the legacy oracle still compares; where it
        // happened is recorded apart.
        self.trace.borrow_mut().push("stop".into());
        self.proofs.push(self.trace.borrow().len());
        !matches!(self.fault, Fault::Proof)
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
#[derive(Clone, Debug, PartialEq, Eq)]
struct Outcome {
    result: Result<LoadedExe, (u32, u32)>,
    trace: Vec<String>,
    ram: Vec<[u32; 512]>,
    /// Trace lengths at the proved stops. The legacy transport has none.
    proofs: Vec<usize>,
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
        proofs: Vec::new(),
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
        proofs: cd.proofs,
    }
}
/// Both transports must agree on everything but the proved stop, which only the
/// new one has: the result, the whole command trace and the RAM.
fn pair(size: usize, fault: Fault) -> Outcome {
    let old = run(true, size, fault, 0x80010000);
    let new = run(false, size, fault, 0x80010000);
    assert!(old.proofs.is_empty());
    let same = Outcome {
        proofs: Vec::new(),
        ..new.clone()
    };
    assert_eq!(old, same, "size={size} fault={fault:?}");
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
        assert_eq!(
            a,
            Outcome {
                proofs: Vec::new(),
                ..b.clone()
            }
        );
        assert!(
            b.result.is_ok(),
            "legacy accepts logical extent even without rounded/align gate"
        );
    }
}

/// A reader whose seek waits run `idle` three times, like a real one that spins.
struct Idling(Cd);
impl Reader for Idling {
    unsafe fn prepare(&mut self) -> bool {
        unsafe { self.0.prepare() }
    }
    unsafe fn start_read_seek_first(&mut self, lba: u32, limit: u32) -> bool {
        unsafe { self.0.start_read_seek_first(lba, limit) }
    }
    unsafe fn start_read_seek_first_idle(
        &mut self,
        lba: u32,
        limit: u32,
        idle: &mut dyn FnMut(),
    ) -> bool {
        for _ in 0..3 {
            idle();
        }
        unsafe { self.0.start_read_seek_first(lba, limit) }
    }
    unsafe fn read_sector(&mut self, out: &mut [u32; 512]) -> bool {
        unsafe { self.0.read_sector(out) }
    }
    unsafe fn stop(&mut self) {
        unsafe { self.0.stop() }
    }
    unsafe fn stop_proved(&mut self) -> bool {
        unsafe { self.0.stop_proved() }
    }
}
/// Counts ticks and logs them in the trace, in order with the other events.
struct Ticking(Events, Rc<RefCell<usize>>);
impl Observer for Ticking {
    fn stage_ok(&mut self, s: u32) {
        self.0.stage_ok(s);
    }
    fn progress(&mut self, n: u32, t: u32) {
        self.0.progress(n, t);
    }
    fn finish(&mut self) {
        self.0.finish();
    }
    fn tick(&mut self) {
        *self.1.borrow_mut() += 1;
        self.0 .0.borrow_mut().push("tick".into());
    }
}
fn ticking_run(size: usize) -> (Result<LoadedExe, (u32, u32)>, Vec<String>, usize) {
    let trace = Trace::default();
    let count = size.div_ceil(2048);
    let data: Vec<[u32; 512]> = (0..count)
        .map(|s| std::array::from_fn(|i| ((s * 512 + i) as u32).wrapping_mul(0x719337b)))
        .collect();
    let checksum = hash(
        data.iter()
            .flat_map(|s| s.iter().flat_map(|w| w.to_le_bytes()))
            .take(size),
    );
    let mut header = [0u32; 512];
    header[0] = 0x582d5350;
    header[1] = 0x45584520;
    header[4] = 0x80010000;
    header[5] = 0x80071234;
    header[6] = 0x80010000;
    header[7] = size as u32;
    header[12] = 0xfffffff0;
    header[13] = 0x30;
    let mut cd = Idling(Cd {
        trace: trace.clone(),
        header,
        data,
        fault: Fault::None,
        starts: 0,
        reads: 0,
        at: 0,
        proofs: Vec::new(),
    });
    let mut ram = Ram(vec![[0xdeadbeef; 512]; count.max(1)]);
    let ticks = Rc::new(RefCell::new(0));
    let mut events = Ticking(Events(trace.clone()), ticks.clone());
    let mut scratch = [u32::MAX; SECTOR_WORDS];
    // SAFETY: as in `run`: stable host RAM, no MMIO.
    let result = unsafe {
        psx_chainloader::try_load(
            &mut cd,
            24,
            &mut scratch,
            checksum,
            &mut ram,
            &mut events,
            0x801f0000,
        )
    };
    let n = *ticks.borrow();
    let lines = trace.borrow().clone();
    (result, lines, n)
}
#[test]
fn ticks_run_after_every_payload_sector_through_each_seek_and_the_checksum() {
    let sectors = 130usize;
    let (result, trace, ticks) = ticking_run(sectors * 2048);
    assert!(result.is_ok());
    // One tick per payload sector; three per seek (header seek plus one per
    // 64-sector chunk: 1 + 3 seeks); one per 4 KiB of checksum, at least
    // size / 4096 of them (the buffer's base alignment decides the exact count).
    let per_sector = sectors;
    let per_seek = 3 * 4;
    let per_sum_min = sectors * 2048 / 4096;
    assert!(
        ticks >= per_sector + per_seek + per_sum_min,
        "{ticks} ticks"
    );
    assert!(
        ticks <= per_sector + per_seek + per_sum_min + 1,
        "{ticks} ticks"
    );
    // The header read itself ticks nothing, and a tick follows each payload
    // read before the next one starts.
    let after_read: Vec<_> = trace
        .windows(2)
        .filter(|w| w[0].starts_with("read:") && w[0] != "read:24")
        .collect();
    assert_eq!(after_read.len(), sectors);
    assert!(after_read.iter().all(|w| w[1] == "tick"));
    // The tick must not disturb the transport: the same trace modulo ticks.
    let quiet = pair(sectors * 2048, Fault::None).trace;
    let without: Vec<_> = trace.into_iter().filter(|s| s != "tick").collect();
    assert_eq!(without, quiet);
}

#[test]
fn only_the_last_stop_is_proved_and_it_is_the_one_before_the_checksum() {
    // 130 sectors = chunks of 64, 64 and 2: three chunk stops, one proof.
    let x = pair(130 * 2048, Fault::None);
    let stops: Vec<usize> = x
        .trace
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == "stop")
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(stops.len(), 4, "header stop plus one per chunk");
    assert_eq!(
        x.proofs,
        [stops[3]],
        "only the final chunk's stop is proved"
    );
    // Nothing touches the drive after the proof but observer events.
    assert!(x.trace[x.proofs[0]..]
        .iter()
        .all(|s| s.starts_with("progress") || s == "finish" || s.starts_with("stage")));
}

#[test]
fn a_payload_of_one_exact_chunk_is_proved_once() {
    let x = pair(64 * 2048, Fault::None);
    assert_eq!(x.proofs.len(), 1);
    let x = pair(2048, Fault::None);
    assert_eq!(x.proofs.len(), 1);
}

#[test]
fn an_empty_payload_proves_the_stop_after_the_header() {
    let x = pair(0, Fault::None);
    assert_eq!(x.proofs.len(), 1, "the header stop is the last one");
}

#[test]
fn an_unproved_stop_fails_the_payload_stage_without_checksumming() {
    let x = run(false, 130 * 2048, Fault::Proof, 0x80010000);
    assert_eq!(x.result, Err((6, 130)));
    assert!(!x.trace.iter().any(|s| s == "finish" || s == "stage:6"));
    let x = run(false, 0, Fault::Proof, 0x80010000);
    assert_eq!(x.result, Err((6, 0)));
}

/// The demo disc's reservations: a 32 KiB loader at 0x801C8000 and the stack at
/// the top of RAM.
const DEMO_RESERVED: [Span; 2] = [
    Span::new(0x801C_8000, 0x801D_0000),
    Span::new(0x801F_8000, RAM_END),
];

#[test]
fn a_payload_clear_of_the_reserved_spans_is_accepted_below_between_and_above() {
    // Cortex-sized at the bottom, the Half-Life intro above the loader.
    assert_eq!(
        check_payload(0x8001_0000, 0x0011_5000, &DEMO_RESERVED),
        Ok(())
    );
    assert_eq!(check_payload(0x801E_0000, 75_776, &DEMO_RESERVED), Ok(()));
    // Ending exactly at a reserved span's start, or starting at its end, is clear.
    assert_eq!(
        check_payload(0x8001_0000, 0x1B_8000, &DEMO_RESERVED),
        Ok(())
    );
    assert_eq!(check_payload(0x801D_0000, 0x8000, &DEMO_RESERVED), Ok(()));
    // An empty payload occupies nothing, even inside a reserved span.
    assert_eq!(check_payload(0x801C_9000, 0, &DEMO_RESERVED), Ok(()));
}

#[test]
fn a_payload_that_touches_the_loader_or_the_stack_names_the_span() {
    assert_eq!(
        check_payload(0x8001_0000, 0x1B_8001, &DEMO_RESERVED),
        Err(Conflict::Reserved(DEMO_RESERVED[0]))
    );
    assert_eq!(
        check_payload(0x801C_7800, 4096, &DEMO_RESERVED),
        Err(Conflict::Reserved(DEMO_RESERVED[0]))
    );
    assert_eq!(
        check_payload(0x801E_0000, 0x1_9000, &DEMO_RESERVED),
        Err(Conflict::Reserved(DEMO_RESERVED[1]))
    );
}

#[test]
fn the_extent_counts_whole_sectors_not_the_logical_size() {
    // A payload of 2049 bytes writes two whole sectors, 4096 bytes.
    assert_eq!(check_payload(0x801C_7800, 2048, &DEMO_RESERVED), Ok(()));
    assert_eq!(
        check_payload(0x801C_7800, 2049, &DEMO_RESERVED),
        Err(Conflict::Reserved(DEMO_RESERVED[0])),
        "the second sector runs to 0x801C8800"
    );
}

#[test]
fn a_payload_outside_ram_is_refused() {
    assert_eq!(
        check_payload(0x8000_FFFF, 0, &[]),
        Err(Conflict::BelowFloor)
    );
    assert_eq!(
        check_payload(0x801F_F800, 4096, &[]),
        Err(Conflict::PastRam)
    );
    assert_eq!(
        check_payload(u32::MAX - 100, 4096, &[]),
        Err(Conflict::PastRam)
    );
    assert_eq!(
        check_payload(0x8001_0000, u32::MAX, &[]),
        Err(Conflict::PastRam)
    );
}

#[test]
fn try_load_reserving_loads_above_the_loader_and_refuses_an_overlap() {
    let run_at = |address: u32, size: usize| {
        let trace = Trace::default();
        let count = size.div_ceil(2048);
        let data: Vec<[u32; 512]> = (0..count)
            .map(|s| std::array::from_fn(|i| ((s * 512 + i) as u32).wrapping_mul(0x719337b)))
            .collect();
        let checksum = hash(
            data.iter()
                .flat_map(|s| s.iter().flat_map(|w| w.to_le_bytes()))
                .take(size),
        );
        let mut header = [0u32; 512];
        header[0] = 0x582d5350;
        header[1] = 0x45584520;
        header[4] = address;
        header[5] = 0x80071234;
        header[6] = address;
        header[7] = size as u32;
        header[12] = 0xfffffff0;
        header[13] = 0x30;
        let mut cd = Cd {
            trace: trace.clone(),
            header,
            data,
            fault: Fault::None,
            starts: 0,
            reads: 0,
            at: 0,
            proofs: Vec::new(),
        };
        let mut ram = Ram(vec![[0; 512]; count.max(1)]);
        let mut events = Events(trace.clone());
        let mut scratch = [u32::MAX; SECTOR_WORDS];
        // SAFETY: host RAM stands in for every address; no MMIO executes.
        let result = unsafe {
            psx_chainloader::try_load_reserving(
                &mut cd,
                24,
                &mut scratch,
                checksum,
                &mut ram,
                &mut events,
                &DEMO_RESERVED,
            )
        };
        let lines = trace.borrow().clone();
        (result, lines)
    };
    // The Half-Life intro: 75,776 bytes at 0x801E0000, above the loader.
    let (result, trace) = run_at(0x801E_0000, 75_776);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(trace.last().unwrap(), "stage:7");
    // The same program one sector lower would write the loader's last bytes.
    let (result, trace) = run_at(0x801C_F800, 75_776);
    assert_eq!(result, Err((5, 0x801C_F800)));
    assert_eq!(
        trace.last().unwrap(),
        "stage:4",
        "a bounds failure stops after the magic"
    );
}
