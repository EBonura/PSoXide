//! Stance polygons converge on the current animated pose, over its wireframe.

use super::*;
use psx_engine::{ViewVertex, WorldProjection, WorldRenderStats};

const BURST_TICKS: u16 = 12;
const FLIGHT_TICKS: u16 = 18;
const HOLD_TICKS: u16 = 6;
const RESTORE_TICKS: u16 = 30;

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
        if duration < BURST_TICKS + FLIGHT_TICKS
            || elapsed >= duration.saturating_add(HOLD_TICKS + RESTORE_TICKS)
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
        BURST_TICKS
            + FLIGHT_TICKS
            + ((u32::from(height_q12.min(4096))
                * u32::from(self.duration - BURST_TICKS - FLIGHT_TICKS))
                >> 12) as u16
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

    /// Once assembled, draw the ordinary shaded model and fade its packets.
    pub const fn is_assembled(self) -> bool {
        self.elapsed >= self.duration
    }

    fn finish_strength_q8(self) -> i32 {
        let restore = self
            .elapsed
            .saturating_sub(self.duration.saturating_add(HOLD_TICKS));
        let t = i32::from(restore.min(RESTORE_TICKS)) * 256 / i32::from(RESTORE_TICKS);
        256 - (((t * t) >> 8) * (768 - 2 * t) >> 8)
    }

    fn tint(self, base: (u8, u8, u8)) -> (u8, u8, u8) {
        let strength = self.finish_strength_q8();
        (
            lerp_tint_channel(base.0, self.color.0, strength),
            lerp_tint_channel(base.1, self.color.1, strength),
            lerp_tint_channel(base.2, self.color.2, strength),
        )
    }

    /// Fade into each face's original lighting and material response, rather
    /// than a uniform approximation that changes again when the effect ends.
    ///
    /// # Safety
    /// Every slot from `first_slot` to the arena cursor must be a `TriTextured`
    /// emitted by the completed, solid player body. Call before equipment or
    /// any other packet type is submitted; dash wireframes must be excluded.
    pub unsafe fn apply_finish_to_model_packets(
        self,
        arena: &mut PrimitivePacketArena<'_>,
        first_slot: usize,
    ) -> bool {
        debug_assert!(self.is_assembled());
        let end_slot = arena.used_slots();
        let strength = self.finish_strength_q8();
        // SAFETY: the caller supplies the immediately preceding body range.
        unsafe {
            arena.mutate_typed_slots::<TriTextured>(first_slot, end_slot, |triangle| {
                let base = (
                    triangle.color_cmd as u8,
                    (triangle.color_cmd >> 8) as u8,
                    (triangle.color_cmd >> 16) as u8,
                );
                let tint = (
                    lerp_tint_channel(base.0, self.color.0, strength),
                    lerp_tint_channel(base.1, self.color.1, strength),
                    lerp_tint_channel(base.2, self.color.2, strength),
                );
                triangle.color_cmd = (triangle.color_cmd & 0xff00_0000)
                    | u32::from(tint.0)
                    | (u32::from(tint.1) << 8)
                    | (u32::from(tint.2) << 16);
            })
        }
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

fn burst_triangle(
    target: [ProjectedVertex; 3],
    offset: ViewVertex,
    progress: i32,
    projection: WorldProjection,
) -> Option<[ProjectedVertex; 3]> {
    let center = ProjectedVertex::new(
        ((i32::from(target[0].sx) + i32::from(target[1].sx) + i32::from(target[2].sx)) / 3) as i16,
        ((i32::from(target[0].sy) + i32::from(target[1].sy) + i32::from(target[2].sy)) / 3) as i16,
        (target[0].sz + target[1].sz + target[2].sz) / 3,
    );
    if target.iter().any(|p| p.sz + offset.z < projection.near_z) {
        return None;
    }
    let displaced = displace(center, offset, projection)?;
    // Treat each small fragment as a rigid plane: project its centre once,
    // then scale its corners for depth and dissolution.
    let scale = (256 - (progress * progress >> 8)) * center.sz / displaced.sz;
    let points = target.map(|p| {
        ProjectedVertex::new(
            clamp_i16(
                i32::from(displaced.sx) + ((i32::from(p.sx) - i32::from(center.sx)) * scale >> 8),
            ),
            clamp_i16(
                i32::from(displaced.sy) + ((i32::from(p.sy) - i32::from(center.sy)) * scale >> 8),
            ),
            p.sz + offset.z,
        )
    });
    if points
        .iter()
        .any(|p| !(-1023..=1023).contains(&p.sx) || !(-1023..=1023).contains(&p.sy))
        || points.iter().map(|p| p.sx).max()? - points.iter().map(|p| p.sx).min()? > 1023
        || points.iter().map(|p| p.sy).max()? - points.iter().map(|p| p.sy).min()? > 511
    {
        return None;
    }
    Some(points)
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
    let bursting = assembly.elapsed < BURST_TICKS;
    let burst_progress =
        i32::from(assembly.elapsed.min(BURST_TICKS)) * 256 / i32::from(BURST_TICKS);
    let burst_fade = 256 - burst_progress;
    let burst_material = material.with_raw_texture(false).with_tint((
        (i32::from(material.tint().0) * burst_fade >> 8) as u8,
        (i32::from(material.tint().1) * burst_fade >> 8) as u8,
        (i32::from(material.tint().2) * burst_fade >> 8) as u8,
    ));
    let attached_material = if bursting {
        burst_material
    } else {
        material
            .with_raw_texture(false)
            .with_tint(tint)
            .with_blend_mode(BlendMode::Opaque)
    };
    let mut burst_vertices = [ProjectedVertex::INVALID; 96];
    let mut burst_offsets = [ViewVertex::ZERO; 8];
    if bursting {
        let radius = assembly.height * burst_progress / 384;
        for (index, offset) in burst_offsets.iter_mut().enumerate() {
            let angle = Angle::from_q12(index as u16 * 512);
            let vector = camera.view_vertex(WorldVertex::new(
                angle.sin_q12() * radius >> 12,
                (index as i32 % 3 - 1) * radius / 2,
                angle.cos_q12() * radius >> 12,
            ));
            *offset = ViewVertex::new(vector.x - zero.x, vector.y - zero.y, vector.z - zero.z);
        }
    }
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
        let (age, remaining) = if bursting {
            (0, 256)
        } else if assembly.elapsed >= assembly.duration {
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
        if !bursting && (age == 0 || (age < FLIGHT_TICKS && index & 3 != 0)) {
            continue;
        }
        if bursting || age == FLIGHT_TICKS {
            attached[attached_count] = if bursting {
                let Some(points) = burst_triangle(
                    target,
                    burst_offsets[index & 7],
                    burst_progress,
                    camera.projection,
                ) else {
                    continue;
                };
                let base = attached_count * 3;
                burst_vertices[base..base + 3].copy_from_slice(&points);
                TexturedModelRenderFace::new_with_palette_bank(
                    [base as u16, base as u16 + 1, base as u16 + 2],
                    face.uvs(),
                    face.palette_bank(),
                )
            } else {
                *face
            };
            attached_count += 1;
            if attached_count == attached.len() {
                merge_attached(
                    &mut stats,
                    world.submit_projected_model_faces(
                        triangles,
                        if bursting { &burst_vertices } else { projected },
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
                if bursting { &burst_vertices } else { projected },
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
        let effect = ModelPhaseAssembly::new(0, 72, (255, 100, 40), 0, 90).unwrap();
        assert_eq!(effect.arrival(0), 30);
        assert_eq!(effect.arrival(4096), 72);
        for height in (0..=4096).step_by(32) {
            let completed = ModelPhaseAssembly {
                elapsed: 72,
                ..effect
            };
            assert_eq!(completed.flight(height), (FLIGHT_TICKS, 0));
        }
    }

    #[test]
    fn the_old_mesh_bursts_before_any_new_faces_start_arriving() {
        let effect = ModelPhaseAssembly::new(0, 72, (255, 100, 40), 0, 90).unwrap();
        for elapsed in 0..=BURST_TICKS {
            for height in [0, 2048, 4096] {
                assert_eq!(ModelPhaseAssembly { elapsed, ..effect }.flight(height).0, 0);
            }
        }
        let target = [
            ProjectedVertex::new(140, 100, 256),
            ProjectedVertex::new(160, 100, 256),
            ProjectedVertex::new(150, 120, 256),
        ];
        let projection = WorldProjection::new(160, 120, 256, 8);
        assert_eq!(
            burst_triangle(target, ViewVertex::ZERO, 0, projection),
            Some(target)
        );
        let moved = burst_triangle(target, ViewVertex::new(40, 0, 0), 128, projection).unwrap();
        assert!(moved.iter().zip(target).all(|(a, b)| a.sx > b.sx));
    }

    #[test]
    fn attached_colour_holds_until_every_piece_has_completed() {
        let effect = ModelPhaseAssembly::new(0, 72, (255, 100, 40), 0, 90).unwrap();
        for elapsed in 30..=78 {
            assert_eq!(
                ModelPhaseAssembly { elapsed, ..effect }.tint((80, 90, 100)),
                effect.color
            );
        }
        assert!(ModelPhaseAssembly::new(108, 72, effect.color, 0, 90).is_none());
    }

    #[test]
    fn finish_eases_into_each_faces_own_shading_without_a_final_colour_step() {
        let effect = ModelPhaseAssembly::new(78, 72, (255, 100, 40), 0, 90).unwrap();
        assert_eq!(effect.finish_strength_q8(), 256);
        assert_eq!(
            ModelPhaseAssembly {
                elapsed: 93,
                ..effect
            }
            .finish_strength_q8(),
            128
        );
        let mut previous = 256;
        for elapsed in 78..=108 {
            let strength = ModelPhaseAssembly { elapsed, ..effect }.finish_strength_q8();
            assert!(strength <= previous);
            previous = strength;
        }
        for base in [(40, 75, 110), (110, 140, 170)] {
            let last = ModelPhaseAssembly {
                elapsed: 107,
                ..effect
            }
            .tint(base);
            for (a, b) in [(last.0, base.0), (last.1, base.1), (last.2, base.2)] {
                assert!(a.abs_diff(b) <= 1);
            }
            assert_eq!(
                ModelPhaseAssembly {
                    elapsed: 108,
                    ..effect
                }
                .tint(base),
                base
            );
        }
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
