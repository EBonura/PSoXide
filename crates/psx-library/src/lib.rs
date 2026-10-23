//! Installing a built PS1 disc into the PSoXide games library.
//!
//! A library disc is named `Game Name (repo)`: `<name>.bin`, `<name>.cue` (its `FILE` line
//! quotes the bin, because the name has spaces and parentheses) and sidecar files beside them,
//! always including `<name>.provenance.json`. Every game's build tool installs through this
//! crate so they all behave the same way:
//!
//! - each file is written under a temporary name beside the live one, hashed, and only then
//!   renamed over the live name, so an interrupted or failed install leaves the previous disc
//!   untouched;
//! - the cue is renamed last, because it is what an emulator opens, so it never appears before
//!   its bin and sidecars;
//! - a stale pair left under an old flat name (`<stem>.bin`, `<stem>.cue`) is removed after the
//!   install, but only when that cannot lose anything: the old bin must be byte-identical to, or
//!   older than, the new one, and an old sidecar is removed only when this install replaces it.
//!   Otherwise the install is refused before anything is written;
//! - the library directory is always an explicit argument; there is no default location.
//!
//! The provenance record states which source commit the disc came from, the digests of the bin
//! and cue as installed, when, and the digest of the bin it replaced.
//!
//! ```no_run
//! use psx_library::{install, Request, Source};
//! use std::path::Path;
//!
//! let source = Source::from_git(Path::new("."), "EBonura/quake-psx")?;
//! let request = Request::new(
//!     Path::new("dist/quake-psx.cue"),
//!     Path::new("/tmp/library"),
//!     "Quake (quake-psx)",
//!     source,
//! )
//! .with_legacy_stem("quake-psx");
//! let installed = install(&request)?;
//! println!("installed {}", installed.cue.display());
//! # Ok::<(), psx_library::Error>(())
//! ```

mod cue;
mod error;
mod install;
mod provenance;
mod publish;

pub use error::Error;
pub use install::{install, Installed, Request, Sidecar};
pub use provenance::{sha256_bytes, sha256_file, Source};

/// The sidecar suffix of the provenance record every install writes.
pub const PROVENANCE_SUFFIX: &str = ".provenance.json";
