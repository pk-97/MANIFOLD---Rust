    use super::*;
    use crate::generators::mesh_common::InstanceTransform;
    use crate::node_graph::light::{Light, LightMode, ShadowSoftness};
    use bytemuck::Zeroable;
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

    fn readback_depth32f(
        device: &manifold_gpu::GpuDevice,
        tex: &manifold_gpu::GpuTexture,
        w: u32,
        h: u32,
    ) -> Vec<f32> {
        let bytes_per_row = w * 4;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("appearance-depth-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared depth readback buffer");
        let values: &[f32] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<f32>(), (w * h) as usize)
        };
        values.to_vec()
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

    fn upload_rgba16f(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        pixels: &[[f32; 4]],
        label: &str,
    ) -> manifold_gpu::GpuTexture {
        assert_eq!(pixels.len(), (w * h) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label,
            mip_levels: 1,
        });
        let mut bytes = Vec::with_capacity(pixels.len() * 8);
        for pixel in pixels {
            for channel in pixel {
                bytes.extend_from_slice(&f16::from_f32(*channel).to_bits().to_le_bytes());
            }
        }
        device.upload_texture(&tex, &bytes);
        tex
    }

    fn upload_rgba32f(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        pixels: &[[f32; 4]],
        label: &str,
    ) -> manifold_gpu::GpuTexture {
        assert_eq!(pixels.len(), (w * h) as usize);
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba32Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label,
            mip_levels: 1,
        });
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| pixel.iter().flat_map(|channel| channel.to_le_bytes()))
            .collect();
        device.upload_texture(&tex, &bytes);
        tex
    }

    fn rgba16f_target(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        label: &str,
    ) -> manifold_gpu::GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::COPY_SRC,
            label,
            mip_levels: 1,
        })
    }

    fn decode_rgba16f_readback(
        buffer: &manifold_gpu::GpuBuffer,
        pixel_count: usize,
    ) -> Vec<[f32; 4]> {
        let ptr = buffer.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u16>(), pixel_count * 4)
        };
        halves
            .chunks_exact(4)
            .map(|pixel| {
                [
                    f16::from_bits(pixel[0]).to_f32(),
                    f16::from_bits(pixel[1]).to_f32(),
                    f16::from_bits(pixel[2]).to_f32(),
                    f16::from_bits(pixel[3]).to_f32(),
                ]
            })
            .collect()
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
            color: [1.0; 4],
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
        let shadow_uniforms = ShadowUniforms {
            light_view_proj: vp,
            model: IDENTITY4,
            appearance: [1.0, 0.0, 0.0, 0.0],
            ..bytemuck::Zeroable::zeroed()
        };
        let alpha_texture = upload_rgba16f(&device, 1, 1, &[[1.0; 4]], "shadow-alpha");
        let alpha_sampler = device.create_sampler(&Default::default());
        let shadow_bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&shadow_uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &vbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &ibuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &vbuf, offset: 0 },
            GpuBinding::Texture { binding: 4, texture: &alpha_texture },
            GpuBinding::Sampler { binding: 5, sampler: &alpha_sampler },
        ];
        let shadow_draw =
            manifold_gpu::GpuEncoder::depth_msaa_draw(&shadow_pipeline, &shadow_bindings, 6, 1);
        let mut shadow_enc = device.create_encoder("shaft-test-shadow-pass");
        shadow_enc.draw_instanced_depth_only_batch(&shadow_map, &shadow_ds, &[shadow_draw], "shaft-test-shadow");
        shadow_enc.commit_and_wait_completed();

        let bias = light.shadow_bias;
        let mut caster_table: Vec<[f32; 4]> = vec![[0.0f32; 4]; MAX_RASTER_SHADOW_CASTERS * CASTER_VEC4_STRIDE];
        caster_table[0] = vp[0];
        caster_table[1] = vp[1];
        caster_table[2] = vp[2];
        caster_table[3] = vp[3];
        caster_table[4] = [bias, 2.0, 1.0 / smap_res as f32, 0.0];
        let caster_bytes: &[u8] = bytemuck::cast_slice(&caster_table);

        let shaft_light_data: Vec<[f32; 4]> = light.packed(0.0).into_iter()
            .chain(point_light.packed(-1.0)).collect();
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

    /// Appearance visibility proof: the same two-triangle geometry is drawn
    /// twice into a native depth target. With all weights one, both triangles
    /// write depth. With the left triangle's weights zero, only the right
    /// triangle remains; the right-side depth is unchanged, proving that the
    /// visibility input does not alter vertex positions or triangle topology.
    #[test]
    fn appearance_weights_discard_zero_triangle_in_shadow_depth() {
        let device = crate::test_device();
        let (w, h) = (8u32, 4u32);
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let vertices = [
            test_mesh_vertex([-1.0, -1.0, 0.5]),
            test_mesh_vertex([0.0, -1.0, 0.5]),
            test_mesh_vertex([-1.0, 1.0, 0.5]),
            test_mesh_vertex([0.0, -1.0, 0.5]),
            test_mesh_vertex([1.0, -1.0, 0.5]),
            test_mesh_vertex([1.0, 1.0, 0.5]),
        ];
        let vbuf = device.create_buffer_shared(std::mem::size_of_val(&vertices) as u64);
        unsafe { vbuf.write(0, bytemuck::cast_slice(&vertices)); }
        let instance = InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0, 0.0, 0.0, 0.0],
        };
        let ibuf = device.create_buffer_shared(std::mem::size_of_val(&instance) as u64);
        unsafe { ibuf.write(0, bytemuck::bytes_of(&instance)); }
        let all_visible = [1.0f32; 6];
        let left_hidden = [0.0f32, 0.0, 0.0, 1.0, 1.0, 1.0];
        let all_visible_buf = device.create_buffer_shared(std::mem::size_of_val(&all_visible) as u64);
        let left_hidden_buf = device.create_buffer_shared(std::mem::size_of_val(&left_hidden) as u64);
        unsafe {
            all_visible_buf.write(0, bytemuck::cast_slice(&all_visible));
            left_hidden_buf.write(0, bytemuck::cast_slice(&left_hidden));
        }
        let pipeline = device.create_render_pipeline_depth_only(
            include_str!("../shaders/shadow_depth.wgsl"),
            "vs_main",
            "fs_shadow",
            GpuTextureFormat::Depth32Float,
            "appearance-weight-shadow-proof",
        );
        let depth_state = device.create_depth_stencil_state(&GpuDepthStencilDesc {
            compare: GpuCompareFunction::Less,
            write_enabled: true,
        });

        let alpha_texture = upload_rgba16f(&device, 1, 1, &[[1.0; 4]], "shadow-alpha");
        let alpha_sampler = device.create_sampler(&Default::default());
        let render = |weights: &manifold_gpu::GpuBuffer| {
            let target = device.create_texture(&GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: GpuTextureFormat::Depth32Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                label: "appearance-weight-shadow-proof-depth",
                mip_levels: 1,
            });
            let uniforms = ShadowUniforms {
                light_view_proj: identity,
                model: identity,
                appearance: [1.0, 1.0, 0.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            };
            let bindings = [
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &vbuf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &ibuf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: weights, offset: 0 },
                GpuBinding::Texture { binding: 4, texture: &alpha_texture },
                GpuBinding::Sampler { binding: 5, sampler: &alpha_sampler },
            ];
            let draw = manifold_gpu::GpuEncoder::depth_msaa_draw(&pipeline, &bindings, 6, 1);
            let mut enc = device.create_encoder("appearance-weight-shadow-proof-pass");
            enc.draw_instanced_depth_only_batch(&target, &depth_state, &[draw], "appearance-weight-shadow-proof");
            enc.commit_and_wait_completed();
            readback_depth32f(&device, &target, w, h)
        };

        let baseline = render(&all_visible_buf);
        let masked = render(&left_hidden_buf);
        let left = |pixels: &[f32]| pixels[(2 * w + 1) as usize];
        let right = |pixels: &[f32]| pixels[(2 * w + 6) as usize];
        assert!((left(&baseline) - 0.5).abs() < 1e-5, "baseline left triangle did not write depth");
        assert!((right(&baseline) - 0.5).abs() < 1e-5, "baseline right triangle did not write depth");
        assert!((left(&masked) - 1.0).abs() < 1e-5, "zero-weight triangle still wrote shadow depth");
        assert!((right(&masked) - right(&baseline)).abs() < 1e-5, "visible triangle depth changed");
    }

    #[test]
    fn shadow_alpha_mask_preserves_uv_selection_transform_factor_and_mirror() {
        let device = crate::test_device();
        let identity = [
            [1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0],
        ];
        let mut vertices = Vec::new();
        for (left, right, uv) in [(-1.0, 0.0, 0.25), (0.0, 1.0, 0.75)] {
            for (x, y) in [(left, -1.0), (right, -1.0), (right, 1.0),
                (left, -1.0), (right, 1.0), (left, 1.0)] {
                let mut v = test_mesh_vertex([x, y, 0.5]);
                v.uv = [uv, 0.5];
                v._pad2 = [1.0 - uv, 0.5];
                vertices.push(v);
            }
        }
        let vbuf = device.create_buffer_shared((vertices.len() * std::mem::size_of::<MeshVertex>()) as u64);
        let vertex_count = vertices.len() as u32;
        unsafe { vbuf.write(0, bytemuck::cast_slice(&vertices)); }
        let ibuf = device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
        let map = upload_rgba16f(&device, 2, 1, &[[1.0, 1.0, 1.0, 0.0], [1.0; 4]], "cutout-proof-map");
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            mag_filter: manifold_gpu::GpuFilterMode::Nearest,
            min_filter: manifold_gpu::GpuFilterMode::Nearest,
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline_depth_only(
            include_str!("../shaders/shadow_depth.wgsl"), "vs_main", "fs_shadow",
            GpuTextureFormat::Depth32Float, "cutout-depth-proof");
        let depth_state = device.create_depth_stencil_state(&GpuDepthStencilDesc {
            compare: GpuCompareFunction::Less, write_enabled: true,
        });
        let render = |uv_set: f32, uv_m: [f32; 4], uv_tx: f32, alpha: f32, mirror: f32| {
            let target = device.create_texture(&GpuTextureDesc {
                width: 8, height: 4, depth: 1, format: GpuTextureFormat::Depth32Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
                label: "cutout-proof-depth", mip_levels: 1,
            });
            let instance = InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0, 0.0, 0.0, mirror] };
            unsafe { ibuf.write(0, bytemuck::bytes_of(&instance)); }
            let uniforms = ShadowUniforms {
                light_view_proj: identity, model: identity, appearance: [1.0, 0.0, 0.0, 0.0],
                alpha: [1.0, 0.5, alpha, 1.0], uv_m, uv_t: [uv_tx, 0.0, uv_set, 0.0],
            };
            let bindings = [
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &vbuf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &ibuf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &vbuf, offset: 0 },
                GpuBinding::Texture { binding: 4, texture: &map },
                GpuBinding::Sampler { binding: 5, sampler: &sampler },
            ];
            let draw = manifold_gpu::GpuEncoder::depth_msaa_draw(&pipeline, &bindings, vertex_count, 1);
            let mut enc = device.create_encoder("cutout-proof-pass");
            enc.draw_instanced_depth_only_batch(&target, &depth_state, &[draw], "cutout-proof");
            enc.commit_and_wait_completed();
            let pixels = readback_depth32f(&device, &target, 8, 4);
            [pixels[17], pixels[22]]
        };
        let uv_identity = [1.0, 0.0, 0.0, 1.0];
        assert_eq!(render(0.0, uv_identity, 0.0, 1.0, 0.0), [1.0, 0.5], "UV0 cutout");
        assert_eq!(render(1.0, uv_identity, 0.0, 1.0, 0.0), [0.5, 1.0], "UV1 cutout");
        assert_eq!(render(0.0, [-1.0, 0.0, 0.0, 1.0], 1.0, 1.0, 0.0), [0.5, 1.0], "transformed cutout");
        assert_eq!(render(0.0, uv_identity, 0.0, 0.4, 0.0), [1.0, 1.0], "factor times map alpha");
        assert_eq!(render(0.0, uv_identity, 0.0, 1.0, 1.0), [0.5, 1.0], "mirrored silhouette");

        for vertex in &mut vertices {
            vertex.color[3] = 0.0;
        }
        unsafe { vbuf.write(0, bytemuck::cast_slice(&vertices)); }
        assert_eq!(
            render(0.0, uv_identity, 0.0, 1.0, 0.0),
            [1.0, 1.0],
            "vertex alpha zero must pass both shadow rays even with an opaque map"
        );
    }

    #[test]
    fn render_scene_vertex_color_interpolation_and_albedo_product() {
        let device = crate::test_device();
        let vertices = [
            MeshVertex {
                position: [-1.0, -1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
                uv: [0.0, 0.0],
                _pad2: [0.0, 0.0],
                tangent: [0.0; 4],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            MeshVertex {
                position: [3.0, -1.0, 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
                uv: [0.0, 0.0],
                _pad2: [0.0, 0.0],
                tangent: [0.0; 4],
                color: [0.0, 1.0, 0.0, 1.0],
            },
            MeshVertex {
                position: [-1.0, 3.0, 0.0],
                _pad0: 0.0,
                normal: [0.0, 0.0, 1.0],
                _pad1: 0.0,
                uv: [0.0, 0.0],
                _pad2: [0.0, 0.0],
                tangent: [0.0; 4],
                color: [0.0, 0.0, 1.0, 1.0],
            },
        ];
        let vertex_buffer = device.create_buffer_shared(std::mem::size_of_val(&vertices) as u64);
        unsafe { vertex_buffer.write(0, bytemuck::cast_slice(&vertices)); }
        let instance = InstanceTransform { pos_scale: [0.0, 0.0, 0.0, 1.0], rot_pad: [0.0; 4] };
        let instance_buffer = device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
        unsafe { instance_buffer.write(0, bytemuck::bytes_of(&instance)); }
        let weights_buffer = device.create_buffer_shared(4);
        unsafe { weights_buffer.write(0, bytemuck::bytes_of(&1.0f32)); }
        let base_map = upload_rgba16f(&device, 1, 1, &[[0.5, 1.0, 0.25, 1.0]], "vertex-color-base-map");
        let base_sampler = device.create_sampler(&Default::default());
        let mut uniforms = RenderSceneUniforms::zeroed();
        uniforms.view_proj = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        uniforms.model = uniforms.view_proj;
        uniforms.base_color = [0.8, 0.6, 0.4, 1.0];
        uniforms.texture_flags[2] = 1.0;
        uniforms.base_color_uv_m = [1.0, 0.0, 0.0, 1.0];
        let output = rgba16f_target(&device, 1, 1, "vertex-color-output");
        const PROBE: &str = r#"
@fragment
fn fs_vertex_color_probe(input: VsOut) -> @location(0) vec4<f32> {
    return resolve_albedo(input.uv, input.vertex_color);
}
"#;
        let shader = format!("{}\n{}", include_str!("../shaders/render_scene.wgsl"), PROBE);
        let pipeline = device.create_render_pipeline(
            &shader,
            "vs_main",
            "fs_vertex_color_probe",
            GpuTextureFormat::Rgba16Float,
            None,
            "vertex-color-albedo-proof",
        );
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
            GpuBinding::Buffer { binding: 15, buffer: &instance_buffer, offset: 0 },
            GpuBinding::Buffer { binding: 46, buffer: &weights_buffer, offset: 0 },
            GpuBinding::Texture { binding: 6, texture: &base_map },
            GpuBinding::Sampler { binding: 22, sampler: &base_sampler },
        ];
        let mut encoder = device.create_encoder("vertex-color-albedo-proof");
        encoder.draw_instanced(
            &pipeline,
            &output,
            &bindings,
            3,
            1,
            manifold_gpu::GpuLoadAction::Clear,
            "vertex-color-albedo-proof-draw",
        );
        encoder.commit_and_wait_completed();
        let actual = readback_rgba16f(&device, &output, 1, 1)[0];
        let barycentric = [0.5f32, 0.25, 0.25];
        let vertex_color = [
            barycentric[0],
            barycentric[1],
            barycentric[2],
            1.0,
        ];
        let expected = [
            uniforms.base_color[0] * 0.5 * vertex_color[0],
            uniforms.base_color[1] * 1.0 * vertex_color[1],
            uniforms.base_color[2] * 0.25 * vertex_color[2],
            1.0,
        ];
        for channel in 0..4 {
            assert!((actual[channel] - expected[channel]).abs() < 0.02,
                "vertex color/albedo channel {channel}: expected {}, got {}",
                expected[channel], actual[channel]);
        }
    }

    fn test_mesh_vertex(position: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 0.0,
            normal: [0.0, 0.0, 1.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
            color: [1.0; 4],
        }
    }

    /// The extension-map sampler uses the production WGSL helper rather than
    /// a test-side copy. Each output pixel selects a distinct metadata row,
    /// covering UV0/UV1 selection, affine flipping, clamp/repeat/mirror
    /// addressing, and nearest versus linear filtering.
    #[test]
    fn extension_map_sampling_metadata_matches_production_helper() {
        use bytemuck::Zeroable;
        let device = crate::test_device();
        let pixels = [
            [0.1, 0.1, 0.0, 1.0],
            [0.2, 0.1, 0.0, 1.0],
            [0.3, 0.1, 0.0, 1.0],
            [0.4, 0.1, 0.0, 1.0],
            [0.1, 0.9, 0.0, 1.0],
            [0.2, 0.9, 0.0, 1.0],
            [0.3, 0.9, 0.0, 1.0],
            [0.4, 0.9, 0.0, 1.0],
        ];
        let map = upload_rgba16f(&device, 4, 2, &pixels, "extension-map-proof");

        let sampler = |wrap_u, wrap_v, mag_filter, min_filter, mip_filter| {
            crate::node_graph::material::MapSamplerDesc {
                wrap_u,
                wrap_v,
                mag_filter,
                min_filter,
                mip_filter,
            }
        };
        let info = |uv_transform, tex_coord, sampler| {
            crate::node_graph::material::MaterialMapInfo { uv_transform, tex_coord, sampler }
        };
        let nearest = manifold_gpu::GpuFilterMode::Nearest;
        let linear = manifold_gpu::GpuFilterMode::Linear;
        let clamp = manifold_gpu::GpuAddressMode::ClampToEdge;
        let repeat = manifold_gpu::GpuAddressMode::Repeat;
        let mirror = manifold_gpu::GpuAddressMode::MirrorRepeat;
        let mut maps = [MaterialMapUniform::zeroed(); 19];
        maps[5] = MaterialMapUniform::from(info(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            sampler(clamp, clamp, nearest, nearest, None),
        ));
        maps[6] = MaterialMapUniform::from(info(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            1,
            sampler(clamp, clamp, nearest, nearest, None),
        ));
        maps[7] = MaterialMapUniform::from(info(
            [-1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
            0,
            sampler(clamp, clamp, nearest, nearest, None),
        ));
        maps[8] = MaterialMapUniform::from(info(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            sampler(repeat, repeat, nearest, nearest, None),
        ));
        maps[9] = MaterialMapUniform::from(info(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            sampler(mirror, mirror, nearest, nearest, None),
        ));
        maps[10] = MaterialMapUniform::from(info(
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            0,
            sampler(clamp, clamp, linear, linear, None),
        ));

        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut uniforms = RenderSceneUniforms::zeroed();
        uniforms.view_proj = identity;
        uniforms.model = identity;
        uniforms.appearance = [1.0, 0.0, 0.0, 0.0];
        let instance = InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0, 0.0, 0.0, 0.0],
        };
        let instances = device.create_buffer_shared(std::mem::size_of::<InstanceTransform>() as u64);
        unsafe { instances.write(0, bytemuck::bytes_of(&instance)); }
        let vertices = [
            test_mesh_vertex([-1.0, -1.0, 0.0]),
            test_mesh_vertex([1.0, -1.0, 0.0]),
            test_mesh_vertex([1.0, 1.0, 0.0]),
            test_mesh_vertex([-1.0, -1.0, 0.0]),
            test_mesh_vertex([1.0, 1.0, 0.0]),
            test_mesh_vertex([-1.0, 1.0, 0.0]),
        ];
        let vertex_buffer = device.create_buffer_shared(std::mem::size_of_val(&vertices) as u64);
        unsafe { vertex_buffer.write(0, bytemuck::cast_slice(&vertices)); }
        let output = device.create_texture(&GpuTextureDesc {
            width: 6, height: 1, depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::COPY_SRC,
            label: "extension-map-proof-output", mip_levels: 1,
        });
        const PROBE: &str = r#"
@fragment
fn fs_extension_map_probe(in: VsOut) -> @location(0) vec4<f32> {
    let pixel = u32(floor(in.clip_pos.x));
    var index = 5u;
    var probe_uv = vec4<f32>(0.25, 0.25, 0.75, 0.25);
    if pixel == 1u { index = 6u; }
    if pixel == 2u { index = 7u; probe_uv.x = 0.75; }
    if pixel == 3u { index = 8u; probe_uv.x = 1.25; }
    if pixel == 4u { index = 9u; probe_uv.x = -0.25; }
    if pixel == 5u { index = 10u; probe_uv = vec4<f32>(0.5); }
    return sample_extension_map(sheen_color_map, probe_uv, index);
}
"#;
        let shader = format!("{}\n{}", include_str!("../shaders/render_scene.wgsl"), PROBE);
        let pipeline = device.create_render_pipeline(
            &shader,
            "vs_main",
            "fs_extension_map_probe",
            GpuTextureFormat::Rgba16Float,
            None,
            "extension-map-proof-pipeline",
        );
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &vertex_buffer, offset: 0 },
            GpuBinding::Buffer { binding: 15, buffer: &instances, offset: 0 },
            GpuBinding::Buffer { binding: 46, buffer: &vertex_buffer, offset: 0 },
            GpuBinding::Texture { binding: 29, texture: &map },
            GpuBinding::Bytes { binding: 50, data: bytemuck::cast_slice(&maps) },
        ];
        let mut enc = device.create_encoder("extension-map-proof");
        enc.draw_instanced(
            &pipeline,
            &output,
            &bindings,
            6,
            1,
            manifold_gpu::GpuLoadAction::Clear,
            "extension-map-proof-draw",
        );
        enc.commit_and_wait_completed();
        let actual = readback_rgba16f(&device, &output, 6, 1);
        let expected = [
            [0.2, 0.1, 0.0, 1.0],
            [0.4, 0.1, 0.0, 1.0],
            [0.2, 0.1, 0.0, 1.0],
            [0.2, 0.1, 0.0, 1.0],
            [0.1, 0.1, 0.0, 1.0],
            [0.25, 0.5, 0.0, 1.0],
        ];
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            for channel in 0..4 {
                assert!((actual[channel] - expected[channel]).abs() < 0.02,
                    "extension map probe pixel {index} channel {channel}: expected {}, got {}",
                    expected[channel], actual[channel]);
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
                    scene.pipeline_for(&device, kind, emit_velocity, emit_ao_mask, emit_denoise_feed, blend, false);
                    assert_eq!(
                        device.render_pipeline_cache_len(),
                        cache_before_use,
                        "pipeline_for({kind:?}, velocity={emit_velocity}, ao_mask={emit_ao_mask}, denoise={emit_denoise_feed}, blend={blend}) after prewarm must be a cache hit, not compile a new pipeline"
                    );
                }
            }
        }

        // D8 (P3): the points dimension is pipeline_for-reachable, so it is
        // prewarm-reachable too — after prewarm it must be a cache hit.
        scene.pipeline_for(&device, MaterialKind::Unlit, false, false, false, false, true);
        assert_eq!(
            device.render_pipeline_cache_len(),
            cache_before_use,
            "pipeline_for(points) after prewarm must be a cache hit, not compile a new pipeline"
        );
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

    #[test]
    fn extension_map_mip_filters_follow_authored_choice() {
        use bytemuck::Zeroable;
        let device = crate::test_device();
        let map = device.create_texture(&GpuTextureDesc {
            width: 4, height: 4, depth: 1, format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "material-mip-checker", mip_levels: 3,
        });
        let data: Vec<u8> = (0..16).flat_map(|i| {
            let c = if (i % 4 + i / 4) % 2 == 0 { 0 } else { 255 };
            [c, c, c, 255]
        }).collect();
        device.upload_texture(&map, &data);
        let mut encoder = device.create_encoder("material-mips");
        encoder.generate_mipmaps(&map);
        encoder.commit_and_wait_completed();
        // Each 2x2 block averages to 0.5 in mip 1. The first mip is black
        // at (1/8,1/8). A 2^0.6 texel footprint independently predicts
        // no mip = 0, nearest mip = 0.5, linear mip = 0.3.
        let shader = format!("{}\n{}", include_str!("../shaders/render_scene.wgsl"), r#"
@vertex fn vs_mip_probe(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs_mip_probe(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = vec2<f32>(0.125) + (p.xy - vec2<f32>(0.5)) * (exp2(0.6) / 4.0);
    return sample_extension_map(sheen_color_map, vec4<f32>(uv, uv), 5u);
}
"#);
        let pipeline = device.create_render_pipeline(&shader, "vs_mip_probe", "fs_mip_probe",
            GpuTextureFormat::Rgba16Float, None, "material-mip-proof");
        let output = device.create_texture(&GpuTextureDesc {
            width: 2, height: 2, depth: 1, format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::COPY_SRC,
            label: "material-mip-output", mip_levels: 1,
        });
        for (filter, expected) in [(None, 0.0),
            (Some(manifold_gpu::GpuFilterMode::Nearest), 0.5),
            (Some(manifold_gpu::GpuFilterMode::Linear), 0.3)] {
            let mut maps = [MaterialMapUniform::zeroed(); 19];
            maps[5] = MaterialMapUniform::from(crate::node_graph::material::MaterialMapInfo {
                sampler: crate::node_graph::material::MapSamplerDesc {
                    min_filter: manifold_gpu::GpuFilterMode::Nearest,
                    mag_filter: manifold_gpu::GpuFilterMode::Nearest,
                    mip_filter: filter,
                    ..Default::default()
                },
                ..Default::default()
            });
            let bindings = [
                GpuBinding::Texture { binding: 29, texture: &map },
                GpuBinding::Bytes { binding: 50, data: bytemuck::cast_slice(&maps) },
            ];
            let mut enc = device.create_encoder("material-mip-proof");
            enc.draw_instanced(&pipeline, &output, &bindings, 3, 1,
                manifold_gpu::GpuLoadAction::Clear, "material-mip-proof");
            enc.commit_and_wait_completed();
            let actual = readback_rgba16f(&device, &output, 2, 2)[0][0];
            assert!((actual - expected).abs() < 0.005,
                "mip filter {filter:?}: expected {expected}, got {actual}");
        }
    }

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
            ..Default::default()
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
        let assert_history_pair = |pair: &[Option<manifold_gpu::GpuTexture>; 2], format| {
            let textures: Vec<_> = pair.iter().flatten().collect();
            assert_eq!(textures.len(), 2);
            assert!(textures.iter().all(|texture| texture.format == format));
            assert_ne!(textures[0].identity_key(), textures[1].identity_key());
        };
        use GpuTextureFormat::*;
        assert_history_pair(&s.rt_irr_history, Rgba16Float);
        assert_history_pair(&s.rt_refl_history, Rgba16Float);
        assert_history_pair(&s.rt_sv_history, Rgba16Float);
        assert_history_pair(&s.rt_sv_m1_history, Rgba16Float);
        assert_history_pair(&s.rt_sv_m2_history, Rgba16Float);
        assert_history_pair(&s.rt_sv_hold_history, R16Float);
        assert_history_pair(&s.rt_sv2_history, Rgba16Float);
        assert_history_pair(&s.rt_sv2_m1_history, Rgba16Float);
        assert_history_pair(&s.rt_sv2_m2_history, Rgba16Float);
        assert_history_pair(&s.rt_sv2_hold_history, R16Float);
        assert_history_pair(&s.rt_svt_history, Rgba16Float);
        assert_history_pair(&s.rt_depth_history, R32Float);
        assert_history_pair(&s.rt_normal_history, Rgba16Float);
        assert_history_pair(&s.rt_moments_history, Rgba32Float);
        let original_hold_textures: Vec<_> = s
            .rt_sv_hold_history
            .iter()
            .chain(s.rt_sv2_hold_history.iter())
            .filter_map(Option::as_ref)
            .cloned()
            .collect();
        let original_hold_ids: Vec<_> = original_hold_textures
            .iter()
            .map(manifold_gpu::GpuTexture::identity_key)
            .collect();
        s.ensure_rt_masks(&device, 128, 128, 256, 256);
        // Same dims: no realloc, no reset.
        assert!(!s.ensure_rt_irradiance(&device, 128, 128, 256, 256));
        let same_size_hold_ids: Vec<_> = s
            .rt_sv_hold_history
            .iter()
            .chain(s.rt_sv2_hold_history.iter())
            .filter_map(Option::as_ref)
            .map(manifold_gpu::GpuTexture::identity_key)
            .collect();
        assert_eq!(same_size_hold_ids, original_hold_ids, "same-size ensure must preserve R16 histories");
        // Tier flip (Half → Native at fixed canvas): trace dims change.
        assert!(s.ensure_rt_irradiance(&device, 256, 256, 256, 256));
        assert_eq!(s.rt_irr_trace_w, 256);
        // The truncation hole: canvas 256 → 257 at Quarter (trace 64 → 64).
        // Full-class textures (history included) must still realloc.
        assert!(s.ensure_rt_irradiance(&device, 64, 64, 256, 256));
        assert!(s.ensure_rt_irradiance(&device, 64, 64, 257, 257));
        assert_eq!(s.rt_irr_width, 257);
        assert_history_pair(&s.rt_sv_hold_history, R16Float);
        assert_history_pair(&s.rt_sv2_hold_history, R16Float);
        for texture in s.rt_sv_hold_history.iter().flatten().chain(s.rt_sv2_hold_history.iter().flatten()) {
            assert_eq!((texture.width, texture.height), (257, 257));
        }
        let resized_hold_ids: Vec<_> = s
            .rt_sv_hold_history
            .iter()
            .chain(s.rt_sv2_hold_history.iter())
            .filter_map(Option::as_ref)
            .map(manifold_gpu::GpuTexture::identity_key)
            .collect();
        assert!(resized_hold_ids.iter().all(|id| !original_hold_ids.contains(id)), "resize must replace R16 histories");
        // R16Float has no compute clear pipeline; its render-target usage must
        // keep the render-pass clear fallback usable for the reset sentinel.
        let hold = s.rt_sv_hold_history[0].as_ref().expect("R16 hold history");
        let bytes_per_row = 257 * 2;
        let readback = device.create_buffer_shared(u64::from(257 * bytes_per_row));
        let mut enc = device.create_encoder("rt-r16-history-clear-read");
        enc.clear_texture(hold, 0.25, 0.0, 0.0, 0.0);
        enc.copy_texture_to_buffer(hold, &readback, 257, 257, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("R16 history readback");
        let values: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast(), (257 * 257) as usize) };
        assert!((f16::from_bits(values[0]).to_f32() - 0.25).abs() < 0.01);
        // Masks guard follows the same dual-pair discipline.
        s.ensure_rt_masks(&device, 64, 64, 256, 256);
        assert_eq!(s.rt_mask_trace_w, 64);
        assert_eq!(s.rt_mask_width, 256);
    }

    /// RT-Stage-3 P4 scratch proof: the post-accumulation ping-pong pair may
    /// reuse the completed pre-accumulation scratch handles. Run the actual
    /// Metal `atrous_post` kernel against nonuniform inputs for odd and even
    /// iteration counts across queued command buffers, then compare every
    /// frame's final pixels with a dedicated backing pair. Temporal histories,
    /// raw irradiance, and the current-frame guides must remain distinct, and
    /// a resize must refresh both aliases.
    #[test]
    fn rt_post_scratch_shared_backing_matches_dedicated_across_queued_frames() {
        use manifold_gpu::raytrace::{
            AtrousParams, AtrousPostParams, MetalShadowRayTracer, ShadowRayTracer,
        };

        const W: u32 = 9;
        const H: u32 = 9;
        const FRAME_COUNT: usize = 4;
        const POST_FRAME_COUNT: usize = FRAME_COUNT - 1;
        let pixel_count = (W * H) as usize;
        let device = crate::test_device();
        let mut scene = RenderScene::new();
        assert!(scene.ensure_rt_irradiance(&device, W, H, W, H));

        let shared_a = scene.rt_irr_filtered.as_ref().expect("filtered alias").clone();
        let shared_b = scene.rt_irr_filtered_b.as_ref().expect("filtered alias b").clone();
        assert!(shared_a.ptr_eq(scene.rt_irr_full_b.as_ref().expect("irr scratch")));
        assert!(shared_b.ptr_eq(scene.rt_normal_full_b.as_ref().expect("normal scratch")));
        assert!(!shared_a.ptr_eq(&shared_b));
        assert_eq!(shared_a.format, GpuTextureFormat::Rgba16Float);
        assert_eq!((shared_a.width, shared_a.height), (W, H));

        let history_ids: Vec<_> = scene
            .rt_irr_history
            .iter()
            .flatten()
            .map(manifold_gpu::GpuTexture::identity_key)
            .collect();
        assert_eq!(history_ids.len(), 2);
        assert_ne!(history_ids[0], history_ids[1]);
        assert!(history_ids.iter().all(|id| *id != shared_a.identity_key() && *id != shared_b.identity_key()));
        assert_ne!(scene.rt_irr_full.as_ref().expect("raw irradiance").identity_key(), shared_a.identity_key());
        assert_ne!(scene.rt_normal_full.as_ref().expect("normal guide").identity_key(), shared_b.identity_key());
        assert!(!scene.rt_moments_history.iter().flatten().any(|t| t.ptr_eq(&shared_a) || t.ptr_eq(&shared_b)));

        let depth = upload_r32f(
            &device,
            W,
            H,
            &(0..pixel_count)
                .map(|i| 0.2 + (i % W as usize) as f32 * 0.01 + (i / W as usize) as f32 * 0.02)
                .collect::<Vec<_>>(),
            "rt-post-scratch-depth",
        );
        let normal = upload_rgba16f(
            &device,
            W,
            H,
            &(0..pixel_count)
                .map(|i| {
                    let x = (i % W as usize) as f32 / W as f32;
                    let y = (i / W as usize) as f32 / H as f32;
                    [0.1 * x, 0.1 * y, 0.98, 1.0]
                })
                .collect::<Vec<_>>(),
            "rt-post-scratch-normal",
        );
        let moments = upload_rgba32f(
            &device,
            W,
            H,
            &(0..pixel_count)
                .map(|i| {
                    let mean = 0.15 + (i % W as usize) as f32 * 0.02;
                    [mean, mean * mean + 0.02, 0.0, 0.0]
                })
                .collect::<Vec<_>>(),
            "rt-post-scratch-moments",
        );
        let sources: Vec<_> = (0..FRAME_COUNT)
            .map(|frame| {
                upload_rgba16f(
                    &device,
                    W,
                    H,
                    &(0..pixel_count)
                        .map(|i| {
                            let x = (i % W as usize) as f32;
                            let y = (i / W as usize) as f32;
                            let gain = 0.25 + frame as f32 * 0.17;
                            [
                                gain + x * 0.03 + y * 0.01,
                                gain * 0.7 + y * 0.02,
                                0.1 + (x + y) * 0.015,
                                1.0,
                            ]
                        })
                        .collect::<Vec<_>>(),
                    "rt-post-scratch-history",
                )
            })
            .collect();

        let shared_raw_irr = scene.rt_irr_full.as_ref().expect("raw irradiance").clone();
        let shared_guide_n = scene.rt_normal_full.as_ref().expect("normal guide").clone();
        let dedicated_raw_irr = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-raw");
        let dedicated_guide_n = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-guide");
        let dedicated_a = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-a");
        let dedicated_b = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-b");
        let dedicated_pre_irr = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-pre-irr");
        let dedicated_pre_n = rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-pre-n");
        let params_buffer = device.create_buffer_shared(std::mem::size_of::<AtrousPostParams>() as u64);
        let atrous_params_buffer = device.create_buffer_shared(std::mem::size_of::<AtrousParams>() as u64);
        let gi_materials = device.create_buffer_shared(std::mem::size_of::<manifold_gpu::raytrace::GiMaterial>() as u64);
        gi_materials.zero_fill();
        let tracer = MetalShadowRayTracer::new(&device);
        let shared_aux_a: [manifold_gpu::GpuTexture; 4] =
            std::array::from_fn(|_| rgba16f_target(&device, W, H, "rt-post-scratch-shared-pre-a"));
        let shared_aux_b: [manifold_gpu::GpuTexture; 4] =
            std::array::from_fn(|_| rgba16f_target(&device, W, H, "rt-post-scratch-shared-pre-b"));
        let dedicated_aux_a: [manifold_gpu::GpuTexture; 4] =
            std::array::from_fn(|_| rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-pre-a"));
        let dedicated_aux_b: [manifold_gpu::GpuTexture; 4] =
            std::array::from_fn(|_| rgba16f_target(&device, W, H, "rt-post-scratch-dedicated-pre-b"));
        let encode_prefilter =
            |encoder: &mut manifold_gpu::GpuEncoder,
             src_irr: &manifold_gpu::GpuTexture,
             dst_irr: &manifold_gpu::GpuTexture,
             src_n: &manifold_gpu::GpuTexture,
             dst_n: &manifold_gpu::GpuTexture,
             src_aux: [&manifold_gpu::GpuTexture; 4],
             dst_aux: [&manifold_gpu::GpuTexture; 4],
             label: &str| {
                let params = AtrousParams::new([W, H], 2, true, 0);
                tracer.atrous_pass(
                    encoder,
                    &params,
                    &atrous_params_buffer,
                    &gi_materials,
                    &depth,
                    &moments,
                    src_aux[0],
                    dst_aux[0],
                    src_aux[1],
                    dst_aux[1],
                    src_irr,
                    dst_irr,
                    src_n,
                    dst_n,
                    src_aux[2],
                    dst_aux[2],
                    src_aux[3],
                    dst_aux[3],
                    label,
                );
            };
        let bytes_per_row = W * 8;
        let readback_size = u64::from(bytes_per_row * H);
        let shared_readbacks: Vec<_> = (0..POST_FRAME_COUNT)
            .map(|_| device.create_buffer_shared(readback_size))
            .collect();
        let dedicated_readbacks: Vec<_> = (0..POST_FRAME_COUNT)
            .map(|_| device.create_buffer_shared(readback_size))
            .collect();
        let shared_raw_readback = device.create_buffer_shared(readback_size);
        let dedicated_raw_readback = device.create_buffer_shared(readback_size);
        let shared_guide_readback = device.create_buffer_shared(readback_size);
        let dedicated_guide_readback = device.create_buffer_shared(readback_size);

        let mut encoder = device.create_encoder("rt-post-scratch-parity");
        for (frame, source) in sources.iter().enumerate() {
            let iterations = [0u32, 1, 2, 3][frame]; // Off, odd, even, odd
            // The two pre-accumulation passes use the shared aliases as their
            // first-pass irradiance/normal destinations, exactly as production
            // does before the post-accumulation block reuses those handles.
            encode_prefilter(
                &mut encoder,
                source,
                &shared_a,
                &normal,
                &shared_b,
                [source, source, source, source],
                [&shared_aux_a[0], &shared_aux_a[1], &shared_aux_a[2], &shared_aux_a[3]],
                "rt-post-scratch-shared-prefilter-0",
            );
            encode_prefilter(
                &mut encoder,
                &shared_a,
                &shared_raw_irr,
                &shared_b,
                &shared_guide_n,
                [&shared_aux_a[0], &shared_aux_a[1], &shared_aux_a[2], &shared_aux_a[3]],
                [&shared_aux_b[0], &shared_aux_b[1], &shared_aux_b[2], &shared_aux_b[3]],
                "rt-post-scratch-shared-prefilter-1",
            );
            encode_prefilter(
                &mut encoder,
                source,
                &dedicated_pre_irr,
                &normal,
                &dedicated_pre_n,
                [source, source, source, source],
                [&dedicated_aux_a[0], &dedicated_aux_a[1], &dedicated_aux_a[2], &dedicated_aux_a[3]],
                "rt-post-scratch-dedicated-prefilter-0",
            );
            encode_prefilter(
                &mut encoder,
                &dedicated_pre_irr,
                &dedicated_raw_irr,
                &dedicated_pre_n,
                &dedicated_guide_n,
                [&dedicated_aux_a[0], &dedicated_aux_a[1], &dedicated_aux_a[2], &dedicated_aux_a[3]],
                [&dedicated_aux_b[0], &dedicated_aux_b[1], &dedicated_aux_b[2], &dedicated_aux_b[3]],
                "rt-post-scratch-dedicated-prefilter-1",
            );
            if iterations == 0 {
                // Off tier: prefiltering still ran, but post filtering is
                // intentionally skipped. Queue the next frame separately.
                encoder.commit_and_continue(&device);
                continue;
            }
            let mut shared_src = source;
            let mut dedicated_src = source;
            for pass in 0..iterations {
                let params = AtrousPostParams::new([W, H], 1u32 << pass, 0.85);
                let write_to_a = (iterations - 1 - pass).is_multiple_of(2);
                let shared_dst = if write_to_a { &shared_a } else { &shared_b };
                let dedicated_dst = if write_to_a { &dedicated_a } else { &dedicated_b };
                tracer.atrous_post_pass(
                    &mut encoder,
                    &params,
                    &params_buffer,
                    &depth,
                    &shared_guide_n,
                    &moments,
                    shared_src,
                    shared_dst,
                    "rt-post-scratch-shared",
                );
                tracer.atrous_post_pass(
                    &mut encoder,
                    &params,
                    &params_buffer,
                    &depth,
                    &dedicated_guide_n,
                    &moments,
                    dedicated_src,
                    dedicated_dst,
                    "rt-post-scratch-dedicated",
                );
                shared_src = shared_dst;
                dedicated_src = dedicated_dst;
            }
            let post_frame = frame - 1;
            // Snapshot each queued frame before the next command buffer can
            // overwrite the ping-pong pair.
            encoder.copy_texture_to_buffer(
                &shared_a,
                &shared_readbacks[post_frame],
                W,
                H,
                bytes_per_row,
            );
            encoder.copy_texture_to_buffer(
                &dedicated_a,
                &dedicated_readbacks[post_frame],
                W,
                H,
                bytes_per_row,
            );
            if frame + 1 != FRAME_COUNT {
                encoder.commit_and_continue(&device);
            }
        }
        // The raw irradiance and current-frame normal guide are distinct from
        // both reused post targets; snapshot them after the final post pass.
        encoder.copy_texture_to_buffer(
            &shared_raw_irr,
            &shared_raw_readback,
            W,
            H,
            bytes_per_row,
        );
        encoder.copy_texture_to_buffer(
            &dedicated_raw_irr,
            &dedicated_raw_readback,
            W,
            H,
            bytes_per_row,
        );
        encoder.copy_texture_to_buffer(
            &shared_guide_n,
            &shared_guide_readback,
            W,
            H,
            bytes_per_row,
        );
        encoder.copy_texture_to_buffer(
            &dedicated_guide_n,
            &dedicated_guide_readback,
            W,
            H,
            bytes_per_row,
        );
        encoder.try_commit_and_wait_completed().expect("RT scratch parity GPU completion");

        let assert_exact_finite = |label: &str, shared: &[[f32; 4]], dedicated: &[[f32; 4]]| {
            assert_eq!(shared.len(), dedicated.len(), "{label} length mismatch");
            for (pixel, (a, b)) in shared.iter().zip(dedicated).enumerate() {
                for channel in 0..4 {
                    assert!(a[channel].is_finite() && b[channel].is_finite(), "{label} non-finite at {pixel}/{channel}");
                    assert_eq!(a[channel].to_bits(), b[channel].to_bits(), "{label} mismatch at {pixel}/{channel}");
                }
            }
        };
        let assert_nonuniform = |label: &str, pixels: &[[f32; 4]]| {
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            let mut nonzero = false;
            for pixel in pixels {
                for channel in pixel {
                    assert!(channel.is_finite(), "{label} non-finite");
                    min = min.min(*channel);
                    max = max.max(*channel);
                    nonzero |= *channel != 0.0;
                }
            }
            assert!(nonzero, "{label} is all zero");
            assert!(max > min, "{label} is uniform");
            assert!(pixels.iter().any(|p| p[..3].iter().any(|v| *v != 0.0)), "{label} has no RGB signal");
            assert!(pixels.windows(2).any(|pair| pair[0][..3] != pair[1][..3]), "{label} has no spatial variation");
        };
        for frame in 0..POST_FRAME_COUNT {
            let shared_pixels = decode_rgba16f_readback(&shared_readbacks[frame], pixel_count);
            let dedicated_pixels = decode_rgba16f_readback(&dedicated_readbacks[frame], pixel_count);
            assert_exact_finite("post pixels", &shared_pixels, &dedicated_pixels);
            assert_nonuniform("post pixels", &shared_pixels);
        }
        let shared_raw = decode_rgba16f_readback(&shared_raw_readback, pixel_count);
        let dedicated_raw = decode_rgba16f_readback(&dedicated_raw_readback, pixel_count);
        assert_exact_finite("raw irradiance", &shared_raw, &dedicated_raw);
        assert_nonuniform("raw irradiance", &shared_raw);
        let shared_guide = decode_rgba16f_readback(&shared_guide_readback, pixel_count);
        let dedicated_guide = decode_rgba16f_readback(&dedicated_guide_readback, pixel_count);
        assert_exact_finite("normal guide", &shared_guide, &dedicated_guide);
        assert_nonuniform("normal guide", &shared_guide);

        // A dimension change creates fresh scratch objects and refreshes both
        // post-filter aliases; no stale handle survives the resize.
        assert!(scene.ensure_rt_irradiance(&device, W, H, W + 1, H + 1));
        let resized_a = scene.rt_irr_filtered.as_ref().expect("resized filtered alias");
        let resized_b = scene.rt_irr_filtered_b.as_ref().expect("resized filtered alias b");
        assert_eq!((resized_a.width, resized_a.height), (W + 1, H + 1));
        assert!(resized_a.ptr_eq(scene.rt_irr_full_b.as_ref().expect("resized irr scratch")));
        assert!(resized_b.ptr_eq(scene.rt_normal_full_b.as_ref().expect("resized normal scratch")));
        assert!(!resized_a.ptr_eq(&shared_a));
        assert!(!resized_b.ptr_eq(&shared_b));
    }

    /// The reflection prefilter scratch is also the later scene-color resolve
    /// target.  Queue the real atrous kernel, a real 4x MSAA resolve, and the
    /// real firefly kernel across several command buffers, comparing that
    /// shared path with a dedicated scratch control at every frame.
    #[test]
    fn rt_firefly_scratch_shared_backing_matches_dedicated_across_queued_frames() {
        use manifold_gpu::raytrace::{AtrousParams, MetalShadowRayTracer, ShadowRayTracer};

        const W: u32 = 9;
        const H: u32 = 9;
        const FRAME_COUNT: usize = 4;
        let pixel_count = (W * H) as usize;
        let device = crate::test_device();
        let mut scene = RenderScene::new();
        assert!(scene.ensure_rt_irradiance(&device, W, H, W, H));
        assert!(!scene.ensure_rt_irradiance(&device, W, H, W, H));

        let shared_scratch = scene.rt_refl_full_b.as_ref().expect("reflection scratch").clone();
        let firefly_scratch = scene.rt_firefly_scratch.as_ref().expect("firefly alias").clone();
        assert!(shared_scratch.ptr_eq(&firefly_scratch));
        assert_eq!(shared_scratch.format, GpuTextureFormat::Rgba16Float);
        assert_eq!((shared_scratch.width, shared_scratch.height), (W, H));
        let raw_reflection = scene.rt_refl_full.as_ref().expect("raw reflection");
        assert_ne!(raw_reflection.identity_key(), shared_scratch.identity_key());
        for history in scene.rt_refl_history.iter().flatten() {
            assert_ne!(history.identity_key(), shared_scratch.identity_key());
            assert_ne!(history.identity_key(), raw_reflection.identity_key());
        }
        assert_ne!(scene.rt_refl_history[0].as_ref().unwrap().identity_key(), scene.rt_refl_history[1].as_ref().unwrap().identity_key());
        for post_scratch in [
            scene.rt_irr_full_b.as_ref().expect("irradiance post scratch"),
            scene.rt_normal_full_b.as_ref().expect("normal post scratch"),
        ] {
            assert_ne!(post_scratch.identity_key(), shared_scratch.identity_key());
            assert_ne!(post_scratch.identity_key(), raw_reflection.identity_key());
        }

        let raw_pixels: Vec<[f32; 4]> = (0..pixel_count)
            .map(|i| {
                let x = (i % W as usize) as f32;
                let y = (i / W as usize) as f32;
                [0.12 + x * 0.025, 0.18 + y * 0.02, 0.1 + (x + y) * 0.01, 1.0]
            })
            .collect();
        let raw_input = upload_rgba16f(&device, W, H, &raw_pixels, "rt-firefly-raw-input");
        let depth = upload_r32f(&device, W, H, &vec![0.5; pixel_count], "rt-firefly-depth");
        let moments = upload_rgba32f(
            &device,
            W,
            H,
            &raw_pixels
                .iter()
                .map(|p| [p[0], p[0] * p[0] + 0.01, 0.0, 0.0])
                .collect::<Vec<_>>(),
            "rt-firefly-moments",
        );
        let normal = upload_rgba16f(
            &device,
            W,
            H,
            &(0..pixel_count)
                .map(|i| {
                    let x = (i % W as usize) as f32 / W as f32;
                    let y = (i / W as usize) as f32 / H as f32;
                    [0.1 * x, 0.1 * y, 0.98, 1.0]
                })
                .collect::<Vec<_>>(),
            "rt-firefly-normal",
        );
        let visibility = upload_rgba16f(
            &device,
            W,
            H,
            &(0..pixel_count)
                .map(|i| {
                    let x = (i % W as usize) as f32;
                    let y = (i / W as usize) as f32;
                    [0.25 + x * 0.01, 0.35 + y * 0.01, 0.5, 1.0]
                })
                .collect::<Vec<_>>(),
            "rt-firefly-visibility",
        );

        let dedicated_prefilter = rgba16f_target(&device, W, H, "rt-firefly-dedicated-prefilter");
        let dedicated_scratch = device.create_texture(&GpuTextureDesc {
            width: W, height: H, depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
            label: "rt-firefly-dedicated-resolve",
            mip_levels: 1,
        });
        let shared_final = rgba16f_target(&device, W, H, "rt-firefly-shared-final");
        let dedicated_final = rgba16f_target(&device, W, H, "rt-firefly-dedicated-final");
        let shared_msaa = device.create_texture_msaa(
            W,
            H,
            GpuTextureFormat::Rgba16Float,
            4,
            "rt-firefly-shared-msaa",
        );
        let dedicated_msaa = device.create_texture_msaa(
            W,
            H,
            GpuTextureFormat::Rgba16Float,
            4,
            "rt-firefly-dedicated-msaa",
        );
        let shared_aux: [manifold_gpu::GpuTexture; 5] = std::array::from_fn(|_| {
            rgba16f_target(&device, W, H, "rt-firefly-shared-atrous")
        });
        let dedicated_aux: [manifold_gpu::GpuTexture; 5] = std::array::from_fn(|_| {
            rgba16f_target(&device, W, H, "rt-firefly-dedicated-atrous")
        });
        let shared_reflection_tail = rgba16f_target(&device, W, H, "rt-firefly-shared-reflection-tail");
        let dedicated_reflection_tail = rgba16f_target(&device, W, H, "rt-firefly-dedicated-reflection-tail");
        let gi_materials = device.create_buffer_shared(std::mem::size_of::<manifold_gpu::raytrace::GiMaterial>() as u64);
        gi_materials.zero_fill();
        let atrous_params_buffer = device.create_buffer_shared(std::mem::size_of::<AtrousParams>() as u64);
        let firefly_params_buffer = device.create_buffer_shared(16);
        let tracer = MetalShadowRayTracer::new(&device);
        let params = AtrousParams::new([W, H], 2, true, 0);
        let firefly_params = manifold_gpu::raytrace::FireflyClampParams::new([W, H], 8.0, 4.0);
        let firefly_stats = tracer.zero_emissive_stats();

        const MSAA_WGSL: &str = r#"
            struct Controls { frame: f32, _pad0: f32, _pad1: f32, _pad2: f32, };
            @group(0) @binding(0) var<uniform> controls: Controls;
            struct VsOut { @builtin(position) position: vec4<f32>, };
            @vertex fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
                var positions = array<vec2<f32>, 3>(
                    vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
                var out: VsOut;
                out.position = vec4<f32>(positions[i], 0.0, 1.0);
                return out;
            }
            @fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
                let p = input.position.xy;
                let base = 0.25 + p.x * 0.025 + p.y * 0.02 + controls.frame * 0.03;
                let low = vec4<f32>(base, base * 0.75, base * 0.5, 0.25 + p.x * 0.025);
                let hot = vec4<f32>(80.0 + controls.frame, 60.0 + controls.frame, 40.0 + controls.frame, 0.625);
                return select(low, hot, distance(p, vec2<f32>(4.5, 4.5)) < 0.75);
            }
        "#;
        let msaa_pipeline = device.create_render_pipeline_msaa(
            MSAA_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba16Float,
            None,
            4,
            "rt-firefly-msaa-pipeline",
        );

        let readback_size = u64::from(W * H * 8);
        let preclamp_shared: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let preclamp_dedicated: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let output_shared: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let output_dedicated: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let reflection_shared: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let reflection_dedicated: Vec<_> = (0..FRAME_COUNT).map(|_| device.create_buffer_shared(readback_size)).collect();
        let raw_readback = device.create_buffer_shared(readback_size);
        let history_readbacks: [manifold_gpu::GpuBuffer; 2] = std::array::from_fn(|_| device.create_buffer_shared(readback_size));
        let mut encoder = device.create_encoder("rt-firefly-scratch-parity");
        let raw_refl = raw_reflection;
        encoder.copy_texture_to_texture(&raw_input, raw_refl, W, H, 1);
        for history in scene.rt_refl_history.iter().flatten() {
            encoder.copy_texture_to_texture(&raw_input, history, W, H, 1);
        }

        for frame in 0..FRAME_COUNT {
            let shared_atrous = [&shared_aux[0], &shared_aux[1], &shared_aux[2], &shared_aux[3], &shared_aux[4]];
            let dedicated_atrous = [&dedicated_aux[0], &dedicated_aux[1], &dedicated_aux[2], &dedicated_aux[3], &dedicated_aux[4]];
            tracer.atrous_pass(
                &mut encoder, &params, &atrous_params_buffer, &gi_materials, &depth, &moments,
                &visibility, shared_atrous[0], &visibility, shared_atrous[1], &raw_input,
                shared_atrous[2], &normal, shared_atrous[3], raw_refl, &shared_scratch,
                &visibility, shared_atrous[4], "rt-firefly-shared-atrous",
            );
            tracer.atrous_pass(
                &mut encoder, &params, &atrous_params_buffer, &gi_materials, &depth, &moments,
                &visibility, dedicated_atrous[0], &visibility, dedicated_atrous[1], &raw_input,
                dedicated_atrous[2], &normal, dedicated_atrous[3], raw_refl, &dedicated_prefilter,
                &visibility, dedicated_atrous[4], "rt-firefly-dedicated-atrous",
            );
            // The second real atrous pass reads the shared scratch before the
            // following MSAA resolve overwrites that same backing.
            tracer.atrous_pass(
                &mut encoder, &params, &atrous_params_buffer, &gi_materials, &depth, &moments,
                &visibility, shared_atrous[0], &visibility, shared_atrous[1], &raw_input,
                shared_atrous[2], &normal, shared_atrous[3], &shared_scratch, &shared_reflection_tail,
                &visibility, shared_atrous[4], "rt-firefly-shared-atrous-readback",
            );
            tracer.atrous_pass(
                &mut encoder, &params, &atrous_params_buffer, &gi_materials, &depth, &moments,
                &visibility, dedicated_atrous[0], &visibility, dedicated_atrous[1], &raw_input,
                dedicated_atrous[2], &normal, dedicated_atrous[3], &dedicated_prefilter, &dedicated_reflection_tail,
                &visibility, dedicated_atrous[4], "rt-firefly-dedicated-atrous-readback",
            );

            // Cross a real submission boundary without a CPU wait before reuse.
            encoder.commit_and_continue(&device);
            let controls = [frame as f32, 0.0, 0.0, 0.0];
            let bindings = [GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&controls) }];
            encoder.draw_instanced_msaa(
                &msaa_pipeline, &shared_msaa, &shared_scratch, &bindings, 3, 1,
                manifold_gpu::GpuLoadAction::Clear, "rt-firefly-shared-msaa-resolve",
            );
            encoder.draw_instanced_msaa(
                &msaa_pipeline, &dedicated_msaa, &dedicated_scratch, &bindings, 3, 1,
                manifold_gpu::GpuLoadAction::Clear, "rt-firefly-dedicated-msaa-resolve",
            );
            // Read the prefilter result after the alias has been overwritten:
            // parity catches a queued prefilter read seeing scene color early.
            encoder.copy_texture_to_buffer(&shared_reflection_tail, &reflection_shared[frame], W, H, W * 8);
            encoder.copy_texture_to_buffer(&dedicated_reflection_tail, &reflection_dedicated[frame], W, H, W * 8);
            encoder.copy_texture_to_buffer(&shared_scratch, &preclamp_shared[frame], W, H, W * 8);
            encoder.copy_texture_to_buffer(&dedicated_scratch, &preclamp_dedicated[frame], W, H, W * 8);
            if frame == 0 {
                encoder.copy_texture_to_buffer(&shared_scratch, &output_shared[frame], W, H, W * 8);
                encoder.copy_texture_to_buffer(&dedicated_scratch, &output_dedicated[frame], W, H, W * 8);
            } else {
                tracer.firefly_clamp(
                    &mut encoder, firefly_stats, &firefly_params, &firefly_params_buffer,
                    &depth, &shared_scratch, &shared_final, "rt-firefly-shared-clamp",
                );
                tracer.firefly_clamp(
                    &mut encoder, firefly_stats, &firefly_params, &firefly_params_buffer,
                    &depth, &dedicated_scratch, &dedicated_final, "rt-firefly-dedicated-clamp",
                );
                encoder.copy_texture_to_buffer(&shared_final, &output_shared[frame], W, H, W * 8);
                encoder.copy_texture_to_buffer(&dedicated_final, &output_dedicated[frame], W, H, W * 8);
            }
            if frame + 1 != FRAME_COUNT {
                encoder.commit_and_continue(&device);
            }
        }
        encoder.copy_texture_to_buffer(raw_refl, &raw_readback, W, H, W * 8);
        for (history, readback) in scene.rt_refl_history.iter().flatten().zip(&history_readbacks) {
            encoder.copy_texture_to_buffer(history, readback, W, H, W * 8);
        }
        encoder.try_commit_and_wait_completed().expect("RT firefly scratch parity GPU completion");

        let assert_exact = |label: &str, a: &[[f32; 4]], b: &[[f32; 4]]| {
            assert_eq!(a.len(), b.len(), "{label} length mismatch");
            for (index, (lhs, rhs)) in a.iter().zip(b).enumerate() {
                for channel in 0..4 {
                    assert!(lhs[channel].is_finite() && rhs[channel].is_finite(), "{label} non-finite at {index}/{channel}");
                    assert_eq!(lhs[channel].to_bits(), rhs[channel].to_bits(), "{label} mismatch at {index}/{channel}");
                }
            }
        };
        let mut saw_clamp = false;
        let center = (H / 2 * W + W / 2) as usize;
        for frame in 0..FRAME_COUNT {
            let shared_pre = decode_rgba16f_readback(&preclamp_shared[frame], pixel_count);
            let dedicated_pre = decode_rgba16f_readback(&preclamp_dedicated[frame], pixel_count);
            let shared_out = decode_rgba16f_readback(&output_shared[frame], pixel_count);
            let dedicated_out = decode_rgba16f_readback(&output_dedicated[frame], pixel_count);
            assert_exact("prefilter parity after reuse",
                &decode_rgba16f_readback(&reflection_shared[frame], pixel_count),
                &decode_rgba16f_readback(&reflection_dedicated[frame], pixel_count));
            assert_exact("pre-clamp parity", &shared_pre, &dedicated_pre);
            assert_exact("firefly output parity", &shared_out, &dedicated_out);
            assert!(shared_out.iter().all(|p| p.iter().all(|v| v.is_finite())));
            assert!(shared_out.iter().any(|p| p[..3].iter().any(|v| *v != 0.0)));
            for (output, input) in shared_out.iter().zip(&shared_pre) {
                assert_eq!(output[3].to_bits(), input[3].to_bits(), "alpha must survive clamp");
            }
            assert_eq!(shared_out[center][3], 0.625, "nonopaque alpha must survive resolve/clamp");
            if frame > 0 {
                assert!(shared_out[center][0] < shared_pre[center][0], "firefly clamp must reduce the hot center");
                saw_clamp = true;
            } else {
                assert_eq!(shared_out[center][0].to_bits(), shared_pre[center][0].to_bits(), "bypass frame must preserve resolve");
            }
        }
        assert!(saw_clamp);
        let expected_raw: Vec<[f32; 4]> = raw_pixels.iter()
            .map(|p| p.map(|v| f16::from_f32(v).to_f32())).collect();
        assert_exact("raw reflection unchanged from input",
            &decode_rgba16f_readback(&raw_readback, pixel_count), &expected_raw);
        assert_exact(
            "raw reflection preserved",
            &decode_rgba16f_readback(&raw_readback, pixel_count),
            &decode_rgba16f_readback(&history_readbacks[0], pixel_count),
        );
        assert_exact(
            "reflection histories preserved",
            &decode_rgba16f_readback(&history_readbacks[0], pixel_count),
            &decode_rgba16f_readback(&history_readbacks[1], pixel_count),
        );

        let old_scratch_id = shared_scratch.identity_key();
        assert!(scene.ensure_rt_irradiance(&device, W + 1, H + 1, W, H));
        let trace_resized = scene.rt_refl_full_b.as_ref().expect("trace-resized scratch").clone();
        assert_ne!(trace_resized.identity_key(), old_scratch_id);
        assert!(trace_resized.ptr_eq(scene.rt_firefly_scratch.as_ref().expect("trace-resized firefly alias")));
        assert!(scene.ensure_rt_irradiance(&device, W + 1, H + 1, W + 1, H + 1));
        let full_resized = scene.rt_refl_full_b.as_ref().expect("full-resized scratch");
        assert_ne!(full_resized.identity_key(), trace_resized.identity_key());
        assert!(full_resized.ptr_eq(scene.rt_firefly_scratch.as_ref().expect("full-resized firefly alias")));
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
