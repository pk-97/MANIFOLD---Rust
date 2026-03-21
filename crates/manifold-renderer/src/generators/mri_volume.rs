use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use super::mri_volume_loader::{MriVolumeData, MriVolumeGpu};
use std::path::PathBuf;

// Parameter indices matching generator_definition_registry.rs
const MODE: usize = 0;
const SLICE_AXIS: usize = 1;
const SLICE_POS: usize = 2;
const WINDOW_CENTER: usize = 3;
const WINDOW_WIDTH: usize = 4;
const SCALE: usize = 5;
const INVERT: usize = 6;
const SHARPEN: usize = 7;
const CINE_SPEED: usize = 8;
const SCAN: usize = 9;
const CAM_DIST: usize = 10;
const ROT_X: usize = 11;
const ROT_Y: usize = 12;
const ROT_Z: usize = 13;
const OPACITY: usize = 14;
const STEPS: usize = 15;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SliceUniforms {
    slice_axis: f32,
    slice_pos: f32,
    window_center: f32,
    window_width: f32,
    aspect_ratio: f32,
    uv_scale: f32,
    invert: f32,
    spacing_x: f32,
    spacing_y: f32,
    spacing_z: f32,
    dim_x: f32,
    dim_y: f32,
    dim_z: f32,
    sharpen: f32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RaymarchUniforms {
    cam_dist: f32,
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    aspect_ratio: f32,
    uv_scale: f32,
    window_center: f32,
    window_width: f32,
    opacity_scale: f32,
    step_count: f32,
    invert: f32,
    _pad: f32,
}

pub struct MriVolumeGenerator {
    // Pipelines
    slice_pipeline: wgpu::RenderPipeline,
    slice_bgl: wgpu::BindGroupLayout,
    slice_uniform_buf: wgpu::Buffer,
    raymarch_pipeline: wgpu::RenderPipeline,
    raymarch_bgl: wgpu::BindGroupLayout,
    raymarch_uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    // Volume state
    volume_gpu: Option<MriVolumeGpu>,
    volume_cpu: Option<MriVolumeData>,
    _load_requested: bool,
    current_gpu_frame: u32,
    // Scan library
    scan_paths: Vec<PathBuf>,
    current_scan_index: i32,
}

fn create_bgl_3tex(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
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
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn create_frag_pipeline(
    device: &wgpu::Device,
    label: &str,
    shader_src: &str,
    bgl: &wgpu::BindGroupLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[bgl],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
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
    })
}

impl MriVolumeGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // Slice pipeline
        let slice_bgl = create_bgl_3tex(device, "MRI Slice BGL");
        let slice_pipeline = create_frag_pipeline(
            device, "MRI Slice Pipeline",
            include_str!("shaders/mri_slice.wgsl"),
            &slice_bgl, target_format,
        );
        let slice_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("MRI Slice Uniforms"),
            size: std::mem::size_of::<SliceUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Raymarch pipeline
        let raymarch_bgl = create_bgl_3tex(device, "MRI Raymarch BGL");
        let raymarch_pipeline = create_frag_pipeline(
            device, "MRI Raymarch Pipeline",
            include_str!("shaders/mri_raymarch.wgsl"),
            &raymarch_bgl, target_format,
        );
        let raymarch_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("MRI Raymarch Uniforms"),
            size: std::mem::size_of::<RaymarchUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("MRI Volume Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let scan_paths = Self::discover_volumes();
        let scan_count = scan_paths.len();
        if scan_count > 0 {
            log::info!("MRI Volume: found {} scans in volumes/", scan_count);
            for (i, p) in scan_paths.iter().enumerate() {
                log::info!("  Scan {}: {}", i, p.file_stem().unwrap_or_default().to_string_lossy());
            }
        } else {
            log::warn!("MRI Volume: no .mrivol files found in assets/mri-data/volumes/");
        }

        Self {
            slice_pipeline,
            slice_bgl,
            slice_uniform_buf,
            raymarch_pipeline,
            raymarch_bgl,
            raymarch_uniform_buf,
            sampler,
            volume_gpu: None,
            volume_cpu: None,
            _load_requested: false,
            current_gpu_frame: 0,
            scan_paths,
            current_scan_index: -1,
        }
    }

    fn discover_volumes() -> Vec<PathBuf> {
        let volumes_dir = PathBuf::from("assets/mri-data/volumes");
        if !volumes_dir.is_dir() {
            return Vec::new();
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&volumes_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "mrivol"))
            .collect();
        paths.sort();
        paths
    }

    fn load_scan(&mut self, index: usize, device: &wgpu::Device, queue: &wgpu::Queue) {
        if index >= self.scan_paths.len() {
            return;
        }
        let path = &self.scan_paths[index];
        match MriVolumeData::load(path) {
            Ok(vol_data) => {
                let auto_window = vol_data.compute_auto_window();
                let gpu = MriVolumeGpu::from_volume_data(device, queue, &vol_data, 0, auto_window);
                log::info!(
                    "MRI scan {}/{}: {}x{}x{}, {} frames ({}) auto-window=[{:.3}, {:.3}]",
                    index + 1, self.scan_paths.len(),
                    gpu.dim[0], gpu.dim[1], gpu.dim[2], vol_data.header.frames,
                    path.file_stem().unwrap_or_default().to_string_lossy(),
                    auto_window[0], auto_window[1],
                );
                self.volume_gpu = Some(gpu);
                self.volume_cpu = Some(vol_data);
                self.current_gpu_frame = 0;
                self.current_scan_index = index as i32;
            }
            Err(e) => {
                log::error!("Failed to load MRI scan: {}", e);
            }
        }
    }

    fn upload_frame(&mut self, queue: &wgpu::Queue, frame_index: u32) {
        let Some(vol_cpu) = &self.volume_cpu else { return };
        let Some(vol_gpu) = &self.volume_gpu else { return };

        if frame_index == self.current_gpu_frame {
            return;
        }

        let h = &vol_cpu.header;
        let frame_voxels = h.frame_voxels();
        let frame_offset = frame_index as usize * frame_voxels;
        let frame_data = &vol_cpu.voxels_f32[frame_offset..frame_offset + frame_voxels];

        let dx = h.dim_x as usize;
        let dy = h.dim_y as usize;
        let dz = h.dim_z as usize;
        let texel_bytes: u32 = 8;
        let unpadded_bytes_per_row = h.dim_x * texel_bytes;
        let padded_bytes_per_row = (unpadded_bytes_per_row + 255) & !255;
        let pad_bytes = (padded_bytes_per_row - unpadded_bytes_per_row) as usize;
        let total_size = padded_bytes_per_row as usize * dy * dz;
        let mut rgba16_data: Vec<u8> = Vec::with_capacity(total_size);

        for z in 0..dz {
            for y in 0..dy {
                for x in 0..dx {
                    let src_idx = x * (dy * dz) + y * dz + z;
                    let val = frame_data[src_idx];
                    let h16 = half::f16::from_f32(val);
                    let bytes = h16.to_le_bytes();
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                    rgba16_data.extend_from_slice(&bytes);
                }
                for _ in 0..pad_bytes {
                    rgba16_data.push(0);
                }
            }
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &vol_gpu.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba16_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(h.dim_y),
            },
            wgpu::Extent3d {
                width: h.dim_x,
                height: h.dim_y,
                depth_or_array_layers: h.dim_z,
            },
        );

        self.current_gpu_frame = frame_index;
    }

    fn render_black(encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::MriVolume
    }

    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
    ) -> f32 {
        // Scan selection
        let scan_index = if ctx.param_count > SCAN as u32 {
            (ctx.params[SCAN].round() as i32).clamp(0, self.scan_paths.len().saturating_sub(1) as i32)
        } else {
            0
        };
        if scan_index != self.current_scan_index && !self.scan_paths.is_empty() {
            self.load_scan(scan_index as usize, device, queue);
        }

        if self.volume_gpu.is_none() {
            Self::render_black(encoder, target);
            return ctx.anim_progress;
        }

        // Cine frame selection
        let total_frames = self.volume_cpu.as_ref().map_or(1, |v| v.header.frames);
        if total_frames > 1 {
            let cine_speed = param(ctx, CINE_SPEED, 1.0);
            let frame_f = ctx.time * cine_speed * total_frames as f32;
            let frame_index = ((frame_f.floor() as i32).rem_euclid(total_frames as i32)) as u32;
            self.upload_frame(queue, frame_index);
        }

        let vol = self.volume_gpu.as_ref().unwrap();

        // Auto-windowed params
        let auto_low = vol.auto_window[0];
        let auto_high = vol.auto_window[1];
        let auto_span = (auto_high - auto_low).max(0.001);
        let raw_center = param(ctx, WINDOW_CENTER, 0.5);
        let raw_width = param(ctx, WINDOW_WIDTH, 0.8);
        let window_center = auto_low + auto_span * raw_center;
        let window_width = auto_span * raw_width;
        let invert = if param(ctx, INVERT, 0.0) > 0.5 { 1.0 } else { 0.0 };
        let scale = param(ctx, SCALE, 1.0);
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        let render_mode = param(ctx, MODE, 0.0).round() as u32;

        if render_mode == 0 {
            // ── Slice mode ──
            let uniforms = SliceUniforms {
                slice_axis: param(ctx, SLICE_AXIS, 0.0).round(),
                slice_pos: param(ctx, SLICE_POS, 0.5),
                window_center,
                window_width,
                aspect_ratio: ctx.aspect,
                uv_scale,
                invert,
                spacing_x: vol.spacing[0],
                spacing_y: vol.spacing[1],
                spacing_z: vol.spacing[2],
                dim_x: vol.dim[0] as f32,
                dim_y: vol.dim[1] as f32,
                dim_z: vol.dim[2] as f32,
                sharpen: param(ctx, SHARPEN, 1.0),
                _pad0: 0.0,
                _pad1: 0.0,
            };
            queue.write_buffer(&self.slice_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("MRI Slice BG"),
                layout: &self.slice_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.slice_uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vol.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("MRI Slice Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&self.slice_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        } else {
            // ── Raymarch volume mode ──
            let uniforms = RaymarchUniforms {
                cam_dist: param(ctx, CAM_DIST, 3.0),
                rot_x: param(ctx, ROT_X, 0.0),
                rot_y: param(ctx, ROT_Y, 0.0),
                rot_z: param(ctx, ROT_Z, 0.0),
                aspect_ratio: ctx.aspect,
                uv_scale,
                window_center,
                window_width,
                opacity_scale: param(ctx, OPACITY, 2.0),
                step_count: param(ctx, STEPS, 128.0).round(),
                invert,
                _pad: 0.0,
            };
            queue.write_buffer(&self.raymarch_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("MRI Raymarch BG"),
                layout: &self.raymarch_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.raymarch_uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vol.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("MRI Raymarch Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&self.raymarch_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {}
}
