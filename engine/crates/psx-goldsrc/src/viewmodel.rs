//! Held-weapon ("viewmodel") triangle table, depth sort and GPU emission.
//!
//! The held model is camera-locked, so everything about a triangle except its
//! projected vertices is fixed for as long as the weapon and the brightness
//! setting are: vertex indices, texture, shade and whether it may be
//! backface-culled. [`build_static`] decodes that once per weapon load. Each
//! pose change then only projects, rejects and bucket-sorts ([`sort`]).
//!
//! Emission has two forms that put identical GP0 words in identical order:
//! [`write_chain`] builds a DMA linked list the caller kicks after the world
//! (the CPU never waits on the GPU per triangle), and [`emit_immediate`] writes
//! GP0 directly for frames whose packet storage cannot hold the chain.
//!
//! Half-Life and Counter-Strike share this module. It reads an HMD8 model,
//! projected vertex arrays and packet materials; each port supplies the
//! storage and its own shade, cull and depth rules.

use psx_asset::hmd8::{Model, RENDER_FACE_INDEX_MASK};
use psx_gpu::material::TexturedGouraudPacketMaterial;

/// Eleven bits address every authored triangle; `NONE` ends a bucket list.
pub const LINK_BITS: u32 = 11;
/// Mask of a triangle's per-sort bucket link in its `meta` word.
pub const LINK_MASK: u32 = (1 << LINK_BITS) - 1;
/// Bucket-list terminator.
pub const NONE: u16 = LINK_MASK as u16;
/// Most triangles a model may have ([`NONE`] is reserved).
pub const MAX_TRIS: usize = NONE as usize;
/// DMA linked-list terminator, for a [`write_chain`] list that ends the walk.
pub const END: u32 = 0x00FF_FFFF;
const TEX_SHIFT: u32 = LINK_BITS;
const SHADE_SHIFT: u32 = TEX_SHIFT + 8;
// Flags beside the three 10-bit vertex indices of an `idx` word.
const TWO_SIDED: u32 = 1 << 30;
const HIDDEN: u32 = 1 << 31;
/// Draw-offset nodes around the triangles: tag plus one GP0 word each.
const OFFSET_NODE_WORDS: usize = 2;
/// Worst-case triangle node: tag, texture-window word, seven packet words.
const TRI_NODE_WORDS: usize = 9;

/// Caller-owned tables. `idx` and `meta` hold one word per authored
/// triangle and must not move while a sort is live; `heads` holds one link
/// per depth bucket and is rewritten by every [`sort`].
///
/// `idx`: vertex indices in bits 0..30, `TWO_SIDED`, `HIDDEN`.
/// `meta`: next link in bits 0..11 (per sort), texture in 11..19, shade in
/// 19..27 (static).
#[derive(Clone, Copy)]
pub struct Tables {
    /// Packed vertex indices and cull flags, one word per triangle.
    pub idx: *mut u32,
    /// Texture, shade and bucket link, one word per triangle.
    pub meta: *mut u32,
    /// One list head per depth bucket.
    pub heads: *mut u16,
    /// Number of depth buckets `heads` holds.
    pub buckets: usize,
}

/// Decode the pose-invariant half of every triangle. `visible(tex)` drops
/// triangles whose texture failed to load, `two_sided(tex)` exempts a texture
/// from backface culling and `shade(normal)` gives the final flat shade.
///
/// # Safety
/// `t.idx` and `t.meta` must be writable for `md.n_tris` words, and
/// `md.n_tris` must not exceed [`MAX_TRIS`].
#[inline(never)]
#[optimize(size)]
pub unsafe fn build_static(
    md: &Model,
    t: Tables,
    visible: impl Fn(usize) -> bool,
    two_sided: impl Fn(usize) -> bool,
    shade: impl Fn([i8; 3]) -> u8,
) {
    for i in 0..md.n_tris {
        let tri = md.tri(i);
        let packed = (tri.idx[0] as u32 & RENDER_FACE_INDEX_MASK)
            | ((tri.idx[1] as u32 & RENDER_FACE_INDEX_MASK) << 10)
            | ((tri.idx[2] as u32 & RENDER_FACE_INDEX_MASK) << 20);
        let flags = if !visible(tri.tex) {
            HIDDEN
        } else if two_sided(tri.tex) {
            TWO_SIDED
        } else {
            0
        };
        unsafe {
            t.idx.add(i).write(packed | flags);
            t.meta.add(i).write(
                ((tri.tex as u32 & 0xff) << TEX_SHIFT)
                    | ((shade(tri.normal) as u32) << SHADE_SHIFT),
            );
        }
    }
}

/// Bucket-sort the triangles of one projected pose. A triangle is dropped
/// when it is hidden, any vertex depth is below `near`, or it is not two-sided
/// and `culled(a, b, c)` holds for its packed screen XY words. `bucket` maps
/// the sum of the three vertex depths to a bucket; buckets draw from the last
/// to the first, and within a bucket from the highest triangle to the lowest.
/// Returns the number of triangles linked.
///
/// # Safety
/// `xy`/`z` must hold every vertex the triangles index, the tables must come
/// from [`build_static`] for this model, and `bucket` must stay in range.
#[inline(always)]
pub unsafe fn sort(
    n_tris: usize,
    t: Tables,
    xy: *const u32,
    z: *const u16,
    near: u16,
    culled: impl Fn(u32, u32, u32) -> bool,
    bucket: impl Fn(u32) -> usize,
) -> usize {
    unsafe {
        for b in 0..t.buckets {
            t.heads.add(b).write(NONE);
        }
        let mut linked = 0usize;
        for i in 0..n_tris {
            let w = t.idx.add(i).read();
            if w & HIDDEN != 0 {
                continue;
            }
            let a = (w & RENDER_FACE_INDEX_MASK) as usize;
            let b = ((w >> 10) & RENDER_FACE_INDEX_MASK) as usize;
            let c = ((w >> 20) & RENDER_FACE_INDEX_MASK) as usize;
            let (za, zb, zc) = (z.add(a).read(), z.add(b).read(), z.add(c).read());
            if za < near || zb < near || zc < near {
                continue;
            }
            if w & TWO_SIDED == 0 && culled(xy.add(a).read(), xy.add(b).read(), xy.add(c).read()) {
                continue;
            }
            let k = bucket(za as u32 + zb as u32 + zc as u32);
            let head = t.heads.add(k);
            let meta = t.meta.add(i);
            meta.write((meta.read() & !LINK_MASK) | *head as u32);
            head.write(i as u16);
            linked += 1;
        }
        linked
    }
}

/// Words [`write_chain`] may need for `linked` triangles.
pub const fn chain_words(linked: usize) -> usize {
    2 * OFFSET_NODE_WORDS + linked * TRI_NODE_WORDS
}

/// The seven packet words of one sorted triangle plus its texture window.
#[inline(always)]
unsafe fn tri_words(
    md: &Model,
    t: Tables,
    xy: *const u32,
    i: usize,
    material: &impl Fn(usize) -> TexturedGouraudPacketMaterial,
) -> (u32, [u32; 7], u32) {
    unsafe {
        let meta = t.meta.add(i).read();
        let w = t.idx.add(i).read();
        let m = material(((meta >> TEX_SHIFT) & 0xff) as usize);
        let shade = (meta >> SHADE_SHIFT) & 0xff;
        let rgb = shade | (shade << 8) | (shade << 16);
        let uv = md.tri_uv_words(i);
        let words = [
            // Flat-shaded: the Gouraud bit is cleared and vertex 0 carries RGB.
            (m.color0_command_word & !0x1000_0000) | rgb,
            xy.add((w & RENDER_FACE_INDEX_MASK) as usize).read(),
            uv[0] as u32 | m.clut_high_word,
            xy.add(((w >> 10) & RENDER_FACE_INDEX_MASK) as usize).read(),
            uv[1] as u32 | m.tpage_high_word,
            xy.add(((w >> 20) & RENDER_FACE_INDEX_MASK) as usize).read(),
            uv[2] as u32,
        ];
        (m.tex_window_word, words, meta & LINK_MASK)
    }
}

/// Write the sorted triangles as one GPU DMA linked list in `out`: a node
/// setting draw offset `offset_on` (GP0 E5 word), one node per triangle
/// (with a texture-window word when it changes, always before the first) and
/// a node setting `offset_off`. When the two offset words are equal the pair
/// changes nothing and is left out. The last node links to `link`: [`END`]
/// stops the walk there, or another list's DMA address continues it.
///
/// Returns `(words, nodes, triangles)`, or `None` when `out` is shorter than
/// [`chain_words`] of the sorted count. With no triangles and no offset pair
/// the list is empty (`nodes == 0`) and the caller links past it. The list
/// stays valid while `out` does; the caller submits it.
///
/// # Safety
/// As for [`sort`], which must have run for this pose. `out` must be
/// word-aligned RAM that stays untouched until the DMA walk finishes.
#[allow(clippy::too_many_arguments)]
pub unsafe fn write_chain(
    md: &Model,
    t: Tables,
    xy: *const u32,
    material: impl Fn(usize) -> TexturedGouraudPacketMaterial,
    linked: usize,
    offset_on: u32,
    offset_off: u32,
    link: u32,
    out: &mut [u32],
) -> Option<(usize, usize, usize)> {
    if out.len() < chain_words(linked) {
        return None;
    }
    let offsets = offset_on != offset_off;
    unsafe {
        let base = out.as_mut_ptr();
        // Every node is followed directly by the next, so a tag can point at
        // the next address as soon as its own length is known.
        let tag =
            |at: usize, len: usize| ((len as u32) << 24) | (base.add(at + 1 + len) as u32 & END);
        let mut at = 0usize;
        if offsets {
            base.write(tag(0, 1));
            base.add(1).write(offset_on);
            at = OFFSET_NODE_WORDS;
        }
        let mut window = u32::MAX;
        let mut tris = 0usize;
        let mut last = None;
        for b in (0..t.buckets).rev() {
            let mut next_link = *t.heads.add(b);
            while next_link != NONE {
                let i = next_link as usize;
                let (next_window, words, next) = tri_words(md, t, xy, i, &material);
                let mut p = at + 1;
                if next_window != window {
                    base.add(p).write(next_window);
                    window = next_window;
                    p += 1;
                }
                for word in words {
                    base.add(p).write(word);
                    p += 1;
                }
                base.add(at).write(tag(at, p - at - 1));
                last = Some(at);
                at = p;
                tris += 1;
                next_link = next as u16;
            }
        }
        if offsets {
            base.add(at).write((1 << 24) | (link & END));
            base.add(at + 1).write(offset_off);
            return Some((at + OFFSET_NODE_WORDS, tris + 2, tris));
        }
        if let Some(l) = last {
            base.add(l)
                .write((base.add(l).read() & !END) | (link & END));
        }
        Some((at, tris, tris))
    }
}

/// Immediate-mode form of [`write_chain`]'s triangles, for frames whose
/// packet storage is short: the same words in the same order, paced once per
/// packet. The caller sets and restores the draw offset around it.
///
/// # Safety
/// As for [`write_chain`].
#[inline(never)]
#[optimize(size)]
pub unsafe fn emit_immediate(
    md: &Model,
    t: Tables,
    xy: *const u32,
    material: impl Fn(usize) -> TexturedGouraudPacketMaterial,
) -> usize {
    use psx_io::gpu::{wait_cmd_ready, write_gp0};
    let mut window = u32::MAX;
    let mut tris = 0usize;
    unsafe {
        for b in (0..t.buckets).rev() {
            let mut link = *t.heads.add(b);
            while link != NONE {
                let (next_window, words, next) = tri_words(md, t, xy, link as usize, &material);
                wait_cmd_ready();
                if next_window != window {
                    write_gp0(next_window);
                    window = next_window;
                }
                for word in words {
                    write_gp0(word);
                }
                tris += 1;
                link = next as u16;
            }
        }
    }
    tris
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Minimal HMD8 blob: one bone, one range, one frame, i8x3 normals.
    fn model(verts: usize, tris: &[([u16; 3], u16, [i8; 3])]) -> Model {
        let mut d = Vec::new();
        let ranges_off = 36 + 4; // header + one clip
        let poses_off = ranges_off + 8 + verts * 6;
        let tri_off = poses_off + 20;
        for w in [
            verts as u32,
            tris.len() as u32,
            0,
            1,
            1,
            (tri_off - ranges_off) as u32,
        ] {
            d.extend_from_slice(&w.to_le_bytes());
        }
        for h in [0u16, 1 << 3, 1, 1] {
            d.extend_from_slice(&h.to_le_bytes());
        }
        let mut head = b"HMD8".to_vec();
        head.extend_from_slice(&d);
        let mut d = head;
        d.extend_from_slice(&[0; 4]); // clip
        for h in [0u16, verts as u16, 0, 0] {
            d.extend_from_slice(&h.to_le_bytes());
        }
        d.resize(tri_off, 0);
        for (n, &(idx, tex, normal)) in tris.iter().enumerate() {
            for h in [idx[0], idx[1], idx[2], tex] {
                d.extend_from_slice(&h.to_le_bytes());
            }
            // Distinct UVs per triangle and corner.
            d.extend((0..6).map(|k| (n * 6 + k) as u8));
            d.extend(normal.iter().map(|&c| c as u8));
            d.extend([0; 3]); // 20-byte records with i8x3 normals
        }
        Model::load(Vec::leak(d))
    }

    fn material(tex: usize) -> TexturedGouraudPacketMaterial {
        TexturedGouraudPacketMaterial {
            tex_window_word: 0xE200_0000 | (tex as u32 & 1),
            color0_command_word: 0x3400_0000,
            clut_high_word: (0x100 + tex as u32) << 16,
            tpage_high_word: (0x200 + tex as u32) << 16,
        }
    }

    const BUCKETS: usize = 8;
    const NEAR: u16 = 10;
    fn shade(n: [i8; 3]) -> u8 {
        (n[0] as u8).wrapping_add(n[1] as u8)
    }
    fn culled(a: u32, b: u32, _c: u32) -> bool {
        a == b // a degenerate edge stands in for a back face
    }
    fn bucket(sum: u32) -> usize {
        (sum as usize / 30).min(BUCKETS - 1)
    }

    /// The emission the ports carried before `build_static`: every triangle
    /// decoded, texture-checked, rejected, shaded and bucketed per sort, then
    /// written from the last bucket to the first.
    fn reference(md: &Model, xy: &[u32], z: &[u16], hidden: usize, two_sided: usize) -> Vec<u32> {
        let mut heads = [NONE; BUCKETS];
        let mut next = [NONE; 64];
        for (t, next_t) in next.iter_mut().enumerate().take(md.n_tris) {
            let tri = md.tri(t);
            if tri.tex == hidden {
                continue;
            }
            let [a, b, c] = tri.idx.map(|i| i as usize);
            if z[a] < NEAR || z[b] < NEAR || z[c] < NEAR {
                continue;
            }
            if tri.tex != two_sided && culled(xy[a], xy[b], xy[c]) {
                continue;
            }
            let k = bucket(z[a] as u32 + z[b] as u32 + z[c] as u32);
            *next_t = heads[k];
            heads[k] = t as u16;
        }
        let mut out = Vec::new();
        let mut window = u32::MAX;
        for k in (0..BUCKETS).rev() {
            let mut t = heads[k];
            while t != NONE {
                let tri = md.tri(t as usize);
                let m = material(tri.tex);
                if m.tex_window_word != window {
                    window = m.tex_window_word;
                    out.push(window);
                }
                let s = shade(tri.normal) as u32;
                let uv = md.tri_uv_words(t as usize);
                let [a, b, c] = tri.idx.map(|i| xy[i as usize]);
                out.extend([
                    (m.color0_command_word & !0x1000_0000) | s | (s << 8) | (s << 16),
                    a,
                    uv[0] as u32 | m.clut_high_word,
                    b,
                    uv[1] as u32 | m.tpage_high_word,
                    c,
                    uv[2] as u32,
                ]);
                t = next[t as usize];
            }
        }
        out
    }

    /// Follow the list's tags from its first node, collecting payload words,
    /// and return them with the final node's link.
    fn walk(out: &[u32], nodes: usize) -> (Vec<u32>, u32) {
        let base = out.as_ptr() as u32 & END;
        let mut words = Vec::new();
        let mut at = 0usize;
        let mut link = END;
        for n in 0..nodes {
            let tag = out[at];
            let len = (tag >> 24) as usize;
            words.extend_from_slice(&out[at + 1..at + 1 + len]);
            link = tag & END;
            if n + 1 < nodes {
                at = (link.wrapping_sub(base) & END) as usize / 4;
            }
        }
        (words, link)
    }

    fn setup() -> (Model, Vec<u32>, Vec<u16>) {
        let tris = [
            ([0, 1, 2], 0, [3, 4, 0]),
            ([1, 2, 3], 1, [5, 6, 0]),
            ([2, 3, 4], 2, [7, 8, 0]), // hidden texture
            ([0, 0, 5], 0, [9, 1, 0]), // culled (degenerate)
            ([0, 0, 6], 3, [2, 2, 0]), // same shape, two-sided texture
            ([5, 6, 7], 1, [4, 4, 0]), // vertex 7 is behind the near plane
            ([3, 4, 5], 1, [6, 1, 0]),
            ([4, 5, 6], 0, [1, 9, 0]),
            ([6, 1, 3], 3, [8, 3, 0]),
        ];
        let md = model(8, &tris);
        assert_eq!(md.n_tris, tris.len());
        let xy = (0..8u32).map(|v| 0x0010_0020 * (v + 1)).collect();
        let z = [20, 50, 90, 40, 70, 30, 60, 5].to_vec();
        (md, xy, z)
    }

    fn tables(idx: &mut [u32], meta: &mut [u32], heads: &mut [u16]) -> Tables {
        Tables {
            idx: idx.as_mut_ptr(),
            meta: meta.as_mut_ptr(),
            heads: heads.as_mut_ptr(),
            buckets: BUCKETS,
        }
    }

    #[test]
    fn chain_matches_the_per_sort_decoder() {
        let (md, xy, z) = setup();
        let (mut idx, mut meta, mut heads) = ([0u32; 64], [0u32; 64], [0u16; BUCKETS]);
        let t = tables(&mut idx, &mut meta, &mut heads);
        let expected = reference(&md, &xy, &z, 2, 3);
        unsafe {
            build_static(&md, t, |tex| tex != 2, |tex| tex == 3, shade);
            // Sort twice: a second pose must not inherit the first one's links.
            sort(md.n_tris, t, xy.as_ptr(), z.as_ptr(), NEAR, culled, |_| 0);
            let linked = sort(md.n_tris, t, xy.as_ptr(), z.as_ptr(), NEAR, culled, bucket);
            assert_eq!(linked, 6);
            let mut out = [0u32; chain_words(6)];
            let (on, off) = (0xE500_1234, 0xE500_0000);
            let (words, nodes, tris) = write_chain(
                &md,
                t,
                xy.as_ptr(),
                material,
                linked,
                on,
                off,
                0x1F_F000,
                &mut out,
            )
            .unwrap();
            assert_eq!((nodes, tris), (8, 6));
            let (got, link) = walk(&out[..words], nodes);
            assert_eq!(link, 0x1F_F000);
            assert_eq!(got.first(), Some(&on));
            assert_eq!(got.last(), Some(&off));
            assert_eq!(&got[1..got.len() - 1], &expected[..]);
            // Equal offsets are left out; the last triangle carries the link.
            let (words, nodes, _) = write_chain(
                &md,
                t,
                xy.as_ptr(),
                material,
                linked,
                off,
                off,
                END,
                &mut out,
            )
            .unwrap();
            assert_eq!(nodes, 6);
            let (got, link) = walk(&out[..words], nodes);
            assert_eq!((got, link), (expected, END));
            // A short buffer is refused rather than overrun.
            assert!(write_chain(
                &md,
                t,
                xy.as_ptr(),
                material,
                linked,
                on,
                off,
                END,
                &mut out[..chain_words(6) - 1]
            )
            .is_none());
        }
    }

    #[test]
    fn empty_pose_writes_only_what_changes_state() {
        let (md, xy, _) = setup();
        let z = [0u16; 8]; // everything behind the near plane
        let (mut idx, mut meta, mut heads) = ([0u32; 64], [0u32; 64], [0u16; BUCKETS]);
        let t = tables(&mut idx, &mut meta, &mut heads);
        unsafe {
            build_static(&md, t, |_| true, |_| false, shade);
            let linked = sort(md.n_tris, t, xy.as_ptr(), z.as_ptr(), NEAR, culled, bucket);
            assert_eq!(linked, 0);
            let mut out = [0u32; chain_words(0)];
            let r = write_chain(&md, t, xy.as_ptr(), material, 0, 1, 2, END, &mut out).unwrap();
            assert_eq!(r, (4, 2, 0));
            assert_eq!(walk(&out, 2), (std::vec![1, 2], END));
            assert_eq!(
                write_chain(&md, t, xy.as_ptr(), material, 0, 2, 2, END, &mut out),
                Some((0, 0, 0))
            );
        }
    }
}
