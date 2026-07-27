use super::*;

impl<'a, 'ot, const OT_DEPTH: usize> WorldRenderPass<'a, 'ot, OT_DEPTH> {
    /// Submit one camera-space room triangle using Tomb Raider's bounded
    /// bounded subdivision schedule.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_tomb_raider_textured_gouraud_view_triangle_uv_words(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        positions: [ViewVertex; 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
    ) -> WorldRenderStats {
        load_tomb_raider_view_projection_gte(projection);
        let vertices = [
            TexturedGouraudViewVertex::new(positions[0], uv_words[0], colors[0]),
            TexturedGouraudViewVertex::new(positions[1], uv_words[1], colors[1]),
            TexturedGouraudViewVertex::new(positions[2], uv_words[2], colors[2]),
        ];
        let leaf_options = (*options)
            .with_tomb_raider_subdivision(false)
            .with_textured_triangle_max_edge(0);
        let subdivision_profile = options.tomb_raider_subdivision_profile;
        let root_depth = tomb_raider_triangle_farthest_depth(&vertices);
        if root_depth >= subdivision_profile.far_depth {
            return self.submit_tomb_raider_textured_gouraud_view_triangle_leaf(
                triangles,
                &vertices,
                projection,
                material,
                &leaf_options,
                0,
            );
        }
        let mut stats = self.submit_tomb_raider_textured_gouraud_view_triangle_split(
            triangles,
            &vertices,
            projection,
            material,
            &leaf_options,
            0,
        );
        if !stats.primitive_overflow
            && !stats.command_overflow
            && !material.is_translucent()
            && root_depth >= subdivision_profile.underdraw_depth
        {
            let underdraw_options = leaf_options.with_depth_bias(
                leaf_options
                    .depth_bias
                    .saturating_add(subdivision_profile.underdraw_depth_bias),
            );
            let underdraw = self.submit_tomb_raider_textured_gouraud_view_triangle_leaf(
                triangles,
                &vertices,
                projection,
                material,
                &underdraw_options,
                3,
            );
            merge_world_stats(&mut stats, underdraw);
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_view_triangle_split(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        vertices: &[TexturedGouraudViewVertex; 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
        split_level: u8,
    ) -> WorldRenderStats {
        let edge_01 = midpoint_textured_gouraud_view(vertices[0], vertices[1]);
        let edge_12 = midpoint_textured_gouraud_view(vertices[1], vertices[2]);
        let edge_20 = midpoint_textured_gouraud_view(vertices[2], vertices[0]);
        let children = [
            [vertices[0], edge_01, edge_20],
            [edge_01, vertices[1], edge_12],
            [edge_20, edge_12, vertices[2]],
            [edge_01, edge_12, edge_20],
        ];
        let mut stats = WorldRenderStats {
            split_triangles: 1,
            ..WorldRenderStats::default()
        };
        let mut index = 0usize;
        while index < children.len() {
            let child = &children[index];
            let next = if options.tomb_raider_subdivision_profile.max_levels
                > split_level.saturating_add(1)
                && tomb_raider_triangle_farthest_depth(child)
                    < options.tomb_raider_subdivision_profile.near_depth
            {
                self.submit_tomb_raider_textured_gouraud_view_triangle_split(
                    triangles, child, projection, material, options, 1,
                )
            } else {
                self.submit_tomb_raider_textured_gouraud_view_triangle_leaf(
                    triangles,
                    child,
                    projection,
                    material,
                    options,
                    split_level.saturating_add(1),
                )
            };
            merge_world_stats(&mut stats, next);
            if stats.primitive_overflow || stats.command_overflow {
                break;
            }
            index += 1;
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_view_triangle_leaf(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
        vertices: &[TexturedGouraudViewVertex; 3],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
        subdivision_level: u8,
    ) -> WorldRenderStats {
        let debug_color = options
            .tomb_raider_debug_subdivision_levels
            .then(|| tomb_raider_debug_subdivision_color(subdivision_level))
            .flatten();
        if vertices
            .iter()
            .any(|vertex| vertex.position.z < projection.near_z)
        {
            return self.submit_textured_gouraud_view_triangle_uv_words(
                triangles,
                [
                    vertices[0].position,
                    vertices[1].position,
                    vertices[2].position,
                ],
                [
                    textured_gouraud_view_uv_word(vertices[0]),
                    textured_gouraud_view_uv_word(vertices[1]),
                    textured_gouraud_view_uv_word(vertices[2]),
                ],
                [
                    debug_color.unwrap_or(vertices[0].color),
                    debug_color.unwrap_or(vertices[1].color),
                    debug_color.unwrap_or(vertices[2].color),
                ],
                projection,
                material,
                *options,
            );
        }
        let Some([a, b, c]) = project_tomb_raider_view_triangle_gte(
            [
                vertices[0].position,
                vertices[1].position,
                vertices[2].position,
            ],
            projection,
        ) else {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        };
        self.submit_textured_gouraud_triangle(
            triangles,
            [a, b, c],
            [
                (
                    vertices[0].u.clamp(0, 255) as u8,
                    vertices[0].v.clamp(0, 255) as u8,
                ),
                (
                    vertices[1].u.clamp(0, 255) as u8,
                    vertices[1].v.clamp(0, 255) as u8,
                ),
                (
                    vertices[2].u.clamp(0, 255) as u8,
                    vertices[2].v.clamp(0, 255) as u8,
                ),
            ],
            [
                debug_color.unwrap_or(vertices[0].color),
                debug_color.unwrap_or(vertices[1].color),
                debug_color.unwrap_or(vertices[2].color),
            ],
            material,
            *options,
        )
    }

    /// Submit one camera-space room quad using the bounded
    /// subdivision schedule from the later PS1 Tomb Raider renderer.
    ///
    /// The important detail is that generated positions remain in camera
    /// space until each leaf is projected. Splitting the already-projected
    /// polygon would preserve the original affine texture plane and therefore
    /// would not provide Tomb Raider's piecewise-perspective correction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_tomb_raider_textured_gouraud_view_quad_uv_words<P>(
        &mut self,
        primitives: &mut P,
        positions: [ViewVertex; 4],
        _root_projected: Option<[ProjectedVertex; 4]>,
        _root_extent_safe: bool,
        mut _warmed_root: Option<&mut QuadTexturedGouraud>,
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        load_tomb_raider_view_projection_gte(projection);
        let vertices = [
            TexturedGouraudViewVertex::new(positions[0], uv_words[0], colors[0]),
            TexturedGouraudViewVertex::new(positions[1], uv_words[1], colors[1]),
            TexturedGouraudViewVertex::new(positions[2], uv_words[2], colors[2]),
            TexturedGouraudViewVertex::new(positions[3], uv_words[3], colors[3]),
        ];
        let leaf_options = (*options)
            .with_tomb_raider_subdivision(false)
            .with_textured_triangle_max_edge(0);
        let subdivision_profile = options.tomb_raider_subdivision_profile;
        let root_depth = tomb_raider_quad_farthest_depth(&vertices);
        if root_depth >= subdivision_profile.far_depth {
            return self.submit_tomb_raider_textured_gouraud_view_quad_leaf(
                primitives,
                &vertices,
                projection,
                material,
                &leaf_options,
                0,
            );
        }
        #[cfg(feature = "tr-subdivision-lattice")]
        if subdivision_profile.max_levels == 1 {
            let mut stats = self.submit_tomb_raider_textured_gouraud_view_quad_lattice(
                primitives,
                &vertices,
                _root_projected,
                _root_extent_safe,
                projection,
                material,
                &leaf_options,
            );
            if !stats.primitive_overflow
                && !stats.command_overflow
                && !material.is_translucent()
                && root_depth >= subdivision_profile.underdraw_depth
            {
                let underdraw_options = leaf_options.with_depth_bias(
                    leaf_options
                        .depth_bias
                        .saturating_add(subdivision_profile.underdraw_depth_bias),
                );
                let underdraw = if let Some(root_projected) = _root_projected {
                    if _root_extent_safe && !underdraw_options.tomb_raider_debug_subdivision_levels
                    {
                        if let Some(quad) = _warmed_root.as_deref_mut() {
                            let prepared_depth = PreparedTriangleDepth::from_quad_average::<OT_DEPTH>(
                                underdraw_options,
                                root_projected,
                            );
                            self.try_submit_warmed_textured_gouraud_quad(
                                quad,
                                root_projected,
                                true,
                                &underdraw_options,
                                prepared_depth,
                            )
                            .unwrap_or_default()
                        } else {
                            self.submit_tomb_raider_textured_gouraud_projected_quad_leaf(
                                primitives,
                                &vertices,
                                root_projected,
                                material,
                                &underdraw_options,
                                true,
                                3,
                            )
                        }
                    } else {
                        self.submit_tomb_raider_textured_gouraud_projected_quad_leaf(
                            primitives,
                            &vertices,
                            root_projected,
                            material,
                            &underdraw_options,
                            _root_extent_safe,
                            3,
                        )
                    }
                } else {
                    self.submit_tomb_raider_textured_gouraud_view_quad_leaf(
                        primitives,
                        &vertices,
                        projection,
                        material,
                        &underdraw_options,
                        3,
                    )
                };
                merge_world_stats(&mut stats, underdraw);
            }
            return stats;
        }
        let mut stats = self.submit_tomb_raider_textured_gouraud_view_quad_split(
            primitives,
            &vertices,
            projection,
            material,
            &leaf_options,
            0,
        );
        if !stats.primitive_overflow
            && !stats.command_overflow
            && !material.is_translucent()
            && root_depth >= subdivision_profile.underdraw_depth
        {
            let underdraw_options = leaf_options.with_depth_bias(
                leaf_options
                    .depth_bias
                    .saturating_add(subdivision_profile.underdraw_depth_bias),
            );
            let underdraw = self.submit_tomb_raider_textured_gouraud_view_quad_leaf(
                primitives,
                &vertices,
                projection,
                material,
                &underdraw_options,
                3,
            );
            merge_world_stats(&mut stats, underdraw);
        }
        stats
    }

    /// Fixed-topology one-level quad split. The nine shared points are
    /// projected once and addressed through a constant four-leaf table.
    #[cfg(feature = "tr-subdivision-lattice")]
    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_view_quad_lattice<P>(
        &mut self,
        primitives: &mut P,
        vertices: &[TexturedGouraudViewVertex; 4],
        root_projected: Option<[ProjectedVertex; 4]>,
        root_extent_safe: bool,
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        // Packet/layout order is 0--1 / 2--3.
        let top = midpoint_textured_gouraud_view(vertices[0], vertices[1]);
        let left = midpoint_textured_gouraud_view(vertices[0], vertices[2]);
        let right = midpoint_textured_gouraud_view(vertices[1], vertices[3]);
        let bottom = midpoint_textured_gouraud_view(vertices[2], vertices[3]);
        let center = midpoint_textured_gouraud_view(top, bottom);
        let lattice = [
            vertices[0],
            top,
            vertices[1],
            left,
            center,
            right,
            vertices[2],
            bottom,
            vertices[3],
        ];
        let Some(projected) = project_tomb_raider_view_lattice_gte(
            [
                lattice[0].position,
                lattice[1].position,
                lattice[2].position,
                lattice[3].position,
                lattice[4].position,
                lattice[5].position,
                lattice[6].position,
                lattice[7].position,
                lattice[8].position,
            ],
            projection,
            root_projected,
        ) else {
            // Preserve the existing near-plane clipping behavior for the
            // uncommon quad that crosses the camera.
            return self.submit_tomb_raider_textured_gouraud_view_quad_split(
                primitives, vertices, projection, material, options, 0,
            );
        };
        const LEAVES: [[usize; 4]; 4] = [[0, 1, 3, 4], [1, 2, 4, 5], [3, 4, 6, 7], [4, 5, 7, 8]];
        let mut stats = WorldRenderStats {
            split_triangles: 1,
            ..WorldRenderStats::default()
        };
        let mut leaf_index = 0usize;
        while leaf_index < LEAVES.len() {
            let ids = LEAVES[leaf_index];
            let leaf_vertices = [
                lattice[ids[0]],
                lattice[ids[1]],
                lattice[ids[2]],
                lattice[ids[3]],
            ];
            let next = self.submit_tomb_raider_textured_gouraud_projected_quad_leaf(
                primitives,
                &leaf_vertices,
                [
                    projected[ids[0]],
                    projected[ids[1]],
                    projected[ids[2]],
                    projected[ids[3]],
                ],
                material,
                options,
                root_extent_safe,
                1,
            );
            merge_world_stats(&mut stats, next);
            if stats.primitive_overflow || stats.command_overflow {
                break;
            }
            leaf_index += 1;
        }
        stats
    }

    #[cfg(feature = "tr-subdivision-lattice")]
    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_projected_quad_leaf<P>(
        &mut self,
        primitives: &mut P,
        vertices: &[TexturedGouraudViewVertex; 4],
        projected: [ProjectedVertex; 4],
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
        root_extent_safe: bool,
        subdivision_level: u8,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        let debug_color = options
            .tomb_raider_debug_subdivision_levels
            .then(|| tomb_raider_debug_subdivision_color(subdivision_level))
            .flatten();
        let [a, b, c, _d] = projected;
        if projected_culled([a, b, c], options.cull_mode) {
            return WorldRenderStats {
                culled_triangles: 1,
                ..WorldRenderStats::default()
            };
        }
        let prepared_depth =
            PreparedTriangleDepth::from_quad_average::<OT_DEPTH>(*options, projected);
        if root_extent_safe {
            return self.submit_textured_gouraud_quad_leaf_uv_words_prepared_depth(
                primitives,
                projected,
                [
                    textured_gouraud_view_uv_word(vertices[0]),
                    textured_gouraud_view_uv_word(vertices[1]),
                    textured_gouraud_view_uv_word(vertices[2]),
                    textured_gouraud_view_uv_word(vertices[3]),
                ],
                [
                    debug_color.unwrap_or(vertices[0].color),
                    debug_color.unwrap_or(vertices[1].color),
                    debug_color.unwrap_or(vertices[2].color),
                    debug_color.unwrap_or(vertices[3].color),
                ],
                material.textured_gouraud_packet_material(),
                options,
                prepared_depth,
            );
        }
        self.submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth(
            primitives,
            None,
            false,
            0,
            projected,
            [
                textured_gouraud_view_uv_word(vertices[0]),
                textured_gouraud_view_uv_word(vertices[1]),
                textured_gouraud_view_uv_word(vertices[2]),
                textured_gouraud_view_uv_word(vertices[3]),
            ],
            [
                debug_color.unwrap_or(vertices[0].color),
                debug_color.unwrap_or(vertices[1].color),
                debug_color.unwrap_or(vertices[2].color),
                debug_color.unwrap_or(vertices[3].color),
            ],
            material,
            options,
            prepared_depth,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_view_quad_split<P>(
        &mut self,
        primitives: &mut P,
        vertices: &[TexturedGouraudViewVertex; 4],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
        split_level: u8,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        // Packet/layout order is 0--1 / 2--3. Generate the four edge
        // midpoints plus the centre exactly once, then form four child quads.
        let top = midpoint_textured_gouraud_view(vertices[0], vertices[1]);
        let left = midpoint_textured_gouraud_view(vertices[0], vertices[2]);
        let right = midpoint_textured_gouraud_view(vertices[1], vertices[3]);
        let bottom = midpoint_textured_gouraud_view(vertices[2], vertices[3]);
        let center = midpoint_textured_gouraud_view(top, bottom);
        let children = [
            [vertices[0], top, left, center],
            [top, vertices[1], center, right],
            [left, center, vertices[2], bottom],
            [center, right, bottom, vertices[3]],
        ];
        let mut stats = WorldRenderStats {
            split_triangles: 1,
            ..WorldRenderStats::default()
        };
        let mut index = 0usize;
        while index < children.len() {
            let child = &children[index];
            let next = if options.tomb_raider_subdivision_profile.max_levels
                > split_level.saturating_add(1)
                && tomb_raider_quad_farthest_depth(child)
                    < options.tomb_raider_subdivision_profile.near_depth
            {
                self.submit_tomb_raider_textured_gouraud_view_quad_split(
                    primitives, child, projection, material, options, 1,
                )
            } else {
                self.submit_tomb_raider_textured_gouraud_view_quad_leaf(
                    primitives,
                    child,
                    projection,
                    material,
                    options,
                    split_level.saturating_add(1),
                )
            };
            merge_world_stats(&mut stats, next);
            if stats.primitive_overflow || stats.command_overflow {
                break;
            }
            index += 1;
        }
        stats
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_tomb_raider_textured_gouraud_view_quad_leaf<P>(
        &mut self,
        primitives: &mut P,
        vertices: &[TexturedGouraudViewVertex; 4],
        projection: WorldProjection,
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
        subdivision_level: u8,
    ) -> WorldRenderStats
    where
        P: PrimitiveSink<QuadTexturedGouraud> + PrimitiveSink<TriTexturedGouraud>,
    {
        let debug_color = options
            .tomb_raider_debug_subdivision_levels
            .then(|| tomb_raider_debug_subdivision_color(subdivision_level))
            .flatten();
        if vertices
            .iter()
            .any(|vertex| vertex.position.z < projection.near_z)
        {
            let mut stats = self.submit_textured_gouraud_view_triangle_uv_words(
                primitives,
                [
                    vertices[0].position,
                    vertices[1].position,
                    vertices[2].position,
                ],
                [
                    textured_gouraud_view_uv_word(vertices[0]),
                    textured_gouraud_view_uv_word(vertices[1]),
                    textured_gouraud_view_uv_word(vertices[2]),
                ],
                [
                    debug_color.unwrap_or(vertices[0].color),
                    debug_color.unwrap_or(vertices[1].color),
                    debug_color.unwrap_or(vertices[2].color),
                ],
                projection,
                material,
                *options,
            );
            if !stats.primitive_overflow && !stats.command_overflow {
                let next = self.submit_textured_gouraud_view_triangle_uv_words(
                    primitives,
                    [
                        vertices[1].position,
                        vertices[3].position,
                        vertices[2].position,
                    ],
                    [
                        textured_gouraud_view_uv_word(vertices[1]),
                        textured_gouraud_view_uv_word(vertices[3]),
                        textured_gouraud_view_uv_word(vertices[2]),
                    ],
                    [
                        debug_color.unwrap_or(vertices[1].color),
                        debug_color.unwrap_or(vertices[3].color),
                        debug_color.unwrap_or(vertices[2].color),
                    ],
                    projection,
                    material,
                    *options,
                );
                merge_world_stats(&mut stats, next);
            }
            return stats;
        }

        let Some(projected) = project_tomb_raider_view_quad_gte(
            [
                vertices[0].position,
                vertices[1].position,
                vertices[2].position,
                vertices[3].position,
            ],
            projection,
        ) else {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        };
        let [a, b, c, _d] = projected;
        if projected_culled([a, b, c], options.cull_mode) {
            return WorldRenderStats {
                culled_triangles: 1,
                ..WorldRenderStats::default()
            };
        }
        let prepared_depth =
            PreparedTriangleDepth::from_quad_average::<OT_DEPTH>(*options, projected);
        self.submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth(
            primitives,
            None,
            false,
            0,
            projected,
            [
                textured_gouraud_view_uv_word(vertices[0]),
                textured_gouraud_view_uv_word(vertices[1]),
                textured_gouraud_view_uv_word(vertices[2]),
                textured_gouraud_view_uv_word(vertices[3]),
            ],
            [
                debug_color.unwrap_or(vertices[0].color),
                debug_color.unwrap_or(vertices[1].color),
                debug_color.unwrap_or(vertices[2].color),
                debug_color.unwrap_or(vertices[3].color),
            ],
            material,
            options,
            prepared_depth,
        )
    }

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
        verts: &[ProjectedVertex; 4],
        uvs: &[(u8, u8); 4],
        colors: &[(u8, u8, u8); 4],
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
        prebuilt_ready_value: u8,
        verts: [ProjectedVertex; 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TextureMaterial,
        options: &WorldSurfaceOptions,
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
        let split_options = (*options).with_textured_triangle_splitting(true);
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
                prebuilt_ready_value,
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
        options: &WorldSurfaceOptions,
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
        ready_value: u8,
        verts: [ProjectedVertex; 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TexturedGouraudPacketMaterial,
        options: &WorldSurfaceOptions,
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
            *valid = ready_value.max(1);
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
        options: &WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> Option<WorldRenderStats> {
        if !extent_safe {
            let split_options = (*options).with_textured_triangle_splitting(true);
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
            if quad.color0_cmd & 0x0200_0000 != 0 {
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

    /// Patch dynamic colours into an otherwise prewarmed room packet.
    #[inline(always)]
    pub(crate) fn try_submit_warmed_textured_gouraud_quad_with_colors(
        &mut self,
        quad: &mut QuadTexturedGouraud,
        verts: [ProjectedVertex; 4],
        colors: [(u8, u8, u8); 4],
        extent_safe: bool,
        options: &WorldSurfaceOptions,
        prepared_depth: PreparedTriangleDepth,
    ) -> Option<WorldRenderStats> {
        if !extent_safe {
            let split_options = (*options).with_textured_triangle_splitting(true);
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
        quad.set_colors(colors);
        self.push_command(
            prepared_depth.slot,
            prepared_depth.depth,
            if quad.color0_cmd & 0x0200_0000 != 0 {
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

/// High-contrast packet colors consumed directly by emulator wireframe mode.
pub(super) const fn tomb_raider_debug_subdivision_color(level: u8) -> Option<(u8, u8, u8)> {
    match level {
        1 => Some((0, 255, 255)),
        2 => Some((255, 0, 255)),
        3 => Some((255, 255, 0)),
        _ => None,
    }
}

#[cfg(all(test, feature = "tr-subdivision-lattice"))]
mod lattice_tests {
    use super::*;
    use psx_gpu::ot::OrderingTable;

    #[test]
    fn one_level_lattice_packets_match_recursive_reference_bitexact() {
        let projection = WorldProjection::new(160, 120, 256, 16);
        let material = TextureMaterial::opaque(2, 4, (128, 128, 128));
        let options = WorldSurfaceOptions::new(DepthBand::whole(), DepthRange::new(16, 8192))
            .with_cull_mode(CullMode::None)
            .with_tomb_raider_subdivision_sector_size(1664)
            .with_tomb_raider_subdivision_max_levels(1)
            .with_tomb_raider_subdivision(false)
            .with_textured_triangle_max_edge(0);
        let vertices = [
            TexturedGouraudViewVertex::new(
                ViewVertex::new(-900, -480, 2800),
                model_uv_word((2, 4)),
                (40, 60, 80),
            ),
            TexturedGouraudViewVertex::new(
                ViewVertex::new(760, -360, 5100),
                model_uv_word((61, 7)),
                (90, 110, 130),
            ),
            TexturedGouraudViewVertex::new(
                ViewVertex::new(-820, 620, 3300),
                model_uv_word((5, 58)),
                (140, 160, 180),
            ),
            TexturedGouraudViewVertex::new(
                ViewVertex::new(840, 540, 5600),
                model_uv_word((59, 62)),
                (190, 210, 230),
            ),
        ];

        let mut lattice_ot_storage = OrderingTable::<64>::new();
        let mut lattice_ot = OtFrame::begin(&mut lattice_ot_storage);
        let mut lattice_scratch = crate::PrimitivePacketScratch::<8>::ZERO;
        let mut lattice_packets = crate::PrimitivePacketArena::new(&mut lattice_scratch);
        let mut lattice_commands = [WorldTriCommand::EMPTY; 8];
        load_tomb_raider_view_projection_gte(projection);
        let lattice_stats = {
            let mut pass = WorldRenderPass::new(&mut lattice_ot, &mut lattice_commands);
            pass.submit_tomb_raider_textured_gouraud_view_quad_lattice(
                &mut lattice_packets,
                &vertices,
                None,
                false,
                projection,
                material,
                &options,
            )
        };

        let mut reference_ot_storage = OrderingTable::<64>::new();
        let mut reference_ot = OtFrame::begin(&mut reference_ot_storage);
        let mut reference_scratch = crate::PrimitivePacketScratch::<8>::ZERO;
        let mut reference_packets = crate::PrimitivePacketArena::new(&mut reference_scratch);
        let mut reference_commands = [WorldTriCommand::EMPTY; 8];
        load_tomb_raider_view_projection_gte(projection);
        let reference_stats = {
            let mut pass = WorldRenderPass::new(&mut reference_ot, &mut reference_commands);
            pass.submit_tomb_raider_textured_gouraud_view_quad_split(
                &mut reference_packets,
                &vertices,
                projection,
                material,
                &options,
                0,
            )
        };

        assert_eq!(lattice_packets.len(), 4);
        assert_eq!(lattice_packets.len(), reference_packets.len());
        assert_eq!(lattice_stats, reference_stats);
        let packet_bytes = core::mem::size_of::<QuadTexturedGouraud>();
        for index in 0..lattice_packets.len() {
            let lattice_command = lattice_commands[index];
            let reference_command = reference_commands[index];
            assert_eq!(lattice_command.slot, reference_command.slot);
            assert_eq!(lattice_command.depth, reference_command.depth);
            assert_eq!(lattice_command.order, reference_command.order);
            assert_eq!(lattice_command.render_layer, reference_command.render_layer);
            assert_eq!(lattice_command.words, reference_command.words);
            let lattice = unsafe {
                core::slice::from_raw_parts(lattice_command.packet_ptr.cast::<u8>(), packet_bytes)
            };
            let reference = unsafe {
                core::slice::from_raw_parts(reference_command.packet_ptr.cast::<u8>(), packet_bytes)
            };
            assert_eq!(lattice, reference, "leaf packet {index} differs");
        }
    }
}
