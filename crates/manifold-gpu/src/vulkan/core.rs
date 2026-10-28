//! The process's one Vulkan instance and device (D16), shared by every
//! `GpuDevice` wrapper: device choice (D21), required features (D1), queues
//! (D5), the memory allocator, debug names (D20), the validation counter and
//! the memory budget (D18).
//!
//! `MANIFOLD_VULKAN_DEVICE` picks a device by a substring of its device or
//! driver name. `MANIFOLD_VULKAN_VALIDATION=1` turns on the Khronos
//! validation layer, `=sync` adds synchronization validation; every error it
//! reports is logged and counted ([`validation_error_count`]).

use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};

use super::submit::{FrameResources, Queue};
use crate::GpuTextureFormat;

/// Queues asked for per wrapper role: content, UI and the async lane (D5).
const WANTED_QUEUES: u32 = 3;

static VALIDATION_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Validation-layer errors reported in this process so far.
#[cfg(test)]
pub(crate) fn validation_error_count() -> u64 {
    VALIDATION_ERRORS.load(Ordering::Relaxed)
}

/// Records and runs a command buffer right away and waits for it: image
/// initialisation and device-level uploads.
struct OneShot {
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
}

struct Debug {
    instance: ash::ext::debug_utils::Instance,
    device: ash::ext::debug_utils::Device,
    messenger: vk::DebugUtilsMessengerEXT,
}

pub(crate) struct Core {
    _entry: ash::Entry,
    pub(crate) instance: ash::Instance,
    pub(crate) physical: vk::PhysicalDevice,
    pub(crate) device: ash::Device,
    pub(crate) limits: vk::PhysicalDeviceLimits,
    pub(crate) name: String,
    pub(crate) queue_family: u32,
    queues: Vec<Arc<Queue>>,
    /// Recycled per-submit resources, shared by every queue (D6).
    pub(crate) frames: Arc<Mutex<Vec<FrameResources>>>,
    /// Nanoseconds per timestamp tick and the mask of valid bits; `None`
    /// when the queue family writes no timestamps.
    pub(crate) timestamps: Option<(f64, u64)>,
    next_queue: AtomicUsize,
    pub(crate) allocator: ManuallyDrop<Mutex<Allocator>>,
    one_shot: Mutex<OneShot>,
    debug: Option<Debug>,
    memory_budget: bool,
    pub(crate) sampler_anisotropy: bool,
    /// Optimal-tiling features per [`GpuTextureFormat`], in `ALL_FORMATS` order.
    format_features: Vec<(GpuTextureFormat, vk::FormatFeatureFlags)>,
}

// Safety: every Vulkan handle here is externally synchronised by the mutex
// that guards it (queues, the one-shot pool, the allocator); the rest are
// immutable after creation.
unsafe impl Send for Core {}
unsafe impl Sync for Core {}

pub(crate) const ALL_FORMATS: [GpuTextureFormat; 13] = [
    GpuTextureFormat::Rgba16Float,
    GpuTextureFormat::Rgba32Float,
    GpuTextureFormat::Rgba8Unorm,
    GpuTextureFormat::R32Float,
    GpuTextureFormat::Rg32Float,
    GpuTextureFormat::R16Float,
    GpuTextureFormat::Rg16Float,
    GpuTextureFormat::R32Uint,
    GpuTextureFormat::Rgba8UnormSrgb,
    GpuTextureFormat::Bgra8Unorm,
    GpuTextureFormat::Bgra8UnormSrgb,
    GpuTextureFormat::R8Unorm,
    GpuTextureFormat::Depth32Float,
];

pub(crate) fn vk_format(format: GpuTextureFormat) -> vk::Format {
    match format {
        GpuTextureFormat::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        GpuTextureFormat::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        GpuTextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        GpuTextureFormat::R32Float => vk::Format::R32_SFLOAT,
        GpuTextureFormat::Rg32Float => vk::Format::R32G32_SFLOAT,
        GpuTextureFormat::R16Float => vk::Format::R16_SFLOAT,
        GpuTextureFormat::Rg16Float => vk::Format::R16G16_SFLOAT,
        GpuTextureFormat::R32Uint => vk::Format::R32_UINT,
        GpuTextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        GpuTextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        GpuTextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        GpuTextureFormat::R8Unorm => vk::Format::R8_UNORM,
        GpuTextureFormat::Depth32Float => vk::Format::D32_SFLOAT,
    }
}

unsafe extern "system" fn on_validation_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    // Safety: the layer passes a valid callback struct for the call's duration.
    let message = unsafe { data.as_ref() }
        .and_then(|d| unsafe { d.message_as_c_str() })
        .map_or_else(|| "(no message)".into(), CStr::to_string_lossy);
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        VALIDATION_ERRORS.fetch_add(1, Ordering::Relaxed);
        // Validation is opt-in, so print even where no logger is installed.
        eprintln!("[Vulkan] {message}");
        log::error!("[Vulkan] {message}");
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        log::warn!("[Vulkan] {message}");
    }
    vk::FALSE
}

/// What a physical device offers, read once while choosing.
struct Candidate {
    physical: vk::PhysicalDevice,
    properties: vk::PhysicalDeviceProperties,
    name: String,
    driver: String,
    family: u32,
    family_queues: u32,
    timestamp_bits: u32,
    extensions: Vec<CString>,
    missing: Vec<&'static str>,
    optional: vk::PhysicalDeviceFeatures,
    uniform_standard_layout: bool,
    scalar_block_layout: bool,
}

impl Candidate {
    fn has_extension(&self, name: &CStr) -> bool {
        self.extensions.iter().any(|e| e.as_c_str() == name)
    }

    /// Discrete beats integrated; a conformant driver beats a portability
    /// one (KosmicKrisp over MoltenVK on the Mac).
    fn rank(&self) -> (u32, bool) {
        let kind = match self.properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
            _ => 0,
        };
        (
            kind,
            !self.has_extension(ash::khr::portability_subset::NAME),
        )
    }
}

impl Core {
    /// The process's device, created on first use. Panics with the reason
    /// when no device meets the floor (D1).
    pub(crate) fn shared() -> Arc<Core> {
        static CORE: OnceLock<Arc<Core>> = OnceLock::new();
        CORE.get_or_init(|| Arc::new(Core::create().unwrap_or_else(|e| panic!("Vulkan: {e}"))))
            .clone()
    }

    fn create() -> Result<Core, String> {
        // Safety: loading the system Vulkan loader.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("no Vulkan loader: {e}"))?;
        let instance_extensions: Vec<CString> =
            unsafe { entry.enumerate_instance_extension_properties(None) }
                .map_err(|e| format!("instance extensions: {e}"))?
                .iter()
                .filter_map(|e| e.extension_name_as_c_str().ok().map(CStr::to_owned))
                .collect();
        let has_instance_ext =
            |name: &CStr| instance_extensions.iter().any(|e| e.as_c_str() == name);

        let validation = std::env::var("MANIFOLD_VULKAN_VALIDATION").ok();
        let layers: Vec<&CStr> = if validation.is_some() {
            let available = unsafe { entry.enumerate_instance_layer_properties() }
                .map_err(|e| format!("instance layers: {e}"))?;
            if available
                .iter()
                .any(|l| l.layer_name_as_c_str().ok() == Some(c"VK_LAYER_KHRONOS_validation"))
            {
                vec![c"VK_LAYER_KHRONOS_validation"]
            } else {
                log::warn!(
                    "[Vulkan] MANIFOLD_VULKAN_VALIDATION set but the validation layer is not installed"
                );
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let mut extensions: Vec<&CStr> = Vec::new();
        let portability = has_instance_ext(ash::khr::portability_enumeration::NAME);
        if portability {
            extensions.push(ash::khr::portability_enumeration::NAME);
        }
        let debug_utils = has_instance_ext(ash::ext::debug_utils::NAME);
        if debug_utils {
            extensions.push(ash::ext::debug_utils::NAME);
        }
        let sync_validation = !layers.is_empty() && validation.as_deref() == Some("sync");
        if sync_validation {
            extensions.push(ash::ext::layer_settings::NAME);
        }
        let extension_ptrs: Vec<*const c_char> = extensions.iter().map(|e| e.as_ptr()).collect();
        let layer_ptrs: Vec<*const c_char> = layers.iter().map(|l| l.as_ptr()).collect();

        let app = vk::ApplicationInfo::default()
            .application_name(c"MANIFOLD")
            .engine_name(c"manifold-gpu")
            .api_version(vk::API_VERSION_1_3);
        let true_value: vk::Bool32 = vk::TRUE;
        let mut sync_setting = vk::LayerSettingEXT::default()
            .layer_name(c"VK_LAYER_KHRONOS_validation")
            .setting_name(c"validate_sync")
            .ty(vk::LayerSettingTypeEXT::BOOL32);
        // ash's `values` counts bytes; the layer wants one BOOL32.
        sync_setting.value_count = 1;
        sync_setting.p_values = (&true_value as *const vk::Bool32).cast();
        let settings = [sync_setting];
        let mut layer_settings = vk::LayerSettingsCreateInfoEXT::default().settings(&settings);
        let mut instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&extension_ptrs)
            .enabled_layer_names(&layer_ptrs);
        if portability {
            instance_info = instance_info.flags(vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR);
        }
        if sync_validation {
            instance_info = instance_info.push_next(&mut layer_settings);
        }
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|e| format!("create instance: {e}"))?;
        if !layers.is_empty() {
            log::info!(
                "[Vulkan] validation on{}",
                if sync_validation {
                    ", with synchronization validation"
                } else {
                    ""
                }
            );
        }

        let messenger = if debug_utils && !layers.is_empty() {
            let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(on_validation_message));
            let messenger = unsafe { loader.create_debug_utils_messenger(&info, None) }
                .map_err(|e| format!("debug messenger: {e}"))?;
            Some((loader, messenger))
        } else {
            None
        };

        let candidate = Self::choose_device(&instance)?;
        log::info!(
            "[Vulkan] device: {} ({}), Vulkan {}.{}",
            candidate.name,
            candidate.driver,
            vk::api_version_major(candidate.properties.api_version),
            vk::api_version_minor(candidate.properties.api_version),
        );

        let queue_count = candidate.family_queues.min(WANTED_QUEUES);
        let priorities = vec![1.0f32; queue_count as usize];
        let queue_infos = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(candidate.family)
            .queue_priorities(&priorities)];
        let mut device_extensions: Vec<&CStr> = Vec::new();
        if candidate.has_extension(ash::khr::portability_subset::NAME) {
            device_extensions.push(ash::khr::portability_subset::NAME);
        }
        let memory_budget = candidate.has_extension(ash::ext::memory_budget::NAME);
        if memory_budget {
            device_extensions.push(ash::ext::memory_budget::NAME);
        }
        let device_extension_ptrs: Vec<*const c_char> =
            device_extensions.iter().map(|e| e.as_ptr()).collect();

        let optional = candidate.optional;
        let features = vk::PhysicalDeviceFeatures::default()
            .robust_buffer_access(true)
            .fill_mode_non_solid(optional.fill_mode_non_solid == vk::TRUE)
            .sampler_anisotropy(optional.sampler_anisotropy == vk::TRUE)
            .independent_blend(optional.independent_blend == vk::TRUE)
            .image_cube_array(optional.image_cube_array == vk::TRUE)
            .fragment_stores_and_atomics(optional.fragment_stores_and_atomics == vk::TRUE)
            .vertex_pipeline_stores_and_atomics(
                optional.vertex_pipeline_stores_and_atomics == vk::TRUE,
            )
            .shader_storage_image_extended_formats(
                optional.shader_storage_image_extended_formats == vk::TRUE,
            )
            .shader_int16(optional.shader_int16 == vk::TRUE)
            .shader_int64(optional.shader_int64 == vk::TRUE)
            .depth_clamp(optional.depth_clamp == vk::TRUE)
            .large_points(optional.large_points == vk::TRUE);
        let mut features12 = vk::PhysicalDeviceVulkan12Features::default()
            .timeline_semaphore(true)
            .uniform_buffer_standard_layout(candidate.uniform_standard_layout)
            .scalar_block_layout(candidate.scalar_block_layout);
        let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
            .dynamic_rendering(true)
            .synchronization2(true);
        let mut features2 = vk::PhysicalDeviceFeatures2::default()
            .features(features)
            .push_next(&mut features12)
            .push_next(&mut features13);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&device_extension_ptrs)
            .push_next(&mut features2);
        let device = unsafe { instance.create_device(candidate.physical, &device_info, None) }
            .map_err(|e| format!("create device: {e}"))?;

        let frames = Arc::new(Mutex::new(Vec::new()));
        let queues = (0..queue_count)
            .map(|index| {
                let raw = unsafe { device.get_device_queue(candidate.family, index) };
                Arc::new(Queue::new(&device, raw, frames.clone(), index))
            })
            .collect();
        let timestamps = (candidate.timestamp_bits > 0).then(|| {
            let mask = if candidate.timestamp_bits >= 64 {
                u64::MAX
            } else {
                (1u64 << candidate.timestamp_bits) - 1
            };
            (
                f64::from(candidate.properties.limits.timestamp_period),
                mask,
            )
        });

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: candidate.physical,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(|e| format!("allocator: {e}"))?;

        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(candidate.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }
        .map_err(|e| format!("command pool: {e}"))?;
        let cmd = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .map_err(|e| format!("command buffer: {e}"))?[0];
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }
            .map_err(|e| format!("fence: {e}"))?;

        let format_features = ALL_FORMATS
            .iter()
            .map(|&format| {
                let props = unsafe {
                    instance.get_physical_device_format_properties(
                        candidate.physical,
                        vk_format(format),
                    )
                };
                (format, props.optimal_tiling_features)
            })
            .collect();

        let debug = messenger.map(|(loader, messenger)| Debug {
            device: ash::ext::debug_utils::Device::new(&instance, &device),
            instance: loader,
            messenger,
        });
        let debug = match debug {
            Some(d) => Some(d),
            None if debug_utils => {
                // Names without the validation layer still help RenderDoc.
                let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
                Some(Debug {
                    device: ash::ext::debug_utils::Device::new(&instance, &device),
                    instance: loader,
                    messenger: vk::DebugUtilsMessengerEXT::null(),
                })
            }
            None => None,
        };

        Ok(Core {
            _entry: entry,
            instance,
            physical: candidate.physical,
            device,
            limits: candidate.properties.limits,
            name: candidate.name,
            queue_family: candidate.family,
            queues,
            frames,
            timestamps,
            next_queue: AtomicUsize::new(0),
            allocator: ManuallyDrop::new(Mutex::new(allocator)),
            one_shot: Mutex::new(OneShot { pool, cmd, fence }),
            debug,
            memory_budget,
            sampler_anisotropy: optional.sampler_anisotropy == vk::TRUE,
            format_features,
        })
    }

    fn choose_device(instance: &ash::Instance) -> Result<Candidate, String> {
        let physicals = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate devices: {e}"))?;
        let mut candidates: Vec<Candidate> = physicals
            .into_iter()
            .map(|physical| Self::inspect(instance, physical))
            .collect();
        if let Ok(wanted) = std::env::var("MANIFOLD_VULKAN_DEVICE") {
            let wanted = wanted.to_lowercase();
            candidates.retain(|c| {
                c.name.to_lowercase().contains(&wanted) || c.driver.to_lowercase().contains(&wanted)
            });
            if candidates.is_empty() {
                return Err(format!("no device matches MANIFOLD_VULKAN_DEVICE={wanted}"));
            }
        }
        let rejected: Vec<String> = candidates
            .iter()
            .filter(|c| !c.missing.is_empty())
            .map(|c| format!("{} ({}) lacks {}", c.name, c.driver, c.missing.join(", ")))
            .collect();
        candidates
            .into_iter()
            .filter(|c| c.missing.is_empty())
            .max_by_key(Candidate::rank)
            .ok_or_else(|| {
                if rejected.is_empty() {
                    "no Vulkan device found".to_string()
                } else {
                    format!(
                        "no device meets the Vulkan 1.3 floor: {}",
                        rejected.join("; ")
                    )
                }
            })
    }

    fn inspect(instance: &ash::Instance, physical: vk::PhysicalDevice) -> Candidate {
        let properties = unsafe { instance.get_physical_device_properties(physical) };
        let mut driver_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver_props);
        unsafe { instance.get_physical_device_properties2(physical, &mut props2) };
        let name = properties
            .device_name_as_c_str()
            .map_or_else(|_| "unknown".into(), |n| n.to_string_lossy().into_owned());
        let driver = driver_props
            .driver_name_as_c_str()
            .map_or_else(|_| "unknown".into(), |n| n.to_string_lossy().into_owned());

        let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut f12)
            .push_next(&mut f13);
        unsafe { instance.get_physical_device_features2(physical, &mut f2) };
        let optional = f2.features;
        let mut missing = Vec::new();
        if properties.api_version < vk::API_VERSION_1_3 {
            missing.push("Vulkan 1.3");
        }
        if optional.robust_buffer_access != vk::TRUE {
            missing.push("robustBufferAccess");
        }
        if f12.timeline_semaphore != vk::TRUE {
            missing.push("timelineSemaphore");
        }
        if f13.dynamic_rendering != vk::TRUE {
            missing.push("dynamicRendering");
        }
        if f13.synchronization2 != vk::TRUE {
            missing.push("synchronization2");
        }
        let uniform_standard_layout = f12.uniform_buffer_standard_layout == vk::TRUE;
        let scalar_block_layout = f12.scalar_block_layout == vk::TRUE;

        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let wanted = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE;
        let family = families
            .iter()
            .position(|f| f.queue_flags.contains(wanted) && f.queue_count > 0);
        if family.is_none() {
            missing.push("a graphics and compute queue");
        }
        let family = family.unwrap_or(0);
        let family_queues = families.get(family).map_or(0, |f| f.queue_count);
        let timestamp_bits = families.get(family).map_or(0, |f| f.timestamp_valid_bits);

        let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e.extension_name_as_c_str().ok().map(CStr::to_owned))
            .collect();

        Candidate {
            physical,
            properties,
            name,
            driver,
            family: family as u32,
            family_queues,
            timestamp_bits,
            extensions,
            missing,
            optional,
            uniform_standard_layout,
            scalar_block_layout,
        }
    }

    /// The next queue for a new wrapper, round robin: separate queues while
    /// the family has them, shared ones after (D5).
    pub(crate) fn take_queue(&self) -> Arc<Queue> {
        let index = self.next_queue.fetch_add(1, Ordering::Relaxed) % self.queues.len();
        self.queues[index].clone()
    }

    /// Name a handle for validation messages and captures (D20).
    pub(crate) fn name<H: vk::Handle>(&self, handle: H, label: &str) {
        let Some(debug) = &self.debug else {
            return;
        };
        let Ok(label) = CString::new(label) else {
            return;
        };
        let info = vk::DebugUtilsObjectNameInfoEXT::default()
            .object_handle(handle)
            .object_name(&label);
        // A failed name only costs a label in a capture.
        let _ = unsafe { debug.device.set_debug_utils_object_name(&info) };
    }

    /// The optimal-tiling features the device offers for `format`.
    pub(crate) fn format_features(&self, format: GpuTextureFormat) -> vk::FormatFeatureFlags {
        self.format_features
            .iter()
            .find(|(f, _)| *f == format)
            .map_or(vk::FormatFeatureFlags::empty(), |(_, flags)| *flags)
    }

    /// Device-local usage and budget from `VK_EXT_memory_budget` (D18).
    pub(crate) fn memory_snapshot(&self) -> Option<crate::GpuMemorySnapshot> {
        if !self.memory_budget {
            return None;
        }
        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let heaps = {
            let mut props = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
            unsafe {
                self.instance
                    .get_physical_device_memory_properties2(self.physical, &mut props)
            };
            props.memory_properties
        };
        let local = (0..heaps.memory_heap_count as usize).filter(|&i| {
            heaps.memory_heaps[i]
                .flags
                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
        });
        let (used, limit) = local.fold((0, 0), |(used, limit), i| {
            (used + budget.heap_usage[i], limit + budget.heap_budget[i])
        });
        Some(crate::GpuMemorySnapshot {
            current_allocated_bytes: used,
            recommended_max_working_set_bytes: limit,
        })
    }

    /// Record `record` into a command buffer, submit it on `queue` and wait
    /// for it.
    pub(crate) fn submit_now(&self, queue: &Queue, record: impl FnOnce(vk::CommandBuffer)) {
        let one_shot = self.one_shot.lock().expect("one-shot pool poisoned");
        unsafe {
            self.device
                .reset_command_pool(one_shot.pool, vk::CommandPoolResetFlags::empty())
                .expect("Vulkan: reset one-shot pool");
            self.device
                .begin_command_buffer(
                    one_shot.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .expect("Vulkan: begin one-shot");
        }
        self.global_barrier(one_shot.cmd);
        record(one_shot.cmd);
        self.global_barrier(one_shot.cmd);
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(one_shot.cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        unsafe {
            self.device
                .end_command_buffer(one_shot.cmd)
                .expect("Vulkan: end one-shot");
            {
                let state = queue.state.lock().expect("queue poisoned");
                self.device
                    .queue_submit2(state.raw, &submit, one_shot.fence)
                    .expect("Vulkan: submit one-shot");
            }
            self.device
                .wait_for_fences(&[one_shot.fence], true, u64::MAX)
                .expect("Vulkan: wait one-shot");
            self.device
                .reset_fences(&[one_shot.fence])
                .expect("Vulkan: reset one-shot fence");
        }
    }
}

impl Core {
    /// All earlier writes, on this queue, before all later reads and writes:
    /// how every command buffer starts (design section 5).
    pub(crate) fn global_barrier(&self, cmd: vk::CommandBuffer) {
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)];
        unsafe {
            self.device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&barrier),
            )
        };
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            let one_shot = self.one_shot.get_mut().expect("one-shot pool poisoned");
            self.device.destroy_fence(one_shot.fence, None);
            self.device.destroy_command_pool(one_shot.pool, None);
            ManuallyDrop::drop(&mut self.allocator);
            self.device.destroy_device(None);
            if let Some(debug) = &self.debug
                && debug.messenger != vk::DebugUtilsMessengerEXT::null()
            {
                debug
                    .instance
                    .destroy_debug_utils_messenger(debug.messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}
