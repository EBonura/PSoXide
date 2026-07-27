//! `skillscape` -- "SkillScape": a menu-heavy OSRS-inspired idle
//! skilling game. Six gathering/production skills (Woodcutting, Mining,
//! Fishing feed Cooking/Smithing/Fletching), a real level-1..99 XP
//! curve lifted from the actual RuneScape formula, a stackable-by-type
//! inventory, a shop, upgradeable tools that cut your gather time, a
//! tiered goal checklist paid out in a second currency (Tokens) spent
//! at its own shop, and memory-card save/load across both card slots
//! -- plus the genuine active-vs-passive choice per skill: stay and
//! press the button in time for the fast rate, or walk away and let it
//! run slower on its own.
//!
//! Controls:
//! - Title: START to begin (continues a memory-card save if one loaded)
//! - Hub: D-pad UP/DOWN select a skill, CROSS opens it, L1 inventory,
//!   R1 shop, L2 goals, R2 equipment, SELECT token shop, START saves
//! - Skill screen: D-pad LEFT/RIGHT picks the resource tier, TRIANGLE
//!   toggles Active/Passive, CROSS performs the action (Active mode,
//!   once the cooldown bar is full), CIRCLE returns to the hub
//! - Inventory / Shop / Goals / Token Shop: D-pad UP/DOWN move the
//!   cursor, CROSS sells a stack (Shop), buys the next tool tier
//!   (Equipment), or buys a perk (Token Shop), CIRCLE returns to the
//!   hub

#![no_std]
#![no_main]

extern crate psx_rt;

mod data;

use data::*;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_mc::{Card, Error as McError, HardwareCard, Icon, Slot};
use psx_pad::{button, poll_port1, PadTracker};
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

// --- OSRS-ish palette ---------------------------------------------------
const BG: (u8, u8, u8) = (10, 8, 6);
const PANEL_BORDER: (u8, u8, u8) = (128, 100, 56);
const PANEL_FILL: (u8, u8, u8) = (40, 31, 20);
const GOLD: (u8, u8, u8) = (255, 213, 64);
const WHITE: (u8, u8, u8) = (222, 218, 205);
const GRAY: (u8, u8, u8) = (118, 108, 96);
const RED: (u8, u8, u8) = (216, 80, 68);
const GREEN: (u8, u8, u8) = (110, 210, 110);
const BLUE: (u8, u8, u8) = (100, 160, 230);
const PURPLE: (u8, u8, u8) = (190, 140, 230);
const BAR_BG: (u8, u8, u8) = (26, 20, 13);
const BAR_XP: (u8, u8, u8) = (216, 166, 40);
const BAR_ACTION: (u8, u8, u8) = (100, 195, 100);

const LEVEL_UP_FRAMES: u16 = 110;
const GOAL_FLASH_FRAMES: u16 = 130;
const SAVE_FLASH_FRAMES: u16 = 90;

const SAVE_NAME: &str = "BASLUS-00001SKLSCP";
const SAVE_DESC: &str = "SKILLSCAPE SAVE";

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Title,
    Hub,
    Action,
    Inventory,
    Shop,
    Goals,
    Equipment,
    TokenShop,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Active,
    Passive,
}

#[derive(Clone, Copy)]
enum LastGain {
    None,
    Gained { xp: u16, item: usize, burnt: bool },
    NoRawFish { item: usize },
}

// --- Goals ----------------------------------------------------------------
//
// Every category has 3 tiers, so completing one unlocks the next as a
// visible, still-climbing target -- the point isn't a single checklist
// you clear once, it's a ladder that keeps going.

#[derive(Clone, Copy)]
enum GoalMetric {
    SkillLevel(usize),
    TotalGathered,
    TotalCooked,
    TotalGoldEarned,
    AllToolsAtTier(usize),
    AnySkillLevel,
    GoalsCompleted,
}

struct GoalDef {
    desc: &'static str,
    metric: GoalMetric,
    target: u32,
    gold_reward: u32,
    token_reward: u32,
}

const NUM_GOALS: usize = 36;

// Three tiers per category (50g/5tok -> 200g/15tok -> 750g/40tok), laid
// out flat and literally -- no macro or const-fn cleverness, just data,
// so there's nothing subtle to get wrong compiling it for a bare-metal
// target.
const GOALS: [GoalDef; NUM_GOALS] = [
    GoalDef { desc: "WOODCUTTING LV 10", metric: GoalMetric::SkillLevel(0), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "WOODCUTTING LV 30", metric: GoalMetric::SkillLevel(0), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "WOODCUTTING LV 60", metric: GoalMetric::SkillLevel(0), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "MINING LV 10", metric: GoalMetric::SkillLevel(1), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "MINING LV 30", metric: GoalMetric::SkillLevel(1), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "MINING LV 60", metric: GoalMetric::SkillLevel(1), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "FISHING LV 10", metric: GoalMetric::SkillLevel(2), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "FISHING LV 30", metric: GoalMetric::SkillLevel(2), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "FISHING LV 60", metric: GoalMetric::SkillLevel(2), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "COOKING LV 10", metric: GoalMetric::SkillLevel(3), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "COOKING LV 30", metric: GoalMetric::SkillLevel(3), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "COOKING LV 60", metric: GoalMetric::SkillLevel(3), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "SMITHING LV 10", metric: GoalMetric::SkillLevel(4), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "SMITHING LV 30", metric: GoalMetric::SkillLevel(4), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "SMITHING LV 60", metric: GoalMetric::SkillLevel(4), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "FLETCHING LV 10", metric: GoalMetric::SkillLevel(5), target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "FLETCHING LV 30", metric: GoalMetric::SkillLevel(5), target: 30, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "FLETCHING LV 60", metric: GoalMetric::SkillLevel(5), target: 60, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "GATHER 100 RESOURCES", metric: GoalMetric::TotalGathered, target: 100, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "GATHER 500 RESOURCES", metric: GoalMetric::TotalGathered, target: 500, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "GATHER 2000 RESOURCES", metric: GoalMetric::TotalGathered, target: 2000, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "COOK 25 MEALS", metric: GoalMetric::TotalCooked, target: 25, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "COOK 100 MEALS", metric: GoalMetric::TotalCooked, target: 100, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "COOK 300 MEALS", metric: GoalMetric::TotalCooked, target: 300, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "EARN 1000 GOLD", metric: GoalMetric::TotalGoldEarned, target: 1000, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "EARN 5000 GOLD", metric: GoalMetric::TotalGoldEarned, target: 5000, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "EARN 20000 GOLD", metric: GoalMetric::TotalGoldEarned, target: 20000, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "IRON TOOLS (ALL 3)", metric: GoalMetric::AllToolsAtTier(1), target: 1, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "STEEL TOOLS (ALL 3)", metric: GoalMetric::AllToolsAtTier(2), target: 1, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "MITHRIL TOOLS (ALL 3)", metric: GoalMetric::AllToolsAtTier(3), target: 1, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "REACH LV 30 (ANY)", metric: GoalMetric::AnySkillLevel, target: 30, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "REACH LV 50 (ANY)", metric: GoalMetric::AnySkillLevel, target: 50, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "REACH LV 80 (ANY)", metric: GoalMetric::AnySkillLevel, target: 80, gold_reward: 750, token_reward: 40 },
    GoalDef { desc: "COMPLETE 10 GOALS", metric: GoalMetric::GoalsCompleted, target: 10, gold_reward: 50, token_reward: 5 },
    GoalDef { desc: "COMPLETE 20 GOALS", metric: GoalMetric::GoalsCompleted, target: 20, gold_reward: 200, token_reward: 15 },
    GoalDef { desc: "COMPLETE 30 GOALS", metric: GoalMetric::GoalsCompleted, target: 30, gold_reward: 750, token_reward: 40 },
];

fn goal_progress(game: &Game, metric: GoalMetric) -> u32 {
    match metric {
        GoalMetric::SkillLevel(i) => level_for_xp(game.xp[i]) as u32,
        GoalMetric::TotalGathered => {
            (0..=ITEM_RAW_LOBSTER).map(|i| game.lifetime_gathered[i]).sum()
        }
        GoalMetric::TotalCooked => {
            game.lifetime_gathered[ITEM_COOKED_SHRIMP]
                + game.lifetime_gathered[ITEM_COOKED_TROUT]
                + game.lifetime_gathered[ITEM_COOKED_LOBSTER]
        }
        GoalMetric::TotalGoldEarned => game.lifetime_gold,
        GoalMetric::AllToolsAtTier(tier) => {
            if game.tool_tier[0] >= tier && game.tool_tier[1] >= tier && game.tool_tier[2] >= tier {
                1
            } else {
                0
            }
        }
        GoalMetric::AnySkillLevel => {
            (0..NUM_SKILLS).map(|i| level_for_xp(game.xp[i]) as u32).max().unwrap_or(0)
        }
        GoalMetric::GoalsCompleted => game.goals_claimed.count_ones(),
    }
}

fn check_goals(game: &mut Game) {
    for i in 0..NUM_GOALS {
        if game.goals_claimed & (1 << i) != 0 {
            continue;
        }
        if goal_progress(game, GOALS[i].metric) >= GOALS[i].target {
            game.goals_claimed |= 1 << i;
            game.gold = game.gold.saturating_add(GOALS[i].gold_reward);
            game.lifetime_gold = game.lifetime_gold.saturating_add(GOALS[i].gold_reward);
            game.tokens = game.tokens.saturating_add(GOALS[i].token_reward);
            game.goal_flash = GOAL_FLASH_FRAMES;
            game.goal_flash_idx = i;
        }
    }
}

// --- Token shop -------------------------------------------------------

struct TokenItem {
    name: &'static str,
    desc: &'static str,
    cost: u32,
}

const TOKEN_XP_TONIC: usize = 0;
const TOKEN_LUCKY_CRATE: usize = 1;
const TOKEN_MASTER_BADGE: usize = 2;
const NUM_TOKEN_ITEMS: usize = 3;

const TOKEN_ITEMS: [TokenItem; NUM_TOKEN_ITEMS] = [
    TokenItem { name: "XP TONIC", desc: "+200 XP, SKILL YOU LAST HAD OPEN", cost: 10 },
    TokenItem { name: "LUCKY CRATE", desc: "RANDOM BUNDLE OF RAW RESOURCES", cost: 15 },
    TokenItem { name: "MASTER BADGE", desc: "PERMANENT TITLE, COSMETIC ONLY", cost: 100 },
];

fn apply_token_item(game: &mut Game, item: usize) {
    match item {
        TOKEN_XP_TONIC => {
            let skill = game.action_skill;
            let old_level = level_for_xp(game.xp[skill]);
            game.xp[skill] = game.xp[skill].saturating_add(200);
            let new_level = level_for_xp(game.xp[skill]);
            if new_level > old_level {
                game.level_up = LEVEL_UP_FRAMES;
                game.level_up_skill = skill;
                game.level_up_level = new_level;
            }
        }
        TOKEN_LUCKY_CRATE => {
            for _ in 0..3 {
                let item = (xorshift32(&mut game.rng) % 9) as usize; // raw gatherables only
                let qty = 1 + xorshift32(&mut game.rng) % 5;
                game.inventory[item] += qty;
                game.lifetime_gathered[item] = game.lifetime_gathered[item].saturating_add(qty);
            }
        }
        TOKEN_MASTER_BADGE => {
            game.has_badge = true;
        }
        _ => {}
    }
}

// --- Save data --------------------------------------------------------

const SAVE_SIZE: usize = 200;

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn get_u64(buf: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(b)
}

struct SaveData {
    xp: [u32; NUM_SKILLS],
    inventory: [u32; NUM_ITEMS],
    gold: u32,
    tool_tier: [usize; 3],
    lifetime_gathered: [u32; NUM_ITEMS],
    lifetime_gold: u32,
    goals_claimed: u64,
    tokens: u32,
    has_badge: bool,
}

fn serialize(game: &Game) -> [u8; SAVE_SIZE] {
    let mut buf = [0u8; SAVE_SIZE];
    let mut off = 0;
    for i in 0..NUM_SKILLS {
        put_u32(&mut buf, off, game.xp[i]);
        off += 4;
    }
    for i in 0..NUM_ITEMS {
        put_u32(&mut buf, off, game.inventory[i]);
        off += 4;
    }
    put_u32(&mut buf, off, game.gold);
    off += 4;
    for i in 0..3 {
        buf[off] = game.tool_tier[i] as u8;
        off += 1;
    }
    for i in 0..NUM_ITEMS {
        put_u32(&mut buf, off, game.lifetime_gathered[i]);
        off += 4;
    }
    put_u32(&mut buf, off, game.lifetime_gold);
    off += 4;
    put_u64(&mut buf, off, game.goals_claimed);
    off += 8;
    put_u32(&mut buf, off, game.tokens);
    off += 4;
    buf[off] = game.has_badge as u8;
    buf
}

fn deserialize(buf: &[u8]) -> Option<SaveData> {
    if buf.len() < SAVE_SIZE {
        return None;
    }
    let mut off = 0;
    let mut xp = [0u32; NUM_SKILLS];
    for slot in xp.iter_mut() {
        *slot = get_u32(buf, off);
        off += 4;
    }
    let mut inventory = [0u32; NUM_ITEMS];
    for slot in inventory.iter_mut() {
        *slot = get_u32(buf, off);
        off += 4;
    }
    let gold = get_u32(buf, off);
    off += 4;
    let mut tool_tier = [0usize; 3];
    for slot in tool_tier.iter_mut() {
        *slot = (buf[off] as usize).min(NUM_TOOL_TIERS - 1);
        off += 1;
    }
    let mut lifetime_gathered = [0u32; NUM_ITEMS];
    for slot in lifetime_gathered.iter_mut() {
        *slot = get_u32(buf, off);
        off += 4;
    }
    let lifetime_gold = get_u32(buf, off);
    off += 4;
    let goals_claimed = get_u64(buf, off);
    off += 8;
    let tokens = get_u32(buf, off);
    off += 4;
    let has_badge = buf[off] != 0;
    Some(SaveData {
        xp,
        inventory,
        gold,
        tool_tier,
        lifetime_gathered,
        lifetime_gold,
        goals_claimed,
        tokens,
        has_badge,
    })
}

#[derive(Clone, Copy)]
enum SaveStatus {
    Saved(Slot),
    Full,
    NoCard,
}

/// Slot order to try, preferring wherever we last successfully read or
/// wrote so a session stays on one physical card once it picks one.
fn slot_order(preferred: Option<Slot>) -> [Slot; 2] {
    match preferred {
        Some(Slot::Two) => [Slot::Two, Slot::One],
        _ => [Slot::One, Slot::Two],
    }
}

/// `AckMode::NoAck` (2026-07-26) -- the two suspects raised by the
/// corrupted-saves report both came back clean on console: a read-only
/// address-echo sweep hit 32/32 on both slots (ruling out "wrote to the
/// wrong frame"), and a direct 128-byte write+read-back round trip (via
/// `hello-memcard-min`'s `run_data_test`, bypassing the directory entirely)
/// came back byte-perfect on slot 1 four times running -- the burst-data
/// path a save exercises that no earlier test had actually covered. Slot 2
/// failed that same round trip twice (immediate `NoCard` on the read right
/// after a write, despite passing the address sweep) -- reads as needing
/// more deselect margin on that slot specifically, not the same class of
/// bug, and `slot_order` already tries slot 1 first.
///
/// That fixed `write_frame` returning `Ok` and reading back correctly, but
/// the save still never appeared in the BIOS card manager. Newly added
/// `write_gap_spins` targets the next suspect: PSn00bSDK's reference card
/// driver (`PSn00bSDK-master/indev/psxpad/card.s`) documents "you must wait
/// at least two vsyncs between each sector write" (~33ms) -- far longer
/// than `deselect_spins` was ever sized for, and a real `Card::write`
/// issues several sector writes back-to-back (directory entry, title,
/// icon, each data block). `4_000_000` here is the most conservative value
/// `hello-memcard-min` currently sweeps (`NOACK 8K DSEL100K WGAP4M`),
/// pending on-console confirmation of the actual minimum needed; it costs
/// only extra milliseconds on an infrequent save, so erring high first.
fn card(slot: Slot) -> HardwareCard {
    HardwareCard::with_noack(slot, 1_024, 32_768, 8_000, 100_000, 4_000_000)
}

/// PS1 BGR555: 5 bits/channel, in the low bits (R, then G, then B).
const fn bgr555((r, g, b): (u8, u8, u8)) -> u16 {
    ((r as u16 >> 3) & 0x1F) | (((g as u16 >> 3) & 0x1F) << 5) | (((b as u16 >> 3) & 0x1F) << 10)
}

/// The save icon: a gold ascending-levels chevron over a partial XP bar,
/// framed and coloured with the same panel palette as the in-game UI.
fn save_icon() -> Icon {
    let clut = [
        0x0000,
        bgr555(PANEL_BORDER),
        bgr555(PANEL_FILL),
        bgr555(GOLD),
        bgr555(BAR_BG),
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    let mut grid = [[2u8; 16]; 16]; // panel fill everywhere by default
    grid[0] = [1; 16];
    grid[15] = [1; 16];
    for row in grid.iter_mut() {
        row[0] = 1;
        row[15] = 1;
    }

    // Chevron widening by one column per side per row, apex at the top.
    for y in 2..=8usize {
        let (left, right) = (9 - y, y + 6);
        for x in left..=right {
            grid[y][x] = 3;
        }
    }

    // Partial XP bar: filled cols 2..=9, empty cols 10..=13.
    for y in 11..=12usize {
        for x in 2..=9usize {
            grid[y][x] = 3;
        }
        for x in 10..=13usize {
            grid[y][x] = 4;
        }
    }

    Icon::new(clut, &grid)
}

fn save_game(game: &Game) -> SaveStatus {
    let payload = serialize(game);
    let mut saw_full = false;
    for slot in slot_order(game.save_slot) {
        let mut card = Card::new(card(slot));
        let formatted = match card.is_formatted() {
            Ok(v) => v,
            Err(_) => continue, // no card in this slot, or unreadable
        };
        if !formatted && card.format().is_err() {
            continue;
        }
        match card.write_with_icon(SAVE_NAME, SAVE_DESC, &payload, &save_icon()) {
            Ok(()) => return SaveStatus::Saved(slot),
            Err(McError::NoSpace) => saw_full = true,
            Err(_) => {}
        }
    }
    if saw_full {
        SaveStatus::Full
    } else {
        SaveStatus::NoCard
    }
}

fn load_game() -> Option<(SaveData, Slot)> {
    for slot in [Slot::One, Slot::Two] {
        let mut card = Card::new(card(slot));
        let mut buf = [0u8; 512];
        if let Ok(n) = card.read(SAVE_NAME, &mut buf) {
            if let Some(data) = deserialize(&buf[..n]) {
                return Some((data, slot));
            }
        }
    }
    None
}

// --- Game state -----------------------------------------------------------

struct Game {
    screen: Screen,
    xp: [u32; NUM_SKILLS],
    inventory: [u32; NUM_ITEMS],
    gold: u32,
    tokens: u32,
    has_badge: bool,
    hub_cursor: usize,
    action_skill: usize,
    action_tier: [usize; NUM_SKILLS],
    modes: [Mode; NUM_SKILLS],
    cooldown: [u16; NUM_SKILLS],
    ready: [bool; NUM_SKILLS],
    last_gain: LastGain,
    inv_cursor: usize,
    shop_cursor: usize,
    equip_cursor: usize,
    goals_cursor: usize,
    token_cursor: usize,
    level_up: u16,
    level_up_skill: usize,
    level_up_level: u8,
    tool_tier: [usize; 3],
    lifetime_gathered: [u32; NUM_ITEMS],
    lifetime_gold: u32,
    goals_claimed: u64,
    goal_flash: u16,
    goal_flash_idx: usize,
    save_flash: u16,
    save_status: SaveStatus,
    save_slot: Option<Slot>,
    pending_save: bool,
    has_save: bool,
    rng: u32,
    frame: u32,
}

fn xorshift32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn tool_speed_pct(game: &Game, skill: usize) -> u32 {
    if skill < 3 {
        TOOL_TIERS[game.tool_tier[skill]].speed_pct
    } else {
        100
    }
}

fn active_cooldown(tier: usize, speed_pct: u32) -> u16 {
    let base = 90 + (tier as u32) * 20;
    ((base * speed_pct) / 100) as u16
}
fn passive_cooldown(tier: usize, speed_pct: u32) -> u16 {
    active_cooldown(tier, speed_pct) * 5 / 3
}
fn full_cooldown(mode: Mode, tier: usize, speed_pct: u32) -> u16 {
    match mode {
        Mode::Active => active_cooldown(tier, speed_pct),
        Mode::Passive => passive_cooldown(tier, speed_pct),
    }
}

fn next_owned_item(inv: &[u32; NUM_ITEMS], from: usize, forward: bool) -> usize {
    let mut i = from;
    for _ in 0..NUM_ITEMS {
        i = if forward {
            (i + 1) % NUM_ITEMS
        } else {
            (i + NUM_ITEMS - 1) % NUM_ITEMS
        };
        if inv[i] > 0 {
            return i;
        }
    }
    from
}

fn perform_action(game: &mut Game, skill: usize, tier_idx: usize) {
    let tier = &SKILLS[skill].tiers[tier_idx];

    if let Some(consume_id) = tier.consumes {
        if game.inventory[consume_id] == 0 {
            game.last_gain = LastGain::NoRawFish { item: consume_id };
            return;
        }
        game.inventory[consume_id] -= 1;
    }

    let level = level_for_xp(game.xp[skill]);
    let mut burnt = false;
    let mut produced = tier.item;
    if let Some(burnt_id) = tier.burnt_item {
        let stop_burn = tier.level_req.saturating_add(15);
        let burn_chance: u32 = if level >= stop_burn {
            0
        } else {
            40 * (stop_burn - level) as u32 / 15
        };
        if xorshift32(&mut game.rng) % 100 < burn_chance {
            burnt = true;
            produced = burnt_id;
        }
    }
    game.inventory[produced] += 1;
    game.lifetime_gathered[produced] = game.lifetime_gathered[produced].saturating_add(1);

    let xp_gain = if burnt { 0 } else { tier.xp };
    let old_level = level;
    game.xp[skill] = game.xp[skill].saturating_add(xp_gain as u32);
    let new_level = level_for_xp(game.xp[skill]);
    if new_level > old_level {
        game.level_up = LEVEL_UP_FRAMES;
        game.level_up_skill = skill;
        game.level_up_level = new_level;
    }
    game.last_gain = LastGain::Gained { xp: xp_gain, item: produced, burnt };
}

#[no_mangle]
fn main() {
    tty::println("skillscape: booted via HLE BIOS");

    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);

    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    let mut pad = PadTracker::new();

    let mut game = Game {
        screen: Screen::Title,
        xp: [0; NUM_SKILLS],
        inventory: [0; NUM_ITEMS],
        gold: 0,
        tokens: 0,
        has_badge: false,
        hub_cursor: 0,
        action_skill: 0,
        action_tier: [0; NUM_SKILLS],
        modes: [Mode::Active; NUM_SKILLS],
        cooldown: [0; NUM_SKILLS],
        ready: [false; NUM_SKILLS],
        last_gain: LastGain::None,
        inv_cursor: 0,
        shop_cursor: 0,
        equip_cursor: 0,
        goals_cursor: 0,
        token_cursor: 0,
        level_up: 0,
        level_up_skill: 0,
        level_up_level: 1,
        tool_tier: [0; 3],
        lifetime_gathered: [0; NUM_ITEMS],
        lifetime_gold: 0,
        goals_claimed: 0,
        goal_flash: 0,
        goal_flash_idx: 0,
        save_flash: 0,
        save_status: SaveStatus::NoCard,
        save_slot: None,
        pending_save: false,
        has_save: false,
        rng: 0xC0FF_EE11,
        frame: 0,
    };

    tty::println("skillscape: attempting memory-card load");
    if let Some((save, slot)) = load_game() {
        game.xp = save.xp;
        game.inventory = save.inventory;
        game.gold = save.gold;
        game.tool_tier = save.tool_tier;
        game.lifetime_gathered = save.lifetime_gathered;
        game.lifetime_gold = save.lifetime_gold;
        game.goals_claimed = save.goals_claimed;
        game.tokens = save.tokens;
        game.has_badge = save.has_badge;
        game.has_save = true;
        game.save_slot = Some(slot);
        tty::println("skillscape: loaded save");
    } else {
        tty::println("skillscape: no save found, starting fresh");
    }

    tty::println("skillscape: entering render loop");
    loop {
        // A pending save runs on its own frame, with no pad poll in the
        // same iteration. `poll_port1()` and the memory-card protocol
        // both ride SIO0, and doing a full is_formatted/format/write
        // sequence immediately after that frame's pad poll left stale
        // controller-transaction state that made card I/O report
        // success even with no card in either slot. One frame with
        // nothing but the card transaction avoids the interleave.
        if game.pending_save {
            game.pending_save = false;
            game.save_status = save_game(&game);
            if let SaveStatus::Saved(slot) = game.save_status {
                game.save_slot = Some(slot);
            }
            game.save_flash = SAVE_FLASH_FRAMES;
        } else {
            pad.update(poll_port1().buttons.bits());
            game.frame = game.frame.wrapping_add(1);

            match game.screen {
                Screen::Title => update_title(&mut game, &pad),
                Screen::Hub => update_hub(&mut game, &pad),
                Screen::Action => update_action(&mut game, &pad),
                Screen::Inventory => update_inventory(&mut game, &pad),
                Screen::Shop => update_shop(&mut game, &pad),
                Screen::Goals => update_goals(&mut game, &pad),
                Screen::Equipment => update_equipment(&mut game, &pad),
                Screen::TokenShop => update_token_shop(&mut game, &pad),
            }
            check_goals(&mut game);
            if game.level_up > 0 {
                game.level_up -= 1;
            }
            if game.goal_flash > 0 {
                game.goal_flash -= 1;
            }
            if game.save_flash > 0 {
                game.save_flash -= 1;
            }
        }

        fb.clear(BG.0, BG.1, BG.2);

        match game.screen {
            Screen::Title => draw_title(&font, &game),
            Screen::Hub => draw_hub(&font, &game),
            Screen::Action => draw_action(&font, &game),
            Screen::Inventory => draw_inventory(&font, &game),
            Screen::Shop => draw_shop(&font, &game),
            Screen::Goals => draw_goals(&font, &game),
            Screen::Equipment => draw_equipment(&font, &game),
            Screen::TokenShop => draw_token_shop(&font, &game),
        }
        if game.level_up > 0 {
            draw_level_up(&font, &game);
        }
        if game.goal_flash > 0 {
            draw_goal_complete(&font, &game);
        }

        gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
    }
}

// --- Update -------------------------------------------------------------

fn update_title(game: &mut Game, pad: &PadTracker) {
    if pad.just_pressed(button::START) {
        game.screen = Screen::Hub;
    }
}

fn update_hub(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 8) {
        game.hub_cursor = (game.hub_cursor + 1) % NUM_SKILLS;
    }
    if pad.repeats(button::UP, 15, 8) {
        game.hub_cursor = (game.hub_cursor + NUM_SKILLS - 1) % NUM_SKILLS;
    }
    if pad.just_pressed(button::CROSS) {
        game.action_skill = game.hub_cursor;
        game.last_gain = LastGain::None;
        game.screen = Screen::Action;
    }
    if pad.just_pressed(button::L1) {
        game.screen = Screen::Inventory;
    }
    if pad.just_pressed(button::R1) {
        game.shop_cursor = next_owned_item(&game.inventory, NUM_ITEMS - 1, true);
        game.screen = Screen::Shop;
    }
    if pad.just_pressed(button::L2) {
        game.screen = Screen::Goals;
    }
    if pad.just_pressed(button::R2) {
        game.screen = Screen::Equipment;
    }
    if pad.just_pressed(button::SELECT) {
        game.screen = Screen::TokenShop;
    }
    if pad.just_pressed(button::START) {
        game.pending_save = true;
    }
}

fn update_action(game: &mut Game, pad: &PadTracker) {
    let skill = game.action_skill;
    let speed = tool_speed_pct(game, skill);

    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
        return;
    }
    // Switching mode or tier always forces a *full* fresh cooldown --
    // never zero. Resetting to zero here used to mean "instantly ready
    // to fire" in Passive mode, which an active toggle would trigger on
    // the very same frame: spamming TRIANGLE was a free-XP exploit,
    // since every toggle-to-Passive produced one action immediately.
    if pad.just_pressed(button::TRIANGLE) {
        game.modes[skill] = match game.modes[skill] {
            Mode::Active => Mode::Passive,
            Mode::Passive => Mode::Active,
        };
        game.cooldown[skill] = full_cooldown(game.modes[skill], game.action_tier[skill], speed);
        game.ready[skill] = false;
    }
    if pad.repeats(button::RIGHT, 15, 10) {
        game.action_tier[skill] = (game.action_tier[skill] + 1) % NUM_TIERS;
        game.cooldown[skill] = full_cooldown(game.modes[skill], game.action_tier[skill], speed);
        game.ready[skill] = false;
    }
    if pad.repeats(button::LEFT, 15, 10) {
        game.action_tier[skill] = (game.action_tier[skill] + NUM_TIERS - 1) % NUM_TIERS;
        game.cooldown[skill] = full_cooldown(game.modes[skill], game.action_tier[skill], speed);
        game.ready[skill] = false;
    }

    let tier_idx = game.action_tier[skill];
    let level = level_for_xp(game.xp[skill]);
    let unlocked = level >= SKILLS[skill].tiers[tier_idx].level_req;
    if !unlocked {
        return;
    }

    match game.modes[skill] {
        Mode::Passive => {
            if game.cooldown[skill] > 0 {
                game.cooldown[skill] -= 1;
            } else {
                perform_action(game, skill, tier_idx);
                game.cooldown[skill] = passive_cooldown(tier_idx, speed);
            }
        }
        Mode::Active => {
            if game.cooldown[skill] > 0 {
                game.cooldown[skill] -= 1;
                game.ready[skill] = false;
            } else {
                game.ready[skill] = true;
            }
            if game.ready[skill] && pad.just_pressed(button::CROSS) {
                perform_action(game, skill, tier_idx);
                game.cooldown[skill] = active_cooldown(tier_idx, speed);
                game.ready[skill] = false;
            }
        }
    }
}

fn update_inventory(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 8) {
        game.inv_cursor = (game.inv_cursor + 1) % NUM_ITEMS;
    }
    if pad.repeats(button::UP, 15, 8) {
        game.inv_cursor = (game.inv_cursor + NUM_ITEMS - 1) % NUM_ITEMS;
    }
    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
    }
}

fn update_shop(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 8) {
        game.shop_cursor = next_owned_item(&game.inventory, game.shop_cursor, true);
    }
    if pad.repeats(button::UP, 15, 8) {
        game.shop_cursor = next_owned_item(&game.inventory, game.shop_cursor, false);
    }
    if pad.just_pressed(button::CROSS) {
        let id = game.shop_cursor;
        let qty = game.inventory[id];
        if qty > 0 {
            let earned = ITEMS[id].sell_price * qty;
            game.gold = game.gold.saturating_add(earned);
            game.lifetime_gold = game.lifetime_gold.saturating_add(earned);
            game.inventory[id] = 0;
            game.shop_cursor = next_owned_item(&game.inventory, id, true);
        }
    }
    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
    }
}

fn update_goals(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 10) {
        game.goals_cursor = (game.goals_cursor + 1).min(NUM_GOALS - 1);
    }
    if pad.repeats(button::UP, 15, 10) {
        game.goals_cursor = game.goals_cursor.saturating_sub(1);
    }
    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
    }
}

fn update_equipment(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 8) {
        game.equip_cursor = (game.equip_cursor + 1) % 3;
    }
    if pad.repeats(button::UP, 15, 8) {
        game.equip_cursor = (game.equip_cursor + 2) % 3;
    }
    if pad.just_pressed(button::CROSS) {
        let skill = game.equip_cursor;
        let cur = game.tool_tier[skill];
        if cur + 1 < NUM_TOOL_TIERS {
            let next = &TOOL_TIERS[cur + 1];
            let level = level_for_xp(game.xp[skill]);
            if game.gold >= next.cost && level >= next.level_req {
                game.gold -= next.cost;
                game.tool_tier[skill] = cur + 1;
            }
        }
    }
    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
    }
}

fn update_token_shop(game: &mut Game, pad: &PadTracker) {
    if pad.repeats(button::DOWN, 15, 8) {
        game.token_cursor = (game.token_cursor + 1) % NUM_TOKEN_ITEMS;
    }
    if pad.repeats(button::UP, 15, 8) {
        game.token_cursor = (game.token_cursor + NUM_TOKEN_ITEMS - 1) % NUM_TOKEN_ITEMS;
    }
    if pad.just_pressed(button::CROSS) {
        let item = game.token_cursor;
        let already_owned = item == TOKEN_MASTER_BADGE && game.has_badge;
        if !already_owned && game.tokens >= TOKEN_ITEMS[item].cost {
            game.tokens -= TOKEN_ITEMS[item].cost;
            apply_token_item(game, item);
        }
    }
    if pad.just_pressed(button::CIRCLE) {
        game.screen = Screen::Hub;
    }
}

// --- Draw -----------------------------------------------------------------

fn draw_panel(x: i16, y: i16, w: u16, h: u16) {
    gpu::draw_rect_flat(x, y, w, h, PANEL_BORDER.0, PANEL_BORDER.1, PANEL_BORDER.2);
    gpu::draw_rect_flat(x + 2, y + 2, w - 4, h - 4, PANEL_FILL.0, PANEL_FILL.1, PANEL_FILL.2);
}

fn draw_bar(x: i16, y: i16, w: u16, h: u16, frac_num: i32, frac_den: i32, fill: (u8, u8, u8)) {
    gpu::draw_rect_flat(x, y, w, h, BAR_BG.0, BAR_BG.1, BAR_BG.2);
    let filled = if frac_den <= 0 {
        w as i32
    } else {
        (w as i32 * frac_num / frac_den).clamp(0, w as i32)
    };
    if filled > 0 {
        gpu::draw_rect_flat(x, y, filled as u16, h, fill.0, fill.1, fill.2);
    }
}

fn draw_title(font: &FontAtlas, game: &Game) {
    let title = "SKILLSCAPE";
    font.draw_text(119, 50, title, GOLD);
    font.draw_text(120, 50, title, GOLD);
    font.draw_text(32, 82, "AN OSRS-INSPIRED IDLE ADVENTURE", GRAY);

    font.draw_text(32, 116, "CHOP MINE FISH COOK SMITH FLETCH", WHITE);
    font.draw_text(24, 128, "LEVEL UP SIX SKILLS FROM 1 TO 99", GRAY);
    font.draw_text(4, 150, "GATHER, SELL, BUY BETTER TOOLS, REPEAT.", GRAY);

    let start_label = if game.has_save {
        "PRESS START TO CONTINUE"
    } else {
        "PRESS START - NEW GAME"
    };
    if (game.frame / 30) % 2 == 0 {
        let x = if game.has_save { 92 } else { 96 };
        font.draw_text(x, 186, start_label, WHITE);
    }
}

fn draw_hub(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 18, "SKILLSCAPE", GOLD);
    font.draw_text(112, 18, "GOLD", GRAY);
    font.draw_text(148, 18, dec5(game.gold).as_str(), GOLD);
    font.draw_text(204, 18, "TOK", PURPLE);
    font.draw_text(232, 18, dec5(game.tokens).as_str(), PURPLE);
    if game.has_badge {
        font.draw_text(280, 18, "MSTR", PURPLE);
    }

    let row_h: i16 = 26;
    let top: i16 = 34;
    for i in 0..NUM_SKILLS {
        let y = top + row_h * i as i16;
        let level = level_for_xp(game.xp[i]);
        let selected = i == game.hub_cursor;

        if selected {
            gpu::draw_rect_flat(8, y - 1, 304, 22, 60, 46, 26);
            font.draw_text(8, y, ">", GOLD);
        }
        font.draw_text(24, y, SKILLS[i].name, GOLD);
        font.draw_text(240, y, "LV", GRAY);
        font.draw_text(264, y, dec3(level as u32).as_str(), WHITE);

        let lo = xp_for_level(level);
        let hi = xp_for_level(level + 1);
        draw_bar(24, y + 11, 272, 5, (game.xp[i] - lo) as i32, (hi - lo) as i32, BAR_XP);
    }

    if game.save_flash > 0 {
        let (msg, tint) = match game.save_status {
            SaveStatus::Saved(Slot::One) => ("GAME SAVED! (MEMORY CARD 1)", GREEN),
            SaveStatus::Saved(Slot::Two) => ("GAME SAVED! (MEMORY CARD 2)", GREEN),
            SaveStatus::Full => ("SAVE FAILED -- MEMORY CARD FULL", RED),
            SaveStatus::NoCard => ("SAVE FAILED -- NO MEMORY CARD", RED),
        };
        font.draw_text(8, 194, msg, tint);
    } else {
        font.draw_text(8, 194, "X:OPEN  START:SAVE  SELECT:TOKENS", GRAY);
    }
    font.draw_text(8, 206, "L1:INV  R1:SHOP  L2:GOALS  R2:GEAR", GRAY);
}

fn draw_action(font: &FontAtlas, game: &Game) {
    let skill = game.action_skill;
    let def = &SKILLS[skill];
    let level = level_for_xp(game.xp[skill]);
    let tier_idx = game.action_tier[skill];
    let tier = &def.tiers[tier_idx];
    let unlocked = level >= tier.level_req;

    font.draw_text(20, 20, def.name, GOLD);
    font.draw_text(240, 20, "LV", GRAY);
    font.draw_text(264, 20, dec3(level as u32).as_str(), WHITE);

    let lo = xp_for_level(level);
    let hi = xp_for_level(level + 1);
    draw_bar(20, 32, 280, 7, (game.xp[skill] - lo) as i32, (hi - lo) as i32, BAR_XP);
    if level >= MAX_LEVEL {
        font.draw_text(20, 42, "MAXED OUT", GREEN);
    } else {
        font.draw_text(20, 42, "XP TO NEXT LV:", GRAY);
        font.draw_text(140, 42, dec7(hi - game.xp[skill]).as_str(), WHITE);
    }

    // Tier selector.
    draw_panel(20, 60, 280, 34);
    if unlocked {
        font.draw_text(30, 68, "<", WHITE);
        font.draw_text(290, 68, ">", WHITE);
        font.draw_text(60, 68, tier.name, GOLD);
        font.draw_text(60, 80, "+", GRAY);
        font.draw_text(70, 80, dec3(tier.xp as u32).as_str(), WHITE);
        font.draw_text(110, 80, "XP", GRAY);
        font.draw_text(150, 80, ITEMS[tier.item].name, WHITE);
    } else {
        font.draw_text(60, 68, tier.name, GRAY);
        font.draw_text(60, 80, "REQUIRES LEVEL", RED);
        font.draw_text(190, 80, dec3(tier.level_req as u32).as_str(), RED);
    }

    // Mode + equipped tool.
    let (mode_label, mode_color) = match game.modes[skill] {
        Mode::Active => ("ACTIVE", GREEN),
        Mode::Passive => ("PASSIVE", BLUE),
    };
    font.draw_text(20, 104, "MODE (TRIANGLE):", GRAY);
    font.draw_text(180, 104, mode_label, mode_color);
    if skill < 3 {
        // Fixed-width "TOOL N/4" -- tier NAMEs vary too much in length
        // (BRONZE..MITHRIL) to safely pack next to the mode label
        // without risking an off-screen overflow on the longest ones.
        font.draw_text(240, 104, "TOOL", GRAY);
        font.draw_text(280, 104, dec1(game.tool_tier[skill] as u32 + 1).as_str(), GOLD);
        font.draw_text(288, 104, "/", GRAY);
        font.draw_text(296, 104, dec1(NUM_TOOL_TIERS as u32).as_str(), GRAY);
    }

    // Action progress / status.
    if unlocked {
        let speed = tool_speed_pct(game, skill);
        let cooldown_max = full_cooldown(game.modes[skill], tier_idx, speed) as i32;
        let progress = cooldown_max - game.cooldown[skill] as i32;
        draw_bar(20, 122, 280, 12, progress, cooldown_max, BAR_ACTION);

        let status = match game.modes[skill] {
            Mode::Active if game.ready[skill] => "READY -- PRESS X",
            Mode::Active => def.verb,
            Mode::Passive => def.verb,
        };
        let status_color = if game.ready[skill] && game.modes[skill] == Mode::Active {
            GOLD
        } else {
            WHITE
        };
        font.draw_text(20, 138, status, status_color);
    } else {
        draw_bar(20, 122, 280, 12, 0, 1, BAR_ACTION);
        font.draw_text(20, 138, "LOCKED", RED);
    }

    // Last gain message.
    match game.last_gain {
        LastGain::None => {
            font.draw_text(20, 162, "...", GRAY);
        }
        LastGain::Gained { xp, item, burnt } => {
            if burnt {
                font.draw_text(20, 162, "BURNT IT!", RED);
                font.draw_text(120, 162, "+1", GRAY);
                font.draw_text(150, 162, ITEMS[item].name, GRAY);
            } else {
                font.draw_text(20, 162, "+", GREEN);
                font.draw_text(30, 162, dec3(xp as u32).as_str(), GREEN);
                font.draw_text(70, 162, "XP", GREEN);
                font.draw_text(120, 162, "+1", WHITE);
                font.draw_text(150, 162, ITEMS[item].name, WHITE);
            }
        }
        LastGain::NoRawFish { item } => {
            font.draw_text(20, 162, "NEED", RED);
            font.draw_text(60, 162, ITEMS[item].name, RED);
        }
    }

    let show_item = tier.consumes.unwrap_or(tier.item);
    font.draw_text(20, 184, "YOU HAVE", GRAY);
    font.draw_text(90, 184, dec5(game.inventory[show_item]).as_str(), WHITE);
    font.draw_text(138, 184, ITEMS[show_item].name, WHITE);

    font.draw_text(8, 210, "O:BACK  TRI:MODE  L/R:TIER  X:ACT", GRAY);
}

const INVENTORY_VISIBLE_ROWS: usize = 14;

fn draw_inventory(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 12, "INVENTORY", GOLD);
    font.draw_text(220, 12, "GOLD", GRAY);
    font.draw_text(260, 12, dec7(game.gold).as_str(), GOLD);

    let top: i16 = 28;
    let row_h: i16 = 12;
    let max_scroll = NUM_ITEMS.saturating_sub(INVENTORY_VISIBLE_ROWS);
    let scroll = game
        .inv_cursor
        .saturating_sub(INVENTORY_VISIBLE_ROWS - 1)
        .min(max_scroll);

    for row in 0..INVENTORY_VISIBLE_ROWS.min(NUM_ITEMS) {
        let i = scroll + row;
        let y = top + row_h * row as i16;
        let selected = i == game.inv_cursor;
        let owned = game.inventory[i] > 0;
        let name_color = if owned { WHITE } else { GRAY };
        if selected {
            gpu::draw_rect_flat(8, y - 1, 304, 11, 50, 38, 22);
            font.draw_text(8, y, ">", GOLD);
        }
        font.draw_text(24, y, ITEMS[i].name, name_color);
        font.draw_text(180, y, "x", GRAY);
        font.draw_text(188, y, dec5(game.inventory[i]).as_str(), name_color);
        font.draw_text(240, y, "WORTH", GRAY);
        font.draw_text(288, y, dec3(ITEMS[i].sell_price).as_str(), GRAY);
    }

    font.draw_text(8, 210, "UP/DOWN SCROLL  O:BACK", GRAY);
}

fn draw_shop(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 20, "GENERAL STORE", GOLD);
    font.draw_text(220, 20, "GOLD", GRAY);
    font.draw_text(260, 20, dec7(game.gold).as_str(), GOLD);

    let any_owned = game.inventory.iter().any(|&q| q > 0);
    if !any_owned {
        font.draw_text(40, 100, "YOU HAVE NOTHING TO SELL", RED);
        font.draw_text(8, 210, "O:BACK", GRAY);
        return;
    }

    let top: i16 = 34;
    let row_h: i16 = 9;
    let mut y = top;
    for i in 0..NUM_ITEMS {
        if game.inventory[i] == 0 {
            continue;
        }
        let selected = i == game.shop_cursor;
        if selected {
            gpu::draw_rect_flat(8, y - 1, 304, 8, 50, 38, 22);
            font.draw_text(8, y, ">", GOLD);
        }
        font.draw_text(24, y, ITEMS[i].name, WHITE);
        font.draw_text(180, y, "x", GRAY);
        font.draw_text(188, y, dec5(game.inventory[i]).as_str(), WHITE);
        font.draw_text(230, y, "@", GRAY);
        font.draw_text(240, y, dec3(ITEMS[i].sell_price).as_str(), GOLD);
        y += row_h;
    }

    font.draw_text(8, 210, "UP/DOWN SELECT  X:SELL ALL  O:BACK", GRAY);
}

const GOALS_VISIBLE_ROWS: usize = 12;

fn draw_goals(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 14, "GOALS", GOLD);
    let done = (0..NUM_GOALS).filter(|&i| game.goals_claimed & (1 << i) != 0).count();
    font.draw_text(150, 14, dec3(done as u32).as_str(), GREEN);
    font.draw_text(174, 14, "/", GRAY);
    font.draw_text(182, 14, dec3(NUM_GOALS as u32).as_str(), GRAY);
    font.draw_text(206, 14, "DONE", GRAY);
    font.draw_text(244, 14, "TOK", PURPLE);
    font.draw_text(272, 14, dec5(game.tokens).as_str(), PURPLE);

    let top: i16 = 28;
    let row_h: i16 = 14;
    let max_scroll = NUM_GOALS.saturating_sub(GOALS_VISIBLE_ROWS);
    let scroll = game
        .goals_cursor
        .saturating_sub(GOALS_VISIBLE_ROWS - 1)
        .min(max_scroll);

    for row in 0..GOALS_VISIBLE_ROWS.min(NUM_GOALS) {
        let i = scroll + row;
        let y = top + row_h * row as i16;
        let claimed = game.goals_claimed & (1 << i) != 0;
        let selected = i == game.goals_cursor;
        let mark = if claimed { "X" } else { "-" };
        let mark_color = if claimed { GREEN } else { GRAY };
        let desc_color = if claimed { GRAY } else { WHITE };
        if selected {
            gpu::draw_rect_flat(4, y - 1, 312, 11, 40, 31, 20);
        }
        font.draw_text(8, y, mark, mark_color);
        font.draw_text(24, y, GOALS[i].desc, desc_color);
        if claimed {
            font.draw_text(220, y, "DONE", GREEN);
        } else {
            let progress = goal_progress(game, GOALS[i].metric).min(GOALS[i].target);
            font.draw_text(216, y, dec5(progress).as_str(), WHITE);
            font.draw_text(260, y, "/", GRAY);
            font.draw_text(272, y, dec5(GOALS[i].target).as_str(), GRAY);
        }
    }

    font.draw_text(8, 210, "UP/DOWN SCROLL  O:BACK", GRAY);
}

fn draw_equipment(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 20, "EQUIPMENT", GOLD);
    font.draw_text(220, 20, "GOLD", GRAY);
    font.draw_text(260, 20, dec7(game.gold).as_str(), GOLD);

    let top: i16 = 48;
    let row_h: i16 = 44;
    for skill in 0..3 {
        let y = top + row_h * skill as i16;
        let selected = skill == game.equip_cursor;
        if selected {
            gpu::draw_rect_flat(8, y - 4, 304, 40, 60, 46, 26);
            font.draw_text(8, y, ">", GOLD);
        }
        font.draw_text(24, y, SKILLS[skill].name, GOLD);
        font.draw_text(24, y + 12, "EQUIPPED", GRAY);
        font.draw_text(100, y + 12, TOOL_TIERS[game.tool_tier[skill]].name, WHITE);
        font.draw_text(180, y + 12, TOOL_NOUNS[skill], WHITE);

        let cur = game.tool_tier[skill];
        if cur + 1 >= NUM_TOOL_TIERS {
            font.draw_text(24, y + 24, "MAX TIER", GREEN);
        } else {
            let next = &TOOL_TIERS[cur + 1];
            let level = level_for_xp(game.xp[skill]);
            let affordable = game.gold >= next.cost && level >= next.level_req;
            let tint = if affordable { GREEN } else { RED };
            font.draw_text(24, y + 24, "UPGRADE", GRAY);
            font.draw_text(90, y + 24, next.name, tint);
            font.draw_text(150, y + 24, dec5(next.cost).as_str(), tint);
            font.draw_text(190, y + 24, "G  LV", GRAY);
            font.draw_text(230, y + 24, dec3(next.level_req as u32).as_str(), tint);
        }
    }

    font.draw_text(8, 210, "UP/DOWN SELECT  X:UPGRADE  O:BACK", GRAY);
}

fn draw_token_shop(font: &FontAtlas, game: &Game) {
    font.draw_text(20, 20, "TOKEN SHOP", PURPLE);
    font.draw_text(220, 20, "TOK", PURPLE);
    font.draw_text(260, 20, dec5(game.tokens).as_str(), PURPLE);
    font.draw_text(8, 34, "EARNED BY COMPLETING GOALS.", GRAY);

    let top: i16 = 54;
    let row_h: i16 = 44;
    for i in 0..NUM_TOKEN_ITEMS {
        let y = top + row_h * i as i16;
        let selected = i == game.token_cursor;
        if selected {
            gpu::draw_rect_flat(8, y - 4, 304, 40, 50, 38, 55);
            font.draw_text(8, y, ">", PURPLE);
        }
        font.draw_text(24, y, TOKEN_ITEMS[i].name, PURPLE);
        let owned = i == TOKEN_MASTER_BADGE && game.has_badge;
        if owned {
            font.draw_text(220, y, "OWNED", GREEN);
        } else {
            let affordable = game.tokens >= TOKEN_ITEMS[i].cost;
            let tint = if affordable { GREEN } else { RED };
            font.draw_text(220, y, dec3(TOKEN_ITEMS[i].cost).as_str(), tint);
            font.draw_text(248, y, "TOK", tint);
        }
        font.draw_text(24, y + 14, TOKEN_ITEMS[i].desc, GRAY);
    }

    font.draw_text(8, 210, "UP/DOWN SELECT  X:BUY  O:BACK", GRAY);
}

fn draw_level_up(font: &FontAtlas, game: &Game) {
    if game.level_up == 0 {
        return;
    }
    draw_panel(48, 92, 224, 40);
    font.draw_text(60, 100, "LEVEL UP!", GOLD);
    font.draw_text(60, 116, SKILLS[game.level_up_skill].name, WHITE);
    font.draw_text(220, 116, "LV", GRAY);
    font.draw_text(244, 116, dec3(game.level_up_level as u32).as_str(), GOLD);
}

fn draw_goal_complete(font: &FontAtlas, game: &Game) {
    if game.goal_flash == 0 {
        return;
    }
    draw_panel(24, 150, 272, 40);
    font.draw_text(36, 158, "GOAL COMPLETE!", GOLD);
    font.draw_text(36, 174, GOALS[game.goal_flash_idx].desc, WHITE);
    font.draw_text(220, 174, "+", GREEN);
    font.draw_text(228, 174, dec3(GOALS[game.goal_flash_idx].gold_reward).as_str(), GREEN);
    font.draw_text(256, 174, "+", PURPLE);
    font.draw_text(264, 174, dec3(GOALS[game.goal_flash_idx].token_reward).as_str(), PURPLE);
}

// --- Fixed-width decimal formatters, no alloc ---------------------------

struct Dec1([u8; 1]);
impl Dec1 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}
fn dec1(v: u32) -> Dec1 {
    Dec1([b'0' + (v.min(9) as u8)])
}

struct Dec3([u8; 3]);
impl Dec3 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}
fn dec3(v: u32) -> Dec3 {
    let v = v.min(999);
    Dec3([b'0' + (v / 100) as u8, b'0' + (v / 10 % 10) as u8, b'0' + (v % 10) as u8])
}

struct Dec5([u8; 5]);
impl Dec5 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}
fn dec5(v: u32) -> Dec5 {
    let v = v.min(99_999);
    let mut out = [0u8; 5];
    let mut n = v;
    for slot in out.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    Dec5(out)
}

struct Dec7([u8; 7]);
impl Dec7 {
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}
fn dec7(v: u32) -> Dec7 {
    let v = v.min(9_999_999);
    let mut out = [0u8; 7];
    let mut n = v;
    for slot in out.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    Dec7(out)
}
