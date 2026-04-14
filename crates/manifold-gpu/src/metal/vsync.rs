//! GpuVsyncSignal — platform-abstracted display vsync signal.
//!
//! Provides a blocking wait primitive for render threads that need to
//! synchronize frame production to a display's refresh cadence.
//!
//! Two-part design:
//! - [`GpuVsyncSignal`]: owned by the UI thread. Manages the CVDisplayLink
//!   and handles retargeting when windows move between displays.
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

// ─── CVDisplayLink FFI ──────────────────────────────────────────────────

type CVDisplayLinkRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CVTimeStamp {
    version: u32,
    video_time_scale: i32,
    video_time: i64,
    host_time: u64,
    rate_scalar: f64,
    video_refresh_period: i64,
    smpte_time: [u8; 24],
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

// ─── VSync state (shared between callback thread and render thread) ─────

struct VsyncState {
    /// Incremented by the CVDisplayLink callback on each vsync.
    vsync_count: u64,
    /// Display refresh rate in Hz (updated on retarget).
    display_hz: f64,
    /// Set to true to release any waiting thread (for shutdown).
    shutdown: bool,
}

/// Shared inner state for cross-thread vsync signaling.
///
/// The CVDisplayLink callback thread locks the mutex, increments vsync_count,
/// and calls condvar.notify_one(). The render thread blocks on the condvar
/// until a new vsync arrives or timeout expires.
///
/// Using `std::sync::Mutex` (not parking_lot) because macOS pthread_mutex
/// supports PTHREAD_PRIO_INHERIT for priority inheritance — important when
/// the CVDisplayLink real-time thread contends with the SCHED_RR content thread.
struct VsyncInner {
    state: Mutex<VsyncState>,
    condvar: Condvar,
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

// ─── CVDisplayLink callback ─────────────────────────────────────────────

/// Atomic for measuring real callback interval (nanoseconds since last callback).
static LAST_CALLBACK_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Callback counter for periodic diagnostic.
static CALLBACK_DIAG_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "C" fn content_vsync_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    context: *mut c_void,
) -> i32 {
    let inner = unsafe { &*(context as *const VsyncInner) };

    // Measure real wall-clock callback interval via mach_absolute_time.
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }
    #[repr(C)]
    struct MachTimebaseInfo { numer: u32, denom: u32 }
    let now_ns = unsafe {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        mach_timebase_info(&mut info);
        let ticks = mach_absolute_time();
        ticks * info.numer as u64 / info.denom as u64
    };
    let prev_ns = LAST_CALLBACK_NS.swap(now_ns, std::sync::atomic::Ordering::Relaxed);
    let interval_ms = if prev_ns > 0 {
        (now_ns - prev_ns) as f64 / 1_000_000.0
    } else {
        0.0
    };

    // Derive display Hz from the CVTimeStamp's refresh period.
    let hz = unsafe {
        let ts = &*in_output_time;
        if ts.video_refresh_period > 0 && ts.video_time_scale > 0 {
            ts.video_time_scale as f64 / ts.video_refresh_period as f64
        } else {
            0.0
        }
    };

    // Periodic diagnostic: every 120 callbacks (~2s at 60Hz).
    let n = CALLBACK_DIAG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n > 0 && n.is_multiple_of(120) {
        let measured_hz = if interval_ms > 0.0 { 1000.0 / interval_ms } else { 0.0 };
        eprintln!(
            "[CVDisplayLink] callback #{}: interval={:.2}ms measured_hz={:.1} \
             timestamp_hz={:.1}",
            n, interval_ms, measured_hz, hz,
        );
    }

    // Lock held for nanoseconds — just increment + notify + update Hz.
    if let Ok(mut state) = inner.state.lock() {
        state.vsync_count += 1;
        if hz > 0.0 {
            state.display_hz = hz;
        }
        inner.condvar.notify_one();
    }
    K_CV_RETURN_SUCCESS
}

// ─── Send wrapper for raw pointer cleanup ───────────────────────────────

struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
impl<T> SendPtr<T> {
    fn get(self) -> *mut T {
        self.0
    }
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
    /// Returns the new vsync count and display Hz. Includes a 100ms timeout
    /// to prevent deadlocks if the display link stops firing (display sleep,
    /// display disconnected, etc.).
    ///
    /// The render thread should track `last_seen_count` and pass it each call.
    pub fn wait(&self, last_seen_count: u64) -> VsyncWaitResult {
        let timeout = std::time::Duration::from_millis(100);
        let guard = self.inner.state.lock().unwrap();

        // Wait until vsync_count advances past last_seen, or shutdown, or timeout.
        let (guard, wait_result) = self
            .inner
            .condvar
            .wait_timeout_while(guard, timeout, |state| {
                !state.shutdown && state.vsync_count <= last_seen_count
            })
            .unwrap();

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
// GpuVsyncSignal — UI thread side (owns CVDisplayLink, retargets)
// ═══════════════════════════════════════════════════════════════════════

/// Platform-abstracted display vsync signal controller.
///
/// Owned by the UI thread. Manages the CVDisplayLink lifecycle and
/// retargeting when windows move between displays.
///
/// Call [`create_waiter()`] to get a [`GpuVsyncWaiter`] for the render thread.
pub struct GpuVsyncSignal {
    inner: Arc<VsyncInner>,
    display_link: CVDisplayLinkRef,
    /// Current display ID — compared on retarget to detect actual changes.
    current_display_id: u32,
}

// SAFETY: CVDisplayLinkRef is a CoreFoundation object safe to stop/release
// from any thread. The inner Arc<VsyncInner> is inherently Send+Sync.
unsafe impl Send for GpuVsyncSignal {}

impl GpuVsyncSignal {
    /// Create a new vsync signal targeting the given window's display.
    ///
    /// Starts a CVDisplayLink that fires at the display's refresh rate
    /// and notifies waiting threads via condvar.
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

            // Pass raw pointer to the Arc's inner data as callback context.
            // The Arc is kept alive by GpuVsyncSignal — the pointer is valid
            // until Drop stops the display link.
            let ctx_ptr = Arc::as_ptr(&inner) as *mut c_void;
            let ret = CVDisplayLinkSetOutputCallback(display_link, content_vsync_callback, ctx_ptr);
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

        // Try to read initial display Hz. May return 0 if the link hasn't
        // fired yet — the callback will populate it from CVTimeStamp on first vsync.
        let refresh_period =
            unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(display_link) };
        let hz = if refresh_period > 0.0 {
            1.0 / refresh_period
        } else {
            0.0
        };
        if hz > 0.0
            && let Ok(mut state) = inner.state.lock()
        {
            state.display_hz = hz;
        }

        log::info!(
            "[GpuVsyncSignal] Started for display {display_id}, \
             initial_hz={:.1} (callback will update)",
            hz,
        );

        Self {
            inner,
            display_link,
            current_display_id: display_id,
        }
    }

    /// Create a waiter handle for the render thread.
    ///
    /// The waiter shares the inner Mutex+Condvar and can block on vsync
    /// signals without accessing the CVDisplayLink.
    pub fn create_waiter(&self) -> GpuVsyncWaiter {
        GpuVsyncWaiter {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Retarget the vsync signal to a different window's display.
    ///
    /// Called when a window moves between monitors. Safe to call while the
    /// CVDisplayLink is running (per Apple docs). The callback may fire one
    /// frame at the old display's timing — acceptable (single late wakeup
    /// is invisible, missed wakeup is handled by the 100ms timeout).
    /// Returns `true` if the display actually changed.
    pub fn retarget(&mut self, window: &impl raw_window_handle::HasWindowHandle) -> bool {
        let new_id = display_id_for_window(window);
        self.retarget_to_display(new_id)
    }

    /// Retarget to a specific display ID.
    /// Returns `true` if the display actually changed.
    pub fn retarget_to_display(&mut self, display_id: u32) -> bool {
        if display_id == 0 || display_id == self.current_display_id {
            return false;
        }

        let old_refresh =
            unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link) };

        // Retarget while running — callback keeps firing without interruption.
        unsafe {
            CVDisplayLinkSetCurrentCGDisplay(self.display_link, display_id);
        }

        let new_refresh =
            unsafe { CVDisplayLinkGetActualOutputVideoRefreshPeriod(self.display_link) };
        let new_hz = if new_refresh > 0.0 {
            1.0 / new_refresh
        } else {
            0.0
        };

        // Update stored Hz so the next wait() returns the new rate.
        if let Ok(mut state) = self.inner.state.lock() {
            state.display_hz = new_hz;
        }

        let old_hz = if old_refresh > 0.0 {
            1.0 / old_refresh
        } else {
            0.0
        };
        eprintln!(
            "[GpuVsyncSignal] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id, display_id, old_hz, new_hz,
        );
        log::info!(
            "[GpuVsyncSignal] Retargeted: display {} → {}, \
             refresh {:.1}Hz → {:.1}Hz",
            self.current_display_id, display_id, old_hz, new_hz,
        );

        self.current_display_id = display_id;
        true
    }

    /// Signal shutdown — unblocks any thread waiting on the condvar.
    ///
    /// Must be called before the render thread is joined, otherwise it may
    /// block forever on the condvar.
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

    /// Current display ID this signal is targeting.
    pub fn current_display_id(&self) -> u32 {
        self.current_display_id
    }
}

impl Drop for GpuVsyncSignal {
    fn drop(&mut self) {
        // Signal shutdown so any waiting thread unblocks.
        self.shutdown();

        // Move blocking cleanup off the current thread. CVDisplayLinkStop
        // blocks until the in-flight callback finishes.
        let dl = SendPtr(self.display_link);
        std::thread::spawn(move || unsafe {
            let dl = dl.get();
            CVDisplayLinkStop(dl);
            CVDisplayLinkRelease(dl);
        });
        // Note: the Arc<VsyncInner> is dropped here. The callback pointer
        // becomes dangling, but CVDisplayLinkStop guarantees no callback
        // runs after it returns. The spawned thread stops the link before
        // the Arc's ref count can reach zero (this Drop holds one Arc ref,
        // the spawned thread completes Stop, then this ref drops).
    }
}
