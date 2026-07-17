//! `node.twist_mesh` — per-vertex twist of an `Array<MeshVertex>` about its
//! own bend axis (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D3/D4, §3 atom
//! table).
//!
//! `theta(v) = angle * (coord(v) - center) * w`, where `coord(v)` is the
//! vertex's coordinate along `axis` and `w` is the optional per-vertex
//! `weights` input (degrading to 1.0 past a short/unwired buffer, D2).
//! Position and normal rotate about `axis` itself by `theta` — exact
//! (D4): `axis=X` rotates `(y, z)`, `axis=Y` rotates `(z, x)`, `axis=Z`
//! rotates `(x, y)` (the standard right-handed per-axis rotation family,
//! cyclic). No pivot subtraction on the rotated pair — the axis passes
//! through local-space origin; `center` only shifts where along `axis`
//! `theta` is zero.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const TWIST_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `angle`, `center` f32), then the derived `weights_len` (u32),
/// then the codegen-injected `dispatch_count`, padded to a 16-byte
/// multiple. 5 words + 3 pad = 32 bytes. Matches
/// `standalone_for_spec::<TwistMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TwistUniforms {
    axis: u32,
    angle: f32,
    center: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: TwistMesh,
    type_id: "node.twist_mesh",
    purpose: "Per-vertex twist of an Array<MeshVertex> about its own `axis`. theta(v) = angle * (coord(v) - center) * w, where coord(v) is the vertex's coordinate along `axis` and w is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Position AND normal rotate about `axis` by theta — exact: axis=X rotates (y,z), axis=Y rotates (z,x), axis=Z rotates (x,y). `angle` is UNBOUNDED (range None) — a saw LFO doing full revolutions is a valid performer gesture (BUG-039 class): sin/cos absorb the wrap with no seam, never clamp it.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        angle: ScalarF32 optional,
        center: ScalarF32 optional,
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
            range: Some((0.0, (TWIST_AXES.len() - 1) as f32)),
            enum_values: TWIST_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: None,
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
    ],
    depth_rule: Terminal,
    composition_notes: "The 'twisting column / vine' atom — a strip extruded along `axis` twists into a helix. Wire node.mesh_ramp's `weights` output to grow the twist progressively along the length (sweep `phase` so the twist unwinds from one end). `angle` is unbounded: wire a saw LFO (min=0, max a multiple of 2*pi) straight into it for continuous full-revolution spin — sin/cos absorb the wrap with no seam, never clamp it downstream (BUG-039 class). The canonical demo is TwistColumn: node.grid_mesh (long in X, narrow in Z) -> node.make_triangles -> node.twist_mesh(axis=X).",
    examples: ["TwistColumn"],
    picker: { label: "Twist Mesh", category: Atom },
    summary: "Twists a mesh around its own length, like wringing out a cloth or spinning a vine. Position and lighting normals both rotate exactly, so continuous saw-LFO spin reads with no seam.",
    category: Geometry3D,
    role: Filter,
    aliases: ["twist mesh", "twist deformer", "spin", "helix"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/twist_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so a bend->twist->taper chain fuses to ~1 dispatch
    // (design decided #10). `weights_len` is a frame-derived uniform the body
    // uses to bounds-check the coincident weight read (degrade to 1.0 past
    // the buffer, D2).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for TwistMesh {
    /// Output `out` is sized to match input `in` — a twist is a per-vertex
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
            Some(ParamValue::Enum(v)) => (*v).min((TWIST_AXES.len() - 1) as u32),
            _ => 1,
        };
        let angle = ctx.scalar_or_param("angle", 1.0);
        let center = ctx.scalar_or_param("center", 0.0);

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
            // in the graph compiler. twist_mesh.wgsl is retained only as the
            // gpu_tests parity oracle. Bindings: uniform(0), buf_in(1),
            // buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.twist_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.twist_mesh",
            )
        });

        let uniforms = TwistUniforms {
            axis,
            angle,
            center,
            weights_len,
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
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.twist_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn twist_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(TwistMesh::TYPE_ID, "node.twist_mesh");

        let in_port = TwistMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = TwistMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["angle", "center"] {
            let port = TwistMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert!(
            !TwistMesh::INPUTS.iter().any(|p| p.name == "axis"),
            "axis is an enum — must not be port-shadowed"
        );

        assert_eq!(TwistMesh::OUTPUTS.len(), 1);
        assert_eq!(TwistMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn twist_mesh_angle_is_unbounded() {
        let angle = TwistMesh::PARAMS.iter().find(|p| p.name == "angle").unwrap();
        assert_eq!(angle.range, None, "angle must be unbounded (BUG-039 class)");
    }

    #[test]
    fn twist_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = TwistMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TwistMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.twist_mesh");
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<TwistMesh>()
            .expect("twist_mesh buffer codegen")
    }

    /// Hand reference: bit-for-bit the committed formula (module doc
    /// comment), f64 internally for a tighter analytic bar, cast to f32.
    fn expected_twist(
        pos: [f32; 3],
        normal: [f32; 3],
        axis: u32,
        angle: f32,
        center: f32,
        w: f32,
    ) -> ([f32; 3], [f32; 3]) {
        let coord = pos[axis as usize] as f64;
        let theta = angle as f64 * (coord - center as f64) * w as f64;
        let c = theta.cos();
        let sn = theta.sin();
        // axis=X rotates (y,z); axis=Y rotates (z,x); axis=Z rotates (x,y).
        let (p_i, q_i) = match axis {
            0 => (1usize, 2usize),
            1 => (2, 0),
            _ => (0, 1),
        };
        let mut new_pos = pos;
        let pp = pos[p_i] as f64;
        let pq = pos[q_i] as f64;
        new_pos[p_i] = (pp * c - pq * sn) as f32;
        new_pos[q_i] = (pp * sn + pq * c) as f32;

        let mut new_n = normal;
        let np = normal[p_i] as f64;
        let nq = normal[q_i] as f64;
        new_n[p_i] = (np * c - nq * sn) as f32;
        new_n[q_i] = (np * sn + nq * c) as f32;

        (new_pos, new_n)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_twist(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        axis: u32,
        angle: f32,
        center: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "twist-mesh-test",
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

        let uniforms = TwistUniforms {
            axis,
            angle,
            center,
            weights_len,
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("twist-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "twist-mesh-test",
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
        let hand = include_str!("shaders/twist_mesh.wgsl");

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
                let from_gen = dispatch_twist(&device, &gen_wgsl, &src, w, None, axis, 1.3, 0.4);
                let from_hand = dispatch_twist(&device, hand, &src, w, None, axis, 1.3, 0.4);
                for i in 0..src.len() {
                    for c in 0..3 {
                        assert!(
                            (from_gen[i].position[c] - from_hand[i].position[c]).abs() < 1e-5,
                            "axis={axis} w={use_w} vertex {i} pos[{c}]: gen={} hand={}",
                            from_gen[i].position[c],
                            from_hand[i].position[c]
                        );
                        assert!(
                            (from_gen[i].normal[c] - from_hand[i].normal[c]).abs() < 1e-5,
                            "axis={axis} w={use_w} vertex {i} normal[{c}]"
                        );
                    }
                    assert_eq!(from_gen[i].uv, from_hand[i].uv);
                }
            }
        }
    }

    /// The exact bar this test guards: a saw LFO doing full revolutions
    /// (angle sweeping past 2*pi and beyond) must keep producing the exact
    /// analytic rotation with no clamp-induced stall. §4 "bend/twist rotate
    /// normals exactly".
    #[test]
    fn exact_normals_match_analytic_rotation_past_2pi() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
        ];
        let center = 0.2f32;
        for axis in [0u32, 1, 2] {
            for &angle in &[0.3f32, std::f32::consts::PI, 2.5 * std::f32::consts::PI, 9.4] {
                let out = dispatch_twist(&device, &gen_wgsl, &src, None, None, axis, angle, center);
                for (i, v) in src.iter().enumerate() {
                    let (exp_pos, exp_n) =
                        expected_twist(v.position, v.normal, axis, angle, center, 1.0);
                    for c in 0..3 {
                        assert!(
                            (out[i].position[c] - exp_pos[c]).abs() < 1e-5,
                            "axis={axis} angle={angle} vertex {i} pos[{c}]: got={} expected={}",
                            out[i].position[c],
                            exp_pos[c]
                        );
                        assert!(
                            (out[i].normal[c] - exp_n[c]).abs() < 1e-5,
                            "axis={axis} angle={angle} vertex {i} normal[{c}]: got={} expected={}",
                            out[i].normal[c],
                            exp_n[c]
                        );
                    }
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
        let out = dispatch_twist(&device, &gen_wgsl, &src, None, None, 0, 0.6, 0.0);
        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].uv, src[i].uv, "uv must pass through unchanged at {i}");
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // 12 identical vertices at x=1.0 so axis=X's theta (coord=x) directly
        // reads off the effective weight through the (y,z) rotation.
        let src: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0];
        let angle = 0.5f32;

        let out = dispatch_twist(&device, &gen_wgsl, &src, Some(&weights), Some(2), 0, angle, 0.0);
        let full = expected_twist([1.0, 0.0, 1.0], [0.0, 1.0, 0.0], 0, angle, 0.0, 1.0);
        let zero = expected_twist([1.0, 0.0, 1.0], [0.0, 1.0, 0.0], 0, angle, 0.0, 0.0);

        assert!(
            (out[0].position[1] - zero.0[1]).abs() < 1e-5 && (out[0].position[2] - zero.0[2]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged, got y={} z={}",
            out[0].position[1],
            out[0].position[2]
        );
        for (i, v) in out.iter().enumerate().skip(2).take(10) {
            assert!(
                (v.position[1] - full.0[1]).abs() < 1e-5 && (v.position[2] - full.0[2]).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0, got y={} z={} expected y={} z={}",
                v.position[1],
                v.position[2],
                full.0[1],
                full.0[2]
            );
        }
    }
}
