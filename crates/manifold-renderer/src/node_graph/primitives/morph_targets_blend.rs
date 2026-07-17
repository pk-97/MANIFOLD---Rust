//! `node.morph_targets_blend` — glTF additive N-ary morph-target blend
//! (GLTF_ANIMATION_DESIGN.md A3).
//!
//! Barrier-free pure per-element kernel: for each vertex, sums up to
//! `target_count` weighted deltas (`node.gltf_morph_deltas_source`'s
//! flattened target-major `Array(MeshVertex)`, looked up via a
//! `BufferGather` `deltas[target * vertex_count + idx]`) onto the base
//! mesh (`in`, coincident — typically `node.gltf_mesh_source`'s output),
//! per glTF 2.0 §3.7.2.1's additive morph formula:
//!
//! `pos' = base.pos + sum(weight[t] * delta[t].pos)`
//! `normal' = normalize(base.normal + sum(weight[t] * delta[t].normal))`
//!
//! `weights` (`node.gltf_morph_weights`' output, one f32 per target) is
//! ALSO `BufferGather` — this atom does no per-frame CPU sampling itself,
//! it only sums whatever the CPU sampler already resolved (same
//! CPU-samples/GPU-sums split A2's `node.gltf_skeleton_pose` +
//! `node.skin_mesh` pair proves).
//!
//! `node.morph_mesh` (the pre-existing static two-mesh lerp) is
//! deliberately untouched — its header already documents this exact
//! boundary ("glTF morph-target playback is a separate future design,
//! do not grow this atom toward it"); this primitive is that separate
//! design, not an extension. A single N-ary node with a uniform-bounded
//! loop (rather than N chained single-target apply nodes) keeps graph
//! topology independent of the imported asset's target count — a
//! Fable-advisory rejection recorded in the A3 phase brief.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Same generous headroom as `node.gltf_morph_weights::MAX_TARGETS` —
/// duplicated rather than shared across the two small A3 primitives (no
/// other coupling between them beyond the same authored `target_count`).
const MAX_TARGETS: usize = 64;

/// Generated-codegen uniform layout: the `target_count` param (Int -> i32),
/// then the derived `deltas_len`/`weights_len` (u32 each), then the
/// codegen-injected `dispatch_count`. 4 words, no padding needed (already
/// a 16-byte multiple). Matches `standalone_for_spec::<MorphTargetsBlend>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MorphTargetsBlendUniforms {
    target_count: i32,
    deltas_len: u32,
    weights_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: MorphTargetsBlend,
    type_id: "node.morph_targets_blend",
    purpose: "glTF additive N-ary morph-target blend: deforms Array(MeshVertex) `in` (the base mesh) by summing up to `target_count` weighted per-target deltas. deltas (Array(MeshVertex), flattened target-major: deltas[target * vertex_count + idx]) and weights (Array(f32), one per target) are both BufferGather — deltas from node.gltf_morph_deltas_source, weights from node.gltf_morph_weights. pos' = base.pos + sum(weight[t] * delta[t].pos); normal' = normalize(base.normal + sum(weight[t] * delta[t].normal)). The effective loop bound is min(target_count, weights_len, deltas_len / vertex_count) — a short or mismatched buffer truncates (skips the missing targets), never reads out of bounds. target_count == 0 is a strict base pass-through. Barrier-free per-element kernel — the codegen path (fusable), never a fusion-boundary WGSL include.",
    inputs: {
        in: Array(MeshVertex) required,
        deltas: Array(MeshVertex) required,
        weights: Array(f32) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("target_count"),
            label: "Target Count",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, MAX_TARGETS as f32)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.gltf_mesh_source's vertices (the base mesh), node.gltf_morph_deltas_source's deltas, and node.gltf_morph_weights' weights into this node's matching inputs. `target_count` must match node.gltf_morph_weights' own `target_count` param (gltf_import.rs sets both from the same GltfObjectMorph) — a short/mismatched deltas or weights buffer truncates the blend rather than reading out of bounds.",
    examples: [],
    picker: { label: "Morph Targets Blend", category: Atom },
    summary: "Blends an imported mesh's morph targets by their live animated weights — the GPU counterpart to a Morph Weights node's sampled weight vector.",
    category: Geometry3D,
    role: Filter,
    aliases: ["morph targets blend", "blend shapes", "morph target playback", "shape keys"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/morph_targets_blend_body.wgsl"),
    input_access: [Coincident, BufferGather, BufferGather],
    derived_uniforms: ["deltas_len:u32", "weights_len:u32"],
}

impl Primitive for MorphTargetsBlend {
    /// Output `out` follows `in`'s capacity — morph blending is a pure
    /// per-vertex deform, same count in and out.
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
        let target_count = match ctx.params.get("target_count") {
            Some(ParamValue::Float(f)) => f.round().clamp(0.0, MAX_TARGETS as f32) as i32,
            _ => 0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(deltas_buf) = ctx.inputs.array("deltas") else {
            return;
        };
        let Some(weights_buf) = ctx.inputs.array("weights") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;

        let in_cap = (in_buf.size / vertex_size) as u32;
        let out_cap = (out_buf.size / vertex_size) as u32;
        let count = in_cap.min(out_cap);
        if count == 0 || target_count == 0 {
            return;
        }
        let deltas_len = (deltas_buf.size / vertex_size) as u32;
        let weights_len = (weights_buf.size / std::mem::size_of::<f32>() as u64) as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.morph_targets_blend standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.morph_targets_blend",
            )
        });

        let uniforms = MorphTargetsBlendUniforms {
            target_count,
            deltas_len,
            weights_len,
            dispatch_count: count,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer { binding: 1, buffer: in_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: deltas_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: weights_buf, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out_buf, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.morph_targets_blend",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn morph_targets_blend_declares_three_required_array_inputs_and_one_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(MorphTargetsBlend::TYPE_ID, "node.morph_targets_blend");
        assert_eq!(MorphTargetsBlend::INPUTS.len(), 3);
        let in_port = MorphTargetsBlend::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));
        let deltas_port = MorphTargetsBlend::INPUTS.iter().find(|p| p.name == "deltas").unwrap();
        assert!(deltas_port.required);
        assert_eq!(deltas_port.ty, PortType::Array(mesh_layout));
        let weights_port = MorphTargetsBlend::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        assert_eq!(MorphTargetsBlend::OUTPUTS.len(), 1);
        assert_eq!(MorphTargetsBlend::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn morph_targets_blend_output_follows_in_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = MorphTargetsBlend::new();
        let params = ParamValues::default();
        let inputs = [("in", 4000_u32), ("deltas", 32000_u32), ("weights", 8_u32)];
        assert_eq!(Primitive::array_output_capacity(&prim, "out", &params, &inputs), Some(4000));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MorphTargetsBlend::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.morph_targets_blend");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. No legacy predecessor to diff against —
    //! parity is against a hand-written Rust reference of the committed
    //! additive morph formula, element-wise, per
    //! DECOMPOSING_GENERATORS.md §9.
    use super::*;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal, _pad1: 0.0, uv: [0.0, 0.0], _pad2: [0.0, 0.0] }
    }

    /// Generated standalone kernel (the shipping runtime path).
    fn generated_wgsl() -> String {
        crate::node_graph::freeze::codegen::standalone_for_spec::<MorphTargetsBlend>()
            .expect("morph_targets_blend buffer codegen")
    }

    /// Hand Rust reference: bit-for-bit the committed formula (module doc
    /// comment) — base + sum(weight[t] * delta[t]), truncated to
    /// min(target_count, weights.len(), deltas.len()/vertex_count).
    fn expected_blend(
        base: &MeshVertex,
        vertex_idx: usize,
        vertex_count: usize,
        deltas: &[MeshVertex],
        weights: &[f32],
        target_count: i32,
    ) -> ([f32; 3], [f32; 3]) {
        let by_deltas = if vertex_count > 0 { deltas.len() / vertex_count } else { 0 };
        let bound = (target_count.max(0) as usize).min(weights.len()).min(by_deltas);
        let mut pos = base.position;
        let mut nrm = base.normal;
        for t in 0..bound {
            let w = weights[t];
            let d = &deltas[t * vertex_count + vertex_idx];
            for c in 0..3 {
                pos[c] += w * d.position[c];
                nrm[c] += w * d.normal[c];
            }
        }
        let mag = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt().max(1e-12);
        (pos, [nrm[0] / mag, nrm[1] / mag, nrm[2] / mag])
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_blend(
        device: &manifold_gpu::GpuDevice,
        verts: &[MeshVertex],
        deltas: &[MeshVertex],
        weights: &[f32],
        target_count: i32,
        weights_len_override: Option<u32>,
        deltas_len_override: Option<u32>,
    ) -> Vec<MeshVertex> {
        let wgsl = generated_wgsl();
        let pipeline =
            device.create_compute_pipeline(&wgsl, crate::node_graph::freeze::codegen::ENTRY, "morph-targets-blend-test");
        let in_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(verts));
        }
        let deltas_buf = device.create_buffer_shared((std::mem::size_of_val(deltas) as u64).max(1));
        if !deltas.is_empty() {
            unsafe {
                deltas_buf.write(0, bytemuck::cast_slice(deltas));
            }
        }
        let weights_buf = device.create_buffer_shared((std::mem::size_of_val(weights) as u64).max(1));
        if !weights.is_empty() {
            unsafe {
                weights_buf.write(0, bytemuck::cast_slice(weights));
            }
        }
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(verts) as u64);

        let uniforms = MorphTargetsBlendUniforms {
            target_count,
            deltas_len: deltas_len_override.unwrap_or(deltas.len() as u32),
            weights_len: weights_len_override.unwrap_or(weights.len() as u32),
            dispatch_count: verts.len() as u32,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &deltas_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &weights_buf, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &out_buf, offset: 0 },
        ];
        let mut enc = device.create_encoder("morph-targets-blend-test");
        enc.dispatch_compute(&pipeline, &bindings, [(verts.len() as u32).div_ceil(256), 1, 1], "morph-targets-blend-test");
        enc.commit_and_wait_completed();
        let ptr = out_buf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, verts.len()) }.to_vec()
    }

    /// Single target, full weight — the blend must equal base + delta
    /// exactly.
    #[test]
    fn generated_matches_hand_formula_single_target_full_weight() {
        let device = crate::test_device();
        let gen_wgsl = generated_wgsl();
        assert!(gen_wgsl.contains("struct Element"), "element struct synthesized");
        assert!(gen_wgsl.contains("var<storage, read_write>"), "output bound read_write");

        let verts = vec![
            mk_vertex([1.0, 2.0, 3.0], [0.0, 1.0, 0.0]),
            mk_vertex([-1.0, 0.0, 0.5], [1.0, 0.0, 0.0]),
        ];
        let deltas = vec![
            mk_vertex([1.0, 0.0, 0.0], [0.1, 0.0, 0.0]),
            mk_vertex([0.0, 2.0, 0.0], [0.0, 0.1, 0.0]),
        ];
        let weights = vec![1.0f32];

        let out = dispatch_blend(&device, &verts, &deltas, &weights, 1, None, None);
        for i in 0..verts.len() {
            let (exp_pos, exp_n) = expected_blend(&verts[i], i, verts.len(), &deltas, &weights, 1);
            for c in 0..3 {
                assert!(
                    (out[i].position[c] - exp_pos[c]).abs() < 1e-4,
                    "vertex {i} pos[{c}]: got={} expected={}",
                    out[i].position[c],
                    exp_pos[c]
                );
                assert!((out[i].normal[c] - exp_n[c]).abs() < 1e-4, "vertex {i} normal[{c}]");
            }
        }
    }

    /// Two targets blended by partial weights — proves summation, not
    /// just single-target pass-through.
    #[test]
    fn generated_matches_hand_formula_multi_target_blend() {
        let device = crate::test_device();
        let verts = vec![mk_vertex([2.0, 0.0, 0.0], [0.0, 0.0, 1.0])];
        // Target-major: deltas[0]=target0's delta for vertex 0, deltas[1]=target1's.
        let deltas = vec![
            mk_vertex([4.0, 0.0, 0.0], [0.2, 0.0, 0.0]),
            mk_vertex([0.0, 6.0, 0.0], [0.0, 0.3, 0.0]),
        ];
        let weights = vec![0.25f32, 0.75];

        let out = dispatch_blend(&device, &verts, &deltas, &weights, 2, None, None);
        let (exp_pos, exp_n) = expected_blend(&verts[0], 0, verts.len(), &deltas, &weights, 2);
        for c in 0..3 {
            assert!(
                (out[0].position[c] - exp_pos[c]).abs() < 1e-4,
                "blended vertex pos[{c}]: got={} expected={}",
                out[0].position[c],
                exp_pos[c]
            );
            assert!((out[0].normal[c] - exp_n[c]).abs() < 1e-4, "blended vertex normal[{c}]");
        }
    }

    /// `target_count == 0` (or all-zero weights) is a strict base
    /// pass-through — no delta ever applied.
    #[test]
    fn zero_target_count_is_base_pass_through() {
        let device = crate::test_device();
        let verts = vec![mk_vertex([5.0, -2.0, 1.0], [0.0, 1.0, 0.0])];
        let deltas = vec![mk_vertex([100.0, 100.0, 100.0], [1.0, 1.0, 1.0])];
        let weights = vec![1.0f32];

        let out = dispatch_blend(&device, &verts, &deltas, &weights, 0, None, None);
        assert_eq!(out[0].position, verts[0].position, "zero target_count must not apply any delta");
        for c in 0..3 {
            assert!((out[0].normal[c] - verts[0].normal[c]).abs() < 1e-4);
        }
    }

    /// A short weights buffer (weights_len < target_count) truncates the
    /// loop bound rather than reading out of bounds — the buffer-length
    /// hazard the A3 phase brief calls out explicitly.
    #[test]
    fn short_weights_buffer_truncates_the_blend_bound() {
        let device = crate::test_device();
        let verts = vec![mk_vertex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0])];
        let deltas = vec![
            mk_vertex([10.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
            mk_vertex([0.0, 10.0, 0.0], [0.0, 0.0, 0.0]),
        ];
        // Only ONE weight provided though target_count claims 2 —
        // weights_len (derived from the buffer) must win.
        let weights = vec![1.0f32];

        let out = dispatch_blend(&device, &verts, &deltas, &weights, 2, Some(1), None);
        assert!(
            (out[0].position[0] - 10.0).abs() < 1e-4,
            "only target 0 applied (weights_len=1 truncates target 1), got x={}",
            out[0].position[0]
        );
        assert!(
            (out[0].position[1]).abs() < 1e-4,
            "target 1 must NOT apply — weights buffer too short, got y={}",
            out[0].position[1]
        );
    }
}
