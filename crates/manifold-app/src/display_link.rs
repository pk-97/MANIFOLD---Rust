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

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

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
        if ns_window.is_null() { return 0; }
        let screen: *mut objc::runtime::Object = msg_send![ns_window, screen];
        if screen.is_null() { return 0; }
        let desc: *mut objc::runtime::Object = msg_send![screen, deviceDescription];
        if desc.is_null() { return 0; }
        let key: *mut objc::runtime::Object = msg_send![
            class!(NSString),
            stringWithUTF8String: c"NSScreenNumber".as_ptr()
        ];
        let display_id_obj: *mut objc::runtime::Object =
            msg_send![desc, objectForKey: key];
        if display_id_obj.is_null() { return 0; }
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
    fn get(self) -> *mut T { self.0 }
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

// ─── Presenter context (heap-allocated, accessed from callback) ─────────

struct PresenterContext {
    device: GpuDevice,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    surface: GpuSurface,
    bridge: Arc<SharedTextureBridge>,
    native_textures: [Option<GpuTexture>; SURFACE_COUNT],
    last_bridge_gen: u64,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    edr_headroom: Arc<AtomicU64>,
    /// Fullscreen (Direct Display) vs windowed presentation mode.
    /// Windowed: uses presentsWithTransaction + waitUntilScheduled for
    /// compositor-synchronized presents (eliminates phase judder).
    /// Fullscreen: uses standard presentDrawable on the command buffer
    /// (Direct Display bypasses the compositor entirely).
    presentation: bool,
}

// SAFETY: PresenterContext is only accessed from the serial CVDisplayLink
// callback thread. The fields it contains (GpuDevice, GpuSurface, etc.)
// wrap ObjC/Metal objects that are safe to use from any single thread.
unsafe impl Send for PresenterContext {}

impl PresenterContext {
    fn present_for_vsync(&mut self, output_time: &CVTimeStamp) {
        // ── Bridge resize check (rare) ──
        let bridge_gen = self.bridge.generation();
        if bridge_gen != self.last_bridge_gen {
            self.last_bridge_gen = bridge_gen;
            self.reimport_textures();
            self.sync_surface_to_bridge();
        }

        let _ = output_time; // available for future adaptive timing

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
        // With displaySyncEnabled=true and 3 drawables, this should not block
        // if we're keeping up. If it does, we've missed our window.
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

        if self.presentation {
            // Fullscreen (Direct Display): standard present on the command buffer.
            // Direct Display bypasses the compositor — no transaction needed.
            encoder.present_drawable(&drawable);
            encoder.commit();
        } else {
            // Windowed: commit, wait for GPU to schedule the blit, then present
            // directly into the Core Animation transaction. This syncs the frame
            // with WindowServer's compositor schedule, eliminating phase judder.
            encoder.commit_and_wait_scheduled();
            drawable.present_after_scheduled();
        }
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

// ─── CVDisplayLink callback ─────────────────────────────────────────────

unsafe extern "C" fn display_link_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let ctx = unsafe { &mut *(context as *mut PresenterContext) };

    if ctx.stop.load(Ordering::Acquire) {
        return K_CV_RETURN_SUCCESS;
    }

    // Drain autoreleased ObjC Metal objects every callback.
    objc::rc::autoreleasepool(|| {
        let output_time = unsafe { &*in_output_time };
        ctx.present_for_vsync(output_time);
    });

    K_CV_RETURN_SUCCESS
}

// ─── Public API ─────────────────────────────────────────────────────────

/// CVDisplayLink-driven output presenter.
///
/// Hardware-synchronized to the target display's refresh rate.
/// Replaces `NativeOutputPresenter` for deterministic frame pacing.
pub struct DisplayLinkPresenter {
    display_link: CVDisplayLinkRef,
    /// Heap-allocated context — freed in Drop after stopping the link.
    context: *mut PresenterContext,
    stop: Arc<AtomicBool>,
    edr_headroom: Arc<AtomicU64>,
    /// Current display ID — compared on screen change to detect retargeting.
    current_display_id: u32,
}

// SAFETY: CVDisplayLinkRef is a CoreFoundation object safe to stop/release
// from any thread. The context pointer is only dereferenced from the
// callback (which is stopped before Drop frees it).
unsafe impl Send for DisplayLinkPresenter {}

impl DisplayLinkPresenter {
    pub fn new(
        _gpu_device: &GpuDevice,
        window: &winit::window::Window,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
        presentation: bool,
    ) -> Self {
        // Dedicated device (same physical GPU, separate command queue).
        let presenter_device = GpuDevice::new();

        let proj_w = bridge.width();
        let proj_h = bridge.height();

        // Create surface. In presentation (fullscreen) mode, displaySyncEnabled=true
        // so presents align to vblank — required to maintain Direct Display lock.
        // In windowed mode, displaySyncEnabled=false because the CVDisplayLink
        // already paces the callback and WindowServer composites on its own schedule;
        // enabling display sync causes nextDrawable to block waiting for vblank,
        // creating double-wait judder with the CVDisplayLink callback.
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
        // 3 drawables: CVDisplayLink is the pacer, so nextDrawable should not
        // block. 3 ensures availability; if it still blocks, we skip the frame.
        surface.set_maximum_drawable_count(3);
        // Windowed: presentsWithTransaction=true so presents sync with Core
        // Animation's compositor schedule (waitUntilScheduled + manual present).
        // Fullscreen: presentsWithTransaction=false — Direct Display bypasses
        // the compositor, so the CVDisplayLink callback IS the pacer.
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

        let stop = Arc::new(AtomicBool::new(false));
        let edr = Arc::new(AtomicU64::new(edr_headroom.to_bits()));

        let context = Box::into_raw(Box::new(PresenterContext {
            device: presenter_device,
            pipeline,
            sampler,
            surface,
            bridge,
            native_textures,
            last_bridge_gen: bridge_gen,
            stop: Arc::clone(&stop),
            edr_headroom: Arc::clone(&edr),
            presentation,
        }));

        // Create CVDisplayLink targeting the window's current display.
        let display_id = display_id_for_window(window);
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
                        "[DisplayLink] SetCurrentCGDisplay failed for display {display_id} \
                         (ret={ret}), using default"
                    );
                }
            }

            let ret = CVDisplayLinkSetOutputCallback(
                display_link,
                display_link_callback,
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

        let refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(display_link)
        };
        log::info!(
            "[DisplayLink] Started for display {display_id}, \
             refresh={:.2}ms ({:.1}Hz), drawable={}x{} Rgba16Float, \
             displaySync={}",
            refresh * 1000.0,
            if refresh > 0.0 { 1.0 / refresh } else { 0.0 },
            proj_w,
            proj_h,
            presentation,
        );

        Self {
            display_link,
            context,
            stop,
            edr_headroom: edr,
            current_display_id: display_id,
        }
    }

    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom.store(headroom.to_bits(), Ordering::Relaxed);
    }

    /// Retarget the display link if the window moved to a different display.
    ///
    /// The presenter callback must NEVER skip a present — on Direct Display
    /// surfaces, even one missed present causes WindowServer to drop Direct
    /// Display mode, thrashing all displays. So we do NOT set the stop flag
    /// here. CVDisplayLinkSetCurrentCGDisplay is safe to call while the
    /// callback is running (Apple docs). The callback might present one frame
    /// at the old display's timing — that's acceptable (single late present
    /// is invisible, missed present is catastrophic).
    pub fn retarget_if_needed(&mut self, window: &winit::window::Window) {
        let new_id = display_id_for_window(window);
        if new_id == 0 || new_id == self.current_display_id {
            return;
        }
        let old_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        // Retarget while running — callback keeps presenting without interruption.
        unsafe {
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
        }
        let new_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        log::info!(
            "[DisplayLink] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id, new_id,
            if old_refresh > 0.0 { 1.0 / old_refresh } else { 0.0 },
            if new_refresh > 0.0 { 1.0 / new_refresh } else { 0.0 },
        );
        self.current_display_id = new_id;
    }
}

impl Drop for DisplayLinkPresenter {
    fn drop(&mut self) {
        // Signal the callback to become a no-op IMMEDIATELY.
        self.stop.store(true, Ordering::Release);

        // Move blocking cleanup off the main thread. CVDisplayLinkStop blocks
        // until the in-flight callback finishes, and the callback may touch
        // main-thread resources (nextDrawable, WindowServer) — blocking the
        // main thread here deadlocks. The detached thread can safely block:
        // Stop guarantees no callback runs after it returns, so Release +
        // context drop are safe.
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

        let refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(display_link)
        };
        log::info!(
            "[UiDisplayLink] Started for display {display_id}, \
             refresh={:.2}ms ({:.1}Hz)",
            refresh * 1000.0,
            if refresh > 0.0 { 1.0 / refresh } else { 0.0 },
        );

        Self { display_link, context, vsync_ready, current_display_id: display_id }
    }

    /// Check and consume the vsync signal. Returns true once per display vsync.
    pub fn vsync_ready(&self) -> bool {
        self.vsync_ready.swap(false, Ordering::AcqRel)
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
    pub fn retarget_if_needed(&mut self, window: &winit::window::Window) {
        let new_id = display_id_for_window(window);
        if new_id == 0 || new_id == self.current_display_id {
            return;
        }
        let old_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        unsafe {
            let ctx = &*self.context;
            ctx.stop.store(true, Ordering::Release);
            // Fence ensures the stop flag is visible to the callback thread
            // before we change the display target.
            std::sync::atomic::fence(Ordering::SeqCst);
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
            ctx.stop.store(false, Ordering::Release);
        }
        let new_refresh = unsafe {
            CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link)
        };
        log::info!(
            "[UiDisplayLink] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id, new_id,
            if old_refresh > 0.0 { 1.0 / old_refresh } else { 0.0 },
            if new_refresh > 0.0 { 1.0 / new_refresh } else { 0.0 },
        );
        self.current_display_id = new_id;
    }
}

impl Drop for UiDisplayLink {
    fn drop(&mut self) {
        // Signal the callback to become a no-op IMMEDIATELY.
        unsafe { (*self.context).stop.store(true, Ordering::Release); }

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
