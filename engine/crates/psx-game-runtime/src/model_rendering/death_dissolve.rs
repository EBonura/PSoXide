//! A corpse sheds its final posed triangles from the highest surfaces down.

use super::*;

/// Stateless five-second dissolve, timed after the death clip's final frame.
#[derive(Copy, Clone, Debug)]
pub struct ModelDeathDissolve {
    elapsed: u16,
    duration: u16,
}

impl ModelDeathDissolve {
    /// Keep playing the death clip before starting the dissolve.
    pub fn after_death(
        death_ticks: u16,
        frames: u16,
        sample_hz: u16,
        video_hz: VideoHz,
    ) -> Option<Self> {
        let hz = video_hz.as_nonzero_u32();
        let final_tick =
            (u32::from(frames.saturating_sub(1)) * hz).div_ceil(u32::from(sample_hz.max(1)));
        let elapsed = u32::from(death_ticks).checked_sub(final_tick)?;
        Some(Self {
            elapsed: elapsed.min(65535) as u16,
            duration: (hz * 5) as u16,
        })
    }

    /// Finished corpses need neither a pose nor any render submissions.
    pub const fn finished(self) -> bool {
        self.elapsed >= self.duration
    }

    fn age(self, y: i32, bottom: i32, top: i32) -> u16 {
        let sweep = i32::from(self.duration) * 3 / 5;
        let delay = (top - y).clamp(0, top - bottom) * sweep / (top - bottom).max(1);
        self.elapsed.saturating_sub(delay as u16)
    }

    fn flight_ticks(self) -> u16 {
        self.duration * 2 / 5
    }
}

fn center(points: [WorldVertex; 3]) -> WorldVertex {
    WorldVertex::new(
        points.iter().map(|p| p.x).sum::<i32>() / 3,
        points.iter().map(|p| p.y).sum::<i32>() / 3,
        points.iter().map(|p| p.z).sum::<i32>() / 3,
    )
}

fn drift(points: [WorldVertex; 3], index: usize, progress: i32) -> [WorldVertex; 3] {
    let c = center(points);
    let sign = if index & 1 == 0 { 1 } else { -1 };
    let angle = Angle::from_q12((sign * progress * 2) as u16);
    let (sin, cos) = (angle.sin_q12(), angle.cos_q12());
    let lift = progress * (48 + (index & 3) as i32 * 6) / 256;
    let dx = ((index & 7) as i32 - 3) * progress / 64;
    let dz = ((index & 3) as i32 - 1) * progress / 32;
    points.map(|p| {
        let (x, y, z) = (p.x - c.x, p.y - c.y, p.z - c.z);
        // Rotate around each fragment's own centre, not the corpse's root.
        WorldVertex::new(
            c.x + dx + x,
            c.y + lift + ((y * cos - z * sin) >> 12),
            c.z + dz + ((y * sin + z * cos) >> 12),
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw<const OT_DEPTH: usize>(
    effect: ModelDeathDissolve,
    projected: &[ProjectedVertex],
    faces: &[TexturedModelRenderFace],
    camera: WorldCamera,
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> TexturedModelRenderStats {
    let mut stats = TexturedModelRenderStats::default();
    if effect.finished() {
        return stats;
    }
    let mut bottom = i32::MAX;
    let mut top = i32::MIN;
    for &p in projected.iter().filter(|p| **p != ProjectedVertex::INVALID) {
        let y = dash_assembly::unproject(p, camera).y;
        bottom = bottom.min(y);
        top = top.max(y);
    }
    if bottom > top {
        return stats;
    }
    let options = options.with_textured_triangle_splitting(false);
    for (index, face) in faces.iter().enumerate() {
        let indices = face.vertex_indices().map(usize::from);
        if indices.iter().any(|&i| i >= projected.len()) {
            continue;
        }
        let original = indices.map(|i| projected[i]);
        if original.contains(&ProjectedVertex::INVALID) {
            continue;
        }
        let points = original.map(|p| dash_assembly::unproject(p, camera));
        let age = effect.age(center(points).y, bottom, top);
        if age >= effect.flight_ticks() {
            continue;
        }
        let progress = i32::from(age) * 256 / i32::from(effect.flight_ticks());
        let mut moved = original;
        if age > 0 {
            let [Some(a), Some(b), Some(c)] =
                drift(points, index, progress).map(|p| camera.project_world(p))
            else {
                continue;
            };
            moved = [a, b, c];
        }
        // Palettes mark nonzero texels for STP, as in the player's dash.
        // Quarter-strength blending avoids a bright shell where pieces overlap.
        // Fade texture modulation to zero without shrinking facets.
        let translucent = progress > 32 + (index as i32 & 31);
        let mat = if translucent {
            let strength = 256 - progress;
            let (r, g, b) = material.tint();
            material
                .with_raw_texture(false)
                .with_blend_mode(BlendMode::AddQuarter)
                .with_tint((
                    (i32::from(r) * strength >> 8) as u8,
                    (i32::from(g) * strength >> 8) as u8,
                    (i32::from(b) * strength >> 8) as u8,
                ))
        } else {
            material
        };
        let face = TexturedModelRenderFace::new_with_palette_bank(
            [0, 1, 2],
            face.uvs(),
            face.palette_bank(),
        );
        let next = world.submit_projected_model_faces(
            triangles,
            &moved,
            &[face],
            mat,
            if age > 0 {
                options
                    .with_material_layer(mat)
                    .with_cull_mode(CullMode::None)
            } else {
                options
            },
        );
        accumulate_model_stats(&mut stats, next);
        if stats.primitive_overflow || stats.command_overflow {
            break;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_pose_precedes_five_second_dissolve_at_both_video_rates() {
        for hz in [50, 60] {
            let video = VideoHz::from_u16(hz);
            let end = 2 * hz;
            assert!(ModelDeathDissolve::after_death(end - 1, 25, 12, video).is_none());
            assert!(
                !ModelDeathDissolve::after_death(end + 5 * hz - 1, 25, 12, video)
                    .unwrap()
                    .finished()
            );
            assert!(ModelDeathDissolve::after_death(end + 5 * hz, 25, 12, video)
                .unwrap()
                .finished());
        }
    }

    #[test]
    fn horizontal_corpse_uses_posed_height_and_lower_faces_leave_later() {
        let effect = ModelDeathDissolve {
            elapsed: 90,
            duration: 300,
        };
        assert_eq!(effect.age(18, 2, 18), 90);
        assert_eq!(effect.age(2, 2, 18), 0);
        assert_eq!(effect.age(10, 2, 18), 0);
        let final_effect = ModelDeathDissolve {
            elapsed: 300,
            ..effect
        };
        assert!(final_effect.age(2, 2, 18) >= final_effect.flight_ticks());
    }

    #[test]
    fn fragments_rise_from_their_own_centres_without_changing_size() {
        let points = [
            WorldVertex::new(-10, 0, 0),
            WorldVertex::new(10, 0, 0),
            WorldVertex::new(0, 12, 0),
        ];
        assert_eq!(drift(points, 0, 0), points);
        let moved = drift(points, 0, 128);
        assert!(center(moved).y > center(points).y + 20);
        let distance = |a: WorldVertex, b: WorldVertex| {
            (a.x - b.x).pow(2) + (a.y - b.y).pow(2) + (a.z - b.z).pow(2)
        };
        assert!((distance(moved[0], moved[1]) - distance(points[0], points[1])).abs() < 60);
    }
}
