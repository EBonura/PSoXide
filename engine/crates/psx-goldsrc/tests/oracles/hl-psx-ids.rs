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
pub const CHUNK_ID_LIGHT: u32 = 3050;
pub const CHUNK_ID_TRAINING_WEAPONS: u32 = 3051;

// ---- sample ids (host/hl-content `SOUNDS` order) ----
pub const GLOCK: u8 = 0;
pub const MP5: u8 = 1;
pub const SHOTGUN: u8 = 2;
pub const PYTHON: u8 = 3;
pub const XBOW: u8 = 4;
pub const GAUSS: u8 = 5;
pub const RPG: u8 = 6;
pub const CBAR_MISS: u8 = 7;
pub const CBAR_HIT: u8 = 8;
pub const EXPLODE: u8 = 9;
pub const RIC: u8 = 10;
pub const ELECTRO: u8 = 11;
pub const PAIN: u8 = 12;
pub const BODYDROP: u8 = 13;
pub const DOOR_MOVE: u8 = 14;
pub const DOOR_STOP: u8 = 15;
pub const BUTTON: u8 = 16;
pub const PICKUP: u8 = 17;
pub const SUIT: u8 = 18;
pub const HC_ATTACK: u8 = 19;
pub const ZO_ATTACK: u8 = 20;
pub const HE_BLAST: u8 = 21;
pub const GLASS_BREAK: u8 = 22;
pub const WOOD_BREAK: u8 = 23;
pub const MEDSHOT: u8 = 24;
pub const STEP1: u8 = 25;
pub const STEP2: u8 = 26;
pub const RELOAD: u8 = 27;
pub const DRY: u8 = 28;
pub const ZO_PAIN: u8 = 29;
pub const HC_PAIN: u8 = 30;
pub const HC_DIE: u8 = 31;
pub const GR_PAIN: u8 = 32;
pub const GR_DIE: u8 = 33;
pub const BA_PAIN: u8 = 34;
pub const BA_DIE: u8 = 35;
pub const HE_PAIN: u8 = 36;
pub const HE_DIE: u8 = 37;
pub const SLV_PAIN: u8 = 38;
pub const SLV_DIE: u8 = 39;
pub const BC_PAIN: u8 = 40;
pub const BC_DIE: u8 = 41;
pub const HEV_BELL: u8 = 42;
pub const GEIGER: u8 = 43; // radiation/toxic-zone click
pub const HEV_ACTIVATE: u8 = 44; // suit power-on voice (pickup)
pub const HEV_HEALTH_CRIT: u8 = 45; // "health critical"
pub const HEV_NEAR_DEATH: u8 = 46; // "near death"
pub const CHARGER_HEALTH_LOOP: u8 = 47;
pub const CHARGER_HEV_LOOP: u8 = 48;
pub const FLASHLIGHT: u8 = 49;
pub const M203: u8 = 50;
pub const SHOTGUN_DOUBLE: u8 = 51;
pub const GAUSS_CHARGE: u8 = 52;
pub const AMMO_PICKUP: u8 = 53;
pub const HEALTHKIT: u8 = 54;
pub const HEALTH_DENY: u8 = 55;
pub const SUIT_DENY: u8 = 56;
pub const RELOAD_357: u8 = 57;
pub const RELOAD_XBOW: u8 = 58;
pub const MP5_CLIP_RELEASE: u8 = 59;
pub const MP5_CLIP_INSERT: u8 = 60;
pub const RELOAD_GLOCK: u8 = 61;
pub const RELOAD_SHOTGUN_ALT: u8 = 62;
pub const SHOTGUN_PUMP: u8 = 63;
pub const BARNEY_ATTACK: u8 = 64;
pub const MENU_MOVE: u8 = 65;
pub const BULLET_HIT1: u8 = 66; // flesh bullet impact (TEXTURETYPE CHAR_TEX_FLESH)
pub const BULLET_HIT2: u8 = 67;
pub const WOOD_IMPACT: u8 = 68; // debris/wood1: crowbar against CHAR_TEX_WOOD
pub const CBAR_HITBOD: u8 = 69; // crowbar landing on flesh (never the world dong)
pub const MENU_ACCEPT: u8 = BUTTON;
// Dialogue is not a global SFX id anymore -- it streams per-map (chunk
// 3100+idx) and plays via play_voice / play_voice_world (local ids).

const MAX_SFX: usize = 70;
