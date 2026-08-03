//! Deterministic tile-native ArchProp geometry shared by editor preview and
//! playtest cooking.
//!
//! An ArchProp is an extruded curved band, not a boolean operation on room
//! geometry. Its X/Z footprint is expressed in whole room sectors, its authored
//! vertical controls are expressed in [`HEIGHT_QUANTUM`] steps, and the shared
//! generator expands that compact recipe into a bounded set of outward-wound
//! quads. Runtime code therefore performs no curve sampling.

use serde::{Deserialize, Serialize};

use crate::{GridUvTransform, ResourceId, HEIGHT_QUANTUM};

pub const ARCH_PROP_MATERIAL_COUNT: usize = 4;
pub const ARCH_PROP_MATERIAL_NAMES: [&str; ARCH_PROP_MATERIAL_COUNT] =
    ["Fascia", "Soffit", "Extrados", "End caps"];

pub const ARCH_PROP_MATERIAL_FASCIA: u8 = 0;
pub const ARCH_PROP_MATERIAL_SOFFIT: u8 = 1;
pub const ARCH_PROP_MATERIAL_EXTRADOS: u8 = 2;
pub const ARCH_PROP_MATERIAL_END_CAP: u8 = 3;

pub const ARCH_PROP_MIN_TILES: u8 = 1;
pub const ARCH_PROP_MAX_TILES: u8 = 3;
pub const ARCH_PROP_MIN_SEGMENTS_PER_QUADRANT: u8 = 2;
pub const ARCH_PROP_MAX_SEGMENTS_PER_QUADRANT: u8 = 6;
pub const DEFAULT_ARCH_PROP_SEGMENTS_PER_QUADRANT: u8 = 3;
pub const ARCH_PROP_MAX_HEIGHT_QUANTA: u16 = 255;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchPortion {
    #[default]
    Full,
    LeftHalf,
    RightHalf,
}

impl ArchPortion {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::LeftHalf => "Left half",
            Self::RightHalf => "Right half",
        }
    }
}

/// Curve family used by the arch profile.
///
/// Keeping this enum in the serialized recipe lets later pointed, segmental,
/// or horseshoe profiles reuse the same placement, material, cook, and runtime
/// pipeline. The first implementation deliberately exposes only the cheap
/// round/elliptical profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchCurve {
    #[default]
    Round,
}

impl ArchCurve {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Round => "Round / elliptical",
        }
    }
}

/// Compact authoring recipe for one tile-snapped procedural arch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchPropGeometry {
    /// Total outer span along local X, in whole room sectors.
    #[serde(default = "default_arch_span_tiles")]
    pub span_tiles: u8,
    /// Extrusion depth along local Z, in whole room sectors.
    #[serde(default = "default_arch_depth_tiles")]
    pub depth_tiles: u8,
    /// Crown rise above the spring line, in [`HEIGHT_QUANTUM`] steps.
    #[serde(default = "default_arch_rise_quanta")]
    pub rise_quanta: u16,
    /// Optional straight support height below the spring line.
    #[serde(default = "default_arch_leg_height_quanta")]
    pub leg_height_quanta: u16,
    /// Thickness between the inner and outer curves.
    #[serde(default = "default_arch_band_thickness_quanta")]
    pub band_thickness_quanta: u16,
    /// Fill the spandrel between the outer curve and a flat crown-height top.
    ///
    /// This turns the arch into a complete rectangular archway insert that can
    /// meet flat floor/ceiling tiles without leaving curved gaps above it.
    #[serde(default)]
    pub filled_top: bool,
    /// Full arch or one cardinal half of the same curve.
    #[serde(default)]
    pub portion: ArchPortion,
    /// Curve family. Round is the only first-pass variant.
    #[serde(default)]
    pub curve: ArchCurve,
    /// Low-poly curve detail. A full arch uses twice this many segments.
    #[serde(default = "default_arch_segments_per_quadrant")]
    pub segments_per_quadrant: u8,
}

impl Default for ArchPropGeometry {
    fn default() -> Self {
        Self {
            span_tiles: default_arch_span_tiles(),
            depth_tiles: default_arch_depth_tiles(),
            rise_quanta: default_arch_rise_quanta(),
            leg_height_quanta: default_arch_leg_height_quanta(),
            band_thickness_quanta: default_arch_band_thickness_quanta(),
            filled_top: false,
            portion: ArchPortion::Full,
            curve: ArchCurve::Round,
            segments_per_quadrant: default_arch_segments_per_quadrant(),
        }
    }
}

const fn default_arch_span_tiles() -> u8 {
    2
}

const fn default_arch_depth_tiles() -> u8 {
    1
}

const fn default_arch_rise_quanta() -> u16 {
    16
}

const fn default_arch_leg_height_quanta() -> u16 {
    16
}

const fn default_arch_band_thickness_quanta() -> u16 {
    4
}

const fn default_arch_segments_per_quadrant() -> u8 {
    DEFAULT_ARCH_PROP_SEGMENTS_PER_QUADRANT
}

pub(crate) const fn default_arch_prop_materials() -> [Option<ResourceId>; ARCH_PROP_MATERIAL_COUNT]
{
    [None; ARCH_PROP_MATERIAL_COUNT]
}

pub(crate) const fn default_arch_prop_uvs() -> [GridUvTransform; ARCH_PROP_MATERIAL_COUNT] {
    [GridUvTransform::IDENTITY; ARCH_PROP_MATERIAL_COUNT]
}

/// One generated ArchProp quad in node-local engine units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedArchPropSurface {
    pub vertices: [[i16; 3]; 4],
    pub uv_q8: [[u8; 2]; 4],
    pub material_slot: u8,
}

/// One conservative node-local collision box. A curved band is represented by
/// one box per profile segment instead of one box around the whole arch, so the
/// opening remains traversable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedArchPropCollisionBox {
    pub min: [i16; 3],
    pub max: [i16; 3],
}

#[derive(Debug, Clone, Copy)]
struct ProfilePoint {
    outer: [i32; 2],
    inner: [i32; 2],
    path_q8: u8,
}

/// Expand one compact arch recipe into a small deterministic list of quads.
///
/// `sector_size` is inherited from the enclosing room. The public recipe keeps
/// horizontal dimensions as tile counts so changing a world's sector size
/// preserves alignment rather than leaving stale absolute-width props behind.
pub fn generate_arch_prop_surfaces(
    geometry: ArchPropGeometry,
    sector_size: i32,
) -> Vec<GeneratedArchPropSurface> {
    let sector_size = sector_size.max(1);
    let span_tiles = i32::from(
        geometry
            .span_tiles
            .clamp(ARCH_PROP_MIN_TILES, ARCH_PROP_MAX_TILES),
    );
    let depth_tiles = i32::from(
        geometry
            .depth_tiles
            .clamp(ARCH_PROP_MIN_TILES, ARCH_PROP_MAX_TILES),
    );
    let half_span = span_tiles.saturating_mul(sector_size) / 2;
    let half_depth = depth_tiles.saturating_mul(sector_size) / 2;
    let rise = i32::from(geometry.rise_quanta.clamp(1, ARCH_PROP_MAX_HEIGHT_QUANTA))
        .saturating_mul(HEIGHT_QUANTUM);
    let leg_height = i32::from(geometry.leg_height_quanta.min(ARCH_PROP_MAX_HEIGHT_QUANTA))
        .saturating_mul(HEIGHT_QUANTUM);
    let requested_thickness = i32::from(
        geometry
            .band_thickness_quanta
            .clamp(1, ARCH_PROP_MAX_HEIGHT_QUANTA),
    )
    .saturating_mul(HEIGHT_QUANTUM);
    let thickness = requested_thickness
        .min(half_span.saturating_sub(HEIGHT_QUANTUM).max(1))
        .min(rise.saturating_sub(HEIGHT_QUANTUM).max(1));
    let inner_half_span = (half_span - thickness).max(1);
    let inner_rise = (rise - thickness).max(1);
    let segments_per_quadrant = usize::from(geometry.segments_per_quadrant.clamp(
        ARCH_PROP_MIN_SEGMENTS_PER_QUADRANT,
        ARCH_PROP_MAX_SEGMENTS_PER_QUADRANT,
    ));
    let (start_step, end_step, total_steps) = match geometry.portion {
        ArchPortion::Full => (0usize, segments_per_quadrant * 2, segments_per_quadrant * 2),
        ArchPortion::LeftHalf => (0usize, segments_per_quadrant, segments_per_quadrant * 2),
        ArchPortion::RightHalf => (
            segments_per_quadrant,
            segments_per_quadrant * 2,
            segments_per_quadrant * 2,
        ),
    };

    let mut points = Vec::with_capacity(end_step - start_step + 1);
    for step in start_step..=end_step {
        points.push(profile_point(
            geometry.curve,
            step,
            total_steps,
            half_span,
            inner_half_span,
            rise,
            inner_rise,
            leg_height,
        ));
    }

    let selected_segments = points.len().saturating_sub(1);
    let leg_count = if leg_height > 0 {
        match geometry.portion {
            ArchPortion::Full => 2,
            ArchPortion::LeftHalf | ArchPortion::RightHalf => 1,
        }
    } else {
        0
    };
    let total_height = leg_height.saturating_add(rise);
    let fill_capacity = if geometry.filled_top {
        selected_segments * 2 + 3
    } else {
        0
    };
    let mut surfaces =
        Vec::with_capacity(selected_segments * 4 + leg_count * 6 + 2 + fill_capacity);

    for pair in points.windows(2) {
        append_band_segment(
            &mut surfaces,
            pair[0],
            pair[1],
            half_depth,
            half_span,
            total_height,
            !geometry.filled_top,
        );
        if geometry.filled_top {
            append_spandrel_segment(
                &mut surfaces,
                pair[0],
                pair[1],
                half_depth,
                half_span,
                total_height,
            );
        }
    }

    if geometry.filled_top {
        let first = *points.first().expect("arch has profile points");
        let last = *points.last().expect("arch has profile points");
        append_flat_arch_top(
            &mut surfaces,
            first.outer[0],
            last.outer[0],
            total_height,
            half_depth,
        );
        append_spandrel_end_cap(&mut surfaces, first, total_height, half_depth, [-1, 0, 0]);
        append_spandrel_end_cap(&mut surfaces, last, total_height, half_depth, [1, 0, 0]);
    }

    let has_left = matches!(geometry.portion, ArchPortion::Full | ArchPortion::LeftHalf);
    let has_right = matches!(geometry.portion, ArchPortion::Full | ArchPortion::RightHalf);
    if leg_height > 0 {
        if has_left {
            append_leg(
                &mut surfaces,
                -half_span,
                -inner_half_span,
                leg_height,
                half_depth,
                half_span,
                total_height,
                true,
            );
        }
        if has_right {
            append_leg(
                &mut surfaces,
                inner_half_span,
                half_span,
                leg_height,
                half_depth,
                half_span,
                total_height,
                false,
            );
        }
    } else {
        if has_left {
            append_profile_end_cap(&mut surfaces, points[0], half_depth, [-1, 0, 0]);
        }
        if has_right {
            append_profile_end_cap(
                &mut surfaces,
                *points.last().expect("arch has points"),
                half_depth,
                [1, 0, 0],
            );
        }
    }

    // Half arches expose the centre cut in addition to their outer support.
    match geometry.portion {
        ArchPortion::Full => {}
        ArchPortion::LeftHalf => append_profile_end_cap(
            &mut surfaces,
            *points.last().expect("left half has points"),
            half_depth,
            [1, 0, 0],
        ),
        ArchPortion::RightHalf => {
            append_profile_end_cap(&mut surfaces, points[0], half_depth, [-1, 0, 0])
        }
    }

    surfaces
}

/// Build the bounded collision approximation used by the cooker.
///
/// The count is `segments + legs` (at most 14 with current authoring limits).
/// This intentionally follows the same sampled profile as rendering; increasing
/// visual curve detail therefore makes collision more accurate but remains
/// explicitly bounded.
pub fn generate_arch_prop_collision_boxes(
    geometry: ArchPropGeometry,
    sector_size: i32,
) -> Vec<GeneratedArchPropCollisionBox> {
    let sector_size = sector_size.max(1);
    let half_span = i32::from(
        geometry
            .span_tiles
            .clamp(ARCH_PROP_MIN_TILES, ARCH_PROP_MAX_TILES),
    )
    .saturating_mul(sector_size)
        / 2;
    let half_depth = i32::from(
        geometry
            .depth_tiles
            .clamp(ARCH_PROP_MIN_TILES, ARCH_PROP_MAX_TILES),
    )
    .saturating_mul(sector_size)
        / 2;
    let rise = i32::from(geometry.rise_quanta.clamp(1, ARCH_PROP_MAX_HEIGHT_QUANTA))
        .saturating_mul(HEIGHT_QUANTUM);
    let leg_height = i32::from(geometry.leg_height_quanta.min(ARCH_PROP_MAX_HEIGHT_QUANTA))
        .saturating_mul(HEIGHT_QUANTUM);
    let requested_thickness = i32::from(
        geometry
            .band_thickness_quanta
            .clamp(1, ARCH_PROP_MAX_HEIGHT_QUANTA),
    )
    .saturating_mul(HEIGHT_QUANTUM);
    let thickness = requested_thickness
        .min(half_span.saturating_sub(HEIGHT_QUANTUM).max(1))
        .min(rise.saturating_sub(HEIGHT_QUANTUM).max(1));
    let inner_half_span = (half_span - thickness).max(1);
    let inner_rise = (rise - thickness).max(1);
    let segments_per_quadrant = usize::from(geometry.segments_per_quadrant.clamp(
        ARCH_PROP_MIN_SEGMENTS_PER_QUADRANT,
        ARCH_PROP_MAX_SEGMENTS_PER_QUADRANT,
    ));
    let (start_step, end_step, total_steps) = match geometry.portion {
        ArchPortion::Full => (0, segments_per_quadrant * 2, segments_per_quadrant * 2),
        ArchPortion::LeftHalf => (0, segments_per_quadrant, segments_per_quadrant * 2),
        ArchPortion::RightHalf => (
            segments_per_quadrant,
            segments_per_quadrant * 2,
            segments_per_quadrant * 2,
        ),
    };

    let mut points = Vec::with_capacity(end_step - start_step + 1);
    for step in start_step..=end_step {
        points.push(profile_point(
            geometry.curve,
            step,
            total_steps,
            half_span,
            inner_half_span,
            rise,
            inner_rise,
            leg_height,
        ));
    }
    let mut boxes = Vec::with_capacity(points.len() + 2);
    for pair in points.windows(2) {
        let xs = [
            pair[0].outer[0],
            pair[0].inner[0],
            pair[1].outer[0],
            pair[1].inner[0],
        ];
        let mut ys = [
            pair[0].outer[1],
            pair[0].inner[1],
            pair[1].outer[1],
            pair[1].inner[1],
        ];
        if geometry.filled_top {
            ys[0] = leg_height.saturating_add(rise);
        }
        boxes.push(collision_box(
            *xs.iter().min().unwrap_or(&0),
            *ys.iter().min().unwrap_or(&0),
            -half_depth,
            *xs.iter().max().unwrap_or(&0),
            *ys.iter().max().unwrap_or(&0),
            half_depth,
        ));
    }
    if leg_height > 0 {
        if matches!(geometry.portion, ArchPortion::Full | ArchPortion::LeftHalf) {
            boxes.push(collision_box(
                -half_span,
                0,
                -half_depth,
                -inner_half_span,
                leg_height,
                half_depth,
            ));
        }
        if matches!(geometry.portion, ArchPortion::Full | ArchPortion::RightHalf) {
            boxes.push(collision_box(
                inner_half_span,
                0,
                -half_depth,
                half_span,
                leg_height,
                half_depth,
            ));
        }
    }
    boxes
}

fn collision_box(
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
) -> GeneratedArchPropCollisionBox {
    GeneratedArchPropCollisionBox {
        min: [clamp_i16(min_x), clamp_i16(min_y), clamp_i16(min_z)],
        max: [clamp_i16(max_x), clamp_i16(max_y), clamp_i16(max_z)],
    }
}

fn profile_point(
    curve: ArchCurve,
    step: usize,
    total_steps: usize,
    half_span: i32,
    inner_half_span: i32,
    rise: i32,
    inner_rise: i32,
    leg_height: i32,
) -> ProfilePoint {
    let t = step as f32 / total_steps.max(1) as f32;
    let angle = core::f32::consts::PI * (1.0 - t);
    let (cos, sin) = match curve {
        ArchCurve::Round => (angle.cos(), angle.sin()),
    };
    ProfilePoint {
        outer: [
            (cos * half_span as f32).round() as i32,
            leg_height + snap_generated_height((sin * rise as f32).round() as i32),
        ],
        inner: [
            (cos * inner_half_span as f32).round() as i32,
            leg_height + snap_generated_height((sin * inner_rise as f32).round() as i32),
        ],
        path_q8: fraction_u8(step, total_steps),
    }
}

fn append_band_segment(
    out: &mut Vec<GeneratedArchPropSurface>,
    a: ProfilePoint,
    b: ProfilePoint,
    half_depth: i32,
    half_span: i32,
    total_height: i32,
    include_extrados: bool,
) {
    let facade_uv = |point: [i32; 2]| {
        [
            normalize_q8(point[0], -half_span, half_span),
            255u8.saturating_sub(normalize_q8(point[1], 0, total_height)),
        ]
    };
    let front = [
        point3(a.outer, -half_depth),
        point3(a.inner, -half_depth),
        point3(b.inner, -half_depth),
        point3(b.outer, -half_depth),
    ];
    append_oriented_quad(
        out,
        front,
        [
            facade_uv(a.outer),
            facade_uv(a.inner),
            facade_uv(b.inner),
            facade_uv(b.outer),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, -1],
    );
    let back = [
        point3(a.outer, half_depth),
        point3(b.outer, half_depth),
        point3(b.inner, half_depth),
        point3(a.inner, half_depth),
    ];
    append_oriented_quad(
        out,
        back,
        [
            facade_uv(a.outer),
            facade_uv(b.outer),
            facade_uv(b.inner),
            facade_uv(a.inner),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, 1],
    );

    if include_extrados {
        append_oriented_quad(
            out,
            [
                point3(a.outer, -half_depth),
                point3(b.outer, -half_depth),
                point3(b.outer, half_depth),
                point3(a.outer, half_depth),
            ],
            [
                [a.path_q8, 0],
                [b.path_q8, 0],
                [b.path_q8, 255],
                [a.path_q8, 255],
            ],
            ARCH_PROP_MATERIAL_EXTRADOS,
            [
                a.outer[0].saturating_add(b.outer[0]),
                a.outer[1].saturating_add(b.outer[1]),
                0,
            ],
        );
    }
    append_oriented_quad(
        out,
        [
            point3(a.inner, -half_depth),
            point3(a.inner, half_depth),
            point3(b.inner, half_depth),
            point3(b.inner, -half_depth),
        ],
        [
            [a.path_q8, 0],
            [a.path_q8, 255],
            [b.path_q8, 255],
            [b.path_q8, 0],
        ],
        ARCH_PROP_MATERIAL_SOFFIT,
        [
            -a.inner[0].saturating_sub(b.inner[0]),
            -a.inner[1].saturating_sub(b.inner[1]),
            0,
        ],
    );
}

fn append_spandrel_segment(
    out: &mut Vec<GeneratedArchPropSurface>,
    a: ProfilePoint,
    b: ProfilePoint,
    half_depth: i32,
    half_span: i32,
    total_height: i32,
) {
    let facade_uv = |point: [i32; 2]| {
        [
            normalize_q8(point[0], -half_span, half_span),
            255u8.saturating_sub(normalize_q8(point[1], 0, total_height)),
        ]
    };
    let a_top = [a.outer[0], total_height];
    let b_top = [b.outer[0], total_height];
    append_oriented_quad(
        out,
        [
            point3(a.outer, -half_depth),
            point3(b.outer, -half_depth),
            point3(b_top, -half_depth),
            point3(a_top, -half_depth),
        ],
        [
            facade_uv(a.outer),
            facade_uv(b.outer),
            facade_uv(b_top),
            facade_uv(a_top),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, -1],
    );
    append_oriented_quad(
        out,
        [
            point3(a.outer, half_depth),
            point3(a_top, half_depth),
            point3(b_top, half_depth),
            point3(b.outer, half_depth),
        ],
        [
            facade_uv(a.outer),
            facade_uv(a_top),
            facade_uv(b_top),
            facade_uv(b.outer),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, 1],
    );
}

fn append_flat_arch_top(
    out: &mut Vec<GeneratedArchPropSurface>,
    x0: i32,
    x1: i32,
    y: i32,
    half_depth: i32,
) {
    append_oriented_quad(
        out,
        [
            vertex3(x0, y, -half_depth),
            vertex3(x0, y, half_depth),
            vertex3(x1, y, half_depth),
            vertex3(x1, y, -half_depth),
        ],
        [[0, 0], [0, 255], [255, 255], [255, 0]],
        ARCH_PROP_MATERIAL_EXTRADOS,
        [0, 1, 0],
    );
}

fn append_spandrel_end_cap(
    out: &mut Vec<GeneratedArchPropSurface>,
    point: ProfilePoint,
    total_height: i32,
    half_depth: i32,
    desired_normal: [i32; 3],
) {
    if point.outer[1] >= total_height {
        return;
    }
    append_oriented_quad(
        out,
        [
            point3(point.outer, -half_depth),
            point3(point.outer, half_depth),
            vertex3(point.outer[0], total_height, half_depth),
            vertex3(point.outer[0], total_height, -half_depth),
        ],
        [[0, 255], [255, 255], [255, 0], [0, 0]],
        ARCH_PROP_MATERIAL_END_CAP,
        desired_normal,
    );
}

fn append_leg(
    out: &mut Vec<GeneratedArchPropSurface>,
    x0: i32,
    x1: i32,
    height: i32,
    half_depth: i32,
    half_span: i32,
    total_height: i32,
    left: bool,
) {
    let y0 = 0;
    let y1 = height;
    let facade_uv = |x: i32, y: i32| {
        [
            normalize_q8(x, -half_span, half_span),
            255u8.saturating_sub(normalize_q8(y, 0, total_height)),
        ]
    };
    append_oriented_quad(
        out,
        [
            vertex3(x0, y0, -half_depth),
            vertex3(x1, y0, -half_depth),
            vertex3(x1, y1, -half_depth),
            vertex3(x0, y1, -half_depth),
        ],
        [
            facade_uv(x0, y0),
            facade_uv(x1, y0),
            facade_uv(x1, y1),
            facade_uv(x0, y1),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, -1],
    );
    append_oriented_quad(
        out,
        [
            vertex3(x0, y0, half_depth),
            vertex3(x0, y1, half_depth),
            vertex3(x1, y1, half_depth),
            vertex3(x1, y0, half_depth),
        ],
        [
            facade_uv(x0, y0),
            facade_uv(x0, y1),
            facade_uv(x1, y1),
            facade_uv(x1, y0),
        ],
        ARCH_PROP_MATERIAL_FASCIA,
        [0, 0, 1],
    );
    let (outer_x, inner_x, outer_normal, inner_normal) = if left {
        (x0, x1, [-1, 0, 0], [1, 0, 0])
    } else {
        (x1, x0, [1, 0, 0], [-1, 0, 0])
    };
    append_oriented_quad(
        out,
        [
            vertex3(outer_x, y0, -half_depth),
            vertex3(outer_x, y1, -half_depth),
            vertex3(outer_x, y1, half_depth),
            vertex3(outer_x, y0, half_depth),
        ],
        [[0, 255], [0, 0], [255, 0], [255, 255]],
        ARCH_PROP_MATERIAL_EXTRADOS,
        outer_normal,
    );
    append_oriented_quad(
        out,
        [
            vertex3(inner_x, y0, -half_depth),
            vertex3(inner_x, y0, half_depth),
            vertex3(inner_x, y1, half_depth),
            vertex3(inner_x, y1, -half_depth),
        ],
        [[0, 255], [255, 255], [255, 0], [0, 0]],
        ARCH_PROP_MATERIAL_SOFFIT,
        inner_normal,
    );
    append_oriented_quad(
        out,
        [
            vertex3(x0, y0, -half_depth),
            vertex3(x0, y0, half_depth),
            vertex3(x1, y0, half_depth),
            vertex3(x1, y0, -half_depth),
        ],
        [[0, 0], [0, 255], [255, 255], [255, 0]],
        ARCH_PROP_MATERIAL_END_CAP,
        [0, -1, 0],
    );
}

fn append_profile_end_cap(
    out: &mut Vec<GeneratedArchPropSurface>,
    point: ProfilePoint,
    half_depth: i32,
    desired_normal: [i32; 3],
) {
    append_oriented_quad(
        out,
        [
            point3(point.outer, -half_depth),
            point3(point.outer, half_depth),
            point3(point.inner, half_depth),
            point3(point.inner, -half_depth),
        ],
        [[0, 0], [255, 0], [255, 255], [0, 255]],
        ARCH_PROP_MATERIAL_END_CAP,
        desired_normal,
    );
}

fn append_oriented_quad(
    out: &mut Vec<GeneratedArchPropSurface>,
    mut vertices: [[i16; 3]; 4],
    mut uv_q8: [[u8; 2]; 4],
    material_slot: u8,
    desired_normal: [i32; 3],
) {
    let normal = quad_normal(vertices);
    let dot = i64::from(normal[0]) * i64::from(desired_normal[0])
        + i64::from(normal[1]) * i64::from(desired_normal[1])
        + i64::from(normal[2]) * i64::from(desired_normal[2]);
    if dot < 0 {
        vertices.swap(1, 3);
        uv_q8.swap(1, 3);
    }
    out.push(GeneratedArchPropSurface {
        vertices,
        uv_q8,
        material_slot,
    });
}

fn quad_normal(vertices: [[i16; 3]; 4]) -> [i32; 3] {
    let ab = [
        i32::from(vertices[1][0]) - i32::from(vertices[0][0]),
        i32::from(vertices[1][1]) - i32::from(vertices[0][1]),
        i32::from(vertices[1][2]) - i32::from(vertices[0][2]),
    ];
    let ac = [
        i32::from(vertices[2][0]) - i32::from(vertices[0][0]),
        i32::from(vertices[2][1]) - i32::from(vertices[0][1]),
        i32::from(vertices[2][2]) - i32::from(vertices[0][2]),
    ];
    [
        ab[1].saturating_mul(ac[2]) - ab[2].saturating_mul(ac[1]),
        ab[2].saturating_mul(ac[0]) - ab[0].saturating_mul(ac[2]),
        ab[0].saturating_mul(ac[1]) - ab[1].saturating_mul(ac[0]),
    ]
}

fn point3(point: [i32; 2], z: i32) -> [i16; 3] {
    vertex3(point[0], point[1], z)
}

fn vertex3(x: i32, y: i32, z: i32) -> [i16; 3] {
    [clamp_i16(x), clamp_i16(y), clamp_i16(z)]
}

fn snap_generated_height(value: i32) -> i32 {
    let half = HEIGHT_QUANTUM / 2;
    ((value.max(0) + half) / HEIGHT_QUANTUM) * HEIGHT_QUANTUM
}

fn normalize_q8(value: i32, min: i32, max: i32) -> u8 {
    let span = max.saturating_sub(min).max(1);
    ((value.saturating_sub(min)).saturating_mul(255) / span).clamp(0, 255) as u8
}

fn fraction_u8(index: usize, total: usize) -> u8 {
    ((index.saturating_mul(255)) / total.max(1)).min(255) as u8
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_full_arch_is_tile_sized_quantized_and_bounded() {
        let geometry = ArchPropGeometry::default();
        let sector_size = 1024;
        let surfaces = generate_arch_prop_surfaces(geometry, sector_size);
        assert_eq!(surfaces.len(), 34);
        assert!(surfaces.iter().all(|surface| {
            surface.vertices.iter().all(|vertex| {
                i32::from(vertex[0]).abs() <= sector_size
                    && i32::from(vertex[2]).abs() <= sector_size / 2
                    && i32::from(vertex[1]) >= 0
                    && i32::from(vertex[1]) % HEIGHT_QUANTUM == 0
            })
        }));
        for slot in 0..ARCH_PROP_MATERIAL_COUNT as u8 {
            assert!(surfaces.iter().any(|surface| surface.material_slot == slot));
        }
    }

    #[test]
    fn half_arches_are_mirrored_and_share_one_generator() {
        let left = ArchPropGeometry {
            portion: ArchPortion::LeftHalf,
            ..Default::default()
        };
        let mut right = left;
        right.portion = ArchPortion::RightHalf;
        let left = generate_arch_prop_surfaces(left, 1024);
        let right = generate_arch_prop_surfaces(right, 1024);
        assert_eq!(left.len(), right.len());
        assert!(left
            .iter()
            .flat_map(|surface| surface.vertices)
            .all(|vertex| vertex[0] <= 0));
        assert!(right
            .iter()
            .flat_map(|surface| surface.vertices)
            .all(|vertex| vertex[0] >= 0));
        assert_eq!(
            left.iter()
                .filter(|surface| surface.material_slot == ARCH_PROP_MATERIAL_END_CAP)
                .count(),
            2
        );
    }

    #[test]
    fn zero_leg_arch_is_an_arc_with_two_support_caps() {
        let geometry = ArchPropGeometry {
            leg_height_quanta: 0,
            ..Default::default()
        };
        let surfaces = generate_arch_prop_surfaces(geometry, 1024);
        assert_eq!(surfaces.len(), 26);
        assert_eq!(
            surfaces
                .iter()
                .filter(|surface| surface.material_slot == ARCH_PROP_MATERIAL_END_CAP)
                .count(),
            2
        );
    }

    #[test]
    fn filled_top_closes_spandrel_to_one_flat_tile_aligned_surface() {
        let geometry = ArchPropGeometry {
            filled_top: true,
            ..ArchPropGeometry::default()
        };
        let surfaces = generate_arch_prop_surfaces(geometry, 1024);
        let total_height = i16::try_from(
            i32::from(geometry.rise_quanta + geometry.leg_height_quanta) * HEIGHT_QUANTUM,
        )
        .unwrap();
        assert_eq!(surfaces.len(), 43);
        assert!(surfaces.iter().any(|surface| {
            surface.material_slot == ARCH_PROP_MATERIAL_EXTRADOS
                && surface
                    .vertices
                    .iter()
                    .all(|vertex| vertex[1] == total_height)
                && surface.vertices.iter().any(|vertex| vertex[0] == -1024)
                && surface.vertices.iter().any(|vertex| vertex[0] == 1024)
        }));
        let collisions = generate_arch_prop_collision_boxes(geometry, 1024);
        assert_eq!(collisions.len(), 8);
        assert!(collisions
            .iter()
            .any(|collision| collision.max[1] == total_height));
    }

    #[test]
    fn polygon_budget_is_bounded_by_authored_limits() {
        let geometry = ArchPropGeometry {
            span_tiles: u8::MAX,
            depth_tiles: u8::MAX,
            rise_quanta: u16::MAX,
            leg_height_quanta: u16::MAX,
            band_thickness_quanta: u16::MAX,
            filled_top: true,
            portion: ArchPortion::Full,
            curve: ArchCurve::Round,
            segments_per_quadrant: u8::MAX,
        };
        let surfaces = generate_arch_prop_surfaces(geometry, 8192);
        assert!(surfaces.len() <= 80);
        assert!(surfaces
            .iter()
            .flat_map(|surface| surface.vertices)
            .all(|vertex| vertex.iter().all(|value| *value != i16::MIN)));
    }
}
