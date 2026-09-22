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
    map_loop_owner: [u16; MAP_LOOP_VOICE_COUNT],
    next_map_loop: usize,
    ear: [i32; 3],
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
            map_loop_owner: [MAP_LOOP_OWNER_NONE; MAP_LOOP_VOICE_COUNT],
            next_map_loop: 0,
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
            self.map_loop_owner[index] = MAP_LOOP_OWNER_NONE;
            index += 1;
        }
        self.next_map_loop = 0;
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
        let mut loop_index = 0usize;
        while loop_index < MAP_LOOP_VOICE_COUNT {
            self.map_loop_owner[loop_index] = MAP_LOOP_OWNER_NONE;
            loop_index += 1;
        }
        self.next_map_loop = 0;
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
        let distance = integer_distance(pos, self.ear);
        let den = if distance >= 2400 {
            16
        } else {
            1 + distance / 300
        } as u16;
        self.play_map_loop_with_volume(local_id, owner, Volume::linear(1, den));
    }

    /// # Safety
    /// Serialize calls with all users of this bank and its SPU voice channels.
    #[inline(never)]
    pub unsafe fn play_map_loop_authored(
        &mut self,
        local_id: u8,
        pos: [i32; 3],
        owner: u16,
        volume_percent: u8,
        packed_attenuation: u8,
    ) {
        let gain = self.authored_volume(volume_percent, packed_attenuation, pos);
        self.play_map_loop_with_volume(local_id, owner, gain);
    }

    #[inline(never)]
    unsafe fn play_map_loop_with_volume(&mut self, local_id: u8, owner: u16, gain: Volume) {
        let Some((addr, rate)) = self.map_sample(local_id) else {
            return;
        };
        let mut index = 0usize;
        while index < MAP_LOOP_VOICE_COUNT {
            if self.map_loop_owner[index] == owner {
                return;
            }
            index += 1;
        }
        let slot = self.next_map_loop;
        self.next_map_loop = (self.next_map_loop + 1) % MAP_LOOP_VOICE_COUNT;
        let voice = Voice::new(MAP_LOOP_VOICE_FIRST + slot as u8);
        voice.set_volume(Volume::SILENCE, Volume::SILENCE);
        Voice::key_off(voice.mask());
        self.map_loop_owner[slot] = owner;

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
        let mut index = 0usize;
        while index < MAP_LOOP_VOICE_COUNT {
            if self.map_loop_owner[index] == owner {
                let voice = Voice::new(MAP_LOOP_VOICE_FIRST + index as u8);
                voice.set_volume(Volume::SILENCE, Volume::SILENCE);
                Voice::key_off(voice.mask());
                self.map_loop_owner[index] = MAP_LOOP_OWNER_NONE;
            }
            index += 1;
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
}
