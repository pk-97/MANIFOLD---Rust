//! Presentation to a window (D15). A surface owns a swapchain sized to the
//! window and one ordinary `GENERAL` target per swapchain image, sized to
//! the drawable size the caller asks for. Callers draw into a drawable's
//! target like any texture; `present_drawable` blits it into the acquired
//! swapchain image (scaling, letterboxing and converting format: a linear
//! float target lands in an sRGB swapchain encoded) and presents it after
//! the submit. Swapchain images never take part in hazard tracking, so they
//! need no layout bookkeeping beyond that one blit. SDR only.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ash::vk;
use ash::vk::{AccessFlags2 as A, PipelineStageFlags2 as S};

use super::core::Core;
use super::submit::{Keep, Queue};
use super::*;
use crate::*;

/// A `VkSurfaceKHR`, destroyed after the surface and every swapchain on it.
struct SurfaceCore {
    core: Arc<Core>,
    raw: vk::SurfaceKHR,
}

impl Drop for SurfaceCore {
    fn drop(&mut self) {
        if let Some(present) = &self.core.present
            && self.raw != vk::SurfaceKHR::null()
        {
            unsafe { present.surface.destroy_surface(self.raw, None) };
        }
    }
}

/// A presentable window surface.
pub struct GpuSurface {
    core: Arc<Core>,
    queue: Arc<Queue>,
    surface: Arc<SurfaceCore>,
    /// The drawable size: every target's size.
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
    vsync: bool,
    max_drawables: AtomicU32,
    /// Fit the target inside the window keeping its aspect, or stretch it.
    aspect_fit: AtomicBool,
    background: Mutex<[f32; 4]>,
    swapchain: Mutex<Option<Arc<Swapchain>>>,
}

/// A swapchain image acquired for one frame, and the target drawn for it.
pub struct GpuDrawable {
    pub(crate) swapchain: Arc<Swapchain>,
    pub(crate) index: u32,
    /// Signalled when the presentation engine releases the image.
    pub(crate) acquired: vk::Semaphore,
}

pub(crate) struct Swapchain {
    core: Arc<Core>,
    queue: Arc<Queue>,
    _surface: Arc<SurfaceCore>,
    pub(crate) raw: vk::SwapchainKHR,
    pub(crate) images: Vec<vk::Image>,
    pub(crate) extent: vk::Extent2D,
    pub(crate) targets: Vec<GpuTexture>,
    /// Per image: signalled by the submit that blits into it, waited by its
    /// present.
    pub(crate) rendered: Vec<vk::Semaphore>,
    /// One more than the images, used in turn for acquires.
    acquire: Vec<vk::Semaphore>,
    next_acquire: AtomicUsize,
    pub(crate) aspect_fit: bool,
    pub(crate) background: [f32; 4],
    /// Set when a present reports the window changed; the next acquire
    /// rebuilds.
    pub(crate) stale: AtomicBool,
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        let device = &self.core.device;
        // Presents go on this queue and may still wait on these semaphores;
        // the queue's lock keeps other submits out while it drains.
        let state = self.queue.state.lock().expect("queue poisoned");
        unsafe {
            let _ = device.queue_wait_idle(state.raw);
            for &semaphore in self.rendered.iter().chain(&self.acquire) {
                device.destroy_semaphore(semaphore, None);
            }
        }
        drop(state);
        if let Some(present) = &self.core.present {
            unsafe { present.swapchain.destroy_swapchain(self.raw, None) };
        }
    }
}

/// Receives `(drawableID, presentedTime)`. Vulkan reports no presented time
/// yet, so the observer never runs there.
static PRESENTED_OBSERVER: OnceLock<fn(u64, f64)> = OnceLock::new();

/// Install the one presented-time observer. Returns false if one is
/// already installed.
pub fn set_presented_observer(observer: fn(u64, f64)) -> bool {
    PRESENTED_OBSERVER.set(observer).is_ok()
}

/// Log once that a Metal layer setting has no Vulkan counterpart.
fn unsupported(what: &str) {
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut logged = LOGGED.lock().expect("surface log poisoned");
    if !logged.iter().any(|w| w == what) {
        log::info!("[Vulkan] surface: {what} is not supported");
        logged.push(what.to_string());
    }
}

fn semaphore(core: &Core) -> vk::Semaphore {
    unsafe {
        core.device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
    }
    .expect("Vulkan: create semaphore")
}

impl GpuDevice {
    /// Create a presentable surface on `window`, drawing `width × height`
    /// targets in `format`. `vsync` false prefers MAILBOX, which never
    /// blocks an acquire on the display; FIFO otherwise and when MAILBOX is
    /// missing.
    pub fn create_surface(
        &self,
        window: &(impl raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle),
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        vsync: bool,
    ) -> GpuSurface {
        let core = self.core.clone();
        let raw = match (
            &core.present,
            window.display_handle(),
            window.window_handle(),
        ) {
            (Some(_), Ok(display), Ok(handle)) => unsafe {
                ash_window::create_surface(
                    &core.entry,
                    &core.instance,
                    display.as_raw(),
                    handle.as_raw(),
                    None,
                )
            }
            .map_err(|e| log::error!("[Vulkan] create surface: {e}"))
            .unwrap_or(vk::SurfaceKHR::null()),
            _ => {
                log::error!("[Vulkan] this window cannot present");
                vk::SurfaceKHR::null()
            }
        };
        GpuSurface {
            surface: Arc::new(SurfaceCore {
                core: core.clone(),
                raw,
            }),
            core,
            queue: self.queue.clone(),
            width,
            height,
            format,
            vsync,
            max_drawables: AtomicU32::new(3),
            aspect_fit: AtomicBool::new(false),
            background: Mutex::new([0.0, 0.0, 0.0, 1.0]),
            swapchain: Mutex::new(None),
        }
    }
}

impl GpuSurface {
    /// Resize the drawable surface: new targets at the next acquire.
    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.mark_stale();
        }
    }

    /// Set the number of swapchain images to ask for (2 or 3).
    pub fn set_maximum_drawable_count(&self, count: u32) {
        if self.max_drawables.swap(count, Ordering::Relaxed) != count {
            self.mark_stale();
        }
    }

    fn mark_stale(&self) {
        if let Some(chain) = self.swapchain.lock().expect("surface poisoned").as_ref() {
            chain.stale.store(true, Ordering::Relaxed);
        }
    }

    /// Acquire the next drawable; `None` when the window cannot present
    /// right now (minimised, or no image within a second).
    pub fn next_drawable(&self) -> Option<GpuDrawable> {
        let present = self.core.present.as_ref()?;
        if self.surface.raw == vk::SurfaceKHR::null() {
            return None;
        }
        let mut slot = self.swapchain.lock().expect("surface poisoned");
        for _ in 0..2 {
            if slot
                .as_ref()
                .is_none_or(|c| c.stale.load(Ordering::Relaxed))
            {
                // The replacement retires the old swapchain, which lives on
                // until the submits presenting from it finish.
                let old = slot.take();
                *slot = Some(Arc::new(self.build_swapchain(old.as_ref().map(|c| c.raw))?));
            }
            let chain = slot.as_ref()?.clone();
            let turn = chain.next_acquire.fetch_add(1, Ordering::Relaxed) % chain.acquire.len();
            let acquired = chain.acquire[turn];
            let result = unsafe {
                present.swapchain.acquire_next_image(
                    chain.raw,
                    1_000_000_000,
                    acquired,
                    vk::Fence::null(),
                )
            };
            match result {
                Ok((index, suboptimal)) => {
                    if suboptimal {
                        chain.stale.store(true, Ordering::Relaxed);
                    }
                    return Some(GpuDrawable {
                        swapchain: chain,
                        index,
                        acquired,
                    });
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    chain.stale.store(true, Ordering::Relaxed)
                }
                Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => return None,
                Err(e) => {
                    if e == vk::Result::ERROR_DEVICE_LOST {
                        super::submit::device_lost("acquire");
                    }
                    log::error!("[Vulkan] acquire: {e}");
                    return None;
                }
            }
        }
        None
    }

    fn build_swapchain(&self, old: Option<vk::SwapchainKHR>) -> Option<Swapchain> {
        let surface = self.surface.raw;
        let present = self.core.present.as_ref()?;
        let physical = self.core.physical;
        let supported = unsafe {
            present.surface.get_physical_device_surface_support(
                physical,
                self.core.queue_family,
                surface,
            )
        }
        .unwrap_or(false);
        if !supported {
            log::error!("[Vulkan] the queue family cannot present to this window");
            return None;
        }
        let caps = unsafe {
            present
                .surface
                .get_physical_device_surface_capabilities(physical, surface)
        }
        .ok()?;
        // A window reports its size; Wayland leaves it to the swapchain.
        let extent = if caps.current_extent.width == u32::MAX {
            vk::Extent2D {
                width: self
                    .width
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: self
                    .height
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        } else {
            caps.current_extent
        };
        if extent.width == 0 || extent.height == 0 {
            return None;
        }
        let formats = unsafe {
            present
                .surface
                .get_physical_device_surface_formats(physical, surface)
        }
        .ok()?;
        // Targets hold linear light unless they are 8-bit, which callers
        // fill already encoded; the blit encodes into an sRGB image.
        let linear = !matches!(
            self.format,
            GpuTextureFormat::Bgra8Unorm | GpuTextureFormat::Rgba8Unorm
        );
        let wanted: &[vk::Format] = if linear {
            &[vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB]
        } else {
            &[vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM]
        };
        let chosen = formats
            .iter()
            .find(|f| {
                wanted.contains(&f.format) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or(formats.first())
            .copied()?;
        let modes = unsafe {
            present
                .surface
                .get_physical_device_surface_present_modes(physical, surface)
        }
        .unwrap_or_default();
        let mode = if !self.vsync && modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else {
            vk::PresentModeKHR::FIFO
        };
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if !LOGGED.swap(true, Ordering::Relaxed) {
            log::info!(
                "[Vulkan] swapchain {:?} {:?}, {}x{}",
                chosen.format,
                mode,
                extent.width,
                extent.height
            );
        }
        let mut count = self
            .max_drawables
            .load(Ordering::Relaxed)
            .max(caps.min_image_count);
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .old_swapchain(old.unwrap_or_default())
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(mode)
            .clipped(true);
        let raw = unsafe { present.swapchain.create_swapchain(&info, None) }
            .map_err(|e| log::error!("[Vulkan] create swapchain: {e}"))
            .ok()?;
        let images = unsafe { present.swapchain.get_swapchain_images(raw) }.unwrap_or_default();
        let targets = images
            .iter()
            .map(|_| {
                super::device::create_texture_on(
                    &self.core,
                    &self.queue,
                    &GpuTextureDesc {
                        width: self.width.max(1),
                        height: self.height.max(1),
                        depth: 1,
                        format: self.format,
                        dimension: GpuTextureDimension::D2,
                        usage: GpuTextureUsage::RENDER_TARGET_FULL,
                        label: "Surface target",
                        mip_levels: 1,
                    },
                    1,
                    None,
                )
                .expect("Vulkan: surface target")
            })
            .collect();
        Some(Swapchain {
            core: self.core.clone(),
            queue: self.queue.clone(),
            _surface: self.surface.clone(),
            raw,
            rendered: images.iter().map(|_| semaphore(&self.core)).collect(),
            acquire: (0..=images.len()).map(|_| semaphore(&self.core)).collect(),
            images,
            extent,
            targets,
            next_acquire: AtomicUsize::new(0),
            aspect_fit: self.aspect_fit.load(Ordering::Relaxed),
            background: *self.background.lock().expect("surface poisoned"),
            stale: AtomicBool::new(false),
        })
    }

    /// Extended dynamic range: Vulkan presents SDR for now.
    pub fn configure_edr(&self) {
        unsupported("extended dynamic range");
    }

    /// Letterbox the drawable inside the window, keeping its aspect.
    pub fn set_contents_gravity_resize_aspect(&self) {
        self.aspect_fit.store(true, Ordering::Relaxed);
        self.mark_stale();
    }

    /// Core Animation transactions are a Mac concept.
    pub fn set_presents_with_transaction(&self, enabled: bool) {
        if enabled {
            unsupported("presenting with Core Animation transactions");
        }
    }

    /// The colour around a letterboxed drawable.
    pub fn set_background_color(&self, r: f64, g: f64, b: f64, a: f64) {
        *self.background.lock().expect("surface poisoned") =
            [r as f32, g as f32, b as f32, a as f32];
        self.mark_stale();
    }
}

impl GpuDrawable {
    /// The target this drawable shows, to draw into.
    pub fn gpu_texture(&self, format: GpuTextureFormat) -> GpuTexture {
        let target = &self.swapchain.targets[self.index as usize];
        assert_eq!(
            target.format, format,
            "drawable texture format must match its surface's"
        );
        target.clone()
    }

    /// Vulkan reports no presented time yet, so nothing is observed.
    pub fn observe_presented(&self) -> Option<u64> {
        None
    }
}

/// A present to issue right after the submit that blits it.
pub(crate) struct PendingPresent {
    pub(crate) swapchain: Arc<Swapchain>,
    pub(crate) index: u32,
}

impl super::encoder::GpuEncoder {
    /// Schedule a drawable for presentation when the command buffer
    /// completes: its own batch waits for the acquire, blits the target into
    /// the swapchain image and signals the present.
    pub fn present_drawable(&mut self, drawable: &GpuDrawable) {
        let chain = drawable.swapchain.clone();
        let index = drawable.index as usize;
        let target = &chain.targets[index];
        self.keep_for_present(target, Keep::Swapchain(chain.clone()));
        // Everything recorded so far is in earlier batches, which the new
        // batch's opening barrier orders before the blit; only the blit
        // waits for the presentation engine.
        let cmd = self.split_batch(vec![(drawable.acquired, 0)]);
        let image = chain.images[index];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let layout = |old, new, src: (S, A), dst: (S, A)| {
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(src.0)
                .src_access_mask(src.1)
                .dst_stage_mask(dst.0)
                .dst_access_mask(dst.1)
                .old_layout(old)
                .new_layout(new)
                .image(image)
                .subresource_range(range)
        };
        let device = &self.core().device;
        let to_dst = [layout(
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            (S::ALL_COMMANDS, A::NONE),
            (S::ALL_TRANSFER, A::TRANSFER_WRITE),
        )];
        unsafe {
            device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_dst),
            )
        };
        let (dst_w, dst_h) = (chain.extent.width as i32, chain.extent.height as i32);
        let (src_w, src_h) = (target.width as i32, target.height as i32);
        let (x0, y0, x1, y1) = if chain.aspect_fit {
            let scale = (dst_w as f32 / src_w as f32).min(dst_h as f32 / src_h as f32);
            let (w, h) = (
                (src_w as f32 * scale).round() as i32,
                (src_h as f32 * scale).round() as i32,
            );
            let (x, y) = ((dst_w - w) / 2, (dst_h - h) / 2);
            (x, y, x + w, y + h)
        } else {
            (0, 0, dst_w, dst_h)
        };
        let corner = |x, y, z| vk::Offset3D { x, y, z };
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        unsafe {
            if (x0, y0, x1, y1) != (0, 0, dst_w, dst_h) {
                let color = vk::ClearColorValue {
                    float32: chain.background,
                };
                device.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &color,
                    &[range],
                );
                let barrier = [vk::MemoryBarrier2::default()
                    .src_stage_mask(S::ALL_TRANSFER)
                    .src_access_mask(A::TRANSFER_WRITE)
                    .dst_stage_mask(S::ALL_TRANSFER)
                    .dst_access_mask(A::TRANSFER_WRITE)];
                device.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().memory_barriers(&barrier),
                );
            }
            device.cmd_blit_image(
                cmd,
                target.view.image.raw,
                vk::ImageLayout::GENERAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageBlit {
                    src_subresource: layers,
                    src_offsets: [corner(0, 0, 0), corner(src_w, src_h, 1)],
                    dst_subresource: layers,
                    dst_offsets: [corner(x0, y0, 0), corner(x1, y1, 1)],
                }],
                vk::Filter::LINEAR,
            );
        }
        let to_present = [layout(
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            (S::ALL_TRANSFER, A::TRANSFER_WRITE),
            (S::NONE, A::NONE),
        )];
        unsafe {
            device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_present),
            )
        };
        let rendered = chain.rendered[index];
        self.end_batch(vec![(rendered, 0)]);
        self.queue_present(PendingPresent {
            swapchain: chain,
            index: drawable.index,
        });
    }
}

/// Present every pending drawable on `queue`, right after the submit that
/// signals their semaphores. A window that changed size marks its swapchain
/// stale.
pub(crate) fn present_all(core: &Core, queue: vk::Queue, presents: &[PendingPresent]) {
    let Some(present) = &core.present else {
        return;
    };
    for pending in presents {
        let chain = &pending.swapchain;
        let waits = [chain.rendered[pending.index as usize]];
        let swapchains = [chain.raw];
        let indices = [pending.index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        match unsafe { present.swapchain.queue_present(queue, &info) } {
            Ok(false) => {}
            Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                chain.stale.store(true, Ordering::Relaxed)
            }
            Err(e) => {
                if e == vk::Result::ERROR_DEVICE_LOST {
                    super::submit::device_lost("present");
                }
                chain.stale.store(true, Ordering::Relaxed);
                log::error!("[Vulkan] present: {e}");
            }
        }
    }
}
