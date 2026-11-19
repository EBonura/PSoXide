//! SFX conveniences for engine applications.
//!
//! The machinery lives in `psx-sfx`, one layer down in the SDK, so the half of
//! the disc that is not an engine application can reach it too. This is the
//! engine's shape on top of it: one voice per sound, set up once at boot, and
//! [`play`] to fire it.
//!
//! Reach past this for anything sharing voices between sounds or needing a
//! one-shot stopped on a clock. [`psx_sfx::Player`] does both.

use psx_spu::{Spu, SpuAddr, TransferTimeout, Voice, Volume};

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

/// Upload a cooked `.psau` one-shot sample through `spu`, configure its voice,
/// and return the next free SPU RAM address.
///
/// # Errors
/// [`TransferTimeout`] when the sample did not land; its voice is left as it
/// was.
pub fn upload_sample_on(
    spu: &mut Spu,
    v: Voice,
    addr: SpuAddr,
    bytes: &[u8],
    volume: Volume,
) -> Result<SpuAddr, TransferTimeout> {
    let mut bank = Bank::new(addr);
    OneShot::new(bank.upload_on(spu, bytes)?, volume).configure(v);
    Ok(bank.next_addr())
}

/// Upload a packed bank of one-shot samples into consecutive SPU RAM through
/// `spu`.
///
/// # Errors
/// [`TransferTimeout`] at the first sample that did not land; the samples
/// after it are not sent.
pub fn upload_samples_on(
    spu: &mut Spu,
    mut addr: SpuAddr,
    samples: &[Sample<'_>],
) -> Result<SpuAddr, TransferTimeout> {
    for sample in samples {
        addr = upload_sample_on(spu, sample.voice, addr, sample.bytes, sample.volume)?;
    }
    Ok(addr)
}

/// [`upload_sample_on`] on a token the caller does not hold.
///
/// # Errors
/// As [`upload_sample_on`].
#[deprecated(note = "use `upload_sample_on` with the `Spu` driver")]
#[allow(deprecated)]
pub fn upload_sample(
    v: Voice,
    addr: SpuAddr,
    bytes: &[u8],
    volume: Volume,
) -> Result<SpuAddr, TransferTimeout> {
    let mut bank = Bank::new(addr);
    OneShot::new(bank.upload(bytes)?, volume).configure(v);
    Ok(bank.next_addr())
}

/// [`upload_samples_on`] on a token the caller does not hold.
#[deprecated(note = "use `upload_samples_on` with the `Spu` driver")]
#[allow(deprecated)]
pub fn upload_samples(
    mut addr: SpuAddr,
    samples: &[Sample<'_>],
) -> Result<SpuAddr, TransferTimeout> {
    for sample in samples {
        addr = upload_sample(sample.voice, addr, sample.bytes, sample.volume)?;
    }
    Ok(addr)
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
    Voice::start(v.mask());
}
