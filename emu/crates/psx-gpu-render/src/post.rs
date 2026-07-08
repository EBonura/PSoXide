//! Screen-space post-process upscaler (xBR).
//!
//! Runs after the HW target is drawn: samples the target's display sub-rect
//! and writes an XBR_SCALE-upscaled, edge-reconstructed image into its own
//! texture, which the frontend paints instead of the raw target. Operating on
//! the composited image (not per-texture) means it has none of the VRAM-packing
//! seam artefacts a texture-space filter hits on PSX content.

use crate::target::TARGET_FORMAT;

/// Fixed xBR upscale factor (must match `XBR_SCALE` in `shaders/xbr.wgsl`).
pub const XBR_SCALE: u32 = 3;

pub struct PostFx {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    /// Output (upscaled) texture + its egui registration. Resized when the
    /// display sub-rect changes.
    out_texture: wgpu::Texture,
    out_view: wgpu::TextureView,
    out_size: (u32, u32),
    egui_id: Option<egui::TextureId>,
}

impl PostFx {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        egui_renderer: Option<&mut egui_wgpu::Renderer>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("psx-xbr"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/xbr.wgsl").into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("psx-xbr-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("psx-xbr-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("psx-xbr-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-xbr-uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let out_size = (XBR_SCALE * 320, XBR_SCALE * 240);
        let (out_texture, out_view) = create_out(&device, out_size);
        let egui_id = egui_renderer
            .map(|r| r.register_native_texture(&device, &out_view, wgpu::FilterMode::Nearest));
        Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            uniform,
            out_texture,
            out_view,
            out_size,
            egui_id,
        }
    }

    pub fn texture_id(&self) -> egui::TextureId {
        self.egui_id.unwrap_or_default()
    }

    pub fn out_texture(&self) -> &wgpu::Texture {
        &self.out_texture
    }

    pub fn out_size(&self) -> (u32, u32) {
        self.out_size
    }

    /// Read the output texture back to RGBA8 (headless dump path).
    pub fn read_rgba8(&self) -> (u32, u32, Vec<u8>) {
        let (w, h) = self.out_size;
        if w == 0 || h == 0 {
            return (0, 0, Vec::new());
        }
        let unpadded_bpr = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bpr = unpadded_bpr.div_ceil(align) * align;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psx-xbr-readback"),
            size: (padded_bpr * h) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.out_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.expect("map xbr readback"));
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded_bpr * h) as usize);
        for row in 0..h {
            let start = (row * padded_bpr) as usize;
            out.extend_from_slice(&data[start..start + unpadded_bpr as usize]);
        }
        drop(data);
        buffer.unmap();
        (w, h, out)
    }

    /// Resize the output texture to `display * XBR_SCALE` when the display
    /// sub-rect changes, re-registering the egui paint texture. Called from the
    /// frontend (which holds the egui renderer), like the HW scale update.
    pub fn ensure_size(
        &mut self,
        display: (u32, u32),
        egui_renderer: Option<&mut egui_wgpu::Renderer>,
    ) {
        let want = (
            display.0.max(1) * XBR_SCALE,
            display.1.max(1) * XBR_SCALE,
        );
        if want == self.out_size {
            return;
        }
        let (tex, view) = create_out(&self.device, want);
        self.out_texture = tex;
        self.out_view = view;
        self.out_size = want;
        if let (Some(id), Some(r)) = (self.egui_id, egui_renderer) {
            r.update_egui_texture_from_wgpu_texture(
                &self.device,
                &self.out_view,
                wgpu::FilterMode::Nearest,
                id,
            );
        }
    }

    /// Upscale the `(x, y, w, h)` display sub-rect (in `src` texels) of the
    /// rendered target into the output texture. Assumes [`PostFx::ensure_size`]
    /// already sized the output for `(w, h)`.
    pub fn run(&mut self, src_view: &wgpu::TextureView, rect: (u32, u32, u32, u32)) {
        let (x, y, w, h) = rect;
        let w = w.max(1);
        let h = h.max(1);
        // uniform: src_origin, src_size, out_size, pad
        let u: [f32; 8] = [
            x as f32,
            y as f32,
            w as f32,
            h as f32,
            self.out_size.0 as f32,
            self.out_size.1 as f32,
            0.0,
            0.0,
        ];
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&u));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psx-xbr-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("psx-xbr-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("psx-xbr-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.out_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
    }
}

fn create_out(device: &wgpu::Device, size: (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("psx-xbr-out"),
        size: wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
