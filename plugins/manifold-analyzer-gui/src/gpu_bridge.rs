//! Thin plugin-side wrapper: an IOSurface + its `GpuTexture` kept alive together.
//!
//! Both the IOSurface creation (`GpuDevice::create_io_surface_bgra8`) and the
//! Metal texture creation (`GpuDevice::create_texture_from_io_surface`) live in
//! `manifold-gpu`. This module just owns both handles and releases the IOSurface
//! on drop.

use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureUsage};
use std::ffi::c_void;

pub type IOSurfaceRef = *mut c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

pub struct IoSurfaceMtlTexture {
    pub width: u32,
    pub height: u32,
    iosurface: IOSurfaceRef,
    gpu_texture: GpuTexture,
}

// The underlying types (IOSurface, MTLTexture) are thread-safe for distinct use;
// we don't mutate this struct concurrently.
unsafe impl Send for IoSurfaceMtlTexture {}
unsafe impl Sync for IoSurfaceMtlTexture {}

impl IoSurfaceMtlTexture {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Option<Self> {
        let iosurface = unsafe { GpuDevice::create_io_surface_bgra8(width, height) }?;
        if iosurface.is_null() {
            eprintln!("manifold-analyzer-gui: create_io_surface_bgra8 returned null");
            return None;
        }
        let gpu_texture = unsafe {
            device.create_texture_from_io_surface(
                iosurface as *const c_void,
                width,
                height,
                GpuTextureFormat::Bgra8Unorm,
                GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
            )
        };
        eprintln!(
            "manifold-analyzer-gui: IoSurfaceMtlTexture ready {}x{} (iosurface={:p})",
            width, height, iosurface
        );
        Some(Self {
            width,
            height,
            iosurface,
            gpu_texture,
        })
    }

    pub fn gpu_texture(&self) -> &GpuTexture {
        &self.gpu_texture
    }

    pub fn iosurface_raw(&self) -> IOSurfaceRef {
        self.iosurface
    }
}

impl Drop for IoSurfaceMtlTexture {
    fn drop(&mut self) {
        unsafe { CFRelease(self.iosurface as *const c_void) };
    }
}
