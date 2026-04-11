use super::mri_volume_loader::{ScanInfo, discover_scans, load_tiff_slice};
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;
use std::path::PathBuf;
use std::sync::mpsc;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::MRI_VOLUME,
        create: |device| Box::new(MriVolumeGenerator::new(device)),
    }
}

// Parameter indices matching generator_definition_registry.rs
const SLICE_AXIS: usize = 0;
const SLICE_POS: usize = 1;
const WINDOW_CENTER: usize = 2;
const WINDOW_WIDTH: usize = 3;
const SCALE: usize = 4;
const INVERT: usize = 5;
const SHARPEN: usize = 6;
const SCAN: usize = 7;
const SNAP: usize = 8;

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
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    // Current slice texture (R8Unorm 2D) — stored as shared-memory buffer for CPU upload
    slice_texture: Option<manifold_gpu::GpuTexture>,
    current_tex_dims: (u32, u32),
    // State tracking
    current_scan_index: i32,
    current_axis: i32,
    current_slice_index: i32,
    // Async slice loading — avoids blocking the 60 FPS content thread on file I/O
    #[allow(clippy::type_complexity)]
    pending_load: Option<mpsc::Receiver<Result<(u32, u32, Vec<u8>), String>>>,
    pending_scan_index: i32,
    pending_axis: i32,
    pending_slice_index: i32,
    // Scan library
    scans: Vec<ScanInfo>,
}

impl MriVolumeGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/mri_slice_compute.wgsl"),
            "cs_main",
            "MRI Slice",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        let scan_path = PathBuf::from("assets/mri-data/volumes");
        let scans = discover_scans(&scan_path);
        if scans.is_empty() {
        } else {
            log::info!("MRI Volume: found {} scan(s)", scans.len());
            for (i, s) in scans.iter().enumerate() {
                let axes: Vec<&str> = [
                    s.axes[0].as_ref().map(|a| {
                        log::info!("  Scan {} ({}): axial={} slices", i, s.name, a.slice_count);
                        "axial"
                    }),
                    s.axes[1].as_ref().map(|a| {
                        log::info!(
                            "  Scan {} ({}): sagittal={} slices",
                            i,
                            s.name,
                            a.slice_count
                        );
                        "sagittal"
                    }),
                    s.axes[2].as_ref().map(|a| {
                        log::info!(
                            "  Scan {} ({}): coronal={} slices",
                            i,
                            s.name,
                            a.slice_count
                        );
                        "coronal"
                    }),
                ]
                .into_iter()
                .flatten()
                .collect();
                log::info!("  Scan {}: {} [{}]", i, s.name, axes.join(", "));
            }
        }

        Self {
            pipeline,
            sampler,
            slice_texture: None,
            current_tex_dims: (0, 0),
            current_scan_index: -1,
            current_axis: -1,
            current_slice_index: -1,
            pending_load: None,
            pending_scan_index: -1,
            pending_axis: -1,
            pending_slice_index: -1,
            scans,
        }
    }

    /// Ensure the 2D texture exists and matches the given dimensions.
    fn ensure_texture(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.current_tex_dims == (width, height) && self.slice_texture.is_some() {
            return;
        }

        let texture = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::R8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "MRI Slice 2D",
            mip_levels: 1,
        });
        self.slice_texture = Some(texture);
        self.current_tex_dims = (width, height);
    }

    /// Upload R8Unorm data to the current texture via a blit encoder.
    fn upload_slice(&self, gpu: &mut GpuEncoder, width: u32, height: u32, data: &[u8]) {
        let Some(texture) = &self.slice_texture else {
            return;
        };
        gpu.native_enc
            .upload_texture(texture, width, height, 1, data);
    }
}

impl Generator for MriVolumeGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::MRI_VOLUME
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if self.scans.is_empty() {
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
            return ctx.anim_progress;
        }

        // Check if async load completed — upload data if ready
        // Must happen before borrowing self.scans to avoid borrow conflicts
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((w, h, data))) => {
                    self.ensure_texture(gpu.device, w, h);
                    self.upload_slice(gpu, w, h, &data);
                    self.current_scan_index = self.pending_scan_index;
                    self.current_axis = self.pending_axis;
                    self.current_slice_index = self.pending_slice_index;
                    // pending_load already None from take()
                }
                Ok(Err(e)) => {
                    log::error!("MRI async load: {e}");
                    // pending_load already None from take()
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still loading — put receiver back, continue displaying previous slice
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("MRI async load: sender thread disconnected");
                    // pending_load already None from take()
                }
            }
        }

        // Scan selection
        let scan_index =
            (param(ctx, SCAN, 0.0).round() as i32).clamp(0, self.scans.len() as i32 - 1);
        let snap = param(ctx, SNAP, 0.0) > 0.5;
        let axis = if snap {
            (ctx.trigger_count % 3) as i32
        } else {
            (param(ctx, SLICE_AXIS, 0.0).round() as i32).clamp(0, 2)
        };

        let scan = &self.scans[scan_index as usize];
        let Some(axis_slices) = &scan.axes[axis as usize] else {
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
            return ctx.anim_progress;
        };

        let slice_pos = param(ctx, SLICE_POS, 0.5);
        let max_idx = axis_slices.slice_count as i32 - 1;
        let slice_index = (slice_pos * max_idx as f32).round() as i32;
        let slice_index = slice_index.clamp(0, max_idx);

        // Check if we need to load a new slice — only spawn if no load in flight
        let need_load = slice_index != self.current_slice_index
            || scan_index != self.current_scan_index
            || axis != self.current_axis;

        if need_load && self.pending_load.is_none() {
            let (tx, rx) = mpsc::channel();
            let path = axis_slices.paths[slice_index as usize].clone();
            self.pending_scan_index = scan_index;
            self.pending_axis = axis;
            self.pending_slice_index = slice_index;
            std::thread::spawn(move || {
                let _ = tx.send(load_tiff_slice(&path));
            });
            self.pending_load = Some(rx);
        }

        let Some(slice_tex) = &self.slice_texture else {
            gpu.native_enc.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
            return ctx.anim_progress;
        };

        // Uniforms
        let scale = param(ctx, SCALE, 1.0);
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let invert = if param(ctx, INVERT, 0.0) > 0.5 {
            1.0
        } else {
            0.0
        };

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

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: slice_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "MRI Slice Compute",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}
}
