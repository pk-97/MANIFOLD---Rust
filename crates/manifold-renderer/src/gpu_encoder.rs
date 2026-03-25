//! Unified GPU encoding context for the content thread hot path.
//!
//! Wraps wgpu types with public field access. When `hal-encoding` is enabled,
//! also carries an optional hal command encoder. Components check `hal_enc`
//! to decide whether to encode via hal (zero overhead) or wgpu (default).
//!
//! In Phase 3e's three-encoder split:
//! - Generators get a GpuEncoder with hal_enc=None (wgpu encoding)
//! - Compositor gets a GpuEncoder with hal_enc=Some (hal encoding)
//! - Copy/fence gets raw wgpu encoder (no GpuEncoder needed)

/// GPU encoding context passed through the entire content thread render loop.
/// Replaces the `(device, queue, encoder)` triplet in all hot-path signatures.
///
/// When `hal_enc` is Some, compositor components encode via the hal command
/// encoder for zero-overhead Metal encoding. When None, they use the wgpu
/// `encoder` field as before.
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
}

// Safety: hal_enc points to a hal encoder on the content thread's stack.
// GpuEncoder is only used within a single frame on the content thread.
#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
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
        }
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
