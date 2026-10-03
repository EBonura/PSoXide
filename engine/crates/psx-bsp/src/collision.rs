//! Caller-owned PXBSP collision-hull traversal: point contents and segment
//! traces through a hull of split planes, with bounded, allocation-free
//! scratch and integer-only arithmetic.
//!
//! PXBSP positions are Y-up Q20.12 and plane normals are Q3.12. The
//! contents codes below are the cooked format's leaf values.

use core::cmp::Ordering;
use core::mem::MaybeUninit;

use crate::{
    ClipNode, CompactPlane, CookedRecord, Leaf, Node, Plane, RecordSlice, Vec3I16, Vec3I32,
};
use psx_gte::math::Mat3I16;
use psx_math::int32::{div_u64_by_u32, mul_q12_i32, mul_q12_i32_wide};

pub const CONTENTS_EMPTY: i16 = -1;
pub const CONTENTS_SOLID: i16 = -2;
pub const CONTENTS_WATER: i16 = -3;
pub const CONTENTS_SLIME: i16 = -4;
pub const CONTENTS_LAVA: i16 = -5;
pub const CONTENTS_SKY: i16 = -6;
pub const Q12_ONE: i32 = 4096;
/// How far a trace stops short of the plane it hits, on the start side,
/// in Q20.12: 1/32 of a unit. Without it a contact rounded onto the plane
/// (or a hair past it) would start the next trace inside solid; the margin
/// is a few times the rounding of a Q0.12 fraction over typical moves and
/// far below anything visible.
pub const TRACE_PLANE_EPSILON_Q12: i32 = 128;
pub const TRACE_STACK_CAPACITY: usize = 64;

/// Result of sampling an ordered feet-to-head point sequence through a hull.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LiquidContentsSample {
    /// Strongest sampled liquid code: lava, slime, water, then empty.
    pub contents: i16,
    /// One-based index of the highest sample in any liquid, or zero.
    pub water_level: u8,
}

impl Default for LiquidContentsSample {
    fn default() -> Self {
        Self {
            contents: CONTENTS_EMPTY,
            water_level: 0,
        }
    }
}

/// Rigid local-to-world transform shared by brush rendering and collision.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrushTransform {
    /// Q20.12 world position of model-local zero.
    pub origin: Vec3I32,
    /// Q3.12 model-local to world rotation.
    pub rotation: Mat3I16,
}

impl BrushTransform {
    pub const IDENTITY: Self = Self {
        origin: Vec3I32 { x: 0, y: 0, z: 0 },
        rotation: Mat3I16::IDENTITY,
    };

    pub const fn translated(origin: Vec3I32) -> Self {
        Self {
            origin,
            rotation: Mat3I16::IDENTITY,
        }
    }

    /// Transform one Q20.12 world point into model-local coordinates.
    pub fn point_to_local(self, point: Vec3I32) -> Vec3I32 {
        inverse_rotate_q12(
            self.rotation,
            Vec3I32 {
                x: point.x.saturating_sub(self.origin.x),
                y: point.y.saturating_sub(self.origin.y),
                z: point.z.saturating_sub(self.origin.z),
            },
        )
    }

    /// Transform one Q20.12 model-local point into world coordinates.
    pub fn point_to_world(self, point: Vec3I32) -> Vec3I32 {
        let rotated = Vec3I32 {
            x: q12_dot(self.rotation.m[0], point),
            y: q12_dot(self.rotation.m[1], point),
            z: q12_dot(self.rotation.m[2], point),
        };
        Vec3I32 {
            x: rotated.x.saturating_add(self.origin.x),
            y: rotated.y.saturating_add(self.origin.y),
            z: rotated.z.saturating_add(self.origin.z),
        }
    }
}

/// One byte-backed flag slot in a caller-owned [`Trace`].
///
/// [`CollisionHull::trace_into`] guarantees that a failed trace leaves every
/// output byte, padding included, exactly as the caller left it. The caller
/// therefore hands this boundary whatever bytes its storage already held, and
/// those bytes are legal by construction. A `bool` field would make any byte
/// other than 0 or 1 undefined behaviour the instant the struct was formed or
/// copied, before a single branch on it ran, and no later read could repair
/// that: the invalid value would already exist.
///
/// Every one of the 256 byte patterns is a valid `TraceFlag`, so the invalid
/// value cannot be created. [`TraceFlag::is_set`] is the only way to ask what a
/// slot means, and it normalizes any non-zero byte to `true` at the point a
/// `bool` is first constructed.
///
/// Equality is byte equality, not meaning equality, because the byte-preserving
/// contract is what callers assert against. The tracer itself only ever writes
/// [`TraceFlag::CLEAR`] or [`TraceFlag::SET`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct TraceFlag(u8);

impl TraceFlag {
    /// The flag is not set. This is also the [`Default`].
    pub const CLEAR: Self = Self(0);
    /// The flag is set, in the canonical byte the tracer writes.
    pub const SET: Self = Self(1);

    /// The flag a Rust `bool` denotes.
    pub const fn new(value: bool) -> Self {
        if value {
            Self::SET
        } else {
            Self::CLEAR
        }
    }

    /// Adopt one arbitrary byte. Every byte is a valid flag.
    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    /// The stored byte, unnormalized. Useful only for byte-level assertions.
    pub const fn byte(self) -> u8 {
        self.0
    }

    /// Normalize the stored byte into a valid `bool`.
    pub const fn is_set(self) -> bool {
        self.0 != 0
    }
}

impl From<bool> for TraceFlag {
    fn from(value: bool) -> Self {
        Self::new(value)
    }
}

impl From<TraceFlag> for bool {
    fn from(flag: TraceFlag) -> Self {
        flag.is_set()
    }
}

/// Result of one segment trace through a hull.
///
/// `fraction` (Q0.12, `0..=4096`) and `end` give how far along the segment
/// the trace got: the whole way, or to just before the first point where it
/// passes from open space into solid. `normal` and `plane_distance` describe
/// that contact plane turned to face the segment start, and stay zero when
/// there is no contact.
///
/// The flags say what the segment passed through: `start_solid` that its
/// start point is in solid, `all_solid` that no part of it is in open space,
/// `in_open` that it touched empty space and `in_water` that it touched a
/// liquid (water, slime or lava).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct Trace {
    pub fraction: i32,
    pub end: Vec3I32,
    pub plane_distance: i32,
    pub normal: Vec3I16,
    pub all_solid: TraceFlag,
    pub start_solid: TraceFlag,
    pub in_open: TraceFlag,
    pub in_water: TraceFlag,
}

impl Trace {
    /// A trace that reached `end` without contact. Every flag but
    /// `all_solid` starts clear; the first open leaf clears that one.
    const fn reaching(end: Vec3I32) -> Self {
        Self {
            fraction: Q12_ONE,
            end,
            plane_distance: 0,
            normal: Vec3I16 { x: 0, y: 0, z: 0 },
            all_solid: TraceFlag::SET,
            start_solid: TraceFlag::CLEAR,
            in_open: TraceFlag::CLEAR,
            in_water: TraceFlag::CLEAR,
        }
    }
}

/// A position along the traced segment, `num / den` in `[0, 1]`.
///
/// Every split point is the ratio of two plane distances of the segment's
/// own end points, so it is kept as that exact ratio rather than rounded:
/// pieces of the segment never drift off the planes that bound them.
#[derive(Copy, Clone)]
struct SegmentParam {
    num: u32,
    den: u32,
}

impl SegmentParam {
    const START: Self = Self { num: 0, den: 1 };
    const END: Self = Self { num: 1, den: 1 };

    /// Where a segment whose end points lie `ds` and `de` from a plane, on
    /// opposite sides, meets it: `|ds| / (|ds| + |de|)`.
    #[inline(always)]
    fn crossing(ds: i32, de: i32) -> Self {
        let near = ds.unsigned_abs();
        let far = de.unsigned_abs();
        match near.checked_add(far) {
            Some(den) => Self { num: near, den },
            // Only distances near the `i32` limits get here; halving both
            // keeps the ratio within one part in 2^31.
            None => Self {
                num: near >> 1,
                den: (near >> 1) + (far >> 1),
            },
        }
    }

    /// Exact ordering of two parameters: one 32x32 product per side.
    #[inline(always)]
    fn order(self, other: Self) -> Ordering {
        if self.den == other.den {
            return self.num.cmp(&other.num);
        }
        // psx-numeric-allow-next-line: R3000 MULTU gives the full 64-bit product of two u32 values; only compared, never divided
        let left = u64::from(self.num) * u64::from(other.den);
        // psx-numeric-allow-next-line: as above
        let right = u64::from(other.num) * u64::from(self.den);
        left.cmp(&right)
    }
}

/// One end of a piece of the segment: a parameter, and whether the piece
/// stops just short of it (`open`) or includes it.
#[derive(Copy, Clone)]
struct PieceEnd {
    at: SegmentParam,
    open: bool,
}

impl PieceEnd {
    /// The tighter of two lower ends.
    #[inline(always)]
    fn later(self, other: Self) -> Self {
        match self.at.order(other.at) {
            Ordering::Greater => self,
            Ordering::Less => other,
            Ordering::Equal => Self {
                at: self.at,
                open: self.open || other.open,
            },
        }
    }

    /// The tighter of two upper ends.
    #[inline(always)]
    fn earlier(self, other: Self) -> Self {
        match self.at.order(other.at) {
            Ordering::Less => self,
            Ordering::Greater => other,
            Ordering::Equal => Self {
                at: self.at,
                open: self.open || other.open,
            },
        }
    }

    /// Whether no parameter lies between `self` (lower) and `upper`.
    #[inline(always)]
    fn empty_up_to(self, upper: Self) -> bool {
        match self.at.order(upper.at) {
            Ordering::Greater => true,
            Ordering::Equal => self.open || upper.open,
            Ordering::Less => false,
        }
    }
}

/// No entry plane: the piece starts at the segment start.
const NO_ENTRY_PLANE: u16 = u16::MAX;

/// A stretch of the segment still to be walked, from `node` down.
#[derive(Copy, Clone)]
struct PendingPiece {
    lower: SegmentParam,
    upper: SegmentParam,
    /// Plane the piece starts on ([`NO_ENTRY_PLANE`] at the segment start).
    entry_plane: u16,
    node: i16,
    /// Side of `entry_plane` the segment start is on: 0 front, 1 back.
    entry_side: u8,
    /// Bit 0: the lower end is excluded. Bit 1: the upper end is excluded.
    open_ends: u8,
}

impl PendingPiece {
    #[inline(always)]
    fn lower_end(&self) -> PieceEnd {
        PieceEnd {
            at: self.lower,
            open: self.open_ends & 1 != 0,
        }
    }

    #[inline(always)]
    fn upper_end(&self) -> PieceEnd {
        PieceEnd {
            at: self.upper,
            open: self.open_ends & 2 != 0,
        }
    }

    #[inline(always)]
    fn set_ends(&mut self, lower: PieceEnd, upper: PieceEnd) {
        self.lower = lower.at;
        self.upper = upper.at;
        self.open_ends = u8::from(lower.open) | (u8::from(upper.open) << 1);
    }
}

/// Caller-owned workspace for one allocation-free BSP hull trace.
///
/// The trace walks the segment front to back and defers the far piece of
/// every plane it splits on; the scratch holds at most
/// [`TRACE_STACK_CAPACITY`] deferred pieces. A trace that needs one more
/// returns `false`; the scratch stays reusable and the caller's output is
/// not modified.
pub struct TraceScratch {
    pieces: [MaybeUninit<PendingPiece>; TRACE_STACK_CAPACITY],
}

impl TraceScratch {
    pub const fn new() -> Self {
        Self {
            pieces: [MaybeUninit::uninit(); TRACE_STACK_CAPACITY],
        }
    }
}

impl Default for TraceScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a hull's split nodes live.
///
/// Hull 0 (point traces) is served from the render BSP itself: the same
/// balanced tree the renderer walks, with leaf records carrying contents.
/// The cooked clipnode chains are a per-brush plane list, so a point
/// location there costs O(brushes) and a long trace pays that at every
/// straddle; on a 460-brush map that was ~1.4M cycles per camera solve.
#[derive(Copy, Clone)]
enum HullNodes<'a> {
    Clip(&'a [ClipNode]),
    Render {
        nodes: RecordSlice<'a, Node>,
        leaves: RecordSlice<'a, Leaf>,
    },
}

/// Where a hull's planes live.
///
/// Guests only ever trace resident, load-validated hulls, so on the target
/// this has the one compact variant and the walker is instantiated once per
/// node layout. The cooker-layout variant exists for host tools and tests,
/// which also exercise the malformed-data paths.
#[derive(Copy, Clone)]
enum PlaneRecords<'a> {
    Compact(&'a [CompactPlane]),
    #[cfg(not(target_arch = "mips"))]
    Packed(RecordSlice<'a, Plane>),
}

/// One node representation, resolved at compile time.
///
/// The walker is generic over this and [`PlaneSource`] so each hull layout a
/// binary uses gets its own copy of the descent loop with no per-node
/// dispatch: the enum form paid two representation branches, a stack round
/// trip for the six-byte node and three more for the plane on every visit
/// (about 50 instructions a node against 25 here).
trait NodeSource: Copy {
    fn len(self) -> usize;
    /// Splitter plane index of node `index`.
    ///
    /// # Safety
    /// `index < self.len()`.
    unsafe fn plane_unchecked(self, index: usize) -> usize;
    /// Child `side` (0 front, 1 back) of node `index`.
    ///
    /// # Safety
    /// `index < self.len()`, `side < 2`.
    unsafe fn child_unchecked(self, index: usize, side: usize) -> i16;
    /// Resolve a negative child to contents.
    fn contents(self, child: i16) -> Option<i16>;
}

impl NodeSource for &[ClipNode] {
    #[inline(always)]
    fn len(self) -> usize {
        <[ClipNode]>::len(self)
    }

    #[inline(always)]
    unsafe fn plane_unchecked(self, index: usize) -> usize {
        unsafe { self.get_unchecked(index).plane as u16 as usize }
    }

    #[inline(always)]
    unsafe fn child_unchecked(self, index: usize, side: usize) -> i16 {
        unsafe { *self.get_unchecked(index).children.get_unchecked(side) }
    }

    /// Clipnodes store contents directly.
    #[inline(always)]
    fn contents(self, child: i16) -> Option<i16> {
        Some(child)
    }
}

/// Render BSP nodes: sixteen-byte records whose first three halfwords are
/// the plane and the two children, with leaf contents in the leaf lump.
#[derive(Copy, Clone)]
struct RenderNodes<'a> {
    nodes: RecordSlice<'a, Node>,
    leaves: RecordSlice<'a, Leaf>,
}

impl NodeSource for RenderNodes<'_> {
    #[inline(always)]
    fn len(self) -> usize {
        self.nodes.len()
    }

    #[inline(always)]
    unsafe fn plane_unchecked(self, index: usize) -> usize {
        // Record lumps are four-byte aligned with sixteen-byte records, so
        // these are aligned halfword loads.
        unsafe {
            let base = self.nodes.as_bytes().as_ptr().add(index * Node::SIZE);
            u16::from_le(core::ptr::read(base.cast::<u16>())) as usize
        }
    }

    #[inline(always)]
    unsafe fn child_unchecked(self, index: usize, side: usize) -> i16 {
        unsafe {
            let base = self
                .nodes
                .as_bytes()
                .as_ptr()
                .add(index * Node::SIZE + 2 + side * 2);
            i16::from_le(core::ptr::read(base.cast::<i16>()))
        }
    }

    /// Render nodes store `-(leaf + 1)`; the leaf's contents byte leads its
    /// record.
    #[inline(always)]
    fn contents(self, child: i16) -> Option<i16> {
        let leaf = (-1i32 - child as i32) as usize;
        self.leaves
            .record_bytes(leaf)
            .map(|bytes| bytes[0] as i8 as i16)
    }
}

/// One plane representation, resolved at compile time. See [`NodeSource`].
trait PlaneSource: Copy {
    /// Whether the walker must range-check node and plane indices. Compact
    /// planes only ever come with load-validated node lumps (see
    /// [`CollisionHull::from_native_clip_nodes`]), so their walk trusts every
    /// index; cooker-layout records are checked and fail closed.
    const CHECKED: bool;
    fn len(self) -> usize;
    /// Signed Q20.12 distances of two points from splitter `index`.
    ///
    /// Axial planes (kinds 0..3, the vast majority in a brush level) index
    /// the point directly; only the general kind pays the three multiplies.
    ///
    /// # Safety
    /// `index < self.len()`.
    unsafe fn distances_unchecked(self, index: usize, a: &[i32; 3], b: &[i32; 3]) -> (i32, i32);
    fn get(self, index: usize) -> Option<Plane>;
}

/// `dot(normal, point) - distance` for a general plane; Q20.12 points against
/// Q3.12 unit normals keep every product and the sum inside `i32` on
/// validated hulls, so the wide multiply and wrapping adds are exact.
#[inline(always)]
fn general_distance(normal: [i32; 3], distance: i32, point: &[i32; 3]) -> i32 {
    mul_q12_i32_wide(point[0], normal[0])
        .wrapping_add(mul_q12_i32_wide(point[1], normal[1]))
        .wrapping_add(mul_q12_i32_wide(point[2], normal[2]))
        .wrapping_sub(distance)
}

#[inline(always)]
fn axial_or_general(
    kind: usize,
    normal: impl FnOnce() -> [i32; 3],
    distance: i32,
    a: &[i32; 3],
    b: &[i32; 3],
) -> (i32, i32) {
    if kind < 3 {
        // `kind < 3` was just tested.
        unsafe {
            (
                a.get_unchecked(kind).wrapping_sub(distance),
                b.get_unchecked(kind).wrapping_sub(distance),
            )
        }
    } else {
        let normal = normal();
        (
            general_distance(normal, distance, a),
            general_distance(normal, distance, b),
        )
    }
}

impl PlaneSource for &[CompactPlane] {
    const CHECKED: bool = false;

    #[inline(always)]
    fn len(self) -> usize {
        <[CompactPlane]>::len(self)
    }

    #[inline(always)]
    unsafe fn distances_unchecked(self, index: usize, a: &[i32; 3], b: &[i32; 3]) -> (i32, i32) {
        let plane = unsafe { self.get_unchecked(index) };
        axial_or_general(
            plane.kind as usize,
            || {
                [
                    plane.normal.x as i32,
                    plane.normal.y as i32,
                    plane.normal.z as i32,
                ]
            },
            plane.distance,
            a,
            b,
        )
    }

    #[inline(always)]
    fn get(self, index: usize) -> Option<Plane> {
        <[CompactPlane]>::get(self, index)
            .copied()
            .map(CompactPlane::decoded)
    }
}

/// Cooker-layout plane records (fourteen bytes, byte aligned): the distance
/// and kind are gathered a byte at a time, and the normal only for the
/// general kind.
#[cfg(not(target_arch = "mips"))]
impl PlaneSource for RecordSlice<'_, Plane> {
    const CHECKED: bool = true;

    #[inline(always)]
    fn len(self) -> usize {
        RecordSlice::len(self)
    }

    #[inline(always)]
    unsafe fn distances_unchecked(self, index: usize, a: &[i32; 3], b: &[i32; 3]) -> (i32, i32) {
        let base = unsafe { self.as_bytes().as_ptr().add(index * Plane::SIZE) };
        let byte = |offset: usize| unsafe { *base.add(offset) };
        let distance = i32::from_le_bytes([byte(6), byte(7), byte(8), byte(9)]);
        let kind = i32::from_le_bytes([byte(10), byte(11), byte(12), byte(13)]);
        axial_or_general(
            kind as u32 as usize,
            || {
                [
                    i16::from_le_bytes([byte(0), byte(1)]) as i32,
                    i16::from_le_bytes([byte(2), byte(3)]) as i32,
                    i16::from_le_bytes([byte(4), byte(5)]) as i32,
                ]
            },
            distance,
            a,
            b,
        )
    }

    #[inline(always)]
    fn get(self, index: usize) -> Option<Plane> {
        RecordSlice::get(self, index)
    }
}

/// Run `$body` with `planes` and `nodes` bound to the concrete sources of
/// `$hull`, so a generic walker is instantiated once per layout the binary
/// actually uses.
macro_rules! with_sources {
    ($hull:expr, |$planes:ident, $nodes:ident| $body:expr) => {
        match ($hull.planes, $hull.nodes) {
            (PlaneRecords::Compact($planes), HullNodes::Clip($nodes)) => $body,
            (PlaneRecords::Compact($planes), HullNodes::Render { nodes, leaves }) => {
                let $nodes = RenderNodes { nodes, leaves };
                $body
            }
            #[cfg(not(target_arch = "mips"))]
            (PlaneRecords::Packed($planes), HullNodes::Clip($nodes)) => $body,
            #[cfg(not(target_arch = "mips"))]
            (PlaneRecords::Packed($planes), HullNodes::Render { nodes, leaves }) => {
                let $nodes = RenderNodes { nodes, leaves };
                $body
            }
        }
    };
}

#[inline(always)]
const fn array(point: Vec3I32) -> [i32; 3] {
    [point.x, point.y, point.z]
}

#[derive(Copy, Clone)]
pub struct CollisionHull<'a> {
    planes: PlaneRecords<'a>,
    nodes: HullNodes<'a>,
    head_node: i16,
}

impl<'a> CollisionHull<'a> {
    /// Construct a hull from packed records whose clip-node bytes already
    /// satisfy the native little-endian layout. Cooker and host-side callers
    /// use this compatibility entry point; the resident runtime validates the
    /// same alignment once at map load and calls [`Self::from_native_clip_nodes`]
    /// directly. This hull range-checks every index it follows and fails
    /// closed on malformed data.
    #[cfg(not(target_arch = "mips"))]
    pub fn new(
        planes: RecordSlice<'a, Plane>,
        nodes: RecordSlice<'a, ClipNode>,
        head_node: i16,
    ) -> Option<Self> {
        let nodes = nodes.as_native_clip_nodes()?;
        Some(Self {
            planes: PlaneRecords::Packed(planes),
            nodes: HullNodes::Clip(nodes),
            head_node,
        })
    }

    /// Construct a collision hull over validated, native-layout clip nodes.
    ///
    /// Resident maps validate the clip-node lump once during load, so the
    /// trace hot path borrows the native node records directly and follows
    /// plane and child indices without range checks.
    ///
    /// # Safety
    /// Every node's plane index must be below `planes.len()` and every
    /// non-negative child below `nodes.len()`, and `head_node` must be
    /// negative or below `nodes.len()`: exactly what
    /// `PxbspResidentMap::validate_references` proves for a resident map.
    /// Cycles are tolerated (the walk is bounded by the node count).
    pub const unsafe fn from_native_clip_nodes(
        planes: &'a [CompactPlane],
        nodes: &'a [ClipNode],
        head_node: i16,
    ) -> Self {
        Self {
            planes: PlaneRecords::Compact(planes),
            nodes: HullNodes::Clip(nodes),
            head_node,
        }
    }

    /// A point hull (hull 0) served by the render BSP: balanced
    /// tree, leaf contents from the leaf records.
    ///
    /// # Safety
    /// As [`Self::from_native_clip_nodes`]: node plane indices below
    /// `planes.len()`, children below `nodes.len()`, leaf children below
    /// `leaves.len()`, `head_node` negative or below `nodes.len()`.
    pub const unsafe fn from_render_bsp(
        planes: &'a [CompactPlane],
        nodes: RecordSlice<'a, Node>,
        leaves: RecordSlice<'a, Leaf>,
        head_node: i16,
    ) -> Self {
        Self {
            planes: PlaneRecords::Compact(planes),
            nodes: HullNodes::Render { nodes, leaves },
            head_node,
        }
    }

    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.point_contents_from(self.head_node, &point)
    }

    /// Sample an ordered feet-to-head point sequence and return the liquid
    /// depth (how many samples from the feet up are submerged) plus the strongest encountered hazard. Unknown contents,
    /// sky, empty, and solid do not count as liquid. Any malformed tree fails
    /// the whole query instead of returning a partial classification.
    pub fn sample_liquid_contents(&self, points: &[Vec3I32]) -> Option<LiquidContentsSample> {
        let mut sample = LiquidContentsSample::default();
        let mut precedence = 0u8;
        for (index, point) in points.iter().copied().enumerate() {
            let contents = self.point_contents(point)?;
            let candidate = liquid_precedence(contents);
            if candidate == 0 {
                break;
            }
            sample.water_level = (index + 1).min(u8::MAX as usize) as u8;
            if candidate > precedence {
                precedence = candidate;
                sample.contents = contents;
            }
        }
        Some(sample)
    }

    /// Trace a Q20.12 point segment through this Y-up PXBSP hull.
    ///
    /// The output position and plane distance are Q20.12. The output normal
    /// is Q3.12. Traversal uses only deterministic `i32` fixed-point math and
    /// a plane epsilon of 128 in Q20.12. `false` reports malformed BSP data or
    /// scratch overflow and leaves `output` byte-for-byte unchanged.
    ///
    /// Byte-for-byte means exactly that, so a caller's flag slot may hold any
    /// of the 256 byte patterns across a failed call. [`TraceFlag`] is what
    /// makes those bytes legal values rather than undefined behaviour; see
    /// there.
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        with_sources!(self, |planes, nodes| trace_segment(
            planes,
            nodes,
            self.head_node,
            start,
            end,
            scratch,
            output
        ))
    }

    /// Apply one mover transform while retaining this model-local hull.
    pub const fn transformed(self, transform: BrushTransform) -> TransformedCollisionHull<'a> {
        TransformedCollisionHull {
            local: self,
            transform,
        }
    }

    fn point_contents_from(&self, node_index: i16, point: &Vec3I32) -> Option<i16> {
        with_sources!(self, |planes, nodes| point_contents_walk(
            planes,
            nodes,
            node_index,
            &array(*point)
        ))
    }
}

/// Descend from `node` to the leaf holding `point` and return its contents.
///
/// A point exactly on a plane goes to the front child. The walk visits at
/// most as many nodes as the hull has, so a malformed cyclic hull fails
/// instead of looping; checked plane sources also range-check every index.
#[inline(never)]
fn point_contents_walk<P: PlaneSource, N: NodeSource>(
    planes: P,
    nodes: N,
    mut node: i16,
    point: &[i32; 3],
) -> Option<i16> {
    let node_count = nodes.len();
    let mut visited = 0usize;
    while node >= 0 {
        let index = node as usize;
        visited += 1;
        if visited > node_count || (P::CHECKED && index >= node_count) {
            return None;
        }
        // SAFETY: validated hulls keep every index in range; checked
        // sources tested `index` above and test `plane` below.
        let plane = unsafe { nodes.plane_unchecked(index) };
        if P::CHECKED && plane >= planes.len() {
            return None;
        }
        let (distance, _) = unsafe { planes.distances_unchecked(plane, point, point) };
        node = unsafe { nodes.child_unchecked(index, usize::from(distance < 0)) };
    }
    nodes.contents(node)
}

/// Trace `start..end` through the hull under `head_node`.
///
/// The segment is cut at every plane it crosses, at the exact parameter
/// where its end points' plane distances change sign, and the pieces are
/// visited front to back: at each split the piece on the segment start's
/// side is walked first and the other is deferred in `scratch`. Leaves
/// therefore arrive in segment order, and the first solid leaf reached
/// straight from an open one is the contact. Its piece starts on the plane
/// that was crossed, which is solved for the contact point with
/// [`plane_contact`], so the margin off the plane is applied once, at the
/// end, and never during descent.
///
/// Returns `false`, leaving `output` untouched, on malformed hull data or
/// when more than [`TRACE_STACK_CAPACITY`] pieces are deferred at once.
#[inline(never)]
fn trace_segment<P: PlaneSource, N: NodeSource>(
    planes: P,
    nodes: N,
    head_node: i16,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut TraceScratch,
    output: &mut Trace,
) -> bool {
    let from = array(*start);
    let to = array(*end);
    let node_count = nodes.len();
    let mut result = Trace::reaching(*end);
    // `None` until the first leaf; then whether the last leaf was solid.
    let mut last_solid: Option<bool> = None;
    let mut deferred = 0usize;
    let mut piece = PendingPiece {
        lower: SegmentParam::START,
        upper: SegmentParam::END,
        entry_plane: NO_ENTRY_PLANE,
        node: head_node,
        entry_side: 0,
        open_ends: 0,
    };
    loop {
        let mut visited = 0usize;
        while piece.node >= 0 {
            let index = piece.node as usize;
            visited += 1;
            if visited > node_count || (P::CHECKED && index >= node_count) {
                return false;
            }
            // SAFETY: as in `point_contents_walk`.
            let plane = unsafe { nodes.plane_unchecked(index) };
            if P::CHECKED && plane >= planes.len() {
                return false;
            }
            let (ds, de) = unsafe { planes.distances_unchecked(plane, &from, &to) };
            let start_in_front = ds >= 0;
            let near = usize::from(!start_in_front);
            if start_in_front == (de >= 0) {
                piece.node = unsafe { nodes.child_unchecked(index, near) };
                continue;
            }
            // The segment crosses this plane at `cross`, and the crossing
            // point itself is on the front side (on-plane points are front).
            // So a start in front keeps the near side up to and including
            // `cross`, and a start behind keeps it up to just short of it.
            let cross = SegmentParam::crossing(ds, de);
            let (lower, upper) = (piece.lower_end(), piece.upper_end());
            let near_upper = upper.earlier(PieceEnd {
                at: cross,
                open: !start_in_front,
            });
            let far_lower = lower.later(PieceEnd {
                at: cross,
                open: start_in_front,
            });
            if far_lower.empty_up_to(upper) {
                piece.node = unsafe { nodes.child_unchecked(index, near) };
                continue;
            }
            if lower.empty_up_to(near_upper) {
                piece.node = unsafe { nodes.child_unchecked(index, near ^ 1) };
                continue;
            }
            if deferred == TRACE_STACK_CAPACITY {
                return false;
            }
            let mut far = PendingPiece {
                lower: cross,
                upper: piece.upper,
                entry_plane: plane as u16,
                node: unsafe { nodes.child_unchecked(index, near ^ 1) },
                entry_side: near as u8,
                open_ends: 0,
            };
            far.set_ends(far_lower, upper);
            scratch.pieces[deferred].write(far);
            deferred += 1;
            piece.set_ends(lower, near_upper);
            piece.node = unsafe { nodes.child_unchecked(index, near) };
        }

        let Some(contents) = nodes.contents(piece.node) else {
            return false;
        };
        let solid = contents == CONTENTS_SOLID;
        if solid {
            match last_solid {
                None => result.start_solid = TraceFlag::SET,
                Some(false) => {
                    if piece.entry_plane == NO_ENTRY_PLANE {
                        return false;
                    }
                    let Some(contact) = planes.get(usize::from(piece.entry_plane)) else {
                        return false;
                    };
                    let (fraction, point) = plane_contact(*start, *end, contact, piece.entry_side);
                    result.fraction = fraction;
                    result.end = point;
                    if piece.entry_side == 0 {
                        result.normal = contact.normal;
                        result.plane_distance = contact.distance;
                    } else {
                        result.normal = Vec3I16 {
                            x: contact.normal.x.saturating_neg(),
                            y: contact.normal.y.saturating_neg(),
                            z: contact.normal.z.saturating_neg(),
                        };
                        result.plane_distance = contact.distance.saturating_neg();
                    }
                    *output = result;
                    return true;
                }
                Some(true) => {}
            }
        } else {
            result.all_solid = TraceFlag::CLEAR;
            if contents == CONTENTS_EMPTY {
                result.in_open = TraceFlag::SET;
            } else if liquid_precedence(contents) != 0 {
                result.in_water = TraceFlag::SET;
            }
        }
        last_solid = Some(solid);

        if deferred == 0 {
            break;
        }
        deferred -= 1;
        // SAFETY: every slot below `deferred` was written above in this call.
        piece = unsafe { scratch.pieces[deferred].assume_init() };
    }
    *output = result;
    true
}

/// World-space query facade over one model-local clipnode hull.
#[derive(Copy, Clone)]
pub struct TransformedCollisionHull<'a> {
    local: CollisionHull<'a>,
    transform: BrushTransform,
}

impl TransformedCollisionHull<'_> {
    pub fn point_contents(&self, point: Vec3I32) -> Option<i16> {
        self.local
            .point_contents(self.transform.point_to_local(point))
    }

    /// Trace a world-space segment through this transformed model hull.
    ///
    /// Failure and output-preservation semantics match
    /// [`CollisionHull::trace_into`].
    pub fn trace_into(
        &self,
        start: &Vec3I32,
        end: &Vec3I32,
        scratch: &mut TraceScratch,
        output: &mut Trace,
    ) -> bool {
        let local_start = self.transform.point_to_local(*start);
        let local_end = self.transform.point_to_local(*end);
        let mut trace = Trace::default();
        if !self
            .local
            .trace_into(&local_start, &local_end, scratch, &mut trace)
        {
            return false;
        }
        trace.end = self.transform.point_to_world(trace.end);
        trace.normal = rotate_normal(self.transform.rotation, trace.normal);
        trace.plane_distance = trace
            .plane_distance
            .saturating_add(normal_dot_point(trace.normal, self.transform.origin));
        *output = trace;
        true
    }
}

/// Signed Q20.12 distance of `point` from `plane`.
///
/// Q20.12 points against Q3.12 unit normals keep every product and the
/// three-term sum inside `i32`, so the wide multiply and wrapping adds are
/// exactly the saturating form's result on validated hulls, at a fraction of
/// its instruction count (this runs twice per visited node).
#[inline(always)]
fn plane_distance(plane: Plane, point: Vec3I32) -> i32 {
    let dot = match plane.kind {
        0 => point.x,
        1 => point.y,
        2 => point.z,
        _ => mul_q12_i32_wide(point.x, plane.normal.x as i32)
            .wrapping_add(mul_q12_i32_wide(point.y, plane.normal.y as i32))
            .wrapping_add(mul_q12_i32_wide(point.z, plane.normal.z as i32)),
    };
    dot.wrapping_sub(plane.distance)
}

const fn liquid_precedence(contents: i16) -> u8 {
    match contents {
        CONTENTS_LAVA => 3,
        CONTENTS_SLIME => 2,
        CONTENTS_WATER => 1,
        _ => 0,
    }
}

/// Return the public Q0.12 fraction and a high-precision Q20.12 endpoint for
/// the epsilon-offset intersection of an original segment and hit plane.
///
/// Every quantity is a sign plus a `u32` magnitude: the two plane distances
/// are `i32`, so their difference and the epsilon-shifted numerator fit
/// `u32` exactly, and the only products are 32x32 (one R3000 `mult`).
/// Divisions use the hardware 32-bit divide where the operands fit and
/// [`psx_math::int32::div_u64_by_u32`] otherwise, so no 64-bit helper is
/// ever linked. Values are identical to the earlier `i64` formulation for
/// every segment a map can hold (the old form saturated only past 2^63,
/// which needs a segment longer than the coordinate range).
///
/// Inlined into the tracer: it is its only caller, and as a standalone
/// function the two evicted each other from the 4 KiB direct-mapped I-cache
/// on every hit (5% of all refills in the Cortex gameplay profile).
#[inline(always)]
fn plane_contact(start: Vec3I32, end: Vec3I32, plane: Plane, side: u8) -> (i32, Vec3I32) {
    let start_distance = plane_distance(plane, start);
    let end_distance = plane_distance(plane, end);
    if start_distance == end_distance {
        return (0, start);
    }
    // `denominator = start - end`, oriented positive; the numerator follows
    // that orientation and is then clamped to `0..=denominator`.
    let denominator = start_distance.abs_diff(end_distance);
    let flip = start_distance < end_distance;
    let (numerator_nonnegative, numerator_magnitude) = if side == 0 {
        signed_offset(start_distance, -TRACE_PLANE_EPSILON_Q12)
    } else {
        signed_offset(start_distance, TRACE_PLANE_EPSILON_Q12)
    };
    let numerator = if numerator_nonnegative != flip {
        numerator_magnitude.min(denominator)
    } else {
        0
    };
    debug_assert!(numerator <= denominator);
    let fraction = if numerator < (1 << 19) {
        // `numerator << 12` fits `u32`, and the quotient cannot exceed
        // `Q12_ONE` because `numerator <= denominator`.
        ((numerator << 12) / denominator).min(Q12_ONE as u32) as i32
    } else {
        div_u64_by_u32(numerator >> 20, numerator << 12, denominator).min(Q12_ONE as u32) as i32
    };
    let along = |from: i32, to: i32| {
        let delta = from.abs_diff(to);
        // psx-numeric-allow-next-line: R3000 MULTU natively produces this 64-bit product; the divide below is 32-bit only
        let product = u64::from(delta) * u64::from(numerator);
        // psx-numeric-allow-next-line: splitting the MULTU result into its HI and LO words
        let (high, low) = ((product >> 32) as u32, product as u32);
        // `numerator <= denominator`, so the quotient is at most `delta` and
        // `high < denominator`: the exact 64-by-32 form applies.
        let quotient = if high == 0 {
            low / denominator
        } else {
            div_u64_by_u32(high, low, denominator)
        };
        if to >= from {
            from.saturating_add_unsigned(quotient)
        } else {
            from.saturating_sub_unsigned(quotient)
        }
    };
    (
        fraction,
        Vec3I32 {
            x: along(start.x, end.x),
            y: along(start.y, end.y),
            z: along(start.z, end.z),
        },
    )
}

/// `value + offset` as a non-negative flag plus a `u32` magnitude, exact for
/// every `i32` input (the sum can exceed `i32` by `|offset|`).
#[inline(always)]
fn signed_offset(value: i32, offset: i32) -> (bool, u32) {
    let offset_magnitude = offset.unsigned_abs();
    if (value >= 0) == (offset >= 0) {
        (value >= 0, value.unsigned_abs() + offset_magnitude)
    } else if value.unsigned_abs() >= offset_magnitude {
        (value >= 0, value.unsigned_abs() - offset_magnitude)
    } else {
        (offset >= 0, offset_magnitude - value.unsigned_abs())
    }
}

fn inverse_rotate_q12(rotation: Mat3I16, vector: Vec3I32) -> Vec3I32 {
    Vec3I32 {
        x: q12_dot(
            [rotation.m[0][0], rotation.m[1][0], rotation.m[2][0]],
            vector,
        ),
        y: q12_dot(
            [rotation.m[0][1], rotation.m[1][1], rotation.m[2][1]],
            vector,
        ),
        z: q12_dot(
            [rotation.m[0][2], rotation.m[1][2], rotation.m[2][2]],
            vector,
        ),
    }
}

fn rotate_normal(rotation: Mat3I16, normal: Vec3I16) -> Vec3I16 {
    let normal = Vec3I32 {
        x: normal.x as i32,
        y: normal.y as i32,
        z: normal.z as i32,
    };
    let clamp = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    Vec3I16 {
        x: clamp(q12_dot(rotation.m[0], normal)),
        y: clamp(q12_dot(rotation.m[1], normal)),
        z: clamp(q12_dot(rotation.m[2], normal)),
    }
}

fn q12_dot(row: [i16; 3], vector: Vec3I32) -> i32 {
    mul_q12_i32(vector.x, row[0] as i32)
        .saturating_add(mul_q12_i32(vector.y, row[1] as i32))
        .saturating_add(mul_q12_i32(vector.z, row[2] as i32))
}

fn normal_dot_point(normal: Vec3I16, point: Vec3I32) -> i32 {
    mul_q12_i32(point.x, normal.x as i32)
        .saturating_add(mul_q12_i32(point.y, normal.y as i32))
        .saturating_add(mul_q12_i32(point.z, normal.z as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `i64` form `plane_contact` replaced, kept as the host oracle.
    fn plane_contact_wide(start: Vec3I32, end: Vec3I32, plane: Plane, side: u8) -> (i32, Vec3I32) {
        let start_distance = i64::from(plane_distance(plane, start));
        let end_distance = i64::from(plane_distance(plane, end));
        let mut denominator = start_distance - end_distance;
        if denominator == 0 {
            return (0, start);
        }
        let epsilon = i64::from(TRACE_PLANE_EPSILON_Q12);
        let mut numerator = if side == 0 {
            start_distance - epsilon
        } else {
            start_distance + epsilon
        };
        if denominator < 0 {
            denominator = -denominator;
            numerator = -numerator;
        }
        numerator = numerator.clamp(0, denominator);
        let fraction = ((numerator << 12) / denominator).clamp(0, i64::from(Q12_ONE)) as i32;
        let along = |from: i32, to: i32| {
            let delta = i64::from(to) - i64::from(from);
            let value = i64::from(from) + delta * numerator / denominator;
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        (
            fraction,
            Vec3I32 {
                x: along(start.x, end.x),
                y: along(start.y, end.y),
                z: along(start.z, end.z),
            },
        )
    }

    #[test]
    fn plane_contact_matches_the_wide_oracle() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for iteration in 0..50_000u32 {
            // Mix map-scale segments (the reachable regime) with full-range
            // coordinates so every branch of both forms is exercised.
            let span: i32 = if iteration % 4 == 0 {
                i32::MAX
            } else {
                1 << 24
            };
            let mut values = [0i32; 7];
            for value in &mut values {
                *value = (next() as i32) % span;
            }
            let start = Vec3I32 {
                x: values[0],
                y: values[1],
                z: values[2],
            };
            let end = Vec3I32 {
                x: values[3],
                y: values[4],
                z: values[5],
            };
            let kind = (next() % 4) as i32;
            let normal = [next() as i16, next() as i16, next() as i16];
            let plane = Plane {
                normal: Vec3I16 {
                    x: normal[0] & 0x1fff,
                    y: normal[1] & 0x1fff,
                    z: normal[2] & 0x1fff,
                },
                distance: values[6],
                kind,
            };
            let side = (next() & 1) as u8;
            assert_eq!(
                plane_contact(start, end, plane, side),
                plane_contact_wide(start, end, plane, side),
                "start {start:?} end {end:?} plane {plane:?} side {side}"
            );
        }
    }
    use crate::CookedRecord;
    use alloc::vec::Vec;

    /// The all-i64 arithmetic `plane_contact` used before the narrow forms.
    fn wide_plane_contact_math(
        start_distance: i64,
        end_distance: i64,
        side: u8,
        start: Vec3I32,
        end: Vec3I32,
    ) -> Option<(i32, Vec3I32)> {
        let mut denominator = start_distance - end_distance;
        if denominator == 0 {
            return None;
        }
        let epsilon = i64::from(TRACE_PLANE_EPSILON_Q12);
        let mut numerator = if side == 0 {
            start_distance - epsilon
        } else {
            start_distance + epsilon
        };
        if denominator < 0 {
            denominator = -denominator;
            numerator = -numerator;
        }
        numerator = numerator.clamp(0, denominator);
        let fraction = ((numerator << 12) / denominator).clamp(0, i64::from(Q12_ONE)) as i32;
        let along = |from: i32, to: i32| {
            let delta = i64::from(to) - i64::from(from);
            let value =
                i64::from(from).saturating_add(delta.saturating_mul(numerator) / denominator);
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        Some((
            fraction,
            Vec3I32 {
                x: along(start.x, end.x),
                y: along(start.y, end.y),
                z: along(start.z, end.z),
            },
        ))
    }

    /// The same arithmetic with the narrow fast paths `plane_contact` now
    /// takes, so the guards can be swept without building a hull.
    fn narrow_plane_contact_math(
        start_distance: i64,
        end_distance: i64,
        side: u8,
        start: Vec3I32,
        end: Vec3I32,
    ) -> Option<(i32, Vec3I32)> {
        let mut denominator = start_distance - end_distance;
        if denominator == 0 {
            return None;
        }
        let epsilon = i64::from(TRACE_PLANE_EPSILON_Q12);
        let mut numerator = if side == 0 {
            start_distance - epsilon
        } else {
            start_distance + epsilon
        };
        if denominator < 0 {
            denominator = -denominator;
            numerator = -numerator;
        }
        numerator = numerator.clamp(0, denominator);
        let narrow_denominator = denominator as u64 as u32;
        let fraction = if numerator < (1 << 19) {
            (((numerator as u32) << 12) / narrow_denominator).min(Q12_ONE as u32) as i32
        } else {
            ((numerator << 12) / denominator).clamp(0, i64::from(Q12_ONE)) as i32
        };
        let along = |from: i32, to: i32| {
            let delta = i64::from(to) - i64::from(from);
            let magnitude = delta.unsigned_abs() * numerator as u64;
            let offset = if magnitude <= u64::from(u32::MAX) {
                let quotient = i64::from((magnitude as u32) / narrow_denominator);
                if delta < 0 {
                    -quotient
                } else {
                    quotient
                }
            } else {
                delta.saturating_mul(numerator) / denominator
            };
            let value = i64::from(from).saturating_add(offset);
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        Some((
            fraction,
            Vec3I32 {
                x: along(start.x, end.x),
                y: along(start.y, end.y),
                z: along(start.z, end.z),
            },
        ))
    }

    #[test]
    fn the_narrow_plane_contact_divisions_answer_exactly_like_the_wide_ones() {
        let corners = [
            0i32,
            1,
            -1,
            127,
            128,
            129,
            4095,
            4096,
            524_287,
            524_288,
            524_289,
            1 << 20,
            (1 << 30) - 1,
            1 << 30,
            i32::MAX,
            i32::MIN,
            i32::MAX - 1,
            i32::MIN + 1,
        ];
        let mut narrow_fraction = 0usize;
        let mut wide_fraction = 0usize;
        let mut checked = 0usize;
        let mut state = 0x853c_49e6_748f_ea9bu64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) as u32
        };
        let mut check =
            |start_distance: i64, end_distance: i64, side: u8, a: Vec3I32, b: Vec3I32| {
                let wide = wide_plane_contact_math(start_distance, end_distance, side, a, b);
                let narrow = narrow_plane_contact_math(start_distance, end_distance, side, a, b);
                assert_eq!(
                    wide, narrow,
                    "plane contact disagreed for d0={start_distance} d1={end_distance} side={side}"
                );
                if wide.is_some() {
                    let denominator = (start_distance - end_distance).abs();
                    let epsilon = i64::from(TRACE_PLANE_EPSILON_Q12);
                    let mut numerator = if side == 0 {
                        start_distance - epsilon
                    } else {
                        start_distance + epsilon
                    };
                    if start_distance - end_distance < 0 {
                        numerator = -numerator;
                    }
                    if numerator.clamp(0, denominator) < (1 << 19) {
                        narrow_fraction += 1;
                    } else {
                        wide_fraction += 1;
                    }
                }
                checked += 1;
            };
        for &first in &corners {
            for &second in &corners {
                for side in 0..2u8 {
                    for &(a, b) in &[
                        (
                            Vec3I32 { x: 0, y: 0, z: 0 },
                            Vec3I32 {
                                x: 1,
                                y: -1,
                                z: 4096,
                            },
                        ),
                        (
                            Vec3I32 {
                                x: i32::MIN,
                                y: i32::MAX,
                                z: 0,
                            },
                            Vec3I32 {
                                x: i32::MAX,
                                y: i32::MIN,
                                z: -1,
                            },
                        ),
                        (
                            Vec3I32 {
                                x: 1_000_000,
                                y: -1_000_000,
                                z: 500,
                            },
                            Vec3I32 {
                                x: 1_004_096,
                                y: -1_000_500,
                                z: 512,
                            },
                        ),
                    ] {
                        check(i64::from(first), i64::from(second), side, a, b);
                    }
                }
            }
        }
        for _ in 0..60_000 {
            let first = next() as i32;
            let second = next() as i32;
            let a = Vec3I32 {
                x: next() as i32,
                y: next() as i32,
                z: next() as i32,
            };
            let b = Vec3I32 {
                x: next() as i32,
                y: next() as i32,
                z: next() as i32,
            };
            check(
                i64::from(first),
                i64::from(second),
                (next() & 1) as u8,
                a,
                b,
            );
            // Near-plane geometry: small distances either side of the epsilon,
            // which is what a real trace produces and the narrow guard targets.
            let near_first = (next() % 40_000) as i32 - 20_000;
            let near_second = (next() % 40_000) as i32 - 20_000;
            let near_a = Vec3I32 {
                x: (next() % 8_000_000) as i32,
                y: (next() % 8_000_000) as i32,
                z: (next() % 8_000_000) as i32,
            };
            let near_b = Vec3I32 {
                x: near_a.x + (next() % 20_000) as i32 - 10_000,
                y: near_a.y + (next() % 20_000) as i32 - 10_000,
                z: near_a.z + (next() % 20_000) as i32 - 10_000,
            };
            check(
                i64::from(near_first),
                i64::from(near_second),
                (next() & 1) as u8,
                near_a,
                near_b,
            );
        }
        assert!(checked > 100_000, "sweep collapsed to {checked} cases");
        assert!(
            narrow_fraction > 1_000 && wide_fraction > 1_000,
            "sweep must exercise both arms, got narrow={narrow_fraction} wide={wide_fraction}"
        );
    }

    fn axial_x_plane() -> [u8; Plane::SIZE] {
        plane(
            Vec3I16 {
                x: Q12_ONE as i16,
                y: 0,
                z: 0,
            },
            0,
            0,
        )
    }

    fn plane(normal: Vec3I16, distance: i32, kind: i32) -> [u8; Plane::SIZE] {
        let mut bytes = [0u8; Plane::SIZE];
        bytes[0..2].copy_from_slice(&normal.x.to_le_bytes());
        bytes[2..4].copy_from_slice(&normal.y.to_le_bytes());
        bytes[4..6].copy_from_slice(&normal.z.to_le_bytes());
        bytes[6..10].copy_from_slice(&distance.to_le_bytes());
        bytes[10..14].copy_from_slice(&kind.to_le_bytes());
        bytes
    }

    fn one_node() -> [u8; 6] {
        node(0, CONTENTS_EMPTY, CONTENTS_SOLID)
    }

    fn node(plane: i16, front: i16, back: i16) -> [u8; 6] {
        let mut bytes = [0u8; 6];
        bytes[0..2].copy_from_slice(&plane.to_le_bytes());
        bytes[2..4].copy_from_slice(&front.to_le_bytes());
        bytes[4..6].copy_from_slice(&back.to_le_bytes());
        bytes
    }

    fn hull<'a>(planes: &'a [u8], nodes: &'a [u8]) -> CollisionHull<'a> {
        CollisionHull::new(
            RecordSlice::new(planes).unwrap(),
            RecordSlice::new(nodes).unwrap(),
            0,
        )
        .expect("aligned hull fixture")
    }

    fn trace(
        hull: &CollisionHull<'_>,
        start: Vec3I32,
        end: Vec3I32,
        scratch: &mut TraceScratch,
    ) -> Trace {
        let mut output = Trace::default();
        assert!(hull.trace_into(&start, &end, scratch, &mut output));
        output
    }

    /// A trace whose every byte, flag slots and padding included, is the
    /// caller's own arbitrary fill or an arbitrary written value.
    ///
    /// The flag slots deliberately keep `fill`, which is normally neither 0 nor
    /// 1. That is legal precisely because [`TraceFlag`] is byte-backed; with
    ///
    /// `bool` slots this helper had to overwrite them with valid booleans, and
    /// so could never test the bytes that actually matter.
    fn sentinel_trace(fill: u8) -> Trace {
        let mut output = core::mem::MaybeUninit::<Trace>::uninit();
        unsafe {
            core::ptr::write_bytes(
                output.as_mut_ptr().cast::<u8>(),
                fill,
                core::mem::size_of::<Trace>(),
            );
            let pointer = output.as_mut_ptr();
            core::ptr::addr_of_mut!((*pointer).fraction).write(0x1122_3344);
            core::ptr::addr_of_mut!((*pointer).end).write(Vec3I32 {
                x: 0x0102_0304,
                y: 0x1112_1314,
                z: 0x2122_2324,
            });
            core::ptr::addr_of_mut!((*pointer).normal).write(Vec3I16 {
                x: 0x3132,
                y: 0x4142,
                z: 0x5152,
            });
            core::ptr::addr_of_mut!((*pointer).plane_distance).write(0x6162_6364);
            output.assume_init()
        }
    }

    fn trace_bytes(trace: &Trace) -> [u8; core::mem::size_of::<Trace>()] {
        let mut bytes = [0u8; core::mem::size_of::<Trace>()];
        unsafe {
            core::ptr::copy_nonoverlapping(
                (trace as *const Trace).cast::<u8>(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
        }
        bytes
    }

    fn deep_crossing_hull(depth: usize) -> (Vec<u8>, Vec<u8>) {
        let mut planes = Vec::with_capacity(depth * 14);
        let mut nodes = Vec::with_capacity(depth * 6);
        for index in 0..depth {
            planes.extend_from_slice(&plane(
                Vec3I16 {
                    x: Q12_ONE as i16,
                    y: 0,
                    z: 0,
                },
                index as i32 * Q12_ONE,
                0,
            ));
            let front = if index + 1 == depth {
                CONTENTS_EMPTY
            } else {
                (index + 1) as i16
            };
            nodes.extend_from_slice(&node(index as i16, front, CONTENTS_SOLID));
        }
        (planes, nodes)
    }

    #[test]
    fn point_contents_follows_axial_plane_children() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let hull = hull(&planes, &nodes);
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 4096,
                y: 0,
                z: 0
            }),
            Some(CONTENTS_EMPTY)
        );
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: -4096,
                y: 0,
                z: 0
            }),
            Some(CONTENTS_SOLID)
        );
    }

    #[test]
    fn compact_working_set_matches_packed_collision() {
        let plane_bytes = axial_x_plane();
        let node_bytes = one_node();
        let packed = hull(&plane_bytes, &node_bytes);
        let plane = Plane::decode(&plane_bytes);
        let compact_planes = [CompactPlane {
            normal: plane.normal,
            kind: plane.kind as u8,
            sign_bits: 0,
            distance: plane.distance,
        }];
        let compact_nodes = [ClipNode::decode(&node_bytes)];
        // SAFETY: one node whose plane is index 0 and whose children are
        // both contents.
        let decoded =
            unsafe { CollisionHull::from_native_clip_nodes(&compact_planes, &compact_nodes, 0) };
        let start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };

        assert_eq!(packed.point_contents(start), decoded.point_contents(start));
        assert_eq!(
            trace(&packed, start, end, &mut TraceScratch::new()),
            trace(&decoded, start, end, &mut TraceScratch::new())
        );
    }

    #[test]
    fn trace_stops_on_the_near_side_of_a_solid_plane() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let trace = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: 4096,
                y: 0,
                z: 0,
            },
            Vec3I32 {
                x: -4096,
                y: 0,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert!(!trace.all_solid.is_set());
        assert!(!trace.start_solid.is_set());
        assert_eq!(trace.fraction, 1984);
        assert_eq!(trace.end.x, 128);
        assert_eq!(trace.normal.x, 4096);
    }

    #[test]
    fn transformed_hull_rotates_and_translates_world_queries() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let transform = BrushTransform {
            origin: Vec3I32 {
                x: 10 * Q12_ONE,
                y: 20 * Q12_ONE,
                z: 30 * Q12_ONE,
            },
            rotation: Mat3I16::rotate_z(64),
        };
        let hull = hull(&planes, &nodes).transformed(transform);
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 10 * Q12_ONE,
                y: 21 * Q12_ONE,
                z: 30 * Q12_ONE,
            }),
            Some(CONTENTS_EMPTY)
        );
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 10 * Q12_ONE,
                y: 19 * Q12_ONE,
                z: 30 * Q12_ONE,
            }),
            Some(CONTENTS_SOLID)
        );

        let start = Vec3I32 {
            x: 10 * Q12_ONE,
            y: 21 * Q12_ONE,
            z: 30 * Q12_ONE,
        };
        let end = Vec3I32 {
            x: 10 * Q12_ONE,
            y: 19 * Q12_ONE,
            z: 30 * Q12_ONE,
        };
        let mut trace = Trace::default();
        assert!(hull.trace_into(&start, &end, &mut TraceScratch::new(), &mut trace,));
        assert_eq!(
            trace.normal,
            Vec3I16 {
                x: 0,
                y: 4096,
                z: 0
            }
        );
        assert_eq!(trace.plane_distance, 20 * Q12_ONE);
        assert!((20 * Q12_ONE..=20 * Q12_ONE + TRACE_PLANE_EPSILON_Q12).contains(&trace.end.y));
    }

    #[test]
    fn contents_codes_match_the_pxbsp_contract() {
        assert_eq!(CONTENTS_EMPTY, -1);
        assert_eq!(CONTENTS_SOLID, -2);
        assert_eq!(CONTENTS_WATER, -3);
        assert_eq!(CONTENTS_SLIME, -4);
        assert_eq!(CONTENTS_LAVA, -5);
        assert_eq!(CONTENTS_SKY, -6);
    }

    #[test]
    fn ordered_liquid_samples_report_depth_and_strongest_hazard() {
        let mut planes = Vec::new();
        for distance in [0, -Q12_ONE, -2 * Q12_ONE] {
            planes.extend_from_slice(&plane(
                Vec3I16 {
                    x: Q12_ONE as i16,
                    y: 0,
                    z: 0,
                },
                distance,
                0,
            ));
        }
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&node(0, CONTENTS_EMPTY, 1));
        nodes.extend_from_slice(&node(1, CONTENTS_WATER, 2));
        nodes.extend_from_slice(&node(2, CONTENTS_SLIME, CONTENTS_LAVA));
        let hull = hull(&planes, &nodes);
        let at = |x| Vec3I32 { x, y: 0, z: 0 };

        assert_eq!(
            hull.sample_liquid_contents(&[at(Q12_ONE), at(Q12_ONE), at(Q12_ONE)]),
            Some(LiquidContentsSample::default())
        );
        assert_eq!(
            hull.sample_liquid_contents(&[at(-Q12_ONE / 2), at(Q12_ONE), at(Q12_ONE)]),
            Some(LiquidContentsSample {
                contents: CONTENTS_WATER,
                water_level: 1,
            })
        );
        assert_eq!(
            hull.sample_liquid_contents(&[at(Q12_ONE), at(-Q12_ONE / 2), at(-3 * Q12_ONE)]),
            Some(LiquidContentsSample::default()),
            "the water level must begin at the feet"
        );
        assert_eq!(
            hull.sample_liquid_contents(&[
                at(-Q12_ONE / 2),
                at(-3 * Q12_ONE / 2),
                at(-3 * Q12_ONE),
            ]),
            Some(LiquidContentsSample {
                contents: CONTENTS_LAVA,
                water_level: 3,
            })
        );
    }

    #[test]
    fn trace_storage_has_a_fixed_guest_size() {
        assert_eq!(core::mem::size_of::<Trace>(), 32);
        assert_eq!(core::mem::size_of::<TraceScratch>(), 1_536);
    }

    #[test]
    fn unobstructed_trace_has_exact_open_result() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 3 * Q12_ONE,
            z: 4 * Q12_ONE,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: Q12_ONE,
                y: 3 * Q12_ONE,
                z: 4 * Q12_ONE,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert_eq!(
            result,
            Trace {
                all_solid: TraceFlag::CLEAR,
                start_solid: TraceFlag::CLEAR,
                in_open: TraceFlag::SET,
                in_water: TraceFlag::CLEAR,
                fraction: Q12_ONE,
                end,
                normal: Vec3I16 { x: 0, y: 0, z: 0 },
                plane_distance: 0,
            }
        );
    }

    #[test]
    fn start_solid_and_all_solid_are_exact() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: -2 * Q12_ONE,
            y: Q12_ONE,
            z: 0,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: -Q12_ONE,
                y: Q12_ONE,
                z: 0,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert_eq!(
            result,
            Trace {
                all_solid: TraceFlag::SET,
                start_solid: TraceFlag::SET,
                in_open: TraceFlag::CLEAR,
                in_water: TraceFlag::CLEAR,
                fraction: Q12_ONE,
                end,
                normal: Vec3I16 { x: 0, y: 0, z: 0 },
                plane_distance: 0,
            }
        );
    }

    #[test]
    fn start_solid_trace_can_exit_into_open_space() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let end = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: -Q12_ONE,
                y: 0,
                z: 0,
            },
            end,
            &mut TraceScratch::new(),
        );
        assert!(result.start_solid.is_set());
        assert!(!result.all_solid.is_set());
        assert!(result.in_open.is_set());
        assert_eq!(result.fraction, Q12_ONE);
        assert_eq!(result.end, end);
    }

    fn x_plane(distance_units: i32) -> [u8; Plane::SIZE] {
        plane(
            Vec3I16 {
                x: Q12_ONE as i16,
                y: 0,
                z: 0,
            },
            distance_units * Q12_ONE,
            0,
        )
    }

    fn x(units_q12: i32) -> Vec3I32 {
        Vec3I32 {
            x: units_q12,
            y: 0,
            z: 0,
        }
    }

    #[test]
    fn a_plane_repeated_deeper_in_the_tree_adds_no_zero_width_solid() {
        // Node 0 splits at x = 0 with open space behind it; node 1 splits on
        // the same plane again with solid behind it. The segment's crossing
        // point is in front of node 0, so node 1's back side is never
        // reached and nothing is hit.
        let planes = x_plane(0);
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&node(0, 1, CONTENTS_EMPTY));
        nodes.extend_from_slice(&node(0, CONTENTS_EMPTY, CONTENTS_SOLID));
        let result = trace(
            &hull(&planes, &nodes),
            x(Q12_ONE),
            x(-Q12_ONE),
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, Q12_ONE);
        assert_eq!(result.end, x(-Q12_ONE));
        assert!(!result.all_solid.is_set());
        assert!(!result.start_solid.is_set());
    }

    #[test]
    fn a_solid_slab_thinner_than_the_margin_still_stops_the_trace() {
        // Open for x >= 1/64 unit and for x < 0, solid in between.
        let mut planes = Vec::new();
        planes.extend_from_slice(&plane(
            Vec3I16 {
                x: Q12_ONE as i16,
                y: 0,
                z: 0,
            },
            Q12_ONE / 64,
            0,
        ));
        planes.extend_from_slice(&x_plane(0));
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&node(0, CONTENTS_EMPTY, 1));
        nodes.extend_from_slice(&node(1, CONTENTS_SOLID, CONTENTS_EMPTY));
        let result = trace(
            &hull(&planes, &nodes),
            x(Q12_ONE),
            x(-Q12_ONE),
            &mut TraceScratch::new(),
        );
        assert!(result.fraction < Q12_ONE);
        assert_eq!(result.normal.x, Q12_ONE as i16);
        assert_eq!(result.plane_distance, Q12_ONE / 64);
        assert!(result.end.x >= Q12_ONE / 64);
    }

    #[test]
    fn leaving_solid_then_entering_again_reports_the_second_entry() {
        // Solid for x < 0 and for x >= 2, open in between; the trace starts
        // in the left solid and runs right.
        let mut planes = Vec::new();
        planes.extend_from_slice(&x_plane(0));
        planes.extend_from_slice(&x_plane(2));
        let mut nodes = Vec::new();
        nodes.extend_from_slice(&node(0, 1, CONTENTS_SOLID));
        nodes.extend_from_slice(&node(1, CONTENTS_SOLID, CONTENTS_EMPTY));
        let result = trace(
            &hull(&planes, &nodes),
            x(-Q12_ONE),
            x(3 * Q12_ONE),
            &mut TraceScratch::new(),
        );
        assert!(result.start_solid.is_set());
        assert!(!result.all_solid.is_set());
        assert!(result.in_open.is_set());
        // Entering x = 2 from below: the normal faces the start (-x).
        assert_eq!(result.normal.x, -(Q12_ONE as i16));
        assert_eq!(result.plane_distance, -2 * Q12_ONE);
        assert_eq!(result.end.x, 2 * Q12_ONE - TRACE_PLANE_EPSILON_Q12);
    }

    #[test]
    fn non_axial_plane_collision_is_exact() {
        let planes = plane(
            Vec3I16 {
                x: 2896,
                y: 2896,
                z: 0,
            },
            0,
            3,
        );
        let nodes = one_node();
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: Q12_ONE,
                y: Q12_ONE,
                z: 0,
            },
            Vec3I32 {
                x: -Q12_ONE,
                y: -Q12_ONE,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, 2002);
        // The high-precision contact solve lands exactly on the packed
        // plane's 128-Q12 epsilon contour. Q0.12 fraction interpolation used
        // to round both coordinates up to 92 (signed distance 130).
        assert_eq!(result.end, Vec3I32 { x: 91, y: 91, z: 0 });
        assert_eq!(
            result.normal,
            Vec3I16 {
                x: 2896,
                y: 2896,
                z: 0,
            }
        );
        assert_eq!(result.plane_distance, 0);
    }

    #[test]
    fn near_plane_epsilon_clamps_to_the_segment_start() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let start = Vec3I32 { x: 64, y: 0, z: 0 };
        let result = trace(
            &hull(&planes, &nodes),
            start,
            Vec3I32 {
                x: -Q12_ONE,
                y: 0,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, 0);
        assert_eq!(result.end, start);
        assert_eq!(result.normal.x, Q12_ONE as i16);
    }

    #[test]
    fn zero_length_trace_is_deterministic() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let point = Vec3I32 {
            x: Q12_ONE,
            y: -7 * Q12_ONE,
            z: 9 * Q12_ONE,
        };
        let result = trace(
            &hull(&planes, &nodes),
            point,
            point,
            &mut TraceScratch::new(),
        );
        assert_eq!(result.fraction, Q12_ONE);
        assert_eq!(result.end, point);
        assert!(result.in_open.is_set());
        assert!(!result.all_solid.is_set());
    }

    #[test]
    fn on_plane_tie_uses_the_front_child_and_normal() {
        let planes = axial_x_plane();
        let nodes = one_node();
        let start = Vec3I32 { x: 0, y: 0, z: 0 };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let first = trace(&hull(&planes, &nodes), start, end, &mut scratch);
        let second = trace(&hull(&planes, &nodes), start, end, &mut scratch);
        assert_eq!(first, second);
        assert_eq!(first.fraction, 0);
        assert_eq!(first.end, start);
        assert_eq!(first.normal.x, Q12_ONE as i16);
    }

    #[test]
    fn static_y_up_brush_floor_hit_uses_the_public_contract() {
        let planes = plane(
            Vec3I16 {
                x: 0,
                y: Q12_ONE as i16,
                z: 0,
            },
            0,
            1,
        );
        let nodes = one_node();
        let result = trace(
            &hull(&planes, &nodes),
            Vec3I32 {
                x: 0,
                y: Q12_ONE,
                z: 0,
            },
            Vec3I32 {
                x: 0,
                y: -Q12_ONE,
                z: 0,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.end.y, TRACE_PLANE_EPSILON_Q12);
        assert_eq!(result.normal.y, Q12_ONE as i16);
        assert_eq!(result.plane_distance, 0);
    }

    #[test]
    fn long_floor_probe_keeps_subunit_contact_precision() {
        let floor = 4 * Q12_ONE;
        let planes = plane(
            Vec3I16 {
                x: 0,
                y: Q12_ONE as i16,
                z: 0,
            },
            floor,
            1,
        );
        let result = trace(
            &hull(&planes, &one_node()),
            Vec3I32 {
                x: 28 * Q12_ONE,
                y: 6 * Q12_ONE,
                z: 12 * Q12_ONE,
            },
            Vec3I32 {
                x: 28 * Q12_ONE,
                y: -32_762 * Q12_ONE,
                z: 12 * Q12_ONE,
            },
            &mut TraceScratch::new(),
        );
        assert_eq!(result.end.y, floor + TRACE_PLANE_EPSILON_Q12);
        assert_eq!(result.end.x, 28 * Q12_ONE);
        assert_eq!(result.end.z, 12 * Q12_ONE);
    }

    #[test]
    fn failed_static_and_transformed_traces_preserve_every_output_byte() {
        let planes = axial_x_plane();
        let invalid_nodes = node(7, CONTENTS_EMPTY, CONTENTS_SOLID);
        let invalid_hull = hull(&planes, &invalid_nodes);
        let start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let mut static_output = sentinel_trace(0xa5);
        let static_before = trace_bytes(&static_output);
        assert!(!invalid_hull.trace_into(&start, &end, &mut scratch, &mut static_output));
        assert_eq!(trace_bytes(&static_output), static_before);

        let transformed = invalid_hull.transformed(BrushTransform::translated(Vec3I32 {
            x: Q12_ONE,
            y: 2 * Q12_ONE,
            z: 3 * Q12_ONE,
        }));
        let mut mover_output = sentinel_trace(0x5a);
        let mover_before = trace_bytes(&mover_output);
        assert!(!transformed.trace_into(&start, &end, &mut scratch, &mut mover_output));
        assert_eq!(trace_bytes(&mover_output), mover_before);

        let cyclic_nodes = node(0, 0, CONTENTS_SOLID);
        let cyclic_hull = hull(&planes, &cyclic_nodes);
        let cyclic_end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut cyclic_output = sentinel_trace(0xc3);
        let cyclic_before = trace_bytes(&cyclic_output);
        assert!(!cyclic_hull.trace_into(&start, &cyclic_end, &mut scratch, &mut cyclic_output,));
        assert_eq!(trace_bytes(&cyclic_output), cyclic_before);
    }

    #[test]
    fn stack_capacity_boundary_succeeds_and_overflow_preserves_output() {
        let (boundary_planes, boundary_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 1) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let boundary = trace(
            &hull(&boundary_planes, &boundary_nodes),
            start,
            end,
            &mut scratch,
        );
        assert_eq!(
            boundary.plane_distance,
            (TRACE_STACK_CAPACITY as i32 - 1) * Q12_ONE
        );
        assert_eq!(boundary.normal.x, Q12_ONE as i16);
        assert!(!boundary.all_solid.is_set());

        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let mut output = sentinel_trace(0x3c);
        let before = trace_bytes(&output);
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut scratch,
            &mut output,
        ));
        assert_eq!(trace_bytes(&output), before);
    }

    #[test]
    fn scratch_reuse_after_overflow_has_no_stale_state() {
        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 2) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut scratch = TraceScratch::new();
        let mut ignored = Trace::default();
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut scratch,
            &mut ignored,
        ));

        let planes = axial_x_plane();
        let nodes = one_node();
        let simple_start = Vec3I32 {
            x: Q12_ONE,
            y: 0,
            z: 0,
        };
        let simple_end = Vec3I32 {
            x: 2 * Q12_ONE,
            y: 0,
            z: 0,
        };
        let reused = trace(
            &hull(&planes, &nodes),
            simple_start,
            simple_end,
            &mut scratch,
        );
        let fresh = trace(
            &hull(&planes, &nodes),
            simple_start,
            simple_end,
            &mut TraceScratch::new(),
        );
        assert_eq!(reused, fresh);
    }

    /// The flag slots are storage for arbitrary bytes, and the boundary that
    /// turns them into meaning normalizes rather than reinterprets.
    ///
    /// Every one of the 256 byte patterns is written into a live flag slot and
    /// read back, which is only a defined program because the slot is byte
    /// backed. The guest once produced 0xe7 in exactly this slot; with a `bool`
    /// there, merely holding that byte was already undefined behaviour and no
    /// later read could undo it.
    #[test]
    fn every_flag_byte_is_a_legal_value_and_normalizes_to_one_bool() {
        for byte in 0..=u8::MAX {
            let flag = TraceFlag::from_byte(byte);
            assert_eq!(flag.byte(), byte, "the slot stores the caller's byte");
            assert_eq!(flag.is_set(), byte != 0, "0x{byte:02x} normalizes wrong");
            // Constructing the `bool` may not smuggle the byte through.
            let boolean: bool = flag.into();
            let observed =
                unsafe { core::ptr::read_volatile(core::ptr::from_ref(&boolean).cast::<u8>()) };
            assert!(observed <= 1, "0x{byte:02x} produced an invalid bool byte");
        }
        assert_eq!(TraceFlag::default(), TraceFlag::CLEAR);
        assert_eq!(TraceFlag::new(true), TraceFlag::SET);
        assert_eq!(TraceFlag::new(false), TraceFlag::CLEAR);
    }

    /// A failed trace preserves poisoned flag bytes exactly, and the caller can
    /// still read them afterwards.
    ///
    /// This is the pairing that used to be impossible to state: the contract
    /// says the bytes survive untouched, and with `bool` slots surviving
    /// untouched meant surviving as an invalid value.
    #[test]
    fn a_failed_trace_preserves_poisoned_flag_bytes_and_they_stay_readable() {
        let (overflow_planes, overflow_nodes) = deep_crossing_hull(TRACE_STACK_CAPACITY + 1);
        let start = Vec3I32 {
            x: (TRACE_STACK_CAPACITY as i32 + 2) * Q12_ONE,
            y: 0,
            z: 0,
        };
        let end = Vec3I32 {
            x: -Q12_ONE,
            y: 0,
            z: 0,
        };
        let mut output = Trace {
            all_solid: TraceFlag::from_byte(0xe7),
            start_solid: TraceFlag::from_byte(0x00),
            in_open: TraceFlag::from_byte(0x02),
            in_water: TraceFlag::from_byte(0xff),
            ..Trace::default()
        };
        let before = trace_bytes(&output);
        assert!(!hull(&overflow_planes, &overflow_nodes).trace_into(
            &start,
            &end,
            &mut TraceScratch::new(),
            &mut output,
        ));
        assert_eq!(trace_bytes(&output), before);
        assert_eq!(output.all_solid.byte(), 0xe7);
        assert!(output.all_solid.is_set());
        assert!(!output.start_solid.is_set());
        assert!(output.in_open.is_set());
        assert!(output.in_water.is_set());
    }
}
