//! Cached GPU resource helpers: each ensure_* allocates or reuses one resource before the frame's passes borrow it.

use super::*;

impl RenderScene {
    /// Ensure the MSAA color + depth targets match the render target size.
    /// Both are `MSAA_SAMPLES`x multisample; the color resolves out at pass
    /// end. Two storage flavors:
    ///
    /// - the default `memoryless` pair — tile-resident, never leaves the
    ///   GPU (the hot path, unchanged since the Metal migration);
    /// - a `real` (private-storage) pair, allocated when `points_active` —
    ///   SCENE_RENDER_MODE_DESIGN.md D8. Apple's tiling parameter buffer
    ///   caps the TOTAL point primitives in a memoryless-attachment pass at
    ///   a few thousand (measured 2026-09-16 on Apple Silicon: 2048 points
    ///   across two draws pass, 4096 fail with
    ///   `kIOGPUCommandBufferCallbackErrorOutOfMemoryForParameterBuffer`,
    ///   at any point size; ~32k-triangle draws are unaffected), so a dense
    ///   scene drawn as points would fault the memoryless pass. Real
    ///   attachments carry the same draw without the budget. Cost while
    ///   Points is on: MSAA color+depth traffic transits real memory — the
    ///   audit-mode tax, accepted; flipping back to any triangle mode
    ///   recreates the memoryless pair on the next frame.
    pub(super) fn ensure_msaa_targets(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32, points_active: bool) {
        if self.depth_width == width
            && self.depth_height == height
            && self.depth_texture.is_some()
            && self.msaa_color.is_some()
            && self.msaa_real == points_active
        {
            return;
        }
        self.msaa_real = points_active;
        if points_active {
            self.msaa_color = Some(device.create_texture_msaa(
                width,
                height,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                MSAA_SAMPLES,
                "node.render_scene msaa color (points)",
            ));
            self.depth_texture = Some(device.create_texture_msaa(
                width,
                height,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                MSAA_SAMPLES,
                "node.render_scene msaa depth (points)",
            ));
        } else {
            self.msaa_color = Some(device.create_texture_msaa_memoryless(
                width,
                height,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                MSAA_SAMPLES,
                "node.render_scene msaa color",
            ));
            self.depth_texture = Some(device.create_texture_msaa_memoryless(
                width,
                height,
                manifold_gpu::GpuTextureFormat::Depth32Float,
                MSAA_SAMPLES,
                "node.render_scene msaa depth",
            ));
        }
        self.depth_width = width;
        self.depth_height = height;
    }

    pub(super) fn ensure_depth_resolve_scratch(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.depth_resolve_scratch.as_ref().is_some_and(|t| t.width == width && t.height == height) {
            return;
        }
        self.depth_resolve_scratch = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "node.render_scene native depth resolve",
            mip_levels: 1,
        }));
    }

    /// Ensure the memoryless MSAA `Rg16Float` velocity aux-MRT target
    /// matches the render target size (GBUFFER_DESIGN.md section 2 D5, P2). Called
    /// ONLY when `evaluate` finds `velocity` wired this frame (D1 lazy
    /// rule) — an unwired scene never calls this, so it costs nothing.
    pub(super) fn ensure_velocity_msaa_target(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.velocity_width == width
            && self.velocity_height == height
            && self.velocity_msaa.is_some()
        {
            return;
        }
        self.velocity_msaa = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::Rg16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa velocity",
        ));
        self.velocity_width = width;
        self.velocity_height = height;
    }

    /// Ensure the memoryless MSAA `R8Unorm` ao_mask aux-MRT target matches
    /// the render target size (RAYTRACING_DESIGN.md section 12 AM1). Called
    /// ONLY when `evaluate` finds `ao_mask` wired this frame (the same D1
    /// lazy rule as velocity) — an unwired scene never calls this.
    pub(super) fn ensure_ao_mask_msaa_target(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.ao_mask_width == width
            && self.ao_mask_height == height
            && self.ao_mask_msaa.is_some()
        {
            return;
        }
        self.ao_mask_msaa = Some(device.create_texture_msaa_memoryless(
            width,
            height,
            manifold_gpu::GpuTextureFormat::R8Unorm,
            MSAA_SAMPLES,
            "node.render_scene msaa ao_mask",
        ));
        self.ao_mask_width = width;
        self.ao_mask_height = height;
    }

    /// RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): ensure the four
    /// memoryless MSAA aux-MRT targets for the denoiser G-buffer feeds
    /// match the render target size. Called ONLY when `evaluate` finds
    /// `rt_denoise_feed` on (same D1 lazy rule as velocity/ao_mask) — an
    /// unwired/off scene never calls this, so it costs nothing.
    pub(super) fn ensure_denoise_msaa_targets(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.denoise_normals_width == width
            && self.denoise_normals_height == height
            && self.denoise_normals_msaa.is_some()
        {
            return;
        }
        self.denoise_normals_msaa = Some(device.create_texture_msaa_memoryless(
            width, height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa denoise normals",
        ));
        self.denoise_roughness_msaa = Some(device.create_texture_msaa_memoryless(
            width, height,
            manifold_gpu::GpuTextureFormat::R16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa denoise roughness",
        ));
        self.denoise_diffuse_albedo_msaa = Some(device.create_texture_msaa_memoryless(
            width, height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa denoise diffuse_albedo",
        ));
        self.denoise_specular_albedo_msaa = Some(device.create_texture_msaa_memoryless(
            width, height,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa denoise specular_albedo",
        ));
        // DN-L (section 17.7): reactive mask — R16Float like roughness.
        self.denoise_reactive_msaa = Some(device.create_texture_msaa_memoryless(
            width, height,
            manifold_gpu::GpuTextureFormat::R16Float,
            MSAA_SAMPLES,
            "node.render_scene msaa denoise reactive_mask",
        ));
        self.denoise_normals_width = width;
        self.denoise_normals_height = height;
        self.denoise_roughness_width = width;
        self.denoise_roughness_height = height;
        self.denoise_diffuse_albedo_width = width;
        self.denoise_diffuse_albedo_height = height;
        self.denoise_specular_albedo_width = width;
        self.denoise_specular_albedo_height = height;
        self.denoise_reactive_width = width;
        self.denoise_reactive_height = height;
    }

    pub(super) fn ensure_sampler(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.refraction_sampler.is_none() {
            self.refraction_sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mip_filter: manifold_gpu::GpuFilterMode::Linear,
                address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
                ..Default::default()
            }));
        }
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mip_filter: manifold_gpu::GpuFilterMode::Linear,
                address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
                compare: None,
                ..Default::default()
            }));
        }
    }

    /// GLB_XFAIL_BURNDOWN_DESIGN.md D3: pack a `MapSamplerDesc` into a small
    /// dense key for the `material_samplers` cache. `GpuAddressMode`/
    /// `GpuFilterMode` don't derive `Hash` (foreign types in `manifold-gpu`,
    /// not worth adding a derive there for one call site) — a hand-rolled
    /// 4-variant/2-variant discriminant packs into 3 bits, well inside a u32.
    pub(super) fn sampler_cache_key(desc: MapSamplerDesc) -> u32 {
        fn addr_code(a: manifold_gpu::GpuAddressMode) -> u32 {
            match a {
                manifold_gpu::GpuAddressMode::ClampToEdge => 0,
                manifold_gpu::GpuAddressMode::Repeat => 1,
                manifold_gpu::GpuAddressMode::MirrorRepeat => 2,
                manifold_gpu::GpuAddressMode::ClampToZero => 3,
            }
        }
        fn filter_code(f: manifold_gpu::GpuFilterMode) -> u32 {
            match f {
                manifold_gpu::GpuFilterMode::Nearest => 0,
                manifold_gpu::GpuFilterMode::Linear => 1,
            }
        }
        addr_code(desc.wrap_u)
            | (addr_code(desc.wrap_v) << 2)
            | (filter_code(desc.mag_filter) << 4)
            | (filter_code(desc.min_filter) << 5)
            | (desc.mip_filter.map_or(2, filter_code) << 6)
    }

    /// GLB_XFAIL_BURNDOWN_DESIGN.md D3: ensure a cached sampler exists for
    /// `desc`, creating it on first sight. Replaces the single hardcoded
    /// REPEAT material-map sampler (binding 22, landed `85b5bb9d`) — but
    /// `MapSamplerDesc::default()` (an unwired map, or a glTF texture with
    /// no explicit `sampler` index) still resolves to REPEAT+linear, so
    /// every asset that never sets these fields (every pre-D3 passing
    /// asset, plus DamagedHelmet's UV range [1.0, 2.0] that motivated the
    /// original fix) gets byte-identical sampler behavior to before this
    /// phase — the striped-helmet regression `85b5bb9d` fixed stays fixed.
    pub(super) fn ensure_material_sampler(&mut self, device: &manifold_gpu::GpuDevice, desc: MapSamplerDesc) {
        let key = Self::sampler_cache_key(desc);
        self.material_samplers.entry(key).or_insert_with(|| {
            device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                mag_filter: desc.mag_filter,
                min_filter: desc.min_filter,
                mip_filter: desc.mip_filter.unwrap_or(manifold_gpu::GpuFilterMode::Nearest),
                address_mode_u: desc.wrap_u,
                address_mode_v: desc.wrap_v,
                address_mode_w: manifold_gpu::GpuAddressMode::Repeat,
                compare: None,
                // Anisotropic filtering (D7, GLB_CONFORMANCE G-P3): glancing-angle
                // minification on grazing surfaces (floors, the AMG's paint) stays
                // sharp instead of over-blurring. Material samplers only — every
                // other sampler in this file keeps the field's default (1).
                max_anisotropy: if desc.min_filter == manifold_gpu::GpuFilterMode::Linear
                    && desc.mag_filter == manifold_gpu::GpuFilterMode::Linear
                    && desc.mip_filter == Some(manifold_gpu::GpuFilterMode::Linear) { 8 } else { 1 },
                lod_max_clamp: if desc.mip_filter.is_none() { 0.0 } else { f32::MAX },
            })
        });
    }

    pub(super) fn ensure_dummy_texture(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.dummy_texture.is_none() {
            let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                label: "node.render_scene dummy",
                mip_levels: 1,
            });
            // RAYTRACING_DESIGN.md section 14 ED1: the dummy doubles as the
            // "no env chain" stand-in for the GI gather's env-miss (and the
            // reflection miss) — it must READ as black, not as undefined
            // GPU memory. Metal's initial texture contents are undefined;
            // upload a 1x1 black Rgba16Float texel once.
            let black = [0u8; 8]; // 4 x f16 zero
            device.upload_texture(&tex, &black);
            self.dummy_texture = Some(tex);
        }
    }

    pub(super) fn ensure_dummy_emissive_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.dummy_emissive_buffer.is_none() {
            // Metal validates one complete pointee even when entry_count=0
            // skips the sampler block. Size from both shared GPU ABI types.
            let bytes = std::mem::size_of::<manifold_gpu::raytrace::EmissiveTriangleGpu>()
                .max(std::mem::size_of::<manifold_gpu::raytrace::EmissiveAliasEntry>());
            let buf = device.create_buffer_shared(bytes as u64);
            // The newly allocated buffer is exclusively owned and not yet
            // submitted, and the write covers exactly its allocation.
            unsafe { std::ptr::write_bytes(buf.mapped_ptr().expect("shared buffer"), 0, bytes); }
            self.dummy_emissive_buffer = Some(buf);
        }
    }

    /// Ensure the shader-ABI binding stubs the main pass ALWAYS needs: the
    /// PCF comparison sampler (`@binding(14)`) and the 1×1 dummy depth
    /// texture bound to non-caster shadow-map slots. Cheap; called every
    /// frame regardless of whether any light casts. The depth-only shadow
    /// *pipeline* and per-caster maps are NOT created here — that is real
    /// shadow work, gated on there actually being a caster
    /// ([`Self::ensure_shadow_pass`]), so an unwired scene pays nothing.
    pub(super) fn ensure_shadow_binding_stubs(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.shadow_sampler.is_none() {
            // Comparison sampler: linear filtering gives hardware 2×2 PCF per
            // tap, clamp addressing so off-map samples read the border depth.
            self.shadow_sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mip_filter: manifold_gpu::GpuFilterMode::Linear,
                address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
                compare: Some(manifold_gpu::GpuCompareFunction::Less),
                ..Default::default()
            }));
        }
        if self.dummy_depth.is_none() {
            // Bound to unused/non-caster shadow-map slots. Never sampled (the
            // shader returns "lit" before sampling a non-caster slot), but
            // Metal requires every declared binding to be a valid texture.
            // RENDER_TARGET | SHADER_READ, like the real maps, so no depth
            // format needs a render-target-only exception (AGX 0x78 guard).
            self.dummy_depth = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Depth32Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene dummy shadow",
                mip_levels: 1,
            }));
        }
    }

    /// Ensure the depth-only shadow pipeline + its depth-stencil state exist.
    /// Called ONLY when at least one caster is present — compiling the shadow
    /// shader is real work an unwired scene must not pay.
    pub(super) fn ensure_shadow_pass(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.shadow_pipeline.is_none() {
            self.shadow_pipeline = Some(device.create_render_pipeline_depth_only(
                include_str!("../shaders/shadow_depth.wgsl"),
                "vs_main",
                "fs_shadow",
                manifold_gpu::GpuTextureFormat::Depth32Float,
                "node.render_scene shadow depth",
            ));
        }
        if self.shadow_depth_stencil.is_none() {
            self.shadow_depth_stencil = Some(device.create_depth_stencil_state(
                &manifold_gpu::GpuDepthStencilDesc {
                    compare: manifold_gpu::GpuCompareFunction::Less,
                    write_enabled: true,
                },
            ));
        }
    }

    /// Ensure shadow map `slot` is a private `Depth32Float` of `resolution`²,
    /// recreating it if the caster's requested resolution changed.
    /// `RENDER_TARGET | SHADER_READ`: rendered as a depth target, then sampled
    /// as a shadow map — both usages at creation (AGX 0x78 guard).
    pub(super) fn ensure_shadow_map(&mut self, device: &manifold_gpu::GpuDevice, slot: usize, resolution: u32) {
        let res = resolution.clamp(256, 4096);
        let needs = match &self.shadow_maps[slot] {
            Some((cached, _)) => *cached != res,
            None => true,
        };
        if needs {
            let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: res,
                height: res,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Depth32Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene shadow map",
                mip_levels: 1,
            });
            self.shadow_maps[slot] = Some((res, tex));
        }
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md D3 (P2): ensure the half-res
    /// `shaft_inscatter` (`Rgba16Float`) and `shaft_depth_half` (`R32Float`)
    /// textures match `half_w x half_h`, recreating on resize. Same
    /// `ensure_shadow_map` lazy pattern — called ONLY when [`wants_shafts`]
    /// is true this frame.
    pub(super) fn ensure_shaft_half_res(&mut self, device: &manifold_gpu::GpuDevice, half_w: u32, half_h: u32) {
        if self.shaft_half_width == half_w
            && self.shaft_half_height == half_h
            && self.shaft_inscatter.is_some()
            && self.shaft_depth_half.is_some()
        {
            return;
        }
        self.shaft_inscatter = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: half_w,
            height: half_h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "node.render_scene shaft inscatter",
            mip_levels: 1,
        }));
        self.shaft_depth_half = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: half_w,
            height: half_h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::R32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "node.render_scene shaft depth half",
            mip_levels: 1,
        }));
        self.shaft_half_width = half_w;
        self.shaft_half_height = half_h;
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md D3 (P2): ensure the full-res internal
    /// depth-resolve target used ONLY when shafts are on and the graph's
    /// `depth` output is unwired this frame — "shafts-on forces the
    /// internal Sample0 depth resolve even when `depth` is unwired" (D3).
    /// The native depth resolve is converted into this R32Float texture by
    /// compute, then read by the shaft downsample kernel.
    pub(super) fn ensure_shaft_depth_internal(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.shaft_depth_internal_width == width
            && self.shaft_depth_internal_height == height
            && self.shaft_depth_internal.is_some()
        {
            return;
        }
        self.shaft_depth_internal = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::R32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_WRITE
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "node.render_scene shaft depth internal",
            mip_levels: 1,
        }));
        self.shaft_depth_internal_width = width;
        self.shaft_depth_internal_height = height;
    }

    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2a/E2b: (re)allocate the opaque-
    /// scene color snapshot when size or format changed. `SHADER_READ` only
    /// — it is populated by a blit (`copy_texture_to_texture`, level 0 only)
    /// then mip-generated (`GpuEncoder::generate_mipmaps`, the same hardware
    /// blit-encoder path `node.gltf_texture_source` uses for its material
    /// maps, F-P6 precedent) — never a render pass, so it needs no
    /// `RENDER_TARGET` bit. `format` mirrors whatever the graph's own
    /// `color` output texture is using this frame (read live from
    /// `target.format` at the call site) so the blit's mandatory
    /// same-format assert can never trip on a future output-format change.
    /// E2b: full mip chain (`max_mip_levels`) so `fs_pbr`'s roughness-driven
    /// refraction blur has levels to sample — level 0 alone (E2a) only
    /// proved the pass seam, not the blur.
    pub(super) fn ensure_opaque_scene_color(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
        format: manifold_gpu::GpuTextureFormat,
    ) {
        if self.opaque_scene_color_width == width
            && self.opaque_scene_color_height == height
            && self.opaque_scene_color_format == format
            && self.opaque_scene_color.is_some()
        {
            return;
        }
        self.opaque_scene_color = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "node.render_scene opaque scene color (E2b transmission snapshot)",
            mip_levels: manifold_gpu::GpuTextureDesc::max_mip_levels(width, height),
        }));
        self.opaque_scene_color_width = width;
        self.opaque_scene_color_height = height;
        self.opaque_scene_color_format = format;
    }

    /// RAYTRACING_DESIGN.md section 8.2 D22: (re)allocate the render-res scratch
    /// color target Pass A/B/shaft-composite resolve into when
    /// `temporal_upscale` is on, instead of the graph's native-res `color`
    /// output. Same fixed `Rgba16Float` format `msaa_color` always resolves
    /// as (so it stays a valid MSAA resolve destination), `RENDER_TARGET`
    /// (the resolve write) + `SHADER_READ` (MetalFX Temporal samples it as
    /// the `color` input) usage — same idiom as every other scratch target
    /// in this file (`ensure_opaque_scene_color` above).
    pub(super) fn ensure_rt_temporal_color_scratch(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.rt_temporal_color_scratch_width == width
            && self.rt_temporal_color_scratch_height == height
            && self.rt_temporal_color_scratch.is_some()
        {
            return;
        }
        self.rt_temporal_color_scratch = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            // RT-Stage-3 P1 (BUG-mkgh): `SHADER_WRITE` added so the firefly
            // clamp can write its result into this scratch (the clamp's
            // `dst` when `temporal_upscale` is on); harmless otherwise —
            // it only widens the usage of a texture MetalFX already reads
            // and the MSAA resolve already writes.
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "node.render_scene RT temporal-upscale render-res color scratch (D22)",
            mip_levels: 1,
        }));
        self.rt_temporal_color_scratch_width = width;
        self.rt_temporal_color_scratch_height = height;
    }

    /// D22: (re)create the live `MetalFxTemporalUpscaler` when the
    /// (render, native) dimension pair changes, or lazily on first use.
    /// Returns `false` (leaves `rt_temporal_upscaler` untouched, i.e. `None`
    /// on first use) when MetalFX Temporal is unavailable on this device —
    /// logged once, not every frame — so a `temporal_upscale`-on scene on
    /// unsupported hardware degrades to "keep whatever `color` last held"
    /// rather than crashing; the caller must check `is_some()` before
    /// encoding an upscale this frame.
    pub(super) fn ensure_rt_temporal_upscaler(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        render_w: u32,
        render_h: u32,
        native_w: u32,
        native_h: u32,
    ) -> bool {
        if let Some(u) = self.rt_temporal_upscaler.as_ref()
            && u.src_w == render_w
            && u.src_h == render_h
            && u.dst_w == native_w
            && u.dst_h == native_h
        {
            return true;
        }
        if let Some(u) = self.rt_temporal_upscaler.as_mut() {
            if u.resize(device, render_w, render_h, native_w, native_h) {
                return true;
            }
            self.rt_temporal_upscaler = None;
        }
        match crate::metalfx_temporal_upscaler::MetalFxTemporalUpscaler::new(
            device, render_w, render_h, native_w, native_h,
        ) {
            Some(u) => {
                self.rt_temporal_upscaler = Some(u);
                self.rt_temporal_unavailable_logged = false;
                true
            }
            None => {
                if !self.rt_temporal_unavailable_logged {
                    log::warn!(
                        "node.render_scene: temporal_upscale is on but MetalFX Temporal is unavailable on this device — rendering natively this frame instead (logged once)"
                    );
                    self.rt_temporal_unavailable_logged = true;
                }
                false
            }
        }
    }

    /// RAYTRACING_DESIGN.md section 17.5 DN-F/DN-G: create or reuse the ML
    /// denoiser when (render, native) dimension pair changes, or lazily on
    /// first use. Returns `false` (leaves `denoiser` as `None` on first use)
    /// when the denoiser is unavailable — logged once, not every frame — so
    /// the caller degrades to the temporal scaler or native path (DN1).
    pub(super) fn ensure_denoiser(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        render_w: u32,
        render_h: u32,
        native_w: u32,
        native_h: u32,
    ) -> bool {
        if let Some(d) = self.denoiser.as_ref()
            && d.matches(render_w, render_h, native_w, native_h)
        {
            return true;
        }
        // Dimensions changed or first creation — replace.
        self.denoiser = None;
        match crate::denoiser::Denoiser::new(
            device, render_w, render_h, native_w, native_h, false,
        ) {
            Some(d) => {
                self.denoiser = Some(d);
                self.denoiser_unavailable_logged = false;
                true
            }
            None => {
                if !self.denoiser_unavailable_logged {
                    log::warn!(
                        "node.render_scene: rt_denoise_feed is on but MetalFX Denoised Scaler is unavailable on this device — falling back to temporal scaler (logged once)"
                    );
                    self.denoiser_unavailable_logged = true;
                }
                false
            }
        }
    }

    /// E2a: (re)allocate the real single-sample `Depth32Float` opaque-depth
    /// snapshot Pass B depth-tests against. `RENDER_TARGET` only — it is
    /// written by a depth-only render pass and never sampled (E2a proves
    /// only the depth-test plumbing; nothing reads this texture in a
    /// shader this phase).
    pub(super) fn ensure_opaque_depth_snapshot(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.opaque_depth_snapshot_width == width
            && self.opaque_depth_snapshot_height == height
            && self.opaque_depth_snapshot.is_some()
        {
            return;
        }
        self.opaque_depth_snapshot = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            // RAYTRACING_DESIGN.md RT-D3: SHADER_READ added alongside the
            // original RENDER_TARGET so the RT shadow-ray compute pass can
            // `depth2d<float>.read()` this same snapshot (no second depth
            // texture) — harmless for the E2a-only (non-RT) case, which
            // never binds it to a compute pipeline.
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "node.render_scene opaque depth snapshot (E2a/RT-D3)",
            mip_levels: 1,
        }));
        self.opaque_depth_snapshot_width = width;
        self.opaque_depth_snapshot_height = height;
    }

    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: allocate the single-sample
    /// depth attachment used to find each transmissive mesh's nearest visible
    /// surface. It is reused for every sorted layer in the frame.
    pub(super) fn ensure_transmissive_depth_scratch(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if self.transmissive_depth_scratch_width == width
            && self.transmissive_depth_scratch_height == height
            && self.transmissive_depth_scratch.is_some()
        {
            return Ok(());
        }
        manifold_node_engine::load::expand::admit_candidate_bytes(
            device.modifier_memory_snapshot(), u64::from(width) * u64::from(height) * 4,
        ).map_err(|error| format!("Glass depth resource admission failed: {error}"))?;
        self.transmissive_depth_scratch = Some(device.try_create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET | manifold_gpu::GpuTextureUsage::COPY_DST,
            label: "node.render_scene transmissive depth scratch (E2b)",
            mip_levels: 1,
        })?);
        self.transmissive_depth_scratch_width = width;
        self.transmissive_depth_scratch_height = height;
        Ok(())
    }

    /// RAYTRACING_DESIGN.md RT-D3: lazily build the raw-MSL shadow-ray
    /// pipelines once per device (kept resident — see
    /// `MetalShadowRayTracer::new`'s doc for why this can't go through
    /// the WGSL pipeline cache).
    pub(super) fn ensure_rt_tracer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_tracer.is_none() {
            self.rt_tracer = Some(manifold_gpu::raytrace::MetalShadowRayTracer::new(device));
        }
    }

    /// Half-res trace target + full-res upsampled mask, resized with the
    /// scene's own output resolution (RT-D3's mode-B half-res dispatch,
    /// D11). RT-P2 widened this from `R32Float` (vis only) to `Rg16Float`
    /// (r = sun visibility, g = AO). Multi-caster shadow fix widens it
    /// again to `Rgba16Float`: one visibility channel per shadow-caster
    /// slot (r=slot 0 .. a=slot 3); AO moved out entirely (folded into
    /// irradiance in-kernel, never written here) — the SAME texture,
    /// extending the SAME dispatch (D16's seam note), not a second mask.
    /// RT-A3a: split into mask and lighting dispatches, each at its own resolution.
    /// Resolution of the mask dispatch (shadow visibility only). Trace-class sized (trace_w/h),
    /// full-class sized (full_w/h) — trace dims change per D4 resolution settings.
    pub(super) fn ensure_rt_masks(&mut self, device: &manifold_gpu::GpuDevice, trace_w: u32, trace_h: u32, full_w: u32, full_h: u32) {
        if self.rt_mask_width == full_w
            && self.rt_mask_height == full_h
            && self.rt_mask_trace_w == trace_w
            && self.rt_mask_trace_h == trace_h
            && self.rt_mask_full.is_some()
        {
            return;
        }

        let make = |w: u32, h: u32, label: &'static str| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };
        self.rt_mask_half = Some(make(trace_w, trace_h, "node.render_scene rt_mask_half (RT-D3/RT-P2 vis)"));
        self.rt_mask_full = Some(make(full_w, full_h, "node.render_scene rt_mask_full (RT-D3/RT-P2 vis)"));
        // RS-A (caster cap 4 -> 8): second shadow-visibility quad — same format and lifecycle.
        self.rt_mask_half2 = Some(make(trace_w, trace_h, "node.render_scene rt_mask_half2 (RS-A vis)"));
        self.rt_mask_full2 = Some(make(full_w, full_h, "node.render_scene rt_mask_full2 (RS-A vis)"));
        // RT-T1-D: à-trous ping-pong scratch — see the field's doc comment.
        self.rt_mask_full_b = Some(make(full_w, full_h, "node.render_scene rt_mask_full_b (RT-T1-D atrous)"));
        // RS-A: second sv quad à-trous ping-pong scratch.
        self.rt_mask_full2_b = Some(make(full_w, full_h, "node.render_scene rt_mask_full2_b (RS-A atrous)"));
        // RT-TL-C (section 16 TL5): sun-transmission tint — MASK-class
        // (written by the mask dispatch), same format and lifecycle as
        // rt_mask_half/rt_mask_full.
        self.rt_svt_half = Some(make(trace_w, trace_h, "node.render_scene rt_svt_half (RT-TL-C)"));
        self.rt_svt_full = Some(make(full_w, full_h, "node.render_scene rt_svt_full (RT-TL-C)"));
        // RT-TL-C: à-trous ping-pong scratch — mirrors rt_mask_full_b.
        self.rt_svt_full_b = Some(make(full_w, full_h, "node.render_scene rt_svt_full_b (RT-TL-C atrous)"));
        self.rt_mask_width = full_w;
        self.rt_mask_height = full_h;
        self.rt_mask_trace_w = trace_w;
        self.rt_mask_trace_h = trace_h;
    }

    /// RAYTRACING_DESIGN.md section 5.2 P2: half-res/full-res demodulated
    /// irradiance targets + the persistent full-res TEMPORAL HISTORY
    /// texture, resized with the scene's own output resolution (mirrors
    /// `ensure_rt_masks`'s lifecycle). Returns `true` when the history
    /// texture was freshly (re)allocated this call — its content is
    /// undefined until the caller's next `accumulate_irradiance` call,
    /// which MUST pass `reset: true` in that case (a dimension change is
    /// itself a discontinuity, same as a cut).
    pub(super) fn ensure_rt_irradiance(&mut self, device: &manifold_gpu::GpuDevice, trace_w: u32, trace_h: u32, full_w: u32, full_h: u32) -> bool {
        if self.rt_irr_width == full_w
            && self.rt_irr_height == full_h
            && self.rt_irr_trace_w == trace_w
            && self.rt_irr_trace_h == trace_h
            && self.rt_irr_history[0].is_some()
        {
            return false;
        }

        let make = |w: u32, h: u32, format: manifold_gpu::GpuTextureFormat, label: &'static str| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };
        // Hold histories need render-pass sentinel clears; reflection
        // scratch also serves as the later MSAA scene-color resolve.
        let make_renderable = |w: u32, h: u32, format: manifold_gpu::GpuTextureFormat, label: &'static str| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };
        let rgba16 = manifold_gpu::GpuTextureFormat::Rgba16Float;
        // Lighting textures (irradiance, reflection, normal) at trace resolution.
        self.rt_irr_half = Some(make(trace_w, trace_h, rgba16, "node.render_scene rt_irr_half (RT-P2)"));
        self.rt_irr_full = Some(make(full_w, full_h, rgba16, "node.render_scene rt_irr_full (RT-P2)"));
        // RT-R1 (section 9.3): trace-res reflection-radiance output — same lifecycle
        // as `rt_irr_half` (the dispatch writes it; T5's kernel is the writer;
        // inert/bind-only until then).
        self.rt_refl_half = Some(make(trace_w, trace_h, rgba16, "node.render_scene rt_refl_half (RT-R1)"));
        // RT-R1 (section 9.3): full-res reflection-radiance output target & atrous
        // scratch (mirror `rt_irr_full`/`rt_irr_full_b`). Inert until T5.
        self.rt_refl_full = Some(make(full_w, full_h, rgba16, "node.render_scene rt_refl_full (RT-R1)"));
        self.rt_refl_full_b = Some(make_renderable(full_w, full_h, rgba16, "node.render_scene reflection/firefly scratch"));
        // The second prefilter pass finishes reading this backing before
        // Pass 2 resolves scene color into it. Neither the accumulator nor
        // the post-filter reads reflection scratch, and there is no CPU
        // mapped access. Both uses remain ordered across GPU checkpoints.
        self.rt_firefly_scratch = self.rt_refl_full_b.clone();
        // RT-T1-C: current-frame primary-hit normal, same half/full
        // lifecycle as irradiance above (not persistent history).
        self.rt_normal_half = Some(make(trace_w, trace_h, rgba16, "node.render_scene rt_normal_half (RT-T1-C)"));
        self.rt_normal_full = Some(make(full_w, full_h, rgba16, "node.render_scene rt_normal_full (RT-T1-C)"));
        // RT-T1-D: second full-res scratch set for the à-trous filter's
        // ping-pong (same lifecycle as irradiance/normal above — not
        // persistent history, rewritten fresh every RT-ready frame).
        self.rt_irr_full_b = Some(make(full_w, full_h, rgba16, "node.render_scene rt_irr_full_b (RT-T1-D atrous)"));
        self.rt_normal_full_b = Some(make(full_w, full_h, rgba16, "node.render_scene rt_normal_full_b (RT-T1-D atrous)"));
        // RT-T1-C: ping-pong history pairs (irradiance, depth, normal) —
        // see this struct's field doc comment for why two textures each.
        self.rt_irr_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_irr_history_a (RT-T1-C)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_irr_history_b (RT-T1-C)"),
        ]
        .map(Some);
        // RT-R2 (RD6): specular history ping-pong pair — same lifecycle +
        // same reset rule as rt_irr_history (I-R2: one reset path, one flip).
        self.rt_refl_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_refl_history_a (RT-R2)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_refl_history_b (RT-R2)"),
        ]
        .map(Some);
        // SV-ACCUM: shadow-visibility history pair — same lifecycle, reset
        // rule, and ping clock as rt_irr_history/rt_refl_history.
        self.rt_sv_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_history_a (SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_history_b (SV-ACCUM)"),
        ]
        .map(Some);
        // SV-ACCUM moments: per-channel first/second visibility moments.
        self.rt_sv_m1_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_m1_a (SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_m1_b (SV-ACCUM)"),
        ]
        .map(Some);
        self.rt_sv_m2_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_m2_a (SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv_m2_b (SV-ACCUM)"),
        ]
        .map(Some);
        self.rt_sv_hold_history = [
            make_renderable(full_w, full_h, manifold_gpu::GpuTextureFormat::R16Float, "node.render_scene rt_sv_hold_a (SV-ACCUM)"),
            make_renderable(full_w, full_h, manifold_gpu::GpuTextureFormat::R16Float, "node.render_scene rt_sv_hold_b (SV-ACCUM)"),
        ]
        .map(Some);
        // RS-A (caster cap 4 -> 8): second shadow-visibility quad SV-ACCUM —
        // independent sigma-gate per quad, same flip clock and lifecycle.
        self.rt_sv2_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_history_a (RS-A SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_history_b (RS-A SV-ACCUM)"),
        ]
        .map(Some);
        self.rt_sv2_m1_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_m1_a (RS-A SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_m1_b (RS-A SV-ACCUM)"),
        ]
        .map(Some);
        self.rt_sv2_m2_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_m2_a (RS-A SV-ACCUM)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_sv2_m2_b (RS-A SV-ACCUM)"),
        ]
        .map(Some);
        self.rt_sv2_hold_history = [
            make_renderable(full_w, full_h, manifold_gpu::GpuTextureFormat::R16Float, "node.render_scene rt_sv2_hold_a (RS-A SV-ACCUM)"),
            make_renderable(full_w, full_h, manifold_gpu::GpuTextureFormat::R16Float, "node.render_scene rt_sv2_hold_b (RS-A SV-ACCUM)"),
        ]
        .map(Some);
        // RT-TL-C (section 16 TL8): sun-transmission tint history pair —
        // same lifecycle, reset rule, and ping clock as rt_irr_history.
        // Full res, Rgba16Float (the scalar hold pairs are R16Float).
        self.rt_svt_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_svt_history_a (RT-TL-C)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_svt_history_b (RT-TL-C)"),
        ]
        .map(Some);
        self.rt_depth_history = [
            make(full_w, full_h, manifold_gpu::GpuTextureFormat::R32Float, "node.render_scene rt_depth_history_a (RT-T1-C)"),
            make(full_w, full_h, manifold_gpu::GpuTextureFormat::R32Float, "node.render_scene rt_depth_history_b (RT-T1-C)"),
        ]
        .map(Some);
        self.rt_normal_history = [
            make(full_w, full_h, rgba16, "node.render_scene rt_normal_history_a (RT-T1-C)"),
            make(full_w, full_h, rgba16, "node.render_scene rt_normal_history_b (RT-T1-C)"),
        ]
        .map(Some);
        // RT-T1-D (BUG-312): luminance-moments ping-pong history — `Rgba32Float`
        // (not `Rg16Float`) so `moment2 - moment1*moment1` doesn't collapse to
        // noise under half-float's ~3-decimal-digit precision at the 1e-4 to
        // 1e-5 variance scale this filter needs (see the MSL kernel's doc
        // comment for the full cancellation argument). ED2: `.b` carries the
        // temporally-accumulated ao (`accumulate_irradiance`'s `history_write.a`
        // is the frame count, so the ao rides here).
        self.rt_moments_history = [
            make(full_w, full_h, manifold_gpu::GpuTextureFormat::Rgba32Float, "node.render_scene rt_moments_history_a (RT-T1-D)"),
            make(full_w, full_h, manifold_gpu::GpuTextureFormat::Rgba32Float, "node.render_scene rt_moments_history_b (RT-T1-D)"),
        ]
        .map(Some);
        // RT-Stage-3 P4 (BUG-eytk): the pre-accumulation à-trous passes
        // above are encoded before post-accumulation filtering writes these
        // handles; the GPU queue orders those reads before the later writes.
        // Reuse their full-resolution RGBA16 scratch handles: cloning a
        // GpuTexture retains the same Metal object and does not allocate.
        // Keep the temporal history pair and raw `rt_irr_full` capture
        // textures separately; only these two frame-local scratch lifetimes
        // overlap at the allocator level.
        self.rt_irr_filtered = self.rt_irr_full_b.clone();
        self.rt_irr_filtered_b = self.rt_normal_full_b.clone();
        self.rt_moments_valid = false;
        self.rt_history_ping = 0;
        self.rt_irr_width = full_w;
        self.rt_irr_height = full_h;
        self.rt_irr_trace_w = trace_w;
        self.rt_irr_trace_h = trace_h;
        true
    }

    /// CPU-mapped `ShadowRayParams` upload buffer, allocated once and
    /// reused every frame (matches this file's `light_buffers` ring
    /// pattern in spirit, minus the multi-frame rotation — the RT pass
    /// runs synchronously within the same frame it's dispatched, unlike
    /// the light data a later frame's draw call might still be reading).
    pub(super) fn ensure_rt_params_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_params_buffer.is_none() {
            self.rt_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::ShadowRayParams>() as u64,
            ));
        }
        // RT-A3a: the mask dispatch needs its own (see the field's doc
        // comment — a shared buffer races the two encode-time uploads).
        if self.rt_mask_params_buffer.is_none() {
            self.rt_mask_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::ShadowRayParams>() as u64,
            ));
        }
    }


    /// RT-P2: CPU-mapped `AccumulateParams` upload buffer — separate from
    /// `rt_params_buffer` (see the field's doc comment).
    pub(super) fn ensure_rt_accumulate_params_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_accumulate_params_buffer.is_none() {
            self.rt_accumulate_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::AccumulateParams>() as u64,
            ));
        }
    }

    /// RT-T1-D: CPU-mapped `AtrousParams` upload buffer — separate from
    /// the other two params buffers above (same non-clobbering reason).
    pub(super) fn ensure_rt_atrous_params_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_atrous_params_buffer.is_none() {
            self.rt_atrous_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::AtrousParams>() as u64,
            ));
        }
    }

    /// RT-Stage-3 P1 (BUG-mkgh): CPU-mapped `FireflyClampParams` upload
    /// buffer — separate from the other params buffers (same
    /// non-clobbering reason).
    pub(super) fn ensure_rt_firefly_params_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_firefly_params_buffer.is_none() {
            self.rt_firefly_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::FireflyClampParams>() as u64,
            ));
        }
    }

    /// RT-Stage-3 P4 (BUG-eytk): CPU-mapped `AtrousPostParams` upload
    /// buffer — separate from the other params buffers (same non-clobbering
    /// reason).
    pub(super) fn ensure_rt_atrous_post_params_buffer(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.rt_atrous_post_params_buffer.is_none() {
            self.rt_atrous_post_params_buffer = Some(device.create_buffer_shared(
                std::mem::size_of::<manifold_gpu::raytrace::AtrousPostParams>() as u64,
            ));
        }
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md D2/D3 (P2): ensure the three internal
    /// shaft pipelines exist. Compiled once, straight from `include_str!`
    /// WGSL — each file forks its own `linearize_depth` copy (the
    /// established "forked, not shared" convention this codebase's other
    /// hand-authored consumers already use, e.g. ssao_from_depth.wgsl), so
    /// no concatenation is needed and each file validates standalone under
    /// `tests/wgsl_validation.rs`'s auto-discovery. Hand-written pipelines,
    /// NOT the `primitive!` codegen path (these are internal `render_scene`
    /// passes, not graph atoms; section 2.5 audit).
    pub(super) fn ensure_shaft_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.shaft_downsample_pipeline.is_none() {
            self.shaft_downsample_pipeline = Some(device.create_compute_pipeline(
                include_str!("../shaders/shaft_depth_downsample.wgsl"),
                "cs_main",
                "node.render_scene shaft downsample",
            ));
        }
        if self.shaft_march_pipeline.is_none() {
            self.shaft_march_pipeline = Some(device.create_compute_pipeline(
                include_str!("../shaders/shaft_march.wgsl"),
                "cs_main",
                "node.render_scene shaft march",
            ));
        }
        if self.shaft_composite_pipeline.is_none() {
            let blend = manifold_gpu::GpuBlendState {
                src_factor: manifold_gpu::GpuBlendFactor::One,
                dst_factor: manifold_gpu::GpuBlendFactor::One,
                operation: manifold_gpu::GpuBlendOp::Add,
                // Alpha untouched (D3): src_alpha=Zero, dst_alpha=One means
                // `dst.a * 1 + 0 = dst.a` regardless of what the fragment
                // shader writes to its own alpha channel.
                src_alpha_factor: manifold_gpu::GpuBlendFactor::Zero,
                dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                alpha_operation: manifold_gpu::GpuBlendOp::Add,
            };
            self.shaft_composite_pipeline = Some(device.create_render_pipeline(
                include_str!("../shaders/shaft_composite.wgsl"),
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(blend),
                "node.render_scene shaft composite",
            ));
        }
    }

    /// Ensure the cached 1-entry identity `InstanceTransform` stub bound to
    /// any object's `instances_n` when that port is unwired (D11). Created
    /// once; cheap, called every frame like the shadow ABI stubs.
    pub(super) fn ensure_identity_instance_stub(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.identity_instance_stub.is_none() {
            let stub = InstanceTransform {
                pos_scale: [0.0, 0.0, 0.0, 1.0],
                rot_pad: [0.0, 0.0, 0.0, 0.0],
            };
            let buf =
                device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
            unsafe {
                buf.write(0, bytemuck::bytes_of(&stub));
            }
            self.identity_instance_stub = Some(buf);
        }
    }

    /// IMPORT_FIDELITY_DESIGN.md D2/F-P1: ensure the three split-sum IBL
    /// GPU resources exist — the prefiltered specular mip chain, the
    /// diffuse irradiance map, and the BRDF LUT — plus their three compute
    /// pipelines. All three are FIXED size (never resized, so this only
    /// ever allocates once); called every frame like the other ABI-stub
    /// ensures (`ensure_dummy_texture`, `ensure_shadow_binding_stubs`) so
    /// `@binding(16..18)` always has something valid bound regardless of
    /// whether `envmap` is wired or any object is PBR.
    pub(super) fn ensure_ibl_resources(&mut self, device: &manifold_gpu::GpuDevice) {
        if self.prefiltered_specular.is_none() {
            self.prefiltered_specular = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: PREFILTER_BASE_WIDTH,
                height: PREFILTER_BASE_HEIGHT,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
                label: "node.render_scene ibl prefiltered specular",
                mip_levels: PREFILTER_MIP_COUNT,
            }));
        }
        if self.irradiance_map.is_none() {
            self.irradiance_map = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: IRRADIANCE_WIDTH,
                height: IRRADIANCE_HEIGHT,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
                label: "node.render_scene ibl irradiance",
                mip_levels: 1,
            }));
        }
        if self.brdf_lut.is_none() {
            self.brdf_lut = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: BRDF_LUT_SIZE,
                height: BRDF_LUT_SIZE,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
                label: "node.render_scene ibl brdf lut",
                mip_levels: 1,
            }));
        }
        // COMPILE_CONTRACT_DESIGN P2: IBL pipelines are prewarmed at startup
        // in prewarm_pipelines, but use lazy creation as fallback if prewarm
        // hasn't run yet (e.g., during warmup itself).
        if self.ibl_prefilter_pipeline.is_none() {
            self.ibl_prefilter_pipeline = Some(device.create_compute_pipeline(
                concat!(
                    include_str!("../shaders/pbr_brdf.wgsl"),
                    include_str!("../shaders/ibl_prefilter_specular.wgsl"),
                ),
                "cs_main",
                "node.render_scene ibl prefilter",
            ));
        }
        if self.ibl_irradiance_pipeline.is_none() {
            self.ibl_irradiance_pipeline = Some(device.create_compute_pipeline(
                concat!(
                    include_str!("../shaders/pbr_brdf.wgsl"),
                    include_str!("../shaders/ibl_irradiance.wgsl"),
                ),
                "cs_main",
                "node.render_scene ibl irradiance",
            ));
        }
        if self.ibl_brdf_lut_pipeline.is_none() {
            self.ibl_brdf_lut_pipeline = Some(device.create_compute_pipeline(
                concat!(
                    include_str!("../shaders/pbr_brdf.wgsl"),
                    include_str!("../shaders/ibl_brdf_lut.wgsl"),
                ),
                "cs_main",
                "node.render_scene ibl brdf lut",
            ));
        }

    }
}
