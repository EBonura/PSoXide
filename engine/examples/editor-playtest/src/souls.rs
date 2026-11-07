//! Souls: the run's currency.
//!
//! One running total, added to when the player lands a killing blow. The
//! per-enemy award is authored on the cooked
//! [`psx_level::LevelGameEntityRecord::soul_value`] field, so it is tuned in
//! the editor's enemy inspector alongside health and touch damage rather than
//! being a constant here.
//!
//! The wallet also remembers the most recent gain and the gameplay tick its
//! display window closes on. That is what a "+40" popup reads, and it costs
//! nothing per frame: there is no timer to advance, because presentation
//! compares the stored deadline against the tick it is already drawing with.
//!
//! Deliberately NOT here, pending a design call (see the task report):
//! dropping the total on death and making it recoverable, persisting it past
//! the session, and anything that spends it. All-zero is the empty wallet, so
//! it needs no boot stamp in the zeroed scene storage.

/// Gameplay ticks (60 Hz) a fresh gain stays legible before its popup closes.
pub(crate) const SOULS_GAIN_DISPLAY_TICKS: u32 = 90;

/// The player's soul total for this run, plus the most recent gain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SoulsWallet {
    total: u32,
    recent_gain: u32,
    /// Gameplay tick the recent-gain window closes on. Zero means no gain has
    /// ever landed, which is why `showing_recent_gain` checks it separately:
    /// a wallet that has never been paid must not read as "showing +0".
    recent_gain_until: u32,
}

impl SoulsWallet {
    /// A wallet holding nothing. Byte-identical to the zeroed scene storage.
    pub(crate) const EMPTY: Self = Self {
        total: 0,
        recent_gain: 0,
        recent_gain_until: 0,
    };

    /// Credit one kill worth `value` souls at gameplay tick `now`.
    ///
    /// A zero award is not a gain: an enemy authored at zero souls must not
    /// pop "+0" over the HUD. Awards that land while an earlier window is
    /// still open ACCUMULATE into it rather than replacing it, so a swing
    /// that kills two enemies on the same tick reads "+80" once instead of
    /// flickering "+40" twice.
    pub(crate) fn award(&mut self, value: u16, now: u32) {
        if value == 0 {
            return;
        }
        let value = u32::from(value);
        self.total = self.total.saturating_add(value);
        self.recent_gain = if self.showing_recent_gain(now) {
            self.recent_gain.saturating_add(value)
        } else {
            value
        };
        self.recent_gain_until = now.saturating_add(SOULS_GAIN_DISPLAY_TICKS);
    }

    /// Souls held.
    pub(crate) fn total(&self) -> u32 {
        self.total
    }

    /// Souls credited by the gain the popup is currently showing.
    pub(crate) fn recent_gain(&self) -> u32 {
        self.recent_gain
    }

    /// Whether the recent-gain popup is open at gameplay tick `now`.
    pub(crate) fn showing_recent_gain(&self, now: u32) -> bool {
        self.recent_gain_until != 0 && now < self.recent_gain_until
    }

    /// Empty the wallet. Not wired to anything yet: it exists so a
    /// drop-on-death rule has one place to call when that is decided.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn clear(&mut self) {
        *self = Self::EMPTY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroed_storage_is_an_empty_wallet() {
        // The scene lives in link-time-zero BSS and is never stamped, so
        // all-zero has to BE the empty wallet, not merely decode as one.
        assert_eq!(SoulsWallet::default(), SoulsWallet::EMPTY);
        assert_eq!(SoulsWallet::EMPTY.total(), 0);
        assert!(!SoulsWallet::EMPTY.showing_recent_gain(0));
    }

    #[test]
    fn kills_accumulate_into_the_total() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(40, 100);
        assert_eq!(wallet.total(), 40);
        wallet.award(25, 400);
        assert_eq!(wallet.total(), 65);
        wallet.award(1, 900);
        assert_eq!(wallet.total(), 66);
    }

    #[test]
    fn a_zero_award_is_not_a_gain() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(0, 100);
        assert_eq!(wallet.total(), 0);
        assert!(!wallet.showing_recent_gain(100));
    }

    #[test]
    fn the_gain_popup_opens_and_closes_on_its_own_deadline() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(40, 100);
        assert_eq!(wallet.recent_gain(), 40);
        assert!(wallet.showing_recent_gain(100));
        assert!(wallet.showing_recent_gain(100 + SOULS_GAIN_DISPLAY_TICKS - 1));
        assert!(!wallet.showing_recent_gain(100 + SOULS_GAIN_DISPLAY_TICKS));
        // The total survives the popup closing; only the popup expires.
        assert_eq!(wallet.total(), 40);
    }

    #[test]
    fn awards_inside_an_open_window_merge_into_one_popup() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(40, 100);
        wallet.award(40, 100);
        assert_eq!(wallet.recent_gain(), 80);
        assert_eq!(wallet.total(), 80);
        // ... and the window is extended from the LATEST award.
        wallet.award(10, 150);
        assert_eq!(wallet.recent_gain(), 90);
        assert!(wallet.showing_recent_gain(150 + SOULS_GAIN_DISPLAY_TICKS - 1));
        assert!(!wallet.showing_recent_gain(150 + SOULS_GAIN_DISPLAY_TICKS));
    }

    #[test]
    fn a_gain_after_the_window_closed_starts_a_fresh_popup() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(40, 100);
        wallet.award(25, 100 + SOULS_GAIN_DISPLAY_TICKS);
        assert_eq!(wallet.recent_gain(), 25);
        assert_eq!(wallet.total(), 65);
    }

    #[test]
    fn a_gain_at_tick_zero_still_shows() {
        // `recent_gain_until == 0` is the "never paid" sentinel, so an award
        // on the very first gameplay tick must not be mistaken for it.
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(7, 0);
        assert!(wallet.showing_recent_gain(0));
        assert_eq!(wallet.recent_gain(), 7);
    }

    #[test]
    fn the_total_saturates_instead_of_wrapping() {
        let mut wallet = SoulsWallet::EMPTY;
        for tick in 0..8 {
            // u16::MAX per kill can only reach u32::MAX after ~65k kills, so
            // drive the field directly rather than pretending otherwise.
            wallet.total = u32::MAX - 4;
            wallet.award(u16::MAX, tick * 1000);
            assert_eq!(wallet.total(), u32::MAX);
        }
    }

    #[test]
    fn clearing_returns_it_to_empty() {
        let mut wallet = SoulsWallet::EMPTY;
        wallet.award(40, 100);
        wallet.clear();
        assert_eq!(wallet, SoulsWallet::EMPTY);
        assert!(!wallet.showing_recent_gain(100));
    }
}
