//! GpuVsyncSignal — platform-abstracted display vsync signal.
//!
//! Provides a blocking wait primitive for render threads that need to
//! synchronize frame production to a display's refresh cadence.
//!
//! Two-part design:
//! - [`GpuVsyncSignal`]: creates the shared condvar infrastructure and
//!   optionally owns a CVDisplayLink. In "headless" mode, external code
//!   (e.g. a unified display link in manifold-app) calls [`notify_vsync()`]
//!   to drive the signal.
//! - [`GpuVsyncWaiter`]: cloned to the render thread. Provides blocking
//!   `wait()` on the condvar — no CVDisplayLink access needed.
//!
//! On macOS: backed by CVDisplayLink + Condvar. The CVDisplayLink fires
//! at the exact hardware refresh rate and notifies the waiting thread.
//!
//! On other platforms (future): the same API can be implemented with
//! DRM vsync (Linux), DXGI frame latency waitable objects (Windows), etc.
//! The consuming code in manifold-app sees only the platform-agnostic API.

use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex};

// ─── CVDisplayLink FFI (exported for manifold-app) ──────────────────────

pub type CVDisplayLinkRef = *mut c_void;

/// CVTimeStamp — timing information from CoreVideo.
/// `host_time` is in mach_absolute_time units.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CVTimeStamp {
    pub version: u32,
    pub video_time_scale: i32,
    pub video_time: i64,
    pub host_time: u64,
    pub rate_scalar: f64,
    pub video_refresh_period: i64,
    pub smpte_time: [u8; 24],
    pub flags: u64,
    pub reserved: u64,
}

pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
    display_link: CVDisplayLinkRef,
    in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    flags_in: u64,
    flags_out: *mut u64,
    context: *mut c_void,
) -> i32;

pub const K_CV_RETURN_SUCCESS: i32 = 0;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    pub fn CVDisplayLinkCreateWithActiveCGDisplays(out: *mut CVDisplayLinkRef) -> i32;
    pub fn CVDisplayLinkSetCurrentCGDisplay(link: CVDisplayLinkRef, display_id: u32) -> i32;
    pub fn CVDisplayLinkSetOutputCallback(
        link: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        context: *mut c_void,
    ) -> i32;
    pub fn CVDisplayLinkStart(link: CVDisplayLinkRef) -> i32;
    pub fn CVDisplayLinkStop(link: CVDisplayLinkRef) -> i32;
    pub fn CVDisplayLinkRelease(link: CVDisplayLinkRef);
    pub fn CVDisplayLinkGetActualOutputVideoRefreshPeriod(link: CVDisplayLinkRef) -> f64;
}

// ─── Display ID extraction ──────────────────────────────────────────────

/// Get the CGDirectDisplayID for the monitor a window is currently on.
///
/// Platform-agnostic input (HasWindowHandle), macOS-specific implementation.
/// Returns 0 if the display ID cannot be determined.
pub fn display_id_for_window(window: &impl raw_window_handle::HasWindowHandle) -> u32 {
    use raw_window_handle::RawWindowHandle;

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

/// Derive display Hz from a CVTimeStamp's refresh period fields.
/// Returns 0 if the fields are invalid.
pub fn hz_from_timestamp(ts: &CVTimeStamp) -> f64 {
    if ts.video_refresh_period > 0 && ts.video_time_scale > 0 {
        ts.video_time_scale as f64 / ts.video_refresh_period as f64
    } else {
        0.0
    }
}

// ─── Send wrapper for raw pointer cleanup ───────────────────────────────

pub struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}
impl<T> SendPtr<T> {
    pub fn get(self) -> *mut T { self.0 }
}

// ─── VSync state (shared between callback thread and render thread) ─────

pub(crate) struct VsyncState {
    pub(crate) vsync_count: u64,
    pub(crate) display_hz: f64,
    pub(crate) shutdown: bool,
}

/// Shared inner state for cross-thread vsync signaling.
///
/// The CVDisplayLink callback thread (or external caller via `notify_vsync`)
/// locks the mutex, increments vsync_count, and calls condvar.notify_one().
/// The render thread blocks on the condvar until a new vsync arrives.
///
/// Using `std::sync::Mutex` (not parking_lot) because macOS pthread_mutex
/// supports PTHREAD_PRIO_INHERIT for priority inheritance — important when
/// the CVDisplayLink real-time thread contends with the SCHED_RR content thread.
pub(crate) struct VsyncInner {
    pub(crate) state: Mutex<VsyncState>,
    pub(crate) condvar: Condvar,
}

// ─── Result type ────────────────────────────────────────────────────────

/// Result from waiting for a vsync signal.
pub struct VsyncWaitResult {
    /// Current vsync count (monotonically increasing).
    pub count: u64,
    /// Display refresh rate in Hz at time of wake.
    pub display_hz: f64,
    /// True if the wait timed out (no vsync received).
    pub timed_out: bool,
}

// ─── CVDisplayLink callback (used by standalone GpuVsyncSignal) ─────────

unsafe extern "C" fn content_vsync_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let inner = unsafe { &*(context as *const VsyncInner) };
    let hz = hz_from_timestamp(unsafe { &*in_output_time });
    if let Ok(mut state) = inner.state.lock() {
        state.vsync_count += 1;
        if hz > 0.0 {
            state.display_hz = hz;
        }
        inner.condvar.notify_one();
    }
    K_CV_RETURN_SUCCESS
}

// ═══════════════════════════════════════════════════════════════════════
// GpuVsyncWaiter — render thread side (blocking wait on condvar)
// ═══════════════════════════════════════════════════════════════════════

/// Render-thread handle for blocking on display vsync signals.
///
/// Created by [`GpuVsyncSignal::create_waiter()`]. The waiter shares the
/// inner Mutex+Condvar with the signal and the CVDisplayLink callback.
/// It has no access to the CVDisplayLink itself — retargeting and lifecycle
/// are managed by the signal on the UI thread.
pub struct GpuVsyncWaiter {
    inner: Arc<VsyncInner>,
}

// Arc<VsyncInner> is Send+Sync by construction (Mutex+Condvar).
unsafe impl Send for GpuVsyncWaiter {}
unsafe impl Sync for GpuVsyncWaiter {}

impl GpuVsyncWaiter {
    /// Block until a new vsync arrives after `last_seen_count`.
    ///
    /// Returns the new vsync count and display Hz. Includes a 32ms timeout
    /// (~1 frame at 30Hz) to prevent deadlocks if the display link stops
    /// firing (display sleep, fullscreen transition, disconnect, etc.).
    /// The short timeout ensures the content thread degrades gracefully
    /// to timer-based pacing during display transitions.
    ///
    /// The render thread should track `last_seen_count` and pass it each call.
    pub fn wait(&self, last_seen_count: u64) -> VsyncWaitResult {
        let timeout = std::time::Duration::from_millis(32);
        let guard = self.inner.state.lock().unwrap();

        let (guard, wait_result) = self.inner.condvar.wait_timeout_while(
            guard,
            timeout,
            |state| !state.shutdown && state.vsync_count <= last_seen_count,
        ).unwrap();

        VsyncWaitResult {
            count: guard.vsync_count,
            display_hz: guard.display_hz,
            timed_out: wait_result.timed_out(),
        }
    }

    /// Current display refresh rate in Hz.
    pub fn display_hz(&self) -> f64 {
        self.inner.state.lock().unwrap().display_hz
    }

    /// Current vsync count.
    pub fn vsync_count(&self) -> u64 {
        self.inner.state.lock().unwrap().vsync_count
    }
}

// ═══════════════════════════════════════════════════════════════════════
// GpuVsyncSignal — UI thread side
// ═══════════════════════════════════════════════════════════════════════

/// Platform-abstracted display vsync signal controller.
///
/// Two modes:
/// - **Standalone** (`new(window)`): owns a CVDisplayLink that fires the
///   condvar automatically. Used when no unified display link exists.
/// - **Headless** (`new_headless()`): no CVDisplayLink. External code calls
///   `notify_vsync(hz)` to drive the signal. Used when a `UnifiedDisplayLink`
///   in manifold-app drives all consumers from a single CVDisplayLink.
///
/// Call [`create_waiter()`] to get a [`GpuVsyncWaiter`] for the render thread.
pub struct GpuVsyncSignal {
    inner: Arc<VsyncInner>,
    /// None in headless mode — the unified display link drives the condvar.
    display_link: Option<CVDisplayLinkRef>,
    current_display_id: u32,
}

// SAFETY: CVDisplayLinkRef is a CoreFoundation object safe to stop/release
// from any thread. The inner Arc<VsyncInner> is inherently Send+Sync.
unsafe impl Send for GpuVsyncSignal {}

impl GpuVsyncSignal {
    /// Create a headless vsync signal (no CVDisplayLink).
    ///
    /// External code (e.g. `UnifiedDisplayLink`) calls `notify_vsync(hz)` to
    /// increment the vsync counter and wake the content thread. This avoids
    /// having a separate CVDisplayLink that races the unified one.
    pub fn new_headless() -> Self {
        let inner = Arc::new(VsyncInner {
            state: Mutex::new(VsyncState {
                vsync_count: 0,
                display_hz: 0.0,
                shutdown: false,
            }),
            condvar: Condvar::new(),
        });
        log::info!("[GpuVsyncSignal] Created in headless mode (external driver)");
        Self {
            inner,
            display_link: None,
            current_display_id: 0,
        }
    }

    /// Create a standalone vsync signal with its own CVDisplayLink.
    ///
    /// Used as fallback when no unified display link is available.
    pub fn new(window: &impl raw_window_handle::HasWindowHandle) -> Self {
        let display_id = display_id_for_window(window);

        let inner = Arc::new(VsyncInner {
            state: Mutex::new(VsyncState {
                vsync_count: 0,
                display_hz: 0.0,
                shutdown: false,
            }),
            condvar: Condvar::new(),
        });

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
                        "[GpuVsyncSignal] SetCurrentCGDisplay failed for display \
                         {display_id} (ret={ret}), using default"
                    );
                }
            }

            let ctx_ptr = Arc::as_ptr(&inner) as *mut c_void;
            let ret = CVDisplayLinkSetOutputCallback(
                display_link,
                content_vsync_callback,
                ctx_ptr,
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
            "[GpuVsyncSignal] Started standalone for display {display_id}"
        );

        Self {
            inner,
            display_link: Some(display_link),
            current_display_id: display_id,
        }
    }

    /// Notify the condvar from external code (headless mode).
    ///
    /// Called by the unified display link callback to wake the content thread.
    /// Increments vsync_count and updates display_hz atomically.
    pub fn notify_vsync(&self, hz: f64) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.vsync_count += 1;
            if hz > 0.0 {
                state.display_hz = hz;
            }
            self.inner.condvar.notify_one();
        }
    }

    /// Create a waiter handle for the render thread.
    pub fn create_waiter(&self) -> GpuVsyncWaiter {
        GpuVsyncWaiter {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Retarget to a different window's display (standalone mode only).
    pub fn retarget(&mut self, window: &impl raw_window_handle::HasWindowHandle) {
        let new_id = display_id_for_window(window);
        self.retarget_to_display(new_id);
    }

    /// Retarget to a specific display ID (standalone mode only).
    pub fn retarget_to_display(&mut self, display_id: u32) {
        let Some(dl) = self.display_link else { return };
        if display_id == 0 || display_id == self.current_display_id {
            return;
        }

        unsafe {
            CVDisplayLinkSetCurrentCGDisplay(dl, display_id);
        }

        log::info!(
            "[GpuVsyncSignal] Retargeted: display {} → {}",
            self.current_display_id, display_id,
        );

        self.current_display_id = display_id;
    }

    /// Signal shutdown — unblocks any thread waiting on the condvar.
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.shutdown = true;
            self.inner.condvar.notify_all();
        }
    }

    /// Current display refresh rate in Hz.
    pub fn display_hz(&self) -> f64 {
        self.inner.state.lock().unwrap().display_hz
    }

    /// Current display ID (standalone mode only, 0 in headless mode).
    pub fn current_display_id(&self) -> u32 {
        self.current_display_id
    }
}

impl Drop for GpuVsyncSignal {
    fn drop(&mut self) {
        self.shutdown();

        if let Some(dl) = self.display_link.take() {
            let dl = SendPtr(dl);
            std::thread::spawn(move || unsafe {
                let dl = dl.get();
                CVDisplayLinkStop(dl);
                CVDisplayLinkRelease(dl);
            });
        }
    }
}
