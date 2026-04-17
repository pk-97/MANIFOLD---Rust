//! CVDisplayLink-driven vsync for the UI thread.
//!
//! The output presenter was removed — the content thread now presents directly
//! to the output drawable in its own command buffer. See
//! `ContentPipeline::render_content_native()` for the direct present path.
//!
//! This module retains only the UiDisplayLink (CVDisplayLink 3) that drives
//! the winit event loop at the MacBook display's exact refresh cadence.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

// ═══════════════════════════════════════════════════════════════════════
// UiDisplayLink — vsync-aligned render trigger for the UI thread
// ═══════════════════════════════════════════════════════════════════════

// FFI: wake the main CFRunLoop without blocking.
unsafe extern "C" {
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopWakeUp(rl: *mut c_void);
}

/// Context for the UI display link callback. Heap-allocated, accessed only
/// from the serial CVDisplayLink callback thread.
struct UiDisplayLinkContext {
    vsync_ready: Arc<AtomicBool>,
    stop: AtomicBool,
}

unsafe impl Send for UiDisplayLinkContext {}
unsafe impl Sync for UiDisplayLinkContext {}

/// CVDisplayLink callback for the UI thread.
/// Sets the vsync flag and wakes the winit event loop via CFRunLoopWakeUp.
///
/// CRITICAL: must NOT call `window.request_redraw()` here — winit's macOS
/// impl does `dispatch_sync` to the main thread, which deadlocks when the
/// main thread is blocked on CoreVideo's internal mutex (e.g. during
/// `CVDisplayLinkSetCurrentCGDisplay` on any display link instance).
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
    // Non-blocking wake: poke the main CFRunLoop so winit iterates and
    // the app checks vsync_ready in about_to_wait.
    unsafe { CFRunLoopWakeUp(CFRunLoopGetMain()) };
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
            // Stop blocks until any in-flight callback returns, eliminating
            // the race between SetCurrentCGDisplay and the callback thread.
            CVDisplayLinkStop(self.display_link);
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, new_id);
            CVDisplayLinkStart(self.display_link);
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
