//! Thin FFI bindings to the macOS frameworks the tool talks to directly.
//!
//! Kept separate from `main.rs` so the unsafe surface stays small and
//! auditable. We only touch CGDisplayStream (deprecated but still functional
//! through macOS 15), CoreGraphics display enumeration, IOSurface use-count
//! refcounting, CoreFoundation retain/release, and libdispatch.

use std::os::raw::{c_char, c_void};

/// kCVPixelFormatType_32BGRA — fourCC 'BGRA' = 0x42475241.
/// Matches `GpuTextureFormat::Bgra8Unorm` so the IOSurface drops in zero-copy.
pub const K_PIXEL_FORMAT_BGRA: i32 = 0x4247_5241_u32 as i32;

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

    pub fn CGDisplayStreamCreateWithDispatchQueue(
        display: u32,
        output_width: usize,
        output_height: usize,
        pixel_format: i32,
        properties: *const c_void,
        queue: *mut c_void,
        handler: *const c_void,
    ) -> *mut c_void;

    pub fn CGDisplayStreamStart(stream: *mut c_void) -> i32;
    // Kept for reference; we exit via libc::exit() now (atexit blackout) so
    // explicit Stop is unnecessary — kernel teardown releases the stream.
    #[allow(dead_code)]
    pub fn CGDisplayStreamStop(stream: *mut c_void) -> i32;
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

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    pub fn dispatch_queue_create(label: *const c_char, attr: *mut c_void) -> *mut c_void;
}
