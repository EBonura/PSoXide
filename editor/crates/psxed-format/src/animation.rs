//! On-disk layout for cooked rigid-skeletal animations (`.psxanim`).
//!
//! The animation format stores already-sampled fixed-point joint pose
//! matrices. Runtime code can index or cheaply interpolate between
//! sampled frames, fetch the joint record for each model part, and
//! submit transformed triangles without evaluating glTF channels or
//! quaternions on the PS1.
//!
//! # File layout
//!
//! ```text
//!   AssetHeader (12 bytes)
//!     magic       = b"PSXA"
//!     version     = VERSION
//!     flags       = reserved
//!     payload_len = everything after this header
//!
//!   AnimationHeader (8 bytes)
//!     joint_count         u16
//!     frame_count         u16
//!     sample_rate_hz      u16
//!     translation_shift   u16   // v2+
//!
//!   Pose table: frame_count × joint_count × 24 bytes
//!     matrix:      i16[9] Q3.12, column-major 3×3
//!     translation: i16[3] model-local units >> translation_shift
//! ```
//!
//! The pose matrix maps model-space vertices into the sampled animated
//! pose. Translation uses the same model-local unit scale as the
//! matching `.psxmdl` vertices, so local precision can be much denser
//! than world/grid precision. Version 1 files used 30-byte records
//! with `i32[3]` translations; readers keep supporting that legacy
//! layout.

/// ASCII magic identifying the `.psxanim` animation format.
pub const MAGIC: [u8; 4] = *b"PSXA";

/// Legacy animation format revision.
pub const VERSION_V1: u16 = 1;

/// Current animation format revision.
pub const VERSION: u16 = 2;

/// Byte layout of the animation payload header.
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct AnimationHeader {
    /// Number of joint pose records per frame.
    pub joint_count: u16,
    /// Number of sampled frames.
    pub frame_count: u16,
    /// Integer sample rate in Hz.
    pub sample_rate_hz: u16,
    /// Shared right shift applied to every stored translation in v2+.
    ///
    /// Runtime reconstructs model-local units with `stored << shift`.
    /// Version 1 files leave this as zero and store `i32` translations.
    pub _reserved: u16,
}

impl AnimationHeader {
    /// Size of the animation header in bytes (always 8).
    pub const SIZE: usize = 8;

    /// Build an animation header.
    pub const fn new(joint_count: u16, frame_count: u16, sample_rate_hz: u16) -> Self {
        Self::new_with_translation_shift(joint_count, frame_count, sample_rate_hz, 0)
    }

    /// Build an animation header with an explicit translation shift.
    pub const fn new_with_translation_shift(
        joint_count: u16,
        frame_count: u16,
        sample_rate_hz: u16,
        translation_shift: u16,
    ) -> Self {
        Self {
            joint_count,
            frame_count,
            sample_rate_hz,
            _reserved: translation_shift,
        }
    }

    /// Shared right shift for compact v2 translations.
    pub const fn translation_shift(&self) -> u16 {
        self._reserved
    }
}

/// Size of one legacy v1 joint pose record in bytes.
pub const POSE_RECORD_SIZE_V1: usize = 30;

/// Size of one current joint pose record in bytes.
pub const POSE_RECORD_SIZE: usize = 24;
