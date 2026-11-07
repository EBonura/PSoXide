//! HL sound effects: one WORLD.PAK chunk of cooked SPU-ADPCM samples,
//! uploaded to SPU RAM once at boot (zero main-RAM cost afterwards).
//!
//! Pack layout (`host/hl-content` -- ID ORDER MUST MATCH the consts here):
//!   "HSFX" | u32 count | count x (u32 offset, u32 len) | .psau blobs
//!
//! Playback rotates a pool of one-shot voices; volume is a linear fraction
//! (den 1 = full). `play_at` derives the fraction from world distance.

use psx_asset::Audio;
use psx_sfx::{OneShot, Sample, PARKING_TAIL as SAMPLE_TAIL};
use psx_spu::{self as spu, Adsr, SpuAddr, Voice, Volume};

pub const CHUNK_ID: u32 = 3000;

// ---- sample ids (host/hl-content `SOUNDS` order) ----
pub const FIRE_GLOCK18: u8 = 0;
pub const FIRE_MP5NAVY: u8 = 1;
pub const FIRE_M3: u8 = 2;
pub const FIRE_DEAGLE: u8 = 3;
pub const FIRE_SCOUT: u8 = 4;
pub const FIRE_M249: u8 = 5;
pub const FIRE_AWP: u8 = 6;
pub const KNIFE_SLASH: u8 = 7;
pub const KNIFE_HITWALL: u8 = 8;
pub const EXPLODE: u8 = 9;
pub const RIC: u8 = 10;
pub const ELECTRO: u8 = 11;
pub const PAIN: u8 = 12;
pub const BODYDROP: u8 = 13;
pub const BUTTON: u8 = 14;
pub const SUIT: u8 = 15;
pub const GLASS_BREAK: u8 = 16;
pub const WOOD_BREAK: u8 = 17;
pub const MEDSHOT: u8 = 18;
pub const STEP1: u8 = 19;
pub const STEP2: u8 = 20;
pub const RELOAD: u8 = 21;
pub const DRY: u8 = 22;
pub const CHARGER_HEALTH_LOOP: u8 = 23;
pub const CHARGER_HEV_LOOP: u8 = 24;
pub const FLASHLIGHT: u8 = 25;
pub const M203: u8 = 26;
pub const FIRE_XM1014: u8 = 27;
pub const GAUSS_CHARGE: u8 = 28;
pub const HEALTH_DENY: u8 = 29;
pub const SUIT_DENY: u8 = 30;
pub const RELOAD_DEAGLE: u8 = 31;
pub const RELOAD_SCOUT: u8 = 32;
pub const RIFLE_CLIP_OUT: u8 = 33;
pub const RIFLE_CLIP_IN: u8 = 34;
pub const PISTOL_CLIP_OUT: u8 = 35;
pub const M3_INSERT_SHELL: u8 = 36;
pub const M3_PUMP: u8 = 37;
pub const MENU_MOVE: u8 = 38;
pub const BULLET_HIT1: u8 = 39;
pub const BULLET_HIT2: u8 = 40;
pub const WOOD_IMPACT: u8 = 41;
pub const KNIFE_HIT: u8 = 42;
pub const HOSTAGE_BARK1: u8 = 43;
pub const HOSTAGE_BARK2: u8 = 44;
pub const RADIO_BOMB_PLANTED: u8 = 45;
pub const RADIO_BOMB_DEFUSED: u8 = 46;
pub const RADIO_CT_WIN: u8 = 47;
pub const RADIO_T_WIN: u8 = 48;
pub const RADIO_ROUND_DRAW: u8 = 49;
pub const RADIO_GO: u8 = 50;
pub const RADIO_HOSTAGE_RESCUED: u8 = 51;
pub const RADIO_HOSTAGE_DOWN: u8 = 52;
pub const C4_PLANT: u8 = 53;
pub const C4_BEEP: u8 = 54;
pub const C4_DISARM: u8 = 55;
pub const C4_DISARMED: u8 = 56;
pub const C4_EXPLODE: u8 = 57;
pub const KEVLAR: u8 = 58;
pub const HEADSHOT: u8 = 59;
pub const KEVLAR_HIT: u8 = 60;
pub const HELMET_HIT: u8 = 61;
pub const PLAYER_DIE: u8 = 62;
pub const FALL_PAIN: u8 = 63;
// One fire sample per weapon that used to borrow the MP5's or the Glock's.
pub const FIRE_AK47: u8 = 64;
pub const FIRE_M4A1: u8 = 65;
pub const FIRE_FAMAS: u8 = 66;
pub const FIRE_GALIL: u8 = 67;
pub const FIRE_AUG: u8 = 68;
pub const FIRE_SG552: u8 = 69;
pub const FIRE_TMP: u8 = 70;
pub const FIRE_MAC10: u8 = 71;
pub const FIRE_UMP45: u8 = 72;
pub const FIRE_P90: u8 = 73;
pub const FIRE_USP: u8 = 74;
pub const FIRE_FIVESEVEN: u8 = 75;
pub const FIRE_ELITE: u8 = 76;
pub const FIRE_P228: u8 = 77;
pub const FIRE_G3SG1: u8 = 78;
pub const FIRE_SG550: u8 = 79;
pub const SMOKE_EXPLODE: u8 = 80;
pub const MELEE_ATTACK: u8 = KNIFE_SLASH; // actor melee swing shares the knife slash
pub const MENU_ACCEPT: u8 = BUTTON;
// Dialogue is not a global SFX id anymore -- it streams per-map (chunk
// 3100+idx) and plays via play_voice / play_voice_world (local ids).

const MAX_SFX: usize = 81;
