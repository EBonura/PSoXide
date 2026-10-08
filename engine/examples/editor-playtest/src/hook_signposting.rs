//! Text-free traversal language: floating world crystal and selection brackets.
use super::*;
use psx_game_runtime::hook_points as hooks;
use psx_gte::lighting::ProjectedLit;
use psx_level::EntityKind;

impl Playtest {
    pub(super) fn draw_hook_beacons_world(
        &self,
        camera: WorldCamera,
        now: SimTick,
        packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) {
        let Some(room) = ROOMS.get(self.room_index.to_usize()) else {
            return;
        };
        // Small surface bias only: unlike HUD graphics, these must sort behind walls.
        let options = pxbsp_surface_options(room)
            .with_depth_bias(-2)
            .with_cull_mode(psx_engine::CullMode::None);
        for (_slot, (index, h)) in ENTITIES
            .iter()
            .enumerate()
            .filter(|(_, h)| h.kind == EntityKind::HookPoint)
            .take(hooks::MAX_HOOK_POINTS)
            .enumerate()
        {
            if h.room != self.room_index {
                continue;
            }
            let p = self.arch_anchor(index, now.as_u32());
            if (p.x - camera.position.x).saturating_abs() > hooks::MAX_RANGE * 2
                || (p.z - camera.position.z).saturating_abs() > hooks::MAX_RANGE * 2
            {
                continue;
            }
            if hook_beacon_off_screen(camera, p) {
                continue;
            }
            let selected = self.hook_selected == Some(index) && self.ranged_ready.aiming();
            let tint = if selected {
                (208, 255, 242)
            } else {
                (80, 206, 178)
            };
            let bright = if selected {
                (244, 255, 248)
            } else {
                (168, 238, 214)
            };

            let yaw = Angle::from_q12(h.yaw as u16);
            let center =
                WorldVertex::new(p.x - yaw.sin().mul_i32(2), p.y, p.z - yaw.cos().mul_i32(2));
            // A faceted octahedron core, with a camera-facing broken diamond
            // silhouette so the same symbol remains readable from any approach.
            let ring = [(8, 0), (0, 8), (-8, 0), (0, -8)]
                .map(|(x, z)| WorldVertex::new(center.x + x, center.y, center.z + z));
            // Six shared corners: every projection is the same as projecting
            // each triangle's corners separately, done once.
            let core = project_all(
                camera,
                &[
                    WorldVertex::new(center.x, center.y + 16, center.z),
                    WorldVertex::new(center.x, center.y - 16, center.z),
                    ring[0],
                    ring[1],
                    ring[2],
                    ring[3],
                ],
            );
            for i in 0..4 {
                submit_projected(
                    [core[0], core[2 + i], core[2 + (i + 1) % 4]],
                    bright,
                    options,
                    packets,
                    world,
                );
                submit_projected(
                    [core[1], core[2 + (i + 1) % 4], core[2 + i]],
                    tint,
                    options,
                    packets,
                    world,
                );
            }
            // One grounded-in-place contact slab and an incomplete crown of
            // independent broken stones. Nothing closes the far side of the arch.
            let point = |x: i32, y: i32, depth: i32| {
                let z = y * 3 / 10 + depth;
                WorldVertex::new(
                    p.x + yaw.cos().mul_i32(x) + yaw.sin().mul_i32(z),
                    p.y + y,
                    p.z - yaw.sin().mul_i32(x) + yaw.cos().mul_i32(z),
                )
            };
            let tick = now.as_u32().wrapping_sub(self.gameplay_epoch.as_u32());
            // Lift the loose crown clear of the contact slab.
            const FRAGMENT_LIFT: i32 = 48;
            // Convex, chipped profiles; varying thickness and bevels expose
            // bright fracture faces against the darker old stone.
            const STONES: [&[(i32, i32)]; 4] = [
                &[
                    (-25, -81),
                    (27, -86),
                    (32, 7),
                    (12, 40),
                    (-20, 29),
                    (-31, -41),
                ],
                &[
                    (-27, 64),
                    (0, 54),
                    (34, 90),
                    (16, 114),
                    (-7, 107),
                    (-30, 86),
                ],
                &[(45, 130), (54, 108), (99, 102), (112, 127), (87, 145)],
                &[(143, 112), (165, 99), (185, 110), (171, 141), (152, 145)],
            ];
            for (stone, profile) in STONES.iter().enumerate() {
                let n = profile.len();
                let cx = profile.iter().map(|p| p.0).sum::<i32>() / n as i32;
                let cy = profile.iter().map(|p| p.1).sum::<i32>() / n as i32;
                let phase = (((tick % 1440) * 4096 / 1440) as u16).wrapping_add(stone as u16 * 977);
                let wave = psx_math::sin_q12(phase);
                let moving = stone != 0;
                let angle = if moving { (wave * 48 / 4096) as u16 } else { 0 };
                let (sin, cos) = (psx_math::sin_q12(angle), psx_math::cos_q12(angle));
                let dx = if moving {
                    psx_math::cos_q12(phase) * 5 / 4096
                } else {
                    0
                };
                let dy = if moving {
                    FRAGMENT_LIFT + wave * (3 + stone as i32) / 4096
                } else {
                    0
                };
                let depth = if moving {
                    stone as i32 * 4 + wave * 3 / 4096
                } else {
                    0
                };
                let thickness = if stone == 0 {
                    27
                } else {
                    24 - stone as i32 * 3
                };
                let mut v = [WorldVertex::new(0, 0, 0); 18];
                for layer in 0..3 {
                    for (i, &(x, y)) in profile.iter().enumerate() {
                        let (x, y) = if layer == 0 {
                            (x + (cx - x) / 12, y + (cy - y) / 12)
                        } else {
                            (x, y)
                        };
                        let x0 = x - cx;
                        let y0 = y - cy;
                        v[layer * n + i] = point(
                            cx + (x0 * cos - y0 * sin) / 4096 + dx,
                            cy + (x0 * sin + y0 * cos) / 4096 + dy,
                            depth
                                + match layer {
                                    0 => 0,
                                    1 => 4,
                                    _ => thickness,
                                },
                        );
                    }
                }
                let front = (
                    95 + stone as u8 * 5,
                    109 + stone as u8 * 4,
                    108 + stone as u8 * 4,
                );
                // Up to eighteen corners shared by every face of the stone.
                let pv = project_prefix(camera, &v, 3 * n);
                for i in 1..n - 1 {
                    submit_projected([pv[0], pv[i], pv[i + 1]], front, options, packets, world);
                    submit_projected(
                        [pv[2 * n], pv[2 * n + i + 1], pv[2 * n + i]],
                        (53, 65, 66),
                        options,
                        packets,
                        world,
                    );
                }
                for i in 0..n {
                    let j = (i + 1) % n;
                    let bright = if profile[j].1 > profile[i].1 {
                        (148, 158, 149)
                    } else {
                        (118, 133, 128)
                    };
                    for (a, b, color) in [(0, n, bright), (n, 2 * n, (64, 78, 78))] {
                        for ids in [[a + i, a + j, b + j], [a + i, b + j, b + i]] {
                            submit_projected(ids.map(|k| pv[k]), color, options, packets, world);
                        }
                    }
                }
            }
            // Small isolated splinters float above the missing crown.
            for (i, (x, y, r)) in [(30, 142, 5), (125, 161, 7), (204, 137, 4)]
                .into_iter()
                .enumerate()
            {
                let phase = (((tick % 1800) * 4096 / 1800) as u16).wrapping_add(i as u16 * 1313);
                let x = x + psx_math::cos_q12(phase) * 4 / 4096;
                let y = y + FRAGMENT_LIFT + psx_math::sin_q12(phase) * 6 / 4096;
                let v = [
                    point(x - r, y - r, 13),
                    point(x + r, y, 17),
                    point(x, y + r * 2, 15),
                    point(x, y, 27),
                ];
                let pv = project_all(camera, &v);
                for (ids, color) in [
                    ([0, 1, 2], (139, 153, 145)),
                    ([0, 3, 1], (69, 85, 83)),
                    ([1, 3, 2], (93, 109, 104)),
                    ([2, 3, 0], (57, 70, 70)),
                ] {
                    submit_projected(ids.map(|k| pv[k]), color, options, packets, world);
                }
            }
            // Three small contact glints replace the former overhead tether.
            if self.hook_attached == Some(index) {
                for (x, y) in [(0, 0), (4, -58), (18, -66)] {
                    let r = if self.hook_charge.ready() { 3 } else { 2 };
                    let color = if self.hook_charge.ready() {
                        (255, 245, 210)
                    } else {
                        (108, 238, 216)
                    };
                    hook_tri(
                        [
                            point(x - r, y, -2),
                            point(x + r, y, -2),
                            point(x, y + r * 2, -2),
                        ],
                        color,
                        camera,
                        options,
                        packets,
                        world,
                    );
                }
            }
        }
    }

    /// Selection is the only overlay. Visibility and body clearance were
    /// proven by refresh_hook_target; unavailable points never get brackets.
    pub(super) fn draw_hook_selection(&self, gpu: &mut psx_gpu::Gpu, camera: WorldCamera) {
        if !self.ranged_ready.aiming() {
            return;
        }
        let Some(index) = self.hook_selected else {
            return;
        };
        let p = self.arch_anchor(
            index,
            self.overlay_sim_tick
                .as_u32()
                .wrapping_add(self.gameplay_epoch.as_u32()),
        );
        let Some(center) = camera.project_world(p) else {
            return;
        };
        let radius = (camera.projection.focal_length * 42 / center.sz.max(1)).clamp(10, 30) as i16;
        for (x, y) in [(-1, -1), (1, -1), (1, 1), (-1, 1)] {
            let corner = (center.sx + x * radius, center.sy + y * radius);
            let color = (232, 255, 244);
            gpu.set_draw_mode(psx_gpu::material::TextureMaterial::blended(
                0,
                0,
                color,
                BlendMode::Average,
            ));
            for end in [
                (corner.0 - x * 5, corner.1),
                (corner.0, corner.1 - y * 5),
            ] {
                gpu.draw(
                    &psx_gpu::prim::LineMono::new(
                        corner.0, corner.1, end.0, end.1, color.0, color.1, color.2,
                    )
                    .translucent(),
                );
            }
        }
    }
}

fn hook_tri(
    points: [WorldVertex; 3],
    color: (u8, u8, u8),
    camera: WorldCamera,
    options: WorldSurfaceOptions,
    packets: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    submit_projected(
        points.map(|p| camera.project_world(p)),
        color,
        options,
        packets,
        world,
    );
}

/// Project a vertex list once so faces that share corners reuse the result.
fn project_all<const N: usize>(
    camera: WorldCamera,
    points: &[WorldVertex; N],
) -> [Option<psx_engine::ProjectedVertex>; N] {
    core::array::from_fn(|i| camera.project_world(points[i]))
}

/// [`project_all`] for the first `used` corners; the rest stay unprojected.
fn project_prefix<const N: usize>(
    camera: WorldCamera,
    points: &[WorldVertex; N],
    used: usize,
) -> [Option<psx_engine::ProjectedVertex>; N] {
    core::array::from_fn(|i| {
        if i < used {
            camera.project_world(points[i])
        } else {
            None
        }
    })
}

fn submit_projected(
    points: [Option<psx_engine::ProjectedVertex>; 3],
    color: (u8, u8, u8),
    options: WorldSurfaceOptions,
    packets: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let [Some(a), Some(b), Some(c)] = points else {
        return;
    };
    if !psx_engine::projected_triangle_batchable([a, b, c]) {
        return;
    }
    let lit = [a, b, c].map(|p| ProjectedLit {
        sx: p.sx,
        sy: p.sy,
        sz: p.sz.clamp(1, 65535) as u16,
        r: color.0,
        g: color.1,
        b: color.2,
    });
    let _ = world.submit_gouraud_triangle(packets, lit, options);
}

/// Every vertex of a beacon lies within this distance of its anchor: the
/// stones reach about 330 world units out, and the bound is rounded up.
const BEACON_RADIUS: i32 = 360;
/// Pixels past the screen edge a beacon may sit and still be kept.
const BEACON_SCREEN_MARGIN: i32 = 64;

/// Conservative test that no beacon triangle can reach the screen: the whole
/// bounding sphere is behind the near plane or beyond one screen edge. A
/// triangle with a corner behind the near plane is already dropped, and one
/// wholly off screen draws nothing, so skipping the beacon changes no pixel.
fn hook_beacon_off_screen(camera: WorldCamera, anchor: RoomPoint) -> bool {
    let view = camera.view_vertex(WorldVertex::new(anchor.x, anchor.y, anchor.z));
    let projection = camera.projection;
    // View-space rounding of each corner is below one unit per axis.
    let radius = BEACON_RADIUS + 4;
    let far = view.z + radius;
    if far < projection.near_z.max(1) {
        return true;
    }
    let f = projection.focal_length;
    let cx = i32::from(projection.screen_x);
    let cy = i32::from(projection.screen_y);
    let m = BEACON_SCREEN_MARGIN;
    // A point at view (x, z) lands at cx + x * f / z. Every point of the
    // sphere has z <= far, so a sphere whose smallest x exceeds the edge
    // distance times far / f is wholly past that edge.
    (view.x - radius) * f > (320 + m - cx) * far
        || (-view.x - radius) * f > (cx + m) * far
        || (view.y - radius) * f > (cy + m) * far
        || (-view.y - radius) * f > (240 + m - cy) * far
}
