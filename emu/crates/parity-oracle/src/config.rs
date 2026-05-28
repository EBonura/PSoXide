//! Harness configuration: which Redux binary, which BIOS, which Lua script.
//!
//! Binary discovery order (first match wins):
//! 1. `PSOXIDE_REDUX_BIN` environment variable
//! 2. a `pcsx-redux` checkout next to the PSoXide repo root
//! 3. that checkout's `bins/Release/pcsx-redux`
//!
//! The `.app` bundle path is deliberately not tried -- it launches the
//! GUI even when `-no-ui` is passed.

use std::env;
use std::path::{Path, PathBuf};

use crate::error::OracleError;

/// Harness configuration.
#[derive(Clone, Debug)]
pub struct OracleConfig {
    /// Path to the Redux binary.
    pub binary: PathBuf,
    /// BIOS image to boot.
    pub bios: PathBuf,
    /// Lua script passed via `-dofile`.
    pub lua_script: PathBuf,
    /// Optional disc image. `None` means boot without a disc inserted.
    pub disc: Option<PathBuf>,
}

impl OracleConfig {
    /// Resolve the Redux binary and construct a config with the given
    /// BIOS + Lua script. Returns [`OracleError::BinaryNotFound`] if no
    /// candidate path exists.
    pub fn new(bios: PathBuf, lua_script: PathBuf) -> Result<Self, OracleError> {
        let binary = resolve_binary()?;
        Ok(Self {
            binary,
            bios,
            lua_script,
            disc: None,
        })
    }

    /// Attach a disc image to be passed via `-iso`. Without this, Redux
    /// boots with no disc inserted (useful for Milestones A and B).
    pub fn with_disc(mut self, disc: PathBuf) -> Self {
        self.disc = Some(disc);
        self
    }

    /// Default Lua script directory (shipped with the crate).
    pub fn default_lua_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
    }
}

fn resolve_binary() -> Result<PathBuf, OracleError> {
    let mut tried: Vec<PathBuf> = Vec::new();

    if let Ok(explicit) = env::var("PSOXIDE_REDUX_BIN") {
        let p = PathBuf::from(&explicit);
        if is_executable(&p) {
            return Ok(p);
        }
        tried.push(p);
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let repo_parent = repo_root.parent().unwrap_or(&repo_root);

    for p in [
        repo_parent.join("pcsx-redux/pcsx-redux"),
        repo_parent.join("pcsx-redux/bins/Release/pcsx-redux"),
    ] {
        if is_executable(&p) {
            return Ok(p);
        }
        tried.push(p);
    }

    Err(OracleError::BinaryNotFound { tried })
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}
