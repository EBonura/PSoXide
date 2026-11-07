//! Policy seams of the PXBSP world renderer.
//!
//! The renderer owns the mechanism of drawing a PXBSP world: PVS marking,
//! node-frustum selection, clipping, batching and packet writing. A game that
//! shares that mechanism but has rules of its own (Quake's water portals,
//! texture chains, dynamic lights and stacked floors) supplies them as a
//! [`WorldHooks`] value instead of forking the renderer.
//!
//! Every seam is a monomorphised call guarded by an associated constant, so
//! there is no dynamic dispatch and a game that uses no seam pays nothing:
//! [`NoHooks`] sets every constant to `false`, the guarded code is removed at
//! compile time, and the renderer emits the packets it always did. The
//! `pxbsp_packet_hash` example pins that word for word.

use crate::pxbsp_resident::PxbspResidentMap;
use crate::render::PxbspTextureBinding;

/// What a hook decides for one drawn face.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FaceOverride {
    /// Draw the face with its material's binding, as the renderer would.
    Default,
    /// Do not draw the face (invisible clip and trigger surfaces).
    Skip,
    /// Draw the face with this binding instead of its material's (a liquid
    /// face with its own turbulence tile).
    Bind(PxbspTextureBinding),
}

/// Game rules the world renderer defers to. Each method is reached only when
/// the matching constant is `true`; the defaults describe the unmodified
/// renderer.
pub trait WorldHooks {
    /// [`Self::merge_visibility`] runs after every PVS row is decoded.
    const MERGES_VISIBILITY: bool = false;
    /// [`Self::chain_material`] picks the material a face shows this tick.
    const CHAINS_MATERIALS: bool = false;
    /// [`Self::face_override`] runs for every face that reaches material
    /// resolution.
    const OVERRIDES_FACES: bool = false;
    /// [`Self::light_corner`] adjusts every materialised face corner.
    const LIGHTS_CORNERS: bool = false;
    /// [`Self::depth_bias`] shifts a face's ordering-table slots.
    const BIASES_DEPTH: bool = false;
    /// Edges split by [`Self::tessellation_budget_q3`] instead of the profile's
    /// depth bands.
    const BOUNDS_TESSELLATION_ERROR: bool = false;

    /// Changes whenever [`Self::merge_visibility`] would produce a different
    /// row for the same camera leaf. The renderer keeps its marked face chain
    /// while the leaf and this value both stay the same.
    fn visibility_epoch(&self) -> u32 {
        0
    }

    /// Adjust the decoded PVS row of camera leaf `leaf` in place. Bit `i` is
    /// leaf `i + 1`. Quake merges the row of the leaf on the far side of a
    /// water surface into the camera's own.
    fn merge_visibility(&mut self, _map: &PxbspResidentMap, _leaf: usize, _row: &mut [u8]) {}

    /// The material `material` shows at `tick`. Quake's texture chains map
    /// each frame's material to the next frame's.
    fn chain_material(&self, material: usize, _tick: u32) -> usize {
        material
    }

    /// Override one face. `material` is the face's authored material.
    fn face_override(&mut self, _face: usize, _material: usize) -> FaceOverride {
        FaceOverride::Default
    }

    /// The colour word of a face corner at world `position` (whole world
    /// units), given the colour the renderer materialised.
    fn light_corner(&self, _position: [i16; 3], color: u32) -> u32 {
        color
    }

    /// Ordering-table slots to add to every packet of a face with these
    /// cooked flags; negative is nearer. Faces with different biases are
    /// never batched together.
    fn depth_bias(&self, _face_flags: u16) -> i16 {
        0
    }

    /// Allowed affine texture displacement, in eighths of a pixel (see
    /// [`psx_engine::ErrorBounded`]). A bound of 16 pixels is `128`.
    fn tessellation_budget_q3(&self) -> u32 {
        0
    }
}

/// The renderer's own behaviour: no seam is used.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoHooks;

impl WorldHooks for NoHooks {}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use super::*;
    use crate::pxbsp::{PxbspLumpKind, PxbspMaterial};
    use crate::pxbsp_resident::tests::{valid_lumps, write_file};
    use crate::render::{configure_projection, load_pxbsp_view, Camera, Renderer};
    use crate::{encode_node_bound_max, encode_node_bound_min, CookedRecord, SliceReader, Vec3I32};

    /// Triangle corners at world x = `x`, facing the camera at the origin side.
    fn triangle(x: i16) -> Vec<u8> {
        let mut vertices = Vec::new();
        for position in [[x, -16i16, -16], [x, 16, -16], [x, 0, 16]] {
            for component in position {
                vertices.extend_from_slice(&component.to_le_bytes());
            }
            vertices.extend_from_slice(&[0, 0, 128, 0, 0, 0]);
        }
        vertices
    }

    fn push_u16(output: &mut Vec<u8>, value: u16) {
        output.extend_from_slice(&value.to_le_bytes());
    }

    /// Two empty leaves (1 and 2) that see only themselves, one triangle in
    /// each (x = 64 and x = 96), two materials, all owned by one node.
    fn two_room_map() -> PxbspResidentMap {
        let mut lumps = valid_lumps();
        let mut vertices = triangle(64);
        vertices.extend_from_slice(&triangle(96));
        lumps[PxbspLumpKind::Vertices as usize] = vertices;
        let mins = [64i16, -16, -16].map(encode_node_bound_min);
        let maxs = [96i16, 16, 16].map(encode_node_bound_max);
        let node = &mut lumps[PxbspLumpKind::Nodes as usize];
        node[4..6].copy_from_slice(&(-3i16).to_le_bytes());
        node[6..9].copy_from_slice(&mins.map(|value| value as u8));
        node[9..12].copy_from_slice(&maxs.map(|value| value as u8));
        node[14..16].copy_from_slice(&2u16.to_le_bytes());
        // Two faces: plane 0, material 0 and material 1.
        let mut faces = Vec::new();
        for (first_vertex, material) in [(0u16, 0u16), (3, 1)] {
            push_u16(&mut faces, 0);
            push_u16(&mut faces, first_vertex);
            push_u16(&mut faces, material);
            faces.extend_from_slice(&[0, 3, 0, 0]);
        }
        lumps[PxbspLumpKind::Faces as usize] = faces;
        lumps[PxbspLumpKind::MarkSurfaces as usize] =
            [0u16, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
        // Leaf 1 sees leaf 1 (row byte 0b01), leaf 2 sees leaf 2 (0b10).
        lumps[PxbspLumpKind::Visibility as usize] = vec![0b01, 0b10];
        let leaf = |contents: i8, visibility: i32, first_mark: u16, marks: u16| {
            let mut bytes = vec![contents as u8, 0];
            push_u16(&mut bytes, marks);
            bytes.extend_from_slice(&visibility.to_le_bytes());
            push_u16(&mut bytes, first_mark);
            bytes.extend_from_slice(&[0; 4]);
            bytes
        };
        lumps[PxbspLumpKind::Leaves as usize] =
            [leaf(-2, -1, 0, 0), leaf(-1, 0, 0, 1), leaf(-1, 1, 1, 1)].concat();
        lumps[PxbspLumpKind::Materials as usize] = vec![0; 2 * PxbspMaterial::SIZE];
        // The world model sees two leaves.
        lumps[PxbspLumpKind::Models as usize][26..28].copy_from_slice(&2i16.to_le_bytes());
        let bytes = write_file(&lumps);
        let mut map = PxbspResidentMap::with_capacity(bytes.len());
        map.load(9, &mut SliceReader::new(&bytes))
            .expect("two-room map");
        map
    }

    fn binding(page: u16, clut: u16) -> PxbspTextureBinding {
        PxbspTextureBinding {
            texture_page: page,
            clut,
            texture_window_word: 0xe200_0000,
            uv_origin: [0; 2],
            page_uv_origin: [0; 2],
            texture_size: [64; 2],
        }
    }

    fn camera() -> Camera {
        Camera {
            origin: Vec3I32 {
                x: 1 << 12,
                y: 0,
                z: 0,
            },
            angles: [0; 3],
        }
    }

    struct Drawn {
        stats: crate::render::RenderStats,
        packets: Vec<u32>,
    }

    fn draw<H: WorldHooks>(
        renderer: &mut Renderer,
        map: &PxbspResidentMap,
        bindings: &[Option<PxbspTextureBinding>],
        tick: u32,
        hooks: &mut H,
    ) -> Drawn {
        let mut storage = vec![0u32; 2048];
        let camera = camera();
        let frame = renderer.draw_pxbsp_world_with(
            map,
            camera,
            load_pxbsp_view(camera),
            bindings,
            tick,
            &mut storage,
            hooks,
        );
        storage.truncate(frame.packet_words);
        Drawn {
            stats: frame.stats,
            packets: storage,
        }
    }

    fn fresh(map: &PxbspResidentMap) -> Renderer {
        configure_projection();
        Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len())
    }

    /// (offset, slot) of every packet in a staged stream.
    fn tags(packets: &[u32]) -> Vec<(usize, u32)> {
        let mut found = Vec::new();
        let mut offset = 0;
        while offset < packets.len() {
            found.push((offset, packets[offset] & 0xffff));
            offset += 1 + (packets[offset] >> 24) as usize;
        }
        assert_eq!(offset, packets.len(), "stream walks to its end");
        found
    }

    #[test]
    fn no_hooks_draws_what_the_plain_entry_point_draws() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let mut plain = fresh(&map);
        let camera = camera();
        let mut storage = vec![0u32; 2048];
        let frame = plain.draw_pxbsp_world(
            &map,
            camera,
            load_pxbsp_view(camera),
            &bindings,
            3,
            &mut storage,
        );
        let hooked = draw(&mut fresh(&map), &map, &bindings, 3, &mut NoHooks);
        assert_eq!(frame.stats, hooked.stats);
        assert_eq!(&storage[..frame.packet_words], &hooked.packets[..]);
        // Camera in leaf 1 sees only leaf 1's triangle.
        assert_eq!(hooked.stats.visible_faces, 1);
    }

    #[derive(Default)]
    struct Merge {
        merge: bool,
        epoch: u32,
        calls: u32,
    }

    impl WorldHooks for Merge {
        const MERGES_VISIBILITY: bool = true;

        fn visibility_epoch(&self) -> u32 {
            self.epoch
        }

        fn merge_visibility(&mut self, map: &PxbspResidentMap, leaf: usize, row: &mut [u8]) {
            self.calls += 1;
            assert_eq!(leaf, 1);
            if self.merge {
                // The far side of a water surface: leaf 2's row.
                let mut other = [0u8; 1];
                map.leaf_visibility_into(2, &mut other)
                    .expect("row of leaf 2");
                row[0] |= other[0];
            }
        }
    }

    #[test]
    fn a_visibility_merge_shows_the_other_rooms_faces_and_follows_its_epoch() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let mut renderer = fresh(&map);
        let mut hooks = Merge::default();
        assert_eq!(
            draw(&mut renderer, &map, &bindings, 0, &mut hooks)
                .stats
                .visible_faces,
            1
        );
        // Same leaf, same epoch: the marked chain is kept, the hook not asked again.
        let calls = hooks.calls;
        draw(&mut renderer, &map, &bindings, 0, &mut hooks);
        assert_eq!(hooks.calls, calls);
        // The merge appears when the hook's epoch moves, with the camera still in leaf 1.
        hooks.merge = true;
        hooks.epoch = 1;
        assert_eq!(
            draw(&mut renderer, &map, &bindings, 0, &mut hooks)
                .stats
                .visible_faces,
            2
        );
        hooks.merge = false;
        hooks.epoch = 2;
        assert_eq!(
            draw(&mut renderer, &map, &bindings, 0, &mut hooks)
                .stats
                .visible_faces,
            1
        );
    }

    struct Override(FaceOverride);

    impl WorldHooks for Override {
        const OVERRIDES_FACES: bool = true;

        fn face_override(&mut self, face: usize, _material: usize) -> FaceOverride {
            if face == 0 {
                self.0
            } else {
                FaceOverride::Default
            }
        }
    }

    #[test]
    fn a_face_override_skips_a_face_or_swaps_its_binding() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let mut merge = Merge {
            merge: true,
            epoch: 1,
            calls: 0,
        };
        let both = draw(&mut fresh(&map), &map, &bindings, 0, &mut merge);
        assert_eq!(both.stats.visible_faces, 2);

        struct Both(Override);
        impl WorldHooks for Both {
            const MERGES_VISIBILITY: bool = true;
            const OVERRIDES_FACES: bool = true;
            fn visibility_epoch(&self) -> u32 {
                1
            }
            fn merge_visibility(&mut self, map: &PxbspResidentMap, _leaf: usize, row: &mut [u8]) {
                let mut other = [0u8; 1];
                map.leaf_visibility_into(2, &mut other).unwrap();
                row[0] |= other[0];
            }
            fn face_override(&mut self, face: usize, material: usize) -> FaceOverride {
                self.0.face_override(face, material)
            }
        }
        let skipped = draw(
            &mut fresh(&map),
            &map,
            &bindings,
            0,
            &mut Both(Override(FaceOverride::Skip)),
        );
        assert_eq!(skipped.stats.visible_faces, 1);
        assert!(skipped.packets.len() < both.packets.len());

        let swapped = draw(
            &mut fresh(&map),
            &map,
            &bindings,
            0,
            &mut Both(Override(FaceOverride::Bind(binding(0x0106, 0x4321)))),
        );
        assert_eq!(swapped.stats.visible_faces, 2);
        assert_eq!(swapped.packets.len(), both.packets.len());
        // Face 0 draws with the new CLUT; face 1 keeps the material's.
        let cluts = |drawn: &Drawn| -> Vec<u32> {
            tags(&drawn.packets)
                .into_iter()
                .map(|(offset, _)| drawn.packets[offset + 4] >> 16)
                .collect()
        };
        assert!(cluts(&both).iter().all(|&clut| clut == 0x1234));
        let swapped_cluts = cluts(&swapped);
        assert!(swapped_cluts.contains(&0x4321) && swapped_cluts.contains(&0x1234));
    }

    struct Chain;

    impl WorldHooks for Chain {
        const CHAINS_MATERIALS: bool = true;

        fn chain_material(&self, material: usize, tick: u32) -> usize {
            if material == 0 && tick >= 5 {
                1
            } else {
                material
            }
        }
    }

    #[test]
    fn a_texture_chain_draws_the_frame_the_tick_selects() {
        let map = two_room_map();
        // Only face 0 is in view from leaf 1: its material is 0, chained to 1.
        let bindings = [Some(binding(0x0105, 0x1111)), Some(binding(0x0105, 0x2222))];
        let clut_of_first = |tick| {
            let drawn = draw(&mut fresh(&map), &map, &bindings, tick, &mut Chain);
            assert_eq!(drawn.stats.visible_faces, 1);
            drawn.packets[4] >> 16
        };
        assert_eq!(clut_of_first(0), 0x1111);
        assert_eq!(clut_of_first(5), 0x2222);
        // An out-of-range answer falls back to the authored material.
        struct Wild;
        impl WorldHooks for Wild {
            const CHAINS_MATERIALS: bool = true;
            fn chain_material(&self, _material: usize, _tick: u32) -> usize {
                99
            }
        }
        let drawn = draw(&mut fresh(&map), &map, &bindings, 0, &mut Wild);
        assert_eq!(drawn.packets[4] >> 16, 0x1111);
    }

    #[derive(Default)]
    struct Lights {
        seen: RefCell<Vec<[i16; 3]>>,
        color: u32,
    }

    impl WorldHooks for Lights {
        const LIGHTS_CORNERS: bool = true;

        fn light_corner(&self, position: [i16; 3], color: u32) -> u32 {
            self.seen.borrow_mut().push(position);
            if self.color == 0 {
                color
            } else {
                self.color
            }
        }
    }

    #[test]
    fn corner_light_sees_each_corner_once_and_can_recolour_it() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let plain = draw(&mut fresh(&map), &map, &bindings, 0, &mut NoHooks);
        let mut identity = Lights::default();
        let same = draw(&mut fresh(&map), &map, &bindings, 0, &mut identity);
        assert_eq!(
            same.packets, plain.packets,
            "an identity light changes nothing"
        );
        assert_eq!(
            *identity.seen.borrow(),
            vec![[64, -16, -16], [64, 16, -16], [64, 0, 16]]
        );
        let mut red = Lights {
            color: 0x0000_00ff,
            ..Lights::default()
        };
        let lit = draw(&mut fresh(&map), &map, &bindings, 0, &mut red);
        assert_ne!(lit.packets, plain.packets);
        assert_eq!(lit.packets.len(), plain.packets.len());
    }

    struct Bias(i16);

    impl WorldHooks for Bias {
        const BIASES_DEPTH: bool = true;

        fn depth_bias(&self, _flags: u16) -> i16 {
            self.0
        }
    }

    #[test]
    fn a_depth_bias_moves_every_packet_slot_and_nothing_else() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let plain = draw(&mut fresh(&map), &map, &bindings, 0, &mut NoHooks);
        let nearer = draw(&mut fresh(&map), &map, &bindings, 0, &mut Bias(-3));
        let farther = draw(&mut fresh(&map), &map, &bindings, 0, &mut Bias(4));
        assert_eq!(nearer.packets.len(), plain.packets.len());
        let base = tags(&plain.packets);
        assert!(base.len() > 1);
        for (shifted, bias) in [(&nearer, -3i32), (&farther, 4)] {
            let moved = tags(&shifted.packets);
            assert_eq!(moved.len(), base.len());
            for ((offset, slot), (moved_offset, moved_slot)) in base.iter().zip(&moved) {
                assert_eq!(offset, moved_offset);
                assert_eq!(*moved_slot as i32, (*slot as i32 + bias).clamp(1, 2047));
            }
            // Everything but the slot bits is unchanged.
            for (index, (&a, &b)) in plain.packets.iter().zip(&shifted.packets).enumerate() {
                if base.iter().any(|(offset, _)| *offset == index) {
                    assert_eq!(a & !0xffff, b & !0xffff);
                } else {
                    assert_eq!(a, b);
                }
            }
        }
    }

    #[test]
    fn faces_with_different_biases_never_share_a_batch() {
        struct ByFace;
        impl WorldHooks for ByFace {
            const MERGES_VISIBILITY: bool = true;
            const BIASES_DEPTH: bool = true;
            fn visibility_epoch(&self) -> u32 {
                1
            }
            fn merge_visibility(&mut self, map: &PxbspResidentMap, _leaf: usize, row: &mut [u8]) {
                let mut other = [0u8; 1];
                map.leaf_visibility_into(2, &mut other).unwrap();
                row[0] |= other[0];
            }
            fn depth_bias(&self, _flags: u16) -> i16 {
                0
            }
        }
        struct Split(core::cell::Cell<i16>);
        impl WorldHooks for Split {
            const MERGES_VISIBILITY: bool = true;
            const BIASES_DEPTH: bool = true;
            fn visibility_epoch(&self) -> u32 {
                1
            }
            fn merge_visibility(&mut self, map: &PxbspResidentMap, _leaf: usize, row: &mut [u8]) {
                let mut other = [0u8; 1];
                map.leaf_visibility_into(2, &mut other).unwrap();
                row[0] |= other[0];
            }
            fn depth_bias(&self, _flags: u16) -> i16 {
                // Alternate: the first face asks 0, the second asks -8.
                let next = if self.0.get() == 0 { -8 } else { 0 };
                self.0.set(next);
                next
            }
        }
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        let together = draw(&mut fresh(&map), &map, &bindings, 0, &mut ByFace);
        assert_eq!(together.stats.visible_faces, 2);
        assert_eq!(
            together.stats.surface_batches, 1,
            "equal biases batch together"
        );
        let apart = draw(
            &mut fresh(&map),
            &map,
            &bindings,
            0,
            &mut Split(core::cell::Cell::new(0)),
        );
        assert_eq!(apart.stats.visible_faces, 2);
        assert_eq!(
            apart.stats.surface_batches, 2,
            "different biases split the batch"
        );
    }
    struct Bounded(u32);

    impl WorldHooks for Bounded {
        const BOUNDS_TESSELLATION_ERROR: bool = true;

        fn tessellation_budget_q3(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn an_error_bound_replaces_the_depth_bands_for_the_whole_draw() {
        let map = two_room_map();
        let bindings = [Some(binding(0x0105, 0x1234)); 2];
        // The triangle is square to the view at a shallow depth: the bands
        // split it, the affine error bound sees nothing to fix.
        let banded = draw(&mut fresh(&map), &map, &bindings, 0, &mut NoHooks);
        let bounded = draw(&mut fresh(&map), &map, &bindings, 0, &mut Bounded(128));
        assert!(banded.stats.packets > 1);
        assert_eq!(bounded.stats.packets, 1);
        assert_eq!(bounded.stats.visible_faces, banded.stats.visible_faces);
        // A zero budget demands every edge be as short as the pieces get.
        let strict = draw(&mut fresh(&map), &map, &bindings, 0, &mut Bounded(0));
        assert!(strict.stats.packets >= bounded.stats.packets);
    }
}
