//! The one light bake the cook and the editor preview share.
//!
//! A Play build bakes light into the corner colours of every quad. The editor
//! preview has to show the same corners, so both go through this module:
//!
//! * [`BakeRig`] colours one corner of a face from the point lights, the
//!   room ambient (or, in a world that gathers its light, what the corner's
//!   hemisphere brings in), the world's facing shade and a shadow query, all
//!   in runtime units and at the corner position the cook snaps to;
//! * [`scene_grid_lights`] turns the scene's point lights into the lights the
//!   rig takes, the same way for the cook (a project already divided into
//!   runtime units) and the preview (the authored project);
//! * [`probe::CollisionProbe`] answers the shadow query with the runtime's own
//!   line of sight, so a shadow falls where the game would stop a ray;
//! * [`caster::FaceCaster`] answers "what does this ray hit" over the drawn
//!   faces, and [`gather`] uses it to solve ambient occlusion, sky light and
//!   bounced light ([`GlobalLight`]).

pub mod albedo;
pub mod caster;
pub mod gather;
#[cfg(test)]
mod gather_tests;
pub mod probe;
pub mod settings;

pub use albedo::AlbedoTable;
pub use caster::{FaceCaster, RayHit};
pub use gather::{BakeMaterials, GiField};
pub use probe::CollisionProbe;
pub use settings::GlobalLight;

use super::cook::units::RuntimeUnits;
use super::faces::GridFace;
use super::light::{
    lit_face_corner, lit_face_corner_gathered, FacingShade, GatheredAmbient, GridLight,
};
use crate::{NodeKind, Scene};
use gather::SampleKey;

/// Light reach for a scene with no World node, runtime units.
const DEFAULT_LIGHT_RADIUS_UNITS: f64 = 1024.0;

/// Point lights of `scene` in runtime units.
///
/// `divisor` is the authored-to-runtime divisor still to apply to node
/// positions and to the World node's sector size: `1.0` for the cook, whose
/// project is already scaled, [`crate::units::WORLD_UNIT_DIVISOR`] for the
/// authored project the editor holds. A light's radius is authored in
/// sectors and resolves against that sector size.
pub fn scene_grid_lights(scene: &Scene, divisor: f64) -> Vec<GridLight> {
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .map(|size| f64::from(size) / divisor)
        .unwrap_or(DEFAULT_LIGHT_RADIUS_UNITS);
    scene
        .nodes()
        .iter()
        .filter_map(|node| {
            let NodeKind::PointLight {
                color,
                intensity,
                radius,
            } = &node.kind
            else {
                return None;
            };
            Some(GridLight {
                position: node.transform.translation.map(|v| f64::from(v) / divisor),
                radius: f64::from(*radius) * radius_units,
                intensity_q8: (f64::from(*intensity) * 256.0)
                    .round()
                    .clamp(0.0, f64::from(u16::MAX)) as u16,
                color: *color,
            })
        })
        .filter(GridLight::is_valid)
        .collect()
}

/// A world that gathers its light, as the rig sees it.
#[derive(Clone, Copy)]
pub struct Gather<'a> {
    /// What the gather solved; `None` while it has not finished (the preview
    /// refines in the background), when corners take the flat room ambient
    /// but already go through the same shoulder.
    pub field: Option<&'a GiField>,
}

/// Colours the corners of faces the way a Play build does.
pub struct BakeRig<'a> {
    /// Grid to runtime units.
    pub units: RuntimeUnits,
    /// Point lights, runtime units.
    pub lights: &'a [GridLight],
    /// The world's facing shade.
    pub shade: FacingShade,
    /// Whether the segment between two points (runtime units) is blocked by
    /// solid world.
    pub occluded: &'a dyn Fn([f64; 3], [f64; 3]) -> bool,
    /// Set when the world gathers its light.
    pub gather: Option<Gather<'a>>,
}

/// Weights of the ring corners at `point` of a face, runtime units.
///
/// A four-corner ring that is a parallelogram (a floor tile, a wall panel) is
/// interpolated bilinearly, so the value at the middle is the mean of the four
/// corners whichever diagonal the GPU later splits it along. Any other ring
/// is interpolated over the triangle fan from corner 0 that holds the point.
fn ring_weights(ring: &[[f64; 3]], point: [f64; 3]) -> Option<Vec<(usize, f64)>> {
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if ring.len() == 4 {
        let sum =
            |a: usize, b: usize| -> [f64; 3] { core::array::from_fn(|k| ring[a][k] + ring[b][k]) };
        let (s02, s13) = (sum(0, 2), sum(1, 3));
        if (0..3).all(|k| (s02[k] - s13[k]).abs() <= 1.0) {
            let (e1, e3, q) = (
                sub(ring[1], ring[0]),
                sub(ring[3], ring[0]),
                sub(point, ring[0]),
            );
            let (a, b, c) = (dot(e1, e1), dot(e1, e3), dot(e3, e3));
            let det = a * c - b * b;
            if det.abs() > 1e-12 {
                let (r1, r3) = (dot(q, e1), dot(q, e3));
                let u = ((r1 * c - r3 * b) / det).clamp(0.0, 1.0);
                let v = ((r3 * a - r1 * b) / det).clamp(0.0, 1.0);
                return Some(vec![
                    (0, (1.0 - u) * (1.0 - v)),
                    (1, u * (1.0 - v)),
                    (2, u * v),
                    (3, (1.0 - u) * v),
                ]);
            }
        }
    }
    let mut best: Option<(f64, Vec<(usize, f64)>)> = None;
    for k in 1..ring.len().saturating_sub(1) {
        let (a, b, c) = (ring[0], ring[k], ring[k + 1]);
        let (v0, v1, v2) = (sub(b, a), sub(c, a), sub(point, a));
        let (d00, d01, d11) = (dot(v0, v0), dot(v0, v1), dot(v1, v1));
        let (d20, d21) = (dot(v2, v0), dot(v2, v1));
        let denom = d00 * d11 - d01 * d01;
        if denom.abs() < 1e-12 {
            continue;
        }
        let v = (d11 * d20 - d01 * d21) / denom;
        let w = (d00 * d21 - d01 * d20) / denom;
        let weights = [1.0 - v - w, v, w];
        let inside = weights.iter().copied().fold(f64::MAX, f64::min);
        if best.as_ref().is_none_or(|(b, _)| inside > *b) {
            best = Some((
                inside,
                vec![(0, weights[0]), (k, weights[1]), (k + 1, weights[2])],
            ));
        }
    }
    // A point just outside (rounding) is pulled onto the triangle.
    let (_, mut picked) = best?;
    for (_, w) in &mut picked {
        *w = w.max(0.0);
    }
    let total: f64 = picked.iter().map(|(_, w)| w).sum();
    (total > 0.0).then(|| {
        picked.iter_mut().for_each(|(_, w)| *w /= total);
        picked
    })
}

impl BakeRig<'_> {
    /// Position of corner `corner` of `face` in runtime units, snapped the way
    /// the cook snaps the quad vertex.
    pub fn corner_point(&self, face: &GridFace, corner: usize) -> [f64; 3] {
        face.grid[corner].map(|g| f64::from(self.units.rt(g)))
    }

    /// The gathered ambient a point of `face` takes. `corner` names the face
    /// corner the point is, when it is one; `parent` is the whole face a cut
    /// piece came from, whose corner values the point is interpolated from.
    fn gathered_ambient(
        &self,
        field: Option<&GiField>,
        face: &GridFace,
        corner: Option<usize>,
        parent: Option<&GridFace>,
        point: [f64; 3],
        flat: [f64; 3],
    ) -> GatheredAmbient {
        let Some(field) = field else {
            return GatheredAmbient {
                front: flat,
                back: flat,
            };
        };
        let side = |back: bool| -> [f64; 3] {
            corner
                .and_then(|c| field.get(&SampleKey::of_corner(face, c, back)))
                .or_else(|| {
                    let parent = parent?;
                    let ring: Vec<[f64; 3]> = (0..parent.grid.len())
                        .map(|c| self.corner_point(parent, c))
                        .collect();
                    let weights = ring_weights(&ring, point)?;
                    let mut out = [0.0; 3];
                    for (id, w) in weights {
                        let v = field.get(&SampleKey::of_corner(parent, id, back))?;
                        for c in 0..3 {
                            out[c] += w * v[c];
                        }
                    }
                    Some(out)
                })
                .unwrap_or(flat)
        };
        GatheredAmbient {
            front: side(false),
            back: side(true),
        }
    }

    /// Lit colour of a point of `face`: corner `corner` of it, or (`None`) a
    /// point `point` inside the face, as for the centre sample that picks a
    /// quad's diagonal. `parent` is the uncut face when `face` is a piece.
    pub fn colour_at(
        &self,
        face: &GridFace,
        corner: Option<usize>,
        parent: Option<&GridFace>,
        point: [f64; 3],
        tint: [u8; 3],
        ambient: [u8; 3],
    ) -> [u8; 3] {
        match self.gather {
            None => lit_face_corner(
                face,
                point,
                tint,
                ambient,
                self.lights,
                self.occluded,
                self.shade,
            ),
            Some(gather) => {
                let amb = self.gathered_ambient(
                    gather.field,
                    face,
                    corner,
                    parent,
                    point,
                    ambient.map(f64::from),
                );
                lit_face_corner_gathered(
                    face,
                    point,
                    tint,
                    &amb,
                    self.lights,
                    self.occluded,
                    self.shade,
                )
            }
        }
    }

    /// Lit colour of corner `corner` of `face`, noise tint included.
    /// `tint` is the material's modulation colour and `ambient` the room's;
    /// `parent` is the whole face when `face` is a piece cut from it.
    pub fn corner(
        &self,
        face: &GridFace,
        corner: usize,
        parent: Option<&GridFace>,
        tint: [u8; 3],
        ambient: [u8; 3],
    ) -> [u8; 3] {
        self.colour_at(
            face,
            Some(corner),
            parent,
            self.corner_point(face, corner),
            tint,
            ambient,
        )
    }

    /// [`Self::corner`] for every corner of `face`, in order.
    pub fn corners(
        &self,
        face: &GridFace,
        parent: Option<&GridFace>,
        tint: [u8; 3],
        ambient: [u8; 3],
    ) -> Vec<[u8; 3]> {
        (0..face.grid.len())
            .map(|corner| self.corner(face, corner, parent, tint, ambient))
            .collect()
    }
}

/// What the light bake of one world is shot against: the grid-to-runtime
/// mapping and the world's collision. Built from the document alone, so the
/// editor preview can bake with the shadows a Play build has.
pub struct LightScene {
    units: RuntimeUnits,
    probe: CollisionProbe,
}

impl LightScene {
    /// The scene of `world`, or why its collision cannot be built.
    pub fn new(world: &super::world::GridWorld) -> Result<Self, String> {
        let units = RuntimeUnits::new(world.scale)?;
        let origins = probe::room_origins(world, &units);
        let cols = probe::collision_tables(world, &units)?;
        let probe = CollisionProbe::new(world, &units, &origins, &cols)?;
        Ok(Self { units, probe })
    }

    /// The grid to runtime mapping of the world.
    pub fn units(&self) -> RuntimeUnits {
        self.units
    }

    /// The collision the shadows are shot against.
    pub fn probe(&self) -> &CollisionProbe {
        &self.probe
    }

    /// Run `f` with a rig that bakes `lights` (runtime units) and the
    /// world's `shade` through this scene's shadows. `gather` is the world's
    /// global light, when it is on. The collision world is parsed once per
    /// call, so bake as many corners as you can in it.
    pub fn with_rig<R>(
        &self,
        lights: &[GridLight],
        shade: FacingShade,
        gather: Option<Gather<'_>>,
        f: impl FnOnce(&BakeRig<'_>) -> R,
    ) -> Result<R, String> {
        self.probe.with_occlusion(|occluded| {
            f(&BakeRig {
                units: self.units,
                lights,
                shade,
                occluded,
                gather,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cook::tests::load_reference;

    #[test]
    fn a_parallelogram_interpolates_bilinearly_and_its_middle_is_the_mean() {
        let ring = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [10.0, 0.0, 10.0],
            [0.0, 0.0, 10.0],
        ];
        let values = [1.0, 2.0, 4.0, 8.0];
        let at = |p: [f64; 3]| -> f64 {
            ring_weights(&ring, p)
                .unwrap()
                .iter()
                .map(|(i, w)| values[*i] * w)
                .sum()
        };
        assert!((at([5.0, 0.0, 5.0]) - 3.75).abs() < 1e-9);
        assert!((at([0.0, 0.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!((at([10.0, 0.0, 10.0]) - 4.0).abs() < 1e-9);
        assert!((at([5.0, 0.0, 0.0]) - 1.5).abs() < 1e-9);
        // A triangle falls back to the fan.
        let tri = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 0.0, 10.0]];
        let w = ring_weights(&tri, [2.5, 0.0, 2.5]).unwrap();
        assert!((w.iter().map(|(_, w)| w).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_preview_reads_the_scene_lights_the_cook_reads() {
        // The cook reads lights from the project divided into runtime units;
        // the editor preview from the authored one. They must agree, or the
        // preview lights the level differently from the disc.
        for name in ["grid-starter", "grid-cathedral", "grid-dungeon"] {
            let authored = load_reference(name).unwrap();
            let mut scaled = authored.clone();
            crate::units::scale_project_to_engine_units(&mut scaled);
            let cook = scene_grid_lights(scaled.active_scene(), 1.0);
            let preview = scene_grid_lights(
                authored.active_scene(),
                f64::from(crate::units::WORLD_UNIT_DIVISOR),
            );
            assert!(!cook.is_empty(), "{name} has lamps");
            assert_eq!(cook.len(), preview.len(), "{name}");
            for (c, p) in cook.iter().zip(&preview) {
                // Scaling rounds the lamp's position to f32 / 16, which is
                // exact; radius and colour come out equal too.
                assert_eq!(c.intensity_q8, p.intensity_q8, "{name}");
                assert_eq!(c.color, p.color, "{name}");
                for k in 0..3 {
                    assert!((c.position[k] - p.position[k]).abs() < 1e-4, "{name}");
                }
                assert!((c.radius - p.radius).abs() < 1e-4, "{name}");
            }
        }
    }
}
