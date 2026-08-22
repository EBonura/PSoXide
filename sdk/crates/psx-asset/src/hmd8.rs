//! HMD8: a merged model container carrying mesh, bone ranges and every clip's
//! composed bone palettes in one blob.
//!
//! Lifted from hl-psx, which developed it to run the whole Half-Life campaign
//! (103 maps, 237 transitions) inside a 223.7 KB model pool covering mesh AND
//! animation. It lives here so both games share one format rather than each
//! carrying a private reader.
//!
//! What it does that `Model` + `Animation` do not:
//!
//! * one blob per model, with a clip table, so a model's whole animation set
//!   is a single asset rather than one file per clip;
//! * vertices stored bone-local and grouped into contiguous RANGES, so each
//!   range pays one matrix load and feeds the static stream straight to RTPT;
//! * a 20-byte composed affine per bone per frame (`HMD8_AFFINE_BYTES`), the
//!   same packed Q11 encoding `.psxanim` v3 already uses;
//! * optional sections gated by flags (body masks, mouth transforms, studio
//!   hitboxes, packed or i8 normals, frame times, vertex SoA), so a consumer
//!   that wants none of the GoldSrc specifics simply does not emit them.
//!
//! Extraction note: the only coupling to its old home was a vertex-count guard
//! reading a game-local budget constant. That is now
//! [`DEFAULT_MAX_VERTS`] with [`Model::load_with_vertex_cap`] for callers that
//! police their own arena.

// ponytail: the lifted reader names its fields and accessors after the format
// fields they read (`n_verts`, `first`, `count`), so per-item docs would only
// restate the name. The TYPES carry the meaning and are documented below.
#![allow(missing_docs)]

use core::ptr;

use psx_gte::math::{Mat3I16, Vec3I16};

// ponytail: unchecked reads, same rationale as map.rs -- no D-cache on the
// R3000, so dropping the bounds branch lets LLVM coalesce these into MIPS
// unaligned word loads (lwl/lwr). Offsets derive from the cooked model header
// (magic-checked at load), so they are in range by construction.
#[inline(always)]
fn rd_u32(d: &[u8], o: usize) -> u32 {
    unsafe {
        let p = d.as_ptr().add(o);
        u32::from_le_bytes([p.read(), p.add(1).read(), p.add(2).read(), p.add(3).read()])
    }
}
#[inline(always)]
fn rd_u16(d: &[u8], o: usize) -> u16 {
    unsafe {
        let p = d.as_ptr().add(o);
        u16::from_le_bytes([p.read(), p.add(1).read()])
    }
}
#[inline(always)]
fn rd_i16(d: &[u8], o: usize) -> i16 {
    rd_u16(d, o) as i16
}

#[inline(always)]
fn decode_q11(raw: u16) -> i16 {
    // LLVM lowers the reserved-code select to a branch on MIPS-I. Express the
    // six-instruction branchless sequence directly: sign-extend and double the
    // twelve-bit value, then use SLTIU to add two only for the reserved +2047
    // encoding. Every caller masks the packed field to twelve bits first.
    #[cfg(target_arch = "mips")]
    unsafe {
        let decoded: u32;
        core::arch::asm!(
            "xori {scratch}, {decoded}, 0x07ff",
            "sltiu {scratch}, {scratch}, 1",
            "sll {decoded}, {decoded}, 20",
            "sra {decoded}, {decoded}, 19",
            "sll {scratch}, {scratch}, 1",
            "addu {decoded}, {decoded}, {scratch}",
            decoded = inlateout(reg) raw as u32 => decoded,
            scratch = lateout(reg) _,
            options(nomem, nostack, preserves_flags),
        );
        return decoded as i16;
    }

    #[cfg(not(target_arch = "mips"))]
    {
        let signed = ((raw << 4) as i16) >> 4;
        signed
            .wrapping_shl(1)
            .wrapping_add(((raw == 0x07ff) as i16) << 1)
    }
}

#[inline(always)]
fn rd_q11_pair(d: &[u8], o: usize) -> (i16, i16) {
    unsafe {
        // Each pair occupies three bytes. One unaligned word load is cheaper
        // on MIPS-I than three independent byte loads and exposes both packed
        // fields directly; byte 3 belongs to the next pair and is masked out.
        let packed = u32::from_le(d.as_ptr().add(o).cast::<u32>().read_unaligned());
        (
            decode_q11((packed & 0x0fff) as u16),
            decode_q11(((packed >> 12) & 0x0fff) as u16),
        )
    }
}
#[inline(always)]
fn unpack_normal_555(packed: u16) -> [i8; 3] {
    let component = |shift: u32| -> i8 {
        let bits = ((packed >> shift) & 0x1f) as i16;
        let signed = if bits & 0x10 != 0 { bits - 32 } else { bits };
        (signed * 8) as i8
    };
    [component(0), component(5), component(10)]
}

/// A parsed HMD8 blob: mesh, bone ranges, clip table and every clip's composed
/// bone palettes, all borrowed from one `&'static` slice with no copying.
///
/// Construct with [`Model::load`] or [`Model::load_with_vertex_cap`]; a blob
/// that fails validation yields an empty model rather than panicking, so a bad
/// cook draws nothing instead of taking the guest down.
#[derive(Clone, Copy)]
pub struct Model {
    data: &'static [u8],
    pub n_verts: usize,
    pub n_tris: usize,
    pub n_frames: usize,
    pub n_clips: usize,
    clips_off: usize,
    frame_times_off: usize,
    ranges_off: usize,
    vertices_off: usize,
    vertices_z_off: usize,
    poses_off: usize,
    pub n_bones: usize,
    pub n_ranges: usize,
    tri_off: usize,
    tri_sz: usize,
    normal_encoding: u8,
    local_to_world_q12: u16,
    mouth_xforms_off: usize,
    hitboxes_off: usize,
    pub n_hitboxes: usize,
    has_body_masks: bool,
    has_mouth: bool,
    has_frame_times: bool,
    #[allow(dead_code)] // read only by the MIPS vertex fast path
    aligned_vertices: bool,
    vertex_soa: bool,
}

/// One HMD8 vertex in the exact two-register layout consumed by the GTE.
///
/// Keeping XY packed avoids rebuilding the same word with shifts and ORs
/// before every model RTPT batch; Z is sign-extended for the paired VZ input.
#[derive(Clone, Copy)]
pub struct GteVertexWords {
    pub xy: u32,
    pub z: u32,
}

/// One frame's bone palette, read lazily: a bone's affine is unpacked from the
/// blob when asked for, not eagerly into a scratch array.
#[derive(Clone, Copy)]
pub struct ModelFrame<'a> {
    data: &'a [u8],
    poses_off: usize,
    n_bones: usize,
    frame_idx: usize,
    mouth_xforms_off: usize,
    has_mouth: bool,
}

/// Two baked bone palettes prepared for fixed-point affine interpolation.
#[derive(Clone, Copy)]
pub struct InterpolatedModelFrame<'a> {
    a: ModelFrame<'a>,
    b: ModelFrame<'a>,
    frac16: i32,
}

/// A contiguous run of vertices sharing one bone.
///
/// This is the reason the format is fast: vertices are stored bone-local and
/// sorted by bone, so a range pays ONE matrix load and then streams straight
/// into RTPT, instead of a per-vertex matrix lookup.
#[derive(Clone, Copy)]
pub struct BoneRange {
    pub first: usize,
    pub count: usize,
    pub bone: usize,
    pub body_mask: u8,
    pub mouth: bool,
}

/// A bone's composed pose, unpacked from the 20-byte record into GTE registers.
#[derive(Clone, Copy)]
pub struct BoneTransform {
    pub rotation: Mat3I16,
    pub translation: Vec3I16,
}

/// A GoldSrc studio hitbox, present only when the cook set the hitbox flag.
#[derive(Clone, Copy)]
pub struct StudioHitbox {
    pub bone: usize,
    pub bbmin: Vec3I16,
    pub bbmax: Vec3I16,
}

const TRI_SZ: usize = 16;
const TRI_SZ_FULL_NORMALS: usize = 20;
const HMD7_HEADER_BYTES: usize = 36;
const HMD7_RANGE_BYTES: usize = 8;
const HMD8_AFFINE_BYTES: usize = 20;
const HMD7_HITBOX_BYTES: usize = 14;
const CLIP_FRAME_COUNT_MASK: u16 = 0x00ff;
const CLIP_FIRST_FRAME_MASK: u16 = 0x7fff;
const CLIP_DURATION_EXT_BIT: u16 = 0x8000;
const DEFAULT_CLIP_HOLD_TICKS: u16 = 40;
const LOCAL_TO_WORLD_IDENTITY_Q12: u16 = 4096;

#[inline(always)]
const fn valid_local_to_world_q12(raw: u16) -> bool {
    matches!(raw, 0 | 256 | 512 | 1024 | 2048 | 4096)
}

const HMD_FLAG_BODY_MASKS: u16 = 1 << 0;
const HMD_FLAG_MOUTH: u16 = 1 << 1;
const HMD_FLAG_PACKED_NORMALS: u16 = 1 << 2;
const HMD_FLAG_I8_NORMALS: u16 = 1 << 3;
const HMD_FLAG_HITBOXES: u16 = 1 << 4;
const HMD_FLAG_FRAME_TIMES: u16 = 1 << 5;
const HMD_FLAG_ALIGNED_MODEL_DATA: u16 = 1 << 6;
const HMD_FLAG_VERTEX_SOA: u16 = 1 << 7;
const HMD7_RANGE_MOUTH: u8 = 1 << 0;
const NORMAL_NONE: u8 = 0;
const NORMAL_I8X3: u8 = 1;
const NORMAL_PACKED_555: u8 = 2;
const MOUTH_XFORM_BYTES: usize = 24;
// Generated from the same cooked model scan that sizes main.rs MODEL_SCRATCH.
// A larger model would overflow projection scratch, so reject it here rather
// than merely clamping the draw and leaving face indices out of bounds.
/// Vertex-count guard used by [`Model::load`].
///
/// A consumer with its own model arena should call
/// [`Model::load_with_vertex_cap`] and pass that arena's real capacity: the
/// guard exists so a cook that outgrew the arena fails to load rather than
/// scribbling past it.
pub const DEFAULT_MAX_VERTS: usize = 4096;

/// A decoded triangle. The blob keeps these packed; this is the unpacked view.
#[derive(Clone, Copy)]
pub struct Tri {
    pub idx: [u16; 3],
    pub tex: usize,
    pub uv: [(u8, u8); 3],
    pub normal: [i8; 3],
    pub body_mask: u8,
}

/// Cold UV payload for a projected model face. Vertex indices live in a
/// separate packed-u32 stream so near/backface rejects touch only four
/// uncached bytes. Texture ids live once per ordered face run, not once per
/// face; the streamed actor set keeps those ids out of every face payload.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct RenderFacePayload {
    pub uv_words: [u16; 3],
}

impl RenderFacePayload {
    pub const ZERO: Self = Self { uv_words: [0; 3] };
}

/// Model loading rejects meshes above 1024 vertices, so three ten-bit indices
/// fit one hot word exactly (with the top two bits unused).
pub const RENDER_FACE_INDEX_MASK: u32 = 0x3ff;

#[inline(always)]
fn uv_word(uv: (u8, u8)) -> u16 {
    (uv.0 as u16) | ((uv.1 as u16) << 8)
}

#[inline(always)]
unsafe fn write_render_face(
    indices: *mut u32,
    payloads: *mut RenderFacePayload,
    t: usize,
    tri: &Tri,
) {
    // SAFETY: contract is the enclosing fn's; see its doc comment.
    unsafe {
        let packed = (tri.idx[0] as u32 & RENDER_FACE_INDEX_MASK)
            | ((tri.idx[1] as u32 & RENDER_FACE_INDEX_MASK) << 10)
            | ((tri.idx[2] as u32 & RENDER_FACE_INDEX_MASK) << 20);
        ptr::write(indices.add(t), packed);
        ptr::write(
            payloads.add(t),
            RenderFacePayload {
                uv_words: [uv_word(tri.uv[0]), uv_word(tri.uv[1]), uv_word(tri.uv[2])],
            },
        );
    }
}

impl Model {
    /// Zero model for static cache slots (never drawn: n_tris = 0).
    pub const EMPTY: Model = Model {
        data: &[],
        n_verts: 0,
        n_tris: 0,
        n_frames: 0,
        n_clips: 0,
        clips_off: 0,
        frame_times_off: 0,
        ranges_off: 0,
        vertices_off: 0,
        vertices_z_off: 0,
        poses_off: 0,
        n_bones: 0,
        n_ranges: 0,
        tri_off: 0,
        tri_sz: 0,
        normal_encoding: NORMAL_NONE,
        local_to_world_q12: 4096,
        mouth_xforms_off: 0,
        hitboxes_off: 0,
        n_hitboxes: 0,
        has_body_masks: false,
        has_mouth: false,
        has_frame_times: false,
        aligned_vertices: false,
        vertex_soa: false,
    };

    /// Parse a HMD8 blob, guarding vertex count with [`DEFAULT_MAX_VERTS`].
    pub fn load(data: &'static [u8]) -> Model {
        Self::load_with_vertex_cap(data, DEFAULT_MAX_VERTS)
    }

    /// Parse a HMD8 blob, guarding vertex count with the caller's own arena
    /// capacity.
    pub fn load_with_vertex_cap(data: &'static [u8], max_verts: usize) -> Model {
        if data.len() < HMD7_HEADER_BYTES || data.get(0..4) != Some(b"HMD8") {
            return Self::EMPTY;
        }
        let n_verts = rd_u32(data, 4) as usize;
        let n_tris = rd_u32(data, 8) as usize;
        let packed_texture_hitbox_counts = rd_u32(data, 12);
        let n_frames = (rd_u32(data, 16) as usize).max(1);
        let n_clips = (rd_u32(data, 20) as usize).max(1);
        let model_data_len = rd_u32(data, 24) as usize;
        let raw_local_to_world_q12 = rd_u16(data, 28);
        if !valid_local_to_world_q12(raw_local_to_world_q12) {
            return Self::EMPTY;
        }
        let local_to_world_q12 = if raw_local_to_world_q12 == 0 {
            LOCAL_TO_WORLD_IDENTITY_Q12
        } else {
            raw_local_to_world_q12
        };
        let hmd_flags = rd_u16(data, 30);
        let n_bones = rd_u16(data, 32) as usize;
        let n_ranges = rd_u16(data, 34) as usize;
        let clips_off = HMD7_HEADER_BYTES;
        let requested_frame_times = hmd_flags & HMD_FLAG_FRAME_TIMES != 0;
        let frame_times_off = clips_off.saturating_add(n_clips.saturating_mul(4));
        let unaligned_ranges_off =
            frame_times_off.saturating_add(if requested_frame_times { n_frames } else { 0 });
        let requested_vertex_soa = hmd_flags & HMD_FLAG_VERTEX_SOA != 0;
        let ranges_off = if requested_vertex_soa {
            unaligned_ranges_off.saturating_add(3) & !3
        } else if hmd_flags & HMD_FLAG_ALIGNED_MODEL_DATA != 0 {
            unaligned_ranges_off.saturating_add(1) & !1
        } else {
            unaligned_ranges_off
        };
        let vertices_off = ranges_off.saturating_add(n_ranges.saturating_mul(HMD7_RANGE_BYTES));
        let vertices_z_off = if requested_vertex_soa {
            vertices_off.saturating_add(n_verts.saturating_mul(4))
        } else {
            vertices_off
        };
        let poses_off = vertices_off.saturating_add(n_verts.saturating_mul(6));
        let poses_len = n_frames
            .saturating_mul(n_bones)
            .saturating_mul(HMD8_AFFINE_BYTES);
        let requested_mouth = hmd_flags & HMD_FLAG_MOUTH != 0;
        let mouth_xforms_off = poses_off.saturating_add(poses_len);
        let mouth_xforms_len = if requested_mouth {
            n_frames.saturating_mul(MOUTH_XFORM_BYTES)
        } else {
            0
        };
        let requested_hitboxes = hmd_flags & HMD_FLAG_HITBOXES != 0;
        let n_hitboxes = if requested_hitboxes {
            (packed_texture_hitbox_counts >> 16) as usize
        } else {
            0
        };
        let hitboxes_off = mouth_xforms_off.saturating_add(mouth_xforms_len);
        let hitboxes_len = n_hitboxes.saturating_mul(HMD7_HITBOX_BYTES);
        let tri_off = ranges_off.saturating_add(model_data_len);
        let full_normals = hmd_flags & HMD_FLAG_I8_NORMALS != 0;
        let tri_sz = if full_normals {
            TRI_SZ_FULL_NORMALS
        } else {
            TRI_SZ
        };
        let requested_body_masks = hmd_flags & HMD_FLAG_BODY_MASKS != 0;
        let requested_packed_normals = hmd_flags & HMD_FLAG_PACKED_NORMALS != 0;
        let metadata_ok = !(requested_body_masks && requested_packed_normals)
            && !(full_normals && requested_packed_normals)
            && hitboxes_off.saturating_add(hitboxes_len) == tri_off;

        // Validate the parsed header against the actual buffer before trusting
        // any of it. The per-field reads below are unchecked (no D-cache on the
        // R3000), so a wrong-sized streamed chunk or a missing magic -- which
        // makes the counts/offsets garbage -- would dereference wild memory and
        // crash (this was c1a2a: heaviest map, first to hit it). An invalid
        // model renders as nothing rather than taking the game down.
        let tri_end = tri_off.saturating_add(n_tris.saturating_mul(tri_sz));
        let mut valid = metadata_ok
            && n_bones > 0
            && n_ranges > 0
            && n_verts <= max_verts
            && (!requested_vertex_soa || (data.as_ptr() as usize + vertices_off) & 3 == 0)
            && poses_off <= tri_off
            && tri_end <= data.len();
        let mut range = 0usize;
        while valid && range < n_ranges {
            let o = ranges_off + range * HMD7_RANGE_BYTES;
            let first = rd_u16(data, o) as usize;
            let count = rd_u16(data, o + 2) as usize;
            let bone = rd_u16(data, o + 4) as usize;
            valid = count > 0 && first.saturating_add(count) <= n_verts && bone < n_bones;
            range += 1;
        }
        let mut hitbox = 0usize;
        while valid && hitbox < n_hitboxes {
            let o = hitboxes_off + hitbox * HMD7_HITBOX_BYTES;
            valid = (rd_u16(data, o) as usize) < n_bones;
            hitbox += 1;
        }
        if !valid {
            // ponytail: null model = draws nothing. If a legit model trips this,
            // fix the cook / raise the cap rather than removing the guard.
            return Model {
                data,
                n_verts: 0,
                n_tris: 0,
                n_frames: 1,
                n_clips: 1,
                clips_off: 0,
                frame_times_off: 0,
                ranges_off: 0,
                vertices_off: 0,
                vertices_z_off: 0,
                poses_off: 0,
                n_bones: 0,
                n_ranges: 0,
                tri_off: 0,
                tri_sz,
                normal_encoding: NORMAL_NONE,
                local_to_world_q12: LOCAL_TO_WORLD_IDENTITY_Q12,
                mouth_xforms_off: 0,
                hitboxes_off: 0,
                n_hitboxes: 0,
                has_body_masks: false,
                has_mouth: false,
                has_frame_times: false,
                aligned_vertices: false,
                vertex_soa: false,
            };
        }

        Model {
            data,
            n_verts,
            n_tris,
            n_frames,
            n_clips,
            clips_off,
            frame_times_off,
            ranges_off,
            vertices_off,
            vertices_z_off,
            poses_off,
            n_bones,
            n_ranges,
            tri_off,
            tri_sz,
            normal_encoding: if full_normals {
                NORMAL_I8X3
            } else if requested_packed_normals {
                NORMAL_PACKED_555
            } else {
                NORMAL_NONE
            },
            local_to_world_q12,
            mouth_xforms_off,
            hitboxes_off,
            n_hitboxes,
            has_body_masks: requested_body_masks,
            has_mouth: requested_mouth,
            has_frame_times: requested_frame_times,
            aligned_vertices: (data.as_ptr() as usize + vertices_off) & 1 == 0,
            vertex_soa: requested_vertex_soa,
        }
    }

    #[inline]
    pub fn local_to_world_q12(&self) -> u16 {
        self.local_to_world_q12
    }

    #[inline]
    pub fn clip_len(&self, clip: usize) -> usize {
        if self.clips_off == 0 {
            return self.n_frames.max(1);
        }
        let c = clip.min(self.n_clips.saturating_sub(1));
        let o = self.clips_off + c * 4;
        ((rd_u16(self.data, o + 2) & CLIP_FRAME_COUNT_MASK) as usize).max(1)
    }

    /// GoldSrc source-sequence duration at the 20 Hz game clock. HMD8 cooks
    /// pack 100 ms quanta in ClipRec.frame_count's unused high byte and
    /// duration bit 8 in ClipRec.first_frame's unused high bit. A zero duration
    /// falls back safely instead of allowing a malformed clip to divide by zero.
    #[inline]
    pub fn clip_hold_ticks(&self, clip: usize) -> u16 {
        if self.clips_off == 0 {
            return DEFAULT_CLIP_HOLD_TICKS;
        }
        let c = clip.min(self.n_clips.saturating_sub(1));
        let o = self.clips_off + c * 4;
        let packed_first = rd_u16(self.data, o);
        let quanta = (rd_u16(self.data, o + 2) >> 8)
            | if packed_first & CLIP_DURATION_EXT_BIT != 0 {
                0x0100
            } else {
                0
            };
        if quanta == 0 {
            DEFAULT_CLIP_HOLD_TICKS
        } else {
            quanta.saturating_mul(2)
        }
    }

    #[inline]
    pub fn clip_frame(&self, clip: usize, local_frame: usize) -> usize {
        if self.clips_off == 0 {
            return local_frame % self.n_frames.max(1);
        }
        let c = clip.min(self.n_clips.saturating_sub(1));
        let o = self.clips_off + c * 4;
        let first = (rd_u16(self.data, o) & CLIP_FIRST_FRAME_MASK) as usize;
        let count = ((rd_u16(self.data, o + 2) & CLIP_FRAME_COUNT_MASK) as usize).max(1);
        (first + (local_frame % count)).min(self.n_frames.saturating_sub(1))
    }

    /// Select two retained palettes and their interpolation fraction using the
    /// exact source-frame phases emitted by the cooker. Non-uniform, motion-
    /// selected poses therefore retain their original timing instead of being
    /// stretched evenly over the clip.
    #[inline]
    fn timed_clip_phase(
        &self,
        clip: usize,
        duration: usize,
        elapsed: usize,
        looping: bool,
    ) -> (usize, usize, u32) {
        let clip = clip.min(self.n_clips.saturating_sub(1));
        let len = self.clip_len(clip);
        if len <= 1 {
            let frame = self.clip_frame(clip, 0);
            return (frame, frame, 0);
        }
        if !self.has_frame_times {
            let duration = duration.max(1);
            let span16 = if looping { len * 16 } else { (len - 1) * 16 };
            let phase16 = if looping {
                (elapsed % duration).saturating_mul(span16) / duration
            } else {
                elapsed.min(duration).saturating_mul(span16) / duration
            };
            let local = (phase16 / 16).min(len - 1);
            let next = if looping {
                (local + 1) % len
            } else {
                (local + 1).min(len - 1)
            };
            return (
                self.clip_frame(clip, local),
                self.clip_frame(clip, next),
                (phase16 & 15) as u32,
            );
        }

        let duration = duration.max(1);
        let target = if looping {
            ((elapsed % duration).saturating_mul(256) / duration).min(255)
        } else {
            (elapsed.min(duration).saturating_mul(255) / duration).min(255)
        };
        let first = self.clip_frame(clip, 0);
        let time = |local: usize| self.data[self.frame_times_off + first + local] as usize;
        let mut left = 0usize;
        while left + 1 < len && time(left + 1) <= target {
            left += 1;
        }
        let (right, left_time, right_time, target_time) = if looping && left + 1 == len {
            (0, time(left), time(0) + 256, target)
        } else {
            let right = (left + 1).min(len - 1);
            (right, time(left), time(right), target)
        };
        let denominator = right_time.saturating_sub(left_time);
        let frac16 = if denominator == 0 {
            0
        } else {
            target_time
                .saturating_sub(left_time)
                .saturating_mul(16)
                .checked_div(denominator)
                .unwrap_or(0)
                .min(15) as u32
        };
        (
            self.clip_frame(clip, left),
            self.clip_frame(clip, right),
            frac16,
        )
    }

    #[inline]
    pub fn looped_clip_phase(
        &self,
        clip: usize,
        duration: usize,
        elapsed: usize,
    ) -> (usize, usize, u32) {
        self.timed_clip_phase(clip, duration, elapsed, true)
    }

    #[inline]
    pub fn one_shot_clip_phase(
        &self,
        clip: usize,
        duration: usize,
        elapsed: usize,
    ) -> (usize, usize, u32) {
        self.timed_clip_phase(clip, duration, elapsed, false)
    }

    #[inline]
    pub fn frame(&self, frame: usize) -> ModelFrame<'_> {
        let f = if frame < self.n_frames { frame } else { 0 };
        ModelFrame {
            data: self.data,
            poses_off: self.poses_off,
            n_bones: self.n_bones,
            frame_idx: f,
            mouth_xforms_off: self.mouth_xforms_off,
            has_mouth: self.has_mouth,
        }
    }

    #[inline]
    pub fn range(&self, index: usize) -> BoneRange {
        let index = index.min(self.n_ranges.saturating_sub(1));
        let o = self.ranges_off + index * HMD7_RANGE_BYTES;
        BoneRange {
            first: rd_u16(self.data, o) as usize,
            count: rd_u16(self.data, o + 2) as usize,
            bone: rd_u16(self.data, o + 4) as usize,
            body_mask: self.data[o + 6],
            mouth: self.data[o + 7] & HMD7_RANGE_MOUTH != 0,
        }
    }

    #[inline]
    pub fn hitbox(&self, index: usize) -> StudioHitbox {
        let index = index.min(self.n_hitboxes.saturating_sub(1));
        let o = self.hitboxes_off + index * HMD7_HITBOX_BYTES;
        StudioHitbox {
            bone: rd_u16(self.data, o) as usize,
            bbmin: Vec3I16::new(
                rd_i16(self.data, o + 2),
                rd_i16(self.data, o + 4),
                rd_i16(self.data, o + 6),
            ),
            bbmax: Vec3I16::new(
                rd_i16(self.data, o + 8),
                rd_i16(self.data, o + 10),
                rd_i16(self.data, o + 12),
            ),
        }
    }

    #[inline]
    pub fn vert(&self, index: usize) -> Vec3I16 {
        if self.vertex_soa {
            let xy = self.vertices_off + index * 4;
            let z = self.vertices_z_off + index * 2;
            return Vec3I16::new(
                rd_i16(self.data, xy),
                rd_i16(self.data, xy + 2),
                rd_i16(self.data, z),
            );
        }
        #[allow(unused_variables)] // read only by the MIPS path below
        let o = self.vertices_off + index * 6;
        #[cfg(target_arch = "mips")]
        if self.aligned_vertices {
            unsafe {
                // New HMD8 cooks halfword-align this six-byte stream. Native
                // LH loads replace six LBU operations plus their shifts/ORs in
                // the per-RTPT model projection loop. The fallback below keeps
                // an old cached HMD8 blob playable without requiring a recook.
                let p = self.data.as_ptr().add(o).cast::<i16>();
                return Vec3I16::new(p.read(), p.add(1).read(), p.add(2).read());
            }
        }
        Vec3I16::new(
            rd_i16(self.data, o),
            rd_i16(self.data, o + 2),
            rd_i16(self.data, o + 4),
        )
    }

    #[inline]
    pub fn vert_gte_words(&self, index: usize) -> GteVertexWords {
        if self.vertex_soa {
            let xy = self.vertices_off + index * 4;
            let z = self.vertices_z_off + index * 2;
            #[cfg(target_arch = "mips")]
            unsafe {
                return GteVertexWords {
                    // `data` is a byte slice, so a normal raw read can retain
                    // align=1 in LLVM even after the cast and become LWL/LWR.
                    // HMD_FLAG_VERTEX_SOA is validated against the actual base
                    // address at load, making these native aligned loads safe.
                    xy: core::ptr::read_volatile(self.data.as_ptr().add(xy).cast::<u32>()),
                    z: core::ptr::read_volatile(self.data.as_ptr().add(z).cast::<i16>()) as i32
                        as u32,
                };
            }
            #[cfg(not(target_arch = "mips"))]
            return GteVertexWords {
                xy: rd_u32(self.data, xy),
                z: rd_i16(self.data, z) as i32 as u32,
            };
        }
        #[allow(unused_variables)] // read only by the MIPS path below
        let o = self.vertices_off + index * 6;
        #[cfg(target_arch = "mips")]
        unsafe {
            let p = self.data.as_ptr().add(o);
            let xy = p.cast::<u32>().read_unaligned();
            let z = if self.aligned_vertices {
                p.add(4).cast::<i16>().read()
            } else {
                rd_i16(self.data, o + 4)
            };
            return GteVertexWords {
                xy,
                z: z as i32 as u32,
            };
        }
        #[cfg(not(target_arch = "mips"))]
        {
            let v = self.vert(index);
            GteVertexWords {
                xy: ((v.y as u16 as u32) << 16) | v.x as u16 as u32,
                z: v.z as i32 as u32,
            }
        }
    }

    #[inline]
    /// Byte length of the header + clips + ranges + static vertices + palettes --
    /// everything the per-frame draw needs. The TriRec/texture tail after it is
    /// only read by `fill_render_faces_raw`/`tri()` (load-time or viewmodels),
    /// so the enemy pool can drop it once the faces are baked.
    pub fn frame_section_len(&self) -> usize {
        self.tri_off
    }

    /// Compact a human HMD8 stream to the body values used by the current map.
    /// Whole bone/body ranges and their static vertices are filtered; the
    /// shared pose palette and mouth transforms move down unchanged. The
    /// triangle tail is deliberately discarded by the caller after face bake.
    ///
    /// Returns the compact frame-section length, or `None` for malformed input.
    pub unsafe fn compact_visible_body_frames_raw(
        &self,
        data: *mut u8,
        data_len: usize,
        visible_bodies: u8,
        remap: *mut u16,
        remap_len: usize,
    ) -> Option<usize> {
        // SAFETY: contract is the enclosing fn's; see its doc comment.
        unsafe {
            if !self.has_body_masks
                || self.n_verts == 0
                || self.n_verts > remap_len
                || self.tri_off > data_len
            {
                return None;
            }

            let old_n = self.n_verts;
            let mut new_n = 0usize;
            ptr::write_bytes(remap, 0xff, old_n);
            let mut new_ranges = 0usize;
            for ri in 0..self.n_ranges {
                let range = self.range(ri);
                if range.body_mask & visible_bodies == 0 {
                    continue;
                }
                let dst_range = self.ranges_off + new_ranges * HMD7_RANGE_BYTES;
                ptr::copy(
                    data.add(self.ranges_off + ri * HMD7_RANGE_BYTES),
                    data.add(dst_range),
                    HMD7_RANGE_BYTES,
                );
                ptr::copy_nonoverlapping(
                    (new_n as u16).to_le_bytes().as_ptr(),
                    data.add(dst_range),
                    2,
                );
                for old in range.first..range.first + range.count {
                    ptr::write(remap.add(old), new_n as u16);
                    new_n += 1;
                }
                new_ranges += 1;
            }
            if new_n == 0 || new_ranges == 0 {
                return None;
            }

            let new_vertices_off = self.ranges_off + new_ranges * HMD7_RANGE_BYTES;
            let mut dst;
            if self.vertex_soa {
                // Copy the hot XY stream before the old Z stream can be
                // overwritten, then compact Z immediately after the retained XY.
                // The total remains exactly six bytes per vertex.
                let new_vertices_z_off = new_vertices_off + new_n * 4;
                let mut new = 0usize;
                for old in 0..old_n {
                    if ptr::read(remap.add(old)) != u16::MAX {
                        ptr::copy(
                            data.add(self.vertices_off + old * 4),
                            data.add(new_vertices_off + new * 4),
                            4,
                        );
                        new += 1;
                    }
                }
                new = 0;
                for old in 0..old_n {
                    if ptr::read(remap.add(old)) != u16::MAX {
                        ptr::copy(
                            data.add(self.vertices_z_off + old * 2),
                            data.add(new_vertices_z_off + new * 2),
                            2,
                        );
                        new += 1;
                    }
                }
                dst = new_vertices_z_off + new_n * 2;
            } else {
                dst = new_vertices_off;
                for old in 0..old_n {
                    if ptr::read(remap.add(old)) != u16::MAX {
                        ptr::copy(data.add(self.vertices_off + old * 6), data.add(dst), 6);
                        dst += 6;
                    }
                }
            }
            let pose_len = self
                .n_frames
                .checked_mul(self.n_bones)?
                .checked_mul(HMD8_AFFINE_BYTES)?;
            if self.poses_off.checked_add(pose_len)? > self.tri_off
                || dst.checked_add(pose_len)? > data_len
            {
                return None;
            }
            ptr::copy(data.add(self.poses_off), data.add(dst), pose_len);
            dst += pose_len;
            if self.has_mouth {
                let xform_len = self.n_frames.checked_mul(MOUTH_XFORM_BYTES)?;
                if self.mouth_xforms_off.checked_add(xform_len)? > self.tri_off
                    || dst.checked_add(xform_len)? > data_len
                {
                    return None;
                }
                ptr::copy(data.add(self.mouth_xforms_off), data.add(dst), xform_len);
                dst += xform_len;
            }
            if self.n_hitboxes != 0 {
                let hitbox_len = self.n_hitboxes.checked_mul(HMD7_HITBOX_BYTES)?;
                if self.hitboxes_off.checked_add(hitbox_len)? > self.tri_off
                    || dst.checked_add(hitbox_len)? > data_len
                {
                    return None;
                }
                ptr::copy(data.add(self.hitboxes_off), data.add(dst), hitbox_len);
                dst += hitbox_len;
            }
            let model_data_len = dst.checked_sub(self.ranges_off)?;
            if model_data_len > u32::MAX as usize || new_n > u32::MAX as usize {
                return None;
            }
            ptr::copy_nonoverlapping((new_n as u32).to_le_bytes().as_ptr(), data.add(4), 4);
            ptr::copy_nonoverlapping(
                (model_data_len as u32).to_le_bytes().as_ptr(),
                data.add(24),
                4,
            );
            ptr::copy_nonoverlapping((new_ranges as u16).to_le_bytes().as_ptr(), data.add(34), 2);
            Some(dst)
        }
    }

    #[inline(always)]
    pub fn tri(&self, t: usize) -> Tri {
        let o = self.tri_off + t * self.tri_sz;
        let d = self.data;
        // Model::new validated the complete triangle table. Raw byte reads
        // avoid Rust's unsafe-precondition guards, which current nightly still
        // emits for every `get_unchecked`/slice index in this per-face hot loop.
        let p = unsafe { d.as_ptr().add(o) };
        Tri {
            idx: [rd_u16(d, o), rd_u16(d, o + 2), rd_u16(d, o + 4)],
            tex: rd_u16(d, o + 6) as usize,
            uv: [
                unsafe { (p.add(8).read(), p.add(9).read()) },
                unsafe { (p.add(10).read(), p.add(11).read()) },
                unsafe { (p.add(12).read(), p.add(13).read()) },
            ],
            normal: match self.normal_encoding {
                NORMAL_I8X3 => unsafe {
                    [
                        p.add(14).read() as i8,
                        p.add(15).read() as i8,
                        p.add(16).read() as i8,
                    ]
                },
                NORMAL_PACKED_555 => unpack_normal_555(rd_u16(d, o + 14)),
                _ => [0; 3],
            },
            body_mask: if self.has_body_masks {
                unsafe {
                    p.add(if self.normal_encoding == NORMAL_I8X3 {
                        17
                    } else {
                        14
                    })
                    .read()
                }
            } else {
                0xff
            },
        }
    }

    /// Packet-order UV words for a triangle whose other steady-render fields
    /// are already in the viewmodel pose cache. The triangle table was fully
    /// validated by `Model::new`, so these raw halfword reads are exact.
    #[inline(always)]
    pub fn tri_uv_words(&self, t: usize) -> [u16; 3] {
        let o = self.tri_off + t * self.tri_sz + 8;
        [
            rd_u16(self.data, o),
            rd_u16(self.data, o + 2),
            rd_u16(self.data, o + 4),
        ]
    }

    #[inline]
    pub fn has_body_masks(&self) -> bool {
        self.has_body_masks
    }

    #[inline]
    pub fn tri_normal(&self, t: usize) -> [i8; 3] {
        let o = self.tri_off + t * self.tri_sz + 14;
        let d = self.data;
        match self.normal_encoding {
            NORMAL_I8X3 => [d[o] as i8, d[o + 1] as i8, d[o + 2] as i8],
            NORMAL_PACKED_555 => unpack_normal_555(rd_u16(d, o)),
            _ => [0; 3],
        }
    }

    /// Number of consecutive texture/body-mask runs in authored face order.
    pub fn render_face_count(&self, visible_bodies: u8) -> usize {
        let mut count = 0usize;
        let mut t = 0usize;
        while t < self.n_tris {
            if self.tri(t).body_mask & visible_bodies != 0 {
                count += 1;
            }
            t += 1;
        }
        count
    }

    pub fn render_texture_mask(&self, visible_bodies: u8) -> u32 {
        let mut mask = 0u32;
        let mut t = 0usize;
        while t < self.n_tris {
            let tri = self.tri(t);
            if tri.body_mask & visible_bodies != 0 && tri.tex < 32 {
                mask |= 1u32 << tri.tex;
            }
            t += 1;
        }
        mask
    }

    pub fn render_face_run_count(&self, visible_bodies: u8) -> usize {
        let mut runs = 0usize;
        let mut last_tex = usize::MAX;
        let mut last_mask = 0u8;
        let mut t = 0usize;
        while t < self.n_tris {
            let tri = self.tri(t);
            if tri.body_mask & visible_bodies == 0 {
                t += 1;
                continue;
            }
            if tri.tex != last_tex || tri.body_mask != last_mask {
                runs += 1;
                last_tex = tri.tex;
                last_mask = tri.body_mask;
            }
            t += 1;
        }
        runs
    }

    /// Split GPU face records into a hot packed-index stream, a cold UV stream,
    /// and ordered texture runs. Each run word is
    /// `end_face:u16 | tex:u8<<16 | body_mask:u8<<24`; `end_face` is relative
    /// to this model. The caller guarantees enough face and run storage.
    pub unsafe fn fill_render_faces_split_raw(
        &self,
        indices: *mut u32,
        payloads: *mut RenderFacePayload,
        runs: *mut u32,
        out_len: usize,
        visible_bodies: u8,
    ) -> (usize, usize) {
        // SAFETY: contract is the enclosing fn's; see its doc comment.
        unsafe {
            if self.n_tris == 0 || out_len == 0 {
                return (0, 0);
            }
            let mut source_t = 0usize;
            let mut out_t = 0usize;
            let mut run_count = 0usize;
            let mut run_tex = usize::MAX;
            let mut run_mask = 0u8;
            while source_t < self.n_tris && out_t < out_len {
                let a = self.tri(source_t);
                source_t += 1;
                if a.body_mask & visible_bodies == 0 {
                    continue;
                }
                if a.tex != run_tex || a.body_mask != run_mask {
                    if run_count != 0 {
                        ptr::write(
                            runs.add(run_count - 1),
                            (out_t as u32)
                                | ((run_tex.min(u8::MAX as usize) as u32) << 16)
                                | ((run_mask as u32) << 24),
                        );
                    }
                    run_tex = a.tex;
                    run_mask = a.body_mask;
                    run_count += 1;
                }
                write_render_face(indices, payloads, out_t, &a);
                out_t += 1;
            }
            if run_count == 0 {
                return (0, 0);
            }
            ptr::write(
                runs.add(run_count - 1),
                (out_t as u32)
                    | ((run_tex.min(u8::MAX as usize) as u32) << 16)
                    | ((run_mask as u32) << 24),
            );
            (out_t, run_count)
        }
    }
}

impl<'a> ModelFrame<'a> {
    #[inline]
    pub fn interpolate(self, b: Self, frac16: u32) -> InterpolatedModelFrame<'a> {
        InterpolatedModelFrame {
            a: self,
            b,
            frac16: frac16 as i32,
        }
    }

    #[inline]
    fn bone(&self, bone: usize) -> BoneTransform {
        let bone = bone.min(self.n_bones.saturating_sub(1));
        let o = self.poses_off + (self.frame_idx * self.n_bones + bone) * HMD8_AFFINE_BYTES;
        let p0 = rd_q11_pair(self.data, o);
        let p1 = rd_q11_pair(self.data, o + 3);
        let p2 = rd_q11_pair(self.data, o + 6);
        let p3 = rd_q11_pair(self.data, o + 9);
        let last = decode_q11(rd_u16(self.data, o + 12) & 0x0fff);
        BoneTransform {
            rotation: Mat3I16 {
                m: [[p0.0, p0.1, p1.0], [p1.1, p2.0, p2.1], [p3.0, p3.1, last]],
            },
            translation: Vec3I16::new(
                rd_i16(self.data, o + 14),
                rd_i16(self.data, o + 16),
                rd_i16(self.data, o + 18),
            ),
        }
    }

    #[inline]
    fn mouth_xform(&self) -> BoneTransform {
        let o = self.mouth_xforms_off + self.frame_idx * MOUTH_XFORM_BYTES;
        BoneTransform {
            rotation: Mat3I16 {
                m: [
                    [
                        rd_i16(self.data, o),
                        rd_i16(self.data, o + 2),
                        rd_i16(self.data, o + 4),
                    ],
                    [
                        rd_i16(self.data, o + 6),
                        rd_i16(self.data, o + 8),
                        rd_i16(self.data, o + 10),
                    ],
                    [
                        rd_i16(self.data, o + 12),
                        rd_i16(self.data, o + 14),
                        rd_i16(self.data, o + 16),
                    ],
                ],
            },
            translation: Vec3I16::new(
                rd_i16(self.data, o + 18),
                rd_i16(self.data, o + 20),
                rd_i16(self.data, o + 22),
            ),
        }
    }

    fn bone_with_mouth(&self, bone: usize, mouth_range: bool, mouth: u8) -> BoneTransform {
        let base = self.bone(bone);
        if mouth == 0 || !mouth_range || !self.has_mouth {
            return base;
        }
        let amount = mouth.min(64) as i32;
        let open = self.mouth_xform();
        let mut relative = BoneTransform {
            rotation: Mat3I16::IDENTITY,
            translation: Vec3I16::ZERO,
        };
        for row in 0..3 {
            for column in 0..3 {
                let identity = if row == column { 4096 } else { 0 };
                relative.rotation.m[row][column] =
                    (identity + ((open.rotation.m[row][column] as i32 - identity) * amount >> 6))
                        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        }
        relative.translation = Vec3I16::new(
            ((open.translation.x as i32 * amount) >> 6) as i16,
            ((open.translation.y as i32 * amount) >> 6) as i16,
            ((open.translation.z as i32 * amount) >> 6) as i16,
        );
        compose_affine(relative, base)
    }
}

impl InterpolatedModelFrame<'_> {
    #[inline(always)]
    fn generic_component(a: i16, b: i16, frac16: i32) -> i16 {
        a.wrapping_add((((b as i32 - a as i32) * frac16) >> 4) as i16)
    }

    #[inline(never)]
    pub fn bone(&self, bone: usize, mouth_range: bool, mouth: u8) -> BoneTransform {
        let a = self.a.bone_with_mouth(bone, mouth_range, mouth);
        if self.frac16 == 0 || self.a.frame_idx == self.b.frame_idx {
            return a;
        }
        let b = self.b.bone_with_mouth(bone, mouth_range, mouth);
        let mut rotation = Mat3I16::ZERO;
        for row in 0..3 {
            for column in 0..3 {
                rotation.m[row][column] = Self::generic_component(
                    a.rotation.m[row][column],
                    b.rotation.m[row][column],
                    self.frac16,
                );
            }
        }
        BoneTransform {
            rotation,
            translation: Vec3I16::new(
                Self::generic_component(a.translation.x, b.translation.x, self.frac16),
                Self::generic_component(a.translation.y, b.translation.y, self.frac16),
                Self::generic_component(a.translation.z, b.translation.z, self.frac16),
            ),
        }
    }
}

#[inline(never)]
fn compose_affine(parent: BoneTransform, child: BoneTransform) -> BoneTransform {
    let rotation = parent.rotation.mul(&child.rotation);
    let transformed = parent.rotation.transform(child.translation);
    BoneTransform {
        rotation,
        translation: Vec3I16::new(
            transformed[0]
                .saturating_add(parent.translation.x as i32)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            transformed[1]
                .saturating_add(parent.translation.y as i32)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            transformed[2]
                .saturating_add(parent.translation.z as i32)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_q11, rd_q11_pair, valid_local_to_world_q12};

    fn reference_decode_q11(raw: u16) -> i16 {
        let code = if raw & 0x0800 != 0 {
            (raw | 0xf000) as i16
        } else {
            raw as i16
        };
        if code == 2047 {
            4096
        } else {
            code << 1
        }
    }

    #[test]
    fn q11_decode_matches_every_packed_code() {
        for raw in 0..=0x0fff {
            assert_eq!(decode_q11(raw), reference_decode_q11(raw), "{raw:#05x}");
        }
    }

    #[test]
    fn unaligned_pair_read_ignores_the_following_byte() {
        let a = 0x07ffu32;
        let b = 0x0800u32;
        let packed = a | (b << 12) | (0xa5 << 24);
        let bytes = packed.to_le_bytes();
        assert_eq!(
            rd_q11_pair(&bytes, 0),
            (
                reference_decode_q11(a as u16),
                reference_decode_q11(b as u16)
            )
        );
    }

    #[test]
    fn model_scale_header_accepts_only_exact_power_of_two_q12_scales() {
        for valid in [0, 256, 512, 1024, 2048, 4096] {
            assert!(valid_local_to_world_q12(valid));
        }
        for invalid in [1, 255, 257, 511, 513, 1000, 4095, 4097, u16::MAX] {
            assert!(!valid_local_to_world_q12(invalid));
        }
    }
}
