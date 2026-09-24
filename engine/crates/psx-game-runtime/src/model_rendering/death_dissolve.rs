//! A corpse sheds its final posed triangles from the highest surfaces down.
//!
//! Every face the sweep passes breaks off, rises, tumbles about its own
//! centre and fades. The corpse holds its final pose for the whole
//! dissolve, so its posed vertices are captured once, relative to the
//! corpse, into a [`ModelDeathCapture`]. Each frame then projects that
//! capture instead of re-posing the model, and moves the flying faces and
//! projects them three corners per GTE `RTPT`, instead of unprojecting and
//! reprojecting every face on the CPU (twelve divides and one full submit a
//! face; about 350 flying faces a frame at the peak on the light enemy).

use super::*;
use psx_engine::{LoadedAnchoredCameraGte, Vec3I16};
use psx_gpu::material::TexturedPacketMaterial;

/// Capture marker, in the height slot, for a vertex not yet seen in front of
/// the camera. The sweep reads only heights, so it tests only those.
const UNCAPTURED: i16 = i16::MIN;

/// The shard loop keeps more state live than the R3000 has registers, and
/// every spill reload from the main-RAM stack stalls. On the scratchpad it
/// costs a cycle. Nothing else holds scratchpad bytes while a model's faces
/// are submitted (see the model face walker's own stack in psx-engine), and
/// the loop's call tree only writes packets.
type ShardStack = psx_engine::scratchpad::ScratchpadStack<0, { psx_engine::scratchpad::SIZE }>;
const _: () = psx_engine::scratchpad::assert_disjoint(&[ShardStack::REGION]);

/// Flying faces are collected in batches of this many and then moved,
/// projected and emitted together, so the classifying loop and the shard
/// loop each stay small enough to keep their state in registers.
const SHARD_BATCH: usize = 48;

/// Anchor-relative world positions of one dissolving corpse's posed
/// vertices, captured from the frozen pose the first time each vertex is
/// seen in front of the camera. All-zero is the empty state, so a static
/// capture lives in `.bss`.
pub struct ModelDeathCapture<const CAP: usize> {
    /// Owning model instance plus one; zero when free.
    owner: u16,
    /// Sim tick after which the owner's dissolve has certainly ended.
    end_tick: u32,
    anchor: WorldVertex,
    len: u16,
    missing: u16,
    bottom: i32,
    top: i32,
    vertices: [[i16; 3]; CAP],
}

impl<const CAP: usize> ModelDeathCapture<CAP> {
    /// Empty capture.
    pub const fn new() -> Self {
        Self {
            owner: 0,
            end_tick: 0,
            anchor: WorldVertex::ZERO,
            len: 0,
            missing: 0,
            bottom: 0,
            top: 0,
            vertices: [[0; 3]; CAP],
        }
    }

    /// Hold the capture for `instance`'s dissolve, starting a new one when
    /// the slot is free, its owner's dissolve is over, or this is a new death
    /// of the same instance. `false` while another corpse still owns it.
    fn claim(
        &mut self,
        instance: usize,
        effect: ModelDeathDissolve,
        now: u32,
        anchor: WorldVertex,
        vertex_count: usize,
    ) -> bool {
        let owner = instance as u16 + 1;
        let end_tick = now + u32::from(effect.duration.saturating_sub(effect.elapsed));
        // One death keeps one end tick; allow a tick of rounding either way.
        if self.owner == owner && self.end_tick.abs_diff(end_tick) <= 1 {
            return true;
        }
        if self.owner != 0 && self.owner != owner && now <= self.end_tick {
            return false;
        }
        let len = vertex_count.min(CAP);
        *self = Self {
            owner,
            end_tick,
            anchor,
            len: len as u16,
            missing: len as u16,
            bottom: i32::MAX,
            top: i32::MIN,
            ..*self
        };
        for vertex in &mut self.vertices[..len] {
            vertex[1] = UNCAPTURED;
        }
        true
    }

    /// Capture every vertex that is in front of the camera for the first time.
    fn capture(&mut self, projected: &[ProjectedVertex], camera: WorldCamera) {
        if self.missing == 0 {
            return;
        }
        let len = usize::from(self.len).min(projected.len());
        for (slot, &p) in self.vertices[..len].iter_mut().zip(projected) {
            if slot[1] != UNCAPTURED || p == ProjectedVertex::INVALID {
                continue;
            }
            let w = dash_assembly::unproject(p, camera);
            let relative = [
                w.x - self.anchor.x,
                w.y - self.anchor.y,
                w.z - self.anchor.z,
            ];
            if relative
                .iter()
                .all(|&v| v > i32::from(UNCAPTURED) && v <= i32::from(i16::MAX))
            {
                *slot = relative.map(|v| v as i16);
                self.missing -= 1;
                self.bottom = self.bottom.min(relative[1]);
                self.top = self.top.max(relative[1]);
            }
        }
    }
}

impl<const CAP: usize> ModelDeathCapture<CAP> {
    /// Every vertex of `instance`'s corpse is captured for this death, so
    /// its frozen pose need not be animated and skinned again.
    pub(super) fn holds_whole(
        &self,
        instance: usize,
        effect: ModelDeathDissolve,
        now: u32,
    ) -> bool {
        let end_tick = now + u32::from(effect.duration.saturating_sub(effect.elapsed));
        self.owner == instance as u16 + 1
            && self.end_tick.abs_diff(end_tick) <= 1
            && self.missing == 0
            && self.len > 0
    }

    /// Project the captured pose into `out` three vertices per `RTPT` and
    /// return how many were written.
    pub(super) fn project(&self, camera: WorldCamera, out: &mut [ProjectedVertex]) -> usize {
        let len = usize::from(self.len).min(out.len());
        let gte = LoadedAnchoredCameraGte::load(camera, self.anchor);
        let point = |i: usize| {
            let v = self.vertices[i.min(len - 1)];
            Vec3I16::new(v[0], v[1], v[2])
        };
        let mut i = 0;
        while i < len {
            let projected = gte.project_points([point(i), point(i + 1), point(i + 2)]);
            for (k, p) in projected.into_iter().enumerate() {
                if i + k < len {
                    out[i + k] = p;
                }
            }
            i += 3;
        }
        len
    }
}

impl<const CAP: usize> Default for ModelDeathCapture<CAP> {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateless five-second dissolve, timed after the death clip's final frame.
#[derive(Copy, Clone, Debug)]
pub struct ModelDeathDissolve {
    elapsed: u16,
    duration: u16,
}

impl ModelDeathDissolve {
    /// Keep playing the death clip before starting the dissolve.
    pub fn after_death(
        death_ticks: u16,
        frames: u16,
        sample_hz: u16,
        video_hz: VideoHz,
    ) -> Option<Self> {
        let hz = video_hz.as_nonzero_u32();
        let final_tick =
            (u32::from(frames.saturating_sub(1)) * hz).div_ceil(u32::from(sample_hz.max(1)));
        let elapsed = u32::from(death_ticks).checked_sub(final_tick)?;
        Some(Self {
            elapsed: elapsed.min(65535) as u16,
            duration: (hz * 5) as u16,
        })
    }

    /// Finished corpses need neither a pose nor any render submissions.
    pub const fn finished(self) -> bool {
        self.elapsed >= self.duration
    }

    #[cfg(test)]
    fn age(self, y: i32, bottom: i32, top: i32) -> u16 {
        let sweep = i32::from(self.duration) * 3 / 5;
        let delay = (top - y).clamp(0, top - bottom) * sweep / (top - bottom).max(1);
        self.elapsed.saturating_sub(delay as u16)
    }

    fn flight_ticks(self) -> u16 {
        self.duration * 2 / 5
    }
}

fn center(points: [WorldVertex; 3]) -> WorldVertex {
    WorldVertex::new(
        points.iter().map(|p| p.x).sum::<i32>() / 3,
        points.iter().map(|p| p.y).sum::<i32>() / 3,
        points.iter().map(|p| p.z).sum::<i32>() / 3,
    )
}

fn drift(points: [WorldVertex; 3], index: usize, progress: i32) -> [WorldVertex; 3] {
    let c = center(points);
    let sign = if index & 1 == 0 { 1 } else { -1 };
    let angle = Angle::from_q12((sign * progress * 2) as u16);
    let (sin, cos) = (angle.sin_q12(), angle.cos_q12());
    let lift = progress * (48 + (index & 3) as i32 * 6) / 256;
    let dx = ((index & 7) as i32 - 3) * progress / 64;
    let dz = ((index & 3) as i32 - 1) * progress / 32;
    points.map(|p| {
        let (x, y, z) = (p.x - c.x, p.y - c.y, p.z - c.z);
        // Rotate around each fragment's own centre, not the corpse's root.
        WorldVertex::new(
            c.x + dx + x,
            c.y + lift + ((y * cos - z * sin) >> 12),
            c.z + dz + ((y * sin + z * cos) >> 12),
        )
    })
}

/// Submission state for one corpse: the run of faces still on the body
/// waiting to be batched, the flying faces' packet words, and the stats.
struct Dissolver<'p, 'f, 'w, 'a, 'b, S, const OT_DEPTH: usize> {
    projected: &'p [ProjectedVertex],
    faces: &'f [TexturedModelRenderFace],
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    packets: ShardPackets,
    triangles: &'w mut S,
    world: &'w mut WorldRenderPass<'a, 'b, OT_DEPTH>,
    stats: TexturedModelRenderStats,
    /// Consecutive faces still on the body. They are drawn exactly as the
    /// live model draws them, one packed batch instead of one call (and one
    /// depth-slot setup) per face; a face the packed path would refuse on its
    /// own goes out alone, so every packet stays the same.
    run_start: usize,
    run_end: usize,
}

impl<S: PrimitiveSink<TriTextured>, const OT_DEPTH: usize>
    Dissolver<'_, '_, '_, '_, '_, S, OT_DEPTH>
{
    fn overflowed(&self) -> bool {
        self.stats.primitive_overflow || self.stats.command_overflow
    }

    #[inline(never)]
    fn flush(&mut self) {
        if self.run_start < self.run_end {
            let next = self.world.submit_batchable_projected_model_faces(
                self.triangles,
                self.projected,
                &self.faces[self.run_start..self.run_end],
                self.material,
                self.options,
            );
            accumulate_model_stats(&mut self.stats, next);
        }
        self.run_start = self.run_end;
    }

    /// Keep face `index` on the body.
    fn keep(&mut self, index: usize, corners: [ProjectedVertex; 3]) {
        if projected_triangle_batchable(corners) {
            if self.run_end != index {
                self.flush();
                self.run_start = index;
            }
            self.run_end = index + 1;
        } else {
            self.flush();
            let next = self.world.submit_projected_model_faces(
                self.triangles,
                self.projected,
                &self.faces[index..index + 1],
                self.material,
                self.options,
            );
            accumulate_model_stats(&mut self.stats, next);
            self.run_start = index + 1;
            self.run_end = index + 1;
        }
    }

    /// Emit face `index`, `progress / 256` of the way through its flight, at
    /// its moved screen corners. `false` once the arenas are full.
    #[inline(always)]
    fn emit(
        &mut self,
        packets: &ShardPackets,
        index: usize,
        moved: [ProjectedVertex; 3],
        progress: i32,
    ) -> bool {
        let face = self.faces[index];
        let (packet, fading) = packets.for_face(index, progress, face.palette_bank());
        let options = if fading {
            &packets.fading_options
        } else {
            &packets.opaque_options
        };
        self.world.submit_projected_packed_triangle(
            self.triangles,
            moved,
            face.uv_words(),
            packet,
            options,
            &mut self.stats,
        )
    }
}

/// Where the sweep stands for one corpse this frame.
#[derive(Copy, Clone)]
struct Sweep {
    effect: ModelDeathDissolve,
    top: i32,
    span: i32,
    ticks: i32,
}

impl Sweep {
    fn new(effect: ModelDeathDissolve, bottom: i32, top: i32) -> Self {
        Self {
            effect,
            top,
            span: top - bottom,
            ticks: i32::from(effect.duration) * 3 / 5,
        }
    }

    /// [`ModelDeathDissolve::age`] with the corpse's span taken once.
    #[inline(always)]
    fn age(self, y: i32) -> u16 {
        let delay = (self.top - y).clamp(0, self.span) * self.ticks / self.span.max(1);
        self.effect.elapsed.saturating_sub(delay as u16)
    }
}

/// Packet words and options for flying faces, prepared once a frame. A
/// fading face differs from the opaque one only in its modulation colour.
#[derive(Copy, Clone)]
struct ShardPackets {
    opaque: TexturedPacketMaterial,
    fading: TexturedPacketMaterial,
    tint: (i32, i32, i32),
    opaque_options: WorldSurfaceOptions,
    fading_options: WorldSurfaceOptions,
}

impl ShardPackets {
    fn new(material: TextureMaterial, options: WorldSurfaceOptions) -> Self {
        let fading = fading_material(material, 0);
        let (r, g, b) = material.tint();
        Self {
            opaque: TexturedPacketMaterial::from_texture(material),
            fading: TexturedPacketMaterial::from_texture(fading),
            tint: (i32::from(r), i32::from(g), i32::from(b)),
            opaque_options: options
                .with_material_layer(material)
                .with_cull_mode(CullMode::None),
            fading_options: options
                .with_material_layer(fading)
                .with_cull_mode(CullMode::None),
        }
    }

    /// The packet words the original per-face path builds for face `index`
    /// at `progress`, from `fading_material` / the plain material.
    #[inline(always)]
    fn for_face(&self, index: usize, progress: i32, bank: u8) -> (TexturedPacketMaterial, bool) {
        if progress > 32 + (index as i32 & 31) {
            let strength = 256 - progress;
            let mut packet = self.fading;
            packet.color_command_word |= (((self.tint.0 * strength) >> 8) as u32 & 0xff)
                | ((((self.tint.1 * strength) >> 8) as u32 & 0xff) << 8)
                | ((((self.tint.2 * strength) >> 8) as u32 & 0xff) << 16);
            (packet.with_clut_bank(bank), true)
        } else {
            (self.opaque.with_clut_bank(bank), false)
        }
    }
}

/// Palettes mark nonzero texels for STP, as in the player's dash.
/// Quarter-strength blending avoids a bright shell where pieces overlap.
/// Fade texture modulation to zero without shrinking facets.
fn fading_material(material: TextureMaterial, strength: i32) -> TextureMaterial {
    let (r, g, b) = material.tint();
    material
        .with_raw_texture(false)
        .with_blend_mode(BlendMode::AddQuarter)
        .with_tint((
            ((i32::from(r) * strength) >> 8) as u8,
            ((i32::from(g) * strength) >> 8) as u8,
            ((i32::from(b) * strength) >> 8) as u8,
        ))
}

/// Where the corpse's corners come from: its capture, or (while another
/// corpse holds the capture) this frame's pose, unprojected. Both give
/// positions relative to `anchor`, so both project through the GTE.
struct Corners<'c, 'p, const CAP: usize> {
    capture: Option<&'c ModelDeathCapture<CAP>>,
    projected: &'p [ProjectedVertex],
    camera: WorldCamera,
    anchor: WorldVertex,
}

impl<const CAP: usize> Corners<'_, '_, CAP> {
    /// Vertices with a known place: this frame's pose, and the capture's.
    fn len(&self) -> usize {
        match self.capture {
            Some(capture) => usize::from(capture.len).min(self.projected.len()),
            None => self.projected.len(),
        }
    }

    /// Height of corner `i` above the anchor, when known this frame.
    #[inline(always)]
    fn height(&self, i: usize) -> Option<i32> {
        match self.capture {
            Some(capture) => {
                let y = capture.vertices[i][1];
                (y != UNCAPTURED).then_some(i32::from(y))
            }
            None => (self.projected[i] != ProjectedVertex::INVALID).then(|| self.live_point(i).y),
        }
    }

    #[inline(always)]
    fn point(&self, i: usize) -> WorldVertex {
        match self.capture {
            Some(capture) => {
                let v = capture.vertices[i];
                WorldVertex::new(i32::from(v[0]), i32::from(v[1]), i32::from(v[2]))
            }
            None => self.live_point(i),
        }
    }

    /// Corner `i` unprojected from this frame's pose, kept out of line so the
    /// captured case carries none of the camera through its loops.
    #[inline(never)]
    fn live_point(&self, i: usize) -> WorldVertex {
        let w = dash_assembly::unproject(self.projected[i], self.camera);
        WorldVertex::new(
            w.x - self.anchor.x,
            w.y - self.anchor.y,
            w.z - self.anchor.z,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw<const CAP: usize, const OT_DEPTH: usize>(
    effect: ModelDeathDissolve,
    capture: Option<&mut ModelDeathCapture<CAP>>,
    instance: usize,
    now: u32,
    anchor: WorldVertex,
    projected: &[ProjectedVertex],
    faces: &[TexturedModelRenderFace],
    camera: WorldCamera,
    material: TextureMaterial,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) -> TexturedModelRenderStats {
    if effect.finished() {
        return TexturedModelRenderStats::default();
    }
    let capture = capture.and_then(|capture| {
        capture
            .claim(instance, effect, now, anchor, projected.len())
            .then_some(capture)
    });
    let (capture, anchor, bottom, top) = match capture {
        Some(capture) => {
            capture.capture(projected, camera);
            (Some(&*capture), capture.anchor, capture.bottom, capture.top)
        }
        None => {
            // Another corpse holds the capture: find this one's height span
            // from the pose, as every frame did before the capture existed.
            let mut bottom = i32::MAX;
            let mut top = i32::MIN;
            for &p in projected.iter().filter(|p| **p != ProjectedVertex::INVALID) {
                let y = dash_assembly::unproject(p, camera).y - anchor.y;
                bottom = bottom.min(y);
                top = top.max(y);
            }
            (None, anchor, bottom, top)
        }
    };
    if bottom > top {
        return TexturedModelRenderStats::default();
    }
    let options = options.with_textured_triangle_splitting(false);
    let mut out = Dissolver {
        projected,
        faces,
        material,
        options,
        packets: ShardPackets::new(material, options),
        triangles,
        world,
        stats: TexturedModelRenderStats::default(),
        run_start: 0,
        run_end: 0,
    };
    let corners = Corners {
        capture,
        projected,
        camera,
        anchor,
    };
    sweep_faces(effect, Sweep::new(effect, bottom, top), &corners, &mut out);
    out.stats
}

/// Classify every face against the sweep: kept faces go out in batched runs,
/// flying ones in batches of [`SHARD_BATCH`].
#[inline(never)]
fn sweep_faces<const CAP: usize, S: PrimitiveSink<TriTextured>, const OT_DEPTH: usize>(
    effect: ModelDeathDissolve,
    sweep: Sweep,
    corners: &Corners<'_, '_, CAP>,
    out: &mut Dissolver<'_, '_, '_, '_, '_, S, OT_DEPTH>,
) {
    let flight = effect.flight_ticks();
    let len = corners.len();
    let mut batch = [0u32; SHARD_BATCH];
    let mut pending = 0;
    let (faces, projected) = (out.faces, out.projected);
    for (index, face) in faces.iter().enumerate() {
        let [a, b, c] = face.vertex_indices().map(usize::from);
        if a >= len || b >= len || c >= len {
            continue;
        }
        // A corner never seen in front of the camera is behind it now, and
        // a face with such a corner is not drawn this frame.
        let (Some(ya), Some(yb), Some(yc)) =
            (corners.height(a), corners.height(b), corners.height(c))
        else {
            continue;
        };
        let age = sweep.age((ya + yb + yc) / 3);
        if age == 0 {
            let kept = [projected[a], projected[b], projected[c]];
            if !kept.contains(&ProjectedVertex::INVALID) {
                out.keep(index, kept);
                if out.overflowed() {
                    return;
                }
            }
        } else if age < flight {
            batch[pending] = index as u32 | u32::from(age) << 16;
            pending += 1;
            if pending == SHARD_BATCH {
                if !fly_batch(effect, corners, &batch, out) {
                    return;
                }
                pending = 0;
            }
        }
    }
    if fly_batch(effect, corners, &batch[..pending], out) {
        out.flush();
    }
}

/// Move, project and emit a batch of flying faces (`index | age << 16`),
/// after the kept faces before them. `false` once the arenas are full.
#[inline(never)]
fn fly_batch<const CAP: usize, S: PrimitiveSink<TriTextured>, const OT_DEPTH: usize>(
    effect: ModelDeathDissolve,
    corners: &Corners<'_, '_, CAP>,
    batch: &[u32],
    out: &mut Dissolver<'_, '_, '_, '_, '_, S, OT_DEPTH>,
) -> bool {
    out.flush();
    if out.overflowed() {
        return false;
    }
    out.run_start = out.faces.len();
    out.run_end = out.faces.len();
    let flight = i32::from(effect.flight_ticks());
    let gte = LoadedAnchoredCameraGte::load(corners.camera, corners.anchor);
    // SAFETY: see `ShardStack`; tools/stack_guard.py proves the tree fits.
    unsafe { ShardStack::run(|| fly_on_stack(flight, gte, corners, batch, out)) }
}

#[inline(always)]
fn fly_on_stack<const CAP: usize, S: PrimitiveSink<TriTextured>, const OT_DEPTH: usize>(
    flight: i32,
    gte: LoadedAnchoredCameraGte,
    corners: &Corners<'_, '_, CAP>,
    batch: &[u32],
    out: &mut Dissolver<'_, '_, '_, '_, '_, S, OT_DEPTH>,
) -> bool {
    // A local copy lives on the scratchpad stack; the fields behind `out`
    // would be reloaded from main RAM for every shard.
    let packets = out.packets;
    let faces = out.faces;
    for &entry in batch {
        let index = (entry & 0xffff) as usize;
        let progress = ((entry >> 16) as i32 * 256) / flight;
        let points = faces[index]
            .vertex_indices()
            .map(|i| corners.point(usize::from(i)));
        let moved = drift(points, index, progress);
        let fits = moved.iter().all(|p| {
            (p.x.wrapping_add(0x8000) | p.y.wrapping_add(0x8000) | p.z.wrapping_add(0x8000)) as u32
                <= 0xffff
        });
        if !fits {
            continue;
        }
        let Some(moved) =
            gte.project_triangle(moved.map(|p| Vec3I16::new(p.x as i16, p.y as i16, p.z as i16)))
        else {
            continue;
        };
        if !out.emit(&packets, index, moved, progress) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn final_pose_precedes_five_second_dissolve_at_both_video_rates() {
        for hz in [50, 60] {
            let video = VideoHz::from_u16(hz);
            let end = 2 * hz;
            assert!(ModelDeathDissolve::after_death(end - 1, 25, 12, video).is_none());
            assert!(
                !ModelDeathDissolve::after_death(end + 5 * hz - 1, 25, 12, video)
                    .unwrap()
                    .finished()
            );
            assert!(ModelDeathDissolve::after_death(end + 5 * hz, 25, 12, video)
                .unwrap()
                .finished());
        }
    }

    #[test]
    fn horizontal_corpse_uses_posed_height_and_lower_faces_leave_later() {
        let effect = ModelDeathDissolve {
            elapsed: 90,
            duration: 300,
        };
        assert_eq!(effect.age(18, 2, 18), 90);
        assert_eq!(effect.age(2, 2, 18), 0);
        assert_eq!(effect.age(10, 2, 18), 0);
        let final_effect = ModelDeathDissolve {
            elapsed: 300,
            ..effect
        };
        assert!(final_effect.age(2, 2, 18) >= final_effect.flight_ticks());
    }

    #[test]
    fn prepared_shard_packets_match_the_per_face_packets() {
        use psx_gpu::prim::TriTextured;
        let words = |t: TriTextured| {
            let p = &t as *const TriTextured as *const u32;
            // SAFETY: TriTextured is a plain repr(C) run of u32 words.
            unsafe { core::slice::from_raw_parts(p, TriTextured::WORDS as usize + 1) }.to_vec()
        };
        let verts = [(10, 20), (-30, 44), (100, 7)];
        for material in [
            TextureMaterial::new(0x7c0, 0x1234).with_tint((128, 96, 200)),
            TextureMaterial::new(0x3c10, 0x0412)
                .with_tint((255, 255, 255))
                .with_raw_texture(true),
        ] {
            let options = WorldSurfaceOptions::new(
                psx_engine::DepthBand::whole(),
                psx_engine::DepthRange::new(0, 1000),
            );
            let packets = ShardPackets::new(material, options);
            for index in [0usize, 5, 31, 64] {
                for progress in [1, 33, 64, 128, 200, 255] {
                    for bank in 0..4u8 {
                        let face = TexturedModelRenderFace::new_with_palette_bank(
                            [0, 1, 2],
                            [(3, 250), (17, 0), (255, 128)],
                            bank,
                        );
                        let fades = progress > 32 + (index as i32 & 31);
                        let mat = if fades {
                            fading_material(material, 256 - progress)
                        } else {
                            material
                        };
                        let old = TriTextured::with_material_packet_texcoords(
                            verts,
                            face.uvs(),
                            mat.with_clut_bank(bank),
                        );
                        let (packet, fading) = packets.for_face(index, progress, bank);
                        assert_eq!(fading, fades);
                        let new = TriTextured::with_packet_material_packed_uv_words(
                            verts,
                            face.uv_words(),
                            packet,
                        );
                        assert_eq!(words(new), words(old), "{index} {progress} {bank}");
                        let layer = options.with_material_layer(mat).render_layer;
                        let got = if fading {
                            packets.fading_options.render_layer
                        } else {
                            packets.opaque_options.render_layer
                        };
                        assert_eq!(got, layer);
                    }
                }
            }
        }
    }

    #[test]
    fn sweep_age_is_the_divided_age_exactly() {
        for elapsed in [0, 1, 17, 90, 179, 180, 250, 299] {
            let effect = ModelDeathDissolve {
                elapsed,
                duration: 300,
            };
            for (bottom, top) in [(2, 18), (-500, 1_700), (0, 1), (40, 40), (-9_000, 7_000)] {
                let sweep = Sweep::new(effect, bottom, top);
                for y in (bottom - 5..=top + 5).step_by(1 + ((top - bottom) / 400) as usize) {
                    assert_eq!(sweep.age(y), effect.age(y, bottom, top), "{elapsed} {y}");
                }
            }
        }
    }

    #[test]
    fn a_capture_holds_one_death_and_frees_when_it_ends() {
        let mut capture = std::boxed::Box::new(ModelDeathCapture::<16>::new());
        let at = |elapsed| ModelDeathDissolve {
            elapsed,
            duration: 300,
        };
        let anchor = WorldVertex::new(40_000, -600, 9_000);
        assert!(capture.claim(3, at(0), 1_000, anchor, 12));
        assert_eq!(capture.missing, 12);
        capture.missing = 5;
        assert!(capture.claim(3, at(40), 1_040, anchor, 12), "same death");
        assert_eq!(capture.missing, 5, "same death keeps its capture");
        assert!(!capture.claim(4, at(0), 1_050, anchor, 12), "busy");
        assert!(capture.claim(4, at(0), 1_301, anchor, 12), "owner finished");
        assert_eq!(capture.missing, 12);
        assert!(
            capture.claim(4, at(0), 5_000, anchor, 12),
            "a new death restarts"
        );
    }

    #[test]
    fn captured_corners_are_the_unprojected_pose_relative_to_the_anchor() {
        use psx_engine::{WorldProjection, Q12};
        let camera = WorldCamera::from_basis(
            WorldProjection::new(160, 120, 256, 16),
            WorldVertex::new(-10_752, 2_400, -24_000),
            Q12::from_raw(1_567),
            Q12::from_raw(3_784),
            Q12::from_raw(-900),
            Q12::from_raw(3_996),
        );
        let projected = [
            ProjectedVertex::new(150, 100, 1_200),
            ProjectedVertex::INVALID,
            ProjectedVertex::new(158, 120, 1_190),
        ];
        let anchor = WorldVertex::new(-10_000, 900, -23_000);
        let mut capture = std::boxed::Box::new(ModelDeathCapture::<4>::new());
        let effect = ModelDeathDissolve {
            elapsed: 0,
            duration: 300,
        };
        assert!(capture.claim(0, effect, 0, anchor, 3));
        capture.capture(&projected, camera);
        assert_eq!(capture.missing, 1);
        let captured = Corners {
            capture: Some(&*capture),
            projected: &projected,
            camera,
            anchor,
        };
        let live = Corners::<4> {
            capture: None,
            projected: &projected,
            camera,
            anchor,
        };
        assert_eq!(captured.len(), 3);
        assert_eq!(captured.height(1), None, "vertex 1 was never seen");
        assert_eq!(live.height(1), None);
        let seen = dash_assembly::unproject(projected[2], camera);
        let relative = WorldVertex::new(seen.x - anchor.x, seen.y - anchor.y, seen.z - anchor.z);
        for corners in [&captured, &live] {
            assert_eq!(corners.point(2), relative);
            assert_eq!(corners.height(2), Some(relative.y));
        }
    }

    #[test]
    fn fragments_rise_from_their_own_centres_without_changing_size() {
        let points = [
            WorldVertex::new(-10, 0, 0),
            WorldVertex::new(10, 0, 0),
            WorldVertex::new(0, 12, 0),
        ];
        assert_eq!(drift(points, 0, 0), points);
        let moved = drift(points, 0, 128);
        assert!(center(moved).y > center(points).y + 20);
        let distance = |a: WorldVertex, b: WorldVertex| {
            (a.x - b.x).pow(2) + (a.y - b.y).pow(2) + (a.z - b.z).pow(2)
        };
        assert!((distance(moved[0], moved[1]) - distance(points[0], points[1])).abs() < 60);
    }
}
