//! Behavioral ordered-stream tests with delayed fake DMA consumption.
use psx_gpu::ordered::{CommandStreamDma, OrderedCommandStream};
use std::{cell::RefCell, rc::Rc};
#[derive(Default)]
struct State {
    pending: Vec<(*const u32, Vec<u32>)>,
    output: Vec<u32>,
    nodes: Vec<usize>,
    waits: usize,
    syncs: usize,
    polls: usize,
    complete_on_poll: bool,
}
impl State {
    fn finish(&mut self) {
        for (ptr, original) in self.pending.drain(..) {
            let now = unsafe { std::slice::from_raw_parts(ptr, original.len()) };
            assert_eq!(now, original, "in-flight node mutated before completion");
            self.output.extend_from_slice(&original[1..]);
        }
    }
}
#[derive(Clone)]
struct Dma(Rc<RefCell<State>>);
impl CommandStreamDma for Dma {
    fn busy(&mut self) -> bool {
        let mut s = self.0.borrow_mut();
        s.polls += 1;
        if s.complete_on_poll {
            s.finish();
        }
        !s.pending.is_empty()
    }
    unsafe fn submit(&mut self, mut p: *const u32) {
        let mut s = self.0.borrow_mut();
        assert!(s.pending.is_empty(), "submitted over busy DMA");
        loop {
            let tag = unsafe { *p };
            let n = (tag >> 24) as usize;
            assert!((1..=15).contains(&n));
            s.nodes.push(n);
            s.pending
                .push((p, unsafe { std::slice::from_raw_parts(p, n + 1) }.to_vec()));
            let link = tag & 0xffffff;
            if link == 0xffffff {
                break;
            }
            let delta = link.wrapping_sub(p as u32 & 0xffffff) & 0xffffff;
            assert!(delta > 0 && delta % 4 == 0);
            p = unsafe { p.add(delta as usize / 4) };
        }
    }
    fn wait(&mut self) {
        let mut s = self.0.borrow_mut();
        s.waits += 1;
        s.finish();
    }
    fn draw_sync(&mut self) {
        let mut s = self.0.borrow_mut();
        assert!(s.pending.is_empty());
        s.syncs += 1;
    }
}
fn stream(cap: usize, complete: bool) -> (OrderedCommandStream<Dma>, Rc<RefCell<State>>) {
    let s = Rc::new(RefCell::new(State {
        complete_on_poll: complete,
        ..Default::default()
    }));
    (
        OrderedCommandStream::with_dma(Box::leak(vec![0; cap].into_boxed_slice()), Dma(s.clone())),
        s,
    )
}
#[test]
fn capacity_reuse_and_async_completion_preserve_order() {
    for capacity in [17, 18, 31, 32, 33, 64, 8192] {
        for complete in [false, true] {
            let (mut list, s) = stream(capacity, complete);
            let mut expected = Vec::new();
            for n in 0..1500u32 {
                let p = [0x60000000 | n, n, n + 1];
                list.push_packet(p);
                expected.extend(p);
                if n % 3 == 0 {
                    let p = [0xe1000000 | n];
                    list.push_packet(p);
                    expected.extend(p);
                }
                if n % 19 == 0 {
                    list.submit();
                }
                if n % 211 == 0 {
                    list.draw_sync();
                }
            }
            list.draw_sync();
            assert_eq!(
                s.borrow().output,
                expected,
                "capacity{capacity} complete{complete}"
            );
        }
    }
}
#[test]
fn exactly_full_packet_retains_spare_tag_and_drop_waits_after_move() {
    let (mut list, s) = stream(17, false);
    list.push_packet([7; 15]);
    list.submit();
    let moved = Some(list);
    drop(moved);
    assert_eq!(s.borrow().output, [7; 15]);
    assert!(s.borrow().waits > 0);
}
#[test]
fn mixed_uploads_and_immediate_fences_preserve_payload() {
    let (mut list, s) = stream(64, false);
    list.push_packet([0xe100040a]);
    list.push_upload(3, 4, 3, 1, &[1, 2, 3]);
    list.push_packet([0x680000ff, 7]);
    list.draw_sync();
    assert_eq!(
        s.borrow().output,
        [0xe100040a, 0xa0000000, 0x00040003, 0x00010003, 0x00020001, 3, 0x680000ff, 7]
    );
    assert_eq!(s.borrow().syncs, 1);
    list.push_packet([0xe2000000]);
    list.draw_sync();
    assert_eq!(s.borrow().output.last(), Some(&0xe2000000));
}
#[test]
#[should_panic(expected = "1..=15")]
fn rejects_oversize_whole_packet() {
    let (mut l, _) = stream(32, false);
    l.push_packet([0; 16]);
}
#[test]
#[should_panic]
fn rejects_mismatched_upload_rectangle() {
    let (mut l, _) = stream(32, false);
    l.push_upload(0, 0, 2, 2, &[0; 3]);
}

mod fixture {
    include!("fixtures/ordered_frame.rs");
    pub fn packets() -> &'static [&'static [u32]] {
        FRAME
    }
}
#[test]
fn captured_frame_preserves_all_264_packets_and_995_words() {
    let (mut list, state) = stream(8192, false);
    let mut expected = Vec::new();
    for packet in fixture::packets() {
        macro_rules! push {
            ($n:literal) => {
                list.push_packet::<$n>((*packet).try_into().unwrap())
            };
        }
        match packet.len() {
            1 => push!(1),
            2 => push!(2),
            3 => push!(3),
            4 => push!(4),
            5 => push!(5),
            6 => push!(6),
            7 => push!(7),
            8 => push!(8),
            9 => push!(9),
            10 => push!(10),
            11 => push!(11),
            12 => push!(12),
            _ => panic!("invalid fixture"),
        }
        expected.extend_from_slice(packet);
    }
    list.draw_sync();
    assert_eq!(fixture::packets().len(), 264);
    assert_eq!(expected.len(), 995);
    assert_eq!(state.borrow().output, expected);
}
