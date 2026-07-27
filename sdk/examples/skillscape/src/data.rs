//! Static game data: the real RuneScape XP curve, skills, resource
//! tiers, and items. Nothing here mutates -- all `const`/`static`, no
//! heap, so it costs nothing at runtime beyond ROM space.

/// XP required to *reach* level N+1 is `XP_TABLE[N]` (index 0 = the
/// threshold for level 2, since level 1 always starts at 0 XP).
/// Generated from the real OSRS formula --
/// `xp(level) = floor(1/4 * sum_{n=1}^{level-1} floor(n + 300*2^(n/7)))`
/// -- so early-game pacing (a level every few actions) through to the
/// level-99 grind (13M+ XP) both feel authentic.
pub const XP_TABLE: [u32; 98] = [
    83, 174, 276, 389, 513, 650, 802, 970, 1155, 1359, 1585, 1834, 2109, 2412, 2747, 3117, 3525,
    3975, 4472, 5021, 5626, 6294, 7031, 7845, 8742, 9733, 10827, 12034, 13366, 14836, 16459,
    18251, 20228, 22410, 24819, 27477, 30412, 33652, 37228, 41176, 45533, 50344, 55654, 61517,
    67988, 75132, 83019, 91726, 101339, 111950, 123666, 136599, 150878, 166642, 184046, 203260,
    224472, 247892, 273748, 302294, 333810, 368605, 407021, 449434, 496261, 547960, 605039,
    668057, 737634, 814452, 899264, 992902, 1096285, 1210428, 1336450, 1475588, 1629208, 1798815,
    1986076, 2192826, 2421095, 2673122, 2951381, 3258602, 3597800, 3972302, 4385785, 4842304,
    5346340, 5902840, 6517262, 7195638, 7944623, 8771568, 9684586, 10692638, 11805616, 13034440,
];

pub const MAX_LEVEL: u8 = 99;

/// Level for a given XP total, 1..=99.
pub fn level_for_xp(xp: u32) -> u8 {
    let mut level: u8 = 1;
    for &threshold in XP_TABLE.iter() {
        if xp >= threshold {
            level += 1;
        } else {
            break;
        }
    }
    level
}

/// XP threshold for `level` (0 if level <= 1, saturates at the
/// level-99 total for anything past it).
pub fn xp_for_level(level: u8) -> u32 {
    if level <= 1 {
        0
    } else {
        XP_TABLE[(level - 2).min(97) as usize]
    }
}

pub const NUM_SKILLS: usize = 6;
pub const NUM_TIERS: usize = 3;
pub const NUM_ITEMS: usize = 19;

// --- Items ------------------------------------------------------------
pub const ITEM_LOGS: usize = 0;
pub const ITEM_OAK_LOGS: usize = 1;
pub const ITEM_WILLOW_LOGS: usize = 2;
pub const ITEM_COPPER_ORE: usize = 3;
pub const ITEM_IRON_ORE: usize = 4;
pub const ITEM_COAL: usize = 5;
pub const ITEM_RAW_SHRIMP: usize = 6;
pub const ITEM_RAW_TROUT: usize = 7;
pub const ITEM_RAW_LOBSTER: usize = 8;
pub const ITEM_COOKED_SHRIMP: usize = 9;
pub const ITEM_COOKED_TROUT: usize = 10;
pub const ITEM_COOKED_LOBSTER: usize = 11;
pub const ITEM_BURNT_FISH: usize = 12;
pub const ITEM_BRONZE_BAR: usize = 13;
pub const ITEM_IRON_BAR: usize = 14;
pub const ITEM_STEEL_BAR: usize = 15;
pub const ITEM_WOODEN_BOW: usize = 16;
pub const ITEM_OAK_BOW: usize = 17;
pub const ITEM_WILLOW_BOW: usize = 18;

pub struct ItemDef {
    pub name: &'static str,
    pub sell_price: u32,
}

pub const ITEMS: [ItemDef; NUM_ITEMS] = [
    ItemDef { name: "LOGS", sell_price: 2 },
    ItemDef { name: "OAK LOGS", sell_price: 5 },
    ItemDef { name: "WILLOW LOGS", sell_price: 12 },
    ItemDef { name: "COPPER ORE", sell_price: 3 },
    ItemDef { name: "IRON ORE", sell_price: 8 },
    ItemDef { name: "COAL", sell_price: 15 },
    ItemDef { name: "RAW SHRIMP", sell_price: 2 },
    ItemDef { name: "RAW TROUT", sell_price: 10 },
    ItemDef { name: "RAW LOBSTER", sell_price: 20 },
    ItemDef { name: "COOKED SHRIMP", sell_price: 5 },
    ItemDef { name: "COOKED TROUT", sell_price: 20 },
    ItemDef { name: "COOKED LOBSTER", sell_price: 40 },
    ItemDef { name: "BURNT FISH", sell_price: 0 },
    ItemDef { name: "BRONZE BAR", sell_price: 8 },
    ItemDef { name: "IRON BAR", sell_price: 20 },
    ItemDef { name: "STEEL BAR", sell_price: 35 },
    ItemDef { name: "WOODEN BOW", sell_price: 10 },
    ItemDef { name: "OAK BOW", sell_price: 25 },
    ItemDef { name: "WILLOW BOW", sell_price: 50 },
];

// --- Tools --------------------------------------------------------------
// Only Woodcutting/Mining/Fishing (skill indices 0/1/2) wield a tool.
// Better tiers cut the action cooldown -- the whole point of the
// gather -> sell -> upgrade loop: `speed_pct` is the percentage of the
// base cooldown that survives, so smaller is faster.
pub const NUM_TOOL_TIERS: usize = 4;

pub struct ToolTier {
    pub name: &'static str,
    pub cost: u32,
    pub level_req: u8,
    pub speed_pct: u32,
}

pub const TOOL_TIERS: [ToolTier; NUM_TOOL_TIERS] = [
    ToolTier { name: "BRONZE", cost: 0, level_req: 1, speed_pct: 100 },
    ToolTier { name: "IRON", cost: 150, level_req: 1, speed_pct: 85 },
    ToolTier { name: "STEEL", cost: 600, level_req: 10, speed_pct: 70 },
    ToolTier { name: "MITHRIL", cost: 2000, level_req: 25, speed_pct: 55 },
];

/// Tool noun per tool-bearing skill (index matches `SKILLS`).
pub const TOOL_NOUNS: [&str; 3] = ["AXE", "PICKAXE", "ROD"];

// --- Skills / tiers -----------------------------------------------------
pub struct Tier {
    pub name: &'static str,
    pub level_req: u8,
    pub xp: u16,
    /// Item produced on success (or on failure too, if `burnt_item` is
    /// `None` -- only Cooking ever fails).
    pub item: usize,
    /// Cooking only: the raw fish this tier consumes from the
    /// inventory. `None` for gathering skills, which produce from thin
    /// air (you're not consuming the tree).
    pub consumes: Option<usize>,
    /// Cooking only: item produced instead of `item` on a burn.
    pub burnt_item: Option<usize>,
}

pub struct SkillDef {
    pub name: &'static str,
    pub verb: &'static str,
    pub tiers: [Tier; NUM_TIERS],
}

pub const SKILLS: [SkillDef; NUM_SKILLS] = [
    SkillDef {
        name: "WOODCUTTING",
        verb: "CHOPPING",
        tiers: [
            Tier { name: "NORMAL TREE", level_req: 1, xp: 25, item: ITEM_LOGS, consumes: None, burnt_item: None },
            Tier { name: "OAK TREE", level_req: 15, xp: 37, item: ITEM_OAK_LOGS, consumes: None, burnt_item: None },
            Tier { name: "WILLOW TREE", level_req: 30, xp: 67, item: ITEM_WILLOW_LOGS, consumes: None, burnt_item: None },
        ],
    },
    SkillDef {
        name: "MINING",
        verb: "MINING",
        tiers: [
            Tier { name: "COPPER ROCK", level_req: 1, xp: 17, item: ITEM_COPPER_ORE, consumes: None, burnt_item: None },
            Tier { name: "IRON ROCK", level_req: 15, xp: 35, item: ITEM_IRON_ORE, consumes: None, burnt_item: None },
            Tier { name: "COAL ROCK", level_req: 30, xp: 50, item: ITEM_COAL, consumes: None, burnt_item: None },
        ],
    },
    SkillDef {
        name: "FISHING",
        verb: "FISHING",
        tiers: [
            Tier { name: "SHRIMP (NET)", level_req: 1, xp: 10, item: ITEM_RAW_SHRIMP, consumes: None, burnt_item: None },
            Tier { name: "TROUT (ROD)", level_req: 20, xp: 50, item: ITEM_RAW_TROUT, consumes: None, burnt_item: None },
            Tier { name: "LOBSTER (CAGE)", level_req: 40, xp: 90, item: ITEM_RAW_LOBSTER, consumes: None, burnt_item: None },
        ],
    },
    SkillDef {
        name: "COOKING",
        verb: "COOKING",
        tiers: [
            Tier { name: "SHRIMP", level_req: 1, xp: 30, item: ITEM_COOKED_SHRIMP, consumes: Some(ITEM_RAW_SHRIMP), burnt_item: Some(ITEM_BURNT_FISH) },
            Tier { name: "TROUT", level_req: 20, xp: 70, item: ITEM_COOKED_TROUT, consumes: Some(ITEM_RAW_TROUT), burnt_item: Some(ITEM_BURNT_FISH) },
            Tier { name: "LOBSTER", level_req: 40, xp: 120, item: ITEM_COOKED_LOBSTER, consumes: Some(ITEM_RAW_LOBSTER), burnt_item: Some(ITEM_BURNT_FISH) },
        ],
    },
    SkillDef {
        name: "SMITHING",
        verb: "SMITHING",
        tiers: [
            Tier { name: "BRONZE BAR", level_req: 1, xp: 20, item: ITEM_BRONZE_BAR, consumes: Some(ITEM_COPPER_ORE), burnt_item: None },
            Tier { name: "IRON BAR", level_req: 15, xp: 45, item: ITEM_IRON_BAR, consumes: Some(ITEM_IRON_ORE), burnt_item: None },
            Tier { name: "STEEL BAR", level_req: 30, xp: 65, item: ITEM_STEEL_BAR, consumes: Some(ITEM_COAL), burnt_item: None },
        ],
    },
    SkillDef {
        name: "FLETCHING",
        verb: "FLETCHING",
        tiers: [
            Tier { name: "WOODEN BOW", level_req: 1, xp: 20, item: ITEM_WOODEN_BOW, consumes: Some(ITEM_LOGS), burnt_item: None },
            Tier { name: "OAK BOW", level_req: 15, xp: 45, item: ITEM_OAK_BOW, consumes: Some(ITEM_OAK_LOGS), burnt_item: None },
            Tier { name: "WILLOW BOW", level_req: 30, xp: 65, item: ITEM_WILLOW_BOW, consumes: Some(ITEM_WILLOW_LOGS), burnt_item: None },
        ],
    },
];
