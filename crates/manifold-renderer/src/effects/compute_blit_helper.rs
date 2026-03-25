// Compute-based counterpart to SimpleBlitHelper.
//
// Bypasses Metal TBDR tile overhead by using compute dispatches instead of
// render passes for fullscreen 2D effects. Same ring-buffered uniform pattern,
// same API shape — effects can swap between render and compute with minimal
// code changes.
//
// Key difference: the target is bound as a storage texture (write-only) instead
// of a color attachment. This eliminates tile alloc/load/store overhead (~290us
// per pass at 4K as measured via Metal Instruments).
//
// Compute shaders must use:
//   textureSampleLevel(source, sampler, uv, 0.0)  — NOT textureSample
//   textureStore(target, coord, color)             — NOT fragment output

use std::cell::Cell;
use crate::gpu_encoder::GpuEncoder;

const RING_SLOTS: u64 = 64;
const UNIFORM_OFFSET_ALIGN: u64 = 256;

/// Cached bind group keyed by source/target texture view pointers.
/// Reused across frames when the same textures are bound (common case).
struct CachedBG {
    bind_group: wgpu::BindGroup,
    source_ptr: usize,
    target_ptr: usize,
}

/// The standard bind group layout entries for compute blit effects.
/// Shared between wgpu and hal pipeline creation.
const COMPUTE_BLIT_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 4] = [
    // binding 0: uniforms (dynamic offset for bind group caching)
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: None, // set per-helper at runtime for wgpu path
        },
        count: None,
    },
    // binding 1: source texture (filterable for textureSampleLevel)
    wgpu::BindGroupLayoutEntry {
        binding: 1,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    // binding 2: sampler (for textureSampleLevel)
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    // binding 3: output storage texture (write-only)
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

pub struct ComputeBlitHelper {
    pub pipeline: wgpu::ComputePipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    ring_buffer: wgpu::Buffer,
    uniform_size: u64,
    slot_stride: u64,
    ring_index: Cell<u64>,
    cached: Option<CachedBG>,

    // --- hal encoding fields (macOS + hal-encoding feature) ---
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_pipeline: Option<crate::hal_pipeline::HalComputePipeline>,
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_sampler: Option<<wgpu::hal::api::Metal as wgpu::hal::Api>::Sampler>,
    /// Persistent mapped pointer to shared-memory ring buffer.
    /// Writes are direct memcpy — no API call, no staging.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    ring_mapped_ptr: Option<*mut u8>,
    /// Cached hal pointer to ring buffer — extracted once at construction.
    /// Avoids per-dispatch as_hal() snatch lock acquisition.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_ring_ptr: Option<*const <wgpu::hal::api::Metal as wgpu::hal::Api>::Buffer>,
}

// Safety: the mapped pointer points to a GPU buffer that is Send+Sync
// (Metal shared storage mode). The pointer is only written from the
// content thread which owns the ComputeBlitHelper.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for ComputeBlitHelper {}

impl ComputeBlitHelper {
    /// Create a new compute-based effect pipeline.
    ///
    /// `uniform_size` — byte size of the effect's uniform struct (must be Pod).
    /// The compute shader must define:
    ///   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
    ///   @group(0) @binding(1) var source_tex: texture_2d<f32>;
    ///   @group(0) @binding(2) var tex_sampler: sampler;
    ///   @group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
    pub fn new(
        device: &wgpu::Device,
        shader_source: &str,
        label: &str,
        uniform_size: u64,
        hal_ctx: Option<&crate::hal_context::HalContext>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(&format!("{label} Compute BGL")),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: std::num::NonZero::new(uniform_size),
                        },
                        count: None,
                    },
                    COMPUTE_BLIT_BGL_ENTRIES[1],
                    COMPUTE_BLIT_BGL_ENTRIES[2],
                    COMPUTE_BLIT_BGL_ENTRIES[3],
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("{label} Compute Layout")),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&format!("{label} Compute Pipeline")),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{label} Sampler")),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let slot_stride =
            (uniform_size + UNIFORM_OFFSET_ALIGN - 1) & !(UNIFORM_OFFSET_ALIGN - 1);

        // --- hal pipeline + shared-memory ring buffer (when feature enabled) ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler, ring_buffer, ring_mapped_ptr, hal_ring_ptr) =
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            // Create hal compute pipeline from same WGSL source
            let hal_pipe = crate::hal_pipeline::create_compute_pipeline(
                ctx,
                shader_source,
                "cs_main",
                &COMPUTE_BLIT_BGL_ENTRIES,
                label,
            );

            // Create hal sampler
            let hal_samp = unsafe {
                ctx.device()
                    .create_sampler(&wgpu::hal::SamplerDescriptor {
                        label: Some(label),
                        address_modes: [wgpu::AddressMode::ClampToEdge; 3],
                        mag_filter: wgpu::FilterMode::Linear,
                        min_filter: wgpu::FilterMode::Linear,
                        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                        lod_clamp: 0.0..32.0,
                        compare: None,
                        anisotropy_clamp: 1,
                        border_color: None,
                    })
                    .expect("Failed to create hal sampler")
            };

            // Create shared-memory ring buffer via hal, import into wgpu
            let buf_size = slot_stride * RING_SLOTS;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some(label),
                        size: buf_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal ring buffer")
            };

            // Map to get persistent pointer (Metal shared storage = always mapped)
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..buf_size)
                    .expect("Failed to map hal ring buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();

            // Import hal buffer into wgpu for the fallback/wgpu bind group path
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some(&format!("{label} Compute Ring UBO")),
                        size: buf_size,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };

            // Cache the hal pointer to the ring buffer — avoids per-dispatch
            // as_hal() snatch lock. Safe: wgpu_buf owns the underlying Metal buffer
            // which lives as long as this ComputeBlitHelper.
            let ring_hal_ptr = {
                let guard = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("ring buffer not Metal");
                let ptr: *const _ = &*guard;
                // Guard dropped here — snatch lock released.
                ptr
            };

            (Some(hal_pipe), Some(hal_samp), wgpu_buf, Some(mapped_ptr), Some(ring_hal_ptr))
        } else {
            let wgpu_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Compute Ring UBO")),
                size: slot_stride * RING_SLOTS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (None, None, wgpu_buf, None, None)
        };

        // wgpu-only ring buffer (when feature not enabled)
        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let ring_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Compute Ring UBO")),
            size: slot_stride * RING_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            ring_buffer,
            uniform_size,
            slot_stride,
            ring_index: Cell::new(0),
            cached: None,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            ring_mapped_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ring_ptr,
        }
    }

    /// Ensure the cached bind group is valid for the given source/target views.
    /// Uses dynamic uniform offset so the bind group can be reused across
    /// frames when the same textures are bound (saves ~10μs per call).
    fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        label: &str,
    ) {
        let src_ptr = std::ptr::from_ref(source_view) as usize;
        let tgt_ptr = std::ptr::from_ref(target_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => c.source_ptr != src_ptr || c.target_ptr != tgt_ptr,
            None => true,
        };

        if needs_recreate {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.ring_buffer,
                            offset: 0,
                            size: std::num::NonZero::new(self.uniform_size),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(target_view),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                source_ptr: src_ptr,
                target_ptr: tgt_ptr,
            });
        }
    }

    /// Execute a compute dispatch: reads source texture, writes to target storage texture.
    /// Dispatches ceil(width/16) x ceil(height/16) workgroups of 16x16 threads.
    ///
    /// When `hal-encoding` feature is enabled and a hal pipeline was created,
    /// this dispatches through the hal encoder (zero CPU overhead). Otherwise
    /// falls back to the wgpu path.
    pub fn dispatch(
        &mut self,
        gpu: &mut GpuEncoder,
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = slot * self.slot_stride;

        // --- hal dispatch path ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let (Some(hal_pipe), Some(mapped_ptr), true) = (
            &self.hal_pipeline,
            self.ring_mapped_ptr,
            gpu.has_hal(),
        ) {
            use wgpu::hal::{self as hal, Device as HalDevice};
            type MetalApi = hal::api::Metal;

            // Direct memcpy to shared-memory buffer (no API call)
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uniform_bytes.as_ptr(),
                    mapped_ptr.add(byte_offset as usize),
                    uniform_bytes.len(),
                );
            }

            // Extract hal texture view pointers — MUST drop each guard before
            // acquiring the next to avoid wgpu's non-reentrant snatch lock panic.
            let hal_source_ptr = {
                let guard = unsafe { source_view.as_hal::<MetalApi>() }
                    .expect("source view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            }; // guard dropped — snatch lock released

            let hal_target_ptr = {
                let guard = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            }; // guard dropped

            // Ring buffer + sampler use cached pointers (no snatch lock needed)
            let hal_ring_ref = unsafe { &*self.hal_ring_ptr.unwrap() };
            let hal_sampler = self.hal_sampler.as_ref().unwrap();

            // Safety: texture view pointers are valid because the wgpu TextureViews
            // (owned by compositor ping-pong buffers) are alive for this entire frame.
            let hal_source_ref = unsafe { &*hal_source_ptr };
            let hal_target_ref = unsafe { &*hal_target_ptr };

            // Create lightweight hal bind group (~0.1μs — copies pointers)
            let hal_bg = unsafe {
                gpu.hal_ctx.unwrap().device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry { binding: 0, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 1, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 2, resource_index: 0, count: 1 },
                            hal::BindGroupEntry { binding: 3, resource_index: 1, count: 1 },
                        ],
                        buffers: &[
                            hal::BufferBinding::new_unchecked(
                                hal_ring_ref,
                                0,
                                std::num::NonZero::new(self.uniform_size),
                            ),
                        ],
                        samplers: &[hal_sampler],
                        textures: &[
                            hal::TextureBinding {
                                view: hal_source_ref,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: hal_target_ref,
                                usage: wgpu::wgt::TextureUses::STORAGE_READ_WRITE,
                            },
                        ],
                        acceleration_structures: &[],
                        external_textures: &[],
                    },
                )
                .expect("Failed to create hal bind group")
            };

            // Encode via hal — zero validation overhead
            unsafe {
                gpu.hal_begin_compute_pass(label);
                gpu.hal_set_compute_pipeline(&hal_pipe.pipeline);
                gpu.hal_set_bind_group(
                    0,
                    &hal_pipe.pipeline_layout,
                    &hal_bg,
                    &[byte_offset as u32],
                );
                gpu.hal_dispatch(width.div_ceil(16), height.div_ceil(16), 1);
                gpu.hal_end_compute_pass();
            }

            // Clean up the ephemeral bind group
            unsafe {
                gpu.hal_ctx
                    .unwrap()
                    .device()
                    .destroy_bind_group(hal_bg);
            }

            return;
        }

        // --- wgpu dispatch path (default / fallback) ---
        gpu.queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Update cached bind group if textures changed (mutation done before
        // the compute pass borrow to satisfy the borrow checker).
        self.ensure_bind_group(gpu.device, source_view, target_view, label);

        let ts = profiler.and_then(|p| p.compute_timestamps(label, width, height));
        let mut pass = gpu.encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: ts,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(
            0,
            &self.cached.as_ref().unwrap().bind_group,
            &[byte_offset as u32],
        );
        pass.dispatch_workgroups(
            width.div_ceil(16),
            height.div_ceil(16),
            1,
        );
    }
}
