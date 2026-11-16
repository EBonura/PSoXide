//! Host-side STR movie encoder.
//!
//! Turns decoded pictures ([`y4m`]) and sound (a WAV) into the stream the
//! SDK's player ([`psx_fmv::player`]) plays: bitstream version 2 video
//! frames and XA-ADPCM sound interleaved in 2336-byte sector records for
//! `mkisopsx --xa-file`.
//!
//! * [`picture`]: the MDEC's colour space and scaling to the movie size.
//! * [`frame`]: forward DCT, quantisation and the bitstream of one frame.
//! * [`model`]: the MDEC's inverse, used by the encoder and to measure.
//! * [`sector`]: sector records and chunk headers.
//! * [`stream`]: the schedule of video and sound sectors, rate control and
//!   the whole encode.
//! * [`inspect`]: reading a stream back (this encoder's or another's).
//!
//! Decoding the source container is not this crate's job: the movie comes
//! in as a Y4M stream and a WAV, which any decoder can write.

pub mod cli;
pub mod frame;
pub mod inspect;
pub mod model;
pub mod picture;
pub mod sector;
pub mod stream;
pub mod y4m;
