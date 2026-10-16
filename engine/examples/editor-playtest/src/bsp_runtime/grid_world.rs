//! Open-hall experiment: draw the world from a grid render pack instead of
//! the PXBSP face pass. Collision, visibility queries, entities and the UI
//! keep using the PXBSP; only world rendering changes (`grid-world` feature).

use psx_level::MAX_ROOM_MATERIALS;

use psx_bsp::pxbsp::{material_flags, PxbspMaterial};
use psx_bsp::RecordSlice;
use psx_bsp::render::grid::{GridFrame, GridMaterial, GridPack, GridRenderer};
use psx_bsp::render::{FrustumPlanes, PxbspTextureBinding, ViewTransform};

#[repr(C)]
struct Aligned<Bytes: ?Sized> {
    _align: [u32; 0],
    bytes: Bytes,
}

/// The cooked pack, word aligned. Empty when the cook was not asked for one.
static GRID_WORLD: &[u8] = {
    static ALIGNED: &Aligned<[u8]> = &Aligned {
        _align: [],
        bytes: *include_bytes!("../../generated/grid_world.bin"),
    };
    &ALIGNED.bytes
};

pub(super) struct GridWorld {
    pack: GridPack<'static>,
    renderer: GridRenderer,
    materials: [GridMaterial; MAX_ROOM_MATERIALS],
    bound: bool,
}

impl GridWorld {
    pub(super) fn load() -> Self {
        let pack = GridPack::parse(GRID_WORLD)
            .unwrap_or_else(|error| panic!("grid pack refused: {error:?} ({} bytes)", GRID_WORLD.len()));
        let renderer = GridRenderer::new(&pack);
        assert!(pack.materials.len() <= MAX_ROOM_MATERIALS, "grid pack has too many materials");
        let materials = [GridMaterial::default(); MAX_ROOM_MATERIALS];
        Self {
            pack,
            renderer,
            materials,
            bound: false,
        }
    }

    pub(super) fn unbind(&mut self) {
        self.bound = false;
        for material in self.materials.iter_mut() {
            material.resident = false;
        }
    }

    /// Resolve every grid material to the VRAM binding the PXBSP already made
    /// for the same texture asset.
    fn bind(&mut self, pxbsp_materials: RecordSlice<'_, PxbspMaterial>, bindings: &[Option<PxbspTextureBinding>]) {
        for (slot, &asset) in self.pack.materials.iter().enumerate() {
            let found = pxbsp_materials.iter().enumerate().find_map(|(index, material)| {
                if material.flags & (material_flags::SKY_APERTURE | material_flags::DIRECTIONAL_SKY) != 0
                    || material.texture_asset != asset
                {
                    return None;
                }
                bindings.get(index).copied().flatten()
            });
            self.materials[slot] = match found {
                Some(binding) => {
                    let [u, v] = binding.page_uv_origin;
                    GridMaterial {
                        // Opaque: blend bits clear.
                        tpage: binding.texture_page & !0x0060,
                        clut: binding.clut,
                        uv_origin: u16::from(u) | (u16::from(v) << 8),
                        // A tile spans 64 page-local texels; the packed add in
                        // the packet writer must not carry between bytes.
                        resident: u16::from(u) + 64 <= 256 && u16::from(v) + 64 <= 256,
                    }
                }
                None => GridMaterial::default(),
            };
        }
        self.bound = true;
    }

    pub(super) fn pack_bytes(&self) -> usize {
        GRID_WORLD.len()
    }

    pub(super) fn working_bytes(&self) -> usize {
        self.renderer.working_bytes() + core::mem::size_of::<[GridMaterial; MAX_ROOM_MATERIALS]>()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(
        &mut self,
        pxbsp_materials: RecordSlice<'_, PxbspMaterial>,
        bindings: &[Option<PxbspTextureBinding>],
        frustum: &FrustumPlanes,
        view: &ViewTransform,
        camera_world: [i32; 3],
        screen: [i32; 2],
        focal_length: i32,
        packets: &mut [u32],
    ) -> GridFrame {
        if !self.bound {
            self.bind(pxbsp_materials, bindings);
        }
        self.renderer.draw(
            &self.pack,
            frustum,
            view,
            camera_world,
            screen,
            focal_length,
            &self.materials[..self.pack.materials.len()],
            packets,
        )
    }
}
