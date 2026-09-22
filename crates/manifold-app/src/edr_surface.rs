//! macOS AppKit interop: EDR (Extended Dynamic Range) surface configuration
//! and miscellaneous NSWindow/NSScreen helpers used from the UI thread.
//!
//! When using Rgba16Float surfaces for HDR output, three properties must be
//! set on the CAMetalLayer for macOS to correctly interpret linear HDR values:
//!
//! 1. `pixelFormat = .rgba16Float` — set via GpuSurface
//! 2. `wantsExtendedDynamicRangeContent = YES` — set via `configure_edr()`
//! 3. `colorspace = kCGColorSpaceExtendedLinearSRGB` — set via `configure_edr()`
//!
//! Without the correct colorspace, macOS doesn't know the values are linear
//! and won't apply the sRGB display transfer function. Subtle bloom gradients
//! (linear 0.02) stay invisible instead of being gamma-expanded to ~0.15.
//!
//! ## Dynamic display capabilities
//!
//! EDR capability varies per-display. A screen's current headroom describes
//! what can be shown now, while its potential headroom describes the maximum
//! available after EDR presentation is enabled. When a window moves between
//! monitors (e.g., MacBook HDR → external projector SDR), the tonemap must
//! switch. An NSNotification observer watches for screen changes and sets a
//! flag checked by the main loop.

use manifold_renderer::presentation::{CurrentHeadroom, DisplayCapabilities, PotentialHeadroom};

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

// ── Event-driven headroom change detection ──────────────────────────────────

#[cfg(target_os = "macos")]
static EDR_SCREEN_CHANGED: AtomicBool = AtomicBool::new(false);

/// Avoid repeating diagnostics across display notifications while native
/// display data is unavailable or invalid.
#[cfg(target_os = "macos")]
static EDR_CAPABILITY_DIAGNOSTIC_EMITTED: AtomicBool = AtomicBool::new(false);

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
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::{class, msg_send, sel};
    use objc2_foundation::NSString;

    // Callback: sets the atomic flag when any screen change occurs.
    extern "C" fn on_screen_changed(
        _this: *mut AnyObject,
        _cmd: Sel,
        _notification: *mut AnyObject,
    ) {
        EDR_SCREEN_CHANGED.store(true, Ordering::Relaxed);
    }

    unsafe {
        let superclass = class!(NSObject);
        let mut builder = ClassBuilder::new(c"ManifoldEDRObserver", superclass)
            .expect("failed to declare ManifoldEDRObserver");
        builder.add_method(
            sel!(onScreenChanged:),
            on_screen_changed as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        let cls: &AnyClass = builder.register();
        let observer: *mut AnyObject = msg_send![cls, new];

        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];

        // NSWindowDidChangeScreenNotification — window moved between displays.
        let name1 = NSString::from_str("NSWindowDidChangeScreenNotification");
        let _: () = msg_send![
            center,
            addObserver: observer,
            selector: sel!(onScreenChanged:),
            name: &*name1,
            object: std::ptr::null::<AnyObject>(),
        ];

        // NSApplicationDidChangeScreenParametersNotification — display
        // connected/disconnected or resolution/brightness changed.
        let name2 = NSString::from_str("NSApplicationDidChangeScreenParametersNotification");
        let _: () = msg_send![
            center,
            addObserver: observer,
            selector: sel!(onScreenChanged:),
            name: &*name2,
            object: std::ptr::null::<AnyObject>(),
        ];

        // Leak the observer intentionally — it must live for the app's lifetime.
        let _ = observer;

        log::info!("[EDR] Registered screen change notification observers");
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn register_screen_change_observer() {}

/// Turn the two native EDR values into typed renderer capabilities.
///
/// This is deliberately independent of AppKit so invalid-value handling can
/// be tested without constructing an NSScreen or NSWindow.
fn display_capabilities_from_native(
    potential_value: f64,
    current_value: f64,
) -> Result<DisplayCapabilities, String> {
    let potential = PotentialHeadroom::new(potential_value)
        .map_err(|error| format!("invalid potential EDR headroom {potential_value:?}: {error}"))?;
    let current = CurrentHeadroom::new(current_value)
        .map_err(|error| format!("invalid current EDR headroom {current_value:?}: {error}"))?;

    Ok(DisplayCapabilities::new(potential, current))
}

#[cfg(target_os = "macos")]
fn sdr_capabilities_with_diagnostic(reason: &str) -> DisplayCapabilities {
    if !EDR_CAPABILITY_DIAGNOSTIC_EMITTED.swap(true, Ordering::Relaxed) {
        log::warn!("[EDR] {reason}; using explicit SDR display capabilities");
    }
    DisplayCapabilities::sdr()
}

/// Query typed EDR capabilities for a specific NSScreen. Lightweight — two
/// Obj-C message sends on the valid path, with no allocations there.
#[cfg(target_os = "macos")]
pub(crate) fn query_screen_capabilities(
    screen: *mut objc2::runtime::AnyObject,
) -> DisplayCapabilities {
    use objc2::msg_send;

    if screen.is_null() {
        return sdr_capabilities_with_diagnostic("NSScreen pointer was null");
    }

    unsafe {
        let potential: f64 = msg_send![
            screen,
            maximumPotentialExtendedDynamicRangeColorComponentValue
        ];
        let current: f64 = msg_send![screen, maximumExtendedDynamicRangeColorComponentValue];

        match display_capabilities_from_native(potential, current) {
            Ok(capabilities) => capabilities,
            Err(error) => sdr_capabilities_with_diagnostic(&format!(
                "native EDR values were invalid (potential={potential:?}, current={current:?}): {error}"
            )),
        }
    }
}

/// Query typed EDR capabilities for the screen that the given winit Window is
/// on. Missing or non-AppKit handles use the explicit SDR policy.
#[cfg(target_os = "macos")]
pub(crate) fn query_window_capabilities(window: &winit::window::Window) -> DisplayCapabilities {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return sdr_capabilities_with_diagnostic("window handle was unavailable");
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return sdr_capabilities_with_diagnostic("window handle was not AppKit");
    };

    unsafe {
        let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return sdr_capabilities_with_diagnostic("NSView had no NSWindow");
        }
        let screen: *mut AnyObject = msg_send![ns_window, screen];
        query_screen_capabilities(screen)
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn query_window_capabilities(_window: &winit::window::Window) -> DisplayCapabilities {
    DisplayCapabilities::sdr()
}

/// Set the NSWindow level for a winit window. 0 = NSNormalWindowLevel,
/// 25 = above NSMainMenuWindowLevel (24) so a borderless "fullscreen"
/// window covers the menu bar on a single display without touching global
/// presentation options.
#[cfg(target_os = "macos")]
pub(crate) fn set_window_level(window: &winit::window::Window, level: i64) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };

    unsafe {
        let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setLevel: level];
    }
}

/// Enable/disable the NSWindow shadow. macOS 26 (Tahoe) draws a 1px border
/// around any window that has a shadow — including borderless presentation
/// windows — so presentation mode must drop the shadow to stay flush with
/// the display edges. Same fix as SDL #15005 / ghostty #11325.
#[cfg(target_os = "macos")]
pub(crate) fn set_window_shadow(window: &winit::window::Window, shadow: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };

    unsafe {
        let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setHasShadow: shadow];
    }
}

#[cfg(test)]
mod tests {
    use super::display_capabilities_from_native;

    #[test]
    fn preserves_sdr_current_headroom_with_hdr_potential() {
        let capabilities = display_capabilities_from_native(8.0, 1.0).unwrap();

        assert_eq!(capabilities.current().value(), 1.0);
        assert_eq!(capabilities.potential().value(), 8.0);
    }

    #[test]
    fn preserves_current_and_potential_headroom() {
        let capabilities = display_capabilities_from_native(8.0, 2.0).unwrap();

        assert_eq!(capabilities.current().value(), 2.0);
        assert_eq!(capabilities.potential().value(), 8.0);
    }

    #[test]
    fn rejects_nonfinite_native_headroom_for_diagnostic_path() {
        let error = display_capabilities_from_native(f64::NAN, 2.0).unwrap_err();

        assert!(error.contains("invalid potential EDR headroom"));
    }
}
