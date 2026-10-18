//! Fitting one texture placement across a patch of faces.
//!
//! A Fit mapping measures itself against one face's frame, so `2 x 2` on
//! every floor of a `4 x 2` patch is eight small tilings. To fit across the
//! patch instead, every face keeps Fit but widens its frame (`FaceUv::grow`)
//! to the patch's bounding rectangle: the repeats then run across the whole
//! patch and each face shows its own share, with neighbours lining up
//! exactly (the texel map is one affine function over the patch).
//!
//! Floors and ceilings are the faces with a patch: one rectangle of
//! sectors. A wall's frame is already its whole run, which `run_faces` of
//! the edit module selects.

use std::collections::BTreeMap;

use crate::face_uv::UvMode;

use super::edit::{edit_uv, FaceRef, FaceTarget, SurfaceKind, UvOp};
use super::faces::UvDefaults;
use super::uv::{own_frame_of, report_for, Frame};
use super::world::GridWorld;

/// Most repeats across a patch the helper accepts.
pub const MAX_PATCH_REPEATS: f32 = 64.0;

/// What one patch (all the floors, or all the ceilings, of a selection)
/// came to.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchFit {
    /// The surface the patch is made of.
    pub kind: SurfaceKind,
    /// Faces in the patch.
    pub faces: usize,
    /// The patch's rectangle in grid units, `[[min s, min t], [max s, max t]]`.
    pub bounds: [[f64; 2]; 2],
    /// Faces the cook cuts into more than one primitive.
    pub cut_faces: usize,
    /// Primitives the cuts add.
    pub quads_added: u32,
    /// The most tiles any one face shows along each axis.
    pub max_tiles_per_face: [f32; 2],
}

/// What [`fit_across`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AcrossReport {
    /// One entry per surface kind that was present.
    pub patches: Vec<PatchFit>,
    /// Selected faces that are not floors or ceilings (walls fit per run).
    pub skipped: usize,
    /// Faces whose placement actually changed.
    pub changed: usize,
}

/// Fit `repeats` (tiles across the whole patch, horizontal and vertical, in
/// steps of 1/16) across the bounding rectangle of the floors and, apart,
/// of the ceilings among `faces`. Other faces are skipped. `tri` picks one
/// triangle of a split surface (`None` both).
pub fn fit_across(
    world: &mut GridWorld,
    defaults: &dyn UvDefaults,
    faces: &[FaceRef],
    tri: Option<u8>,
    repeats: [f32; 2],
) -> Result<AcrossReport, String> {
    let q4 = |value: f32| -> Result<u16, String> {
        if !(value > 0.0 && value <= MAX_PATCH_REPEATS) {
            return Err(format!(
                "repeats across a patch must be in (0, {MAX_PATCH_REPEATS}], got {value}"
            ));
        }
        Ok((value * 16.0).round().max(1.0) as u16)
    };
    let repeats_q4 = [q4(repeats[0])?, q4(repeats[1])?];
    let mut report = AcrossReport::default();
    let mut groups: BTreeMap<SurfaceKind, Vec<(FaceRef, Frame)>> = BTreeMap::new();
    for face in faces {
        let FaceTarget::Surface { kind, .. } = face.target else {
            report.skipped += 1;
            continue;
        };
        match own_frame_of(world, defaults, *face, tri) {
            Some(frame) => groups.entry(kind).or_default().push((*face, frame)),
            None => report.skipped += 1,
        }
    }
    if groups.is_empty() {
        return Err("no floors or ceilings to fit across (walls fit per run)".to_string());
    }
    for (kind, members) in groups {
        let patch = Frame::around(members.iter().flat_map(|(_, f)| [f.min, f.max]));
        for (face, own) in &members {
            // How far the patch reaches past this face on each side, in grid
            // units (the lattice is integer, so the rounding is exact).
            let grow = [
                [
                    (own.min[0] - patch.min[0]).round() as i32,
                    (own.min[1] - patch.min[1]).round() as i32,
                ],
                [
                    (patch.max[0] - own.max[0]).round() as i32,
                    (patch.max[1] - own.max[1]).round() as i32,
                ],
            ];
            let mut changed = edit_uv(world, *face, tri, UvOp::SetMode(UvMode::Fit { repeats_q4 }));
            changed |= edit_uv(world, *face, tri, UvOp::SetGrow(grow));
            report.changed += usize::from(changed);
        }
        let mut fit = PatchFit {
            kind,
            faces: members.len(),
            bounds: [patch.min, patch.max],
            cut_faces: 0,
            quads_added: 0,
            max_tiles_per_face: [0.0; 2],
        };
        for (face, _) in &members {
            if let Some(r) = report_for(world, defaults, *face, tri) {
                fit.cut_faces += usize::from(r.quads_added > 0);
                fit.quads_added += r.quads_added;
                for axis in 0..2 {
                    fit.max_tiles_per_face[axis] = fit.max_tiles_per_face[axis].max(r.tiles[axis]);
                }
            }
        }
        report.patches.push(fit);
    }
    Ok(report)
}

impl AcrossReport {
    /// A line per patch for a tool reply.
    pub fn describe(&self, repeats: [f32; 2]) -> String {
        let n = |v: f32| {
            if (v - v.round()).abs() < 0.005 {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.2}")
            }
        };
        let mut out = String::new();
        for p in &self.patches {
            let [[s0, t0], [s1, t1]] = p.bounds;
            out.push_str(&format!(
                "{} patch of {} face(s), {} x {} sectors: {} x {} tiles across the whole patch (each face shows at most {} x {})",
                match p.kind {
                    SurfaceKind::Floor => "floor",
                    SurfaceKind::Ceiling => "ceiling",
                },
                p.faces,
                n(((s1 - s0) / f64::from(super::units::SECTOR_UNITS)) as f32),
                n(((t1 - t0) / f64::from(super::units::SECTOR_UNITS)) as f32),
                n(repeats[0]),
                n(repeats[1]),
                n(p.max_tiles_per_face[0]),
                n(p.max_tiles_per_face[1]),
            ));
            if p.quads_added > 0 {
                out.push_str(&format!(
                    "\nwarning: the cook cuts {} of these faces (+{} primitives): a primitive holds at most {} tiles per axis",
                    p.cut_faces,
                    p.quads_added,
                    super::uv::MAX_TILES_PER_PRIMITIVE
                ));
            }
            out.push('\n');
        }
        if self.skipped > 0 {
            out.push_str(&format!(
                "{} selected face(s) skipped: only floors and ceilings fit across a patch\n",
                self.skipped
            ));
        }
        out.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_uv::FaceUv;
    use crate::grid::faces::{all_faces, GridFaceKind};
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;
    use crate::grid::uv::report_for;
    use crate::ResourceId;

    fn rid(n: u64) -> ResourceId {
        ron::from_str(&format!("({n})")).expect("a resource id")
    }

    /// A `w` by `d` room with a floor everywhere.
    fn patch_world(w: usize, d: usize) -> GridWorld {
        let mut room = GridRoom::new("r", w, d);
        for z in 0..d {
            for x in 0..w {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(0, Some(rid(7)))));
            }
        }
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn floors(w: usize, d: usize) -> Vec<FaceRef> {
        (0..d)
            .flat_map(|z| (0..w).map(move |x| FaceRef::floor(0, [x as i32, z as i32])))
            .collect()
    }

    fn texels_of(world: &GridWorld, cell: [i32; 2]) -> Vec<[f64; 2]> {
        all_faces(world)
            .into_iter()
            .find(|f| f.cell == cell && matches!(f.kind, GridFaceKind::Floor { .. }))
            .expect("the floor face")
            .texels
    }

    fn extent(texels: &[[f64; 2]], axis: usize) -> (f64, f64) {
        (
            texels.iter().map(|t| t[axis]).fold(f64::MAX, f64::min),
            texels.iter().map(|t| t[axis]).fold(f64::MIN, f64::max),
        )
    }

    #[test]
    fn two_by_two_over_four_by_two_cells_spans_the_whole_rectangle_once() {
        let mut world = patch_world(4, 2);
        let out = fit_across(&mut world, &(), &floors(4, 2), None, [2.0, 2.0]).unwrap();
        assert_eq!(out.patches.len(), 1);
        assert_eq!((out.patches[0].faces, out.changed), (8, 8));
        // Across the patch the texels run 0..128 on both axes; each cell
        // shows its own quarter or half, and neighbours share edges exactly.
        let (mut lo_u, mut hi_u, mut lo_v, mut hi_v) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for z in 0..2 {
            for x in 0..4 {
                let t = texels_of(&world, [x, z]);
                let (u0, u1) = extent(&t, 0);
                let (v0, v1) = extent(&t, 1);
                assert!(
                    (u0 - f64::from(x) * 32.0).abs() < 1e-9,
                    "cell {x},{z}: {u0}"
                );
                assert!((u1 - u0 - 32.0).abs() < 1e-9);
                assert!(
                    (v0 - f64::from(z) * 64.0).abs() < 1e-9,
                    "cell {x},{z}: {v0}"
                );
                assert!((v1 - v0 - 64.0).abs() < 1e-9);
                lo_u = lo_u.min(u0);
                hi_u = hi_u.max(u1);
                lo_v = lo_v.min(v0);
                hi_v = hi_v.max(v1);
            }
        }
        assert_eq!((lo_u, hi_u, lo_v, hi_v), (0.0, 128.0, 0.0, 128.0));
        // The report reads back the patch.
        let r = report_for(&world, &(), floors(4, 2)[0], None).unwrap();
        assert_eq!(r.patch, Some([2.0, 2.0]));
        assert_eq!(r.tiles, [0.5, 1.0]);
        assert!(r.effective_text().contains("part of 2 x 2"));
        assert!(out.describe([2.0, 2.0]).contains("4 x 2 sectors"));
    }

    #[test]
    fn the_patch_follows_a_move_and_a_reset_clears_it() {
        let mut world = patch_world(3, 1);
        fit_across(&mut world, &(), &floors(3, 1), None, [3.0, 1.0]).unwrap();
        // The widening is relative to each face.
        let uv = |w: &GridWorld, x: usize| {
            w.rooms[0]
                .sector(x, 0)
                .unwrap()
                .ground_floor()
                .unwrap()
                .tri_a
                .uv
        };
        assert_eq!(uv(&world, 0).grow, [[0, 0], [2048, 0]]);
        assert_eq!(uv(&world, 1).grow, [[1024, 0], [1024, 0]]);
        assert_eq!(uv(&world, 2).grow, [[2048, 0], [0, 0]]);
        // Moving the room does not change a face's texel span.
        let before = extent(&texels_of(&world, [1, 0]), 0);
        assert!(world.rooms[0].translate([5, 0]));
        let moved = all_faces(&world)
            .into_iter()
            .find(|f| f.cell == [6, 0])
            .unwrap()
            .texels;
        let after = extent(&moved, 0);
        assert!((after.1 - after.0 - (before.1 - before.0)).abs() < 1e-9);
        assert!(world.rooms[0].translate([-5, 0]));
        // A mode change on one face puts it back on its own frame.
        edit_uv(
            &mut world,
            floors(3, 1)[1],
            None,
            UvOp::SetMode(UvMode::fit(1, 1)),
        );
        assert_eq!(uv(&world, 1).grow, [[0; 2]; 2]);
        edit_uv(&mut world, floors(3, 1)[0], None, UvOp::Reset);
        assert!(uv(&world, 0).is_identity());
        assert_eq!(uv(&world, 2).grow, [[2048, 0], [0, 0]]);
    }

    #[test]
    fn the_four_tile_window_limit_is_reported_per_face_and_the_cook_splits() {
        // 16 tiles across 2 cells: 8 per cell, so every face is cut in two.
        let mut world = patch_world(2, 1);
        let out = fit_across(&mut world, &(), &floors(2, 1), None, [16.0, 1.0]).unwrap();
        assert_eq!(out.patches[0].cut_faces, 2);
        assert_eq!(out.patches[0].quads_added, 2);
        assert!(out.describe([16.0, 1.0]).contains("cook cuts 2"));
        let faces = all_faces(&world);
        let pieces: usize = faces
            .iter()
            .map(|f| crate::grid::uv_split::split(f, GridScale { num: 1, den: 1 }, true).len())
            .sum();
        assert_eq!(pieces, 4);
        // Within the limit nothing is cut.
        let mut ok = patch_world(2, 1);
        let out = fit_across(&mut ok, &(), &floors(2, 1), None, [8.0, 1.0]).unwrap();
        assert_eq!(out.patches[0].quads_added, 0);
    }

    #[test]
    fn walls_are_skipped_and_bad_repeats_refused() {
        let mut world = patch_world(2, 1);
        let wall = FaceRef::wall(0, [0, 0], crate::grid::sector::WallEdge::North, 0);
        let err = fit_across(&mut world, &(), &[wall], None, [2.0, 2.0]).unwrap_err();
        assert!(err.contains("no floors"), "{err}");
        assert!(fit_across(&mut world, &(), &floors(2, 1), None, [0.0, 1.0]).is_err());
        assert!(fit_across(&mut world, &(), &floors(2, 1), None, [100.0, 1.0]).is_err());
        let mixed = [wall, floors(2, 1)[0]];
        let out = fit_across(&mut world, &(), &mixed, None, [2.0, 1.0]).unwrap();
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn turns_and_flips_apply_across_the_patch() {
        let mut world = patch_world(2, 1);
        for face in floors(2, 1) {
            edit_uv(&mut world, face, None, UvOp::ToggleFlip(0));
        }
        fit_across(&mut world, &(), &floors(2, 1), None, [2.0, 1.0]).unwrap();
        // Flipped horizontally, the patch's far edge is texel 0 and the near
        // edge texel 128: cell 0 shows the high half.
        let (u0, u1) = extent(&texels_of(&world, [0, 0]), 0);
        assert_eq!((u0, u1), (64.0, 128.0));
        let (u0, u1) = extent(&texels_of(&world, [1, 0]), 0);
        assert_eq!((u0, u1), (0.0, 64.0));
        let r = report_for(&world, &(), floors(2, 1)[0], None).unwrap();
        assert_eq!(r.pieces, [1, 1]);
    }

    #[test]
    fn a_patch_survives_a_save_and_old_files_still_load() {
        let mut world = patch_world(2, 1);
        fit_across(&mut world, &(), &floors(2, 1), None, [2.0, 2.0]).unwrap();
        let text = ron::to_string(&world).unwrap();
        let back: GridWorld = ron::from_str(&text).unwrap();
        assert_eq!(back, world);
        // A face with no widening writes nothing new.
        let plain = ron::to_string(&FaceUv::default()).unwrap();
        assert!(!plain.contains("grow"), "{plain}");
        let old: FaceUv =
            ron::from_str("(offset_texels:(0,0),rotation_deg:0,scale_q8:(256,256))").unwrap();
        assert!(old.is_identity());
    }
}
