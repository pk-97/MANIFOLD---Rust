//! SCENE_FX P1 — default-passthrough scope check for amount-bearing mesh deformers.
//!
//! D3: "off is free" — with every amount-like scalar at its identity value,
//! the atom's GPU output positions must be identical to its input positions.
//! This lets a rack stack ten modifiers and only the active ones cost a dispatch.
//!
//! We compare positions only, not full vertex bytes: `node.taper_mesh` and
//! `node.morph_mesh` renormalize normals at every amount (including identity),
//! so byte-identical normals is impossible without changing their kernels.
//! Positions are the meaningful "off" signal for a mesh deformer.
//!
//! Coverage: the three P1 atoms plus the shipped July family.
//! Identity values: amount=0 for bend/twist angle, morph t, push amount, and
//! the new voxelize/noise/glitch amount atoms; taper=1 (no taper) because the
//! `taper` param is the far-end scale, where 1 leaves the mesh unchanged.
//!
//! This module is a sibling of the atom modules so it can reach their public
//! structs and the crate-private `test_device`; it constructs raw uniform byte
//! buffers to avoid touching the atoms' private uniform structs.

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use manifold_gpu::{
        GpuBinding, GpuBuffer, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use crate::generators::mesh_common::MeshVertex;
    use crate::node_graph::freeze::codegen::standalone_for_spec;
    use crate::node_graph::primitives::{
        bend_mesh::BendMesh,
        glitch_jitter::GlitchJitter,
        morph_mesh::MorphMesh,
        noise_displace::NoiseDisplace,
        push_along_normals::PushAlongNormals,
        taper_mesh::TaperMesh,
        twist_mesh::TwistMesh,
        voxelize_mesh::VoxelizeMesh,
    };

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
        }
    }

    fn make_vertices() -> Vec<MeshVertex> {
        vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
            mk_vertex([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            mk_vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0]),
            mk_vertex([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0]),
        ]
    }

    fn make_filler_weights(device: &manifold_gpu::GpuDevice, src: &[MeshVertex]) -> GpuBuffer {
        device.create_buffer_shared(std::mem::size_of_val(src) as u64)
    }

    fn make_dummy_field(device: &manifold_gpu::GpuDevice) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width: 1,
            height: 1,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
            label: "scene-fx-passthrough dummy field",
            mip_levels: 1,
        });
        device.upload_texture(&tex, &[255u8, 255, 255, 255]);
        tex
    }

    /// Dispatch a generated standalone buffer-domain kernel. `extra_bindings`
    /// are appended after `buf_in` (binding 1); the caller numbers them and
    /// supplies the output buffer binding index.
    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        uniforms: &[u32],
        extra_bindings: Vec<GpuBinding<'_>>,
        out_binding: u32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "scene-fx-passthrough",
        );
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

        let uniform_bytes = bytemuck::cast_slice(uniforms);
        let mut bindings = vec![
            GpuBinding::Bytes { binding: 0, data: uniform_bytes },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
        ];
        bindings.extend(extra_bindings);
        bindings.push(GpuBinding::Buffer {
            binding: out_binding,
            buffer: &dbuf,
            offset: 0,
        });

        let mut enc = device.create_encoder("scene-fx-passthrough");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "scene-fx-passthrough",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    fn assert_position_identity(type_id: &str, src: &[MeshVertex], out: &[MeshVertex]) {
        assert_eq!(out.len(), src.len(), "{type_id}: output count changed");
        for (i, (s, o)) in src.iter().zip(out.iter()).enumerate() {
            assert_eq!(
                s.position, o.position,
                "{type_id}: vertex {i} position is not identical to input at amount=0/identity"
            );
        }
    }

    fn u(v: f32) -> u32 {
        v.to_bits()
    }

    #[test]
    fn default_passthrough_all_amount_mesh_deformers_are_identity() {
        let device = crate::test_device();
        let src = make_vertices();
        let filler = make_filler_weights(&device, &src);
        let dummy_field = make_dummy_field(&device);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let count = src.len() as u32;

        let mut failures: Vec<String> = Vec::new();

        // bend_mesh: axis(u32)=0, angle=0, center=0, weights_len=0, dispatch_count=N, pad x3.
        {
            let wgsl = standalone_for_spec::<BendMesh>().expect("bend_mesh codegen");
            let uniforms = &[0u32, u(0.0), u(0.0), 0u32, count, 0u32, 0u32, 0u32];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.bend_mesh", &src, &out)
            }))
            .map_err(|_| failures.push("node.bend_mesh".into()));
        }

        // twist_mesh: same layout as bend_mesh.
        {
            let wgsl = standalone_for_spec::<TwistMesh>().expect("twist_mesh codegen");
            let uniforms = &[0u32, u(0.0), u(0.0), 0u32, count, 0u32, 0u32, 0u32];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.twist_mesh", &src, &out)
            }))
            .map_err(|_| failures.push("node.twist_mesh".into()));
        }

        // taper_mesh: axis=0, taper=1 (identity), center=0, taper_length=1, weights_len=0,
        // dispatch_count=N, pad x2.
        {
            let wgsl = standalone_for_spec::<TaperMesh>().expect("taper_mesh codegen");
            let uniforms = &[
                0u32, u(1.0), u(0.0), u(1.0), 0u32, count, 0u32, 0u32,
            ];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.taper_mesh", &src, &out)
            }))
            .map_err(|_| failures.push("node.taper_mesh".into()));
        }

        // morph_mesh: t=0, weights_len=0, dispatch_count=N, pad. Bind b at binding 2.
        {
            let wgsl = standalone_for_spec::<MorphMesh>().expect("morph_mesh codegen");
            let uniforms = &[u(0.0), 0u32, count, 0u32];
            let sbuf = device.create_buffer_shared(std::mem::size_of_val(src.as_slice()) as u64);
            unsafe {
                sbuf.write(0, bytemuck::cast_slice(&src));
            }
            let extra = vec![
                GpuBinding::Buffer { binding: 2, buffer: &sbuf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &filler, offset: 0 },
            ];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 4);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.morph_mesh", &src, &out)
            }))
            .map_err(|_| failures.push("node.morph_mesh".into()));
        }

        // push_along_normals: amount=0, field_bias=0.5, weights_len=0, use_field=0,
        // dispatch_count=N, pad x3. Bind dummy field at 3/4.
        {
            let wgsl = standalone_for_spec::<PushAlongNormals>().expect("push_along_normals codegen");
            let uniforms = &[
                u(0.0), u(0.5), 0u32, 0u32, count, 0u32, 0u32, 0u32,
            ];
            let extra = vec![
                GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 },
                GpuBinding::Texture { binding: 3, texture: &dummy_field },
                GpuBinding::Sampler { binding: 4, sampler: &sampler },
            ];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 5);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.push_along_normals", &src, &out)
            }))
            .map_err(|_| failures.push("node.push_along_normals".into()));
        }

        // voxelize_mesh: amount=0, cell_size=1, weights_len=0, dispatch_count=N.
        {
            let wgsl = standalone_for_spec::<VoxelizeMesh>().expect("voxelize_mesh codegen");
            let uniforms = &[u(0.0), u(1.0), 0u32, count];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.voxelize_mesh", &src, &out)
            }))
            .map_err(|_| failures.push("node.voxelize_mesh".into()));
        }

        // noise_displace: amount=0, frequency=1, speed=1, time=0, weights_len=0,
        // dispatch_count=N, pad x2.
        {
            let wgsl = standalone_for_spec::<NoiseDisplace>().expect("noise_displace codegen");
            let uniforms = &[
                u(0.0), u(1.0), u(1.0), u(0.0), 0u32, count, 0u32, 0u32,
            ];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.noise_displace", &src, &out)
            }))
            .map_err(|_| failures.push("node.noise_displace".into()));
        }

        // glitch_jitter: amount=0, rate=10, seed=0, time=0, weights_len=0,
        // dispatch_count=N, pad x2.
        {
            let wgsl = standalone_for_spec::<GlitchJitter>().expect("glitch_jitter codegen");
            let uniforms = &[
                u(0.0), u(10.0), u(0.0), u(0.0), 0u32, count, 0u32, 0u32,
            ];
            let extra = vec![GpuBinding::Buffer { binding: 2, buffer: &filler, offset: 0 }];
            let out = dispatch(&device, &wgsl, &src, uniforms, extra, 3);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_position_identity("node.glitch_jitter", &src, &out)
            }))
            .map_err(|_| failures.push("node.glitch_jitter".into()));
        }

        assert!(
            failures.is_empty(),
            "default_passthrough failed for: {}",
            failures.join(", ")
        );
    }
}
