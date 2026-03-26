//! Cross-device GPU texture sharing via IOSurface (macOS).
//!
//! The content thread renders on its own native Metal device (manifold-gpu).
//! The UI thread renders on a separate wgpu Device. IOSurface provides
//! kernel-managed GPU memory that both can bind to — zero copy.
//!
//! Architecture:
//!   Content Device ──render──▶ IOSurface-backed texture ◀──read── UI Device
//!                              (same kernel GPU memory)
//!
//! Triple-buffered: 3 IOSurfaces allow 2 frames in flight. While the GPU
//! executes frame N and the UI presents frame N-1, the content thread encodes
//! frame N+1 into a third surface without stalling.
#![allow(deprecated)]
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use parking_lot::RwLock;

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use foreign_types::ForeignType;
#[allow(unused_imports)]
use objc::{msg_send, sel, sel_impl};

/// Number of IOSurface buffers. 3 = triple buffering (2 frames in flight).
pub const SURFACE_COUNT: usize = 3;

/// Shared state between content and UI threads for the IOSurface bridge.
/// Created once during init, Arc-shared between both threads.
///
/// Triple-buffered: 3 IOSurfaces allow the content thread to have 2 frames
/// in flight — while GPU executes frame N, content encodes frame N+1 into
/// a different surface, and UI presents frame N-1.
pub struct SharedTextureBridge {
    /// IOSurface kernel objects — triple-buffered for async pipeline.
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
            SURFACE_COUNT, width, height, width * height * BPP,
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
        let bytes_per_row = width * BPP;
        let total_bytes = bytes_per_row * height;

        let mut props = CFMutableDictionary::new();

        unsafe {
            let key_width = CFString::wrap_under_get_rule(io_surface::kIOSurfaceWidth);
            let key_height = CFString::wrap_under_get_rule(io_surface::kIOSurfaceHeight);
            let key_bytes_per_row =
                CFString::wrap_under_get_rule(io_surface::kIOSurfaceBytesPerRow);
            let key_bytes_per_elem =
                CFString::wrap_under_get_rule(io_surface::kIOSurfaceBytesPerElement);
            let key_pixel_format =
                CFString::wrap_under_get_rule(io_surface::kIOSurfacePixelFormat);
            let key_alloc_size =
                CFString::wrap_under_get_rule(io_surface::kIOSurfaceAllocSize);

            props.set(key_width, CFNumber::from(width as i64));
            props.set(key_height, CFNumber::from(height as i64));
            props.set(key_bytes_per_row, CFNumber::from(bytes_per_row as i64));
            props.set(key_bytes_per_elem, CFNumber::from(BPP as i64));
            props.set(
                key_pixel_format,
                CFNumber::from(PIXEL_FORMAT_RGBA16_FLOAT as i64),
            );
            props.set(key_alloc_size, CFNumber::from(total_bytes as i64));

            let surface_ref =
                io_surface::IOSurfaceCreate(props.as_concrete_TypeRef() as *mut _);
            assert!(!surface_ref.is_null(), "IOSurfaceCreate failed");
            TCFType::wrap_under_create_rule(surface_ref)
        }
    }

    /// Create an MTLTexture backed by one of the IOSurfaces, then import it
    /// into the given wgpu Device as a wgpu::Texture.
    ///
    /// # Safety
    /// `wgpu_device` must be from the same adapter (same underlying MTLDevice).
    pub unsafe fn import_texture(
        &self,
        wgpu_device: &wgpu::Device,
        surface_index: usize,
    ) -> wgpu::Texture { unsafe {
        assert!(surface_index < SURFACE_COUNT, "surface_index out of range");
        let width = self.width.load(Ordering::Acquire);
        let height = self.height.load(Ordering::Acquire);

        let hal_device_guard = wgpu_device
            .as_hal::<wgpu_hal::api::Metal>()
            .expect("Not a Metal backend");
        let raw_device: &metal::DeviceRef = hal_device_guard.raw_device();

        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        descriptor.set_width(width as u64);
        descriptor.set_height(height as u64);
        descriptor.set_depth(1);
        descriptor.set_mipmap_level_count(1);
        descriptor.set_sample_count(1);
        descriptor.set_texture_type(metal::MTLTextureType::D2);
        descriptor.set_usage(
            metal::MTLTextureUsage::ShaderRead
                | metal::MTLTextureUsage::ShaderWrite
                | metal::MTLTextureUsage::RenderTarget,
        );
        descriptor.set_storage_mode(metal::MTLStorageMode::Shared);

        let io_surfaces_guard = self.io_surfaces.read();
        let io_surface_ref = io_surfaces_guard[surface_index].as_concrete_TypeRef();
        let raw_mtl_texture: *mut objc::runtime::Object = objc::msg_send![
            raw_device,
            newTextureWithDescriptor:descriptor.as_ref()
            iosurface:io_surface_ref
            plane:0usize
        ];
        drop(io_surfaces_guard);
        assert!(
            !raw_mtl_texture.is_null(),
            "newTextureWithDescriptor:iosurface:plane: failed"
        );

        let mtl_texture = metal::Texture::from_ptr(raw_mtl_texture as *mut _);

        let hal_texture = wgpu_hal::metal::Device::texture_from_raw(
            mtl_texture,
            wgpu_types::TextureFormat::Rgba16Float,
            metal::MTLTextureType::D2,
            1,
            1,
            wgpu_hal::CopyExtent { width, height, depth: 1 },
        );

        let labels = ["IOSurface A", "IOSurface B", "IOSurface C"];
        wgpu_device.create_texture_from_hal::<wgpu_hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some(labels[surface_index]),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        )
    }}

    /// Create a native `manifold_gpu::GpuTexture` backed by one of the IOSurfaces.
    ///
    /// # Safety
    /// The returned GpuTexture is backed by the IOSurface — caller must ensure
    /// the bridge outlives the texture.
    pub unsafe fn import_texture_native(
        &self,
        native_device: &manifold_gpu::GpuDevice,
        surface_index: usize,
    ) -> manifold_gpu::GpuTexture { unsafe {
        assert!(surface_index < SURFACE_COUNT, "surface_index out of range");
        let width = self.width.load(Ordering::Acquire);
        let height = self.height.load(Ordering::Acquire);

        let raw_device: &metal::DeviceRef = native_device.raw_device();

        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        descriptor.set_width(width as u64);
        descriptor.set_height(height as u64);
        descriptor.set_depth(1);
        descriptor.set_mipmap_level_count(1);
        descriptor.set_sample_count(1);
        descriptor.set_texture_type(metal::MTLTextureType::D2);
        descriptor.set_usage(
            metal::MTLTextureUsage::ShaderRead
                | metal::MTLTextureUsage::ShaderWrite
                | metal::MTLTextureUsage::RenderTarget,
        );
        descriptor.set_storage_mode(metal::MTLStorageMode::Shared);

        let io_surfaces_guard = self.io_surfaces.read();
        let io_surface_ref = io_surfaces_guard[surface_index].as_concrete_TypeRef();
        let raw_mtl_texture: *mut objc::runtime::Object = objc::msg_send![
            raw_device,
            newTextureWithDescriptor:descriptor.as_ref()
            iosurface:io_surface_ref
            plane:0usize
        ];
        drop(io_surfaces_guard);
        assert!(
            !raw_mtl_texture.is_null(),
            "newTextureWithDescriptor:iosurface:plane: failed"
        );

        let mtl_texture = metal::Texture::from_ptr(raw_mtl_texture as *mut _);

        manifold_gpu::GpuTexture::from_raw(
            mtl_texture,
            width,
            height,
            1,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
        )
    }}

    /// Resize the bridge. Creates 3 new IOSurfaces at the new dimensions.
    /// Both devices must re-import their textures after this call.
    pub fn resize(&self, width: u32, height: u32) {
        let surface_a = Self::create_io_surface(width, height);
        let surface_b = Self::create_io_surface(width, height);
        let surface_c = Self::create_io_surface(width, height);
        {
            let mut guard = self.io_surfaces.write();
            *guard = [surface_a, surface_b, surface_c];
        }
        self.width.store(width, Ordering::Release);
        self.height.store(height, Ordering::Release);
        self.front_index.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        log::info!(
            "[SharedTextureBridge] resized {}x IOSurface to {}x{}",
            SURFACE_COUNT, width, height,
        );
    }

    /// Which surface index the UI thread should read from.
    pub fn front_index(&self) -> u32 {
        self.front_index.load(Ordering::Acquire)
    }

    /// Publish a completed surface as the new front buffer.
    pub fn publish_front(&self, index: u32) {
        self.front_index.store(index, Ordering::Release);
    }

    pub fn width(&self) -> u32 {
        self.width.load(Ordering::Acquire)
    }

    pub fn height(&self) -> u32 {
        self.height.load(Ordering::Acquire)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}
