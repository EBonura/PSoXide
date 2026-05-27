//! SFX conveniences on top of `psx-spu`.

use psx_asset::Audio;
use psx_spu::{self as spu, Adsr, Pitch, SpuAddr, Voice, Volume};

/// One cooked `.psau` sample mapped to a voice.
pub struct Sample<'a> {
    /// Voice used when this sample is played.
    pub voice: Voice,
    /// Cooked `.psau` bytes, usually from `include_bytes!`.
    pub bytes: &'a [u8],
    /// Per-voice playback volume.
    pub volume: Volume,
}

/// Configure a voice for a pre-uploaded tone sample: half-volume,
/// explicit pitch, and a percussive ADSR. The voice stays silent
/// until [`play`] keys it on.
pub fn configure_voice(v: Voice, addr: SpuAddr, pitch: Pitch) {
    v.set_volume(Volume::HALF, Volume::HALF);
    v.set_pitch(pitch);
    v.set_start_addr(addr);
    v.set_adsr(Adsr::percussive());
}

/// Upload a cooked `.psau` one-shot sample, configure its voice, and
/// return the next free SPU RAM address.
pub fn upload_sample(v: Voice, addr: SpuAddr, bytes: &[u8], volume: Volume) -> SpuAddr {
    let audio = Audio::from_bytes(bytes).expect("psau sample");
    let adpcm = audio.adpcm_bytes();
    spu::upload_adpcm(addr, adpcm);
    v.configure_sample(addr, audio.sample_rate_hz(), volume, Adsr::sample());
    SpuAddr::new(addr.byte_offset() + adpcm.len() as u32)
}

/// Upload a packed bank of one-shot samples into consecutive SPU RAM.
pub fn upload_samples(mut addr: SpuAddr, samples: &[Sample<'_>]) -> SpuAddr {
    for sample in samples {
        addr = upload_sample(sample.voice, addr, sample.bytes, sample.volume);
    }
    addr
}

/// Fire a pre-configured SFX voice -- re-attacks the ADSR envelope
/// so repeated calls replay the sample's attack transient rather
/// than letting the decay tail dominate.
#[inline]
pub fn play(v: Voice) {
    Voice::key_on(v.mask());
}
