use super::*;

impl<'a, 'ot, const OT_DEPTH: usize> WorldRenderPass<'a, 'ot, OT_DEPTH> {
    /// Submit a textured triangle in camera space.
    ///
    /// The triangle is clipped against the projection's near plane
    /// before projection. This avoids whole-surface popping when a
    /// floor or wall crosses the camera plane.
    pub fn submit_textured_view_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [TexturedViewVertex; 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut clipped = [TexturedViewVertex::ZERO; 4];
        let count = clip_textured_triangle_to_near(verts, projection.near_z, &mut clipped);
        let mut stats = WorldRenderStats::default();
        if count < 3 {
            stats.dropped_triangles = 1;
            return stats;
        }
        if count != 3 || !verts.iter().all(|v| v.position.z >= projection.near_z) {
            stats.clipped_triangles = 1;
        }

        let first = self.submit_clipped_textured_triangle(
            triangles,
            [clipped[0], clipped[1], clipped[2]],
            projection,
            material,
            options,
        );
        merge_world_stats(&mut stats, first);
        if stats.primitive_overflow || stats.command_overflow || count == 3 {
            return stats;
        }

        let second = self.submit_clipped_textured_triangle(
            triangles,
            [clipped[0], clipped[2], clipped[3]],
            projection,
            material,
            options,
        );
        merge_world_stats(&mut stats, second);
        stats
    }

    /// Submit a textured quad in camera space as two clipped,
    /// independently culled and sorted triangles.
    ///
    /// Corners arrive in perimeter order `[0, 1, 2, 3]`. Triangles
    /// share the `0`–`2` diagonal per [`TEXTURED_QUAD_TRIANGLES`].
    pub fn submit_textured_view_quad(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [TexturedViewVertex; 4],
        projection: WorldProjection,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let [a, b, c] = TEXTURED_QUAD_TRIANGLES[0];
        let mut stats = self.submit_textured_view_triangle(
            triangles,
            [verts[a], verts[b], verts[c]],
            projection,
            material,
            options,
        );
        if stats.primitive_overflow || stats.command_overflow {
            return stats;
        }

        let [a, b, c] = TEXTURED_QUAD_TRIANGLES[1];
        let second = self.submit_textured_view_triangle(
            triangles,
            [verts[a], verts[b], verts[c]],
            projection,
            material,
            options,
        );
        merge_world_stats(&mut stats, second);
        stats
    }

    /// Transform and submit a textured world-space triangle through
    /// `camera`.
    pub fn submit_textured_world_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        camera: WorldCamera,
        verts: [WorldVertex; 3],
        uvs: [(u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        self.submit_textured_view_triangle(
            triangles,
            [
                camera.textured_view_vertex(verts[0], uvs[0]),
                camera.textured_view_vertex(verts[1], uvs[1]),
                camera.textured_view_vertex(verts[2], uvs[2]),
            ],
            camera.projection,
            material,
            options,
        )
    }

    /// Transform and submit a textured world-space quad through
    /// `camera`.
    pub fn submit_textured_world_quad(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        camera: WorldCamera,
        verts: [WorldVertex; 4],
        uvs: [(u8, u8); 4],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        self.submit_textured_view_quad(
            triangles,
            [
                camera.textured_view_vertex(verts[0], uvs[0]),
                camera.textured_view_vertex(verts[1], uvs[1]),
                camera.textured_view_vertex(verts[2], uvs[2]),
                camera.textured_view_vertex(verts[3], uvs[3]),
            ],
            camera.projection,
            material,
            options,
        )
    }

    /// Submit an animated textured model using predecoded part, vertex, and face records.
    ///
    /// This is the canonical runtime model path. Callers decode cooked `.psxmdl`
    /// parts, vertices, and faces once during asset load, then pass those compact
    /// records here every frame.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_textured_model_predecoded_geometry_faces(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        model: Model<'_>,
        animation: Animation<'_>,
        frame_q12: u32,
        camera: WorldCamera,
        origin: WorldVertex,
        instance_rotation: Mat3I16,
        local_to_world: LocalToWorldScale,
        pose_translation: ModelPoseTranslation,
        projected_vertices: &mut [ProjectedVertex],
        joint_view_transforms: &mut [JointViewTransform],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        faces: &[TexturedModelRenderFace],
        geometry: TexturedModelGeometry<'_>,
    ) -> TexturedModelRenderStats {
        self.submit_textured_model_geometry_impl(
            triangles,
            model,
            animation,
            frame_q12,
            camera,
            origin,
            instance_rotation,
            local_to_world,
            pose_translation,
            projected_vertices,
            joint_view_transforms,
            material,
            options,
            faces,
            geometry,
            true,
        )
    }

    /// Submit a primary-joint animated model using predecoded part, vertex, and face records.
    ///
    /// This is the lower-cost variant for models whose vertices are all
    /// single-bone skinned; callers still pass the same predecoded part,
    /// vertex, and face records.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_textured_model_primary_joints_predecoded_geometry_faces(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        model: Model<'_>,
        animation: Animation<'_>,
        frame_q12: u32,
        camera: WorldCamera,
        origin: WorldVertex,
        instance_rotation: Mat3I16,
        local_to_world: LocalToWorldScale,
        pose_translation: ModelPoseTranslation,
        projected_vertices: &mut [ProjectedVertex],
        joint_view_transforms: &mut [JointViewTransform],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        faces: &[TexturedModelRenderFace],
        geometry: TexturedModelGeometry<'_>,
    ) -> TexturedModelRenderStats {
        self.submit_textured_model_geometry_impl(
            triangles,
            model,
            animation,
            frame_q12,
            camera,
            origin,
            instance_rotation,
            local_to_world,
            pose_translation,
            projected_vertices,
            joint_view_transforms,
            material,
            options,
            faces,
            geometry,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_textured_model_geometry_impl(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        model: Model<'_>,
        animation: Animation<'_>,
        frame_q12: u32,
        camera: WorldCamera,
        origin: WorldVertex,
        instance_rotation: Mat3I16,
        local_to_world: LocalToWorldScale,
        pose_translation: ModelPoseTranslation,
        projected_vertices: &mut [ProjectedVertex],
        joint_view_transforms: &mut [JointViewTransform],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        faces: &[TexturedModelRenderFace],
        geometry: TexturedModelGeometry<'_>,
        blend_vertices: bool,
    ) -> TexturedModelRenderStats {
        let mut stats = TexturedModelRenderStats::default();
        let camera_view = camera_gte_view_matrix(camera);
        let view_instance = if MODEL_GTE_JOINT_COMPOSE || MODEL_GTE_JOINT_TRANSLATION {
            mat3_mul_q12(&camera_view, &instance_rotation)
        } else {
            Mat3I16::IDENTITY
        };
        let view_origin_translation = if MODEL_GTE_JOINT_TRANSLATION {
            compute_view_origin_translation(camera_view, origin, camera.position)
        } else {
            Vec3I32::ZERO
        };
        load_world_projection_gte(camera.projection);

        let joint_count = (model.joint_count() as usize).min(joint_view_transforms.len());
        let pose_sample = animation.looped_pose_sample_q12(frame_q12);
        crate::telemetry::stage_begin(crate::telemetry::stage::TEXTURED_MODEL_JOINTS);
        // EXPLOSION PROBE (diagnostic): capture this model's compose inputs;
        // frozen once a blended (player) vertex is observed this frame.
        super::player_vert_debug::record_joints_begin(&view_instance);
        if let Some(sample) = pose_sample {
            for (joint, joint_view_transform) in joint_view_transforms
                .iter_mut()
                .enumerate()
                .take(joint_count)
            {
                let joint_index = joint as u16;
                super::player_vert_debug::set_joint_slot(joint as u8);
                let joint_transform =
                    if MODEL_GTE_JOINT_TRANSLATION && MODEL_GTE_JOINT_PACKED_TRANSLATION {
                        sample
                            .gte_pose(joint_index)
                            .and_then(|pose| {
                                textured_model_part_gte_transform_with_view_gte_packed_translation(
                                    view_instance,
                                    view_origin_translation,
                                    pose,
                                    pose_translation,
                                    local_to_world,
                                )
                            })
                            .or_else(|| {
                                sample.pose(joint_index).map(|pose| {
                                    textured_model_part_gte_transform_with_view_gte_translation(
                                        view_instance,
                                        view_origin_translation,
                                        apply_model_pose_translation(pose, pose_translation),
                                        local_to_world,
                                    )
                                })
                            })
                    } else {
                        sample.pose(joint_index).map(|pose| {
                            let pose = apply_model_pose_translation(pose, pose_translation);
                            if MODEL_GTE_JOINT_TRANSLATION {
                                textured_model_part_gte_transform_with_view_gte_translation(
                                    view_instance,
                                    view_origin_translation,
                                    pose,
                                    local_to_world,
                                )
                            } else if MODEL_GTE_JOINT_COMPOSE {
                                textured_model_part_gte_transform_with_view_gte_compose(
                                    camera_view,
                                    view_instance,
                                    camera.position,
                                    pose,
                                    instance_rotation,
                                    local_to_world,
                                    origin,
                                )
                            } else {
                                textured_model_part_gte_transform_with_view(
                                    camera_view,
                                    camera.position,
                                    pose,
                                    instance_rotation,
                                    local_to_world,
                                    origin,
                                )
                            }
                        })
                    };

                *joint_view_transform = match joint_transform {
                    Some((rotation, translation)) => JointViewTransform {
                        rotation,
                        translation,
                    },
                    None => JointViewTransform::default(),
                };
            }
        } else {
            for joint_view_transform in joint_view_transforms.iter_mut().take(joint_count) {
                *joint_view_transform = JointViewTransform::default();
            }
        }
        crate::telemetry::stage_end(crate::telemetry::stage::TEXTURED_MODEL_JOINTS);

        let model_vertex_count = model.vertex_count() as usize;
        let model_part_count = model.part_count();
        if !geometry.usable_for(model) {
            stats.vertex_overflow = true;
            return stats;
        }
        let project_count = model_vertex_count
            .min(projected_vertices.len())
            .min(u16::MAX as usize);
        if project_count < model_vertex_count {
            stats.vertex_overflow = true;
        }
        stats.projected_vertices = project_count as u16;
        let near_z = camera.projection.near_z;
        let mut all_projected_vertices_in_front = true;
        let mut all_projected_vertices_inside_hw_bounds = project_count == model_vertex_count;
        let mut projected_min_x = i16::MAX;
        let mut projected_max_x = i16::MIN;
        let mut projected_min_y = i16::MAX;
        let mut projected_max_y = i16::MIN;
        // EXPLOSION PROBE (diagnostic): blended-vertex count for THIS model,
        // so the all-path projected-X bounds below are merged only for the
        // skinned player model, not every textured prop.
        let mut probe_model_blended = 0u32;
        crate::telemetry::stage_begin(crate::telemetry::stage::TEXTURED_MODEL_PROJECT);
        let parts = &geometry.parts[..model_part_count as usize];
        let vertices = &geometry.vertices[..model_vertex_count];
        let mut part_index = 0u16;
        while part_index < model_part_count {
            let part = parts[part_index as usize];
            let primary_joint = part.joint_index() as usize;
            if primary_joint >= joint_count {
                all_projected_vertices_in_front = false;
                all_projected_vertices_inside_hw_bounds = false;
                part_index += 1;
                continue;
            }
            let primary = joint_view_transforms[primary_joint];
            super::player_vert_debug::set_primary_joint(primary_joint as u8);

            scene::load_rotation(&primary.rotation);
            scene::load_translation(primary.translation);

            let mut global_index = part.first_vertex() as usize;
            let part_end = global_index
                .saturating_add(part.vertex_count() as usize)
                .min(project_count);
            while global_index < part_end {
                // Blended vertices (joint seams) take the scalar compose
                // path one at a time.
                if blend_vertices
                    && model_vertex_uses_cpu_blend(vertices[global_index], joint_count)
                {
                    let vertex = vertices[global_index];
                    stats.cpu_blended_vertices = stats.cpu_blended_vertices.wrapping_add(1);
                    probe_model_blended += 1;
                    let projected = project_blended_textured_model_vertex(
                        vertex,
                        primary,
                        joint_view_transforms,
                        camera.projection,
                    );
                    all_projected_vertices_in_front &=
                        projected_model_vertex_in_front(projected, near_z);
                    all_projected_vertices_inside_hw_bounds &=
                        projected_model_vertex_inside_hw_bounds(projected);
                    track_projected_model_bounds(
                        projected,
                        &mut projected_min_x,
                        &mut projected_max_x,
                        &mut projected_min_y,
                        &mut projected_max_y,
                    );
                    projected_vertices[global_index] = projected;
                    global_index += 1;
                    continue;
                }

                // Extent of the single-joint run starting here; unblended
                // models take the whole part in one run with no per-vertex
                // blend checks.
                let run_end = if blend_vertices {
                    let mut end = global_index + 1;
                    while end < part_end && !model_vertex_uses_cpu_blend(vertices[end], joint_count)
                    {
                        end += 1;
                    }
                    end
                } else {
                    part_end
                };

                // Software-pipelined RTPT over the run's triples: kick
                // triple N, fold triple N-1's bookkeeping while the GTE
                // runs, then read N (the next kick would clobber the
                // SXY/SZ FIFOs, so N must be read before N+1 starts).
                // Inputs go straight from the vertex slice to the GTE --
                // no staging copies, no per-vertex blend checks.
                let mut pending: Option<(usize, [scene::Projected; 3])> = None;
                while global_index + 3 <= run_end {
                    let kicked = scene::rtpt_kick(
                        vertices[global_index].position,
                        vertices[global_index + 1].position,
                        vertices[global_index + 2].position,
                    );
                    if let Some((done_index, triple)) = pending.take() {
                        commit_projected_triple(
                            done_index,
                            triple,
                            near_z,
                            projected_vertices,
                            &mut all_projected_vertices_in_front,
                            &mut projected_min_x,
                            &mut projected_max_x,
                            &mut projected_min_y,
                            &mut projected_max_y,
                        );
                    }
                    pending = Some((global_index, kicked.read()));
                    global_index += 3;
                }
                if let Some((done_index, triple)) = pending.take() {
                    commit_projected_triple(
                        done_index,
                        triple,
                        near_z,
                        projected_vertices,
                        &mut all_projected_vertices_in_front,
                        &mut projected_min_x,
                        &mut projected_max_x,
                        &mut projected_min_y,
                        &mut projected_max_y,
                    );
                }
                while global_index < run_end {
                    let projected = project_gte_model_vertex(vertices[global_index]);
                    all_projected_vertices_in_front &=
                        projected_model_vertex_in_front(projected, near_z);
                    track_projected_model_bounds(
                        projected,
                        &mut projected_min_x,
                        &mut projected_max_x,
                        &mut projected_min_y,
                        &mut projected_max_y,
                    );
                    projected_vertices[global_index] = projected;
                    global_index += 1;
                }
            }

            part_index += 1;
        }
        crate::telemetry::stage_end(crate::telemetry::stage::TEXTURED_MODEL_PROJECT);
        // EXPLOSION PROBE (diagnostic): for the skinned player model, surface
        // the ALL-path projected X bounds (blended + batch + remainder) so the
        // overlay can split blended-only vs mesh-wide widening.
        if probe_model_blended > 0 {
            super::player_vert_debug::merge_all_x(projected_min_x, projected_max_x);
        }

        let mut faces_considered = 0u32;
        let packet_material = material.textured_packet_material();
        let packed_fast_faces =
            options.split_textured_triangles && options.textured_split_max_edge == 0;
        let packed_back_in_front_faces = packed_fast_faces
            && all_projected_vertices_in_front
            && options.cull_mode == CullMode::Back;
        let packed_back_average_in_front_faces =
            packed_back_in_front_faces && options.depth_policy == DepthPolicy::Average;
        let packed_back_average_unclamped_faces =
            packed_back_average_in_front_faces && all_projected_vertices_inside_hw_bounds;
        let packed_back_average_unclamped_extent_safe_faces = packed_back_average_unclamped_faces
            && projected_model_bounds_hw_extent_safe(
                projected_min_x,
                projected_max_x,
                projected_min_y,
                projected_max_y,
            );
        crate::telemetry::stage_begin(crate::telemetry::stage::TEXTURED_MODEL_FACES);
        if packed_back_average_unclamped_faces {
            let projected_vertices = &projected_vertices[..project_count];
            let overflow = if packed_back_average_unclamped_extent_safe_faces {
                self.submit_predecoded_model_faces_packed_back_average_unclamped_extent_safe_batch(
                    triangles,
                    projected_vertices,
                    faces,
                    packet_material,
                    options,
                    &mut stats,
                    &mut faces_considered,
                )
            } else {
                self.submit_predecoded_model_faces_packed_back_average_unclamped_batch(
                    triangles,
                    projected_vertices,
                    faces,
                    packet_material,
                    material,
                    options,
                    &mut stats,
                    &mut faces_considered,
                )
            };
            if overflow {
                crate::telemetry::stage_end(crate::telemetry::stage::TEXTURED_MODEL_FACES);
                emit_textured_model_detail_counters(
                    joint_count,
                    model.part_count(),
                    project_count,
                    faces_considered,
                    blend_vertices,
                    all_projected_vertices_in_front,
                    all_projected_vertices_inside_hw_bounds,
                    packed_back_average_unclamped_faces,
                    packed_back_in_front_faces,
                    packed_fast_faces,
                    &stats,
                );
                return stats;
            }
        } else {
            let mut face_index = 0usize;
            while face_index < faces.len() {
                faces_considered = faces_considered.wrapping_add(1);
                let overflow = if packed_back_average_in_front_faces {
                    self.submit_predecoded_model_face_packed_back_average_in_front_fast(
                        triangles,
                        projected_vertices,
                        project_count,
                        faces[face_index],
                        packet_material,
                        material,
                        options,
                        &mut stats,
                    )
                } else if packed_back_in_front_faces {
                    self.submit_predecoded_model_face_packed_back_in_front_fast(
                        triangles,
                        projected_vertices,
                        project_count,
                        faces[face_index],
                        packet_material,
                        material,
                        options,
                        &mut stats,
                    )
                } else if packed_fast_faces {
                    self.submit_predecoded_model_face_packed_fast(
                        triangles,
                        projected_vertices,
                        project_count,
                        faces[face_index],
                        all_projected_vertices_in_front,
                        near_z,
                        packet_material,
                        material,
                        options,
                        &mut stats,
                    )
                } else {
                    self.submit_predecoded_model_face(
                        triangles,
                        projected_vertices,
                        project_count,
                        faces[face_index],
                        all_projected_vertices_in_front,
                        near_z,
                        packet_material,
                        material,
                        options,
                        &mut stats,
                    )
                };
                if overflow {
                    crate::telemetry::stage_end(crate::telemetry::stage::TEXTURED_MODEL_FACES);
                    emit_textured_model_detail_counters(
                        joint_count,
                        model.part_count(),
                        project_count,
                        faces_considered,
                        blend_vertices,
                        all_projected_vertices_in_front,
                        all_projected_vertices_inside_hw_bounds,
                        packed_back_average_unclamped_faces,
                        packed_back_in_front_faces,
                        packed_fast_faces,
                        &stats,
                    );
                    return stats;
                }
                face_index += 1;
            }
        }
        crate::telemetry::stage_end(crate::telemetry::stage::TEXTURED_MODEL_FACES);
        emit_textured_model_detail_counters(
            joint_count,
            model.part_count(),
            project_count,
            faces_considered,
            blend_vertices,
            all_projected_vertices_in_front,
            all_projected_vertices_inside_hw_bounds,
            packed_back_average_unclamped_faces,
            packed_back_in_front_faces,
            packed_fast_faces,
            &stats,
        );

        stats
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_faces_packed_back_average_unclamped_extent_safe_batch(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        faces: &[TexturedModelRenderFace],
        packet_material: TexturedPacketMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
        faces_considered: &mut u32,
    ) -> bool {
        let mut skipped_triangles = 0u16;
        let mut packed_face_calls = 0u16;
        let mut packed_unclamped_face_calls = 0u16;
        let mut culled_triangles = 0u16;
        let mut submitted_triangles = 0u16;
        let mut fast_submitted_triangles = 0u16;

        let mut face_index = 0usize;
        while face_index < faces.len() {
            *faces_considered = faces_considered.wrapping_add(1);
            let face = faces[face_index];
            let ia = face.vertex_indices[0] as usize;
            let ib = face.vertex_indices[1] as usize;
            let ic = face.vertex_indices[2] as usize;
            if ia >= projected_vertices.len()
                || ib >= projected_vertices.len()
                || ic >= projected_vertices.len()
            {
                skipped_triangles = skipped_triangles.wrapping_add(1);
                face_index += 1;
                continue;
            }

            packed_face_calls = packed_face_calls.wrapping_add(1);
            packed_unclamped_face_calls = packed_unclamped_face_calls.wrapping_add(1);
            let projected = unsafe {
                // SAFETY: each index was checked against `projected_vertices.len()`
                // immediately above. The slice is pretrimmed to `project_count`.
                [
                    *projected_vertices.get_unchecked(ia),
                    *projected_vertices.get_unchecked(ib),
                    *projected_vertices.get_unchecked(ic),
                ]
            };

            if projected_back_facing(projected) {
                culled_triangles = culled_triangles.wrapping_add(1);
                face_index += 1;
                continue;
            }

            match self.submit_projected_model_triangle_preclamped_packed_average_untracked(
                triangles,
                projected,
                face.uv_words,
                packet_material,
                options,
            ) {
                ModelTrianglePacketResult::Submitted => {
                    submitted_triangles = submitted_triangles.wrapping_add(1);
                    fast_submitted_triangles = fast_submitted_triangles.wrapping_add(1);
                }
                ModelTrianglePacketResult::CommandOverflow => {
                    flush_packed_unclamped_model_batch_stats(
                        stats,
                        skipped_triangles,
                        packed_face_calls,
                        packed_unclamped_face_calls,
                        culled_triangles,
                        submitted_triangles,
                        fast_submitted_triangles,
                        0,
                    );
                    stats.command_overflow = true;
                    return true;
                }
                ModelTrianglePacketResult::PrimitiveOverflow => {
                    flush_packed_unclamped_model_batch_stats(
                        stats,
                        skipped_triangles,
                        packed_face_calls,
                        packed_unclamped_face_calls,
                        culled_triangles,
                        submitted_triangles,
                        fast_submitted_triangles,
                        0,
                    );
                    stats.primitive_overflow = true;
                    return true;
                }
            }
            face_index += 1;
        }

        flush_packed_unclamped_model_batch_stats(
            stats,
            skipped_triangles,
            packed_face_calls,
            packed_unclamped_face_calls,
            culled_triangles,
            submitted_triangles,
            fast_submitted_triangles,
            0,
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_faces_packed_back_average_unclamped_batch(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        faces: &[TexturedModelRenderFace],
        packet_material: TexturedPacketMaterial,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
        faces_considered: &mut u32,
    ) -> bool {
        let mut skipped_triangles = 0u16;
        let mut packed_face_calls = 0u16;
        let mut packed_unclamped_face_calls = 0u16;
        let mut culled_triangles = 0u16;
        let mut submitted_triangles = 0u16;
        let mut fast_submitted_triangles = 0u16;
        let mut hw_extent_fallbacks = 0u16;

        let mut face_index = 0usize;
        while face_index < faces.len() {
            *faces_considered = faces_considered.wrapping_add(1);
            let face = faces[face_index];
            let ia = face.vertex_indices[0] as usize;
            let ib = face.vertex_indices[1] as usize;
            let ic = face.vertex_indices[2] as usize;
            if ia >= projected_vertices.len()
                || ib >= projected_vertices.len()
                || ic >= projected_vertices.len()
            {
                skipped_triangles = skipped_triangles.wrapping_add(1);
                face_index += 1;
                continue;
            }

            packed_face_calls = packed_face_calls.wrapping_add(1);
            packed_unclamped_face_calls = packed_unclamped_face_calls.wrapping_add(1);
            let projected = unsafe {
                // SAFETY: each index was checked against `projected_vertices.len()`
                // immediately above. The slice is pretrimmed to `project_count`.
                [
                    *projected_vertices.get_unchecked(ia),
                    *projected_vertices.get_unchecked(ib),
                    *projected_vertices.get_unchecked(ic),
                ]
            };

            if projected_back_facing(projected) {
                culled_triangles = culled_triangles.wrapping_add(1);
                face_index += 1;
                continue;
            }

            if projected_triangle_preclamped_hw_extent_safe(projected) {
                match self.submit_projected_model_triangle_preclamped_packed_average_untracked(
                    triangles,
                    projected,
                    face.uv_words,
                    packet_material,
                    options,
                ) {
                    ModelTrianglePacketResult::Submitted => {
                        submitted_triangles = submitted_triangles.wrapping_add(1);
                        fast_submitted_triangles = fast_submitted_triangles.wrapping_add(1);
                    }
                    ModelTrianglePacketResult::CommandOverflow => {
                        flush_packed_unclamped_model_batch_stats(
                            stats,
                            skipped_triangles,
                            packed_face_calls,
                            packed_unclamped_face_calls,
                            culled_triangles,
                            submitted_triangles,
                            fast_submitted_triangles,
                            hw_extent_fallbacks,
                        );
                        stats.command_overflow = true;
                        return true;
                    }
                    ModelTrianglePacketResult::PrimitiveOverflow => {
                        flush_packed_unclamped_model_batch_stats(
                            stats,
                            skipped_triangles,
                            packed_face_calls,
                            packed_unclamped_face_calls,
                            culled_triangles,
                            submitted_triangles,
                            fast_submitted_triangles,
                            hw_extent_fallbacks,
                        );
                        stats.primitive_overflow = true;
                        return true;
                    }
                }
                face_index += 1;
                continue;
            }

            hw_extent_fallbacks = hw_extent_fallbacks.wrapping_add(1);
            flush_packed_unclamped_model_batch_stats(
                stats,
                skipped_triangles,
                packed_face_calls,
                packed_unclamped_face_calls,
                culled_triangles,
                submitted_triangles,
                fast_submitted_triangles,
                hw_extent_fallbacks,
            );
            skipped_triangles = 0;
            packed_face_calls = 0;
            packed_unclamped_face_calls = 0;
            culled_triangles = 0;
            submitted_triangles = 0;
            fast_submitted_triangles = 0;
            hw_extent_fallbacks = 0;

            let uvs = face.uvs();
            let textured = [
                ProjectedTexturedVertex::new(projected[0], uvs[0].0 as i32, uvs[0].1 as i32),
                ProjectedTexturedVertex::new(projected[1], uvs[1].0 as i32, uvs[1].1 as i32),
                ProjectedTexturedVertex::new(projected[2], uvs[2].0 as i32, uvs[2].1 as i32),
            ];
            let tri_stats =
                self.submit_textured_triangle_split(triangles, textured, material, options, 0);
            merge_textured_model_stats(stats, tri_stats);
            if stats.primitive_overflow || stats.command_overflow {
                return true;
            }

            face_index += 1;
        }

        flush_packed_unclamped_model_batch_stats(
            stats,
            skipped_triangles,
            packed_face_calls,
            packed_unclamped_face_calls,
            culled_triangles,
            submitted_triangles,
            fast_submitted_triangles,
            hw_extent_fallbacks,
        );
        false
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_face_packed_back_average_in_front_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        project_count: usize,
        face: TexturedModelRenderFace,
        packet_material: TexturedPacketMaterial,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        let ia = face.vertex_indices[0] as usize;
        let ib = face.vertex_indices[1] as usize;
        let ic = face.vertex_indices[2] as usize;
        if ia >= project_count || ib >= project_count || ic >= project_count {
            stats.skipped_triangles = stats.skipped_triangles.wrapping_add(1);
            return false;
        }
        stats.packed_face_calls = stats.packed_face_calls.wrapping_add(1);
        stats.packed_clamped_face_calls = stats.packed_clamped_face_calls.wrapping_add(1);
        let projected = [
            projected_vertices[ia],
            projected_vertices[ib],
            projected_vertices[ic],
        ];

        if projected_back_facing(projected) {
            stats.culled_triangles = stats.culled_triangles.wrapping_add(1);
            return false;
        }

        let projected = [
            clamp_projected_vertex(projected[0]),
            clamp_projected_vertex(projected[1]),
            clamp_projected_vertex(projected[2]),
        ];
        if projected_triangle_preclamped_hw_extent_safe(projected) {
            let before = stats.submitted_triangles;
            let overflow = self.submit_projected_model_triangle_preclamped_packed_average_fast(
                triangles,
                projected,
                face.uv_words,
                packet_material,
                options,
                stats,
            );
            stats.fast_submitted_triangles = stats
                .fast_submitted_triangles
                .wrapping_add(stats.submitted_triangles.wrapping_sub(before));
            return overflow;
        }

        stats.hw_extent_fallbacks = stats.hw_extent_fallbacks.wrapping_add(1);
        let uvs = face.uvs();
        let textured = [
            ProjectedTexturedVertex::new(projected[0], uvs[0].0 as i32, uvs[0].1 as i32),
            ProjectedTexturedVertex::new(projected[1], uvs[1].0 as i32, uvs[1].1 as i32),
            ProjectedTexturedVertex::new(projected[2], uvs[2].0 as i32, uvs[2].1 as i32),
        ];
        let tri_stats =
            self.submit_textured_triangle_split(triangles, textured, material, options, 0);
        merge_textured_model_stats(stats, tri_stats);
        stats.primitive_overflow || stats.command_overflow
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_face_packed_back_in_front_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        project_count: usize,
        face: TexturedModelRenderFace,
        packet_material: TexturedPacketMaterial,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        let ia = face.vertex_indices[0] as usize;
        let ib = face.vertex_indices[1] as usize;
        let ic = face.vertex_indices[2] as usize;
        if ia >= project_count || ib >= project_count || ic >= project_count {
            stats.skipped_triangles = stats.skipped_triangles.wrapping_add(1);
            return false;
        }
        stats.packed_face_calls = stats.packed_face_calls.wrapping_add(1);
        stats.packed_clamped_face_calls = stats.packed_clamped_face_calls.wrapping_add(1);
        let projected = [
            projected_vertices[ia],
            projected_vertices[ib],
            projected_vertices[ic],
        ];

        if projected_back_facing(projected) {
            stats.culled_triangles = stats.culled_triangles.wrapping_add(1);
            return false;
        }

        let projected = [
            clamp_projected_vertex(projected[0]),
            clamp_projected_vertex(projected[1]),
            clamp_projected_vertex(projected[2]),
        ];
        if projected_triangle_preclamped_hw_extent_safe(projected) {
            let before = stats.submitted_triangles;
            let overflow = self.submit_projected_model_triangle_preclamped_packed_fast(
                triangles,
                projected,
                face.uv_words,
                packet_material,
                options,
                stats,
            );
            stats.fast_submitted_triangles = stats
                .fast_submitted_triangles
                .wrapping_add(stats.submitted_triangles.wrapping_sub(before));
            return overflow;
        }

        stats.hw_extent_fallbacks = stats.hw_extent_fallbacks.wrapping_add(1);
        let uvs = face.uvs();
        let textured = [
            ProjectedTexturedVertex::new(projected[0], uvs[0].0 as i32, uvs[0].1 as i32),
            ProjectedTexturedVertex::new(projected[1], uvs[1].0 as i32, uvs[1].1 as i32),
            ProjectedTexturedVertex::new(projected[2], uvs[2].0 as i32, uvs[2].1 as i32),
        ];
        let tri_stats =
            self.submit_textured_triangle_split(triangles, textured, material, options, 0);
        merge_textured_model_stats(stats, tri_stats);
        stats.primitive_overflow || stats.command_overflow
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_face_packed_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        project_count: usize,
        face: TexturedModelRenderFace,
        all_projected_vertices_in_front: bool,
        near_z: i32,
        packet_material: TexturedPacketMaterial,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        let ia = face.vertex_indices[0] as usize;
        let ib = face.vertex_indices[1] as usize;
        let ic = face.vertex_indices[2] as usize;
        if ia >= project_count || ib >= project_count || ic >= project_count {
            stats.skipped_triangles = stats.skipped_triangles.wrapping_add(1);
            return false;
        }
        stats.packed_face_calls = stats.packed_face_calls.wrapping_add(1);
        stats.packed_general_face_calls = stats.packed_general_face_calls.wrapping_add(1);
        let projected = [
            projected_vertices[ia],
            projected_vertices[ib],
            projected_vertices[ic],
        ];

        if !all_projected_vertices_in_front && projected_model_face_crosses_near(projected, near_z)
        {
            stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
            stats.near_plane_dropped_faces = stats.near_plane_dropped_faces.wrapping_add(1);
            return false;
        }

        if projected_culled(projected, options.cull_mode) {
            stats.culled_triangles = stats.culled_triangles.wrapping_add(1);
            return false;
        }

        let projected = [
            clamp_projected_vertex(projected[0]),
            clamp_projected_vertex(projected[1]),
            clamp_projected_vertex(projected[2]),
        ];
        if projected_triangle_preclamped_hw_extent_safe(projected) {
            let before = stats.submitted_triangles;
            let overflow = self.submit_projected_model_triangle_preclamped_packed_fast(
                triangles,
                projected,
                face.uv_words,
                packet_material,
                options,
                stats,
            );
            stats.fast_submitted_triangles = stats
                .fast_submitted_triangles
                .wrapping_add(stats.submitted_triangles.wrapping_sub(before));
            return overflow;
        }

        stats.hw_extent_fallbacks = stats.hw_extent_fallbacks.wrapping_add(1);
        let uvs = face.uvs();
        let textured = [
            ProjectedTexturedVertex::new(projected[0], uvs[0].0 as i32, uvs[0].1 as i32),
            ProjectedTexturedVertex::new(projected[1], uvs[1].0 as i32, uvs[1].1 as i32),
            ProjectedTexturedVertex::new(projected[2], uvs[2].0 as i32, uvs[2].1 as i32),
        ];
        let tri_stats =
            self.submit_textured_triangle_split(triangles, textured, material, options, 0);
        merge_textured_model_stats(stats, tri_stats);
        stats.primitive_overflow || stats.command_overflow
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn submit_predecoded_model_face(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        projected_vertices: &[ProjectedVertex],
        project_count: usize,
        face: TexturedModelRenderFace,
        all_projected_vertices_in_front: bool,
        near_z: i32,
        packet_material: TexturedPacketMaterial,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        let ia = face.vertex_indices[0] as usize;
        let ib = face.vertex_indices[1] as usize;
        let ic = face.vertex_indices[2] as usize;
        if ia >= project_count || ib >= project_count || ic >= project_count {
            stats.skipped_triangles = stats.skipped_triangles.wrapping_add(1);
            return false;
        }
        stats.fallback_face_calls = stats.fallback_face_calls.wrapping_add(1);
        let projected = [
            projected_vertices[ia],
            projected_vertices[ib],
            projected_vertices[ic],
        ];

        if !all_projected_vertices_in_front && projected_model_face_crosses_near(projected, near_z)
        {
            stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
            stats.near_plane_dropped_faces = stats.near_plane_dropped_faces.wrapping_add(1);
            return false;
        }

        if projected_culled(projected, options.cull_mode) {
            stats.culled_triangles = stats.culled_triangles.wrapping_add(1);
            return false;
        }

        let projected = [
            clamp_projected_vertex(projected[0]),
            clamp_projected_vertex(projected[1]),
            clamp_projected_vertex(projected[2]),
        ];

        if options.split_textured_triangles {
            if options.textured_split_max_edge == 0
                && projected_triangle_preclamped_hw_extent_safe(projected)
            {
                let before = stats.submitted_triangles;
                let overflow = self.submit_projected_model_triangle_preclamped_packed_fast(
                    triangles,
                    projected,
                    face.uv_words,
                    packet_material,
                    options,
                    stats,
                );
                stats.fast_submitted_triangles = stats
                    .fast_submitted_triangles
                    .wrapping_add(stats.submitted_triangles.wrapping_sub(before));
                return overflow;
            }
            let uvs = face.uvs();
            let textured = [
                ProjectedTexturedVertex::new(projected[0], uvs[0].0 as i32, uvs[0].1 as i32),
                ProjectedTexturedVertex::new(projected[1], uvs[1].0 as i32, uvs[1].1 as i32),
                ProjectedTexturedVertex::new(projected[2], uvs[2].0 as i32, uvs[2].1 as i32),
            ];
            let tri_stats =
                self.submit_textured_triangle_split(triangles, textured, material, options, 0);
            merge_textured_model_stats(stats, tri_stats);
            stats.primitive_overflow || stats.command_overflow
        } else {
            self.submit_projected_model_triangle_preclamped_fast(
                triangles,
                projected,
                face.uvs(),
                material,
                options,
                stats,
            )
        }
    }

    #[inline(always)]
    fn submit_projected_model_triangle_preclamped_packed_average_untracked(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        material: TexturedPacketMaterial,
        options: WorldSurfaceOptions,
    ) -> ModelTrianglePacketResult {
        if self.command_len >= self.commands.len() {
            return ModelTrianglePacketResult::CommandOverflow;
        }

        let Some(tri) = triangles.push(TriTextured::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            material,
        )) else {
            return ModelTrianglePacketResult::PrimitiveOverflow;
        };

        let depth = CameraDepth::new(
            ((verts[0].sz + verts[1].sz + verts[2].sz) / 3).saturating_add(options.depth_bias),
        );
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            tri as *mut TriTextured as *mut u32,
            TriTextured::WORDS,
        );
        ModelTrianglePacketResult::Submitted
    }

    #[inline(always)]
    fn submit_projected_model_triangle_preclamped_packed_average_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        material: TexturedPacketMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return true;
        }

        let Some(tri) = triangles.push(TriTextured::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            material,
        )) else {
            stats.primitive_overflow = true;
            return true;
        };

        let depth = CameraDepth::new(
            ((verts[0].sz + verts[1].sz + verts[2].sz) / 3).saturating_add(options.depth_bias),
        );
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            tri as *mut TriTextured as *mut u32,
            TriTextured::WORDS,
        );
        stats.submitted_triangles = stats.submitted_triangles.wrapping_add(1);
        false
    }

    #[inline(always)]
    fn submit_projected_model_triangle_preclamped_packed_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        material: TexturedPacketMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return true;
        }

        let Some(tri) = triangles.push(TriTextured::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            material,
        )) else {
            stats.primitive_overflow = true;
            return true;
        };

        let depth = CameraDepth::new(
            options
                .depth_policy
                .depth_values(verts[0].sz, verts[1].sz, verts[2].sz)
                .saturating_add(options.depth_bias),
        );
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            tri as *mut TriTextured as *mut u32,
            TriTextured::WORDS,
        );
        stats.submitted_triangles = stats.submitted_triangles.wrapping_add(1);
        false
    }

    fn submit_projected_model_triangle_preclamped_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        stats: &mut TexturedModelRenderStats,
    ) -> bool {
        let verts = [
            clamp_projected_vertex(verts[0]),
            clamp_projected_vertex(verts[1]),
            clamp_projected_vertex(verts[2]),
        ];
        if !projected_triangle_hw_safe(verts) {
            stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
            stats.hw_unsafe_dropped_faces = stats.hw_unsafe_dropped_faces.wrapping_add(1);
            return false;
        }
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return true;
        }

        let (uv0, uv1, uv2) = (uvs[0], uvs[1], uvs[2]);
        let Some(tri) = triangles.push(TriTextured::with_material_packet_texcoords(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            [uv0, uv1, uv2],
            material,
        )) else {
            stats.primitive_overflow = true;
            return true;
        };

        let depth = CameraDepth::new(options.depth_policy.depth_values(
            verts[0].sz,
            verts[1].sz,
            verts[2].sz,
        ))
        .saturating_add(options.depth_bias);
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            tri as *mut TriTextured as *mut u32,
            TriTextured::WORDS,
        );
        stats.submitted_triangles = stats.submitted_triangles.wrapping_add(1);
        false
    }

    fn submit_clipped_textured_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [TexturedViewVertex; 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let Some(a) = projection.project_view(verts[0].position) else {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        };
        let Some(b) = projection.project_view(verts[1].position) else {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        };
        let Some(c) = projection.project_view(verts[2].position) else {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        };
        self.submit_textured_triangle(
            triangles,
            [a, b, c],
            [
                (clamp_u8(verts[0].u), clamp_u8(verts[0].v)),
                (clamp_u8(verts[1].u), clamp_u8(verts[1].v)),
                (clamp_u8(verts[2].u), clamp_u8(verts[2].v)),
            ],
            material,
            options,
        )
    }
}
