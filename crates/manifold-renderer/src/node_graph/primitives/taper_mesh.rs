//! `node.taper_mesh` — per-vertex taper of an `Array<MeshVertex>` along
//! `axis` (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D3/D4, §3 atom table).
//!
//! `s(v) = mix(1, taper, clamp((coord(v) - center) / length, 0, 1) * w)`,
//! where `coord(v)` is the vertex's coordinate along `axis` and `w` is the
//! optional per-vertex `weights` input (degrading to 1.0 past a
//! short/unwired buffer, D2). The two off-axis position components scale
//! by `s`; the axis component is unchanged. Normal: the off-axis
//! components divide by `s` (inverse-transpose scale), then the whole
//! normal renormalizes — exact for this transform (D4).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const TAPER_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `taper`, `center`, `length` f32), then the derived
/// `weights_len` (u32), then the codegen-injected `dispatch_count`, padded
/// to a 16-byte multiple. 6 words + 2 pad = 32 bytes. Matches
/// `standalone_for_spec::<TaperMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TaperUniforms {
    axis: u32,
    taper: f32,
    center: f32,
    length: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: TaperMesh,
    type_id: "node.taper_mesh",
    purpose: "Per-vertex taper of an Array<MeshVertex> along `axis`. s(v) = mix(1, taper, clamp((coord(v) - center) / length, 0, 1) * w), where coord(v) is the vertex's coordinate along `axis` and w is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). The two off-axis position components scale by s (taper=1 no change, taper=0 collapses to a point on the axis); normal off-axis components divide by s and the normal renormalizes — exact for this transform (D4). `t*w` is not re-clamped after the mix, so weights above 1.0 produce honest extrapolation (over-taper / flare) rather than a silent clamp.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        taper: ScalarF32 optional,
        center: ScalarF32 optional,
        length: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Y
            range: Some((0.0, (TAPER_AXES.len() - 1) as f32)),
            enum_values: TAPER_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("taper"),
            label: "Taper",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("center"),
            label: "Center",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("length"),
            label: "Length",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.001, 200.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'pencil / spire / carrot' taper — a column extruded along `axis` narrows to a point over `length` starting at `center`. Wire node.mesh_ramp's `weights` output to grow the taper progressively instead of applying it uniformly. `taper=0` collapses the far end to a line on the axis (a cone tip); `taper=1` is pass-through. Composes with node.twist_mesh and node.bend_mesh on the same axis for a tapered twisted column in one fused chain (design decided #10).",
    examples: [],
    picker: { label: "Taper Mesh", category: Atom },
    summary: "Narrows a mesh toward a point along one axis, like sharpening a pencil or a candle flame. The lighting normals scale with it so the taper still shades correctly.",
    category: Geometry3D,
    role: Filter,
    aliases: ["taper mesh", "taper deformer", "cone", "narrow"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/taper_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so a bend->twist->taper chain fuses to ~1 dispatch
    // (design decided #10). `weights_len` is a frame-derived uniform the body
    // uses to bounds-check the coincident weight read (degrade to 1.0 past
    // the buffer, D2).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for TaperMesh {
    /// Output `out` is sized to match input `in` — a taper is a per-vertex
    /// transform, no expansion.
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
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min((TAPER_AXES.len() - 1) as u32),
            _ => 1,
        };
        let taper = ctx.scalar_or_param("taper", 0.5);
        let center = ctx.scalar_or_param("center", 0.0);
        let length = ctx.scalar_or_param("length", 1.0);

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
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
            // Codegen path (design decided #10): the runtime kernel is
            // generated from `wgsl_body` so this atom stays pointwise/fusable
            // in the graph compiler. taper_mesh.wgsl is retained only as the
            // gpu_tests parity oracle. Bindings: uniform(0), buf_in(1),
            // buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.taper_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.taper_mesh",
            )
        });

        let uniforms = TaperUniforms {
            axis,
            taper,
            center,
            length,
            weights_len,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
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
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.taper_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn taper_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(TaperMesh::TYPE_ID, "node.taper_mesh");

        let in_port = TaperMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = TaperMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["taper", "center", "length"] {
            let port = TaperMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert!(
            !TaperMesh::INPUTS.iter().any(|p| p.name == "axis"),
            "axis is an enum — must not be port-shadowed"
        );

        assert_eq!(TaperMesh::OUTPUTS.len(), 1);
        assert_eq!(TaperMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn taper_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = TaperMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TaperMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.taper_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. No legacy predecessor to diff against —
    //! parity is against a hand-written Rust reference of the committed
    //! formula, element-wise, per DECOMPOSING_GENERATORS.md §9.
    use super::*;

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

    /// The generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<TaperMesh>()
            .expect("taper_mesh buffer codegen")
    }

    /// Hand reference: bit-for-bit the committed formula (module doc
    /// comment), f64 internally for a tighter analytic bar, cast to f32.
    fn expected_taper(
        pos: [f32; 3],
        normal: [f32; 3],
        axis: u32,
        taper: f32,
        center: f32,
        length: f32,
        w: f32,
    ) -> ([f32; 3], [f32; 3]) {
        let coord = pos[axis as usize] as f64;
        let t = ((coord - center as f64) / (length as f64).max(1e-6)).clamp(0.0, 1.0);
        let mixf = t * w as f64;
        let s = 1.0 + (taper as f64 - 1.0) * mixf;
        let denom = if s.abs() < 1e-6 { 1e-6 } else { s };

        let (a_i, o1, o2) = match axis {
            0 => (0usize, 1usize, 2usize),
            1 => (1, 2, 0),
            _ => (2, 0, 1),
        };
        let mut new_pos = pos;
        new_pos[o1] = (pos[o1] as f64 * s) as f32;
        new_pos[o2] = (pos[o2] as f64 * s) as f32;
        new_pos[a_i] = pos[a_i];

        let mut n = [0.0f64; 3];
        n[a_i] = normal[a_i] as f64;
        n[o1] = normal[o1] as f64 / denom;
        n[o2] = normal[o2] as f64 / denom;
        let mag = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
        let new_n = [
            (n[0] / mag) as f32,
            (n[1] / mag) as f32,
            (n[2] / mag) as f32,
        ];

        (new_pos, new_n)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_taper(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        axis: u32,
        taper: f32,
        center: f32,
        length: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "taper-mesh-test",
        );
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

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
            None => (device.create_buffer_shared(std::mem::size_of_val(src) as u64), 0),
        };

        let uniforms = TaperUniforms {
            axis,
            taper,
            center,
            length,
            weights_len,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("taper-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "taper-mesh-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn generated_matches_hand_kernel_all_axes() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        assert!(gen_wgsl.contains("struct Element"), "element struct synthesized");
        assert!(gen_wgsl.contains("var<storage, read> buf_in"), "in bound read storage");
        assert!(gen_wgsl.contains("var<storage, read> buf_weights"), "weights bound read storage");
        assert!(gen_wgsl.contains("weights_len: u32"), "derived weights_len injected");
        assert!(gen_wgsl.contains("var<storage, read_write> buf_out"), "out bound read_write");
        // Confirms the "length" WGSL-builtin collision is resolved via the
        // codegen RESERVED-word rename (freeze/codegen.rs wgsl_safe_field).
        assert!(gen_wgsl.contains("p_length"), "length param renamed to avoid the WGSL length() builtin");
        let hand = include_str!("shaders/taper_mesh.wgsl");

        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]),
            mk_vertex([1.0, 2.0, -1.0], [0.577, 0.577, 0.577], [0.5, 0.25]),
            mk_vertex([-3.0, 1.0, 2.0], [0.0, 0.0, 1.0], [0.75, 0.9]),
            mk_vertex([2.0, -1.0, 0.5], [1.0, 0.0, 0.0], [0.2, 0.8]),
        ];
        let weights = [0.3f32, 0.8, 1.0, 0.5];
        for axis in [0u32, 1, 2] {
            for &use_w in &[false, true] {
                let w = if use_w { Some(&weights[..]) } else { None };
                let from_gen = dispatch_taper(&device, &gen_wgsl, &src, w, None, axis, 0.3, 0.1, 2.0);
                let from_hand = dispatch_taper(&device, hand, &src, w, None, axis, 0.3, 0.1, 2.0);
                for i in 0..src.len() {
                    for c in 0..3 {
                        assert!(
                            (from_gen[i].position[c] - from_hand[i].position[c]).abs() < 1e-5,
                            "axis={axis} w={use_w} vertex {i} pos[{c}]: gen={} hand={}",
                            from_gen[i].position[c],
                            from_hand[i].position[c]
                        );
                        assert!(
                            (from_gen[i].normal[c] - from_hand[i].normal[c]).abs() < 1e-4,
                            "axis={axis} w={use_w} vertex {i} normal[{c}]: gen={} hand={}",
                            from_gen[i].normal[c],
                            from_hand[i].normal[c]
                        );
                    }
                    assert_eq!(from_gen[i].uv, from_hand[i].uv);
                }
            }
        }
    }

    #[test]
    fn matches_hand_formula_analytically() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
        ];
        let (taper, center, length) = (0.2f32, -0.5f32, 1.7f32);
        for axis in [0u32, 1, 2] {
            let out = dispatch_taper(&device, &gen_wgsl, &src, None, None, axis, taper, center, length);
            for (i, v) in src.iter().enumerate() {
                let (exp_pos, exp_n) =
                    expected_taper(v.position, v.normal, axis, taper, center, length, 1.0);
                for c in 0..3 {
                    assert!(
                        (out[i].position[c] - exp_pos[c]).abs() < 1e-5,
                        "axis={axis} vertex {i} pos[{c}]: got={} expected={}",
                        out[i].position[c],
                        exp_pos[c]
                    );
                    assert!(
                        (out[i].normal[c] - exp_n[c]).abs() < 1e-4,
                        "axis={axis} vertex {i} normal[{c}]: got={} expected={}",
                        out[i].normal[c],
                        exp_n[c]
                    );
                }
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
        let out = dispatch_taper(&device, &gen_wgsl, &src, None, None, 1, 0.4, 0.0, 1.0);
        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].uv, src[i].uv, "uv must pass through unchanged at {i}");
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // 12 identical vertices at y=1.0 (axis=Y) so the off-axis scale
        // directly reads off the effective weight.
        let src: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([1.0, 1.0, 1.0], [1.0, 0.0, 0.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0];
        let (taper, center, length) = (0.0f32, 0.0f32, 1.0f32);

        let out = dispatch_taper(&device, &gen_wgsl, &src, Some(&weights), Some(2), 1, taper, center, length);
        let full = expected_taper([1.0, 1.0, 1.0], [1.0, 0.0, 0.0], 1, taper, center, length, 1.0);
        let zero = expected_taper([1.0, 1.0, 1.0], [1.0, 0.0, 0.0], 1, taper, center, length, 0.0);

        assert!(
            (out[0].position[0] - zero.0[0]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged, got x={} expected={}",
            out[0].position[0],
            zero.0[0]
        );
        for (i, v) in out.iter().enumerate().skip(2).take(10) {
            assert!(
                (v.position[0] - full.0[0]).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0, got x={} expected={}",
                v.position[0],
                full.0[0]
            );
        }
    }
}
