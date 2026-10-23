//! Cached GPU resource helpers: each ensure_* allocates or reuses one resource before the frame's passes borrow it.

use super::*;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 3: the ensure-cached-GPU-resources block —
    /// every later pass's immutable self-borrow is ensured here first
    /// (depth states, MSAA + resolve targets, denoiser/upscaler, samplers,
    /// dummies, IBL, shadow stubs + maps, RT tracer/masks/params, shafts,
    /// the light ring write). Sets temporal_upscale_active/denoise_active
    /// exactly as the inline code did; rt_irr_needs_reset |= irr_reallocated
    /// stays the one reset-production site in this block.
    #[allow(clippy::too_many_arguments, reason = "BUG-trh7 stage 2 pass method: args are the evaluate() locals the inline block used — destructuring at call sites is the approved shape")]
    pub(super) fn ensure_gpu_resources<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        temporal_upscale_active: &mut bool,
        denoise_active: &mut bool,
        draws: &[ObjectDraw<'ctx>],
        has_transmission: bool,
        rt_ready: bool,
    ) {
        let FramePrelude {
            ref casters, ref light_data, ref shadow_casters,
            depth_wired, wants_shafts_now, velocity_wired, ao_mask_wired,
            denoise_feed, temporal_upscale, native_width, native_height,
            width, height, rt_enabled, rt_shadows_enabled, rt_trace_w, rt_trace_h,
            denoise_aux_ready, ..
        } = *pre;
        let preparing = ctx.gpu.as_ref().is_some_and(|gpu| gpu.preparing);
        let rt_enabled = rt_enabled || preparing;
        let has_subsurface = draws.iter().any(|draw| draw.subsurface_binding[0] > 0.0);
        // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2a: peek color format BEFORE the gpu_encoder block below borrows ctx.
        let opaque_scene_color_target_format =
            has_transmission.then(|| ctx.outputs.texture_2d("color").map(|t| t.format)).flatten();
        let has_casters = !casters.is_empty();
        {
            let gpu = ctx.gpu_encoder();
            if self.depth_stencil.is_none() {
                self.depth_stencil = Some(gpu.device.create_depth_stencil_state(
                    &manifold_gpu::GpuDepthStencilDesc {
                        compare: manifold_gpu::GpuCompareFunction::Greater,
                        write_enabled: true,
                    },
                ));
            }
            // Reversed-Z camera depth uses the same Greater compare, write disabled
            // — the sorted transparent group's depth-stencil state.
            if self.blend_depth_stencil.is_none() {
                self.blend_depth_stencil = Some(gpu.device.create_depth_stencil_state(
                    &manifold_gpu::GpuDepthStencilDesc {
                        compare: manifold_gpu::GpuCompareFunction::Greater,
                        write_enabled: false,
                    },
                ));
            }
            if has_transmission && self.transmission_depth_stencil.is_none() {
                self.transmission_depth_stencil = Some(gpu.device.create_depth_stencil_state(
                    &manifold_gpu::GpuDepthStencilDesc {
                        compare: manifold_gpu::GpuCompareFunction::GreaterEqual,
                        write_enabled: false,
                    },
                ));
            }
            // SCENE_RENDER_MODE_DESIGN.md D8: a Points-active frame draws
            // point primitives, which the memoryless-attachment pass budget
            // can't carry — swap the MSAA pair for real storage (see
            // `ensure_msaa_targets`). `points_active` is also the pass-level
            // aux gate below.
            let points_active = draws.iter().any(|d| d.points);
            self.ensure_msaa_targets(gpu.device, width, height, points_active);
            if depth_wired || wants_shafts_now || self.gizmo_depth_wanted {
                self.ensure_depth_resolve_scratch(gpu.device, width, height);
            }
            // GBUFFER_DESIGN.md section 2 D1/D5 (P2): the velocity aux-MRT
            // memoryless target is allocated ONLY when wired this frame —
            // this call site (not `ensure_msaa_targets`, which always runs)
            // is the actual zero-cost-when-unwired enforcement point.
            if velocity_wired {
                self.ensure_velocity_msaa_target(gpu.device, width, height);
            }
            // RAYTRACING_DESIGN.md section 12 AM1: same zero-cost-when-
            // unwired enforcement point for the ao_mask aux target.
            if ao_mask_wired {
                self.ensure_ao_mask_msaa_target(gpu.device, width, height);
            }
            // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): the four denoise
            // G-buffer MSAA targets, allocated ONLY when `denoise_feed` is on
            // — zero cost otherwise.
            if denoise_feed {
                self.ensure_denoise_msaa_targets(gpu.device, width, height);
            }
            // RAYTRACING_DESIGN.md section 8.2 D22 (T2-B): the render-res color
            // scratch Pass A resolves into, ensured whenever the raw
            // `temporal_upscale` param is on — `width`/`height` are ALREADY
            // shadowed to render res above regardless of hardware
            // availability (so they stay in lockstep with
            // `output_canvas_scale`'s D22 branch for `depth`/`velocity`,
            // which has no per-frame device to query), so Pass 2 always
            // needs a render-res target to resolve into. `ensure_rt_
            // temporal_upscaler`'s return folds in hardware availability —
            // `temporal_upscale_active` (declared before this block,
            // assigned here) is `false` on a device without MetalFX
            // Temporal, and ONLY gates the final upscale+blit at this fn's
            // tail: Pass 2 still renders correctly into the scratch either
            // way, it just never reaches `native_color` that frame (logged
            // once by the ensure call) rather than crashing on a dims
            // mismatch.
            if temporal_upscale {
                self.ensure_rt_temporal_color_scratch(gpu.device, width, height);
                *temporal_upscale_active =
                    self.ensure_rt_temporal_upscaler(gpu.device, width, height, native_width, native_height);
            }
            // RAYTRACING_DESIGN.md section 17.5 DN-F/DN-G: denoiser takes
            // priority over the plain temporal scaler on RT scenes (DN2).
            // `denoise_active` is true when RT is producing output AND the
            // denoiser is available; the denoiser's encode writes directly to
            // `native_color` — no intermediate blit needed.
            // When the denoiser is active but temporal_upscale is off (1:1
            // denoise), the forward pass renders into a native-res scratch
            // so the denoiser's input and output don't alias.
            let denoise_wanted = rt_enabled && rt_ready && denoise_aux_ready && denoiser_available();
            // Gate-block diagnostic (2026-08-08, Peter's "does nothing"
            // report): the conditions fail silently — name the blocker
            // once per transition instead of leaving the feature inert.
            // aux_ready=false is expected for exactly the one live-flip
            // frame (pre-flip plan, D17 transition — BUG-qtkq); a warn that
            // persists past that names a real plan-staleness bug.
            if denoise_feed && !denoise_wanted && !self.denoise_gate_blocked_logged {
                self.denoise_gate_blocked_logged = true;
                log::warn!(
                    "node.render_scene: rt_denoise_feed is on but the denoiser gate is blocked — rt_enabled={rt_enabled}, rt_ready={rt_ready}, aux_ready={denoise_aux_ready}, denoiser_available={}",
                    denoiser_available()
                );
            }
            if denoise_wanted {
                self.denoise_gate_blocked_logged = false;
                if !temporal_upscale {
                    // 1:1 denoise: need a scratch at native res so the
                    // denoiser can read the forward-pass output and write
                    // to native_color without aliasing.
                    self.ensure_rt_temporal_color_scratch(gpu.device, native_width, native_height);
                }
                *denoise_active =
                    self.ensure_denoiser(gpu.device, width, height, native_width, native_height);
                if *denoise_active {
                    // DN2: the denoiser REPLACES the plain temporal
                    // scaler — fused denoise+upscale is the one path
                    // to native_color.
                    *temporal_upscale_active = false;
                }
            }
            self.ensure_sampler(gpu.device);
            // GLB_XFAIL_BURNDOWN_DESIGN.md D3: ensure every distinct
            // per-map-family sampler this frame's draws need. Runs before
            // Pass 2 builds `binding_sets`, so every lookup there is
            // guaranteed a cache hit.
            for draw in draws {
                for desc in draw.sampler_descs {
                    self.ensure_material_sampler(gpu.device, desc);
                }
            }
            self.ensure_dummy_texture(gpu.device);
            self.ensure_dummy_emissive_buffer(gpu.device);
            // IMPORT_FIDELITY_DESIGN.md D2/F-P1: split-sum IBL resources —
            // always ensured (fixed size, allocated once) so
            // `@binding(16..18)` always has something valid, regardless of
            // whether `envmap` is wired this frame.
            self.ensure_ibl_resources(gpu.device);
            if self.prefiltered_sheen.is_none()
                && draws.iter().any(|d| d.uniforms.sheen_params[..3].iter().any(|v| *v > 0.0))
            {
                self.prefiltered_sheen = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: PREFILTER_BASE_WIDTH,
                    height: PREFILTER_BASE_HEIGHT,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::SHADER_READ | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
                    label: "node.render_scene sheen environment",
                    mip_levels: PREFILTER_MIP_COUNT,
                }));
            }
            // Identity instance stub (D11) — bound to any object's
            // instances_n when unwired, both in the main pass and every
            // caster's shadow pass below.
            self.ensure_identity_instance_stub(gpu.device);
            // Shadow ABI stubs (comparison sampler + 1×1 dummy depth) are
            // always needed — the main shader declares @binding(10..14) even
            // in a scene with no casters. The shadow *pipeline* + per-caster
            // maps are created only when a caster exists (unwired = zero cost).
            self.ensure_shadow_binding_stubs(gpu.device);
            if has_casters && (!(rt_enabled && rt_ready && rt_shadows_enabled)
                || draws.iter().any(ObjectDraw::routes_to_transparent)) {
                self.ensure_shadow_pass(gpu.device);
                self.ensure_shadow_maps(gpu.device, casters);
            } else if has_transmission || rt_enabled || has_subsurface {
                // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2a: the transmissive
                // opaque-depth prepass below reuses `shadow_pipeline` (a
                // depth-only pipeline fed the camera's `view_proj` instead
                // of a light's) even when there are zero shadow casters.
                // RAYTRACING_DESIGN.md RT-D3 (P1-part-2): the RT shadow-ray
                // pass reads the SAME prepass as its depth source, so an
                // RT-enabled scene needs it too, independent of
                // `has_transmission`.
                self.ensure_shadow_pass(gpu.device);
            }
            // E2a/RT-D3: allocate the Depth32Float snapshot Pass B (E2a)
            // and the RT shadow-ray pass (RT-D3) both read; the opaque-
            // scene-color snapshot is E2a-only. Both must happen HERE
            // (this block's `{ let gpu = ... }` scope, before
            // `identity_stub` and friends take long-lived immutable
            // borrows of `self` below) — the same `&mut self` ensure calls
            // deferred to right before Pass 2 fetches `target` (the natural
            // place otherwise) would conflict with those borrows under NLL.
            if has_transmission || rt_enabled || has_subsurface {
                self.ensure_opaque_depth_snapshot(gpu.device, width, height);
            }
            if has_transmission {
                self.transmissive_depth_error = self.ensure_transmissive_depth_scratch(gpu.device, width, height).err();
            }
            if has_subsurface {
                self.subsurface_pass.ensure(gpu.device, width, height, draws.len());
            }
            if has_transmission
                && let Some(format) = opaque_scene_color_target_format
            {
                self.ensure_opaque_scene_color(gpu.device, width, height, format);
            }
            // BUG-310: the tracer's 3 raw-MSL pipeline compiles (~30ms)
            // must land in the node's first-evaluate warmup window (the
            // frame-0 JIT window every frame-time gate already exempts),
            // NOT on the frame a performer flips `rt_enabled` mid-set —
            // the BUG-037 prewarm rule. Guarded by `is_none()` inside, so
            // per-frame cost after the first evaluate is one branch.
            self.ensure_rt_tracer(gpu.device);
            // RAYTRACING_DESIGN.md RT-D3/RT-P2: masks + params buffers +
            // irradiance targets, ensured here for the same NLL borrow
            // reason as above.
            if rt_enabled {
                self.ensure_rt_visibility(gpu.device, rt_trace_w, rt_trace_h, width, height, casters.len());
                self.ensure_rt_masks(gpu.device, rt_trace_w, rt_trace_h, width, height);
                self.ensure_rt_params_buffer(gpu.device);
                let irr_reallocated = self.ensure_rt_irradiance(gpu.device, rt_trace_w, rt_trace_h, width, height);
                self.ensure_rt_accumulate_params_buffer(gpu.device);
                self.ensure_rt_atrous_params_buffer(gpu.device);
                // The firefly resolve aliases reflection prefilter scratch,
                // refreshed by ensure_rt_irradiance at render dimensions.
                self.ensure_rt_firefly_params_buffer(gpu.device);
                // RT-Stage-3 P4 (BUG-eytk): atrous_post params buffer —
                // same one-buffer lifetime as the other params buffers.
                self.ensure_rt_atrous_post_params_buffer(gpu.device);
                self.rt_irr_needs_reset = self.rt_irr_needs_reset || irr_reallocated;
            }
            // VOLUMETRIC_LIGHT_DESIGN.md D1/D3 (P2): the whole feature's
            // real GPU cost gate. `wants_shafts_now == false` (the default)
            // means none of these run and none of these textures/pipelines
            // are ever allocated — the "unwired = zero cost" contract
            // extended from fog to shafts (V1).
            if wants_shafts_now {
                self.ensure_shaft_pipelines(gpu.device);
                let half_w = width.div_ceil(2).max(1);
                let half_h = height.div_ceil(2).max(1);
                self.ensure_shaft_half_res(gpu.device, half_w, half_h);
                if !depth_wired {
                    self.ensure_shaft_depth_internal(gpu.device, width, height);
                }
            }
            // The editor's gizmo lines test against this frame's depth: keep the
            // internal copy whether or not `depth` is wired.
            if self.gizmo_depth_wanted {
                self.ensure_shaft_depth_internal(gpu.device, width, height);
            }
            // Rotate + (re)size the light ring buffer, then write this
            // frame's lights and, after them, its caster table into the
            // current ring slot. Rotating across FRAMES_IN_FLIGHT buffers
            // guarantees the CPU never overwrites a buffer an in-flight frame
            // is still reading. The table starts on a 256-byte boundary so it
            // can be bound at an offset; it always has room for one entry so
            // the binding stays valid with no casters.
            let caster_size = std::mem::size_of::<manifold_gpu::ShadowCaster>() as u64;
            let light_bytes = (light_data.len() * std::mem::size_of::<[f32; 4]>()) as u64;
            let caster_offset = light_bytes.next_multiple_of(256);
            let needed = caster_offset + caster_size * (shadow_casters.len().max(1) as u64);
            if self.light_buffers.len() < FRAMES_IN_FLIGHT || self.light_capacity < needed {
                let cap = needed.max(self.light_capacity).max(64).next_power_of_two();
                self.light_buffers = (0..FRAMES_IN_FLIGHT)
                    .map(|_| gpu.device.create_buffer_shared(cap))
                    .collect();
                self.light_capacity = cap;
            }
            self.light_frame = (self.light_frame + 1) % FRAMES_IN_FLIGHT;
            self.caster_offset = caster_offset;
            unsafe {
                let ring = &self.light_buffers[self.light_frame];
                ring.write(0, bytemuck::cast_slice(light_data));
                // Each raster entry describes the layer it really has: a
                // caster past the array (memory refused to grow it) gets
                // texel 0, which the shaders read as unshadowed, and one
                // asking for more than the array side renders and samples
                // at the side.
                let (side, layers) = self.shadow_maps.as_ref()
                    .map_or((0, 0), |maps| (maps.side, maps.layers.len()));
                for (slot, caster) in shadow_casters.iter().enumerate() {
                    let mut entry = *caster;
                    entry.shadow[2] = if slot < layers {
                        entry.shadow[2].max(1.0 / side as f32)
                    } else {
                        0.0
                    };
                    ring.write(caster_offset + caster_size * slot as u64, bytemuck::bytes_of(&entry));
                }
            }
        }
    }

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
            // Bound where a plain depth texture must be valid but nothing
            // real exists. RENDER_TARGET | SHADER_READ, like the real maps,
            // so no depth format needs a render-target-only exception (AGX
            // 0x78 guard).
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
        if self.dummy_depth_array.is_none() {
            self.dummy_depth_array = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Depth32Float,
                dimension: manifold_gpu::GpuTextureDimension::D2Array,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                    | manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene dummy shadow maps",
                mip_levels: 1,
            }));
        }
        if self.dummy_mask_array.is_none() {
            self.dummy_mask_array = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2Array,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ,
                label: "node.render_scene dummy RT shadow mask",
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

    /// Ensure the shadow-map array has a layer per caster at the largest
    /// requested resolution. It grows when either grows and is reused
    /// otherwise; growing drops every cached shadow so each caster renders
    /// fresh. `RENDER_TARGET | SHADER_READ`: rendered as depth targets, then
    /// sampled — both usages at creation (AGX 0x78 guard).
    pub(super) fn ensure_shadow_maps(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        casters: &[manifold_node_engine::scene::light::Light],
    ) {
        let side = casters.iter().map(|l| shadow_map_resolution(l.shadow_resolution)).max().unwrap_or(256);
        let (have_side, have_layers) = self.shadow_maps.as_ref()
            .map_or((0, 0), |maps| (maps.side, maps.layers.len()));
        if side <= have_side && casters.len() <= have_layers {
            return;
        }
        let side = side.max(have_side);
        let layers = casters.len().max(have_layers);
        // A refused size is not retried every frame; a smaller request is.
        if self.shadow_maps_refused.is_some_and(|(s, l)| side >= s && layers >= l) {
            return;
        }
        let texture = match device.try_create_texture(&manifold_gpu::GpuTextureDesc {
            width: side,
            height: side,
            depth: layers as u32,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2Array,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "node.render_scene shadow maps",
            mip_levels: 1,
        }) {
            Ok(texture) => texture,
            Err(error) => {
                // The only limit here is GPU memory: say so instead of
                // dropping shadows quietly. The previous maps stay.
                let megabytes = u64::from(side) * u64::from(side) * 4 * layers as u64 / (1 << 20);
                log::error!(
                    "render_scene: {layers} shadow maps at {side}px need {megabytes} MB of GPU memory \
                     and the allocation failed ({error}); lower Shadow Resolution on the largest lights"
                );
                self.shadow_maps_refused = Some((side, layers));
                return;
            }
        };
        let layers = (0..layers as u32).map(|layer| texture.layer_view(layer)).collect();
        self.shadow_maps = Some(ShadowMapArray { side, texture, layers });
        self.shadow_maps_refused = None;
        self.shadow_cache_keys.clear();
        self.shadow_cache_keys.resize(casters.len().max(have_layers), None);
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

    /// Shadow visibility, four casters per layer (caster c is channel c % 4
    /// of layer c / 4): the trace-res mask, the full-res mask and its
    /// à-trous scratch, and the SV-ACCUM histories. The layer count grows
    /// with the caster count and never shrinks. Growing touches only these
    /// arrays: the other RT histories carry on and only visibility restarts
    /// (`rt_sv_needs_reset`). If the GPU refuses a larger set the old
    /// arrays stay, casters past them go unshadowed, and the refusal is
    /// logged once per size.
    pub(super) fn ensure_rt_visibility(&mut self, device: &manifold_gpu::GpuDevice, trace_w: u32, trace_h: u32, full_w: u32, full_h: u32, casters: usize) {
        let same_size = self.rt_mask_half.as_ref().is_some_and(|t| t.width == trace_w && t.height == trace_h)
            && self.rt_mask_full.as_ref().is_some_and(|t| t.width == full_w && t.height == full_h);
        let have = self.rt_mask_full.as_ref().map_or(0, |t| t.depth);
        let want = (casters.div_ceil(4).max(1) as u32).max(have);
        if same_size && want <= have {
            return;
        }
        if !same_size {
            self.rt_sv_refused = None;
        }
        if self.rt_sv_refused.is_some_and(|refused| want >= refused) {
            return;
        }
        let make = |w: u32, h: u32, layers: u32, format: manifold_gpu::GpuTextureFormat, usage: manifold_gpu::GpuTextureUsage, label: &'static str| {
            device.try_create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: layers,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2Array,
                usage: usage
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE
                    | manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::COPY_SRC,
                label,
                mip_levels: 1,
            })
        };
        let plain = manifold_gpu::GpuTextureUsage::SHADER_READ;
        // Hold histories need render-pass sentinel clears.
        let renderable = manifold_gpu::GpuTextureUsage::RENDER_TARGET;
        let rgba16 = manifold_gpu::GpuTextureFormat::Rgba16Float;
        let r16 = manifold_gpu::GpuTextureFormat::R16Float;
        let build = |layers: u32| -> Result<[manifold_gpu::GpuTexture; 11], String> {
            Ok([
                make(trace_w, trace_h, layers, rgba16, plain, "node.render_scene rt_mask_half (RT-D3/RT-P2 vis)")?,
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_mask_full (RT-D3/RT-P2 vis)")?,
                // RT-T1-D: à-trous ping-pong scratch.
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_mask_full_b (RT-T1-D atrous)")?,
                // SV-ACCUM history pair: same ping clock as rt_irr_history.
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_history_a (SV-ACCUM)")?,
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_history_b (SV-ACCUM)")?,
                // Per-channel first/second visibility moments.
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_m1_a (SV-ACCUM)")?,
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_m1_b (SV-ACCUM)")?,
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_m2_a (SV-ACCUM)")?,
                make(full_w, full_h, layers, rgba16, plain, "node.render_scene rt_sv_m2_b (SV-ACCUM)")?,
                make(full_w, full_h, layers, r16, renderable, "node.render_scene rt_sv_hold_a (SV-ACCUM)")?,
                make(full_w, full_h, layers, r16, renderable, "node.render_scene rt_sv_hold_b (SV-ACCUM)")?,
            ])
        };
        let set = match build(want) {
            Ok(set) => set,
            Err(error) => {
                // Bytes per layer: eight full-res and one trace-res Rgba16
                // texture, two full-res R16 holds.
                let per_layer = u64::from(full_w) * u64::from(full_h) * (8 * 8 + 2 * 2)
                    + u64::from(trace_w) * u64::from(trace_h) * 8;
                let megabytes = per_layer * u64::from(want) / (1 << 20);
                log::error!(
                    "render_scene: ray-traced shadows for {} casters need {megabytes} MB of GPU memory \
                     and the allocation failed ({error}); lights past {} casters go unshadowed",
                    want * 4,
                    if same_size { have * 4 } else { 4 },
                );
                self.rt_sv_refused = Some(want);
                // A new size still needs a set; one layer is the floor.
                match (same_size, build(1)) {
                    (false, Ok(set)) => set,
                    (false, Err(_)) => {
                        self.rt_mask_half = None;
                        self.rt_mask_full = None;
                        self.rt_mask_full_b = None;
                        self.rt_sv_history = [None, None];
                        self.rt_sv_m1_history = [None, None];
                        self.rt_sv_m2_history = [None, None];
                        self.rt_sv_hold_history = [None, None];
                        return;
                    }
                    (true, _) => return,
                }
            }
        };
        let [mask_half, mask_full, mask_full_b, h_a, h_b, m1_a, m1_b, m2_a, m2_b, hold_a, hold_b] = set;
        self.rt_mask_half = Some(mask_half);
        self.rt_mask_full = Some(mask_full);
        self.rt_mask_full_b = Some(mask_full_b);
        self.rt_sv_history = [Some(h_a), Some(h_b)];
        self.rt_sv_m1_history = [Some(m1_a), Some(m1_b)];
        self.rt_sv_m2_history = [Some(m2_a), Some(m2_b)];
        self.rt_sv_hold_history = [Some(hold_a), Some(hold_b)];
        // A new size is covered by the full reset its irradiance histories
        // trigger; a same-size growth restarts visibility alone.
        self.rt_sv_needs_reset = true;
    }

    /// Trace-res and full-res sun-transmission tint targets plus the tint
    /// à-trous scratch, resized with the scene's own output resolution.
    /// Shadow visibility lives in `ensure_rt_visibility`.
    pub(super) fn ensure_rt_masks(&mut self, device: &manifold_gpu::GpuDevice, trace_w: u32, trace_h: u32, full_w: u32, full_h: u32) {
        if self.rt_mask_width == full_w
            && self.rt_mask_height == full_h
            && self.rt_mask_trace_w == trace_w
            && self.rt_mask_trace_h == trace_h
            && self.rt_svt_full.is_some()
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
        // RT-TL-C (section 16 TL5): sun-transmission tint, written by the
        // mask dispatch.
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
        // Reflection scratch also serves as the later MSAA scene-color resolve.
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
