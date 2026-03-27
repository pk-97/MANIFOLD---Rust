//! Native Metal backend for macOS content thread.
//!
//! Owns metal::Device, metal::CommandQueue, metal::CommandBuffer directly.
//! Zero wgpu types, zero wgpu submission tracking, zero "(wgpu internal) Signal"
//! overhead.
//!
//! Shader compilation pipeline: WGSL → naga → SPIR-V → spirv-opt → SPIRV-Cross → MSL.
//! naga parses WGSL and provides binding introspection for the SlotMap.
//! spirv-opt runs optimization passes (constant folding, dead code elimination, etc.).
//! SPIRV-Cross compiles optimized SPIR-V to MSL with explicit resource binding indices
//! matching the SlotMap assignments. Metal compiles MSL at runtime.
//!
//! ## objc2-metal migration path (future task)
//!
//! The current `metal` crate (v0.33 from gfx-rs) is functional but missing newer
//! Metal features (MetalFX, MPS image processing), which is why `mps.rs` and
//! `metalfx.rs` use raw `objc::msg_send!`. `objc2-metal` (v0.3.2, 13M+ downloads)
//! is the successor with full API coverage including MPS and MetalFX. A full
//! migration from `metal` to `objc2-metal` would touch every file in manifold-gpu
//! (different naming conventions, ownership model via `objc2::rc::Retained`).
//! This is a future task — the current `metal` crate works correctly for all
//! existing functionality.

#[allow(unexpected_cfgs)]
pub mod mps;
pub mod archive;
pub mod metalfx;

use crate::types::*;
use spirv_cross2::compile::msl;
use spirv_cross2::Compiler;

// Raw ObjC retain/release — avoids dependency on objc::msg_send! macro.
unsafe extern "C" {
    fn objc_retain(obj: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn objc_release(obj: *mut std::ffi::c_void);
}

// ─── Slot mapping ─────────────────────────────────────────────────────

/// Maps WGSL @binding(N) to Metal argument indices.
/// Built during pipeline creation from naga module introspection.
#[derive(Clone, Debug)]
pub struct SlotMap {
    /// Indexed by WGSL @binding(N). Each entry gives the Metal argument type and index.
    slots: Vec<Option<Slot>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub kind: SlotKind,
    pub metal_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Buffer,
    Texture,
    Sampler,
}

impl SlotMap {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn insert(&mut self, binding: u32, slot: Slot) {
        let idx = binding as usize;
        if idx >= self.slots.len() {
            self.slots.resize(idx + 1, None);
        }
        self.slots[idx] = Some(slot);
    }

    /// Look up the Metal argument index for a WGSL @binding(N).
    #[inline]
    pub fn get(&self, binding: u32) -> Option<&Slot> {
        self.slots.get(binding as usize).and_then(|s| s.as_ref())
    }
}

// ─── GpuDevice ────────────────────────────────────────────────────────

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

    /// Raw Metal command queue reference (for advanced interop).
    pub fn raw_queue(&self) -> &metal::CommandQueueRef {
        &self.queue
    }

    /// Create a GPU texture via device allocation (kernel call per texture).
    /// Prefer `TexturePool::acquire()` for transient textures.
    pub fn create_texture(&self, desc: &GpuTextureDesc) -> GpuTexture {
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        let raw = self.device.new_texture(&mtl_desc);
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
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModePrivate,
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
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModeShared,
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
        let vs_func = find_entry_function(&vs_library, vs_entry, &vs_available, label, "vertex");
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

        let state = self
            .device
            .new_render_pipeline_state(&desc)
            .unwrap_or_else(|e| panic!("{label}: MTL render PSO error: {e}"));

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
        GpuEvent {
            raw,
            counter: std::cell::Cell::new(0),
        }
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
        GpuHeap { heap }
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
    fn build_mtl_texture_desc(desc: &GpuTextureDesc) -> metal::TextureDescriptor {
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
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        mtl_desc.set_storage_mode(metal::MTLStorageMode::Memoryless);
        let raw = self.device.new_texture(&mtl_desc);
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

// ─── GpuTexture ───────────────────────────────────────────────────────

/// GPU texture backed by a native Metal texture.
pub struct GpuTexture {
    pub(crate) raw: metal::Texture,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

impl GpuTexture {
    /// Wrap an existing metal::Texture (e.g. from IOSurface).
    pub fn from_raw(
        raw: metal::Texture,
        width: u32,
        height: u32,
        depth: u32,
        format: GpuTextureFormat,
    ) -> Self {
        Self { raw, width, height, depth, format }
    }

    /// Raw Metal texture reference.
    pub fn raw(&self) -> &metal::TextureRef {
        &self.raw
    }
}

// ─── GpuBuffer ────────────────────────────────────────────────────────

/// GPU buffer backed by a native Metal buffer.
pub struct GpuBuffer {
    pub(crate) raw: metal::Buffer,
    pub size: u64,
    /// Persistent mapped pointer for shared-memory buffers.
    /// Some for MTLStorageMode::Shared, None for Private.
    mapped_ptr: Option<*mut u8>,
}

unsafe impl Send for GpuBuffer {}
unsafe impl Sync for GpuBuffer {}

impl GpuBuffer {
    /// Wrap an existing metal::Buffer (e.g. extracted from wgpu).
    pub fn from_raw(raw: metal::Buffer, size: u64) -> Self {
        let ptr = raw.contents() as *mut u8;
        Self {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Persistent mapped pointer (shared-memory buffers only).
    /// Direct CPU→GPU writes with zero API overhead.
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        self.mapped_ptr
    }

    /// Write data at offset via memcpy (shared-memory buffers only).
    ///
    /// # Safety
    /// Caller must ensure offset + data.len() <= buffer size,
    /// and no GPU reads overlap this write.
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        let ptr = self.mapped_ptr.expect("write() requires shared-memory buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                ptr.add(offset as usize),
                data.len(),
            );
        }
    }

    /// Raw Metal buffer reference.
    pub fn raw(&self) -> &metal::BufferRef {
        &self.raw
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

// ─── GpuSampler ───────────────────────────────────────────────────────

pub struct GpuSampler {
    pub(crate) raw: metal::SamplerState,
}

unsafe impl Send for GpuSampler {}
unsafe impl Sync for GpuSampler {}

// ─── GpuComputePipeline ───────────────────────────────────────────────

/// Reserved WGSL "binding" index for the naga sizes buffer.
/// Not a real @binding — used internally by the slot map.
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

pub struct GpuComputePipeline {
    pub(crate) state: metal::ComputePipelineState,
    pub slot_map: SlotMap,
    pub label: String,
    /// Workgroup size from the shader's @workgroup_size declaration.
    /// Used for dispatch_thread_groups second argument.
    pub workgroup_size: [u32; 3],
    /// Whether this pipeline needs a sizes buffer for runtime-sized arrays.
    pub needs_sizes_buffer: bool,
}

unsafe impl Send for GpuComputePipeline {}
unsafe impl Sync for GpuComputePipeline {}

// ─── GpuRenderPipeline ───────────────────────────────────────────────

pub struct GpuRenderPipeline {
    pub(crate) state: metal::RenderPipelineState,
    pub slot_map: SlotMap,
    pub label: String,
}

unsafe impl Send for GpuRenderPipeline {}
unsafe impl Sync for GpuRenderPipeline {}

// ─── GpuEvent ─────────────────────────────────────────────────────────

/// GPU↔CPU synchronization via MTLSharedEvent.
/// Near-zero overhead polling (direct counter read).
pub struct GpuEvent {
    raw: metal::SharedEvent,
    counter: std::cell::Cell<u64>,
}

unsafe impl Send for GpuEvent {}
unsafe impl Sync for GpuEvent {}

impl GpuEvent {
    /// Check if the GPU has completed work signaled at `value`.
    pub fn is_done(&self, value: u64) -> bool {
        self.raw.signaled_value() >= value
    }

    /// Current signal counter (store after signal_event).
    pub fn current_value(&self) -> u64 {
        self.counter.get()
    }

    /// Read the GPU-side signaled value directly.
    pub fn signaled_value(&self) -> u64 {
        self.raw.signaled_value()
    }

    /// Raw Metal shared event reference.
    pub fn raw(&self) -> &metal::SharedEventRef {
        &self.raw
    }
}

// ─── GpuHeap ──────────────────────────────────────────────────────────

/// GPU heap backed by a native MTLHeap.
/// Sub-allocates textures without per-allocation kernel calls.
pub struct GpuHeap {
    heap: metal::Heap,
}

unsafe impl Send for GpuHeap {}
unsafe impl Sync for GpuHeap {}

impl GpuHeap {
    /// Sub-allocate a texture from this heap.
    /// Returns `None` if the heap doesn't have enough space.
    pub fn new_texture(&self, desc: &GpuTextureDesc) -> Option<GpuTexture> {
        let mtl_desc = GpuDevice::build_mtl_texture_desc(desc);
        // Override storage mode to match heap's storage mode.
        mtl_desc.set_storage_mode(self.heap.storage_mode());
        self.heap.new_texture(&mtl_desc).map(|raw| GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        })
    }

    /// Total heap size in bytes.
    pub fn size(&self) -> u64 {
        self.heap.size()
    }

    /// Currently used heap memory in bytes.
    pub fn used_size(&self) -> u64 {
        self.heap.used_size()
    }

    /// Maximum available contiguous allocation size with given alignment.
    pub fn max_available_size(&self, alignment: u64) -> u64 {
        self.heap.max_available_size_with_alignment(alignment)
    }
}

// ─── TexturePool ──────────────────────────────────────────────────────

/// Frame-stamped texture recycling pool.
///
/// Matches Unity's `RenderTexture.GetTemporary()` / `ReleaseTemporary()` pattern.
/// Textures are recycled by (width, height, format) key, but only after enough
/// frames have passed to guarantee the GPU is done reading them.
///
/// **Frame-stamped lifetime:** each released texture is tagged with the frame
/// it was released on. `acquire()` only recycles textures released at least
/// `frames_in_flight` frames ago. This prevents inter-frame GPU aliasing —
/// the same protection Unity/Unreal use internally.
///
/// After a warmup period (= frames_in_flight), allocation count drops to zero
/// at steady state. All textures are recycled, no kernel calls.
///
/// Uses interior mutability (UnsafeCell) — safe because TexturePool is only
/// used on the content thread (single-threaded).
pub struct TexturePool {
    inner: std::cell::UnsafeCell<TexturePoolInner>,
}

type PoolKey = (u32, u32, GpuTextureFormat);

/// A released texture waiting to be recycled, tagged with the frame it was
/// released on. Only eligible for reuse after `frames_in_flight` frames.
struct PoolEntry {
    texture: GpuTexture,
    release_frame: u64,
}

struct TexturePoolInner {
    available: std::collections::HashMap<PoolKey, Vec<PoolEntry>>,
    /// Owned clone of the Metal device for allocation.
    /// metal::Device is a refcounted ObjC object — clone is just a retain.
    device: metal::Device,
    /// Current frame number, incremented by begin_frame().
    current_frame: u64,
    /// Number of frames that can execute concurrently on the GPU.
    /// Textures are only recycled after this many frames have passed.
    frames_in_flight: u64,
    /// New allocations via device.create_texture().
    stats_allocated: u64,
    /// Textures recycled from pool (avoided allocation).
    stats_recycled: u64,
}

// Safety: TexturePool is only used on the content thread (single-threaded).
unsafe impl Send for TexturePool {}
unsafe impl Sync for TexturePool {}

impl TexturePool {
    /// Create a new texture pool with frame-stamped recycling.
    /// `frames_in_flight` = max concurrent GPU frames (typically 3).
    pub fn new(device: &GpuDevice, frames_in_flight: u64) -> Self {
        let mtl_device = device.raw_device().to_owned();
        log::info!(
            "TexturePool: frame-stamped recycling, {} frames in flight",
            frames_in_flight,
        );
        Self {
            inner: std::cell::UnsafeCell::new(TexturePoolInner {
                available: std::collections::HashMap::new(),
                device: mtl_device,
                current_frame: 0,
                frames_in_flight,
                stats_allocated: 0,
                stats_recycled: 0,
            }),
        }
    }

    /// Mark the start of a new frame. Must be called once per frame before
    /// any acquire/release calls. Drives the frame-stamp recycling clock.
    pub fn begin_frame(&self) {
        let inner = unsafe { &mut *self.inner.get() };
        inner.current_frame += 1;
    }

    /// Acquire a texture, recycling one if a safe match is available.
    /// Only recycles textures released >= `frames_in_flight` frames ago,
    /// guaranteeing the GPU has finished reading them.
    /// Falls back to `device.create_texture()` if no safe match exists.
    pub fn acquire(
        &self,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        usage: GpuTextureUsage,
        _label: &str,
    ) -> GpuTexture {
        let inner = unsafe { &mut *self.inner.get() };
        let key = (width, height, format);

        // Try to recycle a texture that's old enough to be safe.
        if let Some(vec) = inner.available.get_mut(&key)
            && let Some(idx) = vec.iter().position(|entry| {
                inner.current_frame.saturating_sub(entry.release_frame)
                    >= inner.frames_in_flight
            })
        {
            inner.stats_recycled += 1;
            return vec.swap_remove(idx).texture;
        }

        // No safe recycled texture — allocate fresh via device.
        inner.stats_allocated += 1;
        let desc = GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage,
            label: _label,
        };
        let mtl_desc = GpuDevice::build_mtl_texture_desc(&desc);
        let raw = inner.device.new_texture(&mtl_desc);
        GpuTexture {
            raw,
            width,
            height,
            depth: 1,
            format,
        }
    }

    /// Return a texture to the pool for future reuse.
    /// Tagged with the current frame — won't be recycled until
    /// `frames_in_flight` frames have passed.
    pub fn release(&self, texture: GpuTexture) {
        let inner = unsafe { &mut *self.inner.get() };
        let key = (texture.width, texture.height, texture.format);
        inner.available.entry(key).or_default().push(PoolEntry {
            texture,
            release_frame: inner.current_frame,
        });
    }

    /// Release all cached textures. Call on resolution change or shutdown.
    pub fn clear(&self) {
        let inner = unsafe { &mut *self.inner.get() };
        inner.available.clear();
    }

    /// Pool statistics: (total_allocated, total_recycled).
    pub fn stats(&self) -> (u64, u64) {
        let inner = unsafe { &*self.inner.get() };
        (inner.stats_allocated, inner.stats_recycled)
    }

    /// Number of textures currently cached in the pool.
    pub fn cached_count(&self) -> usize {
        let inner = unsafe { &*self.inner.get() };
        inner.available.values().map(|v| v.len()).sum()
    }

    /// Current frame number.
    pub fn current_frame(&self) -> u64 {
        let inner = unsafe { &*self.inner.get() };
        inner.current_frame
    }

    /// Remove textures that have been sitting in the pool unreused for
    /// `stale_frames` frames. Prevents GPU memory from growing monotonically
    /// after resolution changes or project switches.
    pub fn prune_stale(&self, stale_frames: u64) {
        let inner = unsafe { &mut *self.inner.get() };
        let threshold = inner.current_frame.saturating_sub(stale_frames);
        let mut pruned = 0u64;
        inner.available.retain(|_key, entries| {
            let before = entries.len();
            entries.retain(|entry| entry.release_frame >= threshold);
            pruned += (before - entries.len()) as u64;
            !entries.is_empty()
        });
        if pruned > 0 {
            log::debug!(
                "TexturePool: pruned {} stale textures (threshold={})",
                pruned,
                stale_frames,
            );
        }
    }
}

// ─── GpuEncoder ───────────────────────────────────────────────────────

/// Encoder state — tracks the current active Metal encoder.
#[allow(dead_code)]
enum EncoderState {
    None,
    /// Active compute command encoder.
    Compute(*const metal::ComputeCommandEncoderRef),
    /// Active render command encoder.
    Render(*const metal::RenderCommandEncoderRef),
    /// Active blit command encoder.
    Blit(*const metal::BlitCommandEncoderRef),
}

/// Per-frame GPU command encoder. Wraps a retained Metal command buffer.
///
/// Automatically manages compute/render/blit encoder transitions.
/// Compute encoders are kept alive across dispatches for efficiency.
/// Render/blit encoders are ended after each pass.
pub struct GpuEncoder {
    /// Retained MTLCommandBuffer. Released on drop.
    cmd_buf_ptr: *mut std::ffi::c_void,
    state: EncoderState,
}

unsafe impl Send for GpuEncoder {}

impl GpuEncoder {
    fn cmd_buf(&self) -> &metal::CommandBufferRef {
        unsafe { &*(self.cmd_buf_ptr as *const metal::CommandBufferRef) }
    }

    /// Get the raw command buffer for direct encoding (MPS kernels, MetalFX).
    /// Ends any active encoder first to avoid encoding conflicts.
    pub fn raw_cmd_buf(&mut self) -> &metal::CommandBufferRef {
        self.end_current();
        self.cmd_buf()
    }

    /// Ensure a compute encoder is active. Returns a raw pointer to it.
    fn ensure_compute(&mut self) -> *const metal::ComputeCommandEncoderRef {
        if let EncoderState::Compute(ptr) = self.state {
            return ptr;
        }
        self.end_current();
        let enc = self.cmd_buf().new_compute_command_encoder();
        let ptr = enc as *const metal::ComputeCommandEncoderRef;
        // Retain the encoder so it survives autorelease pool drains.
        // The autoreleased reference from new_compute_command_encoder() could
        // be freed by an outer pool drain in release builds.
        unsafe { objc_retain(ptr as *mut std::ffi::c_void); }
        self.state = EncoderState::Compute(ptr);
        ptr
    }

    /// End the current encoder (if any).
    fn end_current(&mut self) {
        match self.state {
            EncoderState::None => {}
            EncoderState::Compute(ptr) => {
                unsafe { &*ptr }.end_encoding();
                unsafe { objc_release(ptr as *mut std::ffi::c_void); }
            }
            EncoderState::Render(ptr) => {
                unsafe { &*ptr }.end_encoding();
                // Render encoders are not retained (created+ended in same scope)
            }
            EncoderState::Blit(ptr) => {
                unsafe { &*ptr }.end_encoding();
                // Blit encoders are not retained (created+ended in same scope)
            }
        }
        self.state = EncoderState::None;
    }

    /// Dispatch a compute shader.
    ///
    /// Automatically manages encoder state — if a compute encoder is already
    /// active, reuses it. If a render/blit encoder is active, ends it first.
    ///
    /// `bindings` use WGSL @binding(N) indices. The pipeline's slot map
    /// translates to Metal buffer/texture/sampler argument indices.
    pub fn dispatch_compute(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        let enc_ptr = self.ensure_compute();
        let enc = unsafe { &*enc_ptr };
        enc.push_debug_group(label);
        enc.set_compute_pipeline_state(&pipeline.state);

        // Collect buffer sizes for the sizes buffer (runtime-sized arrays).
        // naga's MSL backend reads arrayLength() from this auxiliary buffer.
        let mut buffer_sizes: Vec<u32> = Vec::new();

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    // Skip bindings not used by this entry point. Metal ignores
                    // unused argument slots, so this is safe. Multi-entry-point
                    // shaders have per-entry slot maps that may exclude globals
                    // not referenced by the specific entry point.
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_buffer(
                        slot.metal_index as _,
                        Some(&buffer.raw),
                        *offset as _,
                    );
                    // Track buffer size for sizes buffer generation.
                    // Indexed by Metal buffer argument index.
                    let idx = slot.metal_index as usize;
                    if idx >= buffer_sizes.len() {
                        buffer_sizes.resize(idx + 1, 0);
                    }
                    buffer_sizes[idx] = buffer.size as u32;
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        // Bind the sizes buffer if this pipeline has runtime-sized arrays.
        if pipeline.needs_sizes_buffer {
            let slot = pipeline.slot_map.get(SIZES_BUFFER_BINDING)
                .expect("sizes buffer slot missing");
            enc.set_bytes(
                slot.metal_index as _,
                (buffer_sizes.len() * 4) as _,
                buffer_sizes.as_ptr() as *const _,
            );
        }

        let wg = pipeline.workgroup_size;
        enc.dispatch_thread_groups(
            metal::MTLSize::new(workgroups[0] as _, workgroups[1] as _, workgroups[2] as _),
            metal::MTLSize::new(wg[0] as _, wg[1] as _, wg[2] as _),
        );
        enc.pop_debug_group();
    }

    /// Draw a fullscreen triangle with a render pipeline.
    ///
    /// Creates a new render encoder for each call (render targets may differ).
    /// Used by SimpleBlitHelper, DualTextureBlitHelper, etc.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_fullscreen(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        clear: bool,
        store: bool,
        label: &str,
    ) {
        self.end_current();

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(if clear {
            metal::MTLLoadAction::Clear
        } else {
            metal::MTLLoadAction::DontCare
        });
        color.set_store_action(if store {
            metal::MTLStoreAction::Store
        } else {
            metal::MTLStoreAction::DontCare
        });
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        enc.pop_debug_group();
        enc.end_encoding();
        // State goes back to None (render encoder consumed).
    }

    /// Draw instanced geometry with a render pipeline.
    ///
    /// Unlike `draw_fullscreen()` which only sets fragment bindings,
    /// this sets bindings on BOTH vertex and fragment stages.
    /// Used by LinePipeline for instanced line/dot rendering.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        clear: bool,
        label: &str,
    ) {
        self.end_current();

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(if clear {
            metal::MTLLoadAction::Clear
        } else {
            metal::MTLLoadAction::DontCare
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    // Set on both vertex and fragment stages
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_bytes(
                        slot.metal_index as _, data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _, data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        if instance_count > 0 {
            enc.draw_primitives_instanced(
                metal::MTLPrimitiveType::Triangle,
                0, vertex_count as u64, instance_count as u64,
            );
        }
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Clear a texture to a solid color via a render pass with MTLLoadAction::Clear.
    /// No draw call — just load-clear + store.
    pub fn clear_texture(&mut self, texture: &GpuTexture, r: f64, g: f64, b: f64, a: f64) {
        self.end_current();
        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&texture.raw));
        color.set_load_action(metal::MTLLoadAction::Clear);
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(r, g, b, a));
        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.end_encoding();
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.fill_buffer(&buffer.raw, metal::NSRange::new(0, buffer.size), 0);
        enc.end_encoding();
    }

    /// Copy texture to texture via blit encoder.
    pub fn copy_texture_to_texture(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        width: u32,
        height: u32,
        depth: u32,
    ) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.copy_from_texture(
            &src.raw,
            0, // source_slice
            0, // source_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize::new(width as _, height as _, depth as _),
            &dst.raw,
            0, // dest_slice
            0, // dest_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        enc.end_encoding();
    }

    /// Copy texture to buffer via blit encoder (for readback).
    pub fn copy_texture_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    ) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        let src_size = metal::MTLSize::new(width as _, height as _, 1);
        let src_origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        enc.copy_from_texture_to_buffer(
            &src.raw,
            0, // slice
            0, // level
            src_origin,
            src_size,
            &dst.raw,
            0,                      // destination_offset
            bytes_per_row as u64,   // destination_bytes_per_row
            bytes_per_row as u64 * height as u64, // destination_bytes_per_image
            metal::MTLBlitOption::empty(),
        );
        enc.end_encoding();
    }

    /// Upload CPU data to a 2D texture region via blit encoder.
    /// `bytes_per_pixel` is inferred from the texture format.
    pub fn upload_texture(
        &mut self,
        texture: &GpuTexture,
        width: u32,
        height: u32,
        _depth: u32,
        data: &[u8],
    ) {
        self.end_current();
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = width as u64 * bpp as u64;
        let region = metal::MTLRegion::new_2d(0, 0, width as _, height as _);
        texture.raw.replace_region(
            region,
            0, // mipmap level
            data.as_ptr() as *const _,
            bytes_per_row,
        );
    }

    /// Signal a shared event on the GPU timeline.
    /// The event value is incremented automatically.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        let value = event.counter.get() + 1;
        event.counter.set(value);
        // Encode signal on current command buffer (after all work).
        self.end_current();
        self.cmd_buf().encode_signal_event(event.raw(), value);
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    /// Used for per-layer completion signals in async compute.
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        self.cmd_buf().encode_signal_event(event.raw(), value);
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    /// Used by the compositor to wait for all layer generation to complete.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        self.cmd_buf().encode_wait_for_event(event.raw(), value);
    }

    /// Encode a MetalFX spatial upscale (src → dst).
    /// Ends any active encoder first. The scaler must match the texture dimensions.
    pub fn encode_metalfx_upscale(
        &mut self,
        scaler: &metalfx::MetalFxSpatialScaler,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        self.end_current();
        scaler.encode(self.cmd_buf(), src, dst);
    }

    /// Encode an MPS Lanczos upscale (src → dst).
    /// Automatically computes the scale transform from texture dimensions.
    pub fn encode_mps_upscale(
        &mut self,
        scaler: &mps::MpsLanczosScale,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        self.end_current();
        scaler.set_transform(&mps::MpsScaleTransform {
            scale_x: dst.width as f64 / src.width as f64,
            scale_y: dst.height as f64 / src.height as f64,
            translate_x: 0.0,
            translate_y: 0.0,
        });
        scaler.encode(self.cmd_buf(), &src.raw, &dst.raw);
    }

    /// Commit the command buffer to the GPU queue.
    /// Ends any active encoder and commits. Consumes the encoder.
    pub fn commit(mut self) {
        self.end_current();
        self.cmd_buf().commit();
        // Don't release in commit — Drop handles it
    }

}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        if !self.cmd_buf_ptr.is_null() {
            unsafe { objc_release(self.cmd_buf_ptr); }
        }
    }
}

// ─── WGSL→SPIR-V→spirv-opt→SPIRV-Cross→MSL compilation ──────────────

/// Generate optimized SPIR-V from a naga module:
///   1. naga Module → SPIR-V (via naga::back::spv)
///   2. SPIR-V → optimized SPIR-V (via spirv-tools optimizer)
fn compile_to_optimized_spirv(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    label: &str,
) -> Vec<u32> {
    let spv_options = naga::back::spv::Options {
        lang_version: (1, 3),
        flags: naga::back::spv::WriterFlags::empty(),
        ..Default::default()
    };
    let spv_words = naga::back::spv::write_vec(module, info, &spv_options, None)
        .unwrap_or_else(|e| panic!("{label}: naga SPIR-V output error: {e}"));

    optimize_spirv(&spv_words, label)
}

/// Run spirv-opt optimization passes on SPIR-V words.
/// Falls back to unoptimized SPIR-V if optimization fails.
fn optimize_spirv(spv_words: &[u32], label: &str) -> Vec<u32> {
    use spirv_tools::opt::{self, Optimizer};

    let mut optimizer = opt::create(None);

    // Register key optimization passes (same categories as spirv-opt -O):
    optimizer
        .register_pass(opt::Passes::InlineExhaustive)
        .register_pass(opt::Passes::EliminateDeadFunctions)
        .register_pass(opt::Passes::EliminateDeadConstant)
        .register_pass(opt::Passes::EliminateDeadMembers)
        .register_pass(opt::Passes::DeadVariableElimination)
        .register_pass(opt::Passes::ConditionalConstantPropagation)
        .register_pass(opt::Passes::AggressiveDCE)
        .register_pass(opt::Passes::Simplification)
        .register_pass(opt::Passes::StrengthReduction)
        .register_pass(opt::Passes::BlockMerge)
        .register_pass(opt::Passes::CFGCleanup)
        .register_pass(opt::Passes::LocalSingleStoreElim)
        .register_pass(opt::Passes::LocalMultiStoreElim)
        .register_pass(opt::Passes::LocalAccessChainConvert)
        .register_pass(opt::Passes::InsertExtractElim)
        .register_pass(opt::Passes::CopyPropagateArrays)
        .register_pass(opt::Passes::VectorDCE)
        .register_pass(opt::Passes::RedundancyElimination)
        .register_pass(opt::Passes::ReduceLoadSize)
        .register_pass(opt::Passes::CombineAccessChains)
        .register_pass(opt::Passes::CodeSinking)
        .register_pass(opt::Passes::CompactIds);

    match optimizer.optimize(spv_words, &mut |msg| {
        log::warn!("{label}: spirv-opt: {msg:?}");
    }, None) {
        Ok(binary) => {
            // binary.as_words() gives us &[u32]
            binary.as_words().to_vec()
        }
        Err(e) => {
            log::warn!(
                "{label}: spirv-opt optimization failed ({e}), using unoptimized SPIR-V"
            );
            spv_words.to_vec()
        }
    }
}

/// Compile optimized SPIR-V to MSL via SPIRV-Cross for a single entry point.
///
/// SPIRV-Cross's MSL backend compiles one entry point at a time.
/// For multi-entry-point modules (render pipelines), call this once per entry point
/// and create separate Metal libraries for each.
fn compile_spirv_entry_to_msl(
    spv_words: &[u32],
    naga_module: &naga::Module,
    slot_map: &SlotMap,
    entry_point: &str,
    exec_model: spirv_cross2::spirv::ExecutionModel,
    label: &str,
) -> String {
    use spirv_cross2::Module;
    use spirv_cross2::targets::Msl;

    let sc_module = Module::from_words(spv_words);
    let mut compiler: Compiler<Msl> = Compiler::new(sc_module)
        .unwrap_or_else(|e| panic!("{label}: SPIRV-Cross compiler creation error: {e}"));

    // Set the active entry point
    compiler.set_entry_point(entry_point, exec_model)
        .unwrap_or_else(|e| {
            panic!("{label}: SPIRV-Cross set_entry_point('{entry_point}') error: {e}")
        });

    // Add explicit resource bindings matching our SlotMap.
    add_resource_bindings_from_slot_map(
        &mut compiler, naga_module, slot_map, exec_model, label,
    );

    // Configure MSL compiler options
    let mut options = <Msl as spirv_cross2::compile::CompilableTarget>::options();
    options.version = msl::MslVersion::new(2, 4, 0);
    options.platform = msl::MetalPlatform::MacOS;
    options.force_native_arrays = true;

    let artifact = compiler.compile(&options)
        .unwrap_or_else(|e| {
            panic!("{label}: SPIRV-Cross MSL compilation error: {e}")
        });

    artifact.to_string()
}

/// Add explicit MSL resource bindings to SPIRV-Cross compiler matching our SlotMap.
///
/// Iterates over naga module globals, finds their WGSL @binding(N), looks up the
/// Metal argument index from the SlotMap, and tells SPIRV-Cross to use that index.
fn add_resource_bindings_from_slot_map(
    compiler: &mut Compiler<spirv_cross2::targets::Msl>,
    naga_module: &naga::Module,
    slot_map: &SlotMap,
    exec_model: spirv_cross2::spirv::ExecutionModel,
    label: &str,
) {
    for (_handle, gv) in naga_module.global_variables.iter() {
        let Some(ref binding) = gv.binding else {
            continue;
        };
        let wgsl_binding = binding.binding;
        let Some(slot) = slot_map.get(wgsl_binding) else {
            continue;
        };

        // SPIRV-Cross uses descriptor set 0 for all bindings (naga puts
        // everything in set 0 when outputting SPIR-V).
        let resource_binding = msl::ResourceBinding::Qualified {
            set: binding.group,
            binding: wgsl_binding,
        };
        let mut bind_target = msl::BindTarget {
            buffer: 0,
            texture: 0,
            sampler: 0,
            count: None,
        };
        match slot.kind {
            SlotKind::Buffer => bind_target.buffer = slot.metal_index,
            SlotKind::Texture => bind_target.texture = slot.metal_index,
            SlotKind::Sampler => bind_target.sampler = slot.metal_index,
        }

        if let Err(e) = compiler.add_resource_binding(
            exec_model, resource_binding, &bind_target,
        ) {
            log::warn!(
                "{label}: failed to set MSL binding for @binding({wgsl_binding}): {e}"
            );
        }
    }

    // If there's a sizes buffer in the slot map, add it too
    if let Some(sizes_slot) = slot_map.get(SIZES_BUFFER_BINDING) {
        let resource_binding = msl::ResourceBinding::BufferSizeBuffer(sizes_slot.metal_index);
        let bind_target = msl::BindTarget {
            buffer: sizes_slot.metal_index,
            texture: 0,
            sampler: 0,
            count: None,
        };
        if let Err(e) = compiler.add_resource_binding(
            exec_model, resource_binding, &bind_target,
        ) {
            log::warn!(
                "{label}: failed to set MSL sizes buffer binding: {e}"
            );
        }
    }
}

/// Parse WGSL, introspect bindings, compile to MSL for a compute entry point.
/// Returns (slot_map, msl_source, msl_entry_name).
fn compile_wgsl_to_msl(
    wgsl_source: &str,
    entry_point: &str,
    label: &str,
) -> (SlotMap, String, String, [u32; 3]) {
    // Step 1: Parse WGSL
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));

    // Step 2: Validate
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

    // Step 3: Introspect bindings and build slot map
    let (slot_map, _entry_resources) = build_slot_map(&module, entry_point);

    // Step 4: Get workgroup size from naga module
    let entry_idx = module
        .entry_points
        .iter()
        .position(|ep| ep.name == entry_point)
        .unwrap_or_else(|| panic!("{label}: entry point '{entry_point}' not found in module"));
    let workgroup_size = module.entry_points[entry_idx].workgroup_size;

    // Step 5: WGSL → SPIR-V → spirv-opt → SPIRV-Cross → MSL
    let optimized_spirv = compile_to_optimized_spirv(&module, &info, label);
    let msl_source = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &slot_map,
        entry_point,
        spirv_cross2::spirv::ExecutionModel::GLCompute,
        label,
    );

    // SPIRV-Cross preserves entry point names from SPIR-V (which come from
    // naga's SPIR-V backend preserving the original WGSL names).
    let msl_entry_name = entry_point.to_string();

    (slot_map, msl_source, msl_entry_name, workgroup_size)
}

/// Parse WGSL, introspect bindings, compile to MSL for render (vertex + fragment).
///
/// SPIRV-Cross compiles one entry point at a time, so we compile vertex and
/// fragment separately into individual MSL strings. The caller creates separate
/// Metal libraries for each.
///
/// Returns (unified_slot_map, vs_msl, fs_msl).
fn compile_wgsl_to_msl_render(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    label: &str,
) -> (SlotMap, String, String) {
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

    // Build a UNIFIED slot map from the union of both entry points' globals.
    // VS and FS share the same Metal argument table, so bindings visible in
    // either stage need slots (e.g. line pipeline: positions/edges in VS only).
    let (unified_slot_map, _resources_vs, _resources_fs) =
        build_slot_map_render(&module, vs_entry, fs_entry);

    // WGSL → SPIR-V → spirv-opt (shared)
    let optimized_spirv = compile_to_optimized_spirv(&module, &info, label);

    // Compile vertex and fragment entry points to MSL separately.
    // SPIRV-Cross's MSL backend emits one entry point per compile() call.
    let vs_msl = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &unified_slot_map,
        vs_entry,
        spirv_cross2::spirv::ExecutionModel::Vertex,
        label,
    );
    let fs_msl = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &unified_slot_map,
        fs_entry,
        spirv_cross2::spirv::ExecutionModel::Fragment,
        label,
    );

    (unified_slot_map, vs_msl, fs_msl)
}

/// Build a unified SlotMap + per-entry-point EntryPointResources for a render
/// pipeline (vertex + fragment). Both stages share the same Metal argument table,
/// so the slot map includes globals from the union of both entry points.
/// Each stage gets its own EntryPointResources with the shared index assignments.
fn build_slot_map_render(
    module: &naga::Module,
    vs_entry: &str,
    fs_entry: &str,
) -> (SlotMap, naga::back::msl::EntryPointResources, naga::back::msl::EntryPointResources) {
    use naga::back::msl;

    // Collect globals from both entry points
    fn collect_ep_globals(
        module: &naga::Module,
        entry_name: &str,
    ) -> std::collections::HashSet<naga::Handle<naga::GlobalVariable>> {
        let ep = module.entry_points.iter().find(|ep| ep.name == entry_name);
        if let Some(ep) = ep {
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            module.global_variables.iter().map(|(h, _)| h).collect()
        }
    }

    let vs_globals = collect_ep_globals(module, vs_entry);
    let fs_globals = collect_ep_globals(module, fs_entry);

    // Union of both entry points' globals
    let all_globals: std::collections::HashSet<_> =
        vs_globals.union(&fs_globals).copied().collect();

    // Collect bindings from the union
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && all_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    bindings.sort_by_key(|(b, _, _)| *b);

    // Build unified slot map + per-entry-point resources with shared indices
    let mut slot_map = SlotMap::new();
    let mut resources_vs = msl::EntryPointResources::default();
    let mut resources_fs = msl::EntryPointResources::default();
    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    for (binding_num, resource_binding, gv) in &bindings {
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(ty.inner, naga::TypeInner::Image { .. });
        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => {
                access.contains(naga::StorageAccess::STORE)
            }
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Buffer,
                metal_index: idx,
            });
        } else if is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Sampler,
                metal_index: idx,
            });
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Texture,
                metal_index: idx,
            });
        }

        // Add to both entry points' resources — naga + fake_missing_bindings
        // handles the case where a binding is only used in one stage.
        resources_vs.resources.insert(*resource_binding, bind_target.clone());
        resources_fs.resources.insert(*resource_binding, bind_target);
    }

    (slot_map, resources_vs, resources_fs)
}

/// Build a SlotMap and naga EntryPointResources from a naga module.
///
/// Iterates over global variables used by the entry point and assigns
/// sequential Metal argument indices per resource type:
/// - Buffers (uniform + storage) → buffer(0), buffer(1), ...
/// - Textures (sampled + storage) → texture(0), texture(1), ...
/// - Samplers → sampler(0), sampler(1), ...
fn build_slot_map(
    module: &naga::Module,
    entry_point: &str,
) -> (SlotMap, naga::back::msl::EntryPointResources) {
    use naga::back::msl;

    let mut slot_map = SlotMap::new();
    let mut resources = msl::EntryPointResources::default();

    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    // Find which global variables are actually used by this entry point.
    // Multi-entry-point shaders (e.g. fluid_scatter.wgsl) reuse @binding(N)
    // for different types per entry point — we must only map the ones used.
    let ep = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry_point);

    // Scan entry point AND all reachable functions for GlobalVariable references.
    // The entry point's function body may call helper functions that reference
    // globals (e.g. bloom_compute.wgsl: cs_main → blur13 → source_tex_b).
    // We must include globals from called functions too, or bindings get dropped.
    let used_globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
        if let Some(ep) = ep {
            // First collect all functions called from the entry point (transitively).
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);

            // Scan entry point + all called functions for GlobalVariable refs.
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            // Fallback: include all globals if entry point not found
            module.global_variables.iter().map(|(h, _)| h).collect()
        };

    // Collect bindings only for globals referenced by this entry point
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && used_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    // Sort by binding number for deterministic index assignment
    bindings.sort_by_key(|(b, _, _)| *b);

    for (binding_num, resource_binding, gv) in &bindings {
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(
            ty.inner,
            naga::TypeInner::Image { .. }
        );

        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => {
                access.contains(naga::StorageAccess::STORE)
            }
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Buffer,
                metal_index: idx,
            });
        } else if is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Sampler,
                metal_index: idx,
            });
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Texture,
                metal_index: idx,
            });
        }

        resources
            .resources
            .insert(*resource_binding, bind_target);
    }

    // Detect runtime-sized arrays in storage buffers.
    // naga's MSL backend needs a "sizes buffer" containing the byte size of each
    // runtime-sized buffer so it can resolve arrayLength() calls.
    // Covers both top-level `array<T>` and struct with last member `array<T>`.
    let has_runtime_array = bindings.iter().any(|(_, _, gv)| {
        matches!(gv.space, naga::AddressSpace::Storage { .. }) && {
            let ty = &module.types[gv.ty];
            match &ty.inner {
                // Top-level runtime-sized array: var<storage> foo: array<T>
                naga::TypeInner::Array { size: naga::ArraySize::Dynamic, .. } => true,
                // Struct with last member being a runtime-sized array
                naga::TypeInner::Struct { members, .. } => {
                    members.last().is_some_and(|m| {
                        matches!(
                            module.types[m.ty].inner,
                            naga::TypeInner::Array { size: naga::ArraySize::Dynamic, .. }
                        )
                    })
                }
                // Binding array (runtime array of resources)
                naga::TypeInner::BindingArray { size: naga::ArraySize::Dynamic, .. } => true,
                _ => false,
            }
        }
    });

    if has_runtime_array {
        // Assign the sizes buffer to the next available buffer index.
        resources.sizes_buffer = Some(next_buffer as u8);
        // Store in slot map so dispatch can bind it.
        slot_map.insert(SIZES_BUFFER_BINDING, Slot {
            kind: SlotKind::Buffer,
            metal_index: next_buffer,
        });
        next_buffer += 1;
    }

    let _ = (ep, next_buffer); // suppress unused warnings

    (slot_map, resources)
}

/// Collect GlobalVariable handles referenced in a function's expressions.
fn collect_globals_from_function(
    func: &naga::Function,
    out: &mut std::collections::HashSet<naga::Handle<naga::GlobalVariable>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::GlobalVariable(handle) = *expr {
            out.insert(handle);
        }
    }
}

/// Recursively collect all functions called from `func` (transitive closure).
fn collect_called_functions(
    func: &naga::Function,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::CallResult(fn_handle) = *expr
            && out.insert(fn_handle)
        {
            collect_called_functions(&module.functions[fn_handle], module, out);
        }
    }
    // Also scan block statements for Call statements (not all calls have results)
    collect_calls_from_block(&func.body, module, out);
}

/// Scan a naga Block for Call statements and collect called function handles.
fn collect_calls_from_block(
    block: &naga::Block,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for stmt in block.iter() {
        match *stmt {
            naga::Statement::Call { function, .. } => {
                if out.insert(function) {
                    collect_called_functions(&module.functions[function], module, out);
                }
            }
            naga::Statement::Block(ref inner) => {
                collect_calls_from_block(inner, module, out);
            }
            naga::Statement::If { ref accept, ref reject, .. } => {
                collect_calls_from_block(accept, module, out);
                collect_calls_from_block(reject, module, out);
            }
            naga::Statement::Switch { ref cases, .. } => {
                for case in cases {
                    collect_calls_from_block(&case.body, module, out);
                }
            }
            naga::Statement::Loop { ref body, ref continuing, .. } => {
                collect_calls_from_block(body, module, out);
                collect_calls_from_block(continuing, module, out);
            }
            _ => {}
        }
    }
}

/// Find an entry function in a Metal library. Tries the exact name first,
/// then looks for naga-mangled versions (e.g. "cs_main" → "cs_main_").
fn find_entry_function(
    library: &metal::LibraryRef,
    entry_name: &str,
    available: &[String],
    label: &str,
    stage: &str,
) -> metal::Function {
    // Try exact name
    if let Ok(f) = library.get_function(entry_name, None) {
        return f;
    }
    // Try with underscore suffix (naga sometimes appends)
    let mangled = format!("{entry_name}_");
    if let Ok(f) = library.get_function(&mangled, None) {
        return f;
    }
    // Try matching prefix
    for name in available {
        if name.starts_with(entry_name)
            && let Ok(f) = library.get_function(name, None)
        {
            return f;
        }
    }
    panic!(
        "{label}: {stage} function '{entry_name}' not found. Available: {available:?}"
    );
}

// ─── Format conversion helpers ────────────────────────────────────────

fn to_mtl_pixel_format(format: GpuTextureFormat) -> metal::MTLPixelFormat {
    match format {
        GpuTextureFormat::Rgba16Float => metal::MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => metal::MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => metal::MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::R32Float => metal::MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => metal::MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => metal::MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => metal::MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => metal::MTLPixelFormat::R32Uint,
        GpuTextureFormat::Rgba8UnormSrgb => metal::MTLPixelFormat::RGBA8Unorm_sRGB,
        GpuTextureFormat::Bgra8Unorm => metal::MTLPixelFormat::BGRA8Unorm,
        GpuTextureFormat::R8Unorm => metal::MTLPixelFormat::R8Unorm,
    }
}

fn to_mtl_texture_type(dim: GpuTextureDimension, _depth: u32) -> metal::MTLTextureType {
    match dim {
        GpuTextureDimension::D2 => metal::MTLTextureType::D2,
        GpuTextureDimension::D3 => metal::MTLTextureType::D3,
    }
}

fn to_mtl_storage_mode(mode: GpuStorageMode) -> metal::MTLStorageMode {
    match mode {
        GpuStorageMode::Private => metal::MTLStorageMode::Private,
        GpuStorageMode::Shared => metal::MTLStorageMode::Shared,
        GpuStorageMode::Managed => metal::MTLStorageMode::Managed,
        GpuStorageMode::Memoryless => metal::MTLStorageMode::Memoryless,
    }
}

fn to_mtl_texture_usage(usage: GpuTextureUsage) -> metal::MTLTextureUsage {
    let mut mtl = metal::MTLTextureUsage::Unknown;
    if usage.contains(GpuTextureUsage::SHADER_READ) {
        mtl |= metal::MTLTextureUsage::ShaderRead;
    }
    if usage.contains(GpuTextureUsage::SHADER_WRITE) {
        mtl |= metal::MTLTextureUsage::ShaderWrite;
    }
    if usage.contains(GpuTextureUsage::RENDER_TARGET) {
        mtl |= metal::MTLTextureUsage::RenderTarget;
    }
    mtl
}

fn to_mtl_filter(filter: GpuFilterMode) -> metal::MTLSamplerMinMagFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMinMagFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMinMagFilter::Linear,
    }
}

fn to_mtl_mip_filter(filter: GpuFilterMode) -> metal::MTLSamplerMipFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMipFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMipFilter::Linear,
    }
}

fn to_mtl_address(mode: GpuAddressMode) -> metal::MTLSamplerAddressMode {
    match mode {
        GpuAddressMode::ClampToEdge => metal::MTLSamplerAddressMode::ClampToEdge,
        GpuAddressMode::Repeat => metal::MTLSamplerAddressMode::Repeat,
        GpuAddressMode::MirrorRepeat => metal::MTLSamplerAddressMode::MirrorRepeat,
        GpuAddressMode::ClampToZero => metal::MTLSamplerAddressMode::ClampToZero,
    }
}

fn to_mtl_blend_factor(factor: GpuBlendFactor) -> metal::MTLBlendFactor {
    match factor {
        GpuBlendFactor::Zero => metal::MTLBlendFactor::Zero,
        GpuBlendFactor::One => metal::MTLBlendFactor::One,
        GpuBlendFactor::SrcAlpha => metal::MTLBlendFactor::SourceAlpha,
        GpuBlendFactor::OneMinusSrcAlpha => metal::MTLBlendFactor::OneMinusSourceAlpha,
        GpuBlendFactor::DstAlpha => metal::MTLBlendFactor::DestinationAlpha,
        GpuBlendFactor::OneMinusDstAlpha => metal::MTLBlendFactor::OneMinusDestinationAlpha,
        GpuBlendFactor::SrcColor => metal::MTLBlendFactor::SourceColor,
        GpuBlendFactor::OneMinusSrcColor => metal::MTLBlendFactor::OneMinusSourceColor,
        GpuBlendFactor::DstColor => metal::MTLBlendFactor::DestinationColor,
        GpuBlendFactor::OneMinusDstColor => metal::MTLBlendFactor::OneMinusDestinationColor,
    }
}

fn to_mtl_blend_op(op: GpuBlendOp) -> metal::MTLBlendOperation {
    match op {
        GpuBlendOp::Add => metal::MTLBlendOperation::Add,
        GpuBlendOp::Subtract => metal::MTLBlendOperation::Subtract,
        GpuBlendOp::ReverseSubtract => metal::MTLBlendOperation::ReverseSubtract,
        GpuBlendOp::Min => metal::MTLBlendOperation::Min,
        GpuBlendOp::Max => metal::MTLBlendOperation::Max,
    }
}

