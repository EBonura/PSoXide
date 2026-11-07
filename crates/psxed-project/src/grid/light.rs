//! Per-corner light bake of grid faces.
//!
//! Ambient plus lambert-weighted linear falloff from point lights, modulated
//! by the material tint, evaluated at each corner of a [`GridFace`]. Shadows
//! come from an occlusion query the caller supplies: the cook passes the
//! runtime collision's own line of sight, so the bake and the walkable world
//! agree about what is solid. Lights and points share one unit (editor units
//! for the editor preview, runtime units for the cook).

use serde::{Deserialize, Serialize};

use super::faces::{GridFace, GridFaceKind};

/// Offset of a shadow ray's start along the surface normal, so a corner does
/// not shadow itself.
const SHADOW_NUDGE: f64 = 0.25;
/// Below this a distance or a ray length counts as zero.
const EPSILON: f64 = 1.0 / 1024.0;
/// The tint value that leaves a texture unmodified.
const NEUTRAL: f64 = 128.0;

/// A point light the bake evaluates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLight {
    /// Position in the bake's unit.
    pub position: [f64; 3],
    /// Reach in the same unit; no light beyond it.
    pub radius: f64,
    /// Q8.8 intensity, where 256 is one authored unit.
    pub intensity_q8: u16,
    /// Light colour.
    pub color: [u8; 3],
}

impl GridLight {
    /// Whether the light can be evaluated (finite, positive radius).
    pub fn is_valid(&self) -> bool {
        self.position.into_iter().all(f64::is_finite)
            && self.radius.is_finite()
            && self.radius > 0.0
    }
}

/// Brightness the lit colour of a face is scaled to by the way it faces, a
/// fixed multiplier per facing as Tomb Raider shaded its rooms, so a step, a
/// platform or a corner reads even where one lamp lights every surface alike.
///
/// Each field is a Q8 factor (256 leaves the colour as it is). A face takes
/// the blend of the four by the squares of its normal's components, so an
/// axis-aligned face gets exactly one factor and a slope or a diagonal wall
/// something between. The same function runs in the editor preview and the
/// cook, so Play shows what the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacingShade {
    /// Faces looking up (floors).
    pub up: u16,
    /// Faces looking down (ceilings).
    pub down: u16,
    /// Faces looking along the X axis (east or west).
    pub side_x: u16,
    /// Faces looking along the Z axis (north or south).
    pub side_z: u16,
}

impl FacingShade {
    /// No shading: every face keeps its lit colour. What a world made before
    /// this setting has, so levels never change behind their author's back.
    pub const OFF: Self = Self {
        up: 256,
        down: 256,
        side_x: 256,
        side_z: 256,
    };

    /// Subtle but visible: floors as lit, ceilings and walls a little
    /// darker, the two wall directions apart from each other. What a new
    /// world gets.
    pub const SUBTLE: Self = Self {
        up: 256,
        down: 200,
        side_x: 212,
        side_z: 168,
    };

    /// A stronger version for dim levels that need more contrast.
    pub const STRONG: Self = Self {
        up: 256,
        down: 168,
        side_x: 180,
        side_z: 124,
    };

    /// Whether this leaves every colour alone.
    pub fn is_off(&self) -> bool {
        *self == Self::OFF
    }

    /// The factor (0..=1) for a face looking along the unit `normal`.
    pub fn factor(&self, normal: [f64; 3]) -> f64 {
        if self.is_off() {
            return 1.0;
        }
        let vertical = if normal[1] >= 0.0 { self.up } else { self.down };
        let weighted = normal[0] * normal[0] * f64::from(self.side_x)
            + normal[2] * normal[2] * f64::from(self.side_z)
            + normal[1] * normal[1] * f64::from(vertical);
        let total = normal.iter().map(|v| v * v).sum::<f64>();
        if total <= 0.0 {
            return 1.0;
        }
        (weighted / total / 256.0).clamp(0.0, 1.0)
    }

    /// `colour` scaled by the factor for `normal`.
    pub fn apply(&self, colour: [u8; 3], normal: [f64; 3]) -> [u8; 3] {
        if self.is_off() {
            return colour;
        }
        let k = self.factor(normal);
        colour.map(|c| (f64::from(c) * k).round().clamp(0.0, 255.0) as u8)
    }
}

impl Default for FacingShade {
    fn default() -> Self {
        Self::OFF
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn modulated(tint: u8, light: f64) -> u8 {
    (f64::from(tint) * light.clamp(0.0, 255.0) / NEUTRAL)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Lit colour of one point on a surface facing `normal` (unit length).
///
/// `occluded(from, to)` says whether the segment between two points is
/// blocked by solid world; pass `&|_, _| false` for the unshadowed look.
pub fn lit_corner(
    point: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
) -> [u8; 3] {
    lit_corner_nudged(point, normal, tint, ambient, lights, occluded, SHADOW_NUDGE)
}

/// [`lit_corner`] with an explicit start offset of the shadow ray along the
/// normal. A free plane needs a larger one than a floor: its slab collision
/// is cooked from rounded corners, so a corner can sit a unit inside it.
pub fn lit_corner_nudged(
    point: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    nudge: f64,
) -> [u8; 3] {
    let mut accumulated = ambient.map(f64::from);
    accumulate_direct(&mut accumulated, point, normal, lights, occluded, nudge);
    [
        modulated(tint[0], accumulated[0]),
        modulated(tint[1], accumulated[1]),
        modulated(tint[2], accumulated[2]),
    ]
}

/// Add the light of every lamp that reaches a point on a surface facing
/// `normal` (unit length) to `accumulated`, in the bake's 128-neutral units.
/// `nudge` is the start offset of the shadow ray along the normal.
pub fn accumulate_direct(
    accumulated: &mut [f64; 3],
    point: [f64; 3],
    normal: [f64; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    nudge: f64,
) {
    let start = [
        point[0] + normal[0] * nudge,
        point[1] + normal[1] * nudge,
        point[2] + normal[2] * nudge,
    ];
    for light in lights {
        let to_light = [
            light.position[0] - point[0],
            light.position[1] - point[1],
            light.position[2] - point[2],
        ];
        let distance_squared = dot(to_light, to_light);
        if distance_squared >= light.radius * light.radius {
            continue;
        }
        let distance = distance_squared.sqrt();
        let direction = if distance > EPSILON {
            to_light.map(|v| v / distance)
        } else {
            normal
        };
        let lambert = dot(normal, direction).max(0.0);
        if lambert <= 0.0 || occluded(start, light.position) {
            continue;
        }
        let attenuation = 1.0 - distance / light.radius;
        let weight = attenuation * lambert * f64::from(light.intensity_q8) / 256.0;
        for (channel, color) in accumulated.iter_mut().zip(light.color) {
            *channel += f64::from(color) * weight;
        }
    }
}

/// Add the light of every lamp that reaches the free point `point` to
/// `accumulated`, in the bake's 128-neutral units: the linear falloff of
/// [`accumulate_direct`] with no facing term, because a character is lit from
/// every side at once. `occluded(from, to)` is the same shadow query, asked
/// from the point itself (a character's chest is in open air, so there is no
/// surface to nudge off).
pub fn accumulate_omni(
    accumulated: &mut [f64; 3],
    point: [f64; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
) {
    for light in lights {
        let to_light = [
            light.position[0] - point[0],
            light.position[1] - point[1],
            light.position[2] - point[2],
        ];
        let distance_squared = dot(to_light, to_light);
        if distance_squared >= light.radius * light.radius || occluded(point, light.position) {
            continue;
        }
        let attenuation = 1.0 - distance_squared.sqrt() / light.radius;
        let weight = attenuation * f64::from(light.intensity_q8) / 256.0;
        for (channel, color) in accumulated.iter_mut().zip(light.color) {
            *channel += f64::from(color) * weight;
        }
    }
}

/// Shadow ray start offset of a free plane's corner, in the bake's unit when
/// that is runtime units (the cook). The editor preview casts no shadows and
/// ignores it.
pub const PLANE_SHADOW_NUDGE: f64 = 2.0;

/// Lit colour of one corner of `face`. Planes and mesh faces add two things
/// the sector faces do not have: a plane in `fullbright` shows its tint unlit, and a
/// double-sided plane is lit from whichever side the light is on.
pub fn lit_face_corner(
    face: &GridFace,
    point: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    shade: FacingShade,
) -> [u8; 3] {
    let plane = matches!(
        face.kind,
        GridFaceKind::Plane { .. } | GridFaceKind::Mesh { .. }
    );
    if !plane {
        let lit = lit_corner(point, face.normal, tint, ambient, lights, occluded);
        return shade.apply(lit, face.normal);
    }
    if face.unlit {
        return lit_corner(
            point,
            face.normal,
            tint,
            [NEUTRAL as u8; 3],
            &[],
            &|_, _| false,
        );
    }
    let front = lit_corner_nudged(
        point,
        face.normal,
        tint,
        ambient,
        lights,
        occluded,
        PLANE_SHADOW_NUDGE,
    );
    if !face.double_sided {
        return shade.apply(front, face.normal);
    }
    let back = lit_corner_nudged(
        point,
        face.normal.map(|v| -v),
        tint,
        ambient,
        lights,
        occluded,
        PLANE_SHADOW_NUDGE,
    );
    // A both-sided plane shows whichever side is brighter; shade it by the
    // side that is.
    let lit = core::array::from_fn(|i| front[i].max(back[i]));
    shade.apply(lit, face.normal)
}

/// The ambient a corner takes when the world gathers its light (see
/// [`super::lighting::GlobalLight`]): what the corner's hemisphere brings
/// in, on top of which the lamps add. In the bake's 128-neutral units and
/// not yet clamped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GatheredAmbient {
    /// For the visible side of the face.
    pub front: [f64; 3],
    /// For the hidden side, used by a double-sided plane or mesh face.
    pub back: [f64; 3],
}

/// Where the soft shoulder of the overbright range starts, in the bake's
/// units. A corner's light is linear up to here.
pub const SHOULDER_KNEE: f64 = 180.0;
/// The vertex colour the shoulder approaches, 255 being twice the texture.
pub const SHOULDER_TOP: f64 = 255.0;

/// A light value kept inside the vertex colour range with a soft shoulder
/// instead of a hard clip: linear up to [`SHOULDER_KNEE`], then bending
/// toward [`SHOULDER_TOP`] without ever reaching past it. A gathered bake
/// can sum to several times the neutral value near a lamp; clipping each
/// channel on its own turns a warm hot spot white, bending them together-ish
/// keeps the hue.
pub fn shoulder(light: f64) -> f64 {
    if light <= SHOULDER_KNEE {
        return light.max(0.0);
    }
    let room = SHOULDER_TOP - SHOULDER_KNEE;
    SHOULDER_KNEE + room * (1.0 - (-(light - SHOULDER_KNEE) / room).exp())
}

fn finish_gathered(tint: [u8; 3], light: [f64; 3]) -> [u8; 3] {
    core::array::from_fn(|i| modulated(tint[i], shoulder(light[i])))
}

/// [`lit_face_corner`] for a world that gathers its light: the ambient is
/// the given per-corner value instead of the room's flat colour, and the sum
/// goes through the shoulder rather than a hard clip.
pub fn lit_face_corner_gathered(
    face: &GridFace,
    point: [f64; 3],
    tint: [u8; 3],
    ambient: &GatheredAmbient,
    lights: &[GridLight],
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    shade: FacingShade,
) -> [u8; 3] {
    let plane = matches!(
        face.kind,
        GridFaceKind::Plane { .. } | GridFaceKind::Mesh { .. }
    );
    if plane && face.unlit {
        return lit_corner(
            point,
            face.normal,
            tint,
            [NEUTRAL as u8; 3],
            &[],
            &|_, _| false,
        );
    }
    let nudge = if plane {
        PLANE_SHADOW_NUDGE
    } else {
        SHADOW_NUDGE
    };
    let mut front = ambient.front;
    accumulate_direct(&mut front, point, face.normal, lights, occluded, nudge);
    let mut lit = finish_gathered(tint, front);
    if plane && face.double_sided {
        let mut back = ambient.back;
        accumulate_direct(
            &mut back,
            point,
            face.normal.map(|v| -v),
            lights,
            occluded,
            nudge,
        );
        let back = finish_gathered(tint, back);
        lit = core::array::from_fn(|i| lit[i].max(back[i]));
    }
    shade.apply(lit, face.normal)
}

/// Lit colour of every corner of `face` in editor units, without shadows,
/// scaled by the world's facing shade.
///
/// `tint` is the material's modulation colour (`[128; 3]` leaves the texture
/// as authored) and `ambient` the room's ambient light.
pub fn bake_face_corners(
    face: &GridFace,
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[GridLight],
    shade: FacingShade,
) -> Vec<[u8; 3]> {
    face.points
        .iter()
        .map(|p| {
            lit_face_corner(
                face,
                p.map(f64::from),
                tint,
                ambient,
                lights,
                &|_, _| false,
                shade,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::faces::all_faces;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;
    use crate::grid::world::GridWorld;

    fn floor_face() -> GridFace {
        let mut room = GridRoom::new("r", 1, 1);
        room.sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        };
        all_faces(&world).remove(0)
    }

    #[test]
    fn unlit_faces_show_ambient_modulated_by_tint() {
        let face = floor_face();
        let colors = bake_face_corners(&face, [128; 3], [64; 3], &[], FacingShade::OFF);
        assert_eq!(colors, vec![[64; 3]; 4]);
        let tinted = bake_face_corners(&face, [255, 128, 0], [128; 3], &[], FacingShade::OFF);
        assert_eq!(tinted[0], [255, 128, 0]);
    }

    #[test]
    fn a_lamp_above_lights_the_near_corners_more() {
        let face = floor_face();
        let lamp = GridLight {
            position: [0.0, 400.0, 0.0],
            radius: 2000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[lamp], FacingShade::OFF);
        let by_point = |x: i32, z: i32| {
            let i = face
                .points
                .iter()
                .position(|p| p[0] == x && p[2] == z)
                .unwrap();
            colors[i][0]
        };
        assert!(by_point(0, 0) > by_point(1024, 1024));
        assert!(by_point(1024, 1024) >= 32);
    }

    #[test]
    fn faces_turned_away_from_the_lamp_stay_at_ambient() {
        let face = floor_face();
        let below = GridLight {
            position: [512.0, -400.0, 512.0],
            radius: 4000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let colors = bake_face_corners(&face, [128; 3], [32; 3], &[below], FacingShade::OFF);
        assert!(colors.iter().all(|c| *c == [32; 3]));
    }

    #[test]
    fn facing_shade_scales_by_the_way_a_face_looks() {
        let shade = FacingShade::SUBTLE;
        let white = [200u8; 3];
        let k = |n: [f64; 3]| shade.apply(white, n)[0];
        assert_eq!(k([0.0, 1.0, 0.0]), 200, "floors keep their light");
        assert_eq!(
            k([0.0, -1.0, 0.0]),
            (200.0_f64 * 200.0 / 256.0).round() as u8
        );
        assert_eq!(
            k([1.0, 0.0, 0.0]),
            (200.0_f64 * 212.0 / 256.0).round() as u8
        );
        assert_eq!(k([-1.0, 0.0, 0.0]), k([1.0, 0.0, 0.0]));
        assert_eq!(
            k([0.0, 0.0, 1.0]),
            (200.0_f64 * 168.0 / 256.0).round() as u8
        );
        // Walls facing the two axes differ, and a diagonal sits between.
        let h = core::f64::consts::FRAC_1_SQRT_2;
        assert!(k([0.0, 0.0, 1.0]) < k([h, 0.0, h]) && k([h, 0.0, h]) < k([1.0, 0.0, 0.0]));
        // Off leaves the colour alone, bit for bit.
        assert_eq!(
            FacingShade::OFF.apply([1, 2, 3], [0.3, 0.5, 0.8]),
            [1, 2, 3]
        );
        assert!(FacingShade::default().is_off());
    }

    #[test]
    fn the_bake_applies_the_shade_to_walls_but_not_to_lit_floors() {
        let mut room = GridRoom::new("r", 1, 1);
        let sector = room.sector_or_insert(0, 0).unwrap();
        sector.set_ground_floor(Some(GridSurface::flat(0, None)));
        sector
            .north
            .push(crate::grid::surface::GridWall::span(0, 1024, None));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        };
        let faces = all_faces(&world);
        let bake = |face: &GridFace, shade| bake_face_corners(face, [128; 3], [128; 3], &[], shade);
        let (floor, wall) = (&faces[0], &faces[1]);
        assert_eq!(
            bake(floor, FacingShade::SUBTLE),
            bake(floor, FacingShade::OFF)
        );
        let off = bake(wall, FacingShade::OFF)[0][0];
        let on = bake(wall, FacingShade::SUBTLE)[0][0];
        assert!(on < off, "{on} against {off}");
    }

    #[test]
    fn a_blocked_shadow_ray_leaves_the_corner_at_ambient() {
        let face = floor_face();
        let lamp = GridLight {
            position: [0.0, 400.0, 0.0],
            radius: 2000.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let lit = bake_face_corners(&face, [128; 3], [32; 3], &[lamp], FacingShade::OFF);
        let shadowed: Vec<[u8; 3]> = face
            .points
            .iter()
            .map(|p| {
                lit_corner(
                    p.map(f64::from),
                    face.normal,
                    [128; 3],
                    [32; 3],
                    &[lamp],
                    &|_, _| true,
                )
            })
            .collect();
        assert!(lit.iter().any(|c| c[0] > 32));
        assert!(shadowed.iter().all(|c| *c == [32; 3]));
    }
}
