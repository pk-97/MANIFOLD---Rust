//! Pixel-perfect 1:1 output presenter — custom CAMetalLayer, tile-friendly render pass.
//!
//! The output monitor is the primary audience-facing display in a live
//! performance. It uses a custom CAMetalLayer (not wgpu-managed) so the
//! drawable size is always the exact project resolution, regardless of
//! window size or macOS display scaling. Core Animation handles fitting
//! the layer contents to the window via `contentsGravity = resizeAspect`.
//!
//! Architecture:
//!   Content Thread ──render──▶ IOSurface ◀──sample── Presenter Thread
//!                              (bridge)              (fullscreen triangle → drawable)
//!
//! GPU strategy — render pass, not blit:
//!   On Apple Silicon TBDR, MTLBlitCommandEncoder does a large linear memory
//!   transaction (62MB read + 62MB write at 3456×2234 Rgba16Float) that bypasses
//!   tile memory and saturates the memory fabric. A fullscreen triangle render
//!   pass samples the IOSurface texture through the texture sampling hardware
//!   and writes directly to tile memory, with dramatically lower external memory
//!   bandwidth. This eliminates GPU contention with the UI thread.
//!
//! Properties:
//! - **drawableSize = project resolution** (always, regardless of window size)
//! - **Fullscreen triangle render pass** (TBDR tile-friendly, not linear blit)
//! - **EDR** — Rgba16Float + extendedLinearSRGB + wantsExtendedDynamicRangeContent
//! - **Dedicated thread** — never blocks the UI thread
//! - **Only presents on new content** — no redundant GPU work

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use metal::foreign_types::ForeignType;
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
// MSL shader — fullscreen triangle + texture sample (passthrough)
// ---------------------------------------------------------------------------

/// Metal Shading Language source for the output presenter.
/// Fullscreen triangle generated from vertex_id (no vertex buffer).
/// Fragment shader samples the IOSurface texture — pure passthrough.
/// The compositor already handles tonemapping; no additional processing needed.
const PRESENTER_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 uv;
};

vertex VertexOut vs_presenter(uint vid [[vertex_id]]) {
    VertexOut out;
    // Fullscreen triangle: 3 vertices cover the entire screen.
    // Vertex 0: (-1, -1), Vertex 1: (3, -1), Vertex 2: (-1, 3)
    float x = float(int(vid) / 2) * 4.0 - 1.0;
    float y = float(int(vid) % 2) * 4.0 - 1.0;
    out.position = float4(x, y, 0.0, 1.0);
    out.uv = float2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fragment half4 fs_presenter(
    VertexOut in [[stage_in]],
    texture2d<half> tex [[texture(0)]],
    sampler smp [[sampler(0)]]
) {
    return tex.sample(smp, in.uv);
}
"#;

// ---------------------------------------------------------------------------
// NativeOutputPresenter — handle held by Application on the UI thread
// ---------------------------------------------------------------------------

/// Pixel-perfect output presenter backed by a dedicated thread.
///
/// The UI thread creates this handle, which spawns a presenter thread.
/// The thread owns a CAMetalLayer at project resolution and renders the
/// latest IOSurface content via a fullscreen triangle render pass.
/// Dropping the handle stops the thread and releases the layer.
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
    /// Takes the native Metal GPU device, creates a dedicated MTLCommandQueue,
    /// compiles the MSL render pipeline,
    /// configures the CAMetalLayer for pixel-perfect EDR output at project
    /// resolution, and spawns the presenter thread.
    pub fn new(
        gpu_device: &manifold_gpu::GpuDevice,
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

        let device_ref = gpu_device.raw_device();
        let command_queue = device_ref.new_command_queue();

        // --- Compile MSL render pipeline ---
        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_fast_math_enabled(true);
        let library = device_ref
            .new_library_with_source(PRESENTER_MSL, &compile_opts)
            .expect("Failed to compile presenter MSL");

        let vs = library.get_function("vs_presenter", None)
            .expect("vs_presenter not found");
        let fs = library.get_function("fs_presenter", None)
            .expect("fs_presenter not found");

        let pipe_desc = metal::RenderPipelineDescriptor::new();
        pipe_desc.set_vertex_function(Some(&vs));
        pipe_desc.set_fragment_function(Some(&fs));
        pipe_desc
            .color_attachments()
            .object_at(0)
            .unwrap()
            .set_pixel_format(metal::MTLPixelFormat::RGBA16Float);

        let pipeline = device_ref
            .new_render_pipeline_state(&pipe_desc)
            .expect("Failed to create presenter render pipeline");

        // --- Create sampler (nearest — 1:1 pixel-perfect, no interpolation) ---
        let sampler_desc = metal::SamplerDescriptor::new();
        sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
        sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
        let sampler = device_ref.new_sampler(&sampler_desc);

        // --- Create CAMetalLayer ---
        let proj_w = bridge.width();
        let proj_h = bridge.height();

        let layer = metal::MetalLayer::new();
        layer.set_device(device_ref);
        layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        layer.set_framebuffer_only(true);
        // displaySyncEnabled = false: nextDrawable returns immediately.
        // We only call it when content has changed (60/sec), so no tearing.
        // Core Animation still presents at vsync.
        layer.set_display_sync_enabled(false);
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
        }

        // Retain the layer — released in PresenterThread when it exits.
        unsafe {
            objc::runtime::objc_retain(layer_ptr as *mut objc::runtime::Object);
        }

        // --- Import IOSurface textures ---
        let bridge_gen = bridge.generation();
        let native_textures = import_textures(device_ref, &bridge);

        log::info!(
            "[NativeOutputPresenter] Created: {}x{} Rgba16Float, EDR={:.2}x, render-pass",
            proj_w, proj_h, edr_headroom,
        );

        // --- Spawn presenter thread ---
        let stop = Arc::new(AtomicBool::new(false));
        let edr = Arc::new(AtomicU64::new(edr_headroom.to_bits()));

        let thread_state = PresenterThread {
            command_queue,
            pipeline,
            sampler,
            bridge,
            layer_ptr,
            native_textures,
            last_bridge_gen: bridge_gen,
            drawable_width: proj_w,
            drawable_height: proj_h,
            last_front: usize::MAX,
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
// Presenter thread — runs the render-pass presentation loop
// ---------------------------------------------------------------------------

struct PresenterThread {
    command_queue: metal::CommandQueue,
    pipeline: metal::RenderPipelineState,
    sampler: metal::SamplerState,
    bridge: Arc<SharedTextureBridge>,
    layer_ptr: *mut std::ffi::c_void,
    native_textures: [Option<metal::Texture>; SURFACE_COUNT],
    last_bridge_gen: u64,
    drawable_width: u32,
    drawable_height: u32,
    last_front: usize,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    edr_headroom: Arc<AtomicU64>,
}

// SAFETY: CAMetalLayer is documented thread-safe for nextDrawable and property access.
// Metal command queue, pipeline state, sampler, and textures are Send+Sync.
unsafe impl Send for PresenterThread {}

impl PresenterThread {
    fn run(mut self) {
        self.sync_drawable_to_bridge();

        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }

            // Check for bridge resize — reimport textures + update drawable.
            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
                self.sync_drawable_to_bridge();
            }

            // Only present when content thread publishes a new frame.
            let front = self.bridge.front_index() as usize;
            if front == self.last_front {
                std::thread::sleep(std::time::Duration::from_micros(500));
                continue;
            }
            self.last_front = front;

            let Some(source) = self.native_textures[front].as_ref() else {
                continue;
            };

            // Acquire drawable — returns immediately (displaySyncEnabled=false).
            let layer = self.layer();
            let Some(drawable) = layer.next_drawable() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };

            let drawable_tex = drawable.texture();

            // Render pass: sample IOSurface → write to drawable via tile memory.
            // On Apple Silicon TBDR this is dramatically cheaper than a linear blit
            // because it stays in tile memory instead of doing 62MB read + 62MB write
            // through the memory fabric.
            let pass_desc = metal::RenderPassDescriptor::new();
            let color = pass_desc.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(drawable_tex));
            color.set_load_action(metal::MTLLoadAction::DontCare);
            color.set_store_action(metal::MTLStoreAction::Store);

            let cmd_buf = self.command_queue.new_command_buffer();
            let enc = cmd_buf.new_render_command_encoder(pass_desc);

            enc.set_render_pipeline_state(&self.pipeline);
            enc.set_viewport(metal::MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: self.drawable_width as f64,
                height: self.drawable_height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
            enc.set_fragment_texture(0, Some(source));
            enc.set_fragment_sampler_state(0, Some(&self.sampler));
            enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
            enc.end_encoding();

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
