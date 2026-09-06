//! Stance polygons converge on the current animated pose, over its wireframe.

use super::*;
use psx_engine::{ViewVertex, WorldProjection, WorldRenderStats};

const FLIGHT_TICKS: u16 = 18;
const HOLD_TICKS: u16 = 6;
const RESTORE_TICKS: u16 = 6;

/// Timing and colour for rebuilding the player from feet to head.
#[derive(Copy, Clone)]
pub struct ModelPhaseAssembly {
    elapsed: u16,
    duration: u16,
    color: (u8, u8, u8),
    floor_y: i32,
    height: i32,
}

impl ModelPhaseAssembly {
    /// Returns no effect after the completed colour has held and faded back.
    pub fn new(
        elapsed: u16,
        duration: u16,
        color: (u8, u8, u8),
        floor_y: i32,
        height: i32,
    ) -> Option<Self> {
        if duration < FLIGHT_TICKS || elapsed >= duration.saturating_add(HOLD_TICKS + RESTORE_TICKS)
        {
            return None;
        }
        Some(Self {
            elapsed,
            duration,
            color,
            floor_y,
            height: height.max(1),
        })
    }

    fn arrival(self, height_q12: u16) -> u16 {
        FLIGHT_TICKS
            + ((u32::from(height_q12.min(4096)) * u32::from(self.duration - FLIGHT_TICKS)) >> 12)
                as u16
    }

    fn flight(self, height_q12: u16) -> (u16, i32) {
        let arrival = self.arrival(height_q12);
        let age = self
            .elapsed
            .saturating_sub(arrival - FLIGHT_TICKS)
            .min(FLIGHT_TICKS);
        let t = i32::from(age) * 256 / i32::from(FLIGHT_TICKS);
        // Slow down as each piece finds its place, without an overshoot.
        let remaining = 256 - t;
        (age, remaining * remaining >> 8)
    }

    fn tint(self, base: (u8, u8, u8)) -> (u8, u8, u8) {
        let restore = self
            .elapsed
            .saturating_sub(self.duration.saturating_add(HOLD_TICKS));
        let strength = 256 - i32::from(restore.min(RESTORE_TICKS)) * 256 / i32::from(RESTORE_TICKS);
        (
            lerp_tint_channel(base.0, self.color.0, strength),
            lerp_tint_channel(base.1, self.color.1, strength),
            lerp_tint_channel(base.2, self.color.2, strength),
        )
    }

    pub(super) fn wire_color(self) -> (u8, u8, u8) {
        (self.color.0 / 3, self.color.1 / 3, self.color.2 / 3)
    }
}

/// Translate an already projected mesh corner through camera space. Keeping
/// the original projection as the numerator makes arrival pixel-exact; no
/// second skeleton sample or persistent particle positions are needed.
fn displace(
    p: ProjectedVertex,
    offset: ViewVertex,
    projection: WorldProjection,
) -> Option<ProjectedVertex> {
    let z = p.sz.saturating_add(offset.z);
    if p == ProjectedVertex::INVALID || z < projection.near_z || z <= 0 {
        return None;
    }
    let sx = i32::from(projection.screen_x)
        + ((i32::from(p.sx) - i32::from(projection.screen_x)) * p.sz
            + offset.x * projection.focal_length)
            / z;
    let sy = i32::from(projection.screen_y)
        + ((i32::from(p.sy) - i32::from(projection.screen_y)) * p.sz
            - offset.y * projection.focal_length)
            / z;
    if !(-1023..=1023).contains(&sx) || !(-1023..=1023).contains(&sy) {
        return None;
    }
    Some(ProjectedVertex::new(sx as i16, sy as i16, z))
}

fn height_q12(
    assembly: ModelPhaseAssembly,
    points: [ProjectedVertex; 3],
    camera: WorldCamera,
) -> u16 {
    let z = points.iter().map(|p| p.sz).sum::<i32>() / 3;
    let sy = points.iter().map(|p| i32::from(p.sy)).sum::<i32>() / 3;
    let view_y =
        (i32::from(camera.projection.screen_y) - sy) * z / camera.projection.focal_length.max(1);
    let y =
        camera.position.y + ((view_y * camera.cos_pitch.raw() + z * camera.sin_pitch.raw()) >> 12);
    ((y - assembly.floor_y).clamp(0, assembly.height) * 4096 / assembly.height) as u16
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw<const OT_DEPTH: usize>(
    assembly: ModelPhaseAssembly,
    projected: &[ProjectedVertex],
    faces: &[TexturedModelRenderFace],
    camera: WorldCamera,
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    triangles: &mut (impl PrimitiveSink<TriTextured>
              + PrimitiveSink<QuadGouraudBlended>
              + PrimitiveSink<LineMono>),
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> WorldRenderStats {
    let mut stats = WorldRenderStats::default();
    let zero = camera.view_vertex(WorldVertex::ZERO);
    let options = options.with_textured_triangle_splitting(false);
    let wire_options = options.with_depth_bias(options.depth_bias.saturating_add(2));
    let wire_color = assembly.wire_color();
    let tint = assembly.tint(material.tint());
    let attached_material = material
        .with_raw_texture(false)
        .with_tint(tint)
        .with_blend_mode(BlendMode::Opaque);
    let mut attached = [TexturedModelRenderFace::ZERO; 32];
    let mut attached_count = 0;
    for (index, face) in faces.iter().enumerate() {
        let indices = face.vertex_indices().map(usize::from);
        if indices.iter().any(|&i| i >= projected.len()) {
            continue;
        }
        let target = indices.map(|i| projected[i]);
        if target.contains(&ProjectedVertex::INVALID) {
            continue;
        }
        let [a, b, c] = target;
        let area = (i32::from(b.sx) - i32::from(a.sx)) * (i32::from(c.sy) - i32::from(a.sy))
            - (i32::from(b.sy) - i32::from(a.sy)) * (i32::from(c.sx) - i32::from(a.sx));
        if area == 0
            || (options.cull_mode == CullMode::Back && area < 0)
            || (options.cull_mode == CullMode::Front && area > 0)
        {
            continue;
        }
        let (age, remaining) = if assembly.elapsed >= assembly.duration {
            (FLIGHT_TICKS, 0)
        } else {
            assembly.flight(height_q12(assembly, target, camera))
        };
        if age < FLIGHT_TICKS && index & 1 == 0 {
            // Alternate long edges keep the cage legible. Filled
            // faces no longer need a wire pass underneath them.
            let edge = [(a, b), (b, c), (c, a)]
                .into_iter()
                .max_by_key(|(a, b)| {
                    (i32::from(a.sx) - i32::from(b.sx)).abs()
                        + (i32::from(a.sy) - i32::from(b.sy)).abs()
                })
                .unwrap();
            let _ =
                world.submit_projected_line(triangles, [edge.0, edge.1], wire_color, wire_options);
        }
        // The intact body's wireframe remains visible before this piece starts.
        // A few larger travelling facets announce each band of the mesh;
        // its remaining small faces fill in when the same band lands.
        if age == 0 || (age < FLIGHT_TICKS && index & 3 != 0) {
            continue;
        }
        if age == FLIGHT_TICKS {
            attached[attached_count] = *face;
            attached_count += 1;
            if attached_count == attached.len() {
                merge_attached(
                    &mut stats,
                    world.submit_projected_model_faces(
                        triangles,
                        projected,
                        &attached,
                        attached_material,
                        options,
                    ),
                );
                attached_count = 0;
                if stats.primitive_overflow || stats.command_overflow {
                    break;
                }
            }
            continue;
        }
        let next = {
            let angle = Angle::from_q12((index as u16).wrapping_mul(1567));
            let radius = assembly.height * (65 + (index % 4) as i32 * 12) / 100;
            let offset = camera.view_vertex(WorldVertex::new(
                (angle.sin_q12() * radius >> 12) * remaining >> 8,
                (((index % 7) as i32 - 3) * assembly.height / 16) * remaining >> 8,
                (angle.cos_q12() * radius >> 12) * remaining >> 8,
            ));
            let offset = ViewVertex::new(offset.x - zero.x, offset.y - zero.y, offset.z - zero.z);
            let [Some(a), Some(b), Some(c)] =
                target.map(|p| displace(p, offset, camera.projection))
            else {
                continue;
            };
            let strength = i32::from(age) * 256 / i32::from(FLIGHT_TICKS);
            let tint = (
                (i32::from(tint.0) * strength >> 8) as u8,
                (i32::from(tint.1) * strength >> 8) as u8,
                (i32::from(tint.2) * strength >> 8) as u8,
            );
            let center_x = (i32::from(a.sx) + i32::from(b.sx) + i32::from(c.sx)) / 3;
            let center_y = (i32::from(a.sy) + i32::from(b.sy) + i32::from(c.sy)) / 3;
            let scale = 256 + remaining * 2;
            let points = [a, b, c].map(|p| ProjectedLit {
                sx: clamp_i16(center_x + ((i32::from(p.sx) - center_x) * scale >> 8)),
                sy: clamp_i16(center_y + ((i32::from(p.sy) - center_y) * scale >> 8)),
                sz: p.sz.clamp(0, 65535) as u16,
                r: tint.0,
                g: tint.1,
                b: tint.2,
            });
            // A close wall camera can magnify an approaching facet beyond the
            // GPU's polygon limits. Drop that facet rather than wrap its edges.
            if points
                .iter()
                .any(|p| !(-1023..=1023).contains(&p.sx) || !(-1023..=1023).contains(&p.sy))
                || points.iter().map(|p| p.sx).max().unwrap()
                    - points.iter().map(|p| p.sx).min().unwrap()
                    > 1023
                || points.iter().map(|p| p.sy).max().unwrap()
                    - points.iter().map(|p| p.sy).min().unwrap()
                    > 511
            {
                continue;
            }
            // A collapsed fourth corner uses the existing blended packet path
            // for one untextured facet. Its transparency is independent of the
            // model atlas's per-texel semi-transparency bits.
            world.submit_blended_gouraud_quad(
                triangles,
                [points[0], points[1], points[2], points[2]],
                BlendMode::Add,
                options,
            )
        };
        stats.submitted_triangles = stats
            .submitted_triangles
            .saturating_add(next.submitted_triangles);
        stats.culled_triangles = stats.culled_triangles.saturating_add(next.culled_triangles);
        stats.dropped_triangles = stats
            .dropped_triangles
            .saturating_add(next.dropped_triangles);
        stats.primitive_overflow |= next.primitive_overflow;
        stats.command_overflow |= next.command_overflow;
        if stats.primitive_overflow || stats.command_overflow {
            break;
        }
    }
    if attached_count > 0 && !stats.primitive_overflow && !stats.command_overflow {
        merge_attached(
            &mut stats,
            world.submit_projected_model_faces(
                triangles,
                projected,
                &attached[..attached_count],
                attached_material,
                options,
            ),
        );
    }
    stats
}

fn merge_attached(stats: &mut WorldRenderStats, next: TexturedModelRenderStats) {
    stats.submitted_triangles = stats
        .submitted_triangles
        .saturating_add(next.submitted_triangles);
    stats.culled_triangles = stats.culled_triangles.saturating_add(next.culled_triangles);
    stats.dropped_triangles = stats
        .dropped_triangles
        .saturating_add(next.dropped_triangles);
    stats.primitive_overflow |= next.primitive_overflow;
    stats.command_overflow |= next.command_overflow;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feet_land_before_head_and_all_pieces_finish_on_time() {
        let effect = ModelPhaseAssembly::new(0, 60, (255, 100, 40), 0, 90).unwrap();
        assert_eq!(effect.arrival(0), 18);
        assert_eq!(effect.arrival(4096), 60);
        for height in (0..=4096).step_by(32) {
            let completed = ModelPhaseAssembly {
                elapsed: 60,
                ..effect
            };
            assert_eq!(completed.flight(height), (FLIGHT_TICKS, 0));
        }
    }

    #[test]
    fn attached_colour_holds_until_every_piece_has_completed() {
        let effect = ModelPhaseAssembly::new(0, 60, (255, 100, 40), 0, 90).unwrap();
        for elapsed in 18..=66 {
            assert_eq!(
                ModelPhaseAssembly { elapsed, ..effect }.tint((80, 90, 100)),
                effect.color
            );
        }
        assert!(ModelPhaseAssembly::new(72, 60, effect.color, 0, 90).is_none());
    }

    #[test]
    fn arrival_matches_the_original_mesh_and_near_plane_is_respected() {
        let projection = WorldProjection::new(160, 120, 256, 8);
        for p in [
            ProjectedVertex::new(10, 20, 64),
            ProjectedVertex::new(200, 180, 371),
        ] {
            assert_eq!(displace(p, ViewVertex::ZERO, projection), Some(p));
            assert_eq!(displace(p, ViewVertex::new(0, 0, -p.sz), projection), None);
        }
    }
}
