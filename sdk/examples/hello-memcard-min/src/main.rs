//! `hello-memcard-min` -- an interactive memory-card protocol diagnostic,
//! built to chase a real-hardware bug `hello-memcard`'s automatic boot-time
//! round-trip can't isolate: on real silicon, saves fail with
//! `Error::Protocol`, consistently, in both card slots, with two different
//! cards independently confirmed good (files visible and readable in the
//! console's own card manager).
//!
//! Since the pad (also SIO0) reads reliably on the same hardware, the SIO0
//! wiring and controller sub-protocol are not the suspect -- this points at
//! `psx_mc::sio`'s card-specific timing (`Timing::setup_spins`/`byte_spins`/
//! `ack_spins`), which the doc comment on that module already flags as
//! "conservative starting points pending console validation".
//!
//! Console captures (2026-07-26) then showed something a single probe can't
//! reveal: repeated probes *at the same fixed `Timing` preset* flip
//! individual fields between correct and wrong from one attempt to the next
//! -- not a consistent failure a bigger spin count would reliably fix, but
//! per-attempt noise. That ruled out "the constant is too small" as the
//! whole story and motivated [`ProbeStats`]: a running per-field pass rate
//! over many probes at the current slot/timing, so the picture is a
//! percentage instead of one anecdotal sample.
//!
//! A full sweep (10 presets, ~1900 probes, every `Timing` bound spanning a
//! 1000x range) then came back with zero full-frame successes anywhere,
//! which reads less like a mistimed constant and more like a wrong
//! mechanism. `psx_pad`'s own `active_ctrl` doc comment already records that
//! arming `CTRL_ACK_IRQ_EN` -- which `AckMode::Irq` does on every byte --
//! "turned out to disturb real-hardware transfers" on this exact console
//! family, which is why the pad driver's default poll avoids it entirely.
//! The `NOACK` presets test the same avoidance for cards ([`AckMode::NoAck`]).
//! Console captures (2026-07-26): `interbyte_spins = 8_000` hit 24/24 probes,
//! every field, 100% -- the first configuration in the whole investigation
//! to read a real frame perfectly and repeatably.
//!
//! That same setting then failed a real save with `NoCard`, immediately --
//! not `Protocol`, not intermittent. `probe_read` is one isolated
//! transaction per button press, with a large real-world gap (human
//! reaction time, frame boundaries) before the next one; `Card::write`
//! issues several `read_frame`/`write_frame` calls back-to-back with
//! *no* such gap (directory scan, header, icon, one write per allocated
//! block -- see `psx_mc::fs`). `NoCard` this immediately reads as the card
//! not having reset its receiver before the very next transaction
//! re-selects it. `AckMode::NoAck` gained `deselect_spins` -- delay
//! applied right after deselecting -- to test exactly that gap; the
//! `DSEL` presets sweep it at `interbyte_spins = 8_000`, and `DSEL+32K`/
//! `DSEL+100K` both then landed real, successful saves in-game.
//!
//! Console report immediately after: skillscape's save doesn't appear in
//! the BIOS card manager, and *other, pre-existing saves on the same card
//! stopped appearing too* -- while skillscape's own save/load still works
//! through this same driver. That combination (self-consistent to us,
//! invisible/destructive to everyone else) is the signature of a save
//! landing at the wrong physical frame. Every probe run so far, across
//! every preset, tested frame 0 only -- address bytes `0x00, 0x00`. A real
//! save writes at dozens of *non-zero* frame numbers (the directory,
//! title/icon headers, every data block), and TX of a non-zero byte has
//! never once been exercised. `Card::write_frame` also structurally cannot
//! detect a corrupted address: the real memory-card protocol does not echo
//! the address back on writes (only reads do), so a mistransmitted address
//! byte fails silently -- the card reports success for whatever frame it
//! actually received, just not the one that was meant. [`SWEEP_FRAMES`]
//! exists to catch this *before* another real write: it is read-only
//! (never risks card contents) and checks the address-echo `read_frame`
//! *does* get, across frame numbers spanning the whole card, at whichever
//! preset is selected. Real saving is disabled ([`REAL_SAVE_ENABLED`])
//! until a sweep comes back clean.
//!
//! Controls, all live -- nothing reruns automatically, so every combination
//! below can be tried in one boot without reburning a disc:
//! - CROSS increments a counter, shown centered on screen (unused while
//!   real saving is disabled; kept for when it's re-enabled).
//! - SELECT toggles the target card slot (1 or 2) and resets [`ProbeStats`].
//! - UP/DOWN/LEFT/RIGHT move [`ProbeStats`]'s target frame (Circle probes
//!   this frame, not always frame 0 -- needed to test non-zero addresses).
//! - L1 / R1 cycle through [`PRESETS`] -- different `Timing` spin counts --
//!   and also reset [`ProbeStats`].
//! - HOLD CIRCLE repeats a raw, non-destructive protocol probe of the
//!   current target frame as fast as each probe completes, accumulating
//!   [`ProbeStats`] and showing the most recent probe's raw bytes
//!   colour-coded against what the protocol expects. Read-only.
//! - START runs [`run_addr_sweep`] -- read-only, checks address-echo
//!   integrity across [`SWEEP_FRAMES`] at the selected slot/timing -- while
//!   [`REAL_SAVE_ENABLED`] is `false`. Flipping that back to `true` (only
//!   after a sweep reads clean) restores the original behaviour: a real
//!   save of the counter to the selected slot.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_mc::{Block, Card, Error, HardwareCard, ReadDiag, Slot, Timing, FRAME_SIZE};
use psx_pad::{button, poll_port1, PadTracker};
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const NAME: &str = "BASLUS-00001MCMIN"; // <= psx_mc::MAX_NAME (20) chars
const TITLE: &str = "MC MIN TEST";

const WHITE: (u8, u8, u8) = (230, 230, 240);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);
const GREY: (u8, u8, u8) = (150, 150, 160);
const YELLOW: (u8, u8, u8) = (230, 210, 80);

// Wire-protocol constants mirrored from `psx_mc::sio` (private there) so the
// probe's byte dump can colour actual-vs-expected without re-deriving them.
const EXPECT_ID1: u8 = 0x5A;
const EXPECT_ID2: u8 = 0x5D;
const EXPECT_ACK1: u8 = 0x5C;
const EXPECT_ACK2: u8 = 0x5D;

/// RE-ENABLED (2026-07-26) -- console captures cleared both suspects: the
/// address-echo sweep came back 32/32 on both slots, and [`run_data_test`]'s
/// full 128-byte write+read-back round trip came back byte-perfect on slot
/// 1 four times running (`NOACK 8K DSEL+32K`). Slot 2 failed that same
/// round trip twice (`NoCard` on the immediate read-after-write, despite
/// passing the address sweep) -- looks like it needs more deselect margin
/// than slot 1, not another burst-corruption bug, but hasn't been
/// re-validated for real saving. Trust slot 1 first.
const REAL_SAVE_ENABLED: bool = true;

/// Frames to check for address-echo integrity: the whole directory area
/// (the `MC` header plus all 15 directory entries) and every data block's
/// first frame (where a save's title/icon header lives -- the two frames a
/// real `Card::write` always touches, whatever else it does). Read-only.
const SWEEP_FRAMES: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 63, 64, 128, 192, 256, 320, 384, 448,
    512, 576, 640, 704, 768, 832, 896, 960,
];
const EXPECT_END: u8 = 0x47;

/// Scratch frame for [`run_data_test`]: block 15 frame 0, the very last
/// block on the card and, on a blank/formatted card, not linked from any
/// directory entry -- this test writes through `Block` directly, bypassing
/// `Card`'s filesystem layer entirely, so nothing but this one frame is
/// ever touched no matter what the result is.
const SCRATCH_FRAME: u16 = 960;

/// A fixed, easy-to-verify 128-byte pattern (not all-zero, not all one
/// value, no byte repeated nearby) -- a stuck bit or an off-by-N shift
/// shows up immediately in the byte-for-byte comparison.
fn test_pattern() -> [u8; FRAME_SIZE] {
    let mut p = [0u8; FRAME_SIZE];
    for (i, b) in p.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(41).wrapping_add(7);
    }
    p
}

/// Which SIO0 pacing mechanism (`psx_mc::AckMode`) a preset drives
/// `HardwareCard` with -- see the module doc for why there are two.
#[derive(Copy, Clone)]
enum PresetKind {
    /// `AckMode::Irq`: per-byte `/ACK` IRQ-latch + DSR-wait + CTRL.ACK pulse
    /// (the original mechanism).
    Irq(Timing),
    /// `AckMode::NoAck`: no `CTRL_ACK_IRQ_EN`, no per-byte handshake -- a
    /// fixed `interbyte_spins` delay after every byte, matching `psx_pad`'s
    /// real-hardware-proven default pacing, plus `deselect_spins` after
    /// deselecting (see the module doc -- this is what a real `Card::write`
    /// needs that an isolated `probe_read` never exercised).
    NoAck {
        setup_spins: u32,
        byte_spins: u32,
        interbyte_spins: u32,
        deselect_spins: u32,
        write_gap_spins: u32,
    },
}

struct Preset {
    name: &'static str,
    kind: PresetKind,
}

/// Timing/pacing combinations to sweep against real hardware. The first ten
/// widen `Timing` bounds well past what the emulator needs (`DEFAULT` is
/// `Timing::default()`), to test "does it just need more time". The `NOACK`
/// presets test a different mechanism entirely -- see the module doc.
const PRESETS: [Preset; 21] = [
    Preset {
        name: "DEFAULT",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            ack_spins: 200_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "SLOW SETUP",
        kind: PresetKind::Irq(Timing {
            setup_spins: 16_384,
            byte_spins: 32_768,
            ack_spins: 200_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "SLOW BYTE",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 262_144,
            ack_spins: 200_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "SLOW ACK",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            ack_spins: 1_600_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "ALL SLOW",
        kind: PresetKind::Irq(Timing {
            setup_spins: 16_384,
            byte_spins: 262_144,
            ack_spins: 1_600_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "HUGE",
        kind: PresetKind::Irq(Timing {
            setup_spins: 131_072,
            byte_spins: 1_048_576,
            ack_spins: 8_000_000,
            end_delay_spins: 0,
        }),
    },
    Preset {
        name: "END DELAY 4K",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            ack_spins: 200_000,
            end_delay_spins: 4_000,
        }),
    },
    Preset {
        name: "END DELAY 50K",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            ack_spins: 200_000,
            end_delay_spins: 50_000,
        }),
    },
    Preset {
        name: "END DELAY 500K",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 32_768,
            ack_spins: 200_000,
            end_delay_spins: 500_000,
        }),
    },
    Preset {
        name: "BYTE+END DELAY",
        kind: PresetKind::Irq(Timing {
            setup_spins: 1_024,
            byte_spins: 262_144,
            ack_spins: 200_000,
            end_delay_spins: 50_000,
        }),
    },
    // Console captures across all ten presets above: zero full-frame
    // successes over ~1900 probes, and both "add end_delay" variants made
    // END *worse* (flat 0% vs 15% baseline) -- more waiting never helped.
    // These five drop the `/ACK` IRQ mechanism entirely instead of retuning
    // it, matching `psx_pad`'s own proven-working recipe.
    Preset {
        // Exactly psx_pad's default poll: setup=1024, byte=32768 (its
        // `EXCHANGE_WAIT_SPINS`), zero extra inter-byte delay.
        name: "NOACK (=PAD)",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 0,
            deselect_spins: 0,
            write_gap_spins: 0,
        },
    },
    Preset {
        name: "NOACK +2K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 2_000,
            deselect_spins: 0,
            write_gap_spins: 0,
        },
    },
    Preset {
        // Console captures (2026-07-26): 24/24 probes, every field, 100% --
        // the first configuration in the whole investigation to read a real
        // frame perfectly and repeatably. A real save at this exact timing
        // still failed with `NoCard`, immediately -- see the `DSEL` presets
        // below, which target the gap `probe_read` never exercised.
        name: "NOACK +8K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 0,
            write_gap_spins: 0,
        },
    },
    Preset {
        name: "NOACK +32K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 32_000,
            deselect_spins: 0,
            write_gap_spins: 0,
        },
    },
    Preset {
        name: "NOACK SLOW SETUP",
        kind: PresetKind::NoAck {
            setup_spins: 16_384,
            byte_spins: 32_768,
            interbyte_spins: 2_000,
            deselect_spins: 0,
            write_gap_spins: 0,
        },
    },
    // `probe_read` is one isolated transaction per button press, with a
    // large real-world gap before the next one (human reaction time, frame
    // boundaries). `Card::write` issues several `read_frame`/`write_frame`
    // calls back-to-back with *no* such gap (directory scan, header, icon,
    // one write per allocated block). `NOACK +8K` reads perfectly in
    // isolation but a real save fails with `NoCard` immediately -- these
    // four hold `interbyte_spins` at that proven-good 8K and sweep
    // `deselect_spins` instead, to give the card time to reset its
    // receiver between transactions the way a lone probe always had for
    // free. Use these for BOTH probing (safe, non-destructive) and saving.
    Preset {
        name: "NOACK 8K DSEL+4K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 4_000,
            write_gap_spins: 0,
        },
    },
    Preset {
        name: "NOACK 8K DSEL+8K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 8_000,
            write_gap_spins: 0,
        },
    },
    Preset {
        name: "NOACK 8K DSEL+32K",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 32_000,
            write_gap_spins: 0,
        },
    },
    Preset {
        // Real save succeeded (Card::write returned Ok) but never appeared
        // in the BIOS card manager -- see the module doc and
        // `AckMode::NoAck::write_gap_spins`. PSn00bSDK's reference card
        // driver (added to the repo 2026-07-26) documents "wait at least
        // two vsyncs between each sector write" -- ~33ms, which nothing
        // above ever waited between the several back-to-back sector writes
        // a real `Card::write` issues (directory entry, title, icon, each
        // data block). These four hold DSEL+100K (proven for
        // transaction-to-transaction reset) and add write_gap_spins on top,
        // specifically to let each sector's flash commit finish before the
        // next write starts. Use these for a real save, then check the
        // BIOS card manager -- that's the only way to see this one land.
        name: "NOACK 8K DSEL100K WGAP1M",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 100_000,
            write_gap_spins: 1_000_000,
        },
    },
    Preset {
        name: "NOACK 8K DSEL100K WGAP2M",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 100_000,
            write_gap_spins: 2_000_000,
        },
    },
    Preset {
        name: "NOACK 8K DSEL100K WGAP4M",
        kind: PresetKind::NoAck {
            setup_spins: 1_024,
            byte_spins: 32_768,
            interbyte_spins: 8_000,
            deselect_spins: 100_000,
            write_gap_spins: 4_000_000,
        },
    },
];

fn slot_name(s: Slot) -> &'static str {
    match s {
        Slot::One => "1",
        Slot::Two => "2",
    }
}

fn error_name(e: Error) -> &'static str {
    match e {
        Error::NoCard => "NoCard",
        Error::Protocol => "Protocol",
        Error::BadChecksum => "BadChecksum",
        Error::OutOfRange => "OutOfRange",
        Error::NotFormatted => "NotFormatted",
        Error::NotFound => "NotFound",
        Error::NoSpace => "NoSpace",
        Error::Exists => "Exists",
        Error::Corrupt => "Corrupt",
        Error::BufferTooSmall => "BufferTooSmall",
        Error::BadContainer => "BadContainer",
        Error::Compression => "Compression",
        Error::BadName => "BadName",
    }
}

/// Build the `HardwareCard` a preset calls for -- `AckMode::Irq` via
/// `with_timing`, `AckMode::NoAck` via `with_noack`. Shared by the probe and
/// save triggers so both always exercise the exact same pacing.
fn card_for_preset(preset: &Preset, slot: Slot) -> HardwareCard {
    match preset.kind {
        PresetKind::Irq(timing) => HardwareCard::with_timing(slot, timing),
        PresetKind::NoAck {
            setup_spins,
            byte_spins,
            interbyte_spins,
            deselect_spins,
            write_gap_spins,
        } => HardwareCard::with_noack(
            slot,
            setup_spins,
            byte_spins,
            interbyte_spins,
            deselect_spins,
            write_gap_spins,
        ),
    }
}

/// Attempt one save: format if blank, then write the counter.
/// Blocking -- the SIO0 transport spin-waits on every byte.
fn save_with_card(card: HardwareCard, counter: u32) -> Result<(), Error> {
    let mut card = Card::new(card);
    if !card.is_formatted()? {
        card.format()?;
    }
    card.write(NAME, TITLE, &counter.to_le_bytes())
}

/// Result of one [`run_addr_sweep`] pass over [`SWEEP_FRAMES`].
#[derive(Copy, Clone, Default)]
struct AddrSweep {
    tested: u32,
    /// Card responded with a real ID byte at all (sanity: rules out
    /// "no card"/dead transaction from "wrong address").
    id_ok: u32,
    /// `emsb == msb && elsb == lsb` -- the card confirms it received the
    /// address we actually sent.
    addr_ok: u32,
    /// The first frame whose address echo didn't match, and what it
    /// echoed instead, for diagnosis.
    first_bad: Option<(u16, u8, u8)>,
}

/// Read-only sweep: `probe_read` every frame in [`SWEEP_FRAMES`] and check
/// the card's address echo against what was actually requested. Never
/// writes anything -- safe to run as many times as needed. See the module
/// doc for why this exists before real saving is trusted again.
fn run_addr_sweep(preset: &Preset, slot: Slot) -> AddrSweep {
    let mut s = AddrSweep::default();
    for &frame in SWEEP_FRAMES.iter() {
        let mut card = card_for_preset(preset, slot);
        let (_, diag) = card.probe_read(frame);
        s.tested += 1;
        if diag.id1 == EXPECT_ID1 {
            s.id_ok += 1;
        }
        let msb = (frame >> 8) as u8;
        let lsb = frame as u8;
        if diag.emsb == msb && diag.elsb == lsb {
            s.addr_ok += 1;
        } else if s.first_bad.is_none() {
            s.first_bad = Some((frame, diag.emsb, diag.elsb));
        }
    }
    s
}

/// Result of one [`run_data_test`] round trip.
struct DataTest {
    write_result: Result<(), Error>,
    read_result: Result<(), Error>,
    /// How many of the 128 written bytes read back unchanged.
    bytes_ok: u32,
    /// The first offset that didn't round-trip, with what was written vs
    /// what came back, for diagnosis.
    first_bad: Option<(usize, u8, u8)>,
}

/// Write [`test_pattern`] to [`SCRATCH_FRAME`] via `Block::write_frame`
/// directly (bypassing `Card`'s filesystem layer -- no directory entry is
/// touched), then read it back and compare byte-for-byte.
///
/// The address-echo sweep above proved addressing is not the problem: it
/// covers every frame a real save touches and came back 32/32 clean, in
/// both slots, at both presets that landed a real save. But a read and a
/// write are not symmetric transactions -- a read sends only two payload
/// bytes (the address) and *receives* 128; a write *sends* those same two
/// bytes plus 128 more (the real data) plus a checksum, all under timing
/// nothing before this test ever exercised. Real writes also have no way
/// to detect corruption here even in principle -- the protocol never
/// echoes written data back -- so this direct round trip (write, then a
/// fresh read of the same frame) is the only way to see whether a long
/// outbound burst survives intact.
fn run_data_test(preset: &Preset, slot: Slot) -> DataTest {
    let pattern = test_pattern();
    let mut wcard = card_for_preset(preset, slot);
    let write_result = wcard.write_frame(SCRATCH_FRAME, &pattern);

    let mut buf = [0u8; FRAME_SIZE];
    let mut rcard = card_for_preset(preset, slot);
    let read_result = rcard.read_frame(SCRATCH_FRAME, &mut buf);

    let mut bytes_ok = 0u32;
    let mut first_bad = None;
    for i in 0..FRAME_SIZE {
        if buf[i] == pattern[i] {
            bytes_ok += 1;
        } else if first_bad.is_none() {
            first_bad = Some((i, pattern[i], buf[i]));
        }
    }
    DataTest {
        write_result,
        read_result,
        bytes_ok,
        first_bad,
    }
}

/// Fixed-capacity ASCII line builder -- avoids per-call heap-free formatting
/// (there is no heap) while still letting lines be assembled from pieces.
struct LineBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}
impl<const N: usize> LineBuf<N> {
    fn new() -> Self {
        LineBuf { buf: [0; N], len: 0 }
    }
    fn push_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len < N {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
    }
    fn push_dec(&mut self, v: u32) {
        self.push_str(dec(v).as_str());
    }
    fn as_str(&self) -> &str {
        // SAFETY: only ASCII bytes are ever pushed.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

/// Decimal, no leading zeros, "0".."4294967295".
struct Dec {
    buf: [u8; 10],
    len: u8,
}
impl Dec {
    fn as_str(&self) -> &str {
        // SAFETY: only ASCII digits are ever written.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len as usize]) }
    }
}
fn dec(v: u32) -> Dec {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 10;
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let len = (10 - i) as u8;
    let mut buf = [0u8; 10];
    buf[..len as usize].copy_from_slice(&tmp[i..]);
    Dec { buf, len }
}

fn hex2(v: u8) -> [u8; 2] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    [HEX[(v >> 4) as usize], HEX[(v & 0xF) as usize]]
}

/// Draw `label` in grey then `val` as two hex digits tinted green/red
/// depending on whether it matches `expected`. Returns the x to continue
/// drawing the next field at.
fn draw_hex_field(
    font: &FontAtlas,
    x: i16,
    y: i16,
    label: &str,
    val: u8,
    expected: u8,
) -> i16 {
    font.draw_text(x, y, label, GREY);
    let vx = x + font.text_width(label) as i16;
    let h = hex2(val);
    let vs = unsafe { core::str::from_utf8_unchecked(&h) };
    let tint = if val == expected { GREEN } else { RED };
    font.draw_text(vx, y, vs, tint);
    vx + font.text_width(vs) as i16 + 8
}

/// Like [`draw_hex_field`], but also prints the expected/"want" byte next
/// to the actual one (`actual` varies per read for `CHK`, so a fixed
/// constant can't be baked into the call site the way it can for the other
/// fields).
fn draw_hex_pair_field(
    font: &FontAtlas,
    x: i16,
    y: i16,
    label: &str,
    actual: u8,
    want: u8,
) -> i16 {
    font.draw_text(x, y, label, GREY);
    let mut vx = x + font.text_width(label) as i16;
    let a = hex2(actual);
    let astr = unsafe { core::str::from_utf8_unchecked(&a) };
    let tint = if actual == want { GREEN } else { RED };
    font.draw_text(vx, y, astr, tint);
    vx += font.text_width(astr) as i16;
    font.draw_text(vx, y, "/", GREY);
    vx += font.text_width("/") as i16;
    let w = hex2(want);
    let wstr = unsafe { core::str::from_utf8_unchecked(&w) };
    font.draw_text(vx, y, wstr, GREY);
    vx + font.text_width(wstr) as i16 + 8
}

/// Running per-field pass counts for repeated [`HardwareCard::probe_read`]
/// calls at one fixed slot + `Timing` -- a single probe can look fine or
/// broken by chance, so this is what actually answers "does this setting
/// work". Reset whenever the slot or timing preset changes.
#[derive(Copy, Clone, Default)]
struct ProbeStats {
    total: u32,
    id1_ok: u32,
    id2_ok: u32,
    ack1_ok: u32,
    ack2_ok: u32,
    end_ok: u32,
    chk_ok: u32,
    full_ok: u32,
}
impl ProbeStats {
    fn record(&mut self, diag: &ReadDiag, result: &Result<(), Error>) {
        self.total += 1;
        if diag.id1 == EXPECT_ID1 {
            self.id1_ok += 1;
        }
        if diag.id2 == EXPECT_ID2 {
            self.id2_ok += 1;
        }
        if diag.ack1 == EXPECT_ACK1 {
            self.ack1_ok += 1;
        }
        if diag.ack2 == EXPECT_ACK2 {
            self.ack2_ok += 1;
        }
        if diag.end == EXPECT_END {
            self.end_ok += 1;
        }
        if diag.chk == diag.want_chk {
            self.chk_ok += 1;
        }
        if result.is_ok() {
            self.full_ok += 1;
        }
    }
}

/// Draw `label` then `100*ok/total` as a percentage, tinted green at 100%,
/// red at 0%, yellow in between, grey if there is no data yet.
fn draw_pct_field(font: &FontAtlas, x: i16, y: i16, label: &str, ok: u32, total: u32) -> i16 {
    font.draw_text(x, y, label, GREY);
    let vx = x + font.text_width(label) as i16;
    let (pct, tint) = if total == 0 {
        (0, GREY)
    } else {
        let p = ok * 100 / total;
        let t = if p == 100 {
            GREEN
        } else if p == 0 {
            RED
        } else {
            YELLOW
        };
        (p, t)
    };
    let p = dec(pct);
    font.draw_text(vx, y, p.as_str(), tint);
    let vx2 = vx + font.text_width(p.as_str()) as i16;
    font.draw_text(vx2, y, "%", tint);
    vx2 + font.text_width("%") as i16 + 8
}

#[no_mangle]
fn main() {
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);

    let mut pad = PadTracker::new();
    let mut counter: u32 = 0;
    let mut slot = Slot::One;
    let mut preset_idx: usize = 0;
    let mut probe_frame: u16 = 0;

    let mut probe_result: Option<(Result<(), Error>, ReadDiag, Slot)> = None;
    let mut probe_stats = ProbeStats::default();

    let mut save_attempts: u32 = 0;
    let mut save_result: Option<(Result<(), Error>, Slot)> = None;
    let mut sweep_attempts: u32 = 0;
    let mut sweep_result: Option<AddrSweep> = None;
    let mut data_test_attempts: u32 = 0;
    let mut data_test_result: Option<(DataTest, Slot)> = None;

    loop {
        pad.update(poll_port1().buttons.bits());

        if pad.just_pressed(button::CROSS) {
            counter = counter.wrapping_add(1);
        }
        if pad.just_pressed(button::SELECT) {
            slot = match slot {
                Slot::One => Slot::Two,
                Slot::Two => Slot::One,
            };
            probe_stats = ProbeStats::default();
        }
        if pad.just_pressed(button::R1) {
            preset_idx = (preset_idx + 1) % PRESETS.len();
            probe_stats = ProbeStats::default();
        }
        if pad.just_pressed(button::L1) {
            preset_idx = (preset_idx + PRESETS.len() - 1) % PRESETS.len();
            probe_stats = ProbeStats::default();
        }
        // Target frame for Circle -- every probe before this build only
        // ever tested frame 0 (address bytes 0x00, 0x00), which a real
        // save's non-zero addresses never exercised. Up/Down step by one
        // frame, Left/Right jump a whole 64-frame block.
        let mut frame_changed = false;
        if pad.repeats(button::UP, 15, 4) {
            probe_frame = probe_frame.saturating_add(1);
            frame_changed = true;
        }
        if pad.repeats(button::DOWN, 15, 4) {
            probe_frame = probe_frame.saturating_sub(1);
            frame_changed = true;
        }
        if pad.repeats(button::RIGHT, 15, 4) {
            probe_frame = (probe_frame + 64).min(1023);
            frame_changed = true;
        }
        if pad.repeats(button::LEFT, 15, 4) {
            probe_frame = probe_frame.saturating_sub(64);
            frame_changed = true;
        }
        if frame_changed {
            probe_stats = ProbeStats::default();
        }
        // Auto-repeat: a single probe can pass or fail by chance (see the
        // module doc), so holding Circle is how enough samples get
        // collected to see the real pass rate instead of one anecdote.
        if pad.repeats(button::CIRCLE, 6, 1) {
            let mut card = card_for_preset(&PRESETS[preset_idx], slot);
            let (r, diag) = card.probe_read(probe_frame);
            probe_stats.record(&diag, &r);
            probe_result = Some((r, diag, slot));
        }
        if pad.just_pressed(button::START) {
            if REAL_SAVE_ENABLED {
                save_attempts += 1;
                let card = card_for_preset(&PRESETS[preset_idx], slot);
                save_result = Some((save_with_card(card, counter), slot));
            } else {
                sweep_attempts += 1;
                sweep_result = Some(run_addr_sweep(&PRESETS[preset_idx], slot));
            }
        }
        // Deliberate single press, not auto-repeat -- this is the closest
        // thing to a real write in this build, even though it only ever
        // touches SCRATCH_FRAME and never the directory.
        if pad.just_pressed(button::SQUARE) {
            data_test_attempts += 1;
            data_test_result = Some((run_data_test(&PRESETS[preset_idx], slot), slot));
        }

        fb.clear(10, 12, 20);

        font.draw_text(4, 3, "MEMCARD PROTOCOL DIAG", GREY);
        font.draw_text(4, 13, "SEL:SLOT L1/R1:TIMING D-PAD:FRAME", GREY);
        if REAL_SAVE_ENABLED {
            font.draw_text(4, 23, "HOLD CIRCLE:PROBE(safe) START:SAVE", GREY);
        } else {
            font.draw_text(4, 23, "CIRCLE:PROBE START:ADDRSWEEP SQ:DATATEST", RED);
        }

        // Centered probe-frame number -- the thing actually under test now.
        // `X` still bumps `counter` (harmless, kept for when real saving is
        // re-enabled) but no longer gets screen prominence.
        let scale = 2u8;
        let num = dec(probe_frame as u32);
        let w = font.text_width(num.as_str()) as i32 * scale as i32;
        let x = (320 - w) / 2;
        font.draw_text_scaled(x as i16, 35, num.as_str(), scale, scale, WHITE);
        {
            let mut l = LineBuf::<32>::new();
            l.push_str("block ");
            l.push_dec((probe_frame / 64) as u32);
            l.push_str(" frame ");
            l.push_dec((probe_frame % 64) as u32);
            let lw = font.text_width(l.as_str()) as i32;
            font.draw_text(((320 - lw) / 2) as i16, 52, l.as_str(), GREY);
        }
        let _ = counter; // see note above

        // Current target + timing preset.
        {
            let mut l = LineBuf::<40>::new();
            l.push_str("SLOT: ");
            l.push_str(slot_name(slot));
            l.push_str("   TIMING: ");
            l.push_str(PRESETS[preset_idx].name);
            font.draw_text(4, 66, l.as_str(), WHITE);
        }
        match PRESETS[preset_idx].kind {
            PresetKind::Irq(t) => {
                let mut l = LineBuf::<32>::new();
                l.push_str("setup=");
                l.push_dec(t.setup_spins);
                l.push_str(" byte=");
                l.push_dec(t.byte_spins);
                font.draw_text(4, 77, l.as_str(), GREY);

                let mut l2 = LineBuf::<32>::new();
                l2.push_str("ack=");
                l2.push_dec(t.ack_spins);
                l2.push_str(" end_delay=");
                l2.push_dec(t.end_delay_spins);
                font.draw_text(4, 88, l2.as_str(), GREY);
            }
            PresetKind::NoAck {
                setup_spins,
                byte_spins,
                interbyte_spins,
                deselect_spins,
                write_gap_spins,
            } => {
                let mut l = LineBuf::<32>::new();
                l.push_str("setup=");
                l.push_dec(setup_spins);
                l.push_str(" byte=");
                l.push_dec(byte_spins);
                font.draw_text(4, 77, l.as_str(), GREY);

                // Short labels (ib/ds/wg) to fit all three on one line.
                let mut l2 = LineBuf::<40>::new();
                l2.push_str("ib=");
                l2.push_dec(interbyte_spins);
                l2.push_str(" ds=");
                l2.push_dec(deselect_spins);
                l2.push_str(" wg=");
                l2.push_dec(write_gap_spins);
                font.draw_text(4, 88, l2.as_str(), GREY);
            }
        }

        // Running per-field pass rate over every probe at this slot+timing+
        // frame -- a single probe can pass or fail by chance, so this is the
        // number that actually answers "does this setting work".
        {
            let mut l = LineBuf::<24>::new();
            l.push_str("PROBES: ");
            l.push_dec(probe_stats.total);
            font.draw_text(4, 100, l.as_str(), GREY);
        }
        {
            let mut px = 4i16;
            px = draw_pct_field(&font, px, 111, "ID1:", probe_stats.id1_ok, probe_stats.total);
            px = draw_pct_field(&font, px, 111, "ID2:", probe_stats.id2_ok, probe_stats.total);
            let _ = draw_pct_field(&font, px, 111, "ACK1:", probe_stats.ack1_ok, probe_stats.total);

            let mut px2 = 4i16;
            px2 = draw_pct_field(&font, px2, 122, "ACK2:", probe_stats.ack2_ok, probe_stats.total);
            px2 = draw_pct_field(&font, px2, 122, "END:", probe_stats.end_ok, probe_stats.total);
            let _ = draw_pct_field(&font, px2, 122, "CHK:", probe_stats.chk_ok, probe_stats.total);

            let _ = draw_pct_field(&font, 4, 133, "FULL OK:", probe_stats.full_ok, probe_stats.total);
        }

        // Most recent probe's raw bytes -- context for the stats above,
        // e.g. whether wrong fields tend to fail together or independently.
        match &probe_result {
            None => {
                font.draw_text(4, 145, "LAST PROBE: (not tried yet)", GREY);
            }
            Some((r, diag, s)) => {
                let mut l = LineBuf::<40>::new();
                l.push_str("LAST slot ");
                l.push_str(slot_name(*s));
                l.push_str(": ");
                let tint = match r {
                    Ok(()) => {
                        l.push_str("OK");
                        GREEN
                    }
                    Err(e) => {
                        l.push_str(error_name(*e));
                        RED
                    }
                };
                font.draw_text(4, 145, l.as_str(), tint);

                // The address this specific probe requested -- EMSB/ELSB
                // are only "correct" relative to *this* frame, which is no
                // longer always 0.
                let want_msb = (probe_frame >> 8) as u8;
                let want_lsb = probe_frame as u8;

                let mut fx = 4i16;
                fx = draw_hex_field(&font, fx, 156, "ID1:", diag.id1, EXPECT_ID1);
                fx = draw_hex_field(&font, fx, 156, "ID2:", diag.id2, EXPECT_ID2);
                fx = draw_hex_field(&font, fx, 156, "ACK1:", diag.ack1, EXPECT_ACK1);
                let _ = draw_hex_field(&font, fx, 156, "ACK2:", diag.ack2, EXPECT_ACK2);

                let mut gx = 4i16;
                gx = draw_hex_field(&font, gx, 167, "EMSB:", diag.emsb, want_msb);
                gx = draw_hex_field(&font, gx, 167, "ELSB:", diag.elsb, want_lsb);
                gx = draw_hex_field(&font, gx, 167, "END:", diag.end, EXPECT_END);
                let _ = draw_hex_pair_field(&font, gx, 167, "CHK:", diag.chk, diag.want_chk);
            }
        }

        // Read-only address-echo sweep (real saving is disabled -- see the
        // module doc) / real save result, whichever REAL_SAVE_ENABLED picks.
        if REAL_SAVE_ENABLED {
            let mut l = LineBuf::<32>::new();
            l.push_str("SAVE ATTEMPTS: ");
            l.push_dec(save_attempts);
            font.draw_text(4, 184, l.as_str(), GREY);
            match &save_result {
                None => {
                    font.draw_text(4, 196, "SAVE: (not tried yet)", GREY);
                }
                Some((Ok(()), s)) => {
                    let mut l = LineBuf::<24>::new();
                    l.push_str("SAVE slot ");
                    l.push_str(slot_name(*s));
                    l.push_str(": OK");
                    font.draw_text(4, 196, l.as_str(), GREEN);
                }
                Some((Err(e), s)) => {
                    let mut l = LineBuf::<32>::new();
                    l.push_str("SAVE slot ");
                    l.push_str(slot_name(*s));
                    l.push_str(": ERROR");
                    font.draw_text(4, 196, l.as_str(), RED);
                    font.draw_text(4, 207, error_name(*e), YELLOW);
                }
            }
        } else {
            let mut l = LineBuf::<32>::new();
            l.push_str("SWEEPS: ");
            l.push_dec(sweep_attempts);
            font.draw_text(4, 184, l.as_str(), GREY);
            match &sweep_result {
                None => {
                    font.draw_text(4, 196, "ADDR SWEEP: (not run yet)", GREY);
                }
                Some(s) => {
                    let mut l = LineBuf::<40>::new();
                    l.push_str("ADDR OK ");
                    l.push_dec(s.addr_ok);
                    l.push_str("/");
                    l.push_dec(s.tested);
                    l.push_str("  ID OK ");
                    l.push_dec(s.id_ok);
                    l.push_str("/");
                    l.push_dec(s.tested);
                    let tint = if s.addr_ok == s.tested { GREEN } else { RED };
                    font.draw_text(4, 196, l.as_str(), tint);

                    match s.first_bad {
                        None => {
                            font.draw_text(4, 207, "no bad address echoes", GREEN);
                        }
                        Some((frame, emsb, elsb)) => {
                            let mut l2 = LineBuf::<32>::new();
                            l2.push_str("1st bad: frame ");
                            l2.push_dec(frame as u32);
                            l2.push_str(" got ");
                            let h1 = hex2(emsb);
                            l2.push_str(unsafe { core::str::from_utf8_unchecked(&h1) });
                            l2.push_str(",");
                            let h2 = hex2(elsb);
                            l2.push_str(unsafe { core::str::from_utf8_unchecked(&h2) });
                            font.draw_text(4, 207, l2.as_str(), RED);
                        }
                    }
                }
            }
        }

        // Write+read-back round trip on SCRATCH_FRAME only -- the one
        // thing the address sweep structurally can't test (a long outbound
        // data burst). See run_data_test's doc for why this matters.
        {
            let mut l = LineBuf::<40>::new();
            l.push_str("DTEST ");
            l.push_dec(data_test_attempts);
            l.push_str(" W:");
            l.push_str(match &data_test_result {
                Some((d, _)) if d.write_result.is_ok() => "OK",
                Some((d, _)) => error_name(d.write_result.unwrap_err()),
                None => "--",
            });
            l.push_str(" R:");
            l.push_str(match &data_test_result {
                Some((d, _)) if d.read_result.is_ok() => "OK",
                Some((d, _)) => error_name(d.read_result.unwrap_err()),
                None => "--",
            });
            let tint = match &data_test_result {
                Some((d, _)) if d.write_result.is_ok() && d.read_result.is_ok() => WHITE,
                Some(_) => RED,
                None => GREY,
            };
            font.draw_text(4, 218, l.as_str(), tint);
        }
        match &data_test_result {
            None => {
                font.draw_text(4, 229, "BYTES: (not run yet)", GREY);
            }
            Some((d, _)) => {
                let mut l = LineBuf::<40>::new();
                l.push_str("BYTES ");
                l.push_dec(d.bytes_ok);
                l.push_str("/128");
                let tint = if d.bytes_ok as usize == FRAME_SIZE {
                    GREEN
                } else {
                    RED
                };
                if let Some((off, want, got)) = d.first_bad {
                    l.push_str(" @");
                    l.push_dec(off as u32);
                    l.push_str(" want=");
                    let h1 = hex2(want);
                    l.push_str(unsafe { core::str::from_utf8_unchecked(&h1) });
                    l.push_str(" got=");
                    let h2 = hex2(got);
                    l.push_str(unsafe { core::str::from_utf8_unchecked(&h2) });
                }
                font.draw_text(4, 229, l.as_str(), tint);
            }
        }

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}
