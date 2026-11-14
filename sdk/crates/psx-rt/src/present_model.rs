//! The present queue against a model of the handler and the GPU.
//!
//! [`Model`] is a [`Port`]: the thread-side code of `present.rs` runs on it
//! unchanged. Before each of that code's accesses (a boundary: one load or store
//! of a shared word, one kick, one register access) the model can run an event:
//! a VBlank (the handler's flip and kick, transliterated from the assembly in
//! `interrupts.rs`), the end of the running walk, or the end of the oldest
//! drawing. The tests put one event, and pairs of VBlanks, at every boundary of
//! each thread-side sequence, from several starting states, and check on every
//! transition that no part is kicked twice or into the buffer on screen, that
//! no flip shows a buffer the GPU is still drawing, that flips keep the cap,
//! and at the end that every frame was kicked and shown once, in order.

extern crate std;

use std::collections::VecDeque;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    VBlank,
    WalkEnd,
    DrawEnd,
    /// The GPU finishes everything it was sent: the walk, then all drawing.
    GpuIdle,
}

/// One unit of GPU work: a kicked part, a frame drawn through the port, or a
/// draw-done sequence.
#[derive(Clone, Copy, Debug)]
struct Work {
    /// The frame the work draws (`None`: the draw-done sequence alone).
    frame: Option<u32>,
    /// Ends on GP0(1Fh): bit 24 rises once it is drawn.
    raises: bool,
}

/// A part as the model knows it, by the head the thread hands over.
#[derive(Clone, Copy, Debug)]
struct Chain {
    frame: u32,
    last: bool,
}

const HEAD_BASE: u32 = 0x8010_0000;

fn head_of(index: usize) -> u32 {
    HEAD_BASE + 16 * index as u32
}

/// The buffer a frame draws into, and its display word.
fn buffer(frame: u32) -> u16 {
    (frame & 1) as u16
}

fn display_word(frame: u32) -> u32 {
    gp1::display_start(0, u32::from(buffer(frame)) * 256)
}

fn shown_buffer(word: u32) -> u16 {
    (((word >> 10) & 0x1ff) / 256) as u16
}

#[derive(Default)]
struct Model {
    // The shared words.
    head: u32,
    display: u32,
    flip: u32,
    hold: u32,
    min: u32,
    vblanks: u32,
    // The GPU.
    walking: Option<Work>,
    drawing: VecDeque<Work>,
    bit24: bool,
    shown: Option<u16>,
    /// The frame the flip word shows.
    flip_frame: Option<u32>,
    /// The frame `present_drawn` is about to queue.
    drawn_frame: Option<u32>,
    // What the thread handed over, and what happened to it.
    chains: Vec<Chain>,
    kicked: Vec<bool>,
    kick_order: Vec<usize>,
    shown_frames: Vec<u32>,
    last_flip: Option<u32>,
    recoveries: u32,
    // The schedule: events to run before boundary `n`.
    boundary: usize,
    schedule: Vec<(usize, Event)>,
    /// Consecutive boundaries that changed nothing (a wait spinning).
    idle: u32,
    violations: Vec<String>,
}

impl Model {
    fn new(min: u32) -> Model {
        Model {
            min,
            // `Presenter::new` raised bit 24 on an idle GPU.
            bit24: true,
            ..Model::default()
        }
    }

    fn violation(&mut self, what: String) {
        self.violations.push(what);
    }

    /// Run whatever is scheduled before this access; if the thread has been
    /// spinning, let the hardware make the next step of progress (the walk
    /// ends, the drawing ends, an edge comes), so a wait always ends.
    fn boundary(&mut self, changes: bool) {
        let at = self.boundary;
        self.boundary += 1;
        let due: Vec<Event> = self
            .schedule
            .iter()
            .filter(|(n, _)| *n == at)
            .map(|(_, e)| *e)
            .collect();
        for event in due {
            self.event(event);
        }
        if changes {
            self.idle = 0;
        } else {
            self.idle += 1;
            if self.idle > 4 {
                self.idle = 0;
                self.progress();
            }
        }
    }

    fn progress(&mut self) {
        if self.walking.is_some() {
            self.event(Event::WalkEnd);
        } else if !self.drawing.is_empty() {
            self.event(Event::DrawEnd);
        } else {
            self.event(Event::VBlank);
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::VBlank => self.vblank(),
            Event::WalkEnd => {
                if let Some(work) = self.walking.take() {
                    self.drawing.push_back(work);
                }
            }
            Event::DrawEnd => {
                if let Some(work) = self.drawing.pop_front() {
                    if work.raises {
                        self.bit24 = true;
                    }
                }
            }
            Event::GpuIdle => {
                self.event(Event::WalkEnd);
                while !self.drawing.is_empty() {
                    self.event(Event::DrawEnd);
                }
            }
        }
    }

    /// The handler's VBlank path: count, flip, kick, count the hold down.
    fn vblank(&mut self) {
        self.vblanks += 1;
        // The flip (interrupts.rs, the block at the flip word).
        if self.flip != 0 {
            let start = self.flip >> 24 == 5;
            if !(start && self.hold != 0) && self.bit24 {
                let word = self.flip;
                self.flip = 0;
                if start {
                    self.apply_flip(word);
                    self.hold = self.min;
                }
            }
        }
        // The kick (PSX_RT_PRESENT_KICK); the handler counts a waiting part
        // as a skip, which the model does not need.
        let _ = self.kick_body();
        if self.hold != 0 {
            self.hold -= 1;
        }
    }

    fn apply_flip(&mut self, word: u32) {
        let shown = shown_buffer(word);
        let frame = self.flip_frame.take();
        let busy = self
            .walking
            .iter()
            .chain(self.drawing.iter())
            .any(|w| w.frame.is_some_and(|f| buffer(f) == shown));
        if busy {
            self.violation(format!(
                "flip to buffer {shown} while the GPU draws into it"
            ));
        }
        if let Some(last) = self.last_flip {
            if self.vblanks.wrapping_sub(last) < self.min {
                self.violation(format!(
                    "flip {} VBlanks after the last, under the cap {}",
                    self.vblanks.wrapping_sub(last),
                    self.min
                ));
            }
        }
        self.last_flip = Some(self.vblanks);
        self.shown = Some(shown);
        if let Some(frame) = frame {
            self.shown_frames.push(frame);
        }
    }

    /// The kick, shared by the handler and the thread. True when it kicked.
    fn kick_body(&mut self) -> bool {
        if self.head == 0 || self.flip != 0 || self.walking.is_some() {
            return false;
        }
        let index = ((self.head - HEAD_BASE) / 16) as usize;
        let chain = self.chains[index];
        if self.kicked[index] {
            self.violation(format!("part {index} kicked twice"));
        }
        self.kicked[index] = true;
        self.kick_order.push(index);
        if self.shown == Some(buffer(chain.frame)) {
            self.violation(format!(
                "part {index} of frame {} kicked into the buffer on screen",
                chain.frame
            ));
        }
        // Acknowledge, start the walk, move the display word, empty the slot.
        self.bit24 = false;
        self.walking = Some(Work {
            frame: Some(chain.frame),
            raises: chain.last,
        });
        self.flip = self.display;
        if chain.last {
            self.flip_frame = Some(chain.frame);
        }
        self.head = 0;
        true
    }

    /// A part the scenario is about to submit: register its head.
    fn part(&mut self, frame: u32, last: bool) -> u32 {
        self.chains.push(Chain { frame, last });
        self.kicked.push(false);
        head_of(self.chains.len() - 1)
    }

    /// The thread draws frame `frame` through the port.
    fn draw_direct(&mut self, frame: u32) {
        self.boundary(true);
        if self.shown == Some(buffer(frame)) {
            self.violation(format!("frame {frame} drawn into the buffer on screen"));
        }
        self.drawing.push_back(Work {
            frame: Some(frame),
            raises: false,
        });
    }

    /// Run the hardware until nothing is in flight.
    fn settle(&mut self) {
        for _ in 0..200 {
            if self.head == 0 && self.flip == 0 && self.walking.is_none() && self.drawing.is_empty()
            {
                return;
            }
            self.progress();
        }
        self.violation(String::from("never settled: a part or flip stranded"));
    }
}

impl Port for Model {
    fn head(&mut self) -> u32 {
        self.boundary(false);
        self.head
    }

    fn set_display(&mut self, word: u32) {
        self.boundary(true);
        if self.head != 0 {
            self.violation(String::from("display word stored with the slot full"));
        }
        self.display = word;
    }

    fn set_head(&mut self, head: u32) {
        self.boundary(true);
        if self.head != 0 {
            self.violation(String::from("head stored over a part not kicked"));
        }
        self.head = head;
    }

    fn flip(&mut self) -> u32 {
        self.boundary(false);
        self.flip
    }

    fn set_flip(&mut self, word: u32) {
        self.boundary(true);
        if self.flip != 0 {
            self.violation(format!("flip word {word:#x} stored over {:#x}", self.flip));
        }
        self.flip = word;
        if let Some(frame) = self.drawn_frame.take() {
            self.flip_frame = Some(frame);
        }
    }

    fn take_flip(&mut self) -> u32 {
        self.boundary(true);
        core::mem::take(&mut self.flip)
    }

    fn kick(&mut self) {
        self.boundary(false);
        if self.kick_body() {
            self.idle = 0;
        }
    }

    fn is_channel_busy(&mut self) -> bool {
        self.boundary(false);
        self.walking.is_some()
    }

    fn is_draw_done(&mut self) -> bool {
        self.boundary(false);
        self.bit24
    }

    fn vblanks(&mut self) -> u32 {
        self.boundary(false);
        self.vblanks
    }

    fn acknowledge_draw_done(&mut self) {
        self.boundary(true);
        self.bit24 = false;
    }

    fn signal_draw_done(&mut self) {
        self.boundary(true);
        // `kick_draw_done_list`: wait out a walk, then the list, whose walk is
        // waited out too (it is a few words).
        while self.walking.is_some() {
            self.boundary(false);
        }
        self.drawing.push_back(Work {
            frame: None,
            raises: true,
        });
    }

    fn stop_walk_and_raise(&mut self) {
        self.boundary(true);
        self.recoveries += 1;
        self.walking = None;
        self.drawing.push_back(Work {
            frame: None,
            raises: true,
        });
    }
}

/// What a scenario's thread does, frame by frame.
#[derive(Clone, Copy, Debug)]
enum Frame {
    /// One chain.
    Chain,
    /// Two chains: `More` then `Final`.
    TwoParts,
    /// Drawn through the port, then `present_drawn`.
    Drawn,
}

/// Run `frames` on a model with `min` as the cap, `schedule` injected, and
/// return the model after it settles.
fn run(frames: &[Frame], min: u32, schedule: &[(usize, Event)]) -> Model {
    let mut m = Model::new(min);
    m.schedule = schedule.to_vec();
    // The guard is armed after a submit or a drawn frame; the thread's next
    // direct access runs `wait_idle_on`.
    let mut armed = false;
    for (index, frame) in frames.iter().enumerate() {
        let f = index as u32;
        match frame {
            Frame::Chain | Frame::TwoParts => {
                wait_arena_free_on(&mut m);
                if matches!(frame, Frame::TwoParts) {
                    let head = m.part(f, false);
                    wait_slot_empty_on(&mut m);
                    submit_on(&mut m, head, 0);
                }
                let head = m.part(f, true);
                wait_slot_empty_on(&mut m);
                submit_on(&mut m, head, display_word(f));
                armed = true;
            }
            Frame::Drawn => {
                if armed {
                    wait_idle_on(&mut m);
                }
                m.draw_direct(f);
                // `present_drawn`'s own check, then the rest.
                if m.flip != 0 || m.head != 0 {
                    wait_idle_on(&mut m);
                }
                m.drawn_frame = Some(f);
                present_drawn_on(&mut m, display_word(f));
                armed = true;
            }
        }
    }
    m.settle();
    m
}

fn check(m: &Model, frames: &[Frame], what: &str) {
    assert!(m.violations.is_empty(), "{what}: {:?}", m.violations);
    assert!(
        m.kicked.iter().all(|&k| k),
        "{what}: a part was never kicked"
    );
    let expected: Vec<u32> = (0..frames.len() as u32).collect();
    assert_eq!(m.shown_frames, expected, "{what}: frames shown");
    assert_eq!(m.recoveries, 0, "{what}: a stall was recovered");
    // Parts are kicked in the order they were submitted.
    let mut order = m.kick_order.clone();
    order.sort_unstable();
    assert_eq!(order, m.kick_order, "{what}: kick order");
}

/// The boundaries a scenario passes with no injected event.
fn boundaries(frames: &[Frame], min: u32) -> usize {
    run(frames, min, &[]).boundary
}

const SCENARIOS: &[&[Frame]] = &[
    &[Frame::Chain, Frame::Chain, Frame::Chain, Frame::Chain],
    &[
        Frame::TwoParts,
        Frame::TwoParts,
        Frame::Chain,
        Frame::TwoParts,
    ],
    &[Frame::Drawn, Frame::Drawn, Frame::Drawn],
    &[
        Frame::Chain,
        Frame::Drawn,
        Frame::TwoParts,
        Frame::Drawn,
        Frame::Chain,
    ],
];

#[test]
fn every_scenario_runs_clean_without_injection() {
    for frames in SCENARIOS {
        for min in [1, 2, 3] {
            let m = run(frames, min, &[]);
            check(&m, frames, &format!("{frames:?} cap {min}"));
        }
    }
}

#[test]
fn one_event_at_every_boundary() {
    for frames in SCENARIOS {
        for min in [1, 2] {
            let n = boundaries(frames, min) + 8;
            for at in 0..n {
                for event in [
                    Event::VBlank,
                    Event::WalkEnd,
                    Event::DrawEnd,
                    Event::GpuIdle,
                ] {
                    let m = run(frames, min, &[(at, event)]);
                    check(
                        &m,
                        frames,
                        &format!("{frames:?} cap {min} {event:?} at {at}"),
                    );
                }
            }
        }
    }
}

#[test]
fn two_vblanks_at_every_pair_of_boundaries() {
    for frames in SCENARIOS {
        let n = boundaries(frames, 2) + 8;
        for first in 0..n {
            for second in first..n {
                let m = run(
                    frames,
                    2,
                    &[(first, Event::VBlank), (second, Event::VBlank)],
                );
                check(
                    &m,
                    frames,
                    &format!("{frames:?} VBlanks at {first} and {second}"),
                );
            }
        }
    }
}

#[test]
fn a_vblank_and_the_gpu_at_every_pair_of_boundaries() {
    // A VBlank at one boundary and the walk or the drawing ending at another,
    // in either order: the kick racing the thread's slot stores with the
    // channel going idle in between.
    for frames in SCENARIOS {
        let n = boundaries(frames, 1) + 8;
        for first in 0..n {
            for second in 0..n {
                for gpu in [Event::WalkEnd, Event::DrawEnd, Event::GpuIdle] {
                    let m = run(frames, 1, &[(first, Event::VBlank), (second, gpu)]);
                    check(
                        &m,
                        frames,
                        &format!("{frames:?} VBlank at {first}, {gpu:?} at {second}"),
                    );
                }
            }
        }
    }
}

#[test]
fn a_frame_is_shown_without_waiting_for_its_successor() {
    // One chain frame and nothing after it: drawn, then shown at the next edge.
    let mut m = Model::new(2);
    let head = m.part(0, true);
    wait_slot_empty_on(&mut m);
    submit_on(&mut m, head, display_word(0));
    assert_eq!(m.head, 0, "kicked on the thread at once");
    m.event(Event::WalkEnd);
    m.event(Event::DrawEnd);
    m.event(Event::VBlank);
    assert_eq!(m.shown_frames, [0]);
    assert!(m.violations.is_empty(), "{:?}", m.violations);
}

#[test]
fn a_part_waits_for_the_flip_before_it() {
    // Frame 0 drawn but its flip not landed: frame 1 stays in the slot until
    // the edge, then the handler flips 0 and kicks 1 in the same pass.
    let mut m = Model::new(1);
    let head = m.part(0, true);
    submit_on(&mut m, head, display_word(0));
    m.event(Event::WalkEnd);
    m.event(Event::DrawEnd);
    let head = m.part(1, true);
    submit_on(&mut m, head, display_word(1));
    assert_eq!(m.head, head, "frame 1 waits for frame 0's flip");
    m.event(Event::VBlank);
    assert_eq!(m.head, 0);
    assert_eq!(m.shown_frames, [0]);
    assert!(m.violations.is_empty(), "{:?}", m.violations);
}

#[test]
fn the_cap_holds_a_finished_frame() {
    let mut m = Model::new(2);
    for f in 0..3u32 {
        let head = m.part(f, true);
        wait_slot_empty_on(&mut m);
        submit_on(&mut m, head, display_word(f));
        m.event(Event::WalkEnd);
        m.event(Event::DrawEnd);
        m.event(Event::VBlank);
    }
    m.settle();
    assert_eq!(m.shown_frames, [0, 1, 2]);
    assert!(m.violations.is_empty(), "{:?}", m.violations);
}

#[test]
fn cancel_flip_takes_the_word_whole() {
    let mut m = Model::new(1);
    let head = m.part(0, true);
    submit_on(&mut m, head, display_word(0));
    assert_eq!(m.take_flip(), display_word(0));
    assert_eq!(m.flip, 0);
}
