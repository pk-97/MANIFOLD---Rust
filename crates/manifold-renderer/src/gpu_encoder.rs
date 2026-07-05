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

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use half::f16;
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

    /// `resize_sample` must downscale the WHOLE source, not crop the
    /// top-left corner. This is the regression test for the bug class
    /// the user flagged: the DNN atoms filled their analysis staging
    /// with a `copy_texture_to_texture` blit, which crops, so depth /
    /// flow / person ran on the top-left ~9% of a 4K frame.
    ///
    /// Source is a 4×4 vertical split — left half black, right half
    /// white. Downscaled to 2×2, a correct sample-resize keeps the
    /// split (left column black, right column white). A crop of the
    /// top-left 2×2 would be all black, so the right column going white
    /// is the discriminating signal.
    ///
    /// The bug is resolution-dependent (only appears when source >
    /// analysis res), which is exactly why the 256×256 preset-load test
    /// never caught it. This test deliberately uses src > dst.
    #[test]
    fn resize_sample_covers_whole_source_not_top_left_crop() {
        let device = crate::test_device();

        // 4×4 RGBA8 source: columns 0–1 black, columns 2–3 white.
        let mut src_px = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 0..4 {
                let i = (y * 4 + x) * 4;
                let v = if x >= 2 { 255u8 } else { 0u8 };
                src_px[i] = v;
                src_px[i + 1] = v;
                src_px[i + 2] = v;
                src_px[i + 3] = 255;
            }
        }
        let src = device.create_texture(&GpuTextureDesc {
            width: 4,
            height: 4,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "resize-test-src",
            mip_levels: 1,
        });
        device.upload_texture(&src, &src_px);

        // 2×2 RGBA16F destination — the analysis-staging format the DNN
        // atoms use, exercising the rgba16float resize variant.
        let dst = device.create_texture(&GpuTextureDesc {
            width: 2,
            height: 2,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "resize-test-dst",
            mip_levels: 1,
        });

        // Source is rgba8unorm, dst is rgba16float — resize_sample bridges
        // formats (it samples as f32 and stores to the dst format), unlike
        // copy_texture_to_texture which requires matching formats.
        let mut native_enc = device.create_encoder("resize-test");
        {
            let mut gpu = super::GpuEncoder::new(&mut native_enc, &device);
            gpu.resize_sample(&src, &dst);
        }
        native_enc.commit_and_wait_completed();

        let bytes_per_row = 2 * 8; // 2 px × rgba16float (8 bytes)
        let readback = device.create_buffer_shared(u64::from(2 * bytes_per_row));
        let mut rb_enc = device.create_encoder("resize-test-readback");
        rb_enc.copy_texture_to_buffer(&dst, &readback, 2, 2, bytes_per_row);
        rb_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), 2 * 2 * 4) };
        let r_at = |x: usize, y: usize| f16::from_bits(halves[(y * 2 + x) * 4]).to_f32();

        // Left column dark, right column bright — the split survived the
        // downscale. A top-left crop would leave the right column dark.
        assert!(
            r_at(0, 0) < 0.25 && r_at(0, 1) < 0.25,
            "left column should be ~black, got ({}, {})",
            r_at(0, 0),
            r_at(0, 1),
        );
        assert!(
            r_at(1, 0) > 0.75 && r_at(1, 1) > 0.75,
            "right column should be ~white (whole-frame sampled, not \
             top-left cropped), got ({}, {})",
            r_at(1, 0),
            r_at(1, 1),
        );
    }
}
