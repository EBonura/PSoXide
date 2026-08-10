//! Resident PXBSP scene selected by the brush Play manifest.

use alloc::vec;
use alloc::vec::Vec;

use psx_asset::Texture;
use psx_bsp::collision::{Trace, TraceScratch, Q12_ONE};
use psx_bsp::mover::BrushDoorSet;
use psx_bsp::pxbsp::{entity_class, entity_flags, material_blend};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::{
    configure_projection, load_pxbsp_view, Camera, PxbspTextureBinding, Renderer,
};
use psx_bsp::{SliceReader, Vec3I32};
use psx_engine::{button, telemetry, App, Config, Ctx, OtFrame, Scene};
use psx_gpu::material::TextureWindow;
use psx_gpu::VideoMode;
use psx_math::{cos_q12, sin_q12};
use psx_vram::{upload_bytes, Clut, TexDepth, Tpage, VramRect};
use psxed_format::texture::Depth;

use crate::generated_brush::{BRUSH_TEXTURES, BRUSH_TEXTURE_ASSET_IDS, BRUSH_WORLD_PXBSP};
use crate::{OT, PRIMITIVE_PACKETS};

const MAX_DOORS: usize = 16;
const PLAYER_HULL_INDEX: usize = 1;
const WALK_SPEED_Q12: i32 = 4 * 4096;
const TURN_STEP_Q12: i32 = 16;
const STICK_DEADZONE: i16 = 24;
const USE_DISTANCE_UNITS: i32 = 192;
const CAMERA_HEIGHT_Q12: i32 = 48 * 4096;
const FIRST_TEXTURE_X: u16 = 320;
const TEXTURE_COLUMNS: u16 = (1024 - FIRST_TEXTURE_X) / 64;
const FIRST_CLUT_Y: u16 = 480;
const MAP_POSITION_BIAS: i32 = 1_000_000;

pub struct BrushPlaytest {
    map: PxbspResidentMap,
    renderer: Renderer,
    doors: BrushDoorSet<MAX_DOORS>,
    materials: Vec<Option<PxbspTextureBinding>>,
    trace_scratch: TraceScratch,
    player_origin: Vec3I32,
    player_yaw: u16,
}

impl BrushPlaytest {
    pub fn new() -> Self {
        Self {
            map: PxbspResidentMap::with_capacity(BRUSH_WORLD_PXBSP.len()),
            renderer: Renderer::new(),
            doors: BrushDoorSet::EMPTY,
            materials: Vec::new(),
            trace_scratch: TraceScratch::new(),
            player_origin: Vec3I32 { x: 0, y: 0, z: 0 },
            player_yaw: 0,
        }
    }

    fn load_map(&mut self) {
        let mut reader = SliceReader::new(BRUSH_WORLD_PXBSP);
        self.map.load(0, &mut reader).expect("cooked brush PXBSP");
        self.doors
            .init_from_map(&self.map)
            .expect("cooked brush doors");

        let mut spawn = None;
        let entities = self.map.entities();
        for index in 0..entities.len() {
            let entity = entities.get(index).expect("checked brush entity");
            if entity.class_id == entity_class::PLAYER_SPAWN
                && entity.flags & entity_flags::ENABLED != 0
            {
                assert!(spawn.is_none(), "brush Play has multiple player spawns");
                spawn = Some(entity);
            }
        }
        let spawn = spawn.expect("brush Play has no player spawn");
        self.player_origin = spawn.origin;
        self.player_yaw = spawn.angles.y as u16 & 0x0fff;
        self.materials = upload_material_textures(&self.map);
    }

    fn toggle_nearest_door(&mut self) {
        // ponytail: proximity use is the first-playable interaction ceiling.
        // Replace it with compiled PXBSP trigger and logic records when that
        // entity layer lands, keeping BrushDoorSet as the shared mover state.
        let limit_squared = i64::from(USE_DISTANCE_UNITS) * i64::from(USE_DISTANCE_UNITS);
        let nearest = self
            .doors
            .iter()
            .enumerate()
            .filter_map(|(index, door)| {
                if !door.enabled() {
                    return None;
                }
                let origin = door.transform().origin;
                let dx = i64::from((origin.x - self.player_origin.x) >> 12);
                let dy = i64::from((origin.y - self.player_origin.y) >> 12);
                let dz = i64::from((origin.z - self.player_origin.z) >> 12);
                let distance_squared = dx * dx + dy * dy + dz * dz;
                (distance_squared <= limit_squared).then_some((distance_squared, index))
            })
            .min_by_key(|(distance_squared, _)| *distance_squared)
            .map(|(_, index)| index);
        if let Some(index) = nearest {
            self.doors
                .get_mut(index)
                .expect("nearest door index")
                .toggle();
        }
    }

    fn move_player(&mut self, ctx: &Ctx) {
        let (left_x, left_y) = ctx.pad.sticks.left_centered();
        let stick_turn = if left_x.abs() > STICK_DEADZONE {
            i32::from(left_x) / 8
        } else {
            0
        };
        let digital_turn = i32::from(ctx.is_held(button::RIGHT))
            .saturating_sub(i32::from(ctx.is_held(button::LEFT)))
            * TURN_STEP_Q12;
        self.player_yaw = (i32::from(self.player_yaw) + stick_turn + digital_turn) as u16 & 0x0fff;

        let analog_forward = if left_y.abs() > STICK_DEADZONE {
            -i32::from(left_y)
        } else {
            0
        };
        let digital_forward = i32::from(ctx.is_held(button::UP))
            .saturating_sub(i32::from(ctx.is_held(button::DOWN)))
            * 127;
        let forward = if digital_forward != 0 {
            digital_forward
        } else {
            analog_forward
        };
        if forward == 0 {
            return;
        }
        let distance = WALK_SPEED_Q12.saturating_mul(forward) / 127;
        let candidate = Vec3I32 {
            x: self
                .player_origin
                .x
                .saturating_add((cos_q12(self.player_yaw) * distance) >> 12),
            y: self.player_origin.y,
            z: self
                .player_origin
                .z
                .saturating_add((sin_q12(self.player_yaw) * distance) >> 12),
        };

        // ponytail: this thin adapter proves the new hull source in embedded
        // Play. Replace it with character_motor's PXBSP trace provider when
        // the shared motor rebase lands, restoring gravity, steps and slopes.
        let mut moved = Trace::default();
        assert!(trace_player(
            &self.map,
            &self.doors,
            &mut self.trace_scratch,
            &self.player_origin,
            &candidate,
            &mut moved,
        ));
        if moved.fraction == Q12_ONE {
            self.player_origin = moved.end;
            return;
        }
        let along_x = Vec3I32 {
            x: candidate.x,
            ..self.player_origin
        };
        let mut x_trace = Trace::default();
        assert!(trace_player(
            &self.map,
            &self.doors,
            &mut self.trace_scratch,
            &self.player_origin,
            &along_x,
            &mut x_trace,
        ));
        self.player_origin = x_trace.end;
        let along_z = Vec3I32 {
            z: candidate.z,
            ..self.player_origin
        };
        let mut z_trace = Trace::default();
        assert!(trace_player(
            &self.map,
            &self.doors,
            &mut self.trace_scratch,
            &self.player_origin,
            &along_z,
            &mut z_trace,
        ));
        self.player_origin = z_trace.end;
    }

    fn camera(&self) -> Camera {
        Camera {
            origin: Vec3I32 {
                x: self.player_origin.x,
                y: self.player_origin.y.saturating_add(CAMERA_HEIGHT_Q12),
                z: self.player_origin.z,
            },
            angles: [0, self.player_yaw as i16, 0],
        }
    }

    fn emit_map_telemetry(&self) {
        let camera = self.camera();
        for (counter, coordinate) in [
            (
                telemetry::counter::ROOM_CAMERA_GLOBAL_X_BIASED,
                camera.origin.x,
            ),
            (
                telemetry::counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
                camera.origin.y,
            ),
            (
                telemetry::counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
                camera.origin.z,
            ),
        ] {
            let units = coordinate >> 12;
            telemetry::counter(
                counter,
                units.saturating_add(MAP_POSITION_BIAS).max(0) as u32,
            );
        }
        telemetry::counter(
            telemetry::counter::ROOM_PLAYER_VIEW_YAW_Q12,
            u32::from(self.player_yaw),
        );
    }
}

fn trace_player(
    map: &PxbspResidentMap,
    doors: &BrushDoorSet<MAX_DOORS>,
    scratch: &mut TraceScratch,
    start: &Vec3I32,
    end: &Vec3I32,
    output: &mut Trace,
) -> bool {
    let mut best = Trace::default();
    if !map
        .model_collision_hull(0, PLAYER_HULL_INDEX)
        .expect("brush world player hull")
        .trace_into(start, end, scratch, &mut best)
    {
        return false;
    }
    for door in doors.iter() {
        let mut trace = Trace::default();
        if !map
            .model_collision_hull(door.model_index(), PLAYER_HULL_INDEX)
            .expect("brush door player hull")
            .transformed(door.transform())
            .trace_into(start, end, scratch, &mut trace)
        {
            return false;
        }
        if trace.fraction < best.fraction {
            best = trace;
        }
    }
    *output = best;
    true
}

impl Scene for BrushPlaytest {
    fn init(&mut self, _ctx: &mut Ctx) {
        configure_projection();
        self.load_map();
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if ctx.just_pressed(button::CROSS) {
            self.toggle_nearest_door();
        }
        self.doors.tick();
        self.move_player(ctx);
        self.emit_map_telemetry();
    }

    fn render(&mut self, ctx: &mut Ctx) {
        let camera = self.camera();
        let view = load_pxbsp_view(camera);
        // ponytail: this checkpoint gives the resident brush renderer the
        // complete packet scratch. Replace it with the shared bounded frame
        // allocation model during gameplay unification.
        let packets = unsafe { PRIMITIVE_PACKETS.words_mut() };
        let mut used = self
            .renderer
            .draw_pxbsp_world(
                &self.map,
                camera,
                view,
                &self.materials,
                ctx.sim_tick.as_u32(),
                packets,
            )
            .packet_words;
        for door in self.doors.iter() {
            let Some(frame) = self.renderer.draw_pxbsp_model(
                &self.map,
                door.model_index(),
                door.transform(),
                camera,
                view,
                &self.materials,
                ctx.sim_tick.as_u32(),
                &mut packets[used..],
            ) else {
                continue;
            };
            used = used.saturating_add(frame.packet_words);
        }

        let mut ot = OtFrame::begin(unsafe { &mut OT });
        unsafe {
            let first = packets.as_mut_ptr();
            ot.add_tagged_packet_stream_unchecked(first, first.add(used));
        }
        ot.submit_async().detach();
    }
}

fn upload_material_textures(map: &PxbspResidentMap) -> Vec<Option<PxbspTextureBinding>> {
    assert_eq!(
        BRUSH_TEXTURE_ASSET_IDS.len(),
        BRUSH_TEXTURES.len(),
        "brush texture manifest arrays differ"
    );
    // ponytail: the resident checkpoint packs its complete texture set into a
    // fixed VRAM region. Replace this with shared asset residency when brush
    // worlds join the gameplay runtime.
    let mut bindings = vec![None; map.materials().len()];
    let mut page_x = FIRST_TEXTURE_X;
    let mut page_y = 0;
    for (texture_index, (&asset_id, &bytes)) in BRUSH_TEXTURE_ASSET_IDS
        .iter()
        .zip(BRUSH_TEXTURES.iter())
        .enumerate()
    {
        let texture = Texture::from_bytes(bytes).expect("cooked brush texture");
        let depth = match texture.depth() {
            Depth::Bit4 => TexDepth::Bit4,
            Depth::Bit8 => TexDepth::Bit8,
            Depth::Bit15 => TexDepth::Bit15,
        };
        let columns = texture.halfwords_per_row().div_ceil(64);
        assert!(
            columns > 0 && columns <= TEXTURE_COLUMNS,
            "brush texture is too wide"
        );
        if page_x + columns * 64 > 1024 {
            assert_eq!(page_y, 0, "brush Play texture pages exceed VRAM");
            page_x = FIRST_TEXTURE_X;
            page_y = 256;
        }
        assert!(texture.height() <= 256, "brush texture is too tall");
        let tpage = Tpage::new(page_x, page_y, depth);
        let translucent = map.materials().iter().any(|material| {
            material.texture_asset == asset_id && material.blend_mode != material_blend::OPAQUE
        });
        upload_texture_pixels(tpage, texture, translucent);
        let clut = if texture.clut_entries() == 0 {
            None
        } else {
            let clut_y = FIRST_CLUT_Y
                .checked_add(texture_index as u16)
                .expect("brush CLUT row overflow");
            assert!(clut_y < 512, "brush Play CLUT rows exceed VRAM");
            let clut = Clut::new(0, clut_y);
            upload_texture_clut(clut, texture, translucent);
            Some(clut)
        };
        let width = u8::try_from(texture.width()).expect("brush texture width fits u8");
        let height = u8::try_from(texture.height()).expect("brush texture height fits u8");
        let window = TextureWindow::power_of_two_tile(0, 0, width, height);
        let binding = PxbspTextureBinding {
            texture_page: tpage.uv_tpage_word(0),
            clut: clut.map_or(0, Clut::uv_clut_word),
            texture_window_word: window.word(),
            uv_origin: [0, 0],
            texture_size: [width, height],
        };
        for (material_index, material) in map.materials().iter().enumerate() {
            if material.texture_asset == asset_id {
                bindings[material_index] = Some(binding);
            }
        }
        page_x += columns * 64;
    }
    assert!(
        bindings.iter().all(Option::is_some),
        "unresolved brush texture"
    );
    bindings
}

fn upload_texture_pixels(tpage: Tpage, texture: Texture<'_>, translucent: bool) {
    let rect = VramRect::new(
        tpage.x(),
        tpage.y(),
        texture.halfwords_per_row(),
        texture.height(),
    );
    if texture.depth() != Depth::Bit15 {
        upload_bytes(rect, texture.pixel_bytes());
        return;
    }
    let mut pixels = texture.pixel_bytes().to_vec();
    stamp_texture_words(&mut pixels, texture.index_zero_transparent(), translucent);
    upload_bytes(rect, &pixels);
}

fn upload_texture_clut(clut: Clut, texture: Texture<'_>, translucent: bool) {
    let mut colors = texture.clut_bytes().to_vec();
    stamp_texture_words(&mut colors, texture.index_zero_transparent(), translucent);
    upload_bytes(
        VramRect::new(clut.x(), clut.y(), texture.clut_entries(), 1),
        &colors,
    );
}

fn stamp_texture_words(bytes: &mut [u8], transparent_zero: bool, translucent: bool) {
    for color in bytes.chunks_exact_mut(2) {
        let mut value = u16::from_le_bytes([color[0], color[1]]);
        if value == 0 && !transparent_zero {
            value = 1;
        }
        if value != 0 && translucent {
            value |= 0x8000;
        }
        color.copy_from_slice(&value.to_le_bytes());
    }
}

pub fn run() -> ! {
    let scene = unsafe {
        static mut SCENE: core::mem::MaybeUninit<BrushPlaytest> = core::mem::MaybeUninit::uninit();
        let scene = core::ptr::addr_of_mut!(SCENE).cast::<BrushPlaytest>();
        scene.write(BrushPlaytest::new());
        &mut *scene
    };
    #[cfg(target_arch = "mips")]
    psx_rt::assert_stack_headroom();
    App::run(
        Config {
            clear_color: (5, 7, 12),
            video_mode: VideoMode::Ntsc,
            ..Config::default()
        },
        scene,
    )
}
