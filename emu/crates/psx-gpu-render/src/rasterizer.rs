//! Compute-shader rasterizer dispatcher.
//!
//! Phase B.1: one primitive, one dispatch. The pipeline objects are
//! built once and reused; each `dispatch_*` call writes the
//! primitive's parameters into a uniform buffer, picks the matching
//! pipeline, and dispatches a workgroup grid sized to the bounding
//! box.
//!
//! ## Provenance
//!
//! Portions of this module are parity-matched against, and in places
//! derived from, PCSX-Redux (<https://github.com/grumpycoders/pcsx-redux>),
//! Copyright (C) the PCSX-Redux authors, GPL-2.0-or-later. Points of
//! correspondence are flagged inline with `Redux` references. PSoXide is
//! released under GPL-2.0-or-later in part to honor this lineage; see
//! `LICENSE` and `docs/license-audit.md`.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

#[cfg(test)]
use crate::primitive::{BlendMode, PrimFlags};
use crate::primitive::{
    DrawArea, Fill, MonoRect, MonoTri, ShadedTexTri, ShadedTri, TexQuadBilinear, TexRect, TexTri,
    Tpage,
};
use crate::scanline::{self, RowState, ScanlineConsts};
use crate::vram::VramGpu;

const WORKGROUP_SIZE_X: u32 = 8;
const WORKGROUP_SIZE_Y: u32 = 8;

/// Holds every wgpu pipeline object the rasterizer needs. Built once
/// per `VramGpu` and reused for the lifetime of the device.
pub struct Rasterizer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,

    // Shared 3-binding layout (VRAM + prim + draw area) + the
    // per-primitive uniform buffers. The layout is used by the
    // mono-rect pipeline; the uniforms feed the scanline dispatches.
    mono_tri_bg_layout: wgpu::BindGroupLayout,
    mono_tri_uniform: wgpu::Buffer,
    draw_area_uniform: wgpu::Buffer,

    // Shared 4-binding layout (VRAM + prim + draw area + tpage),
    // used by the tex-rect and tex-quad-bilinear pipelines.
    tex_tri_bg_layout: wgpu::BindGroupLayout,
    tex_tri_uniform: wgpu::Buffer,
    tpage_uniform: wgpu::Buffer,

    // Mono-rectangle pipeline (B.5.a). Reuses `mono_tri_bg_layout`
    // since the binding shape is identical (VRAM + prim + draw area).
    mono_rect_pipeline: wgpu::ComputePipeline,
    mono_rect_uniform: wgpu::Buffer,

    // Textured-rectangle pipeline (B.5.b). Reuses `tex_tri_bg_layout`.
    tex_rect_pipeline: wgpu::ComputePipeline,
    tex_rect_uniform: wgpu::Buffer,

    // Fill pipeline (B.5.c). Custom 2-binding shape -- no draw area
    // because fill bypasses clipping.
    fill_pipeline: wgpu::ComputePipeline,
    fill_bg_layout: wgpu::BindGroupLayout,
    fill_uniform: wgpu::Buffer,

    // Per-primitive uniforms for the shaded / shaded-tex scanline
    // dispatches.
    shaded_tri_uniform: wgpu::Buffer,
    shaded_tex_tri_uniform: wgpu::Buffer,

    // Phase B.x: textured triangle with bit-exact scanline-delta UV
    // interpolation. Custom 6-binding shape because it adds a
    // per-row storage buffer + per-primitive scanline-consts uniform.
    tex_tri_scanline_pipeline: wgpu::ComputePipeline,
    tex_tri_scanline_bg_layout: wgpu::BindGroupLayout,
    tex_tri_scanline_consts: wgpu::Buffer,
    /// Resizable per-row storage buffer. Reallocated when a primitive
    /// needs more rows than the current capacity (cheap -- wgpu
    /// doesn't actually free until `submit` completes anyway).
    tex_tri_scanline_rows: std::cell::RefCell<wgpu::Buffer>,

    // Phase B.x: shaded-textured triangle with bit-exact scanline-
    // delta UV + RGB interpolation. Reuses tex_tri_scanline_bg_layout
    // (same 6-binding shape).
    shaded_tex_tri_scanline_pipeline: wgpu::ComputePipeline,
    shaded_tex_tri_scanline_consts: wgpu::Buffer,
    shaded_tex_tri_scanline_rows: std::cell::RefCell<wgpu::Buffer>,

    // Phase C bug fix: axis-aligned textured quad with bilinear UV.
    // Same 4-binding shape as tex_tri (VRAM + prim + draw_area + tpage).
    tex_quad_bilinear_pipeline: wgpu::ComputePipeline,
    tex_quad_bilinear_uniform: wgpu::Buffer,

    // Phase B.x: mono + shaded triangle scanline pipelines. Same
    // 5-binding shape (VRAM + prim + draw area + rows + consts --
    // no tpage since neither samples a texture).
    mono_shaded_scanline_bg_layout: wgpu::BindGroupLayout,
    mono_tri_scanline_pipeline: wgpu::ComputePipeline,
    mono_tri_scanline_consts: wgpu::Buffer,
    mono_tri_scanline_rows: std::cell::RefCell<wgpu::Buffer>,
    shaded_tri_scanline_pipeline: wgpu::ComputePipeline,
    shaded_tri_scanline_consts: wgpu::Buffer,
    shaded_tri_scanline_rows: std::cell::RefCell<wgpu::Buffer>,
}

impl Rasterizer {
    /// Build all pipelines on top of the same device that owns
    /// `VramGpu`. Cheap to call multiple times in tests but in
    /// production you want one shared instance.
    pub fn new(vram: &VramGpu) -> Self {
        let device = vram.device().clone();
        let queue = vram.queue().clone();

        // Bind group: VRAM (storage), primitive uniform, draw-area
        // uniform. All three are visible only to compute stages.
        let mono_tri_bg_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("psx-rasterizer-mono-tri-bgl"),
                entries: &[
                    // 0: VRAM storage buffer (read_write).
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // 1: Primitive uniform (read-only).
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // 2: DrawArea uniform.
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        // Reusable uniform buffers -- wgpu doesn't let us write a
        // struct directly into a freshly-bound resource per dispatch
        // without allocating, so we keep a stable buffer and update
        // it via `queue.write_buffer`.
        let mono_tri_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-mono-tri-uniform"),
            size: std::mem::size_of::<MonoTri>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draw_area_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-draw-area-uniform"),
            // DrawArea is 16 bytes already, but pad-up to wgpu's
            // minimum uniform buffer offset alignment to be safe.
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Textured-triangle pipeline ----------
        let tex_tri_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("psx-rasterizer-tex-tri-bgl"),
            entries: &[
                // 0: VRAM
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // 1: TexTri uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // 2: DrawArea uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // 3: Tpage uniform
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let tex_tri_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tex-tri-uniform"),
            size: std::mem::size_of::<TexTri>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tpage_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tpage-uniform"),
            size: std::mem::size_of::<Tpage>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Mono-rectangle pipeline (B.5.a) ----------
        // Same 3-binding shape as the mono-triangle path: VRAM,
        // primitive uniform, draw area. Reuse the layout directly.
        let mono_rect_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-mono-rect-pl"),
            bind_group_layouts: &[&mono_tri_bg_layout],
            push_constant_ranges: &[],
        });
        let mono_rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-mono-rect-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/mono_rect.wgsl").into()),
        });
        let mono_rect_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("psx-rasterizer-mono-rect"),
            layout: Some(&mono_rect_pl),
            module: &mono_rect_shader,
            entry_point: Some("rasterize"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mono_rect_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-mono-rect-uniform"),
            size: std::mem::size_of::<MonoRect>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Textured-rectangle pipeline (B.5.b) ----------
        // Same 4-binding shape as the textured-triangle path: VRAM,
        // primitive uniform, draw area, tpage. Reuse the layout.
        let tex_rect_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-tex-rect-pl"),
            bind_group_layouts: &[&tex_tri_bg_layout],
            push_constant_ranges: &[],
        });
        let tex_rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-tex-rect-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/tex_rect.wgsl").into()),
        });
        let tex_rect_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("psx-rasterizer-tex-rect"),
            layout: Some(&tex_rect_pl),
            module: &tex_rect_shader,
            entry_point: Some("rasterize"),
            compilation_options: Default::default(),
            cache: None,
        });
        let tex_rect_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tex-rect-uniform"),
            size: std::mem::size_of::<TexRect>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Fill pipeline (B.5.c) ----------
        // 2 bindings: VRAM + Fill uniform. No draw area / no tpage --
        // fill bypasses clipping and never reads VRAM.
        let fill_bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("psx-rasterizer-fill-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let fill_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-fill-pl"),
            bind_group_layouts: &[&fill_bg_layout],
            push_constant_ranges: &[],
        });
        let fill_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-fill-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fill.wgsl").into()),
        });
        let fill_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("psx-rasterizer-fill"),
            layout: Some(&fill_pl),
            module: &fill_shader,
            entry_point: Some("rasterize"),
            compilation_options: Default::default(),
            cache: None,
        });
        let fill_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-fill-uniform"),
            size: std::mem::size_of::<Fill>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shaded_tri_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tri-uniform"),
            size: std::mem::size_of::<ShadedTri>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shaded_tex_tri_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tex-tri-uniform"),
            size: std::mem::size_of::<ShadedTexTri>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Tex-tri scanline pipeline (B.x) ----------
        // 6 bindings: VRAM, prim, draw area, tpage, per-row state
        // (storage), scanline consts (uniform).
        let tex_tri_scanline_bg_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("psx-rasterizer-tex-tri-scanline-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let tex_tri_scanline_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-tex-tri-scanline-pl"),
            bind_group_layouts: &[&tex_tri_scanline_bg_layout],
            push_constant_ranges: &[],
        });
        let tex_tri_scanline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-tex-tri-scanline-shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/tex_tri_scanline.wgsl").into(),
            ),
        });
        let tex_tri_scanline_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("psx-rasterizer-tex-tri-scanline"),
                layout: Some(&tex_tri_scanline_pl),
                module: &tex_tri_scanline_shader,
                entry_point: Some("rasterize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let tex_tri_scanline_consts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tex-tri-scanline-consts"),
            size: std::mem::size_of::<ScanlineConsts>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Pre-allocate room for a 512-row triangle (the max). Avoids
        // reallocating in the hot path. 512 × 64 = 32 KiB.
        let initial_rows_capacity_bytes = 512u64 * std::mem::size_of::<RowState>() as u64;
        let tex_tri_scanline_rows = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tex-tri-scanline-rows"),
            size: initial_rows_capacity_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Shaded-tex-tri scanline pipeline (B.x) ----------
        // Same 6-binding layout as tex_tri_scanline; just a different
        // shader entry that walks RGB in addition to UV.
        let shaded_tex_tri_scanline_pl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("psx-rasterizer-shaded-tex-tri-scanline-pl"),
                bind_group_layouts: &[&tex_tri_scanline_bg_layout],
                push_constant_ranges: &[],
            });
        let shaded_tex_tri_scanline_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("psx-rasterizer-shaded-tex-tri-scanline-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../shaders/shaded_tex_tri_scanline.wgsl").into(),
                ),
            });
        let shaded_tex_tri_scanline_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("psx-rasterizer-shaded-tex-tri-scanline"),
                layout: Some(&shaded_tex_tri_scanline_pl),
                module: &shaded_tex_tri_scanline_shader,
                entry_point: Some("rasterize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let shaded_tex_tri_scanline_consts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tex-tri-scanline-consts"),
            size: std::mem::size_of::<ScanlineConsts>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shaded_tex_tri_scanline_rows = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tex-tri-scanline-rows"),
            size: initial_rows_capacity_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Tex-quad bilinear pipeline (Phase C bug fix) ----------
        // Reuses `tex_tri_bg_layout` (same 4-binding shape: VRAM,
        // prim uniform, draw area, tpage). Different shader entry +
        // dedicated prim uniform.
        let tex_quad_bilinear_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-tex-quad-bilinear-pl"),
            bind_group_layouts: &[&tex_tri_bg_layout],
            push_constant_ranges: &[],
        });
        let tex_quad_bilinear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-tex-quad-bilinear-shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/tex_quad_bilinear.wgsl").into(),
            ),
        });
        let tex_quad_bilinear_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("psx-rasterizer-tex-quad-bilinear"),
                layout: Some(&tex_quad_bilinear_pl),
                module: &tex_quad_bilinear_shader,
                entry_point: Some("rasterize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let tex_quad_bilinear_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-tex-quad-bilinear-uniform"),
            size: std::mem::size_of::<TexQuadBilinear>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---------- Mono / Shaded tri scanline pipelines (B.x) ----------
        // 5-binding shape: VRAM + prim + draw area + rows + consts.
        let mono_shaded_scanline_bg_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("psx-rasterizer-mono-shaded-scanline-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let mono_shaded_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-rasterizer-mono-shaded-scanline-pl"),
            bind_group_layouts: &[&mono_shaded_scanline_bg_layout],
            push_constant_ranges: &[],
        });

        let mono_tri_scanline_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-rasterizer-mono-tri-scanline-shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/mono_tri_scanline.wgsl").into(),
            ),
        });
        let mono_tri_scanline_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("psx-rasterizer-mono-tri-scanline"),
                layout: Some(&mono_shaded_pl),
                module: &mono_tri_scanline_shader,
                entry_point: Some("rasterize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let mono_tri_scanline_consts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-mono-tri-scanline-consts"),
            size: std::mem::size_of::<ScanlineConsts>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mono_tri_scanline_rows = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-mono-tri-scanline-rows"),
            size: initial_rows_capacity_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shaded_tri_scanline_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("psx-rasterizer-shaded-tri-scanline-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../shaders/shaded_tri_scanline.wgsl").into(),
                ),
            });
        let shaded_tri_scanline_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("psx-rasterizer-shaded-tri-scanline"),
                layout: Some(&mono_shaded_pl),
                module: &shaded_tri_scanline_shader,
                entry_point: Some("rasterize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let shaded_tri_scanline_consts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tri-scanline-consts"),
            size: std::mem::size_of::<ScanlineConsts>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shaded_tri_scanline_rows = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-shaded-tri-scanline-rows"),
            size: initial_rows_capacity_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            queue,
            mono_tri_bg_layout,
            mono_tri_uniform,
            draw_area_uniform,
            tex_tri_bg_layout,
            tex_tri_uniform,
            tpage_uniform,
            mono_rect_pipeline,
            mono_rect_uniform,
            tex_rect_pipeline,
            tex_rect_uniform,
            fill_pipeline,
            fill_bg_layout,
            fill_uniform,
            shaded_tri_uniform,
            shaded_tex_tri_uniform,
            tex_tri_scanline_pipeline,
            tex_tri_scanline_bg_layout,
            tex_tri_scanline_consts,
            tex_tri_scanline_rows: std::cell::RefCell::new(tex_tri_scanline_rows),
            shaded_tex_tri_scanline_pipeline,
            shaded_tex_tri_scanline_consts,
            shaded_tex_tri_scanline_rows: std::cell::RefCell::new(shaded_tex_tri_scanline_rows),
            tex_quad_bilinear_pipeline,
            tex_quad_bilinear_uniform,
            mono_shaded_scanline_bg_layout,
            mono_tri_scanline_pipeline,
            mono_tri_scanline_consts,
            mono_tri_scanline_rows: std::cell::RefCell::new(mono_tri_scanline_rows),
            shaded_tri_scanline_pipeline,
            shaded_tri_scanline_consts,
            shaded_tri_scanline_rows: std::cell::RefCell::new(shaded_tri_scanline_rows),
        }
    }

    /// Bit-exact monochrome triangle dispatch. The host runs the CPU
    /// rasterizer's silicon-matched DDA (`scanline::build_setup`) and
    /// the shader tests each pixel against its row's
    /// `[left_x, right_x)` span, so coverage matches the CPU
    /// byte-for-byte.
    pub fn dispatch_mono_tri_scanline(
        &self,
        vram: &VramGpu,
        tri: &MonoTri,
        area: &DrawArea,
    ) -> bool {
        if tri.exceeds_hw_extent() {
            return false;
        }
        let v = [
            (tri.v0[0], tri.v0[1]),
            (tri.v1[0], tri.v1[1]),
            (tri.v2[0], tri.v2[1]),
        ];
        let setup = match scanline::build_setup(v, [(0, 0); 3], [(0, 0, 0); 3], false) {
            Some(s) => s,
            None => return false,
        };
        self.scanline_dispatch(
            vram,
            tri,
            std::mem::size_of::<MonoTri>() as u64,
            &self.mono_tri_scanline_pipeline,
            &self.mono_tri_scanline_consts,
            &self.mono_tri_scanline_rows,
            &setup,
            area,
            tri.bbox_max[0] - tri.bbox_min[0] + 1,
            tri.bbox_max[1] - tri.bbox_min[1] + 1,
            "mono",
        )
    }

    /// Bit-exact Gouraud-shaded triangle dispatch: silicon-matched
    /// DDA coverage + determinant-plane RGB interpolation.
    pub fn dispatch_shaded_tri_scanline(
        &self,
        vram: &VramGpu,
        tri: &ShadedTri,
        area: &DrawArea,
    ) -> bool {
        if tri.exceeds_hw_extent() {
            return false;
        }
        let v = [
            (tri.v0[0], tri.v0[1]),
            (tri.v1[0], tri.v1[1]),
            (tri.v2[0], tri.v2[1]),
        ];
        let unpack_rgb = |c: u32| {
            (
                (c & 0xFF) as i32,
                ((c >> 8) & 0xFF) as i32,
                ((c >> 16) & 0xFF) as i32,
            )
        };
        let rgb = [unpack_rgb(tri.c0), unpack_rgb(tri.c1), unpack_rgb(tri.c2)];
        let setup = match scanline::build_setup(v, [(0, 0); 3], rgb, true) {
            Some(s) => s,
            None => return false,
        };
        self.scanline_dispatch(
            vram,
            tri,
            std::mem::size_of::<ShadedTri>() as u64,
            &self.shaded_tri_scanline_pipeline,
            &self.shaded_tri_scanline_consts,
            &self.shaded_tri_scanline_rows,
            &setup,
            area,
            tri.bbox_max[0] - tri.bbox_min[0] + 1,
            tri.bbox_max[1] - tri.bbox_min[1] + 1,
            "shaded",
        )
    }

    /// Shared dispatch helper for the 5-binding scanline pipelines
    /// (mono + shaded). Writes the prim uniform from the host
    /// struct, uploads per-row state, and dispatches over the bbox.
    #[allow(clippy::too_many_arguments)]
    fn scanline_dispatch<P: bytemuck::Pod>(
        &self,
        vram: &VramGpu,
        tri: &P,
        _prim_size_bytes: u64,
        pipeline: &wgpu::ComputePipeline,
        consts_buf: &wgpu::Buffer,
        rows_cell: &std::cell::RefCell<wgpu::Buffer>,
        setup: &scanline::ScanlineSetup,
        area: &DrawArea,
        bbox_w: i32,
        bbox_h: i32,
        label: &'static str,
    ) -> bool {
        if bbox_w <= 0 || bbox_h <= 0 {
            return false;
        }
        // Both mono and shaded scanline paths reuse `mono_tri_uniform`
        // (mono) / `shaded_tri_uniform` (shaded) -- but to keep this
        // helper generic, we'll write through the existing per-prim
        // uniform we already manage. Looking up which one to use:
        let prim_uniform = match label {
            "mono" => &self.mono_tri_uniform,
            "shaded" => &self.shaded_tri_uniform,
            _ => unreachable!("unknown scanline-dispatch label: {label}"),
        };

        let rows_size_bytes = (setup.rows.len() as u64) * std::mem::size_of::<RowState>() as u64;
        {
            let mut rows_buf = rows_cell.borrow_mut();
            if rows_buf.size() < rows_size_bytes {
                *rows_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("psx-rasterizer-scanline-rows-grown"),
                    size: rows_size_bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
        }
        self.queue
            .write_buffer(prim_uniform, 0, bytemuck::bytes_of(tri));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));
        self.queue
            .write_buffer(consts_buf, 0, bytemuck::bytes_of(&setup.consts));
        let rows_buf = rows_cell.borrow();
        self.queue
            .write_buffer(&rows_buf, 0, bytemuck::cast_slice(&setup.rows));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-scanline-bg"),
            layout: &self.mono_shaded_scanline_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: prim_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: rows_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: consts_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-scanline-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-scanline-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = (bbox_w as u32).div_ceil(WORKGROUP_SIZE_X);
            let groups_y = (bbox_h as u32).div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        true
    }

    /// Bit-exact textured-Gouraud triangle dispatch: silicon-matched
    /// DDA coverage + determinant-plane interpolation for both the
    /// UV pair and the per-vertex tint, so the GPU output matches
    /// the CPU rasterizer byte-for-byte.
    pub fn dispatch_shaded_tex_tri_scanline(
        &self,
        vram: &VramGpu,
        tri: &ShadedTexTri,
        tpage: &Tpage,
        area: &DrawArea,
    ) -> bool {
        if tri.exceeds_hw_extent() {
            return false;
        }
        let v = [
            (tri.v0[0], tri.v0[1]),
            (tri.v1[0], tri.v1[1]),
            (tri.v2[0], tri.v2[1]),
        ];
        let uv = [
            ((tri.uv0 & 0xFF) as i32, ((tri.uv0 >> 8) & 0xFF) as i32),
            ((tri.uv1 & 0xFF) as i32, ((tri.uv1 >> 8) & 0xFF) as i32),
            ((tri.uv2 & 0xFF) as i32, ((tri.uv2 >> 8) & 0xFF) as i32),
        ];
        // Vertex tints are 24-bit RGB packed in the c0/c1/c2 fields.
        let unpack_rgb = |c: u32| {
            (
                (c & 0xFF) as i32,
                ((c >> 8) & 0xFF) as i32,
                ((c >> 16) & 0xFF) as i32,
            )
        };
        let rgb = [unpack_rgb(tri.c0), unpack_rgb(tri.c1), unpack_rgb(tri.c2)];
        let setup = match scanline::build_setup(v, uv, rgb, true) {
            Some(s) => s,
            None => return false,
        };

        let rows_size_bytes = (setup.rows.len() as u64) * std::mem::size_of::<RowState>() as u64;
        {
            let mut rows_buf = self.shaded_tex_tri_scanline_rows.borrow_mut();
            if rows_buf.size() < rows_size_bytes {
                *rows_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("psx-rasterizer-shaded-tex-tri-scanline-rows-grown"),
                    size: rows_size_bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
        }

        self.queue
            .write_buffer(&self.shaded_tex_tri_uniform, 0, bytemuck::bytes_of(tri));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));
        self.queue
            .write_buffer(&self.tpage_uniform, 0, bytemuck::bytes_of(tpage));
        self.queue.write_buffer(
            &self.shaded_tex_tri_scanline_consts,
            0,
            bytemuck::bytes_of(&setup.consts),
        );
        let rows_buf = self.shaded_tex_tri_scanline_rows.borrow();
        self.queue
            .write_buffer(&rows_buf, 0, bytemuck::cast_slice(&setup.rows));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-shaded-tex-tri-scanline-bg"),
            layout: &self.tex_tri_scanline_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.shaded_tex_tri_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.tpage_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: rows_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.shaded_tex_tri_scanline_consts.as_entire_binding(),
                },
            ],
        });

        let bbox_w = tri.bbox_max[0] - tri.bbox_min[0] + 1;
        let bbox_h = tri.bbox_max[1] - tri.bbox_min[1] + 1;
        if bbox_w <= 0 || bbox_h <= 0 {
            return false;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-shaded-tex-tri-scanline-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-shaded-tex-tri-scanline-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.shaded_tex_tri_scanline_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = (bbox_w as u32).div_ceil(WORKGROUP_SIZE_X);
            let groups_y = (bbox_h as u32).div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        true
    }

    /// Bit-exact textured-triangle dispatch: silicon-matched DDA
    /// coverage + determinant-plane UV interpolation. Results match
    /// the CPU rasterizer byte-for-byte.
    ///
    /// Returns `false` if the triangle degenerates (zero height or
    /// zero determinant) -- same drop conditions as the CPU.
    pub fn dispatch_tex_tri_scanline(
        &self,
        vram: &VramGpu,
        tri: &TexTri,
        tpage: &Tpage,
        area: &DrawArea,
    ) -> bool {
        if tri.exceeds_hw_extent() {
            return false;
        }
        let v = [
            (tri.v0[0], tri.v0[1]),
            (tri.v1[0], tri.v1[1]),
            (tri.v2[0], tri.v2[1]),
        ];
        let uv = [
            ((tri.uv0 & 0xFF) as i32, ((tri.uv0 >> 8) & 0xFF) as i32),
            ((tri.uv1 & 0xFF) as i32, ((tri.uv1 >> 8) & 0xFF) as i32),
            ((tri.uv2 & 0xFF) as i32, ((tri.uv2 >> 8) & 0xFF) as i32),
        ];
        let setup = match scanline::build_setup(v, uv, [(0, 0, 0); 3], true) {
            Some(s) => s,
            None => return false,
        };

        // Re-allocate per-row buffer if too small.
        let rows_size_bytes = (setup.rows.len() as u64) * std::mem::size_of::<RowState>() as u64;
        {
            let mut rows_buf = self.tex_tri_scanline_rows.borrow_mut();
            if rows_buf.size() < rows_size_bytes {
                *rows_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("psx-rasterizer-tex-tri-scanline-rows-grown"),
                    size: rows_size_bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
        }

        // Upload uniforms + per-row data.
        self.queue
            .write_buffer(&self.tex_tri_uniform, 0, bytemuck::bytes_of(tri));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));
        self.queue
            .write_buffer(&self.tpage_uniform, 0, bytemuck::bytes_of(tpage));
        self.queue.write_buffer(
            &self.tex_tri_scanline_consts,
            0,
            bytemuck::bytes_of(&setup.consts),
        );
        let rows_buf = self.tex_tri_scanline_rows.borrow();
        self.queue
            .write_buffer(&rows_buf, 0, bytemuck::cast_slice(&setup.rows));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-tex-tri-scanline-bg"),
            layout: &self.tex_tri_scanline_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.tex_tri_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.tpage_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: rows_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.tex_tri_scanline_consts.as_entire_binding(),
                },
            ],
        });

        let bbox_w = tri.bbox_max[0] - tri.bbox_min[0] + 1;
        let bbox_h = tri.bbox_max[1] - tri.bbox_min[1] + 1;
        if bbox_w <= 0 || bbox_h <= 0 {
            return false;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-tex-tri-scanline-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-tex-tri-scanline-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.tex_tri_scanline_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = (bbox_w as u32).div_ceil(WORKGROUP_SIZE_X);
            let groups_y = (bbox_h as u32).div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        true
    }

    /// Dispatch one quick-fill primitive into VRAM. Bypasses all
    /// drawing-state -- matches the CPU `Gpu::fill_rect`. Caller is
    /// responsible for the 16-pixel x/w masking; `Fill::new` does
    /// it for you.
    /// Dispatch one axis-aligned textured quad with bilinear UV
    /// interpolation. The host has already verified the geometry
    /// is axis-aligned via `TexQuadBilinear::is_axis_aligned`.
    /// Matches the CPU rasterizer's `rasterize_axis_aligned_textured_quad`
    /// fast path byte-for-byte.
    pub fn dispatch_tex_quad_bilinear(
        &self,
        vram: &VramGpu,
        quad: &TexQuadBilinear,
        tpage: &Tpage,
        area: &DrawArea,
    ) -> bool {
        if quad.exceeds_hw_extent() {
            return false;
        }
        let w = (quad.v1[0] - quad.v0[0]).abs();
        let h = (quad.v2[1] - quad.v0[1]).abs();
        if w <= 0 || h <= 0 {
            return false;
        }
        self.queue
            .write_buffer(&self.tex_quad_bilinear_uniform, 0, bytemuck::bytes_of(quad));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));
        self.queue
            .write_buffer(&self.tpage_uniform, 0, bytemuck::bytes_of(tpage));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-tex-quad-bilinear-bg"),
            layout: &self.tex_tri_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.tex_quad_bilinear_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.tpage_uniform.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-tex-quad-bilinear-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-tex-quad-bilinear-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.tex_quad_bilinear_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = (w as u32).div_ceil(WORKGROUP_SIZE_X);
            let groups_y = (h as u32).div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        true
    }

    pub fn dispatch_fill(&self, vram: &VramGpu, fill: &Fill) {
        if fill.wh[0] == 0 || fill.wh[1] == 0 {
            return;
        }
        self.queue
            .write_buffer(&self.fill_uniform, 0, bytemuck::bytes_of(fill));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-fill-bg"),
            layout: &self.fill_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.fill_uniform.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-fill-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-fill-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.fill_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = fill.wh[0].div_ceil(WORKGROUP_SIZE_X);
            let groups_y = fill.wh[1].div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// VRAM-to-VRAM copy (`GP0 0x80`). Mirrors the CPU rasterizer's
    /// row-by-row semantics exactly: for each row, the entire source
    /// row is read into a staging buffer first, then written to the
    /// dest row. This means vertically-overlapping copies "smear"
    /// the source down -- the same behaviour the CPU produces (Sony
    /// docs describe this as the row-buffer of the copy unit).
    ///
    /// Implementation: one per-row `src→temp` + `temp→dst` pair,
    /// all queued into a single command encoder so wgpu runs them
    /// strictly in order. Goes through a 1-row staging buffer
    /// because wgpu rejects `copy_buffer_to_buffer` with the same
    /// buffer as src and dst -- we'd need that for direct VRAM-to-
    /// VRAM otherwise.
    pub fn dispatch_vram_copy(
        &self,
        vram: &VramGpu,
        src: (u32, u32),
        dst: (u32, u32),
        wh: (u32, u32),
    ) {
        let (sx, sy) = src;
        let (dx, dy) = dst;
        let (w, h) = wh;
        if w == 0 || h == 0 {
            return;
        }

        let row_bytes = (w as u64) * 4;
        let temp = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-rasterizer-vram-copy-temp"),
            size: row_bytes,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-vram-copy-encoder"),
            });
        for row in 0..h {
            let s_off = ((sy + row) as u64 * super::vram::VRAM_WIDTH as u64 + sx as u64) * 4;
            let d_off = ((dy + row) as u64 * super::vram::VRAM_WIDTH as u64 + dx as u64) * 4;
            // Step 1: src row → temp.
            encoder.copy_buffer_to_buffer(vram.buffer(), s_off, &temp, 0, row_bytes);
            // Step 2: temp → dst row. Same encoder ⇒ runs strictly
            // after step 1, which gives the CPU's row-buffer semantics.
            encoder.copy_buffer_to_buffer(&temp, 0, vram.buffer(), d_off, row_bytes);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Dispatch one monochrome rectangle. `xy` is the top-left
    /// (already includes drawing-offset). Width/height of zero are
    /// dropped silently to match the CPU rasterizer.
    pub fn dispatch_mono_rect(&self, vram: &VramGpu, rect: &MonoRect, area: &DrawArea) {
        if rect.wh[0] == 0 || rect.wh[1] == 0 {
            return;
        }
        self.queue
            .write_buffer(&self.mono_rect_uniform, 0, bytemuck::bytes_of(rect));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-mono-rect-bg"),
            layout: &self.mono_tri_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.mono_rect_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-mono-rect-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-mono-rect-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.mono_rect_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = rect.wh[0].div_ceil(WORKGROUP_SIZE_X);
            let groups_y = rect.wh[1].div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

    /// Dispatch one textured rectangle. Linear UV stepping (no
    /// interpolation) → bit-exact texel parity vs the CPU.
    pub fn dispatch_tex_rect(
        &self,
        vram: &VramGpu,
        rect: &TexRect,
        tpage: &Tpage,
        area: &DrawArea,
    ) {
        if rect.wh[0] == 0 || rect.wh[1] == 0 {
            return;
        }
        self.queue
            .write_buffer(&self.tex_rect_uniform, 0, bytemuck::bytes_of(rect));
        self.queue
            .write_buffer(&self.draw_area_uniform, 0, bytemuck::bytes_of(area));
        self.queue
            .write_buffer(&self.tpage_uniform, 0, bytemuck::bytes_of(tpage));

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-rasterizer-tex-rect-bg"),
            layout: &self.tex_tri_bg_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: vram.buffer().as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.tex_rect_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.draw_area_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.tpage_uniform.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-rasterizer-tex-rect-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psx-rasterizer-tex-rect-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.tex_rect_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let groups_x = rect.wh[0].div_ceil(WORKGROUP_SIZE_X);
            let groups_y = rect.wh[1].div_ceil(WORKGROUP_SIZE_Y);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        self.queue.submit(Some(encoder.finish()));
    }

}

// `DrawArea` is exactly 16 bytes -- std::mem::size_of_val would also
// work, but bytemuck::bytes_of needs `Pod`. Both `MonoTri` and
// `DrawArea` derive Pod in `primitive.rs`. We assert the layout
// invariants at compile time below.
const _: () = {
    assert!(std::mem::size_of::<MonoTri>() == 48);
    assert!(std::mem::size_of::<DrawArea>() == 16);
};

// Just to silence dead-code warnings on `Zeroable` (used implicitly
// via `derive(Pod)`); explicit re-export here so future modules can
// import the trait via the rasterizer module.
#[allow(dead_code)]
fn _phantom_zeroable<T: Zeroable + Pod>() -> T {
    T::zeroed()
}

// =============================================================
//  Tests -- GPU rasterizer vs CPU rasterize_triangle parity
// =============================================================

#[cfg(test)]
#[cfg(test)]
mod tests;
