//! Unified GPU encoding context for the content thread hot path.
//!
//! On macOS, carries a `manifold_gpu::GpuEncoder` for native Metal encoding.
//! On other platforms (or when native encoder is unavailable), falls back to
//! wgpu command encoder.
//!
//! Components check `has_native_encoder()` to decide the dispatch path.
//! The native path uses `manifold_gpu` types for zero-overhead Metal encoding.
//! The wgpu path uses `gpu.encoder` as before.

/// GPU encoding context passed through the entire content thread render loop.
///
/// When `native_enc` is Some, components should encode via the native Metal
/// encoder (zero overhead, no wgpu). When None, they use the wgpu `encoder`
/// field as before.
pub struct GpuEncoder<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// HalContext for hal resource creation (bind groups, samplers).
    /// None when hal-encoding feature is off or on non-macOS.
    pub hal_ctx: Option<&'a crate::hal_context::HalContext>,
    /// Raw pointer to the hal command encoder for zero-overhead encoding.
    /// When Some, components should encode via hal instead of wgpu.
    ///
    /// Raw pointer avoids borrow conflicts with other GpuEncoder fields.
    /// Valid for the duration of the compositor's render call.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    pub hal_enc: Option<*mut crate::hal_context::MetalCommandEncoder>,
    /// Shared-memory uniform arena for generator uniform data.
    /// Owned by GeneratorRenderer, set during render_all().
    /// Generators push uniform data here instead of calling queue.write_buffer().
    pub uniform_arena: Option<*mut crate::uniform_arena::UniformArena>,
    /// Native Metal encoder from manifold-gpu.
    /// When Some, components should dispatch through this for zero wgpu overhead.
    /// Raw pointer avoids borrow conflicts. Valid for the frame's duration.
    #[cfg(target_os = "macos")]
    pub native_enc: Option<*mut manifold_gpu::GpuEncoder>,
    /// Native Metal GPU device for pipeline/resource creation.
    /// Available when native Metal path is active.
    #[cfg(target_os = "macos")]
    pub native_device: Option<*const manifold_gpu::GpuDevice>,
}

// Safety: native_enc points to a manifold-gpu encoder on the content thread's stack.
// GpuEncoder is only used within a single frame on the content thread.
unsafe impl Send for GpuEncoder<'_> {}

impl<'a> GpuEncoder<'a> {
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        encoder: &'a mut wgpu::CommandEncoder,
        hal_ctx: Option<&'a crate::hal_context::HalContext>,
    ) -> Self {
        Self {
            device,
            queue,
            encoder,
            hal_ctx,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_enc: None,
            uniform_arena: None,
            #[cfg(target_os = "macos")]
            native_enc: None,
            #[cfg(target_os = "macos")]
            native_device: None,
        }
    }

    /// Check if native Metal encoding is active.
    #[cfg(target_os = "macos")]
    #[inline]
    pub fn has_native_encoder(&self) -> bool {
        self.native_enc.is_some()
    }

    /// Get mutable reference to the native Metal encoder.
    ///
    /// # Safety
    /// Caller must ensure no other mutable reference to the encoder exists.
    #[cfg(target_os = "macos")]
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn native_encoder_mut(&self) -> Option<&mut manifold_gpu::GpuEncoder> {
        self.native_enc.map(|p| unsafe { &mut *p })
    }

    /// Check if hal encoding is active (hal encoder available).
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[inline]
    pub fn has_hal_encoder(&self) -> bool {
        self.hal_enc.is_some() && self.hal_ctx.is_some()
    }

    /// Get mutable reference to the shared uniform arena (if set).
    /// Generators use this instead of queue.write_buffer() for uniforms.
    ///
    /// # Safety
    /// Caller must ensure no other mutable reference to the arena exists.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn uniform_arena_mut(&self) -> Option<&mut crate::uniform_arena::UniformArena> {
        self.uniform_arena.map(|p| unsafe { &mut *p })
    }

    /// Get mutable reference to the hal encoder and context.
    ///
    /// # Safety
    ///
    /// Caller must ensure no other mutable reference to the hal encoder
    /// exists for the duration of the returned reference.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn hal_encoder_mut(
        &self,
    ) -> Option<(&mut crate::hal_context::MetalCommandEncoder, &crate::hal_context::HalContext)>
    {
        if let (Some(enc_ptr), Some(ctx)) = (self.hal_enc, self.hal_ctx) {
            Some((unsafe { &mut *enc_ptr }, ctx))
        } else {
            None
        }
    }
}

/// Extract a raw Metal texture from a wgpu Texture and wrap as GpuTexture.
///
/// Uses wgpu's `as_hal()` for resource extraction only (NOT for encoding).
/// The returned GpuTexture holds a retained reference to the underlying Metal
/// texture. Safe as long as the wgpu Texture is alive.
///
/// # Safety
/// The Texture must be backed by the Metal backend.
#[cfg(target_os = "macos")]
pub unsafe fn extract_native_texture(
    texture: &wgpu::Texture,
) -> manifold_gpu::GpuTexture {
    type MetalApi = wgpu::hal::api::Metal;
    let guard = unsafe {
        texture.as_hal::<MetalApi>()
            .expect("Texture not Metal")
    };
    let raw_tex = unsafe { (*guard).raw_handle().to_owned() };
    let w = texture.width();
    let h = texture.height();
    let d = texture.depth_or_array_layers();
    manifold_gpu::GpuTexture::from_raw(
        raw_tex, w, h, d,
        manifold_gpu::GpuTextureFormat::Rgba16Float,
    )
}

/// Extract the native Metal buffer from a wgpu::Buffer for native Metal dispatch.
///
/// Uses wgpu's `as_hal()` for resource extraction only (NOT for encoding).
/// The returned GpuBuffer holds a retained reference to the underlying Metal
/// buffer. Safe as long as the wgpu Buffer is alive.
///
/// # Safety
/// The Buffer must be backed by the Metal backend.
/// Relies on wgpu-hal 28's `metal::Buffer` struct layout: `{ raw: metal::Buffer, size: u64 }`.
#[cfg(target_os = "macos")]
pub unsafe fn extract_native_buffer(
    buffer: &wgpu::Buffer,
) -> manifold_gpu::GpuBuffer {
    type MetalApi = wgpu::hal::api::Metal;
    let guard = unsafe {
        buffer.as_hal::<MetalApi>()
            .expect("Buffer not Metal")
    };
    // wgpu-hal metal::Buffer has private `raw: metal::Buffer` as first field.
    // metal::Buffer is a foreign_type wrapping *mut MTLBuffer (ObjC id pointer).
    // Read the ObjC id pointer at offset 0, then retain via BufferRef::to_owned().
    let hal_buf_ptr = &*guard as *const _ as *const *mut std::ffi::c_void;
    let mtl_id = unsafe { *hal_buf_ptr };
    let buf_ref: &metal::BufferRef = unsafe { &*(mtl_id as *const _) };
    let raw_buf = buf_ref.to_owned();
    manifold_gpu::GpuBuffer::from_raw(raw_buf, buffer.size())
}

