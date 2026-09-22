unsafe fn tex_anim_tick(m: &Map) {
    if m.n_tex_anim == 0 {
        return;
    }
    let tenth = SIM_NOW >> 1;
    if tenth == TEX_ANIM_TENTH {
        return;
    }
    TEX_ANIM_TENTH = tenth;
    let mut changed = false;
    let mut pvs_changed = false;
    for c in 0..m.n_tex_anim {
        let (primary, alt) = m.tex_anim_chain(c);
        let pcur = primary
            .get(map::tex_anim_frame_index(tenth, primary.len()))
            .copied()
            .unwrap_or(0);
        let acur = alt
            .get(map::tex_anim_frame_index(tenth, alt.len()))
            .copied()
            .unwrap_or(0);
        for &id in primary {
            let id_changed =
                map::tex_anim_set(id as usize, pcur, if alt.is_empty() { pcur } else { acur });
            changed |= id_changed;
            pvs_changed |= id_changed && pvs_has_texanim(id as usize);
        }
        for &id in alt {
            let id_changed = map::tex_anim_set(
                id as usize,
                acur,
                if primary.is_empty() { acur } else { pcur },
            );
            changed |= id_changed;
            pvs_changed |= id_changed && pvs_has_texanim(id as usize);
        }
    }
    if changed {
        TEX_ANIM_GEN = TEX_ANIM_GEN.wrapping_add(1);
    }
    if pvs_changed {
        PVS_TEX_ANIM_GEN = PVS_TEX_ANIM_GEN.wrapping_add(1);
    }
}
