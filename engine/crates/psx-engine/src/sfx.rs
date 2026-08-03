//! SFX conveniences for engine applications.
//!
//! The machinery lives in `psx-sfx`, one layer down in the SDK, so the half of
//! the disc that is not an engine application can reach it too. This is the
//! engine's shape on top of it: one voice per sound, set up once at boot, and
//! [`play`] to fire it.
//!
//! Reach past this for anything sharing voices between sounds or needing a
//! one-shot stopped on a clock. [`psx_sfx::Player`] does both.

use psx_spu::{SpuAddr, Voice, Volume};

pub use psx_sfx::{Bank, OneShot, Player};

/// One cooked `.psau` sample mapped to a voice.
pub struct Sample<'a> {
    /// Voice used when this sample is played.
    pub voice: Voice,
    /// Cooked `.psau` bytes, usually from `include_bytes!`.
    pub bytes: &'a [u8],
    /// Per-voice playback volume.
    pub volume: Volume,
}

/// Upload a cooked `.psau` one-shot sample, configure its voice, and
/// return the next free SPU RAM address.
pub fn upload_sample(v: Voice, addr: SpuAddr, bytes: &[u8], volume: Volume) -> SpuAddr {
    let mut bank = Bank::new(addr);
    OneShot::new(bank.upload(bytes), volume).configure(v);
    bank.next_addr()
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
///
/// This does not restore the voice's volume, so it must not be paired with a
/// cutoff that silences by writing volume 0. Engine applications let the
/// envelope finish the sound instead; [`psx_sfx::Player`] is the path that
/// does both properly.
#[inline]
pub fn play(v: Voice) {
    Voice::key_on(v.mask());
}
