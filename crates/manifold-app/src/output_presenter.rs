#![allow(dead_code)]
//! Dedicated output-window presenter thread — native Metal, zero wgpu.
//! DEPRECATED: Output window now uses wgpu surface (same path as workspace preview).
//!
//! The output monitor is the primary audience-facing display in a live
//! performance. It runs on its own `GpuDevice` (separate Metal command queue
//! from both the content thread and the UI thread) so that `nextDrawable`
//! blocking in fullscreen mode never stalls workspace rendering, and GPU
//! command submission has zero queue contention.
//!
//! Architecture:
//!   Content Thread ──render──▶ IOSurface ◀──read── Output Presenter Thread
//!                              (bridge)            (separate MTLDevice + queue)
//!
//! The presenter owns a `CAMetalLayer` set on the output window's NSView,
//! configured for EDR (Rgba16Float + extendedLinearSRGB). It polls the
//! IOSurface bridge for new content frames, acquires a drawable from the
//! layer, renders a tonemap blit via `GpuEncoder`, and presents.

use std::sync::{
    Arc,
    mpsc::{Receiver, Sender, TryRecvError, channel},
};

#[allow(unused_imports)]
use objc::{msg_send, sel, sel_impl};

use manifold_gpu::{
    GpuDevice, GpuRenderPipeline, GpuSampler, GpuTexture, GpuTextureFormat,
    GpuSamplerDesc, GpuFilterMode, GpuAddressMode,
};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

// ---------------------------------------------------------------------------
// CGColorSpace FFI (same externs as edr_surface.rs)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    static kCGColorSpaceExtendedLinearSRGB: *const std::ffi::c_void;
}

// ---------------------------------------------------------------------------
// Tonemap blit WGSL — same shader as manifold_renderer::tonemap_blit
// ---------------------------------------------------------------------------

const TONEMAP_BLIT_WGSL: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Uniforms {
    mode: u32,
};
@group(0) @binding(2) var<uniform> u: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vertex_index) / 2) * 4.0 - 1.0;
    let y = f32(i32(vertex_index) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;

fn aces_film(x: vec3<f32>) -> vec3<f32> {
    let a: f32 = 2.51;
    let b: f32 = 0.03;
    let c: f32 = 2.43;
    let d: f32 = 0.59;
    let e: f32 = 0.14;
    return saturate((x * (a * x + b)) / (x * (c * x + d) + e));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(t_source, s_source, in.uv);
    if (u.mode == 1u) {
        return vec4<f32>(aces_film(src.rgb), src.a);
    }
    return src;
}
"#;

/// Uniform layout — must match the WGSL struct.
#[repr(C)]
struct Uniforms {
    /// 0 = passthrough (HDR display), 1 = ACES SDR tonemap.
    mode: u32,
    _pad: [u32; 3],
}

// ---------------------------------------------------------------------------
// Commands sent from the UI thread to the presenter thread
// ---------------------------------------------------------------------------

pub enum OutputCommand {
    /// Output window was resized (e.g. entering/leaving fullscreen).
    /// Drawable stays at project resolution — this is kept for future use.
    #[expect(dead_code)]
    Resize { width: u32, height: u32, scale: f64 },
    /// EDR headroom changed (window moved to a different display).
    UpdateEdrHeadroom(f64),
    /// Stop the presenter thread and return.
    Stop,
}

// ---------------------------------------------------------------------------
// Handle held by Application on the UI thread
// ---------------------------------------------------------------------------

pub struct OutputPresenterHandle {
    pub sender: Sender<OutputCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for OutputPresenterHandle {
    fn drop(&mut self) {
        let _ = self.sender.send(OutputCommand::Stop);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Internal presenter state (lives on the presenter thread)
// ---------------------------------------------------------------------------

struct OutputPresenter {
    device: GpuDevice,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    bridge: Arc<SharedTextureBridge>,

    /// Raw pointer to the CAMetalLayer on the output window's NSView.
    /// Lifetime guaranteed by: presenter thread is joined before window close.
    layer_ptr: *mut std::ffi::c_void,

    native_textures: [Option<GpuTexture>; SURFACE_COUNT],
    last_front: usize,
    last_bridge_gen: u64,

    edr_headroom: f64,
    drawable_width: u32,
    drawable_height: u32,
}

// SAFETY: CAMetalLayer is documented thread-safe for nextDrawable and property access.
// GpuDevice and all GPU types are Send+Sync. The layer pointer is stable (retained by NSView).
unsafe impl Send for OutputPresenter {}

impl OutputPresenter {
    /// Re-import IOSurface textures as native Metal GpuTextures.
    fn reimport_textures(&mut self) {
        self.native_textures = std::array::from_fn(|i| {
            // SAFETY: bridge outlives presenter (Arc), device owns the same
            // underlying MTLDevice that the bridge was created for.
            Some(unsafe { self.bridge.import_texture_native(&self.device, i) })
        });
    }

    /// Reference to the CAMetalLayer.
    fn layer(&self) -> &metal::MetalLayerRef {
        unsafe { &*(self.layer_ptr as *const metal::MetalLayerRef) }
    }

    /// Sync the CAMetalLayer drawableSize to the IOSurface (project) dimensions.
    /// Called when the bridge resizes (project resolution change).
    /// The drawable matches project pixels 1:1 — Core Animation handles
    /// display scaling via contentsGravity = kCAGravityResizeAspect.
    fn sync_drawable_to_bridge(&mut self) {
        let w = self.bridge.width();
        let h = self.bridge.height();
        if w != self.drawable_width || h != self.drawable_height {
            self.drawable_width = w;
            self.drawable_height = h;
            let layer = self.layer();
            layer.set_drawable_size(core_graphics_types::geometry::CGSize {
                width: w as f64,
                height: h as f64,
            });
            eprintln!(
                "[Presenter] drawable synced to project: {}×{}",
                w, h,
            );
        }
    }

    fn run(mut self, rx: Receiver<OutputCommand>) {
        // Initial sync: match drawable to project resolution.
        self.sync_drawable_to_bridge();

        loop {
            // --- Drain all pending commands (non-blocking) ---
            loop {
                match rx.try_recv() {
                    Ok(OutputCommand::Stop) => return,
                    Ok(OutputCommand::Resize { .. }) => {
                        // Window resize — drawable stays at project resolution.
                        // Core Animation handles display scaling.
                    }
                    Ok(OutputCommand::UpdateEdrHeadroom(h)) => {
                        self.edr_headroom = h;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // --- Re-import textures if bridge was resized ---
            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
                // Project resolution changed — sync drawable.
                self.sync_drawable_to_bridge();
            }

            // --- Wait for new content frame (tight poll, no drawable consumption) ---
            let front = self.bridge.front_index() as usize;
            if front == self.last_front {
                std::thread::sleep(std::time::Duration::from_micros(200));
                continue;
            }
            self.last_front = front;

            let Some(source) = self.native_textures[front].as_ref() else {
                continue;
            };

            // --- Acquire drawable at next vsync ---
            // With displaySyncEnabled=true, this blocks until the display is
            // ready. Only called when we have content — no drawable waste.
            // Guarantees every present lands exactly on a vsync boundary.
            let layer_ref = unsafe { &*(self.layer_ptr as *const metal::MetalLayerRef) };
            let Some(drawable) = layer_ref.next_drawable() else {
                continue;
            };
            let drawable_tex = drawable.texture();

            let surface_w = self.drawable_width;
            let surface_h = self.drawable_height;

            // --- Create command buffer + render pass ---
            // 1:1 blit: drawable matches project resolution exactly.
            // No upscale, no viewport fit — fullscreen triangle covers all pixels.
            let mut enc = self.device.create_encoder("Output Blit");
            let cmd_buf = enc.raw_cmd_buf();

            let pass_desc = metal::RenderPassDescriptor::new();
            let color = pass_desc.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(drawable_tex));
            color.set_load_action(metal::MTLLoadAction::DontCare);
            color.set_store_action(metal::MTLStoreAction::Store);

            let render_enc = cmd_buf.new_render_command_encoder(pass_desc);
            render_enc.set_render_pipeline_state(self.pipeline.raw_state());
            render_enc.set_viewport(metal::MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: surface_w as f64,
                height: surface_h as f64,
                znear: 0.0,
                zfar: 1.0,
            });

            // Fragment bindings — use slot map for Metal argument indices.
            if let Some(slot) = self.pipeline.slot_map.get(0) {
                render_enc.set_fragment_texture(
                    slot.metal_index as u64, Some(source.raw()),
                );
            }
            if let Some(slot) = self.pipeline.slot_map.get(1) {
                render_enc.set_fragment_sampler_state(
                    slot.metal_index as u64, Some(self.sampler.raw()),
                );
            }
            let uniforms = Uniforms {
                mode: if self.edr_headroom <= 1.0 { 1 } else { 0 },
                _pad: [0; 3],
            };
            if let Some(slot) = self.pipeline.slot_map.get(2) {
                let data = unsafe {
                    std::slice::from_raw_parts(
                        &uniforms as *const Uniforms as *const u8,
                        std::mem::size_of::<Uniforms>(),
                    )
                };
                render_enc.set_fragment_bytes(
                    slot.metal_index as u64,
                    data.len() as u64,
                    data.as_ptr() as *const _,
                );
            }

            render_enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
            render_enc.end_encoding();

            // Present drawable and commit — cmd_buf reference from raw_cmd_buf().
            // GpuEncoder::drop() will release the retained cmd_buf pointer.
            cmd_buf.present_drawable(drawable);
            cmd_buf.commit();
        }
    }
}

// ---------------------------------------------------------------------------
// Public API: spawn a native Metal presenter thread for one output window
// ---------------------------------------------------------------------------

/// Spawn the output-presenter thread backed by a dedicated `GpuDevice`.
///
/// Creates a `CAMetalLayer` on the output window's NSView, configures it
/// for EDR (Rgba16Float + extendedLinearSRGB), compiles the tonemap blit
/// pipeline, and starts the presenter loop.
///
/// Returns a handle that stops and joins the thread on drop.
pub fn spawn(
    window: &Arc<winit::window::Window>,
    bridge: Arc<SharedTextureBridge>,
    edr_headroom: f64,
) -> OutputPresenterHandle {
    use metal::foreign_types::ForeignType;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // --- Get NSView from winit window ---
    let ns_view = match window.window_handle().unwrap().as_raw() {
        RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut objc::runtime::Object,
        _ => panic!("Expected AppKit window handle"),
    };

    // --- Create a dedicated GpuDevice (separate MTLDevice + MTLCommandQueue) ---
    let device = GpuDevice::new();

    // --- Create CAMetalLayer and configure for EDR output ---
    let layer = metal::MetalLayer::new();
    layer.set_device(device.raw_device());
    layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
    layer.set_framebuffer_only(true);
    layer.set_display_sync_enabled(true);

    // Drawable size is set by the presenter thread to match project resolution
    // (1:1 pixel-perfect). Initial size uses bridge dimensions; presenter syncs
    // on first frame and on bridge resize. Core Animation handles display scaling.
    let proj_w = bridge.width();
    let proj_h = bridge.height();
    layer.set_drawable_size(core_graphics_types::geometry::CGSize {
        width: proj_w as f64,
        height: proj_h as f64,
    });
    // contentsScale = 1.0: drawable pixels ARE the content pixels.
    // Core Animation scales the layer to fit the view via contentsGravity.
    layer.set_contents_scale(1.0);

    // Set CAMetalLayer on the NSView.
    let layer_ptr = layer.as_ptr() as *mut std::ffi::c_void;
    unsafe {
        let _: () = objc::msg_send![ns_view, setLayer: layer_ptr];
        let _: () = objc::msg_send![ns_view, setWantsLayer: true];
    }

    // Configure aspect-correct display scaling.
    // kCAGravityResizeAspect preserves aspect ratio (letterbox/pillarbox).
    unsafe {
        let gravity: *const objc::runtime::Object =
            objc::msg_send![objc::class!(NSString),
                stringWithUTF8String: c"resizeAspect".as_ptr()];
        let _: () = objc::msg_send![layer_ptr as *mut objc::runtime::Object,
                                     setContentsGravity: gravity];
    }

    // Configure EDR: colorspace + wantsExtendedDynamicRangeContent.
    unsafe {
        let cs = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB);
        if !cs.is_null() {
            let _: () = objc::msg_send![layer_ptr as *mut objc::runtime::Object,
                                         setColorspace: cs];
            CGColorSpaceRelease(cs);
        }
        let _: () = objc::msg_send![layer_ptr as *mut objc::runtime::Object,
                                     setWantsExtendedDynamicRangeContent: true];
    }

    // Retain the layer so it survives after the local `MetalLayer` drops.
    // The NSView holds its own retain; this extra retain keeps it alive for
    // the presenter thread. Released in OutputPresenter::drop.
    unsafe { objc::runtime::objc_retain(layer_ptr as *mut objc::runtime::Object); }
    // `layer` (MetalLayer owned value) drops here — releases the +1 from MetalLayer::new().
    // Our explicit retain above keeps the underlying CAMetalLayer alive.

    // --- Compile tonemap blit pipeline (WGSL → MSL → Metal) ---
    let pipeline = device.create_render_pipeline(
        TONEMAP_BLIT_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        None,
        "Output Tonemap Blit",
    );

    // --- Create sampler (nearest — 1:1 pixel-perfect, no interpolation) ---
    let sampler = device.create_sampler(&GpuSamplerDesc {
        min_filter: GpuFilterMode::Nearest,
        mag_filter: GpuFilterMode::Nearest,
        address_mode_u: GpuAddressMode::ClampToEdge,
        address_mode_v: GpuAddressMode::ClampToEdge,
        ..GpuSamplerDesc::default()
    });

    // --- Import IOSurface textures as native Metal GpuTextures ---
    let bridge_gen = bridge.generation();
    let native_textures: [Option<GpuTexture>; SURFACE_COUNT] = std::array::from_fn(|i| {
        Some(unsafe { bridge.import_texture_native(&device, i) })
    });

    let (tx, rx) = channel();

    let presenter = OutputPresenter {
        device,
        pipeline,
        sampler,
        bridge,
        layer_ptr,
        native_textures,
        last_front: usize::MAX,
        last_bridge_gen: bridge_gen,
        edr_headroom,
        drawable_width: proj_w,
        drawable_height: proj_h,
    };

    let thread = std::thread::Builder::new()
        .name("output-presenter".into())
        .spawn(move || presenter.run(rx))
        .expect("failed to spawn output-presenter thread");

    OutputPresenterHandle { sender: tx, thread: Some(thread) }
}
