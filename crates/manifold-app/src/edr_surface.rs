//! macOS EDR (Extended Dynamic Range) surface configuration.
//!
//! When using Rgba16Float surfaces for HDR output, three properties must be
//! set on the CAMetalLayer for macOS to correctly interpret linear HDR values:
//!
//! 1. `pixelFormat = .rgba16Float` — wgpu handles this
//! 2. `wantsExtendedDynamicRangeContent = YES` — wgpu v28 may handle this
//! 3. `colorspace = kCGColorSpaceExtendedLinearSRGB` — wgpu does NOT set this
//!
//! Without the correct colorspace, macOS doesn't know the values are linear
//! and won't apply the sRGB display transfer function. Subtle bloom gradients
//! (linear 0.02) stay invisible instead of being gamma-expanded to ~0.15.
//!
//! Unity: MonitorWindowPlugin.mm ApplyLayerMode() sets all three.
//!
//! ## Dynamic headroom
//!
//! EDR headroom varies per-display. When a window moves between monitors
//! (e.g., MacBook HDR → external projector SDR), the tonemap must switch.
//! An NSNotification observer watches for screen changes and sets a flag
//! checked by the main loop.

#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceExtendedLinearSRGB: *const c_void;
    unsafe fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    unsafe fn CGColorSpaceRelease(space: *mut c_void);
}

// ── Event-driven headroom change detection ──────────────────────────────────

#[cfg(target_os = "macos")]
static EDR_SCREEN_CHANGED: AtomicBool = AtomicBool::new(false);

/// Returns true (once) if an NSNotification fired indicating the window's
/// screen changed or display parameters changed. Resets the flag on read.
#[cfg(target_os = "macos")]
pub(crate) fn edr_screen_changed() -> bool {
    EDR_SCREEN_CHANGED.swap(false, Ordering::Relaxed)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn edr_screen_changed() -> bool {
    false
}

/// Register NSNotification observers for screen changes.
/// Must be called once from the main thread after window creation.
#[cfg(target_os = "macos")]
pub(crate) fn register_screen_change_observer() {
    use objc::{class, msg_send, sel, sel_impl};
    use objc::declare::ClassDecl;
    use objc::runtime::{Object, Sel};

    // Callback: sets the atomic flag when any screen change occurs.
    extern "C" fn on_screen_changed(
        _this: &Object,
        _cmd: Sel,
        _notification: *mut Object,
    ) {
        EDR_SCREEN_CHANGED.store(true, Ordering::Relaxed);
    }

    unsafe {
        let superclass = class!(NSObject);
        let mut decl = ClassDecl::new("ManifoldEDRObserver", superclass)
            .expect("failed to declare ManifoldEDRObserver");

        decl.add_method(
            sel!(onScreenChanged:),
            on_screen_changed as extern "C" fn(&Object, Sel, *mut Object),
        );

        let cls = decl.register();
        let observer: *mut Object = msg_send![cls, new];

        let center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];

        // NSWindowDidChangeScreenNotification — window moved between displays.
        let name1: *const Object =
            msg_send![class!(NSString), stringWithUTF8String:
                c"NSWindowDidChangeScreenNotification".as_ptr()];
        let _: () = msg_send![center,
            addObserver: observer
            selector: sel!(onScreenChanged:)
            name: name1
            object: std::ptr::null::<Object>()
        ];

        // NSApplicationDidChangeScreenParametersNotification — display
        // connected/disconnected or resolution/brightness changed.
        let name2: *const Object =
            msg_send![class!(NSString), stringWithUTF8String:
                c"NSApplicationDidChangeScreenParametersNotification".as_ptr()];
        let _: () = msg_send![center,
            addObserver: observer
            selector: sel!(onScreenChanged:)
            name: name2
            object: std::ptr::null::<Object>()
        ];

        // Leak the observer intentionally — it must live for the app's lifetime.
        // observer is *mut Object (a raw pointer / Copy type), so just don't release it.
        let _ = observer;

        log::info!("[EDR] Registered screen change notification observers");
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register_screen_change_observer() {}

/// Query EDR headroom for a specific NSScreen. Lightweight — two Obj-C
/// message sends, no allocations.
#[cfg(target_os = "macos")]
pub(crate) fn query_screen_headroom(screen: *mut objc::runtime::Object) -> f64 {
    use objc::{msg_send, sel, sel_impl};

    if screen.is_null() {
        return 1.0;
    }

    unsafe {
        let potential: f64 =
            msg_send![screen, maximumPotentialExtendedDynamicRangeColorComponentValue];
        let current: f64 =
            msg_send![screen, maximumExtendedDynamicRangeColorComponentValue];
        let max_ref: f64 =
            msg_send![screen, maximumReferenceExtendedDynamicRangeColorComponentValue];

        if potential > 1.0 {
            potential
        } else if current > 1.0 {
            current
        } else if max_ref > 1.0 {
            max_ref
        } else {
            1.0
        }
    }
}

/// Query the EDR headroom of the screen that the given winit Window is on.
#[cfg(target_os = "macos")]
pub(crate) fn query_window_headroom(window: &winit::window::Window) -> f64 {
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return 1.0;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return 1.0;
    };

    unsafe {
        let ns_view = appkit.ns_view.as_ptr() as *mut objc::runtime::Object;
        let ns_window: *mut objc::runtime::Object = msg_send![ns_view, window];
        if ns_window.is_null() {
            return 1.0;
        }
        let screen: *mut objc::runtime::Object = msg_send![ns_window, screen];
        query_screen_headroom(screen)
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn query_window_headroom(_window: &winit::window::Window) -> f64 {
    1.0
}

/// Configure the CAMetalLayer for EDR output by accessing it through
/// the wgpu Surface's underlying Metal layer.
///
/// Must be called AFTER wgpu configures the surface. Sets the colorspace
/// to extendedLinearSRGB so macOS applies the correct display transfer
/// function for linear HDR values.
///
/// Takes the raw wgpu Surface (not the winit Window) to avoid re-entering
/// winit's event loop, which panics if called during event handling.
///
/// Returns the current EDR headroom (1.0 = SDR only, >1.0 = HDR capable).
#[cfg(target_os = "macos")]
pub(crate) fn configure_edr(surface: &wgpu::Surface<'_>) -> f64 {
    use foreign_types::ForeignType;
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        // Access the CAMetalLayer through wgpu's Metal backend.
        let Some(metal_surface) = surface.as_hal::<wgpu_hal::api::Metal>() else {
            log::error!("[EDR] Could not get Metal surface handle");
            return 1.0;
        };

        // wgpu-hal Metal surface exposes the raw CAMetalLayer via render_layer().
        let layer_mutex = metal_surface.render_layer();
        let layer_guard = layer_mutex.lock();
        let layer = layer_guard.as_ptr() as *mut objc::runtime::Object;

        // 1. Set colorspace to extendedLinearSRGB.
        let cs = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB);
        if cs.is_null() {
            log::error!("[EDR] CGColorSpaceCreateWithName(extendedLinearSRGB) failed");
        } else {
            let _: () = msg_send![layer, setColorspace: cs];
            CGColorSpaceRelease(cs);
            log::info!("[EDR] Set CAMetalLayer colorspace = kCGColorSpaceExtendedLinearSRGB");
        }

        // 2. Ensure wantsExtendedDynamicRangeContent is YES.
        let _: () = msg_send![layer, setWantsExtendedDynamicRangeContent: true];

        // 3. Verify the configuration.
        let wants_edr: bool = msg_send![layer, wantsExtendedDynamicRangeContent];
        let pixel_format: u64 = msg_send![layer, pixelFormat];
        log::info!(
            "[EDR] CAMetalLayer: wantsEDR={} pixelFormat={} (115=RGBA16Float)",
            wants_edr, pixel_format,
        );

        // 4. Query NSScreen EDR headroom — try all three Apple APIs.
        let ns_screen_class = objc::runtime::Class::get("NSScreen").unwrap();
        let screen: *mut objc::runtime::Object = msg_send![ns_screen_class, mainScreen];

        let (current, potential, max_ref) = if !screen.is_null() {
            // macOS 10.15+: current dynamic headroom (varies with brightness)
            let current: f64 = msg_send![
                screen, maximumExtendedDynamicRangeColorComponentValue
            ];
            // macOS 12.0+: maximum potential headroom (hardware limit)
            let potential: f64 = msg_send![
                screen, maximumPotentialExtendedDynamicRangeColorComponentValue
            ];
            // macOS 10.15+: reference headroom (sustained, not peak)
            let max_ref: f64 = msg_send![
                screen, maximumReferenceExtendedDynamicRangeColorComponentValue
            ];
            (current, potential, max_ref)
        } else {
            (0.0, 0.0, 0.0)
        };

        eprintln!(
            "[HDR-DEBUG] NSScreen EDR: current={:.2} potential={:.2} reference={:.2}",
            current, potential, max_ref,
        );

        // Use potential headroom (hardware max) as our EDR capability.
        // Current headroom varies with brightness and could be temporarily low.
        let headroom = if potential > 1.0 {
            potential
        } else if current > 1.0 {
            current
        } else if max_ref > 1.0 {
            max_ref
        } else {
            1.0
        };

        log::info!(
            "[EDR] NSScreen EDR headroom: {:.2}x (1.0=SDR, >1.0=HDR capable)",
            headroom,
        );
        eprintln!(
            "[HDR-DEBUG] CAMetalLayer configured: colorspace=extendedLinearSRGB \
             wantsEDR={} pixelFormat={} headroom={:.2}x",
            wants_edr, pixel_format, headroom,
        );

        headroom
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn configure_edr(_surface: &wgpu::Surface<'_>) -> f64 {
    1.0
}
