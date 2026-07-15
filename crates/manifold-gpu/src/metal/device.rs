//! GpuDevice — native Metal device + command queue for the content thread.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{Encoding, RefEncode};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBinaryArchive, MTLBuffer, MTLCommandBuffer, MTLCommandQueue, MTLCompileOptions,
    MTLComputePipelineDescriptor, MTLDepthStencilDescriptor, MTLDevice, MTLHeap, MTLHeapDescriptor,
    MTLLanguageVersion, MTLLibrary, MTLPipelineOption, MTLRenderPipelineDescriptor, MTLResource,
    MTLResourceOptions, MTLSamplerDescriptor, MTLStorageMode, MTLTexture, MTLTextureDescriptor,
    MTLTextureType, MTLTextureUsage, MTLVertexDescriptor, MTLVertexStepFunction,
};

use super::encoder::{EncoderState, RenderBindCache};
use super::*;
use crate::types::*;

/// Generate a compute clear shader for a given WGSL storage texel format.
fn clear_texture_wgsl(texel_format: &str) -> String {
    format!(
        r#"struct ClearColor {{ r: f32, g: f32, b: f32, a: f32, }}

@group(0) @binding(0) var output_tex: texture_storage_2d<{texel_format}, write>;
@group(0) @binding(1) var<uniform> color: ClearColor;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {{ return; }}
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(color.r, color.g, color.b, color.a));
}}"#
    )
}

/// Generate a compute clear shader for uint storage texel formats.
fn clear_texture_uint_wgsl(texel_format: &str) -> String {
    format!(
        r#"struct ClearColor {{ r: f32, g: f32, b: f32, a: f32, }}

@group(0) @binding(0) var output_tex: texture_storage_2d<{texel_format}, write>;
@group(0) @binding(1) var<uniform> color: ClearColor;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {{ return; }}
    textureStore(output_tex, vec2<i32>(id.xy), vec4<u32>(
        u32(color.r), u32(color.g), u32(color.b), u32(color.a)));
}}"#
    )
}

/// Pre-compiled compute clear pipelines for all storage-capable texture formats.
/// Eliminates the empty render encoder that was previously used for non-Rgba16Float clears.
pub(super) struct ClearPipelines {
    rgba16float: GpuComputePipeline,
    rgba8unorm: GpuComputePipeline,
    bgra8unorm: GpuComputePipeline,
    rgba32float: GpuComputePipeline,
    r32float: GpuComputePipeline,
    rg32float: GpuComputePipeline,
    r32uint: GpuComputePipeline,
}

impl ClearPipelines {
    /// Look up the clear pipeline for a texture format.
    /// Returns None for formats without storage write support (R16Float, R8Unorm, etc.)
    /// — caller falls back to render-pass clear.
    pub(super) fn get(&self, format: crate::GpuTextureFormat) -> Option<&GpuComputePipeline> {
        use crate::GpuTextureFormat::*;
        match format {
            Rgba16Float => Some(&self.rgba16float),
            Rgba8Unorm => Some(&self.rgba8unorm),
            Bgra8Unorm => Some(&self.bgra8unorm),
            Rgba32Float => Some(&self.rgba32float),
            R32Float => Some(&self.r32float),
            Rg32Float => Some(&self.rg32float),
            R32Uint => Some(&self.r32uint),
            // No WGSL storage texture support for these formats.
            R16Float | Rg16Float | R8Unorm | Rgba8UnormSrgb | Bgra8UnormSrgb | Depth32Float => None,
        }
    }
}
use super::format::*;
use super::shader_compiler::{
    compile_wgsl_to_msl, compile_wgsl_to_msl_render, find_entry_function,
};

/// Native Metal device + command queue for the content thread.
pub struct GpuDevice {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// Binary archive for pipeline caching. Protected by Mutex for Sync.
    /// Only locked during pipeline creation (startup), never on the hot path.
    archive: std::sync::Mutex<Option<archive::GpuPipelineArchive>>,
    /// In-memory pipeline cache keyed by shader hash.
    /// Eliminates repeated WGSL→MSL→Metal compilation for the same shader.
    compute_cache: std::sync::Mutex<std::collections::HashMap<u64, GpuComputePipeline>>,
    render_cache: std::sync::Mutex<std::collections::HashMap<u64, GpuRenderPipeline>>,
    /// On-disk MSL cache — skips WGSL→naga→SPIR-V→spirv-opt→SPIRV-Cross on hit.
    msl_cache: std::sync::Mutex<Option<msl_cache::MslCache>>,
    /// Pre-compiled compute clear pipelines per texture format.
    clear_pipelines: std::sync::OnceLock<ClearPipelines>,
    /// Lazily-created linear (bilinear) sampler with clamp addressing.
    /// Shared by `GpuEncoder::resize_sample` and any other helper that
    /// needs a default filtering sampler without minting a fresh
    /// `MTLSamplerState` per frame. Mirrors the `clear_pipelines`
    /// lazy-cache pattern.
    linear_sampler: std::sync::OnceLock<GpuSampler>,
    /// Device-level Xcode capture scope. A scope only defines capture
    /// boundaries through begin/end calls, so it must be retained and
    /// driven per frame — see `capture_scope_begin`/`capture_scope_end`.
    capture_scope: std::sync::OnceLock<Retained<ProtocolObject<dyn objc2_metal::MTLCaptureScope>>>,
}

// Safety: MTLDevice and MTLCommandQueue are thread-safe (Metal guarantee).
// The archive Mutex provides the synchronization for the archive field.
unsafe impl Send for GpuDevice {}
unsafe impl Sync for GpuDevice {}

impl Default for GpuDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuDevice {
    /// Create from the system default Metal device.
    /// Uses a dedicated command queue for content-thread work.
    pub fn new() -> Self {
        let device = objc2_metal::MTLCreateSystemDefaultDevice().expect("No Metal device found");
        let queue = device
            .newCommandQueue()
            .expect("Failed to create command queue");
        Self {
            device,
            queue,
            archive: std::sync::Mutex::new(None),
            compute_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            render_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            msl_cache: std::sync::Mutex::new(None),
            clear_pipelines: std::sync::OnceLock::new(),
            linear_sampler: std::sync::OnceLock::new(),
            capture_scope: std::sync::OnceLock::new(),
        }
    }

    /// Whether this device supports per-dispatch GPU timestamp profiling
    /// (timestamp counter set + stage-boundary sampling). True on Apple
    /// silicon.
    pub fn supports_dispatch_profiling(&self) -> bool {
        super::profiling::timestamp_counter_set(&self.device).is_some()
    }

    /// Create a reusable timestamp sampler with capacity for `max_spans`
    /// profiled dispatches per frame. `None` if the device doesn't support
    /// counter sampling. Attach to a frame's encoder via
    /// [`GpuEncoder::enable_dispatch_profiling`].
    pub fn create_timestamp_sampler(&self, max_spans: usize) -> Option<GpuTimestampSampler> {
        super::profiling::create_sampler(&self.device, max_spans)
    }

    /// Shared linear (bilinear) sampler with clamp-to-edge addressing,
    /// created once on first use. Use for sampling-based resize /
    /// downscale passes (`GpuEncoder::resize_sample`) so callers don't
    /// allocate a per-frame `MTLSamplerState`.
    pub fn linear_sampler(&self) -> &GpuSampler {
        self.linear_sampler
            .get_or_init(|| self.create_sampler(&GpuSamplerDesc::default()))
    }

    /// Set the default Metal capture scope to the device so that Xcode's
    /// GPU frame capture grabs command buffers from ALL threads. The scope
    /// is retained so the content thread can drive its begin/end boundary
    /// each frame — without that, Xcode's camera button silently falls back
    /// to the focused CAMetalLayer's present boundary, which dependency-
    /// tracks only the UI drawable and misses the content queue entirely.
    pub fn install_device_capture_scope(&self) {
        use objc2_metal::{MTLCaptureManager, MTLCaptureScope};
        let manager = unsafe { MTLCaptureManager::sharedCaptureManager() };
        let scope = unsafe { manager.newCaptureScopeWithDevice(&self.device) };
        unsafe { scope.setLabel(Some(&objc2_foundation::NSString::from_str("Content Frame"))) };
        let scope_proto = ProtocolObject::from_ref(&*scope);
        unsafe { manager.setDefaultCaptureScope(Some(scope_proto)) };
        let _ = self.capture_scope.set(scope);
    }

    /// Mark the start of one content frame for Xcode GPU capture. No-op
    /// unless `install_device_capture_scope` ran and Xcode is attached
    /// (begin/end on an idle scope costs nothing measurable).
    pub fn capture_scope_begin(&self) {
        use objc2_metal::MTLCaptureScope;
        if let Some(scope) = self.capture_scope.get() {
            scope.beginScope();
        }
    }

    /// Mark the end of one content frame for Xcode GPU capture.
    pub fn capture_scope_end(&self) {
        use objc2_metal::MTLCaptureScope;
        if let Some(scope) = self.capture_scope.get() {
            scope.endScope();
        }
    }

    /// Raw Metal device reference (for advanced interop).
    pub fn raw_device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// Human-readable Metal device name (e.g. `"Apple M4 Max"`). Used as the
    /// fingerprint for per-device tuning decisions (the freeze perf gate keys
    /// its fuse/don't-fuse verdicts on this) and for logging. Stable for the
    /// process lifetime — the device never changes under us.
    pub fn device_name(&self) -> String {
        self.device.name().to_string()
    }

    /// Raw Metal device pointer as `*mut c_void` (an `id<MTLDevice>`).
    /// Used for FFI interop with native Objective-C plugins.
    pub fn raw_device_ptr(&self) -> *mut std::ffi::c_void {
        Retained::as_ptr(&self.device) as *mut std::ffi::c_void
    }

    /// Raw Metal command queue reference (for advanced interop).
    pub fn raw_queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.queue
    }

    /// Clone the owned Metal command queue handle.
    /// Multiple threads can submit command buffers to the same queue; Metal
    /// serializes them in submission order on that queue.
    pub fn clone_queue(&self) -> Retained<ProtocolObject<dyn MTLCommandQueue>> {
        self.queue.clone()
    }

    /// Create a GPU texture via device allocation (kernel call per texture).
    /// Prefer `TexturePool::acquire()` for transient textures.
    pub fn create_texture(&self, desc: &GpuTextureDesc) -> GpuTexture {
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        let raw = self
            .device
            .newTextureWithDescriptor(&mtl_desc)
            .expect("Metal: texture allocation failed — GPU memory exhausted");
        GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        }
    }

    /// Create a GPU buffer with private storage (GPU-only).
    pub fn create_buffer(&self, size: u64) -> GpuBuffer {
        let raw = self
            .device
            .newBufferWithLength_options(size as usize, MTLResourceOptions::StorageModePrivate)
            .unwrap_or_else(|| {
                panic!("Metal: buffer allocation failed ({size} bytes) — GPU memory exhausted")
            });
        GpuBuffer {
            raw,
            size,
            mapped_ptr: None,
        }
    }

    /// Create a GPU buffer with shared memory (CPU+GPU coherent).
    /// Returns a buffer with a persistent mapped pointer for zero-copy writes.
    pub fn create_buffer_shared(&self, size: u64) -> GpuBuffer {
        let raw = self
            .device
            .newBufferWithLength_options(size as usize, MTLResourceOptions::StorageModeShared)
            .unwrap_or_else(|| {
                panic!(
                    "Metal: shared buffer allocation failed ({size} bytes) — GPU memory exhausted"
                )
            });
        let ptr = unsafe { raw.contents() }.as_ptr() as *mut u8;
        GpuBuffer {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Create a sampler state.
    pub fn create_sampler(&self, desc: &GpuSamplerDesc) -> GpuSampler {
        let mtl_desc = unsafe {
            use objc2::AnyThread;
            MTLSamplerDescriptor::init(MTLSamplerDescriptor::alloc())
        };
        unsafe {
            mtl_desc.setMinFilter(to_mtl_filter(desc.min_filter));
            mtl_desc.setMagFilter(to_mtl_filter(desc.mag_filter));
            mtl_desc.setMipFilter(to_mtl_mip_filter(desc.mip_filter));
            mtl_desc.setSAddressMode(to_mtl_address(desc.address_mode_u));
            mtl_desc.setTAddressMode(to_mtl_address(desc.address_mode_v));
            mtl_desc.setRAddressMode(to_mtl_address(desc.address_mode_w));
            if let Some(compare) = desc.compare {
                mtl_desc.setCompareFunction(to_mtl_compare_function(compare));
            }
            // 1 = isotropic, Metal's own default — setting it unconditionally
            // keeps this a single code path (D7: the field default already
            // matches Metal's implicit default, so this is a no-op at 1).
            mtl_desc.setMaxAnisotropy(desc.max_anisotropy as usize);
        }
        let raw = self
            .device
            .newSamplerStateWithDescriptor(&mtl_desc)
            .expect("newSamplerStateWithDescriptor failed");
        GpuSampler { raw }
    }

    /// Upload pixel data to a texture synchronously (CPU → GPU).
    pub fn upload_texture(&self, texture: &GpuTexture, data: &[u8]) {
        use objc2_metal::{MTLOrigin, MTLRegion, MTLSize};
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = texture.width as u64 * bpp as u64;
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: texture.width as usize,
                height: texture.height as usize,
                depth: 1,
            },
        };
        unsafe {
            texture.raw.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                bytes_per_row as usize,
            );
        }
    }

    /// Number of distinct compute pipelines currently cached (keyed by
    /// shader hash, shared across every caller). Test/verification-only
    /// introspection — proves a prewarm pass (e.g. BUG-037's
    /// `GltfTextureSource::prewarm_pipeline`) actually populated the shared
    /// cache, without needing a live render to observe a cache hit.
    pub fn compute_pipeline_cache_len(&self) -> usize {
        self.compute_cache.lock().unwrap().len()
    }

    /// Sibling of [`Self::compute_pipeline_cache_len`] for render pipelines
    /// (e.g. BUG-037's `RenderScene::prewarm_pipelines`).
    pub fn render_pipeline_cache_len(&self) -> usize {
        self.render_cache.lock().unwrap().len()
    }

    /// Create a compute pipeline from WGSL source (full f32 precision).
    pub fn create_compute_pipeline(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
    ) -> GpuComputePipeline {
        self.create_compute_pipeline_inner(wgsl_source, entry_point, label, false)
    }

    /// Create a compute pipeline with half-precision (f16) ALU optimization.
    pub fn create_compute_pipeline_half(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
    ) -> GpuComputePipeline {
        self.create_compute_pipeline_inner(wgsl_source, entry_point, label, true)
    }

    /// Core compute pipeline creation with configurable half-precision.
    fn create_compute_pipeline_inner(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
        use_half: bool,
    ) -> GpuComputePipeline {
        let hash = archive::pipeline_hash(wgsl_source, entry_point, use_half);
        if let Some(cached) = self.compute_cache.lock().unwrap().get(&hash) {
            return cached.clone();
        }

        // Try MSL cache first (skips naga + spirv-opt + SPIRV-Cross)
        let (slot_map, msl_source, msl_entry_name, workgroup_size) = {
            let mut msl_guard = self.msl_cache.lock().unwrap();
            if let Some(ref mut cache) = *msl_guard
                && let Some(entry) = cache.get_compute(hash)
            {
                (
                    entry.slot_map,
                    entry.msl_source,
                    entry.msl_entry_name,
                    entry.workgroup_size,
                )
            } else {
                if let Some(ref mut cache) = *msl_guard {
                    cache.record_miss();
                }
                drop(msl_guard);

                let result = compile_wgsl_to_msl(wgsl_source, entry_point, label, use_half);

                // Store in MSL cache
                if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
                    cache.put_compute(hash, &result.0, &result.1, &result.2, result.3);
                }
                result
            }
        };

        let compile_opts = unsafe {
            use objc2::AnyThread;
            MTLCompileOptions::init(MTLCompileOptions::alloc())
        };
        unsafe {
            compile_opts.setLanguageVersion(MTLLanguageVersion::Version2_4);
            compile_opts.setMathMode(objc2_metal::MTLMathMode::Fast);
        }
        let msl_ns = NSString::from_str(&msl_source);
        let library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&msl_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL library compile error: {}\nMSL source:\n{msl_source}",
                e.localizedDescription()
            )
        });

        let available_ns_names = unsafe { library.functionNames() };
        let available: Vec<String> = available_ns_names.iter().map(|s| s.to_string()).collect();
        let function = find_entry_function(&library, &msl_entry_name, &available, label, "compute");

        // Use descriptor-based creation when archive is available — enables
        // binary archive lookup (near-instant on cache hit) and auto-populates
        // the archive on miss.
        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            let desc = unsafe {
                use objc2::AnyThread;
                MTLComputePipelineDescriptor::init(MTLComputePipelineDescriptor::alloc())
            };
            unsafe {
                desc.setComputeFunction(Some(&function));
                desc.setLabel(Some(&NSString::from_str(label)));
                let archives =
                    objc2_foundation::NSArray::from_retained_slice(&[arch.raw_archive().clone()]);
                desc.setBinaryArchives(Some(&archives));
            }

            let state = unsafe {
                self.device
                    .newComputePipelineStateWithDescriptor_options_reflection_error(
                        &desc,
                        MTLPipelineOption::None,
                        None,
                    )
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL compute PSO error: {}",
                    e.localizedDescription()
                )
            });

            if !arch.was_added(hash) {
                match unsafe {
                    arch.raw_archive()
                        .addComputePipelineFunctionsWithDescriptor_error(&desc)
                } {
                    Ok(()) => {
                        arch.mark_added(hash);
                    }
                    Err(e) => {
                        log::warn!(
                            "{label}: failed to add to binary archive: {}",
                            e.localizedDescription()
                        );
                    }
                }
            }
            state
        } else {
            unsafe {
                self.device
                    .newComputePipelineStateWithFunction_error(&function)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL compute PSO error: {}",
                    e.localizedDescription()
                )
            })
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        let pipeline = GpuComputePipeline {
            state,
            slot_map,
            label: label.to_string(),
            workgroup_size,
            needs_sizes_buffer,
        };
        self.compute_cache
            .lock()
            .unwrap()
            .insert(hash, pipeline.clone());
        pipeline
    }

    /// Create a specialized compute pipeline by substituting constants in the WGSL
    /// source before compilation.
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

    /// Half-precision variant of `create_specialized_compute_pipeline`.
    pub fn create_specialized_compute_pipeline_half(
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
        self.create_compute_pipeline_half(&source, entry_point, label)
    }

    /// Create a specialized render pipeline by text-replacing patterns in WGSL
    /// source before compilation.
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
    pub fn load_pipeline_archive(&self, path: &std::path::Path) {
        if let Some(arch) = archive::GpuPipelineArchive::load_or_create(&self.device, path) {
            *self.archive.lock().unwrap() = Some(arch);
        }
    }

    /// Save the pipeline binary archive to disk (if loaded and modified).
    pub fn save_pipeline_archive(&self) {
        if let Some(ref mut arch) = *self.archive.lock().unwrap() {
            arch.save();
        }
    }

    /// Load or create an MSL shader cache at the given directory.
    pub fn load_msl_cache(&self, cache_dir: &std::path::Path) {
        *self.msl_cache.lock().unwrap() = Some(msl_cache::MslCache::new(cache_dir.to_path_buf()));
    }

    /// Log MSL cache hit/miss statistics.
    pub fn log_msl_cache_stats(&self) {
        if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
            cache.log_stats();
        }
    }

    /// Create a render pipeline from WGSL source (fullscreen triangle pattern).
    pub fn create_render_pipeline(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        label: &str,
    ) -> GpuRenderPipeline {
        self.create_render_pipeline_inner(
            wgsl_source,
            vs_entry,
            fs_entry,
            color_format,
            blend,
            1,
            label,
        )
    }

    /// Create a render pipeline with MSAA (sample_count > 1).
    pub fn create_render_pipeline_msaa(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        label: &str,
    ) -> GpuRenderPipeline {
        self.create_render_pipeline_inner(
            wgsl_source,
            vs_entry,
            fs_entry,
            color_format,
            blend,
            sample_count,
            label,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_render_pipeline_inner(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        label: &str,
    ) -> GpuRenderPipeline {
        let base_hash = archive::render_pipeline_hash(wgsl_source, vs_entry, fs_entry);
        let hash = if sample_count <= 1 {
            base_hash
        } else {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            base_hash.hash(&mut h);
            sample_count.hash(&mut h);
            h.finish()
        };
        if let Some(cached) = self.render_cache.lock().unwrap().get(&hash) {
            return cached.clone();
        }

        let (slot_map, vs_msl, fs_msl) = {
            let mut msl_guard = self.msl_cache.lock().unwrap();
            if let Some(ref mut cache) = *msl_guard
                && let Some(entry) = cache.get_render(base_hash)
            {
                (entry.slot_map, entry.vs_msl, entry.fs_msl)
            } else {
                if let Some(ref mut cache) = *msl_guard {
                    cache.record_miss();
                }
                drop(msl_guard);

                let result = compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);

                if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
                    cache.put_render(base_hash, &result.0, &result.1, &result.2);
                }
                result
            }
        };

        let compile_opts = unsafe {
            use objc2::AnyThread;
            MTLCompileOptions::init(MTLCompileOptions::alloc())
        };
        unsafe {
            compile_opts.setLanguageVersion(MTLLanguageVersion::Version2_4);
            compile_opts.setMathMode(objc2_metal::MTLMathMode::Fast);
        }

        let vs_ns = NSString::from_str(&vs_msl);
        let vs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&vs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL vertex library compile error: {}\nMSL:\n{vs_msl}",
                e.localizedDescription()
            )
        });
        let fs_ns = NSString::from_str(&fs_msl);
        let fs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&fs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL fragment library compile error: {}\nMSL:\n{fs_msl}",
                e.localizedDescription()
            )
        });

        let vs_names_raw = unsafe { vs_library.functionNames() };
        let vs_available: Vec<String> = vs_names_raw.iter().map(|s| s.to_string()).collect();
        let fs_names_raw = unsafe { fs_library.functionNames() };
        let fs_available: Vec<String> = fs_names_raw.iter().map(|s| s.to_string()).collect();
        let vs_func = find_entry_function(&vs_library, vs_entry, &vs_available, label, "vertex");
        let fs_func = find_entry_function(&fs_library, fs_entry, &fs_available, label, "fragment");

        let desc = unsafe {
            use objc2::AnyThread;
            MTLRenderPipelineDescriptor::init(MTLRenderPipelineDescriptor::alloc())
        };
        unsafe {
            desc.setVertexFunction(Some(&vs_func));
            desc.setFragmentFunction(Some(&fs_func));
            if sample_count > 1 {
                desc.setRasterSampleCount(sample_count as usize);
            }
        }

        let color_attach = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color_attach.setPixelFormat(to_mtl_pixel_format(color_format));
            if let Some(blend) = blend {
                color_attach.setBlendingEnabled(true);
                color_attach.setRgbBlendOperation(to_mtl_blend_op(blend.operation));
                color_attach.setAlphaBlendOperation(to_mtl_blend_op(blend.alpha_operation));
                color_attach.setSourceRGBBlendFactor(to_mtl_blend_factor(blend.src_factor));
                color_attach.setDestinationRGBBlendFactor(to_mtl_blend_factor(blend.dst_factor));
                color_attach.setSourceAlphaBlendFactor(to_mtl_blend_factor(blend.src_alpha_factor));
                color_attach
                    .setDestinationAlphaBlendFactor(to_mtl_blend_factor(blend.dst_alpha_factor));
            }
        }

        // Use binary archive for render pipelines (same pattern as compute).
        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            unsafe {
                let archives =
                    objc2_foundation::NSArray::from_retained_slice(&[arch.raw_archive().clone()]);
                desc.setBinaryArchives(Some(&archives));
            }

            let state = unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            });

            if !arch.was_added(hash) {
                match unsafe {
                    arch.raw_archive()
                        .addRenderPipelineFunctionsWithDescriptor_error(&desc)
                } {
                    Ok(()) => {
                        arch.mark_added(hash);
                    }
                    Err(e) => {
                        log::warn!(
                            "{label}: failed to add render PSO to binary archive: {}",
                            e.localizedDescription()
                        );
                    }
                }
            }
            state
        } else {
            unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            })
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        let pipeline = GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
            needs_sizes_buffer,
        };
        self.render_cache
            .lock()
            .unwrap()
            .insert(hash, pipeline.clone());
        pipeline
    }

    /// Create a render pipeline from WGSL source with a vertex buffer layout.
    #[allow(clippy::too_many_arguments)]
    pub fn create_render_pipeline_with_vertex_layout(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        vertex_layout: &GpuVertexLayout,
        label: &str,
    ) -> GpuRenderPipeline {
        let base_hash = archive::render_pipeline_hash(wgsl_source, vs_entry, fs_entry);
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            base_hash.hash(&mut hasher);
            vertex_layout.stride.hash(&mut hasher);
            hasher.finish()
        };
        if let Some(cached) = self.render_cache.lock().unwrap().get(&hash) {
            return cached.clone();
        }

        let (slot_map, vs_msl, fs_msl) = {
            let mut msl_guard = self.msl_cache.lock().unwrap();
            if let Some(ref mut cache) = *msl_guard
                && let Some(entry) = cache.get_render(base_hash)
            {
                (entry.slot_map, entry.vs_msl, entry.fs_msl)
            } else {
                if let Some(ref mut cache) = *msl_guard {
                    cache.record_miss();
                }
                drop(msl_guard);

                let result = compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);

                if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
                    cache.put_render(base_hash, &result.0, &result.1, &result.2);
                }
                result
            }
        };

        let compile_opts = unsafe {
            use objc2::AnyThread;
            MTLCompileOptions::init(MTLCompileOptions::alloc())
        };
        unsafe {
            compile_opts.setLanguageVersion(MTLLanguageVersion::Version2_4);
            compile_opts.setMathMode(objc2_metal::MTLMathMode::Fast);
        }

        let vs_ns = NSString::from_str(&vs_msl);
        let vs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&vs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL vertex library compile error: {}\nMSL:\n{vs_msl}",
                e.localizedDescription()
            )
        });
        let fs_ns = NSString::from_str(&fs_msl);
        let fs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&fs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL fragment library compile error: {}\nMSL:\n{fs_msl}",
                e.localizedDescription()
            )
        });

        let vs_names_raw = unsafe { vs_library.functionNames() };
        let vs_available: Vec<String> = vs_names_raw.iter().map(|s| s.to_string()).collect();
        let fs_names_raw = unsafe { fs_library.functionNames() };
        let fs_available: Vec<String> = fs_names_raw.iter().map(|s| s.to_string()).collect();
        let vs_func = find_entry_function(&vs_library, vs_entry, &vs_available, label, "vertex");
        let fs_func = find_entry_function(&fs_library, fs_entry, &fs_available, label, "fragment");

        let desc = unsafe {
            use objc2::AnyThread;
            MTLRenderPipelineDescriptor::init(MTLRenderPipelineDescriptor::alloc())
        };
        unsafe {
            desc.setVertexFunction(Some(&vs_func));
            desc.setFragmentFunction(Some(&fs_func));
        }

        const VERTEX_BUFFER_INDEX: usize = 30;
        let vtx_desc = unsafe {
            use objc2::AnyThread;
            MTLVertexDescriptor::init(MTLVertexDescriptor::alloc())
        };
        for attr in &vertex_layout.attributes {
            let a = unsafe {
                vtx_desc
                    .attributes()
                    .objectAtIndexedSubscript(attr.shader_location as usize)
            };
            unsafe {
                a.setFormat(format::to_mtl_vertex_format(attr.format));
                a.setOffset(attr.offset as usize);
                a.setBufferIndex(VERTEX_BUFFER_INDEX);
            }
        }
        let layout = unsafe {
            vtx_desc
                .layouts()
                .objectAtIndexedSubscript(VERTEX_BUFFER_INDEX)
        };
        unsafe {
            layout.setStride(vertex_layout.stride as usize);
            layout.setStepFunction(MTLVertexStepFunction::PerVertex);
            layout.setStepRate(1);
        }
        unsafe { desc.setVertexDescriptor(Some(&vtx_desc)) };

        let color_attach = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color_attach.setPixelFormat(to_mtl_pixel_format(color_format));

            if let Some(blend) = blend {
                color_attach.setBlendingEnabled(true);
                color_attach.setRgbBlendOperation(to_mtl_blend_op(blend.operation));
                color_attach.setAlphaBlendOperation(to_mtl_blend_op(blend.alpha_operation));
                color_attach.setSourceRGBBlendFactor(to_mtl_blend_factor(blend.src_factor));
                color_attach.setDestinationRGBBlendFactor(to_mtl_blend_factor(blend.dst_factor));
                color_attach.setSourceAlphaBlendFactor(to_mtl_blend_factor(blend.src_alpha_factor));
                color_attach
                    .setDestinationAlphaBlendFactor(to_mtl_blend_factor(blend.dst_alpha_factor));
            }
        }

        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            unsafe {
                let archives =
                    objc2_foundation::NSArray::from_retained_slice(&[arch.raw_archive().clone()]);
                desc.setBinaryArchives(Some(&archives));
            }

            let state = unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            });

            if !arch.was_added(hash) {
                match unsafe {
                    arch.raw_archive()
                        .addRenderPipelineFunctionsWithDescriptor_error(&desc)
                } {
                    Ok(()) => {
                        arch.mark_added(hash);
                    }
                    Err(e) => {
                        log::warn!(
                            "{label}: failed to add render PSO to binary archive: {}",
                            e.localizedDescription()
                        );
                    }
                }
            }
            state
        } else {
            unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            })
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        let pipeline = GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
            needs_sizes_buffer,
        };
        self.render_cache
            .lock()
            .unwrap()
            .insert(hash, pipeline.clone());
        pipeline
    }

    /// Get or lazily compile all compute clear pipelines.
    fn clear_pipelines(&self) -> &ClearPipelines {
        self.clear_pipelines.get_or_init(|| {
            let make = |fmt: &str| {
                let wgsl = clear_texture_wgsl(fmt);
                self.create_compute_pipeline(&wgsl, "cs_main", &format!("Clear {fmt}"))
            };
            ClearPipelines {
                rgba16float: make("rgba16float"),
                rgba8unorm: make("rgba8unorm"),
                bgra8unorm: make("bgra8unorm"),
                rgba32float: make("rgba32float"),
                r32float: make("r32float"),
                rg32float: make("rg32float"),
                r32uint: {
                    let wgsl = clear_texture_uint_wgsl("r32uint");
                    self.create_compute_pipeline(&wgsl, "cs_main", "Clear r32uint")
                },
            }
        })
    }

    /// Create a new command encoder for one frame's GPU work.
    pub fn create_encoder(&self, label: &str) -> GpuEncoder {
        let cmd_buf = self
            .queue
            .commandBuffer()
            .expect("Failed to acquire command buffer");
        unsafe { cmd_buf.setLabel(Some(&NSString::from_str(label))) };
        GpuEncoder {
            cmd_buf,
            state: EncoderState::None,
            compute_cache: super::encoder::ComputeBindCache::new(),
            render_cache: RenderBindCache::new(),
            clear_pipelines: self.clear_pipelines() as *const ClearPipelines,
            profile: None,
        }
    }

    /// Create a compiled depth-stencil state object.
    pub fn create_depth_stencil_state(&self, desc: &GpuDepthStencilDesc) -> GpuDepthStencilState {
        let ds_desc = unsafe {
            use objc2::AnyThread;
            MTLDepthStencilDescriptor::init(MTLDepthStencilDescriptor::alloc())
        };
        unsafe {
            ds_desc.setDepthCompareFunction(to_mtl_compare_function(desc.compare));
            ds_desc.setDepthWriteEnabled(desc.write_enabled);
        }
        let state = self
            .device
            .newDepthStencilStateWithDescriptor(&ds_desc)
            .expect("newDepthStencilStateWithDescriptor failed");
        GpuDepthStencilState { raw: state }
    }

    /// Create a render pipeline configured for depth testing.
    #[allow(clippy::too_many_arguments)]
    pub fn create_render_pipeline_depth(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        depth_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        label: &str,
    ) -> GpuRenderPipeline {
        self.create_render_pipeline_depth_inner(
            wgsl_source,
            vs_entry,
            fs_entry,
            color_format,
            depth_format,
            blend,
            sample_count,
            false,
            None,
            label,
        )
    }

    /// Create an MSAA depth-tested render pipeline with optional
    /// alpha-to-coverage. `alpha_to_coverage` converts the fragment
    /// shader's alpha (including a cutout `discard`'s pass/fail) into
    /// per-sample coverage, so the multisample resolve antialiases the
    /// cutout edge — not just the triangle silhouette. Only meaningful
    /// when `sample_count > 1`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_render_pipeline_depth_msaa(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        depth_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        alpha_to_coverage: bool,
        label: &str,
    ) -> GpuRenderPipeline {
        self.create_render_pipeline_depth_inner(
            wgsl_source,
            vs_entry,
            fs_entry,
            color_format,
            depth_format,
            blend,
            sample_count,
            alpha_to_coverage,
            None,
            label,
        )
    }

    /// [`Self::create_render_pipeline_depth_msaa`]'s specialized-plus-MRT
    /// superset (`docs/GBUFFER_DESIGN.md` §2 D5, P2): text-substitutes
    /// `specializations` into `wgsl_source` before compiling (the same
    /// "function constant" mechanism as [`Self::create_specialized_render_pipeline`]
    /// — a single WGSL template compiled into distinct pipeline variants,
    /// never a second shader file), and — when `aux_color_format` is
    /// `Some` — declares a second color attachment (index 1) at that pixel
    /// format so the fragment can write an extra MRT output (e.g.
    /// velocity). `aux_color_format: None` reproduces exactly
    /// [`Self::create_render_pipeline_depth_msaa`]'s single-attachment
    /// pipeline, just from substituted source.
    #[allow(clippy::too_many_arguments)]
    pub fn create_specialized_render_pipeline_depth_msaa(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        specializations: &[(&str, &str)],
        color_format: GpuTextureFormat,
        depth_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        alpha_to_coverage: bool,
        aux_color_format: Option<GpuTextureFormat>,
        label: &str,
    ) -> GpuRenderPipeline {
        let mut source = wgsl_source.to_string();
        for &(pattern, replacement) in specializations {
            source = source.replace(pattern, replacement);
        }
        self.create_render_pipeline_depth_inner(
            &source,
            vs_entry,
            fs_entry,
            color_format,
            depth_format,
            blend,
            sample_count,
            alpha_to_coverage,
            aux_color_format,
            label,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_render_pipeline_depth_inner(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        depth_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        sample_count: u32,
        alpha_to_coverage: bool,
        aux_color_format: Option<GpuTextureFormat>,
        label: &str,
    ) -> GpuRenderPipeline {
        let base_hash = archive::render_pipeline_hash(wgsl_source, vs_entry, fs_entry);
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            base_hash.hash(&mut h);
            sample_count.hash(&mut h);
            depth_format.hash(&mut h);
            alpha_to_coverage.hash(&mut h);
            aux_color_format.hash(&mut h);
            "depth".hash(&mut h);
            h.finish()
        };
        if let Some(cached) = self.render_cache.lock().unwrap().get(&hash) {
            return cached.clone();
        }

        let (slot_map, vs_msl, fs_msl) = {
            let mut msl_guard = self.msl_cache.lock().unwrap();
            if let Some(ref mut cache) = *msl_guard
                && let Some(entry) = cache.get_render(base_hash)
            {
                (entry.slot_map, entry.vs_msl, entry.fs_msl)
            } else {
                if let Some(ref mut cache) = *msl_guard {
                    cache.record_miss();
                }
                drop(msl_guard);

                let result = compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);

                if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
                    cache.put_render(base_hash, &result.0, &result.1, &result.2);
                }
                result
            }
        };

        let compile_opts = unsafe {
            use objc2::AnyThread;
            MTLCompileOptions::init(MTLCompileOptions::alloc())
        };
        unsafe {
            compile_opts.setLanguageVersion(MTLLanguageVersion::Version2_4);
            compile_opts.setMathMode(objc2_metal::MTLMathMode::Fast);
        }

        let vs_ns = NSString::from_str(&vs_msl);
        let vs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&vs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL vertex library compile error: {}\nMSL:\n{vs_msl}",
                e.localizedDescription()
            )
        });
        let fs_ns = NSString::from_str(&fs_msl);
        let fs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&fs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL fragment library compile error: {}\nMSL:\n{fs_msl}",
                e.localizedDescription()
            )
        });

        let vs_names_raw = unsafe { vs_library.functionNames() };
        let vs_available: Vec<String> = vs_names_raw.iter().map(|s| s.to_string()).collect();
        let fs_names_raw = unsafe { fs_library.functionNames() };
        let fs_available: Vec<String> = fs_names_raw.iter().map(|s| s.to_string()).collect();
        let vs_func = find_entry_function(&vs_library, vs_entry, &vs_available, label, "vertex");
        let fs_func = find_entry_function(&fs_library, fs_entry, &fs_available, label, "fragment");

        let desc = unsafe {
            use objc2::AnyThread;
            MTLRenderPipelineDescriptor::init(MTLRenderPipelineDescriptor::alloc())
        };
        unsafe {
            desc.setVertexFunction(Some(&vs_func));
            desc.setFragmentFunction(Some(&fs_func));
            if sample_count > 1 {
                desc.setRasterSampleCount(sample_count as usize);
            }
            if alpha_to_coverage {
                desc.setAlphaToCoverageEnabled(true);
            }
            desc.setDepthAttachmentPixelFormat(to_mtl_pixel_format(depth_format));
        }

        let color_attach = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color_attach.setPixelFormat(to_mtl_pixel_format(color_format));

            if let Some(blend) = blend {
                color_attach.setBlendingEnabled(true);
                color_attach.setRgbBlendOperation(to_mtl_blend_op(blend.operation));
                color_attach.setAlphaBlendOperation(to_mtl_blend_op(blend.alpha_operation));
                color_attach.setSourceRGBBlendFactor(to_mtl_blend_factor(blend.src_factor));
                color_attach.setDestinationRGBBlendFactor(to_mtl_blend_factor(blend.dst_factor));
                color_attach.setSourceAlphaBlendFactor(to_mtl_blend_factor(blend.src_alpha_factor));
                color_attach
                    .setDestinationAlphaBlendFactor(to_mtl_blend_factor(blend.dst_alpha_factor));
            }
        }

        // Optional MRT aux attachment (index 1) — `docs/GBUFFER_DESIGN.md`
        // §2 D3/D5, P2's velocity output. `None` leaves attachment 1 at the
        // descriptor's default (`PixelFormatInvalid` = unused), reproducing
        // exactly today's single-color-attachment pipeline. No blending —
        // velocity (and any future aux MRT use) writes raw values, not a
        // compositing blend.
        if let Some(aux_fmt) = aux_color_format {
            let aux_attach = unsafe { desc.colorAttachments().objectAtIndexedSubscript(1) };
            unsafe {
                aux_attach.setPixelFormat(to_mtl_pixel_format(aux_fmt));
            }
        }

        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            unsafe {
                let archives =
                    objc2_foundation::NSArray::from_retained_slice(&[arch.raw_archive().clone()]);
                desc.setBinaryArchives(Some(&archives));
            }

            let state = unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            });

            if !arch.was_added(hash) {
                match unsafe {
                    arch.raw_archive()
                        .addRenderPipelineFunctionsWithDescriptor_error(&desc)
                } {
                    Ok(()) => {
                        arch.mark_added(hash);
                    }
                    Err(e) => {
                        log::warn!(
                            "{label}: failed to add render PSO to binary archive: {}",
                            e.localizedDescription()
                        );
                    }
                }
            }
            state
        } else {
            unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: MTL render PSO error: {}",
                    e.localizedDescription()
                )
            })
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        let pipeline = GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
            needs_sizes_buffer,
        };
        self.render_cache
            .lock()
            .unwrap()
            .insert(hash, pipeline.clone());
        pipeline
    }

    /// Create a **depth-only** render pipeline — no colour attachment. For the
    /// shadow-map depth pass: geometry is rasterised only to fill a depth
    /// buffer, so the WGSL's `fs_entry` must be a void `@fragment` (writes no
    /// colour) and no colour attachment pixel format is set. Single-sample
    /// (shadow maps are never MSAA). Pair with
    /// [`GpuEncoder::draw_instanced_depth_only`].
    pub fn create_render_pipeline_depth_only(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        depth_format: GpuTextureFormat,
        label: &str,
    ) -> GpuRenderPipeline {
        let base_hash = archive::render_pipeline_hash(wgsl_source, vs_entry, fs_entry);
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            base_hash.hash(&mut h);
            depth_format.hash(&mut h);
            "depth_only".hash(&mut h);
            h.finish()
        };
        if let Some(cached) = self.render_cache.lock().unwrap().get(&hash) {
            return cached.clone();
        }

        let (slot_map, vs_msl, fs_msl) = {
            let mut msl_guard = self.msl_cache.lock().unwrap();
            if let Some(ref mut cache) = *msl_guard
                && let Some(entry) = cache.get_render(base_hash)
            {
                (entry.slot_map, entry.vs_msl, entry.fs_msl)
            } else {
                if let Some(ref mut cache) = *msl_guard {
                    cache.record_miss();
                }
                drop(msl_guard);
                let result = compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);
                if let Some(ref cache) = *self.msl_cache.lock().unwrap() {
                    cache.put_render(base_hash, &result.0, &result.1, &result.2);
                }
                result
            }
        };

        let compile_opts = unsafe {
            use objc2::AnyThread;
            MTLCompileOptions::init(MTLCompileOptions::alloc())
        };
        unsafe {
            compile_opts.setLanguageVersion(MTLLanguageVersion::Version2_4);
            compile_opts.setMathMode(objc2_metal::MTLMathMode::Fast);
        }

        let vs_ns = NSString::from_str(&vs_msl);
        let vs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&vs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL vertex library compile error: {}\nMSL:\n{vs_msl}",
                e.localizedDescription()
            )
        });
        let fs_ns = NSString::from_str(&fs_msl);
        let fs_library = unsafe {
            self.device
                .newLibraryWithSource_options_error(&fs_ns, Some(&compile_opts))
        }
        .unwrap_or_else(|e| {
            panic!(
                "{label}: MTL fragment library compile error: {}\nMSL:\n{fs_msl}",
                e.localizedDescription()
            )
        });

        let vs_available: Vec<String> = unsafe { vs_library.functionNames() }
            .iter()
            .map(|s| s.to_string())
            .collect();
        let fs_available: Vec<String> = unsafe { fs_library.functionNames() }
            .iter()
            .map(|s| s.to_string())
            .collect();
        let vs_func = find_entry_function(&vs_library, vs_entry, &vs_available, label, "vertex");
        let fs_func = find_entry_function(&fs_library, fs_entry, &fs_available, label, "fragment");

        let desc = unsafe {
            use objc2::AnyThread;
            MTLRenderPipelineDescriptor::init(MTLRenderPipelineDescriptor::alloc())
        };
        unsafe {
            desc.setVertexFunction(Some(&vs_func));
            desc.setFragmentFunction(Some(&fs_func));
            desc.setDepthAttachmentPixelFormat(to_mtl_pixel_format(depth_format));
            // Intentionally NO colour attachment pixel format — depth only.
        }

        let mut archive_guard = self.archive.lock().unwrap();
        let state = if let Some(ref mut arch) = *archive_guard {
            unsafe {
                let archives =
                    objc2_foundation::NSArray::from_retained_slice(&[arch.raw_archive().clone()]);
                desc.setBinaryArchives(Some(&archives));
            }
            let state = unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!("{label}: MTL depth-only PSO error: {}", e.localizedDescription())
            });
            if !arch.was_added(hash) {
                match unsafe {
                    arch.raw_archive()
                        .addRenderPipelineFunctionsWithDescriptor_error(&desc)
                } {
                    Ok(()) => arch.mark_added(hash),
                    Err(e) => log::warn!(
                        "{label}: failed to add depth-only PSO to binary archive: {}",
                        e.localizedDescription()
                    ),
                }
            }
            state
        } else {
            unsafe {
                self.device
                    .newRenderPipelineStateWithDescriptor_error(&desc)
            }
            .unwrap_or_else(|e| {
                panic!("{label}: MTL depth-only PSO error: {}", e.localizedDescription())
            })
        };
        drop(archive_guard);

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        let pipeline = GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
            needs_sizes_buffer,
        };
        self.render_cache
            .lock()
            .unwrap()
            .insert(hash, pipeline.clone());
        pipeline
    }

    /// Create a shared event for CPU↔GPU synchronization.
    pub fn create_event(&self) -> GpuEvent {
        let raw = self.device.newSharedEvent().expect("newSharedEvent failed");
        GpuEvent::new(raw)
    }

    /// Create a GPU heap for sub-allocation.
    pub fn create_heap(&self, size: u64, storage_mode: GpuStorageMode) -> GpuHeap {
        let desc = unsafe {
            use objc2::AnyThread;
            MTLHeapDescriptor::init(MTLHeapDescriptor::alloc())
        };
        unsafe {
            desc.setSize(size as usize);
            desc.setStorageMode(to_mtl_storage_mode(storage_mode));
        }
        let heap = self
            .device
            .newHeapWithDescriptor(&desc)
            .expect("newHeapWithDescriptor failed");
        unsafe { heap.setLabel(Some(&NSString::from_str("MANIFOLD TexturePool Heap"))) };
        GpuHeap::new(heap)
    }

    /// Query the heap size and alignment needed for a texture with the given
    /// descriptor. Used to pre-compute heap capacity.
    pub fn heap_texture_size_and_align(&self, desc: &GpuTextureDesc) -> (u64, u64) {
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        let sa = unsafe { self.device.heapTextureSizeAndAlignWithDescriptor(&mtl_desc) };
        (sa.size as u64, sa.align as u64)
    }

    /// Build a Metal TextureDescriptor from GpuTextureDesc (shared helper).
    pub(crate) fn build_mtl_texture_desc(desc: &GpuTextureDesc) -> Retained<MTLTextureDescriptor> {
        let mtl_desc = unsafe {
            use objc2::AnyThread;
            MTLTextureDescriptor::init(MTLTextureDescriptor::alloc())
        };
        unsafe {
            mtl_desc.setPixelFormat(to_mtl_pixel_format(desc.format));
            mtl_desc.setWidth(desc.width as usize);
            mtl_desc.setHeight(desc.height as usize);
            mtl_desc.setDepth(desc.depth as usize);
            mtl_desc.setTextureType(to_mtl_texture_type(desc.dimension, desc.depth));
            mtl_desc.setUsage(to_mtl_texture_usage(desc.usage));
            if desc.usage.contains(GpuTextureUsage::CPU_UPLOAD) {
                mtl_desc.setStorageMode(MTLStorageMode::Shared);
            } else {
                mtl_desc.setStorageMode(MTLStorageMode::Private);
            }
            mtl_desc.setMipmapLevelCount(desc.mip_levels.max(1) as usize);
            mtl_desc.setSampleCount(1);
        }
        mtl_desc
    }

    /// Create a texture with memoryless storage (Apple Silicon only).
    pub fn create_texture_memoryless(&self, desc: &GpuTextureDesc) -> GpuTexture {
        let mtl_desc = Self::build_mtl_texture_desc(desc);
        unsafe { mtl_desc.setStorageMode(MTLStorageMode::Memoryless) };
        let raw = self
            .device
            .newTextureWithDescriptor(&mtl_desc)
            .expect("Metal: memoryless texture allocation failed — GPU memory exhausted");
        GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        }
    }

    /// Create a memoryless multisample texture for MSAA render passes.
    pub fn create_texture_msaa_memoryless(
        &self,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        sample_count: u32,
        label: &str,
    ) -> GpuTexture {
        let mtl_desc = unsafe {
            use objc2::AnyThread;
            MTLTextureDescriptor::init(MTLTextureDescriptor::alloc())
        };
        unsafe {
            mtl_desc.setPixelFormat(to_mtl_pixel_format(format));
            mtl_desc.setWidth(width as usize);
            mtl_desc.setHeight(height as usize);
            mtl_desc.setDepth(1);
            mtl_desc.setTextureType(MTLTextureType::Type2DMultisample);
            mtl_desc.setSampleCount(sample_count as usize);
            mtl_desc.setStorageMode(MTLStorageMode::Memoryless);
            mtl_desc.setUsage(MTLTextureUsage::RenderTarget);
            mtl_desc.setMipmapLevelCount(1);
        }
        let raw = self
            .device
            .newTextureWithDescriptor(&mtl_desc)
            .unwrap_or_else(|| panic!("{label}: MSAA memoryless texture allocation failed"));
        unsafe { raw.setLabel(Some(&NSString::from_str(label))) };
        GpuTexture {
            raw,
            width,
            height,
            depth: 1,
            format,
        }
    }

    /// Create a texture pool with frame-stamped recycling.
    pub fn create_texture_pool(&self, frames_in_flight: u64) -> TexturePool {
        TexturePool::new(self, frames_in_flight)
    }

    /// Create a freestanding BGRA8888 IOSurface of the given size.
    ///
    /// The returned pointer is a +1 Core Foundation handle — the caller owns
    /// it and must release it via `CFRelease` when done. Typically you'd wrap
    /// it in an RAII guard together with the `GpuTexture` it backs.
    ///
    /// `None` on failure (rare — usually malformed properties or kernel refusal).
    ///
    /// # Safety
    /// The returned raw pointer does not carry its lifetime. The caller is
    /// responsible for calling `CFRelease` exactly once.
    pub unsafe fn create_io_surface_bgra8(width: u32, height: u32) -> Option<*mut c_void> {
        // IOSurface property keys are CFString globals from the IOSurface framework.
        #[link(name = "IOSurface", kind = "framework")]
        unsafe extern "C" {
            fn IOSurfaceCreate(properties: *const c_void) -> *mut c_void;

            static kIOSurfaceWidth: *const c_void;
            static kIOSurfaceHeight: *const c_void;
            static kIOSurfaceBytesPerElement: *const c_void;
            static kIOSurfaceBytesPerRow: *const c_void;
            static kIOSurfacePixelFormat: *const c_void;
        }

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFDictionaryCreate(
                allocator: *const c_void,
                keys: *const *const c_void,
                values: *const *const c_void,
                num_values: isize,
                key_callbacks: *const c_void,
                value_callbacks: *const c_void,
            ) -> *const c_void;
            fn CFNumberCreate(
                allocator: *const c_void,
                the_type: i32,
                value_ptr: *const c_void,
            ) -> *const c_void;
            fn CFRelease(cf: *const c_void);

            static kCFTypeDictionaryKeyCallBacks: c_void;
            static kCFTypeDictionaryValueCallBacks: c_void;
        }

        // kCFNumberSInt32Type = 3
        const K_CF_NUMBER_SINT32: i32 = 3;

        // Metal requires `bytesPerRow` on IOSurface-backed textures to be
        // aligned to 16 bytes — otherwise `newTextureWithDescriptor:iosurface:`
        // asserts with "bytesPerRow must be aligned to 16 bytes" and aborts.
        let unaligned_bpr = (width * 4) as i32;
        let bytes_per_row = (unaligned_bpr + 15) & !15;
        let bpp = 4i32;
        let pixel_format = i32::from_be_bytes(*b"BGRA");
        let w = width as i32;
        let h = height as i32;

        let make_num = |v: &i32| unsafe {
            CFNumberCreate(
                std::ptr::null(),
                K_CF_NUMBER_SINT32,
                v as *const i32 as *const c_void,
            )
        };

        unsafe {
            let num_w = make_num(&w);
            let num_h = make_num(&h);
            let num_bpe = make_num(&bpp);
            let num_bpr = make_num(&bytes_per_row);
            let num_pf = make_num(&pixel_format);

            let keys = [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfaceBytesPerRow,
                kIOSurfacePixelFormat,
            ];
            let values = [num_w, num_h, num_bpe, num_bpr, num_pf];

            let dict = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as isize,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            );

            for v in values {
                CFRelease(v);
            }

            if dict.is_null() {
                return None;
            }

            let surface = IOSurfaceCreate(dict);
            CFRelease(dict);

            if surface.is_null() {
                None
            } else {
                Some(surface)
            }
        }
    }

    /// Create a GPU texture backed by an IOSurface.
    ///
    /// # Safety
    /// The IOSurface must remain valid for the lifetime of the returned texture.
    pub unsafe fn create_texture_from_io_surface(
        &self,
        io_surface: *const std::ffi::c_void,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        usage: GpuTextureUsage,
    ) -> GpuTexture {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;

        // IOSurfaceRef encodes as `^{__IOSurface=}`, not an ObjC object.
        #[repr(C)]
        struct IOSurfaceOpaque {
            _priv: [u8; 0],
        }
        unsafe impl RefEncode for IOSurfaceOpaque {
            const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__IOSurface", &[]));
        }

        unsafe {
            let descriptor = {
                use objc2::AnyThread;
                MTLTextureDescriptor::init(MTLTextureDescriptor::alloc())
            };
            descriptor.setPixelFormat(to_mtl_pixel_format(format));
            descriptor.setWidth(width as usize);
            descriptor.setHeight(height as usize);
            descriptor.setDepth(1);
            descriptor.setMipmapLevelCount(1);
            descriptor.setSampleCount(1);
            descriptor.setTextureType(MTLTextureType::Type2D);
            descriptor.setUsage(to_mtl_texture_usage(usage));
            descriptor.setStorageMode(MTLStorageMode::Shared);

            let device_ptr: *mut AnyObject = Retained::as_ptr(&self.device) as *mut AnyObject;
            let desc_ptr: *const AnyObject = Retained::as_ptr(&descriptor) as *const AnyObject;
            let iosurface_ptr: *const IOSurfaceOpaque = io_surface.cast();
            let raw_mtl_texture: *mut AnyObject = msg_send![
                device_ptr,
                newTextureWithDescriptor: desc_ptr,
                iosurface: iosurface_ptr,
                plane: 0usize,
            ];
            assert!(
                !raw_mtl_texture.is_null(),
                "newTextureWithDescriptor:iosurface:plane: failed"
            );
            let mtl_texture: Retained<ProtocolObject<dyn objc2_metal::MTLTexture>> =
                Retained::from_raw(raw_mtl_texture.cast())
                    .expect("newTextureWithDescriptor:iosurface:plane: returned nil");
            GpuTexture::from_raw(mtl_texture, width, height, 1, format)
        }
    }
}
