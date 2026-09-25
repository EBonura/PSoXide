//! Shared parametric brush primitives: ramps, pillars, arches, curved walls
//! and stairs, all expanded from an axis-aligned box plus a small recipe.
//!
//! These moved down here from the editor's drag tool so that the GUI and the
//! authoring MCP expand one implementation rather than two that drift. The
//! generators themselves are unchanged; what is new is [`generate`], which
//! reports why a recipe produced nothing.
//!
//! Every generator snaps its vertices to the working grid, which is what makes
//! validation necessary: a curve whose vertices quantize onto the grid can
//! silently lose segments (see [`surviving_pillar_sides`]).

use crate::brush::Brush;

/// Which parametric solid a recipe expands to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BrushDrawShape {
    /// Plain axis-aligned box.
    #[default]
    Box,
    /// Triangular prism rising towards `direction`.
    Ramp,
    /// N-sided vertical prism: the pillar primitive.
    Cylinder,
    /// Half-elliptical arch of voussoirs, with two straight legs.
    DoorwayArch,
    /// Arc of wall segments swept around the box centre.
    CurvedWall,
    /// Stack of box steps climbing towards `direction`.
    Stairs,
}

impl BrushDrawShape {
    /// Every shape, in toolbar order.
    pub const ALL: [Self; 6] = [
        Self::Box,
        Self::Ramp,
        Self::Cylinder,
        Self::DoorwayArch,
        Self::CurvedWall,
        Self::Stairs,
    ];

    /// Human-readable name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Box => "Box",
            Self::Ramp => "Ramp",
            Self::Cylinder => "Cylinder",
            Self::DoorwayArch => "Doorway Arch",
            Self::CurvedWall => "Curved Wall",
            Self::Stairs => "Stairs",
        }
    }

    /// Whether this shape expands to more than one convex brush.
    pub const fn is_multi_brush(self) -> bool {
        matches!(self, Self::DoorwayArch | Self::CurvedWall | Self::Stairs)
    }

    /// Parse a tool argument.
    pub fn parse(value: &str) -> Option<Self> {
        let key: String = value
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Self::ALL
            .into_iter()
            .find(|shape| {
                let label: String = shape
                    .label()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                label == key
            })
            .or(match key.as_str() {
                "pillar" | "column" => Some(Self::Cylinder),
                "arch" | "doorway" => Some(Self::DoorwayArch),
                "stair" | "steps" => Some(Self::Stairs),
                _ => None,
            })
    }
}

/// Which way a directional shape faces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BrushCardinalDirection {
    /// -Z
    #[default]
    North,
    /// +X
    East,
    /// +Z
    South,
    /// -X
    West,
}

impl BrushCardinalDirection {
    /// Every direction, in toolbar order.
    pub const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// Human-readable name including the world axis.
    pub const fn label(self) -> &'static str {
        match self {
            Self::North => "North (-Z)",
            Self::East => "East (+X)",
            Self::South => "South (+Z)",
            Self::West => "West (-X)",
        }
    }

    /// Parse a tool argument.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "north" | "-z" => Some(Self::North),
            "east" | "+x" => Some(Self::East),
            "south" | "+z" => Some(Self::South),
            "west" | "-x" => Some(Self::West),
            _ => None,
        }
    }
}

/// The recipe a shape expands with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushDrawSettings {
    /// Which solid to build.
    pub shape: BrushDrawShape,
    /// Facing, for Ramp, DoorwayArch, CurvedWall and Stairs.
    pub direction: BrushCardinalDirection,
    /// Sides of a Cylinder pillar, clamped to 3..=32.
    pub cylinder_sides: u8,
    /// Voussoirs per arch, or segments per curved wall.
    pub arch_segments: u8,
    /// Band thickness for DoorwayArch and CurvedWall, world units.
    pub arch_thickness: u16,
    /// Arc swept by a CurvedWall, clamped to 90..=360 degrees.
    pub curved_wall_arc_degrees: u16,
    /// Steps in a Stairs run, clamped to 1..=32.
    pub stair_steps: u8,
}

impl Default for BrushDrawSettings {
    fn default() -> Self {
        Self {
            shape: BrushDrawShape::Box,
            direction: BrushCardinalDirection::North,
            cylinder_sides: 8,
            arch_segments: 6,
            arch_thickness: 32,
            curved_wall_arc_degrees: 90,
            stair_steps: 8,
        }
    }
}

/// Round a coordinate to the nearest multiple of the working grid.
pub fn primitive_snap(value: f64, step: i32) -> i32 {
    let step = f64::from(step.max(1));
    ((value / step).round() * step).clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Grid-snapped band thickness, never thinner than one grid step.
pub fn primitive_thickness(settings: BrushDrawSettings, step: i32) -> i32 {
    primitive_snap(f64::from(settings.arch_thickness.max(1)), step).max(step.max(1))
}

/// Triangular prism filling the box, rising towards `direction`.
pub fn ramp(
    min: [i32; 3],
    max: [i32; 3],
    direction: BrushCardinalDirection,
) -> Option<Brush> {
    use BrushCardinalDirection::{East, North, South, West};
    match direction {
        East => Brush::convex_prism(
            &[[min[0], min[1]], [max[0], min[1]], [max[0], max[1]]],
            [0, 1],
            2,
            [min[2], max[2]],
        ),
        West => Brush::convex_prism(
            &[[min[0], min[1]], [max[0], min[1]], [min[0], max[1]]],
            [0, 1],
            2,
            [min[2], max[2]],
        ),
        South => Brush::convex_prism(
            &[[min[2], min[1]], [max[2], min[1]], [max[2], max[1]]],
            [2, 1],
            0,
            [min[0], max[0]],
        ),
        North => Brush::convex_prism(
            &[[min[2], min[1]], [max[2], min[1]], [min[2], max[1]]],
            [2, 1],
            0,
            [min[0], max[0]],
        ),
    }
}

/// One N-sided vertical prism inscribed in the box: the pillar primitive.
///
/// Vertices are snapped individually, so the result can have fewer sides than
/// requested. [`generate`] reports when that happens.
pub fn cylinder(
    min: [i32; 3],
    max: [i32; 3],
    sides: u8,
    step: i32,
) -> Option<Brush> {
    let sides = usize::from(sides.clamp(3, 32));
    let center_x = (f64::from(min[0]) + f64::from(max[0])) * 0.5;
    let center_z = (f64::from(min[2]) + f64::from(max[2])) * 0.5;
    let radius_x = f64::from(max[0] - min[0]) * 0.5;
    let radius_z = f64::from(max[2] - min[2]) * 0.5;
    let polygon: Vec<_> = (0..sides)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / sides as f64;
            [
                primitive_snap(center_x + radius_x * angle.cos(), step),
                primitive_snap(center_z + radius_z * angle.sin(), step),
            ]
        })
        .collect();
    Brush::convex_prism(&polygon, [0, 2], 1, [min[1], max[1]])
}

/// Half-elliptical arch: `arch_segments` voussoirs plus two straight legs.
///
/// Returns an empty vector when the band is thicker than the opening's radius
/// or rise. [`generate`] turns that into an error carrying the numbers.
pub fn doorway_arch(
    min: [i32; 3],
    max: [i32; 3],
    settings: BrushDrawSettings,
    step: i32,
) -> Vec<Brush> {
    let facing_x = matches!(
        settings.direction,
        BrushCardinalDirection::East | BrushCardinalDirection::West
    );
    let width_axis = if facing_x { 2 } else { 0 };
    let depth_axis = if facing_x { 0 } else { 2 };
    let width_min = min[width_axis];
    let width_max = max[width_axis];
    let center = (f64::from(width_min) + f64::from(width_max)) * 0.5;
    let radius = f64::from(width_max - width_min) * 0.5;
    let vertical_radius = radius.min(f64::from(max[1] - min[1]));
    let thickness = f64::from(primitive_thickness(settings, step));
    if radius <= thickness || vertical_radius <= thickness {
        return Vec::new();
    }
    let spring = f64::from(max[1]) - vertical_radius;
    let inner_radius = radius - thickness;
    let inner_vertical_radius = vertical_radius - thickness;
    let plane_axes = [width_axis, 1];
    let depth = [min[depth_axis], max[depth_axis]];
    let segments = usize::from(settings.arch_segments.clamp(2, 24));
    let point = |angle: f64, horizontal_radius: f64, vertical_radius: f64| {
        [
            primitive_snap(center + horizontal_radius * angle.cos(), step),
            primitive_snap(spring + vertical_radius * angle.sin(), step),
        ]
    };
    let mut brushes = Vec::with_capacity(segments + 2);
    for index in 0..segments {
        let a0 = std::f64::consts::PI * index as f64 / segments as f64;
        let a1 = std::f64::consts::PI * (index + 1) as f64 / segments as f64;
        let polygon = [
            point(a0, inner_radius, inner_vertical_radius),
            point(a0, radius, vertical_radius),
            point(a1, radius, vertical_radius),
            point(a1, inner_radius, inner_vertical_radius),
        ];
        if let Some(brush) =
            Brush::convex_prism(&polygon, plane_axes, depth_axis, depth)
        {
            brushes.push(brush);
        }
    }

    let spring = primitive_snap(spring, step);
    let inner_left = primitive_snap(center - inner_radius, step);
    let inner_right = primitive_snap(center + inner_radius, step);
    for polygon in [
        [
            [width_min, min[1]],
            [inner_left, min[1]],
            [inner_left, spring],
            [width_min, spring],
        ],
        [
            [inner_right, min[1]],
            [width_max, min[1]],
            [width_max, spring],
            [inner_right, spring],
        ],
    ] {
        if let Some(brush) =
            Brush::convex_prism(&polygon, plane_axes, depth_axis, depth)
        {
            brushes.push(brush);
        }
    }
    brushes
}

/// Arc of wall segments swept around the box centre through
/// `curved_wall_arc_degrees`, facing `direction`.
pub fn curved_wall(
    min: [i32; 3],
    max: [i32; 3],
    settings: BrushDrawSettings,
    step: i32,
) -> Vec<Brush> {
    let radius_x = f64::from(max[0] - min[0]) * 0.5;
    let radius_z = f64::from(max[2] - min[2]) * 0.5;
    let thickness = f64::from(primitive_thickness(settings, step));
    if radius_x <= thickness || radius_z <= thickness {
        return Vec::new();
    }
    let center_x = (f64::from(min[0]) + f64::from(max[0])) * 0.5;
    let center_z = (f64::from(min[2]) + f64::from(max[2])) * 0.5;
    let inner_x = radius_x - thickness;
    let inner_z = radius_z - thickness;
    let arc = f64::from(settings.curved_wall_arc_degrees.clamp(90, 360)).to_radians();
    let facing = match settings.direction {
        BrushCardinalDirection::North => -std::f64::consts::FRAC_PI_2,
        BrushCardinalDirection::East => 0.0,
        BrushCardinalDirection::South => std::f64::consts::FRAC_PI_2,
        BrushCardinalDirection::West => std::f64::consts::PI,
    };
    let start = facing - arc * 0.5;
    let segments = usize::from(settings.arch_segments.clamp(2, 32));
    let point = |angle: f64, rx: f64, rz: f64| {
        [
            primitive_snap(center_x + rx * angle.cos(), step),
            primitive_snap(center_z + rz * angle.sin(), step),
        ]
    };
    let mut brushes = Vec::with_capacity(segments);
    for index in 0..segments {
        let a0 = start + arc * index as f64 / segments as f64;
        let a1 = start + arc * (index + 1) as f64 / segments as f64;
        let polygon = [
            point(a0, inner_x, inner_z),
            point(a0, radius_x, radius_z),
            point(a1, radius_x, radius_z),
            point(a1, inner_x, inner_z),
        ];
        if let Some(brush) =
            Brush::convex_prism(&polygon, [0, 2], 1, [min[1], max[1]])
        {
            brushes.push(brush);
        }
    }
    brushes
}

/// Stack of box steps climbing the box towards `direction`.
pub fn stairs(
    min: [i32; 3],
    max: [i32; 3],
    settings: BrushDrawSettings,
    step: i32,
) -> Vec<Brush> {
    let (run_axis, positive) = match settings.direction {
        BrushCardinalDirection::North => (2, false),
        BrushCardinalDirection::East => (0, true),
        BrushCardinalDirection::South => (2, true),
        BrushCardinalDirection::West => (0, false),
    };
    let run = max[run_axis] - min[run_axis];
    let rise = max[1] - min[1];
    let available_steps = (run / step.max(1)).max(1) as usize;
    let steps = usize::from(settings.stair_steps.clamp(1, 32)).min(available_steps);
    let mut brushes = Vec::with_capacity(steps);
    for index in 0..steps {
        let run0 = primitive_snap(
            f64::from(min[run_axis]) + f64::from(run) * index as f64 / steps as f64,
            step,
        );
        let run1 = primitive_snap(
            f64::from(min[run_axis]) + f64::from(run) * (index + 1) as f64 / steps as f64,
            step,
        );
        let level = if positive { index + 1 } else { steps - index };
        let top = primitive_snap(
            f64::from(min[1]) + f64::from(rise) * level as f64 / steps as f64,
            step,
        );
        let mut step_min = min;
        let mut step_max = max;
        step_min[run_axis] = run0;
        step_max[run_axis] = run1;
        step_max[1] = top;
        if let Some(brush) =
            Brush::cuboid_from_corners(step_min, step_max)
        {
            brushes.push(brush);
        }
    }
    brushes
}

/// How many sides a pillar of this footprint actually keeps on `step`.
///
/// Built rather than derived, on purpose. The obvious closed form is the
/// chord condition `2*r*sin(pi/n) > step`, which says when two neighbouring
/// vertices stay distinct. That is necessary and NOT sufficient: snapping
/// also drags vertices into a straight line, and `convex_prism` discards
/// collinear points and rejects non-convex turns. A radius-128 octagon on a
/// 64 grid passes the chord test comfortably and still collapses to a
/// diamond, because its snapped vertices are exactly collinear in pairs.
/// Only building the thing answers the question.
pub fn surviving_pillar_sides(width: i32, depth: i32, sides: u8, step: i32) -> usize {
    cylinder([0, 0, 0], [width, step.max(1), depth], sides, step)
        // n sides plus a cap at each end.
        .map_or(0, |brush| brush.faces.len().saturating_sub(2))
}

/// Smallest square footprint that keeps every side of an n-gon on `step`.
///
/// Searched by building, for the reason in [`surviving_pillar_sides`]. Widths
/// are walked in grid steps and capped so a hopeless request terminates.
pub fn minimum_pillar_footprint(sides: u8, step: i32) -> Option<i32> {
    let step = step.max(1);
    let wanted = usize::from(sides.clamp(3, 32));
    (1..=256)
        .map(|multiple| multiple * step)
        .find(|width| surviving_pillar_sides(*width, *width, sides, step) >= wanted)
}

/// What [`generate`] actually built, as opposed to what was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedPrimitive {
    /// The convex brushes to add to the scene.
    pub brushes: Vec<Brush>,
    /// Notes worth surfacing: segments lost to snapping, clamped inputs.
    pub warnings: Vec<String>,
}

impl GeneratedPrimitive {
    /// Total face count across every brush, the figure that drives draw cost.
    pub fn face_count(&self) -> usize {
        self.brushes.iter().map(|brush| brush.faces.len()).sum()
    }
}

/// Expand a recipe over an axis-aligned box, reporting why it failed.
///
/// The generators themselves return an empty `Vec` or `None` on bad input,
/// which is fine for a mouse drag (the user sees nothing appear and adjusts)
/// and useless for an agent. This wrapper turns every silent failure into an
/// error carrying the arithmetic, and counts segments lost to grid snapping.
pub fn generate(
    min: [i32; 3],
    max: [i32; 3],
    settings: BrushDrawSettings,
    step: i32,
) -> Result<GeneratedPrimitive, String> {
    if (0..3).any(|axis| min[axis] >= max[axis]) {
        return Err(format!(
            "the box has no volume: min {min:?} is not strictly below max {max:?}"
        ));
    }
    let step = step.max(1);
    let mut warnings = Vec::new();
    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];

    let brushes = match settings.shape {
        BrushDrawShape::Box => vec![Brush::cuboid(min, max)],
        BrushDrawShape::Ramp => ramp(min, max, settings.direction)
            .map(|brush| vec![brush])
            .ok_or_else(|| "the ramp collapsed: the box is too thin on one axis".to_string())?,
        BrushDrawShape::Cylinder => {
            let sides = settings.cylinder_sides.clamp(3, 32);
            if settings.cylinder_sides != sides {
                warnings.push(format!(
                    "cylinder_sides clamped to {sides} (valid range 3..=32)"
                ));
            }
            let brush = cylinder(min, max, sides, step)
                .ok_or_else(|| "the pillar collapsed after snapping".to_string())?;
            let built = brush.faces.len().saturating_sub(2);
            if built < usize::from(sides) {
                // The authoritative answer, not a predicted one: this is a
                // silent square-instead-of-octagon, made visible.
                let advice = match minimum_pillar_footprint(sides, step) {
                    Some(width) => format!(
                        "widen the footprint to {width} units, or drop the grid to {}",
                        step / 2
                    ),
                    None => format!("drop the grid below {step}"),
                };
                warnings.push(format!(
                    "asked for a {sides}-sided pillar, the {step}-unit grid allows only \
                     {built} here: {advice}"
                ));
            }
            vec![brush]
        }
        BrushDrawShape::DoorwayArch => {
            let facing_x = matches!(
                settings.direction,
                BrushCardinalDirection::East | BrushCardinalDirection::West
            );
            let width = if facing_x { size[2] } else { size[0] };
            let radius = f64::from(width) * 0.5;
            let vertical_radius = radius.min(f64::from(size[1]));
            let thickness = f64::from(primitive_thickness(settings, step));
            if radius <= thickness || vertical_radius <= thickness {
                return Err(format!(
                    "arch thickness {thickness:.0} must be less than both the span radius \
                     {radius:.0} and the rise {vertical_radius:.0}. Thin the band, widen the \
                     opening to more than {:.0}, or raise the box above {:.0}.",
                    thickness * 2.0,
                    thickness
                ));
            }
            if vertical_radius < radius {
                warnings.push(format!(
                    "the box is wider than it is tall, so this is a segmental arch \
                     (rise {vertical_radius:.0} against span radius {radius:.0}), not a semicircle"
                ));
            }
            let segments = u32::from(settings.arch_segments.clamp(2, 24));
            let built = doorway_arch(min, max, settings, step);
            // segments voussoirs plus the two legs.
            let expected = segments as usize + 2;
            if built.len() < expected {
                warnings.push(format!(
                    "{} of {expected} arch brushes survived grid snapping; \
                     coarsen to fewer segments or enlarge the opening",
                    built.len()
                ));
            }
            if built.is_empty() {
                return Err("every arch segment collapsed after snapping".to_string());
            }
            built
        }
        BrushDrawShape::CurvedWall => {
            let thickness = f64::from(primitive_thickness(settings, step));
            let radius_x = f64::from(size[0]) * 0.5;
            let radius_z = f64::from(size[2]) * 0.5;
            if radius_x <= thickness || radius_z <= thickness {
                return Err(format!(
                    "curved-wall thickness {thickness:.0} must be less than both radii \
                     ({radius_x:.0} and {radius_z:.0}); thin the band or widen the box"
                ));
            }
            let built = curved_wall(min, max, settings, step);
            if built.is_empty() {
                return Err("every curved-wall segment collapsed after snapping".to_string());
            }
            let segments = usize::from(settings.arch_segments.clamp(2, 32));
            if built.len() < segments {
                warnings.push(format!(
                    "{} of {segments} wall segments survived grid snapping",
                    built.len()
                ));
            }
            built
        }
        BrushDrawShape::Stairs => {
            let run_axis = match settings.direction {
                BrushCardinalDirection::North | BrushCardinalDirection::South => 2,
                BrushCardinalDirection::East | BrushCardinalDirection::West => 0,
            };
            let available = (size[run_axis] / step).max(1) as usize;
            let asked = usize::from(settings.stair_steps.clamp(1, 32));
            if asked > available {
                warnings.push(format!(
                    "{asked} steps do not fit in a {}-unit run on a {step}-unit grid; \
                     building {available}",
                    size[run_axis]
                ));
            }
            let built = stairs(min, max, settings, step);
            if built.is_empty() {
                return Err("the stair run produced no steps".to_string());
            }
            built
        }
    };

    if brushes.is_empty() {
        return Err("the recipe produced no brushes".to_string());
    }
    Ok(GeneratedPrimitive { brushes, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failures this module exists to catch: every one of them used to be
    /// an empty vector or a quietly smaller polygon.
    #[test]
    fn generate_reports_what_the_drag_tool_swallowed() {
        let settings = |shape| BrushDrawSettings {
            shape,
            ..BrushDrawSettings::default()
        };

        // A box always works, and is six faces.
        let boxed = generate([0, 0, 0], [512, 512, 512], settings(BrushDrawShape::Box), 64)
            .expect("a box with volume builds");
        assert_eq!(boxed.face_count(), 6);
        assert!(boxed.warnings.is_empty());

        // The headline case. A radius-128 octagon clears the chord condition
        // (2*128*sin(pi/8) = 98 > 64) and STILL collapses to a diamond,
        // because snapping puts its vertices in collinear pairs. The drag
        // tool builds this silently; generate() says so.
        let squashed = generate(
            [0, 0, 0],
            [256, 512, 256],
            settings(BrushDrawShape::Cylinder),
            64,
        )
        .expect("it still builds, it is just not an octagon");
        assert_eq!(squashed.face_count(), 6, "4 sides + 2 caps");
        assert!(
            squashed.warnings.iter().any(|w| w.contains("asked for a 8-sided")),
            "{:?}",
            squashed.warnings
        );

        // Taking the advice produces the octagon that was asked for.
        let width = minimum_pillar_footprint(8, 64).expect("an octagon fits on a 64 grid");
        let pillar = generate(
            [0, 0, 0],
            [width, 512, width],
            settings(BrushDrawShape::Cylinder),
            64,
        )
        .expect("a wide enough octagon builds");
        assert_eq!(pillar.face_count(), 10, "8 sides + 2 caps");
        assert!(pillar.warnings.is_empty(), "{:?}", pillar.warnings);

        // The arch's silent-empty case: thickness at or over the radius.
        let fat = BrushDrawSettings {
            shape: BrushDrawShape::DoorwayArch,
            arch_thickness: 4096,
            ..BrushDrawSettings::default()
        };
        let refused = generate([0, 0, 0], [1024, 1024, 256], fat, 64)
            .expect_err("an over-thick arch band must be refused");
        assert!(refused.contains("thickness"), "{refused}");

        // A workable arch builds its voussoirs plus two legs.
        let arch = BrushDrawSettings {
            shape: BrushDrawShape::DoorwayArch,
            arch_segments: 4,
            arch_thickness: 128,
            ..BrushDrawSettings::default()
        };
        let built = generate([0, 0, 0], [2048, 2048, 512], arch, 64).expect("an arch builds");
        assert_eq!(built.brushes.len(), 6, "4 voussoirs + 2 legs");

        // A box with no volume is an error, not an empty vector.
        assert!(generate([0, 0, 0], [0, 512, 512], settings(BrushDrawShape::Box), 64).is_err());

        // Stairs that cannot fit report the count they actually built.
        let cramped = BrushDrawSettings {
            shape: BrushDrawShape::Stairs,
            stair_steps: 32,
            ..BrushDrawSettings::default()
        };
        let steps = generate([0, 0, 0], [512, 512, 256], cramped, 64).expect("stairs build");
        assert!(
            steps.warnings.iter().any(|w| w.contains("do not fit")),
            "{:?}",
            steps.warnings
        );
    }
}
