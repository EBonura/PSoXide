//! Scatter the already shaded body packets, then return them to the live pose.

use super::*;

const DEPARTURE_TICKS: u32 = 36;
const REBUILD_START: u32 = 12;
const REBUILD_END: u32 = 48;
const FRAGMENT_CAP: usize = 64;

#[derive(Copy, Clone)]
struct DepartureFragment {
    vertices: [[i16; 3]; 3],
    face: TexturedModelRenderFace,
}

impl DepartureFragment {
    const EMPTY: Self = Self {
        vertices: [[0; 3]; 3],
        face: TexturedModelRenderFace::ZERO,
    };

    fn world_vertices(self, origin: WorldVertex) -> [WorldVertex; 3] {
        self.vertices.map(|v| {
            WorldVertex::new(
                origin.x + i32::from(v[0]),
                origin.y + i32::from(v[1]),
                origin.z + i32::from(v[2]),
            )
        })
    }
}

/// One bounded cloud of departing mesh pieces, anchored in the level.
/// Its visual lifetime does not extend the motor's action or invulnerability.
pub struct PlayerDashAssembly {
    started: u32,
    seen_dash: bool,
    explicit_burst: bool,
    active: bool,
    capture_pending: bool,
    interrupted: bool,
    fragment_count: usize,
    fragments: [DepartureFragment; FRAGMENT_CAP],
    fragment_origin: WorldVertex,
    material: TextureMaterial,
}

impl PlayerDashAssembly {
    /// Empty effect state, also used when resetting the player or changing level.
    pub const fn new() -> Self {
        Self {
            started: 0,
            seen_dash: false,
            explicit_burst: false,
            active: false,
            capture_pending: false,
            interrupted: false,
            fragment_count: 0,
            fragments: [DepartureFragment::EMPTY; FRAGMENT_CAP],
            fragment_origin: WorldVertex::ZERO,
            material: TextureMaterial::new(0, 0),
        }
    }

    fn start(&mut self, started: u32) {
        self.started = started;
        self.seen_dash = true;
        self.explicit_burst = false;
        self.active = true;
        self.capture_pending = true;
        self.interrupted = false;
        self.fragment_count = 0;
    }

    /// Play the same breakup and reconstruction for a cinematic impact.
    /// This changes presentation only, without starting a motor action.
    pub fn burst(&mut self, now: SimTick) {
        self.start(now.as_u32());
        self.explicit_burst = true;
        self.seen_dash = false;
    }

    /// Discard particles and restore the solid body at a cinematic handoff.
    pub fn cancel(&mut self) {
        self.active = false;
        self.capture_pending = false;
        self.explicit_burst = false;
        self.seen_dash = false;
    }

    /// Track a new evade without tying reconstruction to its animation clip.
    pub fn observe(&mut self, pose: PlayerActorPoseSnapshot) {
        let now = pose.pose().tick().as_u32();
        self.observe_action(pose.action(), now.saturating_sub(pose.action_tick), now);
    }

    fn observe_action(&mut self, action: CharacterAnimationAction, started: u32, now: u32) {
        if matches!(
            action,
            CharacterAnimationAction::Roll
                | CharacterAnimationAction::DashLeft
                | CharacterAnimationAction::DashRight
        ) {
            if !self.seen_dash || started != self.started {
                self.start(started);
            }
        } else if matches!(
            action,
            CharacterAnimationAction::HitReact
                | CharacterAnimationAction::Death
                | CharacterAnimationAction::LightAttack
                | CharacterAnimationAction::HeavyAttack
                | CharacterAnimationAction::VertLightAttack
                | CharacterAnimationAction::VertHeavyAttack
        ) || (action == CharacterAnimationAction::Intro && !self.explicit_burst)
        {
            // Combat feedback must immediately show the actual hit/attack pose.
            self.interrupted = true;
            self.capture_pending = false;
        }
        if now.saturating_sub(self.started) >= REBUILD_END {
            self.active = false;
            self.capture_pending = false;
        }
    }

    /// Current body effect; walking after recovery still finishes reassembly.
    pub fn visual(&self, now: SimTick) -> DashWireVisual {
        if !self.active || self.interrupted {
            return DashWireVisual::Solid;
        }
        let age = now.as_u32().saturating_sub(self.started);
        if age < REBUILD_START {
            DashWireVisual::Wire
        } else if age < REBUILD_END {
            DashWireVisual::Restoring {
                progress_q8: ((age - REBUILD_START) * 255 / (REBUILD_END - REBUILD_START)) as u8,
            }
        } else {
            DashWireVisual::Solid
        }
    }

    pub(super) fn needs_capture(&self) -> bool {
        self.capture_pending
    }

    pub(super) fn capture(
        &mut self,
        projected: &[ProjectedVertex],
        faces: &[TexturedModelRenderFace],
        camera: WorldCamera,
        material: TextureMaterial,
    ) {
        if !self.capture_pending {
            return;
        }
        self.capture_pending = false;
        self.material = material;
        self.fragment_origin = camera.position;
        let step = faces.len().div_ceil(FRAGMENT_CAP).max(1);
        for face in faces.iter().step_by(step) {
            let indices = face.vertex_indices().map(usize::from);
            if indices
                .iter()
                .any(|&i| i >= projected.len() || projected[i] == ProjectedVertex::INVALID)
            {
                continue;
            }
            let points = indices.map(|i| projected[i]);
            // Visible fragments are near the camera. Relative i16 coordinates
            // retain their exact integer positions with half the coordinate RAM.
            let vertices = points.map(|p| {
                let v = unproject(p, camera);
                [
                    v.x - camera.position.x,
                    v.y - camera.position.y,
                    v.z - camera.position.z,
                ]
            });
            if vertices
                .iter()
                .flatten()
                .any(|v| i16::try_from(*v).is_err())
            {
                continue;
            }
            self.fragments[self.fragment_count] = DepartureFragment {
                vertices: vertices.map(|v| v.map(|c| c as i16)),
                face: *face,
            };
            self.fragment_count += 1;
            if self.fragment_count == FRAGMENT_CAP {
                break;
            }
        }
    }

    /// Draw the departure cloud at its captured world position, even after the
    /// player or camera has moved. Each piece gets its own current depth order.
    pub fn draw_departure<const OT_DEPTH: usize>(
        &self,
        now: SimTick,
        camera: WorldCamera,
        options: WorldSurfaceOptions,
        triangles: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> u16 {
        let age = now.as_u32().saturating_sub(self.started);
        if !self.active || age == 0 || age >= DEPARTURE_TICKS {
            return 0;
        }
        let fade = 255 - age * 255 / DEPARTURE_TICKS;
        let tint = self.material.tint();
        let material = self
            .material
            .with_raw_texture(false)
            .with_blend_mode(BlendMode::Add)
            .with_tint((
                (u32::from(tint.0) * fade / 255) as u8,
                (u32::from(tint.1) * fade / 255) as u8,
                (u32::from(tint.2) * fade / 255) as u8,
            ));
        let options = options
            .with_cull_mode(CullMode::None)
            .with_material_layer(material)
            .with_textured_triangle_splitting(false);
        let mut submitted = 0u16;
        for (index, fragment) in self.fragments[..self.fragment_count].iter().enumerate() {
            let vertices =
                departure_vertices(fragment.world_vertices(self.fragment_origin), index, age);
            let stats = world.submit_textured_world_triangle(
                triangles,
                camera,
                vertices,
                fragment.face.uvs(),
                material.with_clut_bank(fragment.face.palette_bank()),
                options,
            );
            submitted = submitted.saturating_add(stats.submitted_triangles);
            if stats.primitive_overflow || stats.command_overflow {
                break;
            }
        }
        submitted
    }
}

impl Default for PlayerDashAssembly {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn unproject(p: ProjectedVertex, camera: WorldCamera) -> WorldVertex {
    let x = (i32::from(p.sx) - i32::from(camera.projection.screen_x)) * p.sz
        / camera.projection.focal_length.max(1);
    let y = (i32::from(camera.projection.screen_y) - i32::from(p.sy)) * p.sz
        / camera.projection.focal_length.max(1);
    let z = (-y * camera.sin_pitch.raw() + p.sz * camera.cos_pitch.raw()) >> 12;
    let dy = (y * camera.cos_pitch.raw() + p.sz * camera.sin_pitch.raw()) >> 12;
    WorldVertex::new(
        camera.position.x + ((x * camera.cos_yaw.raw() - z * camera.sin_yaw.raw()) >> 12),
        camera.position.y + dy,
        camera.position.z + ((-x * camera.sin_yaw.raw() - z * camera.cos_yaw.raw()) >> 12),
    )
}

fn departure_vertices(vertices: [WorldVertex; 3], index: usize, age: u32) -> [WorldVertex; 3] {
    let center = WorldVertex::new(
        vertices.iter().map(|v| v.x).sum::<i32>() / 3,
        vertices.iter().map(|v| v.y).sum::<i32>() / 3,
        vertices.iter().map(|v| v.z).sum::<i32>() / 3,
    );
    let angle = Angle::from_q12((index as u16).wrapping_mul(1567));
    let radius = age.min(DEPARTURE_TICKS) as i32;
    let drift = WorldVertex::new(
        angle.sin_q12() * radius >> 12,
        (index as i32 % 5 - 1) * radius / 5,
        angle.cos_q12() * radius >> 12,
    );
    let scale = 384 - age.min(DEPARTURE_TICKS) as i32 * 128 / DEPARTURE_TICKS as i32;
    vertices.map(|v| {
        WorldVertex::new(
            center.x + drift.x + ((v.x - center.x) * scale >> 8),
            center.y + drift.y + ((v.y - center.y) * scale >> 8),
            center.z + drift.z + ((v.z - center.z) * scale >> 8),
        )
    })
}

/// Packet geometry stays in its original depth plane, preserving world occlusion.
///
/// # Safety
/// The supplied arena range must contain only the freshly submitted textured
/// player body, before wireframe or equipment packets are appended.
pub(super) unsafe fn scatter_body_packets(
    arena: &mut PrimitivePacketArena<'_>,
    first_slot: usize,
    projected: &[ProjectedVertex],
    visual: DashWireVisual,
) {
    let distance = match visual {
        DashWireVisual::Converting { progress_q8 } => i32::from(progress_q8),
        // Linear approach keeps the travelling pieces visible throughout
        // reconstruction instead of collapsing most distance in its first frames.
        DashWireVisual::Restoring { progress_q8 } => 255 - i32::from(progress_q8),
        _ => return,
    };
    if distance == 0 {
        return;
    }
    let mut top = i16::MAX;
    let mut bottom = i16::MIN;
    for point in projected
        .iter()
        .filter(|point| **point != ProjectedVertex::INVALID)
    {
        top = top.min(point.sy);
        bottom = bottom.max(point.sy);
    }
    let height = (i32::from(bottom) - i32::from(top)).clamp(16, 240);
    let end = arena.used_slots();
    let mut index = 0;
    // SAFETY: the caller guarantees a contiguous range of body TriTextureds.
    unsafe {
        arena.mutate_typed_slots::<TriTextured>(first_slot, end, |triangle| {
            scatter_triangle(triangle, index, height, distance);
            index += 1;
        });
    }
}

fn scatter_triangle(triangle: &mut TriTextured, index: u32, height: i32, distance: i32) {
    if distance == 0 {
        return;
    }
    let corners = [triangle.v0, triangle.v1, triangle.v2]
        .map(|word| (i32::from(word as i16), i32::from((word >> 16) as i16)));
    let cx = corners.iter().map(|p| p.0).sum::<i32>() / 3;
    let cy = corners.iter().map(|p| p.1).sum::<i32>() / 3;
    // Eight stable scattering directions, shared by small groups of facets.
    // No per-particle state, additional posing, or new textures are required.
    const DIRECTIONS: [(i32, i32); 8] = [
        (256, 0),
        (181, 181),
        (0, 256),
        (-181, 181),
        (-256, 0),
        (-181, -181),
        (0, -256),
        (181, -181),
    ];
    let (dx, dy) = DIRECTIONS[(index.wrapping_mul(5) & 7) as usize];
    let radius = height * distance / 640;
    let dx = dx * radius >> 8;
    let dy = dy * radius >> 8;
    // Keep travelling facets legible at native resolution. Shrinking them to
    // points hid most of reconstruction even when its timer was longer.
    let scale = 256 - distance / 4;
    let moved = corners.map(|(x, y)| {
        (
            cx + dx + ((x - cx) * scale >> 8),
            cy + dy + ((y - cy) * scale >> 8),
        )
    });
    let safe = moved
        .iter()
        .all(|&(x, y)| (-1023..=1023).contains(&x) && (-1023..=1023).contains(&y))
        && moved.iter().map(|p| p.0).max().unwrap() - moved.iter().map(|p| p.0).min().unwrap()
            <= 1023
        && moved.iter().map(|p| p.1).max().unwrap() - moved.iter().map(|p| p.1).min().unwrap()
            <= 511;
    if !safe || distance >= 250 {
        // Degenerate polygons preserve the DMA chain without touching its tags.
        triangle.v1 = triangle.v0;
        triangle.v2 = triangle.v0;
        return;
    }
    let packed =
        moved.map(|(x, y)| u32::from(x as i16 as u16) | (u32::from(y as i16 as u16) << 16));
    [triangle.v0, triangle.v1, triangle.v2] = packed;
    // Uploaded model palettes already mark nonzero entries for STP blending.
    // Additive modulation lets a fragment fade all the way to the background,
    // while retaining its original texture and per-face crystal colour.
    // Return small groups to opacity near attachment, avoiding a whole-body
    // brightness jump when the last translucent frame becomes solid.
    if distance < 64 && (index.wrapping_mul(37) & 63) >= distance as u32 {
        return;
    }
    let strength = 255 - distance * distance / 255;
    let base = triangle.color_cmd;
    let channel = |shift: u32| (((base >> shift) & 255) as i32 * strength / 255) as u32;
    triangle.color_cmd =
        ((base | 0x0200_0000) & 0xfe00_0000) | channel(0) | (channel(8) << 8) | (channel(16) << 16);
    triangle.uv1_tpage = (triangle.uv1_tpage & !(3 << 21)) | (1 << 21);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruction_outlasts_recovery_but_never_hides_a_hit_or_attack() {
        let mut effect = PlayerDashAssembly::new();
        effect.observe_action(CharacterAnimationAction::Roll, 100, 100);
        assert!(effect.needs_capture());
        assert_eq!(effect.visual(SimTick::from_u32(110)), DashWireVisual::Wire);
        effect.observe_action(CharacterAnimationAction::Walk, 135, 135);
        assert!(matches!(
            effect.visual(SimTick::from_u32(135)),
            DashWireVisual::Restoring { .. }
        ));
        effect.observe_action(CharacterAnimationAction::HitReact, 136, 136);
        assert!(!effect.needs_capture());
        assert_eq!(effect.visual(SimTick::from_u32(136)), DashWireVisual::Solid);
        effect.observe_action(CharacterAnimationAction::DashLeft, 150, 150);
        assert_eq!(effect.visual(SimTick::from_u32(150)), DashWireVisual::Wire);
        effect.observe_action(CharacterAnimationAction::Idle, 185, 198);
        assert_eq!(effect.visual(SimTick::from_u32(198)), DashWireVisual::Solid);
        assert!(!effect.needs_capture());
    }

    #[test]
    fn explicit_intro_burst_survives_observation_and_cancels_cleanly() {
        let mut effect = PlayerDashAssembly::new();
        effect.burst(SimTick::from_u32(100));
        effect.observe_action(CharacterAnimationAction::Intro, 0, 101);
        assert!(effect.needs_capture());
        assert_eq!(effect.visual(SimTick::from_u32(101)), DashWireVisual::Wire);
        effect.observe_action(CharacterAnimationAction::Intro, 0, 130);
        assert!(matches!(
            effect.visual(SimTick::from_u32(130)),
            DashWireVisual::Restoring { .. }
        ));
        effect.cancel();
        assert!(!effect.active);
        assert!(!effect.needs_capture());
        assert_eq!(effect.visual(SimTick::from_u32(131)), DashWireVisual::Solid);
        effect.observe_action(CharacterAnimationAction::Roll, 140, 140);
        assert!(effect.needs_capture());
        effect.observe_action(CharacterAnimationAction::Intro, 150, 150);
        assert_eq!(effect.visual(SimTick::from_u32(150)), DashWireVisual::Solid);
        effect.burst(SimTick::from_u32(200));
        effect.observe_action(CharacterAnimationAction::Intro, 0, 248);
        assert!(!effect.active);
        assert_eq!(effect.visual(SimTick::from_u32(248)), DashWireVisual::Solid);
    }

    #[test]
    fn departing_mesh_stays_in_world_space_when_the_camera_moves() {
        let mut effect = PlayerDashAssembly::new();
        effect.observe_action(CharacterAnimationAction::Roll, 100, 100);
        let camera = WorldCamera::orbit(
            psx_engine::WorldProjection::new(160, 120, 256, 8),
            WorldVertex::ZERO,
            256,
            Angle::from_q12(430),
            Angle::from_q12(160),
        );
        let points = [
            WorldVertex::new(-8, 0, 0),
            WorldVertex::new(8, 0, 0),
            WorldVertex::new(0, 20, 0),
        ]
        .map(|v| camera.project_world(v).unwrap());
        let face =
            TexturedModelRenderFace::new_with_palette_bank([0, 1, 2], [(0, 0), (8, 0), (4, 8)], 2);
        effect.capture(&points, &[face], camera, TextureMaterial::new(0, 0));
        assert_eq!(effect.fragment_count, 1);
        let captured = effect.fragments[0].world_vertices(effect.fragment_origin);
        for (world, screen) in captured.into_iter().zip(points) {
            let roundtrip = camera.project_world(world).unwrap();
            assert!((i32::from(roundtrip.sx) - i32::from(screen.sx)).abs() <= 2);
            assert!((i32::from(roundtrip.sy) - i32::from(screen.sy)).abs() <= 2);
        }
        let mut moved_camera = camera;
        moved_camera.position.x += 100;
        effect.capture(&points, &[face], moved_camera, TextureMaterial::new(1, 1));
        assert_eq!(
            effect.fragments[0].world_vertices(effect.fragment_origin),
            captured,
            "subsequent camera movement cannot recapture the cloud"
        );
        assert_eq!(effect.fragments[0].face.palette_bank(), 2);
        let moved_screen = moved_camera.project_world(captured[0]).unwrap();
        assert_ne!(moved_screen.sx, points[0].sx);
    }

    fn triangle() -> TriTextured {
        TriTextured {
            tag: 0x08001234,
            tex_window: 0xe2000000,
            color_cmd: 0x246080a0,
            v0: 100 | (100 << 16),
            v1: 110 | (100 << 16),
            v2: 105 | (112 << 16),
            uv0_clut: 0x78201234,
            uv1_tpage: 0x01985678,
            uv2: 0x0000abcd,
        }
    }

    #[test]
    fn fragments_preserve_texture_lighting_ratios_and_dma_links() {
        let mut fragment = triangle();
        let original = triangle();
        scatter_triangle(&mut fragment, 3, 80, 128);
        assert_eq!(fragment.tag, original.tag);
        assert_eq!(fragment.tex_window, original.tex_window);
        assert_eq!(fragment.uv0_clut, original.uv0_clut);
        assert_eq!(fragment.uv2, original.uv2);
        assert_eq!(
            fragment.uv1_tpage & !(3 << 21),
            original.uv1_tpage & !(3 << 21)
        );
        assert_eq!(fragment.color_cmd >> 24, 0x26);
        assert_eq!(fragment.color_cmd & 0xffffff, 0x475f77);
        assert_ne!(fragment.v0, original.v0);
        scatter_triangle(&mut fragment, 3, 80, 255);
        assert_eq!(fragment.v0, fragment.v1);
        assert_eq!(fragment.v0, fragment.v2);
        let mut restored = triangle();
        scatter_triangle(&mut restored, 3, 80, 0);
        assert_eq!(restored.color_cmd, original.color_cmd);
        assert_eq!(restored.v0, original.v0);
        assert_eq!(restored.uv1_tpage, original.uv1_tpage);
    }
}
