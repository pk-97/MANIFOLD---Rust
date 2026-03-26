//! Unified GPU encoding context for the content thread hot path.
//!
//! Carries a `manifold_gpu::GpuEncoder` for native Metal encoding and a
//! reference to `manifold_gpu::GpuDevice` for lazy resource creation.

/// GPU encoding context passed through the entire content thread render loop.
pub struct GpuEncoder<'a> {
    /// Native Metal encoder from manifold-gpu.
    pub native_enc: &'a mut manifold_gpu::GpuEncoder,
    /// Native Metal GPU device for resource creation.
    pub device: &'a manifold_gpu::GpuDevice,
    /// Shared-memory uniform arena for generator uniform data.
    /// Owned by GeneratorRenderer, set during render_all().
    pub uniform_arena: Option<*mut crate::uniform_arena::UniformArena>,
}

// Safety: GpuEncoder is only used within a single frame on the content thread.
unsafe impl Send for GpuEncoder<'_> {}

impl<'a> GpuEncoder<'a> {
    pub fn new(
        native_enc: &'a mut manifold_gpu::GpuEncoder,
        device: &'a manifold_gpu::GpuDevice,
    ) -> Self {
        Self {
            native_enc,
            device,
            uniform_arena: None,
        }
    }

    /// Get mutable reference to the shared uniform arena (if set).
    /// Generators use this instead of buffer writes for uniforms.
    ///
    /// # Safety
    /// Caller must ensure no other mutable reference to the arena exists.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn uniform_arena_mut(
        &self,
    ) -> Option<&mut crate::uniform_arena::UniformArena> {
        self.uniform_arena.map(|p| unsafe { &mut *p })
    }

    // ─── Unified GPU operations ──────────────────────────────────────

    /// Copy texture to texture via native Metal blit.
    pub fn copy_texture_to_texture(
        &mut self,
        src: &manifold_gpu::GpuTexture,
        dst: &manifold_gpu::GpuTexture,
        width: u32,
        height: u32,
    ) {
        self.native_enc.copy_texture_to_texture(src, dst, width, height, 1);
    }

    /// Clear a texture to a solid color via native Metal render pass.
    pub fn clear_texture(
        &mut self,
        texture: &manifold_gpu::GpuTexture,
        r: f64,
        g: f64,
        b: f64,
        a: f64,
    ) {
        self.native_enc.clear_texture(texture, r, g, b, a);
    }

    /// Clear a buffer to zeros via native Metal blit.
    pub fn clear_buffer(&mut self, buffer: &manifold_gpu::GpuBuffer) {
        self.native_enc.clear_buffer(buffer);
    }
}
