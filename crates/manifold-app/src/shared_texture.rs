//! Cross-device GPU texture sharing via IOSurface (macOS).
//!
//! The content thread renders on its own wgpu Device. The UI thread renders on
//! a separate Device. Textures can't cross wgpu Devices, but both Devices share
//! the same underlying MTLDevice. IOSurface provides kernel-managed GPU memory
//! that multiple MTLTextures (from any Device) can bind to — zero copy.
//!
//! Synchronization: MTLSharedEvent signals after the content thread's render
//! completes. The UI thread waits on the event before reading the texture.
//!
//! Architecture:
//!   Content Device ──render──▶ IOSurface-backed texture ◀──read── UI Device
//!                        signal SharedEvent           wait SharedEvent

use std::sync::atomic::{AtomicU64, Ordering};

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use foreign_types::ForeignType;
#[allow(unused_imports)]
use objc::{msg_send, sel, sel_impl};

/// Shared state between content and UI threads for the IOSurface bridge.
/// Created once during init, Arc-shared between both threads.
pub struct SharedTextureBridge {
    /// IOSurface kernel object — owns the GPU memory both textures bind to.
    io_surface: io_surface::IOSurface,
    /// Metal shared event for GPU-to-GPU synchronization.
    /// Content thread signals after render; UI thread waits before read.
    shared_event: metal::SharedEvent,
    /// Monotonically increasing frame counter used as the shared event value.
    frame_counter: AtomicU64,
    /// Texture dimensions.
    pub width: u32,
    pub height: u32,
}

// SAFETY: IOSurface is a kernel-managed object safe to share across threads.
// SharedEvent is explicitly designed for cross-queue/cross-thread use.
// Both are refcounted Obj-C objects.
unsafe impl Send for SharedTextureBridge {}
unsafe impl Sync for SharedTextureBridge {}

/// Bytes per pixel for Rgba16Float.
const BPP: u32 = 8;

/// FourCC for kCVPixelFormatType_64RGBAHalf ('RGhA').
const PIXEL_FORMAT_RGBA16_FLOAT: i32 = 0x52476841u32 as i32;

impl SharedTextureBridge {
    /// Create a new IOSurface bridge at the given dimensions.
    ///
    /// `raw_device` must be the underlying MTLDevice shared by both wgpu Devices
    /// (obtained via `device.as_hal::<Metal>()` → `raw_device()`).
    pub fn new(raw_device: &metal::DeviceRef, width: u32, height: u32) -> Self {
        let io_surface = Self::create_io_surface(width, height);
        let shared_event = raw_device.new_shared_event();

        log::info!(
            "[SharedTextureBridge] created IOSurface {}x{} Rgba16Float ({} bytes)",
            width,
            height,
            width * height * BPP,
        );

        Self {
            io_surface,
            shared_event,
            frame_counter: AtomicU64::new(0),
            width,
            height,
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

    /// Create an MTLTexture backed by this IOSurface, then import it into
    /// the given wgpu Device as a wgpu::Texture.
    ///
    /// Both the content and UI devices call this — each gets their own
    /// MTLTexture handle backed by the same IOSurface memory.
    ///
    /// # Safety
    /// `wgpu_device` must be from the same adapter (same underlying MTLDevice).
    pub unsafe fn import_texture(&self, wgpu_device: &wgpu::Device) -> wgpu::Texture {
        // 1. Get the raw MTLDevice from wgpu
        let hal_device_guard = wgpu_device
            .as_hal::<wgpu_hal::api::Metal>()
            .expect("Not a Metal backend");
        let raw_device: &metal::DeviceRef = hal_device_guard.raw_device();

        // 2. Create an MTLTextureDescriptor
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        descriptor.set_width(self.width as u64);
        descriptor.set_height(self.height as u64);
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

        // 3. Call [MTLDevice newTextureWithDescriptor:iosurface:plane:]
        let io_surface_ref = self.io_surface.as_concrete_TypeRef();
        let raw_mtl_texture: *mut objc::runtime::Object = objc::msg_send![
            raw_device,
            newTextureWithDescriptor:descriptor.as_ref()
            iosurface:io_surface_ref
            plane:0usize
        ];
        assert!(
            !raw_mtl_texture.is_null(),
            "newTextureWithDescriptor:iosurface:plane: failed"
        );

        // 4. Wrap as metal::Texture (takes ownership of the +1 retain from newTexture)
        let mtl_texture = metal::Texture::from_ptr(raw_mtl_texture as *mut _);

        // 5. Create wgpu-hal texture from the raw Metal texture
        let hal_texture = wgpu_hal::metal::Device::texture_from_raw(
            mtl_texture,
            wgpu_types::TextureFormat::Rgba16Float,
            metal::MTLTextureType::D2,
            1, // array_layers
            1, // mip_levels
            wgpu_hal::CopyExtent {
                width: self.width,
                height: self.height,
                depth: 1,
            },
        );

        // 6. Import into wgpu
        wgpu_device.create_texture_from_hal::<wgpu_hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("IOSurface Shared Texture"),
                size: wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
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
    }

    /// Signal the shared event from the content thread's command encoder.
    /// Call this AFTER encoding all render work, BEFORE submit.
    ///
    /// # Safety
    /// `encoder` must be from the content device and about to be submitted.
    pub unsafe fn signal_from_encoder(&self, encoder: &mut wgpu::CommandEncoder) {
        let frame = self.frame_counter.fetch_add(1, Ordering::AcqRel) + 1;

        encoder.as_hal_mut::<wgpu_hal::api::Metal, _, _>(|hal_encoder| {
            if let Some(enc) = hal_encoder {
                if let Some(cmd_buf) = enc.raw_command_buffer() {
                    cmd_buf.encode_signal_event(&self.shared_event, frame);
                }
            }
        });
    }

    /// Wait on the shared event from the UI thread's command encoder.
    /// Call this BEFORE encoding any read of the shared texture.
    ///
    /// # Safety
    /// `encoder` must be from the UI device.
    pub unsafe fn wait_from_encoder(&self, encoder: &mut wgpu::CommandEncoder) {
        let frame = self.frame_counter.load(Ordering::Acquire);
        if frame == 0 {
            return; // No frames rendered yet
        }

        encoder.as_hal_mut::<wgpu_hal::api::Metal, _, _>(|hal_encoder| {
            if let Some(enc) = hal_encoder {
                if let Some(cmd_buf) = enc.raw_command_buffer() {
                    cmd_buf.encode_wait_for_event(&self.shared_event, frame);
                }
            }
        });
    }

    /// Get the current frame counter (for UI to detect new frames).
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter.load(Ordering::Acquire)
    }

    /// Resize the bridge. Creates a new IOSurface at the new dimensions.
    /// Both devices must re-import their textures after this call.
    pub fn resize(&mut self, raw_device: &metal::DeviceRef, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.io_surface = Self::create_io_surface(width, height);
        self.frame_counter.store(0, Ordering::Release);
        self.shared_event = raw_device.new_shared_event();
        log::info!(
            "[SharedTextureBridge] resized IOSurface to {}x{}",
            width,
            height,
        );
    }
}
