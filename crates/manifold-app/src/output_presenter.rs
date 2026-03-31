//! Pixel-perfect 1:1 output presenter — custom CAMetalLayer, native Metal blit.
//!
//! The output monitor is the primary audience-facing display in a live
//! performance. It uses a custom CAMetalLayer (not wgpu-managed) so the
//! drawable size is always the exact project resolution, regardless of
//! window size or macOS display scaling. Core Animation handles fitting
//! the layer contents to the window via `contentsGravity = resizeAspect`.
//!
//! Architecture:
//!   Content Thread ──render──▶ IOSurface ◀──blit── NativeOutputPresenter (UI thread)
//!                              (bridge)            (custom CAMetalLayer + MTLBlit)
//!
//! Key properties:
//! - **drawableSize = project resolution** (always, no matter the window size)
//! - **MTLBlitCommandEncoder** for zero-shader texture copy (IOSurface → drawable)
//! - **displaySyncEnabled = true** — vsync-locked, no free-running poll
//! - **EDR** — Rgba16Float + extendedLinearSRGB + wantsExtendedDynamicRangeContent
//! - **UI thread** — natural frame cadence from winit event loop, not a separate thread

use std::sync::Arc;

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
    static kCGColorSpaceExtendedLinearSRGB: *const std::ffi::c_void;
}

// ---------------------------------------------------------------------------
// NativeOutputPresenter — lives on the UI thread
// ---------------------------------------------------------------------------

/// Pixel-perfect output presenter using a custom CAMetalLayer.
///
/// Owns a CAMetalLayer set on the output window's NSView, configured with
/// drawableSize = project resolution. Each frame, acquires a drawable and
/// uses MTLBlitCommandEncoder to copy the IOSurface content to the drawable
/// texture — zero shaders, zero fragment work, just a GPU memcpy.
pub struct NativeOutputPresenter {
    /// Dedicated MTLCommandQueue (separate from wgpu's queue and content queue).
    command_queue: metal::CommandQueue,

    /// IOSurface bridge (shared with content thread).
    bridge: Arc<SharedTextureBridge>,

    /// Raw pointer to the CAMetalLayer on the output window's NSView.
    /// Retained — released in Drop.
    layer_ptr: *mut std::ffi::c_void,

    /// Native Metal textures imported from the triple-buffered IOSurfaces.
    /// Each is backed by the corresponding IOSurface — zero copy.
    native_textures: [Option<metal::Texture>; SURFACE_COUNT],

    /// Last seen bridge generation — detects resize.
    last_bridge_gen: u64,

    /// Drawable dimensions (always = project resolution).
    drawable_width: u32,
    drawable_height: u32,

    /// Last presented front index — skip re-blit when content hasn't changed.
    last_front: usize,

    /// EDR headroom for the output window's display.
    /// Not currently used for blit (passthrough), but tracked for future tonemap.
    pub(crate) edr_headroom: f64,
}

// SAFETY: NativeOutputPresenter lives exclusively on the UI thread.
// The layer_ptr is a retained CAMetalLayer — safe from the main thread.
// metal::CommandQueue and metal::Texture are Send+Sync.
unsafe impl Send for NativeOutputPresenter {}

impl NativeOutputPresenter {
    /// Create a new presenter with a custom CAMetalLayer on the output window.
    ///
    /// Extracts the raw MTLDevice from the wgpu device (same underlying GPU),
    /// creates a dedicated MTLCommandQueue, configures the CAMetalLayer for
    /// pixel-perfect EDR output at project resolution.
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
        // SAFETY: raw_device points to the same MTLDevice that wgpu uses.
        // Creating a command queue retains it.
        let device_ref = unsafe { metal::DeviceRef::from_ptr(raw_device) };
        let command_queue = device_ref.new_command_queue();

        // --- Create CAMetalLayer ---
        let proj_w = bridge.width();
        let proj_h = bridge.height();

        let layer = metal::MetalLayer::new();
        layer.set_device(device_ref);
        layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        layer.set_framebuffer_only(true);
        // displaySyncEnabled = false: nextDrawable returns immediately instead
        // of blocking until vsync. Since we're on the UI thread, blocking would
        // stall the workspace render. Frame pacing comes from winit's event loop
        // and Core Animation's present-at-vsync behavior.
        layer.set_display_sync_enabled(false);
        // Maximize drawable count to reduce nextDrawable blocking.
        layer.set_maximum_drawable_count(3);
        layer.set_drawable_size(core_graphics_types::geometry::CGSize {
            width: proj_w as f64,
            height: proj_h as f64,
        });
        // contentsScale = 1.0: drawable pixels ARE content pixels.
        // Core Animation scales the layer to fit the view via contentsGravity.
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

        // Retain the layer — our explicit retain keeps it alive after the
        // local `MetalLayer` drops. Released in NativeOutputPresenter::drop.
        unsafe {
            objc::runtime::objc_retain(layer_ptr as *mut objc::runtime::Object);
        }

        // --- Import IOSurface textures as native Metal textures ---
        let bridge_gen = bridge.generation();
        let native_textures = Self::import_textures(device_ref, &bridge);

        log::info!(
            "[NativeOutputPresenter] Created: {}x{} Rgba16Float, EDR={:.2}x, non-blocking",
            proj_w, proj_h, edr_headroom,
        );

        Self {
            command_queue,
            bridge,
            layer_ptr,
            native_textures,
            last_bridge_gen: bridge_gen,
            drawable_width: proj_w,
            drawable_height: proj_h,
            last_front: usize::MAX,
            edr_headroom,
        }
    }

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

    /// Reference to the CAMetalLayer.
    fn layer(&self) -> &metal::MetalLayerRef {
        unsafe { &*(self.layer_ptr as *const metal::MetalLayerRef) }
    }

    /// Get the raw MTLDevice from the command queue.
    fn device(&self) -> &metal::DeviceRef {
        self.command_queue.device()
    }

    /// Present the current front buffer to the output window.
    ///
    /// Called each frame from the UI thread render loop. Acquires a drawable,
    /// copies the IOSurface texture via MTLBlitCommandEncoder, and presents.
    ///
    /// Returns `true` if a frame was presented.
    pub fn present(&mut self) -> bool {
        // Check for bridge resize — reimport textures + update drawable size.
        let bridge_gen = self.bridge.generation();
        if bridge_gen != self.last_bridge_gen {
            self.last_bridge_gen = bridge_gen;
            self.native_textures = Self::import_textures(self.device(), &self.bridge);
            self.sync_drawable_to_bridge();
        }

        // Read which surface the content thread published.
        let front = self.bridge.front_index() as usize;

        // Skip if content hasn't changed since last present.
        if front == self.last_front {
            return false;
        }
        self.last_front = front;

        let Some(source) = self.native_textures[front].as_ref() else {
            return false;
        };

        // Acquire drawable — blocks until vsync with displaySyncEnabled=true.
        let Some(drawable) = self.layer().next_drawable() else {
            return false;
        };

        let drawable_tex = drawable.texture();
        let w = self.drawable_width;
        let h = self.drawable_height;

        // GPU blit: IOSurface texture → drawable texture (same dimensions, same format).
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

        true
    }

    /// Sync the CAMetalLayer drawableSize to the IOSurface (project) dimensions.
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

    /// Update EDR headroom (e.g., window moved to a different display).
    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom = headroom;
    }
}

impl Drop for NativeOutputPresenter {
    fn drop(&mut self) {
        // Release the retained CAMetalLayer.
        unsafe {
            objc::runtime::objc_release(self.layer_ptr as *mut objc::runtime::Object);
        }
        log::info!("[NativeOutputPresenter] Dropped");
    }
}
