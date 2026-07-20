//! `node.morph_mesh` — static two-mesh lerp between two `Array<MeshVertex>`s
//! (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D3/D4/D9, §3 atom table).
//!
//! `n = min(count_a, count_b)`; `pos = mix(a, b, t * w)`, `normal =
//! normalize(mix(a.normal, b.normal, t * w))`, `uv` from `a`. Correspondence
//! is by index — meaningful between variants of one mesh or as a deliberate
//! scramble-morph between unrelated ones; both are stage-valid. `w` is the
//! optional per-vertex `weights` input (degrading to 1.0 past a
//! short/unwired buffer, D2). Normals are approximate (D4) — this is the
//! static two-mesh lerp only; glTF morph-target playback is a separate
//! future design (D9), do not grow this atom toward it.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: the `t` param (f32), then the derived
/// `weights_len` (u32), then the codegen-injected `dispatch_count`, padded
/// to a 16-byte multiple. 3 words + 1 pad = 16 bytes. Matches
/// `standalone_for_spec::<MorphMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MorphUniforms {
    t: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: MorphMesh,
    type_id: "node.morph_mesh",
    purpose: "Static two-mesh lerp between two Array<MeshVertex>s, by index. n = min(count_a, count_b); pos = mix(a, b, t * w), normal = normalize(mix(a.normal, b.normal, t * w)), uv from `a`. `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Correspondence is by index — meaningful between variants of one mesh (a low-poly and a scanned-detail version of the same object) or as a deliberate scramble-morph between unrelated meshes of similar vertex count; both are stage-valid. Normals are approximate (lerp + renormalize), correct-looking for moderate blends. This is the static two-mesh lerp only — glTF morph-target playback is a separate future design, not an extension of this atom.",
    inputs: {
        in: Array(MeshVertex) required,
        b: Array(MeshVertex) required,
        weights: Array(f32) optional,
        t: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("t"),
            label: "Mix",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'dissolve into another shape' atom — wire an LFO or beat ramp into `t` to crossfade continuously between two meshes of the same vertex layout (e.g. two node.revolve_curve profiles at different `sweep`, or a scanned mesh and a procedural stand-in). Wire node.mesh_ramp's `weights` output to mask the morph to a region instead of blending uniformly. Output capacity follows `in` (like node.push_along_normals); run() truncates dispatch to min(a, b, out) so the shader can't read past either input — if the two meshes have different vertex counts, only the first min(count_a, count_b) verts morph, the rest pass through node.push_along_normals-style unaffected (they are simply outside the dispatch).",
    examples: [],
    picker: { label: "Morph Mesh", category: Atom },
    summary: "Blends smoothly between two meshes vertex-by-vertex, so one shape dissolves into another. Works best when both meshes share the same vertex count and layout.",
    category: Geometry3D,
    role: Filter,
    aliases: ["morph mesh", "mesh lerp", "dissolve", "crossfade"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/morph_mesh_body.wgsl"),
    // `in`, `b`, and `weights` are all COINCIDENT (default) — keeps the atom
    // fully pointwise/fusable (design decided #10), the same two-coincident-
    // array shape node.blend_copies proves for InstanceTransform. `weights_len`
    // is a frame-derived uniform the body uses to bounds-check the coincident
    // weight read (degrade to 1.0 past the buffer, D2).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for MorphMesh {
    /// Output `out` follows the SMALLER of `in`/`b` capacities — the shader
    /// dispatch is bounded to `min(count_a, count_b, out)` in `run()`, and
    /// declaring a larger capacity here would just leave the tail
    /// unwritten by this node (a downstream consumer sees a valid but
    /// partially-stale buffer). Declaring the true min keeps the capacity
    /// honest for every downstream reader.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let a = input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n);
        let b = input_capacities.iter().find(|(p, _)| *p == "b").map(|(_, n)| *n);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let t = ctx.scalar_or_param("t", 0.5);

        let Some(a_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(b_buf) = ctx.inputs.array("b") else {
            return;
        };
        // Optional weights: unwired -> reuse `a_buf` as a harmless filler
        // buffer (weights_len=0 means the shader never dereferences it,
        // same pattern as node.push_along_normals / node.bend_mesh).
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(a_buf);
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let a_cap = (a_buf.size / vertex_size) as u32;
        let b_cap = (b_buf.size / vertex_size) as u32;
        let out_cap = (out_buf.size / vertex_size) as u32;
        let count = a_cap.min(b_cap).min(out_cap);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|buf| (buf.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (design decided #10): the runtime kernel is
            // generated from `wgsl_body` so this atom stays pointwise/fusable
            // in the graph compiler. morph_mesh.wgsl is retained only as the
            // gpu_tests parity oracle. Bindings: uniform(0), buf_in(1),
            // buf_b(2), buf_weights(3), buf_out(4).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.morph_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.morph_mesh",
            )
        });

        let uniforms = MorphUniforms {
            t,
            weights_len,
            dispatch_count: count,
            _pad0: 0,
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
                    buffer: a_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: b_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.morph_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn morph_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(MorphMesh::TYPE_ID, "node.morph_mesh");

        for name in ["in", "b"] {
            let port = MorphMesh::INPUTS.iter().find(|p| p.name == name).unwrap();
            assert!(port.required, "{name} must be required");
            assert_eq!(port.ty, PortType::Array(mesh_layout));
        }

        let weights_port = MorphMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        let t_port = MorphMesh::INPUTS
            .iter()
            .find(|p| p.name == "t")
            .unwrap_or_else(|| panic!("t port-shadow input must exist"));
        assert!(!t_port.required, "t should be optional (port-shadow)");
        assert_eq!(t_port.ty, PortType::Scalar(ScalarType::F32));

        assert_eq!(MorphMesh::OUTPUTS.len(), 1);
        assert_eq!(MorphMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn morph_mesh_output_follows_smaller_of_in_and_b() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = MorphMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32), ("b", 20_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(20),
        );
        let inputs2 = [("in", 12_u32), ("b", 40_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs2),
            Some(12),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MorphMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.morph_mesh");
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
        crate::node_graph::freeze::codegen::standalone_for_spec::<MorphMesh>()
            .expect("morph_mesh buffer codegen")
    }

    /// Hand reference: bit-for-bit the committed formula (module doc
    /// comment).
    fn expected_morph(a: &MeshVertex, b: &MeshVertex, t: f32, w: f32) -> ([f32; 3], [f32; 3]) {
        let tw = t * w;
        let pos = [
            a.position[0] + (b.position[0] - a.position[0]) * tw,
            a.position[1] + (b.position[1] - a.position[1]) * tw,
            a.position[2] + (b.position[2] - a.position[2]) * tw,
        ];
        let n = [
            a.normal[0] + (b.normal[0] - a.normal[0]) * tw,
            a.normal[1] + (b.normal[1] - a.normal[1]) * tw,
            a.normal[2] + (b.normal[2] - a.normal[2]) * tw,
        ];
        let mag = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
        (pos, [n[0] / mag, n[1] / mag, n[2] / mag])
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_morph(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        a: &[MeshVertex],
        b: &[MeshVertex],
        weights: Option<&[f32]>,
        weights_len_override: Option<u32>,
        t: f32,
    ) -> Vec<MeshVertex> {
        let pipeline = device.create_compute_pipeline(
            wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "morph-mesh-test",
        );
        let a_buf = device.create_buffer_shared(std::mem::size_of_val(a) as u64);
        unsafe {
            a_buf.write(0, bytemuck::cast_slice(a));
        }
        let b_buf = device.create_buffer_shared(std::mem::size_of_val(b) as u64);
        unsafe {
            b_buf.write(0, bytemuck::cast_slice(b));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(a) as u64);

        let (wbuf, weights_len) = match weights {
            Some(w) => {
                let mut padded = vec![0.0f32; a.len()];
                padded[..w.len().min(a.len())].copy_from_slice(&w[..w.len().min(a.len())]);
                let buf = device.create_buffer_shared((padded.len() * 4).max(4) as u64);
                unsafe {
                    buf.write(0, bytemuck::cast_slice(&padded));
                }
                (buf, weights_len_override.unwrap_or(w.len() as u32))
            }
            None => (device.create_buffer_shared(std::mem::size_of_val(a) as u64), 0),
        };

        let uniforms = MorphUniforms {
            t,
            weights_len,
            dispatch_count: a.len() as u32,
            _pad0: 0,
        };

        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &a_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &b_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &wbuf, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("morph-mesh-test");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [(a.len() as u32).div_ceil(256), 1, 1],
            "morph-mesh-test",
        );
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, a.len()) }.to_vec()
    }


    #[test]
    fn matches_hand_formula_analytically_and_uv_from_a() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let a = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.11, 0.22]),
            mk_vertex([1.0, 2.0, -1.0], [1.0, 0.0, 0.0], [0.33, 0.44]),
        ];
        let b = vec![
            mk_vertex([4.0, 1.0, 0.5], [0.0, 0.0, 1.0], [0.99, 0.88]),
            mk_vertex([-1.0, 0.5, 2.0], [0.0, 1.0, 0.0], [0.66, 0.55]),
        ];
        let t = 0.4f32;
        let out = dispatch_morph(&device, &gen_wgsl, &a, &b, None, None, t);
        for i in 0..a.len() {
            let (exp_pos, exp_n) = expected_morph(&a[i], &b[i], t, 1.0);
            for c in 0..3 {
                assert!(
                    (out[i].position[c] - exp_pos[c]).abs() < 1e-5,
                    "vertex {i} pos[{c}]: got={} expected={}",
                    out[i].position[c],
                    exp_pos[c]
                );
                assert!(
                    (out[i].normal[c] - exp_n[c]).abs() < 1e-4,
                    "vertex {i} normal[{c}]: got={} expected={}",
                    out[i].normal[c],
                    exp_n[c]
                );
            }
            assert_eq!(out[i].uv, a[i].uv, "uv must come from `a`, vertex {i}");
        }
    }

    #[test]
    fn count_and_order_are_preserved_at_t_zero_and_one() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let a = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.2]),
            mk_vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.3, 0.4]),
            mk_vertex([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.6]),
        ];
        let b = vec![
            mk_vertex([9.0, 9.0, 9.0], [1.0, 0.0, 0.0], [0.9, 0.9]),
            mk_vertex([8.0, 8.0, 8.0], [0.0, 1.0, 0.0], [0.8, 0.8]),
            mk_vertex([7.0, 7.0, 7.0], [0.0, 0.0, 1.0], [0.7, 0.7]),
        ];
        let out0 = dispatch_morph(&device, &gen_wgsl, &a, &b, None, None, 0.0);
        assert_eq!(out0.len(), a.len());
        for i in 0..a.len() {
            assert_eq!(out0[i].position, a[i].position, "t=0 should equal a exactly, vertex {i}");
        }
        let out1 = dispatch_morph(&device, &gen_wgsl, &a, &b, None, None, 1.0);
        for i in 0..a.len() {
            for c in 0..3 {
                assert!(
                    (out1[i].position[c] - b[i].position[c]).abs() < 1e-5,
                    "t=1 should equal b, vertex {i} c={c}"
                );
            }
        }
    }

    #[test]
    fn short_weights_degrade_to_one_for_the_tail() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        let a: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0]))
            .collect();
        let b: Vec<MeshVertex> = (0..12)
            .map(|_| mk_vertex([2.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0]))
            .collect();
        let weights = [0.0f32, 0.0];
        let t = 0.7f32;

        let out = dispatch_morph(&device, &gen_wgsl, &a, &b, Some(&weights), Some(2), t);

        assert!(
            (out[0].position[0]).abs() < 1e-5,
            "vertex 0 has explicit weight 0 -> unchanged (equals a), got x={}",
            out[0].position[0]
        );
        assert!(
            (out[1].position[0]).abs() < 1e-5,
            "vertex 1 has explicit weight 0 -> unchanged (equals a), got x={}",
            out[1].position[0]
        );
        let expected_x = 0.0 + (2.0 - 0.0) * t;
        for (i, v) in out.iter().enumerate().skip(2).take(10) {
            assert!(
                (v.position[0] - expected_x).abs() < 1e-5,
                "vertex {i} past weights_len should degrade to w=1.0, got x={} expected={}",
                v.position[0],
                expected_x
            );
        }
    }
}
