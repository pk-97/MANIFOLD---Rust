//! Unified display link — one CVDisplayLink per physical display driving
//! all consumers in a deterministic order:
//!
//!   1. Notify content thread condvar (wakes frame production)
//!   2. Present output frame (blit latest IOSurface → CAMetalLayer)
//!   3. Signal UI thread redraw (set flag + request_redraw)
//!
//! This eliminates the phase-race judder caused by three independent
//! CVDisplayLinks firing at unpredictable offsets on the same display.
//!
//! The presenter is attached/detached dynamically when the output window
//! opens/closes. When no presenter is attached, steps 2 is skipped.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler,
    GpuSamplerDesc, GpuSurface, GpuTexture, GpuTextureFormat, GpuTextureUsage,
    // CVDisplayLink FFI (consolidated in manifold-gpu)
    CVDisplayLinkRef, CVTimeStamp, K_CV_RETURN_SUCCESS, SendPtr,
    CVDisplayLinkCreateWithActiveCGDisplays, CVDisplayLinkSetCurrentCGDisplay,
    CVDisplayLinkSetOutputCallback, CVDisplayLinkStart, CVDisplayLinkStop,
    CVDisplayLinkRelease, CVDisplayLinkGetActualOutputVideoRefreshPeriod,
    display_id_for_window, hz_from_timestamp,
    GpuVsyncSignal,
};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

// ─── Presenter WGSL ────────────────────────────────────────────────────

const PRESENTER_WGSL: &str = r#"
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

// ─── Presenter context (heap-allocated, accessed from callback) ─────────

struct PresenterContext {
    device: GpuDevice,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    surface: GpuSurface,
    bridge: Arc<SharedTextureBridge>,
    native_textures: [Option<GpuTexture>; SURFACE_COUNT],
    last_bridge_gen: u64,
    #[allow(dead_code)]
    edr_headroom: Arc<AtomicU64>,
}

// SAFETY: PresenterContext is only accessed from the serial CVDisplayLink
// callback thread. The fields it contains (GpuDevice, GpuSurface, etc.)
// wrap ObjC/Metal objects that are safe to use from any single thread.
unsafe impl Send for PresenterContext {}

impl PresenterContext {
    fn present_for_vsync(&mut self) {
        // ── Bridge resize check (rare) ──
        let bridge_gen = self.bridge.generation();
        if bridge_gen != self.last_bridge_gen {
            self.last_bridge_gen = bridge_gen;
            self.reimport_textures();
            self.sync_surface_to_bridge();
        }

        // ── Latch latest content frame ──
        // Always present on every callback, even if front_index hasn't changed.
        // In fullscreen presentation mode, macOS engages Direct Display
        // (Direct-to-Screen), bypassing the WindowServer compositor. This
        // optimization requires a present on every hardware vsync to maintain
        // the lock. Skipping presents causes WindowServer to thrash between
        // Direct Display and composited mode — and that thrashing propagates
        // to ALL displays, causing UI drops on the MacBook.
        let front = self.bridge.front_index() as usize;

        let Some(source) = self.native_textures[front].as_ref() else {
            return;
        };

        // ── Acquire drawable ──
        let Some(drawable) = self.surface.next_drawable() else {
            return; // Skip frame — don't stall the callback
        };

        // ── Blit + present ──
        let target = drawable.gpu_texture(GpuTextureFormat::Rgba16Float);
        let w = self.surface.width as f32;
        let h = self.surface.height as f32;

        let mut encoder = self.device.create_encoder("Output Present");
        encoder.draw_fullscreen_viewport(
            &self.pipeline,
            &target,
            &[
                GpuBinding::Texture { binding: 0, texture: source },
                GpuBinding::Sampler { binding: 1, sampler: &self.sampler },
            ],
            (0.0, 0.0, w, h),
            GpuLoadAction::DontCare,
            "Presenter Blit",
        );
        encoder.present_drawable(&drawable);
        encoder.commit();
    }

    fn reimport_textures(&mut self) {
        self.native_textures = import_textures(&self.device, &self.bridge);
    }

    fn sync_surface_to_bridge(&mut self) {
        let w = self.bridge.width();
        let h = self.bridge.height();
        if w != self.surface.width || h != self.surface.height {
            self.surface.resize(w, h);
        }
    }
}

// ─── Unified callback context ──────────────────────────────────────────

struct UnifiedContext {
    /// Content thread notification — shared with GpuVsyncWaiter.
    content_signal: GpuVsyncSignal,

    /// Presenter (null = no output window). Atomically attached/detached
    /// from the UI thread while the callback is running.
    presenter: AtomicPtr<PresenterContext>,

    /// UI thread vsync signal.
    ui_vsync_ready: Arc<AtomicBool>,
    ui_window: Arc<winit::window::Window>,

    /// Shutdown flag — callback becomes a no-op when set.
    stop: AtomicBool,
}

unsafe impl Send for UnifiedContext {}
unsafe impl Sync for UnifiedContext {}

// ─── Unified CVDisplayLink callback ────────────────────────────────────

unsafe extern "C" fn unified_vsync_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let ctx = unsafe { &*(context as *const UnifiedContext) };
    if ctx.stop.load(Ordering::Acquire) {
        return K_CV_RETURN_SUCCESS;
    }

    let hz = hz_from_timestamp(unsafe { &*in_output_time });

    // ── Lightweight signals FIRST (nanoseconds) ──
    // These must complete before the potentially-slow presenter work.
    // If the presenter blocks (e.g. nextDrawable during fullscreen
    // transition), both the content thread and UI thread still get
    // their signals and can proceed normally.

    // 1. Notify content thread (lock + increment + notify_one).
    ctx.content_signal.notify_vsync(hz);

    // 2. Signal UI thread redraw.
    ctx.ui_vsync_ready.store(true, Ordering::Release);
    ctx.ui_window.request_redraw();

    // ── Heavy work LAST (may block on drawable acquisition) ──

    // 3. Present output frame (if presenter is attached).
    //    Reads the current front_index and blits to CAMetalLayer.
    //    Always runs every vsync — required for Direct Display lock.
    //    nextDrawable can block up to 1s during fullscreen transitions,
    //    but content thread + UI already got their signals above.
    let presenter_ptr = ctx.presenter.load(Ordering::Acquire);
    if !presenter_ptr.is_null() {
        objc::rc::autoreleasepool(|| {
            let presenter = unsafe { &mut *presenter_ptr };
            presenter.present_for_vsync();
        });
    }

    K_CV_RETURN_SUCCESS
}

// ─── Public API ────────────────────────────────────────────────────────

/// Unified display link — one CVDisplayLink driving content thread,
/// output presenter, and UI thread from a single callback.
///
/// Eliminates phase-race judder between independent CVDisplayLinks
/// on the same physical display.
pub struct UnifiedDisplayLink {
    display_link: CVDisplayLinkRef,
    context: *mut UnifiedContext,
    vsync_ready: Arc<AtomicBool>,
    edr_headroom: Arc<AtomicU64>,
    current_display_id: u32,
}

unsafe impl Send for UnifiedDisplayLink {}

impl UnifiedDisplayLink {
    /// Create a unified display link targeting the given window's display.
    ///
    /// `content_signal` must be a headless `GpuVsyncSignal` — the unified
    /// callback calls `notify_vsync()` on it to wake the content thread.
    pub fn new(
        window: Arc<winit::window::Window>,
        content_signal: GpuVsyncSignal,
    ) -> Self {
        let display_id = display_id_for_window(window.as_ref());
        let vsync_ready = Arc::new(AtomicBool::new(false));
        let edr_headroom = Arc::new(AtomicU64::new(1.0_f64.to_bits()));

        let context = Box::into_raw(Box::new(UnifiedContext {
            content_signal,
            presenter: AtomicPtr::new(std::ptr::null_mut()),
            ui_vsync_ready: Arc::clone(&vsync_ready),
            ui_window: window,
            stop: AtomicBool::new(false),
        }));

        let mut display_link: CVDisplayLinkRef = std::ptr::null_mut();
        unsafe {
            let ret = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
            assert!(
                ret == K_CV_RETURN_SUCCESS && !display_link.is_null(),
                "CVDisplayLinkCreateWithActiveCGDisplays failed (ret={ret})"
            );

            if display_id != 0 {
                let ret = CVDisplayLinkSetCurrentCGDisplay(display_link, display_id);
                if ret != K_CV_RETURN_SUCCESS {
                    log::warn!(
                        "[UnifiedDisplayLink] SetCurrentCGDisplay failed for display \
                         {display_id} (ret={ret}), using default"
                    );
                }
            }

            let ret = CVDisplayLinkSetOutputCallback(
                display_link,
                unified_vsync_callback,
                context as *mut c_void,
            );
            assert!(
                ret == K_CV_RETURN_SUCCESS,
                "CVDisplayLinkSetOutputCallback failed (ret={ret})"
            );

            let ret = CVDisplayLinkStart(display_link);
            assert!(
                ret == K_CV_RETURN_SUCCESS,
                "CVDisplayLinkStart failed (ret={ret})"
            );
        }

        log::info!(
            "[UnifiedDisplayLink] Started for display {display_id}"
        );

        Self {
            display_link,
            context,
            vsync_ready,
            edr_headroom,
            current_display_id: display_id,
        }
    }

    /// Create a waiter handle for the content thread.
    /// The waiter blocks on the same condvar that the unified callback notifies.
    pub fn create_content_waiter(&self) -> manifold_gpu::GpuVsyncWaiter {
        unsafe { &*self.context }.content_signal.create_waiter()
    }

    /// Check and consume the vsync signal. Returns true once per display vsync.
    pub fn vsync_ready(&self) -> bool {
        self.vsync_ready.swap(false, Ordering::AcqRel)
    }

    /// Attach a presenter for the output window. The callback will start
    /// presenting on the next vsync. Any previously attached presenter is dropped.
    pub fn attach_presenter(
        &self,
        _gpu_device: &GpuDevice,
        window: &winit::window::Window,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
        presentation: bool,
    ) {
        // Create presenter resources on a dedicated device (separate command queue).
        let presenter_device = GpuDevice::new();

        let proj_w = bridge.width();
        let proj_h = bridge.height();

        let surface = presenter_device.create_surface(
            window,
            proj_w,
            proj_h,
            GpuTextureFormat::Rgba16Float,
            presentation, // display-sync only in fullscreen/presentation mode
        );
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);
        surface.set_maximum_drawable_count(3);
        surface.set_presents_with_transaction(false);

        let pipeline = presenter_device.create_render_pipeline(
            PRESENTER_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba16Float,
            None,
            "Presenter Blit",
        );

        let sampler = presenter_device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mag_filter: GpuFilterMode::Nearest,
            ..Default::default()
        });

        let bridge_gen = bridge.generation();
        let native_textures = import_textures(&presenter_device, &bridge);
        let edr = Arc::clone(&self.edr_headroom);
        edr.store(edr_headroom.to_bits(), Ordering::Relaxed);

        let new_ctx = Box::into_raw(Box::new(PresenterContext {
            device: presenter_device,
            pipeline,
            sampler,
            surface,
            bridge,
            native_textures,
            last_bridge_gen: bridge_gen,
            edr_headroom: edr,
        }));

        // Atomically swap in the new presenter.
        let old = unsafe { &*self.context }
            .presenter
            .swap(new_ctx, Ordering::AcqRel);

        log::info!(
            "[UnifiedDisplayLink] Presenter attached: {}x{} Rgba16Float, \
             displaySync={}",
            proj_w, proj_h, presentation,
        );

        // Drop the old presenter if one was attached.
        if !old.is_null() {
            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Detach the presenter. The callback will stop presenting on the next vsync.
    pub fn detach_presenter(&self) {
        let old = unsafe { &*self.context }
            .presenter
            .swap(std::ptr::null_mut(), Ordering::AcqRel);

        if !old.is_null() {
            log::info!("[UnifiedDisplayLink] Presenter detached");
            // The callback might be mid-flight using the old pointer right now.
            // The swap to null means the NEXT callback won't use it.
            // Wait briefly to ensure the current callback (if any) finishes
            // before dropping the PresenterContext. Called from the UI thread
            // during output window close, so a brief delay is acceptable.
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(unsafe { Box::from_raw(old) });
        }
    }

    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom.store(headroom.to_bits(), Ordering::Relaxed);
    }

    /// Retarget the display link if the window moved to a different display.
    ///
    /// Safe to call while the callback is running (per Apple docs).
    /// The callback might fire one frame at the old display's timing.
    pub fn retarget_if_needed(&mut self, window: &winit::window::Window) {
        let new_id = display_id_for_window(window);
        if new_id == 0 || new_id == self.current_display_id {
            return;
        }
        let old_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        unsafe {
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
        }
        let new_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        log::info!(
            "[UnifiedDisplayLink] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id, new_id,
            if old_refresh > 0.0 { 1.0 / old_refresh } else { 0.0 },
            if new_refresh > 0.0 { 1.0 / new_refresh } else { 0.0 },
        );
        self.current_display_id = new_id;
    }

    /// Shut down the content thread's vsync signal.
    /// Must be called before joining the content thread.
    pub fn shutdown_content_signal(&self) {
        unsafe { &*self.context }.content_signal.shutdown();
    }

    /// Current display ID this link is targeting.
    #[allow(dead_code)]
    pub fn current_display_id(&self) -> u32 {
        self.current_display_id
    }
}

impl Drop for UnifiedDisplayLink {
    fn drop(&mut self) {
        // Signal callback to become a no-op.
        unsafe { &*self.context }.stop.store(true, Ordering::Release);

        // Shut down the content signal so the content thread unblocks.
        self.shutdown_content_signal();

        // Move blocking cleanup off the main thread.
        let dl = SendPtr(self.display_link);
        let ctx = SendPtr(self.context);
        std::thread::spawn(move || unsafe {
            let dl = dl.get();
            let ctx = ctx.get();
            CVDisplayLinkStop(dl);
            CVDisplayLinkRelease(dl);
            // Drop presenter if still attached.
            let ctx_ref = &*ctx;
            let presenter = ctx_ref.presenter.load(Ordering::Acquire);
            if !presenter.is_null() {
                drop(Box::from_raw(presenter));
            }
            drop(Box::from_raw(ctx));
        });
    }
}

// ─── Texture import ────────────────────────────────────────────────────

fn import_textures(
    device: &GpuDevice,
    bridge: &SharedTextureBridge,
) -> [Option<GpuTexture>; SURFACE_COUNT] {
    let width = bridge.width();
    let height = bridge.height();

    std::array::from_fn(|i| {
        let io_surface_ref = bridge.raw_io_surface(i);
        Some(unsafe {
            device.create_texture_from_io_surface(
                io_surface_ref,
                width,
                height,
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::SHADER_READ,
            )
        })
    })
}
