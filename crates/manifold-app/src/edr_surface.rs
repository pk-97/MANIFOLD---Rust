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

#[cfg(target_os = "macos")]
use std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceExtendedLinearSRGB: *const c_void;
    unsafe fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    unsafe fn CGColorSpaceRelease(space: *mut c_void);
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
