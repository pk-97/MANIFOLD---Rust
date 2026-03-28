//! GpuDevice — native Metal device + command queue for the content thread.

use crate::types::*;
use super::*;
use super::encoder::EncoderState;
use super::format::*;
use super::shader_compiler::{compile_wgsl_to_msl, compile_wgsl_to_msl_render, find_entry_function};

/// Native Metal device + command queue for the content thread.
/// Created once at startup. Owns the Metal device and a dedicated command queue
/// for content-thread GPU work (separate from the UI thread's wgpu queue).
///
/// Optionally holds a `GpuPipelineArchive` for caching compiled pipeline binaries
/// to disk. When present, all `create_compute_pipeline` calls automatically use
/// the archive — no caller changes needed.
pub struct GpuDevice {
    device: metal::Device,
    queue: metal::CommandQueue,
    /// Binary archive for pipeline caching. Protected by Mutex for Sync.
    /// Only locked during pipeline creation (startup), never on the hot path.
    archive: std::sync::Mutex<Option<archive::GpuPipelineArchive>>,
}

// Safety: metal::Device and metal::CommandQueue are thread-safe (Metal guarantee).
// The archive Mutex provides the synchronization for the archive field.
unsafe impl Send for GpuDevice {}
unsafe impl Sync for GpuDevice {}

impl Default for GpuDevice {
    fn default() -> Self { Self::new() }
}

impl GpuDevice {
    /// Create from the system default Metal device.
    /// Uses a dedicated command queue for content-thread work.
    pub fn new() -> Self {
        let device = metal::Device::system_default().expect("No Metal device found");
        let queue = device.new_command_queue();
        Self {
            device,
            queue,
            archive: std::sync::Mutex::new(None),
        }
    }

    /// Raw Metal device reference (for advanced interop).
    pub fn raw_device(&self) -> &metal::DeviceRef {
        &self.device
    }

    /// Raw Metal device pointer as `*mut c_void` (an `id<MTLDevice>`).
    /// Used for FFI interop with native Objective-C plugins.
    pub fn raw_device_ptr(&self) -> *mut std::ffi::c_void {
        use metal::foreign_types::ForeignType;
        self.device.as_ptr() as *mut std::ffi::c_void
    }

    /// Raw Metal command queue reference (for advanced interop).
    pub fn raw_queue(&self) -> &metal::CommandQueueRef {
        &self.queue
    }

    /// Create a GPU texture via device allocation (kernel call per texture).
    /// Prefer `TexturePool::acquire()` for transient textures.
    pub fn create_texture(&self, desc: &GpuTextureDesc) -> GpuTexture {
        use metal::foreign_types::ForeignType;
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        let raw = self.device.new_texture(&mtl_desc);
        assert!(
            !raw.as_ptr().is_null(),
            "Metal: texture allocation failed — GPU memory exhausted",
        );
        GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        }
    }

    /// Create a GPU buffer with private storage (GPU-only).
    pub fn create_buffer(&self, size: u64, _usage: GpuBufferUsage) -> GpuBuffer {
        use metal::foreign_types::ForeignType;
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModePrivate,
        );
        assert!(
            !raw.as_ptr().is_null(),
            "Metal: buffer allocation failed ({size} bytes) — GPU memory exhausted",
        );
        GpuBuffer {
            raw,
            size,
            mapped_ptr: None,
        }
    }

    /// Create a GPU buffer with shared memory (CPU+GPU coherent).
    /// Returns a buffer with a persistent mapped pointer for zero-copy writes.
    pub fn create_buffer_shared(&self, size: u64) -> GpuBuffer {
        use metal::foreign_types::ForeignType;
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModeShared,
        );
        assert!(
            !raw.as_ptr().is_null(),
            "Metal: shared buffer allocation failed ({size} bytes) — GPU memory exhausted",
        );
        let ptr = raw.contents() as *mut u8;
        GpuBuffer {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Create a sampler state.
    pub fn create_sampler(&self, desc: &GpuSamplerDesc) -> GpuSampler {
        let mtl_desc = metal::SamplerDescriptor::new();
        mtl_desc.set_min_filter(to_mtl_filter(desc.min_filter));
        mtl_desc.set_mag_filter(to_mtl_filter(desc.mag_filter));
        mtl_desc.set_mip_filter(to_mtl_mip_filter(desc.mip_filter));
        mtl_desc.set_address_mode_s(to_mtl_address(desc.address_mode_u));
        mtl_desc.set_address_mode_t(to_mtl_address(desc.address_mode_v));
        mtl_desc.set_address_mode_r(to_mtl_address(desc.address_mode_w));
        let raw = self.device.new_sampler(&mtl_desc);
        GpuSampler { raw }
    }

    /// Upload pixel data to a texture synchronously (CPU → GPU).
    /// Uses Metal `replace_region` which works on all storage modes on macOS.
    /// Best for one-time uploads during initialization (font atlases, LUTs).
    pub fn upload_texture(
        &self,
        texture: &GpuTexture,
        data: &[u8],
    ) {
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = texture.width as u64 * bpp as u64;
        let region = metal::MTLRegion::new_2d(
            0, 0, texture.width as _, texture.height as _,
        );
        texture.raw.replace_region(
            region,
            0, // mipmap level
            data.as_ptr() as *const _,
            bytes_per_row,
        );
    }

    /// Create a compute pipeline from WGSL source.
    ///
    /// 1. Parse WGSL → naga Module
    /// 2. Introspect bindings → build slot map
    /// 3. Compile naga → MSL with slot assignments
    /// 4. Create MTLLibrary from MSL source
    /// 5. Create MTLComputePipelineState from entry function
    ///
    /// If a binary archive is loaded on this device, the pipeline is
    /// automatically cached — archive lookup on hit, recompile + add on miss.
    pub fn create_compute_pipeline(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
    ) -> GpuComputePipeline {
        let (slot_map, msl_source, msl_entry_name, workgroup_size) =
            compile_wgsl_to_msl(wgsl_source, entry_point, label);

        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_language_version(metal::MTLLanguageVersion::V2_4);
        compile_opts.set_fast_math_enabled(true);
        let library = self
            .device
            .new_library_with_source(&msl_source, &compile_opts)
            .unwrap_or_else(|e| {
                panic!("{label}: MTL library compile error: {e}\nMSL source:\n{msl_source}")
            });

        let available = library.function_names();
        let function = find_entry_function(
            &library, &msl_entry_name, &available, label, "compute",
        );

        // Use descriptor-based creation when archive is available — enables
        // binary archive lookup (near-instant on cache hit) and auto-populates
        // the archive on miss.
        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            let hash = archive::pipeline_hash(wgsl_source, entry_point);
            let desc = metal::ComputePipelineDescriptor::new();
            desc.set_compute_function(Some(&function));
            desc.set_label(label);
            desc.set_binary_archives(&[arch.raw_archive()]);

            let state = self
                .device
                .new_compute_pipeline_state(&desc)
                .unwrap_or_else(|e| panic!("{label}: MTL compute PSO error: {e}"));

            if !arch.was_added(hash) {
                if let Err(e) = arch
                    .raw_archive()
                    .add_compute_pipeline_functions_with_descriptor(&desc)
                {
                    log::warn!("{label}: failed to add to binary archive: {e}");
                } else {
                    arch.mark_added(hash);
                }
            }
            state
        } else {
            self.device
                .new_compute_pipeline_state_with_function(&function)
                .unwrap_or_else(|e| panic!("{label}: MTL compute PSO error: {e}"))
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        GpuComputePipeline {
            state,
            slot_map,
            label: label.to_string(),
            workgroup_size,
            needs_sizes_buffer,
        }
    }

    /// Create a specialized compute pipeline by substituting constants in the WGSL
    /// source before compilation. Each `(pattern, replacement)` pair performs a
    /// string replacement — e.g. `("uniforms.mode", "0u")` replaces every
    /// occurrence of `uniforms.mode` with the literal `0u`, allowing naga and
    /// the Metal compiler to constant-fold branches and dead-code eliminate
    /// inactive paths.
    ///
    /// This achieves the same effect as Metal function constants without
    /// requiring naga to emit `[[function_constant]]` annotations.
    pub fn create_specialized_compute_pipeline(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        specializations: &[(&str, &str)],
        label: &str,
    ) -> GpuComputePipeline {
        let mut source = wgsl_source.to_string();
        for &(pattern, replacement) in specializations {
            source = source.replace(pattern, replacement);
        }
        self.create_compute_pipeline(&source, entry_point, label)
    }

    /// Create a specialized render pipeline by text-replacing patterns in WGSL
    /// source before compilation. Same approach as `create_specialized_compute_pipeline`:
    /// replaces occurrences of pattern strings (e.g. `uniforms.mode`) with literal
    /// values (e.g. `0u`) so naga and Metal constant-fold and dead-code eliminate.
    pub fn create_specialized_render_pipeline(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        specializations: &[(&str, &str)],
        color_format: GpuTextureFormat,
        label: &str,
    ) -> GpuRenderPipeline {
        let mut source = wgsl_source.to_string();
        for &(pattern, replacement) in specializations {
            source = source.replace(pattern, replacement);
        }
        self.create_render_pipeline(&source, vs_entry, fs_entry, color_format, None, label)
    }

    /// Load or create a pipeline binary archive at the given path.
    /// Once loaded, all subsequent `create_compute_pipeline` calls automatically
    /// use the archive for caching. Call `save_pipeline_archive()` after all
    /// pipelines have been created to persist to disk.
    pub fn load_pipeline_archive(&self, path: &std::path::Path) {
        if let Some(arch) = archive::GpuPipelineArchive::load_or_create(&self.device, path) {
            *self.archive.lock().unwrap() = Some(arch);
        }
    }

    /// Save the pipeline binary archive to disk (if loaded and modified).
    /// Call after all pipelines have been created (e.g. end of startup).
    pub fn save_pipeline_archive(&self) {
        if let Some(ref mut arch) = *self.archive.lock().unwrap() {
            arch.save();
        }
    }

    /// Create a render pipeline from WGSL source (fullscreen triangle pattern).
    ///
    /// Vertex shader generates a fullscreen triangle from vertex_index.
    /// No vertex buffers needed. Single color attachment.
    pub fn create_render_pipeline(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        label: &str,
    ) -> GpuRenderPipeline {
        // Compile vertex and fragment entry points to separate MSL strings.
        // SPIRV-Cross compiles one entry point at a time.
        let (slot_map, vs_msl, fs_msl) =
            compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);

        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_language_version(metal::MTLLanguageVersion::V2_4);
        compile_opts.set_fast_math_enabled(true);

        // Create separate Metal libraries for vertex and fragment.
        // Metal supports vertex and fragment functions from different libraries.
        let vs_library = self
            .device
            .new_library_with_source(&vs_msl, &compile_opts)
            .unwrap_or_else(|e| {
                panic!("{label}: MTL vertex library compile error: {e}\nMSL:\n{vs_msl}")
            });
        let fs_library = self
            .device
            .new_library_with_source(&fs_msl, &compile_opts)
            .unwrap_or_else(|e| {
                panic!("{label}: MTL fragment library compile error: {e}\nMSL:\n{fs_msl}")
            });

        let vs_available = vs_library.function_names();
        let fs_available = fs_library.function_names();
        let vs_func = find_entry_function(
            &vs_library, vs_entry, &vs_available, label, "vertex",
        );
        let fs_func = find_entry_function(
            &fs_library, fs_entry, &fs_available, label, "fragment",
        );

        let desc = metal::RenderPipelineDescriptor::new();
        desc.set_vertex_function(Some(&vs_func));
        desc.set_fragment_function(Some(&fs_func));

        let color_attach = desc
            .color_attachments()
            .object_at(0)
            .expect("color attachment 0");
        color_attach.set_pixel_format(to_mtl_pixel_format(color_format));

        if let Some(blend) = blend {
            color_attach.set_blending_enabled(true);
            color_attach.set_rgb_blend_operation(to_mtl_blend_op(blend.operation));
            color_attach.set_alpha_blend_operation(to_mtl_blend_op(blend.alpha_operation));
            color_attach.set_source_rgb_blend_factor(to_mtl_blend_factor(blend.src_factor));
            color_attach
                .set_destination_rgb_blend_factor(to_mtl_blend_factor(blend.dst_factor));
            color_attach
                .set_source_alpha_blend_factor(to_mtl_blend_factor(blend.src_alpha_factor));
            color_attach
                .set_destination_alpha_blend_factor(to_mtl_blend_factor(blend.dst_alpha_factor));
        }

        // Use binary archive for render pipelines (same pattern as compute).
        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            let hash = archive::render_pipeline_hash(wgsl_source, vs_entry, fs_entry);
            desc.set_binary_archives(&[arch.raw_archive()]);

            let state = self
                .device
                .new_render_pipeline_state(&desc)
                .unwrap_or_else(|e| panic!("{label}: MTL render PSO error: {e}"));

            if !arch.was_added(hash) {
                if let Err(e) = arch
                    .raw_archive()
                    .add_render_pipeline_functions_with_descriptor(&desc)
                {
                    log::warn!("{label}: failed to add render PSO to binary archive: {e}");
                } else {
                    arch.mark_added(hash);
                }
            }
            state
        } else {
            self.device
                .new_render_pipeline_state(&desc)
                .unwrap_or_else(|e| panic!("{label}: MTL render PSO error: {e}"))
        };
        drop(archive_guard);

        GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
        }
    }

    /// Create a new command encoder for one frame's GPU work.
    pub fn create_encoder(&self, label: &str) -> GpuEncoder {
        // Use retained references — Metal retains all resources set on encoders.
        // Slightly higher overhead than unretained, but guarantees resources
        // survive until GPU execution completes. Required because we extract
        // temporary GpuTexture wrappers (via extract_native_texture) that are
        // dropped before commit.
        let cmd_buf = self.queue.new_command_buffer();
        cmd_buf.set_label(label);
        // Retain the command buffer so it outlives the autorelease pool drain.
        let ptr = cmd_buf as *const metal::CommandBufferRef as *mut std::ffi::c_void;
        unsafe { objc_retain(ptr); }
        GpuEncoder {
            cmd_buf_ptr: ptr,
            state: EncoderState::None,
        }
    }

    /// Create a shared event for CPU↔GPU synchronization.
    pub fn create_event(&self) -> GpuEvent {
        let raw = self.device.new_shared_event();
        GpuEvent::new(raw)
    }

    /// Create a GPU heap for sub-allocation.
    /// Textures allocated from a heap avoid per-allocation kernel calls.
    pub fn create_heap(
        &self,
        size: u64,
        storage_mode: GpuStorageMode,
    ) -> GpuHeap {
        let desc = metal::HeapDescriptor::new();
        desc.set_size(size as _);
        desc.set_storage_mode(to_mtl_storage_mode(storage_mode));
        let heap = self.device.new_heap(&desc);
        heap.set_label("MANIFOLD TexturePool Heap");
        GpuHeap::new(heap)
    }

    /// Query the heap size and alignment needed for a texture with the given
    /// descriptor. Used to pre-compute heap capacity.
    pub fn heap_texture_size_and_align(&self, desc: &GpuTextureDesc) -> (u64, u64) {
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        let sa = self.device.heap_texture_size_and_align(&mtl_desc);
        (sa.size, sa.align)
    }

    /// Build a Metal TextureDescriptor from GpuTextureDesc (shared helper).
    ///
    /// Lossy GPU compression (`allowGPUOptimizedContents`) is enabled by default
    /// in Metal. We never disable it — all Private-storage textures with
    /// ShaderRead + ShaderWrite usage benefit from lossy compression automatically.
    /// This reduces VRAM bandwidth for intermediates without any code changes.
    pub(crate) fn build_mtl_texture_desc(desc: &GpuTextureDesc) -> metal::TextureDescriptor {
        let mtl_desc = metal::TextureDescriptor::new();
        mtl_desc.set_pixel_format(to_mtl_pixel_format(desc.format));
        mtl_desc.set_width(desc.width as u64);
        mtl_desc.set_height(desc.height as u64);
        mtl_desc.set_depth(desc.depth as u64);
        mtl_desc.set_texture_type(to_mtl_texture_type(desc.dimension, desc.depth));
        mtl_desc.set_usage(to_mtl_texture_usage(desc.usage));
        if desc.usage.contains(GpuTextureUsage::CPU_UPLOAD) {
            mtl_desc.set_storage_mode(metal::MTLStorageMode::Shared);
        } else {
            mtl_desc.set_storage_mode(metal::MTLStorageMode::Private);
        }
        mtl_desc.set_mipmap_level_count(1);
        mtl_desc.set_sample_count(1);
        // allowGPUOptimizedContents defaults to true in Metal — we never
        // disable it. This enables lossy GPU compression for Private-storage
        // textures, reducing VRAM bandwidth for intermediates.
        mtl_desc
    }

    /// Create a texture with memoryless storage (Apple Silicon only).
    /// Data stays in tile/cache memory — zero VRAM bandwidth.
    /// Only valid as render pass attachments, NOT for compute storage textures.
    pub fn create_texture_memoryless(&self, desc: &GpuTextureDesc) -> GpuTexture {
        use metal::foreign_types::ForeignType;
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        mtl_desc.set_storage_mode(metal::MTLStorageMode::Memoryless);
        let raw = self.device.new_texture(&mtl_desc);
        assert!(
            !raw.as_ptr().is_null(),
            "Metal: memoryless texture allocation failed — GPU memory exhausted",
        );
        GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        }
    }

    /// Create a texture pool with frame-stamped recycling.
    /// `frames_in_flight` is the number of frames that can be executing
    /// concurrently on the GPU (typically 3 for triple buffering).
    pub fn create_texture_pool(&self, frames_in_flight: u64) -> TexturePool {
        TexturePool::new(self, frames_in_flight)
    }
}
