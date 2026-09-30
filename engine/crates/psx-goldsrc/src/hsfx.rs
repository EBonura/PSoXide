//! GoldSrc resident sound, dialogue and owner-addressed loop playback.
//!
//! One state belongs to one game. Calls must be serialized with its map loader;
//! replacing a bank stops its dependent voices before uploading over SPU RAM.
//! Authored IDs and resident capacity remain caller configuration. Directory
//! truncation retains the legacy bounds panic; valid directories with a bad
//! payload retain the valid prefix rather than rejecting the whole bank.
use psx_asset::Audio;
use psx_sfx::{OneShot, Sample, PARKING_TAIL as SAMPLE_TAIL};
use psx_spu::{self as spu, Adsr, SpuAddr, Voice, Volume};

const SPU_SAMPLE_BASE: u32 = 0x1010;
const VOICE_POOL: u8 = 15;
const DIALOGUE_VOICE: u8 = 15;
const CHARGER_VOICE: u8 = 16;
const MAX_VOICES: usize = 96;
const MAP_LOOP_VOICE_FIRST: u8 = 17;
const MAP_LOOP_VOICE_COUNT: usize = 7;
const MAP_LOOP_OWNER_NONE: u16 = u16::MAX;
/// Authored loops remembered per map. Half-Life's busiest map (c1a4f) places
/// 31 looping ambient_generics; a map with more keys the extras once, at
/// their level when they start, and never changes it.
const AUTHORED_LOOP_CAPACITY: usize = 32;
/// [`Hsfx::update_map_loops`] calls between two re-levels. At a 20 Hz
/// simulation that is every 100 ms; walking speed then moves a loop's level
/// by a few percent per step.
const AUTHORED_LOOP_PERIOD: u8 = 2;
/// GoldSrc's mixer skips a looped channel while both sides are below 8/255
/// of full scale. An authored loop without a voice is keyed only from this
/// gain (thousandths) up.
const AUDIBLE_GAIN_MILLI: u16 = 32;

/// Caller-owned bank/voice state; no allocation and no additional SPU owner.
/// `HEALTH` and `SUIT` select the two resident charger samples.
pub struct Hsfx<const N: usize, const HEALTH: u8, const SUIT: u8> {
    addrs: [u32; N],
    rates: [u32; N],
    count: usize,
    next_voice: u8,
    dialogue_base: u32,
    voice_addrs: [u32; MAX_VOICES],
    voice_rates: [u32; MAX_VOICES],
    voice_count: usize,
    map_loops: MapLoops,
    /// Bit `i` set: map sample `i` loops in hardware (its last ADPCM block
    /// repeats).
    map_sample_loops: [u32; MAX_VOICES / 32],
    authored: AuthoredLoops,
    ear: [i32; 3],
}

/// Which owner holds each map-loop voice, the level last written to it, and
/// when it was keyed. Bookkeeping only (no SPU access), so the allocation order
/// is host-testable.
///
/// A new loop takes a free voice when there is one (lowest index first).
/// Only when all seven are held does it evict one: the quietest, as last set
/// by a key or a level change, and the one keyed longest ago among equally
/// quiet voices. A loop muted in place is therefore the first to go, and the
/// nearest (loudest) loops are the last. The new loop always starts; GoldSrc
/// has no priority to compare it by.
#[derive(Clone, Copy)]
struct MapLoops {
    owner: [u16; MAP_LOOP_VOICE_COUNT],
    level: [i16; MAP_LOOP_VOICE_COUNT],
    keyed_at: [u16; MAP_LOOP_VOICE_COUNT],
    key_count: u16,
    /// Bit per slot: the voice plays a finite authored sample, so its owner
    /// gives it up once the SPU reports the sample's end.
    finite: u8,
}

impl MapLoops {
    const fn new() -> Self {
        Self {
            owner: [MAP_LOOP_OWNER_NONE; MAP_LOOP_VOICE_COUNT],
            level: [0; MAP_LOOP_VOICE_COUNT],
            keyed_at: [0; MAP_LOOP_VOICE_COUNT],
            key_count: 0,
            finite: 0,
        }
    }

    fn slot_of(&self, owner: u16) -> Option<usize> {
        let mut slot = 0usize;
        while slot < MAP_LOOP_VOICE_COUNT {
            if self.owner[slot] == owner {
                return Some(slot);
            }
            slot += 1;
        }
        None
    }

    /// Assign `owner` a slot keyed at `level`: a free one, else the eviction
    /// order above. The caller keys the voice. Keys are rare, so size wins
    /// over speed here.
    #[inline(never)]
    #[optimize(size)]
    fn claim(&mut self, owner: u16, level: Volume) -> usize {
        let slot = self.candidate();
        self.assign(slot, owner, level);
        slot
    }

    /// [`Self::claim`] for a loop that may wait for a voice: it takes a free
    /// one, or evicts the quietest only when `level` is louder than that
    /// voice by more than a quarter. The margin keeps two loops of nearly
    /// equal level from trading one voice (and restarting) on every re-level.
    #[inline(never)]
    #[optimize(size)]
    fn claim_if_louder(&mut self, owner: u16, level: Volume) -> Option<usize> {
        let slot = self.candidate();
        if self.owner[slot] != MAP_LOOP_OWNER_NONE {
            let held = self.level[slot].unsigned_abs() as u32;
            if level.0.unsigned_abs() as u32 <= held + held / 4 {
                return None;
            }
        }
        self.assign(slot, owner, level);
        Some(slot)
    }

    /// The slot a new loop takes: a free voice, else the eviction order above.
    #[inline(never)]
    #[optimize(size)]
    fn candidate(&self) -> usize {
        let mut best = 0usize;
        let mut best_rank = u32::MAX;
        let mut slot = 0usize;
        while slot < MAP_LOOP_VOICE_COUNT {
            // Lowest rank goes: a free voice (0), else by level magnitude,
            // then by age in keys. The u16 age is exact across the counter's
            // wrap for any voice keyed within the last 65,535 keys.
            let rank = if self.owner[slot] == MAP_LOOP_OWNER_NONE {
                0
            } else {
                let age = self.key_count.wrapping_sub(self.keyed_at[slot]);
                ((self.level[slot].unsigned_abs() as u32 + 1) << 16) | !age as u32
            };
            if rank < best_rank {
                best = slot;
                best_rank = rank;
            }
            slot += 1;
        }
        best
    }

    fn assign(&mut self, slot: usize, owner: u16, level: Volume) {
        self.owner[slot] = owner;
        self.level[slot] = level.0;
        self.keyed_at[slot] = self.key_count;
        self.key_count = self.key_count.wrapping_add(1);
        self.finite &= !(1 << slot);
    }

    /// Free every finite slot whose bit is set in `ended` (voice bits from
    /// the first loop voice up).
    fn release_ended(&mut self, ended: u32) {
        let done = self.finite & ended as u8;
        let mut slot = 0usize;
        while slot < MAP_LOOP_VOICE_COUNT {
            if done & (1 << slot) != 0 {
                self.release(slot);
            }
            slot += 1;
        }
    }

    fn release(&mut self, slot: usize) {
        self.owner[slot] = MAP_LOOP_OWNER_NONE;
        self.finite &= !(1 << slot);
    }

    /// Free every voice. Levels and ages of free voices are never read.
    fn clear(&mut self) {
        self.owner = [MAP_LOOP_OWNER_NONE; MAP_LOOP_VOICE_COUNT];
    }
}

/// One authored looping sound (an ambient_generic) and its cooked parameters.
#[derive(Clone, Copy)]
struct AuthoredLoop {
    pos: [i32; 3],
    owner: u16,
    local_id: u8,
    volume_percent: u8,
    packed_attenuation: u8,
}

/// Every authored loop the map has started and not stopped, whether or not it
/// holds a voice. GoldSrc keeps each ambient on its own channel and
/// re-spatializes all of them every frame; one out of range is only muted.
/// With seven loop voices this port instead re-levels the remembered loops
/// periodically: a held loop changes level in place, and one without a voice
/// is keyed once it becomes audible and a voice is free or quieter
/// ([`MapLoops::claim_if_louder`]). A loop that falls out of range keeps its
/// voice at level 0, so walking back restarts nothing, until a louder loop
/// needs that voice.
#[derive(Clone, Copy)]
struct AuthoredLoops {
    entry: [AuthoredLoop; AUTHORED_LOOP_CAPACITY],
    count: u8,
    phase: u8,
}

impl AuthoredLoops {
    const fn new() -> Self {
        Self {
            entry: [AuthoredLoop {
                pos: [0; 3],
                owner: MAP_LOOP_OWNER_NONE,
                local_id: 0,
                volume_percent: 0,
                packed_attenuation: 0,
            }; AUTHORED_LOOP_CAPACITY],
            count: 0,
            phase: AUTHORED_LOOP_PERIOD - 1,
        }
    }

    #[inline(never)]
    #[optimize(size)]
    fn index_of(&self, owner: u16) -> Option<usize> {
        let mut index = 0usize;
        while index < self.count as usize {
            if self.entry[index].owner == owner {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    /// Remember `entry`; false when the table is full.
    #[optimize(size)]
    fn push(&mut self, entry: AuthoredLoop) -> bool {
        if self.count as usize >= AUTHORED_LOOP_CAPACITY {
            return false;
        }
        self.entry[self.count as usize] = entry;
        self.count += 1;
        true
    }

    #[inline(never)]
    #[optimize(size)]
    fn remove(&mut self, owner: u16) {
        if let Some(index) = self.index_of(owner) {
            self.count -= 1;
            self.entry[index] = self.entry[self.count as usize];
        }
    }

    /// True on every [`AUTHORED_LOOP_PERIOD`]th call, the first included.
    fn due(&mut self) -> bool {
        self.phase += 1;
        if self.phase < AUTHORED_LOOP_PERIOD {
            return false;
        }
        self.phase = 0;
        true
    }

    fn clear(&mut self) {
        self.count = 0;
    }
}

#[inline(never)]
fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
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
fn clamped_distance_squared(from: [i32; 3], to: [i32; 3]) -> i32 {
    // Authored falloff reaches zero by 1,250 source units and the one legacy
    // caller caps at 2,400. Clamp beyond that before squaring, avoiding 64-bit
    // arithmetic (expensive on MIPS-I) without changing any audible result.
    let dx = (from[0] - to[0]).clamp(-2_401, 2_401);
    let dy = (from[1] - to[1]).clamp(-2_401, 2_401);
    let dz = (from[2] - to[2]).clamp(-2_401, 2_401);
    dx * dx + dy * dy + dz * dz
}

#[inline]
fn integer_distance(from: [i32; 3], to: [i32; 3]) -> i32 {
    bounded_square_root(clamped_distance_squared(from, to), 4_159) // ceil(sqrt(3 * 2401^2))
}

/// The cooked distance from which [`authored_gain_milli`] is exactly zero for
/// this attenuation byte, or `None` when it never is (ATTN_NONE).
#[inline]
fn silent_distance(packed_attenuation: u8) -> Option<i32> {
    let source = match packed_attenuation & 7 {
        0 => 500,   // 2 * 500 = 1000
        2 => 1_250, // 1250 * 4 / 5 = 1000
        3 => return None,
        4 => 3_334, // 3334 * 3 / 10 = 1000 (3333 leaves 1)
        _ => 800,   // 800 * 5 / 4 = 1000
    };
    let shift = (packed_attenuation >> 3).min(20);
    Some((source + (1 << shift) - 1) >> shift)
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

impl<const N: usize, const HEALTH: u8, const SUIT: u8> Hsfx<N, HEALTH, SUIT> {
    /// Empty state before the resident bank is uploaded.
    pub const fn new() -> Self {
        Self {
            addrs: [0; N],
            rates: [0; N],
            count: 0,
            next_voice: 0,
            dialogue_base: 0,
            voice_addrs: [0; MAX_VOICES],
            voice_rates: [0; MAX_VOICES],
            voice_count: 0,
            map_loops: MapLoops::new(),
            map_sample_loops: [0; MAX_VOICES / 32],
            authored: AuthoredLoops::new(),
            ear: [0; 3],
        }
    }

    /// Silence the dedicated dialogue channel before replacing its SPU-RAM
    /// backing store. Key-off alone only enters the ADSR release phase: the voice
    /// can keep fetching ADPCM blocks while a map load overwrites them, which on
    /// real hardware turns a crossing sentence into a persistent buzz. Zeroing
    /// the live voice volume first makes the handoff immediate; the next
    /// [`Self::play_voice`] call restores its authored volume and restarts the decoder.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn stop_dialogue(&mut self) {
        let voice = Voice::new(DIALOGUE_VOICE);
        voice.set_volume(Volume::SILENCE, Volume::SILENCE);
        Voice::key_off(voice.mask());
        self.voice_count = 0;
    }

    /// Stop every loop whose ADPCM backing lives in the replaceable per-map bank.
    /// This must happen before the next room DMA overwrites that SPU range.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn stop_map_loops(&mut self) {
        let mut index = 0usize;
        while index < MAP_LOOP_VOICE_COUNT {
            let voice = Voice::new(MAP_LOOP_VOICE_FIRST + index as u8);
            voice.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(voice.mask());
            index += 1;
        }
        self.map_loops.clear();
        self.authored.clear();
    }

    /// Leave every voice owned by hl-psx inaudible when gameplay exits. This is a
    /// hard menu boundary, so retaining one-shot release tails has no value and a
    /// malformed/overwritten ADPCM loop must never survive into the frontend.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn stop_all(&mut self) {
        let mut index = 0u8;
        while index < 24 {
            Voice::new(index).set_volume(Volume::SILENCE, Volume::SILENCE);
            index += 1;
        }
        Voice::key_off((1u32 << 24) - 1);
        self.next_voice = 0;
        self.voice_count = 0;
        self.map_loops.clear();
        self.authored.clear();
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
    ///
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn init_from_pack(&mut self, pack: &[u8]) -> usize {
        spu::init();
        if pack.len() < 8 || &pack[0..4] != b"HSFX" {
            return 0;
        }
        let n = (rd_u32(pack, 4) as usize).min(N);
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
            self.addrs[i] = next_addr;
            self.rates[i] = audio.sample_rate_hz();
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
        self.count = ready;
        self.dialogue_base = next_addr; // per-map dialogue streams in above the core
        ready
    }

    /// Stream a per-map dialogue pack (same HSFX layout) into the SPU region above
    /// the resident core SFX, replacing the previous map's dialogue. Local ids
    /// 0..count-1 index this map's lines. Returns the number of lines ready.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn load_dialogue_pack(&mut self, pack: &[u8]) -> usize {
        // The previous map may changelevel in the middle of a sentence. Silence
        // and stop voice 15 before DMA writes replace the region it is decoding.
        self.stop_dialogue();
        self.stop_map_loops();
        if pack.len() < 8 || &pack[0..4] != b"HSFX" || self.dialogue_base == 0 {
            return 0;
        }
        let n = (rd_u32(pack, 4) as usize).min(MAX_VOICES);
        let mut next_addr = self.dialogue_base;
        let mut ready = 0usize;
        self.map_sample_loops = [0; MAX_VOICES / 32];
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
            self.voice_addrs[i] = next_addr;
            let rate = audio.sample_rate_hz().min(u16::MAX as u32);
            let ticks = ((audio.sample_count().saturating_mul(20) + rate.saturating_sub(1)) / rate)
                .clamp(1, u16::MAX as u32);
            // Low 16 bits retain the SPU rate; high 16 bits reuse the same word for
            // the 20 Hz duration used by facial animation (zero extra RAM).
            self.voice_rates[i] = rate | (ticks << 16);
            // The cooker marks a sample whose WAV declares a loop with
            // LOOP-END + REPEAT on its last block (flag byte 1 of 16).
            if bytes.len() >= 16 && bytes[bytes.len() - 15] & 0x02 != 0 {
                self.map_sample_loops[i / 32] |= 1 << (i % 32);
            }
            // Same parking block as the core bank: dialogue lines are packed
            // consecutively too, so a voice reading past one runs into the next.
            next_addr = (next_addr + bytes.len() as u32 + 15) & !15;
            if next_addr + SAMPLE_TAIL.len() as u32 <= 512 * 1024 {
                spu::upload_adpcm(SpuAddr::new(next_addr), &SAMPLE_TAIL);
                next_addr += SAMPLE_TAIL.len() as u32;
            }
            ready = i + 1;
        }
        self.voice_count = ready;
        ready
    }

    /// Play a per-map dialogue line (local id from the streamed dialogue pack) on
    /// the dedicated dialogue voice, so a passing SFX one-shot never cuts it off.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_voice(&mut self, local_id: u8, den: u16) -> u16 {
        let i = local_id as usize;
        if i >= self.voice_count {
            return 0;
        }
        let packed_rate = self.voice_rates[i];
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
            Sample::resident(SpuAddr::new(self.voice_addrs[i]), packed_rate & 0xffff, 0),
            Volume::linear(1, den.max(1)),
        )
        .play(v);
        (packed_rate >> 16) as u16
    }

    #[inline]
    unsafe fn authored_volume(
        &mut self,
        volume_percent: u8,
        packed_attenuation: u8,
        pos: [i32; 3],
    ) -> Volume {
        Volume::linear(
            authored_gain_milli(
                volume_percent,
                packed_attenuation,
                integer_distance(pos, self.ear),
            ),
            1_000,
        )
    }

    /// Play dialogue using the source entity's authored volume and attenuation.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_voice_authored(
        &mut self,
        local_id: u8,
        pos: [i32; 3],
        volume_percent: u8,
        packed_attenuation: u8,
    ) -> u16 {
        let i = local_id as usize;
        if i >= self.voice_count {
            return 0;
        }
        let packed_rate = self.voice_rates[i];
        let gain = self.authored_volume(volume_percent, packed_attenuation, pos);
        if gain == Volume::SILENCE {
            return 0;
        }
        let v = Voice::new(DIALOGUE_VOICE);
        OneShot::new(
            Sample::resident(SpuAddr::new(self.voice_addrs[i]), packed_rate & 0xffff, 0),
            gain,
        )
        .play(v);
        (packed_rate >> 16) as u16
    }

    /// Cooked 20 Hz sample duration for facial animation. The duration shares the
    /// high half of self.voice_rates, so querying it adds no dialogue state or RAM.
    #[inline(always)]
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn voice_ticks(&mut self, local_id: u8) -> u16 {
        let i = local_id as usize;
        if i >= self.voice_count {
            0
        } else {
            (self.voice_rates[i] >> 16) as u16
        }
    }

    /// Distance-attenuated dialogue line (vs the last `set_ear`). Voice carries
    /// further than SFX (people speak up), so the falloff is gentler.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_voice_world(&mut self, local_id: u8, pos: [i32; 3]) -> u16 {
        // Talk monsters use ATTN_NORM (0.8); authored scripted/ambient entities
        // call play_voice_authored with their cooked parameters instead.
        self.play_voice_authored(local_id, pos, 100, 2)
    }

    #[inline]
    unsafe fn map_sample(&mut self, local_id: u8) -> Option<(u32, u32)> {
        let index = local_id as usize;
        if index >= self.voice_count {
            None
        } else {
            Some((self.voice_addrs[index], self.voice_rates[index] & 0xffff))
        }
    }

    /// Play a short authored sample from the current map bank through the ordinary
    /// rotating one-shot pool.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_map_vol(&mut self, local_id: u8, den: u16) {
        let Some((addr, rate)) = self.map_sample(local_id) else {
            return;
        };
        let voice = Voice::new(self.next_voice);
        self.next_voice = (self.next_voice + 1) % VOICE_POOL;
        OneShot::new(
            Sample::resident(SpuAddr::new(addr), rate, 0),
            Volume::linear(1, den.max(1)),
        )
        .play(voice);
    }

    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_map_authored(
        &mut self,
        local_id: u8,
        pos: [i32; 3],
        volume_percent: u8,
        packed_attenuation: u8,
    ) {
        let Some((addr, rate)) = self.map_sample(local_id) else {
            return;
        };
        let gain = self.authored_volume(volume_percent, packed_attenuation, pos);
        if gain == Volume::SILENCE {
            return;
        }
        let voice = Voice::new(self.next_voice);
        self.next_voice = (self.next_voice + 1) % VOICE_POOL;
        OneShot::new(Sample::resident(SpuAddr::new(addr), rate, 0), gain).play(voice);
    }

    #[inline]
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_map(&mut self, local_id: u8) {
        self.play_map_vol(local_id, 1);
    }

    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_map_world(&mut self, local_id: u8, pos: [i32; 3]) {
        let distance = integer_distance(pos, self.ear);
        if distance < 1600 {
            self.play_map_vol(local_id, (1 + distance / 200) as u16);
        }
    }

    /// Start an authored map sample on an owner-addressable voice. `owner` is the
    /// logic-record index, allowing an arriving door/fan/ambient entity to stop
    /// exactly its own SPU channel instead of silencing unrelated machinery.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_map_loop_world(&mut self, local_id: u8, pos: [i32; 3], owner: u16) {
        let gain = self.map_loop_world_gain(pos);
        self.play_map_loop_with_volume(local_id, owner, gain);
    }

    /// The map loop's own falloff: 1 / (1 + distance / 300), 1/16 from 2,400.
    #[inline]
    fn map_loop_world_gain(&self, pos: [i32; 3]) -> Volume {
        let distance = integer_distance(pos, self.ear);
        let den = if distance >= 2400 {
            16
        } else {
            1 + distance / 300
        } as u16;
        Volume::linear(1, den)
    }

    /// GoldSrc's SND_CHANGE_VOL for a map loop: set the level of the voice
    /// `owner` holds in place, without re-keying it, so the loop keeps its
    /// phase and no other loop is evicted. Hsfx voices are mono (equal left
    /// and right), so there is no pan to change. Returns false, touching no
    /// voice, when `owner` holds none (never keyed, stopped, or evicted); the
    /// caller then keys it with a `play_map_loop_*` call.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn set_map_loop_volume(&mut self, owner: u16, gain: Volume) -> bool {
        let Some(slot) = self.map_loops.slot_of(owner) else {
            return false;
        };
        self.map_loops.level[slot] = gain.0;
        Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8).set_volume(gain, gain);
        true
    }

    /// [`Self::set_map_loop_volume`] at [`Self::play_map_loop_world`]'s level
    /// for `pos` against the last `set_ear`, for a loop whose source moves.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn set_map_loop_world(&mut self, owner: u16, pos: [i32; 3]) -> bool {
        let gain = self.map_loop_world_gain(pos);
        self.set_map_loop_volume(owner, gain)
    }

    /// The SPU voice `owner`'s map loop holds, if any.
    pub fn map_loop_voice(&self, owner: u16) -> Option<u8> {
        self.map_loops
            .slot_of(owner)
            .map(|slot| MAP_LOOP_VOICE_FIRST + slot as u8)
    }

    /// Start an authored loop (an ambient_generic) with its cooked volume and
    /// attenuation. A sample that loops is remembered until
    /// [`Self::stop_map_loop`] and follows the listener through
    /// [`Self::update_map_loops`]; it holds a voice only while audible or
    /// until a louder loop needs one (see `AuthoredLoops`). A finite sample
    /// plays once, from its level now, and only when audible, and gives its
    /// voice back when it ends: GoldSrc frees such a channel once it ends or
    /// stays inaudible.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    #[optimize(size)]
    pub unsafe fn play_map_loop_authored(
        &mut self,
        local_id: u8,
        pos: [i32; 3],
        owner: u16,
        volume_percent: u8,
        packed_attenuation: u8,
    ) {
        if self.map_sample(local_id).is_none()
            || self.map_loops.slot_of(owner).is_some()
            || self.authored.index_of(owner).is_some()
        {
            return;
        }
        let entry = AuthoredLoop {
            pos,
            owner,
            local_id,
            volume_percent,
            packed_attenuation,
        };
        let index = local_id as usize;
        if self.map_sample_loops[index / 32] & (1 << (index % 32)) == 0 {
            let milli = self.authored_gain(&entry);
            if milli >= AUDIBLE_GAIN_MILLI {
                self.play_map_loop_with_volume(local_id, owner, Volume::linear(milli, 1_000));
                if let Some(slot) = self.map_loops.slot_of(owner) {
                    self.map_loops.finite |= 1 << slot;
                }
            }
        } else if self.authored.push(entry) {
            self.level_authored_loop(entry);
        } else {
            let gain = self.authored_volume(volume_percent, packed_attenuation, pos);
            self.play_map_loop_with_volume(local_id, owner, gain);
        }
    }

    /// Re-level every remembered authored loop against the last `set_ear`,
    /// keying any that became audible (see `AuthoredLoops`). Call once per
    /// simulation tick after `set_ear`; the work runs on every
    /// [`AUTHORED_LOOP_PERIOD`]th call.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    #[optimize(size)]
    pub unsafe fn update_map_loops(&mut self) {
        if !self.authored.due() {
            return;
        }
        // A finished finite sample holds its voice only in this bookkeeping,
        // at the level it was keyed at, where it would outrank every waiting
        // loop. Key-on clears a voice's ENDX bit, so a set bit is this key's
        // end.
        if self.map_loops.finite != 0 {
            self.map_loops
                .release_ended(Voice::voices_ended() >> MAP_LOOP_VOICE_FIRST);
        }
        let mut index = 0usize;
        while index < self.authored.count as usize {
            self.level_authored_loop(self.authored.entry[index]);
            index += 1;
        }
    }

    /// [`authored_gain_milli`] for `entry` at the current ear. Most of a map's
    /// loops are out of range at any moment; for those the squared distance
    /// already shows a zero gain, and the square root (most of a re-level's
    /// cost) is skipped. The result is the same either way.
    #[inline]
    fn authored_gain(&self, entry: &AuthoredLoop) -> u16 {
        if let Some(silent) = silent_distance(entry.packed_attenuation) {
            if clamped_distance_squared(entry.pos, self.ear) >= silent * silent {
                return 0;
            }
        }
        authored_gain_milli(
            entry.volume_percent,
            entry.packed_attenuation,
            integer_distance(entry.pos, self.ear),
        )
    }

    /// One authored loop at its level for the current ear: in place on the
    /// voice it holds, else keyed when audible and a voice can be had.
    #[inline(never)]
    #[optimize(size)]
    unsafe fn level_authored_loop(&mut self, entry: AuthoredLoop) {
        let milli = self.authored_gain(&entry);
        let gain = Volume::linear(milli, 1_000);
        match self.map_loops.slot_of(entry.owner) {
            Some(slot) => {
                if self.map_loops.level[slot] != gain.0 {
                    self.map_loops.level[slot] = gain.0;
                    Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8).set_volume(gain, gain);
                }
            }
            None => {
                if milli >= AUDIBLE_GAIN_MILLI {
                    if let Some(slot) = self.map_loops.claim_if_louder(entry.owner, gain) {
                        self.key_map_loop(slot, entry.local_id, gain);
                    }
                }
            }
        }
    }

    #[inline(never)]
    #[optimize(size)]
    unsafe fn play_map_loop_with_volume(&mut self, local_id: u8, owner: u16, gain: Volume) {
        if self.map_sample(local_id).is_none() || self.map_loops.slot_of(owner).is_some() {
            return;
        }
        let slot = self.map_loops.claim(owner, gain);
        self.key_map_loop(slot, local_id, gain);
    }

    /// Key map sample `local_id` on loop voice `slot` at `gain`.
    #[inline(never)]
    #[optimize(size)]
    unsafe fn key_map_loop(&mut self, slot: usize, local_id: u8, gain: Volume) {
        let Some((addr, rate)) = self.map_sample(local_id) else {
            return;
        };
        let voice = Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8);
        voice.set_volume(Volume::SILENCE, Volume::SILENCE);
        Voice::key_off(voice.mask());

        // A genuine WAV loop still sustains forever because its ADPCM END block
        // carries REPEAT and this envelope holds at full level. Some GoldSrc
        // ambient entities retain owner/toggle semantics around a finite WAV,
        // though. On real SPU hardware Adsr::sample() makes such a sound loop from
        // the repeat address indefinitely after END; default_tone releases it in
        // ~100 ms while leaving genuine ADPCM loops unchanged until key-off.
        OneShot::new(Sample::resident(SpuAddr::new(addr), rate, 0), gain).play(voice);
    }

    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn stop_map_loop(&mut self, owner: u16) {
        self.authored.remove(owner);
        if let Some(slot) = self.map_loops.slot_of(owner) {
            let voice = Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8);
            voice.set_volume(Volume::SILENCE, Volume::SILENCE);
            Voice::key_off(voice.mask());
            self.map_loops.release(slot);
        }
    }

    /// Fire-and-forget one-shot. `den` is the inverse volume (1 = full, bigger =
    /// quieter); ids come from the consts above.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_vol(&mut self, id: u8, den: u16) {
        let i = id as usize;
        if i >= self.count {
            return;
        }
        let dedicated = id == HEALTH || id == SUIT;
        let v = Voice::new(if dedicated {
            CHARGER_VOICE
        } else {
            let voice = self.next_voice;
            self.next_voice = (self.next_voice + 1) % VOICE_POOL;
            voice
        });
        v.configure_sample(
            SpuAddr::new(self.addrs[i]),
            self.rates[i],
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
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play(&mut self, id: u8) {
        self.play_vol(id, 1);
    }

    /// End the wall-charger bed immediately when +use is released, the charger is
    /// depleted/full, or the player moves their use trace away from it.
    #[inline(never)]
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn charger_stop(&mut self) {
        let voice = Voice::new(CHARGER_VOICE);
        voice.set_volume(Volume::SILENCE, Volume::SILENCE);
        Voice::key_off(voice.mask());
    }

    /// Update the listener position (player) once per frame.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn set_ear(&mut self, pos: [i32; 3]) {
        self.ear = pos;
    }

    /// World-positioned one-shot, attenuated by distance to the last `set_ear`.
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_world(&mut self, id: u8, pos: [i32; 3]) {
        self.play_at_distance(id, integer_distance(pos, self.ear));
    }

    /// World-positioned one-shot: volume falls off with distance, silent past
    /// ~1600 units. `dist2` is squared world distance (dist2_xz-style i32).
    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    pub unsafe fn play_at(&mut self, id: u8, dist2: i32) {
        // den = 1 + dist/200 (integer): full <200u, 1/2 at 400u, 1/8 past 1400u.
        self.play_at_distance(id, bounded_square_root(dist2, 1600));
    }

    unsafe fn play_at_distance(&mut self, id: u8, distance: i32) {
        if distance >= 1600 {
            return;
        }
        let i = id as usize;
        if i >= self.count {
            return;
        }
        let voice = Voice::new({
            let next = self.next_voice;
            self.next_voice = (self.next_voice + 1) % VOICE_POOL;
            next
        });
        OneShot::new(
            Sample::resident(SpuAddr::new(self.addrs[i]), self.rates[i], 0),
            Volume::linear(1, (1 + distance / 200) as u16),
        )
        .play(voice);
    }
}

impl<const N: usize, const HEALTH: u8, const SUIT: u8> Default for Hsfx<N, HEALTH, SUIT> {
    fn default() -> Self {
        Self::new()
    }
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

    fn full_table(levels: [i16; MAP_LOOP_VOICE_COUNT]) -> MapLoops {
        let mut loops = MapLoops::new();
        for (owner, level) in levels.into_iter().enumerate() {
            assert_eq!(loops.claim(owner as u16, Volume(level)), owner);
        }
        loops
    }

    #[test]
    fn map_loop_takes_a_free_voice_before_evicting() {
        // Three held loops, then the middle one stops: the next loop reuses
        // that free voice rather than the next voice in key order, and the
        // two held loops keep theirs.
        let mut loops = MapLoops::new();
        assert_eq!(loops.claim(10, Volume::MAX), 0);
        assert_eq!(loops.claim(11, Volume::MAX), 1);
        assert_eq!(loops.claim(12, Volume::MAX), 2);
        loops.release(1);
        assert_eq!(loops.claim(13, Volume::SILENCE), 1);
        assert_eq!(loops.slot_of(10), Some(0));
        assert_eq!(loops.slot_of(12), Some(2));
        assert_eq!(loops.slot_of(11), None);
        // A free voice wins over a held one, even a silent held one earlier
        // in the table.
        let mut loops = full_table([0; MAP_LOOP_VOICE_COUNT]);
        loops.release(3);
        assert_eq!(loops.claim(14, Volume::MAX), 3);
        assert_eq!(loops.slot_of(0), Some(0));
        // clear frees every voice.
        loops.clear();
        assert_eq!(loops.slot_of(14), None);
        assert_eq!(loops.claim(15, Volume::MAX), 0);
        // Seven keys fill the table; none of them evicts.
        let mut loops = MapLoops::new();
        for owner in 0..MAP_LOOP_VOICE_COUNT as u16 {
            loops.claim(owner, Volume::MAX);
        }
        for owner in 0..MAP_LOOP_VOICE_COUNT as u16 {
            assert_eq!(loops.slot_of(owner), Some(owner as usize));
        }
    }

    #[test]
    fn full_map_loop_table_evicts_the_quietest_voice() {
        let mut loops = full_table([4000, 900, 3000, 200, 3000, 16383, 700]);
        assert_eq!(loops.claim(100, Volume::MAX), 3);
        assert_eq!(loops.slot_of(3), None);
        // The newcomer's own level counts once it holds a voice.
        assert_eq!(loops.claim(101, Volume::MAX), 6);
        assert_eq!(loops.claim(102, Volume::MAX), 1);
    }

    #[test]
    fn equally_quiet_map_loops_evict_the_oldest_first() {
        let mut loops = full_table([500; MAP_LOOP_VOICE_COUNT]);
        // Re-keying slot 0 makes it the newest; slot 1 is now the oldest.
        loops.release(0);
        assert_eq!(loops.claim(20, Volume(500)), 0);
        assert_eq!(loops.claim(21, Volume(500)), 1);
        assert_eq!(loops.claim(22, Volume(500)), 2);
        // Age survives the key counter's wrap.
        let mut loops = MapLoops::new();
        loops.key_count = u16::MAX - 2;
        for owner in 0..MAP_LOOP_VOICE_COUNT as u16 {
            loops.claim(owner, Volume(500));
        }
        assert_eq!(loops.claim(30, Volume(500)), 0);
        assert_eq!(loops.claim(31, Volume(500)), 1);
    }

    #[test]
    fn a_level_change_moves_a_loop_in_the_eviction_order() {
        // Muting a loop in place (set_map_loop_volume's bookkeeping) makes it
        // the first to go; raising the old quietest one protects it.
        let mut loops = full_table([100, 2000, 2000, 2000, 2000, 2000, 2000]);
        loops.level[4] = Volume::SILENCE.0;
        loops.level[0] = Volume::MAX.0;
        assert_eq!(loops.claim(40, Volume::MAX), 4);
        assert_eq!(loops.slot_of(0), Some(0));
    }

    #[test]
    fn map_loop_level_follows_the_play_falloff() {
        let mut sfx = Hsfx::<1, 0, 0>::new();
        // SAFETY: neither call reaches the SPU: set_ear stores the listener,
        // and no owner holds a voice for set_map_loop_volume to write.
        unsafe { sfx.set_ear([0, 0, 0]) };
        assert_eq!(sfx.map_loop_world_gain([0, 0, 0]), Volume::MAX);
        assert_eq!(sfx.map_loop_world_gain([600, 0, 0]), Volume::linear(1, 3));
        assert_eq!(sfx.map_loop_world_gain([0, 2_399, 0]), Volume::linear(1, 8));
        assert_eq!(
            sfx.map_loop_world_gain([0, 0, 2_400]),
            Volume::linear(1, 16)
        );
        assert_eq!(
            sfx.map_loop_world_gain([90_000, 0, 0]),
            Volume::linear(1, 16)
        );
        // With no loop keyed, changing a level touches no voice.
        assert!(!unsafe { sfx.set_map_loop_volume(7, Volume::MAX) });
        assert_eq!(sfx.map_loop_voice(7), None);
    }

    #[test]
    fn a_waiting_loop_takes_a_voice_only_when_free_or_clearly_louder() {
        let mut loops = MapLoops::new();
        assert_eq!(loops.claim_if_louder(50, Volume(100)), Some(0));
        // Full table whose quietest voice (slot 3) is at 1,000.
        let mut loops = full_table([4000, 3000, 2000, 1000, 3000, 16383, 1500]);
        assert_eq!(loops.claim_if_louder(60, Volume(900)), None);
        assert_eq!(loops.claim_if_louder(61, Volume(1250)), None);
        assert_eq!(loops.slot_of(3), Some(3));
        assert_eq!(loops.claim_if_louder(62, Volume(1251)), Some(3));
        assert_eq!(loops.slot_of(62), Some(3));
        // A voice muted in place is taken by any audible loop.
        loops.level[5] = Volume::SILENCE.0;
        assert_eq!(loops.claim_if_louder(63, Volume(1)), Some(5));
    }

    #[test]
    fn an_ended_finite_sample_gives_its_voice_back() {
        let mut loops = full_table([16383; MAP_LOOP_VOICE_COUNT]);
        loops.finite = 0b000_0100;
        // Loops (slot 0) and a finite sample still playing (slot 2 without
        // its end bit) keep their voices.
        loops.release_ended(0b000_0011);
        assert_eq!(loops.slot_of(0), Some(0));
        assert_eq!(loops.slot_of(2), Some(2));
        loops.release_ended(0b000_0101);
        assert_eq!(loops.slot_of(0), Some(0));
        assert_eq!(loops.slot_of(2), None);
        assert_eq!(loops.finite, 0);
        // Re-keying a slot forgets that it was finite.
        loops.finite = 0b000_1000;
        loops.assign(3, 9, Volume::MAX);
        assert_eq!(loops.finite, 0);
    }

    fn authored(owner: u16) -> AuthoredLoop {
        AuthoredLoop {
            pos: [owner as i32, 0, 0],
            owner,
            local_id: owner as u8,
            volume_percent: 100,
            packed_attenuation: 1,
        }
    }

    #[test]
    fn authored_loops_are_remembered_until_stopped() {
        let mut table = AuthoredLoops::new();
        for owner in 0..AUTHORED_LOOP_CAPACITY as u16 {
            assert!(table.push(authored(owner)));
        }
        assert!(!table.push(authored(99)));
        table.remove(4);
        assert_eq!(table.index_of(4), None);
        // The last entry fills the hole; every other owner is still there.
        assert_eq!(table.index_of(AUTHORED_LOOP_CAPACITY as u16 - 1), Some(4));
        for owner in (0..AUTHORED_LOOP_CAPACITY as u16 - 1).filter(|&o| o != 4) {
            assert_eq!(table.index_of(owner), Some(owner as usize));
        }
        assert!(table.push(authored(99)));
        table.remove(1234);
        assert_eq!(table.count as usize, AUTHORED_LOOP_CAPACITY);
        table.clear();
        assert_eq!(table.index_of(0), None);
    }

    #[test]
    fn authored_loops_relevel_on_every_period_th_update() {
        let mut table = AuthoredLoops::new();
        assert!(table.due());
        let due = (0..AUTHORED_LOOP_PERIOD as usize * 3)
            .filter(|_| table.due())
            .count();
        assert_eq!(due, 3);
    }

    #[test]
    fn the_silent_distance_shortcut_matches_the_full_gain() {
        let mut sfx = Hsfx::<1, 0, 0>::new();
        // SAFETY: set_ear only stores the listener.
        unsafe { sfx.set_ear([0, 0, 0]) };
        for shift in [0u8, 1, 3] {
            for mode in 0u8..8 {
                let attenuation = mode | (shift << 3);
                for x in (0..5_000)
                    .step_by(7)
                    .chain([499, 500, 799, 800, 1_249, 1_250, 3_333, 3_334])
                {
                    for pos in [[x, 0, 0], [x, x / 3, -x / 2], [0, 0, x]] {
                        let mut entry = authored(0);
                        entry.packed_attenuation = attenuation;
                        entry.pos = pos;
                        let full =
                            authored_gain_milli(100, attenuation, integer_distance(pos, sfx.ear));
                        assert_eq!(sfx.authored_gain(&entry), full, "{attenuation} {pos:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn authored_loop_audibility_follows_the_ambient_radius() {
        let mut sfx = Hsfx::<1, 0, 0>::new();
        // SAFETY: set_ear only stores the listener.
        unsafe { sfx.set_ear([0, 0, 0]) };
        let at = |mode: u8, x: i32| {
            let mut entry = authored(0);
            entry.packed_attenuation = mode;
            entry.pos = [x, 0, 0];
            sfx.authored_gain(&entry)
        };
        // Medium radius (ATTN_STATIC 1.25) is audible to 775 units, small
        // (ATTN_IDLE 2) to 484, large (ATTN_NORM 0.8) to 1,211.
        assert!(at(1, 774) >= AUDIBLE_GAIN_MILLI);
        assert!(at(1, 776) < AUDIBLE_GAIN_MILLI);
        assert!(at(0, 484) >= AUDIBLE_GAIN_MILLI);
        assert!(at(0, 485) < AUDIBLE_GAIN_MILLI);
        assert!(at(2, 1_211) >= AUDIBLE_GAIN_MILLI);
        assert!(at(2, 1_212) < AUDIBLE_GAIN_MILLI);
        // Play everywhere (ATTN_NONE) keeps its full level at any distance.
        assert_eq!(at(3, 0), 1_000);
        assert_eq!(at(3, 90_000), 1_000);
    }
}
