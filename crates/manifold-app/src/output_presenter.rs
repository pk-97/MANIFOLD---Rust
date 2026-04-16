//! CAMetalDisplayLink-based output presenter (macOS 14+).
//!
//! Decouples content production from display presentation via IOSurface
//! triple buffer. The content thread writes to the IOSurface at its own
//! pace; this presenter reads the latest complete frame at each vsync
//! and blits it to the output drawable.
//!
//! CAMetalDisplayLink provides:
//! - Drawables directly in the callback (no nextDrawable blocking)
//! - Automatic display retargeting when windows move between monitors
//! - preferredFrameRateRange for clean frame rate divisor math
//! - Precise targetTimestamp / targetPresentationTimestamp
//!
//! The callback fires on the main thread's run loop. The blit is a single
//! fullscreen GPU operation (~0.3ms) — well within the vsync budget.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc::{sel, sel_impl};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

// ─── CAMetalDisplayLink FFI ────────────────────────────────────────────

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}

// NSRunLoop mode constant — a global NSString*, not a function.
unsafe extern "C" {
    static NSRunLoopCommonModes: *mut c_void;
}

/// Register the ObjC delegate class for CAMetalDisplayLink.
/// Called once; returns the class for all subsequent delegate instances.
fn delegate_class() -> &'static objc::runtime::Class {
    use std::sync::Once;
    use objc::declare::ClassDecl;
    use objc::runtime::Class;

    static REGISTER: Once = Once::new();
    static mut CLASS: *const Class = std::ptr::null();

    REGISTER.call_once(|| {
        let superclass = Class::get("NSObject").unwrap();
        let mut decl = ClassDecl::new(
            "ManifoldOutputPresenterDelegate",
            superclass,
        )
        .expect("Failed to create delegate class");

        decl.add_ivar::<*mut c_void>("_context");

        unsafe {
            decl.add_method(
                objc::sel!(metalDisplayLink:needsUpdate:),
                presenter_callback
                    as extern "C" fn(
                        &objc::runtime::Object,
                        objc::runtime::Sel,
                        *mut objc::runtime::Object,
                        *mut objc::runtime::Object,
                    ),
            );
        }

        let cls = decl.register();
        unsafe {
            CLASS = cls;
        }
    });

    unsafe { &*CLASS }
}

// ─── Presenter context (shared with callback) ──────────────────────────

struct PresenterContext {
    /// IOSurface triple buffer bridge (Arc-shared with content thread).
    bridge: Arc<SharedTextureBridge>,
    /// Dedicated GPU device for the presenter (own command queue).
    device: manifold_gpu::GpuDevice,
    /// Blit render pipeline (fullscreen triangle + texture sample).
    pipeline: manifold_gpu::GpuRenderPipeline,
    /// Linear sampler for the blit.
    sampler: manifold_gpu::GpuSampler,
    /// Cached IOSurface-backed textures (reimported when generation changes).
    textures: [Option<manifold_gpu::GpuTexture>; SURFACE_COUNT],
    /// Last seen bridge generation.
    generation: u64,
    /// Output drawable dimensions (for aspect-fit calculation).
    surface_width: u32,
    surface_height: u32,
    /// Stop flag — set before invalidate to make in-flight callbacks no-op.
    stop: AtomicBool,
}

// ─── CAMetalDisplayLink callback ───────────────────────────────────────

extern "C" fn presenter_callback(
    this: &objc::runtime::Object,
    _sel: objc::runtime::Sel,
    _link: *mut objc::runtime::Object,
    update: *mut objc::runtime::Object,
) {
    let ctx = unsafe {
        let ptr: *mut c_void = *this.get_ivar("_context");
        &mut *(ptr as *mut PresenterContext)
    };

    if ctx.stop.load(Ordering::Acquire) {
        return;
    }

    // 1. Get drawable from update
    let drawable_ptr: *mut objc::runtime::Object =
        unsafe { objc::msg_send![update, drawable] };
    if drawable_ptr.is_null() {
        return;
    }

    // 2. Read latest IOSurface front_index
    let front = ctx.bridge.front_index() as usize;

    // 3. Reimport textures if bridge resized (generation changed)
    let bridge_gen = ctx.bridge.generation();
    if bridge_gen != ctx.generation {
        ctx.generation = bridge_gen;
        for i in 0..SURFACE_COUNT {
            ctx.textures[i] = Some(unsafe {
                ctx.bridge.import_texture_native(&ctx.device, i)
            });
        }
    }

    let source = match ctx.textures[front].as_ref() {
        Some(t) => t,
        None => return,
    };

    // 4. Wrap drawable and get its texture
    let gpu_drawable =
        unsafe { manifold_gpu::GpuDrawable::from_raw(drawable_ptr as *mut c_void) };
    let target = gpu_drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Rgba16Float);

    // 5. Aspect-fit viewport
    let src_w = ctx.bridge.width() as f32;
    let src_h = ctx.bridge.height() as f32;
    let dst_w = ctx.surface_width as f32;
    let dst_h = ctx.surface_height as f32;
    let source_aspect = src_w / src_h;
    let draw_aspect = dst_w / dst_h;
    let (fit_w, fit_h) = if source_aspect > draw_aspect {
        (dst_w, dst_w / source_aspect)
    } else {
        (dst_h * source_aspect, dst_h)
    };
    let fit_x = (dst_w - fit_w) * 0.5;
    let fit_y = (dst_h - fit_h) * 0.5;

    // 6. Encode blit, schedule present, commit — fully non-blocking.
    // CAMetalDisplayLink already provides vsync-aligned timing (the drawable
    // is delivered at vsync). No need for presentsWithTransaction or
    // waitUntilScheduled — those block the main thread and cause starvation
    // when the GPU is busy with content work at high FPS.
    let mut encoder = ctx.device.create_encoder("Output Present");
    encoder.draw_fullscreen_viewport(
        &ctx.pipeline,
        &target,
        &[
            manifold_gpu::GpuBinding::Texture {
                binding: 0,
                texture: source,
            },
            manifold_gpu::GpuBinding::Sampler {
                binding: 1,
                sampler: &ctx.sampler,
            },
        ],
        (fit_x, fit_y, fit_w, fit_h),
        manifold_gpu::GpuLoadAction::Clear,
        "Output Present",
    );
    encoder.present_drawable(&gpu_drawable);
    encoder.commit();
}

// ─── OutputPresenter ───────────────────────────────────────────────────

/// CAMetalDisplayLink-based output presenter.
///
/// Reads from an IOSurface triple buffer and presents to the output
/// window's CAMetalLayer at the display's exact refresh cadence.
/// Fully decoupled from the content thread's production rate.
pub struct OutputPresenter {
    display_link: *mut objc::runtime::Object,
    delegate: *mut objc::runtime::Object,
    context: *mut PresenterContext,
    /// Retained GpuSurface for configuration (resize, EDR).
    surface: manifold_gpu::GpuSurface,
}

// OutputPresenter is created and used only on the main thread.
// CAMetalDisplayLink is !Send+!Sync — this matches.
unsafe impl Send for OutputPresenter {}

impl OutputPresenter {
    /// Create and start a new output presenter.
    ///
    /// `surface` — CAMetalLayer attached to the output window (takes ownership)
    /// `bridge` — IOSurface triple buffer shared with the content thread
    ///
    /// The presenter starts immediately. The CAMetalDisplayLink fires on
    /// the main thread's run loop at the output display's refresh rate.
    pub fn new(
        surface: manifold_gpu::GpuSurface,
        bridge: Arc<SharedTextureBridge>,
    ) -> Option<Self> {
        // Check CAMetalDisplayLink availability (macOS 14+)
        let link_class = objc::runtime::Class::get("CAMetalDisplayLink");
        if link_class.is_none() {
            log::warn!(
                "[OutputPresenter] CAMetalDisplayLink not available \
                 (requires macOS 14+), falling back to direct present"
            );
            return None;
        }

        // Configure the output surface
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);
        surface.set_maximum_drawable_count(3);
        // CAMetalDisplayLink provides vsync-aligned drawable delivery —
        // no need for presentsWithTransaction (which blocks the main thread
        // via waitUntilScheduled and causes starvation at high FPS).
        surface.set_presents_with_transaction(false);

        // Create dedicated GPU device for the presenter (own command queue)
        let device = manifold_gpu::GpuDevice::new();
        let pipeline = Self::create_blit_pipeline(&device);
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });

        let context = Box::into_raw(Box::new(PresenterContext {
            bridge,
            device,
            pipeline,
            sampler,
            textures: [None, None, None],
            generation: u64::MAX, // force reimport on first callback
            surface_width: surface.width,
            surface_height: surface.height,
            stop: AtomicBool::new(false),
        }));

        // Create delegate instance
        let cls = delegate_class();
        let delegate: *mut objc::runtime::Object = unsafe {
            let alloc: *mut objc::runtime::Object =
                objc::msg_send![cls, alloc];
            let obj: *mut objc::runtime::Object =
                objc::msg_send![alloc, init];
            (*obj).set_ivar("_context", context as *mut c_void);
            obj
        };

        // Create CAMetalDisplayLink
        let layer_ptr = surface.raw_layer_ptr();
        let display_link: *mut objc::runtime::Object = unsafe {
            let alloc: *mut objc::runtime::Object =
                objc::msg_send![link_class.unwrap(), alloc];
            objc::msg_send![alloc, initWithMetalLayer: layer_ptr]
        };

        if display_link.is_null() {
            log::error!("[OutputPresenter] CAMetalDisplayLink creation failed");
            unsafe {
                drop(Box::from_raw(context));
            }
            return None;
        }

        // Set delegate
        unsafe {
            let _: () =
                objc::msg_send![display_link, setDelegate: delegate];
        }

        // Add to main run loop
        unsafe {
            let main_loop: *mut objc::runtime::Object =
                objc::msg_send![objc::class!(NSRunLoop), mainRunLoop];
            let _: () = objc::msg_send![
                display_link,
                addToRunLoop: main_loop
                forMode: NSRunLoopCommonModes
            ];
        }

        log::info!(
            "[OutputPresenter] Started — CAMetalDisplayLink on main thread, \
             surface {}x{}",
            surface.width,
            surface.height,
        );

        Some(Self {
            display_link,
            delegate,
            context,
            surface,
        })
    }

    /// Resize the output drawable (e.g. fullscreen toggle).
    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface.resize(width, height);
        let ctx = unsafe { &mut *self.context };
        ctx.surface_width = width;
        ctx.surface_height = height;
        log::info!(
            "[OutputPresenter] Resized to {}x{}",
            width,
            height,
        );
    }

    /// Pause or resume the presenter (e.g. during display retarget).
    pub fn set_paused(&self, paused: bool) {
        unsafe {
            let _: () =
                objc::msg_send![self.display_link, setPaused: paused];
        }
    }

    fn create_blit_pipeline(
        device: &manifold_gpu::GpuDevice,
    ) -> manifold_gpu::GpuRenderPipeline {
        let shader = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;
        device.create_render_pipeline(
            shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Output Presenter Blit",
        )
    }
}

impl Drop for OutputPresenter {
    fn drop(&mut self) {
        // Signal stop so any in-flight callback returns immediately.
        unsafe {
            (*self.context).stop.store(true, Ordering::Release);
        }
        // Invalidate — synchronous on main thread, no more callbacks after.
        unsafe {
            let _: () = objc::msg_send![self.display_link, invalidate];
        }
        // Free context (safe — no callbacks can access it after invalidate).
        unsafe {
            drop(Box::from_raw(self.context));
        }
        // Release ObjC objects.
        unsafe {
            objc_release(self.display_link as *mut c_void);
            objc_release(self.delegate as *mut c_void);
        }
        log::info!("[OutputPresenter] Stopped and cleaned up");
    }
}

/// Helper: release an ObjC object.
unsafe fn objc_release(ptr: *mut c_void) {
    if !ptr.is_null() {
        let _: () = objc::msg_send![ptr as *mut objc::runtime::Object, release];
    }
}
