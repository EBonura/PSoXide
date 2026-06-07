// ======================================================================
// Scanline-delta triangle rasterizer
// ======================================================================
//
// Scanline-delta triangle rasterizer. Parity-matched against Redux's
// `drawPoly3Gi` / `drawPoly3TGEx8i` family (`pcsx-redux/src/gpu/soft/soft.cc`).
// The scanline-delta approach is what PSX hardware actually does: sort
// vertices by Y, walk each of the three edges as scanline-advancing
// sections, and for each scanline plot pixels from `leftX` to `rightX - 1`
// inclusive with attribute interpolation driven by precomputed per-pixel
// (column) deltas.
//
// Why this beats barycentric division-per-pixel: rounding. Two equivalent
// formulas for interpolated colour or UV produce subtly different integer
// results at edge-adjacent pixels. Matching Redux's exact algorithm
// (including its fixed-point shift sizes) is the only way to hit pixel-
// exact parity on game content; our old barycentric path was 14% off on
// the Crash title screen even with identical primitive inputs.
//
// Naming convention: Redux's `m_deltaRightR` / `m_deltaRightU` etc are
// actually **per-column** (per-X) deltas despite being named "right".
// We keep the Redux names so a side-by-side diff with `soft.cc` stays
// readable -- even where "right" looks wrong in isolation.
//
// Fixed-point layout: X / U / V are Q16.16. Colour channels are stored
// in bits 16..23 of the state (Q16.8 relative to the 8-bit vertex
// colours, i.e. the fraction lives in bits 0..15).

/// One vertex as seen by the scanline setup. All attributes -- position,
/// colour, UV -- are already shifted into the fixed-point domain the
/// rasterizer uses. `y` stays as a plain integer because scanlines step
/// by 1 on hardware; only horizontal attributes need sub-pixel precision.
#[derive(Copy, Clone, Debug, Default)]
struct SlVertex {
    /// X coordinate in Q16.16 fixed-point.
    x: i64,
    /// Y coordinate as a plain pixel integer.
    y: i32,
    /// Red channel in Q16.16 per-8bit: `vtx_r << 16`. Shifted the same
    /// way Redux does so its `v->R = rgb & 0x00ff0000` constant stays
    /// structurally identical.
    r: i64,
    g: i64,
    b: i64,
    /// U coordinate, Q16.16.
    u: i64,
    /// V coordinate, Q16.16.
    v: i64,
}

/// Per-triangle scanline walk state. Built once by `setup_sections_*`,
/// then `next_row_*` advances it by one scanline between rasterizer
/// iterations.
///
/// Redux uses two *arrays of 3 pointers* (one for the left walk, one
/// for the right) plus a section index that decrements on pop. A
/// single-edge side (the long v1→v3) has 2 entries (section count 1);
/// a two-edge side (v1→v2 + v2→v3) has 3 entries (section count 2).
/// We mirror that exactly.
#[derive(Clone, Debug)]
pub(super) struct SlTriSetup {
    /// Sorted vertices: `[v1 (top), v2 (middle), v3 (bottom)]`, by Y
    /// ascending. Stored as owned values because we shuffle pointers
    /// into the left/right arrays below.
    sorted: [SlVertex; 3],
    /// Left-edge walk: `[bottom, maybe_middle, top]` -- `left_section`
    /// indexes the highest-unvisited entry; section descends toward 0.
    pub(super) left_array: [usize; 3],
    pub(super) right_array: [usize; 3],
    /// Number of edge segments remaining on each side. Starts at 1
    /// (single-edge long side) or 2 (pivot-at-middle), decrements in
    /// `next_row_*` when a section exhausts.
    pub(super) left_section: i32,
    pub(super) right_section: i32,

    // --- Current scanline state (updated every row). ---
    /// Left X on the current scanline, Q16.16.
    pub(super) left_x: i64,
    /// Right X on the current scanline, Q16.16. Redux stores this pre-
    /// shifted by 16; the rasterizer reads `rightX >> 16 - 1` as the
    /// inclusive right edge.
    pub(super) right_x: i64,
    /// Pre-step-to-add for left_x each scanline (Q16.16). Changes
    /// whenever the left section pops.
    pub(super) delta_left_x: i64,
    pub(super) delta_right_x: i64,
    /// Rows remaining in the currently-active section. Hits zero →
    /// pop to the next section, recompute deltas.
    pub(super) left_section_height: i32,
    pub(super) right_section_height: i32,

    // --- Gouraud colour at left edge, current scanline (Q16.16). ---
    pub(super) left_r: i64,
    pub(super) left_g: i64,
    pub(super) left_b: i64,
    pub(super) delta_left_r: i64,
    pub(super) delta_left_g: i64,
    pub(super) delta_left_b: i64,

    // --- UV at left edge, current scanline (Q16.16). ---
    pub(super) left_u: i64,
    pub(super) left_v: i64,
    pub(super) delta_left_u: i64,
    pub(super) delta_left_v: i64,

    // --- Per-column (per-X) deltas, computed once at setup time. ---
    //
    // Named "delta_right_*" to match Redux's `m_deltaRightR` / `m_deltaRightU`
    // (which are also mis-named -- they're per-column, not per-edge).
    pub(super) delta_col_r: i64,
    pub(super) delta_col_g: i64,
    pub(super) delta_col_b: i64,
    pub(super) delta_col_u: i64,
    pub(super) delta_col_v: i64,

    // --- Scanline bounds ---
    pub(super) y_min: i32,
    pub(super) y_max: i32,
}

impl SlTriSetup {
    /// Pop the active left section to the next shorter one. Returns
    /// `Err` when the pop runs out of sections (signals the triangle
    /// walk is done).
    fn pop_left_section(&mut self) -> Result<(), ()> {
        self.left_section -= 1;
        if self.left_section <= 0 {
            return Err(());
        }
        self.compute_left_section()
    }

    /// Pop the active right section; same contract as `pop_left_section`.
    fn pop_right_section(&mut self) -> Result<(), ()> {
        self.right_section -= 1;
        if self.right_section <= 0 {
            return Err(());
        }
        self.compute_right_section()
    }

    /// Recompute `left_x` / `delta_left_x` / colour + UV start + deltas
    /// from the pair of vertices defining the currently-active left
    /// section.
    fn compute_left_section(&mut self) -> Result<(), ()> {
        let idx1 = self.left_array[self.left_section as usize];
        let idx2 = self.left_array[(self.left_section - 1) as usize];
        let v1 = self.sorted[idx1];
        let v2 = self.sorted[idx2];
        let height = v2.y - v1.y;
        if height == 0 {
            return Err(());
        }
        let h = height as i64;
        self.delta_left_x = (v2.x - v1.x) / h;
        self.left_x = v1.x;
        // Gouraud + UV tracking are only meaningful when the caller
        // populated them; zero-divides can't happen because h != 0.
        self.delta_left_r = (v2.r - v1.r) / h;
        self.delta_left_g = (v2.g - v1.g) / h;
        self.delta_left_b = (v2.b - v1.b) / h;
        self.left_r = v1.r;
        self.left_g = v1.g;
        self.left_b = v1.b;
        self.delta_left_u = (v2.u - v1.u) / h;
        self.delta_left_v = (v2.v - v1.v) / h;
        self.left_u = v1.u;
        self.left_v = v1.v;
        self.left_section_height = height;
        Ok(())
    }

    fn compute_right_section(&mut self) -> Result<(), ()> {
        let idx1 = self.right_array[self.right_section as usize];
        let idx2 = self.right_array[(self.right_section - 1) as usize];
        let v1 = self.sorted[idx1];
        let v2 = self.sorted[idx2];
        let height = v2.y - v1.y;
        if height == 0 {
            return Err(());
        }
        let h = height as i64;
        self.delta_right_x = (v2.x - v1.x) / h;
        self.right_x = v1.x;
        self.right_section_height = height;
        Ok(())
    }

    /// Advance one scanline. Returns `Err` when the triangle's bottom
    /// edge is past.
    pub(super) fn next_row(&mut self) -> Result<(), ()> {
        self.left_section_height -= 1;
        if self.left_section_height <= 0 {
            self.pop_left_section()?;
        } else {
            self.left_x += self.delta_left_x;
            self.left_r += self.delta_left_r;
            self.left_g += self.delta_left_g;
            self.left_b += self.delta_left_b;
            self.left_u += self.delta_left_u;
            self.left_v += self.delta_left_v;
        }
        self.right_section_height -= 1;
        if self.right_section_height <= 0 {
            self.pop_right_section()?;
        } else {
            self.right_x += self.delta_right_x;
        }
        Ok(())
    }
}

/// `(x << 10) / y` with i64 intermediate, matching Redux's
/// `shl10idiv` helper at `soft.h:276`.
fn shl10_idiv(x: i64, y: i64) -> i64 {
    (x << 10) / y
}

/// Core setup: sort 3 vertices by Y, pick which side has the pivot
/// Hardware extent rule: any triangle whose vertex pairs span more
/// than 1023 pixels horizontally or 511 vertically is silently
/// dropped on real PS1 hardware. Off-screen geometry coming out of
/// projection lands here constantly -- without this gate it
/// rasterises as a giant garbage smear instead of being culled.
///
/// The check is per-edge, not bounding-box: hardware compares each
/// pair of vertices independently. Quads are already split into
/// two triangles by the caller, so each half is gated separately --
/// matching hardware behaviour where one half of a quad can survive
/// while the other gets dropped.
pub(super) fn triangle_exceeds_hw_extent(v0: (i32, i32), v1: (i32, i32), v2: (i32, i32)) -> bool {
    const MAX_DX: i32 = 1023;
    const MAX_DY: i32 = 511;
    let edges = [(v0, v1), (v1, v2), (v2, v0)];
    edges
        .iter()
        .any(|(a, b)| (a.0 - b.0).abs() > MAX_DX || (a.1 - b.1).abs() > MAX_DY)
}

/// (the "middle" vertex v2), and seed left / right walks. Colour + UV
/// are optional -- pass zeros for the ones a particular primitive
/// doesn't use. Returns the setup ready for the scanline loop, or
/// `None` when the triangle has zero height or zero "longest" width
/// (both degenerate).
///
/// "longest" meaning: Redux computes the signed horizontal distance
/// from v2.x to where the long v1→v3 edge crosses y=v2.y. Positive →
/// the long edge is to the RIGHT of v2 → v1 is on the left side and
/// the two-edge walk lives on the left. Negative → the inverse.
pub(super) fn setup_sections(
    v_x: [i32; 3],
    v_y: [i32; 3],
    v_rgb: [(i32, i32, i32); 3],
    v_uv: [(i32, i32); 3],
) -> Option<SlTriSetup> {
    // Build unsorted vertex structs. X/U/V are shifted into Q16.16
    // up front; colour channels get `<< 16` to match Redux's
    // `v->R = rgb & 0x00ff0000` convention.
    let mut verts = [SlVertex::default(); 3];
    for i in 0..3 {
        verts[i] = SlVertex {
            x: (v_x[i] as i64) << 16,
            y: v_y[i],
            r: (v_rgb[i].0 as i64) << 16,
            g: (v_rgb[i].1 as i64) << 16,
            b: (v_rgb[i].2 as i64) << 16,
            u: (v_uv[i].0 as i64) << 16,
            v: (v_uv[i].1 as i64) << 16,
        };
    }
    // Sort by y ascending: bubble sort is fine for n=3.
    if verts[0].y > verts[1].y {
        verts.swap(0, 1);
    }
    if verts[0].y > verts[2].y {
        verts.swap(0, 2);
    }
    if verts[1].y > verts[2].y {
        verts.swap(1, 2);
    }

    let v1 = &verts[0]; // top
    let v2 = &verts[1]; // middle
    let v3 = &verts[2]; // bottom

    let height = v3.y - v1.y;
    if height == 0 {
        return None;
    }
    // `temp = (v2.y - v1.y) / height` in Q16.16.
    let temp = ((v2.y - v1.y) as i64) << 16;
    let temp = temp / (height as i64);
    // longest = temp * (v3.x - v1.x) / (2^16) + (v1.x - v2.x)
    //   -- i.e. extrapolate the v1→v3 edge to y=v2.y, subtract v2.x.
    // Both factors of `temp` are already in Q16.16, so `(v3.x - v1.x) >> 16`
    // drops the fixed fraction before multiply (matches Redux).
    let longest = temp * ((v3.x - v1.x) >> 16) + (v1.x - v2.x);
    if longest == 0 {
        return None;
    }

    let mut setup = SlTriSetup {
        sorted: [verts[0], verts[1], verts[2]],
        left_array: [0; 3],
        right_array: [0; 3],
        left_section: 0,
        right_section: 0,
        left_x: 0,
        right_x: 0,
        delta_left_x: 0,
        delta_right_x: 0,
        left_section_height: 0,
        right_section_height: 0,
        left_r: 0,
        left_g: 0,
        left_b: 0,
        delta_left_r: 0,
        delta_left_g: 0,
        delta_left_b: 0,
        left_u: 0,
        left_v: 0,
        delta_left_u: 0,
        delta_left_v: 0,
        delta_col_r: 0,
        delta_col_g: 0,
        delta_col_b: 0,
        delta_col_u: 0,
        delta_col_v: 0,
        y_min: v1.y,
        y_max: v3.y - 1, // top-left rule: bottom row excluded
    };

    // Layout the left/right arrays depending on which side has the pivot.
    // sorted[] is indexed: 0=v1(top), 1=v2(middle), 2=v3(bottom).
    if longest < 0 {
        // Long edge v1→v3 is on the RIGHT. Left = single edge v1→v3.
        // Right walks v3 → v2 → v1 (two sections).
        setup.right_array = [2, 1, 0];
        setup.right_section = 2;
        setup.left_array = [2, 0, 0];
        setup.left_section = 1;
        setup.compute_left_section().ok()?;
        // Redux: if the first right section degenerates (height 0),
        // pop once and try again. Handles triangles where v1 == v2 in Y.
        if setup.compute_right_section().is_err() {
            setup.right_section -= 1;
            setup.compute_right_section().ok()?;
        }
    } else {
        // Long edge v1→v3 is on the LEFT. Left walks v3 → v2 → v1.
        // Right = single edge v1→v3.
        setup.left_array = [2, 1, 0];
        setup.left_section = 2;
        setup.right_array = [2, 0, 0];
        setup.right_section = 1;
        setup.compute_right_section().ok()?;
        if setup.compute_left_section().is_err() {
            setup.left_section -= 1;
            setup.compute_left_section().ok()?;
        }
    }

    // Clamp `longest` to ±0x1000 (Redux does this as `if (longest <
    // 0x1000) longest = 0x1000` and symmetric for the other sign --
    // prevents pathological per-column deltas when the triangle is
    // degenerately thin horizontally).
    let longest_clamped: i64 = if longest < 0 {
        longest.min(-0x1000)
    } else {
        longest.max(0x1000)
    };

    // Per-column deltas. The formula is Redux's `shl10idiv(temp * ((v3->X
    // - v1->X) >> 10) + ((v1->X - v2->X) << 6), longest)` for each of
    // R/G/B/U/V. The >> 10 and << 6 line up with `temp`'s Q16.16 so
    // the final shl10idiv produces a Q16.16 per-column delta.
    let compute_col_delta = |a3: i64, a1: i64, a2: i64| -> i64 {
        shl10_idiv(
            (temp * ((a3 - a1) >> 10)) + ((a1 - a2) << 6),
            longest_clamped,
        )
    };
    setup.delta_col_r = compute_col_delta(v3.r, v1.r, v2.r);
    setup.delta_col_g = compute_col_delta(v3.g, v1.g, v2.g);
    setup.delta_col_b = compute_col_delta(v3.b, v1.b, v2.b);
    setup.delta_col_u = compute_col_delta(v3.u, v1.u, v2.u);
    setup.delta_col_v = compute_col_delta(v3.v, v1.v, v2.v);

    Some(setup)
}
