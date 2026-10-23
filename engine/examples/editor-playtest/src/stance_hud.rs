//! The stance HUD in the game: the pixel-art sprite HUD of `stance_hud_sprites.rs` and
//! `stance_hud_packets.rs`, fed from the real stance, health and ammunition state.
//!
//! Its packets are built once when gameplay starts, with the atlas and palettes in VRAM, and every
//! frame only the few that changed are patched (a health cell's palette, a pill, a bar sliding)
//! and the packets are linked into slot 0 of the frame's ordering table, so the HUD is drawn
//! before the overlay and after the world. Nothing is rebuilt while the game runs.
//!
//! `update` runs on the 60 Hz simulation clock. The HUD's own animations run at 30 steps a second
//! (the pace they were drawn for), so every second tick steps them: they take the same time on the
//! screen whatever the frame rate is.

use psx_engine::OtFrame;
use psx_game_runtime::vitality::VitalityChannelId;

use super::*;
use crate::hud_packets::{HudPackets, HudVram};
use crate::hud_sprites::{
    AmmoFx, FoldPolicy, FoldTriggers, HudAnim, HudState, Stance, COUNT, ENERGY_PER_PILL, SWAP_TICKS,
};

/// Where the HUD's top-left sits on the screen.
const ORIGIN: (i32, i32) = (6, 6);

/// A hostile this close (runtime units) unfolds the HUD, and has to be farther than the second
/// distance to count as gone. They are the lock-on distances, so the HUD is out whenever an
/// enemy could be targeted, and the margin keeps it from flickering at the edge.
pub(crate) const HUD_ENEMY_ENTER: i32 = LOCK_RANGE;
pub(crate) const HUD_ENEMY_LEAVE: i32 = LOCK_BREAK_RANGE;

/// Keep the ammunition pills showing when the bars are folded.
pub(crate) const HUD_FOLDED_AMMO: bool = false;

/// Measurement features: `stance-hud-open` keeps the HUD unfolded, `stance-hud-off` draws no HUD,
/// so that the HUD's cost is a difference of two runs of the same route. Neither is on in a
/// shipping build.
const FORCE_OPEN: bool = cfg!(feature = "stance-hud-open");
const OFF: bool = cfg!(feature = "stance-hud-off");

/// Ticks between looks for the nearest hostile: the HUD only needs it a few times a second.
const SCAN_TICKS: u8 = 8;

/// The packets live in a `static`: the GPU walks them after `render` returns.
static mut PACKETS: HudPackets = HudPackets::new();

/// The HUD's state, in a `static` for the same reason: the scene is zero-initialised storage and
/// this is not all zero.
static mut HUD: StanceHud = StanceHud::new();

/// The HUD of the game.
pub(crate) fn hud() -> &'static mut StanceHud {
    // SAFETY: single-threaded guest; the HUD is only used from the scene's update, render and
    // gameplay start, one after another.
    unsafe { &mut *core::ptr::addr_of_mut!(HUD) }
}

/// What the HUD needs from the game, once a simulation tick.
#[derive(Clone, Copy)]
pub(crate) struct HudInput {
    /// The active stance.
    pub active: VitalityChannelId,
    /// Each channel's health as a Q12 share, indexed like [`VitalityChannelId`].
    pub share_q12: [u16; 2],
    /// Ammunition energy, 0 to 100.
    pub energy: u16,
    /// The squared distance to the nearest live hostile, if any.
    pub nearest_hostile_sq: Option<i32>,
    /// Lock-on or aiming.
    pub targeting: bool,
    /// Attached to an arch.
    pub attached: bool,
}

/// The HUD's state between ticks.
pub(crate) struct StanceHud {
    vram: Option<HudVram>,
    anim: HudAnim,
    ammo: AmmoFx,
    policy: FoldPolicy,
    state: HudState,
    cells: [u8; 2],
    energy_units: u16,
    parity: bool,
    /// The stance in front, and the ticks since it became so while the bars trade places
    /// (`SWAP_TICKS` and over: not swapping).
    front: Stance,
    swap_ticks: u16,
    scan: u8,
    nearest: Option<i32>,
    ready: bool,
}

impl StanceHud {
    /// A HUD that is not set up yet.
    pub(crate) const fn new() -> Self {
        Self {
            vram: None,
            anim: HudAnim::open(),
            ammo: AmmoFx::new(100),
            policy: FoldPolicy::new(),
            state: HudState::rest(Stance::Horizon, [10, 10], 10),
            cells: [10, 10],
            energy_units: 100,
            parity: false,
            front: Stance::Horizon,
            swap_ticks: SWAP_TICKS,
            scan: 0,
            nearest: None,
            ready: false,
        }
    }

    /// Put the atlas and palettes in VRAM and build the packets, with `input` as the first state.
    /// When VRAM has no room the HUD stays off.
    pub(crate) fn init(&mut self, input: &HudInput) {
        self.ready = false;
        if OFF {
            return;
        }
        let Some(vram) = upload_stance_hud() else {
            return;
        };
        self.cells = [cells(input.share_q12[0]), cells(input.share_q12[1])];
        // SAFETY: single-threaded guest; PACKETS is only touched here and in `link`/`update`.
        unsafe { (*core::ptr::addr_of_mut!(PACKETS)).build(&vram, ORIGIN) };
        self.vram = Some(vram);
        self.anim = HudAnim::open();
        self.ammo = AmmoFx::new(input.energy);
        self.energy_units = input.energy;
        self.policy = FoldPolicy::new();
        self.parity = false;
        self.front = stance_of(input.active);
        self.swap_ticks = SWAP_TICKS;
        self.ready = true;
        self.refresh_state(input);
    }

    /// Advance one simulation tick.
    pub(crate) fn update(&mut self, input: &HudInput) {
        if !self.ready {
            return;
        }
        let cells_now = [cells(input.share_q12[0]), cells(input.share_q12[1])];
        let units = input.energy / ENERGY_PER_PILL;
        let damaged = cells_now
            .iter()
            .zip(self.cells)
            .any(|(now, before)| *now < before);
        // A change of stance starts the bars trading places, drawn from the tick they have reached
        // so that every frame the game renders shows them further on, whatever the frame rate.
        let front = stance_of(input.active);
        if front != self.front {
            self.front = front;
            self.swap_ticks = 0;
        }
        let swapping = self.swap_ticks < SWAP_TICKS;
        self.swap_ticks = self.swap_ticks.saturating_add(1);
        let changed =
            cells_now != self.cells || units != self.energy_units / ENERGY_PER_PILL || swapping;
        self.energy_units = input.energy;
        self.cells = cells_now;
        let hostile_near =
            self.policy
                .near(input.nearest_hostile_sq, HUD_ENEMY_ENTER, HUD_ENEMY_LEAVE);
        let open = FORCE_OPEN
            || self.policy.step(FoldTriggers {
                hostile_near,
                targeting: input.targeting,
                damaged,
                changed,
                attached: input.attached,
            });
        // The animations step every second tick, at 30 a second.
        self.parity = !self.parity;
        if self.parity {
            let front = stance_of(input.active);
            self.anim.step(open, front);
            self.ammo.step(input.energy);
        }
        self.refresh_state(input);
    }

    /// The state the packets are linked for.
    fn refresh_state(&mut self, input: &HudInput) {
        let stance = stance_of(input.active);
        let mut state = HudState::rest(stance, self.cells, 0);
        state.ammo_hint = HUD_FOLDED_AMMO;
        // A swap in progress: how far it has gone, with the new stance in front.
        if self.swap_ticks < SWAP_TICKS {
            state.swap = (u32::from(self.swap_ticks.max(1)) * 4096 / u32::from(SWAP_TICKS)) as u16;
        }
        self.anim.apply(&mut state);
        self.ammo.apply(&mut state);
        self.state = state;
    }

    /// Lay out the packets for the current state, if it changed, and link them into slot 0 of `ot`. Link before the
    /// rest of the frame so that the HUD is walked last of slot 0.
    pub(crate) fn link<const DEPTH: usize>(&mut self, ot: &mut OtFrame<'_, DEPTH>) {
        if !self.ready {
            return;
        }
        // SAFETY: single-threaded guest; the packets stay untouched until the walk ends, which
        // `submit` waits for before the next frame's update or link.
        let packets = unsafe { &mut *core::ptr::addr_of_mut!(PACKETS) };
        packets.link(&self.state, |ptr, tag| {
            // SAFETY: the packets are a `static`, so they outlive the walk.
            unsafe { ot.add_raw_tag_unchecked(0, ptr, tag) }
        });
    }
}

/// The HUD's stance for a vitality channel: Horizon is channel one, Zenith channel two.
fn stance_of(channel: VitalityChannelId) -> Stance {
    if channel == VitalityChannelId::One {
        Stance::Horizon
    } else {
        Stance::Zenith
    }
}

/// Cells lit by a Q12 share: rounded to the nearest tenth, and at least one while any health
/// remains.
fn cells(share_q12: u16) -> u8 {
    let tenths = (u32::from(share_q12) * 10 + 2048) >> 12;
    let tenths = tenths.min(COUNT as u32) as u8;
    if tenths == 0 && share_q12 > 0 {
        1
    } else {
        tenths
    }
}

impl Playtest {
    /// What the HUD needs from the game this tick.
    pub(crate) fn stance_hud_input(&mut self) -> HudInput {
        const VITALITY_Q12_ONE: u32 = 4096;
        let share = |channel| {
            let pool = self.player_vitality.pool(channel);
            if pool.maximum() == 0 {
                0
            } else {
                (u32::from(pool.current()) * VITALITY_Q12_ONE / u32::from(pool.maximum())) as u16
            }
        };
        // The nearest live hostile, looked for a few times a second.
        hud().scan = hud().scan.saturating_sub(1);
        if hud().scan == 0 {
            hud().scan = SCAN_TICKS;
            let player = self.motor.position();
            let mut nearest: Option<i32> = None;
            for index in 0..MODEL_INSTANCES.len() {
                let Some(point) = self.target_position(index) else {
                    continue;
                };
                let dist = square_i32_saturating(point.x.saturating_sub(player.x))
                    .saturating_add(square_i32_saturating(point.z.saturating_sub(player.z)));
                if nearest.is_none_or(|best| dist < best) {
                    nearest = Some(dist);
                }
            }
            hud().nearest = nearest;
        }
        HudInput {
            active: self.player_stance.active(),
            share_q12: [share(VitalityChannelId::One), share(VitalityChannelId::Two)],
            energy: self.combat_flow.energy,
            nearest_hostile_sq: hud().nearest,
            targeting: self.lock_target.is_some() || self.ranged_ready.aiming(),
            attached: self.hook_attached.is_some(),
        }
    }
}
