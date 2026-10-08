//! Resource keys, categories and request priority classes.

/// What kind of data a resource is. Each category has its own budget
/// ([`crate::CategoryConfig`]) and maps onto one page pool.
///
/// The five constants are the categories the streaming design names; a game
/// may use any other value below [`Category::LIMIT`] for its own kinds.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Category(u8);

impl Category {
    /// Exclusive upper bound on category numbers (a key packs the category in
    /// 6 bits).
    pub const LIMIT: u8 = 64;
    /// Visible world data of one region (a BSP subtree and its payload).
    pub const WORLD_REGION: Category = Category(0);
    /// Collision data of one region (hull subtrees).
    pub const COLLISION_REGION: Category = Category(1);
    /// One VRAM texture page or palette block.
    pub const TEXTURE_PAGE: Category = Category(2);
    /// Model, animation clips and atlas of one actor archetype.
    pub const ARCHETYPE_PACK: Category = Category(3);
    /// One SPU sound bank.
    pub const AUDIO_BANK: Category = Category(4);

    /// A game-defined category. `None` when `number` is not below
    /// [`Category::LIMIT`].
    pub const fn from_number(number: u8) -> Option<Category> {
        if number < Self::LIMIT {
            Some(Category(number))
        } else {
            None
        }
    }

    /// The category number, usable as an array index.
    pub const fn number(self) -> u8 {
        self.0
    }
}

/// Identity of one streamed resource: a [`Category`] and the engine's dense
/// index inside it (region number, texture page number, archetype number).
///
/// Packed into one `u32` so a lookup is one integer compare.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceKey(u32);

impl ResourceKey {
    /// Largest index a key can carry (24 bits).
    pub const MAX_INDEX: u32 = (1 << 24) - 1;

    /// A key for `index` inside `category`. `None` when `index` exceeds
    /// [`ResourceKey::MAX_INDEX`].
    pub const fn new(category: Category, index: u32) -> Option<ResourceKey> {
        if index > Self::MAX_INDEX {
            None
        } else {
            Some(ResourceKey(((category.0 as u32) << 24) | index))
        }
    }

    /// The key's category.
    pub const fn category(self) -> Category {
        Category((self.0 >> 24) as u8)
    }

    /// The key's index inside its category.
    pub const fn index(self) -> u32 {
        self.0 & Self::MAX_INDEX
    }

    /// The packed representation, stable across builds.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Rebuild a key from [`ResourceKey::as_u32`]. `None` when the category
    /// bits are out of range.
    pub const fn from_u32(packed: u32) -> Option<ResourceKey> {
        if (packed >> 24) < Category::LIMIT as u32 {
            Some(ResourceKey(packed))
        } else {
            None
        }
    }
}

/// Request priority, most urgent first. The order is the order requests are
/// issued to the transport and the order eviction defends them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Class {
    /// Needed by the current frame (invariant: always resident). Never
    /// deferred by the CPU budget and never evicted while wanted.
    Demand = 0,
    /// Fill for the combat scope: must complete before combat music starts.
    CombatFill = 1,
    /// Lead ring: needed soon, requested nearest first.
    Lead = 2,
    /// Opportunistic fill of whatever room is left.
    Background = 3,
    /// Audio banks and ring refills.
    Audio = 4,
}

impl Class {
    /// Number of classes.
    pub const COUNT: usize = 5;

    /// The class as an array index.
    pub const fn as_index(self) -> usize {
        self as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trips_and_rejects_out_of_range() {
        let key = ResourceKey::new(Category::ARCHETYPE_PACK, 0xABCDE).unwrap();
        assert_eq!(key.category(), Category::ARCHETYPE_PACK);
        assert_eq!(key.index(), 0xABCDE);
        assert_eq!(ResourceKey::from_u32(key.as_u32()), Some(key));
        assert!(ResourceKey::new(Category::WORLD_REGION, ResourceKey::MAX_INDEX + 1).is_none());
        assert!(ResourceKey::from_u32(0xFF00_0000).is_none());
        assert!(Category::from_number(Category::LIMIT).is_none());
    }

    #[test]
    fn classes_order_demand_first() {
        assert!(Class::Demand < Class::CombatFill);
        assert!(Class::Lead < Class::Background);
        assert_eq!(Class::Audio.as_index(), Class::COUNT - 1);
    }
}
