// Frozen production record encoder and face loop, adapted only for borrowed test storage.
use super::*;
const PVS_LINK_END: u16 = u16::MAX;
const PVS_GROUP_LIQUID: u16 = 0x8000;
const PVS_GROUP_FACE_MASK: u16 = 0x7fff;
const PVS_FACE_LOOP: u8 = 0x08;
const PVS_FACE_TRANSLUCENT: u8 = 0x10;
const PVS_FACE_REFINED_TOPOLOGY: u8 = 0x20;
const PVS_FACE_COPLANAR_BACKDROP: u8 = 0x40;
const PVS_FACE_PATCH: u8 = 0x80;
const PVS_FACE_COUNT_MASK: u16 = 0x3fff;
const PVS_FACE_SPECIAL: u16 = 0x4000;
const PVS_FACE_CUTOUT: u16 = 0x8000;
pub(super) fn legacy_record(m: &Fixture, face: usize) -> PvsFaceRec {
    let (first, cnt) = m.face_tris(face);
    let liquid = m.face_liquid(face);
    let cutout = m.face_cutout_backed(face);
    let coplanar_backdrop = m.face_coplanar_backdrop(face);
    let (bc, radius) = m.face_bounds(face);
    PvsFaceRec {
        first: first as u16,
        count: cnt as u16
            | if m.face_needs_generic_emit(face) {
                PVS_FACE_SPECIAL
            } else {
                0
            }
            | if cutout { PVS_FACE_CUTOUT } else { 0 },
        center: [bc[0] as i16, bc[1] as i16, bc[2] as i16],
        radius: radius as u16,
        tex: m.face_tex(face) as u8,
        meta: (if m.face_is_loop(face) {
            PVS_FACE_LOOP
        } else {
            0
        }) | (if m.face_is_patch(face) {
            PVS_FACE_PATCH
        } else {
            0
        }) | (if liquid { PVS_FACE_TRANSLUCENT } else { 0 })
            | (if m.face_refined_topology(face) {
                PVS_FACE_REFINED_TOPOLOGY
            } else {
                0
            })
            | (if coplanar_backdrop {
                PVS_FACE_COPLANAR_BACKDROP
            } else {
                0
            }),
    }
}

fn run(m: &Fixture, visibility: &[u8], animated: &[u32], previous: usize, out: &mut Storage) -> FaceSummary {
 let max_faces=out.indices.len();let max_groups=out.group_first.len();let max_records=out.records.len();let max_leaves=visibility.len()*8;let texture_count=animated.len()*32;
 let tex_is_animated=|t:usize| {let i=t&(texture_count-1);animated[i>>5]&(1<<(i&31))!=0};
 let mut summary=FaceSummary {group_count:previous,..FaceSummary::default()};
    let mut old_group = 0usize;
    while old_group < summary.group_count {
        out.group_face[out.active_groups[old_group] as usize] = PVS_LINK_END;
        old_group += 1;
    }
    summary.leaf_count = 0;
    summary.face_count = 0;

    summary.group_count = 0;
    summary.has_translucent = false;
    summary.has_texture_animation = false;
    out.animated_textures.fill(0);
    // Caller-owned animation generation reset excluded from the list oracle.
    summary.triangle_references = 0;
    // Caller-owned entity candidate reset excluded from the list oracle.
    // A 1-bit duplicate marker recovers 7.4 KiB over the old byte-per-face
    // token array. PVS rebuilds already walk the visible marks; clearing 272
    // words here is cold compared with retaining that RAM every frame.
    out.marks.fill(0);

    for i in 0..m.n_visleaves.min(max_leaves) {
        if visibility[i >> 3] & (1u8 << (i & 7)) == 0 {
            continue;
        }
        let leaf = i + 1;
        summary.leaf_count += 1;

        let (_, m0, mc) = m.leaf(leaf);
        for mj in m0..m0 + mc {
            if mj >= m.n_marks {
                break;
            }
            let face = m.mark(mj);
            if face >= max_faces {
                continue;
            }
            let mark_word = face >> 5;
            let mark_bit = 1u32 << (face & 31);
            if out.marks[mark_word] & mark_bit != 0 {
                continue;
            }
            out.marks[mark_word] |= mark_bit;
            let (first, cnt) = m.face_tris(face);
            if cnt == 0 || first > u16::MAX as usize || cnt > PVS_FACE_COUNT_MASK as usize {
                continue;
            }

            if summary.face_count >= max_faces {
                continue;
            }

            let group = m.face_group(face);
            if group >= max_groups {
                continue;
            }
            if out.group_face[group] == PVS_LINK_END {
                if summary.group_count >= max_groups {
                    break;
                }
                out.group_first[group] = PVS_LINK_END;
                out.group_face[group] = face as u16 & PVS_GROUP_FACE_MASK;
                out.active_groups[summary.group_count] = group as u16;
                summary.group_count += 1;
            }
            if m.face_liquid(face) {
                out.group_face[group] |= PVS_GROUP_LIQUID;
            }

            let entry = summary.face_count;
            out.indices[entry] = face as u16;
            let liquid = m.face_liquid(face);
            let face_tex = m.face_tex(face);
            let animated = tex_is_animated(face_tex);
            summary.has_translucent |= liquid;
            summary.has_texture_animation |= animated;
            if animated {
                let tex = face_tex & (texture_count - 1);
                out.animated_textures[tex >> 5] |= 1u32 << (tex & 31);
            }
            if entry < max_records {
                out.records[entry] = legacy_record(m, face);
            }
            summary.triangle_references += cnt;
            out.next[entry] = out.group_first[group];
            out.group_first[group] = entry as u16;
            summary.face_count += 1;
        }
    }

    summary
}
