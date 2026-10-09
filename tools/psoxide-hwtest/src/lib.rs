//! Host tools for the hardware-test disc. One library, one binary with a
//! subcommand per job; the library is what the cross-checks against the guest
//! source call.

#[macro_use]
pub mod util;
pub mod audio_chain;
pub mod audio_decode;
pub mod audio_report;
pub mod machine_code;
pub mod report;
pub mod sb4;
pub mod tables;
