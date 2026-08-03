//! Root counters (Timer 0 / 1 / 2).
//!
//! Three 16-bit counters at `0x1F80_1100`, `0x1F80_1110`, `0x1F80_1120`,
//! each with three registers:
//! - counter value (offset 0)
//! - mode / control (offset 4)
//! - target value (offset 8)
//!
//! The bus advances counters lazily from the shared CPU cycle clock:
//! scheduler drains and MMIO reads/writes synchronize the bank to the
//! current cycle, then raise timer IRQs through
//! [`crate::irq::IrqSource::Timer0`]…`Timer2`.
//!
//! ## Provenance
//!
//! Register semantics and physical clock relationships follow nocash
//! PSX-SPX's timer and GPU timing documentation, then are checked against
//! JaCzekanski/ps1-tests build-158 silicon captures. Portions of the original
//! implementation were parity-matched against, and in places derived from,
//! PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>), Copyright (C)
//! the PCSX-Redux authors, GPL-2.0-or-later. Points of correspondence are
//! flagged inline. PSoXide is released under GPL-2.0-or-later in part to honor
//! this lineage; see `LICENSE` and `docs/license-audit.md`.

/// One of the three root counters. Fields are 16 bits on hardware but
/// held as `u32` for uniform bus access -- upper bits read as 0.
#[derive(Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Timer {
    /// Current counter value (bits 0..=15 meaningful).
    pub counter: u32,
    /// Mode / control register. See nocash PSX-SPX "Timers" section for
    /// the bit layout; we store the written value verbatim for now.
    pub mode: u32,
    /// Target value the counter compares against.
    pub target: u32,
    /// Sub-tick accumulator in fractional clock-source units. When a
    /// timer's clock source ticks slower than the system clock (HBlank
    /// on T1, /8 on T2), the residual cycles accumulate here until
    /// they cross the source period.
    accum: u64,
    /// Bus cycle at which the counter was last reset (mode write).
    /// Diagnostic -- lets us compare against Redux's `cycleStart`.
    pub last_reset_cycle: u64,
    /// Number of mode writes since reset. Diagnostic.
    pub mode_write_count: u64,
    /// Sync-mode 3 latch: false until the first HBlank/VBlank edge after a
    /// mode write, true once that edge releases the counter into free-run.
    #[serde(default)]
    sync_seen: bool,
    /// Reset-on-target has one extra zero phase on silicon: after the target
    /// is visible the next source tick resets to zero, and the following tick
    /// still reads zero before counting resumes at one.
    #[serde(default)]
    target_reset_hold: bool,
    /// CPU clocks for which the counter is held unchanged. Mode/current-value
    /// writes hold it briefly, and a read of this counter latches it
    /// across its own MMIO wait clocks. The hold happens before source-clock
    /// division: it must not suppress two whole HBlank ticks or two `/8` ticks.
    #[serde(default)]
    counter_hold_cycles: u8,
    /// A current-value write can overlap the setup wait of the immediately
    /// following external-bus read.
    #[serde(default)]
    counter_bus_overlap_pending: bool,
    #[serde(default)]
    counter_write_cycle: u64,
    /// One-shot IRQs stop producing edges after their first event, but this is
    /// independent of observable mode bit 10. In pulse mode the bit returns
    /// high immediately after the short low pulse on real hardware.
    #[serde(default)]
    irq_fired_once: bool,
}

// Mode-register bit layout (nocash PSX-SPX, section "Timers").
/// bit 0: sync enable. When set, the timer obeys the sync-mode
/// bits below; when clear, the timer is "free-run" (pure clock).
const MODE_SYNC_ENABLE: u32 = 1 << 0;
/// bits 1..2: sync mode. Meaning depends on the timer index:
/// - Timer 0 (HBlank-synced): 0=pause in HBlank, 1=reset at HBlank,
///   2=reset+pause, 3=pause until HBlank then free-run.
/// - Timer 1 (VBlank-synced): same four modes.
/// - Timer 2: modes 0/3 stop counter, modes 1/2 free-run.
const MODE_SYNC_MODE_MASK: u32 = 0x6;
/// bit 3: reset counter when reaching target (0 = reset at 0xFFFF).
const MODE_RESET_AT_TARGET: u32 = 1 << 3;
/// bit 4: raise IRQ when target reached.
const MODE_IRQ_ON_TARGET: u32 = 1 << 4;
/// bit 5: raise IRQ when counter wraps at 0xFFFF.
const MODE_IRQ_ON_WRAP: u32 = 1 << 5;
/// bit 6: IRQ repeat mode (0 = one-shot until mode-write).
const MODE_IRQ_REPEAT: u32 = 1 << 6;
/// bit 7: pulse (0) versus toggle (1) behavior for the active-low request.
const MODE_IRQ_TOGGLE: u32 = 1 << 7;
/// bit 10: IRQ status -- active-low request line.
const MODE_IRQ_ACTIVE_LOW: u32 = 1 << 10;
/// bit 11: "reached target" sticky flag.
const MODE_REACHED_TARGET: u32 = 1 << 11;
/// bit 12: "reached 0xFFFF" sticky flag.
const MODE_REACHED_WRAP: u32 = 1 << 12;
/// Standard NTSC/PAL active horizontal display window in GPU clocks. HBlank
/// is the complement of this range. GPU clock = CPU clock * 11/7.
const H_ACTIVE_START_GPU: u64 = 0x260;
const H_ACTIVE_END_GPU: u64 = 0xC60;

/// The full three-timer bank.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Timers {
    /// Per-counter state. Index 0 / 1 / 2 corresponds to Timer 0 / 1 / 2.
    pub timers: [Timer; 3],
    /// Bus cycle at which the timers were last advanced. Used by
    /// `advance_to` so the bus can drive timer state lazily --
    /// once per branch-test scheduler drain and on demand from
    /// MMIO read paths -- instead of paying the per-instruction
    /// 3-counter accumulator/divider cost. Mirrors PCSX-Redux's
    /// `Counters::set` / `update` model where each counter holds
    /// `cycleStart` and the live count is `(cycle - cycleStart) /
    /// rate` evaluated lazily.
    last_advance_cycle: u64,
    /// Offset between the scheduled VBlank IRQ edge and the GPU blanking
    /// signal seen by Timer 1 sync modes, measured in scanlines. Late PSone
    /// silicon exposes these as distinct phases.
    #[serde(default)]
    vblank_sync_offset_lines: u8,
    /// Profile-specific counter-side hold beyond the common CPU MMIO wait.
    #[serde(default)]
    counter_read_extra_hold: [u8; 3],
    /// Timer 0's dot-clock path has a shorter counter-side MMIO hold than its
    /// system-clock path on the measured late PSone.
    #[serde(default)]
    timer0_dot_read_extra_hold: u8,
    /// Diagnostic: count of Timer 2 IRQ fires since reset. Excluded
    /// from save states.
    #[serde(skip)]
    pub dbg_timer2_fires: u64,
}

impl Timers {
    /// Base address of the timer bank.
    pub const BASE: u32 = 0x1F80_1100;
    /// Size of the bank (3 timers × 16 bytes each; last 4 bytes unused).
    pub const SIZE: u32 = 0x30;
    /// Stride between consecutive timers.
    pub const STRIDE: u32 = 0x10;

    /// All counters / modes / targets zero-initialised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Select the Timer 1 vertical-blank sync phase for a hardware profile.
    pub(crate) fn set_vblank_sync_offset_lines(&mut self, lines: u8) {
        self.vblank_sync_offset_lines = lines;
    }

    pub(crate) fn set_counter_read_extra_hold(&mut self, index: usize, cycles: u8) {
        self.counter_read_extra_hold[index] = cycles;
    }

    pub(crate) fn set_timer0_dot_read_extra_hold(&mut self, cycles: u8) {
        self.timer0_dot_read_extra_hold = cycles;
    }

    /// `true` when `phys` falls inside `0x1F80_1100..0x1F80_1130`.
    pub fn contains(phys: u32) -> bool {
        (Self::BASE..Self::BASE + Self::SIZE).contains(&phys)
    }

    /// Read a 32-bit word. `phys` must be inside `BASE..BASE+SIZE`.
    pub fn read32(&mut self, phys: u32) -> u32 {
        let (idx, off) = decode(phys);
        match off {
            0x0 => self.timers[idx].counter,
            0x4 => {
                let mode = self.timers[idx].mode;
                // Reading mode acknowledges both sticky reached flags. If the
                // counter is still at its terminal condition, hardware may
                // latch the target flag again on a later timer clock.
                self.timers[idx].mode &= !(MODE_REACHED_TARGET | MODE_REACHED_WRAP);
                mode
            }
            0x8 => self.timers[idx].target,
            _ => 0,
        }
    }

    /// Write a 32-bit word. `phys` must be inside `BASE..BASE+SIZE`.
    /// `now` is the current bus cycle; used only for diagnostics
    /// (records when mode writes reset the counter).
    pub fn write32(&mut self, phys: u32, value: u32, now: u64) {
        let (idx, off) = decode(phys);
        let t = &mut self.timers[idx];
        let v16 = value & 0xFFFF;
        match off {
            0x0 => {
                t.counter = v16;
                t.target_reset_hold = false;
                // Reset-at-target releases one clock earlier; all other
                // current-value writes occupy the root counter for the full
                // three-clock CPU transaction.
                t.counter_hold_cycles = if t.mode & MODE_RESET_AT_TARGET != 0 {
                    2
                } else {
                    3
                };
                t.counter_bus_overlap_pending = true;
                t.counter_write_cycle = now;
            }
            0x4 => {
                // Mode writes reset the counter and re-arm the IRQ request,
                // but do not acknowledge the read-only reached flags. Real
                // SCPH-9902 captures preserve bits 11/12 across a mode write;
                // only reading the mode register clears them.
                let reached = t.mode & (MODE_REACHED_TARGET | MODE_REACHED_WRAP);
                t.mode = (value & !(MODE_IRQ_ACTIVE_LOW | MODE_REACHED_TARGET | MODE_REACHED_WRAP))
                    | MODE_IRQ_ACTIVE_LOW
                    | reached;
                t.counter = 0;
                t.accum = 0;
                t.last_reset_cycle = now;
                t.mode_write_count = t.mode_write_count.saturating_add(1);
                t.sync_seen = false;
                t.target_reset_hold = false;
                t.counter_hold_cycles = 2;
                t.counter_bus_overlap_pending = false;
                t.irq_fired_once = false;
            }
            0x8 => t.target = v16,
            _ => {}
        }
    }

    /// Advance all three timers by `cycles` system-clock ticks. Each
    /// timer converts that to its own clock source first:
    /// - Timer 0 source 1 / 3 → dot-clock (pixels per `dot_clock_divisor`
    ///   system cycles -- `GPU::dot_clock_divisor` gives the current
    ///   value based on H-resolution).
    /// - Timer 1 source 1 / 3 → HBlank (`hsync_period` cycles per tick).
    /// - Timer 2 source 2 / 3 → system / 8.
    /// - Everything else → system clock.
    ///
    /// Returns a 3-bit mask of timers that fired an IRQ this tick.
    /// The caller (Bus) uses it to call `Irq::raise(IrqSource::Timer0/1/2)`.
    pub fn tick(&mut self, cycles: u64, hsync_period: u64, dot_clock_divisor: u64) -> u8 {
        let mut fired: u8 = 0;
        for i in 0..3 {
            if self.advance_timer(
                i,
                cycles,
                hsync_period,
                dot_clock_divisor,
                u64::MAX,
                hsync_period.saturating_mul(263),
            ) {
                fired |= 1 << i;
            }
        }
        self.last_advance_cycle = self.last_advance_cycle.saturating_add(cycles);
        fired
    }

    /// Advance the timer bank to the absolute cycle `now`, using the
    /// time elapsed since the last advance. Equivalent to calling
    /// `tick(delta, ...)` where `delta = now - last_advance_cycle`.
    /// Returns the same 3-bit "fired" bitmap.
    ///
    /// This is the lazy entry point: bus calls it once per
    /// scheduler drain (instead of every `Bus::tick`), and read /
    /// write paths that observe timer state call it first so the
    /// values they see match the cycle they observe at.
    pub fn advance_to(&mut self, now: u64, hsync_period: u64, dot_clock_divisor: u64) -> u8 {
        self.advance_to_video(
            now,
            hsync_period,
            dot_clock_divisor,
            u64::MAX,
            hsync_period.saturating_mul(263),
        )
    }

    /// Video-aware lazy advance used by the system bus. `next_vblank` is the
    /// scheduler's absolute VBlank-start deadline; together with `period` it
    /// gives Timer 1 a stable beam phase without adding a per-scanline event.
    pub fn advance_to_video(
        &mut self,
        now: u64,
        hsync_period: u64,
        dot_clock_divisor: u64,
        next_vblank: u64,
        vblank_period: u64,
    ) -> u8 {
        let delta = now.saturating_sub(self.last_advance_cycle);
        if delta == 0 {
            return 0;
        }
        let next_timer1_vblank = if self.vblank_sync_offset_lines == 0 || next_vblank == u64::MAX {
            next_vblank
        } else {
            let offset = hsync_period.saturating_mul(self.vblank_sync_offset_lines as u64);
            if next_vblank <= now {
                next_vblank.saturating_add(offset)
            } else {
                let previous_shifted = next_vblank
                    .saturating_sub(vblank_period)
                    .saturating_add(offset);
                if previous_shifted > self.last_advance_cycle {
                    previous_shifted
                } else {
                    next_vblank.saturating_add(offset)
                }
            }
        };
        let mut fired: u8 = 0;
        for i in 0..3 {
            if self.advance_timer(
                i,
                delta,
                hsync_period,
                dot_clock_divisor,
                next_timer1_vblank,
                vblank_period,
            ) {
                fired |= 1 << i;
            }
        }
        self.last_advance_cycle = now;
        fired
    }

    /// Sync the lazy clock to `now` without advancing any state.
    /// Used by the bus when it discards skipped cycles (e.g.
    /// post-warmup resets in tests) -- calling this prevents
    /// `advance_to` from later seeing a huge backlog and trying
    /// to fast-forward through millions of cycles in one go.
    #[allow(dead_code)]
    pub fn sync_clock_to(&mut self, now: u64) {
        self.last_advance_cycle = now;
    }

    /// Latch only the selected root counter for its CPU-side MMIO wait clocks.
    /// Other root counters continue running and can therefore measure the
    /// complete access time of this register.
    pub fn hold_counter_for_read(&mut self, phys: u32, cycles: u32) {
        let (idx, off) = decode(phys);
        let mode = self.timers[idx].mode;
        // Both of the zero cases hold no extra cycles: an IRQ-configured
        // counter 2, and any counter that resets at target.
        let configured_extra = if (idx == 2 && mode & (MODE_IRQ_ON_TARGET | MODE_IRQ_ON_WRAP) != 0)
            || mode & MODE_RESET_AT_TARGET != 0
        {
            0
        } else if idx == 0 && matches!((mode >> 8) & 3, 1 | 3) {
            self.timer0_dot_read_extra_hold
        } else {
            self.counter_read_extra_hold[idx]
        };
        let profile_extra = u32::from(configured_extra);
        let cycles = match off {
            0 => cycles.saturating_add(profile_extra),
            // The plain reset-at-target comparator holds the counter through
            // the complete five-cycle read transaction. Enabling either IRQ
            // path bypasses that comparator hold; consecutive reads then
            // leave two wait clocks on the selected counter. The distinction
            // is directly visible when target=24 clears between reads while
            // target+IRQ=32 relatches over the longer caller gap.
            4 if self.timers[idx].mode & (MODE_IRQ_ON_TARGET | MODE_IRQ_ON_WRAP) == 0 => {
                cycles.min(2).saturating_add(3)
            }
            4 => cycles.min(2),
            _ => return,
        };
        self.timers[idx].counter_hold_cycles = self.timers[idx]
            .counter_hold_cycles
            .saturating_add(cycles.min(u32::from(u8::MAX)) as u8);
    }

    /// Overlap the first external memory-controller wait with a just-written
    /// root counter. Silicon timing sweeps expose this as one missing setup
    /// wait on the first of 64 otherwise-identical accesses.
    pub(crate) fn overlap_counter_write_with_external_read(&mut self, now: u64, stalls: u32) {
        for timer in &mut self.timers {
            if !timer.counter_bus_overlap_pending {
                continue;
            }
            timer.counter_bus_overlap_pending = false;
            if now.saturating_sub(timer.counter_write_cycle) <= 16 {
                timer.counter_hold_cycles = timer
                    .counter_hold_cycles
                    .saturating_add(stalls.saturating_sub(2).min(u32::from(u8::MAX)) as u8);
            }
        }
    }

    /// Is this timer currently paused per its sync-mode bits?
    ///
    /// Timer 0/1 beam-synchronization is handled separately while their
    /// intervals are advanced. Timer 2 has no external beam signal: its
    /// sync modes directly select stopped versus free-running behavior.
    fn is_timer_paused(&self, idx: usize) -> bool {
        let t = &self.timers[idx];
        if t.mode & MODE_SYNC_ENABLE == 0 {
            return false;
        }
        let sync = (t.mode & MODE_SYNC_MODE_MASK) >> 1;
        match (idx, sync) {
            // Real silicon: Timer 2 sync-mode 0 / 3 stop; 1 / 2 free-run.
            (2, 0) | (2, 3) => true,
            _ => false,
        }
    }

    /// VBlank phase is reconstructed by `advance_to_video` from the
    /// scheduler's absolute deadline. The bus still calls this at the edge,
    /// but no additional mutation is necessary here; doing both would apply
    /// Timer 1 reset/release semantics twice.
    pub fn notify_vblank(&mut self) {}

    fn advance_timer(
        &mut self,
        idx: usize,
        cycles: u64,
        hsync_period: u64,
        dot_clock_divisor: u64,
        next_vblank: u64,
        vblank_period: u64,
    ) -> bool {
        let held_cycles = {
            let timer = &mut self.timers[idx];
            let held = cycles.min(u64::from(timer.counter_hold_cycles));
            timer.counter_hold_cycles -= held as u8;
            held
        };
        let cycles = cycles - held_cycles;
        if cycles == 0 {
            return false;
        }
        let interval_start = self.last_advance_cycle.saturating_add(held_cycles);

        if idx == 0 && self.timers[0].mode & MODE_SYNC_ENABLE != 0 {
            return self.advance_timer0_synced(
                interval_start,
                cycles,
                hsync_period,
                dot_clock_divisor,
                vblank_period,
            );
        }
        if idx == 1 && self.timers[1].mode & MODE_SYNC_ENABLE != 0 {
            return self.advance_timer1_synced(
                interval_start,
                cycles,
                hsync_period,
                next_vblank,
                vblank_period,
            );
        }

        // Sync-mode gating -- decide whether this timer is
        // currently paused.
        let paused = self.is_timer_paused(idx);
        if paused {
            return false;
        }

        let t = &mut self.timers[idx];
        let source = (t.mode >> 8) & 0x3;

        // Convert `cycles` system clocks into source clocks.
        let ticks = match (idx, source) {
            // Timer 0 source 1/3 = dot clock. Within a scanline the nominal
            // 11/7 GPU:CPU ratio applies; at the line boundary hardware drops
            // the fractional dot and lands on the documented integer dots per
            // line. This matters over a frame but must not distort short
            // 1000-cycle probes.
            (0, 1) | (0, 3) => dot_ticks_between(
                interval_start,
                interval_start.saturating_add(cycles),
                hsync_period,
                dot_clock_divisor,
                vblank_period,
            ),
            // Timer 1 source 1/3 = HBlank: one tick per HSync period.
            (1, 1) | (1, 3) => hblank_ticks_between(
                interval_start,
                interval_start.saturating_add(cycles),
                hsync_period,
            ),
            // Timer 2 source 2/3 = system clock / 8.
            (2, 2) | (2, 3) => {
                t.accum += cycles;
                let n = t.accum / 8;
                t.accum %= 8;
                n
            }
            // Everything else is 1:1 system clock.
            _ => cycles,
        };

        let fired = advance_counter(t, ticks);
        if idx == 2 && fired {
            self.dbg_timer2_fires += 1;
        }
        fired
    }

    fn advance_timer1_synced(
        &mut self,
        start: u64,
        cycles: u64,
        hsync_period: u64,
        next_vblank: u64,
        vblank_period: u64,
    ) -> bool {
        let end = start.saturating_add(cycles);
        let hsync = hsync_period.max(1);
        let period = vblank_period.max(hsync);
        let total_lines = period / hsync;
        let blank_lines = match total_lines {
            263 => 20, // NTSC: line 243 through the end of the field
            314 => 58, // PAL: line 256 through the end of the field
            _ => 20,
        };
        let blank_duration = blank_lines * hsync;
        let mut cursor = start;
        let mut fired = false;

        while cursor < end {
            let previous_start = if next_vblank <= cursor {
                Some(next_vblank)
            } else if next_vblank >= period {
                Some(next_vblank - period)
            } else {
                None
            };
            let blank_end = previous_start.map(|start| start.saturating_add(blank_duration));
            let in_vblank = blank_end.is_some_and(|blank_end| cursor < blank_end);
            let (boundary, enters_vblank) = if in_vblank {
                (blank_end.expect("active VBlank has an end"), false)
            } else {
                (next_vblank, true)
            };
            let segment = (end - cursor).min(boundary.saturating_sub(cursor).max(1));

            let timer = &mut self.timers[1];
            let source = (timer.mode >> 8) & 0x3;
            let ticks = if matches!(source, 1 | 3) {
                hblank_ticks_between(cursor, cursor.saturating_add(segment), hsync)
            } else {
                segment
            };
            let sync = (timer.mode & MODE_SYNC_MODE_MASK) >> 1;
            let count_enabled = match sync {
                0 => !in_vblank,
                1 => true,
                2 => in_vblank,
                3 => timer.sync_seen,
                _ => unreachable!(),
            };
            if count_enabled {
                fired |= advance_counter(timer, ticks);
            }

            cursor += segment;
            if enters_vblank && cursor == boundary {
                let timer = &mut self.timers[1];
                match (timer.mode & MODE_SYNC_MODE_MASK) >> 1 {
                    1 | 2 => {
                        timer.counter = 0;
                        timer.accum = 0;
                    }
                    3 => timer.sync_seen = true,
                    _ => {}
                }
            }
        }

        fired
    }

    fn advance_timer0_synced(
        &mut self,
        start: u64,
        cycles: u64,
        hsync_period: u64,
        dot_clock_divisor: u64,
        vblank_period: u64,
    ) -> bool {
        let end = start.saturating_add(cycles);
        let hsync = hsync_period.max(1);
        let active_start = (H_ACTIVE_START_GPU * 7 / 11).min(hsync);
        let active_end = (H_ACTIVE_END_GPU * 7 / 11).min(hsync);
        let mut cursor = start;
        let mut fired = false;

        while cursor < end {
            let phase = cursor % hsync;
            let (in_hblank, until_boundary, enters_hblank) = if phase < active_start {
                (true, active_start - phase, false)
            } else if phase < active_end {
                (false, active_end - phase, true)
            } else {
                (true, hsync - phase, false)
            };
            let segment = (end - cursor).min(until_boundary.max(1));

            let timer = &mut self.timers[0];
            let source = (timer.mode >> 8) & 0x3;
            let ticks = if matches!(source, 1 | 3) {
                dot_ticks_between(
                    cursor,
                    cursor + segment,
                    hsync,
                    dot_clock_divisor,
                    vblank_period,
                )
            } else {
                segment
            };
            let sync = (timer.mode & MODE_SYNC_MODE_MASK) >> 1;
            let count_enabled = match sync {
                0 => !in_hblank,
                1 => true,
                2 => in_hblank,
                3 => timer.sync_seen,
                _ => unreachable!(),
            };
            if count_enabled {
                fired |= advance_counter(timer, ticks);
            }

            cursor += segment;
            if enters_hblank && cursor % hsync == active_end {
                let timer = &mut self.timers[0];
                match (timer.mode & MODE_SYNC_MODE_MASK) >> 1 {
                    1 | 2 => {
                        timer.counter = 0;
                        timer.accum = 0;
                    }
                    3 => timer.sync_seen = true,
                    _ => {}
                }
            }
        }

        fired
    }
}

fn advance_counter(t: &mut Timer, ticks: u64) -> bool {
    if ticks == 0 {
        return false;
    }
    // Pre-tick position.
    let old = t.counter;
    let target = t.target & 0xFFFF;
    let mut new_val = (old as u64) + ticks;

    let mut fired = false;
    let mut reached_target = false;
    let mut reached_wrap = false;

    if t.mode & MODE_RESET_AT_TARGET != 0 && target != 0 {
        // Silicon sequence for target=10 is 0,1,...,10,0,0,1,... . The
        // hidden phase after 10 is represented by `target_reset_hold`; the
        // next ordinary phase is zero as well. This exactly explains the
        // public test's 2:1 frequency for counter zero.
        let cycle_len = target as u64 + 2;
        let raw_old = if t.target_reset_hold {
            target as u64 + 1
        } else {
            (old as u64).min(target as u64)
        };
        let distance_to_target = if raw_old < target as u64 {
            target as u64 - raw_old
        } else {
            cycle_len - raw_old + target as u64
        };
        reached_target = ticks >= distance_to_target;
        let raw_new = (raw_old + ticks) % cycle_len;
        t.target_reset_hold = raw_new == target as u64 + 1;
        new_val = if t.target_reset_hold { 0 } else { raw_new };
    } else {
        t.target_reset_hold = false;
        if new_val > 0xFFFF {
            reached_wrap = true;
            new_val &= 0xFFFF;
        }

        // Detect target pass for non-reset-mode too.
        if (old as u64) < (target as u64) && new_val >= (target as u64) {
            reached_target = true;
        }
    }

    if reached_target {
        t.mode |= MODE_REACHED_TARGET;
        if t.mode & MODE_IRQ_ON_TARGET != 0 {
            fired |= fire_irq(t);
        }
    }
    if reached_wrap {
        t.mode |= MODE_REACHED_WRAP;
        if t.mode & MODE_IRQ_ON_WRAP != 0 {
            fired |= fire_irq(t);
        }
    }

    t.counter = new_val as u32;
    fired
}

fn fire_irq(t: &mut Timer) -> bool {
    let repeat = t.mode & MODE_IRQ_REPEAT != 0;
    if t.irq_fired_once && !repeat {
        return false;
    }
    if !repeat {
        t.irq_fired_once = true;
    }

    if t.mode & MODE_IRQ_TOGGLE != 0 {
        // Toggle mode holds the request level until the next eligible event;
        // only the high-to-low transition raises the edge-triggered I_STAT.
        t.mode ^= MODE_IRQ_ACTIVE_LOW;
        t.mode & MODE_IRQ_ACTIVE_LOW == 0
    } else {
        // Pulse mode briefly drives bit 10 low and then returns it high. Lazy
        // counter advancement observes the resulting IRQ edge, while an MMIO
        // read after the event sees the inactive high level (SCPH-9902 PX5
        // cases 36/37).
        t.mode |= MODE_IRQ_ACTIVE_LOW;
        true
    }
}

fn dot_ticks_per_scanline(hsync_period: u64, dot_clock_divisor: u64, vblank_period: u64) -> u64 {
    // Identify the region from frame geometry, not the relative HSync period.
    // Physical NTSC scanlines are about 2172 CPU clocks while PAL is about
    // 2167, the opposite ordering from the old Redux-parity constants. The
    // frame remains unambiguous at 263 versus 314 lines even while a mid-frame
    // display-mode switch temporarily retains the previous HSync cadence.
    let total_lines = vblank_period / hsync_period.max(1);
    let ntsc = total_lines < (263 + 314) / 2;
    match (ntsc, dot_clock_divisor) {
        (true, 10) => 341,
        (true, 8) => 426,
        (true, 7) => 487,
        (true, 5) => 682,
        (true, 4) => 853,
        (false, 10) => 340,
        // PAL 320-wide is the documented exception that rounds 425.75 up.
        (false, 8) => 426,
        (false, 7) => 486,
        (false, 5) => 681,
        (false, 4) => 851,
        (_, divisor) => 3413 / divisor.max(1),
    }
}

/// Count global HBlank-start edges in `(start, end]`. Timer 1's HBlank source
/// is beam-derived, so a mode write does not establish a new divider phase.
/// The edge uses the same active-display end as Timer 0 synchronization.
fn hblank_ticks_between(start: u64, end: u64, hsync_period: u64) -> u64 {
    let hsync = hsync_period.max(1);
    let phase = (H_ACTIVE_END_GPU * 7 / 11).min(hsync - 1);
    let edges_through = |cycle: u64| {
        if cycle < phase {
            0
        } else {
            (cycle - phase) / hsync + 1
        }
    };
    edges_through(end).saturating_sub(edges_through(start))
}

fn dot_ticks_between(
    start: u64,
    end: u64,
    hsync_period: u64,
    divisor: u64,
    vblank_period: u64,
) -> u64 {
    let hsync = hsync_period.max(1);
    let divisor = divisor.max(1);
    let nominal_denominator = 7 * divisor;
    let full_line_ticks = dot_ticks_per_scanline(hsync, divisor, vblank_period);
    let mut cursor = start;
    let mut ticks = 0;

    while cursor < end {
        let phase = cursor % hsync;
        let segment = (end - cursor).min(hsync - phase);
        let begin_ticks = phase * 11 / nominal_denominator;
        let segment_end = phase + segment;
        let end_ticks = if segment_end == hsync {
            full_line_ticks
        } else {
            segment_end * 11 / nominal_denominator
        };
        ticks += end_ticks.saturating_sub(begin_ticks);
        cursor += segment;
    }
    ticks
}

fn decode(phys: u32) -> (usize, u32) {
    let rel = phys - Timers::BASE;
    let idx = (rel / Timers::STRIDE) as usize;
    let off = rel % Timers::STRIDE;
    (idx, off)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HSYNC: u64 = 2172;
    const NTSC_PERIOD: u64 = NTSC_HSYNC * 263;
    const PAL_HSYNC: u64 = 2167;
    const PAL_PERIOD: u64 = PAL_HSYNC * 314;

    #[test]
    fn decodes_each_timer_and_field() {
        assert_eq!(decode(0x1F80_1100), (0, 0x0));
        assert_eq!(decode(0x1F80_1104), (0, 0x4));
        assert_eq!(decode(0x1F80_1108), (0, 0x8));
        assert_eq!(decode(0x1F80_1110), (1, 0x0));
        assert_eq!(decode(0x1F80_1124), (2, 0x4));
    }

    #[test]
    fn write_then_read_roundtrips() {
        let mut t = Timers::new();
        t.write32(0x1F80_1100, 0x1234, 0);
        assert_eq!(t.read32(0x1F80_1100), 0x1234);

        t.write32(0x1F80_1108, 0xABCD, 0);
        assert_eq!(t.read32(0x1F80_1108), 0xABCD);
    }

    #[test]
    fn mode_write_resets_counter() {
        let mut t = Timers::new();
        t.write32(0x1F80_1100, 0xFF, 0);
        t.write32(0x1F80_1104, 0x0001, 100); // mode write at cycle 100
        assert_eq!(t.read32(0x1F80_1100), 0);
        assert_eq!(t.timers[0].last_reset_cycle, 100);
    }

    #[test]
    fn upper_bits_masked_to_16() {
        let mut t = Timers::new();
        t.write32(0x1F80_1100, 0x1234_5678, 0);
        assert_eq!(t.read32(0x1F80_1100), 0x5678);
    }

    #[test]
    fn contains_covers_full_bank() {
        assert!(Timers::contains(0x1F80_1100));
        assert!(Timers::contains(0x1F80_1128));
        assert!(!Timers::contains(0x1F80_1130));
        assert!(!Timers::contains(0x1F80_10FF));
    }

    #[test]
    fn timer0_dot_clock_source_ticks_at_divisor_rate() {
        let mut t = Timers::new();
        // Set Timer 0 mode with clock source = 1 (dot clock).
        t.write32(0x1F80_1104, 1 << 8, 0);
        // At 320-wide the GPU pixel divisor is 8, and the GPU clock is
        // CPU * 11/7, so a dot tick is 7*8/11 ≈ 5.09 system cycles.
        // 80 cycles produce 15 dots. The mode write holds the counter for two
        // CPU clocks before source conversion, which does not discard a dot
        // at this phase.
        let fired = t.tick(80, NTSC_HSYNC, 8);
        assert_eq!(fired, 0);
        assert_eq!(t.read32(0x1F80_1100) & 0xFFFF, 15);
        // Another 40 cycles → accum carries the remainder → 8 more ticks.
        t.tick(40, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1100) & 0xFFFF, 23);
    }

    #[test]
    fn timer0_system_clock_source_unaffected_by_divisor() {
        let mut t = Timers::new();
        t.write32(0x1F80_1104, 0, 0); // source 0 = system clock
                                      // Divisor changes shouldn't matter at system-clock source.
        t.tick(100, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1100) & 0xFFFF, 98);
    }

    #[test]
    fn timer1_sync_mode_3_waits_for_vblank_then_free_runs() {
        let mut t = Timers::new();
        t.write32(0x1F80_1114, MODE_SYNC_ENABLE | (3 << 1), 0);
        let period = NTSC_PERIOD;
        t.advance_to_video(500, NTSC_HSYNC, 8, 1000, period);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 0);
        t.advance_to_video(1000, NTSC_HSYNC, 8, 1000, period);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 0);
        t.advance_to_video(1500, NTSC_HSYNC, 8, 1000 + period, period);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 500);
    }

    #[test]
    fn timer1_hblank_source_counts_global_edges_across_a_field() {
        let mut t = Timers::new();
        t.write32(0x1F80_1114, 1 << 8, 0);
        t.advance_to_video(NTSC_PERIOD, NTSC_HSYNC, 8, NTSC_PERIOD, NTSC_PERIOD);
        assert_eq!(t.read32(0x1F80_1110), 263);
    }

    #[test]
    fn counter_read_latch_holds_only_the_selected_timer() {
        let mut t = Timers::new();
        t.write32(0x1F80_1104, 0, 0);
        t.write32(0x1F80_1114, 0, 0);
        t.advance_to(4, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1100), 2);
        assert_eq!(t.read32(0x1F80_1110), 2);

        t.hold_counter_for_read(0x1F80_1100, 2);
        t.advance_to(6, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1100), 2);
        assert_eq!(t.read32(0x1F80_1110), 4);
    }

    #[test]
    fn timer1_sync_mode_1_resets_at_vblank() {
        let mut t = Timers::new();
        t.write32(0x1F80_1114, MODE_SYNC_ENABLE | (1 << 1), 0);
        let period = NTSC_PERIOD;
        t.advance_to_video(900, NTSC_HSYNC, 8, 1000, period);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 898);
        t.advance_to_video(1000, NTSC_HSYNC, 8, 1000, period);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 0);
    }

    #[test]
    fn timer1_sync_mode_2_pauses_outside_vblank() {
        let mut t = Timers::new();
        t.write32(0x1F80_1114, MODE_SYNC_ENABLE | (2 << 1), 0);
        t.advance_to_video(900, NTSC_HSYNC, 8, 1000, NTSC_PERIOD);
        assert_eq!(t.read32(0x1F80_1110) & 0xFFFF, 0);
    }

    #[test]
    fn timer2_sync_mode_1_free_runs() {
        let mut t = Timers::new();
        // Sync enable + sync mode 1 on Timer 2 = free-run.
        t.write32(0x1F80_1124, MODE_SYNC_ENABLE | (1 << 1), 0);
        t.tick(100, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1120) & 0xFFFF, 98);
    }

    #[test]
    fn timer2_sync_mode_0_stops() {
        let mut t = Timers::new();
        // Sync enable + sync mode 0 on Timer 2 = stop.
        t.write32(0x1F80_1124, MODE_SYNC_ENABLE, 0);
        t.tick(100, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1120) & 0xFFFF, 0);
    }

    #[test]
    fn reading_mode_clears_reached_flags() {
        let mut t = Timers::new();
        t.timers[1].mode = MODE_REACHED_TARGET | MODE_REACHED_WRAP | MODE_IRQ_ACTIVE_LOW;
        assert_eq!(
            t.read32(0x1F80_1114),
            MODE_REACHED_TARGET | MODE_REACHED_WRAP | MODE_IRQ_ACTIVE_LOW
        );
        assert_eq!(t.timers[1].mode, MODE_IRQ_ACTIVE_LOW);
    }

    #[test]
    fn mode_write_preserves_reached_flags_until_read() {
        let mut t = Timers::new();
        t.timers[2].mode = MODE_REACHED_TARGET | MODE_REACHED_WRAP;

        t.write32(0x1F80_1124, MODE_IRQ_ON_WRAP, 100);

        assert_eq!(
            t.read32(0x1F80_1124),
            MODE_IRQ_ON_WRAP | MODE_IRQ_ACTIVE_LOW | MODE_REACHED_TARGET | MODE_REACHED_WRAP
        );
        assert_eq!(
            t.read32(0x1F80_1124),
            MODE_IRQ_ON_WRAP | MODE_IRQ_ACTIVE_LOW
        );
    }

    #[test]
    fn reset_at_target_sets_sticky_reached_target() {
        let mut t = Timers::new();
        t.write32(0x1F80_1128, 10, 0);
        t.write32(0x1F80_1120, 0, 0);
        t.write32(0x1F80_1124, MODE_RESET_AT_TARGET, 0);

        t.advance_to(12, NTSC_HSYNC, 8);

        let mode = t.read32(0x1F80_1124);
        assert_ne!(mode & MODE_REACHED_TARGET, 0);
        assert_eq!(t.read32(0x1F80_1124) & MODE_REACHED_TARGET, 0);
        assert_eq!(t.read32(0x1F80_1120), 10, "target remains visible");
        t.advance_to(13, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1120), 0, "reset phase");
        t.advance_to(14, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1120), 0, "ordinary zero phase");
        t.advance_to(15, NTSC_HSYNC, 8);
        assert_eq!(t.read32(0x1F80_1120), 1);
    }

    #[test]
    fn one_shot_pulse_irq_releases_mode_bit_without_rearming() {
        let mut t = Timers::new();
        t.write32(0x1F80_1128, 10, 0);
        t.write32(0x1F80_1124, MODE_RESET_AT_TARGET | MODE_IRQ_ON_TARGET, 0);

        assert_eq!(t.advance_to(12, NTSC_HSYNC, 8), 1 << 2);
        let mode = t.read32(0x1F80_1124);
        assert_ne!(mode & MODE_REACHED_TARGET, 0);
        assert_ne!(
            mode & MODE_IRQ_ACTIVE_LOW,
            0,
            "the short active-low pulse is over before software reads mode"
        );

        assert_eq!(
            t.advance_to(36, NTSC_HSYNC, 8),
            0,
            "one-shot mode suppresses later target edges"
        );
        t.write32(0x1F80_1124, MODE_RESET_AT_TARGET | MODE_IRQ_ON_TARGET, 36);
        assert_eq!(t.advance_to(48, NTSC_HSYNC, 8), 1 << 2);
    }

    #[test]
    fn dot_clock_line_totals_follow_video_region() {
        let cases = [
            (10, 341, 340),
            (8, 426, 426),
            (7, 487, 486),
            (5, 682, 681),
            (4, 853, 851),
        ];

        for (divisor, ntsc_ticks, pal_ticks) in cases {
            assert_eq!(
                dot_ticks_between(0, NTSC_HSYNC, NTSC_HSYNC, divisor, NTSC_PERIOD),
                ntsc_ticks,
                "NTSC divisor {divisor}"
            );
            assert_eq!(
                dot_ticks_between(0, PAL_HSYNC, PAL_HSYNC, divisor, PAL_PERIOD),
                pal_ticks,
                "PAL divisor {divisor}"
            );
        }
    }

    #[test]
    fn dot_clock_region_detection_survives_retained_hsync_cadence() {
        // Mid-frame display-mode changes can retain the previous region's
        // HSync cadence in the VBlank scheduler. Frame geometry must still
        // select the new region's documented dot totals.
        assert_eq!(dot_ticks_per_scanline(PAL_HSYNC, 4, NTSC_HSYNC * 314), 851);
        assert_eq!(dot_ticks_per_scanline(NTSC_HSYNC, 4, PAL_HSYNC * 263), 853);
    }
}
