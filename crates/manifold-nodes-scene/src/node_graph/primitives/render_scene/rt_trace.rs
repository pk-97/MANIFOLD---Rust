//! The RT trace and accumulate pass: shadow, AO, GI and reflection rays, temporal accumulation (the single history ping flip), and the a-trous denoise.

use super::*;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 7b: the RT trace + accumulate half — trace
    /// gate, emissive stats binding, rt_casters, mask/lighting dispatches,
    /// upsample, hit-dist extract, the two atrous passes, lighting/geo
    /// keys + gestures, accumulate_irradiance, THE ping flip (single flip
    /// clock for every history pair), atrous_post, the emissive pseudo-
    /// light append, and the RT_CAPTURE snapshot. Reset expression C stays
    /// verbatim (reset_decision || rt_just_resumed || take(rt_irr_needs_
    /// reset) || toggle_flipped) — its only consumer is the accumulator.
    /// Returns false only for the degenerate-camera abort (color
    /// unwritten this frame, exactly as the inline return).
    #[allow(clippy::too_many_arguments, reason = "BUG-trh7 stage 2 pass method: args are the evaluate() locals the inline block used — destructuring at call sites is the approved shape")]
    pub(super) fn rt_trace_accumulate<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        rt_ready: bool,
        rt_rendered_this_frame: &mut bool,
        irr_filtered_valid: &mut bool,
        denoise_active: bool,
        rt_just_resumed: bool,
        objects: &[manifold_gpu::raytrace::RtObjectGeometry<'ctx>],
        opaque_draws: &[&ObjectDraw<'ctx>],
        gi_materials_data: &[manifold_gpu::raytrace::GiMaterial],
        alpha_textures: &[&manifold_gpu::GpuTexture],
        topo_key: u64,
        content_key: u64,
        envmap_content: Option<ContentVersion>,
        shaft_light_data: &mut Vec<[f32; 4]>,
        shaft_light_count: &mut u32,
    ) -> bool {
        let FramePrelude {
            ref casters, ref shadow_casters,
            cam, view_proj, prev_view_proj, cam_motion,
            width, height, rt_trace_w, rt_trace_h, rtq,
            rt_shadows_enabled, rt_ao_enabled, rt_gi_enabled, rt_reflections,
            denoise_aux_ready, spec_hit_dist_out, envmap_wired, atmosphere,
            denoise_iterations, denoise_strength, toggle_flipped, reset_decision, ..
        } = *pre;
        let rt_source_trace_generation = ctx.rebuild_epoch;
        let gpu = ctx.gpu_encoder();
            // Only a successfully encoded update authorizes this frame's consumers.
            if rt_ready {
                if rt_source_trace_enabled()
                    && self.rt_source_trace_last_admission != Some((topo_key, content_key))
                {
                    log::info!(
                        "[RT-SOURCE] trace-admission action=trace rebuild_epoch={} resident_accel={:#x} resident_topo={:?} resident_content={:?} current_topo={topo_key:#x} current_content={content_key:#x} objects={}",
                        rt_source_trace_generation, self.rt_accel.as_ref().map(|accel| accel as *const _ as usize).unwrap_or(0), self.rt_accel_topo_key, self.rt_accel_content_key, objects.len()
                    );
                    for (index, (object, draw)) in objects.iter().zip(opaque_draws.iter()).enumerate() {
                        log::info!(
                            "[RT-SOURCE] trace-object index={} vertex_identity={} vertex_size={} instances_identity={} instance_count={} instances_addr={:#x}",
                            index, object.vertex_buffer.identity_key(), object.vertex_buffer.size,
                            object.instances_buffer.map(|buffer| buffer.identity_key()).unwrap_or(0),
                            object.instance_slots, object.instances_addr
                        );
                        log::info!("[RT-SOURCE] trace-content index={} vertices={:?} instances={:?}", index, draw.vertices_content, draw.instances_content);
                    }
                    self.rt_source_trace_last_admission = Some((topo_key, content_key));
                }

                // The shared caster table (one entry per caster slot) sits
                // after the lights in this frame's ring slot.
                let rt_casters: &[manifold_gpu::ShadowCaster] = shadow_casters;
                // The visibility arrays hold four casters per layer; casters
                // past them (memory refused to grow) are not traced and read
                // as unshadowed. No arrays at all (refused at a new size):
                // skip the RT pass this frame.
                let Some(sv_capacity) = self.rt_mask_full.as_ref().map(|mask| mask.depth as usize * 4) else {
                    return false;
                };
                let caster_count = rt_casters.len().min(sv_capacity) as u32;
                let caster_addr = self.light_buffers[self.light_frame].gpu_address() + self.caster_offset;
                let Some(inv_view_proj) = mat4_inverse(view_proj) else {
                    // A degenerate camera projection — no camera this file
                    // builds produces one; skip the RT pass rather than trace
                    // against garbage (leaves the mask at its previous
                    // content, harmless — `rt_enabled` scenes with a sane
                    // camera never hit this).
                    return false;
                };

                // RT-A3a: split dispatch with fuse-when-same-res rule (D16a).
                // Run two dispatches ONLY when mask trace size differs from
                // lighting trace size (shadow native while lighting stays
                // half). Otherwise fold shadow back into the lighting
                // dispatch — one dispatch, monolithic perf.
                // RT_QUALITY_SETTINGS_DESIGN.md D4: one ray-resolution
                // fraction for both dispatches (the per-term native split
                // went out with MANIFOLD_RT_NATIVE_TERMS) — the split below
                // is structurally unreachable and always fuses today.
                let trace_w = rt_trace_w;
                let trace_h = rt_trace_h;
                let (mask_half_w, mask_half_h) = if rt_shadows_enabled {
                    (trace_w, trace_h)
                } else {
                    // Shadow disabled: mask dispatch still runs at 1x1 for the
                    // fold-in case (lighting_shadow_spp = 0), sizing doesn't matter.
                    (1, 1)
                };
                let (light_half_w, light_half_h) = (trace_w, trace_h);
                let mask_sizes_differ =
                    mask_half_w != light_half_w || mask_half_h != light_half_h;

                // ED2 (RAYTRACING_DESIGN.md section 14.2): the flat ambient no
                // longer enters the kernel (`ShadowRayParams` has no
                // `ambient_color`); each material's Ambient knob is applied
                // consumer-side in `render_scene.wgsl`'s `rt_or_flat_ambient`
                // via its own `scene_params[1]` — knob at 0 stays true black
                // on both paths.

                // RT-TL-C (section 16 TL5): find the ONE sun caster whose
                // rgb tint fills out_svt — SVT_SLOT_NONE when no sun exists.
                let svt_slot = rt_svt_slot(casters)
                    .unwrap_or(manifold_gpu::raytrace::SVT_SLOT_NONE);

                // RT-A3a: mask params — built for the split case when trace
                // sizes differ. When sizes match, unused (shadow folded into
                // lighting dispatch). Gated on rt_shadows_enabled: if shadows
                // are off, this dispatch is skipped entirely.
                let mask_shadow_spp: u32 = if rt_shadows_enabled { rtq.shadow_spp } else { 0 };
                self.rt_tracer.as_mut().expect("ensured").set_secondary_lighting(
                    &self.light_buffers[self.light_frame],
                    self.irradiance_map.as_ref().expect("ensured"),
                    self.brdf_lut.as_ref().expect("ensured"),
                    self.prefiltered_sheen.as_ref().unwrap_or_else(|| self.prefiltered_specular.as_ref().expect("ensured")),
                );
                let mask_params = manifold_gpu::raytrace::ShadowRayParams::new(
                    caster_count,
                    caster_addr,
                    mask_shadow_spp,
                    self.jitter_frame_index,
                    [mask_half_w, mask_half_h],
                    [width, height],
                    0.0,    // ao_radius (unused)
                    0,      // ao_spp
                    0,      // gi_spp
                    // RT-T1-B: camera_pos unused by mask dispatch (no primary ray),
                    // but kept for API compatibility.
                    cam.pos,
                    inv_view_proj,
                    0,      // refl_spp
                    0.6,    // refl_max_roughness (unused)
                    0.1,    // refl_rough_band (unused)
                    svt_slot,
                )
                // RT_INSTANCING_DESIGN.md D11: instance_id-indexed
                // normal/gi-material reads land in the slot rows [N, N+Σ).
                .with_slot_row_base(objects.len() as u32)
                .with_lights(pre.light_count, self.light_buffers[self.light_frame].gpu_address());

                // RT-A3a: lighting params (AO + GI + reflection + normal).
                // D16a fuse rule: shadow_spp=1 when mask and lighting trace
                // sizes match (one fused dispatch at monolithic perf); 0 when
                // split (separate mask dispatch handles shadow at its own size).
                // Gated on rt_shadows_enabled.
                let lighting_shadow_spp: u32 =
                    if rt_shadows_enabled && !mask_sizes_differ { rtq.shadow_spp } else { 0 };
                // RT_QUALITY_SETTINGS_DESIGN.md D5: per-frame spp from the
                // quality column, gated on the per-term toggles — off → 0
                // (kernel skips the gather). I2: a tier is never 0.
                let ao_spp = if rt_ao_enabled { rtq.ao_spp } else { 0 };
                let gi_spp = if rt_gi_enabled { rtq.gi_spp } else { 0 };
                let lighting_params = manifold_gpu::raytrace::ShadowRayParams::new(
                    caster_count,
                    caster_addr,
                    lighting_shadow_spp,
                    self.jitter_frame_index,
                    [light_half_w, light_half_h],
                    [width, height],
                    AO_RADIUS_WORLD_UNITS,
                    ao_spp,
                    gi_spp,
                    // RT-T1-B: the primary-visibility-ray origin for the
                    // real interpolated-vertex-normal fetch (AO/GI cosine
                    // sampling) — the SAME camera eye `render_scene.wgsl`'s
                    // raster pass shades from.
                    cam.pos,
                    inv_view_proj,
                    // RT-R1 (section 9.3): reflection config — T4 wires refl_spp to
                    // the rt_reflections scene param, gated on rt_enabled;
                    // T5 tunes the spp/roughness-band constants. 0.6/0.1 are
                    // the RD7 starting constants.
                    if rt_reflections { rtq.refl_spp } else { 0 },
                    0.6,
                    0.1,
                    svt_slot,
                )
                // RT_INSTANCING_DESIGN.md D11: instance_id-indexed
                // normal/gi-material reads land in the slot rows [N, N+Σ).
                .with_slot_row_base(objects.len() as u32)
                .with_lights(pre.light_count, self.light_buffers[self.light_frame].gpu_address())
                .with_env_rotation(pre.env_rotation);
                // RS-B: gi_materials_data already built above (same order as
                // `objects` + `accel`), reused for the GPU upload here.
                // RT_INSTANCING_DESIGN.md D11: canonical per-object rows at
                // [0, N) (the built block, one memcpy) + per-slot rows at
                // [N, N+Σ) duplicating each object's row object-major
                // (same slot addressing as `ensure_normal_sources`), so
                // instance_id-indexed kernel reads via params.slot_row_base
                // land on the slot rows while object-indexed readers use
                // the canonical block. With every object at ≤ 1 slot the
                // slot region is a verbatim copy of the canonical block.
                let gi_materials_buffer = self.rt_gi_materials.as_ref().expect("ensured above");
                self.rt_gi_upload.clear();
                self.rt_gi_upload.extend_from_slice(gi_materials_data);
                for (material, object) in gi_materials_data.iter().zip(objects) {
                    let slots = if object.instances_addr != 0 { object.instance_slots.max(1) } else { 1 };
                    self.rt_gi_upload.extend(std::iter::repeat_n(*material, slots as usize));
                }
                // GiMaterial is repr(C), initialized f32 payload including explicit padding.
                let material_bytes = unsafe {
                    std::slice::from_raw_parts(
                        self.rt_gi_upload.as_ptr().cast::<u8>(),
                        std::mem::size_of_val(self.rt_gi_upload.as_slice()),
                    )
                };
                manifold_gpu::raytrace::encode_inline_copy(
                    gpu.device, gpu.native_enc, gi_materials_buffer, 0, material_bytes,
                );
                let obj_motion_buffer = self.rt_obj_motion.as_ref().expect("ensured above");
                const IDENTITY_M4: [[f32; 4]; 4] = [
                    [1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0],
                ];
                self.rt_motion_upload.clear();
                for draw in opaque_draws {
                    self.rt_motion_upload.push(mat4_inverse(draw.uniforms.model)
                        .map(|inv| mat4_mul(draw.uniforms.prev_model, inv))
                        .unwrap_or(IDENTITY_M4));
                }
                manifold_gpu::raytrace::encode_inline_copy(
                    gpu.device, gpu.native_enc, obj_motion_buffer, 0,
                    bytemuck::cast_slice(&self.rt_motion_upload),
                );
                let tracer = self.rt_tracer.as_ref().expect("ensured above");
                // A cut, seek or topology change voids the measured trace
                // cost, so the trace restarts from small tiles (BUG-k1n2w).
                if reset_decision {
                    tracer.restart_trace_tiles();
                }
                let accel = self.rt_accel.as_ref().expect("rt_ready implies rt_accel.is_some()");
                // P4a: the emissive count/mean/area/local flag live in the
                // GPU-written stats buffer, not CPU params. Bind the table's
                // stats; the tracer's shared zero buffer when there is no
                // table (kernel guards on entry_count == 0).
                let emissive_stats = accel.emissive_table.as_ref()
                    .map(|t| &t.stats)
                    .unwrap_or_else(|| tracer.zero_emissive_stats());
                let params_buffer = self.rt_params_buffer.as_ref().expect("ensured above");
                let caster_ring = &self.light_buffers[self.light_frame];
                let normal_sources_buffer = self.rt_normal_sources.as_ref().expect("ensured above");
                let depth_tex = self.opaque_depth_snapshot.as_ref().expect("ensured above");
                let mask_half = self.rt_mask_half.as_ref().expect("ensured above");
                let mask_full = self.rt_mask_full.as_ref().expect("ensured above");
                let svt_half = self.rt_svt_half.as_ref().expect("ensured above");
                let svt_full = self.rt_svt_full.as_ref().expect("ensured above");
                let irr_half = self.rt_irr_half.as_ref().expect("ensured above");
                let irr_full = self.rt_irr_full.as_ref().expect("ensured above");
                let normal_half = self.rt_normal_half.as_ref().expect("ensured above");
                let normal_full = self.rt_normal_full.as_ref().expect("ensured above");
                let refl_half = self.rt_refl_half.as_ref().expect("ensured above");
                let refl_full = self.rt_refl_full.as_ref().expect("ensured above");
                let _refl_full_b = self.rt_refl_full_b.as_ref().expect("ensured above");

                // RT-A3a D16a fuse rule: split dispatch ONLY when mask trace
                // size differs from lighting (shadow native while lighting
                // stays half). Otherwise one fused dispatch at lighting's
                // resolution (shadow_spp=1 folded into the lighting params).
                if mask_sizes_differ {
                    tracer.dispatch_shadow_rays(
                        gpu.native_enc,
                        gpu.device,
                        accel,
                        emissive_stats,
                        &mask_params,
                        caster_ring,
                        gi_materials_buffer,
                        normal_sources_buffer,
                        objects,
                        alpha_textures,
                        depth_tex,
                        mask_half,
                        svt_half,
                        irr_half,
                        normal_half,
                        refl_half,
                        if envmap_wired.is_some() {
                            self.prefiltered_specular.as_ref().expect("ensured above")
                        } else {
                            self.dummy_texture.as_ref().expect("ensured at 3491")
                        },
                        // RS-C: dummy emissive buffers (mask dispatch has
                        // gi_spp=0, so the sampler block is skipped).
                        self.dummy_emissive_buffer.as_ref().expect("ensured above"),
                        self.dummy_emissive_buffer.as_ref().expect("ensured above"),
                        self.rt_has_translucency,
                        "node.render_scene RT-A3a mask dispatch (shadow visibility)",
                    );
                }
                gpu.checkpoint();

                // RT-A3a: lighting dispatch — AO + GI + reflection + normal.
                // D16a: when fused (mask_sizes_differ=false), shadow_spp=1
                // and out_sv is written here (one dispatch, monolithic perf).
                tracer.dispatch_shadow_rays(
                    gpu.native_enc,
                    gpu.device,
                    accel,
                    emissive_stats,
                    &lighting_params,
                    caster_ring,
                    gi_materials_buffer,
                    normal_sources_buffer,
                    objects,
                    alpha_textures,
                    depth_tex,
                    mask_half,
                    svt_half,
                    irr_half,
                    normal_half,
                    refl_half,
                    if envmap_wired.is_some() {
                        self.prefiltered_specular.as_ref().expect("ensured above")
                    } else {
                        self.dummy_texture.as_ref().expect("ensured at 3491")
                    },
                    // RS-C: pass the real emissive table buffers when
                    // available (the kernel guards on entry_count > 0);
                    // fall back to dummy when no emissive geometry exists.
                    accel.emissive_table.as_ref()
                        .map(|t| &t.triangles)
                        .unwrap_or_else(|| self.dummy_emissive_buffer.as_ref().expect("ensured above")),
                    accel.emissive_table.as_ref()
                        .map(|t| &t.aliases)
                        .unwrap_or_else(|| self.dummy_emissive_buffer.as_ref().expect("ensured above")),
                    self.rt_has_translucency,
                    "node.render_scene RT-A3a lighting dispatch (AO+GI+reflection+normal)",
                );
                gpu.checkpoint();
                tracer.upsample_shadow(
                    gpu.native_enc,
                    &lighting_params,
                    params_buffer,
                    depth_tex,
                    mask_half,
                    mask_full,
                    irr_half,
                    irr_full,
                    normal_half,
                    normal_full,
                    refl_half,
                    refl_full,
                    svt_half,
                    svt_full,
                    "node.render_scene RT-D3/RT-P2 upsample_shadow",
                );

                // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): extract
                // specular hit-distance from the upsampled reflection
                // texture's .a channel into the graph output. Runs before
                // atrous_pass (which ping-pongs refl_full and would
                // overwrite the upsampled hit-distance).
                // Gated on denoise_aux_ready (BUG-qtkq): on the live-flip
                // frame the pre-flip plan has no hit-dist target, so
                // denoise_aux_ready is false and this block idles one frame.
                if denoise_aux_ready {
                    let hit_dist_target = spec_hit_dist_out.expect("denoise_aux_ready implies Some");
                    // COMPILE_CONTRACT_DESIGN P2: hit_dist_extract pipeline is prewarmed
                    // at startup, but use lazy creation as fallback if prewarm hasn't run.
                    if self.hit_dist_extract_pipeline.is_none() {
                        self.hit_dist_extract_pipeline = Some(gpu.device.create_compute_pipeline(
                            include_str!("../shaders/hit_dist_extract.wgsl"),
                            "cs_main",
                            "node.render_scene hit_dist_extract",
                        ));
                    }
                    let hit_dist_pipeline = self.hit_dist_extract_pipeline
                        .as_ref()
                        .expect("just created or prewarmed");
                    gpu.native_enc.dispatch_compute(
                        hit_dist_pipeline,
                        &[
                            GpuBinding::Texture { binding: 0, texture: refl_full },
                            GpuBinding::Texture { binding: 1, texture: hit_dist_target },
                        ],
                        [width.div_ceil(16), height.div_ceil(16), 1],
                        "node.render_scene hit_dist extract",
                    );
                }

                // RT-T1-D (RAYTRACING_DESIGN.md section 8 Tier-1 item 3, BUG-312):
                // ATROUS_ITERATIONS total spatial-filter passes on the RT
                // lighting signal — `upsample_shadow` above is pass 1 (the
                // half->full resample, now also normal-weighted); the two
                // dilated `atrous_pass` calls below are passes 2-3, steps
                // 1 then 2 (brief's committed range: 2-3 total). An EVEN
                // count of dilated passes (2 here) lands the final result
                // back in `mask_full`/`irr_full`/`normal_full` — the
                // buffers `accumulate_irradiance` below already reads — via
                // the `_full_b` scratch set, so no downstream rebinding is
                // needed.
                const ATROUS_ITERATIONS: u32 = 3;
                let read_idx = self.rt_history_ping;
                let write_idx = 1 - read_idx;
                let moments_read = self.rt_moments_history[read_idx].as_ref().expect("ensured above");
                let atrous_params_buffer = self.rt_atrous_params_buffer.as_ref().expect("ensured above");
                let mask_full_b = self.rt_mask_full_b.as_ref().expect("ensured above");
                let irr_full_b = self.rt_irr_full_b.as_ref().expect("ensured above");
                let normal_full_b = self.rt_normal_full_b.as_ref().expect("ensured above");
                let refl_full_b = self.rt_refl_full_b.as_ref().expect("ensured above");
                let svt_full_b = self.rt_svt_full_b.as_ref().expect("ensured above");
                let history_valid = self.rt_moments_valid;
                for pass in 0..(ATROUS_ITERATIONS - 1) {
                    // T1-D: dilation starts at 2, not 1 — the AO/GI trace
                    // dispatch is HALF-res (D11), so every 2x2 block of
                    // full-res texels shares the identical raw noise
                    // sample; a step=1 tap frequently lands in the SAME
                    // block (an exact duplicate, not an independent noisy
                    // sample) and does nothing to reduce variance. step=2
                    // is the smallest offset guaranteed to cross into an
                    // adjacent (independently-sampled) half-res block.
                    let step = 2u32 << pass;
                    let (src_sv, src_irr, src_n, src_refl, src_svt, dst_sv, dst_irr, dst_n, dst_refl, dst_svt) = if pass % 2 == 0 {
                        (mask_full, irr_full, normal_full, refl_full, svt_full, mask_full_b, irr_full_b, normal_full_b, refl_full_b, svt_full_b)
                    } else {
                        (mask_full_b, irr_full_b, normal_full_b, refl_full_b, svt_full_b, mask_full, irr_full, normal_full, refl_full, svt_full)
                    };
                    let atrous_params = manifold_gpu::raytrace::AtrousParams::new(
                        [width, height], step, history_valid,
                        opaque_draws.len() as u32,
                    );
                    tracer.atrous_pass(
                        gpu.native_enc,
                        &atrous_params,
                        atrous_params_buffer,
                        gi_materials_buffer,
                        depth_tex,
                        moments_read,
                        src_sv,
                        dst_sv,
                        src_irr,
                        dst_irr,
                        src_n,
                        dst_n,
                        src_refl,
                        dst_refl,
                        src_svt,
                        dst_svt,
                        "node.render_scene RT-T1-D atrous_pass",
                    );
                }

                // RAYTRACING_DESIGN.md section 5.2 P2/D3, RT-D2: the ONE call site
                // deciding "discard temporal history this frame" for this
                // node's irradiance accumulator — ORs in a just-allocated
                // history texture (dimension change) rather than adding a
                // second reset path. `reset_decision` comes from the single
                // unconditional `detect_reset` call near the top of this fn;
                // `rt_just_resumed` covers an off→on accumulate resume.
                let allocation_reset = std::mem::take(&mut self.rt_irr_needs_reset);
                let reset = reset_decision
                    || rt_just_resumed
                    || allocation_reset
                    || toggle_flipped;
                // RT-T1-C: `prev_view_proj` is the SAME local captured
                // above (BUG-311) before `self.prev_view_proj` was
                // overwritten to this frame's `view_proj` — exactly what
                // MetalFX's own velocity pass reprojects with.
                // The accumulator should not have to INFER a lighting change
                // from pixels — this side knows. A per-texel gate only fires
                // when the changed term is a big enough share of its channel,
                // so a sun-intensity move (a small slice of a buffer dominated
                // by the ambient term) faded while an env move snapped. Peter
                // found exactly that split; the hashed keys below are how this
                // side says so. RAYTRACING_DESIGN.md section 10 addendum
                // (gesture rule): compute both the full lighting key (every
                // input that alters the traced textures) and the
                // geometry-only sub-key (caster position/direction/cone/kind
                // + svt slot). Gesture detection: two consecutive changes arm
                // a hold counter.
                let lighting_key =
                    compute_rt_lighting_key(rt_casters, &atmosphere.ambient_tint, envmap_content, pre.env_rotation, &pre.light_data);
                let (lighting_changed, lighting_gesture, _new_prev, new_gesture) =
                    gesture_detect(
                        self.rt_lighting_key,
                        lighting_key,
                        self.rt_lighting_prev_changed,
                        self.rt_lighting_gesture,
                    );
                let lighting_changed = lighting_changed || (envmap_wired.is_some() && envmap_content.is_none());
                self.rt_lighting_key = Some(lighting_key);
                self.rt_lighting_prev_changed = lighting_changed;
                self.rt_lighting_gesture = new_gesture;

                let geo_key = compute_rt_lighting_geo_key(rt_casters, svt_slot);
                let (geo_changed, geo_gesture, _geo_prev, new_geo_gesture) =
                    gesture_detect(
                        self.rt_lighting_geo_key,
                        geo_key,
                        self.rt_lighting_geo_prev_changed,
                        self.rt_lighting_geo_gesture,
                    );
                self.rt_lighting_geo_key = Some(geo_key);
                self.rt_lighting_geo_prev_changed = geo_changed;
                self.rt_lighting_geo_gesture = new_geo_gesture;

                // RAYTRACING_DESIGN.md section 17.5 DN-G (DN3): capture
                // lighting-changed and gesture signals for the denoiser's
                // reset path. `lighting_changed` OR `geo_changed` = a
                // CPU-vouched lighting change that should reset denoiser
                // history. `gesture_active` = either gesture hold counter
                // is non-zero.
                if rt_source_trace_enabled() {
                    log::info!(
                        "[RT-SOURCE] history scene={} allocation={} resumed={} toggle={} lighting={} lighting_gesture={} geometry_light={}",
                        reset_decision, allocation_reset, rt_just_resumed, toggle_flipped,
                        lighting_changed, lighting_gesture, geo_changed,
                    );
                }
                self.denoiser_lighting_changed = lighting_changed || geo_changed;
                self.denoiser_gesture_active = lighting_gesture || geo_gesture;

                let mut accumulate_params = manifold_gpu::raytrace::AccumulateParams::new(
                    [width, height],
                    IRRADIANCE_ACCUM_ALPHA,
                    reset,
                    opaque_draws.len() as u32,
                    cam.pos,
                    cam_motion,
                    inv_view_proj,
                    prev_view_proj,
                )
                .with_lighting_changed(lighting_changed)
                .with_gesture(lighting_gesture)
                .with_geo_changed(geo_changed)
                .with_geo_gesture(geo_gesture)
                // DN-L (section 17.7): when the denoiser consumes this
                // frame's beauty, our temporal history caps drop to
                // near-raw — the network's history replaces ours.
                .with_denoise_near_raw(denoise_active);
                // More casters grew the visibility arrays: only visibility
                // restarts; every other history carries on.
                if std::mem::take(&mut self.rt_sv_needs_reset) {
                    accumulate_params.reset |= manifold_gpu::raytrace::ACCUM_FLAG_SV_RESET;
                }
                let accumulate_params_buffer =
                    self.rt_accumulate_params_buffer.as_ref().expect("ensured above");
                // RT-T1-C: ping-pong — read last frame's write slot (same
                // `read_idx`/`write_idx` the à-trous pass above already
                // used for `moments_read`), write the OTHER (stale-from-
                // two-frames-ago, about to be fully overwritten) slot, then
                // flip so next frame reads what was just written.
                let irr_history_read = self.rt_irr_history[read_idx].as_ref().expect("ensured above");
                let irr_history_write = self.rt_irr_history[write_idx].as_ref().expect("ensured above");
                let depth_history_read = self.rt_depth_history[read_idx].as_ref().expect("ensured above");
                let depth_history_write = self.rt_depth_history[write_idx].as_ref().expect("ensured above");
                let normal_history_read = self.rt_normal_history[read_idx].as_ref().expect("ensured above");
                let normal_history_write = self.rt_normal_history[write_idx].as_ref().expect("ensured above");
                let moments_write = self.rt_moments_history[write_idx].as_ref().expect("ensured above");
                // RT-R2 (RD6): reflection history ping-pong — same read/write
                // indexing as the irradiance/depth/normal pairs above, sharing
                // the same `rt_history_ping` flip (I-R2: no second flip clock).
                let refl_history_read = self.rt_refl_history[read_idx].as_ref().expect("ensured above");
                let refl_history_write = self.rt_refl_history[write_idx].as_ref().expect("ensured above");
                // SV-ACCUM: shadow-visibility history — same read/write
                // indexing and the same flip clock as every other pair.
                // `mask_full` is the atrous-filtered current frame (the
                // even atrous pass count lands it there — see the
                // ATROUS_ITERATIONS comment above).
                let sv_history_read = self.rt_sv_history[read_idx].as_ref().expect("ensured above");
                let sv_history_write = self.rt_sv_history[write_idx].as_ref().expect("ensured above");
                let sv_m1_read = self.rt_sv_m1_history[read_idx].as_ref().expect("ensured above");
                let sv_m1_write = self.rt_sv_m1_history[write_idx].as_ref().expect("ensured above");
                let sv_m2_read = self.rt_sv_m2_history[read_idx].as_ref().expect("ensured above");
                let sv_m2_write = self.rt_sv_m2_history[write_idx].as_ref().expect("ensured above");
                let sv_hold_read = self.rt_sv_hold_history[read_idx].as_ref().expect("ensured above");
                let sv_hold_write = self.rt_sv_hold_history[write_idx].as_ref().expect("ensured above");
                // RT-TL-C (TL8): svt history gets the same read/write indices
                // as the irradiance channel (svt blends with irr alpha, not sv
                // sigma-gate).
                let svt_history_read = self.rt_svt_history[read_idx].as_ref().expect("ensured above");
                let svt_history_write = self.rt_svt_history[write_idx].as_ref().expect("ensured above");
                tracer.accumulate_irradiance(
                    gpu.native_enc,
                    &accumulate_params,
                    accumulate_params_buffer,
                    obj_motion_buffer,
                    irr_full,
                    depth_tex,
                    normal_full,
                    irr_history_read,
                    irr_history_write,
                    depth_history_read,
                    depth_history_write,
                    normal_history_read,
                    normal_history_write,
                    moments_read,
                    moments_write,
                    refl_full,
                    refl_history_read,
                    refl_history_write,
                    gi_materials_buffer,
                    mask_full,
                    sv_history_read,
                    sv_history_write,
                    sv_m1_read,
                    sv_m1_write,
                    sv_m2_read,
                    sv_m2_write,
                    sv_hold_read,
                    sv_hold_write,
                    svt_full,
                    svt_history_read,
                    svt_history_write,
                    "node.render_scene RT-P2/RT-T1-C/RT-T1-D/RT-R2 accumulate_irradiance",
                );
                self.rt_history_ping = write_idx;
                self.rt_moments_valid = true;
                // RT-Stage-3 P1 (BUG-mkgh): RT actually rendered this frame
                // (trace + accumulate dispatched) — arms the firefly clamp
                // gate at the tail.
                *rt_rendered_this_frame = true;
                gpu.checkpoint();

                // RT-Stage-3 P4 (BUG-eytk): post-accumulation à-trous
                // spatial filter on the just-accumulated irradiance.
                // Gate: iterations > 0 (Off tier) AND not denoise_active
                // (MetalFX owns the tail — D6). The filtered result is
                // consumed by the composite rebind below (irr_filtered_valid).
                //
                // I2 (write-set invariant): `atrous_post` writes ONLY
                // `rt_irr_filtered` / `rt_irr_filtered_b` — `rt_irr_history`
                // is never a write target. `rg` negative proof is lead-side;
                // this comment states the structural fact.
                //
                // I5 (denoise_active bypass): when `denoise_active` is true
                // the entire block is skipped — one boolean in the gate, no
                // separate test needed (the bypass is trivially correct by
                // code inspection).
                if denoise_iterations > 0 && !denoise_active {
                    let params_buffer = self.rt_atrous_post_params_buffer
                        .as_ref().expect("ensured above");
                    let filtered_a = self.rt_irr_filtered
                        .as_ref().expect("ensured above");
                    let filtered_b = self.rt_irr_filtered_b
                        .as_ref().expect("ensured above");
                    // Depth/normal guides: the RT block's opaque_depth_snapshot
                    // (the ALWAYS-ENSURED internal depth — NOT the later MSAA
                    // depth_tex at old line 6349), rt_normal_full (current
                    // frame), and moments_write (the just-written moments slot).
                    let depth_guide = self.opaque_depth_snapshot
                        .as_ref().expect("ensured above");
                    let normal_guide = self.rt_normal_full
                        .as_ref().expect("ensured above");
                    let moments_src = self.rt_moments_history[write_idx]
                        .as_ref().expect("ensured above");

                    // Ping-pong: source for pass 0 = the just-written
                    // history slot; later passes read from the previous
                    // pass's destination. Final destination must be
                    // `rt_irr_filtered` (the composite's single binding
                    // point). The write-set is ONLY rt_irr_filtered* —
                    // rt_irr_history is never a write target (I2).
                    let mut src = self.rt_irr_history[self.rt_history_ping]
                        .as_ref().expect("ensured above");
                    for pass in 0..denoise_iterations {
                        let step = 1u32 << pass;
                        let post_params = manifold_gpu::raytrace::AtrousPostParams::new(
                            [width, height],
                            step,
                            denoise_strength,
                        );
                        // Destination: for N total passes, pass i writes
                        // `_b` when (N-1-i) is odd, else `rt_irr_filtered`.
                        // This guarantees the FINAL pass always writes
                        // `rt_irr_filtered`.
                        let write_to_filtered = (denoise_iterations - 1 - pass).is_multiple_of(2);
                        let dst: &manifold_gpu::GpuTexture = if write_to_filtered {
                            filtered_a
                        } else {
                            filtered_b
                        };
                        tracer.atrous_post_pass(
                            gpu.native_enc,
                            &post_params,
                            params_buffer,
                            depth_guide,
                            normal_guide,
                            moments_src,
                            src,
                            dst,
                            "node.render_scene RT-Stage-3 P4 atrous_post",
                        );
                        // Next pass reads from this pass's destination.
                        src = dst;
                    }
                    *irr_filtered_valid = true;
                    gpu.checkpoint();
                }

                // RAYTRACING_DESIGN.md section 5.2 P3 (D5, "emissive-colored
                // volumetric glow"): every emissive object becomes an
                // extra Point-mode entry in the SAME march light table
                // every Sun/Point light already populates — a real,
                // physically-motivated light source in the existing
                // march, not a separate glow pass. Position = the
                // object's model-matrix translation (same "translation as
                // interior stand-in" convention this file's Blend-group
                // depth sort already uses for a per-object world position
                // with no full bounding-box tracked); slot -1 (unshadowed
                // glow — this pseudo-light has no shadow caster). Gated on
                // `rt_ready` (this `if` block) rather than `rt_enabled`
                // alone: an RT-enabled scene whose accel isn't ready yet
                // has no GI-gathered emissive term either, so gating the
                // glow the same way keeps both RT-P3 additions consistent.
                for d in opaque_draws {
                    let emission = d.uniforms.emission;
                    if emission[0] > 0.0 || emission[1] > 0.0 || emission[2] > 0.0 {
                        let m = d.uniforms.model;
                        shaft_light_data.push([m[3][0], m[3][1], m[3][2], 1.0]);
                        shaft_light_data.push([emission[0], emission[1], emission[2], -1.0]);
                        shaft_light_data.push([EMISSIVE_GLOW_RANGE_WORLD_UNITS, 1.0, 0.0, 0.0]);
                        shaft_light_data.push([0.0; 4]);
                        *shaft_light_count += 1;
                    }
                }
                // ── RT capture: channel snapshots when armed ──
                if RT_CAPTURE_ARM.replace(false) {
                    let refl_write = self.rt_history_ping;
                    let refl_read = 1 - refl_write;
                    let capture = |label: &str, t: &Option<manifold_gpu::GpuTexture>| {
                        if let Some(t) = t {
                            push_rt_capture(label, t);
                        }
                    };
                    capture("refl_raw", &self.rt_refl_full);
                    capture("refl_history_write", &self.rt_refl_history[refl_write]);
                    capture("refl_history_read", &self.rt_refl_history[refl_read]);
                    capture("irr_full", &self.rt_irr_full);
                    // RT-Stage-3 P4 (BUG-eytk): the post-filtered irradiance
                    // capture — taps `rt_irr_filtered` when the filter ran
                    // this frame, falls back to the raw history slot when
                    // not (Off tier / denoise_active). `irr_full` above is
                    // the PRE-accumulation signal — never moved by the
                    // filter; this slot is the POST-accumulation output.
                    if *irr_filtered_valid {
                        capture("irr_accum", &self.rt_irr_filtered);
                    } else {
                        capture("irr_accum", &self.rt_irr_history[refl_write]);
                    }
                    capture("moments", &self.rt_moments_history[refl_write]);
                    // SV-ACCUM: the `mask` channel dumps the ACCUMULATED
                    // visibility (the texture binding 41 actually feeds the
                    // fragment shader) — the gate must measure what the show
                    // consumes, not the pre-accumulation atrous output.
                    // `rt_mask_full` stays in the chain (atrous scratch) but
                    // is no longer the consumed mask.
                    capture("mask", &self.rt_sv_history[refl_write]);
                    // BUG-fh95: the RAW pre-upsample/pre-denoise trace output
                    // (out_sv at trace res, R=vis G=ao) — the texture the
                    // original open-plane 0/0 was read from; the full-res
                    // mask above can't see it (post-atrous).
                    capture("mask_half", &self.rt_mask_half);
                    // BUG-tr5o: the sv snap-HOLD counter — the direct
                    // observable of gate re-trips under camera motion
                    // (sustained >0 on penumbra = re-tripping; ~0 = healthy).
                    capture("sv_hold", &self.rt_sv_hold_history[refl_write]);
                }
            }

        true
    }
}
