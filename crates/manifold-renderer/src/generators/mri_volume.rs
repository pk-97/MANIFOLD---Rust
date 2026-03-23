use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::mri_volume_loader::{ScanInfo, discover_scans, load_tiff_slice};
use std::path::PathBuf;

// Parameter indices matching generator_definition_registry.rs
const SLICE_AXIS: usize = 0;
const SLICE_POS: usize = 1;
const WINDOW_CENTER: usize = 2;
const WINDOW_WIDTH: usize = 3;
const SCALE: usize = 4;
const INVERT: usize = 5;
const SHARPEN: usize = 6;
const SCAN: usize = 7;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SliceUniforms {
    aspect_ratio: f32,
    uv_scale: f32,
    invert: f32,
    sharpen: f32,
    window_center: f32,
    window_width: f32,
    tex_width: f32,
    tex_height: f32,
}

pub struct MriVolumeGenerator {
    // Pipeline
    slice_pipeline: wgpu::RenderPipeline,
    slice_bgl: wgpu::BindGroupLayout,
    slice_uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    // Current slice texture (R8Unorm 2D)
    slice_texture: Option<wgpu::Texture>,
    slice_view: Option<wgpu::TextureView>,
    current_tex_dims: (u32, u32),
    // State tracking
    current_scan_index: i32,
    current_axis: i32,
    current_slice_index: i32,
    // Scan library
    scans: Vec<ScanInfo>,
}

impl MriVolumeGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let slice_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("MRI Slice BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: true,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(
                            wgpu::SamplerBindingType::Filtering,
                        ),
                        count: None,
                    },
                ],
            });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("MRI Slice Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("shaders/mri_slice.wgsl").into(),
            ),
        });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("MRI Slice Pipeline Layout"),
                bind_group_layouts: &[&slice_bgl],
                immediate_size: 0,
            });
        let slice_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("MRI Slice Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        let slice_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("MRI Slice Uniforms"),
            size: std::mem::size_of::<SliceUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("MRI Slice Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let scans = discover_scans(&PathBuf::from("assets/mri-data/volumes"));
        if scans.is_empty() {
            log::warn!("MRI Volume: no scan directories found");
        } else {
            log::info!("MRI Volume: found {} scan(s)", scans.len());
            for (i, s) in scans.iter().enumerate() {
                let axes: Vec<&str> = [
                    s.axes[0].as_ref().map(|a| {
                        log::info!(
                            "  Scan {} ({}): axial={} slices",
                            i, s.name, a.slice_count
                        );
                        "axial"
                    }),
                    s.axes[1].as_ref().map(|a| {
                        log::info!(
                            "  Scan {} ({}): sagittal={} slices",
                            i, s.name, a.slice_count
                        );
                        "sagittal"
                    }),
                    s.axes[2].as_ref().map(|a| {
                        log::info!(
                            "  Scan {} ({}): coronal={} slices",
                            i, s.name, a.slice_count
                        );
                        "coronal"
                    }),
                ]
                .into_iter()
                .flatten()
                .collect();
                log::info!(
                    "  Scan {}: {} [{}]",
                    i,
                    s.name,
                    axes.join(", ")
                );
            }
        }

        Self {
            slice_pipeline,
            slice_bgl,
            slice_uniform_buf,
            sampler,
            slice_texture: None,
            slice_view: None,
            current_tex_dims: (0, 0),
            current_scan_index: -1,
            current_axis: -1,
            current_slice_index: -1,
            scans,
        }
    }

    /// Ensure the 2D texture exists and matches the given dimensions.
    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) {
        if self.current_tex_dims == (width, height) && self.slice_texture.is_some()
        {
            return;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MRI Slice 2D"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.slice_texture = Some(texture);
        self.slice_view = Some(view);
        self.current_tex_dims = (width, height);
    }

    /// Upload R8Unorm data to the current texture.
    fn upload_slice(
        &self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        data: &[u8],
    ) {
        let Some(texture) = &self.slice_texture else {
            return;
        };

        // R8Unorm: 1 byte per texel. Pad rows to 256-byte alignment.
        let unpadded_bpr = width;
        let padded_bpr = (unpadded_bpr + 255) & !255;

        if padded_bpr == unpadded_bpr {
            // No padding needed — upload directly
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            // Pad each row
            let mut padded =
                Vec::with_capacity(padded_bpr as usize * height as usize);
            for y in 0..height as usize {
                let row_start = y * width as usize;
                let row_end = row_start + width as usize;
                padded.extend_from_slice(&data[row_start..row_end]);
                padded.extend(std::iter::repeat_n(
                    0u8,
                    (padded_bpr - unpadded_bpr) as usize,
                ));
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &padded,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn render_black(
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
    ) {
        let _pass =
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("MRI Clear Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
    }
}

impl Generator for MriVolumeGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::MRI_VOLUME
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32 {
        if self.scans.is_empty() {
            Self::render_black(encoder, target);
            return ctx.anim_progress;
        }

        // Scan selection
        let scan_index = (param(ctx, SCAN, 0.0).round() as i32)
            .clamp(0, self.scans.len() as i32 - 1);
        let axis = (param(ctx, SLICE_AXIS, 0.0).round() as i32).clamp(0, 2);

        let scan = &self.scans[scan_index as usize];
        let Some(axis_slices) = &scan.axes[axis as usize] else {
            Self::render_black(encoder, target);
            return ctx.anim_progress;
        };

        let slice_pos = param(ctx, SLICE_POS, 0.5);
        let max_idx = axis_slices.slice_count as i32 - 1;
        let slice_index =
            (slice_pos * max_idx as f32).round() as i32;
        let slice_index = slice_index.clamp(0, max_idx);

        // Check if we need to load a new slice
        let need_load = slice_index != self.current_slice_index
            || scan_index != self.current_scan_index
            || axis != self.current_axis;

        if need_load {
            let path = &axis_slices.paths[slice_index as usize];
            match load_tiff_slice(path) {
                Ok((w, h, data)) => {
                    self.ensure_texture(device, w, h);
                    self.upload_slice(queue, w, h, &data);
                    self.current_scan_index = scan_index;
                    self.current_axis = axis;
                    self.current_slice_index = slice_index;
                }
                Err(e) => {
                    log::error!("MRI: {e}");
                    Self::render_black(encoder, target);
                    return ctx.anim_progress;
                }
            }
        }

        let Some(view) = &self.slice_view else {
            Self::render_black(encoder, target);
            return ctx.anim_progress;
        };

        // Uniforms
        let scale = param(ctx, SCALE, 1.0);
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let invert =
            if param(ctx, INVERT, 0.0) > 0.5 { 1.0 } else { 0.0 };

        let uniforms = SliceUniforms {
            aspect_ratio: ctx.aspect,
            uv_scale,
            invert,
            sharpen: param(ctx, SHARPEN, 1.0),
            window_center: param(ctx, WINDOW_CENTER, 0.5),
            window_width: param(ctx, WINDOW_WIDTH, 0.8),
            tex_width: self.current_tex_dims.0 as f32,
            tex_height: self.current_tex_dims.1 as f32,
        };
        queue.write_buffer(
            &self.slice_uniform_buf,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let bind_group =
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("MRI Slice BG"),
                layout: &self.slice_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self
                            .slice_uniform_buf
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &self.sampler,
                        ),
                    },
                ],
            });

        let ts = profiler.and_then(|p| {
            p.render_timestamps("MRI Slice", ctx.width, ctx.height)
        });
        let mut pass =
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("MRI Slice Pass"),
                color_attachments: &[Some(
                    wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    },
                )],
                depth_stencil_attachment: None,
                timestamp_writes: ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        pass.set_pipeline(&self.slice_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);

        ctx.anim_progress
    }

    fn resize(
        &mut self,
        _device: &wgpu::Device,
        _width: u32,
        _height: u32,
    ) {
    }
}
