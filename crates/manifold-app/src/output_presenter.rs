//! Pixel-perfect 1:1 output presenter — custom CAMetalLayer, native Metal blit.
//!
//! The output monitor is the primary audience-facing display in a live
//! performance. It uses a custom CAMetalLayer (not wgpu-managed) so the
//! drawable size is always the exact project resolution, regardless of
//! window size or macOS display scaling. Core Animation handles fitting
//! the layer contents to the window via `contentsGravity = resizeAspect`.
//!
//! Architecture:
//!   Content Thread ──render──▶ IOSurface ◀──blit── Presenter Thread
//!                              (bridge)            (custom CAMetalLayer + MTLBlit)
//!
//! Key design — "always present latest" (Mailbox-style):
//!   The presenter thread loops on `nextDrawable()` with displaySyncEnabled=true.
//!   Each drawable acquisition blocks until the display's next vsync — this IS
//!   the frame pacer. At each vsync, the thread blits whatever the content
//!   thread's latest IOSurface frame is. No poll loop, no variable detection
//!   timing, no frame hold time variation.
//!
//!   The old presenter (which flickered) polled for NEW content at 200μs intervals
//!   and only presented when content changed. This created ±200μs jitter in when
//!   presents landed relative to vsync, causing some frames to be held for 1
//!   refresh interval and others for 2 — visible as brightness flicker on noisy
//!   content. The new approach eliminates this entirely: every present is exactly
//!   on a vsync boundary, showing the latest available frame.
//!
//! Properties:
//! - **drawableSize = project resolution** (always, regardless of window size)
//! - **MTLBlitCommandEncoder** for zero-shader texture copy (IOSurface → drawable)
//! - **displaySyncEnabled = true** — nextDrawable blocks at vsync (dedicated thread)
//! - **EDR** — Rgba16Float + extendedLinearSRGB + wantsExtendedDynamicRangeContent
//! - **Dedicated thread** — never blocks the UI thread

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use metal::foreign_types::{ForeignType, ForeignTypeRef};
#[allow(unused_imports)]
use objc::{msg_send, sel, sel_impl};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

// ---------------------------------------------------------------------------
// CGColorSpace FFI
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> *mut std::ffi::c_void;
    static kCGColorSpaceExtendedLinearSRGB: *const std::ffi::c_void;
}

// ---------------------------------------------------------------------------
// NativeOutputPresenter — handle held by Application on the UI thread
// ---------------------------------------------------------------------------

/// Pixel-perfect output presenter backed by a dedicated thread.
///
/// The UI thread creates this handle, which spawns a presenter thread.
/// The thread owns a CAMetalLayer at project resolution and blits the
/// latest IOSurface content at each vsync. Dropping the handle stops
/// the thread and releases the layer.
pub struct NativeOutputPresenter {
    /// Atomic stop flag — set by Drop, checked by the presenter thread.
    stop: Arc<AtomicBool>,
    /// EDR headroom — written by UI thread, read by presenter thread.
    edr_headroom: Arc<AtomicU64>,
    /// Presenter thread handle — joined on Drop.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativeOutputPresenter {
    /// Create a new presenter with a custom CAMetalLayer on the output window.
    ///
    /// Extracts the raw MTLDevice from the wgpu device (same underlying GPU),
    /// creates a dedicated MTLCommandQueue, configures the CAMetalLayer for
    /// pixel-perfect EDR output at project resolution, and spawns the thread.
    pub fn new(
        wgpu_device: &wgpu::Device,
        window: &winit::window::Window,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
    ) -> Self {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        // --- Get NSView from winit window ---
        let ns_view = match window.window_handle().unwrap().as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut objc::runtime::Object,
            _ => panic!("Expected AppKit window handle"),
        };

        // --- Extract raw MTLDevice from wgpu ---
        let raw_device: *mut metal::MTLDevice = unsafe {
            let hal_guard = wgpu_device
                .as_hal::<wgpu_hal::api::Metal>()
                .expect("Not a Metal backend");
            let dev_ref: &metal::DeviceRef = hal_guard.raw_device();
            dev_ref.as_ptr()
        };
        let device_ref = unsafe { metal::DeviceRef::from_ptr(raw_device) };
        let command_queue = device_ref.new_command_queue();

        // --- Create CAMetalLayer ---
        let proj_w = bridge.width();
        let proj_h = bridge.height();

        let layer = metal::MetalLayer::new();
        layer.set_device(device_ref);
        layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        layer.set_framebuffer_only(true);
        // displaySyncEnabled = true: nextDrawable blocks until vsync.
        // This is the frame pacer — safe because we're on a dedicated thread.
        layer.set_display_sync_enabled(true);
        layer.set_maximum_drawable_count(3);
        layer.set_drawable_size(core_graphics_types::geometry::CGSize {
            width: proj_w as f64,
            height: proj_h as f64,
        });
        // contentsScale = 1.0: drawable pixels ARE content pixels.
        layer.set_contents_scale(1.0);

        let layer_ptr = layer.as_ptr() as *mut std::ffi::c_void;

        // Set the layer on the NSView.
        unsafe {
            let _: () = msg_send![ns_view, setLayer: layer_ptr];
            let _: () = msg_send![ns_view, setWantsLayer: true];
        }

        // contentsGravity = kCAGravityResizeAspect (letterbox/pillarbox).
        unsafe {
            let gravity: *const objc::runtime::Object =
                msg_send![objc::class!(NSString),
                    stringWithUTF8String: c"resizeAspect".as_ptr()];
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                   setContentsGravity: gravity];
        }

        // Configure EDR: colorspace + wantsExtendedDynamicRangeContent.
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB);
            if !cs.is_null() {
                let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                       setColorspace: cs];
                CGColorSpaceRelease(cs);
            }
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                   setWantsExtendedDynamicRangeContent: true];
        }

        // Black background for letterbox/pillarbox bars.
        unsafe {
            let black = CGColorCreateGenericRGB(0.0, 0.0, 0.0, 1.0);
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                   setBackgroundColor: black];
            // CGColor is retained by the layer — no need to release.
        }

        // Retain the layer — released in PresenterThread Drop (via stop + join).
        unsafe {
            objc::runtime::objc_retain(layer_ptr as *mut objc::runtime::Object);
        }

        // --- Import IOSurface textures ---
        let bridge_gen = bridge.generation();
        let native_textures = import_textures(device_ref, &bridge);

        log::info!(
            "[NativeOutputPresenter] Created: {}x{} Rgba16Float, EDR={:.2}x, vsync=true",
            proj_w, proj_h, edr_headroom,
        );

        // --- Spawn presenter thread ---
        let stop = Arc::new(AtomicBool::new(false));
        let edr = Arc::new(AtomicU64::new(edr_headroom.to_bits()));

        let thread_state = PresenterThread {
            command_queue,
            bridge,
            layer_ptr,
            native_textures,
            last_bridge_gen: bridge_gen,
            drawable_width: proj_w,
            drawable_height: proj_h,
            stop: Arc::clone(&stop),
            edr_headroom: Arc::clone(&edr),
        };

        let thread = std::thread::Builder::new()
            .name("output-presenter".into())
            .spawn(move || thread_state.run())
            .expect("failed to spawn output-presenter thread");

        Self {
            stop,
            edr_headroom: edr,
            thread: Some(thread),
        }
    }

    /// Update EDR headroom (e.g., window moved to a different display).
    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom.store(headroom.to_bits(), Ordering::Relaxed);
    }
}

impl Drop for NativeOutputPresenter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        log::info!("[NativeOutputPresenter] Dropped");
    }
}

// ---------------------------------------------------------------------------
// Presenter thread — runs the vsync-locked blit loop
// ---------------------------------------------------------------------------

/// Internal state that lives on the presenter thread.
struct PresenterThread {
    command_queue: metal::CommandQueue,
    bridge: Arc<SharedTextureBridge>,
    layer_ptr: *mut std::ffi::c_void,
    native_textures: [Option<metal::Texture>; SURFACE_COUNT],
    last_bridge_gen: u64,
    drawable_width: u32,
    drawable_height: u32,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    edr_headroom: Arc<AtomicU64>,
}

// SAFETY: CAMetalLayer is documented thread-safe for nextDrawable and property access.
// Metal command queue and textures are Send+Sync. The layer pointer is stable (retained).
unsafe impl Send for PresenterThread {}

impl PresenterThread {
    fn run(mut self) {
        // Initial sync: match drawable to project resolution.
        self.sync_drawable_to_bridge();

        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }

            // Check for bridge resize — reimport textures + update drawable size.
            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
                self.sync_drawable_to_bridge();
            }

            // Acquire drawable — blocks until vsync (displaySyncEnabled=true).
            // This IS the frame pacer. Every iteration = one vsync interval.
            let layer = self.layer();
            let Some(drawable) = layer.next_drawable() else {
                // Drawable pool exhausted (shouldn't happen with pool of 3).
                // Brief sleep to avoid spinning.
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };

            // Always blit the LATEST content, even if it hasn't changed.
            // This gives consistent frame hold times — every vsync shows a frame,
            // no variable brightness from skipped presents.
            let front = self.bridge.front_index() as usize;
            let Some(source) = self.native_textures[front].as_ref() else {
                continue;
            };

            let drawable_tex = drawable.texture();
            let w = self.drawable_width;
            let h = self.drawable_height;

            // GPU blit: IOSurface → drawable (same dimensions, same format).
            let cmd_buf = self.command_queue.new_command_buffer();
            let blit_enc = cmd_buf.new_blit_command_encoder();
            blit_enc.copy_from_texture(
                source,
                0,
                0,
                metal::MTLOrigin { x: 0, y: 0, z: 0 },
                metal::MTLSize {
                    width: w as u64,
                    height: h as u64,
                    depth: 1,
                },
                drawable_tex,
                0,
                0,
                metal::MTLOrigin { x: 0, y: 0, z: 0 },
            );
            blit_enc.end_encoding();

            cmd_buf.present_drawable(drawable);
            cmd_buf.commit();
        }

        // Release the retained CAMetalLayer.
        unsafe {
            objc::runtime::objc_release(self.layer_ptr as *mut objc::runtime::Object);
        }
    }

    fn layer(&self) -> &metal::MetalLayerRef {
        unsafe { &*(self.layer_ptr as *const metal::MetalLayerRef) }
    }

    fn reimport_textures(&mut self) {
        let device = self.command_queue.device();
        self.native_textures = import_textures(device, &self.bridge);
    }

    fn sync_drawable_to_bridge(&mut self) {
        let w = self.bridge.width();
        let h = self.bridge.height();
        if w != self.drawable_width || h != self.drawable_height {
            self.drawable_width = w;
            self.drawable_height = h;
            self.layer().set_drawable_size(core_graphics_types::geometry::CGSize {
                width: w as f64,
                height: h as f64,
            });
            log::info!(
                "[NativeOutputPresenter] drawable synced to project: {}x{}",
                w, h,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Import all IOSurface-backed textures as native Metal textures.
fn import_textures(
    device: &metal::DeviceRef,
    bridge: &SharedTextureBridge,
) -> [Option<metal::Texture>; SURFACE_COUNT] {
    let width = bridge.width();
    let height = bridge.height();

    std::array::from_fn(|i| {
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        descriptor.set_width(width as u64);
        descriptor.set_height(height as u64);
        descriptor.set_depth(1);
        descriptor.set_mipmap_level_count(1);
        descriptor.set_sample_count(1);
        descriptor.set_texture_type(metal::MTLTextureType::D2);
        descriptor.set_usage(
            metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::ShaderWrite,
        );
        descriptor.set_storage_mode(metal::MTLStorageMode::Shared);

        let io_surface_ref = bridge.raw_io_surface(i);
        let raw_mtl_texture: *mut objc::runtime::Object = unsafe {
            msg_send![
                device,
                newTextureWithDescriptor:descriptor.as_ref()
                iosurface:io_surface_ref
                plane:0usize
            ]
        };
        assert!(
            !raw_mtl_texture.is_null(),
            "newTextureWithDescriptor:iosurface:plane: failed for surface {i}",
        );
        Some(unsafe { metal::Texture::from_ptr(raw_mtl_texture as *mut _) })
    })
}
