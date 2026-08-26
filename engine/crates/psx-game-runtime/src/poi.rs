//! Allocation-free point-of-interest interaction state.
//!
//! Cooked level records own positions, page text, item definitions, and the
//! persistent-flag indices assigned to each point of interest. This module
//! owns the small amount of reusable gameplay policy that should not be
//! duplicated by individual games: nearest-available selection, independent
//! read/reward flags, and a paged message controller shared by world messages
//! and player-activated messages.

use crate::save::{SaveBlock, SAVE_FLAG_CAPACITY};

/// Sentinel used when a point of interest has no persistent flag for a state.
pub const POI_FLAG_NONE: u16 = u16::MAX;

/// Sentinel used when a point of interest grants no resource.
pub const POI_REWARD_NONE: u16 = u16::MAX;

/// Persistent flags assigned by the cooker to one point of interest.
///
/// Reading and collecting are deliberately separate. A repeatable message can
/// therefore remain available forever while its item reward is granted once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiPersistentFlags {
    /// Flag set after the message's final page is dismissed.
    pub read: u16,
    /// Flag set after the optional reward is granted.
    pub reward: u16,
}

impl PoiPersistentFlags {
    /// No persistent state. Primarily useful for non-POI legacy interactions.
    pub const NONE: Self = Self {
        read: POI_FLAG_NONE,
        reward: POI_FLAG_NONE,
    };

    /// Whether the message has been completed in this save.
    pub fn is_read(self, save: &SaveBlock) -> bool {
        persistent_flag(save, self.read)
    }

    /// Whether the optional reward has already been granted in this save.
    pub fn reward_claimed(self, save: &SaveBlock) -> bool {
        persistent_flag(save, self.reward)
    }

    /// Whether a real reward remains to be granted.
    pub fn reward_pending(self, save: &SaveBlock, reward: PoiReward) -> bool {
        reward.is_some() && self.reward != POI_FLAG_NONE && !self.reward_claimed(save)
    }

    /// Mark the message complete. Returns `true` only when a valid flag changed.
    pub fn mark_read(self, save: &mut SaveBlock) -> bool {
        set_persistent_flag(save, self.read)
    }

    /// Mark the reward granted. Returns `true` only when a valid flag changed.
    pub fn mark_reward_claimed(self, save: &mut SaveBlock) -> bool {
        set_persistent_flag(save, self.reward)
    }
}

impl Default for PoiPersistentFlags {
    fn default() -> Self {
        Self::NONE
    }
}

/// Optional inventory reward attached to a point of interest.
///
/// `resource` is a cooker-resolved compact resource id. The game maps it to
/// its inventory representation before calling
/// [`PoiPersistentFlags::mark_reward_claimed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiReward {
    /// Cooked resource id, or [`POI_REWARD_NONE`].
    pub resource: u16,
    /// Number of copies to grant.
    pub quantity: u8,
}

impl PoiReward {
    /// No reward.
    pub const NONE: Self = Self {
        resource: POI_REWARD_NONE,
        quantity: 0,
    };

    /// Whether this describes a non-empty reward.
    pub const fn is_some(self) -> bool {
        self.resource != POI_REWARD_NONE && self.quantity != 0
    }
}

impl Default for PoiReward {
    fn default() -> Self {
        Self::NONE
    }
}

/// Spatial and availability data needed for nearest-POI selection.
///
/// Rendering and text stay outside this record so callers can adapt their own
/// cooked level types without copying strings or allocating adapter tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoiCandidate {
    /// Owning room.
    pub room: u16,
    /// Room-local X coordinate.
    pub x: i32,
    /// Room-local Z coordinate.
    pub z: i32,
    /// Activation radius in XZ units.
    pub radius: u16,
    /// Whether the authored record is active.
    pub enabled: bool,
    /// Whether a completed message remains available.
    pub repeatable: bool,
    /// Cook-assigned read/reward flags.
    pub persistence: PoiPersistentFlags,
    /// Optional one-time reward. A failed grant keeps a completed one-shot
    /// POI available until the player has inventory room for it.
    pub reward: PoiReward,
}

impl PoiCandidate {
    /// Whether this point of interest can currently produce an interaction.
    pub fn is_available(self, save: &SaveBlock) -> bool {
        self.enabled
            && (self.repeatable
                || !self.persistence.is_read(save)
                || self.persistence.reward_pending(save, self.reward))
    }

    /// Whether its marker should use the dim, stationary depleted state.
    pub fn is_depleted(self, save: &SaveBlock) -> bool {
        self.enabled
            && !self.repeatable
            && self.persistence.is_read(save)
            && !self.persistence.reward_pending(save, self.reward)
    }
}

/// Return the nearest available point of interest whose XZ radius contains
/// `player`. Equal-distance ties retain the earlier cooked record.
pub fn nearest_available_poi(
    candidates: &[PoiCandidate],
    room: u16,
    player: [i32; 3],
    save: &SaveBlock,
) -> Option<usize> {
    let mut best = None;
    let mut best_distance = u32::MAX;
    for (index, candidate) in candidates.iter().copied().enumerate() {
        if candidate.room != room || !candidate.is_available(save) {
            continue;
        }
        let radius = u32::from(candidate.radius);
        let dx = player[0].abs_diff(candidate.x);
        let dz = player[2].abs_diff(candidate.z);
        if dx > radius || dz > radius {
            continue;
        }
        let distance = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        let radius_squared = radius.saturating_mul(radius);
        if distance <= radius_squared && distance < best_distance {
            best = Some(index);
            best_distance = distance;
        }
    }
    best
}

/// Contiguous slice of a caller-owned cooked message-page table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessagePageSpan {
    /// First page in the cooked table.
    pub first: u16,
    /// Number of pages.
    pub count: u16,
}

impl MessagePageSpan {
    /// Create a page-table slice.
    pub const fn new(first: u16, count: u16) -> Self {
        Self { first, count }
    }

    /// Whether this span contains no pages.
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    /// Whether every page index in this non-empty span fits in `u16`.
    pub const fn is_valid(self) -> bool {
        self.count != 0 && self.first <= u16::MAX - (self.count - 1)
    }
}

/// Source and presentation mode of the active message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageSource {
    /// A bottom-centred, two-line point-of-interest message.
    PointOfInterest(u16),
    /// The centred, three-line world message shown once per launch.
    World,
}

/// Result of advancing the active message with the interaction button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAdvance {
    /// No message was active.
    Inactive,
    /// Advanced to this absolute cooked page index.
    Advanced(u16),
    /// The final page closed; the caller can persist POI read state now.
    Closed(MessageSource),
}

/// Active paged-message state shared by POI and world-message presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveMessage {
    source: MessageSource,
    pages: MessagePageSpan,
    page_offset: u16,
}

impl ActiveMessage {
    /// Message source, which also selects bottom-centred or centred layout.
    pub const fn source(self) -> MessageSource {
        self.source
    }

    /// Absolute index of the page currently shown.
    pub const fn page(self) -> u16 {
        self.pages.first.saturating_add(self.page_offset)
    }

    /// Zero-based page number within this message.
    pub const fn page_offset(self) -> u16 {
        self.page_offset
    }

    /// Number of pages in this message.
    pub const fn page_count(self) -> u16 {
        self.pages.count
    }
}

/// Allocation-free controller for the one message overlay visible at a time.
///
/// It deliberately exposes only the interaction-button advance operation: an
/// unrelated cancel button cannot accidentally create an early-dismiss path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageController {
    active: Option<ActiveMessage>,
    /// Once-per-launch state for up to 256 playable scene identities.
    world_scenes_shown: [u32; 8],
}

impl MessageController {
    /// Empty controller at the start of a game launch.
    pub const fn new() -> Self {
        Self {
            active: None,
            world_scenes_shown: [0; 8],
        }
    }

    /// Currently visible message, if any.
    pub const fn active(self) -> Option<ActiveMessage> {
        self.active
    }

    /// Whether the per-scene world message has already opened this launch.
    pub const fn world_message_shown(self, scene: u8) -> bool {
        let index = scene as usize;
        self.world_scenes_shown[index / 32] & (1 << (index % 32)) != 0
    }

    /// Open a POI message. Refuses empty spans and never replaces an overlay.
    pub fn open_poi(&mut self, poi: u16, pages: MessagePageSpan) -> bool {
        self.open(MessageSource::PointOfInterest(poi), pages)
    }

    /// Open the world message once during this launch.
    ///
    /// Empty spans and attempts while another message is active do not consume
    /// the one-launch opportunity.
    pub fn open_world(&mut self, scene: u8, pages: MessagePageSpan) -> bool {
        if self.world_message_shown(scene) || self.active.is_some() || !pages.is_valid() {
            return false;
        }
        self.active = Some(ActiveMessage {
            source: MessageSource::World,
            pages,
            page_offset: 0,
        });
        let index = scene as usize;
        self.world_scenes_shown[index / 32] |= 1 << (index % 32);
        true
    }

    /// Advance one page, closing only when the final page is acknowledged.
    pub fn advance(&mut self) -> MessageAdvance {
        let Some(mut active) = self.active else {
            return MessageAdvance::Inactive;
        };
        if active.page_offset.saturating_add(1) < active.pages.count {
            active.page_offset += 1;
            self.active = Some(active);
            MessageAdvance::Advanced(active.page())
        } else {
            self.active = None;
            MessageAdvance::Closed(active.source)
        }
    }

    fn open(&mut self, source: MessageSource, pages: MessagePageSpan) -> bool {
        if self.active.is_some() || !pages.is_valid() {
            return false;
        }
        self.active = Some(ActiveMessage {
            source,
            pages,
            page_offset: 0,
        });
        true
    }
}

impl Default for MessageController {
    fn default() -> Self {
        Self::new()
    }
}

fn persistent_flag(save: &SaveBlock, flag: u16) -> bool {
    let index = usize::from(flag);
    flag != POI_FLAG_NONE && index < usize::from(save.flag_count) && save.flag(index)
}

fn set_persistent_flag(save: &mut SaveBlock, flag: u16) -> bool {
    let index = usize::from(flag);
    if flag == POI_FLAG_NONE
        || index >= usize::from(save.flag_count)
        || index >= SAVE_FLAG_CAPACITY
        || save.flag(index)
    {
        return false;
    }
    save.set_flag(index);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGS: PoiPersistentFlags = PoiPersistentFlags { read: 1, reward: 2 };

    fn candidate(x: i32, repeatable: bool, persistence: PoiPersistentFlags) -> PoiCandidate {
        PoiCandidate {
            room: 3,
            x,
            z: 0,
            radius: 100,
            enabled: true,
            repeatable,
            persistence,
            reward: PoiReward::NONE,
        }
    }

    #[test]
    fn read_and_reward_flags_are_independent_and_round_trip() {
        let mut save = SaveBlock::new(100, 4);
        assert!(!FLAGS.is_read(&save));
        assert!(!FLAGS.reward_claimed(&save));

        assert!(FLAGS.mark_reward_claimed(&mut save));
        assert!(!FLAGS.is_read(&save));
        assert!(FLAGS.reward_claimed(&save));
        assert!(!FLAGS.mark_reward_claimed(&mut save));

        assert!(FLAGS.mark_read(&mut save));
        let mut bytes = [0; crate::save::SAVE_BLOCK_BYTES];
        save.encode(&mut bytes);
        let decoded = SaveBlock::decode(&bytes, 4).unwrap();
        assert!(FLAGS.is_read(&decoded));
        assert!(FLAGS.reward_claimed(&decoded));
    }

    #[test]
    fn pending_reward_requires_both_a_resource_and_a_persistent_flag() {
        let mut save = SaveBlock::new(100, 4);
        let reward = PoiReward {
            resource: 72,
            quantity: 2,
        };
        assert!(FLAGS.reward_pending(&save, reward));
        assert!(!FLAGS.reward_pending(&save, PoiReward::NONE));
        assert!(!PoiPersistentFlags::NONE.reward_pending(&save, reward));
        assert!(FLAGS.mark_reward_claimed(&mut save));
        assert!(!FLAGS.reward_pending(&save, reward));
    }

    #[test]
    fn invalid_or_absent_flags_never_escape_the_cooked_table() {
        let mut save = SaveBlock::new(100, 2);
        let invalid = PoiPersistentFlags {
            read: 2,
            reward: POI_FLAG_NONE,
        };
        assert!(!invalid.mark_read(&mut save));
        assert!(!invalid.mark_reward_claimed(&mut save));
        assert!(!invalid.is_read(&save));
        assert!(!invalid.reward_claimed(&save));
        assert!((0..SAVE_FLAG_CAPACITY).all(|index| !save.flag(index)));
    }

    #[test]
    fn completed_nonrepeatable_poi_depletes_but_repeatable_stays_available() {
        let mut save = SaveBlock::new(100, 4);
        let one_shot = candidate(0, false, FLAGS);
        let repeatable = candidate(0, true, FLAGS);
        assert!(one_shot.is_available(&save));
        assert!(repeatable.is_available(&save));
        assert!(FLAGS.mark_read(&mut save));
        assert!(!one_shot.is_available(&save));
        assert!(one_shot.is_depleted(&save));
        assert!(repeatable.is_available(&save));
        assert!(!repeatable.is_depleted(&save));
    }

    #[test]
    fn completed_one_shot_with_unclaimed_reward_stays_available_for_retry() {
        let mut save = SaveBlock::new(100, 4);
        let mut one_shot = candidate(0, false, FLAGS);
        one_shot.reward = PoiReward {
            resource: 7,
            quantity: 1,
        };

        assert!(FLAGS.mark_read(&mut save));
        assert!(one_shot.is_available(&save));
        assert!(!one_shot.is_depleted(&save));

        assert!(FLAGS.mark_reward_claimed(&mut save));
        assert!(!one_shot.is_available(&save));
        assert!(one_shot.is_depleted(&save));
    }

    #[test]
    fn nearest_selection_ignores_wrong_room_range_disabled_and_depleted() {
        let mut save = SaveBlock::new(100, 8);
        let depleted_flags = PoiPersistentFlags { read: 3, reward: 4 };
        assert!(depleted_flags.mark_read(&mut save));
        let mut records = [
            candidate(60, false, FLAGS),
            candidate(20, false, depleted_flags),
            candidate(40, false, FLAGS),
            candidate(10, false, FLAGS),
            candidate(300, false, FLAGS),
        ];
        records[3].room = 4;
        records[4].radius = 50;
        assert_eq!(
            nearest_available_poi(&records, 3, [0, 99, 0], &save),
            Some(2)
        );

        records[2].enabled = false;
        assert_eq!(
            nearest_available_poi(&records, 3, [0, 0, 0], &save),
            Some(0)
        );

        records[0].x = i32::MIN;
        records[0].radius = u16::MAX;
        assert_eq!(
            nearest_available_poi(&records, 3, [i32::MAX, 0, 0], &save),
            None
        );
    }

    #[test]
    fn nearest_equal_distance_tie_keeps_cooked_order() {
        let save = SaveBlock::new(100, 1);
        let records = [
            candidate(-20, true, PoiPersistentFlags::NONE),
            candidate(20, true, PoiPersistentFlags::NONE),
        ];
        assert_eq!(
            nearest_available_poi(&records, 3, [0, 0, 0], &save),
            Some(0)
        );
    }

    #[test]
    fn poi_pages_advance_and_final_acknowledgement_reports_the_source() {
        let mut messages = MessageController::new();
        assert!(messages.open_poi(7, MessagePageSpan::new(12, 3)));
        let active = messages.active().unwrap();
        assert_eq!(active.source(), MessageSource::PointOfInterest(7));
        assert_eq!(active.page(), 12);
        assert_eq!(active.page_count(), 3);
        assert_eq!(messages.advance(), MessageAdvance::Advanced(13));
        assert_eq!(messages.advance(), MessageAdvance::Advanced(14));
        assert_eq!(
            messages.advance(),
            MessageAdvance::Closed(MessageSource::PointOfInterest(7))
        );
        assert_eq!(messages.active(), None);
        assert_eq!(messages.advance(), MessageAdvance::Inactive);
    }

    #[test]
    fn world_message_opens_once_per_controller_lifetime() {
        let mut messages = MessageController::new();
        assert!(!messages.open_world(3, MessagePageSpan::new(0, 0)));
        assert!(!messages.open_world(3, MessagePageSpan::new(u16::MAX, 2)));
        assert!(!messages.world_message_shown(3));
        assert!(messages.open_world(3, MessagePageSpan::new(20, 1)));
        assert!(messages.world_message_shown(3));
        assert_eq!(
            messages.advance(),
            MessageAdvance::Closed(MessageSource::World)
        );
        assert!(!messages.open_world(3, MessagePageSpan::new(20, 1)));
        assert!(messages.open_world(4, MessagePageSpan::new(30, 1)));
        assert_eq!(
            messages.advance(),
            MessageAdvance::Closed(MessageSource::World)
        );
        assert!(!messages.open_world(3, MessagePageSpan::new(20, 1)));

        let mut next_launch = MessageController::new();
        assert!(next_launch.open_world(3, MessagePageSpan::new(20, 1)));
    }

    #[test]
    fn an_active_message_is_never_replaced() {
        let mut messages = MessageController::new();
        assert!(messages.open_poi(1, MessagePageSpan::new(2, 2)));
        assert!(!messages.open_poi(2, MessagePageSpan::new(9, 1)));
        assert!(!messages.open_world(1, MessagePageSpan::new(30, 1)));
        assert!(!messages.world_message_shown(1));
        assert_eq!(messages.active().unwrap().page(), 2);
    }
}
