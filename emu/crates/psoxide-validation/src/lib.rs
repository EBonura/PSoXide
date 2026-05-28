//! Manifest and exact-hash comparison primitives for PSoXide validation.
//!
//! The crate is deliberately runner-agnostic. The frontend, Redux, and
//! DuckStation adapters can all produce [`ActualHashes`] and compare them
//! against the same manifest data.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Human-editable validation suite.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationSuite {
    /// All validation targets grouped by source category.
    #[serde(default)]
    pub targets: Vec<ValidationTarget>,
}

impl ValidationSuite {
    /// Load a RON validation suite from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ValidationError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ValidationError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        ron::from_str(&text).map_err(|source| ValidationError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Save the suite in a stable pretty RON form.
    pub fn save_pretty(&self, path: impl AsRef<Path>) -> Result<(), ValidationError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| ValidationError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(6)
            .separate_tuple_members(true);
        let text = ron::ser::to_string_pretty(self, pretty).map_err(ValidationError::Serialize)?;
        std::fs::write(path, text).map_err(|source| ValidationError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// One disc/project/example/commercial game being validated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationTarget {
    /// Stable human-readable target name.
    pub name: String,
    /// Source category.
    pub category: ValidationCategory,
    /// Artifact to boot for this target.
    pub artifact: ValidationArtifact,
    /// Exact checkpoints to run.
    #[serde(default)]
    pub checkpoints: Vec<ValidationCheckpoint>,
}

/// Validation source category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationCategory {
    /// Ad-hoc SDK/engine example built for a focused feature.
    Example,
    /// Editor-produced project.
    Project,
    /// Commercial game used as a gold standard.
    Commercial,
}

/// Bootable artifact source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationArtifact {
    /// Editor project directory or `project.ron`; the frontend builds it first.
    Project {
        /// Project path.
        project: PathBuf,
    },
    /// Already-built disc image.
    Disc {
        /// CUE/BIN/ISO/CCD path.
        path: PathBuf,
        /// Use the embedded editor playtest fast-boot path.
        #[serde(default)]
        embedded_playtest: bool,
        /// Force the real BIOS boot path.
        #[serde(default)]
        bios_boot: bool,
    },
    /// Already-built focused example image.
    Example {
        /// CUE/BIN/ISO/EXE path.
        path: PathBuf,
    },
    /// Commercial game image.
    Commercial {
        /// CUE/BIN/ISO/CCD path.
        path: PathBuf,
    },
}

/// One exact validation point for a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheckpoint {
    /// Stable checkpoint name.
    pub name: String,
    /// Runner responsible for this checkpoint.
    #[serde(default)]
    pub runner: ValidationRunner,
    /// When the runner should stop and hash.
    #[serde(default)]
    pub stop: StopCondition,
    /// Optional input tape to replay.
    #[serde(default)]
    pub input_tape: Option<PathBuf>,
    /// Hold the in-game forward input for simple motion benchmarks.
    #[serde(default)]
    pub hold_forward: bool,
    /// Hold the in-game run button for simple motion benchmarks.
    #[serde(default)]
    pub hold_run: bool,
    /// Expected exact hashes.
    #[serde(default)]
    pub expected: ExpectedHashes,
}

/// Supported validation runner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationRunner {
    /// PSoXide's own emulator frontend.
    #[default]
    Psoxide,
    /// PCSX-Redux external runner.
    Redux,
    /// DuckStation external runner.
    Duckstation,
}

impl fmt::Display for ValidationRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Psoxide => f.write_str("psoxide"),
            Self::Redux => f.write_str("redux"),
            Self::Duckstation => f.write_str("duckstation"),
        }
    }
}

impl FromStr for ValidationRunner {
    type Err = ValidationRunnerParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "psoxide" | "frontend" => Ok(Self::Psoxide),
            "redux" | "pcsx-redux" | "pcsx_redux" => Ok(Self::Redux),
            "duckstation" | "duck" => Ok(Self::Duckstation),
            _ => Err(ValidationRunnerParseError(value.to_string())),
        }
    }
}

/// Unknown validation runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationRunnerParseError(String);

impl fmt::Display for ValidationRunnerParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown validation runner: {}", self.0)
    }
}

impl std::error::Error for ValidationRunnerParseError {}

/// Stop condition for a validation checkpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StopCondition {
    /// Maximum CPU instructions to retire before stopping.
    #[serde(default = "default_steps")]
    pub steps: u64,
    /// Optional guest frame-begin telemetry limit.
    #[serde(default)]
    pub guest_frames: Option<u64>,
    /// Optional rendered visual-frame telemetry limit.
    #[serde(default)]
    pub guest_visual_frames: Option<u64>,
}

impl Default for StopCondition {
    fn default() -> Self {
        Self {
            steps: default_steps(),
            guest_frames: None,
            guest_visual_frames: None,
        }
    }
}

fn default_steps() -> u64 {
    100_000_000
}

/// Expected exact hashes for a checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedHashes {
    /// Visible-display pixel hash and dimensions.
    #[serde(default)]
    pub display: Option<PixelHash>,
    /// Full VRAM hash.
    #[serde(default)]
    pub vram: Option<String>,
}

impl ExpectedHashes {
    /// Replace expected hashes with an observed run.
    pub fn bless(&mut self, actual: &ActualHashes) {
        self.display = Some(actual.display.clone());
        self.vram = Some(actual.vram.clone());
    }

    /// Compare against observed hashes. Missing baselines are failures.
    pub fn compare(&self, actual: &ActualHashes) -> CheckReport {
        let mut mismatches = Vec::new();
        match &self.display {
            Some(expected) => {
                if expected.width != actual.display.width
                    || expected.height != actual.display.height
                {
                    mismatches.push(ValidationMismatch::DisplaySize {
                        expected_width: expected.width,
                        expected_height: expected.height,
                        actual_width: actual.display.width,
                        actual_height: actual.display.height,
                    });
                }
                if expected.byte_len != actual.display.byte_len {
                    mismatches.push(ValidationMismatch::DisplayByteLen {
                        expected: expected.byte_len,
                        actual: actual.display.byte_len,
                    });
                }
                if normalize_hash(&expected.hash) != normalize_hash(&actual.display.hash) {
                    mismatches.push(ValidationMismatch::DisplayHash {
                        expected: expected.hash.clone(),
                        actual: actual.display.hash.clone(),
                    });
                }
            }
            None => mismatches.push(ValidationMismatch::MissingDisplayBaseline),
        }

        match &self.vram {
            Some(expected) => {
                if normalize_hash(expected) != normalize_hash(&actual.vram) {
                    mismatches.push(ValidationMismatch::VramHash {
                        expected: expected.clone(),
                        actual: actual.vram.clone(),
                    });
                }
            }
            None => mismatches.push(ValidationMismatch::MissingVramBaseline),
        }

        CheckReport { mismatches }
    }
}

/// One visible-display hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelHash {
    /// FNV-1a-64 hash, formatted as `0x0000000000000000`.
    pub hash: String,
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
    /// Number of bytes hashed.
    pub byte_len: u64,
}

impl PixelHash {
    /// Build a normalized pixel hash from raw values.
    pub fn from_u64(hash: u64, width: u32, height: u32, byte_len: u64) -> Self {
        Self {
            hash: format_hash(hash),
            width,
            height,
            byte_len,
        }
    }
}

/// Observed exact hashes for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualHashes {
    /// Visible-display pixel hash and dimensions.
    pub display: PixelHash,
    /// Full VRAM hash.
    pub vram: String,
}

/// Exact comparison result for one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// All exact mismatches.
    pub mismatches: Vec<ValidationMismatch>,
}

impl CheckReport {
    /// Whether the checkpoint matched exactly.
    pub fn passed(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Why a checkpoint failed exact validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationMismatch {
    /// No display hash baseline exists yet.
    MissingDisplayBaseline,
    /// No VRAM hash baseline exists yet.
    MissingVramBaseline,
    /// Visible-display hash changed.
    DisplayHash {
        /// Expected hash.
        expected: String,
        /// Actual hash.
        actual: String,
    },
    /// Visible-display dimensions changed.
    DisplaySize {
        /// Expected width.
        expected_width: u32,
        /// Expected height.
        expected_height: u32,
        /// Actual width.
        actual_width: u32,
        /// Actual height.
        actual_height: u32,
    },
    /// Visible-display byte length changed.
    DisplayByteLen {
        /// Expected byte count.
        expected: u64,
        /// Actual byte count.
        actual: u64,
    },
    /// Full VRAM hash changed.
    VramHash {
        /// Expected hash.
        expected: String,
        /// Actual hash.
        actual: String,
    },
}

impl fmt::Display for ValidationMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDisplayBaseline => f.write_str("missing display baseline"),
            Self::MissingVramBaseline => f.write_str("missing VRAM baseline"),
            Self::DisplayHash { expected, actual } => {
                write!(f, "display hash expected {expected}, got {actual}")
            }
            Self::DisplaySize {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                f,
                "display size expected {expected_width}x{expected_height}, got {actual_width}x{actual_height}"
            ),
            Self::DisplayByteLen { expected, actual } => {
                write!(f, "display byte_len expected {expected}, got {actual}")
            }
            Self::VramHash { expected, actual } => {
                write!(f, "VRAM hash expected {expected}, got {actual}")
            }
        }
    }
}

/// Validation manifest IO error.
#[derive(Debug, Error)]
pub enum ValidationError {
    /// Could not read the manifest.
    #[error("read {path}: {source}")]
    Read {
        /// Path being read.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// Could not parse the manifest.
    #[error("parse {path}: {source}")]
    Parse {
        /// Path being parsed.
        path: PathBuf,
        /// Underlying RON error.
        source: ron::error::SpannedError,
    },
    /// Could not serialize the manifest.
    #[error("serialize validation suite: {0}")]
    Serialize(ron::Error),
    /// Could not write the manifest.
    #[error("write {path}: {source}")]
    Write {
        /// Path being written.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
}

/// Format a raw hash for manifests and logs.
pub fn format_hash(hash: u64) -> String {
    format!("0x{hash:016x}")
}

fn normalize_hash(value: &str) -> String {
    let value = value.trim();
    let without_prefix = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    match u64::from_str_radix(without_prefix, 16) {
        Ok(hash) => format_hash(hash),
        Err(_) => value.to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bless_and_compare_are_exact() {
        let actual = ActualHashes {
            display: PixelHash::from_u64(0x12, 320, 240, 307_200),
            vram: format_hash(0x34),
        };
        let mut expected = ExpectedHashes::default();
        assert!(!expected.compare(&actual).passed());

        expected.bless(&actual);
        assert!(expected.compare(&actual).passed());

        let changed = ActualHashes {
            display: PixelHash::from_u64(0x13, 320, 240, 307_200),
            vram: format_hash(0x34),
        };
        assert_eq!(
            expected.compare(&changed).mismatches,
            vec![ValidationMismatch::DisplayHash {
                expected: format_hash(0x12),
                actual: format_hash(0x13),
            }]
        );
    }

    #[test]
    fn hash_format_is_stable() {
        assert_eq!(format_hash(0xabc), "0x0000000000000abc");
        assert_eq!(normalize_hash("ABC"), "0x0000000000000abc");
    }

    #[test]
    fn suite_round_trips() {
        let suite = ValidationSuite {
            targets: vec![ValidationTarget {
                name: "demo".to_string(),
                category: ValidationCategory::Project,
                artifact: ValidationArtifact::Project {
                    project: PathBuf::from("editor/projects/demo10"),
                },
                checkpoints: vec![ValidationCheckpoint {
                    name: "boot".to_string(),
                    runner: ValidationRunner::Psoxide,
                    stop: StopCondition {
                        steps: 123,
                        guest_frames: Some(4),
                        guest_visual_frames: Some(2),
                    },
                    input_tape: None,
                    hold_forward: false,
                    hold_run: false,
                    expected: ExpectedHashes::default(),
                }],
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("suite.ron");
        suite.save_pretty(&path).unwrap();
        let loaded = ValidationSuite::load(&path).unwrap();
        assert_eq!(loaded.targets.len(), 1);
        assert_eq!(loaded.targets[0].checkpoints.len(), 1);
    }
}
