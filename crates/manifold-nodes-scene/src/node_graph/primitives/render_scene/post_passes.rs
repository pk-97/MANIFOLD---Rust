//! Passes after the main draw: light shafts, the firefly clamp, and the MetalFX temporal upscale and ML denoise tails.

use super::*;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 9: the light-shaft march + depth-aware
    /// bilateral upsample + additive composite. Gates on wants_shafts_now
    /// internally — when off, none of these textures/pipelines exist and
    /// the method is one branch. Binds its own caster table and shadow
    /// maps (the ensure block guarantees them). Consumes the shaft-light
    /// table the RT block appended its emissive pseudo-lights to.
    #[allow(
        clippy::too_many_arguments,
        reason = "BUG-trh7 stage 2 pass method: args are the evaluate() locals the inline block used — destructuring at call sites is the approved shape"
    )]
    pub(super) fn shafts_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        resolve_target: &manifold_gpu::GpuTexture,
        depth_resolve_target: Option<&manifold_gpu::GpuTexture>,
        mut shaft_light_data: Vec<[f32; 4]>,
        shaft_light_count: u32,
        rt_ready: bool,
    ) {
        let FramePrelude {
            cam,
            atmosphere,
            aspect,
            rt_enabled,
            wants_shafts_now,
            ..
        } = *pre;
        let shadow_maps = self
            .shadow_maps
            .as_ref()
            .map(|maps| &maps.texture)
            .unwrap_or_else(|| self.dummy_depth_array.as_ref().expect("stubs ensured"));
        let light_buffer = &self.light_buffers[self.light_frame];
        let shadow_sampler = self.shadow_sampler.as_ref().expect("stubs ensured");
        let rt_mask_tex = self.rt_sv_history[self.rt_history_ping]
            .as_ref()
            .unwrap_or_else(|| self.dummy_mask_array.as_ref().expect("stubs ensured"));

        // depth-aware bilateral upsample + additive composite. Runs ONLY
        // when `wants_shafts_now` (the V1/D1 gate) — the default
        // `shaft_intensity == 0` never reaches this block, so the rest of
        // this function's behavior (and every byte it produces) is
        // untouched when shafts are off. ----
        if wants_shafts_now {
            let half_w = self.shaft_half_width;
            let half_h = self.shaft_half_height;
            let full_depth = depth_resolve_target.expect("ensured above: wants_shafts_now => Some");
            let half_depth = self.shaft_depth_half.as_ref().expect("ensured above");
            let inscatter = self.shaft_inscatter.as_ref().expect("ensured above");
            let downsample_pipeline = self
                .shaft_downsample_pipeline
                .as_ref()
                .expect("ensured above");
            let march_pipeline = self.shaft_march_pipeline.as_ref().expect("ensured above");
            let composite_pipeline = self
                .shaft_composite_pipeline
                .as_ref()
                .expect("ensured above");

            {
                let gpu = ctx.gpu_encoder();
                gpu.native_enc.dispatch_compute(
                    downsample_pipeline,
                    &[
                        GpuBinding::Texture {
                            binding: 0,
                            texture: full_depth,
                        },
                        GpuBinding::Texture {
                            binding: 1,
                            texture: half_depth,
                        },
                    ],
                    [half_w.div_ceil(16), half_h.div_ceil(16), 1],
                    "node.render_scene shaft downsample",
                );
            }

            let fov_y = match cam.mode {
                manifold_node_engine::scene::camera::CameraMode::Perspective { fov_y } => fov_y,
                manifold_node_engine::scene::camera::CameraMode::Orthographic { .. } => {
                    std::f32::consts::FRAC_PI_3
                }
            };
            let steps = shaft_step_count(atmosphere.shaft_quality) as f32;
            let march_uniforms = ShaftMarchUniforms {
                camera_pos: [cam.pos[0], cam.pos[1], cam.pos[2], cam.near],
                camera_right: [cam.right[0], cam.right[1], cam.right[2], cam.far],
                camera_up: [cam.up[0], cam.up[1], cam.up[2], fov_y],
                camera_fwd: [cam.fwd[0], cam.fwd[1], cam.fwd[2], aspect],
                fog_shaft: [
                    atmosphere.fog_density,
                    atmosphere.height_falloff,
                    atmosphere.shaft_anisotropy,
                    atmosphere.shaft_intensity,
                ],
                // RAYTRACING_DESIGN.md section 5.2 P3/D5: `rt_shadow_mask` (below)
                // is only meaningfully populated when RT is on AND its
                // accel structure is ready (same `rt_ready` gate the
                // surface pass uses) — a stale/never-written mask read with
                // the flag on would corrupt the Sun term, so the flag
                // mirrors that exact condition, not `rt_enabled` alone.
                misc: [
                    steps,
                    shaft_light_count as f32,
                    cam.lens.exposure_ev,
                    if rt_enabled && rt_ready { 1.0 } else { 0.0 },
                ],
            };
            // D4 always-bind discipline: the march's `shaft_lights` binding
            // must be a valid (non-zero-length) buffer even at 0 lights
            // (`shaft_light_count` gates the shader's loop, not the
            // binding's presence) — checked HERE, after every append
            // (real lights above, RT-P3's emissive pseudo-lights in the RT
            // block above) has already happened, so a stub only gets added
            // when the buffer is genuinely still empty. 4 vec4s = one
            // zeroed light-shaped stub.
            if shaft_light_data.is_empty() {
                shaft_light_data.extend([[0.0f32; 4]; LIGHT_VEC4_STRIDE]);
            }
            let shaft_light_bytes: &[u8] = bytemuck::cast_slice(&shaft_light_data);
            {
                let gpu = ctx.gpu_encoder();
                gpu.native_enc.dispatch_compute(
                    march_pipeline,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&march_uniforms),
                        },
                        GpuBinding::Texture {
                            binding: 1,
                            texture: half_depth,
                        },
                        GpuBinding::Bytes {
                            binding: 2,
                            data: shaft_light_bytes,
                        },
                        GpuBinding::Buffer {
                            binding: 3,
                            buffer: light_buffer,
                            offset: self.caster_offset,
                        },
                        GpuBinding::Texture {
                            binding: 4,
                            texture: shadow_maps,
                        },
                        GpuBinding::Sampler {
                            binding: 8,
                            sampler: shadow_sampler,
                        },
                        GpuBinding::Texture {
                            binding: 9,
                            texture: inscatter,
                        },
                        GpuBinding::Texture {
                            binding: 10,
                            texture: rt_mask_tex,
                        },
                    ],
                    [half_w.div_ceil(16), half_h.div_ceil(16), 1],
                    "node.render_scene shaft march",
                );
            }

            let composite_uniforms = ShaftCompositeUniforms {
                near_far: [cam.near, cam.far, 0.0, 0.0],
            };
            let gpu = ctx.gpu_encoder();
            gpu.native_enc.draw_instanced(
                composite_pipeline,
                resolve_target,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&composite_uniforms),
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: inscatter,
                    },
                    GpuBinding::Texture {
                        binding: 2,
                        texture: half_depth,
                    },
                    GpuBinding::Texture {
                        binding: 3,
                        texture: full_depth,
                    },
                ],
                manifold_gpu::DrawCount::Direct {
                    vertices: 3,
                    instances: 1,
                },
                manifold_gpu::GpuLoadAction::Load,
                "node.render_scene shaft composite",
            );
        }
    }

    /// BUG-trh7 stage 2, pass 10: the firefly clamp on the fully
    /// composited resolve_target, writing `target`, so the upscale/denoise
    /// tail reads the clamped color. Depth guide is the internal
    /// single-sample opaque_depth_snapshot. Gates on
    /// rt_rendered_this_frame && rt_firefly_clamp_enabled && !denoise_active
    /// — clamping when RT did not render is a forbidden move.
    pub(super) fn firefly_clamp_pass<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        resolve_target: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        rt_rendered_this_frame: bool,
        denoise_active: bool,
    ) {
        let FramePrelude {
            width,
            height,
            rt_firefly_clamp_enabled,
            ..
        } = *pre;
        // RT-Stage-3 P1 (BUG-mkgh): the firefly clamp runs on the fully
        // composited `resolve_target` (Pass A + transmissive Pass B + shafts)
        // and writes `target`, so the upscale/denoise tail below reads the
        // clamped color. Depth guide is the internal single-sample
        // `opaque_depth_snapshot` (the SAME depth `atrous_filter`/
        // `accumulate_irradiance` read) — NOT `self.depth_texture`, which is
        // the 4x-MSAA memoryless Pass-2 target and unreadable in a compute
        // pass, and NOT the lazy graph `depth` output, which is `None` when
        // unwired. `opaque_depth_snapshot` is ensured whenever `rt_enabled`,
        // so it is resident on every frame `rt_rendered_this_frame` can be
        // true.
        let firefly_clamp_active =
            rt_rendered_this_frame && rt_firefly_clamp_enabled && !denoise_active;
        if firefly_clamp_active {
            let tracer = self
                .rt_tracer
                .as_ref()
                .expect("rt_rendered_this_frame implies rt_tracer ensured");
            let firefly_params_buffer = self
                .rt_firefly_params_buffer
                .as_ref()
                .expect("ensured above");
            let firefly_depth = self
                .opaque_depth_snapshot
                .as_ref()
                .expect("ensured above: rt_enabled implies opaque depth snapshot");
            let firefly_params = manifold_gpu::raytrace::FireflyClampParams::new(
                [width, height],
                FIREFLY_MEDIAN_GAIN,
                // P4a: the FIXED minimum — the kernel applies
                // max(floor, stats->mean_power) itself.
                FIREFLY_ABS_FLOOR_MIN,
            );
            // P4a: the floor's mean-power rider rides the GPU stats buffer;
            // bind the table's stats (the kernel reads mean_power even with
            // entry_count 0 — zero stats reduce the floor to the minimum,
            // which is the no-emissive-scene intent).
            let emissive_stats = self
                .rt_accel
                .as_ref()
                .and_then(|a| a.emissive_table.as_ref())
                .map(|t| &t.stats)
                .unwrap_or_else(|| tracer.zero_emissive_stats());
            tracer.firefly_clamp(
                ctx.gpu_encoder().native_enc,
                emissive_stats,
                &firefly_params,
                firefly_params_buffer,
                firefly_depth,
                resolve_target,
                target,
                "node.render_scene RT firefly_clamp",
            );
        }
    }

    /// BUG-trh7 stage 2, pass 11: the MetalFX Temporal upscale tail —
    /// upscales the render-res scratch to native res with this frame's
    /// own jitter, then re-pairs the upscaled RGB with the scene's real
    /// alpha (BUG-om0v). Reset expression A stays verbatim
    /// (reset_decision || upscale_just_resumed). Returns false only for
    /// the BUG-317-class plan-missing abort (degrade loud, skip the frame
    /// exactly as the inline return).
    pub(super) fn temporal_upscale_tail<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        temporal_upscale_active: bool,
        target: &manifold_gpu::GpuTexture,
        depth_resolve_target: Option<&manifold_gpu::GpuTexture>,
        velocity_resolve_target: Option<&manifold_gpu::GpuTexture>,
    ) -> bool {
        let FramePrelude {
            reset_decision,
            upscale_just_resumed,
            temporal_upscale: _,
            jitter_px,
            native_width,
            native_height,
            ..
        } = *pre;
        // shafts) has now resolved into `target` — the render-res color
        // scratch when `temporal_upscale_active`. MetalFX Temporal upscales
        // it to native res using this SAME frame's jitter + the render-res
        // `depth`/`velocity` graph outputs (forced-consumed whenever
        // `temporal_upscale` is on — `force_consumed_outputs`; sized to
        // render res by `output_canvas_scale`'s D22 branch, so they already
        // match `target`'s dims with no extra scratch needed), then the
        // upscaled result becomes this node's real `color` output via a
        // same-format, same-size blit into `native_color` — the ONLY way
        // `native_color` is written this frame in upscaled mode (Pass 2
        // above wrote into the scratch, never into it directly).
        let native_color = ctx
            .outputs
            .texture_2d("color")
            .expect("pass2_draw already required the color output");
        if temporal_upscale_active {
            let upscaler = self
                .rt_temporal_upscaler
                .as_ref()
                .expect("ensured above: temporal_upscale_active implies Some");
            // BUG-317 belt-and-braces: `force_consumed_outputs` puts
            // `depth`/`velocity` in the plan's consumed set whenever
            // `temporal_upscale` is on, and PresetRuntime recompiles the
            // plan on a live toggle before the frame executes — so these
            // are Some on every reachable path. If a future param path
            // bypasses that seam, a live set must degrade (skip the
            // upscale, log loudly), never abort mid-show.
            let (Some(depth_src), Some(velocity_src)) =
                (depth_resolve_target, velocity_resolve_target)
            else {
                if !self.rt_temporal_unavailable_logged {
                    self.rt_temporal_unavailable_logged = true;
                    log::error!(
                        "node.render_scene: temporal_upscale is on but the execution plan has no depth/velocity target (stale consumed_outputs — BUG-317 class); skipping upscale this frame"
                    );
                }
                return false;
            };
            // D22/RT-D2: the SAME shared reset decision computed once near
            // the top of this fn (`reset_decision`), plus the explicit
            // off→on resume latch.
            let reset = reset_decision || upscale_just_resumed;
            let gpu = ctx.gpu_encoder();
            // MTL4 skip: on ring saturation `upscale` returns false and
            // leaves `output` holding last frame's upscale — the copy below
            // then presents that stale frame (one-frame freeze), the same
            // degradation the denoiser's MTL4 skip path documents.
            upscaler.upscale(
                gpu,
                target,
                depth_src,
                velocity_src,
                jitter_px.0,
                jitter_px.1,
                reset,
            );
            // BUG-om0v: MetalFX Temporal does not preserve the source's alpha
            // channel — it writes an opaque (or otherwise undefined) alpha into
            // `upscaler.output`, so blitting it straight into `native_color`
            // (as this tail used to) made the scene layer's background opaque
            // and blocked every layer beneath it. The non-upscale path carries
            // the fragment shader's own alpha (0 in the background) through the
            // MSAA resolve untouched. Fix at the root: re-pair the upscaled RGB
            // with the scene's real alpha, bilinearly upsampled from the
            // render-res scratch the upscaler read (`target`). `native_color`
            // is Rgba16Float storage-writeable, so this is one compute pass —
            // no new scratch and no second blit.
            // Upscale_alpha_combine pipeline is prewarmed
            // at startup, but use lazy creation as fallback if prewarm hasn't run.
            if self.upscale_alpha_combine_pipeline.is_none() {
                self.upscale_alpha_combine_pipeline = Some(gpu.device.create_compute_pipeline(
                    include_str!("../shaders/upscale_alpha_combine.wgsl"),
                    "cs_main",
                    "node.render_scene upscale_alpha_combine",
                ));
            }
            let combine_pipeline = self
                .upscale_alpha_combine_pipeline
                .as_ref()
                .expect("just created or prewarmed");
            let combine_sampler = gpu.device.linear_sampler();
            gpu.native_enc.dispatch_compute(
                combine_pipeline,
                &[
                    GpuBinding::Texture {
                        binding: 0,
                        texture: &upscaler.output.texture,
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: target,
                    },
                    GpuBinding::Sampler {
                        binding: 2,
                        sampler: combine_sampler,
                    },
                    GpuBinding::Texture {
                        binding: 3,
                        texture: native_color,
                    },
                ],
                [native_width.div_ceil(16), native_height.div_ceil(16), 1],
                "node.render_scene upscale alpha combine",
            );
        }
        true
    }

    /// BUG-trh7 stage 2, pass 12: the ML denoiser tail — fused
    /// denoise+upscale from render res to native (or 1:1), writing
    /// directly to native_color. Reset expression B stays verbatim
    /// (reset_decision || rt_just_resumed || denoiser_lighting_changed
    /// || denoiser_gesture_active — the three sources, strobes do NOT
    /// reset). The MTL4-saturated fallback copy is unchanged. Returns
    /// false only for the BUG-317-class plan-missing abort.
    #[allow(
        clippy::too_many_arguments,
        reason = "BUG-trh7 stage 2 pass method: args are the evaluate() locals the inline block used — destructuring at call sites is the approved shape"
    )]
    pub(super) fn denoise_tail<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        denoise_active: bool,
        rt_just_resumed: bool,
        target: &manifold_gpu::GpuTexture,
        depth_resolve_target: Option<&manifold_gpu::GpuTexture>,
        velocity_resolve_target: Option<&manifold_gpu::GpuTexture>,
    ) -> bool {
        let FramePrelude {
            reset_decision,
            temporal_upscale,
            jitter_px,
            native_width,
            native_height,
            spec_hit_dist_out,
            normals_resolve_target,
            roughness_resolve_target,
            diffuse_albedo_resolve_target,
            specular_albedo_resolve_target,
            reactive_resolve_target,
            ..
        } = *pre;
        let native_color = ctx
            .outputs
            .texture_2d("color")
            .expect("pass2_draw already required the color output");
        // RAYTRACING_DESIGN.md section 17.5 DN-G (DN2/DN3): ML denoiser
        // path — replaces the plain temporal scaler on RT scenes when
        // enabled (DN2). Fused denoise+upscale from render res to native
        // res (or 1:1 native denoise when `temporal_upscale` is off),
        // writing directly to `native_color`.
        if denoise_active {
            let denoiser = self
                .denoiser
                .as_ref()
                .expect("ensured above: denoise_active implies Some");
            // Belt-and-braces: `force_consumed_outputs` puts all eight
            // denoiser feeds in the plan's consumed set whenever
            // `denoise_feed` is on — same BUG-317 class guard as the
            // temporal path.
            let (
                Some(depth_src),
                Some(velocity_src),
                Some(normal_src),
                Some(roughness_src),
                Some(diffuse_src),
                Some(specular_src),
                Some(hit_dist_src),
                Some(reactive_src),
            ) = (
                depth_resolve_target,
                velocity_resolve_target,
                normals_resolve_target,
                roughness_resolve_target,
                diffuse_albedo_resolve_target,
                specular_albedo_resolve_target,
                spec_hit_dist_out,
                reactive_resolve_target,
            )
            else {
                if !self.denoiser_unavailable_logged {
                    self.denoiser_unavailable_logged = true;
                    log::error!(
                        "node.render_scene: denoise is on but the execution plan is missing denoiser feeds (stale consumed_outputs — BUG-317 class); skipping denoise this frame"
                    );
                }
                return false;
            };
            // RAYTRACING_DESIGN.md section 17.5 DN-G (DN3): ONE reset
            // path, extended not duplicated. Three signal sources drive
            // `reset` on the denoiser:
            //   1. TemporalResetDetector — cut/seek/rebuild → reset
            //      (`reset_decision` + the off→on `rt_just_resumed` latch)
            //   2. cpu_lighting_changed — any lighting key change → reset
            //   3. gesture flags — gesture hold active → reset
            // Strobes do NOT reset (D3). `denoiser_lighting_changed` and
            // `denoiser_gesture_active` were captured from the RT block's
            // key computation above (when RT is on). When RT is off,
            // `denoise_active` is false and this block is unreachable.
            let reset = reset_decision
                || rt_just_resumed
                || self.denoiser_lighting_changed
                || self.denoiser_gesture_active;
            // D-64 motion-vector honesty check: the denoiser expects
            // jitter-free motion vectors (its jitterOffset compensates the
            // current frame). Our WGSL velocity fragment already subtracts
            // the jitter delta via `velocity_jitter` — same subtraction the
            // T2-B plain temporal scaler path uses. No difference from the
            // T2-B path; both consume the same jitter-free velocity output.
            // Set the denoiser's own jitter offset to match this frame's
            // camera jitter (0,0 when `temporal_upscale` is off).
            denoiser.set_jitter(jitter_px.0, jitter_px.1);
            // Color source: `target` holds the forward-pass output at the
            // correct resolution (render res when temporal_upscale is on,
            // native res for 1:1 denoise). The denoiser writes its output
            // directly to `native_color` — fused denoise+upscale.
            let color_src: &manifold_gpu::GpuTexture = if temporal_upscale || !denoise_active {
                target
            } else {
                // 1:1 denoise: target is the native-res scratch (ensured
                // above). This codepath is only reached when
                // `denoise_active` and `!temporal_upscale`.
                target
            };
            let gpu = ctx.gpu_encoder();
            let denoised = denoiser.encode(
                gpu,
                color_src,
                depth_src,
                velocity_src,
                normal_src,
                roughness_src,
                diffuse_src,
                specular_src,
                hit_dist_src,
                native_color, // output directly to native res
                reset,
                // DN-L (section 17.7): reactive mask — emissive
                // surfaces and object-moved draws marked by the
                // forward pass, so the denoiser trusts the current
                // frame over its history there (emissive strobes
                // and movers no longer trail).
                Some(reactive_src),
            );

            if !denoised && !temporal_upscale {
                // MTL4 allocator ring saturated: the denoiser left
                // native_color untouched. For 1:1 denoise the forward
                // pass rendered into color_src (the native-res scratch),
                // so copy it to native_color as a safe un-denoised
                // fallback. With temporal_upscale on the denoiser
                // REPLACES the temporal scaler, so nothing else writes
                // native_color this frame — the skip leaves last frame's
                // output in place (one-frame freeze under extreme
                // backpressure, recovers when the ring drains).
                gpu.native_enc.copy_texture_to_texture(
                    color_src,
                    native_color,
                    native_width,
                    native_height,
                    1,
                );
            }
        }
        true
    }
}
