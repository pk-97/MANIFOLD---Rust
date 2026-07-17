//! `node.push_along_normals` — displace each vertex of an `Array<MeshVertex>`
//! outward (or inward) along its own normal. The core "inflate / breathe"
//! deformer (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D1/D2/D4).
//!
//! `pos += normal * amount * w * f`, where `w` is the optional per-vertex
//! `weights` input (D2 — a short or unwired weights buffer degrades to
//! 1.0, never to silent 0) and `f` is an optional `field` Texture2D
//! sampled bilinear at the vertex's own UV (`sample.r - field_bias`), or
//! 1.0 when unwired. Normals are passed through unchanged — approximate
//! at extremes (D4); wire `node.facet_normals` downstream after a heavy
//! push, or keep `amount` moderate to keep the source mesh's smooth
//! normals.

use std::borrow::Cow;

use manifold_gpu::{
    GpuBinding, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amount`,
/// `field_bias` f32), then the derived `weights_len` (u32 — 0 when unwired,
/// collapsing the "absent" and "short" degrade-to-1.0 cases into one bounds
/// check), then the injected optional-texture flag `use_field` (u32), then the
/// codegen-injected `dispatch_count`, padded to a 16-byte multiple. 5 words +
/// 3 pad = 32 bytes. Matches `standalone_for_spec::<PushAlongNormals>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PushUniforms {
    amount: f32,
    field_bias: f32,
    weights_len: u32,
    use_field: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: PushAlongNormals,
    type_id: "node.push_along_normals",
    purpose: "Displace each vertex of an Array<MeshVertex> outward (or inward) along its own normal: pos += normal * amount * w * f. `w` is the optional per-vertex `weights` input (from node.mesh_ramp or any weights producer) — a short or unwired weights buffer degrades to 1.0 (full push), never to silent 0. `f` is an optional Texture2D `field` sampled bilinear at the vertex's own UV as (sample.r - field_bias), or 1.0 when unwired. Normals pass through unchanged — approximate at extremes, correct-looking for moderate organic-motion amounts; wire node.facet_normals downstream after a heavy push if the faceted look isn't wanted, or keep amount moderate to keep the source mesh's smooth normals.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        field: Texture2D optional,
        amount: ScalarF32 optional,
        field_bias: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.2),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("field_bias"),
            label: "Field Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'breathe' / 'inflate' atom — wire an LFO or node.envelope_follower_ar into `amount` for a mesh that pulses with the low band. Wire node.mesh_ramp's `weights` output to grow the push progressively across the mesh instead of uniformly. Wire a noise or image Texture2D into `field` to localize the push (field_bias = 0.5 centers it, matching node.push_mesh's height_bias convention). Pair with node.facet_normals downstream once amount is large enough that the unchanged normals start looking wrong under lighting.",
    examples: ["Breathe"],
    picker: { label: "Push Along Normals", category: Atom },
    summary: "Pushes every point of a mesh outward or inward along its own surface direction — the 3D version of a bulge or breathe effect, optionally masked and driven by an image.",
    category: Geometry3D,
    role: Filter,
    aliases: ["push along normals", "inflate", "breathe", "bulge"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/push_along_normals_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable, so a breathe→twist→taper chain fuses to ~1 dispatch
    // (design D#10). `weights_len` is a frame-derived uniform the body uses to
    // bounds-check the coincident weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
    extra_fields: {
        dummy_field: Option<GpuTexture> = None,
    },
}

impl Primitive for PushAlongNormals {
    /// Output `out` is sized to match input `in` — displacement is a
    /// per-vertex transform, no expansion.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = ctx.scalar_or_param("amount", 0.2);
        let field_bias = ctx.scalar_or_param("field_bias", 0.5);

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        // Optional weights: unwired -> reuse `src` as a harmless filler
        // buffer (weights_len=0 means the shader never dereferences it,
        // same pattern as node.torus_wrap_field's `normal_disp`).
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let field_wired = ctx.inputs.texture_2d("field");
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design D#10): the runtime kernel is generated from
            // `wgsl_body` so this atom stays pointwise/fusable in the graph
            // compiler. push_along_normals.wgsl is retained only as the gpu_tests
            // parity oracle. Bindings match: uniform(0), buf_in(1), buf_weights(2),
            // tex_field(3), samp(4), buf_out(5).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.push_along_normals standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.push_along_normals",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        // Metal requires every declared binding to be present at dispatch
        // even when the kernel's `has_field == 0` branch skips the sample
        // result. Cache a 1x1 white texture as the unwired fallback bind.
        let dummy = self.dummy_field.get_or_insert_with(|| {
            let tex = gpu.device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::Rgba8Unorm,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                label: "node.push_along_normals dummy field",
                mip_levels: 1,
            });
            gpu.device.upload_texture(&tex, &[255u8, 255, 255, 255]);
            tex
        });
        let field_tex = field_wired.unwrap_or(dummy);

        let uniforms = PushUniforms {
            amount,
            field_bias,
            weights_len,
            use_field: u32::from(field_wired.is_some()),
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: field_tex,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.push_along_normals",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn push_along_normals_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(PushAlongNormals::TYPE_ID, "node.push_along_normals");

        let in_port = PushAlongNormals::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = PushAlongNormals::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        let field_port = PushAlongNormals::INPUTS.iter().find(|p| p.name == "field").unwrap();
        assert!(!field_port.required);
        assert_eq!(field_port.ty, PortType::Texture2D);

        for name in ["amount", "field_bias"] {
            let port = PushAlongNormals::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(PushAlongNormals::OUTPUTS.len(), 1);
        assert_eq!(PushAlongNormals::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn push_along_normals_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = PushAlongNormals::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = PushAlongNormals::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.push_along_normals");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. No legacy predecessor to diff against —
    //! parity is against a hand-written Rust reference of the committed
    //! formula, element-wise, per DECOMPOSING_GENERATORS.md §9.
    use super::*;
    use half::f16;
    use manifold_gpu::GpuTextureFormat as Fmt;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
        }
    }

    /// A spatially UNIFORM field texture (every texel identical). Bilinear
    /// interpolation of a constant field is exactly that constant
    /// regardless of addressing/filtering edge cases, so the sampled
    /// value is hand-computable exactly — sidesteps needing to replicate
    /// Metal's bilinear kernel in Rust to get an element-wise parity bar.
    fn uniform_field_tex(device: &manifold_gpu::GpuDevice, w: u32, h: u32, value: f32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            px[i * 4] = f16::from_f32(value);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: Fmt::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "push-along-normals-field-test",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// The generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<PushAlongNormals>()
            .expect("push_along_normals buffer codegen")
    }

    /// `weights_len` overrides the logical weights length independently of the
    /// physical buffer, so the degrade-to-1.0 tail can be exercised WITHOUT an
    /// out-of-bounds coincident pre-read: the weights buffer is always sized to
    /// `src.len()` elements (the real graph always matches capacities), and
    /// `weights_len < count` is the bounds the body honors. `None` weights →
    /// weights_len 0 (unwired), the filler buffer is `src` (≥ count*4 bytes).
    #[allow(clippy::too_many_arguments)]
    fn dispatch_push(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        field: Option<&GpuTexture>,
        amount: f32,
        field_bias: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "push-normals-test",
        );
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

        // Physical weights buffer is always `src.len()` elements so the
        // coincident pre-read `buf_weights[idx]` is in-bounds for every thread;
        // logical length comes from `weights_len_override` (or the slice length).
        let (wbuf, weights_len) = match weights {
            Some(w) => {
                let mut padded = vec![0.0f32; src.len()];
                padded[..w.len().min(src.len())].copy_from_slice(&w[..w.len().min(src.len())]);
                let b = device.create_buffer_shared((padded.len() * 4).max(4) as u64);
                unsafe {
                    b.write(0, bytemuck::cast_slice(&padded));
                }
                (b, weights_len_override.unwrap_or(w.len() as u32))
            }
            // Unwired: bind `src` as the harmless filler (run()'s pattern), len 0.
            None => (device.create_buffer_shared(std::mem::size_of_val(src) as u64), 0),
        };

        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let dummy;
        let field_ref: &GpuTexture = match field {
            Some(f) => f,
            None => {
                dummy = {
                    let tex = device.create_texture(&GpuTextureDesc {
                        width: 1,
                        height: 1,
                        depth: 1,
                        format: GpuTextureFormat::Rgba8Unorm,
                        dimension: GpuTextureDimension::D2,
                        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                        label: "push-normals-dummy",
                        mip_levels: 1,
                    });
                    device.upload_texture(&tex, &[255u8, 255, 255, 255]);
                    tex
                };
                &dummy
            }
        };

        let uniforms = PushUniforms {
            amount,
            field_bias,
            weights_len,
            use_field: u32::from(field.is_some()),
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Texture { binding: 3, texture: field_ref },
            GpuBinding::Sampler { binding: 4, sampler: &sampler },
            GpuBinding::Buffer { binding: 5, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("push-normals-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "push-normals-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn generated_matches_hand_kernel_all_modes() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        assert!(gen_wgsl.contains("struct Element"), "element struct synthesized");
        assert!(gen_wgsl.contains("var<storage, read> buf_in"), "in bound read storage");
        assert!(gen_wgsl.contains("var<storage, read> buf_weights"), "weights bound read storage");
        assert!(gen_wgsl.contains("tex_field"), "optional field texture bound");
        assert!(gen_wgsl.contains("use_field: u32"), "optional-texture use flag injected");
        assert!(gen_wgsl.contains("weights_len: u32"), "derived weights_len injected");
        assert!(gen_wgsl.contains("var<storage, read_write> buf_out"), "out bound read_write");
        let hand = include_str!("shaders/push_along_normals.wgsl");

        let tex = uniform_field_tex(&device, 8, 8, 0.6);
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]),
            mk_vertex([1.0, 2.0, -1.0], [0.577, 0.577, 0.577], [0.5, 0.25]),
            mk_vertex([-3.0, 1.0, 2.0], [0.0, 0.0, 1.0], [0.75, 0.9]),
            mk_vertex([2.0, -1.0, 0.5], [1.0, 0.0, 0.0], [0.2, 0.8]),
        ];
        let weights = [0.3f32, 0.8, 1.0, 0.5];
        // Sweep: (weights?, field?) across all four wire combinations.
        for &(use_w, use_f) in &[(false, false), (true, false), (false, true), (true, true)] {
            let w = if use_w { Some(&weights[..]) } else { None };
            let f = if use_f { Some(&tex) } else { None };
            let from_gen_wgsl = dispatch_push(&device, &gen_wgsl, &src, w, None, f, 0.6, 0.4);
            let from_hand = dispatch_push(&device, hand, &src, w, None, f, 0.6, 0.4);
            for i in 0..src.len() {
                for c in 0..3 {
                    assert!(
                        (from_gen_wgsl[i].position[c] - from_hand[i].position[c]).abs() < 1e-6,
                        "w={use_w} f={use_f} vertex {i} pos[{c}]: gen={} hand={}",
                        from_gen_wgsl[i].position[c],
                        from_hand[i].position[c]
                    );
                    assert!((from_gen_wgsl[i].normal[c] - from_hand[i].normal[c]).abs() < 1e-6);
                }
                assert_eq!(from_gen_wgsl[i].uv, from_hand[i].uv);
            }
        }
    }

    #[test]
    fn count_order_and_uv_are_preserved() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.2]),
            mk_vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.3, 0.4]),
            mk_vertex([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.6]),
        ];
        let out = dispatch_push(&device, &gen_wgsl, &src, None, None, None, 0.5, 0.5);
        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].uv, src[i].uv, "uv must pass through unchanged at {i}");
            assert_eq!(out[i].normal, src[i].normal, "normal must pass through unchanged at {i}");
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // 12 identical vertices (position 0, normal +Y) so displacement
        // magnitude along Y directly reads off the effective weight. The §4
        // invariant guards the "silent zero" failure: even though the weight
        // buffer's tail (verts 2..12) physically holds 0.0, a logical
        // weights_len of 2 must make those verts degrade to w=1.0 (deform at
        // full), never to 0. weights_len is forced short here independently of
        // the physical buffer (which is full-size) so the coincident pre-read
        // stays in-bounds — the graph always matches capacities, so a genuinely
        // short physical wire never reaches this atom at runtime.
        let src: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0]; // first 2 explicit-zero, rest padded 0.0
        let amount = 0.7f32;

        let out = dispatch_push(&device, &gen_wgsl, &src, Some(&weights), Some(2), None, amount, 0.5);

        assert!(
            (out[0].position[1]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged, got y={}",
            out[0].position[1]
        );
        assert!(
            (out[1].position[1]).abs() < 1e-5,
            "vertex 1 has explicit weight 0 -> unchanged, got y={}",
            out[1].position[1]
        );
        for (i, v) in out.iter().enumerate().skip(2).take(10) {
            assert!(
                (v.position[1] - amount).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0 (full push {amount}), got y={}",
                v.position[1]
            );
        }
    }

    #[test]
    fn matches_hand_formula_with_weights_only() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]),
            mk_vertex([1.0, 2.0, -1.0], [1.0, 0.0, 0.0], [0.5, 0.25]),
            mk_vertex([-3.0, 1.0, 2.0], [0.0, 0.0, 1.0], [0.75, 0.9]),
        ];
        let weights = [0.3f32, 0.8, 1.0];
        let amount = 0.6f32;

        let out = dispatch_push(&device, &gen_wgsl, &src, Some(&weights), None, None, amount, 0.5);

        for i in 0..src.len() {
            let v = &src[i];
            let w = weights[i];
            // Hand reference: f = 1.0 exactly (field unwired).
            let expected = [
                v.position[0] + v.normal[0] * amount * w,
                v.position[1] + v.normal[1] * amount * w,
                v.position[2] + v.normal[2] * amount * w,
            ];
            for (c, exp) in expected.iter().enumerate() {
                assert!(
                    (out[i].position[c] - exp).abs() < 1e-5,
                    "vertex {i} position[{c}]: got={} expected={exp}",
                    out[i].position[c]
                );
            }
            assert_eq!(out[i].normal, v.normal);
            assert_eq!(out[i].uv, v.uv);
        }
    }

    #[test]
    fn matches_hand_formula_with_uniform_field() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let tex = uniform_field_tex(&device, 8, 8, 0.75);
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]),
            mk_vertex([2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.5, 0.5]),
            mk_vertex([0.0, 0.0, 5.0], [0.0, 1.0, 0.0], [1.0, 1.0]),
        ];
        let amount = 0.4f32;
        let field_bias = 0.4f32; // f = 0.75 - 0.4 = 0.35 exactly, all weights default to 1.0

        let out = dispatch_push(&device, &gen_wgsl, &src, None, None, Some(&tex), amount, field_bias);

        let f = 0.75f32 - field_bias;
        for i in 0..src.len() {
            let v = &src[i];
            let expected_y = v.position[1] + v.normal[1] * amount * 1.0 * f;
            assert!(
                (out[i].position[1] - expected_y).abs() < 1e-4,
                "vertex {i}: got y={} expected y={} (f={f})",
                out[i].position[1],
                expected_y
            );
            assert_eq!(out[i].position[0], v.position[0], "no X displacement (normal is +Y)");
            assert_eq!(out[i].position[2], v.position[2], "no Z displacement (normal is +Y)");
        }
    }
}
