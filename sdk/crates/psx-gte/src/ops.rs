//! GTE function-op wrappers.
//!
//! Each op corresponds to one COP2 cofun instruction. The constants
//! below are the exact 32-bit encodings PSX-SPX documents -- the same
//! bits the MIPS CPU would dispatch on hardware and the same bits the
//! emulator's `Gte::execute` decodes. On host we hand them straight
//! to the per-thread software GTE in [`crate::host`]; on MIPS we emit
//! them as `.word` instructions so LLVM's integrated assembler can't
//! reject the COP2 mnemonics it doesn't recognise.
//!
//! Encoding (per PSX-SPX, "GTE Coprocessor Opcodes"):
//!
//! ```text
//!   bits 31..26 = 0x12 (010010)      -- COP2
//!   bit  25     = 1                   -- `cop2 cofun` prefix
//!   bit  19     = sf                  -- fraction-shift
//!   bits 18..17 = mx                  -- MVMVA matrix select
//!   bits 16..15 = vx                  -- MVMVA vector select
//!   bits 14..13 = cv                  -- MVMVA translation select
//!   bit  10     = lm                  -- IR lower-bound mode
//!   bits 5..0   = opcode              -- function id
//! ```
//!
//! The `0x4A000000` base (`0x12<<26 | 1<<25`) is common to every op.

#[cfg(target_arch = "mips")]
use core::arch::asm;

/// Emit one cofun instruction. On MIPS this is a `.word` with the
/// encoding baked into the immediate; on host it executes against the
/// thread-local software Gte.
macro_rules! cofun {
    ($instr:expr) => {{
        #[cfg(target_arch = "mips")]
        // SAFETY: every `$instr` passed in this module is a `0x4A..` COP2
        // cofun encoding, which reads and writes GTE registers only. It
        // writes no CPU register, needs no stack (`nostack`), and the asm
        // conservatively omits `nomem` even though the op never touches RAM.
        // The wrappers' `# Safety` input-register preconditions only decide
        // whether the result is meaningful; stale inputs give garbage
        // numbers, not undefined behaviour.
        unsafe {
            asm!(
                ".word {instr}",
                instr = const $instr,
                options(nostack, preserves_flags),
            )
        }
        #[cfg(not(target_arch = "mips"))]
        {
            $crate::host::execute($instr);
        }
    }};
}

/// Emit the deprecated forwarder that keeps an op's old mnemonic name
/// compiling after its rename. `$note` is the full deprecation note, spelled
/// out at each use because `#[deprecated]` only takes a string literal.
macro_rules! renamed_op {
    ($old:ident => $new:ident, $note:literal) => {
        #[doc = concat!("Renamed to [`", stringify!($new), "`].")]
        ///
        /// # Safety
        #[doc = concat!("See [`", stringify!($new), "`].")]
        #[deprecated(note = $note)]
        #[inline(always)]
        pub unsafe fn $old() {
            // SAFETY: same contract as the renamed function.
            unsafe { $new() }
        }
    };
}

/// Perspective-project V0 (`RTPS`, `sf=1, lm=0`).
///
/// # Safety
/// Assumes V0, RT, TR, OFX/OFY, H, DQA/DQB are loaded.
#[doc(alias = "RTPS")]
#[doc(alias = "RotTransPers")]
#[inline(always)]
pub unsafe fn project_single() {
    cofun!(crate::encoding::command::RTPS)
}

/// Perspective-project V0, V1 and V2 in sequence (`RTPT`, `sf=1, lm=0`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "RTPT")]
#[doc(alias = "RotTransPers3")]
#[inline(always)]
pub unsafe fn project_triple() {
    cofun!(crate::encoding::command::RTPT)
}

/// Winding of the three SXY entries (`NCLIP`): the Z component of
/// `(SXY1-SXY0) × (SXY2-SXY0)` into MAC0.
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCLIP")]
#[doc(alias = "NormalClip")]
#[inline(always)]
pub unsafe fn screen_winding() {
    cofun!(crate::encoding::command::NCLIP)
}

/// Outer product of IR with the rotation matrix diagonal (`OP`, `sf=1`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "OP")]
#[doc(alias = "OuterProduct12")]
#[inline(always)]
pub unsafe fn outer_product() {
    cofun!(crate::encoding::command::OP)
}

/// Average SZ1..SZ3 weighted by ZSF3 into OTZ and MAC0 (`AVSZ3`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "AVSZ3")]
#[doc(alias = "AverageZ3")]
#[inline(always)]
pub unsafe fn average_z3() {
    cofun!(crate::encoding::command::AVSZ3)
}

/// Average SZ0..SZ3 weighted by ZSF4 into OTZ and MAC0 (`AVSZ4`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "AVSZ4")]
#[doc(alias = "AverageZ4")]
#[inline(always)]
pub unsafe fn average_z4() {
    cofun!(crate::encoding::command::AVSZ4)
}

/// Square the current IR vector into MAC1/2/3 (`SQR`, `sf=1`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "SQR")]
#[doc(alias = "Square12")]
#[inline(always)]
pub unsafe fn square() {
    cofun!(crate::encoding::command::SQR)
}

/// Square the current IR vector into MAC1/2/3 without the fractional
/// right shift (`SQR`, `sf=0`).
///
/// This form is useful for integer vector lengths: the MAC registers retain
/// the exact component squares while the CPU prepares a reciprocal scale.
///
/// # Safety
/// IR1, IR2 and IR3 must contain the vector to square.
#[doc(alias = "SQR")]
#[doc(alias = "Square0")]
#[inline(always)]
pub unsafe fn square_unshifted() {
    cofun!(crate::encoding::command::SQR_UNSHIFTED)
}

/// Light, color and depth-cue one vertex normal (`NCDS`, `sf=1, lm=0`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCDS")]
#[doc(alias = "NormalColorDpq")]
#[inline(always)]
pub unsafe fn light_color_depth_single() {
    cofun!(crate::encoding::command::NCDS)
}

/// Light and color one vertex normal, no depth cue (`NCCS`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCCS")]
#[doc(alias = "NormalColorCol")]
#[inline(always)]
pub unsafe fn light_color_single() {
    cofun!(crate::encoding::command::NCCS)
}

/// Light one vertex normal, without the RGBC modulate (`NCS`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCS")]
#[doc(alias = "NormalColor")]
#[inline(always)]
pub unsafe fn light_single() {
    cofun!(crate::encoding::command::NCS)
}

/// [`light_color_depth_single`] for V0, V1 and V2 (`NCDT`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCDT")]
#[doc(alias = "NormalColorDpq3")]
#[inline(always)]
pub unsafe fn light_color_depth_triple() {
    cofun!(crate::encoding::command::NCDT)
}

/// [`light_single`] for V0, V1 and V2 (`NCT`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCT")]
#[doc(alias = "NormalColor3")]
#[inline(always)]
pub unsafe fn light_triple() {
    cofun!(crate::encoding::command::NCT)
}

/// [`light_color_single`] for V0, V1 and V2 (`NCCT`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "NCCT")]
#[doc(alias = "NormalColorCol3")]
#[inline(always)]
pub unsafe fn light_color_triple() {
    cofun!(crate::encoding::command::NCCT)
}

/// Depth-cue RGBC toward the far color by IR0 (`DPCS`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "DPCS")]
#[doc(alias = "DpqColor")]
#[inline(always)]
pub unsafe fn depth_cue_single() {
    cofun!(crate::encoding::command::DPCS)
}

/// [`depth_cue_single`] run three times against the RGB FIFO (`DPCT`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "DPCT")]
#[doc(alias = "DpqColor3")]
#[inline(always)]
pub unsafe fn depth_cue_triple() {
    cofun!(crate::encoding::command::DPCT)
}

/// Interpolate IR toward the far color by IR0, then push the color FIFO
/// (`INTPL`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "INTPL")]
#[inline(always)]
pub unsafe fn interpolate_far_color() {
    cofun!(crate::encoding::command::INTPL)
}

/// Depth-cue a lit color: `RGBC*IR` toward FC by IR0 (`DCPL`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "DCPL")]
#[doc(alias = "DpqColorLight")]
#[inline(always)]
pub unsafe fn depth_cue_light() {
    cofun!(crate::encoding::command::DCPL)
}

/// Light IR through the light color matrix and modulate by RGBC (`CC`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "CC")]
#[doc(alias = "ColorCol")]
#[inline(always)]
pub unsafe fn color_color() {
    cofun!(crate::encoding::command::CC)
}

/// [`color_color`] followed by a depth cue toward the far color (`CDP`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "CDP")]
#[doc(alias = "ColorDpq")]
#[inline(always)]
pub unsafe fn color_depth_cue() {
    cofun!(crate::encoding::command::CDP)
}

/// Scale the IR vector by IR0 into MAC, then push the color FIFO
/// (`GPF`, `sf=1`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "GPF")]
#[inline(always)]
pub unsafe fn scale_vector() {
    cofun!(crate::encoding::command::GPF)
}

/// Scale the IR vector by IR0 into MAC, then push the color FIFO, without
/// the fractional right shift (`GPF`, `sf=0`).
///
/// # Safety
/// IR0 through IR3 must contain the scalar and vector inputs.
#[doc(alias = "GPF")]
#[inline(always)]
pub unsafe fn scale_vector_unshifted() {
    cofun!(crate::encoding::command::GPF_UNSHIFTED)
}

/// Add the IR vector scaled by IR0 to MAC, then push the color FIFO
/// (`GPL`, `sf=1`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "GPL")]
#[inline(always)]
pub unsafe fn scale_vector_accumulate() {
    cofun!(crate::encoding::command::GPL)
}

/// Rotate V0 by RT and add TR, without the perspective divide
/// (`MVMVA` with `mx=RT, vx=V0, cv=TR, sf=1, lm=0`).
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "MVMVA")]
#[doc(alias = "RotTrans")]
#[inline(always)]
pub unsafe fn rotate_translate_v0() {
    cofun!(crate::encoding::command::ROTATE_TRANSLATE_V0)
}

/// Rotate V0 by RT with the far color as the translation
/// (`MVMVA` with `mx=RT, vx=V0, cv=FC, sf=1, lm=0`).
///
/// PSX-SPX documents this `cv=FC` combination as hardware-bugged
/// (the first matrix-row product is discarded mid-accumulate), which makes
/// it a sharp emulator-vs-silicon conformance check rather than a useful
/// transform.
///
/// # Safety
/// See [`project_single`].
#[doc(alias = "MVMVA")]
#[inline(always)]
pub unsafe fn rotate_v0_far_color() {
    cofun!(crate::encoding::command::ROTATE_V0_FAR_COLOR)
}

// Deprecated forwarders for the mnemonic names these ops had before the
// naming convention (sdk/docs/NAMING.md).
renamed_op!(rtps => project_single, "renamed to `project_single`");
renamed_op!(rtpt => project_triple, "renamed to `project_triple`");
renamed_op!(nclip => screen_winding, "renamed to `screen_winding`");
renamed_op!(op_sf1 => outer_product, "renamed to `outer_product`");
renamed_op!(avsz3 => average_z3, "renamed to `average_z3`");
renamed_op!(avsz4 => average_z4, "renamed to `average_z4`");
renamed_op!(sqr => square, "renamed to `square`");
renamed_op!(sqr_sf0 => square_unshifted, "renamed to `square_unshifted`");
renamed_op!(ncds => light_color_depth_single, "renamed to `light_color_depth_single`");
renamed_op!(nccs => light_color_single, "renamed to `light_color_single`");
renamed_op!(ncs => light_single, "renamed to `light_single`");
renamed_op!(ncdt => light_color_depth_triple, "renamed to `light_color_depth_triple`");
renamed_op!(nct => light_triple, "renamed to `light_triple`");
renamed_op!(ncct => light_color_triple, "renamed to `light_color_triple`");
renamed_op!(dpcs => depth_cue_single, "renamed to `depth_cue_single`");
renamed_op!(dpct => depth_cue_triple, "renamed to `depth_cue_triple`");
renamed_op!(intpl => interpolate_far_color, "renamed to `interpolate_far_color`");
renamed_op!(dcpl => depth_cue_light, "renamed to `depth_cue_light`");
renamed_op!(cc => color_color, "renamed to `color_color`");
renamed_op!(cdp => color_depth_cue, "renamed to `color_depth_cue`");
renamed_op!(gpf => scale_vector, "renamed to `scale_vector`");
renamed_op!(gpf_sf0 => scale_vector_unshifted, "renamed to `scale_vector_unshifted`");
renamed_op!(gpl => scale_vector_accumulate, "renamed to `scale_vector_accumulate`");
renamed_op!(mvmva_rt_v0_tr_sf1 => rotate_translate_v0, "renamed to `rotate_translate_v0`");
renamed_op!(mvmva_rt_v0_fc_sf1 => rotate_v0_far_color, "renamed to `rotate_v0_far_color`");
