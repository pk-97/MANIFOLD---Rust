//! `node.bend_mesh` — classic per-vertex bend of an `Array<MeshVertex>`
//! (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D3/D4, §3 atom table).
//!
//! Rotation convention (bend axis `A`, companion `C` = the next axis in
//! cyclic X→Y→Z→X order, rotation happens about the THIRD axis `B` — the
//! one that is neither `A` nor `C`):
//! - `axis=X` (A=X, C=Y, B=Z): rotate `(x, y)` about Z, pivoting `x` by
//!   `center`.
//! - `axis=Y` (A=Y, C=Z, B=X): rotate `(y, z)` about X, pivoting `y` by
//!   `center`.
//! - `axis=Z` (A=Z, C=X, B=Y): rotate `(z, x)` about Y, pivoting `z` by
//!   `center`.
//!
//! `s = coord(A) - center`, `theta = angle * s * w` (`w` the optional
//! per-vertex `weights` input, degrading to 1.0 past a short/unwired
//! buffer per D2). Position: `A' = center + s*cos(theta) - C*sin(theta)`,
//! `C' = s*sin(theta) + C*cos(theta)`. Normal rotates by the SAME theta
//! with NO center pivot (a direction, not a point) — exact (D4).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const BEND_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `angle`, `center` f32), then the derived `weights_len` (u32),
/// then the codegen-injected `dispatch_count`, padded to a 16-byte
/// multiple. 5 words + 3 pad = 32 bytes. Matches
/// `standalone_for_spec::<BendMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BendUniforms {
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
    name: BendMesh,
    type_id: "node.bend_mesh",
    purpose: "Classic per-vertex bend of an Array<MeshVertex>. axis selects the bend coordinate A (X/Y/Z); the rotation happens about the third axis B (the one that is neither A nor its cyclic-next companion C), pivoting A's own coordinate by `center`: axis=X rotates (x,y) about Z pivoting x; axis=Y rotates (y,z) about X pivoting y; axis=Z rotates (z,x) about Y pivoting z. theta = angle * (coord(A) - center) * w, where w is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Position AND normal rotate by the same theta — exact, not approximate (normal has no center pivot, since it's a direction not a point). `angle` is UNBOUNDED (range None) — a saw LFO doing full revolutions is a valid performer gesture, never clamp it.",
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
            range: Some((0.0, (BEND_AXES.len() - 1) as f32)),
            enum_values: BEND_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
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
    composition_notes: "The classic 'bend a rod into an arc' deformer. Wire node.mesh_ramp's `weights` output to grow the bend progressively across the mesh (sweep `phase` to unfurl a bend over bars). `angle` is unbounded — wire a saw LFO doing full revolutions straight into it, sin/cos absorb the wrap with no seam (BUG-039 class, do not add a downstream clamp). Pair with node.facet_normals downstream only if a heavy compound bend+push chain needs a reset; bend's own normal transform is exact on its own.",
    examples: ["TwistColumn"],
    picker: { label: "Bend Mesh", category: Atom },
    summary: "Curves a mesh into an arc around a hinge line, like bending a rod. Position and lighting normals both rotate exactly, so it reads correctly at any angle, including full revolutions.",
    category: Geometry3D,
    role: Filter,
    aliases: ["bend mesh", "bend deformer", "curve", "arc"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/bend_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so a bend->twist->taper chain fuses to ~1 dispatch
    // (design decided #10). `weights_len` is a frame-derived uniform the body
    // uses to bounds-check the coincident weight read (degrade to 1.0 past
    // the buffer, D2).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for BendMesh {
    /// Output `out` is sized to match input `in` — a bend is a per-vertex
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
            Some(ParamValue::Enum(v)) => (*v).min((BEND_AXES.len() - 1) as u32),
            _ => 1,
        };
        let angle = ctx.scalar_or_param("angle", 0.5);
        let center = ctx.scalar_or_param("center", 0.0);

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        // Optional weights: unwired -> reuse `src` as a harmless filler
        // buffer (weights_len=0 means the shader never dereferences it,
        // same pattern as node.push_along_normals).
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
            // in the graph compiler. bend_mesh.wgsl is retained only as the
            // gpu_tests parity oracle. Bindings: uniform(0), buf_in(1),
            // buf_weights(2), buf_out(3).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.bend_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.bend_mesh",
            )
        });

        let uniforms = BendUniforms {
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
            "node.bend_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn bend_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(BendMesh::TYPE_ID, "node.bend_mesh");

        let in_port = BendMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = BendMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["angle", "center"] {
            let port = BendMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert!(
            !BendMesh::INPUTS.iter().any(|p| p.name == "axis"),
            "axis is an enum — must not be port-shadowed"
        );

        assert_eq!(BendMesh::OUTPUTS.len(), 1);
        assert_eq!(BendMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn bend_mesh_angle_is_unbounded() {
        let angle = BendMesh::PARAMS.iter().find(|p| p.name == "angle").unwrap();
        assert_eq!(angle.range, None, "angle must be unbounded (BUG-039 class)");
    }

    #[test]
    fn bend_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = BendMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BendMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.bend_mesh");
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<BendMesh>()
            .expect("bend_mesh buffer codegen")
    }

    /// Hand reference: bit-for-bit the committed formula (module doc
    /// comment), f64 internally for a tighter analytic bar, cast to f32.
    fn expected_bend(
        pos: [f32; 3],
        normal: [f32; 3],
        axis: u32,
        angle: f32,
        center: f32,
        w: f32,
    ) -> ([f32; 3], [f32; 3]) {
        let coord = pos[axis as usize] as f64;
        let s = coord - center as f64;
        let theta = angle as f64 * s * w as f64;
        let c = theta.cos();
        let sn = theta.sin();
        let (a_i, c_i, b_i) = match axis {
            0 => (0usize, 1usize, 2usize), // A=x, C=y, B=z
            1 => (1, 2, 0),                // A=y, C=z, B=x
            _ => (2, 0, 1),                // A=z, C=x, B=y
        };
        let mut new_pos = pos;
        let p_c = pos[c_i] as f64;
        new_pos[a_i] = (center as f64 + s * c - p_c * sn) as f32;
        new_pos[c_i] = (s * sn + p_c * c) as f32;
        new_pos[b_i] = pos[b_i];

        let mut new_n = normal;
        let n_a = normal[a_i] as f64;
        let n_c = normal[c_i] as f64;
        new_n[a_i] = (n_a * c - n_c * sn) as f32;
        new_n[c_i] = (n_a * sn + n_c * c) as f32;
        new_n[b_i] = normal[b_i];

        (new_pos, new_n)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_bend(
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
            "bend-mesh-test",
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

        let uniforms = BendUniforms {
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
        let mut enc = device.create_encoder("bend-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(src.len() as u32).div_ceil(256), 1, 1],
            "bend-mesh-test",
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
        let hand = include_str!("shaders/bend_mesh.wgsl");

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
                let from_gen = dispatch_bend(&device, &gen_wgsl, &src, w, None, axis, 1.3, 0.4);
                let from_hand = dispatch_bend(&device, hand, &src, w, None, axis, 1.3, 0.4);
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

    #[test]
    fn exact_normals_match_analytic_rotation() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let src = vec![
            mk_vertex([0.5, -0.3, 1.2], [0.267, 0.535, 0.802], [0.1, 0.2]),
            mk_vertex([-1.1, 0.9, -0.4], [0.0, 1.0, 0.0], [0.3, 0.7]),
            mk_vertex([2.0, 2.0, -2.0], [0.707, 0.0, 0.707], [0.9, 0.4]),
        ];
        let angle = 0.9f32;
        let center = 0.2f32;
        for axis in [0u32, 1, 2] {
            let out = dispatch_bend(&device, &gen_wgsl, &src, None, None, axis, angle, center);
            for (i, v) in src.iter().enumerate() {
                let (exp_pos, exp_n) = expected_bend(v.position, v.normal, axis, angle, center, 1.0);
                for c in 0..3 {
                    assert!(
                        (out[i].position[c] - exp_pos[c]).abs() < 1e-5,
                        "axis={axis} vertex {i} pos[{c}]: got={} expected={}",
                        out[i].position[c],
                        exp_pos[c]
                    );
                    assert!(
                        (out[i].normal[c] - exp_n[c]).abs() < 1e-5,
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
        let out = dispatch_bend(&device, &gen_wgsl, &src, None, None, 1, 0.6, 0.0);
        assert_eq!(out.len(), src.len());
        for i in 0..src.len() {
            assert_eq!(out[i].uv, src[i].uv, "uv must pass through unchanged at {i}");
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        // 12 identical vertices at x=1.0 so the bend amount along Y directly
        // reads off the effective weight. weights_len forced short (2)
        // independently of the (full-size) physical buffer, per D2/§4.
        let src: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]))
            .collect();
        let weights = [0.0f32, 0.0];
        let angle = 0.5f32;

        let out = dispatch_bend(&device, &gen_wgsl, &src, Some(&weights), Some(2), 2, angle, 0.0);

        // axis=Z rotates (z,x) about Y pivoting z by center(0). With z=0,
        // theta=0 for w=0 -> position/normal pass through unchanged.
        assert!(
            (out[0].position[0] - 1.0).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged, got x={}",
            out[0].position[0]
        );
        assert!(
            (out[1].position[0] - 1.0).abs() < 1e-5,
            "vertex 1 has explicit weight 0 -> unchanged, got x={}",
            out[1].position[0]
        );
        // Past weights_len, w degrades to 1.0: axis=Z pivots z (=0) by
        // center(0), so s=0 -> theta=0 regardless of w. Use a case where
        // w actually matters instead: rerun with axis=1 so coord=y (also 0
        // here)... pick axis=0 (A=x) directly so w multiplies a nonzero s.
        let out2 = dispatch_bend(&device, &gen_wgsl, &src, Some(&weights), Some(2), 0, angle, 0.0);
        let full = expected_bend([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0, angle, 0.0, 1.0);
        let zero = expected_bend([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0, angle, 0.0, 0.0);
        assert!(
            (out2[0].position[0] - zero.0[0]).abs() < 1e-5,
            "vertex 0 explicit weight 0: got={} expected={}",
            out2[0].position[0],
            zero.0[0]
        );
        for (i, v) in out2.iter().enumerate().skip(2).take(10) {
            assert!(
                (v.position[1] - full.0[1]).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0, got y={} expected={}",
                v.position[1],
                full.0[1]
            );
        }
    }
}
