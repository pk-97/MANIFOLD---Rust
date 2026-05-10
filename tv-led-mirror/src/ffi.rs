//! Thin FFI bindings to the macOS frameworks the tool talks to directly.
//!
//! Capture proper now goes through ScreenCaptureKit via [`crate::capture`];
//! what's left here is the small surface that ScreenCaptureKit doesn't cover:
//! display enumeration for `--list-displays`, IOSurface use-count refcounting
//! for the GPU-readback hold window, and CoreFoundation retain/release.

use std::os::raw::c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGMainDisplayID() -> u32;

    pub fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;

    pub fn CGDisplayPixelsWide(display: u32) -> usize;
    pub fn CGDisplayPixelsHigh(display: u32) -> usize;
    pub fn CGDisplayIsBuiltin(display: u32) -> i32;
}

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    pub fn IOSurfaceIncrementUseCount(surface: *const c_void);
    pub fn IOSurfaceDecrementUseCount(surface: *const c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRetain(p: *const c_void) -> *const c_void;
    pub fn CFRelease(p: *const c_void);
}
