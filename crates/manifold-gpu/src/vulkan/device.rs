//! Vulkan `GpuDevice`: the handle every resource hangs off. One `VkInstance`
//! and `VkDevice` are shared by every wrapper in the process; a wrapper owns
//! its queue (D16). Methods still to build name the phase that builds them.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{AllocationCreateDesc, AllocationScheme};

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};

use super::core::{Core, vk_format};
use super::pipeline::{
    DiskPipelineCache, SpirvCache, compare_op, compute_hash, create_compute, create_render,
};
use super::submit::Queue;
use super::types::{BufferCore, EventCore, ImageCore, SamplerCore, ViewCore};
use super::*;
use crate::alloc_counts::{
    BUFFER_ALLOCATION, TEXTURE_ALLOCATION, alloc_log_backtrace, alloc_log_enabled,
};
use crate::*;

pub struct GpuDevice {
    pub(crate) core: Arc<Core>,
    pub(crate) queue: Arc<Queue>,
    resource_scope_id: u64,
    linear_sampler: OnceLock<GpuSampler>,
    /// Compute pipelines by shader translation identity, as on Metal.
    compute_cache: Mutex<HashMap<u64, GpuComputePipeline>>,
    /// Render pipelines by shader translation identity and target state.
    render_cache: Mutex<HashMap<u64, GpuRenderPipeline>>,
    /// `copy_depth_to_float`'s pipeline, built with the first encoder.
    depth_to_float: OnceLock<Arc<GpuComputePipeline>>,
    pipeline_cache: Mutex<Option<DiskPipelineCache>>,
    spirv_cache: Mutex<Option<SpirvCache>>,
    /// Fence-stamped drop retirement, set once at pipeline init; every
    /// texture and buffer allocated afterwards carries the mark.
    retirement: OnceLock<Arc<crate::RetireMark>>,
}

impl Default for GpuDevice {
    fn default() -> Self {
        Self::new()
    }
}

/// The texture pool's allocations.
pub(crate) struct PoolAllocator {
    core: Arc<Core>,
    queue: Arc<Queue>,
}

impl PoolAllocator {
    pub(crate) fn new(device: &GpuDevice) -> Self {
        Self {
            core: device.core.clone(),
            queue: device.queue.clone(),
        }
    }

    pub(crate) fn allocate(
        &self,
        desc: &GpuTextureDesc,
        retire: Option<Arc<crate::RetireMark>>,
    ) -> GpuTexture {
        let mut texture = create_texture_on(&self.core, &self.queue, desc, 1, retire)
            .unwrap_or_else(|e| panic!("Vulkan: TexturePool allocation failed: {e}"));
        texture.depth = 1;
        texture
    }
}

/// Usages a format cannot take, already warned about.
static WARNED_USAGE: Mutex<Vec<(GpuTextureFormat, vk::ImageUsageFlags)>> = Mutex::new(Vec::new());

/// The image usage for `desc`, without what the format cannot do. Copies
/// are always allowed: Metal blits any texture.
fn image_usage(core: &Core, desc: &GpuTextureDesc) -> vk::ImageUsageFlags {
    let features = core.format_features(desc.format);
    let attachment = if desc.format == GpuTextureFormat::Depth32Float {
        (
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
        )
    } else {
        (
            vk::ImageUsageFlags::COLOR_ATTACHMENT,
            vk::FormatFeatureFlags::COLOR_ATTACHMENT,
        )
    };
    let wanted = [
        (
            GpuTextureUsage::SHADER_READ,
            vk::ImageUsageFlags::SAMPLED,
            vk::FormatFeatureFlags::SAMPLED_IMAGE,
        ),
        (
            GpuTextureUsage::SHADER_WRITE,
            vk::ImageUsageFlags::STORAGE,
            vk::FormatFeatureFlags::STORAGE_IMAGE,
        ),
        (GpuTextureUsage::RENDER_TARGET, attachment.0, attachment.1),
    ];
    let mut usage = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
    for (gpu, vk_usage, feature) in wanted {
        if !desc.usage.contains(gpu) {
            continue;
        }
        if features.contains(feature) {
            usage |= vk_usage;
            continue;
        }
        let mut warned = WARNED_USAGE.lock().expect("usage warnings poisoned");
        if !warned.contains(&(desc.format, vk_usage)) {
            warned.push((desc.format, vk_usage));
            log::warn!(
                "[Vulkan] {:?} cannot be used as {vk_usage:?} on this device; created without it",
                desc.format
            );
        }
    }
    usage
}

/// Allocate, name and initialise an image to `GENERAL` (design section 5),
/// and view all of it.
pub(super) fn create_texture_on(
    core: &Arc<Core>,
    queue: &Queue,
    desc: &GpuTextureDesc,
    samples: u32,
    retire: Option<Arc<crate::RetireMark>>,
) -> Result<GpuTexture, String> {
    if alloc_log_enabled() {
        eprintln!(
            "[gpu-alloc] tex {} {}x{}x{} {:?} mips={}",
            desc.label, desc.width, desc.height, desc.depth, desc.format, desc.mip_levels
        );
        alloc_log_backtrace();
    }
    let (image_type, view_type, extent_depth, layers) = match desc.dimension {
        GpuTextureDimension::D2 => (vk::ImageType::TYPE_2D, vk::ImageViewType::TYPE_2D, 1, 1),
        GpuTextureDimension::D3 => (
            vk::ImageType::TYPE_3D,
            vk::ImageViewType::TYPE_3D,
            desc.depth.max(1),
            1,
        ),
        GpuTextureDimension::D2Array => (
            vk::ImageType::TYPE_2D,
            vk::ImageViewType::TYPE_2D_ARRAY,
            1,
            desc.depth.max(1),
        ),
    };
    let mip_levels = desc.mip_levels.max(1);
    let info = vk::ImageCreateInfo::default()
        .image_type(image_type)
        .format(vk_format(desc.format))
        .extent(vk::Extent3D {
            width: desc.width.max(1),
            height: desc.height.max(1),
            depth: extent_depth,
        })
        .mip_levels(mip_levels)
        .array_layers(layers)
        .samples(vk::SampleCountFlags::from_raw(samples))
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(image_usage(core, desc))
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { core.device.create_image(&info, None) }
        .map_err(|e| format!("image {} ({}x{}): {e}", desc.label, desc.width, desc.height))?;
    let requirements = unsafe { core.device.get_image_memory_requirements(image) };
    let allocation =
        core.allocator
            .lock()
            .expect("allocator poisoned")
            .allocate(&AllocationCreateDesc {
                name: desc.label,
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            });
    let allocation = match allocation {
        Ok(a) => a,
        Err(e) => {
            unsafe { core.device.destroy_image(image, None) };
            return Err(format!(
                "texture memory for {} ({}x{}, {:?}): {e}",
                desc.label, desc.width, desc.height, desc.format
            ));
        }
    };
    unsafe {
        core.device
            .bind_image_memory(image, allocation.memory(), allocation.offset())
    }
    .map_err(|e| format!("bind image memory: {e}"))?;
    core.name(image, desc.label);
    let aspect = if desc.format == GpuTextureFormat::Depth32Float {
        vk::ImageAspectFlags::DEPTH
    } else {
        vk::ImageAspectFlags::COLOR
    };
    let image = Arc::new(ImageCore::new(
        core.clone(),
        image,
        allocation,
        aspect,
        desc.usage,
        mip_levels,
    ));
    let range = vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: mip_levels,
        base_array_layer: 0,
        layer_count: layers,
    };
    core.submit_now(queue, |cmd| {
        let barrier = [vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .image(image.raw)
            .subresource_range(range)];
        unsafe {
            core.device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barrier),
            )
        };
    });
    let view = ViewCore::new(
        image,
        desc.format,
        view_type,
        (0, mip_levels),
        (0, layers),
        desc.label,
    );
    Ok(GpuTexture {
        width: desc.width,
        height: desc.height,
        depth: desc.depth,
        format: desc.format,
        retire,
        view: Arc::new(view),
    })
}

/// Every use a Metal buffer allows.
const BUFFER_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::from_raw(
    vk::BufferUsageFlags::STORAGE_BUFFER.as_raw()
        | vk::BufferUsageFlags::UNIFORM_BUFFER.as_raw()
        | vk::BufferUsageFlags::TRANSFER_SRC.as_raw()
        | vk::BufferUsageFlags::TRANSFER_DST.as_raw()
        | vk::BufferUsageFlags::INDIRECT_BUFFER.as_raw()
        | vk::BufferUsageFlags::VERTEX_BUFFER.as_raw()
        | vk::BufferUsageFlags::INDEX_BUFFER.as_raw(),
);

fn to_vk_filter(filter: GpuFilterMode) -> vk::Filter {
    match filter {
        GpuFilterMode::Nearest => vk::Filter::NEAREST,
        GpuFilterMode::Linear => vk::Filter::LINEAR,
    }
}

fn to_vk_address(mode: GpuAddressMode) -> vk::SamplerAddressMode {
    match mode {
        GpuAddressMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        GpuAddressMode::Repeat => vk::SamplerAddressMode::REPEAT,
        GpuAddressMode::MirrorRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        GpuAddressMode::ClampToZero => vk::SamplerAddressMode::CLAMP_TO_BORDER,
    }
}

pub(crate) fn to_vk_compare(compare: GpuCompareFunction) -> vk::CompareOp {
    match compare {
        GpuCompareFunction::Never => vk::CompareOp::NEVER,
        GpuCompareFunction::Less => vk::CompareOp::LESS,
        GpuCompareFunction::Equal => vk::CompareOp::EQUAL,
        GpuCompareFunction::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        GpuCompareFunction::Greater => vk::CompareOp::GREATER,
        GpuCompareFunction::NotEqual => vk::CompareOp::NOT_EQUAL,
        GpuCompareFunction::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
        GpuCompareFunction::Always => vk::CompareOp::ALWAYS,
    }
}

impl GpuDevice {
    fn on_core(core: Arc<Core>) -> Self {
        Self {
            queue: core.take_queue(),
            core,
            resource_scope_id: crate::alloc_counts::next_resource_scope_id(),
            linear_sampler: OnceLock::new(),
            compute_cache: Mutex::new(HashMap::new()),
            render_cache: Mutex::new(HashMap::new()),
            depth_to_float: OnceLock::new(),
            pipeline_cache: Mutex::new(None),
            spirv_cache: Mutex::new(None),
            retirement: OnceLock::new(),
        }
    }

    fn retirement_mark(&self) -> Option<Arc<crate::RetireMark>> {
        self.retirement.get().map(Arc::clone)
    }

    fn create_buffer_in(&self, size: u64, location: MemoryLocation) -> Result<GpuBuffer, String> {
        if alloc_log_enabled() {
            eprintln!("[gpu-alloc] buf {size} {location:?}");
        }
        let mut buffer = create_buffer_on(&self.core, size, location)?;
        crate::alloc_counts::record_allocation(self.resource_scope_id, BUFFER_ALLOCATION);
        buffer.retire = self.retirement_mark();
        Ok(buffer)
    }
}

/// A buffer with every Metal use, `GpuOnly` ones unmapped. No retirement
/// mark and no allocation count: the wrapper adds those.
pub(crate) fn create_buffer_on(
    core: &Arc<Core>,
    size: u64,
    location: MemoryLocation,
) -> Result<GpuBuffer, String> {
    // Vulkan buffers cannot be empty; Metal's can.
    let info = vk::BufferCreateInfo::default()
        .size(size.max(4))
        .usage(BUFFER_USAGE)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let buffer = unsafe { core.device.create_buffer(&info, None) }
        .map_err(|e| format!("buffer ({size} bytes): {e}"))?;
    let requirements = unsafe { core.device.get_buffer_memory_requirements(buffer) };
    let allocation =
        core.allocator
            .lock()
            .expect("allocator poisoned")
            .allocate(&AllocationCreateDesc {
                name: "buffer",
                requirements,
                location,
                linear: true,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            });
    let allocation = match allocation {
        Ok(a) => a,
        Err(e) => {
            unsafe { core.device.destroy_buffer(buffer, None) };
            return Err(format!("buffer memory ({size} bytes): {e}"));
        }
    };
    unsafe {
        core.device
            .bind_buffer_memory(buffer, allocation.memory(), allocation.offset())
    }
    .map_err(|e| format!("bind buffer memory: {e}"))?;
    Ok(GpuBuffer {
        size,
        inner: Arc::new(BufferCore::new(
            core.clone(),
            buffer,
            allocation,
            location != MemoryLocation::GpuOnly,
        )),
        retire: None,
    })
}

impl GpuDevice {
    /// Ray-traced shadows and lighting (P7).
    pub fn supports_ray_tracing(&self) -> bool {
        false
    }

    /// The ray-tracing denoiser (P7).
    pub fn supports_denoiser(&self) -> bool {
        false
    }

    /// Temporal and spatial upscaling (P7); `fsr1` is the portable spatial path.
    pub fn supports_upscaler(&self) -> bool {
        false
    }

    /// The GPU FFT behind `GpuFft` (P7).
    pub fn supports_fft(&self) -> bool {
        false
    }

    /// Device-local usage and budget for scene-modifier admission (D18);
    /// `None` where the driver lacks `VK_EXT_memory_budget`.
    pub fn modifier_memory_snapshot(&self) -> Option<crate::GpuMemorySnapshot> {
        self.core.memory_snapshot()
    }
}

impl GpuDevice {
    /// [`GpuDevice::new`] after taking the machine-wide GPU queue
    /// ([`crate::queue`]) for the rest of the process.
    pub fn new_queued(label: &str) -> Self {
        crate::queue::acquire_for_process(label);
        Self::new()
    }

    /// Create from the system default Metal device.
    pub fn new() -> Self {
        // This crate's own unit tests are dev-only by construction.
        #[cfg(test)]
        crate::queue::acquire_for_process("manifold-gpu unit tests");
        Self::on_core(Core::shared())
    }

    /// Another wrapper on this wrapper's native device, with its own queue,
    /// caches and resource scope (D16).
    pub fn sibling(&self) -> Self {
        Self::on_core(self.core.clone())
    }

    /// Process-local identity for this device wrapper's resource ownership.
    pub fn resource_scope_id(&self) -> u64 {
        self.resource_scope_id
    }

    /// Whether this device supports per-dispatch GPU timestamp profiling
    /// (timestamp counter set + stage-boundary sampling).
    pub fn supports_dispatch_profiling(&self) -> bool {
        self.core.timestamps.is_some()
    }

    /// Create a reusable timestamp sampler with capacity for `max_spans`
    /// profiled dispatches per frame.
    pub fn create_timestamp_sampler(&self, max_spans: usize) -> Option<GpuTimestampSampler> {
        super::profiling::create_sampler(&self.core, max_spans)
    }

    /// Shared linear (bilinear) sampler with clamp-to-edge addressing,
    /// created once on first use.
    pub fn linear_sampler(&self) -> &GpuSampler {
        self.linear_sampler
            .get_or_init(|| self.create_sampler(&GpuSamplerDesc::default()))
    }

    /// Set the default Metal capture scope to the device so that Xcode's
    /// GPU frame capture grabs command buffers from ALL threads.
    pub fn install_device_capture_scope(&self) {}

    /// Mark the start of one content frame for Xcode GPU capture.
    pub fn capture_scope_begin(&self) {}

    /// Mark the end of one content frame for Xcode GPU capture.
    pub fn capture_scope_end(&self) {}

    /// Create the device's single content-thread residency manager.
    pub fn create_residency_manager(&self) -> Option<super::GpuResidencyManager> {
        None
    }

    /// Human-readable Metal device name (e.g.
    pub fn device_name(&self) -> String {
        self.core.name.clone()
    }

    /// Deterministic retirement for GPU-referenced objects (BUG-84fv class).
    pub fn retire_after_queue<T: 'static>(&self, obj: T, _label: &'static str) {
        struct Pin<T>(T);
        // Safety: the waiter thread only DROPS the pinned object. Callers
        // pass GPU handles (their drops are thread-safe) or plain data, as
        // on Metal.
        unsafe impl<T> Send for Pin<T> {}
        let pin = Pin(obj);
        self.queue.after_submitted(Box::new(move || drop(pin)));
    }

    /// Successful Metal buffer, texture and RT acceleration-structure
    /// allocations the calling thread made on this device.
    pub fn allocation_counts(&self) -> [u64; 3] {
        crate::alloc_counts::allocation_counts(self.resource_scope_id)
    }

    /// Create a GPU texture via device allocation (kernel call per texture).
    pub fn create_texture(&self, desc: &GpuTextureDesc) -> GpuTexture {
        self.try_create_texture(desc)
            .unwrap_or_else(|e| panic!("Vulkan: texture allocation failed — {e}"))
    }

    /// Fallible sibling of [`Self::create_texture`] for staged resource
    /// preparation.
    pub fn try_create_texture(&self, desc: &GpuTextureDesc) -> Result<GpuTexture, String> {
        let texture = create_texture_on(&self.core, &self.queue, desc, 1, self.retirement_mark())?;
        crate::alloc_counts::record_allocation(self.resource_scope_id, TEXTURE_ALLOCATION);
        Ok(texture)
    }

    /// Create a buffer for `usage`: `Gpu` in device-local memory and never
    /// mapped, `Upload` in host-visible memory written through the mapping,
    /// `Readback` in host-cached memory (D14).
    pub fn create_buffer(&self, size: u64, usage: BufferUse) -> GpuBuffer {
        self.try_create_buffer(size, usage)
            .unwrap_or_else(|e| panic!("Vulkan: {e} — GPU memory exhausted"))
    }

    /// Fallible sibling of [`Self::create_buffer`] for staged allocation.
    pub fn try_create_buffer(&self, size: u64, usage: BufferUse) -> Result<GpuBuffer, String> {
        let location = match usage {
            BufferUse::Gpu => MemoryLocation::GpuOnly,
            BufferUse::Upload => MemoryLocation::CpuToGpu,
            BufferUse::Readback => MemoryLocation::GpuToCpu,
        };
        self.create_buffer_in(size, location)
    }

    /// Create a sampler state.
    pub fn create_sampler(&self, desc: &GpuSamplerDesc) -> GpuSampler {
        let anisotropy = desc.max_anisotropy > 1 && self.core.sampler_anisotropy;
        let mut info = vk::SamplerCreateInfo::default()
            .min_filter(to_vk_filter(desc.min_filter))
            .mag_filter(to_vk_filter(desc.mag_filter))
            .mipmap_mode(match desc.mip_filter {
                GpuFilterMode::Nearest => vk::SamplerMipmapMode::NEAREST,
                GpuFilterMode::Linear => vk::SamplerMipmapMode::LINEAR,
            })
            .address_mode_u(to_vk_address(desc.address_mode_u))
            .address_mode_v(to_vk_address(desc.address_mode_v))
            .address_mode_w(to_vk_address(desc.address_mode_w))
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK)
            .anisotropy_enable(anisotropy)
            .max_anisotropy(
                (desc.max_anisotropy as f32).min(self.core.limits.max_sampler_anisotropy),
            )
            .min_lod(0.0)
            .max_lod(desc.lod_max_clamp.min(vk::LOD_CLAMP_NONE));
        if let Some(compare) = desc.compare {
            info = info.compare_enable(true).compare_op(to_vk_compare(compare));
        }
        let raw = unsafe { self.core.device.create_sampler(&info, None) }
            .unwrap_or_else(|e| panic!("Vulkan: sampler creation failed: {e}"));
        GpuSampler {
            inner: Arc::new(SamplerCore::new(self.core.clone(), raw)),
        }
    }

    /// Upload pixel data to a texture synchronously (CPU → GPU).
    pub fn upload_texture(&self, texture: &GpuTexture, data: &[u8]) {
        let view = &texture.view;
        assert!(
            view.image.usage.contains(GpuTextureUsage::CPU_UPLOAD),
            "texture CPU upload requires CPU_UPLOAD usage (CPU-visible storage)"
        );
        let bytes = texture.width as usize
            * texture.height as usize
            * texture.format.bytes_per_pixel() as usize;
        assert!(
            data.len() >= bytes,
            "upload_texture: {} bytes for a {}x{} {:?} texture",
            data.len(),
            texture.width,
            texture.height,
            texture.format
        );
        let staging = self.create_buffer(bytes as u64, BufferUse::Upload);
        // Safety: the staging buffer is new and `bytes` long.
        unsafe { staging.write(0, &data[..bytes]) };
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: view.image.aspect,
                mip_level: view.base_mip,
                base_array_layer: view.base_layer,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: texture.width,
                height: texture.height,
                depth: 1,
            })];
        self.core.submit_now(&self.queue, |cmd| unsafe {
            self.core.device.cmd_copy_buffer_to_image(
                cmd,
                staging.inner.raw,
                view.image.raw,
                vk::ImageLayout::GENERAL,
                &region,
            )
        });
    }

    /// Number of distinct compute pipelines currently cached (keyed by
    /// shader hash, shared across every caller).
    pub fn compute_pipeline_cache_len(&self) -> usize {
        self.compute_cache
            .lock()
            .expect("compute cache poisoned")
            .len()
    }

    /// Sibling of [`Self::compute_pipeline_cache_len`] for render pipelines
    /// (e.g.
    pub fn render_pipeline_cache_len(&self) -> usize {
        self.render_cache
            .lock()
            .expect("render cache poisoned")
            .len()
    }

    /// Create a compute pipeline from WGSL source.
    pub fn create_compute_pipeline(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
    ) -> GpuComputePipeline {
        let hash = compute_hash(wgsl_source, entry_point);
        if let Some(cached) = self
            .compute_cache
            .lock()
            .expect("compute cache poisoned")
            .get(&hash)
        {
            return cached.clone();
        }
        record_cold_touch(ColdTouchKind::PipelineCompile);
        let cached = self
            .spirv_cache
            .lock()
            .expect("SPIR-V cache poisoned")
            .as_ref()
            .and_then(|cache| cache.get(hash));
        let (slot_map, spirv, workgroup_size) = cached.unwrap_or_else(|| {
            let compiled = super::shader_compiler::compile_wgsl_to_spirv_compute(
                wgsl_source,
                entry_point,
                label,
            );
            if let Some(cache) = self
                .spirv_cache
                .lock()
                .expect("SPIR-V cache poisoned")
                .as_ref()
            {
                cache.put(hash, &compiled.0, &compiled.1, compiled.2);
            }
            compiled
        });
        let disk = self.pipeline_cache.lock().expect("pipeline cache poisoned");
        let inner = create_compute(
            &self.core,
            disk.as_ref().map_or(vk::PipelineCache::null(), |c| c.raw),
            &slot_map,
            &spirv,
            entry_point,
            label,
        );
        drop(disk);
        let pipeline = GpuComputePipeline {
            slot_map,
            label: label.to_string(),
            workgroup_size,
            inner,
        };
        self.compute_cache
            .lock()
            .expect("compute cache poisoned")
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

    /// Load or create a pipeline binary archive at the given path.
    pub fn load_pipeline_archive(&self, path: &std::path::Path) {
        *self.pipeline_cache.lock().expect("pipeline cache poisoned") =
            Some(DiskPipelineCache::load(&self.core, path));
    }

    /// Save the pipeline binary archive to disk (if loaded and modified).
    pub fn save_pipeline_archive(&self) {
        if let Some(cache) = self
            .pipeline_cache
            .lock()
            .expect("pipeline cache poisoned")
            .as_ref()
        {
            cache.save(&self.core);
        }
    }

    /// Load or create an MSL shader cache at the given directory.
    pub fn load_msl_cache(&self, cache_dir: &std::path::Path) {
        *self.spirv_cache.lock().expect("SPIR-V cache poisoned") = Some(SpirvCache::new(cache_dir));
    }

    /// Log MSL cache hit/miss statistics.
    pub fn log_msl_cache_stats(&self) {
        if let Some(cache) = self
            .spirv_cache
            .lock()
            .expect("SPIR-V cache poisoned")
            .as_ref()
        {
            cache.log_stats();
        }
    }

    /// Create (or fetch from the cache) a render pipeline.
    pub fn create_render_pipeline(&self, desc: &RenderPipelineDesc) -> GpuRenderPipeline {
        let mut source = desc.wgsl_source.to_string();
        for &(pattern, replacement) in desc.specializations {
            source = source.replace(pattern, replacement);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        "vk-render-v1".hash(&mut hasher);
        source.hash(&mut hasher);
        desc.vs_entry.hash(&mut hasher);
        desc.fs_entry.hash(&mut hasher);
        desc.point_size_location.hash(&mut hasher);
        let shader = hasher.finish();
        desc.color_format.hash(&mut hasher);
        desc.blend.hash(&mut hasher);
        desc.aux_color_formats.hash(&mut hasher);
        desc.depth_format.hash(&mut hasher);
        desc.sample_count.max(1).hash(&mut hasher);
        desc.alpha_to_coverage.hash(&mut hasher);
        desc.vertex_layout.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(cached) = self
            .render_cache
            .lock()
            .expect("render cache poisoned")
            .get(&key)
        {
            return cached.clone();
        }
        record_cold_touch(ColdTouchKind::PipelineCompile);
        let cached = self
            .spirv_cache
            .lock()
            .expect("SPIR-V cache poisoned")
            .as_ref()
            .and_then(|cache| cache.get(shader));
        let (slot_map, spirv) = match cached {
            Some((slot_map, spirv, _)) => (slot_map, spirv),
            None => {
                let (slot_map, spirv) = super::shader_compiler::compile_wgsl_to_spirv_render(
                    &source,
                    desc.vs_entry,
                    desc.fs_entry,
                    desc.point_size_location,
                    desc.label,
                );
                if let Some(cache) = self
                    .spirv_cache
                    .lock()
                    .expect("SPIR-V cache poisoned")
                    .as_ref()
                {
                    cache.put(shader, &slot_map, &spirv, [0; 3]);
                }
                (slot_map, spirv)
            }
        };
        let disk = self.pipeline_cache.lock().expect("pipeline cache poisoned");
        let inner = create_render(
            &self.core,
            disk.as_ref().map_or(vk::PipelineCache::null(), |c| c.raw),
            &slot_map,
            &spirv,
            desc,
        );
        drop(disk);
        let pipeline = GpuRenderPipeline {
            slot_map,
            label: desc.label.to_string(),
            sample_count: desc.sample_count.max(1),
            inner,
        };
        self.render_cache
            .lock()
            .expect("render cache poisoned")
            .insert(key, pipeline.clone());
        pipeline
    }

    /// Prepare device-global pipelines for texture clears and depth-to-float
    /// conversion before encoding work. Clears record vkCmdClearColorImage,
    /// so only depth-to-float compiles.
    pub fn prepare_utility_pipelines(&self) {
        self.depth_to_float();
    }

    fn depth_to_float(&self) -> &Arc<GpuComputePipeline> {
        self.depth_to_float.get_or_init(|| {
            Arc::new(self.create_compute_pipeline(
                crate::shader_common::DEPTH_TO_FLOAT_WGSL,
                "cs_main",
                "Depth to Float",
            ))
        })
    }

    /// Create a new command encoder for one frame's GPU work.
    pub fn create_encoder(&self, label: &str) -> GpuEncoder {
        GpuEncoder::new(
            self.core.clone(),
            self.queue.clone(),
            label,
            self.depth_to_float().clone(),
        )
    }

    /// Create a compiled depth-stencil state object.
    pub fn create_depth_stencil_state(&self, desc: &GpuDepthStencilDesc) -> GpuDepthStencilState {
        GpuDepthStencilState {
            compare: compare_op(desc.compare),
            write: desc.write_enabled,
            clear_depth: match desc.compare {
                GpuCompareFunction::Greater | GpuCompareFunction::GreaterEqual => 0.0,
                _ => 1.0,
            },
        }
    }

    /// Create a shared event for CPU↔GPU synchronization.
    pub fn create_event(&self) -> GpuEvent {
        GpuEvent {
            inner: Arc::new(EventCore::new(self.core.clone())),
        }
    }

    /// Attach fence-stamped drop retirement to this device (BUG-l7t4 class
    /// fix).
    pub fn set_retirement(&self, mark: Arc<crate::RetireMark>) {
        if self.retirement.set(mark).is_err() {
            log::warn!("GpuDevice::set_retirement called twice — keeping the first mark");
        }
    }

    /// The content frame-completion clock, when retirement is wired.
    pub fn frame_clock(&self) -> Option<crate::FrameClock> {
        self.retirement.get().map(|mark| mark.frame_clock())
    }

    /// Create a multisample texture for MSAA render passes. Discrete GPUs
    /// have no memoryless storage, so it gets memory like any other.
    pub fn create_texture_msaa_memoryless(
        &self,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        sample_count: u32,
        label: &str,
    ) -> GpuTexture {
        self.create_texture_msaa(width, height, format, sample_count, label)
    }

    /// Create a multisample render target for MSAA render passes.
    pub fn create_texture_msaa(
        &self,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        sample_count: u32,
        label: &str,
    ) -> GpuTexture {
        let desc = GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET,
            label,
            mip_levels: 1,
        };
        create_texture_on(
            &self.core,
            &self.queue,
            &desc,
            sample_count,
            self.retirement_mark(),
        )
        .unwrap_or_else(|e| panic!("{label}: MSAA texture allocation failed — {e}"))
    }
}
