//! Floating combat damage numbers.
//!
//! A hit spawns a number at the point of impact; it rises and dissolves
//! over its lifetime, tinted by the damage channel that dealt it
//! (Horizon orange-red, Zenith teal -- the same two colours the HUD and
//! the Choir Needle projectile use). Both directions are shown: damage
//! the player deals and damage the player takes.
//!
//! # Why this is not a UI node
//!
//! These are transient gameplay particles with lifetimes, not authored
//! scene UI. They are spawned from the simulation, they expire on their
//! own, and nothing else can reference them. So they own their state
//! here and submit themselves in the overlay pass, rather than becoming
//! entries in the world-anchored UI tree.
//!
//! # The dissolve
//!
//! The PS1 has no per-texel alpha. The font atlas is a 4bpp CLUT of
//! white glyphs recoloured by the per-texel tint multiplier, so the
//! obvious fade -- ramp the tint toward black -- draws a *black* number
//! rather than an absent one, which reads as the text turning into a
//! dark smear over a lit wall.
//!
//! The mode that actually fades is additive semi-transparency:
//! `framebuffer + glyph`. Scaling the tint then scales the contribution,
//! and at tint zero the glyph adds nothing and is genuinely invisible.
//! That gives a true dissolve in and out with no pop at either end, and
//! it suits the material: these numbers should read as emissive, like
//! the rest of the game's combat feedback.
//!
//! # Seating the glyph
//!
//! Legibility over a lit surface is the cost of additive: adding can
//! only push a pixel toward the tint's own hue, so an orange Horizon
//! number over the enemy's warm chest converges with it and a number
//! over the player's pale body washes out to near-white. Neither is a
//! tuning problem -- it is what additive does.
//!
//! The fix is a SUBTRACTIVE backing plate: one flat rectangle behind
//! the number, strong enough to floor whatever is under it. The glyphs
//! then land on black rather than on the scene, so a number reads as
//! its authored channel colour wherever it falls. Where the background
//! is already dark -- most of this game -- the subtract clamps to zero
//! and the plate is invisible, so it costs contrast only where contrast
//! was missing.
//!
//! Three things were measured against it and lost:
//!
//! - A one-pixel-offset drop shadow (the first version shipped). At one
//!   pixel the subtract lands mostly INSIDE the glyph rather than around
//!   it, so it darkened the number's own strokes instead of separating
//!   them, and left the additive wash untouched.
//! - A per-glyph knockout: the same subtract at zero offset. This works
//!   and looks nearly identical to the plate, but it is a second full
//!   pass over the glyphs -- four words each -- where the plate is three
//!   words however long the number is.
//! - Plate AND knockout together. Indistinguishable from the plate
//!   alone, because the plate has already floored everything the
//!   knockout would have.
//!
//! [`TextBlend::Average`] is not an option for the plate even though it
//! is the obvious "semi-transparent box": at tint zero it still halves
//! the background, so it cannot fade out and would be left behind as a
//! grey rectangle after the number had gone.

use super::*;
use psx_font::TextBlend;
use psx_game_runtime::destructibles::DamageChannel;

/// Live numbers the pool can show at once.
///
/// Sized to what can plausibly be on screen, not to a table capacity.
/// With [`NUMBER_LIFETIME_TICKS`] at 0.8 s: the player's swing cooldown
/// lets about two of their swings overlap in that window, this map
/// cooks one enemy (`GAME_ENTITIES` has a single record) and a swing
/// arc realistically catches one or two, and the incoming side is
/// bounded by the enemy's 45-tick attack cooldown plus a projectile
/// impact, so about two. Six covers that with headroom and costs
/// [`core::mem::size_of::<DamageNumbers>()`] bytes of BSS.
///
/// Deliberately NOT sized to `MAX_GAME_ENTITIES` (64): a slot per
/// cookable entity would reserve storage for a brawl the game does not
/// have, and past about four simultaneous numbers the screen is
/// unreadable anyway. When the pool is full the oldest slot is
/// recycled, so a burst always shows the most recent hits.
const MAX_DAMAGE_NUMBERS: usize = 6;

/// How long a number lives, in 60 Hz simulation ticks.
const NUMBER_LIFETIME_TICKS: u32 = 48;

/// Ticks spent fading in from nothing.
const FADE_IN_TICKS: u32 = 6;

/// Ticks spent fading back out to nothing.
const FADE_OUT_TICKS: u32 = 18;

/// Total screen-space rise over a full lifetime, in pixels.
///
/// Screen space rather than world space on purpose: the anchor stays
/// pinned to the world point (so the number tracks the enemy as the
/// camera moves), but the rise itself is a constant number of pixels,
/// so a hit at the far end of a corridor drifts as legibly as one in
/// the player's face. A world-space rise would be almost motionless at
/// range.
const NUMBER_RISE_PIXELS: i32 = 22;

/// Fraction of a struck actor's height at which its number floats,
/// Q8. Chest height, deliberately NOT over the head: the enemy health
/// bar lives above the head, and a number rising into it would read as
/// one broken widget rather than two working ones.
const STRUCK_ACTOR_LIFT_Q8: i32 = 192;

/// Fraction of the player's height at which a number for damage TAKEN
/// floats, Q8. Well clear of the head, for a reason the first capture
/// made obvious: the player's body is pale blue and fills the middle of
/// the frame, and pale blue is the one background additive text cannot
/// survive -- the glyphs saturated to white and vanished. The collision
/// capsule is also shorter than the rendered body, so clearing it needs
/// more than a full height.
const PLAYER_HIT_LIFT_Q8: i32 = 400;

/// Authored Horizon channel colour, shared with the HUD.
const HORIZON_RGB: (u8, u8, u8) = (214, 75, 48);

/// Authored Zenith channel colour, shared with the HUD and the Choir
/// Needle projectile tint.
const ZENITH_RGB: (u8, u8, u8) = (67, 169, 154);

/// Peak strength of the subtractive backing plate. The tint multiplier
/// is `texel * tint / 128`, so 104 subtracts 207/255 of each channel at
/// full envelope.
///
/// Chosen off a sweep over this level's real surfaces at the shipping
/// 25% light intensity: 72 (the old drop shadow's value) lets the
/// enemy's lit chest bleed through as a warm cast, 104 floors every
/// surface in the room, and 128 -- a mathematically complete knockout
/// -- is indistinguishable from 104 on any of them. 104 is the smaller
/// number that is already enough.
const PLATE_STRENGTH: i32 = 104;

/// Margin between the plate's edge and the digits it seats, in pixels.
const PLATE_PAD_X: i32 = 2;
/// Vertical counterpart to [`PLATE_PAD_X`].
const PLATE_PAD_Y: i32 = 2;

/// The band of the glyph cell that digits actually ink, as a row offset
/// from the text origin and a height.
///
/// The plate is sized to THIS, not to `line_height`. A display face's
/// cell is full height and mostly leading -- [`DAMAGE_NUMBER_FACE`]
/// inks rows 1..10 of a 14-row cell -- so a plate sized to the cell
/// hangs four empty rows below the digits and reads as a box the number
/// is sitting on top of rather than one it is inside. Sizing to the ink
/// also makes the plate smaller, which is the cheaper thing to fill.
///
/// Hand-written but not unchecked: `the_plate_is_sized_to_the_digits`
/// recomputes both from the face's own bitmap.
const DIGIT_INK_TOP: i32 = 1;
/// Height of the band described by [`DIGIT_INK_TOP`].
const DIGIT_INK_HEIGHT: i32 = 10;

/// Horizontal screen offset for a number over a struck actor, pixels.
///
/// The HUD draws a 34px-wide enemy health bar over the same actor, at
/// `model_height * 1.25`. Centring the number under it works at the
/// distance it was first captured, but a rise is a fixed number of
/// SCREEN pixels while the gap to the bar shrinks with distance, so at
/// range the number would climb into the bar. Stepping it to one side
/// clears the bar at every distance for one add, instead of making the
/// rise depend on how far away the target is.
///
/// Half the bar's width, plus half the widest value this level's
/// weapons can deal, plus a margin.
///
/// Has grown twice with the face -- 26 for the original 5x8, 32 for a
/// 10px-advance display face, 36 for [`DAMAGE_NUMBER_FACE`]'s 12px one
/// -- because what it has to clear is a font metric, not a layout
/// choice. Each step is the smallest value that leaves any clearance at
/// all, since every pixel here also pushes the number further from the
/// point it was dealt at. The test at the bottom of this file recomputes
/// the requirement from the font and the cooked weapon table, so the
/// next face change cannot regress it silently.
const STRUCK_ACTOR_X_OFFSET: i8 = 36;

/// The face damage numbers are drawn in.
///
/// Deliberately NOT the HUD's face. HUD chrome is set in a compact 5x8
/// italic that suits a static label read at leisure; a damage number is
/// read in a glance while both it and the camera are moving, so it
/// wants the opposite -- upright, heavy strokes, and digits that cannot
/// be mistaken for each other.
///
/// Picked from a comparison sheet rendered over this level's real
/// surfaces at the shipping 25% light intensity. `KENNEY_FUTURE_NARROW`
/// carries two-pixel strokes on an 8px-wide digit set in a 12px cell,
/// so the four columns of built-in letterspacing keep a two-digit value
/// from fusing into one blob while the camera moves. All ten digits are
/// pixel-distinct; the closest pair still differs in six pixels.
///
/// The extra advance is the whole cost of the choice, and it is paid in
/// exactly one place -- [`STRUCK_ACTOR_X_OFFSET`], which has to be wide
/// enough to walk the number around the enemy health bar.
///
/// The obvious weight candidate, `KENNEY_BLOCKS`, is ruled out on
/// legibility rather than weight: its cut-out display forms make `0`/`8`
/// and `4`/`1` nearly identical at this size.
///
/// Alternates that also passed, if this reads too wide in motion:
/// `KENNEY_MINI_SQUARE` (12x16, the same two-pixel weight in a tighter
/// advance), `BASIC_8X16` (8x16, lighter and narrower again).
pub(super) const DAMAGE_NUMBER_FACE: &psx_font::BitmapFont = &psx_font::fonts::KENNEY_FUTURE_NARROW;

/// Damage channel a number is tinted by.
///
/// Stored as a `u8` in the pool so the whole scene's zeroed BSS is a
/// valid empty pool; `Horizon` is deliberately the zero value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum DamageNumberChannel {
    /// R1/R2 player attacks and legacy untyped enemy damage.
    Horizon,
    /// L1/L2 player attacks and the Choir Needle projectile.
    Zenith,
}

impl DamageNumberChannel {
    const fn to_raw(self) -> u8 {
        match self {
            Self::Horizon => 0,
            Self::Zenith => 1,
        }
    }

    const fn rgb_from_raw(raw: u8) -> (u8, u8, u8) {
        if raw == 0 {
            HORIZON_RGB
        } else {
            ZENITH_RGB
        }
    }
}

/// Where a number is pinned: a world point plus a horizontal screen
/// nudge applied after projection.
///
/// The nudge is screen-space rather than world-space because what it
/// has to avoid -- the HUD's enemy health bar -- is itself positioned
/// in screen space, and because a world-space sidestep would swing
/// across the target as the camera orbits.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct DamageNumberAnchor {
    world: [i32; 3],
    x_offset: i8,
}

/// Anchor for a number over an actor the player struck: chest height on
/// the struck body, stepped aside so it never climbs into the enemy
/// health bar the HUD hangs above the same actor.
pub(super) fn struck_actor_anchor(position: [i32; 3], height: u16) -> DamageNumberAnchor {
    DamageNumberAnchor {
        world: lift(position, height, STRUCK_ACTOR_LIFT_Q8),
        x_offset: STRUCK_ACTOR_X_OFFSET,
    }
}

/// Anchor for a number for damage the PLAYER took: above the player's
/// head, off the pale body that would swallow it. No sidestep -- the
/// player's own HUD lives in the screen corners, not over the body.
pub(super) fn player_hit_anchor(position: [i32; 3], height: u16) -> DamageNumberAnchor {
    DamageNumberAnchor {
        world: lift(position, height, PLAYER_HIT_LIFT_Q8),
        x_offset: 0,
    }
}

fn lift(position: [i32; 3], height: u16, fraction_q8: i32) -> [i32; 3] {
    [
        position[0],
        position[1].saturating_add((i32::from(height).saturating_mul(fraction_q8)) >> 8),
        position[2],
    ]
}

/// Map the combat system's damage channel onto the presentation one.
///
/// They are separate types on purpose: `DamageChannel` also drives
/// destructible affinity matching, and a number's tint should not
/// become a reason not to change that.
pub(super) const fn damage_number_channel_for(channel: DamageChannel) -> DamageNumberChannel {
    match channel {
        DamageChannel::Horizon => DamageNumberChannel::Horizon,
        DamageChannel::Zenith => DamageNumberChannel::Zenith,
    }
}

/// One floating number.
///
/// Plain integers throughout: the scene lives in link-time-zeroed BSS
/// and is made valid field by field, so every field here has to be
/// valid as all-zero bytes. All-zero is `live == 0`, an empty slot.
#[derive(Copy, Clone, Debug)]
struct DamageNumberSlot {
    /// Non-zero while the slot holds a number.
    live: u8,
    /// [`DamageNumberChannel::to_raw`].
    channel: u8,
    /// Damage shown.
    amount: u16,
    /// Gameplay tick the number was spawned on.
    spawn_tick: u32,
    /// Room the anchor belongs to; a number from another room is not
    /// drawn, since its room-local coordinates mean nothing here.
    room: RoomIndex,
    /// Room-local world anchor, already lifted to display height.
    anchor: [i32; 3],
    /// Horizontal screen-space nudge applied after projection, pixels.
    /// Fits in the slot's existing tail padding, so it is free.
    x_offset: i8,
}

/// Fixed-capacity pool of floating damage numbers.
#[derive(Copy, Clone, Debug)]
pub(super) struct DamageNumbers {
    slots: [DamageNumberSlot; MAX_DAMAGE_NUMBERS],
    /// Bit `i` set while slot `i` holds a number.
    ///
    /// Redundant with the per-slot `live` flag, and worth the byte: it
    /// makes the overwhelmingly common case -- no combat feedback on
    /// screen, which is most of any play session -- a single compare
    /// instead of walking the pool every visual frame. Numbers are the
    /// kind of system that must cost nothing when it is doing nothing.
    live_mask: u8,
}

impl Default for DamageNumbers {
    fn default() -> Self {
        Self {
            slots: [DamageNumberSlot {
                live: 0,
                channel: 0,
                amount: 0,
                spawn_tick: 0,
                room: RoomIndex(0),
                anchor: [0; 3],
                x_offset: 0,
            }; MAX_DAMAGE_NUMBERS],
            live_mask: 0,
        }
    }
}

impl DamageNumbers {
    /// Spawn a number for `amount` damage at a room-local world point.
    ///
    /// `anchor` is the final display point -- build it with
    /// [`struck_actor_anchor`] or [`player_hit_anchor`] rather than
    /// passing a raw foot position, since where the number sits is the
    /// difference between legible and invisible.
    ///
    /// Zero-damage connections are dropped: a "0" floating off an enemy
    /// is noise, not feedback. `now` is the gameplay tick, the same
    /// clock the overlay pass reads, so a number's age is independent
    /// of the frame rate.
    pub(super) fn spawn(
        &mut self,
        anchor: DamageNumberAnchor,
        room: RoomIndex,
        amount: u16,
        channel: DamageNumberChannel,
        now: SimTick,
    ) {
        if amount == 0 {
            return;
        }
        let now = now.as_u32();
        let slot = self.recycle_slot(now);
        self.live_mask |= 1 << slot;
        self.slots[slot] = DamageNumberSlot {
            live: 1,
            channel: channel.to_raw(),
            amount,
            spawn_tick: now,
            room,
            anchor: anchor.world,
            x_offset: anchor.x_offset,
        };
    }

    /// Index of the slot a new number should take: the first expired
    /// one, else the oldest live one.
    ///
    /// Recycling the oldest rather than dropping the newest matters --
    /// the hit that just landed is the one the player is looking for,
    /// and the number it would replace is already most of the way
    /// through its fade.
    fn recycle_slot(&self, now: u32) -> usize {
        let mut oldest = 0usize;
        let mut oldest_age = 0u32;
        for (index, slot) in self.slots.iter().enumerate() {
            let age = slot_age(slot, now);
            let expired = slot.live == 0 || age >= NUMBER_LIFETIME_TICKS;
            if expired {
                return index;
            }
            if age >= oldest_age {
                oldest_age = age;
                oldest = index;
            }
        }
        oldest
    }

    /// Draw every live number for this visual frame.
    ///
    /// Returns how many were actually submitted, so the caller can
    /// report the at-rest versus in-combat cost separately.
    pub(super) fn draw(
        &mut self,
        font: &FontAtlas,
        camera: WorldCamera,
        room: RoomIndex,
        now: SimTick,
    ) -> usize {
        // The whole system's at-rest cost: one compare. Everything
        // below, including reading the font handle, is skipped while no
        // hit is being shown.
        if self.live_mask == 0 {
            return 0;
        }
        let now = now.as_u32();
        let mut drawn = 0usize;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.live == 0 {
                continue;
            }
            let age = slot_age(slot, now);
            if age >= NUMBER_LIFETIME_TICKS {
                // Retire here rather than in a separate pass: the draw
                // is the only thing that reads a number, so expiry has
                // no observer to be late for.
                self.live_mask &= !(1 << index);
                continue;
            }
            if slot.room != room {
                continue;
            }
            let envelope = envelope_q8(age);
            if envelope == 0 {
                continue;
            }
            // Reuse the shared projection rather than a private copy:
            // this is the same camera transform the world pass used, so
            // the number lands exactly where the geometry did.
            let Some(projected) = camera.project_world(RoomPoint::new(
                slot.anchor[0],
                slot.anchor[1],
                slot.anchor[2],
            )) else {
                continue;
            };

            let mut digits = [0u8; DIGIT_BUFFER];
            let text = format_amount(&mut digits, slot.amount);
            let width = font.text_width(text) as i32;
            let x = i32::from(projected.sx) - width / 2 + i32::from(slot.x_offset);
            let y = i32::from(projected.sy) - rise_pixels(age);
            if !on_screen(x, y, width) {
                continue;
            }
            let (x, y) = (x as i16, y as i16);

            // The plate first, so the glyphs land on black rather than
            // on the scene. Three words however long the number is.
            let plate = plate_strength(envelope);
            font.draw_backdrop_blended(
                x - PLATE_PAD_X as i16,
                y + (DIGIT_INK_TOP - PLATE_PAD_Y) as i16,
                (width + PLATE_PAD_X * 2) as u16,
                (DIGIT_INK_HEIGHT + PLATE_PAD_Y * 2) as u16,
                (plate, plate, plate),
                TextBlend::Subtract,
            );

            // The sign and the digits are separate runs because the
            // sign has to drop to the digits' mid-height; see
            // [`MINUS_DROP`]. The split costs one extra draw-mode word
            // and no extra glyphs -- the same characters are drawn
            // either way.
            let tint = channel_tint(slot.channel, envelope);
            font.draw_text_blended(x, y + MINUS_DROP, MINUS_SIGN, tint, TextBlend::Add);
            font.draw_text_blended(
                x + font.glyph_advance(MINUS_CHAR) as i16,
                y,
                &text[MINUS_SIGN.len()..],
                tint,
                TextBlend::Add,
            );
            drawn += 1;
        }
        drawn
    }
}

/// Widest string a number renders to: the sign plus a `u16`'s five
/// decimal digits.
const DIGIT_BUFFER: usize = 6;

/// The sign every damage number carries. Damage is always a loss to
/// whoever took it, in both directions, so it is always negative --
/// there is no case here that would want a `+`.
const MINUS_SIGN: &str = "-";

/// [`MINUS_SIGN`] as the character the font is measured by.
const MINUS_CHAR: char = '-';

/// Rows the sign drops to sit at the digits' mid-height.
///
/// Kenney's faces are drawn for all-caps display text, so their hyphen
/// sits at cap-middle -- which against full-height digits is cap-TOP,
/// and reads as an overbar rather than a minus. Dropping it is why the
/// sign is drawn as its own run instead of being folded into the same
/// string as the digits.
///
/// A hand-written constant, but not an unchecked one:
/// `the_sign_sits_at_the_digits_mid_height` recomputes it from the
/// face's own bitmap and fails if a face change moves it.
const MINUS_DROP: i16 = 4;

/// Age of a number in simulation ticks, clamped to zero while its spawn
/// tick is still in the future.
///
/// That case is normal, not defensive. A hit is stamped with the tick
/// the simulation is on; the overlay pass reads `overlay_sim_tick`, a
/// snapshot taken during render and published a frame later, so it
/// trails the simulation. A number spawned this tick is therefore
/// routinely "younger than zero" for a frame or two.
///
/// Letting that wrap to a near-`u32::MAX` age made every number look
/// instantly expired. Without the idle mask the pool merely skipped a
/// couple of frames and recovered; once the mask started believing the
/// verdict and retiring the slot, the number never drew at all. Both
/// halves of that bug live here.
fn slot_age(slot: &DamageNumberSlot, now: u32) -> u32 {
    let delta = now.wrapping_sub(slot.spawn_tick);
    if delta > u32::MAX / 2 {
        0
    } else {
        delta
    }
}

/// Fade envelope over a number's life, Q8 (`256` = fully present).
///
/// Ramps up, holds, ramps down. The hold is the longest phase so the
/// number is readable for most of its life rather than spending it all
/// in transition.
fn envelope_q8(age: u32) -> i32 {
    if age >= NUMBER_LIFETIME_TICKS {
        return 0;
    }
    if age < FADE_IN_TICKS {
        return (age as i32 * 256) / FADE_IN_TICKS as i32;
    }
    let fade_out_start = NUMBER_LIFETIME_TICKS - FADE_OUT_TICKS;
    if age < fade_out_start {
        return 256;
    }
    let remaining = NUMBER_LIFETIME_TICKS - age;
    (remaining as i32 * 256) / FADE_OUT_TICKS as i32
}

/// Screen-space rise at `age`, in pixels.
fn rise_pixels(age: u32) -> i32 {
    (age as i32 * NUMBER_RISE_PIXELS) / NUMBER_LIFETIME_TICKS as i32
}

/// Plate tint at a given envelope, scaled QUADRATICALLY.
///
/// Linear scaling was measurably wrong. Subtracting from a lit surface
/// is more visible than adding to it at the same strength, so a
/// linearly-faded subtract outran its own number: the first captures
/// showed the value arriving as a black silhouette and only then
/// blooming into channel colour. Squaring holds the plate back until
/// the number it is clearing space for has actually arrived, which is
/// what makes both ends read as a dissolve rather than a smudge.
///
/// The easing matters more for a plate than it did for the drop shadow
/// it replaced: the plate is a solid rectangle, so a linear one would
/// open a black box on the wall a few frames before anything filled it.
fn plate_strength(envelope_q8: i32) -> u8 {
    let eased = (envelope_q8 * envelope_q8) / 256;
    ((PLATE_STRENGTH * eased) / 256) as u8
}

/// Channel colour scaled by the fade envelope.
///
/// The extra halving is the tint multiplier's neutral point: the GPU
/// computes `texel * tint / 128`, so a tint of 128 leaves the atlas's
/// white glyph alone. Halving an 0..255 authored colour first means a
/// full-envelope number adds exactly the authored RGB to the frame,
/// instead of saturating the strongest channel and shifting the hue.
fn channel_tint(channel: u8, envelope_q8: i32) -> (u8, u8, u8) {
    let rgb = DamageNumberChannel::rgb_from_raw(channel);
    let scale = |component: u8| -> u8 { ((i32::from(component) * envelope_q8) / 512) as u8 };
    (scale(rgb.0), scale(rgb.1), scale(rgb.2))
}

/// Whether a number at `(x, y)` with `width` is worth submitting.
///
/// Projection can put an anchor far outside the frame (behind a wall,
/// off to the side); the GPU would clip it, but the glyph packets would
/// still be built and pushed. One compare per number is cheaper.
///
/// The vertical extent is the PLATE's, not the glyph cell's, because
/// the plate is the topmost and bottommost thing drawn. It was a bare
/// `+ 8` when the face was 8 pixels tall; leaving that behind a taller
/// face would have rejected numbers that still had rows on screen.
fn on_screen(x: i32, y: i32, width: i32) -> bool {
    let top = y + DIGIT_INK_TOP - PLATE_PAD_Y;
    let bottom = top + DIGIT_INK_HEIGHT + PLATE_PAD_Y * 2;
    x + width >= 0 && x < i32::from(SCREEN_W) && bottom >= 0 && top < i32::from(SCREEN_H)
}

/// Render `amount` as a signed decimal into `buffer`, returning the
/// populated slice as a `str`. No allocation and no formatting
/// machinery: this runs on every visible number every frame.
///
/// The sign is part of the string rather than something the draw path
/// prepends, so the width the caller centres on is the width of what is
/// actually drawn. Getting that wrong would drift every number off the
/// impact point by half a glyph.
fn format_amount(buffer: &mut [u8; DIGIT_BUFFER], amount: u16) -> &str {
    let mut value = amount;
    let mut index = DIGIT_BUFFER;
    loop {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
        // Stop at 1, not 0: the last slot belongs to the sign.
        if value == 0 || index == 1 {
            break;
        }
    }
    index -= 1;
    buffer[index] = b'-';
    core::str::from_utf8(&buffer[index..]).unwrap_or("-0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_rises_holds_and_falls_to_nothing() {
        // Both ends reach exactly zero -- that is what makes it a
        // dissolve rather than a pop.
        assert_eq!(envelope_q8(0), 0);
        assert_eq!(envelope_q8(NUMBER_LIFETIME_TICKS), 0);
        assert_eq!(envelope_q8(NUMBER_LIFETIME_TICKS + 1000), 0);
        assert_eq!(envelope_q8(FADE_IN_TICKS), 256);
        assert_eq!(envelope_q8(NUMBER_LIFETIME_TICKS - FADE_OUT_TICKS), 256);

        // Monotone up, then flat, then monotone down.
        for age in 1..FADE_IN_TICKS {
            assert!(envelope_q8(age) > envelope_q8(age - 1), "rise at {age}");
        }
        for age in (NUMBER_LIFETIME_TICKS - FADE_OUT_TICKS + 1)..NUMBER_LIFETIME_TICKS {
            assert!(envelope_q8(age) < envelope_q8(age - 1), "fall at {age}");
        }
        for age in 0..=NUMBER_LIFETIME_TICKS {
            let envelope = envelope_q8(age);
            assert!(
                (0..=256).contains(&envelope),
                "envelope {envelope} at {age}"
            );
        }
    }

    #[test]
    fn tint_reaches_the_authored_colour_and_zero() {
        // At full envelope the additive glyph contributes exactly the
        // authored RGB, because the tint's neutral point is 128.
        assert_eq!(
            channel_tint(0, 256),
            (HORIZON_RGB.0 / 2, HORIZON_RGB.1 / 2, HORIZON_RGB.2 / 2)
        );
        assert_eq!(
            channel_tint(1, 256),
            (ZENITH_RGB.0 / 2, ZENITH_RGB.1 / 2, ZENITH_RGB.2 / 2)
        );
        assert_eq!(channel_tint(0, 0), (0, 0, 0));
        assert_eq!(channel_tint(1, 0), (0, 0, 0));
        // The two channels never collide, or the feedback would be
        // telling the player nothing.
        assert_ne!(channel_tint(0, 256), channel_tint(1, 256));
    }

    /// The plate must never outrun the number it clears space for. At
    /// every envelope it stays a smaller fraction of its own peak than
    /// the number is of its peak, which is what squaring buys --
    /// otherwise a black box opens on the wall before any colour
    /// arrives to fill it.
    #[test]
    fn the_plate_never_leads_the_number() {
        assert_eq!(plate_strength(0), 0);
        assert_eq!(plate_strength(256), PLATE_STRENGTH as u8);
        for envelope in 0..=256 {
            let plate = i32::from(plate_strength(envelope));
            // Plate fraction of peak <= number fraction of peak.
            assert!(
                plate * 256 <= envelope * PLATE_STRENGTH,
                "plate leads the number at envelope {envelope}"
            );
            assert!(plate >= i32::from(plate_strength((envelope - 1).max(0))));
        }
        // And it must reach nothing, or it outlives the number it seats.
        assert_eq!(plate_strength(envelope_q8(NUMBER_LIFETIME_TICKS)), 0);
    }

    /// The plate only earns its place if it actually floors the
    /// surfaces this game puts behind a number. Pin it against the
    /// brightest thing measured in the shipping level at 25% light
    /// intensity, so a later strength cut has to argue with a number.
    #[test]
    fn the_plate_floors_the_levels_lit_surfaces() {
        // Sampled from a hardware dump of the tutorial room: the
        // enemy's lit chest, and the player's pale body, which is the
        // brightest surface a number lands on.
        const BRIGHTEST_SAMPLED_CHANNEL: i32 = 160;
        // The GPU computes `texel * tint / 128` from a white glyph.
        let subtracted = (255 * PLATE_STRENGTH) / 128;
        assert!(
            subtracted >= BRIGHTEST_SAMPLED_CHANNEL,
            "plate subtracts {subtracted}, leaving a cast on a {BRIGHTEST_SAMPLED_CHANNEL} surface"
        );
    }

    #[test]
    fn rise_is_monotone_and_covers_the_authored_distance() {
        assert_eq!(rise_pixels(0), 0);
        assert_eq!(rise_pixels(NUMBER_LIFETIME_TICKS), NUMBER_RISE_PIXELS);
        for age in 1..=NUMBER_LIFETIME_TICKS {
            assert!(rise_pixels(age) >= rise_pixels(age - 1), "rise at {age}");
        }
    }

    #[test]
    fn amounts_render_signed_and_without_leading_zeros() {
        for (amount, expected) in [
            (0u16, "-0"),
            (1, "-1"),
            (7, "-7"),
            (10, "-10"),
            (99, "-99"),
            (100, "-100"),
            (12345, "-12345"),
            // The buffer has to hold the sign as well as five digits.
            (u16::MAX, "-65535"),
        ] {
            let mut buffer = [0u8; DIGIT_BUFFER];
            assert_eq!(format_amount(&mut buffer, amount), expected);
        }
    }

    /// Rows of `ch` that carry ink in [`DAMAGE_NUMBER_FACE`], first and
    /// last. Glyph cells are full height and mostly empty, so the cell
    /// says nothing about where a character actually sits.
    fn inked_rows(ch: char) -> (i16, i16) {
        let glyph = DAMAGE_NUMBER_FACE
            .glyph_index(ch)
            .expect("the damage face must cover this character");
        let mut first = None;
        let mut last = 0u8;
        for row in 0..DAMAGE_NUMBER_FACE.glyph_h {
            if DAMAGE_NUMBER_FACE.glyph_row_packed(glyph, row) != 0 {
                first.get_or_insert(row);
                last = row;
            }
        }
        let first = first.expect("glyph must have ink");
        (i16::from(first), i16::from(last))
    }

    fn mid_inked_row(ch: char) -> i16 {
        let (first, last) = inked_rows(ch);
        (first + last) / 2
    }

    fn last_inked_row(ch: char) -> i16 {
        inked_rows(ch).1
    }

    /// The sign is drawn as its own run at [`MINUS_DROP`] rows below the
    /// digits, because these display faces put their hyphen at cap-top.
    /// Recompute the correct drop from the face's own bitmap so a face
    /// change cannot leave the sign floating above the number.
    #[test]
    fn the_sign_sits_at_the_digits_mid_height() {
        // Every digit shares a baseline and cap height, so '8' stands in.
        let wanted = mid_inked_row('8') - mid_inked_row(MINUS_CHAR);
        assert_eq!(
            MINUS_DROP, wanted,
            "MINUS_DROP should be {wanted} for this face, not {MINUS_DROP}"
        );
        // Dropping it must not push its INK out of the plate. The glyph
        // CELL is full height and mostly empty, so the cell overflowing
        // means nothing -- only the drawn rows do.
        let sign_ink_bottom = MINUS_DROP + last_inked_row(MINUS_CHAR);
        let plate_bottom = (DIGIT_INK_TOP + DIGIT_INK_HEIGHT + PLATE_PAD_Y) as i16;
        assert!(
            sign_ink_bottom < plate_bottom,
            "the dropped sign's ink reaches row {sign_ink_bottom}, past the plate at {plate_bottom}"
        );
    }

    /// The plate is sized to the digits' ink band, not to the glyph
    /// cell. Recompute the band from the face so a face change cannot
    /// leave the plate hanging below the number or clipping it.
    #[test]
    fn the_plate_is_sized_to_the_digits() {
        let mut top = i16::MAX;
        let mut bottom = i16::MIN;
        for digit in '0'..='9' {
            let (first, last) = inked_rows(digit);
            top = top.min(first);
            bottom = bottom.max(last);
        }
        assert_eq!(
            DIGIT_INK_TOP as i16, top,
            "DIGIT_INK_TOP should be {top} for this face"
        );
        assert_eq!(
            DIGIT_INK_HEIGHT as i16,
            bottom - top + 1,
            "DIGIT_INK_HEIGHT should be {} for this face",
            bottom - top + 1
        );
        // And the band must be a real saving over the cell, or the
        // constants are just restating `line_height` with extra steps.
        assert!(
            DIGIT_INK_HEIGHT < i32::from(DAMAGE_NUMBER_FACE.line_height),
            "the ink band should be shorter than the glyph cell"
        );
    }

    /// The sign is part of the measured string, so a number stays
    /// centred on the point it was dealt at rather than drifting by
    /// half a glyph. This is the property that breaks silently if the
    /// sign is ever prepended at draw time instead.
    #[test]
    fn the_measured_width_includes_the_sign() {
        let mut buffer = [0u8; DIGIT_BUFFER];
        let text = format_amount(&mut buffer, 27);
        let measured: u16 = text
            .chars()
            .map(|ch| u16::from(DAMAGE_NUMBER_FACE.glyph_advance(ch)))
            .sum();
        let digits_only: u16 = "27"
            .chars()
            .map(|ch| u16::from(DAMAGE_NUMBER_FACE.glyph_advance(ch)))
            .sum();
        assert_eq!(
            measured,
            digits_only + u16::from(DAMAGE_NUMBER_FACE.glyph_advance(MINUS_CHAR)),
            "the sign has to be inside the width the draw path centres on"
        );
    }

    /// The mask is what makes the system free when nothing is on
    /// screen, so it has to track the slots exactly: set on spawn,
    /// cleared once the number it stood for has expired.
    #[test]
    fn the_live_mask_tracks_the_slots_it_stands_for() {
        let mut numbers = DamageNumbers::default();
        assert_eq!(numbers.live_mask, 0, "a zeroed pool must read as idle");

        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            5,
            DamageNumberChannel::Horizon,
            SimTick::ZERO,
        );
        assert_eq!(numbers.live_mask, 1);
        // A zero-damage spawn is dropped and must not mark the pool busy.
        let before = numbers.live_mask;
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            0,
            DamageNumberChannel::Horizon,
            SimTick::ZERO,
        );
        assert_eq!(numbers.live_mask, before);

        // Every set bit corresponds to a slot the pool believes is live.
        for index in 0..MAX_DAMAGE_NUMBERS {
            let masked = numbers.live_mask & (1 << index) != 0;
            assert_eq!(masked, numbers.slots[index].live == 1, "slot {index}");
        }
    }

    /// The regression that cost the most time here: the overlay tick
    /// trails the simulation tick, so a number is briefly younger than
    /// zero. If that wraps, the number reads as ancient, gets retired
    /// on its very first draw, and -- once the idle mask trusts the
    /// retirement -- never appears at all.
    #[test]
    fn a_number_spawned_ahead_of_the_overlay_clock_is_not_expired() {
        let mut numbers = DamageNumbers::default();
        let spawn_tick = SimTick::from_u32(1_000);
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            42,
            DamageNumberChannel::Zenith,
            spawn_tick,
        );

        // The overlay is up to a few ticks behind the simulation.
        for behind in 1..=4u32 {
            let age = slot_age(&numbers.slots[0], 1_000 - behind);
            assert_eq!(age, 0, "overlay {behind} tick(s) behind must read as age 0");
            assert!(age < NUMBER_LIFETIME_TICKS, "must not read as expired");
            assert_eq!(envelope_q8(age), 0, "and must still be invisible at age 0");
        }

        // Once the clocks meet, it ages normally and expires on time.
        assert_eq!(slot_age(&numbers.slots[0], 1_000), 0);
        assert_eq!(slot_age(&numbers.slots[0], 1_010), 10);
        assert!(
            slot_age(&numbers.slots[0], 1_000 + NUMBER_LIFETIME_TICKS) >= NUMBER_LIFETIME_TICKS
        );
    }

    #[test]
    fn zero_damage_never_occupies_a_slot() {
        let mut numbers = DamageNumbers::default();
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            0,
            DamageNumberChannel::Horizon,
            SimTick::ZERO,
        );
        assert!(numbers.slots.iter().all(|slot| slot.live == 0));
    }

    /// A burst past capacity must keep the newest hits, not the stalest
    /// ones: the number the player is looking for is the one that just
    /// landed.
    #[test]
    fn overflow_recycles_the_oldest_and_keeps_the_newest() {
        let mut numbers = DamageNumbers::default();
        for index in 0..MAX_DAMAGE_NUMBERS {
            numbers.spawn(
                player_hit_anchor([0, 0, 0], 64),
                RoomIndex(0),
                index as u16 + 1,
                DamageNumberChannel::Horizon,
                SimTick::from_u32(index as u32),
            );
        }
        assert!(numbers.slots.iter().all(|slot| slot.live == 1));

        // Still inside every lifetime, so the pool is genuinely full.
        let now = SimTick::from_u32(MAX_DAMAGE_NUMBERS as u32);
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            999,
            DamageNumberChannel::Zenith,
            now,
        );

        let amounts: alloc::vec::Vec<u16> = numbers.slots.iter().map(|slot| slot.amount).collect();
        assert!(amounts.contains(&999), "newest hit was dropped");
        // Slot 0 held the oldest (spawn tick 0) and is the one reused.
        assert!(!amounts.contains(&1), "oldest hit was not recycled");
    }

    /// An expired slot is reused before any live one, so a steady drip
    /// of hits never evicts a number that is still on screen.
    #[test]
    fn expired_slots_are_reused_before_live_ones() {
        let mut numbers = DamageNumbers::default();
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            11,
            DamageNumberChannel::Horizon,
            SimTick::ZERO,
        );
        let later = SimTick::from_u32(NUMBER_LIFETIME_TICKS);
        numbers.spawn(
            player_hit_anchor([0, 0, 0], 64),
            RoomIndex(0),
            22,
            DamageNumberChannel::Horizon,
            later,
        );
        assert_eq!(
            numbers.slots[0].amount, 22,
            "expired slot 0 should be reused"
        );
        assert!(numbers.slots[1..].iter().all(|slot| slot.live == 0));
    }

    /// The pool is stored in link-time-zeroed scene BSS, so all-zero
    /// bytes have to decode as "no numbers".
    #[test]
    fn a_zeroed_pool_is_an_empty_pool() {
        let numbers = DamageNumbers::default();
        assert!(numbers.slots.iter().all(|slot| slot.live == 0));
        assert_eq!(numbers.live_mask, 0);
        // The pool is the slots plus the idle mask and its padding, and
        // nothing else. This is BSS on a machine with 2 MB of it, so a
        // silent growth here is worth failing a test over.
        assert!(
            core::mem::size_of::<DamageNumbers>()
                <= core::mem::size_of::<DamageNumberSlot>() * MAX_DAMAGE_NUMBERS + 4,
            "pool grew past slots + mask: {} bytes",
            core::mem::size_of::<DamageNumbers>()
        );
        // The mask has to have a bit for every slot.
        assert!(MAX_DAMAGE_NUMBERS <= u8::BITS as usize);
    }

    /// The two anchors exist because the first capture put a number on
    /// the player's own pale body and the additive glyphs saturated to
    /// white. Pin the ordering that fixed it: a number for damage taken
    /// clears the body, one for damage dealt stays below the head so it
    /// cannot collide with the enemy health bar drawn there.
    #[test]
    fn player_anchor_clears_the_body_and_struck_anchor_stays_below_the_head() {
        const HEIGHT: u16 = 64;
        let feet = [100, -40, 250];

        let struck = struck_actor_anchor(feet, HEIGHT);
        let taken = player_hit_anchor(feet, HEIGHT);

        // Both keep the world horizontal anchor exactly; only the
        // height moves, and any sidestep is screen-space.
        for anchor in [struck, taken] {
            assert_eq!(anchor.world[0], feet[0]);
            assert_eq!(anchor.world[2], feet[2]);
        }
        // Chest: above the feet, below the head.
        assert!(struck.world[1] > feet[1]);
        assert!(struck.world[1] < feet[1] + i32::from(HEIGHT));
        // Overhead: clear of the collision capsule, which is itself
        // shorter than the rendered body.
        assert!(taken.world[1] > feet[1] + i32::from(HEIGHT));
        assert!(taken.world[1] > struck.world[1]);

        // Only the struck-actor number steps aside, and it steps far
        // enough that a realistic value clears the 34px-wide enemy
        // health bar the HUD centres on the same actor.
        assert_eq!(taken.x_offset, 0);
        assert!(
            i32::from(struck.x_offset) - widest_realistic_half_width() > ENEMY_BAR_HALF_WIDTH,
            "a struck-actor number would overlap the enemy health bar"
        );
    }

    /// `marker_runtime`'s bar geometry, restated so this file's spacing
    /// assertion is checkable without reaching into that module.
    const ENEMY_BAR_HALF_WIDTH: i32 = 17;

    /// Half the width of the widest damage value the cooked level can
    /// actually produce, measured through [`DAMAGE_NUMBER_FACE`].
    ///
    /// Read off `WEAPONS` rather than assumed, and deliberately not off
    /// `u16::MAX`: the type allows 65535 but this level's hardest hit is
    /// two digits, and sizing the sidestep for a value that cannot occur
    /// would push every real number needlessly far off its target. If a
    /// weapon ever does cook a three-digit hit, this recomputes and the
    /// assertion below asks for a wider sidestep.
    fn widest_realistic_half_width() -> i32 {
        let hardest = WEAPONS
            .iter()
            .map(|weapon| weapon.damage)
            .max()
            .unwrap_or(99);
        // '8' is the widest digit in every fixed-cell face here, and the
        // proportional ones advance by glyph, so measure the real value.
        let mut digits = [0u8; DIGIT_BUFFER];
        let width: i32 = format_amount(&mut digits, hardest)
            .chars()
            .map(|ch| i32::from(DAMAGE_NUMBER_FACE.glyph_advance(ch)))
            .sum();
        width / 2
    }

    /// The sidestep is a hand-written constant but the thing it has to
    /// clear is a font metric, so changing the face must not silently
    /// walk numbers back under the bar.
    #[test]
    fn the_sidestep_clears_the_bar_for_the_chosen_face() {
        let clearance =
            i32::from(STRUCK_ACTOR_X_OFFSET) - widest_realistic_half_width() - ENEMY_BAR_HALF_WIDTH;
        assert!(
            clearance > 0,
            "a {}x{} face needs a bigger STRUCK_ACTOR_X_OFFSET: clearance {clearance}px",
            DAMAGE_NUMBER_FACE.glyph_w,
            DAMAGE_NUMBER_FACE.glyph_h,
        );
    }

    /// The whole point of the face swap: damage numbers are read at a
    /// glance while moving, so they must not be set in the HUD's compact
    /// italic. Pin the two properties that decision rests on.
    #[test]
    fn the_damage_face_is_upright_and_heavier_than_the_hud_face() {
        let hud = &psx_font::fonts::SPLEEN_5X8_ITALIC;
        assert!(
            DAMAGE_NUMBER_FACE.glyph_h > hud.glyph_h,
            "the damage face must be taller than the HUD's 5x8"
        );
        // Ink coverage over the digits stands in for stroke weight: the
        // HUD face is one pixel wide everywhere, the damage face two.
        let ink = |font: &psx_font::BitmapFont| -> u32 {
            ('0'..='9')
                .filter_map(|ch| font.glyph_index(ch))
                .flat_map(|glyph| {
                    (0..font.glyph_h).map(move |row| font.glyph_row_packed(glyph, row))
                })
                .map(u32::count_ones)
                .sum()
        };
        assert!(
            ink(DAMAGE_NUMBER_FACE) > ink(hud) * 2,
            "the damage face should carry far more ink per digit than the HUD's"
        );
    }

    #[test]
    fn offscreen_numbers_are_rejected() {
        assert!(on_screen(0, 0, 16));
        assert!(on_screen(SCREEN_W as i32 - 1, SCREEN_H as i32 - 1, 16));
        assert!(!on_screen(-20, 100, 16));
        assert!(!on_screen(SCREEN_W as i32, 100, 16));
        // Below the frame: the plate's top row is what has to clear it,
        // and the plate starts slightly above the text origin.
        let first_below = i32::from(SCREEN_H) + PLATE_PAD_Y - DIGIT_INK_TOP;
        assert!(!on_screen(100, first_below, 16));
        assert!(on_screen(100, first_below - 1, 16));
        assert!(!on_screen(100, -40, 16));
        // Partially on screen still draws.
        assert!(on_screen(-8, 100, 16));

        // The vertical reject tracks the plate, so a number whose plate
        // still has a row on screen is kept. The old fixed `+ 8` got
        // this wrong for any face taller than the 8x8 it was written
        // for, silently dropping numbers at the top edge.
        let plate_top_at = |y: i32| y + DIGIT_INK_TOP - PLATE_PAD_Y;
        let last_visible = -(DIGIT_INK_HEIGHT + PLATE_PAD_Y * 2) - DIGIT_INK_TOP + PLATE_PAD_Y;
        assert!(
            on_screen(100, last_visible, 16),
            "dropped a number still on screen"
        );
        assert!(
            !on_screen(100, last_visible - 1, 16),
            "kept a number fully above the frame"
        );
        assert!(
            plate_top_at(last_visible) < 0,
            "the check should be at the top edge"
        );
    }
}
