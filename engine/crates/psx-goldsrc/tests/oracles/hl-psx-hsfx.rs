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
const SPU_SAMPLE_BASE: u32 = 0x1010; // BIOS convention: 0x0000..0x1000 reserved
const VOICE_POOL: u8 = 15; // voices 0..14 one-shot SFX
const DIALOGUE_VOICE: u8 = 15; // long voice lines play on their own channel (not cut by SFX)
const CHARGER_VOICE: u8 = 16; // continuous wall health/HEV charger bed

static mut ADDRS: [u32; MAX_SFX] = [0; MAX_SFX];
static mut RATES: [u32; MAX_SFX] = [0; MAX_SFX];
static mut COUNT: usize = 0;
static mut NEXT_VOICE: u8 = 0;

// ---- per-map dialogue region (streamed on map load, above the resident core) ----
pub const VOICE_CHUNK_BASE: u32 = 3100; // WORLD.PAK chunk id = 3100 + map index
const MAX_VOICES: usize = 96; // dialogue + authored mechanical/ambient samples per map
static mut DIALOGUE_BASE: u32 = 0; // SPU addr where per-map dialogue starts (= end of core)
static mut VOICE_ADDRS: [u32; MAX_VOICES] = [0; MAX_VOICES];
static mut VOICE_RATES: [u32; MAX_VOICES] = [0; MAX_VOICES];
static mut VOICE_COUNT: usize = 0;
const MAP_LOOP_VOICE_FIRST: u8 = 17;
const MAP_LOOP_VOICE_COUNT: usize = 7; // voices 17..23, all remaining hardware channels
const MAP_LOOP_OWNER_NONE: u16 = u16::MAX;
static mut MAP_LOOP_OWNER: [u16; MAP_LOOP_VOICE_COUNT] =
    [MAP_LOOP_OWNER_NONE; MAP_LOOP_VOICE_COUNT];
static mut NEXT_MAP_LOOP: usize = 0;

/// Silence the dedicated dialogue channel before replacing its SPU-RAM
/// backing store. Key-off alone only enters the ADSR release phase: the voice
/// can keep fetching ADPCM blocks while a map load overwrites them, which on
/// real hardware turns a crossing sentence into a persistent buzz. Zeroing
/// the live voice volume first makes the handoff immediate; the next
/// [`play_voice`] call restores its authored volume and restarts the decoder.
pub unsafe fn stop_dialogue() {
    let voice = Voice::new(DIALOGUE_VOICE);
    voice.set_volume(Volume::SILENCE, Volume::SILENCE);
    Voice::key_off(voice.mask());
    VOICE_COUNT = 0;
}

/// Stop every loop whose ADPCM backing lives in the replaceable per-map bank.
/// This must happen before the next room DMA overwrites that SPU range.
pub unsafe fn stop_map_loops() {
    let mut index = 0usize;
    while index < MAP_LOOP_VOICE_COUNT {
        let voice = Voice::new(MAP_LOOP_VOICE_FIRST + index as u8);
        voice.set_volume(Volume::SILENCE, Volume::SILENCE);
        Voice::key_off(voice.mask());
        MAP_LOOP_OWNER[index] = MAP_LOOP_OWNER_NONE;
        index += 1;
    }
    NEXT_MAP_LOOP = 0;
}

/// Leave every voice owned by hl-psx inaudible when gameplay exits. This is a
/// hard menu boundary, so retaining one-shot release tails has no value and a
/// malformed/overwritten ADPCM loop must never survive into the frontend.
pub unsafe fn stop_all() {
    let mut index = 0u8;
    while index < 24 {
        Voice::new(index).set_volume(Volume::SILENCE, Volume::SILENCE);
        index += 1;
    }
    Voice::key_off((1u32 << 24) - 1);
    NEXT_VOICE = 0;
    VOICE_COUNT = 0;
    let mut loop_index = 0usize;
    while loop_index < MAP_LOOP_VOICE_COUNT {
        MAP_LOOP_OWNER[loop_index] = MAP_LOOP_OWNER_NONE;
        loop_index += 1;
    }
    NEXT_MAP_LOOP = 0;
}

fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Parse a staged HSFX pack and upload every sample to SPU RAM.
/// Returns the number of samples ready.
/// One ADPCM block of silence that loops onto itself, written after every
/// sample in the bank so a voice reading past its own END lands on nothing.
///
/// The stray-sound problem is described at `play_dialogue` below and was
/// mitigated there by moving one-shots off `Adsr::sample()`. That helped but
/// could not fix it: `default_tone` still releases over ~100 ms, and the voice
/// keeps reading forward the whole time -- straight into whichever sample the
/// bank packed next. On 2026-08-04 a player heard a grenade explosion after a
/// crowbar hit, which is not in Half-Life's material table at all.
///
/// Flags `0x07` = LOOP-START | REPEAT | END. LOOP-START is the bit that works:
/// the hardware latches the repeat address off this block while decoding it,
/// so it does not depend on a register written before key-on. Pointing the
/// repeat register at a shared silence block was tried in psx-spu and the
/// launcher capture showed voices running straight past it.

pub unsafe fn init_from_pack(pack: &[u8]) -> usize {
    spu::init();
    if pack.len() < 8 || &pack[0..4] != b"HSFX" {
        return 0;
    }
    let n = (rd_u32(pack, 4) as usize).min(MAX_SFX);
    let mut next_addr = SPU_SAMPLE_BASE;
    let mut ready = 0usize;
    for i in 0..n {
        let off = rd_u32(pack, 8 + i * 8) as usize;
        let len = rd_u32(pack, 12 + i * 8) as usize;
        if off + len > pack.len() {
            break;
        }
        let Ok(audio) = Audio::from_bytes(&pack[off..off + len]) else {
            break;
        };
        let bytes = audio.adpcm_bytes();
        if next_addr + bytes.len() as u32 > 512 * 1024 {
            break;
        }
        let addr = SpuAddr::new(next_addr);
        spu::upload_adpcm(addr, bytes);
        ADDRS[i] = next_addr;
        RATES[i] = audio.sample_rate_hz();
        // Park this sample before the next one starts. Sixteen-byte aligned
        // because that is one ADPCM block; the old eight-byte rounding is the
        // SPU address granularity, not the block size.
        next_addr = (next_addr + bytes.len() as u32 + 15) & !15;
        if next_addr + SAMPLE_TAIL.len() as u32 <= 512 * 1024 {
            spu::upload_adpcm(SpuAddr::new(next_addr), &SAMPLE_TAIL);
            next_addr += SAMPLE_TAIL.len() as u32;
        }
        ready = i + 1;
    }
    COUNT = ready;
    DIALOGUE_BASE = next_addr; // per-map dialogue streams in above the core
    ready
}

/// Stream a per-map dialogue pack (same HSFX layout) into the SPU region above
/// the resident core SFX, replacing the previous map's dialogue. Local ids
/// 0..count-1 index this map's lines. Returns the number of lines ready.
pub unsafe fn load_dialogue_pack(pack: &[u8]) -> usize {
    // The previous map may changelevel in the middle of a sentence. Silence
    // and stop voice 15 before DMA writes replace the region it is decoding.
    stop_dialogue();
    stop_map_loops();
    if pack.len() < 8 || &pack[0..4] != b"HSFX" || DIALOGUE_BASE == 0 {
        return 0;
    }
    let n = (rd_u32(pack, 4) as usize).min(MAX_VOICES);
    let mut next_addr = DIALOGUE_BASE;
    let mut ready = 0usize;
    for i in 0..n {
        let off = rd_u32(pack, 8 + i * 8) as usize;
        let len = rd_u32(pack, 12 + i * 8) as usize;
        if off + len > pack.len() {
            break;
        }
        let Ok(audio) = Audio::from_bytes(&pack[off..off + len]) else {
            break;
        };
        let bytes = audio.adpcm_bytes();
        if next_addr + bytes.len() as u32 > 512 * 1024 {
            break; // out of SPU RAM: drop the rest of this map's dialogue
        }
        spu::upload_adpcm(SpuAddr::new(next_addr), bytes);
        VOICE_ADDRS[i] = next_addr;
        let rate = audio.sample_rate_hz().min(u16::MAX as u32);
        let ticks = ((audio.sample_count().saturating_mul(20) + rate.saturating_sub(1)) / rate)
            .clamp(1, u16::MAX as u32);
        // Low 16 bits retain the SPU rate; high 16 bits reuse the same word for
        // the 20 Hz duration used by facial animation (zero extra RAM).
        VOICE_RATES[i] = rate | (ticks << 16);
        // Same parking block as the core bank: dialogue lines are packed
        // consecutively too, so a voice reading past one runs into the next.
        next_addr = (next_addr + bytes.len() as u32 + 15) & !15;
        if next_addr + SAMPLE_TAIL.len() as u32 <= 512 * 1024 {
            spu::upload_adpcm(SpuAddr::new(next_addr), &SAMPLE_TAIL);
            next_addr += SAMPLE_TAIL.len() as u32;
        }
        ready = i + 1;
    }
    VOICE_COUNT = ready;
    ready
}

/// Play a per-map dialogue line (local id from the streamed dialogue pack) on
/// the dedicated dialogue voice, so a passing SFX one-shot never cuts it off.
pub unsafe fn play_voice(local_id: u8, den: u16) -> u16 {
    let i = local_id as usize;
    if i >= VOICE_COUNT {
        return 0;
    }
    let packed_rate = VOICE_RATES[i];
    let v = Voice::new(DIALOGUE_VOICE);
    // default_tone for every one-shot in this file: on real hardware a
    // sample()-enveloped voice that hits END+mute enters a release too
    // slow to ever finish and loops from its repeat address -- which,
    // in a packed bank, can be ANOTHER sample: the random stray sounds.
    // Measured by PSoXide hardware-tests SB1 (console QR, 2026-08-02).
    // default_tone plays the line out fully, then ~100 ms release.
    // Deliberate loops (map loops, chargers) keep sample(): a looped
    // sample never hits the mute path and needs its held sustain.
    OneShot::new(
        Sample::resident(SpuAddr::new(VOICE_ADDRS[i]), packed_rate & 0xffff, 0),
        Volume::linear(1, den.max(1)),
    )
    .play(v);
    (packed_rate >> 16) as u16
}

#[inline]
fn bounded_square_root(d2: i32, limit: i32) -> i32 {
    if d2 < 0 {
        return limit;
    }
    let mut lo = 0;
    let mut hi = limit;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if mid * mid < d2 {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
fn integer_distance(from: [i32; 3], to: [i32; 3]) -> i32 {
    // Authored falloff reaches zero by 1,250 source units and the one legacy
    // caller caps at 2,400. Clamp beyond that before squaring, avoiding 64-bit
    // arithmetic (expensive on MIPS-I) without changing any audible result.
    let dx = (from[0] - to[0]).clamp(-2_401, 2_401);
    let dy = (from[1] - to[1]).clamp(-2_401, 2_401);
    let dz = (from[2] - to[2]).clamp(-2_401, 2_401);
    let d2 = dx * dx + dy * dy + dz * dz;
    bounded_square_root(d2, 4_159) // ceil(sqrt(3 * 2401^2))
}

/// GoldSrc channel gain in tenths of a percent. Its mixer uses
/// `gain = volume * (1 - distance * attenuation / 1000)`. The cooker stores
/// the SDK attenuation choice plus the BSP coordinate shift in one byte.
#[inline]
fn authored_gain_milli(volume_percent: u8, packed_attenuation: u8, distance: i32) -> u16 {
    let mode = packed_attenuation & 7;
    let shift = (packed_attenuation >> 3).min(20);
    let scale = 1i32.checked_shl(shift as u32).unwrap_or(i32::MAX);
    let source_distance = distance.max(0).saturating_mul(scale);
    let loss = match mode {
        0 => source_distance.saturating_mul(2),
        1 => source_distance.saturating_mul(5) / 4,
        2 => source_distance.saturating_mul(4) / 5,
        3 => 0,
        4 => source_distance.saturating_mul(3) / 10,
        _ => source_distance.saturating_mul(5) / 4,
    };
    (volume_percent.min(100) as i32 * (1_000 - loss).max(0) / 100) as u16
}

#[inline]
unsafe fn authored_volume(volume_percent: u8, packed_attenuation: u8, pos: [i32; 3]) -> Volume {
    Volume::linear(
        authored_gain_milli(
            volume_percent,
            packed_attenuation,
            integer_distance(pos, EAR),
        ),
        1_000,
    )
}

/// Play dialogue using the source entity's authored volume and attenuation.
pub unsafe fn play_voice_authored(
    local_id: u8,
    pos: [i32; 3],
    volume_percent: u8,
    packed_attenuation: u8,
) -> u16 {
    let i = local_id as usize;
    if i >= VOICE_COUNT {
        return 0;
    }
    let packed_rate = VOICE_RATES[i];
    let gain = authored_volume(volume_percent, packed_attenuation, pos);
    if gain == Volume::SILENCE {
        return 0;
    }
    let v = Voice::new(DIALOGUE_VOICE);
    OneShot::new(
        Sample::resident(SpuAddr::new(VOICE_ADDRS[i]), packed_rate & 0xffff, 0),
        gain,
    )
    .play(v);
    (packed_rate >> 16) as u16
}

/// Cooked 20 Hz sample duration for facial animation. The duration shares the
/// high half of VOICE_RATES, so querying it adds no dialogue state or RAM.
#[inline(always)]
pub unsafe fn voice_ticks(local_id: u8) -> u16 {
    let i = local_id as usize;
    if i >= VOICE_COUNT {
        0
    } else {
        (VOICE_RATES[i] >> 16) as u16
    }
}

/// Distance-attenuated dialogue line (vs the last `set_ear`). Voice carries
/// further than SFX (people speak up), so the falloff is gentler.
pub unsafe fn play_voice_world(local_id: u8, pos: [i32; 3]) -> u16 {
    // Talk monsters use ATTN_NORM (0.8); authored scripted/ambient entities
    // call play_voice_authored with their cooked parameters instead.
    play_voice_authored(local_id, pos, 100, 2)
}

#[inline]
unsafe fn map_sample(local_id: u8) -> Option<(u32, u32)> {
    let index = local_id as usize;
    if index >= VOICE_COUNT {
        None
    } else {
        Some((VOICE_ADDRS[index], VOICE_RATES[index] & 0xffff))
    }
}

/// Play a short authored sample from the current map bank through the ordinary
/// rotating one-shot pool.
pub unsafe fn play_map_vol(local_id: u8, den: u16) {
    let Some((addr, rate)) = map_sample(local_id) else {
        return;
    };
    let voice = Voice::new(NEXT_VOICE);
    NEXT_VOICE = (NEXT_VOICE + 1) % VOICE_POOL;
    OneShot::new(
        Sample::resident(SpuAddr::new(addr), rate, 0),
        Volume::linear(1, den.max(1)),
    )
    .play(voice);
}

pub unsafe fn play_map_authored(
    local_id: u8,
    pos: [i32; 3],
    volume_percent: u8,
    packed_attenuation: u8,
) {
    let Some((addr, rate)) = map_sample(local_id) else {
        return;
    };
    let gain = authored_volume(volume_percent, packed_attenuation, pos);
    if gain == Volume::SILENCE {
        return;
    }
    let voice = Voice::new(NEXT_VOICE);
    NEXT_VOICE = (NEXT_VOICE + 1) % VOICE_POOL;
    OneShot::new(Sample::resident(SpuAddr::new(addr), rate, 0), gain).play(voice);
}

#[inline]
pub unsafe fn play_map(local_id: u8) {
    play_map_vol(local_id, 1);
}

pub unsafe fn play_map_world(local_id: u8, pos: [i32; 3]) {
    let distance = integer_distance(pos, EAR);
    if distance < 1600 {
        play_map_vol(local_id, (1 + distance / 200) as u16);
    }
}

/// Start an authored map sample on an owner-addressable voice. `owner` is the
/// logic-record index, allowing an arriving door/fan/ambient entity to stop
/// exactly its own SPU channel instead of silencing unrelated machinery.
pub unsafe fn play_map_loop_world(local_id: u8, pos: [i32; 3], owner: u16) {
    let distance = integer_distance(pos, EAR);
    let den = if distance >= 2400 {
        16
    } else {
        1 + distance / 300
    } as u16;
    play_map_loop_with_volume(local_id, owner, Volume::linear(1, den));
}

pub unsafe fn play_map_loop_authored(
    local_id: u8,
    pos: [i32; 3],
    owner: u16,
    volume_percent: u8,
    packed_attenuation: u8,
) {
    let gain = authored_volume(volume_percent, packed_attenuation, pos);
    play_map_loop_with_volume(local_id, owner, gain);
}

unsafe fn play_map_loop_with_volume(local_id: u8, owner: u16, gain: Volume) {
    let Some((addr, rate)) = map_sample(local_id) else {
        return;
    };
    let mut index = 0usize;
    while index < MAP_LOOP_VOICE_COUNT {
        if MAP_LOOP_OWNER[index] == owner {
            return;
        }
        index += 1;
    }
    let slot = NEXT_MAP_LOOP;
    NEXT_MAP_LOOP = (NEXT_MAP_LOOP + 1) % MAP_LOOP_VOICE_COUNT;
    let voice = Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8);
    voice.set_volume(Volume::SILENCE, Volume::SILENCE);
    Voice::key_off(voice.mask());
    MAP_LOOP_OWNER[slot] = owner;

    // A genuine WAV loop still sustains forever because its ADPCM END block
    // carries REPEAT and this envelope holds at full level. Some GoldSrc
    // ambient entities retain owner/toggle semantics around a finite WAV,
    // though. On real SPU hardware Adsr::sample() makes such a sound loop from
    // the repeat address indefinitely after END; default_tone releases it in
    // ~100 ms while leaving genuine ADPCM loops unchanged until key-off.
    OneShot::new(Sample::resident(SpuAddr::new(addr), rate, 0), gain).play(voice);
}

pub unsafe fn stop_map_loop(owner: u16) {
    let mut index = 0usize;
    while index < MAP_LOOP_VOICE_COUNT {
        if MAP_LOOP_OWNER[index] == owner {
            let voice = Voice::new(MAP_LOOP_VOICE_FIRST + index as u8);
            voice.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(voice.mask());
            MAP_LOOP_OWNER[index] = MAP_LOOP_OWNER_NONE;
        }
        index += 1;
    }
}

/// Fire-and-forget one-shot. `den` is the inverse volume (1 = full, bigger =
/// quieter); ids come from the consts above.
pub unsafe fn play_vol(id: u8, den: u16) {
    let i = id as usize;
    if i >= COUNT {
        return;
    }
    let dedicated = id == CHARGER_HEALTH_LOOP || id == CHARGER_HEV_LOOP;
    let v = Voice::new(if dedicated {
        CHARGER_VOICE
    } else {
        let voice = NEXT_VOICE;
        NEXT_VOICE = (NEXT_VOICE + 1) % VOICE_POOL;
        voice
    });
    v.configure_sample(
        SpuAddr::new(ADDRS[i]),
        RATES[i],
        Volume::linear(1, den.max(1)),
        // Charger hums are loops and keep the held sustain; anything on
        // the rotating pool is a one-shot and must be able to end.
        if dedicated {
            Adsr::sample()
        } else {
            Adsr::default_tone()
        },
    );
    Voice::key_on(v.mask());
}

/// Full-volume one-shot (player-local sounds: own weapon, pain, pickups).
pub unsafe fn play(id: u8) {
    play_vol(id, 1);
}

/// End the wall-charger bed immediately when +use is released, the charger is
/// depleted/full, or the player moves their use trace away from it.
#[inline(never)]
pub unsafe fn charger_stop() {
    let voice = Voice::new(CHARGER_VOICE);
    voice.set_volume(Volume::SILENCE, Volume::SILENCE);
    Voice::key_off(voice.mask());
}

static mut EAR: [i32; 3] = [0; 3];

/// Update the listener position (player) once per frame.
pub unsafe fn set_ear(pos: [i32; 3]) {
    EAR = pos;
}

/// World-positioned one-shot, attenuated by distance to the last `set_ear`.
pub unsafe fn play_world(id: u8, pos: [i32; 3]) {
    play_at_distance(id, integer_distance(pos, EAR));
}

/// World-positioned one-shot: volume falls off with distance, silent past
/// ~1600 units. `dist2` is squared world distance (dist2_xz-style i32).
pub unsafe fn play_at(id: u8, dist2: i32) {
    // den = 1 + dist/200 (integer): full <200u, 1/2 at 400u, 1/8 past 1400u.
    play_at_distance(id, bounded_square_root(dist2, 1600));
}

unsafe fn play_at_distance(id: u8, distance: i32) {
    if distance >= 1600 {
        return;
    }
    let i = id as usize;
    if i >= COUNT {
        return;
    }
    let voice = Voice::new({
        let next = NEXT_VOICE;
        NEXT_VOICE = (NEXT_VOICE + 1) % VOICE_POOL;
        next
    });
    OneShot::new(
        Sample::resident(SpuAddr::new(ADDRS[i]), RATES[i], 0),
        Volume::linear(1, (1 + distance / 200) as u16),
    )
    .play(voice);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_gain_reproduces_goldsrc_linear_falloff() {
        assert_eq!(authored_gain_milli(40, 0, 0), 400);
        assert_eq!(authored_gain_milli(100, 0, 250), 500);
        assert_eq!(authored_gain_milli(100, 0, 500), 0);
        assert_eq!(authored_gain_milli(90, 3, 20_000), 900);
        assert_eq!(authored_gain_milli(100, 4, 1_000), 700);
    }

    #[test]
    fn cooked_coordinate_shift_preserves_source_distance() {
        // 250 cooked units at scale 2 are 500 source units: ATTN_NORM (0.8)
        // therefore retains 60%, exactly like 500 units in an unscaled map.
        assert_eq!(authored_gain_milli(100, 2 | (1 << 3), 250), 600);
        assert_eq!(authored_gain_milli(100, 2, 500), 600);
    }

    #[test]
    fn shared_distance_path_preserves_generic_audio_cutoff() {
        assert_eq!(integer_distance([0, 0, 0], [300, 400, 0]), 500);
        assert_eq!(bounded_square_root(1_599 * 1_599, 1_600), 1_599);
        assert_eq!(bounded_square_root(1_600 * 1_600, 1_600), 1_600);
        assert_eq!(bounded_square_root(-1, 1_600), 1_600);
    }
}
