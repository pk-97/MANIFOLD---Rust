//! Pixel-perfect output presenter — inline blit from the main frame encoder.
//!
//! `OutputBlitter` holds a `GpuSurface` (CAMetalLayer) at project resolution
//! and a native Metal fullscreen-triangle pipeline. Each frame,
//! `present_all_windows()` checks whether the content thread has published a
//! new IOSurface index and, if so, encodes the output blit into the SAME
//! command buffer used for the workspace window.
//!
//! This preserves the old presenter's exact shader + layer behavior while
//! eliminating the second command queue that caused GPU scheduler contention.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use metal::foreign_types::ForeignType;
#[allow(unused_imports)]
use objc::{msg_send, sel, sel_impl};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

const PRESENTER_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 uv;
};

vertex VertexOut vs_presenter(uint vid [[vertex_id]]) {
    VertexOut out;
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

#[allow(dead_code)]
/// Inline output window presenter. No dedicated thread, no separate queue.
pub struct OutputBlitter {
    /// CAMetalLayer on the output window (project resolution, EDR).
    pub(crate) surface: manifold_gpu::GpuSurface,
    /// Native Metal fullscreen blit pipeline. This matches the pre-inline
    /// presenter path exactly instead of relying on WGSL -> MSL translation.
    pipeline: metal::RenderPipelineState,
    sampler: metal::SamplerState,
    /// Last front_index blitted — skip if unchanged (no new content).
    pub(crate) last_front_index: usize,
    /// Throttled diagnostic for output drawable starvation.
    last_no_drawable_log: Option<std::time::Instant>,
    /// Throttled diagnostic for slow output blits.
    last_slow_log: Option<std::time::Instant>,
}

/// Pixel-perfect output presenter backed by a dedicated thread.
pub struct NativeOutputPresenter {
    stop: Arc<AtomicBool>,
    edr_headroom: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[allow(dead_code)]
impl OutputBlitter {
    /// Create an `OutputBlitter` for the given window.
    pub fn new(
        gpu_device: &manifold_gpu::GpuDevice,
        window: &winit::window::Window,
        proj_w: u32,
        proj_h: u32,
    ) -> Self {
        let surface = gpu_device.create_surface(
            window,
            proj_w,
            proj_h,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            false,
        );
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);

        let device_ref = gpu_device.raw_device();
        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_fast_math_enabled(true);
        let library = device_ref
            .new_library_with_source(PRESENTER_MSL, &compile_opts)
            .expect("Failed to compile presenter MSL");

        let vs = library
            .get_function("vs_presenter", None)
            .expect("vs_presenter not found");
        let fs = library
            .get_function("fs_presenter", None)
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

        let sampler_desc = metal::SamplerDescriptor::new();
        sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
        sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
        let sampler = device_ref.new_sampler(&sampler_desc);

        log::info!(
            "[OutputBlitter] Created: {}x{} Rgba16Float, native MSL inline path",
            proj_w, proj_h,
        );

        Self {
            surface,
            pipeline,
            sampler,
            last_front_index: usize::MAX,
            last_no_drawable_log: None,
            last_slow_log: None,
        }
    }

    /// Blit `front_index` IOSurface to the output drawable if it has changed.
    pub(crate) fn blit_if_new(
        &mut self,
        front_index: usize,
        compositor_tex: &manifold_gpu::GpuTexture,
        gpu_device: &manifold_gpu::GpuDevice,
    ) {
        if front_index == self.last_front_index {
            return;
        }
        self.last_front_index = front_index;

        let t0 = std::time::Instant::now();
        let drawable = match self.surface.next_drawable() {
            Some(drawable) => {
                self.last_no_drawable_log = None;
                drawable
            }
            None => {
                let now = std::time::Instant::now();
                let should_log = self.last_no_drawable_log
                    .is_none_or(|t| now.duration_since(t).as_millis() >= 500);
                if should_log {
                    eprintln!(
                        "[OutputBlitter] next_drawable returned None \
                         (front_index={}, surface={}x{}, format={:?})",
                        front_index,
                        self.surface.width,
                        self.surface.height,
                        self.surface.format,
                    );
                    self.last_no_drawable_log = Some(now);
                }
                return;
            }
        };
        let drawable_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        let mut encoder = gpu_device.create_encoder("Output Frame");
        let cmd_buf = encoder.raw_cmd_buf();
        {
            let pass_desc = metal::RenderPassDescriptor::new();
            let color = pass_desc.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(drawable.texture()));
            color.set_load_action(metal::MTLLoadAction::DontCare);
            color.set_store_action(metal::MTLStoreAction::Store);

            let enc = cmd_buf.new_render_command_encoder(pass_desc);
            enc.push_debug_group("Output Blit");
            enc.set_render_pipeline_state(&self.pipeline);
            enc.set_fragment_texture(0, Some(compositor_tex.raw()));
            enc.set_fragment_sampler_state(0, Some(&self.sampler));
            enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
            enc.pop_debug_group();
            enc.end_encoding();
        }
        let encode_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if total_ms >= 16.0 {
            let now = std::time::Instant::now();
            let should_log = self.last_slow_log
                .is_none_or(|t| now.duration_since(t).as_millis() >= 500);
            if should_log {
                eprintln!(
                    "[OutputBlitter] slow blit total={:.2}ms drawable={:.2}ms encode={:.2}ms \
                     front_index={} surface={}x{}",
                    total_ms,
                    drawable_ms,
                    encode_ms,
                    front_index,
                    self.surface.width,
                    self.surface.height,
                );
                self.last_slow_log = Some(now);
            }
        }
        encoder.present_drawable(&drawable);
        encoder.commit();
    }
}

impl NativeOutputPresenter {
    pub fn new(
        gpu_device: &manifold_gpu::GpuDevice,
        window: &winit::window::Window,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
    ) -> Self {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let ns_view = match window.window_handle().unwrap().as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut objc::runtime::Object,
            _ => panic!("Expected AppKit window handle"),
        };

        let device_ref = gpu_device.raw_device();
        let command_queue = gpu_device.clone_queue();

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
        pipe_desc.color_attachments().object_at(0).unwrap()
            .set_pixel_format(metal::MTLPixelFormat::RGBA16Float);

        let pipeline = device_ref
            .new_render_pipeline_state(&pipe_desc)
            .expect("Failed to create presenter render pipeline");

        let sampler_desc = metal::SamplerDescriptor::new();
        sampler_desc.set_min_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_mag_filter(metal::MTLSamplerMinMagFilter::Nearest);
        sampler_desc.set_address_mode_s(metal::MTLSamplerAddressMode::ClampToEdge);
        sampler_desc.set_address_mode_t(metal::MTLSamplerAddressMode::ClampToEdge);
        let sampler = device_ref.new_sampler(&sampler_desc);

        let proj_w = bridge.width();
        let proj_h = bridge.height();

        let layer = metal::MetalLayer::new();
        layer.set_device(device_ref);
        layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        layer.set_framebuffer_only(true);
        // Audience display path: let CAMetalLayer pace drawable acquisition to
        // the actual display refresh instead of free-running in software.
        layer.set_display_sync_enabled(true);
        layer.set_maximum_drawable_count(3);
        layer.set_drawable_size(core_graphics_types::geometry::CGSize {
            width: proj_w as f64,
            height: proj_h as f64,
        });
        layer.set_contents_scale(1.0);

        let layer_ptr = layer.as_ptr() as *mut std::ffi::c_void;
        unsafe {
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                setAllowsNextDrawableTimeout: true];
            let _: () = msg_send![ns_view, setLayer: layer_ptr];
            let _: () = msg_send![ns_view, setWantsLayer: true];
        }

        unsafe {
            let gravity: *const objc::runtime::Object =
                msg_send![objc::class!(NSString),
                    stringWithUTF8String: c"resizeAspect".as_ptr()];
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                   setContentsGravity: gravity];
        }

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

        unsafe {
            let black = CGColorCreateGenericRGB(0.0, 0.0, 0.0, 1.0);
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                                   setBackgroundColor: black];
        }

        unsafe {
            objc::runtime::objc_retain(layer_ptr as *mut objc::runtime::Object);
        }

        let bridge_gen = bridge.generation();
        let native_textures = import_textures(device_ref, &bridge);

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
    }
}

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

unsafe impl Send for PresenterThread {}

impl PresenterThread {
    fn run(mut self) {
        self.sync_drawable_to_bridge();

        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }

            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
                self.sync_drawable_to_bridge();
            }

            // Use the latest available frame every presenter tick. Drawable
            // acquisition is display-synchronized, so this thread stays aligned
            // with the output monitor's real refresh cadence.
            let front = self.bridge.front_index() as usize;
            self.last_front = front;

            let Some(source) = self.native_textures[front].as_ref() else {
                continue;
            };

            let layer = self.layer();
            let Some(drawable) = layer.next_drawable() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };

            let pass_desc = metal::RenderPassDescriptor::new();
            let color = pass_desc.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(drawable.texture()));
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
        }
    }
}

unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> *mut std::ffi::c_void;
    static kCGColorSpaceExtendedLinearSRGB: *const std::ffi::c_void;
}

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
