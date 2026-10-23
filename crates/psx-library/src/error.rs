//! The ways an install can fail.

use std::path::PathBuf;
use std::{fmt, io};

/// Why an install was refused or failed. Nothing is installed unless `install` returns `Ok`;
/// a failure after the first file was published still leaves every live file either the old
/// version or the complete new one.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    Io {
        /// What was being done, with the path.
        action: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The library name cannot be used as a file name stem.
    InvalidName(String),
    /// The cue sheet has no `FILE` directive to point at the installed bin.
    CueWithoutFile(PathBuf),
    /// A temporary file did not hash to what was written into it.
    DigestMismatch {
        /// The temporary file.
        path: PathBuf,
        /// What it hashed to.
        actual: String,
        /// What it should have hashed to.
        expected: String,
    },
    /// A bin under the old flat name differs from the new disc and is not older than it.
    LegacyDiscNotOlder(PathBuf),
    /// The source commit could not be read from git.
    Git(String),
    /// The provenance base is not a JSON object.
    ProvenanceBase,
    /// Serialising the provenance record failed.
    Json(serde_json::Error),
}

impl Error {
    pub(crate) fn io(action: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            action: action.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, source } => write!(f, "{action}: {source}"),
            Self::InvalidName(name) => write!(
                f,
                "library name {name:?} must be non-empty, not start with '.', and hold no path \
                 separator"
            ),
            Self::CueWithoutFile(path) => write!(f, "{}: no FILE directive", path.display()),
            Self::DigestMismatch {
                path,
                actual,
                expected,
            } => write!(
                f,
                "{} hashes to {actual}, expected {expected}; install aborted",
                path.display()
            ),
            Self::LegacyDiscNotOlder(path) => write!(
                f,
                "{} is under the old name, differs from the disc being installed and is not \
                 older than it; compare the two and remove or rename one by hand",
                path.display()
            ),
            Self::Git(reason) => write!(f, "cannot read the source commit: {reason}"),
            Self::ProvenanceBase => f.write_str("the provenance base must be a JSON object"),
            Self::Json(error) => write!(f, "provenance record: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
