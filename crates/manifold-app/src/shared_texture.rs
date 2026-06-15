//! Cross-device GPU texture sharing via IOSurface (macOS).
//!
//! Both threads share the same underlying MTLDevice via manifold-gpu GpuDevice.
//! IOSurface provides kernel-managed GPU memory that multiple MTLTextures
//! (from any Device) can bind to — zero copy.
//!
//! Architecture:
//!   Content Device ──render──▶ IOSurface-backed texture ◀──read── UI Device
//!                              (same kernel GPU memory)
//!
//! Note: `io_surface` crate is deprecated in favor of `objc2-io-surface`.
//! Migration planned but deferred — too risky for this pass.
#![allow(deprecated)]

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

/// Number of IOSurface buffers (triple-buffered).
/// 3 surfaces allow 2 content frames in flight while the UI reads a third.
pub const SURFACE_COUNT: usize = 3;

/// Shared state between content and UI threads for the IOSurface bridge.
/// Created once during init, Arc-shared between both threads.
/// Uses interior mutability (RwLock) so resize works through Arc.
///
/// Triple-buffered: three IOSurfaces allow the content thread to have 2 frames
/// in flight while the UI thread reads a completed frame. An atomic front_index
/// tracks which surface is safe to read. Combined with separate Metal command
/// queues (content + UI), this eliminates GPU starvation and
/// synchronous poll stalls.
pub struct SharedTextureBridge {
    /// Three IOSurface kernel objects — triple-buffered for async pipeline.
    /// Behind RwLock for resize (rare write, frequent read via import_texture).
    io_surfaces: RwLock<[io_surface::IOSurface; SURFACE_COUNT]>,
    /// Texture dimensions (atomic for lock-free dimension checks).
    width: AtomicU32,
    height: AtomicU32,
    /// Which surface the UI thread should read (0, 1, or 2).
    /// Content thread calls `publish_front()` after confirming GPU completion.
    front_index: AtomicU32,
    /// Generation counter — incremented on resize so both sides detect stale textures.
    generation: AtomicU64,
}

// SAFETY: IOSurface is a kernel-managed object safe to share across threads.
unsafe impl Send for SharedTextureBridge {}
unsafe impl Sync for SharedTextureBridge {}

/// Bytes per pixel for Rgba16Float.
const BPP: u32 = 8;

/// FourCC for kCVPixelFormatType_64RGBAHalf ('RGhA').
const PIXEL_FORMAT_RGBA16_FLOAT: i32 = 0x52476841u32 as i32;

impl SharedTextureBridge {
    /// Create a new triple-buffered IOSurface bridge at the given dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let surface_a = Self::create_io_surface(width, height);
        let surface_b = Self::create_io_surface(width, height);
        let surface_c = Self::create_io_surface(width, height);

        log::info!(
            "[SharedTextureBridge] created {}x IOSurface {}x{} Rgba16Float ({} bytes each)",
            SURFACE_COUNT,
            width,
            height,
            width * height * BPP,
        );

        Self {
            io_surfaces: RwLock::new([surface_a, surface_b, surface_c]),
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
            front_index: AtomicU32::new(0),
            generation: AtomicU64::new(0),
        }
    }

    /// Create an IOSurface for Rgba16Float at the given dimensions.
    fn create_io_surface(width: u32, height: u32) -> io_surface::IOSurface {
        // Align bytes_per_row to 256 bytes — Metal validation rejects IOSurface-backed
        // textures whose stride doesn't meet the GPU's minimum linear texture alignment.
        let bytes_per_row = (width * BPP + 255) & !255;
        let total_bytes = bytes_per_row * height;

        let mut props = CFMutableDictionary::new();

        unsafe {
            let key_width = CFString::wrap_under_get_rule(io_surface::kIOSurfaceWidth);
            let key_height = CFString::wrap_under_get_rule(io_surface::kIOSurfaceHeight);
            let key_bytes_per_row =
                CFString::wrap_under_get_rule(io_surface::kIOSurfaceBytesPerRow);
            let key_bytes_per_elem =
                CFString::wrap_under_get_rule(io_surface::kIOSurfaceBytesPerElement);
            let key_pixel_format = CFString::wrap_under_get_rule(io_surface::kIOSurfacePixelFormat);
            let key_alloc_size = CFString::wrap_under_get_rule(io_surface::kIOSurfaceAllocSize);

            props.set(key_width, CFNumber::from(width as i64));
            props.set(key_height, CFNumber::from(height as i64));
            props.set(key_bytes_per_row, CFNumber::from(bytes_per_row as i64));
            props.set(key_bytes_per_elem, CFNumber::from(BPP as i64));
            props.set(
                key_pixel_format,
                CFNumber::from(PIXEL_FORMAT_RGBA16_FLOAT as i64),
            );
            props.set(key_alloc_size, CFNumber::from(total_bytes as i64));

            let surface_ref = io_surface::IOSurfaceCreate(props.as_concrete_TypeRef() as *mut _);
            assert!(!surface_ref.is_null(), "IOSurfaceCreate failed");
            TCFType::wrap_under_create_rule(surface_ref)
        }
    }

    /// Create a `GpuTexture` backed by one of the IOSurfaces.
    ///
    /// `surface_index` selects which of the triple-buffered surfaces (0, 1, or 2).
    ///
    /// # Safety
    /// The returned GpuTexture is backed by the IOSurface — caller must ensure
    /// the bridge outlives the texture.
    pub unsafe fn import_texture_native(
        &self,
        device: &manifold_gpu::GpuDevice,
        surface_index: usize,
    ) -> manifold_gpu::GpuTexture {
        unsafe {
            assert!(
                surface_index < SURFACE_COUNT,
                "surface_index must be 0..{SURFACE_COUNT}"
            );
            // Acquire the read lock BEFORE loading dimensions — resize()
            // updates width/height under the write lock, so holding the read
            // lock here guarantees we see dimensions that match the surfaces.
            let io_surfaces_guard = self.io_surfaces.read();
            let width = self.width.load(Ordering::Acquire);
            let height = self.height.load(Ordering::Acquire);

            let io_surface_ref =
                io_surfaces_guard[surface_index].as_concrete_TypeRef() as *const std::ffi::c_void;
            let texture = device.create_texture_from_io_surface(
                io_surface_ref,
                width,
                height,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            );
            drop(io_surfaces_guard);
            texture
        }
    }

    /// Resize the bridge. Creates three new IOSurfaces at the new dimensions.
    /// Both devices must re-import their textures after this call
    /// (detected via generation counter).
    pub fn resize(&self, width: u32, height: u32) {
        let surface_a = Self::create_io_surface(width, height);
        let surface_b = Self::create_io_surface(width, height);
        let surface_c = Self::create_io_surface(width, height);
        {
            let mut guard = self.io_surfaces.write();
            // Update dimensions while holding the write lock so that
            // import_texture_native() (which reads width/height then acquires
            // the read lock) never sees new surfaces with stale dimensions.
            self.width.store(width, Ordering::Release);
            self.height.store(height, Ordering::Release);
            *guard = [surface_a, surface_b, surface_c];
        }
        self.front_index.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        log::info!(
            "[SharedTextureBridge] resized {}x IOSurface to {}x{}",
            SURFACE_COUNT,
            width,
            height,
        );
    }

    /// Which surface index the UI thread should read from.
    /// Updated by the content thread after confirming GPU completion.
    pub fn front_index(&self) -> u32 {
        self.front_index.load(Ordering::Acquire)
    }

    /// Publish a completed surface as the new front buffer.
    /// Called by the content thread after confirming the GPU finished
    /// writing to this surface (fence ready).
    pub fn publish_front(&self, index: u32) {
        self.front_index.store(index, Ordering::Release);
    }

    /// Current dimensions.
    pub fn width(&self) -> u32 {
        self.width.load(Ordering::Acquire)
    }

    pub fn height(&self) -> u32 {
        self.height.load(Ordering::Acquire)
    }

    /// Generation counter — changes on resize. Both sides compare against
    /// their last-seen generation to know when to re-import.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Get the raw IOSurfaceRef pointer for a surface index.
    /// Used for direct CALayer display (zero-copy, pixel-perfect).
    /// The returned pointer is valid as long as no resize occurs.
    // Direct-CALayer display path isn't wired to this accessor yet; scoped
    // allow so the rest of the module still trips dead-code.
    #[allow(dead_code)]
    pub fn raw_io_surface(&self, index: usize) -> *const std::ffi::c_void {
        let guard = self.io_surfaces.read();
        guard[index].as_concrete_TypeRef() as *const std::ffi::c_void
    }
}
