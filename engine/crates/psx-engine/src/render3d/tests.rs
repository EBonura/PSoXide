use super::*;
use psx_gpu::material::BlendMode;
use psx_gpu::ot::OrderingTable;

const fn command(slot: usize, depth: i32, primitive_index: usize) -> GouraudTriCommand {
    GouraudTriCommand {
        slot: DepthSlot::new(slot),
        depth,
        primitive_index,
        next: GOURAUD_COMMAND_NONE,
    }
}

const fn world_command(slot: usize, depth: i32, order: usize) -> WorldTriCommand {
    world_command_layer(slot, depth, WorldRenderLayer::Opaque, order)
}

const fn world_command_layer(
    slot: usize,
    depth: i32,
    render_layer: WorldRenderLayer,
    order: usize,
) -> WorldTriCommand {
    WorldTriCommand {
        packet_ptr: core::ptr::null_mut(),
        depth,
        slot: slot as u16,
        order: order as u16,
        next: WORLD_COMMAND_NONE,
        render_layer: world_render_layer_code(render_layer),
        words: 0,
    }
}

#[test]
fn projected_vertex_invalid_sentinel_is_not_renderable() {
    assert!(!ProjectedVertex::INVALID.is_valid());
    assert!(!ProjectedVertex::default().is_valid());
    assert!(!ProjectedVertex::new(0, 0, -1).is_valid());
    assert!(ProjectedVertex::new(0, 0, 16).is_valid());
}

#[test]
fn bucketed_world_pass_keeps_commands_for_reverse_flush() {
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut commands = [WorldTriCommand::EMPTY; 3];
    let mut pass = WorldRenderPass::new_bucketed(&mut ot, &mut commands);

    pass.push_command(
        DepthSlot::new(4),
        100,
        WorldRenderLayer::Opaque,
        core::ptr::null_mut(),
        0,
    );
    pass.push_command(
        DepthSlot::new(4),
        100,
        WorldRenderLayer::Opaque,
        core::ptr::null_mut(),
        0,
    );
    pass.push_command(
        DepthSlot::new(4),
        100,
        WorldRenderLayer::Opaque,
        core::ptr::null_mut(),
        0,
    );

    assert_eq!(pass.command_len, 3);
    assert!(!pass.ordering.uses_slot_heads());
    assert!(!pass.ordering.uses_slot_tails());
    let compact = pass.commands.as_ptr().cast::<BucketedWorldCommand>();
    for index in 0..3 {
        // SAFETY: the three push_command calls above initialised these entries.
        let command = unsafe { *compact.add(index) };
        assert_eq!(command.slot(), 4);
        assert_eq!(command.words(), 0);
    }
}

#[test]
fn deferred_world_pass_does_not_use_slot_links() {
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut commands = [WorldTriCommand::EMPTY; 1];
    let pass = WorldRenderPass::new_deferred_sorted(&mut ot, &mut commands);

    assert!(!pass.ordering.uses_slot_heads());
    assert!(!pass.ordering.uses_slot_tails());
}

#[test]
fn slot_sorted_world_pass_initializes_slot_links() {
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut commands = [WorldTriCommand::EMPTY; 1];
    let pass = WorldRenderPass::new_deferred_slot_sorted(&mut ot, &mut commands);

    assert!(pass.ordering.uses_slot_heads());
    assert!(pass.ordering.uses_slot_tails());
    assert_eq!(pass.slot_heads()[4], WORLD_COMMAND_NONE);
    assert_eq!(pass.slot_tails()[4], WORLD_COMMAND_NONE);
}

/// The canonical quad split must always share the `0`–`2`
/// diagonal. The pre-fix bug used `(0,1,2)` + `(2,1,3)`,
/// which puts the second triangle on the OTHER diagonal and
/// leaves a triangular hole near corner `3`. This test fails
/// loudly if anyone reintroduces that pattern.
#[test]
fn textured_quad_triangles_share_zero_two_diagonal() {
    assert_eq!(TEXTURED_QUAD_TRIANGLES[0], [0, 1, 2]);
    assert_eq!(TEXTURED_QUAD_TRIANGLES[1], [0, 2, 3]);
    // Both triangles must contain the diagonal endpoints.
    for tri in TEXTURED_QUAD_TRIANGLES {
        assert!(tri.contains(&0), "{tri:?} missing corner 0");
        assert!(tri.contains(&2), "{tri:?} missing corner 2");
    }
    // All four corners must appear at least once across the
    // two triangles -- otherwise some part of the quad is
    // never drawn.
    for corner in 0..4 {
        assert!(
            TEXTURED_QUAD_TRIANGLES
                .iter()
                .any(|tri| tri.contains(&corner)),
            "corner {corner} not covered"
        );
    }
}

/// For a convex unit square laid out as
///
/// ```text
///   (0,0) ─── (1,0)        0 ─ 1
///     │         │          │   │
///   (0,1) ─── (1,1)        3 ─ 2
/// ```
///
/// both generated triangles must have the same signed-area
/// orientation. If they don't, one of them is flipped and a
/// `CullMode::Back` pass would reject one half -- which is
/// exactly how the old buggy split looked: half the quad
/// rendered, half disappeared.
#[test]
fn textured_quad_split_produces_consistent_winding() {
    // Screen-space corners as if the renderer just projected
    // a unit-aligned floor quad. Y grows downward in PSX
    // screen space, but the sign of the cross product is
    // what we're checking -- the absolute orientation
    // doesn't matter.
    let v: [(i32, i32); 4] = [(0, 0), (10, 0), (10, 10), (0, 10)];
    let signed_area = |a: (i32, i32), b: (i32, i32), c: (i32, i32)| -> i32 {
        let abx = b.0 - a.0;
        let aby = b.1 - a.1;
        let acx = c.0 - a.0;
        let acy = c.1 - a.1;
        abx * acy - aby * acx
    };
    let [a0, b0, c0] = TEXTURED_QUAD_TRIANGLES[0];
    let [a1, b1, c1] = TEXTURED_QUAD_TRIANGLES[1];
    let area0 = signed_area(v[a0], v[b0], v[c0]);
    let area1 = signed_area(v[a1], v[b1], v[c1]);
    assert!(area0 != 0, "first triangle is degenerate");
    assert!(area1 != 0, "second triangle is degenerate");
    assert_eq!(
        area0.signum(),
        area1.signum(),
        "split halves must wind the same way (got {area0} vs {area1})"
    );
}

/// The two halves must tile the quad without overlap. Picking
/// a point that lies strictly above the `0`–`2` diagonal should
/// land in exactly one triangle; the same goes for a point
/// below. Reproduces the old bug: under `(0,1,2)+(2,1,3)`, a
/// point in the lower-left quadrant (near corner `3`) lies in
/// neither half.
#[test]
fn textured_quad_split_tiles_the_quad_without_holes() {
    let v: [(i32, i32); 4] = [(0, 0), (10, 0), (10, 10), (0, 10)];
    // Inside-triangle test using barycentric sign check.
    let in_triangle = |t: [usize; 3], p: (i32, i32)| -> bool {
        let a = v[t[0]];
        let b = v[t[1]];
        let c = v[t[2]];
        let s1 = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let s2 = (c.0 - b.0) * (p.1 - b.1) - (c.1 - b.1) * (p.0 - b.0);
        let s3 = (a.0 - c.0) * (p.1 - c.1) - (a.1 - c.1) * (p.0 - c.0);
        (s1 >= 0 && s2 >= 0 && s3 >= 0) || (s1 <= 0 && s2 <= 0 && s3 <= 0)
    };
    // Probes carefully chosen so they're strictly *inside* one
    // half or the other, never on the diagonal. The point
    // (2, 7) is the killer probe: under the OLD `(2,1,3)`
    // second triangle it falls into the hole (y > x AND
    // x+y < 10), so the assertion below would have caught
    // the bug at unit-test time.
    for &p in &[(2, 1), (8, 2), (8, 8), (2, 7)] {
        let covered = in_triangle(TEXTURED_QUAD_TRIANGLES[0], p)
            || in_triangle(TEXTURED_QUAD_TRIANGLES[1], p);
        assert!(covered, "point {p:?} fell into the split's hole");
    }
}

#[test]
fn model_uv_limit_clamps_to_declared_atlas_extent() {
    assert_eq!(clamp_model_uv(64, 32, 127, 63), (64, 32));
    assert_eq!(clamp_model_uv(153, 80, 127, 63), (127, 63));
    assert_eq!(clamp_model_uv(240, 250, 255, 255), (240, 250));
    assert_eq!(clamp_model_uv(300, 260, 255, 255), (255, 255));
    assert_eq!(clamp_model_uv(-4, -9, 127, 63), (0, 0));
}

#[test]
fn model_uv_limit_allows_full_8bpp_page() {
    assert_eq!(model_uv_max(0), 0);
    assert_eq!(model_uv_max(64), 63);
    assert_eq!(model_uv_max(128), 127);
    assert_eq!(model_uv_max(256), 255);
    assert_eq!(model_uv_max(512), 255);
}

#[test]
fn projected_model_bounds_hw_extent_safe_obeys_ps1_triangle_limits() {
    assert!(projected_model_bounds_hw_extent_safe(0, 1023, 0, 511));
    assert!(!projected_model_bounds_hw_extent_safe(0, 1024, 0, 511));
    assert!(!projected_model_bounds_hw_extent_safe(0, 1023, 0, 512));
    assert!(!projected_model_bounds_hw_extent_safe(10, 9, 0, 0));
}

#[test]
fn model_face_packed_uv_words_match_packet_texcoords() {
    let material = TextureMaterial::opaque(0x1234, 0x0160, (96, 128, 160));
    let verts = [(12, 34), (56, 78), (90, 123)];
    let uvs = [(3, 5), (17, 7), (11, 29)];
    let face = TexturedModelRenderFace::new([0, 1, 2], uvs);

    let tuple_packet = TriTextured::with_material_packet_texcoords(verts, uvs, material);
    let packed_packet =
        TriTextured::with_material_packed_uv_words(verts, face.uv_words(), material);

    assert_eq!(core::mem::size_of::<TexturedModelRenderFace>(), 12);
    assert_eq!(core::mem::align_of::<TexturedModelRenderFace>(), 4);
    assert_eq!(face.corner_words, [0x0503_0000, 0x0711_0001, 0x1d0b_0002]);
    assert_eq!(face.vertex_indices(), [0, 1, 2]);
    assert_eq!(face.uvs(), uvs);
    assert_eq!(tuple_packet.tex_window, packed_packet.tex_window);
    assert_eq!(tuple_packet.color_cmd, packed_packet.color_cmd);
    assert_eq!(tuple_packet.v0, packed_packet.v0);
    assert_eq!(tuple_packet.uv0_clut, packed_packet.uv0_clut);
    assert_eq!(tuple_packet.v1, packed_packet.v1);
    assert_eq!(tuple_packet.uv1_tpage, packed_packet.uv1_tpage);
    assert_eq!(tuple_packet.v2, packed_packet.v2);
    assert_eq!(tuple_packet.uv2, packed_packet.uv2);
}

#[test]
fn model_face_palette_bank_uses_spare_vertex_bits_without_growing() {
    let face = TexturedModelRenderFace::new_with_palette_bank(
        [451, 17, 32],
        [(3, 5), (17, 7), (11, 29)],
        3,
    );
    assert_eq!(core::mem::size_of::<TexturedModelRenderFace>(), 12);
    assert_eq!(face.vertex_indices(), [451, 17, 32]);
    assert_eq!(face.palette_bank(), 3);
    assert_eq!(face.corner_words[0] as u16, 0xc1c3);

    let material = TextureMaterial::opaque(0x0120, 0x0160, (128, 128, 128));
    assert_eq!(material.with_clut_bank(3).clut_word(), 0x0123);
    assert_eq!(
        material
            .textured_packet_material()
            .with_clut_bank(3)
            .clut_high_word,
        0x0123_0000
    );
}

#[test]
fn model_face_uv_offset_wraps_each_byte_independently() {
    let face = TexturedModelRenderFace::new([0, 1, 2], [(250, 1), (3, 254), (128, 64)]);
    let moved = face.with_uv_offset(ModelUvOffset::new(10, 4));

    assert_eq!(moved.vertex_indices(), face.vertex_indices());
    assert_eq!(moved.uvs(), [(4, 5), (13, 2), (138, 68)]);
    assert_eq!(face.uvs(), [(250, 1), (3, 254), (128, 64)]);
}

#[test]
fn model_no_cull_unclamped_batch_keeps_both_windings() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let projected = [
        ProjectedVertex::new(10, 10, 100),
        ProjectedVertex::new(30, 10, 120),
        ProjectedVertex::new(10, 30, 140),
    ];
    let faces = [
        TexturedModelRenderFace::new([0, 1, 2], [(0, 0), (15, 0), (0, 15)]),
        TexturedModelRenderFace::new([0, 2, 1], [(0, 0), (0, 15), (15, 0)]),
    ];
    let material = TextureMaterial::blended(0, 0, (128, 128, 128), BlendMode::Average);
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true);

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 2];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 2];
    let mut stats = TexturedModelRenderStats::default();
    let mut faces_considered = 0;
    let uv_offset = ModelUvOffset::new(10, 4);
    let overflow = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
        pass.submit_predecoded_model_faces_packed_average_unclamped_extent_safe_batch::<false>(
            &mut triangles,
            &projected,
            &faces,
            material.textured_packet_material(),
            None,
            uv_offset,
            false,
            options,
            &mut stats,
            &mut faces_considered,
        )
    };

    assert!(!overflow);
    assert_eq!(faces_considered, 2);
    assert_eq!(stats.packed_face_calls, 2);
    assert_eq!(stats.packed_unclamped_face_calls, 2);
    assert_eq!(stats.culled_triangles, 0);
    assert_eq!(stats.submitted_triangles, 2);
    assert_eq!(stats.fast_submitted_triangles, 2);
    assert_eq!(
        triangle_storage[0].uv0_clut as u16,
        faces[0].with_uv_offset(uv_offset).uv_words()[0]
    );
    assert_eq!(
        triangle_storage[1].uv1_tpage as u16,
        faces[1].with_uv_offset(uv_offset).uv_words()[1]
    );
}

#[test]
fn layered_bucketed_model_batch_culls_once_and_keeps_material_passes_contiguous() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let projected = [
        ProjectedVertex::new(10, 10, 100),
        ProjectedVertex::new(30, 10, 120),
        ProjectedVertex::new(10, 30, 140),
    ];
    let faces = [
        TexturedModelRenderFace::new([0, 1, 2], [(0, 0), (15, 0), (0, 15)]),
        TexturedModelRenderFace::new([0, 2, 1], [(0, 0), (0, 15), (15, 0)]),
        TexturedModelRenderFace::new([0, 1, 2], [(16, 0), (31, 0), (16, 15)]),
    ];
    let base_material = TextureMaterial::opaque(1, 2, (128, 128, 128));
    let secondary_material = TextureMaterial::blended(3, 4, (96, 112, 128), BlendMode::AddQuarter);
    let base_options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000));
    let secondary_options = base_options.with_material_layer(secondary_material);

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 6];
    let triangle_start = triangle_storage.as_ptr();
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 6];
    let mut stats = TexturedModelRenderStats::default();
    let mut faces_considered = 0;
    {
        let mut pass = WorldRenderPass::new_bucketed(&mut ot, &mut commands);
        pass.submit_predecoded_model_faces_layered_bucketed_average_unclamped_extent_safe_batch::<
            true,
        >(
            &mut triangles,
            &projected,
            &faces,
            base_material.textured_packet_material(),
            None,
            secondary_material.textured_packet_material(),
            ModelUvOffset::new(5, 7),
            base_options,
            secondary_options,
            &mut stats,
            &mut faces_considered,
        );

        assert_eq!(pass.command_len(), 4);
        let compact = pass.commands.as_ptr().cast::<BucketedWorldCommand>();
        let command_ptr = |index| unsafe { (*compact.add(index)).packet_ptr };
        // Packets are allocated face-by-face, but commands must preserve the
        // old all-base-then-all-secondary material-pass order.
        assert_eq!(command_ptr(0), triangle_start.cast_mut().cast::<u32>());
        assert_eq!(command_ptr(1), unsafe {
            triangle_start.add(2).cast_mut().cast::<u32>()
        });
        assert_eq!(command_ptr(2), unsafe {
            triangle_start.add(1).cast_mut().cast::<u32>()
        });
        assert_eq!(command_ptr(3), unsafe {
            triangle_start.add(3).cast_mut().cast::<u32>()
        });
    }

    assert_eq!(faces_considered, 6);
    assert_eq!(stats.packed_face_calls, 6);
    assert_eq!(stats.packed_unclamped_face_calls, 6);
    assert_eq!(stats.culled_triangles, 2);
    assert_eq!(stats.submitted_triangles, 4);
    assert_eq!(stats.fast_submitted_triangles, 4);
    assert_eq!(triangle_storage[0].uv0_clut as u16, faces[0].uv_words()[0]);
    assert_eq!(
        triangle_storage[1].uv0_clut as u16,
        faces[0].with_uv_offset(ModelUvOffset::new(5, 7)).uv_words()[0]
    );
    assert!(!stats.primitive_overflow);
    assert!(!stats.command_overflow);
}

#[test]
fn gouraud_packed_uv_words_match_packet_texcoords() {
    let material = TextureMaterial::opaque(0x1234, 0x0160, (96, 128, 160));
    let verts = [(12, 34), (56, 78), (90, 123)];
    let uvs = [(3, 5), (17, 7), (11, 29)];
    let uv_words = [
        (uvs[0].0 as u16) | ((uvs[0].1 as u16) << 8),
        (uvs[1].0 as u16) | ((uvs[1].1 as u16) << 8),
        (uvs[2].0 as u16) | ((uvs[2].1 as u16) << 8),
    ];
    let colors = [(7, 13, 19), (23, 29, 31), (37, 41, 43)];

    let tuple_packet =
        TriTexturedGouraud::with_material_packet_texcoords(verts, uvs, colors, material);
    let packed_packet =
        TriTexturedGouraud::with_material_packed_uv_words(verts, uv_words, colors, material);
    let prepacked_packet = TriTexturedGouraud::with_packet_material_packed_uv_words(
        verts,
        uv_words,
        colors,
        material.textured_gouraud_packet_material(),
    );

    assert_eq!(packet_uv_words_to_pairs(uv_words), uvs);
    assert_eq!(tuple_packet.tex_window, packed_packet.tex_window);
    assert_eq!(tuple_packet.color0_cmd, packed_packet.color0_cmd);
    assert_eq!(tuple_packet.v0, packed_packet.v0);
    assert_eq!(tuple_packet.uv0_clut, packed_packet.uv0_clut);
    assert_eq!(tuple_packet.color1, packed_packet.color1);
    assert_eq!(tuple_packet.v1, packed_packet.v1);
    assert_eq!(tuple_packet.uv1_tpage, packed_packet.uv1_tpage);
    assert_eq!(tuple_packet.color2, packed_packet.color2);
    assert_eq!(tuple_packet.v2, packed_packet.v2);
    assert_eq!(tuple_packet.uv2, packed_packet.uv2);
    assert_eq!(packed_packet.tex_window, prepacked_packet.tex_window);
    assert_eq!(packed_packet.color0_cmd, prepacked_packet.color0_cmd);
    assert_eq!(packed_packet.v0, prepacked_packet.v0);
    assert_eq!(packed_packet.uv0_clut, prepacked_packet.uv0_clut);
    assert_eq!(packed_packet.color1, prepacked_packet.color1);
    assert_eq!(packed_packet.v1, prepacked_packet.v1);
    assert_eq!(packed_packet.uv1_tpage, prepacked_packet.uv1_tpage);
    assert_eq!(packed_packet.color2, prepacked_packet.color2);
    assert_eq!(packed_packet.v2, prepacked_packet.v2);
    assert_eq!(packed_packet.uv2, prepacked_packet.uv2);
}

#[test]
fn depth_policy_picks_expected_scalar() {
    let verts = [
        ProjectedLit {
            sx: 0,
            sy: 0,
            sz: 100,
            r: 0,
            g: 0,
            b: 0,
        },
        ProjectedLit {
            sx: 0,
            sy: 0,
            sz: 400,
            r: 0,
            g: 0,
            b: 0,
        },
        ProjectedLit {
            sx: 0,
            sy: 0,
            sz: 700,
            r: 0,
            g: 0,
            b: 0,
        },
    ];

    assert_eq!(DepthPolicy::Average.depth(verts), 400);
    assert_eq!(DepthPolicy::Nearest.depth(verts), 100);
    assert_eq!(DepthPolicy::Farthest.depth(verts), 700);
    assert_eq!(DepthPolicy::Fixed(250).depth(verts), 250);
}

#[test]
fn local_to_world_scale_applies_q12_without_i64() {
    let half = LocalToWorldScale::from_q12(0x0800);
    assert_eq!(half.apply(8192), 4096);
    assert_eq!(half.apply(-8192), -4096);
    assert_eq!(half.apply(4095), 2047);

    let identity = LocalToWorldScale::from_q12(0);
    assert_eq!(identity.q12(), 0x1000);
    assert_eq!(identity.apply(-12345), -12345);
}

#[test]
fn joint_world_transform_stops_before_camera_view() {
    let pose = JointPose {
        matrix: Mat3I16::IDENTITY.m,
        translation: Vec3I32::new(256, 128, -64),
    };
    let origin = WorldVertex::new(1000, 2000, 3000);
    let joint =
        compute_joint_world_transform(pose, Mat3I16::IDENTITY, LocalToWorldScale::IDENTITY, origin);

    assert_eq!(joint.rotation, Mat3I16::IDENTITY);
    assert_eq!(joint.translation, WorldVertex::new(1256, 2128, 2936));
}

#[test]
fn model_pose_translation_offsets_joint_pose_without_rotating_it() {
    let pose = JointPose {
        matrix: Mat3I16::IDENTITY.m,
        translation: Vec3I32::new(10, -20, 30),
    };

    let adjusted = apply_model_pose_translation(pose, ModelPoseTranslation { x: -3, y: 5, z: 7 });

    assert_eq!(adjusted.matrix, pose.matrix);
    assert_eq!(adjusted.translation, Vec3I32::new(7, -15, 37));
}

#[test]
fn commands_sort_in_ot_insertion_order() {
    let mut commands = [
        command(5, 600, 0),
        command(5, 300, 1),
        command(3, 400, 2),
        command(5, 300, 3),
    ];

    sort_for_ot_insert(&mut commands);

    assert_eq!(commands[0], command(3, 400, 2));
    assert_eq!(commands[1], command(5, 300, 3));
    assert_eq!(commands[2], command(5, 300, 1));
    assert_eq!(commands[3], command(5, 600, 0));
}

#[test]
fn projected_backface_culling_uses_screen_winding() {
    let front = [
        ProjectedVertex::new(0, 0, 100),
        ProjectedVertex::new(10, 0, 100),
        ProjectedVertex::new(0, 10, 100),
    ];
    let back = [front[0], front[2], front[1]];

    assert!(!projected_back_facing(front));
    assert!(projected_back_facing(back));
}

#[test]
fn textured_near_clip_keeps_visible_polygon() {
    let input = [
        TexturedViewVertex::new(ViewVertex::new(-20, 0, 20), 0, 0),
        TexturedViewVertex::new(ViewVertex::new(20, 0, 80), 63, 0),
        TexturedViewVertex::new(ViewVertex::new(-20, 40, 80), 0, 63),
    ];
    let mut out = [TexturedViewVertex::ZERO; 4];

    let count = clip_textured_triangle_to_near(input, 40, &mut out);

    assert_eq!(count, 4);
    assert_eq!(out[0].position.z, 40);
    assert_eq!(out[1].position.z, 40);
    assert!(out[..count].iter().all(|v| v.position.z >= 40));
}

#[test]
fn gouraud_room_triangle_clips_at_near_plane_without_full_view_arena() {
    const ZERO: TriTexturedGouraud = TriTexturedGouraud::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0, 0), (0, 0, 0), (0, 0, 0)],
        0,
        0,
    );
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 8];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 8];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
    let projection = WorldProjection::new(160, 120, 200, 40);

    let stats = pass.submit_textured_gouraud_view_triangle_uv_words(
        &mut triangles,
        [
            ViewVertex::new(-20, 0, 20),
            ViewVertex::new(20, 0, 80),
            ViewVertex::new(-20, 40, 80),
        ],
        [0, 63, 63 << 8],
        [(64, 96, 128), (128, 160, 192), (192, 224, 255)],
        projection,
        TextureMaterial::opaque(0, 0, (128, 128, 128)),
        WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
            .with_cull_mode(CullMode::None),
    );

    assert_eq!(stats.clipped_triangles, 1);
    assert_eq!(stats.dropped_triangles, 0);
    assert!(stats.submitted_triangles >= 2);
    assert!(pass.command_len() >= 2);
}

#[test]
fn textured_submit_splits_triangles_that_exceed_ps1_extent() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 8];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 8];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let stats = pass.submit_textured_triangle(
        &mut triangles,
        [
            ProjectedVertex::new(0, 0, 100),
            ProjectedVertex::new(0, 700, 100),
            ProjectedVertex::new(128, 0, 100),
        ],
        [(0, 0), (63, 0), (0, 63)],
        TextureMaterial::opaque(0, 0, (128, 128, 128)),
        WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
            .with_cull_mode(CullMode::None),
    );

    assert!(stats.submitted_triangles > 1);
    assert!(stats.split_triangles > 0);
    assert_eq!(stats.dropped_triangles, 0);
    assert!(!stats.primitive_overflow);
    assert!(!stats.command_overflow);
    pass.flush();
}

#[test]
fn world_camera_orbit_projects_target_to_screen_center() {
    let projection = WorldProjection::new(160, 120, 200, 40);
    let target = WorldVertex::new(0, -90, 0);

    let camera = WorldCamera::orbit_yaw(projection, target, 0, 120, Angle::ZERO);
    let projected = camera.project_world(target).expect("target in front");

    assert_eq!(projected.sx, 160);
    assert_eq!(projected.sy, 120);
    assert!(projected.sz >= projection.near_z);
}

#[test]
fn world_camera_projects_world_quad() {
    let projection = WorldProjection::new(160, 120, 200, 40);
    let camera = WorldCamera::orbit_yaw(projection, WorldVertex::ZERO, 0, 200, Angle::ZERO);

    let projected = camera.project_world_quad([
        WorldVertex::new(-10, 10, 0),
        WorldVertex::new(10, 10, 0),
        WorldVertex::new(-10, -10, 0),
        WorldVertex::new(10, -10, 0),
    ]);

    assert!(projected.is_some());
}

#[test]
fn loaded_world_camera_gte_projects_quad_like_cpu() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(0, 128, 0);
    let camera = WorldCamera::orbit_yaw(projection, target, 512, 1536, Angle::from_q12(170));
    let quad = [
        WorldVertex::new(-128, 320, 64),
        WorldVertex::new(128, 320, 64),
        WorldVertex::new(128, 64, 64),
        WorldVertex::new(-128, 64, 64),
    ];

    let cpu = camera.project_world_quad(quad).expect("quad in front");
    let gte = LoadedWorldCameraGte::load(camera)
        .project_world_quad(quad)
        .expect("quad in front");

    for i in 0..4 {
        assert!((gte[i].sx - cpu[i].sx).abs() <= 2);
        assert!((gte[i].sy - cpu[i].sy).abs() <= 2);
        assert!((gte[i].sz - cpu[i].sz).abs() <= 2);
    }
}

#[test]
fn contiguous_gte_projection_matches_ordered_index_projection() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(0, 128, 0);
    let camera = WorldCamera::orbit_yaw(projection, target, 512, 1536, Angle::from_q12(170));
    let vertices = [
        WorldVertex::new(-128, 320, 64),
        WorldVertex::new(128, 320, 64),
        WorldVertex::new(128, 64, 64),
        WorldVertex::new(-128, 64, 64),
        WorldVertex::new(0, 160, -32),
    ];
    let indices = [0, 1, 2, 3, 4];
    let mut indexed = [ProjectedVertex::INVALID; 5];
    let mut contiguous = [ProjectedVertex::INVALID; 5];

    project_world_vertex_indices_gte(camera, &vertices, &indices, &mut indexed);
    project_world_vertices_gte(camera, &vertices, &mut contiguous);

    assert_eq!(contiguous, indexed);
}

#[test]
fn textured_model_gte_transform_matches_world_camera_projection() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(0, 512, 0);
    let camera = WorldCamera::orbit_yaw(projection, target, 1120, 2048, Angle::from_q12(220));
    let pose = JointPose {
        matrix: [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]],
        translation: Vec3I32::new(20, -16, 32),
    };
    let origin = WorldVertex::new(0, 512, 0);
    let local = Vec3I16::new(64, 128, -32);

    let cpu_world = WorldVertex::new(
        origin.x + pose.translation.x + local.x as i32,
        origin.y + pose.translation.y + local.y as i32,
        origin.z + pose.translation.z + local.z as i32,
    );
    let cpu_view = camera.view_vertex(cpu_world);
    let cpu_projected = camera.project_world(cpu_world).expect("in front");

    let (rotation, translation) = textured_model_part_gte_transform(
        camera,
        pose,
        Mat3I16::IDENTITY,
        LocalToWorldScale::IDENTITY,
        origin,
    );
    let gte_x = translation.x + dot_q12_row_i16(rotation.m[0], local);
    let gte_y = translation.y + dot_q12_row_i16(rotation.m[1], local);
    let gte_z = translation.z + dot_q12_row_i16(rotation.m[2], local);

    assert_close_i32(gte_x, cpu_view.x, 4);
    assert_close_i32(gte_y, -cpu_view.y, 4);
    assert_close_i32(gte_z, cpu_view.z, 4);

    let gte_sx = projection.screen_x as i32 + (gte_x * projection.focal_length) / gte_z;
    let gte_sy = projection.screen_y as i32 + (gte_y * projection.focal_length) / gte_z;
    assert_close_i32(gte_sx, cpu_projected.sx as i32, 1);
    assert_close_i32(gte_sy, cpu_projected.sy as i32, 1);
}

#[test]
fn textured_model_gte_transform_applies_instance_rotation() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(0, 512, 0);
    let camera = WorldCamera::orbit_yaw(projection, target, 1120, 2048, Angle::from_q12(220));
    let pose = JointPose {
        matrix: Mat3I16::IDENTITY.m,
        translation: Vec3I32::new(0, 0, 0),
    };
    let origin = WorldVertex::new(0, 512, 0);
    let local = Vec3I16::new(128, 0, 0);
    let quarter_yaw = Mat3I16 {
        m: [[0, 0, 0x1000], [0, 0x1000, 0], [-0x1000, 0, 0]],
    };

    let cpu_world = WorldVertex::new(origin.x, origin.y, origin.z - local.x as i32);
    let cpu_view = camera.view_vertex(cpu_world);
    let cpu_projected = camera.project_world(cpu_world).expect("in front");

    let (rotation, translation) = textured_model_part_gte_transform(
        camera,
        pose,
        quarter_yaw,
        LocalToWorldScale::IDENTITY,
        origin,
    );
    let gte_x = translation.x + dot_q12_row_i16(rotation.m[0], local);
    let gte_y = translation.y + dot_q12_row_i16(rotation.m[1], local);
    let gte_z = translation.z + dot_q12_row_i16(rotation.m[2], local);

    assert_close_i32(gte_x, cpu_view.x, 4);
    assert_close_i32(gte_y, -cpu_view.y, 4);
    assert_close_i32(gte_z, cpu_view.z, 4);

    let gte_sx = projection.screen_x as i32 + (gte_x * projection.focal_length) / gte_z;
    let gte_sy = projection.screen_y as i32 + (gte_y * projection.focal_length) / gte_z;
    assert_close_i32(gte_sx, cpu_projected.sx as i32, 1);
    assert_close_i32(gte_sy, cpu_projected.sy as i32, 1);
}

#[test]
fn gte_joint_compose_matches_cpu_joint_rotation_path() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(80, 256, -48);
    let camera = WorldCamera::orbit_yaw(projection, target, 960, 1536, Angle::from_q12(337));
    let view = camera_gte_view_matrix(camera);
    let instance_rotation = Mat3I16::rotate_y(704).mul(&Mat3I16::rotate_x(192));
    let view_instance = mat3_mul_q12(&view, &instance_rotation);
    let pose = JointPose {
        matrix: Mat3I16::rotate_z(384).mul(&Mat3I16::rotate_y(256)).m,
        translation: Vec3I32::new(320, -128, 512),
    };
    let origin = WorldVertex::new(384, 768, -256);

    let (cpu_rotation, cpu_translation) = textured_model_part_gte_transform_with_view(
        view,
        camera.position,
        pose,
        instance_rotation,
        LocalToWorldScale::IDENTITY,
        origin,
    );
    let (gte_rotation, gte_translation) = textured_model_part_gte_transform_with_view_gte_compose(
        view,
        view_instance,
        camera.position,
        pose,
        instance_rotation,
        LocalToWorldScale::IDENTITY,
        origin,
    );

    let mut row = 0usize;
    while row < 3 {
        let mut col = 0usize;
        while col < 3 {
            assert_close_i32(
                gte_rotation.m[row][col] as i32,
                cpu_rotation.m[row][col] as i32,
                1,
            );
            col += 1;
        }
        row += 1;
    }
    assert_close_i32(gte_translation.x, cpu_translation.x, 0);
    assert_close_i32(gte_translation.y, cpu_translation.y, 0);
    assert_close_i32(gte_translation.z, cpu_translation.z, 0);
}

#[test]
fn gte_joint_translation_matches_cpu_path_with_quantized_large_pose_offset() {
    let projection = WorldProjection::new(160, 118, 320, 48);
    let target = WorldVertex::new(80, 256, -48);
    let camera = WorldCamera::orbit_yaw(projection, target, 960, 1536, Angle::from_q12(337));
    let view = camera_gte_view_matrix(camera);
    let instance_rotation = Mat3I16::rotate_y(704).mul(&Mat3I16::rotate_x(192));
    let view_instance = mat3_mul_q12(&view, &instance_rotation);
    let pose = JointPose {
        matrix: Mat3I16::rotate_z(384).mul(&Mat3I16::rotate_y(256)).m,
        translation: Vec3I32::new(4307, -7019, 40882),
    };
    let origin = WorldVertex::new(384, 768, -256);
    let view_origin = compute_view_origin_translation(view, origin, camera.position);

    let (quantized, shift) = quantize_pose_translation_for_gte(pose.translation);
    assert_eq!(shift, 1);
    assert_eq!(quantized, Vec3I16::new(2154, -3510, 20441));

    let (cpu_rotation, cpu_translation) = textured_model_part_gte_transform_with_view(
        view,
        camera.position,
        pose,
        instance_rotation,
        LocalToWorldScale::IDENTITY,
        origin,
    );
    let (gte_rotation, gte_translation) =
        textured_model_part_gte_transform_with_view_gte_translation(
            view_instance,
            view_origin,
            pose,
            LocalToWorldScale::IDENTITY,
        );

    let mut row = 0usize;
    while row < 3 {
        let mut col = 0usize;
        while col < 3 {
            assert_close_i32(
                gte_rotation.m[row][col] as i32,
                cpu_rotation.m[row][col] as i32,
                1,
            );
            col += 1;
        }
        row += 1;
    }
    assert_close_i32(gte_translation.x, cpu_translation.x, 4);
    assert_close_i32(gte_translation.y, cpu_translation.y, 4);
    assert_close_i32(gte_translation.z, cpu_translation.z, 4);
}

#[test]
fn world_projection_accepts_vertices_on_near_plane() {
    let projection = WorldProjection::new(160, 120, 200, 40);

    let projected = projection.project_view(ViewVertex::new(0, 0, 40));

    assert_eq!(projected, Some(ProjectedVertex::new(160, 120, 40)));
}

#[test]
fn world_commands_sort_in_ot_insertion_order() {
    let mut commands = [
        world_command(5, 600, 0),
        world_command(5, 300, 1),
        world_command(3, 400, 2),
        world_command(5, 300, 3),
    ];

    sort_world_for_ot_insert(&mut commands);

    assert_eq!(commands[0], world_command(3, 400, 2));
    assert_eq!(commands[1], world_command(5, 300, 3));
    assert_eq!(commands[2], world_command(5, 300, 1));
    assert_eq!(commands[3], world_command(5, 600, 0));
}

#[test]
fn world_render_layer_follows_texture_material_transparency() {
    let opaque = TextureMaterial::opaque(0, 0, (128, 128, 128));
    let transparent = TextureMaterial::blended(0, 0, (128, 128, 128), BlendMode::Average);

    assert_eq!(
        WorldRenderLayer::for_material(opaque),
        WorldRenderLayer::Opaque
    );
    assert_eq!(
        WorldRenderLayer::for_material(transparent),
        WorldRenderLayer::Transparent
    );

    assert_eq!(
        WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
            .with_material_layer(transparent)
            .render_layer,
        WorldRenderLayer::Transparent
    );
}

#[test]
fn textured_submit_uses_transparent_layer_for_translucent_material() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 1];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 1];
    let material = TextureMaterial::blended(0, 0, (128, 128, 128), BlendMode::Average);

    let stats = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
        pass.submit_textured_triangle(
            &mut triangles,
            [
                ProjectedVertex::new(0, 0, 100),
                ProjectedVertex::new(16, 0, 100),
                ProjectedVertex::new(0, 16, 100),
            ],
            [(0, 0), (15, 0), (0, 15)],
            material,
            WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
                .with_cull_mode(CullMode::None),
        )
    };

    assert_eq!(stats.submitted_triangles, 1);
    assert_eq!(
        commands[0].render_layer,
        world_render_layer_code(WorldRenderLayer::Transparent)
    );
}

#[test]
fn textured_split_leaf_fast_matches_recursive_leaf_packet() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let material = TextureMaterial::opaque(7, 11, (96, 128, 160));
    let options = WorldSurfaceOptions::new(DepthBand::new(1, 6), DepthRange::new(0, 1000))
        .with_depth_policy(DepthPolicy::Average)
        .with_depth_bias(9)
        .with_cull_mode(CullMode::None)
        .with_textured_triangle_splitting(true);
    let verts = [
        ProjectedTexturedVertex::new(ProjectedVertex::new(1040, 20, 100), 3, 5),
        ProjectedTexturedVertex::new(ProjectedVertex::new(1010, 80, 120), 17, 7),
        ProjectedTexturedVertex::new(ProjectedVertex::new(990, 30, 140), 11, 29),
    ];

    let mut old_ot_storage = OrderingTable::<8>::new();
    let mut old_ot = OtFrame::begin(&mut old_ot_storage);
    let mut old_storage = [const { ZERO }; 1];
    let mut old_triangles = PrimitiveArena::new(&mut old_storage);
    let mut old_commands = [WorldTriCommand::EMPTY; 1];
    let old_stats = {
        let mut pass = WorldRenderPass::new(&mut old_ot, &mut old_commands);
        pass.submit_textured_triangle_split(&mut old_triangles, verts, material, options, 0)
    };

    let mut fast_ot_storage = OrderingTable::<8>::new();
    let mut fast_ot = OtFrame::begin(&mut fast_ot_storage);
    let mut fast_storage = [const { ZERO }; 1];
    let mut fast_triangles = PrimitiveArena::new(&mut fast_storage);
    let mut fast_commands = [WorldTriCommand::EMPTY; 1];
    let fast_stats = {
        let mut pass = WorldRenderPass::new(&mut fast_ot, &mut fast_commands);
        pass.submit_textured_triangle_split_leaf_fast(&mut fast_triangles, verts, material, options)
            .expect("triangle should not need recursive splitting")
    };

    assert_eq!(old_stats, fast_stats);
    assert_eq!(old_stats.submitted_triangles, 1);
    assert_eq!(old_storage[0].tex_window, fast_storage[0].tex_window);
    assert_eq!(old_storage[0].color_cmd, fast_storage[0].color_cmd);
    assert_eq!(old_storage[0].v0, fast_storage[0].v0);
    assert_eq!(old_storage[0].uv0_clut, fast_storage[0].uv0_clut);
    assert_eq!(old_storage[0].v1, fast_storage[0].v1);
    assert_eq!(old_storage[0].uv1_tpage, fast_storage[0].uv1_tpage);
    assert_eq!(old_storage[0].v2, fast_storage[0].v2);
    assert_eq!(old_storage[0].uv2, fast_storage[0].uv2);
    assert_eq!(old_commands[0].slot, fast_commands[0].slot);
    assert_eq!(old_commands[0].depth, fast_commands[0].depth);
    assert_eq!(old_commands[0].render_layer, fast_commands[0].render_layer);
    assert_eq!(old_commands[0].words, fast_commands[0].words);
}

#[test]
fn textured_split_leaf_fast_preserves_quality_split_fallback() {
    const ZERO: TriTextured = TriTextured::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        0,
        0,
        (0, 0, 0),
    );
    let material = TextureMaterial::opaque(0, 0, (128, 128, 128));
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
        .with_cull_mode(CullMode::None)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(16);
    let verts = [
        ProjectedTexturedVertex::new(ProjectedVertex::new(0, 0, 100), 0, 0),
        ProjectedTexturedVertex::new(ProjectedVertex::new(64, 0, 100), 63, 0),
        ProjectedTexturedVertex::new(ProjectedVertex::new(0, 16, 100), 0, 15),
    ];

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 1];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 1];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    assert!(pass
        .submit_textured_triangle_split_leaf_fast(&mut triangles, verts, material, options)
        .is_none());
    assert_eq!(triangles.len(), 0);
    assert_eq!(pass.command_len, 0);
}

#[test]
fn prescreened_gouraud_submit_matches_unculled_submit_depth() {
    const ZERO: TriTexturedGouraud = TriTexturedGouraud::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0, 0), (0, 0, 0), (0, 0, 0)],
        0,
        0,
    );
    let material = TextureMaterial::opaque(0, 0, (128, 128, 128));
    let verts = [
        ProjectedVertex::new(0, 0, 100),
        ProjectedVertex::new(16, 0, 120),
        ProjectedVertex::new(0, 16, 140),
    ];
    let uvs = [(0, 0), (15, 0), (0, 15)];
    let colors = [(128, 128, 128), (96, 96, 96), (64, 64, 64)];
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
        .with_cull_mode(CullMode::None);

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 2];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 2];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let regular = pass.submit_textured_gouraud_triangle(
        &mut triangles,
        verts,
        uvs,
        colors,
        material,
        options,
    );
    let prescreened = pass.submit_textured_gouraud_triangle_prescreened(
        &mut triangles,
        verts,
        uvs,
        colors,
        material,
        options,
    );

    assert_eq!(regular.submitted_triangles, 1);
    assert_eq!(prescreened.submitted_triangles, 1);
    assert_eq!(commands[0].depth_raw(), commands[1].depth_raw());
}

#[test]
fn prescreened_gouraud_honors_quality_split_max_edge() {
    const ZERO: TriTexturedGouraud = TriTexturedGouraud::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0, 0), (0, 0, 0), (0, 0, 0)],
        0,
        0,
    );
    let material = TextureMaterial::opaque(0, 0, (128, 128, 128));
    let verts = [
        ProjectedVertex::new(0, 0, 100),
        ProjectedVertex::new(64, 0, 120),
        ProjectedVertex::new(0, 16, 140),
    ];
    let uvs = [(0, 0), (63, 0), (0, 15)];
    let colors = [(128, 128, 128), (96, 96, 96), (64, 64, 64)];
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000))
        .with_cull_mode(CullMode::None)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(16);

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut triangle_storage = [const { ZERO }; 16];
    let mut triangles = PrimitiveArena::new(&mut triangle_storage);
    let mut commands = [WorldTriCommand::EMPTY; 16];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let stats = pass.submit_textured_gouraud_triangle_prescreened_u8(
        &mut triangles,
        verts,
        uvs,
        colors,
        material,
        options,
    );

    assert!(stats.split_triangles > 0);
    assert!(stats.submitted_triangles > 1);
    assert_eq!(triangles.len(), stats.submitted_triangles as usize);
}

#[test]
fn static_prop_quad_leaf_matches_generic_packet_and_depth() {
    let material = TextureMaterial::opaque(2, 4, (128, 96, 64));
    let verts = [
        ProjectedVertex::new(8, 4, 100),
        ProjectedVertex::new(32, 6, 120),
        ProjectedVertex::new(30, 36, 140),
        ProjectedVertex::new(10, 34, 160),
    ];
    let uvs = [(3, 5), (17, 7), (19, 29), (5, 27)];
    let colors = [(128, 96, 64), (80, 120, 160), (32, 48, 96), (160, 128, 96)];
    let base_options = WorldSurfaceOptions::new(DepthBand::new(1, 6), DepthRange::new(0, 1000))
        .with_depth_bias(13);
    let generic_options = base_options
        .with_depth_policy(DepthPolicy::Average)
        .with_cull_mode(CullMode::None)
        .with_material_layer(material)
        .with_textured_triangle_splitting(true)
        .with_textured_triangle_max_edge(0);

    let mut regular_ot_storage = OrderingTable::<8>::new();
    let mut regular_ot = OtFrame::begin(&mut regular_ot_storage);
    let mut regular_scratch = crate::PrimitivePacketScratch::<2>::ZERO;
    let mut regular_packets = crate::PrimitivePacketArena::new(&mut regular_scratch);
    let mut regular_commands = [WorldTriCommand::EMPTY; 2];
    let regular = {
        let mut pass = WorldRenderPass::new(&mut regular_ot, &mut regular_commands);
        pass.submit_textured_gouraud_quad_prescreened_u8(
            &mut regular_packets,
            &verts,
            &uvs,
            &colors,
            material,
            generic_options,
        )
    };

    let mut leaf_ot_storage = OrderingTable::<8>::new();
    let mut leaf_ot = OtFrame::begin(&mut leaf_ot_storage);
    let mut leaf_scratch = crate::PrimitivePacketScratch::<2>::ZERO;
    let mut leaf_packets = crate::PrimitivePacketArena::new(&mut leaf_scratch);
    let mut leaf_commands = [WorldTriCommand::EMPTY; 2];
    let leaf = {
        let mut pass = WorldRenderPass::new(&mut leaf_ot, &mut leaf_commands);
        pass.submit_static_prop_textured_gouraud_quad_prescreened_u8(
            &mut leaf_packets,
            &verts,
            &uvs,
            &colors,
            material,
            &base_options,
        )
    };

    assert_eq!(regular.submitted_triangles, leaf.submitted_triangles);
    assert_eq!(regular.split_triangles, leaf.split_triangles);
    assert_eq!(regular_packets.len(), leaf_packets.len());
    assert_eq!(regular_commands[0].slot, leaf_commands[0].slot);
    assert_eq!(regular_commands[0].depth, leaf_commands[0].depth);
    assert_eq!(
        regular_commands[0].render_layer,
        leaf_commands[0].render_layer
    );
    assert_eq!(regular_commands[0].words, leaf_commands[0].words);

    let packet_words = usize::from(regular_commands[0].words) + 1;
    // SAFETY: both command pointers reference live packet-arena slots and
    // `words + 1` includes the packet tag followed by the declared GP0 words.
    let regular_words =
        unsafe { core::slice::from_raw_parts(regular_commands[0].packet_ptr, packet_words) };
    let leaf_words =
        unsafe { core::slice::from_raw_parts(leaf_commands[0].packet_ptr, packet_words) };
    assert_eq!(regular_words, leaf_words);
}

#[test]
fn prepared_depth_gouraud_submit_matches_fixed_depth_leaf() {
    const ZERO: TriTexturedGouraud = TriTexturedGouraud::new(
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0), (0, 0), (0, 0)],
        [(0, 0, 0), (0, 0, 0), (0, 0, 0)],
        0,
        0,
    );
    let material = TextureMaterial::opaque(2, 4, (128, 128, 128));
    let verts = [
        ProjectedVertex::new(8, 4, 100),
        ProjectedVertex::new(32, 6, 120),
        ProjectedVertex::new(14, 36, 140),
    ];
    let uvs = [(3, 5), (17, 7), (11, 29)];
    let colors = [(128, 96, 64), (80, 120, 160), (32, 48, 96)];
    let options = WorldSurfaceOptions::new(DepthBand::new(1, 6), DepthRange::new(0, 1000))
        .with_depth_policy(DepthPolicy::Fixed(320))
        .with_depth_bias(13);
    let prepared = PreparedTriangleDepth::from_fixed_options::<8>(options).unwrap();

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut regular_storage = [const { ZERO }; 1];
    let mut regular_triangles = PrimitiveArena::new(&mut regular_storage);
    let mut regular_commands = [WorldTriCommand::EMPTY; 1];
    let regular = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut regular_commands);
        pass.submit_textured_gouraud_triangle_prescreened_u8(
            &mut regular_triangles,
            verts,
            uvs,
            colors,
            material,
            options,
        )
    };

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut prepared_storage = [const { ZERO }; 1];
    let mut prepared_triangles = PrimitiveArena::new(&mut prepared_storage);
    let mut prepared_commands = [WorldTriCommand::EMPTY; 1];
    let prepared_stats = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut prepared_commands);
        pass.submit_textured_gouraud_triangle_leaf_u8_prepared_depth(
            &mut prepared_triangles,
            verts,
            uvs,
            colors,
            material,
            options,
            prepared,
        )
    };

    assert_eq!(regular.submitted_triangles, 1);
    assert_eq!(prepared_stats.submitted_triangles, 1);
    assert_eq!(
        regular_storage[0].tex_window,
        prepared_storage[0].tex_window
    );
    assert_eq!(
        regular_storage[0].color0_cmd,
        prepared_storage[0].color0_cmd
    );
    assert_eq!(regular_storage[0].v0, prepared_storage[0].v0);
    assert_eq!(regular_storage[0].uv0_clut, prepared_storage[0].uv0_clut);
    assert_eq!(regular_storage[0].color1, prepared_storage[0].color1);
    assert_eq!(regular_storage[0].v1, prepared_storage[0].v1);
    assert_eq!(regular_storage[0].uv1_tpage, prepared_storage[0].uv1_tpage);
    assert_eq!(regular_storage[0].color2, prepared_storage[0].color2);
    assert_eq!(regular_storage[0].v2, prepared_storage[0].v2);
    assert_eq!(regular_storage[0].uv2, prepared_storage[0].uv2);
    assert_eq!(regular_commands[0].slot, prepared_commands[0].slot);
    assert_eq!(regular_commands[0].depth, prepared_commands[0].depth);
    assert_eq!(
        regular_commands[0].render_layer,
        prepared_commands[0].render_layer
    );
    assert_eq!(regular_commands[0].words, prepared_commands[0].words);
}

#[test]
fn prepared_depth_quad_splits_before_ps1_extent_rejection() {
    let material = TextureMaterial::opaque(2, 4, (128, 128, 128));
    // Every coordinate fits the PS1 packet field, but each GP0(3Ch)
    // triangle exceeds the real GPU's 1023x511 delta limits.
    let verts = [
        ProjectedVertex::new(-900, -400, 100),
        ProjectedVertex::new(900, -400, 120),
        ProjectedVertex::new(-900, 400, 140),
        ProjectedVertex::new(900, 400, 160),
    ];
    let uv_words = [
        model_uv_word((0, 0)),
        model_uv_word((63, 0)),
        model_uv_word((0, 63)),
        model_uv_word((63, 63)),
    ];
    let colors = [(128, 96, 64), (80, 120, 160), (32, 48, 96), (160, 128, 96)];
    let options = WorldSurfaceOptions::new(DepthBand::new(1, 6), DepthRange::new(0, 1000))
        .with_depth_policy(DepthPolicy::Fixed(320))
        .with_cull_mode(CullMode::None);
    let prepared = PreparedTriangleDepth::from_fixed_options::<8>(options).unwrap();

    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<16>::ZERO;
    let mut packets = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [WorldTriCommand::EMPTY; 16];
    let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

    let stats = pass.submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth(
        &mut packets,
        None,
        false,
        1,
        verts,
        uv_words,
        colors,
        material,
        &options,
        prepared,
    );

    assert!(stats.split_triangles > 0);
    assert!(stats.submitted_triangles > 2);
    assert_eq!(packets.len(), stats.submitted_triangles as usize);
}

fn adaptive_quad_packet_count(depth: i32) -> (usize, WorldRenderStats) {
    adaptive_quad_packet_count_with_profile(depth, AdaptiveSubdivisionProfile::REFERENCE)
}

fn adaptive_quad_packet_count_with_profile(
    depth: i32,
    profile: AdaptiveSubdivisionProfile,
) -> (usize, WorldRenderStats) {
    let projection = WorldProjection::new(160, 120, 256, 16);
    let positions = [
        ViewVertex::new(-256, -256, depth),
        ViewVertex::new(256, -256, depth),
        ViewVertex::new(-256, 256, depth),
        ViewVertex::new(256, 256, depth),
    ];
    let uv_words = [
        model_uv_word((0, 0)),
        model_uv_word((63, 0)),
        model_uv_word((0, 63)),
        model_uv_word((63, 63)),
    ];
    let colors = [(128, 128, 128); 4];
    let material = TextureMaterial::opaque(2, 4, (128, 128, 128));
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 8192))
        .with_cull_mode(CullMode::None)
        .with_adaptive_subdivision_profile(profile);
    let mut ot_storage = OrderingTable::<64>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<32>::ZERO;
    let mut packets = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [WorldTriCommand::EMPTY; 32];
    let stats = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
        pass.submit_adaptive_textured_gouraud_view_quad_uv_words(
            &mut packets,
            positions,
            None,
            false,
            None,
            uv_words,
            colors,
            projection,
            material,
            &options,
        )
    };
    (packets.len(), stats)
}

#[cfg(feature = "tr-subdivision-lattice")]
#[test]
fn adaptive_lattice_root_projection_reuse_is_exact() {
    let projection = WorldProjection::new(160, 120, 256, 16);
    let vertices = [
        ViewVertex::new(-300, -200, 2000),
        ViewVertex::new(0, -180, 2100),
        ViewVertex::new(300, -160, 2200),
        ViewVertex::new(-280, 0, 2300),
        ViewVertex::new(0, 20, 2400),
        ViewVertex::new(280, 40, 2500),
        ViewVertex::new(-260, 200, 2600),
        ViewVertex::new(0, 220, 2700),
        ViewVertex::new(260, 240, 2800),
    ];
    load_adaptive_view_projection_gte(projection);
    let projected =
        project_adaptive_view_lattice_gte(vertices, projection, None).expect("valid lattice");
    let root = [projected[0], projected[2], projected[6], projected[8]];

    load_adaptive_view_projection_gte(projection);
    let reused = project_adaptive_view_lattice_gte(vertices, projection, Some(root))
        .expect("valid reused lattice");

    assert_eq!(reused, projected);
}

#[test]
fn adaptive_quad_subdivision_uses_exact_two_depth_bands() {
    let (far_packets, far_stats) = adaptive_quad_packet_count(7 * 1024);
    let (middle_packets, middle_stats) = adaptive_quad_packet_count(4 * 1024);
    let (near_packets, near_stats) = adaptive_quad_packet_count(2 * 1024);

    assert_eq!(far_packets, 1);
    assert_eq!(middle_packets, 5);
    assert_eq!(near_packets, 16);
    assert_eq!(far_stats.split_triangles, 0);
    assert_eq!(middle_stats.split_triangles, 1);
    assert_eq!(near_stats.split_triangles, 5);
}

#[test]
fn adaptive_quad_subdivision_scales_with_cortex_sector_size() {
    let cortex = AdaptiveSubdivisionProfile::for_sector_size(1664);
    assert_eq!(
        cortex,
        AdaptiveSubdivisionProfile {
            max_levels: 2,
            far_depth: 8320,
            near_depth: 4992,
            underdraw_depth: 4160,
            underdraw_depth_bias: 416,
        }
    );

    // Cortex's 3300-unit horizontal boom and 600-unit camera-to-focus height
    // difference place its focus near z=3354. That is outside the reference engine's second
    // band but remains inside the equivalent three-sector Cortex band.
    let (tr5_focus_packets, tr5_focus_stats) =
        adaptive_quad_packet_count_with_profile(3354, AdaptiveSubdivisionProfile::REFERENCE);
    let (cortex_focus_packets, cortex_focus_stats) =
        adaptive_quad_packet_count_with_profile(3354, cortex);
    assert_eq!(tr5_focus_packets, 5);
    assert_eq!(tr5_focus_stats.split_triangles, 1);
    assert_eq!(cortex_focus_packets, 16);
    assert_eq!(cortex_focus_stats.split_triangles, 5);

    // A surface at z=6000 was entirely outside the unscaled root band.
    let (tr5_far_packets, _) =
        adaptive_quad_packet_count_with_profile(6000, AdaptiveSubdivisionProfile::REFERENCE);
    let (cortex_far_packets, cortex_far_stats) =
        adaptive_quad_packet_count_with_profile(6000, cortex);
    assert_eq!(tr5_far_packets, 1);
    assert_eq!(cortex_far_packets, 5);
    assert_eq!(cortex_far_stats.split_triangles, 1);
}

#[test]
fn adaptive_quad_subdivision_can_stop_after_one_level() {
    let cortex = WorldSurfaceOptions::new(DepthBand::new(0, 31), DepthRange::new(1, i32::MAX))
        .with_adaptive_subdivision_sector_size(1664)
        .with_adaptive_subdivision_max_levels(1)
        .adaptive_subdivision_profile;
    let (packets, stats) = adaptive_quad_packet_count_with_profile(3354, cortex);

    assert_eq!(cortex.max_levels, 1);
    assert_eq!(packets, 4);
    assert_eq!(stats.split_triangles, 1);
}

#[test]
fn adaptive_debug_colors_identify_generated_levels() {
    use super::world_pass_gouraud::adaptive_debug_subdivision_color;

    assert_eq!(adaptive_debug_subdivision_color(0), None);
    assert_eq!(adaptive_debug_subdivision_color(1), Some((0, 255, 255)));
    assert_eq!(adaptive_debug_subdivision_color(2), Some((255, 0, 255)));
    assert_eq!(adaptive_debug_subdivision_color(3), Some((255, 255, 0)));
}

#[test]
fn adaptive_quad_subdivision_matches_psx_far_boundary() {
    let (boundary_packets, boundary_stats) =
        adaptive_quad_packet_count(ADAPTIVE_SUBDIVIDE_FAR_DEPTH);
    let (inside_packets, inside_stats) =
        adaptive_quad_packet_count(ADAPTIVE_SUBDIVIDE_FAR_DEPTH - 1);

    assert_eq!(boundary_packets, 1);
    assert_eq!(boundary_stats.split_triangles, 0);
    assert_eq!(inside_packets, 5);
    assert_eq!(inside_stats.split_triangles, 1);
}

fn adaptive_triangle_packet_count(depth: i32) -> (usize, WorldRenderStats) {
    let projection = WorldProjection::new(160, 120, 256, 16);
    let positions = [
        ViewVertex::new(-256, -256, depth),
        ViewVertex::new(256, -256, depth),
        ViewVertex::new(0, 256, depth),
    ];
    let uv_words = [
        model_uv_word((0, 0)),
        model_uv_word((63, 0)),
        model_uv_word((32, 63)),
    ];
    let colors = [(128, 128, 128); 3];
    let material = TextureMaterial::opaque(2, 4, (128, 128, 128));
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 8192))
        .with_cull_mode(CullMode::None)
        .with_adaptive_subdivision(true);
    let mut ot_storage = OrderingTable::<64>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut packet_scratch = crate::PrimitivePacketScratch::<32>::ZERO;
    let mut packets = crate::PrimitivePacketArena::new(&mut packet_scratch);
    let mut commands = [WorldTriCommand::EMPTY; 32];
    let stats = {
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);
        pass.submit_adaptive_textured_gouraud_view_triangle_uv_words(
            &mut packets,
            positions,
            uv_words,
            colors,
            projection,
            material,
            &options,
        )
    };
    (packets.len(), stats)
}

#[test]
fn adaptive_triangle_subdivision_uses_exact_two_depth_bands() {
    let (far_packets, far_stats) = adaptive_triangle_packet_count(7 * 1024);
    let (middle_packets, middle_stats) = adaptive_triangle_packet_count(4 * 1024);
    let (near_packets, near_stats) = adaptive_triangle_packet_count(2 * 1024);

    assert_eq!(far_packets, 1);
    assert_eq!(middle_packets, 5);
    assert_eq!(near_packets, 16);
    assert_eq!(far_stats.split_triangles, 0);
    assert_eq!(middle_stats.split_triangles, 1);
    assert_eq!(near_stats.split_triangles, 5);
}

#[test]
fn adaptive_subdivision_reprojects_camera_space_midpoints() {
    let projection = WorldProjection::new(160, 120, 256, 16);
    let a = TexturedGouraudViewVertex::new(
        ViewVertex::new(-512, 0, 1024),
        model_uv_word((0, 0)),
        (64, 64, 64),
    );
    let b = TexturedGouraudViewVertex::new(
        ViewVertex::new(512, 0, 4096),
        model_uv_word((64, 0)),
        (192, 192, 192),
    );
    let projected_a = projection.project_view(a.position).unwrap();
    let projected_b = projection.project_view(b.position).unwrap();
    let midpoint = midpoint_textured_gouraud_view(a, b);
    let projected_midpoint = projection.project_view(midpoint.position).unwrap();
    let projected_edge_midpoint = midpoint_i16(projected_a.sx, projected_b.sx);

    assert_ne!(projected_midpoint.sx, projected_edge_midpoint);
    assert_eq!(projected_midpoint.sx, projection.screen_x);
    assert_eq!(midpoint.u, 32);
    assert_eq!(midpoint.color, (128, 128, 128));
}

#[test]
fn adaptive_identity_rtps_matches_world_projection() {
    let projection = WorldProjection::new(160, 120, 256, 16);
    load_adaptive_view_projection_gte(projection);
    for vertex in [
        ViewVertex::new(-512, 320, 2048),
        ViewVertex::new(384, -224, 1536),
        ViewVertex::new(0, 0, 4096),
    ] {
        assert_eq!(
            project_adaptive_view_vertex_gte(vertex, projection),
            projection.project_view(vertex)
        );
    }
}

#[test]
fn adaptive_identity_rtpt_matches_world_projection() {
    let projection = WorldProjection::new(160, 120, 256, 16);
    let vertices = [
        ViewVertex::new(-512, 320, 2048),
        ViewVertex::new(384, -224, 1536),
        ViewVertex::new(0, 0, 4096),
    ];
    load_adaptive_view_projection_gte(projection);

    assert_eq!(
        project_adaptive_view_triangle_gte(vertices, projection),
        Some([
            projection.project_view(vertices[0]).unwrap(),
            projection.project_view(vertices[1]).unwrap(),
            projection.project_view(vertices[2]).unwrap(),
        ])
    );
}

#[test]
fn prebuilt_static_room_quad_only_patches_positions_after_first_draw() {
    let first = [
        ProjectedVertex::new(10, 10, 200),
        ProjectedVertex::new(30, 10, 200),
        ProjectedVertex::new(30, 30, 200),
        ProjectedVertex::new(10, 30, 200),
    ];
    let second = [
        ProjectedVertex::new(11, 12, 200),
        ProjectedVertex::new(31, 12, 200),
        ProjectedVertex::new(31, 32, 200),
        ProjectedVertex::new(11, 32, 200),
    ];
    let material = TextureMaterial::opaque(3, 4, (128, 128, 128));
    let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(0, 1000));
    let prepared = PreparedTriangleDepth::from_quad_average::<8>(options, first);
    let mut ot_storage = OrderingTable::<8>::new();
    let mut ot = OtFrame::begin(&mut ot_storage);
    let mut commands = [WorldTriCommand::EMPTY; 2];
    let mut pass = WorldRenderPass::new_bucketed(&mut ot, &mut commands);
    let mut packet = QuadTexturedGouraud::EMPTY;
    let mut valid = 0;
    let first_colors = [(10, 20, 30), (40, 50, 60), (70, 80, 90), (100, 110, 120)];

    let _ = pass.submit_prebuilt_textured_gouraud_quad(
        &mut packet,
        &mut valid,
        true,
        1,
        first,
        [0, 1, 2, 3],
        first_colors,
        material.textured_gouraud_packet_material(),
        &options,
        prepared,
    );
    let packed_colors = (
        packet.color0_cmd,
        packet.color1,
        packet.color2,
        packet.color3,
    );
    let first_v0 = packet.v0;
    let warmed = pass.try_submit_warmed_textured_gouraud_quad(
        &mut packet,
        second,
        false,
        &options,
        prepared,
    );

    assert_eq!(valid, 1);
    assert!(warmed.is_some());
    assert_ne!(packet.v0, first_v0);
    assert_eq!(
        (
            packet.color0_cmd,
            packet.color1,
            packet.color2,
            packet.color3
        ),
        packed_colors
    );
}

#[test]
fn world_commands_put_transparent_ties_before_opaque_insertions() {
    let mut commands = [
        world_command_layer(5, 300, WorldRenderLayer::Opaque, 0),
        world_command_layer(5, 300, WorldRenderLayer::Transparent, 1),
        world_command_layer(5, 300, WorldRenderLayer::Opaque, 2),
    ];

    sort_world_for_ot_insert(&mut commands);

    assert_eq!(
        commands[0],
        world_command_layer(5, 300, WorldRenderLayer::Transparent, 1)
    );
    assert_eq!(
        commands[1],
        world_command_layer(5, 300, WorldRenderLayer::Opaque, 2)
    );
    assert_eq!(
        commands[2],
        world_command_layer(5, 300, WorldRenderLayer::Opaque, 0)
    );
}

fn dot_q12_row_i16(row: [i16; 3], v: Vec3I16) -> i32 {
    ((row[0] as i32) * (v.x as i32)
        + (row[1] as i32) * (v.y as i32)
        + (row[2] as i32) * (v.z as i32))
        >> 12
}

fn assert_close_i32(actual: i32, expected: i32, tolerance: i32) {
    let delta = actual.saturating_sub(expected).abs();
    assert!(
        delta <= tolerance,
        "actual {actual}, expected {expected}, delta {delta}, tolerance {tolerance}"
    );
}

#[test]
fn compact_view_vertex_lerp_matches_weighted_form() {
    let values = [-32_768, -10_000, -1, 0, 1, 10_000, 32_767];
    let weights = [0, 1, 63, 127, 128, 129, 254, 255];
    for &a in &values {
        for &b in &values {
            for &weight in &weights {
                let t = i32::from(weight);
                let expected = ((a * (256 - t)) + (b * t)) >> 8;
                let actual =
                    lerp_view_vertex(ViewVertex::new(a, a, a), ViewVertex::new(b, b, b), weight);
                assert_eq!(actual, ViewVertex::new(expected, expected, expected));
            }
        }
    }
}

/// The chunked blended-vertex flush must produce exactly the same
/// projected vertices as the per-vertex slow path it replaces: same
/// transforms, same lerp, same RTPS wrapper, only the GTE matrix loads
/// are amortized. Runs on the host software GTE, so equality is exact.
#[cfg(not(feature = "vert-debug"))]
#[test]
fn blended_chunk_flush_matches_per_vertex_slow_path() {
    let projection = WorldProjection::new(160, 120, 200, 40);
    load_world_projection_gte(projection);
    let near_z = projection.near_z;

    let primary = JointViewTransform {
        rotation: Mat3I16::IDENTITY,
        translation: Vec3I32::new(10, -6, 900),
    };
    let second_primary = JointViewTransform {
        rotation: Mat3I16::rotate_y(37),
        translation: Vec3I32::new(-35, 19, 860),
    };
    let yaw90 = Mat3I16 {
        m: [[0, 0, 4096], [0, 4096, 0], [-4096, 0, 0]],
    };
    let pitch90 = Mat3I16 {
        m: [[4096, 0, 0], [0, 0, -4096], [0, 4096, 0]],
    };
    let joint_view_transforms = [
        primary,
        JointViewTransform {
            rotation: yaw90,
            translation: Vec3I32::new(-24, 14, 950),
        },
        JointViewTransform {
            rotation: pitch90,
            translation: Vec3I32::new(40, 2, 800),
        },
    ];

    // Seam vertices alternating between two secondary joints with varied
    // weights, sized past one chunk so the mid-part flush runs too.
    const SEAM_VERTS: usize = BLENDED_VERTEX_CHUNK + 5;
    let vertices: [ModelVertex; SEAM_VERTS] = core::array::from_fn(|i| {
        let s = i as i16;
        ModelVertex {
            position: Vec3I16::new(40 + 3 * s, -25 + 2 * s, 60 + 5 * s),
            joint1: 1 + (i % 2) as u8,
            blend: 16 + ((i * 13) % 240) as u8,
        }
    });

    // Expected: the per-vertex slow path with the primary joint loaded,
    // exactly as the model pass guarantees before each blended vertex.
    let expected: [ProjectedVertex; SEAM_VERTS] = core::array::from_fn(|i| {
        let part_primary = if i < BLENDED_VERTEX_CHUNK / 2 {
            primary
        } else {
            second_primary
        };
        scene::load_rotation(&part_primary.rotation);
        scene::load_translation(part_primary.translation);
        project_blended_textured_model_vertex(
            vertices[i],
            part_primary,
            &joint_view_transforms,
            projection,
        )
    });

    let mut actual = [ProjectedVertex::default(); SEAM_VERTS];
    let mut all_in_front = true;
    let mut all_inside = true;
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (i16::MAX, i16::MIN, i16::MAX, i16::MIN);
    scene::load_rotation(&primary.rotation);
    scene::load_translation(primary.translation);
    let indices: [u16; SEAM_VERTS] = core::array::from_fn(|i| i as u16);
    for chunk in indices.chunks(BLENDED_VERTEX_CHUNK) {
        let mut primary_views = [ViewVertex::ZERO; BLENDED_VERTEX_CHUNK];
        for (slot, &vertex_index) in chunk.iter().enumerate() {
            let part_primary = if usize::from(vertex_index) < BLENDED_VERTEX_CHUNK / 2 {
                primary
            } else {
                second_primary
            };
            scene::load_rotation(&part_primary.rotation);
            scene::load_translation(part_primary.translation);
            let transformed =
                scene::transform_vertex_scheduled(vertices[usize::from(vertex_index)].position);
            primary_views[slot] = ViewVertex::new(transformed.x, transformed.y, transformed.z);
        }
        unsafe {
            flush_blended_model_vertex_chunk(
                chunk.as_ptr(),
                primary_views.as_mut_ptr(),
                chunk.len(),
                &vertices,
                second_primary,
                &joint_view_transforms,
                projection,
                near_z,
                &mut actual,
                &mut all_in_front,
                &mut all_inside,
                &mut min_x,
                &mut max_x,
                &mut min_y,
                &mut max_y,
            );
        }
    }

    assert_eq!(actual, expected);
    // The seam fixture sits in front of the near plane and on-screen, so
    // the fold flags must agree with a direct per-vertex evaluation.
    for &projected in &expected {
        assert!(projected_model_vertex_in_front(projected, near_z));
    }
    assert!(all_in_front);
    assert!(all_inside);
}

/// GROUNDING PROBE (diagnostic, remove after the float is closed): replicate
/// the full player vertex chain on the host GTE for the settled pitched-down
/// frame and diff it against exact f64 math, stage by stage.
#[test]
fn grounding_probe_player_lowest_vertex_matches_reference() {
    extern crate std;
    // By name, not by index: the model_NN prefix shifts whenever a model is
    // added to the scene (the swords pushed Aletha from 000 to 002), and the
    // clip_NN prefix shifts whenever a clip joins the character's set.
    let models = std::fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/editor-playtest/generated/models"
    ))
    .expect("cooked models dir")
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .find(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("_aletha_delivered"))
    })
    .expect("cooked aletha model dir");
    let model_bytes = std::fs::read(models.join("mesh.psxmdl")).expect("cooked mesh");
    let idle = std::fs::read_dir(&models)
        .expect("cooked model dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_aletha_idle.psxanim"))
        })
        .expect("cooked idle clip");
    let clip_bytes = std::fs::read(idle).expect("cooked idle");
    let model = psx_asset::Model::from_bytes(&model_bytes).expect("model");
    let clip = psx_asset::Animation::from_bytes(&clip_bytes).expect("clip");

    psx_gte::host::reset();
    // The settled pitched-down frame: camera basis + positions from telemetry.
    let projection = WorldProjection::new(160, 120, 320, 4);
    let camera = WorldCamera::from_basis(
        projection,
        WorldVertex::new(128, 170, 182),
        Q12::from_raw(0),
        Q12::from_raw(4096),
        Q12::from_raw(-3702),
        Q12::from_raw(1769),
    );
    // origin = player (128,4,128) + lift apply(1754) with composed q12 98.
    let composed = LocalToWorldScale::from_q12(98);
    let lift = composed.apply(1754);
    let origin = WorldVertex::new(128, 4 + lift, 128);
    let instance_rotation = Mat3I16::IDENTITY;

    // The model path's convention: X/Y rows scaled and H divided (see
    // MODEL_GTE_XY_SCALE), which keeps RTPS out of its H/SZ <= 2 clamp.
    let camera_view = model_gte_view_matrix(camera);
    let view_instance = mat3_mul_q12(&camera_view, &instance_rotation);
    let view_origin_translation =
        compute_view_origin_translation(camera_view, origin, camera.position);
    load_world_projection_gte(model_gte_projection(camera.projection));

    // Find the model's lowest vertex and its part joint at idle frame 0.
    let first = model.vertex(0).unwrap();
    let mut lowest = (i32::MAX, 0u16, first);
    for part_index in 0..model.part_count() {
        let part = model.part(part_index).unwrap();
        let joint = part.joint_index() as u16;
        let pose = clip.pose(0, joint).unwrap();
        for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
            let vertex = model.vertex(v).unwrap();
            // exact world y of this vertex
            let m = pose.matrix;
            let p = [
                vertex.position.x as f64,
                vertex.position.y as f64,
                vertex.position.z as f64,
            ];
            let ry = (m[0][1] as f64 * p[0] + m[1][1] as f64 * p[1] + m[2][1] as f64 * p[2])
                / 4096.0
                + pose.translation.y as f64;
            let world_y = ry * 98.0 / 4096.0;
            if (world_y as i32) < lowest.0 {
                lowest = (world_y as i32, joint, vertex);
            }
        }
    }
    let (_, joint, vertex) = lowest;
    let pose = clip.pose(0, joint).unwrap();

    // Guest chain (the shipping non-packed path).
    let (rotation, translation) = textured_model_part_gte_transform_with_view_gte_translation(
        view_instance,
        view_origin_translation,
        pose,
        composed,
    );
    scene::load_rotation(&rotation);
    scene::load_translation(translation);
    let gte = scene::project_vertex_scheduled(vertex.position);

    // Exact f64 reference of the same chain.
    let m = pose.matrix;
    let p = [
        vertex.position.x as f64,
        vertex.position.y as f64,
        vertex.position.z as f64,
    ];
    let local = [
        (m[0][0] as f64 * p[0] + m[1][0] as f64 * p[1] + m[2][0] as f64 * p[2]) / 4096.0
            + pose.translation.x as f64,
        (m[0][1] as f64 * p[0] + m[1][1] as f64 * p[1] + m[2][1] as f64 * p[2]) / 4096.0
            + pose.translation.y as f64,
        (m[0][2] as f64 * p[0] + m[1][2] as f64 * p[1] + m[2][2] as f64 * p[2]) / 4096.0
            + pose.translation.z as f64,
    ];
    let scale = 98.0 / 4096.0;
    let world = [
        origin.x as f64 + local[0] * scale,
        origin.y as f64 + local[1] * scale,
        origin.z as f64 + local[2] * scale,
    ];
    let cam = [
        camera.position.x as f64,
        camera.position.y as f64,
        camera.position.z as f64,
    ];
    let d = [world[0] - cam[0], world[1] - cam[1], world[2] - cam[2]];
    let (sy, cy, sp, cp) = (0.0, 1.0, -3702.0 / 4096.0, 1769.0 / 4096.0);
    let x1 = d[0] * cy - d[2] * sy;
    let z1 = -d[0] * sy - d[2] * cy;
    let y2 = d[1] * cp - z1 * sp;
    let z2 = d[1] * sp + z1 * cp;
    let ref_sx = 160.0 + x1 * 320.0 / z2;
    let ref_sy = 120.0 - y2 * 320.0 / z2;

    // Floor point under her via the same reference math.
    let fd = [0.0, 4.0 - cam[1], 128.0 - cam[2]];
    let fz1 = -fd[2] * cy;
    let fy2 = fd[1] * cp - fz1 * sp;
    let fz2 = fd[1] * sp + fz1 * cp;
    let floor_sy = 120.0 - fy2 * 320.0 / fz2;

    // Floor point at the VERTEX's own (x,z): the grounded reference for it.
    let vfd = [world[0] - cam[0], 4.0 - cam[1], world[2] - cam[2]];
    let vz1 = -vfd[0] * sy - vfd[2] * cy;
    let vy2 = vfd[1] * cp - vz1 * sp;
    let vz2 = vfd[1] * sp + vz1 * cp;
    let vertex_floor_sy = 120.0 - vy2 * 320.0 / vz2;
    std::eprintln!(
        "PROBE world=({:.2},{:.2},{:.2}) gte=({},{}) ref=({:.2},{:.2}) own_floor_sy={:.2} centre_floor_sy={:.2}",
        world[0], world[1], world[2], gte.sx, gte.sy, ref_sx, ref_sy, vertex_floor_sy, floor_sy
    );
    // The regression this probe protects: the GTE chain must track the exact
    // projection within the PS1's own quantization (2px here), i.e. no hidden
    // vertical offset inside the joint/vertex pipeline.
    assert!(
        (f64::from(gte.sy) - ref_sy).abs() <= 2.0,
        "GTE sy drifted from the reference"
    );
    assert!(
        (f64::from(gte.sx) - ref_sx).abs() <= 3.0,
        "GTE sx drifted from the reference"
    );
}
