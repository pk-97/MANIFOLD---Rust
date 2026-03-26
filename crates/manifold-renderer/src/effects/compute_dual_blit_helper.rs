// Compute-based counterpart to DualTextureBlitHelper.
//
// Bypasses Metal TBDR tile overhead by using compute dispatches instead of
// render passes for fullscreen 2D effects that read two input textures.
// Same ring-buffered uniform pattern, same API shape — effects can swap
// between render and compute with minimal code changes.
//
// Key difference from ComputeBlitHelper: 5 bindings (two source textures).
// Key difference from DualTextureBlitHelper: target is a storage texture
// (write-only) instead of a color attachment, eliminating tile overhead.
//
// Compute shaders must use:
//   textureSampleLevel(source_a, sampler, uv, 0.0)  — NOT textureSample
//   textureSampleLevel(source_b, sampler, uv, 0.0)  — NOT textureSample
//   textureStore(target, coord, color)               — NOT fragment output

use std::cell::Cell;
use crate::gpu_encoder::GpuEncoder;

const RING_SLOTS: u64 = 64;
const UNIFORM_OFFSET_ALIGN: u64 = 256;

/// Cached bind group keyed by source_a/source_b/target texture view pointers.
/// Reused across frames when the same textures are bound (common case).
struct CachedBG {
    bind_group: wgpu::BindGroup,
    source_a_ptr: usize,
    source_b_ptr: usize,
    target_ptr: usize,
}

/// The standard bind group layout entries for compute dual-blit effects.
/// Shared between wgpu and hal pipeline creation.
const COMPUTE_DUAL_BLIT_BGL_ENTRIES: [wgpu::BindGroupLayoutEntry; 5] = [
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
    // binding 1: source texture A (filterable for textureSampleLevel)
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
    // binding 2: source texture B (filterable for textureSampleLevel)
    wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    },
    // binding 3: sampler (for textureSampleLevel)
    wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    },
    // binding 4: output storage texture (write-only)
    wgpu::BindGroupLayoutEntry {
        binding: 4,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba16Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    },
];

pub struct ComputeDualBlitHelper {
    pub pipeline: wgpu::ComputePipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    ring_buffer: wgpu::Buffer,
    uniform_size: u64,
    slot_stride: u64,
    ring_index: Cell<u64>,
    /// 1x1 placeholder bound as source_b when it's not read.
    pub dummy_view: wgpu::TextureView,
    cached: Option<CachedBG>,

    // --- native Metal pipeline (macOS) ---
    #[cfg(target_os = "macos")]
    native_pipeline: Option<manifold_gpu::GpuComputePipeline>,
    #[cfg(target_os = "macos")]
    native_sampler: Option<manifold_gpu::GpuSampler>,
    /// 1x1 native dummy texture for source_b when it's not read.
    #[cfg(target_os = "macos")]
    native_dummy: Option<manifold_gpu::GpuTexture>,

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
    /// Cached hal view of the 1x1 dummy texture.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_dummy_view_ptr: Option<*const <wgpu::hal::api::Metal as wgpu::hal::Api>::TextureView>,
}

// Safety: the mapped pointer points to a GPU buffer that is Send+Sync
// (Metal shared storage mode). The pointer is only written from the
// content thread which owns the ComputeDualBlitHelper.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
unsafe impl Send for ComputeDualBlitHelper {}

// Safety: native_pipeline and native_sampler are Send+Sync (Metal guarantee).
#[cfg(all(target_os = "macos", not(feature = "hal-encoding")))]
unsafe impl Send for ComputeDualBlitHelper {}

impl ComputeDualBlitHelper {
    /// Create a new compute-based two-texture effect pipeline.
    ///
    /// `uniform_size` — byte size of the effect's uniform struct (must be Pod).
    /// The compute shader must define:
    ///   @group(0) @binding(0) var<uniform> uniforms: YourStruct;
    ///   @group(0) @binding(1) var source_tex_a: texture_2d<f32>;
    ///   @group(0) @binding(2) var source_tex_b: texture_2d<f32>;
    ///   @group(0) @binding(3) var tex_sampler: sampler;
    ///   @group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;
    pub fn new(
        device: &wgpu::Device,
        shader_source: &str,
        label: &str,
        uniform_size: u64,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Self {
        let _ = &hal_ctx; // suppress unused warning when hal-encoding is off
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(&format!("{label} Compute Dual BGL")),
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
                    COMPUTE_DUAL_BLIT_BGL_ENTRIES[1],
                    COMPUTE_DUAL_BLIT_BGL_ENTRIES[2],
                    COMPUTE_DUAL_BLIT_BGL_ENTRIES[3],
                    COMPUTE_DUAL_BLIT_BGL_ENTRIES[4],
                ],
            });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("{label} Compute Dual Layout")),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&format!("{label} Compute Dual Pipeline")),
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

        // 1x1 dummy texture for source_b binding when it's not read
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("{label} Dummy")),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // --- hal pipeline + shared-memory ring buffer (when feature enabled) ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        let (hal_pipeline, hal_sampler, ring_buffer, ring_mapped_ptr, hal_ring_ptr,
             hal_dummy_view_ptr) =
        if let Some(ctx) = hal_ctx {
            use wgpu::hal::Device as HalDevice;

            // Create hal compute pipeline from same WGSL source
            let hal_pipe = crate::hal_pipeline::create_compute_pipeline(
                ctx,
                shader_source,
                "cs_main",
                &COMPUTE_DUAL_BLIT_BGL_ENTRIES,
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
                    .expect("Failed to create hal dual compute sampler")
            };

            // Create shared-memory ring buffer via hal, import into wgpu
            let buf_size = slot_stride * RING_SLOTS;
            let hal_buf = unsafe {
                ctx.device()
                    .create_buffer(&wgpu::hal::BufferDescriptor {
                        label: Some(label),
                        size: buf_size,
                        usage: wgpu::wgt::BufferUses::UNIFORM
                            | wgpu::wgt::BufferUses::MAP_WRITE,
                        memory_flags: wgpu::hal::MemoryFlags::PREFER_COHERENT,
                    })
                    .expect("Failed to create hal dual compute ring buffer")
            };

            // Map to get persistent pointer (Metal shared storage = always mapped)
            let mapping = unsafe {
                ctx.device()
                    .map_buffer(&hal_buf, 0..buf_size)
                    .expect("Failed to map hal dual compute ring buffer")
            };
            let mapped_ptr = mapping.ptr.as_ptr();

            // Import hal buffer into wgpu for the fallback/wgpu bind group path
            let wgpu_buf = unsafe {
                device.create_buffer_from_hal::<wgpu::hal::api::Metal>(
                    hal_buf,
                    &wgpu::BufferDescriptor {
                        label: Some(&format!("{label} Compute Dual Ring UBO")),
                        size: buf_size,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    },
                )
            };

            // Cache the hal pointer to the ring buffer
            let ring_hal_ptr = {
                let guard = unsafe { wgpu_buf.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("ring buffer not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };

            // Cache hal view of dummy texture
            let dummy_hal_ptr = {
                let guard = unsafe { dummy_view.as_hal::<wgpu::hal::api::Metal>() }
                    .expect("dummy view not Metal");
                let ptr: *const _ = &*guard;
                ptr
            };

            (Some(hal_pipe), Some(hal_samp), wgpu_buf, Some(mapped_ptr),
             Some(ring_hal_ptr), Some(dummy_hal_ptr))
        } else {
            let wgpu_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("{label} Compute Dual Ring UBO")),
                size: slot_stride * RING_SLOTS,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            (None, None, wgpu_buf, None, None, None)
        };

        // wgpu-only ring buffer (when feature not enabled)
        #[cfg(not(all(target_os = "macos", feature = "hal-encoding")))]
        let ring_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label} Compute Dual Ring UBO")),
            size: slot_stride * RING_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // --- native Metal pipeline from manifold-gpu ---
        #[cfg(target_os = "macos")]
        let (native_pipeline, native_sampler, native_dummy) =
            if let Some(dev) = native_device {
                let pipe = dev.create_compute_pipeline(shader_source, "cs_main", label);
                let samp = dev.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
                let dummy = dev.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
                    label: "ComputeDualBlit Native Dummy",
                });
                (Some(pipe), Some(samp), Some(dummy))
            } else {
                (None, None, None)
            };

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            ring_buffer,
            uniform_size,
            slot_stride,
            ring_index: Cell::new(0),
            dummy_view,
            cached: None,
            #[cfg(target_os = "macos")]
            native_pipeline,
            #[cfg(target_os = "macos")]
            native_sampler,
            #[cfg(target_os = "macos")]
            native_dummy,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_pipeline,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_sampler,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            ring_mapped_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_ring_ptr,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_dummy_view_ptr,
        }
    }

    /// Ensure the cached bind group is valid for the given source_a/source_b/target views.
    /// Uses dynamic uniform offset so the bind group can be reused across
    /// frames when the same textures are bound (saves ~10us per call).
    fn ensure_bind_group(
        &mut self,
        device: &wgpu::Device,
        source_a_view: &wgpu::TextureView,
        source_b_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        label: &str,
    ) {
        let a_ptr = std::ptr::from_ref(source_a_view) as usize;
        let b_ptr = std::ptr::from_ref(source_b_view) as usize;
        let tgt_ptr = std::ptr::from_ref(target_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => {
                c.source_a_ptr != a_ptr
                    || c.source_b_ptr != b_ptr
                    || c.target_ptr != tgt_ptr
            }
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
                        resource: wgpu::BindingResource::TextureView(source_a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(source_b_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(target_view),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                source_a_ptr: a_ptr,
                source_b_ptr: b_ptr,
                target_ptr: tgt_ptr,
            });
        }
    }

    /// Execute a compute dispatch reading only source_a.
    /// Binds the internal 1x1 dummy as source_b.
    ///
    /// Use this instead of `dispatch(..., &self.dummy_view, ...)` to avoid
    /// borrow conflicts (`&mut self` + `&self.dummy_view` on the same helper).
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_a_only(
        &mut self,
        gpu: &mut GpuEncoder,
        source_a_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let _slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = _slot * self.slot_stride;

        // ── NATIVE METAL dispatch path ─────────────────────────────────
        #[cfg(target_os = "macos")]
        if let Some(ref native_pipe) = self.native_pipeline
            && gpu.has_native_encoder()
        {
            let native_samp = self.native_sampler.as_ref().unwrap();
            let native_dummy = self.native_dummy.as_ref().unwrap();
            let native_source_a = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(source_a_view)
            };
            let native_target = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(target_view)
            };
            let native_enc = unsafe { gpu.native_encoder_mut() }.unwrap();
            native_enc.dispatch_compute(
                native_pipe,
                &[
                    manifold_gpu::GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                    manifold_gpu::GpuBinding::Texture { binding: 1, texture: &native_source_a },
                    manifold_gpu::GpuBinding::Texture { binding: 2, texture: native_dummy },
                    manifold_gpu::GpuBinding::Sampler { binding: 3, sampler: native_samp },
                    manifold_gpu::GpuBinding::Texture { binding: 4, texture: &native_target },
                ],
                [width.div_ceil(16), height.div_ceil(16), 1],
                label,
            );
            return;
        }

        // --- hal dispatch path ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let (Some(hal_pipe), Some(mapped_ptr), true) = (
            &self.hal_pipeline,
            self.ring_mapped_ptr,
            gpu.has_hal_encoder(),
        ) {
            use wgpu::hal::{self as hal, CommandEncoder as HalCmdEnc, Device as HalDevice};
            type MetalApi = hal::api::Metal;
            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();

            // Direct memcpy to shared-memory buffer
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uniform_bytes.as_ptr(),
                    mapped_ptr.add(byte_offset as usize),
                    uniform_bytes.len(),
                );
            }

            // Extract hal texture view pointers — drop each guard before next
            let hal_source_a_ptr = {
                let guard = unsafe { source_a_view.as_hal::<MetalApi>() }
                    .expect("source_a view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            };

            let hal_target_ptr = {
                let guard = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            };

            let hal_ring_ref = unsafe { &*self.hal_ring_ptr.unwrap() };
            let hal_sampler = self.hal_sampler.as_ref().unwrap();
            let hal_source_a_ref = unsafe { &*hal_source_a_ptr };
            let hal_dummy_ref =
                unsafe { &*self.hal_dummy_view_ptr.expect("hal dummy view not cached") };
            let hal_target_ref = unsafe { &*hal_target_ptr };

            let hal_bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry {
                                binding: 0,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 1,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 2,
                                resource_index: 1,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 3,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 4,
                                resource_index: 2,
                                count: 1,
                            },
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            hal_ring_ref,
                            0,
                            std::num::NonZero::new(self.uniform_size),
                        )],
                        samplers: &[hal_sampler],
                        textures: &[
                            hal::TextureBinding {
                                view: hal_source_a_ref,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: hal_dummy_ref,
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
                .expect("Failed to create hal dual compute bind group")
            };

            unsafe {
                hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                    label: Some(label),
                    timestamp_writes: None,
                });
                hal_enc.set_compute_pipeline(&hal_pipe.pipeline);
                hal_enc.set_bind_group(
                    &hal_pipe.pipeline_layout,
                    0,
                    &hal_bg,
                    &[byte_offset as u32],
                );
                hal_enc.dispatch([width.div_ceil(16), height.div_ceil(16), 1]);
                hal_enc.end_compute_pass();
            }

            unsafe {
                hal_ctx.device().destroy_bind_group(hal_bg);
            }

            return;
        }

        // --- wgpu dispatch path (default / fallback) ---
        gpu.queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Inline ensure_bind_group with split borrows — avoids &mut self +
        // &self.dummy_view conflict.
        let a_ptr = std::ptr::from_ref(source_a_view) as usize;
        let b_ptr = std::ptr::from_ref(&self.dummy_view) as usize;
        let tgt_ptr = std::ptr::from_ref(target_view) as usize;

        let needs_recreate = match &self.cached {
            Some(c) => {
                c.source_a_ptr != a_ptr
                    || c.source_b_ptr != b_ptr
                    || c.target_ptr != tgt_ptr
            }
            None => true,
        };

        if needs_recreate {
            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                        resource: wgpu::BindingResource::TextureView(source_a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.dummy_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(target_view),
                    },
                ],
            });
            self.cached = Some(CachedBG {
                bind_group,
                source_a_ptr: a_ptr,
                source_b_ptr: b_ptr,
                target_ptr: tgt_ptr,
            });
        }

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
        pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }

    /// Execute a compute dispatch reading two source textures.
    ///
    /// For passes that don't read source_b, use `dispatch_a_only`.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        gpu: &mut GpuEncoder,
        source_a_view: &wgpu::TextureView,
        source_b_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        uniform_bytes: &[u8],
        label: &str,
        width: u32,
        height: u32,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let _slot = self.ring_index.get() % RING_SLOTS;
        self.ring_index.set(self.ring_index.get() + 1);
        let byte_offset = _slot * self.slot_stride;

        // ── NATIVE METAL dispatch path ─────────────────────────────────
        #[cfg(target_os = "macos")]
        if let Some(ref native_pipe) = self.native_pipeline
            && gpu.has_native_encoder()
        {
            let native_samp = self.native_sampler.as_ref().unwrap();
            let native_source_a = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(source_a_view)
            };
            let native_source_b = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(source_b_view)
            };
            let native_target = unsafe {
                crate::gpu_encoder::extract_native_texture_from_view(target_view)
            };
            let native_enc = unsafe { gpu.native_encoder_mut() }.unwrap();
            native_enc.dispatch_compute(
                native_pipe,
                &[
                    manifold_gpu::GpuBinding::Bytes { binding: 0, data: uniform_bytes },
                    manifold_gpu::GpuBinding::Texture { binding: 1, texture: &native_source_a },
                    manifold_gpu::GpuBinding::Texture { binding: 2, texture: &native_source_b },
                    manifold_gpu::GpuBinding::Sampler { binding: 3, sampler: native_samp },
                    manifold_gpu::GpuBinding::Texture { binding: 4, texture: &native_target },
                ],
                [width.div_ceil(16), height.div_ceil(16), 1],
                label,
            );
            return;
        }

        // --- hal dispatch path (uses separate hal command encoder) ---
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let (Some(hal_pipe), Some(mapped_ptr), true) = (
            &self.hal_pipeline,
            self.ring_mapped_ptr,
            gpu.has_hal_encoder(),
        ) {
            use wgpu::hal::{self as hal, CommandEncoder as HalCmdEnc, Device as HalDevice};
            type MetalApi = hal::api::Metal;
            let (hal_enc, hal_ctx) = unsafe { gpu.hal_encoder_mut() }.unwrap();

            // Direct memcpy to shared-memory buffer (no API call, no staging)
            unsafe {
                std::ptr::copy_nonoverlapping(
                    uniform_bytes.as_ptr(),
                    mapped_ptr.add(byte_offset as usize),
                    uniform_bytes.len(),
                );
            }

            // Extract hal texture view pointers — MUST drop each guard before
            // acquiring the next to avoid wgpu's non-reentrant snatch lock panic.
            let hal_source_a_ptr = {
                let guard = unsafe { source_a_view.as_hal::<MetalApi>() }
                    .expect("source_a view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            };

            let hal_source_b_ptr = {
                let guard = unsafe { source_b_view.as_hal::<MetalApi>() }
                    .expect("source_b view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            };

            let hal_target_ptr = {
                let guard = unsafe { target_view.as_hal::<MetalApi>() }
                    .expect("target view not Metal");
                &*guard as *const <MetalApi as hal::Api>::TextureView
            };

            // Ring buffer uses cached pointer (no snatch lock needed)
            let hal_ring_ref = unsafe { &*self.hal_ring_ptr.unwrap() };
            let hal_sampler = self.hal_sampler.as_ref().unwrap();
            let hal_source_a_ref = unsafe { &*hal_source_a_ptr };
            let hal_source_b_ref = unsafe { &*hal_source_b_ptr };
            let hal_target_ref = unsafe { &*hal_target_ptr };

            // Create lightweight hal bind group (~0.1us — copies pointers)
            let hal_bg = unsafe {
                hal_ctx.device().create_bind_group(
                    &hal::BindGroupDescriptor {
                        label: None,
                        layout: &hal_pipe.bind_group_layout,
                        entries: &[
                            hal::BindGroupEntry {
                                binding: 0,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 1,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 2,
                                resource_index: 1,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 3,
                                resource_index: 0,
                                count: 1,
                            },
                            hal::BindGroupEntry {
                                binding: 4,
                                resource_index: 2,
                                count: 1,
                            },
                        ],
                        buffers: &[hal::BufferBinding::new_unchecked(
                            hal_ring_ref,
                            0,
                            std::num::NonZero::new(self.uniform_size),
                        )],
                        samplers: &[hal_sampler],
                        textures: &[
                            hal::TextureBinding {
                                view: hal_source_a_ref,
                                usage: wgpu::wgt::TextureUses::RESOURCE,
                            },
                            hal::TextureBinding {
                                view: hal_source_b_ref,
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
                .expect("Failed to create hal dual compute bind group")
            };

            // Encode directly into the hal command encoder — zero validation overhead.
            unsafe {
                hal_enc.begin_compute_pass(&hal::ComputePassDescriptor {
                    label: Some(label),
                    timestamp_writes: None,
                });
                hal_enc.set_compute_pipeline(&hal_pipe.pipeline);
                hal_enc.set_bind_group(
                    &hal_pipe.pipeline_layout,
                    0,
                    &hal_bg,
                    &[byte_offset as u32],
                );
                hal_enc.dispatch([width.div_ceil(16), height.div_ceil(16), 1]);
                hal_enc.end_compute_pass();
            }

            // Clean up the ephemeral bind group
            unsafe {
                hal_ctx.device().destroy_bind_group(hal_bg);
            }

            return;
        }

        // --- wgpu dispatch path (default / fallback) ---
        gpu.queue.write_buffer(&self.ring_buffer, byte_offset, uniform_bytes);

        // Update cached bind group if textures changed
        self.ensure_bind_group(gpu.device, source_a_view, source_b_view, target_view, label);

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
        pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }
}
