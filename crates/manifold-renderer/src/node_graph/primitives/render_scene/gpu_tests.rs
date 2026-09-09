    use super::*;
    use crate::generators::mesh_common::InstanceTransform;
    use crate::node_graph::light::{Light, LightMode, ShadowSoftness};
    use half::f16;
    use manifold_gpu::{
        GpuCompareFunction, GpuDepthStencilDesc, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    fn readback_rgba16f(
        device: &manifold_gpu::GpuDevice,
        tex: &manifold_gpu::GpuTexture,
        w: u32,
        h: u32,
    ) -> Vec<[f32; 4]> {
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("shaft-march-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    fn upload_r32f(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        raw: &[f32],
        label: &str,
    ) -> manifold_gpu::GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::R32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label,
            mip_levels: 1,
        });
        let bytes =
            unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), std::mem::size_of_val(raw)) };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn mat4_mul_vec4_test(m: [[f32; 4]; 4], v: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for (row, slot) in out.iter_mut().enumerate() {
            *slot = m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row] * v[3];
        }
        out
    }

    fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }
    fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn scale3(a: [f32; 3], s: f32) -> [f32; 3] {
        [a[0] * s, a[1] * s, a[2] * s]
    }
    fn len3(a: [f32; 3]) -> f32 {
        (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
    }
    fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn henyey_greenstein_test(g: f32, cos_theta: f32) -> f32 {
        let g2 = g * g;
        let denom = (1.0 + g2 - 2.0 * g * cos_theta).max(1e-6).powf(1.5);
        (1.0 - g2) / (4.0 * std::f32::consts::PI * denom)
    }

    /// CINEMATIC_POST_DESIGN.md D2's committed hash, Rust twin — WGSL
    /// `fract` is `x - floor(x)` (always non-negative), NOT Rust's
    /// `f32::fract` (which truncates toward zero and can be negative).
    #[allow(clippy::excessive_precision)] // the committed hash constant, verbatim
    fn hash01(px: [f32; 2]) -> f32 {
        let d = px[0] * 12.9898 + px[1] * 78.233;
        let v = d.sin() * 43758.5453;
        v - v.floor()
    }

    /// One march-visible light, CPU-twin shape mirroring `shaft_lights`'
    /// binding(2) 3-vec4 packing (VOLUMETRIC_LIGHT_DESIGN.md D2, P3): `slot
    /// < 0.0` means unshadowed glow (no caster table lookup at all) — this
    /// is how the Point-light case below stays simple (no second shadow
    /// map fixture needed; D2's honest "no slot -> vis=1.0" consequence is
    /// exactly what's being exercised).
    struct MarchLight {
        mode_point: bool,
        pos: [f32; 3],
        dir: [f32; 3],
        range: f32,
        color: [f32; 3],
        slot: f32,
    }

    /// Plain-Rust twin of `shaft_march.wgsl`'s `cs_main`, Sun AND Point
    /// lights (P3), shadow visibility computed against a KNOWN-constant
    /// occluder light-space depth (`occluder_ndc_z`) rather than
    /// re-deriving PCF — legitimate because the GPU fixture's shadow map is
    /// spatially uniform by construction (a single flat occluder plane
    /// under a straight-down Sun ortho projection projects to the SAME
    /// light-space depth everywhere), so a linear-filtered hardware compare
    /// against a constant field can never partially blend — it's exactly
    /// the same boolean at all 4 taps. Only `slot == 0` (the Sun in this
    /// fixture) ever consults `vp`/`bias`/`occluder_ndc_z`; any other slot
    /// (including the Point light's `-1`, unshadowed) short-circuits to
    /// `vis = 1.0`, matching `shadow_vis`'s `slot_f < 0.0` branch.
    #[allow(clippy::too_many_arguments)]
    fn cpu_march_reference(
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        raw_depth: f32,
        near: f32,
        far: f32,
        cam_pos: [f32; 3],
        right: [f32; 3],
        up: [f32; 3],
        fwd: [f32; 3],
        fov_y: f32,
        aspect: f32,
        fog_density: f32,
        height_falloff: f32,
        g: f32,
        shaft_intensity: f32,
        steps: u32,
        exposure_ev: f32,
        lights: &[MarchLight],
        vp: [[f32; 4]; 4],
        bias: f32,
        occluder_ndc_z: f32,
    ) -> [f32; 3] {
        let view_z = crate::node_graph::camera::linearize_depth(raw_depth, near, far);
        let uv = [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32];
        let ndc_x = uv[0] * 2.0 - 1.0;
        let ndc_y = 1.0 - uv[1] * 2.0;
        let tan_half_fov = (fov_y * 0.5).tan();

        let ray = add3(
            add3(
                scale3(fwd, view_z),
                scale3(right, ndc_x * aspect * tan_half_fov * view_z),
            ),
            scale3(up, ndc_y * tan_half_fov * view_z),
        );
        let ray_length = len3(ray);
        let ray_dir = if ray_length > 1e-6 { scale3(ray, 1.0 / ray_length) } else { fwd };

        let seg = ray_length / steps as f32;
        let t0 = (hash01([x as f32, y as f32]) - 0.5) * seg;

        let mut transmittance = 1.0f32;
        let mut accum = [0.0f32; 3];
        for i in 0..steps {
            let t = seg * (i as f32 + 0.5) + t0;
            let px = add3(cam_pos, scale3(ray_dir, t));
            let sigma = fog_density * (-height_falloff * px[1].max(0.0)).exp();

            for light in lights {
                let vis = if light.slot < 0.0 {
                    1.0
                } else {
                    let clip = mat4_mul_vec4_test(vp, [px[0], px[1], px[2], 1.0]);
                    if clip[3] <= 0.0 {
                        1.0
                    } else {
                        let ndc_z = clip[2] / clip[3];
                        let ndc_xy = [clip[0] / clip[3], clip[1] / clip[3]];
                        let suv = [ndc_xy[0] * 0.5 + 0.5, ndc_xy[1] * -0.5 + 0.5];
                        if !(0.0..=1.0).contains(&suv[0])
                            || !(0.0..=1.0).contains(&suv[1])
                            || !(0.0..=1.0).contains(&ndc_z)
                        {
                            1.0
                        } else {
                            let ref_depth = ndc_z - bias;
                            if ref_depth < occluder_ndc_z { 1.0 } else { 0.0 }
                        }
                    }
                };

                // D2: Sun att = 1.0, fixed L (dir toward light). Point att =
                // 1/(1+d²/range²) (light.rs:261), L = normalize(pos - x).
                let (light_to_x_dir, att) = if light.mode_point {
                    let to_light = sub3(light.pos, px);
                    let d_sq = dot3(to_light, to_light);
                    let r_sq = light.range * light.range;
                    let att = if r_sq < 1e-10 { 0.0 } else { 1.0 / (1.0 + d_sq / r_sq) };
                    let d = d_sq.max(1e-12).sqrt();
                    (scale3(to_light, -1.0 / d), att)
                } else {
                    (light.dir, 1.0)
                };
                let cos_theta = dot3(ray_dir, light_to_x_dir);
                let phase = henyey_greenstein_test(g, cos_theta);
                for (c, acc) in accum.iter_mut().enumerate() {
                    *acc += transmittance * sigma * vis * att * phase * light.color[c] * seg;
                }
            }
            transmittance *= (-sigma * seg).exp();
        }

        [
            accum[0] * shaft_intensity * exposure_ev.exp2(),
            accum[1] * shaft_intensity * exposure_ev.exp2(),
            accum[2] * shaft_intensity * exposure_ev.exp2(),
        ]
    }

    /// VOLUMETRIC_LIGHT_DESIGN.md V3: the SHIPPING `shaft_march.wgsl` kernel
    /// (dispatched exactly as `ensure_shaft_pipelines`/`evaluate` builds it —
    /// `depth_common.wgsl` concatenated ahead) must agree with
    /// `cpu_march_reference` (same committed D2 math, independent Rust
    /// implementation) within 1e-3, on a small synthetic grid: one Sun
    /// light (shadow-casting, against a fabricated 8x8 shadow map — a real
    /// depth-only render of one flat occluder quad, not a hand-uploaded
    /// texture, since Depth32Float textures don't support CPU upload) PLUS
    /// one Point light (P3, `cast_shadows=false` — no second caster map
    /// needed, exercises D2's unshadowed-glow branch and the Point
    /// attenuation/phase math), flat scene depth.
    #[test]
    fn shaft_march_matches_cpu_reference() {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let raw_depth = 0.5f32;
        let half_depth_raw = vec![raw_depth; (w * h) as usize];
        let half_depth = upload_r32f(&device, w, h, &half_depth_raw, "shaft-test-half-depth");

        let cam_pos = [0.0f32, 10.0, 20.0];
        let right = [1.0f32, 0.0, 0.0];
        let up = [0.0f32, 1.0, 0.0];
        let fwd = [0.0f32, 0.0, -1.0];
        let near = 0.1f32;
        let far = 100.0f32;
        let fov_y = std::f32::consts::FRAC_PI_3;
        let aspect = 1.0f32;

        let light = Light::sun(
            [0.0, 50.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            30.0,
            true,
            ShadowSoftness::Soft,
            0.001,
            8,
        );
        assert_eq!(light.mode, LightMode::Sun);
        let vp = light.shadow_view_proj();

        // P3: a Point light alongside the Sun. Deliberately `cast_shadows:
        // false` — no second shadow map fixture needed; D2's honest
        // consequence ("no caster slot -> vis=1.0 unshadowed glow") is
        // exactly what this exercises, and it's a real production shape
        // ("one bare glow" in the P3 acceptance demo).
        let point_light = Light::point(
            [8.0, 6.0, 4.0],
            [0.0, 0.0, 0.0],
            [0.3, 0.6, 1.0],
            2.0,
            12.0,
            false,
            ShadowSoftness::Soft,
            0.001,
            8,
        );
        assert_eq!(point_light.mode, LightMode::Point);

        // Fabricate the 8x8 shadow map by actually rendering one large flat
        // occluder quad at world y=20 (between the light at y=50 and the
        // geometry the march samples near y<=10), sized well beyond the
        // ortho frustum's +/-30 extent so the WHOLE map gets one constant
        // stored depth — same production shadow_depth.wgsl pipeline
        // render_scene.rs itself uses for real casters.
        let smap_res = 8u32;
        let shadow_map = device.create_texture(&GpuTextureDesc {
            width: smap_res,
            height: smap_res,
            depth: 1,
            format: GpuTextureFormat::Depth32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label: "shaft-test-shadow-map",
            mip_levels: 1,
        });
        let shadow_ds = device.create_depth_stencil_state(&GpuDepthStencilDesc {
            compare: GpuCompareFunction::Less,
            write_enabled: true,
        });
        let shadow_pipeline = device.create_render_pipeline_depth_only(
            include_str!("../shaders/shadow_depth.wgsl"),
            "vs_main",
            "fs_shadow",
            GpuTextureFormat::Depth32Float,
            "shaft-test-shadow-pipeline",
        );
        let quad_y = 20.0f32;
        let ext = 80.0f32;
        let mk_vertex = |x: f32, z: f32| MeshVertex {
            position: [x, quad_y, z],
            _pad0: 0.0,
            normal: [0.0, 1.0, 0.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
        };
        let quad_verts = [
            mk_vertex(-ext, -ext),
            mk_vertex(ext, -ext),
            mk_vertex(ext, ext),
            mk_vertex(-ext, -ext),
            mk_vertex(ext, ext),
            mk_vertex(-ext, ext),
        ];
        let vbuf = device.create_buffer_shared(std::mem::size_of_val(&quad_verts) as u64);
        unsafe {
            vbuf.write(0, bytemuck::cast_slice(&quad_verts));
        }
        let identity_inst = InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0, 0.0, 0.0, 0.0],
        };
        let ibuf = device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
        unsafe {
            ibuf.write(0, bytemuck::bytes_of(&identity_inst));
        }

        const IDENTITY4: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let shadow_uniforms = ShadowUniforms { light_view_proj: vp, model: IDENTITY4 };
        let shadow_bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&shadow_uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &vbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &ibuf, offset: 0 },
        ];
        let shadow_draw =
            manifold_gpu::GpuEncoder::depth_msaa_draw(&shadow_pipeline, &shadow_bindings, 6, 1);
        let mut shadow_enc = device.create_encoder("shaft-test-shadow-pass");
        shadow_enc.draw_instanced_depth_only_batch(&shadow_map, &shadow_ds, &[shadow_draw], "shaft-test-shadow");
        shadow_enc.commit_and_wait_completed();

        let bias = light.shadow_bias;
        let mut caster_table: Vec<[f32; 4]> = vec![[0.0f32; 4]; MAX_SHADOW_CASTING_LIGHTS * CASTER_VEC4_STRIDE];
        caster_table[0] = vp[0];
        caster_table[1] = vp[1];
        caster_table[2] = vp[2];
        caster_table[3] = vp[3];
        caster_table[4] = [bias, 2.0, 1.0 / smap_res as f32, 0.0];
        let caster_bytes: &[u8] = bytemuck::cast_slice(&caster_table);

        // 3-vec4-per-light packing (matches `shaft_march.wgsl`'s
        // binding(2) layout / `RenderScene::evaluate`'s shaft_light_data
        // build): [pos_or_dir(.w=mode)], [color.rgb, slot], [range, 0,0,0].
        // Sun (slot 0, this fixture's only caster) -> dir-toward-light,
        // mode 0. Point (slot -1, unshadowed) -> world pos, mode 1.
        let shaft_light_data: Vec<[f32; 4]> = vec![
            [-light.dir[0], -light.dir[1], -light.dir[2], 0.0],
            [light.color[0], light.color[1], light.color[2], 0.0],
            [light.range, 0.0, 0.0, 0.0],
            [point_light.pos[0], point_light.pos[1], point_light.pos[2], 1.0],
            [point_light.color[0], point_light.color[1], point_light.color[2], -1.0],
            [point_light.range, 0.0, 0.0, 0.0],
        ];
        let shaft_light_bytes: &[u8] = bytemuck::cast_slice(&shaft_light_data);

        let dummy_depth = device.create_texture(&GpuTextureDesc {
            width: 1,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Depth32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label: "shaft-test-dummy-depth",
            mip_levels: 1,
        });
        let shadow_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Linear,
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            compare: Some(GpuCompareFunction::Less),
            ..Default::default()
        });

        let fog_density = 0.05f32;
        let height_falloff = 0.0f32;
        let g = 0.0f32;
        let shaft_intensity = 1.0f32;
        let steps = 8u32;
        let exposure_ev = 0.0f32;
        let march_uniforms = ShaftMarchUniforms {
            camera_pos: [cam_pos[0], cam_pos[1], cam_pos[2], near],
            camera_right: [right[0], right[1], right[2], far],
            camera_up: [up[0], up[1], up[2], fov_y],
            camera_fwd: [fwd[0], fwd[1], fwd[2], aspect],
            fog_shaft: [fog_density, height_falloff, g, shaft_intensity],
            misc: [steps as f32, 2.0, exposure_ev, 0.0],
        };

        let pipeline = device.create_compute_pipeline(
            include_str!("../shaders/shaft_march.wgsl"),
            "cs_main",
            "shaft-march-test",
        );

        let out_tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            label: "shaft-test-out",
            mip_levels: 1,
        });

        let mut enc = device.create_encoder("shaft-march-dispatch");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&march_uniforms) },
                GpuBinding::Texture { binding: 1, texture: &half_depth },
                GpuBinding::Bytes { binding: 2, data: shaft_light_bytes },
                GpuBinding::Bytes { binding: 3, data: caster_bytes },
                GpuBinding::Texture { binding: 4, texture: &shadow_map },
                GpuBinding::Texture { binding: 5, texture: &dummy_depth },
                GpuBinding::Texture { binding: 6, texture: &dummy_depth },
                GpuBinding::Texture { binding: 7, texture: &dummy_depth },
                GpuBinding::Sampler { binding: 8, sampler: &shadow_sampler },
                GpuBinding::Texture { binding: 9, texture: &out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "shaft-march-dispatch",
        );
        enc.commit_and_wait_completed();

        let gpu_out = readback_rgba16f(&device, &out_tex, w, h);

        let occluder_ndc_z = {
            let clip = mat4_mul_vec4_test(vp, [0.0, quad_y, 0.0, 1.0]);
            clip[2] / clip[3]
        };

        let march_lights = [
            MarchLight {
                mode_point: false,
                pos: light.pos,
                dir: light.dir,
                range: light.range,
                color: [light.color[0], light.color[1], light.color[2]],
                slot: 0.0,
            },
            MarchLight {
                mode_point: true,
                pos: point_light.pos,
                dir: point_light.dir,
                range: point_light.range,
                color: [point_light.color[0], point_light.color[1], point_light.color[2]],
                slot: -1.0,
            },
        ];

        for y in 0..h {
            for x in 0..w {
                let cpu = cpu_march_reference(
                    x, y, w, h, raw_depth, near, far, cam_pos, right, up, fwd, fov_y, aspect,
                    fog_density, height_falloff, g, shaft_intensity, steps, exposure_ev,
                    &march_lights, vp, bias, occluder_ndc_z,
                );
                let idx = (y * w + x) as usize;
                let gpu = gpu_out[idx];
                for c in 0..3 {
                    assert!(
                        (cpu[c] - gpu[c]).abs() < 1e-3,
                        "pixel ({x},{y}) channel {c}: cpu={} gpu={} (V3 tolerance 1e-3)",
                        cpu[c],
                        gpu[c]
                    );
                }
            }
        }
    }

    #[test]
    fn prewarm_pipelines_populates_the_shared_render_cache() {
        let device = crate::test_device();
        // Order-independent (BUG-144): the cache is process-global and shared
        // with other gpu_tests, so another test's prewarm may already have
        // populated the exact entries this call would add — an
        // after > before delta then reads zero even though prewarm worked.
        // Assert the cache ends up populated, not that THIS call grew it.
        RenderScene::prewarm_pipelines(&device);
        let after = device.render_pipeline_cache_len();
        assert!(
            after > 0,
            "prewarm_pipelines must leave the render cache populated: after={after}"
        );

        // Idempotent: a second call must not grow the cache further (every
        // variant already hit).
        RenderScene::prewarm_pipelines(&device);
        assert_eq!(
            device.render_pipeline_cache_len(),
            after,
            "a second prewarm pass must be a pure cache hit, not add more entries"
        );

        // EVERY combination `pipeline_for` can compile lazily must already
        // be warm — proves this isn't just "some pipeline got created", and
        // fails loudly if a new aux-output dimension is added to
        // `pipeline_for` without being added to `prewarm_pipelines`
        // (RAYTRACING_DESIGN.md section 12 AM1 added `emit_ao_mask` as
        // exactly such a dimension).
        let mut scene = RenderScene::default();
        let cache_before_use = device.render_pipeline_cache_len();
        for kind in [
            MaterialKind::Unlit,
            MaterialKind::Phong,
            MaterialKind::Pbr,
            MaterialKind::Cel,
        ] {
            for blend in [false, true] {
                for (emit_velocity, emit_ao_mask, emit_denoise_feed) in [
                    (false, false, false),
                    (true, false, false),
                    (false, true, false),
                    (true, true, false),
                    (false, false, true),
                    (true, false, true),
                    (false, true, true),
                    (true, true, true),
                ] {
                    scene.pipeline_for(&device, kind, emit_velocity, emit_ao_mask, emit_denoise_feed, blend);
                    assert_eq!(
                        device.render_pipeline_cache_len(),
                        cache_before_use,
                        "pipeline_for({kind:?}, velocity={emit_velocity}, ao_mask={emit_ao_mask}, denoise={emit_denoise_feed}, blend={blend}) after prewarm must be a cache hit, not compile a new pipeline"
                    );
                }
            }
        }
    }

    // --- G-P3 anisotropic filtering (GLB_CONFORMANCE_DESIGN.md D7) -----
    //
    // Both proofs sample the SAME horizontally-striped texture (constant
    // along U, alternating bands along V — no detail in U at all) with the
    // SAME highly anisotropic derivative via `textureSampleGrad` (ddx large
    // in U, ddy small in V): the "floor viewed edge-on" case. Isotropic
    // filtering picks its LOD off the larger (U) footprint and blurs V
    // along with it even though V never needed blurring; anisotropic
    // filtering takes multiple taps along U at a sharper effective LOD,
    // preserving V's stripe edges. `textureSampleGrad` (not the implicit-
    // derivative `textureSample`) is required here because a compute
    // kernel has no rasterizer-derived screen-space derivatives to draw
    // an implicit LOD from — WGSL restricts `textureSample` to fragment
    // shaders for exactly that reason.

    const ANISO_TEST_SIZE: u32 = 128;
    const ANISO_TEST_BANDS: u32 = 8;
    const ANISO_TEST_ROW_LEN: u32 = 256;

    const ANISO_TEST_WGSL: &str = r#"
const N: u32 = 256u;

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<storage, read_write> out_buf: array<vec4<f32>>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= N) {
        return;
    }
    let v = (f32(gid.x) + 0.5) / f32(N);
    let uv = vec2<f32>(0.5, v);
    // Grazing-minification derivative: large in U (the axis the texture
    // carries no detail in — Metal's isotropic LOD pick is driven by
    // this larger footprint regardless), small in V (the axis the
    // stripes actually live in).
    let ddx = vec2<f32>(0.5, 0.0);
    let ddy = vec2<f32>(0.0, 0.02);
    out_buf[gid.x] = textureSampleGrad(src_tex, samp, uv, ddx, ddy);
}
"#;

    /// 128x128 Rgba8Unorm, `ANISO_TEST_BANDS` horizontal bands alternating
    /// ~0.05 / ~0.95 (avoids pure 0/1 clipping ambiguity), full mip chain
    /// via the same hardware `generate_mipmaps` blit `gltf_texture_source`
    /// uses (F-P6 precedent) — a synthesized fixture, not production
    /// texture-decode code, so this does not touch mip generation itself
    /// (forbidden move).
    fn make_striped_grazing_texture(device: &manifold_gpu::GpuDevice) -> manifold_gpu::GpuTexture {
        let n = ANISO_TEST_SIZE;
        let band_h = n / ANISO_TEST_BANDS;
        let mut data = vec![0u8; (n * n * 4) as usize];
        for y in 0..n {
            let band = y / band_h;
            let v: u8 = if band.is_multiple_of(2) { 13 } else { 242 };
            for x in 0..n {
                let o = ((y * n + x) * 4) as usize;
                data[o] = v;
                data[o + 1] = v;
                data[o + 2] = v;
                data[o + 3] = 255;
            }
        }
        let mip_levels = manifold_gpu::GpuTextureDesc::max_mip_levels(n, n);
        let tex = device.create_texture(&GpuTextureDesc {
            width: n,
            height: n,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
            label: "aniso-test-stripes",
            mip_levels,
        });
        device.upload_texture(&tex, &data);
        let mut enc = device.create_encoder("aniso-test-mip-gen");
        enc.generate_mipmaps(&tex);
        enc.commit_and_wait_completed();
        tex
    }

    /// Dispatch `ANISO_TEST_WGSL` and read back `ANISO_TEST_ROW_LEN` grazing
    /// samples as RGBA floats.
    fn sample_grazing_row(
        device: &manifold_gpu::GpuDevice,
        tex: &manifold_gpu::GpuTexture,
        sampler: &manifold_gpu::GpuSampler,
    ) -> Vec<[f32; 4]> {
        let pipeline = device.create_compute_pipeline(ANISO_TEST_WGSL, "cs_main", "aniso-test-sample");
        let out_buf = device.create_buffer_shared(u64::from(ANISO_TEST_ROW_LEN) * 16);
        let bindings = [
            GpuBinding::Texture { binding: 0, texture: tex },
            GpuBinding::Sampler { binding: 1, sampler },
            GpuBinding::Buffer { binding: 2, buffer: &out_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("aniso-test-dispatch");
        enc.dispatch_compute(&pipeline, &bindings, [ANISO_TEST_ROW_LEN / 64, 1, 1], "aniso-test");
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared output buffer");
        let floats: &[f32] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<f32>(), (ANISO_TEST_ROW_LEN * 4) as usize)
        };
        (0..ANISO_TEST_ROW_LEN as usize)
            .map(|i| {
                let o = i * 4;
                [floats[o], floats[o + 1], floats[o + 2], floats[o + 3]]
            })
            .collect()
    }

    /// D7 invariant (`docs/GLB_CONFORMANCE_DESIGN.md` section 4): `max_anisotropy:
    /// 1` must be byte-identical to pre-field behavior. Every call site that
    /// predates the field builds its `GpuSamplerDesc` via `..Default::
    /// default()` and never mentions the field at all — that shape, and a
    /// sampler that explicitly spells `max_anisotropy: 1`, must sample
    /// identically.
    #[test]
    fn sampler_aniso_one_is_byte_identical() {
        let device = crate::test_device();
        let tex = make_striped_grazing_texture(&device);

        let untouched = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });
        let explicit_one = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Linear,
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            compare: None,
            max_anisotropy: 1,
        });

        let a = sample_grazing_row(&device, &tex, &untouched);
        let b = sample_grazing_row(&device, &tex, &explicit_one);
        assert_eq!(a.len(), b.len());
        for (i, (pa, pb)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                pa, pb,
                "row sample {i}: default-spread sampler (max_anisotropy never mentioned) must \
                 be byte-identical to an explicit max_anisotropy: 1 sampler"
            );
        }
    }

    /// G-P3 gate proof (`docs/GLB_CONFORMANCE_DESIGN.md`, the F-P3 numeric-
    /// not-look style): aniso 8 must sharpen grazing minification relative
    /// to aniso 1. High-frequency energy (total variation — sum of absolute
    /// consecutive-sample deltas along the sampled row) is high when the
    /// alternating bands stay resolved and collapses toward zero as they
    /// blur into a uniform mid-grey, so it is a direct, numeric proxy for
    /// "did anisotropic filtering keep this sharp."
    #[test]
    fn aniso_sharpens_grazing_minification() {
        let device = crate::test_device();
        let tex = make_striped_grazing_texture(&device);

        let aniso_1 = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Linear,
            max_anisotropy: 1,
            ..Default::default()
        });
        let aniso_8 = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Linear,
            max_anisotropy: 8,
            ..Default::default()
        });

        let low = sample_grazing_row(&device, &tex, &aniso_1);
        let high = sample_grazing_row(&device, &tex, &aniso_8);

        let energy =
            |row: &[[f32; 4]]| -> f32 { row.windows(2).map(|w| (w[1][0] - w[0][0]).abs()).sum() };
        let e1 = energy(&low);
        let e8 = energy(&high);
        assert!(
            e8 > e1 * 1.1,
            "aniso 8 must sharpen the grazing-angle stripe pattern relative to aniso 1: \
             e(aniso=1)={e1:.4} e(aniso=8)={e8:.4}"
        );
    }

    // ── RAYTRACING_DESIGN.md section 17.5 DN-E gates ────────────

    /// DN-E I-DN1: `rt_denoise_feed` off (default) forces nothing beyond the
    /// existing D14 shape — byte-identical path proven by graph-tool render
    /// cmp above; this is the structural cross-check.
    #[test]
    fn rt_denoise_feed_off_forces_nothing() {
        let s = RenderScene::new();
        let mut params = crate::node_graph::effect_node::ParamValues::default();
        // Default: rt_enabled=false, temporal_upscale=false, rt_denoise_feed=false
        let forced = s.force_consumed_outputs(&params);
        assert!(forced.is_empty(), "default scene must force no outputs");

        // rt_enabled only (D14): forces depth+velocity only, never denoise feeds.
        params.insert("rt_enabled".into(), ParamValue::Bool(true));
        let forced = s.force_consumed_outputs(&params);
        assert_eq!(forced, &["depth", "velocity"],
            "rt_enabled must force depth/velocity only, not denoise feeds");
    }

    /// DN-E: `rt_denoise_feed=true` forces the eight G-buffer outputs
    /// (DN-L added `reactive_mask`).
    #[test]
    fn rt_denoise_feed_on_forces_all_feeds() {
        let s = RenderScene::new();
        let mut params = crate::node_graph::effect_node::ParamValues::default();
        params.insert("rt_denoise_feed".into(), ParamValue::Bool(true));
        let forced = s.force_consumed_outputs(&params);
        let expected: &[&str] = &[
            "depth", "velocity", "normals", "roughness",
            "diffuse_albedo", "specular_albedo", "specular_hit_distance",
            "reactive_mask",
        ];
        assert_eq!(forced, expected,
            "rt_denoise_feed=true must force all 8 denoiser G-buffer outputs");
    }

    /// DN-E: output_format maps all 5 new ports to their correct formats.
    #[test]
    fn rt_denoise_feed_output_formats_are_correct() {
        let s = RenderScene::new();
        assert_eq!(s.output_format("normals"), Some(manifold_gpu::GpuTextureFormat::Rgba16Float));
        assert_eq!(s.output_format("roughness"), Some(manifold_gpu::GpuTextureFormat::R16Float));
        assert_eq!(s.output_format("diffuse_albedo"), Some(manifold_gpu::GpuTextureFormat::Rgba16Float));
        assert_eq!(s.output_format("specular_albedo"), Some(manifold_gpu::GpuTextureFormat::Rgba16Float));
        assert_eq!(s.output_format("specular_hit_distance"), Some(manifold_gpu::GpuTextureFormat::R16Float));
        assert_eq!(s.output_format("reactive_mask"), Some(manifold_gpu::GpuTextureFormat::R16Float));
    }

    /// DN-E: the `rt_denoise_feed` param exists, defaults to false, and is a Bool.
    #[test]
    fn rt_denoise_feed_param_exists_with_correct_default() {
        let s = RenderScene::new();
        let param = s.parameters().iter().find(|p| p.name == "rt_denoise_feed")
            .expect("rt_denoise_feed param must exist");
        assert_eq!(param.name, "rt_denoise_feed");
        assert_eq!(param.default, ParamValue::Bool(false));
        assert_eq!(param.ty, crate::node_graph::parameters::ParamType::Bool);
    }

    /// RT_QUALITY_SETTINGS_DESIGN.md I3: a ray-resolution change reallocates
    /// the RT targets and reports the reset — temporal history never survives
    /// a trace-dims change, and a canvas change with truncating-equal trace
    /// dims still reallocates the full-res targets.
    #[test]
    fn ray_resolution_or_canvas_change_fires_rt_realloc_reset() {
        let device = crate::test_device();
        let mut s = RenderScene::new();

        // First allocation resets.
        assert!(s.ensure_rt_irradiance(&device, 128, 128, 256, 256));
        s.ensure_rt_masks(&device, 128, 128, 256, 256);
        // Same dims: no realloc, no reset.
        assert!(!s.ensure_rt_irradiance(&device, 128, 128, 256, 256));
        // Tier flip (Half → Native at fixed canvas): trace dims change.
        assert!(s.ensure_rt_irradiance(&device, 256, 256, 256, 256));
        assert_eq!(s.rt_irr_trace_w, 256);
        // The truncation hole: canvas 256 → 257 at Quarter (trace 64 → 64).
        // Full-class textures (history included) must still realloc.
        assert!(s.ensure_rt_irradiance(&device, 64, 64, 256, 256));
        assert!(s.ensure_rt_irradiance(&device, 64, 64, 257, 257));
        assert_eq!(s.rt_irr_width, 257);
        // Masks guard follows the same dual-pair discipline.
        s.ensure_rt_masks(&device, 64, 64, 256, 256);
        assert_eq!(s.rt_mask_trace_w, 64);
        assert_eq!(s.rt_mask_width, 256);
    }

    /// RT-Stage-3 P1 (BUG-mkgh): the firefly clamp's median is a partial
    /// selection over the non-void 3x3 subset's luma, returning the element
    /// at sorted index `n/2` (odd n = the middle; even n = the element at
    /// index n/2). This is the Rust mirror of the MSL `firefly_median_luma`
    /// in `crates/manifold-gpu/src/metal/raytrace.rs` — kept in lockstep by
    /// the gpu-proofs value test, which asserts the GPU kernel lands on the
    /// same order statistic. Plain (non-GPU) unit test: pins the selection
    /// convention so a retune can't silently drift the CPU mirror.
    #[test]
    fn firefly_median_selection_is_the_n_over_2_order_statistic() {
        let median = |ls: &[f32]| {
            let mut v = ls.to_vec();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };

        // Odd n: the true middle.
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        // The full 9-element subset (center included) of a hot-center
        // firefly: median is the 5th-smallest (a dim neighbor), not the
        // outlier — which is exactly what lets the clamp engage.
        assert_eq!(median(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 100.0]), 1.0);
        // Neighbors 1..8 + center 100 => median 5 (the 5th-smallest).
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 100.0]), 5.0);
        // Even n: index n/2 (the upper of the two middles), matching the MSL
        // `mid = n / 2` convention.
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 3.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4.0);
    }
