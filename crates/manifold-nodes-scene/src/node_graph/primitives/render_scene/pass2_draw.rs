//! Pass 2, the main draw: per-draw binding sets, the opaque MSAA batch with its aux attachments, and the glass Pass B over the opaque scene copy.

use super::water_shade::{LookBindings, RippleUniforms, ripple_uniforms};
use super::*;
use manifold_node_engine::scene::camera::CameraMode;

impl RenderScene {
    /// BUG-trh7 stage 2, pass 8: the draw — target/resolve-target selection,
    /// the 56-binding sets, opaque/blend split, aux attachments, Pass A,
    /// the depth resolve copy, and the E2a seam (snapshot blit + Pass B).
    /// Returns the targets the shafts/firefly/tails pass through
    /// (resolve_target is where everything above landed; target is the
    /// final color before the tails). None = the zero-vertex or
    /// missing-color abort (cleared exactly as the inline code did).
    #[allow(
        clippy::too_many_arguments,
        reason = "BUG-trh7 stage 2 pass method: args are the evaluate() locals the inline block used — destructuring at call sites is the approved shape"
    )]
    pub(super) fn pass2_draw<'ctx, 'gpu>(
        &mut self,
        ctx: &mut EffectNodeContext<'ctx, 'gpu>,
        pre: &FramePrelude<'ctx>,
        draws: &[ObjectDraw<'ctx>],
        fields: &[FieldObject<'ctx>],
        has_transmission: bool,
        denoise_active: bool,
        rt_rendered_this_frame: bool,
        irr_filtered_valid: bool,
    ) -> Option<Pass2Targets> {
        let FramePrelude {
            envmap_wired,
            wants_shafts_now,
            width,
            height,
            temporal_upscale,
            rt_firefly_clamp_enabled,
            denoise_aux_ready,
            normals_resolve_target,
            roughness_resolve_target,
            diffuse_albedo_resolve_target,
            specular_albedo_resolve_target,
            reactive_resolve_target,
            ..
        } = *pre;
        // A frame with no water holds no G-buffer.
        if fields.is_empty() {
            self.water_march.reset();
        }
        // ---- Pass 2 (immutable phase): draw. Every object composites into
        // ONE 4x-MSAA color+depth pass (cleared once); the shared depth
        // buffer resolves inter-object occlusion, and the pass resolves out
        // to the single-sample `target` at the end.
        let native_color = ctx.outputs.texture_2d("color")?;
        // RAYTRACING_DESIGN.md section 8.2 D22 (T2-B): when `temporal_upscale` is
        // on, Pass A/B/shaft-composite below resolve into the render-res
        // scratch instead of the graph's native-res `color` output —
        // `target` (unchanged variable name; every downstream use in Pass 2
        // is untouched) is that redirect point. Keyed on the raw param, NOT
        // `temporal_upscale_active`: `width`/`height` are already
        // render-res whenever the param is on regardless of hardware
        // availability (see the ensure-block comment above), so the MSAA
        // resolve MUST land in a render-res target either way — the
        // hardware-availability question only decides whether the tail
        // below can actually upscale that scratch into `native_color`. The
        // real `native_color` texture is only touched again at this fn's
        // very end.
        let target: &manifold_gpu::GpuTexture = if temporal_upscale {
            self.rt_temporal_color_scratch
                .as_ref()
                .expect("ensured above")
        } else if denoise_active {
            // 1:1 denoise: render into the native-res scratch so the
            // denoiser can read color from it and write to native_color
            // without aliasing (the scratch was ensured at native dims
            // in the ensure block above).
            self.rt_temporal_color_scratch
                .as_ref()
                .expect("ensured above")
        } else {
            native_color
        };
        // RT-Stage-3 P1 (BUG-mkgh): the firefly clamp runs on the node's
        // final resolved color, before the upscale/denoise tail. When it's
        // active the forward pass resolves into `rt_firefly_scratch`, whose
        // reflection-prefilter use has finished, and the clamp writes `target` —
        // never `rt_temporal_color_scratch`, because under temporal upscale
        // `target` IS that scratch and the clamp's source/destination would
        // alias. Gated on RT having actually rendered this frame AND the
        // param AND not `denoise_active` (the denoiser owns the tail then).
        let firefly_clamp_active =
            rt_rendered_this_frame && rt_firefly_clamp_enabled && !denoise_active;
        let resolve_target: &manifold_gpu::GpuTexture = if firefly_clamp_active {
            self.rt_firefly_scratch.as_ref().expect("ensured above")
        } else {
            target
        };
        // `opaque_scene_color` was
        // already (re)allocated above, sized to this exact `target`'s
        // format — see `opaque_scene_color_target_format`'s doc comment for
        // why that ensure call has to happen before this point.
        // `None` when unwired — the graph compiler
        // never assigns "depth" a step-output binding in that case (same
        // lazy mechanism as `render_mesh`'s G-buffer outputs), so this pass
        // costs zero new bytes unless a consumer actually wired it (I1).
        let graph_depth_resolve_target = ctx.outputs.texture_2d("depth");
        // When shafts are on and `depth`
        // is unwired, resolve into the internal target instead (ensured
        // above) so the march has a depth source regardless — "shafts-on
        // forces the internal Sample0 depth resolve even when `depth` is
        // unwired". When `depth` IS wired, the graph's own resolve target
        // is used for BOTH the graph consumer and the march (one resolve,
        // two readers).
        let depth_resolve_target =
            graph_depth_resolve_target.or(if wants_shafts_now || self.gizmo_depth_wanted {
                self.shaft_depth_internal.as_ref()
            } else {
                None
            });
        // Same lazy rule as `depth` —
        // `None` when unwired, costing zero new bytes (I1).
        let velocity_resolve_target = ctx.outputs.texture_2d("velocity");
        // RAYTRACING_DESIGN.md section 12 AM1: same lazy rule again.
        let ao_mask_resolve_target = ctx.outputs.texture_2d("ao_mask");
        // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): denoise G-buffer
        // resolve targets were read early (before the ensure block) so
        // denoise_aux_ready gates every downstream decision from one value.
        let depth_stencil = self.depth_stencil.as_ref().expect("just inserted");
        let depth_tex = self.depth_texture.as_ref().expect("just inserted");
        let msaa_color = self.msaa_color.as_ref().expect("just inserted");
        let sampler = self.sampler.as_ref().expect("just inserted");
        // Per-draw, per-map-family sampler
        // lookup — every descriptor was ensured into the cache above, so
        // this is a guaranteed hit, never a fallback/dummy.
        let material_samplers = &self.material_samplers;
        let sampler_for = |desc: MapSamplerDesc| -> &manifold_gpu::GpuSampler {
            material_samplers
                .get(&Self::sampler_cache_key(desc))
                .expect("ensured in the 'Ensure cached GPU resources' block above")
        };
        let dummy = self.dummy_texture.as_ref().expect("just inserted");
        let shadow_sampler = self.shadow_sampler.as_ref().expect("stubs ensured");
        let dummy_depth = self.dummy_depth.as_ref().expect("stubs ensured");
        let envmap_texture = envmap_wired.unwrap_or(dummy);
        // Every caster's shadow map is a layer of one array; the 1-layer
        // stand-in keeps the binding valid in a scene with no casters.
        let shadow_maps = self
            .shadow_maps
            .as_ref()
            .map(|maps| &maps.texture)
            .unwrap_or_else(|| self.dummy_depth_array.as_ref().expect("stubs ensured"));
        let dummy_mask_array = self.dummy_mask_array.as_ref().expect("stubs ensured");

        if !draws.iter().any(|d| d.vertex_count > 0) && fields.is_empty() {
            // Every object had zero drawable vertices — clear so stale
            // pool contents don't leak through (matches render_mesh's
            // vertex_count == 0 fallback).
            ctx.gpu_encoder()
                .native_enc
                .clear_texture(resolve_target, 0.0, 0.0, 0.0, 0.0);
            if firefly_clamp_active {
                // Zero-vertex + clamp: RT didn't render (no geometry), so
                // this branch is unreachable with the clamp armed — but
                // clear `target` too so a future gate change can't strand
                // a stale `target` behind an un-run clamp.
                ctx.gpu_encoder()
                    .native_enc
                    .clear_texture(target, 0.0, 0.0, 0.0, 0.0);
            }
            return None;
        }

        // Binding arrays must outlive the batch call, so build them all
        // first, then reference each from a `Draw`. The light buffer
        // (this frame's ring slot, which also holds the caster table) is
        // scene-wide — every object's set borrows the same one.
        let light_buffer = &self.light_buffers[self.light_frame];
        let prefiltered_specular = self.prefiltered_specular.as_ref().expect("ensured");
        let irradiance_map = self.irradiance_map.as_ref().expect("ensured");
        let brdf_lut = self.brdf_lut.as_ref().expect("ensured");
        // `None` whenever
        // `has_transmission` is false — nothing below reads it in that case
        // (a transmissive draw with no instances does not set
        // `has_transmission` and binds the dummy; it draws nothing), but the
        // option lets the same closure serve both cases.
        let opaque_scene_color_snapshot = self.opaque_scene_color.as_ref();
        // RAYTRACING_DESIGN.md RT-D3: the full-res RT shadow-visibility
        // mask, sampled by `shadow_factor` in place of the shadow-map
        // sampler when `scene_params.w > 0.5` — always bound (the ABI-
        // stub discipline every optional texture in this shader uses),
        // dummy when RT isn't active this frame.
        // SV-ACCUM: now reads the temporally-ACCUMULATED visibility
        // history — `rt_history_ping` indexes whichever ping-pong slot
        // `accumulate_irradiance` most recently WROTE this frame, exactly
        // the `rt_irr_tex` discipline below. Before SV-ACCUM this bound
        // the raw post-atrous `rt_mask_full` — the only RT channel with
        // no temporal amortization, and the penumbra boil Peter reported.
        let rt_mask_tex = self.rt_sv_history[self.rt_history_ping]
            .as_ref()
            .unwrap_or(dummy_mask_array);
        // RAYTRACING_DESIGN.md section 5.2 P2, extended RT-T1-C: the temporally-
        // accumulated demodulated-irradiance history — `rt_history_ping`
        // always indexes whichever ping-pong slot `accumulate_irradiance`
        // most recently WROTE this frame (dummy when RT isn't active this
        // frame — same ABI-stub discipline as `rt_mask_tex`).
        // RT-Stage-3 P4 (BUG-eytk): when the post-accumulation filter ran
        // this frame, the composite binds the filtered irradiance instead of
        // the raw history slot — the single rebind seam the filter hangs on.
        let rt_irr_tex = if irr_filtered_valid {
            self.rt_irr_filtered.as_ref().unwrap_or(dummy)
        } else {
            self.rt_irr_history[self.rt_history_ping]
                .as_ref()
                .unwrap_or(dummy)
        };
        // RAYTRACING_DESIGN.md section 9 RD1: the accumulated specular history
        // texture fs_pbr SUBSTITUTES for its prefiltered-env fetch when
        // `rt_flags.x > 0.5` — always bound (ABI-stub discipline), dummy
        // whenever the substitution is gated off (the textureLoad is
        // unreachable then). `rt_history_ping` indexes the most recent
        // write from `accumulate_irradiance` (RT-R2: swapped every frame
        // by the same ping-pong clock as `rt_irr_history`).
        let rt_refl_tex = self.rt_refl_history[self.rt_history_ping]
            .as_ref()
            .unwrap_or(dummy);
        // RT-TL-C (section 16 TL5): accumulated svt history — the just-written
        // slot after the ping flip, same ABI-stub discipline as rt_refl_tex.
        let rt_svt_tex = self.rt_svt_history[self.rt_history_ping]
            .as_ref()
            .unwrap_or(dummy);
        // D11: Pass 2 binds its own identity stub (each pass binds what it
        // needs since the stage-2 carve — the ensure block guarantees it).
        let identity_stub = self.identity_instance_stub.as_ref().expect("ensured");
        let prev_positions_dummy = self.prev_positions_dummy.as_ref().expect("ensured");
        // The scene-wide bindings every mesh draw and every water draw shares;
        // only the scene-colour snapshot at binding 27 differs per use.
        let caster_offset = self.caster_offset;
        let refraction_sampler = self.refraction_sampler.as_ref().expect("ensured");
        let prefiltered_sheen = self.prefiltered_sheen.as_ref().unwrap_or(dummy);
        // The sun's light through the water; a black texel, a cleared depth
        // and slot 0 when no caustics ran.
        let caustic_fit = self.caustics.fit().expect("caustics encode ran this frame");
        let (caustic_map, caustic_surface) = self.caustics.maps().unwrap_or((dummy, dummy_depth));
        let scene_bindings = |opaque_scene_color| {
            [
                GpuBinding::Texture {
                    binding: 2,
                    texture: envmap_texture,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 8,
                    buffer: light_buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 9,
                    buffer: light_buffer,
                    offset: caster_offset,
                },
                GpuBinding::Texture {
                    binding: 10,
                    texture: shadow_maps,
                },
                GpuBinding::Texture {
                    binding: 11,
                    texture: caustic_map,
                },
                GpuBinding::Texture {
                    binding: 12,
                    texture: caustic_surface,
                },
                GpuBinding::Buffer {
                    binding: 13,
                    buffer: caustic_fit,
                    offset: 0,
                },
                GpuBinding::Sampler {
                    binding: 14,
                    sampler: shadow_sampler,
                },
                GpuBinding::Texture {
                    binding: 16,
                    texture: prefiltered_specular,
                },
                GpuBinding::Texture {
                    binding: 17,
                    texture: irradiance_map,
                },
                GpuBinding::Texture {
                    binding: 18,
                    texture: brdf_lut,
                },
                GpuBinding::Texture {
                    binding: 27,
                    texture: opaque_scene_color,
                },
                GpuBinding::Sampler {
                    binding: 28,
                    sampler: refraction_sampler,
                },
                GpuBinding::Texture {
                    binding: 41,
                    texture: rt_mask_tex,
                },
                GpuBinding::Texture {
                    binding: 42,
                    texture: rt_irr_tex,
                },
                GpuBinding::Texture {
                    binding: 43,
                    texture: rt_refl_tex,
                },
                GpuBinding::Texture {
                    binding: 45,
                    texture: rt_svt_tex,
                },
                GpuBinding::Texture {
                    binding: 49,
                    texture: prefiltered_sheen,
                },
            ]
        };
        // Indexed by draw, so a draw wholly outside the camera's clip volume
        // keeps its place with no binding set built for it.
        let binding_sets: Vec<Option<[GpuBinding; 58]>> = draws
            .iter()
            .enumerate()
            .map(|(draw_index, draw)| {
                if !draw.in_view {
                    return None;
                }
                let [
                    s2,
                    s3,
                    s8,
                    s9,
                    s10,
                    s11,
                    s12,
                    s13,
                    s14,
                    s16,
                    s17,
                    s18,
                    s27,
                    s28,
                    s41,
                    s42,
                    s43,
                    s45,
                    s49,
                ] = scene_bindings(if draw.is_transmissive && has_transmission {
                    opaque_scene_color_snapshot
                        .expect("ensured above whenever has_transmission is true")
                } else {
                    dummy
                });
                Some([
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&draw.uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: draw.vertices,
                        offset: 0,
                    },
                    s2,
                    s3,
                    // normal_map_n (cotangent-frame tangent-space reconstruction).
                    GpuBinding::Texture {
                        binding: 4,
                        texture: draw.normal_map.unwrap_or(dummy),
                    },
                    // binding(5): roughness_map — permanently dead stub, see
                    // shader-side comment; always the dummy.
                    GpuBinding::Texture {
                        binding: 5,
                        texture: draw.legacy_roughness_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 6,
                        texture: draw.base_color_map.unwrap_or(dummy),
                    },
                    // binding(7): metallic_map — permanently dead stub, see
                    // shader-side comment; always the dummy.
                    GpuBinding::Texture {
                        binding: 7,
                        texture: draw.legacy_metallic_map.unwrap_or(dummy),
                    },
                    // Lights: ring-buffered storage buffer (was a 4KB-capped
                    // Bytes bind). Holds one zeroed entry when no light is
                    // wired; the shader gates reads on `light_count` (D4).
                    s8,
                    // The caster table, after the lights in the same ring
                    // slot — always bound, same D4 discipline.
                    s9,
                    s10,
                    s11,
                    s12,
                    s13,
                    s14,
                    // D11: per-object instances_n, or the identity stub when
                    // that object leaves it unwired.
                    GpuBinding::Buffer {
                        binding: 15,
                        buffer: draw.camera_instances().unwrap_or(identity_stub),
                        offset: 0,
                    },
                    // Split-sum IBL.
                    // Always bound (same always-bind ABI-stub discipline as
                    // bindings 4/5/7/10-14 above) — `ensure_ibl_resources`
                    // guarantees these three exist regardless of whether
                    // `envmap` is wired this frame; `fs_pbr` only actually
                    // samples them for PBR materials, which already require
                    // `envmap` wired (the `requires_envmap` gate above).
                    s16,
                    s17,
                    s18,
                    // The three NEW
                    // per-object maps (normal_map_n reuses binding(4) above).
                    // Same always-bind ABI-stub discipline.
                    GpuBinding::Texture {
                        binding: 19,
                        texture: draw.mr_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 20,
                        texture: draw.occlusion_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 21,
                        texture: draw.emissive_map.unwrap_or(dummy),
                    },
                    // Per-map-family
                    // samplers, one per texture family — no longer one
                    // scene-wide REPEAT sampler. Order matches
                    // `ObjectDraw::sampler_descs` / `Material`'s field order.
                    GpuBinding::Sampler {
                        binding: 22,
                        sampler: sampler_for(draw.sampler_descs[0]), // base_color
                    },
                    GpuBinding::Sampler {
                        binding: 23,
                        sampler: sampler_for(draw.sampler_descs[1]), // normal
                    },
                    GpuBinding::Sampler {
                        binding: 24,
                        sampler: sampler_for(draw.sampler_descs[2]), // mr
                    },
                    GpuBinding::Sampler {
                        binding: 25,
                        sampler: sampler_for(draw.sampler_descs[3]), // occlusion
                    },
                    GpuBinding::Sampler {
                        binding: 26,
                        sampler: sampler_for(draw.sampler_descs[4]), // emissive
                    },
                    // Opaque-scene-
                    // color snapshot — bound for real only on the object
                    // that will actually sample it (E2b), the 1×1 dummy for
                    // everything else. Declared in the WGSL (pipeline-layout
                    // correctness) but not yet read by `fs_pbr` this phase —
                    // no shading change.
                    s27,
                    s28,
                    // Sheen/iridescence/anisotropy extension
                    // textures. Same always-bind ABI-stub discipline as
                    // 19-21; sampled with the existing base_color_sampler
                    // (binding 22, sRGB colour maps) / mr_sampler (binding
                    // 24, linear data maps) rather than adding five more
                    // dedicated sampler bindings — a documented v1
                    // simplification (no per-extension-map sampler wrap/
                    // filter override; every extension texture gets that
                    // family's default repeat/linear behavior).
                    GpuBinding::Texture {
                        binding: 29,
                        texture: draw.sheen_color_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 30,
                        texture: draw.sheen_roughness_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 31,
                        texture: draw.iridescence_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 32,
                        texture: draw.iridescence_thickness_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 33,
                        texture: draw.anisotropy_map.unwrap_or(dummy),
                    },
                    // Texture completion: clearcoat/specular/
                    // transmission/volume-thickness extension textures,
                    // same always-bind ABI-stub discipline + shared-sampler
                    // simplification as 29-33 above.
                    GpuBinding::Texture {
                        binding: 34,
                        texture: draw.clearcoat_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 35,
                        texture: draw.clearcoat_roughness_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 36,
                        texture: draw.clearcoat_normal_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 37,
                        texture: draw.specular_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 38,
                        texture: draw.specular_color_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 39,
                        texture: draw.transmission_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 40,
                        texture: draw.volume_thickness_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 47,
                        texture: draw.diffuse_transmission_map.unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 48,
                        texture: draw.diffuse_transmission_color_map.unwrap_or(dummy),
                    },
                    GpuBinding::Bytes {
                        binding: 50,
                        data: bytemuck::cast_slice(&draw.map_uniforms),
                    },
                    s49,
                    s41,
                    s42,
                    s43,
                    s45,
                    // Optional per-vertex appearance weights. Unwired draws
                    // bind the existing vertex buffer as an unused ABI dummy;
                    // the shader reads it only when the uniform flag is set.
                    GpuBinding::Buffer {
                        binding: 46,
                        buffer: draw.weights.unwrap_or(draw.vertices),
                        offset: 0,
                    },
                    GpuBinding::Texture {
                        binding: 53,
                        texture: self.volume_optics.path(draw_index).unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 54,
                        texture: self.volume_optics.density().unwrap_or(dummy),
                    },
                    GpuBinding::Texture {
                        binding: 55,
                        texture: self
                            .volume_optics
                            .nearest(draw_index)
                            .unwrap_or(dummy_depth),
                    },
                    // Last frame's vertex positions for a deforming mesh, read
                    // by the velocity variant only when `render_mode.z` is set;
                    // every other draw binds the one-record stand-in.
                    GpuBinding::Buffer {
                        binding: 56,
                        buffer: draw
                            .prev_positions
                            .as_ref()
                            .and_then(prev_positions::PrevDraw::bound)
                            .unwrap_or(prev_positions_dummy),
                        offset: 0,
                    },
                    // Last frame's instance records for an instance array that
                    // moves on its own, read by the velocity variant only when
                    // `render_mode.w` is set; every other draw binds the
                    // identity stub.
                    GpuBinding::Buffer {
                        binding: 57,
                        buffer: draw.camera_prev_instances().unwrap_or(identity_stub),
                        offset: 0,
                    },
                    GpuBinding::Bytes {
                        binding: 51,
                        data: bytemuck::bytes_of(&draw.subsurface_binding),
                    },
                    GpuBinding::Texture {
                        binding: 52,
                        texture: self.subsurface_pass.output.as_ref().unwrap_or(dummy),
                    },
                    // The wet mask is about the opaque surfaces it was drawn
                    // over: glass in front of a wet floor stays dry.
                    GpuBinding::Texture {
                        binding: 68,
                        texture: self
                            .wet_pass
                            .mask()
                            .filter(|_| !draw.routes_to_transparent())
                            .unwrap_or(dummy),
                    },
                ])
            })
            .collect();
        // Split into the existing
        // opaque/mask group (order unchanged — a zero-Blend scene produces
        // byte-identical `draw_calls` to before this phase) and the sorted
        // transparent group, drawn as `second_pass` in the SAME encoder pass
        // right after (occlusion by opaque geometry falls out of the shared,
        // still-live MSAA depth buffer; no separate resolve/reclear).
        let mut draw_calls: Vec<manifold_gpu::Draw> = Vec::with_capacity(draws.len());
        let mut blend_entries: Vec<(f32, usize, manifold_gpu::Draw)> =
            Vec::with_capacity(draws.len());
        for (draw_index, (draw, bindings)) in draws.iter().zip(&binding_sets).enumerate() {
            let Some(bindings) = bindings else {
                continue;
            };
            // SCENE_RENDER_MODE_DESIGN.md D6/D8: the color pass carries each
            // draw's fill mode (Lines under wireframe) and topology (Point
            // under Points). The depth/shadow batches never see either —
            // they force Fill + Triangle in the encoder.
            let call = if draw.points {
                // D8: points draw EVERY vertex in the buffer (one dot per
                // vertex), not the triangle-multiple-of-3 count — the
                // trailing-vertex truncation must not drop dots.
                draw.camera_live(
                    manifold_gpu::Draw::new(
                        &draw.pipeline,
                        bindings,
                        draw.point_count,
                        draw.instance_count,
                    )
                    .primitive(manifold_gpu::GpuPrimitiveType::Point),
                )
            } else {
                draw.camera_live(
                    manifold_gpu::Draw::new(
                        &draw.pipeline,
                        bindings,
                        draw.vertex_count,
                        draw.instance_count,
                    )
                    .fill_mode(draw.fill_mode),
                )
            };
            if draw.routes_to_transparent() {
                blend_entries.push((draw.sort_depth, draw_index, call));
            } else {
                draw_calls.push(call);
            }
        }
        // Back-to-front: farthest object (largest view-space depth along
        // the camera's forward axis) drawn first, so nearer glass blends
        // correctly over farther glass.
        blend_entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let blend_draw_calls: Vec<manifold_gpu::Draw> =
            blend_entries.iter().map(|(_, _, call)| *call).collect();
        // When the scene has a
        // transmissive object, the Blend group is pulled OUT of Pass A
        // entirely and drawn as a separate Pass B below (after the opaque-
        // color snapshot blit) instead of as Pass A's in-pass `second_pass`
        // group — memoryless `msaa_color`/`msaa_depth` cannot survive past
        // Pass A's end, so a transmissive fragment can only sample "the
        // scene behind it" from a real resolved snapshot taken between the
        // two passes. A zero-transmission scene (`has_transmission ==
        // false`, the overwhelming common case) takes the exact `second_pass
        // = Some(...)` path unchanged from before this phase — byte-
        // identical single pass, same as today.
        let second_pass = if has_transmission || blend_draw_calls.is_empty() {
            None
        } else {
            Some((
                self.blend_depth_stencil.as_ref().expect("ensured"),
                blend_draw_calls.as_slice(),
            ))
        };

        // The aux-MRT slot P1 built but never
        // exercised. `velocity_pair` is `Some` only when BOTH this node's
        // own memoryless velocity_msaa scratch AND the graph-allocated
        // single-sample resolve target exist — i.e. exactly when
        // `velocity_wired` gated `ensure_velocity_msaa_target` above.
        // `aux_color_storage`/`aux_color` follow the same "declare outside,
        // conditionally initialize, borrow" shape needed to hand the pass a
        // `&[(&GpuTexture, &GpuTexture)]` from either branch without an
        // allocation.
        // SCENE_RENDER_MODE_DESIGN.md D8 (P3): under Points the pass has NO
        // aux attachments, even when velocity/ao_mask/denoise are wired — the
        // aux targets are memoryless (their own tiling budget) and the
        // points pipeline emits no aux outputs anyway, so declaring them
        // would both reimpose the memoryless point budget and mismatch the
        // single-attachment points pipeline. The aux resolve targets keep
        // their clear values for the frame (Points is a shading audit mode,
        // not a temporal-upscale input — the documented v1 scope).
        let points_active = draws.iter().any(|d| d.points);
        let velocity_pair = match (self.velocity_msaa.as_ref(), velocity_resolve_target) {
            (Some(msaa), Some(resolve)) if !points_active => Some((msaa, resolve)),
            _ => None,
        };
        // RAYTRACING_DESIGN.md section 12 AM1: same both-or-nothing pairing
        // for ao_mask. Clear colors differ per attachment: velocity 0 (no
        // motion), ao_mask 1 (background keeps full screen-space AO). The
        // aux order here (velocity, then ao_mask) MUST match the FsOut
        // @location order in `aux_variant`'s specializations.
        let ao_mask_pair = match (self.ao_mask_msaa.as_ref(), ao_mask_resolve_target) {
            (Some(msaa), Some(resolve)) if !points_active => Some((msaa, resolve)),
            _ => None,
        };
        let velocity_att = velocity_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let ao_mask_att = ao_mask_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [1.0; 4],
        });
        // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): denoise G-buffer
        // attachments — all-four-or-none, gated by denoise_aux_ready
        // (BUG-qtkq), mirroring the velocity/ao_mask pair pattern above.
        // On the live-flip frame denoise_aux_ready is false so none fire;
        // the MSAA self-fields were ensured under raw denoise_feed one
        // frame early (harmless, preserves ensure-before-use).
        let normals_pair = match (self.denoise_normals_msaa.as_ref(), normals_resolve_target) {
            (Some(msaa), Some(resolve)) if denoise_aux_ready && !points_active => {
                Some((msaa, resolve))
            }
            _ => None,
        };
        let roughness_pair = match (
            self.denoise_roughness_msaa.as_ref(),
            roughness_resolve_target,
        ) {
            (Some(msaa), Some(resolve)) if denoise_aux_ready && !points_active => {
                Some((msaa, resolve))
            }
            _ => None,
        };
        let diffuse_albedo_pair = match (
            self.denoise_diffuse_albedo_msaa.as_ref(),
            diffuse_albedo_resolve_target,
        ) {
            (Some(msaa), Some(resolve)) if denoise_aux_ready && !points_active => {
                Some((msaa, resolve))
            }
            _ => None,
        };
        let specular_albedo_pair = match (
            self.denoise_specular_albedo_msaa.as_ref(),
            specular_albedo_resolve_target,
        ) {
            (Some(msaa), Some(resolve)) if denoise_aux_ready && !points_active => {
                Some((msaa, resolve))
            }
            _ => None,
        };
        // DN-L (section 17.7): reactive mask — same all-or-none gate as the
        // other four feeds.
        let reactive_pair = match (self.denoise_reactive_msaa.as_ref(), reactive_resolve_target) {
            (Some(msaa), Some(resolve)) if denoise_aux_ready && !points_active => {
                Some((msaa, resolve))
            }
            _ => None,
        };
        let n_att = normals_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let r_att = roughness_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let da_att = diffuse_albedo_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let sa_att = specular_albedo_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let rm_att = reactive_pair.map(|(msaa, resolve)| manifold_gpu::AuxColorAttachment {
            msaa,
            resolve,
            clear: [0.0; 4],
        });
        let aux_storage_seven: [manifold_gpu::AuxColorAttachment; 7];
        let aux_storage_six: [manifold_gpu::AuxColorAttachment; 6];
        let aux_storage_five: [manifold_gpu::AuxColorAttachment; 5];
        let aux_storage_two: [manifold_gpu::AuxColorAttachment; 2];
        let aux_storage_one: [manifold_gpu::AuxColorAttachment; 1];
        // DN-L: the reactive attachment (`rm`) joins the denoise four's
        // all-or-none group — every arm that carries n/r/da/sa carries rm.
        let aux_color: &[manifold_gpu::AuxColorAttachment] = match (
            velocity_att,
            ao_mask_att,
            n_att,
            r_att,
            da_att,
            sa_att,
            rm_att,
        ) {
            (Some(v), Some(a), Some(n), Some(r), Some(da), Some(sa), Some(rm)) => {
                aux_storage_seven = [v, a, n, r, da, sa, rm];
                &aux_storage_seven
            }
            (Some(v), None, Some(n), Some(r), Some(da), Some(sa), Some(rm)) => {
                aux_storage_six = [v, n, r, da, sa, rm];
                &aux_storage_six
            }
            (None, Some(a), Some(n), Some(r), Some(da), Some(sa), Some(rm)) => {
                aux_storage_six = [a, n, r, da, sa, rm];
                &aux_storage_six
            }
            (Some(v), Some(a), None, None, None, None, None) => {
                aux_storage_two = [v, a];
                &aux_storage_two
            }
            (Some(v), None, None, None, None, None, None) => {
                aux_storage_one = [v];
                &aux_storage_one
            }
            (None, Some(a), None, None, None, None, None) => {
                aux_storage_one = [a];
                &aux_storage_one
            }
            (None, None, Some(n), Some(r), Some(da), Some(sa), Some(rm)) => {
                aux_storage_five = [n, r, da, sa, rm];
                &aux_storage_five
            }
            _ => &[],
        };

        let depth_store = match depth_resolve_target {
            Some(_) => manifold_gpu::DepthStore::Resolve(
                self.depth_resolve_scratch
                    .as_ref()
                    .expect("depth consumer ensured native resolve"),
            ),
            None => manifold_gpu::DepthStore::Discard,
        };
        let pass = manifold_gpu::DrawPass {
            aux_color,
            second_group: second_pass,
            ..manifold_gpu::DrawPass::msaa(
                msaa_color,
                resolve_target,
                manifold_gpu::GpuLoadAction::Clear,
            )
            .depth(
                depth_tex,
                depth_stencil,
                manifold_gpu::GpuLoadAction::Clear,
                depth_store,
            )
        };
        ctx.gpu_encoder()
            .native_enc
            .draw(&pass, &draw_calls, "node.render_scene");
        // Pass A's velocity read last frame's vertex positions and instance
        // records; refresh them for the next frame.
        self.encode_prev_position_copies(ctx, draws);

        if let Some(output) = depth_resolve_target {
            ctx.gpu_encoder().native_enc.copy_depth_to_float(
                self.depth_resolve_scratch
                    .as_ref()
                    .expect("native resolve ensured"),
                output,
            );
        }
        // The editor's gizmo lines read the internal copy; a wired `depth`
        // output is copied there so both readers see the same frame.
        if self.gizmo_depth_wanted
            && let (Some(output), Some(internal)) =
                (depth_resolve_target, self.shaft_depth_internal.as_ref())
        {
            if graph_depth_resolve_target.is_some() {
                ctx.gpu_encoder().native_enc.copy_texture_to_texture(
                    output,
                    internal,
                    internal.width,
                    internal.height,
                    1,
                );
            }
            self.gizmo_depth_ready = true;
        }

        // ---- The snapshot + Pass B
        // seam. Pass A above just resolved the fully-shaded opaque scene
        // into `target`; blit it into `opaque_scene_color` (the texture an
        // E2b transmissive shader will sample), then draw the Blend group
        // as its own single-sample pass, loading (not clearing) both
        // `target`'s color and `opaque_depth_snapshot`'s depth so it
        // composites onto Pass A's result and depth-tests against it. No
        // MSAA on this pass: memoryless MSAA color can't be re-loaded across
        // a pass boundary, and a non-memoryless MSAA target would cost real
        // VRAM gated only on this — see the confessed-shortcuts field in
        // this phase's landing report for why single-sample was chosen).
        // Entirely skipped when `has_transmission` is false — zero bytes,
        // zero passes, zero behavior change for every scene without glass. ----
        if has_transmission {
            let opaque_scene_color = self.opaque_scene_color.as_ref().expect("ensured above");
            let opaque_depth_snapshot = self.opaque_depth_snapshot.as_ref().expect("ensured above");
            let transmissive_depth_scratch = self
                .transmissive_depth_scratch
                .as_ref()
                .expect("transmission depth scratch ensured above");
            let opaque_depth_stencil = self.depth_stencil.as_ref().expect("ensured");
            let transmission_depth_stencil = self
                .transmission_depth_stencil
                .as_ref()
                .expect("transmission depth state ensured above");
            let shadow_pipeline = self.shadow_pipeline.as_ref().expect("ensured").clone();
            let solid_depth_pipeline = self.solid_depth_pipeline.as_ref().expect("ensured").clone();
            if !fields.is_empty() {
                let view = FieldView {
                    view_proj: pre.view_proj,
                    prev_view_proj: pre.prev_view_proj,
                    jitter_ndc: [pre.jitter_ndc.0, pre.jitter_ndc.1],
                    prev_jitter_ndc: [pre.prev_jitter_ndc.0, pre.prev_jitter_ndc.1],
                    frame_seconds: ctx.time.delta.0 as f32,
                    width,
                    height,
                };
                // Water is drawn where the march found it nearest: the march
                // writes the depth and velocity outputs there, after Pass A's
                // depth copy above.
                let mut failure: Option<(u32, &'static str)> = None;
                // The first field with mist draws it; the buffer sums them all.
                let mut mist_field: Option<&FieldObject> = None;
                let seconds = ctx.time.seconds.0 as f32;
                {
                    let gpu = ctx.gpu_encoder();
                    #[cfg(feature = "fluid-perf-proofs")]
                    gpu.native_enc
                        .profile_next_encoder("node.render_scene water march");
                    // The water meshes' nearest depth over the opaque scene:
                    // where each ray meets the water, so the march only
                    // refines the hit.
                    let meshed = fields.iter().any(|field| field.mesh.is_some());
                    if meshed {
                        gpu.native_enc.copy_texture_to_texture(
                            opaque_depth_snapshot,
                            transmissive_depth_scratch,
                            width,
                            height,
                            1,
                        );
                    }
                    for field in fields {
                        let Some(mesh) = &field.mesh else {
                            continue;
                        };
                        // Gain 1, no weights, not a cutout: the solid depth
                        // pipeline discards nothing.
                        let uniforms = ShadowUniforms {
                            light_view_proj: pre.view_proj,
                            model: field.model,
                            appearance: [1.0, 0.0, 0.0, 0.0],
                            alpha: [0.0, 0.0, 1.0, 0.0],
                            uv_m: [0.0; 4],
                            uv_t: [0.0; 4],
                            instance_inner: field.uniforms.instance_inner,
                        };
                        let bindings = [
                            GpuBinding::Bytes {
                                binding: 0,
                                data: bytemuck::bytes_of(&uniforms),
                            },
                            GpuBinding::Buffer {
                                binding: 1,
                                buffer: mesh.vertices,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 2,
                                buffer: identity_stub,
                                offset: 0,
                            },
                            GpuBinding::Buffer {
                                binding: 3,
                                buffer: mesh.vertices,
                                offset: 0,
                            },
                            GpuBinding::Texture {
                                binding: 4,
                                texture: dummy,
                            },
                            GpuBinding::Sampler {
                                binding: 5,
                                sampler: self.ripple_detail.sampler(),
                            },
                        ];
                        let draw = manifold_gpu::Draw::new(
                            &solid_depth_pipeline,
                            &bindings,
                            mesh.vertex_count,
                            1,
                        );
                        let draw = match &mesh.live_args {
                            Some((args, offset)) => draw.indirect(args, *offset),
                            None => draw,
                        };
                        gpu.native_enc.draw(
                            &manifold_gpu::DrawPass::depth_only(
                                transmissive_depth_scratch,
                                opaque_depth_stencil,
                                manifold_gpu::GpuLoadAction::Load,
                            ),
                            std::slice::from_ref(&draw),
                            "node.render_scene water first hit",
                        );
                    }
                    self.water_march
                        .begin(gpu.device, gpu.native_enc, width, height);
                    for field in fields {
                        if let Err(error) = self.water_march.encode(
                            gpu.device,
                            gpu.native_enc,
                            &view,
                            &field_draw(field),
                            opaque_depth_snapshot,
                            field.mesh.is_some().then_some(transmissive_depth_scratch),
                            depth_resolve_target,
                            velocity_resolve_target,
                        ) {
                            failure = Some((field.slot, error));
                            break;
                        }
                    }
                    // The mist integrates up to the nearest surface, so it runs
                    // once every hit is final.
                    if failure.is_none() {
                        for field in fields {
                            match self.water_march.encode_mist(
                                gpu.device,
                                gpu.native_enc,
                                &view,
                                &field_draw(field),
                                opaque_depth_snapshot,
                            ) {
                                Ok(true) => mist_field = mist_field.or(Some(field)),
                                Ok(false) => {}
                                Err(error) => {
                                    failure = Some((field.slot, error));
                                    break;
                                }
                            }
                        }
                    }
                    // The water refracts the opaque scene, so snapshot it as
                    // a transmissive layer does.
                    if failure.is_none() {
                        gpu.native_enc.copy_texture_to_texture(
                            resolve_target,
                            opaque_scene_color,
                            width,
                            height,
                            1,
                        );
                        if opaque_scene_color.mip_level_count() > 1 {
                            gpu.native_enc.generate_mipmaps(opaque_scene_color);
                        }
                    }
                }
                if failure.is_none() {
                    let hits = self.water_march.hits().expect("begin ran this frame");
                    let gpu = ctx.gpu_encoder();
                    // The size of a pixel on the water: metres per pixel per
                    // metre of distance (perspective), or flat (orthographic).
                    let footprint = match pre.cam.mode {
                        CameraMode::Perspective { fov_y } => {
                            [2.0 * (fov_y * 0.5).tan() / height as f32, 0.0]
                        }
                        CameraMode::Orthographic { half_height } => {
                            [0.0, 2.0 * half_height / height as f32]
                        }
                    };
                    // The foam over the hits, before any water shades.
                    if let Err(error) = self.foam_pass.encode(
                        gpu.device,
                        gpu.native_enc,
                        fields,
                        &foam_pass::FoamView {
                            view_proj: pre.view_proj,
                            width,
                            height,
                            hits,
                            footprint,
                        },
                    ) {
                        failure = fields.first().map(|field| (field.slot, error));
                    }
                    // The ripple uniforms of the mist field, for the mist draw's
                    // unread look bindings.
                    let mut mist_ripple: Option<RippleUniforms> = None;
                    for field in fields {
                        let shade = match shade_uniforms(pre.view_proj, width, height, field.slot) {
                            Ok(shade) => shade,
                            Err(error) => {
                                failure = Some((field.slot, error));
                                break;
                            }
                        };
                        let tile = self.ripple_detail.tile(field.slot);
                        let ripple_data = match ripple_uniforms(
                            field,
                            tile.map(|tile| tile.tile_metres),
                            field.foam.is_some() && self.foam_pass.mask().is_some(),
                            seconds,
                            footprint,
                        ) {
                            Ok(uniforms) => uniforms,
                            Err(error) => {
                                failure = Some((field.slot, error));
                                break;
                            }
                        };
                        if mist_field.is_some_and(|mist| mist.slot == field.slot) {
                            mist_ripple = Some(ripple_data);
                        }
                        // A field without a tile reads no ripples, so the stand-ins are never sampled.
                        let look = LookBindings {
                            uniforms: &ripple_data,
                            slopes: match tile {
                                Some(tile) => &tile.texture,
                                None => self.ripple_detail.dummy_slopes(),
                            },
                            sampler: self.ripple_detail.sampler(),
                            velocity: match field.velocity {
                                Some(velocity) => velocity,
                                None => self.ripple_detail.dummy_velocity(),
                            },
                            foam: match self.foam_pass.mask() {
                                Some(mask) => mask,
                                None => self.ripple_detail.dummy_slopes(),
                            },
                            tiles: field.tiles,
                        };
                        #[cfg(feature = "fluid-perf-proofs")]
                        gpu.native_enc
                            .profile_next_encoder("node.render_scene water shade");
                        self.water_shade.encode(
                            gpu.device,
                            gpu.native_enc,
                            resolve_target,
                            opaque_depth_snapshot,
                            opaque_depth_stencil,
                            &scene_bindings(opaque_scene_color),
                            bytemuck::bytes_of(&field.uniforms),
                            hits,
                            &shade,
                            &look,
                        );
                    }
                    if failure.is_none()
                        && let (Some(field), Some(ripple_data), Some(mist)) =
                            (mist_field, mist_ripple.as_ref(), self.water_march.mist())
                    {
                        match shade_uniforms(pre.view_proj, width, height, field.slot) {
                            Ok(shade) => {
                                // The mist shader reads none of the look inputs,
                                // so the stand-ins fill them.
                                let look = LookBindings {
                                    uniforms: ripple_data,
                                    slopes: self.ripple_detail.dummy_slopes(),
                                    sampler: self.ripple_detail.sampler(),
                                    velocity: self.ripple_detail.dummy_velocity(),
                                    foam: self.ripple_detail.dummy_slopes(),
                                    tiles: field.tiles,
                                };
                                #[cfg(feature = "fluid-perf-proofs")]
                                gpu.native_enc
                                    .profile_next_encoder("node.render_scene water mist");
                                self.water_shade.encode_mist(
                                    gpu.device,
                                    gpu.native_enc,
                                    resolve_target,
                                    opaque_depth_snapshot,
                                    &scene_bindings(opaque_scene_color),
                                    bytemuck::bytes_of(&field.uniforms),
                                    mist,
                                    &shade,
                                    &look,
                                );
                            }
                            Err(error) => failure = Some((field.slot, error)),
                        }
                    }
                }
                if let Some((slot, error)) = failure {
                    Self::fail_frame(ctx, Some(slot), error);
                    return None;
                }
            }
            // Capture the already-composited layers before each farther-to-nearer
            // draw. Every glass layer then transmits the layers behind it, instead
            // of all panes replacing one another with the original opaque image.
            // A run of plain Blend layers needs no snapshot or depth seed and
            // shares one state, so it draws as one batch; each transmissive
            // layer keeps its own pass.
            let mut layer = 0;
            while layer < blend_entries.len() {
                let draw_index = blend_entries[layer].1;
                let draw = &draws[draw_index];
                let batch_end = if draw.is_transmissive {
                    layer + 1
                } else {
                    layer
                        + blend_entries[layer..]
                            .iter()
                            .take_while(|(_, index, _)| !draws[*index].is_transmissive)
                            .count()
                };
                let nearest_surface = draw.is_transmissive
                    && !draw.points
                    && draw.fill_mode == manifold_gpu::GpuTriangleFillMode::Fill;
                #[cfg(feature = "fluid-perf-proofs")]
                if nearest_surface {
                    water_perf::record(draw_index, draw);
                }
                let gpu = ctx.gpu_encoder();
                // The layer prepass starts from opaque depth, so a solid mesh
                // cannot reveal a triangle behind an opaque surface and depth
                // testing within the mesh selects its nearest surface.
                if nearest_surface {
                    let shadow_uniforms = ShadowUniforms::for_draw(pre.view_proj, draw);
                    let shadow_bindings = [
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&shadow_uniforms),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: draw.vertices,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 2,
                            buffer: draw.camera_instances().unwrap_or(identity_stub),
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 3,
                            buffer: draw.weights.unwrap_or(draw.vertices),
                            offset: 0,
                        },
                        GpuBinding::Texture {
                            binding: 4,
                            texture: draw.base_color_map.unwrap_or(dummy),
                        },
                        GpuBinding::Sampler {
                            binding: 5,
                            sampler: &self.material_samplers
                                [&Self::sampler_cache_key(draw.sampler_descs[0])],
                        },
                    ];
                    let depth_draw = draw.camera_live(manifold_gpu::Draw::new(
                        if shadow_uniforms.discard_free() {
                            &solid_depth_pipeline
                        } else {
                            &shadow_pipeline
                        },
                        &shadow_bindings,
                        draw.vertex_count,
                        draw.instance_count,
                    ));

                    #[cfg(feature = "fluid-perf-proofs")]
                    gpu.native_enc
                        .profile_next_encoder("node.render_scene water opaque-depth seed");
                    gpu.native_enc.copy_texture_to_texture(
                        opaque_depth_snapshot,
                        transmissive_depth_scratch,
                        width,
                        height,
                        1,
                    );
                    gpu.native_enc.draw(
                        &manifold_gpu::DrawPass::depth_only(
                            transmissive_depth_scratch,
                            opaque_depth_stencil,
                            manifold_gpu::GpuLoadAction::Load,
                        ),
                        std::slice::from_ref(&depth_draw),
                        "node.render_scene transmissive depth prepass",
                    );
                }
                // Only a transmissive shader samples the snapshot
                // (`transmission_factor > 0` gates every read); a plain
                // Blend layer composites straight onto `resolve_target`.
                if needs_layer_snapshot(draw.is_transmissive) {
                    #[cfg(feature = "fluid-perf-proofs")]
                    gpu.native_enc
                        .profile_next_encoder("node.render_scene water scene-color snapshot");
                    gpu.native_enc.copy_texture_to_texture(
                        resolve_target,
                        opaque_scene_color,
                        width,
                        height,
                        1,
                    );
                    if opaque_scene_color.mip_level_count() > 1 {
                        #[cfg(feature = "fluid-perf-proofs")]
                        gpu.native_enc
                            .profile_next_encoder("node.render_scene water scene-color mipmaps");
                        gpu.native_enc.generate_mipmaps(opaque_scene_color);
                    }
                }
                gpu.native_enc.draw(
                    &manifold_gpu::DrawPass::color(
                        resolve_target,
                        manifold_gpu::GpuLoadAction::Load,
                    )
                    .depth(
                        if nearest_surface {
                            transmissive_depth_scratch
                        } else {
                            opaque_depth_snapshot
                        },
                        if nearest_surface {
                            transmission_depth_stencil
                        } else {
                            self.blend_depth_stencil.as_ref().expect("ensured")
                        },
                        manifold_gpu::GpuLoadAction::Load,
                        manifold_gpu::DepthStore::Store,
                    ),
                    &blend_draw_calls[layer..batch_end],
                    "node.render_scene transmissive layer",
                );
                layer = batch_end;
            }
        }

        Some((
            target.clone(),
            resolve_target.clone(),
            depth_resolve_target.cloned(),
            velocity_resolve_target.cloned(),
        ))
    }
}

/// What the march and the mist integration read of one water field.
fn field_draw<'a>(field: &'a FieldObject<'_>) -> FieldDraw<'a> {
    FieldDraw {
        levelset: field.levelset,
        velocity: field.velocity,
        mist: field.mist,
        tiles: field.tiles,
        extent: field.extent,
        pool_tiles: field.pool_tiles,
        cell_size: field.cell_size,
        model: field.model,
        prev_model: field.prev_model,
        slot: field.slot,
    }
}
