//! `node.melt_mesh` — per-vertex downward melt of an `Array<MeshVertex>`.
//!
//! Per vertex: `pos.y -= amount * (simplex(pos.xz * frequency + seed) * 0.5 + 0.5)`,
//! where `w` is the optional per-vertex `weights` input (degrading to 1.0 past a
//! short/unwired buffer). Normals, uv, and tangent pass through unchanged.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amount`,
/// `frequency`, `seed` f32), then the derived `weights_len` (u32), then the
/// codegen-injected `dispatch_count`, padded to a 16-byte multiple. 5 words +
/// 3 pad = 32 bytes. Matches `standalone_for_spec::<MeltMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeltUniforms {
    amount: f32,
    frequency: f32,
    seed: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: MeltMesh,
    type_id: "node.melt_mesh",
    purpose: "Per-vertex downward melt of an Array<MeshVertex>. pos.y -= amount * (simplex3(pos.x * frequency + seed, pos.z * frequency + seed, 0.0) * 0.5 + 0.5). `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Normals, uv, and tangent pass through unchanged — wire node.facet_normals downstream after a heavy melt if the unchanged normals start reading wrong under lighting.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amount: ScalarF32 optional,
        frequency: ScalarF32 optional,
        seed: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frequency"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'melt / drip' deformer: pulls vertices down by a noise-modulated amount, as if the model is softening. Wire node.mesh_ramp's `weights` output to melt from the bottom up or localize the effect.",
    examples: [],
    picker: { label: "Melt", category: Atom },
    summary: "Pulls every vertex downward by a noise-driven amount, making a mesh appear to melt or slump.",
    category: Geometry3D,
    role: Filter,
    aliases: ["melt", "melt mesh", "drip", "slump"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/melt_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so it can chain with other mesh deformers. `weights_len`
    // is a frame-derived uniform the body uses to bounds-check the coincident
    // weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for MeltMesh {
    /// Output `out` is sized to match input `in` — melt is a per-vertex
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
        let amount = ctx.scalar_or_param("amount", 0.0);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let seed = ctx.scalar_or_param("seed", 0.0);

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
            // Codegen path: the runtime kernel is generated from `wgsl_body`
            // (with noise_common prepended) so this atom stays pointwise/fusable
            // in the graph compiler.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.melt_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.melt_mesh",
            )
        });

        let uniforms = MeltUniforms {
            amount,
            frequency,
            seed,
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
            "node.melt_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn melt_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(MeltMesh::TYPE_ID, "node.melt_mesh");

        let in_port = MeltMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = MeltMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["amount", "frequency", "seed"] {
            let port = MeltMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(MeltMesh::OUTPUTS.len(), 1);
        assert_eq!(MeltMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn melt_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = MeltMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = MeltMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.melt_mesh");
    }
}
