//! Passes that run before the RT block and the main draw: split-sum IBL convolution, raster shadow depth, and the opaque camera-depth snapshot.

use super::*;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 4: the split-sum IBL convolution — runs
    /// before the main pass so the prefiltered/irradiance/LUT textures are
    /// ready to sample (see `run_ibl_convolution`'s doc comment for the
    /// cache-vs-correctness tradeoff). Returns the logical envmap content version
    /// the RT block's lighting key folds in, so an env rebake snaps the
    /// accumulator instead of fading on the EMA floor.
    pub(super) fn ibl_convolution_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
    ) -> Option<ContentVersion> {
        let envmap_content = ctx.inputs.content_version("envmap");
        {
            let rebuild_epoch = ctx.rebuild_epoch;
            let gpu = ctx.gpu_encoder();
            let sampler = self.sampler.as_ref().expect("ensured").clone();
            self.run_ibl_convolution(gpu, &sampler, pre.envmap_wired, envmap_content, rebuild_epoch);
            gpu.checkpoint();
        }
        envmap_content
    }

    /// BUG-trh7 stage 2, pass 5: the raster shadow depth pre-passes — one
    /// depth-only pass per caster, skipped when no light casts or when RT
    /// shadows own the visibility (the explicit gate is load-bearing: the
    /// ensure block never allocates the maps on that path). D6 dirty keys
    /// and the non-PBR degrade-loud warn latch are unchanged.
    pub(super) fn raster_shadow_prepasses<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        opaque_draws: &[&ObjectDraw<'ctx>],
        will_rt_accumulate_this_frame: bool,
        rt_ready: bool,
        has_transparent: bool,
    ) {
        let FramePrelude { ref casters, rt_enabled, rt_shadows_enabled, .. } = *pre;
        let has_casters = !casters.is_empty();
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        // RAYTRACING_DESIGN.md section 14 ED2 (PBR-only consumers, Peter
        // 2026-07-31): cel draws in an RT scene get the flat ambient
        // recompose and NO traced env/GI — degrade loud, not silent. Once
        // per scene instance: per-frame spam breaks the hot path, and a
        // silent hole reads as "RT looks wrong".
        if will_rt_accumulate_this_frame
            && !self.rt_nonpbr_warned
            && opaque_draws
                .iter()
                .any(|d| matches!(d.kind, MaterialKind::Cel))
        {
            self.rt_nonpbr_warned = true;
            log::warn!(
                "render_scene: RT lighting reaches PBR materials only — this scene has \
                 cel objects, which get flat ambient only (RAYTRACING_DESIGN.md section 14 ED2)"
            );
        }
        // RAYTRACING_DESIGN.md RT-D3: shadow maps STOP RENDERING when
        // RT shadows own the visibility — the RT shadow-ray pass below
        // replaces this entire depth-only-per-caster loop (the ensure
        // block above never allocates `shadow_maps` when that path is
        // the sole consumer, so this loop's `None` short-circuit would
        // no-op each caster; the explicit gate is load-bearing).
        // RT toggles: when rt_shadows is off, maps MUST render so the
        // WGSL raster fallthrough path has real data to sample — the sv
        // textures are unwritten (shadow_spp=0) and the WGSL rt_flags.w
        // gate directs shadow_factor to the raster path below (the sv
        // read branch is never entered).
        if has_casters && (!(rt_enabled && rt_ready && rt_shadows_enabled) || has_transparent) {
            // Per-object shadow toggle: this raster depth-only pass is the
            // ONLY place `cast_shadows == false` removes an object from —
            // it stays in `opaque_draws` (and therefore the prepass/accel
            // above and below) unchanged.
            let caster_draws: Vec<&ObjectDraw> =
                opaque_draws.iter().copied().filter(|d| d.cast_shadows).collect();
            let shadow_pipeline = self.shadow_pipeline.as_ref().expect("ensured").clone();
            let shadow_ds = self.shadow_depth_stencil.as_ref().expect("ensured");
            for (slot, l) in casters.iter().take(MAX_RASTER_SHADOW_CASTERS).enumerate() {
                let Some((_, shadow_map)) = self.shadow_maps[slot].as_ref() else {
                    continue;
                };
                let vp = l.shadow_view_proj();

                // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6: this caster's
                // dirty-check key — everything the depth-only batch below
                // reads. `shadow_view_proj()` is a pure function of the
                // light alone (light.rs), so a static light reproduces the
                // exact same `vp` bytes every frame; the per-draw component
                // list catches every other input (geometry, transforms,
                // instancing). Hashed into a fixed hasher — zero per-frame
                // allocation (CLAUDE.md hot-path discipline); `AHasher` is
                // already this crate's fast-hash workhorse (`ahash::AHashMap`
                // throughout `execution.rs`).
                use std::hash::{Hash, Hasher};
                let mut hasher = ahash::AHasher::default();
                hasher.write(bytemuck::bytes_of(&vp));
                hasher.write_u32(l.shadow_resolution);
                hasher.write_usize(caster_draws.len());
                for d in &caster_draws {
                    hasher.write(bytemuck::bytes_of(&d.uniforms.model));
                    d.vertices_content.hash(&mut hasher);
                    d.indices_content.hash(&mut hasher);
                    d.instances_content.hash(&mut hasher);
                    d.weights_content.hash(&mut hasher);
                    hasher.write_u32(d.gain.to_bits());
                    hasher.write(bytemuck::bytes_of(&ShadowUniforms::for_draw(vp, d)));
                    d.rt_texture_content[0].hash(&mut hasher);
                    hasher.write_u32(Self::sampler_cache_key(d.sampler_descs[0]));
                    hasher.write_u32(d.vertex_count);
                    hasher.write_u32(d.instance_count);
                }
                // D6: the rebuild-epoch term — guards against a topology
                // rebuild carrying this primitive's own Rust state (this
                // struct, including `shadow_cache_keys`) into a BRAND NEW
                // executor whose slot generations reset to 0 (see
                // `Executor::rebuild_epoch`'s doc comment for the full
                // hazard). Folding it in means a key computed under a prior
                // executor lifetime can never coincidentally match this one.
                hasher.write_u64(ctx.rebuild_epoch);
                let shadow_key = hasher.finish();

                // I1: never serve on ANY mismatch — including the `None` a
                // fresh primitive (or a resolution/topology change that
                // reset `shadow_cache_keys`) starts with. A full match means
                // `shadow_maps[slot]`'s PERSISTED texture (never cleared
                // except on resolution change — see `ensure_shadow_map`)
                // already holds exactly this content, so the depth-only
                // batch this caster would otherwise issue is redundant.
                if caster_draws.iter().all(|draw| draw.geometry_content_known && draw.appearance_content_known
                    && (draw.base_color_map.is_none() || draw.rt_texture_content[0].is_some()))
                    && self.shadow_cache_keys[slot] == Some(shadow_key) {
                    continue;
                }
                self.shadow_cache_keys[slot] = Some(shadow_key);

                let shadow_uniforms: Vec<ShadowUniforms> = caster_draws
                    .iter()
                    .map(|d| ShadowUniforms::for_draw(vp, d))
                    .collect();
                let shadow_bindings: Vec<[GpuBinding; 6]> = caster_draws
                    .iter()
                    .zip(&shadow_uniforms)
                    .map(|(d, su)| {
                        [
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::bytes_of(su),
                            },
                            GpuBinding::Buffer {
                                binding: 1,
                                buffer: d.vertices,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 2,
                                buffer: d.instances.unwrap_or(identity_stub),
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 3,
                                buffer: d.weights.unwrap_or(d.vertices),
                                offset: 0,
                            },
                            GpuBinding::Texture { binding: 4, texture: d.base_color_map.unwrap_or(self.dummy_texture.as_ref().expect("ensured")) },
                            GpuBinding::Sampler { binding: 5, sampler: &self.material_samplers[&Self::sampler_cache_key(d.sampler_descs[0])] },
                        ]
                    })
                    .collect();
                let shadow_draws: Vec<manifold_gpu::DepthMsaaDraw> = caster_draws
                    .iter()
                    .zip(&shadow_bindings)
                    .map(|(d, b)| {
                        d.live(manifold_gpu::GpuEncoder::depth_msaa_draw(
                            &shadow_pipeline,
                            b,
                            d.vertex_count,
                            d.instance_count,
                        ))
                    })
                    .collect();
                ctx.gpu_encoder()
                    .native_enc
                    .draw_instanced_depth_only_batch(
                        shadow_map,
                        shadow_ds,
                        &shadow_draws,
                        "node.render_scene shadow",
                    );
            }
        }
    }

    /// BUG-trh7 stage 2, pass 6: the opaque-only camera-space depth prepass
    /// into `opaque_depth_snapshot` — Pass B's depth test source, the RT
    /// shadow-ray pass's depth source, and the atrous_post/firefly guides.
    /// Reuses the shadow pipeline with the camera's view_proj; skipped
    /// entirely when the scene has no transmissive object and no RT.
    pub(super) fn opaque_depth_snapshot_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        opaque_draws: &[&ObjectDraw<'ctx>],
        has_transmission: bool,
        has_subsurface: bool,
    ) {
        let FramePrelude { view_proj, rt_enabled, .. } = *pre;
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        if has_transmission || rt_enabled || has_subsurface {
            let opaque_depth_pipeline = self.shadow_pipeline.as_ref().expect("ensured above").clone();
            let opaque_depth_ds = self.depth_stencil.as_ref().expect("ensured above");
            let opaque_depth_snapshot = self.opaque_depth_snapshot.as_ref().expect("ensured above");
            let cam_uniforms: Vec<ShadowUniforms> = opaque_draws
                .iter()
                .map(|d| ShadowUniforms::for_draw(view_proj, d))
                .collect();
            let cam_bindings: Vec<[GpuBinding; 6]> = opaque_draws
                .iter()
                .zip(&cam_uniforms)
                .map(|(d, su)| {
                    [
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(su),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: d.vertices,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 2,
                            buffer: d.instances.unwrap_or(identity_stub),
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 3,
                            buffer: d.weights.unwrap_or(d.vertices),
                            offset: 0,
                        },
                        GpuBinding::Texture { binding: 4, texture: d.base_color_map.unwrap_or(self.dummy_texture.as_ref().expect("ensured")) },
                        GpuBinding::Sampler { binding: 5, sampler: &self.material_samplers[&Self::sampler_cache_key(d.sampler_descs[0])] },
                    ]
                })
                .collect();
            let cam_draws: Vec<manifold_gpu::DepthMsaaDraw> = opaque_draws
                .iter()
                .zip(&cam_bindings)
                .map(|(d, b)| {
                    d.live(manifold_gpu::GpuEncoder::depth_msaa_draw(
                        &opaque_depth_pipeline,
                        b,
                        d.vertex_count,
                        d.instance_count,
                    ))
                })
                .collect();
            ctx.gpu_encoder()
                .native_enc
                .draw_instanced_depth_only_batch(
                    opaque_depth_snapshot,
                    opaque_depth_ds,
                    &cam_draws,
                    "node.render_scene E2a opaque depth snapshot",
                );
        }
    }

    /// IMPORT_FIDELITY_DESIGN.md D2/F-P1: run the split-sum IBL convolution
    /// passes. Call ONLY after [`Self::ensure_ibl_resources`].
    ///
    /// The BRDF LUT is envmap-independent (view/roughness-only) — built
    /// EXACTLY ONCE per device, gated by `brdf_lut_built`, a genuine cache
    /// hit every frame after the first (see the `ibl_brdf_lut_builds_once`
    /// gpu-proofs test).
    ///
    /// Cached convolution follows logical environment content. Recopying the
    /// same pixels into recycled storage leaves the persisted convolution valid.
    /// A content stamp includes resource identity and executor lifetime; unknown
    /// metadata always recomputes. Physical input bindings are resolved afresh.
    pub(super) fn run_ibl_convolution(
        &mut self,
        gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
        sampler: &manifold_gpu::GpuSampler,
        envmap: Option<&manifold_gpu::GpuTexture>,
        envmap_content: Option<ContentVersion>,
        rebuild_epoch: u64,
    ) {
        if !self.brdf_lut_built {
            let lut = self.brdf_lut.as_ref().expect("ensured");
            let pipeline = self.ibl_brdf_lut_pipeline.as_ref().expect("ensured");
            let uniforms = LutUniforms {
                width: BRDF_LUT_SIZE,
                height: BRDF_LUT_SIZE,
                _pad0: 0.0,
                _pad1: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Texture { binding: 1, texture: lut },
                ],
                [BRDF_LUT_SIZE.div_ceil(16), BRDF_LUT_SIZE.div_ceil(16), 1],
                "node.render_scene ibl brdf lut",
            );
            self.brdf_lut_built = true;
        }

        let Some(envmap) = envmap else {
            // Unwired ⇒ next wire-up must re-convolve unconditionally, per
            // I2 (never serve a stale key across an unwired gap).
            self.ibl_cache_key = None;
            return;
        };

        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        envmap_content.hash(&mut hasher);
        hasher.write_u64(rebuild_epoch);
        hasher.write_u8(u8::from(self.prefiltered_sheen.is_some()));
        let ibl_key = hasher.finish();
        if envmap_content.is_some() && self.ibl_cache_key == Some(ibl_key) {
            // I2: cache hit — the persisted prefiltered-specular/irradiance
            // textures already hold this exact envmap's convolution.
            return;
        }
        self.ibl_cache_key = Some(ibl_key);

        let src_width = envmap.width;
        let src_height = envmap.height;

        // Irradiance: one dispatch, whole 32x16 texture.
        {
            let irradiance = self.irradiance_map.as_ref().expect("ensured");
            let pipeline = self.ibl_irradiance_pipeline.as_ref().expect("ensured");
            let uniforms = IrradianceUniforms {
                dst_width: IRRADIANCE_WIDTH,
                dst_height: IRRADIANCE_HEIGHT,
                src_width,
                src_height,
            };
            gpu.native_enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Texture { binding: 1, texture: envmap },
                    GpuBinding::Sampler { binding: 2, sampler },
                    GpuBinding::Texture { binding: 3, texture: irradiance },
                ],
                [IRRADIANCE_WIDTH.div_ceil(8), IRRADIANCE_HEIGHT.div_ceil(8), 1],
                "node.render_scene ibl irradiance",
            );
        }

        // Prefiltered specular chain: one dispatch per mip, each writing
        // into a single-mip VIEW of the chain (`GpuTexture::mip_level_view`
        // — the hardware `generate_mipmaps` box-filter blit cannot express
        // GGX importance convolution, so each level is its own compute
        // pass rather than a derived box-filtered downsample).
        for (chain, sheen) in [
            (self.prefiltered_specular.as_ref(), 0.0),
            (self.prefiltered_sheen.as_ref(), 1.0),
        ] {
            let Some(chain) = chain else { continue };
            let pipeline = self.ibl_prefilter_pipeline.as_ref().expect("ensured");
            for mip in 0..PREFILTER_MIP_COUNT {
                let mip_w = (PREFILTER_BASE_WIDTH >> mip).max(1);
                let mip_h = (PREFILTER_BASE_HEIGHT >> mip).max(1);
                let roughness = mip as f32 / (PREFILTER_MIP_COUNT - 1) as f32;
                let view = chain.mip_level_view(mip, mip_w, mip_h);
                let uniforms = PrefilterUniforms {
                    dst_width: mip_w,
                    dst_height: mip_h,
                    src_width,
                    src_height,
                    roughness,
                    sheen,
                    _pad1: 0.0,
                    _pad2: 0.0,
                };
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&uniforms),
                        },
                        GpuBinding::Texture { binding: 1, texture: envmap },
                        GpuBinding::Sampler { binding: 2, sampler },
                        GpuBinding::Texture { binding: 3, texture: &view },
                    ],
                    [mip_w.div_ceil(16), mip_h.div_ceil(16), 1],
                    "node.render_scene ibl prefilter mip",
                );
            }
        }
    }
}

/// Per-(caster, object) uniform for the shadow depth pass
/// (`shaders/shadow_depth.wgsl`). The vertex shader composes
/// `light_view_proj · model · position` and writes only depth. 192 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ShadowUniforms {
    pub(super) light_view_proj: [[f32; 4]; 4],
    pub(super) model: [[f32; 4]; 4],
    /// `(gain, weights_wired, 0, 0)` for the depth visibility discard.
    pub(super) appearance: [f32; 4],
    pub(super) alpha: [f32; 4],
    pub(super) uv_m: [f32; 4],
    pub(super) uv_t: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<ShadowUniforms>() == 192);

impl ShadowUniforms {
    pub(super) fn for_draw(view_proj: [[f32; 4]; 4], draw: &ObjectDraw<'_>) -> Self {
        Self {
            light_view_proj: view_proj,
            model: draw.uniforms.model,
            appearance: draw.uniforms.appearance,
            alpha: [draw.uniforms.alpha_params[0], draw.uniforms.alpha_params[1],
                draw.uniforms.base_color[3], draw.uniforms.texture_flags[2]],
            uv_m: draw.uniforms.base_color_uv_m,
            uv_t: draw.map_uniforms[0].offset_set,
        }
    }
}
