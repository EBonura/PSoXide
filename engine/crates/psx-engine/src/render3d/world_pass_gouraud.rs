use super::*;

impl<'a, 'ot, const OT_DEPTH: usize> WorldRenderPass<'a, 'ot, OT_DEPTH> {
    /// Clip one textured, vertex-lit camera-space triangle at the near plane.
    /// Cached rooms use this only for the uncommon surface that straddles the
    /// camera; the ordinary projected-cache path stays allocation-free and
    /// avoids carrying a full second view-space arena.
    pub(crate) fn submit_textured_gouraud_view_triangle_uv_words(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        positions: [ViewVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let input = [
            TexturedGouraudViewVertex::new(positions[0], uv_words[0], colors[0]),
            TexturedGouraudViewVertex::new(positions[1], uv_words[1], colors[1]),
            TexturedGouraudViewVertex::new(positions[2], uv_words[2], colors[2]),
        ];
        let mut clipped = [TexturedGouraudViewVertex::ZERO; 4];
        let count = clip_textured_gouraud_triangle_to_near(input, projection.near_z, &mut clipped);
        let mut stats = WorldRenderStats::default();
        if count < 3 {
            stats.dropped_triangles = 1;
            return stats;
        }
        if count != 3
            || positions
                .iter()
                .any(|position| position.z < projection.near_z)
        {
            stats.clipped_triangles = 1;
        }

        let mut submit = |pass: &mut Self,
                          triangle: [TexturedGouraudViewVertex; 3],
                          stats: &mut WorldRenderStats| {
            let Some(a) = projection.project_view(triangle[0].position) else {
                stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
                return;
            };
            let Some(b) = projection.project_view(triangle[1].position) else {
                stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
                return;
            };
            let Some(c) = projection.project_view(triangle[2].position) else {
                stats.dropped_triangles = stats.dropped_triangles.wrapping_add(1);
                return;
            };
            let next = pass.submit_textured_gouraud_triangle(
                triangles,
                [a, b, c],
                [
                    (
                        triangle[0].u.clamp(0, 255) as u8,
                        triangle[0].v.clamp(0, 255) as u8,
                    ),
                    (
                        triangle[1].u.clamp(0, 255) as u8,
                        triangle[1].v.clamp(0, 255) as u8,
                    ),
                    (
                        triangle[2].u.clamp(0, 255) as u8,
                        triangle[2].v.clamp(0, 255) as u8,
                    ),
                ],
                [triangle[0].color, triangle[1].color, triangle[2].color],
                material,
                options,
            );
            merge_world_stats(stats, next);
        };
        submit(self, [clipped[0], clipped[1], clipped[2]], &mut stats);
        if !stats.primitive_overflow && !stats.command_overflow && count == 4 {
            submit(self, [clipped[0], clipped[2], clipped[3]], &mut stats);
        }
        stats
    }

    /// Submit a projected textured Gouraud triangle.
    ///
    /// This is the room/static-light path: callers CPU-project the
    /// world vertices once, compute one tint per vertex, then let the
    /// GPU interpolate that tint across the textured triangle.
    pub fn submit_textured_gouraud_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if projected_culled(verts, options.cull_mode) {
            stats.culled_triangles = 1;
            return stats;
        }

        let textured = [
            ProjectedTexturedGouraudVertex::new(
                verts[0],
                uvs[0].0 as i32,
                uvs[0].1 as i32,
                colors[0],
            ),
            ProjectedTexturedGouraudVertex::new(
                verts[1],
                uvs[1].0 as i32,
                uvs[1].1 as i32,
                colors[1],
            ),
            ProjectedTexturedGouraudVertex::new(
                verts[2],
                uvs[2].0 as i32,
                uvs[2].1 as i32,
                colors[2],
            ),
        ];
        if projected_triangle_can_skip_split(verts, options) {
            merge_world_stats(
                &mut stats,
                self.submit_textured_gouraud_triangle_leaf(triangles, textured, material, options),
            );
            return stats;
        }
        merge_world_stats(
            &mut stats,
            self.submit_textured_gouraud_triangle_split(triangles, textured, material, options, 0),
        );
        stats
    }

    /// Submit a projected textured Gouraud triangle after the caller has
    /// already applied the desired winding/cull policy.
    pub fn submit_textured_gouraud_triangle_prescreened(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let textured = [
            ProjectedTexturedGouraudVertex::new(
                verts[0],
                uvs[0].0 as i32,
                uvs[0].1 as i32,
                colors[0],
            ),
            ProjectedTexturedGouraudVertex::new(
                verts[1],
                uvs[1].0 as i32,
                uvs[1].1 as i32,
                colors[1],
            ),
            ProjectedTexturedGouraudVertex::new(
                verts[2],
                uvs[2].0 as i32,
                uvs[2].1 as i32,
                colors[2],
            ),
        ];
        if projected_triangle_can_skip_split(verts, options) {
            return self
                .submit_textured_gouraud_triangle_leaf(triangles, textured, material, options);
        }
        self.submit_textured_gouraud_triangle_split(triangles, textured, material, options, 0)
    }

    /// Submit a projected textured Gouraud triangle whose UVs are already
    /// clamped to packet-space bytes.
    ///
    /// Cached room surfaces store PS1-ready UVs, so the common hardware-safe
    /// case can skip the intermediate `ProjectedTexturedGouraudVertex`
    /// construction and per-component UV clamping used by the general path.
    #[inline(always)]
    pub fn submit_textured_gouraud_triangle_prescreened_u8(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        if projected_triangle_can_skip_split(verts, options) {
            return self.submit_textured_gouraud_triangle_leaf_u8(
                triangles, verts, uvs, colors, material, options,
            );
        }
        self.submit_textured_gouraud_triangle_prescreened(
            triangles, verts, uvs, colors, material, options,
        )
    }

    /// Submit a projected textured Gouraud quad as one GP0 quad packet when
    /// both underlying triangles are hardware-safe. Falls back to the normal
    /// two-triangle prescreened path for oversized quads.
    pub fn submit_textured_gouraud_quad_prescreened_u8<P>(
        &mut self,
        primitives: &mut P,
        verts: [ProjectedVertex; 4],
        uvs: [(u8, u8); 4],
        colors: [(u8, u8, u8); 4],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        let first = [verts[0], verts[1], verts[2]];
        let second = [verts[0], verts[2], verts[3]];
        // The GPU extent check is a hardware invariant, not an optional
        // quality knob. Force the splitter on for this decision even when a
        // caller disabled discretionary subdivision.
        let split_options = options.with_textured_triangle_splitting(true);
        let quad_extent_safe =
            options.textured_split_max_edge == 0 && projected_quad_bounds_hw_extent_safe(verts);
        if !quad_extent_safe
            && (!projected_triangle_can_skip_split(first, split_options)
                || !projected_triangle_can_skip_split(second, split_options))
        {
            let mut stats = self.submit_textured_gouraud_triangle_prescreened_u8(
                primitives,
                first,
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                split_options,
            );
            if stats.primitive_overflow || stats.command_overflow {
                return stats;
            }
            let second_stats = self.submit_textured_gouraud_triangle_prescreened_u8(
                primitives,
                second,
                [uvs[0], uvs[2], uvs[3]],
                [colors[0], colors[2], colors[3]],
                material,
                split_options,
            );
            merge_world_stats(&mut stats, second_stats);
            return stats;
        }

        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let depth_value = match options.depth_policy {
            DepthPolicy::Average => (verts[0].sz + verts[1].sz + verts[2].sz + verts[3].sz) / 4,
            DepthPolicy::Nearest => verts[0]
                .sz
                .min(verts[1].sz)
                .min(verts[2].sz)
                .min(verts[3].sz),
            DepthPolicy::Farthest => verts[0]
                .sz
                .max(verts[1].sz)
                .max(verts[2].sz)
                .max(verts[3].sz),
            DepthPolicy::Fixed(depth) => depth,
        };
        let depth = CameraDepth::new(depth_value.saturating_add(options.depth_bias));
        let slot = options
            .depth_band
            .slot_depth::<OT_DEPTH>(options.depth_range, depth);
        let packet_material = TexturedGouraudPacketMaterial::from_texture(material);
        let quad_verts = [verts[1], verts[0], verts[2], verts[3]];
        let quad_uvs = [uvs[1], uvs[0], uvs[2], uvs[3]];
        let quad_colors = [colors[1], colors[0], colors[2], colors[3]];
        let uv_words = [
            model_uv_word(quad_uvs[0]),
            model_uv_word(quad_uvs[1]),
            model_uv_word(quad_uvs[2]),
            model_uv_word(quad_uvs[3]),
        ];
        let Some(quad) =
            primitives.push(QuadTexturedGouraud::with_packet_material_packed_uv_words(
                [
                    (quad_verts[0].sx, quad_verts[0].sy),
                    (quad_verts[1].sx, quad_verts[1].sy),
                    (quad_verts[2].sx, quad_verts[2].sy),
                    (quad_verts[3].sx, quad_verts[3].sy),
                ],
                uv_words,
                quad_colors,
                packet_material,
            ))
        else {
            stats.primitive_overflow = true;
            return stats;
        };

        self.push_command(
            slot,
            depth.raw(),
            if packet_material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            quad as *mut QuadTexturedGouraud as *mut u32,
            QuadTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 2;
        stats
    }

    /// Submit a fixed-depth cached-room quad as one GP0(3Ch) packet when
    /// both hardware triangles are safe. Oversized packets are split through
    /// the normal triangle path so the real PS1 GPU cannot discard visible
    /// room geometry for exceeding its 1023x511 coordinate-delta limits.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth<P>(
        &mut self,
        primitives: &mut P,
        prebuilt: Option<(&mut QuadTexturedGouraud, &mut u8)>,
        prebuilt_colors_static: bool,
        verts: [ProjectedVertex; 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        // GP0(3Ch) uses tri(0,1,2) + tri(1,2,3). The caller has already
        // reordered the quad so that packet diagonal matches the authored
        // surface split.
        let first = [verts[0], verts[1], verts[2]];
        let second = [verts[1], verts[2], verts[3]];
        let split_options = options.with_textured_triangle_splitting(true);
        if !projected_triangle_can_skip_split(first, split_options)
            || !projected_triangle_can_skip_split(second, split_options)
        {
            let mut stats = self.submit_textured_gouraud_triangle_prescreened_uv_words(
                primitives,
                first,
                [uv_words[0], uv_words[1], uv_words[2]],
                [colors[0], colors[1], colors[2]],
                material,
                split_options,
            );
            if stats.primitive_overflow || stats.command_overflow {
                return stats;
            }
            let second_stats = self.submit_textured_gouraud_triangle_prescreened_uv_words(
                primitives,
                second,
                [uv_words[1], uv_words[2], uv_words[3]],
                [colors[1], colors[2], colors[3]],
                material,
                split_options,
            );
            merge_world_stats(&mut stats, second_stats);
            return stats;
        }

        let packet_material = material.textured_gouraud_packet_material();
        if let Some((quad, valid)) = prebuilt {
            return self.submit_prebuilt_textured_gouraud_quad(
                quad,
                valid,
                prebuilt_colors_static,
                verts,
                uv_words,
                colors,
                packet_material,
                options,
                prepared_depth,
            );
        }
        self.submit_textured_gouraud_quad_leaf_uv_words_prepared_depth(
            primitives,
            verts,
            uv_words,
            colors,
            packet_material,
            options,
            prepared_depth,
        )
    }

    /// Submit a projected textured Gouraud triangle with fixed depth already
    /// prepared by the caller.
    ///
    /// Cached room rendering sorts by tile cell, so depth mapping is shared by
    /// all triangles in the cell. This keeps the normal hardware-extent guard
    /// and splitter fallback while avoiding repeated depth-key work on the
    /// common hardware-safe leaf path.
    #[cfg(not(feature = "room-surface-profile"))]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_u8_prepared_depth(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        if projected_triangle_can_skip_split(verts, options) {
            return self.submit_textured_gouraud_triangle_leaf_u8_prepared_depth(
                triangles,
                verts,
                uvs,
                colors,
                material,
                options,
                prepared_depth,
            );
        }
        self.submit_textured_gouraud_triangle_prescreened(
            triangles, verts, uvs, colors, material, options,
        )
    }

    /// Submit a cached room triangle whose UVs are already packed as
    /// packet low words.
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_uv_words(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        if projected_triangle_can_skip_split(verts, options) {
            return self.submit_textured_gouraud_triangle_leaf_uv_words(
                triangles, verts, uv_words, colors, material, options,
            );
        }
        self.submit_textured_gouraud_triangle_prescreened(
            triangles,
            verts,
            packet_uv_words_to_pairs(uv_words),
            colors,
            material,
            options,
        )
    }

    /// Submit a cached room triangle with packed UV words and a caller-
    /// prepared fixed depth.
    #[cfg(not(feature = "room-surface-profile"))]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_uv_words_prepared_depth(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        if projected_triangle_can_skip_split(verts, options) {
            return self.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth(
                triangles,
                verts,
                uv_words,
                colors,
                material.textured_gouraud_packet_material(),
                options,
                prepared_depth,
            );
        }
        self.submit_textured_gouraud_triangle_prescreened(
            triangles,
            verts,
            packet_uv_words_to_pairs(uv_words),
            colors,
            material,
            options,
        )
    }

    /// Profiled variant of [`submit_textured_gouraud_triangle_prescreened_u8`].
    #[cfg(feature = "room-surface-profile")]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_u8_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let hw_safe_start = TexturedGouraudSubmitMicroProfile::cycle();
        let hardware_safe = projected_triangle_can_skip_split(verts, options);
        profile.add_hw_safe_test(TexturedGouraudSubmitMicroProfile::elapsed(hw_safe_start));
        if hardware_safe {
            profile.count_hw_safe();
            return self.submit_textured_gouraud_triangle_leaf_u8_profiled(
                triangles, verts, uvs, colors, material, options, profile,
            );
        }

        profile.count_fallback();
        let fallback_start = TexturedGouraudSubmitMicroProfile::cycle();
        let stats = self.submit_textured_gouraud_triangle_prescreened(
            triangles, verts, uvs, colors, material, options,
        );
        profile.add_fallback(TexturedGouraudSubmitMicroProfile::elapsed(fallback_start));
        stats
    }

    /// Profiled variant of
    /// [`submit_textured_gouraud_triangle_prescreened_u8_prepared_depth`].
    #[cfg(feature = "room-surface-profile")]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_u8_prepared_depth_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let hw_safe_start = TexturedGouraudSubmitMicroProfile::cycle();
        let hardware_safe = projected_triangle_can_skip_split(verts, options);
        profile.add_hw_safe_test(TexturedGouraudSubmitMicroProfile::elapsed(hw_safe_start));
        if hardware_safe {
            profile.count_hw_safe();
            return self.submit_textured_gouraud_triangle_leaf_u8_prepared_depth_profiled(
                triangles,
                verts,
                uvs,
                colors,
                material,
                options,
                prepared_depth,
                profile,
            );
        }

        profile.count_fallback();
        let fallback_start = TexturedGouraudSubmitMicroProfile::cycle();
        let stats = self.submit_textured_gouraud_triangle_prescreened(
            triangles, verts, uvs, colors, material, options,
        );
        profile.add_fallback(TexturedGouraudSubmitMicroProfile::elapsed(fallback_start));
        stats
    }

    /// Profiled variant of
    /// [`submit_textured_gouraud_triangle_prescreened_uv_words`].
    #[cfg(feature = "room-surface-profile")]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_uv_words_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let hw_safe_start = TexturedGouraudSubmitMicroProfile::cycle();
        let hardware_safe = projected_triangle_can_skip_split(verts, options);
        profile.add_hw_safe_test(TexturedGouraudSubmitMicroProfile::elapsed(hw_safe_start));
        if hardware_safe {
            profile.count_hw_safe();
            return self.submit_textured_gouraud_triangle_leaf_uv_words_profiled(
                triangles, verts, uv_words, colors, material, options, profile,
            );
        }

        profile.count_fallback();
        let fallback_start = TexturedGouraudSubmitMicroProfile::cycle();
        let stats = self.submit_textured_gouraud_triangle_prescreened(
            triangles,
            verts,
            packet_uv_words_to_pairs(uv_words),
            colors,
            material,
            options,
        );
        profile.add_fallback(TexturedGouraudSubmitMicroProfile::elapsed(fallback_start));
        stats
    }

    /// Profiled variant of
    /// [`submit_textured_gouraud_triangle_prescreened_uv_words_prepared_depth`].
    #[cfg(feature = "room-surface-profile")]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_prescreened_uv_words_prepared_depth_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let hw_safe_start = TexturedGouraudSubmitMicroProfile::cycle();
        let hardware_safe = projected_triangle_can_skip_split(verts, options);
        profile.add_hw_safe_test(TexturedGouraudSubmitMicroProfile::elapsed(hw_safe_start));
        if hardware_safe {
            profile.count_hw_safe();
            return self.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth_profiled(
                triangles,
                verts,
                uv_words,
                colors,
                material.textured_gouraud_packet_material(),
                options,
                prepared_depth,
                profile,
            );
        }

        profile.count_fallback();
        let fallback_start = TexturedGouraudSubmitMicroProfile::cycle();
        let stats = self.submit_textured_gouraud_triangle_prescreened(
            triangles,
            verts,
            packet_uv_words_to_pairs(uv_words),
            colors,
            material,
            options,
        );
        profile.add_fallback(TexturedGouraudSubmitMicroProfile::elapsed(fallback_start));
        stats
    }

    fn submit_textured_gouraud_triangle_split(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedTexturedGouraudVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        split_depth: u8,
    ) -> WorldRenderStats {
        if !options.split_textured_triangles {
            return self.submit_textured_gouraud_triangle_leaf(triangles, verts, material, options);
        }

        let verts = [
            clamp_projected_textured_gouraud_vertex(verts[0]),
            clamp_projected_textured_gouraud_vertex(verts[1]),
            clamp_projected_textured_gouraud_vertex(verts[2]),
        ];

        let needs_split = projected_textured_gouraud_needs_split(verts, options);
        if needs_split && split_depth < MAX_TEXTURED_HW_SPLIT_DEPTH {
            return self.submit_split_textured_gouraud_triangle(
                triangles,
                verts,
                material,
                options,
                split_depth,
            );
        }
        if projected_textured_gouraud_exceeds_hw_extent(verts) {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        }

        self.submit_textured_gouraud_triangle_leaf(triangles, verts, material, options)
    }

    fn submit_split_textured_gouraud_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedTexturedGouraudVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        split_depth: u8,
    ) -> WorldRenderStats {
        let edge = largest_projected_gouraud_edge(verts);
        let mut stats = WorldRenderStats {
            split_triangles: 1,
            ..WorldRenderStats::default()
        };

        let (first, second) = match edge {
            0 => {
                let mid = midpoint_projected_textured_gouraud(verts[0], verts[1]);
                ([verts[0], mid, verts[2]], [mid, verts[1], verts[2]])
            }
            1 => {
                let mid = midpoint_projected_textured_gouraud(verts[1], verts[2]);
                ([verts[0], verts[1], mid], [verts[0], mid, verts[2]])
            }
            _ => {
                let mid = midpoint_projected_textured_gouraud(verts[2], verts[0]);
                ([verts[0], verts[1], mid], [mid, verts[1], verts[2]])
            }
        };

        let first_stats = self.submit_textured_gouraud_triangle_split(
            triangles,
            first,
            material,
            options,
            split_depth + 1,
        );
        merge_world_stats(&mut stats, first_stats);
        if stats.primitive_overflow || stats.command_overflow {
            return stats;
        }

        let second_stats = self.submit_textured_gouraud_triangle_split(
            triangles,
            second,
            material,
            options,
            split_depth + 1,
        );
        merge_world_stats(&mut stats, second_stats);
        stats
    }

    fn submit_textured_gouraud_triangle_leaf(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedTexturedGouraudVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let uv0 = (clamp_u8(verts[0].u), clamp_u8(verts[0].v));
        let uv1 = (clamp_u8(verts[1].u), clamp_u8(verts[1].v));
        let uv2 = (clamp_u8(verts[2].u), clamp_u8(verts[2].v));
        let Some(tri) = triangles.push(TriTexturedGouraud::with_material_packet_texcoords(
            [
                (verts[0].projected.sx, verts[0].projected.sy),
                (verts[1].projected.sx, verts[1].projected.sy),
                (verts[2].projected.sx, verts[2].projected.sy),
            ],
            [uv0, uv1, uv2],
            [verts[0].color, verts[1].color, verts[2].color],
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };

        let depth = CameraDepth::new(
            options
                .depth_policy
                .depth_values(
                    verts[0].projected.sz,
                    verts[1].projected.sz,
                    verts[2].projected.sz,
                )
                .saturating_add(options.depth_bias),
        );
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    #[inline(always)]
    fn submit_textured_gouraud_triangle_leaf_u8(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(tri) = triangles.push(TriTexturedGouraud::with_material_packet_texcoords(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uvs,
            colors,
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
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
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    #[inline(always)]
    #[allow(dead_code)]
    fn submit_textured_gouraud_triangle_leaf_uv_words(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(tri) = triangles.push(TriTexturedGouraud::with_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            colors,
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
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
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a cached room triangle using a caller-prepared fixed depth.
    ///
    /// This path intentionally skips the projected hardware-extent check used
    /// by the general prescreened submitter. Cooked cached-room cells are
    /// already clipped/projected by the caller, and the normal demo3 cache
    /// profiling shows this path never falls back to CPU triangle splitting.
    #[cfg(any(not(feature = "room-surface-profile"), test))]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_leaf_u8_prepared_depth(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(tri) = triangles.push(TriTexturedGouraud::with_material_packet_texcoords(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uvs,
            colors,
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };

        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a cached room triangle with packed UV words and a caller-
    /// prepared fixed depth.
    #[cfg(any(not(feature = "room-surface-profile"), test))]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TexturedGouraudPacketMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(tri) = triangles.push(TriTexturedGouraud::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            colors,
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };

        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a cached room quad as a single GP0(3Ch) textured-Gouraud
    /// quad with a caller-prepared fixed depth, instead of two
    /// [`TriTexturedGouraud`] leaves.
    ///
    /// `verts`/`uv_words`/`colors` are in **GP0 packet order** -- the
    /// hardware rasterizes the quad as `tri(v0,v1,v2)+tri(v1,v2,v3)`
    /// (the `1`-`2` diagonal). The caller is responsible for ordering
    /// the four corners so this hardware split lands on the engine's
    /// chosen diagonal, which makes the output pixel-identical to the
    /// two-triangle submission (proved by
    /// `textured_gouraud_quad_matches_two_triangle_split_bitexact`).
    pub(crate) fn submit_textured_gouraud_quad_leaf_uv_words_prepared_depth(
        &mut self,
        quads: &mut impl PrimitiveSink<QuadTexturedGouraud>,
        verts: [ProjectedVertex; 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TexturedGouraudPacketMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(quad) = quads.push(QuadTexturedGouraud::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
                (verts[3].sx, verts[3].sy),
            ],
            uv_words,
            colors,
            material,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };

        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            quad as *mut QuadTexturedGouraud as *mut u32,
            QuadTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a PREBUILT textured Gouraud quad from a caller-owned
    /// static pool instead of the per-frame arena.
    ///
    /// `valid` is this surface's pool validity byte: zero means the
    /// packet skeleton has never been written for the room currently
    /// owning the pool slot, so the FULL packet is constructed here
    /// (and the byte set); afterwards only the vertex and colour words
    /// are rewritten per frame. A packet is therefore pushed only by
    /// the call that constructed or patched it -- a surface culled on
    /// earlier frames simply takes the constructor on its first
    /// visible frame. In-place patching is safe behind the present
    /// flip's DMA drain.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(crate) fn submit_prebuilt_textured_gouraud_quad(
        &mut self,
        quad: &mut QuadTexturedGouraud,
        valid: &mut u8,
        colors_static: bool,
        verts: [ProjectedVertex; 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TexturedGouraudPacketMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }
        let xy = [
            (verts[0].sx, verts[0].sy),
            (verts[1].sx, verts[1].sy),
            (verts[2].sx, verts[2].sy),
            (verts[3].sx, verts[3].sy),
        ];
        if *valid == 0 {
            *quad = QuadTexturedGouraud::with_packet_material_packed_uv_words(
                xy, uv_words, colors, material,
            );
            *valid = 1;
        } else {
            quad.set_positions(xy);
            if !colors_static {
                quad.set_colors(colors);
            }
        }
        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            quad as *mut QuadTexturedGouraud as *mut u32,
            QuadTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Patch and queue a warmed static room packet without rebuilding its
    /// immutable material, UV, or colour words. Returns `None` when the quad
    /// must use the normal hardware-extent fallback.
    #[inline(always)]
    pub(crate) fn try_submit_warmed_textured_gouraud_quad(
        &mut self,
        quad: &mut QuadTexturedGouraud,
        verts: [ProjectedVertex; 4],
        extent_safe: bool,
        material: TexturedGouraudPacketMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> Option<WorldRenderStats> {
        if !extent_safe {
            let split_options = options.with_textured_triangle_splitting(true);
            if !projected_triangle_can_skip_split([verts[0], verts[1], verts[2]], split_options)
                || !projected_triangle_can_skip_split([verts[1], verts[2], verts[3]], split_options)
            {
                return None;
            }
        }

        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return Some(stats);
        }
        quad.set_positions([
            (verts[0].sx, verts[0].sy),
            (verts[1].sx, verts[1].sy),
            (verts[2].sx, verts[2].sy),
            (verts[3].sx, verts[3].sy),
        ]);
        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            quad as *mut QuadTexturedGouraud as *mut u32,
            QuadTexturedGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        Some(stats)
    }

    #[cfg(feature = "room-surface-profile")]
    #[allow(dead_code)]
    #[inline(always)]
    fn submit_textured_gouraud_triangle_leaf_u8_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            profile.count_command_overflow();
            return stats;
        }

        let packet_start = TexturedGouraudSubmitMicroProfile::cycle();
        let packet = TriTexturedGouraud::with_material_packet_texcoords(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uvs,
            colors,
            material,
        );
        profile.add_packet_fill(TexturedGouraudSubmitMicroProfile::elapsed(packet_start));

        let push_start = TexturedGouraudSubmitMicroProfile::cycle();
        let Some(tri) = triangles.push(packet) else {
            profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));
            stats.primitive_overflow = true;
            profile.count_primitive_overflow();
            return stats;
        };
        profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));

        let depth_start = TexturedGouraudSubmitMicroProfile::cycle();
        let depth = CameraDepth::new(
            options
                .depth_policy
                .depth_values(verts[0].sz, verts[1].sz, verts[2].sz)
                .saturating_add(options.depth_bias),
        );
        let slot = options
            .depth_band
            .slot_depth::<OT_DEPTH>(options.depth_range, depth);
        let render_layer = if material.is_translucent() {
            WorldRenderLayer::Transparent
        } else {
            options.render_layer
        };
        profile.add_depth(TexturedGouraudSubmitMicroProfile::elapsed(depth_start));

        let command_start = TexturedGouraudSubmitMicroProfile::cycle();
        self.push_command(
            slot,
            depth.raw(),
            render_layer,
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        profile.add_command(TexturedGouraudSubmitMicroProfile::elapsed(command_start));
        stats.submitted_triangles = 1;
        stats
    }

    #[cfg(feature = "room-surface-profile")]
    #[allow(dead_code)]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_leaf_u8_prepared_depth_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            profile.count_command_overflow();
            return stats;
        }

        let packet_start = TexturedGouraudSubmitMicroProfile::cycle();
        let packet = TriTexturedGouraud::with_material_packet_texcoords(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uvs,
            colors,
            material,
        );
        profile.add_packet_fill(TexturedGouraudSubmitMicroProfile::elapsed(packet_start));

        let push_start = TexturedGouraudSubmitMicroProfile::cycle();
        let Some(tri) = triangles.push(packet) else {
            profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));
            stats.primitive_overflow = true;
            profile.count_primitive_overflow();
            return stats;
        };
        profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));

        let command_start = TexturedGouraudSubmitMicroProfile::cycle();
        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        profile.add_command(TexturedGouraudSubmitMicroProfile::elapsed(command_start));
        stats.submitted_triangles = 1;
        stats
    }

    #[cfg(feature = "room-surface-profile")]
    #[inline(always)]
    fn submit_textured_gouraud_triangle_leaf_uv_words_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            profile.count_command_overflow();
            return stats;
        }

        let packet_start = TexturedGouraudSubmitMicroProfile::cycle();
        let packet = TriTexturedGouraud::with_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            colors,
            material,
        );
        profile.add_packet_fill(TexturedGouraudSubmitMicroProfile::elapsed(packet_start));

        let push_start = TexturedGouraudSubmitMicroProfile::cycle();
        let Some(tri) = triangles.push(packet) else {
            profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));
            stats.primitive_overflow = true;
            profile.count_primitive_overflow();
            return stats;
        };
        profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));

        let depth_start = TexturedGouraudSubmitMicroProfile::cycle();
        let depth = CameraDepth::new(
            options
                .depth_policy
                .depth_values(verts[0].sz, verts[1].sz, verts[2].sz)
                .saturating_add(options.depth_bias),
        );
        let slot = options
            .depth_band
            .slot_depth::<OT_DEPTH>(options.depth_range, depth);
        let render_layer = if material.is_translucent() {
            WorldRenderLayer::Transparent
        } else {
            options.render_layer
        };
        profile.add_depth(TexturedGouraudSubmitMicroProfile::elapsed(depth_start));

        let command_start = TexturedGouraudSubmitMicroProfile::cycle();
        self.push_command(
            slot,
            depth.raw(),
            render_layer,
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        profile.add_command(TexturedGouraudSubmitMicroProfile::elapsed(command_start));
        stats.submitted_triangles = 1;
        stats
    }

    #[cfg(feature = "room-surface-profile")]
    #[inline(always)]
    pub(crate) fn submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth_profiled(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        verts: [ProjectedVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TexturedGouraudPacketMaterial,
        options: WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
        profile: &mut TexturedGouraudSubmitMicroProfile,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            profile.count_command_overflow();
            return stats;
        }

        let packet_start = TexturedGouraudSubmitMicroProfile::cycle();
        let packet = TriTexturedGouraud::with_packet_material_packed_uv_words(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            uv_words,
            colors,
            material,
        );
        profile.add_packet_fill(TexturedGouraudSubmitMicroProfile::elapsed(packet_start));

        let push_start = TexturedGouraudSubmitMicroProfile::cycle();
        let Some(tri) = triangles.push(packet) else {
            profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));
            stats.primitive_overflow = true;
            profile.count_primitive_overflow();
            return stats;
        };
        profile.add_primitive_push(TexturedGouraudSubmitMicroProfile::elapsed(push_start));

        let command_start = TexturedGouraudSubmitMicroProfile::cycle();
        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if material.is_translucent() {
                WorldRenderLayer::Transparent
            } else {
                options.render_layer
            },
            tri as *mut TriTexturedGouraud as *mut u32,
            TriTexturedGouraud::WORDS,
        );
        profile.add_command(TexturedGouraudSubmitMicroProfile::elapsed(command_start));
        stats.submitted_triangles = 1;
        stats
    }
}
