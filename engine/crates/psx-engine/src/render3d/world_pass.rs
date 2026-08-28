use super::*;
use psx_gpu::prim::LineMono;

impl<'a, 'ot, const OT_DEPTH: usize> WorldRenderPass<'a, 'ot, OT_DEPTH> {
    /// Start a world render pass.
    pub fn new(ot: &'a mut OtFrame<'ot, OT_DEPTH>, commands: &'a mut [WorldTriCommand]) -> Self {
        Self {
            ot,
            commands,
            slot_heads: MaybeUninit::new([WORLD_COMMAND_NONE; OT_DEPTH]),
            slot_tails: MaybeUninit::uninit(),
            command_len: 0,
            next_order: 0,
            ordering: WorldCommandOrdering::LinkedSorted,
        }
    }

    /// Start a world render pass that sorts submitted commands at flush.
    ///
    /// This keeps painter ordering comparable to [`Self::new`] while avoiding
    /// the hot per-triangle linked-list insertion cost. It is the preferred
    /// mode for scenes that submit a few hundred opaque world/model packets.
    pub fn new_deferred_sorted(
        ot: &'a mut OtFrame<'ot, OT_DEPTH>,
        commands: &'a mut [WorldTriCommand],
    ) -> Self {
        Self {
            ot,
            commands,
            slot_heads: MaybeUninit::uninit(),
            slot_tails: MaybeUninit::uninit(),
            command_len: 0,
            next_order: 0,
            ordering: WorldCommandOrdering::DeferredSorted,
        }
    }

    /// Start a world render pass that appends commands into OT buckets and
    /// sorts only within each occupied bucket at flush.
    ///
    /// This preserves the same exact same-slot depth/layer/order semantics as
    /// the global deferred sorter, but avoids comparing triangles that already
    /// landed in different ordering-table slots.
    pub fn new_deferred_slot_sorted(
        ot: &'a mut OtFrame<'ot, OT_DEPTH>,
        commands: &'a mut [WorldTriCommand],
    ) -> Self {
        Self {
            ot,
            commands,
            slot_heads: MaybeUninit::new([WORLD_COMMAND_NONE; OT_DEPTH]),
            slot_tails: MaybeUninit::new([WORLD_COMMAND_NONE; OT_DEPTH]),
            command_len: 0,
            next_order: 0,
            ordering: WorldCommandOrdering::DeferredSlotSorted,
        }
    }

    /// Start a world render pass that appends commands into coarse OT buckets.
    ///
    /// This is the fastest ordered mode: it preserves submission order within
    /// each depth slot and relies on a sufficiently deep OT for depth
    /// separation. It avoids both per-command insertion sorting and frame-end
    /// global sorting.
    pub fn new_bucketed(
        ot: &'a mut OtFrame<'ot, OT_DEPTH>,
        commands: &'a mut [WorldTriCommand],
    ) -> Self {
        debug_assert!(
            core::mem::size_of::<BucketedWorldCommand>() <= core::mem::size_of::<WorldTriCommand>()
        );
        debug_assert!(
            core::mem::align_of::<BucketedWorldCommand>()
                <= core::mem::align_of::<WorldTriCommand>()
        );
        Self {
            ot,
            commands,
            slot_heads: MaybeUninit::uninit(),
            slot_tails: MaybeUninit::uninit(),
            command_len: 0,
            next_order: 0,
            ordering: WorldCommandOrdering::Bucketed,
        }
    }

    /// Number of world/model triangle commands queued in this pass.
    pub const fn command_len(&self) -> usize {
        self.command_len
    }

    #[inline(always)]
    pub(super) fn slot_heads(&self) -> &[u16; OT_DEPTH] {
        debug_assert!(self.ordering.uses_slot_heads());
        // SAFETY: constructors initialize `slot_heads` exactly for ordering
        // modes that access per-slot linked lists.
        unsafe { self.slot_heads.assume_init_ref() }
    }

    #[inline(always)]
    fn slot_heads_mut(&mut self) -> &mut [u16; OT_DEPTH] {
        debug_assert!(self.ordering.uses_slot_heads());
        // SAFETY: constructors initialize `slot_heads` exactly for ordering
        // modes that access per-slot linked lists.
        unsafe { self.slot_heads.assume_init_mut() }
    }

    #[inline(always)]
    pub(super) fn slot_tails(&self) -> &[u16; OT_DEPTH] {
        debug_assert!(self.ordering.uses_slot_tails());
        // SAFETY: constructors initialize `slot_tails` exactly for ordering
        // modes that append commands into per-slot linked lists.
        unsafe { self.slot_tails.assume_init_ref() }
    }

    #[inline(always)]
    fn slot_tails_mut(&mut self) -> &mut [u16; OT_DEPTH] {
        debug_assert!(self.ordering.uses_slot_tails());
        // SAFETY: constructors initialize `slot_tails` exactly for ordering
        // modes that append commands into per-slot linked lists.
        unsafe { self.slot_tails.assume_init_mut() }
    }

    /// Submit a projected textured triangle.
    pub fn submit_textured_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 3],
        uvs: [(u8, u8); 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if projected_culled(verts, options.cull_mode) {
            stats.culled_triangles = 1;
            return stats;
        }

        let textured = [
            ProjectedTexturedVertex::new(verts[0], uvs[0].0 as i32, uvs[0].1 as i32),
            ProjectedTexturedVertex::new(verts[1], uvs[1].0 as i32, uvs[1].1 as i32),
            ProjectedTexturedVertex::new(verts[2], uvs[2].0 as i32, uvs[2].1 as i32),
        ];
        merge_world_stats(
            &mut stats,
            self.submit_textured_triangle_split(triangles, textured, material, options, 0),
        );
        stats
    }

    /// Submit a textured triangle whose vertices are already projected.
    ///
    /// This is the common packet path for pre-projected surfaces and
    /// GTE-projected model/world batches. Projection is intentionally
    /// kept outside so the expensive part can happen once per shared
    /// vertex rather than once per face.
    pub fn submit_projected_textured_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedTexturedVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        let projected = [verts[0].projected, verts[1].projected, verts[2].projected];
        if projected_culled(projected, options.cull_mode) {
            stats.culled_triangles = 1;
            return stats;
        }

        merge_world_stats(
            &mut stats,
            self.submit_textured_triangle_split(triangles, verts, material, options, 0),
        );
        stats
    }

    pub(super) fn submit_textured_triangle_split(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedTexturedVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        split_depth: u8,
    ) -> WorldRenderStats {
        if !options.split_textured_triangles {
            return self.submit_textured_triangle_leaf(triangles, verts, material, options);
        }

        let verts = [
            clamp_projected_textured_vertex(verts[0]),
            clamp_projected_textured_vertex(verts[1]),
            clamp_projected_textured_vertex(verts[2]),
        ];

        let needs_split = projected_textured_needs_split(verts, options);
        if needs_split && split_depth < MAX_TEXTURED_HW_SPLIT_DEPTH {
            return self.submit_split_textured_triangle(
                triangles,
                verts,
                material,
                options,
                split_depth,
            );
        }
        if projected_textured_exceeds_hw_extent(verts) {
            return WorldRenderStats {
                dropped_triangles: 1,
                ..WorldRenderStats::default()
            };
        }

        self.submit_textured_triangle_leaf(triangles, verts, material, options)
    }

    #[cfg(test)]
    pub(super) fn submit_textured_triangle_split_leaf_fast(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedTexturedVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> Option<WorldRenderStats> {
        if !options.split_textured_triangles {
            return Some(self.submit_textured_triangle_leaf(triangles, verts, material, options));
        }

        let verts = [
            clamp_projected_textured_vertex(verts[0]),
            clamp_projected_textured_vertex(verts[1]),
            clamp_projected_textured_vertex(verts[2]),
        ];

        if projected_textured_needs_split(verts, options) {
            return None;
        }

        Some(self.submit_textured_triangle_leaf(triangles, verts, material, options))
    }

    fn submit_split_textured_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedTexturedVertex; 3],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
        split_depth: u8,
    ) -> WorldRenderStats {
        let edge = largest_projected_edge(verts);
        let mut stats = WorldRenderStats {
            split_triangles: 1,
            ..WorldRenderStats::default()
        };

        let (first, second) = match edge {
            0 => {
                let mid = midpoint_projected_textured(verts[0], verts[1]);
                ([verts[0], mid, verts[2]], [mid, verts[1], verts[2]])
            }
            1 => {
                let mid = midpoint_projected_textured(verts[1], verts[2]);
                ([verts[0], verts[1], mid], [verts[0], mid, verts[2]])
            }
            _ => {
                let mid = midpoint_projected_textured(verts[2], verts[0]);
                ([verts[0], verts[1], mid], [mid, verts[1], verts[2]])
            }
        };

        let first_stats = self.submit_textured_triangle_split(
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

        let second_stats = self.submit_textured_triangle_split(
            triangles,
            second,
            material,
            options,
            split_depth + 1,
        );
        merge_world_stats(&mut stats, second_stats);
        stats
    }

    fn submit_textured_triangle_leaf(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedTexturedVertex; 3],
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
        let Some(tri) = triangles.push(TriTextured::with_material_packet_texcoords(
            [
                (verts[0].projected.sx, verts[0].projected.sy),
                (verts[1].projected.sx, verts[1].projected.sy),
                (verts[2].projected.sx, verts[2].projected.sy),
            ],
            [uv0, uv1, uv2],
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
            tri as *mut TriTextured as *mut u32,
            TriTextured::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a projected textured quad as two independently culled
    /// and sorted textured triangles.
    ///
    /// Corners arrive in perimeter order `[0, 1, 2, 3]`. Triangles
    /// are split along the `0`–`2` diagonal -- see
    /// [`TEXTURED_QUAD_TRIANGLES`] for why the alternate split is
    /// wrong.
    pub fn submit_textured_quad(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        verts: [ProjectedVertex; 4],
        uvs: [(u8, u8); 4],
        material: TextureMaterial,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let [a, b, c] = TEXTURED_QUAD_TRIANGLES[0];
        let mut stats = self.submit_textured_triangle(
            triangles,
            [verts[a], verts[b], verts[c]],
            [uvs[a], uvs[b], uvs[c]],
            material,
            options,
        );
        if stats.primitive_overflow || stats.command_overflow {
            return stats;
        }

        let [a, b, c] = TEXTURED_QUAD_TRIANGLES[1];
        let second = self.submit_textured_triangle(
            triangles,
            [verts[a], verts[b], verts[c]],
            [uvs[a], uvs[b], uvs[c]],
            material,
            options,
        );
        stats.submitted_triangles = stats
            .submitted_triangles
            .saturating_add(second.submitted_triangles);
        stats.culled_triangles = stats
            .culled_triangles
            .saturating_add(second.culled_triangles);
        stats.primitive_overflow |= second.primitive_overflow;
        stats.command_overflow |= second.command_overflow;
        stats
    }

    /// Submit a Gouraud triangle packet already projected and lit.
    pub fn submit_gouraud_triangle(
        &mut self,
        triangles: &mut impl PrimitiveSink<TriGouraud>,
        verts: [ProjectedLit; 3],
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        let projected = [
            ProjectedVertex::from(verts[0]),
            ProjectedVertex::from(verts[1]),
            ProjectedVertex::from(verts[2]),
        ];
        if projected_culled(projected, options.cull_mode) {
            stats.culled_triangles = 1;
            return stats;
        }

        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }

        let Some(tri) = triangles.push(TriGouraud::new(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
            ],
            [
                (verts[0].r, verts[0].g, verts[0].b),
                (verts[1].r, verts[1].g, verts[1].b),
                (verts[2].r, verts[2].g, verts[2].b),
            ],
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };

        let depth =
            CameraDepth::new(options.depth_policy.depth(verts)).saturating_add(options.depth_bias);
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            tri as *mut TriGouraud as *mut u32,
            TriGouraud::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    /// Submit a projected Gouraud quad using one of the PS1 GPU's native
    /// semi-transparency equations.
    ///
    /// The packet includes its own GP0(E1) draw mode, so it remains correct
    /// when interleaved with textured world/model packets in the ordering
    /// table. One quad accounts for the two hardware triangles it rasterises.
    pub fn submit_blended_gouraud_quad(
        &mut self,
        quads: &mut impl PrimitiveSink<QuadGouraudBlended>,
        verts: [ProjectedLit; 4],
        blend_mode: BlendMode,
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        let projected = [
            ProjectedVertex::from(verts[0]),
            ProjectedVertex::from(verts[1]),
            ProjectedVertex::from(verts[2]),
            ProjectedVertex::from(verts[3]),
        ];
        if projected_culled(
            [projected[0], projected[1], projected[2]],
            options.cull_mode,
        ) && projected_culled(
            [projected[1], projected[2], projected[3]],
            options.cull_mode,
        ) {
            stats.culled_triangles = 2;
            return stats;
        }
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }
        let Some(quad) = quads.push(QuadGouraudBlended::new(
            [
                (verts[0].sx, verts[0].sy),
                (verts[1].sx, verts[1].sy),
                (verts[2].sx, verts[2].sy),
                (verts[3].sx, verts[3].sy),
            ],
            [
                (verts[0].r, verts[0].g, verts[0].b),
                (verts[1].r, verts[1].g, verts[1].b),
                (verts[2].r, verts[2].g, verts[2].b),
                (verts[3].r, verts[3].g, verts[3].b),
            ],
            blend_mode,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };
        let depth = CameraDepth::new(options.depth_policy.depth([verts[0], verts[1], verts[2]]))
            .saturating_add(options.depth_bias);
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            WorldRenderLayer::Transparent,
            quad as *mut QuadGouraudBlended as *mut u32,
            QuadGouraudBlended::WORDS,
        );
        stats.submitted_triangles = 2;
        stats
    }

    /// Submit a monochrome line between two already-projected vertices.
    ///
    /// The GPU's own line rasteriser (GP0 0x40), not a thin quad: three data
    /// words, no texture, no culling (a line has no facing). Depth is the
    /// nearer end, so a wireframe over solid geometry sorts in front of it.
    pub fn submit_projected_line(
        &mut self,
        lines: &mut impl PrimitiveSink<LineMono>,
        verts: [ProjectedVertex; 2],
        color: (u8, u8, u8),
        options: WorldSurfaceOptions,
    ) -> WorldRenderStats {
        let mut stats = WorldRenderStats::default();
        if self.command_len >= self.commands.len() {
            stats.command_overflow = true;
            return stats;
        }
        let Some(line) = lines.push(LineMono::new(
            verts[0].sx,
            verts[0].sy,
            verts[1].sx,
            verts[1].sy,
            color.0,
            color.1,
            color.2,
        )) else {
            stats.primitive_overflow = true;
            return stats;
        };
        let depth = CameraDepth::new(
            verts[0]
                .sz
                .min(verts[1].sz)
                .saturating_add(options.depth_bias),
        );
        self.push_command(
            options
                .depth_band
                .slot_depth::<OT_DEPTH>(options.depth_range, depth),
            depth.raw(),
            options.render_layer,
            line as *mut LineMono as *mut u32,
            LineMono::WORDS,
        );
        stats.submitted_triangles = 1;
        stats
    }

    #[inline(always)]
    pub(super) fn push_command(
        &mut self,
        slot: DepthSlot,
        depth: i32,
        render_layer: WorldRenderLayer,
        packet_ptr: *mut u32,
        words: u8,
    ) {
        let command_index = self.command_len;
        debug_assert!(command_index < WORLD_COMMAND_NONE as usize);
        if self.ordering == WorldCommandOrdering::Bucketed {
            // SAFETY: new_bucketed verifies layout/alignment and this index is
            // bounded by the WorldTriCommand slice length, whose elements are
            // at least as large as BucketedWorldCommand. Bucketed mode never
            // reads the same storage through WorldTriCommand.
            unsafe {
                self.commands
                    .as_mut_ptr()
                    .cast::<BucketedWorldCommand>()
                    .add(command_index)
                    .write(BucketedWorldCommand::new(packet_ptr, slot.index(), words));
            }
            self.command_len += 1;
            return;
        }

        self.commands[command_index] = WorldTriCommand {
            packet_ptr,
            depth,
            slot: slot.index().min(u16::MAX as usize) as u16,
            order: self.next_order,
            next: WORLD_COMMAND_NONE,
            render_layer: world_render_layer_code(render_layer),
            words,
        };
        self.command_len += 1;
        self.next_order = self.next_order.wrapping_add(1);
        match self.ordering {
            WorldCommandOrdering::LinkedSorted => self.insert_command_in_slot(command_index),
            WorldCommandOrdering::DeferredSorted => {}
            WorldCommandOrdering::DeferredSlotSorted => self.append_command_in_slot(command_index),
            WorldCommandOrdering::Bucketed => unreachable!(),
        }
    }

    fn append_command_in_slot(&mut self, command_index: usize) {
        if OT_DEPTH == 0 || command_index >= WORLD_COMMAND_NONE as usize {
            return;
        }

        let slot = self.commands[command_index].slot as usize;
        debug_assert!(slot < OT_DEPTH);
        let command_link = command_index as u16;
        let tail = self.slot_tails()[slot];
        if tail == WORLD_COMMAND_NONE {
            self.slot_heads_mut()[slot] = command_link;
        } else {
            self.commands[tail as usize].next = command_link;
        }
        self.slot_tails_mut()[slot] = command_link;
    }

    fn insert_command_in_slot(&mut self, command_index: usize) {
        if OT_DEPTH == 0 || command_index >= WORLD_COMMAND_NONE as usize {
            return;
        }

        let slot = self.commands[command_index].slot as usize;
        debug_assert!(slot < OT_DEPTH);
        let command_link = command_index as u16;
        let head = self.slot_heads()[slot];
        if head == WORLD_COMMAND_NONE
            || should_insert_world_before(
                self.commands[command_index],
                self.commands[head as usize],
            )
        {
            self.commands[command_index].next = head;
            self.slot_heads_mut()[slot] = command_link;
            return;
        }

        let mut prev = head as usize;
        loop {
            let next = self.commands[prev].next;
            if next == WORLD_COMMAND_NONE
                || should_insert_world_before(
                    self.commands[command_index],
                    self.commands[next as usize],
                )
            {
                self.commands[command_index].next = next;
                self.commands[prev].next = command_link;
                return;
            }
            prev = next as usize;
        }
    }

    fn sort_slot_links(&mut self) {
        let mut slot = 0;
        while slot < OT_DEPTH {
            let head = self.slot_heads()[slot];
            let sorted = self.merge_sort_slot_links(head);
            self.slot_heads_mut()[slot] = sorted;
            self.slot_tails_mut()[slot] = WORLD_COMMAND_NONE;
            slot += 1;
        }
    }

    fn merge_sort_slot_links(&mut self, head: u16) -> u16 {
        if head == WORLD_COMMAND_NONE {
            return head;
        }
        let next = self.commands[head as usize].next;
        if next == WORLD_COMMAND_NONE {
            return head;
        }

        let mid = self.split_slot_links(head);
        let left = self.merge_sort_slot_links(head);
        let right = self.merge_sort_slot_links(mid);
        self.merge_sorted_slot_links(left, right)
    }

    fn split_slot_links(&mut self, head: u16) -> u16 {
        let mut slow = head;
        let mut fast = self.commands[head as usize].next;
        while fast != WORLD_COMMAND_NONE {
            fast = self.commands[fast as usize].next;
            if fast != WORLD_COMMAND_NONE {
                slow = self.commands[slow as usize].next;
                fast = self.commands[fast as usize].next;
            }
        }

        let mid = self.commands[slow as usize].next;
        self.commands[slow as usize].next = WORLD_COMMAND_NONE;
        mid
    }

    fn merge_sorted_slot_links(&mut self, mut left: u16, mut right: u16) -> u16 {
        let mut head = WORLD_COMMAND_NONE;
        let mut tail = WORLD_COMMAND_NONE;

        while left != WORLD_COMMAND_NONE && right != WORLD_COMMAND_NONE {
            let take_left = !should_insert_world_before(
                self.commands[right as usize],
                self.commands[left as usize],
            );
            let link = if take_left {
                let next = self.commands[left as usize].next;
                let out = left;
                left = next;
                out
            } else {
                let next = self.commands[right as usize].next;
                let out = right;
                right = next;
                out
            };
            self.commands[link as usize].next = WORLD_COMMAND_NONE;
            if head == WORLD_COMMAND_NONE {
                head = link;
            } else {
                self.commands[tail as usize].next = link;
            }
            tail = link;
        }

        let rest = if left != WORLD_COMMAND_NONE {
            left
        } else {
            right
        };
        if head == WORLD_COMMAND_NONE {
            rest
        } else {
            self.commands[tail as usize].next = rest;
            head
        }
    }

    /// Sort and insert all submitted triangles into the ordering table.
    pub fn flush(&mut self) {
        if self.ordering == WorldCommandOrdering::DeferredSorted {
            sort_world_for_ot_insert(&mut self.commands[..self.command_len]);
            let mut command_index = 0;
            while command_index < self.command_len {
                let command = &self.commands[command_index];
                if !command.packet_ptr.is_null() {
                    // SAFETY: Commands are created only from primitive
                    // arenas borrowed by submit methods. Those packets live
                    // until after this pass flushes and the frame submits.
                    unsafe {
                        self.ot.add_raw_unchecked(
                            command.slot as usize,
                            command.packet_ptr,
                            command.words,
                        )
                    };
                }
                command_index += 1;
            }
            return;
        }

        if self.ordering == WorldCommandOrdering::Bucketed {
            // OrderingTable::insert prepends packets. Walking submitted
            // commands backwards preserves same-slot submission order without
            // building/reversing per-slot linked lists. The PS1 implementation
            // performs the whole walk in one scheduled MIPS loop; the host
            // fallback has identical packet-chain semantics.
            let commands = self.commands.as_ptr().cast::<BucketedWorldCommand>();
            // SAFETY: Bucketed push_command initialised every compact entry
            // below command_len. Every packet came from a live primitive arena,
            // and each slot was produced by this OT-depth-aware pass.
            unsafe {
                self.ot.add_packed_commands_reverse_unchecked(
                    commands.cast::<usize>(),
                    self.command_len,
                )
            };
            return;
        }

        if self.ordering == WorldCommandOrdering::DeferredSlotSorted {
            self.sort_slot_links();
        }

        let mut slot = 0;
        while slot < OT_DEPTH {
            let mut command_index = self.slot_heads()[slot];
            while command_index != WORLD_COMMAND_NONE {
                let command = self.commands[command_index as usize];
                if !command.packet_ptr.is_null() {
                    // SAFETY: Commands are created only from primitive
                    // arenas borrowed by submit methods. Those packets live
                    // until after this pass flushes and the frame submits.
                    unsafe {
                        self.ot.add_raw_unchecked(
                            command.slot as usize,
                            command.packet_ptr,
                            command.words,
                        )
                    };
                }
                command_index = command.next;
            }
            slot += 1;
        }
    }
}
