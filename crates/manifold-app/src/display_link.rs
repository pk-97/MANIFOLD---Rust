//! CVDisplayLink-driven output presenter.
//!
//! Replaces the manually-paced presenter thread with a hardware-synchronized
//! callback from CoreVideo. The CVDisplayLink fires at the exact refresh cadence
//! of the target display, providing:
//!   - deterministic frame pacing (no sleep/spin jitter)
//!   - precise vsync timing via `outputTime.hostTime`
//!   - automatic cadence adaptation when the window moves between displays
//!   - OS-managed real-time priority thread (no manual SCHED_RR)
//!
//! Submission timing model (per review):
//!   callback fires → coarse sleep → tight spin until outputTime - margin
//!   → acquire drawable → read front_index → blit → present
//!
//! This ensures GPU work completes inside the compositor acceptance window
//! and latches the freshest content frame possible.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler,
    GpuSamplerDesc, GpuSurface, GpuTexture, GpuTextureFormat, GpuTextureUsage,
};

use crate::shared_texture::{SURFACE_COUNT, SharedTextureBridge};

// ─── CVDisplayLink FFI ──────────────────────────────────────────────────

type CVDisplayLinkRef = *mut c_void;

/// CVTimeStamp — timing information from CoreVideo.
/// `host_time` is in mach_absolute_time units.
#[repr(C)]
#[derive(Clone, Copy)]
struct CVTimeStamp {
    version: u32,
    video_time_scale: i32,
    video_time: i64,
    host_time: u64,
    rate_scalar: f64,
    video_refresh_period: i64,
    smpte_time: [u8; 24], // CVSMPTETime — opaque, we only use host_time
    flags: u64,
    reserved: u64,
}

type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
    display_link: CVDisplayLinkRef,
    in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    flags_in: u64,
    flags_out: *mut u64,
    context: *mut c_void,
) -> i32;

const K_CV_RETURN_SUCCESS: i32 = 0;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVDisplayLinkCreateWithActiveCGDisplays(out: *mut CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkSetCurrentCGDisplay(link: CVDisplayLinkRef, display_id: u32) -> i32;
    fn CVDisplayLinkSetOutputCallback(
        link: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        context: *mut c_void,
    ) -> i32;
    fn CVDisplayLinkStart(link: CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkStop(link: CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkRelease(link: CVDisplayLinkRef);
    fn CVDisplayLinkGetActualOutputVideoRefreshPeriod(link: CVDisplayLinkRef) -> f64;
}

// ─── Display ID extraction ──────────────────────────────────────────────

/// Get the CGDirectDisplayID for the monitor a window is currently on.
fn display_id_for_window(window: &winit::window::Window) -> u32 {
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let ns_view = match window.window_handle().unwrap().as_raw() {
        RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut objc::runtime::Object,
        _ => return 0,
    };

    unsafe {
        let ns_window: *mut objc::runtime::Object = msg_send![ns_view, window];
        if ns_window.is_null() {
            return 0;
        }
        let screen: *mut objc::runtime::Object = msg_send![ns_window, screen];
        if screen.is_null() {
            return 0;
        }
        let desc: *mut objc::runtime::Object = msg_send![screen, deviceDescription];
        if desc.is_null() {
            return 0;
        }
        let key: *mut objc::runtime::Object = msg_send![
            class!(NSString),
            stringWithUTF8String: c"NSScreenNumber".as_ptr()
        ];
        let display_id_obj: *mut objc::runtime::Object = msg_send![desc, objectForKey: key];
        if display_id_obj.is_null() {
            return 0;
        }
        msg_send![display_id_obj, unsignedIntValue]
    }
}

// ─── Send wrapper for raw pointers moved to cleanup threads ─────────────

/// Wrapper to send raw pointers to the cleanup thread in Drop impls.
/// SAFETY: CVDisplayLinkRef is a CoreFoundation object safe to stop/release
/// from any thread. Context pointers are heap-allocated and exclusively
/// owned by the cleanup thread after the stop flag is set.
struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
impl<T> SendPtr<T> {
    fn get(self) -> *mut T {
        self.0
    }
}

// ─── Presenter WGSL (same as NativeOutputPresenter) ─────────────────────

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

// ─── Presenter context ─────────────────────────────────────────────────

/// GPU resources for blitting IOSurface content to the output window's
/// CAMetalLayer. Shared between the main thread (windowed mode) and the
/// CVDisplayLink callback (fullscreen/Direct Display mode).
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

unsafe impl Send for PresenterContext {}

impl PresenterContext {
    /// Blit the latest IOSurface frame to the CAMetalLayer drawable.
    ///
    /// `use_transaction`: when true, uses commit_and_wait_scheduled + manual
    /// present for Core Animation transaction sync (windowed mode, main thread).
    /// When false, uses standard presentDrawable (fullscreen/Direct Display).
    fn present_frame(&mut self, use_transaction: bool) {
        // ── Bridge resize check (rare) ──
        let bridge_gen = self.bridge.generation();
        if bridge_gen != self.last_bridge_gen {
            self.last_bridge_gen = bridge_gen;
            self.native_textures = import_textures(&self.device, &self.bridge);
            let w = self.bridge.width();
            let h = self.bridge.height();
            if w != self.surface.width || h != self.surface.height {
                self.surface.resize(w, h);
            }
        }

        // ── Latch latest content frame ──
        let front = self.bridge.front_index() as usize;
        let Some(source) = self.native_textures[front].as_ref() else {
            return;
        };

        let Some(drawable) = self.surface.next_drawable() else {
            return;
        };

        // ── Blit ──
        let target = drawable.gpu_texture(GpuTextureFormat::Rgba16Float);
        let w = self.surface.width as f32;
        let h = self.surface.height as f32;

        let mut encoder = self.device.create_encoder("Output Present");
        encoder.draw_fullscreen_viewport(
            &self.pipeline,
            &target,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler: &self.sampler,
                },
            ],
            (0.0, 0.0, w, h),
            GpuLoadAction::DontCare,
            "Presenter Blit",
        );

        // ── Present ──
        if use_transaction {
            // Windowed (main thread): commit, wait for GPU to schedule the
            // blit, then present directly into the Core Animation transaction.
            encoder.commit_and_wait_scheduled();
            drawable.present_after_scheduled();
        } else {
            // Fullscreen (Direct Display): standard present on the command
            // buffer. Direct Display bypasses the compositor.
            encoder.present_drawable(&drawable);
            encoder.commit();
        }
    }
}

// ─── Fullscreen callback (Direct Display — presents every vsync) ───────

/// Callback context for fullscreen presenter. Heap-allocated, accessed
/// only from the serial CVDisplayLink callback thread.
struct FullscreenCallbackContext {
    presenter: PresenterContext,
    stop: AtomicBool,
}

unsafe impl Send for FullscreenCallbackContext {}

unsafe extern "C" fn fullscreen_present_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let ctx = unsafe { &mut *(context as *mut FullscreenCallbackContext) };
    if ctx.stop.load(Ordering::Acquire) {
        return K_CV_RETURN_SUCCESS;
    }
    // Must present every vsync to maintain Direct Display lock.
    objc::rc::autoreleasepool(|| {
        ctx.presenter.present_frame(false);
    });
    K_CV_RETURN_SUCCESS
}

// ─── Windowed callback (lightweight — just sets flag + request_redraw) ──

/// Callback context for windowed presenter. The CVDisplayLink only sets
/// a flag; the main thread does the actual blit + present.
struct WindowedCallbackContext {
    vsync_ready: Arc<AtomicBool>,
    window: Arc<winit::window::Window>,
    stop: AtomicBool,
}

unsafe impl Send for WindowedCallbackContext {}
unsafe impl Sync for WindowedCallbackContext {}

unsafe extern "C" fn windowed_vsync_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let ctx = unsafe { &*(context as *const WindowedCallbackContext) };
    if ctx.stop.load(Ordering::Acquire) {
        return K_CV_RETURN_SUCCESS;
    }
    ctx.vsync_ready.store(true, Ordering::Release);
    ctx.window.request_redraw();
    K_CV_RETURN_SUCCESS
}

// ─── Public API ─────────────────────────────────────────────────────────

/// Display-linked output presenter.
///
/// Two modes:
/// - **Fullscreen (Direct Display)**: CVDisplayLink callback does the full
///   blit + present every vsync. Required to maintain the Direct Display lock.
/// - **Windowed**: CVDisplayLink callback just sets a flag. The main thread
///   calls [`present_if_ready()`] to do the blit + present with
///   `presentsWithTransaction`, syncing with the WindowServer compositor.
enum PresenterMode {
    /// Fullscreen: callback owns the presenter and does the blit.
    Fullscreen {
        context: *mut FullscreenCallbackContext,
    },
    /// Windowed: main thread owns the presenter, callback just signals.
    Windowed {
        presenter: Box<PresenterContext>,
        vsync_ready: Arc<AtomicBool>,
        /// Raw pointer to the callback context (freed in Drop).
        callback_context: *mut WindowedCallbackContext,
    },
}

pub struct DisplayLinkPresenter {
    display_link: CVDisplayLinkRef,
    mode: PresenterMode,
    edr_headroom: Arc<AtomicU64>,
    current_display_id: u32,
}

// SAFETY: CVDisplayLinkRef is a CoreFoundation object safe to stop/release
// from any thread. Presenter resources are only accessed from one thread
// at a time (main thread for windowed, callback thread for fullscreen).
unsafe impl Send for DisplayLinkPresenter {}

impl DisplayLinkPresenter {
    pub fn new(
        _gpu_device: &GpuDevice,
        window: &winit::window::Window,
        window_arc: Option<Arc<winit::window::Window>>,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
        presentation: bool,
    ) -> Self {
        let presenter_device = GpuDevice::new();
        let proj_w = bridge.width();
        let proj_h = bridge.height();

        // displaySyncEnabled: true for fullscreen (Direct Display), false for windowed.
        let surface = presenter_device.create_surface(
            window,
            proj_w,
            proj_h,
            GpuTextureFormat::Rgba16Float,
            presentation,
        );
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);
        surface.set_maximum_drawable_count(3);
        // Windowed: presentsWithTransaction=true — the main thread does the
        // present inside the winit event loop where CA transactions exist.
        // Fullscreen: false — Direct Display bypasses the compositor.
        surface.set_presents_with_transaction(!presentation);

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
        let edr = Arc::new(AtomicU64::new(edr_headroom.to_bits()));

        let presenter_ctx = PresenterContext {
            device: presenter_device,
            pipeline,
            sampler,
            surface,
            bridge,
            native_textures,
            last_bridge_gen: bridge_gen,
            edr_headroom: Arc::clone(&edr),
        };

        // Create CVDisplayLink.
        let display_id = display_id_for_window(window);
        let mut display_link: CVDisplayLinkRef = std::ptr::null_mut();
        unsafe {
            let ret = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
            assert!(ret == K_CV_RETURN_SUCCESS && !display_link.is_null());
            if display_id != 0 {
                CVDisplayLinkSetCurrentCGDisplay(display_link, display_id);
            }
        }

        let mode = if presentation {
            // Fullscreen: callback owns the presenter and does the blit.
            let ctx = Box::into_raw(Box::new(FullscreenCallbackContext {
                presenter: presenter_ctx,
                stop: AtomicBool::new(false),
            }));
            unsafe {
                CVDisplayLinkSetOutputCallback(
                    display_link,
                    fullscreen_present_callback,
                    ctx as *mut c_void,
                );
            }
            PresenterMode::Fullscreen { context: ctx }
        } else {
            // Windowed: lightweight callback, main thread does the blit.
            let vsync_ready = Arc::new(AtomicBool::new(false));
            let win_arc = window_arc.expect("window_arc required for windowed presenter");
            let cb_ctx = Box::into_raw(Box::new(WindowedCallbackContext {
                vsync_ready: Arc::clone(&vsync_ready),
                window: win_arc,
                stop: AtomicBool::new(false),
            }));
            unsafe {
                CVDisplayLinkSetOutputCallback(
                    display_link,
                    windowed_vsync_callback,
                    cb_ctx as *mut c_void,
                );
            }
            PresenterMode::Windowed {
                presenter: Box::new(presenter_ctx),
                vsync_ready,
                callback_context: cb_ctx,
            }
        };

        unsafe {
            let ret = CVDisplayLinkStart(display_link);
            assert!(ret == K_CV_RETURN_SUCCESS);
        }

        log::info!(
            "[DisplayLink] Started for display {display_id}, \
             mode={}, drawable={}x{} Rgba16Float",
            if presentation {
                "fullscreen"
            } else {
                "windowed"
            },
            proj_w,
            proj_h,
        );

        Self {
            display_link,
            mode,
            edr_headroom: edr,
            current_display_id: display_id,
        }
    }

    /// Present the latest content frame if a vsync signal is pending.
    /// **Windowed mode only** — called from the main thread's event loop.
    /// Uses presentsWithTransaction to sync with the WindowServer compositor.
    /// In fullscreen mode this is a no-op (the callback does the present).
    pub fn present_if_ready(&mut self) {
        if let PresenterMode::Windowed {
            ref mut presenter,
            ref vsync_ready,
            ..
        } = self.mode
            && vsync_ready.swap(false, Ordering::AcqRel)
        {
            objc::rc::autoreleasepool(|| {
                presenter.present_frame(true);
            });
        }
    }

    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom
            .store(headroom.to_bits(), Ordering::Relaxed);
    }

    /// Retarget the presenter if the window moved to a different display.
    /// Returns `true` if the display actually changed (new display ID).
    pub fn retarget_if_needed(&mut self, window: &winit::window::Window) -> bool {
        let new_id = display_id_for_window(window);
        if new_id == 0 || new_id == self.current_display_id {
            return false;
        }
        unsafe {
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
        }
        log::info!(
            "[DisplayLink] Retargeted: display {} → {}",
            self.current_display_id,
            new_id,
        );
        self.current_display_id = new_id;
        true
    }
}

impl Drop for DisplayLinkPresenter {
    fn drop(&mut self) {
        // Signal the callback to become a no-op.
        match &self.mode {
            PresenterMode::Fullscreen { context } => {
                unsafe { &**context }.stop.store(true, Ordering::Release);
            }
            PresenterMode::Windowed {
                callback_context, ..
            } => {
                unsafe { &**callback_context }
                    .stop
                    .store(true, Ordering::Release);
            }
        }

        // Move blocking cleanup off the main thread.
        let dl = SendPtr(self.display_link);
        match &self.mode {
            PresenterMode::Fullscreen { context } => {
                let ctx = SendPtr(*context);
                std::thread::spawn(move || unsafe {
                    let dl = dl.get();
                    CVDisplayLinkStop(dl);
                    CVDisplayLinkRelease(dl);
                    drop(Box::from_raw(ctx.get()));
                });
            }
            PresenterMode::Windowed {
                callback_context, ..
            } => {
                let ctx = SendPtr(*callback_context);
                std::thread::spawn(move || unsafe {
                    let dl = dl.get();
                    CVDisplayLinkStop(dl);
                    CVDisplayLinkRelease(dl);
                    drop(Box::from_raw(ctx.get()));
                });
                // Note: PresenterContext (owned by the enum) is dropped
                // normally when the DisplayLinkPresenter is dropped.
            }
        }
    }
}

// ─── Texture import (shared with NativeOutputPresenter) ─────────────────

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

// ═══════════════════════════════════════════════════════════════════════
// UiDisplayLink — vsync-aligned render trigger for the UI thread
// ═══════════════════════════════════════════════════════════════════════

/// Context for the UI display link callback. Heap-allocated, accessed only
/// from the serial CVDisplayLink callback thread.
struct UiDisplayLinkContext {
    vsync_ready: Arc<AtomicBool>,
    window: Arc<winit::window::Window>,
    stop: AtomicBool,
}

unsafe impl Send for UiDisplayLinkContext {}
unsafe impl Sync for UiDisplayLinkContext {}

/// CVDisplayLink callback for the UI thread.
/// Sets the vsync flag and wakes the winit event loop via request_redraw.
unsafe extern "C" fn ui_display_link_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let ctx = unsafe { &*(context as *const UiDisplayLinkContext) };
    if ctx.stop.load(Ordering::Acquire) {
        return K_CV_RETURN_SUCCESS;
    }
    ctx.vsync_ready.store(true, Ordering::Release);
    ctx.window.request_redraw();
    K_CV_RETURN_SUCCESS
}

/// CVDisplayLink-driven vsync signal for the UI thread.
///
/// Fires at the MacBook display's exact refresh cadence and wakes the winit
/// event loop via `request_redraw`. The event loop checks `vsync_ready()`
/// to decide when to render, replacing the free-running `FrameTimer`.
///
/// This aligns UI submission to the MacBook's vsync, reducing near-miss
/// frame drops caused by event loop scheduling jitter.
pub struct UiDisplayLink {
    display_link: CVDisplayLinkRef,
    context: *mut UiDisplayLinkContext,
    vsync_ready: Arc<AtomicBool>,
    /// Current display ID — compared on screen change to detect retargeting.
    current_display_id: u32,
}

unsafe impl Send for UiDisplayLink {}

impl UiDisplayLink {
    /// Create a CVDisplayLink bound to the display the given window is on.
    pub fn new(window: Arc<winit::window::Window>) -> Self {
        let display_id = display_id_for_window(&window);
        let vsync_ready = Arc::new(AtomicBool::new(false));

        let context = Box::into_raw(Box::new(UiDisplayLinkContext {
            vsync_ready: Arc::clone(&vsync_ready),
            window,
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
                        "[UiDisplayLink] SetCurrentCGDisplay failed for display \
                         {display_id} (ret={ret}), using default"
                    );
                }
            }

            let ret = CVDisplayLinkSetOutputCallback(
                display_link,
                ui_display_link_callback,
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

        let refresh = unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(display_link) };
        log::info!(
            "[UiDisplayLink] Started for display {display_id}, \
             refresh={:.2}ms ({:.1}Hz)",
            refresh * 1000.0,
            if refresh > 0.0 { 1.0 / refresh } else { 0.0 },
        );

        Self {
            display_link,
            context,
            vsync_ready,
            current_display_id: display_id,
        }
    }

    /// Check and consume the vsync signal. Returns true once per display vsync.
    pub fn vsync_ready(&self) -> bool {
        self.vsync_ready.swap(false, Ordering::AcqRel)
    }

    /// Non-destructive check: has the display link callback fired since last
    /// consumed by `vsync_ready()`? Used to confirm the display link is alive
    /// after a retarget without consuming the signal.
    pub fn is_alive(&self) -> bool {
        self.vsync_ready.load(Ordering::Acquire)
    }

    /// Retarget the display link if the window moved to a different display.
    ///
    /// NEVER calls CVDisplayLinkStop — that blocks waiting for the in-flight
    /// callback, which can deadlock during macOS modal drag loops (the callback's
    /// request_redraw may need the main thread, which is blocked in Stop).
    ///
    /// Instead: set the stop flag (callback becomes a no-op), retarget in-place
    /// with SetCurrentCGDisplay (safe to call while running per Apple docs),
    /// then clear the flag. At most 1 vsync signal is missed.
    /// Retarget the display link if the window moved to a different display.
    /// Returns `true` if the display actually changed (new display ID).
    pub fn retarget_if_needed(&mut self, window: &winit::window::Window) -> bool {
        let new_id = display_id_for_window(window);
        if new_id == 0 || new_id == self.current_display_id {
            return false;
        }
        let old_refresh =
            unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link) };
        unsafe {
            let ctx = &*self.context;
            ctx.stop.store(true, Ordering::Release);
            // Fence ensures the stop flag is visible to the callback thread
            // before we change the display target.
            std::sync::atomic::fence(Ordering::SeqCst);
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
            ctx.stop.store(false, Ordering::Release);
        }
        let new_refresh =
            unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link) };
        log::info!(
            "[UiDisplayLink] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id,
            new_id,
            if old_refresh > 0.0 {
                1.0 / old_refresh
            } else {
                0.0
            },
            if new_refresh > 0.0 {
                1.0 / new_refresh
            } else {
                0.0
            },
        );
        self.current_display_id = new_id;
        true
    }
}

impl Drop for UiDisplayLink {
    fn drop(&mut self) {
        // Signal the callback to become a no-op IMMEDIATELY.
        unsafe {
            (*self.context).stop.store(true, Ordering::Release);
        }

        // Move blocking cleanup off the main thread. CVDisplayLinkStop blocks
        // until the in-flight callback finishes, and the callback calls
        // request_redraw() which may need the main thread — blocking the
        // main thread here deadlocks.
        let dl = SendPtr(self.display_link);
        let ctx = SendPtr(self.context);
        std::thread::spawn(move || unsafe {
            let dl = dl.get();
            let ctx = ctx.get();
            CVDisplayLinkStop(dl);
            CVDisplayLinkRelease(dl);
            drop(Box::from_raw(ctx));
        });
    }
}
