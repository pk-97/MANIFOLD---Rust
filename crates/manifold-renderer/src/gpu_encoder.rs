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
    /// Texture pool for heap-backed allocation and recycling.
    /// Avoids per-allocation kernel calls for transient textures.
    pub pool: Option<&'a manifold_gpu::TexturePool>,
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
            pool: None,
            uniform_arena: None,
        }
    }

    /// Create with a texture pool for heap-backed allocation.
    pub fn with_pool(
        native_enc: &'a mut manifold_gpu::GpuEncoder,
        device: &'a manifold_gpu::GpuDevice,
        pool: &'a manifold_gpu::TexturePool,
    ) -> Self {
        Self {
            native_enc,
            device,
            pool: Some(pool),
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
    pub unsafe fn uniform_arena_mut(&self) -> Option<&mut crate::uniform_arena::UniformArena> {
        self.uniform_arena.map(|p| unsafe { &mut *p })
    }

    // ─── Unified GPU operations ──────────────────────────────────────

    /// Copy texture to texture via native Metal blit.
    ///
    /// This is a same-size pixel copy: it does NOT scale. `width`/
    /// `height` are the copy extent from origin (0,0); calling it with
    /// a `dst` smaller than `src` silently copies the top-left corner,
    /// not a downscale. To resize between differently-sized textures
    /// use [`Self::resize_sample`]. (The size-mismatch trap is the
    /// cropped-DNN-analysis bug class — see `resize_sample`.)
    pub fn copy_texture_to_texture(
        &mut self,
        src: &manifold_gpu::GpuTexture,
        dst: &manifold_gpu::GpuTexture,
        width: u32,
        height: u32,
    ) {
        self.native_enc
            .copy_texture_to_texture(src, dst, width, height, 1);
    }

    /// Resize `src` into `dst` by bilinear sampling — the correct way
    /// to change a texture's resolution. Samples the ENTIRE `src` into
    /// `dst`'s extent (downscale or upscale), unlike
    /// [`Self::copy_texture_to_texture`], which is a same-size blit that
    /// crops to the top-left on a size mismatch.
    ///
    /// This exists because every DNN/FFI analysis atom needs "downscale
    /// the full-res source into an analysis-resolution staging texture
    /// for readback." Hand-rolling that with a blit silently cropped the
    /// top-left corner (depth/flow/person estimated on ~9% of a 4K frame
    /// — flat corner → dead Z-slider, corner-only flow). `blob_detect_ffi`
    /// independently hit and fixed this with a private downscale shader;
    /// this is that fix lifted to one shared operation so the bug can't
    /// recur per-primitive.
    ///
    /// `dst` must be a storage-writable `Rgba16Float` or `Rgba8Unorm`
    /// texture (the formats the analysis staging textures use). The
    /// pipeline is device-cached (cheap per-frame); the sampler is the
    /// device's shared linear/clamp sampler.
    pub fn resize_sample(
        &mut self,
        src: &manifold_gpu::GpuTexture,
        dst: &manifold_gpu::GpuTexture,
    ) {
        let shader = match dst.format {
            manifold_gpu::GpuTextureFormat::Rgba16Float => RESIZE_SAMPLE_RGBA16F_WGSL,
            manifold_gpu::GpuTextureFormat::Rgba8Unorm => RESIZE_SAMPLE_RGBA8UNORM_WGSL,
            other => panic!(
                "resize_sample: unsupported dst format {other:?} — only \
                 Rgba16Float and Rgba8Unorm are supported (the analysis-\
                 staging formats). Add a shader variant if a new format \
                 is needed."
            ),
        };
        // Device-cached: a hashmap lookup after first compile.
        let device = self.device;
        let pipeline = device.create_compute_pipeline(shader, "cs_main", "resize_sample");
        let sampler = device.linear_sampler();
        self.native_enc.dispatch_compute(
            &pipeline,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: src,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: dst,
                },
            ],
            [dst.width.div_ceil(16), dst.height.div_ceil(16), 1],
            "resize_sample",
        );
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

// Bilinear sample-resize compute shaders for [`GpuEncoder::resize_sample`].
// One variant per supported storage format (textureStore is format-typed
// in WGSL). Both sample the whole source at the destination's per-texel UV,
// so the entire frame is covered regardless of the size ratio.
const RESIZE_SAMPLE_RGBA16F_WGSL: &str = r#"
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    textureStore(dst_tex, vec2<i32>(id.xy), textureSampleLevel(src_tex, samp, uv, 0.0));
}
"#;

const RESIZE_SAMPLE_RGBA8UNORM_WGSL: &str = r#"
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var dst_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    textureStore(dst_tex, vec2<i32>(id.xy), textureSampleLevel(src_tex, samp, uv, 0.0));
}
"#;
