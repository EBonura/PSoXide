use super::*;

impl<'a, 'ot, const OT_DEPTH: usize> WorldRenderPass<'a, 'ot, OT_DEPTH> {
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
        if !projected_triangle_can_skip_split(first, options)
            || !projected_triangle_can_skip_split(second, options)
        {
            let mut stats = self.submit_textured_gouraud_triangle_prescreened_u8(
                primitives,
                first,
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                options,
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
                options,
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
    /// `fill` writes the full packet skeleton (opcode, texture window,
    /// UV/clut/tpage words) -- done once when the pool slot is claimed
    /// for a room; afterwards only the four vertex words and four
    /// colour words are rewritten per frame. In-place patching is safe
    /// because the present flip drains the ordering-table DMA before
    /// the next render touches any packet.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_prebuilt_textured_gouraud_quad(
        &mut self,
        quad: &mut QuadTexturedGouraud,
        fill: bool,
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
        if fill {
            *quad = QuadTexturedGouraud::with_packet_material_packed_uv_words(
                xy, uv_words, colors, material,
            );
        } else {
            quad.set_positions(xy);
            quad.set_colors(colors);
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
