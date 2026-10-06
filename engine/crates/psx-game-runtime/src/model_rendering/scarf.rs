//! Small fixed-step cloth ribbon, enabled by a model's `scarf_neck` socket.
//! Simulation owns sixteen particles; drawing never advances physics.

use super::*;
use psx_engine::{LoadedAnchoredCameraGte, Vec3I16, ViewVertex, WorldRenderLayer};
use psx_math::int32::isqrt_i32;

const ROWS: usize = 8;
const POINTS: usize = ROWS * 2;
const Q: i32 = 16;
type Point = [i32; 3];
// The fixed-update cloth solve has no live renderer/collision scratch data.
// The guest build proves its complete call tree fits in the scratchpad.
type ScarfStack = psx_engine::scratchpad::ScratchpadStack<0, { psx_engine::scratchpad::SIZE }>;
const _: () = psx_engine::scratchpad::assert_disjoint(&[ScarfStack::REGION]);

/// A neck wrap and a tapering cloth tail. All dynamic storage is bounded and
/// independent of the camera, framebuffer, material palette and render cadence.
pub struct PlayerScarf {
    active: bool,
    model: u16,
    tick: u32,
    solve_tick: u32,
    anchor: WorldVertex,
    basis: Mat3I16,
    height: i32,
    points: [Point; POINTS],
    previous: [Point; POINTS],
}

impl PlayerScarf {
    /// Empty state, also used at scene changes and respawns.
    pub const fn new() -> Self {
        Self {
            active: false,
            model: 0,
            tick: 0,
            solve_tick: 0,
            anchor: WorldVertex::ZERO,
            basis: Mat3I16::IDENTITY,
            height: 1,
            points: [[0; 3]; POINTS],
            previous: [[0; 3]; POINTS],
        }
    }

    /// Whether the current player model opts into the scarf.
    pub const fn active(&self) -> bool {
        self.active
    }

    /// Sample the attachment and advance exactly once for this simulation tick.
    /// Teleports, model replacements and clock discontinuities reset the ribbon.
    pub fn tick(
        &mut self,
        tables: ModelTables,
        pose: PlayerActorPoseSnapshot,
        height: i32,
        floor_y: i32,
        hz: VideoHz,
    ) {
        let model = pose.model();
        let sockets = tables
            .model_sockets
            .get(
                model.socket_first.to_usize()
                    ..model
                        .socket_first
                        .to_usize()
                        .saturating_add(model.socket_count as usize),
            )
            .unwrap_or(&[]);
        let Some(socket) = sockets.iter().find(|s| s.name == "scarf_neck") else {
            self.active = false;
            return;
        };
        let Some(joint) = pose.pose().joint_world_transform(socket.joint) else {
            self.active = false;
            return;
        };
        let offset = rotate(joint.rotation, socket.translation);
        let anchor = WorldVertex::new(
            joint.translation.x.saturating_add(offset[0]),
            // The socket sits at the upper neck; the wrap belongs below the jaw.
            joint
                .translation
                .y
                .saturating_add(offset[1])
                .saturating_sub(height / 20),
            joint.translation.z.saturating_add(offset[2]),
        );
        // SAFETY: this phase only mutates owned particle arrays. Attachment
        // sampling finished above; no other scratchpad reservation is live.
        unsafe {
            ScarfStack::run(|| {
                self.advance(
                    pose.pose().tick().as_u32(),
                    model.index.raw(),
                    anchor,
                    // A neck joint's bind basis need not use the model's upright/forward
                    // axes. Position follows the animated neck; cloth orientation uses
                    // the actor's presentation basis so the tail starts down the back.
                    pose.pose().rotation(),
                    height,
                    // The visual origin is lifted to the hips by the model
                    // floor correction. Only the motor supplies the real floor.
                    floor_y,
                    hz,
                );
            });
        }
    }

    #[inline(never)]
    fn advance(
        &mut self,
        tick: u32,
        model: u16,
        anchor: WorldVertex,
        basis: Mat3I16,
        height: i32,
        floor_y: i32,
        hz: VideoHz,
    ) {
        let height = height.clamp(128, 4096);
        if self.active && tick == self.tick && model == self.model {
            return;
        }
        let delta = [
            self.anchor.x - anchor.x,
            self.anchor.y - anchor.y,
            self.anchor.z - anchor.z,
        ];
        let reset = !self.active
            || model != self.model
            || height != self.height
            || tick.wrapping_sub(self.tick) > 8
            || delta.iter().any(|d| d.abs() > height * 2);
        self.active = true;
        self.model = model;
        self.tick = tick;
        self.anchor = anchor;
        self.basis = basis;
        self.height = height;
        if reset {
            self.solve_tick = tick;
            for row in 0..ROWS {
                for side in 0..2 {
                    let p = self.rest_point(row, side);
                    self.points[row * 2 + side] = p;
                    self.previous[row * 2 + side] = p;
                }
            }
            for i in 2..POINTS {
                self.collide_body(i, floor_y);
            }
            self.previous = self.points;
            return;
        }
        // Rebase both Verlet positions without dragging free particles with
        // the moving attachment. The displacement becomes physical inertia.
        let bound = height * Q * 2;
        for i in 0..POINTS {
            for axis in 0..3 {
                self.points[i][axis] =
                    (self.points[i][axis] + delta[axis] * Q).clamp(-bound, bound);
                self.previous[i][axis] =
                    (self.previous[i][axis] + delta[axis] * Q).clamp(-bound, bound);
            }
        }
        let pins = [self.rest_point(0, 0), self.rest_point(0, 1)];
        self.points[..2].copy_from_slice(&pins);
        self.previous[..2].copy_from_slice(&pins);
        // Cloth solves at a fixed half-video rate (30 Hz NTSC, 25 Hz PAL),
        // independent of rendered frames. The animated neck still updates on
        // every gameplay tick, and both histories rebase together above.
        if tick.wrapping_sub(self.solve_tick) < 2 {
            for i in 2..POINTS {
                self.collide_body(i, floor_y);
            }
            return;
        }
        self.solve_tick = tick;
        let segment = height * Q / 16;
        let hz = hz.as_nonzero_u32() as i32 / 2;
        let gravity = (height * 3600 / (64 * hz * hz)).max(1);
        let damping = if hz <= 25 { 236 } else { 240 };
        for i in 2..POINTS {
            let old = self.points[i];
            for axis in 0..3 {
                let velocity =
                    ((old[axis] - self.previous[i][axis]) * damping / 256).clamp(-segment, segment);
                self.points[i][axis] = (old[axis] + velocity).clamp(-bound, bound);
            }
            self.points[i][1] -= gravity;
            self.previous[i] = old;
        }
        for _ in 0..2 {
            self.points[..2].copy_from_slice(&pins);
            for row in 0..ROWS {
                self.constrain(row * 2, row * 2 + 1, self.width(row).pow(2), 256);
                if row > 0 {
                    for side in 0..2 {
                        self.constrain(
                            (row - 1) * 2 + side,
                            row * 2 + side,
                            segment * segment,
                            256,
                        );
                    }
                    let width = (self.width(row - 1) + self.width(row)) / 2;
                    let diagonal_squared = segment * segment + width * width;
                    // Alternate diagonals resist shearing without doubling the solve.
                    self.constrain(
                        (row - 1) * 2 + (row & 1),
                        row * 2 + 1 - (row & 1),
                        diagonal_squared,
                        192,
                    );
                }
                if row > 1 {
                    for side in 0..2 {
                        self.constrain(
                            (row - 2) * 2 + side,
                            row * 2 + side,
                            segment * segment * 4,
                            48,
                        );
                    }
                }
            }
            for i in 2..POINTS {
                self.collide_body(i, floor_y);
            }
        }
        self.points[..2].copy_from_slice(&pins);
        self.previous[..2].copy_from_slice(&pins);
    }

    fn width(&self, row: usize) -> i32 {
        self.height * Q * (16 - row as i32) / (12 * 16)
    }

    fn rest_point(&self, row: usize, side: usize) -> Point {
        let local = [
            if side == 0 {
                -self.width(row) / 2
            } else {
                self.width(row) / 2
            },
            -self.height * Q / 80 - row as i32 * self.height * Q / 16,
            -self.height * Q / 18 - row as i32 * self.height * Q / 64,
        ];
        rotate(self.basis, local)
    }

    fn constrain(&mut self, a: usize, b: usize, rest_squared: i32, stiffness: i32) {
        let mut pa = self.points[a];
        let mut pb = self.points[b];
        let delta = core::array::from_fn::<_, 3, _>(|k| pb[k] - pa[k]);
        let squared = delta
            .iter()
            .map(|v| v.saturating_mul(*v))
            .fold(0i32, i32::saturating_add);
        // Position-based length correction using squared distances. This
        // converges around the rest length without a square root per link.
        // Divide before multiplying to keep all arithmetic in signed 32 bits.
        let ratio = (squared - rest_squared) / (squared.saturating_add(rest_squared) / 256).max(1);
        let factor = ratio.clamp(-256, 256) * stiffness / 256;
        let free_a = a >= 2;
        let free_b = b >= 2;
        let both_free = free_a && free_b;
        for k in 0..3 {
            // Keep these constant divisions: a runtime 256/512 selection
            // otherwise emits three expensive MIPS DIVs for every constraint.
            let correction = delta[k] * factor / 256;
            let correction = if both_free {
                correction / 2
            } else {
                correction
            };
            if free_a {
                pa[k] += correction;
            }
            if free_b {
                pb[k] -= correction;
            }
        }
        self.points[a] = pa;
        self.points[b] = pb;
    }

    fn collide_body(&mut self, i: usize, floor_y: i32) {
        // Upright torso capsule, deliberately cheaper than sixteen BSP traces.
        let p = self.points[i];
        let center_y = p[1].clamp(-self.height * Q / 3, -self.height * Q / 8);
        let d = [p[0], p[1] - center_y, p[2]];
        let radius = self.height * Q / 9;
        let d2 = d
            .iter()
            .map(|x| x.saturating_mul(*x))
            .fold(0i32, i32::saturating_add);
        if d2 < radius * radius {
            let len = isqrt_i32(d2).max(1);
            if d2 == 0 {
                self.points[i][2] = -radius;
            } else {
                let factor = (radius * 256 + len - 1) / len;
                self.points[i] = [
                    d[0] * factor / 256,
                    center_y + d[1] * factor / 256,
                    d[2] * factor / 256,
                ];
            }
        }
        self.points[i][1] = self.points[i][1].max((floor_y - self.anchor.y + 8) * Q);
    }

    /// Draw opaque, double-sided cloth in the supplied persistent stance hue.
    /// The body can fade independently; no texture upload or body re-pose occurs.
    pub fn draw<const OT_DEPTH: usize>(
        &self,
        camera: WorldCamera,
        color: (u8, u8, u8),
        assembly: Option<ModelPhaseAssembly>,
        options: WorldSurfaceOptions,
        triangles: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) -> u16 {
        if !self.active {
            return 0;
        }
        let mut vertices = [WorldVertex::ZERO; 32];
        // Two hand-shaped cloth edges, in 1/1024 of character height.
        // The upper edge hugs the neck; the wider lower edge folds onto the
        // collarbones with an off-centre front point. Unequal heights and
        // depths avoid the rigid, circular tube silhouette of the first wrap.
        const WRAP: [[Point; 2]; 8] = [
            [[30, 4, -2], [44, -24, 0]],
            [[23, 0, 20], [34, -35, 33]],
            [[2, -5, 27], [-7, -54, 43]],
            [[-23, 4, 20], [-34, -29, 30]],
            [[-31, 6, -4], [-43, -18, -6]],
            [[-24, 4, -31], [-34, -22, -47]],
            [[0, 2, -39], [0, -26, -58]],
            [[25, 4, -29], [37, -20, -43]],
        ];
        for (i, edge) in WRAP.into_iter().enumerate() {
            for (side, point) in edge.into_iter().enumerate() {
                let v = rotate(self.basis, point.map(|axis| axis * self.height / 1024));
                vertices[i * 2 + side] = WorldVertex::new(
                    self.anchor.x + v[0],
                    self.anchor.y + v[1],
                    self.anchor.z + v[2],
                );
            }
        }
        for i in 0..POINTS {
            vertices[16 + i] = WorldVertex::new(
                self.anchor.x + self.points[i][0] / Q,
                self.anchor.y + self.points[i][1] / Q,
                self.anchor.z + self.points[i][2] / Q,
            );
        }
        let gte = LoadedAnchoredCameraGte::load(camera, self.anchor);
        let mut projected = [ProjectedVertex::INVALID; 32];
        for start in (0..32).step_by(3) {
            let local = core::array::from_fn(|k| {
                let p = vertices[(start + k).min(31)];
                Vec3I16::new(
                    (p.x - self.anchor.x) as i16,
                    (p.y - self.anchor.y) as i16,
                    (p.z - self.anchor.z) as i16,
                )
            });
            let out = gte.project_points(local);
            for k in 0..3 {
                if start + k < 32 {
                    projected[start + k] = out[k];
                }
            }
        }
        let assembly = assembly.filter(|effect| !effect.is_assembled());
        let options = options
            .with_cull_mode(CullMode::None)
            .with_render_layer(WorldRenderLayer::Opaque);
        let mut submitted = 0;
        for strip in 0..15 {
            let (a, b, shade) = if strip < 8 {
                (
                    strip * 2,
                    ((strip + 1) % 8) * 2,
                    [210, 190, 230, 205, 174, 190, 200, 224][strip],
                )
            } else {
                (16 + (strip - 8) * 2, 18 + (strip - 8) * 2, 220)
            };
            for (facet, (indices, brightness)) in [
                ([a, b, a + 1], shade),
                ([a + 1, b, b + 1], (shade + 20).min(256)),
            ]
            .into_iter()
            .enumerate()
            {
                let tint = (
                    (u32::from(color.0) * brightness / 256) as u8,
                    (u32::from(color.1) * brightness / 256) as u8,
                    (u32::from(color.2) * brightness / 256) as u8,
                );
                if let Some(effect) = assembly {
                    if let Some(fragment) =
                        effect.cloth_fragment(indices.map(|i| vertices[i]), strip * 2 + facet)
                    {
                        submitted += submit_clipped_cloth(
                            fragment.map(|v| camera.view_vertex(v)),
                            camera,
                            tint,
                            options,
                            triangles,
                            world,
                        );
                    }
                    continue;
                }
                let p = indices.map(|i| projected[i]);
                if p.iter().all(|p| {
                    *p != ProjectedVertex::INVALID
                        && p.sz > camera.projection.focal_length / 2
                        && p.sz < 65535
                        && p.sx > -1023
                        && p.sx < 1023
                        && p.sy > -1023
                        && p.sy < 1023
                }) && projected_triangle_batchable(p)
                {
                    submitted += submit_cloth_triangle(p, tint, options, triangles, world);
                } else {
                    submitted += submit_clipped_cloth(
                        indices.map(|i| camera.view_vertex(vertices[i])),
                        camera,
                        tint,
                        options,
                        triangles,
                        world,
                    );
                }
            }
        }
        submitted
    }
}

impl Default for PlayerScarf {
    fn default() -> Self {
        Self::new()
    }
}

fn rotate(basis: Mat3I16, p: Point) -> Point {
    core::array::from_fn(|r| {
        (i32::from(basis.m[r][0]) * p[0]
            + i32::from(basis.m[r][1]) * p[1]
            + i32::from(basis.m[r][2]) * p[2])
            >> 12
    })
}

fn submit_cloth_triangle<const N: usize>(
    p: [ProjectedVertex; 3],
    color: (u8, u8, u8),
    options: WorldSurfaceOptions,
    arena: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, N>,
) -> u16 {
    if !projected_triangle_batchable(p) {
        return 0;
    }
    world
        .submit_gouraud_triangle(
            arena,
            p.map(|v| ProjectedLit {
                sx: v.sx,
                sy: v.sy,
                sz: v.sz.clamp(0, 65535) as u16,
                r: color.0,
                g: color.1,
                b: color.2,
            }),
            options,
        )
        .submitted_triangles
}

// Near crossings preserve the remaining cloth polygon, rather than making
// a whole segment disappear beside the camera. At most four output corners.
fn submit_clipped_cloth<const N: usize>(
    input: [ViewVertex; 3],
    camera: WorldCamera,
    color: (u8, u8, u8),
    options: WorldSurfaceOptions,
    arena: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, N>,
) -> u16 {
    let near = camera.projection.near_z.max(1);
    let mut polygon = [ViewVertex::new(0, 0, 0); 4];
    let mut count = 0;
    for i in 0..3 {
        let a = input[i];
        let b = input[(i + 1) % 3];
        if a.z >= near {
            polygon[count] = a;
            count += 1;
        }
        if (a.z >= near) != (b.z >= near) {
            let t = (near - a.z) * 4096 / (b.z - a.z);
            polygon[count] = ViewVertex::new(
                a.x + ((b.x - a.x) * t >> 12),
                a.y + ((b.y - a.y) * t >> 12),
                near,
            );
            count += 1;
        }
    }
    let mut submitted = 0;
    for i in 1..count.saturating_sub(1) {
        if let [Some(a), Some(b), Some(c)] =
            [polygon[0], polygon[i], polygon[i + 1]].map(|v| camera.projection.project_view(v))
        {
            submitted += submit_cloth_triangle([a, b, c], color, options, arena, world);
        }
    }
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::{DepthBand, DepthRange, OtFrame, WorldProjection, WorldTriCommand};
    use psx_gpu::{ot::OrderingTable, prim::TriGouraud};
    fn step(s: &mut PlayerScarf, t: u32, x: i32, z: i32) {
        s.advance(
            t,
            1,
            WorldVertex::new(x, 1000, z),
            Mat3I16::IDENTITY,
            1024,
            0,
            VideoHz::NTSC,
        );
    }
    #[test]
    fn pins_follow_pose_and_tail_has_inertia_without_unbounded_stretch() {
        let mut s = PlayerScarf::new();
        for t in 0..180 {
            step(&mut s, t, 0, 0);
        }
        let before = s.points[14][0];
        step(&mut s, 180, 40, 0);
        assert!(
            s.points[14][0] < before - 20 * Q,
            "free cloth must lag the moving neck"
        );
        for t in 181..500 {
            step(&mut s, t, 40, 0);
        }
        assert_eq!(s.points[0], s.rest_point(0, 0));
        assert_eq!(s.points[1], s.rest_point(0, 1));
        for row in 1..ROWS {
            for side in 0..2 {
                let a = s.points[row * 2 + side];
                let b = s.points[(row - 1) * 2 + side];
                let length = isqrt_i32((0..3).map(|k| (a[k] - b[k]).pow(2)).sum());
                assert!(
                    length < 1024 * Q / 10,
                    "constraint stretched too far: {length}"
                );
            }
        }
        assert!(s.points.iter().all(|p| p[1] >= -992 * Q));
    }
    #[test]
    fn render_frequency_does_not_advance_physics_and_teleports_reset_velocity() {
        let mut s = PlayerScarf::new();
        step(&mut s, 0, 0, 0);
        step(&mut s, 1, 30, 0);
        let saved = s.points;
        let previous = s.previous;
        step(&mut s, 1, 30, 0);
        assert_eq!(s.points, saved);
        assert_eq!(s.previous, previous);
        step(&mut s, 2, 20000, -20000);
        assert_eq!(s.points, s.previous);
        assert_eq!(s.points[0], s.rest_point(0, 0));
        assert_eq!(s.points[1], s.rest_point(0, 1));
        step(&mut s, 1000, 20000, -20000);
        assert_eq!(s.points, s.previous);
    }
    #[test]
    fn turns_and_long_runs_remain_bounded_and_outside_torso() {
        let mut s = PlayerScarf::new();
        for t in 0..1200 {
            let yaw = Angle::from_q12((t * 31) as u16);
            let basis = Mat3I16 {
                m: [
                    [yaw.cos_q12() as i16, 0, yaw.sin_q12() as i16],
                    [0, 4096, 0],
                    [-(yaw.sin_q12() as i16), 0, yaw.cos_q12() as i16],
                ],
            };
            s.advance(
                t,
                1,
                WorldVertex::new(t as i32 * 30, 1000, 0),
                basis,
                1024,
                0,
                VideoHz::NTSC,
            );
            for p in &s.points[2..] {
                assert!(p.iter().all(|v| v.abs() < 1024 * Q * 2));
                let y = p[1].clamp(-1024 * Q / 3, -1024 * Q / 8);
                let d2 = p[0] * p[0] + (p[1] - y).pow(2) + p[2] * p[2];
                assert!(
                    d2 >= (1024 * Q / 9 - 8).pow(2),
                    "tick={t} point={p:?} length={}",
                    isqrt_i32(d2)
                );
            }
        }
    }
    #[test]
    fn pal_and_ntsc_solve_on_fixed_ticks_and_preserve_attachment() {
        for hz in [VideoHz::PAL, VideoHz::NTSC] {
            let mut s = PlayerScarf::new();
            for t in 0..600 {
                s.advance(
                    t,
                    1,
                    WorldVertex::new(0, 1000, 0),
                    Mat3I16::IDENTITY,
                    1024,
                    0,
                    hz,
                );
                assert_eq!(s.solve_tick, t & !1);
                assert_eq!(s.points[0], s.rest_point(0, 0));
                assert_eq!(s.points[1], s.rest_point(0, 1));
                assert!(s.points[14][1] < s.points[0][1]);
            }
        }
    }
    #[test]
    fn near_crossing_clips_to_two_triangles_and_capacity_is_bounded() {
        let camera = WorldCamera::orbit(
            WorldProjection::new(160, 120, 256, 32),
            WorldVertex::ZERO,
            1800,
            Angle::from_q12(0),
            Angle::from_q12(0),
        );
        let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 8192))
            .with_cull_mode(CullMode::None);
        let mut storage = OrderingTable::<64>::new();
        let mut ot = OtFrame::begin(&mut storage);
        let mut commands = [WorldTriCommand::EMPTY; 64];
        let mut slots = psx_engine::PrimitivePacketScratch::<2>::ZERO;
        let mut arena = PrimitivePacketArena::new(&mut slots);
        let mut world = WorldRenderPass::new_bucketed(&mut ot, &mut commands);
        let crossing = [
            ViewVertex::new(0, 0, 16),
            ViewVertex::new(-8, -8, 64),
            ViewVertex::new(8, -8, 64),
        ];
        assert_eq!(
            submit_clipped_cloth(
                crossing,
                camera,
                (255, 113, 58),
                options,
                &mut arena,
                &mut world
            ),
            2
        );
        assert_eq!(arena.used_slots(), 2);
        assert_eq!(
            submit_clipped_cloth(
                crossing,
                camera,
                (255, 113, 58),
                options,
                &mut arena,
                &mut world
            ),
            0
        );
        assert_eq!(arena.used_slots(), 2);
        assert_eq!(
            submit_clipped_cloth(
                [ViewVertex::new(0, 0, -10); 3],
                camera,
                (255, 113, 58),
                options,
                &mut arena,
                &mut world
            ),
            0
        );
        unsafe {
            arena.mutate_typed_slots::<TriGouraud>(0, 2, |triangle| {
                // GP0 command remains an opaque Gouraud triangle after clipping.
                assert_eq!(triangle.color0_cmd >> 24, 0x30);
            });
        }
    }
    #[test]
    fn settled_tip_points_down_after_stopping() {
        let mut s = PlayerScarf::new();
        for t in 0..300 {
            step(&mut s, t, (t.min(120) * 20) as i32, 0);
        }
        let centers: [Point; ROWS] = core::array::from_fn(|r| {
            core::array::from_fn(|a| (s.points[r * 2][a] + s.points[r * 2 + 1][a]) / 2)
        });
        // The model's visual origin can be around hip height (~700 here).
        // The tip must pass below it instead of landing on that invisible plane.
        assert!(
            1000 + centers[ROWS - 1][1] / Q < 650,
            "tip too high: {centers:?}"
        );
        let tip_drop = centers[ROWS - 2][1] - centers[ROWS - 1][1];
        assert!(tip_drop > 1024 * Q / 24, "tip must hang down: {centers:?}");
    }
    #[test]
    fn palette_hue_persists_without_fading_and_draw_is_read_only() {
        let mut s = PlayerScarf::new();
        for t in 0..180 {
            step(&mut s, t, 0, 0);
        }
        let before = s.points;
        let camera = WorldCamera::orbit(
            WorldProjection::new(160, 120, 256, 8),
            WorldVertex::new(0, 700, 0),
            1800,
            Angle::from_q12(0),
            Angle::from_q12(0),
        );
        let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 8192));
        for color in [(255, 113, 58), (108, 224, 198)] {
            let mut storage = OrderingTable::<64>::new();
            let mut ot = OtFrame::begin(&mut storage);
            let mut commands = [WorldTriCommand::EMPTY; 64];
            let mut slots = psx_engine::PrimitivePacketScratch::<64>::ZERO;
            let mut arena = PrimitivePacketArena::new(&mut slots);
            let mut world = WorldRenderPass::new_bucketed(&mut ot, &mut commands);
            assert_eq!(
                s.draw(camera, color, None, options, &mut arena, &mut world),
                30
            );
            let mut count = 0;
            let end = arena.used_slots();
            unsafe {
                arena.mutate_typed_slots::<TriGouraud>(0, end, |triangle| {
                    let rgb = triangle.color0_cmd;
                    if color.0 > color.1 {
                        assert!((rgb & 255) > ((rgb >> 8) & 255));
                    } else {
                        assert!(((rgb >> 8) & 255) > (rgb & 255));
                    }
                    count += 1;
                });
            }
            assert_eq!(count, 30);
            assert_eq!(s.points, before);
        }
    }
}
